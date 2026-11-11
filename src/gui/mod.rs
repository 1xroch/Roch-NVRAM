// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! The window.
//!
//! Everything it decides -- what is queued, which buttons work, what a Quick
//! Settings row does -- lives in `roch_nvram::session::Session` and is tested
//! there. This module draws it, asks the questions, and runs SCEWIN on a
//! background thread so the window keeps painting while the firmware is read
//! or written.

mod dialogs;
mod shell;
mod theme;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;

use eframe::egui::{
    self, Align, Button, Color32, ComboBox, CornerRadius, Frame, Layout, Margin, RichText, ScrollArea, Sense, Stroke,
    TextEdit, TextureHandle, Ui,
};
use egui_extras::{Column, TableBuilder};

use roch_nvram::backend::{ApplyPlan, ApplyResult, LiveExport, ScewinBackend};
use roch_nvram::catalog::CatalogEntry;
use roch_nvram::compare::{compare_snapshots, export_compare_csv, load_snapshot, CompareResult, NvramSnapshot};
use roch_nvram::core::{is_editable, setting_display, write_transaction, ParsedProfile, PendingChange, ScewinDocument};
use roch_nvram::parser::{BiosSetting, Kind};
use roch_nvram::quick_settings::default_presets;
use roch_nvram::session::{option_items, thousands, QuickEditor, Session, Tone};
use roch_nvram::{Result, APP_NAME, VERSION};

use dialogs::{Answer, Dialog, Level};
use theme::{bold, palette, Palette};

const ICON: &[u8] = include_bytes!("../../assets/roch_nvram.ico");

/// How the window was asked to start.
pub struct Options {
    pub live: bool,
    pub profile: PathBuf,
    pub nvram: PathBuf,
    pub project_root: PathBuf,
}

fn decode_icon() -> Option<image::RgbaImage> {
    image::load_from_memory_with_format(ICON, image::ImageFormat::Ico).ok().map(|icon| icon.to_rgba8())
}

fn fatal(title: &str, text: &str) {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(title)
        .set_description(text)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

pub fn run(options: Options) -> ExitCode {
    // Live mode opens with nothing loaded: reading the firmware is what the
    // Export button is for. Offline mode opens its sample export.
    let mut opened = None;
    let backend = if options.live {
        Some(Arc::new(ScewinBackend::new(&options.project_root)))
    } else {
        let absent: Vec<String> = [&options.profile, &options.nvram]
            .iter()
            .filter(|path| !path.is_file())
            .map(|path| path.display().to_string())
            .collect();
        if !absent.is_empty() {
            fatal(
                "Sample NVRAM not found",
                &format!(
                    "Offline mode needs a sample export, which is not shipped because it is board-specific.\n\n\
                     Missing:\n{}\n\nPoint the app at your own export with --profile and --nvram.",
                    absent.join("\n")
                ),
            );
            return ExitCode::from(1);
        }
        let open = || -> Result<(ParsedProfile, ScewinDocument)> {
            let profile = ParsedProfile::load(&options.profile)?;
            let document = ScewinDocument::open(&options.nvram)?;
            document.verify_profile(&profile)?;
            Ok((profile, document))
        };
        match open() {
            Ok(pair) => opened = Some(pair),
            Err(error) => {
                fatal(
                    "NVRAM could not be opened",
                    &format!("{APP_NAME} could not open the NVRAM export. Nothing was written.\n\n{error}"),
                );
                return ExitCode::from(1);
            }
        }
        None
    };

    let title = window_title(options.live);
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(&title)
        .with_inner_size([1500.0, 850.0])
        .with_min_inner_size([700.0, 480.0])
        .with_decorations(false)
        .with_transparent(false);
    let icon = decode_icon();
    if let Some(icon) = &icon {
        viewport = viewport.with_icon(egui::IconData { rgba: icon.as_raw().clone(), width: icon.width(), height: icon.height() });
    }
    let native = eframe::NativeOptions { viewport, centered: true, ..Default::default() };

    let result = eframe::run_native(
        APP_NAME,
        native,
        Box::new(move |cc| {
            theme::install_fonts(&cc.egui_ctx);
            let dark = theme::load_dark_mode();
            theme::apply(&cc.egui_ctx, dark);
            let icon_texture = icon.map(|icon| {
                let small = image::imageops::resize(&icon, 48, 48, image::imageops::FilterType::Lanczos3);
                cc.egui_ctx.load_texture(
                    "roch-icon",
                    egui::ColorImage::from_rgba_unmultiplied([48, 48], small.as_raw()),
                    egui::TextureOptions::LINEAR,
                )
            });
            let mut app = App::new(backend, dark, icon_texture);
            if let Some((profile, document)) = opened {
                app.show_nvram(profile, document, "Startup");
            }
            Ok(Box::new(app))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            fatal("Unexpected error", &format!("{APP_NAME} could not start.\n\n{error}"));
            ExitCode::from(1)
        }
    }
}

// ----- state -----------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Nvram,
    Quick,
    Pending,
    Compare,
    Log,
}

const TABS: [(Tab, &str); 5] = [
    (Tab::Nvram, "NVRAM"),
    (Tab::Quick, "Quick Settings"),
    (Tab::Pending, "Pending Changes"),
    (Tab::Compare, "Compare"),
    (Tab::Log, "Log"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KindFilter {
    All,
    Dropdown,
    Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WarningFilter {
    All,
    EditableOnly,
    WarningsOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompareMode {
    ChangedOnly,
    AllEntries,
    SameOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Stock,
    Overclocked,
}

/// What a question does when it is answered.
enum Action {
    ExportLive,
    ConfirmImport(Box<ApplyPlan>),
    CancelImport,
    QueueLoaded { changes: Vec<PendingChange>, reset: Vec<String>, summary: String },
    CancelLoad { summary: String },
    RestartPc,
}

enum JobResult {
    Export(Result<LiveExport>),
    Prepare(Result<ApplyPlan>),
    Apply(Result<ApplyResult>),
}

struct Job {
    message: String,
    receiver: Receiver<JobResult>,
}

struct EditState {
    setting: BiosSetting,
    editor: QuickEditor,
}

struct App {
    session: Session,
    backend: Option<Arc<ScewinBackend>>,
    dark: bool,
    sync_caption: bool,
    icon: Option<TextureHandle>,
    title: String,
    tab: Tab,

    search: String,
    kind: KindFilter,
    warnings: WarningFilter,
    haystacks: Vec<String>,
    /// is_editable for each setting, worked out once per export.
    editable: Vec<bool>,
    filtered: Vec<usize>,
    filter_key: Option<(String, KindFilter, WarningFilter, usize)>,
    generation: usize,
    selected_setting: Option<usize>,
    /// The export ID of the selected pending change. Queue edits reorder the
    /// queue, so a position would drift onto a different change.
    selected_change: Option<String>,

    stock: Option<NvramSnapshot>,
    overclocked: Option<NvramSnapshot>,
    compare: Option<CompareResult>,
    compare_summary: (String, Tone),
    compare_search: String,
    compare_mode: CompareMode,
    compare_filtered: Vec<usize>,
    compare_key: Option<(String, CompareMode, usize)>,
    compare_generation: usize,
    compare_selected: Option<usize>,

    /// Newest first; the catalog file is append-only, oldest first.
    catalog: Vec<CatalogEntry>,
    catalog_selected: Option<usize>,

    dialogs: Vec<Dialog<Action>>,
    edit: Option<EditState>,
    job: Option<Job>,
    /// A saved export picked with Load NVRAM before anything was loaded: it is
    /// compared once the live export that was started for it arrives.
    load_after_export: Option<NvramSnapshot>,
}

impl App {
    fn new(backend: Option<Arc<ScewinBackend>>, dark: bool, icon: Option<TextureHandle>) -> Self {
        let live = backend.is_some();
        let app_root = backend.as_ref().map(|backend| backend.app_root.clone());
        let session = Session::new(live, app_root, default_presets());
        let mut catalog = session.catalog.entries();
        catalog.reverse();
        Self {
            session,
            backend,
            dark,
            sync_caption: true,
            icon,
            title: window_title(live),
            tab: Tab::Nvram,
            search: String::new(),
            kind: KindFilter::All,
            warnings: WarningFilter::All,
            haystacks: Vec::new(),
            editable: Vec::new(),
            filtered: Vec::new(),
            filter_key: None,
            generation: 0,
            selected_setting: None,
            selected_change: None,
            stock: None,
            overclocked: None,
            compare: None,
            compare_summary: ("Select NVRAM 1 and NVRAM 2. Changed settings are shown by default.".into(), Tone::Normal),
            compare_search: String::new(),
            compare_mode: CompareMode::ChangedOnly,
            compare_filtered: Vec::new(),
            compare_key: None,
            compare_generation: 0,
            compare_selected: None,
            catalog,
            catalog_selected: None,
            dialogs: Vec::new(),
            edit: None,
            job: None,
            load_after_export: None,
        }
    }

    fn palette(&self) -> &'static Palette {
        palette(self.dark)
    }

    fn live(&self) -> bool {
        self.backend.is_some()
    }

    // ----- messages ----------------------------------------------------------

    fn message(&mut self, level: Level, title: &str, text: impl Into<String>) {
        self.dialogs.push(Dialog {
            level,
            title: title.into(),
            text: text.into(),
            on_yes: None,
            on_no: None,
            buttons: ("Yes", "No"),
        });
    }

    fn info(&mut self, title: &str, text: impl Into<String>) {
        self.message(Level::Information, title, text);
    }

    fn warning(&mut self, title: &str, text: impl Into<String>) {
        self.message(Level::Warning, title, text);
    }

    fn critical(&mut self, title: &str, text: impl Into<String>) {
        self.message(Level::Critical, title, text);
    }

    fn question(&mut self, title: &str, text: impl Into<String>, on_yes: Action, on_no: Option<Action>) {
        self.dialogs.push(Dialog {
            level: Level::Information,
            title: title.into(),
            text: text.into(),
            on_yes: Some(on_yes),
            on_no,
            buttons: ("Yes", "No"),
        });
    }

    fn nvram_loaded(&mut self, action: &str) -> bool {
        if self.session.loaded() {
            return true;
        }
        let hint = if self.live() {
            "Press Export to read the live settings first."
        } else {
            "Start the app with --profile and --nvram to open an export."
        };
        self.info("No NVRAM loaded", format!("{action} needs an NVRAM export.\n\n{hint}"));
        false
    }

    fn log(&self, event: &str, message: &str) {
        self.session.log(event, message);
    }

    // ----- loading -----------------------------------------------------------

    /// Put a freshly opened export in the table and catalogue it.
    fn show_nvram(&mut self, profile: ParsedProfile, document: ScewinDocument, source: &str) {
        let entry = self.session.show_nvram(profile, document, source);
        self.add_catalog_entry(entry);
        let settings = self.session.profile.as_ref().map(|p| p.settings.as_slice()).unwrap_or_default();
        self.haystacks = settings
            .iter()
            .map(|s| [&s.question, &s.help, &s.token_hex, &s.offset_hex, &s.export_id].map(|f| f.as_str()).join(" ").to_lowercase())
            .collect();
        self.editable = settings.iter().map(|s| is_editable(s).0).collect();
        self.generation += 1;
        self.selected_setting = None;
        self.selected_change = None;
    }

    /// Newest first, so the selected Log row moves down with its capture.
    fn add_catalog_entry(&mut self, entry: CatalogEntry) {
        self.catalog.insert(0, entry);
        if let Some(index) = &mut self.catalog_selected {
            *index += 1;
        }
    }

    fn refresh_filter(&mut self) {
        let key = (self.search.trim().to_lowercase(), self.kind, self.warnings, self.generation);
        if self.filter_key.as_ref() == Some(&key) {
            return;
        }
        let settings = self.session.profile.as_ref().map(|p| p.settings.as_slice()).unwrap_or_default();
        self.filtered = settings
            .iter()
            .enumerate()
            .filter(|(index, setting)| {
                match key.1 {
                    KindFilter::Dropdown if setting.kind != Kind::Options => return false,
                    KindFilter::Value if setting.kind != Kind::Value => return false,
                    _ => {}
                }
                match key.2 {
                    WarningFilter::WarningsOnly if setting.warnings.is_empty() => return false,
                    WarningFilter::EditableOnly if !self.editable[*index] => return false,
                    _ => {}
                }
                key.0.is_empty() || self.haystacks[*index].contains(&key.0)
            })
            .map(|(index, _)| index)
            .collect();
        self.filter_key = Some(key);
    }

    // ----- SCEWIN on a background thread -------------------------------------

    fn start_job(&mut self, message: &str, work: impl FnOnce(&ScewinBackend) -> JobResult + Send + 'static) {
        let Some(backend) = self.backend.clone() else { return };
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(work(&backend));
        });
        self.job = Some(Job { message: message.into(), receiver });
    }

    fn poll_job(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else { return };
        let result = match job.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            Err(TryRecvError::Disconnected) => {
                self.job = None;
                self.critical("Unexpected error", "The SCEWIN worker stopped without a result.");
                return;
            }
        };
        self.job = None;
        match result {
            JobResult::Export(Ok(live)) => {
                let count = live.profile.settings.len();
                let path = live.nvram_path.display().to_string();
                self.show_nvram(live.profile, live.document, "Export NVRAM");
                if let Some(snapshot) = self.load_after_export.take() {
                    self.queue_from_snapshot(snapshot);
                } else {
                    self.info("NVRAM exported", format!("Read {} settings from the live NVRAM.\n\n{path}", thousands(count)));
                }
            }
            JobResult::Export(Err(error)) => {
                self.load_after_export = None;
                self.log("NVRAM_EXPORT_FAILED", &format!("tag=current; error={error}"));
                self.critical("NVRAM export failed", error.to_string());
            }
            JobResult::Prepare(Err(error)) => {
                self.log("APPLY_PREFLIGHT_FAILED", &error.to_string());
                self.critical("Import preflight failed", error.to_string());
            }
            JobResult::Prepare(Ok(plan)) => {
                let mut lines = vec![format!("Import {} queued NVRAM change(s)?", plan.changes.len()), String::new()];
                for change in plan.changes.iter().take(12) {
                    lines.push(format!("• {}: {} → {}", change.question, change.old_display, change.new_display));
                }
                if plan.changes.len() > 12 {
                    lines.push(format!("• ...and {} more", plan.changes.len() - 12));
                }
                lines.push(String::new());
                lines.push("A full NVRAM backup and transaction log will be created before import.".into());
                lines.push("Incorrect firmware settings can prevent booting. A reboot may be required.".into());
                self.question(
                    "Confirm NVRAM import",
                    lines.join("\n"),
                    Action::ConfirmImport(Box::new(plan)),
                    Some(Action::CancelImport),
                );
            }
            JobResult::Apply(Err(error)) => {
                self.log("APPLY_FAILED", &error.to_string());
                self.critical("NVRAM import not verified", error.to_string());
            }
            JobResult::Apply(Ok(result)) => {
                self.log(
                    "APPLY_SUCCESS",
                    &format!(
                        "Imported and verified {} change(s); backup={}",
                        result.verified_count,
                        result.backup_dir.display()
                    ),
                );
                let verified = result.verified_export;
                self.show_nvram(verified.profile, verified.document, "Live refresh after apply");
                // Every queued value verified, but SCEWIN imports the whole file:
                // a non-zero exit (never seen in a clean import) is worth a look.
                let code = result.import_result.returncode;
                let exit_note = if code == 0 {
                    String::new()
                } else {
                    format!(
                        "\n\nSCEWIN exited with code {code}. Check import.log in the backup folder before \
                         restarting."
                    )
                };
                self.dialogs.push(Dialog {
                    level: if code == 0 { Level::Information } else { Level::Warning },
                    title: "NVRAM changes applied".into(),
                    text: format!(
                        "All {} queued change(s) were imported and verified.\n\nBackup: {}{exit_note}\n\n\
                         Restart Windows for settings that require a reboot. Save your work in other \
                         programs before you restart.",
                        result.verified_count,
                        result.backup_dir.display()
                    ),
                    on_yes: Some(Action::RestartPc),
                    on_no: None,
                    buttons: ("Restart now", "Later"),
                });
            }
        }
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::ExportLive => self.start_export(),
            Action::ConfirmImport(plan) => {
                for change in &plan.changes {
                    self.log(
                        "APPLY_REQUESTED_CHANGE",
                        &format!(
                            "{}: {} -> {}; token={}; offset={}; width={}",
                            change.question, change.old_display, change.new_display, change.token_hex, change.offset_hex,
                            change.width_hex
                        ),
                    );
                }
                self.start_job("Importing queued NVRAM settings through SCEWIN...", move |backend| {
                    JobResult::Apply(backend.execute_apply(&plan))
                });
            }
            Action::CancelImport => self.log("APPLY_CANCELLED", "User cancelled the confirmed NVRAM import."),
            Action::QueueLoaded { changes, reset, summary } => {
                let count = changes.len();
                self.session.queue_all(changes, &reset);
                self.log("NVRAM_LOAD_QUEUED", &summary);
                self.info(
                    "NVRAM loaded",
                    format!(
                        "Queued {} change(s) from the saved NVRAM.\n\nReview them under Pending Changes, then press \
                         Import to write them.",
                        thousands(count)
                    ),
                );
            }
            Action::CancelLoad { summary } => self.log("NVRAM_LOAD_CANCELLED", &summary),
            Action::RestartPc => self.restart_pc(),
        }
    }

    /// Restart Windows right away, after an import the user wants to take effect.
    fn restart_pc(&mut self) {
        self.log("RESTART_REQUESTED", "User chose Restart now after a verified NVRAM import.");
        let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let mut command = std::process::Command::new(PathBuf::from(system_root).join("System32").join("shutdown.exe"));
        command.args(["/r", "/t", "0"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let failure = match command.status() {
            Ok(status) if status.success() => return,
            Ok(status) => format!("shutdown exited with {status}."),
            Err(error) => format!("Could not run shutdown: {error}"),
        };
        self.log("RESTART_FAILED", &failure);
        self.critical("Restart failed", format!("{failure}\n\nThe settings are applied; restart Windows yourself."));
    }

    // ----- the round trip ----------------------------------------------------

    /// Read the live NVRAM on demand. Nothing touches firmware until this runs.
    fn export_live_nvram(&mut self) {
        if !self.session.changes.is_empty() {
            let count = self.session.changes.len();
            self.question(
                "Discard queued changes?",
                format!(
                    "Reading the live NVRAM replaces the table with a fresh export.\n\n{count} queued change(s) will \
                     be cleared. Nothing has been written to firmware.\n\nContinue?"
                ),
                Action::ExportLive,
                None,
            );
            return;
        }
        self.start_export();
    }

    /// Read the live NVRAM through SCEWIN, the way SCEWIN's Export.bat does.
    fn start_export(&mut self) {
        if !self.live() {
            self.info("Dry run", "Reading the live NVRAM is disabled in offline mode.");
            return;
        }
        self.start_job("Reading the live NVRAM through SCEWIN...", |backend| {
            JobResult::Export(backend.export_current("current"))
        });
    }

    fn apply_changes(&mut self) {
        if !self.live() {
            self.info("Dry run", "Import is disabled in offline mode.");
            return;
        }
        if !self.nvram_loaded("Import") {
            return;
        }
        if self.session.changes.is_empty() {
            self.info("No changes", "Queue at least one setting change first.");
            return;
        }
        self.log("APPLY_PREFLIGHT", &format!("Starting preflight for {} queued change(s).", self.session.changes.len()));
        let profile = self.session.profile.clone().expect("loaded");
        let changes: Vec<PendingChange> = self.session.changes.values().cloned().collect();
        self.start_job("Refreshing live NVRAM before import...", move |backend| {
            JobResult::Prepare(backend.prepare_apply(&profile, &changes))
        });
    }

    // ----- editing -----------------------------------------------------------

    fn edit_selected(&mut self) {
        if !self.nvram_loaded("Queueing a change") {
            return;
        }
        let setting = self
            .selected_setting
            .filter(|index| self.filtered.contains(index))
            .and_then(|index| self.session.profile.as_ref().and_then(|p| p.settings.get(index)))
            .cloned();
        let Some(setting) = setting else {
            self.info("Select a setting", "Select an NVRAM setting first.");
            return;
        };
        let (editable, reason) = is_editable(&setting);
        if !editable {
            self.warning("Setting blocked", format!("This entry is not editable in the safe build:\n\n{reason}"));
            return;
        }
        let editor = if setting.kind == Kind::Options {
            let mut editor = QuickEditor::Options { items: option_items(Some(&setting)), selected: None };
            editor.select_code(&setting.current_code_hex.clone().unwrap_or_default().to_uppercase());
            editor
        } else {
            QuickEditor::Value { text: setting.current_value.clone().unwrap_or_default() }
        };
        self.edit = Some(EditState { setting, editor });
    }

    /// The Save button of the edit dialog. True when the dialog can close.
    fn accept_edit(&mut self) -> bool {
        let Some(edit) = &self.edit else { return true };
        let (Some(profile), Some(document)) = (&self.session.profile, &self.session.document) else { return true };
        match document.build_change(profile, &edit.setting.export_id, &edit.editor.requested_raw()) {
            Ok(change) => {
                self.session.queue_edit(change);
                true
            }
            Err(error) => {
                self.warning("Cannot queue change", error.to_string());
                false
            }
        }
    }

    fn load_nvram(&mut self) {
        // Live with nothing loaded yet, the saved file is picked first and the
        // live NVRAM is read right after, so the comparison is always against
        // what the board holds now.
        if !self.session.loaded() && !self.live() {
            self.nvram_loaded("Load NVRAM");
            return;
        }
        let mut dialog = rfd::FileDialog::new()
            .set_title("Load saved NVRAM export")
            .add_filter("SCEWIN NVRAM files", &["txt"])
            .add_filter("All files", &["*"]);
        if self.session.catalog.archive_dir.is_dir() {
            dialog = dialog.set_directory(&self.session.catalog.archive_dir);
        }
        let Some(filename) = dialog.pick_file() else { return };
        let snapshot = match load_snapshot(&filename) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.critical("Could not load NVRAM", error.to_string());
                self.log("NVRAM_LOAD_FAILED", &format!("path={}; error={error}", filename.display()));
                return;
            }
        };
        if !self.session.loaded() {
            self.load_after_export = Some(snapshot);
            self.start_export();
            return;
        }
        self.queue_from_snapshot(snapshot);
    }

    /// Queue every difference between a saved export and the loaded live one.
    fn queue_from_snapshot(&mut self, snapshot: NvramSnapshot) {
        let diff = self.session.differences_from(&snapshot);
        let (already, missing, blocked) = (diff.already, diff.missing, diff.blocked.len());
        // Queued changes for settings the saved file already has live.
        let stale: Vec<String> = diff.reset.into_iter().filter(|id| self.session.changes.contains_key(id)).collect();
        if diff.queued.is_empty() && stale.is_empty() {
            self.info(
                "Nothing to restore",
                format!(
                    "No applicable differences were found.\n\n{} setting(s) already match the live NVRAM.\n{} were \
                     not found in the live export.\n{} could not be queued.",
                    thousands(already),
                    thousands(missing),
                    thousands(blocked)
                ),
            );
            self.log(
                "NVRAM_LOAD_EMPTY",
                &format!(
                    "path={}; already_match={already}; missing={missing}; skipped={blocked}",
                    snapshot.path.display()
                ),
            );
            return;
        }

        let live_hii = self.session.document.as_ref().map(|d| d.hii_crc32.clone()).unwrap_or_default();
        let mut lines = vec![
            format!("Queue {} setting(s) from:", thousands(diff.queued.len())),
            snapshot.path.display().to_string(),
            String::new(),
        ];
        if snapshot.hii_crc32 != live_hii {
            lines.push(format!("WARNING: HII CRC differs (saved {} vs live {live_hii}).", snapshot.hii_crc32));
            lines.push("Settings are matched by token/offset/width instead.".into());
            lines.push(String::new());
        }
        for change in diff.queued.iter().take(12) {
            lines.push(format!("• {}: {} → {}", change.question, change.old_display, change.new_display));
        }
        if diff.queued.len() > 12 {
            lines.push(format!("• ...and {} more", thousands(diff.queued.len() - 12)));
        }
        if !stale.is_empty() {
            lines.push(format!(
                "• {} queued change(s) will be dropped: the saved file has the live value for them.",
                thousands(stale.len())
            ));
        }
        lines.push(String::new());
        lines.push(format!(
            "{} already match, {} not in live export, {} skipped.",
            thousands(already),
            thousands(missing),
            thousands(blocked)
        ));
        lines.push(String::new());
        lines.push("Nothing is written yet. Review under Pending Changes, then press Import.".into());
        let path = snapshot.path.display().to_string();
        let queued_summary = format!(
            "path={path}; queued={}; dropped={}; already_match={already}; missing={missing}; skipped={blocked}; \
             saved_HII={}; live_HII={live_hii}",
            diff.queued.len(),
            stale.len(),
            snapshot.hii_crc32
        );
        let cancel_summary = format!("path={path}; candidates={}", diff.queued.len());
        self.question(
            "Load NVRAM",
            lines.join("\n"),
            Action::QueueLoaded { changes: diff.queued, reset: stale, summary: queued_summary },
            Some(Action::CancelLoad { summary: cancel_summary }),
        );
    }

    fn save_dialog(title: &str, name: &str, filter: (&str, &str)) -> Option<PathBuf> {
        let cwd = std::env::current_dir().unwrap_or_default();
        let exports = cwd.join("exports");
        rfd::FileDialog::new()
            .set_title(title)
            .set_directory(if exports.is_dir() { exports } else { cwd })
            .set_file_name(name)
            .add_filter(filter.0, &[filter.1])
            .save_file()
    }

    fn export_nvram(&mut self) {
        if !self.nvram_loaded("Exporting a modified nvram.txt") {
            return;
        }
        if self.session.changes.is_empty() {
            self.info("No changes", "Queue at least one setting change first.");
            return;
        }
        let Some(filename) = Self::save_dialog("Export modified SCEWIN file", "nvram_modified_dry_run.txt", ("Text files", "txt"))
        else {
            return;
        };
        let document = self.session.document.as_ref().expect("loaded");
        match document.write_modified_copy(&filename, self.session.changes.values()) {
            Ok(output) => {
                self.log(
                    "NVRAM_EXPORT_CREATED",
                    &format!("Created modified export {} with {} queued change(s).", output.display(), self.session.changes.len()),
                );
                self.info(
                    "Dry-run export created",
                    format!("Created:\n{}\n\nThe original nvram.txt was not changed.", output.display()),
                );
            }
            Err(error) => self.critical("Export failed", error.to_string()),
        }
    }

    fn export_transaction(&mut self) {
        if !self.nvram_loaded("Exporting a transaction") {
            return;
        }
        if self.session.changes.is_empty() {
            self.info("No changes", "Queue at least one setting change first.");
            return;
        }
        let Some(filename) = Self::save_dialog("Export transaction", "bios_transaction_dry_run.json", ("JSON files", "json"))
        else {
            return;
        };
        let (Some(profile), Some(document)) = (&self.session.profile, &self.session.document) else { return };
        match write_transaction(&filename, profile, document, self.session.changes.values()) {
            Ok(output) => {
                self.log(
                    "TRANSACTION_EXPORTED",
                    &format!("Created transaction {} with {} queued change(s).", output.display(), self.session.changes.len()),
                );
                self.info("Transaction created", format!("Created:\n{}", output.display()));
            }
            Err(error) => self.critical("Export failed", error.to_string()),
        }
    }

    // ----- compare -----------------------------------------------------------

    fn load_compare_file(&mut self, role: Role) {
        let (title, label) = match role {
            Role::Stock => ("Select NVRAM 1 nvram.txt", "NVRAM 1"),
            Role::Overclocked => ("Select NVRAM 2 nvram.txt", "NVRAM 2"),
        };
        let Some(filename) = rfd::FileDialog::new()
            .set_title(title)
            .add_filter("SCEWIN NVRAM files", &["txt"])
            .add_filter("All files", &["*"])
            .pick_file()
        else {
            return;
        };
        let snapshot = match load_snapshot(&filename) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.critical("NVRAM comparison load failed", error.to_string());
                self.log("COMPARE_OPEN_FAILED", &format!("source={label}; path={}; error={error}", filename.display()));
                return;
            }
        };
        let entry = self.session.catalog_snapshot(&format!("Compare {label}"), &snapshot);
        self.add_catalog_entry(entry);
        let (count, hii) = (snapshot.settings.len(), snapshot.hii_crc32.clone());
        match role {
            Role::Stock => self.stock = Some(snapshot),
            Role::Overclocked => self.overclocked = Some(snapshot),
        }
        if self.stock.is_some() && self.overclocked.is_some() {
            self.run_comparison();
        } else {
            self.compare_summary = (
                format!("Loaded {label} ({} settings, HII {hii}). Select the other file to compare.", thousands(count)),
                self.compare_summary.1,
            );
        }
    }

    fn run_comparison(&mut self) {
        let (Some(stock), Some(overclocked)) = (self.stock.clone(), self.overclocked.clone()) else { return };
        let result = compare_snapshots(stock, overclocked);
        let mismatch = if result.hii_match {
            String::new()
        } else {
            format!(
                " WARNING: HII CRC differs ({} vs {}); offsets may not describe the same BIOS build.",
                result.stock.hii_crc32, result.overclocked.hii_crc32
            )
        };
        self.compare_summary = (
            format!(
                "Compared {} records: {} changed or missing, {} unchanged. HII CRC: {}.{mismatch}",
                thousands(result.rows.len()),
                thousands(result.changed_count()),
                thousands(result.same_count()),
                result.stock.hii_crc32
            ),
            if result.hii_match { Tone::Normal } else { Tone::Warning },
        );
        self.log(
            "COMPARE_COMPLETE",
            &format!(
                "nvram1={}; nvram2={}; changed={}; unchanged={}; HII_match={}",
                result.stock.path.display(),
                result.overclocked.path.display(),
                result.changed_count(),
                result.same_count(),
                if result.hii_match { "True" } else { "False" }
            ),
        );
        self.compare = Some(result);
        self.compare_generation += 1;
        self.compare_selected = None;
    }

    fn swap_compare_files(&mut self) {
        std::mem::swap(&mut self.stock, &mut self.overclocked);
        if self.stock.is_some() && self.overclocked.is_some() {
            self.run_comparison();
            self.log("COMPARE_FILES_SWAPPED", "NVRAM 1 and NVRAM 2 were swapped.");
        }
    }

    fn export_comparison(&mut self) {
        let Some(result) = &self.compare else {
            self.info("No comparison", "Load both NVRAM files first.");
            return;
        };
        let Some(filename) = Self::save_dialog("Export changed NVRAM settings", "nvram_changed_settings.csv", ("CSV files", "csv"))
        else {
            return;
        };
        let changed = result.changed_count();
        match export_compare_csv(&filename, result, true) {
            Ok(output) => {
                self.log("COMPARE_EXPORTED", &format!("Exported {changed} changed rows to {}.", output.display()));
                self.info("Comparison exported", format!("Exported {changed} changed setting(s):\n{}", output.display()));
            }
            Err(error) => self.critical("Comparison export failed", error.to_string()),
        }
    }

    fn refresh_compare_filter(&mut self) {
        let key = (self.compare_search.trim().to_lowercase(), self.compare_mode, self.compare_generation);
        if self.compare_key.as_ref() == Some(&key) {
            return;
        }
        let rows = self.compare.as_ref().map(|result| result.rows.as_slice()).unwrap_or_default();
        self.compare_filtered = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                match key.1 {
                    CompareMode::ChangedOnly if !row.changed => return false,
                    CompareMode::SameOnly if row.changed => return false,
                    _ => {}
                }
                if key.0.is_empty() {
                    return true;
                }
                let fields = [
                    &row.setting, &row.help, &row.stock_value, &row.overclocked_value, &row.token_hex, &row.offset_hex,
                    &row.width_hex, &row.kind, &row.status, &row.identity,
                ];
                fields.map(|f| f.as_str()).join(" ").to_lowercase().contains(&key.0)
            })
            .map(|(index, _)| index)
            .collect();
        self.compare_key = Some(key);
    }

    // ----- log ---------------------------------------------------------------

    fn open_path(&mut self, path: &Path, title: &str, what: &str) {
        if open::that_detached(path).is_err() {
            self.warning(title, format!("{what}:\n{}", path.display()));
        }
    }

    fn open_selected_nvram(&mut self) {
        let Some(entry) = self.catalog_selected.and_then(|index| self.catalog.get(index)).cloned() else {
            self.info("Select a capture", "Select a catalogued NVRAM row first.");
            return;
        };
        if entry.archived_path.is_empty() || !Path::new(&entry.archived_path).is_file() {
            self.warning("No archived NVRAM", "No archived nvram.txt was stored for this capture.");
            return;
        }
        self.open_path(Path::new(&entry.archived_path), "Could not open file", "Archived NVRAM");
    }

    fn refresh_catalog(&mut self) {
        self.catalog = self.session.catalog.entries();
        self.catalog.reverse();
        self.catalog_selected = None;
    }

    fn export_catalog(&mut self) {
        let Some(filename) = Self::save_dialog("Export NVRAM catalog", "nvram_catalog.csv", ("CSV files", "csv")) else {
            return;
        };
        match self.session.catalog.export_csv(&filename) {
            Ok(output) => self.info("Catalog exported", format!("Created:\n{}", output.display())),
            Err(error) => self.critical("Catalog export failed", error.to_string()),
        }
    }

    fn set_dark_mode(&mut self, ctx: &egui::Context, dark: bool) {
        self.dark = dark;
        theme::apply(ctx, dark);
        theme::save_dark_mode(dark);
        self.sync_caption = true;
    }

    fn status_text(&self) -> String {
        if let Some(job) = &self.job {
            return job.message.clone();
        }
        self.session.status_text(self.backend.as_ref().map(|backend| backend.backup_root.as_path()))
    }
}

// ----- drawing ---------------------------------------------------------------

fn window_title(live: bool) -> String {
    if live { format!("{APP_NAME} {VERSION}") } else { format!("{APP_NAME} {VERSION} — Dry Run") }
}

fn accent_button(ui: &mut Ui, palette: &Palette, text: &str, enabled: bool) -> egui::Response {
    let button = if enabled {
        Button::new(RichText::new(text).font(bold(12.0)).color(Color32::WHITE))
            .fill(palette.selected)
            .stroke(Stroke::new(1.0, palette.selected))
    } else {
        Button::new(RichText::new(text).font(bold(12.0)))
    };
    ui.add_enabled(enabled, button)
}

fn plain_button(ui: &mut Ui, text: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, Button::new(RichText::new(text).font(bold(12.0))))
}

fn header_cell(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).font(bold(12.0))).truncate());
}

fn cell(ui: &mut Ui, text: &str, color: Color32, strong: bool) {
    let mut rich = RichText::new(text).color(color);
    if strong {
        rich = rich.font(bold(12.0));
    }
    ui.add(egui::Label::new(rich).truncate().selectable(false));
}

const ROW_HEIGHT: f32 = 22.0;
const HEADER_HEIGHT: f32 = 28.0;

impl App {
    fn body(&mut self, ui: &mut Ui) {
        let p = self.palette();
        ui.spacing_mut().item_spacing.y = 9.0;
        if !self.live() {
            Frame::new()
                .fill(p.warning_bg)
                .stroke(Stroke::new(1.0, p.warning))
                .inner_margin(Margin::same(9))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new("DRY RUN ONLY — This build never executes SCEWIN and never writes firmware variables.")
                            .font(bold(12.0))
                            .color(p.warning),
                    );
                });
        }
        ui.label(RichText::new(self.session.profile_line()).font(bold(12.0)).color(p.muted));

        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ComboBox::from_id_salt("warning-filter")
                    .selected_text(match self.warnings {
                        WarningFilter::All => "All",
                        WarningFilter::EditableOnly => "Editable only",
                        WarningFilter::WarningsOnly => "Warnings only",
                    })
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.warnings, WarningFilter::All, "All");
                        ui.selectable_value(&mut self.warnings, WarningFilter::EditableOnly, "Editable only");
                        ui.selectable_value(&mut self.warnings, WarningFilter::WarningsOnly, "Warnings only");
                    });
                ComboBox::from_id_salt("kind-filter")
                    .selected_text(match self.kind {
                        KindFilter::All => "All",
                        KindFilter::Dropdown => "Dropdown",
                        KindFilter::Value => "Value",
                    })
                    .width(100.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.kind, KindFilter::All, "All");
                        ui.selectable_value(&mut self.kind, KindFilter::Dropdown, "Dropdown");
                        ui.selectable_value(&mut self.kind, KindFilter::Value, "Value");
                    });
                ui.add(
                    TextEdit::singleline(&mut self.search)
                        .hint_text("Search setting, help text, token, offset, or export ID...")
                        .desired_width(ui.available_width())
                        .min_size(egui::vec2(0.0, 26.0))
                        .align(egui::Align2::LEFT_CENTER),
                );
            });
        });

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            for (tab, name) in TABS {
                let selected = self.tab == tab;
                let text = RichText::new(name).font(bold(13.0)).color(if selected { Color32::WHITE } else { p.text });
                let button = Button::new(text)
                    .fill(if selected { p.selected } else { p.surface })
                    .stroke(Stroke::new(1.0, if selected { p.selected } else { p.border }))
                    .corner_radius(CornerRadius::same(5))
                    .min_size(egui::vec2(0.0, 26.0));
                if ui.add(button).clicked() {
                    self.tab = tab;
                }
            }
        });

        Frame::new()
            .stroke(Stroke::new(1.0, p.border))
            .fill(p.surface)
            .inner_margin(Margin::same(9))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                ui.spacing_mut().item_spacing.y = 6.0;
                match self.tab {
                    Tab::Nvram => self.settings_tab(ui),
                    Tab::Quick => self.quick_tab(ui),
                    Tab::Pending => self.changes_tab(ui),
                    Tab::Compare => self.compare_tab(ui),
                    Tab::Log => self.log_tab(ui),
                }
            });
    }

    /// A button row pinned to the bottom of a tab, with the table above it.
    fn with_buttons(ui: &mut Ui, buttons: impl FnOnce(&mut Ui), table: impl FnOnce(&mut Ui)) {
        egui::Panel::bottom(ui.scope_id().with("buttons"))
            .frame(Frame::new().inner_margin(Margin { top: 6, ..Margin::ZERO }))
            .show_separator_line(false)
            .show(ui, |ui| ui.horizontal(buttons));
        egui::CentralPanel::no_frame().show(ui, table);
    }

    fn settings_tab(&mut self, ui: &mut Ui) {
        self.refresh_filter();
        let p = self.palette();
        let mut clicked = None;
        let mut double = false;
        let mut press: Option<&'static str> = None;

        let buttons = self.session.nvram_buttons();
        Self::with_buttons(
            ui,
            |ui| {
                for (label, enabled) in buttons {
                    let response = match label {
                        "Import" | "Queue selected change" => accent_button(ui, p, label, enabled),
                        _ => plain_button(ui, label, enabled),
                    };
                    let response = match label {
                        "Export" => response.on_hover_text(
                            "Read the live NVRAM through SCEWIN and load it into the table. Nothing is read from the \
                             firmware until you press this.",
                        ),
                        "Import" => response.on_hover_text(
                            "Write the queued changes through SCEWIN. A backup and a verification pass run around the \
                             import.",
                        ),
                        "Load NVRAM..." => response.on_hover_text(
                            "Restore settings from a saved nvram.txt export (for example after a CMOS clear). \
                             The live NVRAM is read first if it is not loaded yet. Differences are queued for \
                             review before anything is written.",
                        ),
                        _ => response,
                    };
                    if response.clicked() {
                        press = Some(label);
                    }
                }
            },
            |ui| {
                let settings = self.session.profile.as_ref().map(|p| p.settings.as_slice()).unwrap_or_default();
                // Preserve the exact record sequence from nvram.txt. Sorting is
                // deliberately not offered, so the first rows always match the
                // source export order.
                TableBuilder::new(ui)
                    .id_salt("settings-table")
                    .sense(Sense::click())
                    .striped(false)
                    .resizable(true)
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .column(Column::initial(240.0).at_least(60.0).clip(true))
                    .column(Column::initial(360.0).at_least(60.0).clip(true))
                    .column(Column::initial(60.0).at_least(40.0).clip(true))
                    .column(Column::initial(60.0).at_least(40.0).clip(true))
                    .column(Column::initial(50.0).at_least(40.0).clip(true))
                    .column(Column::initial(150.0).at_least(60.0).clip(true))
                    .column(Column::remainder().at_least(80.0).clip(true))
                    .header(HEADER_HEIGHT, |mut header| {
                        for name in ["Setting", "Help String", "Token", "Offset", "Width", "BIOS Default", "Options/Value"] {
                            header.col(|ui| header_cell(ui, name));
                        }
                    })
                    .body(|body| {
                        body.rows(ROW_HEIGHT, self.filtered.len(), |mut row| {
                            let index = self.filtered[row.index()];
                            let setting = &settings[index];
                            let selected = self.selected_setting == Some(index);
                            row.set_selected(selected);
                            let queued = self.session.changes.contains_key(&setting.export_id);
                            let color = if selected {
                                Color32::WHITE
                            } else if !self.editable[index] {
                                p.semantic_muted
                            } else if queued {
                                p.semantic_queued
                            } else {
                                p.text
                            };
                            let bios_default = match setting.bios_default_raw.as_deref() {
                                None | Some("") => "Unknown".to_string(),
                                Some(raw) => raw.to_string(),
                            };
                            let options_value = match setting.kind {
                                Kind::Options if setting.options.is_empty() => "Unknown".to_string(),
                                Kind::Options => setting
                                    .options
                                    .iter()
                                    .map(|o| format!("{}[{}]{}", if o.selected { "*" } else { "" }, o.code_hex, o.label))
                                    .collect::<Vec<_>>()
                                    .join(" | "),
                                _ => setting.current_value.as_ref().map_or("Unknown".into(), |v| format!("<{v}>")),
                            };
                            for text in [
                                setting.question.as_str(),
                                setting.help.as_str(),
                                setting.token_hex.as_str(),
                                setting.offset_hex.as_str(),
                                setting.width_hex.as_str(),
                                bios_default.as_str(),
                                options_value.as_str(),
                            ] {
                                row.col(|ui| cell(ui, text, color, queued));
                            }
                            let response = row.response();
                            if response.clicked() {
                                clicked = Some(index);
                            }
                            if response.double_clicked() {
                                clicked = Some(index);
                                double = true;
                            }
                        });
                    });
            },
        );

        if let Some(index) = clicked {
            self.selected_setting = Some(index);
        }
        if double {
            self.edit_selected();
        }
        match press {
            Some("Export") => self.export_live_nvram(),
            Some("Import") => self.apply_changes(),
            Some("Queue selected change") => self.edit_selected(),
            Some("Load NVRAM...") => self.load_nvram(),
            _ => {}
        }
    }

    fn changes_tab(&mut self, ui: &mut Ui) {
        let p = self.palette();
        let loaded = self.session.loaded();
        let live = self.live();
        let mut press: Option<&'static str> = None;
        let mut clicked = None;
        if self.selected_change.as_ref().is_some_and(|id| !self.session.changes.contains_key(id)) {
            self.selected_change = None;
        }
        Self::with_buttons(
            ui,
            |ui| {
                if plain_button(ui, "Remove selected", true).clicked() {
                    press = Some("remove");
                }
                if plain_button(ui, "Clear all", true).clicked() {
                    press = Some("clear");
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if live
                        && accent_button(ui, p, "Import", loaded)
                            .on_hover_text(
                                "Write the queued changes through SCEWIN. A backup and a verification pass run around \
                                 the import.",
                            )
                            .clicked()
                    {
                        press = Some("import");
                    }
                    if plain_button(ui, "Export modified nvram.txt", loaded).clicked() {
                        press = Some("export");
                    }
                    if plain_button(ui, "Export transaction JSON", loaded).clicked() {
                        press = Some("transaction");
                    }
                });
            },
            |ui| {
                TableBuilder::new(ui)
                    .id_salt("changes-table")
                    .sense(Sense::click())
                    .resizable(true)
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .column(Column::initial(260.0).at_least(60.0).clip(true))
                    .column(Column::initial(520.0).at_least(60.0).clip(true))
                    .column(Column::initial(180.0).at_least(60.0).clip(true))
                    .column(Column::remainder().at_least(60.0).clip(true))
                    .header(HEADER_HEIGHT, |mut header| {
                        for name in ["Setting", "Help String", "Before", "After"] {
                            header.col(|ui| header_cell(ui, name));
                        }
                    })
                    .body(|body| {
                        body.rows(ROW_HEIGHT, self.session.changes.len(), |mut row| {
                            let Some((id, change)) = self.session.changes.get_index(row.index()) else { return };
                            let selected = self.selected_change.as_ref() == Some(id);
                            row.set_selected(selected);
                            let color = if selected { Color32::WHITE } else { p.text };
                            for text in [&change.question, &change.help, &change.old_display, &change.new_display] {
                                row.col(|ui| cell(ui, text, color, false));
                            }
                            if row.response().clicked() {
                                clicked = Some(id.clone());
                            }
                        });
                    });
            },
        );
        if let Some(id) = clicked {
            self.selected_change = Some(id);
        }
        match press {
            Some("remove") => {
                if let Some(id) = self.selected_change.take() {
                    self.session.remove_change(&id);
                }
            }
            Some("clear") => {
                self.session.clear_changes();
                self.selected_change = None;
            }
            Some("import") => self.apply_changes(),
            Some("export") => self.export_nvram(),
            Some("transaction") => self.export_transaction(),
            _ => {}
        }
    }

    fn quick_tab(&mut self, ui: &mut Ui) {
        let p = self.palette();
        ui.horizontal(|ui| {
            ui.label("Preset:");
            let current = self.session.current_preset().map(|preset| preset.name.clone()).unwrap_or_default();
            let mut chosen = None;
            ComboBox::from_id_salt("quick-preset")
                .selected_text(current)
                .width(ui.available_width() - 8.0)
                .show_ui(ui, |ui| {
                    for (index, preset) in self.session.presets.iter().enumerate() {
                        if ui.selectable_label(self.session.preset_index == Some(index), &preset.name).clicked() {
                            chosen = Some(index);
                        }
                    }
                });
            if let Some(index) = chosen.filter(|index| Some(*index) != self.session.preset_index) {
                self.session.select_preset(index);
            }
        });
        let (notice, tone) = &self.session.quick_notice;
        ui.add(egui::Label::new(RichText::new(notice).font(bold(12.0)).color(p.tone(*tone))).wrap());

        let mut queue = None;
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for layout_index in 0..self.session.quick_layout.len() {
                let (section_index, range) = self.session.quick_layout[layout_index].clone();
                let title = self.session.quick_sections[section_index].title.clone();
                ui.add_space(6.0);
                Frame::new()
                    .stroke(Stroke::new(1.0, p.border))
                    .fill(p.surface)
                    .inner_margin(Margin { left: 9, right: 9, top: 9, bottom: 9 })
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(&title).font(bold(12.0)));
                        egui::Grid::new(("quick-grid", section_index))
                            .num_columns(5)
                            .spacing([14.0, 6.0])
                            .min_row_height(26.0)
                            .show(ui, |ui| {
                                for heading in ["Setting", "Current", "New value"] {
                                    ui.label(RichText::new(heading).color(p.muted));
                                }
                                ui.end_row();
                                for index in range {
                                    let view = self.session.quick_row_view(index);
                                    let row = &mut self.session.quick_rows[index];
                                    let resolved = &row.resolved;
                                    ui.add(egui::Label::new(resolved.spec.name.as_str()).sense(Sense::hover())).on_hover_ui(|ui| {
                                        let spec = &resolved.spec;
                                        ui.label(if spec.help.is_empty() { "No firmware help text." } else { spec.help.as_str() });
                                        ui.label(format!("Identities: {}", spec.identities.join(", ")));
                                        if !resolved.missing.is_empty() {
                                            ui.label(format!("Not on this board: {}", resolved.missing.join(", ")));
                                        }
                                        if !resolved.blocked.is_empty() {
                                            ui.label(format!("Blocked: {}", resolved.blocked.join("; ")));
                                        }
                                    });
                                    ui.label(&view.current);
                                    ui.add_enabled_ui(view.enabled, |ui| match &mut row.editor {
                                        QuickEditor::Options { items, selected } => {
                                            let shown = selected.and_then(|i| items.get(i)).map(|(label, _)| label.clone()).unwrap_or_default();
                                            ComboBox::from_id_salt(("quick-editor", index)).selected_text(shown).width(240.0).show_ui(
                                                ui,
                                                |ui| {
                                                    for (item_index, (label, _)) in items.iter().enumerate() {
                                                        ui.selectable_value(selected, Some(item_index), label);
                                                    }
                                                },
                                            );
                                        }
                                        QuickEditor::Value { text } => {
                                            ui.add(
                                                TextEdit::singleline(text)
                                                    .hint_text("Decimal or 0x-prefixed hexadecimal")
                                                    .desired_width(240.0),
                                            );
                                        }
                                    });
                                    if accent_button(ui, p, "Queue", view.enabled).clicked() {
                                        queue = Some(index);
                                    }
                                    ui.label(RichText::new(&view.state).color(p.tone(view.tone)));
                                    ui.end_row();
                                }
                            });
                    });
            }
        });
        if let Some(index) = queue {
            if self.nvram_loaded("Queueing a Quick Setting") {
                let errors = self.session.queue_quick_row(index);
                if !errors.is_empty() {
                    self.warning("Cannot queue change", errors.join("\n"));
                }
            }
        }
    }

    fn compare_tab(&mut self, ui: &mut Ui) {
        self.refresh_compare_filter();
        let p = self.palette();
        let mut press: Option<&'static str> = None;
        for (label, snapshot, hint, key) in [
            ("NVRAM 1:", &self.stock, "Select the first nvram.txt export", "stock"),
            ("NVRAM 2:", &self.overclocked, "Select the second nvram.txt export", "oc"),
        ] {
            ui.horizontal(|ui| {
                ui.label(label);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if plain_button(ui, "Browse...", true).clicked() {
                        press = Some(key);
                    }
                    let mut shown = snapshot.as_ref().map(|s| s.path.display().to_string()).unwrap_or_default();
                    ui.add(
                        TextEdit::singleline(&mut shown)
                            .hint_text(hint)
                            .interactive(false)
                            .desired_width(ui.available_width())
                            .min_size(egui::vec2(0.0, 26.0))
                            .align(egui::Align2::LEFT_CENTER),
                    );
                });
            });
        }
        let (summary, tone) = &self.compare_summary;
        ui.add(egui::Label::new(RichText::new(summary).font(bold(12.0)).color(p.tone(*tone))).wrap());
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ComboBox::from_id_salt("compare-mode")
                    .selected_text(match self.compare_mode {
                        CompareMode::ChangedOnly => "Changed only",
                        CompareMode::AllEntries => "All entries",
                        CompareMode::SameOnly => "Same only",
                    })
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.compare_mode, CompareMode::ChangedOnly, "Changed only");
                        ui.selectable_value(&mut self.compare_mode, CompareMode::AllEntries, "All entries");
                        ui.selectable_value(&mut self.compare_mode, CompareMode::SameOnly, "Same only");
                    });
                ui.add(
                    TextEdit::singleline(&mut self.compare_search)
                        .hint_text("Search compared setting, value, token, offset, or status...")
                        .desired_width(ui.available_width())
                        .min_size(egui::vec2(0.0, 26.0))
                        .align(egui::Align2::LEFT_CENTER),
                );
            });
        });

        let has_result = self.compare.is_some();
        let mut clicked = None;
        Self::with_buttons(
            ui,
            |ui| {
                if plain_button(ui, "Swap files", true).clicked() {
                    press = Some("swap");
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if plain_button(ui, "Export comparison CSV", has_result).clicked() {
                        press = Some("csv");
                    }
                });
            },
            |ui| {
                let rows = self.compare.as_ref().map(|result| result.rows.as_slice()).unwrap_or_default();
                TableBuilder::new(ui)
                    .id_salt("compare-table")
                    .sense(Sense::click())
                    .resizable(true)
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .column(Column::initial(240.0).at_least(60.0).clip(true))
                    .column(Column::initial(320.0).at_least(60.0).clip(true))
                    .column(Column::initial(160.0).at_least(60.0).clip(true))
                    .column(Column::initial(160.0).at_least(60.0).clip(true))
                    .columns(Column::initial(60.0).at_least(40.0).clip(true), 3)
                    .column(Column::initial(70.0).at_least(40.0).clip(true))
                    .column(Column::remainder().at_least(80.0).clip(true))
                    .header(HEADER_HEIGHT, |mut header| {
                        for name in
                            ["Setting", "Help String", "NVRAM 1 Value", "NVRAM 2 Value", "Token", "Offset", "Width", "Type", "Status"]
                        {
                            header.col(|ui| header_cell(ui, name));
                        }
                    })
                    .body(|body| {
                        body.rows(ROW_HEIGHT, self.compare_filtered.len(), |mut row| {
                            let index = self.compare_filtered[row.index()];
                            let item = &rows[index];
                            let selected = self.compare_selected == Some(index);
                            row.set_selected(selected);
                            let color = if selected {
                                Color32::WHITE
                            } else if item.changed {
                                p.semantic_warning
                            } else {
                                p.text
                            };
                            for text in [
                                &item.setting, &item.help, &item.stock_value, &item.overclocked_value, &item.token_hex,
                                &item.offset_hex, &item.width_hex, &item.kind, &item.status,
                            ] {
                                row.col(|ui| cell(ui, text, color, item.changed));
                            }
                            if row.response().clicked() {
                                clicked = Some(index);
                            }
                        });
                    });
            },
        );
        if let Some(index) = clicked {
            self.compare_selected = Some(index);
        }
        match press {
            Some("stock") => self.load_compare_file(Role::Stock),
            Some("oc") => self.load_compare_file(Role::Overclocked),
            Some("swap") => self.swap_compare_files(),
            Some("csv") => self.export_comparison(),
            _ => {}
        }
    }

    fn log_tab(&mut self, ui: &mut Ui) {
        let p = self.palette();
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Catalog of every NVRAM the tool has opened. A new dated entry is added each time the tool \
                     starts and whenever another NVRAM export is loaded.",
                )
                .font(bold(12.0))
                .color(p.muted),
            )
            .wrap(),
        );
        ui.add(egui::Label::new(format!("Catalog file: {}", self.session.catalog.catalog_path.display())).selectable(true));

        let mut press: Option<&'static str> = None;
        let mut clicked = None;
        let mut double = false;
        Self::with_buttons(
            ui,
            |ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("Double-click a row to open the archived nvram.txt for that capture.").color(p.muted),
                    );
                    ui.horizontal(|ui| {
                        for (label, key) in [
                            ("Open NVRAM file", "open"),
                            ("Open NVRAM archive folder", "archive"),
                            ("Refresh catalog", "refresh"),
                            ("Open catalog folder", "folder"),
                        ] {
                            if plain_button(ui, label, true).clicked() {
                                press = Some(key);
                            }
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if plain_button(ui, "Export catalog CSV...", true).clicked() {
                                press = Some("csv");
                            }
                        });
                    });
                });
            },
            |ui| {
                TableBuilder::new(ui)
                    .id_salt("catalog-table")
                    .sense(Sense::click())
                    .resizable(true)
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .column(Column::initial(150.0).at_least(60.0).clip(true))
                    .column(Column::initial(180.0).at_least(60.0).clip(true))
                    .column(Column::initial(100.0).at_least(60.0).clip(true))
                    .column(Column::initial(80.0).at_least(60.0).clip(true))
                    .column(Column::initial(70.0).at_least(50.0).clip(true))
                    .column(Column::initial(480.0).at_least(80.0).clip(true))
                    .column(Column::remainder().at_least(80.0).clip(true))
                    .header(HEADER_HEIGHT, |mut header| {
                        for name in ["Date & Time", "Source", "SCEWIN Ver", "HII CRC32", "Settings", "SHA-256", "NVRAM File"] {
                            header.col(|ui| header_cell(ui, name));
                        }
                    })
                    .body(|body| {
                        body.rows(ROW_HEIGHT, self.catalog.len(), |mut row| {
                            let index = row.index();
                            let entry = &self.catalog[index];
                            let selected = self.catalog_selected == Some(index);
                            row.set_selected(selected);
                            let color = if selected { Color32::WHITE } else { p.text };
                            let archived = Path::new(&entry.archived_path)
                                .file_name()
                                .filter(|_| !entry.archived_path.is_empty())
                                .map_or("—".to_string(), |name| name.to_string_lossy().into_owned());
                            let version = if entry.amisce_version.is_empty() { "?" } else { entry.amisce_version.as_str() };
                            for text in [
                                entry.timestamp.as_str(),
                                entry.source.as_str(),
                                version,
                                entry.hii_crc32.as_str(),
                                &thousands(entry.settings),
                                entry.sha256.as_str(),
                            ] {
                                row.col(|ui| cell(ui, text, color, false));
                            }
                            row.col(|ui| {
                                let tip = if entry.archived_path.is_empty() {
                                    "No archived copy was stored.".to_string()
                                } else {
                                    entry.archived_path.clone()
                                };
                                ui.add(egui::Label::new(RichText::new(&archived).color(color)).truncate().selectable(false))
                                    .on_hover_text(tip);
                            });
                            let response = row.response();
                            if response.clicked() {
                                clicked = Some(index);
                            }
                            if response.double_clicked() {
                                clicked = Some(index);
                                double = true;
                            }
                        });
                    });
            },
        );
        if let Some(index) = clicked {
            self.catalog_selected = Some(index);
        }
        if double {
            self.open_selected_nvram();
        }
        match press {
            Some("open") => self.open_selected_nvram(),
            Some("archive") => {
                let dir = self.session.catalog.archive_dir.clone();
                let _ = std::fs::create_dir_all(&dir);
                self.open_path(&dir, "Could not open folder", "NVRAM archive");
            }
            Some("refresh") => self.refresh_catalog(),
            Some("folder") => {
                let dir = self.session.catalog.catalog_dir.clone();
                let _ = std::fs::create_dir_all(&dir);
                self.open_path(&dir, "Could not open folder", "Catalog folder");
            }
            Some("csv") => self.export_catalog(),
            _ => {}
        }
    }

    fn edit_dialog(&mut self, ui: &mut Ui) {
        let p = self.palette();
        let live = self.live();
        let Some(edit) = &mut self.edit else { return };
        let mut save = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::unique("edit-dialog")).frame(dialogs::modal_frame(ui, p)).show(ui.ctx(), |ui| {
            ui.set_width(560.0);
            ui.label(RichText::new("Queue NVRAM change").font(bold(13.0)));
            ui.add_space(6.0);
            let setting = &edit.setting;
            egui::Grid::new("edit-form").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Setting:");
                ui.label(if setting.question.is_empty() { "N/A" } else { &setting.question });
                ui.end_row();
                ui.label("Current:");
                ui.label(setting_display(setting));
                ui.end_row();
                ui.label("Identity:");
                ui.add(egui::Label::new(&setting.export_id).selectable(true));
                ui.end_row();
                ui.label("Help String:");
                let help = if setting.help.is_empty() { "No firmware help text." } else { &setting.help };
                ui.add(egui::Label::new(help).wrap());
                ui.end_row();
                ui.label("New value:");
                match &mut edit.editor {
                    QuickEditor::Options { items, selected } => {
                        let shown = selected.and_then(|i| items.get(i)).map(|(label, _)| label.clone()).unwrap_or_default();
                        ComboBox::from_id_salt("edit-editor").selected_text(shown).width(400.0).show_ui(ui, |ui| {
                            for (index, (label, _)) in items.iter().enumerate() {
                                ui.selectable_value(selected, Some(index), label);
                            }
                        });
                    }
                    QuickEditor::Value { text } => {
                        let response = ui.add(
                            TextEdit::singleline(text).hint_text("Decimal or 0x-prefixed hexadecimal").desired_width(400.0),
                        );
                        if response.lost_focus()
                            && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
                        {
                            save = true;
                        }
                    }
                }
                ui.end_row();
            });
            ui.add_space(6.0);
            let warning = if live {
                "This queues a change. It is written only after you press Import and confirm."
            } else {
                "Dry run: this queues a change but does not write firmware."
            };
            ui.label(RichText::new(warning).font(bold(12.0)).color(p.warning));
            ui.add_space(6.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
                if ui.button("Save").clicked() {
                    save = true;
                }
            });
        });
        if modal.should_close() && self.dialogs.is_empty() {
            cancel = true;
        }
        if (save && self.accept_edit()) || cancel {
            self.edit = None;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_job(&ctx);
        // Alt+F4 or the taskbar's Close would orphan SCEWIN mid-import and skip
        // the verification, so closing waits until the job is done.
        if self.job.is_some() && ctx.input(|input| input.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if self.sync_caption {
            theme::sync_title_bar(frame, self.dark);
            self.sync_caption = false;
        }
        let p = self.palette();
        shell::resize_edges(&ctx);

        egui::Panel::top("title")
            .exact_size(shell::TITLE_HEIGHT)
            .frame(Frame::new().fill(p.window))
            .show_separator_line(false)
            .show(ui, |ui| {
                if shell::title_bar(ui, p, &self.title, self.icon.as_ref(), self.dark).toggle_theme {
                    self.set_dark_mode(&ctx, !self.dark);
                }
                // A hairline between the title bar and the tabs.
                let rect = ui.max_rect();
                ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, p.border));
            });

        let status = self.status_text();
        egui::Panel::bottom("footer")
            .exact_size(shell::FOOTER_HEIGHT)
            .frame(Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)))
            .show_separator_line(false)
            .show(ui, |ui| shell::footer(ui, p, &status));

        egui::CentralPanel::default()
            .frame(Frame::new().fill(p.window).inner_margin(Margin { left: 14, right: 14, top: 11, bottom: 11 }))
            .show(ui, |ui| self.body(ui));

        if self.edit.is_some() {
            self.edit_dialog(ui);
        }
        if let Some(job) = &self.job {
            dialogs::busy(ui, p, &job.message);
        }
        // Only the topmost message is live; answering it reveals the next.
        if let Some(mut dialog) = self.dialogs.pop() {
            let depth = self.dialogs.len();
            match dialogs::show(ui, p, depth, &mut dialog) {
                None => self.dialogs.push(dialog),
                Some(Answer::Closed) | Some(Answer::No(None)) => {}
                Some(Answer::Yes(action)) | Some(Answer::No(Some(action))) => self.perform(action),
            }
        }
    }
}
