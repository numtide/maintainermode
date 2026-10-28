use crate::html::{Html, html};
use anyhow::Result;
use axum::Router;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::{Arc, RwLock};

use crate::area::{self, Area, Index, Ownership};
use crate::dossier;
use crate::github::OAuth;
use crate::ledger::{self, Ledger};
use crate::metrics::{self, Report};
use crate::queue::{self, Entry, Queues};
use crate::store::Store;
use crate::telemetry::Recompute;

/// Pull request pages read GitHub on a miss of the cache; crawlers must
/// not walk them.
const ROBOTS: &str = "User-agent: *\nDisallow: /pr/\nDisallow: /part/\nDisallow: /u/\nDisallow: /api/v1/prs/\nDisallow: /api/v1/people/\n";

/// The name of an Octicon that `nix/primer.nix` ships; a name it does not
/// ship fails the build.
macro_rules! octicon {
    ($name:literal) => {{
        const _: &str = include_str!(concat!(env!("PRIMER_DIR"), "/octicons/", $name, "-16.svg"));
        $name
    }};
}
pub(crate) use octicon;

/// The page adds the symbol it points to: see [`crate::assets::sprite`].
pub(crate) fn icon(name: &str) -> crate::html::Html {
    crate::html::html!(
        "<svg class=\"octicon\" width=\"16\" height=\"16\" aria-hidden=\"true\"><use href=\"#i-{name}\"></use></svg>"
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
    pub bot_token: Option<Arc<crate::secret::Secret>>,
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
    /// The git copy of the archived repositories, when this server keeps one.
    pub git_dir: Option<std::path::PathBuf>,
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
    /// What pull request pages read from GitHub on demand.
    pub pr_cache: crate::prpage::Cache,
    /// The poller of the bot account's notifications; off by default.
    pub notifications: crate::notifications::Status,
}

impl App {
    /// The archive that the rows of the main database come from.
    pub(crate) fn nixpkgs_archive(&self) -> Option<&Arc<crate::archive::Archive>> {
        self.archive
            .iter()
            .find(|a| a.repo.eq_ignore_ascii_case(crate::events::REPO))
    }

    /// The archive holds every number of nixpkgs, so the facts of every
    /// pull request are in the `history` table.
    /// Items that fail are retried and do not hold this back.
    pub(crate) fn history_complete(&self) -> bool {
        self.nixpkgs_archive()
            .is_some_and(|a| a.walked_back().unwrap_or(false))
    }

    pub fn recompute(&self) -> Result<()> {
        let start = std::time::Instant::now();
        let mut prs = self.store.prs()?;
        crate::changed::apply(&mut prs);
        let index = self.index.read().unwrap();
        let issues = self.store.issues()?;
        let links = crate::links::build(&index, &prs, &issues);
        crate::links::mark_treewide(&links, &mut prs);
        let own = Ownership::new(&index);
        let ledger = ledger::compute(&prs, |p| own.orphan(p), Utc::now());
        let areas: HashMap<String, Area> = self
            .store
            .members()?
            .iter()
            .map(|m| (m.login.clone(), Area::of(m, &index)))
            .collect();
        let queues = queue::build(&prs, &ledger, |p| own.orphan(p));
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
            issues_complete: self
                .nixpkgs_archive()
                .is_some_and(|a| a.open_complete().unwrap_or(false)),
            open_by_dir,
            versions,
            committers,
            board,
        });
        drop(index);
        self.reassess();
        crate::telemetry::recomputed(Recompute::All, start.elapsed());
        Ok(())
    }

    /// Judges the advisories again after the index or a channel moved.
    pub fn reassess(&self) {
        let ch = self.channels.read().unwrap().clone();
        let current = self.health.read().unwrap().clone();
        let index = self.index.read().unwrap();
        if !current.assessed_for(&index, &ch) {
            let mut h = (*current).clone();
            h.assess(&index, &ch);
            drop(index);
            *self.health.write().unwrap() = Arc::new(h);
        } else {
            drop(index);
        }
        self.recompute_packages();
    }

    /// Walks every package and every open pull request, so it runs when the
    /// index, the pull requests or the health data change, not on a request.
    pub fn recompute_packages(&self) {
        let start = std::time::Instant::now();
        let s = self.snap();
        let h = self.health.read().unwrap().clone();
        let p = crate::catalog::package_stats(&s, &self.index.read().unwrap(), &h);
        *self.packages.write().unwrap() = Arc::new(p);
        crate::assets::touch();
        crate::telemetry::recomputed(Recompute::Packages, start.elapsed());
    }

    /// Reads every stored pull request, so it runs after a sync pass rather
    /// than on each request.
    pub fn recompute_metrics(&self) -> Result<()> {
        let start = std::time::Instant::now();
        let facts = metrics::merge(self.store.facts()?, &self.snap().pr_data);
        *self.metrics.write().unwrap() = Arc::new(metrics::compute(&facts, Utc::now()));
        crate::assets::touch();
        crate::telemetry::recomputed(Recompute::Metrics, start.elapsed());
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
        self.store
            .session(&id)
            .ok()
            .flatten()
            .map(|s| s.login)
            .inspect(|_| crate::telemetry::saw_session())
    }

    /// The current bot token, named so that /metrics shows its budget.
    pub(crate) fn bot(&self) -> Option<String> {
        let t = self.bot_token.as_ref()?.get();
        crate::telemetry::name_token("bot", &t);
        Some(t)
    }

    /// The session, or the API token of the person.
    pub(crate) fn caller(&self, headers: &HeaderMap) -> Option<String> {
        self.viewer(headers)
            .or_else(|| crate::apitoken::login_of(self, crate::sets::bearer(headers)?))
    }
}

pub(crate) type Shared = Arc<App>;

pub fn router(app: Shared) -> Router {
    Router::new()
        .merge(crate::home::routes())
        .merge(crate::review::routes())
        .route("/record", get(record_redirect))
        .route("/me/tags", post(set_tags))
        .route("/auth/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/auth/logout", post(logout))
        .merge(crate::assets::routes())
        .route("/robots.txt", get(|| async { ROBOTS }))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/v1/events", get(api_events))
        .merge(crate::nixpkgs::routes())
        .merge(crate::api::routes())
        .merge(crate::catalog::routes())
        .merge(crate::search::routes())
        .merge(crate::profile::routes())
        .merge(crate::apitoken::routes())
        .merge(crate::sets::routes())
        .merge(crate::cli_login::routes())
        .merge(crate::progress::routes())
        .merge(crate::prpage::routes())
        .merge(crate::rescan::routes())
        .merge(crate::welcome::routes())
        .merge(crate::notify::routes())
        .merge(crate::me::routes())
        .merge(crate::needs::routes())
        .merge(crate::followup::routes())
        .layer(axum::middleware::from_fn(crate::me::no_store))
        .layer(axum::middleware::from_fn(same_origin))
        .layer(axum::middleware::from_fn(crate::assets::etag))
        .layer(axum::middleware::from_fn(crate::telemetry::track))
        .with_state(app)
}

pub(crate) fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    cookie_values(headers, name).next()
}

/// A browser can send two cookies of one name, with another path or
/// domain, the most specific first.
fn cookie_values<'a>(headers: &'a HeaderMap, name: &'a str) -> impl Iterator<Item = String> + 'a {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .filter(move |(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

/// What a person sees when a sign-in does not finish: why, and a way to
/// start again.
fn sign_in_again(app: &App, why: &str) -> Response {
    let mut res = crate::ui::Page::new(crate::ui::Section::Home, "Sign in again")
        .body(html!(
            "<div class=\"Box p-3\"><p>{why}</p>\
             <a class=\"btn btn-primary\" href=\"/auth/login\">Sign in with GitHub</a></div>"
        ))
        .render(app, None);
    *res.status_mut() = StatusCode::BAD_REQUEST;
    res
}

pub(crate) fn set_cookie(app: &App, name: &str, value: &str, max_age: i64) -> String {
    let secure = if app.secure_cookies { "; Secure" } else { "" };
    format!("{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")
}

/// The cookies of a sign-in come back on GitHub's redirect, a request that
/// another site starts, and Firefox 158 does not send a `SameSite=Lax`
/// cookie there. They hold a nonce and a path for ten minutes, never a
/// session. A browser refuses `SameSite=None` without `Secure`, so plain
/// HTTP keeps `Lax`.
fn sign_in_cookie(secure: bool, name: &str, value: &str, max_age: i64) -> String {
    let site = if secure { "None; Secure" } else { "Lax" };
    format!("{name}={value}; Path=/; HttpOnly; SameSite={site}; Max-Age={max_age}")
}

pub(crate) fn random_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
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

pub(crate) const CONTRIBUTING: &str =
    "https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md";

pub(crate) fn ci_label(e: &Entry) -> Html {
    ci_icon(e.ci.as_deref())
}

pub(crate) fn ci_icon(ci: Option<&str>) -> Html {
    match ci {
        Some("SUCCESS") => html!(
            "<span class=\"color-fg-success\" role=\"img\" title=\"checks pass\" aria-label=\"checks pass\">{}</span>",
            icon(octicon!("check-circle-fill"))
        ),
        Some("FAILURE" | "ERROR") => html!(
            "<span class=\"color-fg-danger\" role=\"img\" title=\"checks fail\" aria-label=\"checks fail\">{}</span>",
            icon(octicon!("x-circle-fill"))
        ),
        _ => html!(
            "<span class=\"color-fg-attention\" role=\"img\" title=\"checks pending\" aria-label=\"checks pending\">{}</span>",
            icon(octicon!("dot-fill"))
        ),
    }
}

/// A login, linked to its profile.
pub(crate) fn person_link(login: &str) -> Html {
    html!(
        "<a href=\"/u/{}\">{login}</a>",
        crate::github::urlencode(login)
    )
}

pub(crate) fn people(logins: &[String]) -> Html {
    crate::html::join(logins.iter().map(|l| person_link(l)), ", ")
}

/// One `Box-row` per entry, to go in a [`crate::ui::section`].
pub(crate) fn entry_rows(entries: &[&Entry]) -> crate::html::Html {
    review_rows(entries, false, |_| Default::default())
}

/// "11 months", "3 weeks": for a sentence, where `ago` is for a column.
pub(crate) fn age_words(t: DateTime<Utc>) -> String {
    let days = (Utc::now() - t).num_days().max(0);
    let (n, unit) = match days {
        0 => return "less than a day".into(),
        d if d < 14 => (d, "day"),
        d if d < 60 => (d / 7, "week"),
        d if d < 730 => (d / 30, "month"),
        d => (d / 365, "year"),
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// The mark of the nixpkgs-review runs on a pull request in a list: who
/// built it, in the title.
pub(crate) fn built_badge(m: &crate::built::Mark) -> Html {
    use crate::ui::Tone;
    let tone = if m.pushes_since > 0 {
        Tone::Neutral
    } else if m.failed.is_empty() {
        Tone::Success
    } else {
        Tone::Danger
    };
    let title = format!("nixpkgs-review by {}", m.by.join(", "));
    html!(
        "<span title=\"{title}\">{}</span>",
        crate::ui::badge(tone, Html::text(&m.words()))
    )
}

/// The mark of a treewide pull request in a list.
pub(crate) fn treewide_badge(t: &crate::links::Treewide) -> Html {
    crate::ui::badge(crate::ui::Tone::Neutral, Html::text(&t.mark()))
}

/// As [`entry_rows`], with the age as the wait when `waiting`; `extra`
/// gives each row HTML under its meta line and after its buttons.
pub(crate) fn review_rows(
    entries: &[&Entry],
    waiting: bool,
    extra: impl Fn(&Entry) -> (Html, Html),
) -> Html {
    use crate::ui::{Kind, Tone, badge, item_row};
    let mut out = Html::new();
    for e in entries {
        let mut badges = Vec::new();
        if e.mergeable == "CONFLICTING" {
            badges.push(badge(Tone::Danger, "conflict"));
        }
        if e.unowned {
            badges.push(badge(Tone::Attention, "no maintainer"));
        }
        badges.extend(e.treewide.as_ref().map(treewide_badge));
        badges.extend(e.built.as_ref().map(built_badge));
        if let Some(t) = &e.too_large {
            badges.push(badge(Tone::Attention, Html::text(&t.mark())));
        }
        let age = if waiting {
            html!(
                "<span class=\"color-fg-attention text-bold\">waiting {}</span>",
                age_words(e.created_at)
            )
        } else {
            html!("{} old", ago(e.created_at))
        };
        let mut meta = html!(
            "#{} by <a href=\"/u/{a}\">{a}</a> · {age} · <span class=\"branch-name\">{}</span>",
            e.number,
            e.base,
            a = e.author
        );
        if !e.approved_by.is_empty() {
            meta.push(&html!(" · approved by {}", people(&e.approved_by)));
        }
        let (why, end) = extra(e);
        meta.push(&why);
        out.push(&item_row(
            Some(Kind::PR_OPEN),
            crate::prpage::title_link(e.number, Html::text(&e.title), "Link--primary text-bold"),
            "",
            meta,
            &badges,
            html!("<span class=\"ml-2\">{}</span>{end}", ci_label(e)),
        ));
    }
    out
}

/// `mine` is the old name of `foryou`.
pub(crate) fn bucket_name(b: &str) -> Option<&'static str> {
    match b {
        "foryou" | "mine" => Some("foryou"),
        "unowned" => Some("unowned"),
        "firsttime" => Some("firsttime"),
        "all" => Some("all"),
        _ => None,
    }
}

/// What makes a pull request "for you": an area of the person (what they
/// maintain, own, changed or tagged), or what they watch.
pub(crate) struct Interest<'a> {
    mine: Option<Area>,
    /// pull request → the names of the watched subjects that link to it.
    prs: std::collections::HashMap<u64, Vec<String>>,
    paths: Vec<&'a str>,
}

impl<'a> Interest<'a> {
    /// `own` is the person's subscriptions, given only when the person
    /// asks for their own queue; it also opens their private tags.
    pub(crate) fn of(s: &Snapshot, viewer: Option<&str>, own: Option<&'a [String]>) -> Self {
        let mine = viewer
            .and_then(|v| s.areas.get(v))
            .map(|a| if own.is_some() { a.clone() } else { a.public() });
        let mut prs: std::collections::HashMap<u64, Vec<String>> = Default::default();
        let mut paths: Vec<&str> = Vec::new();
        for key in own.unwrap_or_default() {
            match crate::catalog::Subject::parse(key) {
                Some(crate::catalog::Subject::Area(_)) => {
                    paths.extend(key.strip_prefix("area:"));
                }
                Some(sub) => {
                    for n in s.links.prs.get(key).into_iter().flatten() {
                        prs.entry(*n).or_default().push(sub.label());
                    }
                }
                None => {}
            }
        }
        Interest { mine, prs, paths }
    }

    pub(crate) fn maintains(&self, e: &Entry) -> bool {
        self.mine.as_ref().is_some_and(|a| a.covers(&e.files))
    }

    pub(crate) fn watches(&self, e: &Entry) -> bool {
        self.prs.contains_key(&e.number)
            || e.files
                .iter()
                .any(|f| self.paths.iter().any(|p| area::under(f, p)))
    }

    /// Why `e` is for the person, one line per kind of tie, most direct
    /// first: "you maintain hello", "you watch nginx".
    pub(crate) fn why(&self, e: &Entry) -> Vec<String> {
        let hit = |prefixes: &[String]| -> Vec<String> {
            prefixes
                .iter()
                .filter(|p| e.files.iter().any(|f| area::under(f, p)))
                .map(|p| short_path(p))
                .collect()
        };
        let mut out = Vec::new();
        if let Some(a) = &self.mine {
            for (what, list) in [
                ("you maintain", hit(&a.maintains)),
                ("you own in ci/OWNERS", hit(&a.owns)),
                ("you changed this year", hit(&a.committed)),
                ("your tag", hit(&a.tags)),
            ] {
                if !list.is_empty() {
                    out.push(format!("{what}: {}", some_of(list)));
                }
            }
        }
        let mut watched: Vec<String> = self.prs.get(&e.number).cloned().unwrap_or_default();
        watched.extend(
            self.paths
                .iter()
                .filter(|p| e.files.iter().any(|f| area::under(f, p)))
                .map(|p| format!("{p}/")),
        );
        if !watched.is_empty() {
            out.push(format!("you watch: {}", some_of(watched)));
        }
        out
    }
}

/// `pkgs/by-name/he/hello/package.nix` is "hello"; other paths stay, less
/// a trailing `default.nix` or `package.nix`.
pub(crate) fn short_path(p: &str) -> String {
    let p = p
        .trim_end_matches("/default.nix")
        .trim_end_matches("/package.nix");
    match p.split_once("pkgs/by-name/") {
        Some((_, rest)) => rest.split('/').nth(1).unwrap_or(rest).to_string(),
        None => p.to_string(),
    }
}

/// "a, b and 3 more".
pub(crate) fn some_of(mut list: Vec<String>) -> String {
    const SHOWN: usize = 2;
    list.dedup();
    let more = list.len().saturating_sub(SHOWN);
    list.truncate(SHOWN);
    let mut out = list.join(", ");
    if more > 0 {
        let _ = write!(out, " and {more} more");
    }
    out
}

pub(crate) fn review_bucket<'a>(
    s: &'a Snapshot,
    viewer: Option<&str>,
    own: Option<&[String]>,
    bucket: &str,
) -> Vec<&'a Entry> {
    let interest = Interest::of(s, viewer, own);
    let name = bucket_name(bucket);
    let list = if name == Some("firsttime") {
        &s.queues.firsttime
    } else {
        &s.queues.review
    };
    list.iter()
        .filter(|e| Some(e.author.as_str()) != viewer)
        .filter(|e| match name {
            Some("foryou") => interest.maintains(e) || interest.watches(e),
            Some("unowned") => e.unowned,
            _ => true,
        })
        .collect()
}

pub(crate) fn stat(n: usize, label: &str, hint: impl Into<Html>) -> Html {
    let hint = hint.into();
    html!(
        "<div class=\"Box p-3 flex-1\"><div class=\"f1 text-bold\">{n}</div><div class=\"text-bold\">{label}</div>\
         <div class=\"f6 color-fg-muted\">{hint}</div></div>"
    )
}

/// The `#tags` section of `/me/settings`.
pub(crate) fn tags_box(app: &App, login: &str) -> Html {
    let area = app.snap().areas.get(login).cloned().unwrap_or_default();
    html!(
        "<p class=\"mb-2\">For you on <a href=\"/review\">/review</a> holds {} paths you maintain, \
         {} you own in ci/OWNERS, {} you changed this year, and your tags.</p>\
         <form method=\"post\" action=\"/me/tags\">\
         <label class=\"f6\" for=\"tags-list\">Your tags: path prefixes, one per line, such as <code>pkgs/development/python-modules</code></label>\
         <textarea class=\"form-control width-full my-2\" id=\"tags-list\" name=\"tags\" rows=\"3\">{}</textarea>\
         <button class=\"btn btn-sm\" type=\"submit\">Save tags</button></form>",
        area.maintains.len(),
        area.owns.len(),
        area.committed.len(),
        area.tags.join("\n")
    )
}

#[derive(Deserialize)]
struct RecordQuery {
    login: Option<String>,
}

async fn record_redirect(Query(q): Query<RecordQuery>) -> Redirect {
    match q
        .login
        .as_deref()
        .map(str::trim)
        .filter(|l| crate::catalog::valid_login(l))
    {
        Some(l) => Redirect::permanent(&format!("/u/{}", crate::github::urlencode(l))),
        None => Redirect::permanent("/packages"),
    }
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

#[derive(Deserialize)]
struct LoginQuery {
    next: Option<String>,
}

/// A path on this site, so a sign-in or a form cannot send a person
/// elsewhere. The allow-list also keeps out what makes `Redirect::to` panic.
pub(crate) fn local_path(next: &str) -> bool {
    next.starts_with('/')
        && !next.starts_with("//")
        && next
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/?=&#_.-%+".contains(&b))
}

async fn login(State(app): State<Shared>, Query(q): Query<LoginQuery>) -> Response {
    let Some(oauth) = &app.oauth else {
        return (StatusCode::SERVICE_UNAVAILABLE, "login is not configured").into_response();
    };
    let state = random_id();
    let mut h = HeaderMap::new();
    h.append(
        header::SET_COOKIE,
        sign_in_cookie(app.secure_cookies, "oauth_state", &state, 600)
            .parse()
            .unwrap(),
    );
    if let Some(next) = q.next.as_deref().filter(|n| local_path(n)) {
        h.append(
            header::SET_COOKIE,
            sign_in_cookie(app.secure_cookies, "next", next, 600)
                .parse()
                .unwrap(),
        );
    }
    (h, Redirect::to(&oauth.authorize_url(&state))).into_response()
}

#[derive(Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    /// Set when the person cancels on GitHub.
    error: Option<String>,
}

async fn callback(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(cb): Query<Callback>,
) -> Response {
    let Some(oauth) = &app.oauth else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if cb.error.is_some() {
        return Redirect::to("/").into_response();
    }
    let sent: Vec<String> = cookie_values(&headers, "oauth_state").collect();
    let (Some(code), Some(state)) = (cb.code.as_deref(), cb.state.as_deref()) else {
        return sign_in_again(&app, "GitHub did not send a code back.");
    };
    if !sent.iter().any(|s| s == state) {
        let agent = headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        tracing::info!(
            "sign-in: the state cookie is {}, {agent}",
            if sent.is_empty() {
                "missing"
            } else {
                "different"
            }
        );
        return sign_in_again(
            &app,
            "The sign-in did not finish: this browser did not send back its sign-in cookie, or the sign-in took more than 10 minutes.",
        );
    }
    let (login, token) = match oauth.exchange(code).await {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    };
    let id = random_id();
    if let Err(e) = app.store.create_session(&id, &login, &token) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    let next = match cookie(&headers, "next").filter(|n| local_path(n)) {
        Some(n) => n,
        None if crate::welcome::first_visit(&app, &login) => "/".into(),
        None => "/review".into(),
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
        sign_in_cookie(app.secure_cookies, "oauth_state", "", 0)
            .parse()
            .unwrap(),
    );
    h.append(
        header::SET_COOKIE,
        sign_in_cookie(app.secure_cookies, "next", "", 0)
            .parse()
            .unwrap(),
    );
    (h, Redirect::to(&next)).into_response()
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

pub(crate) fn status_value(app: &App) -> serde_json::Value {
    let s = app.snap();
    json!(Status {
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

pub(crate) const NIXPKGS: &str = "https://github.com/NixOS/nixpkgs/blob/master";

/// The rules below say what this site shows; how to work on
/// nixpkgs is nixpkgs' own guide.
pub(crate) fn nixpkgs_guides() -> Html {
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
    let items: Html = links
        .iter()
        .map(|(file, what)| html!("<li><a href=\"{NIXPKGS}/{file}\">{file}</a>: {what}</li>"))
        .collect();
    html!(
        "<div class=\"flash mb-4\" id=\"contributor-guide\"><strong>These are the rules of Maintainer Mode, not of nixpkgs.</strong> \
         nixpkgs decides how the work is done. Read its contributor guide first:\
         <ul class=\"ml-4 mt-2\">{items}<li><a href=\"https://nixos.org/manual/nixpkgs/unstable/\">The Nixpkgs manual</a>: the reference</li></ul></div>"
    )
}

fn package_stats(app: &App) -> Arc<crate::catalog::PackageStats> {
    app.packages.read().unwrap().clone()
}

pub(crate) fn metrics_value(app: &App) -> serde_json::Value {
    let mut v = serde_json::to_value(Report::clone(&app.metrics.read().unwrap()))
        .unwrap_or_else(|_| json!({}));
    v["packages"] = json!(*package_stats(app));
    v
}

pub(crate) fn packages_section(app: &App) -> Html {
    let p = package_stats(app);
    let mut out = Html::from("<h2 class=\"f4 mt-4 mb-2\" id=\"packages\">Packages</h2>");
    if p.rev.is_empty() {
        out.push(&Html::from(
            "<div class=\"flash\">The first nixpkgs index is still being built. These numbers come from it.</div>",
        ));
        return out;
    }
    let b = &p.advisories_by_band;
    out.push(&html!(
        "<p class=\"f6 color-fg-muted\">One package per package name, at nixpkgs {}. \
         <a href=\"/state?tab=rules#the-state-of-nixpkgs\">How each number is made</a></p>\
         <div class=\"d-flex flex-wrap gap-2 mb-2\">{}{}{}{}</div>\
         <div class=\"d-flex flex-wrap gap-2 mb-4\">{}{}{}{}{}</div>",
        &p.rev[..p.rev.len().min(12)],
        stat(p.packages, "packages", "in the swept package sets"),
        stat(
            p.behind_upstream,
            "behind upstream",
            "a newer release in its git tags, its registry or repology"
        ),
        stat(
            p.with_advisories,
            "with open advisories",
            html!(
                "that can affect master; worst: {} critical, {} high, {} medium, {} low, {} with no score; \
                 {} more only in a channel",
                b.critical,
                b.high,
                b.medium,
                b.low,
                b.unscored,
                p.fixed_in_master_only
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
            match p.bumps_median_age_days {
                Some(d) => html!("median age {d} days; <a href=\"/review?tab=bumps\">the list</a>"),
                None => "<a href=\"/review?tab=bumps\">the list</a>".into(),
            }
        ),
        stat(
            p.tracker_issues_to_close.len(),
            "tracker issues to close",
            "fixed in master and every channel; <a href=\"#tracker-issues-to-close\">the list</a>"
        ),
    ));
    out.push(&maintainers_section(&p.maintainers));
    out.push(&to_close_section(&p.tracker_issues_to_close));
    out
}

/// It names nobody: the load of one person is a score.
fn maintainers_section(m: &crate::load::Spread) -> Html {
    if m.maintainers == 0 {
        return Html::new();
    }
    let cost: Html = match m.since {
        Some(t) => html!(
            "1, plus the commits on master since {} that changed its file or directory",
            t.format("%Y-%m-%d").to_string()
        ),
        None => "1: this server has no git copy to count commits".into(),
    };
    let rows: Vec<crate::chart::BarRow> = m
        .buckets
        .iter()
        .map(|b| crate::chart::BarRow {
            label: b.maintains.to_string(),
            value: b.maintainers.unwrap_or(0) as f64,
            tip: b
                .maintainers
                .map_or_else(|| "under 3".into(), |n| html!("{n}")),
            cells: vec![Html::text(&pct(b.load_share))],
        })
        .collect();
    let median = m
        .median_load
        .map_or_else(Html::new, |l| html!("; the median load is {l:.1}"));
    let half = m.half_load_maintainers.map_or_else(Html::new, |n| {
        stat(
            n,
            "carry half of the load",
            html!(
                "{} of the maintainers",
                pct(Some(n as f64 / m.maintainers as f64))
            ),
        )
    });
    html!(
        "<h2 class=\"f4 mt-4 mb-2\" id=\"maintainers\">Maintainers</h2>\
         <div class=\"d-flex flex-wrap gap-2 mb-2\">{}{}{half}</div>{}",
        stat(
            m.maintainers,
            "maintainers",
            html!("people with a package or a module, team members included{median}")
        ),
        stat(
            m.by_maintainers[1],
            "with one maintainer",
            html!(
                "packages and modules; {} have two, {} three or more, {} none",
                m.by_maintainers[2],
                m.by_maintainers[3],
                m.by_maintainers[0]
            )
        ),
        crate::chart::bars(
            "maintainer-load",
            "Packages and modules per maintainer",
            html!(
                "The load of a package or a module is {cost}. Its maintainers share it. No page shows the load of one person."
            ),
            &["Packages and modules", "Maintainers", "Share of the load"],
            &rows
        )
    )
}

fn to_close_section(list: &[crate::catalog::ToClose]) -> Html {
    let mut out = Html::from(
        "<h2 class=\"f4 mt-4 mb-2\" id=\"tracker-issues-to-close\">Tracker issues to close</h2>\
         <p class=\"f6 color-fg-muted\">The nixpkgs issue of these security tracker advisories is still open, \
         but master and every channel read already ship a version outside the affected range. \
         Check the fix, then close the issue. <code>GET /api/v1/metrics</code> gives the list under \
         <code>packages.tracker_issues_to_close</code>.</p>",
    );
    if list.is_empty() {
        out.push(&Html::from("<p class=\"color-fg-muted\">None.</p>"));
        return out;
    }
    let rows: Vec<Vec<Html>> = list
        .iter()
        .map(|t| {
            vec![
                html!("<a href=\"/p/{0}\">{0}</a>", t.attr),
                html!(
                    "<a href=\"https://tracker.security.nixos.org/issue/{0}\">{0}</a> {1}",
                    t.code,
                    t.title
                ),
                Html::text(t.fixed_in.as_deref().unwrap_or("–")),
                Html::text(&t.version),
                html!(
                    "<a href=\"https://github.com/NixOS/nixpkgs/issues/{0}\">#{0}</a>",
                    t.issue
                ),
            ]
        })
        .collect();
    out.push(&table(
        &["package", "advisory", "fixed in", "master", "issue"],
        &rows,
    ));
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

pub(crate) fn num(v: Option<f64>) -> String {
    v.map_or_else(|| "–".into(), |v| format!("{v}"))
}

pub(crate) fn pct(v: Option<f64>) -> String {
    v.map_or_else(|| "–".into(), |v| format!("{:.0}%", v * 100.0))
}

pub(crate) fn table(head: &[&str], rows: &[Vec<Html>]) -> Html {
    let mut out = Html::from("<div class=\"markdown-body\"><table class=\"numbers\"><thead><tr>");
    for h in head {
        out.push(&html!("<th>{h}</th>"));
    }
    out.push(&Html::from("</tr></thead><tbody>"));
    for r in rows {
        out.push(&Html::from("<tr>"));
        for c in r {
            out.push(&html!("<td>{c}</td>"));
        }
        out.push(&Html::from("</tr>"));
    }
    out.push(&Html::from("</tbody></table></div>"));
    out
}

pub(crate) fn cell(v: impl ToString) -> Html {
    Html::text(&v.to_string())
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
    fn sign_in_cookies_come_back_on_a_redirect_from_another_site() {
        let https = sign_in_cookie(true, "oauth_state", "abc", 600);
        assert!(https.contains("SameSite=None; Secure"), "{https}");
        let http = sign_in_cookie(false, "oauth_state", "abc", 600);
        assert!(
            http.contains("SameSite=Lax") && !http.contains("Secure"),
            "{http}"
        );
        let mut h = HeaderMap::new();
        h.append(header::COOKIE, "oauth_state=old; mood=all".parse().unwrap());
        h.append(header::COOKIE, "oauth_state=new".parse().unwrap());
        let sent: Vec<String> = cookie_values(&h, "oauth_state").collect();
        assert_eq!(sent, ["old", "new"]);
        assert_eq!(cookie(&h, "mood").as_deref(), Some("all"));
    }

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
            built: None,
            files: vec![file.into()],
            unowned,
            labels: vec![],
            treewide: None,
            too_large: None,
        }
    }

    #[test]
    fn a_row_marks_who_built_it_and_greys_a_run_behind_the_head() {
        let mark = |failed: &[&str], pushes_since: usize| crate::built::Mark {
            ok: vec!["x86_64-linux".into(), "aarch64-linux".into()],
            failed: failed.iter().map(|s| s.to_string()).collect(),
            by: vec!["alice".into(), "bob".into()],
            pushes_since,
            weight: crate::built::weight(pushes_since),
        };
        let mut e = entry(1, "x", "lib/strings.nix", false);
        assert!(!entry_rows(&[&e]).contains("nixpkgs-review"));
        e.built = Some(mark(&[], 0));
        let html = entry_rows(&[&e]);
        assert!(html.contains(
            "<span title=\"nixpkgs-review by alice, bob\"><span class=\"Label Label--success\">built on 2 systems</span></span>"
        ), "{html}");
        e.built = Some(mark(&["aarch64-darwin"], 0));
        assert!(
            entry_rows(&[&e])
                .contains("<span class=\"Label Label--danger\">fails on aarch64-darwin</span>")
        );
        e.built = Some(mark(&["aarch64-darwin"], 2));
        assert!(
            entry_rows(&[&e])
                .contains("<span class=\"Label\">fails on aarch64-darwin 2 pushes ago</span>")
        );
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
            numbers(review_bucket(&s, Some("bob"), Some(&watched), "foryou")),
            vec![1, 2, 3]
        );
        assert_eq!(
            numbers(review_bucket(&s, Some("bob"), Some(&[]), "mine")),
            vec![1]
        );
        assert_eq!(
            numbers(review_bucket(&s, Some("bob"), Some(&watched), "unowned")),
            vec![4]
        );
        assert_eq!(numbers(review_bucket(&s, None, None, "all")).len(), 5);
        s.queues
            .review
            .push(entry(6, "x", "pkgs/by-name/jq/jq/package.nix", false));
        s.links.prs.insert("pkg:jq".into(), vec![6]);
        let machines: std::collections::HashSet<String> =
            ["pkg:jq".to_string(), "pkg:curl".to_string()].into();
        assert_eq!(
            numbers(crate::mine::rank_prs(&s, "bob", &watched, &machines)),
            vec![2, 6, 1, 3]
        );
        assert_eq!(
            numbers(crate::mine::rank_prs(
                &s,
                "bob",
                &watched,
                &Default::default()
            )),
            vec![1, 2, 3]
        );
        let why: std::collections::HashMap<u64, Vec<String>> =
            crate::mine::rank_prs_why(&s, "bob", &watched, &machines)
                .into_iter()
                .map(|(e, w)| (e.number, w))
                .collect();
        assert_eq!(why[&1], ["you maintain: hello"]);
        assert_eq!(why[&2], ["on your machines: curl", "you watch: curl"]);
        assert_eq!(why[&3], ["you watch: nixos/modules/services/web-servers/"]);
        assert_eq!(why[&6], ["on your machines: jq"]);
    }

    #[test]
    fn treewide_pull_requests_come_last_in_for_you_in_the_same_order() {
        let mut s = Snapshot::default();
        let wide = crate::links::Treewide::of(340, 0);
        let hello = "pkgs/by-name/he/hello/package.nix";
        s.queues.review = vec![
            entry(1, "x", hello, false),
            entry(2, "x", "pkgs/by-name/cu/curl/package.nix", false),
            entry(3, "x", hello, false),
            entry(4, "x", "pkgs/by-name/jq/jq/package.nix", false),
        ];
        s.queues.review[0].treewide = wide;
        s.queues.review[1].treewide = wide;
        s.areas.insert(
            "bob".into(),
            Area {
                maintains: vec!["pkgs/by-name/he/hello".into()],
                ..Default::default()
            },
        );
        s.links.prs.insert("pkg:curl".into(), vec![2]);
        s.links.prs.insert("pkg:jq".into(), vec![4]);
        let machines: std::collections::HashSet<String> =
            ["pkg:jq".to_string(), "pkg:curl".to_string()].into();
        let ranked = crate::mine::rank_prs(&s, "bob", &[], &machines);
        let numbers: Vec<u64> = ranked.iter().map(|e| e.number).collect();
        assert_eq!(numbers, [4, 3, 2, 1]);
        let rows = entry_rows(&ranked).into_string();
        assert_eq!(rows.matches("treewide · 340 packages").count(), 2);
        assert!(rows.find("pr 3") < rows.find("treewide"));
    }

    #[test]
    fn a_pull_request_too_large_to_link_reaches_nobody_and_stays_in_the_queue() {
        let mut p = crate::ledger::tests::pr(7, "x", 0);
        p.files = vec!["pkgs/by-name/he/hello/package.nix".into()];
        p.changed_files = 19_485;
        let e = crate::queue::entry(&p, &Ledger::default(), &Default::default());
        assert!(e.files.is_empty());
        let mut s = Snapshot::default();
        s.queues.review = vec![e];
        s.areas.insert(
            "bob".into(),
            Area {
                maintains: vec!["pkgs/by-name/he/hello".into()],
                ..Default::default()
            },
        );
        let watched = vec!["area:pkgs/by-name".to_string()];
        assert!(review_bucket(&s, Some("bob"), Some(&watched), "foryou").is_empty());
        let all = review_bucket(&s, None, None, "all");
        assert!(
            entry_rows(&all)
                .into_string()
                .contains("too large · 19,485 files")
        );
        let v = serde_json::to_value(all[0]).unwrap();
        assert_eq!(v["too_large"]["files"], 19_485);
        assert!(v["treewide"].is_null());
    }

    #[test]
    fn paths_get_short_names() {
        assert_eq!(short_path("pkgs/by-name/he/hello/package.nix"), "hello");
        assert_eq!(short_path("pkgs/by-name/he/hello"), "hello");
        assert_eq!(
            short_path("pkgs/development/python-modules/requests/default.nix"),
            "pkgs/development/python-modules/requests"
        );
        assert_eq!(some_of(vec!["a".into()]), "a");
        assert_eq!(
            some_of(vec!["a".into(), "b".into(), "c".into(), "d".into()]),
            "a, b and 2 more"
        );
    }

    #[test]
    fn the_queue_of_someone_else_ignores_their_tags() {
        let mut s = Snapshot::default();
        s.queues.review = vec![
            entry(1, "x", "pkgs/by-name/he/hello/package.nix", false),
            entry(2, "x", "lib/strings.nix", false),
        ];
        s.areas.insert(
            "bob".into(),
            Area {
                maintains: vec!["pkgs/by-name/he/hello".into()],
                tags: vec!["lib".into()],
                ..Default::default()
            },
        );
        let numbers = |v: Vec<&Entry>| v.iter().map(|e| e.number).collect::<Vec<_>>();
        assert_eq!(
            numbers(review_bucket(&s, Some("bob"), Some(&[]), "foryou")),
            vec![1, 2]
        );
        assert_eq!(
            numbers(review_bucket(&s, Some("bob"), None, "foryou")),
            vec![1]
        );
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

    #[test]
    fn package_numbers_come_from_the_last_recompute() {
        let app = crate::sets::tests::app(&["hello"]);
        app.index
            .write()
            .unwrap()
            .by_pname
            .insert("hello".into(), "hello".into());
        let packages = |v: serde_json::Value| v["packages"]["packages"].clone();
        assert_eq!(packages(metrics_value(&app)), 0);
        app.recompute().unwrap();
        assert_eq!(packages(metrics_value(&app)), 1);
        app.index.write().unwrap().by_pname.clear();
        assert_eq!(packages(metrics_value(&app)), 1);
    }
}

#[cfg(test)]
pub(crate) mod ux_pages {
    const LIST: &str = include_str!("../nix/ux-pages.txt");

    pub(crate) fn get_routes() -> Vec<String> {
        let mut dirs = vec![std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src"
        ))];
        let mut files = Vec::new();
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push(path);
                }
            }
        }
        let mut routes = Vec::new();
        for path in files {
            let src = std::fs::read_to_string(&path).unwrap();
            let src = src.split("#[cfg(test)]").next().unwrap();
            for call in src.split(".route(").skip(1) {
                let call = call.trim_start();
                let Some(rest) = call.strip_prefix('"') else {
                    continue;
                };
                let (route, rest) = rest.split_once('"').unwrap();
                let handler = rest.trim_start().trim_start_matches(',').trim_start();
                if handler.starts_with("get(") {
                    routes.push(route.to_string());
                }
            }
        }
        routes.extend(crate::api::served().map(|e| e.path.to_string()));
        routes.sort();
        routes.dedup();
        routes
    }

    pub(crate) fn matches(route: &str, path: &str) -> bool {
        let mut want = route.trim_start_matches('/').split('/');
        let mut have = path.trim_start_matches('/').split('/');
        loop {
            match (want.next(), have.next()) {
                (None, None) => return true,
                (Some(w), Some(h)) if w.starts_with('*') => return !h.is_empty(),
                (Some(w), Some(h)) if w.starts_with(':') => {
                    if h.is_empty() {
                        return false;
                    }
                }
                (Some(w), Some(h)) if w == h => {}
                _ => return false,
            }
        }
    }

    pub(crate) struct Entries {
        pages: Vec<String>,
        pub(crate) skips: Vec<String>,
    }

    pub(crate) fn entries() -> Entries {
        let mut vars = std::collections::HashMap::new();
        let mut pages = Vec::new();
        let mut skips = Vec::new();
        for line in LIST.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let words: Vec<&str> = line.split_whitespace().collect();
            match words[..] {
                ["set", name, value] => {
                    vars.insert(name, value);
                }
                ["skip", route] => skips.push(route.to_string()),
                [kind, path, ..] if ["public", "signed-in", "both"].contains(&kind) => {
                    let mut path = path.split('?').next().unwrap().to_string();
                    for (name, value) in &vars {
                        path = path.replace(&format!("${{{name}}}"), value);
                    }
                    assert!(!path.contains("${"), "unknown variable in {line}");
                    pages.push(path);
                }
                _ => panic!("nix/ux-pages.txt: cannot read {line:?}"),
            }
        }
        Entries { pages, skips }
    }

    #[test]
    fn route_matching_follows_axum() {
        assert!(matches("/u/:login", "/u/zimbatm"));
        assert!(!matches("/u/:login", "/u/zimbatm/maintains"));
        assert!(matches("/m/*file", "/m/nixos/modules/a.nix"));
        assert!(!matches("/m/*file", "/m/"));
        assert!(matches("/", "/"));
    }

    #[test]
    fn every_page_route_is_in_the_audit_list() {
        let routes = get_routes();
        assert!(routes.len() > 30, "found only {routes:?}");
        let e = entries();
        for route in &routes {
            let pages = e.pages.iter().filter(|p| matches(route, p)).count();
            let skipped = e.skips.contains(route);
            assert!(
                pages > 0 || skipped,
                "GET {route} is not in nix/ux-pages.txt: add a page or a skip line"
            );
            assert!(
                !(pages > 0 && skipped),
                "GET {route} is both a page and a skip in nix/ux-pages.txt"
            );
        }
        for page in &e.pages {
            assert!(
                routes.iter().any(|r| matches(r, page)),
                "{page} in nix/ux-pages.txt matches no route"
            );
        }
        for skip in &e.skips {
            assert!(
                routes.contains(skip),
                "skip {skip} in nix/ux-pages.txt is not a route"
            );
        }
    }
}
