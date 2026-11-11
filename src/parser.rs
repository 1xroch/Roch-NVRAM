// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! Parse AMI SCEWIN/AMISCE nvram.txt exports without executing SCEWIN.

use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::pycompat::{decode_latin1, int_base16, splitlines, strip};

static OPTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?P<selected>\*)?\[(?P<code>[0-9A-Fa-f]+)\](?P<label>.*?)(?:\s*//.*)?$").unwrap()
});
static ENUM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(?P<code>[0-9A-Fa-f]+)\](?P<label>.*)$").unwrap());
static VALUE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^<(?P<value>[^>]*)>").unwrap());

pub const OPTIONS_PREFIX: &str = "Options\t=";
pub const FIELD_PREFIXES: [&str; 7] = [
    "Setup Question\t=",
    "Help String\t=",
    "Token\t=",
    "Offset\t=",
    "Width\t=",
    "BIOS Default\t=",
    "Value\t=",
];

/// Indexes of the lines that make up a record's Options block.
///
/// The reader ([`parse_record`]) and the writer (`ScewinDocument`) must agree
/// on this exactly. When the writer scanned fewer lines than the reader listed
/// it left a stale `*` behind and produced an import record with two selected
/// options, so the rule lives here once and both call it.
///
/// A row belongs to the block from the `Options\t=` line until the first blank
/// line or the first line that starts another record field. Indentation is not
/// part of the rule: AMISCE indents continuation rows, but a row at column 0 is
/// still a row.
pub fn option_line_indexes<S: AsRef<str>>(lines: &[S]) -> Vec<usize> {
    let mut indexes = Vec::new();
    let mut in_options = false;
    for (index, line) in lines.iter().enumerate() {
        let line = line.as_ref();
        if line.starts_with(OPTIONS_PREFIX) {
            in_options = true;
            indexes.push(index);
        } else if FIELD_PREFIXES.iter().any(|prefix| line.starts_with(prefix)) {
            in_options = false;
        } else if !in_options {
            continue;
        } else if strip(line).is_empty() {
            in_options = false;
        } else {
            indexes.push(index);
        }
    }
    indexes
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut digest = Sha256::new();
    let mut stream = fs::File::open(path)?;
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex(&digest.finalize()))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// SCEWIN exports are commonly ANSI/Latin-1 with mixed CRLF/CR line endings.
pub fn read_sce_text(path: &Path) -> io::Result<String> {
    let raw = fs::read(path)?;
    Ok(decode_latin1(&raw).replace("\r\n", "\n").replace('\r', "\n"))
}

/// What kind of record a setting is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A dropdown: an Options block with one selected row.
    Options,
    /// A free value: `Value\t=<...>`.
    Value,
    #[default]
    #[serde(other)]
    Unknown,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Options => "options",
            Kind::Value => "value",
            Kind::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BiosOption {
    pub code_hex: String,
    pub code: i64,
    pub label: String,
    pub selected: bool,
}

/// One setup question. Field names and order are those of the profile JSON.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BiosSetting {
    pub source: String,
    pub question: String,
    pub help: String,
    pub token_hex: String,
    pub token: Option<i64>,
    pub offset_hex: String,
    pub offset: Option<i64>,
    pub width_hex: String,
    pub width: Option<i64>,
    pub kind: Kind,

    pub bios_default_raw: Option<String>,
    pub bios_default_type: Option<String>,
    pub bios_default_code_hex: Option<String>,
    pub bios_default_label: Option<String>,
    pub bios_default_value: Option<String>,

    pub current_value: Option<String>,
    pub current_code_hex: Option<String>,
    pub current_code: Option<i64>,
    pub current_label: Option<String>,

    pub options: Vec<BiosOption>,
    pub warnings: Vec<String>,
    pub export_id: String,
}

impl BiosSetting {
    fn new(source: &str) -> Self {
        Self { source: source.to_string(), ..Self::default() }
    }

    fn finalize(&mut self, hii_crc32: Option<&str>) {
        let crc = hii_crc32.filter(|crc| !crc.is_empty()).unwrap_or("NOCRC");
        let or = |value: &str, missing: &'static str| {
            if value.is_empty() { missing.to_string() } else { value.to_string() }
        };
        self.export_id = format!(
            "{crc}:{}:{}:{}",
            or(&self.token_hex, "NOTOKEN"),
            or(&self.offset_hex, "NOOFFSET"),
            or(&self.width_hex, "NOWIDTH"),
        );

        if self.bios_default_raw.as_deref().unwrap_or("").is_empty() {
            self.warnings.push("missing_bios_default".into());
        }

        if self.kind == Kind::Options {
            let selected: Vec<&BiosOption> = self.options.iter().filter(|option| option.selected).collect();
            let mut selected_codes: Vec<&str> = selected.iter().map(|option| option.code_hex.as_str()).collect();
            selected_codes.sort_unstable();
            selected_codes.dedup();

            if selected.is_empty() {
                self.warnings.push("no_selected_option".into());
            } else if selected_codes.len() == 1 {
                let chosen = selected[0].clone();
                let duplicate_rows = selected.len() > 1;
                self.current_code_hex = Some(chosen.code_hex);
                self.current_code = Some(chosen.code);
                self.current_label = Some(chosen.label);
                if duplicate_rows {
                    self.warnings.push("duplicate_selected_option_rows".into());
                }
            } else {
                self.warnings.push("multiple_selected_option_codes".into());
            }

            let mut keys: Vec<(&str, &str)> = self
                .options
                .iter()
                .map(|option| (option.code_hex.as_str(), option.label.as_str()))
                .collect();
            let total = keys.len();
            keys.sort_unstable();
            keys.dedup();
            if keys.len() != total {
                self.warnings.push("duplicate_option_rows".into());
            }
            if self.options.iter().any(|option| option.label.is_empty()) {
                self.warnings.push("empty_option_label".into());
            }
        }

        if self.question == "N/A" {
            self.warnings.push("question_name_na".into());
        }
    }
}

/// The header fields of an export, in the order the profile JSON lists them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_file_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_on: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amisce_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hii_crc32: Option<String>,
}

static METADATA_RES: LazyLock<[Regex; 4]> = LazyLock::new(|| {
    [
        Regex::new(r"(?m)// Script File Name\s*:\s*(.+)").unwrap(),
        Regex::new(r"(?m)// Created on\s+(.+?)\s*$").unwrap(),
        Regex::new(r"(?m)// AMISCE Utility\. Ver\s+([^\s]+)").unwrap(),
        Regex::new(r"(?m)HIICrc32=\s*([0-9A-Fa-f]+)").unwrap(),
    ]
});

pub fn parse_metadata(text: &str) -> Metadata {
    let found = |index: usize| {
        METADATA_RES[index]
            .captures(text)
            .map(|captures| strip(&captures[1]).to_string())
    };
    Metadata {
        script_file_name: found(0),
        created_on: found(1),
        amisce_version: found(2),
        // Upper-cased as ScewinDocument does, so export IDs agree with it.
        hii_crc32: found(3).map(|crc| crc.to_uppercase()),
    }
}

/// Dupes.txt carries its records commented out with `// `.
pub fn normalize_commented_dupes(text: &str) -> String {
    splitlines(text)
        .into_iter()
        .map(|line| {
            if let Some(rest) = line.strip_prefix("// ") {
                rest
            } else if line == "//" {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn split_records(text: &str) -> Vec<Vec<&str>> {
    let lines = splitlines(text);
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("Setup Question\t="))
        .map(|(index, _)| index)
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(index, &start)| {
            let end = starts.get(index + 1).copied().unwrap_or(lines.len());
            lines[start..end].to_vec()
        })
        .collect()
}

pub fn parse_hex(value: &str) -> Option<i64> {
    int_base16(value).and_then(|number| i64::try_from(number).ok())
}

/// Everything after the first `\t=` of a field line.
fn field_value(line: &str) -> &str {
    line.split_once("\t=").map_or("", |(_, rest)| rest)
}

pub fn parse_record(block: &[&str], source: &str, hii_crc32: Option<&str>) -> BiosSetting {
    let mut setting = BiosSetting::new(source);
    // Option-block membership is decided by the shared rule so the reader and
    // the writer can never disagree about which rows are options.
    let option_lines = option_line_indexes(block);

    for (index, &line) in block.iter().enumerate() {
        if option_lines.binary_search(&index).is_ok() {
            let header = line.starts_with(OPTIONS_PREFIX);
            if header {
                setting.kind = Kind::Options;
            }
            let row = if header { field_value(line) } else { line };
            match OPTION_RE.captures(row) {
                Some(captures) => {
                    let code = &captures["code"];
                    setting.options.push(BiosOption {
                        code_hex: code.to_uppercase(),
                        code: parse_hex(code).unwrap_or(i64::MAX),
                        label: strip(&captures["label"]).to_string(),
                        selected: captures.name("selected").is_some(),
                    });
                }
                None => setting.warnings.push(
                    if header { "unparsed_options_header" } else { "unparsed_option_line" }.into(),
                ),
            }
        } else if line.starts_with("Setup Question\t=") {
            setting.question = strip(field_value(line)).to_string();
        } else if line.starts_with("Help String\t=") {
            setting.help = strip(field_value(line)).to_string();
        } else if line.starts_with("Token\t=") {
            let value = field_value(line);
            let value = value.split_once("//").map_or(value, |(before, _)| before);
            let value = strip(value).to_uppercase();
            setting.token = parse_hex(&value);
            setting.token_hex = value;
        } else if line.starts_with("Offset\t=") {
            let value = strip(field_value(line)).to_uppercase();
            setting.offset = parse_hex(&value);
            setting.offset_hex = value;
        } else if line.starts_with("Width\t=") {
            let value = strip(field_value(line)).to_uppercase();
            setting.width = parse_hex(&value);
            setting.width_hex = value;
        } else if line.starts_with("BIOS Default\t=") {
            let value = strip(field_value(line)).to_string();
            if let Some(captures) = ENUM_RE.captures(&value) {
                setting.bios_default_type = Some("enum".into());
                setting.bios_default_code_hex = Some(captures["code"].to_uppercase());
                setting.bios_default_label = Some(strip(&captures["label"]).to_string());
            } else if let Some(captures) = VALUE_RE.captures(&value) {
                setting.bios_default_type = Some("value".into());
                setting.bios_default_value = Some(captures["value"].to_string());
            } else {
                setting.bios_default_type = Some("other".into());
            }
            setting.bios_default_raw = Some(value);
        } else if line.starts_with("Value\t=") {
            setting.kind = Kind::Value;
            let value = strip(field_value(line));
            setting.current_value = Some(match VALUE_RE.captures(value) {
                Some(captures) => captures["value"].to_string(),
                None => value.to_string(),
            });
        }
    }

    if setting.kind == Kind::Unknown {
        setting.warnings.push("missing_value_or_options".into());
    }
    setting.finalize(hii_crc32);
    setting
}

/// One parsed export file.
#[derive(Clone, Debug)]
pub struct ParsedFile {
    pub path: String,
    pub sha256: String,
    pub metadata: Metadata,
    pub settings: Vec<BiosSetting>,
}

pub fn parse_sce_file(path: &Path, source: &str, commented: bool) -> io::Result<ParsedFile> {
    let text = read_sce_text(path)?;
    let metadata = parse_metadata(&text);
    let parse_text = if commented { normalize_commented_dupes(&text) } else { text };
    let settings = split_records(&parse_text)
        .iter()
        .map(|block| parse_record(block, source, metadata.hii_crc32.as_deref()))
        .collect();
    Ok(ParsedFile {
        path: path.display().to_string(),
        sha256: sha256_file(path)?,
        metadata,
        settings,
    })
}

/// Profile `source` block: where a parsed profile came from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileSource {
    pub nvram_path: Option<String>,
    pub nvram_sha256: Option<String>,
    pub dupes_path: Option<String>,
    pub dupes_sha256: Option<String>,
}

/// The parsed profile JSON as `scewin-parser` and the live export write it.
#[derive(Clone, Debug, Serialize)]
pub struct ProfileDocument<'a> {
    pub schema_version: u32,
    pub label: &'a str,
    pub source: &'a ProfileSource,
    pub metadata: &'a Metadata,
    pub active_settings: &'a [BiosSetting],
    pub excluded_duplicates: &'a [BiosSetting],
}

/// Counts over a parsed export, for the `--summary` output.
pub fn make_summary(
    active: &[BiosSetting],
    dupes: &[BiosSetting],
    metadata: &Metadata,
) -> IndexMap<String, serde_json::Value> {
    use serde_json::{json, Value};

    let mut question_groups: IndexMap<&str, usize> = IndexMap::new();
    let mut offset_width_groups: IndexMap<(&str, &str), usize> = IndexMap::new();
    let mut kind_counts: IndexMap<&str, usize> = IndexMap::new();
    let mut width_counts: IndexMap<Option<i64>, usize> = IndexMap::new();
    let mut warning_counts: IndexMap<&str, usize> = IndexMap::new();
    for setting in active {
        *question_groups.entry(&setting.question).or_default() += 1;
        *offset_width_groups.entry((&setting.offset_hex, &setting.width_hex)).or_default() += 1;
        *kind_counts.entry(setting.kind.as_str()).or_default() += 1;
        *width_counts.entry(setting.width).or_default() += 1;
        for warning in &setting.warnings {
            *warning_counts.entry(warning).or_default() += 1;
        }
    }
    let mut widths: Vec<(Option<i64>, usize)> = width_counts.into_iter().collect();
    widths.sort_by_key(|(width, _)| width.unwrap_or(-1));
    let width_byte_counts: serde_json::Map<String, Value> = widths
        .into_iter()
        .map(|(width, count)| (width.map_or("None".to_string(), |w| w.to_string()), json!(count)))
        .collect();

    let groups = |counts: &mut dyn Iterator<Item = usize>| {
        let sizes: Vec<usize> = counts.filter(|&count| count > 1).collect();
        (sizes.len(), sizes.iter().sum::<usize>())
    };
    let (name_groups, name_settings) = groups(&mut question_groups.values().copied());
    let (ow_groups, ow_settings) = groups(&mut offset_width_groups.values().copied());

    let mut summary = IndexMap::new();
    summary.insert("metadata".into(), json!(metadata));
    summary.insert("active_setting_count".into(), json!(active.len()));
    summary.insert("excluded_duplicate_setting_count".into(), json!(dupes.len()));
    summary.insert("kind_counts".into(), json!(kind_counts));
    summary.insert("width_byte_counts".into(), Value::Object(width_byte_counts));
    summary.insert("unique_display_name_count".into(), json!(question_groups.len()));
    summary.insert("duplicate_display_name_group_count".into(), json!(name_groups));
    summary.insert("settings_in_duplicate_display_name_groups".into(), json!(name_settings));
    summary.insert("duplicate_offset_width_group_count".into(), json!(ow_groups));
    summary.insert("settings_in_duplicate_offset_width_groups".into(), json!(ow_settings));
    summary.insert("warning_counts".into(), json!(warning_counts));
    summary.insert(
        "identity_note".into(),
        json!(
            "export_id uses HII CRC + token + offset + width. It is suitable for \
             matching this exact SCEWIN export family, but it is not a substitute \
             for a UEFI variable-store GUID and must be invalidated when the HII \
             CRC or BIOS version changes."
        ),
    );
    summary
}
