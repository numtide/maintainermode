//! The archive JSON reduced to the rows of the main database: `Pr`, `Issue`
//! and `Facts`. Each function cuts the full item down to the shape that the
//! sync queries return, with their caps, and reads it with the same parser,
//! so the ledger and the queues see the same data from either source.
//!
//! `mergeable` and the CI state are not in the archive: a projected `Pr` has
//! UNKNOWN and no CI.

use serde_json::{Map, Value, json};

use crate::metrics::Facts;
use crate::model::{Issue, Pr};

const PR_LABELS: usize = 30;
const ISSUE_LABELS: usize = 20;
const FILES: usize = 50;
const REVIEWS: usize = 30;
const REVIEW_COMMENTS: usize = 5;
const COMMENTS: usize = 30;
const PUSHES: usize = 30;
const HISTORY_REVIEWS: usize = 10;

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
    n.insert("reviews".into(), first(&item["reviews"], HISTORY_REVIEWS));
    crate::github::facts(&Value::Object(n))
}

/// The top-level fields where the projection and the stored row differ,
/// apart from the fields the archive does not hold.
pub(crate) fn differ<T: serde::Serialize>(projected: &T, stored: &T) -> Vec<String> {
    let (Ok(Value::Object(a)), Ok(Value::Object(b))) = (
        serde_json::to_value(projected),
        serde_json::to_value(stored),
    ) else {
        return vec!["?".into()];
    };
    a.iter()
        .filter(|(k, _)| !matches!(k.as_str(), "mergeable" | "ci"))
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

    /// `authorAssociation` changes when the author joins the organization,
    /// with no change to `updatedAt`: the author of #444470 joined after the
    /// archive read it.
    fn same<T: serde::Serialize>(got: &T, want: &T) -> Vec<String> {
        let mut d = differ(got, want);
        d.retain(|k| k != "association");
        d
    }

    #[test]
    fn the_projection_matches_the_sync() {
        let f = fixtures();
        let archive = f["archive"].as_object().unwrap();
        let github = &f["github"];
        let (mut prs, mut issues) = (0, 0);
        for (number, item) in archive {
            if is_pr(item) {
                let want = crate::github::normalize(&github[format!("p{number}")]).unwrap();
                let got = pr(item).unwrap();
                assert_eq!(same(&got, &want), Vec::<String>::new(), "#{number}");
                let want = crate::github::facts(&github[format!("h{number}")]).unwrap();
                let got = facts(item).unwrap();
                assert_eq!(same(&got, &want), Vec::<String>::new(), "#{number}");
                prs += 1;
            } else {
                let want = crate::github::normalize_issue(&github[format!("i{number}")]).unwrap();
                let got = issue(item).unwrap();
                assert_eq!(same(&got, &want), Vec::<String>::new(), "#{number}");
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
        assert!(differ(&a, &b).is_empty());
        b.title.push('!');
        assert_eq!(differ(&a, &b), vec!["title".to_string()]);
    }
}
