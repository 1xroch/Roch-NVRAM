// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! Where things live on disk.

use std::env;
use std::path::{Component, Path, PathBuf};

/// `%LOCALAPPDATA%\NVRAM`, or `~/.nvram` where that is not set. Backups, the
/// SCEWIN runtime copy, the catalog and the activity log all live under it.
pub fn default_app_root() -> PathBuf {
    if let Some(local) = env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty()) {
        return PathBuf::from(local).join("NVRAM");
    }
    home_dir().join(".nvram")
}

fn home_dir() -> PathBuf {
    for name in ["USERPROFILE", "HOME"] {
        if let Some(value) = env::var_os(name).filter(|value| !value.is_empty()) {
            return PathBuf::from(value);
        }
    }
    PathBuf::from(".")
}

/// The directory the executable is in.
pub fn exe_dir() -> PathBuf {
    env::current_exe()
        .ok()
        .and_then(|exe| exe.canonicalize().ok().or(Some(exe)))
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The directory holding `scewin/` and `data/`.
///
/// A packaged build keeps them beside the executable. Under `cargo run` the
/// executable is in `target/`, so the source tree is used instead.
pub fn base_dir() -> PathBuf {
    let beside = exe_dir();
    if beside.join("scewin").is_dir() {
        return beside;
    }
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if source.join("scewin").is_dir() {
        return source;
    }
    beside
}

/// The `assets/` directory, beside the executable or in the source tree.
/// Returns `None` when neither exists; the shipped presets and icon are also
/// compiled into the executable, so a lone .exe still has them.
pub fn asset_root() -> Option<PathBuf> {
    let beside = exe_dir().join("assets");
    if beside.is_dir() {
        return Some(beside);
    }
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    source.is_dir().then_some(source)
}

/// `Path.resolve()`: absolute, with every existing component canonicalised and
/// the rest appended, so a file that does not exist yet still resolves.
pub fn resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir().unwrap_or_default().join(path)
    };
    let mut existing = absolute.clone();
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            let mut out = strip_verbatim(real);
            for part in rest.iter().rev() {
                out.push(part);
            }
            return normalise(&out);
        }
        match (existing.file_name().map(|name| name.to_os_string()), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => return normalise(&absolute),
        }
    }
}

/// Whether two paths name the same file, the way Python compares resolved
/// paths: case-insensitively on Windows.
pub fn same_file(a: &Path, b: &Path) -> bool {
    let (a, b) = (resolve(a), resolve(b));
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `canonicalize` on Windows returns `\\?\C:\...`; Python shows `C:\...`.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}
