//! The settings page, and the old addresses of /me. Everything under /me is private to the
//! person who signed in.

use axum::Router;
use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;

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

/// Every answer for one person, a redirect or an error too: /me, /inbox,
/// /needs, the private API, the CLI login, and /packages with a session,
/// whose filters are private.
pub async fn no_store(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let private = path == "/me"
        || path.starts_with("/me/")
        || path == crate::notify::INBOX
        || path.starts_with("/needs/")
        || crate::api::is_private(path)
        || path == "/login/cli"
        || path.starts_with("/api/v1/login")
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

pub(crate) const SETTINGS: [(&str, &str); 9] = [
    ("notifications", "Notifications"),
    ("followup", "Pull requests"),
    ("feed", "Feed"),
    ("upload", "Upload"),
    ("api", "API token"),
    ("tags", "Tags"),
    ("goal", "Goal"),
    ("start", "Get started"),
    ("github", "GitHub"),
];

/// What the server does with the GitHub token of a session. Keep it in
/// step with the lent tokens part of docs/data.md.
fn github_box() -> crate::html::Html {
    let body = crate::html::Html::from(
        "<div class=\"Box-row\"><p>When you sign in, the server keeps the GitHub token of your session. \
         It uses the token for one read that helps you: the paths you changed in nixpkgs, when you sign in and then once a day.</p>\
         <p>Shared work, such as the archive of nixpkgs, uses the server's own tokens. \
         When they have less than 1000 points left, your token can pay for shared reads that keep the data fresh: \
         at most a tenth of its budget in each rate limit window. It never pays for the walk back through history.</p>\
         <p class=\"mb-0\">Sign out to delete the token from this server. \
         You can also revoke it in your <a href=\"https://github.com/settings/applications\">GitHub settings</a>.</p></div>",
    );
    crate::html::html!(
        "<div id=\"github\">{}</div>",
        crate::ui::section(
            crate::html::html!(
                "{} Your GitHub sign-in",
                crate::web::icon(crate::web::octicon!("key"))
            ),
            None,
            None,
            body
        )
    )
}

async fn settings_page(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let body = [
        crate::notify::settings_box(&app, &login),
        crate::followup::settings_box(&app, &login),
        crate::catalog::feed_box(&app, &login),
        crate::sets::token_box(&app, &login),
        crate::apitoken::settings_box(&app, &login),
        crate::web::tags_box(&app, &login),
        crate::progress::goal_box(&app, &login),
        crate::welcome::settings_box(&app, &login),
        github_box(),
    ]
    .into_iter()
    .collect::<crate::html::Html>();
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
        let r = settings_page(State(app.clone()), HeaderMap::new()).await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        let h = signed_in(&app, "alice");
        let r = settings_page(State(app), h).await;
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
            "/me/settings/followup",
            "/me/feed",
            "/me/upload-token",
            "/me/api-token",
            "/me/tags",
            "/me/progress",
            "/me/start",
        ] {
            assert!(html.contains(&format!("action=\"{action}\"")), "{action}");
        }
    }
}
