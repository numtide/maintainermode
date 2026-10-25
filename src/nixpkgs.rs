//! The "nixpkgs" place at `/state`: the numbers, the rules of this site, the
//! docs and what the server has read. Each is a tab.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Redirect, Response};
use axum::routing::get;
use serde::Deserialize;
use std::fmt::Write;

use crate::html::{Html, html};
use crate::ui::{Page, Section, Tab};
use crate::web::{Shared, octicon};

const RULES: &str = include_str!("../docs/rules.md");

const REPO: &str = "https://github.com/numtide/maintainermode/blob/main";

const DOCS: [(&str, &str, &str); 10] = [
    (
        "health",
        "Package pages, the risk score, upstream versions and channels.",
        include_str!("../docs/health.md"),
    ),
    (
        "personal",
        "What you maintain and watch, and the packages of your machines.",
        include_str!("../docs/personal.md"),
    ),
    (
        "data",
        "How the data is read, what is kept, and its limits.",
        include_str!("../docs/data.md"),
    ),
    (
        "operations",
        "How to run the server, and its variables.",
        include_str!("../docs/operations.md"),
    ),
    (
        "api",
        "The JSON API, its paging, and the command line.",
        include_str!("../docs/api.md"),
    ),
    (
        "events",
        "The plan of the event log.",
        include_str!("../docs/events.md"),
    ),
    (
        "ingest",
        "The plan of the archive of issues and pull requests.",
        include_str!("../docs/ingest.md"),
    ),
    (
        "rescan",
        "Server rescans of public flakes, and their threat model.",
        include_str!("../docs/rescan.md"),
    ),
    (
        "file-matching",
        "Design: match pull requests and issues by the nixpkgs files a scan read.",
        include_str!("../docs/file-matching.md"),
    ),
    (
        "architecture",
        "Plan: one source for GitHub data, typed HTML, data apart from views.",
        include_str!("../docs/architecture.md"),
    ),
];

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/state", get(state_page))
        .route(
            "/rules",
            get(|| async { Redirect::permanent("/state?tab=rules") }),
        )
        .route(
            "/sync",
            get(|| async { Redirect::permanent("/state?tab=status") }),
        )
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Numbers,
    Rules,
    Docs,
    Status,
}

impl View {
    const ALL: [View; 4] = [View::Numbers, View::Rules, View::Docs, View::Status];

    fn parse(tab: Option<&str>) -> Self {
        match tab {
            Some("rules") => View::Rules,
            Some("docs") => View::Docs,
            Some("status") => View::Status,
            _ => View::Numbers,
        }
    }

    fn href(self) -> &'static str {
        match self {
            View::Numbers => "/state",
            View::Rules => "/state?tab=rules",
            View::Docs => "/state?tab=docs",
            View::Status => "/state?tab=status",
        }
    }

    fn label(self) -> &'static str {
        match self {
            View::Numbers => "Numbers",
            View::Rules => "Rules",
            View::Docs => "Docs",
            View::Status => "Data status",
        }
    }
}

/// The address of each tab, for the table of `crate::api`.
#[cfg(test)]
pub(crate) fn views() -> Vec<&'static str> {
    View::ALL.iter().map(|v| v.href()).collect()
}

fn tabs(current: View) -> Vec<Tab> {
    View::ALL
        .iter()
        .map(|&v| Tab::new(v.href(), v.label(), v == current))
        .collect()
}

#[derive(Deserialize)]
struct StateQuery {
    range: Option<String>,
    tab: Option<String>,
    doc: Option<String>,
}

async fn state_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<StateQuery>,
) -> Response {
    let viewer = app.viewer(&headers);
    let view = View::parse(q.tab.as_deref());
    let page = match view {
        View::Numbers => Page::new(Section::Nixpkgs, "State of nixpkgs")
            .sub(
                "Pull request flow and package health from public data. \
                 Aggregates only: no figure names a person. \
                 <a href=\"/state?tab=rules#the-state-of-nixpkgs\">How each number is made</a> · \
                 <a href=\"/api/v1/metrics\">JSON</a>",
            )
            .body(crate::numbers::numbers(
                &app,
                crate::numbers::Range::parse(q.range.as_deref()),
            )),
        View::Rules => {
            let (title, html) = markdown(RULES);
            Page::new(Section::Nixpkgs, title).body(html!(
                "{}<article class=\"markdown-body\">{html}</article>",
                crate::web::nixpkgs_guides()
            ))
        }
        View::Docs => docs(q.doc.as_deref()),
        View::Status => Page::new(Section::Nixpkgs, "Data status")
            .sub(
                "What this server has read from GitHub and the other sources, and how fresh it is. \
                 The same data is at <a href=\"/api/v1/sync\">/api/v1/sync</a>.",
            )
            .body(crate::status::body(&app)),
    };
    page.icon(octicon!("graph"))
        .subnav(tabs(view))
        .render(&app, viewer.as_deref())
}

fn docs(doc: Option<&str>) -> Page {
    if let Some((id, _, src)) = DOCS.iter().find(|(id, ..)| Some(*id) == doc) {
        let (title, html) = markdown(src);
        return Page::new(Section::Nixpkgs, title)
            .sub(html!(
                "<a href=\"/state?tab=docs\">All docs</a> · The source is \
                 <a href=\"{REPO}/docs/{id}.md\">docs/{id}.md</a>."
            ))
            .body(html!("<article class=\"markdown-body\">{html}</article>"));
    }
    let missing = match doc {
        Some(d) => crate::ui::note(html!("There is no doc named <code>{d}</code>.")),
        None => Html::new(),
    };
    let rows: Html = DOCS
        .iter()
        .map(|(id, what, src)| {
            let title = src
                .strip_prefix("# ")
                .and_then(|s| s.lines().next())
                .unwrap_or(id);
            crate::ui::item_row(
                None,
                Html::text(title),
                &format!("/state?tab=docs&doc={id}"),
                *what,
                &[],
                "",
            )
        })
        .collect();
    Page::new(Section::Nixpkgs, "Docs")
        .sub(html!(
            "How Maintainer Mode works, from the <a href=\"{REPO}/docs\">docs</a> in its source. \
             The rules have <a href=\"/state?tab=rules\">their own tab</a>."
        ))
        .body(html!(
            "{missing}{}",
            crate::ui::section("Docs", Some(DOCS.len()), None, rows)
        ))
}

/// A link from one doc to another opens its tab here. A link to another
/// file of the source opens it on GitHub.
fn doc_link(dest: &str) -> String {
    if dest.starts_with('#') || dest.contains("://") || dest.starts_with("mailto:") {
        return dest.to_string();
    }
    let (path, frag) = match dest.split_once('#') {
        Some((p, f)) => (p, format!("#{f}")),
        None => (dest, String::new()),
    };
    let path = path.trim_start_matches("./");
    if let Some(id) = path.strip_suffix(".md").filter(|id| !id.contains('/')) {
        if id == "rules" {
            return format!("/state?tab=rules{frag}");
        }
        if DOCS.iter().any(|(d, ..)| *d == id) {
            return format!("/state?tab=docs&doc={id}{frag}");
        }
    }
    match path.strip_prefix("../") {
        Some(p) => format!("{REPO}/{p}{frag}"),
        None => format!("{REPO}/docs/{path}{frag}"),
    }
}

/// Splits off the `# ` title, so the page header shows it once.
fn markdown(src: &str) -> (String, Html) {
    use pulldown_cmark::{Event, Options, Parser, Tag};
    let (title, rest) = match src.strip_prefix("# ") {
        Some(s) => s.split_once('\n').unwrap_or((s, "")),
        None => ("", src),
    };
    let events = Parser::new_ext(rest, Options::ENABLE_TABLES).map(|e| match e {
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: doc_link(&dest_url).into(),
            title,
            id,
        }),
        e => e,
    });
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, events);
    // the docs are files of this repository, built into the binary.
    (
        title.trim().to_string(),
        Html::trusted(anchor_headings(&html)),
    )
}

/// The id GitHub gives a heading, from its inner HTML: tags and entities
/// are not part of the text.
fn slug(inner: &str) -> String {
    let mut out = String::new();
    let mut skip: Option<char> = None;
    for ch in inner.to_lowercase().chars() {
        match (skip, ch) {
            (Some(end), _) => {
                if ch == end {
                    skip = None;
                }
            }
            (None, '<') => skip = Some('>'),
            (None, '&') => skip = Some(';'),
            (None, 'a'..='z' | '0'..='9' | '-') => out.push(ch),
            (None, ' ') => out.push('-'),
            _ => {}
        }
    }
    out
}

/// Gives each heading the id GitHub would, so `#the-rules` links still land.
fn anchor_headings(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find("<h") {
        let level = rest.as_bytes().get(i + 2).copied();
        let close = rest[i..].find('>').map(|j| i + j);
        match (level, close) {
            (Some(l @ b'1'..=b'6'), Some(c)) if c == i + 3 => {
                let tag_end = format!("</h{}>", l as char);
                let text_end = rest[c..].find(&tag_end).map_or(rest.len(), |j| c + j);
                let slug = slug(&rest[c + 1..text_end]);
                out.push_str(&rest[..i]);
                let _ = write!(out, "<h{} id=\"{slug}\">", l as char);
                rest = &rest[c + 1..];
            }
            _ => {
                out.push_str(&rest[..i + 2]);
                rest = &rest[i + 2..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_view_has_one_current_tab() {
        for v in View::ALL {
            assert_eq!(View::parse(v.href().split_once("tab=").map(|t| t.1)), v);
            assert_eq!(tabs(v).iter().filter(|t| t.current).count(), 1);
        }
        assert_eq!(View::parse(Some("nonsense")), View::Numbers);
    }

    #[test]
    fn the_rules_keep_their_anchors() {
        let (title, html) = markdown(RULES);
        assert_eq!(title, "Rules and numbers");
        assert!(html.contains("id=\"the-rules\""));
        assert!(html.contains("id=\"the-state-of-nixpkgs\""));
        assert!(html.contains("id=\"treewide-pull-requests\""));
        assert!(!html.contains("<h1"));
    }

    #[test]
    fn a_heading_id_skips_tags_and_entities() {
        assert_eq!(slug("<code>GET /api/v1/sets</code>"), "get-apiv1sets");
        assert_eq!(slug("Rules &amp; numbers"), "rules--numbers");
    }

    #[test]
    fn every_anchor_between_docs_lands_on_a_heading() {
        let mut html: std::collections::HashMap<String, String> = DOCS
            .iter()
            .map(|(id, _, src)| (id.to_string(), markdown(src).1.into_string()))
            .collect();
        html.insert("rules".into(), markdown(RULES).1.into_string());
        for (from, page) in &html {
            for href in page.split("<a href=\"").skip(1) {
                let href = href.split('"').next().unwrap().replace("&amp;", "&");
                let (target, anchor) = match href.split_once('#') {
                    Some(("", a)) => (from.clone(), a.to_string()),
                    Some(("/state?tab=rules", a)) => ("rules".into(), a.to_string()),
                    Some((p, a)) => match p.strip_prefix("/state?tab=docs&doc=") {
                        Some(doc) => (doc.to_string(), a.to_string()),
                        None => continue,
                    },
                    None => continue,
                };
                let page = html
                    .get(&target)
                    .unwrap_or_else(|| panic!("{from}: no doc {target}"));
                assert!(
                    page.contains(&format!("id=\"{anchor}\"")),
                    "{from} links to {target}#{anchor}, which has no such heading"
                );
            }
        }
    }

    #[test]
    fn every_doc_in_the_source_is_served() {
        let dir = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/docs")).unwrap();
        let mut files: Vec<String> = dir
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter_map(|f| f.strip_suffix(".md").map(str::to_string))
            .filter(|f| f != "rules")
            .collect();
        files.sort();
        let mut served: Vec<String> = DOCS.iter().map(|(id, ..)| id.to_string()).collect();
        served.sort();
        assert_eq!(files, served);
    }

    #[test]
    fn links_between_docs_stay_on_the_site() {
        assert_eq!(
            doc_link("health.md#the-risk-score"),
            "/state?tab=docs&doc=health#the-risk-score"
        );
        assert_eq!(doc_link("rules.md"), "/state?tab=rules");
        assert_eq!(doc_link("#disk"), "#disk");
        assert_eq!(doc_link("https://nixos.org"), "https://nixos.org");
        assert_eq!(
            doc_link("../gha/update.yml"),
            format!("{REPO}/gha/update.yml")
        );
        for (id, _, src) in DOCS {
            let (title, html) = markdown(src);
            assert!(!title.is_empty(), "{id}");
            for href in html.split("href=\"").skip(1) {
                assert!(
                    ["/", "#", "https://", "http://", "mailto:"]
                        .iter()
                        .any(|p| href.starts_with(p)),
                    "{id}: {}",
                    &href[..href.len().min(60)]
                );
            }
        }
    }
}
