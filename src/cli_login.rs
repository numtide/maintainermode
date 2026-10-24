//! `maintainer-mode login`: a device code sign-in. The tool asks for a
//! code, the person confirms it on the site while signed in, and the tool
//! fetches a new upload token. It works over SSH, with no browser on the
//! machine that runs the tool.

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::ui::{Page, Section};
use crate::web::{App, Shared, esc, octicon, random_id};

const EXPIRES_MINUTES: u64 = 10;
pub const INTERVAL_SECONDS: u64 = 2;
/// Requests need no sign-in, so the table has a cap.
const MAX_PENDING: usize = 1000;
/// No vowels, so a code cannot spell a word; no letters that look like digits.
const ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";

struct Pending {
    user_code: String,
    /// monotonic, so a step of the wall clock cannot expire a code early.
    expires: Instant,
    done: Option<(String, String)>,
}

static PENDING: Mutex<Option<HashMap<String, Pending>>> = Mutex::new(None);

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/v1/login", post(start))
        .route("/api/v1/login/poll", post(poll))
        .route("/login/cli", get(page).post(confirm))
}

fn user_code() -> String {
    use rand::Rng;
    let mut r = rand::thread_rng();
    let mut s: String = (0..8)
        .map(|_| ALPHABET[r.gen_range(0..ALPHABET.len())] as char)
        .collect();
    s.insert(4, '-');
    s
}

fn normalize(code: &str) -> String {
    let c: String = code
        .chars()
        .filter(char::is_ascii_alphabetic)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    match c.len() {
        8 => format!("{}-{}", &c[..4], &c[4..]),
        _ => c,
    }
}

fn refuse(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

async fn start(State(app): State<Shared>) -> Response {
    let now = Instant::now();
    let mut guard = PENDING.lock().unwrap();
    let pending = guard.get_or_insert_with(HashMap::new);
    pending.retain(|_, p| p.expires > now);
    if pending.len() >= MAX_PENDING {
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "too many sign-ins at once, try again later",
        );
    }
    let device = random_id();
    let user = user_code();
    pending.insert(
        device.clone(),
        Pending {
            user_code: user.clone(),
            expires: now + Duration::from_secs(EXPIRES_MINUTES * 60),
            done: None,
        },
    );
    let base = app.public_url.trim_end_matches('/');
    Json(json!({
        "device_code": device,
        "user_code": user,
        "url": format!("{base}/login/cli?code={user}"),
        "interval": INTERVAL_SECONDS,
        "expires_in": EXPIRES_MINUTES * 60,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct Poll {
    device_code: String,
}

/// 202 until the person confirms; the token goes out once.
async fn poll(Json(p): Json<Poll>) -> Response {
    let now = Instant::now();
    let mut guard = PENDING.lock().unwrap();
    let pending = guard.get_or_insert_with(HashMap::new);
    let Some(mut e) = pending.remove(&p.device_code) else {
        return refuse(StatusCode::NOT_FOUND, "unknown or expired code");
    };
    if e.expires <= now {
        return refuse(StatusCode::GONE, "the code expired, run login again");
    }
    match e.done.take() {
        Some((login, token)) => Json(json!({ "login": login, "token": token })).into_response(),
        None => {
            pending.insert(p.device_code, e);
            (StatusCode::ACCEPTED, Json(json!({ "status": "pending" }))).into_response()
        }
    }
}

#[derive(Deserialize)]
struct CodeQuery {
    code: Option<String>,
}

/// `normalize` leaves only letters and a dash, so the code needs no escape.
fn sign_in_href(code: &str) -> String {
    format!("/auth/login?next=%2Flogin%2Fcli%3Fcode%3D{code}")
}

fn render(app: &App, login: &str, body: String) -> Response {
    Page::new(
        Section::Personal(crate::ui::Personal::Settings),
        "Sign in the command line tool",
    )
    .icon(octicon!("key"))
    .body(body)
    .render(app, Some(login))
}

async fn page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<CodeQuery>,
) -> Response {
    let code = normalize(q.code.as_deref().unwrap_or(""));
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to(&sign_in_href(&code)).into_response();
    };
    let named = if code.is_empty() {
        String::new()
    } else {
        format!(
            "<p>The link names the code <code>{}</code>. Type it only if your terminal shows the same code.</p>",
            esc(&code)
        )
    };
    // RFC 8628 §5.4: the person types the code, so a link alone cannot
    // hand the token to the person who sent it.
    let body = format!(
        "<div class=\"Box\"><div class=\"Box-row\">\
         <p>A <code>maintainer-mode login</code> asks for an upload token for <strong>{who}</strong>. \
         Confirm only a code that your own terminal shows now. A person who sent you this link gets the token.</p>{named}\
         <form method=\"post\" action=\"/login/cli\">\
         <label for=\"code\" class=\"d-block mb-1\">The code in your terminal</label>\
         <input id=\"code\" name=\"code\" class=\"form-control text-mono mb-3\" autocomplete=\"off\" required>\
         <p class=\"color-fg-muted\">The token can only send your package sets. Any upload token you made before stops working.</p>\
         <button class=\"btn btn-primary\" type=\"submit\">Confirm</button> \
         <a class=\"btn\" href=\"/\">Cancel</a></form></div></div>",
        who = esc(&login),
    );
    render(&app, &login, body)
}

#[derive(Deserialize)]
struct CodeForm {
    code: String,
}

async fn confirm(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<CodeForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to(&sign_in_href(&normalize(&f.code))).into_response();
    };
    let code = normalize(&f.code);
    let now = Instant::now();
    let token = random_id();
    {
        let mut guard = PENDING.lock().unwrap();
        let pending = guard.get_or_insert_with(HashMap::new);
        let Some(p) = pending
            .values_mut()
            .find(|p| p.user_code == code && p.done.is_none() && p.expires > now)
        else {
            return (
                StatusCode::NOT_FOUND,
                render(
                    &app,
                    &login,
                    "<div class=\"flash flash-error\">No sign-in waits for this code. It can have expired after 10 minutes. Run <code>maintainer-mode login</code> again.</div>".into(),
                ),
            )
                .into_response();
        };
        // The tool must never get a token that the store did not keep.
        if let Err(e) = app
            .store
            .set_upload_token(&login, &crate::sets::token_hash(&token))
        {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
        p.done = Some((login.clone(), token));
    }
    render(
        &app,
        &login,
        "<div class=\"flash flash-success\">Done. Go back to your terminal. \
         <a href=\"/me/settings#upload\">Your settings</a> can revoke the token.</div>"
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_reads_the_same_with_any_case_or_dash() {
        let c = user_code();
        assert_eq!(c.len(), 9);
        assert_eq!(normalize(&c.to_lowercase().replace('-', " ")), c);
    }

    #[tokio::test]
    async fn a_confirmed_code_gives_the_token_once() {
        let app = crate::sets::tests::app(&[]);
        let r = start(State(app.clone())).await;
        let v: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(r.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        let device = v["device_code"].as_str().unwrap().to_string();
        let user = v["user_code"].as_str().unwrap().to_lowercase();
        let ask = || {
            poll(Json(Poll {
                device_code: device.clone(),
            }))
        };
        assert_eq!(ask().await.status(), StatusCode::ACCEPTED);
        let h = crate::notify::tests::signed_in(&app, "alice");
        let r = confirm(State(app.clone()), h, Form(CodeForm { code: user })).await;
        assert_eq!(r.status(), StatusCode::OK);
        let r = ask().await;
        assert_eq!(r.status(), StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(r.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        let token = v["token"].as_str().unwrap();
        assert_eq!(
            app.store
                .upload_login(&crate::sets::token_hash(token))
                .unwrap()
                .as_deref(),
            Some("alice")
        );
        assert_eq!(ask().await.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_link_never_fills_in_the_code() {
        let app = crate::sets::tests::app(&[]);
        let h = crate::notify::tests::signed_in(&app, "alice");
        let r = page(
            State(app.clone()),
            h,
            Query(CodeQuery {
                code: Some("bcdf-ghjk".into()),
            }),
        )
        .await;
        let html = String::from_utf8(
            axum::body::to_bytes(r.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(
            html.contains("names the code <code>BCDF-GHJK</code>"),
            "{html}"
        );
        assert!(!html.contains("value=\"BCDF-GHJK\""));
    }
}
