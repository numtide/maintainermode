//! Package health from public sources: the newest upstream version from
//! repology, advisories from the nixpkgs security tracker, and the last build
//! on Hydra. The first two are bulk sweeps, kept a day; Hydra is asked per
//! page view, within a limit, and cached. Adapted from nixfresh's collectors
//! (MIT).

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::web::App;

const REPOLOGY: &str = "https://repology.org/api/v1/projects";
const REPOLOGY_REPO: &str = "nix_unstable";
/// repology asks for no more than one request a second.
const REPOLOGY_PAUSE: Duration = Duration::from_millis(1100);
const REPOLOGY_PAGES: usize = 100;
const TRACKER: &str = "https://tracker.security.nixos.org/api/v1/issues";
/// The tracker ignores `limit` and sends 10 issues a page: 3094 issues on
/// 2026-10-17 are 310 pages, about 13 minutes a day with the pause.
const TRACKER_PAGES: usize = 400;
/// The tracker answers 429 after about 30 quick pages.
const TRACKER_PAUSE: Duration = Duration::from_millis(2500);
const HYDRA: &str = "https://hydra.nixos.org";
const HYDRA_TTL: Duration = Duration::from_secs(3600);
/// A failed request is asked again sooner than an answer.
const HYDRA_RETRY: Duration = Duration::from_secs(300);
const HYDRA_CONCURRENT: usize = 4;
const HYDRA_PER_MINUTE: u32 = 30;
const HYDRA_CACHE_MAX: usize = 20_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Advisory {
    /// `NIXPKGS-2026-3072`.
    pub code: String,
    pub cves: Vec<String>,
    pub title: String,
    /// The nixpkgs issue the tracker opened for it.
    pub issue: Option<u64>,
    /// The affected ranges of each CVE for this attribute.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranges: Vec<crate::affected::Ranges>,
}

impl Advisory {
    pub fn url(&self) -> String {
        format!("https://tracker.security.nixos.org/issue/{}", self.code)
    }

    pub fn issue_url(&self) -> Option<String> {
        self.issue
            .map(|n| format!("https://github.com/NixOS/nixpkgs/issues/{n}"))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Health {
    /// attribute → newest upstream version, only where nixpkgs is behind.
    pub upstream: BTreeMap<String, String>,
    /// attribute → advisories the tracker attaches to it.
    pub advisories: BTreeMap<String, Vec<Advisory>>,
    pub repology_at: Option<DateTime<Utc>>,
    pub tracker_at: Option<DateTime<Utc>>,
    /// attribute → the upstream version and when a refresh first saw it, so
    /// a daily refresh does not make an old release new again.
    #[serde(default)]
    pub upstream_seen: BTreeMap<String, (String, DateTime<Utc>)>,
    /// CVE id → severity and EPSS, for the CVEs the tracker names.
    #[serde(default)]
    pub cves: BTreeMap<String, crate::severity::Cve>,
    #[serde(default)]
    pub epss_at: Option<DateTime<Utc>>,
    /// attribute → the newest release from its git tags or its registry.
    #[serde(default)]
    pub direct: BTreeMap<String, crate::upstream::Direct>,
    /// attribute → the OSV ids that match the version nixpkgs ships.
    #[serde(default)]
    pub osv: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub osv_vulns: BTreeMap<String, crate::osv::Vuln>,
    /// OSV id → failures in a row and the last one, for the backoff.
    #[serde(default)]
    pub osv_failed: BTreeMap<String, (u32, DateTime<Utc>)>,
    #[serde(default)]
    pub osv_at: Option<DateTime<Utc>>,
    /// source → what its last pass did.
    #[serde(default)]
    pub runs: BTreeMap<String, Run>,
    /// attribute → advisory code → its verdict against master and the
    /// channels. Derived, so never stored; see [`Health::assess`].
    #[serde(skip)]
    pub verdicts: HashMap<String, HashMap<String, crate::affected::Verdict>>,
    /// The index and channel revisions the verdicts were made from.
    #[serde(skip)]
    pub assessed: String,
}

/// What one source did on its last pass, for `/sync`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub at: DateTime<Utc>,
    pub ok: bool,
    pub line: String,
}

impl Run {
    pub fn ok(line: String) -> Run {
        Run {
            at: Utc::now(),
            ok: true,
            line,
        }
    }

    pub fn failed(line: String) -> Run {
        Run {
            at: Utc::now(),
            ok: false,
            line,
        }
    }
}

impl Health {
    /// The first fill dates every release to the epoch: a backlog is not news.
    pub fn set_upstream(&mut self, upstream: BTreeMap<String, String>, now: DateTime<Utc>) {
        let first = self.upstream_seen.is_empty();
        let mut seen = std::mem::take(&mut self.upstream_seen);
        self.upstream_seen = upstream
            .iter()
            .map(|(attr, v)| {
                let at = match seen.remove(attr) {
                    Some((old, at)) if old == *v => at,
                    _ if first => DateTime::UNIX_EPOCH,
                    _ => now,
                };
                (attr.clone(), (v.clone(), at))
            })
            .collect();
        self.upstream = upstream;
        self.repology_at = Some(now);
    }

    /// When `version` of `attr` was first seen upstream.
    pub fn upstream_since(&self, attr: &str, version: &str) -> Option<DateTime<Utc>> {
        let direct = self
            .direct
            .get(attr)
            .filter(|d| d.latest.as_deref() == Some(version))
            .map(|d| d.seen_at);
        let repology = self
            .upstream_seen
            .get(attr)
            .filter(|(v, _)| v == version)
            .map(|(_, at)| *at);
        direct.into_iter().chain(repology).min()
    }

    /// A verdict not made yet is unknown, which keeps the advisory open.
    pub fn verdict(&self, attr: &str, code: &str) -> &crate::affected::Verdict {
        self.verdicts
            .get(attr)
            .and_then(|m| m.get(code))
            .unwrap_or(&crate::affected::UNKNOWN)
    }

    fn assess_key(&self, index: &crate::area::Index, ch: &crate::channels::Channels) -> String {
        let tracker = self.tracker_at.map(|t| t.to_rfc3339()).unwrap_or_default();
        [tracker.as_str(), index.rev.as_str()]
            .into_iter()
            .chain(ch.list.iter().map(|c| c.rev.as_str()))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The verdicts were made from this index, these channels and this sweep.
    pub fn assessed_for(&self, index: &crate::area::Index, ch: &crate::channels::Channels) -> bool {
        self.assessed == self.assess_key(index, ch)
    }

    pub fn assess(&mut self, index: &crate::area::Index, ch: &crate::channels::Channels) {
        let key = self.assess_key(index, ch);
        let mut out: HashMap<String, HashMap<String, crate::affected::Verdict>> = HashMap::new();
        for (attr, list) in &self.advisories {
            let Some(p) = index.pkgs.get(attr) else {
                continue;
            };
            let seen = ch.of(attr, &p.version);
            let channels: Vec<(&str, &str, bool)> = seen
                .iter()
                .filter_map(|s| Some((s.channel.label(), s.version?, s.channel.stable())))
                .collect();
            let m = out.entry(attr.clone()).or_default();
            for a in list {
                m.insert(
                    a.code.clone(),
                    crate::affected::judge(&a.ranges, &a.title, attr, &p.version, &channels),
                );
            }
        }
        self.verdicts = out;
        self.assessed = key;
    }

    /// OSV matches the tracker does not already name by a CVE.
    pub fn osv_extra(&self, attr: &str) -> Vec<(&str, Option<&crate::osv::Vuln>)> {
        let tracked: std::collections::HashSet<&String> = self
            .advisories
            .get(attr)
            .into_iter()
            .flatten()
            .flat_map(|a| a.cves.iter())
            .collect();
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut out = Vec::new();
        for id in self.osv.get(attr).into_iter().flatten() {
            let v = self.osv_vulns.get(id);
            if seen.contains(id.as_str())
                || v.is_some_and(|v| {
                    v.cves.iter().any(|c| tracked.contains(c))
                        || v.aliases.iter().any(|a| seen.contains(a.as_str()))
                })
            {
                continue;
            }
            seen.insert(id);
            seen.extend(
                v.into_iter()
                    .flat_map(|v| v.aliases.iter().map(String::as_str)),
            );
            out.push((id.as_str(), v));
        }
        out
    }

    /// The newest upstream release when it is newer than `current`, and
    /// where it comes from. The package's own tags or registry overrule
    /// repology; repology counts only for a release on the same numbering
    /// scheme.
    pub fn newest(&self, attr: &str, current: &str) -> Option<(&str, &str)> {
        if let Some(d) = self.direct.get(attr) {
            if let Some(v) = &d.latest {
                return (crate::version::compare(v, current) == std::cmp::Ordering::Greater)
                    .then_some((v.as_str(), d.from.as_str()));
            }
        }
        let r = self.upstream.get(attr)?;
        crate::upstream::newer(r, current).then_some((r.as_str(), "repology"))
    }
}

/// The health sources run in their own task, so a slow source never holds
/// back the pull request and issue sync.
pub const PASS: Duration = Duration::from_secs(600);

fn stale(t: Option<DateTime<Utc>>) -> bool {
    t.is_none_or(|t| Utc::now() - t > chrono::Duration::hours(24))
}

fn publish(app: &App, h: &Health) {
    match serde_json::to_string(h) {
        Ok(s) => {
            if let Err(e) = app.store.set("health", &s) {
                tracing::warn!("health: {e:#}");
            }
        }
        Err(e) => tracing::warn!("health: {e:#}"),
    }
    let mut h = h.clone();
    {
        let index = app.index.read().unwrap();
        let ch = app.channels.read().unwrap().clone();
        h.assess(&index, &ch);
    }
    *app.health.write().unwrap() = Arc::new(h);
    app.recompute_packages();
}

/// One attribute per package name, plus every attribute someone maintains
/// or watches, with a source that can be asked directly.
fn candidates(app: &App) -> Vec<crate::upstream::Candidate> {
    use crate::catalog::Subject;
    let index = app.index.read().unwrap();
    let mut first: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in app.store.members().unwrap_or_default() {
        for (sub, _) in crate::catalog::maintained(&index, &m.login).packages {
            if let Subject::Pkg(a) = sub {
                first.insert(a);
            }
        }
    }
    for key in app.store.watched_by_anyone().unwrap_or_default() {
        if let Some(Subject::Pkg(a)) = Subject::parse(&key) {
            first.insert(a);
        }
    }
    let attrs: std::collections::BTreeSet<&String> =
        index.by_pname.values().chain(first.iter()).collect();
    attrs
        .into_iter()
        .filter_map(|attr| {
            let p = index.pkgs.get(attr)?;
            let url = p.facts.as_ref()?.src_url.as_deref()?;
            Some(crate::upstream::Candidate {
                attr: attr.clone(),
                pname: p.pname.clone(),
                version: p.version.clone(),
                origin: crate::upstream::origin(url)?,
                first: first.contains(attr),
            })
        })
        .filter(|c| !c.version.is_empty())
        .collect()
}

fn osv_queries(app: &App) -> Vec<crate::osv::Query> {
    let index = app.index.read().unwrap();
    index
        .pkgs
        .iter()
        .filter_map(|(attr, p)| {
            let url = p.facts.as_ref().and_then(|f| f.src_url.as_deref());
            crate::osv::query(attr, &p.pname, &p.version, url)
        })
        .collect()
}

fn note(h: &mut Health, source: &str, r: Run) {
    if r.ok {
        tracing::info!("{source}: {}", r.line);
    } else {
        tracing::warn!("{source}: {}", r.line);
    }
    h.runs.insert(source.to_string(), r);
}

/// Reads only the sources that are due, so a source that is down is asked
/// again on the next pass while the others keep their day.
pub async fn run(app: Arc<App>, nvd_key: Option<crate::secret::Secret>) {
    let mut h: Health = (**app.health.read().unwrap()).clone();
    loop {
        if stale(h.repology_at) {
            match repology().await {
                Ok(u) => {
                    h.set_upstream(u, Utc::now());
                    publish(&app, &h);
                }
                Err(e) => tracing::warn!("repology: {e:#}"),
            }
        }
        let no_ranges = !h.advisories.is_empty()
            && h.advisories.values().flatten().all(|a| a.ranges.is_empty());
        if stale(h.tracker_at) || no_ranges {
            match tracker().await {
                Ok(a) => {
                    h.advisories = a;
                    h.tracker_at = Some(Utc::now());
                    publish(&app, &h);
                }
                Err(e) => tracing::warn!("security tracker: {e:#}"),
            }
        }
        let r = crate::severity::records(&mut h).await;
        note(&mut h, "cve", r);
        publish(&app, &h);
        let key = nvd_key.as_ref().map(|k| k.get());
        let r = crate::severity::nvd(&mut h, key.as_deref()).await;
        note(&mut h, "nvd", r);
        if h.tracker_at.is_some() && stale(h.epss_at) {
            let r = crate::severity::epss(&mut h).await;
            if r.ok {
                h.epss_at = Some(r.at);
            }
            note(&mut h, "epss", r);
        }
        publish(&app, &h);
        if crate::osv::due(h.osv_at) {
            let q = osv_queries(&app);
            if !q.is_empty() {
                let r = crate::osv::matches(&mut h, q).await;
                note(&mut h, "osv", r);
            }
        }
        if !h.osv.is_empty() {
            let r = crate::osv::details(&mut h).await;
            note(&mut h, "osv-records", r);
        }
        publish(&app, &h);
        let c = candidates(&app);
        let r = if c.is_empty() {
            Run::ok("waiting for an index with source URLs".into())
        } else {
            crate::upstream::pass(&mut h, c).await
        };
        note(&mut h, "upstream", r);
        publish(&app, &h);
        tokio::time::sleep(PASS).await;
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("maintainer-mode (+https://github.com/numtide/maintainermode)")
        .timeout(Duration::from_secs(60))
        .build()
        .expect("http client")
}

/// Every project repology calls outdated in nixpkgs unstable, page by page.
/// The cursor is the last project name of a page.
pub async fn repology() -> Result<BTreeMap<String, String>> {
    let http = http();
    let mut out = BTreeMap::new();
    let mut cursor = String::new();
    for _ in 0..REPOLOGY_PAGES {
        let url = if cursor.is_empty() {
            format!("{REPOLOGY}/?inrepo={REPOLOGY_REPO}&outdated=1")
        } else {
            format!(
                "{REPOLOGY}/{}/?inrepo={REPOLOGY_REPO}&outdated=1",
                crate::github::urlencode(&cursor)
            )
        };
        let page: BTreeMap<String, Vec<Value>> = http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let count = page.len();
        for (project, entries) in &page {
            let Some(newest) = entries
                .iter()
                .find(|e| e["status"] == "newest")
                .and_then(|e| e["version"].as_str())
            else {
                continue;
            };
            for e in entries
                .iter()
                .filter(|e| e["repo"] == REPOLOGY_REPO && e["status"] == "outdated")
            {
                if let Some(attr) = e["srcname"].as_str() {
                    out.insert(attr.to_string(), newest.to_string());
                }
            }
            if *project > cursor {
                cursor = project.clone();
            }
        }
        if count < 200 {
            break;
        }
        tokio::time::sleep(REPOLOGY_PAUSE).await;
    }
    Ok(out)
}

fn issue_number(url: &str) -> Option<u64> {
    url.strip_prefix("https://github.com/NixOS/nixpkgs/issues/")?
        .parse()
        .ok()
}

/// Some tracker fields can come as JSON inside a string.
fn field(v: &Value, key: &str) -> Value {
    match &v[key] {
        Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
        other => other.clone(),
    }
}

/// The ranges one suggestion gives each of its attributes.
fn suggestion_ranges(s: &Value) -> Vec<(String, crate::affected::Ranges)> {
    let products = field(s, "affected_products");
    let products: Vec<&Value> = products.as_array().into_iter().flatten().collect();
    let names: Vec<&str> = products
        .iter()
        .map(|p| p["name"].as_str().unwrap_or(""))
        .collect();
    let packages = field(s, "packages");
    let cve = s["cve_id"].as_str().map(String::from);
    packages
        .as_object()
        .into_iter()
        .flat_map(|m| m.keys())
        .map(|attr| {
            let constraints = crate::affected::products_for(attr, &names)
                .into_iter()
                .flat_map(|i| products[i]["version_constraints"].as_array())
                .flatten()
                .filter_map(|c| Some((c[0].as_str()?.to_string(), c[1].as_str()?.to_string())))
                .collect();
            (
                attr.clone(),
                crate::affected::Ranges {
                    cve: cve.clone(),
                    constraints,
                },
            )
        })
        .collect()
}

/// One page of tracker issues into advisories by attribute.
pub fn read_page(page: &Value, out: &mut BTreeMap<String, Vec<Advisory>>) {
    for issue in page["results"].as_array().into_iter().flatten() {
        let Some(code) = issue["code"].as_str() else {
            continue;
        };
        let suggestions = issue["suggestions"].as_array().cloned().unwrap_or_default();
        let mut cves: Vec<String> = suggestions
            .iter()
            .filter_map(|s| s["cve_id"].as_str().map(String::from))
            .collect();
        cves.sort();
        cves.dedup();
        let mut by_attr: BTreeMap<String, Vec<crate::affected::Ranges>> = BTreeMap::new();
        for s in &suggestions {
            for (attr, r) in suggestion_ranges(s) {
                by_attr.entry(attr).or_default().push(r);
            }
        }
        for (attr, ranges) in by_attr {
            out.entry(attr).or_default().push(Advisory {
                code: code.to_string(),
                cves: cves.clone(),
                title: issue["title"].as_str().unwrap_or(code).to_string(),
                issue: issue["github_issue_url"].as_str().and_then(issue_number),
                ranges,
            });
        }
    }
}

/// Every tracker issue with its suggestions; each names the attributes it
/// affects.
pub async fn tracker() -> Result<BTreeMap<String, Vec<Advisory>>> {
    let http = http();
    let mut out: BTreeMap<String, Vec<Advisory>> = BTreeMap::new();
    let mut url = format!("{TRACKER}?expand=suggestions&limit=100");
    for n in 0..TRACKER_PAGES {
        let fetched = async {
            anyhow::Ok(
                http.get(&url)
                    .send()
                    .await?
                    .error_for_status()?
                    .json::<Value>()
                    .await?,
            )
        };
        // the newest issues come first, so a throttled sweep keeps what it read
        let page = match fetched.await {
            Ok(p) => p,
            Err(e) if n > 0 => {
                tracing::warn!("security tracker stopped at page {n}: {e:#}");
                break;
            }
            Err(e) => return Err(e),
        };
        read_page(&page, &mut out);
        tokio::time::sleep(TRACKER_PAUSE).await;
        match page["next"].as_str() {
            Some(next) => url = next.replacen("http://", "https://", 1),
            None => break,
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct Build {
    pub id: u64,
    pub ok: bool,
    /// Hydra's words for `buildstatus`.
    pub status: &'static str,
    pub at: Option<DateTime<Utc>>,
}

impl Build {
    pub fn url(&self) -> String {
        format!("{HYDRA}/build/{}", self.id)
    }
}

fn status_word(code: i64) -> &'static str {
    match code {
        0 => "succeeded",
        1 => "failed",
        2 => "dependency failed",
        3 | 4 => "aborted",
        6 => "failed with output",
        7 => "timed out",
        10 => "log limit exceeded",
        11 => "output limit exceeded",
        _ => "failed",
    }
}

/// What a page learns about a Hydra job.
#[derive(Debug, Clone)]
pub enum Last {
    Build(Build),
    /// Hydra knows no finished build of the job.
    Missing,
    /// Hydra did not answer in time, or answered with an error.
    NoAnswer,
    /// The limit was reached, so no request was made.
    NotAsked,
}

impl Last {
    pub fn build(&self) -> Option<&Build> {
        match self {
            Last::Build(b) => Some(b),
            _ => None,
        }
    }

    pub fn state(&self) -> &'static str {
        match self {
            Last::Build(_) => "build",
            Last::Missing => "no finished build",
            Last::NoAnswer => "no answer",
            Last::NotAsked => "not asked yet",
        }
    }

    fn ttl(&self) -> Duration {
        match self {
            Last::NoAnswer => HYDRA_RETRY,
            _ => HYDRA_TTL,
        }
    }
}

/// A fixed one-minute window.
struct Window {
    start: Instant,
    used: u32,
}

impl Window {
    fn take(&mut self, now: Instant, cap: u32) -> bool {
        if now.duration_since(self.start) >= Duration::from_secs(60) {
            self.start = now;
            self.used = 0;
        }
        if self.used >= cap {
            return false;
        }
        self.used += 1;
        true
    }
}

/// The last finished Hydra build of a job, cached an hour. Page views ask
/// for it, so a crawler would reach Hydra through us: a few requests at a
/// time and a cap a minute keep that small.
pub struct Hydra {
    http: reqwest::Client,
    base: String,
    cache: Mutex<HashMap<String, (Instant, Last)>>,
    running: tokio::sync::Semaphore,
    window: Mutex<Window>,
    per_minute: u32,
    not_asked: AtomicU64,
}

impl Default for Hydra {
    fn default() -> Self {
        Self::new(HYDRA, HYDRA_CONCURRENT, HYDRA_PER_MINUTE)
    }
}

impl Hydra {
    fn new(base: &str, concurrent: usize, per_minute: u32) -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent("maintainer-mode")
                .timeout(Duration::from_secs(5))
                .build()
                .expect("http client"),
            base: base.to_string(),
            cache: Mutex::new(HashMap::new()),
            running: tokio::sync::Semaphore::new(concurrent),
            window: Mutex::new(Window {
                start: Instant::now(),
                used: 0,
            }),
            per_minute,
            not_asked: AtomicU64::new(0),
        }
    }

    pub fn package_job(attr: &str) -> String {
        format!("nixpkgs/unstable/{attr}.x86_64-linux")
    }

    pub fn test_job(name: &str) -> String {
        format!("nixos/trunk-combined/nixos.tests.{name}.x86_64-linux")
    }

    pub fn job_url(job: &str) -> String {
        format!("{HYDRA}/job/{job}")
    }

    /// The limits and the counts, for `/sync`.
    pub fn stats(&self) -> Value {
        serde_json::json!({
            "concurrent": HYDRA_CONCURRENT,
            "per_minute": self.per_minute,
            "kept_seconds": HYDRA_TTL.as_secs(),
            "cached": self.cache.lock().unwrap().len(),
            "not_asked": self.not_asked.load(Ordering::Relaxed),
        })
    }

    fn cached(&self, job: &str) -> Option<Last> {
        let cache = self.cache.lock().unwrap();
        let (at, last) = cache.get(job)?;
        (at.elapsed() < last.ttl()).then(|| last.clone())
    }

    /// What a page view already learned about a job; it never asks Hydra.
    pub fn known(&self, job: &str) -> Option<Last> {
        self.cached(job)
    }

    fn keep(&self, job: &str, last: &Last) {
        let mut cache = self.cache.lock().unwrap();
        if cache.len() >= HYDRA_CACHE_MAX {
            cache.retain(|_, (at, l)| at.elapsed() < l.ttl());
        }
        cache.insert(job.to_string(), (Instant::now(), last.clone()));
    }

    fn refuse(&self) -> Last {
        self.not_asked.fetch_add(1, Ordering::Relaxed);
        Last::NotAsked
    }

    /// Over the limit, the answer is `NotAsked` and nothing is sent.
    pub async fn last(&self, job: &str) -> Last {
        if let Some(last) = self.cached(job) {
            return last;
        }
        let Ok(_permit) = self.running.try_acquire() else {
            return self.refuse();
        };
        if !self
            .window
            .lock()
            .unwrap()
            .take(Instant::now(), self.per_minute)
        {
            return self.refuse();
        }
        let last = self.ask(job).await;
        self.keep(job, &last);
        last
    }

    async fn ask(&self, job: &str) -> Last {
        let Ok(resp) = self
            .http
            .get(format!("{}/job/{job}/latest-finished", self.base))
            .header("Accept", "application/json")
            .send()
            .await
        else {
            return Last::NoAnswer;
        };
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Last::Missing;
        }
        if !resp.status().is_success() {
            return Last::NoAnswer;
        }
        resp.json::<Value>()
            .await
            .ok()
            .and_then(|v| {
                let code = v["buildstatus"].as_i64()?;
                Some(Build {
                    id: v["id"].as_u64()?,
                    ok: code == 0,
                    status: status_word(code),
                    at: v["stoptime"]
                        .as_i64()
                        .or(v["timestamp"].as_i64())
                        .and_then(|t| DateTime::from_timestamp(t, 0)),
                })
            })
            .map_or(Last::Missing, Last::Build)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tracker_issue_link_gives_the_issue_number() {
        assert_eq!(
            issue_number("https://github.com/NixOS/nixpkgs/issues/574783"),
            Some(574783)
        );
        assert_eq!(issue_number("https://example.org/1"), None);
    }

    #[test]
    fn a_tracker_page_keeps_the_ranges_of_each_attribute() {
        let page = serde_json::json!({ "results": [{
            "code": "NIXPKGS-2026-3072",
            "title": "cups: security issues < 2.4.20",
            "github_issue_url": "https://github.com/NixOS/nixpkgs/issues/574783",
            "suggestions": [
                {
                    "cve_id": "CVE-2026-1",
                    "affected_products": [
                        { "name": "libcups", "version_constraints": [["affected", "<9"]] },
                        { "name": "cups", "version_constraints": [["affected", "==< 2.4.20"]] }
                    ],
                    "packages": { "cups": { "branches": {} } }
                },
                {
                    "cve_id": "CVE-2026-2",
                    "affected_products": "[{\"name\":\"CUPS\",\"version_constraints\":[[\"unaffected\",\"==2.4.20\"],[\"affected\",\"<2.4.20\"]]}]",
                    "packages": "{\"cups\":{},\"cups-filters\":{}}"
                }
            ]
        }]});
        let mut out = BTreeMap::new();
        read_page(&page, &mut out);
        let cups = &out["cups"][0];
        assert_eq!(cups.cves, ["CVE-2026-1", "CVE-2026-2"]);
        assert_eq!(cups.issue, Some(574783));
        assert_eq!(cups.ranges.len(), 2);
        assert_eq!(
            cups.ranges[0].constraints,
            [("affected".to_string(), "==< 2.4.20".to_string())]
        );
        assert_eq!(cups.ranges[1].constraints.len(), 2);
        assert_eq!(out["cups-filters"][0].ranges.len(), 1);
        let v = serde_json::to_value(cups).unwrap();
        let back: Advisory = serde_json::from_value(v).unwrap();
        assert_eq!(back.ranges, cups.ranges);
        let old: Advisory =
            serde_json::from_str(r#"{"code":"A","cves":[],"title":"t","issue":null}"#).unwrap();
        assert!(old.ranges.is_empty());
    }

    #[test]
    fn an_osv_match_shows_once_and_not_beside_its_tracker_advisory() {
        let mut h = Health::default();
        h.advisories.insert(
            "requests".into(),
            vec![Advisory {
                code: "NIXPKGS-1".into(),
                cves: vec!["CVE-1".into()],
                title: String::new(),
                issue: None,
                ranges: vec![],
            }],
        );
        h.osv.insert(
            "requests".into(),
            vec!["GHSA-a".into(), "GHSA-b".into(), "PYSEC-b".into()],
        );
        let v = |cves: &[&str], aliases: &[&str]| crate::osv::Vuln {
            cves: cves.iter().map(|s| s.to_string()).collect(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        h.osv_vulns
            .insert("GHSA-a".into(), v(&["CVE-1"], &["CVE-1"]));
        h.osv_vulns.insert("GHSA-b".into(), v(&[], &[]));
        h.osv_vulns.insert("PYSEC-b".into(), v(&[], &["GHSA-b"]));
        let ids: Vec<&str> = h
            .osv_extra("requests")
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(ids, vec!["GHSA-b"]);
    }

    #[test]
    fn the_minute_window_stops_at_its_cap_and_opens_again() {
        let t0 = Instant::now();
        let mut w = Window { start: t0, used: 0 };
        assert!(w.take(t0, 2));
        assert!(w.take(t0, 2));
        assert!(!w.take(t0 + Duration::from_secs(59), 2));
        assert!(w.take(t0 + Duration::from_secs(60), 2));
    }

    /// A Hydra stand-in that answers 404 and counts the requests.
    async fn not_found_server() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                h.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf).await;
                let _ = s
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    )
                    .await;
            }
        });
        (base, hits)
    }

    #[tokio::test]
    async fn a_missing_job_is_kept_and_not_asked_again() {
        let (base, hits) = not_found_server().await;
        let hydra = Hydra::new(&base, 4, 30);
        assert!(matches!(hydra.last("a").await, Last::Missing));
        assert!(matches!(hydra.last("a").await, Last::Missing));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn over_the_minute_cap_nothing_is_sent() {
        let (base, hits) = not_found_server().await;
        let hydra = Hydra::new(&base, 4, 1);
        assert!(matches!(hydra.last("a").await, Last::Missing));
        assert!(matches!(hydra.last("b").await, Last::NotAsked));
        assert!(matches!(hydra.last("a").await, Last::Missing));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(hydra.stats()["not_asked"], 1);
        assert_eq!(hydra.stats()["cached"], 1);
    }

    #[tokio::test]
    async fn with_every_request_running_nothing_more_is_sent() {
        let (base, hits) = not_found_server().await;
        let hydra = Hydra::new(&base, 1, 30);
        let held = hydra.running.try_acquire().unwrap();
        assert!(matches!(hydra.last("a").await, Last::NotAsked));
        drop(held);
        assert!(matches!(hydra.last("a").await, Last::Missing));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn no_answer_is_kept_a_short_time() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let hydra = Hydra::new(&base, 4, 1);
        assert!(matches!(hydra.last("a").await, Last::NoAnswer));
        assert!(matches!(hydra.last("a").await, Last::NoAnswer));
        assert_eq!(hydra.stats()["not_asked"], 0);
        assert_eq!(Last::NoAnswer.ttl(), HYDRA_RETRY);
    }
}
