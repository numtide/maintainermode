//! `/committers`: one queue of everyone around commit access, each with a
//! status, for the people who grant and remove it. Only the delegation team
//! ([`crate::committer::DELEGATION`]) sees it. It shows no score and
//! recommends nobody: each row is facts, and the order is one fact that the
//! viewer picks. Like the ledger, the facts are a pure function of the
//! stored pull request history.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use chrono::{DateTime, Datelike, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use crate::committer::DELEGATION;
use crate::html::{Html, html};
use crate::metrics::Facts;
use crate::model::automation;
use crate::ui::{Page, Section, Tab, Tone, badge, empty, item_row, note, pager, section};
use crate::web::{App, Shared, octicon};

pub const YEAR_DAYS: i64 = 365;
/// RFC 55: a committer with no use of commit access in six months is
/// retired. The site sees merges only, not pushes.
pub const INACTIVE_DAYS: i64 = 180;
/// The lowest number of pull requests reviewed in the last year that a row
/// of a person who is not a committer needs; the page starts at the second.
pub const MINS: [&str; 4] = ["1", "10", "25", "50"];
pub const DEFAULT_MIN: usize = 10;
pub const STATUSES: [&str; 5] = ["all", "active", "inactive", "candidate", "former"];
pub const SORTS: [&str; 8] = [
    "reviews",
    "reviews_all",
    "approved",
    "merged",
    "merges",
    "last_merge",
    "first",
    "maintains",
];
const NOMINATIONS: &str = "https://github.com/NixOS/nixpkgs-committers";

pub fn routes() -> Router<Shared> {
    Router::new().route("/committers", get(page))
}

/// One person's facts from the whole pull request history.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Person {
    /// As GitHub spells it.
    pub login: String,
    /// Pull requests of other people reviewed in the last 365 days.
    pub reviewed_365d: usize,
    /// Pull requests of other people reviewed, ever.
    pub reviewed: usize,
    pub first_review_at: Option<DateTime<Utc>>,
    /// Parts of nixpkgs reviewed, as the metrics name them.
    pub areas: usize,
    /// Pull requests approved in the last 365 days that a committer other
    /// than the reviewer merged: the judgement matched the merge button.
    pub approved_merged_365d: usize,
    /// Pull requests of first-time contributors reviewed in the last 365
    /// days: where teaching happens.
    pub first_timers_365d: usize,
    /// Own pull requests merged in the last 365 days.
    pub merged_365d: usize,
    /// Own pull requests merged, ever.
    pub merged: usize,
    pub first_pr_at: Option<DateTime<Utc>>,
    /// Pull requests of other people this person merged in the last 365 days.
    pub merges_365d: usize,
    /// Pull requests of other people this person merged, ever.
    pub merges: usize,
    /// The last merge by this person, an own pull request included.
    pub last_merge_at: Option<DateTime<Utc>>,
}

/// A reviewer on one pull request: the login as spelled, the first review,
/// and the last approval.
type Reviewer<'a> = (&'a str, DateTime<Utc>, Option<DateTime<Utc>>);

/// Lower-case login → facts.
#[derive(Debug, Clone, Default)]
pub struct People(pub HashMap<String, Person>);

fn earliest(slot: &mut Option<DateTime<Utc>>, at: DateTime<Utc>) {
    *slot = Some(slot.map_or(at, |s| s.min(at)));
}

fn latest(slot: &mut Option<DateTime<Utc>>, at: DateTime<Utc>) {
    *slot = Some(slot.map_or(at, |s| s.max(at)));
}

pub fn compute(facts: &[Facts], now: DateTime<Utc>) -> People {
    let since = now - Duration::days(YEAR_DAYS);
    let mut by: HashMap<String, Person> = HashMap::new();
    let mut areas: HashMap<String, BTreeSet<String>> = HashMap::new();
    let person = |by: &mut HashMap<String, Person>, login: &str| -> String {
        let key = login.to_lowercase();
        by.entry(key.clone()).or_insert_with(|| Person {
            login: login.to_string(),
            ..Default::default()
        });
        key
    };
    for f in facts {
        if !f.bot() && f.author != "ghost" {
            let k = person(&mut by, &f.author);
            let p = by.get_mut(&k).unwrap();
            earliest(&mut p.first_pr_at, f.created_at);
            if let Some(at) = f.merged_at {
                p.merged += 1;
                p.merged_365d += (at >= since) as usize;
            }
        }
        // the merge bot presses the button for a maintainer: no person merged
        let merger = f
            .merged_by
            .as_deref()
            .filter(|m| !automation(m) && *m != "ghost");
        if let Some((by_login, at)) = merger.zip(f.merged_at) {
            let k = person(&mut by, by_login);
            let p = by.get_mut(&k).unwrap();
            let others = !by_login.eq_ignore_ascii_case(&f.author);
            p.merges += others as usize;
            p.merges_365d += (at >= since && others) as usize;
            latest(&mut p.last_merge_at, at);
        }
        // one entry per reviewer: the first review, and the last approval
        let mut seen: HashMap<String, Reviewer<'_>> = HashMap::new();
        for r in &f.reviews {
            if r.by.eq_ignore_ascii_case(&f.author) || r.by == "ghost" {
                continue;
            }
            let e = seen
                .entry(r.by.to_lowercase())
                .or_insert((r.by.as_str(), r.at, None));
            e.1 = e.1.min(r.at);
            if r.state == "APPROVED" {
                e.2 = Some(e.2.map_or(r.at, |a: DateTime<Utc>| a.max(r.at)));
            }
        }
        let merged_by_other =
            |reviewer: &str| merger.is_some_and(|m| !m.eq_ignore_ascii_case(reviewer));
        let first_time = f.first_time();
        for (login, first, approved) in seen.into_values() {
            let k = person(&mut by, login);
            let p = by.get_mut(&k).unwrap();
            p.reviewed += 1;
            p.reviewed_365d += (first >= since) as usize;
            p.first_timers_365d += (first >= since && first_time) as usize;
            earliest(&mut p.first_review_at, first);
            if approved.is_some_and(|a| a >= since) && merged_by_other(login) {
                p.approved_merged_365d += 1;
            }
            areas
                .entry(k)
                .or_default()
                .extend(f.areas.iter().filter(|a| *a != "other").cloned());
        }
    }
    for (k, set) in areas {
        if let Some(p) = by.get_mut(&k) {
            p.areas = set.len();
        }
    }
    People(by)
}

/// Where a person stands, from the committer fact and the merges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// A committer who merged in the last [`INACTIVE_DAYS`].
    Active,
    /// A committer with no merge in [`INACTIVE_DAYS`]: the retirement rule
    /// of nixpkgs-committers may apply. Pushes without a pull request are
    /// not seen.
    Inactive,
    /// Not a committer; reviews pull requests.
    Candidate,
    /// Not a committer now; merged pull requests before.
    Former,
}

impl Status {
    fn key(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Inactive => "inactive",
            Self::Candidate => "candidate",
            Self::Former => "former",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Active => "committer",
            Self::Inactive => "committer, no merge in 6 months",
            Self::Candidate => "not a committer",
            Self::Former => "former committer",
        }
    }

    fn tone(self) -> Tone {
        match self {
            Self::Active => Tone::Success,
            Self::Inactive => Tone::Attention,
            Self::Candidate | Self::Former => Tone::Neutral,
        }
    }

    fn of(p: &Person, committer: bool, now: DateTime<Utc>) -> Self {
        let recent = p
            .last_merge_at
            .is_some_and(|t| now - t < Duration::days(INACTIVE_DAYS));
        match (committer, recent, p.last_merge_at.is_some()) {
            (true, true, _) => Self::Active,
            (true, false, _) => Self::Inactive,
            (false, _, true) => Self::Former,
            (false, _, false) => Self::Candidate,
        }
    }
}

/// A row of the page: the facts, the status, and what the index adds.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    #[serde(flatten)]
    pub person: Person,
    pub status: Status,
    /// Packages and NixOS modules whose `meta.maintainers` name the login.
    pub maintains: usize,
    /// From the review record of the window of the main database (the last
    /// 60 days and the open pull requests), as the profile shows it: pull
    /// requests a committer merged with no change after the review.
    pub merged_unchanged_60d: usize,
    /// The author changed the pull request after a review that asked for
    /// changes or commented on a changed line.
    pub problems_found_60d: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Opts {
    pub status: Option<String>,
    pub sort: Option<String>,
    pub min: Option<usize>,
    pub q: Option<String>,
    pub after: Option<u64>,
}

impl Opts {
    pub fn status(&self) -> &str {
        self.status
            .as_deref()
            .filter(|s| STATUSES.contains(s))
            .unwrap_or(STATUSES[0])
    }

    pub fn sort(&self) -> &str {
        self.sort
            .as_deref()
            .filter(|s| SORTS.contains(s))
            .unwrap_or(SORTS[0])
    }

    pub fn min(&self) -> usize {
        self.min.unwrap_or(DEFAULT_MIN).max(1)
    }

    pub fn q(&self) -> &str {
        self.q.as_deref().map(str::trim).unwrap_or_default()
    }

    fn query(&self, status: &str, sort: &str, min: usize) -> String {
        let mut parts = Vec::new();
        if status != STATUSES[0] {
            parts.push(format!("status={status}"));
        }
        if sort != SORTS[0] {
            parts.push(format!("sort={sort}"));
        }
        if min != DEFAULT_MIN {
            parts.push(format!("min={min}"));
        }
        if !self.q().is_empty() {
            parts.push(format!("q={}", crate::github::urlencode(self.q())));
        }
        if parts.is_empty() {
            "/committers".to_string()
        } else {
            format!("/committers?{}", parts.join("&"))
        }
    }

    fn here(&self) -> String {
        self.query(self.status(), self.sort(), self.min())
    }
}

/// Packages and modules by direct maintainer, lower-case login → count.
type Maintains = Arc<HashMap<String, usize>>;

/// Walked once per index revision: the walk is 100,000 packages.
fn maintains_count(index: &crate::area::Index) -> Maintains {
    static CACHE: Mutex<Option<(String, Maintains)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap();
    if let Some((_, m)) = cache.as_ref().filter(|(rev, _)| *rev == index.rev) {
        return m.clone();
    }
    let mut out: HashMap<String, usize> = HashMap::new();
    let ms = index
        .pkgs
        .values()
        .map(|p| &p.maintainers)
        .chain(index.modules.values().map(|m| &m.maintainers));
    for logins in ms {
        for l in logins {
            *out.entry(l.to_lowercase()).or_default() += 1;
        }
    }
    let out = Arc::new(out);
    *cache = Some((index.rev.clone(), out.clone()));
    out
}

/// The queue: every committer, and everyone else who reviewed at least
/// `min` pull requests of other people in the last year, then the status
/// and the search applied, in the order of `sort`. The 60-day record is
/// not filled: [`record`] does that for the rows of one page.
pub fn rows(app: &App, o: &Opts, now: DateTime<Utc>) -> (Vec<Row>, HashMap<Status, usize>) {
    let s = app.snap();
    let report = app.metrics.read().unwrap().clone();
    let maintains = maintains_count(&app.index.read().unwrap());
    let q = o.q().to_lowercase();
    let min = o.min();
    let want = o.status();
    let mut counts: HashMap<Status, usize> = HashMap::new();
    let mut out: Vec<Row> = Vec::new();
    let nobody = Person::default();
    // a committer with nothing in the history is in the queue too
    let silent: Vec<(String, &Person)> = s
        .committers
        .logins()
        .filter(|l| !report.people.0.contains_key(*l))
        .map(|l| (l.to_string(), &nobody))
        .collect();
    let everyone = report
        .people
        .0
        .iter()
        .map(|(k, p)| (k.as_str(), p))
        .chain(silent.iter().map(|(k, p)| (k.as_str(), *p)));
    for (k, p) in everyone {
        if automation(k) || k == "ghost" {
            continue;
        }
        let committer = s.committers.is(k);
        if !committer && p.reviewed_365d < min {
            continue;
        }
        if !q.is_empty() && !k.contains(&q) {
            continue;
        }
        let status = Status::of(p, committer, now);
        *counts.entry(status).or_default() += 1;
        if want != "all" && status.key() != want {
            continue;
        }
        let mut person = p.clone();
        if person.login.is_empty() {
            person.login = k.to_string();
        }
        out.push(Row {
            status,
            maintains: maintains.get(k).copied().unwrap_or_default(),
            merged_unchanged_60d: 0,
            problems_found_60d: 0,
            person,
        });
    }
    let sort = o.sort();
    let far = DateTime::<Utc>::MAX_UTC;
    out.sort_by(|a, b| {
        let (x, y) = (&a.person, &b.person);
        let by_fact = match sort {
            "reviews_all" => y.reviewed.cmp(&x.reviewed),
            "approved" => y.approved_merged_365d.cmp(&x.approved_merged_365d),
            "merged" => y.merged_365d.cmp(&x.merged_365d),
            "merges" => y.merges_365d.cmp(&x.merges_365d),
            "last_merge" => x
                .last_merge_at
                .unwrap_or(far)
                .cmp(&y.last_merge_at.unwrap_or(far)),
            "first" => x
                .first_pr_at
                .unwrap_or(far)
                .cmp(&y.first_pr_at.unwrap_or(far)),
            "maintains" => b.maintains.cmp(&a.maintains),
            _ => y.reviewed_365d.cmp(&x.reviewed_365d),
        };
        by_fact.then_with(|| x.login.to_lowercase().cmp(&y.login.to_lowercase()))
    });
    (out, counts)
}

/// Fills the 60-day record of the rows of one page from the snapshot.
fn record(rows: &mut [Row], s: &crate::web::Snapshot) {
    for r in rows {
        let login = &r.person.login;
        let Some(at) = s.reviewed_by.get(login) else {
            continue;
        };
        let record = crate::dossier::rows(at.iter().map(|i| &s.pr_data[*i]), login);
        r.merged_unchanged_60d = record
            .iter()
            .filter(|r| r.outcome == crate::dossier::Outcome::MergedUnchanged)
            .count();
        r.problems_found_60d = record.iter().filter(|r| r.problem_found).count();
    }
}

/// Who may see the page, and why not.
pub(crate) enum Access {
    Granted,
    SignIn,
    NotDelegate,
}

pub(crate) fn access(app: &App, viewer: Option<&str>) -> Access {
    match viewer {
        None => Access::SignIn,
        Some(v) if app.snap().committers.delegates(v) => Access::Granted,
        Some(_) => Access::NotDelegate,
    }
}

pub(crate) fn forbidden_text(app: &App) -> String {
    let read = match app.snap().committers.delegation_at {
        Some(t) => format!("The server read the team {} ago.", crate::web::ago(t)),
        None => "The server has not read the team yet.".to_string(),
    };
    format!(
        "This page is for the people who grant commit access to nixpkgs: the members of the GitHub team {DELEGATION}. {read}"
    )
}

/// The JSON of `/api/v1/committers`: the rows in the order of `sort`,
/// paged as every list of the API, with the filter, the counts of each
/// status and the team.
pub(crate) fn value(
    app: &App,
    o: &Opts,
    limit: Option<usize>,
    cursor: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Value, String> {
    let (mut all, counts) = rows(app, o, now);
    // the window of the page, as `api::page` cuts it
    let start = cursor
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map_or(Ok(0), str::parse::<usize>)
        .map_err(|_| format!("bad cursor {}", cursor.unwrap_or_default()))?;
    let end = start
        .saturating_add(
            limit
                .unwrap_or(crate::api::LIMIT)
                .clamp(1, crate::api::MAX_LIMIT),
        )
        .min(all.len());
    if let Some(page) = all.get_mut(start.min(end)..end) {
        record(page, &app.snap());
    }
    let mut v = crate::api::page(&all, limit, cursor)?;
    v["team"] = json!(DELEGATION);
    v["status"] = json!(o.status());
    v["sort"] = json!(o.sort());
    v["min"] = json!(o.min());
    v["q"] = json!(o.q());
    v["counts"] = json!(
        [
            Status::Active,
            Status::Inactive,
            Status::Candidate,
            Status::Former
        ]
        .iter()
        .map(|s| (s.key(), counts.get(s).copied().unwrap_or_default()))
        .collect::<HashMap<_, _>>()
    );
    v["inactive_days"] = json!(INACTIVE_DAYS);
    Ok(v)
}

fn ym(t: Option<DateTime<Utc>>) -> String {
    t.map_or_else(|| "–".into(), |t| format!("{}-{:02}", t.year(), t.month()))
}

fn row_html(r: &Row) -> Html {
    let p = &r.person;
    let login = &p.login;
    let href = format!("/u/{}", crate::github::urlencode(login));
    let mut parts = vec![html!(
        "reviewed <b>{}</b> in 12 months, {} since {}, {} parts of nixpkgs",
        p.reviewed_365d,
        p.reviewed,
        ym(p.first_review_at),
        p.areas
    )];
    match r.status {
        Status::Active | Status::Inactive | Status::Former => parts.push(html!(
            "merged <b>{}</b> pull requests of others in 12 months, {} ever, last {}",
            p.merges_365d,
            p.merges,
            ym(p.last_merge_at)
        )),
        Status::Candidate => parts.push(html!(
            "<b>{}</b> approvals that a committer merged",
            p.approved_merged_365d
        )),
    }
    parts.push(html!(
        "<b>{}</b> first-time contributors reviewed",
        p.first_timers_365d
    ));
    let lines = [
        crate::html::join(parts, " · "),
        html!(
            "last 60 days: {} merged unchanged after the review, {} problems found",
            r.merged_unchanged_60d,
            r.problems_found_60d
        ),
        html!(
            "<b>{}</b> own pull requests merged in 12 months, {} since {} · maintains {}",
            p.merged_365d,
            p.merged,
            ym(p.first_pr_at),
            r.maintains
        ),
    ];
    let enc = crate::github::urlencode(login);
    let gh = |q: &str| {
        format!(
            "https://github.com/NixOS/nixpkgs/pulls?q={}",
            crate::github::urlencode(&format!("is:pr {q}"))
        )
    };
    let links = [
        (format!("{href}?tab=reviews"), "record"),
        (
            gh(&format!("reviewed-by:{login} -author:{login}")),
            "reviews",
        ),
        (
            gh(&format!("commenter:{login} -author:{login}")),
            "comments",
        ),
        (gh(&format!("author:{login}")), "pull requests"),
        (
            format!("{NOMINATIONS}/pulls?q=is%3Apr+{enc}"),
            "nominations",
        ),
    ];
    let links = crate::html::join(
        links
            .iter()
            .map(|(u, label)| html!("<a href=\"{u}\">{label}</a>")),
        " · ",
    );
    let meta = html!(
        "{}<div class=\"mt-1\">How they work: {links}</div>",
        crate::html::join(lines.into_iter().map(|l| html!("<div>{l}</div>")), "")
    );
    item_row(
        None,
        Html::text(login),
        &href,
        meta,
        &[badge(r.status.tone(), r.status.label())],
        Html::new(),
    )
}

fn bar(o: &Opts, counts: &HashMap<Status, usize>, total: usize) -> Html {
    let (status, sort, min) = (o.status(), o.sort(), o.min());
    let all: usize = counts.values().sum();
    let mut choices =
        vec![Tab::new(o.query("all", sort, min), "Everyone", status == "all").count(all)];
    choices.extend(
        [
            (Status::Active, "Committers"),
            (Status::Inactive, "No merge in 6 months"),
            (Status::Candidate, "Not committers"),
            (Status::Former, "Former committers"),
        ]
        .iter()
        .map(|(s, label)| {
            Tab::new(o.query(s.key(), sort, min), *label, status == s.key())
                .count(counts.get(s).copied().unwrap_or_default())
        }),
    );
    let mins: Vec<Tab> = MINS
        .iter()
        .map(|m| {
            let n: usize = m.parse().unwrap();
            Tab::new(o.query(status, sort, n), html!("{m}+"), n == min)
        })
        .collect();
    let sorts: Vec<Tab> = [
        ("reviews", "Reviews, 12 months"),
        ("reviews_all", "Reviews, all time"),
        ("approved", "Approvals merged"),
        ("merges", "Merges for others"),
        ("last_merge", "Last merge, longest ago"),
        ("merged", "Own pull requests merged"),
        ("first", "Longest tenure"),
        ("maintains", "Maintains most"),
    ]
    .iter()
    .map(|(k, label)| Tab::new(o.query(status, k, min), *label, *k == sort))
    .collect();
    let keep = vec![
        ("status", status.to_string()),
        ("sort", sort.to_string()),
        ("min", min.to_string()),
    ];
    let mut out = crate::ui::FilterBar {
        choices,
        rows: html!(
            "{}{}",
            crate::ui::chips(
                "Reviews of people who are not committers",
                "not committers: reviewed at least".into(),
                &mins
            ),
            crate::ui::chips("Order", "order by".into(), &sorts)
        ),
        action: "/committers",
        keep,
        placeholder: "Part of a login",
        q: o.q().to_string(),
        clear: (!o.q().is_empty()).then(|| o.here()),
        ..Default::default()
    }
    .render();
    out.push(&html!(
        "<p class=\"f6 color-fg-muted mb-2\">{total} people in this list. Bots and ghost are left out.</p>"
    ));
    out
}

pub(crate) fn body(app: &App, o: &Opts, now: DateTime<Utc>) -> Html {
    let (mut all, counts) = rows(app, o, now);
    let start = o
        .after
        .map_or(0, |a| usize::try_from(a).unwrap_or(usize::MAX))
        .min(all.len());
    let end = (start + crate::review::PAGE).min(all.len());
    record(&mut all[start..end], &app.snap());
    let (page, next) = crate::review::paged_by_offset(&all, o.after);
    let list: Html = if page.is_empty() {
        empty(
            "Nobody matches. Pick another status, lower the number of reviews, or clear the search.",
        )
    } else {
        page.iter().map(row_html).collect()
    };
    let mut out = Html::new();
    if o.after.is_none() {
        out.push(&note(html!(
            "Facts for the people who grant and remove commit access, from public data. No score, no \
             recommendation: the order is one fact, the one you pick. The status comes from the merges: a \
             committer with no merge in {INACTIVE_DAYS} days is marked, as the <a href=\"{NOMINATIONS}#inactivity\">retirement \
             rule</a> counts six months; a push without a pull request is not seen. \"Approvals that a committer \
             merged\" counts pull requests approved in the last 12 months that someone else merged. \
             <a href=\"{NOMINATIONS}#what-we-look-for-in-committers\">What the team looks for</a> is mostly not a \
             number: whether a person teaches, defers, takes feedback and upholds quality shows in the reviews and \
             comments themselves. The links under each row lead there."
        )));
        out.push(&bar(o, &counts, all.len()));
    }
    out.push(&section("People", Some(all.len()), None, list));
    out.push(&pager(&o.here(), next.as_deref()));
    out
}

async fn page(State(app): State<Shared>, headers: HeaderMap, Query(o): Query<Opts>) -> Response {
    let viewer = app.viewer(&headers);
    match access(&app, viewer.as_deref()) {
        Access::SignIn => return Redirect::to("/auth/login").into_response(),
        Access::NotDelegate => {
            let mut res = Page::new(Section::Other, "Commit access")
                .icon(octicon!("people"))
                .body(note(Html::text(&forbidden_text(&app))))
                .render(&app, viewer.as_deref());
            *res.status_mut() = StatusCode::FORBIDDEN;
            return res;
        }
        Access::Granted => {}
    }
    Page::new(Section::Personal(crate::ui::Personal::Access), "Commit access")
        .icon(octicon!("people"))
        .sub(html!(
            "For the members of {DELEGATION}. The facts are public; this list is not. \
             <a href=\"/state?tab=docs&amp;doc=personal#for-the-people-who-grant-commit-access\">How it is made</a>"
        ))
        .query(o.q())
        .body(body(&app, &o, Utc::now()))
        .render(&app, viewer.as_deref())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::metrics::Seen;

    pub(crate) fn views() -> Vec<String> {
        let mut v = vec!["/committers".to_string()];
        v.extend(STATUSES.iter().map(|s| format!("/committers?status={s}")));
        v.extend(SORTS.iter().map(|s| format!("/committers?sort={s}")));
        v.extend(MINS.iter().map(|m| format!("/committers?min={m}")));
        v
    }

    fn t(day: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(day * 86_400, 0).unwrap()
    }

    fn pr(number: u64, author: &str, created: i64) -> Facts {
        Facts {
            number,
            title: format!("pr {number}"),
            author: author.into(),
            author_is_bot: false,
            association: "CONTRIBUTOR".into(),
            state: "OPEN".into(),
            is_draft: false,
            base: "master".into(),
            created_at: t(created),
            updated_at: t(created),
            closed_at: None,
            merged_at: None,
            merged_by: None,
            additions: 1,
            deletions: 0,
            changed_files: 1,
            labels: vec![],
            areas: vec!["packages: by-name".into()],
            ai: None,
            reviews: vec![],
        }
    }

    fn merged(mut f: Facts, by: &str, day: i64) -> Facts {
        f.state = "MERGED".into();
        f.merged_at = Some(t(day));
        f.merged_by = Some(by.into());
        f
    }

    fn seen(by: &str, day: i64, state: &str) -> Seen {
        Seen {
            by: by.into(),
            at: t(day),
            state: state.into(),
        }
    }

    #[test]
    fn the_facts_of_a_person_come_from_the_whole_history() {
        let now = t(1000);
        let mut old = merged(pr(1, "alice", 10), "carol", 20);
        old.reviews = vec![seen("Bob", 12, "COMMENTED"), seen("bob", 14, "APPROVED")];
        old.areas = vec!["lib".into(), "other".into()];
        let mut recent = merged(pr(2, "alice", 900), "carol", 910);
        recent.reviews = vec![
            seen("bob", 905, "APPROVED"),
            seen("alice", 906, "COMMENTED"),
        ];
        let mut own_button = merged(pr(3, "alice", 920), "bob", 930);
        own_button.reviews = vec![seen("bob", 925, "APPROVED")];
        let mut open = pr(4, "dave", 950);
        open.reviews = vec![seen("bob", 951, "APPROVED"), seen("ghost", 952, "APPROVED")];
        let mut bot = merged(pr(5, "r-ryantm", 960), "bob", 961);
        bot.author_is_bot = true;
        let self_merge = merged(pr(6, "bob", 970), "bob", 971);
        let mut by_bot = merged(pr(7, "dave", 980), "nixpkgs-merge-bot", 981);
        by_bot.reviews = vec![seen("bob", 980, "APPROVED")];
        let p = compute(
            &[old, recent, own_button, open, bot, self_merge, by_bot],
            now,
        );
        let bob = &p.0["bob"];
        assert_eq!(bob.login, "Bob");
        assert_eq!((bob.reviewed, bob.reviewed_365d), (5, 4));
        assert_eq!(bob.first_review_at, Some(t(12)));
        assert_eq!(bob.areas, 2, "lib and by-name; other does not count");
        assert_eq!(
            bob.approved_merged_365d, 1,
            "#2 only: #1 is old, #3 bob merged, #4 is open, #7 the bot merged"
        );
        assert_eq!(
            (bob.merges, bob.merges_365d),
            (2, 2),
            "the self merge is not for others"
        );
        assert_eq!(bob.last_merge_at, Some(t(971)));
        assert_eq!((bob.merged, bob.first_pr_at), (1, Some(t(970))));
        let alice = &p.0["alice"];
        assert_eq!((alice.merged, alice.merged_365d), (3, 2));
        assert_eq!(alice.first_pr_at, Some(t(10)));
        assert_eq!(
            alice.reviewed, 0,
            "a review on your own pull request is not a review"
        );
        assert!(!p.0.contains_key("ghost") && !p.0.contains_key("r-ryantm"));
        assert!(
            !p.0.contains_key("nixpkgs-merge-bot"),
            "the bot is no person"
        );
        assert_eq!(p.0["carol"].last_merge_at, Some(t(910)));
    }

    #[test]
    fn the_status_follows_the_committer_fact_and_the_last_merge() {
        let now = t(1000);
        let fresh = Person {
            last_merge_at: Some(t(900)),
            ..Default::default()
        };
        let stale = Person {
            last_merge_at: Some(t(700)),
            ..Default::default()
        };
        let never = Person::default();
        assert_eq!(Status::of(&fresh, true, now), Status::Active);
        assert_eq!(Status::of(&stale, true, now), Status::Inactive);
        assert_eq!(Status::of(&never, true, now), Status::Inactive);
        assert_eq!(Status::of(&stale, false, now), Status::Former);
        assert_eq!(Status::of(&never, false, now), Status::Candidate);
    }

    /// (login, reviewed in the year, reviewed ever, last merge day)
    fn app_with(
        people: &[(&str, usize, usize, Option<i64>)],
        committers: &[&str],
        delegates: &[&str],
    ) -> Shared {
        let app = crate::sets::tests::app(&[]);
        let mut report = crate::metrics::Report::default();
        for (login, year, all, merge) in people {
            report.people.0.insert(
                login.to_lowercase(),
                Person {
                    login: login.to_string(),
                    reviewed_365d: *year,
                    reviewed: *all,
                    last_merge_at: merge.map(t),
                    ..Default::default()
                },
            );
        }
        *app.metrics.write().unwrap() = std::sync::Arc::new(report);
        let c: Vec<String> = committers.iter().map(|c| c.to_string()).collect();
        *app.snapshot.write().unwrap() = std::sync::Arc::new(crate::web::Snapshot {
            committers: crate::committer::Committers::build(&[], &[], &c, None, t(0))
                .with_delegates(delegates),
            ..Default::default()
        });
        app
    }

    #[test]
    fn the_queue_holds_every_committer_and_the_reviewers_above_the_line() {
        let now = t(1000);
        let app = app_with(
            &[
                ("Alice", 30, 100, None),
                ("bob", 12, 400, None),
                ("carol", 3, 3, None),
                ("dan", 50, 50, Some(990)),
                ("erin", 0, 10, Some(500)),
                ("frank", 20, 20, Some(100)),
            ],
            &["dan", "erin"],
            &[],
        );
        let logins = |o: Opts| -> Vec<(String, Status)> {
            rows(&app, &o, now)
                .0
                .into_iter()
                .map(|r| (r.person.login, r.status))
                .collect()
        };
        assert_eq!(
            logins(Opts::default()),
            [
                ("dan".to_string(), Status::Active),
                ("Alice".to_string(), Status::Candidate),
                ("frank".to_string(), Status::Former),
                ("bob".to_string(), Status::Candidate),
                ("erin".to_string(), Status::Inactive),
            ],
            "carol is under the line; erin is in as a committer with no reviews"
        );
        assert_eq!(
            logins(Opts {
                status: Some("inactive".into()),
                ..Default::default()
            }),
            [("erin".to_string(), Status::Inactive)]
        );
        assert_eq!(
            logins(Opts {
                min: Some(1),
                sort: Some("reviews_all".into()),
                status: Some("candidate".into()),
                ..Default::default()
            })
            .iter()
            .map(|(l, _)| l.as_str())
            .collect::<Vec<_>>(),
            ["bob", "Alice", "carol"]
        );
        assert_eq!(
            logins(Opts {
                q: Some("ALI".into()),
                ..Default::default()
            })
            .len(),
            1
        );
        let (_, counts) = rows(&app, &Opts::default(), now);
        assert_eq!(counts[&Status::Candidate], 2);
        let v = value(&app, &Opts::default(), None, None, now).unwrap();
        assert_eq!(v["total"], 5);
        assert_eq!(v["items"][0]["login"], "dan");
        assert_eq!(v["items"][0]["status"], "active");
        assert_eq!(v["counts"]["former"], 1);
        assert_eq!(v["min"], DEFAULT_MIN);
    }

    async fn get(app: &Shared, login: Option<&str>, o: Opts) -> Response {
        let mut h = HeaderMap::new();
        if let Some(l) = login {
            let id = format!("s-{l}");
            app.store.create_session(&id, l, "gho_x").unwrap();
            h.insert("cookie", format!("session={id}").parse().unwrap());
        }
        page(State(app.clone()), h, Query(o)).await
    }

    async fn text(r: Response) -> String {
        let b = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(b.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn only_a_delegate_sees_the_page() {
        let app = app_with(&[("alice", 30, 100, None)], &[], &["Philip"]);
        let r = get(&app, None, Opts::default()).await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        let r = get(&app, Some("alice"), Opts::default()).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let h = text(r).await;
        assert!(h.contains(DELEGATION) && !h.contains("href=\"/u/alice\""));
        assert!(
            !h.contains("href=\"/committers\""),
            "no menu entry for alice"
        );
        let r = get(&app, Some("philip"), Opts::default()).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            r.headers().get("cache-control").unwrap(),
            "private, no-store"
        );
        let h = text(r).await;
        assert!(h.contains("href=\"/u/alice\">alice</a>"));
        assert!(h.contains("reviewed <b>30</b> in 12 months"));
        assert!(h.contains("not a committer"));
        assert!(
            h.contains("href=\"/committers\""),
            "the menu links the page"
        );
    }
}
