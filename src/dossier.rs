//! A person's review record, as evidence for the people who grant commit
//! access. It gates nothing. Like the ledger, it is a pure function of the
//! stored pull request facts.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::collections::HashMap;

use crate::ledger::MOVE_DAYS;
use crate::model::{Pr, Review};

pub const RECENT: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// A committer other than the reviewer merged it with no push after the
    /// review: the reviewer's judgement matched the merge button.
    MergedUnchanged,
    MergedAfterChanges,
    /// The reviewer pressed the button themselves; it says nothing about a
    /// match with someone else's judgement.
    MergedByReviewer,
    ChangedAfterReview,
    Open,
    Closed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub pr: u64,
    pub pr_title: String,
    pub pr_author: String,
    pub review_url: String,
    pub at: DateTime<Utc>,
    pub state: String,
    pub outcome: Outcome,
    /// The author changed the pull request within the move window after a
    /// review that requested changes or commented on a changed line.
    pub problem_found: bool,
    /// The areas of the state of nixpkgs that the pull request touches.
    pub areas: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Dossier {
    pub login: String,
    pub reviewed: usize,
    pub merged_unchanged: usize,
    pub problems_found: usize,
    pub recent: Vec<Row>,
    /// Who else reviewed the same pull requests, most overlap first.
    pub co_reviewers: Vec<(String, usize)>,
    /// Whose pull requests this person reviewed, most first.
    pub authors: Vec<(String, usize)>,
}

fn row(pr: &Pr, mine: &[&Review]) -> Row {
    let first = mine.iter().map(|r| r.submitted_at).min().unwrap();
    let last = mine.iter().max_by_key(|r| r.submitted_at).unwrap();
    let pushed_after = |t: DateTime<Utc>| pr.pushes.iter().any(|p| *p > t);
    let outcome = match (pr.state.as_str(), pr.merged_at) {
        ("MERGED", Some(_)) if pr.merged_by.as_deref() == Some(last.author.as_str()) => {
            Outcome::MergedByReviewer
        }
        ("MERGED", Some(_)) if !pushed_after(last.submitted_at) => Outcome::MergedUnchanged,
        ("MERGED", _) => Outcome::MergedAfterChanges,
        ("OPEN", _) if pushed_after(first) => Outcome::ChangedAfterReview,
        ("OPEN", _) => Outcome::Open,
        _ => Outcome::Closed,
    };
    let problem_found = mine.iter().any(|r| {
        (r.state == "CHANGES_REQUESTED" || r.inline_on_change)
            && pr
                .pushes
                .iter()
                .any(|p| *p > r.submitted_at && *p <= r.submitted_at + Duration::days(MOVE_DAYS))
    });
    Row {
        pr: pr.number,
        pr_title: pr.title.clone(),
        pr_author: pr.author.clone(),
        review_url: last.url.clone(),
        at: last.submitted_at,
        state: last.state.clone(),
        outcome,
        problem_found,
        areas: crate::metrics::areas(&pr.files),
    }
}

fn ranked(counts: HashMap<String, usize>) -> Vec<(String, usize)> {
    let mut v: Vec<_> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.truncate(10);
    v
}

fn reviewed<'a>(prs: &'a [Pr], login: &'a str) -> impl Iterator<Item = (&'a Pr, Vec<&'a Review>)> {
    prs.iter()
        .filter(move |p| p.author != login)
        .filter_map(move |pr| {
            let mine: Vec<&Review> = pr.reviews.iter().filter(|r| r.author == login).collect();
            (!mine.is_empty()).then_some((pr, mine))
        })
}

/// Every row, newest first, where `build` keeps the last `RECENT`.
pub fn rows(prs: &[Pr], login: &str) -> Vec<Row> {
    let mut rows: Vec<Row> = reviewed(prs, login)
        .map(|(pr, mine)| row(pr, &mine))
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.at));
    rows
}

pub fn build(prs: &[Pr], login: &str) -> Dossier {
    let mut rows = Vec::new();
    let mut co: HashMap<String, usize> = HashMap::new();
    let mut authors: HashMap<String, usize> = HashMap::new();
    for (pr, mine) in reviewed(prs, login) {
        rows.push(row(pr, &mine));
        *authors.entry(pr.author.clone()).or_default() += 1;
        let mut others: Vec<&str> = pr
            .reviews
            .iter()
            .filter(|r| r.author != login && !r.author_is_bot)
            .map(|r| r.author.as_str())
            .collect();
        others.sort();
        others.dedup();
        for o in others {
            *co.entry(o.to_string()).or_default() += 1;
        }
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.at));
    Dossier {
        login: login.to_string(),
        reviewed: rows.len(),
        merged_unchanged: rows
            .iter()
            .filter(|r| r.outcome == Outcome::MergedUnchanged)
            .count(),
        problems_found: rows.iter().filter(|r| r.problem_found).count(),
        recent: rows.into_iter().take(RECENT).collect(),
        co_reviewers: ranked(co),
        authors: ranked(authors),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::tests::{pr, review, t};

    #[test]
    fn merged_by_someone_else_without_a_push_is_unchanged() {
        let mut p = pr(1, "alice", 0);
        p.reviews.push(review(1, "bob", 2));
        p.state = "MERGED".into();
        p.merged_at = Some(t(3));
        p.merged_by = Some("carol".into());
        let d = build(&[p], "bob");
        assert_eq!(d.merged_unchanged, 1);
    }

    #[test]
    fn a_push_after_the_review_is_a_problem_found_not_unchanged() {
        let mut p = pr(1, "alice", 0);
        let mut r = review(1, "bob", 2);
        r.state = "CHANGES_REQUESTED".into();
        p.reviews.push(r);
        p.pushes.push(t(3));
        p.state = "MERGED".into();
        p.merged_at = Some(t(4));
        p.merged_by = Some("carol".into());
        let d = build(&[p], "bob");
        assert_eq!(d.merged_unchanged, 0);
        assert_eq!(d.problems_found, 1);
        assert_eq!(d.recent[0].outcome, Outcome::MergedAfterChanges);
    }

    #[test]
    fn merging_your_own_review_does_not_count() {
        let mut p = pr(1, "alice", 0);
        p.reviews.push(review(1, "bob", 2));
        p.state = "MERGED".into();
        p.merged_at = Some(t(3));
        p.merged_by = Some("bob".into());
        assert_eq!(build(&[p], "bob").merged_unchanged, 0);
    }
}
