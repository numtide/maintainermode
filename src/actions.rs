//! The page and the buttons that start a nixpkgs-review run on a pull
//! request, or a nix-update of a package, in the person's own workflow
//! repository.

use axum::Router;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;
use std::fmt::Write;

use crate::web::{App, Shared, ago, esc, icon, octicon, page};
use crate::workflows::{self, REVIEW, UPDATE, valid_repo};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/actions", get(actions_page))
        .route("/actions/settings", post(save_settings))
        .route("/actions/review", post(start_review))
        .route("/actions/update", post(start_update))
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
}

impl Act {
    pub fn of(app: &App, viewer: Option<&str>) -> Act {
        Act {
            review: viewer.is_some(),
            claude: viewer.is_some_and(|v| crate::llm::opted_in(app, v)),
        }
    }

    pub fn buttons(self, number: u64) -> String {
        let mut out = String::new();
        if self.review {
            out.push_str(&review_button(number));
        }
        if self.claude {
            out.push_str(&crate::llm::claude_button(number));
        }
        out
    }
}

/// A small button for a pull request row.
pub fn review_button(number: u64) -> String {
    format!(
        "<form method=\"post\" action=\"/actions/review\" class=\"d-inline ml-2\">\
         <input type=\"hidden\" name=\"pr\" value=\"{number}\">\
         <button class=\"btn btn-sm\" type=\"submit\" title=\"run nixpkgs-review in your GitHub Actions\">Review in CI</button></form>"
    )
}

/// The update form for a package page, prefilled with the upstream release.
pub fn update_form(attr: &str, version: Option<&str>) -> String {
    format!(
        "<form method=\"post\" action=\"/actions/update\" class=\"d-flex flex-items-center\" style=\"gap:8px\">\
         <input type=\"hidden\" name=\"attr\" value=\"{a}\">\
         <input class=\"form-control input-sm\" name=\"version\" value=\"{v}\" placeholder=\"newest\" aria-label=\"version\" style=\"max-width:140px\">\
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

/// `update hello 2.13` → the compare link of `update/hello`.
fn update_links(runs: &[workflows::Run], fork: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out = String::new();
    for r in runs {
        let Some(attr) = r
            .name
            .strip_prefix("update ")
            .and_then(|s| s.split_whitespace().next())
        else {
            continue;
        };
        if r.status != "completed" || !seen.insert(attr.to_string()) {
            continue;
        }
        let _ = write!(
            out,
            "<div class=\"Box-row d-flex\"><span class=\"flex-auto\"><span class=\"branch-name\">{b}</span> on {f}</span>\
             <a class=\"btn btn-sm\" href=\"{}\">{}</a></div>",
            esc(&workflows::compare_url(fork, attr)),
            if r.conclusion.as_deref() == Some("success") {
                "Open a pull request"
            } else {
                "See the branch, keep working"
            },
            b = esc(&workflows::update_branch(attr)),
            f = esc(fork),
        );
    }
    out
}

async fn actions_page(State(app): State<Shared>, headers: HeaderMap) -> Html<String> {
    let viewer = app.viewer(&headers);
    let mut out = String::from(
        "<div class=\"Subhead\"><h1 class=\"Subhead-heading\">Actions</h1>\
         <div class=\"Subhead-description\">Run nixpkgs-review on a pull request, or nix-update on a package, in GitHub Actions of a repository you own. \
         Maintainer Mode starts the workflow with your token when you press a button; the workflow acts as you. \
         It never writes to nixpkgs itself.</div></div>",
    );
    let Some(login) = viewer.as_deref() else {
        out.push_str("<div class=\"blankslate\"><p>Sign in to run workflows.</p></div>");
        return page(&app, None, "actions", &out);
    };
    if let Some(m) = take_flash(&app, login) {
        let _ = write!(out, "<div class=\"flash mb-3\">{}</div>", esc(&m));
    }
    let r = repos(&app, login);
    let _ = write!(
        out,
        "<details class=\"Box mb-4\"><summary class=\"Box-header text-bold\">{} Repositories</summary><div class=\"Box-row\">\
         <form method=\"post\" action=\"/actions/settings\">\
         <label class=\"f6\" for=\"review_repo\">Your fork of <a href=\"https://github.com/Defelo/nixpkgs-review-gha\">nixpkgs-review-gha</a>, with <code>review.yml</code></label>\
         <input class=\"form-control width-full mb-2\" id=\"review_repo\" name=\"review_repo\" value=\"{}\">\
         <label class=\"f6\" for=\"update_repo\">The repository with <code>update.yml</code> (see the README); the same one works</label>\
         <input class=\"form-control width-full mb-2\" id=\"update_repo\" name=\"update_repo\" value=\"{}\">\
         <label class=\"f6\" for=\"nixpkgs_fork\">Your fork of nixpkgs, where updates push their branch</label>\
         <input class=\"form-control width-full mb-2\" id=\"nixpkgs_fork\" name=\"nixpkgs_fork\" value=\"{}\">\
         <button class=\"btn btn-sm\" type=\"submit\">Save</button></form>\
         <p class=\"f6 color-fg-muted mt-2\">The Maintainer Mode GitHub App must be installed on the workflow repositories, \
         and the workflows need a <code>GH_TOKEN</code> secret that can push to your nixpkgs fork.</p></div></details>",
        icon(octicon!("gear")),
        esc(&r.review),
        esc(&r.update),
        esc(&r.fork)
    );
    out.push_str(
        "<form method=\"post\" action=\"/actions/review\" class=\"d-flex mb-4\" style=\"gap:8px;max-width:420px\">\
         <input class=\"form-control flex-auto\" name=\"pr\" placeholder=\"pull request number\" aria-label=\"pull request number\" inputmode=\"numeric\">\
         <button class=\"btn btn-primary\" type=\"submit\">Review in CI</button></form>",
    );
    let Some(t) = token(&app, login) else {
        out.push_str("<div class=\"blankslate\"><p>No GitHub token in your session; sign in again.</p></div>");
        return page(&app, viewer.as_deref(), "actions", &out);
    };
    for (title, repo, file) in [
        ("nixpkgs-review runs", &r.review, REVIEW),
        ("nix-update runs", &r.update, UPDATE),
    ] {
        let body = match workflows::runs(&t, repo, file).await {
            Ok(runs) => {
                let mut b = run_rows(&runs);
                if file == UPDATE {
                    b.push_str(&update_links(&runs, &r.fork));
                }
                b
            }
            Err(e) => format!(
                "<div class=\"Box-row color-fg-muted\">{}</div>",
                esc(&format!("{e:#}"))
            ),
        };
        let _ = write!(
            out,
            "<div class=\"Box mb-4\"><div class=\"Box-header text-bold\">{title} <span class=\"color-fg-muted text-normal f6\">{} · {}</span></div>{body}</div>",
            esc(repo),
            file
        );
    }
    page(&app, viewer.as_deref(), "actions", &out)
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
    Redirect::to("/actions").into_response()
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
    let repo = repos(&app, &login).review;
    let msg = match workflows::dispatch(&t, &repo, REVIEW, json!({ "pr": pr.to_string() })).await {
        Ok(()) => format!("Started nixpkgs-review of #{pr} in {repo}."),
        Err(e) => format!("Could not start nixpkgs-review of #{pr}: {e:#}"),
    };
    flash(&app, &login, &msg);
    Redirect::to("/actions").into_response()
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
    let r = repos(&app, &login);
    let inputs = json!({ "attr": attr, "version": version, "fork": r.fork });
    let msg = match workflows::dispatch(&t, &r.update, UPDATE, inputs).await {
        Ok(()) => format!(
            "Started nix-update of {attr} in {}. It pushes {} to {} either way.",
            r.update,
            workflows::update_branch(&attr),
            r.fork
        ),
        Err(e) => format!("Could not start nix-update of {attr}: {e:#}"),
    };
    flash(&app, &login, &msg);
    Redirect::to("/actions").into_response()
}
