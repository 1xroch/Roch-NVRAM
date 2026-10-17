// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! Running SCEWIN: export, preflight, backup, import, and verification.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Local;
use serde_json::{json, Value};

use crate::core::{
    raw_value, setting_display, write_file, ParsedProfile, PendingChange, ScewinDocument,
};
use crate::error::{Error, Result};
use crate::parser::{parse_sce_file, sha256_file, ProfileDocument, ProfileSource};
use crate::paths;
use crate::pycompat::{decode_latin1, text_mode, to_json, JsonStyle};

const REQUIRED: [&str; 3] = ["SCEWIN_64.exe", "amifldrv64.sys", "amigendrv64.sys"];
const TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Debug)]
pub struct ScewinRunResult {
    pub returncode: i32,
    pub output: String,
    pub log_path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct LiveExport {
    pub profile: ParsedProfile,
    pub document: ScewinDocument,
    pub profile_path: PathBuf,
    pub nvram_path: PathBuf,
    pub dupes_path: Option<PathBuf>,
    pub run_result: ScewinRunResult,
}

#[derive(Clone, Debug)]
pub struct ApplyPlan {
    pub live_export: LiveExport,
    pub changes: Vec<PendingChange>,
}

#[derive(Clone, Debug)]
pub struct ApplyResult {
    pub backup_dir: PathBuf,
    pub verified_export: LiveExport,
    pub import_result: ScewinRunResult,
    pub verified_count: usize,
}

/// Whether this process has administrator rights.
pub fn is_windows_admin() -> bool {
    #[cfg(windows)]
    {
        // SAFETY: IsUserAnAdmin takes no arguments and only reads the token.
        unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Parse an export (and its Dupes.txt) into a profile JSON.
pub fn write_live_profile(nvram_path: &Path, dupes_path: Option<&Path>, output_path: &Path, label: &str) -> Result<PathBuf> {
    let read = |error: std::io::Error| Error::validation(format!("Could not parse NVRAM file: {error}"));
    let active = parse_sce_file(nvram_path, "nvram", false).map_err(read)?;
    let dupes_path = dupes_path.filter(|path| path.exists());
    let dupes = match dupes_path {
        Some(path) => Some(parse_sce_file(path, "dupes", true).map_err(read)?),
        None => None,
    };
    let source = ProfileSource {
        nvram_path: Some(nvram_path.display().to_string()),
        nvram_sha256: Some(active.sha256.clone()),
        dupes_path: dupes_path.map(|path| path.display().to_string()),
        dupes_sha256: dupes.as_ref().map(|parsed| parsed.sha256.clone()),
    };
    let document = ProfileDocument {
        schema_version: 1,
        label,
        source: &source,
        metadata: &active.metadata,
        active_settings: &active.settings,
        excluded_duplicates: dupes.as_ref().map_or(&[], |parsed| &parsed.settings),
    };
    write_file(output_path, text_mode(&to_json(&document, JsonStyle::Indent2, false)).as_bytes())?;
    Ok(output_path.to_path_buf())
}

/// Rebuild the queued changes against a fresh export, refusing every way the
/// live NVRAM can have moved since they were queued.
pub fn remap_changes<'a>(
    old_profile: &ParsedProfile,
    fresh_profile: &ParsedProfile,
    fresh_document: &ScewinDocument,
    changes: impl IntoIterator<Item = &'a PendingChange>,
) -> Result<Vec<PendingChange>> {
    let queued: Vec<&PendingChange> = changes.into_iter().collect();
    if old_profile.hii_crc32() != fresh_profile.hii_crc32() {
        return Err(Error::mismatch(format!(
            "The live HII CRC changed from {} to {}. Nothing was imported.",
            old_profile.hii_crc32(),
            fresh_profile.hii_crc32()
        )));
    }

    let old_by_id = old_profile.by_id();
    let fresh_by_id = fresh_profile.by_id();
    let mut remapped = Vec::new();
    let mut conflicts = Vec::new();

    for change in &queued {
        let (Some(old), Some(fresh)) = (old_by_id.get(change.export_id.as_str()), fresh_by_id.get(change.export_id.as_str()))
        else {
            conflicts.push(format!("{}: setting is missing from the fresh export", change.question));
            continue;
        };
        if old.kind != fresh.kind {
            conflicts.push(format!("{}: setting type changed", change.question));
            continue;
        }
        if raw_value(old) != raw_value(fresh) {
            conflicts.push(format!(
                "{}: live value changed from {} to {}",
                change.question,
                setting_display(old),
                setting_display(fresh)
            ));
            continue;
        }
        match fresh_document.build_change(fresh_profile, &change.export_id, &change.new_raw) {
            Ok(rebuilt) => remapped.push(rebuilt),
            Err(error) => conflicts.push(format!("{}: {error}", change.question)),
        }
    }

    if !conflicts.is_empty() || remapped.len() != queued.len() {
        return Err(Error::validation(format!(
            "The queued changes no longer match the live NVRAM export. Nothing was imported.\n\n{}",
            first_twenty(&conflicts)
        )));
    }
    Ok(remapped)
}

/// What did not take, after an import.
pub fn verify_changes<'a>(profile: &ParsedProfile, changes: impl IntoIterator<Item = &'a PendingChange>) -> Vec<String> {
    let by_id = profile.by_id();
    let mut mismatches = Vec::new();
    for change in changes {
        let Some(setting) = by_id.get(change.export_id.as_str()) else {
            mismatches.push(format!("{}: missing from verification export", change.question));
            continue;
        };
        // Both sides are already in the same form: raw_value upper-cases
        // option codes, and build_change stores new_raw the same way.
        if raw_value(setting) != change.new_raw {
            mismatches.push(format!(
                "{}: expected {}, but live export reports {}",
                change.question,
                change.new_display,
                setting_display(setting)
            ));
        }
    }
    mismatches
}

fn first_twenty(lines: &[String]) -> String {
    let mut detail = lines.iter().take(20).cloned().collect::<Vec<_>>().join("\n");
    if lines.len() > 20 {
        detail.push_str(&format!("\n…and {} more", lines.len() - 20));
    }
    detail
}

fn copy(source: &Path, destination: &Path) -> Result<()> {
    fs::copy(source, destination).map(|_| ()).map_err(|error| {
        Error::execution(format!("Could not copy {} to {}: {error}", source.display(), destination.display()))
    })
}

#[derive(Clone, Debug)]
pub struct ScewinBackend {
    pub project_root: PathBuf,
    pub asset_dir: PathBuf,
    pub app_root: PathBuf,
    pub runtime_dir: PathBuf,
    pub backup_root: PathBuf,
}

impl ScewinBackend {
    pub fn new(project_root: impl AsRef<Path>) -> Self {
        Self::with_app_root(project_root, paths::default_app_root())
    }

    pub fn with_app_root(project_root: impl AsRef<Path>, app_root: PathBuf) -> Self {
        let project_root = paths::resolve(project_root.as_ref());
        Self {
            asset_dir: project_root.join("scewin"),
            project_root,
            runtime_dir: app_root.join("runtime"),
            backup_root: app_root.join("backups"),
            app_root,
        }
    }

    pub fn executable(&self) -> PathBuf {
        self.runtime_dir.join("SCEWIN_64.exe")
    }

    pub fn ensure_ready(&self) -> Result<()> {
        if !cfg!(windows) {
            return Err(Error::execution("Live SCEWIN mode is only available on Windows."));
        }
        if !is_windows_admin() {
            return Err(Error::execution(
                "Administrator access is required. Close the app and start it again, approving the \
                 administrator prompt.",
            ));
        }
        let missing: Vec<&str> = REQUIRED.iter().copied().filter(|name| !self.asset_dir.join(name).is_file()).collect();
        if !missing.is_empty() {
            return Err(Error::execution(format!(
                "The bundled SCEWIN runtime is incomplete: {}",
                missing.join(", ")
            )));
        }

        let setup = |error: std::io::Error| Error::execution(format!("Could not prepare the SCEWIN runtime: {error}"));
        fs::create_dir_all(&self.runtime_dir).map_err(setup)?;
        fs::create_dir_all(&self.backup_root).map_err(setup)?;
        for name in REQUIRED {
            let source = self.asset_dir.join(name);
            let destination = self.runtime_dir.join(name);
            let stale = !destination.exists() || sha256_file(&source).map_err(setup)? != sha256_file(&destination).map_err(setup)?;
            if stale {
                copy(&source, &destination)?;
            }
        }
        Ok(())
    }

    fn run(&self, log_name: &str, args: &[&str]) -> Result<ScewinRunResult> {
        self.ensure_ready()?;
        let log_path = self.runtime_dir.join(log_name);
        let (mut reader, writer) =
            std::io::pipe().map_err(|error| Error::execution(format!("Could not start SCEWIN: {error}")))?;
        let mut child = {
            let mut command = Command::new(self.executable());
            command.args(args).current_dir(&self.runtime_dir).stdin(Stdio::null());
            let stderr = writer
                .try_clone()
                .map_err(|error| Error::execution(format!("Could not start SCEWIN: {error}")))?;
            command.stdout(writer).stderr(stderr);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                command.creation_flags(CREATE_NO_WINDOW);
            }
            command.spawn().map_err(|error| Error::execution(format!("Could not start SCEWIN: {error}")))?
            // The command, and with it this process's copy of the pipe's write
            // end, is dropped here, so the reader sees EOF when SCEWIN exits.
        };
        let collector = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = reader.read_to_end(&mut bytes);
            bytes
        });

        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if started.elapsed() >= TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(error) => return Err(Error::execution(format!("Could not start SCEWIN: {error}"))),
            }
        };
        let output = decode_latin1(&collector.join().unwrap_or_default());
        let _ = fs::write(&log_path, output.as_bytes());
        match status {
            Some(status) => Ok(ScewinRunResult { returncode: status.code().unwrap_or(-1), output, log_path }),
            None => Err(Error::execution(format!(
                "SCEWIN did not finish within {} seconds. See {}.",
                TIMEOUT.as_secs(),
                log_path.display()
            ))),
        }
    }

    fn is_valid_export(path: &Path) -> bool {
        let Ok(metadata) = fs::metadata(path) else { return false };
        if !metadata.is_file() || metadata.len() < 100 {
            return false;
        }
        let Ok(bytes) = fs::read(path) else { return false };
        let head = decode_latin1(&bytes[..bytes.len().min(2_000_000)]);
        head.contains("HIICrc32=") && head.contains("Setup Question\t=")
    }

    /// Read the live NVRAM, the way SCEWIN's Export.bat does, and parse it.
    pub fn export_current(&self, tag: &str) -> Result<LiveExport> {
        self.ensure_ready()?;
        let safe_tag: String = tag.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect();
        let nvram_name = format!("nvram_{safe_tag}.txt");
        let dupes_name = format!("Dupes_{safe_tag}.txt");
        let nvram_path = self.runtime_dir.join(&nvram_name);
        let dupes_path = self.runtime_dir.join(&dupes_name);
        let profile_path = self.runtime_dir.join(format!("profile_{safe_tag}.json"));
        for path in [&nvram_path, &dupes_path, &profile_path] {
            let _ = fs::remove_file(path);
        }

        let result = self.run(&format!("{safe_tag}_export.log"), &["/O", "/S", &nvram_name, "/SD", &dupes_name])?;
        if !Self::is_valid_export(&nvram_path) {
            let detail = result.output.trim();
            let detail = if detail.is_empty() {
                format!("SCEWIN exited with code {}.", result.returncode)
            } else {
                detail.to_string()
            };
            return Err(Error::execution(format!("SCEWIN did not create a valid NVRAM export.\n\n{detail}")));
        }

        let dupes = dupes_path.exists().then_some(dupes_path);
        write_live_profile(&nvram_path, dupes.as_deref(), &profile_path, "NVRAM")?;
        let profile = ParsedProfile::load(&profile_path)?;
        let document = ScewinDocument::open(&nvram_path)?;
        document.verify_profile(&profile)?;
        Ok(LiveExport { profile, document, profile_path, nvram_path, dupes_path: dupes, run_result: result })
    }

    /// Re-export and check that every queued change still applies.
    pub fn prepare_apply(&self, old_profile: &ParsedProfile, changes: &[PendingChange]) -> Result<ApplyPlan> {
        if changes.is_empty() {
            return Err(Error::validation("Queue at least one setting change first."));
        }
        let live = self.export_current("preapply")?;
        let remapped = remap_changes(old_profile, &live.profile, &live.document, changes)?;
        Ok(ApplyPlan { live_export: live, changes: remapped })
    }

    /// Back up, import, export again, and verify every value.
    pub fn execute_apply(&self, plan: &ApplyPlan) -> Result<ApplyResult> {
        self.ensure_ready()?;
        if plan.changes.is_empty() {
            return Err(Error::validation("The apply plan contains no changes."));
        }

        let stamp = Local::now().format("%Y%m%d_%H%M%S_%6f").to_string();
        let backup_dir = self.backup_root.join(&stamp);
        fs::create_dir_all(&self.backup_root)
            .and_then(|_| fs::create_dir(&backup_dir))
            .map_err(|error| Error::execution(format!("Could not create the backup folder: {error}")))?;

        let live = &plan.live_export;
        copy(&live.nvram_path, &backup_dir.join("nvram_before.txt"))?;
        if let Some(dupes) = live.dupes_path.as_ref().filter(|path| path.exists()) {
            copy(dupes, &backup_dir.join("Dupes_before.txt"))?;
        }
        copy(&live.run_result.log_path, &backup_dir.join("preapply_export.log"))?;

        let apply_name = "nvram_apply.txt";
        let apply_path = self.runtime_dir.join(apply_name);
        let _ = fs::remove_file(&apply_path);
        live.document.write_modified_copy(&apply_path, &plan.changes)?;
        copy(&apply_path, &backup_dir.join(apply_name))?;

        let transaction_path = backup_dir.join("transaction.json");
        let mut transaction = json!({
            "schema_version": 1,
            "dry_run_only": false,
            "created": Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
            "hii_crc32": live.document.hii_crc32,
            "source_nvram": live.nvram_path.display().to_string(),
            "source_sha256": live.document.source_sha256,
            "backup_folder": backup_dir.display().to_string(),
            "changes": plan.changes,
        });
        let save = |transaction: &Value| write_file(&transaction_path, text_mode(&to_json(transaction, JsonStyle::Indent2, true)).as_bytes());
        save(&transaction)?;

        let import_result = self.run("last_import.log", &["/I", "/S", apply_name])?;
        copy(&import_result.log_path, &backup_dir.join("import.log"))?;
        transaction["import_returncode"] = json!(import_result.returncode);
        transaction["import_output"] = json!(import_result.output);
        save(&transaction)?;

        let verified = match self.export_current("verify") {
            Ok(verified) => verified,
            Err(error) => {
                transaction["verification_error"] = json!(error.to_string());
                save(&transaction)?;
                return Err(Error::execution(format!(
                    "SCEWIN import ran, but a verification export could not be created.\n\nBackup: {}\n\n{error}",
                    backup_dir.display()
                )));
            }
        };

        copy(&verified.nvram_path, &backup_dir.join("nvram_after_verify.txt"))?;
        copy(&verified.run_result.log_path, &backup_dir.join("verify_export.log"))?;
        let mismatches = verify_changes(&verified.profile, &plan.changes);
        transaction["verification_mismatches"] = json!(mismatches);
        save(&transaction)?;

        if !mismatches.is_empty() {
            return Err(Error::execution(format!(
                "SCEWIN completed, but one or more values did not verify.\n\n{}\n\nBackup: {}",
                first_twenty(&mismatches),
                backup_dir.display()
            )));
        }

        Ok(ApplyResult {
            backup_dir,
            verified_export: verified,
            import_result,
            verified_count: plan.changes.len(),
        })
    }
}
