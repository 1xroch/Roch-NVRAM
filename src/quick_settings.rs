// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! Quick Settings presets: the overclocking-relevant knobs of a platform.
//!
//! A preset is a JSON file under `assets/quick_settings/`. It is generated from
//! a stock export and a tuned export of the same board by
//! `make-quick-settings`: every setting whose value differs between the two,
//! sorted into CPU, RAM, and GPU sections.
//!
//! A preset never names a setting by its HII CRC. Each entry carries one or
//! more token/offset/width identities -- the same key Compare and Load NVRAM
//! match on -- so it applies to any export of that firmware family, and an
//! entry with several identities writes the same value to each. That is how
//! the per-channel copies of a DDR5 timing become a single control.
//!
//! [`resolve`] matches a preset against a loaded profile and [`changes_for`]
//! turns one control's value into ordinary [`PendingChange`]s, so whatever the
//! Quick Settings tab queues goes through the same preflight, backup, and
//! verification as a change queued from the NVRAM table.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde_json::Value;

use crate::compare::{has_identity, identity_without_crc};
use crate::core::{is_editable, raw_value, setting_display, ParsedProfile, PendingChange, ScewinDocument};
use crate::error::{Error, Result};
use crate::parser::{BiosSetting, Kind};
use crate::paths;

pub const SCHEMA_VERSION: u64 = 1;
pub const PRESET_SUFFIX: &str = "json";

/// The presets compiled into the executable, used when no `assets/` folder is
/// beside it.
const BUILT_IN: &[(&str, &str)] = &[(
    "msi_z790_ddr5.json",
    include_str!("../assets/quick_settings/msi_z790_ddr5.json"),
)];

pub fn preset_dir() -> Option<PathBuf> {
    paths::asset_root().map(|root| root.join("quick_settings"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickSetting {
    pub name: String,
    pub kind: Kind,
    /// Raw value as SCEWIN writes it: an option code or a value text.
    pub tuned: String,
    /// What the tuned option is called; empty for values.
    pub tuned_label: String,
    pub help: String,
    /// token:offset:width, as [`identity_without_crc`].
    pub identities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickSection {
    pub title: String,
    pub settings: Vec<QuickSetting>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickPreset {
    pub name: String,
    pub platform: String,
    pub source: IndexMap<String, String>,
    pub sections: Vec<QuickSection>,
    pub path: PathBuf,
}

impl QuickPreset {
    pub fn setting_count(&self) -> usize {
        self.sections.iter().map(|section| section.settings.len()).sum()
    }
}

fn require(condition: bool, message: impl FnOnce() -> String) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::validation(format!("Quick Settings preset: {}", message())))
    }
}

/// `str(value)` of a JSON scalar, the way the Python reader saw it.
fn text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(other) => other.to_string(),
    }
}

/// Truthy-or-empty: `str(data.get(key) or "")`.
fn text_or_empty(value: Option<&Value>) -> String {
    match value {
        Some(Value::Bool(false)) => String::new(),
        Some(Value::Number(number)) if number.as_f64() == Some(0.0) => String::new(),
        other => text(other),
    }
}

fn py_repr(text: &str) -> String {
    crate::pycompat::repr_str(text)
}

fn parse_setting(data: &Value, place: &str) -> Result<QuickSetting> {
    let data = data.as_object();
    require(data.is_some(), || format!("{place} is not an object"))?;
    let data = data.unwrap();
    let name = text_or_empty(data.get("name")).trim().to_string();
    require(!name.is_empty(), || format!("{place} has no name"))?;
    let kind_text = text_or_empty(data.get("kind"));
    let kind = match kind_text.as_str() {
        "options" => Kind::Options,
        "value" => Kind::Value,
        _ => {
            return Err(Error::validation(format!(
                "Quick Settings preset: {} has kind {}, expected options or value",
                py_repr(&name),
                py_repr(&kind_text)
            )))
        }
    };
    let mut tuned = text(data.get("tuned"));
    require(!tuned.is_empty(), || format!("{} has no tuned value", py_repr(&name)))?;
    let identities: Vec<String> = match data.get("identities") {
        Some(Value::Array(items)) => items.iter().map(|item| text(Some(item)).to_uppercase()).collect(),
        _ => Vec::new(),
    };
    require(!identities.is_empty(), || format!("{} has no identities", py_repr(&name)))?;
    for identity in &identities {
        require(has_identity(identity) && identity.matches(':').count() == 2, || {
            format!("{} has a malformed identity {}", py_repr(&name), py_repr(identity))
        })?;
    }
    if kind == Kind::Options {
        tuned = tuned.to_uppercase();
    }
    Ok(QuickSetting {
        name,
        kind,
        tuned,
        tuned_label: text_or_empty(data.get("tuned_label")),
        help: text_or_empty(data.get("help")),
        identities,
    })
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Parse a preset from its JSON text. `path` names it in messages.
pub fn parse_preset(json_text: &str, path: &Path) -> Result<QuickPreset> {
    let name_of_file = file_name(path);
    let data: Value = serde_json::from_str(json_text).map_err(|error| {
        Error::validation(format!("Could not read Quick Settings preset {name_of_file}: {error}"))
    })?;
    let data = data.as_object();
    require(data.is_some(), || "top level is not an object".into())?;
    let data = data.unwrap();
    let schema = data.get("schema_version");
    require(schema.and_then(Value::as_f64) == Some(SCHEMA_VERSION as f64), || {
        let shown = match schema {
            None | Some(Value::Null) => "None".to_string(),
            Some(Value::String(text)) => py_repr(text),
            Some(other) => other.to_string(),
        };
        format!("{name_of_file} has schema_version {shown}, expected {SCHEMA_VERSION}")
    })?;
    let name = match text_or_empty(data.get("name")) {
        name if name.is_empty() => path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        name => name,
    };

    let mut sections = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let raw_sections = match data.get("sections") {
        Some(Value::Array(items)) => items.as_slice(),
        _ => &[],
    };
    for (index, raw_section) in raw_sections.iter().enumerate() {
        let raw_section = raw_section.as_object();
        require(raw_section.is_some(), || format!("section {index} is not an object"))?;
        let raw_section = raw_section.unwrap();
        let title = text_or_empty(raw_section.get("title")).trim().to_string();
        require(!title.is_empty(), || format!("section {index} has no title"))?;
        let raw_settings = match raw_section.get("settings") {
            Some(Value::Array(items)) => items.as_slice(),
            _ => &[],
        };
        let settings = raw_settings
            .iter()
            .enumerate()
            .map(|(position, item)| parse_setting(item, &format!("{title} setting {position}")))
            .collect::<Result<Vec<_>>>()?;
        for setting in &settings {
            for identity in &setting.identities {
                require(seen.insert(identity.clone()), || {
                    format!("identity {identity} appears twice ({})", py_repr(&setting.name))
                })?;
            }
        }
        sections.push(QuickSection { title, settings });
    }
    require(!sections.is_empty(), || format!("{name_of_file} has no sections"))?;

    let source = match data.get("source") {
        Some(Value::Object(map)) => map.iter().map(|(key, value)| (key.clone(), text(Some(value)))).collect(),
        _ => IndexMap::new(),
    };
    Ok(QuickPreset {
        name,
        platform: text_or_empty(data.get("platform")),
        source,
        sections,
        path: path.to_path_buf(),
    })
}

pub fn load_preset(path: impl AsRef<Path>) -> Result<QuickPreset> {
    let path = path.as_ref();
    let text = fs::read_to_string(path).map_err(|error| {
        Error::validation(format!("Could not read Quick Settings preset {}: {error}", file_name(path)))
    })?;
    parse_preset(&text, path)
}

fn sort_presets(presets: &mut [QuickPreset]) {
    presets.sort_by_key(|preset| preset.name.to_lowercase());
}

/// Every preset in the directory, by name. A broken file is skipped, not fatal.
pub fn load_presets(directory: impl AsRef<Path>) -> Vec<QuickPreset> {
    let Ok(entries) = fs::read_dir(directory.as_ref()) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == PRESET_SUFFIX))
        .collect();
    paths.sort();
    let mut presets: Vec<QuickPreset> = paths.iter().filter_map(|path| load_preset(path).ok()).collect();
    sort_presets(&mut presets);
    presets
}

/// The presets the window offers: the `assets/quick_settings` folder when
/// there is one, otherwise the ones compiled in.
pub fn default_presets() -> Vec<QuickPreset> {
    if let Some(dir) = preset_dir().filter(|dir| dir.is_dir()) {
        return load_presets(dir);
    }
    let mut presets: Vec<QuickPreset> = BUILT_IN
        .iter()
        .filter_map(|(name, text)| parse_preset(text, &PathBuf::from("assets/quick_settings").join(name)).ok())
        .collect();
    sort_presets(&mut presets);
    presets
}

/// One preset control matched against the loaded NVRAM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSetting {
    pub spec: QuickSetting,
    /// Live settings this control writes.
    pub targets: Vec<BiosSetting>,
    /// Identities the loaded export does not have.
    pub missing: Vec<String>,
    /// Why a matched setting cannot be written.
    pub blocked: Vec<String>,
}

impl ResolvedSetting {
    pub fn available(&self) -> bool {
        !self.targets.is_empty() && self.blocked.is_empty()
    }

    pub fn current_raw(&self) -> Vec<String> {
        self.targets.iter().map(raw_value).collect()
    }

    pub fn matches_tuned(&self) -> bool {
        !self.targets.is_empty() && self.current_raw().iter().all(|raw| *raw == self.spec.tuned)
    }

    pub fn current_display(&self) -> String {
        if self.targets.is_empty() {
            return if self.blocked.is_empty() { "not on this board" } else { "blocked" }.into();
        }
        let mut shown: Vec<String> = Vec::new();
        for target in &self.targets {
            let text = setting_display(target);
            if !shown.contains(&text) {
                shown.push(text);
            }
        }
        shown.join(" / ")
    }
}

pub type ResolvedPage = Vec<(QuickSection, Vec<ResolvedSetting>)>;

/// The preset laid out with nothing loaded: every entry present, none matched.
pub fn placeholders(preset: &QuickPreset) -> ResolvedPage {
    preset
        .sections
        .iter()
        .map(|section| {
            let rows = section
                .settings
                .iter()
                .map(|spec| ResolvedSetting {
                    spec: spec.clone(),
                    targets: Vec::new(),
                    missing: spec.identities.clone(),
                    blocked: Vec::new(),
                })
                .collect();
            (section.clone(), rows)
        })
        .collect()
}

/// Match every preset entry to the loaded export by token/offset/width.
pub fn resolve(preset: &QuickPreset, profile: &ParsedProfile, _document: &ScewinDocument) -> ResolvedPage {
    let mut by_identity: HashMap<String, &BiosSetting> = HashMap::new();
    for setting in &profile.settings {
        let key = identity_without_crc(setting);
        if has_identity(&key) {
            by_identity.entry(key).or_insert(setting);
        }
    }

    preset
        .sections
        .iter()
        .map(|section| {
            let rows = section
                .settings
                .iter()
                .map(|spec| {
                    let mut resolved = ResolvedSetting {
                        spec: spec.clone(),
                        targets: Vec::new(),
                        missing: Vec::new(),
                        blocked: Vec::new(),
                    };
                    for identity in &spec.identities {
                        let Some(live) = by_identity.get(identity) else {
                            resolved.missing.push(identity.clone());
                            continue;
                        };
                        if live.kind != spec.kind {
                            resolved.blocked.push(format!(
                                "{identity}: live setting is {}, preset expects {}",
                                live.kind.as_str(),
                                spec.kind.as_str()
                            ));
                            continue;
                        }
                        let (editable, reason) = is_editable(live);
                        if !editable {
                            resolved.blocked.push(format!("{identity}: {reason}"));
                            continue;
                        }
                        resolved.targets.push((*live).clone());
                    }
                    resolved
                })
                .collect();
            (section.clone(), rows)
        })
        .collect()
}

/// Turn one control's value into pending changes, one per live target.
///
/// Returns `(changes, reset_ids, errors)`. `reset_ids` are targets whose live
/// value already equals the requested one: nothing to write, and any change
/// queued for them earlier should be dropped, the way the edit dialog does it.
pub fn changes_for(
    resolved: &ResolvedSetting,
    raw: &str,
    profile: &ParsedProfile,
    document: &ScewinDocument,
) -> (Vec<PendingChange>, Vec<String>, Vec<String>) {
    let mut changes = Vec::new();
    let mut reset = Vec::new();
    let mut errors = Vec::new();
    let wanted = if resolved.spec.kind == Kind::Options { raw.to_uppercase() } else { raw.to_string() };
    for target in &resolved.targets {
        if wanted == raw_value(target) {
            reset.push(target.export_id.clone());
            continue;
        }
        match document.build_change(profile, &target.export_id, &wanted) {
            Ok(change) => changes.push(change),
            Err(error) => errors.push(format!("{} ({}): {error}", resolved.spec.name, identity_without_crc(target))),
        }
    }
    (changes, reset, errors)
}
