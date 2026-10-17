//! Notifications: the inbox at `/me/inbox` and its unread count in the
//! header. The events are those of the private Atom feed. Only the owner
//! sees their inbox and their choices.

use axum::Router;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;
use std::sync::RwLock;

use crate::catalog::{Event, head, person_events};
use crate::github::urlencode;
use crate::web::{App, Shared, ago, esc, icon, octicon, page};

const READ_AT: &str = "inbox_read_at";
const KINDS: &str = "notify_kinds";

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/me/inbox", get(inbox_page))
        .route("/me/inbox/read", post(mark_read))
        .route("/me/notify", post(save_kinds))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    PullRequest,
    Issue,
    Advisory,
    Release,
}

impl Kind {
    pub const ALL: [Kind; 4] = [
        Kind::PullRequest,
        Kind::Issue,
        Kind::Advisory,
        Kind::Release,
    ];

    /// From the id that `catalog::events` and `catalog::set_events` give.
    pub fn of(id: &str) -> Kind {
        match id.split_once('-').map_or(id, |(k, _)| k) {
            "issue" => Kind::Issue,
            "advisory" => Kind::Advisory,
            "upstream" => Kind::Release,
            _ => Kind::PullRequest,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Kind::PullRequest => "pr",
            Kind::Issue => "issue",
            Kind::Advisory => "advisory",
            Kind::Release => "release",
        }
    }

    fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.key() == s)
    }

    fn label(self) -> &'static str {
        match self {
            Kind::PullRequest => "Pull requests",
            Kind::Issue => "Issues",
            Kind::Advisory => "Advisories",
            Kind::Release => "Releases",
        }
    }

    fn icon(self) -> String {
        match self {
            Kind::PullRequest => icon(octicon!("git-pull-request")),
            Kind::Issue => icon(octicon!("issue-opened")),
            Kind::Advisory => icon(octicon!("shield")),
            Kind::Release => icon(octicon!("tag")),
        }
    }
}

/// The kinds a person counts in the header and gets by email. All of them
/// until the person chooses.
pub fn kinds(app: &App, login: &str) -> Vec<Kind> {
    match app.store.setting(login, KINDS).ok().flatten() {
        None => Kind::ALL.to_vec(),
        Some(v) => v.split(',').filter_map(Kind::parse).collect(),
    }
}

pub(crate) fn read_at(app: &App, login: &str) -> Option<DateTime<Utc>> {
    app.store
        .setting(login, READ_AT)
        .ok()
        .flatten()
        .and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn unread(e: &Event, read_at: Option<DateTime<Utc>>) -> bool {
    read_at.is_none_or(|r| e.at > r)
}

pub fn unread_count(events: &[Event], read_at: Option<DateTime<Utc>>, kinds: &[Kind]) -> usize {
    events
        .iter()
        .filter(|e| unread(e, read_at) && kinds.contains(&Kind::of(&e.id)))
        .count()
}

/// Unread counts per login, for the header. The background pass fills it
/// after each recompute; a page view only reads it.
#[derive(Default)]
pub struct Counts(RwLock<HashMap<String, usize>>);

impl Counts {
    pub fn get(&self, login: &str) -> usize {
        self.0.read().unwrap().get(login).copied().unwrap_or(0)
    }

    fn set(&self, login: &str, n: usize) {
        self.0.write().unwrap().insert(login.to_string(), n);
    }
}

pub fn recount_one(app: &App, login: &str) {
    let n = unread_count(
        &person_events(app, login),
        read_at(app, login),
        &kinds(app, login),
    );
    app.inbox.set(login, n);
}

/// Everyone who signed in once; a login with nothing watched costs a few
/// small reads.
pub fn recount(app: &App) {
    for m in app.store.members().unwrap_or_default() {
        recount_one(app, &m.login);
    }
}

#[derive(Deserialize)]
struct InboxQuery {
    sub: Option<String>,
    kind: Option<String>,
}

fn inbox_href(sub: Option<&str>, kind: Option<Kind>) -> String {
    let mut q = Vec::new();
    if let Some(s) = sub {
        q.push(format!("sub={}", urlencode(s)));
    }
    if let Some(k) = kind {
        q.push(format!("kind={}", k.key()));
    }
    if q.is_empty() {
        "/me/inbox".into()
    } else {
        format!("/me/inbox?{}", q.join("&"))
    }
}

async fn inbox_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<InboxQuery>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let all = person_events(&app, &login);
    let read = read_at(&app, &login);
    let n = unread_count(&all, read, &kinds(&app, &login));
    app.inbox.set(&login, n);
    let sub = q.sub.as_deref().filter(|s| !s.is_empty());
    let kind = q.kind.as_deref().and_then(Kind::parse);
    let any_unread = all.iter().any(|e| unread(e, read));
    let mut out = head(
        &format!("{} Inbox", icon(octicon!("inbox"))),
        "What happened in the last 30 days to what you watch and to your package set, newest first. \
         These are the events of your Atom feed. Only you see this page.",
        if any_unread {
            "<form method=\"post\" action=\"/me/inbox/read\"><button class=\"btn btn-sm\" type=\"submit\">Mark all read</button></form>"
        } else {
            ""
        },
    );
    out.push_str("<div class=\"d-flex flex-wrap flex-items-center mb-3\" style=\"gap:8px\">");
    let sel = |on: bool| if on { " selected" } else { "" };
    let _ = write!(
        out,
        "<a class=\"SegmentedControl-item btn btn-sm{}\" href=\"{}\">All</a>",
        sel(kind.is_none()),
        esc(&inbox_href(sub, None))
    );
    for k in Kind::ALL {
        let _ = write!(
            out,
            "<a class=\"SegmentedControl-item btn btn-sm{}\" href=\"{}\">{} {}</a>",
            sel(kind == Some(k)),
            esc(&inbox_href(sub, Some(k))),
            k.icon(),
            k.label()
        );
    }
    let subjects: BTreeSet<&str> = all.iter().map(|e| e.subject.as_str()).collect();
    if !subjects.is_empty() {
        let _ = write!(
            out,
            "<form method=\"get\" action=\"/me/inbox\" class=\"d-flex\" style=\"gap:8px\">{}\
             <select class=\"form-select\" name=\"sub\" aria-label=\"subscription\"><option value=\"\">Every subscription</option>",
            kind.map(|k| format!(
                "<input type=\"hidden\" name=\"kind\" value=\"{}\">",
                k.key()
            ))
            .unwrap_or_default()
        );
        for s in &subjects {
            let _ = write!(
                out,
                "<option value=\"{v}\"{}>{v}</option>",
                if sub == Some(*s) { " selected" } else { "" },
                v = esc(s)
            );
        }
        out.push_str(
            "</select><button class=\"btn btn-sm\" type=\"submit\">Filter</button></form>",
        );
    }
    out.push_str("</div>");
    let shown: Vec<&Event> = all
        .iter()
        .filter(|e| sub.is_none_or(|s| e.subject == s))
        .filter(|e| kind.is_none_or(|k| Kind::of(&e.id) == k))
        .collect();
    if shown.is_empty() {
        out.push_str(
            "<div class=\"blankslate\"><p>Nothing here. Subscribe to packages on <a href=\"/watch\">/watch</a>, \
             or send your package set on <a href=\"/me/sets\">/me/sets</a>.</p></div>",
        );
    } else {
        out.push_str("<div class=\"Box\">");
        for e in shown {
            let new = unread(e, read);
            let _ = write!(
                out,
                "<div class=\"Box-row d-flex flex-items-start\"><span class=\"mr-2 color-fg-muted\">{}</span>\
                 <div class=\"flex-auto\"><a class=\"Link--primary{}\" href=\"{}\">{}</a>\
                 <div class=\"f6 color-fg-muted mt-1\">{} · {} ago</div></div>{}</div>",
                Kind::of(&e.id).icon(),
                if new { " text-bold" } else { "" },
                esc(&e.url),
                esc(&e.title),
                esc(&e.subject),
                ago(e.at),
                if new {
                    "<span class=\"Label Label--accent ml-2\">new</span>"
                } else {
                    ""
                }
            );
        }
        out.push_str("</div>");
    }
    page(&app, Some(&login), "", &out).into_response()
}

async fn mark_read(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    if let Err(e) = app
        .store
        .set_setting(&login, READ_AT, &Utc::now().to_rfc3339())
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    app.inbox.set(&login, 0);
    Redirect::to("/me/inbox").into_response()
}

/// The section of `/me/settings`.
pub fn settings_box(app: &App, login: &str) -> String {
    let on = kinds(app, login);
    let mut boxes = String::new();
    for k in Kind::ALL {
        let _ = write!(
            boxes,
            "<div class=\"form-checkbox my-1\"><label><input type=\"checkbox\" name=\"k_{key}\" value=\"1\"{}> {} {}</label></div>",
            if on.contains(&k) { " checked" } else { "" },
            k.icon(),
            k.label(),
            key = k.key()
        );
    }
    format!(
        "<div class=\"Box mb-4\" id=\"notify\"><div class=\"Box-header text-bold\">{} Notifications</div><div class=\"Box-row\">\
         <form method=\"post\" action=\"/me/notify\">\
         <p>Your <a href=\"/me/inbox\">inbox</a> shows every event. The unread count in the header counts only the kinds you choose here.</p>\
         {boxes}<button class=\"btn btn-sm mt-2\" type=\"submit\">Save</button></form></div></div>",
        icon(octicon!("bell")),
    )
}

#[derive(Deserialize)]
struct KindsForm {
    #[serde(default)]
    k_pr: Option<String>,
    #[serde(default)]
    k_issue: Option<String>,
    #[serde(default)]
    k_advisory: Option<String>,
    #[serde(default)]
    k_release: Option<String>,
}

impl KindsForm {
    fn chosen(&self) -> Vec<Kind> {
        [
            (Kind::PullRequest, &self.k_pr),
            (Kind::Issue, &self.k_issue),
            (Kind::Advisory, &self.k_advisory),
            (Kind::Release, &self.k_release),
        ]
        .into_iter()
        .filter(|(_, v)| v.as_deref() == Some("1"))
        .map(|(k, _)| k)
        .collect()
    }
}

async fn save_kinds(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<KindsForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let keys: Vec<&str> = f.chosen().into_iter().map(Kind::key).collect();
    if let Err(e) = app.store.set_setting(&login, KINDS, &keys.join(",")) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    recount_one(&app, &login);
    Redirect::to("/me/settings#notify").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn event(id: &str, at: DateTime<Utc>) -> Event {
        Event {
            at,
            id: id.into(),
            title: String::new(),
            url: String::new(),
            subject: "hello".into(),
        }
    }

    #[test]
    fn the_kind_comes_from_the_event_id() {
        assert_eq!(Kind::of("pr-1-merged"), Kind::PullRequest);
        assert_eq!(Kind::of("issue-2-issue opened"), Kind::Issue);
        assert_eq!(Kind::of("advisory-NIXPKGS-1-hello"), Kind::Advisory);
        assert_eq!(Kind::of("upstream-hello-2.13"), Kind::Release);
        for k in Kind::ALL {
            assert_eq!(Kind::parse(k.key()), Some(k));
        }
    }

    #[test]
    fn the_unread_count_starts_after_the_read_time_and_keeps_the_chosen_kinds() {
        let now = Utc::now();
        let list = [
            event("pr-1-merged", now - Duration::hours(1)),
            event("issue-2-issue opened", now - Duration::hours(2)),
            event("upstream-hello-2", now - Duration::hours(3)),
            event("pr-3-pull request opened", now - Duration::days(2)),
        ];
        assert_eq!(unread_count(&list, None, &Kind::ALL), 4);
        let read = Some(now - Duration::days(1));
        assert_eq!(unread_count(&list, read, &Kind::ALL), 3);
        assert_eq!(unread_count(&list, read, &[Kind::PullRequest]), 1);
        assert_eq!(unread_count(&list, read, &[]), 0);
        assert_eq!(unread_count(&list, Some(now), &Kind::ALL), 0);
    }

    pub(crate) fn signed_in(app: &Shared, login: &str) -> HeaderMap {
        app.store.create_session("s", login, "t").unwrap();
        let mut h = HeaderMap::new();
        h.insert(axum::http::header::COOKIE, "session=s".parse().unwrap());
        h
    }

    #[tokio::test]
    async fn the_inbox_is_for_its_owner_and_marks_all_read() {
        let app = crate::sets::tests::app(&[]);
        let r = inbox_page(
            State(app.clone()),
            HeaderMap::new(),
            Query(InboxQuery {
                sub: None,
                kind: None,
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        let h = signed_in(&app, "alice");
        let r = inbox_page(
            State(app.clone()),
            h.clone(),
            Query(InboxQuery {
                sub: None,
                kind: Some("advisory".into()),
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        app.inbox.set("alice", 3);
        assert_eq!(app.inbox.get("alice"), 3);
        let r = mark_read(State(app.clone()), h).await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        assert_eq!(app.inbox.get("alice"), 0);
        assert!(read_at(&app, "alice").is_some());
    }

    #[test]
    fn a_filter_link_keeps_the_other_filter() {
        assert_eq!(inbox_href(None, None), "/me/inbox");
        assert_eq!(
            inbox_href(Some("pkgs/by-name/"), Some(Kind::Advisory)),
            "/me/inbox?sub=pkgs%2Fby-name%2F&kind=advisory"
        );
    }
}
