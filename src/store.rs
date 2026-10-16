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
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
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
             CREATE TABLE IF NOT EXISTS package_set (
               login TEXT PRIMARY KEY,
               attrs TEXT NOT NULL,
               dropped INTEGER NOT NULL,
               uploaded_at TEXT NOT NULL,
               scanned_at TEXT,
               nixpkgs_rev TEXT
             );
             CREATE TABLE IF NOT EXISTS channel (
               key TEXT PRIMARY KEY,
               data TEXT NOT NULL
             );
             UPDATE watch SET package = 'pkg:' || package WHERE instr(package, ':') = 0;
             CREATE TABLE IF NOT EXISTS session (
               id TEXT PRIMARY KEY,
               login TEXT NOT NULL,
               token TEXT NOT NULL,
               created_at TEXT NOT NULL,
               seen_at TEXT NOT NULL
             );",
        )?;
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

    pub fn set_tags(&self, login: &str, tags: &[String]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE member SET tags = ?2 WHERE login = ?1",
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
    /// spreads its reads across them; see the README on rate limits.
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
}

/// A person's package set from their latest report. Private: only the owner
/// reads it, and no public number counts it.
pub struct PackageSet {
    pub attrs: Vec<String>,
    pub dropped: usize,
    pub uploaded_at: DateTime<Utc>,
    pub scanned_at: Option<String>,
    pub nixpkgs_rev: Option<String>,
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

    pub fn put_package_set(&self, login: &str, set: &PackageSet) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO package_set (login, attrs, dropped, uploaded_at, scanned_at, nixpkgs_rev)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(login) DO UPDATE SET attrs = excluded.attrs, dropped = excluded.dropped,
               uploaded_at = excluded.uploaded_at, scanned_at = excluded.scanned_at,
               nixpkgs_rev = excluded.nixpkgs_rev",
            params![
                login,
                serde_json::to_string(&set.attrs)?,
                set.dropped as i64,
                set.uploaded_at.to_rfc3339(),
                set.scanned_at,
                set.nixpkgs_rev
            ],
        )?;
        Ok(())
    }

    pub fn package_set(&self, login: &str) -> Result<Option<PackageSet>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT attrs, dropped, uploaded_at, scanned_at, nixpkgs_rev FROM package_set WHERE login = ?1",
                [login],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((attrs, dropped, uploaded, scanned_at, nixpkgs_rev)) = row else {
            return Ok(None);
        };
        Ok(Some(PackageSet {
            attrs: serde_json::from_str(&attrs)?,
            dropped: dropped.max(0) as usize,
            uploaded_at: DateTime::parse_from_rfc3339(&uploaded)?.with_timezone(&Utc),
            scanned_at,
            nixpkgs_rev,
        }))
    }

    pub fn delete_package_set(&self, login: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM package_set WHERE login = ?1", [login])?;
        Ok(())
    }

    /// Deletes every set with no upload since `before`; returns how many.
    pub fn purge_package_sets(&self, before: DateTime<Utc>) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM package_set WHERE uploaded_at < ?1",
            [before.to_rfc3339()],
        )?)
    }
}
