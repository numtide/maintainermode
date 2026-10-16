//! A person's choice about LLMs. Off by default. When it is on, an agent
//! with the person's MCP token may read this site through `/mcp`, and the
//! pull request rows offer a Claude Code session.

use axum::Router;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::Utc;
use serde::Deserialize;
use std::fmt::Write;

use crate::catalog::head;
use crate::sets::token_hash;
use crate::web::{App, Shared, esc, icon, octicon, page, random_id};

const OPT_IN: &str = "llm_ok";
const MCP_HASH: &str = "mcp_token_hash";
const MCP_AT: &str = "mcp_token_at";

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/me/settings", get(settings_page).post(save_settings))
        .route("/me/mcp-token", post(new_mcp_token))
        .route("/me/mcp-token/delete", post(delete_mcp_token))
}

pub fn opted_in(app: &App, login: &str) -> bool {
    app.store
        .setting(login, OPT_IN)
        .ok()
        .flatten()
        .is_some_and(|v| v == "1")
}

/// The login an MCP token belongs to, whether or not that person still
/// opts in.
pub fn mcp_login(app: &App, token: &str) -> Option<String> {
    app.store
        .setting_login(MCP_HASH, &token_hash(token))
        .ok()
        .flatten()
}

pub fn has_mcp_token(app: &App, login: &str) -> bool {
    app.store
        .setting(login, MCP_HASH)
        .ok()
        .flatten()
        .is_some_and(|h| !h.is_empty())
}

pub fn mcp_url(app: &App) -> String {
    format!("{}/mcp", app.public_url.trim_end_matches('/'))
}

fn revoke(app: &App, login: &str) -> anyhow::Result<()> {
    app.store.set_setting(login, MCP_HASH, "")?;
    app.store.set_setting(login, MCP_AT, "")
}

fn mcp_box(app: &App, login: &str) -> String {
    let at = app
        .store
        .setting(login, MCP_AT)
        .ok()
        .flatten()
        .filter(|_| has_mcp_token(app, login));
    let state = match at {
        Some(at) => format!(
            "<p>You made an MCP token on {}. The server shows a token once and keeps only its hash.</p>\
             <form method=\"post\" action=\"/me/mcp-token\" class=\"d-inline\"><button class=\"btn btn-sm\" type=\"submit\">Replace the token</button></form> \
             <form method=\"post\" action=\"/me/mcp-token/delete\" class=\"d-inline\"><button class=\"btn btn-sm btn-danger\" type=\"submit\">Revoke the token</button></form>",
            esc(at.get(..10).unwrap_or(&at))
        ),
        None => "<form method=\"post\" action=\"/me/mcp-token\"><button class=\"btn btn-sm btn-primary\" type=\"submit\">Make an MCP token</button></form>".into(),
    };
    format!(
        "<div class=\"Box mb-4\" id=\"mcp\"><div class=\"Box-header text-bold\">{} MCP server</div><div class=\"Box-row\">\
         <p>An agent reads this site at <code>{}</code>, with the Streamable HTTP transport. \
         It sends your token as <code>Authorization: Bearer</code>. Every tool only reads; none writes to GitHub.</p>{state}</div></div>",
        icon(octicon!("key")),
        esc(&mcp_url(app)),
    )
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
    if on {
        out.push_str(&mcp_box(&app, &login));
    }
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
    let saved = app
        .store
        .set_setting(&login, OPT_IN, if on { "1" } else { "" })
        .and_then(|_| if on { Ok(()) } else { revoke(&app, &login) });
    if let Err(e) = saved {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    Redirect::to("/me/settings").into_response()
}

async fn new_mcp_token(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    if !opted_in(&app, &login) {
        return Redirect::to("/me/settings").into_response();
    }
    let token = random_id();
    let saved = app
        .store
        .set_setting(&login, MCP_HASH, &token_hash(&token))
        .and_then(|_| {
            app.store
                .set_setting(&login, MCP_AT, &Utc::now().to_rfc3339())
        });
    if let Err(e) = saved {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    let mut out = head(
        &format!("{} Your MCP token", icon(octicon!("key"))),
        "Copy it now. The server keeps only its hash, so this page cannot show it again.",
        "",
    );
    let add = format!(
        "claude mcp add --transport http maintainer-mode {} --header \"Authorization: Bearer {}\"",
        mcp_url(&app),
        token
    );
    let _ = write!(
        out,
        "<div class=\"Box mb-4\"><div class=\"Box-row\">\
         <input class=\"form-control width-full mb-2 text-mono\" readonly value=\"{t}\" aria-label=\"MCP token\">\
         <p>To add the server to Claude Code on your machine:</p>\
         <pre class=\"p-2 color-bg-subtle\" style=\"white-space:pre-wrap\">{a}</pre>\
         <p>Any token you made before no longer works.</p>\
         <a class=\"btn btn-sm\" href=\"/me/settings#mcp\">Back to your settings</a></div></div>",
        t = esc(&token),
        a = esc(&add),
    );
    let Html(body) = page(&app, Some(&login), "", &out);
    ([(header::CACHE_CONTROL, "no-store")], Html(body)).into_response()
}

async fn delete_mcp_token(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    if let Err(e) = revoke(&app, &login) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    Redirect::to("/me/settings#mcp").into_response()
}
