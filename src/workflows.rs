//! Starts and lists GitHub Actions runs in a repository the person owns, with
//! their own token. Maintainer Mode writes nothing to nixpkgs: the workflow
//! acts as the person, and only when they press the button.

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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

/// One GitHub answer of the setup check: the HTTP status, or `None` when no
/// answer came or the check did not ask.
pub type Reply = Option<u16>;

fn ok(r: Reply) -> bool {
    r.is_some_and(|s| (200..300).contains(&s))
}

/// What GitHub said about one workflow repository.
#[derive(Debug, Clone, Default)]
pub struct RepoAnswers {
    pub repo: Reply,
    pub branch: Option<String>,
    /// `.github/workflows/<file>` on the default branch.
    pub file: Reply,
    /// 200 when an installation of the App, as the person sees it, includes
    /// the repository, and 404 when none does. The Actions API of a public
    /// repository answers any token, so it cannot tell.
    pub app: Reply,
}

#[derive(Debug, Clone, Default)]
pub struct Answers {
    pub review: RepoAnswers,
    pub update: RepoAnswers,
    pub fork: Reply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mark {
    Ok,
    Missing,
    Unknown,
}

/// Which button a line of the check blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Needed {
    Review,
    Update,
    Both,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub step: u8,
    pub mark: Mark,
    pub needed: Needed,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub at: DateTime<Utc>,
    pub lines: Vec<Line>,
}

impl Check {
    pub fn step(&self, n: u8) -> Mark {
        let marks: Vec<Mark> = self
            .lines
            .iter()
            .filter(|l| l.step == n)
            .map(|l| l.mark)
            .collect();
        if marks.contains(&Mark::Missing) {
            Mark::Missing
        } else if !marks.is_empty() && marks.iter().all(|m| *m == Mark::Ok) {
            Mark::Ok
        } else {
            Mark::Unknown
        }
    }

    /// The first line that blocks the button. A line that GitHub could not
    /// answer blocks nothing.
    pub fn blocker(&self, button: Needed) -> Option<&Line> {
        self.lines
            .iter()
            .find(|l| l.mark == Mark::Missing && (l.needed == button || l.needed == Needed::Both))
    }
}

/// The repository names the check reads, and whether the person saved them.
pub struct Names<'a> {
    pub review: &'a str,
    pub update: &'a str,
    pub fork: &'a str,
    pub saved: bool,
}

const EXPIRED: &str = "GitHub refused your token. Sign out and sign in again.";

fn line(step: u8, mark: Mark, needed: Needed, text: String) -> Line {
    Line {
        step,
        mark,
        needed,
        text,
    }
}

fn other(step: u8, needed: Needed, repo: &str, r: Reply) -> Line {
    let text = match r {
        Some(401) => return line(step, Mark::Missing, needed, EXPIRED.into()),
        Some(s) => format!("GitHub answered {s} for {repo}. Check again later."),
        None => format!("GitHub did not answer for {repo}. Check again later."),
    };
    line(step, Mark::Unknown, needed, text)
}

fn not_checked(step: u8, needed: Needed, repo: &str) -> Line {
    line(
        step,
        Mark::Unknown,
        needed,
        format!("Not checked: Maintainer Mode cannot read {repo}."),
    )
}

fn repo_line(repo: &str, a: &RepoAnswers, needed: Needed) -> Line {
    match a.repo {
        r if ok(r) => line(
            1,
            Mark::Ok,
            needed,
            format!("{repo} exists, and your token can read it."),
        ),
        Some(403 | 404) => line(
            1,
            Mark::Missing,
            needed,
            format!(
                "{repo} does not exist, or your token cannot read it. Fork nixpkgs-review-gha (step 1), or correct the name (step 5)."
            ),
        ),
        r => other(1, needed, repo, r),
    }
}

fn file_line(repo: &str, file: &str, a: &RepoAnswers, needed: Needed) -> Line {
    if !ok(a.repo) {
        return not_checked(2, needed, repo);
    }
    let branch = a.branch.as_deref().unwrap_or("main");
    match a.file {
        r if ok(r) => line(
            2,
            Mark::Ok,
            needed,
            format!("{repo} has .github/workflows/{file} on {branch}."),
        ),
        Some(404) => {
            let fix = if file == REVIEW {
                "Sync your fork with Defelo/nixpkgs-review-gha."
            } else {
                "Copy gha/update.yml there (step 2)."
            };
            line(
                2,
                Mark::Missing,
                needed,
                format!(
                    "{repo} has no .github/workflows/{file} on its default branch {branch}. {fix}"
                ),
            )
        }
        r => other(2, needed, repo, r),
    }
}

fn app_line(repo: &str, a: &RepoAnswers, needed: Needed) -> Line {
    if !ok(a.repo) {
        return not_checked(4, needed, repo);
    }
    match a.app {
        r if ok(r) => line(
            4,
            Mark::Ok,
            needed,
            format!("The App is installed on {repo}."),
        ),
        Some(404) => line(
            4,
            Mark::Missing,
            needed,
            format!("The App is not installed on {repo}. Install the App on {repo} (step 4)."),
        ),
        r => other(4, needed, repo, r),
    }
}

/// Each line says what is OK, or what is missing and how to fix it.
pub fn verdict(n: &Names, a: &Answers) -> Vec<Line> {
    let repos = if n.review == n.update {
        vec![(n.review, &a.review, Needed::Both)]
    } else {
        vec![
            (n.review, &a.review, Needed::Review),
            (n.update, &a.update, Needed::Update),
        ]
    };
    let mut lines: Vec<Line> = repos
        .iter()
        .map(|(repo, ra, needed)| repo_line(repo, ra, *needed))
        .collect();
    lines.push(file_line(n.review, REVIEW, &a.review, Needed::Review));
    lines.push(file_line(n.update, UPDATE, &a.update, Needed::Update));
    lines.push(match a.fork {
        r if ok(r) => line(3, Mark::Ok, Needed::Update, format!("{} exists.", n.fork)),
        Some(403 | 404) => line(
            3,
            Mark::Missing,
            Needed::Update,
            format!(
                "{} does not exist, or your token cannot read it. Fork NixOS/nixpkgs, or correct the name (step 5).",
                n.fork
            ),
        ),
        r => other(3, Needed::Update, n.fork, r),
    });
    lines.push(line(
        3,
        Mark::Unknown,
        Needed::Update,
        format!(
            "Maintainer Mode cannot read secrets. Make sure that {} has a GH_TOKEN secret that can push to {}.",
            n.update, n.fork
        ),
    ));
    lines.extend(
        repos
            .iter()
            .map(|(repo, ra, needed)| app_line(repo, ra, *needed)),
    );
    lines.push(if n.saved {
        line(
            5,
            Mark::Ok,
            Needed::Both,
            "You saved the repository names.".into(),
        )
    } else {
        line(
            5,
            Mark::Unknown,
            Needed::Both,
            "You did not save the repository names, so the defaults below apply.".into(),
        )
    });
    lines
}

async fn status(token: &str, path: &str) -> (Reply, Value) {
    let Ok(resp) = api(token, reqwest::Method::GET, path).send().await else {
        return (None, Value::Null);
    };
    let s = resp.status();
    let v = if s.is_success() {
        resp.json().await.unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    (Some(s.as_u16()), v)
}

async fn probe_file(token: &str, repo: &str, file: &str) -> Reply {
    status(
        token,
        &format!("/repos/{repo}/contents/.github/workflows/{file}"),
    )
    .await
    .0
}

async fn probe_repo(token: &str, repo: &str, file: &str) -> RepoAnswers {
    if !valid_repo(repo) {
        return RepoAnswers {
            repo: Some(404),
            ..RepoAnswers::default()
        };
    }
    let (r, v) = status(token, &format!("/repos/{repo}")).await;
    if !ok(r) {
        return RepoAnswers {
            repo: r,
            ..RepoAnswers::default()
        };
    }
    RepoAnswers {
        repo: r,
        branch: v["default_branch"].as_str().map(String::from),
        file: probe_file(token, repo, file).await,
        app: None,
    }
}

async fn probe_fork(token: &str, fork: &str) -> Reply {
    if !valid_repo(fork) {
        return Some(404);
    }
    status(token, &format!("/repos/{fork}")).await.0
}

const PER_PAGE: usize = 100;
const MAX_PAGES: usize = 10;

/// Every item of `key` on the pages of a GitHub listing. `Err` holds the
/// answer that stopped it.
async fn listing(token: &str, path: &str, key: &str) -> Result<Vec<Value>, Reply> {
    let mut out = Vec::new();
    for page in 1..=MAX_PAGES {
        let (r, v) = status(token, &format!("{path}?per_page={PER_PAGE}&page={page}")).await;
        if !ok(r) {
            return Err(r);
        }
        let items = v[key].as_array().cloned().unwrap_or_default();
        let last = items.len() < PER_PAGE;
        out.extend(items);
        if last {
            break;
        }
    }
    Ok(out)
}

/// The repositories that the installations of the App include, as the
/// person sees them. A user token of the App lists only that App's
/// installations.
async fn app_repos(token: &str) -> Result<Vec<String>, Reply> {
    let installations = listing(token, "/user/installations", "installations").await?;
    let mut repos = Vec::new();
    for id in installations.iter().filter_map(|i| i["id"].as_u64()) {
        let path = format!("/user/installations/{id}/repositories");
        repos.extend(full_names(&listing(token, &path, "repositories").await?));
    }
    Ok(repos)
}

fn full_names(repos: &[Value]) -> Vec<String> {
    repos
        .iter()
        .filter_map(|r| r["full_name"].as_str().map(String::from))
        .collect()
}

fn installed(app_repos: &Result<Vec<String>, Reply>, repo: &str) -> Reply {
    match app_repos {
        Ok(list) if list.iter().any(|r| r.eq_ignore_ascii_case(repo)) => Some(200),
        Ok(_) => Some(404),
        Err(r) => *r,
    }
}

/// Reads, with the person's token, what [`verdict`] needs. It only reads.
pub async fn probe(token: &str, review: &str, update: &str, fork: &str) -> Answers {
    let (mut review_a, fork_a, app) = tokio::join!(
        probe_repo(token, review, REVIEW),
        probe_fork(token, fork),
        app_repos(token)
    );
    let mut update_a = if review == update {
        let file = if ok(review_a.repo) {
            probe_file(token, update, UPDATE).await
        } else {
            None
        };
        RepoAnswers {
            file,
            ..review_a.clone()
        }
    } else {
        probe_repo(token, update, UPDATE).await
    };
    review_a.app = installed(&app, review);
    update_a.app = installed(&app, update);
    Answers {
        review: review_a,
        update: update_a,
        fork: fork_a,
    }
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

    fn reachable() -> RepoAnswers {
        RepoAnswers {
            repo: Some(200),
            branch: Some("main".into()),
            file: Some(200),
            app: Some(200),
        }
    }

    fn all_ok() -> Answers {
        Answers {
            review: reachable(),
            update: reachable(),
            fork: Some(200),
        }
    }

    const SAME: Names = Names {
        review: "alice/nixpkgs-review-gha",
        update: "alice/nixpkgs-review-gha",
        fork: "alice/nixpkgs",
        saved: true,
    };

    fn check(n: &Names, a: &Answers) -> Check {
        Check {
            at: Utc::now(),
            lines: verdict(n, a),
        }
    }

    #[test]
    fn a_complete_setup_is_ready_for_both_buttons() {
        let c = check(&SAME, &all_ok());
        assert!(c.blocker(Needed::Review).is_none() && c.blocker(Needed::Update).is_none());
        for step in [1, 2, 4, 5] {
            assert_eq!(c.step(step), Mark::Ok, "step {step}");
        }
        assert_eq!(c.step(3), Mark::Unknown, "secrets stay unread");
        assert_eq!(c.lines.iter().filter(|l| l.step == 1).count(), 1);
    }

    #[test]
    fn a_missing_repository_skips_what_depends_on_it() {
        let a = Answers {
            review: RepoAnswers {
                repo: Some(404),
                ..RepoAnswers::default()
            },
            update: RepoAnswers {
                repo: Some(404),
                ..RepoAnswers::default()
            },
            fork: Some(200),
        };
        let c = check(&SAME, &a);
        assert!(!c.blocker(Needed::Review).is_none() && !c.blocker(Needed::Update).is_none());
        assert_eq!(c.step(1), Mark::Missing);
        assert_eq!(c.step(2), Mark::Unknown);
        assert_eq!(c.step(4), Mark::Unknown);
        assert!(
            c.blocker(Needed::Review)
                .unwrap()
                .text
                .contains("Fork nixpkgs-review-gha")
        );
    }

    #[test]
    fn an_app_without_access_blocks_the_buttons() {
        let mut a = all_ok();
        a.review.app = Some(404);
        a.update.app = Some(404);
        let c = check(&SAME, &a);
        assert_eq!(c.step(4), Mark::Missing);
        assert!(!c.blocker(Needed::Review).is_none());
        assert!(
            c.blocker(Needed::Update)
                .unwrap()
                .text
                .contains("Install the App")
        );
    }

    #[test]
    fn only_a_listed_installation_counts_as_installed() {
        let page = json!({ "repositories": [
            { "full_name": "Alice/nixpkgs-review-gha" },
            { "full_name": "alice/dotfiles" },
        ]});
        let repos = Ok(full_names(page["repositories"].as_array().unwrap()));
        assert_eq!(installed(&repos, "alice/nixpkgs-review-gha"), Some(200));
        assert_eq!(installed(&repos, "alice/updates"), Some(404));
        assert_eq!(
            installed(&Ok(vec![]), "alice/nixpkgs-review-gha"),
            Some(404)
        );
        assert_eq!(installed(&Err(Some(401)), "alice/x"), Some(401));
        assert_eq!(installed(&Err(None), "alice/x"), None);
    }

    #[test]
    fn a_public_repository_without_the_app_blocks_the_buttons() {
        let mut a = all_ok();
        let none = Ok(vec!["alice/dotfiles".to_string()]);
        a.review.app = installed(&none, SAME.review);
        a.update.app = installed(&none, SAME.update);
        let c = check(&SAME, &a);
        assert_eq!(c.step(1), Mark::Ok);
        assert_eq!(c.step(4), Mark::Missing);
        assert!(
            c.blocker(Needed::Review)
                .unwrap()
                .text
                .contains("The App is not installed on alice/nixpkgs-review-gha")
        );
    }

    #[test]
    fn a_missing_update_workflow_blocks_only_the_update_button() {
        let mut a = all_ok();
        a.update.file = Some(404);
        let c = check(&SAME, &a);
        assert!(c.blocker(Needed::Review).is_none());
        assert!(!c.blocker(Needed::Update).is_none());
        let l = c.blocker(Needed::Update).unwrap();
        assert_eq!(l.step, 2);
        assert!(
            l.text
                .contains("update.yml on its default branch main. Copy gha/update.yml")
        );
    }

    #[test]
    fn a_missing_fork_blocks_only_the_update_button() {
        let mut a = all_ok();
        a.fork = Some(404);
        let c = check(&SAME, &a);
        assert!(c.blocker(Needed::Review).is_none());
        assert!(!c.blocker(Needed::Update).is_none());
        assert_eq!(c.step(3), Mark::Missing);
    }

    #[test]
    fn two_repositories_are_checked_apart() {
        let n = Names {
            update: "alice/updates",
            ..SAME
        };
        let mut a = all_ok();
        a.update.app = Some(404);
        let c = check(&n, &a);
        assert_eq!(c.lines.iter().filter(|l| l.step == 1).count(), 2);
        assert!(c.blocker(Needed::Review).is_none());
        assert!(!c.blocker(Needed::Update).is_none());
        assert!(
            c.blocker(Needed::Update)
                .unwrap()
                .text
                .contains("alice/updates")
        );
    }

    #[test]
    fn no_answer_from_github_blocks_nothing() {
        let a = Answers {
            review: RepoAnswers::default(),
            update: RepoAnswers::default(),
            fork: None,
        };
        let c = check(&SAME, &a);
        assert!(c.blocker(Needed::Review).is_none() && c.blocker(Needed::Update).is_none());
        assert_eq!(c.step(1), Mark::Unknown);
        assert!(c.lines[0].text.contains("did not answer"));
    }

    #[test]
    fn a_refused_token_says_to_sign_in_again() {
        let a = Answers {
            review: RepoAnswers {
                repo: Some(401),
                ..RepoAnswers::default()
            },
            update: RepoAnswers::default(),
            fork: Some(401),
        };
        let c = check(&SAME, &a);
        assert!(!c.blocker(Needed::Review).is_none());
        assert_eq!(c.blocker(Needed::Review).unwrap().text, EXPIRED);
    }

    #[test]
    fn unsaved_names_use_the_defaults_and_block_nothing() {
        let n = Names {
            saved: false,
            ..SAME
        };
        let c = check(&n, &all_ok());
        assert_eq!(c.step(5), Mark::Unknown);
        assert!(c.blocker(Needed::Review).is_none());
    }

    #[test]
    fn a_check_survives_the_settings_store() {
        let c = check(&SAME, &all_ok());
        let back: Check = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.lines, c.lines);
    }
}
