//! The "nixpkgs" place at `/state`: the numbers, the rules of this site and
//! what the server has read. Each is a tab.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Redirect, Response};
use axum::routing::get;
use serde::Deserialize;
use std::fmt::Write;

use crate::ui::{Page, Section, Tab};
use crate::web::{Shared, octicon};

const RULES: &str = include_str!("../docs/rules.md");

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
    Status,
}

impl View {
    const ALL: [View; 3] = [View::Numbers, View::Rules, View::Status];

    fn parse(tab: Option<&str>) -> Self {
        match tab {
            Some("rules") => View::Rules,
            Some("status") => View::Status,
            _ => View::Numbers,
        }
    }

    fn href(self) -> &'static str {
        match self {
            View::Numbers => "/state",
            View::Rules => "/state?tab=rules",
            View::Status => "/state?tab=status",
        }
    }

    fn label(self) -> &'static str {
        match self {
            View::Numbers => "Numbers",
            View::Rules => "Rules",
            View::Status => "Data status",
        }
    }
}

fn tabs(current: View) -> Vec<Tab> {
    View::ALL
        .iter()
        .map(|&v| Tab::new(v.href(), v.label(), v == current))
        .collect()
}

#[derive(Deserialize)]
struct StateQuery {
    tab: Option<String>,
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
            .body(crate::web::numbers(&app)),
        View::Rules => {
            let (title, html) = markdown(RULES);
            Page::new(Section::Nixpkgs, title).body(format!(
                "{}<article class=\"markdown-body\">{html}</article>",
                crate::web::nixpkgs_guides()
            ))
        }
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

/// Splits off the `# ` title, so the page header shows it once.
fn markdown(src: &str) -> (String, String) {
    let (title, rest) = match src.strip_prefix("# ") {
        Some(s) => s.split_once('\n').unwrap_or((s, "")),
        None => ("", src),
    };
    let mut html = String::new();
    pulldown_cmark::html::push_html(
        &mut html,
        pulldown_cmark::Parser::new_ext(rest, pulldown_cmark::Options::ENABLE_TABLES),
    );
    (title.trim().to_string(), anchor_headings(&html))
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
                let slug: String = rest[c + 1..text_end]
                    .to_lowercase()
                    .chars()
                    .filter_map(|ch| match ch {
                        'a'..='z' | '0'..='9' | '-' => Some(ch),
                        ' ' => Some('-'),
                        _ => None,
                    })
                    .collect();
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
        assert!(!html.contains("<h1"));
    }
}
