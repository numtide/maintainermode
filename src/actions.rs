//! The page and the buttons that start a nixpkgs-review run on a pull
//! request, or a nix-update of a package, in the person's own workflow
//! repository.

use axum::Router;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;
use std::fmt::Write;

use crate::ui::{Page, Section};
use crate::web::{App, Shared, ago, esc, icon, octicon};
use crate::workflows::{self, Check, Line, Mark, Names, Needed, REVIEW, UPDATE, valid_repo};
use chrono::Utc;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/actions", get(actions_page))
        .route("/me/settings/workflows", post(save_settings))
        .route("/me/settings/workflows/check", post(check_again))
        .route("/me/runs", get(runs_page))
        .route("/me/runs/review", post(start_review))
        .route("/me/runs/update", post(start_update))
}

pub struct Repos {
    pub review: String,
    pub update: String,
    pub fork: String,
}

pub fn repos(app: &App, login: &str) -> Repos {
    let get = |k: &str, default: String| {
        app.store
            .setting(login, k)
            .ok()
            .flatten()
            .filter(|v| !v.is_empty())
            .unwrap_or(default)
    };
    Repos {
        review: get("review_repo", format!("{login}/nixpkgs-review-gha")),
        update: get("update_repo", format!("{login}/nixpkgs-review-gha")),
        fork: get("nixpkgs_fork", format!("{login}/nixpkgs")),
    }
}

/// Which buttons a pull request row shows to the viewer.
#[derive(Clone, Copy, Default)]
pub struct Act {
    pub review: bool,
    pub claude: bool,
    /// The last setup check found a missing step for Review in CI.
    pub incomplete: bool,
}

impl Act {
    pub fn of(app: &App, viewer: Option<&str>) -> Act {
        Act {
            review: viewer.is_some(),
            claude: viewer.is_some_and(|v| crate::llm::opted_in(app, v)),
            incomplete: viewer.is_some_and(|v| !ready(app, v, Needed::Review)),
        }
    }

    pub fn buttons(self, number: u64) -> String {
        let mut out = String::new();
        if self.review {
            out.push_str(&review_button(number, !self.incomplete));
        }
        if self.claude {
            out.push_str(&crate::llm::claude_button(number));
        }
        out
    }
}

/// Whether a button starts its workflow, or links to the setup. Without a
/// fresh check, the person can press it; the click runs the check first.
pub fn ready(app: &App, login: &str, button: Needed) -> bool {
    fresh_check(app, login).is_none_or(|c| c.blocker(button).is_none())
}

fn setup_link(label: &str, class: &str) -> String {
    format!(
        "<a class=\"btn btn-sm {class}\" href=\"{SETUP}\" title=\"finish the setup of your workflow repository first\">{label}</a>"
    )
}

/// A small button for a pull request row.
pub fn review_button(number: u64, ready: bool) -> String {
    if !ready {
        return setup_link("Review in CI", "ml-2");
    }
    format!(
        "<form method=\"post\" action=\"/me/runs/review\" class=\"d-inline ml-2\">\
         <input type=\"hidden\" name=\"pr\" value=\"{number}\">\
         <button class=\"btn btn-sm\" type=\"submit\" title=\"run nixpkgs-review in your GitHub Actions\">Review in CI</button></form>"
    )
}

/// The update form for a package page, prefilled with the upstream release.
pub fn update_form(attr: &str, version: Option<&str>, ready: bool) -> String {
    if !ready {
        return format!(
            "{} <span class=\"f6 color-fg-muted\">Finish the setup of your workflow repository first.</span>",
            setup_link("Update with nix-update", "")
        );
    }
    format!(
        "<form method=\"post\" action=\"/me/runs/update\" class=\"d-flex flex-items-center gap-2\">\
         <input type=\"hidden\" name=\"attr\" value=\"{a}\">\
         <input class=\"form-control input-sm input-narrow\" name=\"version\" value=\"{v}\" placeholder=\"newest\" aria-label=\"version\">\
         <button class=\"btn btn-sm\" type=\"submit\" title=\"run nix-update in your GitHub Actions\">Update with nix-update</button></form>",
        a = esc(attr),
        v = esc(version.unwrap_or(""))
    )
}

fn token(app: &App, login: &str) -> Option<String> {
    app.store.token_for(login).ok().flatten()
}

fn flash(app: &App, login: &str, msg: &str) {
    let _ = app.store.set_setting(login, "flash", msg);
}

fn take_flash(app: &App, login: &str) -> Option<String> {
    let m = app.store.setting(login, "flash").ok().flatten()?;
    let _ = app.store.set_setting(login, "flash", "");
    (!m.is_empty()).then_some(m)
}

fn run_rows(runs: &[workflows::Run]) -> String {
    if runs.is_empty() {
        return "<div class=\"Box-row color-fg-muted\">No runs yet.</div>".into();
    }
    runs.iter()
        .map(|r| {
            let (class, word) = match (r.status.as_str(), r.conclusion.as_deref()) {
                ("completed", Some("success")) => ("color-fg-success", "succeeded"),
                ("completed", Some(c)) => ("color-fg-danger", c),
                (s, _) => ("color-fg-attention", s),
            };
            format!(
                "<div class=\"Box-row d-flex\"><a class=\"flex-auto Link--primary\" href=\"{}\">{}</a>\
                 <span class=\"{class} mr-2\">{}</span><span class=\"color-fg-muted f6\">{}</span></div>",
                esc(&r.url),
                esc(&r.name),
                esc(word),
                r.created_at.map(|t| format!("{} ago", ago(t))).unwrap_or_default()
            )
        })
        .collect()
}

/// What an update run left: a branch to open a pull request from, a branch
/// to keep working on, or no branch. `pushed` is `None` when GitHub did not
/// say.
fn update_row(attr: &str, fork: &str, conclusion: Option<&str>, pushed: Option<bool>) -> String {
    let success = conclusion == Some("success");
    if pushed == Some(false) {
        let text = if success {
            format!("{} is already up to date. No branch was pushed.", esc(attr))
        } else {
            format!(
                "{}: {}. No branch was pushed.",
                esc(attr),
                esc(conclusion.unwrap_or("not finished"))
            )
        };
        return format!("<div class=\"Box-row color-fg-muted\">{text}</div>");
    }
    format!(
        "<div class=\"Box-row d-flex\"><span class=\"flex-auto\"><span class=\"branch-name\">{b}</span> on {f}</span>\
         <a class=\"btn btn-sm\" href=\"{}\">{}</a></div>",
        esc(&workflows::compare_url(fork, attr)),
        if success {
            "Open a pull request"
        } else {
            "Open the branch and continue"
        },
        b = esc(&workflows::update_branch(attr)),
        f = esc(fork),
    )
}

/// The last finished run of each attribute (`update hello 2.13`), and what
/// it left.
async fn update_links(token: &str, repo: &str, runs: &[workflows::Run], fork: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    let latest: Vec<(&str, &workflows::Run)> = runs
        .iter()
        .filter_map(|r| {
            let attr = r.name.strip_prefix("update ")?.split_whitespace().next()?;
            (r.status == "completed" && seen.insert(attr)).then_some((attr, r))
        })
        .collect();
    let pushed = futures_util::future::join_all(
        latest
            .iter()
            .map(|(_, r)| workflows::pushed(token, repo, r.id)),
    )
    .await;
    latest
        .iter()
        .zip(pushed)
        .map(|((attr, r), p)| update_row(attr, fork, r.conclusion.as_deref(), p.ok()))
        .collect()
}

pub fn app_page(slug: &str) -> String {
    format!("https://github.com/apps/{slug}")
}

const DOCS: &str = "https://github.com/numtide/maintainermode/blob/main/docs/actions.md";

const WHAT: &str = "Run nixpkgs-review on a pull request, or nix-update on a package, in GitHub Actions of a repository you own. \
     Maintainer Mode starts the workflow with your token when you press a button; the workflow acts as you. \
     It never writes to nixpkgs itself.";

/// Signed in, the runs are on `/me/runs`. Signed out, a short page says
/// what the buttons do.
async fn actions_page(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if app.viewer(&headers).is_some() {
        return Redirect::to("/me/runs").into_response();
    }
    let sign_in = if app.oauth.is_some() {
        "<a class=\"btn btn-primary\" href=\"/auth/login\">Sign in with GitHub</a> "
    } else {
        ""
    };
    let app_link = app
        .github_app_slug
        .as_deref()
        .map(|s| {
            format!(
                "<p class=\"f6 color-fg-muted\">The workflows start through the GitHub App <a href=\"{}\">{}</a>. You install it on your workflow repository only.</p>",
                esc(&app_page(s)),
                esc(s)
            )
        })
        .unwrap_or_default();
    Page::new(Section::Home, "Actions")
        .icon(octicon!("play"))
        .sub(WHAT)
        .body(format!(
            "<div class=\"blankslate\"><p>Sign in to run workflows. Your runs are then in the menu, under Runs.</p>\
             <p>{sign_in}<a class=\"btn\" href=\"{DOCS}\">How to set it up</a></p>{app_link}</div>"
        ))
        .render(&app, None)
}

async fn runs_page(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let viewer = app.viewer(&headers);
    let page = Page::new(Section::Home, "Runs")
        .icon(octicon!("play"))
        .sub(WHAT);
    let mut out = String::new();
    let Some(login) = viewer.as_deref() else {
        return Redirect::to("/auth/login").into_response();
    };
    if let Some(m) = take_flash(&app, login) {
        let _ = write!(out, "<div class=\"flash mb-3\">{}</div>", esc(&m));
    }
    let r = repos(&app, login);
    out.push_str(&crate::ui::note(&format!(
        "Runs start in {} and {}. <a href=\"/me/settings#workflows\">Change the repositories</a>.",
        esc(&r.review),
        esc(&r.update)
    )));
    let review_ready = ready(&app, login, Needed::Review);
    if !review_ready || !ready(&app, login, Needed::Update) {
        let _ = write!(
            out,
            "<div class=\"flash flash-warn mb-3\">The setup of your workflow repository is not complete. \
             <a href=\"{SETUP}\">Finish it in your settings</a>.</div>"
        );
    }
    if review_ready {
        out.push_str(
            "<form method=\"post\" action=\"/me/runs/review\" class=\"inline-form mb-4\">\
             <input class=\"form-control flex-auto\" name=\"pr\" placeholder=\"pull request number\" aria-label=\"pull request number\" inputmode=\"numeric\">\
             <button class=\"btn btn-primary\" type=\"submit\">Review in CI</button></form>",
        );
    }
    let Some(t) = token(&app, login) else {
        out.push_str("<div class=\"blankslate\"><p>No GitHub token in your session; sign in again.</p></div>");
        return page.body(out).render(&app, viewer.as_deref());
    };
    for (title, repo, file) in [
        ("nixpkgs-review runs", &r.review, REVIEW),
        ("nix-update runs", &r.update, UPDATE),
    ] {
        let body = match workflows::runs(&t, repo, file).await {
            Ok(runs) => {
                let mut b = run_rows(&runs);
                if file == UPDATE {
                    b.push_str(&update_links(&t, repo, &runs, &r.fork).await);
                }
                b
            }
            Err(e) => format!(
                "<div class=\"Box-row color-fg-muted\">{}</div>",
                esc(&format!("{e:#}"))
            ),
        };
        out.push_str(&crate::ui::section(
            &format!(
                "{title} <span class=\"color-fg-muted text-normal f6\">{} · {file}</span>",
                esc(repo)
            ),
            None,
            None,
            &body,
        ));
    }
    page.body(out).render(&app, viewer.as_deref())
}

const CHECK_KEY: &str = "workflows_check";
const CHECK_TTL_SECS: i64 = 300;
const REVIEW_GHA: &str = "https://github.com/Defelo/nixpkgs-review-gha";
const UPDATE_YML: &str = "https://github.com/numtide/maintainermode/blob/main/gha/update.yml";

fn saved(app: &App, login: &str) -> bool {
    ["review_repo", "update_repo", "nixpkgs_fork"]
        .iter()
        .any(|k| {
            app.store
                .setting(login, k)
                .ok()
                .flatten()
                .is_some_and(|v| !v.is_empty())
        })
}

/// The last setup check, of any age.
fn last_check(app: &App, login: &str) -> Option<Check> {
    let v = app.store.setting(login, CHECK_KEY).ok().flatten()?;
    serde_json::from_str(&v).ok()
}

fn fresh_check(app: &App, login: &str) -> Option<Check> {
    last_check(app, login).filter(|c| (Utc::now() - c.at).num_seconds() < CHECK_TTL_SECS)
}

pub(crate) fn store_check(app: &App, login: &str, c: &Check) {
    if let Ok(v) = serde_json::to_string(c) {
        let _ = app.store.set_setting(login, CHECK_KEY, &v);
    }
}

pub(crate) fn forget_check(app: &App, login: &str) {
    let _ = app.store.set_setting(login, CHECK_KEY, "");
}

/// The setup check of the person, from the cache while it is fresh. `None`
/// without a token.
async fn setup(app: &App, login: &str) -> Option<Check> {
    if let Some(c) = fresh_check(app, login) {
        return Some(c);
    }
    let t = token(app, login)?;
    let r = repos(app, login);
    let answers = workflows::probe(&t, &r.review, &r.update, &r.fork).await;
    let c = Check {
        at: Utc::now(),
        lines: workflows::verdict(
            &Names {
                review: &r.review,
                update: &r.update,
                fork: &r.fork,
                saved: saved(app, login),
            },
            &answers,
        ),
    };
    store_check(app, login, &c);
    Some(c)
}

const SETUP: &str = "/me/settings#workflows";

fn mark_html(mark: Mark) -> (&'static str, String, &'static str) {
    match mark {
        Mark::Ok => (
            "color-fg-success",
            icon(octicon!("check-circle-fill")),
            "done",
        ),
        Mark::Missing => ("color-fg-danger", icon(octicon!("x-circle-fill")), "to do"),
        Mark::Unknown => ("color-fg-muted", icon(octicon!("circle")), "not checked"),
    }
}

fn step_row(n: u8, mark: Mark, title: &str, body: &str, lines: &[&Line]) -> String {
    let (class, ic, word) = mark_html(mark);
    let lines: String = lines
        .iter()
        .map(|l| {
            let (class, ic, _) = mark_html(l.mark);
            format!("<li class=\"f6 {class}\">{ic} {}</li>", esc(&l.text))
        })
        .collect();
    let lines = if lines.is_empty() {
        String::new()
    } else {
        format!("<ul class=\"list-style-none mt-1\">{lines}</ul>")
    };
    format!(
        "<div class=\"Box-row d-flex flex-items-start\">\
         <span class=\"{class} mr-2\" title=\"{word}\">{ic}</span>\
         <div class=\"flex-auto min-width-0\"><div class=\"text-bold\">{n}. {title} <span class=\"Label ml-1\">{word}</span></div>\
         <div class=\"mt-1\">{body}</div>{lines}</div></div>"
    )
}

/// The `#workflows` section of `/me/settings`: the setup steps as a
/// checklist, each with what the check found.
pub async fn repos_box(app: &App, login: &str) -> String {
    let r = repos(app, login);
    let flash = take_flash(app, login)
        .map(|m| format!("<div class=\"flash m-2\">{}</div>", esc(&m)))
        .unwrap_or_default();
    let check = setup(app, login).await;
    let mark = |n: u8| match &check {
        Some(c) => c.step(n),
        None if n == 5 && saved(app, login) => Mark::Ok,
        None => Mark::Unknown,
    };
    let lines = |n: u8| -> Vec<&Line> {
        check
            .iter()
            .flat_map(|c| c.lines.iter().filter(move |l| l.step == n))
            .collect()
    };
    let install = match app.github_app_slug.as_deref() {
        Some(slug) => format!(
            "<a class=\"btn btn-sm btn-primary\" href=\"{}/installations/new\">Install {}</a> \
             <span class=\"f6 color-fg-muted\">Choose \"Only select repositories\" and pick {}. \
             The App needs the \"Actions: read and write\" permission and nothing else.</span>",
            esc(&app_page(slug)),
            esc(slug),
            esc(&r.update)
        ),
        None => "This site names no GitHub App. Ask the people who run it.".into(),
    };
    let steps = [
        step_row(
            1,
            mark(1),
            &format!("Fork <a href=\"{REVIEW_GHA}\">Defelo/nixpkgs-review-gha</a>"),
            &format!("<a class=\"btn btn-sm\" href=\"{REVIEW_GHA}/fork\">Fork it on GitHub</a>"),
            &lines(1),
        ),
        step_row(
            2,
            mark(2),
            &format!(
                "Copy <a href=\"{UPDATE_YML}\">gha/update.yml</a> into <code>.github/workflows/</code> of your fork"
            ),
            "<span class=\"f6 color-fg-muted\">The fork already has <code>review.yml</code>. \
             Commit the file on the default branch.</span>",
            &lines(2),
        ),
        step_row(
            3,
            mark(3),
            "Add a <code>GH_TOKEN</code> secret that can push to your nixpkgs fork",
            &format!(
                "<a class=\"btn btn-sm\" href=\"https://github.com/{u}/settings/secrets/actions\">Open the secrets of {u}</a> \
                 <span class=\"f6 color-fg-muted\">No fork of nixpkgs yet? \
                 <a href=\"https://github.com/NixOS/nixpkgs/fork\">Fork NixOS/nixpkgs</a>.</span>",
                u = esc(&r.update)
            ),
            &lines(3),
        ),
        step_row(
            4,
            mark(4),
            "Install the GitHub App on that fork only",
            &install,
            &lines(4),
        ),
        step_row(
            5,
            mark(5),
            "Save the repository names",
            &format!(
                "<form method=\"post\" action=\"/me/settings/workflows\">\
                 <label class=\"f6\" for=\"review_repo\">Your fork of nixpkgs-review-gha, with <code>review.yml</code></label>\
                 <input class=\"form-control width-full mb-2\" id=\"review_repo\" name=\"review_repo\" value=\"{}\">\
                 <label class=\"f6\" for=\"update_repo\">The repository with <code>update.yml</code>; the same one works</label>\
                 <input class=\"form-control width-full mb-2\" id=\"update_repo\" name=\"update_repo\" value=\"{}\">\
                 <label class=\"f6\" for=\"nixpkgs_fork\">Your fork of nixpkgs, where updates push their branch</label>\
                 <input class=\"form-control width-full mb-2\" id=\"nixpkgs_fork\" name=\"nixpkgs_fork\" value=\"{}\">\
                 <button class=\"btn btn-sm\" type=\"submit\">Save</button></form>",
                esc(&r.review),
                esc(&r.update),
                esc(&r.fork)
            ),
            &lines(5),
        ),
    ]
    .concat();
    let status = match &check {
        Some(c) => format!(
            "Checked {} ago with your GitHub token. The check only reads.",
            ago(c.at)
        ),
        None => "Not checked: there is no GitHub token in your session. Sign in again.".into(),
    };
    format!(
        "<div id=\"workflows\">{}</div>",
        crate::ui::section(
            &format!("{} Workflows", icon(octicon!("play"))),
            None,
            Some((DOCS, "docs/actions.md")),
            &format!(
                "{flash}{steps}<div class=\"Box-row d-flex flex-items-center\">\
                 <span class=\"flex-auto f6 color-fg-muted\">{status}</span>\
                 <form method=\"post\" action=\"/me/settings/workflows/check\">\
                 <button class=\"btn btn-sm\" type=\"submit\">Check again</button></form></div>"
            )
        )
    )
}

/// The redirect to the setup when the check finds a missing step, so that a
/// click never fails on a setup that is not complete.
async fn not_ready(app: &App, login: &str, button: Needed) -> Option<Response> {
    let c = setup(app, login).await?;
    let l = c.blocker(button)?;
    flash(
        app,
        login,
        &format!("Finish the setup first (step {}): {}", l.step, l.text),
    );
    Some(Redirect::to(SETUP).into_response())
}

async fn check_again(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    forget_check(&app, &login);
    Redirect::to(SETUP).into_response()
}

#[derive(Deserialize)]
struct SettingsForm {
    review_repo: String,
    update_repo: String,
    nixpkgs_fork: String,
}

async fn save_settings(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<SettingsForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    for (k, v) in [
        ("review_repo", f.review_repo.trim()),
        ("update_repo", f.update_repo.trim()),
        ("nixpkgs_fork", f.nixpkgs_fork.trim()),
    ] {
        if !v.is_empty() && !valid_repo(v) {
            return (StatusCode::BAD_REQUEST, format!("{v} is not owner/name")).into_response();
        }
        if let Err(e) = app.store.set_setting(&login, k, v) {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }
    forget_check(&app, &login);
    Redirect::to(SETUP).into_response()
}

#[derive(Deserialize)]
struct ReviewForm {
    pr: String,
}

async fn start_review(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<ReviewForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let Ok(pr) = f.pr.trim().trim_start_matches('#').parse::<u64>() else {
        return (StatusCode::BAD_REQUEST, "not a pull request number").into_response();
    };
    let Some(t) = token(&app, &login) else {
        return Redirect::to("/auth/login").into_response();
    };
    if let Some(r) = not_ready(&app, &login, Needed::Review).await {
        return r;
    }
    let repo = repos(&app, &login).review;
    let msg = match workflows::dispatch(&t, &repo, REVIEW, json!({ "pr": pr.to_string() })).await {
        Ok(()) => format!("Started nixpkgs-review of #{pr} in {repo}."),
        Err(e) => {
            forget_check(&app, &login);
            format!("Could not start nixpkgs-review of #{pr}: {e:#}")
        }
    };
    flash(&app, &login, &msg);
    Redirect::to("/me/runs").into_response()
}

#[derive(Deserialize)]
struct UpdateForm {
    attr: String,
    #[serde(default)]
    version: String,
}

async fn start_update(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<UpdateForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let attr = f.attr.trim().to_string();
    if !app.index.read().unwrap().pkgs.contains_key(&attr) {
        return (StatusCode::BAD_REQUEST, "not an attribute in the index").into_response();
    }
    let version = f.version.trim().to_string();
    if !version
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.+".contains(c))
    {
        return (StatusCode::BAD_REQUEST, "not a version").into_response();
    }
    let Some(t) = token(&app, &login) else {
        return Redirect::to("/auth/login").into_response();
    };
    if let Some(r) = not_ready(&app, &login, Needed::Update).await {
        return r;
    }
    let r = repos(&app, &login);
    let inputs = json!({ "attr": attr, "version": version, "fork": r.fork });
    let msg = match workflows::dispatch(&t, &r.update, UPDATE, inputs).await {
        Ok(()) => format!(
            "Started nix-update of {attr} in {}. It pushes {} to {} when nix-update changes anything, even if the build fails.",
            r.update,
            workflows::update_branch(&attr),
            r.fork
        ),
        Err(e) => {
            forget_check(&app, &login);
            format!("Could not start nix-update of {attr}: {e:#}")
        }
    };
    flash(&app, &login, &msg);
    Redirect::to("/me/runs").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn actions_sends_a_person_to_their_runs() {
        let app = crate::sets::tests::app(&[]);
        let r = actions_page(State(app.clone()), HeaderMap::new()).await;
        assert_eq!(r.status(), StatusCode::OK);
        let r = runs_page(State(app.clone()), HeaderMap::new()).await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        let h = crate::notify::tests::signed_in(&app, "alice");
        let r = actions_page(State(app.clone()), h).await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        assert_eq!(r.headers()["location"], "/me/runs");
    }

    #[test]
    fn the_buttons_post_under_me() {
        assert!(review_button(1, true).contains("action=\"/me/runs/review\""));
        assert!(update_form("hello", None, true).contains("action=\"/me/runs/update\""));
    }

    #[test]
    fn an_incomplete_setup_turns_the_buttons_into_links() {
        let app = crate::sets::tests::app(&[]);
        assert!(ready(&app, "alice", Needed::Review), "never checked");
        missing_app(&app, "alice");
        let act = Act::of(&app, Some("alice"));
        let html = act.buttons(1);
        assert!(html.contains(&format!("href=\"{SETUP}\"")));
        assert!(!html.contains("<form"));
        assert!(!ready(&app, "alice", Needed::Update));
        let form = update_form("hello", None, false);
        assert!(form.contains(&format!("href=\"{SETUP}\"")));
        assert!(!form.contains("<form"));
    }

    #[test]
    fn a_stale_check_leaves_the_buttons_as_forms() {
        let app = crate::sets::tests::app(&[]);
        missing_app(&app, "alice");
        let mut c = last_check(&app, "alice").unwrap();
        c.at = Utc::now() - chrono::Duration::seconds(CHECK_TTL_SECS + 1);
        store_check(&app, "alice", &c);
        assert!(ready(&app, "alice", Needed::Review));
        assert!(Act::of(&app, Some("alice")).buttons(1).contains("<form"));
    }

    fn missing_app(app: &App, login: &str) {
        let mut a = workflows::Answers {
            fork: Some(200),
            ..Default::default()
        };
        a.review = workflows::RepoAnswers {
            repo: Some(200),
            branch: Some("main".into()),
            file: Some(200),
            app: Some(404),
        };
        a.update = a.review.clone();
        let r = repos(app, login);
        let names = Names {
            review: &r.review,
            update: &r.update,
            fork: &r.fork,
            saved: false,
        };
        store_check(
            app,
            login,
            &Check {
                at: Utc::now(),
                lines: workflows::verdict(&names, &a),
            },
        );
    }

    #[tokio::test]
    async fn the_settings_show_each_step_and_what_the_check_found() {
        let app = crate::sets::tests::app(&[]);
        missing_app(&app, "alice");
        let html = repos_box(&app, "alice").await;
        for n in 1..=5 {
            assert!(html.contains(&format!("{n}. ")), "step {n}");
        }
        assert!(html.contains("https://github.com/Defelo/nixpkgs-review-gha/fork"));
        assert!(html.contains("gha/update.yml"));
        assert!(
            html.contains("https://github.com/alice/nixpkgs-review-gha/settings/secrets/actions")
        );
        assert!(html.contains("The App is not installed on alice/nixpkgs-review-gha."));
        assert!(html.contains("action=\"/me/settings/workflows/check\""));
        assert!(!html.contains("token\":"));
    }

    #[tokio::test]
    async fn a_click_on_an_incomplete_setup_opens_the_setup() {
        let app = crate::sets::tests::app(&[]);
        let h = crate::notify::tests::signed_in(&app, "alice");
        missing_app(&app, "alice");
        let r = start_review(
            State(app.clone()),
            h.clone(),
            Form(ReviewForm { pr: "1".into() }),
        )
        .await;
        assert_eq!(r.headers()["location"], SETUP);
        let html = repos_box(&app, "alice").await;
        assert!(html.contains("Finish the setup first (step 4)"));
        let r = check_again(State(app.clone()), h).await;
        assert_eq!(r.headers()["location"], SETUP);
        assert!(last_check(&app, "alice").is_none());
    }

    #[test]
    fn an_update_row_tells_the_four_cases_apart() {
        let row = |c, p| update_row("hello", "alice/nixpkgs", Some(c), p);
        assert!(row("success", Some(true)).contains("Open a pull request"));
        assert!(row("failure", Some(true)).contains("Open the branch and continue"));
        let fresh = row("success", Some(false));
        assert!(fresh.contains("hello is already up to date"));
        assert!(!fresh.contains("href"));
        let failed = row("failure", Some(false));
        assert!(failed.contains("failure. No branch was pushed."));
        assert!(!failed.contains("href"));
        assert!(row("failure", None).contains("Open the branch and continue"));
    }
}
