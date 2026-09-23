//! Command line surface: global options, the subcommands, and their exit codes.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use serde::Deserialize;

use crate::ast;
use crate::output::{
    build_tree, compare_children_first, depth_key, ComparedStateJson, EntryJson, EntryView,
    Envelope, MemberJson, MemberView, RepoJson, ScopeJson, StateChangeJson, StateJson, Status,
    TreeChild,
};
use crate::repo::{in_scope, parent_path, state_hash, Entry, Kind, Repo};
use crate::store::{
    default_db_path, CachedMember, MemberVersion, NewMemberNote, NewNote, NoteVersion, Store,
};
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
    /// List entries, and with `--members` their declarations, that are missing a note or stale.
    Pending(PendingArgs),
    /// Show the tree, or one file or directory subtree, with note freshness.
    Read(ReadArgs),
    /// Annotate the current version of one file, directory, symlink or submodule.
    Set(SetArgs),
    /// Annotate the current version of one AST member (declaration) inside a file.
    MemberSet(MemberSetArgs),
    /// Import a JSON batch of {path, hash, note} records atomically.
    Import(ImportArgs),
    /// Report the hash of the current repository state and what changed since a recorded state.
    State(StateArgs),
}

#[derive(Debug, Args)]
struct PendingArgs {
    /// Restrict the listing to this path or subtree (repository-root relative).
    path: Option<String>,
    /// Also list the missing or stale AST members of the scoped files.
    #[arg(long)]
    members: bool,
    /// Keep only these directories and their contents (repeatable; `--only src,lib` works too).
    #[arg(long = "only", value_name = "DIR", value_delimiter = ',')]
    only: Vec<String>,
    /// List the whole scope even when `--only` was given; conflicts with `--only`.
    #[arg(long, conflicts_with = "only")]
    all: bool,
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
    /// List the annotatable AST members of the scoped files.
    #[arg(long)]
    members: bool,
    /// Keep only these directories and their contents (repeatable; `--only src,lib` works too).
    #[arg(long = "only", value_name = "DIR", value_delimiter = ',')]
    only: Vec<String>,
    /// Show the whole scope even when `--only` was given; conflicts with `--only`.
    #[arg(long, conflicts_with = "only")]
    all: bool,
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
    /// Also re-note every stale member of the scope with that member's own previous note text.
    #[arg(long)]
    ast: bool,
    /// Emit the versioned JSON envelope instead of text.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct MemberSetArgs {
    /// File holding the declaration (repository-root relative).
    path: String,
    /// Symbol key of the declaration, e.g. `method:Cache.put:0`.
    symbol: String,
    /// One-line note text; when omitted the note is read from stdin.
    #[arg(long, value_name = "TEXT")]
    note: Option<String>,
    /// Reject the write unless the current member hash is exactly this value.
    #[arg(long, value_name = "HASH")]
    expected_hash: Option<String>,
    /// Emit the versioned JSON envelope instead of text.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct StateArgs {
    /// Compare against this recorded state hash instead of the most recently recorded state.
    #[arg(long, value_name = "HASH")]
    compare: Option<String>,
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
        Command::Pending(args) => cmd_pending(&mut ctx, args),
        Command::Read(args) => cmd_read(&mut ctx, args),
        Command::Set(args) => cmd_set(&mut ctx, args),
        Command::MemberSet(args) => cmd_member_set(&mut ctx, args),
        Command::Import(args) => cmd_import(&mut ctx, args),
        Command::State(args) => cmd_state(&mut ctx, args),
    }
}

fn cmd_pending(ctx: &mut Ctx, args: PendingArgs) -> Result<(), CmdError> {
    let scope = scope_of(ctx, args.path.as_deref())?;
    let filters = PathFilters::resolve(ctx, &scope, &args.only, args.all)?;
    let mut views = scoped_views_in(ctx, &scope)?;
    views.retain(|view| view.status != Status::Fresh);
    if let Some(filters) = &filters {
        filters.retain_views(&mut views);
    }
    views.sort_by(|a, b| compare_children_first(&a.path, &b.path));

    // `--members` adds the declaration level below the tree level: a declaration whose note is
    // missing or belongs to older content is unfinished work too, and without this flag the only
    // way to see it would be a `read --members` that lists every fresh declaration as well. The
    // tree listing itself is identical with and without the flag.
    let mut parse_error = false;
    let mut members: Vec<MemberView> = Vec::new();
    if args.members {
        members = scoped_member_views(ctx, &scope, filters.as_ref(), &mut parse_error)?;
        members.retain(|member| member.status != Status::Fresh);
        // Shallowest file first, then file order, then document order inside a file: the same
        // coarse-to-fine reading the entry listing gives.
        members.sort_by(|a, b| {
            depth_key(&scope.path, &a.path)
                .cmp(&depth_key(&scope.path, &b.path))
                .then(a.path.cmp(&b.path))
                .then(a.start_line.cmp(&b.start_line))
                .then(a.symbol.cmp(&b.symbol))
        });
    }

    if args.json {
        let mut envelope = Envelope::new("pending", repo_json(ctx));
        envelope.scope = Some(ScopeJson {
            path: scope.path.clone(),
            kind: scope.kind.as_str().to_string(),
            depth: None,
            filters: filters.as_ref().map(PathFilters::names),
        });
        envelope.entries = views.iter().map(EntryJson::from).collect();
        envelope.members = members.iter().map(MemberJson::from).collect();
        if args.members {
            envelope.parse_error = Some(parse_error);
        }
        return envelope.print();
    }

    // The text tree is drawn parent first so a file always sits above its declarations and a
    // directory above its files; the JSON `entries` array keeps its documented children-first
    // order, so `--json` output is unchanged by this rendering.
    let mut by_path = views.clone();
    by_path.sort_by(|a, b| a.path.cmp(&b.path));
    crate::output::print_lines(&tree_lines(&scope, &by_path, &members)?)
}

fn cmd_read(ctx: &mut Ctx, args: ReadArgs) -> Result<(), CmdError> {
    // `--members` may fill the derived member cache, so the whole command takes a mutable context
    // even though the note tables are only read.
    let scope = scope_of(ctx, args.path.as_deref())?;
    let filters = PathFilters::resolve(ctx, &scope, &args.only, args.all)?;
    let mut views = scoped_views_in(ctx, &scope)?;
    if let Some(depth) = args.depth {
        views.retain(|view| depth_key(&scope.path, &view.path) <= depth);
    }
    if let Some(filters) = &filters {
        filters.retain_views(&mut views);
    }
    views.sort_by(|a, b| a.path.cmp(&b.path));

    let mut parse_error = false;
    let mut members: Vec<MemberView> = Vec::new();
    if args.members {
        members = scoped_member_views(ctx, &scope, filters.as_ref(), &mut parse_error)?;
        if let Some(depth) = args.depth {
            members.retain(|member| depth_key(&scope.path, &member.path) <= depth);
        }
        members.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then(a.start_line.cmp(&b.start_line))
                .then(a.symbol.cmp(&b.symbol))
        });
    }

    if args.json {
        let mut envelope = Envelope::new("read", repo_json(ctx));
        envelope.scope = Some(ScopeJson {
            path: scope.path.clone(),
            kind: scope.kind.as_str().to_string(),
            depth: args.depth,
            filters: filters.as_ref().map(PathFilters::names),
        });
        envelope.entries = views.iter().map(EntryJson::from).collect();
        envelope.members = members.iter().map(MemberJson::from).collect();
        if args.members {
            envelope.parse_error = Some(parse_error);
        }
        return envelope.print();
    }

    crate::output::print_lines(&tree_lines(&scope, &views, &members)?)
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

    let scope = scope_of(ctx, Some(&args.path))?;
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

    // `--ast` bulk re-note: a stale member whose note text is already known is re-noted for its
    // current hash, carrying the same text forward. Members that were never annotated keep
    // `missing`; this never invents a note.
    let mut parse_error = false;
    let mut ast_notes: Vec<NewMemberNote> = Vec::new();
    if args.ast {
        if entry.kind != Kind::File {
            return Err(CmdError::usage(format!(
                "--ast re-notes the members of one file; {} is a {}",
                entry.path,
                entry.kind.as_str()
            )));
        }
        let members = parse_members_of(ctx, &entry.path, &mut parse_error)?;
        let versions = ctx.store.load_member_versions(&ctx.repo.identity)?;
        let stored = versions.get(&entry.path);
        for member in members {
            let Some(history) = stored.and_then(|stored| stored.get(&member.symbol)) else {
                continue;
            };
            if history.iter().any(|version| version.hash == member.hash) {
                continue; // already fresh for this exact content
            }
            if let Some(previous) = history.first() {
                ast_notes.push(new_member_note(&member, &entry.path, &previous.note));
            }
        }
        for member_note in &ast_notes {
            ctx.store
                .write_member_note(&ctx.repo.identity, member_note)?;
        }
    }
    let ast_suffix = match ast_notes.len() {
        0 => String::new(),
        count => format!("; re-noted {count} stale member(s)"),
    };

    if args.json {
        let mut envelope = Envelope::new("set", repo_json(ctx));
        envelope.scope = Some(ScopeJson {
            path: entry.path.clone(),
            kind: entry.kind.as_str().to_string(),
            depth: None,
            filters: None,
        });
        envelope.message = Some(format!("annotated {} (fresh){ast_suffix}", entry.path));
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
        envelope.members = ast_notes.iter().map(MemberJson::from_new).collect();
        if args.ast {
            envelope.parse_error = Some(parse_error);
        }
        return envelope.print();
    }

    crate::output::print_lines(&[format!(
        "annotated {} [{}] fresh {}{}",
        entry.path,
        entry.kind.as_str(),
        entry.hash,
        ast_suffix
    )])
}

fn cmd_member_set(ctx: &mut Ctx, args: MemberSetArgs) -> Result<(), CmdError> {
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

    let scope = scope_of(ctx, Some(&args.path))?;
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
    if entry.kind != Kind::File {
        return Err(CmdError::usage(format!(
            "{} is a {}, not a source file with AST members",
            entry.path,
            entry.kind.as_str()
        )));
    }
    if ast::language_for_path(&entry.path).is_none() {
        return Err(CmdError::usage(format!(
            "no AST adapter for {}; member notes need a supported source file",
            entry.path
        )));
    }
    let mut parse_error = false;
    let members = parse_members_of(ctx, &entry.path, &mut parse_error)?;
    let member = members
        .iter()
        .find(|member| member.symbol == args.symbol)
        .ok_or_else(|| {
            CmdError::usage(format!(
                "{} has no member {}; run `treenotes read {} --members` to list them",
                entry.path, args.symbol, entry.path
            ))
        })?;
    if let Some(expected) = &args.expected_hash {
        if expected != &member.hash {
            return Err(CmdError::usage(format!(
                "expected hash {} does not match the current hash {} of {} in {}",
                expected, member.hash, member.symbol, entry.path
            )));
        }
    }

    let written = new_member_note(member, &entry.path, &note);
    ctx.store.write_member_note(&ctx.repo.identity, &written)?;

    if args.json {
        let mut envelope = Envelope::new("member-set", repo_json(ctx));
        envelope.scope = Some(ScopeJson {
            path: entry.path.clone(),
            kind: entry.kind.as_str().to_string(),
            depth: None,
            filters: None,
        });
        envelope.parse_error = Some(parse_error);
        envelope.message = Some(format!(
            "annotated member {} of {} (fresh)",
            member.symbol, entry.path
        ));
        envelope.members = vec![MemberJson::from_new(&written)];
        return envelope.print();
    }

    crate::output::print_lines(&[format!(
        "annotated {} [{}] fresh {}",
        member.symbol, member.symbol_kind, member.hash
    )])
}

/// Parse one repository-root relative source file into its members.
///
/// An unsupported extension is not an error: the file simply has no members. `parse_error` is set
/// when the grammar had to recover from a syntax error; a file that cannot be read as UTF-8 is an
/// environment failure.
///
/// The result is cached in the derived `member_index` tables under the file's own content hash, so
/// a file whose bytes have not changed is never parsed twice — parsing is the expensive part of
/// `read --members`, and the cache is what makes repeating it cheap. A cache hit is not an
/// assumption about the bytes: it is keyed by the hash the inventory just computed for the bytes on
/// disk, so an edited file misses and is parsed again.
fn parse_members_of(
    ctx: &mut Ctx,
    path: &str,
    parse_error: &mut bool,
) -> Result<Vec<ast::Member>, CmdError> {
    let Some(language) = ast::language_for_path(path) else {
        return Ok(Vec::new());
    };
    let file_hash = ctx
        .entries
        .iter()
        .find(|entry| entry.path == path)
        .map(|entry| entry.hash.clone())
        .ok_or_else(|| CmdError::usage(format!("{path} is not part of the Git-visible tree")))?;
    if let Some((cached_error, cached)) =
        ctx.store
            .load_member_index(&ctx.repo.identity, path, &file_hash)?
    {
        *parse_error |= cached_error;
        return Ok(cached.iter().map(member_from_cached).collect());
    }
    let absolute = ctx.repo.absolute(path);
    let source = ast::read_source(&absolute, path)?;
    let parsed = ast::parse_members(language, &source)?;
    *parse_error |= parsed.parse_error;
    let cached: Vec<CachedMember> = parsed.members.iter().map(cached_member_of).collect();
    ctx.store.store_member_index(
        &ctx.repo.identity,
        path,
        &file_hash,
        parsed.parse_error,
        &cached,
    )?;
    Ok(parsed.members)
}

/// The cache record for a freshly parsed member.
fn cached_member_of(member: &ast::Member) -> CachedMember {
    CachedMember {
        symbol: member.symbol.clone(),
        symbol_kind: member.symbol_kind.clone(),
        name: member.name.clone(),
        qualified_name: member.qualified_name.clone(),
        start_line: member.start_line,
        end_line: member.end_line,
        hash: member.hash.clone(),
    }
}

/// The member a cache record describes, indistinguishable from the parsed original.
fn member_from_cached(cached: &CachedMember) -> ast::Member {
    ast::Member {
        symbol: cached.symbol.clone(),
        symbol_kind: cached.symbol_kind.clone(),
        name: cached.name.clone(),
        qualified_name: cached.qualified_name.clone(),
        start_line: cached.start_line,
        end_line: cached.end_line,
        hash: cached.hash.clone(),
    }
}

/// Every AST member of the scoped files, paired with the stored member notes of its symbol.
fn scoped_member_views(
    ctx: &mut Ctx,
    scope: &Scope,
    filters: Option<&PathFilters>,
    parse_error: &mut bool,
) -> Result<Vec<MemberView>, CmdError> {
    let versions = ctx.store.load_member_versions(&ctx.repo.identity)?;
    let no_notes: BTreeMap<String, Vec<MemberVersion>> = BTreeMap::new();
    let no_versions: Vec<MemberVersion> = Vec::new();
    // Only regular files are source: symlinks, submodules and directories are never parsed. The
    // paths are collected first so parsing (which fills the cache through `ctx`) can borrow the
    // context mutably while this loop walks an owned list. A file the `--only` window excludes is
    // not parsed at all, and a syntax error in it cannot mark the listing untrustworthy.
    let paths: Vec<String> = ctx
        .entries
        .iter()
        .filter(|entry| entry.kind == Kind::File)
        .filter(|entry| in_scope(&entry.path, &scope.path, scope.kind))
        .filter(|entry| match filters {
            Some(filters) => filters.keeps(&entry.path),
            None => true,
        })
        .map(|entry| entry.path.clone())
        .collect();
    let mut views = Vec::new();
    for path in paths {
        let members = parse_members_of(ctx, &path, parse_error)?;
        let stored = versions.get(&path).unwrap_or(&no_notes);
        for member in &members {
            let history = stored.get(&member.symbol).unwrap_or(&no_versions);
            views.push(MemberView::build(&path, member, history));
        }
    }
    Ok(views)
}

/// Build the member-note record for one freshly parsed member.
fn new_member_note(member: &ast::Member, path: &str, note: &str) -> NewMemberNote {
    NewMemberNote {
        path: path.to_string(),
        symbol: member.symbol.clone(),
        symbol_kind: member.symbol_kind.clone(),
        name: member.name.clone(),
        qualified_name: member.qualified_name.clone(),
        start_line: member.start_line,
        end_line: member.end_line,
        hash: member.hash.clone(),
        note: note.to_string(),
    }
}

/// Report the aggregate hash of the current tree state, whether this build has already computed
/// it, how much of the AST member cache survives, and what changed since a recorded state.
///
/// The state hash is a pure function of the inventory, so an unchanged checkout always reports the
/// same hash — including on another worktree or after a branch switch that lands on identical
/// bytes. Recording it (once: an already-known state is not rewritten) is what lets a later run
/// name exactly which files changed instead of re-reading the tree.
fn cmd_state(ctx: &mut Ctx, args: StateArgs) -> Result<(), CmdError> {
    let current = state_hash(&ctx.entries);
    let commit = ctx.repo.head_commit()?;
    let known = ctx
        .store
        .snapshot_info(&ctx.repo.identity, &current)?
        .is_some();

    // Member-cache accounting: one database query, then one pass over the inventory.
    let cached = ctx.store.member_index_keys(&ctx.repo.identity)?;
    let mut member_cache_hits = 0_usize;
    let mut member_cache_misses = 0_usize;
    for entry in &ctx.entries {
        if entry.kind != Kind::File || ast::language_for_path(&entry.path).is_none() {
            continue;
        }
        if cached.contains(&(entry.path.clone(), entry.hash.clone())) {
            member_cache_hits += 1;
        } else {
            member_cache_misses += 1;
        }
    }

    let compared =
        match &args.compare {
            Some(state) => Some(ctx.store.snapshot(&ctx.repo.identity, state)?.ok_or_else(
                || {
                    CmdError::usage(format!(
                        "state {state} has never been recorded by this database; run `treenotes \
                         state` on the checkout you want to compare with"
                    ))
                },
            )?),
            None => ctx
                .store
                .latest_snapshot(&ctx.repo.identity, Some(&current))?,
        };
    let changes = match &compared {
        Some((_, entries)) => state_changes(&ctx.entries, entries),
        None => Vec::new(),
    };
    // Only a state this build has not seen is written: an unchanged `state` run stays read-only,
    // and the commit a state was first observed at is not rewritten by later observations.
    let recorded = if known {
        false
    } else {
        ctx.store.record_snapshot(
            &ctx.repo.identity,
            &current,
            commit.as_deref(),
            &ctx.entries,
        )?;
        true
    };

    if args.json {
        let mut envelope = Envelope::new("state", repo_json(ctx));
        envelope.state = Some(StateJson {
            state_hash: current,
            commit,
            known,
            recorded,
            member_cache_hits,
            member_cache_misses,
            compared_state: compared.as_ref().map(|(info, _)| ComparedStateJson {
                state_hash: info.state_hash.clone(),
                commit: info.commit.clone(),
                updated_at: info.updated_at.clone(),
            }),
            changes,
        });
        return envelope.print();
    }

    let mut lines = vec![format!(
        "state {} [{}] {}",
        short_hash(&current),
        if known { "known" } else { "new" },
        match &commit {
            Some(commit) => format!("commit {}", short_hash(commit)),
            None => "no commit yet".to_string(),
        }
    )];
    lines.push(format!(
        "member cache {}/{} source file(s) parsed",
        member_cache_hits,
        member_cache_hits + member_cache_misses
    ));
    match &compared {
        Some((info, _)) => {
            lines.push(format!(
                "compared with {} ({} change(s))",
                short_hash(&info.state_hash),
                changes.len()
            ));
            for change in &changes {
                lines.push(format!(
                    "  {} [{}] {}{}",
                    change.path,
                    change.kind,
                    change.change,
                    match &change.hash {
                        Some(hash) => format!(" {}", short_hash(hash)),
                        None => String::new(),
                    }
                ));
            }
        }
        None => lines.push("no earlier recorded state to compare with".to_string()),
    }
    crate::output::print_lines(&lines)
}

/// Entry-level differences between the current tree and one recorded state.
///
/// Directories are skipped: a directory hash is a function of its children's hashes, so a changed
/// directory is always accompanied by a changed leaf and would only add noise. Paths are compared,
/// not positions, so a rename is one removal plus one addition and never a rebinding.
fn state_changes(current: &[Entry], recorded: &[(String, String, String)]) -> Vec<StateChangeJson> {
    let previous: BTreeMap<&str, (&str, &str)> = recorded
        .iter()
        .map(|(path, kind, hash)| (path.as_str(), (kind.as_str(), hash.as_str())))
        .collect();
    let mut changes = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for entry in current {
        seen.insert(entry.path.as_str());
        if entry.kind == Kind::Dir {
            continue;
        }
        let kind = entry.kind.as_str();
        match previous.get(entry.path.as_str()).copied() {
            None => changes.push(StateChangeJson {
                path: entry.path.clone(),
                kind: kind.to_string(),
                change: "added".to_string(),
                hash: Some(entry.hash.clone()),
                previous_hash: None,
                previous_kind: None,
            }),
            Some((old_kind, old_hash)) => {
                if old_kind == kind && old_hash == entry.hash {
                    continue;
                }
                let kind_changed = old_kind != kind;
                changes.push(StateChangeJson {
                    path: entry.path.clone(),
                    kind: kind.to_string(),
                    change: if kind_changed {
                        "kind-changed"
                    } else {
                        "modified"
                    }
                    .to_string(),
                    hash: Some(entry.hash.clone()),
                    previous_hash: Some(old_hash.to_string()),
                    previous_kind: kind_changed.then(|| old_kind.to_string()),
                });
            }
        }
    }
    for (path, kind, hash) in recorded {
        if kind == Kind::Dir.as_str() || seen.contains(path.as_str()) {
            continue;
        }
        changes.push(StateChangeJson {
            path: path.clone(),
            kind: kind.clone(),
            change: "removed".to_string(),
            hash: None,
            previous_hash: Some(hash.clone()),
            previous_kind: None,
        });
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    changes
}

/// Hex part of a hash string, truncated for display.
fn short_hash(hash: &str) -> String {
    crate::output::hex_of(hash).chars().take(12).collect()
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

/// One nested ASCII tree of a listing: every entry with the declarations that belong to it.
///
/// Both slices must be in path order, with each file's declarations grouped and kept in document
/// order. Anything that only *locates* the rest — an ancestor directory, or a file that holds
/// listed declarations but is not itself unfinished — is drawn as `(path) [context]` without a
/// hash or a status, so a fresh file is never presented as pending work.
fn tree_lines(
    scope: &Scope,
    views: &[EntryView],
    members: &[MemberView],
) -> Result<Vec<String>, CmdError> {
    let mut listed: BTreeMap<&str, &EntryView> = BTreeMap::new();
    for view in views {
        listed.insert(view.path.as_str(), view);
    }
    let mut grouped: BTreeMap<&str, Vec<&MemberView>> = BTreeMap::new();
    for member in members {
        grouped
            .entry(member.path.as_str())
            .or_default()
            .push(member);
    }
    // The listing must be drawn in tree order: an ancestor above every descendant, siblings in
    // name order. A plain byte sort of the paths is not that order — `/` sorts after `.`, so a
    // sibling whose name extends an ancestor's (`src/cli/marketplaces.rs` beside the directory
    // `src/cli/marketplaces`) would come *before* the directory and, being no descendant of it,
    // push the directory's own children below a node `build_tree` has already popped off its
    // level stack. Comparing component by component keeps shared prefixes together while every
    // ancestor still precedes all of its descendants.
    let mut paths: Vec<&str> = listed.keys().copied().collect();
    paths.extend(grouped.keys().copied());
    paths.sort_by(|a, b| a.split('/').cmp(b.split('/')));
    paths.dedup();

    let mut children: Vec<(Option<String>, TreeChild)> = Vec::new();
    // A label per path that has already been drawn, so a child can name the node it hangs from.
    let mut labels: BTreeMap<&str, &str> = BTreeMap::new();
    match listed.get(scope.path.as_str()) {
        Some(view) => {
            children.push((None, TreeChild::Entry((*view).clone())));
            if let Some(previous) = view.previous_body() {
                children.push((Some(view.path.clone()), TreeChild::Detail(previous)));
            }
        }
        None => children.push((None, TreeChild::Context(scope.path.clone()))),
    }
    labels.insert(scope.path.as_str(), scope.path.as_str());

    for path in paths {
        if path != scope.path.as_str() {
            match listed.get(path) {
                Some(view) => {
                    children.push((
                        nearest_label(&labels, path),
                        TreeChild::Entry((*view).clone()),
                    ));
                    labels.insert(path, path);
                    if let Some(previous) = view.previous_body() {
                        children.push((Some(path.to_string()), TreeChild::Detail(previous)));
                    }
                }
                None => {
                    // A declaration whose file is not itself listed: draw the file and every
                    // ancestor that is missing as context, top down.
                    let mut chain: Vec<&str> = vec![path];
                    let mut parent = parent_path(path);
                    while !labels.contains_key(parent) {
                        chain.push(parent);
                        parent = parent_path(parent);
                    }
                    for context in chain.iter().rev() {
                        children.push((
                            nearest_label(&labels, context),
                            TreeChild::Context((*context).to_string()),
                        ));
                        labels.insert(context, context);
                    }
                }
            }
        }
        if let Some(group) = grouped.get(path) {
            let owner = labels.get(path).copied().unwrap_or(scope.path.as_str());
            for member in group {
                children.push((
                    Some(owner.to_string()),
                    TreeChild::Member((*member).clone()),
                ));
                if let Some(previous) = member.previous_body() {
                    children.push((Some(member.symbol.clone()), TreeChild::Detail(previous)));
                }
            }
        }
    }
    // A scope that needs nothing drawn is not a tree: printing only the scope as context would
    // turn an empty `pending` into a line that looks like an entry.
    if children.len() == 1 && matches!(children[0].1, TreeChild::Context(_)) {
        return Ok(Vec::new());
    }
    build_tree(&children)
}

/// Label of the closest already drawn ancestor of `path`.
fn nearest_label<'a>(labels: &BTreeMap<&'a str, &'a str>, path: &str) -> Option<String> {
    let mut parent = parent_path(path);
    loop {
        if let Some(label) = labels.get(parent) {
            return Some((*label).to_string());
        }
        if parent == "." {
            return None;
        }
        parent = parent_path(parent);
    }
}

/// Resolve a command's path argument to its scope; the whole repository when none was given.
fn scope_of(ctx: &Ctx, path: Option<&str>) -> Result<Scope, CmdError> {
    match path {
        Some(path) => resolve_scope(&ctx.repo, &ctx.entries, path),
        None => Ok(Scope {
            path: ".".to_string(),
            kind: Kind::Dir,
        }),
    }
}

/// A resolved `--only` window: the directories whose subtrees stay in a listing.
///
/// A filter is a *window over the scope*, never a second scope. It only ever removes entries the
/// scope would have listed, so `read src --depth 2 --only lib` still counts depth from `src` and a
/// path keeps its repository-root relative spelling. That is the point of the flag: a repository
/// with thirty top-level directories is read one subtree at a time instead of drowning the reader
/// in entries nobody asked about. The scope itself and the directories between it and a named
/// directory stay, so a windowed tree still hangs from its scope line instead of starting mid-air.
struct PathFilters {
    /// Named directories, repository-root relative, deduplicated and in name order.
    only: Vec<String>,
    /// The scope, every named directory, and the directories between them.
    nodes: BTreeSet<String>,
}

impl PathFilters {
    /// Resolve `--only` arguments against `scope`; `None` when no filter applies.
    fn resolve(
        ctx: &Ctx,
        scope: &Scope,
        only: &[String],
        all: bool,
    ) -> Result<Option<PathFilters>, CmdError> {
        if all || only.is_empty() {
            return Ok(None);
        }
        let mut named: BTreeSet<String> = BTreeSet::new();
        for arg in only {
            let dir = scope_of(ctx, Some(arg.as_str()))?;
            if dir.kind != Kind::Dir {
                return Err(CmdError::usage(format!(
                    "--only takes a directory; {} is a {}",
                    dir.path,
                    dir.kind.as_str()
                )));
            }
            if !in_scope(&dir.path, &scope.path, scope.kind) {
                return Err(CmdError::usage(format!(
                    "--only {} is outside the scope {}",
                    dir.path, scope.path
                )));
            }
            named.insert(dir.path);
        }
        let mut nodes = named.clone();
        for dir in &named {
            let mut current = dir.as_str();
            while current != scope.path.as_str() {
                current = parent_path(current);
                nodes.insert(current.to_string());
                if current == "." {
                    break;
                }
            }
        }
        Ok(Some(PathFilters {
            only: named.into_iter().collect(),
            nodes,
        }))
    }

    /// The named directories, as reported by `--json`.
    fn names(&self) -> Vec<String> {
        self.only.clone()
    }

    /// True when `path` is inside a named directory, or is one of the directories between the
    /// scope and a named directory. `path` is repository-root relative.
    fn keeps(&self, path: &str) -> bool {
        if self.nodes.contains(path) {
            return true;
        }
        self.only
            .iter()
            .any(|dir| dir == "." || path.starts_with(&format!("{dir}/")))
    }

    /// Drop the entries the window excludes.
    fn retain_views(&self, views: &mut Vec<EntryView>) {
        views.retain(|view| self.keeps(&view.path));
    }
}

/// Current entries within an already resolved scope, paired with their stored note versions.
fn scoped_views_in(ctx: &Ctx, scope: &Scope) -> Result<Vec<EntryView>, CmdError> {
    let versions = ctx.store.load_versions(&ctx.repo.identity)?;
    let empty: Vec<NoteVersion> = Vec::new();
    let mut views = Vec::new();
    for entry in &ctx.entries {
        if in_scope(&entry.path, &scope.path, scope.kind) {
            let stored = versions.get(&entry.path).unwrap_or(&empty);
            views.push(EntryView::build(entry, stored));
        }
    }
    Ok(views)
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
