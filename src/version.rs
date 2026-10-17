// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! The product name, and the version read from Cargo.toml.
//!
//! The name titles the window and the version is stamped into the executable's
//! file properties by build.rs, so a build can be identified from the binary
//! alone. Nothing else in the sources writes a version; tests/version.rs checks
//! the changelog and the README against this one.

pub const APP_NAME: &str = "Roch NVRAM";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Windows file-version resources want four numbers.
pub fn version_tuple() -> [u32; 4] {
    let mut parts = [0u32; 4];
    for (slot, part) in parts.iter_mut().zip(VERSION.split('.')) {
        *slot = part.parse().unwrap_or(0);
    }
    parts
}
