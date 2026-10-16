use anyhow::Result;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::{Arc, RwLock};

use crate::area::{self, Area, Index, Ownership};
use crate::dossier::{self, Outcome};
use crate::github::OAuth;
use crate::ledger::{self, Dropped, Ledger, Trigger};
use crate::metrics::{self, Report};
use crate::queue::{self, Entry, Queues};
use crate::store::Store;

const PRIMER_CSS: &str = include_str!(concat!(env!("PRIMER_DIR"), "/primer.css"));
const CLUB_CSS: &str = include_str!("club.css");
const RULES: &str = include_str!("../docs/rules.md");

macro_rules! octicon {
    ($name:literal) => {
        include_str!(concat!(env!("PRIMER_DIR"), "/octicons/", $name, "-16.svg"))
    };
}
pub(crate) use octicon;

pub(crate) fn icon(svg: &str) -> String {
    svg.replacen(
        "<svg ",
        "<svg class=\"octicon\" fill=\"currentColor\" aria-hidden=\"true\" ",
        1,
    )
}

#[derive(Default)]
pub struct Snapshot {
    pub ledger: Ledger,
    pub queues: Queues,
    pub areas: HashMap<String, Area>,
    pub prs: usize,
    pub index_rev: String,
    pub pr_data: Vec<crate::model::Pr>,
    pub issues: Vec<crate::model::Issue>,
    pub links: crate::links::Links,
    pub pr_at: HashMap<u64, usize>,
    /// login → positions in `pr_data` of the pull requests that login reviewed.
    pub reviewed_by: HashMap<String, Vec<usize>>,
    pub issue_at: HashMap<u64, usize>,
    /// Every open issue has been read once, so an issue missing here is closed.
    pub issues_complete: bool,
    /// directory → open pull requests that change something under it.
    pub open_by_dir: HashMap<String, usize>,
    /// pname → every version nixpkgs carries of it, under any attribute.
    pub versions: HashMap<String, std::collections::HashSet<String>>,
    pub committers: crate::committer::Committers,
    pub board: crate::committer::Board,
}

pub struct App {
    pub store: Store,
    pub oauth: Option<OAuth>,
    pub sync_token: Option<String>,
    pub github_app: Option<crate::github_app::GithubApp>,
    pub index: RwLock<Index>,
    pub snapshot: RwLock<Arc<Snapshot>>,
    pub secure_cookies: bool,
    pub health: RwLock<Arc<crate::health::Health>>,
    /// nixpkgs channels next to master, in the configured order.
    pub channels: RwLock<Arc<crate::channels::Channels>>,
    pub hydra: crate::health::Hydra,
    /// Where the nixpkgs source is unpacked for the evaluator.
    pub data_dir: std::path::PathBuf,
    pub index_sets: Vec<String>,
    pub metrics: RwLock<Arc<Report>>,
    pub(crate) packages: RwLock<Arc<crate::catalog::PackageStats>>,
    /// The URL people reach, for links that leave the site, like feeds.
    pub public_url: String,
    pub events: Arc<crate::events::Log>,
    pub archive: Vec<Arc<crate::archive::Archive>>,
    /// Server-side flake rescans; None while they are off.
    pub rescan: Option<crate::rescan::Config>,
}

impl App {
    pub fn recompute(&self) -> Result<()> {
        let prs = self.store.prs()?;
        let index = self.index.read().unwrap();
        let own = Ownership::new(&index);
        let ledger = ledger::compute(&prs, |p| own.orphan(p), Utc::now());
        let areas: HashMap<String, Area> = self
            .store
            .members()?
            .iter()
            .map(|m| (m.login.clone(), Area::of(m, &index)))
            .collect();
        let member_areas: Vec<Area> = areas.values().cloned().collect();
        let queues = queue::build(&prs, &ledger, &member_areas);
        let issues = self.store.issues()?;
        let links = crate::links::build(&index, &prs, &issues);
        let pr_at = prs.iter().enumerate().map(|(i, p)| (p.number, i)).collect();
        let reviewed_by = dossier::by_reviewer(&prs);
        let issue_at = issues
            .iter()
            .enumerate()
            .map(|(i, p)| (p.number, i))
            .collect();
        let open_by_dir = crate::catalog::open_by_dir(&prs);
        let mut versions: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
        for p in index.pkgs.values() {
            versions
                .entry(p.pname.clone())
                .or_default()
                .insert(p.version.clone());
        }
        let committers = crate::committer::Committers::load(&self.store, &prs, Utc::now());
        let board = crate::committer::board(&prs, &ledger, &index, &links);
        *self.snapshot.write().unwrap() = Arc::new(Snapshot {
            ledger,
            queues,
            areas,
            prs: prs.len(),
            index_rev: index.rev.clone(),
            pr_data: prs,
            issues,
            links,
            pr_at,
            reviewed_by,
            issue_at,
            issues_complete: self.store.get("issues_open_done")?.is_some(),
            open_by_dir,
            versions,
            committers,
            board,
        });
        drop(index);
        self.recompute_packages();
        Ok(())
    }

    /// Walks every package and every open pull request, so it runs when the
    /// index, the pull requests or the health data change, not on a request.
    pub fn recompute_packages(&self) {
        let s = self.snap();
        let h = self.health.read().unwrap().clone();
        let p = crate::catalog::package_stats(&s, &self.index.read().unwrap(), &h);
        *self.packages.write().unwrap() = Arc::new(p);
    }

    /// Reads every stored pull request, so it runs after a sync pass rather
    /// than on each request.
    pub fn recompute_metrics(&self) -> Result<()> {
        let facts = metrics::merge(self.store.facts()?, &self.snap().pr_data);
        *self.metrics.write().unwrap() = Arc::new(metrics::compute(&facts, Utc::now()));
        Ok(())
    }

    /// Everything a ready pull request needs except a known `mergeable`.
    pub fn merge_candidates(&self) -> Vec<u64> {
        self.snap()
            .queues
            .review
            .iter()
            .filter(|e| {
                e.mergeable == "UNKNOWN"
                    && e.ci.as_deref() == Some("SUCCESS")
                    && !e.approved_by.is_empty()
                    && !e.credited_by.is_empty()
            })
            .map(|e| e.number)
            .collect()
    }

    pub(crate) fn snap(&self) -> Arc<Snapshot> {
        self.snapshot.read().unwrap().clone()
    }

    pub(crate) fn viewer(&self, headers: &HeaderMap) -> Option<String> {
        let id = cookie(headers, "session")?;
        self.store.session(&id).ok().flatten().map(|s| s.login)
    }
}

pub(crate) type Shared = Arc<App>;

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/", get(merge_page))
        .route("/review", get(review_page))
        .route("/record", get(record_page))
        .route("/rules", get(rules_page))
        .route("/u/:login", get(person_page))
        .route("/me/tags", post(set_tags))
        .route("/auth/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/auth/logout", post(logout))
        .route("/static/primer.css", get(|| css(PRIMER_CSS)))
        .route("/static/club.css", get(|| css(CLUB_CSS)))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/v1/status", get(api_status))
        .route("/api/v1/queue/merge", get(api_merge))
        .route("/api/v1/queue/review", get(api_review))
        .route("/api/v1/people/:login", get(api_person))
        .route("/api/v1/metrics", get(api_metrics))
        .route("/api/v1/events", get(api_events))
        .route("/state", get(state_page))
        .merge(crate::catalog::routes())
        .merge(crate::bumps::routes())
        .merge(crate::committer::routes())
        .merge(crate::actions::routes())
        .merge(crate::status::routes())
        .merge(crate::sets::routes())
        .merge(crate::archive::routes())
        .merge(crate::llm::routes())
        .merge(crate::progress::routes())
        .merge(crate::mcp::routes())
        .merge(crate::rescan::routes())
        .merge(crate::welcome::routes())
        .layer(axum::middleware::from_fn(same_origin))
        .with_state(app)
}

pub(crate) fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

pub(crate) fn set_cookie(app: &App, name: &str, value: &str, max_age: i64) -> String {
    let secure = if app.secure_cookies { "; Secure" } else { "" };
    format!("{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")
}

pub(crate) fn random_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(crate) fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Coarser as it grows: nobody needs the day of something five years old.
pub(crate) fn ago(t: DateTime<Utc>) -> String {
    let d = Utc::now() - t;
    let days = d.num_days();
    if d.num_minutes() < 60 {
        format!("{}m", d.num_minutes().max(0))
    } else if d.num_hours() < 24 {
        format!("{}h", d.num_hours())
    } else if days < 14 {
        format!("{days}d")
    } else if days < 60 {
        format!("{}w", days / 7)
    } else if days < 730 {
        format!("{}mo", days / 30)
    } else {
        format!("{}y", days / 365)
    }
}

async fn css(body: &'static str) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        body,
    )
}

pub(crate) fn page(app: &App, viewer: Option<&str>, tab: &str, body: &str) -> Html<String> {
    let s = app.snap();
    let watched = viewer
        .and_then(|v| app.store.watched(v).ok())
        .map(|w| w.len().to_string())
        .unwrap_or_default();
    let mut tabs = vec![
        (
            "merge",
            "/",
            icon(octicon!("git-merge")),
            "Ready to merge",
            s.queues.merge.len().to_string(),
        ),
        (
            "review",
            "/review",
            icon(octicon!("eye")),
            "Needs review",
            s.queues.review.len().to_string(),
        ),
        (
            "packages",
            "/packages",
            icon(octicon!("package")),
            "Packages",
            String::new(),
        ),
        (
            "watch",
            "/watch",
            icon(octicon!("bell")),
            "Watching",
            watched,
        ),
        (
            "record",
            "/record",
            icon(octicon!("law")),
            "Review record",
            String::new(),
        ),
        (
            "state",
            "/state",
            icon(octicon!("graph")),
            "State of nixpkgs",
            String::new(),
        ),
        (
            "actions",
            "/actions",
            icon(octicon!("play")),
            "Actions",
            String::new(),
        ),
    ];
    if viewer.is_some_and(|v| s.committers.is(v)) {
        tabs.insert(
            1,
            (
                "committer",
                "/committer",
                icon(octicon!("key")),
                "Committer",
                String::new(),
            ),
        );
    }
    let mut nav = String::new();
    for (id, href, ic, label, n) in tabs {
        let current = if id == tab {
            " aria-current=\"page\""
        } else {
            ""
        };
        let counter = if n.is_empty() {
            String::new()
        } else {
            format!("<span class=\"Counter\">{n}</span>")
        };
        let _ = write!(
            nav,
            "<li class=\"d-inline-flex\"><a class=\"UnderlineNav-item\" href=\"{href}\"{current}>{ic}<span>{label}</span>{counter}</a></li>"
        );
    }
    let who = match viewer {
        Some(v) => format!(
            "{}<a class=\"Header-link mr-3\" href=\"/u/{v}\">{} {v}</a>\
             <a class=\"Header-link mr-3\" href=\"/me/progress\">{} Progress</a>\
             <a class=\"Header-link mr-3\" href=\"/me/settings\">{} Settings</a>\
             <form method=\"post\" action=\"/auth/logout\" class=\"d-inline\"><button class=\"btn btn-sm\" type=\"submit\">{} Sign out</button></form>",
            if crate::welcome::pending(app, v) {
                "<a class=\"Header-link mr-3\" href=\"/welcome\">Get started</a>"
            } else {
                ""
            },
            icon(octicon!("person")),
            icon(octicon!("flame")),
            icon(octicon!("gear")),
            icon(octicon!("sign-out")),
            v = esc(v)
        ),
        None if app.oauth.is_some() => format!(
            "<a class=\"btn btn-sm btn-primary\" href=\"/auth/login\">{} Sign in with GitHub</a>",
            icon(octicon!("sign-in"))
        ),
        None => String::new(),
    };
    Html(format!(
        r#"<!doctype html>
<html lang="en" data-color-mode="auto" data-light-theme="light" data-dark-theme="dark">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Maintainer Mode</title>
<link rel="stylesheet" href="/static/primer.css">
<link rel="stylesheet" href="/static/club.css">
</head>
<body class="color-bg-default color-fg-default">
<header class="Header">
  <div class="Header-item Header-item--full"><a class="Header-link f4 d-flex flex-items-center" href="/">{club} <span class="ml-2">Maintainer Mode</span></a></div>
  <div class="Header-item">{who}</div>
</header>
<nav class="UnderlineNav px-3" aria-label="Queues"><ul class="UnderlineNav-body list-style-none">{nav}</ul></nav>
<main class="container-lg px-3 py-4">{body}</main>
<footer class="container-lg px-3 py-4 f6 color-fg-muted border-top">
  {prs} pull requests synced · nixpkgs index {rev} · <a href="{CONTRIBUTING}">nixpkgs contributor guide</a> · <a href="/rules#the-rules">the rules</a> · <a href="/sync">sync status</a> · <a href="/api/v1/status">API</a><br>
  A personal tool for nixpkgs maintenance, run in public. Not an official NixOS project, and not made by or affiliated with GitHub. Styled with Primer and Octicons (MIT), in the NixOS brand colors.{nvd}
</footer>
</body>
</html>"#,
        club = icon(octicon!("people")),
        nvd = if app.store.get("nvd_api").ok().flatten().is_some() {
            "<br>This product uses the NVD API but is not endorsed or certified by the NVD."
        } else {
            ""
        },
        prs = s.prs,
        rev = esc(if s.index_rev.is_empty() {
            "pending"
        } else {
            &s.index_rev
        }),
    ))
}

pub(crate) const CONTRIBUTING: &str =
    "https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md";

/// Maintainer Mode finds the work; these say how nixpkgs wants it done.
pub(crate) fn nixpkgs_guides() -> String {
    guide_box()
}

pub(crate) fn ci_label(e: &Entry) -> String {
    match e.ci.as_deref() {
        Some("SUCCESS") => format!(
            "<span class=\"color-fg-success\" title=\"checks pass\">{}</span>",
            icon(octicon!("check-circle-fill"))
        ),
        Some("FAILURE" | "ERROR") => format!(
            "<span class=\"color-fg-danger\" title=\"checks fail\">{}</span>",
            icon(octicon!("x-circle-fill"))
        ),
        _ => format!(
            "<span class=\"color-fg-attention\" title=\"checks pending\">{}</span>",
            icon(octicon!("dot-fill"))
        ),
    }
}

fn people(logins: &[String]) -> String {
    logins
        .iter()
        .map(|l| format!("<a href=\"/u/{0}\">{0}</a>", esc(l)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn rows(entries: &[&Entry], empty: &str, act: crate::actions::Act) -> String {
    if entries.is_empty() {
        return format!("<div class=\"blankslate\"><p>{empty}</p></div>");
    }
    let mut out = String::from("<div class=\"Box\">");
    for e in entries.iter().take(200) {
        let conflict = if e.mergeable == "CONFLICTING" {
            "<span class=\"Label Label--danger ml-1\">conflict</span>"
        } else {
            ""
        };
        let unowned = if e.unowned {
            "<span class=\"Label Label--attention ml-1\">unowned</span>"
        } else {
            ""
        };
        let mut meta = format!(
            "#{} by <a href=\"/u/{a}\">{a}</a> · {} old · <span class=\"branch-name\">{}</span>",
            e.number,
            ago(e.created_at),
            esc(&e.base),
            a = esc(&e.author)
        );
        if !e.approved_by.is_empty() {
            let _ = write!(meta, " · approved by {}", people(&e.approved_by));
        }
        if !e.credited_by.is_empty() {
            let _ = write!(meta, " · moved by {}", people(&e.credited_by));
        }
        let _ = write!(
            out,
            "<div class=\"Box-row d-flex flex-items-start\">\
               <span class=\"mr-2 color-fg-open\">{pr}</span>\
               <div class=\"flex-auto\">\
                 <a class=\"Link--primary text-bold\" href=\"{url}\">{title}</a>{conflict}{unowned}\
                 <div class=\"f6 color-fg-muted mt-1\">{meta}</div>\
               </div>\
               <span class=\"ml-2\">{ci}</span>{act}\
             </div>",
            pr = icon(octicon!("git-pull-request")),
            url = esc(&e.url),
            title = esc(&e.title),
            ci = ci_label(e),
            act = act.buttons(e.number),
        );
    }
    if entries.len() > 200 {
        let _ = write!(
            out,
            "<div class=\"Box-footer f6 color-fg-muted\">{} more in the JSON API</div>",
            entries.len() - 200
        );
    }
    out.push_str("</div>");
    out
}

async fn merge_page(State(app): State<Shared>, headers: HeaderMap) -> Html<String> {
    let viewer = app.viewer(&headers);
    let s = app.snap();
    let entries: Vec<&Entry> = s.queues.merge.iter().collect();
    let intro = if viewer.is_none() {
        crate::welcome::intro(&app, &headers)
    } else {
        String::new()
    };
    let body = format!(
        "{intro}<div class=\"Subhead\"><h1 class=\"Subhead-heading\">Ready to merge</h1>\
         <div class=\"Subhead-description\">Checks pass, no conflict, the current head is approved, and a review on it earned credit. \
         Ordered by the author's credit, so people who review rise. Work it from the top. \
         <a href=\"{NIXPKGS}/CONTRIBUTING.md#how-to-merge-pull-requests-yourself\">How nixpkgs merges</a></div></div>{}",
        rows(
            &entries,
            "Nothing is ready yet.",
            crate::actions::Act::of(&app, viewer.as_deref())
        )
    );
    page(&app, viewer.as_deref(), "merge", &body)
}

#[derive(Deserialize)]
struct ReviewQuery {
    bucket: Option<String>,
}

pub(crate) const BUCKETS: [&str; 3] = ["foryou", "unowned", "all"];

/// `mine` is the old name of `foryou`.
pub(crate) fn bucket_name(b: &str) -> Option<&'static str> {
    match b {
        "foryou" | "mine" => Some("foryou"),
        "unowned" => Some("unowned"),
        "all" => Some("all"),
        _ => None,
    }
}

/// `watched` are subscription keys. They are private: pass them only for
/// the person who asks for their own queue.
pub(crate) fn review_bucket<'a>(
    s: &'a Snapshot,
    viewer: Option<&str>,
    watched: &[String],
    bucket: &str,
) -> Vec<&'a Entry> {
    let mine = viewer.and_then(|v| s.areas.get(v));
    let mut prs: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut paths: Vec<&str> = Vec::new();
    for key in watched {
        match crate::catalog::Subject::parse(key) {
            Some(crate::catalog::Subject::Area(_)) => {
                paths.extend(key.strip_prefix("area:"));
            }
            Some(_) => prs.extend(s.links.prs.get(key).into_iter().flatten()),
            None => {}
        }
    }
    let watches = |e: &Entry| {
        prs.contains(&e.number)
            || e.files
                .iter()
                .any(|f| paths.iter().any(|p| area::under(f, p)))
    };
    s.queues
        .review
        .iter()
        .filter(|e| Some(e.author.as_str()) != viewer)
        .filter(|e| match bucket_name(bucket) {
            Some("foryou") => mine.is_some_and(|a| a.covers(&e.files)) || watches(e),
            Some("unowned") => e.unowned,
            _ => true,
        })
        .collect()
}

pub(crate) fn watched_of(app: &App, login: Option<&str>) -> Vec<String> {
    login
        .and_then(|l| app.store.watched(l).ok())
        .unwrap_or_default()
}

const HYGIENE: &str = "<div class=\"flash mb-3\"><strong>The newcomer path.</strong> Nobody in the club knows these areas, so they rot. \
A first review needs no domain knowledge: is <code>meta</code> complete (description, homepage, license, maintainers, mainProgram)? \
Are hashes fixed-output and pinned? Are <code>platforms</code> right? Does a CLI have <code>versionCheckHook</code>? \
Leave the finding as an inline comment on the changed line. A review here earns full credit at any age.</div>";

async fn review_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<ReviewQuery>,
) -> Html<String> {
    let viewer = app.viewer(&headers);
    let s = app.snap();
    let bucket = match q.bucket.as_deref().and_then(bucket_name) {
        Some("foryou") if viewer.is_none() => "unowned",
        Some(b) => b,
        None if viewer.is_some() => "foryou",
        None => "unowned",
    };
    let watched = watched_of(&app, viewer.as_deref());
    let mut entries = Vec::new();
    let mut tabs = String::new();
    for id in BUCKETS {
        if id == "foryou" && viewer.is_none() {
            continue;
        }
        let these = review_bucket(&s, viewer.as_deref(), &watched, id);
        let label = match id {
            "foryou" => "For you",
            "unowned" => "Unowned",
            _ => "All",
        };
        let sel = if id == bucket { " selected" } else { "" };
        let _ = write!(
            tabs,
            "<a class=\"SegmentedControl-item btn btn-sm{sel}\" href=\"/review?bucket={id}\">{label} <span class=\"Counter\">{}</span></a> ",
            these.len()
        );
        if id == bucket {
            entries = these;
        }
    }
    let intro = match bucket {
        "foryou" => "Pull requests that touch what you maintain (<code>meta.maintainers</code>, <code>meta.teams</code>), \
            own in <code>ci/OWNERS</code>, changed this year, tagged, or <a href=\"/watch\">watch</a>. Oldest first."
            .to_string(),
        "unowned" => HYGIENE.to_string(),
        _ => "Every listed pull request that is not ready to merge. Oldest first.".to_string(),
    };
    let body = format!(
        "<div class=\"Subhead\"><h1 class=\"Subhead-heading\">Needs review</h1>\
         <div class=\"Subhead-description\">Credit comes when your review moves a pull request: the author pushes or replies, \
         you comment on a changed line, or it merges within {} days. \
         <a href=\"{NIXPKGS}/CONTRIBUTING.md#how-to-review-pull-requests\">How nixpkgs reviews</a></div></div>\
         {}<div class=\"mb-3\">{tabs}</div><div class=\"mb-3\">{intro}</div>{}",
        ledger::MOVE_DAYS,
        viewer
            .as_deref()
            .map(|v| crate::progress::widget(&app, v))
            .unwrap_or_default(),
        rows(
            &entries,
            "Nothing here.",
            crate::actions::Act::of(&app, viewer.as_deref())
        )
    );
    page(&app, viewer.as_deref(), "review", &body)
}

#[derive(Deserialize)]
struct RecordQuery {
    login: Option<String>,
}

/// There is deliberately no ranking of people: a leaderboard drives volume,
/// and a global order lets five people dominate while the rest disengage.
async fn record_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<RecordQuery>,
) -> Response {
    let viewer = app.viewer(&headers);
    if let Some(l) = q.login.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
        return Redirect::to(&format!("/u/{}", crate::github::urlencode(l))).into_response();
    }
    if let Some(v) = &viewer {
        return Redirect::to(&format!("/u/{v}")).into_response();
    }
    let body = "<div class=\"Subhead\"><h1 class=\"Subhead-heading\">Review record</h1>\
        <div class=\"Subhead-description\">One person's reviews and what came of each. There is no ranking.</div></div>\
        <form method=\"get\" action=\"/record\" class=\"d-flex\" style=\"gap:8px;max-width:420px\">\
        <input class=\"form-control flex-auto\" name=\"login\" placeholder=\"GitHub login\" aria-label=\"GitHub login\">\
        <button class=\"btn\" type=\"submit\">Show record</button></form>";
    page(&app, None, "record", body).into_response()
}

fn trigger_label(t: &Trigger) -> &'static str {
    match t {
        Trigger::AuthorPushed => "author pushed",
        Trigger::AuthorReplied => "author replied",
        Trigger::InlineOnChange => "comment on a changed line",
        Trigger::Merged => "merged",
    }
}

fn outcome_label(o: Outcome) -> &'static str {
    match o {
        Outcome::MergedUnchanged => "<span class=\"Label Label--success\">merged unchanged</span>",
        Outcome::MergedAfterChanges => "<span class=\"Label\">merged after changes</span>",
        Outcome::MergedByReviewer => "<span class=\"Label\">merged by the reviewer</span>",
        Outcome::ChangedAfterReview => {
            "<span class=\"Label Label--attention\">changed after review</span>"
        }
        Outcome::Open => "<span class=\"Label\">open</span>",
        Outcome::Closed => "<span class=\"Label\">closed</span>",
    }
}

fn stat(n: usize, label: &str, hint: &str) -> String {
    format!(
        "<div class=\"Box p-3 flex-1\"><div class=\"f1 text-bold\">{n}</div><div class=\"text-bold\">{label}</div>\
         <div class=\"f6 color-fg-muted\">{hint}</div></div>"
    )
}

fn overlap(title: &str, list: &[(String, usize)]) -> String {
    let mut out =
        format!("<div class=\"Box flex-1\"><div class=\"Box-header text-bold\">{title}</div>");
    if list.is_empty() {
        out.push_str("<div class=\"Box-row color-fg-muted\">None yet.</div>");
    }
    for (l, n) in list {
        let _ = write!(
            out,
            "<div class=\"Box-row d-flex\"><a class=\"flex-auto\" href=\"/u/{l}\">{l}</a><span class=\"Counter\">{n}</span></div>",
            l = esc(l)
        );
    }
    out.push_str("</div>");
    out
}

async fn person_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(login): Path<String>,
) -> Html<String> {
    let viewer = app.viewer(&headers);
    let s = app.snap();
    let p = s.ledger.people.get(&login).cloned().unwrap_or_default();
    let d = dossier::build(&s.pr_data, &login);
    let listed = if p.listed {
        "<span class=\"Label Label--success\">listed</span>".to_string()
    } else {
        format!(
            "<span class=\"Label\">not listed: {} of {} authors</span>",
            p.counterparties,
            ledger::MIN_COUNTERPARTIES
        )
    };
    let mut out = format!(
        "<div class=\"Subhead\"><h1 class=\"Subhead-heading\">{l} {listed}</h1>\
         <div class=\"Subhead-description\">Evidence, not a verdict. Maintainer Mode grants nothing; nixpkgs committers do. \
         Reviews in the synced data: the last {} days and every open pull request.</div></div>\
         <div class=\"d-flex flex-wrap mb-4\" style=\"gap:16px\">{}{}</div>",
        ledger::WINDOW_DAYS,
        stat(
            d.merged_unchanged,
            "merged unchanged after their review",
            "a committer merged it later, with no push after the review"
        ),
        stat(
            d.problems_found,
            "problems found",
            "the author changed the pull request after their review asked for it"
        ),
        l = esc(&login)
    );
    if viewer.as_deref() == Some(login.as_str()) {
        let area = s.areas.get(&login).cloned().unwrap_or_default();
        let _ = write!(
            out,
            "<div class=\"flash mb-4\">{} <a href=\"/me/progress\">Your progress</a>: your reviews, your streak, your goal and your badges. \
             Only you see it.</div>",
            icon(octicon!("flame"))
        );
        let _ = write!(
            out,
            "<div class=\"Box mb-4\"><div class=\"Box-header text-bold\">{} Your area</div>\
             <div class=\"Box-row f6\">{} paths you maintain · {} you own in ci/OWNERS · {} you changed this year</div>\
             <div class=\"Box-row\"><form method=\"post\" action=\"/me/tags\">\
             <label class=\"f6\" for=\"tags\">Tags: path prefixes you want to see, one per line, like <code>pkgs/development/python-modules</code></label>\
             <textarea class=\"form-control width-full my-2\" id=\"tags\" name=\"tags\" rows=\"3\">{}</textarea>\
             <button class=\"btn btn-sm\" type=\"submit\">Save tags</button></form></div></div>",
            icon(octicon!("tag")),
            area.maintains.len(),
            area.owns.len(),
            area.committed.len(),
            esc(&area.tags.join("\n"))
        );
    }
    let _ = write!(
        out,
        "<div class=\"Box mb-4\"><div class=\"Box-header text-bold\">Last {} reviews, and what came of each</div>",
        dossier::RECENT
    );
    if d.recent.is_empty() {
        out.push_str("<div class=\"Box-row color-fg-muted\">No reviews in the synced data.</div>");
    }
    for r in &d.recent {
        let found = if r.problem_found {
            " <span class=\"Label Label--attention\">problem found</span>"
        } else {
            ""
        };
        let _ = write!(
            out,
            "<div class=\"Box-row d-flex\"><div class=\"flex-auto\">\
             <a class=\"Link--primary\" href=\"https://github.com/NixOS/nixpkgs/pull/{n}\">#{n} {t}</a>\
             <div class=\"f6 color-fg-muted\">{} · <a href=\"{u}\">{}</a> · author <a href=\"/u/{a}\">{a}</a></div></div>\
             <div class=\"text-right\">{}{found}</div></div>",
            r.at.format("%Y-%m-%d"),
            esc(&r.state.to_lowercase().replace('_', " ")),
            outcome_label(r.outcome),
            n = r.pr,
            t = esc(&r.pr_title),
            u = esc(&r.review_url),
            a = esc(&r.pr_author),
        );
    }
    out.push_str("</div>");
    let _ = write!(
        out,
        "<div class=\"d-flex flex-wrap mb-4\" style=\"gap:16px\">{}{}</div>",
        overlap("Reviewed alongside", &d.co_reviewers),
        overlap("Reviewed pull requests of", &d.authors)
    );
    let _ = write!(
        out,
        "<details class=\"Box\"><summary class=\"Box-header text-bold\">Credit: {:.2}, reciprocity {:.0}% \
         <span class=\"f6 color-fg-muted text-normal\">(decides listing only; reciprocity is shown, never acted on)</span></summary>",
        p.score,
        p.reciprocity * 100.0
    );
    let mut credits: Vec<_> = s.ledger.credits_of(&login).collect();
    credits.sort_by_key(|c| std::cmp::Reverse(c.at));
    if credits.is_empty() {
        out.push_str("<div class=\"Box-row color-fg-muted\">No credit yet.</div>");
    }
    for c in credits {
        let why: Vec<&str> = c.triggers.iter().map(trigger_label).collect();
        let status = match &c.dropped {
            None => format!(
                "{:.2} <span class=\"color-fg-muted\">of {:.2}</span>",
                c.value, c.weight
            ),
            Some(Dropped::DailyCap) => {
                "<span class=\"Label\">over the daily cap</span>".to_string()
            }
            Some(Dropped::Voided { by, url }) => format!(
                "<a class=\"Label Label--danger\" href=\"{}\">voided by {}</a>",
                esc(url),
                esc(by)
            ),
        };
        let mut factors = Vec::new();
        if c.orphan {
            factors.push("unowned".to_string());
        } else if !c.queue_head {
            factors.push(format!("picked fresh ×{}", ledger::PICKED_FACTOR));
        }
        if c.pair_factor < 1.0 {
            factors.push(format!("repeat author ×{}", c.pair_factor));
        }
        let _ = write!(
            out,
            "<div class=\"Box-row d-flex\"><div class=\"flex-auto\">\
             <a href=\"{u}\">review</a> of <a href=\"https://github.com/NixOS/nixpkgs/pull/{n}\">#{n}</a> {t} by <a href=\"/u/{a}\">{a}</a>\
             <div class=\"f6 color-fg-muted\">{} · {} {}</div></div><div class=\"text-right\">{status}</div></div>",
            c.at.format("%Y-%m-%d"),
            why.join(", "),
            if factors.is_empty() {
                String::new()
            } else {
                format!("· {}", factors.join(", "))
            },
            u = esc(&c.review_url),
            n = c.pr,
            t = esc(&c.pr_title),
            a = esc(&c.pr_author),
        );
    }
    out.push_str("</details>");
    page(&app, viewer.as_deref(), "record", &out)
}

#[derive(Deserialize)]
struct TagsForm {
    tags: String,
}

async fn set_tags(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<TagsForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let tags: Vec<String> = f
        .tags
        .lines()
        .map(|l| l.trim().trim_matches('/').to_string())
        .filter(|l| !l.is_empty())
        .take(50)
        .collect();
    if let Err(e) = app
        .store
        .set_tags(&login, &tags)
        .and_then(|_| app.recompute())
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    Redirect::to(&format!("/u/{login}")).into_response()
}

async fn login(State(app): State<Shared>) -> Response {
    let Some(oauth) = &app.oauth else {
        return (StatusCode::SERVICE_UNAVAILABLE, "login is not configured").into_response();
    };
    let state = random_id();
    (
        [(
            header::SET_COOKIE,
            set_cookie(&app, "oauth_state", &state, 600),
        )],
        Redirect::to(&oauth.authorize_url(&state)),
    )
        .into_response()
}

#[derive(Deserialize)]
struct Callback {
    code: String,
    state: String,
}

async fn callback(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(cb): Query<Callback>,
) -> Response {
    let Some(oauth) = &app.oauth else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if cookie(&headers, "oauth_state").as_deref() != Some(cb.state.as_str()) {
        return (StatusCode::BAD_REQUEST, "state mismatch, try again").into_response();
    }
    let (login, token) = match oauth.exchange(&cb.code).await {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    };
    let id = random_id();
    if let Err(e) = app.store.create_session(&id, &login, &token) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    let next = if crate::welcome::first_visit(&app, &login) {
        "/welcome"
    } else {
        "/review?bucket=foryou"
    };
    let bg = app.clone();
    let who = login.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::refresh_member(&bg, &who, &token).await {
            tracing::warn!("area of {who}: {e:#}");
        }
    });
    let mut h = HeaderMap::new();
    h.append(
        header::SET_COOKIE,
        set_cookie(&app, "session", &id, 30 * 86400)
            .parse()
            .unwrap(),
    );
    h.append(
        header::SET_COOKIE,
        set_cookie(&app, "oauth_state", "", 0).parse().unwrap(),
    );
    (h, Redirect::to(next)).into_response()
}

async fn logout(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if let Some(id) = cookie(&headers, "session") {
        let _ = app.store.delete_session(&id);
    }
    (
        [(header::SET_COOKIE, set_cookie(&app, "session", "", 0))],
        Redirect::to("/"),
    )
        .into_response()
}

#[derive(Serialize)]
struct Status {
    prs: usize,
    last_sync: Option<String>,
    watermark: Option<String>,
    open_done: bool,
    index_rev: String,
    computed_at: DateTime<Utc>,
    members: usize,
    committers: serde_json::Value,
    rules: serde_json::Value,
}

async fn api_status(State(app): State<Shared>) -> Json<Status> {
    let s = app.snap();
    Json(Status {
        prs: s.prs,
        last_sync: app.store.get("last_sync").ok().flatten(),
        watermark: app.store.get("watermark").ok().flatten(),
        open_done: app.store.get("open_done").ok().flatten().is_some(),
        index_rev: s.index_rev.clone(),
        computed_at: s.ledger.computed_at,
        members: s.areas.len(),
        committers: json!({
            "logins": s.committers.len(),
            "merged_last_365_days": s.committers.from_merges,
            "team": crate::committer::TEAM,
            "team_members": s.committers.from_team,
            "team_read_at": s.committers.team_at,
        }),
        rules: json!({
            "move_days": ledger::MOVE_DAYS,
            "half_life_days": ledger::HALF_LIFE_DAYS,
            "window_days": ledger::WINDOW_DAYS,
            "daily_cap": ledger::DAILY_CAP,
            "per_pr_cap": 1,
            "min_counterparties": ledger::MIN_COUNTERPARTIES,
            "queue_head_days": ledger::QUEUE_HEAD_DAYS,
            "picked_factor": ledger::PICKED_FACTOR,
            "pair_factor": ledger::PAIR_FACTOR,
        }),
    })
}

async fn api_merge(State(app): State<Shared>) -> Json<Vec<Entry>> {
    Json(app.snap().queues.merge.clone())
}

#[derive(Deserialize)]
struct ApiReview {
    bucket: Option<String>,
    login: Option<String>,
}

/// `bucket=foryou` takes `login`, so an agent can ask for anyone's routed
/// queue; areas are derived from public data and are not secret.
/// Subscriptions are, so they count only when the session is that login.
async fn api_review(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<ApiReview>,
) -> Json<Vec<Entry>> {
    let s = app.snap();
    let viewer = app.viewer(&headers);
    let login = q.login.or_else(|| viewer.clone());
    let watched = if login.is_some() && login == viewer {
        watched_of(&app, viewer.as_deref())
    } else {
        Vec::new()
    };
    let bucket = q.bucket.as_deref().unwrap_or("all");
    Json(
        review_bucket(&s, login.as_deref(), &watched, bucket)
            .into_iter()
            .cloned()
            .collect(),
    )
}

async fn api_person(State(app): State<Shared>, Path(login): Path<String>) -> Response {
    let s = app.snap();
    let person = s
        .ledger
        .people
        .get(&login)
        .cloned()
        .unwrap_or(ledger::Person {
            login: login.clone(),
            ..Default::default()
        });
    let credits: Vec<_> = s.ledger.credits_of(&login).cloned().collect();
    let dossier = dossier::build(&s.pr_data, &login);
    Json(json!({ "person": person, "dossier": dossier, "credits": credits, "area": s.areas.get(&login) }))
        .into_response()
}

pub fn load_index(store: &Store) -> Index {
    store
        .get("index")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub async fn refresh_index(app: &App) -> Result<()> {
    let index = area::fetch(&app.data_dir, &app.index_sets).await?;
    app.store.set("index", &serde_json::to_string(&index)?)?;
    app.store.set("index_at", &Utc::now().to_rfc3339())?;
    *app.index.write().unwrap() = index;
    Ok(())
}

pub fn load_health(store: &Store) -> crate::health::Health {
    store
        .get("health")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Gives each heading the id GitHub would, so `#the-rules` links still land.
fn anchor_headings(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find("<h") {
        let level = rest.as_bytes().get(i + 2).copied();
        let close = rest[i..].find('>').map(|j| i + j);
        match (level, close) {
            (Some(l @ b'1'..=b'6'), Some(c)) if c == i + 3 => {
                let tag_end = format!("</h{}>", l as char);
                let text_end = rest[c..].find(&tag_end).map_or(rest.len(), |j| c + j);
                let slug: String = rest[c + 1..text_end]
                    .to_lowercase()
                    .chars()
                    .filter_map(|ch| match ch {
                        'a'..='z' | '0'..='9' | '-' => Some(ch),
                        ' ' => Some('-'),
                        _ => None,
                    })
                    .collect();
                out.push_str(&rest[..i]);
                let _ = write!(out, "<h{} id=\"{slug}\">", l as char);
                rest = &rest[c + 1..];
            }
            _ => {
                out.push_str(&rest[..i + 2]);
                rest = &rest[i + 2..];
            }
        }
    }
    out.push_str(rest);
    out
}

const NIXPKGS: &str = "https://github.com/NixOS/nixpkgs/blob/master";

/// The rules below say what this site shows and credits; how to work on
/// nixpkgs is nixpkgs' own guide.
fn guide_box() -> String {
    let links = [
        (
            "CONTRIBUTING.md",
            "how to open, review and merge a pull request, commit messages, and the automation and AI policy",
        ),
        (
            "pkgs/README.md",
            "how to write, update and review a package",
        ),
        (
            "nixos/README.md",
            "how to write a NixOS module and its test",
        ),
        (
            "maintainers/README.md",
            "how to become a maintainer, and what it means",
        ),
    ];
    let items: String = links
        .iter()
        .map(|(file, what)| format!("<li><a href=\"{NIXPKGS}/{file}\">{file}</a>: {what}</li>"))
        .collect();
    format!(
        "<div class=\"flash mb-4\" id=\"contributor-guide\"><strong>These are the rules of Maintainer Mode, not of nixpkgs.</strong> \
         nixpkgs decides how the work is done. Read its contributor guide first:\
         <ul class=\"ml-4 mt-2\">{items}<li><a href=\"https://nixos.org/manual/nixpkgs/unstable/\">The Nixpkgs manual</a>: the reference</li></ul></div>"
    )
}

async fn rules_page(State(app): State<Shared>, headers: HeaderMap) -> Html<String> {
    let viewer = app.viewer(&headers);
    let mut html = String::new();
    pulldown_cmark::html::push_html(
        &mut html,
        pulldown_cmark::Parser::new_ext(RULES, pulldown_cmark::Options::ENABLE_TABLES),
    );
    let body = format!(
        "{}<article class=\"markdown-body\">{}</article>",
        guide_box(),
        anchor_headings(&html)
    );
    page(&app, viewer.as_deref(), "rules", &body)
}

fn package_stats(app: &App) -> Arc<crate::catalog::PackageStats> {
    app.packages.read().unwrap().clone()
}

async fn api_metrics(State(app): State<Shared>) -> Json<serde_json::Value> {
    let mut v = serde_json::to_value(Report::clone(&app.metrics.read().unwrap()))
        .unwrap_or_else(|_| json!({}));
    v["packages"] = json!(*package_stats(&app));
    Json(v)
}

fn packages_section(app: &App) -> String {
    let p = package_stats(app);
    let mut out = String::from("<h2 class=\"f4 mt-4 mb-2\" id=\"packages\">Packages</h2>");
    if p.rev.is_empty() {
        out.push_str(
            "<div class=\"flash\">The first nixpkgs index is still being built. These numbers come from it.</div>",
        );
        return out;
    }
    let b = &p.advisories_by_band;
    let _ = write!(
        out,
        "<p class=\"f6 color-fg-muted\">One package per package name, at nixpkgs {}. \
         <a href=\"/rules#the-state-of-nixpkgs\">How each number is made</a></p>\
         <div class=\"d-flex flex-wrap mb-2\" style=\"gap:8px\">{}{}{}{}</div>\
         <div class=\"d-flex flex-wrap mb-4\" style=\"gap:8px\">{}{}{}{}</div>",
        esc(&p.rev[..p.rev.len().min(12)]),
        stat(p.packages, "packages", "in the swept package sets"),
        stat(
            p.behind_upstream,
            "behind upstream",
            "a newer release in its git tags, its registry or repology"
        ),
        stat(
            p.with_advisories,
            "with open advisories",
            &format!(
                "worst: {} critical, {} high, {} medium, {} low, {} with no score",
                b.critical, b.high, b.medium, b.low, b.unscored
            )
        ),
        stat(
            p.unmaintained,
            "unmaintained",
            "no meta.maintainers and no meta.teams"
        ),
        stat(p.broken, "broken", "meta.broken"),
        stat(p.insecure, "insecure", "meta.knownVulnerabilities is set"),
        stat(
            p.with_packaging_gaps,
            "with packaging gaps",
            "see the Packaging row of a package page"
        ),
        stat(
            p.open_bumps,
            "open version bumps",
            &match p.bumps_median_age_days {
                Some(d) => format!("median age {d} days; <a href=\"/bumps\">the list</a>"),
                None => "<a href=\"/bumps\">the list</a>".into(),
            }
        ),
    );
    out
}

#[derive(Deserialize)]
struct EventsQuery {
    /// Not `#[serde(flatten)]`: serde_urlencoded then hands every value
    /// over as a string, and `number` fails to parse.
    #[serde(rename = "type")]
    kind: Option<String>,
    number: Option<u64>,
    actor: Option<String>,
    /// For clients that cannot set `Last-Event-ID` on the first request.
    after: Option<u64>,
}

async fn api_events(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use futures_util::StreamExt;
    let after = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
        .or(q.after)
        .unwrap_or_else(|| app.events.head());
    let stream = crate::events::follow(app.events.clone(), after, q.filter()).map(|s| {
        Ok::<_, std::convert::Infallible>(Event::default().id(s.seq.to_string()).data(s.json))
    });
    (
        [("x-accel-buffering", "no")],
        Sse::new(stream).keep_alive(KeepAlive::default()),
    )
        .into_response()
}

fn num(v: Option<f64>) -> String {
    v.map_or_else(|| "–".into(), |v| format!("{v}"))
}

fn pct(v: Option<f64>) -> String {
    v.map_or_else(|| "–".into(), |v| format!("{:.0}%", v * 100.0))
}

fn table(head: &[&str], rows: &[Vec<String>]) -> String {
    let mut out =
        String::from("<div class=\"markdown-body\" style=\"overflow-x:auto\"><table><thead><tr>");
    for h in head {
        let _ = write!(out, "<th>{h}</th>");
    }
    out.push_str("</tr></thead><tbody>");
    for r in rows {
        out.push_str("<tr>");
        for c in r {
            let _ = write!(out, "<td>{c}</td>");
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table></div>");
    out
}

async fn state_page(State(app): State<Shared>, headers: HeaderMap) -> Html<String> {
    let viewer = app.viewer(&headers);
    let r = app.metrics.read().unwrap().clone();
    let mut body = String::from(
        "<div class=\"Subhead\"><h1 class=\"Subhead-heading\">State of nixpkgs</h1>\
         <div class=\"Subhead-description\">Pull request flow and package health from public data. \
         Aggregates only: no figure names a person. \
         <a href=\"/rules#the-state-of-nixpkgs\">How each number is made</a> · \
         <a href=\"/api/v1/metrics\">JSON</a></div></div>",
    );
    let Some(at) = r.generated_at else {
        body.push_str("<div class=\"flash\">The first sync has not finished yet.</div>");
        body.push_str(&packages_section(&app));
        return page(&app, viewer.as_deref(), "state", &body);
    };
    let n = &r.now;
    let _ = write!(
        body,
        "<p class=\"f6 color-fg-muted\">{} pull requests, opened since {}. Computed {}. {}</p>",
        r.pull_requests,
        r.earliest_created
            .map(|t| t.format("%Y-%m-%d").to_string())
            .unwrap_or_default(),
        ago(at),
        match app.store.get("history_done").ok().flatten() {
            Some(_) => "The history walk is complete.".to_string(),
            None =>
                "The history walk is still running, so older months are incomplete.".to_string(),
        }
    );
    let _ = write!(
        body,
        "<div class=\"d-flex flex-wrap mb-4\" style=\"gap:8px\">{}{}{}{}{}</div>",
        stat(
            n.open,
            "open",
            &format!(
                "{} drafts, {} from bots, median age {} days",
                n.drafts,
                n.by_bots,
                num(n.age_days_p50)
            )
        ),
        stat(
            n.never_reviewed,
            "never reviewed",
            "human authors, not drafts, no review by anyone else"
        ),
        stat(
            n.security,
            "open security fixes",
            &format!("median age {} days", num(n.security_age_days_p50))
        ),
        stat(
            n.first_time,
            "open from first-time contributors",
            "by GitHub's association or the nixpkgs label"
        ),
        stat(
            n.ai,
            "open with disclosed AI use",
            "Assisted-by trailer or llm-assisted label; a floor"
        ),
    );
    let ages: Vec<Vec<String>> = n
        .age
        .iter()
        .map(|(k, v)| vec![esc(k.split_once(": ").map_or(k, |(_, l)| l)), v.to_string()])
        .chain(
            n.base
                .iter()
                .map(|(k, v)| vec![format!("base: {k}"), v.to_string()]),
        )
        .collect();
    let c = &r.concentration_365d;
    let load = |l: &metrics::Load, what: &str| {
        vec![
            what.to_string(),
            l.total.to_string(),
            l.people.to_string(),
            l.people_for_half.to_string(),
            l.people_for_80.to_string(),
        ]
    };
    let _ = write!(
        body,
        "<div class=\"d-flex flex-wrap mb-4\" style=\"gap:16px\"><div class=\"flex-1\"><h2 class=\"f4 mb-2\">Open now</h2>{}</div>\
         <div class=\"flex-1\"><h2 class=\"f4 mb-2\">Who carries the load, last 365 days</h2>{}</div></div>",
        table(&["", "open"], &ages),
        table(
            &["", "total", "people", "people for half", "people for 80%"],
            &[
                load(&c.merges, "merges"),
                load(&c.reviews, "reviews"),
                load(&c.authored, "authored, humans"),
            ]
        ),
    );
    let months: Vec<Vec<String>> = r
        .months
        .iter()
        .rev()
        .take(24)
        .map(|m| {
            vec![
                m.month.clone(),
                m.opened.to_string(),
                m.opened_by_bots.to_string(),
                m.opened_first_time.to_string(),
                format!("{} / {}", m.opened_ai, m.opened_ai_other_form),
                m.merged.to_string(),
                m.closed_unmerged.to_string(),
                m.open_at_end.to_string(),
                m.reviews.to_string(),
                m.reviewers.to_string(),
                m.mergers.to_string(),
                num(m.first_review_hours_p50),
                pct(m.reviewed_within_7d),
                format!("{} / {}", num(m.merge_days_p50), num(m.merge_days_p90)),
                pct(m.merge_rate),
                pct(m.merge_rate_first_time),
            ]
        })
        .collect();
    let _ = write!(
        body,
        "<h2 class=\"f4 mb-2\">Month by month</h2>{}",
        table(
            &[
                "month",
                "opened",
                "bots",
                "first-time",
                "AI / other form",
                "merged",
                "closed",
                "open at end",
                "reviews",
                "reviewers",
                "mergers",
                "first review, h (p50)",
                "reviewed in 7 days",
                "days to merge (p50 / p90)",
                "merge rate",
                "first-time merge rate",
            ],
            &months
        )
    );
    let areas: Vec<Vec<String>> = r
        .areas
        .iter()
        .map(|a| {
            vec![
                esc(&a.area),
                a.open.to_string(),
                a.never_reviewed.to_string(),
                a.opened_365d.to_string(),
                a.merged_365d.to_string(),
                num(a.merge_days_p50_365d),
            ]
        })
        .collect();
    let tools: Vec<Vec<String>> = r
        .ai_tools_365d
        .iter()
        .map(|(t, n)| vec![esc(t), n.to_string()])
        .collect();
    let _ = write!(
        body,
        "<div class=\"d-flex flex-wrap mt-4\" style=\"gap:16px\"><div class=\"flex-1\"><h2 class=\"f4 mb-2\">By area</h2>{}</div>\
         <div><h2 class=\"f4 mb-2\">Disclosed AI tools, last 365 days</h2>{}</div></div>",
        table(
            &[
                "area",
                "open",
                "never reviewed",
                "opened, 365 d",
                "merged, 365 d",
                "days to merge (p50)"
            ],
            &areas
        ),
        table(&["tool", "pull requests"], &tools),
    );
    body.push_str(&packages_section(&app));
    page(&app, viewer.as_deref(), "state", &body)
}

/// SameSite=Lax still sends the session cookie on a POST from a sibling
/// subdomain, which is same-site. Browsers say where a request comes from in
/// `Sec-Fetch-Site`; a client without it is not a browser.
fn foreign(method: &axum::http::Method, headers: &HeaderMap) -> bool {
    method == axum::http::Method::POST
        && headers
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v != "same-origin" && v != "none")
}

async fn same_origin(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    if foreign(req.method(), req.headers()) {
        return (StatusCode::FORBIDDEN, "send this from a page of this site").into_response();
    }
    next.run(req).await
}

impl EventsQuery {
    fn filter(self) -> crate::events::Filter {
        crate::events::Filter {
            kind: self.kind,
            number: self.number,
            actor: self.actor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;

    #[test]
    fn ages_lose_precision_as_they_grow() {
        let back = |d: chrono::Duration| ago(Utc::now() - d);
        assert_eq!(back(chrono::Duration::minutes(5)), "5m");
        assert_eq!(back(chrono::Duration::hours(30)), "1d");
        assert_eq!(back(chrono::Duration::days(20)), "2w");
        assert_eq!(back(chrono::Duration::days(200)), "6mo");
        assert_eq!(back(chrono::Duration::days(2090)), "5y");
    }

    fn entry(number: u64, author: &str, file: &str, unowned: bool) -> Entry {
        Entry {
            number,
            title: format!("pr {number}"),
            url: String::new(),
            author: author.into(),
            author_score: 0.0,
            base: "master".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            ci: None,
            mergeable: "MERGEABLE".into(),
            reviewers: vec![],
            approved_by: vec![],
            credited_by: vec![],
            files: vec![file.into()],
            unowned,
        }
    }

    #[test]
    fn for_you_joins_the_area_and_the_subscriptions() {
        let mut s = Snapshot::default();
        s.queues.review = vec![
            entry(1, "x", "pkgs/by-name/he/hello/package.nix", false),
            entry(2, "x", "pkgs/by-name/cu/curl/package.nix", false),
            entry(
                3,
                "x",
                "nixos/modules/services/web-servers/nginx/default.nix",
                false,
            ),
            entry(4, "x", "lib/strings.nix", true),
            entry(5, "bob", "pkgs/by-name/he/hello/package.nix", false),
        ];
        s.areas.insert(
            "bob".into(),
            Area {
                maintains: vec!["pkgs/by-name/he/hello".into()],
                ..Default::default()
            },
        );
        s.links.prs.insert("pkg:curl".into(), vec![2]);
        let watched = vec![
            "pkg:curl".to_string(),
            "area:nixos/modules/services/web-servers".to_string(),
        ];
        let numbers = |v: Vec<&Entry>| v.iter().map(|e| e.number).collect::<Vec<_>>();
        assert_eq!(
            numbers(review_bucket(&s, Some("bob"), &watched, "foryou")),
            vec![1, 2, 3]
        );
        assert_eq!(
            numbers(review_bucket(&s, Some("bob"), &[], "mine")),
            vec![1]
        );
        assert_eq!(
            numbers(review_bucket(&s, Some("bob"), &watched, "unowned")),
            vec![4]
        );
        assert_eq!(numbers(review_bucket(&s, None, &[], "all")).len(), 5);
    }

    #[test]
    fn a_post_from_another_site_is_refused() {
        let h = |v: Option<&str>| {
            let mut h = HeaderMap::new();
            if let Some(v) = v {
                h.insert("sec-fetch-site", v.parse().unwrap());
            }
            h
        };
        assert!(foreign(&Method::POST, &h(Some("cross-site"))));
        assert!(foreign(&Method::POST, &h(Some("same-site"))));
        assert!(!foreign(&Method::POST, &h(Some("same-origin"))));
        assert!(!foreign(&Method::POST, &h(None)));
        assert!(!foreign(&Method::GET, &h(Some("cross-site"))));
    }
    #[test]
    fn events_query_parses_every_filter() {
        let uri: axum::http::Uri = "/api/v1/events?type=PushEvent&number=123&actor=alice&after=3"
            .parse()
            .unwrap();
        let Query(q) = Query::<EventsQuery>::try_from_uri(&uri).unwrap();
        assert_eq!(q.after, Some(3));
        let f = q.filter();
        assert_eq!(f.kind.as_deref(), Some("PushEvent"));
        assert_eq!(f.number, Some(123));
        assert_eq!(f.actor.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn package_numbers_come_from_the_last_recompute() {
        let app = crate::sets::tests::app(&["hello"]);
        app.index
            .write()
            .unwrap()
            .by_pname
            .insert("hello".into(), "hello".into());
        let packages = |v: Json<serde_json::Value>| v.0["packages"]["packages"].clone();
        assert_eq!(packages(api_metrics(State(app.clone())).await), 0);
        app.recompute().unwrap();
        assert_eq!(packages(api_metrics(State(app.clone())).await), 1);
        app.index.write().unwrap().by_pname.clear();
        assert_eq!(packages(api_metrics(State(app.clone())).await), 1);
    }
}
