//! Starts and lists GitHub Actions runs in a repository the person owns, with
//! their own token. Maintainer Mode writes nothing to nixpkgs: the workflow
//! acts as the person, and only when they press the button.

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};

pub const REVIEW: &str = "review.yml";
pub const UPDATE: &str = "update.yml";

/// `owner/name`, as GitHub spells a repository.
pub fn valid_repo(r: &str) -> bool {
    let mut parts = r.split('/');
    let ok = |p: Option<&str>| {
        p.is_some_and(|p| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && p.len() <= 100
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
    };
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

fn api(token: &str, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
    crate::github::http()
        .request(method, format!("https://api.github.com{path}"))
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
}

/// What to tell the person when GitHub says no: the usual cause is an app
/// without access to their repository.
fn refused(status: reqwest::StatusCode, body: &str, repo: &str) -> anyhow::Error {
    let hint = match status.as_u16() {
        401 => "your GitHub session expired; sign out and in again",
        403 | 404 => {
            "the Maintainer Mode GitHub App cannot reach this repository: install it on the repository, \
             and check that the workflow file exists on its default branch"
        }
        422 => "GitHub refused the inputs; the workflow may be an older version",
        _ => "GitHub refused the request",
    };
    anyhow::anyhow!(
        "{repo}: {hint} ({status}: {})",
        body.chars().take(200).collect::<String>()
    )
}

async fn default_branch(token: &str, repo: &str) -> Result<String> {
    let resp = api(token, reqwest::Method::GET, &format!("/repos/{repo}"))
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        return Err(refused(status, &body, repo));
    }
    let v: Value = serde_json::from_str(&body)?;
    Ok(v["default_branch"].as_str().unwrap_or("main").to_string())
}

pub async fn dispatch(token: &str, repo: &str, workflow: &str, inputs: Value) -> Result<()> {
    if !valid_repo(repo) {
        bail!("{repo:?} is not owner/name");
    }
    let branch = default_branch(token, repo).await?;
    let resp = api(
        token,
        reqwest::Method::POST,
        &format!("/repos/{repo}/actions/workflows/{workflow}/dispatches"),
    )
    .json(&json!({ "ref": branch, "inputs": inputs }))
    .send()
    .await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(refused(status, &resp.text().await?, repo));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct Run {
    pub id: u64,
    pub name: String,
    pub url: String,
    /// queued, in_progress or completed.
    pub status: String,
    /// success, failure, cancelled and so on, once completed.
    pub conclusion: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
}

pub async fn runs(token: &str, repo: &str, workflow: &str) -> Result<Vec<Run>> {
    if !valid_repo(repo) {
        bail!("{repo:?} is not owner/name");
    }
    let resp = api(
        token,
        reqwest::Method::GET,
        &format!("/repos/{repo}/actions/workflows/{workflow}/runs?per_page=15"),
    )
    .send()
    .await?;
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        return Err(refused(status, &body, repo));
    }
    let v: Value = serde_json::from_str(&body)?;
    Ok(v["workflow_runs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| Run {
            id: r["id"].as_u64().unwrap_or(0),
            name: r["display_title"]
                .as_str()
                .or(r["name"].as_str())
                .unwrap_or("")
                .to_string(),
            url: r["html_url"].as_str().unwrap_or("").to_string(),
            status: r["status"].as_str().unwrap_or("").to_string(),
            conclusion: r["conclusion"].as_str().map(String::from),
            created_at: r["created_at"]
                .as_str()
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.with_timezone(&Utc)),
        })
        .collect())
}

/// The step of `.github/workflows/update.yml` that pushes the branch.
const PUSH_STEP: &str = "push the branch";

fn pushed_in(jobs: &Value) -> bool {
    jobs["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|j| j["steps"].as_array().into_iter().flatten())
        .any(|s| s["name"] == PUSH_STEP && s["conclusion"] == "success")
}

/// Whether an update run pushed its branch: the push job is skipped when
/// nix-update changed nothing, and the run conclusion alone does not say so.
pub async fn pushed(token: &str, repo: &str, run: u64) -> Result<bool> {
    if !valid_repo(repo) {
        bail!("{repo:?} is not owner/name");
    }
    let resp = api(
        token,
        reqwest::Method::GET,
        &format!("/repos/{repo}/actions/runs/{run}/jobs?per_page=30"),
    )
    .send()
    .await?;
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        return Err(refused(status, &body, repo));
    }
    Ok(pushed_in(&serde_json::from_str(&body)?))
}

/// The branch the update workflow pushes to when nix-update changed something.
pub fn update_branch(attr: &str) -> String {
    format!("update/{attr}")
}

/// GitHub's page to open a pull request from that branch of the fork.
pub fn compare_url(fork: &str, attr: &str) -> String {
    let owner = fork.split('/').next().unwrap_or(fork);
    let name = fork.split('/').nth(1).unwrap_or("nixpkgs");
    format!(
        "https://github.com/NixOS/nixpkgs/compare/master...{owner}:{name}:{}?expand=1",
        update_branch(attr)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_is_owner_slash_name() {
        assert!(valid_repo("zimbatm/nixpkgs-review-gha"));
        assert!(!valid_repo("zimbatm"));
        assert!(!valid_repo("a/b/c"));
        assert!(!valid_repo("a/../b"));
        assert!(!valid_repo("a/.."));
        assert!(!valid_repo("a/b?x=1"));
    }

    #[test]
    fn only_a_push_step_that_succeeded_counts_as_pushed() {
        let jobs = |push: Value| {
            json!({ "jobs": [
                { "name": "update / update", "steps": [{ "name": "nix-update", "conclusion": "success" }] },
                push,
            ]})
        };
        let pushed = jobs(json!({ "name": "update / push", "conclusion": "success",
            "steps": [{ "name": "push the branch", "conclusion": "success" }] }));
        let skipped =
            jobs(json!({ "name": "update / push", "conclusion": "skipped", "steps": [] }));
        let failed = jobs(json!({ "name": "update / push", "conclusion": "failure",
            "steps": [{ "name": "push the branch", "conclusion": "failure" }] }));
        assert!(pushed_in(&pushed));
        assert!(!pushed_in(&skipped));
        assert!(!pushed_in(&failed));
        assert!(!pushed_in(&json!({})));
    }

    #[test]
    fn the_compare_link_opens_a_pull_request_from_the_fork() {
        assert_eq!(
            compare_url("alice/nixpkgs", "hello"),
            "https://github.com/NixOS/nixpkgs/compare/master...alice:nixpkgs:update/hello?expand=1"
        );
    }
}
