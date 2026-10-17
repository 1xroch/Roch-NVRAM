// Roch NVRAM -- an editor for AMI SCEWIN NVRAM exports.
// Copyright (C) 2026 Roch Studio
// SPDX-License-Identifier: GPL-3.0-or-later

//! Safe, user-displayable failures.

use std::fmt;

/// Which kind of failure, for callers that react differently to each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Input that cannot be used as given: a bad value, a malformed file, a
    /// queue that no longer matches the live NVRAM.
    Validation,
    /// The export and the profile, or two exports, describe different BIOS
    /// layouts.
    ProfileMismatch,
    /// SCEWIN could not export, import, or verify the requested transaction.
    Execution,
    /// Anything else the user should be told about, such as a file that could
    /// not be written.
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn validation(message: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Validation, message: message.into() }
    }

    pub fn mismatch(message: impl Into<String>) -> Self {
        Self { kind: ErrorKind::ProfileMismatch, message: message.into() }
    }

    pub fn execution(message: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Execution, message: message.into() }
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Other, message: message.into() }
    }

    pub fn is_validation(&self) -> bool {
        self.kind == ErrorKind::Validation
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T, E = Error> = std::result::Result<T, E>;
