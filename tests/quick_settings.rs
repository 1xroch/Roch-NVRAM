// Quick Settings: presets, matching, and the tab that queues from them.
//
// A preset names settings by token/offset/width and carries a tuned value for
// each. These tests check that a preset file is validated on the way in, that
// resolve() matches it to a loaded export the way Compare and Load NVRAM do,
// that one control writes every identity it carries, and that the tab feeds
// the same queue as the NVRAM table. The shipped preset is loaded too, so a
// malformed edit to it fails here rather than in the window.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use roch_nvram::core::{ParsedProfile, ScewinDocument};
use roch_nvram::quick_settings::{
    changes_for, default_presets, load_preset, load_presets, placeholders, preset_dir, resolve, QuickPreset,
    ResolvedSetting, SCHEMA_VERSION,
};
use roch_nvram::session::{QuickEditor, Session, Tone};
use serde_json::{json, Value};
use tempfile::TempDir;

use common::build;

// Two channel copies of a timing, a ratio, a dropdown, and a value whose kind
// the preset gets wrong.
const RECORDS: &str = "Setup Question\t= tCL                          40
Help String\t= CAS latency, channel A.
Token\t= 1251
Offset\t= 0D49
Width\t= 02
BIOS Default\t= <0>
Value\t= <0>

Setup Question\t= tCL                          40
Help String\t= CAS latency, channel B.
Token\t= 12A1
Offset\t= 0DB3
Width\t= 02
BIOS Default\t= <0>
Value\t= <0>

Setup Question\t= P-Core Ratio
Help String\t= Sets the P-Core ratio.
Token\t= 28F6
Offset\t= 0C66
Width\t= 01
BIOS Default\t= <0>
Value\t= <54>

Setup Question\t= Intel C-State
Help String\t= CPU power management.
Token\t= 01F1
Offset\t= 0014
Width\t= 01
BIOS Default\t= [02]Auto
Options\t=[00]Disabled
\t[01]Enabled
\t*[02]Auto

Setup Question\t= Ring Ratio
Help String\t= Not a dropdown, whatever the preset says.
Token\t= 2936
Offset\t= 0CA8
Width\t= 01
BIOS Default\t= <0>
Value\t= <0>

";

fn preset_dict() -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "name": "Synthetic",
        "platform": "test",
        "source": {"board": "bench"},
        "sections": [
            {
                "title": "CPU",
                "settings": [
                    {"name": "P-Core Ratio", "kind": "value", "tuned": "54", "identities": ["28F6:0C66:01"]},
                    {"name": "Intel C-State", "kind": "options", "tuned": "00", "tuned_label": "Disabled",
                     "identities": ["01F1:0014:01"]},
                    // wrong on purpose: the record is a value
                    {"name": "Ring Ratio", "kind": "options", "tuned": "45", "identities": ["2936:0CA8:01"]},
                    {"name": "Not Here", "kind": "value", "tuned": "1", "identities": ["FFFF:FFFF:01"]},
                ],
            },
            {
                "title": "RAM",
                "settings": [
                    {"name": "tCL", "kind": "value", "tuned": "38", "identities": ["1251:0D49:02", "12A1:0DB3:02"]},
                ],
            },
        ],
    })
}

fn write_preset(root: &Path, file: &str, data: &Value) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    let path = root.join(file);
    fs::write(&path, data.to_string()).unwrap();
    path
}

fn names(preset: &QuickPreset) -> Vec<&str> {
    preset.sections.iter().flat_map(|section| section.settings.iter().map(|s| s.name.as_str())).collect()
}

#[test]
fn a_preset_loads_with_its_sections_in_order() {
    let temp = TempDir::new().unwrap();
    let preset = load_preset(write_preset(temp.path(), "synthetic.json", &preset_dict())).unwrap();
    assert_eq!(preset.name, "Synthetic");
    let titles: Vec<&str> = preset.sections.iter().map(|section| section.title.as_str()).collect();
    assert_eq!(titles, ["CPU", "RAM"]);
    assert_eq!(preset.setting_count(), 5);
    assert_eq!(preset.sections[1].settings[0].identities, ["1251:0D49:02", "12A1:0DB3:02"]);
}

#[test]
fn option_codes_are_upper_cased_on_the_way_in() {
    let mut data = preset_dict();
    data["sections"][0]["settings"][1]["tuned"] = json!("0a");
    data["sections"][0]["settings"][1]["identities"] = json!(["01f1:0014:01"]);
    let temp = TempDir::new().unwrap();
    let setting = load_preset(write_preset(temp.path(), "p.json", &data)).unwrap().sections[0].settings[1].clone();
    assert_eq!(setting.tuned, "0A");
    assert_eq!(setting.identities, ["01F1:0014:01"]);
}

#[test]
fn the_wrong_schema_a_duplicate_identity_and_a_bad_kind_are_refused() {
    let temp = TempDir::new().unwrap();
    let mut data = preset_dict();
    data["schema_version"] = json!(99);
    assert!(load_preset(write_preset(temp.path(), "schema.json", &data)).unwrap_err().is_validation());

    let mut data = preset_dict();
    data["sections"][1]["settings"][0]["identities"].as_array_mut().unwrap().push(json!("28F6:0C66:01"));
    let error = load_preset(write_preset(temp.path(), "dup.json", &data)).unwrap_err();
    assert!(error.message.contains("appears twice"), "{error}");

    let mut data = preset_dict();
    data["sections"][0]["settings"][0]["kind"] = json!("toggle");
    let error = load_preset(write_preset(temp.path(), "kind.json", &data)).unwrap_err();
    assert!(error.message.contains("kind"), "{error}");
}

#[test]
fn a_broken_file_in_the_folder_does_not_take_the_others_down() {
    let temp = TempDir::new().unwrap();
    let mut beta = preset_dict();
    beta["name"] = json!("Beta");
    let mut alpha = preset_dict();
    alpha["name"] = json!("Alpha");
    write_preset(temp.path(), "b.json", &beta);
    write_preset(temp.path(), "a.json", &alpha);
    fs::write(temp.path().join("broken.json"), "{not json").unwrap();
    let names: Vec<String> = load_presets(temp.path()).into_iter().map(|preset| preset.name).collect();
    assert_eq!(names, ["Alpha", "Beta"]);
    assert!(load_presets(temp.path().join("missing")).is_empty());
}

fn check_shipped(preset: &QuickPreset) {
    // Identities are MSI's layout; the name and source must say so rather
    // than claim the socket.
    assert!(preset.name.contains("MSI"));
    assert_eq!(preset.source.get("vendor").map(String::as_str), Some("MSI"));
    let titles: Vec<&str> = preset.sections.iter().map(|section| section.title.as_str()).collect();
    assert_eq!(titles, ["CPU", "RAM", "GPU / PCIe"]);
    let names = names(preset);
    // The value MSI pads onto a name must not survive into the control label.
    assert!(names.contains(&"tCL"));
    assert!(names.contains(&"P-Core Ratio"));
    assert!(!names.contains(&"Fast Boot")); // the Windows one is dropped, the MRC one renamed
    assert!(names.contains(&"MRC Fast Boot"));
    // The standing list: on the page even though stock and tuned agree on them.
    for name in ["Hyper-Threading", "tMOD", "tWR", "tRFC", "Extreme Memory Profile(XMP)"] {
        assert!(names.contains(&name), "{name}");
    }
    assert_eq!(names.iter().filter(|name| **name == "Hyper-Threading").count(), 1, "the global switch, not per core");
    assert!(!names.contains(&"tZQCS"));
    let padded = Regex::new(r"\s{2,}").unwrap();
    for name in &names {
        assert!(!padded.is_match(name), "{name}");
    }
    let tcl = preset.sections.iter().flat_map(|s| &s.settings).find(|s| s.name == "tCL").unwrap();
    assert_eq!(tcl.identities.len(), 2, "both channel copies of tCL are one control");
    assert!(preset.setting_count() >= 60);
}

#[test]
fn the_shipped_msi_z790_preset_is_valid_and_names_its_vendor() {
    let presets = load_presets(preset_dir().expect("assets/ in the source tree"));
    let preset = presets.iter().find(|preset| preset.platform == "msi-z790-ddr5").expect("shipped preset");
    check_shipped(preset);
    // The copy compiled into the executable is the same file.
    let built_in = default_presets();
    assert_eq!(built_in.iter().find(|p| p.platform == "msi-z790-ddr5").unwrap().sections, preset.sections);
}

struct Live {
    temp: TempDir,
    preset: QuickPreset,
    profile: ParsedProfile,
    document: ScewinDocument,
}

impl Live {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let preset = load_preset(write_preset(temp.path(), "synthetic.json", &preset_dict())).unwrap();
        let (profile, document) = build(&temp.path().join("live"), RECORDS);
        Self { temp, preset, profile, document }
    }

    fn rows(&self) -> Vec<ResolvedSetting> {
        resolve(&self.preset, &self.profile, &self.document).into_iter().flat_map(|(_, rows)| rows).collect()
    }

    fn row(&self, name: &str) -> ResolvedSetting {
        self.rows().into_iter().find(|row| row.spec.name == name).unwrap()
    }
}

#[test]
fn a_control_matches_every_identity_it_carries() {
    let live = Live::new();
    let tcl = live.row("tCL");
    assert_eq!(tcl.targets.len(), 2);
    assert!(tcl.missing.is_empty());
    assert!(tcl.available());
    assert_eq!(tcl.current_display(), "0");
    assert!(!tcl.matches_tuned());
}

#[test]
fn matches_tuned_reads_the_live_value() {
    let live = Live::new();
    assert!(live.row("P-Core Ratio").matches_tuned());
    assert!(!live.row("Intel C-State").matches_tuned());
    assert_eq!(live.row("Intel C-State").current_display(), "Auto");
}

#[test]
fn an_identity_the_board_lacks_and_a_kind_mismatch_are_reported_not_guessed() {
    let live = Live::new();
    let absent = live.row("Not Here");
    assert!(!absent.available());
    assert_eq!(absent.missing, ["FFFF:FFFF:01"]);
    assert_eq!(absent.current_display(), "not on this board");
    let ring = live.row("Ring Ratio");
    assert!(!ring.available());
    assert_eq!(ring.blocked.len(), 1);
    assert!(ring.blocked[0].contains("preset expects options"));
}

#[test]
fn placeholders_carry_every_control_with_nothing_matched() {
    let live = Live::new();
    let laid_out = placeholders(&live.preset);
    let titles: Vec<&str> = laid_out.iter().map(|(section, _)| section.title.as_str()).collect();
    assert_eq!(titles, ["CPU", "RAM"]);
    let tcl = &laid_out[1].1[0];
    assert!(tcl.targets.is_empty());
    assert_eq!(tcl.missing, tcl.spec.identities);
    assert!(!tcl.available());
}

#[test]
fn changes_for_writes_one_change_per_target() {
    let live = Live::new();
    let (changes, reset, errors) = changes_for(&live.row("tCL"), "38", &live.profile, &live.document);
    assert!(errors.is_empty());
    assert!(reset.is_empty());
    let shown: Vec<(&str, &str)> = changes.iter().map(|c| (c.token_hex.as_str(), c.new_display.as_str())).collect();
    assert_eq!(shown, [("1251", "38"), ("12A1", "38")]);
}

#[test]
fn changes_for_resets_a_target_already_at_the_value() {
    let live = Live::new();
    let (changes, reset, errors) = changes_for(&live.row("P-Core Ratio"), "54", &live.profile, &live.document);
    assert!(changes.is_empty());
    assert_eq!(reset.len(), 1);
    assert!(errors.is_empty());
}

#[test]
fn changes_for_refuses_an_option_code_the_setting_does_not_offer() {
    let live = Live::new();
    let cstate = live.row("Intel C-State");
    let (changes, _, errors) = changes_for(&cstate, "7F", &live.profile, &live.document);
    assert!(changes.is_empty());
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("Intel C-State"));
    let (changes, _, errors) = changes_for(&cstate, "00", &live.profile, &live.document);
    assert!(errors.is_empty());
    assert_eq!(changes[0].new_display, "Disabled");
}

// ----- the tab, through the window's state ---------------------------------

struct Tab {
    live: Live,
    session: Session,
}

impl Tab {
    fn new() -> Self {
        Self::with_preset(None)
    }

    fn with_preset(preset: Option<QuickPreset>) -> Self {
        let live = Live::new();
        let preset = preset.unwrap_or_else(|| live.preset.clone());
        let session = Session::new(false, Some(live.temp.path().join("appdata")), vec![preset]);
        Self { live, session }
    }

    fn load(&mut self) {
        self.session.show_nvram(self.live.profile.clone(), self.live.document.clone(), "test");
    }

    fn row(&self, name: &str) -> usize {
        self.session.quick_row_by_name(name).unwrap()
    }

    fn set_text(&mut self, name: &str, text: &str) {
        let index = self.row(name);
        match &mut self.session.quick_rows[index].editor {
            QuickEditor::Value { text: current } => *current = text.into(),
            QuickEditor::Options { .. } => panic!("{name} is a dropdown"),
        }
    }

    fn select(&mut self, name: &str, code: &str) {
        let index = self.row(name);
        assert!(self.session.quick_rows[index].editor.select_code(code));
    }

    fn queue(&mut self, name: &str) -> Vec<String> {
        let index = self.row(name);
        self.session.queue_quick_row(index)
    }

    fn state(&self, name: &str) -> String {
        self.session.quick_row_view(self.row(name)).state
    }

    fn displays(&self) -> Vec<String> {
        self.session.changes.values().map(|change| change.new_display.clone()).collect()
    }
}

#[test]
fn nothing_can_be_queued_before_an_nvram_is_loaded() {
    let tab = Tab::new();
    assert_eq!(tab.session.quick_rows.len(), 5);
    for index in 0..5 {
        let view = tab.session.quick_row_view(index);
        assert!(!view.enabled);
        assert_eq!(view.current, "—");
    }
    assert!(tab.session.quick_notice.0.contains("No NVRAM loaded"));
}

#[test]
fn loading_an_export_fills_current_values_and_states() {
    let mut tab = Tab::new();
    tab.load();
    assert_eq!(tab.session.quick_row_view(tab.row("tCL")).current, "0");
    assert_eq!(tab.state("P-Core Ratio"), "");
    assert_eq!(tab.state("Not Here"), "not on this board");
    assert!(!tab.session.quick_row_view(tab.row("Not Here")).enabled);
    assert_eq!(tab.state("Ring Ratio"), "blocked");
    assert_eq!(tab.session.quick_row_view(tab.row("Ring Ratio")).tone, Tone::Warning);
    assert!(tab.session.quick_row_view(tab.row("tCL")).enabled);
}

#[test]
fn the_editor_starts_on_the_current_value() {
    let mut tab = Tab::new();
    tab.load();
    assert_eq!(tab.session.quick_rows[tab.row("tCL")].editor.requested_raw(), "0");
    assert_eq!(tab.session.quick_rows[tab.row("P-Core Ratio")].editor.requested_raw(), "54");
    let QuickEditor::Options { items, .. } = &tab.session.quick_rows[tab.row("Intel C-State")].editor else {
        panic!("a dropdown");
    };
    assert_eq!(items.len(), 3);
    assert_eq!(tab.session.quick_rows[tab.row("Intel C-State")].editor.requested_raw(), "02"); // Auto, the live value
    // Queue with nothing changed is a no-op rather than a surprise write.
    tab.queue("tCL");
    assert!(tab.session.changes.is_empty());
}

#[test]
fn queueing_one_row_writes_every_identity_and_shows_in_pending_changes() {
    let mut tab = Tab::new();
    tab.load();
    tab.set_text("tCL", "38");
    assert!(tab.queue("tCL").is_empty());
    let mut tokens: Vec<&str> = tab.session.changes.values().map(|c| c.token_hex.as_str()).collect();
    tokens.sort();
    assert_eq!(tokens, ["1251", "12A1"]);
    assert_eq!(tab.state("tCL"), "queued → 38");
}

#[test]
fn an_edited_value_is_what_gets_queued_and_the_current_value_resets_it() {
    let mut tab = Tab::new();
    tab.load();
    tab.set_text("P-Core Ratio", "55");
    tab.queue("P-Core Ratio");
    assert_eq!(tab.displays(), ["55"]);
    tab.set_text("P-Core Ratio", "54");
    tab.queue("P-Core Ratio");
    assert!(tab.session.changes.is_empty());
    assert_eq!(tab.state("P-Core Ratio"), "");
}

#[test]
fn a_dropdown_row_queues_the_selected_option_and_a_current_pick_resets_it() {
    let mut tab = Tab::new();
    tab.load();
    tab.select("Intel C-State", "00");
    tab.queue("Intel C-State");
    assert_eq!(tab.displays(), ["Disabled"]);
    assert_eq!(tab.state("Intel C-State"), "queued → Disabled");
    tab.select("Intel C-State", "01");
    tab.queue("Intel C-State");
    assert_eq!(tab.displays(), ["Enabled"]);
    tab.select("Intel C-State", "02"); // the live value, Auto
    tab.queue("Intel C-State");
    assert!(tab.session.changes.is_empty());
    assert_eq!(tab.state("Intel C-State"), "");
}

#[test]
fn a_bad_value_reports_an_error_and_queues_nothing() {
    let mut tab = Tab::new();
    tab.load();
    tab.set_text("P-Core Ratio", "300");
    let errors = tab.queue("P-Core Ratio");
    assert_eq!(errors.len(), 1);
    assert!(errors[0].starts_with("P-Core Ratio (28F6:0C66:01): Value exceeds"), "{}", errors[0]);
    assert!(tab.session.changes.is_empty());
}

#[test]
fn the_notice_says_how_many_controls_apply() {
    let mut tab = Tab::new();
    tab.load();
    // tCL, P-Core Ratio, C-State apply; Ring Ratio is blocked; Not Here is absent.
    assert!(tab.session.quick_notice.0.contains("3 of 5 controls apply"));
}

#[test]
fn another_vendors_export_is_reported_as_a_mismatch_not_a_page_of_dead_rows() {
    let mut data = preset_dict();
    data["name"] = json!("Other Vendor Z790");
    data["source"] = json!({"vendor": "ASUS", "board": "some ASUS board"});
    // One identity per control, unique across sections and absent from RECORDS.
    let mut number = 0;
    for section in data["sections"].as_array_mut().unwrap() {
        for setting in section["settings"].as_array_mut().unwrap() {
            setting["identities"] = json!([format!("EE{number:02X}:00{number:02X}:01")]);
            number += 1;
        }
    }
    let temp = TempDir::new().unwrap();
    let foreign = load_preset(write_preset(temp.path(), "foreign.json", &data)).unwrap();
    let mut tab = Tab::with_preset(Some(foreign));
    tab.load();
    let (notice, tone) = &tab.session.quick_notice;
    assert!(notice.contains("does not match the loaded NVRAM"));
    assert!(notice.contains("0 of 5"));
    assert!(notice.contains("some ASUS board"));
    assert!(notice.contains("ASRock"));
    assert_eq!(*tone, Tone::Warning);
    assert!((0..5).all(|index| !tab.session.quick_row_view(index).enabled));
}

#[test]
fn showing_another_export_rebuilds_the_rows_against_it() {
    let mut tab = Tab::new();
    tab.load();
    tab.set_text("tCL", "38");
    tab.queue("tCL");
    tab.load(); // show_nvram clears the queue and rebuilds
    assert!(tab.session.changes.is_empty());
    assert_eq!(tab.state("tCL"), "");
    assert_eq!(tab.session.quick_rows[tab.row("tCL")].editor.requested_raw(), "0");
}
