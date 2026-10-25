//! The settings page, and the old addresses of /me. Everything under /me is private to the
//! person who signed in.

use axum::Router;
use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;

use crate::html::{Html, html};
use crate::ui::{Page, Personal, Section, Tab, section};
use crate::web::{App, Shared, icon, octicon};

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

/// A section: its id, its label and what it shows.
type Row = (&'static str, &'static str, fn(&App, &str) -> Html);

/// The boxes of `/me/settings` and the sections in each. Other pages,
/// `PAGES` in api.rs, nix/test.nix and the redirects after each form link
/// to the section ids.
const GROUPS: [(&str, &str, &str, &[Row]); 4] = [
    (
        "news",
        "News",
        octicon!("bell"),
        &[
            ("notifications", "Unread count", crate::notify::settings_box),
            ("feed", "Atom feed", crate::catalog::feed_box),
            ("followup", "Follow-up plan", crate::followup::settings_box),
        ],
    ),
    (
        "reviews",
        "Reviews",
        octicon!("eye"),
        &[
            ("tags", "Tags", crate::web::tags_box),
            ("goal", "Weekly goal", crate::progress::goal_box),
        ],
    ),
    (
        "machines",
        "Your machines",
        octicon!("device-desktop"),
        &[("upload", "Upload", crate::sets::token_box)],
    ),
    (
        "account",
        "Account",
        octicon!("person"),
        &[
            ("api", "API token", crate::apitoken::settings_box),
            ("github", "GitHub sign-in", github_box),
            ("start", "Get started", crate::welcome::settings_box),
        ],
    ),
];

/// The id of each section of `/me/settings`.
#[cfg(test)]
pub(crate) fn sections() -> impl Iterator<Item = &'static str> {
    GROUPS
        .iter()
        .flat_map(|(_, _, _, list)| list.iter().map(|(id, _, _)| *id))
}

/// What the server does with the GitHub token of a session. Keep it in
/// step with the lent tokens part of docs/data.md.
fn github_box(_: &App, _: &str) -> Html {
    Html::from(
        "<p>The server keeps the GitHub token of your session. Sign out to delete it from this server. \
         You can also revoke it in your <a href=\"https://github.com/settings/applications\">GitHub settings</a>.</p>\
         <details class=\"fold\"><summary>What the server reads with it</summary>\
         <p>It reads one thing for you: the paths you changed in nixpkgs, when you sign in and then once a day.</p>\
         <p>Shared work, such as the archive of nixpkgs, uses the server's own tokens. \
         When they have less than 1000 points left, your token can pay for shared reads that keep the data fresh: \
         at most a tenth of its budget in each rate limit window. It never pays for the walk back through history.</p></details>",
    )
}

async fn settings_page(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let body: Html = GROUPS
        .iter()
        .map(|(group, title, symbol, list)| {
            let rows: Html = list
                .iter()
                .map(|(id, label, render)| {
                    html!(
                        "<div class=\"Box-row setting\" id=\"{id}\"><h3 class=\"setting-label\">{label}</h3>\
                         <div class=\"setting-body\">{}</div></div>",
                        render(&app, &login)
                    )
                })
                .collect();
            html!(
                "<div id=\"{group}\">{}</div>",
                section(html!("{} {title}", icon(symbol)), None, None, rows)
            )
        })
        .collect();
    let jump = GROUPS
        .iter()
        .map(|(id, title, _, _)| Tab::new(format!("#{id}"), *title, false))
        .collect();
    Page::new(Section::Personal(Personal::Settings), "Settings")
        .icon(octicon!("gear"))
        .sub("Only you see this page.")
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
        for (id, ..) in GROUPS {
            assert!(html.contains(&format!("<div id=\"{id}\">")), "#{id}");
            assert!(html.contains(&format!("href=\"#{id}\"")), "link to #{id}");
        }
        assert_eq!(sections().count(), 9);
        for id in sections() {
            assert_eq!(
                html.matches(&format!("<div class=\"Box-row setting\" id=\"{id}\">"))
                    .count(),
                1,
                "#{id}"
            );
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
