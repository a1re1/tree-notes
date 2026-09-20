//! SQLite note storage: versioned, content-addressed notes in a local database.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;

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
        let mode: String = conn
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .map_err(|e| CmdError::env(format!("cannot enable WAL journalling: {e}")))?;
        let _ = mode;
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
