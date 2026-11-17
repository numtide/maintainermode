//! The Discourse thread "PRs ready for review": a person posts the link of
//! a pull request there to ask for a review. The pull requests posted
//! there make the Asked on Discourse choice of `/review`, any author,
//! because the ask itself is the signal.
//!
//! Discourse sends no `ETag` and `Cache-Control: no-store` on its JSON, so
//! a conditional request saves nothing. The task is gentle instead: one
//! read of the topic every hour, which names every post id of the thread;
//! then only the posts the store has not seen, 100 to a request, with a
//! second between requests. The first run reads the whole thread, about
//! 80 requests, once. A post names its links in `link_counts`, so no HTML
//! is parsed.
//!
//! The thread is not ours: it can close, move, or refuse anonymous
//! clients. Every failure of a pass is one line in the journal with what
//! to do, and the same line on `/state?tab=status`. The site never waits
//! on it: a failed pass keeps the last good data.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::html::{Html, html};
use crate::store::Store;

pub(crate) const DEFAULT_TOPIC: &str = "https://discourse.nixos.org/t/prs-ready-for-review/3032";
/// The title of the thread, for the words of the page.
pub(crate) const THREAD_NAME: &str = "PRs ready for review";
/// The variable of `--discourse-topic`, for the advice in the journal.
pub(crate) const VAR: &str = "MAINTAINER_MODE_DISCOURSE_TOPIC";
/// The thread gets 5 to 10 posts a day: an hour is soon enough.
pub(crate) const EVERY: Duration = Duration::from_secs(3600);
/// [`EVERY`] in words.
pub(crate) const EVERY_WORDS: &str = "hour";
/// Between two requests of one pass.
const PAUSE: Duration = Duration::from_secs(1);
/// Post ids per `posts.json` request. Discourse answers 100 in one page.
const BATCH: usize = 100;
const TIMEOUT: Duration = Duration::from_secs(30);
/// After a failed pass, the waits before the next try, then the last one
/// again. A `Retry-After` header wins over them.
const RETRIES: [Duration; 5] = [
    Duration::from_secs(10 * 60),
    Duration::from_secs(30 * 60),
    Duration::from_secs(3600),
    Duration::from_secs(2 * 3600),
    Duration::from_secs(6 * 3600),
];
/// A topic page or a batch larger than this is not what we asked for.
const MAX_BODY: usize = 16 << 20;

#[derive(clap::Args)]
pub struct DiscourseArgs {
    /// The Discourse thread whose pull requests make Asked on Discourse on
    /// /review, read every hour; an empty value turns it off.
    #[arg(
        long = "discourse-topic",
        env = "MAINTAINER_MODE_DISCOURSE_TOPIC",
        default_value = DEFAULT_TOPIC
    )]
    topic: String,
}

impl DiscourseArgs {
    /// The thread to follow; `None` when the value is empty.
    pub fn topic(&self) -> Result<Option<Topic>> {
        match self.topic.trim() {
            "" => Ok(None),
            url => Topic::parse(url)
                .with_context(|| {
                    format!(
                        "{VAR}: set it to the URL of the thread, or to an empty value to turn the Discourse tab off"
                    )
                })
                .map(Some),
        }
    }
}

/// Where the thread is: `https://discourse.nixos.org/t/prs-ready-for-review/3032`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topic {
    /// `https://discourse.nixos.org`
    pub base: String,
    pub id: u64,
    /// The slug in the URL, kept for the links to posts; Discourse
    /// redirects without it.
    pub slug: Option<String>,
}

impl Topic {
    /// Takes the URL of the thread, with or without its slug, and with or
    /// without a post number at the end, as a browser shows it.
    pub fn parse(url: &str) -> Result<Topic> {
        let url = url.trim().trim_end_matches('/');
        let u = reqwest::Url::parse(url).with_context(|| format!("{url:?} is not a URL"))?;
        if !matches!(u.scheme(), "http" | "https") {
            anyhow::bail!("{url:?}: the topic URL must start with http:// or https://");
        }
        let host = u
            .host_str()
            .with_context(|| format!("{url:?} has no host"))?;
        let base = match u.port() {
            Some(p) => format!("{}://{host}:{p}", u.scheme()),
            None => format!("{}://{host}", u.scheme()),
        };
        let parts: Vec<&str> = u
            .path_segments()
            .map(|s| s.filter(|p| !p.is_empty()).collect())
            .unwrap_or_default();
        let shape = "the topic URL looks like https://discourse.nixos.org/t/<slug>/<id> or https://discourse.nixos.org/t/<id>";
        let (slug, id) = match parts.as_slice() {
            ["t", id] => (None, *id),
            ["t", id, n] if id.parse::<u64>().is_ok() && n.parse::<u64>().is_ok() => (None, *id),
            ["t", slug, id] => (Some(*slug), *id),
            ["t", slug, id, _post] => (Some(*slug), *id),
            _ => anyhow::bail!("{url:?}: {shape}"),
        };
        let id: u64 = id
            .parse()
            .with_context(|| format!("{url:?}: {id:?} is not a topic id; {shape}"))?;
        Ok(Topic {
            base,
            id,
            slug: slug.map(str::to_string),
        })
    }

    /// The page of the thread, for people.
    pub fn url(&self) -> String {
        match &self.slug {
            Some(s) => format!("{}/t/{s}/{}", self.base, self.id),
            None => format!("{}/t/{}", self.base, self.id),
        }
    }

    /// One post of the thread, for people.
    pub fn post_url(&self, post_number: u64) -> String {
        format!("{}/{post_number}", self.url())
    }

    fn json_url(&self) -> String {
        format!("{}/t/{}.json", self.base, self.id)
    }

    fn posts_url(&self, ids: &[u64]) -> String {
        let mut u = format!("{}/t/{}/posts.json?", self.base, self.id);
        for (i, id) in ids.iter().enumerate() {
            if i > 0 {
                u.push('&');
            }
            u.push_str("post_ids[]=");
            u.push_str(&id.to_string());
        }
        u
    }
}

/// One post of the thread, as the store keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Post {
    pub id: u64,
    pub post_number: u64,
    pub username: String,
    pub created_at: DateTime<Utc>,
    /// The nixpkgs pull requests the post links to, each once.
    pub prs: Vec<u64>,
}

/// One ask for a review: a post that names the pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Mention {
    pub post_number: u64,
    pub username: String,
    pub created_at: DateTime<Utc>,
}

/// pull request → its asks, the newest first.
pub type Mentions = HashMap<u64, Vec<Mention>>;

pub(crate) fn mentions(posts: &[Post]) -> Mentions {
    let mut m: Mentions = HashMap::new();
    for p in posts {
        for n in &p.prs {
            m.entry(*n).or_default().push(Mention {
                post_number: p.post_number,
                username: p.username.clone(),
                created_at: p.created_at,
            });
        }
    }
    for v in m.values_mut() {
        v.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then(b.post_number.cmp(&a.post_number))
        });
    }
    m
}

/// The newest ask of each pull request, for the order of the queue.
pub(crate) fn latest(m: &Mentions) -> HashMap<u64, DateTime<Utc>> {
    m.iter()
        .filter_map(|(n, v)| v.first().map(|f| (*n, f.created_at)))
        .collect()
}

/// The `why` of a row of the queue, as the API shows it.
pub(crate) fn why(m: &[Mention]) -> Vec<String> {
    m.first()
        .map(|f| {
            let more = match m.len() {
                0 | 1 => String::new(),
                2 => ", and once before".to_string(),
                n => format!(", and {} times before", n - 1),
            };
            vec![format!(
                "Asked on Discourse by {} {} ago{more}",
                f.username,
                crate::web::ago(f.created_at)
            )]
        })
        .unwrap_or_default()
}

/// The same line on the page, with a link to the post.
pub(crate) fn why_html(topic: Option<&Topic>, m: &[Mention]) -> Html {
    let Some(f) = m.first() else {
        return Html::new();
    };
    let more = match m.len() {
        0 | 1 => Html::new(),
        2 => Html::from(", and once before"),
        n => html!(", and {} times before", n - 1),
    };
    let when = match topic {
        Some(t) => html!(
            "<a href=\"{}\">{} ago</a>",
            t.post_url(f.post_number),
            crate::web::ago(f.created_at)
        ),
        None => html!("{} ago", crate::web::ago(f.created_at)),
    };
    html!(
        "<div class=\"mt-1 color-fg-default\">{} Asked on Discourse by {} {when}{more}</div>",
        crate::web::icon(crate::web::octicon!("comment")),
        f.username
    )
}

/// What the task did, for `/state?tab=status` and `/api/v1/sync`.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Stats {
    pub passes: u64,
    pub requests: u64,
    pub failed_passes: u64,
    /// Failed passes in a row; the wait grows with it.
    pub failures_in_a_row: u32,
    /// The last pass that read the topic.
    pub checked_at: Option<DateTime<Utc>>,
    /// When the task tries next.
    pub next_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    /// What an operator can do about `last_error`.
    pub advice: Option<String>,
    pub title: Option<String>,
    /// Posts in the thread, by Discourse.
    pub posts: u64,
    /// Posts the store has.
    pub posts_read: u64,
    pub highest_post_number: u64,
    /// Pull requests with at least one ask.
    pub asked: u64,
    pub closed: bool,
}

/// The topic this server follows, and the counters of its task.
pub struct Discourse {
    topic: Option<Topic>,
    pub(crate) stats: Mutex<Stats>,
}

impl Default for Discourse {
    /// Off: no topic, no task.
    fn default() -> Self {
        Discourse::new(None)
    }
}

impl Discourse {
    pub fn new(topic: Option<Topic>) -> Self {
        Discourse {
            topic,
            stats: Default::default(),
        }
    }

    pub fn topic(&self) -> Option<&Topic> {
        self.topic.as_ref()
    }

    /// Whether this server reads the thread at all.
    pub fn on(&self) -> bool {
        self.topic.is_some()
    }

    pub fn stats(&self) -> serde_json::Value {
        let s = self.stats.lock().unwrap().clone();
        serde_json::json!({
            "on": self.on(),
            "topic": self.topic.as_ref().map(Topic::url),
            "every_seconds": EVERY.as_secs(),
            "passes": s.passes,
            "requests": s.requests,
            "failed_passes": s.failed_passes,
            "failures_in_a_row": s.failures_in_a_row,
            "checked_at": s.checked_at,
            "next_at": s.next_at,
            "last_error": s.last_error,
            "advice": s.advice,
            "title": s.title,
            "posts": s.posts,
            "posts_read": s.posts_read,
            "highest_post_number": s.highest_post_number,
            "asked": s.asked,
            "closed": s.closed,
        })
    }
}

/// Why a pass failed, with what to do about it. `Display` is the journal
/// line; `advice` the step for an operator.
#[derive(Debug)]
pub(crate) enum Fail {
    /// 429, or Discourse's own "slow down" answer.
    RateLimited { retry_after: Option<Duration> },
    /// 401, 403, 451, or an HTML page in place of JSON: a login wall or a
    /// challenge.
    Refused { status: u16, hint: &'static str },
    /// 404 or 410: the topic is gone, or private.
    Gone { status: u16 },
    /// 5xx, or no answer.
    Unavailable(String),
    /// The JSON is not the shape Discourse had.
    Shape(String),
    /// Our own database.
    Store(String),
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fail::RateLimited {
                retry_after: Some(d),
            } => {
                write!(
                    f,
                    "Discourse rate-limited us (429, Retry-After {}s)",
                    d.as_secs()
                )
            }
            Fail::RateLimited { retry_after: None } => write!(f, "Discourse rate-limited us (429)"),
            Fail::Refused { status, hint } => {
                write!(
                    f,
                    "Discourse refused the anonymous JSON API ({status}, {hint})"
                )
            }
            Fail::Gone { status } => write!(f, "the topic is gone ({status})"),
            Fail::Unavailable(e) => write!(f, "Discourse did not answer: {e}"),
            Fail::Shape(e) => write!(f, "the answer of Discourse has an unexpected shape: {e}"),
            Fail::Store(e) => write!(f, "the database refused the posts: {e}"),
        }
    }
}

impl std::error::Error for Fail {}

impl Fail {
    pub(crate) fn advice(&self) -> String {
        match self {
            Fail::RateLimited { .. } => {
                "Nothing to do once: the task waits as long as Discourse asks. If it stays, another client on this address shares the limit; check what else talks to the forum from here.".into()
            }
            Fail::Refused { .. } => format!(
                "Open {{topic}}.json in a browser without a login. If it needs one, or shows a challenge page, ask the forum admins to let the User-Agent \"maintainer-mode\" read the topic JSON. To turn the tab off, set {VAR} to an empty value."
            ),
            Fail::Gone { .. } => format!(
                "The thread was deleted, made private, or merged into another. Set {VAR} to the URL of the thread that replaced it, or to an empty value to turn the tab off."
            ),
            Fail::Unavailable(_) => {
                "Nothing to do once: the task tries again later, and the last good data stays on the site. If it stays for a day, check that this host reaches the forum, and the status page of the forum.".into()
            }
            Fail::Shape(_) => {
                "The Discourse API changed, or something else answers at the forum's address. Open the URL in a browser, then report the line above at https://github.com/numtide/maintainermode/issues.".into()
            }
            Fail::Store(_) => {
                "Check the disk of the database and the journal for other SQLite errors.".into()
            }
        }
    }

    /// How long to wait after this failure, before the retry ladder.
    fn wait(&self) -> Option<Duration> {
        match self {
            Fail::RateLimited { retry_after } => *retry_after,
            _ => None,
        }
    }
}

/// `t/<id>.json`: the thread, with every post id.
#[derive(Deserialize)]
struct TopicJson {
    title: Option<String>,
    posts_count: Option<u64>,
    highest_post_number: Option<u64>,
    #[serde(default)]
    closed: bool,
    #[serde(default)]
    archived: bool,
    post_stream: Stream,
}

#[derive(Deserialize)]
struct Stream {
    /// Every post id, in post order.
    #[serde(default)]
    stream: Vec<u64>,
    /// The first chunk, or the posts asked for.
    #[serde(default)]
    posts: Vec<PostJson>,
}

/// `t/<id>/posts.json`: the posts asked for.
#[derive(Deserialize)]
struct PostsJson {
    post_stream: Stream,
}

#[derive(Deserialize)]
struct PostJson {
    id: u64,
    post_number: u64,
    #[serde(default)]
    username: String,
    created_at: DateTime<Utc>,
    #[serde(default)]
    deleted_at: Option<DateTime<Utc>>,
    #[serde(default)]
    link_counts: Vec<Link>,
    /// The HTML; read only when `link_counts` is empty.
    #[serde(default)]
    cooked: String,
}

#[derive(Deserialize)]
struct Link {
    url: String,
}

/// The nixpkgs pull request a URL points at, if any:
/// `https://github.com/NixOS/nixpkgs/pull/123`, `/files` and `#…` too.
pub(crate) fn pr_of(url: &str) -> Option<u64> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("https://www.github.com/"))?;
    let mut parts = rest.splitn(4, '/');
    let org = parts.next()?;
    let repo = parts.next()?;
    let kind = parts.next()?;
    let number = parts.next()?;
    if !org.eq_ignore_ascii_case("NixOS") || !repo.eq_ignore_ascii_case("nixpkgs") || kind != "pull"
    {
        return None;
    }
    let digits: String = number.chars().take_while(|c| c.is_ascii_digit()).collect();
    (!digits.is_empty()
        && number[digits.len()..]
            .chars()
            .next()
            .is_none_or(|c| matches!(c, '/' | '#' | '?')))
    .then(|| digits.parse().ok())
    .flatten()
}

/// The pull requests a post links to, each once, in the order of the post.
fn prs_of(p: &PostJson) -> Vec<u64> {
    let mut out: Vec<u64> = Vec::new();
    let mut push = |n: u64| {
        if !out.contains(&n) {
            out.push(n);
        }
    };
    if p.link_counts.is_empty() {
        // an old post, or one whose links Discourse did not count
        for piece in p.cooked.split("href=\"").skip(1) {
            if let Some(end) = piece.find('"') {
                if let Some(n) = pr_of(&piece[..end]) {
                    push(n);
                }
            }
        }
    } else {
        for l in &p.link_counts {
            if let Some(n) = pr_of(&l.url) {
                push(n);
            }
        }
    }
    out
}

impl PostJson {
    /// A deleted post keeps its row, with no ask, so it is not read again.
    fn into_post(self) -> Post {
        let prs = if self.deleted_at.is_some() {
            Vec::new()
        } else {
            prs_of(&self)
        };
        Post {
            id: self.id,
            post_number: self.post_number,
            username: self.username,
            created_at: self.created_at,
            prs,
        }
    }
}

/// One GET; the body as JSON, or a [`Fail`] that says what happened.
async fn get_json<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
    stats: &Mutex<Stats>,
) -> Result<T, Fail> {
    stats.lock().unwrap().requests += 1;
    let res = http
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| Fail::Unavailable(format!("{url}: {e}")))?;
    let status = res.status();
    let retry_after = res
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let content_type = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    match status.as_u16() {
        200 => {}
        429 => return Err(Fail::RateLimited { retry_after }),
        s @ (401 | 403 | 451) => {
            return Err(Fail::Refused {
                status: s,
                hint: if content_type.contains("html") {
                    "an HTML page, likely a login or a challenge"
                } else {
                    "no access"
                },
            });
        }
        s @ (404 | 410) => return Err(Fail::Gone { status: s }),
        s if status.is_server_error() => {
            return Err(Fail::Unavailable(format!("{url}: HTTP {s}")));
        }
        s => return Err(Fail::Shape(format!("{url}: HTTP {s}"))),
    }
    if content_type.contains("text/html") {
        return Err(Fail::Refused {
            status: 200,
            hint: "an HTML page where JSON was expected, likely a challenge",
        });
    }
    let body = res
        .bytes()
        .await
        .map_err(|e| Fail::Unavailable(format!("{url}: {e}")))?;
    if body.len() > MAX_BODY {
        return Err(Fail::Shape(format!(
            "{url}: {} MB, more than a topic or a batch of posts",
            body.len() >> 20
        )));
    }
    serde_json::from_slice(&body).map_err(|e| Fail::Shape(format!("{url}: {e}")))
}

/// The result of one pass, for the journal.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub new_posts: usize,
    pub gone_posts: usize,
    pub asked: usize,
    pub closed: bool,
}

/// One pass: the topic, then the posts the store lacks, then the posts the
/// thread lost.
pub(crate) async fn pass(
    store: &Store,
    stats: &Mutex<Stats>,
    http: &reqwest::Client,
    topic: &Topic,
) -> Result<Outcome, Fail> {
    let t: TopicJson = get_json(http, &topic.json_url(), stats).await?;
    if t.post_stream.stream.is_empty() {
        return Err(Fail::Shape(format!(
            "{}: post_stream.stream is empty",
            topic.json_url()
        )));
    }
    let known: HashSet<u64> = store
        .discourse_post_ids()
        .map_err(|e| Fail::Store(format!("{e:#}")))?
        .into_iter()
        .collect();
    let in_thread: HashSet<u64> = t.post_stream.stream.iter().copied().collect();
    // the first chunk comes with the topic: one request saved on a backfill
    let first: Vec<Post> = t
        .post_stream
        .posts
        .into_iter()
        .filter(|p| !known.contains(&p.id) && in_thread.contains(&p.id))
        .map(PostJson::into_post)
        .collect();
    let have: HashSet<u64> = first.iter().map(|p| p.id).collect();
    let missing: Vec<u64> = t
        .post_stream
        .stream
        .iter()
        .copied()
        .filter(|id| !known.contains(id) && !have.contains(id))
        .collect();
    let mut new_posts = first.len();
    if !first.is_empty() {
        store
            .put_discourse_posts(&first)
            .map_err(|e| Fail::Store(format!("{e:#}")))?;
    }
    if !missing.is_empty() {
        tracing::info!(
            "discourse: topic {} has {} posts the store lacks: {} requests to go",
            topic.id,
            missing.len(),
            missing.len().div_ceil(BATCH)
        );
    }
    for (i, ids) in missing.chunks(BATCH).enumerate() {
        tokio::time::sleep(PAUSE).await;
        let batch: PostsJson = get_json(http, &topic.posts_url(ids), stats).await?;
        let posts: Vec<Post> = batch
            .post_stream
            .posts
            .into_iter()
            .filter(|p| in_thread.contains(&p.id))
            .map(PostJson::into_post)
            .collect();
        if posts.is_empty() {
            return Err(Fail::Shape(format!(
                "posts.json gave no post for {} ids ({}…)",
                ids.len(),
                ids[0]
            )));
        }
        // each batch goes to the store at once: a pass cut short keeps its work
        store
            .put_discourse_posts(&posts)
            .map_err(|e| Fail::Store(format!("{e:#}")))?;
        new_posts += posts.len();
        let batches = missing.len().div_ceil(BATCH);
        // a backfill tells where it is, every 10 batches and at the end
        if batches > 1 && ((i + 1).is_multiple_of(10) || i + 1 == batches) {
            tracing::info!(
                "discourse: batch {}/{batches}: {new_posts} posts read so far",
                i + 1
            );
        }
    }
    // a post that left the thread was deleted or moved: its ask goes too
    let gone: Vec<u64> = known
        .iter()
        .copied()
        .filter(|id| !in_thread.contains(id))
        .collect();
    if !gone.is_empty() {
        store
            .delete_discourse_posts(&gone)
            .map_err(|e| Fail::Store(format!("{e:#}")))?;
    }
    let posts = store
        .discourse_posts()
        .map_err(|e| Fail::Store(format!("{e:#}")))?;
    let asked = mentions(&posts).len();
    let closed = t.closed || t.archived;
    {
        let mut s = stats.lock().unwrap();
        s.title = t.title;
        s.posts = t.posts_count.unwrap_or(in_thread.len() as u64);
        s.highest_post_number = t.highest_post_number.unwrap_or(0);
        s.posts_read = posts.len() as u64;
        s.asked = asked as u64;
        s.closed = closed;
        s.checked_at = Some(Utc::now());
    }
    Ok(Outcome {
        new_posts,
        gone_posts: gone.len(),
        asked,
        closed,
    })
}

pub async fn run(app: Arc<crate::web::App>) {
    let Some(topic) = app.discourse.topic().cloned() else {
        tracing::info!("discourse: off ({VAR} is empty): /review has no Asked on Discourse");
        return;
    };
    let http = reqwest::Client::builder()
        .user_agent("maintainer-mode (+https://github.com/numtide/maintainermode)")
        .timeout(TIMEOUT)
        .build()
        .expect("http client");
    tracing::info!(
        "discourse: following {} every {} min",
        topic.url(),
        EVERY.as_secs() / 60
    );
    // the other tasks start first; this one is the least urgent
    tokio::time::sleep(Duration::from_secs(45)).await;
    let mut warned_closed = false;
    loop {
        let stats = &app.discourse.stats;
        stats.lock().unwrap().passes += 1;
        let wait = match pass(&app.store, stats, &http, &topic).await {
            Ok(o) => {
                {
                    let mut s = stats.lock().unwrap();
                    s.failures_in_a_row = 0;
                    s.last_error = None;
                    s.advice = None;
                }
                if o.new_posts > 0 || o.gone_posts > 0 {
                    tracing::info!(
                        "discourse: {} new posts, {} gone, {} pull requests asked for review",
                        o.new_posts,
                        o.gone_posts,
                        o.asked
                    );
                    // the queue takes the new asks at the next recompute;
                    // this makes it now
                    let a = app.clone();
                    match tokio::task::spawn_blocking(move || a.recompute()).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => tracing::warn!("discourse: recompute: {e:#}"),
                        Err(e) => tracing::warn!("discourse: recompute: {e:#}"),
                    }
                } else {
                    tracing::debug!("discourse: no new post");
                }
                if o.closed && !warned_closed {
                    tracing::warn!(
                        "discourse: the topic {} is closed: new asks go to another thread. Set {VAR} to its URL when you know it; the asks of this one stay until their pull requests close",
                        topic.url()
                    );
                }
                warned_closed = o.closed;
                EVERY
            }
            Err(e) => {
                let advice = e.advice().replace("{topic}", &topic.url());
                let (n, wait) = {
                    let mut s = stats.lock().unwrap();
                    s.failed_passes += 1;
                    s.failures_in_a_row += 1;
                    let n = s.failures_in_a_row;
                    s.last_error = Some(e.to_string());
                    s.advice = Some(advice.clone());
                    (
                        n,
                        e.wait()
                            .unwrap_or_else(|| RETRIES[(n as usize - 1).min(RETRIES.len() - 1)]),
                    )
                };
                tracing::warn!(
                    "discourse: {e}. {advice} Next try in {} min (failure {n} in a row)",
                    wait.as_secs().div_ceil(60)
                );
                wait
            }
        };
        stats.lock().unwrap().next_at = Some(
            Utc::now() + chrono::Duration::from_std(wait).unwrap_or(chrono::Duration::hours(1)),
        );
        tokio::time::sleep(wait).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::extract::{Query, State};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn a_topic_url_in_its_shapes() {
        let full = Topic::parse(DEFAULT_TOPIC).unwrap();
        assert_eq!(
            full,
            Topic {
                base: "https://discourse.nixos.org".into(),
                id: 3032,
                slug: Some("prs-ready-for-review".into()),
            }
        );
        assert_eq!(full.url(), DEFAULT_TOPIC);
        assert_eq!(full.post_url(7971), format!("{DEFAULT_TOPIC}/7971"));
        assert_eq!(full.json_url(), "https://discourse.nixos.org/t/3032.json");
        assert_eq!(
            Topic::parse("https://discourse.nixos.org/t/prs-ready-for-review/3032/7971").unwrap(),
            full,
            "a post number at the end is dropped"
        );
        let bare = Topic::parse("https://discourse.nixos.org/t/3032/").unwrap();
        assert_eq!(bare.slug, None);
        assert_eq!(bare.url(), "https://discourse.nixos.org/t/3032");
        assert_eq!(
            Topic::parse("http://127.0.0.1:8080/t/3032/12")
                .unwrap()
                .base,
            "http://127.0.0.1:8080",
            "a bare id and a post number"
        );
        assert_eq!(
            Topic::parse("http://localhost:1/t/slug/3032")
                .unwrap()
                .posts_url(&[1, 2]),
            "http://localhost:1/t/3032/posts.json?post_ids[]=1&post_ids[]=2"
        );
        for bad in [
            "discourse.nixos.org/t/3032",
            "https://discourse.nixos.org/",
            "https://discourse.nixos.org/t/prs-ready-for-review",
            "https://discourse.nixos.org/c/dev/3032",
            "ftp://discourse.nixos.org/t/3032",
        ] {
            let e = Topic::parse(bad).unwrap_err().to_string();
            assert!(e.contains(bad), "{bad}: {e}");
        }
    }

    #[test]
    fn a_link_names_a_nixpkgs_pull_request_or_nothing() {
        assert_eq!(
            pr_of("https://github.com/NixOS/nixpkgs/pull/583673"),
            Some(583673)
        );
        assert_eq!(
            pr_of("https://github.com/nixos/nixpkgs/pull/1/files"),
            Some(1)
        );
        assert_eq!(
            pr_of("https://github.com/NixOS/nixpkgs/pull/2#issuecomment-9"),
            Some(2)
        );
        assert_eq!(
            pr_of("https://github.com/NixOS/nixpkgs/pull/3?diff=split"),
            Some(3)
        );
        assert_eq!(pr_of("https://github.com/NixOS/nixpkgs/pulls"), None);
        assert_eq!(pr_of("https://github.com/NixOS/nixpkgs/issues/4"), None);
        assert_eq!(pr_of("https://github.com/NixOS/nix/pull/5"), None);
        assert_eq!(pr_of("https://github.com/NixOS/nixpkgs/pull/abc"), None);
        assert_eq!(pr_of("https://gitlab.com/NixOS/nixpkgs/pull/6"), None);
    }

    fn post_json(id: u64, number: u64, links: &[&str], cooked: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "post_number": number, "username": format!("u{number}"),
            "created_at": (DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap()
                + chrono::Duration::hours(number as i64)).to_rfc3339(),
            "deleted_at": null,
            "link_counts": links.iter().map(|u| serde_json::json!({"url": u})).collect::<Vec<_>>(),
            "cooked": cooked,
        })
    }

    #[test]
    fn the_links_of_a_post_each_once_from_the_counts_or_the_html() {
        let p: PostJson = serde_json::from_value(post_json(
            1,
            1,
            &[
                "https://github.com/NixOS/nixpkgs/pull/10",
                "https://github.com/NixOS/nixpkgs/issues/11",
                "https://github.com/NixOS/nixpkgs/pull/10/files",
                "https://github.com/NixOS/nixpkgs/pull/12",
            ],
            "",
        ))
        .unwrap();
        assert_eq!(p.into_post().prs, [10, 12]);
        let p: PostJson = serde_json::from_value(post_json(
            2,
            2,
            &[],
            "<p>Please: <a href=\"https://github.com/NixOS/nixpkgs/pull/20\">#20</a> and \
             <a href=\"https://github.com/NixOS/nixpkgs/pull/20\">again</a>, \
             <a href=\"https://example.org\">x</a></p>",
        ))
        .unwrap();
        assert_eq!(p.into_post().prs, [20], "without counts, the HTML");
    }

    #[test]
    fn the_mentions_of_a_pull_request_newest_first() {
        let at = |d: u32| {
            DateTime::parse_from_rfc3339(&format!("2026-01-{d:02}T00:00:00Z"))
                .unwrap()
                .with_timezone(&Utc)
        };
        let posts = vec![
            Post {
                id: 1,
                post_number: 1,
                username: "a".into(),
                created_at: at(1),
                prs: vec![7, 8],
            },
            Post {
                id: 2,
                post_number: 2,
                username: "b".into(),
                created_at: at(5),
                prs: vec![7],
            },
            Post {
                id: 3,
                post_number: 3,
                username: "c".into(),
                created_at: at(3),
                prs: vec![],
            },
        ];
        let m = mentions(&posts);
        assert_eq!(m.len(), 2);
        assert_eq!(
            m[&7].iter().map(|x| x.post_number).collect::<Vec<_>>(),
            [2, 1]
        );
        assert_eq!(latest(&m)[&7], at(5));
        assert_eq!(latest(&m)[&8], at(1));
        let w = why(&m[&7]);
        assert_eq!(w.len(), 1);
        assert!(w[0].starts_with("Asked on Discourse by b "), "{w:?}");
        assert!(w[0].ends_with(", and once before"), "{w:?}");
        let h = why_html(Some(&Topic::parse(DEFAULT_TOPIC).unwrap()), &m[&7]).to_string();
        assert!(h.contains(&format!("href=\"{DEFAULT_TOPIC}/2\"")), "{h}");
        assert!(why(&[]).is_empty());
    }

    /// A forced answer: status, content type, body.
    type Answer = (u16, &'static str, &'static str);

    /// A stub forum: a thread of `ids`, the first 20 with the topic, the
    /// rest by `posts.json`. `hits` counts the requests.
    #[derive(Clone)]
    struct Forum {
        ids: Arc<std::sync::RwLock<Vec<u64>>>,
        hits: Arc<AtomicU64>,
        answer: Arc<std::sync::RwLock<Option<Answer>>>,
        closed: Arc<std::sync::RwLock<bool>>,
    }

    impl Forum {
        fn post(&self, id: u64) -> serde_json::Value {
            // post id 100+n is post number n; even numbers ask for a PR
            let n = id - 100;
            let links: Vec<String> = if n.is_multiple_of(2) {
                vec![format!(
                    "https://github.com/NixOS/nixpkgs/pull/{}",
                    1000 + n
                )]
            } else {
                vec![]
            };
            let links: Vec<&str> = links.iter().map(String::as_str).collect();
            post_json(id, n, &links, "")
        }
    }

    async fn forum() -> (String, Forum) {
        let f = Forum {
            ids: Arc::new(std::sync::RwLock::new((101..=150).collect())),
            hits: Default::default(),
            answer: Default::default(),
            closed: Default::default(),
        };
        async fn topic(State(f): State<Forum>) -> axum::response::Response {
            f.hits.fetch_add(1, Ordering::SeqCst);
            if let Some((status, ct, body)) = *f.answer.read().unwrap() {
                return (
                    StatusCode::from_u16(status).unwrap(),
                    [("content-type", ct), ("retry-after", "7")],
                    body,
                )
                    .into_response();
            }
            let ids = f.ids.read().unwrap().clone();
            let first: Vec<_> = ids.iter().take(20).map(|id| f.post(*id)).collect();
            axum::Json(serde_json::json!({
                "title": "PRs ready for review",
                "posts_count": ids.len(),
                "highest_post_number": ids.last().map(|i| i - 100),
                "closed": *f.closed.read().unwrap(),
                "archived": false,
                "post_stream": {"stream": ids, "posts": first},
            }))
            .into_response()
        }
        async fn posts(
            State(f): State<Forum>,
            Query(q): Query<Vec<(String, u64)>>,
        ) -> axum::response::Response {
            f.hits.fetch_add(1, Ordering::SeqCst);
            let posts: Vec<_> = q
                .iter()
                .filter(|(k, _)| k == "post_ids[]")
                .map(|(_, id)| f.post(*id))
                .collect();
            axum::Json(serde_json::json!({"post_stream": {"posts": posts}})).into_response()
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/t/3032.json", get(topic))
            .route("/t/3032/posts.json", get(posts))
            .with_state(f.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, f)
    }

    #[tokio::test]
    async fn a_pass_reads_what_the_store_lacks_and_drops_what_the_thread_lost() {
        let (base, f) = forum().await;
        let topic = Topic::parse(&format!("{base}/t/3032")).unwrap();
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let stats = Mutex::new(Stats::default());
        let http = reqwest::Client::new();

        let o = pass(&store, &stats, &http, &topic).await.unwrap();
        assert_eq!(
            o,
            Outcome {
                new_posts: 50,
                gone_posts: 0,
                asked: 25,
                closed: false
            }
        );
        assert_eq!(
            f.hits.load(Ordering::SeqCst),
            2,
            "the topic, then one batch of 30"
        );
        let m = mentions(&store.discourse_posts().unwrap());
        assert_eq!(m[&1002][0].username, "u2");
        let s = stats.lock().unwrap().clone();
        assert_eq!(
            (s.posts, s.posts_read, s.asked, s.highest_post_number),
            (50, 50, 25, 50)
        );
        assert_eq!(s.title.as_deref(), Some("PRs ready for review"));

        // nothing new: one request, nothing written
        let o = pass(&store, &stats, &http, &topic).await.unwrap();
        assert_eq!(o.new_posts, 0);
        assert_eq!(f.hits.load(Ordering::SeqCst), 3);

        // two new posts, one old one deleted
        {
            let mut ids = f.ids.write().unwrap();
            ids.retain(|id| *id != 102);
            ids.extend([151, 152]);
        }
        let o = pass(&store, &stats, &http, &topic).await.unwrap();
        assert_eq!(
            o,
            Outcome {
                new_posts: 2,
                gone_posts: 1,
                asked: 25,
                closed: false
            },
            "#1002 left with its post, #1052 came"
        );
        assert_eq!(f.hits.load(Ordering::SeqCst), 5);
        let m = mentions(&store.discourse_posts().unwrap());
        assert!(!m.contains_key(&1002));
        assert!(m.contains_key(&1052));

        *f.closed.write().unwrap() = true;
        assert!(pass(&store, &stats, &http, &topic).await.unwrap().closed);
    }

    #[tokio::test]
    async fn each_failure_says_what_happened_and_what_to_do() {
        let (base, f) = forum().await;
        let topic = Topic::parse(&format!("{base}/t/3032")).unwrap();
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let stats = Mutex::new(Stats::default());
        let http = reqwest::Client::new();
        let fail = |status: u16, ct: &'static str, body: &'static str| {
            *f.answer.write().unwrap() = Some((status, ct, body));
        };

        fail(429, "application/json", "{}");
        let e = pass(&store, &stats, &http, &topic).await.unwrap_err();
        assert!(
            matches!(e, Fail::RateLimited { retry_after: Some(d) } if d.as_secs() == 7),
            "{e:?}"
        );
        assert!(e.to_string().contains("Retry-After 7s"));

        fail(
            403,
            "text/html; charset=utf-8",
            "<html>Just a moment</html>",
        );
        let e = pass(&store, &stats, &http, &topic).await.unwrap_err();
        assert!(matches!(e, Fail::Refused { status: 403, .. }), "{e:?}");
        assert!(e.to_string().contains("challenge"), "{e}");
        assert!(e.advice().contains(VAR));

        fail(200, "text/html", "<html>login</html>");
        assert!(matches!(
            pass(&store, &stats, &http, &topic).await.unwrap_err(),
            Fail::Refused { status: 200, .. }
        ));

        fail(404, "application/json", "{\"errors\":[\"not found\"]}");
        let e = pass(&store, &stats, &http, &topic).await.unwrap_err();
        assert!(matches!(e, Fail::Gone { status: 404 }), "{e:?}");
        assert!(e.advice().contains("replaced it"), "{}", e.advice());

        fail(503, "text/plain", "down");
        assert!(matches!(
            pass(&store, &stats, &http, &topic).await.unwrap_err(),
            Fail::Unavailable(_)
        ));

        fail(
            200,
            "application/json",
            "{\"post_stream\": {\"stream\": []}}",
        );
        let e = pass(&store, &stats, &http, &topic).await.unwrap_err();
        assert!(matches!(e, Fail::Shape(_)), "{e:?}");
        assert!(e.to_string().contains("stream is empty"), "{e}");

        fail(200, "application/json", "not json");
        assert!(matches!(
            pass(&store, &stats, &http, &topic).await.unwrap_err(),
            Fail::Shape(_)
        ));

        // a host that does not answer
        let dead = Topic::parse("http://127.0.0.1:9/t/1").unwrap();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        assert!(matches!(
            pass(&store, &stats, &http, &dead).await.unwrap_err(),
            Fail::Unavailable(_)
        ));
        assert_eq!(stats.lock().unwrap().requests, 8);
    }

    #[test]
    fn the_words_of_the_interval_match_it() {
        assert_eq!(EVERY.as_secs(), 3600);
        assert_eq!(EVERY_WORDS, "hour");
    }

    #[test]
    fn the_retry_ladder_grows_then_stays() {
        let wait = |n: usize| RETRIES[(n - 1).min(RETRIES.len() - 1)];
        assert_eq!(wait(1).as_secs(), 600);
        assert_eq!(wait(5).as_secs(), 6 * 3600);
        assert_eq!(wait(50).as_secs(), 6 * 3600);
        assert!(
            EVERY <= wait(3),
            "the first retries come sooner than a pass"
        );
    }
}
