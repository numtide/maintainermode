//! `GET /metrics` in the Prometheus text format 0.0.4, for the operator's
//! Grafana. It has a listener of its own: behind the reverse proxy every
//! request on the public port comes from 127.0.0.1, so that port cannot
//! tell the operator from a visitor.

use axum::Router;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::DateTime;
use serde_json::Value;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::web::{App, Shared};

const CONTENT_TYPE: &str = "text/plain; version=0.0.4";
const COMMIT: &str = match option_env!("MAINTAINER_MODE_REV") {
    Some(r) => r,
    None => "unknown",
};
const BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];
const METHODS: [&str; 9] = [
    "GET", "HEAD", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "CONNECT", "TRACE",
];
/// The keys that name an element of an array in `/api/v1/sync`, in order.
const ID_KEYS: [&str; 5] = ["repo", "channel", "source", "name", "id"];

#[derive(Default, Clone)]
struct Histogram {
    /// Per bucket, not cumulative; the last one is above the largest bound.
    counts: [u64; BUCKETS.len() + 1],
    sum: f64,
}

impl Histogram {
    fn observe(&mut self, secs: f64) {
        let i = BUCKETS
            .iter()
            .position(|b| secs <= *b)
            .unwrap_or(BUCKETS.len());
        self.counts[i] += 1;
        self.sum += secs;
    }

    fn write(&self, out: &mut String, name: &str, labels: &[(&str, &str)]) {
        let mut total = 0;
        for (i, n) in self.counts.iter().enumerate() {
            total += n;
            let le = BUCKETS.get(i).map_or("+Inf".into(), |b| number(*b));
            let mut with = labels.to_vec();
            with.push(("le", le.as_str()));
            sample(out, &format!("{name}_bucket"), &with, total as f64);
        }
        sample(out, &format!("{name}_sum"), labels, self.sum);
        sample(out, &format!("{name}_count"), labels, total as f64);
    }
}

struct Http {
    /// Route template, method, status.
    requests: BTreeMap<(String, &'static str, u16), u64>,
    durations: BTreeMap<String, Histogram>,
}

static HTTP: Mutex<Http> = Mutex::new(Http {
    requests: BTreeMap::new(),
    durations: BTreeMap::new(),
});
static SIGNED_IN: AtomicU64 = AtomicU64::new(0);
static EVENTS_INSERTED: AtomicU64 = AtomicU64::new(0);
static EVENTS_LAST_INSERT_MS: AtomicU64 = AtomicU64::new(0);
static START: OnceLock<f64> = OnceLock::new();

struct Timer {
    total: AtomicU64,
    /// The bits of an f64.
    last: AtomicU64,
}

impl Timer {
    const fn new() -> Self {
        Self {
            total: AtomicU64::new(0),
            last: AtomicU64::new(0),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Recompute {
    All,
    Packages,
    Metrics,
}

static RECOMPUTE: [Timer; 3] = [Timer::new(), Timer::new(), Timer::new()];

struct Github {
    /// A hash of the current token of each kind; never the token.
    names: BTreeMap<&'static str, u64>,
    /// Remaining points and reset time, as of the last answer.
    rate: BTreeMap<&'static str, (i64, i64)>,
    /// API and status; no status when the request got no answer.
    requests: BTreeMap<(&'static str, Option<u16>), u64>,
}

static GITHUB: Mutex<Github> = Mutex::new(Github {
    names: BTreeMap::new(),
    rate: BTreeMap::new(),
    requests: BTreeMap::new(),
});

tokio::task_local! {
    static SESSION: Cell<bool>;
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub(crate) fn started() {
    START.get_or_init(unix_now);
}

/// Called where a session cookie is found valid; the request in flight
/// counts once, however often its handler asks.
pub(crate) fn saw_session() {
    let _ = SESSION.try_with(|s| s.set(true));
}

pub(crate) fn events_added(n: usize) {
    EVENTS_INSERTED.fetch_add(n as u64, Relaxed);
    EVENTS_LAST_INSERT_MS.store((unix_now() * 1000.0) as u64, Relaxed);
}

pub(crate) fn recomputed(what: Recompute, took: Duration) {
    let t = &RECOMPUTE[what as usize];
    t.total.fetch_add(1, Relaxed);
    t.last.store(took.as_secs_f64().to_bits(), Relaxed);
}

fn token_hash(token: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    token.hash(&mut h);
    h.finish()
}

/// Lets the budget of `token` show as `kind`. A token with no name, such
/// as a lent one, counts in the requests only.
pub(crate) fn name_token(kind: &'static str, token: &str) {
    GITHUB.lock().unwrap().names.insert(kind, token_hash(token));
}

/// One answer of the GitHub GraphQL API, or `None` when none came.
pub(crate) fn graphql_answer(token: &str, resp: Option<&reqwest::Response>) {
    let header = |r: &reqwest::Response, n: &str| -> Option<i64> {
        r.headers().get(n)?.to_str().ok()?.parse().ok()
    };
    let rate = resp.and_then(|r| {
        Some((
            header(r, "x-ratelimit-remaining")?,
            header(r, "x-ratelimit-reset")?,
        ))
    });
    let hash = token_hash(token);
    let mut g = GITHUB.lock().unwrap();
    *g.requests
        .entry(("graphql", resp.map(|r| r.status().as_u16())))
        .or_default() += 1;
    let kind = g.names.iter().find(|(_, h)| **h == hash).map(|(k, _)| *k);
    if let (Some(kind), Some(rate)) = (kind, rate) {
        g.rate.insert(kind, rate);
    }
}

fn method(m: &axum::http::Method) -> &'static str {
    METHODS
        .iter()
        .copied()
        .find(|x| *x == m.as_str())
        .unwrap_or("other")
}

/// The layer of the main router that counts and times each request.
pub(crate) async fn track(req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", |p| p.as_str())
        .to_string();
    let method = method(req.method());
    let start = Instant::now();
    let (res, signed_in) = SESSION
        .scope(Cell::new(false), async move {
            let res = next.run(req).await;
            (res, SESSION.with(Cell::get))
        })
        .await;
    if signed_in {
        SIGNED_IN.fetch_add(1, Relaxed);
    }
    let took = start.elapsed().as_secs_f64();
    let mut h = HTTP.lock().unwrap();
    *h.requests
        .entry((route.clone(), method, res.status().as_u16()))
        .or_default() += 1;
    h.durations.entry(route).or_default().observe(took);
    res
}

fn escape(v: &str) -> String {
    let mut s = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' => s.push_str("\\\\"),
            '"' => s.push_str("\\\""),
            '\n' => s.push_str("\\n"),
            c => s.push(c),
        }
    }
    s
}

fn number(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v == f64::INFINITY {
        "+Inf".into()
    } else if v == f64::NEG_INFINITY {
        "-Inf".into()
    } else {
        format!("{v}")
    }
}

fn sample<K: AsRef<str>, V: AsRef<str>>(
    out: &mut String,
    name: &str,
    labels: &[(K, V)],
    value: f64,
) {
    out.push_str(name);
    if !labels.is_empty() {
        out.push('{');
        for (i, (k, v)) in labels.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(out, "{}=\"{}\"", k.as_ref(), escape(v.as_ref()));
        }
        out.push('}');
    }
    let _ = writeln!(out, " {}", number(value));
}

fn family(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

fn single(out: &mut String, name: &str, kind: &str, help: &str, value: f64) {
    family(out, name, kind, help);
    sample::<&str, &str>(out, name, &[], value);
}

type Labels = Vec<(String, String)>;
type Gauges = BTreeMap<String, Vec<(Labels, f64)>>;

fn clean(key: &str) -> String {
    key.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn free_label(labels: &Labels, key: &str) -> String {
    let mut k = key.to_string();
    let mut n = 1;
    while labels.iter().any(|(l, _)| *l == k) {
        n += 1;
        k = format!("{key}{n}");
    }
    k
}

fn walk(v: &Value, name: &str, labels: &mut Labels, id: Option<&str>, out: &mut Gauges) {
    let mut add = |name: String, value: f64| {
        out.entry(name).or_default().push((labels.clone(), value));
    };
    match v {
        Value::Number(n) => add(name.into(), n.as_f64().unwrap_or(f64::NAN)),
        Value::Bool(b) => add(name.into(), if *b { 1.0 } else { 0.0 }),
        Value::String(s) => {
            if let Ok(t) = DateTime::parse_from_rfc3339(s) {
                add(
                    format!("{name}_timestamp_seconds"),
                    t.timestamp_millis() as f64 / 1000.0,
                );
            }
        }
        Value::Null => {}
        Value::Object(m) => {
            for (k, x) in m.iter().filter(|(k, _)| Some(k.as_str()) != id) {
                walk(x, &format!("{name}_{}", clean(k)), labels, None, out);
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                let Value::Object(m) = x else {
                    continue;
                };
                let key = ID_KEYS.iter().copied().find_map(|k| match m.get(k)? {
                    Value::String(s) => Some((k, s.clone())),
                    Value::Number(n) => Some((k, n.to_string())),
                    _ => None,
                });
                let (label, value) = key.clone().unwrap_or(("i", i.to_string()));
                labels.push((free_label(labels, label), value));
                walk(x, name, labels, key.map(|(k, _)| k), out);
                labels.pop();
            }
        }
    }
}

/// The JSON of `/api/v1/sync` as gauges: see docs/operations.md#metrics.
fn flatten(state: &Value) -> Gauges {
    let mut out = Gauges::new();
    walk(
        state,
        "maintainer_mode_sync",
        &mut Vec::new(),
        None,
        &mut out,
    );
    out
}

fn write_gauges(out: &mut String, gauges: &Gauges) {
    for (name, samples) in gauges {
        let _ = writeln!(out, "# TYPE {name} gauge");
        let mut seen: Vec<&Labels> = Vec::new();
        for (labels, value) in samples {
            if !seen.contains(&labels) {
                seen.push(labels);
                sample(out, name, labels, *value);
            }
        }
    }
}

fn resident_bytes() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb: f64 = s
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .trim()
        .strip_suffix("kB")?
        .trim()
        .parse()
        .ok()?;
    Some(kb * 1024.0)
}

fn open_fds() -> Option<f64> {
    Some(std::fs::read_dir("/proc/self/fd").ok()?.count() as f64)
}

fn write_http(out: &mut String) {
    let (requests, durations) = {
        let h = HTTP.lock().unwrap();
        (h.requests.clone(), h.durations.clone())
    };
    family(
        out,
        "maintainer_mode_http_requests_total",
        "counter",
        "Requests to the main port, by route template, method and status.",
    );
    for ((route, method, status), n) in &requests {
        let status = status.to_string();
        let labels = [
            ("route", route.as_str()),
            ("method", *method),
            ("status", status.as_str()),
        ];
        sample(
            out,
            "maintainer_mode_http_requests_total",
            &labels,
            *n as f64,
        );
    }
    let name = "maintainer_mode_http_request_duration_seconds";
    family(
        out,
        name,
        "histogram",
        "Time to the response head, by route template.",
    );
    for (route, h) in &durations {
        h.write(out, name, &[("route", route.as_str())]);
    }
    single(
        out,
        "maintainer_mode_http_signed_in_requests_total",
        "counter",
        "Requests whose handler found a valid session.",
        SIGNED_IN.load(Relaxed) as f64,
    );
}

fn write_github(out: &mut String) {
    let (rate, requests) = {
        let g = GITHUB.lock().unwrap();
        (g.rate.clone(), g.requests.clone())
    };
    let remaining = "maintainer_mode_github_rate_remaining";
    family(
        out,
        remaining,
        "gauge",
        "GraphQL points left on a server token, as of its last answer.",
    );
    for (kind, (left, _)) in &rate {
        sample(out, remaining, &[("token", kind)], *left as f64);
    }
    let reset = "maintainer_mode_github_rate_reset_timestamp_seconds";
    family(
        out,
        reset,
        "gauge",
        "When the GraphQL budget of a server token resets.",
    );
    for (kind, (_, at)) in &rate {
        sample(out, reset, &[("token", kind)], *at as f64);
    }
    let name = "maintainer_mode_github_requests_total";
    family(
        out,
        name,
        "counter",
        "Requests to the GitHub GraphQL API, by status; error is no answer.",
    );
    for ((kind, status), n) in &requests {
        let status = status.map_or("error".into(), |s| s.to_string());
        sample(
            out,
            name,
            &[("kind", *kind), ("status", status.as_str())],
            *n as f64,
        );
    }
}

pub(crate) fn render(app: &App) -> String {
    let mut out = String::with_capacity(32 * 1024);
    family(
        &mut out,
        "maintainer_mode_build_info",
        "gauge",
        "The version and git commit of the running build.",
    );
    sample(
        &mut out,
        "maintainer_mode_build_info",
        &[("version", env!("CARGO_PKG_VERSION")), ("commit", COMMIT)],
        1.0,
    );
    if let Some(b) = resident_bytes() {
        single(
            &mut out,
            "maintainer_mode_process_resident_memory_bytes",
            "gauge",
            "Resident memory of the server process.",
            b,
        );
    }
    if let Some(n) = open_fds() {
        single(
            &mut out,
            "maintainer_mode_process_open_fds",
            "gauge",
            "Open file descriptors of the server process.",
            n,
        );
    }
    single(
        &mut out,
        "maintainer_mode_process_start_time_seconds",
        "gauge",
        "When the server process started, in unix time.",
        *START.get_or_init(unix_now),
    );
    write_http(&mut out);
    single(
        &mut out,
        "maintainer_mode_events_inserted_total",
        "counter",
        "Events added to the event log.",
        EVENTS_INSERTED.load(Relaxed) as f64,
    );
    single(
        &mut out,
        "maintainer_mode_events_last_insert_timestamp_seconds",
        "gauge",
        "When an insert last added an event, or the start of the process before the first one.",
        match EVENTS_LAST_INSERT_MS.load(Relaxed) {
            0 => *START.get_or_init(unix_now),
            ms => ms as f64 / 1000.0,
        },
    );
    for (what, prefix) in [
        (Recompute::All, "maintainer_mode_recompute"),
        (Recompute::Packages, "maintainer_mode_recompute_packages"),
        (Recompute::Metrics, "maintainer_mode_recompute_metrics"),
    ] {
        let t = &RECOMPUTE[what as usize];
        single(
            &mut out,
            &format!("{prefix}_duration_seconds"),
            "gauge",
            "Wall time of the last finished run.",
            f64::from_bits(t.last.load(Relaxed)),
        );
        single(
            &mut out,
            &format!("{prefix}_total"),
            "counter",
            "Finished runs.",
            t.total.load(Relaxed) as f64,
        );
    }
    write_github(&mut out);
    write_gauges(&mut out, &flatten(&crate::status::state(app)));
    out
}

async fn metrics(State(app): State<Shared>) -> Response {
    match tokio::task::spawn_blocking(move || render(&app)).await {
        Ok(body) => ([(header::CONTENT_TYPE, CONTENT_TYPE)], body).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// The router of the metrics listener: `/metrics` and nothing else.
pub(crate) fn router(app: Shared) -> Router {
    Router::new()
        .route("/metrics", get(metrics))
        .with_state(app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text(state: Value) -> String {
        let mut out = String::new();
        write_gauges(&mut out, &flatten(&state));
        out
    }

    #[test]
    fn sync_json_becomes_gauges() {
        let out = text(json!({
            "archive": [{
                "repo": "NixOS/nixpkgs",
                "items": 577377,
                "percent": 99.5,
                "last_error": null,
                "git": { "bytes": 5125038080u64, "fetched_at": "2026-10-25T21:28:34.740309138+00:00" },
            }],
            "events": { "polling": true, "newest": "2026-10-25T21:29:06Z" },
            "health": { "sources": { "osv-records": { "ok": false, "line": "0 records read" } } },
            "index": { "rev": "96daf2fb", "packages": 36929 },
            "hydra_builds": { "jobsets": ["nixpkgs/unstable"] },
        }));
        for line in [
            "# TYPE maintainer_mode_sync_archive_items gauge",
            "maintainer_mode_sync_archive_items{repo=\"NixOS/nixpkgs\"} 577377",
            "maintainer_mode_sync_archive_percent{repo=\"NixOS/nixpkgs\"} 99.5",
            "maintainer_mode_sync_archive_git_bytes{repo=\"NixOS/nixpkgs\"} 5125038080",
            "maintainer_mode_sync_archive_git_fetched_at_timestamp_seconds{repo=\"NixOS/nixpkgs\"} 1792963714.74",
            "maintainer_mode_sync_events_polling 1",
            "maintainer_mode_sync_events_newest_timestamp_seconds 1792963746",
            "maintainer_mode_sync_health_sources_osv_records_ok 0",
            "maintainer_mode_sync_index_packages 36929",
        ] {
            assert!(out.lines().any(|l| l == line), "no {line:?} in\n{out}");
        }
        for gone in ["_repo", "last_error", "_line", "_rev", "jobsets"] {
            assert!(!out.contains(gone), "{gone} in\n{out}");
        }
        assert_eq!(out.matches("# TYPE").count(), out.lines().count() / 2);
    }

    #[test]
    fn array_elements_get_a_label_once_and_escaped() {
        let out = text(json!({
            "channels": { "list": [
                { "channel": "a\"b\\c\nd", "attributes": 1 },
                { "channel": "a\"b\\c\nd", "attributes": 2 },
                { "attributes": 3, "read_at": "nope" },
                7,
            ] },
            "outer": [{ "name": "x", "inner": [{ "name": "y", "n": 1 }, { "id": 4, "n": 2 }] }],
        }));
        assert_eq!(
            out,
            "# TYPE maintainer_mode_sync_channels_list_attributes gauge\n\
             maintainer_mode_sync_channels_list_attributes{channel=\"a\\\"b\\\\c\\nd\"} 1\n\
             maintainer_mode_sync_channels_list_attributes{i=\"2\"} 3\n\
             # TYPE maintainer_mode_sync_outer_inner_n gauge\n\
             maintainer_mode_sync_outer_inner_n{name=\"x\",name2=\"y\"} 1\n\
             maintainer_mode_sync_outer_inner_n{name=\"x\",id=\"4\"} 2\n"
        );
    }

    #[test]
    fn a_histogram_is_cumulative() {
        let mut h = Histogram::default();
        for s in [0.00390625, 0.375, 0.375, 42.0] {
            h.observe(s);
        }
        let mut out = String::new();
        h.write(&mut out, "x", &[("route", "/pr/:number")]);
        let want = [
            ("0.005", 1),
            ("0.01", 1),
            ("0.025", 1),
            ("0.05", 1),
            ("0.1", 1),
            ("0.25", 1),
            ("0.5", 3),
            ("1", 3),
            ("2.5", 3),
            ("5", 3),
            ("10", 3),
            ("+Inf", 4),
        ]
        .iter()
        .map(|(le, n)| format!("x_bucket{{route=\"/pr/:number\",le=\"{le}\"}} {n}\n"))
        .collect::<String>()
            + "x_sum{route=\"/pr/:number\"} 42.75390625\nx_count{route=\"/pr/:number\"} 4\n";
        assert_eq!(out, want);
    }

    #[test]
    fn only_a_named_token_shows_its_budget() {
        let answer = |remaining: &str| {
            reqwest::Response::from(
                axum::http::Response::builder()
                    .header("x-ratelimit-remaining", remaining)
                    .header("x-ratelimit-reset", "1792963800")
                    .body("")
                    .unwrap(),
            )
        };
        name_token("bot", "test-bot-token");
        graphql_answer("test-bot-token", Some(&answer("4000")));
        graphql_answer("a-lent-token", Some(&answer("12")));
        graphql_answer("a-lent-token", None);
        let mut out = String::new();
        write_github(&mut out);
        for line in [
            "maintainer_mode_github_rate_remaining{token=\"bot\"} 4000",
            "maintainer_mode_github_rate_reset_timestamp_seconds{token=\"bot\"} 1792963800",
        ] {
            assert!(out.lines().any(|l| l == line), "no {line:?} in\n{out}");
        }
        assert!(
            out.lines()
                .filter(|l| l.contains("token="))
                .all(|l| l.contains("token=\"bot\"") || l.contains("token=\"app\"")),
            "{out}"
        );
        let has = |start: &str| out.lines().any(|l| l.starts_with(start));
        assert!(has(
            "maintainer_mode_github_requests_total{kind=\"graphql\",status=\"200\"} "
        ));
        assert!(has(
            "maintainer_mode_github_requests_total{kind=\"graphql\",status=\"error\"} "
        ));
    }

    #[tokio::test]
    async fn metrics_listener_counts_the_main_router() {
        let app = crate::sets::tests::app(&[]);
        let site = crate::e2e::serve(app.clone()).await;
        let main = |path: &str| site.client.get(format!("{}{path}", site.base)).send();
        assert_eq!(main("/healthz").await.unwrap().status(), 200);
        assert_eq!(main("/metrics").await.unwrap().status(), 404);
        app.store
            .create_session("telemetry-session", "alice", "dummy-token")
            .unwrap();
        let before = SIGNED_IN.load(Relaxed);
        site.get("/", Some("telemetry-session")).await;
        assert!(SIGNED_IN.load(Relaxed) > before);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(app)).await.unwrap() });
        let r = site
            .client
            .get(format!("{base}/metrics"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()[header::CONTENT_TYPE], CONTENT_TYPE);
        let body = r.text().await.unwrap();
        let has = |start: &str| body.lines().any(|l| l.starts_with(start));
        assert!(has("maintainer_mode_build_info{version=\""), "{body}");
        assert!(
            has(
                "maintainer_mode_http_requests_total{route=\"/healthz\",method=\"GET\",status=\"200\"} "
            ),
            "{body}"
        );
        assert!(
            has(
                "maintainer_mode_http_requests_total{route=\"unmatched\",method=\"GET\",status=\"404\"} "
            ),
            "{body}"
        );
        assert!(has(
            "maintainer_mode_http_request_duration_seconds_count{route=\"/healthz\"} "
        ));
        assert!(has("maintainer_mode_sync_pull_requests_stored 0"), "{body}");
        let other = site.client.get(format!("{base}/healthz")).send();
        assert_eq!(other.await.unwrap().status(), 404);
    }
}
