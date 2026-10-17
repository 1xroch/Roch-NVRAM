// Compare, its CSV, and the activity log.

mod common;

use std::fs;

use regex::Regex;
use roch_nvram::catalog::ActivityLogger;
use roch_nvram::compare::{compare_files, export_compare_csv};
use tempfile::TempDir;

use common::*;

fn make_nvram(option_code: &str, value: &str, hii: &str) -> String {
    let star = |code: &str| if option_code == code { "*" } else { "" };
    format!(
        "{}Setup Question\t= Test Option
Help String\t= Test option help
Token\t= 10
Offset\t= 20
Width\t= 01
BIOS Default\t= [00]Disabled
Options\t={}[00]Disabled
         {}[01]Enabled

Setup Question\t= Test Value
Help String\t= Test value help
Token\t= 11
Offset\t= 21
Width\t= 02
BIOS Default\t= <100>
Value\t= <{value}>

",
        HEADER.replace("ABCD1234", hii),
        star("00"),
        star("01"),
    )
}

fn stock() -> String {
    make_nvram("00", "100", "ABCD1234")
}

#[test]
fn compare_detects_changed_values_and_preserves_order() {
    let temp = TempDir::new().unwrap();
    let (a, b) = (temp.path().join("stock.txt"), temp.path().join("overclocked.txt"));
    write_latin1(&a, &stock());
    write_latin1(&b, &make_nvram("01", "120", "ABCD1234"));
    let result = compare_files(&a, &b).unwrap();
    assert!(result.hii_match);
    assert_eq!(result.changed_count(), 2);
    let names: Vec<&str> = result.rows.iter().map(|row| row.setting.as_str()).collect();
    assert_eq!(names, ["Test Option", "Test Value"]);
    assert_eq!(result.rows[0].stock_value, "Disabled [00]");
    assert_eq!(result.rows[0].overclocked_value, "Enabled [01]");
    assert_eq!(result.rows[1].stock_value, "<100>");
    assert_eq!(result.rows[1].overclocked_value, "<120>");
}

#[test]
fn name_only_difference_is_not_a_change() {
    let temp = TempDir::new().unwrap();
    let (a, b) = (temp.path().join("stock.txt"), temp.path().join("overclocked.txt"));
    // Same token/offset/width and same value, but the Setup Question label
    // differs between the two BIOS builds. This must not count as changed.
    write_latin1(&a, &stock());
    write_latin1(&b, &stock().replace("Test Value", "Test Value 1.341V"));
    let result = compare_files(&a, &b).unwrap();
    let renamed = result.rows.iter().find(|row| row.setting.contains("1.341V")).unwrap();
    assert!(!renamed.changed);
    assert_eq!(renamed.status, "Same");
    assert_eq!(renamed.setting, "Test Value → Test Value 1.341V");
    assert_eq!(result.changed_count(), 0);
}

#[test]
fn compare_reports_hii_mismatch() {
    let temp = TempDir::new().unwrap();
    let (a, b) = (temp.path().join("stock.txt"), temp.path().join("overclocked.txt"));
    write_latin1(&a, &stock());
    write_latin1(&b, &make_nvram("00", "100", "FFFF0000"));
    let result = compare_files(&a, &b).unwrap();
    assert!(!result.hii_match);
    assert_eq!(result.changed_count(), 0);
}

#[test]
fn presence_and_type_changes_count() {
    let temp = TempDir::new().unwrap();
    let (a, b) = (temp.path().join("stock.txt"), temp.path().join("overclocked.txt"));
    write_latin1(&a, &stock());
    let only_option = stock().split("Setup Question\t= Test Value").next().unwrap().to_string();
    write_latin1(&b, &only_option);
    let result = compare_files(&a, &b).unwrap();
    assert_eq!(result.rows[1].status, "Only in NVRAM 1");
    assert_eq!(result.rows[1].overclocked_value, "—");
    let reversed = compare_files(&b, &a).unwrap();
    assert_eq!(reversed.rows[1].status, "Only in NVRAM 2");
}

#[test]
fn csv_exports_changed_rows() {
    let temp = TempDir::new().unwrap();
    let (a, b) = (temp.path().join("stock.txt"), temp.path().join("overclocked.txt"));
    write_latin1(&a, &stock());
    write_latin1(&b, &make_nvram("00", "101", "ABCD1234"));
    let result = compare_files(&a, &b).unwrap();
    let output = export_compare_csv(temp.path().join("changes.csv"), &result, true).unwrap();
    let bytes = fs::read(output).unwrap();
    assert!(bytes.starts_with(b"\xEF\xBB\xBF"), "Excel needs the BOM");
    let text = String::from_utf8(bytes[3..].to_vec()).unwrap();
    assert!(text.contains("NVRAM 1 Value,NVRAM 2 Value"));
    assert!(text.contains("Test Value"));
    assert!(!text.contains("Test Option,"));
    assert!(text.ends_with("\r\n"));
}

#[test]
fn log_contains_local_date_time_event_and_message() {
    let temp = TempDir::new().unwrap();
    let logger = ActivityLogger::new(Some(temp.path().to_path_buf()));
    let line = logger.write("nvram_open", "Opened\nstock.txt");
    assert!(Regex::new(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} \| NVRAM_OPEN \|").unwrap().is_match(&line));
    assert!(logger.read_text().contains("Opened stock.txt"));
    assert_eq!(logger.log_path.file_name().unwrap(), "nvram.log");
    assert!(logger.log_path.is_file());
}
