//! A person's own progress, shown to that person only. It is a pure
//! function of their reviews, from the archive and the ledger, and it
//! compares them with nobody.

use axum::Router;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::dossier::{self, Outcome, Row};
use crate::html::{Html, html};
use crate::ledger::{Credit, MIN_COUNTERPARTIES, WINDOW_DAYS};
use crate::metrics::Facts;
use crate::ui::{Page, Section, Tone, badge, section};
use crate::web::{App, Shared, octicon};

const GOAL: &str = "weekly_goal";
pub const MAX_GOAL: u32 = 50;
pub const AREAS_FOR_BADGE: usize = 5;
const COUNTS: [usize; 3] = [10, 50, 100];
/// The months of the chart, the current one included.
const MONTHS: usize = 12;

pub fn routes() -> Router<Shared> {
    Router::new().route("/me/progress", get(progress_page).post(save_goal))
}

/// One person's reviews over the whole archive: each pull request counts
/// once, at the first review of the person on it.
#[derive(Debug, Default, Clone, Serialize)]
pub struct History {
    pub reviewed: usize,
    pub since: Option<DateTime<Utc>>,
    /// Pull requests reviewed in each month, by `YYYY-MM`.
    pub months: BTreeMap<String, usize>,
    /// The parts of nixpkgs reviewed, as the metrics name them.
    pub areas: usize,
    #[serde(skip)]
    counts: [Option<DateTime<Utc>>; 3],
    #[serde(skip)]
    five_areas: Option<DateTime<Utc>>,
}

pub type Histories = HashMap<String, History>;

/// When a person first reviewed a pull request, and the parts it touches.
type Reviewed<'a> = (DateTime<Utc>, &'a [String]);

/// From the facts of every pull request; it runs with the metrics.
pub fn histories(facts: &[Facts]) -> Histories {
    let mut by: HashMap<&str, Vec<Reviewed>> = HashMap::new();
    for f in facts {
        let mut first: HashMap<&str, DateTime<Utc>> = HashMap::new();
        for r in &f.reviews {
            let at = first.entry(r.by.as_str()).or_insert(r.at);
            *at = (*at).min(r.at);
        }
        for (login, at) in first {
            by.entry(login).or_default().push((at, &f.areas));
        }
    }
    by.into_iter()
        .map(|(login, mut reviews)| {
            reviews.sort_by_key(|(at, _)| *at);
            let mut h = History {
                reviewed: reviews.len(),
                since: reviews.first().map(|(at, _)| *at),
                ..Default::default()
            };
            let mut areas: BTreeSet<&str> = BTreeSet::new();
            for (i, (at, a)) in reviews.iter().enumerate() {
                for (k, n) in COUNTS.iter().enumerate() {
                    if i + 1 == *n {
                        h.counts[k] = Some(*at);
                    }
                }
                *h.months.entry(at.format("%Y-%m").to_string()).or_default() += 1;
                areas.extend(a.iter().map(String::as_str).filter(|a| *a != "other"));
                if h.five_areas.is_none() && areas.len() >= AREAS_FOR_BADGE {
                    h.five_areas = Some(*at);
                }
            }
            h.areas = areas.len();
            (login.to_string(), h)
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct Badge {
    pub id: &'static str,
    pub name: &'static str,
    pub how: &'static str,
    pub earned: Option<DateTime<Utc>>,
    /// What is left to earn it, when that is a count.
    pub left: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Progress {
    pub reviews_week: usize,
    pub reviews_month: usize,
    pub goal: Option<u32>,
    pub counterparties: usize,
    pub listed: bool,
    /// When the pull requests of the person leave the queues if they review
    /// no other author before.
    pub listed_until: Option<DateTime<Utc>>,
    pub all_time: History,
    pub badges: Vec<Badge>,
}

fn week_of(t: DateTime<Utc>) -> NaiveDate {
    let d = t.date_naive();
    d - Duration::days(d.weekday().num_days_from_monday().into())
}

fn same_month(a: DateTime<Utc>, b: DateTime<Utc>) -> bool {
    a.year() == b.year() && a.month() == b.month()
}

fn live<'a>(credits: &[&'a Credit]) -> Vec<&'a Credit> {
    credits
        .iter()
        .copied()
        .filter(|c| c.dropped.is_none())
        .collect()
}

/// The authors counted in the window keep the person listed until the
/// third most recent of them is older than the window.
fn listed_until(credits: &[&Credit], now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let window = Duration::days(WINDOW_DAYS);
    let mut latest: HashMap<&str, DateTime<Utc>> = HashMap::new();
    for c in live(credits).into_iter().filter(|c| now - c.at < window) {
        let at = latest.entry(c.pr_author.as_str()).or_insert(c.at);
        *at = (*at).max(c.at);
    }
    let mut ats: Vec<DateTime<Utc>> = latest.into_values().collect();
    ats.sort_by(|a, b| b.cmp(a));
    ats.get(MIN_COUNTERPARTIES - 1).map(|at| *at + window)
}

/// Everything but the badges, which only the full page needs.
pub fn build(
    rows: &[Row],
    credits: &[&Credit],
    counterparties: usize,
    goal: Option<u32>,
    all_time: History,
    now: DateTime<Utc>,
) -> Progress {
    let this_week = week_of(now);
    let listed = counterparties >= MIN_COUNTERPARTIES;
    Progress {
        reviews_week: rows.iter().filter(|r| week_of(r.at) == this_week).count(),
        reviews_month: rows.iter().filter(|r| same_month(r.at, now)).count(),
        goal,
        counterparties,
        listed,
        listed_until: if listed {
            listed_until(credits, now)
        } else {
            None
        },
        all_time,
        badges: Vec::new(),
    }
}

/// `kept` gives the date of a badge that was earned before: the two that
/// read the window of the ledger would go away with it.
pub fn badges(
    h: &History,
    rows: &[Row],
    credits: &[&Credit],
    kept: impl Fn(&str) -> Option<DateTime<Utc>>,
) -> Vec<Badge> {
    let orphan = live(credits)
        .iter()
        .filter(|c| c.orphan)
        .map(|c| c.at)
        .min();
    let unchanged = rows
        .iter()
        .filter(|r| r.outcome == Outcome::MergedUnchanged)
        .map(|r| r.at)
        .min();
    let count = |k: usize, id: &'static str, name: &'static str, how: &'static str| Badge {
        id,
        name,
        how,
        earned: h.counts[k],
        left: h.counts[k]
            .is_none()
            .then(|| format!("{} to go", COUNTS[k] - h.reviewed.min(COUNTS[k]))),
    };
    vec![
        Badge {
            id: "first",
            name: "First review",
            how: "a review on someone else's pull request",
            earned: h.since,
            left: None,
        },
        count(0, "10", "10 reviews", "reviews on 10 pull requests"),
        count(1, "50", "50 reviews", "reviews on 50 pull requests"),
        count(2, "100", "100 reviews", "reviews on 100 pull requests"),
        Badge {
            id: "five-parts",
            name: "Five parts of nixpkgs",
            how: "reviews in 5 parts of nixpkgs, such as python, NixOS modules or lib",
            earned: h.five_areas,
            left: h
                .five_areas
                .is_none()
                .then(|| format!("{} of {AREAS_FOR_BADGE} parts so far", h.areas)),
        },
        Badge {
            id: "orphan",
            name: "First review with no maintainer",
            how: "a review that led to a change, a reply or a merge on a pull request with no maintainer and no owner",
            earned: kept("orphan").or(orphan),
            left: None,
        },
        Badge {
            id: "unchanged",
            name: "Merged unchanged",
            how: "a committer merged a pull request after your review, with no push after it",
            earned: kept("unchanged").or(unchanged),
            left: None,
        },
    ]
}

fn goal_of(app: &App, login: &str) -> Option<u32> {
    app.store
        .setting(login, GOAL)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .filter(|g| (1..=MAX_GOAL).contains(g))
}

fn badge_key(id: &str) -> String {
    format!("badge_{id}")
}

pub fn of(app: &App, login: &str) -> Progress {
    let s = app.snap();
    let mine = s
        .reviewed_by
        .get(login)
        .into_iter()
        .flatten()
        .map(|&i| &s.pr_data[i]);
    let rows = dossier::rows(mine, login);
    let credits: Vec<&Credit> = s.ledger.credits_of(login).collect();
    let counterparties = s.ledger.people.get(login).map_or(0, |p| p.counterparties);
    let all_time = app
        .reviews
        .read()
        .unwrap()
        .get(login)
        .cloned()
        .unwrap_or_default();
    let mut p = build(
        &rows,
        &credits,
        counterparties,
        goal_of(app, login),
        all_time,
        Utc::now(),
    );
    let kept = |id: &str| {
        app.store
            .setting(login, &badge_key(id))
            .ok()
            .flatten()
            .and_then(|v| v.parse::<DateTime<Utc>>().ok())
    };
    p.badges = badges(&p.all_time, &rows, &credits, kept);
    for b in p
        .badges
        .iter()
        .filter(|b| matches!(b.id, "orphan" | "unchanged"))
    {
        if let (Some(at), None) = (b.earned, kept(b.id)) {
            let _ = app
                .store
                .set_setting(login, &badge_key(b.id), &at.to_rfc3339());
        }
    }
    p
}

fn this_week(p: &Progress) -> Html {
    match p.goal {
        Some(g) => html!(
            "<progress max=\"{g}\" value=\"{}\" aria-label=\"weekly goal\"></progress> {} of your goal of {g} pull requests this week",
            p.reviews_week.min(g as usize),
            p.reviews_week
        ),
        None => html!(
            "{} pull requests reviewed this week. <a href=\"/me/settings#goal\">Set a weekly goal</a>",
            p.reviews_week
        ),
    }
}

/// The last [`MONTHS`] months, oldest first, the current one included.
fn last_months(now: DateTime<Utc>) -> Vec<String> {
    let (mut y, mut m) = (now.year(), now.month());
    let mut out = Vec::new();
    for _ in 0..MONTHS {
        out.push(format!("{y}-{m:02}"));
        if m == 1 {
            (y, m) = (y - 1, 12);
        } else {
            m -= 1;
        }
    }
    out.reverse();
    out
}

fn over_time(app: &App, h: &History) -> Html {
    let Some(since) = h.since else {
        return Html::from("<div class=\"Box-row\">No review yet.</div>");
    };
    let months = last_months(Utc::now());
    let values = months
        .iter()
        .map(|m| Some(h.months.get(m).copied().unwrap_or(0) as f64))
        .collect();
    let chart = crate::chart::Chart {
        id: "reviews-by-month",
        title: "Pull requests reviewed each month",
        note: "The last 12 months. The current month is not over.".into(),
        months: &months,
        form: crate::chart::Form::Bars,
        unit: crate::chart::Unit::Count,
        series: vec![crate::chart::Series {
            name: "reviewed",
            class: "c1",
            values,
        }],
        gap: None,
    }
    .render();
    let partial = if app.history_complete() {
        Html::new()
    } else {
        Html::from(" The archive is still being read, so older reviews can be missing.")
    };
    html!(
        "<div class=\"Box-row\"><strong>{}</strong> pull requests reviewed since {}.{partial}</div><div class=\"Box-row\">{chart}</div>",
        h.reviewed,
        since.format("%B %Y").to_string()
    )
}

fn in_the_queues(p: &Progress) -> Html {
    let why = "<a href=\"/state?tab=rules#2-whose-pull-requests-appear\">Why</a>";
    let text = match (p.listed, p.listed_until) {
        (true, Some(until)) => html!(
            "Your pull requests are in the queues. With no review of another author, they leave them on {}.",
            until.format("%Y-%m-%d").to_string()
        ),
        (true, None) => "Your pull requests are in the queues.".into(),
        (false, _) => html!(
            "Your pull requests are not in the queues yet. Review the pull requests of {} more authors: \
             a review counts when it leads to a change, a reply or a merge.",
            MIN_COUNTERPARTIES.saturating_sub(p.counterparties)
        ),
    };
    html!("<div class=\"Box-row\">{text} {}</div>", Html::from(why))
}

/// `lead` comes before the name, such as "Next:".
fn badge_row(b: &Badge, lead: &str) -> Html {
    let state = match (b.earned, &b.left) {
        (Some(t), _) => badge(
            Tone::Success,
            Html::text(&format!("earned {}", t.format("%Y-%m-%d"))),
        ),
        (None, Some(left)) => badge(Tone::Neutral, Html::text(left)),
        (None, None) => badge(Tone::Neutral, "not yet"),
    };
    html!(
        "<div class=\"Box-row d-flex\"><div class=\"flex-auto\"><span class=\"color-fg-muted\">{lead}</span><span class=\"text-bold\">{}</span>\
         <div class=\"f6 color-fg-muted\">{}</div></div><div>{state}</div></div>",
        b.name,
        b.how
    )
}

/// The badges earned, then the next one; the whole list folds.
fn badges_box(p: &Progress) -> Html {
    let mut earned: Vec<&Badge> = p.badges.iter().filter(|b| b.earned.is_some()).collect();
    earned.sort_by_key(|b| b.earned);
    let mut out: Html = earned.iter().map(|b| badge_row(b, "")).collect();
    if earned.is_empty() {
        out.push(&Html::from(
            "<div class=\"Box-row color-fg-muted\">No badge yet.</div>",
        ));
    }
    if let Some(next) = p.badges.iter().find(|b| b.earned.is_none()) {
        out.push(&badge_row(next, "Next: "));
    }
    let all: Html = p.badges.iter().map(|b| badge_row(b, "")).collect();
    out.push(&html!(
        "<div class=\"Box-row\"><details class=\"fold\"><summary class=\"f6\">All {} badges</summary>{all}</details></div>",
        p.badges.len()
    ));
    out
}

async fn progress_page(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let p = of(&app, &login);
    let mut out = section(
        "This week",
        None,
        p.goal.map(|_| ("/me/settings#goal", "change your goal")),
        html!("<div class=\"Box-row\">{}</div>", this_week(&p)),
    );
    out.push(&html!(
        "<div id=\"over-time\">{}</div>",
        section("Over time", None, None, over_time(&app, &p.all_time))
    ));
    out.push(&html!(
        "<div id=\"listing\">{}</div>",
        section("In the queues", None, None, in_the_queues(&p))
    ));
    out.push(&html!(
        "<div id=\"badges\">{}</div>",
        section("Badges", None, None, badges_box(&p))
    ));
    Page::new(
        Section::Personal(crate::ui::Personal::Progress),
        "Your progress",
    )
    .icon(octicon!("pulse"))
    .sub(html!(
        "Only you see this page. Your public record is on <a href=\"/u/{l}\">/u/{l}</a>. \
         <a href=\"{}\">How nixpkgs reviews</a>",
        crate::web::CONTRIBUTING,
        l = login
    ))
    .body(out)
    .render(&app, Some(&login))
}

/// The `#goal` section of `/me/settings`.
pub(crate) fn goal_box(app: &App, login: &str) -> Html {
    let goal = goal_of(app, login).unwrap_or(0);
    html!(
        "<form method=\"post\" action=\"/me/progress\" class=\"d-flex flex-wrap flex-items-center gap-2\">\
         <label for=\"goal-n\">Pull requests to review each week</label>\
         <input class=\"form-control input-narrow\" type=\"number\" min=\"0\" max=\"{MAX_GOAL}\" id=\"goal-n\" name=\"goal\" value=\"{goal}\">\
         <button class=\"btn btn-sm\" type=\"submit\">Save</button></form>\
         <p class=\"note f6 color-fg-muted mt-2\">Monday to Sunday, UTC. 0 removes the goal. \
         A careful review counts for more than many quick ones. <a href=\"/me/progress\">Your progress</a> shows how far you are.</p>"
    )
}

#[derive(Deserialize)]
struct GoalForm {
    goal: String,
}

async fn save_goal(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<GoalForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let value = match f.goal.trim().parse::<u32>() {
        Ok(0) => String::new(),
        Ok(g) if g <= MAX_GOAL => g.to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                format!("the goal is a number from 0 to {MAX_GOAL}"),
            )
                .into_response();
        }
    };
    if let Err(e) = app.store.set_setting(&login, GOAL, &value) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    Redirect::to("/me/settings#goal").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Trigger;
    use crate::ledger::tests::{pr, review, t};

    fn row(pr: u64, day: i64, outcome: Outcome) -> Row {
        Row {
            pr,
            pr_title: String::new(),
            pr_author: "alice".into(),
            review_url: String::new(),
            at: t(day),
            state: "COMMENTED".into(),
            outcome,
            problem_found: false,
        }
    }

    fn credit(author: &str, day: i64, orphan: bool) -> Credit {
        Credit {
            reviewer: "bob".into(),
            pr: 1,
            pr_title: String::new(),
            pr_author: author.into(),
            review_url: String::new(),
            at: t(day),
            triggers: vec![Trigger::InlineOnChange],
            queue_head: true,
            orphan,
            pair_factor: 1.0,
            weight: 1.0,
            value: 1.0,
            dropped: None,
        }
    }

    #[test]
    fn the_week_starts_on_monday_and_the_month_on_the_first() {
        // t(0) is Monday 2026-06-01
        let rows = [
            row(1, -1, Outcome::Open),
            row(2, 0, Outcome::Open),
            row(3, 3, Outcome::Open),
            row(4, 8, Outcome::Open),
        ];
        let p = build(&rows, &[], 0, None, History::default(), t(6));
        assert_eq!(p.reviews_week, 2);
        assert_eq!(p.reviews_month, 3);
        assert!(!p.listed && p.listed_until.is_none());
    }

    #[test]
    fn the_history_counts_each_pull_request_once_over_the_whole_archive() {
        let facts: Vec<Facts> = (1..=12)
            .map(|n| {
                let mut p = pr(n, "alice", 0);
                p.files = vec![
                    match n {
                        1 => "pkgs/development/python-modules/a/default.nix",
                        2 => "nixos/modules/a.nix",
                        3 => "lib/a.nix",
                        4 => "doc/a.md",
                        _ => "pkgs/top-level/perl-packages.nix",
                    }
                    .into(),
                ];
                p.reviews = vec![
                    review(n, "bob", n as i64 * 40),
                    review(n + 100, "bob", n as i64 * 40 + 1),
                ];
                Facts::from(&p)
            })
            .collect();
        let h = &histories(&facts)["bob"];
        assert_eq!(h.reviewed, 12, "two reviews on one pull request count once");
        assert_eq!(h.since, Some(t(40)));
        assert_eq!(h.counts[0], Some(t(400)));
        assert_eq!(h.counts[1], None);
        assert!(h.five_areas.is_some_and(|at| at <= t(200)));
        assert_eq!(h.months.values().sum::<usize>(), 12);
        let b = badges(h, &[], &[], |_| None);
        let get = |id: &str| b.iter().find(|b| b.id == id).unwrap();
        assert_eq!(get("first").earned, Some(t(40)));
        assert_eq!(get("10").earned, Some(t(400)));
        assert_eq!(get("50").left.as_deref(), Some("38 to go"));
        assert_eq!(get("orphan").earned, None);
    }

    #[test]
    fn a_badge_of_the_window_stays_once_earned() {
        let h = History::default();
        let rows = [row(20, 11, Outcome::MergedUnchanged)];
        let credits = [credit("alice", 3, true)];
        let refs: Vec<&Credit> = credits.iter().collect();
        let b = badges(&h, &rows, &refs, |_| None);
        let get = |b: &[Badge], id: &str| b.iter().find(|x| x.id == id).unwrap().earned;
        assert_eq!(get(&b, "orphan"), Some(t(3)));
        assert_eq!(get(&b, "unchanged"), Some(t(11)));
        let later = badges(&h, &[], &[], |id| (id == "orphan").then(|| t(3)));
        assert_eq!(get(&later, "orphan"), Some(t(3)), "kept after the window");
        assert_eq!(get(&later, "unchanged"), None);
        assert_eq!(
            later
                .iter()
                .find(|x| x.id == "five-parts")
                .unwrap()
                .left
                .as_deref(),
            Some("0 of 5 parts so far")
        );
    }

    #[test]
    fn the_queues_keep_a_person_until_the_third_latest_author_ages_out() {
        let credits = [
            credit("ann", 10, false),
            credit("ann", 30, false),
            credit("cat", 20, false),
            credit("dan", 25, false),
            credit("eve", 5, false),
        ];
        let refs: Vec<&Credit> = credits.iter().collect();
        let p = build(&[], &refs, 4, None, History::default(), t(40));
        assert!(p.listed);
        assert_eq!(p.listed_until, Some(t(20) + Duration::days(WINDOW_DAYS)));
        let p = build(&[], &refs, 2, None, History::default(), t(40));
        assert!(!p.listed && p.listed_until.is_none());
    }

    #[test]
    fn over_time_draws_the_last_twelve_months() {
        let app = crate::sets::tests::app(&[]);
        assert!(
            over_time(&app, &History::default())
                .to_string()
                .contains("No review yet.")
        );
        let now = Utc::now().format("%Y-%m").to_string();
        let h = History {
            reviewed: 12,
            since: Some(t(0)),
            months: [(now, 4)].into(),
            ..Default::default()
        };
        let html = over_time(&app, &h).to_string();
        assert!(
            html.contains("<strong>12</strong> pull requests reviewed since June 2026."),
            "{html}"
        );
        assert!(html.contains("id=\"reviews-by-month\"") && !html.contains("NaN"));
    }
}
