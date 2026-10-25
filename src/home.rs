//! The landing page. A visitor gets ways into nixpkgs; a person who signed
//! in gets Needs you on top, then their inbox, their packages and their own
//! pull requests.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use axum::routing::get;
use serde::Deserialize;

use crate::catalog::{Flags, Subject, subject_row};
use crate::html::{Html, html};
use crate::queue::Entry;
use crate::ui::{Kind, Page, Section, empty, item_row};
use crate::web::{App, Shared, ago, icon, octicon};

const SHOWN: usize = 8;

pub fn routes() -> Router<Shared> {
    Router::new().route("/", get(home))
}

fn section(title: impl Into<Html>, more: Option<(&str, &str)>, body: impl Into<Html>) -> Html {
    crate::ui::section(title, None, more, body)
}

fn entry_rows(entries: &[&Entry]) -> Html {
    entries
        .iter()
        .take(SHOWN)
        .map(|e| {
            item_row(
                Some(Kind::PR_OPEN),
                crate::prpage::title_link(
                    e.number,
                    Html::text(&e.title),
                    "Link--primary text-bold",
                ),
                "",
                html!(
                    "#{} by <a href=\"/u/{a}\">{a}</a> · {} old",
                    e.number,
                    ago(e.created_at),
                    a = e.author
                ),
                &[],
                "",
            )
        })
        .collect()
}

fn subject_rows(rows: &[(Subject, Flags)]) -> Html {
    rows.iter()
        .take(SHOWN)
        .map(|(sub, f)| subject_row(sub, f, "", ""))
        .collect()
}

/// Packages with an open advisory in master, the highest risk first.
pub(crate) fn need_help(app: &App) -> Vec<(Subject, Flags)> {
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let mut help: Vec<(Subject, Flags)> = h
        .advisories
        .keys()
        .filter(|a| {
            index
                .pkgs
                .get(*a)
                .is_some_and(|p| index.by_pname.get(&p.pname) == Some(*a))
        })
        .map(|a| {
            let sub = Subject::Pkg(a.clone());
            let f = Flags::of(&s, &index, &h, &sub);
            (sub, f)
        })
        .filter(|(_, f)| f.risk() > 0)
        .collect();
    help.sort_by_key(|(sub, f)| (f.rank(), sub.label()));
    help
}

fn visitor(app: &App, headers: &HeaderMap) -> Html {
    let s = app.snap();
    let mut out = crate::welcome::intro(app, headers);
    if out.is_empty() {
        out.push(&Html::from("<h1 class=\"sr-only\">Maintainer Mode</h1>"));
    }
    let first: Vec<&Entry> = s.queues.review.iter().filter(|e| e.unowned).collect();
    out.push(&section(
        "Pull requests with no maintainer",
        Some(("/review?tab=unowned", "all of them")),
        if first.is_empty() {
            empty("None right now.")
        } else {
            html!(
                "<div class=\"Box-row f6 color-fg-muted\">No maintainer, team or ci/OWNERS line covers their files. A first review helps most here. \
                 Read <a href=\"{}#how-to-review-pull-requests\">how nixpkgs reviews</a> first.</div>{}",
                crate::web::CONTRIBUTING,
                entry_rows(&first)
            )
        },
    ));
    out.push(&crate::first::section(app));
    let help = need_help(app);
    out.push(&section(
        "Packages that need help",
        Some(("/packages", "search all")),
        if help.is_empty() {
            empty(if app.health.read().unwrap().tracker_at.is_none() {
                "The health data is not read yet."
            } else {
                "None right now."
            })
        } else {
            subject_rows(&help)
        },
    ));
    let p = app.packages.read().unwrap().clone();
    out.push(&section(
        "nixpkgs in numbers",
        Some(("/state", "the state of nixpkgs")),
        html!(
            "<div class=\"Box-row d-flex flex-wrap gap-4\">\
             <span><strong>{}</strong> open pull requests need review</span>\
             <span><strong>{}</strong> packages behind upstream</span>\
             <span><strong>{}</strong> packages with an open advisory in master</span>\
             <span><strong>{}</strong> packages with no maintainer</span>\
             <span><strong>{}</strong> open version bumps</span></div>",
            s.queues.review.len(),
            p.behind_upstream,
            p.with_advisories,
            p.unmaintained,
            p.open_bumps
        ),
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
    }
    out
}

/// What the list above leaves out: the history in the inbox, every package
/// and every open pull request of the person, and for a committer the
/// merge queue.
fn more_links(app: &App, s: &crate::web::Snapshot, login: &str) -> Html {
    let counter = |n: usize| {
        if n > 0 {
            html!(" <span class=\"Counter\">{n}</span>")
        } else {
            Html::new()
        }
    };
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
            .body(visitor(&app, &headers))
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
