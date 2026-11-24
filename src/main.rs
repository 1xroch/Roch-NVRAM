// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

// No console window behind the editor in a release build.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gui;

use std::path::PathBuf;
use std::process::ExitCode;

use roch_nvram::paths::base_dir;

const USAGE: &str = "usage: roch-nvram [--offline] [--profile PROFILE] [--nvram NVRAM] [--no-elevate]

  --offline      Open the sample export without running or importing through SCEWIN.
  --profile      Parsed profile JSON for --offline (default: data/z890_tachyon_nvram_parsed.json).
  --nvram        SCEWIN export for --offline (default: data/z890_tachyon_nvram.txt).
  --no-elevate   Do not ask for administrator rights at launch.
  --export FILE  Read the live NVRAM into FILE (AMISCE layout) without opening the window.

Linux:
  roch-nvram setup [--status | --show | --undo]
                 Stop asking for the password (run it with sudo).";

struct Args {
    export: Option<PathBuf>,
    offline: bool,
    no_elevate: bool,
    profile: PathBuf,
    nvram: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let base = base_dir();
    let mut args = Args {
        export: None,
        offline: false,
        no_elevate: false,
        profile: base.join("data").join("z890_tachyon_nvram_parsed.json"),
        nvram: base.join("data").join("z890_tachyon_nvram.txt"),
    };
    let mut iter = std::env::args_os().skip(1);
    while let Some(arg) = iter.next() {
        let text = arg.to_string_lossy().into_owned();
        let (flag, inline) = match text.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag.to_string(), Some(PathBuf::from(value))),
            _ => (text.clone(), None),
        };
        let mut value = |name: &str| inline.clone().or_else(|| iter.next().map(PathBuf::from)).ok_or(format!("{name} expects a path"));
        match flag.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "--offline" => args.offline = true,
            "--export" => args.export = Some(value("--export")?),
            "--no-elevate" => args.no_elevate = true,
            "--profile" => args.profile = value("--profile")?,
            "--nvram" => args.nvram = value("--nvram")?,
            _ => return Err(format!("unrecognized argument: {text}")),
        }
    }
    Ok(args)
}

/// Ask Windows to start this program again with administrator rights.
///
/// SCEWIN needs those rights the moment Export or Import is pressed, and a
/// running process cannot elevate itself, so the prompt comes at launch --
/// the way run.bat did it. Offline mode never asks. Returns true when the
/// elevated copy was started and this one should exit; when the prompt is
/// declined the window opens anyway, and Export explains what is missing.
/// Only live mode elevates, and live mode takes no other arguments, so none
/// are forwarded.
#[cfg(windows)]
fn relaunch_elevated() -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    if roch_nvram::backend::is_windows_admin() {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    // The elevated copy must not ask again if elevation silently failed.
    let parameters = "--no-elevate";
    let wide = |text: &std::ffi::OsStr| text.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let verb = wide("runas".as_ref());
    let file = wide(exe.as_os_str());
    let params = wide(parameters.as_ref());
    let directory = std::env::current_dir().map(|dir| wide(dir.as_os_str())).unwrap_or_else(|_| vec![0]);
    // SAFETY: every pointer is a NUL-terminated UTF-16 buffer that outlives
    // the call; a null window handle is allowed.
    let result = unsafe {
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), params.as_ptr(), directory.as_ptr(), SW_SHOWNORMAL)
    };
    // Values above 32 mean the elevated process was started.
    result as isize > 32
}

#[cfg(not(windows))]
fn relaunch_elevated() -> bool {
    false
}

/// `--export FILE`: one live export, copied to FILE, with no window.
fn export_to(target: &std::path::Path) -> ExitCode {
    let backend = roch_nvram::backend::ScewinBackend::new(base_dir());
    match backend.export_current("cli").and_then(|export| {
        std::fs::copy(&export.nvram_path, target)
            .map_err(|error| roch_nvram::Error::execution(format!("{}: {error}", target.display())))
            .map(|_| export)
    }) {
        Ok(export) => {
            println!("{}", export.run_result.output.trim());
            println!("{} settings written to {}", export.profile.settings.len(), target.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    // Linux: the root helpers and `setup`, which print to the terminal instead of opening the window.
    #[cfg(target_os = "linux")]
    {
        let raw: Vec<String> = std::env::args().skip(1).collect();
        if let Some(code) = roch_nvram::linux::run_helper_command(&raw) {
            return ExitCode::from(u8::try_from(code).unwrap_or(1));
        }
    }
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            let text = if message.is_empty() { USAGE.to_string() } else { format!("{message}\n\n{USAGE}") };
            eprintln!("{text}");
            rfd::MessageDialog::new()
                .set_title(roch_nvram::APP_NAME)
                .set_description(&text)
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
            return ExitCode::from(2);
        }
    };

    if let Some(target) = args.export {
        return export_to(&target);
    }

    if !args.offline && !args.no_elevate && relaunch_elevated() {
        return ExitCode::SUCCESS;
    }

    gui::run(gui::Options {
        live: !args.offline,
        profile: args.profile,
        nvram: args.nvram,
        project_root: base_dir(),
    })
}
