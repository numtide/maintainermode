//! A person's API token: the CLI and other tools send it as
//! `Authorization: Bearer` to read the private parts of `/api/v1`.

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use chrono::Utc;

use crate::html::{Html, html};
use crate::sets::token_hash;
use crate::ui::{Page, Section};
use crate::web::{App, Shared, octicon, random_id};

const HASH: &str = "api_token_hash";
const AT: &str = "api_token_at";

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/me/api-token", post(new_token))
        .route("/me/api-token/delete", post(delete_token))
}

pub fn login_of(app: &App, token: &str) -> Option<String> {
    app.store
        .setting_login(HASH, &token_hash(token))
        .ok()
        .flatten()
}

fn made_at(app: &App, login: &str) -> Option<String> {
    let hash = app.store.setting(login, HASH).ok().flatten()?;
    if hash.is_empty() {
        return None;
    }
    app.store.setting(login, AT).ok().flatten()
}

/// The `#api` section of `/me/settings`.
pub fn settings_box(app: &App, login: &str) -> Html {
    let state = match made_at(app, login) {
        Some(at) => html!(
            "<p class=\"mb-2\">You made a token on {}.</p>\
             <form method=\"post\" action=\"/me/api-token\" class=\"d-inline\"><button class=\"btn btn-sm\" type=\"submit\">Replace the token</button></form> \
             <form method=\"post\" action=\"/me/api-token/delete\" class=\"d-inline\"><button class=\"btn btn-sm btn-danger\" type=\"submit\">Revoke the token</button></form>",
            at.get(..10).unwrap_or(&at)
        ),
        None => "<form method=\"post\" action=\"/me/api-token\"><button class=\"btn btn-sm btn-primary\" type=\"submit\">Make an API token</button></form>".into(),
    };
    html!(
        "<p class=\"mb-2\">The <code>maintainer-mode</code> command and your own tools read your private data from \
         <a href=\"https://github.com/numtide/maintainermode/blob/main/docs/api.md\">the API</a> with this token, \
         as <code>Authorization: Bearer</code>. It only reads.</p>{state}"
    )
}

async fn new_token(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let token = random_id();
    let saved = app
        .store
        .set_setting(&login, HASH, &token_hash(&token))
        .and_then(|_| app.store.set_setting(&login, AT, &Utc::now().to_rfc3339()));
    if let Err(e) = saved {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    let out = html!(
        "<div class=\"Box mb-4\"><div class=\"Box-row\">\
         <input class=\"form-control width-full mb-2 text-mono\" readonly value=\"{token}\" aria-label=\"API token\">\
         <p>For the command line: <code>export MAINTAINER_MODE_API_TOKEN=…</code></p>\
         <p>Any token you made before no longer works.</p>\
         <a class=\"btn btn-sm\" href=\"/me/settings#api\">Back to your settings</a></div></div>"
    );
    Page::new(
        Section::Personal(crate::ui::Personal::Settings),
        "Your API token",
    )
    .icon(octicon!("key"))
    .sub("Copy it now. The server keeps only its hash, so this page cannot show it again.")
    .body(out)
    .render(&app, Some(&login))
}

async fn delete_token(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let done = app
        .store
        .set_setting(&login, HASH, "")
        .and_then(|_| app.store.set_setting(&login, AT, ""));
    if let Err(e) = done {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    Redirect::to("/me/settings#api").into_response()
}
