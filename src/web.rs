use anyhow::Result;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
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
use crate::ui::{Page, Section, Tab};

const PRIMER_CSS: &str = include_str!(concat!(env!("PRIMER_DIR"), "/primer.css"));
const CLUB_CSS: &str = include_str!("club.css");
/// Pull request pages read GitHub on a miss of the cache; crawlers must
/// not walk them.
const ROBOTS: &str = "User-agent: *\nDisallow: /pr/\nDisallow: /api/v1/prs/\n";

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
    pub sync_token: Option<Arc<crate::secret::Secret>>,
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
    pub inbox: crate::notify::Counts,
    /// The SMTP server of the email digest; None while email is off.
    pub smtp: Option<crate::notify::Smtp>,
    /// What pull request pages read from GitHub on demand.
    pub pr_cache: crate::prpage::Cache,
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
        let queues = queue::build(&prs, &ledger, |p| own.orphan(p));
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
        .merge(crate::home::routes())
        .merge(crate::review::routes())
        .route("/u/:login", get(person_page))
        .route("/me/tags", post(set_tags))
        .route("/auth/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/auth/logout", post(logout))
        .route("/static/primer.css", get(|| css(PRIMER_CSS)))
        .route("/static/club.css", get(|| css(CLUB_CSS)))
        .route("/robots.txt", get(|| async { ROBOTS }))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/v1/status", get(api_status))
        .route("/api/v1/queue/merge", get(api_merge))
        .route("/api/v1/queue/review", get(api_review))
        .route("/api/v1/people/:login", get(api_person))
        .route("/api/v1/metrics", get(api_metrics))
        .route("/api/v1/events", get(api_events))
        .merge(crate::nixpkgs::routes())
        .merge(crate::catalog::routes())
        .merge(crate::bumps::routes())
        .merge(crate::committer::routes())
        .merge(crate::actions::routes())
        .merge(crate::status::routes())
        .merge(crate::sets::routes())
        .merge(crate::archive::routes())
        .merge(crate::llm::routes())
        .merge(crate::progress::routes())
        .merge(crate::prpage::routes())
        .merge(crate::mcp::routes())
        .merge(crate::rescan::routes())
        .merge(crate::welcome::routes())
        .merge(crate::notify::routes())
        .merge(crate::me::routes())
        .layer(axum::middleware::from_fn(crate::me::no_store))
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

pub(crate) const CONTRIBUTING: &str =
    "https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md";

/// Maintainer Mode finds the work; these say how nixpkgs wants it done.
pub(crate) fn nixpkgs_guides() -> String {
    guide_box()
}

pub(crate) fn ci_label(e: &Entry) -> String {
    match e.ci.as_deref() {
        Some("SUCCESS") => format!(
            "<span class=\"color-fg-success\" role=\"img\" title=\"checks pass\" aria-label=\"checks pass\">{}</span>",
            icon(octicon!("check-circle-fill"))
        ),
        Some("FAILURE" | "ERROR") => format!(
            "<span class=\"color-fg-danger\" role=\"img\" title=\"checks fail\" aria-label=\"checks fail\">{}</span>",
            icon(octicon!("x-circle-fill"))
        ),
        _ => format!(
            "<span class=\"color-fg-attention\" role=\"img\" title=\"checks pending\" aria-label=\"checks pending\">{}</span>",
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

/// One `Box-row` per entry, to go in a [`crate::ui::section`].
pub(crate) fn entry_rows(entries: &[&Entry], act: crate::actions::Act) -> String {
    use crate::ui::{Kind, Tone, badge, item_row};
    let mut out = String::new();
    for e in entries {
        let mut badges = Vec::new();
        if e.mergeable == "CONFLICTING" {
            badges.push(badge(Tone::Danger, "conflict"));
        }
        if e.unowned {
            badges.push(badge(Tone::Attention, "no maintainer"));
        }
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
            let _ = write!(meta, " · credited review by {}", people(&e.credited_by));
        }
        out.push_str(&item_row(
            Some(Kind::PR_OPEN),
            &crate::prpage::title_link(e.number, &esc(&e.title), "Link--primary text-bold"),
            "",
            &meta,
            &badges,
            &format!(
                "<span class=\"ml-2\">{}</span>{}",
                ci_label(e),
                act.buttons(e.number)
            ),
        ));
    }
    out
}

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
    let rows: String = list
        .iter()
        .map(|(l, n)| {
            format!(
                "<div class=\"Box-row d-flex\"><a class=\"flex-auto\" href=\"/u/{l}\">{l}</a><span class=\"Counter\">{n}</span></div>",
                l = esc(l)
            )
        })
        .collect();
    format!(
        "<div class=\"flex-1\">{}</div>",
        crate::ui::section(title, None, None, &rows)
    )
}

/// The tabs of `/u/<login>`. Both are public, so they never show private
/// data: that goes under `/me`.
pub(crate) fn person_tabs(login: &str, maintains: bool) -> Vec<Tab> {
    let base = format!("/u/{}", crate::github::urlencode(login));
    vec![
        Tab::new(base.clone(), "Record", !maintains),
        Tab::new(format!("{base}/maintains"), "Maintains", maintains),
    ]
}

/// There is deliberately no ranking of people: a leaderboard drives volume,
/// and a global order lets five people dominate while the rest disengage.
async fn person_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(login): Path<String>,
) -> Response {
    use crate::ui::{Tone, badge, empty, section};
    let viewer = app.viewer(&headers);
    let s = app.snap();
    let p = s.ledger.people.get(&login).cloned().unwrap_or_default();
    let d = dossier::build(&s.pr_data, &login);
    let listed = if p.listed {
        badge(Tone::Success, "listed")
    } else {
        badge(
            Tone::Neutral,
            &format!(
                "not listed: {} of {} authors",
                p.counterparties,
                ledger::MIN_COUNTERPARTIES
            ),
        )
    };
    let mut out = format!(
        "<div class=\"d-flex flex-wrap gap-3 mb-4\">{}{}</div>",
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
    );
    let mut recent = String::new();
    for r in &d.recent {
        let found = if r.problem_found {
            format!(" {}", badge(Tone::Attention, "problem found"))
        } else {
            String::new()
        };
        let _ = write!(
            recent,
            "<div class=\"Box-row d-flex\"><div class=\"flex-auto\">\
             {t}\
             <div class=\"f6 color-fg-muted\">{} · <a href=\"{u}\">{}</a> · author <a href=\"/u/{a}\">{a}</a></div></div>\
             <div class=\"text-right\">{}{found}</div></div>",
            r.at.format("%Y-%m-%d"),
            esc(&r.state.to_lowercase().replace('_', " ")),
            outcome_label(r.outcome),
            t = crate::prpage::title_link(
                r.pr,
                &format!("#{} {}", r.pr, esc(&r.pr_title)),
                "Link--primary"
            ),
            u = esc(&r.review_url),
            a = esc(&r.pr_author),
        );
    }
    if recent.is_empty() {
        recent = empty("No reviews in the synced data.");
    }
    out.push_str(&section(
        &format!("Last {} reviews, and what came of each", dossier::RECENT),
        None,
        None,
        &recent,
    ));
    let _ = write!(
        out,
        "<div class=\"d-flex flex-wrap gap-3\">{}{}</div>",
        overlap("Reviewed alongside", &d.co_reviewers),
        overlap("Reviewed pull requests of", &d.authors)
    );
    let _ = write!(
        out,
        "<details class=\"Box mt-4\"><summary class=\"Box-header text-bold\">Credit: {:.2}, reciprocity {:.0}% \
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
             <a href=\"{u}\">review</a> of <a href=\"/pr/{n}\">#{n}</a> {t} by <a href=\"/u/{a}\">{a}</a>\
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
    Page::new(Section::Packages, format!("Review record of {login}"))
        .heading(esc(&login))
        .icon(octicon!("person"))
        .subnav(person_tabs(&login, false))
        .sub(format!(
            "Evidence, not a verdict. Maintainer Mode grants nothing; nixpkgs committers do. \
             Reviews in the synced data: the last {} days and every open pull request.",
            ledger::WINDOW_DAYS
        ))
        .actions(listed)
        .body(out)
        .render(&app, viewer.as_deref())
}

/// The `#tags` section of `/me/settings`.
pub(crate) fn tags_box(app: &App, login: &str) -> String {
    let area = app.snap().areas.get(login).cloned().unwrap_or_default();
    format!(
        "<div id=\"tags\">{}</div>",
        crate::ui::section(
            &format!("{} Tags", icon(octicon!("tag"))),
            None,
            None,
            &format!(
                "<div class=\"Box-row f6\">For you on <a href=\"/review\">/review</a> holds {} paths you maintain, \
                 {} you own in ci/OWNERS, {} you changed this year, and your tags.</div>\
                 <div class=\"Box-row\"><form method=\"post\" action=\"/me/tags\">\
                 <label class=\"f6\" for=\"tags-list\">Path prefixes you want to see, one per line, like <code>pkgs/development/python-modules</code></label>\
                 <textarea class=\"form-control width-full my-2\" id=\"tags-list\" name=\"tags\" rows=\"3\">{}</textarea>\
                 <button class=\"btn btn-sm\" type=\"submit\">Save tags</button></form></div>",
                area.maintains.len(),
                area.owns.len(),
                area.committed.len(),
                esc(&area.tags.join("\n"))
            ),
        )
    )
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
    Redirect::to("/me/settings#tags").into_response()
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
        "/"
    } else {
        "/review"
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
         <a href=\"/state?tab=rules#the-state-of-nixpkgs\">How each number is made</a></p>\
         <div class=\"d-flex flex-wrap gap-2 mb-2\">{}{}{}{}</div>\
         <div class=\"d-flex flex-wrap gap-2 mb-4\">{}{}{}{}</div>",
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
                Some(d) =>
                    format!("median age {d} days; <a href=\"/review?tab=bumps\">the list</a>"),
                None => "<a href=\"/review?tab=bumps\">the list</a>".into(),
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
    let mut out = String::from("<div class=\"markdown-body\"><table><thead><tr>");
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

/// The Numbers tab of `/state`.
pub(crate) fn numbers(app: &App) -> String {
    let r = app.metrics.read().unwrap().clone();
    let mut body = String::new();
    let Some(at) = r.generated_at else {
        body.push_str("<div class=\"flash\">The first sync has not finished yet.</div>");
        body.push_str(&packages_section(app));
        return body;
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
        "<div class=\"d-flex flex-wrap gap-2 mb-4\">{}{}{}{}{}</div>",
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
        "<div class=\"d-flex flex-wrap gap-3 mb-4\"><div class=\"flex-1\"><h2 class=\"f4 mb-2\">Open now</h2>{}</div>\
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
        "<div class=\"d-flex flex-wrap gap-3 mt-4\"><div class=\"flex-1\"><h2 class=\"f4 mb-2\">By area</h2>{}</div>\
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
    body.push_str(&packages_section(app));
    body
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
