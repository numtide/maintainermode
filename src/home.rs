//! The landing page. A visitor gets ways into nixpkgs; a person who signed
//! in gets what needs them: their inbox, what they maintain, what to review,
//! and their own pull requests.

use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Html;
use axum::routing::get;
use std::fmt::Write;

use crate::catalog::{Flags, Subject, maintained, person_events};
use crate::queue::Entry;
use crate::web::{App, Shared, ago, esc, icon, octicon, page};

const SHOWN: usize = 8;

pub fn routes() -> Router<Shared> {
    Router::new().route("/", get(home))
}

fn section(title: &str, more: Option<(&str, &str)>, body: &str) -> String {
    let more = more
        .map(|(href, label)| {
            format!(" <a class=\"f6 text-normal ml-2\" href=\"{href}\">{label}</a>")
        })
        .unwrap_or_default();
    format!(
        "<div class=\"Box mb-4\"><div class=\"Box-header text-bold\">{title}{more}</div>{body}</div>"
    )
}

fn empty(text: &str) -> String {
    format!("<div class=\"Box-row color-fg-muted\">{text}</div>")
}

fn entry_rows(entries: &[&Entry]) -> String {
    entries
        .iter()
        .take(SHOWN)
        .map(|e| {
            format!(
                "<div class=\"Box-row\"><a class=\"Link--primary text-bold\" href=\"{}\">{}</a>\
                 <div class=\"f6 color-fg-muted\">#{} by <a href=\"/u/{a}\">{a}</a> · {} old</div></div>",
                esc(&e.url),
                esc(&e.title),
                e.number,
                ago(e.created_at),
                a = esc(&e.author)
            )
        })
        .collect()
}

fn subject_rows(rows: &[(Subject, Flags)]) -> String {
    rows.iter()
        .take(SHOWN)
        .map(|(sub, f)| {
            format!(
                "<div class=\"Box-row d-flex flex-items-center flex-wrap\" style=\"gap:8px\">\
                 <a class=\"Link--primary text-bold flex-auto\" href=\"{}\">{} {}</a>{}</div>",
                sub.href(),
                sub.icon(),
                esc(&sub.label()),
                f.html()
            )
        })
        .collect()
}

fn search_box() -> String {
    format!(
        "<form method=\"get\" action=\"/packages\" class=\"d-flex mb-4\" style=\"gap:8px;max-width:560px\">\
         <input class=\"form-control flex-auto\" name=\"q\" placeholder=\"a package, nixos/MODULE, or a path\" aria-label=\"search\">\
         <button class=\"btn\" type=\"submit\">{} Search</button></form>",
        icon(octicon!("search"))
    )
}

fn visitor(app: &App, headers: &HeaderMap) -> String {
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let mut out = crate::welcome::intro(app, headers);
    out.push_str(&search_box());
    let first: Vec<&Entry> = s.queues.review.iter().filter(|e| e.unowned).rev().collect();
    out.push_str(&section(
        "Pull requests that need a first review",
        Some(("/review?bucket=unowned", "all of them")),
        &if first.is_empty() {
            empty("None right now.")
        } else {
            format!(
                "<div class=\"Box-row f6 color-fg-muted\">No maintainer covers their files, so a newcomer review helps most here. \
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
            "<div class=\"Box-row d-flex flex-wrap\" style=\"gap:24px\">\
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
            "<div class=\"flash mb-3\">New here? <a href=\"/welcome\">Get started</a>: \
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
                "<div class=\"Box-row f6\"><span class=\"color-fg-muted\">{} ago · {}</span> <a href=\"{}\">{}</a></div>",
                ago(e.at),
                esc(&e.subject),
                esc(&e.url),
                esc(&e.title)
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
        Some(("/me/inbox", "open the inbox")),
        &if news.is_empty() {
            empty("Nothing new. <a href=\"/watch\">Subscribe</a> to packages, modules or directories to hear about them.")
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
        Some((
            &format!("/u/{}/maintains", esc(login)),
            "everything you maintain",
        )),
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
        Some(("/review?bucket=foryou", "the whole queue")),
        &if review.is_empty() {
            empty("No open pull request touches what you maintain or watch. The <a href=\"/review?bucket=unowned\">unowned ones</a> need reviews too.")
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
                "<div class=\"Box-row d-flex\"><a class=\"Link--primary flex-auto\" href=\"{}\">#{} {}</a>\
                 <span class=\"Label\">{state}</span></div>",
                esc(&p.url),
                p.number,
                esc(&p.title)
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
            Some(("/committer", "the committer view")),
            &format!(
                "<div class=\"Box-row\">{} pull requests are ready to merge. \
                 <a href=\"/committer\">See them</a>, yours and your areas first.</div>",
                s.queues.merge.len()
            ),
        ));
    }
    out
}

async fn home(State(app): State<Shared>, headers: HeaderMap) -> Html<String> {
    let viewer = app.viewer(&headers);
    let body = match viewer.as_deref() {
        Some(login) => dashboard(&app, login),
        None => visitor(&app, &headers),
    };
    page(&app, viewer.as_deref(), "home", &body)
}
