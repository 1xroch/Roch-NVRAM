// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! Comparing two exports by token + offset + width.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::core::{raw_value, write_file};
use crate::error::{Error, Result};
use crate::parser::{parse_sce_file, BiosSetting, Kind};
use crate::paths;
use crate::pycompat::{csv_row, utf8_sig};

#[derive(Clone, Debug)]
pub struct NvramSnapshot {
    pub path: PathBuf,
    pub sha256: String,
    pub hii_crc32: String,
    pub amisce_version: String,
    pub settings: Vec<BiosSetting>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareRow {
    pub setting: String,
    pub help: String,
    pub stock_value: String,
    pub overclocked_value: String,
    pub token_hex: String,
    pub offset_hex: String,
    pub width_hex: String,
    pub kind: String,
    pub status: String,
    pub changed: bool,
    pub identity: String,
}

#[derive(Clone, Debug)]
pub struct CompareResult {
    pub stock: NvramSnapshot,
    pub overclocked: NvramSnapshot,
    pub rows: Vec<CompareRow>,
    pub hii_match: bool,
}

impl CompareResult {
    pub fn changed_count(&self) -> usize {
        self.rows.iter().filter(|row| row.changed).count()
    }

    pub fn same_count(&self) -> usize {
        self.rows.iter().filter(|row| !row.changed).count()
    }
}

pub fn load_snapshot(path: impl AsRef<Path>) -> Result<NvramSnapshot> {
    let source = paths::resolve(path.as_ref());
    if !source.is_file() {
        return Err(Error::validation(format!("NVRAM file not found: {}", source.display())));
    }
    let parsed = parse_sce_file(&source, "compare", false)
        .map_err(|error| Error::validation(format!("Could not parse NVRAM file: {error}")))?;
    let hii_crc32 = parsed.metadata.hii_crc32.clone().unwrap_or_default().to_uppercase();
    if hii_crc32.is_empty() {
        return Err(Error::validation("The selected file does not contain HIICrc32."));
    }
    if parsed.settings.is_empty() {
        return Err(Error::validation("The selected file does not contain SCEWIN setup records."));
    }
    Ok(NvramSnapshot {
        path: source,
        sha256: parsed.sha256,
        hii_crc32,
        amisce_version: parsed.metadata.amisce_version.clone().unwrap_or_else(|| "Unknown".into()),
        settings: parsed.settings,
    })
}

pub const EMPTY_IDENTITY: &str = "::";

/// `token:offset:width`, the identity that matches a setting across exports.
pub fn identity_without_crc(setting: &BiosSetting) -> String {
    format!(
        "{}:{}:{}",
        setting.token_hex.to_uppercase(),
        setting.offset_hex.to_uppercase(),
        setting.width_hex.to_uppercase()
    )
}

/// Whether a token/offset/width identity can be matched against another file.
///
/// A record carrying none of the three fields collapses to [`EMPTY_IDENTITY`],
/// which cannot identify anything. Every caller has to skip those the same
/// way: when the indexes dropped them but the row loops did not, two identical
/// files reported a spurious "Only in NVRAM 1" change.
pub fn has_identity(key: &str) -> bool {
    !key.is_empty() && key != EMPTY_IDENTITY
}

pub fn display_value(setting: Option<&BiosSetting>) -> String {
    let Some(setting) = setting else {
        return "—".into();
    };
    match setting.kind {
        Kind::Options => {
            let label = setting.current_label.as_deref().filter(|label| !label.is_empty());
            let code = setting.current_code_hex.as_deref().filter(|code| !code.is_empty());
            match (label, code) {
                (Some(label), Some(code)) => format!("{label} [{}]", code.to_uppercase()),
                (None, Some(code)) => format!("[{}]", code.to_uppercase()),
                _ => "Unknown".into(),
            }
        }
        Kind::Value => match &setting.current_value {
            Some(value) => format!("<{value}>"),
            None => "Unknown".into(),
        },
        Kind::Unknown => "Unknown".into(),
    }
}

/// Settings by identity, leaving out any identity that appears twice.
pub fn unique_index(settings: &[BiosSetting]) -> (HashMap<String, &BiosSetting>, HashSet<String>) {
    let mut index: HashMap<String, &BiosSetting> = HashMap::new();
    let mut ambiguous = HashSet::new();
    for setting in settings {
        let key = identity_without_crc(setting);
        if !has_identity(&key) {
            continue;
        }
        match index.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                ambiguous.insert(entry.key().clone());
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(setting);
            }
        }
    }
    for key in &ambiguous {
        index.remove(key);
    }
    (index, ambiguous)
}

pub fn compare_snapshots(stock: NvramSnapshot, overclocked: NvramSnapshot) -> CompareResult {
    let rows = {
        let (stock_index, stock_ambiguous) = unique_index(&stock.settings);
        let (oc_index, oc_ambiguous) = unique_index(&overclocked.settings);
        let ambiguous: HashSet<&String> = stock_ambiguous.iter().chain(oc_ambiguous.iter()).collect();

        let make_row = |key: &str, stock_setting: Option<&BiosSetting>, oc_setting: Option<&BiosSetting>| {
            let base = stock_setting.or(oc_setting);
            let (status, changed) = match (stock_setting, oc_setting) {
                _ if ambiguous.contains(&key.to_string()) => ("Ambiguous identity", true),
                (None, _) => ("Only in NVRAM 2", true),
                (_, None) => ("Only in NVRAM 1", true),
                (Some(a), Some(b)) if a.kind != b.kind => ("Type changed", true),
                (Some(a), Some(b)) if raw_value(a) != raw_value(b) => ("Changed", true),
                _ => ("Same", false),
            };

            // Only value/type/presence differences count as a change. A
            // differing setting name (the question label) is shown for context
            // but never marks a row as changed on its own -- matching is by
            // token/offset/width identity.
            let stock_name = stock_setting.map(|s| s.question.as_str()).unwrap_or("");
            let oc_name = oc_setting.map(|s| s.question.as_str()).unwrap_or("");
            let mut name = [stock_name, oc_name, "N/A"].into_iter().find(|n| !n.is_empty()).unwrap().to_string();
            if !stock_name.is_empty() && !oc_name.is_empty() && stock_name != oc_name {
                name = format!("{stock_name} → {oc_name}");
            }

            CompareRow {
                setting: name,
                help: base.map(|s| s.help.clone()).unwrap_or_default(),
                stock_value: display_value(stock_setting),
                overclocked_value: display_value(oc_setting),
                token_hex: base.map(|s| s.token_hex.clone()).unwrap_or_default(),
                offset_hex: base.map(|s| s.offset_hex.clone()).unwrap_or_default(),
                width_hex: base.map(|s| s.width_hex.clone()).unwrap_or_default(),
                kind: base.map_or("unknown", |s| s.kind.as_str()).to_string(),
                status: status.to_string(),
                changed,
                identity: key.to_string(),
            }
        };

        // Preserve the exact NVRAM 1 order. Entries found only in NVRAM 2 are
        // appended in their original NVRAM 2 file order.
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        for setting in &stock.settings {
            let key = identity_without_crc(setting);
            if !has_identity(&key) || !seen.insert(key.clone()) {
                continue;
            }
            rows.push(make_row(&key, Some(setting), oc_index.get(&key).copied()));
        }
        for setting in &overclocked.settings {
            let key = identity_without_crc(setting);
            if !has_identity(&key) || !seen.insert(key.clone()) {
                continue;
            }
            rows.push(make_row(&key, stock_index.get(&key).copied(), Some(setting)));
        }
        rows
    };

    let hii_match = stock.hii_crc32 == overclocked.hii_crc32;
    CompareResult { stock, overclocked, rows, hii_match }
}

pub fn compare_files(stock: impl AsRef<Path>, overclocked: impl AsRef<Path>) -> Result<CompareResult> {
    Ok(compare_snapshots(load_snapshot(stock)?, load_snapshot(overclocked)?))
}

pub fn export_compare_csv(path: impl AsRef<Path>, result: &CompareResult, changed_only: bool) -> Result<PathBuf> {
    let output = path.as_ref();
    let mut text = csv_row(&[
        "Setting",
        "Help String",
        "NVRAM 1 Value",
        "NVRAM 2 Value",
        "Token",
        "Offset",
        "Width",
        "Type",
        "Status",
        "Identity",
    ]);
    for row in result.rows.iter().filter(|row| row.changed || !changed_only) {
        text.push_str(&csv_row(&[
            &row.setting,
            &row.help,
            &row.stock_value,
            &row.overclocked_value,
            &row.token_hex,
            &row.offset_hex,
            &row.width_hex,
            &row.kind,
            &row.status,
            &row.identity,
        ]));
    }
    write_file(output, &utf8_sig(&text))
        .map_err(|error| Error::other(format!("Could not export comparison CSV: {error}")))?;
    Ok(output.to_path_buf())
}
