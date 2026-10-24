//! The notifications of a machine account that watches NixOS/nixpkgs with
//! "All activity". GitHub opens a thread for each issue and pull request
//! that changes, so the list names what to read again.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use reqwest::header::{ACCEPT, IF_MODIFIED_SINCE};
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;
use tokio::sync::Notify;

use crate::events::REPO;
use crate::github;
use crate::secret::Secret;
use crate::store::Store;
use crate::web::App;

const API: &str = "https://api.github.com";
const MAX_PAGES: usize = 20;
const PRS_PER_QUERY: usize = 10;
const ISSUES_PER_QUERY: usize = 50;
const DEFAULT_INTERVAL: u64 = 60;
const ERROR_WAIT: u64 = 60;
const RECOMPUTE_EVERY: StdDuration = StdDuration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    PullRequest,
    Issue,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Thread {
    pub id: String,
    pub kind: Kind,
    pub number: u64,
    pub updated_at: DateTime<Utc>,
}

fn time(v: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(v.as_str()?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// `…/repos/NixOS/nixpkgs/pulls/123` or `…/issues/123`.
fn number_of(url: &str, kind: Kind) -> Option<u64> {
    let mut parts = url.trim_end_matches('/').rsplit('/');
    let number = parts.next()?.parse().ok()?;
    let segment = match kind {
        Kind::PullRequest => "pulls",
        Kind::Issue => "issues",
    };
    (parts.next()? == segment).then_some(number)
}

/// A thread on an issue or pull request of nixpkgs; None for anything else.
pub fn thread(v: &Value) -> Option<Thread> {
    if !v["repository"]["full_name"]
        .as_str()?
        .eq_ignore_ascii_case(REPO)
    {
        return None;
    }
    let kind = match v["subject"]["type"].as_str()? {
        "PullRequest" => Kind::PullRequest,
        "Issue" => Kind::Issue,
        _ => return None,
    };
    let id = match &v["id"] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    Some(Thread {
        id,
        kind,
        number: number_of(v["subject"]["url"].as_str()?, kind)?,
        updated_at: time(&v["updated_at"])?,
    })
}

fn next_link(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let (url, rel) = part.split_once(';')?;
        rel.contains("rel=\"next\"").then(|| {
            url.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        })
    })
}

#[derive(Debug)]
pub enum Answer {
    /// A `304`, which costs no rate limit.
    NotModified { interval: u64 },
    Changed {
        interval: u64,
        last_modified: Option<String>,
        threads: Vec<Thread>,
        /// The newest update of any thread, kept or not.
        newest: Option<DateTime<Utc>>,
    },
}

pub struct Api {
    http: reqwest::Client,
    base: String,
}

impl Api {
    pub fn new(base: &str) -> Self {
        Self {
            http: github::http(),
            base: base.trim_end_matches('/').to_string(),
        }
    }

    /// The unread threads. `since` is the `Last-Modified` of the last
    /// answer that was handled in full.
    pub async fn list(&self, token: &str, since: Option<&str>) -> Result<Answer> {
        let mut url = format!(
            "{}/notifications?all=false&participating=false&per_page=50",
            self.base
        );
        let mut interval = DEFAULT_INTERVAL;
        let mut last_modified = None;
        let mut threads = Vec::new();
        let mut newest = None;
        for page in 0..MAX_PAGES {
            let mut req = self
                .http
                .get(&url)
                .bearer_auth(token)
                .header(ACCEPT, "application/vnd.github+json");
            if let (0, Some(s)) = (page, since) {
                req = req.header(IF_MODIFIED_SINCE, s);
            }
            let resp = req.send().await?;
            let header = |name: &str| {
                resp.headers()
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(String::from)
            };
            if page == 0 {
                interval = header("x-poll-interval")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(DEFAULT_INTERVAL);
                if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
                    return Ok(Answer::NotModified { interval });
                }
                last_modified = header("last-modified");
            }
            let next = header("link").and_then(|l| next_link(&l));
            let status = resp.status();
            if !status.is_success() {
                bail!("GET /notifications: {status}");
            }
            let batch: Vec<Value> = resp.json().await.context("notifications page")?;
            for v in &batch {
                newest = newest.max(time(&v["updated_at"]));
                threads.extend(thread(v));
            }
            match next {
                Some(n) => url = n,
                None => break,
            }
        }
        Ok(Answer::Changed {
            interval,
            last_modified,
            threads,
            newest,
        })
    }

    /// Marks every thread updated at or before `at` read, so the list stays
    /// short. It changes only the notifications of the account.
    pub async fn mark_read(&self, token: &str, at: DateTime<Utc>) -> Result<()> {
        let resp = self
            .http
            .put(format!("{}/notifications", self.base))
            .bearer_auth(token)
            .header(ACCEPT, "application/vnd.github+json")
            .json(&json!({
                "last_read_at": at.to_rfc3339_opts(SecondsFormat::Secs, true),
                "read": true,
            }))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            bail!("PUT /notifications: {status}");
        }
        Ok(())
    }
}

pub struct Applied {
    pub stored: usize,
    /// The first fetch that failed, when one did. Its threads stay unread.
    pub failed: Option<String>,
}

/// Reads each changed item with the walk's fields, stores it, and marks the
/// threads read up to the oldest one that failed.
pub async fn apply(
    api: &Api,
    token: &str,
    store: &Store,
    client: &mut github::Client,
    status: &Status,
    threads: &[Thread],
    newest: Option<DateTime<Utc>>,
) -> Result<Applied> {
    let mut stored = 0;
    let mut failed = None;
    let mut oldest_failed: Option<DateTime<Utc>> = None;
    for (kind, per_query) in [
        (Kind::PullRequest, PRS_PER_QUERY),
        (Kind::Issue, ISSUES_PER_QUERY),
    ] {
        let mine: Vec<&Thread> = threads.iter().filter(|t| t.kind == kind).collect();
        for chunk in mine.chunks(per_query) {
            let numbers: Vec<u64> = chunk
                .iter()
                .map(|t| t.number)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let (prs, issues) = match kind {
                Kind::PullRequest => (numbers.as_slice(), &[][..]),
                Kind::Issue => (&[][..], numbers.as_slice()),
            };
            match github::by_number(store, client, prs, issues).await {
                Ok((prs, issues)) => {
                    store.put_prs(&prs)?;
                    store.note_live(&prs, Utc::now())?;
                    store.put_issues(&issues)?;
                    stored += prs.len() + issues.len();
                    status.seen(chunk.iter().map(|t| t.updated_at), Utc::now());
                }
                Err(e) => {
                    tracing::warn!("notifications: fetch: {e:#}");
                    failed.get_or_insert_with(|| format!("{e:#}"));
                    let t = chunk.iter().map(|t| t.updated_at).min();
                    oldest_failed = oldest_failed.into_iter().chain(t).min();
                }
            }
        }
    }
    let read_up_to = match oldest_failed {
        Some(t) => Some(t - Duration::seconds(1)),
        None => newest,
    };
    if let Some(at) = read_up_to {
        api.mark_read(token, at).await?;
    }
    Ok(Applied { stored, failed })
}

/// While the poller is healthy, the full walk of pull requests and issues
/// runs at most this often. A pass that starts up to a minute early counts.
pub const WALK_EVERY_WHEN_HEALTHY: i64 = 1800;
const WALK_SLACK: i64 = 60;

/// The full walk runs on each pass, except while the poller is healthy:
/// then only after the first walks are done and every
/// `WALK_EVERY_WHEN_HEALTHY`.
pub fn walk_due(
    healthy: bool,
    first_walks_done: bool,
    last_walk: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    !healthy
        || !first_walks_done
        || last_walk
            .is_none_or(|t| now - t >= Duration::seconds(WALK_EVERY_WHEN_HEALTHY - WALK_SLACK))
}

#[derive(Default)]
pub struct Status(Mutex<Inner>);

#[derive(Default)]
struct Inner {
    on: bool,
    interval: u64,
    last_poll: Option<DateTime<Utc>>,
    last_ok: Option<DateTime<Utc>>,
    /// When each thread was fetched, and its delay after the GitHub update.
    seen: VecDeque<(DateTime<Utc>, i64)>,
    errors: VecDeque<DateTime<Utc>>,
    last_error: Option<(DateTime<Utc>, String)>,
}

impl Inner {
    fn prune(&mut self, now: DateTime<Utc>) {
        let hour_ago = now - Duration::hours(1);
        while self.seen.front().is_some_and(|(t, _)| *t < hour_ago) {
            self.seen.pop_front();
        }
        while self.errors.front().is_some_and(|t| *t < hour_ago) {
            self.errors.pop_front();
        }
    }
}

impl Status {
    pub fn on() -> Self {
        Self(Mutex::new(Inner {
            on: true,
            interval: DEFAULT_INTERVAL,
            ..Default::default()
        }))
    }

    fn seen(&self, updates: impl Iterator<Item = DateTime<Utc>>, now: DateTime<Utc>) {
        let mut s = self.0.lock().unwrap();
        for u in updates {
            s.seen.push_back((now, (now - u).num_seconds().max(0)));
        }
        s.prune(now);
    }

    fn polled(&self, now: DateTime<Utc>, interval: u64, error: Option<String>) {
        let mut s = self.0.lock().unwrap();
        s.last_poll = Some(now);
        s.interval = interval;
        match error {
            None => s.last_ok = Some(now),
            Some(e) => {
                s.errors.push_back(now);
                s.last_error = Some((now, e));
            }
        }
        s.prune(now);
    }

    /// On, and the last poll that went well is recent: it is at most three
    /// intervals old, or 5 minutes when that is longer.
    pub fn healthy(&self, now: DateTime<Utc>) -> bool {
        let s = self.0.lock().unwrap();
        let window = Duration::seconds((3 * s.interval as i64).max(300));
        s.on && s.last_ok.is_some_and(|t| now - t <= window)
            && s.last_error
                .as_ref()
                .is_none_or(|(t, _)| *t < s.last_ok.unwrap())
    }

    pub fn json(&self, now: DateTime<Utc>) -> Value {
        let healthy = self.healthy(now);
        let mut s = self.0.lock().unwrap();
        s.prune(now);
        let n = s.seen.len();
        json!({
            "enabled": s.on,
            "healthy": healthy,
            "interval_seconds": s.on.then_some(s.interval),
            "last_poll": s.last_poll,
            "last_ok": s.last_ok,
            "threads_last_hour": n,
            "average_delay_seconds": (n > 0).then(|| s.seen.iter().map(|(_, d)| d).sum::<i64>() / n as i64),
            "errors_last_hour": s.errors.len(),
            "last_error": s.last_error.as_ref().map(|(at, m)| json!({ "at": at, "message": m })),
        })
    }
}

/// The tokens of the sync, in its order: the GitHub App, the bot, then the
/// tokens that people lend.
async fn client(app: &App, bot: &str) -> github::Client {
    let mut tokens = Vec::new();
    if let Some(a) = &app.github_app {
        match a.token().await {
            Ok(t) => tokens.push(t),
            Err(e) => tracing::warn!("notifications: GitHub App token: {e:#}"),
        }
    }
    tokens.push(bot.to_string());
    tokens.extend(app.store.lent_tokens().unwrap_or_default());
    github::Client::new(tokens)
}

/// Recomputes at once after a change, then at most once per
/// `RECOMPUTE_EVERY`.
async fn recompute_soon(app: Arc<App>, wake: Arc<Notify>) {
    loop {
        wake.notified().await;
        let a = app.clone();
        match tokio::task::spawn_blocking(move || {
            a.recompute()?;
            crate::notify::recount(&a);
            anyhow::Ok(())
        })
        .await
        {
            Ok(Err(e)) => tracing::warn!("notifications: recompute: {e:#}"),
            Err(e) => tracing::warn!("notifications: recompute: {e}"),
            Ok(Ok(())) => {}
        }
        tokio::time::sleep(RECOMPUTE_EVERY).await;
    }
}

async fn poll_once(
    app: &App,
    api: &Api,
    bot: &str,
    since: &mut Option<String>,
    wake: &Notify,
) -> Result<(u64, Option<String>)> {
    let (interval, last_modified, threads, newest) = match api.list(bot, since.as_deref()).await? {
        Answer::NotModified { interval } => return Ok((interval, None)),
        Answer::Changed {
            interval,
            last_modified,
            threads,
            newest,
        } => (interval, last_modified, threads, newest),
    };
    let numbers: Vec<u64> = threads.iter().map(|t| t.number).collect();
    crate::archive::mark_due(app, REPO, &numbers);
    let mut client = client(app, bot).await;
    let applied = apply(
        api,
        bot,
        &app.store,
        &mut client,
        &app.notifications,
        &threads,
        newest,
    )
    .await?;
    if applied.stored > 0 {
        wake.notify_one();
    }
    if applied.failed.is_none() {
        *since = last_modified;
    }
    Ok((interval, applied.failed))
}

/// Polls the notifications of the bot account at the interval GitHub asks
/// for.
pub async fn run(app: Arc<App>, token: Arc<Secret>) {
    let api = Api::new(API);
    let wake = Arc::new(Notify::new());
    tokio::spawn(recompute_soon(app.clone(), wake.clone()));
    let mut since = None;
    loop {
        let wait = match poll_once(&app, &api, &token.get(), &mut since, &wake).await {
            Ok((interval, failed)) => {
                app.notifications.polled(Utc::now(), interval, failed);
                interval
            }
            Err(e) => {
                tracing::warn!("notifications: {e:#}");
                app.notifications
                    .polled(Utc::now(), ERROR_WAIT, Some(format!("{e:#}")));
                ERROR_WAIT
            }
        };
        tokio::time::sleep(StdDuration::from_secs(wait.max(1))).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::extract::{Query, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use std::collections::HashMap;
    use std::path::Path;

    fn note(id: &str, repo: &str, kind: &str, url: &str, at: &str) -> Value {
        json!({
            "id": id, "updated_at": at,
            "repository": { "full_name": repo },
            "subject": { "type": kind, "url": url },
        })
    }

    const PR_URL: &str = "https://api.github.com/repos/NixOS/nixpkgs/pulls/7";
    const ISSUE_URL: &str = "https://api.github.com/repos/NixOS/nixpkgs/issues/9";

    #[test]
    fn keeps_nixpkgs_pull_requests_and_issues_only() {
        let at = "2026-10-17T10:00:00Z";
        let t = thread(&note("1", "NixOS/nixpkgs", "PullRequest", PR_URL, at)).unwrap();
        assert_eq!(
            (t.id.as_str(), t.kind, t.number),
            ("1", Kind::PullRequest, 7)
        );
        let t = thread(&note("2", "NixOS/nixpkgs", "Issue", ISSUE_URL, at)).unwrap();
        assert_eq!((t.kind, t.number), (Kind::Issue, 9));
        assert!(thread(&note("3", "NixOS/nix", "Issue", ISSUE_URL, at)).is_none());
        assert!(thread(&note("4", "NixOS/nixpkgs", "Release", ISSUE_URL, at)).is_none());
        assert!(thread(&note("5", "NixOS/nixpkgs", "Discussion", ISSUE_URL, at)).is_none());
        let mut v = note("6", "NixOS/nixpkgs", "Issue", ISSUE_URL, at);
        v["subject"]["url"] = Value::Null;
        assert!(thread(&v).is_none());
        assert!(thread(&note("7", "NixOS/nixpkgs", "Issue", ISSUE_URL, "soon")).is_none());
    }

    #[test]
    fn number_comes_from_the_subject_url() {
        assert_eq!(number_of(PR_URL, Kind::PullRequest), Some(7));
        assert_eq!(number_of(&format!("{ISSUE_URL}/"), Kind::Issue), Some(9));
        assert_eq!(number_of(PR_URL, Kind::Issue), None);
        assert_eq!(number_of(ISSUE_URL, Kind::PullRequest), None);
        assert_eq!(
            number_of(
                "https://api.github.com/repos/NixOS/nixpkgs/pulls/x",
                Kind::PullRequest
            ),
            None
        );
        assert_eq!(number_of("", Kind::Issue), None);
    }

    #[test]
    fn link_header_gives_the_next_page() {
        assert_eq!(
            next_link("<https://a/n?page=2>; rel=\"next\", <https://a/n?page=5>; rel=\"last\"")
                .as_deref(),
            Some("https://a/n?page=2")
        );
        assert_eq!(next_link("<https://a/n?page=1>; rel=\"prev\""), None);
    }

    #[test]
    fn healthy_needs_a_recent_poll_without_error() {
        let s = Status::default();
        let now = Utc::now();
        assert!(!s.healthy(now));
        let s = Status::on();
        assert!(!s.healthy(now));
        s.polled(now, 60, None);
        assert!(s.healthy(now));
        assert!(!s.healthy(now + Duration::minutes(6)));
        s.polled(now + Duration::seconds(60), 60, Some("boom".into()));
        assert!(!s.healthy(now + Duration::seconds(61)));
        s.polled(now + Duration::seconds(120), 60, None);
        assert!(s.healthy(now + Duration::seconds(121)));
        let v = s.json(now + Duration::seconds(121));
        assert_eq!(v["errors_last_hour"], 1);
        assert_eq!(v["last_error"]["message"], "boom");
    }

    #[test]
    fn the_walk_waits_only_while_the_poller_is_healthy() {
        let now = Utc::now();
        let ago = |m: i64| Some(now - Duration::minutes(m));
        assert!(walk_due(false, true, ago(1), now));
        assert!(walk_due(true, false, ago(1), now));
        assert!(walk_due(true, true, None, now));
        assert!(!walk_due(true, true, ago(20), now));
        assert!(walk_due(true, true, ago(29), now));
        assert!(walk_due(true, true, ago(31), now));
    }

    const LAST_MODIFIED: &str = "Sat, 17 Oct 2026 10:00:00 GMT";

    #[derive(Default)]
    struct Fake {
        base: Mutex<String>,
        marked: Mutex<Vec<Value>>,
        graphql_fails: bool,
    }

    async fn list(
        State(f): State<Arc<Fake>>,
        headers: HeaderMap,
        Query(q): Query<HashMap<String, String>>,
    ) -> axum::response::Response {
        if headers
            .get("if-modified-since")
            .is_some_and(|v| v == LAST_MODIFIED)
        {
            return (StatusCode::NOT_MODIFIED, [("x-poll-interval", "90")]).into_response();
        }
        let base = f.base.lock().unwrap().clone();
        if q.get("page").map(String::as_str) == Some("2") {
            return axum::Json(json!([note(
                "12",
                "NixOS/nixpkgs",
                "Issue",
                ISSUE_URL,
                "2026-10-17T09:58:00Z"
            )]))
            .into_response();
        }
        let link = format!("<{base}/notifications?page=2>; rel=\"next\"");
        (
            [
                ("x-poll-interval", "60".to_string()),
                ("last-modified", LAST_MODIFIED.to_string()),
                ("link", link),
            ],
            axum::Json(json!([
                note(
                    "10",
                    "NixOS/nixpkgs",
                    "PullRequest",
                    PR_URL,
                    "2026-10-17T09:59:00Z"
                ),
                note(
                    "11",
                    "NixOS/nix",
                    "PullRequest",
                    PR_URL,
                    "2026-10-17T09:59:30Z"
                ),
            ])),
        )
            .into_response()
    }

    async fn mark(State(f): State<Arc<Fake>>, axum::Json(b): axum::Json<Value>) -> StatusCode {
        f.marked.lock().unwrap().push(b);
        StatusCode::RESET_CONTENT
    }

    async fn graphql(State(f): State<Arc<Fake>>) -> axum::response::Response {
        if f.graphql_fails {
            return StatusCode::BAD_GATEWAY.into_response();
        }
        let item = |n: u64| {
            json!({ "number": n, "title": "t", "url": "u", "state": "OPEN",
                    "createdAt": "2026-10-01T00:00:00Z", "updatedAt": "2026-10-17T09:59:00Z" })
        };
        axum::Json(json!({ "data": {
            "rateLimit": { "remaining": 5000 },
            "repository": { "p7": item(7), "i9": item(9) },
        } }))
        .into_response()
    }

    async fn serve(fake: Arc<Fake>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        *fake.base.lock().unwrap() = base.clone();
        let app = Router::new()
            .route("/notifications", get(list).put(mark))
            .route("/graphql", post(graphql))
            .with_state(fake);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    #[tokio::test]
    async fn if_modified_since_gives_304_and_keeps_the_interval() {
        let base = serve(Arc::new(Fake::default())).await;
        let api = Api::new(&base);
        let Answer::Changed {
            interval,
            last_modified,
            threads,
            newest,
        } = api.list("t", None).await.unwrap()
        else {
            panic!("first poll must list the threads");
        };
        assert_eq!(interval, 60);
        assert_eq!(last_modified.as_deref(), Some(LAST_MODIFIED));
        assert_eq!(
            threads.iter().map(|t| t.number).collect::<Vec<_>>(),
            vec![7, 9]
        );
        assert_eq!(newest, time(&json!("2026-10-17T09:59:30Z")));
        match api.list("t", last_modified.as_deref()).await.unwrap() {
            Answer::NotModified { interval } => assert_eq!(interval, 90),
            a => panic!("expected 304, got {a:?}"),
        }
    }

    async fn applied(fails: bool) -> (Applied, Arc<Fake>, Store) {
        let fake = Arc::new(Fake {
            graphql_fails: fails,
            ..Default::default()
        });
        let base = serve(fake.clone()).await;
        let api = Api::new(&base);
        let Answer::Changed {
            threads, newest, ..
        } = api.list("t", None).await.unwrap()
        else {
            panic!("first poll must list the threads");
        };
        let store = Store::open(Path::new(":memory:")).unwrap();
        let mut client = github::Client::at(&format!("{base}/graphql"), vec!["t".into()]);
        let status = Status::on();
        let a = apply(&api, "t", &store, &mut client, &status, &threads, newest)
            .await
            .unwrap();
        (a, fake, store)
    }

    #[tokio::test]
    async fn changed_items_are_stored_and_marked_read() {
        let (a, fake, store) = applied(false).await;
        assert_eq!(a.stored, 2);
        assert!(a.failed.is_none());
        assert_eq!(store.prs().unwrap()[0].number, 7);
        assert_eq!(store.issues().unwrap()[0].number, 9);
        let marked = fake.marked.lock().unwrap();
        assert_eq!(marked.len(), 1);
        assert_eq!(marked[0]["last_read_at"], "2026-10-17T09:59:30Z");
    }

    #[tokio::test]
    async fn a_failed_fetch_stays_unread() {
        let (a, fake, store) = applied(true).await;
        assert_eq!(a.stored, 0);
        assert!(a.failed.is_some());
        assert!(store.prs().unwrap().is_empty());
        let marked = fake.marked.lock().unwrap();
        assert_eq!(marked[0]["last_read_at"], "2026-10-17T09:57:59Z");
    }
}
