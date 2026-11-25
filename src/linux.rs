// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! Linux: SCEWIN's job done natively.
//!
//! - **The HII database** is where the firmware's `HiiDB` variable says, in
//!   memory the firmware reserved. Reading `/dev/mem` needs root, so a small
//!   helper (this program with [`HII_DUMP_ARG`], started through `pkexec`)
//!   copies it out, and it is kept per BIOS build: the prompt comes once, and
//!   again after a BIOS update.
//! - **The current values** are the UEFI variables in efivarfs, which any user
//!   may read, so exporting needs nothing more.
//! - **Writing** needs root. The helper started with [`IMPORT_ARG`] reads the
//!   edited export from its input, reads the HII database itself, and changes
//!   only the bytes [`crate::sce::plan_import`] allows: existing variables,
//!   same size, same attributes.
//!
//! `roch-nvram setup` (see [`setup`]) installs a root-owned copy and a polkit
//! rule so that those two helpers start without a password.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::hii::{HiiDatabase, VarStore};
use crate::parser::sha256_bytes;
use crate::sce::{self, Variables};

pub const EFIVARS: &str = "/sys/firmware/efi/efivars";
const HIIDB_VARIABLE: &str = "HiiDB-1b838190-4625-4ead-abc9-cd5e6af18fe0";
/// The helper that copies the HII database out of firmware memory.
pub const HII_DUMP_ARG: &str = "--hii-dump";
/// The helper that writes an edited export's changes.
pub const IMPORT_ARG: &str = "--sce-import";
/// Largest database believed: AMI's run to a few megabytes.
const HII_MAX: usize = 64 << 20;

/// The variables in efivarfs.
pub struct EfiVars;

fn variable_path(store: &VarStore) -> PathBuf {
    Path::new(EFIVARS).join(format!("{}-{}", store.name, store.guid))
}

impl Variables for EfiVars {
    fn read(&self, store: &VarStore) -> Option<Vec<u8>> {
        let bytes = fs::read(variable_path(store)).ok()?;
        (bytes.len() >= 4).then(|| bytes[4..].to_vec())
    }
}

/// Whether this machine has what the native reader needs.
pub fn check_ready() -> Result<(), String> {
    if !Path::new(EFIVARS).is_dir() {
        return Err("This system was not started through UEFI (no /sys/firmware/efi/efivars), so its BIOS settings cannot be read.".into());
    }
    hii_location().map(|_| ()).map_err(|e| {
        format!("The firmware does not publish its setup database (the HiiDB variable): {e}. Only AMI BIOSes do; on others, use SCEWIN on Windows.")
    })
}

/// Where the firmware put its HII database, and how long it is.
fn hii_location() -> io::Result<(u64, usize)> {
    let bytes = fs::read(Path::new(EFIVARS).join(HIIDB_VARIABLE))?;
    let data = bytes.get(4..).unwrap_or_default();
    let size = data.get(..4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    let address = match data.len() {
        8 => data.get(4..8).map(|b| u64::from(u32::from_le_bytes(b.try_into().unwrap()))),
        12 | 16 => data.get(4..12).map(|b| u64::from_le_bytes(b.try_into().unwrap())),
        _ => None,
    };
    match (address, size) {
        (Some(address), Some(size)) if address != 0 && (1024..=HII_MAX).contains(&size) => Ok((address, size)),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, format!("HiiDB holds {} unexpected bytes", data.len()))),
    }
}

/// Firmware memory, never RAM the kernel hands out: the range must lie in one
/// of the regions the firmware map calls reserved or ACPI NVS.
fn in_firmware_memory(address: u64, size: usize) -> bool {
    let read = |path: PathBuf| fs::read_to_string(path).ok().map(|t| t.trim().to_string());
    let number = |text: Option<String>| text.and_then(|t| u64::from_str_radix(t.trim_start_matches("0x"), 16).ok());
    let Some(last) = (size as u64).checked_sub(1).and_then(|span| address.checked_add(span)) else { return false };
    let Ok(entries) = fs::read_dir("/sys/firmware/memmap") else { return false };
    entries.flatten().any(|entry| {
        let (start, end, kind) = (number(read(entry.path().join("start"))), number(read(entry.path().join("end"))), read(entry.path().join("type")));
        matches!((start, end, kind.as_deref()), (Some(start), Some(end), Some("Reserved" | "ACPI Non-volatile Storage"))
            if start <= address && last <= end)
    })
}

/// As root: the database, straight from firmware memory.
fn read_hii_from_memory() -> io::Result<Vec<u8>> {
    let (address, size) = hii_location()?;
    if !in_firmware_memory(address, size) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("HiiDB points at {address:#x}, outside firmware memory")));
    }
    let file = fs::File::open("/dev/mem")?;
    let mut data = vec![0u8; size];
    std::os::unix::fs::FileExt::read_exact_at(&file, &mut data, address)?;
    Ok(data)
}

fn dmi(name: &str) -> String {
    fs::read_to_string(Path::new("/sys/class/dmi/id").join(name)).map(|t| t.trim().to_string()).unwrap_or_default()
}

/// The HII database, from the copy kept for this BIOS build when there is one,
/// otherwise through the helper (the one password prompt).
pub fn hii_database(app_root: &Path) -> Result<Vec<u8>, String> {
    let (address, size) = hii_location().map_err(|e| format!("Could not find the BIOS setup database: {e}"))?;
    let build = format!(
        "{}|{}|{}|{}|{}|{size}|{address:x}",
        dmi("board_vendor"),
        dmi("board_name"),
        dmi("bios_vendor"),
        dmi("bios_version"),
        dmi("bios_date")
    );
    let cache = app_root.join("hii").join(format!("{}.bin", &sha256_bytes(build.as_bytes())[..16]));
    if let Ok(data) = fs::read(&cache) {
        if data.len() == size && !HiiDatabase::parse(&data).questions.is_empty() {
            return Ok(data);
        }
    }
    let data = if is_root() {
        read_hii_from_memory().map_err(|e| format!("Could not read the BIOS setup database: {e}"))?
    } else {
        run_helper(HII_DUMP_ARG, None)?
    };
    if data.len() != size || HiiDatabase::parse(&data).questions.is_empty() {
        return Err("The BIOS setup database that was read holds no settings.".into());
    }
    if let Some(folder) = cache.parent() {
        let _ = fs::create_dir_all(folder).and_then(|()| fs::write(&cache, &data));
    }
    Ok(data)
}

pub fn is_root() -> bool {
    // SAFETY: geteuid takes no arguments and only returns the caller's effective user id.
    unsafe { libc::geteuid() == 0 }
}

/// Starts this program as root with `argument`, through `pkexec`, feeding it
/// `input`; its standard output, or why it failed.
fn run_helper(argument: &str, input: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let program = setup::helper_program().map_err(|e| format!("Could not find this program: {e}"))?;
    let mut command = Command::new("pkexec");
    // pkexec refuses to run at all when SHELL is not in /etc/shells.
    command.env_remove("SHELL").arg(&program).arg(argument);
    command.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => "Reading the BIOS settings needs root, and pkexec (polkit) is not installed to ask for it.".to_string(),
        _ => format!("Could not start pkexec: {e}"),
    })?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin.write_all(input).map_err(|e| format!("Could not hand the helper its input: {e}"))?;
    }
    let output = child.wait_with_output().map_err(|e| format!("Lost the helper: {e}"))?;
    match output.status.code() {
        Some(0) => Ok(output.stdout),
        Some(126) => Err("The password prompt was declined, so nothing was read or written.".into()),
        Some(127) => Err("No password prompt could be shown (no polkit agent). Run `sudo roch-nvram setup` once, or start Roch NVRAM from a desktop session.".into()),
        code => {
            let mut message = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if message.is_empty() {
                message = String::from_utf8_lossy(&output.stdout).trim().to_string();
            }
            Err(if message.is_empty() { format!("The helper stopped with code {code:?}.") } else { message })
        }
    }
}

/// SCEWIN's `/O /S nvram.txt /SD Dupes.txt`: both files, and a line for the log.
pub fn export_files(app_root: &Path, nvram: &Path, dupes: &Path) -> Result<String, String> {
    let db = HiiDatabase::parse(&hii_database(app_root)?);
    let created = chrono::Local::now().format("%m/%d/%y at %H:%M:%S").to_string();
    let script = nvram.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let export = sce::export(&db, &EfiVars, &script, &created);
    // Latin-1, as SCEWIN writes and the parser reads: anything else becomes '?'.
    let latin1 = |text: &str| -> Vec<u8> { text.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect() };
    fs::write(nvram, latin1(&export.nvram)).map_err(|e| format!("{}: {e}", nvram.display()))?;
    fs::write(dupes, latin1(&export.dupes)).map_err(|e| format!("{}: {e}", dupes.display()))?;
    Ok(format!(
        "Exported {} settings ({} duplicates set aside, {} in variables the running system cannot read) from HII CRC {:08X}.",
        export.exported, export.duplicates, export.unreadable, db.crc32
    ))
}

/// SCEWIN's `/I /S nvram_apply.txt`: the helper writes what the file changes.
pub fn import_file(path: &Path) -> Result<String, String> {
    let text = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let report = if is_root() { serve_import(&text)? } else { run_helper(IMPORT_ARG, Some(&text))? };
    Ok(String::from_utf8_lossy(&report).into_owned())
}

// linux/fs.h
const FS_IOC_GETFLAGS: libc::c_ulong = 0x8008_6601;
const FS_IOC_SETFLAGS: libc::c_ulong = 0x4008_6602;
const FS_IMMUTABLE_FL: libc::c_int = 0x10;

fn set_immutable(file: &fs::File, on: bool) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut flags: libc::c_int = 0;
    // SAFETY: both ioctls read or write one int at the pointer, which lives across the call.
    unsafe {
        if libc::ioctl(file.as_raw_fd(), FS_IOC_GETFLAGS, &mut flags) != 0 {
            return Err(io::Error::last_os_error());
        }
        flags = if on { flags | FS_IMMUTABLE_FL } else { flags & !FS_IMMUTABLE_FL };
        if libc::ioctl(file.as_raw_fd(), FS_IOC_SETFLAGS, &flags) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Rewrites an existing variable with the same attributes and length. efivarfs
/// marks most variables immutable, so that is lifted for the write and put back.
fn write_variable(store: &VarStore, data: &[u8]) -> io::Result<()> {
    let path = variable_path(store);
    let existing = fs::read(&path)?;
    if existing.len() != data.len() + 4 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{} changed size", store.name)));
    }
    let mut bytes = existing[..4].to_vec();
    bytes.extend_from_slice(data);
    let handle = fs::File::open(&path)?;
    let was_immutable = {
        let mut flags: libc::c_int = 0;
        // SAFETY: as in set_immutable.
        unsafe { libc::ioctl(std::os::fd::AsRawFd::as_raw_fd(&handle), FS_IOC_GETFLAGS, &mut flags) == 0 && flags & FS_IMMUTABLE_FL != 0 }
    };
    if was_immutable {
        set_immutable(&handle, false)?;
    }
    // efivarfs takes a variable in one write: the attributes, then the data.
    let result = fs::OpenOptions::new().write(true).custom_flags(libc::O_CLOEXEC).open(&path).and_then(|mut file| {
        let written = file.write(&bytes)?;
        if written == bytes.len() { Ok(()) } else { Err(io::Error::new(io::ErrorKind::WriteZero, "short write")) }
    });
    if was_immutable {
        let _ = set_immutable(&handle, true);
    }
    result
}

/// As root: plan the import against the live database, then write.
fn serve_import(text: &[u8]) -> Result<Vec<u8>, String> {
    let db = HiiDatabase::parse(&read_hii_from_memory().map_err(|e| format!("Could not read the BIOS setup database: {e}"))?);
    let text: String = text.iter().map(|&b| b as char).collect();
    let writes = sce::plan_import(&db, &EfiVars, &text)?;
    let mut report = String::new();
    if writes.is_empty() {
        report.push_str("Nothing to write: every value already matches.\n");
    }
    for (index, write) in writes.iter().enumerate() {
        if let Err(e) = write_variable(&write.store, &write.data) {
            let done: Vec<&str> = writes[..index].iter().map(|w| w.store.name.as_str()).collect();
            return Err(format!(
                "Writing {} failed: {e}.{}\nIf \"Password protection of Runtime Variables\" is enabled in the BIOS, the firmware refuses writes from the OS.",
                write.store.name,
                if done.is_empty() { " Nothing was written.".to_string() } else { format!(" Already written: {}.", done.join(", ")) }
            ));
        }
        for change in &write.changes {
            report.push_str(&format!("{change}\n"));
        }
    }
    Ok(report.into_bytes())
}

/// The helpers' entry points; `None` when `args` is not one of them.
pub fn run_helper_command(args: &[String]) -> Option<i32> {
    let first = args.first()?.as_str();
    if first == "setup" {
        return Some(setup::run(&args[1..]));
    }
    if first != HII_DUMP_ARG && first != IMPORT_ARG {
        return None;
    }
    if !is_root() {
        eprintln!("{first} is the root helper; Roch NVRAM starts it through pkexec.");
        return Some(1);
    }
    let result = if first == HII_DUMP_ARG {
        read_hii_from_memory().map_err(|e| format!("Could not read the BIOS setup database: {e}"))
    } else {
        let mut input = Vec::new();
        io::stdin().read_to_end(&mut input).map_err(|e| e.to_string()).and_then(|_| serve_import(&input))
    };
    Some(match result {
        Ok(bytes) => {
            let mut out = io::stdout().lock();
            if out.write_all(&bytes).and_then(|()| out.flush()).is_ok() { 0 } else { 1 }
        }
        Err(message) => {
            eprintln!("{message}");
            1
        }
    })
}

/// `roch-nvram setup`: no password for the two helpers.
///
/// The rule names one program nobody but root can change, or it would hand out root: a package's
/// `/usr/bin/roch-nvram` already is, so the rule names it and an update replaced in place keeps
/// working; anything else (`~/.local/bin`) is copied to [`setup::INSTALLED`] first, and the copy is
/// used only while it is this same build.
pub mod setup {
    use std::ffi::CStr;
    use std::os::unix::fs::{chown, MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};

    use super::{is_root, HII_DUMP_ARG, IMPORT_ARG};

    pub const INSTALLED: &str = "/usr/local/bin/roch-nvram";
    pub const RULE_FILE: &str = "/etc/polkit-1/rules.d/49-roch-nvram.rules";

    const USAGE: &str = "\
roch-nvram setup            stop asking for the password (run it with sudo)
roch-nvram setup --status   say whether it is set up, and whether this build will use it
roch-nvram setup --show     what setup would change, without changing it
roch-nvram setup --undo     remove it (with sudo), so reading and writing ask again
";

    /// This program when nobody but root can change it (a package install), else the installed
    /// copy while it is this very build, else this executable, which brings the prompt back.
    pub fn helper_program() -> std::io::Result<PathBuf> {
        let exe = std::env::current_exe()?;
        Ok(if root_protected(&exe) || !installed_matches(&exe) { exe } else { PathBuf::from(INSTALLED) })
    }

    /// Root-owned and writable by nobody else: the file, and every folder above it, since whoever
    /// can write a folder can swap the file inside it.
    fn root_protected(path: &Path) -> bool {
        let safe = |meta: &std::fs::Metadata| meta.uid() == 0 && meta.mode() & 0o022 == 0;
        match std::fs::metadata(path) {
            Ok(meta) if meta.is_file() && safe(&meta) => {}
            _ => return false,
        }
        path.ancestors().skip(1).all(|folder| folder.as_os_str().is_empty() || std::fs::metadata(folder).is_ok_and(|meta| safe(&meta)))
    }

    fn installed_matches(exe: &Path) -> bool {
        let installed = Path::new(INSTALLED);
        let Ok(meta) = std::fs::metadata(installed) else { return false };
        if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            return false;
        }
        if exe == installed {
            return true;
        }
        let Ok(own) = std::fs::metadata(exe) else { return false };
        own.len() == meta.len() && matches!((std::fs::read(exe), std::fs::read(installed)), (Ok(a), Ok(b)) if a == b)
    }

    pub fn run(arguments: &[String]) -> i32 {
        match arguments.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            [] => install(),
            ["--status"] => status(),
            ["--show"] => show(),
            ["--undo"] => undo(),
            _ => {
                eprint!("{USAGE}");
                2
            }
        }
    }

    fn require_root(verb: &str) -> bool {
        if is_root() {
            return true;
        }
        let exe = std::env::current_exe().map(|e| e.display().to_string()).unwrap_or_else(|_| "roch-nvram".into());
        eprintln!("That needs root. Run: sudo {exe} setup{verb}");
        false
    }

    fn invoking_user() -> Option<String> {
        if let Ok(name) = std::env::var("SUDO_USER") {
            if !name.is_empty() && name != "root" {
                return Some(name);
            }
        }
        let uid: u32 = std::env::var("PKEXEC_UID").ok()?.parse().ok()?;
        // SAFETY: getpwuid returns static storage or null; the name is copied out at once.
        unsafe {
            let entry = libc::getpwuid(uid);
            (!entry.is_null()).then(|| CStr::from_ptr((*entry).pw_name).to_string_lossy().into_owned())
        }
    }

    fn plain_name(name: &str) -> bool {
        !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    }

    fn plain_path(path: &str) -> bool {
        path.starts_with('/') && !path.bytes().any(|b| b == b'"' || b == b'\\' || b.is_ascii_control())
    }

    fn rule_text(user: &str, program: &str) -> String {
        format!(
            "// Written by `roch-nvram setup` for {user}; `sudo roch-nvram setup --undo` removes it.\n\
             // Lets {user} start Roch NVRAM's root helpers without a password: only {program}, which\n\
             // nobody but root can change, only as {HII_DUMP_ARG} (read the BIOS setup database) or\n\
             // {IMPORT_ARG} (write checked BIOS setting changes), and only from an active local session.\n\
             polkit.addRule(function (action, subject) {{\n\
             \x20   if (action.id == \"org.freedesktop.policykit.exec\" &&\n\
             \x20       action.lookup(\"program\") == \"{program}\" &&\n\
             \x20       (action.lookup(\"command_line\") == \"{program} {HII_DUMP_ARG}\" ||\n\
             \x20        action.lookup(\"command_line\") == \"{program} {IMPORT_ARG}\") &&\n\
             \x20       subject.user == \"{user}\" && subject.local && subject.active) {{\n\
             \x20       return polkit.Result.YES;\n\
             \x20   }}\n\
             }});\n"
        )
    }

    fn show() -> i32 {
        println!("Setup would:");
        match std::env::current_exe() {
            Ok(exe) if root_protected(&exe) => println!("  leave this program where it is: {} is root-owned, so the rule can name it", exe.display()),
            _ => println!("  copy this program to {INSTALLED}, owned by root and writable only by root"),
        }
        println!("  write {RULE_FILE}, letting you start that program as {HII_DUMP_ARG} or {IMPORT_ARG} without a password");
        println!("Anything running as you could then change BIOS settings without asking.");
        0
    }

    fn put(target: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
        let folder = target.parent().ok_or("no folder")?;
        std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
        let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let temporary = folder.join(format!(".{name}.new"));
        let _ = std::fs::remove_file(&temporary);
        let result = (|| {
            std::fs::write(&temporary, bytes).map_err(|e| format!("{}: {e}", temporary.display()))?;
            chown(&temporary, Some(0), Some(0)).map_err(|e| format!("chown: {e}"))?;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode)).map_err(|e| format!("chmod: {e}"))?;
            std::fs::rename(&temporary, target).map_err(|e| format!("{}: {e}", target.display()))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    /// The program the rule names, and whether it had to be copied there.
    fn install_files(user: &str) -> Result<(String, bool), String> {
        let exe = std::env::current_exe().map_err(|e| format!("could not find this program: {e}"))?;
        let (program, copied) = if root_protected(&exe) {
            // A package's copy: a stale one setup made earlier would only confuse, so it goes.
            let _ = std::fs::remove_file(INSTALLED);
            (exe.display().to_string(), false)
        } else {
            let bytes = std::fs::read(&exe).map_err(|e| format!("{}: {e}", exe.display()))?;
            put(Path::new(INSTALLED), &bytes, 0o755)?;
            (INSTALLED.to_string(), true)
        };
        if !plain_path(&program) {
            return Err(format!("{program} cannot be named in a polkit rule"));
        }
        put(Path::new(RULE_FILE), rule_text(user, &program).as_bytes(), 0o644)?;
        Ok((program, copied))
    }

    fn install() -> i32 {
        if !require_root("") {
            return 1;
        }
        let Some(user) = invoking_user().filter(|u| plain_name(u)) else {
            eprintln!("Could not tell who this is for: run it through sudo or pkexec rather than as root directly.");
            return 1;
        };
        match install_files(&user) {
            Ok((program, copied)) => {
                println!("Wrote {RULE_FILE}. Roch NVRAM will no longer ask {user} for a password.");
                if copied {
                    println!("It names the copy at {program}: run setup again after updating Roch NVRAM.");
                } else {
                    println!("It names {program}, so an update installed in its place keeps working.");
                }
                println!("`sudo roch-nvram setup --undo` removes it.");
                0
            }
            Err(error) => {
                eprintln!("Setup failed: {error}");
                1
            }
        }
    }

    fn status() -> i32 {
        let exe = std::env::current_exe().unwrap_or_default();
        let installed = Path::new(INSTALLED).exists();
        let rule = match std::fs::metadata(RULE_FILE) {
            Ok(_) => Some(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
            Err(_) => None,
        };
        if root_protected(&exe) {
            match rule {
                Some(true) => println!("Set up: {RULE_FILE} is in place."),
                Some(false) => println!("Not set up: reading and writing the BIOS settings ask for a password. Run `sudo roch-nvram setup`."),
                None => println!("{RULE_FILE} is readable only by root; `sudo roch-nvram setup --status` shows it."),
            }
            println!("{} is root-owned, so with the rule in place it starts without asking, updates included.", exe.display());
            return 0;
        }
        match (installed, rule) {
            (false, Some(false)) => println!("Not set up: reading and writing the BIOS settings ask for a password."),
            (true, Some(true)) => println!("Set up: {INSTALLED} and {RULE_FILE} are in place."),
            (true, None) => println!("Set up: {INSTALLED} is in place ({RULE_FILE} is readable only by root)."),
            (false, _) => println!("Not set up: {INSTALLED} is missing. Run `sudo roch-nvram setup`."),
            (true, Some(false)) => println!("Partly set up: {RULE_FILE} is missing. Run `sudo roch-nvram setup` again."),
        }
        if installed {
            if installed_matches(&exe) {
                println!("This build matches the installed copy, so it starts without asking.");
            } else {
                println!("This build differs from the installed copy, so it asks. Run `sudo {} setup` to update it.", exe.display());
            }
        }
        0
    }

    fn undo() -> i32 {
        if !require_root(" --undo") {
            return 1;
        }
        let mut failed = false;
        for path in [RULE_FILE, INSTALLED] {
            match std::fs::remove_file(path) {
                Ok(()) => println!("Removed {path}"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    eprintln!("{path}: {e}");
                    failed = true;
                }
            }
        }
        i32::from(failed)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_rule_allows_only_the_two_helpers_of_one_program() {
            let rule = rule_text("local", "/usr/bin/roch-nvram");
            assert!(rule.contains("== \"/usr/bin/roch-nvram --hii-dump\""));
            assert!(rule.contains("== \"/usr/bin/roch-nvram --sce-import\""));
            assert!(rule.contains("subject.user == \"local\" && subject.local && subject.active"));
            assert!(!plain_name("a\" || true || \"b"));
            assert!(plain_path("/usr/bin/roch-nvram") && !plain_path("/a\"b"));
        }

        #[test]
        fn a_home_install_is_not_protected() {
            let home = std::env::temp_dir().join(format!("roch-nvram-setup-test-{}", std::process::id()));
            std::fs::write(&home, b"x").unwrap();
            assert!(!root_protected(&home));
            let _ = std::fs::remove_file(home);
        }
    }
}
