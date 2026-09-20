//! SQLite note storage: versioned, content-addressed notes in a local database.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};

use crate::repo::Kind;
use crate::{CmdError, SCHEMA_VERSION};

const SCHEMA_SQL: &str = "
CREATE TABLE notes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    repository  TEXT NOT NULL,
    path        TEXT NOT NULL,
    kind        TEXT NOT NULL,
    hash        TEXT NOT NULL,
    note        TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (repository, path, kind, hash)
);
CREATE INDEX notes_repository_path ON notes (repository, path);
CREATE INDEX notes_repository_hash ON notes (repository, hash);
";

/// Member-note table, created on a fresh database and added by the version 1 -> 2 migration.
const MEMBER_SCHEMA_SQL: &str = "
CREATE TABLE member_notes (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    repository     TEXT NOT NULL,
    path           TEXT NOT NULL,
    symbol         TEXT NOT NULL,
    symbol_kind    TEXT NOT NULL,
    name           TEXT NOT NULL,
    qualified_name TEXT NOT NULL,
    start_line     INTEGER NOT NULL,
    end_line       INTEGER NOT NULL,
    hash           TEXT NOT NULL,
    note           TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    UNIQUE (repository, path, symbol, hash)
);
CREATE INDEX member_notes_repository_path ON member_notes (repository, path);
";

/// Derived state schema, created on a fresh database and added by the version 1/2 -> 3 migrations.
///
/// Nothing here is a note: `snapshots`/`snapshot_entries` remember which repository states have
/// already been computed (and at which commit), and `member_index_files`/`member_index` cache the
/// members parsed for one file hash so an unchanged file is never parsed twice. Dropping all four
/// tables loses no note and only costs a reparse.
///
/// Every statement is `IF NOT EXISTS`, so applying this to a database whose recorded version is
/// behind its actual tables (a rewound `user_version`, or a tool older than the tables it found)
/// adds only what is genuinely missing instead of failing the whole migration.
const STATE_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS snapshots (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    repository  TEXT NOT NULL,
    state_hash  TEXT NOT NULL,
    commit_sha  TEXT,
    updated_at  TEXT NOT NULL,
    UNIQUE (repository, state_hash)
);
CREATE INDEX IF NOT EXISTS snapshots_repository_id ON snapshots (repository, id);
CREATE TABLE IF NOT EXISTS snapshot_entries (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    snapshot_id INTEGER NOT NULL,
    path        TEXT NOT NULL,
    kind        TEXT NOT NULL,
    hash        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS snapshot_entries_snapshot ON snapshot_entries (snapshot_id, path);
CREATE TABLE IF NOT EXISTS member_index_files (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    repository  TEXT NOT NULL,
    path        TEXT NOT NULL,
    file_hash   TEXT NOT NULL,
    parse_error INTEGER NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (repository, path, file_hash)
);
CREATE TABLE IF NOT EXISTS member_index (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    repository     TEXT NOT NULL,
    path           TEXT NOT NULL,
    file_hash      TEXT NOT NULL,
    symbol         TEXT NOT NULL,
    symbol_kind    TEXT NOT NULL,
    name           TEXT NOT NULL,
    qualified_name TEXT NOT NULL,
    start_line     INTEGER NOT NULL,
    end_line       INTEGER NOT NULL,
    hash           TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS member_index_lookup ON member_index (repository, path, file_hash);
";

/// One stored member-note version.
#[derive(Clone, Debug)]
pub struct MemberVersion {
    /// treenotes member kind the note was written for.
    pub symbol_kind: String,
    /// Member hash the note is bound to.
    pub hash: String,
    /// One-line note text.
    pub note: String,
    /// RFC3339 UTC timestamp of the last write.
    pub updated_at: String,
}

/// A member-note version about to be written.
#[derive(Clone, Debug)]
pub struct NewMemberNote {
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
    /// 1-based first line of the declaration at write time (display metadata).
    pub start_line: usize,
    /// 1-based last line of the declaration at write time (display metadata).
    pub end_line: usize,
    /// Member hash the note belongs to.
    pub hash: String,
    /// One-line note text.
    pub note: String,
}

/// One stored note version.
#[derive(Clone, Debug)]
pub struct NoteVersion {
    /// Entry kind the note was written for.
    pub kind: Kind,
    /// Content hash the note is bound to.
    pub hash: String,
    /// One-line note text.
    pub note: String,
    /// RFC3339 UTC timestamp of the last write.
    pub updated_at: String,
}

/// A note version about to be written.
#[derive(Clone, Debug)]
pub struct NewNote {
    /// Repository-root relative path.
    pub path: String,
    /// Entry kind.
    pub kind: Kind,
    /// Content hash the note belongs to.
    pub hash: String,
    /// One-line note text.
    pub note: String,
}

/// Handle to the notes database.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating when needed) the database at `path`.
    pub fn open(path: &Path) -> Result<Store, CmdError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| {
                    CmdError::env(format!(
                        "cannot create the database directory {}: {e}",
                        parent.display()
                    ))
                })?;
            }
        }
        let conn = Connection::open(path)
            .map_err(|e| CmdError::env(format!("cannot open database {}: {e}", path.display())))?;
        conn.busy_timeout(Duration::from_secs(10))
            .map_err(|e| CmdError::env(format!("cannot set the database busy timeout: {e}")))?;
        // Switching a fresh database into WAL mode briefly needs an exclusive lock that
        // SQLite's busy handler does not cover, so several processes opening the same
        // not-yet-initialized database at once can collide here. Retry while the
        // competing process holds the lock; every other open failure is still fatal.
        let mut wal_error = None;
        for _ in 0..500 {
            match conn.query_row("PRAGMA journal_mode = WAL", [], |row| {
                row.get::<_, String>(0)
            }) {
                Ok(_mode) => {
                    wal_error = None;
                    break;
                }
                Err(error) => match &error {
                    rusqlite::Error::SqliteFailure(code, _)
                        if code.code == rusqlite::ErrorCode::DatabaseBusy
                            || code.code == rusqlite::ErrorCode::DatabaseLocked =>
                    {
                        wal_error = Some(error);
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    _ => {
                        return Err(CmdError::env(format!(
                            "cannot enable WAL journalling: {error}"
                        )))
                    }
                },
            }
        }
        if let Some(error) = wal_error {
            return Err(CmdError::env(format!(
                "cannot enable WAL journalling: {error}"
            )));
        }
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| CmdError::env(format!("cannot set synchronous mode: {e}")))?;
        let mut store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Validate or install the schema, refusing unsupported versions and unknown files.
    fn migrate(&mut self) -> Result<(), CmdError> {
        // Serialize first-open initialization and commit the schema and its version together.
        let transaction = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| CmdError::env(format!("cannot start schema transaction: {e}")))?;
        let version: i64 = transaction
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|e| CmdError::env(format!("cannot read the database schema version: {e}")))?;
        if version == SCHEMA_VERSION {
            return Ok(());
        }
        if version == 1 || version == 2 {
            // Additive migration: an older database keeps every note and gains exactly the tables
            // its version lacks. No existing row is rewritten.
            if version == 1 {
                transaction.execute_batch(MEMBER_SCHEMA_SQL).map_err(|e| {
                    CmdError::env(format!("cannot add the member-note schema: {e}"))
                })?;
            }
            transaction
                .execute_batch(STATE_SCHEMA_SQL)
                .map_err(|e| CmdError::env(format!("cannot add the state schema: {e}")))?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(|e| CmdError::env(format!("cannot set the schema version: {e}")))?;
            return transaction
                .commit()
                .map_err(|e| CmdError::env(format!("cannot commit the schema migration: {e}")));
        }
        if version != 0 {
            return Err(CmdError::env(format!(
                "database schema version {version} is not supported by this treenotes build (expected {SCHEMA_VERSION})"
            )));
        }
        let objects: i64 = transaction
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| CmdError::env(format!("cannot inspect the database: {e}")))?;
        if objects > 0 {
            return Err(CmdError::env(
                "this database has tables but no treenotes schema version; refusing to change it",
            ));
        }
        transaction
            .execute_batch(SCHEMA_SQL)
            .map_err(|e| CmdError::env(format!("cannot create the notes schema: {e}")))?;
        transaction
            .execute_batch(MEMBER_SCHEMA_SQL)
            .map_err(|e| CmdError::env(format!("cannot create the member-note schema: {e}")))?;
        transaction
            .execute_batch(STATE_SCHEMA_SQL)
            .map_err(|e| CmdError::env(format!("cannot create the state schema: {e}")))?;
        transaction
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(|e| CmdError::env(format!("cannot set the schema version: {e}")))?;
        transaction
            .commit()
            .map_err(|e| CmdError::env(format!("cannot commit the notes schema: {e}")))
    }

    /// All note versions of one repository, grouped by path, newest first per path.
    /// Upserts retain the newly allocated autoincrement ID so write order is clock-independent.
    pub fn load_versions(
        &self,
        repository: &str,
    ) -> Result<BTreeMap<String, Vec<NoteVersion>>, CmdError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT path, kind, hash, note, updated_at FROM notes WHERE repository = ?1 \
                 ORDER BY path ASC, id DESC",
            )
            .map_err(|e| CmdError::env(format!("cannot read notes: {e}")))?;
        let rows = statement
            .query_map([repository], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(|e| CmdError::env(format!("cannot read notes: {e}")))?;
        let mut grouped: BTreeMap<String, Vec<NoteVersion>> = BTreeMap::new();
        for row in rows {
            let (path, kind_name, hash, note, updated_at) =
                row.map_err(|e| CmdError::env(format!("cannot read notes: {e}")))?;
            let kind = Kind::from_name(&kind_name).ok_or_else(|| {
                CmdError::env(format!("note for {path} has unknown kind {kind_name:?}"))
            })?;
            grouped.entry(path).or_default().push(NoteVersion {
                kind,
                hash,
                note,
                updated_at,
            });
        }
        Ok(grouped)
    }

    /// All member-note versions of one repository, grouped by path then symbol, newest first.
    pub fn load_member_versions(
        &self,
        repository: &str,
    ) -> Result<BTreeMap<String, BTreeMap<String, Vec<MemberVersion>>>, CmdError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT path, symbol, symbol_kind, hash, note, updated_at FROM member_notes \
                 WHERE repository = ?1 ORDER BY path ASC, symbol ASC, id DESC",
            )
            .map_err(|e| CmdError::env(format!("cannot read member notes: {e}")))?;
        let rows = statement
            .query_map([repository], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(|e| CmdError::env(format!("cannot read member notes: {e}")))?;
        let mut grouped: BTreeMap<String, BTreeMap<String, Vec<MemberVersion>>> = BTreeMap::new();
        for row in rows {
            let (path, symbol, symbol_kind, hash, note, updated_at) =
                row.map_err(|e| CmdError::env(format!("cannot read member notes: {e}")))?;
            grouped
                .entry(path)
                .or_default()
                .entry(symbol)
                .or_default()
                .push(MemberVersion {
                    symbol_kind,
                    hash,
                    note,
                    updated_at,
                });
        }
        Ok(grouped)
    }

    /// Write one member-note version.
    pub fn write_member_note(
        &mut self,
        repository: &str,
        note: &NewMemberNote,
    ) -> Result<(), CmdError> {
        let updated_at = now_rfc3339();
        self.conn
            .execute(
                "INSERT INTO member_notes (repository, path, symbol, symbol_kind, name, \
                 qualified_name, start_line, end_line, hash, note, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
                 ON CONFLICT (repository, path, symbol, hash) DO UPDATE SET \
                 id = excluded.id, name = excluded.name, \
                 qualified_name = excluded.qualified_name, start_line = excluded.start_line, \
                 end_line = excluded.end_line, note = excluded.note, \
                 updated_at = excluded.updated_at",
                rusqlite::params![
                    repository,
                    note.path,
                    note.symbol,
                    note.symbol_kind,
                    note.name,
                    note.qualified_name,
                    note.start_line as i64,
                    note.end_line as i64,
                    note.hash,
                    note.note,
                    updated_at
                ],
            )
            .map_err(|e| {
                CmdError::env(format!(
                    "cannot store the member note for {} {}: {e}",
                    note.path, note.symbol
                ))
            })
            .map(|_| ())
    }

    /// Write one note version.
    pub fn write_note(&mut self, repository: &str, note: &NewNote) -> Result<(), CmdError> {
        let updated_at = now_rfc3339();
        self.conn
            .execute(
                "INSERT INTO notes (repository, path, kind, hash, note, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT (repository, path, kind, hash) \
                 DO UPDATE SET id = excluded.id, note = excluded.note, updated_at = excluded.updated_at",
                rusqlite::params![
                    repository,
                    note.path,
                    note.kind.as_str(),
                    note.hash,
                    note.note,
                    updated_at
                ],
            )
            .map_err(|e| CmdError::env(format!("cannot store the note for {}: {e}", note.path)))
            .map(|_| ())
    }

    /// Write many note versions in a single transaction: all of them or none of them.
    pub fn write_notes(&mut self, repository: &str, notes: &[NewNote]) -> Result<(), CmdError> {
        let transaction = self
            .conn
            .transaction()
            .map_err(|e| CmdError::env(format!("cannot start a database transaction: {e}")))?;
        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO notes (repository, path, kind, hash, note, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                     ON CONFLICT (repository, path, kind, hash) \
                     DO UPDATE SET id = excluded.id, note = excluded.note, updated_at = excluded.updated_at",
                )
                .map_err(|e| CmdError::env(format!("cannot prepare the note write: {e}")))?;
            for note in notes {
                let updated_at = now_rfc3339();
                statement
                    .execute(rusqlite::params![
                        repository,
                        note.path,
                        note.kind.as_str(),
                        note.hash,
                        note.note,
                        updated_at
                    ])
                    .map_err(|e| {
                        CmdError::env(format!("cannot store the note for {}: {e}", note.path))
                    })?;
            }
        }
        transaction
            .commit()
            .map_err(|e| CmdError::env(format!("cannot commit the note batch: {e}")))
    }
}

/// Default database location: `~/.tree-notes/notes.sqlite3`.
pub fn default_db_path() -> Result<PathBuf, CmdError> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        CmdError::env("cannot determine the home directory; pass --db <PATH> explicitly")
    })?;
    if home.is_empty() {
        return Err(CmdError::env(
            "cannot determine the home directory; pass --db <PATH> explicitly",
        ));
    }
    Ok(PathBuf::from(home)
        .join(".tree-notes")
        .join("notes.sqlite3"))
}

/// Current UTC time as an RFC3339 `...Z` timestamp.
pub fn now_rfc3339() -> String {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = elapsed.as_secs() as i64;
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:09}Z",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60,
        elapsed.subsec_nanos()
    )
}

/// Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}
/// One cached AST member of a file whose bytes are still exactly the ones it was parsed from.
#[derive(Clone, Debug)]
pub struct CachedMember {
    /// Symbol key, `<symbol-kind>:<qualified name>:<ordinal>`.
    pub symbol: String,
    /// treenotes member kind.
    pub symbol_kind: String,
    /// Declared name as written.
    pub name: String,
    /// Name qualified by its container chain.
    pub qualified_name: String,
    /// 1-based first line of the declaration in the parsed bytes.
    pub start_line: usize,
    /// 1-based last line of the declaration in the parsed bytes.
    pub end_line: usize,
    /// Member hash of the declaration.
    pub hash: String,
}

/// The `(path, kind, hash)` rows of one recorded state, in insertion order.
pub type SnapshotEntries = Vec<(String, String, String)>;

/// One recorded repository state.
#[derive(Clone, Debug)]
pub struct SnapshotInfo {
    /// `tnt1:state:<hex>` of the recorded state.
    pub state_hash: String,
    /// Commit observed when the state was recorded, when the repository had one.
    pub commit: Option<String>,
    /// RFC3339 UTC timestamp of the last recording of this state.
    pub updated_at: String,
}

/// Derived state and member-cache storage.
impl Store {
    /// Cached members of `path` at exactly `file_hash`, with that parse's error flag, or `None`
    /// when those bytes have never been parsed.
    ///
    /// An empty vector is a real cached answer ("this file has no members"), which is why the
    /// marker row in `member_index_files` exists: `None` and `Some(vec![])` mean different things.
    pub fn load_member_index(
        &self,
        repository: &str,
        path: &str,
        file_hash: &str,
    ) -> Result<Option<(bool, Vec<CachedMember>)>, CmdError> {
        let marker: Option<i64> = self
            .conn
            .query_row(
                "SELECT parse_error FROM member_index_files WHERE repository = ?1 AND path = ?2 \
                 AND file_hash = ?3",
                rusqlite::params![repository, path, file_hash],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| CmdError::env(format!("cannot read the member cache: {e}")))?;
        let Some(parse_error) = marker else {
            return Ok(None);
        };
        let mut statement = self
            .conn
            .prepare(
                "SELECT symbol, symbol_kind, name, qualified_name, start_line, end_line, hash \
                 FROM member_index WHERE repository = ?1 AND path = ?2 AND file_hash = ?3 \
                 ORDER BY start_line ASC, symbol ASC",
            )
            .map_err(|e| CmdError::env(format!("cannot read the member cache: {e}")))?;
        let rows = statement
            .query_map(rusqlite::params![repository, path, file_hash], |row| {
                Ok(CachedMember {
                    symbol: row.get(0)?,
                    symbol_kind: row.get(1)?,
                    name: row.get(2)?,
                    qualified_name: row.get(3)?,
                    start_line: row.get::<_, i64>(4)? as usize,
                    end_line: row.get::<_, i64>(5)? as usize,
                    hash: row.get(6)?,
                })
            })
            .map_err(|e| CmdError::env(format!("cannot read the member cache: {e}")))?;
        let mut members = Vec::new();
        for row in rows {
            members.push(
                row.map_err(|e| CmdError::env(format!("cannot read the member cache: {e}")))?,
            );
        }
        Ok(Some((parse_error != 0, members)))
    }

    /// Replace the cached members of `path` with the ones just parsed for `file_hash`.
    ///
    /// Rows for other file hashes of the same path are dropped: that content can never be asked
    /// for again, because the cache is only ever consulted with the file's current hash.
    pub fn store_member_index(
        &mut self,
        repository: &str,
        path: &str,
        file_hash: &str,
        parse_error: bool,
        members: &[CachedMember],
    ) -> Result<(), CmdError> {
        let transaction = self
            .conn
            .transaction()
            .map_err(|e| CmdError::env(format!("cannot start a database transaction: {e}")))?;
        transaction
            .execute(
                "DELETE FROM member_index WHERE repository = ?1 AND path = ?2",
                rusqlite::params![repository, path],
            )
            .and_then(|_| {
                transaction.execute(
                    "DELETE FROM member_index_files WHERE repository = ?1 AND path = ?2",
                    rusqlite::params![repository, path],
                )
            })
            .map_err(|e| {
                CmdError::env(format!("cannot refresh the member cache for {path}: {e}"))
            })?;
        transaction
            .execute(
                "INSERT INTO member_index_files (repository, path, file_hash, parse_error, \
                 updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    repository,
                    path,
                    file_hash,
                    if parse_error { 1_i64 } else { 0_i64 },
                    now_rfc3339()
                ],
            )
            .map_err(|e| {
                CmdError::env(format!("cannot refresh the member cache for {path}: {e}"))
            })?;
        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO member_index (repository, path, file_hash, symbol, symbol_kind, \
                     name, qualified_name, start_line, end_line, hash) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                )
                .map_err(|e| {
                    CmdError::env(format!("cannot refresh the member cache for {path}: {e}"))
                })?;
            for member in members {
                statement
                    .execute(rusqlite::params![
                        repository,
                        path,
                        file_hash,
                        member.symbol,
                        member.symbol_kind,
                        member.name,
                        member.qualified_name,
                        member.start_line as i64,
                        member.end_line as i64,
                        member.hash
                    ])
                    .map_err(|e| {
                        CmdError::env(format!("cannot refresh the member cache for {path}: {e}"))
                    })?;
            }
        }
        transaction
            .commit()
            .map_err(|e| CmdError::env(format!("cannot commit the member cache for {path}: {e}")))
    }

    /// Every `(path, file_hash)` pair whose members are already cached for this repository.
    ///
    /// One query, so accounting for a whole tree stays O(1) round trips to the database.
    pub fn member_index_keys(
        &self,
        repository: &str,
    ) -> Result<std::collections::BTreeSet<(String, String)>, CmdError> {
        let mut statement = self
            .conn
            .prepare("SELECT path, file_hash FROM member_index_files WHERE repository = ?1")
            .map_err(|e| CmdError::env(format!("cannot read the member cache: {e}")))?;
        let rows = statement
            .query_map([repository], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| CmdError::env(format!("cannot read the member cache: {e}")))?;
        let mut keys = std::collections::BTreeSet::new();
        for row in rows {
            keys.insert(
                row.map_err(|e| CmdError::env(format!("cannot read the member cache: {e}")))?,
            );
        }
        Ok(keys)
    }

    /// Record `state_hash`, the commit it was observed at and every entry of the state, so a
    /// later invocation can name exactly what changed without re-reading anything.
    pub fn record_snapshot(
        &mut self,
        repository: &str,
        state_hash: &str,
        commit: Option<&str>,
        entries: &[crate::repo::Entry],
    ) -> Result<(), CmdError> {
        let transaction = self
            .conn
            .transaction()
            .map_err(|e| CmdError::env(format!("cannot start a database transaction: {e}")))?;
        transaction
            .execute(
                "INSERT INTO snapshots (repository, state_hash, commit_sha, updated_at) \
                 VALUES (?1, ?2, ?3, ?4) ON CONFLICT (repository, state_hash) DO UPDATE SET \
                 commit_sha = excluded.commit_sha, updated_at = excluded.updated_at",
                rusqlite::params![repository, state_hash, commit, now_rfc3339()],
            )
            .map_err(|e| CmdError::env(format!("cannot record the state: {e}")))?;
        let snapshot_id: i64 = transaction
            .query_row(
                "SELECT id FROM snapshots WHERE repository = ?1 AND state_hash = ?2",
                rusqlite::params![repository, state_hash],
                |row| row.get(0),
            )
            .map_err(|e| CmdError::env(format!("cannot record the state: {e}")))?;
        transaction
            .execute(
                "DELETE FROM snapshot_entries WHERE snapshot_id = ?1",
                [snapshot_id],
            )
            .map_err(|e| CmdError::env(format!("cannot record the state: {e}")))?;
        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO snapshot_entries (snapshot_id, path, kind, hash) \
                     VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(|e| CmdError::env(format!("cannot record the state: {e}")))?;
            for entry in entries {
                statement
                    .execute(rusqlite::params![
                        snapshot_id,
                        entry.path,
                        entry.kind.as_str(),
                        entry.hash
                    ])
                    .map_err(|e| CmdError::env(format!("cannot record the state: {e}")))?;
            }
        }
        transaction
            .commit()
            .map_err(|e| CmdError::env(format!("cannot commit the recorded state: {e}")))
    }

    /// The recorded state with this exact hash, when this build has already computed it.
    pub fn snapshot_info(
        &self,
        repository: &str,
        state_hash: &str,
    ) -> Result<Option<SnapshotInfo>, CmdError> {
        self.conn
            .query_row(
                "SELECT state_hash, commit_sha, updated_at FROM snapshots \
                 WHERE repository = ?1 AND state_hash = ?2",
                rusqlite::params![repository, state_hash],
                |row| {
                    Ok(SnapshotInfo {
                        state_hash: row.get(0)?,
                        commit: row.get(1)?,
                        updated_at: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(|e| CmdError::env(format!("cannot read the recorded states: {e}")))
    }

    /// The recorded state with this exact hash, with its entries.
    pub fn snapshot(
        &self,
        repository: &str,
        state_hash: &str,
    ) -> Result<Option<(SnapshotInfo, SnapshotEntries)>, CmdError> {
        let found = self
            .conn
            .query_row(
                "SELECT id, state_hash, commit_sha, updated_at FROM snapshots WHERE \
                 repository = ?1 AND state_hash = ?2 ORDER BY id DESC LIMIT 1",
                rusqlite::params![repository, state_hash],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        SnapshotInfo {
                            state_hash: row.get(1)?,
                            commit: row.get(2)?,
                            updated_at: row.get(3)?,
                        },
                    ))
                },
            )
            .optional()
            .map_err(|e| CmdError::env(format!("cannot read the recorded states: {e}")))?;
        let Some((snapshot_id, info)) = found else {
            return Ok(None);
        };
        let entries = self.snapshot_entry_rows(snapshot_id)?;
        Ok(Some((info, entries)))
    }

    /// Every `(path, kind, hash)` row of one recorded state.
    fn snapshot_entry_rows(&self, snapshot_id: i64) -> Result<SnapshotEntries, CmdError> {
        let mut statement = self
            .conn
            .prepare("SELECT path, kind, hash FROM snapshot_entries WHERE snapshot_id = ?1")
            .map_err(|e| CmdError::env(format!("cannot read the recorded states: {e}")))?;
        let rows = statement
            .query_map([snapshot_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|e| CmdError::env(format!("cannot read the recorded states: {e}")))?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(
                row.map_err(|e| CmdError::env(format!("cannot read the recorded states: {e}")))?,
            );
        }
        Ok(entries)
    }

    /// The most recently recorded state whose hash is not `exclude`, with its entries.
    pub fn latest_snapshot(
        &self,
        repository: &str,
        exclude: Option<&str>,
    ) -> Result<Option<(SnapshotInfo, SnapshotEntries)>, CmdError> {
        let found = self
            .conn
            .query_row(
                "SELECT id, state_hash, commit_sha, updated_at FROM snapshots WHERE \
                 repository = ?1 AND (?2 IS NULL OR state_hash <> ?2) ORDER BY id DESC LIMIT 1",
                rusqlite::params![repository, exclude],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        SnapshotInfo {
                            state_hash: row.get(1)?,
                            commit: row.get(2)?,
                            updated_at: row.get(3)?,
                        },
                    ))
                },
            )
            .optional()
            .map_err(|e| CmdError::env(format!("cannot read the recorded states: {e}")))?;
        let Some((snapshot_id, info)) = found else {
            return Ok(None);
        };
        let entries = self.snapshot_entry_rows(snapshot_id)?;
        Ok(Some((info, entries)))
    }
}
