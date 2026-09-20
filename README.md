# treenotes

Content-versioned notes for the files and directories of a Git repository, built for external
LLM agents.

`treenotes` gives an agent a compact, annotated map of a working tree without loading the source
into its context. It hashes the *working-tree bytes* of every Git-visible file (tracked and
untracked-but-not-ignored), stores one-line notes keyed by `repository + path + kind + content
hash` in a local SQLite database, and reports exactly what still needs attention: freshly
annotated entries, entries whose note belongs to an older content version (`stale`), and entries
with no note at all (`missing`).

**No AI integration.** `treenotes` never calls a model, classifier, adapter or network service.
An external agent reads the source itself, writes notes with `set`/`import`, and can hand the
exported JSON to whatever relevance classifier it likes (`jev`, or anything else).

## Install

```console
$ cargo install --path .          # or: cargo build --release && cp target/release/treenotes ~/.local/bin/
$ treenotes --help
```

Build requirements: a Rust toolchain (Rust **1.85 or newer**, the manifest's `rust-version`, which
the dependency set requires) and `git` on `PATH`. SQLite is bundled through
`rusqlite`, so no system SQLite is needed.

## Command reference

```
treenotes [--repo DIR] [--db PATH] <COMMAND>

treenotes pending    [PATH] [--members] [--json]
treenotes read       [PATH] [--depth N] [--members] [--json]
treenotes set        PATH [--note TEXT] [--expected-hash HASH] [--ast] [--json]
treenotes member-set PATH SYMBOL [--note TEXT] [--expected-hash HASH] [--json]
treenotes import     [FILE|-] [--json]
treenotes state      [--compare HASH] [--json]
```

Global options:

| Option | Meaning |
| --- | --- |
| `--repo DIR` | Work in the repository containing `DIR` (default: the current directory). |
| `--db PATH` | Notes database (default: `~/.tree-notes/notes.sqlite3`). |

Commands:

* `pending [PATH]` — list entries under `PATH` (default: the whole repository) that are `missing`
  or `stale`. The current hash is always shown. Children come before their parents so an agent can
  summarize files first and directories afterwards. Stale entries also show the previous note,
  always labelled as *not current*, together with the hash that note belongs to. Exits 0 even when
  the list is empty.
* `pending [PATH] --members` — additionally list the `missing` or `stale` declarations inside the
  scoped source files (the same members `read --members` shows, minus the fresh ones). Entries and
  members are one list: every entry, then every pending declaration in shallowest-file-first, then
  file, then document order. `parse_error` is reported for the scope exactly as in `read --members`.
  This is how an agent finds unannotated methods: a declaration is never mixed into `entries`, and
  without a pending declaration the members array stays empty.
* `read [PATH] [--depth N]` — show the tree, or exactly one file, or one directory subtree, with
  freshness for every entry. `--depth 0` shows only the scope itself. Output is an indented text
  map by default, or a versioned JSON envelope with `--json`. Root (`.`) and unannotated entries
  are always included so coverage gaps stay visible.
* `set PATH --note TEXT [--expected-hash HASH]` — annotate the *current* version of `PATH`. With no
  `--note`, the one-line note is read from stdin (`printf 'summary\n' | treenotes set src/lib.rs`),
  which avoids shell-quoting friction. `--expected-hash` refuses the write unless the current hash
  still matches, guarding against annotating content that changed while the agent was reading it.
* `read PATH --members` — additionally list the annotatable declarations *inside* the scoped source
  files (Java, Rust, TypeScript/TSX, JavaScript and Python). Each member carries its symbol key
  (`<kind>:<qualified name>:<ordinal>`), its declared and qualified names, its line span, its own
  `tnt2` hash and its own note status. Scoped non-source files (symlinks, submodules, directories,
  unsupported extensions) contribute nothing. When a grammar has to recover from a syntax error the
  members recovered so far are still listed and `parse_error` is `true`, so an agent can distrust
  the list instead of seeing an empty one.
* `member-set PATH SYMBOL --note TEXT` — annotate the *current* version of one declaration. With no
  `--note` the text is read from stdin, as for `set`. `--expected-hash` refuses the write unless the
  member's current hash still matches, and an unknown symbol (or a file with no AST adapter) is an
  invalid-input error with the exit code `set` uses. Member notes are independent of file notes:
  annotating a method does not annotate its file.
* `set PATH --ast` — after annotating the file itself, re-note every *stale* member of that file
  with the text of that member's own most recent stored note. Members that were never annotated
  stay `missing`: `--ast` carries existing summaries forward, it never invents one. `--ast` needs
  exactly one file, so it is rejected for a directory, symlink or submodule.
* `import [FILE|-]` — read a JSON array of `{ "path", "hash", "note" }` records (from a file, `-`,
  or stdin when omitted). Every record is validated first: the array must be non-empty, paths must
  be repository-root relative with no `.`/`..`/empty components, duplicates after normalization are
  rejected, notes must be non-empty single lines, and every `hash` must equal the *current* hash of
  that path. Only then is the whole batch written in one transaction — one bad record means no
  notes at all are written.
* `state [--compare HASH]` — report the `tnt1:state` hash of the whole current working tree, the
  commit it was observed at, whether this database had already recorded that exact state, and how
  much of the AST member cache would survive a rebuild. By default it is compared against the most
  recently recorded *different* state; `--compare HASH` compares against one named recorded state.
  The comparison lists only real leaves: `added`, `modified`, `kind-changed` (a file replaced by a
  symlink, say) and `removed`, ordered by path. A state this database has not seen is recorded (once)
  with the current commit and a full entry list, so later runs can name exactly what changed without
  re-reading anything; re-observing a known state writes nothing. Directories are omitted from the
  change list because a directory hash moves exactly when one of its leaves does.

### Path arguments

Paths are **strictly repository-root relative** (`src/lib.rs`, `.` for the root), regardless of the
directory the command runs in. A relative argument is never re-interpreted against the caller's
working directory, so the same spelling names the same entry from anywhere: with a root `a.txt`
and a `sub/a.txt`, `a.txt` always selects the root file — even from inside `sub` — and `sub/a.txt`
selects the nested one. A leading `./` and empty components are dropped; `..` is rejected.

Exported JSON paths can therefore be passed back unchanged to any command without ambiguity.
`read`, `pending`, and `set` also accept absolute paths inside the repository; `import` requires
repo-relative paths. Arguments outside the repository, ignored or excluded files, and nonexistent
paths are rejected with exit code 1 rather than silently matching nothing.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success (including an empty `pending` listing). |
| `1` | Invalid input: bad or out-of-repo scope, malformed/duplicate/mismatched import record, empty, whitespace-only or multiline note, failed `--expected-hash` guard. |
| `2` | Environment failure: not a Git repository, unreadable path, a Git-indexed path whose ancestor has become a symlink, unsupported database schema, I/O or SQLite error — and command line usage errors reported by clap (`--help` and `--version` still exit 0). |

`--json` output on stdout is always one parseable document; every diagnostic goes to stderr.

## How entries are inventoried

* **Repository identity** is the canonical Git *common* directory (`git rev-parse
  --git-common-dir`), so every linked worktree of one repository shares notes. Unrelated
  repositories — including independent clones — are isolated; a clone is not a worktree of the same
  repository.
* **Inventory** comes from `git ls-files` with NUL-delimited output: tracked entries plus untracked
  files that are not ignored (`.gitignore` rules honoured). Deleted tracked files are omitted — the
  inventory describes what exists now. Git metadata is never scanned. Directories, including the
  root `.`, are synthesized entries so ancestors can be annotated too.
* **Bytes, not index objects**: file hashes stream the working-tree bytes through BLAKE3
  (`BufReader`, 64 KiB windows), so unstaged edits change the hash.
* **Symlinks** are hashed by their *target text* and never followed; a symlinked directory outside
  the repository is an opaque `symlink` entry, so the scan never walks out of the repository. If a
  directory on the path to a Git-indexed file has been replaced by a symlink, the scan **fails
  closed** (exit 2) and names the symlink ancestor, instead of following the link and reporting the
  external file under the indexed path.
* **Submodules** are opaque `submodule` entries hashed from the gitlink commit recorded in the
  index. Their contents are **not** inventoried or hashed recursively; a submodule note records
  "this vendored dependency is pinned at this commit".
* **Unreadable or unsupported paths fail loudly** (exit 2) instead of being dropped, including
  non-UTF-8 paths, special files, and directories that are not submodules.
* **The notes database and its `-wal`/`-shm` sidecars are excluded** even when `--db` points inside
  the worktree.
* **Exported notes never contain file contents.** Only one-line summaries you wrote are stored, and
  `read`/`pending` never read file contents back into their output.

## Hash semantics

All hashes are BLAKE3 with a version/domain tag. Within one scheme version (`tnt1`) a hash is
stable across machines and platforms, and the scheme tag is part of every hash string. The tag is a
*version*, not a promise: a future scheme version may change it, which changes every hash, and
stored notes keep the scheme-tagged hash they were written for — so hashes from different scheme
versions are never confused, and notes bound to the old scheme report as `stale` rather than being
silently reused.

| Entry | Hash |
| --- | --- |
| file | `tnt1:file:H` = BLAKE3(`treenotes-hash-v1\|file\0` \|\| working-tree bytes) |
| symlink | `tnt1:symlink:H` = BLAKE3(`treenotes-hash-v1\|symlink\0` \|\| link target bytes) |
| submodule | `tnt1:submodule:H` = BLAKE3(`treenotes-hash-v1\|submodule\0` \|\| index gitlink sha) |
| directory | `tnt1:dir:H` = BLAKE3(`treenotes-hash-v1\|dir\0` \|\| children) |
| member | `tnt2:member:H` = BLAKE3(`treenotes-hash-v2\|member\0` \|\| symbol key \|\| `\0` \|\| normalised declaration text) |
| state | `tnt1:state:H` = BLAKE3(`treenotes-hash-v1\|state\0` \|\| entries) |

Member hashes deliberately use a **different scheme tag** (`tnt2`) because they hash different input:
not whole working-tree bytes but `symbol_key = <kind>:<qualified name>:<ordinal>` plus the
declaration's own source text with insignificant whitespace collapsed. Comments are *not* stripped,
so editing a doc comment re-stales the member — a false-stale is safer than a false-fresh — and
whitespace inside string literals is preserved. Two different scheme tags never collide, and a note
bound to one scheme reports as `stale` under the other rather than being reused.

Directory children are encoded unambiguously and deterministically: each child contributes
`u32-le(name length) || name || kind tag || u32-le(hash length) || hash`, and children are sorted by
name then kind tag before hashing. Directory hashes are computed bottom-up, so editing, adding,
removing or renaming a descendant changes the hashes of its ancestors only — unrelated siblings keep
their hash, their status and their notes.

State entries are encoded the same way as directory children but over the whole inventory:
`u32-le(path length) || path || kind tag || u32-le(hash length) || hash`, in path order. The state
hash is therefore a pure function of what the tree contains — not of Git history — so an unchanged
checkout always reports the same state, including in another worktree and after an empty commit.
Recording a state stores that list, which is what makes "what changed since?" a diff between two
recorded inventories instead of a re-read.

**Cost.** Hashing is O(total bytes) per invocation, streamed and never buffered whole. `read` and
`pending` re-hash the **entire** inventory even when a scope limits how much output is printed (a
deliberate trade: the hash of a scope always agrees with the hash of the whole tree). A scope bounds
the listing, not the scanning; large repositories are dominated by this scan regardless of
`pending PATH` or `read PATH --depth N`.

**The repeated work that is cached** is the *parsing*, not the hashing. `read --members`, `set
--ast` and `member-set` need the members of a file, and parsing a supported source file is far more
expensive than hashing it. Members are cached in the database under `(path, file hash)` — the hash
just computed from the bytes on disk — so a file that did not change is never parsed twice, and an
edited file misses the cache and is parsed again. The cache is keyed by content, never by
mtime, and it never caches a *note*: notes stay bound to hashes exactly as before. A `tnt1:state`
hash answers the coarser question ("has this build already computed this whole tree?") and lets a
recorded state stand in for the commit a rebuild would otherwise diff against.

## Note status semantics

Notes are keyed by `(repository, path, kind, hash)`, and **history is kept**: every annotated
version is retained, so reverting a file, switching branches, or checking out an old revision
restores the exact note written for that content.

| Status | Meaning |
| --- | --- |
| `fresh` | A note exists for exactly this path, kind and current hash. |
| `stale` | Notes exist for this path and kind, but not for the current hash. The latest one is shown under `previous`, always labelled as not current, with its own hash and timestamp. |
| `missing` | No note has ever been written for this path and kind. |

A stale note is **never** presented as the current summary: `note`/`note_hash`/`note_updated_at` are
`null` and the older text appears only in the `previous` block. Because the *path* is part of the
key, identical bytes at two different paths keep their own purposes and their own notes.

Notes are never garbage-collected in this version; the database grows with annotation history only.

## JSON contract

`--json` prints one versioned envelope. `version` is `2`; it is bumped whenever the shape changes.
Version 2 adds the `members` array (member listing, `set --ast`, `member-set`) and the optional
`parse_error` flag. Every envelope still carries `entries` exactly as before, and an envelope that
deals in no members carries an empty `members` array and no `parse_error`, so a consumer that
ignores unknown fields keeps working.

```json
{
  "version": 2,
  "tool": "treenotes",
  "command": "read",
  "repository": {
    "identity": "tnt1:repo:3f...",
    "root": "/home/me/project",
    "common_dir": "/home/me/project/.git"
  },
  "scope": { "path": ".", "kind": "dir", "depth": null },
  "entries": [
    {
      "path": "src/lib.rs",
      "kind": "file",
      "hash": "tnt1:file:9a...",
      "status": "stale",
      "note": null,
      "note_hash": null,
      "note_updated_at": null,
      "previous": {
        "hash": "tnt1:file:1c...",
        "note": "entry point, wires the command surface to the scanner",
        "updated_at": "2026-09-20T01:10:00Z"
      }
    }
  ],
  "members": [
    {
      "path": "src/lib.rs",
      "symbol": "method:Cache.put:0",
      "symbol_kind": "method",
      "name": "put",
      "qualified_name": "Cache.put",
      "start_line": 12,
      "end_line": 18,
      "hash": "tnt2:member:5e...",
      "status": "missing",
      "note": null,
      "note_hash": null,
      "note_updated_at": null,
      "previous": null
    }
  ]
}
```

Field notes:

* `version`, `tool`, `command` and `repository` are always present. `repository.identity` is the
  same value in every linked worktree.
* `scope` appears for `pending` and `read` (`path` is `"."` for the whole repository; `kind` is
  `file`/`dir`/`symlink`/`submodule`; `depth` is the requested limit or `null`).
* `entries` is a deterministic array: `read` orders entries by path; `pending` orders descendants
  before ancestors with the scope last.
* `set --json` adds `message`; `import --json` adds `imported` (number of records written) and
  `message`, with one `entries` item per written record.
* `members` is always present and ordered by path, then start line. `read --members` fills it for the
  scoped files; `set --ast` and `member-set` report the members they wrote. A member uses the same
  freshness vocabulary as an entry (`fresh`/`stale`/`missing`, `note`, `previous`), plus `symbol`,
  `symbol_kind`, `name`, `qualified_name`, `start_line` and `end_line`.
* `parse_error` appears on `read --members`, `set --ast` and `member-set` and is `true` when at
  least one scoped source file needed grammar error recovery. It is absent everywhere else.
* A stale entry carries `note: null` plus the `previous` object, whose `note` is the most recently
  written version of that path and kind. A fresh entry carries `note`, `note_hash` and
  `note_updated_at`. `missing` entries carry neither.
* The immediate `set --json` and `import --json` acknowledgments report `note_updated_at: null` for
  the records just written — the write the caller just performed is reported back, no timestamp is
  read again — while a subsequent `read`/`pending` of the same fresh entry carries the stored
  `note_updated_at`.
* `state` appears only on `state --json`. It carries `state_hash`, `commit` (null before the first
  commit), `known` (this exact state was already recorded), `recorded` (this run wrote it),
  `member_cache_hits`/`member_cache_misses` (counts of supported source leaves in the tree whose
  members are or are not already cached), `compared_state` (the recorded state compared against, or
  `null`) and `changes` (one record per added, modified, kind-changed or removed leaf, ordered by
  path, each with `hash` and `previous_hash`).

`import` input — one JSON array, all fields required, unknown fields rejected:

```json
[
  { "path": "src/lib.rs", "hash": "tnt1:file:9a...", "note": "entry point; wires commands to the scanner" },
  { "path": "src", "hash": "tnt1:dir:4d...", "note": "library sources, one module per concern" }
]
```

## External-agent workflow

```console
# 1. What still needs a summary? Children first, so files are summarized before their directories.
$ treenotes pending --json

# 2. The agent reads the source files it selected. treenotes reads those same bytes itself only to
#    hash them and never puts file contents in its output; each entry carries the current `hash` of
#    exactly the bytes the agent is reading.

# 3. Write the summaries back in one batch, using those original hashes as guards.
$ treenotes import batch.json

# 4. Read the compact annotated map, scoped to what the next step needs.
$ treenotes read src --depth 2 --json

# 5. Feed that JSON to an external relevance classifier (e.g. jev) — outside treenotes.

# After edits, re-run `pending`: only entries whose content changed reappear, and any version
# already annotated is reused, so notes written earlier are never lost.
$ treenotes pending
```

Because every record carries the hash the agent derived its note from, a file edited mid-read makes
the whole import fail (`exit 1`) instead of attaching a note to content nobody read. For the same
reason a note written for hash `H` stays bound to `H`: once the file changes, the entry becomes
`stale` rather than silently inheriting the new content. Guards compare against the snapshot taken
when the command starts; treenotes does not lock the working tree. An edit after that snapshot can
make the acknowledgment immediately out of date, but the note remains bound to the original hash.
Re-run `pending` after concurrent edits.

## Limitations

* AST members exist only for Java, Rust, TypeScript/TSX, JavaScript and Python; other languages have
  no members until an adapter is added (one module plus one registry entry).
* Member hashes are `tnt2` and include comments and doc comments, so editing prose re-stales the
  member. Member ordinals disambiguate overloads and come from document order, so reordering
  overloaded declarations of the same name re-stales them.
* A file whose grammar needs error recovery still yields members; `parse_error` marks the listing as
  untrustworthy rather than hiding it.
* Submodules are one opaque entry each; nested submodule trees are not inventoried.
* Symlinks are hashed by target text only; target contents are deliberately never followed.
* Non-UTF-8 paths and special files abort the scan (exit 2) rather than being silently skipped.
* Notes are not garbage-collected; history grows with distinct annotated versions.
* Linked worktrees share notes; independent clones of the same upstream do not (different common
  directories, therefore different repository identities).
* Hashing is proportional to the bytes in the tree, on every invocation. The derived member cache
  removes repeated *parsing*, not the inventory scan, and notes are cached only implicitly (an
  unchanged file's members are reused, but its note is still resolved from the `member_notes`
  table).

## Development

```console
$ cargo fmt --all -- --check
$ cargo clippy --all-targets --all-features -- -D warnings
$ cargo test --all-targets --all-features
```

Integration tests in `tests/cli.rs` build real temporary Git repositories (including linked
worktrees and submodules) and temporary databases; they never touch `~/.tree-notes`.

## AST/member annotations

Methods and other named declarations inside a file can be annotated in Java, Rust,
TypeScript/TSX, JavaScript and Python: `read PATH --members` lists them, `member-set` annotates one
of them, and `set PATH --ast` carries a file's existing member summaries forward after an edit.
Member notes use the `tnt2` hash scheme and live in their own `member_notes` table; the `tnt1`
file/dir/symlink/submodule scheme and every file note are untouched, and older databases are
migrated additively (schema version 3 adds the derived state tables `snapshots`/`snapshot_entries`
and the derived member cache `member_index_files`/`member_index` — all four are regenerable and can
be dropped at any time without losing a note). Adding a language is one adapter module plus one
registry entry — see
[`docs/ast-annotations-plan.md`](docs/ast-annotations-plan.md) for the design and the decisions
still open (comment stripping, ordinal churn, member-vs-file precedence in `pending`).
