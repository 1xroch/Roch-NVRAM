// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! The NVRAM catalog and the activity log.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::write_file;
use crate::error::{Error, Result};
use crate::paths;
use crate::pycompat::{collapse_whitespace, csv_row, to_json, utf8_sig, JsonStyle};

fn timestamp() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut stream = OpenOptions::new().create(true).append(true).open(path)?;
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")
}

/// Persistent timestamped logger for NVRAM-related activity.
#[derive(Debug)]
pub struct ActivityLogger {
    pub app_root: PathBuf,
    pub log_dir: PathBuf,
    pub log_path: PathBuf,
    lock: Mutex<()>,
}

impl ActivityLogger {
    pub fn new(app_root: Option<PathBuf>) -> Self {
        let app_root = app_root.unwrap_or_else(paths::default_app_root);
        let log_dir = app_root.join("logs");
        let _ = fs::create_dir_all(&log_dir);
        Self { log_path: log_dir.join("nvram.log"), log_dir, app_root, lock: Mutex::new(()) }
    }

    /// Append one line; the line is returned whether or not the disk took it,
    /// because a log that cannot be written must not stop the editor.
    pub fn write(&self, event: &str, message: &str) -> String {
        let clean_event = collapse_whitespace(event).to_uppercase();
        let clean_message = collapse_whitespace(&message.replace(['\r', '\n'], " "));
        let line = format!("{} | {clean_event} | {clean_message}", timestamp());
        let _guard = self.lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = append_line(&self.log_path, &line);
        line
    }

    pub fn read_text(&self) -> String {
        fs::read(&self.log_path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).unwrap_or_default()
    }
}

/// A single catalogued NVRAM capture.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub timestamp: String,
    pub source: String,
    pub path: String,
    pub hii_crc32: String,
    pub settings: i64,
    pub sha256: String,
    pub amisce_version: String,
    pub archived_path: String,
}

impl CatalogEntry {
    /// Lenient, like the original reader: absent or null fields are empty.
    fn from_json(data: &Value) -> Option<Self> {
        let data = data.as_object()?;
        let text = |key: &str| match data.get(key) {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
        };
        let settings = match data.get("settings") {
            None | Some(Value::Null) => 0,
            Some(Value::Number(number)) => number.as_i64().or_else(|| number.as_f64().map(|f| f as i64))?,
            Some(Value::String(text)) if text.is_empty() => 0,
            Some(Value::String(text)) => text.trim().parse().ok()?,
            Some(Value::Bool(flag)) => i64::from(*flag),
            Some(_) => return None,
        };
        Some(Self {
            timestamp: text("timestamp"),
            source: text("source"),
            path: text("path"),
            hii_crc32: text("hii_crc32"),
            settings,
            sha256: text("sha256"),
            amisce_version: text("amisce_version"),
            archived_path: text("archived_path"),
        })
    }
}

/// Persistent catalog of NVRAM captures.
///
/// Unlike the activity log, this does not record individual changes. Every
/// time the tool opens an NVRAM export (manual open, live export, a
/// comparison load) one dated record is appended describing that file, and a
/// verbatim copy of the actual nvram.txt is archived alongside the catalog so
/// it can be reopened later.
#[derive(Debug)]
pub struct NvramCatalog {
    pub app_root: PathBuf,
    pub catalog_dir: PathBuf,
    pub catalog_path: PathBuf,
    pub archive_dir: PathBuf,
    lock: Mutex<()>,
}

impl NvramCatalog {
    pub fn new(app_root: Option<PathBuf>) -> Self {
        let app_root = app_root.unwrap_or_else(paths::default_app_root);
        let catalog_dir = app_root.join("logs");
        let _ = fs::create_dir_all(&catalog_dir);
        Self {
            catalog_path: catalog_dir.join("nvram_catalog.jsonl"),
            archive_dir: catalog_dir.join("nvram"),
            catalog_dir,
            app_root,
            lock: Mutex::new(()),
        }
    }

    /// Copy the actual nvram.txt into the archive and return its path.
    ///
    /// Files are content-addressed by SHA-256 so re-opening an identical NVRAM
    /// does not create duplicate copies. Every catalog row still points at the
    /// verbatim file it was captured from.
    fn archive_file(&self, source: &Path, hii_crc32: &str, sha256: &str) -> String {
        if !source.is_file() || fs::create_dir_all(&self.archive_dir).is_err() {
            return String::new();
        }
        let key: String = if sha256.is_empty() {
            Local::now().format("%Y%m%d_%H%M%S_%6f").to_string()
        } else {
            sha256.chars().take(16).collect()
        };
        let hii = if hii_crc32.is_empty() { "NVRAM" } else { hii_crc32 };
        let destination = self.archive_dir.join(format!("{hii}_{key}.txt"));
        if !destination.exists() && fs::copy(source, &destination).is_err() {
            return String::new();
        }
        destination.display().to_string()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        source: &str,
        path: &Path,
        hii_crc32: &str,
        settings: usize,
        sha256: &str,
        amisce_version: &str,
        archive_source: Option<&Path>,
    ) -> CatalogEntry {
        let hii = hii_crc32.to_uppercase();
        let archived_path = archive_source.map(|source| self.archive_file(source, &hii, sha256)).unwrap_or_default();
        let entry = CatalogEntry {
            timestamp: timestamp(),
            source: collapse_whitespace(source),
            path: path.display().to_string(),
            hii_crc32: hii,
            settings: settings as i64,
            sha256: sha256.to_string(),
            amisce_version: amisce_version.to_string(),
            archived_path,
        };
        let _guard = self.lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = append_line(&self.catalog_path, &to_json(&entry, JsonStyle::Line, true));
        entry
    }

    /// Every entry, oldest first. Unreadable lines are skipped.
    pub fn entries(&self) -> Vec<CatalogEntry> {
        let Ok(bytes) = fs::read(&self.catalog_path) else {
            return Vec::new();
        };
        String::from_utf8_lossy(&bytes)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|value| CatalogEntry::from_json(&value))
            .collect()
    }

    pub fn export_csv(&self, path: impl AsRef<Path>) -> Result<PathBuf> {
        let output = path.as_ref();
        let mut text = csv_row(&[
            "Date & Time",
            "Source",
            "SCEWIN Version",
            "HII CRC32",
            "Settings",
            "SHA-256",
            "Original Path",
            "Archived NVRAM File",
        ]);
        for entry in self.entries() {
            text.push_str(&csv_row(&[
                entry.timestamp,
                entry.source,
                entry.amisce_version,
                entry.hii_crc32,
                entry.settings.to_string(),
                entry.sha256,
                entry.path,
                entry.archived_path,
            ]));
        }
        write_file(output, &utf8_sig(&text)).map_err(|error| Error::other(error.message))?;
        Ok(output.to_path_buf())
    }
}
