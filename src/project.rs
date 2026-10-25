//! The archive JSON reduced to the rows of the main database: `Pr`, `Issue`
//! and `Facts`. Each function cuts the full item down to a fixed shape, with
//! caps (30 reviews, 5 inline comments per review, 30 comments, 30 pushes,
//! 50 files), and reads it with the parsers of `github`. The archive is the
//! one source of these rows: [`write`] runs on each archive write.
//!
//! `mergeable` and the CI state are not in the archive, and the archived
//! `authorAssociation` goes stale: [`pr_live`] and [`facts_live`] take them
//! from `pr_live`. A plain projected `Pr` has UNKNOWN and no CI.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

use crate::metrics::Facts;
use crate::model::{Issue, Pr};
use crate::store::{Live, Store};

const PR_LABELS: usize = 30;
const ISSUE_LABELS: usize = 20;
const FILES: usize = 50;
const REVIEWS: usize = 30;
const REVIEW_COMMENTS: usize = 5;
const COMMENTS: usize = 30;
const PUSHES: usize = 30;

fn list(v: &Value) -> &[Value] {
    v["nodes"].as_array().map_or(&[], Vec::as_slice)
}

fn first(v: &Value, n: usize) -> Value {
    let l = list(v);
    json!({ "nodes": &l[..n.min(l.len())] })
}

fn last(l: &[Value], n: usize) -> Value {
    json!({ "nodes": &l[l.len().saturating_sub(n)..] })
}

fn is_pr(item: &Value) -> bool {
    item["__typename"] == "PullRequest"
}

/// `commits(last: 1)`: the newest commit in the timeline, with no CI state.
fn last_commit(item: &Value) -> Value {
    let commit = list(&item["timelineItems"])
        .iter()
        .rev()
        .find(|t| t["__typename"] == "PullRequestCommit")
        .map(|t| json!({ "message": t["commit"]["message"] }));
    json!({ "nodes": commit.map(|c| vec![json!({ "commit": c })]).unwrap_or_default() })
}

fn copy(item: &Value, keys: &[&str]) -> Map<String, Value> {
    keys.iter()
        .map(|k| (k.to_string(), item[*k].clone()))
        .collect()
}

pub(crate) fn pr(item: &Value) -> Option<Pr> {
    if !is_pr(item) {
        return None;
    }
    let mut n = copy(
        item,
        &[
            "number",
            "title",
            "url",
            "isDraft",
            "state",
            "createdAt",
            "updatedAt",
            "mergedAt",
            "closedAt",
            "headRefOid",
            "baseRefName",
            "additions",
            "deletions",
            "changedFiles",
            "authorAssociation",
            "body",
            "mergedBy",
            "mergeCommit",
            "author",
        ],
    );
    n.insert("labels".into(), first(&item["labels"], PR_LABELS));
    n.insert("commits".into(), last_commit(item));
    n.insert("files".into(), first(&item["files"], FILES));
    let reviews: Vec<Value> = list(&item["reviews"])
        .iter()
        .take(REVIEWS)
        .map(|r| {
            let mut r = r.clone();
            r["comments"] = first(&r["comments"], REVIEW_COMMENTS);
            r
        })
        .collect();
    n.insert("reviews".into(), json!({ "nodes": reviews }));
    n.insert("comments".into(), last(list(&item["comments"]), COMMENTS));
    let pushes: Vec<Value> = list(&item["timelineItems"])
        .iter()
        .filter(|t| {
            matches!(
                t["__typename"].as_str(),
                Some("PullRequestCommit" | "HeadRefForcePushedEvent")
            )
        })
        .cloned()
        .collect();
    n.insert("timelineItems".into(), last(&pushes, PUSHES));
    crate::github::normalize(&Value::Object(n))
}

pub(crate) fn issue(item: &Value) -> Option<Issue> {
    if item["__typename"] != "Issue" {
        return None;
    }
    let mut n = copy(
        item,
        &[
            "number",
            "title",
            "url",
            "state",
            "createdAt",
            "updatedAt",
            "closedAt",
            "body",
            "author",
        ],
    );
    n.insert("labels".into(), first(&item["labels"], ISSUE_LABELS));
    let count = item["comments"]["totalCount"]
        .as_u64()
        .unwrap_or(list(&item["comments"]).len() as u64);
    n.insert("comments".into(), json!({ "totalCount": count }));
    crate::github::normalize_issue(&Value::Object(n))
}

pub(crate) fn facts(item: &Value) -> Option<Facts> {
    if !is_pr(item) {
        return None;
    }
    let mut n = copy(
        item,
        &[
            "number",
            "title",
            "state",
            "isDraft",
            "createdAt",
            "updatedAt",
            "closedAt",
            "mergedAt",
            "baseRefName",
            "authorAssociation",
            "additions",
            "deletions",
            "changedFiles",
            "author",
            "mergedBy",
        ],
    );
    n.insert("labels".into(), first(&item["labels"], PR_LABELS));
    n.insert("files".into(), first(&item["files"], FILES));
    n.insert("commits".into(), last_commit(item));
    n.insert("reviews".into(), first(&item["reviews"], REVIEWS));
    crate::github::facts(&Value::Object(n))
}

/// [`pr`] with the live fields of `pr_live` joined in.
pub(crate) fn pr_live(item: &Value, live: Option<&Live>) -> Option<Pr> {
    let mut p = pr(item)?;
    if let Some(l) = live {
        l.apply(&mut p);
    }
    Some(p)
}

/// [`facts`] with the author association of `pr_live`.
pub(crate) fn facts_live(item: &Value, live: Option<&Live>) -> Option<Facts> {
    let mut f = facts(item)?;
    if let Some(l) = live {
        f.association = l.association.clone();
    }
    Some(f)
}

/// Which projected rows go to the main database: open items, items updated
/// in the window of the ledger, and the facts of every pull request.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rows {
    pub(crate) window_from: DateTime<Utc>,
}

impl Rows {
    pub(crate) fn at(now: DateTime<Utc>) -> Self {
        Rows {
            window_from: now - chrono::Duration::days(crate::ledger::WINDOW_DAYS),
        }
    }
}

/// Projects archived items and writes the rows that `rows` keeps. A stored
/// row with a newer `updated_at` stays. Returns the rows written.
pub(crate) fn write(store: &Store, rows: Rows, items: &[(u64, Value)]) -> Result<usize> {
    let live = store.live()?;
    let (mut prs, mut issues, mut facts) = (Vec::new(), Vec::new(), Vec::new());
    for (n, item) in items {
        let l = live.get(n);
        if let Some(p) = pr_live(item, l) {
            if let Some(f) = facts_live(item, l) {
                facts.push(f);
            }
            if p.is_open() || p.updated_at >= rows.window_from {
                prs.push(p);
            }
        } else if let Some(i) = issue(item) {
            if i.state == "OPEN" || i.updated_at >= rows.window_from {
                issues.push(i);
            }
        }
    }
    store.put_prs(&prs)?;
    store.put_issues(&issues)?;
    store.put_facts(&facts)?;
    Ok(prs.len() + issues.len() + facts.len())
}

/// The fields a projection cannot know when `pr_live` has no row for the
/// same head.
#[cfg(test)]
pub(crate) const NOT_ARCHIVED: [&str; 2] = ["mergeable", "ci"];

/// The top-level fields where the projection and the stored row differ,
/// apart from `skip`.
#[cfg(test)]
pub(crate) fn differ<T: serde::Serialize>(projected: &T, stored: &T, skip: &[&str]) -> Vec<String> {
    let (Ok(Value::Object(a)), Ok(Value::Object(b))) = (
        serde_json::to_value(projected),
        serde_json::to_value(stored),
    ) else {
        return vec!["?".into()];
    };
    a.iter()
        .filter(|(k, _)| !skip.contains(&k.as_str()))
        .filter(|(k, v)| b.get(k.as_str()) != Some(v))
        .map(|(k, _)| k.clone())
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Real items from NixOS/nixpkgs, read once from the archive and once
    /// with the sync queries, at the same `updatedAt`. Text the parsers do
    /// not read is cut out to keep the file small.
    pub(crate) fn fixtures() -> Value {
        let z = include_bytes!("testdata/project.json.zst");
        serde_json::from_slice(&zstd::decode_all(z.as_slice()).unwrap()).unwrap()
    }

    /// With `pr_live` joined in, the projection equals the sync in every
    /// field. Without it, `authorAssociation` can be stale: the author of
    /// #444470 joined the organization after the archive read it. The facts
    /// equal those cut from the sync, so both sources of `history` keep the
    /// same reviews.
    #[test]
    fn the_projection_matches_the_sync() {
        let f = fixtures();
        let archive = f["archive"].as_object().unwrap();
        let github = &f["github"];
        let none = Vec::<String>::new();
        let (mut prs, mut issues) = (0, 0);
        for (number, item) in archive {
            if is_pr(item) {
                let want = crate::github::normalize(&github[format!("p{number}")]).unwrap();
                let live = Live::of(&want, chrono::Utc::now());
                let mut d = differ(&pr(item).unwrap(), &want, &NOT_ARCHIVED);
                d.retain(|k| k != "association" && k != "merge_commit");
                assert_eq!(d, none, "#{number}");
                let got = pr_live(item, Some(&live)).unwrap();
                // the sync queries never asked for the merge commit
                assert_eq!(differ(&got, &want, &["merge_commit"]), none, "#{number}");
                assert_eq!(
                    got.merge_commit.as_deref(),
                    item["mergeCommit"]["oid"].as_str(),
                    "#{number}"
                );
                assert!(got.merge_commit.is_some(), "#{number}");
                let want = Facts::from(&want);
                let got = facts_live(item, Some(&live)).unwrap();
                assert_eq!(differ(&got, &want, &[]), none, "#{number}");
                prs += 1;
            } else {
                let want = crate::github::normalize_issue(&github[format!("i{number}")]).unwrap();
                let got = issue(item).unwrap();
                assert_eq!(differ(&got, &want, &[]), none, "#{number}");
                issues += 1;
            }
        }
        assert_eq!((prs, issues), (5, 2));
    }

    #[test]
    fn the_caps_hold() {
        let f = fixtures();
        let item = &f["archive"]["444470"];
        assert!(list(&item["reviews"]).len() > REVIEWS);
        let p = pr(item).unwrap();
        assert!(p.reviews.len() + p.author_activity.len() <= REVIEWS + COMMENTS);
        assert!(p.pushes.len() <= PUSHES);
        assert_eq!(p.mergeable, "UNKNOWN");
        assert_eq!(p.ci, None);
    }

    #[test]
    fn a_kind_projects_only_to_its_own_row() {
        let f = fixtures();
        assert!(issue(&f["archive"]["444470"]).is_none());
        assert!(pr(&f["archive"]["453372"]).is_none());
        assert!(facts(&f["archive"]["453372"]).is_none());
    }

    #[test]
    fn differ_ignores_what_the_archive_lacks() {
        let f = fixtures();
        let a = pr(&f["archive"]["300123"]).unwrap();
        let mut b = a.clone();
        b.mergeable = "MERGEABLE".into();
        b.ci = Some("SUCCESS".into());
        assert!(differ(&a, &b, &NOT_ARCHIVED).is_empty());
        b.title.push('!');
        assert_eq!(differ(&a, &b, &NOT_ARCHIVED), vec!["title".to_string()]);
    }

    #[test]
    fn a_live_row_of_another_head_sets_only_the_association() {
        let f = fixtures();
        let item = &f["archive"]["300123"];
        let mut live = Live::of(&pr(item).unwrap(), chrono::Utc::now());
        live.mergeable = "MERGEABLE".into();
        live.ci = Some("SUCCESS".into());
        live.association = "MEMBER".into();
        let p = pr_live(item, Some(&live)).unwrap();
        assert_eq!(
            (p.mergeable.as_str(), p.ci.as_deref()),
            ("MERGEABLE", Some("SUCCESS"))
        );
        live.head_oid = "other".into();
        let p = pr_live(item, Some(&live)).unwrap();
        assert_eq!((p.mergeable.as_str(), p.ci.as_deref()), ("UNKNOWN", None));
        assert_eq!(p.association, "MEMBER");
    }
}
