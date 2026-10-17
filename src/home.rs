//! The landing page. A visitor gets ways into nixpkgs; a person who signed
//! in gets what needs them: their inbox, what they maintain, what to review,
//! and their own pull requests.

use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;
use std::fmt::Write;

use crate::catalog::{Flags, Subject, maintained, person_events, subject_row};
use crate::queue::Entry;
use crate::ui::{Kind, Page, Section, empty, item_row};
use crate::web::{App, Shared, ago, esc, icon, octicon};

const SHOWN: usize = 8;

pub fn routes() -> Router<Shared> {
    Router::new().route("/", get(home))
}

fn section(title: &str, more: Option<(&str, &str)>, body: &str) -> String {
    crate::ui::section(title, None, more, body)
}

fn entry_rows(entries: &[&Entry]) -> String {
    entries
        .iter()
        .take(SHOWN)
        .map(|e| {
            item_row(
                Some(Kind::PR_OPEN),
                &crate::prpage::title_link(e.number, &esc(&e.title), "Link--primary text-bold"),
                "",
                &format!(
                    "#{} by <a href=\"/u/{a}\">{a}</a> · {} old",
                    e.number,
                    ago(e.created_at),
                    a = esc(&e.author)
                ),
                &[],
                "",
            )
        })
        .collect()
}

fn subject_rows(rows: &[(Subject, Flags)]) -> String {
    rows.iter()
        .take(SHOWN)
        .map(|(sub, f)| subject_row(sub, f, "", ""))
        .collect()
}

fn search_box() -> String {
    format!(
        "<form method=\"get\" action=\"/packages\" class=\"inline-form mb-4\">\
         <input class=\"form-control flex-auto\" name=\"q\" placeholder=\"a package, nixos/MODULE, a path or @login\" aria-label=\"search\">\
         <button class=\"btn\" type=\"submit\">{} Search</button></form>",
        icon(octicon!("search"))
    )
}

fn visitor(app: &App, headers: &HeaderMap) -> String {
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let mut out = crate::welcome::intro(app, headers);
    out.push_str(&search_box());
    let first: Vec<&Entry> = s.queues.review.iter().filter(|e| e.unowned).collect();
    out.push_str(&section(
        "Pull requests with no maintainer",
        Some(("/review?tab=unowned", "all of them")),
        &if first.is_empty() {
            empty("None right now.")
        } else {
            format!(
                "<div class=\"Box-row f6 color-fg-muted\">No maintainer covers their files, so your review helps most here. \
                 Read <a href=\"{}#how-to-review-pull-requests\">how nixpkgs reviews</a> first.</div>{}",
                crate::web::CONTRIBUTING,
                entry_rows(&first)
            )
        },
    ));
    let mut help: Vec<(Subject, Flags)> = {
        let index = app.index.read().unwrap();
        h.advisories
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
            .collect()
    };
    help.sort_by_key(|(sub, f)| (f.rank(), sub.label()));
    out.push_str(&section(
        "Packages that need help",
        Some(("/packages", "search all")),
        &if help.is_empty() {
            empty("The health data is not read yet.")
        } else {
            subject_rows(&help)
        },
    ));
    let p = app.packages.read().unwrap().clone();
    out.push_str(&section(
        "nixpkgs in numbers",
        Some(("/state", "the state of nixpkgs")),
        &format!(
            "<div class=\"Box-row d-flex flex-wrap gap-4\">\
             <span><strong>{}</strong> open pull requests need review</span>\
             <span><strong>{}</strong> packages behind upstream</span>\
             <span><strong>{}</strong> packages with an open advisory</span>\
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

fn dashboard(app: &App, login: &str) -> String {
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let mut out = String::new();
    if crate::welcome::pending(app, login) {
        out.push_str(
            "<div class=\"callout mb-3\">New here? <a href=\"/welcome\">Get started</a>: \
             subscribe to what you maintain, and set up your inbox.</div>",
        );
    }
    out.push_str(&crate::progress::widget(app, login));

    let unread = app.inbox.get(login);
    let events = person_events(app, login);
    let news: String = events
        .iter()
        .take(5)
        .map(|e| {
            format!(
                "<div class=\"Box-row f6\"><span class=\"color-fg-muted\">{} ago · {}</span> {}</div>",
                ago(e.at),
                esc(&e.subject),
                crate::prpage::link(&e.url, &esc(&e.title), "")
            )
        })
        .collect();
    out.push_str(&section(
        &format!(
            "{} Inbox{}",
            icon(octicon!("inbox")),
            if unread > 0 {
                format!(" <span class=\"Counter\">{unread}</span>")
            } else {
                String::new()
            }
        ),
        Some(("/me", "open the inbox")),
        &if news.is_empty() {
            empty("Nothing new. <a href=\"/me?tab=watch\">Watch</a> packages, modules or directories to hear about them.")
        } else {
            news
        },
    ));

    let mine: Vec<(Subject, Flags)> = {
        let index = app.index.read().unwrap();
        let m = maintained(&index, login);
        let mut v: Vec<(Subject, Flags)> = m
            .packages
            .iter()
            .chain(&m.modules)
            .map(|(sub, _)| {
                let f = Flags::of(&s, &index, &h, sub);
                (sub.clone(), f)
            })
            .filter(|(_, f)| f.risk() > 0 || f.open_prs > 0)
            .collect();
        v.sort_by_key(|(sub, f)| (f.rank(), sub.label()));
        v
    };
    out.push_str(&section(
        &format!(
            "{} What you maintain that needs you",
            icon(octicon!("package"))
        ),
        Some(("/me?tab=maintain", "everything you maintain")),
        &if mine.is_empty() {
            empty(
                "Nothing needs you right now, or nixpkgs names you as a maintainer of nothing yet.",
            )
        } else {
            subject_rows(&mine)
        },
    ));

    let watched = app.store.watched(login).unwrap_or_default();
    let review = crate::web::review_bucket(&s, Some(login), &watched, "foryou");
    out.push_str(&section(
        &format!("{} Pull requests for you to review", icon(octicon!("eye"))),
        Some(("/review?tab=foryou", "the whole queue")),
        &if review.is_empty() {
            empty("No open pull request touches what you maintain or watch. The pull requests with <a href=\"/review?tab=unowned\">no maintainer</a> need reviews too.")
        } else {
            entry_rows(&review)
        },
    ));

    let mut own: Vec<&crate::model::Pr> = s
        .pr_data
        .iter()
        .filter(|p| p.is_open() && p.author.eq_ignore_ascii_case(login))
        .collect();
    own.sort_by_key(|p| std::cmp::Reverse(p.updated_at));
    let own_html: String = own
        .iter()
        .take(SHOWN)
        .map(|p| {
            let state = if p.is_draft {
                "draft"
            } else if s.queues.merge.iter().any(|e| e.number == p.number) {
                "ready to merge"
            } else if p.mergeable == "CONFLICTING" {
                "has a conflict"
            } else if matches!(p.ci.as_deref(), Some("FAILURE" | "ERROR")) {
                "checks fail"
            } else if p.reviews.iter().any(|r| r.state == "APPROVED") {
                "approved, waiting"
            } else if p.reviews.is_empty() {
                "waiting for a first review"
            } else {
                "in review"
            };
            let mut line = String::new();
            let _ = write!(
                line,
                "<div class=\"Box-row d-flex\"><span class=\"flex-auto\">{}</span>\
                 <span class=\"Label\">{state}</span></div>",
                crate::prpage::title_link(
                    p.number,
                    &format!("#{} {}", p.number, esc(&p.title)),
                    "Link--primary"
                )
            );
            line
        })
        .collect();
    out.push_str(&section(
        &format!(
            "{} Your open pull requests",
            icon(octicon!("git-pull-request"))
        ),
        Some((
            &format!("https://github.com/NixOS/nixpkgs/pulls/{}", esc(login)),
            "on GitHub",
        )),
        &if own_html.is_empty() {
            empty("You have no open pull request in the synced data.")
        } else {
            own_html
        },
    ));

    if s.committers.is(login) {
        out.push_str(&section(
            &format!("{} You can merge", icon(octicon!("git-merge"))),
            Some(("/review?tab=ready", "Ready to merge")),
            &format!(
                "<div class=\"Box-row\">{} pull requests are ready to merge. \
                 <a href=\"/review?tab=ready\">See them</a>, your areas first.</div>",
                s.queues.merge.len()
            ),
        ));
    }
    out
}

async fn home(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let viewer = app.viewer(&headers);
    let page = match viewer.as_deref() {
        Some(login) => Page::new(Section::Home, "Home").body(dashboard(&app, login)),
        None => Page::new(Section::Home, "Maintainer Mode").body(visitor(&app, &headers)),
    };
    page.render(&app, viewer.as_deref())
}
