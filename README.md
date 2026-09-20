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

treenotes pending [PATH] [--json]
treenotes read    [PATH] [--depth N] [--json]
treenotes set     PATH [--note TEXT] [--expected-hash HASH] [--json]
treenotes import  [FILE|-] [--json]
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
* `read [PATH] [--depth N]` — show the tree, or exactly one file, or one directory subtree, with
  freshness for every entry. `--depth 0` shows only the scope itself. Output is an indented text
  map by default, or a versioned JSON envelope with `--json`. Root (`.`) and unannotated entries
  are always included so coverage gaps stay visible.
* `set PATH --note TEXT [--expected-hash HASH]` — annotate the *current* version of `PATH`. With no
  `--note`, the one-line note is read from stdin (`printf 'summary\n' | treenotes set src/lib.rs`),
  which avoids shell-quoting friction. `--expected-hash` refuses the write unless the current hash
  still matches, guarding against annotating content that changed while the agent was reading it.
* `import [FILE|-]` — read a JSON array of `{ "path", "hash", "note" }` records (from a file, `-`,
  or stdin when omitted). Every record is validated first: the array must be non-empty, paths must
  be repository-root relative with no `.`/`..`/empty components, duplicates after normalization are
  rejected, notes must be non-empty single lines, and every `hash` must equal the *current* hash of
  that path. Only then is the whole batch written in one transaction — one bad record means no
  notes at all are written.

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

Directory children are encoded unambiguously and deterministically: each child contributes
`u32-le(name length) || name || kind tag || u32-le(hash length) || hash`, and children are sorted by
name then kind tag before hashing. Directory hashes are computed bottom-up, so editing, adding,
removing or renaming a descendant changes the hashes of its ancestors only — unrelated siblings keep
their hash, their status and their notes.

**Cost.** Hashing is O(total bytes) per invocation, streamed and never buffered whole. `read` and
`pending` re-hash the **entire** inventory even when a scope limits how much output is printed (a
deliberate trade: no fragile on-disk snapshot cache, and scoped output always agrees with unscoped
output). A scope bounds the listing, not the scanning; large repositories are dominated by this
scan regardless of `pending PATH` or `read PATH --depth N`.

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

`--json` prints one versioned envelope. `version` is `1`; it is bumped whenever the shape changes.

```json
{
  "version": 1,
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
* A stale entry carries `note: null` plus the `previous` object, whose `note` is the most recently
  written version of that path and kind. A fresh entry carries `note`, `note_hash` and
  `note_updated_at`. `missing` entries carry neither.
* The immediate `set --json` and `import --json` acknowledgments report `note_updated_at: null` for
  the records just written — the write the caller just performed is reported back, no timestamp is
  read again — while a subsequent `read`/`pending` of the same fresh entry carries the stored
  `note_updated_at`.

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

* Submodules are one opaque entry each; nested submodule trees are not inventoried.
* Symlinks are hashed by target text only; target contents are deliberately never followed.
* Non-UTF-8 paths and special files abort the scan (exit 2) rather than being silently skipped.
* Notes are not garbage-collected; history grows with distinct annotated versions.
* Linked worktrees share notes; independent clones of the same upstream do not (different common
  directories, therefore different repository identities).
* Hashing is proportional to the bytes in the tree, on every invocation.

## Development

```console
$ cargo fmt --all -- --check
$ cargo clippy --all-targets --all-features -- -D warnings
$ cargo test --all-targets --all-features
```

Integration tests in `tests/cli.rs` build real temporary Git repositories (including linked
worktrees and submodules) and temporary databases; they never touch `~/.tree-notes`.
