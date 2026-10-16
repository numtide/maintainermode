use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashSet;

use crate::area::Area;
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
    pub files: Vec<String>,
    /// No member's area covers it: the newcomer bucket.
    pub unowned: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct Queues {
    pub merge: Vec<Entry>,
    pub review: Vec<Entry>,
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
        files: pr.files.clone(),
        unowned: false,
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

pub fn build(prs: &[Pr], ledger: &Ledger, members: &[Area]) -> Queues {
    let credited = credited(ledger);
    let mut q = Queues::default();
    for pr in prs
        .iter()
        .filter(|p| p.is_open() && !p.is_draft && ledger.is_listed(&p.author))
    {
        let mut e = entry(pr, ledger, &credited);
        e.unowned = !members.iter().any(|a| a.covers(&pr.files));
        if e.ready() {
            q.merge.push(e);
        } else {
            q.review.push(e);
        }
    }
    // the merge queue rewards those who help; the review queue drains the
    // backlog from its oldest end, which is also where credit is full
    q.merge.sort_by(|a, b| {
        b.author_score
            .total_cmp(&a.author_score)
            .then(a.created_at.cmp(&b.created_at))
    });
    q.review.sort_by_key(|e| e.created_at);
    q
}
