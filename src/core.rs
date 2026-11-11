// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! The source export, its parsed profile, and the safe edits between them.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::parser::{option_line_indexes, sha256_bytes, sha256_file, BiosSetting, Kind, Metadata, ProfileSource};
use crate::paths;
use crate::pycompat::{decode_latin1, encode_latin1, int_base0, repr_str, strip, text_mode, to_json, JsonStyle};

static HII_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^HIICrc32=\s*([0-9A-Fa-f]+)\s*$").unwrap());
static SETUP_QUESTION_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^Setup Question\t=").unwrap());
static TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^Token\t=\s*([0-9A-Fa-f]+)").unwrap());
static OFFSET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^Offset\t=\s*([0-9A-Fa-f]+)\s*$").unwrap());
static WIDTH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^Width\t=\s*([0-9A-Fa-f]+)\s*$").unwrap());
// The prefix is \s* (not \s+) to match the parser's OPTION_RE: AMISCE indents
// continuation rows, but a row at column 0 is still a row.
static OPTION_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<prefix>Options\t=|\s*)(?P<star>\*)?\[(?P<code>[0-9A-Fa-f]+)\](?P<tail>.*)$").unwrap()
});
static VALUE_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?P<prefix>Value\t=\s*)<(?P<value>[^>]*)>(?P<tail>.*)$").unwrap());

/// One queued edit, already validated against the profile it was built from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingChange {
    pub export_id: String,
    pub question: String,
    pub kind: Kind,
    pub old_display: String,
    pub new_display: String,
    pub new_raw: String,
    pub token_hex: String,
    pub offset_hex: String,
    pub width_hex: String,
    #[serde(default)]
    pub help: String,
}

/// Where one record sits in the source text (byte offsets into the decoded text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordSpan {
    pub export_id: String,
    pub start: usize,
    pub end: usize,
}

/// A parsed profile JSON, as `scewin-parser` or a live export writes it.
#[derive(Clone, Debug)]
pub struct ParsedProfile {
    pub path: PathBuf,
    pub label: String,
    pub metadata: Metadata,
    pub settings: Vec<BiosSetting>,
    pub source: ProfileSource,
}

impl ParsedProfile {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        // Callers only handle Error, so a missing file or malformed JSON has
        // to arrive as one.
        let text = fs::read_to_string(path)
            .map_err(|error| Error::validation(format!("Could not read the parsed profile: {error}")))?;
        let data: Value = serde_json::from_str(&text)
            .map_err(|error| Error::validation(format!("The parsed profile is not valid JSON: {error}")))?;
        let Value::Object(data) = data else {
            return Err(Error::validation("The parsed profile is not a SCEWIN profile object."));
        };
        let schema_ok = data.get("schema_version").and_then(Value::as_f64) == Some(1.0);
        if !schema_ok {
            return Err(Error::validation("Unsupported parsed profile schema."));
        }
        let Some(Value::Array(raw_settings)) = data.get("active_settings") else {
            return Err(Error::validation("Profile does not contain active_settings."));
        };
        let settings = raw_settings
            .iter()
            .map(|item| serde_json::from_value::<BiosSetting>(item.clone()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| Error::validation(format!("The parsed profile has a malformed setting: {error}")))?;
        let label = match data.get("label") {
            Some(Value::String(label)) if !label.is_empty() => label.clone(),
            _ => path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default(),
        };
        let metadata = data
            .get("metadata")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        let source = data
            .get("source")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        Ok(Self { path: path.to_path_buf(), label, metadata, settings, source })
    }

    pub fn hii_crc32(&self) -> String {
        self.metadata.hii_crc32.clone().unwrap_or_default().to_uppercase()
    }

    /// Settings by export ID. Like a Python dict built in file order, a later
    /// duplicate replaces an earlier one.
    pub fn by_id(&self) -> HashMap<&str, &BiosSetting> {
        self.settings.iter().map(|setting| (setting.export_id.as_str(), setting)).collect()
    }

    pub fn find(&self, export_id: &str) -> Option<&BiosSetting> {
        self.settings.iter().rev().find(|setting| setting.export_id == export_id)
    }

    pub fn find_mut(&mut self, export_id: &str) -> Option<&mut BiosSetting> {
        self.settings.iter_mut().rev().find(|setting| setting.export_id == export_id)
    }
}

/// The value a setting actually holds, as SCEWIN writes it.
///
/// Options report their selected code, values report their text. This is what
/// comparisons and verification test; [`setting_display`] is what people read.
pub fn raw_value(setting: &BiosSetting) -> String {
    if setting.kind == Kind::Options {
        return setting.current_code_hex.clone().unwrap_or_default().to_uppercase();
    }
    setting.current_value.clone().unwrap_or_default()
}

pub fn setting_display(setting: &BiosSetting) -> String {
    let text = if setting.kind == Kind::Options {
        setting.current_label.clone().filter(|label| !label.is_empty())
    } else {
        setting.current_value.clone()
    };
    text.unwrap_or_else(|| "Unknown".to_string())
}

const BLOCKING_WARNINGS: [&str; 7] = [
    "question_name_na",
    "missing_value_or_options",
    "unparsed_options_header",
    "unparsed_option_line",
    "multiple_selected_option_codes",
    "empty_option_label",
    "no_selected_option",
];

/// Whether a setting can be edited, and why not when it cannot. Settings the
/// parser could not read unambiguously are refused rather than guessed at.
pub fn is_editable(setting: &BiosSetting) -> (bool, String) {
    let hit: BTreeSet<&str> = setting
        .warnings
        .iter()
        .map(String::as_str)
        .filter(|warning| BLOCKING_WARNINGS.contains(warning))
        .collect();
    if !hit.is_empty() {
        return (false, hit.into_iter().collect::<Vec<_>>().join(", "));
    }
    match setting.kind {
        Kind::Options => {
            let codes: Vec<String> = setting.options.iter().map(|option| option.code_hex.to_uppercase()).collect();
            let unique: BTreeSet<&String> = codes.iter().collect();
            let empty_label = setting.options.iter().any(|option| option.label.is_empty());
            if setting.options.is_empty() || unique.len() != codes.len() || empty_label {
                return (false, "ambiguous option table".into());
            }
        }
        Kind::Value => {}
        Kind::Unknown => return (false, "unknown setting type".into()),
    }
    (true, String::new())
}

/// Check a typed value and put it in the form SCEWIN stores.
pub fn normalize_value(value: &str, width: Option<i64>) -> Result<String> {
    let cleaned = strip(value);
    if cleaned.is_empty() {
        return Err(Error::validation("Value cannot be empty."));
    }
    // Control characters include the line breaks the parser splits on, which
    // would cut the Value line in two.
    if cleaned.contains(['<', '>']) || cleaned.chars().any(char::is_control) {
        return Err(Error::validation("Value contains unsupported characters."));
    }
    if encode_latin1(cleaned).is_err() {
        return Err(Error::validation(
            "Value contains characters that a SCEWIN export cannot store. Use plain Latin-1 text.",
        ));
    }

    // Small widths are overwhelmingly numeric SCEWIN fields. Accept decimal or 0x.
    if let Some(width) = width.filter(|&width| width <= 8) {
        let number = int_base0(cleaned)
            .ok_or_else(|| Error::validation("Enter a decimal number or a 0x-prefixed hexadecimal number."))?;
        if number < 0 {
            return Err(Error::validation("Negative values are not supported by this editor."));
        }
        if width < 0 {
            return Err(Error::validation(format!("The setting reports an invalid width ({width}).")));
        }
        let maximum: u128 = (1u128 << (width * 8)) - 1;
        if number as u128 > maximum {
            return Err(Error::validation(format!(
                "Value exceeds the unsigned {width}-byte maximum ({maximum})."
            )));
        }
        return Ok(number.to_string());
    }
    Ok(cleaned.to_string())
}

/// An immutable source export plus safe functions that create modified copies.
#[derive(Clone, Debug)]
pub struct ScewinDocument {
    pub path: PathBuf,
    /// The export decoded as Latin-1, line endings and all.
    pub text: String,
    pub source_sha256: String,
    pub hii_crc32: String,
    /// Every record that has a token, offset and width, in file order.
    pub records: IndexMap<String, RecordSpan>,
}

impl ScewinDocument {
    /// Decode a SCEWIN export verbatim. Nothing is normalised: an AMISCE
    /// export is not uniformly CRLF -- real ones carry stray `\r\r\n` sequences
    /// in the header -- so rewriting line endings on the way in and out would
    /// change bytes outside the records being edited.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let read = |error: std::io::Error| Error::validation(format!("Could not read the SCEWIN export: {error}"));
        let bytes = fs::read(&path).map_err(read)?;
        let source_sha256 = sha256_bytes(&bytes);
        let text = decode_latin1(&bytes);
        let hii_crc32 = HII_RE
            .captures(&text)
            .map(|captures| captures[1].to_uppercase())
            .ok_or_else(|| Error::validation("The SCEWIN file does not contain HIICrc32."))?;
        let records = index_records(&text, &hii_crc32)?;
        Ok(Self { path, text, source_sha256, hii_crc32, records })
    }

    pub fn verify_profile(&self, profile: &ParsedProfile) -> Result<()> {
        if profile.hii_crc32() != self.hii_crc32 {
            let theirs = profile.hii_crc32();
            return Err(Error::mismatch(format!(
                "HII CRC mismatch: export is {}, profile is {}.",
                self.hii_crc32,
                if theirs.is_empty() { "missing" } else { &theirs }
            )));
        }
        let missing = profile
            .settings
            .iter()
            .filter(|setting| !self.records.contains_key(&setting.export_id))
            .count();
        if missing > 0 {
            return Err(Error::mismatch(format!(
                "{missing} profile settings are missing from the source export."
            )));
        }
        Ok(())
    }

    pub fn build_change(&self, profile: &ParsedProfile, export_id: &str, new_raw: &str) -> Result<PendingChange> {
        let setting = profile
            .find(export_id)
            .ok_or_else(|| Error::validation("The selected setting is not in the loaded profile."))?;
        let (editable, reason) = is_editable(setting);
        if !editable {
            return Err(Error::validation(format!("This setting is blocked from editing: {reason}.")));
        }

        let old_display = setting_display(setting);
        let (normalized, new_display) = if setting.kind == Kind::Options {
            let normalized = new_raw.to_uppercase();
            let matching: Vec<_> = setting
                .options
                .iter()
                .filter(|option| option.code_hex.to_uppercase() == normalized)
                .collect();
            if matching.len() != 1 {
                return Err(Error::validation("The selected option code is not unique in this setting."));
            }
            let label = matching[0].label.clone();
            (normalized, label)
        } else {
            let normalized = normalize_value(new_raw, setting.width)?;
            (normalized.clone(), normalized)
        };

        Ok(PendingChange {
            export_id: export_id.to_string(),
            question: if setting.question.is_empty() { "N/A".into() } else { setting.question.clone() },
            kind: setting.kind,
            old_display,
            new_display,
            new_raw: normalized,
            token_hex: setting.token_hex.clone(),
            offset_hex: setting.offset_hex.clone(),
            width_hex: setting.width_hex.clone(),
            help: setting.help.clone(),
        })
    }

    fn modify_block(&self, block: &str, change: &PendingChange) -> Result<String> {
        let mut lines: Vec<String> = block.split('\n').map(str::to_string).collect();
        // The writer splits on \n alone, but the parser also breaks lines at a
        // bare \r and the other splitlines boundaries. A record holding one of
        // those mid-line would be read as more rows than are rewritten here,
        // which could leave two options selected. Trailing \r is a line ending.
        let foreign_break = |line: &String| {
            line.trim_end_matches('\r').contains(['\r', '\u{0b}', '\u{0c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}'])
        };
        if lines.iter().any(foreign_break) {
            return Err(Error::validation(format!(
                "The record for {} contains an unusual line break. It was not changed.",
                change.export_id
            )));
        }
        if change.kind == Kind::Value {
            let mut changed = 0;
            for line in lines.iter_mut() {
                if let Some(captures) = VALUE_LINE_RE.captures(line) {
                    *line = format!("{}<{}>{}", &captures["prefix"], change.new_raw, &captures["tail"]);
                    changed += 1;
                }
            }
            if changed != 1 {
                return Err(Error::validation(format!(
                    "Expected one Value line for {}; found {changed}.",
                    change.export_id
                )));
            }
            return Ok(lines.join("\n"));
        }

        let mut target_hits = 0;
        let mut option_rows = 0;
        // Same membership rule as the parser. Scanning fewer rows than the
        // parser listed would leave a stale "*" behind and write a record with
        // two selected options, which SCEWIN would import as an ambiguous
        // question.
        for index in option_line_indexes(&lines) {
            let line = &lines[index];
            let Some(captures) = OPTION_LINE_RE.captures(line) else {
                // The parser records unparsed_option_line for these and
                // is_editable refuses the setting, so this is unreachable
                // unless the record changed underneath us. Refuse rather than
                // write a partially normalised block.
                return Err(Error::validation(format!(
                    "Unrecognised option row for {}: {}.",
                    change.export_id,
                    repr_str(line)
                )));
            };
            option_rows += 1;
            let code = &captures["code"];
            let star = if code.to_uppercase() == change.new_raw.to_uppercase() { "*" } else { "" };
            if !star.is_empty() {
                target_hits += 1;
            }
            let rewritten = format!("{}{star}[{code}]{}", &captures["prefix"], &captures["tail"]);
            lines[index] = rewritten;
        }

        if option_rows == 0 {
            return Err(Error::validation(format!("No option rows found for {}.", change.export_id)));
        }
        if target_hits != 1 {
            return Err(Error::validation(format!(
                "Expected one matching option code for {}; found {target_hits}.",
                change.export_id
            )));
        }
        Ok(lines.join("\n"))
    }

    /// The source text with the changes applied. Everything outside the edited
    /// records is the source, byte for byte.
    pub fn render<'a>(&self, changes: impl IntoIterator<Item = &'a PendingChange>) -> Result<String> {
        let change_map: HashMap<&str, &PendingChange> =
            changes.into_iter().map(|change| (change.export_id.as_str(), change)).collect();
        let unknown = change_map.keys().filter(|id| !self.records.contains_key(**id)).count();
        if unknown > 0 {
            return Err(Error::validation(format!("Changes reference {unknown} unknown source settings.")));
        }

        let mut spans: Vec<&RecordSpan> = self.records.values().collect();
        spans.sort_by_key(|span| span.start);
        let mut out = String::with_capacity(self.text.len());
        let mut cursor = 0;
        for span in spans {
            let Some(change) = change_map.get(span.export_id.as_str()) else {
                continue;
            };
            out.push_str(&self.text[cursor..span.start]);
            out.push_str(&self.modify_block(&self.text[span.start..span.end], change)?);
            cursor = span.end;
        }
        out.push_str(&self.text[cursor..]);
        Ok(out)
    }

    pub fn write_modified_copy<'a>(
        &self,
        output: impl AsRef<Path>,
        changes: impl IntoIterator<Item = &'a PendingChange>,
    ) -> Result<PathBuf> {
        let output = output.as_ref();
        if paths::same_file(output, &self.path) {
            return Err(Error::validation("The original SCEWIN export cannot be overwritten."));
        }
        let rendered = self.render(changes)?;
        // AMISCE exports use a legacy encoding. Line endings are whatever the
        // source had: only the edited records differ from the file this
        // document was read from, byte for byte.
        let encoded = encode_latin1(&rendered).map_err(|c| {
            Error::validation(format!(
                "The modified export contains a character that cannot be written in the SCEWIN \
                 Latin-1 encoding: {}.",
                repr_str(&c.to_string())
            ))
        })?;
        write_file(output, &encoded)?;
        Ok(output.to_path_buf())
    }
}

fn index_records(text: &str, hii_crc32: &str) -> Result<IndexMap<String, RecordSpan>> {
    let starts: Vec<usize> = SETUP_QUESTION_RE.find_iter(text).map(|found| found.start()).collect();
    let mut records = IndexMap::new();
    for (index, &start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(text.len());
        let block = &text[start..end];
        let field = |pattern: &Regex| pattern.captures(block).map(|captures| captures[1].to_uppercase());
        let (Some(token), Some(offset), Some(width)) = (field(&TOKEN_RE), field(&OFFSET_RE), field(&WIDTH_RE)) else {
            continue;
        };
        let export_id = format!("{hii_crc32}:{token}:{offset}:{width}");
        if records.contains_key(&export_id) {
            return Err(Error::validation(format!("Duplicate export identity in source: {export_id}")));
        }
        records.insert(
            export_id.clone(),
            RecordSpan { export_id, start, end },
        );
    }
    Ok(records)
}

/// Write a file, creating its folder, with an error the user can read.
pub fn write_file(output: &Path, bytes: &[u8]) -> Result<()> {
    let attempt = || -> std::io::Result<()> {
        if let Some(parent) = output.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, bytes)
    };
    attempt().map_err(|error| Error::other(format!("Could not write {}: {error}", output.display())))
}

/// A dry-run transaction: what Import would write, without writing it.
pub fn write_transaction<'a>(
    output: impl AsRef<Path>,
    profile: &ParsedProfile,
    document: &ScewinDocument,
    changes: impl IntoIterator<Item = &'a PendingChange>,
) -> Result<PathBuf> {
    let output = output.as_ref();
    let profile_sha256 = sha256_file(&profile.path)
        .map_err(|error| Error::other(format!("Could not read the profile: {error}")))?;
    let changes: Vec<&PendingChange> = changes.into_iter().collect();
    let payload = json!({
        "schema_version": 1,
        "dry_run_only": true,
        "profile_label": profile.label,
        "hii_crc32": document.hii_crc32,
        "source_nvram": document.path.display().to_string(),
        "source_sha256": document.source_sha256,
        "profile_path": profile.path.display().to_string(),
        "profile_sha256": profile_sha256,
        "changes": changes,
    });
    write_file(output, text_mode(&to_json(&payload, JsonStyle::Indent2, true)).as_bytes())?;
    Ok(output.to_path_buf())
}
