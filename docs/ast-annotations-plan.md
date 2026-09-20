# Plan: AST-backed method annotations for treenotes

Status: **implemented** (see §11). The `tnt1` file/dir/symlink/submodule scheme, the `notes` table and
every existing command keep working exactly as before; AST support adds the `tnt2` member scheme,
the `member_notes` table and JSON envelope `version: 2` with a `members` array. Sections §1-§7
describe the shipped design and §8's recommendations were all followed as written.

## 1. What we are adding, and what we are not

The request: annotate **methods and other named declarations inside a file** in Java, Rust,
TypeScript/JavaScript and Python, and make the parsing layer pluggable so more languages can follow.
We are *not* adding a general semantic index, call graphs, or cross-file resolution in this pass.
Every annotation stays bound to content, exactly like today's notes.

Non-goals for the first pass:

* Cross-file symbol resolution, references, call graphs, inheritance walks.
* Type inference or any compiler-grade analysis.
* Languages other than the four families above (the registry is shaped so adding one is data, not
  a rewrite).
* Any model, network service or classifier call — the crate's "no AI integration" rule stands.

## 2. The central problem: a symbol must be addressable by content

Today a note is keyed by `(repository, path, kind, hash)` where `hash` is the hash of the whole
file. A method is defined by a **span inside** the file, so a note on a method must be keyed by
something that survives edits elsewhere in the file. Editing an unrelated function must not make
every method note stale, and deleting a method must not silently rebind its note to whatever
moves into that byte range. Two properties are required of the symbol key:

1. **Deterministic** — identical bytes must always produce identical keys (both for note identity
   and for reproducibility in tests and across machines).
2. **Content-bound** — the key must change when the declaration changes, and must be derived from
   the declaration itself, not from its file offset alone.

The chosen scheme (call it `tnt2`, but see §8: it is a *new hash scheme string*, introduced only
when this ships):

```
symbol_key   = <symbol-kind> ":" <display-name> ":" <ordinal>
symbol_hash  = "tnt2:member:<hex>"
             = BLAKE3("treenotes-hash-v2|member\0" || symbol_key || "\0" || normalised-body)
```

* `symbol-kind` — the treenotes-side kind tag: `method`, `function`, `constructor`, `field`,
  `class`, `struct`, `enum`, `trait`, `interface`, `type`, `module`, `constant`, … (a small closed
  set per language, mapped from node kinds by the adapter).
* `display-name` — the declared name as written, unqualified (`name`, `Inner.go`, `Foo::method`).
  Qualified paths are used where the grammar gives a container chain (NestJS/TS classes, Rust
  `impl` blocks, Java inner classes, Python nested defs) so `a.b` and `a.c` don't collide.
* `ordinal` — zero-based index among siblings with the same kind+name in the same container. This
  makes `impl Foo` + `impl Foo` (legal in Rust) and overloaded Java methods distinct without
  hashing signatures yet. It is the only ordinal-sensitive part.
* `normalised-body` — the declaration's source text with insignificant whitespace collapsed
  (runs of space/tab/CR/LF outside strings become one space) — a first pass. It is deliberately
  conservative: comments are **not** stripped in v1 (comment edits change the symbol hash, which
  is a *safer* false-stale than a false-fresh), and string bodies are not normalised (so changing
  `"a   b"` to `"a b"` does not change the hash).

Why not hash the byte span alone, or the AST without text?

* **Span alone** breaks on any edit above the declaration and can rebind a note to different text
  when lines are inserted — unacceptable, because a note would silently describe code nobody wrote
  it for.
* **Normalised text** is stable under formatting, reordering of *unrelated* declarations and
  comment-free reformatting, and it changes exactly when the declaration's code changes. Hashing
  the tree structure without text would call a changed literal or a renamed called function
  "identical", i.e. it would claim freshness for a body that changed — the one failure mode we
  refuse (see the existing "notes are never presented as the current summary" rule).
* Reordering *sibling* declarations changes ordinals only when kind+name collide, so ordinary
  reordering does not churn the database.

Consequences that fall out of this:

* Editing one method stales exactly that member note (plus the file note and the ancestor directory
  notes, as today). Siblings stay `fresh`.
* Adding a method stales the file and directory notes and adds one `missing` member; it does not
touch its siblings unless it is a same-kind same-name sibling inserted *before* them.
* Renaming a method produces a `missing` member and leaves a stale member note behind. We keep the
  history, so the old note is still visible under the old key. (Rename *detection* is out of scope;
  see §9 open questions.)

## 3. Identity, schema and record shape

A member note is one more **kind**. Rather than adding four kinds (`java_method`, …), add a single
kind `member` and keep the language in the symbol key. Rationale: the store's uniqueness constraint
is `(repository, path, kind, hash)`, and the *path* already encodes the language via its extension;
a per-language kind would multiply kinds over time for no gain.

```sql
CREATE TABLE member_notes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    repository  TEXT NOT NULL,
    path        TEXT NOT NULL,
    symbol      TEXT NOT NULL,   -- symbol_key, e.g. 'method:Cache.put:0'
    symbol_kind TEXT NOT NULL,   -- 'method', 'class', ...
    name        TEXT NOT NULL,   -- display name, as written
    start_line  INTEGER NOT NULL,
    end_line    INTEGER NOT NULL,
    hash        TEXT NOT NULL,   -- 'tnt2:member:<hex>'
    note        TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (repository, path, symbol, hash)
);
CREATE INDEX member_notes_repository_path ON member_notes (repository, path);
CREATE INDEX member_notes_repository_hash ON member_notes (repository, hash);
```

Notes:

* A **separate table**, not a `kind` value in `notes`. The existing `notes` table's constraint and
  index are exactly right for tree entries, and mixing a nullable `symbol` column into it would
  weaken the invariant that every `notes` row is a tree entry. Schema version goes `1 -> 2`, and the
  existing migration behaviour is preserved: version `1` databases get an additive migration
  (`CREATE TABLE member_notes ...` + `PRAGMA user_version = 2`); version `0` non-empty databases
  still fail closed; version `> SCHEMA_VERSION` still fails with the "not supported by this
  treenotes build" message.
* `start_line`/`end_line` are **display metadata**, refreshed on every parse. They are deliberately
  *not* part of note identity, so a note survives a declaration moving down the file. Output shows
  the *current* lines, not the lines at write time.
* The note text is one line, same `validate_note` rules as today (non-empty, no newlines).
* `hash` is `tnt2:member:…`; note history is kept by the same upsert-on-conflict pattern, so
  reverting a method restores the exact note written for that version.

## 4. Language registry and adapters

New module `src/lang/` (or `src/ast/`), with one adapter per language family:

```rust
/// One language we can parse, with its tree-sitter grammar.
pub struct Language {
    /// CLI/config name: 'java', 'rust', 'typescript', 'javascript', 'python'.
    pub id: &'static str,
    /// File extensions that select this language.
    pub extensions: &'static [&'static str],
    /// tree-sitter grammar handle.
    pub grammar: fn() -> tree_sitter::Language,
    /// Node kinds that become annotatable members, with the member kind and name field.
    pub members: &'static [MemberSpec],
}

pub struct MemberSpec {
    /// tree-sitter node kind, e.g. "method_declaration".
    pub node_kind: &'static str,
    /// treenotes member kind: "method", "function", "class", ...
    pub member_kind: &'static str,
    /// Field holding the name (usually "name"); fallback: first identifier child.
    pub name_field: Option<&'static str>,
    /// When true the node is a container whose qualified name prefixes its children.
    pub container: bool,
    /// Node kind whose subtree to skip (e.g. "block"/"function_body") so nested closures or
    /// local functions do not become top-level members.
    pub body_kind: Option<&'static str>,
}

pub fn registry() -> &'static [Language];
pub fn language_for_path(path: &str) -> Option<&'static Language>;
pub fn parse_members(language: &Language, source: &str) -> Result<Vec<Member>, CmdError>;
```

`Member` carries `symbol`, `symbol_kind`, `name`, `start_line`, `end_line`, `hash` (computed as in
§2) and the qualified container path, so output can nest members under their class/impl/module.

### 4.1 Verified grammar mapping (first pass)

The node kinds below were **checked against the actual crates** (throwaway probe on this machine:
`tree-sitter 0.26.13`, `tree-sitter-java 0.23.5`, `tree-sitter-rust 0.24.2`,
`tree-sitter-python 0.25.0`, `tree-sitter-javascript 0.25.0`, `tree-sitter-typescript 0.23.2`;
all five parsed the sample sources without errors and produced the kinds listed):

| Language (crate, version) | extensions | container node kinds | member node kinds | name field |
| --- | --- | --- | --- | --- |
| Java (`tree-sitter-java` 0.23.5) | `.java` | `class_declaration`, `interface_declaration`, `enum_declaration`, `record_declaration`, `annotation_type_declaration` | `method_declaration`, `constructor_declaration`, `field_declaration`, `enum_constant` | `name` |
| Rust (`tree-sitter-rust` 0.24.2) | `.rs` | `mod_item`, `impl_item`, `trait_item`, `struct_item`, `enum_item` | `function_item`, `const_item`, `static_item`, `type_item`, `struct_item`, `enum_item`, `trait_item`, `mod_item`, `macro_definition` | `name` (`impl_item` may have none) |
| TypeScript (`tree-sitter-typescript` 0.23.2) | `.ts`, `.mts`, `.cts` | `class_declaration`, `interface_declaration`, `internal_module`, `enum_declaration` | `method_definition`, `function_declaration`, `abstract_method_signature`, `public_field_definition`, `variable_declarator` (only when the initializer is `arrow_function`/`function`), `type_alias_declaration`, `interface_declaration`, `enum_declaration` | `name` |
| TSX (`tree-sitter-typescript` 0.23.2) | `.tsx` | as TS | as TS | `name` |
| JavaScript (`tree-sitter-javascript` 0.25.0) | `.js`, `.mjs`, `.cjs`, `.jsx` | `class_declaration` | `method_definition`, `function_declaration`, `function_expression` (named), `variable_declarator` (arrow/function init) | `name` |
| Python (`tree-sitter-python` 0.25.0) | `.py`, `.pyi` | `class_definition` | `function_definition`, `decorated_definition` (unwrap to the inner `function_definition`/`class_definition`), `assignment` (only when the target is a plain identifier and the value is `lambda` or the target is SCREAMING_CASE — see §5) | `name` (via the `name` field or the `identifier` child) |

Grammar-version notes:

* All grammar crates depend on `tree-sitter-language` (`LANGUAGE: LanguageFn`) and are used as
  `Language::from(grammar::LANGUAGE)` (verified: `tree_sitter_java::LANGUAGE.into()`,
  `tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()`,
  `tree_sitter_typescript::LANGUAGE_TSX.into()`, `tree_sitter_javascript::LANGUAGE.into()`).
  This is the *new* ABI-stable shape; the old `language()` function is gone.
* `tree-sitter` (runtime) and the grammar crates each carry their own ABI/`tree-sitter` version
  requirement. `tree-sitter = "0.26"` builds and runs against all six crates above; a `0.27` bump
  must be re-verified. Pin **exact** versions in `Cargo.toml` (`=0.23.5`, `=0.24.2`, `=0.25.0`,
  `=0.25.0`, `=0.23.2`) so a grammar update never silently changes which nodes exist — the node
  kinds in the table are a contract, and a grammar bump is a code review, not a lockfile churn.
* All five grammars ship pre-generated `parser.c`/`scanner.c` behind a `cc` build script; the crate
  keeps working offline and on a machine with only `cc` (no `node`, no `npm`, no grammar checkout).

### 4.2 Adding a fifth language later

Adding a language = one new module + one `Language` entry: extension list, grammar handle, and a
`MemberSpec` table. No change to hashing, storage, CLI or output. `cargo test` includes a
completeness test (see §7) that fails when an adapter's `MemberSpec` names a node kind the pinned
grammar does not define, so a bad table is caught at test time instead of silently finding nothing.
If a grammar lacks a stable name field, the spec's `name_field: None` falls back to "first child of
kind `identifier`/`type_identifier`/`property_identifier`", and, failing that, to `"<anonymous>"`
with the ordinal, which keeps keys unique.

## 5. What becomes a member (per-language rules)

* **Java** — the class/interface/enum/record/annotation declarations themselves (kind `class`,
  `interface`, `enum`, `record`, `annotation`), their methods (`method`), constructors
  (`constructor`), fields (`field`) and enum constants (`constant`). Names come from the `name`
  field; overloads disambiguate by ordinal. Nested and inner types get qualified names
  (`Foo.Inner.go`). Anonymous classes (`object_creation_expression` with a `class_body`) are
  **skipped** — no stable name.
* **Rust** — `fn` (kind `function`), `struct` (`struct`), `enum` (`enum`), `trait` (`trait`),
  `impl` (`impl`, name = the `type` field's text, e.g. `impl T for S` -> `S` with qualified
  `S::m` children), `mod` (`module`), `const`/`static` (`constant`), `type` alias (`type`), and
  `macro_rules!` (`macro`). `impl` blocks with no explicit `type` (`impl { ... }` is not legal;
  but `impl Trait for _` and generic params mean the text can be empty) fall back to
  `impl:<ordinal>`. Items inside a `function_item`'s `block` are **not** members (they are locals);
  items inside a `mod`/`impl`/`trait` body are.
* **TypeScript / JavaScript** — classes (`class`), interfaces (`interface`), enums (`enum`), type
  aliases (`type`), namespaces (`namespace`), functions (`function`), class methods including
  getters/setters and the constructor (`method`/`constructor`), class fields (`field`), and
  `const f = (...) => …` / `const f = function () {}` (`function`). A `variable_declarator` is only
  a member when its initializer is a `function`/`arrow_function` — `const x = 1` is not. Both
  `export` wrappers (`export_statement`) must be unwrapped (verified: an exported class/function is
  nested one level deeper). JSX files (`.jsx`) are parsed with the JavaScript grammar; `.tsx` uses
  `LANGUAGE_TSX`.
* **Python** — `def` (`function`), `async def` (`function`), `class` (`class`), `decorated_definition`
  unwrapped to its `definition` child (decorators are part of the body text that gets hashed, so
  adding a decorator re-stales the member), and module/class-level `NAME = ...` constants. The
  constant rule: an `assignment` whose single target is a plain `identifier` **and** (the target is
  all-uppercase, or the value is a `lambda`). This deliberately excludes ordinary locals such as
  `x = 1` at class level. Nested `def`s become members qualified by their outer function
  (`outer.inner`), because Python's nested functions are addressable and useful to annotate.

## 6. CLI and JSON surface

New subcommand (working name `symbols`), plus `--members` on the existing commands:

```
treenotes symbols [PATH] [--json]                 # every member of a file, or of every supported
                                                  # file under a directory scope

treenotes pending PATH --members [--json]          # members that are `missing` or `stale`

treenotes member-set PATH SYMBOL [--note TEXT]    # annotate one member
                    [--expected-hash HASH] [--json]

treenotes read PATH --members [--json]            # tree map plus its members

treenotes pending PATH --members [--json]

treenotes import-members [FILE|-] [--json]        # batch: {path, symbol, hash, note}
```

Batch import matches the existing `import` contract exactly — validate everything first, then write
in one transaction, one bad record means no writes.

JSON: a new **`members` array** inside the existing envelope, plus a `members` scope block, and
`JSON_VERSION` bumped `1 -> 2`. Keeping members out of `entries` avoids overloading `status`
semantics and lets older consumers ignore the new field. Shape:

```json
{
  "version": 2,
  "tool": "treenotes",
  "command": "symbols",
  "repository": { "identity": "tnt1:repo:3f...", "root": "/home/me/project", "common_dir": "/home/me/project/.git" },
  "scope": { "path": "src/store.rs", "kind": "file" },
  "members": [
    {
      "path": "src/store.rs",
      "symbol": "function:write_notes:0",
      "symbol_kind": "function",
      "name": "write_notes",
      "qualified_name": "Store::write_notes",
      "start_line": 222,
      "end_line": 255,
      "hash": "tnt2:member:9a...",
      "status": "stale",
      "note": null,
      "note_hash": null,
      "note_updated_at": null,
      "previous": { "hash": "tnt2:member:1c...", "note": "...", "updated_at": "2026-09-20T01:10:00Z" }
    }
  ],
  "entries": []
}
```

`symbol`, `symbol_kind`, `qualified_name`, `start_line`, `end_line`, `hash`, `status`, `note`,
`note_hash`, `note_updated_at`, `previous` — the freshness fields reuse today's exact three-state
semantics (`fresh`/`stale`/`missing`) and the same "a stale note is never presented as current"
rule. Text output mirrors it, indented under the file, with the same `previous (stale, not current)`
labelling:

```
src/store.rs [file] 9a1c2f3e8b76 stale "SQLite note storage"
  fn write_notes:0 [function] 4d4d8b91c002 missing
  fn write_note:0 [function] c3a0e1f77a91 fresh "one note version, upsert"
  fn load_versions:0 [function] 77b1e2... stale
    previous (stale, not current): tnt2:member:1123... "all versions of one repo"
```

Exit codes are unchanged (0/1/2). Failure handling for the new paths:

* Unsupported extension (`symbols main.go`, `symbols docs/`) — not an error: exit 0 with an empty
  member list and, in text mode, one line naming the reason (`no AST adapter for "main.go"`).
  A directory scope simply reports no members for unsupported files.
* Parse errors — the grammar is error-tolerant: `root_node().has_error()` is reported as a warning
  field (`"parse_error": true`) and members are still listed from the recovered tree. `symbols`
  never exits 2 for an unparsable file, matching "agents see coverage gaps" rather than "tool
  explodes"; but a file that cannot be *read* is still an environment failure (exit 2), like today.
* Non-UTF-8 file that is otherwise text — today's inventory never reads contents for entries, so
  this is new: `symbols` decodes strictly and reports exit 2 (the crate's existing "fail loudly
  instead of dropping" rule).

## 7. Work breakdown for the PR

Land as one PR, ~6-9 commits, each with tests:

1. **Deps + adapter skeleton.** `Cargo.toml`: `tree-sitter = "0.26"` plus the five grammar crates
   pinned exactly. `src/ast/mod.rs` with `Language`, `MemberSpec`, `Member`, `registry()`,
   `language_for_path()`, `parse_members()`. No CLI change yet.
2. **Java adapter** + fixtures (`tests/fixtures/java/*.java`) covering: nested/inner classes,
   interfaces, enums with constants, records, annotations, overloads, anonymous classes skipped.
3. **Rust adapter** + fixtures: `impl` blocks (multiple, generic, trait impls), traits with default
   bodies, nested modules, consts/statics/type aliases, `macro_rules!`, items inside a function
   body excluded.
4. **TS/JS adapter** + fixtures (`.ts`, `.tsx`, `.js`, `.jsx`): exported declarations, interfaces,
   type aliases, namespaces, classes with constructor/getters/fields, arrow-function consts,
   `const x = 1` excluded.
5. **Python adapter** + fixtures: classes, decorated defs, async defs, nested defs, SCREAMING_CASE
   constants included and `x = 1` excluded, lambdas.
6. **Storage**: `member_notes` table, schema migration `1 -> 2`, `write_member_note(s)`,
   `load_member_versions`, history and stale/previous resolution. Unit tests for the migration
   (empty DB, version-1 DB with notes, version-3 DB rejected, DB with tables but no version).
7. **Symbol hashing** (`src/ast/hash.rs`): `tnt2:member:` computation, whitespace normalisation, and
   the invariants as tests — *edit a sibling method ⇒ the other method's hash is unchanged*;
   *reformat (indentation only) ⇒ hash unchanged*; *change a literal ⇒ hash changes*; *reorder two
   differently-named methods ⇒ both hashes unchanged*; *two same-named methods ⇒ distinct keys*.
8. **CLI**: `symbols`, `member-pending`, `member-set`, `import-members`, `--members` on `read`/
   `pending`; JSON version bump to 2; text rendering.
9. **Docs**: README sections for member notes, the `tnt2` member hash, the language table (four
   families, extensions), and the "unsupported extension is not an error" rule; update the JSON
   contract section and the `Limitations` list (parse-error tolerance, comments count as content).

### Tests that must exist in the PR

* **Adapter completeness** — for each adapter, parse each fixture and assert the *exact* expected
  `(symbol, symbol_kind, start_line, end_line)` list. Fixtures are the specification: a grammar
  bump that changes node kinds fails here. This is the external anchor for the whole feature.
* **Hash invariants** (listed in step 7) as table-driven unit tests on fixture sources.
* **Staleness end-to-end** — in a real temporary repository (reuse the `Sandbox` helper in
  `tests/cli.rs`): annotate two methods of one file, edit one, run `member-pending` and assert the
  other method is still `fresh` and the edited one is `stale` with the old note under `previous`.
* **Schema migration** — a version-1 database written by today's code still opens, its `notes`
  rows survive, and `PRAGMA user_version` becomes 2; a hand-written version-3 database is rejected
  with the existing message and exit 2.
* **Determinism** — `symbols --json` on the same tree twice is byte-identical, and contains no
  source text beyond the member name/qualified name (the existing "never contains source text"
  test generalized to members).
* **Round-trip** — `member-set` with `--expected-hash` guards exactly like `set`; `import-members`
  is atomic (one bad record ⇒ zero rows written).

### Performance budget

Parsing is O(bytes) per supported file, with a fresh `Parser` per file (tree-sitter parsers are not
`Sync`; either one parser per invocation walked over the inventory, or a per-file parser — start
simple). This roughly doubles the per-file constant on large files but does not change the O(total
bytes) scan cost the README already documents. `pending --members` and `read --members` only parse
files in scope, so scoped output stays cheaper than unscoped output; the *entry* scan is unchanged.

### Backward compatibility

* `tnt1` file/dir hashes and existing `notes` rows are untouched; version-1 databases migrate
  additively.
* `--json` consumers get `version: 2`; the `entries` array keeps its shape, so a consumer that
  ignores `members` still works apart from the version field.
* No new runtime dependencies beyond the six crates above; no network, no model, no config file.

## 8. Decisions to confirm before coding

These are the choices worth a maintainer's yes/no; each has a cost to change later.

1. **New hash scheme string `tnt2` for member hashes** (file/dir hashes stay `tnt1`), or keep the
   `tnt1` prefix and add only the `member` kind? Recommendation: `tnt2:member:…` — it is an
   independent scheme (different domain string, different inputs) and the tag is documented as a
   version with no stability promise. Mixed-prefix output is fine because the *kind* is part of
   every hash string.
2. **Separate `member_notes` table** vs. `kind = 'member'` rows in `notes`. Recommendation:
   separate table (§3), because `notes`' uniqueness constraint and indices are exactly the tree
   invariant, and the symbol columns are meaningless for tree entries.
3. **JSON version bump to 2** with members under a new `members` array. Recommendation: yes;
   alternative is a `--json-version 2` flag, which I'd rather not carry.
4. **Comments count as content** in the member hash. Recommendation: yes for the first pass
   (adding a doc comment re-stales the member). Stripping comments is a follow-up that needs its
   own fixtures per language, and a comment-strip bug that *under*-stales is the dangerous
   direction.
5. **Member kinds**: closed set, per §4.1 table. Question for review: which node kinds should be
   *members* versus only *containers* (e.g. is a Java `field_declaration` a first-class annotatable
   member, or only a container child?). Recommendation: annotatable, because "this field holds a
   cached index" is exactly the kind of note an agent wants.
6. **Nested functions in Python / local items in Rust.** Recommendation: Python nested `def`s are
   members with qualified names; Rust items inside a `fn` body are **not** (they are locals and
   their spans shift with ordinary edits). Confirm.
7. **Ordinal-based overload disambiguation** vs. including parameter types in the symbol key.
   Recommendation: ordinal first (no signature parsing, no grammar-specific type strings); revisit
   if Java overload churn shows up in practice.

## 9. Open questions / risks

* **Rename churn** — renames create a `missing` member and orphan a stale note. Content hashing
  already keeps the *unchanged* members' notes intact (see §12): moving a method inside the same file
  changes only that method's own hash, and an untouched file is not even reparsed. A rename is still
  a new declaration with no note, and the old note stays bound to the old symbol as `previous`; a
  future similarity-based suggestion could help, but is out of scope.
* **Grammar ABI drift** — even pinned, a grammar crate's minor bump can add/rename node kinds.
  The adapter-completeness tests turn that into a loud test failure. Keep the pin exact and upgrade
  deliberately.
* **Binary size / build time** — six grammar crates compile C sources; expect a noticeable build
  time and binary size increase (measure before/after in the PR description; if it matters, feature
  flags per language are possible later, at the cost of combinatorial test matrices).
* **Error-tolerant parse quality** — a badly broken file yields a recovered tree with odd spans.
  `parse_error` is surfaced so an agent can distrust the member list; the CLI never hides it.
* **Whole-file notes still required** — member notes do not replace file notes: the file note
  answers "what is this file for", the member note answers "what does this method do". `pending`
  should probably list a file even when all its members are annotated until the file itself has a
  note. Confirm with the maintainer.

## 10. Suggested follow-ups (not in this PR)

* Comment/doc-comment stripping so doc edits do not re-stale members.
* Language additions: Go, C/C++, C#, Ruby, Kotlin, Swift, Scala, Bash, TOML/YAML/JSON schema
  containers (grammars exist for all of these as `tree-sitter-*` crates; each is one adapter).
* `--members-only` / `--depth` filtering for very large classes.
* Optional per-language opt-in via `~/.tree-notes/config.toml`, if feature-flagged grammars are
  ever introduced.
* Member-level `import` streaming for repositories large enough that a batch exceeds memory.
* Diffing two *recorded* states down to the member level: `state --compare` already names which
  files changed, so a follow-up could parse only the changed files and leave the cached members of
  the rest untouched even when the file hash is unchanged but the note text changed.
* Per-language feature flags, if the six grammar crates' build cost ever needs trimming.

## 11. Implementation status (this branch)

* [`src/ast/`](../src/ast) — the language registry plus one adapter module per language (Java, Rust,
  TypeScript, TSX, JavaScript, Python). Every grammar is pinned to an exact version in `Cargo.toml`:
  the node kinds each adapter names are a contract, so a grammar bump is a code review.
* `tnt2` member hashes: `tnt2:member:<hex>` =
  BLAKE3(`treenotes-hash-v2|member\0` || `symbol_key` || `\0` || normalised body), where
  `symbol_key` is `<symbol-kind>:<qualified name>:<ordinal>`. Whitespace outside string literals is
  collapsed; comments are kept (false-stale is the safe direction).
* `member_notes` table (separate from `notes`, as recommended) with the additive schema `1 -> 2`
  migration: a version-1 database keeps every file note and gains the member table.
* JSON envelope `version: 2` with a `members` array and an optional `parse_error` flag; `entries`
  keeps its version-1 shape.
* CLI: `read [PATH] --members`, `set PATH --ast` and `member-set PATH SYMBOL [--note TEXT]
  [--expected-hash HASH] [--json]`.
* Fixtures in `tests/fixtures/` and integration tests in `tests/cli.rs` cover member listing per
  language, per-declaration staleness, the hash guard, parse errors, the schema migration and
  `--ast` carrying summaries forward.

Open for the maintainer, unchanged from §8 and §9: comment/doc-comment stripping, member ordinals
and overload churn, member-vs-file note precedence in `pending`, and per-language build cost.

## 12. Incremental recomputation (this branch)

The build cost of parsing is acceptable, so this branch does not change *what* is computed. It
makes repeating that computation cheap, and it does so without a second hash scheme or a second
notion of identity.

### 12.1 Nothing at the top level is replaced

A member note is **supplemental**. Every guarantee the top level had, it still has:

* `entries` in every envelope keep their version-1 shape, meaning and order; `members` is a sibling
  array that is empty for commands that deal in no members.
* Member notes live in their own `member_notes` table. Writing one never touches `notes`, so a
  `member-set` cannot change a file's status, hash or note — and `set PATH` (without `--ast`) cannot
  change any member's status, hash or note.
* `set PATH --ast` is the one command that writes both, and it still writes the file note for the
  file's current hash *and* only carries *existing* member text forward; it never invents a member
  note, and it never rewrites a member that is already fresh.
* `pending` continues to list files, directories, symlinks and submodules exactly as before.
  Member-level pending is deliberately not added: a file with unannotated members still shows up as
  the file it is, and the deeper listing is opt-in via `read PATH --members`.
* JSON `version` stays `2`: the new `state` block is emitted only by `state`, absent (not `null`)
  everywhere else, so no existing consumer sees a changed shape.

### 12.2 Hashes, not paths, decide whether work is redone

The existing member hash is already the answer to rename churn: `tnt2:member:<hex>` covers the
symbol key *and* the declaration's own text, so an edit to one method changes only that method's
hash, reordering differently-named declarations changes nothing, and formatting-only changes inside
one declaration keep it fresh. Two things follow, both already tested:

* One edited method re-stales exactly that member: its siblings stay `fresh` with their notes.
* A renamed declaration is a new symbol at a new hash — a different note, never a rebinding.

### 12.3 The repository state hash

`tnt1:state:<hex>` = BLAKE3(`treenotes-hash-v1|state\0` || entries), where each entry contributes
`u32-le(path length) || path || kind tag || u32-le(hash length) || hash`, in path order. It is a
pure function of paths, kinds and content hashes — *not* of Git history — so:

* an unchanged checkout reports the same state on every invocation, including from another linked
  worktree, and after an empty commit;
* reverting to a previously observed tree reproduces that tree's state hash exactly, so a settled
  state is recognisable later;
* it is the global summary a future git-branch comparison can key on: the state hash is what "have
  we already computed this?" is asked with.

`treenotes state` records a state it has not seen before, together with every `(path, kind, hash)` of
that state and the commit HEAD pointed at when it was observed — that stored entry list is the stable
side of a later diff. Recording is once per distinct state, so repeat runs are read-only.

`state` also names what changed against a recorded state (`--compare HASH`, else the most recently
recorded *different* state), leaf by leaf: `added`, `modified`, `kind-changed`, `removed`. Directories
are omitted because a directory hash moves exactly when one of its leaves does. A rename is one
removal plus one addition, which is the honest description of it: the same bytes at a new path are a
different note.

### 12.4 The member cache

Parsing is the expensive part; it is cached in the derived tables `member_index_files` (one marker
row per `(path, file hash)`, carrying that parse's `parse_error`) and `member_index` (the members
with their symbol keys, spans and hashes).

* The key is the **file hash the inventory just computed from the bytes on disk**, so an edited file
  misses and is reparsed, and a file that did not change is never parsed twice. No mtime, no
  assumption about bytes that were not hashed.
* An empty member list is a real cached answer, which is why the marker row exists: `None` (never
  parsed) and `Some(vec![])` (parsed, no members) mean different things.
* `state` reports `member_cache_hits`/`member_cache_misses` over the supported source leaves, so the
  saving is observable rather than asserted.
* Rows for a path's *other* file hashes are dropped when a new parse is stored: the cache is only
  ever consulted with the file's current hash, so those rows can never be asked for again.

### 12.5 Schema

Schema version 3 adds `snapshots`, `snapshot_entries`, `member_index_files` and `member_index`. The
`1 -> 2` migration is unchanged; `2 -> 3` is additive, and every version-3 table is derived data
that can be dropped at any time. Losing all four costs a reparse and a re-recorded state; it never
costs a note.

### 12.6 What is deliberately *not* here

* `pending --members` is the opt-in member-level listing, and it does not change `pending`'s
  tree-level meaning: without the flag the output is byte for byte what it was, the flag only
  appends the `missing`/`stale` declarations of the same scope. There is still no separate
  `member-pending` subcommand, and no declaration ever appears in `entries`.
* No reuse of a cached **note**: notes stay bound to hashes through `member_notes` exactly as before.
* No mtime- or stat-based fast path in the inventory: the scan still hashes every working-tree file
  on every invocation, which is what keeps scoped and unscoped output in agreement.
* No git-ref plumbing beyond reading `HEAD` for the commit label: comparing against a *branch* rather
  than a recorded state is the follow-up in §10, and the state hash is the hook it needs.
