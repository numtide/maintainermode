//! A person's choice about LLMs. Off by default. When it is on, an agent
//! with the person's MCP token may read this site through `/mcp`, and the
//! pull request rows offer a Claude Code session.

use axum::Router;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use serde::Deserialize;
use std::fmt::Write;

use crate::catalog::head;
use crate::web::{App, Shared, icon, octicon, page};

const OPT_IN: &str = "llm_ok";

pub fn routes() -> Router<Shared> {
    Router::new().route("/me/settings", get(settings_page).post(save_settings))
}

pub fn opted_in(app: &App, login: &str) -> bool {
    app.store
        .setting(login, OPT_IN)
        .ok()
        .flatten()
        .is_some_and(|v| v == "1")
}

async fn settings_page(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let on = opted_in(&app, &login);
    let mut out = head(
        &format!("{} Settings", icon(octicon!("gear"))),
        "Choices that apply to you only.",
        "",
    );
    let _ = write!(
        out,
        "<div class=\"Box mb-4\" id=\"llm\"><div class=\"Box-header text-bold\">LLMs</div><div class=\"Box-row\">\
         <form method=\"post\" action=\"/me/settings\">\
         <div class=\"form-checkbox mt-0\"><label><input type=\"checkbox\" name=\"llm_ok\" value=\"1\"{checked}> \
         I am OK with LLMs using my data on this site</label>\
         <p class=\"note\">Off by default. When it is on, an agent that holds your MCP token can read what you watch and the public data \
         of this site through <code>/mcp</code>, and each open pull request offers a Claude Code session. \
         Nothing here writes to GitHub. Turning it off revokes your MCP token.</p></div>\
         <button class=\"btn btn-sm\" type=\"submit\">Save</button></form></div></div>",
        checked = if on { " checked" } else { "" },
    );
    page(&app, Some(&login), "", &out).into_response()
}

#[derive(Deserialize)]
struct SettingsForm {
    #[serde(default)]
    llm_ok: Option<String>,
}

async fn save_settings(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<SettingsForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let on = f.llm_ok.as_deref() == Some("1");
    if let Err(e) = app
        .store
        .set_setting(&login, OPT_IN, if on { "1" } else { "" })
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    Redirect::to("/me/settings").into_response()
}
