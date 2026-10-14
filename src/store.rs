use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

use crate::model::Pr;

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
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS member (
               login TEXT PRIMARY KEY,
               committed TEXT NOT NULL,
               tags TEXT NOT NULL,
               refreshed_at TEXT NOT NULL
             );
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
