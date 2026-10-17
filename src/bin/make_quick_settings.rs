// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! Build a Quick Settings preset from a stock export and a tuned export.
//!
//! ```text
//! make-quick-settings stock.txt tuned.txt --vendor ASUS --board "ROG Maximus Z790 Apex" ^
//!     --name "ASUS Z790 (LGA 1700 DDR5)" --platform asus-z790-ddr5 ^
//!     --out assets/quick_settings/asus_z790_ddr5.json
//! ```
//!
//! Every setting whose value differs between the two files is a candidate. The
//! RULES table sorts candidates into the CPU, RAM, and GPU sections by name and
//! help text, and drops the ones that change between exports without being
//! overclocking controls (fan curves, the driver installer, DMI lane
//! coefficients, derived read-backs). Anything the rules do not place is
//! listed at the end so a new board's leftovers are visible rather than
//! silently lost.
//!
//! Candidates with the same name, kind, and tuned value collapse into one
//! control that writes all of their identities -- the per-channel copies of a
//! DDR5 timing. Same name but a different tuned value stays separate, with
//! the value in the name.
//!
//! The EXTRA list adds the controls that belong on the page whether or not the
//! tuned export touched them -- Hyper-Threading, XMP, the common timings --
//! when the exports have them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::{Regex, RegexBuilder};
use roch_nvram::compare::{has_identity, identity_without_crc};
use roch_nvram::core::raw_value;
use roch_nvram::parser::{parse_sce_file, BiosSetting, Metadata};
use roch_nvram::pycompat::{text_mode, to_json, JsonStyle};
use roch_nvram::quick_settings::SCHEMA_VERSION;
use serde_json::{json, Value};

const CPU: &str = "CPU";
const RAM: &str = "RAM";
const GPU: &str = "GPU / PCIe";

/// (section, name pattern, help pattern). First match wins; `None` means drop.
const RULES: &[(Option<&str>, &str, &str)] = &[
    // Not overclocking controls, even though a tuned export changes them.
    (None, r"^tZQCS$", r""), // ZQ calibration interval, not a tuning knob
    (None, r"Fan\d* level|Fan Speed", r""),
    (None, r"Driver Utility Installer", r""),
    (None, r"^DMI Gen", r""),
    (None, r"^M2_\d", r""),                   // NVMe slot link speed
    (None, r"^CRSV\d", r""),                  // reserved V/F points the firmware fills in
    (None, r"Core Dis Value", r""),           // internal core-disable mask
    (None, r"Adjusted DRAM Frequency", r""),  // read-back of the applied clock
    (None, r"High Temperature Threshold", r""), // tuned value is a Reserved code
    (None, r"Core Voltage Offset Mode", r""), // tuned code has no label
    (None, r"^Fast Boot$", r"Windows"),       // OS fast startup, not the MRC one
    // Memory: timings, clocks, training, the DIMM rails and the IMC rails.
    (Some(RAM), r"^Fast Boot$", r"MRC"),
    (Some(RAM), r"^t[A-Z][A-Za-z_0-9]*$", r""),
    (
        Some(RAM),
        r"DRAM|Memory|IMC|Gear|Command Rate|Power Down Mode|Vddq Training|Round Trip|SA GV|Extreme Memory|XMP|Try It",
        r"",
    ),
    (Some(RAM), r"^(VDD|VDDQ|VPP) ", r""),
    (Some(RAM), r"^CPU (SA|VDDQ|VDD2) Voltage", r""),
    // The graphics slot and the PCIe features a GPU cares about.
    (Some(GPU), r"Re-?Size BAR|Resizable BAR|^PCI_E1 |PCIe Native Power", r""),
    // Everything else about the processor.
    (
        Some(CPU),
        r"CPU|Core|Ring|EIST|Speed Shift|Turbo|C-State|Power Limit|AVX|TVB|CEP|Loadline|Over Voltage|Over Current|Switching Frequency|VRM|Cooler|Current Limit|Hyper|Lite Load|TjMax|Package C|Thermal Velocity",
        r"",
    ),
];

/// Controls that belong on the page even when the two exports agree on them:
/// the switches and timings anyone tuning this platform reaches for, whether
/// or not the tuned export happened to touch them. Matched exactly, so
/// "Hyper-Threading" is the global switch, not the eight per-core copies. A
/// name the exports do not have is simply not emitted.
const EXTRA: &[(&str, &str)] = &[
    ("Hyper-Threading", CPU),
    ("Active P-Cores", CPU),
    ("E-Core Ratio", CPU),
    ("E-Core Ratio Apply Mode", CPU),
    ("CPU Lite Load", CPU),
    ("CPU Lite Load Control", CPU),
    ("AC Loadline", CPU),
    ("DC Loadline", CPU),
    ("CPU Core Voltage Offset Mode", CPU),
    ("CPU Core Voltage Offset", CPU),
    ("TjMax Offset", CPU),
    ("Package C State Limit", CPU),
    ("Thermal Velocity Boost", CPU),
    ("Enhanced Thermal Velocity Boost", CPU),
    ("Extreme Memory Profile(XMP)", RAM),
    ("Memory Try It!", RAM),
    ("SA GV", RAM),
    ("tMOD", RAM),
    ("tWR", RAM),
    ("tRTP", RAM),
    ("tRFC", RAM),
    ("tRFC4", RAM),
    ("tWTR", RAM),
    ("tWTR_L", RAM),
    ("tWTR_S", RAM),
    ("tCCD", RAM),
    ("tCCD_L", RAM),
    ("tRRD_S", RAM),
];

/// (name pattern, help pattern, display name). Applied after classification.
const RENAMES: &[(&str, &str, &str)] = &[
    (r"^Fast Boot$", r"MRC", "MRC Fast Boot"), // the memory-training one, not Windows'
];

struct Rule {
    section: Option<&'static str>,
    name: Regex,
    help: Option<Regex>,
}

fn compile(name: &str, help: &str) -> (Regex, Option<Regex>) {
    let help = (!help.is_empty()).then(|| RegexBuilder::new(help).case_insensitive(true).build().unwrap());
    (Regex::new(name).unwrap(), help)
}

static COMPILED_RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    RULES
        .iter()
        .map(|(section, name, help)| {
            let (name, help) = compile(name, help);
            Rule { section: *section, name, help }
        })
        .collect()
});

static COMPILED_RENAMES: LazyLock<Vec<(Regex, Option<Regex>, &'static str)>> = LazyLock::new(|| {
    RENAMES
        .iter()
        .map(|(name, help, display)| {
            let (name, help) = compile(name, help);
            (name, help, *display)
        })
        .collect()
});

static PADDING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s{2,}").unwrap());

/// The setting name without the value MSI pads onto it.
///
/// An MSI export writes "tCL                          40": the name, a run of
/// spaces, then the current value. Keeping that would show the stock value in
/// the label and stop the two channel copies from grouping once one of them is
/// changed, so everything after the first run of two or more spaces is
/// dropped.
fn canonical_name(question: &str) -> String {
    PADDING.split(question.trim()).next().unwrap_or("").trim().to_string()
}

fn matches(name_pattern: &Regex, help_pattern: &Option<Regex>, name: &str, help: &str) -> bool {
    name_pattern.is_match(name) && help_pattern.as_ref().is_none_or(|pattern| pattern.is_match(help))
}

fn rename(name: &str, help: &str) -> String {
    COMPILED_RENAMES
        .iter()
        .find(|(name_pattern, help_pattern, _)| matches(name_pattern, help_pattern, name, help))
        .map_or_else(|| name.to_string(), |(_, _, display)| display.to_string())
}

enum Placement {
    Section(&'static str),
    Drop,
    NoRule,
}

fn classify(name: &str, help: &str) -> Placement {
    match COMPILED_RULES.iter().find(|rule| matches(&rule.name, &rule.help, name, help)) {
        Some(Rule { section: Some(section), .. }) => Placement::Section(section),
        Some(_) => Placement::Drop,
        None => Placement::NoRule,
    }
}

struct Args {
    stock: PathBuf,
    tuned: PathBuf,
    name: String,
    platform: String,
    vendor: String,
    board: String,
    out: PathBuf,
}

const USAGE: &str = "usage: make-quick-settings STOCK TUNED --name NAME --platform PLATFORM [--vendor VENDOR] [--board BOARD] --out OUT";

fn parse_args() -> Result<Args, String> {
    let mut positional = Vec::new();
    let mut flags: HashMap<String, String> = HashMap::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "-h" || arg == "--help" {
            return Err(String::new());
        }
        if let Some(flag) = arg.strip_prefix("--") {
            let (flag, value) = match flag.split_once('=') {
                Some((flag, value)) => (flag.to_string(), value.to_string()),
                None => (flag.to_string(), args.next().ok_or(format!("--{flag} expects a value"))?),
            };
            if !["name", "platform", "vendor", "board", "out"].contains(&flag.as_str()) {
                return Err(format!("unrecognized argument: --{flag}"));
            }
            flags.insert(flag, value);
        } else {
            positional.push(arg);
        }
    }
    if positional.len() != 2 {
        return Err("expected STOCK and TUNED".into());
    }
    let mut required = |name: &str| flags.remove(name).ok_or(format!("the following arguments are required: --{name}"));
    let (name, platform, out) = (required("name")?, required("platform")?, required("out")?);
    Ok(Args {
        stock: PathBuf::from(&positional[0]),
        tuned: PathBuf::from(&positional[1]),
        name,
        platform,
        vendor: flags.remove("vendor").unwrap_or_default(),
        board: flags.remove("board").unwrap_or_default(),
        out: PathBuf::from(out),
    })
}

fn load(path: &Path) -> Result<(Metadata, Vec<BiosSetting>), String> {
    let parsed = parse_sce_file(path, "nvram", false).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((parsed.metadata, parsed.settings))
}

/// (section, name, kind, tuned value): what makes candidates one control.
type GroupKey = (String, String, String, String);

#[derive(Debug)]
struct Entry {
    name: String,
    kind: String,
    tuned: String,
    tuned_label: String,
    help: String,
    identities: Vec<String>,
}

fn run(args: Args) -> Result<(), String> {
    let (stock_meta, stock) = load(&args.stock)?;
    let (tuned_meta, tuned) = load(&args.tuned)?;
    let mut tuned_by_id: HashMap<String, &BiosSetting> = HashMap::new();
    for setting in &tuned {
        let key = identity_without_crc(setting);
        if has_identity(&key) {
            tuned_by_id.insert(key, setting); // last wins, like a dict comprehension
        }
    }

    let mut groups: IndexMap<GroupKey, Entry> = IndexMap::new();
    let mut section_of: HashMap<GroupKey, &'static str> = HashMap::new();
    let mut unplaced = Vec::new();
    let mut dropped = Vec::new();
    let mut seen = HashSet::new();
    let mut placed = HashSet::new();

    let mut place = |section: &'static str, name: &str, before: &BiosSetting, after: &BiosSetting, key: &str,
                     placed: &mut HashSet<String>| {
        let kind = before.kind.as_str().to_string();
        let tuned_raw = Some(raw_value(after)).filter(|raw| !raw.is_empty()).unwrap_or_else(|| raw_value(before));
        let tuned_label = if kind == "options" { after.current_label.clone().unwrap_or_default() } else { String::new() };
        let group = (section.to_string(), name.to_string(), kind.clone(), tuned_raw.clone());
        section_of.insert(group.clone(), section);
        groups
            .entry(group)
            .or_insert_with(|| Entry {
                name: name.to_string(),
                kind,
                tuned: tuned_raw,
                tuned_label,
                help: before.help.clone(),
                identities: Vec::new(),
            })
            .identities
            .push(key.to_string());
        placed.insert(key.to_string());
    };

    // First, what the tuned export changed.
    for before in &stock {
        let key = identity_without_crc(before);
        if !has_identity(&key) || !seen.insert(key.clone()) {
            continue;
        }
        let Some(after) = tuned_by_id.get(&key).filter(|after| after.kind == before.kind) else {
            continue;
        };
        if raw_value(before) == raw_value(after) {
            continue;
        }
        let name = canonical_name(&before.question);
        let section = classify(&name, &before.help);
        let name = rename(&name, &before.help);
        let label = format!("{name}  [{key}]  {} -> {}", raw_value(before), raw_value(after));
        match section {
            Placement::Drop => dropped.push(label),
            Placement::NoRule => unplaced.push(label),
            Placement::Section(section) => place(section, &name, before, after, &key, &mut placed),
        }
    }

    // Then the standing list, for anything the exports agree on.
    let extra: HashMap<&str, &'static str> = EXTRA.iter().copied().collect();
    for before in &stock {
        let key = identity_without_crc(before);
        if !has_identity(&key) || placed.contains(&key) {
            continue;
        }
        let name = canonical_name(&before.question);
        let Some(&section) = extra.get(name.as_str()) else {
            continue;
        };
        let after = tuned_by_id.get(&key).copied().unwrap_or(before);
        if after.kind != before.kind {
            continue;
        }
        if raw_value(after).is_empty() && raw_value(before).is_empty() {
            unplaced.push(format!("{name}  [{key}]  no readable value; left out"));
            continue;
        }
        place(section, &name, before, after, &key, &mut placed);
    }
    drop(place);

    // A name that appears with two different tuned values is two controls;
    // tell them apart by the value rather than by an opaque token. When two
    // records also share the label (two "SA GV" switches that both read
    // Disabled) the token is all that is left to tell them apart.
    let mut by_name: IndexMap<(String, String), Vec<GroupKey>> = IndexMap::new();
    for key in groups.keys() {
        by_name.entry((key.0.clone(), key.1.clone())).or_default().push(key.clone());
    }
    for keys in by_name.values().filter(|keys| keys.len() >= 2) {
        let labelled: Vec<String> = keys
            .iter()
            .map(|key| {
                let entry = &groups[key];
                let shown = if entry.tuned_label.is_empty() { &entry.tuned } else { &entry.tuned_label };
                format!("{} ({shown})", entry.name)
            })
            .collect();
        for (key, candidate) in keys.iter().zip(&labelled) {
            let entry = groups.get_mut(key).unwrap();
            let mut candidate = candidate.clone();
            if labelled.iter().filter(|other| **other == candidate).count() > 1 {
                let token = entry.identities[0].split(':').next().unwrap_or("").to_string();
                candidate = format!("{}, token {token})", &candidate[..candidate.len() - 1]);
            }
            entry.name = candidate;
        }
    }

    let mut sections: IndexMap<&str, Vec<&Entry>> = [CPU, RAM, GPU].into_iter().map(|title| (title, Vec::new())).collect();
    for (key, entry) in &groups {
        sections.get_mut(section_of[key]).unwrap().push(entry);
    }

    let text = |value: &Option<String>| value.clone().unwrap_or_default();
    let preset = json!({
        "schema_version": SCHEMA_VERSION,
        "name": args.name,
        "platform": args.platform,
        "source": {
            // Identities are a vendor's layout. An ASUS, ASRock, or Gigabyte
            // board will not match a preset built from an MSI export, by design.
            "vendor": args.vendor,
            "board": args.board,
            "stock_hii_crc32": text(&stock_meta.hii_crc32),
            "tuned_hii_crc32": text(&tuned_meta.hii_crc32),
            "amisce_version": text(&tuned_meta.amisce_version),
            "stock_created_on": text(&stock_meta.created_on),
            "tuned_created_on": text(&tuned_meta.created_on),
        },
        "sections": sections
            .iter()
            .filter(|(_, entries)| !entries.is_empty())
            .map(|(title, entries)| json!({
                "title": title,
                "settings": entries.iter().map(|entry| json!({
                    "name": entry.name,
                    "kind": entry.kind,
                    "tuned": entry.tuned,
                    "tuned_label": entry.tuned_label,
                    "help": entry.help,
                    "identities": entry.identities,
                })).collect::<Vec<Value>>(),
            }))
            .collect::<Vec<Value>>(),
    });
    if let Some(parent) = args.out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(&args.out, text_mode(&(to_json(&preset, JsonStyle::Indent2, false) + "\n")))
        .map_err(|e| format!("{}: {e}", args.out.display()))?;

    let total: usize = sections.values().map(Vec::len).sum();
    let identities: usize = sections.values().flatten().map(|entry| entry.identities.len()).sum();
    println!("wrote {}: {total} controls over {identities} identities", args.out.display());
    for (title, entries) in &sections {
        println!("  {title}: {}", entries.len());
    }
    if !dropped.is_empty() {
        println!("\ndropped as not overclocking controls ({}):", dropped.len());
        for line in &dropped {
            println!("  - {line}");
        }
    }
    if !unplaced.is_empty() {
        println!("\nUNPLACED -- changed but no rule matched ({}); add a rule or they are left out:", unplaced.len());
        for line in &unplaced {
            println!("  ? {line}");
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) if message.is_empty() => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("{USAGE}\nmake-quick-settings: error: {message}");
            return ExitCode::from(2);
        }
    };
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("make-quick-settings: {message}");
            ExitCode::FAILURE
        }
    }
}
