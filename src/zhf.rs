//! Zero Hydra Failures: every failing job of the jobsets that the campaign
//! watches, read from Hydra, so a person sees the failures of what they
//! maintain, picks one that nobody owns, and everyone sees how far the
//! community is.
//!
//! Hydra has no JSON for the failing jobs of an evaluation: `/eval/<id>`
//! gives 150,000 build ids and nothing else. The HTML page with `?full=1`
//! lists every build in tabs, the failing ones first, so the read stops at
//! the "Still Succeeding" tab: about 2 MB of 100 MB. Hydra renders the
//! whole page before its first byte, so one read costs it about 30 s. A
//! page is read once per new evaluation, which comes about once a day, and
//! `latest-eval`, one redirect, says when there is one.
//!
//! The campaign is the open nixpkgs issue "NixOS NN.MM – Zero Hydra
//! Failures". It starts on master, and moves to `release-NN.MM` when that
//! jobset exists. With no open issue, the page follows master: the next
//! campaign starts there.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration as StdDuration;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;

use crate::area::Index;
use crate::catalog::Subject;
use crate::html::{Html, html};
use crate::ui::{Page, Section, Tab, Tone, badge, empty, item_row};
use crate::web::{App, Shared, ago, icon, octicon};

pub(crate) const PATH: &str = "/zhf";
const EVERY: StdDuration = StdDuration::from_secs(15 * 60);
/// Hydra renders the whole page before it answers; see the module doc.
const PAGE_TIMEOUT: StdDuration = StdDuration::from_secs(300);
/// The read stops here: what follows is the builds that pass.
const END_MARK: &str = "id=\"tabs-still-succeed\"";
/// A page with no end mark in this much is not an evaluation page.
const MAX_PAGE: usize = 48 << 20;
const KEEP_DAYS: i64 = 120;
const ROWS: usize = 50;
pub(crate) const TABS: [(&str, &str); 4] = [
    ("yours", "Yours"),
    ("unowned", "No maintainer"),
    ("all", "All"),
    ("pick", "Pick one"),
];

/// Why a build is on the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    /// The build itself failed, timed out, or hit a limit: the place to fix.
    Failed,
    /// A dependency failed; it passes once that one is fixed.
    DependencyFailed,
    /// Aborted or cancelled: says nothing about the package.
    Aborted,
}

impl Why {
    /// From the `title` of Hydra's status icon. `None` for a build that
    /// passed.
    fn of(title: &str) -> Option<Why> {
        match title {
            "Succeeded" => None,
            "Dependency failed" => Some(Why::DependencyFailed),
            "Aborted" | "Cancelled" | "Unsupported system" => Some(Why::Aborted),
            _ => Some(Why::Failed),
        }
    }
}

/// One failing build, as the evaluation page lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub build: u64,
    /// Like `nixpkgs.hello.x86_64-linux`.
    pub job: String,
    /// Hydra's words: "Failed", "Dependency failed", "Timed out".
    pub status: String,
    pub why: Why,
    pub system: String,
    /// When the build finished.
    pub at: Option<i64>,
    /// Like `hello-2.12.1`.
    #[serde(default)]
    pub name: Option<String>,
}

/// What an evaluation counts, by Hydra's tabs and by ours.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub failed: u64,
    pub dependency_failed: u64,
    pub aborted: u64,
    /// system → (failed, dependency failed).
    pub by_system: BTreeMap<String, (u64, u64)>,
    /// Hydra's own tabs: `still-fail`, `now-fail`, `still-succeed`, …
    pub tabs: BTreeMap<String, u64>,
}

impl Counts {
    /// The failing builds: the ones that fail and the ones a failure blocks.
    pub fn failing(&self) -> u64 {
        self.failed + self.dependency_failed
    }

    fn add(&mut self, r: &Row) {
        let s = self.by_system.entry(r.system.clone()).or_default();
        match r.why {
            Why::Failed => {
                self.failed += 1;
                s.0 += 1;
            }
            Why::DependencyFailed => {
                self.dependency_failed += 1;
                s.1 += 1;
            }
            Why::Aborted => self.aborted += 1,
        }
    }
}

/// One evaluation of one jobset, with its failing builds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Eval {
    pub jobset: String,
    pub id: u64,
    /// When Hydra evaluated.
    pub at: DateTime<Utc>,
    pub fetched_at: DateTime<Utc>,
    pub counts: Counts,
    #[serde(default)]
    pub rows: Vec<Row>,
}

impl Eval {
    pub fn url(&self) -> String {
        format!("{}/eval/{}", crate::health::HYDRA, self.id)
    }
}

/// One point of the burndown.
#[derive(Debug, Clone, Serialize)]
pub struct Point {
    pub jobset: String,
    pub eval: u64,
    pub at: DateTime<Utc>,
    pub failing: u64,
    pub failed: u64,
}

/// What the evaluation page says.
#[derive(Debug, Default)]
pub(crate) struct Parsed {
    pub at: Option<i64>,
    pub tabs: BTreeMap<String, u64>,
    pub rows: Vec<Row>,
}

fn attr_between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let i = s.find(open)? + open.len();
    let j = s[i..].find(close)? + i;
    Some(&s[i..j])
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

/// The text of each `<a …>text</a>` of a row.
fn link_texts(row: &str) -> Vec<&str> {
    row.split("<a ")
        .skip(1)
        .filter_map(|a| {
            let i = a.find('>')? + 1;
            let j = a[i..].find("</a>")? + i;
            Some(a[i..j].trim())
        })
        .collect()
}

/// One `<tr>` of a build table; `None` when it is not one.
fn parse_row(tr: &str) -> Option<Row> {
    let status = attr_between(tr, "title=\"", "\" alt=\"")?;
    let why = Why::of(status)?;
    let links = link_texts(tr);
    let build: u64 = links.first()?.parse().ok()?;
    let job = unescape(links.get(1)?);
    let system = attr_between(tr, "<tt>", "</tt>")?.to_string();
    let at = attr_between(tr, "data-timestamp=\"", "\"").and_then(|t| t.parse().ok());
    let name = tr
        .find("</time>")
        .and_then(|i| attr_between(&tr[i..], "<td>", "</td>"))
        .map(|n| unescape(n.trim()))
        .filter(|n| !n.is_empty());
    Some(Row {
        build,
        job,
        status: status.to_string(),
        why,
        system,
        at,
        name,
    })
}

/// The page up to the end mark. The tabs before it hold every build that
/// did not pass, and some that did; one build can sit in two tabs.
pub(crate) fn parse(html: &str) -> Parsed {
    let html = match html.find(END_MARK) {
        Some(i) => &html[..i],
        None => html,
    };
    let mut out = Parsed::default();
    if let Some(nav) = attr_between(html, "<ul class=\"nav nav-tabs\">", "</ul>") {
        for a in nav.split("href=\"#tabs-").skip(1) {
            let Some(id) = a.split('"').next() else {
                continue;
            };
            let text = a.split("</a>").next().unwrap_or("");
            let n = text
                .rsplit_once('(')
                .and_then(|(_, n)| n.split(')').next())
                .and_then(|n| n.trim().parse().ok())
                .unwrap_or(0);
            out.tabs.insert(id.to_string(), n);
        }
    }
    out.at = html
        .find("This evaluation was performed")
        .and_then(|i| attr_between(&html[i..], "data-timestamp=\"", "\""))
        .and_then(|t| t.parse().ok());
    let mut seen = HashSet::new();
    for tr in html.split("<tr>").skip(1) {
        let tr = tr.split("</tr>").next().unwrap_or("");
        if !tr.contains("class=\"build-status\"") {
            continue;
        }
        if let Some(r) = parse_row(tr) {
            if seen.insert(r.build) {
                out.rows.push(r);
            }
        }
    }
    out
}

/// A Hydra jobset the campaign watches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Source {
    pub project: String,
    pub name: String,
    /// `nixpkgs.` in the NixOS jobsets, whose jobs are
    /// `nixpkgs.hello.x86_64-linux`; nothing in the nixpkgs ones.
    prefix: &'static str,
    /// The nixpkgs jobsets build Linux too; the NixOS ones already have it.
    darwin_only: bool,
}

impl Source {
    pub fn key(&self) -> String {
        format!("{}/{}", self.project, self.name)
    }

    pub fn label(&self) -> String {
        format!("{}:{}", self.project, self.name)
    }

    pub fn url(&self) -> String {
        format!("{}/jobset/{}", crate::health::HYDRA, self.key())
    }

    fn keep(&self, r: &Row) -> bool {
        !self.darwin_only || r.system.ends_with("-darwin")
    }

    /// What a job builds: the attribute, the NixOS test, or something else
    /// of the release, like an installer image.
    pub fn kind(&self, job: &str, system: &str) -> Kind {
        let base = job
            .strip_suffix(system)
            .and_then(|j| j.strip_suffix('.'))
            .unwrap_or(job);
        if let Some(t) = base.strip_prefix("nixos.tests.") {
            return Kind::Test(t.to_string());
        }
        match base.strip_prefix(self.prefix) {
            Some(attr) if !attr.starts_with("nixos.") => Kind::Package(attr.to_string()),
            _ => Kind::Other(base.to_string()),
        }
    }
}

/// Master, or a release branch.
fn sources(release: Option<&str>) -> Vec<Source> {
    match release {
        Some(v) => vec![
            Source {
                project: "nixos".into(),
                name: format!("release-{v}"),
                prefix: "nixpkgs.",
                darwin_only: false,
            },
            Source {
                project: "nixpkgs".into(),
                name: format!("nixpkgs-{v}-darwin"),
                prefix: "",
                darwin_only: true,
            },
        ],
        None => vec![
            Source {
                project: "nixos".into(),
                name: "unstable".into(),
                prefix: "nixpkgs.",
                darwin_only: false,
            },
            Source {
                project: "nixpkgs".into(),
                name: "unstable".into(),
                prefix: "",
                darwin_only: true,
            },
        ],
    }
}

/// The open tracking issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Campaign {
    pub number: u64,
    pub title: String,
    /// Like `26.11`.
    pub release: String,
    pub opened_at: DateTime<Utc>,
}

impl Campaign {
    pub fn url(&self) -> String {
        format!("https://github.com/NixOS/nixpkgs/issues/{}", self.number)
    }
}

/// `NN.MM` in a title.
fn release_in(title: &str) -> Option<String> {
    let b = title.as_bytes();
    (0..b.len().saturating_sub(4)).find_map(|i| {
        let w = &b[i..i + 5];
        let digit = |x: u8| x.is_ascii_digit();
        let before = i == 0 || !digit(b[i - 1]);
        let after = i + 5 >= b.len() || !digit(b[i + 5]);
        (before
            && after
            && digit(w[0])
            && digit(w[1])
            && w[2] == b'.'
            && digit(w[3])
            && digit(w[4]))
        .then(|| title[i..i + 5].to_string())
    })
}

/// The newest open issue named "Zero Hydra Failures" with a release in its
/// title.
pub(crate) fn campaign(issues: &[crate::model::Issue]) -> Option<Campaign> {
    issues
        .iter()
        .filter(|i| i.is_open() && i.title.to_lowercase().contains("zero hydra failures"))
        .filter_map(|i| {
            Some(Campaign {
                number: i.number,
                title: i.title.clone(),
                release: release_in(&i.title)?,
                opened_at: i.created_at,
            })
        })
        .max_by_key(|c| (c.opened_at, c.number))
}

/// What the page shows: the campaign, its jobsets, their newest
/// evaluations and the burndown.
#[derive(Default)]
pub struct Snapshot {
    pub campaign: Option<Campaign>,
    /// The release jobset exists: the campaign left master.
    pub branched: bool,
    pub sources: Vec<Source>,
    /// The newest evaluation of each source, in the order of `sources`.
    pub evals: Vec<Eval>,
    /// Every stored evaluation of the sources, the oldest first.
    pub history: Vec<Point>,
    pub checked_at: Option<DateTime<Utc>>,
}

impl Snapshot {
    pub fn eval_of(&self, source: &Source) -> Option<&Eval> {
        self.evals.iter().find(|e| e.jobset == source.key())
    }

    /// The failing builds now, over every source.
    pub fn failing(&self) -> u64 {
        self.evals.iter().map(|e| e.counts.failing()).sum()
    }

    /// The failing builds at each evaluation, the newest count of each
    /// jobset summed: the burndown. It starts once every jobset has a
    /// count, so a jobset that joins does not look like a rise.
    pub fn burndown(&self) -> Vec<(DateTime<Utc>, u64)> {
        let mut last: BTreeMap<&str, u64> = BTreeMap::new();
        let mut out = Vec::new();
        for p in &self.history {
            last.insert(&p.jobset, p.failing);
            if last.len() >= self.sources.len() {
                out.push((p.at, last.values().sum()));
            }
        }
        out
    }

    /// The count the campaign started from: the first evaluation after the
    /// issue opened, or the oldest kept with no campaign.
    pub fn start(&self) -> Option<(DateTime<Utc>, u64)> {
        let since = self.campaign.as_ref().map(|c| c.opened_at);
        self.burndown()
            .into_iter()
            .find(|(at, _)| since.is_none_or(|s| *at >= s))
    }
}

#[derive(Default, Clone, Serialize)]
struct Stats {
    passes: u64,
    probes: u64,
    pages: u64,
    bytes: u64,
    failed_passes: u64,
    last_error: Option<String>,
    page_at: Option<DateTime<Utc>>,
}

/// The state in memory, and the counters of the task.
pub struct Zhf {
    snapshot: RwLock<Arc<Snapshot>>,
    stats: Mutex<Stats>,
}

impl Default for Zhf {
    fn default() -> Self {
        Zhf {
            snapshot: RwLock::new(Arc::new(Snapshot::default())),
            stats: Default::default(),
        }
    }
}

impl Zhf {
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.read().unwrap().clone()
    }

    pub fn stats(&self) -> serde_json::Value {
        let s = self.stats.lock().unwrap().clone();
        let snap = self.snapshot();
        serde_json::json!({
            "campaign": snap.campaign.as_ref().map(|c| c.number),
            "release": snap.campaign.as_ref().map(|c| c.release.clone()),
            "branched": snap.branched,
            "jobsets": snap.sources.iter().map(Source::key).collect::<Vec<_>>(),
            "evals": snap.evals.iter().map(|e| serde_json::json!({
                "jobset": e.jobset, "id": e.id, "at": e.at, "fetched_at": e.fetched_at,
                "failed": e.counts.failed, "dependency_failed": e.counts.dependency_failed,
            })).collect::<Vec<_>>(),
            "failing": snap.failing(),
            "checked_at": snap.checked_at,
            "every_seconds": EVERY.as_secs(),
            "passes": s.passes,
            "probes": s.probes,
            "pages": s.pages,
            "bytes": s.bytes,
            "failed_passes": s.failed_passes,
            "last_error": s.last_error,
            "page_at": s.page_at,
        })
    }
}

/// The id of the newest evaluation of a jobset: `latest-eval` redirects
/// to it. `None` when Hydra has no such jobset.
async fn latest_eval(http: &reqwest::Client, base: &str, jobset: &str) -> Result<Option<u64>> {
    let resp = http
        .get(format!("{base}/jobset/{jobset}/latest-eval"))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    anyhow::ensure!(
        resp.status().is_redirection(),
        "latest-eval of {jobset} answered {}",
        resp.status()
    );
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .context("latest-eval without Location")?;
    let id = location
        .split("/eval/")
        .nth(1)
        .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|s| s.parse().ok())
        .with_context(|| format!("latest-eval of {jobset} points to {location}"))?;
    Ok(Some(id))
}

/// The evaluation page, up to the end mark.
async fn eval_page(http: &reqwest::Client, base: &str, id: u64) -> Result<(String, u64)> {
    let mut resp = http
        .get(format!("{base}/eval/{id}?full=1"))
        .send()
        .await?
        .error_for_status()?;
    let mut buf = Vec::new();
    let mut read: u64 = 0;
    while let Some(chunk) = resp.chunk().await? {
        read += chunk.len() as u64;
        let from = buf.len().saturating_sub(END_MARK.len());
        buf.extend_from_slice(&chunk);
        if buf[from..]
            .windows(END_MARK.len())
            .any(|w| w == END_MARK.as_bytes())
        {
            break;
        }
        anyhow::ensure!(
            buf.len() < MAX_PAGE,
            "eval {id}: no end mark in {MAX_PAGE} bytes"
        );
    }
    Ok((String::from_utf8_lossy(&buf).into_owned(), read))
}

/// Reads and stores the newest evaluation of a source, when the server
/// does not have it yet. Gives whether the jobset exists.
async fn refresh_source(
    app: &App,
    http: &reqwest::Client,
    base: &str,
    src: &Source,
    known: Option<u64>,
) -> Result<bool> {
    app.hydra.zhf.stats.lock().unwrap().probes += 1;
    let Some(id) = latest_eval(http, base, &src.key()).await? else {
        return Ok(false);
    };
    if known == Some(id) || app.store.zhf_has_eval(&src.key(), id)? {
        return Ok(true);
    }
    let (html, read) = eval_page(http, base, id).await?;
    let parsed = tokio::task::spawn_blocking(move || parse(&html)).await?;
    let now = Utc::now();
    let mut counts = Counts {
        tabs: parsed.tabs,
        ..Default::default()
    };
    let rows: Vec<Row> = parsed
        .rows
        .into_iter()
        .filter(|r| src.keep(r))
        .inspect(|r| counts.add(r))
        .collect();
    let eval = Eval {
        jobset: src.key(),
        id,
        at: parsed
            .at
            .and_then(|t| DateTime::from_timestamp(t, 0))
            .unwrap_or(now),
        fetched_at: now,
        counts,
        rows,
    };
    tracing::info!(
        "zhf: {} eval {id}: {} failed, {} dependency failed, {} aborted",
        src.key(),
        eval.counts.failed,
        eval.counts.dependency_failed,
        eval.counts.aborted
    );
    app.store.put_zhf_eval(&eval)?;
    let mut s = app.hydra.zhf.stats.lock().unwrap();
    s.pages += 1;
    s.bytes += read;
    s.page_at = Some(now);
    Ok(true)
}

/// The snapshot from the store, for the sources of the campaign.
fn load(app: &App, campaign: Option<Campaign>, branched: bool, sources: Vec<Source>) -> Snapshot {
    let mut evals = Vec::new();
    let mut history = Vec::new();
    for s in &sources {
        match app.store.zhf_latest(&s.key()) {
            Ok(Some(e)) => evals.push(e),
            Ok(None) => {}
            Err(e) => tracing::warn!("zhf latest {}: {e:#}", s.key()),
        }
        match app.store.zhf_history(&s.key()) {
            Ok(h) => history.extend(h),
            Err(e) => tracing::warn!("zhf history {}: {e:#}", s.key()),
        }
    }
    history.sort_by(|a, b| a.at.cmp(&b.at).then(a.eval.cmp(&b.eval)));
    Snapshot {
        campaign,
        branched,
        sources,
        evals,
        history,
        checked_at: Some(Utc::now()),
    }
}

/// The sources of `list` that Hydra has, each brought up to date. A
/// source that does not answer stays when the server read it before.
async fn refresh_all(
    app: &App,
    http: &reqwest::Client,
    base: &str,
    list: Vec<Source>,
    before: &Snapshot,
    errors: &mut Vec<anyhow::Error>,
) -> Vec<Source> {
    let mut out = Vec::new();
    for s in list {
        let known = before.eval_of(&s).map(|e| e.id);
        match refresh_source(app, http, base, &s, known).await {
            Ok(true) => out.push(s),
            Ok(false) => {}
            Err(e) => {
                let read = known.is_some() || app.store.zhf_has_any(&s.key()).unwrap_or(false);
                if read {
                    out.push(s);
                }
                errors.push(e);
            }
        }
    }
    out
}

async fn pass(app: &App, http: &reqwest::Client, base: &str) -> Result<()> {
    let campaign = campaign(&app.snap().issues);
    let before = app.hydra.zhf.snapshot();
    let mut errors = Vec::new();
    let mut srcs = Vec::new();
    let mut branched = false;
    if let Some(c) = &campaign {
        srcs = refresh_all(
            app,
            http,
            base,
            sources(Some(&c.release)),
            &before,
            &mut errors,
        )
        .await;
        branched = srcs.iter().any(|s| s.project == "nixos");
    }
    if !branched {
        srcs = refresh_all(app, http, base, sources(None), &before, &mut errors).await;
    }
    let snap = load(app, campaign, branched, srcs);
    *app.hydra.zhf.snapshot.write().unwrap() = Arc::new(snap);
    if let Err(e) = app.store.purge_zhf(Utc::now() - Duration::days(KEEP_DAYS)) {
        tracing::warn!("zhf purge: {e:#}");
    }
    match errors.pop() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

pub async fn run(app: Arc<App>) {
    run_with(app, crate::health::HYDRA.to_string()).await
}

async fn run_with(app: Arc<App>, base: String) {
    let http = reqwest::Client::builder()
        .user_agent(crate::health::HYDRA_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(PAGE_TIMEOUT)
        .build()
        .expect("http client");
    // What the store has, before Hydra answers.
    {
        let campaign = campaign(&app.snap().issues);
        let release = campaign
            .as_ref()
            .map(|c| sources(Some(&c.release)))
            .filter(|s| app.store.zhf_has_any(&s[0].key()).unwrap_or(false));
        let branched = release.is_some();
        let srcs = release.unwrap_or_else(|| sources(None));
        let mut snap = load(&app, campaign, branched, srcs);
        snap.checked_at = None;
        *app.hydra.zhf.snapshot.write().unwrap() = Arc::new(snap);
    }
    tokio::time::sleep(StdDuration::from_secs(30)).await;
    loop {
        app.hydra.zhf.stats.lock().unwrap().passes += 1;
        match pass(&app, &http, &base).await {
            Ok(()) => app.hydra.zhf.stats.lock().unwrap().last_error = None,
            Err(e) => {
                tracing::warn!("zhf: {e:#}");
                let mut s = app.hydra.zhf.stats.lock().unwrap();
                s.failed_passes += 1;
                s.last_error = Some(format!("{e:#}"));
            }
        }
        tokio::time::sleep(EVERY).await;
    }
}

/// What a failing job builds.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum Kind {
    Package(String),
    Test(String),
    Other(String),
}

impl Kind {
    pub fn name(&self) -> &str {
        match self {
            Kind::Package(s) | Kind::Test(s) | Kind::Other(s) => s,
        }
    }

    pub fn attr(&self) -> Option<&str> {
        match self {
            Kind::Package(a) => Some(a),
            _ => None,
        }
    }
}

/// One failing build of an item.
#[derive(Debug, Clone, Serialize)]
pub struct Build {
    pub system: String,
    pub why: Why,
    pub status: String,
    pub build: u64,
    pub url: String,
    pub log: String,
    pub at: Option<DateTime<Utc>>,
}

/// One failing job, over its systems.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    #[serde(flatten)]
    pub kind: Kind,
    /// Like `hello-2.12.1`, as Hydra names the build.
    pub nixname: Option<String>,
    pub builds: Vec<Build>,
    /// It fails itself on some system: the place to fix.
    pub failed: bool,
    pub maintainers: Vec<String>,
    pub teams: Vec<String>,
    /// In the index: the page `/p/<attr>` exists.
    pub indexed: bool,
}

impl Item {
    pub fn unowned(&self) -> bool {
        self.maintainers.is_empty() && self.teams.is_empty()
    }

    pub fn subject(&self) -> Option<Subject> {
        self.indexed
            .then(|| self.kind.attr().map(|a| Subject::Pkg(a.to_string())))
            .flatten()
    }
}

/// Every failing job of the snapshot, one item per job name, the direct
/// failures first, then the ones on most systems, then by name.
pub(crate) fn items(snap: &Snapshot, index: &Index) -> Vec<Item> {
    let mut by_kind: BTreeMap<Kind, Item> = BTreeMap::new();
    for src in &snap.sources {
        let Some(eval) = snap.eval_of(src) else {
            continue;
        };
        for r in &eval.rows {
            if r.why == Why::Aborted {
                continue;
            }
            let kind = src.kind(&r.job, &r.system);
            let item = by_kind.entry(kind.clone()).or_insert_with(|| {
                let p = kind.attr().and_then(|a| index.pkgs.get(a));
                Item {
                    kind: kind.clone(),
                    nixname: None,
                    builds: Vec::new(),
                    failed: false,
                    maintainers: p.map(|p| p.maintainers.clone()).unwrap_or_default(),
                    teams: p.map(|p| p.teams.clone()).unwrap_or_default(),
                    indexed: p.is_some(),
                }
            });
            if item.builds.iter().any(|b| b.system == r.system) {
                continue;
            }
            item.failed |= r.why == Why::Failed;
            if item.nixname.is_none() {
                item.nixname = r.name.clone();
            }
            item.builds.push(Build {
                system: r.system.clone(),
                why: r.why,
                status: r.status.clone(),
                build: r.build,
                url: format!("{}/build/{}", crate::health::HYDRA, r.build),
                log: format!("{}/build/{}/nixlog/1/tail", crate::health::HYDRA, r.build),
                at: r.at.and_then(|t| DateTime::from_timestamp(t, 0)),
            });
        }
    }
    let mut out: Vec<Item> = by_kind.into_values().collect();
    for i in &mut out {
        i.builds.sort_by(|a, b| a.system.cmp(&b.system));
    }
    out.sort_by(|a, b| {
        b.failed
            .cmp(&a.failed)
            .then_with(|| b.indexed.cmp(&a.indexed))
            .then_with(|| b.builds.len().cmp(&a.builds.len()))
            .then_with(|| a.kind.cmp(&b.kind))
    });
    out
}

/// The packages a login maintains, through a team, or watches.
fn own(app: &App, index: &Index, login: &str) -> HashSet<String> {
    let mut out: HashSet<String> = crate::catalog::maintained(index, login)
        .packages
        .into_iter()
        .filter_map(|(s, _)| match s {
            Subject::Pkg(a) => Some(a),
            _ => None,
        })
        .collect();
    let l = login.to_lowercase();
    for (attr, p) in &index.pkgs {
        if p.maintainers.iter().any(|m| m.to_lowercase() == l) {
            out.insert(attr.clone());
        }
    }
    for key in app.store.watched(login).unwrap_or_default() {
        if let Some(Subject::Pkg(a)) = Subject::parse(&key) {
            out.insert(a);
        }
    }
    out
}

fn matches(i: &Item, q: &str) -> bool {
    q.is_empty()
        || i.kind.name().contains(q)
        || i.nixname.as_deref().is_some_and(|n| n.contains(q))
        || i.maintainers.iter().any(|m| m.eq_ignore_ascii_case(q))
}

/// The items of a tab. `yours` needs a login.
pub(crate) fn list(app: &App, tab: &str, login: Option<&str>, q: &str) -> Vec<Item> {
    let snap = app.hydra.zhf.snapshot();
    let index = app.index.read().unwrap();
    let all = items(&snap, &index);
    let q = q.trim().to_lowercase();
    match tab {
        "yours" => {
            let Some(login) = login else { return vec![] };
            let mine = own(app, &index, login);
            all.into_iter()
                .filter(|i| i.kind.attr().is_some_and(|a| mine.contains(a)))
                .filter(|i| matches(i, &q))
                .collect()
        }
        "unowned" => all
            .into_iter()
            .filter(|i| i.failed && i.indexed && i.unowned())
            .filter(|i| matches(i, &q))
            .collect(),
        _ => all.into_iter().filter(|i| matches(i, &q)).collect(),
    }
}

/// One direct failure at random, a package with no maintainer when there
/// is one, so two people who push the button do not pick the same.
pub(crate) fn pick(app: &App, seed: u64) -> Option<Item> {
    let snap = app.hydra.zhf.snapshot();
    let index = app.index.read().unwrap();
    let all = items(&snap, &index);
    let direct: Vec<&Item> = all.iter().filter(|i| i.failed && i.indexed).collect();
    let unowned: Vec<&Item> = direct.iter().copied().filter(|i| i.unowned()).collect();
    let pool = if unowned.is_empty() {
        &direct
    } else {
        &unowned
    };
    if pool.is_empty() {
        return None;
    }
    // A small multiplicative hash: the seed is a time, so it only has to
    // spread.
    let n = (seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 20) as usize % pool.len();
    Some(pool[n].clone())
}

pub(crate) fn item_json(i: &Item) -> serde_json::Value {
    let mut v = serde_json::to_value(i).unwrap_or_default();
    if let Some(sub) = i.subject() {
        v["href"] = serde_json::Value::String(sub.href());
    }
    v
}

/// The JSON of `/api/v1/zhf`.
pub(crate) fn value(
    app: &App,
    tab: &str,
    login: Option<&str>,
    q: &str,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<serde_json::Value, String> {
    let snap = app.hydra.zhf.snapshot();
    let start = snap.start();
    let mut v = serde_json::json!({
        "campaign": snap.campaign,
        "branched": snap.branched,
        "jobsets": snap.sources.iter().map(|s| {
            let e = snap.eval_of(s);
            serde_json::json!({
                "jobset": s.key(), "url": s.url(),
                "eval": e.map(|e| e.id), "eval_url": e.map(Eval::url), "evaluated_at": e.map(|e| e.at),
                "counts": e.map(|e| &e.counts),
            })
        }).collect::<Vec<_>>(),
        "failing": snap.failing(),
        "start": start.map(|(at, n)| serde_json::json!({ "at": at, "failing": n })),
        "burndown": snap.burndown().iter().map(|(at, n)| serde_json::json!({ "at": at, "failing": n })).collect::<Vec<_>>(),
        "checked_at": snap.checked_at,
        "tab": tab,
    });
    if tab == "pick" {
        v["pick"] = pick(app, Utc::now().timestamp() as u64)
            .map(|i| item_json(&i))
            .unwrap_or_default();
    } else {
        let rows: Vec<serde_json::Value> = list(app, tab, login, q).iter().map(item_json).collect();
        let page = crate::api::page(&rows, limit, cursor)?;
        for (k, x) in page.as_object().into_iter().flatten() {
            v[k] = x.clone();
        }
    }
    Ok(v)
}

pub fn routes() -> Router<Shared> {
    Router::new().route("/zhf", get(page))
}

#[derive(Deserialize)]
struct ZhfQuery {
    tab: Option<String>,
    q: Option<String>,
    after: Option<String>,
    seed: Option<u64>,
}

fn tabs(current: &str, login: Option<&str>, counts: &BTreeMap<&str, usize>) -> Vec<Tab> {
    TABS.iter()
        .filter(|(id, _)| *id != "yours" || login.is_some())
        .map(|(id, label)| {
            let t = Tab::new(format!("{PATH}?tab={id}"), *label, *id == current);
            match counts.get(id) {
                Some(n) => t.count(*n),
                None => t,
            }
        })
        .collect()
}

fn system_badge(b: &Build) -> Html {
    let tone = match b.why {
        Why::Failed => Tone::Danger,
        _ => Tone::Neutral,
    };
    html!(
        "<a class=\"no-underline\" href=\"{}\" title=\"{}: build {}\">{}</a>",
        b.url,
        b.status,
        b.build,
        badge(tone, html!("{} {}", b.system, b.status.to_lowercase()))
    )
}

fn item_html(i: &Item) -> Html {
    let title = match (&i.kind, i.subject()) {
        (_, Some(sub)) => html!(
            "<a class=\"Link--primary text-bold\" href=\"{}\">{} {}</a>",
            sub.href(),
            sub.icon(),
            sub.label()
        ),
        (Kind::Test(t), _) => html!(
            "<span class=\"text-bold\">{} NixOS test {t}</span>",
            icon(octicon!("checklist"))
        ),
        (k, _) => html!("<span class=\"text-bold\">{}</span>", k.name()),
    };
    let who = if let Kind::Package(_) = i.kind {
        if !i.indexed {
            html!("not in the index")
        } else if i.unowned() {
            badge(Tone::Attention, "no maintainer")
        } else {
            let mut parts = Vec::new();
            if !i.maintainers.is_empty() {
                parts.push(crate::web::people(&i.maintainers));
            }
            if !i.teams.is_empty() {
                parts.push(html!("team {}", Html::text(&i.teams.join(", "))));
            }
            crate::html::join(parts, " · ")
        }
    } else {
        Html::new()
    };
    let first = i
        .builds
        .iter()
        .find(|b| b.why == Why::Failed)
        .or(i.builds.first());
    let links = first
        .map(|b| {
            html!(
                "<a href=\"{}\">log</a> · <a href=\"{}\">build {}</a>{}",
                b.log,
                b.url,
                b.build,
                b.at.map(|t| html!(", {} ago", ago(t))).unwrap_or_default()
            )
        })
        .unwrap_or_default();
    let name = i
        .nixname
        .as_deref()
        .map(|n| html!("<code>{n}</code>"))
        .unwrap_or_default();
    let meta = crate::html::join(
        [name, who, links].into_iter().filter(|h| !h.is_empty()),
        " · ",
    );
    let badges: Vec<Html> = i.builds.iter().map(system_badge).collect();
    item_row(None, title, "", meta, &badges, "")
}

fn burndown_svg(points: &[(DateTime<Utc>, u64)]) -> Html {
    if points.len() < 2 {
        return Html::new();
    }
    let (w, h, pad) = (400.0, 80.0, 4.0);
    let t0 = points[0].0.timestamp() as f64;
    let t1 = points[points.len() - 1].0.timestamp() as f64;
    let max = points.iter().map(|p| p.1).max().unwrap_or(1).max(1) as f64;
    let span = (t1 - t0).max(1.0);
    let path: Vec<String> = points
        .iter()
        .map(|(at, n)| {
            let x = pad + (at.timestamp() as f64 - t0) / span * (w - 2.0 * pad);
            let y = pad + (1.0 - *n as f64 / max) * (h - 2.0 * pad);
            format!("{x:.1},{y:.1}")
        })
        .collect();
    let line = path.join(" ");
    let wash = format!(
        "{} {:.1},{:.1} {:.1},{:.1}",
        line,
        w - pad,
        h - pad,
        pad,
        h - pad
    );
    html!(
        "<svg class=\"chart-svg c1\" viewBox=\"0 0 {w} {h}\" role=\"img\" aria-label=\"Failing builds over time\">\
         <polygon class=\"wash\" points=\"{wash}\"/><polyline class=\"line\" points=\"{line}\"/></svg>"
    )
}

/// The campaign, the count, the bar and the burndown.
fn progress(snap: &Snapshot) -> Html {
    if snap.evals.is_empty() {
        return Html::new();
    }
    let now = snap.failing();
    let start = snap.start();
    let direct: u64 = snap.evals.iter().map(|e| e.counts.failed).sum();
    let mut by_system: BTreeMap<&str, u64> = BTreeMap::new();
    for e in &snap.evals {
        for (s, (f, d)) in &e.counts.by_system {
            *by_system.entry(s).or_default() += f + d;
        }
    }
    let systems = crate::html::join(
        by_system
            .iter()
            .map(|(s, n)| html!("{s} {}", crate::chart::thousands(*n as i64))),
        " · ",
    );
    let bar = match start {
        Some((at, s)) if s > 0 => {
            let fixed = s.saturating_sub(now);
            let pct = (fixed as f64 / s as f64 * 100.0).round() as u64;
            html!(
                "<span class=\"zhf-bar my-2\" role=\"progressbar\" aria-valuenow=\"{pct}\" aria-valuemin=\"0\" aria-valuemax=\"100\" aria-label=\"{pct}% fixed\">\
                 <span class=\"zhf-bar-fill\" style=\"width: {pct}%\"></span></span>\
                 <div class=\"f6 color-fg-muted\">{} fixed since {}, when {} builds failed · {pct}%</div>",
                crate::chart::thousands(fixed as i64),
                at.format("%Y-%m-%d").to_string(),
                crate::chart::thousands(s as i64)
            )
        }
        _ => Html::new(),
    };
    let evals = crate::html::join(
        snap.sources.iter().map(|s| match snap.eval_of(s) {
            Some(e) => html!(
                "<a href=\"{}\">{}</a> <a href=\"{}\">eval {}</a>, {} ago",
                s.url(),
                s.label(),
                e.url(),
                e.id,
                ago(e.at)
            ),
            None => html!("<a href=\"{}\">{}</a>: not read yet", s.url(), s.label()),
        }),
        " · ",
    );
    html!(
        "<div class=\"Box mb-4\"><div class=\"Box-body\">\
         <div class=\"f2 text-bold\">{} failing builds</div>\
         <div class=\"f6 color-fg-muted\">{} fail themselves, the rest wait on a dependency · {systems}</div>\
         {bar}{chart}\
         <div class=\"f6 color-fg-muted mt-2\">{evals}</div>\
         </div></div>",
        crate::chart::thousands(now as i64),
        crate::chart::thousands(direct as i64),
        chart = match burndown_svg(&snap.burndown()) {
            svg if svg.is_empty() => svg,
            svg => html!(
                "<div class=\"charts mt-3\"><figure class=\"chart chart-wide\">{svg}</figure></div>"
            ),
        }
    )
}

fn sub(snap: &Snapshot) -> Html {
    if snap.sources.is_empty() {
        return "The failing jobs of the next NixOS release on Hydra, for the Zero Hydra Failures campaign.".into();
    }
    let hydra = crate::html::join(
        snap.sources
            .iter()
            .map(|s| html!("<a href=\"{}\">{}</a>", s.url(), s.label())),
        " and ",
    );
    match &snap.campaign {
        Some(c) if snap.branched => html!(
            "NixOS {} is branched off: the community fixes what fails on {hydra} before the release. \
             The tracking issue is <a href=\"{}\">#{}</a>.",
            c.release,
            c.url(),
            c.number
        ),
        Some(c) => html!(
            "The campaign for NixOS {} runs on master until the branch-off: what fails on {hydra} now fails in the release. \
             The tracking issue is <a href=\"{}\">#{}</a>.",
            c.release,
            c.url(),
            c.number
        ),
        None => html!(
            "No campaign is open. These are the failures on master, {hydra}; the next Zero Hydra Failures starts there, \
             at the branch-off of the next release."
        ),
    }
}

async fn page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<ZhfQuery>,
) -> Response {
    let viewer = app.viewer(&headers);
    let login = viewer.as_deref();
    let tab = match q.tab.as_deref() {
        Some(t) if TABS.iter().any(|(id, _)| *id == t) => t,
        Some(_) => return Redirect::to(PATH).into_response(),
        None if login.is_some() => "yours",
        None => "all",
    };
    if tab == "yours" && login.is_none() {
        return Redirect::to(PATH).into_response();
    }
    let snap = app.hydra.zhf.snapshot();
    let q_text = q.q.as_deref().unwrap_or("").trim();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for (id, _) in TABS.iter().filter(|(id, _)| *id != "pick") {
        if *id != "yours" || login.is_some() {
            counts.insert(id, list(&app, id, login, "").len());
        }
    }
    let body = if snap.evals.is_empty() {
        empty(if snap.sources.is_empty() {
            "The server has not asked Hydra yet. Come back in a few minutes."
        } else {
            "The server is reading the newest evaluation from Hydra. Come back in a few minutes."
        })
    } else if tab == "pick" {
        let seed = q.seed.unwrap_or_else(|| Utc::now().timestamp() as u64);
        match pick(&app, seed) {
            Some(i) => {
                let note = Html::from(if i.unowned() {
                    "A package with no maintainer that fails itself: nobody else is on it. \
                     Fix it, and add yourself to <code>meta.maintainers</code> if you like it."
                } else {
                    "Every failure with no maintainer is taken: here is one with maintainers. Ask them first in the pull request."
                });
                html!(
                    "<div class=\"Box mb-4\"><div class=\"Box-row f6 color-fg-muted\">{note}</div>{}\
                     <div class=\"Box-row\"><a class=\"btn btn-sm\" href=\"{PATH}?tab=pick&amp;seed={}\">Another one</a> \
                     <span class=\"f6 color-fg-muted ml-2\">Read the log, then <a href=\"https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md\">open a pull request</a> against {}.</span></div></div>",
                    item_html(&i),
                    seed.wrapping_add(1),
                    if snap.branched {
                        "master, with a backport, or the release branch"
                    } else {
                        "master"
                    }
                )
            }
            None => empty("No direct failure to pick right now."),
        }
    } else {
        let all = list(&app, tab, login, q_text);
        let start: usize = q
            .after
            .as_deref()
            .and_then(|a| a.parse().ok())
            .unwrap_or(0)
            .min(all.len());
        let end = (start + ROWS).min(all.len());
        let rows: Html = all[start..end].iter().map(item_html).collect();
        let lead = Html::from(match tab {
            "yours" => {
                "The packages you maintain, through a team, or watch, that fail. The ones that fail themselves come first; the others wait on a dependency."
            }
            "unowned" => {
                "Packages with no maintainer that fail themselves. Fix one, and add yourself to <code>meta.maintainers</code> if you like it."
            }
            _ => {
                "Every failing job: packages, NixOS tests and the rest. The ones that fail themselves come first; the greyed ones wait on a dependency."
            }
        });
        let search = html!(
            "<form class=\"Box-row d-flex flex-wrap gap-2\" method=\"get\" action=\"{PATH}\">\
             <input type=\"hidden\" name=\"tab\" value=\"{tab}\">\
             <input class=\"form-control input-sm flex-auto\" type=\"search\" name=\"q\" value=\"{q_text}\" placeholder=\"Filter by name or maintainer\" aria-label=\"Filter\">\
             <button class=\"btn btn-sm\" type=\"submit\">Filter</button></form>"
        );
        let body = if rows.is_empty() {
            empty(match tab {
                "yours" if q_text.is_empty() => {
                    "Nothing of yours fails. Pick one with no maintainer instead."
                }
                _ => "Nothing matches.",
            })
        } else {
            html!("{search}{rows}")
        };
        let href = if q_text.is_empty() {
            format!("{PATH}?tab={tab}")
        } else {
            format!("{PATH}?tab={tab}&q={}", crate::github::urlencode(q_text))
        };
        html!(
            "<div class=\"Box mb-4\"><div class=\"Box-row f6 color-fg-muted\">{lead}</div>{body}</div>{}",
            crate::ui::pager(&href, (end < all.len()).then(|| end.to_string()).as_deref())
        )
    };
    Page::new(Section::Nixpkgs, "Zero Hydra Failures")
        .icon(octicon!("flame"))
        .sub(sub(&snap))
        .subnav(tabs(tab, login, &counts))
        .body(html!("{}{body}", progress(&snap)))
        .render(&app, login)
}

/// The home page's button: during a campaign, with its release and the
/// count of the person's failures, or of the ones with no maintainer.
pub(crate) fn home_link(app: &App, login: Option<&str>) -> Html {
    let snap = app.hydra.zhf.snapshot();
    let Some(c) = &snap.campaign else {
        return html!(
            "<a class=\"btn btn-sm\" href=\"{PATH}\">{} Hydra failures</a>",
            icon(octicon!("flame"))
        );
    };
    let (href, n) = match login {
        Some(l) => (
            format!("{PATH}?tab=yours"),
            list(app, "yours", Some(l), "").len(),
        ),
        None => (
            format!("{PATH}?tab=unowned"),
            list(app, "unowned", None, "").len(),
        ),
    };
    let count = if n > 0 {
        html!(" <span class=\"Counter\">{n}</span>")
    } else {
        Html::new()
    };
    html!(
        "<a class=\"btn btn-sm\" href=\"{href}\">{} Zero Hydra Failures {}{count}</a>",
        icon(octicon!("flame")),
        c.release
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("testdata/hydra-eval.html");

    #[test]
    fn the_evaluation_page_gives_the_failing_builds_and_the_tabs() {
        let p = parse(FIXTURE);
        assert_eq!(p.at, Some(1793299459));
        assert_eq!(p.tabs["still-fail"], 2695);
        assert_eq!(p.tabs["now-fail"], 6);
        assert_eq!(p.tabs["aborted"], 27);
        assert_eq!(p.tabs["still-succeed"], 147234);
        assert_eq!(p.tabs["inputs"], 0);
        // 27 aborted + 6 now failing + 13 still failing; the 6 that now
        // succeed and the 4 new ones passed, and the rows after the end
        // mark are not read.
        assert_eq!(p.rows.len(), 46);
        assert!(p.rows.iter().all(|r| r.status != "Succeeded"));
        let acl = p.rows.iter().find(|r| r.job == "nixos.tests.acl").unwrap();
        assert_eq!(acl.build, 351093960);
        assert_eq!(acl.system, "aarch64-linux");
        assert_eq!(acl.why, Why::Failed);
        assert_eq!(acl.at, Some(1793187360));
        assert_eq!(acl.name.as_deref(), Some("acl-2.4.0"));
        let dep = p
            .rows
            .iter()
            .find(|r| r.job == "nixos.tests.activation-bashless-closure.initrd")
            .unwrap();
        assert_eq!(dep.why, Why::DependencyFailed);
        assert_eq!(dep.status, "Dependency failed");
        let timed = p.rows.iter().filter(|r| r.status == "Timed out").count();
        assert_eq!(timed, 25);
        assert!(
            p.rows
                .iter()
                .any(|r| r.job == "nixpkgs.inventree.x86_64-linux")
        );
    }

    #[test]
    fn a_job_is_a_package_a_test_or_something_else() {
        let nixos = &sources(Some("26.11"))[0];
        assert_eq!(nixos.key(), "nixos/release-26.11");
        assert_eq!(
            nixos.kind("nixpkgs.hello.x86_64-linux", "x86_64-linux"),
            Kind::Package("hello".into())
        );
        assert_eq!(
            nixos.kind(
                "nixpkgs.python3Packages.tornado.aarch64-linux",
                "aarch64-linux"
            ),
            Kind::Package("python3Packages.tornado".into())
        );
        assert_eq!(
            nixos.kind("nixos.tests.acl", "aarch64-linux"),
            Kind::Test("acl".into())
        );
        assert_eq!(
            nixos.kind("nixos.tests.ersatztv.x86_64-linux", "x86_64-linux"),
            Kind::Test("ersatztv".into())
        );
        assert_eq!(
            nixos.kind("nixos.iso_minimal.x86_64-linux", "x86_64-linux"),
            Kind::Other("nixos.iso_minimal".into())
        );
        let darwin = &sources(Some("26.11"))[1];
        assert_eq!(darwin.key(), "nixpkgs/nixpkgs-26.11-darwin");
        assert_eq!(
            darwin.kind("hello.aarch64-darwin", "aarch64-darwin"),
            Kind::Package("hello".into())
        );
        let row = |system: &str| Row {
            build: 1,
            job: "hello".into(),
            status: "Failed".into(),
            why: Why::Failed,
            system: system.into(),
            at: None,
            name: None,
        };
        assert!(darwin.keep(&row("aarch64-darwin")));
        assert!(!darwin.keep(&row("x86_64-linux")));
        assert!(nixos.keep(&row("x86_64-linux")));
        assert_eq!(sources(None)[0].key(), "nixos/unstable");
        assert_eq!(sources(None)[1].key(), "nixpkgs/unstable");
    }

    fn issue(number: u64, title: &str, open: bool, days_ago: i64) -> crate::model::Issue {
        let at = Utc::now() - Duration::days(days_ago);
        crate::model::Issue {
            number,
            title: title.into(),
            url: String::new(),
            author: "rm".into(),
            state: if open { "OPEN" } else { "CLOSED" }.into(),
            created_at: at,
            updated_at: at,
            closed_at: None,
            labels: vec![],
            comments: 0,
            mentions: vec![],
        }
    }

    #[test]
    fn the_campaign_is_the_newest_open_zhf_issue() {
        let issues = vec![
            issue(1, "NixOS 26.05 – Zero Hydra Failures", false, 200),
            issue(2, "NixOS 26.11 – Zero Hydra Failures", true, 3),
            issue(3, "zero hydra failures: 26.11 edition, darwin", true, 2),
            issue(4, "Zero Hydra Failures with no release", true, 1),
            issue(5, "Something about 26.11", true, 0),
        ];
        let c = campaign(&issues).unwrap();
        assert_eq!(c.number, 3);
        assert_eq!(c.release, "26.11");
        assert_eq!(campaign(&issues[..1]), None);
        assert_eq!(release_in("NixOS 24.05 — ZHF"), Some("24.05".into()));
        assert_eq!(release_in("build 126.050"), None);
    }

    fn eval(jobset: &str, id: u64, days_ago: i64, rows: Vec<Row>) -> Eval {
        let mut counts = Counts::default();
        for r in &rows {
            counts.add(r);
        }
        Eval {
            jobset: jobset.into(),
            id,
            at: Utc::now() - Duration::days(days_ago),
            fetched_at: Utc::now(),
            counts,
            rows,
        }
    }

    fn row(build: u64, job: &str, status: &str, system: &str) -> Row {
        Row {
            build,
            job: job.into(),
            status: status.into(),
            why: Why::of(status).unwrap(),
            system: system.into(),
            at: None,
            name: Some(format!("{}-1.0", job.split('.').nth(1).unwrap_or(job))),
        }
    }

    #[test]
    fn items_merge_the_systems_and_put_direct_failures_first() {
        let srcs = sources(Some("26.11"));
        let linux = eval(
            "nixos/release-26.11",
            10,
            1,
            vec![
                row(
                    1,
                    "nixpkgs.dep.x86_64-linux",
                    "Dependency failed",
                    "x86_64-linux",
                ),
                row(2, "nixpkgs.hello.x86_64-linux", "Failed", "x86_64-linux"),
                row(
                    3,
                    "nixpkgs.hello.aarch64-linux",
                    "Timed out",
                    "aarch64-linux",
                ),
                row(4, "nixos.tests.acl", "Failed", "aarch64-linux"),
                row(5, "nixpkgs.gone.x86_64-linux", "Aborted", "x86_64-linux"),
            ],
        );
        let darwin = eval(
            "nixpkgs/nixpkgs-26.11-darwin",
            11,
            1,
            vec![row(6, "hello.aarch64-darwin", "Failed", "aarch64-darwin")],
        );
        let snap = Snapshot {
            sources: srcs,
            evals: vec![linux, darwin],
            ..Default::default()
        };
        let mut index = Index::default();
        index.pkgs.insert("hello".into(), Default::default());
        index.pkgs.insert(
            "dep".into(),
            crate::area::Package {
                maintainers: vec!["alice".into()],
                ..Default::default()
            },
        );
        let items = items(&snap, &index);
        let names: Vec<&str> = items.iter().map(|i| i.kind.name()).collect();
        // Direct failures first, then what the index knows, then by name.
        assert_eq!(names, ["hello", "acl", "dep"]);
        let hello = &items[0];
        assert!(hello.failed && hello.unowned() && hello.indexed);
        let systems: Vec<&str> = hello.builds.iter().map(|b| b.system.as_str()).collect();
        assert_eq!(systems, ["aarch64-darwin", "aarch64-linux", "x86_64-linux"]);
        assert_eq!(items[1].kind, Kind::Test("acl".into()));
        assert!(!items[2].failed);
        assert_eq!(items[2].maintainers, ["alice"]);
        assert_eq!(snap.failing(), 5);
    }

    #[test]
    fn the_burndown_sums_the_newest_count_of_each_jobset() {
        let at = |d: i64| Utc::now() - Duration::days(d);
        let point = |jobset: &str, eval: u64, d: i64, failing: u64| Point {
            jobset: jobset.into(),
            eval,
            at: at(d),
            failing,
            failed: failing,
        };
        let snap = Snapshot {
            sources: sources(None),
            history: vec![
                point("a", 1, 10, 100),
                point("b", 2, 9, 50),
                point("a", 3, 5, 80),
                point("b", 4, 1, 40),
            ],
            ..Default::default()
        };
        // The first point has one jobset of two: not a rise.
        let counts: Vec<u64> = snap.burndown().iter().map(|(_, n)| *n).collect();
        assert_eq!(counts, [150, 130, 120]);
        assert_eq!(snap.start().map(|(_, n)| n), Some(150));
        let with = Snapshot {
            campaign: Some(Campaign {
                number: 1,
                title: String::new(),
                release: "26.11".into(),
                opened_at: at(7),
            }),
            ..snap
        };
        assert_eq!(with.start().map(|(_, n)| n), Some(130));
    }

    #[tokio::test]
    async fn latest_eval_reads_the_redirect() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route(
            "/jobset/nixos/release-26.11/latest-eval",
            get(|| async {
                Redirect::to("https://hydra.nixos.org/eval/1830830?name=release-26.11")
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        assert_eq!(
            latest_eval(&http, &base, "nixos/release-26.11")
                .await
                .unwrap(),
            Some(1830830)
        );
        assert_eq!(
            latest_eval(&http, &base, "nixos/release-99.99")
                .await
                .unwrap(),
            None
        );
    }
}
