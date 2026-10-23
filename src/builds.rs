//! The Hydra build state of the packages that people maintain or watch, on
//! master and on each active channel, and its changes, which become inbox
//! events.
//!
//! Two sources, both small for hydra.nixos.org: a feed of the latest
//! finished builds of `nixpkgs/unstable`, asked every 5 minutes, and a
//! slow sweep that asks one job at a time. The feed is sorted by build id,
//! not by finish time, so a late build of a mass rebuild can fall out of
//! it; the sweep finds it within a day. The feed is for master only:
//! `latestbuilds` without a job on `nixos/trunk-combined` took 642 s on
//! 2026-10-23, and nothing tells which jobsets are slow like that.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration as StdDuration;

use crate::area::Index;
use crate::catalog::{Event, Subject};
use crate::web::App;

pub(crate) const SYSTEM: &str = "x86_64-linux";
const FEED_ROWS: usize = 200;
/// Asked once when none of the [`FEED_ROWS`] rows were seen before.
const FEED_ROWS_MORE: usize = 1000;
const FEED_EVERY: u32 = 50;
/// One sweep request a tick: 10 a minute.
const TICK: StdDuration = StdDuration::from_secs(6);
const SWEEP_AFTER: Duration = Duration::hours(24);
const TRACK_EVERY: u32 = 600;
/// Three, so that a cancelled last build still shows the one before it.
const JOB_ROWS: usize = 3;
/// The builds a page shows: one request, about 6 KB.
pub(crate) const HISTORY: usize = 30;
const KEEP_DAYS: i64 = 45;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Passes,
    Fails,
    DependencyFails,
}

impl State {
    /// Aborted and cancelled builds say nothing about the package.
    pub fn of(code: i64) -> Option<State> {
        match code {
            0 => Some(State::Passes),
            2 => Some(State::DependencyFails),
            3 | 4 | 9 => None,
            _ => Some(State::Fails),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            State::Passes => "passes",
            State::Fails => "fails",
            State::DependencyFails => "dependency fails",
        }
    }

    pub fn parse(s: &str) -> Option<State> {
        [State::Passes, State::Fails, State::DependencyFails]
            .into_iter()
            .find(|x| x.key() == s)
    }

    pub fn red(self) -> bool {
        self != State::Passes
    }
}

/// The Hydra jobset that builds the packages of a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jobset {
    pub project: String,
    pub name: String,
    /// The job of `hello` is `nixpkgs.hello.x86_64-linux` in a NixOS
    /// release jobset.
    prefix: &'static str,
}

impl Jobset {
    pub fn master() -> Jobset {
        Jobset {
            project: "nixpkgs".into(),
            name: "unstable".into(),
            prefix: "",
        }
    }

    /// The unstable channels follow master; a channel of another system,
    /// like `nixpkgs-26.05-darwin`, has none.
    pub fn of_channel(channel: &str) -> Option<Jobset> {
        if matches!(
            channel,
            "nixos-unstable" | "nixos-unstable-small" | "nixpkgs-unstable"
        ) {
            return Some(Jobset::master());
        }
        let rest = channel.strip_prefix("nixos-")?;
        let version = rest.strip_suffix("-small").unwrap_or(rest);
        let (y, m) = version.split_once('.')?;
        let digits = |s: &str| s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit());
        (digits(y) && digits(m)).then(|| Jobset {
            project: "nixos".into(),
            name: format!("release-{rest}"),
            prefix: "nixpkgs.",
        })
    }

    pub fn key(&self) -> String {
        format!("{}/{}", self.project, self.name)
    }

    pub fn job(&self, attr: &str) -> String {
        format!("{}{attr}.{SYSTEM}", self.prefix)
    }

    /// What [`crate::health::Hydra::last`] takes.
    pub fn path(&self, attr: &str) -> String {
        format!("{}/{}", self.key(), self.job(attr))
    }

    /// `hello.x86_64-linux` gives `hello`; another system gives nothing.
    pub fn attr_of<'a>(&self, job: &'a str) -> Option<&'a str> {
        job.strip_prefix(self.prefix)?
            .strip_suffix(SYSTEM)?
            .strip_suffix('.')
    }

    pub fn label(&self) -> String {
        label(&self.key())
    }
}

/// `nixos/release-26.05` is `nixos-26.05`; master is `unstable`.
pub fn label(key: &str) -> String {
    match key.strip_prefix("nixos/release-") {
        Some(v) => format!("nixos-{v}"),
        None if key == "nixpkgs/unstable" => "unstable".into(),
        None => key.into(),
    }
}

/// Master first, then the jobset of each configured channel, once each.
pub fn jobsets(channels: &crate::channels::Channels) -> Vec<Jobset> {
    let mut out = vec![Jobset::master()];
    for c in &channels.list {
        if let Some(j) = Jobset::of_channel(&c.name).filter(|j| !out.contains(j)) {
            out.push(j);
        }
    }
    out
}

/// One finished build, as `/api/latestbuilds` gives it.
#[derive(Debug, Clone, Deserialize)]
pub struct Row {
    pub id: u64,
    pub job: String,
    pub buildstatus: Option<i64>,
    pub timestamp: Option<i64>,
    #[serde(default)]
    pub nixname: Option<String>,
}

/// One build of a job's history, the newest first.
#[derive(Debug, Clone, Serialize)]
pub struct Past {
    pub id: u64,
    /// Hydra's words for `buildstatus`.
    pub status: &'static str,
    /// None for an aborted or cancelled build.
    pub state: Option<&'static str>,
    /// When Hydra queued it.
    pub at: Option<DateTime<Utc>>,
    /// Like `python3.14-tornado-6.5.8`.
    pub name: Option<String>,
}

pub(crate) fn history(rows: &[Row]) -> Vec<Past> {
    rows.iter()
        .filter_map(|r| {
            let code = r.buildstatus?;
            Some(Past {
                id: r.id,
                status: crate::health::status_word(code),
                state: State::of(code).map(State::key),
                at: r.timestamp.and_then(|t| DateTime::from_timestamp(t, 0)),
                name: r.nixname.clone(),
            })
        })
        .collect()
}

/// nixpkgs rebuilds nearly everything every few weeks, so a job with no
/// build in this time is a name that Hydra no longer builds, like
/// `python3Packages.tornado`, last built in 2016.
const STALE: Duration = Duration::days(180);

/// A build queued within [`STALE`]; a build with no time counts as recent.
pub fn recent(queued: Option<i64>, now: DateTime<Utc>) -> bool {
    queued
        .and_then(|t| DateTime::from_timestamp(t, 0))
        .is_none_or(|t| now - t < STALE)
}

/// A job that Hydra no longer builds: nothing recent, and no build of the
/// version that nixpkgs has now, when that version is known.
pub fn dead(h: &[Past], version: Option<&str>, now: DateTime<Utc>) -> bool {
    let Some(newest) = h.iter().filter_map(|p| p.at).max() else {
        return false;
    };
    if now - newest < STALE {
        return false;
    }
    match version.filter(|v| !v.is_empty()) {
        Some(v) => {
            let tail = format!("-{v}");
            !h.iter()
                .any(|p| p.name.as_deref().is_some_and(|n| n.ends_with(&tail)))
        }
        None => true,
    }
}

/// The builds that passed, and the builds that tell: aborted and
/// cancelled ones do not count.
pub fn passed(h: &[Past]) -> (usize, usize) {
    let told: Vec<&Past> = h.iter().filter(|p| p.state.is_some()).collect();
    let ok = told
        .iter()
        .filter(|p| p.state == Some(State::Passes.key()))
        .count();
    (ok, told.len())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub jobset: String,
    pub attr: String,
    pub build: u64,
    pub state: State,
    pub at: DateTime<Utc>,
}

type Key = (String, String);

/// The state of each tracked package on each jobset, in memory, and the
/// builds that page views learned, kept until the background task stores
/// them.
pub struct Builds {
    states: RwLock<HashMap<Key, (u64, State)>>,
    jobsets: RwLock<Arc<Vec<Jobset>>>,
    tracked: RwLock<Arc<HashSet<String>>>,
    pending: Mutex<Vec<(String, String, u64, i64)>>,
    stats: Mutex<Stats>,
    /// output path → whether cache.nixos.org has it, and when asked.
    cache: RwLock<HashMap<String, (bool, DateTime<Utc>)>>,
}

impl Default for Builds {
    fn default() -> Self {
        Builds {
            states: Default::default(),
            jobsets: RwLock::new(Arc::new(vec![Jobset::master()])),
            tracked: Default::default(),
            pending: Default::default(),
            stats: Default::default(),
            cache: Default::default(),
        }
    }
}

#[derive(Default, Clone)]
struct Stats {
    feed_requests: u64,
    feed_rows: u64,
    sweep_requests: u64,
    failed_requests: u64,
    changes: u64,
    cache_requests: u64,
    feed_at: Option<DateTime<Utc>>,
}

impl Builds {
    pub fn tracked(&self) -> Arc<HashSet<String>> {
        self.tracked.read().unwrap().clone()
    }

    /// Whether cache.nixos.org has the path, when the server asked.
    pub fn cached(&self, path: &str) -> Option<bool> {
        self.cache.read().unwrap().get(path).map(|(ok, _)| *ok)
    }

    pub fn jobsets(&self) -> Arc<Vec<Jobset>> {
        self.jobsets.read().unwrap().clone()
    }

    pub fn state(&self, jobset: &str, attr: &str) -> Option<State> {
        self.states
            .read()
            .unwrap()
            .get(&(jobset.to_string(), attr.to_string()))
            .map(|(_, s)| *s)
    }

    /// The first jobset, in the order of [`jobsets`], where the last build
    /// of `attr` is red, with its state.
    pub fn red_on(&self, attr: &str) -> Option<(State, String)> {
        let states = self.states.read().unwrap();
        self.jobsets().iter().find_map(|j| {
            let s = states.get(&(j.key(), attr.to_string()))?.1;
            s.red().then(|| (s, j.label()))
        })
    }

    pub fn red(&self, attrs: impl IntoIterator<Item = String>) -> HashMap<String, (State, String)> {
        attrs
            .into_iter()
            .filter_map(|a| self.red_on(&a).map(|r| (a, r)))
            .collect()
    }

    /// A page view learned a build of `project/jobset/job`; the background
    /// task stores it.
    pub fn saw(&self, path: &str, id: u64, code: i64) {
        let mut parts = path.splitn(3, '/');
        let (Some(p), Some(n), Some(job)) = (parts.next(), parts.next(), parts.next()) else {
            return;
        };
        let jobsets = self.jobsets();
        let Some(j) = jobsets.iter().find(|j| j.project == p && j.name == n) else {
            return;
        };
        let Some(attr) = j.attr_of(job) else { return };
        if !self.tracked.read().unwrap().contains(attr) {
            return;
        }
        let mut pending = self.pending.lock().unwrap();
        if pending.len() < 1000 {
            pending.push((j.key(), attr.to_string(), id, code));
        }
    }

    pub fn stats(&self) -> serde_json::Value {
        let s = self.stats.lock().unwrap().clone();
        serde_json::json!({
            "tracked": self.tracked.read().unwrap().len(),
            "jobsets": self.jobsets().iter().map(Jobset::key).collect::<Vec<_>>(),
            "feed_requests": s.feed_requests,
            "feed_rows": s.feed_rows,
            "sweep_requests": s.sweep_requests,
            "failed_requests": s.failed_requests,
            "changes": s.changes,
            "cache_requests": s.cache_requests,
            "cache_known": self.cache.read().unwrap().len(),
            "feed_at": s.feed_at,
            "feed_every_seconds": TICK.as_secs() * u64::from(FEED_EVERY),
            "sweep_per_minute": 60 / TICK.as_secs(),
        })
    }

    /// No build tells the state any more, or only a dead job's old builds
    /// do.
    fn forget(
        &self,
        store: &crate::store::Store,
        jobset: &str,
        attr: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.states
            .write()
            .unwrap()
            .remove(&(jobset.to_string(), attr.to_string()));
        store.forget_hydra_state(jobset, attr, now)
    }

    /// Records a build of a tracked package. Gives the change, if the state
    /// changed from a known state: the first build seen is only a start.
    fn record(
        &self,
        store: &crate::store::Store,
        jobset: &str,
        attr: &str,
        id: u64,
        code: i64,
        now: DateTime<Utc>,
    ) -> Option<Change> {
        let key = (jobset.to_string(), attr.to_string());
        let before = self.states.read().unwrap().get(&key).copied();
        if before.is_some_and(|(b, _)| b > id) {
            return None;
        }
        let Some(state) = State::of(code) else {
            let _ = store.hydra_checked(jobset, attr, now);
            return None;
        };
        let change = before
            .filter(|(b, s)| *b < id && *s != state)
            .map(|_| Change {
                jobset: jobset.to_string(),
                attr: attr.to_string(),
                build: id,
                state,
                at: now,
            });
        if let Err(e) = store.put_hydra_state(jobset, attr, id, state, now, change.as_ref()) {
            tracing::warn!("hydra state {jobset} {attr}: {e:#}");
            return None;
        }
        self.states.write().unwrap().insert(key, (id, state));
        if change.is_some() {
            self.stats.lock().unwrap().changes += 1;
        }
        change
    }
}

/// The packages that someone who uses the site maintains or watches.
fn tracked(app: &App) -> HashSet<String> {
    let mut out: HashSet<String> = app
        .store
        .watched_by_anyone()
        .unwrap_or_default()
        .iter()
        .filter_map(|k| match Subject::parse(k) {
            Some(Subject::Pkg(a)) => Some(a),
            _ => None,
        })
        .collect();
    let index = app.index.read().unwrap();
    for login in app.store.logins().unwrap_or_default() {
        for (sub, _) in crate::catalog::maintained(&index, &login).packages {
            if let Subject::Pkg(a) = sub {
                out.insert(a);
            }
        }
    }
    out
}

pub(crate) fn latest_url(base: &str, j: &Jobset, job: Option<&str>, rows: usize) -> String {
    let mut url = format!(
        "{base}/api/latestbuilds?nr={rows}&project={}&jobset={}",
        j.project, j.name
    );
    match job {
        Some(job) => url.push_str(&format!("&job={}", crate::github::urlencode(job))),
        None => url.push_str(&format!("&system={SYSTEM}")),
    }
    url
}

async fn fetch(http: &reqwest::Client, url: &str) -> Result<Vec<Row>> {
    http.get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("latestbuilds")
}

/// The first build that says something: the newest one not aborted or
/// cancelled.
pub(crate) fn telling(rows: &[Row]) -> Option<&Row> {
    rows.iter()
        .find(|r| r.buildstatus.is_some_and(|c| State::of(c).is_some()))
}

/// The ids not seen before, and whether the rows reach back to them.
fn fresh(rows: &[Row], seen_max: u64) -> (Vec<&Row>, bool) {
    let new: Vec<&Row> = rows.iter().filter(|r| r.id > seen_max).collect();
    let reaches = rows.iter().any(|r| r.id <= seen_max) || seen_max == 0;
    (new, reaches)
}

async fn feed(app: &App, http: &reqwest::Client, base: &str) -> Result<()> {
    let master = Jobset::master();
    let seen_max: u64 = app
        .store
        .get("hydra_feed_max")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut rows = fetch(http, &latest_url(base, &master, None, FEED_ROWS)).await?;
    let mut requests = 1;
    if !fresh(&rows, seen_max).1 {
        rows = fetch(http, &latest_url(base, &master, None, FEED_ROWS_MORE)).await?;
        requests += 1;
    }
    let tracked = app.hydra.builds.tracked.read().unwrap().clone();
    let now = Utc::now();
    let (mut new, _) = fresh(&rows, seen_max);
    new.sort_by_key(|r| r.id);
    for r in &new {
        let (Some(attr), Some(code)) = (master.attr_of(&r.job), r.buildstatus) else {
            continue;
        };
        if tracked.contains(attr) {
            app.hydra
                .builds
                .record(&app.store, &master.key(), attr, r.id, code, now);
        }
    }
    if let Some(max) = rows.iter().map(|r| r.id).max().filter(|m| *m > seen_max) {
        app.store.set("hydra_feed_max", &max.to_string())?;
    }
    let mut s = app.hydra.builds.stats.lock().unwrap();
    s.feed_requests += requests;
    s.feed_rows += rows.len() as u64;
    s.feed_at = Some(now);
    Ok(())
}

async fn sweep_one(app: &App, http: &reqwest::Client, base: &str) -> Result<bool> {
    let tracked = app.hydra.builds.tracked.read().unwrap().clone();
    let jobsets = app.hydra.builds.jobsets();
    let now = Utc::now();
    let checked = app.store.hydra_checked_at()?;
    let due = jobsets
        .iter()
        .flat_map(|j| tracked.iter().map(move |a| (j, a)))
        .map(|(j, a)| {
            let at = checked.get(&(j.key(), a.clone())).copied();
            (at, j, a)
        })
        .filter(|(at, ..)| at.is_none_or(|t| now - t >= SWEEP_AFTER))
        .min_by(|x, y| (x.0, x.2, x.1.key()).cmp(&(y.0, y.2, y.1.key())));
    let Some((_, j, attr)) = due else {
        return Ok(false);
    };
    app.hydra.builds.stats.lock().unwrap().sweep_requests += 1;
    let rows = fetch(http, &latest_url(base, j, Some(&j.job(attr)), JOB_ROWS)).await?;
    match telling(&rows).filter(|r| recent(r.timestamp, now)) {
        Some(r) => {
            app.hydra.builds.record(
                &app.store,
                &j.key(),
                attr,
                r.id,
                r.buildstatus.unwrap_or(1),
                now,
            );
        }
        None => app.hydra.builds.forget(&app.store, &j.key(), attr, now)?,
    }
    Ok(true)
}

const CACHE: &str = "https://cache.nixos.org";
/// A path that is there stays there; a missing one is asked again later.
const CACHE_AGAIN: Duration = Duration::hours(1);
/// cache.nixos.org is a CDN: 5 small requests a tick are 50 a minute.
const CACHE_PER_TICK: usize = 5;

/// `toJSON` and not a JSON attribute set: the iets evaluator does not
/// force nested values when it prints JSON.
const OUT_PATHS_EXPR: &str = r#"
let
  input = builtins.fromJSON (builtins.readFile ./input.json);
  pkgs = import input.path {
    inherit (input) system;
    overlays = [ ];
    config = {
      allowUnfree = true;
      allowBroken = true;
      allowInsecurePredicate = _: true;
    };
  };
  lib = pkgs.lib;
  path =
    attr:
    let
      v = builtins.tryEval (
        let
          p = lib.attrByPath (lib.splitString "." attr) null pkgs;
        in
        if lib.isDerivation p then builtins.seq p.outPath p.outPath else null
      );
    in
    if v.success then v.value else null;
in
builtins.toJSON (
  lib.filterAttrs (_: v: v != null) (
    lib.listToAttrs (
      map (a: {
        name = a;
        value = path a;
      }) input.attrs
    )
  )
)
"#;

/// The output path of each of `attrs` in the nixpkgs at `dir`, under the
/// sweep lock. A few hundred attributes take seconds.
pub fn out_paths(dir: &std::path::Path, attrs: &[String]) -> Result<HashMap<String, String>> {
    if attrs.is_empty() {
        return Ok(HashMap::new());
    }
    let _one = crate::eval::SWEEP.lock().unwrap_or_else(|e| e.into_inner());
    let input = serde_json::json!({
        "path": dir.to_string_lossy(),
        "system": SYSTEM,
        "attrs": attrs,
    });
    let text = crate::eval::eval_with_input("outpaths", OUT_PATHS_EXPR, &input)?;
    let json: String = serde_json::from_str(&text)?;
    Ok(serde_json::from_str(&json)?)
}

/// `/nix/store/<hash>-name` gives `<hash>`.
fn store_hash(path: &str) -> Option<&str> {
    let h = path.strip_prefix("/nix/store/")?.get(..32)?;
    h.bytes().all(|b| b.is_ascii_alphanumeric()).then_some(h)
}

/// The output paths of the tracked packages on each channel, the ones not
/// asked yet first.
fn cache_due(app: &App, now: DateTime<Utc>) -> Vec<String> {
    let tracked = app.hydra.builds.tracked();
    let cache = app.hydra.builds.cache.read().unwrap();
    let mut due: Vec<(Option<DateTime<Utc>>, String)> = app
        .channels
        .read()
        .unwrap()
        .list
        .iter()
        .flat_map(|c| c.out_paths.iter())
        .filter(|(a, _)| tracked.contains(*a))
        .filter_map(|(_, p)| match cache.get(p) {
            None => Some((None, p.clone())),
            Some((false, at)) if now - *at >= CACHE_AGAIN => Some((Some(*at), p.clone())),
            _ => None,
        })
        .collect();
    due.sort();
    due.dedup();
    due.into_iter()
        .map(|(_, p)| p)
        .take(CACHE_PER_TICK)
        .collect()
}

async fn cache_some(app: &App, http: &reqwest::Client, base: &str) -> Result<()> {
    let now = Utc::now();
    for path in cache_due(app, now) {
        let Some(hash) = store_hash(&path) else {
            continue;
        };
        app.hydra.builds.stats.lock().unwrap().cache_requests += 1;
        let status = http
            .head(format!("{base}/{hash}.narinfo"))
            .send()
            .await?
            .status();
        let ok = match status.as_u16() {
            200 => true,
            404 => false,
            _ => anyhow::bail!("cache.nixos.org answered {status}"),
        };
        app.hydra
            .builds
            .cache
            .write()
            .unwrap()
            .insert(path, (ok, now));
    }
    Ok(())
}

fn flush(app: &App) {
    let pending = std::mem::take(&mut *app.hydra.builds.pending.lock().unwrap());
    let now = Utc::now();
    for (jobset, attr, id, code) in pending {
        app.hydra
            .builds
            .record(&app.store, &jobset, &attr, id, code, now);
    }
}

fn load(app: &App) {
    match app.store.hydra_states() {
        Ok(list) => {
            let mut states = app.hydra.builds.states.write().unwrap();
            for (jobset, attr, build, state) in list {
                states.insert((jobset, attr), (build, state));
            }
        }
        Err(e) => tracing::warn!("hydra states: {e:#}"),
    }
}

/// The channels resolve after the start, so the jobsets follow them.
fn refresh(app: &App) {
    let j = jobsets(&app.channels.read().unwrap());
    *app.hydra.builds.jobsets.write().unwrap() = Arc::new(j);
}

pub async fn run(app: Arc<App>) {
    run_with(app, crate::health::HYDRA.to_string(), CACHE.to_string()).await
}

async fn run_with(app: Arc<App>, base: String, cache: String) {
    let http = reqwest::Client::builder()
        .user_agent(crate::health::HYDRA_AGENT)
        .timeout(StdDuration::from_secs(30))
        .build()
        .expect("http client");
    load(&app);
    let mut tick: u32 = 0;
    loop {
        if tick.is_multiple_of(TRACK_EVERY) {
            refresh(&app);
            let a = app.clone();
            if let Ok(t) = tokio::task::spawn_blocking(move || tracked(&a)).await {
                *app.hydra.builds.tracked.write().unwrap() = Arc::new(t);
            }
            if let Err(e) = app
                .store
                .purge_hydra(Utc::now() - Duration::days(KEEP_DAYS))
            {
                tracing::warn!("hydra purge: {e:#}");
            }
        }
        flush(&app);
        let result = if tick.is_multiple_of(FEED_EVERY) {
            feed(&app, &http, &base).await
        } else {
            sweep_one(&app, &http, &base).await.map(|_| ())
        };
        if let Err(e) = result {
            app.hydra.builds.stats.lock().unwrap().failed_requests += 1;
            tracing::warn!("hydra builds: {e:#}");
        }
        if let Err(e) = cache_some(&app, &http, &cache).await {
            app.hydra.builds.stats.lock().unwrap().failed_requests += 1;
            tracing::warn!("binary cache: {e:#}");
        }
        tick = tick.wrapping_add(1);
        tokio::time::sleep(TICK).await;
    }
}

fn title(c: &Change) -> String {
    let what = match c.state {
        State::Passes => "Hydra build fixed",
        State::Fails => "Hydra build fails",
        State::DependencyFails => "Hydra build fails: a dependency fails",
    };
    if c.jobset == Jobset::master().key() {
        what.into()
    } else {
        format!("{what} on {}", label(&c.jobset))
    }
}

/// The build changes of `attrs` since `since`, as feed events.
pub(crate) fn events(app: &App, attrs: &BTreeSet<String>, since: DateTime<Utc>) -> Vec<Event> {
    app.store
        .hydra_changes(since)
        .unwrap_or_default()
        .into_iter()
        .filter(|c| attrs.contains(&c.attr))
        .map(|c| Event {
            at: c.at,
            id: format!("hydra-{}-{}-{}", label(&c.jobset), c.attr, c.build),
            title: title(&c),
            url: format!("{}/build/{}", crate::health::HYDRA, c.build),
            subject: c.attr,
        })
        .collect()
}

/// What a person gets build events for: what they maintain and what they
/// watch.
pub(crate) fn interests(index: &Index, login: &str, watched: &[Subject]) -> BTreeSet<String> {
    crate::catalog::maintained(index, login)
        .packages
        .into_iter()
        .chain(watched.iter().cloned().map(|s| (s, None)))
        .filter_map(|(s, _)| match s {
            Subject::Pkg(a) => Some(a),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    const MASTER: &str = "nixpkgs/unstable";
    const STABLE: &str = "nixos/release-26.05";

    fn store() -> Store {
        Store::open(std::path::Path::new(":memory:")).unwrap()
    }

    fn row(id: u64, job: &str, code: i64) -> Row {
        Row {
            id,
            job: job.into(),
            buildstatus: Some(code),
            timestamp: None,
            nixname: None,
        }
    }

    #[test]
    fn the_status_code_gives_the_state() {
        assert_eq!(State::of(0), Some(State::Passes));
        assert_eq!(State::of(1), Some(State::Fails));
        assert_eq!(State::of(2), Some(State::DependencyFails));
        assert_eq!(State::of(7), Some(State::Fails));
        for c in [3, 4, 9] {
            assert_eq!(State::of(c), None, "{c}");
        }
        for s in [State::Passes, State::Fails, State::DependencyFails] {
            assert_eq!(State::parse(s.key()), Some(s));
        }
    }

    #[test]
    fn each_channel_has_its_jobset_and_job_names() {
        let unstable = Jobset::of_channel("nixos-unstable").unwrap();
        assert_eq!(unstable, Jobset::master());
        assert_eq!(
            unstable.path("hello"),
            "nixpkgs/unstable/hello.x86_64-linux"
        );
        let stable = Jobset::of_channel("nixos-26.05").unwrap();
        assert_eq!(stable.key(), STABLE);
        assert_eq!(stable.job("hello"), "nixpkgs.hello.x86_64-linux");
        assert_eq!(stable.label(), "nixos-26.05");
        assert_eq!(stable.attr_of("nixpkgs.hello.x86_64-linux"), Some("hello"));
        assert_eq!(stable.attr_of("hello.x86_64-linux"), None);
        assert_eq!(
            Jobset::of_channel("nixos-26.05-small").unwrap().key(),
            "nixos/release-26.05-small"
        );
        for other in ["nixpkgs-26.05-darwin", "nixos-26.5", "stable", ""] {
            assert_eq!(Jobset::of_channel(other), None, "{other}");
        }
        assert_eq!(
            unstable.attr_of("python3Packages.foo.x86_64-linux"),
            Some("python3Packages.foo")
        );
        assert_eq!(unstable.attr_of("hello.aarch64-linux"), None);
        let ch = crate::channels::Channels {
            list: ["nixos-unstable", "nixos-26.05", "", "nixos-26.05"]
                .iter()
                .map(|n| crate::channels::Channel {
                    name: n.to_string(),
                    ..Default::default()
                })
                .collect(),
        };
        let keys: Vec<String> = jobsets(&ch).iter().map(Jobset::key).collect();
        assert_eq!(keys, [MASTER, STABLE]);
    }

    #[test]
    fn only_a_change_from_a_known_state_is_a_change() {
        let s = store();
        let b = Builds::default();
        let t = Utc::now();
        assert_eq!(b.record(&s, MASTER, "a", 10, 0, t), None);
        assert_eq!(b.record(&s, MASTER, "a", 11, 0, t), None);
        assert_eq!(b.record(&s, MASTER, "a", 12, 4, t), None);
        assert_eq!(b.state(MASTER, "a"), Some(State::Passes));
        let c = b.record(&s, MASTER, "a", 13, 1, t).unwrap();
        assert_eq!((c.build, c.state), (13, State::Fails));
        assert_eq!(b.record(&s, MASTER, "a", 9, 0, t), None, "an older build");
        assert_eq!(b.state(MASTER, "a"), Some(State::Fails));
        assert_eq!(
            b.record(&s, MASTER, "a", 13, 1, t),
            None,
            "the same build again"
        );
        let c = b.record(&s, MASTER, "a", 14, 2, t).unwrap();
        assert_eq!(c.state, State::DependencyFails);
        assert_eq!(
            b.record(&s, MASTER, "a", 15, 0, t).unwrap().state,
            State::Passes
        );
        assert_eq!(
            b.record(&s, STABLE, "a", 8, 1, t),
            None,
            "each jobset starts alone"
        );
        assert_eq!(b.state(STABLE, "a"), Some(State::Fails));
        let changes = s.hydra_changes(t - Duration::hours(1)).unwrap();
        assert_eq!(
            changes.iter().map(|c| c.build).collect::<Vec<_>>(),
            vec![15, 14, 13]
        );
        let again = Builds::default();
        *again.states.write().unwrap() = s
            .hydra_states()
            .unwrap()
            .into_iter()
            .map(|(j, a, b, st)| ((j, a), (b, st)))
            .collect();
        assert_eq!(again.state(MASTER, "a"), Some(State::Passes));
        assert_eq!(again.state(STABLE, "a"), Some(State::Fails));
    }

    #[test]
    fn red_names_the_first_jobset_where_the_build_is_red() {
        let s = store();
        let b = Builds::default();
        *b.jobsets.write().unwrap() = Arc::new(vec![
            Jobset::master(),
            Jobset::of_channel("nixos-26.05").unwrap(),
        ]);
        let t = Utc::now();
        b.record(&s, MASTER, "a", 1, 0, t);
        b.record(&s, STABLE, "a", 2, 2, t);
        b.record(&s, MASTER, "b", 3, 1, t);
        b.record(&s, STABLE, "b", 4, 1, t);
        let red = b.red(["a".to_string(), "b".to_string(), "c".to_string()]);
        assert_eq!(
            red["a"],
            (State::DependencyFails, "nixos-26.05".to_string())
        );
        assert_eq!(red["b"], (State::Fails, "unstable".to_string()));
        assert!(!red.contains_key("c"));
    }

    #[test]
    fn the_feed_asks_for_more_only_when_it_does_not_reach_back() {
        let r = vec![
            row(30, "a.x86_64-linux", 0),
            row(20, "a.x86_64-linux", 0),
            row(10, "a.x86_64-linux", 0),
        ];
        let (new, reaches) = fresh(&r, 20);
        assert_eq!(new.len(), 1);
        assert!(reaches);
        assert!(!fresh(&r, 5).1);
        assert!(fresh(&r, 0).1, "the first poll only starts");
    }

    #[test]
    fn a_cancelled_last_build_shows_the_one_before() {
        let r = vec![row(3, "a.x86_64-linux", 4), row(2, "a.x86_64-linux", 1)];
        assert_eq!(telling(&r).unwrap().id, 2);
    }

    #[test]
    fn a_job_with_only_old_builds_of_another_version_is_dead() {
        let now = Utc::now();
        let at = |days: i64| Some((now - Duration::days(days)).timestamp());
        let past = |name: &str, days| {
            history(&[Row {
                timestamp: at(days),
                nixname: Some(name.into()),
                ..row(1, "a", 0)
            }])
        };
        let tornado = past("python3.4-tornado-4.2.1", 3900);
        assert!(dead(&tornado, Some("6.5.8"), now));
        assert!(dead(&tornado, None, now));
        assert!(
            !dead(&past("python3.14-tornado-6.5.8", 3900), Some("6.5.8"), now),
            "an old build of the current version"
        );
        assert!(
            !dead(&past("python3.4-tornado-4.2.1", 30), Some("6.5.8"), now),
            "a recent build: the channel can lag"
        );
        assert!(!dead(&[], Some("1"), now));
        assert!(recent(at(30), now));
        assert!(!recent(at(200), now));
        assert!(recent(None, now));
    }

    #[test]
    fn the_history_counts_the_builds_that_tell() {
        let h = history(&[
            row(5, "a", 0),
            row(4, "a", 4),
            row(3, "a", 1),
            row(2, "a", 0),
            Row {
                buildstatus: None,
                ..row(1, "a", 0)
            },
        ]);
        assert_eq!(h.len(), 4, "an unfinished build is left out");
        assert_eq!(h[1].state, None);
        assert_eq!(h[2].status, "failed");
        assert_eq!(passed(&h), (2, 3));
    }

    /// A Hydra stand-in that answers each request with the next body.
    async fn hydra(bodies: Vec<&'static str>) -> (String, Arc<Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let asked = Arc::new(Mutex::new(Vec::new()));
        let a = asked.clone();
        tokio::spawn(async move {
            let mut bodies = bodies.into_iter();
            while let Ok((mut s, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let line = String::from_utf8_lossy(&buf[..n])
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                a.lock().unwrap().push(line);
                let body = bodies.next().unwrap_or("[]");
                let _ = s
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            }
        });
        (base, asked)
    }

    #[tokio::test]
    async fn the_feed_and_the_sweep_find_a_package_that_turns_red() {
        let app = crate::sets::tests::app(&[]);
        *app.hydra.builds.tracked.write().unwrap() =
            Arc::new(["hello".to_string()].into_iter().collect());
        let (base, asked) = hydra(vec![
            r#"[{"id":5,"job":"hello.x86_64-linux","buildstatus":0},{"id":4,"job":"other.x86_64-linux","buildstatus":1}]"#,
            r#"[{"id":7,"job":"hello.x86_64-linux","buildstatus":1},{"id":5,"job":"hello.x86_64-linux","buildstatus":0}]"#,
            r#"[{"id":9,"job":"hello.x86_64-linux","buildstatus":0}]"#,
            r#"[{"id":9,"job":"hello.x86_64-linux","buildstatus":0},{"id":8,"job":"x.x86_64-linux","buildstatus":0}]"#,
        ])
        .await;
        let http = reqwest::Client::new();
        feed(&app, &http, &base).await.unwrap();
        assert_eq!(app.hydra.builds.state(MASTER, "hello"), Some(State::Passes));
        assert_eq!(app.hydra.builds.state(MASTER, "other"), None, "not tracked");
        feed(&app, &http, &base).await.unwrap();
        assert_eq!(app.hydra.builds.state(MASTER, "hello"), Some(State::Fails));
        let since = Utc::now() - Duration::hours(1);
        let attrs: BTreeSet<String> = ["hello".to_string()].into_iter().collect();
        let ev = events(&app, &attrs, since);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].id, "hydra-unstable-hello-7");
        assert_eq!(
            crate::notify::Kind::of(&ev[0].id),
            crate::notify::Kind::Build
        );
        assert_eq!(ev[0].title, "Hydra build fails");
        assert!(ev[0].url.ends_with("/build/7"));
        assert!(events(&app, &BTreeSet::new(), since).is_empty());
        app.store.set("hydra_feed_max", "8").unwrap();
        feed(&app, &http, &base).await.unwrap();
        assert_eq!(
            app.hydra.builds.state(MASTER, "hello"),
            Some(State::Passes),
            "the 200 rows did not reach back, so the feed asked for 1000"
        );
        let asked = asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 4);
        assert!(asked[0].contains("nr=200&project=nixpkgs&jobset=unstable&system=x86_64-linux"));
        assert!(asked[3].contains("nr=1000"), "{asked:?}");

        *app.hydra.builds.jobsets.write().unwrap() = Arc::new(vec![
            Jobset::master(),
            Jobset::of_channel("nixos-26.05").unwrap(),
        ]);
        let (base, asked) = hydra(vec![
            r#"[{"id":31,"job":"nixpkgs.hello.x86_64-linux","buildstatus":0}]"#,
            r#"[{"id":32,"job":"nixpkgs.hello.x86_64-linux","buildstatus":1}]"#,
        ])
        .await;
        assert!(sweep_one(&app, &http, &base).await.unwrap());
        assert_eq!(app.hydra.builds.state(STABLE, "hello"), Some(State::Passes));
        assert!(
            asked.lock().unwrap()[0]
                .contains("nr=3&project=nixos&jobset=release-26.05&job=nixpkgs.hello.x86_64-linux")
        );
        assert!(
            !sweep_one(&app, &http, &base).await.unwrap(),
            "both jobsets were checked less than a day ago"
        );
        app.store
            .hydra_checked(STABLE, "hello", Utc::now() - Duration::days(2))
            .unwrap();
        assert!(sweep_one(&app, &http, &base).await.unwrap());
        let ev = events(&app, &attrs, since);
        assert_eq!(ev[0].title, "Hydra build fails on nixos-26.05");
        assert_eq!(ev[0].id, "hydra-nixos-26.05-hello-32");
        assert_eq!(
            app.hydra.builds.red_on("hello"),
            Some((State::Fails, "nixos-26.05".into()))
        );

        let (base, _) = hydra(vec![
            r#"[{"id":40,"job":"nixpkgs.hello.x86_64-linux","buildstatus":1,"timestamp":1454105416}]"#,
        ])
        .await;
        app.store
            .hydra_checked(STABLE, "hello", Utc::now() - Duration::days(2))
            .unwrap();
        assert!(sweep_one(&app, &http, &base).await.unwrap());
        assert_eq!(
            app.hydra.builds.state(STABLE, "hello"),
            None,
            "a build from 2016 is a dead job: it forgets the state"
        );
        assert!(
            app.store
                .hydra_states()
                .unwrap()
                .iter()
                .all(|s| s.0 != STABLE)
        );

        app.hydra
            .builds
            .saw("nixos/release-26.05/nixpkgs.hello.x86_64-linux", 33, 0);
        app.hydra
            .builds
            .saw("nixos/release-26.05/nixpkgs.other.x86_64-linux", 34, 1);
        app.hydra
            .builds
            .saw("nixos/trunk-combined/nixos.tests.foo.x86_64-linux", 35, 1);
        assert_eq!(app.hydra.builds.pending.lock().unwrap().len(), 1);
        flush(&app);
        assert_eq!(
            app.hydra.builds.red_on("hello"),
            None,
            "a page view saw it fixed"
        );
    }

    #[test]
    fn a_store_path_gives_its_hash() {
        assert_eq!(
            store_hash("/nix/store/pa6rydwgjxxmmvdxap7qk9c85h8jbimy-hello-2.12.3"),
            Some("pa6rydwgjxxmmvdxap7qk9c85h8jbimy")
        );
        assert_eq!(store_hash("/nix/store/short"), None);
        assert_eq!(store_hash("/tmp/pa6rydwgjxxmmvdxap7qk9c85h8jbimy-x"), None);
    }

    /// A cache stand-in: 200 for the paths of `present`, 404 for others.
    async fn cache(present: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let asked = Arc::new(Mutex::new(Vec::new()));
        let a = asked.clone();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let line = String::from_utf8_lossy(&buf[..n])
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let status = if line.contains(present) {
                    "200 OK"
                } else {
                    "404 Not Found"
                };
                a.lock().unwrap().push(line);
                let _ = s
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await;
            }
        });
        (base, asked)
    }

    #[tokio::test]
    async fn the_cache_check_asks_each_path_once_and_a_missing_one_again_later() {
        let app = crate::sets::tests::app(&[]);
        let built = "/nix/store/pa6rydwgjxxmmvdxap7qk9c85h8jbimy-hello-2.12.3";
        let missing = "/nix/store/32h5slqkza0zwjz2ycvkpwm391vzz68p-python3.14-tornado-6.5.8";
        *app.hydra.builds.tracked.write().unwrap() = Arc::new(
            ["hello", "python3Packages.tornado"]
                .map(String::from)
                .into_iter()
                .collect(),
        );
        *app.channels.write().unwrap() = Arc::new(crate::channels::Channels {
            list: vec![crate::channels::Channel {
                key: "nixos-unstable".into(),
                name: "nixos-unstable".into(),
                rev: "a".repeat(40),
                out_paths: [
                    ("hello".to_string(), built.to_string()),
                    ("python3Packages.tornado".to_string(), missing.to_string()),
                    (
                        "untracked".to_string(),
                        "/nix/store/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-u".to_string(),
                    ),
                ]
                .into_iter()
                .collect(),
                ..Default::default()
            }],
        });
        let (base, asked) = cache("pa6rydwgjxxmmvdxap7qk9c85h8jbimy").await;
        let http = reqwest::Client::new();
        cache_some(&app, &http, &base).await.unwrap();
        assert_eq!(app.hydra.builds.cached(built), Some(true));
        assert_eq!(app.hydra.builds.cached(missing), Some(false));
        let lines = asked.lock().unwrap().clone();
        assert_eq!(lines.len(), 2, "the untracked path is not asked: {lines:?}");
        assert!(
            lines
                .iter()
                .all(|l| l.starts_with("HEAD /") && l.contains(".narinfo"))
        );
        cache_some(&app, &http, &base).await.unwrap();
        assert_eq!(
            asked.lock().unwrap().len(),
            2,
            "nothing is due within the hour"
        );
        app.hydra
            .builds
            .cache
            .write()
            .unwrap()
            .get_mut(missing)
            .unwrap()
            .1 -= Duration::hours(2);
        cache_some(&app, &http, &base).await.unwrap();
        let lines = asked.lock().unwrap().clone();
        assert_eq!(lines.len(), 3);
        assert!(lines[2].contains("32h5slqkza0zwjz2ycvkpwm391vzz68p"));
        let rows = crate::catalog::binary_cache(&app, "hello");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].channel.as_str(), rows[0].cached),
            ("nixos-unstable", Some(true))
        );
        assert!(crate::catalog::binary_cache(&app, "jq").is_empty());
    }
}
