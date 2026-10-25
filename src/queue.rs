use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashSet;

use crate::ledger::Ledger;
use crate::model::Pr;

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub author_score: f64,
    pub base: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub ci: Option<String>,
    pub mergeable: String,
    pub reviewers: Vec<String>,
    /// Logins whose approval is on the current head commit.
    pub approved_by: Vec<String>,
    /// Logins whose review on this pull request earned credit.
    pub credited_by: Vec<String>,
    /// The paths that match it to the areas of people: none when it is
    /// [`too_large`](Entry::too_large), so that it reaches nobody.
    pub files: Vec<String>,
    /// No member's area covers it: the newcomer bucket.
    pub unowned: bool,
    pub treewide: Option<crate::links::Treewide>,
    pub too_large: Option<crate::links::TooLarge>,
}

#[derive(Debug, Default, Serialize)]
pub struct Queues {
    pub merge: Vec<Entry>,
    pub review: Vec<Entry>,
    /// First contributions that wait for their first review, oldest first.
    /// Any author, listed or not.
    pub firsttime: Vec<Entry>,
}

/// The label the nixpkgs bot puts on the pull request of a new contributor.
const FIRST_LABEL: &str = "12.first-time contribution";

pub fn is_first_contribution(pr: &Pr) -> bool {
    matches!(
        pr.association.as_str(),
        "FIRST_TIME_CONTRIBUTOR" | "FIRST_TIMER"
    ) || pr.labels.iter().any(|l| l == FIRST_LABEL)
}

/// Only reviews count as an answer: comments of other people are not stored.
pub fn first_timer_waits(pr: &Pr) -> bool {
    pr.is_open()
        && !pr.is_draft
        && !pr.author_is_bot
        && is_first_contribution(pr)
        && pr
            .reviews
            .iter()
            .all(|r| r.author == pr.author || r.author_is_bot)
}

pub(crate) fn entry(pr: &Pr, ledger: &Ledger, credited: &HashSet<(u64, &str)>) -> Entry {
    let mut reviewers: Vec<String> = Vec::new();
    let mut approved_by: Vec<String> = Vec::new();
    let mut credited_by: Vec<String> = Vec::new();
    for r in pr
        .reviews
        .iter()
        .filter(|r| r.author != pr.author && !r.author_is_bot)
    {
        if !reviewers.contains(&r.author) {
            reviewers.push(r.author.clone());
        }
        if r.state == "APPROVED"
            && r.commit_oid.as_deref() == Some(pr.head_oid.as_str())
            && !approved_by.contains(&r.author)
        {
            approved_by.push(r.author.clone());
        }
        if credited.contains(&(pr.number, r.author.as_str())) && !credited_by.contains(&r.author) {
            credited_by.push(r.author.clone());
        }
    }
    let too_large = crate::links::TooLarge::of(pr);
    Entry {
        number: pr.number,
        title: pr.title.clone(),
        url: pr.url.clone(),
        author: pr.author.clone(),
        author_score: ledger.score(&pr.author),
        base: pr.base.clone(),
        created_at: pr.created_at,
        updated_at: pr.updated_at,
        ci: pr.ci.clone(),
        mergeable: pr.mergeable.clone(),
        reviewers,
        approved_by,
        credited_by,
        files: if too_large.is_some() {
            Vec::new()
        } else {
            pr.files.clone()
        },
        unowned: false,
        treewide: pr.treewide,
        too_large,
    }
}

impl Entry {
    /// Green, no conflict, the current head approved by someone other than
    /// the author, and at least one review on it that earned credit.
    pub fn ready(&self) -> bool {
        self.ci.as_deref() == Some("SUCCESS")
            && self.mergeable == "MERGEABLE"
            && !self.approved_by.is_empty()
            && !self.credited_by.is_empty()
    }
}

pub(crate) fn credited(ledger: &Ledger) -> HashSet<(u64, &str)> {
    ledger
        .credits
        .iter()
        .filter(|c| c.dropped.is_none())
        .map(|c| (c.pr, c.reviewer.as_str()))
        .collect()
}

/// `unowned`: no `meta.maintainers`, `meta.teams` or `ci/OWNERS` line covers a
/// file the pull request changes, by public nixpkgs data.
pub fn build(prs: &[Pr], ledger: &Ledger, unowned: impl Fn(&Pr) -> bool) -> Queues {
    let credited = credited(ledger);
    let mut q = Queues::default();
    for pr in prs
        .iter()
        .filter(|p| p.is_open() && !p.is_draft && ledger.is_listed(&p.author))
    {
        let mut e = entry(pr, ledger, &credited);
        e.unowned = unowned(pr);
        if e.ready() {
            q.merge.push(e);
        } else {
            q.review.push(e);
        }
    }
    for pr in prs.iter().filter(|p| first_timer_waits(p)) {
        let mut e = entry(pr, ledger, &credited);
        e.unowned = unowned(pr);
        q.firsttime.push(e);
    }
    q.firsttime.sort_by_key(|e| (e.created_at, e.number));
    // the merge queue rewards those who help; the review queue drains the
    // backlog from its oldest end, which is also where credit is full
    q.merge.sort_by(|a, b| {
        b.author_score
            .total_cmp(&a.author_score)
            .then(a.created_at.cmp(&b.created_at))
    });
    q.review.sort_by_key(|e| (e.created_at, e.number));
    q
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::tests::{pr, review};

    fn first(number: u64, created: i64) -> Pr {
        let mut p = pr(number, &format!("new{number}"), created);
        p.association = "FIRST_TIME_CONTRIBUTOR".into();
        p
    }

    #[test]
    fn a_first_timer_waits_until_someone_else_reviews() {
        assert!(first_timer_waits(&first(1, 0)));
        let mut by_label = pr(2, "x", 0);
        assert!(!first_timer_waits(&by_label));
        by_label.labels.push(FIRST_LABEL.into());
        assert!(first_timer_waits(&by_label));
        let mut timer = pr(3, "x", 0);
        timer.association = "FIRST_TIMER".into();
        assert!(first_timer_waits(&timer));

        let mut own = first(4, 0);
        own.reviews.push(review(1, "new4", 1));
        let mut bot = review(2, "ci[bot]", 1);
        bot.author_is_bot = true;
        own.reviews.push(bot);
        assert!(first_timer_waits(&own));
        own.reviews.push(review(3, "helper", 2));
        assert!(!first_timer_waits(&own));

        let mut draft = first(5, 0);
        draft.is_draft = true;
        let mut closed = first(6, 0);
        closed.state = "CLOSED".into();
        let mut from_bot = first(7, 0);
        from_bot.author_is_bot = true;
        let mut known = first(8, 0);
        known.association = "CONTRIBUTOR".into();
        for p in [draft, closed, from_bot, known] {
            assert!(!first_timer_waits(&p), "#{}", p.number);
        }
    }

    #[test]
    fn the_oldest_first_timer_comes_first_listed_or_not() {
        let prs = vec![
            first(30, 5),
            first(20, -300),
            first(10, -300),
            pr(40, "x", -400),
        ];
        let q = build(&prs, &Ledger::default(), |p| p.number == 10);
        let order: Vec<u64> = q.firsttime.iter().map(|e| e.number).collect();
        assert_eq!(order, [10, 20, 30]);
        assert!(q.firsttime[0].unowned && !q.firsttime[1].unowned);
        assert!(q.review.is_empty());
    }
}
