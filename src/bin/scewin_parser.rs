// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! Parse AMI SCEWIN/AMISCE nvram.txt exports without executing SCEWIN.
//!
//! ```text
//! scewin-parser nvram.txt --dupes Dupes.txt --label "Z890 Tachyon / 270K" \
//!     --output parsed.json --summary summary.json
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use roch_nvram::parser::{make_summary, parse_sce_file, ProfileDocument, ProfileSource};
use roch_nvram::pycompat::{text_mode, to_json, JsonStyle};
use serde_json::json;

const USAGE: &str = "usage: scewin-parser NVRAM [--dupes DUPES] [--label LABEL] [--output OUTPUT] [--summary SUMMARY]";

struct Args {
    nvram: PathBuf,
    dupes: Option<PathBuf>,
    label: String,
    output: PathBuf,
    summary: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut nvram = None;
    let mut dupes = None;
    let mut label = String::new();
    let mut output = PathBuf::from("parsed.json");
    let mut summary = PathBuf::from("summary.json");
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy().into_owned();
        let (flag, inline) = match text.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag.to_string(), Some(value.to_string())),
            _ => (text.clone(), None),
        };
        let mut value = |name: &str| -> Result<std::ffi::OsString, String> {
            inline.clone().map(Into::into).or_else(|| args.next()).ok_or(format!("{name} expects a value"))
        };
        match flag.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "--dupes" => dupes = Some(PathBuf::from(value("--dupes")?)),
            "--label" => label = value("--label")?.to_string_lossy().into_owned(),
            "--output" => output = PathBuf::from(value("--output")?),
            "--summary" => summary = PathBuf::from(value("--summary")?),
            _ if flag.starts_with("--") => return Err(format!("unrecognized argument: {flag}")),
            _ if nvram.is_none() => nvram = Some(PathBuf::from(arg)),
            _ => return Err(format!("unrecognized argument: {text}")),
        }
    }
    let nvram = nvram.ok_or("the following arguments are required: nvram")?;
    Ok(Args { nvram, dupes, label, output, summary })
}

fn run(args: Args) -> Result<(), String> {
    let active = parse_sce_file(&args.nvram, "nvram", false).map_err(|e| format!("{}: {e}", args.nvram.display()))?;
    let dupes = match &args.dupes {
        Some(path) => Some(parse_sce_file(path, "dupes", true).map_err(|e| format!("{}: {e}", path.display()))?),
        None => None,
    };
    let dupe_settings = dupes.as_ref().map_or(&[][..], |parsed| &parsed.settings);
    let source = ProfileSource {
        nvram_path: Some(args.nvram.display().to_string()),
        nvram_sha256: Some(active.sha256.clone()),
        dupes_path: args.dupes.as_ref().map(|path| path.display().to_string()),
        dupes_sha256: dupes.as_ref().map(|parsed| parsed.sha256.clone()),
    };
    let output = ProfileDocument {
        schema_version: 1,
        label: &args.label,
        source: &source,
        metadata: &active.metadata,
        active_settings: &active.settings,
        excluded_duplicates: dupe_settings,
    };
    let mut summary = make_summary(&active.settings, dupe_settings, &active.metadata);
    summary.insert("label".into(), json!(args.label));
    summary.insert("source".into(), json!(source));

    let write = |path: &PathBuf, text: String| std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()));
    write(&args.output, text_mode(&to_json(&output, JsonStyle::Indent2, false)))?;
    write(&args.summary, text_mode(&to_json(&summary, JsonStyle::Indent2, false)))?;
    println!("{}", to_json(&summary, JsonStyle::Indent2, true));
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
            eprintln!("{USAGE}\nscewin-parser: error: {message}");
            return ExitCode::from(2);
        }
    };
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("scewin-parser: {message}");
            ExitCode::FAILURE
        }
    }
}
