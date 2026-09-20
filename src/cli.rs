//! Command line surface: global options, the four subcommands, and their exit codes.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use serde::Deserialize;

use crate::output::{
    compare_children_first, depth_key, EntryJson, EntryView, Envelope, RepoJson, ScopeJson, Status,
};
use crate::repo::{in_scope, Entry, Kind, Repo};
use crate::store::{default_db_path, NewNote, NoteVersion, Store};
use crate::CmdError;

/// Compact, content-versioned notes for a Git repository.
#[derive(Debug, Parser)]
#[command(
    name = "treenotes",
    version,
    about = "Content-versioned notes for Git repository files and directories",
    long_about = "treenotes maps the Git-visible working tree of a repository to content hashes and \
                  stores one-line notes keyed by repository, path, kind and hash in a local SQLite \
                  database. Notes survive edits and branch switches, and are shared by linked \
                  worktrees. It never calls an AI model or any network service; external agents \
                  read source and write notes themselves."
)]
struct Cli {
    /// Repository (or any path inside it); defaults to the current directory.
    #[arg(long, global = true, value_name = "DIR")]
    repo: Option<PathBuf>,

    /// Notes database; defaults to ~/.tree-notes/notes.sqlite3.
    #[arg(long, global = true, value_name = "PATH")]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List entries that are missing a note or whose note is stale.
    Pending(PendingArgs),
    /// Show the tree, or one file or directory subtree, with note freshness.
    Read(ReadArgs),
    /// Annotate the current version of one file, directory, symlink or submodule.
    Set(SetArgs),
    /// Import a JSON batch of {path, hash, note} records atomically.
    Import(ImportArgs),
}

#[derive(Debug, Args)]
struct PendingArgs {
    /// Restrict the listing to this path or subtree (repository-root relative).
    path: Option<String>,
    /// Emit the versioned JSON envelope instead of text.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ReadArgs {
    /// Show only this file, or this directory and its subtree (repository-root relative).
    path: Option<String>,
    /// Show at most this many levels below the scope (0 shows just the scope).
    #[arg(long, value_name = "N")]
    depth: Option<usize>,
    /// Emit the versioned JSON envelope instead of text.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct SetArgs {
    /// File, directory, symlink or submodule to annotate (repository-root relative).
    path: String,
    /// One-line note text; when omitted the note is read from stdin.
    #[arg(long, value_name = "TEXT")]
    note: Option<String>,
    /// Reject the write unless the current content hash is exactly this value.
    #[arg(long, value_name = "HASH")]
    expected_hash: Option<String>,
    /// Emit the versioned JSON envelope instead of text.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ImportArgs {
    /// JSON file holding an array of {path, hash, note} records; `-` or omitted reads stdin.
    #[arg(value_name = "FILE")]
    file: Option<String>,
    /// Emit the versioned JSON envelope instead of text.
    #[arg(long)]
    json: bool,
}

/// JSON record accepted by `treenotes import`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportRecord {
    /// Repository-root relative path of the annotated entry.
    path: String,
    /// Hash the note is written for; must equal the current hash of that entry.
    hash: String,
    /// One-line note text.
    note: String,
}

/// Parsed state shared by every subcommand.
struct Ctx {
    repo: Repo,
    store: Store,
    entries: Vec<Entry>,
}

/// Path scope of a command, resolved against the repository root.
struct Scope {
    path: String,
    kind: Kind,
}

/// Parse arguments and execute one command.
///
/// Exit codes: 0 success (including an empty `pending` listing); 1 invalid input, invalid scope,
/// rejected record, or failed validation; 2 environment failure (git, filesystem, database,
/// unsupported schema) or a command line syntax error reported by clap.
/// Env override: `TREENOTES_DB` is not consulted; use `--db`.
pub fn run<I>(args: I) -> Result<(), CmdError>
where
    I: IntoIterator<Item = OsString>,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            return Err(CmdError {
                code: err.exit_code() as u8,
                message: String::new(),
            });
        }
    };

    let repo = Repo::discover(cli.repo.as_deref())?;
    let db_path = match &cli.db {
        Some(path) => path.clone(),
        None => default_db_path()?,
    };
    let db_path = absolute_path(&db_path)?;
    let exclude = db_artifacts(&db_path);
    let store = Store::open(&db_path)?;
    let entries = repo.inventory(&exclude)?;
    let mut ctx = Ctx {
        repo,
        store,
        entries,
    };

    match cli.command {
        Command::Pending(args) => cmd_pending(&ctx, args),
        Command::Read(args) => cmd_read(&ctx, args),
        Command::Set(args) => cmd_set(&mut ctx, args),
        Command::Import(args) => cmd_import(&mut ctx, args),
    }
}

fn cmd_pending(ctx: &Ctx, args: PendingArgs) -> Result<(), CmdError> {
    let (scope, mut views) = scoped_views(ctx, args.path.as_deref())?;
    views.retain(|view| view.status != Status::Fresh);
    views.sort_by(|a, b| compare_children_first(&a.path, &b.path));

    if args.json {
        let mut envelope = Envelope::new("pending", repo_json(ctx));
        envelope.scope = Some(ScopeJson {
            path: scope.path.clone(),
            kind: scope.kind.as_str().to_string(),
            depth: None,
        });
        envelope.entries = views.iter().map(EntryJson::from).collect();
        return envelope.print();
    }

    let mut lines = Vec::new();
    for view in &views {
        lines.push(view.text_line(&scope.path));
        if let Some(previous) = view.text_previous_line(&scope.path) {
            lines.push(previous);
        }
    }
    crate::output::print_lines(&lines)
}

fn cmd_read(ctx: &Ctx, args: ReadArgs) -> Result<(), CmdError> {
    let (scope, mut views) = scoped_views(ctx, args.path.as_deref())?;
    if let Some(depth) = args.depth {
        views.retain(|view| depth_key(&scope.path, &view.path) <= depth);
    }
    views.sort_by(|a, b| a.path.cmp(&b.path));

    if args.json {
        let mut envelope = Envelope::new("read", repo_json(ctx));
        envelope.scope = Some(ScopeJson {
            path: scope.path.clone(),
            kind: scope.kind.as_str().to_string(),
            depth: args.depth,
        });
        envelope.entries = views.iter().map(EntryJson::from).collect();
        return envelope.print();
    }

    let lines: Vec<String> = views
        .iter()
        .map(|view| view.text_line(&scope.path))
        .collect();
    crate::output::print_lines(&lines)
}

fn cmd_set(ctx: &mut Ctx, args: SetArgs) -> Result<(), CmdError> {
    let note = match &args.note {
        Some(text) => validate_note(text)?,
        None => {
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|e| CmdError::env(format!("cannot read the note from stdin: {e}")))?;
            validate_note(&buffer)?
        }
    };

    let (scope, _) = scoped_views(ctx, Some(&args.path))?;
    let entry = ctx
        .entries
        .iter()
        .find(|entry| entry.path == scope.path)
        .ok_or_else(|| {
            CmdError::usage(format!(
                "{} is not part of the Git-visible tree",
                scope.path
            ))
        })?
        .clone();
    if let Some(expected) = &args.expected_hash {
        if expected != &entry.hash {
            return Err(CmdError::usage(format!(
                "expected hash {} does not match the current hash {} of {}",
                expected, entry.hash, entry.path
            )));
        }
    }

    ctx.store.write_note(
        &ctx.repo.identity,
        &NewNote {
            path: entry.path.clone(),
            kind: entry.kind,
            hash: entry.hash.clone(),
            note: note.clone(),
        },
    )?;

    if args.json {
        let mut envelope = Envelope::new("set", repo_json(ctx));
        envelope.scope = Some(ScopeJson {
            path: entry.path.clone(),
            kind: entry.kind.as_str().to_string(),
            depth: None,
        });
        envelope.message = Some(format!("annotated {} (fresh)", entry.path));
        envelope.entries = vec![EntryJson::from(&EntryView {
            path: entry.path.clone(),
            kind: entry.kind,
            hash: entry.hash.clone(),
            status: Status::Fresh,
            note: Some(note),
            note_hash: Some(entry.hash.clone()),
            note_updated_at: None,
            previous: None,
        })];
        return envelope.print();
    }

    crate::output::print_lines(&[format!(
        "annotated {} [{}] fresh {}",
        entry.path,
        entry.kind.as_str(),
        entry.hash
    )])
}

fn cmd_import(ctx: &mut Ctx, args: ImportArgs) -> Result<(), CmdError> {
    let text = match args.file.as_deref() {
        None | Some("-") => {
            let mut buffer = String::new();
            std::io::stdin().read_to_string(&mut buffer).map_err(|e| {
                CmdError::env(format!("cannot read the import batch from stdin: {e}"))
            })?;
            buffer
        }
        Some(path) => std::fs::read_to_string(path)
            .map_err(|e| CmdError::env(format!("cannot read import file {path}: {e}")))?,
    };

    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| CmdError::usage(format!("import batch is not valid JSON: {e}")))?;
    let items = value
        .as_array()
        .ok_or_else(|| {
            CmdError::usage("import batch must be a JSON array of {path, hash, note} records")
        })?
        .clone();
    if items.is_empty() {
        return Err(CmdError::usage("import batch contains no records"));
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut notes: Vec<NewNote> = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let record: ImportRecord = serde_json::from_value(item.clone()).map_err(|e| {
            CmdError::usage(format!(
                "record {index} is not a valid {{path, hash, note}} record: {e}"
            ))
        })?;
        let path = normalize_relative_path(&record.path)
            .map_err(|e| CmdError::usage(format!("record {index}: {e}")))?;
        let note = validate_note(&record.note)
            .map_err(|e| CmdError::usage(format!("record {index}: {e}")))?;
        if !seen.insert(path.clone()) {
            return Err(CmdError::usage(format!(
                "record {index}: duplicate path {path} after normalization"
            )));
        }
        let entry = ctx
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .ok_or_else(|| {
                CmdError::usage(format!(
                    "record {index}: {path} is not part of the current Git-visible tree"
                ))
            })?;
        if record.hash != entry.hash {
            return Err(CmdError::usage(format!(
                "record {index}: hash {} does not match the current hash {} of {path}",
                record.hash, entry.hash
            )));
        }
        notes.push(NewNote {
            path: entry.path.clone(),
            kind: entry.kind,
            hash: entry.hash.clone(),
            note,
        });
    }

    let count = notes.len();
    ctx.store.write_notes(&ctx.repo.identity, &notes)?;

    if args.json {
        let mut envelope = Envelope::new("import", repo_json(ctx));
        envelope.imported = Some(count);
        envelope.message = Some(format!("imported {count} notes"));
        envelope.entries = notes.iter().map(EntryJson::from_new).collect();
        return envelope.print();
    }

    crate::output::print_lines(&[format!("imported {count} notes")])
}

/// Current entries within a path scope, paired with their stored note versions.
fn scoped_views(ctx: &Ctx, path: Option<&str>) -> Result<(Scope, Vec<EntryView>), CmdError> {
    let scope = match path {
        Some(path) => resolve_scope(&ctx.repo, &ctx.entries, path)?,
        None => Scope {
            path: ".".to_string(),
            kind: Kind::Dir,
        },
    };
    let versions = ctx.store.load_versions(&ctx.repo.identity)?;
    let empty: Vec<NoteVersion> = Vec::new();
    let mut views = Vec::new();
    for entry in &ctx.entries {
        if in_scope(&entry.path, &scope.path, scope.kind) {
            let stored = versions.get(&entry.path).unwrap_or(&empty);
            views.push(EntryView::build(entry, stored));
        }
    }
    Ok((scope, views))
}

/// Resolve a user supplied path argument to a Git-visible tree entry.
fn resolve_scope(repo: &Repo, entries: &[Entry], arg: &str) -> Result<Scope, CmdError> {
    if arg.is_empty() {
        return Err(CmdError::usage("path must not be empty"));
    }
    if arg == "." || arg == "./" {
        return Ok(Scope {
            path: ".".to_string(),
            kind: Kind::Dir,
        });
    }

    let raw = Path::new(arg);
    let mut candidates: Vec<String> = Vec::new();

    if raw.is_absolute() {
        let lexical = normalize(raw);
        match relative_to(&repo.root, &lexical) {
            Some(rel) => candidates.push(rel),
            None => {
                if let Ok(canonical) = lexical.canonicalize() {
                    if let Some(rel) = relative_to(&repo.root, &canonical) {
                        candidates.push(rel);
                    }
                }
            }
        }
    } else {
        // Exported paths always refer to the repository root, regardless of the caller's cwd.
        if let Some(rel) = root_relative(arg) {
            candidates.push(rel);
        }
    }

    for candidate in &candidates {
        if let Some(entry) = entries.iter().find(|entry| entry.path == *candidate) {
            return Ok(Scope {
                path: entry.path.clone(),
                kind: entry.kind,
            });
        }
    }

    if candidates.is_empty() {
        return Err(CmdError::usage(format!(
            "path {arg} is outside the repository at {}",
            repo.root.display()
        )));
    }
    if candidates
        .iter()
        .any(|candidate| repo.absolute(candidate).symlink_metadata().is_ok())
    {
        return Err(CmdError::usage(format!(
            "path {arg} is inside the repository but not part of its Git-visible tree \
             (missing, ignored, or excluded)"
        )));
    }

    Err(CmdError::usage(format!("path {arg} does not exist")))
}

/// Repository-root reading of a relative argument: the path as written with `.` and empty
/// components dropped; `None` when it escapes the root or is not a plain relative path.
fn root_relative(arg: &str) -> Option<String> {
    if arg.starts_with('/') || arg.contains('\\') {
        return None;
    }
    let mut parts = Vec::new();
    for part in arg.split('/') {
        match part {
            "" | "." => continue,
            ".." => return None,
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

/// Repository-root relative form of `path`, when it lies inside `root`.
fn relative_to(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    if rel.as_os_str().is_empty() {
        return Some(".".to_string());
    }
    let text = rel.to_str()?;
    Some(text.to_string())
}

/// Normalize a repository-relative path from an import record.
fn normalize_relative_path(raw: &str) -> Result<String, CmdError> {
    if raw.is_empty() {
        return Err(CmdError::usage("path must not be empty"));
    }
    if raw.contains('\0') {
        return Err(CmdError::usage("path must not contain a NUL byte"));
    }
    if raw.starts_with('/') {
        return Err(CmdError::usage(format!(
            "path {raw} must be repository-root relative, not absolute"
        )));
    }
    if raw.contains('\\') {
        return Err(CmdError::usage(format!(
            "path {raw} must use forward slashes"
        )));
    }
    if raw == "." {
        return Ok(".".to_string());
    }
    let mut parts = Vec::new();
    for part in raw.split('/') {
        match part {
            "" => {
                return Err(CmdError::usage(format!(
                    "path {raw} has an empty component"
                )))
            }
            "." | ".." => {
                return Err(CmdError::usage(format!(
                    "path {raw} contains a {part} component"
                )))
            }
            other => parts.push(other),
        }
    }
    Ok(parts.join("/"))
}

/// Validate a note: nonempty, single line.
fn validate_note(raw: &str) -> Result<String, CmdError> {
    let mut note = raw.to_string();
    while note.ends_with('\n') || note.ends_with('\r') {
        note.pop();
    }
    if note.trim().is_empty() {
        return Err(CmdError::usage("note must not be empty"));
    }
    if note.contains('\n') || note.contains('\r') {
        return Err(CmdError::usage(
            "note must be a single line; newlines are not allowed",
        ));
    }
    Ok(note)
}

/// Lexically normalize a path (no symlink resolution, `..` applied textually).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Absolute form of `path`, resolving symlinks in the deepest existing ancestor.
fn absolute_path(path: &Path) -> Result<PathBuf, CmdError> {
    let base = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| CmdError::from_context("cannot read the current directory", e))?
            .join(path)
    };
    let base = normalize(&base);
    if base.exists() {
        return base
            .canonicalize()
            .map_err(|e| CmdError::from_context(&format!("cannot resolve {}", base.display()), e));
    }
    let mut missing: Vec<OsString> = Vec::new();
    let mut current = base.clone();
    while !current.exists() {
        match current.file_name() {
            Some(name) => missing.push(name.to_os_string()),
            None => break,
        }
        if !current.pop() {
            break;
        }
    }
    let mut resolved = current
        .canonicalize()
        .map_err(|e| CmdError::from_context(&format!("cannot resolve {}", base.display()), e))?;
    for part in missing.iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

/// Database file plus its SQLite sidecars; these are never part of the tree.
fn db_artifacts(base: &Path) -> Vec<PathBuf> {
    let mut paths = vec![base.to_path_buf()];
    let text = base.to_string_lossy().to_string();
    paths.push(PathBuf::from(format!("{text}-wal")));
    paths.push(PathBuf::from(format!("{text}-shm")));
    paths
}

fn repo_json(ctx: &Ctx) -> RepoJson {
    RepoJson {
        identity: ctx.repo.identity.clone(),
        root: ctx.repo.root.to_string_lossy().to_string(),
        common_dir: ctx.repo.common_dir.to_string_lossy().to_string(),
    }
}
