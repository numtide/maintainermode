//! The Hydra build state of the packages that people maintain or watch,
//! and its changes, which become inbox events.
//!
//! Two sources, both small for hydra.nixos.org: a feed of the latest
//! finished builds of `nixpkgs/unstable`, asked every 5 minutes, and a
//! slow sweep that asks one job at a time. The feed is sorted by build id,
//! not by finish time, so a late build of a mass rebuild can fall out of
//! it; the sweep finds it within a day.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
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
pub(crate) const JOB_ROWS: usize = 3;
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

/// One finished build, as `/api/latestbuilds` gives it.
#[derive(Debug, Clone, Deserialize)]
pub struct Row {
    pub id: u64,
    pub job: String,
    pub buildstatus: Option<i64>,
    pub timestamp: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub attr: String,
    pub build: u64,
    pub state: State,
    pub at: DateTime<Utc>,
}

/// The state of each tracked package, in memory, and the builds that page
/// views learned, kept until the background task stores them.
#[derive(Default)]
pub struct Builds {
    states: RwLock<HashMap<String, (u64, State)>>,
    tracked: RwLock<Arc<HashSet<String>>>,
    pending: Mutex<Vec<(String, u64, i64)>>,
    stats: Mutex<Stats>,
}

#[derive(Default, Clone, serde::Serialize)]
struct Stats {
    tracked: usize,
    feed_requests: u64,
    feed_rows: u64,
    sweep_requests: u64,
    failed_requests: u64,
    changes: u64,
    feed_at: Option<DateTime<Utc>>,
}

impl Builds {
    pub fn state(&self, attr: &str) -> Option<State> {
        self.states.read().unwrap().get(attr).map(|(_, s)| *s)
    }

    /// The tracked packages whose last build is red.
    pub fn red(&self, attrs: impl IntoIterator<Item = String>) -> HashMap<String, State> {
        let states = self.states.read().unwrap();
        attrs
            .into_iter()
            .filter_map(|a| {
                let s = states.get(&a)?.1;
                s.red().then_some((a, s))
            })
            .collect()
    }

    /// A page view learned a build; the background task stores it.
    pub fn saw(&self, job: &str, id: u64, code: i64) {
        let Some(attr) = attr_of(job) else { return };
        if !self.tracked.read().unwrap().contains(attr) {
            return;
        }
        let mut p = self.pending.lock().unwrap();
        if p.len() < 1000 {
            p.push((attr.to_string(), id, code));
        }
    }

    pub fn stats(&self) -> serde_json::Value {
        let mut s = self.stats.lock().unwrap().clone();
        s.tracked = self.tracked.read().unwrap().len();
        serde_json::json!({
            "tracked": s.tracked,
            "feed_requests": s.feed_requests,
            "feed_rows": s.feed_rows,
            "sweep_requests": s.sweep_requests,
            "failed_requests": s.failed_requests,
            "changes": s.changes,
            "feed_at": s.feed_at,
            "feed_every_seconds": TICK.as_secs() * u64::from(FEED_EVERY),
            "sweep_per_minute": 60 / TICK.as_secs(),
        })
    }

    /// Records a build of a tracked package. Gives the change, if the state
    /// changed from a known state: the first build seen is only a start.
    fn record(
        &self,
        store: &crate::store::Store,
        attr: &str,
        id: u64,
        code: i64,
        now: DateTime<Utc>,
    ) -> Option<Change> {
        let state = State::of(code);
        let before = self.states.read().unwrap().get(attr).copied();
        if before.is_some_and(|(b, _)| b > id) {
            return None;
        }
        let Some(state) = state else {
            let _ = store.hydra_checked(attr, now);
            return None;
        };
        let change = before
            .filter(|(b, s)| *b < id && *s != state)
            .map(|_| Change {
                attr: attr.to_string(),
                build: id,
                state,
                at: now,
            });
        if let Err(e) = store.put_hydra_state(attr, id, state, now, change.as_ref()) {
            tracing::warn!("hydra state {attr}: {e:#}");
            return None;
        }
        self.states
            .write()
            .unwrap()
            .insert(attr.to_string(), (id, state));
        if change.is_some() {
            self.stats.lock().unwrap().changes += 1;
        }
        change
    }
}

/// `hello.x86_64-linux` gives `hello`; another system gives nothing.
pub(crate) fn attr_of(job: &str) -> Option<&str> {
    let job = job.rsplit_once('/').map_or(job, |(_, j)| j);
    job.strip_suffix(SYSTEM)?.strip_suffix('.')
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

fn latest_url(base: &str, job: Option<&str>, rows: usize) -> String {
    let mut url = format!("{base}/api/latestbuilds?nr={rows}&project=nixpkgs&jobset=unstable");
    match job {
        Some(j) => url.push_str(&format!("&job={}", crate::github::urlencode(j))),
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
    let seen_max: u64 = app
        .store
        .get("hydra_feed_max")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut rows = fetch(http, &latest_url(base, None, FEED_ROWS)).await?;
    let mut requests = 1;
    if !fresh(&rows, seen_max).1 {
        rows = fetch(http, &latest_url(base, None, FEED_ROWS_MORE)).await?;
        requests += 1;
    }
    let tracked = app.hydra.builds.tracked.read().unwrap().clone();
    let now = Utc::now();
    let (new, _) = fresh(&rows, seen_max);
    let mut sorted = new;
    sorted.sort_by_key(|r| r.id);
    for r in &sorted {
        let (Some(attr), Some(code)) = (attr_of(&r.job), r.buildstatus) else {
            continue;
        };
        if tracked.contains(attr) {
            app.hydra.builds.record(&app.store, attr, r.id, code, now);
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
    let now = Utc::now();
    let checked = app.store.hydra_checked_at()?;
    let due = tracked
        .iter()
        .filter(|a| checked.get(*a).is_none_or(|t| now - *t >= SWEEP_AFTER))
        .min_by_key(|a| (checked.get(*a).copied(), (*a).clone()));
    let Some(attr) = due.cloned() else {
        return Ok(false);
    };
    app.hydra.builds.stats.lock().unwrap().sweep_requests += 1;
    let job = format!("{attr}.{SYSTEM}");
    let rows = fetch(http, &latest_url(base, Some(&job), JOB_ROWS)).await?;
    match telling(&rows) {
        Some(r) => {
            app.hydra
                .builds
                .record(&app.store, &attr, r.id, r.buildstatus.unwrap_or(1), now);
        }
        None => app.store.hydra_checked(&attr, now)?,
    }
    Ok(true)
}

fn flush(app: &App) {
    let pending = std::mem::take(&mut *app.hydra.builds.pending.lock().unwrap());
    let now = Utc::now();
    for (attr, id, code) in pending {
        app.hydra.builds.record(&app.store, &attr, id, code, now);
    }
}

fn load(app: &App) {
    match app.store.hydra_states() {
        Ok(list) => {
            let mut states = app.hydra.builds.states.write().unwrap();
            for (attr, build, state) in list {
                states.insert(attr, (build, state));
            }
        }
        Err(e) => tracing::warn!("hydra states: {e:#}"),
    }
}

pub async fn run(app: Arc<App>) {
    run_with(app, crate::health::HYDRA.to_string()).await
}

async fn run_with(app: Arc<App>, base: String) {
    let http = reqwest::Client::builder()
        .user_agent(crate::health::HYDRA_AGENT)
        .timeout(StdDuration::from_secs(30))
        .build()
        .expect("http client");
    load(&app);
    let mut tick: u32 = 0;
    loop {
        if tick.is_multiple_of(TRACK_EVERY) {
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
        tick = tick.wrapping_add(1);
        tokio::time::sleep(TICK).await;
    }
}

fn title(state: State) -> &'static str {
    match state {
        State::Passes => "Hydra build fixed",
        State::Fails => "Hydra build fails",
        State::DependencyFails => "Hydra build fails: a dependency fails",
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
            id: format!("hydra-{}-{}", c.attr, c.build),
            title: title(c.state).to_string(),
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

    fn store() -> Store {
        Store::open(std::path::Path::new(":memory:")).unwrap()
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
    fn a_job_gives_its_attribute_on_this_system_only() {
        assert_eq!(attr_of("hello.x86_64-linux"), Some("hello"));
        assert_eq!(
            attr_of("nixpkgs/unstable/python3Packages.foo.x86_64-linux"),
            Some("python3Packages.foo")
        );
        assert_eq!(attr_of("hello.aarch64-linux"), None);
        assert_eq!(attr_of("unstable"), None);
    }

    #[test]
    fn only_a_change_from_a_known_state_is_a_change() {
        let s = store();
        let b = Builds::default();
        let t = Utc::now();
        assert_eq!(b.record(&s, "a", 10, 0, t), None);
        assert_eq!(b.record(&s, "a", 11, 0, t), None);
        assert_eq!(b.record(&s, "a", 12, 4, t), None);
        assert_eq!(b.state("a"), Some(State::Passes));
        let c = b.record(&s, "a", 13, 1, t).unwrap();
        assert_eq!((c.build, c.state), (13, State::Fails));
        assert_eq!(b.record(&s, "a", 9, 0, t), None, "an older build");
        assert_eq!(b.state("a"), Some(State::Fails));
        assert_eq!(b.record(&s, "a", 13, 1, t), None, "the same build again");
        let c = b.record(&s, "a", 14, 2, t).unwrap();
        assert_eq!(c.state, State::DependencyFails);
        assert_eq!(b.record(&s, "a", 15, 0, t).unwrap().state, State::Passes);
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
            .map(|(a, b, st)| (a, (b, st)))
            .collect();
        assert_eq!(again.state("a"), Some(State::Passes));
    }

    #[test]
    fn the_feed_asks_for_more_only_when_it_does_not_reach_back() {
        let rows = |ids: &[u64]| -> Vec<Row> {
            ids.iter()
                .map(|&id| Row {
                    id,
                    job: "a.x86_64-linux".into(),
                    buildstatus: Some(0),
                    timestamp: None,
                })
                .collect()
        };
        let r = rows(&[30, 20, 10]);
        let (new, reaches) = fresh(&r, 20);
        assert_eq!(new.len(), 1);
        assert!(reaches);
        assert!(!fresh(&r, 5).1);
        assert!(fresh(&r, 0).1, "the first poll only starts");
    }

    #[test]
    fn a_cancelled_last_build_shows_the_one_before() {
        let r = vec![
            Row {
                id: 3,
                job: "a.x86_64-linux".into(),
                buildstatus: Some(4),
                timestamp: None,
            },
            Row {
                id: 2,
                job: "a.x86_64-linux".into(),
                buildstatus: Some(1),
                timestamp: None,
            },
        ];
        assert_eq!(telling(&r).unwrap().id, 2);
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
        assert_eq!(app.hydra.builds.state("hello"), Some(State::Passes));
        assert_eq!(app.hydra.builds.state("other"), None, "not tracked");
        feed(&app, &http, &base).await.unwrap();
        assert_eq!(app.hydra.builds.state("hello"), Some(State::Fails));
        let since = Utc::now() - Duration::hours(1);
        let attrs: BTreeSet<String> = ["hello".to_string()].into_iter().collect();
        let ev = events(&app, &attrs, since);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].id, "hydra-hello-7");
        assert_eq!(ev[0].title, "Hydra build fails");
        assert!(ev[0].url.ends_with("/build/7"));
        assert!(events(&app, &BTreeSet::new(), since).is_empty());
        app.store.set("hydra_feed_max", "8").unwrap();
        feed(&app, &http, &base).await.unwrap();
        assert_eq!(
            app.hydra.builds.state("hello"),
            Some(State::Passes),
            "the 200 rows did not reach back, so the feed asked for 1000"
        );
        let asked = asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 4);
        assert!(asked[0].contains("nr=200&project=nixpkgs&jobset=unstable&system=x86_64-linux"));
        assert!(asked[3].contains("nr=1000"), "{asked:?}");
        let (base, asked) = hydra(vec![
            r#"[{"id":20,"job":"hello.x86_64-linux","buildstatus":4},{"id":19,"job":"hello.x86_64-linux","buildstatus":0}]"#,
        ])
        .await;
        app.store
            .hydra_checked("hello", Utc::now() - Duration::days(2))
            .unwrap();
        assert!(sweep_one(&app, &http, &base).await.unwrap());
        assert_eq!(app.hydra.builds.state("hello"), Some(State::Passes));
        assert!(
            asked.lock().unwrap()[0]
                .contains("nr=3&project=nixpkgs&jobset=unstable&job=hello.x86_64-linux")
        );
        assert!(
            !sweep_one(&app, &http, &base).await.unwrap(),
            "checked less than a day ago"
        );
        let ev = events(&app, &attrs, since);
        assert_eq!(ev[0].title, "Hydra build fixed");
    }
}
