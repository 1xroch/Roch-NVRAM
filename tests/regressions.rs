// Bugs that were fixed once and must stay fixed.

mod common;

use std::fs;

use roch_nvram::compare::{compare_files, has_identity};
use roch_nvram::core::{raw_value, ParsedProfile, PendingChange, ScewinDocument};
use roch_nvram::parser::{option_line_indexes, parse_sce_file, Kind, ProfileDocument, ProfileSource};
use roch_nvram::pycompat::{to_json, JsonStyle};
use tempfile::TempDir;

use common::*;

// An option row at column 0. The reader always accepted these; the writer used
// to stop at the first unindented line and leave its "*" in place.
const UNINDENTED_OPTIONS: &str = "Setup Question\t= Fan Mode
Help String\t= Fan control mode
Token\t= 0100
Offset\t= 0010
Width\t= 01
BIOS Default\t= [00]Low
Options\t=[00]Low
\t[01]Mid
*[02]High

";

// Two option codes that share a label. Comparing displayed labels cannot tell
// these apart; comparing raw values can.
const DUPLICATE_LABELS: &str = "Setup Question\t= Fan Curve
Help String\t= Fan curve selection
Token\t= 0101
Offset\t= 0011
Width\t= 01
BIOS Default\t= [00]Auto
Options\t=*[00]Auto
\t[01]Auto
\t[02]Manual

";

// Width 0x10, so normalize_value passes the text through instead of parsing a number.
const WIDE_VALUE: &str = "Setup Question\t= Board Label
Help String\t= Free text field
Token\t= 0102
Offset\t= 0012
Width\t= 10
BIOS Default\t= <hello>
Value\t= <hello>

";

// Records that carry no token, offset or width at all.
const WITHOUT_IDENTITY: &str = "Setup Question\t= Real
Help String\t= has an identity
Token\t= 0100
Offset\t= 0010
Width\t= 01
BIOS Default\t= <1>
Value\t= <1>

Setup Question\t= Orphan A
Help String\t= no token, offset or width
Value\t= <5>

Setup Question\t= Orphan B
Help String\t= no token, offset or width
Value\t= <5>

";

const EURO_SIGN: char = '\u{20AC}'; // valid Unicode, but not representable in Latin-1

#[test]
fn reader_and_writer_agree_on_which_rows_are_options() {
    let block: Vec<&str> = UNINDENTED_OPTIONS.lines().collect();
    let rows: Vec<&str> = option_line_indexes(&block).into_iter().map(|index| block[index]).collect();
    assert_eq!(rows, ["Options\t=[00]Low", "\t[01]Mid", "*[02]High"]);
}

#[test]
fn option_block_ends_at_a_blank_line_and_at_the_next_field() {
    let block = ["Options\t=[00]Low", "\t[01]Mid", "", "\t[02]High", "Token\t= 01"];
    assert_eq!(option_line_indexes(&block), [0, 1]);
}

#[test]
fn writing_a_change_leaves_exactly_one_selected_option() {
    let temp = TempDir::new().unwrap();
    let (profile, document) = build(temp.path(), UNINDENTED_OPTIONS);
    let setting = &profile.settings[0];
    let codes: Vec<&str> = setting.options.iter().map(|option| option.code_hex.as_str()).collect();
    assert_eq!(codes, ["00", "01", "02"]);

    let change = document.build_change(&profile, &setting.export_id, "01").unwrap();
    let output = document.write_modified_copy(temp.path().join("apply.txt"), [&change]).unwrap();
    let written = String::from_utf8(fs::read(output).unwrap()).unwrap();
    let selected: Vec<&str> = written.lines().filter(|line| line.contains("*[")).collect();
    assert_eq!(selected, ["\t*[01]Mid"]);
}

#[test]
fn duplicate_option_labels_are_distinguished_by_raw_value() {
    let temp = TempDir::new().unwrap();
    let (profile, document) = build(temp.path(), DUPLICATE_LABELS);
    let setting = &profile.settings[0];
    let change = document.build_change(&profile, &setting.export_id, "01").unwrap();
    // Both codes read "Auto", so the labels match even though this is a real
    // change. The editor must compare raw values, not the display.
    assert_eq!(change.old_display, change.new_display);
    assert_ne!(change.new_raw, raw_value(setting));
    assert_eq!(raw_value(setting), "00");
    assert_eq!(change.new_raw, "01");
}

#[test]
fn selecting_the_current_option_is_still_recognised_as_a_no_op() {
    let temp = TempDir::new().unwrap();
    let (profile, document) = build(temp.path(), DUPLICATE_LABELS);
    let setting = &profile.settings[0];
    let change = document.build_change(&profile, &setting.export_id, "00").unwrap();
    assert_eq!(change.new_raw, raw_value(setting));
}

#[test]
fn queueing_a_non_latin1_value_is_refused() {
    let temp = TempDir::new().unwrap();
    let (profile, document) = build(temp.path(), WIDE_VALUE);
    let setting = &profile.settings[0];
    let error = document.build_change(&profile, &setting.export_id, &format!("caf{EURO_SIGN}")).unwrap_err();
    assert!(error.is_validation());
}

#[test]
fn writer_reports_text_it_cannot_encode() {
    let temp = TempDir::new().unwrap();
    let (profile, document) = build(temp.path(), WIDE_VALUE);
    let setting = &profile.settings[0];
    let smuggled = PendingChange {
        export_id: setting.export_id.clone(),
        question: setting.question.clone(),
        kind: Kind::Value,
        old_display: "hello".into(),
        new_display: format!("caf{EURO_SIGN}"),
        new_raw: format!("caf{EURO_SIGN}"),
        token_hex: setting.token_hex.clone(),
        offset_hex: setting.offset_hex.clone(),
        width_hex: setting.width_hex.clone(),
        help: String::new(),
    };
    let error = document.write_modified_copy(temp.path().join("apply.txt"), [&smuggled]).unwrap_err();
    assert!(error.is_validation());
    assert!(!temp.path().join("apply.txt").exists());
}

#[test]
fn numbers_are_range_checked_against_the_width() {
    let temp = TempDir::new().unwrap();
    let records = WIDE_VALUE.replace("Width\t= 10", "Width\t= 01");
    let (profile, document) = build(temp.path(), &records);
    let id = &profile.settings[0].export_id;
    assert_eq!(document.build_change(&profile, id, "255").unwrap().new_raw, "255");
    assert_eq!(document.build_change(&profile, id, "0xff").unwrap().new_raw, "255");
    for bad in ["256", "-1", "abc", "010", "", "<1>"] {
        assert!(document.build_change(&profile, id, bad).unwrap_err().is_validation(), "{bad}");
    }
}

#[test]
fn missing_profile_reports_a_validation_error() {
    let temp = TempDir::new().unwrap();
    assert!(ParsedProfile::load(temp.path().join("absent.json")).unwrap_err().is_validation());
}

#[test]
fn malformed_profile_reports_a_validation_error() {
    let temp = TempDir::new().unwrap();
    let broken = temp.path().join("profile.json");
    fs::write(&broken, "{not json").unwrap();
    assert!(ParsedProfile::load(&broken).unwrap_err().is_validation());
}

#[test]
fn profile_that_is_not_an_object_reports_a_validation_error() {
    let temp = TempDir::new().unwrap();
    let listed = temp.path().join("profile.json");
    fs::write(&listed, "[1, 2, 3]").unwrap();
    assert!(ParsedProfile::load(&listed).unwrap_err().is_validation());
}

#[test]
fn missing_export_reports_a_validation_error() {
    let temp = TempDir::new().unwrap();
    assert!(ScewinDocument::open(temp.path().join("absent.txt")).unwrap_err().is_validation());
}

// The written file must differ from the source only inside edited records.
//
// The document used to normalise line endings on the way in and re-expand
// every newline to CRLF on the way out, so a real AMISCE export -- which
// carries stray "\r\r\n" sequences in its header -- came back two bytes larger
// than it went in.
fn template(n: &str) -> String {
    [
        "// AMISCE Utility. Ver 5.05.01.0002",
        "HIICrc32= ABCD1234",
        "",
        "Setup Question\t= Fan Mode",
        "Help String\t= Fan control",
        "Token\t= 0100",
        "Offset\t= 0010",
        "Width\t= 01",
        "BIOS Default\t= [00]Low",
        "Options\t=*[00]Low",
        "\t[01]Mid",
    ]
    .iter()
    .map(|line| format!("{line}{n}"))
    .collect()
}

#[test]
fn a_no_op_render_reproduces_the_source_bytes() {
    for newline in ["\r\n", "\n", "\r\r\n"] {
        let temp = TempDir::new().unwrap();
        let nvram = temp.path().join("nvram.txt");
        fs::write(&nvram, template(newline)).unwrap();
        let output = ScewinDocument::open(&nvram)
            .unwrap()
            .write_modified_copy(temp.path().join("out.txt"), [])
            .unwrap();
        assert_eq!(fs::read(output).unwrap(), fs::read(&nvram).unwrap(), "{newline:?}");
    }
}

#[test]
fn only_the_edited_record_differs() {
    // How a real AMISCE export is actually shaped: the doubled CRs appear on
    // two header lines only, and the records themselves are plain CRLF.
    let realistic = template("\r\n").replacen(
        "// AMISCE Utility. Ver 5.05.01.0002\r\nHIICrc32= ABCD1234\r\n",
        "// AMISCE Utility. Ver 5.05.01.0002\r\r\nHIICrc32= ABCD1234\r\r\n",
        1,
    );
    let temp = TempDir::new().unwrap();
    let nvram = temp.path().join("nvram.txt");
    fs::write(&nvram, &realistic).unwrap();
    let parsed = parse_sce_file(&nvram, "nvram", false).unwrap();
    let profile_path = temp.path().join("profile.json");
    let source = ProfileSource::default();
    let document_json = ProfileDocument {
        schema_version: 1,
        label: "Synthetic",
        source: &source,
        metadata: &parsed.metadata,
        active_settings: &parsed.settings,
        excluded_duplicates: &[],
    };
    fs::write(&profile_path, to_json(&document_json, JsonStyle::Line, true)).unwrap();
    let profile = ParsedProfile::load(&profile_path).unwrap();
    let document = ScewinDocument::open(&nvram).unwrap();
    document.verify_profile(&profile).unwrap();

    let setting = &profile.settings[0];
    let change = document.build_change(&profile, &setting.export_id, "01").unwrap();
    let output = document.write_modified_copy(temp.path().join("out.txt"), [&change]).unwrap();

    let before = fs::read(&nvram).unwrap();
    let after = fs::read(output).unwrap();
    let span = &document.records[&setting.export_id];
    let differing: Vec<usize> = (0..before.len().min(after.len())).filter(|&i| before[i] != after[i]).collect();
    assert!(!differing.is_empty(), "the change must actually be written");
    for offset in differing {
        assert!(
            span.start <= offset && offset < span.end,
            "byte {offset} changed outside the edited record {}..{}",
            span.start,
            span.end
        );
    }
    // The header's doubled CRs are part of what must survive untouched.
    assert!(after.windows(3).any(|window| window == b"\r\r\n"));
}

#[test]
fn latin1_text_round_trips_byte_for_byte() {
    // Bytes above 0x7F are single Latin-1 characters, never UTF-8.
    let temp = TempDir::new().unwrap();
    let records = DUPLICATE_LABELS.replace("Fan curve selection", "Ventilateur \u{e9}t\u{e9} \u{b0}C");
    let (profile, document) = build(temp.path(), &records);
    let change = document.build_change(&profile, &profile.settings[0].export_id, "02").unwrap();
    let output = document.write_modified_copy(temp.path().join("out.txt"), [&change]).unwrap();
    let written = fs::read(output).unwrap();
    assert!(written.windows(3).any(|window| window == b"\xE9t\xE9"));
    assert!(written.windows(9).any(|window| window == b"\t*[02]Man"));
}

#[test]
fn records_without_an_identity_are_not_compared() {
    let temp = TempDir::new().unwrap();
    let first = temp.path().join("first.txt");
    let second = temp.path().join("second.txt");
    write_latin1(&first, &format!("{HEADER}{WITHOUT_IDENTITY}"));
    write_latin1(&second, &format!("{HEADER}{WITHOUT_IDENTITY}"));
    let result = compare_files(&first, &second).unwrap();
    assert_eq!(result.changed_count(), 0);
    let names: Vec<&str> = result.rows.iter().map(|row| row.setting.as_str()).collect();
    assert_eq!(names, ["Real"]);
}

#[test]
fn has_identity_rejects_empty_and_placeholder_keys() {
    assert!(!has_identity(""));
    assert!(!has_identity("::"));
    assert!(has_identity("0100:0010:01"));
    assert!(has_identity("0100::"));
}

#[test]
fn a_record_with_a_bare_carriage_return_is_not_rewritten() {
    // The parser reads a bare \r as a line break and the writer does not, so
    // the writer could miss an option row and leave two selected.
    let temp = tempfile::TempDir::new().unwrap();
    let records = "Setup Question\t= Fan Curve
Help String\t= Fan curve selection
Token\t= 0101
Offset\t= 0011
Width\t= 01
BIOS Default\t= [00]Auto
Options\t=*[00]Auto
\t[01]Balanced\r\t[02]Manual

";
    let (profile, document) = common::build(temp.path(), records);
    let id = profile.settings[0].export_id.clone();
    let change = document.build_change(&profile, &id, "01").unwrap();
    let error = document.render([&change]).unwrap_err();
    assert!(error.to_string().contains("unusual line break"), "{error}");
}

#[test]
fn control_characters_are_refused_in_values() {
    for value in ["1\u{0b}2", "a\u{0c}b", "x\u{85}y", "tab\there"] {
        assert!(roch_nvram::core::normalize_value(value, Some(16)).is_err(), "{value:?}");
    }
}
