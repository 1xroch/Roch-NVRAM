// Shared helpers for the integration tests.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use roch_nvram::core::{ParsedProfile, ScewinDocument};
use roch_nvram::parser::{parse_sce_file, ProfileDocument, ProfileSource};
use roch_nvram::pycompat::{to_json, JsonStyle};

// Committed alongside the tests so a fresh clone can run them. It is a small
// synthetic export, not a board dump, but it keeps the record shapes the real
// ones have: a blocked setting, an N/A question, options, and numeric values.
pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

pub fn nvram_fixture() -> PathBuf {
    fixtures().join("nvram.txt")
}

pub fn dupes_fixture() -> PathBuf {
    fixtures().join("Dupes.txt")
}

pub const HEADER: &str = "// Script File Name : nvram.txt
// Created on 07/10/26 at 12:00:00
// AMISCE Utility. Ver 5.05.01.0002
HIICrc32= ABCD1234

";

pub fn write_latin1(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let bytes: Vec<u8> = text.chars().map(|c| u8::try_from(c as u32).expect("latin-1")).collect();
    fs::write(path, bytes).unwrap();
}

/// Write `HEADER + records` as root/nvram.txt.
pub fn write_nvram(root: &Path, records: &str) -> PathBuf {
    let nvram = root.join("nvram.txt");
    write_latin1(&nvram, &format!("{HEADER}{records}"));
    nvram
}

/// Write a profile for an export the way the tests in the original suite did.
pub fn write_profile(nvram: &Path, profile_path: &Path) {
    let parsed = parse_sce_file(nvram, "nvram", false).unwrap();
    let source = ProfileSource::default();
    let document = ProfileDocument {
        schema_version: 1,
        label: "Synthetic",
        source: &source,
        metadata: &parsed.metadata,
        active_settings: &parsed.settings,
        excluded_duplicates: &[],
    };
    fs::write(profile_path, to_json(&document, JsonStyle::Line, true)).unwrap();
}

/// A synthetic export plus its parsed profile, both opened.
pub fn build(root: &Path, records: &str) -> (ParsedProfile, ScewinDocument) {
    fs::create_dir_all(root).unwrap();
    let nvram = write_nvram(root, records);
    let profile_path = root.join("profile.json");
    write_profile(&nvram, &profile_path);
    let profile = ParsedProfile::load(&profile_path).unwrap();
    let document = ScewinDocument::open(&nvram).unwrap();
    document.verify_profile(&profile).unwrap();
    (profile, document)
}

/// The text of one record in a rendered export.
pub fn block<'a>(rendered: &'a str, question: &str) -> &'a str {
    let start = rendered.find(&format!("Setup Question\t= {question}")).expect("record present");
    let end = rendered[start + 1..].find("Setup Question\t=").map_or(rendered.len(), |i| start + 1 + i);
    &rendered[start..end]
}
