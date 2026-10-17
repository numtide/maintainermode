//! The personal menu points here. Everything under /me is private to the
//! person who signed in.

use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;

use crate::ui::{Page, Section, section};
use crate::web::{Shared, esc, octicon};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/me", get(me_page))
        .route("/me/runs", get(runs_page))
}

fn links(list: &[(&str, &str, &str)]) -> String {
    list.iter()
        .map(|(href, label, what)| {
            format!(
                "<div class=\"Box-row\"><a class=\"text-bold\" href=\"{href}\">{label}</a>\
                 <div class=\"f6 color-fg-muted\">{what}</div></div>"
            )
        })
        .collect()
}

async fn me_page(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let maintains = format!("/u/{}/maintains", esc(&login));
    let record = format!("/u/{}", esc(&login));
    let body = section(
        "Your pages",
        None,
        None,
        &links(&[
            (
                "/me/inbox",
                "Inbox",
                "New pull requests, merges, issues, advisories and releases.",
            ),
            (
                &maintains,
                "Maintain",
                "What nixpkgs names you as a maintainer of. This page is public.",
            ),
            (
                "/watch",
                "Watch",
                "The packages, modules and directories you watch.",
            ),
            (
                "/me/sets",
                "My machines",
                "The packages your own machines run.",
            ),
            (
                "/me/progress",
                "Progress",
                "Your reviews, your streak and your goal.",
            ),
            (
                &record,
                "Record",
                "Your reviews and what came of each. This page is public.",
            ),
            ("/me/settings", "Settings", "Email, LLMs and tokens."),
        ]),
    );
    Page::new(Section::Home, "My packages")
        .icon(octicon!("package"))
        .sub("Only you see this page.")
        .body(body)
        .render(&app, Some(&login))
}

async fn runs_page(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let body = section(
        "Workflow runs",
        None,
        None,
        &links(&[(
            "/actions",
            "Actions",
            "Your nixpkgs-review and nix-update runs, and the repositories they run in.",
        )]),
    );
    Page::new(Section::Home, "Runs")
        .icon(octicon!("play"))
        .sub("The workflows you started in your own repositories.")
        .body(body)
        .render(&app, Some(&login))
}
