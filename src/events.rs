use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, DurationRound, NaiveDate, Utc};
use futures_util::Stream;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params, params_from_iter};
use serde_json::Value;
use std::collections::VecDeque;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;
use tokio::sync::watch;

use crate::github;
use crate::store::Store;
use crate::web::App;

const REPO: &str = "NixOS/nixpkgs";
const EVENTS_URL: &str = "https://api.github.com/repos/NixOS/nixpkgs/events?per_page=100";
/// The Events API serves 300 events at most.
const EVENT_PAGES: usize = 3;
const REPLAY_BATCH: usize = 500;

const SCHEMA: &str = "PRAGMA journal_mode = WAL;
     PRAGMA busy_timeout = 5000;
     CREATE TABLE IF NOT EXISTS event (
       seq INTEGER PRIMARY KEY,
       id TEXT NOT NULL UNIQUE,
       type TEXT NOT NULL,
       created_at TEXT NOT NULL,
       actor TEXT NOT NULL,
       number INTEGER,
       payload BLOB NOT NULL
     );
     CREATE INDEX IF NOT EXISTS event_created_at ON event (created_at);";

/// The nixpkgs event log: one SQLite file per month under `dir`, named
/// `YYYY-MM.sqlite`. Only the file of the current month takes writes.
pub struct Log {
    dir: PathBuf,
    writer: Mutex<Writer>,
    head: watch::Sender<u64>,
}

#[derive(Default)]
struct Writer {
    month: String,
    conn: Option<Connection>,
    /// An event at the end of a month can come again from the gap fill
    /// after the month changed.
    prev: Option<Connection>,
    next_seq: u64,
}

pub struct Stored {
    pub seq: u64,
    pub json: String,
}

#[derive(Clone, Default)]
pub struct Filter {
    pub kind: Option<String>,
    pub number: Option<u64>,
    pub actor: Option<String>,
}

struct Row {
    id: String,
    kind: String,
    created_at: String,
    actor: String,
    number: Option<u64>,
    payload: Vec<u8>,
}

impl Row {
    fn of(e: &Value) -> Option<Self> {
        let id = match &e["id"] {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return None,
        };
        let p = &e["payload"];
        Some(Self {
            id,
            kind: e["type"].as_str()?.to_string(),
            created_at: e["created_at"].as_str()?.to_string(),
            actor: e["actor"]["login"].as_str().unwrap_or("").to_string(),
            number: p["number"]
                .as_u64()
                .or_else(|| p["pull_request"]["number"].as_u64())
                .or_else(|| p["issue"]["number"].as_u64()),
            payload: zstd::encode_all(serde_json::to_vec(e).ok()?.as_slice(), 3).ok()?,
        })
    }
}

fn month_of(t: DateTime<Utc>) -> String {
    t.format("%Y-%m").to_string()
}

/// Not SQLITE_OPEN_READ_ONLY: a read-only connection cannot open a WAL file
/// after the last writer closed it and removed the `-wal` file.
fn reader(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open {}", path.display()))?;
    conn.busy_timeout(StdDuration::from_secs(5))?;
    Ok(conn)
}

fn has_id(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT 1 FROM event WHERE id = ?1", [id], |_| Ok(()))
        .optional()?
        .is_some())
}

impl Log {
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let mut head = 0;
        // the newest month file can be empty: the writer creates it before
        // the first insert of the month
        for month in months(dir)?.iter().rev() {
            let conn = Connection::open(dir.join(format!("{month}.sqlite")))?;
            conn.execute_batch(SCHEMA)?;
            head = conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM event", [], |r| {
                r.get::<_, i64>(0)
            })? as u64;
            if head > 0 {
                break;
            }
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            writer: Mutex::new(Writer {
                next_seq: head + 1,
                ..Default::default()
            }),
            head: watch::channel(head).0,
        })
    }

    fn path(&self, month: &str) -> PathBuf {
        self.dir.join(format!("{month}.sqlite"))
    }

    pub fn months(&self) -> Result<Vec<String>> {
        months(&self.dir)
    }

    /// The highest `seq` stored.
    pub fn head(&self) -> u64 {
        *self.head.borrow()
    }

    fn writer(&self, now: DateTime<Utc>) -> Result<std::sync::MutexGuard<'_, Writer>> {
        let month = month_of(now);
        let mut w = self.writer.lock().unwrap();
        if w.conn.is_none() || w.month != month {
            w.conn = None;
            w.prev = None;
            let conn = Connection::open(self.path(&month))?;
            conn.execute_batch(SCHEMA)?;
            let prev = months(&self.dir)?
                .into_iter()
                .rfind(|m| *m < month)
                .map(|m| reader(&self.path(&m)))
                .transpose()?;
            w.month = month;
            w.conn = Some(conn);
            w.prev = prev;
        }
        Ok(w)
    }

    /// Whether any of these GitHub event ids is stored already.
    pub fn knows_any(&self, ids: &[&str]) -> Result<bool> {
        let w = self.writer(Utc::now())?;
        for id in ids {
            if has_id(w.conn.as_ref().unwrap(), id)?
                || w.prev.as_ref().map(|p| has_id(p, id)).transpose()? == Some(true)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Stores the events in the given order and skips any id already stored.
    /// Returns how many are new.
    pub fn insert(&self, events: &[Value]) -> Result<usize> {
        self.insert_at(events, Utc::now())
    }

    fn insert_at(&self, events: &[Value], now: DateTime<Utc>) -> Result<usize> {
        let mut guard = self.writer(now)?;
        let w = &mut *guard;
        let tx = w.conn.as_mut().unwrap().transaction()?;
        let mut added = 0;
        for row in events.iter().filter_map(Row::of) {
            if w.prev.as_ref().map(|p| has_id(p, &row.id)).transpose()? == Some(true) {
                continue;
            }
            let n = tx.execute(
                "INSERT OR IGNORE INTO event (seq, id, type, created_at, actor, number, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    w.next_seq as i64,
                    row.id,
                    row.kind,
                    row.created_at,
                    row.actor,
                    row.number,
                    row.payload
                ],
            )?;
            if n == 1 {
                w.next_seq += 1;
                added += 1;
            }
        }
        tx.commit()?;
        if added > 0 {
            self.head.send_replace(w.next_seq - 1);
        }
        Ok(added)
    }

    /// Events with `after < seq <= upto` that pass the filter, in `seq`
    /// order, at most `limit`.
    pub fn read(&self, after: u64, upto: u64, f: &Filter, limit: usize) -> Result<Vec<Stored>> {
        let mut sql = String::from("SELECT seq, payload FROM event WHERE seq > ?1 AND seq <= ?2");
        let mut args: Vec<rusqlite::types::Value> =
            vec![(after as i64).into(), (upto as i64).into()];
        if let Some(t) = &f.kind {
            args.push(t.clone().into());
            sql.push_str(&format!(" AND type = ?{}", args.len()));
        }
        if let Some(n) = f.number {
            args.push((n as i64).into());
            sql.push_str(&format!(" AND number = ?{}", args.len()));
        }
        if let Some(a) = &f.actor {
            args.push(a.clone().into());
            sql.push_str(&format!(" AND actor = ?{}", args.len()));
        }
        sql.push_str(" ORDER BY seq LIMIT ?");
        let mut out = Vec::new();
        for m in self.months()? {
            let conn = reader(&self.path(&m))?;
            let mut a = args.clone();
            a.push(((limit - out.len()) as i64).into());
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(a), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?;
            for row in rows {
                let (seq, z) = row?;
                out.push(Stored {
                    seq: seq as u64,
                    json: String::from_utf8(zstd::decode_all(z.as_slice())?)?,
                });
            }
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// The events GitHub created on `day`, as NDJSON lines in the GH Archive
    /// format. An event arrives in the month file of the day it arrived, so
    /// the next month's file is read too.
    pub fn day(&self, day: NaiveDate) -> Result<Vec<u8>> {
        let from = day.format("%Y-%m-%dT00:00:00Z").to_string();
        let to = (day + Duration::days(1))
            .format("%Y-%m-%dT00:00:00Z")
            .to_string();
        let first = day.format("%Y-%m").to_string();
        let last = (day + Duration::days(32)).format("%Y-%m").to_string();
        let mut rows = Vec::new();
        for m in self.months()? {
            if m < first || m > last {
                continue;
            }
            let conn = reader(&self.path(&m))?;
            let mut stmt = conn.prepare(
                "SELECT created_at, seq, payload FROM event WHERE created_at >= ?1 AND created_at < ?2",
            )?;
            for row in stmt.query_map([&from, &to], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                ))
            })? {
                rows.push(row?);
            }
        }
        rows.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        let mut out = Vec::new();
        for (_, _, z) in rows {
            out.extend(zstd::decode_all(z.as_slice())?);
            out.push(b'\n');
        }
        Ok(out)
    }

    /// A consistent copy of one month file, while the writer may run.
    pub fn snapshot(&self, month: &str, dest: &Path) -> Result<()> {
        let _ = std::fs::remove_file(dest);
        let conn = reader(&self.path(month))?;
        conn.execute("VACUUM INTO ?1", [dest.to_string_lossy()])?;
        Ok(())
    }
}

fn months(dir: &Path) -> Result<Vec<String>> {
    let mut out: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".sqlite").map(String::from))
        .filter(|m| {
            m.len() == 7 && NaiveDate::parse_from_str(&format!("{m}-01"), "%Y-%m-%d").is_ok()
        })
        .collect();
    out.sort();
    Ok(out)
}

/// Events after `after` that pass the filter, then each new one as it is
/// stored. A slow reader reads from the files, so it never loses an event.
pub fn follow(log: Arc<Log>, after: u64, filter: Filter) -> impl Stream<Item = Stored> {
    let rx = log.head.subscribe();
    let state = (log, rx, after, filter, VecDeque::new());
    futures_util::stream::unfold(
        state,
        |(log, mut rx, mut after, filter, mut buf)| async move {
            loop {
                if let Some(s) = buf.pop_front() {
                    return Some((s, (log, rx, after, filter, buf)));
                }
                let head = *rx.borrow_and_update();
                if head > after {
                    let (l, f) = (log.clone(), filter.clone());
                    match tokio::task::spawn_blocking(move || l.read(after, head, &f, REPLAY_BATCH))
                        .await
                    {
                        Ok(Ok(rows)) => {
                            after = match rows.last() {
                                Some(r) if rows.len() == REPLAY_BATCH => r.seq,
                                _ => head,
                            };
                            buf.extend(rows);
                            continue;
                        }
                        Ok(Err(e)) => tracing::warn!("event replay: {e:#}"),
                        Err(e) => tracing::warn!("event replay: {e}"),
                    }
                    return None;
                }
                rx.changed().await.ok()?;
            }
        },
    )
}

fn by_id(a: &Value, b: &Value) -> std::cmp::Ordering {
    let key = |v: &Value| {
        let id = match &v["id"] {
            Value::String(s) => s.parse::<u128>().unwrap_or(0),
            Value::Number(n) => n.as_u64().unwrap_or(0) as u128,
            _ => 0,
        };
        (v["created_at"].as_str().unwrap_or("").to_string(), id)
    };
    key(a).cmp(&key(b))
}

/// One poll of the Events API. Returns the new events and the interval
/// GitHub asks for. A `304` answer costs no rate limit.
async fn poll_once(
    log: &Log,
    http: &reqwest::Client,
    token: &str,
    etag: &mut Option<String>,
) -> Result<(usize, u64)> {
    let mut events: Vec<Value> = Vec::new();
    let mut interval = 60;
    let mut new_etag = None;
    for page in 1..=EVENT_PAGES {
        let mut req = http
            .get(format!("{EVENTS_URL}&page={page}"))
            .bearer_auth(token)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json");
        if let (1, Some(e)) = (page, etag.as_deref()) {
            req = req.header(reqwest::header::IF_NONE_MATCH, e);
        }
        let resp = req.send().await?;
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(String::from)
        };
        if page == 1 {
            interval = header("x-poll-interval")
                .and_then(|v| v.parse().ok())
                .unwrap_or(60);
            if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
                return Ok((0, interval));
            }
            new_etag = header("etag");
        }
        let more = header("link").is_some_and(|l| l.contains("rel=\"next\""));
        let status = resp.status();
        if !status.is_success() {
            bail!("events page {page}: {status}");
        }
        let batch: Vec<Value> = resp.json().await.context("events page")?;
        let ids: Vec<&str> = batch.iter().filter_map(|e| e["id"].as_str()).collect();
        let known = log.knows_any(&ids)?;
        events.extend(batch);
        if known || !more {
            break;
        }
    }
    events.sort_by(by_id);
    let n = log.insert(&events)?;
    *etag = new_etag;
    Ok((n, interval))
}

/// Polls the Events API with the deployment's own token, at the interval
/// GitHub asks for.
pub async fn poll(log: Arc<Log>, token: String) {
    let http = github::http();
    let mut etag = None;
    loop {
        let wait = match poll_once(&log, &http, &token, &mut etag).await {
            Ok((n, wait)) => {
                if n > 0 {
                    tracing::debug!("events: {n} new");
                }
                wait
            }
            Err(e) => {
                tracing::warn!("events poll: {e:#}");
                60
            }
        };
        tokio::time::sleep(StdDuration::from_secs(wait.max(1))).await;
    }
}

fn archive_url(hour: DateTime<Utc>) -> String {
    format!(
        "https://data.gharchive.org/{}-{}.json.gz",
        hour.format("%Y-%m-%d"),
        hour.format("%-H")
    )
}

fn nixpkgs_events(gz: &[u8]) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    let reader = std::io::BufReader::new(flate2::read::MultiGzDecoder::new(gz));
    for line in reader.lines() {
        let line = line?;
        // a cheap test first: the file holds every public event on GitHub
        if !line.contains("\"name\":\"NixOS/nixpkgs\"") {
            continue;
        }
        let v: Value = serde_json::from_str(&line)?;
        if v["repo"]["name"] == REPO {
            out.push(v);
        }
    }
    out.sort_by(by_id);
    Ok(out)
}

enum Fill {
    Added(usize),
    NotYet,
}

async fn fill_hour(log: &Arc<Log>, http: &reqwest::Client, hour: DateTime<Utc>) -> Result<Fill> {
    let resp = http.get(archive_url(hour)).send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Fill::NotYet);
    }
    let gz = resp.error_for_status()?.bytes().await?;
    let log = log.clone();
    tokio::task::spawn_blocking(move || {
        let events = nixpkgs_events(&gz)?;
        Ok(Fill::Added(log.insert(&events)?))
    })
    .await?
}

/// GH Archive publishes an hour well within this delay. Some hours never
/// come, and the fill must not wait for them.
const MISSING_AFTER: i64 = 6;

fn missing(hour: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now - hour >= Duration::hours(MISSING_AFTER)
}

/// Each hour, adds the events of the past hour that GH Archive has and the
/// poll missed.
pub async fn archive(app: Arc<App>) {
    let (log, store) = (&app.events, &app.store);
    let http = github::http();
    loop {
        let now = Utc::now();
        let target = now.duration_trunc(Duration::hours(1)).unwrap() - Duration::hours(1);
        let mut next = store
            .get_time("gharchive_next")
            .ok()
            .flatten()
            .unwrap_or(target)
            .max(target - Duration::hours(23));
        while next <= target {
            match fill_hour(log, &http, next).await {
                Ok(Fill::Added(n)) => {
                    tracing::info!(
                        "GH Archive {}: {n} new events",
                        next.format("%Y-%m-%d %H:00")
                    );
                }
                Ok(Fill::NotYet) if missing(next, now) => {
                    tracing::warn!("GH Archive {}: missing, skipped", archive_url(next));
                }
                Ok(Fill::NotYet) => break,
                Err(e) => {
                    tracing::warn!("GH Archive {}: {e:#}", archive_url(next));
                    break;
                }
            }
            next += Duration::hours(1);
            if let Err(e) = store.set("gharchive_next", &next.to_rfc3339()) {
                tracing::warn!("GH Archive: {e:#}");
            }
        }
        tokio::time::sleep(StdDuration::from_secs(600)).await;
    }
}

/// An S3-compatible bucket, written with path-style URLs and AWS Signature
/// Version 4.
pub struct Bucket {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    access_key: String,
    secret_key: String,
    http: reqwest::Client,
}

impl Bucket {
    /// The credentials file holds `AWS_ACCESS_KEY_ID=…` and
    /// `AWS_SECRET_ACCESS_KEY=…` lines.
    pub fn new(endpoint: &str, bucket: &str, region: &str, credentials: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(credentials)
            .with_context(|| format!("read {}", credentials.display()))?;
        let get = |key: &str| {
            text.lines()
                .filter_map(|l| l.trim().split_once('='))
                .find(|(k, _)| k.trim() == key)
                .map(|(_, v)| v.trim().to_string())
                .with_context(|| format!("{key} in {}", credentials.display()))
        };
        Ok(Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            bucket: bucket.to_string(),
            region: region.to_string(),
            access_key: get("AWS_ACCESS_KEY_ID")?,
            secret_key: get("AWS_SECRET_ACCESS_KEY")?,
            http: github::http(),
        })
    }

    pub async fn put(&self, key: &str, body: Vec<u8>, content_type: &str) -> Result<()> {
        let url = reqwest::Url::parse(&format!("{}/{}/{key}", self.endpoint, self.bucket))?;
        let host = match (url.host_str(), url.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_string(),
            _ => bail!("no host in {}", self.endpoint),
        };
        let now = Utc::now();
        let hash = hex(&sha256(&body));
        let date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let auth = sigv4(
            &Request {
                method: "PUT",
                host: &host,
                path: url.path(),
                headers: &[("x-amz-content-sha256", &hash), ("x-amz-date", &date)],
                payload_hash: &hash,
            },
            &self.access_key,
            &self.secret_key,
            &self.region,
            now,
        );
        let resp = self
            .http
            .put(url)
            .header("x-amz-content-sha256", &hash)
            .header("x-amz-date", &date)
            .header(reqwest::header::AUTHORIZATION, auth)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            bail!(
                "put {key}: {status}: {}",
                resp.text().await.unwrap_or_default()
            );
        }
        Ok(())
    }
}

fn sha256(data: &[u8]) -> Vec<u8> {
    use sha2::Digest;
    sha2::Sha256::digest(data).to_vec()
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    use hmac::Mac;
    let mut m = hmac::Hmac::<sha2::Sha256>::new_from_slice(key).expect("any key length");
    m.update(data.as_bytes());
    m.finalize().into_bytes().to_vec()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

struct Request<'a> {
    method: &'a str,
    host: &'a str,
    path: &'a str,
    /// Signed besides `host`, with lower-case names.
    headers: &'a [(&'a str, &'a str)],
    payload_hash: &'a str,
}

fn sigv4(
    r: &Request,
    access_key: &str,
    secret_key: &str,
    region: &str,
    now: DateTime<Utc>,
) -> String {
    let date = now.format("%Y%m%d").to_string();
    let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let mut headers: Vec<(&str, &str)> = vec![("host", r.host)];
    headers.extend_from_slice(r.headers);
    headers.sort();
    let canonical_headers: String = headers
        .iter()
        .map(|(k, v)| format!("{k}:{}\n", v.trim()))
        .collect();
    let signed: Vec<&str> = headers.iter().map(|(k, _)| *k).collect();
    let signed = signed.join(";");
    let canonical = format!(
        "{}\n{}\n\n{canonical_headers}\n{signed}\n{}",
        r.method, r.path, r.payload_hash
    );
    let scope = format!("{date}/{region}/s3/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}",
        hex(&sha256(canonical.as_bytes()))
    );
    let mut key = hmac(format!("AWS4{secret_key}").as_bytes(), &date);
    for part in [region, "s3", "aws4_request"] {
        key = hmac(&key, part);
    }
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed}, Signature={}",
        hex(&hmac(&key, &to_sign))
    )
}

/// The daily files wait for the gap fill of the last hour of the day.
const DAY_SETTLES: i64 = 2;

async fn copy_once(log: &Arc<Log>, store: &Store, bucket: &Bucket) -> Result<()> {
    let now = Utc::now();
    let last = (now - Duration::hours(DAY_SETTLES)).date_naive() - Duration::days(1);
    let mut day = match store.get("events_copied_day")? {
        Some(d) => NaiveDate::parse_from_str(&d, "%Y-%m-%d")? + Duration::days(1),
        None => last,
    };
    while day <= last {
        let l = log.clone();
        let body = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            Ok(zstd::encode_all(l.day(day)?.as_slice(), 19)?)
        })
        .await??;
        let key = format!("{}.ndjson.zst", day.format("%Y/%m/%d"));
        bucket.put(&key, body, "application/zstd").await?;
        store.set("events_copied_day", &day.format("%Y-%m-%d").to_string())?;
        tracing::info!("copied {key}");
        day += Duration::days(1);
    }
    let current = month_of(now);
    for m in log.months()? {
        let done = format!("events_copied_{m}");
        if m >= current || store.get(&done)?.is_some() {
            continue;
        }
        let (l, month) = (log.clone(), m.clone());
        let body = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            let tmp = l.dir.join(format!(".{month}.copy"));
            l.snapshot(&month, &tmp)?;
            let body = zstd::encode_all(std::fs::File::open(&tmp)?, 10)?;
            std::fs::remove_file(&tmp)?;
            Ok(body)
        })
        .await??;
        let key = format!("{}.sqlite.zst", m.replace('-', "/"));
        bucket.put(&key, body, "application/zstd").await?;
        store.set(&done, &now.to_rfc3339())?;
        tracing::info!("copied {key}");
    }
    Ok(())
}

/// Copies each finished day, and each finished month file once, to the
/// bucket.
pub async fn copy(app: Arc<App>, bucket: Bucket) {
    loop {
        if let Err(e) = copy_once(&app.events, &app.store, &bucket).await {
            tracing::warn!("event copy: {e:#}");
        }
        tokio::time::sleep(StdDuration::from_secs(3600)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use futures_util::StreamExt;
    use serde_json::json;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mm-events-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn ev(id: &str, kind: &str, number: u64, at: &str) -> Value {
        json!({
            "id": id, "type": kind, "created_at": at,
            "actor": { "login": "alice" },
            "repo": { "name": REPO },
            "payload": { "action": "opened", "number": number, "pull_request": { "number": number } },
        })
    }

    #[test]
    fn seq_continues_across_months_and_ids_are_unique() {
        let dir = tmp("months");
        let log = Log::open(&dir).unwrap();
        let oct = Utc.with_ymd_and_hms(2026, 10, 31, 23, 0, 0).unwrap();
        let nov = Utc.with_ymd_and_hms(2026, 11, 1, 0, 30, 0).unwrap();
        let a = ev("1", "PullRequestEvent", 7, "2026-10-31T22:59:00Z");
        let b = ev("2", "IssueCommentEvent", 8, "2026-10-31T23:59:00Z");
        assert_eq!(log.insert_at(&[a.clone(), a.clone()], oct).unwrap(), 1);
        assert_eq!(log.insert_at(&[a, b.clone()], nov).unwrap(), 1);
        assert_eq!(log.insert_at(&[b], nov).unwrap(), 0);
        assert_eq!(log.months().unwrap(), vec!["2026-10", "2026-11"]);
        assert_eq!(log.head(), 2);
        drop(log);
        let log = Log::open(&dir).unwrap();
        assert_eq!(log.head(), 2);
        let all = log.read(0, 2, &Filter::default(), 10).unwrap();
        assert_eq!(all.iter().map(|s| s.seq).collect::<Vec<_>>(), vec![1, 2]);
        let v: Value = serde_json::from_str(&all[1].json).unwrap();
        assert_eq!(v["id"], "2");
        let f = Filter {
            kind: Some("IssueCommentEvent".into()),
            number: Some(8),
            actor: Some("alice".into()),
        };
        assert_eq!(log.read(0, 2, &f, 10).unwrap().len(), 1);
        let f = Filter {
            number: Some(9),
            ..Default::default()
        };
        assert!(log.read(0, 2, &f, 10).unwrap().is_empty());
        let day = log
            .day(NaiveDate::from_ymd_opt(2026, 10, 31).unwrap())
            .unwrap();
        assert_eq!(String::from_utf8(day).unwrap().lines().count(), 2);
        log.snapshot("2026-10", &dir.join("copy.sqlite")).unwrap();
        assert!(dir.join("copy.sqlite").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn seq_continues_after_an_empty_month_file() {
        let dir = tmp("empty-month");
        let log = Log::open(&dir).unwrap();
        let oct = Utc.with_ymd_and_hms(2026, 10, 31, 23, 0, 0).unwrap();
        let nov = Utc.with_ymd_and_hms(2026, 11, 1, 0, 5, 0).unwrap();
        let a = ev("1", "PushEvent", 1, "2026-10-31T22:59:00Z");
        let b = ev("2", "PushEvent", 2, "2026-11-01T00:06:00Z");
        assert_eq!(log.insert_at(&[a], oct).unwrap(), 1);
        assert_eq!(log.insert_at(&[], nov).unwrap(), 0);
        assert_eq!(log.months().unwrap(), vec!["2026-10", "2026-11"]);
        drop(log);
        let log = Log::open(&dir).unwrap();
        assert_eq!(log.head(), 1);
        assert_eq!(log.insert_at(&[b], nov).unwrap(), 1);
        assert_eq!(log.head(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn follow_replays_then_goes_live() {
        let dir = tmp("follow");
        let log = Arc::new(Log::open(&dir).unwrap());
        log.insert(&[ev("1", "PushEvent", 1, "2026-10-16T00:00:00Z")])
            .unwrap();
        log.insert(&[ev("2", "PullRequestEvent", 2, "2026-10-16T00:01:00Z")])
            .unwrap();
        let mut s = Box::pin(follow(log.clone(), 1, Filter::default()));
        assert_eq!(s.next().await.unwrap().seq, 2);
        log.insert(&[ev("3", "PullRequestEvent", 3, "2026-10-16T00:02:00Z")])
            .unwrap();
        assert_eq!(s.next().await.unwrap().seq, 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn archive_keeps_nixpkgs_only() {
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let other = json!({ "id": "5", "type": "PushEvent", "repo": { "name": "NixOS/nix" } });
        writeln!(gz, "{other}").unwrap();
        writeln!(gz, "{}", ev("9", "PushEvent", 1, "2026-10-16T00:00:00Z")).unwrap();
        writeln!(gz, "{}", ev("4", "PushEvent", 1, "2026-10-16T00:00:00Z")).unwrap();
        let events = nixpkgs_events(&gz.finish().unwrap()).unwrap();
        let ids: Vec<&str> = events.iter().filter_map(|e| e["id"].as_str()).collect();
        assert_eq!(ids, vec!["4", "9"]);
        assert_eq!(
            archive_url(Utc.with_ymd_and_hms(2026, 10, 14, 5, 0, 0).unwrap()),
            "https://data.gharchive.org/2026-10-14-5.json.gz"
        );
        let hour = Utc.with_ymd_and_hms(2026, 10, 14, 5, 0, 0).unwrap();
        assert!(!missing(hour, hour + Duration::minutes(90)));
        assert!(missing(hour, hour + Duration::hours(6)));
    }

    /// The "GET Object" example of the AWS Signature Version 4 documentation.
    #[test]
    fn sigv4_matches_the_aws_example() {
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let auth = sigv4(
            &Request {
                method: "GET",
                host: "examplebucket.s3.amazonaws.com",
                path: "/test.txt",
                headers: &[
                    ("range", "bytes=0-9"),
                    ("x-amz-content-sha256", empty),
                    ("x-amz-date", "20130524T000000Z"),
                ],
                payload_hash: empty,
            },
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap(),
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }
}
