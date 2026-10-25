//! The first steps: a short introduction on the home page for visitors, and
//! a checklist for a person who signed in, computed from their own state.

use axum::Router;
use axum::extract::{Form, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use std::collections::HashSet;

use crate::html::{Html, html};
use crate::web::{App, Shared, icon, octicon};

const DONE: &str = "welcome_done";
const SEEN: &str = "welcome_at";

use crate::web::CONTRIBUTING;
pub(crate) const MAINTAINERS_README: &str =
    "https://github.com/NixOS/nixpkgs/blob/master/maintainers/README.md";
const GUIDES: [(&str, &str, &str); 5] = [
    (
        "CONTRIBUTING.md",
        CONTRIBUTING,
        "how to open, review and merge a pull request, commit messages, and the policy on automation and AI",
    ),
    (
        "pkgs/README.md",
        "https://github.com/NixOS/nixpkgs/blob/master/pkgs/README.md",
        "how to write and update a package",
    ),
    (
        "nixos/README.md",
        "https://github.com/NixOS/nixpkgs/blob/master/nixos/README.md",
        "how to write a NixOS module and its test",
    ),
    (
        "maintainers/README.md",
        MAINTAINERS_README,
        "how to become a maintainer, and what it means",
    ),
    (
        "The Nixpkgs manual",
        "https://nixos.org/manual/nixpkgs/unstable/",
        "the reference",
    ),
];

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/welcome", get(|| async { Redirect::permanent("/#start") }))
        .route("/me/start", post(save_done))
}

/// True once, on the first sign-in of a login.
pub fn first_visit(app: &App, login: &str) -> bool {
    if app.store.setting(login, SEEN).ok().flatten().is_some() {
        return false;
    }
    let _ = app
        .store
        .set_setting(login, SEEN, &chrono::Utc::now().to_rfc3339());
    true
}

/// The header shows "Get started" until the person says they are done.
pub fn pending(app: &App, login: &str) -> bool {
    app.store.setting(login, DONE).ok().flatten().as_deref() != Some("1")
}

/// The top of the home page for a visitor: what the site is, and how to
/// sign in.
pub fn intro(app: &App) -> Html {
    let yours = Html::from(if app.oauth.is_some() {
        "<a href=\"/auth/login\">Sign in with GitHub</a> to see"
    } else {
        "Signed in, you see"
    });
    html!(
        "<div class=\"Box mb-4\" id=\"intro\"><div class=\"Box-body\">\
         <h1 class=\"f3 mb-2\">Tools that make it easier to contribute to nixpkgs</h1>\
         <p>Maintainer Mode shows the packages, pull requests and advisories of nixpkgs, and what needs work. \
         {yours} what you maintain, what you watch and the packages of your machines.</p>\
         <p class=\"mb-0\">New to nixpkgs? Read the <a href=\"{CONTRIBUTING}\">nixpkgs contributor guide</a> first. \
         Then pick a package below.</p>{}</div></div>",
        crate::search::page_form("", "gap-2 mt-3")
    )
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mark {
    Done,
    Todo,
    Optional,
}

fn step(mark: Mark, title: &str, body: impl Into<Html>) -> Html {
    let body = body.into();
    let (class, ic, word) = match mark {
        Mark::Done => (
            "color-fg-success",
            icon(octicon!("check-circle-fill")),
            "done",
        ),
        Mark::Todo => ("color-fg-attention", icon(octicon!("circle")), "to do"),
        Mark::Optional => ("color-fg-muted", icon(octicon!("circle")), "optional"),
    };
    html!(
        "<div class=\"Box-row d-flex flex-items-start\">\
         <span class=\"{class} mr-2\" title=\"{word}\">{ic}</span>\
         <div class=\"flex-auto\"><div class=\"text-bold\">{title} <span class=\"Label ml-1\">{word}</span></div>\
         <div class=\"mt-1\">{body}</div></div></div>"
    )
}

/// What the checklist reads about one login. Only that login sees it.
struct Facts {
    index_ready: bool,
    maintained: usize,
    maintained_watched: usize,
    watched: usize,
    queue: usize,
    reviewed: usize,
    feed: bool,
}

impl Facts {
    fn of(app: &App, login: &str) -> Facts {
        let watched: HashSet<String> = app
            .store
            .watched(login)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let (index_ready, keys) = {
            let index = app.index.read().unwrap();
            let m = crate::catalog::maintained(&index, login);
            let keys: Vec<String> = m
                .packages
                .iter()
                .chain(&m.modules)
                .map(|(s, _)| s.key())
                .collect();
            (!index.rev.is_empty(), keys)
        };
        let s = app.snap();
        Facts {
            index_ready,
            maintained: keys.len(),
            maintained_watched: keys.iter().filter(|k| watched.contains(*k)).count(),
            watched: watched.len(),
            queue: crate::mine::for_you(app, &s, login).len(),
            reviewed: crate::dossier::build(&s.pr_data, login).reviewed,
            feed: app.store.feed_token(login).ok().flatten().is_some(),
        }
    }

    fn maintainer(&self) -> bool {
        self.maintained > 0
    }
}

fn mark(done: bool) -> Mark {
    if done { Mark::Done } else { Mark::Todo }
}

/// A person who maintains nothing yet learns how to start; a maintainer
/// gets help to keep up with what they maintain.
fn checklist(f: &Facts) -> Html {
    if !f.index_ready {
        return step(
            Mark::Todo,
            "What you maintain",
            "The first nixpkgs index is still being built. Come back in a few minutes.",
        );
    }
    if f.maintainer() {
        maintainer_steps(f)
    } else {
        newcomer_steps(f)
    }
}

fn newcomer_steps(f: &Facts) -> Html {
    let mut out = step(
        mark(f.reviewed > 0),
        "Make a first review",
        html!(
            "<a href=\"/review?tab=unowned\">Pull requests with no maintainer</a> wait the longest, and a first review needs no domain knowledge. \
             Is <code>meta</code> complete? Are the hashes pinned? Are <code>platforms</code> right? \
             Leave each finding as an inline comment on the changed line. \
             <a href=\"/state?tab=rules#2-whose-pull-requests-appear\">Your reviews also put your own pull requests in the queues.</a>"
        ),
    );
    out.push(&step(
        mark(f.watched > 0),
        "Pick a first package",
        "<a href=\"/#first\">These packages</a> are small, have no maintainer, and had work recently. \
         Watch one, fix or update it, then add yourself to its <code>meta.maintainers</code>.",
    ));
    let guides: Html = GUIDES
        .iter()
        .map(|(name, url, what)| html!("<li><a href=\"{url}\">{name}</a>: {what}</li>"))
        .collect();
    out.push(&step(
        Mark::Optional,
        "Learn how nixpkgs works",
        html!(
            "Maintainer Mode helps you find work. nixpkgs decides how the work is done.<ul class=\"ml-3 mt-1\">{guides}</ul>"
        ),
    ));
    out
}

fn maintainer_steps(f: &Facts) -> Html {
    let button = Html::from(if f.maintained_watched < f.maintained {
        "<form method=\"post\" action=\"/me/watch/maintained\" class=\"mt-2\">\
         <button class=\"btn btn-sm btn-primary\" type=\"submit\">Watch all of it</button></form>"
    } else {
        ""
    });
    let mut out = step(
        mark(f.maintained_watched == f.maintained),
        "Watch what you maintain",
        html!(
            "nixpkgs names you on <a href=\"/packages?filter=maintain\">{} packages and modules</a>. \
             You watch {} of them. Watching puts their pull requests, issues, advisories and releases in front of you.{button}",
            f.maintained,
            f.maintained_watched
        ),
    );
    out.push(&step(
        mark(f.reviewed > 0),
        "Review the pull requests on them",
        html!(
            "<a href=\"/review?tab=foryou\">{} open pull requests</a> touch what you maintain or watch. \
             You reviewed {} pull requests.",
            f.queue, f.reviewed
        ),
    ));
    out.push(&step(
        mark(f.feed),
        "Get the news",
        "<a href=\"/inbox\">Your inbox</a> shows new pull requests, merges, issues, advisories and releases of what you watch. \
         For a feed reader, make a private Atom feed in <a href=\"/me/settings#feed\">your settings</a>.",
    ));
    out
}

fn done_form(done: bool, label: &str) -> Html {
    html!(
        "<form method=\"post\" action=\"/me/start\"><input type=\"hidden\" name=\"done\" value=\"{}\">\
         <button class=\"btn btn-sm\" type=\"submit\">{label}</button></form>",
        if done { "1" } else { "" },
    )
}

/// The Get started card of the home page, until the person hides it.
pub fn card(app: &App, login: &str) -> Html {
    if !pending(app, login) {
        return Html::new();
    }
    let f = Facts::of(app, login);
    let (title, lead) = if f.maintainer() {
        ("Keep up with your packages", "")
    } else {
        (
            "Start contributing",
            "You do not need to maintain a package to help. ",
        )
    };
    html!(
        "<section class=\"Box mb-4\" id=\"start\"><div class=\"Box-header d-flex flex-items-center\">\
         <h2 class=\"Box-title flex-auto\">{title}</h2>{}</div>\
         <div class=\"Box-row f6 color-fg-muted\">{lead}Three steps, from your real state. Only you see this card.</div>{}</section>",
        done_form(true, "Hide"),
        checklist(&f)
    )
}

/// The `#start` section of `/me/settings`.
pub fn settings_box(app: &App, login: &str) -> Html {
    let (text, form) = if pending(app, login) {
        (
            "The home page shows the card with your first steps.",
            done_form(true, "Hide the card"),
        )
    } else {
        (
            "You hid the card with your first steps.",
            done_form(false, "Show the card again"),
        )
    };
    html!(
        "<div class=\"d-flex flex-wrap flex-items-center gap-2\"><span class=\"flex-auto\">{text}</span>{form}</div>"
    )
}

#[derive(Deserialize)]
struct DoneForm {
    #[serde(default)]
    done: String,
}

async fn save_done(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<DoneForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let v = if f.done == "1" { "1" } else { "" };
    if let Err(e) = app.store.set_setting(&login, DONE, v) {
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    Redirect::to(if v == "1" { "/" } else { "/#start" }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            index_ready: true,
            maintained: 0,
            maintained_watched: 0,
            watched: 0,
            queue: 0,
            reviewed: 0,
            feed: false,
        }
    }

    fn steps(h: &Html) -> usize {
        h.matches("class=\"Box-row d-flex flex-items-start\"")
            .count()
    }

    #[test]
    fn a_newcomer_learns_to_review_and_to_pick_a_first_package() {
        let html = checklist(&facts());
        assert_eq!(steps(&html), 3);
        assert!(html.contains("/review?tab=unowned"));
        assert!(html.contains("/#first"));
        assert!(!html.contains("/me/watch/maintained"));
        for (name, url, _) in GUIDES {
            assert!(html.contains(url), "{name} is missing");
        }
    }

    #[test]
    fn a_maintainer_gets_help_with_what_they_maintain() {
        let f = Facts {
            maintained: 3,
            maintained_watched: 1,
            ..facts()
        };
        let html = checklist(&f);
        assert_eq!(steps(&html), 3);
        assert!(html.contains("/packages?filter=maintain"));
        assert!(html.contains("/me/watch/maintained"));
        assert!(!html.contains("/review?tab=unowned"));
        let all = Facts {
            maintained_watched: 3,
            ..f
        };
        assert!(!checklist(&all).contains("/me/watch/maintained"));
    }

    #[test]
    fn the_checklist_reads_the_state_of_the_login() {
        let app = crate::sets::tests::app(&["hello"]);
        let f = Facts::of(&app, "alice");
        assert!(f.index_ready && !f.feed && f.watched == 0);
        app.store.set_feed_token("alice", "t").unwrap();
        app.store.watch("alice", "pkg:hello").unwrap();
        let f = Facts::of(&app, "alice");
        assert!(f.feed);
        assert_eq!(f.watched, 1);
        assert!(!Facts::of(&app, "bob").feed);
    }

    #[test]
    fn the_first_visit_is_the_first_only() {
        let app = crate::sets::tests::app(&[]);
        assert!(pending(&app, "alice"));
        assert!(first_visit(&app, "alice"));
        assert!(!first_visit(&app, "alice"));
        app.store.set_setting("alice", DONE, "1").unwrap();
        assert!(!pending(&app, "alice"));
    }

    #[test]
    fn the_card_shows_until_it_is_hidden() {
        let app = crate::sets::tests::app(&[]);
        let card = card(&app, "alice");
        assert!(card.contains("id=\"start\""));
        assert!(card.contains("action=\"/me/start\""));
        assert!(settings_box(&app, "alice").contains("Hide the card"));
        app.store.set_setting("alice", DONE, "1").unwrap();
        assert!(super::card(&app, "alice").is_empty());
        assert!(settings_box(&app, "alice").contains("Show the card again"));
    }
}
