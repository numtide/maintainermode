//! The ledger is a pure function of the stored pull request facts and the
//! current time. It keeps no state of its own, so deleting the database and
//! syncing again yields the same ledger.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::model::{Pr, Review};

/// docs/rules.md explains each constant. Change it with the number.
pub const MOVE_DAYS: i64 = 14;
pub const HALF_LIFE_DAYS: f64 = 30.0;
pub const WINDOW_DAYS: i64 = 60;
pub const DAILY_CAP: usize = 5;
pub const MIN_COUNTERPARTIES: usize = 3;
pub const QUEUE_HEAD_DAYS: i64 = 14;
pub const PICKED_FACTOR: f64 = 0.5;
pub const PAIR_FACTOR: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    AuthorPushed,
    AuthorReplied,
    InlineOnChange,
    Merged,
}

#[derive(Debug, Clone, Serialize)]
pub struct Credit {
    pub reviewer: String,
    pub pr: u64,
    pub pr_title: String,
    pub pr_author: String,
    pub review_url: String,
    pub at: DateTime<Utc>,
    pub triggers: Vec<Trigger>,
    pub queue_head: bool,
    /// No maintainer and no owner: full weight at any age, so the hygiene
    /// review a newcomer can do pays as much as a deep one in a busy area.
    pub orphan: bool,
    /// 1, 1/2, 1/4… for the first, second, third credit on one author.
    pub pair_factor: f64,
    /// pair factor times the queue-head factor, before decay.
    pub weight: f64,
    /// weight after decay to now.
    pub value: f64,
    /// Why the credit counts for nothing, if it does not count.
    pub dropped: Option<Dropped>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Dropped {
    DailyCap,
    Voided { by: String, url: String },
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Person {
    pub login: String,
    pub score: f64,
    /// Distinct authors this person earned credit from inside the window.
    pub counterparties: usize,
    pub listed: bool,
    /// Share of in-window credit that came from authors who also earned
    /// credit on this person's pull requests. Shown, never acted on.
    pub reciprocity: f64,
    pub credits: usize,
}

#[derive(Debug, Default, Serialize)]
pub struct Ledger {
    pub computed_at: DateTime<Utc>,
    pub people: BTreeMap<String, Person>,
    pub credits: Vec<Credit>,
}

impl Ledger {
    pub fn is_listed(&self, login: &str) -> bool {
        self.people.get(login).is_some_and(|p| p.listed)
    }

    pub fn score(&self, login: &str) -> f64 {
        self.people.get(login).map_or(0.0, |p| p.score)
    }

    pub fn credits_of<'a>(&'a self, login: &'a str) -> impl Iterator<Item = &'a Credit> {
        self.credits.iter().filter(move |c| c.reviewer == login)
    }
}

fn eligible(pr: &Pr, r: &Review) -> bool {
    !r.bot() && r.author != pr.author && r.author != "ghost"
}

/// The facts that followed a review and show it moved the pull request.
pub fn triggers(pr: &Pr, r: &Review) -> Vec<Trigger> {
    let from = r.submitted_at;
    let until = from + Duration::days(MOVE_DAYS);
    let within = |t: &DateTime<Utc>| *t > from && *t <= until;
    let mut out = Vec::new();
    if pr.pushes.iter().any(within) {
        out.push(Trigger::AuthorPushed);
    }
    if pr.author_activity.iter().any(within) {
        out.push(Trigger::AuthorReplied);
    }
    if r.inline_on_change {
        out.push(Trigger::InlineOnChange);
    }
    if pr.merged_at.is_some_and(|m| m >= from && m <= until) {
        out.push(Trigger::Merged);
    }
    out
}

/// `orphan` says whether a pull request has no maintainer and no owner in
/// nixpkgs. It must read public data only, or the ledger stops being
/// recomputable by anyone.
pub fn compute(prs: &[Pr], orphan: impl Fn(&Pr) -> bool, now: DateTime<Utc>) -> Ledger {
    let mut earned: Vec<Credit> = Vec::new();
    for pr in prs.iter().filter(|p| !p.bot()) {
        let unowned = orphan(pr);
        let mut seen: HashSet<&str> = HashSet::new();
        let mut reviews: Vec<&Review> = pr.reviews.iter().filter(|r| eligible(pr, r)).collect();
        reviews.sort_by_key(|r| r.submitted_at);
        for r in reviews {
            if seen.contains(r.author.as_str()) {
                continue;
            }
            let t = triggers(pr, r);
            if t.is_empty() {
                continue;
            }
            // one credit per reviewer per pull request: further rounds on the
            // same change are the same piece of work
            seen.insert(&r.author);
            let dropped =
                pr.voids
                    .iter()
                    .find(|v| v.review_id == r.id())
                    .map(|v| Dropped::Voided {
                        by: v.by.clone(),
                        url: v.url.clone(),
                    });
            earned.push(Credit {
                reviewer: r.author.clone(),
                pr: pr.number,
                pr_title: pr.title.clone(),
                pr_author: pr.author.clone(),
                review_url: r.url.clone(),
                at: r.submitted_at,
                triggers: t,
                queue_head: r.submitted_at - pr.created_at >= Duration::days(QUEUE_HEAD_DAYS),
                orphan: unowned,
                pair_factor: 1.0,
                weight: 0.0,
                value: 0.0,
                dropped,
            });
        }
    }
    earned.sort_by(|a, b| a.at.cmp(&b.at).then(a.pr.cmp(&b.pr)));

    let window = Duration::days(WINDOW_DAYS);
    let mut per_day: HashMap<(String, chrono::NaiveDate), usize> = HashMap::new();
    let mut pair_history: HashMap<(String, String), Vec<DateTime<Utc>>> = HashMap::new();
    for c in earned.iter_mut() {
        if c.dropped.is_some() {
            continue;
        }
        let day = per_day
            .entry((c.reviewer.clone(), c.at.date_naive()))
            .or_default();
        if *day >= DAILY_CAP {
            c.dropped = Some(Dropped::DailyCap);
            continue;
        }
        *day += 1;
        let history = pair_history
            .entry((c.reviewer.clone(), c.pr_author.clone()))
            .or_default();
        let prior = history.iter().filter(|t| c.at - **t < window).count();
        history.push(c.at);
        c.pair_factor = PAIR_FACTOR.powi(prior as i32);
        let head = if c.queue_head || c.orphan {
            1.0
        } else {
            PICKED_FACTOR
        };
        c.weight = c.pair_factor * head;
        let age = (now - c.at).num_seconds().max(0) as f64 / 86400.0;
        c.value = c.weight * 0.5f64.powf(age / HALF_LIFE_DAYS);
    }

    let live = |c: &&Credit| c.dropped.is_none();
    let in_window = |c: &&Credit| now - c.at < window;
    let mut gave_to: HashMap<&str, HashSet<&str>> = HashMap::new();
    for c in earned.iter().filter(live).filter(in_window) {
        gave_to
            .entry(c.reviewer.as_str())
            .or_default()
            .insert(c.pr_author.as_str());
    }

    let mut people: BTreeMap<String, Person> = BTreeMap::new();
    for c in earned.iter().filter(live) {
        let p = people.entry(c.reviewer.clone()).or_insert_with(|| Person {
            login: c.reviewer.clone(),
            ..Default::default()
        });
        p.score += c.value;
        p.credits += 1;
    }
    let mut weights: HashMap<&str, (f64, f64)> = HashMap::new();
    for c in earned.iter().filter(live).filter(in_window) {
        let w = weights.entry(c.reviewer.as_str()).or_default();
        w.0 += c.weight;
        if gave_to
            .get(c.pr_author.as_str())
            .is_some_and(|s| s.contains(c.reviewer.as_str()))
        {
            w.1 += c.weight;
        }
    }
    for p in people.values_mut() {
        let authors = gave_to.get(p.login.as_str());
        p.counterparties = authors.map_or(0, |a| a.len());
        p.listed = p.counterparties >= MIN_COUNTERPARTIES;
        let (total, mutual) = weights.get(p.login.as_str()).copied().unwrap_or_default();
        p.reciprocity = if total > 0.0 { mutual / total } else { 0.0 };
    }

    Ledger {
        computed_at: now,
        people,
        credits: earned,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::Void;

    pub fn t(day: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::days(day)
    }

    pub fn pr(number: u64, author: &str, created: i64) -> Pr {
        Pr {
            number,
            title: format!("pr {number}"),
            url: format!("https://github.com/NixOS/nixpkgs/pull/{number}"),
            author: author.into(),
            author_is_bot: false,
            state: "OPEN".into(),
            is_draft: false,
            base: "master".into(),
            created_at: t(created),
            updated_at: t(created),
            merged_at: None,
            merged_by: None,
            merge_commit: None,
            mergeable: "MERGEABLE".into(),
            head_oid: "abc".into(),
            files: vec![],
            ci: Some("SUCCESS".into()),
            reviews: vec![],
            author_activity: vec![],
            pushes: vec![],
            voids: vec![],
            closed_at: None,
            additions: 0,
            deletions: 0,
            changed_files: 0,
            labels: vec![],
            association: String::new(),
            ai: None,
            treewide: None,
        }
    }

    pub fn review(id: u64, author: &str, day: i64) -> Review {
        Review {
            url: format!("https://github.com/NixOS/nixpkgs/pull/1#pullrequestreview-{id}"),
            author: author.into(),
            author_is_bot: false,
            state: "COMMENTED".into(),
            submitted_at: t(day),
            commit_oid: Some("abc".into()),
            inline_on_change: false,
        }
    }

    #[test]
    fn lgtm_alone_earns_nothing() {
        let mut p = pr(1, "alice", 0);
        p.reviews.push(review(1, "bob", 20));
        let l = compute(&[p], |_| false, t(21));
        assert!(l.credits.is_empty());
    }

    #[test]
    fn own_review_never_counts() {
        let mut p = pr(1, "alice", 0);
        let mut r = review(1, "alice", 20);
        r.inline_on_change = true;
        p.reviews.push(r);
        p.pushes.push(t(21));
        assert!(compute(&[p], |_| false, t(22)).credits.is_empty());
    }

    #[test]
    fn reviews_of_community_bots_never_count() {
        let prs: Vec<Pr> = ["r-ryantm", "dependabot[bot]", "alice"]
            .into_iter()
            .enumerate()
            .map(|(i, author)| {
                let mut p = pr(i as u64, author, 0);
                p.reviews.push(review(i as u64, "bob", 20));
                p.pushes.push(t(21));
                p
            })
            .collect();
        let l = compute(&prs, |_| false, t(22));
        assert_eq!(
            l.credits
                .iter()
                .map(|c| c.pr_author.as_str())
                .collect::<Vec<_>>(),
            ["alice"]
        );
    }

    #[test]
    fn push_after_review_earns_queue_head_credit() {
        let mut p = pr(1, "alice", 0);
        p.reviews.push(review(1, "bob", 20));
        p.pushes.push(t(21));
        let l = compute(&[p], |_| false, t(20));
        assert_eq!(l.credits.len(), 1);
        assert_eq!(l.credits[0].triggers, vec![Trigger::AuthorPushed]);
        assert_eq!(l.credits[0].weight, 1.0);
    }

    #[test]
    fn push_too_late_or_before_does_not_count() {
        let mut p = pr(1, "alice", 0);
        p.reviews.push(review(1, "bob", 20));
        p.pushes.push(t(19));
        p.pushes.push(t(20 + MOVE_DAYS + 1));
        assert!(compute(&[p], |_| false, t(40)).credits.is_empty());
    }

    #[test]
    fn fresh_pick_earns_half() {
        let mut p = pr(1, "alice", 0);
        let mut r = review(1, "bob", 1);
        r.inline_on_change = true;
        p.reviews.push(r);
        assert_eq!(
            compute(&[p], |_| false, t(1)).credits[0].weight,
            PICKED_FACTOR
        );
    }

    #[test]
    fn decay_halves_after_half_life() {
        let mut p = pr(1, "alice", 0);
        let mut r = review(1, "bob", 20);
        r.inline_on_change = true;
        p.reviews.push(r);
        let l = compute(&[p], |_| false, t(20 + HALF_LIFE_DAYS as i64));
        assert!((l.credits[0].value - 0.5).abs() < 1e-9);
    }

    #[test]
    fn one_credit_per_pull_request() {
        let mut p = pr(1, "alice", 0);
        for (i, d) in [20, 21, 22].into_iter().enumerate() {
            let mut r = review(i as u64, "bob", d);
            r.inline_on_change = true;
            p.reviews.push(r);
        }
        assert_eq!(compute(&[p], |_| false, t(23)).credits.len(), 1);
    }

    #[test]
    fn pair_returns_diminish_and_pair_is_not_listed() {
        let prs: Vec<Pr> = (1..=3)
            .map(|n| {
                let mut p = pr(n, "alice", 0);
                let mut r = review(n, "bob", 20 + n as i64);
                r.inline_on_change = true;
                p.reviews.push(r);
                p
            })
            .collect();
        let l = compute(&prs, |_| false, t(24));
        let w: Vec<f64> = l.credits.iter().map(|c| c.pair_factor).collect();
        assert_eq!(w, vec![1.0, 0.5, 0.25]);
        assert!(!l.is_listed("bob"));
    }

    #[test]
    fn three_authors_list_and_reciprocity_is_reported() {
        let mut prs = Vec::new();
        for (n, author) in [(1, "alice"), (2, "carol"), (3, "dave")] {
            let mut p = pr(n, author, 0);
            let mut r = review(n, "bob", 20);
            r.inline_on_change = true;
            p.reviews.push(r);
            prs.push(p);
        }
        let mut back = pr(4, "bob", 0);
        let mut r = review(4, "alice", 20);
        r.inline_on_change = true;
        back.reviews.push(r);
        prs.push(back);
        let l = compute(&prs, |_| false, t(21));
        assert!(l.is_listed("bob"));
        assert!(!l.is_listed("alice"));
        assert!((l.people["bob"].reciprocity - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn daily_cap_drops_the_excess() {
        let prs: Vec<Pr> = (0..(DAILY_CAP as u64 + 2))
            .map(|n| {
                let mut p = pr(n, &format!("a{n}"), 0);
                let mut r = review(n, "bob", 20);
                r.inline_on_change = true;
                p.reviews.push(r);
                p
            })
            .collect();
        let l = compute(&prs, |_| false, t(20));
        let capped = l
            .credits
            .iter()
            .filter(|c| matches!(c.dropped, Some(Dropped::DailyCap)))
            .count();
        assert_eq!(capped, 2);
        assert_eq!(l.people["bob"].credits, DAILY_CAP);
    }

    #[test]
    fn committer_void_is_shown_and_does_not_count() {
        let mut p = pr(1, "alice", 0);
        let mut r = review(77, "bob", 20);
        r.inline_on_change = true;
        p.reviews.push(r);
        p.voids.push(Void {
            review_id: "77".into(),
            by: "carol".into(),
            url: "https://github.com/NixOS/nixpkgs/pull/1#issuecomment-1".into(),
            at: t(21),
        });
        let l = compute(&[p], |_| false, t(21));
        assert!(matches!(l.credits[0].dropped, Some(Dropped::Voided { .. })));
        assert!(!l.people.contains_key("bob"));
    }

    #[test]
    fn fresh_review_on_unowned_pull_request_earns_full() {
        let mut p = pr(1, "alice", 0);
        let mut r = review(1, "bob", 1);
        r.inline_on_change = true;
        p.reviews.push(r);
        assert_eq!(compute(&[p], |_| true, t(1)).credits[0].weight, 1.0);
    }
}
