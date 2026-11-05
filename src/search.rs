//! The search box of the header. A name that has a page goes to it; other
//! words list the packages, modules, directories, people, pull requests and
//! advisories that match. A qualifier such as `is:outdated` or `by:login`
//! lists the packages of all of nixpkgs with that trait, in the grammar of
//! [`crate::query`]; see [`crate::catalog::filter`] for the words.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::area::Index;
use crate::catalog::filter::{self, Failing, Filter, Has, Is};
use crate::catalog::{Flags, Subject, valid_login};
use crate::health::Health;
use crate::html::{Html, html};
use crate::query::Problem;
use crate::ui::{Kind, Page, Section, item_row, pager, section};
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
    after: Option<u64>,
}

#[derive(Default)]
pub(crate) struct Hits {
    /// With a qualifier in the query, the whole list of the packages it
    /// keeps, in its order; the page shows one page of it.
    pub subjects: Vec<Subject>,
    pub people: Vec<String>,
    pub prs: Vec<Pr>,
    pub advisories: Vec<Advisory>,
    /// The query had a qualifier.
    pub filtered: bool,
    /// The words of the query that this site does not know.
    pub problems: Vec<Problem>,
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

/// The problems of a query, each with the query as it was likely meant,
/// as a link that `href` makes.
pub(crate) fn problem_lines(q: &str, problems: &[Problem], href: impl Fn(&str) -> String) -> Html {
    let mut out = Html::new();
    for p in problems {
        let fix = p
            .suggestion
            .as_ref()
            .map(|s| {
                let to = q.replacen(&p.token, s, 1);
                html!(": <a href=\"{}\">{s}</a>?", href(&to))
            })
            .unwrap_or_default();
        out.push(&html!(
            "<p class=\"f6 color-fg-danger mb-1\" data-problem>{}{fix}</p>",
            p.message
        ));
    }
    out
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

/// Where a query goes without a list of hits. A query with a qualifier
/// always lists.
pub(crate) fn jump(s: &Snapshot, index: &Index, q: &str) -> Option<String> {
    if !Filter::parse(q).is_plain() {
        return None;
    }
    if let Some(login) = q.strip_prefix('@').filter(|l| valid_login(l)) {
        return Some(format!("/u/{login}"));
    }
    if let Some(href) = crate::catalog::exact(s, index, q) {
        return Some(href);
    }
    pr_number(q).map(|n| format!("/pr/{n}"))
}

pub(crate) fn find(app: &App, s: &Snapshot, index: &Index, h: &Health, q: &str) -> Hits {
    let q = q.trim();
    if q.is_empty() {
        return Hits::default();
    }
    let f = Filter::parse(q);
    if f.is_plain() && f.problems.is_empty() {
        let ql = q.trim_start_matches('@').to_lowercase();
        return Hits {
            subjects: crate::catalog::search(s, index, q),
            people: people(index, &ql),
            prs: prs(s, &ql),
            advisories: advisories(h, q),
            ..Default::default()
        };
    }
    let stats = app.packages.read().unwrap().clone();
    let failing = f.asks_hydra().then(|| Failing::gather(&app.hydra, index));
    let subjects = filter::select(&f, index, &stats.traits, failing.as_ref())
        .into_iter()
        .map(|(attr, _)| Subject::Pkg(attr.clone()))
        .collect();
    // The other kinds match the words of a name, as without qualifiers.
    let ql = f.text.trim_start_matches('@');
    let (people, prs, advisories) = if f.text.is_empty() {
        Default::default()
    } else {
        (people(index, ql), prs(s, ql), advisories(h, &f.text))
    };
    Hits {
        subjects,
        people,
        prs,
        advisories,
        filtered: true,
        problems: f.problems,
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
    let (hits, rows, next) = {
        let index = app.index.read().unwrap();
        if let Some(href) = jump(&s, &index, &q) {
            return Redirect::to(&href).into_response();
        }
        let hits = find(&app, &s, &index, &h, &q);
        let (shown, next) = if hits.filtered {
            crate::review::paged_by_offset(&hits.subjects, p.after)
        } else {
            (&hits.subjects[..], None)
        };
        let rows: Vec<(Subject, Flags)> = shown
            .iter()
            .map(|sub| (sub.clone(), Flags::of(&s, &index, &h, sub)))
            .collect();
        (hits, rows, next)
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
        .body(body(&q, &hits, &rows, next.as_deref()))
        .render(&app, viewer.as_deref())
}

const HELP: &str = "<ul class=\"ml-3\">\
<li>a package: <code>hello</code>, <code>python3Packages.requests</code></li>\
<li>a NixOS module: <code>nixos/nginx</code></li>\
<li>a directory: <code>pkgs/development/python-modules</code></li>\
<li>a person: <code>@login</code></li>\
<li>a pull request: <code>#123456</code> or its link</li>\
<li>an advisory: <code>CVE-2024-3094</code>, <code>GHSA-…</code>, <code>NIXPKGS-2026-…</code></li>\
</ul>";

/// The qualifiers, as GitHub's and nixkeeper's, so one query works on both
/// sites.
fn filter_help() -> Html {
    let word = |w: String, about: &str| {
        html!(
            "<li><a href=\"/search?q={}\"><code>{w}</code></a> {about}</li>",
            crate::github::urlencode(&w)
        )
    };
    let is: Html = Is::ALL
        .iter()
        .map(|i| word(format!("is:{}", i.word()), i.about()))
        .collect();
    let has: Html = Has::ALL
        .iter()
        .map(|h| word(format!("has:{}", h.word()), h.about()))
        .collect();
    html!(
        "<p class=\"mt-3\">Or packages with a trait, with the words of <a href=\"https://nixkeeper.com/\">nixkeeper</a> \
         and GitHub: <code>is:outdated -has:pr in:python3Packages</code>. A <code>-</code> leaves them out. \
         <code>in:</code> is a package set, <code>by:</code> a maintainer, <code>-maintainer:</code> one left out, \
         <code>team:</code> a team (<code>team:none</code>, <code>-team:</code>), \
         <code>sort:az</code> and <code>order:reverse</code> the order; the highest risk comes first otherwise.</p>\
         <ul class=\"ml-3\">{is}{has}</ul>\
         <p class=\"f6 color-fg-muted mt-2\">Press <kbd>/</kbd> on any page to search.</p>"
    )
}

/// A search box in the page. `search-inline` alone shows it only on a
/// phone, where the header has no box; `inline-form` in `class` comes later
/// in club.css and shows it on every screen, as on this page.
pub(crate) fn page_form(q: &str, class: &str) -> Html {
    html!(
        "<form class=\"search-inline {class}\" role=\"search\" method=\"get\" action=\"/search\">\
         <input class=\"form-control flex-auto\" type=\"search\" name=\"q\" value=\"{q}\" placeholder=\"{}\" aria-label=\"Search\" autocomplete=\"off\" data-search>\
         <button class=\"btn\" type=\"submit\">Search</button></form>",
        crate::ui::SEARCH_PLACEHOLDER
    )
}

fn body(q: &str, hits: &Hits, rows: &[(Subject, Flags)], next: Option<&str>) -> Html {
    let help = html!("{}{}", Html::from(HELP), filter_help());
    let mut out = page_form(q, "inline-form mb-2");
    out.push(&html!(
        "<div class=\"mb-4\">{}</div>",
        problem_lines(q, &hits.problems, |to| {
            format!("/search?q={}", crate::github::urlencode(to))
        })
    ));
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
    if hits.filtered {
        if !hits.subjects.is_empty() {
            let mut list = Html::new();
            for (sub, f) in rows {
                list.push(&crate::catalog::subject_row(sub, f, "", ""));
            }
            out.push(&section("Packages", Some(hits.subjects.len()), None, list));
            out.push(&pager(
                &format!("/search?q={}", crate::github::urlencode(q)),
                next,
            ));
        }
    } else if !rows.is_empty() {
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

/// The hits of `/search`, as JSON: one page of `results`, `limit` rows
/// from `cursor`, with `total` and `next_cursor`; at most `limit` of each
/// other kind. A query that `/search` would send to a page also names
/// that page.
pub(crate) fn value(
    app: &App,
    q: &str,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<Value, String> {
    let q = q.trim();
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let (mut hits, page) = {
        let index = app.index.read().unwrap();
        (find(app, &s, &index, &h, q), jump(&s, &index, q))
    };
    let total = hits.subjects.len();
    let (range, next) = crate::api::window(total, limit, cursor)?;
    let subjects: Vec<Subject> = hits.subjects.drain(range).collect();
    if let Some(n) = limit {
        hits.people.truncate(n);
        hits.prs.truncate(n);
        hits.advisories.truncate(n);
    }
    let mut results = crate::catalog::subject_list(app, &subjects);
    for (r, sub) in results.iter_mut().zip(&subjects) {
        let (kind, url) = match sub {
            Subject::Pkg(a) => ("pkg", format!("/p/{a}")),
            Subject::Module(f) => ("module", format!("/m/{f}")),
            Subject::Area(p) => ("area", format!("/a/{p}")),
        };
        r["kind"] = json!(kind);
        r["url"] = json!(url);
    }
    Ok(json!({
        "query": q,
        "page": page,
        "problems": hits.problems,
        "results": results,
        "total": total,
        "next_cursor": next,
        "people": hits.people.iter().map(|l| json!({"login": l, "url": format!("/u/{l}")})).collect::<Vec<_>>(),
        "pull_requests": hits.prs.iter().map(|p| json!({
            "number": p.number, "title": p.title, "state": p.state, "draft": p.draft, "url": format!("/pr/{}", p.number),
        })).collect::<Vec<_>>(),
        "advisories": hits.advisories.iter().map(|a| json!({
            "id": a.id, "title": a.title, "url": a.url, "packages": a.packages,
        })).collect::<Vec<_>>(),
    }))
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
