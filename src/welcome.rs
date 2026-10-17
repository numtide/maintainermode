//! The first steps: a short introduction on the home page for visitors, and
//! a checklist for a person who signed in, computed from their own state.

use axum::Router;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use std::collections::HashSet;

use crate::ui::{Page, Section};
use crate::web::{App, Shared, esc, icon, octicon};

const DONE: &str = "welcome_done";
const SEEN: &str = "welcome_at";
const INTRO_COOKIE: &str = "intro";

pub const CONTRIBUTING: &str = "https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md";
const MAINTAINERS_README: &str =
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
        .route("/welcome", get(welcome_page).post(save_done))
        .route("/intro/dismiss", post(dismiss_intro))
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

pub fn intro(app: &App, headers: &HeaderMap) -> String {
    if crate::web::cookie(headers, INTRO_COOKIE).as_deref() == Some("hidden") {
        return String::new();
    }
    let yours = if app.oauth.is_some() {
        "<a href=\"/auth/login\">Sign in with GitHub</a> to see"
    } else {
        "Signed in, you see"
    };
    format!(
        "<div class=\"Box mb-4\" id=\"intro\"><div class=\"Box-body\">\
         <div class=\"d-flex flex-items-start\"><h2 class=\"f3 flex-auto\">Tools that make it easier to contribute to nixpkgs</h2>\
         <form method=\"post\" action=\"/intro/dismiss\"><button class=\"btn btn-sm\" type=\"submit\">Hide</button></form></div>\
         <ul class=\"ml-3 my-2\">\
         <li><strong>Navigate nixpkgs.</strong> <a href=\"/packages\">Packages and NixOS modules</a> with their issues, pull requests, version bumps and advisories, \
         and <a href=\"/state\">the state of nixpkgs</a> in numbers.</li>\
         <li><strong>Make it yours.</strong> {yours} what you maintain and watch, a private feed, and the packages of your own system.</li>\
         <li><strong>Act on it.</strong> <a href=\"/review\">Review the pull requests that need it</a>, \
         and start nixpkgs-review or nix-update in your own GitHub Actions.</li></ul>\
         <p class=\"mb-0\">New to nixpkgs? Read the <a href=\"{CONTRIBUTING}\">nixpkgs contributor guide</a> first. \
         Maintainer Mode helps you find work; nixpkgs decides how the work is done.</p></div></div>"
    )
}

async fn dismiss_intro(State(app): State<Shared>) -> Response {
    (
        [(
            header::SET_COOKIE,
            crate::web::set_cookie(&app, INTRO_COOKIE, "hidden", 365 * 86400),
        )],
        Redirect::to("/"),
    )
        .into_response()
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mark {
    Done,
    Todo,
    Optional,
}

fn step(mark: Mark, title: &str, body: &str) -> String {
    let (class, ic, word) = match mark {
        Mark::Done => (
            "color-fg-success",
            icon(octicon!("check-circle-fill")),
            "done",
        ),
        Mark::Todo => ("color-fg-attention", icon(octicon!("circle")), "to do"),
        Mark::Optional => ("color-fg-muted", icon(octicon!("circle")), "optional"),
    };
    format!(
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
    credited: usize,
    feed: bool,
    inbox: bool,
    system: bool,
    actions: bool,
    llm: bool,
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
        let setting = |k: &str| {
            app.store
                .setting(login, k)
                .ok()
                .flatten()
                .is_some_and(|v| !v.is_empty())
        };
        Facts {
            index_ready,
            maintained: keys.len(),
            maintained_watched: keys.iter().filter(|k| watched.contains(*k)).count(),
            watched: watched.len(),
            queue: crate::web::review_bucket(
                &s,
                Some(login),
                &watched.iter().cloned().collect::<Vec<_>>(),
                "foryou",
            )
            .len(),
            reviewed: crate::dossier::build(&s.pr_data, login).reviewed,
            credited: s.ledger.credits_of(login).count(),
            feed: app.store.feed_token(login).ok().flatten().is_some(),
            inbox: true,
            system: app.store.package_set(login).ok().flatten().is_some()
                || !app.store.flake_refs(login).unwrap_or_default().is_empty(),
            actions: ["review_repo", "update_repo", "nixpkgs_fork"]
                .iter()
                .any(|k| setting(k)),
            llm: crate::llm::opted_in(app, login),
        }
    }
}

fn mark(done: bool) -> Mark {
    if done { Mark::Done } else { Mark::Todo }
}

fn checklist(f: &Facts, login: &str) -> String {
    let l = esc(login);
    let mut out = String::from("<div class=\"Box mb-4\">");
    if !f.index_ready {
        out.push_str(&step(
            Mark::Todo,
            "What you maintain",
            "The first nixpkgs index is still being built. Come back in a few minutes.",
        ));
    } else if f.maintained == 0 {
        out.push_str(&step(
            mark(f.reviewed > 0),
            "Start as a reviewer",
            &format!(
                "No package or module names you in <code>meta.maintainers</code> yet. \
                 Start with <a href=\"/review?bucket=unowned\">the unowned pull requests</a>: nobody watches them, \
                 and a first review needs no domain knowledge. Is <code>meta</code> complete? Are the hashes pinned? \
                 Are <code>platforms</code> right? Leave each finding as an inline comment on the changed line. \
                 To maintain a package later, read <a href=\"{MAINTAINERS_README}\">maintainers/README.md</a>."
            ),
        ));
    } else {
        let button = if f.maintained_watched < f.maintained {
            "<form method=\"post\" action=\"/watch/maintained\" class=\"mt-2\">\
             <button class=\"btn btn-sm btn-primary\" type=\"submit\">Subscribe to all of it</button></form>"
        } else {
            ""
        };
        out.push_str(&step(
            mark(f.maintained_watched == f.maintained),
            "What you maintain",
            &format!(
                "nixpkgs names you on <a href=\"/u/{l}/maintains\">{} packages and modules</a>. \
                 You watch {} of them.{button}",
                f.maintained, f.maintained_watched
            ),
        ));
    }
    out.push_str(&step(
        mark(f.watched > 0),
        "What you watch",
        &format!(
            "You watch {}. Subscribe to any package, module or nixpkgs directory.\
             <form method=\"post\" action=\"/watch\" class=\"inline-form mt-2\">\
             <input class=\"form-control flex-auto input-sm\" name=\"name\" placeholder=\"hello, nixos/nginx or pkgs/development/python-modules\" aria-label=\"what to subscribe to\">\
             <button class=\"btn btn-sm\" type=\"submit\">Subscribe</button></form>",
            f.watched
        ),
    ));
    out.push_str(&step(
        mark(f.credited > 0),
        "The review queue for you",
        &format!(
            "<a href=\"/review\">{} pull requests</a> touch what you maintain, own, changed this year or tagged. \
             A review earns credit when it moves a pull request. You have {} credited reviews.",
            f.queue, f.credited
        ),
    ));
    let inbox = if f.inbox {
        " Your inbox on this site shows the same news."
    } else {
        ""
    };
    out.push_str(&step(
        mark(f.feed),
        "Notifications",
        &format!(
            "Make a private Atom feed on <a href=\"/watch#feed\">/watch</a>. It tells you about new pull requests, merges, \
             issues, advisories and releases of what you watch.{inbox}"
        ),
    ));
    out.push_str(&step(
        mark(f.system),
        "Your system",
        "Upload the closure of your machines on <a href=\"/me/sets\">/me/sets</a>. \
         Your feed then tells you about advisories and releases of the packages you really run. It stays private.",
    ));
    out.push_str(&step(
        mark(f.actions),
        "Actions",
        "Fork <a href=\"https://github.com/Defelo/nixpkgs-review-gha\">nixpkgs-review-gha</a>, install the GitHub App on it, \
         and save your repositories on <a href=\"/actions\">/actions</a>. \
         Then \"Review in CI\" and \"Update with nix-update\" run in your own GitHub Actions.",
    ));
    out.push_str(&step(
        if f.llm { Mark::Done } else { Mark::Optional },
        "LLMs",
        &format!(
            "Off by default. Turn them on in <a href=\"/me/settings\">your settings</a> to get a Claude Code button on \
             pull requests and an MCP server. Follow the <a href=\"{CONTRIBUTING}#automationai-policy\">nixpkgs policy on AI</a>."
        ),
    ));
    let guides: String = GUIDES
        .iter()
        .map(|(name, url, what)| format!("<li><a href=\"{url}\">{name}</a>: {what}</li>"))
        .collect();
    out.push_str(&step(
        Mark::Optional,
        "Read the nixpkgs guides",
        &format!(
            "Maintainer Mode helps you find work. nixpkgs decides how the work is done.<ul class=\"ml-3 mt-1\">{guides}</ul>"
        ),
    ));
    out.push_str("</div>");
    out
}

async fn welcome_page(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let done = !pending(&app, &login);
    let button = format!(
        "<form method=\"post\" action=\"/welcome\"><input type=\"hidden\" name=\"done\" value=\"{}\">\
         <button class=\"btn btn-sm\" type=\"submit\">{}</button></form>",
        if done { "" } else { "1" },
        if done {
            "Show \"Get started\" again"
        } else {
            "I am done, hide \"Get started\""
        }
    );
    Page::new(Section::Home, "Get started")
        .sub("Your first steps, from your real state. Come back to this page at any time. Only you see it.")
        .actions(button)
        .body(checklist(&Facts::of(&app, &login), &login))
        .render(&app, Some(&login))
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
    Redirect::to(if v == "1" { "/" } else { "/welcome" }).into_response()
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
            credited: 0,
            feed: false,
            inbox: false,
            system: false,
            actions: false,
            llm: false,
        }
    }

    #[test]
    fn a_newcomer_gets_the_unowned_queue_and_the_maintainer_guide() {
        let html = checklist(&facts(), "alice");
        assert!(html.contains("/review?bucket=unowned"));
        assert!(html.contains(MAINTAINERS_README));
        assert!(!html.contains("/watch/maintained"));
        assert!(!html.contains("inbox"));
        for (name, url, _) in GUIDES {
            assert!(html.contains(url), "{name} is missing");
        }
    }

    #[test]
    fn a_maintainer_can_subscribe_to_the_rest() {
        let f = Facts {
            maintained: 3,
            maintained_watched: 1,
            inbox: true,
            ..facts()
        };
        let html = checklist(&f, "alice");
        assert!(html.contains("/u/alice/maintains"));
        assert!(html.contains("/watch/maintained"));
        assert!(html.contains("inbox"));
        let all = Facts {
            maintained_watched: 3,
            ..f
        };
        assert!(!checklist(&all, "alice").contains("/watch/maintained"));
    }

    #[test]
    fn the_checklist_reads_the_state_of_the_login() {
        let app = crate::sets::tests::app(&["hello"]);
        let f = Facts::of(&app, "alice");
        assert!(f.index_ready && !f.feed && !f.llm && !f.actions && f.inbox);
        app.store.set_feed_token("alice", "t").unwrap();
        app.store.watch("alice", "pkg:hello").unwrap();
        app.store
            .set_setting("alice", "review_repo", "alice/r")
            .unwrap();
        let f = Facts::of(&app, "alice");
        assert!(f.feed && f.actions);
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
}
