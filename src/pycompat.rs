// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! The few places where the exact behaviour of the original Python matters.
//!
//! SCEWIN exports are Latin-1 text, and the files this program writes --
//! profiles, transactions, the catalog, CSV -- are read back by other tools and
//! by earlier releases. Line splitting, whitespace, integer parsing, JSON
//! layout and CSV quoting therefore follow what the Python implementation did
//! rather than what Rust does by default.

use std::io;

use serde::Serialize;

/// Whitespace as Python's `str.isspace` defines it.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}

/// `str.strip()`.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `str.split()` with no separator: runs of whitespace, no empty parts.
pub fn split_whitespace(text: &str) -> impl Iterator<Item = &str> {
    text.split(is_space).filter(|part| !part.is_empty())
}

/// `" ".join(text.split())`.
pub fn collapse_whitespace(text: &str) -> String {
    split_whitespace(text).collect::<Vec<_>>().join(" ")
}

/// `str.splitlines()`: every Unicode line boundary Python recognises, `\r\n`
/// as one, and no empty element after a final line break.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        let boundary = matches!(
            c,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}'
                | '\u{2028}' | '\u{2029}'
        );
        if !boundary {
            continue;
        }
        lines.push(&text[start..index]);
        let mut end = index + c.len_utf8();
        if c == '\r' {
            if let Some(&(next_index, '\n')) = chars.peek() {
                chars.next();
                end = next_index + 1;
            }
        }
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Decode bytes as Latin-1: every byte is the code point of the same value.
pub fn decode_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&byte| byte as char).collect()
}

/// Encode text as Latin-1, or return the first character that has no byte.
pub fn encode_latin1(text: &str) -> Result<Vec<u8>, char> {
    text.chars()
        .map(|c| u8::try_from(u32::from(c)).map_err(|_| c))
        .collect()
}

/// `repr()` of a string, for error messages.
pub fn repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (0x7f..=0xa0).contains(&(c as u32)) || c == '\u{ad}' => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Digits with Python's single-underscore separators, in the given base.
fn parse_digits(digits: &str, radix: u32, allow_leading_underscore: bool) -> Option<u128> {
    if digits.is_empty() {
        return None;
    }
    let mut value: u128 = 0;
    let mut previous_underscore = !allow_leading_underscore;
    let mut seen_digit = false;
    let mut overflow = false;
    for c in digits.chars() {
        if c == '_' {
            if previous_underscore {
                return None;
            }
            previous_underscore = true;
            continue;
        }
        let digit = c.to_digit(radix)?;
        previous_underscore = false;
        seen_digit = true;
        match value.checked_mul(u128::from(radix)).and_then(|v| v.checked_add(u128::from(digit))) {
            Some(next) => value = next,
            None => overflow = true,
        }
    }
    if previous_underscore || !seen_digit {
        return None;
    }
    // Too large for u128 is still a valid number; saturate so range checks
    // report it as exceeding the maximum.
    Some(if overflow { u128::MAX } else { value })
}

/// `int(text, 16)`: optional sign, optional `0x`, underscores between digits.
pub fn int_base16(text: &str) -> Option<i128> {
    let text = strip(text);
    let (negative, body) = split_sign(text);
    let (body, after_prefix) = match body.get(..2) {
        Some(prefix) if prefix.eq_ignore_ascii_case("0x") => (&body[2..], true),
        _ => (body, false),
    };
    let magnitude = parse_digits(body, 16, after_prefix)?;
    signed(negative, magnitude)
}

/// `int(text, 0)`: base from the prefix, and no leading zeros on a decimal.
pub fn int_base0(text: &str) -> Option<i128> {
    let text = strip(text);
    let (negative, body) = split_sign(text);
    let lower = body.to_ascii_lowercase();
    let magnitude = if let Some(rest) = lower.strip_prefix("0x") {
        parse_digits(rest, 16, true)?
    } else if let Some(rest) = lower.strip_prefix("0o") {
        parse_digits(rest, 8, true)?
    } else if let Some(rest) = lower.strip_prefix("0b") {
        parse_digits(rest, 2, true)?
    } else {
        let value = parse_digits(body, 10, false)?;
        if body.starts_with('0') && value != 0 {
            return None;
        }
        value
    };
    signed(negative, magnitude)
}

fn split_sign(text: &str) -> (bool, &str) {
    if let Some(rest) = text.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = text.strip_prefix('+') {
        (false, rest)
    } else {
        (false, text)
    }
}

fn signed(negative: bool, magnitude: u128) -> Option<i128> {
    let value = i128::try_from(magnitude).unwrap_or(i128::MAX);
    Some(if negative { -value } else { value })
}

// ----- JSON ---------------------------------------------------------------

/// How `json.dumps` lays a document out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonStyle {
    /// `json.dumps(value)`: one line, `", "` and `": "` separators.
    Line,
    /// `json.dumps(value, indent=2)`.
    Indent2,
}

struct PyFormatter {
    style: JsonStyle,
    ensure_ascii: bool,
    depth: usize,
    has_value: bool,
}

impl PyFormatter {
    fn newline<W: ?Sized + io::Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(b"\n")?;
        for _ in 0..self.depth {
            writer.write_all(b"  ")?;
        }
        Ok(())
    }
}

impl serde_json::ser::Formatter for PyFormatter {
    fn begin_array<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.depth += 1;
        self.has_value = false;
        writer.write_all(b"[")
    }

    fn end_array<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.depth -= 1;
        if self.has_value && self.style == JsonStyle::Indent2 {
            self.newline(writer)?;
        }
        self.has_value = true;
        writer.write_all(b"]")
    }

    fn begin_array_value<W: ?Sized + io::Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        match self.style {
            JsonStyle::Line => {
                if !first {
                    writer.write_all(b", ")?;
                }
                Ok(())
            }
            JsonStyle::Indent2 => {
                if !first {
                    writer.write_all(b",")?;
                }
                self.newline(writer)
            }
        }
    }

    fn end_array_value<W: ?Sized + io::Write>(&mut self, _writer: &mut W) -> io::Result<()> {
        self.has_value = true;
        Ok(())
    }

    fn begin_object<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.depth += 1;
        self.has_value = false;
        writer.write_all(b"{")
    }

    fn end_object<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.depth -= 1;
        if self.has_value && self.style == JsonStyle::Indent2 {
            self.newline(writer)?;
        }
        self.has_value = true;
        writer.write_all(b"}")
    }

    fn begin_object_key<W: ?Sized + io::Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        self.begin_array_value(writer, first)
    }

    fn begin_object_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        writer.write_all(b": ")
    }

    fn end_object_value<W: ?Sized + io::Write>(&mut self, _writer: &mut W) -> io::Result<()> {
        self.has_value = true;
        Ok(())
    }

    fn write_string_fragment<W: ?Sized + io::Write>(&mut self, writer: &mut W, fragment: &str) -> io::Result<()> {
        if !self.ensure_ascii {
            return writer.write_all(fragment.as_bytes());
        }
        let mut units = [0u16; 2];
        for c in fragment.chars() {
            if c.is_ascii() {
                writer.write_all(&[c as u8])?;
            } else {
                for unit in c.encode_utf16(&mut units) {
                    write!(writer, "\\u{unit:04x}")?;
                }
            }
        }
        Ok(())
    }
}

/// Serialise the way Python's `json.dumps` would.
pub fn to_json<T: Serialize + ?Sized>(value: &T, style: JsonStyle, ensure_ascii: bool) -> String {
    let mut out = Vec::new();
    let formatter = PyFormatter { style, ensure_ascii, depth: 0, has_value: false };
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    value.serialize(&mut serializer).expect("serialising to memory cannot fail");
    String::from_utf8(out).expect("JSON output is UTF-8")
}

/// Text as Python's `Path.write_text` puts it on disk: `\n` becomes the
/// platform line ending. Profiles and transactions are written this way, so a
/// file from this build and one from the original are byte-for-byte equal.
pub fn text_mode(text: &str) -> String {
    if cfg!(windows) {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

// ----- CSV ----------------------------------------------------------------

/// A row as Python's `csv.writer` (excel dialect) writes it, `\r\n` included.
pub fn csv_row<S: AsRef<str>>(fields: &[S]) -> String {
    let mut line = String::new();
    for (index, field) in fields.iter().enumerate() {
        let field = field.as_ref();
        if index > 0 {
            line.push(',');
        }
        let quote = field.contains([',', '"', '\r', '\n']) || (fields.len() == 1 && field.is_empty());
        if quote {
            line.push('"');
            line.push_str(&field.replace('"', "\"\""));
            line.push('"');
        } else {
            line.push_str(field);
        }
    }
    line.push_str("\r\n");
    line
}

/// UTF-8 with a byte-order mark, which is what Excel needs to read UTF-8 CSV.
pub fn utf8_sig(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() + 3);
    bytes.extend_from_slice(b"\xEF\xBB\xBF");
    bytes.extend_from_slice(text.as_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(splitlines("a\r\nb\rc\n"), vec!["a", "b", "c"]);
        assert_eq!(splitlines("a\r\r\nb"), vec!["a", "", "b"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("\n"), vec![""]);
        assert_eq!(splitlines("x\u{85}y"), vec!["x", "y"]);
    }

    #[test]
    fn integers_match_python() {
        assert_eq!(int_base0("0x37"), Some(55));
        assert_eq!(int_base0("10"), Some(10));
        assert_eq!(int_base0("00"), Some(0));
        assert_eq!(int_base0("010"), None);
        assert_eq!(int_base0("1_000"), Some(1000));
        assert_eq!(int_base0("1__0"), None);
        assert_eq!(int_base0("0x_1f"), Some(31));
        assert_eq!(int_base0("-5"), Some(-5));
        assert_eq!(int_base0("abc"), None);
        assert_eq!(int_base16("0400"), Some(1024));
        assert_eq!(int_base16("0X10"), Some(16));
        assert_eq!(int_base16(""), None);
    }

    #[test]
    fn json_layout_matches_python() {
        let value = json!({"a": 1, "b": [1, 2], "c": [], "d": {}, "e": "caf\u{e9}"});
        assert_eq!(
            to_json(&value, JsonStyle::Line, true),
            concat!(r#"{"a": 1, "b": [1, 2], "c": [], "d": {}, "e": "caf"#, "\\", r#"u00e9"}"#)
        );
        assert_eq!(
            to_json(&value, JsonStyle::Indent2, false),
            "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2\n  ],\n  \"c\": [],\n  \"d\": {},\n  \"e\": \"caf\u{e9}\"\n}"
        );
    }

    #[test]
    fn csv_quotes_like_python() {
        assert_eq!(csv_row(&["a", "b,c", "say \"hi\"", ""]), "a,\"b,c\",\"say \"\"hi\"\"\",\r\n");
    }
}
