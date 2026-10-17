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

//! Roch NVRAM: read an AMI SCEWIN export, queue setting changes, and write
//! them back only after a backup, a fresh re-export that confirms nothing
//! moved, and a value-by-value verification afterwards.
//!
//! Everything here is independent of the window, which lives in the
//! `roch-nvram` binary.

pub mod backend;
pub mod catalog;
pub mod compare;
pub mod core;
pub mod error;
pub mod parser;
pub mod paths;
pub mod pycompat;
pub mod quick_settings;
pub mod session;
pub mod version;

pub use crate::error::{Error, ErrorKind, Result};
pub use crate::version::{APP_NAME, VERSION};
