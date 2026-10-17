// Parsing, record order, edits, and the files they produce.

mod common;

use std::fs;

use roch_nvram::backend::write_live_profile;
use roch_nvram::core::{ParsedProfile, ScewinDocument};
use roch_nvram::core::write_transaction;
use roch_nvram::parser::BiosSetting;
use serde_json::Value;
use tempfile::TempDir;

use common::*;

struct Fixture {
    _temp: TempDir,
    profile: ParsedProfile,
    document: ScewinDocument,
}

impl Fixture {
    fn new() -> Self {
        // The profile is derived from the committed export instead of being
        // committed next to it, so the two cannot drift apart.
        let temp = TempDir::new().unwrap();
        let profile_path = temp.path().join("profile.json");
        write_live_profile(&nvram_fixture(), Some(&dupes_fixture()), &profile_path, "NVRAM").unwrap();
        let profile = ParsedProfile::load(&profile_path).unwrap();
        let document = ScewinDocument::open(nvram_fixture()).unwrap();
        document.verify_profile(&profile).unwrap();
        Self { _temp: temp, profile, document }
    }

    fn find(&self, question: &str) -> &BiosSetting {
        self.profile.settings.iter().find(|item| item.question == question).expect(question)
    }
}

#[test]
fn profile_matches_source() {
    let fx = Fixture::new();
    assert_eq!(fx.profile.hii_crc32(), "B176E215");
    assert_eq!(fx.document.records.len(), 8);
}

#[test]
fn profile_preserves_nvram_order() {
    let fx = Fixture::new();
    let actual: Vec<&str> = fx.profile.settings[..5].iter().map(|item| item.question.as_str()).collect();
    assert_eq!(
        actual,
        ["Multi-Theme", "CPU Temp Loading Time", "N/A", "BCLK Output Source", "CPU Base Clock 100.00MHz"]
    );
}

#[test]
fn option_change_moves_star() {
    let fx = Fixture::new();
    let setting = fx.find("Extreme Memory Profile(X.M.P.)");
    let change = fx.document.build_change(&fx.profile, &setting.export_id, "05").unwrap();
    let rendered = fx.document.render([&change]).unwrap();
    let block = block(&rendered, "Extreme Memory Profile(X.M.P.)");
    assert!(block.contains("Options\t=[00]Disabled"));
    assert!(block.contains("*[05]User Profile"));
    assert!(!block.contains("Options\t=*[00]Disabled"));
}

#[test]
fn numeric_change() {
    let fx = Fixture::new();
    let setting = fx.find("Performance CPU Clock Ratio 37");
    let change = fx.document.build_change(&fx.profile, &setting.export_id, "0x37").unwrap();
    let rendered = fx.document.render([&change]).unwrap();
    assert!(block(&rendered, "Performance CPU Clock Ratio 37").contains("Value\t=<55>"));
}

#[test]
fn warned_setting_is_blocked() {
    let fx = Fixture::new();
    let setting = fx.find("Multi-Theme");
    let error = fx.document.build_change(&fx.profile, &setting.export_id, "1A").unwrap_err();
    assert!(error.is_validation());
}

#[test]
fn original_cannot_be_overwritten() {
    let fx = Fixture::new();
    let setting = fx.find("Extreme Memory Profile(X.M.P.)");
    let change = fx.document.build_change(&fx.profile, &setting.export_id, "05").unwrap();
    let error = fx.document.write_modified_copy(nvram_fixture(), [&change]).unwrap_err();
    assert!(error.is_validation());
}

#[test]
fn write_outputs() {
    let fx = Fixture::new();
    let setting = fx.find("Extreme Memory Profile(X.M.P.)");
    let change = fx.document.build_change(&fx.profile, &setting.export_id, "05").unwrap();
    let temp = TempDir::new().unwrap();
    let nvram_output = fx.document.write_modified_copy(temp.path().join("modified.txt"), [&change]).unwrap();
    let transaction_output =
        write_transaction(temp.path().join("transaction.json"), &fx.profile, &fx.document, [&change]).unwrap();
    assert!(nvram_output.exists());
    let payload: Value = serde_json::from_str(&fs::read_to_string(transaction_output).unwrap()).unwrap();
    assert_eq!(payload["dry_run_only"], Value::Bool(true));
    assert_eq!(payload["changes"].as_array().unwrap().len(), 1);
}

#[test]
fn the_dupes_file_is_parsed_into_the_profile() {
    let temp = TempDir::new().unwrap();
    let output = temp.path().join("profile.json");
    write_live_profile(&nvram_fixture(), Some(&dupes_fixture()), &output, "NVRAM").unwrap();
    let data: Value = serde_json::from_str(&fs::read_to_string(&output).unwrap()).unwrap();
    let dupes = data["excluded_duplicates"].as_array().unwrap();
    assert_eq!(dupes.len(), 1);
    assert_eq!(dupes[0]["question"], "Memory Try It !");
    assert_eq!(dupes[0]["source"], "dupes");
    assert_eq!(data["metadata"]["hii_crc32"], "B176E215");
    // The keys are written in the order the original tool wrote them.
    let keys: Vec<&String> = data.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        ["schema_version", "label", "source", "metadata", "active_settings", "excluded_duplicates"]
    );
}
