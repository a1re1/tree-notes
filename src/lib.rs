//! `treenotes` - content-versioned notes for Git repository files and directories.
//!
//! The crate maps a Git-visible working tree to content hashes and stores one-line notes
//! keyed by `(repository, repo-relative path, entry kind, content hash)` in a local SQLite
//! database. Notes survive edits, branch switches and linked worktrees because they are
//! bound to content, not to a mutable path alone.
//!
//! No command in this crate talks to a model, network service, adapter or classifier.

use std::fmt;

pub mod cli;
pub mod output;
pub mod repo;
pub mod store;

/// Program name used in help output and JSON envelopes.
pub const PROGRAM: &str = "treenotes";

/// Version of the JSON envelope written by `--json` commands.
pub const JSON_VERSION: u32 = 1;

/// Version of the SQLite schema this build reads and writes.
pub const SCHEMA_VERSION: i64 = 1;

/// Exit code: success.
pub const EXIT_OK: u8 = 0;
/// Exit code: invalid input, invalid scope, failed validation, rejected record.
pub const EXIT_USAGE: u8 = 1;
/// Exit code: environment failure (git, filesystem, database, unsupported schema).
pub const EXIT_ENV: u8 = 2;

/// An error with a documented process exit code.
#[derive(Debug)]
pub struct CmdError {
    /// Process exit code: [`EXIT_USAGE`] or [`EXIT_ENV`].
    pub code: u8,
    /// Human readable message, printed to stderr as `error: <message>`.
    pub message: String,
}

impl CmdError {
    /// Invalid input, rejected record, or invalid path scope.
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_USAGE,
            message: message.into(),
        }
    }

    /// Repository, filesystem, or database failure.
    pub fn env(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_ENV,
            message: message.into(),
        }
    }

    /// Wrap an arbitrary error as an environment failure.
    pub fn from_context(context: &str, err: impl fmt::Display) -> Self {
        Self::env(format!("{context}: {err}"))
    }
}

impl fmt::Display for CmdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CmdError {}
