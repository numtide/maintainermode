//! Your own pull requests: what happened to them, and a follow-up plan for
//! each. A plan says what reminder the author gets when nobody reviews in
//! some days. Maintainer Mode never merges and never comments: a plan only
//! decides a reminder, and the reminder links to GitHub.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::collections::HashMap;

use crate::catalog::Event;
use crate::model::{Pr, Review};
use crate::web::App;

pub(crate) const DAYS: u32 = 7;
pub(crate) const SUBJECT: &str = "Your pull requests";
const DAYS_KEY: &str = "followup_days";
const ACTION_KEY: &str = "followup_action";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    Ask,
    Merge,
    Nothing,
}

impl Action {
    pub(crate) const ALL: [Action; 3] = [Action::Ask, Action::Merge, Action::Nothing];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Action::Ask => "ask",
            Action::Merge => "merge",
            Action::Nothing => "none",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.key() == s)
    }

    fn reminder(self) -> &'static str {
        match self {
            Action::Ask => "a reminder to ask the maintainers",
            Action::Merge => "a reminder to merge it yourself",
            Action::Nothing => "no reminder",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Source {
    /// Set on the page of the pull request.
    PullRequest,
    /// The default the person saved in the settings.
    Default,
    /// Nothing saved: the built-in default, which makes no event.
    BuiltIn,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Plan {
    pub(crate) days: u32,
    pub(crate) action: Action,
    pub(crate) source: Source,
    /// For a plan set on the pull request: when the author first set it.
    pub(crate) since: Option<DateTime<Utc>>,
}

impl Plan {
    /// A plan set on the page counts from the time it was set: "if nobody
    /// reviews in 7 days" means 7 days from now. A default counts from
    /// the opening of the pull request.
    pub(crate) fn deadline(&self, pr: &Pr) -> DateTime<Utc> {
        self.since.unwrap_or(pr.created_at).max(pr.created_at)
            + Duration::days(i64::from(self.days))
    }

    /// Only a plan that the person chose makes inbox events and the item
    /// at the top of Needs you.
    pub(crate) fn chosen(&self) -> bool {
        self.source != Source::BuiltIn
    }
}

/// The plans of one person.
#[derive(Clone, Debug)]
pub(crate) struct Plans {
    pub(crate) by_pr: HashMap<u64, Plan>,
    pub(crate) default: Plan,
    pub(crate) committer: bool,
}

impl Plans {
    pub(crate) fn built_in(committer: bool) -> Plan {
        Plan {
            days: DAYS,
            action: if committer {
                Action::Merge
            } else {
                Action::Ask
            },
            source: Source::BuiltIn,
            since: None,
        }
    }

    pub(crate) fn of(app: &App, login: &str, committer: bool) -> Plans {
        let mut default = Plans::built_in(committer);
        let days = app.store.setting(login, DAYS_KEY).ok().flatten();
        let action = app.store.setting(login, ACTION_KEY).ok().flatten();
        if let (Some(d), Some(a)) = (
            days.and_then(|d| d.parse::<u32>().ok()),
            action.as_deref().and_then(Action::parse),
        ) {
            default = Plan {
                days: d,
                action: a,
                source: Source::Default,
                since: None,
            };
        }
        let by_pr = app
            .store
            .followups(login)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|f| {
                Some((
                    f.number,
                    Plan {
                        days: f.days,
                        action: Action::parse(&f.action)?,
                        source: Source::PullRequest,
                        since: Some(f.since),
                    },
                ))
            })
            .collect();
        Plans {
            by_pr,
            default,
            committer,
        }
    }

    pub(crate) fn get(&self, n: u64) -> Plan {
        self.by_pr.get(&n).copied().unwrap_or(self.default)
    }
}

/// Reviews by someone else: not a bot, not the author, not dismissed.
pub(crate) fn others(pr: &Pr) -> impl Iterator<Item = &Review> {
    pr.reviews.iter().filter(move |r| {
        !r.author_is_bot && !r.author.eq_ignore_ascii_case(&pr.author) && r.state != "DISMISSED"
    })
}

pub(crate) fn verb(state: &str) -> &'static str {
    match state {
        "APPROVED" => "approved",
        "CHANGES_REQUESTED" => "requested changes",
        _ => "commented",
    }
}

/// Why Maintainer Mode does not suggest a merge, one short phrase each.
/// Empty: every condition it can see holds. GitHub and nixpkgs still
/// decide.
pub(crate) fn merge_gate(pr: &Pr, committer: bool) -> Vec<&'static str> {
    let mut out = Vec::new();
    if !committer {
        out.push("this site does not know you as a committer");
    }
    match pr.ci.as_deref() {
        Some("SUCCESS") => {}
        Some("FAILURE" | "ERROR") => out.push("checks fail"),
        Some("PENDING" | "EXPECTED") => out.push("checks are pending"),
        _ => out.push("the state of the checks is unknown"),
    }
    match pr.mergeable.as_str() {
        "MERGEABLE" => {}
        "CONFLICTING" => out.push("it has a merge conflict"),
        _ => out.push("GitHub does not know yet if it can merge"),
    }
    if pr.is_draft {
        out.push("it is a draft");
    }
    if !crate::prpanel::changes_requested(pr).is_empty() {
        out.push("changes are requested");
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Status {
    Merged,
    Closed,
    /// Someone else reviewed: the plan is done.
    Reviewed {
        by: String,
        at: DateTime<Utc>,
    },
    Draft,
    Off,
    Waiting(DateTime<Utc>),
    /// The deadline passed with no review. `blocked` lists why a merge is
    /// not suggested; it is empty for a plan that only asks.
    Due {
        deadline: DateTime<Utc>,
        blocked: Vec<&'static str>,
    },
}

pub(crate) fn status(pr: &Pr, plan: &Plan, committer: bool, now: DateTime<Utc>) -> Status {
    if pr.state == "MERGED" || pr.merged_at.is_some() {
        return Status::Merged;
    }
    if !pr.is_open() {
        return Status::Closed;
    }
    if let Some(r) = others(pr).min_by_key(|r| r.submitted_at) {
        return Status::Reviewed {
            by: r.author.clone(),
            at: r.submitted_at,
        };
    }
    if pr.is_draft {
        return Status::Draft;
    }
    if plan.action == Action::Nothing {
        return Status::Off;
    }
    let deadline = plan.deadline(pr);
    if now < deadline {
        return Status::Waiting(deadline);
    }
    Status::Due {
        deadline,
        blocked: if plan.action == Action::Merge {
            merge_gate(pr, committer)
        } else {
            Vec::new()
        },
    }
}

fn day(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d").to_string()
}

fn quiet_days(pr: &Pr, now: DateTime<Utc>) -> i64 {
    (now - pr.created_at).num_days().max(0)
}

/// One plain sentence or two for the page and Needs you.
pub(crate) fn sentence(pr: &Pr, plan: &Plan, st: &Status, now: DateTime<Utc>) -> String {
    match st {
        Status::Merged => "It is merged.".into(),
        Status::Closed => "It is closed.".into(),
        Status::Reviewed { by, at } => {
            format!("{by} reviewed it on {}. No reminder comes.", day(*at))
        }
        Status::Draft => "It is a draft. No reminder comes while it is a draft.".into(),
        Status::Off => "No review yet. You chose no reminder.".into(),
        Status::Waiting(d) => format!(
            "No review yet. On {} you get {}.",
            day(*d),
            plan.action.reminder()
        ),
        Status::Due { blocked, .. } => {
            let n = quiet_days(pr, now);
            match plan.action {
                Action::Merge if blocked.is_empty() => {
                    format!("No review in {n} days. You can merge it yourself.")
                }
                Action::Merge => format!(
                    "No review in {n} days. Do not merge it yet: {}.",
                    blocked.join(", ")
                ),
                _ => format!("No review in {n} days. Ask its maintainers for a review."),
            }
        }
    }
}

/// The events of the person's own pull requests since `since`: reviews
/// by someone else, the merge, the close, and the deadline of a plan the
/// person chose. Comments outside a review are not in the synced data.
pub(crate) fn events<'a>(
    prs: impl IntoIterator<Item = &'a Pr>,
    login: &str,
    plans: &Plans,
    now: DateTime<Utc>,
    since: DateTime<Utc>,
) -> Vec<Event> {
    let mut out = Vec::new();
    for pr in prs
        .into_iter()
        .filter(|p| p.author.eq_ignore_ascii_case(login))
    {
        let n = pr.number;
        let plan = plans.get(n);
        let mut push = |at: DateTime<Utc>, id: String, title: String, url: &str| {
            if at >= since && at <= now {
                out.push(Event {
                    at,
                    id: format!("mine-{n}-{id}"),
                    title: format!("{title}: #{n} {}", pr.title),
                    url: url.to_string(),
                    subject: SUBJECT.into(),
                });
            }
        };
        let first = others(pr).map(|r| r.submitted_at).min();
        for r in others(pr) {
            let mut title = format!("{} {}", r.author, verb(&r.state));
            if plan.chosen()
                && plan.action != Action::Nothing
                && first == Some(r.submitted_at)
                && r.submitted_at < plan.deadline(pr)
            {
                title.push_str(", so no reminder comes");
            }
            let id = match r.id() {
                "" => r.submitted_at.timestamp().to_string(),
                x => x.to_string(),
            };
            push(r.submitted_at, format!("review-{id}"), title, &r.url);
        }
        if let Some(t) = pr.merged_at {
            let title = match pr.merged_by.as_deref() {
                Some(by) if by.eq_ignore_ascii_case(login) => "you merged it".to_string(),
                Some(by) => format!("merged by {by}"),
                None => "merged".to_string(),
            };
            push(t, "merged".into(), title, &pr.url);
        } else if let Some(t) = pr.closed_at.filter(|_| !pr.is_open()) {
            push(t, "closed".into(), "closed".into(), &pr.url);
        }
        if !plan.chosen() {
            continue;
        }
        if let st @ Status::Due { deadline, .. } = status(pr, &plan, plans.committer, now) {
            let text = sentence(pr, &plan, &st, now);
            let text = text.trim_end_matches('.');
            push(
                deadline,
                format!("due-{}", day(deadline)),
                text.to_string(),
                &pr.url,
            );
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.at));
    out
}

/// The events of one person's own pull requests in the last `days`.
pub(crate) fn own_events(app: &App, login: &str, days: i64) -> Vec<Event> {
    let s = app.snap();
    let plans = Plans::of(app, login, s.committers.is(login));
    let now = Utc::now();
    events(&s.pr_data, login, &plans, now, now - Duration::days(days))
}

/// Removes the plans of pull requests that left the synced data. With no
/// synced data yet, it keeps everything.
pub(crate) fn purge(app: &App) {
    let s = app.snap();
    if s.pr_data.is_empty() {
        return;
    }
    let keep: Vec<u64> = s.pr_data.iter().map(|p| p.number).collect();
    if let Err(e) = app.store.purge_followups(&keep) {
        tracing::warn!("follow-up plans: {e:#}");
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ledger::tests::{pr, review};

    fn now() -> DateTime<Utc> {
        "2026-10-17T12:00:00Z".parse().unwrap()
    }

    fn days_ago(d: i64) -> DateTime<Utc> {
        now() - Duration::days(d)
    }

    pub(crate) fn mine(n: u64, opened: i64) -> Pr {
        let mut p = pr(n, "alice", 0);
        p.created_at = days_ago(opened);
        p.updated_at = days_ago(0);
        p.title = format!("change {n}");
        p
    }

    fn by(p: &Pr, who: &str, state: &str, at: DateTime<Utc>, id: u64) -> Review {
        let mut r = review(id, who, 0);
        r.state = state.into();
        r.submitted_at = at;
        r.url = format!("{}#pullrequestreview-{id}", p.url);
        r
    }

    fn plan(days: u32, action: Action, since: Option<DateTime<Utc>>) -> Plan {
        Plan {
            days,
            action,
            source: if since.is_some() {
                Source::PullRequest
            } else {
                Source::Default
            },
            since,
        }
    }

    fn plans(list: &[(u64, Plan)], committer: bool) -> Plans {
        Plans {
            by_pr: list.iter().copied().collect(),
            default: Plans::built_in(committer),
            committer,
        }
    }

    fn ids(list: &[Event]) -> Vec<&str> {
        list.iter().map(|e| e.id.as_str()).collect()
    }

    #[test]
    fn reviews_by_others_merges_and_closes_are_events_and_self_reviews_are_not() {
        let mut a = mine(1, 10);
        let r = by(&a, "bob", "APPROVED", days_ago(2), 11);
        a.reviews.push(r);
        let r = by(&a, "alice", "COMMENTED", days_ago(3), 12);
        a.reviews.push(r);
        let mut bot = by(&a, "r-ryantm", "COMMENTED", days_ago(3), 13);
        bot.author_is_bot = true;
        a.reviews.push(bot);
        let r = by(&a, "carol", "DISMISSED", days_ago(4), 14);
        a.reviews.push(r);
        a.state = "MERGED".into();
        a.merged_at = Some(days_ago(1));
        a.merged_by = Some("dave".into());
        let mut b = mine(2, 10);
        b.state = "CLOSED".into();
        b.closed_at = Some(days_ago(1) - Duration::hours(1));
        let mut c = mine(3, 10);
        c.state = "MERGED".into();
        c.merged_at = Some(days_ago(5));
        c.merged_by = Some("Alice".into());
        let mut old = mine(4, 90);
        let r = by(&old, "bob", "CHANGES_REQUESTED", days_ago(40), 41);
        old.reviews.push(r);
        let mut theirs = pr(5, "bob", 0);
        theirs.created_at = days_ago(3);
        let r = by(&theirs, "carol", "APPROVED", days_ago(1), 51);
        theirs.reviews.push(r);
        let list = events(
            &[a, b, c, old, theirs],
            "alice",
            &plans(&[], false),
            now(),
            days_ago(30),
        );
        assert_eq!(
            ids(&list),
            [
                "mine-1-merged",
                "mine-2-closed",
                "mine-1-review-11",
                "mine-3-merged"
            ]
        );
        assert_eq!(list[0].title, "merged by dave: #1 change 1");
        assert_eq!(list[1].title, "closed: #2 change 2");
        assert_eq!(list[2].title, "bob approved: #1 change 1");
        assert_eq!(
            list[2].url,
            "https://github.com/NixOS/nixpkgs/pull/1#pullrequestreview-11"
        );
        assert_eq!(list[3].title, "you merged it: #3 change 3");
        assert!(list.iter().all(|e| e.subject == SUBJECT));
        assert!(
            list.iter()
                .all(|e| crate::notify::Kind::of(&e.id) == crate::notify::Kind::Mine)
        );
    }

    #[test]
    fn a_plan_counts_from_the_time_it_was_set_and_a_default_from_the_opening() {
        let p = mine(1, 10);
        assert_eq!(plan(7, Action::Ask, None).deadline(&p), days_ago(3));
        assert_eq!(
            plan(7, Action::Ask, Some(days_ago(2))).deadline(&p),
            now() + Duration::days(5)
        );
        assert_eq!(
            plan(3, Action::Ask, Some(days_ago(20))).deadline(&p),
            days_ago(7)
        );
        assert_eq!(Plans::built_in(true).action, Action::Merge);
        assert_eq!(Plans::built_in(false).action, Action::Ask);
        assert!(!Plans::built_in(false).chosen());
    }

    #[test]
    fn the_deadline_gives_the_status_and_the_sentence() {
        let p = mine(1, 2);
        let ask = plan(7, Action::Merge, Some(now()));
        let st = status(&p, &ask, true, now());
        assert_eq!(st, Status::Waiting(now() + Duration::days(7)));
        assert_eq!(
            sentence(&p, &ask, &st, now()),
            "No review yet. On 2026-10-24 you get a reminder to merge it yourself."
        );
        let later = now() + Duration::days(7);
        let st = status(&p, &ask, true, later);
        assert_eq!(
            st,
            Status::Due {
                deadline: later,
                blocked: vec![]
            }
        );
        assert_eq!(
            sentence(&p, &ask, &st, later),
            "No review in 9 days. You can merge it yourself."
        );
        let a = plan(7, Action::Ask, Some(now()));
        let st = status(&p, &a, false, later);
        assert!(sentence(&p, &a, &st, later).ends_with("Ask its maintainers for a review."));
        let off = plan(7, Action::Nothing, None);
        assert_eq!(status(&p, &off, true, later), Status::Off);
        let mut d = p.clone();
        d.is_draft = true;
        assert_eq!(status(&d, &a, true, later), Status::Draft);
    }

    #[test]
    fn each_failing_condition_stops_the_merge_suggestion() {
        let ok = mine(1, 10);
        assert!(merge_gate(&ok, true).is_empty());
        assert_eq!(
            merge_gate(&ok, false),
            ["this site does not know you as a committer"]
        );
        type Case = (&'static str, fn(&mut Pr));
        let cases: [Case; 7] = [
            ("checks fail", |p| p.ci = Some("FAILURE".into())),
            ("checks are pending", |p| p.ci = Some("PENDING".into())),
            ("the state of the checks is unknown", |p| p.ci = None),
            ("it has a merge conflict", |p| {
                p.mergeable = "CONFLICTING".into()
            }),
            ("GitHub does not know yet if it can merge", |p| {
                p.mergeable = "UNKNOWN".into()
            }),
            ("it is a draft", |p| p.is_draft = true),
            ("changes are requested", |p| {
                let r = by(p, "bob", "CHANGES_REQUESTED", days_ago(1), 9);
                p.reviews.push(r);
            }),
        ];
        for (want, change) in cases {
            let mut p = ok.clone();
            change(&mut p);
            assert_eq!(merge_gate(&p, true), [want], "{want}");
        }
        let mut p = ok.clone();
        p.ci = Some("ERROR".into());
        let m = plan(7, Action::Merge, None);
        let st = status(&p, &m, true, now());
        assert_eq!(
            sentence(&p, &m, &st, now()),
            "No review in 10 days. Do not merge it yet: checks fail."
        );
        let st = status(&ok, &m, false, now());
        assert!(sentence(&ok, &m, &st, now()).contains("does not know you as a committer"));
    }

    #[test]
    fn a_review_before_the_deadline_cancels_the_reminder() {
        let mut p = mine(1, 2);
        let set = plan(7, Action::Merge, Some(days_ago(1)));
        let list = |p: &Pr, at: DateTime<Utc>| {
            events(
                std::slice::from_ref(p),
                "alice",
                &plans(&[(1, set)], true),
                at,
                at - Duration::days(30),
            )
        };
        let after = now() + Duration::days(10);
        assert_eq!(ids(&list(&p, after)), ["mine-1-due-2026-10-23"]);
        assert_eq!(
            list(&p, after)[0].title,
            "No review in 12 days. You can merge it yourself: #1 change 1"
        );
        assert!(list(&p, now()).is_empty());
        let r = by(&p, "bob", "COMMENTED", now() + Duration::days(2), 7);
        p.reviews.push(r);
        let got = list(&p, after);
        assert_eq!(ids(&got), ["mine-1-review-7"]);
        assert_eq!(
            got[0].title,
            "bob commented, so no reminder comes: #1 change 1"
        );
        assert!(matches!(
            status(&p, &set, true, after),
            Status::Reviewed { ref by, .. } if by == "bob"
        ));
        let mut q = mine(2, 2);
        let r = by(&q, "alice", "COMMENTED", now(), 8);
        q.reviews.push(r);
        let default = plans(&[], true);
        assert!(
            events(std::slice::from_ref(&q), "alice", &default, after, now()).is_empty(),
            "a built-in default makes no deadline event, and a self review is no review"
        );
    }
}
