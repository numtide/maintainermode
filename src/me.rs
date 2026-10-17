//! The settings page, and the old addresses of /me. Everything under /me is private to the
//! person who signed in.

use axum::Router;
use axum::extract::{Query, Request, State};
use axum::http::HeaderMap;
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use serde::Deserialize;

use crate::ui::{Page, Personal, Section, Tab};
use crate::web::{Shared, octicon};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/me", get(old_me))
        .route("/me/settings", get(settings_page))
        .route(
            "/watch",
            get(|| async { Redirect::permanent("/packages?filter=watch") }),
        )
        .route(
            "/me/inbox",
            get(|| async { Redirect::permanent(crate::notify::INBOX) }),
        )
        .route(
            "/me/sets",
            get(|| async { Redirect::permanent(crate::mine::MACHINES) }),
        )
}

/// Every answer under /me and /inbox, a redirect or an error too, is for
/// one person. So is /packages with a session: its filters are private.
pub async fn no_store(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let private = path == "/me"
        || path.starts_with("/me/")
        || path == crate::notify::INBOX
        || (path == "/packages" && crate::web::cookie(req.headers(), "session").is_some());
    let mut res = next.run(req).await;
    if private {
        res.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
    }
    res
}

const SETTINGS: [(&str, &str); 9] = [
    ("email", "Email"),
    ("feed", "Feed"),
    ("upload", "Upload"),
    ("llm", "LLMs"),
    ("mcp", "MCP"),
    ("workflows", "Workflows"),
    ("tags", "Tags"),
    ("goal", "Goal"),
    ("start", "Get started"),
];

#[derive(Deserialize)]
struct SettingsQuery {
    mail: Option<String>,
    /// GitHub adds it when it sends the person back after an App install.
    setup_action: Option<String>,
}

async fn settings_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<SettingsQuery>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    if q.setup_action.is_some() {
        crate::actions::forget_check(&app, &login);
    }
    let workflows = crate::actions::repos_box(&app, &login).await;
    let body = [
        crate::notify::settings_box(&app, &login, q.mail.as_deref()),
        crate::catalog::feed_box(&app, &login),
        crate::sets::token_box(&app, &login),
        crate::llm::llm_box(&app, &login),
        crate::llm::mcp_box(&app, &login),
        workflows,
        crate::web::tags_box(&app, &login),
        crate::progress::goal_box(&app, &login),
        crate::welcome::settings_box(&app, &login),
    ]
    .concat();
    let jump = SETTINGS
        .iter()
        .map(|(id, label)| Tab::new(format!("#{id}"), *label, false))
        .collect();
    Page::new(Section::Personal(Personal::Settings), "Settings")
        .icon(octicon!("gear"))
        .sub("Choices and tokens that apply to you only. Only you see this page.")
        .subnav(jump)
        .body(body)
        .render(&app, Some(&login))
}

/// `/me` and its old tabs: the inbox has its own page, the rest are
/// filters of `/packages`.
async fn old_me(uri: axum::http::Uri) -> Redirect {
    let query = uri.query().unwrap_or_default();
    let tab = query
        .split('&')
        .find_map(|p| p.strip_prefix("tab="))
        .unwrap_or_default();
    let to = match tab {
        "inbox" => {
            let keep: Vec<&str> = query
                .split('&')
                .filter(|p| p.starts_with("sub=") || p.starts_with("kind="))
                .collect();
            if keep.is_empty() {
                crate::notify::INBOX.to_string()
            } else {
                format!("{}?{}", crate::notify::INBOX, keep.join("&"))
            }
        }
        "maintain" | "watch" | "machines" => format!("/packages?filter={tab}"),
        _ => "/packages?filter=mine".to_string(),
    };
    Redirect::permanent(&to)
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
    async fn me_and_its_old_tabs_redirect() {
        for (from, to) in [
            ("/me", "/packages?filter=mine"),
            ("/me?tab=inbox", "/inbox"),
            (
                "/me?tab=inbox&sub=hello&kind=pr",
                "/inbox?sub=hello&kind=pr",
            ),
            ("/me?tab=maintain", "/packages?filter=maintain"),
            ("/me?tab=watch", "/packages?filter=watch"),
            ("/me?tab=machines", "/packages?filter=machines"),
            ("/me?tab=nonsense", "/packages?filter=mine"),
        ] {
            let r = old_me(from.parse().unwrap()).await.into_response();
            assert_eq!(r.status(), StatusCode::PERMANENT_REDIRECT, "{from}");
            assert_eq!(r.headers()[header::LOCATION], to, "{from}");
        }
    }

    #[tokio::test]
    async fn one_settings_page_holds_every_section() {
        let app = crate::sets::tests::app(&[]);
        let r = settings_page(
            State(app.clone()),
            HeaderMap::new(),
            Query(SettingsQuery {
                mail: None,
                setup_action: None,
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        let h = signed_in(&app, "alice");
        crate::actions::store_check(
            &app,
            "alice",
            &crate::workflows::Check {
                at: chrono::Utc::now(),
                lines: vec![],
            },
        );
        let r = settings_page(
            State(app),
            h,
            Query(SettingsQuery {
                mail: None,
                setup_action: None,
            }),
        )
        .await;
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
            "/me/settings/workflows/check",
            "/me/tags",
            "/me/progress",
            "/me/start",
        ] {
            assert!(html.contains(&format!("action=\"{action}\"")), "{action}");
        }
    }
}
