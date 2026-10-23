//! The JSON API, and the one table of what the site, the API, the MCP
//! server and the CLI can do. [`ENDPOINTS`] gives each endpoint with its
//! parameters, its MCP tool and its CLI command: `GET /api/v1`, the routes
//! of the JSON answers and the MCP `tools/list` are made from it, and
//! [`answer`] builds the JSON for the API and the MCP server both.
//! [`PAGES`] maps each view of the site to its endpoints. The tests at the
//! bottom fail when a page, a tab, an endpoint or a tool is not in them.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::web::{App, Shared};

pub(crate) const LIMIT: usize = 50;
pub(crate) const MAX_LIMIT: usize = 200;
pub(crate) const QUEUES: [&str; 6] = ["foryou", "unowned", "firsttime", "bumps", "all", "ready"];
pub(crate) const SECTIONS: [&str; 6] = [
    "ready",
    "yours",
    "maintainer_approved",
    "security",
    "backports",
    "waiting",
];
pub(crate) const MOODS: [&str; 5] = ["all", "mine", "machines", "review", "maintain"];
pub(crate) const FILTERS: [&str; 4] = ["mine", "machines", "maintain", "watch"];
const KINDS: [&str; 6] = ["mine", "pr", "issue", "advisory", "release", "build"];

#[derive(Clone, Copy)]
pub(crate) enum Type {
    Text,
    Int,
    OneOf(&'static [&'static str]),
}

/// A query parameter, or a path parameter when the path names it. The MCP
/// tool takes it as an argument of the same name.
pub(crate) struct Param {
    pub name: &'static str,
    pub kind: Type,
    pub required: bool,
    pub about: &'static str,
}

const fn opt(name: &'static str, kind: Type, about: &'static str) -> Param {
    Param {
        name,
        kind,
        required: false,
        about,
    }
}

const fn need(name: &'static str, kind: Type, about: &'static str) -> Param {
    Param {
        name,
        kind,
        required: true,
        about,
    }
}

const LIMIT_P: Param = opt(
    "limit",
    Type::Int,
    "rows per page: 50 by default, 200 at most",
);
const CURSOR_P: Param = opt("cursor", Type::Text, "the next_cursor of the page before");

/// What gives a capability, or why nothing does.
pub(crate) enum Cover<T> {
    By(T),
    Without(&'static str),
}
use Cover::{By, Without};

pub(crate) struct Endpoint {
    /// An axum path: `:name` and `*name` are parameters.
    pub path: &'static str,
    pub about: &'static str,
    pub params: &'static [Param],
    /// It answers only for the caller: their session, or their MCP token.
    pub private: bool,
    /// The name and the title of the MCP tool. An endpoint with a tool gets
    /// its route and its JSON from this module.
    pub mcp: Cover<(&'static str, &'static str)>,
    /// The CLI command; `get` prints the JSON of any GET endpoint.
    pub cli: Cover<&'static str>,
}

pub(crate) const ENDPOINTS: &[Endpoint] = &[
    Endpoint {
        path: "/api/v1/prs",
        about: "Open pull requests in one queue, paged. foryou: what login maintains, owns, changed or tagged; for your own login, also the packages of your machines and what you watch, in the order of the page. unowned: no maintainer. firsttime: first contributions with no review yet, the oldest first. bumps: version bumps; with login, only what login maintains. all: every pull request that needs review. ready: ready to merge; section picks a list of the committer view.",
        params: &[
            opt("queue", Type::OneOf(&QUEUES), "the queue: all by default"),
            opt(
                "section",
                Type::OneOf(&SECTIONS),
                "with queue=ready: ready by default; yours needs your own login and a committer",
            ),
            opt(
                "login",
                Type::Text,
                "whose queue, for foryou and bumps; for foryou, the caller by default",
            ),
            LIMIT_P,
            CURSOR_P,
        ],
        private: false,
        mcp: By(("prs", "Open pull requests")),
        cli: By("prs"),
    },
    Endpoint {
        path: "/api/v1/prs/:number",
        about: "One pull request: its state, its reviews, its place in the queues, the packages and modules it touches, its files, its discussion and the merge decision.",
        params: &[need("number", Type::Int, "the number of the pull request")],
        private: false,
        mcp: By(("pr", "A pull request")),
        cli: By("pr"),
    },
    Endpoint {
        path: "/api/v1/people/:login",
        about: "The public profile of a GitHub login: facts (committer, teams, first pull request), its open and recent pull requests, the areas it changes, its public flake sets, its review record, and the packages and NixOS modules whose meta.maintainers or meta.teams name it, with their flags. It never ranks people.",
        params: &[need("login", Type::Text, "a GitHub login")],
        private: false,
        mcp: By(("person", "A person")),
        cli: By("record"),
    },
    Endpoint {
        path: "/api/v1/needs",
        about: "Your Needs you list, the most urgent first: advisories on your machines, reviews and reminders on your own pull requests, pull requests to review, packages to update. It also gives the follow-up plan of each of your open pull requests, and the events of your own pull requests in the last 30 days.",
        params: &[opt(
            "mood",
            Type::OneOf(&MOODS),
            "keep the items of one mood: all by default",
        )],
        private: true,
        mcp: By(("needs", "What needs me")),
        cli: By("needs"),
    },
    Endpoint {
        path: "/api/v1/mine",
        about: "Your packages, as the filters of /packages show them: mine (all of them), machines (the packages of your sets), maintain, or watch. The packages of your machines come first, then what you maintain, then what you watch. In each group, what needs attention comes first. Each row has its health flags and why it is in the list.",
        params: &[
            opt("filter", Type::OneOf(&FILTERS), "mine by default"),
            opt(
                "set",
                Type::Text,
                "with filter=machines: the name of one of your sets",
            ),
            opt(
                "output",
                Type::Text,
                "with set: an output label of that set, like web-1; keep the packages of that output",
            ),
            opt("q", Type::Text, "keep the rows whose name holds this text"),
            LIMIT_P,
            CURSOR_P,
        ],
        private: true,
        mcp: By(("my_packages", "My packages")),
        cli: By("mine"),
    },
    Endpoint {
        path: "/api/v1/sets",
        about: "Your named package sets: machine reports and flakes, private and public, with their packages, their source, their expiry and the state of their last scan. A set that you sent with --with-outputs also has its output labels, such as host names, and the outputs that hold each package.",
        params: &[
            opt(
                "set",
                Type::Text,
                "the name of one of your sets, like infra",
            ),
            opt(
                "output",
                Type::Text,
                "with set: an output label of that set, like web-1; keep the packages of that output",
            ),
        ],
        private: true,
        mcp: By(("my_sets", "My package sets")),
        cli: By("get"),
    },
    Endpoint {
        path: "/api/v1/inbox",
        about: "Your inbox, the newest first: the events of what you watch, of the packages of your sets and of your own pull requests, with the unread count. Reading it marks nothing read.",
        params: &[
            opt("kind", Type::OneOf(&KINDS), "keep the events of one kind"),
            opt(
                "sub",
                Type::Text,
                "keep the events of one subject, like pkg:hello",
            ),
            LIMIT_P,
            CURSOR_P,
        ],
        private: true,
        mcp: By(("inbox", "My inbox")),
        cli: By("get"),
    },
    Endpoint {
        path: "/api/v1/progress",
        about: "Your own progress: your reviews this week and this month, your weekly goal, the authors your reviews helped, and your badges. It compares you with nobody.",
        params: &[],
        private: true,
        mcp: By(("progress", "My progress")),
        cli: By("get"),
    },
    Endpoint {
        path: "/api/v1/search",
        about: "What the search box of the site finds: packages, NixOS modules and areas of the tree with their health flags, maintainers, pull requests whose title has the words, and advisories with that id. `page` is the page the site opens at once, if any.",
        params: &[
            need(
                "q",
                Type::Text,
                "an attribute, a package name, nixos/NAME, a module file, a path, @login, #NUMBER, words of a pull request title, or a CVE, GHSA or NIXPKGS id",
            ),
            opt(
                "limit",
                Type::Int,
                "at most this many of each kind; the search box asks for 5",
            ),
        ],
        private: false,
        mcp: By((
            "search",
            "Search packages, modules, areas, people, pull requests and advisories",
        )),
        cli: By("search"),
    },
    Endpoint {
        path: "/api/v1/first",
        about: "Packages for a first contribution: small packages under pkgs/by-name with no maintainer, and a pull request to review or an issue to fix from the last 60 days. The newest task comes first.",
        params: &[LIMIT_P, CURSOR_P],
        private: false,
        mcp: By(("first_packages", "Packages for a first contribution")),
        cli: By("first"),
    },
    Endpoint {
        path: "/api/v1/need-help",
        about: "Packages that need help: an open advisory in master, the highest risk first.",
        params: &[LIMIT_P, CURSOR_P],
        private: false,
        mcp: By(("need_help", "Packages that need help")),
        cli: By("get"),
    },
    Endpoint {
        path: "/api/v1/packages/index",
        about: "The public package list of the current index: pname and version of each attribute, gzip.",
        params: &[],
        private: false,
        mcp: Without(
            "the full package list for maintainer-mode export: it is too large for one answer; search finds one package",
        ),
        cli: By("export"),
    },
    Endpoint {
        path: "/api/v1/packages/:name",
        about: "One nixpkgs package: version, newest upstream release, last Hydra build, open advisories, maintainers, and its issues and pull requests.",
        params: &[need(
            "name",
            Type::Text,
            "an attribute or a package name, like hello or python3Packages.requests",
        )],
        private: false,
        mcp: By(("package", "A package")),
        cli: By("show"),
    },
    Endpoint {
        path: "/api/v1/modules/*file",
        about: "One NixOS module: its NixOS test on Hydra, maintainers, and its issues and pull requests.",
        params: &[need(
            "file",
            Type::Text,
            "the module file, like nixos/modules/services/web-servers/nginx/default.nix, or nixos/NAME",
        )],
        private: false,
        mcp: By(("module", "A NixOS module")),
        cli: By("show"),
    },
    Endpoint {
        path: "/api/v1/areas/*path",
        about: "A path of the nixpkgs tree: the packages and modules under it with their flags, and its issues and pull requests.",
        params: &[need(
            "path",
            Type::Text,
            "a nixpkgs path, like pkgs/development/python-modules",
        )],
        private: false,
        mcp: By(("area", "An area of nixpkgs")),
        cli: By("show"),
    },
    Endpoint {
        path: "/api/v1/find/*name",
        about: "Redirects a name to its package, module or area.",
        params: &[need("name", Type::Text, "a package, nixos/NAME or a path")],
        private: false,
        mcp: Without("a redirect: the tools package, module and area take a name"),
        cli: By("show"),
    },
    Endpoint {
        path: "/api/v1/metrics",
        about: "The state of nixpkgs, month by month: pull request flow, review times and package health. Aggregates only: no figure names a person.",
        params: &[],
        private: false,
        mcp: By(("metrics", "The state of nixpkgs")),
        cli: By("metrics"),
    },
    Endpoint {
        path: "/api/v1/status",
        about: "The counts and the rules of the queues.",
        params: &[],
        private: false,
        mcp: By(("status", "The rules of the queues")),
        cli: By("get"),
    },
    Endpoint {
        path: "/api/v1/sync",
        about: "What the server has read from GitHub and the other sources, and how fresh it is.",
        params: &[],
        private: false,
        mcp: By(("sync_status", "Sync status")),
        cli: By("get"),
    },
    Endpoint {
        path: "/api/v1/events",
        about: "The event log, as Server-Sent Events.",
        params: &[
            opt("type", Type::Text, "keep one event type"),
            opt("number", Type::Int, "keep one issue or pull request"),
            opt("actor", Type::Text, "keep one login"),
            opt("after", Type::Int, "start after this event id"),
        ],
        private: false,
        mcp: Without("a stream of Server-Sent Events: an MCP answer is one JSON message"),
        cli: Without("a stream: read it with curl -N"),
    },
    Endpoint {
        path: "/api/v1/archive/:owner/:repo/:number",
        about: "One stored issue or pull request of a public repository that the server archives.",
        params: &[
            need(
                "owner",
                Type::Text,
                "the owner of the repository, like NixOS",
            ),
            need("repo", Type::Text, "the repository, like nixpkgs"),
            need(
                "number",
                Type::Int,
                "the number of the issue or pull request",
            ),
        ],
        private: false,
        mcp: By(("archive_item", "An archived issue or pull request")),
        cli: By("get"),
    },
    Endpoint {
        path: "POST /api/v1/reports",
        about: "Uploads a strict package report, with an upload token.",
        params: &[],
        private: false,
        mcp: Without("it writes, and the MCP server only reads"),
        cli: By("submit"),
    },
    Endpoint {
        path: "POST /api/v1/login",
        about: "Starts a `maintainer-mode login`: a device code and a user code.",
        params: &[],
        private: false,
        mcp: Without("it makes an upload token, and the MCP server only reads"),
        cli: By("login"),
    },
    Endpoint {
        path: "POST /api/v1/login/poll",
        about: "The upload token, once the person confirms the code on /login/cli.",
        params: &[],
        private: false,
        mcp: Without("it gives an upload token, and the MCP server only reads"),
        cli: By("login"),
    },
];

/// Each view of the site: a route, with a tab, a filter or a section. A
/// target is an endpoint path with sample parameters; `<x>` is a value.
pub(crate) const PAGES: &[(&str, Cover<&[&str]>)] = &[
    (
        "/",
        By(&[
            "/api/v1/needs",
            "/api/v1/inbox",
            "/api/v1/mine?filter=mine",
            "/api/v1/people/:login",
            "/api/v1/first",
            "/api/v1/prs?queue=unowned",
            "/api/v1/prs?queue=ready",
            "/api/v1/need-help",
            "/api/v1/metrics",
        ]),
    ),
    ("/?mood=all", By(&["/api/v1/needs?mood=all"])),
    ("/?mood=mine", By(&["/api/v1/needs?mood=mine"])),
    ("/?mood=machines", By(&["/api/v1/needs?mood=machines"])),
    ("/?mood=review", By(&["/api/v1/needs?mood=review"])),
    ("/?mood=maintain", By(&["/api/v1/needs?mood=maintain"])),
    ("/#first", By(&["/api/v1/first"])),
    ("/review", By(&["/api/v1/prs", "/api/v1/progress"])),
    ("/review?tab=foryou", By(&["/api/v1/prs?queue=foryou"])),
    ("/review?tab=unowned", By(&["/api/v1/prs?queue=unowned"])),
    (
        "/review?tab=firsttime",
        By(&["/api/v1/prs?queue=firsttime"]),
    ),
    ("/review?tab=bumps", By(&["/api/v1/prs?queue=bumps"])),
    (
        "/review?tab=bumps&filter=mine",
        By(&["/api/v1/prs?queue=bumps&login=<login>"]),
    ),
    ("/review?tab=all", By(&["/api/v1/prs?queue=all"])),
    ("/review?tab=ready", By(&["/api/v1/prs?queue=ready"])),
    (
        "/review?tab=ready&section=yours",
        By(&["/api/v1/prs?queue=ready&section=yours"]),
    ),
    (
        "/review?tab=ready&section=maintainer_approved",
        By(&["/api/v1/prs?queue=ready&section=maintainer_approved"]),
    ),
    (
        "/review?tab=ready&section=security",
        By(&["/api/v1/prs?queue=ready&section=security"]),
    ),
    (
        "/review?tab=ready&section=backports",
        By(&["/api/v1/prs?queue=ready&section=backports"]),
    ),
    (
        "/review?tab=ready&section=waiting",
        By(&["/api/v1/prs?queue=ready&section=waiting"]),
    ),
    ("/state", By(&["/api/v1/metrics"])),
    ("/state?tab=rules", By(&["/api/v1/status"])),
    (
        "/state?tab=docs",
        Without("the docs are the Markdown files in docs/ of the repository"),
    ),
    ("/state?tab=status", By(&["/api/v1/sync"])),
    ("/packages", By(&["/api/v1/search", "/api/v1/mine"])),
    ("/packages?q=", By(&["/api/v1/search?q=<name>"])),
    ("/search", By(&["/api/v1/search"])),
    ("/search?q=", By(&["/api/v1/search?q=<words>"])),
    ("/packages?filter=mine", By(&["/api/v1/mine?filter=mine"])),
    (
        "/packages?filter=machines",
        By(&["/api/v1/mine?filter=machines", "/api/v1/sets"]),
    ),
    (
        "/packages?filter=machines&set=",
        By(&[
            "/api/v1/mine?filter=machines&set=<name>",
            "/api/v1/sets?set=<name>",
        ]),
    ),
    (
        "/packages?filter=machines&set=&output=",
        By(&[
            "/api/v1/mine?filter=machines&set=<name>&output=<label>",
            "/api/v1/sets?set=<name>&output=<label>",
        ]),
    ),
    (
        "/packages?filter=maintain",
        By(&["/api/v1/mine?filter=maintain"]),
    ),
    ("/packages?filter=watch", By(&["/api/v1/mine?filter=watch"])),
    ("/packages?filter=all", By(&["/api/v1/search"])),
    ("/p/:attr", By(&["/api/v1/packages/:name"])),
    ("/m/*file", By(&["/api/v1/modules/*file"])),
    ("/a/*path", By(&["/api/v1/areas/*path"])),
    ("/u/:login", By(&["/api/v1/people/:login"])),
    ("/u/:login?tab=reviews", By(&["/api/v1/people/:login"])),
    ("/u/:login?tab=maintains", By(&["/api/v1/people/:login"])),
    ("/u/:login?tab=flakes", By(&["/api/v1/people/:login"])),
    ("/pr/:number", By(&["/api/v1/prs/:number", "/api/v1/needs"])),
    ("/inbox", By(&["/api/v1/inbox"])),
    ("/inbox?sub=", By(&["/api/v1/inbox?sub=<key>"])),
    ("/inbox?kind=mine", By(&["/api/v1/inbox?kind=mine"])),
    ("/inbox?kind=pr", By(&["/api/v1/inbox?kind=pr"])),
    ("/inbox?kind=issue", By(&["/api/v1/inbox?kind=issue"])),
    ("/inbox?kind=advisory", By(&["/api/v1/inbox?kind=advisory"])),
    ("/inbox?kind=release", By(&["/api/v1/inbox?kind=release"])),
    ("/inbox?kind=build", By(&["/api/v1/inbox?kind=build"])),
    ("/me/progress", By(&["/api/v1/progress"])),
    (
        "/me/settings",
        By(&["/api/v1/sets", "/api/v1/progress", "/api/v1/needs"]),
    ),
    (
        "/me/settings#email",
        Without("your address and its choices: settings that only the page changes"),
    ),
    ("/me/settings#followup", By(&["/api/v1/needs"])),
    (
        "/me/settings#feed",
        Without("the secret address of your Atom feed; /api/v1/inbox gives its events"),
    ),
    ("/me/settings#upload", By(&["/api/v1/sets"])),
    ("/me/settings#llm", Without("the LLM choice itself")),
    (
        "/me/settings#mcp",
        Without("the MCP token: the server shows it once and keeps only its hash"),
    ),
    (
        "/me/settings#workflows",
        Without("the repositories of your workflow buttons: settings that only the page changes"),
    ),
    (
        "/me/settings#tags",
        Without("a setting; /api/v1/prs?queue=foryou with your own login uses it"),
    ),
    ("/me/settings#goal", By(&["/api/v1/progress"])),
    ("/me/settings#start", Without("the steps of Get started")),
    (
        "/me/runs",
        Without(
            "the runs of your workflow repository: the page reads them from GitHub with the token of your session, which the API and the MCP server do not hold",
        ),
    ),
    (
        "/actions",
        Without("the sign-in page of the workflow buttons; it has no data"),
    ),
    (
        "/claude/pr/:number",
        Without("a link to Claude Code on the web; its prompt names the MCP server"),
    ),
    (
        "/confirm/:token",
        Without("the link that confirms an email address; it changes a setting"),
    ),
    (
        "/unsubscribe/:token",
        Without("the link that stops the email digest; it changes a setting"),
    ),
    (
        "/login/cli",
        Without("confirms the code of `maintainer-mode login`; it makes an upload token"),
    ),
];

/// The index shows `<name>` for a path parameter.
fn display(path: &str) -> String {
    path.split('/')
        .map(
            |s| match s.strip_prefix(':').or_else(|| s.strip_prefix('*')) {
                Some(n) => format!("<{n}>"),
                None => s.to_string(),
            },
        )
        .collect::<Vec<_>>()
        .join("/")
}

fn param_json(p: &Param) -> Value {
    let mut v = json!({ "name": p.name, "required": p.required, "description": p.about });
    match p.kind {
        Type::Text => v["type"] = json!("string"),
        Type::Int => v["type"] = json!("integer"),
        Type::OneOf(values) => {
            v["type"] = json!("string");
            v["values"] = json!(values);
        }
    }
    v
}

async fn index() -> Json<Value> {
    let endpoints: Vec<Value> = ENDPOINTS
        .iter()
        .map(|e| {
            json!({
                "path": display(e.path),
                "description": e.about,
                "parameters": e.params.iter().map(param_json).collect::<Vec<_>>(),
                "private": e.private,
                "mcp_tool": match e.mcp { By((name, _)) => Some(name), Without(_) => None },
                "cli": match e.cli { By(c) => Some(format!("maintainer-mode {c}")), Without(_) => None },
            })
        })
        .collect();
    let pages: Vec<Value> = PAGES
        .iter()
        .map(|(page, cover)| match cover {
            By(list) => {
                json!({ "page": page, "api": list.iter().map(|t| display(t)).collect::<Vec<_>>() })
            }
            Without(why) => json!({ "page": page, "api": [], "why": why }),
        })
        .collect();
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "paging": format!(
            "lists return at most limit items (default {LIMIT}, at most {MAX_LIMIT}); pass next_cursor as cursor for the next page"
        ),
        "private": "a private endpoint answers only for you: send your session cookie, or Authorization: Bearer <your MCP token>",
        "endpoints": endpoints,
        "pages": pages,
    }))
}

/// The MCP `inputSchema` of an endpoint.
pub(crate) fn schema(e: &Endpoint) -> Value {
    let mut properties = serde_json::Map::new();
    for p in e.params {
        let mut v = param_json(p);
        let o = v.as_object_mut().unwrap();
        o.remove("name");
        o.remove("required");
        if let Some(values) = o.remove("values") {
            o.insert("enum".into(), values);
        }
        if p.name == "limit" {
            o.insert("minimum".into(), json!(1));
            o.insert("maximum".into(), json!(MAX_LIMIT));
        }
        properties.insert(p.name.into(), v);
    }
    let required: Vec<&str> = e
        .params
        .iter()
        .filter(|p| p.required)
        .map(|p| p.name)
        .collect();
    json!({ "type": "object", "properties": properties, "required": required })
}

pub(crate) fn tool(name: &str) -> Option<&'static Endpoint> {
    ENDPOINTS
        .iter()
        .find(|e| matches!(e.mcp, By((n, _)) if n == name))
}

/// Paths that answer only for the caller, for `Cache-Control`.
pub(crate) fn is_private(path: &str) -> bool {
    ENDPOINTS.iter().any(|e| e.private && e.path == path)
}

/// The parameters of every endpoint, by name: the query and the path of a
/// request, or the arguments of an MCP tool call.
#[derive(Debug, Default, Deserialize, Serialize)]
pub(crate) struct Args {
    pub queue: Option<String>,
    pub section: Option<String>,
    pub login: Option<String>,
    pub mood: Option<String>,
    pub filter: Option<String>,
    pub set: Option<String>,
    pub output: Option<String>,
    pub q: Option<String>,
    pub kind: Option<String>,
    pub sub: Option<String>,
    pub name: Option<String>,
    pub file: Option<String>,
    pub path: Option<String>,
    pub owner: Option<String>,
    pub repo: Option<String>,
    pub number: Option<u64>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

impl Args {
    fn text(o: &Option<String>) -> Option<&str> {
        o.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }
}

/// The required parameters and the values of each `OneOf`, from the table.
fn check(e: &Endpoint, a: &Args) -> Result<(), String> {
    let v = serde_json::to_value(a).map_err(|e| e.to_string())?;
    for p in e.params {
        let given = &v[p.name];
        let given = match given {
            Value::String(s) if s.trim().is_empty() => &Value::Null,
            g => g,
        };
        if given.is_null() {
            if p.required {
                return Err(format!("{} is missing", p.name));
            }
            continue;
        }
        if let (Type::OneOf(values), Some(s)) = (p.kind, given.as_str()) {
            if !values.contains(&s) {
                return Err(format!(
                    "no {} {s}; use one of {}",
                    p.name,
                    values.join(", ")
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) enum Fail {
    Bad(String),
    Missing(String),
    /// A pull request that is not in the synced data: its GitHub URL helps.
    Unsynced(u64),
    SignIn,
    Failed(String),
}

impl Fail {
    pub(crate) fn text(self) -> String {
        match self {
            Fail::Bad(m) | Fail::Missing(m) | Fail::Failed(m) => m,
            Fail::Unsynced(n) => crate::prpage::not_in_data(n),
            Fail::SignIn => "sign in first, or send your MCP token".into(),
        }
    }

    fn json(self) -> Value {
        match self {
            Fail::Unsynced(n) => {
                json!({ "error": Fail::Unsynced(n).text(), "url": crate::prpage::github_url(n) })
            }
            f => json!({ "error": f.text() }),
        }
    }

    fn code(&self) -> StatusCode {
        match self {
            Fail::Bad(_) => StatusCode::BAD_REQUEST,
            Fail::Missing(_) | Fail::Unsynced(_) => StatusCode::NOT_FOUND,
            Fail::SignIn => StatusCode::UNAUTHORIZED,
            Fail::Failed(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

pub(crate) struct Answer {
    pub value: Value,
    /// The caller's private data shaped it.
    pub private: bool,
}

/// The one builder of the JSON of each endpoint with an MCP tool: the API
/// and the MCP server both call it, so their answers cannot differ.
/// `caller` is the session or the MCP token.
pub(crate) async fn answer(
    app: &App,
    e: &Endpoint,
    a: &Args,
    caller: Option<&str>,
) -> Result<Answer, Fail> {
    check(e, a).map_err(Fail::Bad)?;
    let me = match (e.private, caller) {
        (true, None) => return Err(Fail::SignIn),
        (_, c) => c.unwrap_or_default(),
    };
    let text = |o: &Option<String>| Args::text(o).unwrap_or_default().to_string();
    let paged = |items: &[Value]| page(items, a.limit, a.cursor.as_deref()).map_err(Fail::Bad);
    let mut private = e.private;
    let value = match e.path {
        "/api/v1/prs" => {
            let p = prs_value(app, a, caller).map_err(Fail::Bad)?;
            private = p.private;
            p.value
        }
        "/api/v1/prs/:number" => {
            let n = a.number.unwrap_or_default();
            crate::prpage::pr_value(app, n)
                .await
                .ok_or(Fail::Unsynced(n))?
        }
        "/api/v1/people/:login" => person_value(app, &text(&a.login)),
        "/api/v1/needs" => {
            let mood = crate::needs::Mood::parse(a.mood.as_deref()).unwrap_or_default();
            crate::needs::value(app, me, mood)
        }
        "/api/v1/mine" => {
            let f =
                crate::mine::Filter::parse(Some(Args::text(&a.filter).unwrap_or("mine")), false);
            crate::mine::value(
                app,
                me,
                f,
                &text(&a.q),
                crate::mine::Pick {
                    set: Args::text(&a.set),
                    output: Args::text(&a.output),
                },
                a.limit,
                a.cursor.as_deref(),
            )
            .map_err(Fail::Bad)?
        }
        "/api/v1/sets" => {
            crate::sets::sets_value(app, me, Args::text(&a.set), Args::text(&a.output))
                .map_err(Fail::Missing)?
        }
        "/api/v1/inbox" => crate::notify::inbox_value(
            app,
            me,
            Args::text(&a.kind),
            Args::text(&a.sub),
            a.limit,
            a.cursor.as_deref(),
        )
        .map_err(Fail::Bad)?,
        "/api/v1/progress" => {
            json!({ "login": me, "progress": crate::progress::of(app, me, true) })
        }
        "/api/v1/search" => crate::search::value(app, &text(&a.q), a.limit),
        "/api/v1/first" => paged(&crate::first::rows(app))?,
        "/api/v1/need-help" => {
            let rows: Vec<Value> = crate::home::need_help(app)
                .iter()
                .map(|(sub, f)| json!({ "key": sub.key(), "name": sub.label(), "flags": f.json() }))
                .collect();
            paged(&rows)?
        }
        "/api/v1/packages/:name" => {
            let n = text(&a.name);
            crate::catalog::package_value(app, &n)
                .await
                .ok_or_else(|| Fail::Missing(format!("{n} is not in the nixpkgs index")))?
        }
        "/api/v1/modules/*file" => {
            let f = text(&a.file);
            crate::catalog::module_value(app, &f)
                .await
                .ok_or_else(|| Fail::Missing(format!("{f} is not a NixOS module in the index")))?
        }
        "/api/v1/areas/*path" => {
            let p = text(&a.path);
            crate::catalog::area_value(app, &p)
                .ok_or_else(|| Fail::Bad(format!("{p} is not a nixpkgs path")))?
        }
        "/api/v1/metrics" => crate::web::metrics_value(app),
        "/api/v1/status" => crate::web::status_value(app),
        "/api/v1/sync" => crate::status::state(app),
        "/api/v1/archive/:owner/:repo/:number" => {
            let json = crate::archive::item_json(
                app,
                &text(&a.owner),
                &text(&a.repo),
                a.number.unwrap_or_default(),
            )
            .map_err(|(code, m)| match code {
                StatusCode::NOT_FOUND => Fail::Missing(m.into()),
                _ => Fail::Failed(m.into()),
            })?;
            serde_json::from_str(&json).map_err(|e| Fail::Failed(e.to_string()))?
        }
        p => return Err(Fail::Failed(format!("no builder for {p}"))),
    };
    Ok(Answer { value, private })
}

/// The endpoints with an MCP tool: this module makes their routes. The
/// others have their own handler next to the page they serve.
pub(crate) fn served() -> impl Iterator<Item = &'static Endpoint> {
    ENDPOINTS.iter().filter(|e| matches!(e.mcp, By(_)))
}

pub fn routes() -> Router<Shared> {
    let mut r = Router::new()
        .route("/api/v1", get(index))
        .route("/api/v1/", get(index));
    for e in served() {
        r = r.route(
            e.path,
            get(
                move |State(app): State<Shared>,
                      headers: HeaderMap,
                      path: Option<Path<HashMap<String, String>>>,
                      Query(query): Query<Value>| async move {
                    reply(
                        &app,
                        &headers,
                        e,
                        path.map(|p| p.0).unwrap_or_default(),
                        query,
                    )
                    .await
                },
            ),
        );
    }
    r
}

pub(crate) async fn reply(
    app: &App,
    headers: &HeaderMap,
    e: &Endpoint,
    path: HashMap<String, String>,
    mut query: Value,
) -> Response {
    let fail = |f: Fail| {
        let code = f.code();
        (code, Json(f.json())).into_response()
    };
    for (k, v) in path {
        let int = e
            .params
            .iter()
            .any(|p| p.name == k && matches!(p.kind, Type::Int));
        query[&k] = match v.parse::<u64>() {
            Ok(n) if int => json!(n),
            Err(_) if int => return fail(Fail::Bad(format!("{k} is not a number"))),
            _ => json!(v),
        };
    }
    for p in e.params.iter().filter(|p| matches!(p.kind, Type::Int)) {
        if let Some(s) = query[p.name].as_str().map(str::to_string) {
            match s.trim().parse::<u64>() {
                Ok(n) => query[p.name] = json!(n),
                Err(_) => return fail(Fail::Bad(format!("{} is not a number", p.name))),
            }
        }
    }
    let a: Args = match serde_json::from_value(query) {
        Ok(a) => a,
        Err(err) => return fail(Fail::Bad(err.to_string())),
    };
    let caller = app.caller(headers);
    match answer(app, e, &a, caller.as_deref()).await {
        Ok(Answer { value, private }) => {
            let cache = if private {
                "private, no-store"
            } else {
                "no-cache"
            };
            ([(header::CACHE_CONTROL, cache)], Json(value)).into_response()
        }
        Err(f) => fail(f),
    }
}

/// `cursor` is the offset of the first item. It is opaque to clients.
pub(crate) fn page<T: Serialize>(
    items: &[T],
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<Value, String> {
    let start = match cursor.map(str::trim).filter(|c| !c.is_empty()) {
        None => 0,
        Some(c) => c.parse::<usize>().map_err(|_| format!("bad cursor {c}"))?,
    };
    let limit = limit.unwrap_or(LIMIT).clamp(1, MAX_LIMIT);
    let end = start.saturating_add(limit).min(items.len());
    let slice = items.get(start..end).unwrap_or(&[]);
    Ok(json!({
        "total": items.len(),
        "items": slice,
        "next_cursor": (end < items.len()).then(|| end.to_string()),
    }))
}

pub(crate) struct Prs {
    pub value: Value,
    /// The caller's subscriptions shaped the answer.
    pub private: bool,
}

/// `caller` is the session or the MCP token. Subscriptions, tags and
/// machines are private, so they count only when `login` is the caller.
pub(crate) fn prs_value(app: &App, q: &Args, caller: Option<&str>) -> Result<Prs, String> {
    let queue = q.queue.as_deref().unwrap_or("all");
    if !QUEUES.contains(&queue) {
        return Err(format!(
            "no queue {queue}; use one of {}",
            QUEUES.join(", ")
        ));
    }
    let login = Args::text(&q.login).or(caller.filter(|_| queue == "foryou"));
    let own = login.is_some() && login == caller;
    let s = app.snap();
    let mut out;
    let mut private = false;
    match queue {
        "ready" => {
            let section = Args::text(&q.section).unwrap_or("ready");
            let b = &s.board;
            out = match section {
                "ready" => page(&s.queues.merge, q.limit, q.cursor.as_deref()),
                "yours" => {
                    let Some(l) = login.filter(|_| own) else {
                        return Err("section=yours needs your own login".into());
                    };
                    if !s.committers.is(l) {
                        return Err(format!("{l} is not a committer"));
                    }
                    private = true;
                    let rows: Vec<Value> = crate::committer::yours_list(app, &s, l)
                        .into_iter()
                        .map(|(e, why)| {
                            let mut v = json!(e);
                            v["why"] = json!(why);
                            v
                        })
                        .collect();
                    page(&rows, q.limit, q.cursor.as_deref())
                }
                "maintainer_approved" => page(&b.maintainer_approved, q.limit, q.cursor.as_deref()),
                "security" => page(&b.security, q.limit, q.cursor.as_deref()),
                "backports" => page(&b.backports, q.limit, q.cursor.as_deref()),
                "waiting" => page(&b.waiting, q.limit, q.cursor.as_deref()),
                _ => {
                    return Err(format!(
                        "no section {section}; use one of {}",
                        SECTIONS.join(", ")
                    ));
                }
            }?;
            out["section"] = json!(section);
        }
        "bumps" => {
            let h = app.health.read().unwrap().clone();
            let index = app.index.read().unwrap();
            let mut rows = crate::bumps::open(&s, &index, &h);
            if let Some(l) = login {
                rows = crate::bumps::mine(app, &index, l, own, rows);
                private = own;
            }
            let items: Vec<Value> = rows.iter().map(crate::bumps::row_json).collect();
            out = page(&items, q.limit, q.cursor.as_deref())?;
            out["median_age_days"] =
                json!(crate::bumps::median_age_days(&rows, chrono::Utc::now()));
            out["login"] = json!(login);
        }
        _ => {
            if queue == "foryou" && login.is_none() {
                return Err("queue=foryou needs login, a session or an MCP token".into());
            }
            private = own;
            let entries = match login {
                Some(l) if own && queue == "foryou" => crate::mine::for_you(app, &s, l),
                _ => crate::web::review_bucket(&s, login, None, queue),
            };
            out = page(&entries, q.limit, q.cursor.as_deref())?;
            out["login"] = json!(login);
        }
    }
    out["queue"] = json!(queue);
    Ok(Prs {
        value: out,
        private,
    })
}

/// Public data only: the same answer for every caller.
pub(crate) fn person_value(app: &App, login: &str) -> Value {
    let s = app.snap();
    let person = s
        .ledger
        .people
        .get(login)
        .cloned()
        .unwrap_or(crate::ledger::Person {
            login: login.to_string(),
            ..Default::default()
        });
    let credits: Vec<_> = s.ledger.credits_of(login).cloned().collect();
    let dossier = crate::dossier::build(&s.pr_data, login);
    let area = s.areas.get(login).map(crate::area::Area::public);
    drop(s);
    let mut maintains = crate::catalog::maintains_value(app, login);
    if let Some(m) = maintains.as_object_mut() {
        m.remove("login");
    }
    let profile = crate::profile::profile_value(app, login);
    json!({
        "login": login,
        "facts": profile["facts"],
        "open_prs": profile["open_prs"],
        "recent_prs": profile["recent_prs"],
        "areas": profile["areas"],
        "flakes": profile["flakes"],
        "person": person,
        "dossier": dossier,
        "credits": credits,
        "area": area,
        "maintains": maintains,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_carries_the_cursor_of_the_next() {
        let items: Vec<u32> = (0..120).collect();
        let p = page(&items, None, None).unwrap();
        assert_eq!(p["total"], 120);
        assert_eq!(p["items"].as_array().unwrap().len(), LIMIT);
        assert_eq!(p["next_cursor"], "50");
        let p = page(&items, Some(100), Some("50")).unwrap();
        assert_eq!(p["items"][0], 50);
        assert_eq!(p["items"].as_array().unwrap().len(), 70);
        assert!(p["next_cursor"].is_null());
        let p = page(&items, Some(10), Some("500")).unwrap();
        assert_eq!(p["items"].as_array().unwrap().len(), 0);
        assert!(page(&items, None, Some("x")).is_err());
        assert_eq!(
            page(&(0..300).collect::<Vec<u32>>(), Some(9999), None).unwrap()["items"]
                .as_array()
                .unwrap()
                .len(),
            MAX_LIMIT
        );
    }

    #[test]
    fn foryou_needs_a_login_and_an_unknown_queue_fails() {
        let app = crate::sets::tests::app(&[]);
        let q = Args {
            queue: Some("foryou".into()),
            ..Default::default()
        };
        assert!(prs_value(&app, &q, None).is_err());
        let p = prs_value(&app, &q, Some("alice")).unwrap();
        assert!(p.private);
        assert_eq!(p.value["items"], json!([]));
        let q = Args {
            queue: Some("foryou".into()),
            login: Some("bob".into()),
            ..Default::default()
        };
        assert!(!prs_value(&app, &q, Some("alice")).unwrap().private);
        let q = Args {
            queue: Some("bumps".into()),
            ..Default::default()
        };
        let p = prs_value(&app, &q, Some("alice")).unwrap();
        assert!(!p.private);
        assert!(p.value["login"].is_null());
        let q = Args {
            queue: Some("bumps".into()),
            login: Some("alice".into()),
            ..Default::default()
        };
        assert!(prs_value(&app, &q, Some("alice")).unwrap().private);
        let q = Args {
            queue: Some("firsttime".into()),
            ..Default::default()
        };
        let p = prs_value(&app, &q, None).unwrap();
        assert_eq!((p.private, &p.value["queue"]), (false, &json!("firsttime")));
        let q = Args {
            queue: Some("nope".into()),
            ..Default::default()
        };
        assert!(prs_value(&app, &q, None).is_err());
        let q = Args {
            queue: Some("ready".into()),
            section: Some("security".into()),
            ..Default::default()
        };
        assert_eq!(
            prs_value(&app, &q, None).unwrap().value["section"],
            "security"
        );
        let q = Args {
            queue: Some("ready".into()),
            section: Some("yours".into()),
            ..Default::default()
        };
        assert!(prs_value(&app, &q, None).is_err());
        assert!(prs_value(&app, &q, Some("alice")).is_err());
    }

    #[test]
    fn the_public_record_has_no_tags() {
        let app = crate::sets::tests::app(&[]);
        app.store
            .set_tags("alice", &["pkgs/secret".into()])
            .unwrap();
        app.recompute().unwrap();
        let v = person_value(&app, "alice");
        assert!(v["area"].is_object());
        assert!(!v.to_string().contains("pkgs/secret"));
    }

    fn get_routes() -> Vec<String> {
        crate::web::ux_pages::get_routes()
    }

    /// The path of a page or a target, without its query and fragment.
    fn route_of(view: &str) -> &str {
        view.split(['?', '#']).next().unwrap()
    }

    /// Every view of the site that the table must cover: each GET page
    /// route, and each value of each tab, filter and section.
    fn views() -> Vec<String> {
        let mut v: Vec<String> = crate::review::TABS
            .iter()
            .map(|(id, _)| format!("/review?tab={id}"))
            .collect();
        v.extend(
            crate::committer::sections()
                .iter()
                .map(|s| format!("/review?tab=ready&section={s}")),
        );
        v.extend(crate::nixpkgs::views().iter().map(|s| s.to_string()));
        v.extend(crate::profile::views());
        v.extend(
            crate::mine::Filter::ALL
                .iter()
                .map(|f| format!("/packages?filter={}", f.key())),
        );
        v.extend(
            crate::needs::Mood::ALL
                .iter()
                .map(|m| format!("/?mood={}", m.key())),
        );
        v.extend(
            crate::notify::Kind::ALL
                .iter()
                .map(|k| format!("/inbox?kind={}", k.key())),
        );
        v.extend(
            crate::me::SETTINGS
                .iter()
                .map(|(id, _)| format!("/me/settings#{id}")),
        );
        let skips = crate::web::ux_pages::entries().skips;
        v.extend(
            get_routes()
                .into_iter()
                .filter(|r| !r.starts_with("/api/") && !skips.contains(r)),
        );
        v
    }

    #[test]
    fn the_inbox_kinds_of_the_api_are_those_of_the_site() {
        let site: Vec<&str> = crate::notify::Kind::ALL.iter().map(|k| k.key()).collect();
        assert_eq!(KINDS.to_vec(), site);
    }

    #[test]
    fn every_view_of_the_site_is_in_the_table() {
        let views = views();
        assert!(views.len() > 40, "found only {views:?}");
        for view in &views {
            assert!(
                PAGES.iter().any(|(p, _)| p == view),
                "{view} is not in api::PAGES: name the endpoints that give its data, or say why there is none"
            );
        }
        let routes = get_routes();
        for (i, (page, _)) in PAGES.iter().enumerate() {
            assert!(
                routes.iter().any(|r| r == route_of(page)),
                "{page} in api::PAGES is not a GET route"
            );
            assert!(
                !PAGES[..i].iter().any(|(p, _)| p == page),
                "{page} is twice in api::PAGES"
            );
        }
    }

    #[test]
    fn every_page_target_is_an_endpoint_with_valid_parameters() {
        for (page, cover) in PAGES {
            let By(targets) = cover else { continue };
            assert!(!targets.is_empty(), "{page} has no target");
            for t in *targets {
                let e = ENDPOINTS
                    .iter()
                    .find(|e| e.path == route_of(t))
                    .unwrap_or_else(|| panic!("{t} of {page} is not in api::ENDPOINTS"));
                let query = t.split_once('?').map_or("", |(_, q)| q);
                for kv in query.split('&').filter(|s| !s.is_empty()) {
                    let (k, v) = kv.split_once('=').unwrap();
                    let p = e
                        .params
                        .iter()
                        .find(|p| p.name == k)
                        .unwrap_or_else(|| panic!("{t}: {} takes no {k}", e.path));
                    if let Type::OneOf(values) = p.kind {
                        assert!(values.contains(&v), "{t}: {k}={v} is not one of {values:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn every_api_route_is_in_the_table_and_back() {
        let routes: Vec<String> = get_routes()
            .into_iter()
            .filter(|r| r.starts_with("/api/v1/") && r != "/api/v1/")
            .collect();
        for r in &routes {
            assert!(
                ENDPOINTS.iter().any(|e| e.path == r),
                "GET {r} is not in api::ENDPOINTS"
            );
        }
        for e in ENDPOINTS.iter().filter(|e| !e.path.starts_with("POST ")) {
            assert!(
                routes.iter().any(|r| r == e.path),
                "{} in api::ENDPOINTS is not a GET route",
                e.path
            );
        }
    }

    #[test]
    fn every_parameter_is_an_argument_and_every_path_part_a_parameter() {
        let fields = serde_json::to_value(Args::default()).unwrap();
        for e in ENDPOINTS {
            for p in e
                .params
                .iter()
                .filter(|_| served().any(|s| s.path == e.path))
            {
                assert!(
                    fields.get(p.name).is_some(),
                    "{}: api::Args has no field {}",
                    e.path,
                    p.name
                );
            }
            for part in e.path.split('/') {
                if let Some(n) = part.strip_prefix(':').or_else(|| part.strip_prefix('*')) {
                    assert!(
                        e.params.iter().any(|p| p.name == n && p.required),
                        "{}: the path parameter {n} is not a required parameter",
                        e.path
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn every_tool_has_a_builder_and_a_cli_command() {
        use clap::CommandFactory;
        let app = crate::sets::tests::app(&[]);
        let cli = crate::Cli::command();
        let mut names = Vec::new();
        for e in ENDPOINTS {
            if let By(c) = e.cli {
                assert!(
                    cli.find_subcommand(c).is_some(),
                    "{}: maintainer-mode has no command {c}",
                    e.path
                );
            }
            let By((name, _)) = e.mcp else { continue };
            assert!(!names.contains(&name), "the tool {name} is twice");
            names.push(name);
            let mut a = serde_json::to_value(Args::default()).unwrap();
            for p in e.params.iter().filter(|p| p.required) {
                a[p.name] = match p.kind {
                    Type::Int => json!(1),
                    _ => json!("zz-none"),
                };
            }
            let a: Args = serde_json::from_value(a).unwrap();
            if e.private {
                assert!(matches!(answer(&app, e, &a, None).await, Err(Fail::SignIn)));
            }
            match answer(&app, e, &a, Some("alice")).await {
                Err(Fail::Failed(m)) if m.starts_with("no builder") => {
                    panic!("{}: api::answer has no builder", e.path)
                }
                Err(Fail::Bad(m)) => panic!("{}: {m}", e.path),
                _ => {}
            }
        }
    }

    #[test]
    fn a_tool_checks_its_arguments_from_the_table() {
        let e = tool("prs").unwrap();
        let bad = Args {
            queue: Some("nope".into()),
            ..Default::default()
        };
        assert!(check(e, &bad).unwrap_err().contains("use one of"));
        assert!(check(tool("person").unwrap(), &Args::default()).is_err());
        let s = schema(tool("needs").unwrap());
        assert_eq!(s["properties"]["mood"]["enum"], json!(MOODS));
        assert_eq!(
            schema(tool("package").unwrap())["required"],
            json!(["name"])
        );
        assert!(is_private("/api/v1/sets") && !is_private("/api/v1/prs"));
    }
}
