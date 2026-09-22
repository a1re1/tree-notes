//! Status computation, JSON envelopes, and text rendering.

use std::io::Write;

use serde::Serialize;

use crate::repo::{depth_within, Entry, Kind};
use crate::store::{MemberVersion, NoteVersion};
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

/// One AST member of a file, paired with the stored member notes of its symbol.
#[derive(Clone, Debug)]
pub struct MemberView {
    /// Repository-root relative path of the file holding the declaration.
    pub path: String,
    /// Symbol key, `<symbol-kind>:<qualified name>:<ordinal>`.
    pub symbol: String,
    /// treenotes member kind.
    pub symbol_kind: String,
    /// Declared name as written.
    pub name: String,
    /// Name qualified by its container chain.
    pub qualified_name: String,
    /// 1-based first line of the declaration in the current file.
    pub start_line: usize,
    /// 1-based last line of the declaration in the current file.
    pub end_line: usize,
    /// Current `tnt2:member:<hex>` hash of the declaration.
    pub hash: String,
    /// Freshness of the member note for the current hash.
    pub status: Status,
    /// Note text for the current hash, when fresh.
    pub note: Option<String>,
    /// Hash the displayed note belongs to, when one is displayed.
    pub note_hash: Option<String>,
    /// Timestamp of the displayed note, when one is displayed.
    pub note_updated_at: Option<String>,
    /// Latest historical note, present only for stale members.
    pub previous: Option<Previous>,
}

impl MemberView {
    /// Combine a freshly parsed member with the stored member notes of its symbol.
    pub fn build(
        path: &str,
        member: &crate::ast::Member,
        versions: &[MemberVersion],
    ) -> MemberView {
        let matching = versions.iter().find(|version| version.hash == member.hash);
        let latest = versions.first();
        let (status, note, note_hash, note_updated_at, previous) = match matching {
            Some(version) => (
                Status::Fresh,
                Some(version.note.clone()),
                Some(version.hash.clone()),
                Some(version.updated_at.clone()),
                None,
            ),
            None => match latest {
                Some(version) => (
                    Status::Stale,
                    None,
                    None,
                    None,
                    Some(Previous {
                        hash: version.hash.clone(),
                        note: version.note.clone(),
                        updated_at: version.updated_at.clone(),
                    }),
                ),
                None => (Status::Missing, None, None, None, None),
            },
        };
        MemberView {
            path: path.to_string(),
            symbol: member.symbol.clone(),
            symbol_kind: member.symbol_kind.clone(),
            name: member.name.clone(),
            qualified_name: member.qualified_name.clone(),
            start_line: member.start_line,
            end_line: member.end_line,
            hash: member.hash.clone(),
            status,
            note,
            note_hash,
            note_updated_at,
            previous,
        }
    }

    /// Short display form of the current hash (12 hex characters).
    pub fn short_hash(&self) -> String {
        hex_of(&self.hash).chars().take(12).collect()
    }

    /// Ordinal of this symbol, the last `:`-separated component of the symbol key.
    pub fn ordinal(&self) -> &str {
        self.symbol
            .rsplit_once(':')
            .map(|(_, ordinal)| ordinal)
            .unwrap_or("0")
    }

    /// Label of one declaration, the form used wherever a label is needed.
    pub fn label(&self) -> String {
        format!("{} {}:{}", self.name, self.symbol_kind, self.ordinal())
    }

    /// Body of one declaration: label, member kind, hash and status, then the note when fresh.
    pub fn body(&self) -> String {
        let mut body = format!(
            "{} [{}] {} {}",
            self.label(),
            self.symbol_kind,
            self.short_hash(),
            self.status.as_str()
        );
        if let Some(note) = &self.note {
            body.push_str(&format!(" {}", quote(note)));
        }
        body
    }

    /// Previous-note body of a stale declaration, labelled as not current.
    pub fn previous_body(&self) -> Option<String> {
        let previous = self.previous.as_ref()?;
        Some(format!(
            "previous (stale, not current): {} {}",
            previous.hash,
            quote(&previous.note)
        ))
    }

    /// Text line for one member, indented one level below its file.
    pub fn text_line(&self, scope: &str) -> String {
        format!(
            "{}{}",
            "  ".repeat(depth_within(scope, &self.path) + 1),
            self.body()
        )
    }

    /// Text line naming the previous note of a stale member, labelled as not current.
    pub fn text_previous_line(&self, scope: &str) -> Option<String> {
        Some(format!(
            "{}{}",
            "  ".repeat(depth_within(scope, &self.path) + 2),
            self.previous_body()?
        ))
    }
}

/// JSON form of one AST member.
#[derive(Debug, Serialize)]
pub struct MemberJson {
    /// Repository-root relative path of the file holding the declaration.
    pub path: String,
    /// Symbol key.
    pub symbol: String,
    /// treenotes member kind.
    pub symbol_kind: String,
    /// Declared name as written.
    pub name: String,
    /// Name qualified by its container chain.
    pub qualified_name: String,
    /// 1-based first line of the declaration.
    pub start_line: usize,
    /// 1-based last line of the declaration.
    pub end_line: usize,
    /// Current member hash.
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

impl From<&MemberView> for MemberJson {
    fn from(view: &MemberView) -> MemberJson {
        MemberJson {
            path: view.path.clone(),
            symbol: view.symbol.clone(),
            symbol_kind: view.symbol_kind.clone(),
            name: view.name.clone(),
            qualified_name: view.qualified_name.clone(),
            start_line: view.start_line,
            end_line: view.end_line,
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

impl MemberJson {
    /// JSON form of a member note that was just written for the current member version.
    pub fn from_new(note: &crate::store::NewMemberNote) -> MemberJson {
        MemberJson {
            path: note.path.clone(),
            symbol: note.symbol.clone(),
            symbol_kind: note.symbol_kind.clone(),
            name: note.name.clone(),
            qualified_name: note.qualified_name.clone(),
            start_line: note.start_line,
            end_line: note.end_line,
            hash: note.hash.clone(),
            status: Status::Fresh.as_str().to_string(),
            note: Some(note.note.clone()),
            note_hash: Some(note.hash.clone()),
            note_updated_at: None,
            previous: None,
        }
    }
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

    /// Body of one entry: path, kind, hash, status, then the note when fresh.
    fn body(&self) -> String {
        let mut body = format!(
            "{} [{}] {} {}",
            self.path,
            self.kind.as_str(),
            self.short_hash(),
            self.status.as_str()
        );
        if let Some(note) = &self.note {
            body.push_str(&format!(" {}", quote(note)));
        }
        body
    }

    /// Body of one entry inside the text tree.
    pub fn tree_body(&self) -> String {
        self.body()
    }

    /// Text line for one entry, indented relative to a scope, for listings that are not a tree.
    pub fn text_line(&self, scope: &str) -> String {
        format!(
            "{}{}",
            "    ".repeat(depth_within(scope, &self.path)),
            self.body()
        )
    }

    /// Previous-note body of a stale entry, labelled as not current.
    pub fn previous_body(&self) -> Option<String> {
        let previous = self.previous.as_ref()?;
        Some(format!(
            "previous (stale, not current): {} {}",
            previous.hash,
            quote(&previous.note)
        ))
    }

    /// Text line naming the previous note of a stale entry, labelled as not current.
    pub fn text_previous_line(&self, scope: &str) -> Option<String> {
        Some(format!(
            "{}{}",
            "    ".repeat(depth_within(scope, &self.path) + 1),
            self.previous_body()?
        ))
    }
}

/// Sort key: entry depth relative to the scope.
pub fn depth_key(scope: &str, path: &str) -> usize {
    depth_within(scope, path)
}

/// One node of the text tree, in the order it should be drawn.
#[derive(Clone, Debug)]
pub enum TreeChild {
    /// A file or directory entry, as it appears in `entries`, with its hash and note status.
    Entry(EntryView),
    /// A path that is *not* part of the listing and is drawn only to locate something below it
    /// (an ancestor directory, or a file that holds a listed declaration). It carries no hash and
    /// no status, because nothing about it is unfinished.
    Context(String),
    /// A declaration inside the file named by the node it hangs from.
    Member(MemberView),
    /// Extra information about the node it hangs from, such as a stale previous note.
    Detail(String),
}

/// Render the requested entries and declarations as one nested ASCII tree.
///
/// Each element names the *closest requested ancestor*, or `None` when nothing in the listing
/// contains it (a `pending` directory whose parent directory is already annotated, say). The first
/// element must have no ancestor: it is the tree root the rest hangs from. An element appears
/// directly below its ancestor; a member is always placed below the file it was parsed from, which
/// is the preceding file entry, so no parentage is ever inferred from a declaration's name.
///
/// A file that is not itself listed is still printed when it holds a listed declaration: it
/// appears in parentheses as `(path) [context]`, never with a note status, because nothing about
/// that file is unfinished — it is printed only to say where the declarations below it live.
pub fn build_tree(children: &[(Option<String>, TreeChild)]) -> Result<Vec<String>, CmdError> {
    if children.is_empty() {
        return Ok(Vec::new());
    }
    if children[0].0.is_some() {
        return Err(CmdError::usage("tree listing is missing its root entry"));
    }
    let total = children.len();
    let mut lines = vec![node_body(&children[0].1)];
    // Per level: the label of the ancestor at that depth, and whether the node that opened the
    // level was the last child of its own parent (so the level draws blanks instead of pipes).
    let mut labels: Vec<String> = vec![node_label(&children[0].1)];
    let mut closes: Vec<bool> = vec![false];
    for index in 1..total {
        let parent = children[index]
            .0
            .as_deref()
            .ok_or_else(|| CmdError::usage("tree listing has a second root entry"))?;
        let level = labels
            .iter()
            .rposition(|label| label == parent)
            .ok_or_else(|| {
                CmdError::usage(format!("tree listing names unknown ancestor {parent}"))
            })?;
        // The node is one level below its parent, so the levels strictly above it draw a pipe when
        // the ancestor that opened them is not the last child of its own parent, and blanks when it
        // is — the same convention as `tree` and `git log --graph`.
        let depth = level + 1;
        let ancestors: String = (1..depth)
            .map(|above| {
                if closes.get(above).copied().unwrap_or(false) {
                    "    "
                } else {
                    "│   "
                }
            })
            .collect();
        let last = children[index + 1..]
            .iter()
            .all(|(next, _)| next.as_deref() != Some(parent));
        let connector = if last { "└── " } else { "├── " };
        lines.push(format!(
            "{ancestors}{connector}{}",
            node_body(&children[index].1)
        ));
        // The node that was just drawn owns the next level: its own "last child" flag decides
        // whether the levels underneath it draw a pipe or a blank.
        labels.truncate(level + 1);
        labels.push(node_label(&children[index].1));
        closes.truncate(level + 1);
        closes.push(last);
    }
    Ok(lines)
}

/// Body of one node of the tree, without any branch decoration.
fn node_body(child: &TreeChild) -> String {
    match child {
        TreeChild::Entry(view) => view.tree_body(),
        TreeChild::Context(path) => format!("({path}) [context]"),
        TreeChild::Member(member) => member.body(),
        TreeChild::Detail(text) => text.clone(),
    }
}

/// The label other nodes name as their ancestor. A detail line is never an ancestor.
pub fn node_label(child: &TreeChild) -> String {
    match child {
        TreeChild::Entry(view) => view.path.clone(),
        TreeChild::Context(path) => path.clone(),
        TreeChild::Member(member) => member.symbol.clone(),
        TreeChild::Detail(_) => String::new(),
    }
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

/// One entry-level change between the current state and a recorded state.
#[derive(Debug, Serialize)]
pub struct StateChangeJson {
    /// Repository-root relative path.
    pub path: String,
    /// Entry kind in the current state (`file`, `symlink`, `submodule`).
    pub kind: String,
    /// `added`, `removed`, `modified`, or `kind-changed`.
    pub change: String,
    /// Current hash, absent for `removed`.
    pub hash: Option<String>,
    /// Hash in the compared state, present when the path existed there.
    pub previous_hash: Option<String>,
    /// Kind in the compared state, present only for `kind-changed`.
    pub previous_kind: Option<String>,
}

/// The recorded state a `treenotes state` run was compared against.
#[derive(Debug, Serialize)]
pub struct ComparedStateJson {
    /// `tnt1:state:<hex>` of the compared state.
    pub state_hash: String,
    /// Commit the compared state was observed at, when it was recorded with one.
    pub commit: Option<String>,
    /// When the compared state was recorded.
    pub updated_at: String,
}

/// The derived state block emitted by `treenotes state`.
#[derive(Debug, Serialize)]
pub struct StateJson {
    /// Aggregate hash of the current inventory: the path, kind and hash of every entry, in path
    /// order. Equal state hashes mean identical trees, so anything derived from them is
    /// identical too.
    pub state_hash: String,
    /// Commit `HEAD` pointed at when the state was observed; null before the first commit.
    pub commit: Option<String>,
    /// True when this exact state hash had already been recorded by an earlier invocation.
    pub known: bool,
    /// Whether this invocation recorded the state.
    pub recorded: bool,
    /// Source files whose members are already cached for their current content hash.
    pub member_cache_hits: usize,
    /// Source files whose members would have to be parsed again.
    pub member_cache_misses: usize,
    /// The recorded state this run is compared against, when one exists.
    pub compared_state: Option<ComparedStateJson>,
    /// Entry-level changes against `compared_state`, ordered by path. Directories are omitted:
    /// a directory hash changes exactly when one of its leaves does.
    pub changes: Vec<StateChangeJson>,
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
    /// Human readable result line, for `set`, `member-set` and `import`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// True when the grammar had to recover from a syntax error in the scoped files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<bool>,
    /// Derived state block, present only on `state`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<StateJson>,
    /// Deterministically ordered entries.
    pub entries: Vec<EntryJson>,
    /// Deterministically ordered AST members.
    pub members: Vec<MemberJson>,
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
            parse_error: None,
            state: None,
            entries: Vec::new(),
            members: Vec::new(),
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
