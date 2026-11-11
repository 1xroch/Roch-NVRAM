// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! The editor's state, without the window.
//!
//! What is loaded, what is queued, and what the Quick Settings page shows all
//! live here, so the rules the window follows -- which buttons work, what a
//! Queue press does, what Load NVRAM would queue -- are testable without
//! drawing anything.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use indexmap::IndexMap;

use crate::catalog::{ActivityLogger, CatalogEntry, NvramCatalog};
use crate::compare::{has_identity, identity_without_crc, NvramSnapshot};
use crate::core::{is_editable, raw_value, ParsedProfile, PendingChange, ScewinDocument};
use crate::parser::{BiosSetting, Kind};
use crate::quick_settings::{changes_for, placeholders, resolve, QuickPreset, QuickSection, ResolvedSetting};

/// How a label should be coloured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Warning,
    Queued,
    Muted,
}

/// What a saved nvram.txt would change in the live NVRAM.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedNvramDiff {
    pub queued: Vec<PendingChange>,
    pub blocked: Vec<String>,
    pub missing: usize,
    pub already: usize,
    /// Live export IDs whose saved value is already the live one. A change
    /// queued earlier for one of them would undo the restore.
    pub reset: Vec<String>,
}

/// A Quick Settings row's editor: a dropdown of option codes or a text field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuickEditor {
    /// `(label shown, option code)` pairs, and the selected index.
    Options { items: Vec<(String, String)>, selected: Option<usize> },
    Value { text: String },
}

impl QuickEditor {
    pub fn requested_raw(&self) -> String {
        match self {
            QuickEditor::Options { items, selected } => {
                selected.and_then(|index| items.get(index)).map(|(_, code)| code.clone()).unwrap_or_default()
            }
            QuickEditor::Value { text } => text.clone(),
        }
    }

    /// Select an option by its code; false when the dropdown has no such code.
    pub fn select_code(&mut self, code: &str) -> bool {
        if let QuickEditor::Options { items, selected } = self {
            if let Some(index) = items.iter().position(|(_, item)| item == code) {
                *selected = Some(index);
                return true;
            }
        }
        false
    }
}

/// Dropdown entries for a setting: `label  [code]`, each code once.
pub fn option_items(setting: Option<&BiosSetting>) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for option in setting.map(|setting| setting.options.as_slice()).unwrap_or_default() {
        let code = option.code_hex.to_uppercase();
        if seen.insert(code.clone()) {
            items.push((format!("{}  [{code}]", option.label), code));
        }
    }
    items
}

/// One Quick Settings control and its editor.
#[derive(Clone, Debug)]
pub struct QuickRow {
    pub resolved: ResolvedSetting,
    pub editor: QuickEditor,
}

impl QuickRow {
    fn new(resolved: ResolvedSetting) -> Self {
        let editor = if resolved.spec.kind == Kind::Options {
            QuickEditor::Options { items: option_items(resolved.targets.first()), selected: None }
        } else {
            QuickEditor::Value { text: String::new() }
        };
        let mut row = Self { resolved, editor };
        row.show_current();
        row
    }

    /// Start the editor on the live value, so Queue with nothing changed is a
    /// no-op.
    pub fn show_current(&mut self) {
        let Some(raw) = self.resolved.current_raw().into_iter().next() else {
            return;
        };
        match &mut self.editor {
            QuickEditor::Options { .. } => {
                self.editor.select_code(&raw);
            }
            QuickEditor::Value { text } => *text = raw,
        }
    }
}

/// What one Quick Settings row shows right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickRowView {
    pub current: String,
    pub state: String,
    pub tone: Tone,
    pub enabled: bool,
}

/// The editor without the window.
pub struct Session {
    pub live_mode: bool,
    pub profile: Option<ParsedProfile>,
    pub document: Option<ScewinDocument>,
    /// The queue, in the order changes were first queued.
    pub changes: IndexMap<String, PendingChange>,
    pub logger: ActivityLogger,
    pub catalog: NvramCatalog,
    pub presets: Vec<QuickPreset>,
    pub preset_index: Option<usize>,
    pub quick_sections: Vec<QuickSection>,
    /// Rows of every section, flattened; `quick_layout` says which belong where.
    pub quick_rows: Vec<QuickRow>,
    pub quick_layout: Vec<(usize, std::ops::Range<usize>)>,
    pub quick_notice: (String, Tone),
}

impl Session {
    pub fn new(live_mode: bool, app_root: Option<PathBuf>, presets: Vec<QuickPreset>) -> Self {
        let preset_index = (!presets.is_empty()).then_some(0);
        let mut session = Self {
            live_mode,
            profile: None,
            document: None,
            changes: IndexMap::new(),
            logger: ActivityLogger::new(app_root.clone()),
            catalog: NvramCatalog::new(app_root),
            presets,
            preset_index,
            quick_sections: Vec::new(),
            quick_rows: Vec::new(),
            quick_layout: Vec::new(),
            quick_notice: (String::new(), Tone::Normal),
        };
        session.rebuild_quick_rows();
        session
    }

    pub fn loaded(&self) -> bool {
        self.profile.is_some() && self.document.is_some()
    }

    pub fn log(&self, event: &str, message: &str) {
        // Audit trail on disk (nvram.log). The Log tab shows the catalog.
        self.logger.write(event, message);
    }

    /// The NVRAM tab's buttons, in order, and whether each can be pressed.
    pub fn nvram_buttons(&self) -> Vec<(&'static str, bool)> {
        let loaded = self.loaded();
        let mut buttons = Vec::new();
        if self.live_mode {
            // The SCEWIN round trip, as separate steps: Export reads the
            // firmware (/O) and Import writes the reviewed queue back (/I).
            buttons.push(("Export", true));
            buttons.push(("Import", loaded));
        }
        buttons.push(("Queue selected change", loaded));
        // Live, Load NVRAM reads the live NVRAM itself when nothing is loaded
        // yet; the saved file is compared against that, never written as-is.
        buttons.push(("Load NVRAM...", loaded || self.live_mode));
        buttons
    }

    pub fn profile_line(&self) -> String {
        match (&self.profile, &self.document) {
            (Some(profile), Some(document)) => format!(
                "{}   |   HII CRC32 {}   |   {} settings",
                profile.label,
                document.hii_crc32,
                thousands(profile.settings.len())
            ),
            _ => "No NVRAM loaded   |   press Export to read the live settings".into(),
        }
    }

    pub fn status_text(&self, backup_root: Option<&std::path::Path>) -> String {
        let source = match &self.document {
            Some(document) => format!("Source SHA-256: {}…", &document.source_sha256[..16.min(document.source_sha256.len())]),
            None => "No NVRAM loaded".into(),
        };
        let backups = backup_root.map(|root| format!("   |   Backups: {}", root.display())).unwrap_or_default();
        format!("{source}   |   Queued changes: {}{backups}", self.changes.len())
    }

    /// Put a freshly opened export in the table and catalogue it.
    pub fn show_nvram(&mut self, profile: ParsedProfile, document: ScewinDocument, source: &str) -> CatalogEntry {
        self.profile = Some(profile);
        self.document = Some(document);
        self.changes.clear();
        self.rebuild_quick_rows();
        self.log_nvram_open(source)
    }

    fn log_nvram_open(&self, source: &str) -> CatalogEntry {
        let profile = self.profile.as_ref().expect("an NVRAM is loaded");
        let document = self.document.as_ref().expect("an NVRAM is loaded");
        self.log(
            "NVRAM_OPEN",
            &format!(
                "{source}; path={}; HII={}; settings={}; SHA256={}",
                document.path.display(),
                document.hii_crc32,
                profile.settings.len(),
                document.source_sha256
            ),
        );
        self.catalog.record(
            source,
            &document.path,
            &document.hii_crc32,
            profile.settings.len(),
            &document.source_sha256,
            profile.metadata.amisce_version.as_deref().unwrap_or(""),
            Some(&document.path),
        )
    }

    /// Catalogue a file opened for Compare.
    pub fn catalog_snapshot(&self, label: &str, snapshot: &NvramSnapshot) -> CatalogEntry {
        self.log(
            "NVRAM_OPEN",
            &format!(
                "{label}; path={}; HII={}; settings={}; SHA256={}",
                snapshot.path.display(),
                snapshot.hii_crc32,
                snapshot.settings.len(),
                snapshot.sha256
            ),
        );
        self.catalog.record(
            label,
            &snapshot.path,
            &snapshot.hii_crc32,
            snapshot.settings.len(),
            &snapshot.sha256,
            &snapshot.amisce_version,
            Some(&snapshot.path),
        )
    }

    // ----- the queue --------------------------------------------------------

    /// Queue an edit from the NVRAM table, or drop the queued change when the
    /// edit puts the setting back to its live value.
    pub fn queue_edit(&mut self, change: PendingChange) {
        // Compare raw values, not the displayed labels: two option codes may
        // carry the same label, and comparing labels silently discarded a real
        // change (and any change already queued for the setting).
        let live = self.profile.as_ref().and_then(|profile| profile.find(&change.export_id)).map(raw_value);
        if live.as_deref() == Some(change.new_raw.as_str()) {
            if let Some(removed) = self.changes.shift_remove(&change.export_id) {
                self.log("CHANGE_REMOVED", &format!("{}: queued change was reset to the current value.", removed.question));
            }
        } else {
            self.log(
                "CHANGE_QUEUED",
                &format!(
                    "{}: {} -> {}; token={}; offset={}; width={}",
                    change.question, change.old_display, change.new_display, change.token_hex, change.offset_hex, change.width_hex
                ),
            );
            self.changes.insert(change.export_id.clone(), change);
        }
    }

    pub fn remove_change(&mut self, export_id: &str) {
        if let Some(change) = self.changes.shift_remove(export_id) {
            self.log(
                "CHANGE_REMOVED",
                &format!("{}: removed queued {} -> {}.", change.question, change.old_display, change.new_display),
            );
        }
    }

    pub fn clear_changes(&mut self) {
        let count = self.changes.len();
        self.changes.clear();
        if count > 0 {
            self.log("CHANGES_CLEARED", &format!("Cleared {count} queued change(s)."));
        }
    }

    // ----- Load NVRAM -------------------------------------------------------

    /// Match a saved nvram.txt to the live export by token/offset/width.
    ///
    /// The saved file is never written wholesale. Every difference becomes a
    /// normal pending change, so the preflight, backup, and verification path
    /// still applies to everything Load NVRAM queues.
    pub fn differences_from(&self, snapshot: &NvramSnapshot) -> SavedNvramDiff {
        let (Some(profile), Some(document)) = (&self.profile, &self.document) else {
            return SavedNvramDiff::default();
        };
        differences_from(profile, document, snapshot)
    }

    /// Queue a Load NVRAM result: drop the changes `reset` names, then queue `changes`.
    pub fn queue_all(&mut self, changes: Vec<PendingChange>, reset: &[String]) {
        for id in reset {
            self.changes.shift_remove(id);
        }
        for change in changes {
            self.changes.insert(change.export_id.clone(), change);
        }
    }

    // ----- Quick Settings ---------------------------------------------------

    pub fn current_preset(&self) -> Option<&QuickPreset> {
        self.preset_index.and_then(|index| self.presets.get(index))
    }

    pub fn select_preset(&mut self, index: usize) {
        if index < self.presets.len() {
            self.preset_index = Some(index);
            self.rebuild_quick_rows();
        }
    }

    /// Lay the selected preset out against whatever NVRAM is loaded.
    ///
    /// Rebuilt whenever the preset changes or a different export is shown, so
    /// the rows always hold the live setting records they will write to.
    pub fn rebuild_quick_rows(&mut self) {
        self.quick_sections.clear();
        self.quick_rows.clear();
        self.quick_layout.clear();
        let Some(preset) = self.current_preset().cloned() else {
            self.quick_notice = (
                "No Quick Settings presets found. Presets live in assets/quick_settings/ and are built \
                 with make-quick-settings from a stock and a tuned export."
                    .into(),
                Tone::Normal,
            );
            return;
        };
        let groups = match (&self.profile, &self.document) {
            (Some(profile), Some(document)) => resolve(&preset, profile, document),
            _ => placeholders(&preset),
        };
        for (index, (section, rows)) in groups.into_iter().enumerate() {
            let start = self.quick_rows.len();
            self.quick_rows.extend(rows.into_iter().map(QuickRow::new));
            self.quick_layout.push((index, start..self.quick_rows.len()));
            self.quick_sections.push(section);
        }
        self.refresh_quick_notice();
    }

    fn refresh_quick_notice(&mut self) {
        let Some(preset) = self.current_preset() else {
            return;
        };
        let origin = [
            preset.source.get("board").map(String::as_str),
            preset.source.get("vendor").map(String::as_str),
            Some(preset.platform.as_str()),
        ]
        .into_iter()
        .flatten()
        .find(|text| !text.is_empty())
        .unwrap_or("the tuned export")
        .to_string();
        let total = preset.setting_count();
        let name = preset.name.clone();
        let how_to = "Set a new value and press Queue, then review under Pending Changes and press Import.";
        self.quick_notice = if !self.loaded() {
            (
                format!("{name}: {total} controls. No NVRAM loaded: current values appear after you press Export."),
                Tone::Normal,
            )
        } else {
            // Identities are one vendor's layout. Say so up front when the
            // loaded export is from a different family, rather than showing a
            // page of disabled rows and leaving the reason to the tooltips.
            let available = self.quick_rows.iter().filter(|row| row.resolved.available()).count();
            if available == 0 {
                (
                    format!(
                        "{name} does not match the loaded NVRAM: 0 of {total} controls were found. It was \
                         built from {origin}. MSI, ASUS, ASRock, and Gigabyte firmware lay their settings out \
                         differently, so each vendor needs a preset built from its own stock and tuned exports \
                         (make-quick-settings). Nothing here can be queued."
                    ),
                    Tone::Warning,
                )
            } else if available < total {
                (
                    format!(
                        "{name}: {available} of {total} controls apply to the loaded NVRAM; the rest are not on \
                         this board or are blocked. {how_to}"
                    ),
                    Tone::Normal,
                )
            } else {
                (format!("{name}: all {total} controls apply. {how_to}"), Tone::Normal)
            }
        };
    }

    /// Current value, queued state, and whether the row can be used.
    pub fn quick_row_view(&self, index: usize) -> QuickRowView {
        let loaded = self.loaded();
        let resolved = &self.quick_rows[index].resolved;
        let queued = resolved
            .targets
            .iter()
            .find_map(|target| self.changes.get(&target.export_id));
        // A blocked row has no targets either, so test blocked first or a kind
        // mismatch reads as the setting being absent.
        let (state, tone) = if !loaded {
            (String::new(), Tone::Normal)
        } else if !resolved.blocked.is_empty() {
            ("blocked".into(), Tone::Warning)
        } else if resolved.targets.is_empty() {
            ("not on this board".into(), Tone::Muted)
        } else if let Some(change) = queued {
            (format!("queued → {}", change.new_display), Tone::Queued)
        } else {
            (String::new(), Tone::Normal)
        };
        QuickRowView {
            current: if loaded { resolved.current_display() } else { "—".into() },
            state,
            tone,
            enabled: loaded && resolved.available(),
        }
    }

    /// Queue (control, raw value) pairs. Returns (queued, reset, errors).
    pub fn queue_quick(&mut self, requests: &[(usize, String)], via: &str) -> (usize, usize, Vec<String>) {
        let (Some(profile), Some(document)) = (&self.profile, &self.document) else {
            return (0, 0, Vec::new());
        };
        let mut queued = 0;
        let mut reset = 0;
        let mut errors = Vec::new();
        let mut log: Vec<(&str, String)> = Vec::new();
        for (index, raw) in requests {
            let resolved = &self.quick_rows[*index].resolved;
            let (changes, reset_ids, row_errors) = changes_for(resolved, raw, profile, document);
            errors.extend(row_errors);
            for export_id in reset_ids {
                if let Some(dropped) = self.changes.shift_remove(&export_id) {
                    reset += 1;
                    log.push((
                        "CHANGE_REMOVED",
                        format!("{}: queued change was reset to the current value ({via}).", dropped.question),
                    ));
                }
            }
            for change in changes {
                log.push((
                    "CHANGE_QUEUED",
                    format!(
                        "{}: {} -> {}; token={}; offset={}; width={}; via {via}",
                        change.question, change.old_display, change.new_display, change.token_hex, change.offset_hex, change.width_hex
                    ),
                ));
                self.changes.insert(change.export_id.clone(), change);
                queued += 1;
            }
        }
        for (event, message) in log {
            self.log(event, &message);
        }
        (queued, reset, errors)
    }

    /// The Queue button of one Quick Settings row.
    pub fn queue_quick_row(&mut self, index: usize) -> Vec<String> {
        let raw = self.quick_rows[index].editor.requested_raw();
        self.queue_quick(&[(index, raw)], "Quick Settings").2
    }

    pub fn quick_row_by_name(&self, name: &str) -> Option<usize> {
        self.quick_rows.iter().position(|row| row.resolved.spec.name == name)
    }
}

/// [`Session::differences_from`] without a session.
pub fn differences_from(profile: &ParsedProfile, document: &ScewinDocument, snapshot: &NvramSnapshot) -> SavedNvramDiff {
    let mut current_by_identity: HashMap<String, &BiosSetting> = HashMap::new();
    let mut ambiguous_current = HashSet::new();
    for setting in &profile.settings {
        let key = identity_without_crc(setting);
        if !has_identity(&key) {
            continue;
        }
        match current_by_identity.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                ambiguous_current.insert(entry.key().clone());
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(setting);
            }
        }
    }
    let mut saved_counts: HashMap<String, usize> = HashMap::new();
    for setting in &snapshot.settings {
        let key = identity_without_crc(setting);
        if has_identity(&key) {
            *saved_counts.entry(key).or_default() += 1;
        }
    }

    let mut diff = SavedNvramDiff::default();
    let mut seen = HashSet::new();
    for saved in &snapshot.settings {
        let key = identity_without_crc(saved);
        if !has_identity(&key) || !seen.insert(key.clone()) {
            continue;
        }
        let name = if saved.question.is_empty() { "N/A" } else { saved.question.as_str() };
        if saved_counts[&key] > 1 || ambiguous_current.contains(&key) {
            diff.blocked.push(format!("{name}: ambiguous token/offset/width identity"));
            continue;
        }
        let Some(current) = current_by_identity.get(&key) else {
            diff.missing += 1;
            continue;
        };
        if saved.kind != current.kind {
            diff.blocked.push(format!("{name}: type differs from the live export"));
            continue;
        }
        if raw_value(saved) == raw_value(current) {
            diff.already += 1;
            diff.reset.push(current.export_id.clone());
            continue;
        }
        let (editable, reason) = is_editable(current);
        if !editable {
            diff.blocked.push(format!("{name}: {reason}"));
            continue;
        }
        match document.build_change(profile, &current.export_id, &raw_value(saved)) {
            Ok(change) => diff.queued.push(change),
            Err(error) => diff.blocked.push(format!("{name}: {error}")),
        }
    }
    diff
}

/// `f"{n:,}"`.
pub fn thousands(n: impl std::fmt::Display) -> String {
    let digits = n.to_string();
    let (sign, digits) = digits.strip_prefix('-').map_or(("", digits.as_str()), |rest| ("-", rest));
    let mut out = String::new();
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    format!("{sign}{out}")
}
