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

use crate::ui::{Page, Personal, Section, Tab};
use crate::web::{Shared, octicon};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/me", get(me_page))
        .route("/me/settings", get(settings_page))
        .route(
            "/watch",
            get(|| async { Redirect::permanent("/me?tab=watch") }),
        )
        .route(
            "/me/inbox",
            get(|| async { Redirect::permanent(crate::notify::INBOX) }),
        )
        .route(
            "/me/sets",
            get(|| async { Redirect::permanent("/me?tab=machines") }),
        )
}

/// Every answer under /me and /inbox, a redirect or an error too, is for
/// one person.
pub async fn no_store(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let private = path == "/me" || path.starts_with("/me/") || path == crate::notify::INBOX;
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

/// What one tab of `/me` adds to the page. `sub`, `actions` and `body` are
/// HTML.
pub(crate) struct TabBody {
    pub sub: String,
    pub actions: String,
    pub body: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MeTab {
    Maintain,
    Watch,
    Machines,
}

impl MeTab {
    const ALL: [MeTab; 3] = [MeTab::Maintain, MeTab::Watch, MeTab::Machines];

    fn parse(s: Option<&str>) -> Option<MeTab> {
        match s {
            Some("maintain") => Some(MeTab::Maintain),
            Some("watch") => Some(MeTab::Watch),
            Some("machines") => Some(MeTab::Machines),
            _ => None,
        }
    }

    fn key(self) -> &'static str {
        match self {
            MeTab::Maintain => "maintain",
            MeTab::Watch => "watch",
            MeTab::Machines => "machines",
        }
    }

    fn label(self) -> &'static str {
        match self {
            MeTab::Maintain => "Maintain",
            MeTab::Watch => "Watch",
            MeTab::Machines => "My machines",
        }
    }
}

#[derive(Deserialize)]
struct MeQuery {
    tab: Option<String>,
}

async fn me_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<MeQuery>,
    uri: axum::http::Uri,
) -> Response {
    let Some(tab) = MeTab::parse(q.tab.as_deref()) else {
        let rest: Vec<&str> = uri
            .query()
            .unwrap_or_default()
            .split('&')
            .filter(|p| p.starts_with("sub=") || p.starts_with("kind="))
            .collect();
        let to = if rest.is_empty() {
            crate::notify::INBOX.to_string()
        } else {
            format!("{}?{}", crate::notify::INBOX, rest.join("&"))
        };
        return Redirect::permanent(&to).into_response();
    };
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let t = match tab {
        MeTab::Maintain => crate::catalog::maintain_tab(&app, &login),
        MeTab::Watch => crate::catalog::watch_tab(&app, &login),
        MeTab::Machines => crate::sets::machines_tab(&app, &login),
    };
    let tabs = MeTab::ALL
        .iter()
        .map(|&m| Tab::new(format!("/me?tab={}", m.key()), m.label(), m == tab))
        .collect();
    Page::new(Section::Home, format!("My packages: {}", tab.label()))
        .heading("My packages")
        .icon(octicon!("package"))
        .sub(format!("{} Only you see this page.", t.sub))
        .subnav(tabs)
        .actions(t.actions)
        .body(t.body)
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

    fn q(tab: &str) -> Query<MeQuery> {
        Query(MeQuery {
            tab: Some(tab.into()),
        })
    }

    fn uri(s: &str) -> axum::http::Uri {
        s.parse().unwrap()
    }

    #[tokio::test]
    async fn me_has_three_tabs_for_its_owner_only() {
        let app = crate::sets::tests::app(&["hello"]);
        let r = me_page(
            State(app.clone()),
            HeaderMap::new(),
            q("watch"),
            uri("/me?tab=watch"),
        )
        .await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        let r = me_page(
            State(app.clone()),
            HeaderMap::new(),
            q("inbox"),
            uri("/me?tab=inbox&kind=pr"),
        )
        .await;
        assert_eq!(r.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(r.headers()[header::LOCATION], "/inbox?kind=pr");
        let h = signed_in(&app, "alice");
        app.store.watch("alice", "pkg:hello").unwrap();
        for (tab, want) in [
            ("maintain", "/u/alice/maintains"),
            ("watch", "action=\"/me/watch/remove\""),
            ("machines", "No report yet."),
        ] {
            let r = me_page(State(app.clone()), h.clone(), q(tab), uri("/me")).await;
            assert_eq!(r.status(), StatusCode::OK, "{tab}");
            let html = body(r).await;
            assert!(html.contains(want), "{tab}: {want}");
            for m in MeTab::ALL {
                assert!(html.contains(&format!("href=\"/me?tab={}\"", m.key())));
            }
            assert!(
                html.contains(&format!("href=\"/me?tab={tab}\" aria-current=\"page\"")),
                "{tab}"
            );
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
