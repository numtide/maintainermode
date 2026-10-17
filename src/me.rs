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

use crate::ui::{Page, Section, Tab};
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
            get(|| async { Redirect::permanent("/me?tab=inbox") }),
        )
        .route(
            "/me/sets",
            get(|| async { Redirect::permanent("/me?tab=machines") }),
        )
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

/// What one tab of `/me` adds to the page. `sub`, `actions` and `body` are
/// HTML.
pub(crate) struct TabBody {
    pub sub: String,
    pub actions: String,
    pub body: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MeTab {
    Inbox,
    Maintain,
    Watch,
    Machines,
}

impl MeTab {
    const ALL: [MeTab; 4] = [MeTab::Inbox, MeTab::Maintain, MeTab::Watch, MeTab::Machines];

    fn parse(s: Option<&str>) -> MeTab {
        match s {
            Some("maintain") => MeTab::Maintain,
            Some("watch") => MeTab::Watch,
            Some("machines") => MeTab::Machines,
            _ => MeTab::Inbox,
        }
    }

    fn key(self) -> &'static str {
        match self {
            MeTab::Inbox => "inbox",
            MeTab::Maintain => "maintain",
            MeTab::Watch => "watch",
            MeTab::Machines => "machines",
        }
    }

    fn label(self) -> &'static str {
        match self {
            MeTab::Inbox => "Inbox",
            MeTab::Maintain => "Maintain",
            MeTab::Watch => "Watch",
            MeTab::Machines => "My machines",
        }
    }
}

#[derive(Deserialize)]
struct MeQuery {
    tab: Option<String>,
    sub: Option<String>,
    kind: Option<String>,
}

async fn me_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<MeQuery>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let tab = MeTab::parse(q.tab.as_deref());
    let t = match tab {
        MeTab::Inbox => crate::notify::inbox_tab(&app, &login, q.sub.as_deref(), q.kind.as_deref()),
        MeTab::Maintain => crate::catalog::maintain_tab(&app, &login),
        MeTab::Watch => crate::catalog::watch_tab(&app, &login),
        MeTab::Machines => crate::sets::machines_tab(&app, &login),
    };
    let unread = app.inbox.get(&login);
    let tabs = MeTab::ALL
        .iter()
        .map(|&m| {
            let t = Tab::new(format!("/me?tab={}", m.key()), m.label(), m == tab);
            match m {
                MeTab::Inbox if unread > 0 => t.count(unread),
                _ => t,
            }
        })
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
            sub: None,
            kind: None,
        })
    }

    #[tokio::test]
    async fn me_has_four_tabs_for_its_owner_only() {
        let app = crate::sets::tests::app(&["hello"]);
        let r = me_page(State(app.clone()), HeaderMap::new(), q("inbox")).await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        let h = signed_in(&app, "alice");
        app.store.watch("alice", "pkg:hello").unwrap();
        for (tab, want) in [
            ("inbox", "Nothing here."),
            ("maintain", "/u/alice/maintains"),
            ("watch", "action=\"/me/watch/remove\""),
            ("machines", "No report yet."),
            ("nonsense", "Nothing here."),
        ] {
            let r = me_page(State(app.clone()), h.clone(), q(tab)).await;
            assert_eq!(r.status(), StatusCode::OK, "{tab}");
            let html = body(r).await;
            assert!(html.contains(want), "{tab}: {want}");
            for m in MeTab::ALL {
                assert!(html.contains(&format!("href=\"/me?tab={}\"", m.key())));
            }
            let key = MeTab::parse(Some(tab)).key();
            assert!(
                html.contains(&format!("href=\"/me?tab={key}\" aria-current=\"page\"")),
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
