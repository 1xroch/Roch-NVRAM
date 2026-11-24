// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! The firmware's HII database: what each setup question is called, where it
//! is stored, what it may hold and what it defaults to.
//!
//! An AMI BIOS exports the database for its own tools (SCEWIN reads it the
//! same way) and publishes where in the `HiiDB` UEFI variable. The bytes are
//! the UEFI `ExportPackageLists` layout: package lists, each a GUID and a
//! length followed by packages. Two kinds matter here:
//!
//! - **Forms** (type 2): the IFR, a stream of opcodes. Varstores name the UEFI
//!   variables the questions live in; one-of, numeric and checkbox questions
//!   give a prompt, a varstore, an offset and a width; options and defaults
//!   follow inside the question's scope.
//! - **Strings** (type 4): one package per language, a sequence of string
//!   blocks numbered from 1. The prompts and option names are ids into it.
//!
//! Everything here reads bytes and nothing else, so it is the same on every
//! platform and is tested against synthetic databases.

use std::collections::HashMap;
use std::fmt;

/// A UEFI GUID as the firmware stores it (the first three fields little-endian).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Guid(pub [u8; 16]);

impl Guid {
    pub fn from_slice(bytes: &[u8]) -> Option<Guid> {
        Some(Guid(bytes.get(..16)?.try_into().ok()?))
    }
}

impl fmt::Display for Guid {
    /// The text form efivarfs uses in its file names.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            u16::from_le_bytes([b[4], b[5]]),
            u16::from_le_bytes([b[6], b[7]]),
            b[8],
            b[9],
            b[10],
            b[11],
            b[12],
            b[13],
            b[14],
            b[15]
        )
    }
}

impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// A UEFI variable that holds questions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VarStore {
    pub name: String,
    pub guid: Guid,
    pub size: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OneOfOption {
    pub value: u64,
    pub label: String,
    pub default: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionKind {
    OneOf { options: Vec<OneOfOption> },
    Numeric { min: u64, max: u64, step: u64, default: Option<u64> },
    CheckBox { default: bool },
}

/// One setup question with storage in a UEFI variable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    /// The question id, which SCEWIN calls the Token.
    pub token: u16,
    pub prompt: String,
    pub help: String,
    /// Index into [`HiiDatabase::varstores`].
    pub varstore: usize,
    pub offset: u16,
    pub width: u8,
    pub kind: QuestionKind,
}

/// The parsed database, questions in the order the forms list them.
#[derive(Clone, Debug, Default)]
pub struct HiiDatabase {
    /// CRC-32 of the exported bytes: changes whenever the BIOS's setup changes.
    pub crc32: u32,
    pub varstores: Vec<VarStore>,
    pub questions: Vec<Question>,
}

const PACKAGE_FORMS: u8 = 0x02;
const PACKAGE_STRINGS: u8 = 0x04;
const PACKAGE_END: u8 = 0xDF;

const IFR_ONE_OF: u8 = 0x05;
const IFR_CHECKBOX: u8 = 0x06;
const IFR_NUMERIC: u8 = 0x07;
const IFR_ONE_OF_OPTION: u8 = 0x09;
const IFR_FORM_SET: u8 = 0x0E;
const IFR_VARSTORE: u8 = 0x24;
const IFR_VARSTORE_EFI: u8 = 0x26;
const IFR_END: u8 = 0x29;
const IFR_DEFAULT: u8 = 0x5B;

const OPTION_DEFAULT: u8 = 0x10;
const CHECKBOX_DEFAULT: u8 = 0x01;
/// EFI_HII_DEFAULT_CLASS_STANDARD: what "BIOS Default" means.
const DEFAULT_STANDARD: u16 = 0;

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// An unsigned little-endian number of `size` bytes.
fn uint_at(data: &[u8], at: usize, size: usize) -> Option<u64> {
    let bytes = data.get(at..at + size)?;
    Some(bytes.iter().rev().fold(0u64, |acc, &b| acc << 8 | u64::from(b)))
}

/// The IFR value types' sizes: u8, u16, u32, u64, boolean.
fn type_size(kind: u8) -> Option<usize> {
    match kind {
        0 => Some(1),
        1 => Some(2),
        2 => Some(4),
        3 => Some(8),
        4 => Some(1),
        _ => None,
    }
}

/// A prompt as one line of text: the firmware's own line breaks and the 0x7F it
/// uses as a hard space become spaces.
fn clean(text: &str) -> String {
    let spaced: String = text.chars().map(|c| if c.is_control() || c == '\u{7f}' { ' ' } else { c }).collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// CRC-32 (IEEE), as SCEWIN's HIICrc32.
pub fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, entry) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *entry = c;
    }
    !data.iter().fold(!0u32, |c, &b| table[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8))
}

/// Reads a string package: its language and strings by id. `None` when the
/// package is malformed rather than guessing past the damage.
fn parse_strings(package: &[u8]) -> Option<(String, HashMap<u16, String>)> {
    let header_size = u32_at(package, 4)? as usize;
    let info_offset = u32_at(package, 8)? as usize;
    let language_bytes = package.get(46..header_size)?;
    let language = String::from_utf8_lossy(language_bytes.split(|&b| b == 0).next()?).into_owned();
    let mut strings = HashMap::new();
    let mut id: u16 = 1;
    let mut at = info_offset;
    let ucs2_end = |from: usize| -> Option<usize> {
        let mut end = from;
        while u16_at(package, end)? != 0 {
            end += 2;
        }
        Some(end)
    };
    let ucs2 = |from: usize, to: usize| -> String {
        let units: Vec<u16> = package[from..to].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    };
    let byte_end = |from: usize| -> Option<usize> { Some(from + package.get(from..)?.iter().position(|&b| b == 0)?) };
    loop {
        let block = *package.get(at)?;
        match block {
            0x00 => break,
            // SIBT_STRING_SCSU and _FONT: one string of bytes, the font form with a font id first.
            0x10 | 0x11 => {
                let start = at + 1 + usize::from(block == 0x11);
                let end = byte_end(start)?;
                strings.insert(id, String::from_utf8_lossy(&package[start..end]).into_owned());
                id = id.wrapping_add(1);
                at = end + 1;
            }
            // SIBT_STRINGS_SCSU and _FONT: a count, then that many.
            0x12 | 0x13 => {
                let count_at = at + 1 + usize::from(block == 0x13);
                let count = u16_at(package, count_at)?;
                at = count_at + 2;
                for _ in 0..count {
                    let end = byte_end(at)?;
                    strings.insert(id, String::from_utf8_lossy(&package[at..end]).into_owned());
                    id = id.wrapping_add(1);
                    at = end + 1;
                }
            }
            // SIBT_STRING_UCS2 and _FONT.
            0x14 | 0x15 => {
                let start = at + 1 + usize::from(block == 0x15);
                let end = ucs2_end(start)?;
                strings.insert(id, ucs2(start, end));
                id = id.wrapping_add(1);
                at = end + 2;
            }
            // SIBT_STRINGS_UCS2 and _FONT.
            0x16 | 0x17 => {
                let count_at = at + 1 + usize::from(block == 0x17);
                let count = u16_at(package, count_at)?;
                at = count_at + 2;
                for _ in 0..count {
                    let end = ucs2_end(at)?;
                    strings.insert(id, ucs2(at, end));
                    id = id.wrapping_add(1);
                    at = end + 2;
                }
            }
            // SIBT_DUPLICATE: the same text as an earlier id.
            0x20 => {
                let original = u16_at(package, at + 1)?;
                let text = strings.get(&original).cloned().unwrap_or_default();
                strings.insert(id, text);
                id = id.wrapping_add(1);
                at += 3;
            }
            // SIBT_SKIP2 and SIBT_SKIP1: ids with no string.
            0x21 => {
                id = id.wrapping_add(u16_at(package, at + 1)?);
                at += 3;
            }
            0x22 => {
                id = id.wrapping_add(u16::from(*package.get(at + 1)?));
                at += 2;
            }
            // SIBT_EXT1, EXT2, EXT4: extension blocks (fonts), skipped by their length.
            0x30 => at += usize::from(*package.get(at + 2)?).max(1),
            0x31 => at += usize::from(u16_at(package, at + 2)?).max(1),
            0x32 => at += (u32_at(package, at + 2)? as usize).max(1),
            _ => return None,
        }
    }
    Some((language, strings))
}

/// An open scope, and the question it belongs to when it is a question's own.
/// Options and defaults go to the nearest enclosing question: firmware wraps
/// them in suppress-if and gray-out-if scopes inside the question's.
struct Open {
    question: Option<usize>,
}

impl HiiDatabase {
    /// Parses an `ExportPackageLists` blob. Package lists after a damaged one
    /// are dropped, never guessed at.
    pub fn parse(data: &[u8]) -> HiiDatabase {
        let mut db = HiiDatabase { crc32: crc32(data), ..HiiDatabase::default() };
        let mut at = 0usize;
        while at + 20 <= data.len() {
            let Some(length) = u32_at(data, at + 16).map(|l| l as usize) else { break };
            if length < 20 || at + length > data.len() {
                break;
            }
            db.read_package_list(&data[at + 20..at + length]);
            at += length;
        }
        db
    }

    fn read_package_list(&mut self, body: &[u8]) {
        let mut forms = Vec::new();
        let mut strings: Option<HashMap<u16, String>> = None;
        let mut fallback: Option<HashMap<u16, String>> = None;
        let mut at = 0usize;
        while let Some(header) = u32_at(body, at) {
            let (length, kind) = ((header & 0x00FF_FFFF) as usize, (header >> 24) as u8);
            if length < 4 || at + length > body.len() {
                break;
            }
            let package = &body[at..at + length];
            match kind {
                PACKAGE_FORMS => forms.push(package),
                PACKAGE_STRINGS => {
                    if let Some((language, table)) = parse_strings(package) {
                        if strings.is_none() && language.eq_ignore_ascii_case("en-US") {
                            strings = Some(table);
                        } else if fallback.is_none() {
                            fallback = Some(table);
                        }
                    }
                }
                PACKAGE_END => break,
                _ => {}
            }
            at += length;
        }
        let strings = strings.or(fallback).unwrap_or_default();
        for package in forms {
            self.read_forms(&package[4..], &strings);
        }
    }

    fn read_forms(&mut self, ifr: &[u8], strings: &HashMap<u16, String>) {
        let text = |id: u16| strings.get(&id).map(|s| clean(s)).unwrap_or_default();
        // Varstore ids are only unique within a form set.
        let mut stores: HashMap<u16, usize> = HashMap::new();
        let mut scopes: Vec<Open> = Vec::new();
        let mut at = 0usize;
        while at + 2 <= ifr.len() {
            let opcode = ifr[at];
            let length = usize::from(ifr[at + 1] & 0x7F);
            let scoped = ifr[at + 1] & 0x80 != 0;
            if length < 2 || at + length > ifr.len() {
                break;
            }
            let op = &ifr[at..at + length];
            let mut opened = None;
            match opcode {
                IFR_FORM_SET => stores.clear(),
                IFR_VARSTORE if length >= 22 => {
                    // Guid, VarStoreId, Size, Name.
                    if let (Some(guid), Some(id), Some(size)) = (Guid::from_slice(&op[2..]), u16_at(op, 18), u16_at(op, 20)) {
                        let name = String::from_utf8_lossy(op[22..].split(|&b| b == 0).next().unwrap_or(&[])).into_owned();
                        stores.insert(id, self.add_store(VarStore { name, guid, size }));
                    }
                }
                IFR_VARSTORE_EFI if length >= 26 => {
                    // VarStoreId, Guid, Attributes, Size, Name.
                    if let (Some(id), Some(guid), Some(size)) = (u16_at(op, 2), Guid::from_slice(&op[4..]), u16_at(op, 24)) {
                        let name = String::from_utf8_lossy(op[26..].split(|&b| b == 0).next().unwrap_or(&[])).into_owned();
                        stores.insert(id, self.add_store(VarStore { name, guid, size }));
                    }
                }
                IFR_ONE_OF | IFR_NUMERIC | IFR_CHECKBOX if length >= 13 => {
                    opened = self.read_question(opcode, op, &stores, &text);
                }
                IFR_ONE_OF_OPTION if length >= 6 => {
                    let (label, flags, kind) = (u16_at(op, 2).unwrap_or(0), op[4], op[5]);
                    let value = type_size(kind).and_then(|size| uint_at(op, 6, size));
                    if let (Some(index), Some(value)) = (scopes.iter().rev().find_map(|s| s.question), value) {
                        if let QuestionKind::OneOf { options } = &mut self.questions[index].kind {
                            options.push(OneOfOption { value, label: text(label), default: flags & OPTION_DEFAULT != 0 });
                        }
                    }
                }
                IFR_DEFAULT if length >= 6 => {
                    let standard = u16_at(op, 2) == Some(DEFAULT_STANDARD);
                    let value = type_size(op[4]).and_then(|size| uint_at(op, 5, size));
                    if let (Some(index), Some(value), true) = (scopes.iter().rev().find_map(|s| s.question), value, standard) {
                        match &mut self.questions[index].kind {
                            QuestionKind::Numeric { default, .. } => *default = Some(value),
                            QuestionKind::CheckBox { default } => *default = value != 0,
                            QuestionKind::OneOf { options } => {
                                for option in options.iter_mut() {
                                    option.default = option.value == value;
                                }
                            }
                        }
                    }
                }
                IFR_END => {
                    scopes.pop();
                }
                _ => {}
            }
            if scoped {
                scopes.push(Open { question: opened });
            }
            at += length;
        }
    }

    fn add_store(&mut self, store: VarStore) -> usize {
        if let Some(index) = self.varstores.iter().position(|s| *s == store) {
            return index;
        }
        self.varstores.push(store);
        self.varstores.len() - 1
    }

    /// The question header: Prompt, Help, QuestionId, VarStoreId, VarOffset,
    /// QuestionFlags, then the opcode's own flags and range.
    fn read_question(
        &mut self,
        opcode: u8,
        op: &[u8],
        stores: &HashMap<u16, usize>,
        text: &impl Fn(u16) -> String,
    ) -> Option<usize> {
        let (prompt, help, token, store, offset) = (u16_at(op, 2)?, u16_at(op, 4)?, u16_at(op, 6)?, u16_at(op, 8)?, u16_at(op, 10)?);
        let varstore = *stores.get(&store)?;
        let flags = *op.get(13)?;
        let (width, kind) = match opcode {
            IFR_CHECKBOX => (1u8, QuestionKind::CheckBox { default: flags & CHECKBOX_DEFAULT != 0 }),
            _ => {
                let size = 1usize << (flags & 0x03);
                let kind = if opcode == IFR_ONE_OF {
                    QuestionKind::OneOf { options: Vec::new() }
                } else {
                    let (min, max, step) =
                        (uint_at(op, 14, size).unwrap_or(0), uint_at(op, 14 + size, size).unwrap_or(u64::MAX), uint_at(op, 14 + 2 * size, size).unwrap_or(0));
                    QuestionKind::Numeric { min, max, step, default: None }
                };
                (size as u8, kind)
            }
        };
        if usize::from(offset) + usize::from(width) > usize::from(self.varstores[varstore].size) {
            return None;
        }
        self.questions.push(Question { token, prompt: text(prompt), help: text(help), varstore, offset, width, kind });
        Some(self.questions.len() - 1)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds databases for tests: one package list with a string package and a
    /// forms package.
    pub struct Builder {
        strings: Vec<String>,
        ifr: Vec<u8>,
    }

    impl Builder {
        pub fn new() -> Self {
            Builder { strings: Vec::new(), ifr: Vec::new() }
        }

        pub fn string(&mut self, text: &str) -> u16 {
            self.strings.push(text.into());
            self.strings.len() as u16
        }

        fn op(&mut self, opcode: u8, scoped: bool, body: &[u8]) {
            self.ifr.push(opcode);
            self.ifr.push((body.len() + 2) as u8 | if scoped { 0x80 } else { 0 });
            self.ifr.extend_from_slice(body);
        }

        pub fn form_set(&mut self) {
            self.op(IFR_FORM_SET, true, &[0; 20]);
        }

        pub fn end(&mut self) {
            self.op(IFR_END, false, &[]);
        }

        pub fn varstore(&mut self, id: u16, guid: [u8; 16], size: u16, name: &str) {
            let mut body = guid.to_vec();
            body.extend(id.to_le_bytes());
            body.extend(size.to_le_bytes());
            body.extend(name.as_bytes());
            body.push(0);
            self.op(IFR_VARSTORE, false, &body);
        }

        fn header(&mut self, prompt: &str, token: u16, store: u16, offset: u16) -> Vec<u8> {
            let (p, h) = (self.string(prompt), self.string(&format!("Help for {prompt}.")));
            let mut body = Vec::new();
            for v in [p, h, token, store, offset] {
                body.extend(v.to_le_bytes());
            }
            body.push(0);
            body
        }

        pub fn one_of(&mut self, prompt: &str, token: u16, store: u16, offset: u16, options: &[(u8, &str, bool)]) {
            let mut body = self.header(prompt, token, store, offset);
            body.extend([0x00, 0, 255, 1]);
            self.op(IFR_ONE_OF, true, &body);
            for &(value, label, default) in options {
                let id = self.string(label);
                let mut option = id.to_le_bytes().to_vec();
                option.extend([if default { OPTION_DEFAULT } else { 0 }, 0, value]);
                self.op(IFR_ONE_OF_OPTION, false, &option);
            }
            self.end();
        }

        pub fn numeric16(&mut self, prompt: &str, token: u16, store: u16, offset: u16, range: (u16, u16), default: u16) {
            let mut body = self.header(prompt, token, store, offset);
            body.push(0x01);
            for v in [range.0, range.1, 1] {
                body.extend(v.to_le_bytes());
            }
            self.op(IFR_NUMERIC, true, &body);
            let mut def = DEFAULT_STANDARD.to_le_bytes().to_vec();
            def.push(1);
            def.extend(default.to_le_bytes());
            self.op(IFR_DEFAULT, false, &def);
            self.end();
        }

        /// A one-of whose second option sits inside a suppress-if scope, as firmware writes them.
        pub fn one_of_nested(&mut self, prompt: &str, token: u16, store: u16, offset: u16) {
            let mut body = self.header(prompt, token, store, offset);
            body.extend([0x00, 0, 255, 1]);
            self.op(IFR_ONE_OF, true, &body);
            let first = self.string("Disabled");
            self.op(IFR_ONE_OF_OPTION, false, &[first.to_le_bytes()[0], first.to_le_bytes()[1], OPTION_DEFAULT, 0, 0]);
            self.op(0x0A, true, &[]);
            self.op(0x12, false, &[0]);
            let second = self.string("Profile1");
            self.op(IFR_ONE_OF_OPTION, false, &[second.to_le_bytes()[0], second.to_le_bytes()[1], 0, 0, 1]);
            self.end();
            self.end();
        }

        pub fn checkbox(&mut self, prompt: &str, token: u16, store: u16, offset: u16, default: bool) {
            let mut body = self.header(prompt, token, store, offset);
            body.push(u8::from(default));
            self.op(IFR_CHECKBOX, false, &body);
        }

        pub fn build(&self) -> Vec<u8> {
            let mut strings_pkg = vec![0u8; 4];
            let header_size = 46 + 6;
            strings_pkg.extend((header_size as u32).to_le_bytes());
            strings_pkg.extend((header_size as u32).to_le_bytes());
            strings_pkg.resize(46, 0);
            strings_pkg.extend(b"en-US\0");
            strings_pkg.push(0x16);
            strings_pkg.extend((self.strings.len() as u16).to_le_bytes());
            for s in &self.strings {
                for unit in s.encode_utf16() {
                    strings_pkg.extend(unit.to_le_bytes());
                }
                strings_pkg.extend([0, 0]);
            }
            strings_pkg.push(0);
            let header = (strings_pkg.len() as u32) | u32::from(PACKAGE_STRINGS) << 24;
            strings_pkg[..4].copy_from_slice(&header.to_le_bytes());

            let mut forms_pkg = (((self.ifr.len() + 4) as u32) | u32::from(PACKAGE_FORMS) << 24).to_le_bytes().to_vec();
            forms_pkg.extend(&self.ifr);
            let end = (4u32 | u32::from(PACKAGE_END) << 24).to_le_bytes();

            let mut list = vec![0xAB; 16];
            let length = 20 + forms_pkg.len() + strings_pkg.len() + end.len();
            list.extend((length as u32).to_le_bytes());
            list.extend(forms_pkg);
            list.extend(strings_pkg);
            list.extend(end);
            list
        }
    }

    pub const SETUP_GUID: [u8; 16] = [0x43, 0xd6, 0x87, 0xec, 0xa4, 0xeb, 0xb5, 0x4b, 0xa1, 0xe5, 0x3f, 0x3e, 0x36, 0xb2, 0x0d, 0xa9];

    pub fn sample() -> Vec<u8> {
        let mut b = Builder::new();
        b.form_set();
        b.varstore(0xF101, SETUP_GUID, 16, "Setup");
        b.one_of("Multi-Theme", 0x12, 0xF101, 0, &[(0, "Theme 1", true), (1, "Theme 2", false)]);
        b.numeric16("CPU Base\u{7f}Clock", 0x19, 0xF101, 2, (8000, 20000), 10000);
        b.checkbox("Fast Boot", 0x20, 0xF101, 4, false);
        // The same storage again under another question: a duplicate.
        b.one_of("Multi-Theme", 0x13, 0xF101, 0, &[(0, "Theme 1", true), (1, "Theme 2", false)]);
        b.one_of_nested("Extreme Memory Profile", 0x40, 0xF101, 5);
        // A question whose varstore is not declared is left out.
        b.checkbox("Orphan", 0x30, 0x0BAD, 0, false);
        b.end();
        b.build()
    }

    #[test]
    fn reads_varstores_questions_options_and_defaults() {
        let db = HiiDatabase::parse(&sample());
        assert_eq!(db.varstores.len(), 1);
        assert_eq!(db.varstores[0].name, "Setup");
        assert_eq!(db.varstores[0].guid.to_string(), "ec87d643-eba4-4bb5-a1e5-3f3e36b20da9");
        assert_eq!(db.questions.len(), 5);
        let QuestionKind::OneOf { options: xmp } = &db.questions[4].kind else { panic!() };
        assert_eq!(xmp.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), ["Disabled", "Profile1"], "options inside a suppress-if belong to the question");
        let theme = &db.questions[0];
        assert_eq!((theme.token, theme.offset, theme.width), (0x12, 0, 1));
        assert_eq!(theme.help, "Help for Multi-Theme.");
        let QuestionKind::OneOf { options } = &theme.kind else { panic!("{theme:?}") };
        assert_eq!(options.iter().map(|o| (o.value, o.label.as_str(), o.default)).collect::<Vec<_>>(), [(0, "Theme 1", true), (1, "Theme 2", false)]);
        let clock = &db.questions[1];
        assert_eq!(clock.prompt, "CPU Base Clock", "0x7F is the firmware's hard space");
        assert_eq!(clock.kind, QuestionKind::Numeric { min: 8000, max: 20000, step: 1, default: Some(10000) });
        assert_eq!(clock.width, 2);
        assert_eq!(db.questions[2].kind, QuestionKind::CheckBox { default: false });
    }

    #[test]
    fn damage_stops_the_walk_instead_of_guessing() {
        let mut data = sample();
        data.truncate(data.len() - 10);
        assert!(HiiDatabase::parse(&data).questions.is_empty());
        assert!(HiiDatabase::parse(&[0xFF; 7]).questions.is_empty());
    }

    #[test]
    fn crc32_matches_the_ieee_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
