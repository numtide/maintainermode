//! The search box of the header. A name that has a page goes to it; other
//! words list the packages, modules, directories, people, pull requests and
//! advisories that match.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::area::Index;
use crate::catalog::{Flags, Subject, valid_login};
use crate::health::Health;
use crate::html::{Html, html};
use crate::ui::{Kind, Page, Section, item_row, section};
use crate::web::{App, Shared, Snapshot, icon, octicon};

const PEOPLE: usize = 20;
const PRS: usize = 30;
/// Shorter words match too many pull request titles to help.
const PR_MIN_CHARS: usize = 3;

pub fn routes() -> Router<Shared> {
    Router::new().route("/search", get(page))
}

#[derive(Deserialize)]
struct Params {
    q: Option<String>,
}

#[derive(Default)]
pub(crate) struct Hits {
    pub subjects: Vec<Subject>,
    pub people: Vec<String>,
    pub prs: Vec<Pr>,
    pub advisories: Vec<Advisory>,
}

pub(crate) struct Pr {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub draft: bool,
}

pub(crate) struct Advisory {
    pub id: String,
    pub title: String,
    pub url: String,
    pub packages: Vec<String>,
}

impl Hits {
    fn is_empty(&self) -> bool {
        self.subjects.is_empty()
            && self.people.is_empty()
            && self.prs.is_empty()
            && self.advisories.is_empty()
    }
}

/// `#123`, `123` or a link to the pull request.
fn pr_number(q: &str) -> Option<u64> {
    let q = q.trim();
    let n = match q.find("github.com/NixOS/nixpkgs/pull/") {
        Some(i) => q[i + "github.com/NixOS/nixpkgs/pull/".len()..]
            .split(['/', '#', '?'])
            .next()?,
        None => q.strip_prefix('#').unwrap_or(q),
    };
    n.parse().ok().filter(|&n| n > 0)
}

/// Where a query goes without a list of hits.
pub(crate) fn jump(s: &Snapshot, index: &Index, q: &str) -> Option<String> {
    if let Some(login) = q.strip_prefix('@').filter(|l| valid_login(l)) {
        return Some(format!("/u/{login}"));
    }
    if let Some(href) = crate::catalog::exact(s, index, q) {
        return Some(href);
    }
    pr_number(q).map(|n| format!("/pr/{n}"))
}

pub(crate) fn find(s: &Snapshot, index: &Index, h: &Health, q: &str) -> Hits {
    let q = q.trim();
    if q.is_empty() {
        return Hits::default();
    }
    let ql = q.trim_start_matches('@').to_lowercase();
    Hits {
        subjects: crate::catalog::search(s, index, q),
        people: people(index, &ql),
        prs: prs(s, &ql),
        advisories: advisories(h, q),
    }
}

fn people(index: &Index, ql: &str) -> Vec<String> {
    let mut out: Vec<&String> = index
        .maintainers
        .keys()
        .filter(|l| l.to_lowercase().contains(ql))
        .collect();
    out.sort_by_key(|l| (l.to_lowercase() != ql, l.len(), l.as_str()));
    out.into_iter().take(PEOPLE).cloned().collect()
}

/// `hel` finds "hello: 2.12.1 -> 2.12.2" but not "add shell completions".
pub(crate) fn at_word(text: &str, ql: &str) -> bool {
    text.match_indices(ql).any(|(i, _)| {
        !text[..i]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
    })
}

/// Open ones first, then the most recently updated.
fn prs(s: &Snapshot, ql: &str) -> Vec<Pr> {
    if ql.chars().count() < PR_MIN_CHARS {
        return Vec::new();
    }
    let mut out: Vec<&crate::model::Pr> = s
        .pr_data
        .iter()
        .filter(|p| at_word(&p.title.to_lowercase(), ql))
        .collect();
    out.sort_by_key(|p| (!p.is_open(), std::cmp::Reverse(p.updated_at)));
    out.into_iter()
        .take(PRS)
        .map(|p| Pr {
            number: p.number,
            title: p.title.clone(),
            state: p.state.clone(),
            draft: p.is_draft,
        })
        .collect()
}

/// An id of the security tracker, a CVE, or an OSV id such as GHSA-…;
/// never words, which would match every title.
fn advisories(h: &Health, q: &str) -> Vec<Advisory> {
    let id = q.to_uppercase();
    if !id.contains('-') || id.contains(char::is_whitespace) {
        return Vec::new();
    }
    let mut found: BTreeMap<String, Advisory> = BTreeMap::new();
    let mut add = |key: &str, title: &str, url: String, attr: &str| {
        let a = found.entry(key.to_string()).or_insert_with(|| Advisory {
            id: key.to_string(),
            title: title.to_string(),
            url,
            packages: Vec::new(),
        });
        if !a.packages.iter().any(|p| p == attr) {
            a.packages.push(attr.to_string());
        }
    };
    for (attr, list) in &h.advisories {
        for a in list {
            if a.code.eq_ignore_ascii_case(&id)
                || a.cves.iter().any(|c| c.eq_ignore_ascii_case(&id))
            {
                add(&a.code, &a.title, a.url(), attr);
            }
        }
    }
    for (attr, ids) in &h.osv {
        for o in ids {
            let v = h.osv_vulns.get(o);
            let hit = o.eq_ignore_ascii_case(&id)
                || v.is_some_and(|v| {
                    v.aliases
                        .iter()
                        .chain(&v.cves)
                        .any(|a| a.eq_ignore_ascii_case(&id))
                });
            if hit {
                let title = v.and_then(|v| v.summary.as_deref()).unwrap_or("");
                add(o, title, crate::osv::Vuln::url(o), attr);
            }
        }
    }
    let mut out: Vec<Advisory> = found.into_values().collect();
    for a in &mut out {
        a.packages.sort();
    }
    out
}

async fn page(State(app): State<Shared>, headers: HeaderMap, Query(p): Query<Params>) -> Response {
    let viewer = app.viewer(&headers);
    let q = p.q.unwrap_or_default().trim().to_string();
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let (hits, rows) = {
        let index = app.index.read().unwrap();
        if let Some(href) = jump(&s, &index, &q) {
            return Redirect::to(&href).into_response();
        }
        let hits = find(&s, &index, &h, &q);
        let rows: Vec<(Subject, Flags)> = hits
            .subjects
            .iter()
            .map(|sub| (sub.clone(), Flags::of(&s, &index, &h, sub)))
            .collect();
        (hits, rows)
    };
    let title = if q.is_empty() {
        "Search".to_string()
    } else {
        format!("Search: {q}")
    };
    Page::new(Section::Other, title)
        .heading("Search")
        .icon(octicon!("search"))
        .query(&q)
        .body(body(&q, &hits, &rows))
        .render(&app, viewer.as_deref())
}

const HELP: &str = "<ul class=\"ml-3\">\
<li>a package: <code>hello</code>, <code>python3Packages.requests</code></li>\
<li>a NixOS module: <code>nixos/nginx</code></li>\
<li>a directory: <code>pkgs/development/python-modules</code></li>\
<li>a person: <code>@login</code></li>\
<li>a pull request: <code>#123456</code> or its link</li>\
<li>an advisory: <code>CVE-2024-3094</code>, <code>GHSA-…</code>, <code>NIXPKGS-2026-…</code></li>\
</ul><p class=\"f6 color-fg-muted mt-2\">Press <kbd>/</kbd> on any page to search.</p>";

/// On a phone the header has no box, so the page brings its own.
fn body(q: &str, hits: &Hits, rows: &[(Subject, Flags)]) -> Html {
    let help = Html::from(HELP);
    let mut out = html!(
        "<form class=\"search-inline inline-form mb-4\" role=\"search\" method=\"get\" action=\"/search\">\
         <input class=\"form-control flex-auto\" type=\"search\" name=\"q\" value=\"{q}\" placeholder=\"{}\" aria-label=\"Search\" autocomplete=\"off\" data-search>\
         <button class=\"btn\" type=\"submit\">Search</button></form>",
        crate::ui::SEARCH_PLACEHOLDER
    );
    if q.is_empty() {
        out.push(&html!(
            "<div class=\"Box p-3 mb-4\"><p>Type one of these in the search box:</p>{help}</div>"
        ));
        return out;
    }
    if hits.is_empty() {
        out.push(&html!(
            "<div class=\"blankslate\"><p>Nothing matches <strong>{q}</strong>.</p></div><div class=\"Box p-3 mb-4\"><p>You can search for:</p>{help}</div>"
        ));
        return out;
    }
    if !rows.is_empty() {
        out.push(&section(
            "Packages, modules and directories",
            Some(rows.len()),
            None,
            crate::catalog::subject_rows(rows),
        ));
    }
    if !hits.people.is_empty() {
        let list: Html = hits
            .people
            .iter()
            .map(|l| {
                item_row(
                    None,
                    html!("{} {l}", icon(octicon!("person"))),
                    &format!("/u/{l}"),
                    "",
                    &[],
                    "",
                )
            })
            .collect();
        out.push(&section("People", Some(hits.people.len()), None, list));
    }
    if !hits.prs.is_empty() {
        let list: Html = hits
            .prs
            .iter()
            .map(|p| {
                let kind = Kind::of_pr(&p.state);
                let draft = if p.draft { " · draft" } else { "" };
                item_row(
                    Some(kind),
                    Html::text(&p.title),
                    &format!("/pr/{}", p.number),
                    html!("#{}{draft}", p.number),
                    &[],
                    "",
                )
            })
            .collect();
        out.push(&section("Pull requests", Some(hits.prs.len()), None, list));
    }
    if !hits.advisories.is_empty() {
        let list: Html = hits
            .advisories
            .iter()
            .map(|a| {
                let pkgs = crate::html::join(
                    a.packages
                        .iter()
                        .map(|p| html!("<a href=\"/p/{0}\">{0}</a>", p)),
                    ", ",
                );
                let title = if a.title.is_empty() {
                    Html::new()
                } else {
                    html!(" {}", a.title)
                };
                item_row(
                    None,
                    html!(
                        "{} <a class=\"Link--primary text-bold\" href=\"{}\">{}</a>{title}",
                        icon(octicon!("shield")),
                        a.url,
                        a.id
                    ),
                    "",
                    html!("affects {pkgs}"),
                    &[],
                    "",
                )
            })
            .collect();
        out.push(&section(
            "Advisories",
            Some(hits.advisories.len()),
            None,
            list,
        ));
    }
    out
}

/// The hits of `/search`, as JSON, at most `limit` of each kind. A query
/// that `/search` would send to a page also names that page.
pub(crate) fn value(app: &App, q: &str, limit: Option<usize>) -> Value {
    let q = q.trim();
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let (mut hits, page) = {
        let index = app.index.read().unwrap();
        (find(&s, &index, &h, q), jump(&s, &index, q))
    };
    if let Some(n) = limit {
        hits.subjects.truncate(n);
        hits.people.truncate(n);
        hits.prs.truncate(n);
        hits.advisories.truncate(n);
    }
    let mut results = crate::catalog::subject_list(app, &hits.subjects);
    for (r, sub) in results.iter_mut().zip(&hits.subjects) {
        let (kind, url) = match sub {
            Subject::Pkg(a) => ("pkg", format!("/p/{a}")),
            Subject::Module(f) => ("module", format!("/m/{f}")),
            Subject::Area(p) => ("area", format!("/a/{p}")),
        };
        r["kind"] = json!(kind);
        r["url"] = json!(url);
    }
    json!({
        "query": q,
        "page": page,
        "results": results,
        "people": hits.people.iter().map(|l| json!({"login": l, "url": format!("/u/{l}")})).collect::<Vec<_>>(),
        "pull_requests": hits.prs.iter().map(|p| json!({
            "number": p.number, "title": p.title, "state": p.state, "draft": p.draft, "url": format!("/pr/{}", p.number),
        })).collect::<Vec<_>>(),
        "advisories": hits.advisories.iter().map(|a| json!({
            "id": a.id, "title": a.title, "url": a.url, "packages": a.packages,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_request_numbers() {
        assert_eq!(pr_number("#123"), Some(123));
        assert_eq!(pr_number("123"), Some(123));
        assert_eq!(
            pr_number("https://github.com/NixOS/nixpkgs/pull/456/files#diff"),
            Some(456)
        );
        assert_eq!(pr_number("hello"), None);
        assert_eq!(pr_number("#0"), None);
    }

    #[test]
    fn an_advisory_id_lists_the_packages_it_affects() {
        let mut h = Health::default();
        let adv = crate::health::Advisory {
            code: "NIXPKGS-2026-1".into(),
            cves: vec!["CVE-2024-3094".into()],
            title: "xz backdoor".into(),
            issue: None,
            ranges: Vec::new(),
        };
        h.advisories.insert("xz".into(), vec![adv.clone()]);
        h.advisories.insert("xz_5".into(), vec![adv]);
        h.osv.insert("xz".into(), vec!["GHSA-x".into()]);
        h.osv_vulns.insert(
            "GHSA-x".into(),
            crate::osv::Vuln {
                cves: vec!["CVE-2024-3094".into()],
                ..Default::default()
            },
        );
        let a = advisories(&h, "cve-2024-3094");
        let ids: Vec<&str> = a.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["GHSA-x", "NIXPKGS-2026-1"]);
        assert_eq!(a[1].packages, ["xz", "xz_5"]);
        assert!(advisories(&h, "xz backdoor").is_empty());
    }

    #[test]
    fn titles_match_at_the_start_of_a_word() {
        assert!(at_word("hello: 2.12.1 -> 2.12.2", "hel"));
        assert!(at_word("python3packages.hello: init", "hel"));
        assert!(!at_word("ripgrep: add shell completions", "hel"));
        assert!(at_word("ripgrep: add shell completions", "shell comp"));
    }

    #[test]
    fn people_match_by_login() {
        let mut index = Index::default();
        for l in ["zimbatm", "zimbat", "alice"] {
            index.maintainers.insert(l.into(), Default::default());
        }
        assert_eq!(people(&index, "zimbatm"), ["zimbatm"]);
        assert_eq!(people(&index, "zim"), ["zimbat", "zimbatm"]);
    }
}
