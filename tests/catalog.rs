// The catalog of every NVRAM the tool has opened, and its archive.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use roch_nvram::catalog::NvramCatalog;
use tempfile::TempDir;

use common::write_latin1;

const NVRAM_TEXT: &str = "// Script File Name : nvram.txt
// Created on 07/10/26 at 12:00:00
// AMISCE Utility. Ver 5.05.01.0002
HIICrc32= ABCD1234

Setup Question\t= Test Option
Token\t= 10
Offset\t= 20
Width\t= 01
";

fn make_nvram(root: &Path) -> PathBuf {
    let path = root.join("nvram.txt");
    write_latin1(&path, NVRAM_TEXT);
    path
}

#[test]
fn record_appends_dated_entry_with_version() {
    let temp = TempDir::new().unwrap();
    let catalog = NvramCatalog::new(Some(temp.path().to_path_buf()));
    let source = make_nvram(temp.path());
    let entry = catalog.record("Startup", &source, "abcd1234", 42, &"deadbeef".repeat(8), "5.05.01.0002", Some(&source));
    assert!(Regex::new(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$").unwrap().is_match(&entry.timestamp));
    assert_eq!(entry.hii_crc32, "ABCD1234");
    assert_eq!(entry.amisce_version, "5.05.01.0002");
    assert_eq!(catalog.catalog_path.file_name().unwrap(), "nvram_catalog.jsonl");

    let entries = catalog.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].settings, 42);
    // One JSON object per line, laid out as the original wrote it.
    let line = fs::read_to_string(&catalog.catalog_path).unwrap();
    assert!(line.starts_with("{\"timestamp\": \""), "{line}");
    assert!(line.contains("\"settings\": 42, "));
}

#[test]
fn actual_nvram_is_archived_verbatim() {
    let temp = TempDir::new().unwrap();
    let catalog = NvramCatalog::new(Some(temp.path().to_path_buf()));
    let source = make_nvram(temp.path());
    let entry = catalog.record("Startup", &source, "ABCD1234", 1, &"a".repeat(64), "", Some(&source));
    assert!(!entry.archived_path.is_empty());
    let archived = PathBuf::from(&entry.archived_path);
    assert!(archived.is_file());
    assert_eq!(archived.file_name().unwrap(), "ABCD1234_aaaaaaaaaaaaaaaa.txt");
    assert_eq!(fs::read(archived).unwrap(), fs::read(source).unwrap());
}

#[test]
fn identical_nvram_is_deduplicated() {
    let temp = TempDir::new().unwrap();
    let catalog = NvramCatalog::new(Some(temp.path().to_path_buf()));
    let source = make_nvram(temp.path());
    let sha = "b".repeat(64);
    let first = catalog.record("Startup", &source, "ABCD1234", 1, &sha, "", Some(&source));
    let second = catalog.record("Manual open", &source, "ABCD1234", 1, &sha, "", Some(&source));
    assert_eq!(first.archived_path, second.archived_path);
    assert_eq!(fs::read_dir(&catalog.archive_dir).unwrap().count(), 1);
    assert_eq!(catalog.entries().len(), 2);
}

#[test]
fn lines_written_by_earlier_releases_still_load() {
    let temp = TempDir::new().unwrap();
    let catalog = NvramCatalog::new(Some(temp.path().to_path_buf()));
    fs::write(
        &catalog.catalog_path,
        "{\"timestamp\": \"2026-09-01 10:00:00\", \"source\": \"Startup\", \"path\": \"C:\\\\nvram.txt\", \
         \"hii_crc32\": \"ABCD1234\", \"settings\": 5000, \"sha256\": \"x\"}\n\nnot json\n",
    )
    .unwrap();
    let entries = catalog.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].settings, 5000);
    assert_eq!(entries[0].path, "C:\\nvram.txt");
    assert_eq!(entries[0].archived_path, "");
}

#[test]
fn export_csv_contains_version_and_archive_columns() {
    let temp = TempDir::new().unwrap();
    let catalog = NvramCatalog::new(Some(temp.path().to_path_buf()));
    let source = make_nvram(temp.path());
    catalog.record("Startup", &source, "ABCD1234", 1, &"c".repeat(64), "5.05.01.0002", Some(&source));
    let output = catalog.export_csv(temp.path().join("out").join("catalog.csv")).unwrap();
    let text = String::from_utf8(fs::read(output).unwrap()).unwrap();
    assert!(text.contains("SCEWIN Version"));
    assert!(text.contains("Archived NVRAM File"));
    assert!(text.contains("5.05.01.0002"));
}
