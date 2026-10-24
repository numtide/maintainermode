//! Notifications: the inbox at `/inbox` and its unread count in the
//! header. The events are those of the private Atom feed. Only the owner
//! sees their inbox and their choices.

use axum::Router;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::sync::RwLock;

use crate::catalog::{Event, person_events};
use crate::github::urlencode;
use crate::html::{Html, html};
use crate::ui::{Page, Personal, Section, Tab};
use crate::web::{App, Shared, ago, icon, octicon};

pub(crate) const INBOX: &str = "/inbox";
const READ_AT: &str = "inbox_read_at";
const KINDS: &str = "notify_kinds";
const MINE_OFF: &str = "notify_mine_off";
/// The kinds the form showed when the person saved; a kind added later
/// is on until they choose.
const KNOWN: &str = "notify_kinds_known";
/// The kinds before [`KNOWN`] was saved.
const FIRST_KINDS: &str = "pr,issue,advisory,release";

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/inbox", get(inbox_page))
        .route("/me/inbox/read", post(mark_read))
        .route("/me/notify", post(save_notify))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Mine,
    PullRequest,
    Issue,
    Advisory,
    Release,
    Build,
}

impl serde::Serialize for Kind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.key())
    }
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::Mine,
        Kind::PullRequest,
        Kind::Issue,
        Kind::Advisory,
        Kind::Release,
        Kind::Build,
    ];

    /// From the id that `catalog::events`, `catalog::set_events`,
    /// `followup::events` and `builds::events` give.
    pub fn of(id: &str) -> Kind {
        match id.split_once('-').map_or(id, |(k, _)| k) {
            "mine" => Kind::Mine,
            "issue" => Kind::Issue,
            "advisory" => Kind::Advisory,
            "upstream" => Kind::Release,
            "hydra" => Kind::Build,
            _ => Kind::PullRequest,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Kind::Mine => "mine",
            Kind::PullRequest => "pr",
            Kind::Issue => "issue",
            Kind::Advisory => "advisory",
            Kind::Release => "release",
            Kind::Build => "build",
        }
    }

    fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.key() == s)
    }

    fn label(self) -> &'static str {
        match self {
            Kind::Mine => "Your pull requests",
            Kind::PullRequest => "Pull requests",
            Kind::Issue => "Issues",
            Kind::Advisory => "Advisories",
            Kind::Release => "Releases",
            Kind::Build => "Hydra builds",
        }
    }

    fn icon(self) -> crate::html::Html {
        match self {
            Kind::Mine => icon(octicon!("person")),
            Kind::PullRequest => icon(octicon!("git-pull-request")),
            Kind::Issue => icon(octicon!("issue-opened")),
            Kind::Advisory => icon(octicon!("shield")),
            Kind::Release => icon(octicon!("tag")),
            Kind::Build => icon(octicon!("tools")),
        }
    }
}

/// The kinds a person counts in the header. All of them
/// until the person chooses. Your own pull requests have their own switch,
/// so a choice saved before they existed keeps them on.
pub fn kinds(app: &App, login: &str) -> Vec<Kind> {
    let mut out: Vec<Kind> = match app.store.setting(login, KINDS).ok().flatten() {
        None => Kind::ALL.to_vec(),
        Some(v) => {
            let known = app.store.setting(login, KNOWN).ok().flatten();
            let known = known.as_deref().unwrap_or(FIRST_KINDS);
            let mut chosen: Vec<Kind> = v
                .split(',')
                .filter_map(Kind::parse)
                .filter(|k| *k != Kind::Mine)
                .collect();
            chosen.extend(
                Kind::ALL
                    .into_iter()
                    .filter(|k| *k != Kind::Mine && !known.split(',').any(|x| x == k.key())),
            );
            chosen
        }
    };
    let off = app.store.setting(login, MINE_OFF).ok().flatten();
    out.retain(|k| *k != Kind::Mine);
    if off.as_deref() != Some("1") {
        out.insert(0, Kind::Mine);
    }
    out
}

pub(crate) fn read_at(app: &App, login: &str) -> Option<DateTime<Utc>> {
    app.store.setting_time(login, READ_AT)
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

/// Unread counts per login, for the header. A recompute, the inbox page and
/// each change of settings write it.
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
    crate::followup::purge(app);
    crate::review::purge(app);
    for m in app.store.members().unwrap_or_default() {
        recount_one(app, &m.login);
    }
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
        INBOX.to_string()
    } else {
        format!("{INBOX}?{}", q.join("&"))
    }
}

#[derive(Deserialize)]
pub(crate) struct InboxQuery {
    pub(crate) sub: Option<String>,
    pub(crate) kind: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct InboxRow<'a> {
    id: &'a str,
    kind: Kind,
    at: DateTime<Utc>,
    title: &'a str,
    url: &'a str,
    subject: &'a str,
    unread: bool,
}

/// What `/inbox` shows for one filter. Reading it marks nothing read.
pub(crate) struct Inbox<'a> {
    rows: Vec<InboxRow<'a>>,
    /// Counted with the kinds the person chose, as the header counts.
    unread: usize,
    any_unread: bool,
    subjects: BTreeSet<&'a str>,
    kind: Option<Kind>,
    sub: Option<&'a str>,
}

impl<'a> Inbox<'a> {
    fn new(
        app: &App,
        login: &str,
        all: &'a [Event],
        kind: Option<Kind>,
        sub: Option<&'a str>,
    ) -> Self {
        let read = read_at(app, login);
        Inbox {
            rows: all
                .iter()
                .filter(|e| sub.is_none_or(|s| e.subject == s))
                .filter(|e| kind.is_none_or(|k| Kind::of(&e.id) == k))
                .map(|e| InboxRow {
                    id: &e.id,
                    kind: Kind::of(&e.id),
                    at: e.at,
                    title: &e.title,
                    url: &e.url,
                    subject: &e.subject,
                    unread: unread(e, read),
                })
                .collect(),
            unread: unread_count(all, read, &kinds(app, login)),
            any_unread: all.iter().any(|e| unread(e, read)),
            subjects: all.iter().map(|e| e.subject.as_str()).collect(),
            kind,
            sub,
        }
    }
}

/// The events of `/inbox`, as JSON. Reading them marks nothing read.
pub(crate) fn inbox_value(
    app: &App,
    login: &str,
    kind: Option<&str>,
    sub: Option<&str>,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<serde_json::Value, String> {
    let kind = match kind.filter(|k| !k.is_empty()) {
        None => None,
        Some(k) => Some(Kind::parse(k).ok_or_else(|| format!("no kind {k}"))?),
    };
    let all = person_events(app, login);
    let inbox = Inbox::new(app, login, &all, kind, sub.filter(|s| !s.is_empty()));
    let mut out = crate::api::page(&inbox.rows, limit, cursor)?;
    out["login"] = serde_json::json!(login);
    out["unread"] = serde_json::json!(inbox.unread);
    out["subjects"] = serde_json::json!(inbox.subjects);
    Ok(out)
}

/// `/inbox`. Opening it counts the unread events again.
async fn inbox_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<InboxQuery>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let all = person_events(&app, &login);
    let inbox = Inbox::new(
        &app,
        &login,
        &all,
        q.kind.as_deref().and_then(Kind::parse),
        q.sub.as_deref().filter(|s| !s.is_empty()),
    );
    app.inbox.set(&login, inbox.unread);
    inbox_view(&inbox).render(&app, Some(&login))
}

fn inbox_view(inbox: &Inbox) -> Page {
    let (sub, kind) = (inbox.sub, inbox.kind);
    let actions = if inbox.any_unread {
        Html::from(
            "<form method=\"post\" action=\"/me/inbox/read\"><button class=\"btn btn-sm\" type=\"submit\">Mark all read</button></form>",
        )
    } else {
        Html::new()
    };
    let mut tabs = vec![Tab::new(inbox_href(sub, None), "All", kind.is_none())];
    for k in Kind::ALL {
        tabs.push(Tab::new(
            inbox_href(sub, Some(k)),
            html!("{} {}", k.icon(), k.label()),
            kind == Some(k),
        ));
    }
    let mut out = Html::new();
    if !inbox.subjects.is_empty() {
        let hidden = kind
            .map(|k| {
                html!(
                    "<input type=\"hidden\" name=\"kind\" value=\"{}\">",
                    k.key()
                )
            })
            .unwrap_or_default();
        out.push(&html!(
            "<form method=\"get\" action=\"{INBOX}\" class=\"d-flex gap-2 mb-3\">{hidden}\
             <select class=\"form-select\" name=\"sub\" aria-label=\"what you watch\"><option value=\"\">Everything you watch</option>"
        ));
        for s in &inbox.subjects {
            let sel = if sub == Some(*s) { " selected" } else { "" };
            out.push(&html!("<option value=\"{s}\"{sel}>{s}</option>"));
        }
        out.push(&Html::from(
            "</select><button class=\"btn btn-sm\" type=\"submit\">Filter</button></form>",
        ));
    }
    if inbox.rows.is_empty() {
        out.push(&Html::from(
            "<div class=\"blankslate\"><p>Nothing here. <a href=\"/packages?filter=watch\">Watch</a> a package, a module or a directory, \
             or send the packages of <a href=\"/me/settings#upload\">your machines</a>.</p></div>",
        ));
    } else {
        out.push(&Html::from("<div class=\"Box\">"));
        for e in &inbox.rows {
            let class = if e.unread {
                "Link--primary text-bold"
            } else {
                "Link--primary"
            };
            let title = if e.url.is_empty() {
                html!("<span class=\"{class}\">{}</span>", e.title)
            } else {
                crate::prpage::link(e.url, Html::text(e.title), class)
            };
            let new = if e.unread {
                "<span class=\"Label Label--accent ml-2 flex-shrink-0\">new</span>"
            } else {
                ""
            };
            out.push(&html!(
                "<div class=\"Box-row d-flex flex-items-start\"><span class=\"mr-2 color-fg-muted\">{}</span>\
                 <div class=\"flex-auto min-width-0 wb-break-word\">{title}\
                 <div class=\"f6 color-fg-muted mt-1\">{} · {} ago</div></div>{}</div>",
                e.kind.icon(),
                e.subject,
                ago(e.at),
                Html::from(new)
            ));
        }
        out.push(&Html::from("</div>"));
    }
    Page::new(Section::Personal(Personal::Inbox), "Inbox")
        .icon(octicon!("inbox"))
        .sub(
            "What happened in the last 30 days to your pull requests, to what you watch and to your machines, newest first. \
             Your Atom feed has the same events. Only you see this page.",
        )
        .subnav(tabs)
        .actions(actions)
        .body(out)
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
    Redirect::to(INBOX).into_response()
}

/// The `#notifications` section of `/me/settings`.
pub(crate) fn settings_box(app: &App, login: &str) -> Html {
    let on = kinds(app, login);
    let mut boxes = Html::new();
    for k in Kind::ALL {
        boxes.push(&html!(
            "<div class=\"form-checkbox my-1\"><label><input type=\"checkbox\" name=\"k_{key}\" value=\"1\"{}> {} {}</label></div>",
            if on.contains(&k) { " checked" } else { "" },
            k.icon(),
            k.label(),
            key = k.key()
        ));
    }
    html!(
        "<div id=\"notifications\">{}</div>",
        crate::ui::section(
            html!("{} Notifications", icon(octicon!("bell"))),
            None,
            None,
            html!(
                "<div class=\"Box-row\"><form method=\"post\" action=\"/me/notify\">\
                 <p>Your <a href=\"/inbox\">inbox</a> shows every event. The unread count in the header includes only the kinds you choose here.</p>\
                 {boxes}<button class=\"btn btn-sm mt-2\" type=\"submit\">Save</button></form></div>"
            )
        )
    )
}

#[derive(Deserialize)]
struct NotifyForm {
    #[serde(default)]
    k_mine: Option<String>,
    #[serde(default)]
    k_pr: Option<String>,
    #[serde(default)]
    k_issue: Option<String>,
    #[serde(default)]
    k_advisory: Option<String>,
    #[serde(default)]
    k_release: Option<String>,
    #[serde(default)]
    k_build: Option<String>,
}

impl NotifyForm {
    fn chosen(&self) -> Vec<Kind> {
        [
            (Kind::Mine, &self.k_mine),
            (Kind::PullRequest, &self.k_pr),
            (Kind::Issue, &self.k_issue),
            (Kind::Advisory, &self.k_advisory),
            (Kind::Release, &self.k_release),
            (Kind::Build, &self.k_build),
        ]
        .into_iter()
        .filter(|(_, v)| v.as_deref() == Some("1"))
        .map(|(k, _)| k)
        .collect()
    }
}

async fn save_notify(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<NotifyForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let chosen = f.chosen();
    let keys: Vec<&str> = chosen
        .iter()
        .copied()
        .filter(|k| *k != Kind::Mine)
        .map(Kind::key)
        .collect();
    let mine_off = if chosen.contains(&Kind::Mine) {
        ""
    } else {
        "1"
    };
    let saved = app
        .store
        .set_setting(&login, KINDS, &keys.join(","))
        .and_then(|_| {
            let all: Vec<&str> = Kind::ALL
                .into_iter()
                .filter(|k| *k != Kind::Mine)
                .map(Kind::key)
                .collect();
            app.store.set_setting(&login, KNOWN, &all.join(","))
        })
        .and_then(|_| app.store.set_setting(&login, MINE_OFF, mine_off));
    if let Err(e) = saved {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    recount_one(&app, &login);
    Redirect::to("/me/settings#notifications").into_response()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::http::header;
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
        assert_eq!(Kind::of("mine-1-review-7"), Kind::Mine);
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
        let r = mark_read(State(app.clone()), HeaderMap::new()).await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        assert!(read_at(&app, "alice").is_none());
        let h = signed_in(&app, "alice");
        app.inbox.set("alice", 3);
        let q = |kind: Option<&str>| {
            Query(InboxQuery {
                sub: None,
                kind: kind.map(String::from),
            })
        };
        let r = inbox_page(State(app.clone()), HeaderMap::new(), q(None)).await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        let r = inbox_page(State(app.clone()), h.clone(), q(Some("advisory"))).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()[header::CACHE_CONTROL], "private, no-store");
        let b = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(b.to_vec()).unwrap();
        assert!(html.contains("href=\"/inbox?kind=advisory\" aria-current=\"page\""));
        assert!(html.contains("<a href=\"/inbox\" aria-current=\"page\">"));
        assert!(!html.contains("site-tab\" href=\"/\" aria-current"));
        assert_eq!(app.inbox.get("alice"), 0);
        app.inbox.set("alice", 3);
        assert_eq!(app.inbox.get("alice"), 3);
        let r = mark_read(State(app.clone()), h).await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        assert_eq!(app.inbox.get("alice"), 0);
        assert!(read_at(&app, "alice").is_some());
    }

    #[tokio::test]
    async fn your_pull_requests_stay_on_after_an_old_choice_and_have_their_own_switch() {
        let app = crate::sets::tests::app(&[]);
        assert_eq!(kinds(&app, "alice"), Kind::ALL);
        app.store.set_setting("alice", KINDS, "pr,issue").unwrap();
        assert_eq!(
            kinds(&app, "alice"),
            [Kind::Mine, Kind::PullRequest, Kind::Issue, Kind::Build],
            "a kind added after the choice is on"
        );
        let h = signed_in(&app, "alice");
        let form = |mine: bool| NotifyForm {
            k_mine: mine.then(|| "1".into()),
            k_pr: None,
            k_issue: None,
            k_advisory: Some("1".into()),
            k_release: None,
            k_build: None,
        };
        save_notify(State(app.clone()), h.clone(), Form(form(false))).await;
        assert_eq!(kinds(&app, "alice"), [Kind::Advisory]);
        save_notify(State(app.clone()), h, Form(form(true))).await;
        assert_eq!(kinds(&app, "alice"), [Kind::Mine, Kind::Advisory]);
    }

    #[test]
    fn a_filter_link_keeps_the_other_filter() {
        assert_eq!(inbox_href(None, None), "/inbox");
        assert_eq!(
            inbox_href(Some("pkgs/by-name/"), Some(Kind::Advisory)),
            "/inbox?sub=pkgs%2Fby-name%2F&kind=advisory"
        );
    }
}
