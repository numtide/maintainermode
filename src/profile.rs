//! `/u/<login>`: the public profile of one person. It shows facts from
//! public data. There is deliberately no score and no ranking of people: a
//! leaderboard drives volume, and a global order lets five people dominate
//! while the rest disengage. Private data goes under `/me`.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;

use crate::dossier::{self, Outcome};
use crate::ledger;
use crate::model::Pr;
use crate::ui::{Kind, Page, Section, Tab, Tone, badge, empty, item_row, note, pager, section};
use crate::web::{App, Shared, Snapshot, ago, ci_icon, esc, icon, octicon, stat};

/// A pull request with no review this long waits for a first review.
pub(crate) const WAITING_DAYS: i64 = 7;
const RECENT_DAYS: i64 = 60;
const RECENT_MAX: usize = 20;
const AREAS_DAYS: i64 = 365;
const AREAS_MAX: usize = 8;
const PAGE: usize = 25;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/u/:login", get(page))
        .route("/u/:login/maintains", get(old_maintains))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ProfileTab {
    Overview,
    Reviews,
    Maintains,
}

impl ProfileTab {
    const ALL: [ProfileTab; 3] = [Self::Overview, Self::Reviews, Self::Maintains];

    fn parse(s: Option<&str>) -> Self {
        match s {
            Some("reviews" | "record") => Self::Reviews,
            Some("maintains") => Self::Maintains,
            _ => Self::Overview,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Reviews => "reviews",
            Self::Maintains => "maintains",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Reviews => "Reviews",
            Self::Maintains => "Maintains",
        }
    }

    fn href(self, login: &str) -> String {
        let base = format!("/u/{}", crate::github::urlencode(login));
        match self {
            Self::Overview => base,
            t => format!("{base}?tab={}", t.key()),
        }
    }
}

fn tabs(login: &str, current: ProfileTab) -> Vec<Tab> {
    ProfileTab::ALL
        .iter()
        .map(|t| Tab::new(t.href(login), t.label(), *t == current))
        .collect()
}

async fn old_maintains(Path(login): Path<String>) -> Redirect {
    Redirect::permanent(&ProfileTab::Maintains.href(&login))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReviewState {
    Draft,
    NoReview,
    ChangesRequested,
    Approved,
    Reviewed,
}

impl ReviewState {
    fn label(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::NoReview => "no review yet",
            Self::ChangesRequested => "changes requested",
            Self::Approved => "approved",
            Self::Reviewed => "reviewed",
        }
    }

    fn badge(self) -> String {
        let tone = match self {
            Self::Approved => Tone::Success,
            _ => Tone::Neutral,
        };
        badge(tone, self.label())
    }
}

/// The last approval or change request of each reviewer counts; a comment
/// does not undo either, as on GitHub.
pub(crate) fn review_state(p: &Pr) -> ReviewState {
    if p.is_draft {
        return ReviewState::Draft;
    }
    let mut reviews: Vec<_> = p
        .reviews
        .iter()
        .filter(|r| !r.author_is_bot && !r.author.eq_ignore_ascii_case(&p.author))
        .filter(|r| r.state != "DISMISSED")
        .collect();
    if reviews.is_empty() {
        return ReviewState::NoReview;
    }
    reviews.sort_by_key(|r| r.submitted_at);
    let mut last: HashMap<String, &str> = HashMap::new();
    for r in reviews {
        let e = last.entry(r.author.to_lowercase()).or_insert("COMMENTED");
        if r.state != "COMMENTED" {
            *e = r.state.as_str();
        }
    }
    if last.values().any(|s| *s == "CHANGES_REQUESTED") {
        ReviewState::ChangesRequested
    } else if last.values().any(|s| *s == "APPROVED") {
        ReviewState::Approved
    } else {
        ReviewState::Reviewed
    }
}

pub(crate) fn waiting(p: &Pr, now: DateTime<Utc>) -> bool {
    review_state(p) == ReviewState::NoReview && now - p.created_at >= Duration::days(WAITING_DAYS)
}

fn by(p: &Pr, login: &str) -> bool {
    p.author.eq_ignore_ascii_case(login)
}

/// The ones waiting for a first review come first, the longest wait first;
/// then the others, the newest first.
pub(crate) fn open_prs<'a>(prs: &'a [Pr], login: &str, now: DateTime<Utc>) -> Vec<&'a Pr> {
    let mut out: Vec<(&Pr, bool)> = prs
        .iter()
        .filter(|p| p.is_open() && by(p, login))
        .map(|p| (p, waiting(p, now)))
        .collect();
    out.sort_by(|(a, wa), (b, wb)| match (wa, wb) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (true, true) => a
            .created_at
            .cmp(&b.created_at)
            .then(a.number.cmp(&b.number)),
        (false, false) => b
            .created_at
            .cmp(&a.created_at)
            .then(b.number.cmp(&a.number)),
    });
    out.into_iter().map(|(p, _)| p).collect()
}

fn done_at(p: &Pr) -> Option<DateTime<Utc>> {
    p.merged_at.or(p.closed_at)
}

pub(crate) fn recent_prs<'a>(prs: &'a [Pr], login: &str, now: DateTime<Utc>) -> Vec<&'a Pr> {
    let since = now - Duration::days(RECENT_DAYS);
    let mut out: Vec<&Pr> = prs
        .iter()
        .filter(|p| !p.is_open() && by(p, login))
        .filter(|p| done_at(p).is_some_and(|t| t >= since))
        .collect();
    out.sort_by_key(|p| std::cmp::Reverse(done_at(p)));
    out.truncate(RECENT_MAX);
    out
}

/// The package directory of a `default.nix` or `package.nix`, the parent
/// directory of any other file.
fn dir_of(file: &str) -> Option<String> {
    let key = crate::area::area_key(file);
    if key == file {
        file.rsplit_once('/').map(|(d, _)| d.to_string())
    } else {
        Some(key)
    }
}

/// Each pull request counts once per directory.
pub(crate) fn top_areas(prs: &[Pr], login: &str, now: DateTime<Utc>) -> Vec<(String, usize)> {
    let since = now - Duration::days(AREAS_DAYS);
    let mut count: HashMap<String, usize> = HashMap::new();
    for p in prs.iter().filter(|p| by(p, login) && p.created_at >= since) {
        let dirs: BTreeSet<String> = p.files.iter().filter_map(|f| dir_of(f)).collect();
        for d in dirs {
            *count.entry(d).or_default() += 1;
        }
    }
    let mut out: Vec<(String, usize)> = count.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.truncate(AREAS_MAX);
    out
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Facts {
    pub committer: bool,
    pub packages: usize,
    pub modules: usize,
    pub teams: Vec<String>,
    pub first_pr_at: Option<DateTime<Utc>>,
}

pub(crate) fn facts(app: &App, s: &Snapshot, login: &str) -> Facts {
    let l = login.to_lowercase();
    let index = app.index.read().unwrap();
    let m = crate::catalog::maintained(&index, login);
    let teams = index
        .teams
        .iter()
        .filter(|(_, members)| members.iter().any(|x| x.eq_ignore_ascii_case(&l)))
        .map(|(t, _)| t.clone())
        .collect();
    drop(index);
    let synced = s
        .pr_data
        .iter()
        .filter(|p| by(p, login))
        .map(|p| p.created_at)
        .min();
    let walked = app.metrics.read().unwrap().first_pr.get(&l).copied();
    Facts {
        committer: s.committers.is(login),
        packages: m.packages.len(),
        modules: m.modules.len(),
        teams,
        first_pr_at: synced.into_iter().chain(walked).min(),
    }
}

fn checks(ci: Option<&str>) -> Option<&'static str> {
    match ci? {
        "SUCCESS" => Some("pass"),
        "FAILURE" | "ERROR" => Some("fail"),
        _ => Some("pending"),
    }
}

pub(crate) fn pr_json(p: &Pr, now: DateTime<Utc>) -> Value {
    json!({
        "number": p.number,
        "title": p.title,
        "url": p.url,
        "state": p.state,
        "base": p.base,
        "created_at": p.created_at,
        "merged_at": p.merged_at,
        "closed_at": p.closed_at,
        "review": review_state(p),
        "checks": checks(p.ci.as_deref()),
        "waiting_for_first_review": p.is_open() && waiting(p, now),
    })
}

/// The public profile for the API, beside the review record.
pub(crate) fn profile_value(app: &App, login: &str) -> Value {
    let s = app.snap();
    let now = Utc::now();
    let f = facts(app, &s, login);
    let open: Vec<Value> = open_prs(&s.pr_data, login, now)
        .into_iter()
        .map(|p| pr_json(p, now))
        .collect();
    let recent: Vec<Value> = recent_prs(&s.pr_data, login, now)
        .into_iter()
        .map(|p| pr_json(p, now))
        .collect();
    let areas: Vec<Value> = top_areas(&s.pr_data, login, now)
        .into_iter()
        .map(|(path, n)| json!({ "path": path, "pull_requests": n }))
        .collect();
    json!({
        "facts": f,
        "open_prs": open,
        "recent_prs": recent,
        "areas": areas,
    })
}

fn header_facts(login: &str, f: &Facts) -> String {
    let mut b = Vec::new();
    if f.committer {
        b.push(badge(
            Tone::Neutral,
            &format!("{} committer", icon(octicon!("git-merge"))),
        ));
    }
    if f.packages + f.modules > 0 {
        let mut what = Vec::new();
        if f.packages > 0 {
            what.push(plural(f.packages, "package", "packages"));
        }
        if f.modules > 0 {
            what.push(plural(f.modules, "module", "modules"));
        }
        b.push(format!(
            "<a class=\"Label\" href=\"{}\">maintains {}</a>",
            esc(&ProfileTab::Maintains.href(login)),
            what.join(" and ")
        ));
    }
    if !f.teams.is_empty() {
        let names: Vec<String> = f.teams.iter().map(|t| esc(t)).collect();
        b.push(badge(
            Tone::Neutral,
            &format!(
                "{} {}: {}",
                icon(octicon!("people")),
                if names.len() == 1 { "team" } else { "teams" },
                names.join(", ")
            ),
        ));
    }
    if let Some(t) = f.first_pr_at {
        b.push(badge(
            Tone::Neutral,
            &format!("first pull request {}", t.format("%Y-%m-%d")),
        ));
    }
    if b.is_empty() {
        return String::new();
    }
    format!(
        "<span class=\"d-inline-flex flex-wrap gap-1 flex-items-center\">{}</span>",
        b.join(" ")
    )
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The login follows the avatar, so the image is decorative: `alt` is empty.
fn avatar(login: &str) -> String {
    format!(
        "<img class=\"avatar mr-2 v-align-middle\" src=\"https://avatars.githubusercontent.com/{}?s=96\" \
         alt=\"\" width=\"48\" height=\"48\" loading=\"lazy\">",
        crate::github::urlencode(login)
    )
}

fn open_row(p: &Pr, now: DateTime<Utc>) -> String {
    let state = review_state(p);
    let mut badges = vec![state.badge()];
    if waiting(p, now) {
        badges.push(badge(
            Tone::Attention,
            &format!("waiting {}", ago(p.created_at)),
        ));
    }
    if p.mergeable == "CONFLICTING" {
        badges.push(badge(Tone::Danger, "conflict"));
    }
    item_row(
        Some(Kind::PR_OPEN),
        &crate::prpage::title_link(p.number, &esc(&p.title), "Link--primary text-bold"),
        "",
        &format!(
            "#{} · opened {} ago · <span class=\"branch-name\">{}</span>",
            p.number,
            ago(p.created_at),
            esc(&p.base)
        ),
        &badges,
        &match p.ci.as_deref() {
            None => String::new(),
            ci => format!("<span class=\"ml-2\">{}</span>", ci_icon(ci)),
        },
    )
}

fn recent_row(p: &Pr) -> String {
    let (kind, when) = match (p.merged_at, p.closed_at) {
        (Some(t), _) => (Kind::PR_MERGED, format!("merged {} ago", ago(t))),
        (None, Some(t)) => (Kind::PR_CLOSED, format!("closed {} ago", ago(t))),
        _ => (Kind::PR_CLOSED, format!("opened {} ago", ago(p.created_at))),
    };
    let merger = p
        .merged_by
        .as_deref()
        .filter(|m| !m.eq_ignore_ascii_case(&p.author))
        .map(|m| format!(" by <a href=\"/u/{0}\">{0}</a>", esc(m)))
        .unwrap_or_default();
    item_row(
        Some(kind),
        &crate::prpage::title_link(p.number, &esc(&p.title), "Link--primary text-bold"),
        "",
        &format!("#{} · {when}{merger}", p.number),
        &[],
        "",
    )
}

fn overview(s: &Snapshot, login: &str, after: usize, now: DateTime<Utc>) -> String {
    let open = open_prs(&s.pr_data, login, now);
    let waiting_n = open.iter().filter(|p| waiting(p, now)).count();
    let mut out = String::new();
    if waiting_n > 0 {
        out.push_str(&note(&format!(
            "{} of {} wait for a first review for {WAITING_DAYS} days or more. They come first. A review helps here.",
            plural(waiting_n, "pull request", "pull requests"),
            esc(login)
        )));
    }
    let rows: String = open
        .iter()
        .skip(after)
        .take(PAGE)
        .map(|p| open_row(p, now))
        .collect();
    out.push_str(&section(
        &format!("{} Open pull requests", icon(octicon!("git-pull-request"))),
        Some(open.len()),
        None,
        &if rows.is_empty() {
            empty("No open pull request in the synced data.")
        } else {
            rows
        },
    ));
    let next = after + PAGE;
    if next < open.len() {
        out.push_str(&pager(
            &ProfileTab::Overview.href(login),
            Some(&next.to_string()),
        ));
    }
    let recent = recent_prs(&s.pr_data, login, now);
    let rows: String = recent.iter().map(|p| recent_row(p)).collect();
    out.push_str(&section(
        &format!(
            "{} Merged or closed in the last {RECENT_DAYS} days",
            icon(octicon!("git-merge"))
        ),
        Some(recent.len()),
        None,
        &if rows.is_empty() {
            empty("None in the synced data.")
        } else {
            rows
        },
    ));
    let areas = top_areas(&s.pr_data, login, now);
    let rows: String = areas
        .iter()
        .map(|(d, n)| {
            item_row(
                None,
                &format!("{} {}/", icon(octicon!("file-directory")), esc(d)),
                &format!("/a/{}", esc(d)),
                &plural(*n, "pull request", "pull requests"),
                &[],
                "",
            )
        })
        .collect();
    out.push_str(&section(
        &format!(
            "{} Where they work in nixpkgs",
            icon(octicon!("file-directory"))
        ),
        None,
        None,
        &if rows.is_empty() {
            empty("No pull request in the synced data.")
        } else {
            rows
        },
    ));
    out
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

fn overlap(title: &str, list: &[(String, usize)]) -> String {
    const SHOWN: usize = 15;
    let mut rows: String = list
        .iter()
        .take(SHOWN)
        .map(|(l, n)| {
            format!(
                "<div class=\"Box-row d-flex\"><a class=\"flex-auto\" href=\"/u/{l}\">{l}</a><span class=\"Counter\">{n}</span></div>",
                l = esc(l)
            )
        })
        .collect();
    if list.len() > SHOWN {
        rows.push_str(&format!(
            "<div class=\"Box-row color-fg-muted\">and {} more</div>",
            list.len() - SHOWN
        ));
    }
    format!(
        "<div class=\"col-grow\">{}</div>",
        section(title, None, None, &rows)
    )
}

/// The review record: facts about reviews, not a judgement.
fn reviews(s: &Snapshot, login: &str) -> String {
    let d = dossier::build(&s.pr_data, login);
    let mut out = note(&format!(
        "These are facts about reviews, not a judgement. Maintainer Mode gives no rights; nixpkgs committers do. \
         It counts the reviews of the last {} days and on every open pull request.",
        ledger::WINDOW_DAYS,
    ));
    let _ = write!(
        out,
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
            "<div class=\"Box-row d-flex\"><div class=\"flex-auto min-width-0 wb-break-word\">\
             {t}\
             <div class=\"f6 color-fg-muted\">{} · <a href=\"{u}\">{}</a> · author <a href=\"/u/{a}\">{a}</a></div></div>\
             <div class=\"text-right ml-2 flex-shrink-0\">{}{found}</div></div>",
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
    out
}

#[derive(Deserialize)]
pub(crate) struct ProfileQuery {
    tab: Option<String>,
    after: Option<String>,
}

async fn page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(login): Path<String>,
    Query(q): Query<ProfileQuery>,
) -> Response {
    let viewer = app.viewer(&headers);
    if !crate::catalog::valid_login(&login) {
        let page = Page::new(Section::Other, "No such login")
            .body(empty("This is not a GitHub login."))
            .render(&app, viewer.as_deref());
        return (StatusCode::NOT_FOUND, page).into_response();
    }
    let tab = ProfileTab::parse(q.tab.as_deref());
    let s = app.snap();
    let now = Utc::now();
    let f = facts(&app, &s, &login);
    let (title, body) = match tab {
        ProfileTab::Overview => {
            let after = q.after.as_deref().and_then(|a| a.parse().ok()).unwrap_or(0);
            (login.clone(), overview(&s, &login, after, now))
        }
        ProfileTab::Reviews => (format!("Reviews of {login}"), reviews(&s, &login)),
        ProfileTab::Maintains => (
            format!("What {login} maintains"),
            crate::catalog::maintains_body(&app, &login),
        ),
    };
    drop(s);
    let own = viewer
        .as_deref()
        .is_some_and(|v| v.eq_ignore_ascii_case(&login));
    let mut actions = format!(
        "<a class=\"btn btn-sm\" href=\"https://github.com/{}\">{} GitHub</a>",
        crate::github::urlencode(&login),
        icon(octicon!("mark-github"))
    );
    if own {
        let _ = write!(
            actions,
            " <a class=\"btn btn-sm\" href=\"/packages?filter=mine\">{} Your packages</a>",
            icon(octicon!("person"))
        );
    }
    Page::new(Section::Other, title)
        .heading(format!("{}{}", avatar(&login), esc(&login)))
        .sub(header_facts(&login, &f))
        .subnav(tabs(&login, tab))
        .actions(actions)
        .body(body)
        .render(&app, viewer.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::tests::{pr, review, t};

    #[test]
    fn a_long_wait_for_a_first_review_comes_first() {
        let now = t(30);
        let mut fresh = pr(1, "alice", 28);
        fresh.reviews.clear();
        let old_waiting = pr(2, "alice", 5);
        let older_waiting = pr(3, "alice", 1);
        let mut reviewed = pr(4, "alice", 2);
        reviewed.reviews.push(review(1, "bob", 3));
        let mut newer = pr(5, "alice", 29);
        newer.reviews.push(review(2, "bob", 29));
        let mut own_review = pr(6, "alice", 10);
        own_review.reviews.push(review(3, "alice", 11));
        let mut draft = pr(7, "alice", 0);
        draft.is_draft = true;
        let other = pr(8, "bob", 0);
        let mut merged = pr(9, "alice", 0);
        merged.state = "MERGED".into();
        let prs = vec![
            fresh,
            old_waiting,
            older_waiting,
            reviewed,
            newer,
            own_review,
            draft,
            other,
            merged,
        ];
        let order: Vec<u64> = open_prs(&prs, "Alice", now)
            .iter()
            .map(|p| p.number)
            .collect();
        assert_eq!(order, vec![3, 2, 6, 5, 1, 4, 7]);
    }

    #[test]
    fn the_review_state_follows_the_last_decision_of_each_reviewer() {
        let mut p = pr(1, "alice", 0);
        assert_eq!(review_state(&p), ReviewState::NoReview);
        let mut r = review(1, "bob", 1);
        r.state = "CHANGES_REQUESTED".into();
        p.reviews.push(r);
        p.reviews.push(review(2, "bob", 2));
        assert_eq!(review_state(&p), ReviewState::ChangesRequested);
        let mut r = review(3, "bob", 3);
        r.state = "APPROVED".into();
        p.reviews.push(r);
        assert_eq!(review_state(&p), ReviewState::Approved);
        let mut q = pr(2, "alice", 0);
        q.reviews.push(review(4, "carol", 1));
        assert_eq!(review_state(&q), ReviewState::Reviewed);
        q.is_draft = true;
        assert_eq!(review_state(&q), ReviewState::Draft);
    }

    #[test]
    fn recent_and_areas_keep_to_their_window() {
        let now = t(100);
        let mut a = pr(1, "alice", 0);
        a.state = "MERGED".into();
        a.merged_at = Some(t(90));
        a.files = vec![
            "pkgs/by-name/he/hello/package.nix".into(),
            "pkgs/by-name/he/hello/fix.patch".into(),
        ];
        let mut b = pr(2, "alice", 0);
        b.state = "CLOSED".into();
        b.closed_at = Some(t(10));
        b.files = vec!["nixos/modules/services/x.nix".into()];
        let mut c = pr(3, "alice", 95);
        c.files = vec!["pkgs/by-name/he/hello/package.nix".into()];
        let prs = vec![a, b, c];
        let recent: Vec<u64> = recent_prs(&prs, "alice", now)
            .iter()
            .map(|p| p.number)
            .collect();
        assert_eq!(recent, vec![1]);
        assert_eq!(
            top_areas(&prs, "alice", now),
            vec![
                ("pkgs/by-name/he/hello".to_string(), 2),
                ("nixos/modules/services".to_string(), 1)
            ]
        );
    }

    async fn html(r: Response) -> String {
        let b = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(b.to_vec()).unwrap()
    }

    fn sample() -> Shared {
        let app = crate::sets::tests::app(&[]);
        let now = Utc::now();
        let mut waiting = pr(1, "alice", 0);
        waiting.title = "hello: 1.0 -> 2.0".into();
        waiting.created_at = now - Duration::days(20);
        waiting.files = vec!["pkgs/by-name/he/hello/package.nix".into()];
        let mut fresh = pr(2, "alice", 0);
        fresh.created_at = now - Duration::days(1);
        fresh.ci = Some("FAILURE".into());
        fresh.reviews.push(review(1, "bob", 0));
        let mut merged = pr(3, "alice", 0);
        merged.state = "MERGED".into();
        merged.created_at = now - Duration::days(9);
        merged.merged_at = Some(now - Duration::days(3));
        merged.merged_by = Some("carol".into());
        app.store.put_prs(&[waiting, fresh, merged]).unwrap();
        app.index
            .write()
            .unwrap()
            .teams
            .insert("web".into(), vec!["alice".into()]);
        app.recompute().unwrap();
        app
    }

    async fn get(app: &Shared, tab: Option<&str>) -> Response {
        page(
            State(app.clone()),
            HeaderMap::new(),
            Path("alice".into()),
            Query(ProfileQuery {
                tab: tab.map(String::from),
                after: None,
            }),
        )
        .await
    }

    #[tokio::test]
    async fn the_overview_shows_the_facts_and_the_waiting_pull_request_first() {
        let app = sample();
        let r = get(&app, None).await;
        assert_eq!(r.status(), StatusCode::OK);
        let h = html(r).await;
        assert!(!h.contains("href=\"/packages\" aria-current"));
        assert!(h.contains("src=\"https://avatars.githubusercontent.com/alice?s=96\""));
        assert!(h.contains("loading=\"lazy\""));
        assert!(h.contains("href=\"https://github.com/alice\""));
        assert!(h.contains("alt=\"\" width=\"48\" height=\"48\""));
        assert!(h.contains("team: web"));
        assert!(h.contains("first pull request "));
        assert!(h.contains("selected\" href=\"/u/alice\" aria-current=\"page\">Overview</a>"));
        assert!(h.contains("href=\"/u/alice?tab=reviews\">Reviews</a>"));
        assert!(h.contains("href=\"/u/alice?tab=maintains\">Maintains</a>"));
        assert!(h.contains("A review helps here."));
        let first = h.find("hello: 1.0 -&gt; 2.0").unwrap();
        let second = h.find("pr 2</a>").unwrap();
        assert!(first < second, "the waiting pull request comes first");
        assert!(h.contains("no review yet"));
        assert!(h.contains("aria-label=\"checks fail\""));
        assert!(h.contains("merged 3d ago by <a href=\"/u/carol\">carol</a>"));
        assert!(h.contains("href=\"/a/pkgs/by-name/he/hello\""));
        assert!(!h.contains("Your private pages"));
        assert!(!h.contains("score"), "no score on the overview");
    }

    #[tokio::test]
    async fn the_other_tabs_and_the_old_address_answer() {
        let app = sample();
        let h = html(get(&app, Some("reviews")).await).await;
        assert!(h.contains("aria-current=\"page\">Reviews</a>"));
        assert!(h.contains("facts about reviews, not a judgement"));
        assert!(
            !h.to_lowercase().contains("credit"),
            "no credit on the profile"
        );
        assert!(!h.contains("in the queues"));
        let h = html(get(&app, Some("maintains")).await).await;
        assert!(h.contains("What alice maintains"));
        assert!(h.contains("aria-current=\"page\">Maintains</a>"));
        let r = old_maintains(Path("alice".into())).await.into_response();
        assert_eq!(
            r.headers()["location"].to_str().unwrap(),
            "/u/alice?tab=maintains"
        );
        let r = page(
            State(app.clone()),
            HeaderMap::new(),
            Path("not a login".into()),
            Query(ProfileQuery {
                tab: None,
                after: None,
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }
    #[test]
    fn the_api_gives_the_facts_and_the_open_pull_requests() {
        let app = sample();
        let v = profile_value(&app, "alice");
        assert_eq!(v["facts"]["teams"], json!(["web"]));
        assert_eq!(v["open_prs"][0]["number"], 1);
        assert_eq!(v["open_prs"][0]["waiting_for_first_review"], true);
        assert_eq!(v["open_prs"][1]["checks"], "fail");
        assert_eq!(v["recent_prs"][0]["number"], 3);
    }
}

#[cfg(test)]
mod own {
    use super::*;

    #[tokio::test]
    async fn the_owner_gets_a_link_to_their_packages_and_no_private_data() {
        let app = crate::sets::tests::app(&[]);
        app.store.watch("alice", "pkg:secret-watch").unwrap();
        let h = crate::notify::tests::signed_in(&app, "alice");
        let q = || {
            Query(ProfileQuery {
                tab: None,
                after: None,
            })
        };
        let r = page(State(app.clone()), h.clone(), Path("alice".into()), q()).await;
        let b = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(b.to_vec()).unwrap();
        assert!(html.contains("href=\"/packages?filter=mine\">"));
        assert!(html.contains("Your packages"));
        assert!(!html.contains("secret-watch"));
        let r = page(State(app.clone()), h, Path("bob".into()), q()).await;
        let b = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(
            !String::from_utf8(b.to_vec())
                .unwrap()
                .contains("Your private pages")
        );
    }
}
