//! What the update bot (nixpkgs-update, the account r-ryantm) did and will
//! do for each package: its last attempt, its place in the queue, and why
//! it gave up. The supervisor of the bot publishes its SQLite state and the
//! log of each attempt at nixpkgs-update-logs.nixos.org; this reads the
//! state every hour, and the tail of a log when a page asks.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use crate::web::App;

pub(crate) const BASE: &str = "https://nixpkgs-update-logs.nixos.org";
const STATE_URL: &str = "https://nixpkgs-update-logs.nixos.org/~supervisor/state.db";
pub(crate) const README: &str =
    "https://github.com/NixOS/infra/blob/main/non-critical-infra/modules/nixpkgs-update/README.md";
const EVERY: Duration = Duration::from_secs(3600);
/// After a failed read: a download of 15 MB is not asked again at once.
const RETRY: Duration = Duration::from_secs(600);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// The throughput is the attempts of these last days.
const RATE_DAYS: i64 = 7;
/// The supervisor gives an attempt with no result up after 12 hours: the
/// worker died or never reported.
const WORKER_GIVEN_UP: i64 = 12 * 3600;
/// `timeout 6h` in the worker script.
const TIMEOUT_EXIT: i64 = 124;
/// A worker's timeout, for the words of the page.
pub(crate) const TIMEOUT_HOURS: u32 = 6;
/// The end of a log that holds the reason.
const TAIL: usize = 4096;
/// A server that ignores `Range` sends the whole log; read no more than this.
const TAIL_READ_MAX: usize = 1 << 20;
const TAIL_TIMEOUT: Duration = Duration::from_secs(5);
/// A finished attempt never changes: its reason is kept for a long time.
const TAIL_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
/// A running attempt and a missing answer are asked again soon.
const TAIL_RETRY: Duration = Duration::from_secs(300);
const TAIL_CACHE_MAX: usize = 20_000;
const TAIL_CONCURRENT: usize = 4;
const TAIL_PER_MINUTE: u32 = 60;
/// How long a part of a page waits for a log.
const PART_WAIT: Duration = Duration::from_secs(4);
const LINE_MAX: usize = 200;

/// One attempt of the bot on an attribute, the last one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub started: DateTime<Utc>,
    pub finished: Option<DateTime<Utc>>,
    pub exit_code: Option<i64>,
}

/// What an attempt came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Running,
    Succeeded,
    Failed,
    TimedOut,
    /// Started, never reported, and the supervisor gave it up.
    Lost,
}

impl Attempt {
    pub fn state(&self, now: DateTime<Utc>) -> State {
        match (self.finished, self.exit_code) {
            (Some(_), Some(0)) => State::Succeeded,
            (Some(_), Some(TIMEOUT_EXIT)) => State::TimedOut,
            (Some(_), _) => State::Failed,
            (None, _) if (now - self.started).num_seconds() > WORKER_GIVEN_UP => State::Lost,
            (None, _) => State::Running,
        }
    }

    /// The worker writes one log a day for each attribute, named after the
    /// UTC day the attempt started.
    pub fn log_url(&self, attr: &str) -> String {
        format!("{BASE}/{attr}/{}.log", self.started.format("%Y-%m-%d"))
    }
}

/// A place in the queue of the bot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queued {
    /// 1 is next.
    pub position: usize,
    /// Which fetcher found the update: `github`, `repology` or
    /// `updatescript`.
    pub fetcher: String,
    /// The version nixpkgs has and the one the fetcher saw; an update
    /// script gives `0` and `1`, which say nothing.
    pub old: String,
    pub new: String,
    pub url: Option<String>,
}

impl Queued {
    /// The versions, when the fetcher knows them.
    pub fn versions(&self) -> Option<(&str, &str)> {
        (self.fetcher != "updatescript" && !self.old.is_empty() && !self.new.is_empty())
            .then_some((self.old.as_str(), self.new.as_str()))
    }

    pub fn source(&self) -> &str {
        match self.fetcher.as_str() {
            "github" => "GitHub releases",
            "updatescript" => "its update script",
            f => f,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub last: Option<Attempt>,
    pub queued: Option<Queued>,
}

/// The state of the bot, as last read.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Data {
    pub read_at: Option<DateTime<Utc>>,
    /// `Last-Modified` of the state file.
    pub file_at: Option<DateTime<Utc>>,
    /// Entries waiting in the queue.
    pub queue_len: usize,
    /// Attempts in the last [`RATE_DAYS`] days.
    pub attempts: usize,
    /// Attributes with a logged attempt.
    pub logged: usize,
    pub entries: BTreeMap<String, Entry>,
}

impl Data {
    pub fn per_day(&self) -> f64 {
        self.attempts as f64 / RATE_DAYS as f64
    }

    /// When the bot reaches a position, at its pace of the last days.
    pub fn eta(&self, position: usize) -> Option<DateTime<Utc>> {
        let per_day = self.per_day();
        if per_day <= 0.0 {
            return None;
        }
        let days = position as f64 / per_day;
        self.read_at
            .map(|t| t + chrono::Duration::seconds((days * 86400.0) as i64))
    }

    pub fn get(&self, attr: &str) -> Option<&Entry> {
        self.entries.get(attr)
    }

    /// The last attempt on an attribute, when it failed or timed out.
    pub fn failed_attempt(&self, attr: &str, now: DateTime<Utc>) -> Option<&Attempt> {
        let a = self.get(attr)?.last.as_ref()?;
        matches!(a.state(now), State::Failed | State::TimedOut).then_some(a)
    }

    /// The update script of the package itself put it in the queue: it
    /// found something, even when no version is known.
    pub fn by_update_script(&self, attr: &str) -> bool {
        self.get(attr)
            .and_then(|e| e.queued.as_ref())
            .is_some_and(|q| q.fetcher == "updatescript")
    }
}

/// `old new [url]`, as the fetchers write it.
fn payload(s: &str) -> (String, String, Option<String>) {
    let mut w = s.split_whitespace();
    let old = w.next().unwrap_or_default().to_string();
    let new = w.next().unwrap_or_default().to_string();
    let url = w.next().map(String::from);
    (old, new, url)
}

fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).unwrap_or_default()
}

/// The state file, as the supervisor's `dequeue` reads it: the newest run
/// of each fetcher counts for an attribute, and the rows go in the order
/// `last_started`, `order_jitter`, `attr_path`, `fetcher_id`.
pub fn read(path: &Path, now: DateTime<Utc>) -> Result<Data> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("open")?;
    let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
    let mut stmt = conn
        .prepare("SELECT attr_path, started, finished, exit_code FROM log")
        .context("log")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<i64>>(3)?,
        ))
    })?;
    let since = now.timestamp() - RATE_DAYS * 86400;
    let mut attempts = 0;
    for row in rows {
        let (attr, started, finished, exit_code) = row?;
        if started >= since {
            attempts += 1;
        }
        entries.entry(attr).or_default().last = Some(Attempt {
            started: at(started),
            finished: finished.map(at),
            exit_code,
        });
    }
    let logged = entries.len();
    let fetchers: HashMap<i64, String> = conn
        .prepare("SELECT fetcher_id, name FROM fetchers")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut stmt = conn
        .prepare(
            "SELECT attr_path, fetcher_id, payload, is_dequeued \
             FROM queue \
             ORDER BY last_started ASC, order_jitter, attr_path, fetcher_id, fetcher_run_started DESC",
        )
        .context("queue")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    let mut seen: HashSet<(String, i64)> = HashSet::new();
    let mut position = 0;
    for row in rows {
        let (attr, fetcher_id, payload_text, dequeued) = row?;
        if !seen.insert((attr.clone(), fetcher_id)) || dequeued != 0 {
            continue;
        }
        position += 1;
        let e = entries.entry(attr).or_default();
        if e.queued.is_some() {
            continue;
        }
        let (old, new, url) = payload(&payload_text);
        e.queued = Some(Queued {
            position,
            fetcher: fetchers
                .get(&fetcher_id)
                .cloned()
                .unwrap_or_else(|| fetcher_id.to_string()),
            old,
            new,
            url,
        });
    }
    Ok(Data {
        read_at: Some(now),
        file_at: None,
        queue_len: position,
        attempts,
        logged,
        entries,
    })
}

/// The end of a log, read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Outcome {
    /// The last line, or the line that says the most.
    pub line: String,
    /// The pull request the attempt opened.
    pub pr: Option<u64>,
    /// A pull request that may already carry the update.
    pub existing: Option<u64>,
    pub build_failed: bool,
    /// The bot gave up because there was nothing to do: the update script
    /// changed nothing, or a bot branch already has the version.
    pub nothing_to_do: bool,
}

impl Outcome {
    /// Not a failure anyone has to act on.
    pub fn benign(&self) -> bool {
        self.nothing_to_do || self.existing.is_some()
    }
}

/// A last line that says the bot had nothing to do.
fn nothing_to_do(line: &str) -> bool {
    line.starts_with("The diff was empty")
        || line.starts_with("An auto update branch exists with an equal or greater version")
        || line.contains(" is not newer than ")
}

/// The numbers the bot prints as API URLs.
fn github_number(line: &str, kind: &str) -> Option<u64> {
    let marker = format!("api.github.com/repos/NixOS/nixpkgs/{kind}/");
    let rest = &line[line.find(&marker)? + marker.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn shorten(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= LINE_MAX {
        return s.to_string();
    }
    let mut out: String = s.chars().take(LINE_MAX - 1).collect();
    out.push('…');
    out
}

/// What the end of a log says.
pub fn outcome(tail: &str) -> Outcome {
    let lines: Vec<&str> = tail
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut out = Outcome::default();
    let mut existing_ahead = false;
    for l in &lines {
        if let Some(n) = github_number(l, "pulls") {
            out.pr = Some(n);
        }
        if l.starts_with("There might already be an open PR") {
            existing_ahead = true;
        } else if existing_ahead {
            if let Some(n) = github_number(l, "issues") {
                out.existing = Some(n);
            }
        }
        if l.starts_with("error: Cannot build") || l.starts_with("error: builder for") {
            out.build_failed = true;
            out.line = shorten(l);
        }
    }
    if out.build_failed {
        return out;
    }
    out.line = match (out.pr, out.existing) {
        (Some(_), _) => "opened a pull request".into(),
        (None, Some(_)) => "a pull request may already carry this update".into(),
        _ => lines
            .iter()
            .rev()
            .find(|l| !l.starts_with("http"))
            .map(|l| shorten(l))
            .unwrap_or_default(),
    };
    out.nothing_to_do = out.pr.is_none() && out.existing.is_none() && nothing_to_do(&out.line);
    out
}

/// The reason of an attempt, as a page shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tail {
    Read(Outcome),
    NoAnswer,
    /// A request is on its way; the page asks again.
    Asking,
    /// Over the limit: nothing was sent.
    NotAsked,
    /// Not in the cache; the script of the page asks for it.
    Later,
}

impl Tail {
    pub fn outcome(&self) -> Option<&Outcome> {
        match self {
            Tail::Read(o) => Some(o),
            _ => None,
        }
    }

    pub fn state(&self) -> &'static str {
        match self {
            Tail::Read(_) => "read",
            Tail::NoAnswer => "no answer",
            Tail::Asking => "asking",
            Tail::NotAsked => "not asked yet",
            Tail::Later => "later",
        }
    }
}

/// The state of the bot, and the reasons of the attempts a page asked for.
/// The script of a page asks for a reason, a few at a time and a cap a
/// minute, as the Hydra rows do.
pub struct Bot {
    pub data: RwLock<Arc<Data>>,
    http: reqwest::Client,
    base: String,
    /// log URL → when it was read, whether the attempt was over, and the
    /// tail.
    tails: Mutex<HashMap<String, (Instant, bool, Tail)>>,
    asking: Mutex<HashSet<String>>,
    running: tokio::sync::Semaphore,
    window: Mutex<crate::health::Window>,
    not_asked: std::sync::atomic::AtomicU64,
}

impl Default for Bot {
    fn default() -> Self {
        Self::new(BASE)
    }
}

impl Bot {
    fn new(base: &str) -> Self {
        Self {
            data: Default::default(),
            http: crate::health::http(TAIL_TIMEOUT),
            base: base.to_string(),
            tails: Mutex::new(HashMap::new()),
            asking: Mutex::new(HashSet::new()),
            running: tokio::sync::Semaphore::new(TAIL_CONCURRENT),
            window: Mutex::new(crate::health::Window::new()),
            not_asked: Default::default(),
        }
    }

    pub fn snapshot(&self) -> Arc<Data> {
        self.data.read().unwrap().clone()
    }

    fn log_url(&self, attr: &str, a: &Attempt) -> String {
        a.log_url(attr).replacen(BASE, &self.base, 1)
    }

    /// The limits and the counts, for `/sync`.
    pub fn stats(&self) -> serde_json::Value {
        let d = self.snapshot();
        serde_json::json!({
            "read_at": d.read_at,
            "file_at": d.file_at,
            "every_seconds": EVERY.as_secs(),
            "queue": d.queue_len,
            "logged": d.logged,
            "attempts_per_day": (d.per_day() * 10.0).round() / 10.0,
            "rate_days": RATE_DAYS,
            "known": d.entries.len(),
            "tails": {
                "concurrent": TAIL_CONCURRENT,
                "per_minute": TAIL_PER_MINUTE,
                "cached": self.tails.lock().unwrap().len(),
                "not_asked": self.not_asked.load(std::sync::atomic::Ordering::Relaxed),
            },
        })
    }

    /// The last attempt failed, and its log, when read, does not say that
    /// there was nothing to do.
    pub fn failed_on(&self, attr: &str, now: DateTime<Utc>) -> bool {
        let data = self.snapshot();
        let Some(a) = data.failed_attempt(attr, now) else {
            return false;
        };
        !matches!(self.known(attr, a), Some(Tail::Read(o)) if o.benign())
    }

    /// See [`Data::by_update_script`].
    pub fn by_update_script(&self, attr: &str) -> bool {
        self.snapshot().by_update_script(attr)
    }

    /// What the cache holds about the log of an attempt; it never asks.
    pub fn known(&self, attr: &str, a: &Attempt) -> Option<Tail> {
        let url = self.log_url(attr, a);
        let tails = self.tails.lock().unwrap();
        let (at, over, tail) = tails.get(&url)?;
        let ttl = match tail {
            Tail::Read(_) if *over => TAIL_TTL,
            _ => TAIL_RETRY,
        };
        (at.elapsed() < ttl).then(|| tail.clone())
    }

    fn keep(&self, url: &str, over: bool, tail: &Tail) {
        let mut tails = self.tails.lock().unwrap();
        if tails.len() >= TAIL_CACHE_MAX {
            tails.retain(|_, (at, over, t)| {
                at.elapsed()
                    < match t {
                        Tail::Read(_) if *over => TAIL_TTL,
                        _ => TAIL_RETRY,
                    }
            });
        }
        tails.insert(url.to_string(), (Instant::now(), over, tail.clone()));
    }

    /// A reason for the cache, so a test never reaches the log server.
    #[cfg(test)]
    pub(crate) fn preset(&self, attr: &str, a: &Attempt, tail: Tail) {
        let url = self.log_url(attr, a);
        self.keep(&url, a.finished.is_some(), &tail);
    }

    fn refuse(&self) -> Tail {
        self.not_asked
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Tail::NotAsked
    }

    /// The reason of an attempt. Over the limit, the answer is `NotAsked`
    /// and nothing is sent.
    pub async fn tail(&self, attr: &str, a: &Attempt) -> Tail {
        if let Some(t) = self.known(attr, a) {
            return t;
        }
        let Ok(_permit) = self.running.try_acquire() else {
            return self.refuse();
        };
        if !self
            .window
            .lock()
            .unwrap()
            .take(Instant::now(), TAIL_PER_MINUTE)
        {
            return self.refuse();
        }
        let url = self.log_url(attr, a);
        let tail = match fetch_tail(&self.http, &url).await {
            Ok(text) => Tail::Read(outcome(&text)),
            Err(e) => {
                tracing::debug!("update bot log {url}: {e:#}");
                Tail::NoAnswer
            }
        };
        self.keep(&url, a.finished.is_some(), &tail);
        tail
    }

    /// [`Bot::tail`] for a part of a page: after [`PART_WAIT`] the part goes
    /// on with [`Tail::Asking`], and the request ends in the background.
    pub async fn tail_for_part(app: &crate::web::Shared, attr: &str, a: &Attempt) -> Tail {
        if let Some(t) = app.updatebot.known(attr, a) {
            return t;
        }
        let url = app.updatebot.log_url(attr, a);
        if !app.updatebot.asking.lock().unwrap().insert(url.clone()) {
            return Tail::Asking;
        }
        let (app, attr, a) = (app.clone(), attr.to_string(), a.clone());
        let ask = tokio::spawn(async move {
            let t = app.updatebot.tail(&attr, &a).await;
            app.updatebot.asking.lock().unwrap().remove(&url);
            t
        });
        match tokio::time::timeout(PART_WAIT, ask).await {
            Ok(Ok(t)) => t,
            Ok(Err(_)) => Tail::NoAnswer,
            Err(_) => Tail::Asking,
        }
    }
}

/// The last [`TAIL`] bytes of a log. The server honours `Range`; one that
/// did not would send the whole log, so the read stops at a limit.
async fn fetch_tail(http: &reqwest::Client, url: &str) -> Result<String> {
    let mut resp = http
        .get(url)
        .header(reqwest::header::RANGE, format!("bytes=-{TAIL}"))
        .send()
        .await?
        .error_for_status()?;
    let mut buf: Vec<u8> = Vec::with_capacity(TAIL);
    let mut read = 0;
    while let Some(chunk) = resp.chunk().await? {
        read += chunk.len();
        buf.extend_from_slice(&chunk);
        if buf.len() > TAIL {
            buf.drain(..buf.len() - TAIL);
        }
        if read > TAIL_READ_MAX {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

pub fn load(store: &crate::store::Store) -> Data {
    store
        .get("updatebot")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Downloads the state file when it changed, reads it, and publishes it.
/// Returns whether anything changed.
async fn refresh(app: &App, dir: &Path, http: &reqwest::Client) -> Result<bool> {
    let current = app.updatebot.snapshot();
    let mut req = http.get(STATE_URL);
    if let Some(t) = current.file_at {
        req = req.header(
            reqwest::header::IF_MODIFIED_SINCE,
            t.format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
        );
    }
    let resp = req.send().await.context("download")?;
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(false);
    }
    let resp = resp.error_for_status().context("download")?;
    let file_at = resp
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| DateTime::parse_from_rfc2822(s).ok())
        .map(|t| t.with_timezone(&Utc));
    let bytes = resp.bytes().await.context("download")?;
    let size = bytes.len();
    let now = Utc::now();
    let path = dir.join("state.db");
    let dir = dir.to_path_buf();
    let mut data = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&dir).context("data dir")?;
        std::fs::write(&path, &bytes).context("write")?;
        read(&path, now)
    })
    .await
    .context("read")??;
    data.file_at = file_at;
    tracing::info!(
        "update bot: {} queued, {} attributes logged, {:.0} attempts a day, {} bytes",
        data.queue_len,
        data.logged,
        data.per_day(),
        size
    );
    match serde_json::to_string(&data) {
        Ok(s) => {
            if let Err(e) = app.store.set("updatebot", &s) {
                tracing::warn!("update bot: {e:#}");
            }
        }
        Err(e) => tracing::warn!("update bot: {e:#}"),
    }
    *app.updatebot.data.write().unwrap() = Arc::new(data);
    Ok(true)
}

/// Its own task: one read an hour, sooner after a failure. The last read
/// serves the pages until the first one of this start.
pub async fn run(app: Arc<App>) {
    let stored = load(&app.store);
    if stored.read_at.is_some() {
        *app.updatebot.data.write().unwrap() = Arc::new(stored);
    }
    let http = crate::health::http(DOWNLOAD_TIMEOUT);
    let dir = app.data_dir.join("updatebot");
    loop {
        let wait = match refresh(&app, &dir, &http).await {
            Ok(_) => EVERY,
            Err(e) => {
                tracing::warn!("update bot: {e:#}");
                RETRY
            }
        };
        tokio::time::sleep(wait).await;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A row of `log`: attribute, started, finished, exit code.
    type LogRow<'a> = (&'a str, i64, Option<i64>, Option<i64>);
    /// A row of `queue`: attribute, fetcher, payload, run, last started,
    /// dequeued.
    type QueueRow<'a> = (&'a str, i64, &'a str, i64, Option<i64>, i64);

    /// A state file as `supervisor.py` makes it.
    pub(crate) fn state(dir: &Path, log: &[LogRow], queue: &[QueueRow]) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("state.db");
        let _ = std::fs::remove_file(&path);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE fetchers (fetcher_id integer PRIMARY KEY, name text NOT NULL UNIQUE);
             INSERT INTO fetchers VALUES (1, 'github'), (3, 'updatescript'), (1458, 'repology');
             CREATE TABLE queue (fetcher_id integer NOT NULL, fetcher_run_started integer NOT NULL,
               attr_path text NOT NULL, payload text NOT NULL, is_dequeued integer NOT NULL DEFAULT 0,
               last_started integer, order_jitter integer,
               PRIMARY KEY (fetcher_run_started, fetcher_id, attr_path));
             CREATE TABLE log (attr_path text PRIMARY KEY, started integer NOT NULL,
               finished integer, exit_code integer);",
        )
        .unwrap();
        for (attr, started, finished, code) in log {
            conn.execute(
                "INSERT INTO log VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![attr, started, finished, code],
            )
            .unwrap();
        }
        for (attr, fetcher, payload, run, last_started, dequeued) in queue {
            conn.execute(
                "INSERT INTO queue VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![fetcher, run, attr, payload, dequeued, last_started, 0],
            )
            .unwrap();
        }
        path
    }

    fn now() -> DateTime<Utc> {
        at(1_793_737_800)
    }

    #[test]
    fn the_queue_goes_in_dequeue_order_and_skips_dequeued_rows() {
        let dir = std::env::temp_dir().join(format!("mm-updatebot-{}", std::process::id()));
        let path = state(
            &dir,
            &[
                ("hello", 1_793_000_000, Some(1_793_000_100), Some(0)),
                ("flameshot", 1_792_681_213, Some(1_792_681_292), Some(1)),
                ("gittype", 1_793_663_182, Some(1_793_684_782), Some(124)),
                ("emote", 1_793_737_700, None, None),
                ("old", 1_700_000_000, Some(1_700_000_001), Some(1)),
            ],
            &[
                ("flameshot", 3, "0 1", 10, Some(1_792_681_213), 0),
                (
                    "flameshot",
                    1458,
                    "14.0.0 15.0.0 https://repology.org/project/flameshot/versions",
                    10,
                    Some(1_792_681_213),
                    0,
                ),
                (
                    "fresh",
                    1,
                    "1.0 1.1 https://github.com/x/y/releases",
                    10,
                    None,
                    0,
                ),
                ("gone", 1, "1 2", 10, Some(1_792_000_000), 1),
                ("hello", 1458, "2.12.2 2.12.3", 10, Some(1_793_000_000), 0),
                ("hello", 1458, "2.12.2 2.12.4", 9, Some(1_793_000_000), 0),
            ],
        );
        let d = read(&path, now()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(d.queue_len, 4);
        // gittype and emote started in the last 7 days
        assert_eq!(d.attempts, 2);
        assert_eq!(d.logged, 5);
        let fresh = d.get("fresh").unwrap().queued.as_ref().unwrap();
        assert_eq!((fresh.position, fresh.source()), (1, "GitHub releases"));
        assert_eq!(fresh.versions(), Some(("1.0", "1.1")));
        let f = d.get("flameshot").unwrap();
        let q = f.queued.as_ref().unwrap();
        assert_eq!((q.position, q.fetcher.as_str()), (2, "updatescript"));
        assert_eq!(q.versions(), None);
        assert_eq!(f.last.as_ref().unwrap().state(now()), State::Failed);
        let h = d.get("hello").unwrap();
        assert_eq!(h.queued.as_ref().unwrap().position, 4);
        assert_eq!(h.queued.as_ref().unwrap().new, "2.12.3");
        assert_eq!(h.last.as_ref().unwrap().state(now()), State::Succeeded);
        assert!(d.get("gone").is_none_or(|e| e.queued.is_none()));
        assert_eq!(
            d.get("gittype")
                .unwrap()
                .last
                .as_ref()
                .unwrap()
                .state(now()),
            State::TimedOut
        );
        assert_eq!(
            d.get("emote").unwrap().last.as_ref().unwrap().state(now()),
            State::Running
        );
        assert_eq!(
            d.get("old").unwrap().last.as_ref().unwrap().state(now()),
            State::Failed
        );
        let lost = Attempt {
            started: at(1_793_600_000),
            finished: None,
            exit_code: None,
        };
        assert_eq!(lost.state(now()), State::Lost);
        assert_eq!(
            d.get("flameshot")
                .unwrap()
                .last
                .as_ref()
                .unwrap()
                .log_url("flameshot"),
            "https://nixpkgs-update-logs.nixos.org/flameshot/2026-10-22.log"
        );
    }

    #[test]
    fn the_estimate_follows_the_pace() {
        let d = Data {
            read_at: Some(now()),
            attempts: 7 * 2000,
            ..Default::default()
        };
        assert_eq!(d.per_day(), 2000.0);
        assert_eq!(d.eta(1000), Some(now() + chrono::Duration::hours(12)));
        assert_eq!(Data::default().eta(5), None);
    }

    #[test]
    fn the_end_of_a_log_says_what_happened() {
        let o =
            outcome("cc @x\n\n> tip\nhttps://api.github.com/repos/NixOS/nixpkgs/pulls/582811\n");
        assert_eq!(o.pr, Some(582811));
        assert_eq!(o.line, "opened a pull request");
        let o = outcome(
            "The auto update branch does not match or exceed the new version.\n\
             There might already be an open PR for this update:\n\
             - gpclient: 3.0.5 -> 3.2.0\n  URL \"https://api.github.com/repos/NixOS/nixpkgs/issues/582351\"\n",
        );
        assert_eq!((o.pr, o.existing), (None, Some(582351)));
        assert_eq!(o.line, "a pull request may already carry this update");
        let o = outcome(
            "       Output paths:\n/nix/store/a-vendor\nerror: Cannot build '/nix/store/w-kclvm_cli-0.13.2.drv'.\n       Reason: 1 dependency failed.\n       Output paths:\n         /nix/store/j-kclvm_cli-0.13.2\n",
        );
        assert!(o.build_failed);
        assert_eq!(
            o.line,
            "error: Cannot build '/nix/store/w-kclvm_cli-0.13.2.drv'."
        );
        let o = outcome("attrpath: x\n\nThe diff was empty after rewrites.\n\n");
        assert_eq!(o.line, "The diff was empty after rewrites.");
        assert!(o.nothing_to_do && o.benign());
        assert!(
            outcome("An auto update branch exists with an equal or greater version\n").benign()
        );
        assert!(outcome("0.3.0 is not newer than 0.3.0-unstable-2023-12-27 according to Nix; versionComparison: -1\n").nothing_to_do);
        assert!(!outcome("Version in attr path x not compatible with 3.4.11\n").benign());
        assert_eq!(outcome("").line, "");
        let long = "x".repeat(300);
        assert_eq!(outcome(&long).line.chars().count(), LINE_MAX);
    }

    #[test]
    fn a_payload_has_two_versions_and_maybe_a_url() {
        assert_eq!(
            payload("2.8.12 3.4.8 https://github.com/libsdl-org/SDL_image/releases"),
            (
                "2.8.12".into(),
                "3.4.8".into(),
                Some("https://github.com/libsdl-org/SDL_image/releases".into())
            )
        );
        assert_eq!(payload("0 1"), ("0".into(), "1".into(), None));
    }

    async fn log_server(body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let app = axum::Router::new().route(
                "/*path",
                axum::routing::get(move |h: axum::http::HeaderMap| async move {
                    assert_eq!(h.get("range").unwrap(), "bytes=-4096");
                    body
                }),
            );
            axum::serve(listener, app).await.unwrap();
        });
        base
    }

    #[tokio::test]
    async fn a_read_tail_is_kept_and_not_asked_again() {
        let base = log_server("x\nThe diff was empty after rewrites.\n").await;
        let bot = Bot::new(&base);
        let a = Attempt {
            started: at(1_792_681_213),
            finished: Some(at(1_792_681_292)),
            exit_code: Some(1),
        };
        assert_eq!(bot.known("flameshot", &a), None);
        let t = bot.tail("flameshot", &a).await;
        assert_eq!(
            t.outcome().unwrap().line,
            "The diff was empty after rewrites."
        );
        assert_eq!(bot.known("flameshot", &a), Some(t));
        assert_eq!(bot.stats()["tails"]["cached"], 1);
        // a failure with nothing to do is no failure once the log is read
        let mut d = Data::default();
        d.entries.insert(
            "flameshot".into(),
            Entry {
                last: Some(a.clone()),
                queued: None,
            },
        );
        d.entries.insert(
            "other".into(),
            Entry {
                last: Some(a.clone()),
                queued: None,
            },
        );
        *bot.data.write().unwrap() = Arc::new(d);
        assert!(!bot.failed_on("flameshot", Utc::now()));
        assert!(bot.failed_on("other", Utc::now()));
    }

    #[tokio::test]
    async fn over_the_minute_cap_nothing_is_sent() {
        let base = log_server("done\n").await;
        let bot = Bot::new(&base);
        let a = |n: i64| Attempt {
            started: at(1_792_000_000 + n * 86400),
            finished: Some(at(1_792_000_001 + n * 86400)),
            exit_code: Some(1),
        };
        for n in 0..TAIL_PER_MINUTE as i64 {
            assert!(matches!(bot.tail("x", &a(n)).await, Tail::Read(_)));
        }
        assert_eq!(bot.tail("x", &a(999)).await, Tail::NotAsked);
        assert_eq!(bot.stats()["tails"]["not_asked"], 1);
    }
}
