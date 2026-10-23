use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::Mutex;

use crate::metrics::Facts;
use crate::model::{Issue, Pr};

pub struct Store {
    conn: Mutex<Connection>,
}

/// Someone who logged in. Their area is derived, plus the tags they added.
pub struct Member {
    pub login: String,
    pub committed: Vec<String>,
    pub tags: Vec<String>,
    pub refreshed_at: DateTime<Utc>,
}

pub struct Session {
    pub login: String,
    pub token: String,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let mut conn =
            Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             CREATE TABLE IF NOT EXISTS pr (
               number INTEGER PRIMARY KEY,
               updated_at TEXT NOT NULL,
               state TEXT NOT NULL,
               data TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS history (
               number INTEGER PRIMARY KEY,
               updated_at TEXT NOT NULL,
               data TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS issue (
               number INTEGER PRIMARY KEY,
               updated_at TEXT NOT NULL,
               state TEXT NOT NULL,
               data TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS member (
               login TEXT PRIMARY KEY,
               committed TEXT NOT NULL,
               tags TEXT NOT NULL,
               refreshed_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS watch (
               login TEXT NOT NULL,
               package TEXT NOT NULL,
               created_at TEXT NOT NULL,
               PRIMARY KEY (login, package)
             );
             CREATE TABLE IF NOT EXISTS setting (
               login TEXT NOT NULL,
               key TEXT NOT NULL,
               value TEXT NOT NULL,
               PRIMARY KEY (login, key)
             );
             CREATE TABLE IF NOT EXISTS feed (
               login TEXT PRIMARY KEY,
               token TEXT NOT NULL UNIQUE,
               created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS upload_token (
               login TEXT PRIMARY KEY,
               hash TEXT NOT NULL UNIQUE,
               created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS channel (
               key TEXT PRIMARY KEY,
               data TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS rescan_run (login TEXT NOT NULL, at TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS unsubscribe (
               hash TEXT PRIMARY KEY,
               login TEXT NOT NULL,
               created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS mail_confirm (
               hash TEXT PRIMARY KEY,
               login TEXT NOT NULL,
               address TEXT NOT NULL,
               created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS mail_log (login TEXT NOT NULL, kind TEXT NOT NULL, at TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS mail_log_at ON mail_log (at);
             CREATE INDEX IF NOT EXISTS mail_log_own ON mail_log (login, kind, at);
             CREATE TABLE IF NOT EXISTS mail_skip (hour TEXT PRIMARY KEY, n INTEGER NOT NULL);
             UPDATE watch SET package = 'pkg:' || package WHERE instr(package, ':') = 0;
             CREATE TABLE IF NOT EXISTS session (
               id TEXT PRIMARY KEY,
               login TEXT NOT NULL,
               token TEXT NOT NULL,
               created_at TEXT NOT NULL,
               seen_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS needs_snooze (
               login TEXT NOT NULL,
               key TEXT NOT NULL,
               fingerprint TEXT NOT NULL,
               at TEXT NOT NULL,
               PRIMARY KEY (login, key)
             );
             CREATE TABLE IF NOT EXISTS needs_seen (
               session TEXT NOT NULL,
               key TEXT NOT NULL,
               at TEXT NOT NULL,
               PRIMARY KEY (session, key)
             );
             CREATE TABLE IF NOT EXISTS followup (
               login TEXT NOT NULL,
               number INTEGER NOT NULL,
               days INTEGER NOT NULL,
               action TEXT NOT NULL,
               since TEXT NOT NULL,
               PRIMARY KEY (login, number)
             );
             CREATE TABLE IF NOT EXISTS mail_once (
               login TEXT NOT NULL,
               id TEXT NOT NULL,
               at TEXT NOT NULL,
               PRIMARY KEY (login, id)
             );
             CREATE TABLE IF NOT EXISTS hydra_state (
               jobset TEXT NOT NULL,
               attr TEXT NOT NULL,
               build INTEGER,
               state TEXT,
               checked_at TEXT NOT NULL,
               PRIMARY KEY (jobset, attr)
             );
             CREATE TABLE IF NOT EXISTS hydra_change (
               jobset TEXT NOT NULL,
               attr TEXT NOT NULL,
               build INTEGER NOT NULL,
               state TEXT NOT NULL,
               at TEXT NOT NULL,
               PRIMARY KEY (jobset, attr, build)
             );",
        )?;
        migrate_sets(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn put_prs(&self, prs: &[Pr]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        for pr in prs {
            tx.execute(
                "INSERT INTO pr (number, updated_at, state, data) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(number) DO UPDATE SET
                   updated_at = excluded.updated_at, state = excluded.state, data = excluded.data",
                params![
                    pr.number,
                    pr.updated_at.to_rfc3339(),
                    pr.state,
                    serde_json::to_string(pr)?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn put_facts(&self, facts: &[Facts]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        for f in facts {
            tx.execute(
                "INSERT INTO history (number, updated_at, data) VALUES (?1, ?2, ?3)
                 ON CONFLICT(number) DO UPDATE SET updated_at = excluded.updated_at, data = excluded.data",
                params![f.number, f.updated_at.to_rfc3339(), serde_json::to_string(f)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn facts(&self) -> Result<Vec<Facts>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT data FROM history")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row?)?);
        }
        Ok(out)
    }

    /// Who merged each pull request in the history walk, and when.
    pub fn merges(&self) -> Result<Vec<(DateTime<Utc>, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT json_extract(data, '$.merged_at'), json_extract(data, '$.merged_by') FROM history
             WHERE json_extract(data, '$.merged_by') IS NOT NULL AND json_extract(data, '$.merged_at') IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (at, by) = row?;
            out.push((DateTime::parse_from_rfc3339(&at)?.with_timezone(&Utc), by));
        }
        Ok(out)
    }

    pub fn set_mergeable(&self, number: u64, mergeable: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE pr SET data = json_set(data, '$.mergeable', ?2) WHERE number = ?1",
            params![number, mergeable],
        )?;
        Ok(())
    }

    pub fn prs(&self) -> Result<Vec<Pr>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT data FROM pr")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row?)?);
        }
        Ok(out)
    }

    pub fn put_issues(&self, issues: &[Issue]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        for i in issues {
            tx.execute(
                "INSERT INTO issue (number, updated_at, state, data) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(number) DO UPDATE SET
                   updated_at = excluded.updated_at, state = excluded.state, data = excluded.data",
                params![
                    i.number,
                    i.updated_at.to_rfc3339(),
                    i.state,
                    serde_json::to_string(i)?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn issues(&self) -> Result<Vec<Issue>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT data FROM issue")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row?)?);
        }
        Ok(out)
    }

    pub fn get(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    pub fn delete_key(&self, key: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM meta WHERE key = ?1", [key])?;
        Ok(())
    }

    pub fn put_member(&self, login: &str, committed: &[String]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO member (login, committed, tags, refreshed_at) VALUES (?1, ?2, '[]', ?3)
             ON CONFLICT(login) DO UPDATE SET committed = excluded.committed, refreshed_at = excluded.refreshed_at",
            params![login, serde_json::to_string(committed)?, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// A person whose first area refresh failed has no member row yet. The
    /// new row is stale at once, so the next sync pass refreshes it.
    pub fn set_tags(&self, login: &str, tags: &[String]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO member (login, committed, tags, refreshed_at)
             VALUES (?1, '[]', ?2, '1970-01-01T00:00:00+00:00')
             ON CONFLICT(login) DO UPDATE SET tags = excluded.tags",
            params![login, serde_json::to_string(tags)?],
        )?;
        Ok(())
    }

    pub fn members(&self) -> Result<Vec<Member>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT login, committed, tags, refreshed_at FROM member")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (login, committed, tags, refreshed) = row?;
            out.push(Member {
                login,
                committed: serde_json::from_str(&committed)?,
                tags: serde_json::from_str(&tags)?,
                refreshed_at: DateTime::parse_from_rfc3339(&refreshed)?.with_timezone(&Utc),
            });
        }
        Ok(out)
    }

    pub fn get_time(&self, key: &str) -> Result<Option<DateTime<Utc>>> {
        Ok(self
            .get(key)?
            .and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
            .map(|t| t.with_timezone(&Utc)))
    }

    pub fn create_session(&self, id: &str, login: &str, token: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO session (id, login, token, created_at, seen_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, login, token, now],
        )?;
        Ok(())
    }

    pub fn session(&self, id: &str) -> Result<Option<Session>> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE session SET seen_at = ?2 WHERE id = ?1",
            params![id, Utc::now().to_rfc3339()],
        )?;
        Ok(conn
            .query_row(
                "SELECT login, token FROM session WHERE id = ?1",
                [id],
                |r| {
                    Ok(Session {
                        login: r.get(0)?,
                        token: r.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM session WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn forget_token(&self, token: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM session WHERE token = ?1", [token])?;
        Ok(())
    }

    pub fn token_for(&self, login: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT token FROM session WHERE login = ?1 ORDER BY seen_at DESC LIMIT 1",
                [login],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Tokens of people seen in the last 30 days, most recent first. The sync
    /// spreads its reads across them; see docs/data.md on rate limits.
    pub fn lent_tokens(&self) -> Result<Vec<String>> {
        let since = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT token FROM session WHERE seen_at > ?1
             GROUP BY token ORDER BY MAX(seen_at) DESC",
        )?;
        let rows = stmt.query_map([since], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

impl Store {
    pub fn watch(&self, login: &str, key: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO watch (login, package, created_at) VALUES (?1, ?2, ?3)",
            params![login, key, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn unwatch(&self, login: &str, key: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM watch WHERE login = ?1 AND package = ?2",
            params![login, key],
        )?;
        Ok(())
    }

    /// Every key anyone watches, once.
    pub fn watched_by_anyone(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT DISTINCT package FROM watch ORDER BY package")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn watched(&self, login: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT package FROM watch WHERE login = ?1 ORDER BY package")?;
        let rows = stmt.query_map([login], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

impl Store {
    pub fn feed_token(&self, login: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row("SELECT token FROM feed WHERE login = ?1", [login], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// A new token replaces the old one, so a leaked feed link can be cut.
    pub fn set_feed_token(&self, login: &str, token: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO feed (login, token, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(login) DO UPDATE SET token = excluded.token, created_at = excluded.created_at",
            params![login, token, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn feed_login(&self, token: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row("SELECT login FROM feed WHERE token = ?1", [token], |r| {
                r.get(0)
            })
            .optional()?)
    }
}

impl Store {
    pub fn setting(&self, login: &str, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT value FROM setting WHERE login = ?1 AND key = ?2",
                [login, key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn has_table(&self, name: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten()
        .is_some()
    }

    pub fn set_setting(&self, login: &str, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO setting (login, key, value) VALUES (?1, ?2, ?3)
             ON CONFLICT(login, key) DO UPDATE SET value = excluded.value",
            [login, key, value],
        )?;
        Ok(())
    }
}

impl Store {
    pub fn put_channel(&self, c: &crate::channels::Channel) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO channel (key, data) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET data = excluded.data",
            params![c.key, serde_json::to_string(c)?],
        )?;
        Ok(())
    }

    pub fn channels(&self) -> Result<Vec<crate::channels::Channel>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT data FROM channel")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row?)?);
        }
        Ok(out)
    }

    /// Drops the channels no longer configured.
    pub fn keep_channels(&self, keys: &[String]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT key FROM channel")?;
        let stored: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for k in stored.iter().filter(|k| !keys.contains(k)) {
            conn.execute("DELETE FROM channel WHERE key = ?1", [k])?;
        }
        Ok(())
    }

    /// The login whose setting `key` holds `value`, for a value that only
    /// one login can hold, like a token hash.
    pub fn setting_login(&self, key: &str, value: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT login FROM setting WHERE key = ?1 AND value = ?2",
                [key, value],
                |r| r.get(0),
            )
            .optional()?)
    }
}

/// The data of one named set: an upload, or the last good scan of a flake.
pub struct PackageSet {
    pub attrs: Vec<String>,
    pub dropped: usize,
    pub uploaded_at: DateTime<Utc>,
    pub scanned_at: Option<String>,
    pub nixpkgs_rev: Option<String>,
    /// attribute → the version the report gave, only where nixpkgs had
    /// that exact version at upload.
    pub versions: std::collections::BTreeMap<String, String>,
    /// Only from an upload with `--with-outputs`.
    pub outputs: SetOutputs,
}

/// The outputs of a set, such as its hosts, and the outputs that hold each
/// attribute.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SetOutputs {
    pub labels: Vec<String>,
    /// attribute → indices in `labels`
    pub of: std::collections::BTreeMap<String, Vec<usize>>,
}

impl SetOutputs {
    /// The labels of the outputs that hold `attr`.
    pub fn labels_of(&self, attr: &str) -> impl Iterator<Item = &str> {
        self.of
            .get(attr)
            .into_iter()
            .flatten()
            .filter_map(|i| self.labels.get(*i))
            .map(String::as_str)
    }
}

/// The name of the set of an upload with no name, and of the sets that
/// existed before names.
pub const DEFAULT_SET: &str = "default";

/// One of the named sets of a person. Private unless `public` is set,
/// which only a set with a public flake reference can be.
pub struct NamedSet {
    pub name: String,
    /// `None` for a flake that the server has not scanned with success yet.
    pub data: Option<PackageSet>,
    /// The public flake reference and output of the set.
    pub flake: Option<(String, String)>,
    pub public: bool,
    /// The server scans the flake after each nixpkgs index.
    pub rescan: bool,
}

pub enum Put {
    Stored,
    /// The name belongs to a flake that the server scans.
    Scanned,
    /// The login has [`MAX_SETS`] other sets.
    Full,
}

pub enum AddFlake {
    Added,
    /// The login has a set with this name.
    Taken,
    Full,
}

/// Sets a person can have, all kinds together.
pub const MAX_SETS: usize = 50;

const SET_TABLE: &str = "CREATE TABLE IF NOT EXISTS package_set (
  login TEXT NOT NULL,
  name TEXT NOT NULL,
  attrs TEXT NOT NULL DEFAULT '[]',
  dropped INTEGER NOT NULL DEFAULT 0,
  uploaded_at TEXT,
  scanned_at TEXT,
  nixpkgs_rev TEXT,
  versions TEXT NOT NULL DEFAULT '{}',
  reference TEXT,
  output TEXT,
  public INTEGER NOT NULL DEFAULT 0,
  rescan INTEGER NOT NULL DEFAULT 0,
  created_at TEXT,
  seen_at TEXT,
  last_at TEXT,
  error TEXT,
  outputs TEXT NOT NULL DEFAULT '{}',
  PRIMARY KEY (login, name)
);";

fn has_table(conn: &Connection, table: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
        [table, column],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Before names, a login had one uploaded set (`package_set`, keyed by
/// login) and its server-scanned flakes (`flake_ref`). Both move to the
/// one table of named sets: the upload as [`DEFAULT_SET`], each flake as
/// `flake-<id>`.
pub(crate) fn migrate_sets(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;
    let old = has_table(&tx, "package_set")? && !has_column(&tx, "package_set", "name")?;
    if old {
        if !has_column(&tx, "package_set", "versions")? {
            tx.execute_batch(
                "ALTER TABLE package_set ADD COLUMN versions TEXT NOT NULL DEFAULT '{}'",
            )?;
        }
        tx.execute_batch("ALTER TABLE package_set RENAME TO package_set_v1")?;
    }
    tx.execute_batch(SET_TABLE)?;
    if !has_column(&tx, "package_set", "outputs")? {
        tx.execute_batch("ALTER TABLE package_set ADD COLUMN outputs TEXT NOT NULL DEFAULT '{}'")?;
    }
    if old {
        tx.execute(
            "INSERT INTO package_set (login, name, attrs, dropped, uploaded_at, scanned_at, nixpkgs_rev, versions)
             SELECT login, ?1, attrs, dropped, uploaded_at, scanned_at, nixpkgs_rev, versions FROM package_set_v1",
            [DEFAULT_SET],
        )?;
        tx.execute_batch("DROP TABLE package_set_v1")?;
    }
    if has_table(&tx, "flake_ref")? {
        tx.execute_batch(
            "INSERT OR IGNORE INTO package_set (login, name, attrs, dropped, uploaded_at, scanned_at,
               nixpkgs_rev, reference, output, rescan, created_at, seen_at, last_at, error)
             SELECT login, 'flake-' || id, attrs, dropped, scanned_at, scanned_at,
               nixpkgs_rev, reference, output, 1, created_at, seen_at, last_at, error FROM flake_ref;
             DROP TABLE flake_ref;",
        )?;
    }
    tx.commit()?;
    Ok(())
}

const SET_COLUMNS: &str = "name, attrs, dropped, uploaded_at, scanned_at, nixpkgs_rev, versions, reference, output, public, rescan, outputs";

fn set_row(r: &rusqlite::Row) -> rusqlite::Result<NamedSet> {
    let uploaded = time_of(r.get(3)?);
    let reference: Option<String> = r.get(7)?;
    let output: Option<String> = r.get(8)?;
    Ok(NamedSet {
        name: r.get(0)?,
        data: match uploaded {
            Some(uploaded_at) => Some(PackageSet {
                attrs: serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or_default(),
                dropped: r.get::<_, i64>(2)?.max(0) as usize,
                uploaded_at,
                scanned_at: r.get(4)?,
                nixpkgs_rev: r.get(5)?,
                versions: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
                outputs: serde_json::from_str(&r.get::<_, String>(11)?).unwrap_or_default(),
            }),
            None => None,
        },
        flake: reference.zip(output),
        public: r.get(9)?,
        rescan: r.get(10)?,
    })
}

impl Store {
    /// A new token replaces the old one. Only its hash is kept.
    pub fn set_upload_token(&self, login: &str, hash: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO upload_token (login, hash, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(login) DO UPDATE SET hash = excluded.hash, created_at = excluded.created_at",
            params![login, hash, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn upload_token_at(&self, login: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT created_at FROM upload_token WHERE login = ?1",
                [login],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn upload_login(&self, hash: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT login FROM upload_token WHERE hash = ?1",
                [hash],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn delete_upload_token(&self, login: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM upload_token WHERE login = ?1", [login])?;
        Ok(())
    }

    /// An upload replaces the whole set of that name, the flake and the
    /// public mark too: `flake` makes it a public flake, `None` a private
    /// set.
    pub fn put_package_set(
        &self,
        login: &str,
        name: &str,
        set: &PackageSet,
        flake: Option<(&str, &str)>,
    ) -> Result<Put> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let rescan: Option<bool> = tx
            .query_row(
                "SELECT rescan FROM package_set WHERE login = ?1 AND name = ?2",
                [login, name],
                |r| r.get(0),
            )
            .optional()?;
        match rescan {
            Some(true) => return Ok(Put::Scanned),
            Some(false) => {}
            None => {
                let n: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM package_set WHERE login = ?1",
                    [login],
                    |r| r.get(0),
                )?;
                if n as usize >= MAX_SETS {
                    return Ok(Put::Full);
                }
            }
        }
        tx.execute(
            "INSERT INTO package_set (login, name, attrs, dropped, uploaded_at, scanned_at, nixpkgs_rev, versions, reference, output, public, outputs)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(login, name) DO UPDATE SET attrs = excluded.attrs, dropped = excluded.dropped,
               uploaded_at = excluded.uploaded_at, scanned_at = excluded.scanned_at,
               nixpkgs_rev = excluded.nixpkgs_rev, versions = excluded.versions,
               reference = excluded.reference, output = excluded.output, public = excluded.public,
               outputs = excluded.outputs",
            params![
                login,
                name,
                serde_json::to_string(&set.attrs)?,
                set.dropped as i64,
                set.uploaded_at.to_rfc3339(),
                set.scanned_at,
                set.nixpkgs_rev,
                serde_json::to_string(&set.versions)?,
                flake.map(|f| f.0),
                flake.map(|f| f.1),
                flake.is_some(),
                serde_json::to_string(&set.outputs)?,
            ],
        )?;
        tx.commit()?;
        Ok(Put::Stored)
    }

    /// The data of one set, if it has any.
    pub fn package_set(&self, login: &str, name: &str) -> Result<Option<PackageSet>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                &format!("SELECT {SET_COLUMNS} FROM package_set WHERE login = ?1 AND name = ?2"),
                [login, name],
                set_row,
            )
            .optional()?
            .and_then(|s| s.data))
    }

    /// All sets of a login, by name. Private: only for the owner.
    pub fn sets(&self, login: &str) -> Result<Vec<NamedSet>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SET_COLUMNS} FROM package_set WHERE login = ?1 ORDER BY name"
        ))?;
        let rows = stmt.query_map([login], set_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The sets that the owner shows on their profile: public flakes only.
    pub fn public_sets(&self, login: &str) -> Result<Vec<NamedSet>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SET_COLUMNS} FROM package_set
             WHERE login = ?1 AND public = 1 AND reference IS NOT NULL AND output IS NOT NULL
             ORDER BY name"
        ))?;
        let rows = stmt.query_map([login], set_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Only a set with a flake reference can be public. Returns whether a
    /// set changed.
    pub fn set_public(&self, login: &str, name: &str, public: bool) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "UPDATE package_set SET public = ?3
             WHERE login = ?1 AND name = ?2 AND reference IS NOT NULL AND output IS NOT NULL",
            params![login, name, public],
        )? > 0)
    }

    pub fn delete_package_set(&self, login: &str, name: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM package_set WHERE login = ?1 AND name = ?2",
            [login, name],
        )?;
        Ok(())
    }

    pub fn delete_package_sets(&self, login: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM package_set WHERE login = ?1", [login])?;
        Ok(())
    }

    /// Deletes every uploaded set with no upload since `before`; returns
    /// how many. A set of a scanned flake follows
    /// [`Store::purge_flake_results`].
    pub fn purge_package_sets(&self, before: DateTime<Utc>) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM package_set WHERE rescan = 0 AND uploaded_at < ?1",
            [before.to_rfc3339()],
        )?)
    }
}

/// A named set whose public flake the server scans again after each
/// nixpkgs index.
pub struct FlakeRef {
    pub login: String,
    pub name: String,
    pub reference: String,
    pub output: String,
    pub public: bool,
    pub last_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub attrs: Vec<String>,
    pub dropped: usize,
    pub scanned_at: Option<DateTime<Utc>>,
    pub nixpkgs_rev: Option<String>,
}

fn mail_hour(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H").to_string()
}

fn time_of(s: Option<String>) -> Option<DateTime<Utc>> {
    s.and_then(|t| DateTime::parse_from_rfc3339(&t).ok())
        .map(|t| t.with_timezone(&Utc))
}

const FLAKE_COLUMNS: &str = "login, name, reference, output, public, last_at, error, attrs, dropped, uploaded_at, nixpkgs_rev";

fn flake_row(r: &rusqlite::Row) -> rusqlite::Result<FlakeRef> {
    Ok(FlakeRef {
        login: r.get(0)?,
        name: r.get(1)?,
        reference: r.get(2)?,
        output: r.get(3)?,
        public: r.get(4)?,
        last_at: time_of(r.get(5)?),
        error: r.get(6)?,
        attrs: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
        dropped: r.get::<_, i64>(8)?.max(0) as usize,
        scanned_at: time_of(r.get(9)?),
        nixpkgs_rev: r.get(10)?,
    })
}

impl Store {
    /// Adds a flake for the server to scan, under a new name. At most
    /// `max` such flakes and [`MAX_SETS`] sets for the login.
    pub fn add_flake_ref(
        &self,
        login: &str,
        name: &str,
        reference: &str,
        output: &str,
        public: bool,
        max: usize,
    ) -> Result<AddFlake> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let (all, flakes): (i64, i64) = tx.query_row(
            "SELECT COUNT(*), COALESCE(SUM(rescan), 0) FROM package_set WHERE login = ?1",
            [login],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if flakes as usize >= max || all as usize >= MAX_SETS {
            return Ok(AddFlake::Full);
        }
        let now = Utc::now().to_rfc3339();
        let added = tx.execute(
            "INSERT OR IGNORE INTO package_set (login, name, reference, output, public, rescan, created_at, seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)",
            params![login, name, reference, output, public, now],
        )?;
        if added == 0 {
            return Ok(AddFlake::Taken);
        }
        tx.execute(
            "UPDATE package_set SET seen_at = ?2 WHERE login = ?1 AND rescan = 1",
            params![login, now],
        )?;
        tx.commit()?;
        Ok(AddFlake::Added)
    }

    pub fn flake_refs(&self, login: &str) -> Result<Vec<FlakeRef>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FLAKE_COLUMNS} FROM package_set WHERE login = ?1 AND rescan = 1 ORDER BY name"
        ))?;
        let rows = stmt.query_map([login], flake_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The owner looked at the flakes: they stay due for scans.
    pub fn touch_flake_refs(&self, login: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE package_set SET seen_at = ?2 WHERE login = ?1 AND rescan = 1",
            params![login, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// References never scanned first, then those last scanned before `since`.
    /// Only those whose owner came back after `seen_since`.
    pub fn flake_refs_due(
        &self,
        since: DateTime<Utc>,
        seen_since: DateTime<Utc>,
    ) -> Result<Vec<FlakeRef>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FLAKE_COLUMNS} FROM package_set
             WHERE rescan = 1 AND (last_at IS NULL OR last_at < ?1) AND seen_at >= ?2
             ORDER BY last_at IS NOT NULL, last_at, created_at, login, name"
        ))?;
        let rows = stmt.query_map([since.to_rfc3339(), seen_since.to_rfc3339()], flake_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// A failure keeps the attributes of the last good scan. A set that the
    /// owner deleted or gave another flake during the scan stays as it is.
    pub fn finish_flake_ref(&self, r: &FlakeRef, result: Result<&PackageSet, &str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().to_rfc3339();
        let key = "login = ?1 AND name = ?2 AND reference = ?3 AND output = ?4 AND rescan = 1";
        match result {
            Ok(set) => conn.execute(
                &format!(
                    "UPDATE package_set SET last_at = ?5, error = NULL, attrs = ?6, dropped = ?7,
                       uploaded_at = ?8, scanned_at = ?8, nixpkgs_rev = ?9, versions = '{{}}' WHERE {key}"
                ),
                params![
                    r.login,
                    r.name,
                    r.reference,
                    r.output,
                    now,
                    serde_json::to_string(&set.attrs)?,
                    set.dropped as i64,
                    set.uploaded_at.to_rfc3339(),
                    set.nixpkgs_rev
                ],
            )?,
            Err(e) => conn.execute(
                &format!("UPDATE package_set SET last_at = ?5, error = ?6 WHERE {key}"),
                params![r.login, r.name, r.reference, r.output, now, e],
            )?,
        };
        Ok(())
    }

    /// The 90-day rule of an upload: the attributes go 90 days after the last
    /// good scan or the last visit of the owner. The reference stays.
    /// Returns how many.
    pub fn purge_flake_results(&self, before: DateTime<Utc>) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "UPDATE package_set SET attrs = '[]', dropped = 0, uploaded_at = NULL, scanned_at = NULL,
               nixpkgs_rev = NULL, versions = '{}'
             WHERE rescan = 1 AND uploaded_at IS NOT NULL AND (uploaded_at < ?1 OR seen_at < ?1)",
            [before.to_rfc3339()],
        )?)
    }

    pub fn log_rescan_run(&self, login: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now();
        conn.execute(
            "DELETE FROM rescan_run WHERE at < ?1",
            [(now - chrono::Duration::days(2)).to_rfc3339()],
        )?;
        conn.execute(
            "INSERT INTO rescan_run (login, at) VALUES (?1, ?2)",
            [login, &now.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn rescan_runs_since(&self, login: &str, since: DateTime<Utc>) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM rescan_run WHERE login = ?1 AND at >= ?2",
            [login, &since.to_rfc3339()],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    pub fn rescan_runs_total_since(&self, since: DateTime<Utc>) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM rescan_run WHERE at >= ?1",
            [since.to_rfc3339()],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Counts only, for /sync: references, failing ones, the last scan.
    pub fn flake_ref_counts(&self) -> Result<(usize, usize, Option<DateTime<Utc>>)> {
        let conn = self.conn.lock().unwrap();
        let (n, failing, last): (i64, i64, Option<String>) = conn.query_row(
            "SELECT COUNT(*), COUNT(error), MAX(last_at) FROM package_set WHERE rescan = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok((n as usize, failing as usize, time_of(last)))
    }
}

impl Store {
    /// Logins whose setting `key` is not empty, with its value.
    pub fn settings_with(&self, key: &str) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT login, value FROM setting WHERE key = ?1 AND value != '' ORDER BY login",
        )?;
        let rows = stmt.query_map([key], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Each email carries its own unsubscribe token; only its hash is kept.
    pub fn add_unsubscribe(&self, login: &str, hash: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO unsubscribe (hash, login, created_at) VALUES (?1, ?2, ?3)",
            params![hash, login, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn unsubscribe_login(&self, hash: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT login FROM unsubscribe WHERE hash = ?1",
                [hash],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn delete_unsubscribes(&self, login: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM unsubscribe WHERE login = ?1", [login])?;
        Ok(())
    }

    pub fn purge_unsubscribes(&self, before: DateTime<Utc>) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM unsubscribe WHERE created_at < ?1",
            [before.to_rfc3339()],
        )?)
    }
}

/// A link that confirms a digest address. Only the hash of its token is kept.
pub struct Confirmation {
    pub login: String,
    pub address: String,
    pub created_at: DateTime<Utc>,
}

impl Store {
    pub fn add_confirmation(
        &self,
        login: &str,
        address: &str,
        hash: &str,
        at: DateTime<Utc>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO mail_confirm (hash, login, address, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![hash, login, address, at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn confirmation(&self, hash: &str) -> Result<Option<Confirmation>> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(String, String, String)> = conn
            .query_row(
                "SELECT login, address, created_at FROM mail_confirm WHERE hash = ?1",
                [hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        Ok(row.and_then(|(login, address, at)| {
            Some(Confirmation {
                login,
                address,
                created_at: time_of(Some(at))?,
            })
        }))
    }

    pub fn delete_confirmations(&self, login: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM mail_confirm WHERE login = ?1", [login])?;
        Ok(())
    }

    /// Logs an email of `kind` when it fits under both limits of the 24
    /// hours before `now`: `total_max` for the server, `own_max` for this
    /// login and kind. A refused email only adds one to the skip count of
    /// its hour, so refusals cannot grow the tables.
    pub fn take_mail_slot(
        &self,
        login: &str,
        kind: &str,
        total_max: usize,
        own_max: usize,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let since = (now - chrono::Duration::days(1)).to_rfc3339();
        let old = now - chrono::Duration::days(2);
        conn.execute("DELETE FROM mail_log WHERE at < ?1", [old.to_rfc3339()])?;
        conn.execute("DELETE FROM mail_skip WHERE hour < ?1", [mail_hour(old)])?;
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM mail_log WHERE at >= ?1",
            [&since],
            |r| r.get(0),
        )?;
        let own: i64 = conn.query_row(
            "SELECT COUNT(*) FROM mail_log WHERE login = ?1 AND kind = ?2 AND at >= ?3",
            [login, kind, &since],
            |r| r.get(0),
        )?;
        let fits = (total as usize) < total_max && (own as usize) < own_max;
        if fits {
            conn.execute(
                "INSERT INTO mail_log (login, kind, at) VALUES (?1, ?2, ?3)",
                [login, kind, &now.to_rfc3339()],
            )?;
        } else {
            conn.execute(
                "INSERT INTO mail_skip (hour, n) VALUES (?1, 1)
                 ON CONFLICT(hour) DO UPDATE SET n = n + 1",
                [mail_hour(now)],
            )?;
        }
        Ok(fits)
    }

    /// Emails sent since `since`, and emails skipped since the start of
    /// its hour, for /sync.
    pub fn mail_counts(&self, since: DateTime<Utc>) -> Result<(usize, usize)> {
        let conn = self.conn.lock().unwrap();
        let sent: i64 = conn.query_row(
            "SELECT COUNT(*) FROM mail_log WHERE at >= ?1",
            [since.to_rfc3339()],
            |r| r.get(0),
        )?;
        let skipped: i64 = conn.query_row(
            "SELECT COALESCE(SUM(n), 0) FROM mail_skip WHERE hour >= ?1",
            [mail_hour(since)],
            |r| r.get(0),
        )?;
        Ok((sent as usize, skipped as usize))
    }

    pub fn purge_confirmations(&self, before: DateTime<Utc>) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM mail_confirm WHERE created_at < ?1",
            [before.to_rfc3339()],
        )?)
    }
}

/// What a person put aside on Needs you, and what each session saw there.
impl Store {
    /// item key → the fingerprint it had when the person snoozed it.
    pub fn needs_snoozed(&self, login: &str) -> Result<std::collections::HashMap<String, String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT key, fingerprint FROM needs_snooze WHERE login = ?1")?;
        let rows = stmt.query_map([login], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn needs_snooze(&self, login: &str, key: &str, fingerprint: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO needs_snooze (login, key, fingerprint, at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(login, key) DO UPDATE SET fingerprint = excluded.fingerprint, at = excluded.at",
            params![login, key, fingerprint, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// Snoozes whose item is gone: a new item with the same key starts fresh.
    pub fn needs_forget(&self, login: &str, present: &[String]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let keys: Vec<String> = {
            let mut stmt = tx.prepare("SELECT key FROM needs_snooze WHERE login = ?1")?;
            let rows = stmt.query_map([login], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for k in keys.iter().filter(|k| !present.contains(k)) {
            tx.execute(
                "DELETE FROM needs_snooze WHERE login = ?1 AND key = ?2",
                params![login, k],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Records the open items of one session, by the hash of its id, and
    /// gives the number of items it saw that are open no more.
    pub fn needs_session(&self, session: &str, present: &[String]) -> Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM needs_seen WHERE at < ?1",
            [(Utc::now() - chrono::Duration::days(30)).to_rfc3339()],
        )?;
        let now = Utc::now().to_rfc3339();
        for k in present {
            tx.execute(
                "INSERT OR IGNORE INTO needs_seen (session, key, at) VALUES (?1, ?2, ?3)",
                params![session, k, now],
            )?;
        }
        let seen: Vec<String> = {
            let mut stmt = tx.prepare("SELECT key FROM needs_seen WHERE session = ?1")?;
            let rows = stmt.query_map([session], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        tx.commit()?;
        Ok(seen.iter().filter(|k| !present.contains(k)).count())
    }
}

/// One row of `followup`: what the author of a pull request wants when
/// nobody reviews it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FollowUp {
    pub number: u64,
    pub days: u32,
    pub action: String,
    pub since: DateTime<Utc>,
}

impl Store {
    /// `since` stays from the first save, so a change of the action does
    /// not move the deadline.
    pub fn set_followup(
        &self,
        login: &str,
        number: u64,
        days: u32,
        action: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO followup (login, number, days, action, since) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(login, number) DO UPDATE SET days = excluded.days, action = excluded.action",
            params![login, number as i64, days, action, now.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn delete_followup(&self, login: &str, number: u64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM followup WHERE login = ?1 AND number = ?2",
            params![login, number as i64],
        )?;
        Ok(())
    }

    pub fn followups(&self, login: &str) -> Result<Vec<FollowUp>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT number, days, action, since FROM followup WHERE login = ?1 ORDER BY number",
        )?;
        let rows = stmt.query_map([login], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (number, days, action, since) = row?;
            let Some(since) = time_of(Some(since)) else {
                continue;
            };
            out.push(FollowUp {
                number: number as u64,
                days: days.clamp(0, u32::MAX as i64) as u32,
                action,
                since,
            });
        }
        Ok(out)
    }

    /// Plans of pull requests that are no longer in the synced data.
    pub fn purge_followups(&self, keep: &[u64]) -> Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let all: Vec<i64> = {
            let mut stmt = tx.prepare("SELECT DISTINCT number FROM followup")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut n = 0;
        for number in all.into_iter().filter(|x| !keep.contains(&(*x as u64))) {
            n += tx.execute("DELETE FROM followup WHERE number = ?1", [number])?;
        }
        tx.commit()?;
        Ok(n)
    }

    /// True the first time `id` is marked for `login`: an event that gets
    /// its own email gets it once.
    pub fn mail_once(&self, login: &str, id: &str, now: DateTime<Utc>) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM mail_once WHERE at < ?1",
            [(now - chrono::Duration::days(45)).to_rfc3339()],
        )?;
        Ok(conn.execute(
            "INSERT OR IGNORE INTO mail_once (login, id, at) VALUES (?1, ?2, ?3)",
            params![login, id, now.to_rfc3339()],
        )? == 1)
    }

    /// Everyone who signed in, chose a setting or watches something.
    pub fn logins(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT login FROM session UNION SELECT login FROM setting UNION SELECT login FROM watch",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn put_hydra_state(
        &self,
        jobset: &str,
        attr: &str,
        build: u64,
        state: crate::builds::State,
        now: DateTime<Utc>,
        change: Option<&crate::builds::Change>,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO hydra_state (jobset, attr, build, state, checked_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(jobset, attr) DO UPDATE SET build = ?3, state = ?4, checked_at = ?5",
            params![jobset, attr, build as i64, state.key(), now.to_rfc3339()],
        )?;
        if let Some(c) = change {
            tx.execute(
                "INSERT OR IGNORE INTO hydra_change (jobset, attr, build, state, at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![c.jobset, c.attr, c.build as i64, c.state.key(), c.at.to_rfc3339()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The sweep asked, and Hydra had no build that tells the state.
    pub fn hydra_checked(&self, jobset: &str, attr: &str, now: DateTime<Utc>) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO hydra_state (jobset, attr, checked_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(jobset, attr) DO UPDATE SET checked_at = ?3",
            params![jobset, attr, now.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn forget_hydra_state(&self, jobset: &str, attr: &str, now: DateTime<Utc>) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO hydra_state (jobset, attr, checked_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(jobset, attr) DO UPDATE SET build = NULL, state = NULL, checked_at = ?3",
            params![jobset, attr, now.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn hydra_states(&self) -> Result<Vec<(String, String, u64, crate::builds::State)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT jobset, attr, build, state FROM hydra_state WHERE build IS NOT NULL AND state IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (jobset, attr, build, state) = row?;
            if let Some(s) = crate::builds::State::parse(&state) {
                out.push((jobset, attr, build as u64, s));
            }
        }
        Ok(out)
    }

    pub fn hydra_checked_at(
        &self,
    ) -> Result<std::collections::HashMap<(String, String), DateTime<Utc>>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT jobset, attr, checked_at FROM hydra_state")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (jobset, attr, at) = row?;
            if let Ok(t) = DateTime::parse_from_rfc3339(&at) {
                out.insert((jobset, attr), t.with_timezone(&Utc));
            }
        }
        Ok(out)
    }

    /// Newest first.
    pub fn hydra_changes(&self, since: DateTime<Utc>) -> Result<Vec<crate::builds::Change>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT jobset, attr, build, state, at FROM hydra_change WHERE at >= ?1 ORDER BY at DESC, build DESC",
        )?;
        let rows = stmt.query_map([since.to_rfc3339()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (jobset, attr, build, state, at) = row?;
            let (Some(state), Ok(at)) = (
                crate::builds::State::parse(&state),
                DateTime::parse_from_rfc3339(&at),
            ) else {
                continue;
            };
            out.push(crate::builds::Change {
                jobset,
                attr,
                build: build as u64,
                state,
                at: at.with_timezone(&Utc),
            });
        }
        Ok(out)
    }

    pub fn purge_hydra(&self, before: DateTime<Utc>) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let b = before.to_rfc3339();
        Ok(
            conn.execute("DELETE FROM hydra_change WHERE at < ?1", [&b])?
                + conn.execute("DELETE FROM hydra_state WHERE checked_at < ?1", [&b])?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_followup_keeps_its_first_time_and_goes_with_its_pull_request() {
        let s = Store::open(std::path::Path::new(":memory:")).unwrap();
        let t0: DateTime<Utc> = "2026-10-17T12:00:00Z".parse().unwrap();
        let t1 = t0 + chrono::Duration::days(2);
        s.set_followup("alice", 1, 7, "merge", t0).unwrap();
        s.set_followup("alice", 1, 3, "ask", t1).unwrap();
        s.set_followup("alice", 2, 5, "none", t1).unwrap();
        s.set_followup("bob", 1, 9, "ask", t1).unwrap();
        let a = s.followups("alice").unwrap();
        assert_eq!(
            a[0],
            FollowUp {
                number: 1,
                days: 3,
                action: "ask".into(),
                since: t0
            }
        );
        assert_eq!(a.len(), 2);
        s.delete_followup("alice", 2).unwrap();
        assert_eq!(s.followups("alice").unwrap().len(), 1);
        assert_eq!(s.purge_followups(&[2]).unwrap(), 2);
        assert!(s.followups("alice").unwrap().is_empty());
        assert!(s.followups("bob").unwrap().is_empty());
    }

    #[test]
    fn an_event_mail_is_marked_once() {
        let s = Store::open(std::path::Path::new(":memory:")).unwrap();
        let now = Utc::now();
        assert!(s.mail_once("alice", "mine-1-merged", now).unwrap());
        assert!(!s.mail_once("alice", "mine-1-merged", now).unwrap());
        assert!(s.mail_once("bob", "mine-1-merged", now).unwrap());
        let later = now + chrono::Duration::days(46);
        assert!(s.mail_once("alice", "mine-1-merged", later).unwrap());
    }

    #[test]
    fn refused_emails_add_no_rows() {
        let s = Store::open(std::path::Path::new(":memory:")).unwrap();
        let now = Utc::now();
        assert!(s.take_mail_slot("alice", "confirm", 10, 1, now).unwrap());
        for _ in 0..50 {
            assert!(!s.take_mail_slot("alice", "confirm", 10, 1, now).unwrap());
        }
        let rows = |table: &str| -> i64 {
            s.conn
                .lock()
                .unwrap()
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!((rows("mail_log"), rows("mail_skip")), (1, 1));
        assert_eq!(
            s.mail_counts(now - chrono::Duration::days(1)).unwrap(),
            (1, 50)
        );
    }

    #[test]
    fn tags_stick_before_the_first_area_refresh() {
        let s = Store::open(std::path::Path::new(":memory:")).unwrap();
        s.set_tags("alice", &["pkgs/a".into()]).unwrap();
        let m = s.members().unwrap();
        assert_eq!(m[0].tags, vec!["pkgs/a"]);
        assert!(Utc::now() - m[0].refreshed_at > chrono::Duration::days(1));
        s.put_member("alice", &["pkgs/b".into()]).unwrap();
        s.set_tags("alice", &["pkgs/c".into()]).unwrap();
        let m = s.members().unwrap();
        assert_eq!(m[0].committed, vec!["pkgs/b"]);
        assert_eq!(m[0].tags, vec!["pkgs/c"]);
    }

    #[test]
    fn the_old_sets_move_to_named_sets() {
        let dir = std::env::temp_dir().join(format!("mm-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("db.sqlite");
        let _ = std::fs::remove_file(&path);
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE package_set (login TEXT PRIMARY KEY, attrs TEXT NOT NULL, dropped INTEGER NOT NULL,
                   uploaded_at TEXT NOT NULL, scanned_at TEXT, nixpkgs_rev TEXT);
                 INSERT INTO package_set VALUES ('alice', '[\"hello\"]', 2, '2026-10-01T00:00:00+00:00', NULL, 'abc');
                 CREATE TABLE flake_ref (id INTEGER PRIMARY KEY, login TEXT NOT NULL, reference TEXT NOT NULL,
                   output TEXT NOT NULL, created_at TEXT NOT NULL, seen_at TEXT NOT NULL, last_at TEXT, error TEXT,
                   attrs TEXT NOT NULL DEFAULT '[]', dropped INTEGER NOT NULL DEFAULT 0, scanned_at TEXT,
                   nixpkgs_rev TEXT, UNIQUE (login, reference, output));
                 INSERT INTO flake_ref (id, login, reference, output, created_at, seen_at, attrs, scanned_at)
                   VALUES (7, 'alice', 'github:o/r', 'x', '2026-10-01T00:00:00+00:00', '2026-10-01T00:00:00+00:00',
                   '[\"jq\"]', '2026-10-02T00:00:00+00:00');",
            )
            .unwrap();
        }
        for _ in 0..2 {
            let s = Store::open(&path).unwrap();
            let sets = s.sets("alice").unwrap();
            let names: Vec<&str> = sets.iter().map(|x| x.name.as_str()).collect();
            assert_eq!(names, [DEFAULT_SET, "flake-7"]);
            let d = sets[0].data.as_ref().unwrap();
            assert_eq!((d.attrs.clone(), d.dropped), (vec!["hello".to_string()], 2));
            assert!(d.versions.is_empty() && !sets[0].public && sets[0].flake.is_none());
            assert!(sets[1].rescan && !sets[1].public);
            assert_eq!(sets[1].data.as_ref().unwrap().attrs, ["jq"]);
            assert_eq!(s.flake_refs("alice").unwrap()[0].reference, "github:o/r");
        }
        let c = Connection::open(&path).unwrap();
        assert!(!has_table(&c, "flake_ref").unwrap() && !has_table(&c, "package_set_v1").unwrap());
        drop(c);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
