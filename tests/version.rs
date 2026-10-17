// One version, written once and agreed everywhere it appears.
//
// Cargo.toml is the only place it is written. The window title and the
// executable's file properties read from it directly; the changelog and the
// README cannot, so they are checked against it here.

use std::fs;
use std::path::Path;

use regex::Regex;
use roch_nvram::version::{version_tuple, APP_NAME, VERSION};

fn read(name: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(name)).unwrap()
}

#[test]
fn it_is_three_numbers() {
    assert!(Regex::new(r"^\d+\.\d+\.\d+$").unwrap().is_match(VERSION));
}

#[test]
fn the_windows_tuple_is_the_version_plus_a_build_field() {
    let parts: Vec<u32> = VERSION.split('.').map(|part| part.parse().unwrap()).collect();
    assert_eq!(version_tuple(), [parts[0], parts[1], parts[2], 0]);
}

#[test]
fn nothing_else_declares_a_version() {
    // The point of reading it from Cargo.toml: fail rather than let a second
    // literal drift away from it.
    let pattern = Regex::new(r#"(?m)^\s*(pub )?const VERSION\s*:\s*&str\s*=\s*""#).unwrap();
    let mut stack = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                assert!(!pattern.is_match(&fs::read_to_string(&path).unwrap()), "{}", path.display());
            }
        }
    }
}

#[test]
fn the_build_script_stamps_the_resource_from_cargo() {
    let build = read("build.rs");
    assert!(build.contains("CARGO_PKG_VERSION"));
    assert!(build.contains("VERSIONINFO"));
}

#[test]
fn the_changelog_leads_with_this_version() {
    let changelog = read("CHANGELOG.md");
    let first = Regex::new(r"(?m)^## (\S+)").unwrap().captures(&changelog).expect("a version heading");
    assert_eq!(&first[1], VERSION);
}

#[test]
fn the_readme_names_this_version() {
    assert!(read("README.md").contains(&format!("# {APP_NAME} {VERSION}")));
}
