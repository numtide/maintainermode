//! The personal menu points here. Everything under /me is private to the
//! person who signed in.

use axum::Router;
use axum::extract::{Query, Request, State};
use axum::http::HeaderMap;
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use serde::Deserialize;

use crate::ui::{Page, Section, Tab, section};
use crate::web::{Shared, esc, octicon};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/me", get(me_page))
        .route("/me/runs", get(runs_page))
        .route("/me/settings", get(settings_page))
}

/// Every answer under /me, a redirect or an error too, is for one person.
pub async fn no_store(req: Request, next: Next) -> Response {
    let private = req.uri().path() == "/me" || req.uri().path().starts_with("/me/");
    let mut res = next.run(req).await;
    if private {
        res.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
    }
    res
}

const SETTINGS: [(&str, &str); 8] = [
    ("email", "Email"),
    ("feed", "Feed"),
    ("upload", "Upload"),
    ("llm", "LLMs"),
    ("mcp", "MCP"),
    ("workflows", "Workflows"),
    ("tags", "Tags"),
    ("goal", "Goal"),
];

#[derive(Deserialize)]
struct SettingsQuery {
    mail: Option<String>,
}

async fn settings_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<SettingsQuery>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let body = [
        crate::notify::settings_box(&app, &login, q.mail.as_deref()),
        crate::catalog::feed_box(&app, &login),
        crate::sets::token_box(&app, &login),
        crate::llm::llm_box(&app, &login),
        crate::llm::mcp_box(&app, &login),
        crate::actions::repos_box(&app, &login),
        crate::web::tags_box(&app, &login),
        crate::progress::goal_box(&app, &login),
    ]
    .concat();
    let jump = SETTINGS
        .iter()
        .map(|(id, label)| Tab::new(format!("#{id}"), *label, false))
        .collect();
    Page::new(Section::Home, "Settings")
        .icon(octicon!("gear"))
        .sub("Choices and tokens that apply to you only. Only you see this page.")
        .subnav(jump)
        .body(body)
        .render(&app, Some(&login))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::tests::signed_in;
    use axum::http::StatusCode;

    async fn body(r: Response) -> String {
        let b = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(b.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn one_settings_page_holds_every_section() {
        let app = crate::sets::tests::app(&[]);
        let r = settings_page(
            State(app.clone()),
            HeaderMap::new(),
            Query(SettingsQuery { mail: None }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        let h = signed_in(&app, "alice");
        let r = settings_page(State(app), h, Query(SettingsQuery { mail: None })).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            r.headers().get(header::CACHE_CONTROL).unwrap(),
            "private, no-store"
        );
        let html = body(r).await;
        for (id, _) in SETTINGS {
            assert!(html.contains(&format!("<div id=\"{id}\">")), "#{id}");
            assert!(html.contains(&format!("href=\"#{id}\"")), "link to #{id}");
        }
        for action in [
            "/me/notify",
            "/me/feed",
            "/me/upload-token",
            "/me/settings/llm",
            "/me/settings/workflows",
            "/me/tags",
            "/me/progress",
        ] {
            assert!(html.contains(&format!("action=\"{action}\"")), "{action}");
        }
    }
}
