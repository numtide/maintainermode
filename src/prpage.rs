//! Our own page for one nixpkgs pull request: its facts, its place in the
//! queues, what it touches, its diff and its discussion. People act on
//! GitHub; Maintainer Mode posts nothing.

use anyhow::{Context, Result, bail};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fmt::Write;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::OnceCell;

use crate::actions::Act;
use crate::archive::RESERVE;
use crate::area::Index;
use crate::catalog::{Flags, Subject};
use crate::ledger::{self, Credit, Dropped};
use crate::model::Pr;
use crate::queue::{self, Entry};
use crate::ui::{Page, Section, kv};
use crate::web::{App, Shared, Snapshot, ago, esc, icon, octicon};

const PULL: &str = "https://github.com/NixOS/nixpkgs/pull/";
const API: &str = "https://api.github.com";
const NIXPKGS: &str = "https://github.com/NixOS/nixpkgs/blob/master";
/// GitHub's REST API serves at most 3000 files of a pull request; a review
/// page needs far fewer.
const FILE_CAP: usize = 100;
const PER_PAGE: usize = 100;
/// A patch longer than this shows as a note and a link.
const PATCH_LINES: usize = 1500;
/// Files with more changed lines than this start folded.
const FOLD_LINES: usize = 400;
/// Reviews, review comments per review, and comments read live.
const FIRST: usize = 100;
const BODY_CAP: usize = 65_536;
/// The archive holds every note; the page shows the latest.
const NOTE_CAP: usize = 300;
const SUBJECTS: usize = 50;
const FETCH_SECS: u64 = 15;
const CACHE_BYTES: usize = 32 << 20;
const CACHE_ENTRIES: usize = 256;
/// Live reads from GitHub per minute, for the whole server.
const LIVE_PER_MINUTE: usize = 20;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/pr/:number", get(pr_page))
        .route("/api/v1/prs/:number", get(api_pr))
}

pub(crate) fn github_url(n: u64) -> String {
    format!("{PULL}{n}")
}

/// The small GitHub mark that sits next to a pull request title.
pub(crate) fn github_icon(n: u64) -> String {
    format!(
        " <a class=\"color-fg-muted ml-1\" href=\"{}\" title=\"Open on GitHub\" aria-label=\"Open #{n} on GitHub\">{}</a>",
        github_url(n),
        icon(octicon!("mark-github"))
    )
}

/// The number of a link to a nixpkgs pull request itself, not to a part of it.
pub(crate) fn number_of(url: &str) -> Option<u64> {
    let rest = url.strip_prefix(PULL)?;
    (!rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
        .then(|| rest.parse().ok())
        .flatten()
}

/// A pull request title that opens our page, and the GitHub mark after it.
pub(crate) fn title_link(n: u64, text: &str, class: &str) -> String {
    format!(
        "<a class=\"{class}\" href=\"/pr/{n}\">{text}</a>{}",
        github_icon(n)
    )
}

/// As `title_link` for any link: a nixpkgs pull request opens our page.
/// `text` is HTML.
pub(crate) fn link(url: &str, text: &str, class: &str) -> String {
    match number_of(url) {
        Some(n) => title_link(n, text, class),
        None => format!("<a class=\"{class}\" href=\"{}\">{text}</a>", esc(url)),
    }
}

struct Lru<V> {
    map: HashMap<String, (u64, usize, Arc<V>)>,
    seq: u64,
    bytes: usize,
}

impl<V> Default for Lru<V> {
    fn default() -> Self {
        Self {
            map: HashMap::new(),
            seq: 0,
            bytes: 0,
        }
    }
}

impl<V> Lru<V> {
    fn get(&mut self, key: &str) -> Option<Arc<V>> {
        self.seq += 1;
        let seq = self.seq;
        self.map.get_mut(key).map(|e| {
            e.0 = seq;
            e.2.clone()
        })
    }

    fn put(&mut self, key: String, value: Arc<V>, size: usize) {
        self.seq += 1;
        if let Some((_, old, _)) = self.map.insert(key, (self.seq, size, value)) {
            self.bytes -= old;
        }
        self.bytes += size;
        while self.bytes > CACHE_BYTES || self.map.len() > CACHE_ENTRIES {
            let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.0)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some((_, size, _)) = self.map.remove(&oldest) {
                self.bytes -= size;
            }
        }
    }
}

type Flights<V> = Mutex<HashMap<String, Arc<OnceCell<Result<Arc<V>, String>>>>>;

/// What the pages read from GitHub on demand, in memory only: the files per
/// head commit, the discussion per update.
#[derive(Default)]
pub struct Cache {
    files: Mutex<Lru<Files>>,
    talk: Mutex<Lru<Talk>>,
    files_flights: Flights<Files>,
    talk_flights: Flights<Talk>,
    budget: Budget,
}

/// Keeps live reads off the budget that the sync needs.
#[derive(Default)]
struct Budget {
    /// A token, per API, with less than `RESERVE` left, until its reset.
    low: Mutex<HashMap<u64, DateTime<Utc>>>,
    /// Live reads started in the current minute.
    window: Mutex<(Option<Instant>, usize)>,
}

impl Budget {
    fn key(api: &str, token: &str) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (api, token).hash(&mut h);
        h.finish()
    }

    fn usable(&self, api: &str, token: &str, now: DateTime<Utc>) -> bool {
        let mut low = self.low.lock().unwrap();
        low.retain(|_, until| *until > now);
        !low.contains_key(&Self::key(api, token))
    }

    fn note(&self, api: &str, token: &str, remaining: i64, reset: DateTime<Utc>) {
        if remaining < RESERVE {
            self.low
                .lock()
                .unwrap()
                .insert(Self::key(api, token), reset);
        }
    }

    fn admit(&self, now: Instant) -> bool {
        let mut w = self.window.lock().unwrap();
        if w.0
            .is_none_or(|start| now.duration_since(start) >= Duration::from_secs(60))
        {
            *w = (Some(now), 0);
        }
        w.1 += 1;
        w.1 <= LIVE_PER_MINUTE
    }
}

/// Runs `read` once for all the requests that miss the cache on one key
/// at the same time.
async fn joined<V>(
    flights: &Flights<V>,
    key: &str,
    read: impl std::future::Future<Output = Result<Arc<V>, String>>,
) -> Result<Arc<V>, String> {
    let cell = flights
        .lock()
        .unwrap()
        .entry(key.to_string())
        .or_default()
        .clone();
    let out = cell.get_or_init(|| read).await.clone();
    let mut map = flights.lock().unwrap();
    if map.get(key).is_some_and(|c| Arc::ptr_eq(c, &cell)) {
        map.remove(key);
    }
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct File {
    pub path: String,
    pub previous: Option<String>,
    /// added, removed, modified, renamed, copied, changed or unchanged.
    pub status: String,
    pub additions: u64,
    pub deletions: u64,
    /// None for a binary file, or a diff too large for the GitHub API.
    pub patch: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Files {
    pub head: String,
    pub files: Vec<File>,
    /// The pull request changes more files than were read.
    pub capped: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Note {
    /// review, comment or inline.
    pub kind: &'static str,
    pub author: String,
    pub at: DateTime<Utc>,
    pub url: String,
    pub body: String,
    pub state: Option<String>,
    pub path: Option<String>,
    pub line: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct Talk {
    pub body: String,
    pub notes: Vec<Note>,
    /// Earlier notes left out by `NOTE_CAP`.
    pub omitted: usize,
    /// archive or github.
    pub source: &'static str,
    pub updated_at: Option<DateTime<Utc>>,
}

fn nodes(v: &Value) -> impl Iterator<Item = &Value> {
    v["nodes"].as_array().into_iter().flatten()
}

fn time(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// The archive and the live query share GitHub's GraphQL shape.
fn talk_of(v: &Value, source: &'static str) -> Talk {
    let shown = |n: &&Value| n["isMinimized"].as_bool() != Some(true);
    let mut notes = Vec::new();
    for r in nodes(&v["reviews"]).filter(shown) {
        let state = text(&r["state"]);
        let body = text(&r["body"]);
        if let Some(at) = time(&r["submittedAt"]).or_else(|| time(&r["createdAt"])) {
            if !body.trim().is_empty() || state != "COMMENTED" {
                notes.push(Note {
                    kind: "review",
                    author: text(&r["author"]["login"]),
                    at,
                    url: text(&r["url"]),
                    body,
                    state: Some(state),
                    path: None,
                    line: None,
                });
            }
        }
        for c in nodes(&r["comments"]).filter(shown) {
            let Some(at) = time(&c["createdAt"]) else {
                continue;
            };
            notes.push(Note {
                kind: "inline",
                author: text(&c["author"]["login"]),
                at,
                url: text(&c["url"]),
                body: text(&c["body"]),
                state: None,
                path: c["path"].as_str().map(String::from),
                line: c["line"].as_u64().or_else(|| c["originalLine"].as_u64()),
            });
        }
    }
    for c in nodes(&v["comments"]).filter(shown) {
        let Some(at) = time(&c["createdAt"]) else {
            continue;
        };
        notes.push(Note {
            kind: "comment",
            author: text(&c["author"]["login"]),
            at,
            url: text(&c["url"]),
            body: text(&c["body"]),
            state: None,
            path: None,
            line: None,
        });
    }
    notes.sort_by_key(|n| n.at);
    let omitted = notes.len().saturating_sub(NOTE_CAP);
    notes.drain(..omitted);
    Talk {
        body: text(&v["body"]),
        notes,
        omitted,
        source,
        updated_at: time(&v["updatedAt"]),
    }
}

fn talk_size(t: &Talk) -> usize {
    t.body.len() + t.notes.iter().map(|n| n.body.len() + 200).sum::<usize>()
}

fn file_of(v: &Value) -> Option<File> {
    Some(File {
        path: v["filename"].as_str()?.to_string(),
        previous: v["previous_filename"].as_str().map(String::from),
        status: text(&v["status"]),
        additions: v["additions"].as_u64().unwrap_or(0),
        deletions: v["deletions"].as_u64().unwrap_or(0),
        patch: v["patch"].as_str().map(String::from),
    })
}

fn files_size(f: &Files) -> usize {
    f.files
        .iter()
        .map(|f| f.path.len() + f.patch.as_ref().map_or(0, String::len) + 100)
        .sum()
}

const NOT_LIVE: &str = "this server reads GitHub only for people who sign in";
const BUSY: &str = "this server reads GitHub for many pages now; try again in a minute";
const LOW: &str = "this server keeps its GitHub budget for the sync now";

fn header_i64(resp: &reqwest::Response, name: &str) -> Option<i64> {
    resp.headers().get(name)?.to_str().ok()?.parse().ok()
}

/// Each token with budget left in turn, then no token: the REST API
/// answers public data without one, at a lower rate.
async fn rest_get(
    http: &reqwest::Client,
    budget: &Budget,
    tokens: &[String],
    url: &str,
) -> Result<Value> {
    let mut last = String::new();
    let usable = tokens
        .iter()
        .filter(|t| budget.usable("rest", t, Utc::now()));
    for token in usable.map(Some).chain([None]) {
        let mut req = http
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await?;
        if let (Some(t), Some(left), Some(reset)) = (
            token,
            header_i64(&resp, "x-ratelimit-remaining"),
            header_i64(&resp, "x-ratelimit-reset").and_then(|s| DateTime::from_timestamp(s, 0)),
        ) {
            budget.note("rest", t, left, reset);
        }
        let status = resp.status();
        if status.is_success() {
            return resp.json().await.context("GitHub REST answer");
        }
        if !matches!(status.as_u16(), 401 | 403 | 429) {
            bail!("GitHub answered {status}");
        }
        last = format!("GitHub answered {status}");
    }
    bail!("{last}")
}

async fn fetch_files(api: &str, budget: &Budget, tokens: &[String], n: u64) -> Result<Vec<File>> {
    let http = crate::github::http();
    let mut out = Vec::new();
    for page in 1.. {
        let url =
            format!("{api}/repos/NixOS/nixpkgs/pulls/{n}/files?per_page={PER_PAGE}&page={page}");
        let v = rest_get(&http, budget, tokens, &url).await?;
        let list = v.as_array().context("a list of files")?;
        out.extend(list.iter().filter_map(file_of));
        if list.len() < PER_PAGE || out.len() >= FILE_CAP {
            break;
        }
    }
    out.truncate(FILE_CAP);
    Ok(out)
}

const TALK_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!, $first: Int!) {
  rateLimit { cost remaining resetAt }
  repository(owner: $owner, name: $name) { pullRequest(number: $number) {
    updatedAt body
    reviews(first: $first) { nodes { url state submittedAt createdAt body isMinimized author { login }
      comments(first: $first) { nodes { url createdAt body path line originalLine isMinimized author { login } } } } }
    comments(first: $first) { nodes { url createdAt body isMinimized author { login } } }
  } }
}";

async fn fetch_talk(budget: &Budget, tokens: Vec<String>, n: u64) -> Result<Talk> {
    if tokens.is_empty() {
        bail!("this server has no GitHub token of its own to read it with");
    }
    let http = crate::github::http();
    let vars = json!({ "owner": "NixOS", "name": "nixpkgs", "number": n, "first": FIRST });
    let mut last = LOW.to_string();
    for token in tokens
        .iter()
        .filter(|t| budget.usable("graphql", t, Utc::now()))
    {
        let data = match crate::github::graphql_once(&http, token, TALK_QUERY, &vars).await? {
            crate::github::Outcome::Data(v) => v,
            crate::github::Outcome::BadToken(_) => {
                last = "GitHub refused the token of this server".into();
                continue;
            }
        };
        let rate = &data["rateLimit"];
        if let (Some(left), Some(reset)) = (rate["remaining"].as_i64(), time(&rate["resetAt"])) {
            budget.note("graphql", token, left, reset);
        }
        let pr = &data["repository"]["pullRequest"];
        if pr.is_null() {
            bail!("GitHub has no pull request #{n}");
        }
        return Ok(talk_of(pr, "github"));
    }
    bail!("{last}")
}

async fn timed<T>(f: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(Duration::from_secs(FETCH_SECS), f)
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("GitHub did not answer in {FETCH_SECS} s")))
}

/// `live` lets a miss of the cache read GitHub; anonymous visitors and the
/// API never spend the server's budget.
async fn files(app: &App, pr: &Pr, live: bool) -> Result<Arc<Files>, String> {
    let key = format!("{}@{}", pr.number, pr.head_oid);
    let cache = &app.pr_cache;
    if let Some(f) = cache.files.lock().unwrap().get(&key) {
        return Ok(f);
    }
    if !live {
        return Err(NOT_LIVE.into());
    }
    joined(&cache.files_flights, &key, async {
        if let Some(f) = cache.files.lock().unwrap().get(&key) {
            return Ok(f);
        }
        if !cache.budget.admit(Instant::now()) {
            return Err(BUSY.into());
        }
        let tokens = crate::archive::tokens(app).await;
        let list = timed(fetch_files(API, &cache.budget, &tokens, pr.number))
            .await
            .map_err(|e| format!("{e:#}"))?;
        let files = Files {
            head: pr.head_oid.clone(),
            capped: (list.len() as u64) < pr.changed_files,
            files: list,
        };
        let size = files_size(&files);
        let files = Arc::new(files);
        cache
            .files
            .lock()
            .unwrap()
            .put(key.clone(), files.clone(), size);
        Ok(files)
    })
    .await
}

async fn archived(app: &App, n: u64) -> Option<Talk> {
    let a = app
        .archive
        .iter()
        .find(|a| a.repo.eq_ignore_ascii_case(crate::archive::DEFAULT_REPO))
        .filter(|a| !a.is_private())?
        .clone();
    tokio::task::spawn_blocking(move || {
        let json = a.item(n).ok()??.json?;
        let v: Value = serde_json::from_str(&json).ok()?;
        (v["__typename"] == "PullRequest").then(|| talk_of(&v, "archive"))
    })
    .await
    .ok()?
}

/// The archive copy, parsed once per update and kept in the cache.
async fn stored(app: &App, pr: &Pr, key: &str) -> Option<Arc<Talk>> {
    let key = format!("archive:{key}");
    if let Some(t) = app.pr_cache.talk.lock().unwrap().get(&key) {
        return Some(t);
    }
    let t = archived(app, pr.number).await?;
    let size = talk_size(&t);
    let t = Arc::new(t);
    app.pr_cache.talk.lock().unwrap().put(key, t.clone(), size);
    Some(t)
}

/// The archive when it is as new as the synced data, else a live read; a
/// stale archive copy is still better than nothing when the read fails.
async fn talk(app: &App, pr: &Pr, live: bool) -> Result<Arc<Talk>, String> {
    let key = format!("{}@{}", pr.number, pr.updated_at.to_rfc3339());
    let cache = &app.pr_cache;
    if let Some(t) = cache.talk.lock().unwrap().get(&key) {
        return Ok(t);
    }
    let stored = stored(app, pr, &key).await;
    if let Some(t) = &stored {
        if !live || t.updated_at.is_some_and(|u| u >= pr.updated_at) {
            return Ok(t.clone());
        }
    }
    if !live {
        return Err(NOT_LIVE.into());
    }
    let read = joined(&cache.talk_flights, &key, async {
        if let Some(t) = cache.talk.lock().unwrap().get(&key) {
            return Ok(t);
        }
        if !cache.budget.admit(Instant::now()) {
            return Err(BUSY.into());
        }
        let tokens = crate::archive::tokens(app).await;
        let t = timed(fetch_talk(&cache.budget, tokens, pr.number))
            .await
            .map_err(|e| format!("{e:#}"))?;
        let size = talk_size(&t);
        let t = Arc::new(t);
        cache.talk.lock().unwrap().put(key.clone(), t.clone(), size);
        Ok(t)
    })
    .await;
    read.or_else(|e| stored.ok_or(e))
}

fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Removes `<!-- … -->`, which may span several events; `open` carries the
/// state from one event to the next.
fn strip_comments(s: &str, open: &mut bool) -> String {
    let mut out = String::new();
    let mut rest = s;
    loop {
        if *open {
            match rest.find("-->") {
                Some(i) => {
                    rest = &rest[i + 3..];
                    *open = false;
                }
                None => return out,
            }
        } else {
            match rest.find("<!--") {
                Some(i) => {
                    out.push_str(&rest[..i]);
                    rest = &rest[i + 4..];
                    *open = true;
                }
                None => {
                    out.push_str(rest);
                    return out;
                }
            }
        }
    }
}

/// Browsers drop whitespace and control characters inside a scheme, so the
/// check reads the URL without them.
fn safe_url(u: pulldown_cmark::CowStr<'_>) -> pulldown_cmark::CowStr<'_> {
    let clean: String = u
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect::<String>()
        .to_lowercase();
    let scheme = clean
        .split_once(':')
        .map(|(s, _)| s)
        .filter(|s| !s.contains(['/', '?', '#']));
    match scheme {
        None | Some("http" | "https" | "mailto") => u,
        Some(_) => "#".into(),
    }
}

/// Markdown written by anyone on GitHub: raw HTML shows as text, HTML
/// comments go, only web and mail links stay, and an image becomes a link so
/// the page loads nothing from elsewhere.
pub(crate) fn markdown(text: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut open = false;
    let events = Parser::new_ext(cut(text, BODY_CAP), opts).filter_map(|e| match e {
        Event::Html(t) | Event::InlineHtml(t) => {
            let kept = strip_comments(&t, &mut open);
            (!kept.trim().is_empty()).then(|| Event::Text(kept.into()))
        }
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        })
        | Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Some(Event::Start(Tag::Link {
            link_type,
            dest_url: safe_url(dest_url),
            title,
            id,
        })),
        Event::End(TagEnd::Image) => Some(Event::End(TagEnd::Link)),
        e => Some(e),
    });
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, events);
    html
}

#[derive(Debug, Serialize)]
pub struct Place {
    /// merge, review, or none.
    pub queue: &'static str,
    pub unowned: bool,
    pub why: Vec<String>,
    pub approved_by: Vec<String>,
    pub credited_by: Vec<String>,
}

fn place(s: &Snapshot, pr: &Pr) -> Place {
    let find = |q: &[Entry]| q.iter().find(|e| e.number == pr.number).cloned();
    let merge = find(&s.queues.merge);
    let review = find(&s.queues.review);
    let queue = match (&merge, &review) {
        (Some(_), _) => "merge",
        (None, Some(_)) => "review",
        _ => "none",
    };
    let e = merge
        .or(review)
        .unwrap_or_else(|| queue::entry(pr, &s.ledger, &queue::credited(&s.ledger)));
    let mut why: Vec<String> = Vec::new();
    match queue {
        "merge" => why.push(
            "checks pass, it has no conflict, the current head is approved, and a review on it earned credit"
                .into(),
        ),
        "review" => {
            match e.ci.as_deref() {
                Some("SUCCESS") => {}
                Some("FAILURE" | "ERROR") => why.push("checks fail".into()),
                _ => why.push("checks are pending".into()),
            }
            match e.mergeable.as_str() {
                "CONFLICTING" => why.push("it has a conflict with its base branch".into()),
                "UNKNOWN" => {
                    why.push("GitHub has not said yet whether it merges cleanly".into())
                }
                _ => {}
            }
            if e.approved_by.is_empty() {
                why.push("nobody approved the current head commit".into());
            }
            if e.credited_by.is_empty() {
                why.push("no review on it earned credit yet".into());
            }
        }
        _ => {
            if pr.state == "MERGED" {
                why.push("it is merged".into());
            } else if !pr.is_open() {
                why.push("it is closed".into());
            }
            if pr.is_draft {
                why.push("it is a draft".into());
            }
            if pr.author_is_bot {
                why.push("a bot opened it".into());
            } else if !s.ledger.is_listed(&pr.author) {
                let p = s.ledger.people.get(&pr.author);
                why.push(format!(
                    "its author is not listed: credit from {} of {} authors",
                    p.map_or(0, |p| p.counterparties),
                    ledger::MIN_COUNTERPARTIES
                ));
            }
        }
    }
    Place {
        queue,
        unowned: e.unowned,
        why,
        approved_by: e.approved_by,
        credited_by: e.credited_by,
    }
}

pub(crate) struct Touched {
    pub subject: Subject,
    pub flags: Flags,
    pub maintainers: Vec<String>,
    pub teams: Vec<String>,
    /// Maintainers of it whose approval is on the current head.
    pub approved_by: Vec<String>,
}

impl Touched {
    fn json(&self) -> Value {
        json!({
            "key": self.subject.key(),
            "name": self.subject.label(),
            "href": self.subject.href(),
            "flags": self.flags.json(),
            "maintainers": self.maintainers,
            "teams": self.teams,
            "maintainer_approved": self.approved_by,
        })
    }
}

fn touched(s: &Snapshot, index: &Index, h: &crate::health::Health, pr: &Pr) -> Vec<Touched> {
    let mut keys: Vec<&String> = s
        .links
        .prs
        .iter()
        .filter(|(_, ns)| ns.contains(&pr.number))
        .map(|(k, _)| k)
        .collect();
    keys.sort();
    let approvals = crate::committer::approvals(pr);
    keys.into_iter()
        .filter_map(|k| Subject::parse(k))
        .take(SUBJECTS)
        .map(|subject| {
            let (maintainers, teams) = match &subject {
                Subject::Pkg(a) => index
                    .pkgs
                    .get(a)
                    .map(|p| (p.maintainers.clone(), p.teams.clone())),
                Subject::Module(f) => index
                    .modules
                    .get(f)
                    .map(|m| (m.maintainers.clone(), m.teams.clone())),
                Subject::Area(_) => None,
            }
            .unwrap_or_default();
            let all: Vec<String> = maintainers
                .iter()
                .cloned()
                .chain(index.team_members(&teams))
                .map(|l| l.to_lowercase())
                .collect();
            let mut approved_by: Vec<String> = approvals
                .iter()
                .filter(|(l, _)| all.contains(&l.to_lowercase()))
                .map(|(l, _)| l.to_string())
                .collect();
            approved_by.dedup();
            Touched {
                flags: Flags::of(s, index, h, &subject),
                subject,
                maintainers,
                teams,
                approved_by,
            }
        })
        .collect()
}

fn ai_text(ai: Option<&str>) -> String {
    match ai {
        None => "none disclosed in the last commit or the description".into(),
        Some(crate::github::OTHER_FORM) => {
            "attributed in a form the nixpkgs policy does not accept: not an <code>Assisted-by:</code> trailer"
                .into()
        }
        Some("disclosed") => "disclosed".into(),
        Some(t) => format!("disclosed: {}", esc(t)),
    }
}

fn state_label(pr: &Pr) -> String {
    let (ic, class, word) = match pr.state.as_str() {
        "MERGED" => (icon(octicon!("git-merge")), "Label--success", "merged"),
        "CLOSED" => (
            icon(octicon!("git-pull-request-closed")),
            "Label--danger",
            "closed",
        ),
        _ => (icon(octicon!("git-pull-request")), "Label--success", "open"),
    };
    let draft = if pr.is_draft {
        " <span class=\"Label\">draft</span>"
    } else {
        ""
    };
    format!("<span class=\"Label {class}\">{ic} {word}</span>{draft}")
}

fn ci_text(ci: Option<&str>) -> String {
    match ci {
        Some("SUCCESS") => format!(
            "<span class=\"color-fg-success\">{} checks pass</span>",
            icon(octicon!("check-circle-fill"))
        ),
        Some("FAILURE" | "ERROR") => format!(
            "<span class=\"color-fg-danger\">{} checks fail</span>",
            icon(octicon!("x-circle-fill"))
        ),
        Some(_) => format!(
            "<span class=\"color-fg-attention\">{} checks pending</span>",
            icon(octicon!("dot-fill"))
        ),
        None => "<span class=\"color-fg-muted\">no checks reported</span>".into(),
    }
}

fn mergeable_text(m: &str) -> &'static str {
    match m {
        "MERGEABLE" => "<span class=\"color-fg-success\">merges cleanly</span>",
        "CONFLICTING" => "<span class=\"Label Label--danger\">conflict</span>",
        _ => "<span class=\"color-fg-muted\">not known yet</span>",
    }
}

fn people(logins: &[String]) -> String {
    if logins.is_empty() {
        return "<span class=\"color-fg-muted\">none</span>".into();
    }
    logins
        .iter()
        .map(|l| format!("<a href=\"/u/{0}\">{0}</a>", esc(l)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn about(pr: &Pr) -> String {
    let labels: String = if pr.labels.is_empty() {
        "<span class=\"color-fg-muted\">none</span>".into()
    } else {
        pr.labels
            .iter()
            .map(|l| format!("<span class=\"Label mr-1\">{}</span>", esc(l)))
            .collect()
    };
    let when = match (pr.merged_at, pr.closed_at) {
        (Some(t), _) => format!(
            "{} ago, merged {} ago{}",
            ago(pr.created_at),
            ago(t),
            pr.merged_by
                .as_deref()
                .map(|m| format!(" by <a href=\"/u/{0}\">{0}</a>", esc(m)))
                .unwrap_or_default()
        ),
        (None, Some(t)) => format!("{} ago, closed {} ago", ago(pr.created_at), ago(t)),
        _ => format!(
            "{} ago, updated {} ago",
            ago(pr.created_at),
            ago(pr.updated_at)
        ),
    };
    let mut out = String::from(
        "<div class=\"Box mb-4\"><div class=\"Box-header\"><h2 class=\"Box-title\">About</h2></div>",
    );
    out.push_str(&kv("State", &state_label(pr)));
    out.push_str(&kv(
        "Author",
        &format!(
            "<a href=\"/u/{a}\">{a}</a>{}",
            if pr.association.is_empty() {
                String::new()
            } else {
                format!(
                    " <span class=\"color-fg-muted f6\">{}</span>",
                    esc(&pr.association.to_lowercase().replace('_', " "))
                )
            },
            a = esc(&pr.author)
        ),
    ));
    out.push_str(&kv(
        "Base",
        &format!("<span class=\"branch-name\">{}</span>", esc(&pr.base)),
    ));
    out.push_str(&kv("Labels", &labels));
    out.push_str(&kv("Age", &when));
    out.push_str(&kv(
        "Size",
        &format!(
            "<span class=\"color-fg-success\">+{}</span> <span class=\"color-fg-danger\">−{}</span> in {} file{}",
            pr.additions,
            pr.deletions,
            pr.changed_files,
            if pr.changed_files == 1 { "" } else { "s" }
        ),
    ));
    if pr.is_open() {
        out.push_str(&kv("Checks", &ci_text(pr.ci.as_deref())));
        out.push_str(&kv("Mergeable", mergeable_text(&pr.mergeable)));
    }
    out.push_str(&kv("AI use", &ai_text(pr.ai.as_deref())));
    out.push_str("</div>");
    out
}

fn place_html(p: &Place, credits: &[&Credit], for_you: bool) -> String {
    let mut out = String::from(
        "<div class=\"Box mb-4\"><div class=\"Box-header\"><h2 class=\"Box-title\">In Maintainer Mode</h2></div>",
    );
    let queue = match p.queue {
        "merge" => "<a href=\"/review?tab=ready\">Ready to merge</a>".to_string(),
        "review" => {
            let mut buckets = Vec::new();
            if for_you {
                buckets.push("<a href=\"/review?tab=foryou\">for you</a>");
            }
            if p.unowned {
                buckets.push("<a href=\"/review?tab=unowned\">no maintainer</a>");
            }
            buckets.push("<a href=\"/review?tab=all\">all</a>");
            format!("Needs review: {}", buckets.join(", "))
        }
        _ => "<span class=\"color-fg-muted\">in no queue</span>".to_string(),
    };
    out.push_str(&kv("Queue", &queue));
    if !p.why.is_empty() {
        out.push_str(&kv(
            if p.queue == "merge" {
                "Why"
            } else {
                "Why not ready"
            },
            &esc(&p.why.join("; ")),
        ));
    }
    if p.unowned {
        out.push_str(&kv(
            "Unowned",
            "no <code>meta.maintainers</code>, <code>meta.teams</code> or <code>ci/OWNERS</code> line covers its files. \
             A review here earns full credit at any age.",
        ));
    }
    out.push_str(&kv("Approved head", &people(&p.approved_by)));
    let mut rows = String::new();
    for c in credits {
        let status = match &c.dropped {
            None => format!("{:.2}", c.value),
            Some(Dropped::DailyCap) => "over the daily cap".into(),
            Some(Dropped::Voided { by, url }) => format!(
                "<a class=\"Label Label--danger\" href=\"{}\">voided by {}</a>",
                esc(url),
                esc(by)
            ),
        };
        let _ = write!(
            rows,
            "<div><a href=\"/u/{r}\">{r}</a> · <a href=\"{u}\">review</a> {} ago · {status}</div>",
            ago(c.at),
            r = esc(&c.reviewer),
            u = esc(&c.review_url),
        );
    }
    if rows.is_empty() {
        rows = "<span class=\"color-fg-muted\">none yet</span>".into();
    }
    out.push_str(&kv(
        "Credit earned",
        &format!("{rows}<div class=\"f6 color-fg-muted\"><a href=\"/rules#the-rules\">How credit works</a></div>"),
    ));
    out.push_str("</div>");
    out
}

fn touched_html(list: &[Touched]) -> String {
    let mut out = String::from(
        "<div class=\"Box mb-4\"><div class=\"Box-header\"><h2 class=\"Box-title\">What it touches</h2></div>",
    );
    if list.is_empty() {
        out.push_str(
            "<div class=\"Box-row color-fg-muted\">No package or NixOS module in the index matches its files or its title.</div>",
        );
    }
    for t in list {
        let approved = if t.approved_by.is_empty() {
            String::new()
        } else {
            format!(
                " · <span class=\"color-fg-success\">{} approved by {}</span>",
                icon(octicon!("check-circle-fill")),
                people(&t.approved_by)
            )
        };
        let teams = if t.teams.is_empty() {
            String::new()
        } else {
            format!(
                " · teams {}",
                t.teams
                    .iter()
                    .map(|x| esc(x))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let _ = write!(
            out,
            "<div class=\"Box-row\"><div class=\"d-flex flex-items-center flex-wrap gap-2\">\
             <a class=\"Link--primary text-bold\" href=\"{}\">{} {}</a> {}</div>\
             <div class=\"f6 color-fg-muted mt-1\">maintainers {}{teams}{approved}</div></div>",
            t.subject.href(),
            t.subject.icon(),
            esc(&t.subject.label()),
            t.flags.html(),
            people(&t.maintainers),
        );
    }
    out.push_str("</div>");
    out
}

fn checklist(pr: &Pr, act: Act) -> String {
    let items: [(&str, String); 7] = [
        (
            "The commit messages follow the nixpkgs format, like <code>hello: 2.12 -&gt; 2.13</code> or <code>nixos/nginx: fix the test</code>.",
            format!("{NIXPKGS}/CONTRIBUTING.md#commit-conventions"),
        ),
        (
            "<code>meta</code> is complete: <code>description</code>, <code>homepage</code>, <code>license</code>, <code>maintainers</code>, <code>mainProgram</code>.",
            format!("{NIXPKGS}/pkgs/README.md#meta-attributes"),
        ),
        (
            "Sources are fixed-output and pinned by a hash. No <code>lib.fakeHash</code> is left.",
            format!("{NIXPKGS}/pkgs/README.md#sources"),
        ),
        (
            "A command-line tool has <code>versionCheckHook</code>, or another check that runs it.",
            "https://nixos.org/manual/nixpkgs/unstable/#versioncheckhook".to_string(),
        ),
        (
            "<code>meta.platforms</code> matches what upstream supports.",
            format!("{NIXPKGS}/pkgs/README.md#meta-attributes"),
        ),
        (
            "The tests run: <code>passthru.tests</code>, the NixOS test of a module, or nixpkgs-review.",
            format!("{NIXPKGS}/pkgs/README.md#package-tests"),
        ),
        (
            "AI use is disclosed with an <code>Assisted-by:</code> trailer.",
            format!("{NIXPKGS}/CONTRIBUTING.md#automationai-policy"),
        ),
    ];
    let mut out = format!(
        "<div class=\"Box mb-4\"><div class=\"Box-header\"><h2 class=\"Box-title d-inline\">{} Review checklist</h2> \
         <span class=\"f6 text-normal color-fg-muted\">from <a href=\"{NIXPKGS}/CONTRIBUTING.md#how-to-review-pull-requests\">CONTRIBUTING.md</a> \
         and <a href=\"{NIXPKGS}/pkgs/README.md#reviewing-contributions\">pkgs/README.md</a></span></div><div class=\"Box-row\"><ul class=\"ml-3\">",
        icon(octicon!("checklist"))
    );
    for (text, href) in &items {
        let _ = write!(out, "<li>{text} <a href=\"{}\">Guide</a></li>", esc(href));
    }
    out.push_str("</ul>");
    let title_ok = !crate::subject::of_title(&pr.title).is_empty();
    let _ = write!(
        out,
        "<p class=\"f6 color-fg-muted mt-2\">The title {}. AI use: {}.</p></div>",
        if title_ok {
            "names what it changes"
        } else {
            "does not start with <code>name:</code>"
        },
        ai_text(pr.ai.as_deref())
    );
    let buttons = if pr.is_open() {
        act.buttons(pr.number)
    } else {
        String::new()
    };
    let _ = write!(
        out,
        "<div class=\"Box-row d-flex flex-items-center flex-wrap gap-2\">{}{buttons}</div>\
         <div class=\"Box-footer f6 color-fg-muted\">Maintainer Mode posts nothing on nixpkgs. You leave your review on GitHub.</div></div>",
        exits(pr.number),
    );
    out
}

fn exits(n: u64) -> String {
    format!(
        "<a class=\"btn btn-sm btn-primary\" href=\"{u}/files\">{} Review on GitHub</a>\
         <a class=\"btn btn-sm\" href=\"{u}\">{} Open on GitHub</a>",
        icon(octicon!("file-diff")),
        icon(octicon!("mark-github")),
        u = github_url(n),
    )
}

/// `diff-<sha256 of the path>` is GitHub's anchor of one file in the diff.
fn file_anchor(n: u64, path: &str) -> String {
    use sha2::Digest;
    let hash: String = sha2::Sha256::digest(path.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{}/files#diff-{hash}", github_url(n))
}

fn patch_html(patch: &str) -> String {
    let mut out = String::from("<pre class=\"diff\">");
    for line in patch.lines() {
        let class = match line.as_bytes().first() {
            Some(b'+') => "diff-add",
            Some(b'-') => "diff-del",
            Some(b'@') => "diff-hunk",
            Some(b'\\') => "diff-note",
            _ => "",
        };
        let _ = write!(
            out,
            "<span class=\"{class}\">{}</span>",
            if line.is_empty() {
                " ".into()
            } else {
                esc(line)
            }
        );
    }
    out.push_str("</pre>");
    out
}

fn file_html(n: u64, f: &File) -> String {
    let lines = f.patch.as_deref().map_or(0, |p| p.lines().count());
    let name = match &f.previous {
        Some(p) if p != &f.path => format!("{} → {}", esc(p), esc(&f.path)),
        _ => esc(&f.path),
    };
    let link = file_anchor(n, &f.path);
    let body = match &f.patch {
        None => format!(
            "<div class=\"Box-row color-fg-muted\">No diff here: a binary file, or a diff too large for the GitHub API. <a href=\"{link}\">See it on GitHub</a></div>"
        ),
        Some(_) if lines > PATCH_LINES => format!(
            "<div class=\"Box-row color-fg-muted\">A large diff of {lines} lines. <a href=\"{link}\">See it on GitHub</a></div>"
        ),
        Some(p) => patch_html(p),
    };
    format!(
        "<details class=\"Box mb-3\"{open}><summary class=\"Box-header d-flex flex-items-center gap-2\">\
         <span class=\"Label\">{status}</span><span class=\"text-mono f6 flex-auto\">{name}</span>\
         <span class=\"color-fg-success f6\">+{a}</span><span class=\"color-fg-danger f6\">−{d}</span>\
         <a class=\"f6\" href=\"{link}\">GitHub</a></summary>{body}</details>",
        open = if lines <= FOLD_LINES { " open" } else { "" },
        status = esc(&f.status),
        a = f.additions,
        d = f.deletions,
    )
}

/// Why a read failed, with a way to sign in when that is the reason.
fn why(e: &str) -> String {
    if e == NOT_LIVE {
        format!("{} (<a href=\"/auth/login\">sign in</a>)", esc(e))
    } else {
        esc(e)
    }
}

fn files_html(pr: &Pr, files: &Result<Arc<Files>, String>) -> String {
    let mut out = format!(
        "<h2 class=\"f4 mt-4 mb-2\" id=\"files\">{} Files changed <span class=\"Counter\">{}</span></h2>",
        icon(octicon!("file-diff")),
        pr.changed_files
    );
    match files {
        Err(e) => {
            let _ = write!(
                out,
                "<div class=\"flash mb-4\">The diff could not be read: {}. <a href=\"{}/files\">Read it on GitHub</a></div>",
                why(e),
                github_url(pr.number)
            );
        }
        Ok(f) => {
            if f.files.is_empty() {
                out.push_str("<div class=\"blankslate\"><p>No changed files.</p></div>");
            }
            for file in &f.files {
                out.push_str(&file_html(pr.number, file));
            }
            if f.capped {
                let _ = write!(
                    out,
                    "<div class=\"flash mb-4\">This page shows the first {} files. <a href=\"{}/files\">See all on GitHub</a></div>",
                    f.files.len(),
                    github_url(pr.number)
                );
            }
        }
    }
    out
}

fn description_html(pr: &Pr, talk: &Result<Arc<Talk>, String>) -> String {
    let body = match talk {
        Ok(t) if t.body.trim().is_empty() => {
            "<p class=\"color-fg-muted\">No description.</p>".to_string()
        }
        Ok(t) => markdown(&t.body),
        Err(e) => format!(
            "<p class=\"color-fg-muted\">The description could not be read: {}. <a href=\"{}\">Read it on GitHub</a></p>",
            why(e),
            github_url(pr.number)
        ),
    };
    format!(
        "<div class=\"Box mb-4\"><div class=\"Box-header\"><h2 class=\"Box-title\">Description</h2></div>\
         <div class=\"Box-row markdown-body\">{body}</div></div>"
    )
}

fn note_html(n: &Note) -> String {
    let what = match n.kind {
        "review" => {
            let s = n.state.as_deref().unwrap_or("");
            let label = match s {
                "APPROVED" => "<span class=\"Label Label--success\">approved</span>".to_string(),
                "CHANGES_REQUESTED" => {
                    "<span class=\"Label Label--danger\">changes requested</span>".to_string()
                }
                _ => format!(
                    "<span class=\"Label\">{}</span>",
                    esc(&s.to_lowercase().replace('_', " "))
                ),
            };
            format!("reviewed {label}")
        }
        "inline" => format!(
            "commented on <span class=\"text-mono\">{}{}</span>",
            esc(n.path.as_deref().unwrap_or("")),
            n.line.map(|l| format!(":{l}")).unwrap_or_default()
        ),
        _ => "commented".to_string(),
    };
    let body = if n.body.trim().is_empty() {
        String::new()
    } else {
        format!(
            "<div class=\"markdown-body mt-2\">{}</div>",
            markdown(&n.body)
        )
    };
    format!(
        "<div class=\"Box-row\"><div class=\"f6\"><a class=\"text-bold\" href=\"/u/{a}\">{a}</a> {what} \
         <a class=\"color-fg-muted\" href=\"{u}\">{} ago</a></div>{body}</div>",
        ago(n.at),
        a = esc(&n.author),
        u = esc(&n.url),
    )
}

fn talk_html(pr: &Pr, talk: &Result<Arc<Talk>, String>) -> String {
    let mut out = format!(
        "<h2 class=\"f4 mt-4 mb-2\" id=\"discussion\">{} Reviews and comments</h2><div class=\"Box mb-4\">",
        icon(octicon!("comment"))
    );
    match talk {
        Ok(t) => {
            if t.notes.is_empty() {
                out.push_str(
                    "<div class=\"Box-row color-fg-muted\">No review or comment yet.</div>",
                );
            }
            if t.omitted > 0 {
                let _ = write!(
                    out,
                    "<div class=\"Box-row color-fg-muted\">{} earlier reviews and comments are not shown. <a href=\"{}\">Read them on GitHub</a></div>",
                    t.omitted,
                    github_url(pr.number)
                );
            }
            for n in &t.notes {
                out.push_str(&note_html(n));
            }
            let _ = write!(
                out,
                "<div class=\"Box-footer f6 color-fg-muted\">{}. <a href=\"{}\">The whole conversation on GitHub</a></div>",
                if t.source == "archive" {
                    "From the archive of this server"
                } else {
                    "Read from GitHub: the first 100 reviews and comments of each kind"
                },
                github_url(pr.number)
            );
        }
        Err(e) => {
            let _ = write!(
                out,
                "<div class=\"Box-row color-fg-muted\">The conversation could not be read: {}. The synced reviews, without their text:</div>",
                why(e)
            );
            for r in &pr.reviews {
                let _ = write!(
                    out,
                    "<div class=\"Box-row f6\"><a href=\"/u/{a}\">{a}</a> {} <a class=\"color-fg-muted\" href=\"{}\">{} ago</a></div>",
                    esc(&r.state.to_lowercase().replace('_', " ")),
                    esc(&r.url),
                    ago(r.submitted_at),
                    a = esc(&r.author)
                );
            }
            let _ = write!(
                out,
                "<div class=\"Box-footer f6\"><a href=\"{}\">The conversation on GitHub</a></div>",
                github_url(pr.number)
            );
        }
    }
    out.push_str("</div>");
    out
}

fn not_synced(app: &App, viewer: Option<&str>, n: u64) -> Response {
    let body = format!(
        "<div class=\"blankslate\"><p>Pull request #{n} is not in the synced data: \
         the sync keeps open pull requests and those updated in its window.</p>\
         <p class=\"d-flex flex-justify-center gap-2\">{}</p></div>",
        exits(n)
    );
    Page::new(Section::Review, format!("Pull request #{n}"))
        .sub("NixOS/nixpkgs")
        .body(body)
        .render(app, viewer)
}

async fn pr_page(State(app): State<Shared>, headers: HeaderMap, Path(n): Path<u64>) -> Response {
    let viewer = app.viewer(&headers);
    let s = app.snap();
    let Some(pr) = s.pr_at.get(&n).map(|&i| &s.pr_data[i]) else {
        return not_synced(&app, viewer.as_deref(), n);
    };
    let live = viewer.is_some();
    let (files, talk) = tokio::join!(files(&app, pr, live), talk(&app, pr, live));
    let h = app.health.read().unwrap().clone();
    let list = touched(&s, &app.index.read().unwrap(), &h, pr);
    let place = place(&s, pr);
    let for_you = place.queue == "review"
        && viewer.as_deref().is_some_and(|v| {
            let watched = crate::web::watched_of(&app, Some(v));
            crate::web::review_bucket(&s, Some(v), &watched, "foryou")
                .iter()
                .any(|e| e.number == n)
        });
    let credits: Vec<&Credit> = s.ledger.credits.iter().filter(|c| c.pr == n).collect();
    let act = Act::of(&app, viewer.as_deref());
    let page = Page::new(Section::Review, format!("#{n} {}", pr.title))
        .heading(format!(
            "{} <span class=\"color-fg-muted\">#{n}</span>{}",
            esc(&pr.title),
            github_icon(n)
        ))
        .sub(format!(
            "{} <a href=\"/u/{a}\">{a}</a> wants to merge into <span class=\"branch-name\">{}</span> · opened {} ago",
            state_label(pr),
            esc(&pr.base),
            ago(pr.created_at),
            a = esc(&pr.author)
        ))
        .actions(exits(n));
    let mut out = about(pr);
    out.push_str(&place_html(&place, &credits, for_you));
    out.push_str(&touched_html(&list));
    out.push_str(&description_html(pr, &talk));
    out.push_str(&checklist(pr, act));
    out.push_str(&files_html(pr, &files));
    out.push_str(&talk_html(pr, &talk));
    page.body(out).render(&app, viewer.as_deref())
}

/// The same data as the page, with nothing that depends on who asks.
async fn api_pr(State(app): State<Shared>, Path(n): Path<u64>) -> Response {
    let s = app.snap();
    let Some(pr) = s.pr_at.get(&n).map(|&i| &s.pr_data[i]) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": format!("pull request #{n} is not in the synced data"),
                "url": github_url(n),
            })),
        )
            .into_response();
    };
    let (files, talk) = tokio::join!(files(&app, pr, false), talk(&app, pr, false));
    let h = app.health.read().unwrap().clone();
    let list = touched(&s, &app.index.read().unwrap(), &h, pr);
    let credits: Vec<&Credit> = s.ledger.credits.iter().filter(|c| c.pr == n).collect();
    let (files, files_error) = match files {
        Ok(f) => (json!(*f), Value::Null),
        Err(e) => (Value::Null, json!(e)),
    };
    let (talk, talk_error) = match talk {
        Ok(t) => (json!(*t), Value::Null),
        Err(e) => (Value::Null, json!(e)),
    };
    Json(json!({
        "number": pr.number,
        "title": pr.title,
        "url": pr.url,
        "author": pr.author,
        "state": pr.state,
        "draft": pr.is_draft,
        "base": pr.base,
        "labels": pr.labels,
        "created_at": pr.created_at,
        "updated_at": pr.updated_at,
        "merged_at": pr.merged_at,
        "closed_at": pr.closed_at,
        "additions": pr.additions,
        "deletions": pr.deletions,
        "changed_files": pr.changed_files,
        "ci": pr.ci,
        "mergeable": pr.mergeable,
        "head": pr.head_oid,
        "ai": pr.ai,
        "queue": place(&s, pr),
        "credits": credits,
        "touches": list.iter().map(Touched::json).collect::<Vec<_>>(),
        "reviews": pr.reviews,
        "files": files,
        "files_error": files_error,
        "discussion": talk,
        "discussion_error": talk_error,
        "review_on_github": format!("{}/files", github_url(n)),
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::tests::pr;

    #[test]
    fn only_a_plain_pull_request_link_maps_to_our_page() {
        assert_eq!(
            number_of("https://github.com/NixOS/nixpkgs/pull/123"),
            Some(123)
        );
        assert_eq!(
            number_of("https://github.com/NixOS/nixpkgs/pull/123#pullrequestreview-4"),
            None
        );
        assert_eq!(number_of("https://github.com/NixOS/nix/pull/123"), None);
        assert_eq!(number_of("https://github.com/NixOS/nixpkgs/pull/"), None);
        assert!(
            link("https://github.com/NixOS/nixpkgs/pull/7", "t", "x")
                .starts_with("<a class=\"x\" href=\"/pr/7\">t</a> <a class=\"color-fg-muted ml-1\" href=\"https://github.com/NixOS/nixpkgs/pull/7\"")
        );
        assert_eq!(
            link("https://example.org/?a=\"", "t", "x"),
            "<a class=\"x\" href=\"https://example.org/?a=&quot;\">t</a>"
        );
    }

    #[test]
    fn markdown_escapes_html_and_drops_unsafe_links() {
        let html = markdown(
            "<!-- template\nnoise -->\nHi <script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n\n\
             [a](javascript:alert(1)) [b](JaVa\tScRiPt:x) [c](https://nixos.org) ![d](https://e.org/i.png) <data:text/html,x>",
        );
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("<img"), "{html}");
        assert!(!html.contains("template"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(!html.to_lowercase().contains("javascript:"), "{html}");
        assert!(!html.contains("href=\"data:"), "{html}");
        assert!(
            html.contains("<a href=\"https://nixos.org\">c</a>"),
            "{html}"
        );
        assert!(
            html.contains("<a href=\"https://e.org/i.png\">d</a>"),
            "{html}"
        );
    }

    #[test]
    fn comments_end_in_a_later_event() {
        let mut open = false;
        assert_eq!(strip_comments("a<!-- b", &mut open), "a");
        assert!(open);
        assert_eq!(strip_comments("c --> d<!--e-->f", &mut open), " df");
        assert!(!open);
    }

    #[test]
    fn a_patch_colours_its_lines_and_escapes_them() {
        let html =
            patch_html("@@ -1,2 +1,2 @@\n context\n-old <a>\n+new\n\\ No newline at end of file");
        assert!(html.contains("<span class=\"diff-hunk\">@@ -1,2 +1,2 @@</span>"));
        assert!(html.contains("<span class=\"diff-del\">-old &lt;a&gt;</span>"));
        assert!(html.contains("<span class=\"diff-add\">+new</span>"));
        assert!(html.contains("<span class=\"\"> context</span>"));
        assert!(html.contains("diff-note"));
    }

    #[test]
    fn a_large_or_binary_file_links_to_github() {
        let mut f = File {
            path: "a.png".into(),
            previous: None,
            status: "added".into(),
            additions: 0,
            deletions: 0,
            patch: None,
        };
        let html = file_html(5, &f);
        assert!(html.contains("binary file"));
        assert!(html.contains("https://github.com/NixOS/nixpkgs/pull/5/files#diff-"));
        f.patch = Some("+x\n".repeat(PATCH_LINES + 1));
        assert!(file_html(5, &f).contains("A large diff"));
    }

    #[test]
    fn the_archive_and_the_live_read_give_one_timeline() {
        let v = json!({
            "body": "the description",
            "updatedAt": "2026-01-03T00:00:00Z",
            "reviews": { "nodes": [
                { "url": "r1", "state": "COMMENTED", "body": "", "submittedAt": "2026-01-02T00:00:00Z",
                  "author": { "login": "bob" },
                  "comments": { "nodes": [ { "url": "c1", "createdAt": "2026-01-02T00:00:00Z", "body": "nit",
                    "path": "pkgs/a.nix", "line": 3, "author": { "login": "bob" } } ] } },
                { "url": "r2", "state": "APPROVED", "body": "", "submittedAt": "2026-01-03T00:00:00Z",
                  "author": { "login": "carol" }, "comments": { "nodes": [] } }
            ] },
            "comments": { "nodes": [
                { "url": "i1", "createdAt": "2026-01-01T00:00:00Z", "body": "hello", "author": { "login": "alice" } },
                { "url": "i2", "createdAt": "2026-01-01T01:00:00Z", "body": "spam", "isMinimized": true, "author": { "login": "x" } }
            ] }
        });
        let t = talk_of(&v, "archive");
        assert_eq!(t.body, "the description");
        let order: Vec<&str> = t.notes.iter().map(|n| n.url.as_str()).collect();
        assert_eq!(order, ["i1", "c1", "r2"]);
        assert_eq!(t.notes[1].path.as_deref(), Some("pkgs/a.nix"));
        assert_eq!(t.notes[1].line, Some(3));
    }

    #[test]
    fn the_cache_drops_the_oldest_entry() {
        let mut c: Lru<u8> = Lru::default();
        for i in 0..CACHE_ENTRIES + 1 {
            c.put(i.to_string(), Arc::new(0), 1);
            if i == 1 {
                assert!(c.get("0").is_some());
            }
        }
        assert_eq!(c.map.len(), CACHE_ENTRIES);
        assert!(c.get("0").is_some());
        assert!(c.get("1").is_none());
        c.put("big".into(), Arc::new(0), CACHE_BYTES);
        assert_eq!(c.map.len(), 1);
        assert_eq!(c.bytes, CACHE_BYTES);
    }

    async fn body(r: Response) -> (StatusCode, String) {
        let status = r.status();
        let b = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8(b.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn the_page_and_the_api_read_the_cache_before_github() {
        let app = crate::sets::tests::app(&["hello"]);
        let mut p = pr(42, "alice", 0);
        p.title = "hello: 1 -> <2>".into();
        p.changed_files = 1;
        app.store.put_prs(std::slice::from_ref(&p)).unwrap();
        app.recompute().unwrap();
        let files = Files {
            head: p.head_oid.clone(),
            capped: false,
            files: vec![File {
                path: "pkgs/by-name/he/hello/package.nix".into(),
                previous: None,
                status: "modified".into(),
                additions: 1,
                deletions: 1,
                patch: Some("@@ -1 +1 @@\n-1\n+2".into()),
            }],
        };
        app.pr_cache
            .files
            .lock()
            .unwrap()
            .put(format!("42@{}", p.head_oid), Arc::new(files), 1);
        let talk = talk_of(&json!({ "body": "**bold** <b>raw</b>" }), "github");
        app.pr_cache.talk.lock().unwrap().put(
            format!("42@{}", p.updated_at.to_rfc3339()),
            Arc::new(talk),
            1,
        );
        let (status, html) =
            body(pr_page(State(app.clone()), HeaderMap::new(), Path(42)).await).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("hello: 1 -&gt; &lt;2&gt;"));
        assert!(html.contains("<strong>bold</strong> &lt;b&gt;raw&lt;/b&gt;"));
        assert!(html.contains("<span class=\"diff-add\">+2</span>"));
        assert!(html.contains("https://github.com/NixOS/nixpkgs/pull/42/files\""));
        assert!(html.contains("its author is not listed"));
        let (status, api) = body(api_pr(State(app.clone()), Path(42)).await).await;
        assert_eq!(status, StatusCode::OK);
        let v: Value = serde_json::from_str(&api).unwrap();
        assert_eq!(v["files"]["files"][0]["additions"], 1);
        assert_eq!(v["discussion"]["body"], "**bold** <b>raw</b>");
        assert_eq!(v["queue"]["queue"], "none");

        let (status, html) =
            body(pr_page(State(app.clone()), HeaderMap::new(), Path(1)).await).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("not in the synced data"));
        assert!(html.contains("href=\"https://github.com/NixOS/nixpkgs/pull/1\""));
        let (status, api) = body(api_pr(State(app), Path(1)).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(api.contains("not in the synced data"));
    }

    #[tokio::test]
    async fn the_archive_copy_is_parsed_once_and_shows_the_latest_notes() {
        let mut app = crate::sets::tests::app(&[]);
        let dir = std::env::temp_dir().join(format!("mm-prpage-archive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = crate::archive::Archive::open(&dir, crate::archive::DEFAULT_REPO, 0).unwrap();
        let comments: Vec<Value> = (0..NOTE_CAP + 5)
            .map(|i| {
                json!({ "url": format!("c{i}"), "createdAt": format!("2026-01-01T00:{:02}:{:02}Z", i / 60, i % 60),
                        "body": format!("note {i}"), "author": { "login": "bob" } })
            })
            .collect();
        let item = |body: &str| {
            json!({ "__typename": "PullRequest", "body": body, "updatedAt": "2026-02-01T00:00:00Z",
                    "comments": { "nodes": comments } })
        };
        a.put(42, &item("first copy"), Utc::now()).unwrap();
        let a = Arc::new(a);
        Arc::get_mut(&mut app).unwrap().archive = vec![a.clone()];
        let mut p = pr(42, "alice", 0);
        p.updated_at = "2026-01-15T00:00:00Z".parse().unwrap();
        app.store.put_prs(std::slice::from_ref(&p)).unwrap();
        app.recompute().unwrap();

        let (_, html) = body(pr_page(State(app.clone()), HeaderMap::new(), Path(42)).await).await;
        assert!(html.contains("first copy"));
        assert!(html.contains("5 earlier reviews and comments are not shown"));
        assert!(!html.contains(">note 4<"));
        assert!(html.contains(">note 5<"));

        a.put(42, &item("second copy"), Utc::now()).unwrap();
        let (_, api) = body(api_pr(State(app.clone()), Path(42)).await).await;
        let v: Value = serde_json::from_str(&api).unwrap();
        assert_eq!(v["discussion"]["body"], "first copy");
        assert_eq!(v["discussion"]["omitted"], 5);
        assert_eq!(v["discussion"]["notes"].as_array().unwrap().len(), NOTE_CAP);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_token_low_on_budget_rests_until_its_reset() {
        let b = Budget::default();
        let now = Utc::now();
        b.note("graphql", "t", RESERVE, now + chrono::Duration::hours(1));
        assert!(b.usable("graphql", "t", now));
        b.note(
            "graphql",
            "t",
            RESERVE - 1,
            now + chrono::Duration::hours(1),
        );
        assert!(!b.usable("graphql", "t", now));
        assert!(b.usable("rest", "t", now));
        assert!(b.usable("graphql", "u", now));
        assert!(b.usable("graphql", "t", now + chrono::Duration::hours(2)));

        let start = Instant::now();
        assert!((0..LIVE_PER_MINUTE).all(|_| b.admit(start)));
        assert!(!b.admit(start + Duration::from_secs(59)));
        assert!(b.admit(start + Duration::from_secs(60)));
    }

    #[tokio::test]
    async fn reads_at_the_same_time_on_one_key_run_once() {
        let flights: Flights<u32> = Default::default();
        let runs = std::sync::atomic::AtomicUsize::new(0);
        let read = || async {
            runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(Arc::new(7))
        };
        let (a, b) = tokio::join!(joined(&flights, "k", read()), joined(&flights, "k", read()));
        assert_eq!((*a.unwrap(), *b.unwrap()), (7, 7));
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(flights.lock().unwrap().is_empty());
        joined(&flights, "k", read()).await.unwrap();
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn an_anonymous_visitor_never_makes_the_server_read_github() {
        let app = crate::sets::tests::app(&[]);
        let p = pr(43, "alice", 0);
        app.store.put_prs(std::slice::from_ref(&p)).unwrap();
        app.recompute().unwrap();
        let (status, html) =
            body(pr_page(State(app.clone()), HeaderMap::new(), Path(43)).await).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("The diff could not be read: this server reads GitHub only for people who sign in (<a href=\"/auth/login\">sign in</a>)"));
        let (_, api) = body(api_pr(State(app.clone()), Path(43)).await).await;
        let v: Value = serde_json::from_str(&api).unwrap();
        assert_eq!(v["files_error"], NOT_LIVE);
        assert_eq!(v["discussion_error"], NOT_LIVE);
        assert_eq!(app.pr_cache.budget.window.lock().unwrap().1, 0);
    }

    #[test]
    fn the_page_says_why_a_pull_request_is_not_ready() {
        let mut s = Snapshot::default();
        let mut p = pr(9, "alice", 0);
        p.ci = Some("FAILURE".into());
        p.mergeable = "CONFLICTING".into();
        let e = queue::entry(&p, &s.ledger, &Default::default());
        s.queues.review.push(e);
        let place = place(&s, &p);
        assert_eq!(place.queue, "review");
        assert!(place.why.contains(&"checks fail".to_string()));
        assert!(place.why.iter().any(|w| w.contains("conflict")));
        assert!(place.why.iter().any(|w| w.contains("approved")));
        s.queues.review.clear();
        p.is_draft = true;
        let place = super::place(&s, &p);
        assert_eq!(place.queue, "none");
        assert!(place.why.contains(&"it is a draft".to_string()));
        assert!(place.why.iter().any(|w| w.contains("not listed")));
    }
}
