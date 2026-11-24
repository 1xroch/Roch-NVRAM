// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! A native stand-in for SCEWIN's export (`/O`) and import (`/I`), built on the
//! firmware's own HII database ([`crate::hii`]) and its UEFI variables.
//!
//! The export is written in AMISCE's layout, so everything downstream (the
//! parser, the queue, the pre-import checks and the verification) works on it
//! unchanged. Token is the question id, as AMISCE prints it; Offset and Width
//! place the question in its variable.
//!
//! The import goes the other way, and is deliberately narrow: it changes only
//! the bytes of questions the HII database defines, only to a value that
//! question allows (one of its options, 0 or 1 for a checkbox, inside a
//! numeric's range), and refuses the whole import if anything does not check
//! out. Nothing is written here; [`plan_import`] says what to write.

use std::collections::{BTreeMap, HashMap};

use crate::hii::{Guid, HiiDatabase, Question, QuestionKind, VarStore};
use crate::parser::{parse_metadata, parse_record, split_records, Kind};
use crate::pycompat::int_base0;

/// Where the firmware keeps its own variables (SecureBoot, SetupMode, boot
/// entries). Setup questions show them, but they are not settings to write.
const EFI_GLOBAL_VARIABLE: Guid = Guid([
    0x61, 0xdf, 0xe4, 0x8b, 0xca, 0x93, 0xd2, 0x11, 0xaa, 0x0d, 0x00, 0xe0, 0x98, 0x03, 0x2b, 0x8c,
]);

/// Reads a variable's current bytes (without the attribute word). `None` when
/// it cannot be read from here, which leaves its questions out of the export.
pub trait Variables {
    fn read(&self, store: &VarStore) -> Option<Vec<u8>>;
}

/// An export in AMISCE's layout.
#[derive(Clone, Debug)]
pub struct Export {
    pub nvram: String,
    pub dupes: String,
    pub exported: usize,
    pub duplicates: usize,
    /// Questions whose variable could not be read (volatile or boot-time-only ones).
    pub unreadable: usize,
}

fn le_value(data: &[u8], offset: u16, width: u8) -> Option<u64> {
    let bytes = data.get(usize::from(offset)..usize::from(offset) + usize::from(width))?;
    Some(bytes.iter().rev().fold(0u64, |acc, &b| acc << 8 | u64::from(b)))
}

fn code(value: u64, width: u8) -> String {
    format!("{value:0digits$X}", digits = usize::from(width) * 2)
}

/// The (code, label) rows a question offers, for one-ofs and checkboxes.
fn option_rows(question: &Question) -> Option<Vec<(u64, String, bool)>> {
    match &question.kind {
        QuestionKind::OneOf { options } => Some(options.iter().map(|o| (o.value, o.label.clone(), o.default)).collect()),
        QuestionKind::CheckBox { default } => {
            Some(vec![(0, "Disabled".into(), !*default), (1, "Enabled".into(), *default)])
        }
        QuestionKind::Numeric { .. } => None,
    }
}

fn record(question: &Question, value: u64) -> String {
    let prompt = if question.prompt.is_empty() { "N/A" } else { question.prompt.as_str() };
    let mut out = format!(
        "Setup Question\t= {prompt}\nHelp String\t= {}\nToken\t={:X}\t// Do NOT change this line\nOffset\t={:X}\nWidth\t={:02X}\n",
        question.help, question.token, question.offset, question.width
    );
    match (&question.kind, option_rows(question)) {
        (QuestionKind::Numeric { default, .. }, _) => {
            if let Some(default) = default {
                out.push_str(&format!("BIOS Default\t=<{default}>\n"));
            }
            out.push_str(&format!("Value\t=<{value}>\t// Change to the desired value\n"));
        }
        (_, Some(rows)) => {
            if let Some((default, label, _)) = rows.iter().find(|row| row.2) {
                out.push_str(&format!("BIOS Default\t=[{}]{label}\n", code(*default, question.width)));
            }
            for (index, (row_value, label, _)) in rows.iter().enumerate() {
                let star = if *row_value == value { "*" } else { "" };
                let row = format!("{star}[{}]{label}", code(*row_value, question.width));
                if index == 0 {
                    out.push_str(&format!("Options\t={row}\t// Move \"*\" to the desired Option\n"));
                } else {
                    out.push_str(&format!("\t{row}\n"));
                }
            }
        }
        (_, None) => {}
    }
    out
}

/// The header AMISCE writes, with this tool named in place of AMISCE.
fn header(script: &str, created: &str, crc32: u32) -> String {
    format!(
        "// Script File Name : {script}\n// Created on {created}\n// Roch NVRAM {} native export (no AMISCE). Same layout as AMISCE.\nHIICrc32= {crc32:08X}\n\n",
        crate::VERSION
    )
}

/// Every readable question, in form order. A question whose storage another
/// question already covers (the same variable, offset and width) goes to the
/// duplicates file, commented out, as SCEWIN's `/SD` does.
pub fn export(db: &HiiDatabase, variables: &dyn Variables, script: &str, created: &str) -> Export {
    let mut cache: HashMap<usize, Option<Vec<u8>>> = HashMap::new();
    let mut seen: HashMap<(usize, u16, u8), ()> = HashMap::new();
    let mut nvram = header(script, created, db.crc32);
    let mut dupes = header("Dupes.txt", created, db.crc32);
    let (mut exported, mut duplicates, mut unreadable) = (0, 0, 0);
    for question in &db.questions {
        let data = cache.entry(question.varstore).or_insert_with(|| variables.read(&db.varstores[question.varstore]));
        let Some(value) = data.as_deref().and_then(|data| le_value(data, question.offset, question.width)) else {
            unreadable += 1;
            continue;
        };
        let text = record(question, value);
        if seen.insert((question.varstore, question.offset, question.width), ()).is_some() {
            for line in text.lines() {
                dupes.push_str(&format!("// {line}\n"));
            }
            dupes.push_str("//\n");
            duplicates += 1;
        } else {
            nvram.push_str(&text);
            nvram.push('\n');
            exported += 1;
        }
    }
    Export { nvram, dupes, exported, duplicates, unreadable }
}

/// A variable being edited: its bytes as read, as they will be written, and what changed.
type Edit = (Vec<u8>, Vec<u8>, Vec<String>);

/// One variable to write: its new bytes and what changed in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VarWrite {
    pub store: VarStore,
    pub data: Vec<u8>,
    pub changes: Vec<String>,
}

/// Works out the variable writes an edited export asks for, checking every
/// value against what its question allows. Records that match the live value
/// change nothing. Any record that cannot be placed or does not check out
/// refuses the whole import.
pub fn plan_import(db: &HiiDatabase, variables: &dyn Variables, text: &str) -> Result<Vec<VarWrite>, String> {
    let crc = parse_metadata(text).hii_crc32.unwrap_or_default();
    let live_crc = format!("{:08X}", db.crc32);
    if crc != live_crc {
        return Err(format!(
            "The file was exported from a different BIOS setup (HIICrc32 {}, this one is {live_crc}). Nothing was written.",
            if crc.is_empty() { "missing".into() } else { crc }
        ));
    }

    // The first question at each place, as the export lists it.
    let mut by_place: HashMap<(u16, u16, u8), &Question> = HashMap::new();
    for question in &db.questions {
        by_place.entry((question.token, question.offset, question.width)).or_insert(question);
    }

    // Per variable: its bytes as read, as they will be written, and what changed.
    let mut buffers: BTreeMap<usize, Edit> = BTreeMap::new();
    let mut problems = Vec::new();
    for block in split_records(text) {
        let setting = parse_record(&block, "import", Some(&crc));
        let name = if setting.question.is_empty() { "N/A".to_string() } else { setting.question.clone() };
        let place = (setting.token, setting.offset, setting.width);
        let (Some(token), Some(offset), Some(width)) = place else {
            problems.push(format!("{name}: no Token, Offset or Width"));
            continue;
        };
        let place = (u16::try_from(token).ok(), u16::try_from(offset).ok(), u8::try_from(width).ok());
        let Some(question) = (match place {
            (Some(t), Some(o), Some(w)) => by_place.get(&(t, o, w)).copied(),
            _ => None,
        }) else {
            problems.push(format!("{name}: Token {token:X} at offset {offset:X} is not a setting in this BIOS"));
            continue;
        };
        let wanted = match setting.kind {
            Kind::Options => setting.current_code.and_then(|c| u64::try_from(c).ok()),
            Kind::Value => setting.current_value.as_deref().and_then(int_base0).and_then(|v| u64::try_from(v).ok()),
            Kind::Unknown => None,
        };
        let Some(wanted) = wanted else {
            // The export itself leaves no row selected when the live value is none of the
            // options (the BIOS fills such questions in at boot). Unedited, it asks for nothing.
            if setting.kind == Kind::Options && !setting.options.iter().any(|o| o.selected) {
                continue;
            }
            problems.push(format!("{name}: no single value is selected"));
            continue;
        };
        let store = &db.varstores[question.varstore];
        let entry = match buffers.entry(question.varstore) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => match variables.read(store) {
                Some(data) => entry.insert((data.clone(), data, Vec::new())),
                None => {
                    problems.push(format!("{name}: its variable {} cannot be read", store.name));
                    continue;
                }
            },
        };
        let (start, end) = (usize::from(question.offset), usize::from(question.offset) + usize::from(question.width));
        let Some(slot) = entry.1.get_mut(start..end) else {
            problems.push(format!("{name}: offset {offset:X} is past the end of {}", store.name));
            continue;
        };
        let current = slot.iter().rev().fold(0u64, |acc, &b| acc << 8 | u64::from(b));
        // Only a change is checked: what the BIOS holds now is not this tool's to judge.
        if current == wanted {
            continue;
        }
        let allowed = match &question.kind {
            QuestionKind::OneOf { options } => options.iter().any(|o| o.value == wanted),
            QuestionKind::CheckBox { .. } => wanted <= 1,
            QuestionKind::Numeric { min, max, .. } => min > max || (*min..=*max).contains(&wanted),
        };
        let fits = u32::from(question.width) >= 8 || wanted < 1u64 << (u32::from(question.width) * 8);
        if !allowed || !fits {
            problems.push(format!("{name}: {wanted} is not a value this setting allows"));
            continue;
        }
        if store.guid == EFI_GLOBAL_VARIABLE {
            problems.push(format!("{name}: {} is the firmware's own variable; change it in BIOS setup", store.name));
            continue;
        }
        slot.copy_from_slice(&wanted.to_le_bytes()[..usize::from(question.width)]);
        entry.2.push(format!("{name}: {current} -> {wanted} ({} offset {offset:X})", store.name));
    }
    if !problems.is_empty() {
        let shown: Vec<String> = problems.iter().take(20).cloned().collect();
        return Err(format!("Nothing was written; {} setting(s) did not check out:\n{}", problems.len(), shown.join("\n")));
    }
    Ok(buffers
        .into_iter()
        .filter(|(_, (before, after, _))| before != after)
        .map(|(index, (_, data, changes))| VarWrite { store: db.varstores[index].clone(), data, changes })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::hii::tests::sample;
    use crate::parser::{normalize_commented_dupes, parse_metadata};

    struct Fake(RefCell<HashMap<String, Vec<u8>>>);

    impl Variables for Fake {
        fn read(&self, store: &VarStore) -> Option<Vec<u8>> {
            self.0.borrow().get(&store.name).cloned()
        }
    }

    fn setup_variable() -> Fake {
        // Multi-Theme 1, base clock 10000 (0x2710), Fast Boot off.
        let mut data = vec![0u8; 16];
        data[0] = 1;
        data[2..4].copy_from_slice(&10000u16.to_le_bytes());
        Fake(RefCell::new(HashMap::from([("Setup".to_string(), data)])))
    }

    #[test]
    fn the_export_reads_back_through_the_parser() {
        let db = HiiDatabase::parse(&sample());
        let export = export(&db, &setup_variable(), "nvram.txt", "11/24/26 at 13:00:00");
        assert_eq!((export.exported, export.duplicates, export.unreadable), (4, 1, 0));
        let crc = format!("{:08X}", db.crc32);
        assert_eq!(parse_metadata(&export.nvram).hii_crc32.as_deref(), Some(crc.as_str()));
        let settings: Vec<_> = split_records(&export.nvram).iter().map(|b| parse_record(b, "nvram", Some(&crc))).collect();
        assert_eq!(settings.len(), 4);
        let theme = &settings[0];
        assert_eq!((theme.question.as_str(), theme.token_hex.as_str(), theme.offset_hex.as_str(), theme.width_hex.as_str()), ("Multi-Theme", "12", "0", "01"));
        assert_eq!(theme.current_label.as_deref(), Some("Theme 2"));
        assert_eq!(theme.bios_default_label.as_deref(), Some("Theme 1"));
        assert!(theme.warnings.is_empty(), "{:?}", theme.warnings);
        assert_eq!(settings[1].current_value.as_deref(), Some("10000"));
        assert_eq!(settings[1].bios_default_value.as_deref(), Some("10000"));
        assert_eq!(settings[2].current_label.as_deref(), Some("Disabled"));
        assert_eq!(split_records(&normalize_commented_dupes(&export.dupes)).len(), 1);
    }

    #[test]
    fn an_unreadable_variable_leaves_its_questions_out() {
        let db = HiiDatabase::parse(&sample());
        let none = Fake(RefCell::new(HashMap::new()));
        let export = export(&db, &none, "nvram.txt", "now");
        assert_eq!((export.exported, export.unreadable), (0, 5));
    }

    fn edited(db: &HiiDatabase, vars: &Fake, from: &str, to: &str) -> String {
        let text = export(db, vars, "nvram.txt", "now").nvram;
        assert!(text.contains(from), "{from} not in export");
        text.replacen(from, to, 1)
    }

    #[test]
    fn an_import_writes_only_what_changed_within_what_is_allowed() {
        let db = HiiDatabase::parse(&sample());
        let vars = setup_variable();
        let unchanged = export(&db, &vars, "nvram.txt", "now").nvram;
        assert_eq!(plan_import(&db, &vars, &unchanged).unwrap(), Vec::new());

        let text = edited(&db, &vars, "Value\t=<10000>", "Value\t=<12500>");
        let text = text.replacen("Options\t=*[00]Disabled", "Options\t=[00]Disabled", 1).replacen("\t[01]Enabled", "\t*[01]Enabled", 1);
        let writes = plan_import(&db, &vars, &text).unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].store.name, "Setup");
        assert_eq!(&writes[0].data[2..5], &[0xD4, 0x30, 1], "12500 little-endian, then Fast Boot on");
        assert_eq!(writes[0].data[0], 1, "untouched bytes stay");
        assert_eq!(writes[0].changes.len(), 2);
    }

    #[test]
    fn values_the_bios_set_itself_do_not_block_an_import_that_leaves_them_alone() {
        let db = HiiDatabase::parse(&sample());
        let vars = setup_variable();
        // Multi-Theme holds 7, which is none of its options, so the export selects no row.
        vars.0.borrow_mut().get_mut("Setup").unwrap()[0] = 7;
        let text = edited(&db, &vars, "Value\t=<10000>", "Value\t=<12500>");
        let writes = plan_import(&db, &vars, &text).unwrap();
        assert_eq!(writes[0].changes.len(), 1, "only the edited value");
        assert_eq!(writes[0].data[0], 7, "and the odd one stays as it was");
    }

    #[test]
    fn an_import_refuses_values_outside_the_question_and_other_bioses() {
        let db = HiiDatabase::parse(&sample());
        let vars = setup_variable();
        let out_of_range = edited(&db, &vars, "Value\t=<10000>", "Value\t=<50000>");
        assert!(plan_import(&db, &vars, &out_of_range).unwrap_err().contains("not a value this setting allows"));
        let no_such_option = edited(&db, &vars, "*[01]Theme 2", "*[07]Theme 2");
        assert!(plan_import(&db, &vars, &no_such_option).is_err());
        let moved = edited(&db, &vars, "Offset\t=2\n", "Offset\t=6\n");
        assert!(plan_import(&db, &vars, &moved).unwrap_err().contains("is not a setting in this BIOS"));
        let other = edited(&db, &vars, "HIICrc32= ", "HIICrc32= 1");
        assert!(plan_import(&db, &vars, &other).unwrap_err().contains("different BIOS setup"));
    }
}
