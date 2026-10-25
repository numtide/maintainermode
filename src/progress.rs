//! A person's own progress, shown to that person only. It is a pure
//! function of their review rows and the ledger, and it compares them with
//! nobody.

use axum::Router;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::dossier::{self, Outcome, Row};
use crate::html::{Html, html};
use crate::ledger::{Credit, MIN_COUNTERPARTIES, WINDOW_DAYS};
use crate::ui::{Page, Section, Tone, badge, section};
use crate::web::{App, Shared, icon, octicon};

const GOAL: &str = "weekly_goal";
pub const MAX_GOAL: u32 = 50;
pub const AREAS_FOR_BADGE: usize = 5;

pub fn routes() -> Router<Shared> {
    Router::new().route("/me/progress", get(progress_page).post(save_goal))
}

#[derive(Debug, Serialize)]
pub struct Badge {
    pub name: &'static str,
    pub how: &'static str,
    pub earned: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct Progress {
    pub reviews_week: usize,
    pub reviews_month: usize,
    pub goal: Option<u32>,
    pub counterparties: usize,
    pub listed: bool,
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

/// Everything but the badges, which only the full page needs.
pub fn build(
    rows: &[Row],
    counterparties: usize,
    goal: Option<u32>,
    now: DateTime<Utc>,
) -> Progress {
    let this_week = week_of(now);
    Progress {
        reviews_week: rows.iter().filter(|r| week_of(r.at) == this_week).count(),
        reviews_month: rows.iter().filter(|r| same_month(r.at, now)).count(),
        goal,
        counterparties,
        listed: counterparties >= MIN_COUNTERPARTIES,
        badges: Vec::new(),
    }
}

/// `areas` gives the areas a pull request touches; it runs only until the
/// fifth area shows up.
pub fn badges(rows: &[Row], credits: &[&Credit], areas: impl Fn(u64) -> Vec<String>) -> Vec<Badge> {
    let live = live(credits);
    let mut oldest_first: Vec<&Row> = rows.iter().collect();
    oldest_first.sort_by_key(|r| r.at);
    let nth = |n: usize| oldest_first.get(n - 1).map(|r| r.at);
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut five_areas = None;
    for r in &oldest_first {
        seen.extend(areas(r.pr).into_iter().filter(|a| a != "other"));
        if seen.len() >= AREAS_FOR_BADGE {
            five_areas = Some(r.at);
            break;
        }
    }
    vec![
        Badge {
            name: "First review",
            how: "a review on someone else's pull request",
            earned: nth(1),
        },
        Badge {
            name: "First review with no maintainer",
            how: "a review that led to a change, a reply or a merge on a pull request with no maintainer and no owner",
            earned: live.iter().filter(|c| c.orphan).map(|c| c.at).min(),
        },
        Badge {
            name: "Merged unchanged",
            how: "a committer merged a pull request after your review, with no push after it",
            earned: oldest_first
                .iter()
                .find(|r| r.outcome == Outcome::MergedUnchanged)
                .map(|r| r.at),
        },
        Badge {
            name: "10 reviews",
            how: "reviews on 10 pull requests",
            earned: nth(10),
        },
        Badge {
            name: "50 reviews",
            how: "reviews on 50 pull requests",
            earned: nth(50),
        },
        Badge {
            name: "100 reviews",
            how: "reviews on 100 pull requests",
            earned: nth(100),
        },
        Badge {
            name: "Five parts of nixpkgs",
            how: "reviews in 5 parts of nixpkgs, such as python, NixOS modules or lib",
            earned: five_areas,
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

pub fn of(app: &App, login: &str, with_badges: bool) -> Progress {
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
    let mut p = build(&rows, counterparties, goal_of(app, login), Utc::now());
    if with_badges {
        p.badges = badges(&rows, &credits, |n| {
            s.pr_at
                .get(&n)
                .map(|&i| crate::metrics::areas(&s.pr_data[i].files))
                .unwrap_or_default()
        });
    }
    p
}

fn goal_line(p: &Progress) -> Html {
    match p.goal {
        Some(g) => html!(
            "<progress max=\"{g}\" value=\"{}\" aria-label=\"weekly goal\"></progress> {} of your goal of {g} this week",
            p.reviews_week.min(g as usize),
            p.reviews_week
        ),
        None => "No weekly goal yet.".into(),
    }
}

fn listing_line(p: &Progress) -> String {
    if p.listed {
        "Your pull requests show in the queues.".to_string()
    } else {
        format!(
            "{} more authors until your pull requests show in the queues. In the last {3} days, your reviews led to a change, a reply or a merge on the pull requests of {1} of {2} authors.",
            MIN_COUNTERPARTIES - p.counterparties,
            p.counterparties,
            MIN_COUNTERPARTIES,
            WINDOW_DAYS
        )
    }
}

fn stat(n: usize, label: &str) -> Html {
    html!(
        "<div class=\"Box p-3 flex-1\"><div class=\"f1 text-bold\">{n}</div><div class=\"f6\">{label}</div></div>"
    )
}

async fn progress_page(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let p = of(&app, &login, true);
    let mut out = html!(
        "<div class=\"d-flex flex-wrap gap-3 mb-4\">{}{}</div>",
        stat(p.reviews_week, "pull requests reviewed this week"),
        stat(p.reviews_month, "pull requests reviewed this month"),
    );
    out.push(&section(
        "Your weekly goal",
        None,
        Some(("/me/settings#goal", "change it")),
        html!("<div class=\"Box-row\">{}</div>", goal_line(&p)),
    ));
    out.push(&html!(
        "<div id=\"listing\">{}</div>",
        section(
            "In the queues",
            None,
            None,
            html!(
                "<div class=\"Box-row\">{} <a href=\"/state?tab=rules#2-whose-pull-requests-appear\">Why</a></div>",
                listing_line(&p)
            )
        )
    ));
    let mut badges = Html::new();
    for b in &p.badges {
        let state = match b.earned {
            Some(t) => badge(
                Tone::Success,
                Html::text(&format!("earned {}", t.format("%Y-%m-%d"))),
            ),
            None => badge(Tone::Neutral, "not yet"),
        };
        badges.push(&html!(
            "<div class=\"Box-row d-flex\"><div class=\"flex-auto\"><span class=\"text-bold\">{}</span>\
             <div class=\"f6 color-fg-muted\">{}</div></div><div>{state}</div></div>",
            b.name,
            b.how
        ));
    }
    out.push(&html!(
        "<div id=\"badges\">{}</div>",
        section("Badges", None, None, badges)
    ));
    out.push(&crate::web::nixpkgs_guides());
    Page::new(
        Section::Personal(crate::ui::Personal::Progress),
        "Your progress",
    )
    .icon(octicon!("pulse"))
    .sub(html!(
        "Only you see this page. \
             It counts the reviews of the last {WINDOW_DAYS} days and on every open pull request. \
             Your public record is on <a href=\"/u/{l}\">/u/{l}</a>.",
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
    use crate::ledger::tests::t;

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

    fn credit(pr: u64, day: i64, orphan: bool) -> Credit {
        Credit {
            reviewer: "bob".into(),
            pr,
            pr_title: String::new(),
            pr_author: "alice".into(),
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
        let p = build(&rows, 0, None, t(6));
        assert_eq!(p.reviews_week, 2);
        assert_eq!(p.reviews_month, 3);
        assert!(!p.listed);
        assert!(build(&rows, 3, None, t(6)).listed);
    }

    #[test]
    fn badges_come_from_the_own_record() {
        let mut rows: Vec<Row> = (0..10).map(|i| row(i, i as i64, Outcome::Open)).collect();
        rows.push(row(20, 11, Outcome::MergedUnchanged));
        rows.push(row(21, 12, Outcome::Open));
        rows.push(row(22, 13, Outcome::Open));
        let credits = [credit(1, 3, true)];
        let refs: Vec<&Credit> = credits.iter().collect();
        let asked = std::cell::RefCell::new(Vec::new());
        let b = badges(&rows, &refs, |n| {
            asked.borrow_mut().push(n);
            let a: &[&str] = match n {
                20 => &["lib", "doc", "ci"],
                21 => &["nixos: modules", "other"],
                22 => &["perl"],
                _ => &["packages: python"],
            };
            a.iter().map(|s| s.to_string()).collect()
        });
        let earned = |name: &str| b.iter().find(|b| b.name == name).unwrap().earned;
        assert_eq!(earned("First review"), Some(t(0)));
        assert_eq!(earned("10 reviews"), Some(t(9)));
        assert_eq!(earned("50 reviews"), None);
        assert_eq!(earned("First review with no maintainer"), Some(t(3)));
        assert_eq!(earned("Merged unchanged"), Some(t(11)));
        assert_eq!(earned("Five parts of nixpkgs"), Some(t(12)));
        assert!(!asked.borrow().contains(&22));
        let p = build(&rows, 1, Some(3), t(12));
        assert!(p.badges.is_empty());
        assert!(!p.listed);
    }
}
