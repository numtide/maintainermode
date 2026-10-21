//! A full copy of the issues and pull requests of GitHub repositories, as
//! the raw GraphQL JSON, so that nothing depends on GitHub to read them.
//! One SQLite file per repository: `archive/<owner>/<name>.sqlite`.

use anyhow::{Context, Result, bail};
use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use crate::github;
use crate::store::Store;
use crate::web::App;

pub const DEFAULT_REPO: &str = "NixOS/nixpkgs";
pub const PASS_EVERY: u64 = 300;
/// The walk stops for the pass when the token has less left, so the
/// pull request and issue sync keeps its budget.
pub(crate) const RESERVE: i64 = 1000;
/// On the first pass, "recent" reaches this far back.
const RECENT_DAYS: i64 = 30;
const LIST_PAGES: usize = 20;
/// An event makes its item due after this delay, so a burst of events on
/// one item costs one fetch.
const HINT_DELAY: i64 = 5;
const HINT_BATCH: usize = 5000;
/// Items per query, then the first page of comments, timeline items and
/// files, then the first page of reviews and of the inline comments of each.
/// GitHub answers 502 after 10 s, so a failed batch is asked again smaller.
const LEVELS: [(usize, i64, i64); 4] = [(10, 100, 30), (5, 100, 30), (1, 50, 20), (1, 10, 10)];
const CALM: usize = 20;
const EXPORT_CHUNK: u64 = 10_000;

const SCHEMA: &str = "PRAGMA journal_mode = WAL;
     PRAGMA busy_timeout = 5000;
     CREATE TABLE IF NOT EXISTS item (
       number INTEGER PRIMARY KEY,
       kind TEXT NOT NULL,
       updated_at TEXT NOT NULL,
       fetched_at TEXT NOT NULL,
       size INTEGER NOT NULL,
       data BLOB
     );
     CREATE INDEX IF NOT EXISTS item_fetched_at ON item (fetched_at);
     CREATE TABLE IF NOT EXISTS due (number INTEGER PRIMARY KEY, at TEXT NOT NULL);
     CREATE INDEX IF NOT EXISTS due_at ON due (at);
     CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
     CREATE TABLE IF NOT EXISTS failed (number INTEGER PRIMARY KEY, at TEXT NOT NULL, error TEXT NOT NULL);";

/// `kind` of a number that GitHub resolves to no issue or pull request: a
/// deleted or transferred issue, or a discussion.
const NONE: &str = "none";

const ACTOR: &str = "author { __typename login }";

const COMMENT: &str = "id databaseId url createdAt updatedAt lastEditedAt body authorAssociation
  isMinimized minimizedReason author { __typename login }";

const REVIEW_COMMENT: &str = "id databaseId url createdAt updatedAt lastEditedAt body
  path line originalLine startLine originalStartLine diffHunk outdated subjectType
  authorAssociation isMinimized minimizedReason author { __typename login }
  replyTo { databaseId } commit { oid } originalCommit { oid }";

const FILE: &str = "path additions deletions changeType";

const SUBJECT: &str = "... on Issue { number repository { nameWithOwner } }
  ... on PullRequest { number repository { nameWithOwner } }";

const ISSUE_ITEMS: &str = "ASSIGNED_EVENT, UNASSIGNED_EVENT, LABELED_EVENT, UNLABELED_EVENT,
  CLOSED_EVENT, REOPENED_EVENT, CROSS_REFERENCED_EVENT, REFERENCED_EVENT, RENAMED_TITLE_EVENT,
  MILESTONED_EVENT, MARKED_AS_DUPLICATE_EVENT, TRANSFERRED_EVENT";

const PR_ITEMS: &str = "PULL_REQUEST_COMMIT, HEAD_REF_FORCE_PUSHED_EVENT, REVIEW_REQUESTED_EVENT,
  REVIEW_REQUEST_REMOVED_EVENT, REVIEW_DISMISSED_EVENT, MERGED_EVENT, READY_FOR_REVIEW_EVENT,
  CONVERT_TO_DRAFT_EVENT, BASE_REF_CHANGED_EVENT";

fn timeline(pr: bool) -> String {
    let mut s = format!(
        "__typename
         ... on AssignedEvent {{ createdAt actor {{ login }} assignee {{ ... on Actor {{ login }} }} }}
         ... on UnassignedEvent {{ createdAt actor {{ login }} assignee {{ ... on Actor {{ login }} }} }}
         ... on LabeledEvent {{ createdAt actor {{ login }} label {{ name }} }}
         ... on UnlabeledEvent {{ createdAt actor {{ login }} label {{ name }} }}
         ... on ClosedEvent {{ createdAt actor {{ login }} stateReason }}
         ... on ReopenedEvent {{ createdAt actor {{ login }} stateReason }}
         ... on CrossReferencedEvent {{ createdAt actor {{ login }} willCloseTarget source {{ {SUBJECT} }} }}
         ... on ReferencedEvent {{ createdAt actor {{ login }} commit {{ oid }} commitRepository {{ nameWithOwner }} }}
         ... on RenamedTitleEvent {{ createdAt actor {{ login }} previousTitle currentTitle }}
         ... on MilestonedEvent {{ createdAt actor {{ login }} milestoneTitle }}
         ... on MarkedAsDuplicateEvent {{ createdAt actor {{ login }} }}
         ... on TransferredEvent {{ createdAt actor {{ login }} fromRepository {{ nameWithOwner }} }}"
    );
    if pr {
        s.push_str(
            "
         ... on PullRequestCommit { pullRequestCommit: commit { oid authoredDate committedDate message author { name user { login } } } }
         ... on HeadRefForcePushedEvent { createdAt actor { login } beforeCommit { oid } afterCommit { oid } }
         ... on ReviewRequestedEvent { createdAt actor { login } requestedReviewer { ... on Actor { login } ... on Team { combinedSlug } } }
         ... on ReviewRequestRemovedEvent { createdAt actor { login } requestedReviewer { ... on Actor { login } ... on Team { combinedSlug } } }
         ... on ReviewDismissedEvent { createdAt actor { login } dismissalMessage previousReviewState review { databaseId } }
         ... on MergedEvent { createdAt actor { login } commit { oid } mergeRefName }
         ... on ReadyForReviewEvent { createdAt actor { login } }
         ... on ConvertToDraftEvent { createdAt actor { login } }
         ... on BaseRefChangedEvent { createdAt actor { login } previousRefName currentRefName }",
        );
    }
    s
}

fn review(comments: i64) -> String {
    format!(
        "id databaseId url state submittedAt createdAt updatedAt lastEditedAt body authorAssociation
         isMinimized minimizedReason author {{ __typename login }} commit {{ oid }}
         comments(first: {comments}) {{ totalCount pageInfo {{ hasNextPage endCursor }} nodes {{ {REVIEW_COMMENT} }} }}"
    )
}

fn connection(field: &str, n: i64, args: &str, nodes: &str) -> String {
    format!(
        "{field}(first: {n}{args}) {{ totalCount pageInfo {{ hasNextPage endCursor }} nodes {{ {nodes} }} }}"
    )
}

/// The fields both kinds share. `state` has a different type in each, so
/// the pull request asks for it under an alias.
fn common(n: i64) -> String {
    format!(
        "id databaseId number title url createdAt updatedAt closedAt lastEditedAt
         locked activeLockReason body authorAssociation {ACTOR}
         milestone {{ number title }}
         assignees(first: 20) {{ nodes {{ login }} }}
         labels(first: 100) {{ nodes {{ name }} }}
         {}",
        connection("comments", n, "", COMMENT)
    )
}

fn fragments(n: i64, r: i64) -> String {
    format!(
        "fragment I on Issue {{
           {} state stateReason
           {}
         }}
         fragment P on PullRequest {{
           {} pullRequestState: state isDraft merged mergedAt mergedBy {{ login }} mergeCommit {{ oid }}
           baseRefName baseRefOid headRefName headRefOid headRepository {{ nameWithOwner }}
           additions deletions changedFiles reviewDecision
           {}
           {}
           {}
         }}",
        common(n),
        connection(
            "timelineItems",
            n,
            &format!(", itemTypes: [{ISSUE_ITEMS}]"),
            &timeline(false)
        ),
        common(n),
        connection(
            "timelineItems",
            n,
            &format!(", itemTypes: [{ISSUE_ITEMS}, {PR_ITEMS}]"),
            &timeline(true)
        ),
        connection("files", n, "", FILE),
        connection("reviews", r, "", &review(r)),
    )
}

fn batch_query(numbers: &[u64], n: i64, r: i64) -> String {
    let fields: String = numbers
        .iter()
        .map(|k| format!("n{k}: issueOrPullRequest(number: {k}) {{ __typename ...I ...P }}\n"))
        .collect();
    format!(
        "query($owner: String!, $name: String!) {{
           rateLimit {{ cost remaining resetAt }}
           repository(owner: $owner, name: $name) {{ {fields} }}
         }}
         {}",
        fragments(n, r)
    )
}

/// A field asked under two types must have one shape, so the queries alias
/// it on one side; the stored JSON uses GitHub's own name again.
const ALIASES: [(&str, &str); 2] = [
    ("pullRequestState", "state"),
    ("pullRequestCommit", "commit"),
];

fn unalias(v: &mut Value) {
    match v {
        Value::Object(o) => {
            for (alias, name) in ALIASES {
                if let Some(x) = o.remove(alias) {
                    o.insert(name.to_string(), x);
                }
            }
            o.values_mut().for_each(unalias);
        }
        Value::Array(a) => a.iter_mut().for_each(unalias),
        _ => {}
    }
}

/// A nested connection that can have more than its first page.
struct More {
    on: &'static str,
    field: &'static str,
    args: String,
    nodes: String,
}

impl More {
    fn query(&self) -> String {
        format!(
            "query($id: ID!, $after: String) {{
               rateLimit {{ cost remaining resetAt }}
               node(id: $id) {{ ... on {} {{ {}(first: 100, after: $after{}) {{
                 pageInfo {{ hasNextPage endCursor }} nodes {{ {} }}
               }} }} }}
             }}",
            self.on, self.field, self.args, self.nodes
        )
    }
}

fn more_of(typename: &str) -> Vec<More> {
    let pr = typename == "PullRequest";
    let items = if pr {
        format!(", itemTypes: [{ISSUE_ITEMS}, {PR_ITEMS}]")
    } else {
        format!(", itemTypes: [{ISSUE_ITEMS}]")
    };
    let on = if pr { "PullRequest" } else { "Issue" };
    let mut out = vec![
        More {
            on,
            field: "comments",
            args: String::new(),
            nodes: COMMENT.into(),
        },
        More {
            on,
            field: "timelineItems",
            args: items,
            nodes: timeline(pr),
        },
    ];
    if pr {
        out.push(More {
            on,
            field: "files",
            args: String::new(),
            nodes: FILE.into(),
        });
        out.push(More {
            on,
            field: "reviews",
            args: String::new(),
            nodes: review(30),
        });
    }
    out
}

fn review_comments() -> More {
    More {
        on: "PullRequestReview",
        field: "comments",
        args: String::new(),
        nodes: REVIEW_COMMENT.into(),
    }
}

/// `owner/name`, with the characters GitHub allows.
pub fn parse_repo(s: &str) -> Result<(String, String)> {
    let ok = |p: &str| {
        !p.is_empty()
            && !p.starts_with('.')
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match s.trim().split_once('/') {
        Some((o, n)) if ok(o) && ok(n) => Ok((o.to_string(), n.to_string())),
        _ => bail!("not a repository: {s:?}; write it as owner/name"),
    }
}

pub fn path_of(dir: &Path, repo: &str) -> Result<PathBuf> {
    let (owner, name) = parse_repo(repo)?;
    Ok(dir.join(owner).join(format!("{name}.sqlite")))
}

pub struct Archive {
    pub repo: String,
    owner: String,
    name: String,
    /// Items fetched per pass.
    pub budget: usize,
    conn: Mutex<Connection>,
}

pub struct Item {
    pub kind: String,
    pub json: Option<String>,
}

impl Archive {
    pub fn open(dir: &Path, repo: &str, budget: usize) -> Result<Self> {
        let (owner, name) = parse_repo(repo)?;
        let path = path_of(dir, repo)?;
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).with_context(|| format!("create {}", p.display()))?;
        }
        let conn = Connection::open(&path).with_context(|| format!("open {}", path.display()))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            repo: format!("{owner}/{name}"),
            owner,
            name,
            budget,
            conn: Mutex::new(conn),
        })
    }

    /// For the export, next to a running server. Not SQLITE_OPEN_READ_ONLY:
    /// a read-only connection cannot open a WAL file after the last writer
    /// removed the `-wal` file.
    pub fn open_existing(dir: &Path, repo: &str) -> Result<Self> {
        let (owner, name) = parse_repo(repo)?;
        let path = path_of(dir, repo)?;
        if !path.exists() {
            bail!("no archive of {owner}/{name} at {}", path.display());
        }
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("open {}", path.display()))?;
        conn.busy_timeout(StdDuration::from_secs(5))?;
        Ok(Self {
            repo: format!("{owner}/{name}"),
            owner,
            name,
            budget: 0,
            conn: Mutex::new(conn),
        })
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

    /// True also when the file cannot say, so a read error hides the items.
    pub fn is_private(&self) -> bool {
        self.get("private").map_or(true, |p| p.is_some())
    }

    fn delete_key(&self, key: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM meta WHERE key = ?1", [key])?;
        Ok(())
    }

    fn num(&self, key: &str) -> Result<Option<i64>> {
        Ok(self.get(key)?.and_then(|v| v.parse().ok()))
    }

    fn time(&self, key: &str) -> Result<Option<DateTime<Utc>>> {
        Ok(self
            .get(key)?
            .and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
            .map(|t| t.with_timezone(&Utc)))
    }

    /// Stores one item, or marks the number as `NONE` when `json` is null.
    pub fn put(&self, number: u64, json: &Value, now: DateTime<Utc>) -> Result<()> {
        let (kind, updated_at, data) = match json["__typename"].as_str() {
            Some("Issue") => ("issue", json["updatedAt"].as_str().unwrap_or(""), true),
            Some("PullRequest") => ("pull", json["updatedAt"].as_str().unwrap_or(""), true),
            _ => (NONE, "", false),
        };
        let blob = if data {
            Some(zstd::encode_all(serde_json::to_vec(json)?.as_slice(), 3)?)
        } else {
            None
        };
        let size = blob.as_ref().map_or(0, Vec::len) as i64;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let old: Option<(String, i64)> = tx
            .query_row(
                "SELECT kind, size FROM item WHERE number = ?1",
                [number as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        tx.execute(
            "INSERT OR REPLACE INTO item (number, kind, updated_at, fetched_at, size, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                number as i64,
                kind,
                updated_at,
                now.to_rfc3339(),
                size,
                blob
            ],
        )?;
        let bump = |key: &str, by: i64| {
            tx.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + ?2",
                params![key, by],
            )
        };
        let counter = |k: &str| if k == NONE { "none" } else { "items" };
        if let Some((k, s)) = &old {
            bump(counter(k), -1)?;
            bump("bytes", -s)?;
        }
        bump(counter(kind), 1)?;
        bump("bytes", size)?;
        tx.execute(
            "DELETE FROM due WHERE number = ?1 AND at <= ?2",
            params![number as i64, now.to_rfc3339()],
        )?;
        tx.execute("DELETE FROM failed WHERE number = ?1", [number as i64])?;
        tx.commit()?;
        Ok(())
    }

    pub fn item(&self, number: u64) -> Result<Option<Item>> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(String, Option<Vec<u8>>)> = conn
            .query_row(
                "SELECT kind, data FROM item WHERE number = ?1",
                [number as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((kind, data)) = row else {
            return Ok(None);
        };
        let json = data
            .map(|z| -> Result<String> { Ok(String::from_utf8(zstd::decode_all(z.as_slice())?)?) })
            .transpose()?;
        Ok(Some(Item { kind, json }))
    }

    fn updated_at(&self, number: u64) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT updated_at FROM item WHERE number = ?1",
                [number as i64],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Marks a number to fetch at `at`, or earlier if it is due earlier.
    pub fn due(&self, number: u64, at: DateTime<Utc>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO due (number, at) VALUES (?1, ?2)
             ON CONFLICT(number) DO UPDATE SET at = min(at, excluded.at)",
            params![number as i64, at.to_rfc3339()],
        )?;
        Ok(())
    }

    /// Moves a number to `at`, also when it was due earlier.
    fn put_off(&self, number: u64, at: DateTime<Utc>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO due (number, at) VALUES (?1, ?2)",
            params![number as i64, at.to_rfc3339()],
        )?;
        Ok(())
    }

    fn fail(&self, number: u64, error: &str, now: DateTime<Utc>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO failed (number, at, error) VALUES (?1, ?2, ?3)",
            params![number as i64, now.to_rfc3339(), error],
        )?;
        Ok(())
    }

    /// Whether every number from 1 to `top` has a row.
    fn stored_up_to(&self, top: u64) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM item WHERE number BETWEEN 1 AND ?1",
            [top as i64],
            |r| r.get(0),
        )?;
        Ok(n as u64 >= top)
    }

    fn due_now(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<u64>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT number FROM due WHERE at <= ?1 ORDER BY at LIMIT ?2")?;
        let rows = stmt.query_map(params![now.to_rfc3339(), limit as i64], |r| {
            r.get::<_, i64>(0)
        })?;
        Ok(rows
            .map(|r| r.map(|n| n as u64))
            .collect::<Result<_, _>>()?)
    }

    /// Up to `limit` numbers below `cursor` that are not stored, highest
    /// first.
    fn missing_below(&self, cursor: u64, limit: usize) -> Result<Vec<u64>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT 1 FROM item WHERE number = ?1")?;
        let mut out = Vec::new();
        let mut n = cursor;
        while n > 1 && out.len() < limit {
            n -= 1;
            if !stmt.exists([n as i64])? {
                out.push(n);
            }
        }
        Ok(out)
    }

    fn count(&self, sql: &str, arg: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row(sql, [arg], |r| r.get(0))?)
    }

    fn left(&self) -> Result<Option<i64>> {
        let walked = self.num("items")?.unwrap_or(0) + self.num("none")?.unwrap_or(0);
        Ok(self.num("top")?.map(|t| (t - walked).max(0)))
    }

    fn sample(&self, key: &str) -> Result<Option<(DateTime<Utc>, i64)>> {
        Ok(self.get(key)?.and_then(|v| {
            let (t, l) = v.split_once(' ')?;
            let t = DateTime::parse_from_rfc3339(t).ok()?.with_timezone(&Utc);
            Some((t, l.parse().ok()?))
        }))
    }

    /// The numbers left at a time 6 to 12 hours ago, or since the first
    /// sample. Refetches do not change the numbers left, so they do not
    /// count as pace.
    fn pace_base(&self) -> Result<Option<(DateTime<Utc>, i64)>> {
        Ok(self.sample("pace_prev")?.or(self.sample("pace_mark")?))
    }

    pub fn mark_pace(&self, now: DateTime<Utc>) -> Result<()> {
        let Some(left) = self.left()? else {
            return Ok(());
        };
        if self
            .sample("pace_mark")?
            .is_none_or(|(t, _)| now - t >= Duration::hours(6))
        {
            if let Some(m) = self.get("pace_mark")? {
                self.set("pace_prev", &m)?;
            }
            self.set("pace_mark", &format!("{} {left}", now.to_rfc3339()))?;
        }
        Ok(())
    }

    pub fn progress(&self, now: DateTime<Utc>) -> Result<Value> {
        let items = self.num("items")?.unwrap_or(0);
        let none = self.num("none")?.unwrap_or(0);
        let top = self.num("top")?;
        let left = self.left()?;
        let per_hour = match (left, self.pace_base()?) {
            (Some(l), Some((t, then))) if now - t > Duration::minutes(10) => {
                Some((then - l) as f64 * 3600.0 / (now - t).num_seconds() as f64)
            }
            _ => None,
        };
        let done = self.get("backfill_done")?.is_some();
        Ok(json!({
            "repo": self.repo,
            "items": items,
            "none": none,
            "bytes": self.num("bytes")?.unwrap_or(0),
            "top": top,
            "oldest": self.num("cursor")?,
            "complete": done,
            "percent": top
                .filter(|t| *t > 0)
                .map(|t| ((items + none) as f64 * 100.0 / t as f64).min(100.0)),
            "due": self.count("SELECT COUNT(*) FROM due WHERE at <= ?1", &now.to_rfc3339())?,
            "failed": self.count("SELECT COUNT(*) FROM failed WHERE at <= ?1", &now.to_rfc3339())?,
            "per_hour": per_hour,
            "hours_left": match (left, per_hour) {
                (Some(_), _) if done => Some(0.0),
                (Some(l), Some(p)) if p > 0.0 => Some(l as f64 / p),
                _ => None,
            },
            "items_per_pass": self.budget,
            "last_pass": self.get("pass_ended")?,
            "last_error": self.get("last_error")?.filter(|e| !e.is_empty()),
        }))
    }

    /// Writes every stored item, as one JSON line, into one zstd file per
    /// 10 000 numbers. Returns the items and the files written.
    pub fn export(&self, out: &Path) -> Result<(usize, usize)> {
        let dir = out.join(&self.owner).join(&self.name);
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT number, data FROM item WHERE data IS NOT NULL ORDER BY number")?;
        let mut rows = stmt.query([])?;
        let (mut items, mut files) = (0, 0);
        let mut current: Option<(u64, zstd::Encoder<'static, std::fs::File>)> = None;
        let finish = |chunk: u64, enc: zstd::Encoder<'static, std::fs::File>| -> Result<()> {
            enc.finish()?.sync_all()?;
            let name = format!("{:07}.ndjson.zst", chunk * EXPORT_CHUNK);
            std::fs::rename(dir.join(format!(".{name}")), dir.join(&name))?;
            Ok(())
        };
        while let Some(row) = rows.next()? {
            let number = row.get::<_, i64>(0)? as u64;
            let z: Vec<u8> = row.get(1)?;
            let chunk = number / EXPORT_CHUNK;
            if current.as_ref().is_some_and(|(c, _)| *c != chunk) {
                let (c, enc) = current.take().unwrap();
                finish(c, enc)?;
                files += 1;
            }
            if current.is_none() {
                let name = format!(".{:07}.ndjson.zst", chunk * EXPORT_CHUNK);
                let f = std::fs::File::create(dir.join(&name))?;
                current = Some((chunk, zstd::Encoder::new(f, 10)?));
            }
            let enc = &mut current.as_mut().unwrap().1;
            enc.write_all(&zstd::decode_all(z.as_slice())?)?;
            enc.write_all(b"\n")?;
            items += 1;
        }
        if let Some((c, enc)) = current {
            finish(c, enc)?;
            files += 1;
        }
        Ok((items, files))
    }
}

pub fn open_all(dir: &Path, repos: &[String], budget: usize) -> Result<Vec<Arc<Archive>>> {
    let mut out: Vec<Arc<Archive>> = Vec::new();
    for r in repos.iter().filter(|r| !r.trim().is_empty()) {
        let a = Archive::open(dir, r, budget)?;
        if !out.iter().any(|o| o.repo.eq_ignore_ascii_case(&a.repo)) {
            out.push(Arc::new(a));
        }
    }
    Ok(out)
}

/// A connect error or a timeout ends the pass: the network is down, and
/// the items are not at fault.
fn transient(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<reqwest::Error>()
            .is_some_and(|r| r.is_connect() || r.is_timeout())
    })
}

struct Pass<'a> {
    store: &'a Store,
    a: &'a Archive,
    client: &'a mut github::Client,
    remaining: i64,
}

impl Pass<'_> {
    async fn query(&mut self, query: &str, vars: Value, allowed: &[&str]) -> Result<Value> {
        let data = self
            .client
            .graphql_allowing(self.store, query, vars, allowed)
            .await?;
        if let Some(r) = data["rateLimit"]["remaining"].as_i64() {
            self.remaining = r;
        }
        Ok(data)
    }

    fn low(&self) -> bool {
        self.remaining < RESERVE
    }

    /// Reads the numbers of one kind updated since the last listing,
    /// newest first, and marks the changed ones due. Resumes from its
    /// cursor after `LIST_PAGES`.
    async fn list(&mut self, field: &str, now: DateTime<Utc>) -> Result<usize> {
        let a = self.a;
        let (cursor_key, started_key, mark_key) = (
            format!("list_cursor_{field}"),
            format!("list_started_{field}"),
            format!("watermark_{field}"),
        );
        let mut cursor = a.get(&cursor_key)?;
        let started = match a.time(&started_key)? {
            Some(t) if cursor.is_some() => t,
            _ => {
                a.set(&started_key, &now.to_rfc3339())?;
                now
            }
        };
        let floor = a
            .time(&mark_key)?
            .unwrap_or(now - Duration::days(RECENT_DAYS));
        let query = format!(
            "query($owner: String!, $name: String!, $after: String) {{
               rateLimit {{ cost remaining resetAt }}
               repository(owner: $owner, name: $name) {{
                 isPrivate
                 {field}(first: 100, after: $after, orderBy: {{ field: UPDATED_AT, direction: DESC }}) {{
                   pageInfo {{ hasNextPage endCursor }}
                   nodes {{ number updatedAt }}
                 }}
               }}
             }}"
        );
        let mut marked = 0;
        for _ in 0..LIST_PAGES {
            if self.low() {
                return Ok(marked);
            }
            let vars = json!({ "owner": a.owner, "name": a.name, "after": cursor });
            let data = self.query(&query, vars, &[]).await?;
            if data["repository"]["isPrivate"].as_bool() != Some(false) {
                a.set("private", "1")?;
                bail!(
                    "{} is private or unknown; the archive keeps public repositories only",
                    a.repo
                );
            }
            a.delete_key("private")?;
            let conn = &data["repository"][field];
            let mut reached = false;
            let mut top = a.num("top")?.unwrap_or(0);
            for n in conn["nodes"].as_array().into_iter().flatten() {
                let (Some(number), Some(at)) = (n["number"].as_u64(), n["updatedAt"].as_str())
                else {
                    continue;
                };
                top = top.max(number as i64);
                let t = DateTime::parse_from_rfc3339(at)?.with_timezone(&Utc);
                if t < floor {
                    reached = true;
                    continue;
                }
                if a.updated_at(number)?.as_deref() != Some(at) {
                    a.due(number, now)?;
                    marked += 1;
                }
            }
            a.set("top", &top.to_string())?;
            let next = conn["pageInfo"]["hasNextPage"]
                .as_bool()
                .unwrap_or(false)
                .then(|| conn["pageInfo"]["endCursor"].as_str().map(String::from))
                .flatten();
            match next {
                Some(c) if !reached => {
                    a.set(&cursor_key, &c)?;
                    cursor = Some(c);
                }
                _ => {
                    a.set(&mark_key, &(started - Duration::minutes(2)).to_rfc3339())?;
                    a.delete_key(&cursor_key)?;
                    return Ok(marked);
                }
            }
        }
        Ok(marked)
    }

    /// Follows `hasNextPage` on one connection of the node `id`.
    async fn rest(&mut self, id: &str, more: &More, conn: &mut Value) -> Result<()> {
        let query = more.query();
        while conn["pageInfo"]["hasNextPage"].as_bool() == Some(true) {
            let after = conn["pageInfo"]["endCursor"].clone();
            let data = self
                .query(&query, json!({ "id": id, "after": after }), &[])
                .await?;
            let page = &data["node"][more.field];
            let Some(nodes) = page["nodes"].as_array() else {
                bail!("{} of {id}: no page", more.field);
            };
            if let Some(list) = conn["nodes"].as_array_mut() {
                list.extend(nodes.iter().cloned());
            }
            conn["pageInfo"] = page["pageInfo"].clone();
        }
        Ok(())
    }

    async fn complete(&mut self, item: &mut Value) -> Result<()> {
        let id = item["id"].as_str().unwrap_or("").to_string();
        let typename = item["__typename"].as_str().unwrap_or("").to_string();
        for more in more_of(&typename) {
            let mut conn = item[more.field].take();
            self.rest(&id, &more, &mut conn).await?;
            item[more.field] = conn;
        }
        let rc = review_comments();
        if let Some(reviews) = item
            .get_mut("reviews")
            .and_then(|r| r.get_mut("nodes"))
            .and_then(Value::as_array_mut)
        {
            for r in reviews {
                let rid = r["id"].as_str().unwrap_or("").to_string();
                let mut conn = r["comments"].take();
                self.rest(&rid, &rc, &mut conn).await?;
                r["comments"] = conn;
            }
        }
        Ok(())
    }

    async fn batch(&mut self, numbers: &[u64], n: i64, r: i64) -> Result<()> {
        let a = self.a;
        let vars = json!({ "owner": a.owner, "name": a.name });
        let mut data = self
            .query(&batch_query(numbers, n, r), vars, &["NOT_FOUND"])
            .await?;
        let Some(repo) = data.get_mut("repository").filter(|r| r.is_object()) else {
            bail!("GitHub does not know {}", a.repo);
        };
        let mut found = Vec::new();
        for k in numbers {
            found.push((*k, repo[format!("n{k}")].take()));
        }
        let mut items = Vec::new();
        for (k, mut item) in found {
            if !item.is_null() {
                self.complete(&mut item).await?;
                unalias(&mut item);
            }
            items.push((k, item));
        }
        let now = Utc::now();
        for (k, item) in &items {
            a.put(*k, item, now)?;
        }
        Ok(())
    }

    /// Fetches `numbers` in order. `done` counts the ones handled, stored or
    /// put off, so the caller can move its cursor even after an error.
    async fn fetch(&mut self, numbers: &[u64], done: &mut usize) -> Result<()> {
        let (mut level, mut calm) = (0, 0);
        while *done < numbers.len() && !self.low() {
            let (size, n, r) = LEVELS[level];
            let chunk = &numbers[*done..(*done + size).min(numbers.len())];
            match self.batch(chunk, n, r).await {
                Ok(()) => {
                    *done += chunk.len();
                    calm += 1;
                    if level > 0 && calm >= CALM {
                        level -= 1;
                        calm = 0;
                    }
                }
                Err(e) if transient(&e) || !self.client.has_token() => return Err(e),
                Err(e) if level + 1 < LEVELS.len() => {
                    tracing::warn!("archive {} batch of {size}: {e:#}", self.a.repo);
                    level += 1;
                    calm = 0;
                    tokio::time::sleep(StdDuration::from_secs(1)).await;
                }
                Err(e) => {
                    tracing::warn!(
                        "archive {} #{}: {e:#}; next try in a day",
                        self.a.repo,
                        chunk[0]
                    );
                    let error = format!("{e:#}");
                    for k in chunk {
                        self.a.fail(*k, &error, Utc::now())?;
                        self.a.put_off(*k, Utc::now() + Duration::days(1))?;
                    }
                    *done += chunk.len();
                }
            }
        }
        Ok(())
    }
}

/// Marks the items that got an event as due soon. The event log covers
/// nixpkgs only.
fn hints(log: &crate::events::Log, a: &Archive, now: DateTime<Utc>) -> Result<usize> {
    let head = log.head();
    let Some(after) = a.num("event_seq")? else {
        a.set("event_seq", &head.to_string())?;
        return Ok(0);
    };
    let rows = log.read(after as u64, head, &Default::default(), HINT_BATCH)?;
    let mut n = 0;
    for r in &rows {
        let v: Value = serde_json::from_str(&r.json)?;
        let p = &v["payload"];
        if let Some(number) = p["number"]
            .as_u64()
            .or_else(|| p["pull_request"]["number"].as_u64())
            .or_else(|| p["issue"]["number"].as_u64())
        {
            a.due(number, now + Duration::minutes(HINT_DELAY))?;
            n += 1;
        }
    }
    if let Some(last) = rows.last() {
        a.set("event_seq", &last.seq.to_string())?;
    }
    Ok(n)
}

/// One pass over one repository: the event hints, the listing of recent
/// updates, the due items, then the walk back through history.
async fn pass(
    store: &Store,
    events: &crate::events::Log,
    a: &Archive,
    client: &mut github::Client,
) -> Result<usize> {
    let now = Utc::now();
    if a.repo.eq_ignore_ascii_case(crate::events::REPO) {
        hints(events, a, now)?;
    }
    let mut p = Pass {
        store,
        a,
        client,
        remaining: i64::MAX,
    };
    for field in ["pullRequests", "issues"] {
        p.list(field, now).await?;
    }
    let due = a.due_now(Utc::now(), a.budget)?;
    let mut done = 0;
    p.fetch(&due, &mut done).await?;
    let mut fetched = done;
    let listed = a.get("watermark_pullRequests")?.is_some() && a.get("watermark_issues")?.is_some();
    let left = a.budget.saturating_sub(due.len());
    if listed && left > 0 && a.get("backfill_done")?.is_none() && !p.low() {
        let Some(top) = a.num("top")? else {
            return Ok(fetched);
        };
        let cursor = a.num("cursor")?.unwrap_or(top + 1).max(1) as u64;
        let back = a.missing_below(cursor, left)?;
        let mut done = 0;
        let r = p.fetch(&back, &mut done).await;
        fetched += done;
        let reached = match done {
            0 if back.is_empty() => 1,
            0 => cursor,
            d => back[d - 1],
        };
        a.set("cursor", &reached.to_string())?;
        if reached <= 1 && a.stored_up_to(top as u64)? {
            a.set("backfill_done", &Utc::now().to_rfc3339())?;
        }
        r?;
    }
    if p.low() {
        tracing::info!(
            "archive {}: the token is low on budget, the walk waits for the next pass",
            a.repo
        );
    }
    Ok(fetched)
}

const NO_TOKEN: &str =
    "no GitHub App token and no bot token; the walk does not use the tokens that people lend";

/// The server's own tokens only: a token that a person lent pays for
/// their own reviews, not for the archive.
pub(crate) async fn tokens(app: &App) -> Vec<String> {
    let mut tokens = Vec::new();
    if let Some(g) = &app.github_app {
        if let Ok(t) = g.token().await {
            tokens.push(t);
        }
    }
    tokens.extend(app.bot_token.iter().map(|s| s.get()));
    tokens
}

/// The background walk, in its own task so it never delays the sync.
pub async fn walk(app: Arc<App>) {
    let mut said = false;
    loop {
        let round = std::time::Instant::now();
        let mut client = github::Client::new(tokens(&app).await);
        if !client.has_token() {
            if !said {
                tracing::info!("archive: {NO_TOKEN}");
                said = true;
            }
            for a in &app.archive {
                let _ = a.set("last_error", NO_TOKEN);
            }
        } else {
            for a in &app.archive {
                let started = Utc::now();
                match pass(&app.store, &app.events, a, &mut client).await {
                    Ok(n) => {
                        if n > 0 {
                            tracing::info!("archive {}: {n} items", a.repo);
                        }
                        let _ = a.set("last_error", "");
                    }
                    Err(e) => {
                        tracing::warn!("archive {}: {e:#}", a.repo);
                        let _ = a.set("last_error", &format!("{e:#}"));
                    }
                }
                let _ = a.set("pass_ended", &Utc::now().to_rfc3339());
                let _ = a.mark_pace(Utc::now());
                let _ = a.set(
                    "pass_seconds",
                    &(Utc::now() - started).num_seconds().to_string(),
                );
            }
        }
        let wait = StdDuration::from_secs(PASS_EVERY).saturating_sub(round.elapsed());
        tokio::time::sleep(wait.max(StdDuration::from_secs(30))).await;
    }
}

const NOT_ARCHIVED: &str = "this server does not archive that repository";

/// The stored JSON of one item of a public archived repository.
pub(crate) fn item_json(
    app: &App,
    owner: &str,
    repo: &str,
    number: u64,
) -> Result<String, (StatusCode, &'static str)> {
    let name = format!("{owner}/{repo}");
    let a = app
        .archive
        .iter()
        .find(|a| a.repo.eq_ignore_ascii_case(&name) && !a.is_private())
        .ok_or((StatusCode::NOT_FOUND, NOT_ARCHIVED))?;
    match a.item(number) {
        Ok(Some(Item {
            json: Some(json), ..
        })) => Ok(json),
        Ok(Some(_)) => Err((
            StatusCode::NOT_FOUND,
            "GitHub has no issue or pull request with this number",
        )),
        Ok(None) => Err((StatusCode::NOT_FOUND, "not in the archive yet")),
        Err(e) => {
            tracing::warn!("archive {name}#{number}: {e:#}");
            Err((StatusCode::INTERNAL_SERVER_ERROR, "archive read failed"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::{Json, Router};
    use std::sync::atomic::{AtomicBool, Ordering};

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mm-archive-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn repo_names() {
        assert_eq!(
            parse_repo("NixOS/nixpkgs").unwrap(),
            ("NixOS".into(), "nixpkgs".into())
        );
        assert!(parse_repo("NixOS").is_err());
        assert!(parse_repo("../etc").is_err());
        assert!(parse_repo("NixOS/..").is_err());
        assert!(parse_repo("a/b/c").is_err());
    }

    #[test]
    fn put_counts_and_reads_back() {
        let dir = tmp("put");
        let a = Archive::open(&dir, "NixOS/nixpkgs", 10).unwrap();
        assert!(dir.join("NixOS/nixpkgs.sqlite").exists());
        let now = Utc::now();
        let pr = json!({ "__typename": "PullRequest", "number": 7, "updatedAt": "2026-10-16T00:00:00Z", "body": "x" });
        a.due(7, now).unwrap();
        a.due(5, now + Duration::minutes(5)).unwrap();
        a.due(5, now - Duration::minutes(1)).unwrap();
        assert_eq!(a.due_now(now, 10).unwrap(), vec![5, 7]);
        a.put(7, &pr, now).unwrap();
        a.put(7, &pr, now).unwrap();
        a.put(6, &Value::Null, now).unwrap();
        assert_eq!(a.due_now(now, 10).unwrap(), vec![5]);
        assert_eq!(a.num("items").unwrap(), Some(1));
        assert_eq!(a.num("none").unwrap(), Some(1));
        let item = a.item(7).unwrap().unwrap();
        assert_eq!(item.kind, "pull");
        let v: Value = serde_json::from_str(&item.json.unwrap()).unwrap();
        assert_eq!(v["body"], "x");
        assert!(a.item(6).unwrap().unwrap().json.is_none());
        assert!(a.item(8).unwrap().is_none());
        assert_eq!(a.missing_below(9, 3).unwrap(), vec![8, 5, 4]);
        assert_eq!(a.missing_below(2, 3).unwrap(), vec![1]);
        let p = a.progress(now).unwrap();
        assert_eq!(p["items"], 1);
        assert!(p["bytes"].as_i64().unwrap() > 0);

        let out = dir.join("out");
        assert_eq!(a.export(&out).unwrap(), (1, 1));
        let z = std::fs::read(out.join("NixOS/nixpkgs/0000000.ndjson.zst")).unwrap();
        let text = String::from_utf8(zstd::decode_all(z.as_slice()).unwrap()).unwrap();
        assert_eq!(text.lines().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn queries_name_every_kind() {
        let q = batch_query(&[1, 2], 100, 30);
        assert!(q.contains("n1: issueOrPullRequest(number: 1)"));
        assert!(q.contains("fragment I on Issue"));
        assert!(q.contains("fragment P on PullRequest"));
        assert_eq!(q.matches('{').count(), q.matches('}').count());
        for m in more_of("PullRequest")
            .iter()
            .chain([review_comments()].iter())
        {
            let q = m.query();
            assert_eq!(q.matches('{').count(), q.matches('}').count());
        }
        assert_eq!(more_of("Issue").len(), 2);
    }

    #[test]
    fn aliases_go_back_to_github_names() {
        let mut v = json!({ "pullRequestState": "MERGED", "timelineItems": { "nodes": [
            { "__typename": "PullRequestCommit", "pullRequestCommit": { "oid": "a" } } ] } });
        unalias(&mut v);
        assert_eq!(v["state"], "MERGED");
        assert_eq!(v["timelineItems"]["nodes"][0]["commit"]["oid"], "a");
        assert!(v.get("pullRequestState").is_none());
    }

    /// A GitHub that answers the walk's queries for `1..=top`. Number `k`
    /// was updated `top - k` days ago; each fifth number is an issue.
    struct Fake {
        top: u64,
        now: DateTime<Utc>,
        page: usize,
        gone: Vec<u64>,
        broken: Mutex<Vec<u64>>,
        bumped: Mutex<Vec<(u64, DateTime<Utc>)>>,
        private: AtomicBool,
    }

    impl Fake {
        fn new(top: u64, page: usize) -> Self {
            Self {
                top,
                now: Utc::now(),
                page,
                gone: vec![],
                broken: Mutex::new(vec![]),
                bumped: Mutex::new(vec![]),
                private: AtomicBool::new(false),
            }
        }

        fn updated(&self, k: u64) -> String {
            let bumped = self.bumped.lock().unwrap();
            let t = bumped
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, t)| *t)
                .unwrap_or(self.now - Duration::days((self.top - k) as i64));
            t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        }

        fn item(&self, k: u64) -> Value {
            let empty =
                json!({ "totalCount": 0, "pageInfo": { "hasNextPage": false }, "nodes": [] });
            let comments = if k == 3 {
                json!({ "totalCount": 2, "pageInfo": { "hasNextPage": true, "endCursor": "c1" },
                        "nodes": [{ "body": "first" }] })
            } else {
                empty.clone()
            };
            let mut v = json!({ "id": format!("N{k}"), "number": k, "updatedAt": self.updated(k),
                                "comments": comments, "timelineItems": empty });
            if k.is_multiple_of(5) {
                v["__typename"] = "Issue".into();
            } else {
                v["__typename"] = "PullRequest".into();
                v["pullRequestState"] = "OPEN".into();
                v["files"] = empty.clone();
                v["reviews"] = empty;
            }
            v
        }

        fn answer(&self, query: &str, vars: &Value) -> Value {
            let rate = json!({ "cost": 1, "remaining": 4000 });
            if query.contains("node(id: $id)") {
                return json!({ "data": { "rateLimit": rate, "node": { "comments": {
                    "pageInfo": { "hasNextPage": false }, "nodes": [{ "body": "second" }] } } } });
            }
            if query.contains("issueOrPullRequest") {
                let numbers: Vec<u64> = query
                    .split("issueOrPullRequest(number: ")
                    .skip(1)
                    .filter_map(|s| s.split(')').next()?.parse().ok())
                    .collect();
                let broken = self.broken.lock().unwrap().clone();
                let mut repo = serde_json::Map::new();
                let mut errors = vec![];
                for k in numbers {
                    if broken.contains(&k) {
                        errors.push(json!({ "type": "FORBIDDEN", "path": [format!("n{k}")] }));
                    }
                    let gone = self.gone.contains(&k) || k > self.top;
                    if gone {
                        errors.push(json!({ "type": "NOT_FOUND", "path": [format!("n{k}")] }));
                    }
                    repo.insert(
                        format!("n{k}"),
                        if gone { Value::Null } else { self.item(k) },
                    );
                }
                let mut out = json!({ "data": { "rateLimit": rate, "repository": repo } });
                if !errors.is_empty() {
                    out["errors"] = errors.into();
                }
                return out;
            }
            let field = if query.contains("pullRequests(first") {
                "pullRequests"
            } else {
                "issues"
            };
            let mut all: Vec<u64> = (1..=self.top)
                .filter(|k| !self.gone.contains(k) && (k.is_multiple_of(5)) == (field == "issues"))
                .collect();
            all.sort_by_key(|k| std::cmp::Reverse(self.updated(*k)));
            let from: usize = vars["after"].as_str().map_or(0, |c| c.parse().unwrap());
            let to = (from + self.page).min(all.len());
            let nodes: Vec<Value> = all[from..to]
                .iter()
                .map(|k| json!({ "number": k, "updatedAt": self.updated(*k) }))
                .collect();
            json!({ "data": { "rateLimit": rate, "repository": { "isPrivate": self.private.load(Ordering::Relaxed), field: {
                "pageInfo": { "hasNextPage": to < all.len(), "endCursor": to.to_string() },
                "nodes": nodes } } } })
        }
    }

    async fn serve(fake: Arc<Fake>) -> String {
        async fn graphql(State(f): State<Arc<Fake>>, Json(body): Json<Value>) -> Json<Value> {
            Json(f.answer(body["query"].as_str().unwrap_or(""), &body["variables"]))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/graphql", axum::routing::post(graphql))
            .with_state(fake);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}/graphql")
    }

    struct Rig {
        dir: PathBuf,
        url: String,
        store: Store,
        events: crate::events::Log,
    }

    impl Rig {
        async fn new(name: &str, fake: Arc<Fake>) -> Self {
            let dir = tmp(name);
            Self {
                url: serve(fake).await,
                store: Store::open(Path::new(":memory:")).unwrap(),
                events: crate::events::Log::open(&dir.join("events")).unwrap(),
                dir,
            }
        }

        async fn try_pass(&self, budget: usize) -> (Archive, Result<usize>) {
            let a = Archive::open(&self.dir, "NixOS/nixpkgs", budget).unwrap();
            let mut client = github::Client::at(&self.url, vec!["t".into()]);
            let r = pass(&self.store, &self.events, &a, &mut client).await;
            (a, r)
        }

        /// One pass with a fresh `Archive`, as after a restart.
        async fn pass(&self, budget: usize) -> Archive {
            let a = Archive::open(&self.dir, "NixOS/nixpkgs", budget).unwrap();
            let mut client = github::Client::at(&self.url, vec!["t".into()]);
            pass(&self.store, &self.events, &a, &mut client)
                .await
                .unwrap();
            a
        }
    }

    #[tokio::test]
    async fn walk_resumes_and_reaches_the_first_number() {
        let mut fake = Fake::new(60, 1);
        fake.gone = vec![7];
        let fake = Arc::new(fake);
        let rig = Rig::new("walk", fake.clone()).await;

        let a = rig.pass(4).await;
        assert!(a.get("list_cursor_pullRequests").unwrap().is_some());
        assert!(a.get("watermark_pullRequests").unwrap().is_none());
        assert!(a.get("watermark_issues").unwrap().is_some());
        assert_eq!(a.num("top").unwrap(), Some(60));
        assert!(a.get("cursor").unwrap().is_none());

        let a = rig.pass(4).await;
        assert!(a.get("list_cursor_pullRequests").unwrap().is_none());
        assert!(a.get("watermark_pullRequests").unwrap().is_some());
        let recent = a.count(
            "SELECT COUNT(*) FROM due WHERE at <= ?1",
            &Utc::now().to_rfc3339(),
        );
        assert!(recent.unwrap() > 20);

        let mut cursor = u64::MAX;
        let mut passes = 2;
        loop {
            let a = rig.pass(4).await;
            if let Some(c) = a.num("cursor").unwrap() {
                assert!((c as u64) <= cursor);
                cursor = c as u64;
            }
            passes += 1;
            assert!(passes < 40, "the walk does not end");
            if a.get("backfill_done").unwrap().is_some() {
                break;
            }
        }
        let a = rig.pass(4).await;
        assert_eq!(a.get("last_error").unwrap(), None);
        assert_eq!(a.num("items").unwrap(), Some(59));
        assert_eq!(a.num("none").unwrap(), Some(1));
        assert_eq!(a.item(7).unwrap().unwrap().kind, NONE);
        assert_eq!(a.item(5).unwrap().unwrap().kind, "issue");
        let v: Value = serde_json::from_str(&a.item(3).unwrap().unwrap().json.unwrap()).unwrap();
        assert_eq!(v["comments"]["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(v["state"], "OPEN");
        assert_eq!(a.progress(Utc::now()).unwrap()["complete"], true);

        fake.bumped.lock().unwrap().push((40, Utc::now()));
        let a = rig.pass(4).await;
        let v: Value = serde_json::from_str(&a.item(40).unwrap().unwrap().json.unwrap()).unwrap();
        assert_eq!(v["updatedAt"], fake.updated(40));
        std::fs::remove_dir_all(&rig.dir).unwrap();
    }

    #[tokio::test]
    async fn a_failed_item_holds_back_the_end() {
        let fake = Arc::new(Fake::new(12, 100));
        fake.broken.lock().unwrap().push(5);
        let rig = Rig::new("failed", fake.clone()).await;
        rig.pass(100).await;
        let a = rig.pass(100).await;
        assert_eq!(a.num("cursor").unwrap(), Some(1));
        assert!(a.item(5).unwrap().is_none());
        assert_eq!(a.num("items").unwrap(), Some(11));
        assert!(a.get("backfill_done").unwrap().is_none());
        let p = a.progress(Utc::now()).unwrap();
        assert_eq!(p["failed"], 1);
        assert_eq!(p["complete"], false);

        fake.broken.lock().unwrap().clear();
        a.due(5, Utc::now()).unwrap();
        let a = rig.pass(100).await;
        assert!(a.item(5).unwrap().is_some());
        assert!(a.get("backfill_done").unwrap().is_some());
        assert_eq!(a.progress(Utc::now()).unwrap()["failed"], 0);
        std::fs::remove_dir_all(&rig.dir).unwrap();
    }

    #[tokio::test]
    async fn a_private_repository_is_not_walked_or_served() {
        let fake = Arc::new(Fake::new(5, 100));
        let rig = Rig::new("private", fake.clone()).await;
        let a = rig.pass(100).await;
        assert!(!a.is_private());
        assert_eq!(a.num("items").unwrap(), Some(5));

        fake.private.store(true, Ordering::Relaxed);
        fake.bumped.lock().unwrap().push((4, Utc::now()));
        let (a, r) = rig.try_pass(100).await;
        assert!(format!("{:#}", r.unwrap_err()).contains("public repositories only"));
        assert!(a.is_private());
        assert!(a.due_now(Utc::now(), 10).unwrap().is_empty());

        fake.private.store(false, Ordering::Relaxed);
        let a = rig.pass(100).await;
        assert!(!a.is_private());
        std::fs::remove_dir_all(&rig.dir).unwrap();
    }

    #[test]
    fn pace_counts_new_numbers_not_refetches() {
        let dir = tmp("pace");
        let a = Archive::open(&dir, "NixOS/nixpkgs", 10).unwrap();
        let t0 = Utc::now();
        let pr = |k: u64| json!({ "__typename": "PullRequest", "number": k, "updatedAt": "x" });
        a.set("top", "100").unwrap();
        for k in 91..=100 {
            a.put(k, &pr(k), t0).unwrap();
        }
        a.mark_pace(t0).unwrap();
        assert!(a.progress(t0 + Duration::minutes(5)).unwrap()["per_hour"].is_null());

        let t1 = t0 + Duration::hours(1);
        for k in 71..=90 {
            a.put(k, &pr(k), t1).unwrap();
        }
        for _ in 0..5 {
            for k in 91..=100 {
                a.put(k, &pr(k), t1).unwrap();
            }
        }
        a.mark_pace(t1).unwrap();
        let p = a.progress(t1).unwrap();
        assert_eq!(p["per_hour"], 20.0);
        assert_eq!(p["hours_left"], 3.5);

        let t7 = t0 + Duration::hours(7);
        a.mark_pace(t7).unwrap();
        assert_eq!(a.sample("pace_prev").unwrap().unwrap().0, t0);
        assert_eq!(a.progress(t7).unwrap()["per_hour"], 20.0 / 7.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn event(id: u64, number: u64) -> Value {
        json!({ "id": id.to_string(), "type": "IssueCommentEvent", "created_at": "2026-10-16T00:00:00Z",
                "actor": { "login": "alice" }, "repo": { "name": crate::events::REPO },
                "payload": { "issue": { "number": number } } })
    }

    #[test]
    fn events_make_items_due() {
        let dir = tmp("hints");
        let log = crate::events::Log::open(&dir.join("events")).unwrap();
        let a = Archive::open(&dir, "NixOS/nixpkgs", 10).unwrap();
        let now = Utc::now();
        log.insert(&[event(1, 11)]).unwrap();
        assert_eq!(hints(&log, &a, now).unwrap(), 0);
        log.insert(&[event(2, 12), event(3, 13)]).unwrap();
        assert_eq!(hints(&log, &a, now).unwrap(), 2);
        assert_eq!(hints(&log, &a, now).unwrap(), 0);
        assert!(a.due_now(now, 10).unwrap().is_empty());
        let later = now + Duration::minutes(HINT_DELAY);
        assert_eq!(a.due_now(later, 10).unwrap(), vec![12, 13]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Writes the queries to `$MM_DUMP`, to check them against GitHub's
    /// public schema (docs/ingest.md).
    #[test]
    fn dump_queries() {
        let Ok(dir) = std::env::var("MM_DUMP") else {
            return;
        };
        let dir = PathBuf::from(dir);
        std::fs::write(dir.join("batch.graphql"), batch_query(&[1, 2], 100, 30)).unwrap();
        for (i, m) in more_of("PullRequest")
            .iter()
            .chain(more_of("Issue").iter())
            .chain([review_comments()].iter())
            .enumerate()
        {
            std::fs::write(dir.join(format!("more{i}.graphql")), m.query()).unwrap();
        }
    }
}
