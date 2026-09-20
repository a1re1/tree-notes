//! Status computation, JSON envelopes, and text rendering.

use std::io::Write;

use serde::Serialize;

use crate::repo::{depth_within, Entry, Kind};
use crate::store::NoteVersion;
use crate::{CmdError, JSON_VERSION, PROGRAM};

/// Freshness of a note relative to the current content hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// A note exists for exactly this path, kind and content hash.
    Fresh,
    /// Notes exist for this path and kind, but not for the current hash.
    Stale,
    /// No note has ever been written for this path and kind.
    Missing,
}

impl Status {
    /// Stable lowercase name used in output and JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Fresh => "fresh",
            Status::Stale => "stale",
            Status::Missing => "missing",
        }
    }
}

/// A note entry as rendered in JSON and text output.
#[derive(Clone, Debug)]
pub struct EntryView {
    /// Repository-root relative path (`"."` for the root directory).
    pub path: String,
    /// Entry kind.
    pub kind: Kind,
    /// Current content hash.
    pub hash: String,
    /// Freshness of the note for the current hash.
    pub status: Status,
    /// Note text for the current hash, when fresh.
    pub note: Option<String>,
    /// Hash the displayed note belongs to, when one is displayed.
    pub note_hash: Option<String>,
    /// Timestamp of the displayed note, when one is displayed.
    pub note_updated_at: Option<String>,
    /// Latest historical note, present only for stale entries.
    pub previous: Option<Previous>,
}

/// The latest note recorded for a different content hash.
#[derive(Clone, Debug)]
pub struct Previous {
    /// Hash the previous note belongs to.
    pub hash: String,
    /// One-line note text.
    pub note: String,
    /// Timestamp of the previous note.
    pub updated_at: String,
}

impl EntryView {
    /// Combine a current tree entry with the stored note versions of its path.
    pub fn build(entry: &Entry, versions: &[NoteVersion]) -> EntryView {
        let matching = versions
            .iter()
            .find(|version| version.kind == entry.kind && version.hash == entry.hash);
        match matching {
            Some(version) => EntryView {
                path: entry.path.clone(),
                kind: entry.kind,
                hash: entry.hash.clone(),
                status: Status::Fresh,
                note: Some(version.note.clone()),
                note_hash: Some(version.hash.clone()),
                note_updated_at: Some(version.updated_at.clone()),
                previous: None,
            },
            None => {
                let latest = versions.iter().find(|version| version.kind == entry.kind);
                match latest {
                    Some(version) => EntryView {
                        path: entry.path.clone(),
                        kind: entry.kind,
                        hash: entry.hash.clone(),
                        status: Status::Stale,
                        note: None,
                        note_hash: None,
                        note_updated_at: None,
                        previous: Some(Previous {
                            hash: version.hash.clone(),
                            note: version.note.clone(),
                            updated_at: version.updated_at.clone(),
                        }),
                    },
                    None => EntryView {
                        path: entry.path.clone(),
                        kind: entry.kind,
                        hash: entry.hash.clone(),
                        status: Status::Missing,
                        note: None,
                        note_hash: None,
                        note_updated_at: None,
                        previous: None,
                    },
                }
            }
        }
    }

    /// Status as it appears in JSON.
    pub fn status_name(&self) -> &'static str {
        self.status.as_str()
    }

    /// Short display form of the current hash (12 hex characters).
    pub fn short_hash(&self) -> String {
        let hex = hex_of(&self.hash);
        hex.chars().take(12).collect()
    }

    /// Text line for one entry, indented relative to a scope.
    pub fn text_line(&self, scope: &str) -> String {
        let indent = "  ".repeat(depth_within(scope, &self.path));
        let mut line = format!(
            "{indent}{} [{}] {} {}",
            self.path,
            self.kind.as_str(),
            self.short_hash(),
            self.status.as_str()
        );
        if let Some(note) = &self.note {
            line.push_str(&format!(" {}", quote(note)));
        }
        line
    }

    /// Text line naming the previous note of a stale entry, labelled as not current.
    pub fn text_previous_line(&self, scope: &str) -> Option<String> {
        let previous = self.previous.as_ref()?;
        let indent = "  ".repeat(depth_within(scope, &self.path) + 1);
        Some(format!(
            "{indent}previous (stale, not current): {} {}",
            previous.hash,
            quote(&previous.note)
        ))
    }
}

/// Sort key: entry depth relative to the scope.
pub fn depth_key(scope: &str, path: &str) -> usize {
    depth_within(scope, path)
}

/// Ordering used by `pending`: descendants before their ancestors, scope last, name order
/// among unrelated paths, so agents can summarize files before the directories containing them.
pub fn compare_children_first(a: &str, b: &str) -> std::cmp::Ordering {
    /// Total-order key: `/` sorts after every other byte and a path ends after its `/`s, so
    /// `d/f` sorts before `d`, and the root (`.`) sorts last.
    fn key(path: &str) -> (u8, Vec<u8>) {
        if path == "." {
            return (1, Vec::new());
        }
        let mut out = Vec::with_capacity(path.len() + 2);
        for byte in path.bytes() {
            out.push(if byte == b'/' { 0xff } else { byte });
        }
        out.push(0xff);
        out.push(0xff);
        (0, out)
    }
    key(a).cmp(&key(b))
}

/// Quote a note for text output, escaping newlines and quotes defensively.
fn quote(note: &str) -> String {
    serde_json::to_string(note).expect("serializing a string cannot fail")
}

/// Hex part of a hash string (`tnt1:file:<hex>` -> `<hex>`).
pub fn hex_of(hash: &str) -> &str {
    match hash.rsplit_once(':') {
        Some((_, hex)) => hex,
        None => hash,
    }
}

/// JSON form of one tree entry.
#[derive(Debug, Serialize)]
pub struct EntryJson {
    /// Repository-root relative path.
    pub path: String,
    /// Entry kind.
    pub kind: String,
    /// Current content hash.
    pub hash: String,
    /// `fresh`, `stale`, or `missing`.
    pub status: String,
    /// Note text for the current hash (null unless fresh).
    pub note: Option<String>,
    /// Hash the note belongs to (null unless fresh).
    pub note_hash: Option<String>,
    /// Timestamp of the note (null unless fresh).
    pub note_updated_at: Option<String>,
    /// Latest historical note, present only when stale.
    pub previous: Option<PreviousJson>,
}

/// JSON form of the latest historical note.
#[derive(Debug, Serialize)]
pub struct PreviousJson {
    /// Hash the previous note belongs to.
    pub hash: String,
    /// One-line note text.
    pub note: String,
    /// Timestamp of the previous note.
    pub updated_at: String,
}

impl EntryJson {
    /// JSON form of a note that was just written for the current entry version.
    pub fn from_new(note: &crate::store::NewNote) -> EntryJson {
        EntryJson {
            path: note.path.clone(),
            kind: note.kind.as_str().to_string(),
            hash: note.hash.clone(),
            status: Status::Fresh.as_str().to_string(),
            note: Some(note.note.clone()),
            note_hash: Some(note.hash.clone()),
            note_updated_at: None,
            previous: None,
        }
    }
}

impl From<&EntryView> for EntryJson {
    fn from(view: &EntryView) -> EntryJson {
        EntryJson {
            path: view.path.clone(),
            kind: view.kind.as_str().to_string(),
            hash: view.hash.clone(),
            status: view.status.as_str().to_string(),
            note: view.note.clone(),
            note_hash: view.note_hash.clone(),
            note_updated_at: view.note_updated_at.clone(),
            previous: view.previous.as_ref().map(|previous| PreviousJson {
                hash: previous.hash.clone(),
                note: previous.note.clone(),
                updated_at: previous.updated_at.clone(),
            }),
        }
    }
}

/// Repository identity block of the JSON envelope.
#[derive(Debug, Serialize)]
pub struct RepoJson {
    /// Stable repository identity, shared by all linked worktrees.
    pub identity: String,
    /// Absolute path of this worktree's root.
    pub root: String,
    /// Absolute path of the canonical Git common directory.
    pub common_dir: String,
}

/// Scope block of the JSON envelope.
#[derive(Debug, Serialize)]
pub struct ScopeJson {
    /// Repository-root relative scope path (`"."` for the whole repository).
    pub path: String,
    /// `file`, `dir`, `symlink`, or `submodule`.
    pub kind: String,
    /// Maximum depth below the scope, when `--depth` was given.
    pub depth: Option<usize>,
}

/// Versioned JSON envelope emitted on stdout by `--json` commands.
#[derive(Debug, Serialize)]
pub struct Envelope {
    /// Envelope version; bump when the shape changes.
    pub version: u32,
    /// Always `treenotes`.
    pub tool: String,
    /// Command that produced the envelope.
    pub command: String,
    /// Repository identity.
    pub repository: RepoJson,
    /// Scope of the query, when the command takes a path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeJson>,
    /// Number of records written, for `import`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported: Option<usize>,
    /// Human readable result line, for `set` and `import`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Deterministically ordered entries.
    pub entries: Vec<EntryJson>,
}

impl Envelope {
    /// Start an envelope for `command` in `repository`.
    pub fn new(command: &str, repository: RepoJson) -> Envelope {
        Envelope {
            version: JSON_VERSION,
            tool: PROGRAM.to_string(),
            command: command.to_string(),
            repository,
            scope: None,
            imported: None,
            message: None,
            entries: Vec::new(),
        }
    }

    /// Emit the envelope as pretty, deterministic JSON followed by a newline.
    pub fn print(&self) -> Result<(), CmdError> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| CmdError::env(format!("cannot serialize JSON: {e}")))?;
        let mut stdout = std::io::stdout();
        stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.write_all(b"\n"))
            .and_then(|()| stdout.flush())
            .map_err(|e| CmdError::env(format!("cannot write to stdout: {e}")))
    }
}

/// Print lines to stdout, flushing once.
pub fn print_lines(lines: &[String]) -> Result<(), CmdError> {
    let mut stdout = std::io::stdout();
    for line in lines {
        stdout
            .write_all(line.as_bytes())
            .and_then(|()| stdout.write_all(b"\n"))
            .map_err(|e| CmdError::env(format!("cannot write to stdout: {e}")))?;
    }
    stdout
        .flush()
        .map_err(|e| CmdError::env(format!("cannot write to stdout: {e}")))
}
