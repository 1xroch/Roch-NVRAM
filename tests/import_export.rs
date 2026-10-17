// The on-demand Export / Import round trip, and Load NVRAM.
//
// The window does not read the firmware when it opens. Export reads the live
// NVRAM on demand, Import writes the reviewed queue back, and Load NVRAM fills
// that queue from a saved export through differences_from.

mod common;

use std::path::Path;

use roch_nvram::compare::load_snapshot;
use roch_nvram::session::{differences_from, SavedNvramDiff, Session};
use tempfile::TempDir;

use common::*;

const RECORDS: &str = "Setup Question\t= Fan Curve
Help String\t= Fan curve selection
Token\t= 0101
Offset\t= 0011
Width\t= 01
BIOS Default\t= [00]Auto
Options\t=*[00]Auto
\t[01]Balanced
\t[02]Manual

Setup Question\t= Board Label
Help String\t= Free text field
Token\t= 0102
Offset\t= 0012
Width\t= 10
BIOS Default\t= <hello>
Value\t= <hello>

";

fn differences(root: &Path, saved_records: &str) -> SavedNvramDiff {
    let (profile, document) = build(&root.join("live"), RECORDS);
    let saved = root.join("saved");
    std::fs::create_dir_all(&saved).unwrap();
    let saved = write_nvram(&saved, saved_records);
    differences_from(&profile, &document, &load_snapshot(saved).unwrap())
}

fn manual() -> String {
    RECORDS.replace("Options\t=*[00]Auto", "Options\t=[00]Auto").replace("\t[02]Manual", "\t*[02]Manual")
}

#[test]
fn a_changed_option_is_queued_and_matches_are_counted() {
    let temp = TempDir::new().unwrap();
    let diff = differences(temp.path(), &manual());
    assert_eq!(diff.queued.len(), 1);
    assert_eq!(diff.queued[0].question, "Fan Curve");
    assert_eq!(diff.queued[0].new_raw, "02");
    assert_eq!(diff.queued[0].new_display, "Manual");
    assert_eq!(diff.already, 1);
    assert_eq!(diff.missing, 0);
    assert!(diff.blocked.is_empty());
}

#[test]
fn a_changed_value_is_queued() {
    let temp = TempDir::new().unwrap();
    let diff = differences(temp.path(), &RECORDS.replace("Value\t= <hello>", "Value\t= <world>"));
    assert_eq!(diff.queued.len(), 1);
    assert_eq!(diff.queued[0].question, "Board Label");
    assert_eq!(diff.queued[0].new_raw, "world");
    assert_eq!(diff.already, 1);
}

#[test]
fn an_identical_file_queues_nothing() {
    let temp = TempDir::new().unwrap();
    let diff = differences(temp.path(), RECORDS);
    assert!(diff.queued.is_empty());
    assert_eq!(diff.already, 2);
    assert_eq!(diff.missing, 0);
    assert!(diff.blocked.is_empty());
}

#[test]
fn settings_absent_from_the_live_export_are_counted_not_queued() {
    let temp = TempDir::new().unwrap();
    let extra = format!(
        "{RECORDS}Setup Question\t= Not On This Board
Help String\t= only in the saved file
Token\t= 0999
Offset\t= 0999
Width\t= 01
BIOS Default\t= <1>
Value\t= <2>

"
    );
    let diff = differences(temp.path(), &extra);
    assert!(diff.queued.is_empty());
    assert_eq!(diff.missing, 1);
    assert_eq!(diff.already, 2);
}

#[test]
fn a_type_change_is_blocked_rather_than_queued() {
    let temp = TempDir::new().unwrap();
    // Same token/offset/width as the live Fan Curve, but a value record.
    let retyped = RECORDS.replace(
        "BIOS Default\t= [00]Auto\nOptions\t=*[00]Auto\n\t[01]Balanced\n\t[02]Manual",
        "BIOS Default\t= <1>\nValue\t= <2>",
    );
    let diff = differences(temp.path(), &retyped);
    assert!(diff.queued.is_empty());
    assert_eq!(diff.blocked.len(), 1);
    assert!(diff.blocked[0].contains("type differs"));
}

#[test]
fn duplicate_saved_identity_is_blocked_rather_than_queued() {
    let temp = TempDir::new().unwrap();
    let changed = manual();
    let first_record = changed.split("Setup Question\t= Board Label").next().unwrap();
    let duplicate = format!("{changed}{first_record}");
    let diff = differences(temp.path(), &duplicate);
    assert!(diff.queued.is_empty());
    assert_eq!(diff.already, 1);
    assert_eq!(diff.blocked.len(), 1);
    assert!(diff.blocked[0].contains("ambiguous"));
}

// ----- the NVRAM tab's buttons ---------------------------------------------

fn session(root: &Path, live: bool) -> Session {
    Session::new(live, Some(root.join("appdata")), Vec::new())
}

#[test]
fn the_nvram_tab_row_leads_with_export_and_import() {
    let temp = TempDir::new().unwrap();
    let labels: Vec<&str> = session(temp.path(), true).nvram_buttons().into_iter().map(|(label, _)| label).collect();
    assert_eq!(labels, ["Export", "Import", "Queue selected change", "Load NVRAM..."]);
}

#[test]
fn only_export_works_before_an_nvram_is_loaded() {
    let temp = TempDir::new().unwrap();
    for (label, enabled) in session(temp.path(), true).nvram_buttons() {
        assert_eq!(enabled, label == "Export", "{label}");
    }
}

#[test]
fn every_button_works_once_an_nvram_is_loaded() {
    let temp = TempDir::new().unwrap();
    let mut session = session(temp.path(), true);
    let (profile, document) = build(&temp.path().join("live"), RECORDS);
    session.show_nvram(profile, document, "test");
    assert!(session.nvram_buttons().into_iter().all(|(_, enabled)| enabled));
    assert!(session.status_text(None).contains("Queued changes: 0"));
    // Opening an export catalogues it.
    assert_eq!(session.catalog.entries().len(), 1);
    assert_eq!(session.catalog.entries()[0].source, "test");
}

#[test]
fn offline_mode_offers_neither_export_nor_import() {
    let temp = TempDir::new().unwrap();
    let labels: Vec<&str> = session(temp.path(), false).nvram_buttons().into_iter().map(|(label, _)| label).collect();
    assert!(!labels.contains(&"Export"));
    assert!(!labels.contains(&"Import"));
}

#[test]
fn an_edit_back_to_the_live_value_drops_the_queued_change() {
    let temp = TempDir::new().unwrap();
    let mut session = session(temp.path(), true);
    let (profile, document) = build(&temp.path().join("live"), RECORDS);
    let id = profile.settings[0].export_id.clone();
    let to_manual = document.build_change(&profile, &id, "02").unwrap();
    let to_auto = document.build_change(&profile, &id, "00").unwrap();
    session.show_nvram(profile, document, "test");
    session.queue_edit(to_manual);
    assert_eq!(session.changes.len(), 1);
    session.queue_edit(to_auto);
    assert!(session.changes.is_empty());
    let log = session.logger.read_text();
    assert!(log.contains("| CHANGE_QUEUED | Fan Curve: Auto -> Manual"));
    assert!(log.contains("| CHANGE_REMOVED | Fan Curve: queued change was reset to the current value."));
}
