// Profile generation, the import preflight, and verification.

mod common;

use roch_nvram::backend::{remap_changes, verify_changes, write_live_profile};
use roch_nvram::core::{ParsedProfile, ScewinDocument};
use roch_nvram::ErrorKind;
use tempfile::TempDir;

use common::*;

struct Fixture {
    _temp: TempDir,
    profile_path: std::path::PathBuf,
    profile: ParsedProfile,
    document: ScewinDocument,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let profile_path = temp.path().join("profile.json");
        write_live_profile(&nvram_fixture(), Some(&dupes_fixture()), &profile_path, "NVRAM").unwrap();
        let profile = ParsedProfile::load(&profile_path).unwrap();
        let document = ScewinDocument::open(nvram_fixture()).unwrap();
        Self { _temp: temp, profile_path, profile, document }
    }

    fn xmp_id(&self) -> String {
        self.profile
            .settings
            .iter()
            .find(|item| item.question == "Extreme Memory Profile(X.M.P.)")
            .unwrap()
            .export_id
            .clone()
    }
}

#[test]
fn live_profile_generation() {
    let temp = TempDir::new().unwrap();
    let output = temp.path().join("live.json");
    write_live_profile(&nvram_fixture(), Some(&dupes_fixture()), &output, "NVRAM").unwrap();
    let profile = ParsedProfile::load(&output).unwrap();
    assert_eq!(profile.hii_crc32(), "B176E215");
    assert_eq!(profile.settings.len(), 8);
    assert_eq!(profile.settings[0].question, "Multi-Theme");
    assert_eq!(profile.label, "NVRAM");
}

#[test]
fn remap_unchanged_setting() {
    let fx = Fixture::new();
    let queued = fx.document.build_change(&fx.profile, &fx.xmp_id(), "05").unwrap();
    let remapped = remap_changes(&fx.profile, &fx.profile, &fx.document, [&queued]).unwrap();
    assert_eq!(remapped.len(), 1);
    assert_eq!(remapped[0].new_raw, "05");
}

#[test]
fn remap_rejects_stale_value() {
    let fx = Fixture::new();
    let id = fx.xmp_id();
    let queued = fx.document.build_change(&fx.profile, &id, "05").unwrap();
    let mut changed = ParsedProfile::load(&fx.profile_path).unwrap();
    let setting = changed.find_mut(&id).unwrap();
    setting.current_code_hex = Some("05".into());
    setting.current_label = Some("User Profile".into());
    let error = remap_changes(&fx.profile, &changed, &fx.document, [&queued]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
    assert!(error.message.contains("live value changed from Disabled to User Profile"));
}

#[test]
fn remap_rejects_a_changed_hii_crc() {
    let fx = Fixture::new();
    let queued = fx.document.build_change(&fx.profile, &fx.xmp_id(), "05").unwrap();
    let mut changed = ParsedProfile::load(&fx.profile_path).unwrap();
    changed.metadata.hii_crc32 = Some("00000000".into());
    let error = remap_changes(&fx.profile, &changed, &fx.document, [&queued]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::ProfileMismatch);
}

#[test]
fn verification() {
    let fx = Fixture::new();
    let id = fx.xmp_id();
    let queued = fx.document.build_change(&fx.profile, &id, "05").unwrap();
    assert!(!verify_changes(&fx.profile, [&queued]).is_empty());

    let mut verified = ParsedProfile::load(&fx.profile_path).unwrap();
    let setting = verified.find_mut(&id).unwrap();
    setting.current_code_hex = Some("05".into());
    setting.current_label = Some("User Profile".into());
    assert!(verify_changes(&verified, [&queued]).is_empty());
}
