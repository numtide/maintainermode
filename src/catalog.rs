//! A page per package, NixOS module and path area: health, then the issues
//! and pull requests attached to it. Anyone may read them; a person who
//! logged in can subscribe, and reads what changed in a private Atom feed.

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;

use crate::area::{Index, area_key, under};
use crate::health::{Advisory, Build, Health, Hydra};
use crate::links::{module_key, module_name, pkg_key};
use crate::model::{Issue, Pr};
use crate::web::{Shared, Snapshot, ago, esc, icon, octicon, page, random_id};

const LIST: usize = 100;
const FEED_DAYS: i64 = 30;
/// Directories a reader starts from; `ci/OWNERS` adds its own.
const TOP_AREAS: [&str; 14] = [
    "lib",
    "doc",
    "ci",
    "maintainers",
    "nixos/lib",
    "nixos/modules",
    "nixos/tests",
    "pkgs/stdenv",
    "pkgs/build-support",
    "pkgs/top-level",
    "pkgs/by-name",
    "pkgs/development/python-modules",
    "pkgs/development/haskell-modules",
    "pkgs/os-specific/linux",
];

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/packages", get(browse))
        .route("/p/:attr", get(package_page))
        .route("/m/*file", get(module_page))
        .route("/a/*path", get(area_page))
        .route("/u/:login/maintains", get(maintains_page))
        .route("/watch", get(watch_page).post(add_watch))
        .route("/watch/remove", post(remove_watch))
        .route("/watch/maintained", post(watch_maintained))
        .route("/me/feed", post(new_feed))
        .route("/feed/:token", get(feed))
        .route("/api/v1/packages/:attr", get(api_package))
        .route("/api/v1/modules/*file", get(api_module))
        .route("/api/v1/areas/*path", get(api_area))
        .route("/api/v1/maintainers/:login", get(api_maintains))
        .route("/api/v1/find/*name", get(api_find))
}

/// Counted once per pull request, for every ancestor of every file it
/// changes, so an area's count is a lookup.
pub fn open_by_dir(prs: &[Pr]) -> HashMap<String, usize> {
    let mut out: HashMap<String, usize> = HashMap::new();
    for pr in prs.iter().filter(|p| p.is_open()) {
        let mut dirs: HashSet<&str> = HashSet::new();
        for f in &pr.files {
            let mut p = f.as_str();
            while let Some((parent, _)) = p.rsplit_once('/') {
                dirs.insert(parent);
                p = parent;
            }
        }
        for d in dirs {
            *out.entry(d.to_string()).or_default() += 1;
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Subject {
    Pkg(String),
    Module(String),
    Area(String),
}

fn valid_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 200
        && !p.contains("..")
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./+".contains(c))
}

impl Subject {
    pub fn key(&self) -> String {
        match self {
            Subject::Pkg(a) => pkg_key(a),
            Subject::Module(f) => module_key(f),
            Subject::Area(p) => format!("area:{p}"),
        }
    }

    pub fn parse(key: &str) -> Option<Subject> {
        let (kind, rest) = key.split_once(':')?;
        if !valid_path(rest) {
            return None;
        }
        match kind {
            "pkg" => Some(Subject::Pkg(rest.into())),
            "module" => Some(Subject::Module(rest.into())),
            "area" => Some(Subject::Area(rest.into())),
            _ => None,
        }
    }

    /// What a person types: an attribute, a package name, `nixos/NAME`, a
    /// module file, or a path.
    pub fn resolve(index: &Index, input: &str) -> Option<Subject> {
        let s = input.trim().trim_matches('/');
        if !valid_path(s) {
            return None;
        }
        if let Some(attr) = index.resolve(s) {
            return Some(Subject::Pkg(attr.to_string()));
        }
        if index.modules.contains_key(s) {
            return Some(Subject::Module(s.to_string()));
        }
        if s.starts_with("nixos/") && !s.starts_with("nixos/modules/") {
            if let Some(f) = index.modules.keys().find(|f| module_name(f) == s) {
                return Some(Subject::Module(f.clone()));
            }
        }
        s.contains('/').then(|| Subject::Area(s.to_string()))
    }

    pub fn href(&self) -> String {
        match self {
            Subject::Pkg(a) => format!("/p/{}", esc(a)),
            Subject::Module(f) => format!("/m/{}", esc(f)),
            Subject::Area(p) => format!("/a/{}", esc(p)),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Subject::Pkg(a) => a.clone(),
            Subject::Module(f) => module_name(f),
            Subject::Area(p) => format!("{p}/"),
        }
    }

    fn icon(&self) -> String {
        match self {
            Subject::Pkg(_) => icon(octicon!("package")),
            Subject::Module(_) => icon(octicon!("gear")),
            Subject::Area(_) => icon(octicon!("file-directory")),
        }
    }
}

/// Pull requests and issues of a subject, newest first.
fn attached<'a>(
    s: &'a Snapshot,
    index: &Index,
    subject: &Subject,
) -> (Vec<&'a Pr>, Vec<&'a Issue>) {
    let pr = |n: &u64| s.pr_at.get(n).map(|&i| &s.pr_data[i]);
    let issue = |n: &u64| s.issue_at.get(n).map(|&i| &s.issues[i]);
    match subject {
        Subject::Pkg(_) | Subject::Module(_) => {
            let key = subject.key();
            (
                s.links
                    .prs
                    .get(&key)
                    .into_iter()
                    .flatten()
                    .filter_map(pr)
                    .collect(),
                s.links
                    .issues
                    .get(&key)
                    .into_iter()
                    .flatten()
                    .filter_map(issue)
                    .collect(),
            )
        }
        Subject::Area(path) => {
            let mut prs: Vec<&Pr> = s
                .pr_data
                .iter()
                .filter(|p| p.files.iter().any(|f| under(f, path)))
                .collect();
            prs.sort_by_key(|p| std::cmp::Reverse(p.number));
            let mut numbers: HashSet<u64> = HashSet::new();
            for (attr, p) in &index.pkgs {
                if p.file.as_deref().is_some_and(|f| under(f, path)) {
                    numbers.extend(s.links.issues.get(&pkg_key(attr)).into_iter().flatten());
                }
            }
            for f in index.modules.keys().filter(|f| under(f, path)) {
                numbers.extend(s.links.issues.get(&module_key(f)).into_iter().flatten());
            }
            for i in &s.issues {
                if i.mentions.iter().any(|m| under(m, path)) {
                    numbers.insert(i.number);
                }
            }
            let mut issues: Vec<&Issue> = numbers.iter().filter_map(issue).collect();
            issues.sort_by_key(|i| std::cmp::Reverse(i.number));
            (prs, issues)
        }
    }
}

/// Advisories whose nixpkgs issue is still open, or not synced yet. The sync
/// keeps only issues open or updated in its window, so once every open issue
/// is read, an issue it does not have closed long ago.
fn open_advisories<'a>(h: &'a Health, s: &Snapshot, attr: &str) -> Vec<&'a Advisory> {
    h.advisories
        .get(attr)
        .into_iter()
        .flatten()
        .filter(|a| match a.issue {
            None => true,
            Some(n) => match s.issue_at.get(&n) {
                Some(&i) => s.issues[i].is_open(),
                None => !s.issues_complete,
            },
        })
        .collect()
}

/// repology's newest release, unless nixpkgs already carries it: under this
/// attribute, or under another of the same package, like `openssl_3`.
fn upstream<'a>(s: &Snapshot, index: &Index, h: &'a Health, attr: &str) -> Option<&'a String> {
    let newest = h.upstream.get(attr)?;
    let pname = &index.pkgs.get(attr)?.pname;
    (!s.versions.get(pname).is_some_and(|v| v.contains(newest))).then_some(newest)
}

struct Flags {
    upstream: Option<String>,
    advisories: usize,
    unmaintained: bool,
    open_prs: usize,
    open_issues: usize,
}

impl Flags {
    fn of(s: &Snapshot, index: &Index, h: &Health, subject: &Subject) -> Flags {
        let (prs, issues) = attached(s, index, subject);
        let (upstream, advisories, unmaintained) = match subject {
            Subject::Pkg(attr) => {
                let p = index.pkgs.get(attr);
                (
                    upstream(s, index, h, attr).cloned(),
                    open_advisories(h, s, attr).len(),
                    p.is_some_and(|p| p.maintainers.is_empty() && p.teams.is_empty()),
                )
            }
            Subject::Module(f) => (
                None,
                0,
                index
                    .modules
                    .get(f)
                    .is_some_and(|m| m.maintainers.is_empty() && m.teams.is_empty()),
            ),
            Subject::Area(_) => (None, 0, false),
        };
        Flags {
            upstream,
            advisories,
            unmaintained,
            open_prs: prs.iter().filter(|p| p.is_open()).count(),
            open_issues: issues.iter().filter(|i| i.is_open()).count(),
        }
    }

    fn html(&self) -> String {
        let mut out = Vec::new();
        if self.advisories > 0 {
            out.push(format!(
                "<span class=\"Label Label--danger\">{} {} advisor{}</span>",
                icon(octicon!("shield")),
                self.advisories,
                if self.advisories == 1 { "y" } else { "ies" }
            ));
        }
        if let Some(u) = &self.upstream {
            out.push(format!(
                "<span class=\"Label Label--attention\">upstream {}</span>",
                esc(u)
            ));
        }
        if self.unmaintained {
            out.push("<span class=\"Label\">no maintainer</span>".into());
        }
        out.push(format!(
            "<span class=\"Label\" title=\"open pull requests\">{} {}</span>",
            icon(octicon!("git-pull-request")),
            self.open_prs
        ));
        out.push(format!(
            "<span class=\"Label\" title=\"open issues\">{} {}</span>",
            icon(octicon!("issue-opened")),
            self.open_issues
        ));
        out.join(" ")
    }

    fn json(&self) -> Value {
        json!({
            "upstream": self.upstream,
            "open_advisories": self.advisories,
            "unmaintained": self.unmaintained,
            "open_prs": self.open_prs,
            "open_issues": self.open_issues,
        })
    }
}

fn pr_row(p: &Pr, act: bool) -> String {
    let (ic, class) = match p.state.as_str() {
        "MERGED" => (icon(octicon!("git-merge")), "color-fg-done"),
        "CLOSED" => (icon(octicon!("git-pull-request-closed")), "color-fg-danger"),
        _ => (icon(octicon!("git-pull-request")), "color-fg-open"),
    };
    let ci = match p.ci.as_deref() {
        Some("SUCCESS") if p.is_open() => " · checks pass",
        Some("FAILURE" | "ERROR") if p.is_open() => " · checks fail",
        _ => "",
    };
    let when = match (p.merged_at, p.closed_at) {
        (Some(t), _) => format!("merged {} ago", ago(t)),
        (None, Some(t)) => format!("closed {} ago", ago(t)),
        _ => format!("opened {} ago", ago(p.created_at)),
    };
    format!(
        "<div class=\"Box-row d-flex flex-items-start\"><span class=\"mr-2 {class}\">{ic}</span>\
         <div class=\"flex-auto\"><a class=\"Link--primary text-bold\" href=\"{u}\">{t}</a>{draft}\
         <div class=\"f6 color-fg-muted mt-1\">#{n} by <a href=\"/u/{a}\">{a}</a> · {when}{ci}</div></div>{button}</div>",
        u = esc(&p.url),
        t = esc(&p.title),
        n = p.number,
        a = esc(&p.author),
        button = if act && p.is_open() {
            crate::actions::review_button(p.number)
        } else {
            String::new()
        },
        draft = if p.is_draft {
            " <span class=\"Label ml-1\">draft</span>"
        } else {
            ""
        },
    )
}

fn issue_row(i: &Issue) -> String {
    let (ic, class) = if i.is_open() {
        (icon(octicon!("issue-opened")), "color-fg-open")
    } else {
        (icon(octicon!("issue-closed")), "color-fg-done")
    };
    let when = match i.closed_at {
        Some(t) if !i.is_open() => format!("closed {} ago", ago(t)),
        _ => format!("opened {} ago", ago(i.created_at)),
    };
    let labels: String = i
        .labels
        .iter()
        .filter(|l| !l.starts_with("0.kind:") && !l.starts_with("9."))
        .take(3)
        .map(|l| format!(" <span class=\"Label ml-1\">{}</span>", esc(l)))
        .collect();
    format!(
        "<div class=\"Box-row d-flex flex-items-start\"><span class=\"mr-2 {class}\">{ic}</span>\
         <div class=\"flex-auto\"><a class=\"Link--primary text-bold\" href=\"{u}\">{t}</a>{labels}\
         <div class=\"f6 color-fg-muted mt-1\">#{n} by {a} · {when} · {c} comments</div></div></div>",
        u = esc(&i.url),
        t = esc(&i.title),
        n = i.number,
        a = esc(&i.author),
        c = i.comments,
    )
}

/// Open items in a box, closed ones folded under it.
fn section<T>(
    title: &str,
    items: &[&T],
    open: impl Fn(&T) -> bool,
    row: impl Fn(&T) -> String,
) -> String {
    let (o, c): (Vec<&&T>, Vec<&&T>) = items.iter().partition(|x| open(x));
    let mut out = format!(
        "<div class=\"Box mb-4\"><div class=\"Box-header text-bold\">{title} <span class=\"Counter\">{}</span></div>",
        o.len()
    );
    if o.is_empty() {
        out.push_str("<div class=\"Box-row color-fg-muted\">None open.</div>");
    }
    for x in o.iter().take(LIST) {
        out.push_str(&row(x));
    }
    if o.len() > LIST {
        let _ = write!(
            out,
            "<div class=\"Box-footer f6 color-fg-muted\">{} more in the JSON API</div>",
            o.len() - LIST
        );
    }
    out.push_str("</div>");
    if !c.is_empty() {
        let _ = write!(
            out,
            "<details class=\"Box mb-4\"><summary class=\"Box-header\">Closed or merged in the synced window <span class=\"Counter\">{}</span></summary>",
            c.len()
        );
        for x in c.iter().take(LIST) {
            out.push_str(&row(x));
        }
        out.push_str("</details>");
    }
    out
}

fn people_links(logins: &[String]) -> String {
    if logins.is_empty() {
        return "<span class=\"color-fg-muted\">none</span>".into();
    }
    logins
        .iter()
        .map(|l| format!("<a href=\"/u/{0}/maintains\">{0}</a>", esc(l)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn teams_html(index: &Index, teams: &[String]) -> String {
    teams
        .iter()
        .map(|t| {
            format!(
                "{} ({})",
                esc(t),
                people_links(index.teams.get(t).map(Vec::as_slice).unwrap_or(&[]))
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn source_link(rev: &str, file: &str) -> String {
    let r = if rev.is_empty() { "master" } else { rev };
    format!(
        "<a class=\"branch-name\" href=\"https://github.com/NixOS/nixpkgs/blob/{}/{f}\">{f}</a>",
        esc(r),
        f = esc(file)
    )
}

fn subscribe_button(watching: Option<bool>, subject: &Subject) -> String {
    match watching {
        None => "<a class=\"btn btn-sm\" href=\"/auth/login\">Sign in to subscribe</a>".into(),
        Some(false) => format!(
            "<form method=\"post\" action=\"/watch\" class=\"d-inline\"><input type=\"hidden\" name=\"key\" value=\"{}\">\
             <button class=\"btn btn-sm btn-primary\" type=\"submit\">{} Subscribe</button></form>",
            esc(&subject.key()),
            icon(octicon!("bell"))
        ),
        Some(true) => format!(
            "<form method=\"post\" action=\"/watch/remove\" class=\"d-inline\"><input type=\"hidden\" name=\"key\" value=\"{}\">\
             <button class=\"btn btn-sm\" type=\"submit\">Unsubscribe</button></form>",
            esc(&subject.key())
        ),
    }
}

fn watching(app: &crate::web::App, viewer: Option<&str>, subject: &Subject) -> Option<bool> {
    let v = viewer?;
    Some(
        app.store
            .watched(v)
            .unwrap_or_default()
            .contains(&subject.key()),
    )
}

fn head(title: &str, sub: &str, button: &str) -> String {
    format!(
        "<div class=\"Subhead d-flex flex-items-center\"><div class=\"flex-auto\"><h1 class=\"Subhead-heading\">{title}</h1>\
         <div class=\"Subhead-description\">{sub}</div></div><div>{button}</div></div>"
    )
}

fn build_row(label: &str, job: &str, b: Option<&Build>) -> String {
    let body = match b {
        Some(b) => format!(
            "<a href=\"{}\" class=\"{}\">{}</a> <span class=\"color-fg-muted\">{}</span>",
            b.url(),
            if b.ok {
                "color-fg-success"
            } else {
                "color-fg-danger"
            },
            b.status,
            b.at.map(|t| format!("{} ago", ago(t))).unwrap_or_default()
        ),
        None => format!(
            "<span class=\"color-fg-muted\">no finished build of <a href=\"{}\">{}</a></span>",
            esc(&Hydra::job_url(job)),
            esc(job)
        ),
    };
    kv(label, &body)
}

fn kv(k: &str, v: &str) -> String {
    format!(
        "<div class=\"Box-row d-flex\"><div class=\"text-bold\" style=\"min-width:140px\">{k}</div><div class=\"flex-auto\">{v}</div></div>"
    )
}

fn advisories_html(list: &[&Advisory]) -> String {
    if list.is_empty() {
        return "<span class=\"color-fg-success\">none open</span>".into();
    }
    list.iter()
        .map(|a| {
            let issue = a
                .issue
                .map(|n| {
                    format!(
                        " · <a href=\"https://github.com/NixOS/nixpkgs/issues/{n}\">#{n}</a>"
                    )
                })
                .unwrap_or_default();
            format!(
                "<div><a class=\"color-fg-danger\" href=\"{}\">{}</a> {}{issue}<div class=\"f6 color-fg-muted\">{}</div></div>",
                esc(&a.url()),
                esc(&a.code),
                esc(&a.title),
                esc(&a.cves.join(", "))
            )
        })
        .collect()
}

fn not_found(app: &crate::web::App, viewer: Option<&str>, what: &str) -> Response {
    let body = format!(
        "<div class=\"blankslate\"><p>{} is not in the nixpkgs index{}.</p><p><a href=\"/packages\">Search packages, modules and areas</a></p></div>",
        esc(what),
        if app.index.read().unwrap().rev.is_empty() {
            " yet: the first index is still being built"
        } else {
            ""
        }
    );
    (StatusCode::NOT_FOUND, page(app, viewer, "packages", &body)).into_response()
}

async fn package_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    let viewer = app.viewer(&headers);
    let (attr, p, rev) = {
        let index = app.index.read().unwrap();
        let Some(attr) = index.resolve(&name).map(String::from) else {
            drop(index);
            return not_found(&app, viewer.as_deref(), &name);
        };
        let p = index.pkgs.get(&attr).cloned().unwrap_or_default();
        (attr, p, index.rev.clone())
    };
    if attr != name {
        return Redirect::to(&format!("/p/{attr}")).into_response();
    }
    let subject = Subject::Pkg(attr.clone());
    let job = Hydra::package_job(&attr);
    let build = app.hydra.last(&job).await;
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let (prs, issues) = attached(&s, &index, &subject);
    let mut out = head(
        &format!("{} {}", subject.icon(), esc(&attr)),
        &format!(
            "{} {}{}",
            esc(&p.pname),
            esc(&p.version),
            p.file
                .as_deref()
                .map(|f| format!(" · {}", source_link(&rev, f)))
                .unwrap_or_default()
        ),
        &subscribe_button(watching(&app, viewer.as_deref(), &subject), &subject),
    );
    out.push_str("<div class=\"Box mb-4\"><div class=\"Box-header text-bold\">Health</div>");
    let version = match upstream(&s, &index, &h, &attr) {
        Some(u) => format!(
            "{} <span class=\"Label Label--attention\">upstream {}</span>",
            esc(&p.version),
            esc(u)
        ),
        None => format!(
            "{} <span class=\"color-fg-muted f6\">{}</span>",
            esc(&p.version),
            if h.repology_at.is_some() {
                "no newer release known to repology"
            } else {
                "upstream not checked yet"
            }
        ),
    };
    out.push_str(&kv("Version", &version));
    out.push_str(&build_row("Hydra", &job, build.as_ref()));
    out.push_str(&kv(
        "Advisories",
        &advisories_html(&open_advisories(&h, &s, &attr)),
    ));
    if viewer.is_some() {
        out.push_str(&kv(
            "Update",
            &crate::actions::update_form(
                &attr,
                upstream(&s, &index, &h, &attr).map(String::as_str),
            ),
        ));
    }
    out.push_str(&kv("Maintainers", &people_links(&p.maintainers)));
    if !p.teams.is_empty() {
        out.push_str(&kv("Teams", &teams_html(&index, &p.teams)));
    }
    out.push_str("</div>");
    out.push_str(&section(
        "Pull requests",
        &prs,
        |p| p.is_open(),
        |p| pr_row(p, viewer.is_some()),
    ));
    out.push_str(&section("Issues", &issues, |i| i.is_open(), issue_row));
    page(&app, viewer.as_deref(), "packages", &out).into_response()
}

async fn module_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(file): Path<String>,
) -> Response {
    let viewer = app.viewer(&headers);
    let (m, rev) = {
        let index = app.index.read().unwrap();
        match index.modules.get(&file) {
            Some(m) => (m.clone(), index.rev.clone()),
            None => {
                drop(index);
                return not_found(&app, viewer.as_deref(), &file);
            }
        }
    };
    let subject = Subject::Module(file.clone());
    let name = module_name(&file);
    let job = Hydra::test_job(name.trim_start_matches("nixos/"));
    let build = app.hydra.last(&job).await;
    let s = app.snap();
    let index = app.index.read().unwrap();
    let (prs, issues) = attached(&s, &index, &subject);
    let mut out = head(
        &format!("{} {}", subject.icon(), esc(&name)),
        &format!("NixOS module · {}", source_link(&rev, &file)),
        &subscribe_button(watching(&app, viewer.as_deref(), &subject), &subject),
    );
    out.push_str("<div class=\"Box mb-4\"><div class=\"Box-header text-bold\">Health</div>");
    out.push_str(&build_row("NixOS test", &job, build.as_ref()));
    out.push_str(&kv("Maintainers", &people_links(&m.maintainers)));
    if !m.teams.is_empty() {
        out.push_str(&kv("Teams", &teams_html(&index, &m.teams)));
    }
    out.push_str(&kv(
        "Area",
        &format!(
            "<a href=\"/a/{0}\">{0}/</a>",
            esc(area_key(&file)
                .rsplit_once('/')
                .map_or("nixos/modules", |(d, _)| d))
        ),
    ));
    out.push_str("</div>");
    out.push_str(&section(
        "Pull requests",
        &prs,
        |p| p.is_open(),
        |p| pr_row(p, viewer.is_some()),
    ));
    out.push_str(&section("Issues", &issues, |i| i.is_open(), issue_row));
    page(&app, viewer.as_deref(), "packages", &out).into_response()
}

/// Packages and modules under a path, the ones that need attention first.
fn contents(index: &Index, s: &Snapshot, h: &Health, path: &str) -> Vec<(Subject, Flags)> {
    let mut v: Vec<(Subject, Flags)> = index
        .pkgs
        .iter()
        .filter(|(attr, p)| {
            p.file.as_deref().is_some_and(|f| under(f, path))
                && index.by_pname.get(&p.pname) == Some(attr)
        })
        .map(|(attr, _)| Subject::Pkg(attr.clone()))
        .chain(
            index
                .modules
                .keys()
                .filter(|f| under(f, path))
                .map(|f| Subject::Module(f.clone())),
        )
        .map(|sub| {
            let f = Flags::of(s, index, h, &sub);
            (sub, f)
        })
        .collect();
    v.sort_by_key(|(sub, f)| {
        (
            std::cmp::Reverse(f.advisories),
            f.upstream.is_none(),
            std::cmp::Reverse(f.open_prs + f.open_issues),
            sub.label(),
        )
    });
    v
}

fn subject_rows(rows: &[(Subject, Flags)]) -> String {
    let mut out = String::new();
    for (sub, f) in rows.iter().take(LIST) {
        let _ = write!(
            out,
            "<div class=\"Box-row d-flex flex-items-center flex-wrap\" style=\"gap:8px\">\
             <a class=\"Link--primary text-bold flex-auto\" href=\"{}\">{} {}</a>{}</div>",
            sub.href(),
            sub.icon(),
            esc(&sub.label()),
            f.html()
        );
    }
    if rows.len() > LIST {
        let _ = write!(
            out,
            "<div class=\"Box-footer f6 color-fg-muted\">{} more in the JSON API</div>",
            rows.len() - LIST
        );
    }
    out
}

async fn area_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(path): Path<String>,
) -> Response {
    let viewer = app.viewer(&headers);
    let path = path.trim_matches('/').to_string();
    if !valid_path(&path) {
        return (StatusCode::BAD_REQUEST, "not a nixpkgs path").into_response();
    }
    let subject = Subject::Area(path.clone());
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let (prs, issues) = attached(&s, &index, &subject);
    let rows = contents(&index, &s, &h, &path);
    let owners: Vec<String> = index
        .owners
        .iter()
        .filter(|(p, _)| under(&path, p) || under(p, &path))
        .map(|(p, l)| format!("<a href=\"/a/{0}\">{0}</a> {1}", esc(p), people_links(l)))
        .collect();
    let parent = path
        .rsplit_once('/')
        .map(|(p, _)| format!("<a href=\"/a/{0}\">{0}/</a> · ", esc(p)))
        .unwrap_or_default();
    let mut out = head(
        &format!("{} {}/", subject.icon(), esc(&path)),
        &format!(
            "{parent}{} packages and modules · {} open pull requests",
            rows.len(),
            s.open_by_dir.get(&path).copied().unwrap_or(0)
        ),
        &subscribe_button(watching(&app, viewer.as_deref(), &subject), &subject),
    );
    let prefix = format!("{path}/");
    let mut children: Vec<(&String, &usize)> = s
        .open_by_dir
        .iter()
        .filter(|(d, _)| d.strip_prefix(&prefix).is_some_and(|r| !r.contains('/')))
        .collect();
    children.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    if !owners.is_empty() || !children.is_empty() {
        out.push_str("<div class=\"Box mb-4\">");
        if !owners.is_empty() {
            out.push_str(&kv("ci/OWNERS", &owners.join("<br>")));
        }
        if !children.is_empty() {
            let list: Vec<String> = children
                .iter()
                .take(40)
                .map(|(d, n)| {
                    format!(
                        "<a href=\"/a/{0}\">{1}</a>&nbsp;<span class=\"color-fg-muted\">{n}</span>",
                        esc(d),
                        esc(d.rsplit('/').next().unwrap_or(d))
                    )
                })
                .collect();
            out.push_str(&kv("Busy below", &list.join(" · ")));
        }
        out.push_str("</div>");
    }
    if !rows.is_empty() {
        let _ = write!(
            out,
            "<div class=\"Box mb-4\"><div class=\"Box-header text-bold\">Packages and modules <span class=\"Counter\">{}</span></div>{}</div>",
            rows.len(),
            subject_rows(&rows)
        );
    }
    out.push_str(&section(
        "Pull requests",
        &prs,
        |p| p.is_open(),
        |p| pr_row(p, viewer.is_some()),
    ));
    out.push_str(&section("Issues", &issues, |i| i.is_open(), issue_row));
    page(&app, viewer.as_deref(), "packages", &out).into_response()
}

#[derive(Deserialize)]
struct Search {
    q: Option<String>,
}

async fn browse(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<Search>,
) -> Html<String> {
    let viewer = app.viewer(&headers);
    let q = q.q.unwrap_or_default().trim().to_string();
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let mut out = head(
        &format!("{} Packages, modules and areas", icon(octicon!("package"))),
        "Each has a page with its health, its issues and its pull requests. Subscribe to the ones you care about.",
        "",
    );
    let _ = write!(
        out,
        "<form method=\"get\" action=\"/packages\" class=\"d-flex mb-4\" style=\"gap:8px;max-width:560px\">\
         <input class=\"form-control flex-auto\" name=\"q\" value=\"{}\" placeholder=\"hello, nixos/nginx, pkgs/development/python-modules\" aria-label=\"search\" autofocus>\
         <button class=\"btn\" type=\"submit\">{} Search</button></form>",
        esc(&q),
        icon(octicon!("search"))
    );
    if let Some(v) = &viewer {
        let _ = write!(
            out,
            "<p><a href=\"/u/{0}/maintains\">What you maintain</a> · <a href=\"/watch\">What you watch</a></p>",
            esc(v)
        );
    }
    if q.is_empty() {
        let mut areas: Vec<String> = TOP_AREAS.iter().map(|a| a.to_string()).collect();
        areas.extend(index.owners.iter().map(|(p, _)| p.clone()));
        areas.sort();
        areas.dedup();
        out.push_str("<div class=\"Box\"><div class=\"Box-header text-bold\">Areas</div>");
        for a in areas {
            let _ = write!(
                out,
                "<div class=\"Box-row d-flex\"><a class=\"flex-auto\" href=\"/a/{0}\">{1} {0}/</a><span class=\"Label\">{2} {3}</span></div>",
                esc(&a),
                icon(octicon!("file-directory")),
                icon(octicon!("git-pull-request")),
                s.open_by_dir.get(&a).copied().unwrap_or(0)
            );
        }
        out.push_str("</div>");
        return page(&app, viewer.as_deref(), "packages", &out);
    }
    let ql = q.to_lowercase();
    let mut hits: Vec<Subject> = Vec::new();
    if let Some(sub) = Subject::resolve(&index, &q) {
        hits.push(sub);
    }
    let mut pkgs: Vec<&String> = index
        .by_pname
        .iter()
        .filter(|(pname, _)| pname.to_lowercase().contains(&ql))
        .map(|(_, attr)| attr)
        .collect();
    pkgs.extend(
        index
            .pkgs
            .keys()
            .filter(|a| a.contains('.') && a.to_lowercase().contains(&ql)),
    );
    pkgs.sort_by_key(|a| (a.len(), a.as_str()));
    pkgs.dedup();
    hits.extend(pkgs.into_iter().take(50).map(|a| Subject::Pkg(a.clone())));
    let mut mods: Vec<&String> = index
        .modules
        .keys()
        .filter(|f| f.to_lowercase().contains(&ql))
        .collect();
    mods.sort_by_key(|f| (module_name(f).len(), f.as_str()));
    hits.extend(
        mods.into_iter()
            .take(30)
            .map(|f| Subject::Module(f.clone())),
    );
    let mut dirs: Vec<&String> = s
        .open_by_dir
        .keys()
        .filter(|d| d.to_lowercase().contains(&ql))
        .collect();
    dirs.sort_by_key(|d| (d.len(), d.as_str()));
    hits.extend(dirs.into_iter().take(20).map(|d| Subject::Area(d.clone())));
    let mut seen = HashSet::new();
    hits.retain(|h| seen.insert(h.clone()));
    let rows: Vec<(Subject, Flags)> = hits
        .into_iter()
        .map(|sub| {
            let f = Flags::of(&s, &index, &h, &sub);
            (sub, f)
        })
        .collect();
    if rows.is_empty() {
        out.push_str("<div class=\"blankslate\"><p>Nothing matches.</p></div>");
    } else {
        let _ = write!(out, "<div class=\"Box\">{}</div>", subject_rows(&rows));
    }
    page(&app, viewer.as_deref(), "packages", &out)
}

struct Maintained {
    packages: Vec<(Subject, Option<String>)>,
    modules: Vec<(Subject, Option<String>)>,
}

/// What a login maintains, directly or through a team (named in the second
/// field). One row per package name: the other attributes of the same
/// package are aliases or other package sets.
fn maintained(index: &Index, login: &str) -> Maintained {
    let l = login.to_lowercase();
    let teams: Vec<&String> = index
        .teams
        .iter()
        .filter(|(_, m)| m.contains(&l))
        .map(|(t, _)| t)
        .collect();
    let via = |maintainers: &[String], ts: &[String]| -> Option<Option<String>> {
        if maintainers.iter().any(|m| m.to_lowercase() == l) {
            return Some(None);
        }
        ts.iter()
            .find(|t| teams.contains(t))
            .map(|t| Some(t.clone()))
    };
    let mut packages: BTreeMap<String, (Subject, Option<String>)> = BTreeMap::new();
    for (attr, p) in &index.pkgs {
        if let Some(how) = via(&p.maintainers, &p.teams) {
            let canonical = index.by_pname.get(&p.pname).unwrap_or(attr);
            let key = if index.pkgs.get(canonical).is_some_and(|c| c.file == p.file) {
                canonical.clone()
            } else {
                attr.clone()
            };
            packages
                .entry(key.clone())
                .or_insert((Subject::Pkg(key), how));
        }
    }
    let modules = index
        .modules
        .iter()
        .filter_map(|(f, m)| {
            via(&m.maintainers, &m.teams).map(|how| (Subject::Module(f.clone()), how))
        })
        .collect();
    Maintained {
        packages: packages.into_values().collect(),
        modules,
    }
}

fn maintained_rows(
    s: &Snapshot,
    index: &Index,
    h: &Health,
    list: &[(Subject, Option<String>)],
) -> String {
    let mut out = String::new();
    for (sub, how) in list {
        let f = Flags::of(s, index, h, sub);
        let version = match sub {
            Subject::Pkg(a) => index
                .pkgs
                .get(a)
                .map(|p| {
                    format!(
                        " <span class=\"color-fg-muted f6\">{}</span>",
                        esc(&p.version)
                    )
                })
                .unwrap_or_default(),
            _ => String::new(),
        };
        let via = how
            .as_ref()
            .map(|t| format!(" <span class=\"Label\">via {}</span>", esc(t)))
            .unwrap_or_default();
        let _ = write!(
            out,
            "<div class=\"Box-row d-flex flex-items-center flex-wrap\" style=\"gap:8px\">\
             <span class=\"flex-auto\"><a class=\"Link--primary text-bold\" href=\"{}\">{} {}</a>{version}{via}</span>{}</div>",
            sub.href(),
            sub.icon(),
            esc(&sub.label()),
            f.html()
        );
    }
    out
}

async fn maintains_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(login): Path<String>,
) -> Html<String> {
    let viewer = app.viewer(&headers);
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let m = maintained(&index, &login);
    let own = viewer
        .as_deref()
        .is_some_and(|v| v.eq_ignore_ascii_case(&login));
    let button = if own && !(m.packages.is_empty() && m.modules.is_empty()) {
        format!(
            "<form method=\"post\" action=\"/watch/maintained\" class=\"d-inline\">\
             <button class=\"btn btn-sm btn-primary\" type=\"submit\">{} Subscribe to all of them</button></form>",
            icon(octicon!("bell"))
        )
    } else {
        String::new()
    };
    let mut out = head(
        &format!("What {} maintains", esc(&login)),
        &format!(
            "Packages and NixOS modules whose <code>meta.maintainers</code> or <code>meta.teams</code> name <a href=\"/u/{0}\">{0}</a>, \
             at nixpkgs {1}. Change the list in nixpkgs, not here.",
            esc(&login),
            esc(if index.rev.is_empty() {
                "(index pending)"
            } else {
                &index.rev[..index.rev.len().min(12)]
            })
        ),
        &button,
    );
    let _ = write!(
        out,
        "<div class=\"Box mb-4\"><div class=\"Box-header text-bold\">{} Packages <span class=\"Counter\">{}</span></div>{}</div>",
        icon(octicon!("package")),
        m.packages.len(),
        if m.packages.is_empty() {
            "<div class=\"Box-row color-fg-muted\">None.</div>".into()
        } else {
            maintained_rows(&s, &index, &h, &m.packages)
        }
    );
    let _ = write!(
        out,
        "<div class=\"Box mb-4\"><div class=\"Box-header text-bold\">{} NixOS modules <span class=\"Counter\">{}</span></div>{}</div>",
        icon(octicon!("gear")),
        m.modules.len(),
        if m.modules.is_empty() {
            "<div class=\"Box-row color-fg-muted\">None.</div>".into()
        } else {
            maintained_rows(&s, &index, &h, &m.modules)
        }
    );
    page(&app, viewer.as_deref(), "packages", &out)
}

#[derive(Deserialize)]
struct WatchForm {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

async fn add_watch(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<WatchForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let subject = match (&f.key, &f.name) {
        (Some(k), _) => Subject::parse(k),
        (None, Some(n)) => Subject::resolve(&app.index.read().unwrap(), n),
        _ => None,
    };
    let Some(subject) = subject else {
        return (
            StatusCode::BAD_REQUEST,
            "not a package, a module or a nixpkgs path",
        )
            .into_response();
    };
    if let Err(e) = app.store.watch(&login, &subject.key()) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    let back = if f.name.is_some() {
        "/watch".to_string()
    } else {
        subject.href()
    };
    Redirect::to(&back).into_response()
}

async fn remove_watch(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<WatchForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let Some(key) = f.key else {
        return (StatusCode::BAD_REQUEST, "no key").into_response();
    };
    if let Err(e) = app.store.unwatch(&login, &key) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    let back = match Subject::parse(&key) {
        Some(s)
            if headers
                .get(header::REFERER)
                .and_then(|r| r.to_str().ok())
                .is_some_and(|r| !r.ends_with("/watch")) =>
        {
            s.href()
        }
        _ => "/watch".into(),
    };
    Redirect::to(&back).into_response()
}

async fn watch_maintained(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let m = maintained(&app.index.read().unwrap(), &login);
    for (sub, _) in m.packages.iter().chain(&m.modules) {
        if let Err(e) = app.store.watch(&login, &sub.key()) {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }
    Redirect::to("/watch").into_response()
}

async fn new_feed(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    if let Err(e) = app.store.set_feed_token(&login, &random_id()) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    Redirect::to("/watch#feed").into_response()
}

fn subscriptions(app: &crate::web::App, login: &str) -> Vec<Subject> {
    app.store
        .watched(login)
        .unwrap_or_default()
        .iter()
        .filter_map(|k| Subject::parse(k))
        .collect()
}

async fn watch_page(State(app): State<Shared>, headers: HeaderMap) -> Html<String> {
    let viewer = app.viewer(&headers);
    let mut out = head(
        &format!("{} Watching", icon(octicon!("bell"))),
        "Packages, modules and areas you subscribed to, with what needs attention first. \
         New pull requests, merges, issues and advisories reach you through your Atom feed.",
        "",
    );
    let Some(login) = viewer.as_deref() else {
        out.push_str("<div class=\"blankslate\"><p>Sign in to subscribe.</p></div>");
        return page(&app, None, "watch", &out);
    };
    out.push_str(
        "<form method=\"post\" action=\"/watch\" class=\"d-flex mb-3\" style=\"gap:8px;max-width:560px\">\
         <input class=\"form-control flex-auto\" name=\"name\" placeholder=\"hello, nixos/nginx or pkgs/development/python-modules\" aria-label=\"what to subscribe to\">\
         <button class=\"btn btn-primary\" type=\"submit\">Subscribe</button></form>",
    );
    let _ = write!(
        out,
        "<p class=\"f6\"><a href=\"/u/{0}/maintains\">What you maintain</a>: subscribe to all of it in one step.</p>",
        esc(login)
    );
    let subs = subscriptions(&app, login);
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    {
        let index = app.index.read().unwrap();
        if subs.is_empty() {
            out.push_str("<div class=\"blankslate\"><p>You watch nothing yet.</p></div>");
        } else {
            let mut rows: Vec<(Subject, Flags)> = subs
                .into_iter()
                .map(|sub| {
                    let f = Flags::of(&s, &index, &h, &sub);
                    (sub, f)
                })
                .collect();
            rows.sort_by_key(|(sub, f)| {
                (
                    std::cmp::Reverse(f.advisories),
                    f.upstream.is_none(),
                    std::cmp::Reverse(f.open_prs + f.open_issues),
                    sub.label(),
                )
            });
            out.push_str("<div class=\"Box mb-4\">");
            for (sub, f) in &rows {
                let _ = write!(
                    out,
                    "<div class=\"Box-row d-flex flex-items-center flex-wrap\" style=\"gap:8px\">\
                     <a class=\"Link--primary text-bold flex-auto\" href=\"{}\">{} {}</a>{}\
                     <form method=\"post\" action=\"/watch/remove\" class=\"d-inline\"><input type=\"hidden\" name=\"key\" value=\"{}\">\
                     <button class=\"btn btn-sm\" type=\"submit\">Unsubscribe</button></form></div>",
                    sub.href(),
                    sub.icon(),
                    esc(&sub.label()),
                    f.html(),
                    esc(&sub.key())
                );
            }
            out.push_str("</div>");
        }
    }
    let feed = match app.store.feed_token(login).ok().flatten() {
        Some(t) => format!(
            "<p>Add this link to a feed reader. It is private: anyone with it reads what you watch.</p>\
             <input class=\"form-control width-full mb-2\" readonly value=\"{}/feed/{}\" aria-label=\"feed link\">\
             <form method=\"post\" action=\"/me/feed\"><button class=\"btn btn-sm\" type=\"submit\">Replace the link</button></form>",
            esc(app.public_url.trim_end_matches('/')),
            esc(&t)
        ),
        None => "<form method=\"post\" action=\"/me/feed\"><button class=\"btn btn-sm\" type=\"submit\">Make a feed link</button></form>"
            .into(),
    };
    let _ = write!(
        out,
        "<div class=\"Box\" id=\"feed\"><div class=\"Box-header text-bold\">{} Atom feed</div><div class=\"Box-row\">{feed}</div></div>",
        icon(octicon!("rss"))
    );
    page(&app, viewer.as_deref(), "watch", &out)
}

struct Event {
    at: DateTime<Utc>,
    id: String,
    title: String,
    url: String,
    subject: String,
}

fn events(
    s: &Snapshot,
    index: &Index,
    h: &Health,
    subs: &[Subject],
    since: DateTime<Utc>,
) -> Vec<Event> {
    let mut out: Vec<Event> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for sub in subs {
        let label = sub.label();
        let (prs, issues) = attached(s, index, sub);
        for p in prs {
            let mut push = |at: DateTime<Utc>, verb: &str| {
                if at >= since && seen.insert(format!("pr-{}-{verb}", p.number)) {
                    out.push(Event {
                        at,
                        id: format!("pr-{}-{verb}", p.number),
                        title: format!("{verb}: #{} {}", p.number, p.title),
                        url: p.url.clone(),
                        subject: label.clone(),
                    });
                }
            };
            push(p.created_at, "pull request opened");
            if let Some(t) = p.merged_at {
                push(t, "merged");
            } else if let Some(t) = p.closed_at {
                push(t, "pull request closed");
            }
        }
        for i in issues {
            let mut push = |at: DateTime<Utc>, verb: &str| {
                if at >= since && seen.insert(format!("issue-{}-{verb}", i.number)) {
                    out.push(Event {
                        at,
                        id: format!("issue-{}-{verb}", i.number),
                        title: format!("{verb}: #{} {}", i.number, i.title),
                        url: i.url.clone(),
                        subject: label.clone(),
                    });
                }
            };
            push(i.created_at, "issue opened");
            if let Some(t) = i.closed_at {
                push(t, "issue closed");
            }
        }
        if let Subject::Pkg(attr) = sub {
            for a in open_advisories(h, s, attr) {
                let at = a
                    .issue
                    .and_then(|n| s.issue_at.get(&n))
                    .map(|&i| s.issues[i].created_at);
                if let Some(at) = at.filter(|t| *t >= since) {
                    if seen.insert(format!("advisory-{}-{attr}", a.code)) {
                        out.push(Event {
                            at,
                            id: format!("advisory-{}-{attr}", a.code),
                            title: format!("advisory {}: {}", a.code, a.title),
                            url: a.url(),
                            subject: label.clone(),
                        });
                    }
                }
            }
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.at));
    out.truncate(200);
    out
}

fn xml(s: &str) -> String {
    esc(s).replace('\'', "&apos;")
}

async fn feed(State(app): State<Shared>, Path(token): Path<String>) -> Response {
    let token = token.trim_end_matches(".atom");
    let Some(login) = app.store.feed_login(token).ok().flatten() else {
        return (StatusCode::NOT_FOUND, "no such feed").into_response();
    };
    let subs = subscriptions(&app, &login);
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let list = {
        let index = app.index.read().unwrap();
        events(
            &s,
            &index,
            &h,
            &subs,
            Utc::now() - Duration::days(FEED_DAYS),
        )
    };
    let base = app.public_url.trim_end_matches('/');
    let updated = list.first().map_or_else(Utc::now, |e| e.at);
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<feed xmlns=\"http://www.w3.org/2005/Atom\">\
         <title>Maintainer Mode: what {l} watches</title><id>{b}/feed/{l}</id><updated>{}</updated>\
         <link rel=\"alternate\" href=\"{b}/watch\"/>",
        updated.to_rfc3339(),
        l = xml(&login),
        b = xml(base)
    );
    for e in list {
        let _ = write!(
            out,
            "<entry><id>{b}/e/{}</id><title>{}: {}</title><updated>{}</updated><link href=\"{}\"/><author><name>{}</name></author></entry>",
            xml(&e.id),
            xml(&e.subject),
            xml(&e.title),
            e.at.to_rfc3339(),
            xml(&e.url),
            xml(&e.subject),
            b = xml(base)
        );
    }
    out.push_str("</feed>");
    (
        [
            (header::CONTENT_TYPE, "application/atom+xml; charset=utf-8"),
            (header::CACHE_CONTROL, "private, max-age=300"),
        ],
        out,
    )
        .into_response()
}

fn pr_json(p: &Pr) -> Value {
    json!({
        "number": p.number, "title": p.title, "url": p.url, "author": p.author,
        "state": p.state, "draft": p.is_draft, "ci": p.ci,
        "created_at": p.created_at, "merged_at": p.merged_at, "closed_at": p.closed_at,
    })
}

fn issue_json(i: &Issue) -> Value {
    json!({
        "number": i.number, "title": i.title, "url": i.url, "author": i.author,
        "state": i.state, "labels": i.labels, "comments": i.comments,
        "created_at": i.created_at, "closed_at": i.closed_at,
    })
}

fn subject_json(app: &crate::web::App, subject: &Subject, extra: Value) -> Value {
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let (prs, issues) = attached(&s, &index, subject);
    let mut v = json!({
        "key": subject.key(),
        "name": subject.label(),
        "flags": Flags::of(&s, &index, &h, subject).json(),
        "prs": prs.iter().map(|p| pr_json(p)).collect::<Vec<_>>(),
        "issues": issues.iter().map(|i| issue_json(i)).collect::<Vec<_>>(),
    });
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    v
}

async fn api_package(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    let (attr, p) = {
        let index = app.index.read().unwrap();
        match index.resolve(&name) {
            Some(a) => (a.to_string(), index.pkgs.get(a).cloned()),
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "not in the index"})),
                )
                    .into_response();
            }
        }
    };
    let job = Hydra::package_job(&attr);
    let build = app.hydra.last(&job).await;
    let advisories: Vec<Advisory> = {
        let s = app.snap();
        let h = app.health.read().unwrap().clone();
        open_advisories(&h, &s, &attr)
            .into_iter()
            .cloned()
            .collect()
    };
    let extra = json!({ "attr": attr, "package": p, "hydra": build, "hydra_job": job, "advisories": advisories });
    Json(subject_json(&app, &Subject::Pkg(attr.clone()), extra)).into_response()
}

async fn api_module(State(app): State<Shared>, Path(file): Path<String>) -> Response {
    let Some(m) = app.index.read().unwrap().modules.get(&file).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "not a NixOS module in the index"})),
        )
            .into_response();
    };
    let job = Hydra::test_job(module_name(&file).trim_start_matches("nixos/"));
    let build = app.hydra.last(&job).await;
    let extra = json!({ "file": file, "module": m, "test": build, "test_job": job });
    Json(subject_json(&app, &Subject::Module(file.clone()), extra)).into_response()
}

async fn api_area(State(app): State<Shared>, Path(path): Path<String>) -> Response {
    let path = path.trim_matches('/').to_string();
    if !valid_path(&path) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "not a nixpkgs path"})),
        )
            .into_response();
    }
    let contents: Vec<Value> = {
        let s = app.snap();
        let h = app.health.read().unwrap().clone();
        let index = app.index.read().unwrap();
        contents(&index, &s, &h, &path)
            .into_iter()
            .map(|(sub, f)| json!({ "key": sub.key(), "name": sub.label(), "flags": f.json() }))
            .collect()
    };
    Json(subject_json(
        &app,
        &Subject::Area(path.clone()),
        json!({ "contents": contents }),
    ))
    .into_response()
}

async fn api_maintains(State(app): State<Shared>, Path(login): Path<String>) -> Json<Value> {
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let m = maintained(&index, &login);
    let rows = |list: &[(Subject, Option<String>)]| -> Vec<Value> {
        list.iter()
            .map(|(sub, how)| {
                let version = match sub {
                    Subject::Pkg(a) => index.pkgs.get(a).map(|p| p.version.clone()),
                    _ => None,
                };
                json!({
                    "key": sub.key(), "name": sub.label(), "version": version,
                    "via_team": how, "flags": Flags::of(&s, &index, &h, sub).json(),
                })
            })
            .collect()
    };
    Json(json!({
        "login": login,
        "rev": index.rev,
        "packages": rows(&m.packages),
        "modules": rows(&m.modules),
    }))
}

/// Sends a name to the API of what it resolves to, so a client needs to know
/// only the name.
async fn api_find(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    match Subject::resolve(&app.index.read().unwrap(), &name) {
        Some(sub) => Redirect::temporary(&format!("/api/v1{}", api_path(&sub))).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "not a package, a module or a nixpkgs path"})),
        )
            .into_response(),
    }
}

fn api_path(sub: &Subject) -> String {
    match sub {
        Subject::Pkg(a) => format!("/packages/{a}"),
        Subject::Module(f) => format!("/modules/{f}"),
        Subject::Area(p) => format!("/areas/{p}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::area::{Module, Package};

    fn index() -> Index {
        let mut i = Index::default();
        for (attr, pname, m, t) in [
            ("hello", "hello", vec!["Alice"], vec![]),
            ("python3Packages.foo", "foo", vec![], vec!["py"]),
        ] {
            i.pkgs.insert(
                attr.into(),
                Package {
                    pname: pname.into(),
                    version: "1".into(),
                    file: Some(format!("pkgs/{pname}/default.nix")),
                    maintainers: m.into_iter().map(String::from).collect(),
                    teams: t.into_iter().map(String::from).collect(),
                },
            );
            i.by_pname.insert(pname.into(), attr.into());
        }
        i.modules.insert(
            "nixos/modules/services/misc/gitea.nix".into(),
            Module {
                maintainers: vec!["alice".into()],
                teams: vec![],
            },
        );
        i.teams.insert("py".into(), vec!["alice".into()]);
        i
    }

    #[test]
    fn input_resolves_to_a_package_a_module_or_an_area() {
        let i = index();
        assert_eq!(
            Subject::resolve(&i, "hello"),
            Some(Subject::Pkg("hello".into()))
        );
        assert_eq!(
            Subject::resolve(&i, "foo"),
            Some(Subject::Pkg("python3Packages.foo".into()))
        );
        assert_eq!(
            Subject::resolve(&i, "nixos/gitea"),
            Some(Subject::Module(
                "nixos/modules/services/misc/gitea.nix".into()
            ))
        );
        assert_eq!(
            Subject::resolve(&i, "pkgs/development/"),
            Some(Subject::Area("pkgs/development".into()))
        );
        assert_eq!(Subject::resolve(&i, "../etc"), None);
        assert_eq!(
            Subject::parse("pkg:hello"),
            Some(Subject::Pkg("hello".into()))
        );
        assert_eq!(Subject::parse("evil:x"), None);
    }

    #[test]
    fn a_login_maintains_directly_and_through_a_team() {
        let m = maintained(&index(), "ALICE");
        assert_eq!(
            m.packages,
            vec![
                (Subject::Pkg("hello".into()), None),
                (
                    Subject::Pkg("python3Packages.foo".into()),
                    Some("py".into())
                )
            ]
        );
        assert_eq!(m.modules.len(), 1);
    }

    fn issue(number: u64, state: &str) -> Issue {
        Issue {
            number,
            title: format!("issue {number}"),
            url: format!("https://github.com/NixOS/nixpkgs/issues/{number}"),
            author: "alice".into(),
            state: state.into(),
            created_at: crate::ledger::tests::t(0),
            updated_at: crate::ledger::tests::t(0),
            closed_at: None,
            labels: vec![],
            comments: 0,
            mentions: vec![],
        }
    }

    #[test]
    fn an_advisory_whose_issue_the_sync_never_saw_is_closed_once_the_sync_is_complete() {
        let mut h = Health::default();
        h.advisories.insert(
            "hello".into(),
            [(None, "A"), (Some(1), "B"), (Some(2), "C"), (Some(3), "D")]
                .into_iter()
                .map(|(issue, code)| Advisory {
                    code: code.into(),
                    cves: vec![],
                    title: code.into(),
                    issue,
                })
                .collect(),
        );
        let mut s = Snapshot {
            issues: vec![issue(1, "OPEN"), issue(2, "CLOSED")],
            ..Default::default()
        };
        s.issue_at = [(1, 0), (2, 1)].into();
        let codes = |s: &Snapshot| -> Vec<String> {
            open_advisories(&h, s, "hello")
                .iter()
                .map(|a| a.code.clone())
                .collect()
        };
        assert_eq!(codes(&s), ["A", "B", "D"]);
        s.issues_complete = true;
        assert_eq!(codes(&s), ["A", "B"]);
    }

    #[test]
    fn open_pull_requests_count_for_every_directory_above_a_file() {
        let mut p = crate::ledger::tests::pr(1, "a", 0);
        p.files = vec!["pkgs/a/b.nix".into(), "pkgs/a/c.nix".into()];
        let d = open_by_dir(&[p]);
        assert_eq!(d["pkgs"], 1);
        assert_eq!(d["pkgs/a"], 1);
    }
}
