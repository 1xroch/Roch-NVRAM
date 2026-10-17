// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! Stamps the icon and the version from Cargo.toml into roch-nvram.exe.
//!
//! The resource is generated here rather than committed, so the file properties
//! of a build can never carry a version other than the one in Cargo.toml. It is
//! linked into the GUI executable only: the test harnesses and the command-line
//! tools get nothing.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/roch_nvram.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let mut numbers: Vec<u32> = version
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect();
    numbers.resize(4, 0);
    let commas = numbers.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let dotted = numbers.iter().map(u32::to_string).collect::<Vec<_>>().join(".");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest_dir.join("assets").join("roch_nvram.ico");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let rc = out.join("roch_nvram.rc");
    let rc_text = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {commas}
PRODUCTVERSION {commas}
FILEFLAGSMASK 0x3f
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Roch Studio"
      VALUE "FileDescription", "Roch NVRAM"
      VALUE "FileVersion", "{dotted}"
      VALUE "InternalName", "RochNVRAM"
      VALUE "OriginalFilename", "roch-nvram.exe"
      VALUE "ProductName", "Roch NVRAM"
      VALUE "ProductVersion", "{dotted}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = icon.display().to_string().replace('\\', "\\\\"),
    );
    fs::write(&rc, rc_text).expect("write the resource script");

    // A missing resource compiler costs the icon and the file properties, not
    // the build.
    if let Err(error) = embed_resource::compile_for(&rc, ["roch-nvram"], embed_resource::NONE)
        .manifest_optional()
    {
        println!("cargo:warning=Windows resources were not embedded: {error}");
    }
}
