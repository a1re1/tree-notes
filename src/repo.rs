//! Git repository discovery, Git-visible tree inventory, and content hashing.
//!
//! Hash scheme (`tnt1`):
//!
//! * file     `tnt1:file:<hex>` = BLAKE3(`treenotes-hash-v1|file\0` || working-tree file bytes)
//! * symlink  `tnt1:symlink:<hex>` = BLAKE3(`treenotes-hash-v1|symlink\0` || link target bytes)
//! * submodule `tnt1:submodule:<hex>` = BLAKE3(`treenotes-hash-v1|submodule\0` || index gitlink sha)
//! * dir      `tnt1:dir:<hex>` = BLAKE3(`treenotes-hash-v1|dir\0` || children), where each child
//!   contributes `u32-le(name length) || name || kind tag || u32-le(hash length) || hash`, and
//!   children are sorted by name then kind tag.
//! * state    `tnt1:state:<hex>` = BLAKE3(`treenotes-hash-v1|state\0` || entries), where each
//!   entry contributes `u32-le(path length) || path || kind tag || u32-le(hash length) || hash`,
//!   in path order. It is the one hash that names a whole repository *state*: two states with
//!   equal state hashes have identical paths, kinds and content hashes, so anything derived from
//!   them (member lists, notes) is identical too.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::CmdError;

/// Version/domain tag that prefixes every hash string produced by this crate.
pub const HASH_SCHEME: &str = "tnt1";

const PREFIX_FILE: &[u8] = b"treenotes-hash-v1|file\0";
const PREFIX_DIR: &[u8] = b"treenotes-hash-v1|dir\0";
const PREFIX_SYMLINK: &[u8] = b"treenotes-hash-v1|symlink\0";
const PREFIX_SUBMODULE: &[u8] = b"treenotes-hash-v1|submodule\0";
const PREFIX_REPO: &[u8] = b"treenotes-repo-v1\0";
const PREFIX_STATE: &[u8] = b"treenotes-hash-v1|state\0";

/// The kind of a tree entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// A regular file (or any non-symlink, non-directory working-tree file).
    File,
    /// A directory; `.` is the repository root directory.
    Dir,
    /// A symbolic link; the hash covers the link target, never the target's contents.
    Symlink,
    /// A Git submodule (gitlink); opaque, never inventoried recursively.
    Submodule,
}

impl Kind {
    /// Stable lowercase name used in output and in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Dir => "dir",
            Kind::Symlink => "symlink",
            Kind::Submodule => "submodule",
        }
    }

    /// Parse the stable lowercase name.
    pub fn from_name(name: &str) -> Option<Kind> {
        match name {
            "file" => Some(Kind::File),
            "dir" => Some(Kind::Dir),
            "symlink" => Some(Kind::Symlink),
            "submodule" => Some(Kind::Submodule),
            _ => None,
        }
    }

    /// Single byte tag used inside directory hashes.
    fn tag(self) -> u8 {
        match self {
            Kind::File => 1,
            Kind::Submodule => 2,
            Kind::Dir => 3,
            Kind::Symlink => 4,
        }
    }
}

/// One entry of the current Git-visible tree.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Repository-root relative path; `.` for the root directory.
    pub path: String,
    /// Entry kind.
    pub kind: Kind,
    /// Current content hash, see the module documentation.
    pub hash: String,
}

/// A discovered Git worktree and its repository identity.
#[derive(Clone, Debug)]
pub struct Repo {
    /// Canonical worktree root (the `git rev-parse --show-toplevel` directory).
    pub root: PathBuf,
    /// Canonical Git common directory, shared by every linked worktree.
    pub common_dir: PathBuf,
    /// Stable repository identity, `tnt1:repo:<hex>` of the canonical common directory.
    pub identity: String,
}

impl Repo {
    /// Discover the repository from `repo_arg` (or the current directory when `None`).
    pub fn discover(repo_arg: Option<&Path>) -> Result<Repo, CmdError> {
        let start = match repo_arg {
            Some(path) => path.to_path_buf(),
            None => std::env::current_dir()
                .map_err(|e| CmdError::from_context("cannot read the current directory", e))?,
        };
        if !start.is_dir() {
            return Err(CmdError::env(format!(
                "repository directory {} is not a directory",
                start.display()
            )));
        }
        let start = start.canonicalize().map_err(|e| {
            CmdError::from_context(&format!("cannot resolve {}", start.display()), e)
        })?;

        let top = run_git_capture(&start, &["rev-parse", "--show-toplevel"])?;
        let top = String::from_utf8(top)
            .map_err(|_| CmdError::env("git returned a non-UTF-8 worktree root"))?;
        let root = PathBuf::from(top.strip_suffix('\n').unwrap_or(&top))
            .canonicalize()
            .map_err(|e| CmdError::from_context("cannot resolve the worktree root", e))?;

        let common = run_git_capture(&start, &["rev-parse", "--git-common-dir"])?;
        let common = String::from_utf8(common)
            .map_err(|_| CmdError::env("git returned a non-UTF-8 git common directory"))?;
        let common = PathBuf::from(common.strip_suffix('\n').unwrap_or(&common));
        let common = if common.is_absolute() {
            common
        } else {
            start.join(common)
        };
        let common_dir = common
            .canonicalize()
            .map_err(|e| CmdError::from_context("cannot resolve the git common directory", e))?;

        let mut hasher = blake3::Hasher::new();
        hasher.update(PREFIX_REPO);
        hasher.update(common_dir.to_string_lossy().as_bytes());
        let identity = format!("{HASH_SCHEME}:repo:{}", hasher.finalize().to_hex());

        Ok(Repo {
            root,
            common_dir,
            identity,
        })
    }

    /// Absolute path of a repository-root relative path.
    pub fn absolute(&self, relative: &str) -> PathBuf {
        if relative == "." {
            self.root.clone()
        } else {
            self.root.join(relative)
        }
    }

    /// Commit `HEAD` points at in this worktree, or `None` while the repository has no commit
    /// yet (an unborn branch). The commit is advisory metadata for a recorded state: a state can
    /// be recorded, and matched again, entirely from content hashes without any commit at all.
    pub fn head_commit(&self) -> Result<Option<String>, CmdError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["rev-parse", "--verify", "--quiet", "HEAD"])
            .output()
            .map_err(|e| CmdError::from_context("failed to run git", e))?;
        if !output.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8(output.stdout)
            .map_err(|_| CmdError::env("git returned a non-UTF-8 commit"))?;
        let commit = text.trim();
        if commit.is_empty() {
            Ok(None)
        } else {
            Ok(Some(commit.to_string()))
        }
    }

    /// Root-relative form of an absolute path, when it lies inside this worktree.
    fn relative_of(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.root).ok()?;
        let text = rel.to_str()?;
        Some(if text.is_empty() {
            ".".to_string()
        } else {
            text.to_string()
        })
    }

    /// Inventory the Git-visible tree: tracked entries that still exist on disk plus
    /// untracked non-ignored files, with synthesized directories and the root `.`.
    ///
    /// `exclude` holds absolute paths (the database and its sidecars) that are skipped even
    /// when they live inside the worktree.
    pub fn inventory(&self, exclude: &[PathBuf]) -> Result<Vec<Entry>, CmdError> {
        let excluded: BTreeSet<String> = exclude
            .iter()
            .filter_map(|path| self.relative_of(path))
            .collect();

        let tracked = run_git_capture(
            &self.root,
            &["ls-files", "-z", "--cached", "--stage", "--full-name"],
        )?;
        let untracked = run_git_capture(
            &self.root,
            &[
                "ls-files",
                "-z",
                "--others",
                "--exclude-standard",
                "--full-name",
            ],
        )?;

        let mut index: BTreeMap<String, IndexEntry> = BTreeMap::new();
        for record in split_nul(&tracked)? {
            let (meta, path) = record.split_once('\t').ok_or_else(|| {
                CmdError::env(format!("unexpected git ls-files record: {record:?}"))
            })?;
            let mut fields = meta.split(' ');
            let mode = fields.next().unwrap_or("");
            let object = fields.next().unwrap_or("");
            let stage = fields.next().unwrap_or("0");
            let candidate = IndexEntry {
                gitlink: if mode == "160000" {
                    Some(object.to_string())
                } else {
                    None
                },
                stage: stage.to_string(),
            };
            match index.get(path) {
                Some(existing) if existing.stage == "0" && candidate.stage != "0" => {}
                _ => {
                    index.insert(path.to_string(), candidate);
                }
            }
        }
        for path in split_nul(&untracked)? {
            index.entry(path.to_string()).or_insert(IndexEntry {
                gitlink: None,
                stage: "0".to_string(),
            });
        }

        let mut leaves: BTreeMap<String, (Kind, String)> = BTreeMap::new();
        for (path, meta) in &index {
            if excluded.contains(path) {
                continue;
            }
            // Git's index may still name descendants of a directory replaced by a symlink.
            // Check every ancestor before even statting the leaf, which otherwise follows it.
            let mut ancestor = self.root.clone();
            for component in path.split('/').take(path.split('/').count() - 1) {
                ancestor.push(component);
                match fs::symlink_metadata(&ancestor) {
                    Ok(status) if status.file_type().is_symlink() => {
                        return Err(CmdError::env(format!(
                            "cannot scan {path}: ancestor {} is a symlink",
                            ancestor.display()
                        )));
                    }
                    Ok(_) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
                    Err(err) => {
                        return Err(CmdError::env(format!(
                            "cannot stat ancestor of {path}: {err}"
                        )));
                    }
                }
            }
            let absolute = self.absolute(path);
            let status = match fs::symlink_metadata(&absolute) {
                Ok(status) => status,
                // Deleted tracked files (and vanished untracked files) are not part of the tree.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    return Err(CmdError::env(format!("cannot stat {path}: {err}")));
                }
            };
            let entry = if status.file_type().is_symlink() {
                (Kind::Symlink, hash_symlink(&absolute, path)?)
            } else if status.is_dir() {
                match &meta.gitlink {
                    Some(sha) => (Kind::Submodule, hash_submodule(sha)),
                    None => {
                        return Err(CmdError::env(format!(
                            "{path} is a directory but is not a submodule; refusing to skip it"
                        )));
                    }
                }
            } else if status.is_file() {
                (Kind::File, hash_file(&absolute, path)?)
            } else {
                return Err(CmdError::env(format!(
                    "{path} has an unsupported file type (not a file, directory or symlink)"
                )));
            };
            leaves.insert(path.clone(), entry);
        }

        Ok(self.assemble(leaves))
    }

    /// Compute directory hashes bottom-up and return every entry, root first.
    fn assemble(&self, leaves: BTreeMap<String, (Kind, String)>) -> Vec<Entry> {
        let mut dirs: BTreeSet<String> = BTreeSet::new();
        dirs.insert(".".to_string());
        for path in leaves.keys() {
            let mut current = path.as_str();
            while let Some((parent, _)) = current.rsplit_once('/') {
                dirs.insert(parent.to_string());
                current = parent;
            }
        }

        enum Kid {
            Hash(String),
            Dir(String),
        }

        let mut children: BTreeMap<String, Vec<(String, Kind, Kid)>> = BTreeMap::new();
        for (path, (kind, hash)) in &leaves {
            let parent = parent_path(path);
            children.entry(parent.to_string()).or_default().push((
                last_component(path).to_string(),
                *kind,
                Kid::Hash(hash.clone()),
            ));
        }
        for dir in &dirs {
            if dir == "." {
                continue;
            }
            let parent = parent_path(dir);
            children.entry(parent.to_string()).or_default().push((
                last_component(dir).to_string(),
                Kind::Dir,
                Kid::Dir(dir.clone()),
            ));
        }

        let mut ordered: Vec<&String> = dirs.iter().collect();
        ordered.sort_by_key(|dir| std::cmp::Reverse(depth_of(dir)));

        let mut dir_hashes: BTreeMap<String, String> = BTreeMap::new();
        for dir in ordered {
            let mut kids = children.remove(dir).unwrap_or_default();
            kids.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            let mut hasher = blake3::Hasher::new();
            hasher.update(PREFIX_DIR);
            for (name, kind, kid) in kids {
                let hash = match kid {
                    Kid::Hash(hash) => hash,
                    Kid::Dir(path) => dir_hashes.get(&path).cloned().unwrap_or_default(),
                };
                hasher.update(&(name.len() as u32).to_le_bytes());
                hasher.update(name.as_bytes());
                hasher.update(&[kind.tag()]);
                hasher.update(&(hash.len() as u32).to_le_bytes());
                hasher.update(hash.as_bytes());
            }
            dir_hashes.insert(
                dir.clone(),
                format!("{HASH_SCHEME}:dir:{}", hasher.finalize().to_hex()),
            );
        }

        let mut entries: Vec<Entry> = Vec::with_capacity(leaves.len() + dirs.len());
        for (path, (kind, hash)) in leaves {
            entries.push(Entry { path, kind, hash });
        }
        for (path, hash) in dir_hashes {
            entries.push(Entry {
                path,
                kind: Kind::Dir,
                hash,
            });
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        if let Some(pos) = entries.iter().position(|entry| entry.path == ".") {
            let root = entries.remove(pos);
            entries.insert(0, root);
        }
        entries
    }
}

/// Index information for one Git-visible path.
struct IndexEntry {
    gitlink: Option<String>,
    stage: String,
}

/// Path of the parent directory, `"."` for top level entries.
pub fn parent_path(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some((parent, _)) => parent,
        None => ".",
    }
}

/// Final path component (the whole path when it has no separator).
pub fn last_component(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some((_, name)) => name,
        None => path,
    }
}

/// Depth of a repository-relative path: `"."` is 0, `"a"` is 1, `"a/b"` is 2.
pub fn depth_of(path: &str) -> usize {
    if path == "." {
        0
    } else {
        path.matches('/').count() + 1
    }
}

/// Depth of `path` relative to `scope` (0 when `path == scope`).
pub fn depth_within(scope: &str, path: &str) -> usize {
    if path == scope {
        0
    } else if scope == "." {
        depth_of(path)
    } else {
        depth_of(path) - depth_of(scope)
    }
}

/// Aggregate hash of a whole tree state: the path, kind and hash of every entry, in path order.
///
/// It is a pure function of the inventory, so two checkouts with the same paths and the same
/// file, directory, symlink and submodule hashes produce the same state hash — which is what
/// makes "have we already computed this state?" answerable without re-reading the tree.
pub fn state_hash(entries: &[Entry]) -> String {
    let mut ordered: Vec<&Entry> = entries.iter().collect();
    ordered.sort_by(|a, b| a.path.cmp(&b.path));
    let mut hasher = blake3::Hasher::new();
    hasher.update(PREFIX_STATE);
    for entry in ordered {
        hasher.update(&(entry.path.len() as u32).to_le_bytes());
        hasher.update(entry.path.as_bytes());
        hasher.update(&[entry.kind.tag()]);
        hasher.update(&(entry.hash.len() as u32).to_le_bytes());
        hasher.update(entry.hash.as_bytes());
    }
    format!("{HASH_SCHEME}:state:{}", hasher.finalize().to_hex())
}

/// True when `path` is the scope itself or a descendant of a directory scope.
pub fn in_scope(path: &str, scope: &str, scope_kind: Kind) -> bool {
    if scope == "." {
        true
    } else if scope_kind == Kind::Dir {
        path == scope || path.starts_with(&format!("{scope}/"))
    } else {
        path == scope
    }
}

/// Stream a working-tree file through BLAKE3; the contents are never retained.
pub fn hash_file(absolute: &Path, label: &str) -> Result<String, CmdError> {
    let file =
        File::open(absolute).map_err(|e| CmdError::env(format!("cannot read {label}: {e}")))?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut hasher = blake3::Hasher::new();
    hasher.update(PREFIX_FILE);
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|e| CmdError::env(format!("cannot read {label}: {e}")))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{HASH_SCHEME}:file:{}", hasher.finalize().to_hex()))
}

/// Hash a symlink by its target text; the target is never followed.
pub fn hash_symlink(absolute: &Path, label: &str) -> Result<String, CmdError> {
    let target = fs::read_link(absolute)
        .map_err(|e| CmdError::env(format!("cannot read symlink {label}: {e}")))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(PREFIX_SYMLINK);
    hasher.update(target.as_os_str().as_encoded_bytes());
    Ok(format!(
        "{HASH_SCHEME}:symlink:{}",
        hasher.finalize().to_hex()
    ))
}

/// Hash a submodule by the gitlink commit recorded in the index; contents are not read.
pub fn hash_submodule(gitlink_sha: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PREFIX_SUBMODULE);
    hasher.update(gitlink_sha.as_bytes());
    format!("{HASH_SCHEME}:submodule:{}", hasher.finalize().to_hex())
}

/// Split NUL-delimited `git` output, rejecting non-UTF-8 paths instead of losing them.
fn split_nul(bytes: &[u8]) -> Result<Vec<&str>, CmdError> {
    let mut out = Vec::new();
    for part in bytes.split(|byte| *byte == 0) {
        if part.is_empty() {
            continue;
        }
        let text = std::str::from_utf8(part).map_err(|_| {
            CmdError::env("the repository contains a path that is not valid UTF-8; treenotes refuses to drop it")
        })?;
        out.push(text);
    }
    Ok(out)
}

/// Run `git -C dir <args>` and return stdout, or a classified failure.
fn run_git_capture(dir: &Path, args: &[&str]) -> Result<Vec<u8>, CmdError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| CmdError::from_context("failed to run git", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let message = if stderr.contains("not a git repository") {
            format!("{} is not inside a Git repository", dir.display())
        } else if stderr.is_empty() {
            format!("git {} failed", args.join(" "))
        } else {
            stderr
        };
        return Err(CmdError::env(message));
    }
    Ok(output.stdout)
}
