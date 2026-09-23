//! Integration tests for the `treenotes` CLI.
//!
//! Every test runs the real binary against a real temporary Git repository and a temporary
//! SQLite database outside the repository. No test touches a user's real notes database: `HOME`
//! is redirected into the sandbox for every invocation.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_treenotes");

struct Sandbox {
    _tmp: TempDir,
    root: PathBuf,
    repo: PathBuf,
    db: PathBuf,
    home: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = tmp.path().to_path_buf();
        let repo = root.join("repo");
        let home = root.join("home");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&home).unwrap();
        let sandbox = Sandbox {
            db: root.join("db").join("notes.sqlite3"),
            _tmp: tmp,
            root,
            repo,
            home,
        };
        sandbox.git(&["init", "-q"]);
        sandbox.git(&["config", "user.email", "test@example.com"]);
        sandbox.git(&["config", "user.name", "Treenotes Test"]);
        sandbox.git(&["config", "commit.gpgsign", "false"]);
        sandbox
    }

    fn git_raw(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("git runs")
    }

    fn git(&self, args: &[&str]) -> String {
        self.git_in(&self.repo.clone(), args)
    }

    fn db_conn(&self) -> rusqlite::Connection {
        fs::create_dir_all(self.db.parent().unwrap()).unwrap();
        rusqlite::Connection::open(&self.db).unwrap()
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.git_raw(dir, args);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.repo.join(rel)
    }

    fn write(&self, rel: &str, contents: &str) {
        let path = self.path(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn remove(&self, rel: &str) {
        fs::remove_file(self.path(rel)).unwrap();
    }

    fn mkdir(&self, rel: &str) {
        fs::create_dir_all(self.path(rel)).unwrap();
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    fn cmd_in(&self, dir: &Path) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.current_dir(dir).env("HOME", &self.home);
        cmd
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> Output {
        let mut cmd = self.cmd_in(dir);
        cmd.arg("--db").arg(&self.db).args(args);
        cmd.output().expect("treenotes runs")
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in(&self.repo.clone(), args)
    }

    fn run_stdin(&self, args: &[&str], stdin: &str) -> Output {
        let mut cmd = self.cmd_in(&self.repo.clone());
        cmd.arg("--db")
            .arg(&self.db)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("treenotes starts");
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().expect("treenotes finishes")
    }

    fn stdout(&self, out: &Output) -> String {
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "treenotes {args:?} failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stderr.is_empty(),
            "treenotes {args:?} wrote diagnostics on success: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        self.stdout(&out)
    }

    fn fails(&self, args: &[&str], expected_code: i32) -> String {
        let out = self.run(args);
        let code = out.status.code().unwrap_or(-1);
        assert_eq!(
            code,
            expected_code,
            "treenotes {args:?} exit {code}, stdout={}, stderr={}",
            self.stdout(&out),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stderr).to_string()
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.ok(args)).expect("stdout is JSON")
    }

    fn json_in(&self, dir: &Path, args: &[&str]) -> Value {
        let out = self.run_in(dir, args);
        assert!(
            out.status.success(),
            "treenotes {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_str(&self.stdout(&out)).expect("stdout is JSON")
    }

    fn entries(&self, json: &Value) -> Vec<Value> {
        json["entries"].as_array().expect("entries array").clone()
    }

    fn entry(&self, json: &Value, path: &str) -> Value {
        self.entries(json)
            .into_iter()
            .find(|entry| entry["path"] == path)
            .unwrap_or_else(|| panic!("no entry for {path} in {json}"))
    }

    fn hash_of(&self, path: &str) -> String {
        let json = self.json(&["read", path, "--json"]);
        self.entry(&json, path)["hash"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn paths(&self, json: &Value) -> Vec<String> {
        self.entries(json)
            .iter()
            .map(|entry| entry["path"].as_str().unwrap().to_string())
            .collect()
    }

    fn status_map(&self, json: &Value) -> Vec<(String, String)> {
        self.entries(json)
            .iter()
            .map(|entry| {
                (
                    entry["path"].as_str().unwrap().to_string(),
                    entry["status"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    fn annotate(&self, path: &str, note: &str) {
        self.ok(&["set", path, "--note", note]);
    }
}

fn text_lines(text: &str) -> Vec<String> {
    text.lines().map(|line| line.to_string()).collect()
}

fn marker_repo() -> Sandbox {
    let sandbox = Sandbox::new();
    sandbox.write("a.txt", "alpha\n");
    sandbox.write("lib/b.txt", "beta\n");
    sandbox.write("lib/deep/c.txt", "gamma\n");
    sandbox.write("other/x.txt", "delta\n");
    sandbox.commit("init");
    sandbox
}

// -------------------------------------------------------------------------------------------
// Inventory and repository identity
// -------------------------------------------------------------------------------------------

#[test]
fn inventory_covers_git_visible_tree_and_excludes_noise() {
    let sandbox = marker_repo();
    sandbox.write(".gitignore", "ignored.txt\nignored-dir/\n");
    sandbox.write("ignored.txt", "ignore me\n");
    sandbox.write("ignored-dir/secret.txt", "ignore me\n");
    sandbox.write("untracked.txt", "untracked but visible\n");
    sandbox.write("gone.txt", "deleted later\n");
    sandbox.write("my file.txt", "name with spaces\n");
    sandbox.git(&["add", ".gitignore", "gone.txt", "my file.txt"]);
    sandbox.commit("ignore rules");
    sandbox.remove("gone.txt");

    let json = sandbox.json(&["read", "--json"]);
    let paths: BTreeSet<String> = sandbox.paths(&json).into_iter().collect();
    for expected in [
        ".",
        "a.txt",
        "lib",
        "lib/b.txt",
        "lib/deep",
        "lib/deep/c.txt",
        "other",
        "other/x.txt",
        "untracked.txt",
        "my file.txt",
        ".gitignore",
    ] {
        assert!(paths.contains(expected), "missing {expected} in {paths:?}");
    }
    for absent in [
        "gone.txt",
        "ignored.txt",
        "ignored-dir",
        "ignored-dir/secret.txt",
        ".git",
    ] {
        assert!(!paths.contains(absent), "unexpected {absent} in {paths:?}");
    }
    assert_eq!(sandbox.entry(&json, ".")["kind"], "dir");
    assert_eq!(sandbox.entry(&json, "lib")["kind"], "dir");
    assert_eq!(sandbox.entry(&json, "lib/b.txt")["kind"], "file");
    assert_eq!(sandbox.paths(&json)[0], ".");
}

#[test]
fn empty_repository_still_reports_root() {
    let sandbox = Sandbox::new();
    let json = sandbox.json(&["read", "--json"]);
    assert_eq!(sandbox.paths(&json), vec![".".to_string()]);
    assert_eq!(sandbox.entry(&json, ".")["status"], "missing");

    let pending = sandbox.json(&["pending", "--json"]);
    assert_eq!(sandbox.paths(&pending), vec![".".to_string()]);

    sandbox.annotate(".", "empty repository root");
    let read = sandbox.json(&["read", "--json"]);
    assert_eq!(sandbox.entry(&read, ".")["status"], "fresh");

    // A repository with no commits but with an untracked file still lists that file.
    let sandbox = Sandbox::new();
    sandbox.write("loose.txt", "untracked\n");
    let json = sandbox.json(&["read", "--json"]);
    let paths = sandbox.paths(&json);
    assert!(paths.contains(&"loose.txt".to_string()), "{paths:?}");
    assert!(paths.contains(&".".to_string()), "{paths:?}");
}

#[test]
fn git_repository_roots_with_trailing_whitespace_are_preserved() {
    // `git rev-parse --show-toplevel` prints the worktree root verbatim; the root must not be
    // trimmed, or treenotes would look for a different (nonexistent) directory.
    for suffix in [" ", "\n"] {
        let mut sandbox = Sandbox::new();
        sandbox.write("a.txt", "alpha\n");
        sandbox.write("lib/b.txt", "beta\n");
        sandbox.commit("init");

        let renamed = sandbox.root.join(format!("repo{suffix}"));
        fs::rename(&sandbox.repo, &renamed).unwrap();
        sandbox.repo = renamed;
        assert!(!sandbox.root.join("repo").exists());

        let json = sandbox.json(&["read", "--json"]);
        let root = json["repository"]["root"].as_str().unwrap();
        assert_eq!(
            root,
            sandbox.repo.canonicalize().unwrap().to_string_lossy(),
            "worktree root changed: {root:?} for suffix {suffix:?}"
        );
        assert_eq!(sandbox.entry(&json, "a.txt")["status"], "missing");
        assert_eq!(sandbox.entry(&json, "lib/b.txt")["status"], "missing");
        assert!(sandbox.paths(&json).iter().all(|path| !path.contains(' ')));

        // Notes are written and read back in the odd root, and scopes stay root relative.
        sandbox.ok(&["set", "lib/b.txt", "--note", "note in odd root"]);
        let json = sandbox.json(&["read", "lib/b.txt", "--json"]);
        assert_eq!(
            sandbox.entry(&json, "lib/b.txt")["note"],
            "note in odd root"
        );
        assert_eq!(json["scope"]["path"], "lib/b.txt");
        let out = sandbox.run_in(&sandbox.path("lib"), &["read", "lib/b.txt", "--json"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn outside_repository_is_an_environment_error() {
    let sandbox = Sandbox::new();
    let outside = sandbox.root.join("not-a-repo");
    fs::create_dir_all(&outside).unwrap();
    let out = sandbox.run_in(&outside, &["read", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not inside a Git repository"));
}

// -------------------------------------------------------------------------------------------
// Status semantics
// -------------------------------------------------------------------------------------------

#[test]
fn status_is_missing_then_fresh_then_stale_then_fresh_again() {
    let sandbox = marker_repo();
    assert_eq!(
        sandbox.status_map(&sandbox.json(&["read", "--json"]))[1].1,
        "missing"
    );

    let original = sandbox.hash_of("a.txt");
    sandbox.annotate("a.txt", "first version");
    let read = sandbox.json(&["read", "a.txt", "--json"]);
    let entry = sandbox.entry(&read, "a.txt");
    assert_eq!(entry["status"], "fresh");
    assert_eq!(entry["note"], "first version");
    assert_eq!(entry["note_hash"], original.as_str());
    assert_eq!(entry["hash"], original.as_str());

    let pending = sandbox.json(&["pending", "--json"]);
    assert!(!sandbox.paths(&pending).contains(&"a.txt".to_string()));

    sandbox.write("a.txt", "alpha edited\n");
    let read = sandbox.json(&["read", "a.txt", "--json"]);
    let entry = sandbox.entry(&read, "a.txt");
    assert_eq!(entry["status"], "stale");
    assert!(
        entry["note"].is_null(),
        "stale entries must not show a current note"
    );
    assert!(entry["note_hash"].is_null());
    let previous = &entry["previous"];
    assert_eq!(previous["hash"], original.as_str());
    assert_eq!(previous["note"], "first version");
    assert!(previous["updated_at"].as_str().unwrap().ends_with('Z'));

    let pending_text = sandbox.ok(&["pending", "a.txt"]);
    assert!(pending_text.contains("stale"), "{pending_text}");
    assert!(pending_text.contains("previous (stale"), "{pending_text}");
    assert!(
        pending_text.contains("previous (stale, not current)"),
        "{pending_text}"
    );

    // Reverting to the annotated content restores the exact original note.
    sandbox.write("a.txt", "alpha\n");
    let read = sandbox.json(&["read", "a.txt", "--json"]);
    let entry = sandbox.entry(&read, "a.txt");
    assert_eq!(entry["status"], "fresh");
    assert_eq!(entry["note"], "first version");
    assert_eq!(entry["hash"], original.as_str());
}

#[test]
fn the_latest_note_version_is_the_previous_note_of_a_stale_entry() {
    let sandbox = marker_repo();
    let hash_v1 = sandbox.hash_of("a.txt");
    sandbox.ok(&[
        "set",
        "a.txt",
        "--note",
        "note v1",
        "--expected-hash",
        &hash_v1,
    ]);

    sandbox.write("a.txt", "alpha v2\n");
    let hash_v2 = sandbox.hash_of("a.txt");
    sandbox.ok(&[
        "set",
        "a.txt",
        "--note",
        "note v2",
        "--expected-hash",
        &hash_v2,
    ]);
    assert_ne!(hash_v1, hash_v2);

    // A third, unannotated version: the previous note is the most recently written one.
    sandbox.write("a.txt", "alpha v3\n");
    let entry = sandbox.entry(&sandbox.json(&["read", "a.txt", "--json"]), "a.txt");
    assert_eq!(entry["status"], "stale");
    assert_eq!(entry["previous"]["note"], "note v2");
    assert_eq!(entry["previous"]["hash"], hash_v2.as_str());

    // Rewriting an older version adds a new version without losing the historical one.
    sandbox.write("a.txt", "alpha\n");
    let entry = sandbox.entry(&sandbox.json(&["read", "a.txt", "--json"]), "a.txt");
    assert_eq!(entry["note"], "note v1");
    sandbox.ok(&[
        "set",
        "a.txt",
        "--note",
        "note v1 rewritten",
        "--expected-hash",
        &hash_v1,
    ]);
    let entry = sandbox.entry(&sandbox.json(&["read", "a.txt", "--json"]), "a.txt");
    assert_eq!(entry["note"], "note v1 rewritten");

    // Two note versions written in the very same instant: the later row must win, so a stale
    // entry never shows an older note just because its timestamp collided.
    sandbox.write("a.txt", "alpha tiebreak\n");
    let conn = sandbox.db_conn();
    let repository: String = conn
        .query_row("SELECT DISTINCT repository FROM notes LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    for (hash, note) in [
        ("tnt1:file:first", "same instant first"),
        ("tnt1:file:second", "same instant second"),
    ] {
        conn.execute(
            // A future timestamp, so the tie-break is decided by insertion order alone.
            "INSERT INTO notes (repository, path, kind, hash, note, updated_at) \
             VALUES (?1, 'a.txt', 'file', ?2, ?3, '2099-01-01T00:00:00.000000000Z')",
            rusqlite::params![repository, hash, note],
        )
        .unwrap();
    }
    drop(conn);
    let entry = sandbox.entry(&sandbox.json(&["read", "a.txt", "--json"]), "a.txt");
    assert_eq!(entry["status"], "stale");
    assert_eq!(entry["previous"]["note"], "same instant second");
    assert_eq!(entry["previous"]["hash"], "tnt1:file:second");
    let pending = sandbox.ok(&["pending", "a.txt"]);
    assert!(pending.contains("same instant second"), "{pending}");
    assert!(!pending.contains("same instant first"), "{pending}");

    // A rewrite is the latest write even if older records have a future timestamp.
    sandbox.write("a.txt", "alpha\n");
    sandbox.annotate("a.txt", "latest single write");
    sandbox.write("a.txt", "alpha unannotated\n");
    let entry = sandbox.entry(&sandbox.json(&["read", "a.txt", "--json"]), "a.txt");
    assert_eq!(entry["previous"]["note"], "latest single write");

    sandbox.write("a.txt", "alpha v2\n");
    let batch = serde_json::json!([
        {"path": "a.txt", "hash": hash_v2, "note": "latest batch write"}
    ]);
    let out = sandbox.run_stdin(&["import"], &batch.to_string());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    sandbox.write("a.txt", "alpha unannotated\n");
    let entry = sandbox.entry(&sandbox.json(&["read", "a.txt", "--json"]), "a.txt");
    assert_eq!(entry["previous"]["note"], "latest batch write");
}

#[test]
fn directory_hashes_propagate_to_ancestors_only() {
    let sandbox = marker_repo();
    let before = sandbox.json(&["read", "--json"]);
    let root_hash = sandbox.entry(&before, ".")["hash"]
        .as_str()
        .unwrap()
        .to_string();
    let other_hash = sandbox.entry(&before, "other")["hash"]
        .as_str()
        .unwrap()
        .to_string();

    // The same tree hashed twice is identical: directory hashes are deterministic.
    let again = sandbox.json(&["read", "--json"]);
    assert_eq!(sandbox.entry(&again, ".")["hash"], root_hash.as_str());

    sandbox.write("lib/deep/c.txt", "gamma edited\n");
    let after = sandbox.json(&["read", "--json"]);
    assert_ne!(sandbox.entry(&after, ".")["hash"], root_hash.as_str());
    assert_ne!(
        sandbox.entry(&after, "lib")["hash"],
        before["entries"][2]["hash"]
    );
    assert_eq!(sandbox.entry(&after, "other")["hash"], other_hash.as_str());

    // Unchanged siblings and their notes are untouched.
    sandbox.annotate("other/x.txt", "sibling file");
    sandbox.annotate("other", "sibling directory");
    sandbox.write("lib/deep/c.txt", "gamma edited again\n");
    let pending = sandbox.json(&["pending", "--json"]);
    let pending_paths = sandbox.paths(&pending);
    assert!(pending_paths.contains(&"lib/deep/c.txt".to_string()));
    assert!(pending_paths.contains(&"lib/deep".to_string()));
    assert!(pending_paths.contains(&"lib".to_string()));
    assert!(pending_paths.contains(&".".to_string()));
    assert!(!pending_paths.contains(&"other".to_string()));
    assert!(!pending_paths.contains(&"other/x.txt".to_string()));
}

#[test]
fn additions_removals_and_renames_invalidate_the_right_paths() {
    let sandbox = marker_repo();
    sandbox.annotate("a.txt", "alpha note");
    let hash = sandbox.hash_of("a.txt");

    sandbox.git(&["mv", "a.txt", "renamed.txt"]);
    sandbox.commit("rename");
    let json = sandbox.json(&["read", "--json"]);
    let paths = sandbox.paths(&json);
    assert!(paths.contains(&"renamed.txt".to_string()));
    assert!(!paths.contains(&"a.txt".to_string()));
    assert_eq!(
        sandbox.hash_of("renamed.txt"),
        hash,
        "content is path independent"
    );
    // Identical bytes at a new path is a new, unannotated entry (path identity).
    assert_eq!(sandbox.entry(&json, "renamed.txt")["status"], "missing");

    sandbox.annotate(".", "repository root");
    sandbox.write("added.txt", "new file\n");
    sandbox.commit("add");
    let json = sandbox.json(&["read", "--json"]);
    assert_eq!(sandbox.entry(&json, "added.txt")["status"], "missing");
    assert_eq!(sandbox.entry(&json, ".")["status"], "stale");
    assert_eq!(
        sandbox.entry(&json, ".")["previous"]["note"],
        "repository root"
    );

    fs::remove_file(sandbox.path("added.txt")).unwrap();
    sandbox.commit("remove");
    let json = sandbox.json(&["read", "--json"]);
    assert!(!sandbox.paths(&json).contains(&"added.txt".to_string()));
}

#[test]
fn identical_bytes_at_different_paths_keep_separate_notes() {
    let sandbox = Sandbox::new();
    sandbox.write("one.txt", "same bytes\n");
    sandbox.write("two.txt", "same bytes\n");
    sandbox.commit("identical");
    assert_eq!(sandbox.hash_of("one.txt"), sandbox.hash_of("two.txt"));

    sandbox.annotate("one.txt", "first purpose");
    let json = sandbox.json(&["read", "--json"]);
    assert_eq!(sandbox.entry(&json, "one.txt")["status"], "fresh");
    assert_eq!(sandbox.entry(&json, "two.txt")["status"], "missing");

    sandbox.annotate("two.txt", "second purpose");
    let json = sandbox.json(&["read", "--json"]);
    assert_eq!(sandbox.entry(&json, "one.txt")["note"], "first purpose");
    assert_eq!(sandbox.entry(&json, "two.txt")["note"], "second purpose");
}

// -------------------------------------------------------------------------------------------
// Repository identity across worktrees
// -------------------------------------------------------------------------------------------

#[test]
fn linked_worktrees_share_notes_for_matching_content_only() {
    let sandbox = marker_repo();
    sandbox.annotate("a.txt", "main worktree note");
    let hash = sandbox.hash_of("a.txt");

    let linked = sandbox.root.join("linked");
    sandbox.git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "linked-branch",
        linked.to_str().unwrap(),
    ]);

    let json = sandbox.json_in(&linked, &["read", "a.txt", "--json"]);
    assert_eq!(sandbox.entry(&json, "a.txt")["status"], "fresh");
    assert_eq!(sandbox.entry(&json, "a.txt")["note"], "main worktree note");
    assert_eq!(sandbox.entry(&json, "a.txt")["hash"], hash.as_str());

    let main_identity = sandbox.json(&["read", "--json"])["repository"]["identity"]
        .as_str()
        .unwrap()
        .to_string();
    let linked_identity = sandbox.json_in(&linked, &["read", "--json"])["repository"]["identity"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(main_identity, linked_identity);
    let linked_root = sandbox.json_in(&linked, &["read", "--json"])["repository"]["root"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        linked_root,
        linked.canonicalize().unwrap().to_string_lossy()
    );

    // Divergent content in the linked worktree gets its own note version.
    fs::write(linked.join("a.txt"), "divergent\n").unwrap();
    let divergent = sandbox.json_in(&linked, &["read", "a.txt", "--json"]);
    let entry = sandbox.entry(&divergent, "a.txt");
    assert_eq!(entry["status"], "stale");
    assert_eq!(entry["previous"]["note"], "main worktree note");
    let divergent_hash = entry["hash"].as_str().unwrap().to_string();
    let note = serde_json::json!([{ "path": "a.txt", "hash": divergent_hash, "note": "linked worktree note" }]);
    let batch = sandbox.root.join("linked-batch.json");
    fs::write(&batch, note.to_string()).unwrap();
    let out = sandbox.run_in(&linked, &["import", batch.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The two worktrees keep their own note for their own content.
    let main = sandbox.json(&["read", "a.txt", "--json"]);
    assert_eq!(sandbox.entry(&main, "a.txt")["note"], "main worktree note");
    assert_eq!(sandbox.entry(&main, "a.txt")["status"], "fresh");
    let linked_read = sandbox.json_in(&linked, &["read", "a.txt", "--json"]);
    assert_eq!(
        sandbox.entry(&linked_read, "a.txt")["note"],
        "linked worktree note"
    );
}

#[test]
fn unrelated_repositories_are_isolated() {
    let sandbox = marker_repo();
    sandbox.annotate("a.txt", "repository one note");

    let other_repo = sandbox.root.join("other-repo");
    fs::create_dir_all(&other_repo).unwrap();
    sandbox.git_in(&other_repo, &["init", "-q"]);
    sandbox.git_in(&other_repo, &["config", "user.email", "test@example.com"]);
    sandbox.git_in(&other_repo, &["config", "user.name", "Treenotes Test"]);
    fs::write(other_repo.join("a.txt"), "alpha\n").unwrap();
    sandbox.git_in(&other_repo, &["add", "-A"]);
    sandbox.git_in(&other_repo, &["commit", "-q", "-m", "init"]);

    let json = sandbox.json_in(&other_repo, &["read", "a.txt", "--json"]);
    assert_eq!(sandbox.entry(&json, "a.txt")["status"], "missing");
    let identity = json["repository"]["identity"].as_str().unwrap();
    let main_identity = sandbox.json(&["read", "--json"])["repository"]["identity"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(identity, main_identity);
}

// -------------------------------------------------------------------------------------------
// Scopes, ordering, and text output
// -------------------------------------------------------------------------------------------

#[test]
fn pending_orders_children_before_parents_and_omits_fresh_entries() {
    let sandbox = Sandbox::new();
    sandbox.write("a.txt", "alpha\n");
    sandbox.write("d/f.txt", "f\n");
    sandbox.write("d/g.txt", "g\n");
    sandbox.commit("init");

    let json = sandbox.json(&["pending", "--json"]);
    assert_eq!(
        sandbox.paths(&json),
        vec!["a.txt", "d/f.txt", "d/g.txt", "d", "."]
    );
    assert_eq!(json["version"], 3);
    assert_eq!(json["tool"], "treenotes");
    assert_eq!(json["command"], "pending");
    assert_eq!(json["scope"]["path"], ".");

    sandbox.annotate("a.txt", "alpha");
    sandbox.annotate("d/f.txt", "f");
    sandbox.annotate("d/g.txt", "g");
    sandbox.annotate("d", "directory d");
    sandbox.annotate(".", "repository root");
    assert_eq!(
        sandbox.paths(&sandbox.json(&["pending", "--json"])).len(),
        0
    );
    let out = sandbox.run(&["pending"]);
    assert!(out.status.success());
    assert_eq!(sandbox.stdout(&out), "");
}

#[test]
fn read_scopes_depth_and_subdirectory_invocation() {
    let sandbox = marker_repo();
    sandbox.annotate("lib/b.txt", "beta note");

    // Scope a single file.
    let json = sandbox.json(&["read", "lib/b.txt", "--json"]);
    assert_eq!(sandbox.paths(&json), vec!["lib/b.txt"]);
    assert_eq!(json["scope"]["path"], "lib/b.txt");
    assert_eq!(json["scope"]["kind"], "file");

    // Scope a subtree, and bound it with --depth.
    let json = sandbox.json(&["read", "lib", "--json"]);
    assert_eq!(
        sandbox.paths(&json),
        vec!["lib", "lib/b.txt", "lib/deep", "lib/deep/c.txt"]
    );
    let json = sandbox.json(&["read", "lib", "--depth", "1", "--json"]);
    assert_eq!(sandbox.paths(&json), vec!["lib", "lib/b.txt", "lib/deep"]);
    assert_eq!(json["scope"]["depth"], 1);
    let json = sandbox.json(&["read", "lib", "--depth", "0", "--json"]);
    assert_eq!(sandbox.paths(&json), vec!["lib"]);

    // Relative paths are strictly repository-root relative: from a subdirectory the same
    // spelling a root-level caller would use names the same entry, and a bare
    // subdirectory-relative name is never re-interpreted against the caller's directory.
    let json = sandbox.json_in(
        &sandbox.path("lib/deep"),
        &["read", "lib/deep/c.txt", "--json"],
    );
    assert_eq!(sandbox.paths(&json), vec!["lib/deep/c.txt"]);
    assert_eq!(json["scope"]["path"], "lib/deep/c.txt");
    let json = sandbox.json_in(&sandbox.path("lib/deep"), &["read", "lib/b.txt", "--json"]);
    assert_eq!(sandbox.paths(&json), vec!["lib/b.txt"]);
    let json = sandbox.json_in(&sandbox.path("lib/deep"), &["read", "--json"]);
    assert_eq!(sandbox.paths(&json)[0], ".");
    let out = sandbox.run_in(&sandbox.path("lib"), &["pending", "lib/b.txt"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // `c.txt` from inside `lib/deep` is the root-level spelling of a path that does not exist,
    // not the cwd-relative `lib/deep/c.txt`; `b.txt` from inside `lib` is rejected the same way.
    let out = sandbox.run_in(&sandbox.path("lib/deep"), &["read", "c.txt", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("does not exist"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = sandbox.run_in(&sandbox.path("lib"), &["read", "b.txt"]);
    assert_eq!(out.status.code(), Some(1));
    let out = sandbox.run_in(
        &sandbox.path("lib"),
        &["set", "b.txt", "--note", "cwd relative"],
    );
    assert_eq!(out.status.code(), Some(1));

    // Text output is an indented map rooted at the scope.
    let text = sandbox.ok(&["read", "lib"]);
    let lines = text_lines(&text);
    assert!(lines[0].starts_with("lib [dir]"), "{lines:?}");
    assert!(
        !lines[0].starts_with([' ', '│', '├', '└']),
        "the scope is the tree root and is never indented: {lines:?}"
    );
    // Every other line is drawn with a branch connector, so nesting is always visible.
    assert!(
        lines[1..].iter().all(|line| line.contains("── ")),
        "{lines:?}"
    );
    assert!(lines[1].starts_with("├── lib/b.txt [file]"), "{lines:?}");
    assert!(
        lines.iter().any(|line| line.contains("\"beta note\"")),
        "{lines:?}"
    );

    // The absolute path of the repository root is the root scope.
    let json = sandbox.json(&["read", sandbox.repo.to_str().unwrap(), "--json"]);
    assert_eq!(json["scope"]["path"], ".");
    assert!(sandbox.paths(&json).len() > 1);

    // Invalid, ignored, missing and outside scopes are usage errors with distinct messages.
    sandbox.write(".gitignore", "ignored.txt\n");
    sandbox.commit("ignore");
    sandbox.write("ignored.txt", "ignore me\n");
    assert!(sandbox
        .fails(&["read", "ignored.txt"], 1)
        .contains("not part of its Git-visible tree"));
    assert!(sandbox
        .fails(&["read", "missing.txt"], 1)
        .contains("does not exist"));
    assert!(sandbox
        .fails(&["read", "../outside"], 1)
        .contains("outside the repository"));
    assert!(sandbox
        .fails(&["read", sandbox.root.to_str().unwrap()], 1)
        .contains("outside the repository"));
}

#[test]
fn only_filters_a_listing_to_the_named_directories() {
    let sandbox = marker_repo();
    sandbox.annotate("lib/b.txt", "beta note");

    // The unfiltered listing is the whole scope, and it says so: no filter, so `filters` is null.
    let json = sandbox.json(&["read", "--json"]);
    assert_eq!(
        sandbox.paths(&json),
        vec![
            ".",
            "a.txt",
            "lib",
            "lib/b.txt",
            "lib/deep",
            "lib/deep/c.txt",
            "other",
            "other/x.txt"
        ]
    );
    assert!(json["scope"]["filters"].is_null());

    // `--only` keeps the named subtree plus every directory between the scope and it.
    let json = sandbox.json(&["read", "--only", "lib", "--json"]);
    assert_eq!(
        sandbox.paths(&json),
        vec![".", "lib", "lib/b.txt", "lib/deep", "lib/deep/c.txt"]
    );
    assert_eq!(json["scope"]["path"], ".");
    assert_eq!(json["scope"]["filters"], serde_json::json!(["lib"]));

    // The filter is a window over the scope, never a second scope: the scope, the depth origin and
    // the repository-root relative spellings are unchanged by it.
    let json = sandbox.json(&["read", "lib", "--only", "lib/deep", "--json"]);
    assert_eq!(
        sandbox.paths(&json),
        vec!["lib", "lib/deep", "lib/deep/c.txt"]
    );
    assert_eq!(json["scope"]["path"], "lib");
    assert_eq!(json["scope"]["filters"], serde_json::json!(["lib/deep"]));
    let json = sandbox.json(&[
        "read", "lib", "--only", "lib/deep", "--depth", "1", "--json",
    ]);
    assert_eq!(sandbox.paths(&json), vec!["lib", "lib/deep"]);

    // Several trees at once, in one comma-separated flag or repeated; both name the same window.
    let json = sandbox.json(&["read", "--only", "lib,other", "--json"]);
    assert_eq!(
        sandbox.paths(&json),
        vec![
            ".",
            "lib",
            "lib/b.txt",
            "lib/deep",
            "lib/deep/c.txt",
            "other",
            "other/x.txt"
        ]
    );
    assert_eq!(
        json["scope"]["filters"],
        serde_json::json!(["lib", "other"])
    );
    let repeated = sandbox.json(&["read", "--only", "other", "--only", "lib", "--json"]);
    assert_eq!(sandbox.paths(&repeated), sandbox.paths(&json));

    // Text output is the same windowed tree, still rooted at the scope line.
    let lines = text_lines(&sandbox.ok(&["read", "--only", "lib"]));
    assert!(lines[0].starts_with(". [dir]"), "{lines:?}");
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert!(
        lines
            .iter()
            .all(|line| !line.contains("a.txt") && !line.contains("other")),
        "{lines:?}"
    );

    // `pending` filters identically, and `--all` is the unfiltered listing spelled out.
    let json = sandbox.json(&["pending", "--only", "lib", "--json"]);
    assert_eq!(
        sandbox.paths(&json),
        vec!["lib/deep/c.txt", "lib/deep", "lib", "."]
    );
    assert_eq!(json["scope"]["filters"], serde_json::json!(["lib"]));
    let all = sandbox.json(&["pending", "--all", "--json"]);
    assert_eq!(
        sandbox.paths(&all),
        vec![
            "a.txt",
            "lib/deep/c.txt",
            "lib/deep",
            "lib",
            "other/x.txt",
            "other",
            "."
        ]
    );
    assert!(all["scope"]["filters"].is_null());

    // `--members` follows the same window: a file the filter excluded is not even parsed.
    sandbox.write("lib/inner.py", &fixture("tests/fixtures/python/service.py"));
    sandbox.write("other/App.java", &fixture("tests/fixtures/java/Cache.java"));
    sandbox.commit("sources");
    let json = sandbox.json(&["read", "--only", "lib", "--members", "--json"]);
    let members = sandbox.members(&json);
    assert!(!members.is_empty());
    assert!(
        members
            .iter()
            .all(|member| member["path"] == "lib/inner.py"),
        "{members:?}"
    );
    assert_eq!(json["parse_error"], false);

    // A filter naming a file, a missing path, or a path the scope excludes is invalid input; the
    // contradictory pair `--only`/`--all` is a command line usage error.
    assert!(sandbox
        .fails(&["read", "--only", "a.txt"], 1)
        .contains("takes a directory"));
    assert!(sandbox
        .fails(&["read", "--only", "nope"], 1)
        .contains("does not exist"));
    assert!(sandbox
        .fails(&["read", "lib", "--only", "other"], 1)
        .contains("outside the scope"));
    let out = sandbox.run(&["read", "--only", "lib", "--all"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn root_relative_paths_win_over_same_named_files_in_subdirectories() {
    let sandbox = Sandbox::new();
    sandbox.write("a.txt", "root alpha\n");
    sandbox.write("sub/a.txt", "nested alpha\n");
    sandbox.commit("init");

    let root_hash = sandbox.hash_of("a.txt");
    let nested_hash = sandbox.hash_of("sub/a.txt");
    assert_ne!(root_hash, nested_hash);

    // From inside `sub`, a bare `a.txt` still selects the repository-root file.
    let json = sandbox.json_in(&sandbox.path("sub"), &["read", "a.txt", "--json"]);
    assert_eq!(json["scope"]["path"], "a.txt");
    assert_eq!(sandbox.paths(&json), vec!["a.txt"]);
    assert_eq!(sandbox.entry(&json, "a.txt")["hash"], root_hash.as_str());

    let json = sandbox.json_in(&sandbox.path("sub"), &["pending", "a.txt", "--json"]);
    assert_eq!(json["scope"]["path"], "a.txt");
    assert_eq!(sandbox.paths(&json), vec!["a.txt"]);
    assert_eq!(sandbox.entry(&json, "a.txt")["hash"], root_hash.as_str());

    // `set` from inside `sub` annotates the root file only.
    let out = sandbox.run_in(
        &sandbox.path("sub"),
        &["set", "a.txt", "--note", "root note"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        sandbox.entry(&sandbox.json(&["read", "a.txt", "--json"]), "a.txt")["note"],
        "root note"
    );
    let nested = sandbox.json(&["read", "sub/a.txt", "--json"]);
    assert_eq!(sandbox.entry(&nested, "sub/a.txt")["status"], "missing");
    assert!(sandbox.entry(&nested, "sub/a.txt")["note"].is_null());

    // Exported JSON paths round-trip through `set` unchanged, even from inside `sub`.
    let pending = sandbox.json_in(&sandbox.path("sub"), &["pending", "--json"]);
    let record = sandbox.entry(&pending, "sub/a.txt");
    let out = sandbox.run_in(
        &sandbox.path("sub"),
        &[
            "set",
            record["path"].as_str().unwrap(),
            "--note",
            "nested note",
            "--expected-hash",
            record["hash"].as_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let nested = sandbox.json_in(&sandbox.path("sub"), &["read", "sub/a.txt", "--json"]);
    assert_eq!(sandbox.entry(&nested, "sub/a.txt")["status"], "fresh");
    assert_eq!(sandbox.entry(&nested, "sub/a.txt")["note"], "nested note");
    let root = sandbox.json(&["read", "a.txt", "--json"]);
    assert_eq!(sandbox.entry(&root, "a.txt")["status"], "fresh");
    assert_eq!(sandbox.entry(&root, "a.txt")["note"], "root note");
}

#[test]
fn json_output_is_deterministic_and_never_contains_source_text() {
    let sandbox = marker_repo();
    sandbox.write("secret.txt", "SECRET_SOURCE_MARKER\n");
    sandbox.commit("secret");
    sandbox.annotate("secret.txt", "a note about the secret file");

    let first = sandbox.ok(&["read", "--json"]);
    let second = sandbox.ok(&["read", "--json"]);
    assert_eq!(first, second, "JSON output must be byte-stable");
    assert!(!first.contains("SECRET_SOURCE_MARKER"));
    let pending = sandbox.ok(&["pending", "--json"]);
    assert!(!pending.contains("SECRET_SOURCE_MARKER"));

    // Exported notes are one-line summaries, not file contents.
    let multiline = sandbox.run(&["set", "a.txt", "--note", "first\nSECRET_SOURCE_MARKER"]);
    assert_eq!(multiline.status.code(), Some(1));
    let from_stdin = sandbox.run_stdin(&["set", "a.txt"], "note from stdin\n");
    assert!(
        from_stdin.status.success(),
        "{}",
        String::from_utf8_lossy(&from_stdin.stderr)
    );
    let json = sandbox.json(&["read", "a.txt", "--json"]);
    assert_eq!(sandbox.entry(&json, "a.txt")["note"], "note from stdin");
}

// -------------------------------------------------------------------------------------------
// Writes
// -------------------------------------------------------------------------------------------

#[test]
fn set_validates_notes_and_hash_guards() {
    let sandbox = marker_repo();
    let hash = sandbox.hash_of("a.txt");

    assert!(sandbox
        .fails(&["set", "a.txt", "--note", ""], 1)
        .contains("must not be empty"));
    assert!(sandbox
        .fails(&["set", "a.txt", "--note", "   \t "], 1)
        .contains("must not be empty"));
    let blank = sandbox.run_stdin(&["set", "a.txt"], " \t \n");
    assert_eq!(blank.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&blank.stderr).contains("must not be empty"),
        "{}",
        String::from_utf8_lossy(&blank.stderr)
    );
    assert!(sandbox
        .fails(&["set", "a.txt", "--note", "two\nlines"], 1)
        .contains("single line"));
    sandbox.fails(&["set", "missing.txt", "--note", "nope"], 1);
    sandbox.fails(&["set", "ignored-path", "--note", "nope"], 1);

    sandbox.ok(&[
        "set",
        "a.txt",
        "--note",
        "guarded note",
        "--expected-hash",
        &hash,
    ]);
    let json = sandbox.json(&["read", "a.txt", "--json"]);
    assert_eq!(sandbox.entry(&json, "a.txt")["status"], "fresh");

    // The guard rejects a hash that no longer matches: no write happened.
    sandbox.write("a.txt", "alpha changed\n");
    let message = sandbox.fails(
        &[
            "set",
            "a.txt",
            "--note",
            "stale write",
            "--expected-hash",
            &hash,
        ],
        1,
    );
    assert!(
        message.contains("does not match the current hash"),
        "{message}"
    );
    let json = sandbox.json(&["read", "a.txt", "--json"]);
    let entry = sandbox.entry(&json, "a.txt");
    assert_eq!(entry["status"], "stale");
    assert_eq!(entry["previous"]["note"], "guarded note");
    assert_eq!(entry["previous"]["hash"], hash.as_str());

    // Writing the current hash with the guard succeeds, and a directory can be annotated too.
    let current = sandbox.hash_of("a.txt");
    sandbox.ok(&[
        "set",
        "a.txt",
        "--note",
        "new content note",
        "--expected-hash",
        &current,
    ]);
    sandbox.annotate("lib", "library directory");
    let json = sandbox.json(&["read", "lib", "--json"]);
    assert_eq!(sandbox.entry(&json, "lib")["kind"], "dir");
    assert_eq!(sandbox.entry(&json, "lib")["status"], "fresh");

    // Quotes and control characters are escaped in text output and survive JSON exactly.
    let tricky = "quotes \" and tab\there";
    sandbox.ok(&["set", "a.txt", "--note", tricky]);
    let text = sandbox.ok(&["read", "a.txt"]);
    assert!(text.contains("quotes \\\" and tab\\there"), "{text}");
    assert_eq!(
        text.matches('\t').count(),
        0,
        "raw controls leaked: {text:?}"
    );
    let json = sandbox.json(&["read", "a.txt", "--json"]);
    assert_eq!(sandbox.entry(&json, "a.txt")["note"], tricky);
    assert_eq!(sandbox.entry(&json, "a.txt")["status"], "fresh");
}

#[test]
fn import_is_atomic_and_validates_every_record() {
    let sandbox = marker_repo();
    let a = sandbox.hash_of("a.txt");
    let lib = sandbox.hash_of("lib");

    let batch = sandbox.root.join("batch.json");
    fs::write(
        &batch,
        serde_json::json!([
            { "path": "a.txt", "hash": a, "note": "alpha note" },
            { "path": "lib", "hash": lib, "note": "library note" }
        ])
        .to_string(),
    )
    .unwrap();
    let out = sandbox.run(&["import", batch.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(sandbox.stdout(&out).trim(), "imported 2 notes");
    let json = sandbox.json(&["read", "--json"]);
    assert_eq!(sandbox.entry(&json, "a.txt")["note"], "alpha note");
    assert_eq!(sandbox.entry(&json, "lib")["note"], "library note");

    // A batch whose second record is wrong writes nothing at all.
    let a_now = sandbox.hash_of("a.txt");
    let rollback = sandbox.root.join("rollback.json");
    fs::write(
        &rollback,
        serde_json::json!([
            { "path": "a.txt", "hash": a_now, "note": "should not persist" },
            { "path": "lib/b.txt", "hash": "tnt1:file:deadbeef", "note": "bad hash" }
        ])
        .to_string(),
    )
    .unwrap();
    let message = sandbox.fails(&["import", rollback.to_str().unwrap()], 1);
    assert!(
        message.contains("does not match the current hash"),
        "{message}"
    );
    let json = sandbox.json(&["read", "a.txt", "--json"]);
    assert_eq!(sandbox.entry(&json, "a.txt")["note"], "alpha note");
    let conn = sandbox.db_conn();
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM notes WHERE note = 'should not persist'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "failed import must not write any record");

    // Duplicate records, unsafe paths, malformed records and multiline notes are all rejected.
    let cases: Vec<&str> = vec![
        r#"[{"path":"a.txt","hash":"H","note":"n"},{"path":"a.txt","hash":"H","note":"n"}]"#,
        r#"[{"path":"/etc/passwd","hash":"H","note":"n"}]"#,
        r#"[{"path":"../escape.txt","hash":"H","note":"n"}]"#,
        r#"[{"path":"lib/../a.txt","hash":"H","note":"n"}]"#,
        r#"[{"path":"lib//b.txt","hash":"H","note":"n"}]"#,
        r#"[{"path":"a.txt","hash":"H","note":"one\ntwo"}]"#,
        r#"[{"path":"a.txt","hash":"H","note":"  \t  "}]"#,
        r#"[{"path":"a.txt","hash":"H"}]"#,
        r#"[{"path":"a.txt","hash":"H","note":"n","extra":1}]"#,
        r#"{"path":"a.txt"}"#,
        r#"[]"#,
        r#"not json"#,
    ];
    for case in cases {
        let path = sandbox.root.join("case.json");
        fs::write(&path, case).unwrap();
        let out = sandbox.run(&["import", path.to_str().unwrap()]);
        assert_eq!(
            out.status.code(),
            Some(1),
            "batch {case} should be rejected, stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // A whitespace-only note is rejected exactly like an empty one.
    let blank = sandbox.root.join("blank.json");
    fs::write(&blank, r#"[{"path":"a.txt","hash":"H","note":"  \t  "}]"#).unwrap();
    assert!(sandbox
        .fails(&["import", blank.to_str().unwrap()], 1)
        .contains("must not be empty"));

    // The duplicate case is caught after normalisation.
    let dup = sandbox.root.join("dup.json");
    fs::write(
        &dup,
        serde_json::json!([
            { "path": "a.txt", "hash": a_now, "note": "one" },
            { "path": "a.txt", "hash": a_now, "note": "two" }
        ])
        .to_string(),
    )
    .unwrap();
    assert!(sandbox
        .fails(&["import", dup.to_str().unwrap()], 1)
        .contains("duplicate path"));

    // A valid batch over stdin works, and reports counts in JSON.
    let stdin_batch = serde_json::json!([
        { "path": "other/x.txt", "hash": sandbox.hash_of("other/x.txt"), "note": "from stdin" }
    ])
    .to_string();
    let out = sandbox.run_stdin(&["import", "-"], &stdin_batch);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = sandbox.json(&["read", "other/x.txt", "--json"]);
    assert_eq!(sandbox.entry(&json, "other/x.txt")["note"], "from stdin");

    let stdin_json = serde_json::json!([
        { "path": "lib/deep/c.txt", "hash": sandbox.hash_of("lib/deep/c.txt"), "note": "stdin json" }
    ])
    .to_string();
    let out = sandbox.run_stdin(&["import", "-", "--json"], &stdin_json);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: Value = serde_json::from_str(&sandbox.stdout(&out)).unwrap();
    assert_eq!(json["command"], "import");
    assert_eq!(json["imported"], 1);
    assert_eq!(json["entries"][0]["status"], "fresh");
}

#[test]
fn hash_guards_bind_notes_to_the_snapshot_they_were_derived_from() {
    let sandbox = marker_repo();
    let snapshot = sandbox.hash_of("a.txt");
    sandbox.ok(&[
        "set",
        "a.txt",
        "--note",
        "note for the snapshot",
        "--expected-hash",
        &snapshot,
    ]);
    // The file changes after the write: the note stays bound to the snapshot hash and the
    // entry becomes stale rather than silently inheriting the new content.
    sandbox.write("a.txt", "alpha changed after the note\n");
    let json = sandbox.json(&["read", "a.txt", "--json"]);
    let entry = sandbox.entry(&json, "a.txt");
    assert_eq!(entry["status"], "stale");
    assert_eq!(entry["previous"]["hash"], snapshot.as_str());
    assert_eq!(entry["previous"]["note"], "note for the snapshot");
    assert_ne!(entry["hash"], snapshot.as_str());
}

// -------------------------------------------------------------------------------------------
// Filesystem edges
// -------------------------------------------------------------------------------------------

#[test]
fn symlinks_hash_their_target_and_are_never_followed() {
    let sandbox = marker_repo();
    let outside = sandbox.root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("outside.txt"), "outside content\n").unwrap();

    std::os::unix::fs::symlink("a.txt", sandbox.path("link-to-a")).unwrap();
    std::os::unix::fs::symlink(
        outside.join("outside.txt"),
        sandbox.path("link-outside-file"),
    )
    .unwrap();
    std::os::unix::fs::symlink(&outside, sandbox.path("link-outside-dir")).unwrap();

    let json = sandbox.json(&["read", "--json"]);
    let paths = sandbox.paths(&json);
    for link in ["link-to-a", "link-outside-file", "link-outside-dir"] {
        assert!(paths.contains(&link.to_string()), "{paths:?}");
        assert_eq!(sandbox.entry(&json, link)["kind"], "symlink");
    }
    assert!(
        !paths
            .iter()
            .any(|path| path.starts_with("link-outside-dir/")),
        "symlinked directories outside the repository must not be traversed: {paths:?}"
    );
    assert!(!paths.iter().any(|path| path.contains("outside.txt")));

    // The hash follows the target text, not the target contents.
    let before = sandbox.hash_of("link-to-a");
    sandbox.write("a.txt", "alpha rewritten\n");
    assert_eq!(sandbox.hash_of("link-to-a"), before);
    fs::remove_file(sandbox.path("link-to-a")).unwrap();
    std::os::unix::fs::symlink("lib/b.txt", sandbox.path("link-to-a")).unwrap();
    assert_ne!(sandbox.hash_of("link-to-a"), before);

    sandbox.annotate("link-outside-file", "symlink out of the tree");
    let json = sandbox.json(&["read", "link-outside-file", "--json"]);
    assert_eq!(sandbox.entry(&json, "link-outside-file")["status"], "fresh");
}

#[test]
fn indexed_descendants_of_a_symlinked_directory_fail_closed() {
    let sandbox = Sandbox::new();
    sandbox.write("sub/a", "tracked content\n");
    sandbox.commit("init");

    // Git still lists `sub/a` from the index, but the directory on the way to it has been
    // replaced by a symlink pointing outside the repository.
    let external = sandbox.root.join("external");
    fs::create_dir_all(&external).unwrap();
    fs::write(external.join("a"), "external content\n").unwrap();
    fs::remove_dir_all(sandbox.path("sub")).unwrap();
    std::os::unix::fs::symlink(&external, sandbox.path("sub")).unwrap();

    // The scan fails closed with an environment error naming the symlink ancestor.
    let message = sandbox.fails(&["read", "sub/a"], 2);
    assert!(message.contains("ancestor"), "{message}");
    assert!(message.contains("symlink"), "{message}");
    assert!(message.contains("sub/a"), "{message}");
    assert!(sandbox.fails(&["pending"], 2).contains("symlink"));
    assert!(sandbox
        .fails(&["set", "sub/a", "--note", "nope"], 2)
        .contains("symlink"));

    // Nothing is reported for the external file, and the ancestor is not re-reported as a
    // second `sub` entry of another kind.
    let out = sandbox.run(&["read", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "{}", sandbox.stdout(&out));
}

#[test]
fn submodules_are_opaque_entries() {
    let sandbox = marker_repo();
    let sub = sandbox.root.join("sub-origin");
    fs::create_dir_all(&sub).unwrap();
    sandbox.git_in(&sub, &["init", "-q"]);
    sandbox.git_in(&sub, &["config", "user.email", "test@example.com"]);
    sandbox.git_in(&sub, &["config", "user.name", "Treenotes Test"]);
    fs::write(sub.join("inner.txt"), "submodule content\n").unwrap();
    sandbox.git_in(&sub, &["add", "-A"]);
    sandbox.git_in(&sub, &["commit", "-q", "-m", "sub"]);
    let sha = sandbox
        .git_in(&sub, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    fs::write(sub.join("inner.txt"), "submodule content v2\n").unwrap();
    sandbox.git_in(&sub, &["add", "-A"]);
    sandbox.git_in(&sub, &["commit", "-q", "-m", "sub v2"]);
    let sha_v2 = sandbox
        .git_in(&sub, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    assert_ne!(sha, sha_v2);

    sandbox.mkdir("vendor");
    let out = Command::new("git")
        .args(["clone", "-q"])
        .arg(&sub)
        .arg(sandbox.path("vendor/sub"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    sandbox.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{sha},vendor/sub"),
    ]);
    let url = sub.to_string_lossy().to_string();
    sandbox.write(
        ".gitmodules",
        &format!("[submodule \"vendor/sub\"]\n\tpath = vendor/sub\n\turl = {url}\n"),
    );
    sandbox.commit("add submodule");

    let json = sandbox.json(&["read", "--json"]);
    let paths = sandbox.paths(&json);
    assert!(paths.contains(&"vendor/sub".to_string()), "{paths:?}");
    assert_eq!(sandbox.entry(&json, "vendor/sub")["kind"], "submodule");
    assert!(
        !paths.iter().any(|path| path.starts_with("vendor/sub/")),
        "submodule contents must not be inventoried: {paths:?}"
    );

    sandbox.annotate("vendor/sub", "vendored dependency pinned by commit");
    let json = sandbox.json(&["read", "vendor/sub", "--json"]);
    assert_eq!(sandbox.entry(&json, "vendor/sub")["status"], "fresh");

    // Moving the gitlink to another commit changes the hash and stales the note.
    let before = sandbox.hash_of("vendor/sub");
    sandbox.git(&[
        "update-index",
        "--cacheinfo",
        &format!("160000,{sha_v2},vendor/sub"),
    ]);
    // `git add -A` during the commit above refreshed the gitlink to the submodule's checked out
    // commit, so the annotated version is the newer one; pinning the older commit changes the hash
    // and stales the note without touching the submodule's files.
    sandbox.git(&[
        "update-index",
        "--cacheinfo",
        &format!("160000,{sha},vendor/sub"),
    ]);
    assert_ne!(sha, sha_v2);
    assert_ne!(sandbox.hash_of("vendor/sub"), before);
    let json = sandbox.json(&["read", "vendor/sub", "--json"]);
    let entry = sandbox.entry(&json, "vendor/sub");
    assert_eq!(entry["status"], "stale");
    assert_eq!(
        entry["previous"]["note"],
        "vendored dependency pinned by commit"
    );
    assert_eq!(entry["previous"]["hash"], before.as_str());
}

#[test]
fn database_inside_the_repository_is_excluded() {
    let sandbox = marker_repo();
    let db = sandbox.path("notes.sqlite3");
    fs::write(sandbox.path("notes.sqlite3-wal"), "sidecar\n").unwrap();
    fs::write(sandbox.path("notes.sqlite3-shm"), "sidecar\n").unwrap();

    let mut cmd = sandbox.cmd_in(&sandbox.repo.clone());
    cmd.arg("--db")
        .arg(&db)
        .arg("set")
        .arg("a.txt")
        .arg("--note")
        .arg("inside repo db");
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut cmd = sandbox.cmd_in(&sandbox.repo.clone());
    cmd.arg("--db").arg(&db).args(["read", "--json"]);
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    let paths = sandbox.paths(&json);
    for absent in ["notes.sqlite3", "notes.sqlite3-wal", "notes.sqlite3-shm"] {
        assert!(!paths.contains(&absent.to_string()), "{paths:?}");
    }
    assert_eq!(sandbox.entry(&json, "a.txt")["note"], "inside repo db");
}

#[test]
fn unsupported_schema_versions_fail_clearly() {
    let sandbox = marker_repo();
    let conn = sandbox.db_conn();
    conn.pragma_update(None, "user_version", 99).unwrap();
    drop(conn);

    let message = sandbox.fails(&["read", "--json"], 2);
    assert!(message.contains("schema version 99"), "{message}");

    // A database that is not a treenotes database at all is left untouched.
    let sandbox = marker_repo();
    sandbox
        .db_conn()
        .execute_batch("CREATE TABLE other (id INTEGER);")
        .unwrap();
    let message = sandbox.fails(&["read", "--json"], 2);
    assert!(message.contains("no treenotes schema version"), "{message}");
}

#[test]
fn concurrent_fresh_processes_initialize_one_schema() {
    let sandbox = Sandbox::new();
    let names: Vec<String> = (0..8).map(|index| format!("f{index}.txt")).collect();
    for name in &names {
        sandbox.write(name, &format!("content of {name}\n"));
    }
    sandbox.commit("init");
    assert!(!sandbox.db.exists(), "database must start absent");

    // Every process opens the same, not-yet-created database and must install the schema once.
    let mut children = Vec::new();
    for name in &names {
        let mut cmd = sandbox.cmd_in(&sandbox.repo.clone());
        cmd.arg("--db")
            .arg(&sandbox.db)
            .args(["set", name, "--note", &format!("note for {name}")])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        children.push((name.clone(), cmd.spawn().expect("treenotes starts")));
    }
    for (name, child) in children {
        let out = child.wait_with_output().expect("treenotes finishes");
        assert!(
            out.status.success(),
            "concurrent set {name} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let conn = sandbox.db_conn();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3, "schema version after concurrent initialization");
    let count: i64 = conn
        .query_row("SELECT count(*) FROM notes", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, names.len() as i64, "every concurrent write survived");
    drop(conn);

    let json = sandbox.json(&["read", "--json"]);
    for name in &names {
        assert_eq!(
            sandbox.entry(&json, name)["note"],
            format!("note for {name}"),
            "{name}"
        );
    }
}

#[test]
fn default_database_location_can_be_redirected_with_global_flags() {
    let sandbox = marker_repo();
    let mut cmd = sandbox.cmd_in(&sandbox.repo.clone());
    cmd.args(["set", "a.txt", "--note", "default db note"]);
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let default_db = sandbox.home.join(".tree-notes").join("notes.sqlite3");
    assert!(default_db.is_file(), "default database was not created");

    let mut cmd = sandbox.cmd_in(&sandbox.repo.clone());
    cmd.args(["read", "a.txt", "--json"]);
    let out = cmd.output().unwrap();
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(sandbox.entry(&json, "a.txt")["note"], "default db note");

    // --repo works from outside the worktree, --db from anywhere.
    let elsewhere = sandbox.root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let mut cmd = sandbox.cmd_in(&elsewhere);
    cmd.arg("--repo")
        .arg(&sandbox.repo)
        .arg("--db")
        .arg(&sandbox.db)
        .args(["read", "a.txt", "--json"]);
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(sandbox.entry(&json, "a.txt")["status"], "missing");

    // Diagnostics never pollute stdout: JSON stays parseable on failure too.
    let out = sandbox.run(&["read", "missing.txt", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());
}

// -------------------------------------------------------------------------------------------
// AST members
// -------------------------------------------------------------------------------------------

/// Read one checked-in fixture exactly as the repository stores it.
fn fixture(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

impl Sandbox {
    fn members(&self, json: &Value) -> Vec<Value> {
        json["members"].as_array().expect("members array").clone()
    }

    fn member(&self, json: &Value, symbol: &str) -> Value {
        self.members(json)
            .into_iter()
            .find(|member| member["symbol"] == symbol)
            .unwrap_or_else(|| panic!("no member {symbol} in {json}"))
    }
}

#[test]
fn ast_members_are_listed_for_source_files_only() {
    let sandbox = Sandbox::new();
    sandbox.write("Cache.java", &fixture("tests/fixtures/java/Cache.java"));
    sandbox.write("store.rs", &fixture("tests/fixtures/rust/store.rs"));
    sandbox.write("lib/inner.py", &fixture("tests/fixtures/python/service.py"));
    sandbox.write("notes.txt", "plain text is not source\n");
    sandbox.commit("init");

    let json = sandbox.json(&["read", "--members", "--json"]);
    assert_eq!(json["version"], 3);
    assert_eq!(json["command"], "read");
    assert_eq!(json["parse_error"], false);
    assert!(!sandbox.members(&json).is_empty());
    assert!(
        sandbox
            .members(&json)
            .iter()
            .all(|member| member["status"] == "missing"),
        "members start out unannotated"
    );
    for expected in [
        "class:Cache:0",
        "field:Cache.size:0",
        "method:Cache.size:0",
        "method:Cache.size:1",
        "constructor:Cache.Cache:0",
        "method:Cache.Inner.go:0",
        "constant:Mode.FAST:0",
        "record:Point:0",
        "function:Cache::new:0",
        "impl:Cache:0",
        "function:inner::deep:0",
        "function:plain.nested:0",
        "function:Service.run:0",
    ] {
        let member = sandbox.member(&json, expected);
        assert!(
            !member["symbol_kind"].as_str().unwrap().is_empty(),
            "{expected}"
        );
        assert!(member["start_line"].as_u64().unwrap() >= 1, "{expected}");
        assert!(member["end_line"].as_u64().unwrap() >= member["start_line"].as_u64().unwrap());
        assert!(
            !member["qualified_name"].as_str().unwrap().is_empty(),
            "{expected}"
        );
    }
    // Overloaded declarations are distinct members with distinct hashes.
    let first = sandbox.member(&json, "method:Cache.size:0")["hash"]
        .as_str()
        .unwrap()
        .to_string();
    let second = sandbox.member(&json, "method:Cache.size:1")["hash"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(first, second);
    assert!(first.starts_with("tnt2:member:"), "{first}");
    // Non-source files never contribute members.
    assert!(sandbox
        .members(&json)
        .iter()
        .all(|member| member["path"] != "notes.txt"));

    // Scoped member listing stays inside the scope.
    let json = sandbox.json(&["read", "lib/inner.py", "--members", "--json"]);
    let paths: BTreeSet<String> = sandbox
        .members(&json)
        .iter()
        .map(|member| member["path"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(paths, BTreeSet::from(["lib/inner.py".to_string()]));
    assert_eq!(
        sandbox.member(&json, "function:plain.nested:0")["qualified_name"],
        "plain.nested"
    );
    // A local constant inside a function body is not an addressable member.
    assert!(sandbox
        .members(&json)
        .iter()
        .all(|member| member["symbol"] != "constant:outer.LOCAL_LIMIT:0"));

    // Text output lists members indented below their file, with the current hash.
    let text = sandbox.ok(&["read", "Cache.java", "--members"]);
    assert!(text.contains("size method:0 [method]"), "{text}");
    assert!(text.contains("missing"), "{text}");

    // Without `--members` nothing about the envelope changes except the version.
    let json = sandbox.json(&["read", "--json"]);
    assert!(json["members"].as_array().unwrap().is_empty());
    assert!(json["parse_error"].is_null());
}

#[test]
fn member_notes_are_per_declaration_and_guarded() {
    let sandbox = Sandbox::new();
    let source = fixture("tests/fixtures/java/Cache.java");
    sandbox.write("Cache.java", &source);
    sandbox.commit("init");

    let json = sandbox.json(&["read", "Cache.java", "--members", "--json"]);
    let first = sandbox.member(&json, "method:Cache.size:0")["hash"]
        .as_str()
        .unwrap()
        .to_string();
    let second = sandbox.member(&json, "method:Cache.size:1")["hash"]
        .as_str()
        .unwrap()
        .to_string();

    let out = sandbox.ok(&[
        "member-set",
        "Cache.java",
        "method:Cache.size:0",
        "--note",
        "cached size",
    ]);
    assert!(out.contains("method:Cache.size:0"), "{out}");
    sandbox.ok(&[
        "member-set",
        "Cache.java",
        "method:Cache.size:1",
        "--note",
        "size with fallback",
        "--expected-hash",
        &second,
    ]);

    let json = sandbox.json(&["read", "Cache.java", "--members", "--json"]);
    assert_eq!(
        sandbox.member(&json, "method:Cache.size:0")["status"],
        "fresh"
    );
    assert_eq!(
        sandbox.member(&json, "method:Cache.size:0")["note"],
        "cached size"
    );
    assert_eq!(
        sandbox.member(&json, "method:Cache.size:1")["status"],
        "fresh"
    );
    // Member notes are not file notes.
    let file = sandbox.json(&["read", "Cache.java", "--json"]);
    assert_eq!(sandbox.entry(&file, "Cache.java")["status"], "missing");

    // Note text is validated exactly like a file note.
    sandbox.fails(
        &[
            "member-set",
            "Cache.java",
            "method:Cache.size:0",
            "--note",
            "two\nlines",
        ],
        1,
    );
    let blank = sandbox.run_stdin(&["member-set", "Cache.java", "method:Cache.size:0"], "  \n");
    assert_eq!(blank.status.code(), Some(1));

    // Unknown symbol and unsupported file are invalid input.
    let message = sandbox.fails(
        &[
            "member-set",
            "Cache.java",
            "method:Cache.missing:0",
            "--note",
            "x",
        ],
        1,
    );
    assert!(message.contains("has no member"), "{message}");
    sandbox.write("notes.txt", "plain\n");
    sandbox.commit("txt");
    sandbox.fails(&["member-set", "notes.txt", "whatever", "--note", "x"], 1);
    assert!(sandbox
        .fails(
            &[
                "member-set",
                "Cache.java",
                "method:Cache.size:0",
                "--note",
                "x",
                "--expected-hash",
                "tnt2:member:deadbeef",
            ],
            1,
        )
        .contains("does not match"));

    // Editing one declaration re-stales only that declaration.
    sandbox.write(
        "Cache.java",
        &source.replace("return size;", "return size + 1;"),
    );
    sandbox.commit("edit size()");
    let json = sandbox.json(&["read", "Cache.java", "--members", "--json"]);
    let edited = sandbox.member(&json, "method:Cache.size:0");
    assert_eq!(edited["status"], "stale");
    assert_eq!(edited["note"], Value::Null);
    assert_eq!(edited["previous"]["note"], "cached size");
    assert_ne!(edited["hash"], first.as_str());
    assert_eq!(
        sandbox.member(&json, "method:Cache.size:1")["status"],
        "fresh"
    );
    assert_eq!(sandbox.member(&json, "class:Cache:0")["status"], "missing");
}

#[test]
fn set_ast_renotes_only_stale_members_that_already_have_a_note() {
    let sandbox = Sandbox::new();
    let source = fixture("tests/fixtures/java/Cache.java");
    sandbox.write("Cache.java", &source);
    sandbox.commit("init");
    sandbox.annotate("Cache.java", "java cache fixture");
    sandbox.ok(&[
        "member-set",
        "Cache.java",
        "method:Cache.size:0",
        "--note",
        "cached size",
    ]);

    sandbox.write(
        "Cache.java",
        &source.replace("return size;", "return size + 1;"),
    );
    sandbox.commit("edit size()");
    let json = sandbox.json(&["read", "Cache.java", "--members", "--json"]);
    assert_eq!(
        sandbox.member(&json, "method:Cache.size:0")["status"],
        "stale"
    );

    let json = sandbox.json(&[
        "set",
        "Cache.java",
        "--note",
        "java cache fixture, revisited",
        "--ast",
        "--json",
    ]);
    let written = sandbox.members(&json);
    assert_eq!(written.len(), 1, "{json}");
    assert_eq!(written[0]["symbol"], "method:Cache.size:0");
    assert_eq!(written[0]["note"], "cached size");
    assert_eq!(written[0]["status"], "fresh");

    let json = sandbox.json(&["read", "Cache.java", "--members", "--json"]);
    assert_eq!(
        sandbox.member(&json, "method:Cache.size:0")["status"],
        "fresh"
    );
    assert_eq!(
        sandbox.member(&json, "method:Cache.size:0")["note"],
        "cached size"
    );
    // A member that was never annotated is never invented by `--ast`.
    assert_eq!(
        sandbox.member(&json, "method:Cache.size:1")["status"],
        "missing"
    );

    // `--ast` needs exactly one file to re-note.
    sandbox.fails(&["set", ".", "--note", "root", "--ast"], 1);
}

#[test]
fn parse_errors_are_surfaced_without_hiding_recovered_members() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "Broken.java",
        "package demo;\n\nclass Broken {\n    void ok() {}\n    void broken( { }\n}\n",
    );
    sandbox.commit("init");

    let json = sandbox.json(&["read", "--members", "--json"]);
    assert_eq!(json["parse_error"], true);
    assert!(
        !sandbox.members(&json).is_empty(),
        "recovered members are still listed"
    );
}

#[test]
fn version_one_databases_migrate_additively() {
    let sandbox = marker_repo();
    sandbox.annotate("a.txt", "keep me across the migration");

    // Rewind the database to the version-1 shape: file notes only, no member table.
    let conn = sandbox.db_conn();
    conn.execute_batch("DROP TABLE member_notes; PRAGMA user_version = 1;")
        .unwrap();
    drop(conn);

    let json = sandbox.json(&["read", "a.txt", "--json"]);
    assert_eq!(json["version"], 3);
    assert_eq!(
        sandbox.entry(&json, "a.txt")["note"],
        "keep me across the migration"
    );

    let conn = sandbox.db_conn();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3);
    for table in [
        "member_notes",
        "snapshots",
        "snapshot_entries",
        "member_index_files",
        "member_index",
    ] {
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1, "{table} is created by the migration");
    }
    drop(conn);

    // The migrated database accepts member notes straight away.
    sandbox.write("Cache.java", &fixture("tests/fixtures/java/Cache.java"));
    sandbox.commit("java");
    sandbox.ok(&[
        "member-set",
        "Cache.java",
        "class:Cache:0",
        "--note",
        "cache fixture class",
    ]);
    let json = sandbox.json(&["read", "Cache.java", "--members", "--json"]);
    assert_eq!(sandbox.member(&json, "class:Cache:0")["status"], "fresh");
}

#[test]
fn schema_two_databases_gain_only_the_derived_tables() {
    let sandbox = marker_repo();
    sandbox.annotate("a.txt", "keep me across the migration");

    // Rewind the database to the version-2 shape: the note tables only, no derived state.
    let conn = sandbox.db_conn();
    conn.execute_batch(
        "DROP TABLE member_index; DROP TABLE member_index_files; DROP TABLE snapshot_entries; \
         DROP TABLE snapshots; PRAGMA user_version = 2;",
    )
    .unwrap();
    drop(conn);

    let json = sandbox.json(&["read", "a.txt", "--json"]);
    assert_eq!(json["version"], 3);
    assert_eq!(
        sandbox.entry(&json, "a.txt")["note"],
        "keep me across the migration"
    );

    // The derived state tables exist again and the state command works on the migrated database.
    let state = sandbox.json(&["state", "--json"]);
    assert!(state["state"]["state_hash"].as_str().is_some(), "{state}");
}

#[test]
fn state_hashes_the_tree_and_names_only_what_changed() {
    let sandbox = Sandbox::new();
    let java = fixture("tests/fixtures/java/Cache.java");
    sandbox.write("a.txt", "alpha\n");
    sandbox.write("d/f.txt", "f\n");
    sandbox.write("Cache.java", &java);
    sandbox.commit("init");

    let first = sandbox.json(&["state", "--json"]);
    let state_hash = first["state"]["state_hash"].as_str().unwrap().to_string();
    assert!(state_hash.starts_with("tnt1:state:"), "{first}");
    assert_eq!(first["version"], 3);
    assert_eq!(first["command"], "state");
    assert_eq!(first["state"]["known"], false);
    assert_eq!(first["state"]["recorded"], true);
    assert!(first["state"]["commit"].as_str().is_some(), "{first}");
    assert!(first["state"]["compared_state"].is_null());
    assert_eq!(first["state"]["changes"].as_array().unwrap().len(), 0);
    // Only the Java fixture is source; nothing has parsed it yet.
    assert_eq!(first["state"]["member_cache_hits"], 0);
    assert_eq!(first["state"]["member_cache_misses"], 1);

    // Re-observing the identical tree is a cache hit: nothing is re-recorded.
    let again = sandbox.json(&["state", "--json"]);
    assert_eq!(again["state"]["state_hash"], first["state"]["state_hash"]);
    assert_eq!(again["state"]["known"], true);
    assert_eq!(again["state"]["recorded"], false);

    // Parsing a file once is enough for every later state report.
    sandbox.json(&["read", "Cache.java", "--members", "--json"]);
    let cached = sandbox.json(&["state", "--json"]);
    assert_eq!(cached["state"]["member_cache_hits"], 1);
    assert_eq!(cached["state"]["member_cache_misses"], 0);

    // The state hash names content, not history: a commit that changes no bytes leaves it alone.
    sandbox.git(&["commit", "-q", "--allow-empty", "-m", "empty"]);
    assert_eq!(
        sandbox.json(&["state", "--json"])["state"]["state_hash"],
        state_hash
    );

    // One edited file and one rename, diffed against the first recorded state.
    sandbox.write("a.txt", "alpha edited\n");
    sandbox.write("d/g.txt", "f\n");
    sandbox.remove("d/f.txt");
    sandbox.commit("edit and rename");
    let changed = sandbox.json(&["state", "--json", "--compare", &state_hash]);
    let changes = changed["state"]["changes"].as_array().unwrap().clone();
    let described: Vec<(String, String)> = changes
        .iter()
        .map(|change| {
            (
                change["path"].as_str().unwrap().to_string(),
                change["change"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        described,
        vec![
            ("a.txt".to_string(), "modified".to_string()),
            ("d/f.txt".to_string(), "removed".to_string()),
            ("d/g.txt".to_string(), "added".to_string()),
        ],
        "{changed}"
    );
    // The untouched source file is absent: only real changes are named.
    assert!(
        changes.iter().all(|change| change["path"] != "Cache.java"),
        "{changed}"
    );
    assert_eq!(changed["state"]["known"], false);

    let text = sandbox.ok(&["state"]);
    assert!(text.starts_with("state "), "{text}");
    assert!(
        text.contains("member cache 1/1 source file(s) parsed"),
        "{text}"
    );
    assert!(text.contains("a.txt [file] modified"), "{text}");

    // Reverting the working tree reproduces the exact original state hash — the hash is a pure
    // function of the paths, kinds and content, so a settled state can be recognised later.
    sandbox.write("a.txt", "alpha\n");
    sandbox.write("d/f.txt", "f\n");
    sandbox.remove("d/g.txt");
    sandbox.commit("revert");
    let reverted = sandbox.json(&["state", "--json"]);
    assert_eq!(reverted["state"]["state_hash"], state_hash);
    assert_eq!(reverted["state"]["known"], true);
    assert_eq!(reverted["state"]["recorded"], false);

    // Comparing against a state this database never recorded is invalid input.
    let message = sandbox.fails(&["state", "--compare", "tnt1:state:00"], 1);
    assert!(message.contains("has never been recorded"), "{message}");
}

#[test]
fn pending_members_lists_only_missing_and_stale_declarations() {
    let sandbox = Sandbox::new();
    let source = fixture("tests/fixtures/java/Cache.java");
    sandbox.write("Cache.java", &source);
    sandbox.write("notes.txt", "plain text is not source\n");
    sandbox.commit("init");

    // Without the flag nothing about `pending` changes: no members, no parse_error.
    let plain = sandbox.json(&["pending", "--json"]);
    assert!(plain["members"].as_array().unwrap().is_empty());
    assert!(plain["parse_error"].is_null());

    // With the flag the tree listing is identical, and every unannotated declaration appears.
    let json = sandbox.json(&["pending", "--members", "--json"]);
    assert_eq!(json["version"], 3);
    assert_eq!(json["command"], "pending");
    assert_eq!(json["parse_error"], false);
    assert_eq!(sandbox.paths(&json), sandbox.paths(&plain));
    assert!(sandbox
        .members(&json)
        .iter()
        .all(|member| member["status"] == "missing"));
    assert!(sandbox
        .members(&json)
        .iter()
        .any(|member| member["symbol"] == "method:Cache.size:0"));

    // One annotated declaration leaves the list without touching its siblings, and annotating the
    // file itself does not annotate its declarations.
    sandbox.ok(&[
        "member-set",
        "Cache.java",
        "method:Cache.size:0",
        "--note",
        "cached size",
    ]);
    sandbox.annotate("Cache.java", "cache with two size overloads");
    let json = sandbox.json(&["pending", "--members", "--json"]);
    // The file note and one declaration are fresh; the other file and its declarations are not.
    assert!(!sandbox.paths(&json).contains(&"Cache.java".to_string()));
    assert!(sandbox.paths(&json).contains(&"notes.txt".to_string()));
    assert!(!sandbox
        .members(&json)
        .iter()
        .any(|member| member["symbol"] == "method:Cache.size:0"));
    assert!(sandbox
        .members(&json)
        .iter()
        .any(|member| member["symbol"] == "method:Cache.size:1"));

    // Editing one declaration re-stales exactly that one and puts it back in the list.
    sandbox.write(
        "Cache.java",
        &source.replace("return size;", "return this.size;"),
    );
    sandbox.commit("edit one method");
    let json = sandbox.json(&["pending", "--members", "--json"]);
    let stale = sandbox.member(&json, "method:Cache.size:0");
    assert_eq!(stale["status"], "stale");
    assert_eq!(stale["previous"]["note"], "cached size");
    // The sibling was never annotated at all, so it stays `missing` — untouched by the edit.
    assert_eq!(
        sandbox.member(&json, "method:Cache.size:1")["status"],
        "missing"
    );

    // Text output names each pending declaration; nothing fresh is printed.
    let text = sandbox.ok(&["pending", "--members"]);
    assert!(text.contains("size method:0 [method]"), "{text}");
    assert!(!text.contains("fresh"), "{text}");
}

#[test]
fn text_trees_group_pending_declarations_under_their_files() {
    let sandbox = Sandbox::new();
    sandbox.write("Cache.java", &fixture("tests/fixtures/java/Cache.java"));
    sandbox.write(
        "lib/service.py",
        &fixture("tests/fixtures/python/service.py"),
    );
    sandbox.write("lib/deep/tool.py", "def tool():\n    return 1\n");
    sandbox.commit("init");
    // One directory and one leaf file are annotated; two files own unannotated declarations.
    sandbox.annotate("lib", "library sources");
    sandbox.annotate("lib/deep/tool.py", "tiny helper");

    let text = sandbox.ok(&["pending", "--members"]);
    let lines = text_lines(&text);
    let position = |needle: &str| {
        lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no line for {needle}: {lines:?}"))
    };

    // The scope is the root, and every pending entry hangs from it with a branch connector.
    assert!(lines[0].starts_with(". [dir]"), "{lines:?}");
    assert!(!lines[0].starts_with(['│', '├', '└']), "{lines:?}");
    assert!(
        lines[1..].iter().all(|line| line.contains("── ")),
        "{lines:?}"
    );

    // Declarations are drawn under the file they were parsed from: the Java file before its own
    // methods, and both before the Python file that comes later in path order.
    // Depth is the display column the branch connector lands in, over four-column levels.
    let depth = |needle: &str| {
        let line = &lines[position(needle)];
        let column = line
            .find('─')
            .unwrap_or_else(|| panic!("no branch connector for {needle}"));
        line[..column].chars().count() / 4 + 1
    };
    assert_eq!(depth("Cache.java [file]"), 1, "{lines:?}");
    assert_eq!(depth("size method:0"), 2, "{lines:?}");
    assert_eq!(depth("lib/service.py [file]"), 1, "{lines:?}");
    assert_eq!(depth("run function:0"), 2, "{lines:?}");
    assert!(
        position("size method:0") < position("lib/service.py [file]"),
        "{lines:?}"
    );

    // A fresh file that owns a pending declaration is drawn as context only: no hash, no status.
    let context = position("(lib/deep/tool.py) [context]");
    let line = &lines[context];
    assert!(!line.contains("missing"), "{lines:?}");
    assert!(!line.contains("fresh"), "{lines:?}");
    assert!(context < position("tool function:0"), "{lines:?}");
    assert_eq!(depth("lib/deep [dir]"), 1, "{lines:?}");
    assert_eq!(depth("tool function:0"), 3, "{lines:?}");
    // Neither the annotated directory nor the fresh file is ever listed as pending work.
    let pending = sandbox.paths(&sandbox.json(&["pending", "--json"]));
    assert!(
        !pending.contains(&"lib".to_string()) && !pending.contains(&"lib/deep/tool.py".to_string()),
        "{pending:?}"
    );

    // The text tree is a rendering only: the JSON view and its filtering are unchanged.
    let json = sandbox.json(&["pending", "--members", "--json"]);
    assert_eq!(sandbox.entry(&json, "Cache.java")["status"], "missing");
    assert!(
        !sandbox
            .paths(&json)
            .contains(&"lib/deep/tool.py".to_string()),
        "a fresh file is never listed as pending"
    );
    assert_eq!(
        sandbox.member(&json, "function:tool:0")["path"],
        "lib/deep/tool.py"
    );
}

#[test]
fn text_tree_keeps_a_directory_and_its_children_above_a_sibling_file_named_like_it() {
    let sandbox = Sandbox::new();
    sandbox.write("src/cli/marketplaces.rs", "export function a() {}\n");
    sandbox.write("src/cli/marketplaces/top.ts", "export function b() {}\n");
    sandbox.write("src/cli/other.rs", "export function c() {}\n");
    sandbox.commit("layout");

    // `src/cli/marketplaces.rs` shares a prefix with the directory `src/cli/marketplaces` but is
    // not below it. Drawing the sibling first would leave the directory's own child hanging from
    // an ancestor the renderer had already closed.
    let read = text_lines(&sandbox.ok(&["read"]));
    let at = |lines: &[String], needle: &str| {
        lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle} missing from {lines:?}"))
    };
    let indent = |line: &str| line.find("── ").expect("every non-root line is drawn");
    let dir = at(&read, "src/cli/marketplaces [dir]");
    let child = at(&read, "src/cli/marketplaces/top.ts [file]");
    let sibling = at(&read, "src/cli/marketplaces.rs [file]");
    assert!(dir < child && child < sibling, "{read:?}");
    assert!(
        indent(&read[child]) > indent(&read[dir]),
        "the file hangs one level below its directory: {read:?}"
    );
    assert_eq!(
        indent(&read[sibling]),
        indent(&read[dir]),
        "the like-named file is a sibling of the directory: {read:?}"
    );

    // `pending` feeds the same renderer a children-first listing, and draws the same tree.
    let pending = text_lines(&sandbox.ok(&["pending"]));
    assert!(
        at(&pending, "src/cli/marketplaces [dir]")
            < at(&pending, "src/cli/marketplaces/top.ts [file]"),
        "{pending:?}"
    );
    assert!(
        at(&pending, "src/cli/marketplaces/top.ts [file]")
            < at(&pending, "src/cli/marketplaces.rs [file]"),
        "{pending:?}"
    );
}
