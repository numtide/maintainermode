//! The landing page. A visitor gets a first package to work on; a person
//! who signed in gets Needs you. Then one row of links leads to the rest.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use axum::routing::get;
use serde::Deserialize;

use crate::html::{Html, html};
use crate::ui::{Page, Section};
use crate::web::{App, Shared, icon, octicon};

pub fn routes() -> Router<Shared> {
    Router::new().route("/", get(home))
}

fn counter(n: usize) -> Html {
    if n > 0 {
        html!(" <span class=\"Counter\">{n}</span>")
    } else {
        Html::new()
    }
}

/// One short introduction, then one list, a first package to work on,
/// then one row of links to the rest. Each row of the list gives one small
/// task, so a visitor can start without knowing nixpkgs well.
fn visitor(app: &App) -> Html {
    let s = app.snap();
    let unowned = s.queues.review.iter().filter(|e| e.unowned).count();
    let mut out = crate::welcome::intro(app);
    out.push(&crate::first::section(app));
    out.push(&html!(
        "<nav class=\"d-flex flex-wrap gap-2 mb-4\" aria-label=\"More in nixpkgs\">{}\
         <a class=\"btn btn-sm\" href=\"/review?tab=unowned\">{} Pull requests with no maintainer{}</a>\
         <a class=\"btn btn-sm\" href=\"/packages\">{} Packages and directories</a>\
         <a class=\"btn btn-sm\" href=\"/state\">{} The state of nixpkgs</a></nav>",
        crate::zhf::home_link(app, None),
        icon(octicon!("git-pull-request")),
        counter(unowned),
        icon(octicon!("package")),
        icon(octicon!("graph")),
    ));
    out
}

fn dashboard(app: &App, login: &str, mood: crate::needs::Mood, headers: &HeaderMap) -> Html {
    let s = app.snap();
    let mut out = Html::from("<h1 class=\"sr-only\">Home</h1>");
    out.push(&crate::needs::section(app, login, mood, headers));
    out.push(&more_links(app, &s, login));
    out.push(&crate::welcome::card(app, login));
    let maintains = {
        let index = app.index.read().unwrap();
        let m = crate::catalog::maintained(&index, login);
        !m.packages.is_empty() || !m.modules.is_empty()
    };
    if !maintains {
        out.push(&crate::first::section(app));
    } else if crate::load::light(app, login) {
        out.push(&crate::first::help_section(app));
    }
    out
}

/// What the list above leaves out: the history in the inbox, every package
/// and every open pull request of the person, and for a committer the
/// merge queue.
fn more_links(app: &App, s: &crate::web::Snapshot, login: &str) -> Html {
    let own = s
        .pr_data
        .iter()
        .filter(|p| p.is_open() && p.author.eq_ignore_ascii_case(login))
        .count();
    let mut links = vec![
        html!(
            "<a class=\"btn btn-sm\" href=\"/inbox\">{} Inbox{}</a>",
            icon(octicon!("inbox")),
            counter(app.inbox.get(login))
        ),
        html!(
            "<a class=\"btn btn-sm\" href=\"/packages?filter=mine\">{} Your packages</a>",
            icon(octicon!("package"))
        ),
        html!(
            "<a class=\"btn btn-sm\" href=\"https://github.com/NixOS/nixpkgs/pulls/{login}\">{} Your pull requests{}</a>",
            icon(octicon!("git-pull-request")),
            counter(own)
        ),
    ];
    if s.committers.is(login) {
        links.push(html!(
            "<a class=\"btn btn-sm\" href=\"/review?tab=ready\">{} Ready to merge{}</a>",
            icon(octicon!("git-merge")),
            counter(s.queues.merge.len())
        ));
    }
    let zhf = crate::zhf::home_link(app, Some(login));
    if !zhf.is_empty() {
        links.push(zhf);
    }
    html!(
        "<nav class=\"d-flex flex-wrap gap-2 mb-4\" aria-label=\"More for you\">{}</nav>",
        crate::html::join(links, "")
    )
}

#[derive(Deserialize)]
struct HomeQuery {
    mood: Option<String>,
}

async fn home(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<HomeQuery>,
) -> Response {
    let viewer = app.viewer(&headers);
    let Some(login) = viewer.as_deref() else {
        return Page::new(Section::Home, "Maintainer Mode")
            .body(visitor(&app))
            .bare()
            .render(&app, None);
    };
    let mood = crate::needs::Mood::of(q.mood.as_deref(), &headers);
    let mut res = Page::new(Section::Home, "Home")
        .body(dashboard(&app, login, mood, &headers))
        .bare()
        .render(&app, Some(login));
    if let Some(m) = crate::needs::Mood::parse(q.mood.as_deref()) {
        if let Ok(v) = HeaderValue::from_str(&crate::needs::mood_cookie(&app, m)) {
            res.headers_mut().append(header::SET_COOKIE, v);
        }
    }
    res
}
