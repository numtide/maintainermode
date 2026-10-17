//! One place to review: the queues as tabs of `/review`.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Redirect, Response};
use axum::routing::get;
use serde::Deserialize;

use crate::ui::{Page, Section, Tab, empty, pager, section};
use crate::web::{Shared, entry_rows, octicon, review_bucket, watched_of};

pub(crate) const PAGE: usize = 50;
const NIXPKGS: &str = "https://github.com/NixOS/nixpkgs/blob/master";

const TABS: [(&str, &str); 5] = [
    ("foryou", "For you"),
    ("unowned", "No maintainer"),
    ("bumps", "Bumps"),
    ("all", "All"),
    ("ready", "Ready to merge"),
];

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/review", get(review_page))
        .route(
            "/merge",
            get(|| async { Redirect::permanent("/review?tab=ready") }),
        )
        .route(
            "/committer",
            get(|| async { Redirect::permanent("/review?tab=ready") }),
        )
        .route(
            "/bumps",
            get(|| async { Redirect::permanent("/review?tab=bumps") }),
        )
}

/// `after` is the number of the last pull request of the previous page. When
/// that pull request left the list, the page starts at the first larger
/// number, so `items` must be in ascending number order.
pub(crate) fn paged<T>(
    items: &[T],
    after: Option<u64>,
    number: impl Fn(&T) -> u64,
) -> (&[T], Option<String>) {
    let start = match after {
        None => 0,
        Some(a) => items
            .iter()
            .position(|x| number(x) == a)
            .map(|i| i + 1)
            .or_else(|| items.iter().position(|x| number(x) > a))
            .unwrap_or(items.len()),
    };
    let end = (start + PAGE).min(items.len());
    let page = &items[start..end];
    let next = (end < items.len())
        .then(|| page.last().map(|x| number(x).to_string()))
        .flatten();
    (page, next)
}

/// For a list in another order: `after` is the count of rows on the
/// previous pages. A row that leaves or moves shifts the next page by one
/// row at most, never by a page.
pub(crate) fn paged_by_offset<T>(items: &[T], after: Option<u64>) -> (&[T], Option<String>) {
    let start = after
        .map_or(0, |a| usize::try_from(a).unwrap_or(usize::MAX))
        .min(items.len());
    let end = (start + PAGE).min(items.len());
    let next = (end < items.len()).then(|| end.to_string());
    (&items[start..end], next)
}

#[derive(Deserialize)]
struct ReviewQuery {
    tab: Option<String>,
    after: Option<u64>,
    filter: Option<String>,
}

fn tab_id(asked: Option<&str>, signed_in: bool) -> &'static str {
    match asked {
        Some("foryou") if signed_in => "foryou",
        Some("unowned") => "unowned",
        Some("bumps") => "bumps",
        Some("all") => "all",
        Some("ready") => "ready",
        _ if signed_in => "foryou",
        _ => "unowned",
    }
}

const NO_MAINTAINER: &str = "<div class=\"callout mb-3\">No maintainer, team or <code>ci/OWNERS</code> line covers the files of these pull requests. \
A first review needs no special knowledge. Is <code>meta</code> complete (description, homepage, license, maintainers, mainProgram)? \
Are the hashes fixed-output and pinned? Are the <code>platforms</code> correct? Does a CLI have <code>versionCheckHook</code>? \
Write each finding as a comment on the changed line. A review here gets full credit at any age.</div>";

async fn review_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<ReviewQuery>,
) -> Response {
    let viewer = app.viewer(&headers);
    let v = viewer.as_deref();
    let s = app.snap();
    let tab = tab_id(q.tab.as_deref(), v.is_some());
    let watched = watched_of(&app, v);
    let act = crate::actions::Act::of(&app, v);
    let (bumps, body) = {
        let index = app.index.read().unwrap();
        let h = app.health.read().unwrap();
        let bumps = crate::bumps::open(&s, &index, &h);
        let n = bumps.len();
        let body = match tab {
            "bumps" => crate::bumps::tab(
                &app,
                &index,
                v,
                bumps,
                q.filter.as_deref() == Some("mine"),
                q.after,
            ),
            "ready" => crate::committer::ready_tab(&app, &s, v, q.after),
            _ => {
                let entries = review_bucket(&s, v, &watched, tab);
                let (page, next) = paged(&entries, q.after, |e| e.number);
                let (title, intro) = match tab {
                    "foryou" => (
                        "For you",
                        "<p>Pull requests that touch what you maintain (<code>meta.maintainers</code>, <code>meta.teams</code>), \
                         own in <code>ci/OWNERS</code>, changed this year, tagged, or <a href=\"/watch\">watch</a>. Oldest first.</p>",
                    ),
                    "unowned" => ("No maintainer", NO_MAINTAINER),
                    _ => (
                        "All",
                        "<p>Every pull request <a href=\"/rules#the-rules\">in the queues</a> that is not ready to merge. Oldest first.</p>",
                    ),
                };
                let rows = if page.is_empty() {
                    empty("Nothing here.")
                } else {
                    entry_rows(page, act)
                };
                format!(
                    "{intro}{}{}",
                    section(title, Some(entries.len()), None, &rows),
                    pager(&format!("/review?tab={tab}"), next.as_deref())
                )
            }
        };
        (n, body)
    };
    let mut tabs = Vec::new();
    for (id, label) in TABS {
        let count = match id {
            "foryou" if v.is_none() => continue,
            "bumps" => bumps,
            "ready" => s.queues.merge.len(),
            _ => review_bucket(&s, v, &watched, id).len(),
        };
        tabs.push(Tab::new(format!("/review?tab={id}"), label, id == tab).count(count));
    }
    let sub = if tab == "ready" {
        format!(
            "Pull requests that wait for a committer. \
             <a href=\"{NIXPKGS}/CONTRIBUTING.md#how-to-merge-pull-requests-yourself\">How nixpkgs merges</a>"
        )
    } else {
        format!(
            "You get credit when your review leads to a change, a reply or a merge within {} days. \
             <a href=\"{NIXPKGS}/CONTRIBUTING.md#how-to-review-pull-requests\">How nixpkgs reviews</a>",
            crate::ledger::MOVE_DAYS
        )
    };
    let widget = v
        .map(|v| crate::progress::widget(&app, v))
        .unwrap_or_default();
    Page::new(Section::Review, "Review")
        .icon(octicon!("eye"))
        .sub(sub)
        .subnav(tabs)
        .body(format!("{widget}{body}"))
        .render(&app, v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_starts_after_the_cursor() {
        let items: Vec<u64> = (1..=120).collect();
        let (p, next) = paged(&items, None, |n| *n);
        assert_eq!((p.len(), p[0], next.as_deref()), (PAGE, 1, Some("50")));
        let (p, next) = paged(&items, Some(100), |n| *n);
        assert_eq!((p.len(), p[0], next), (20, 101, None));
        let gone: Vec<u64> = items.iter().copied().filter(|n| *n != 50).collect();
        let (p, _) = paged(&gone, Some(50), |n| *n);
        assert_eq!(p[0], 51);
        let (p, next) = paged(&items, Some(500), |n| *n);
        assert!(p.is_empty() && next.is_none());
    }

    #[test]
    fn a_ready_page_never_repeats_when_a_row_leaves() {
        let mut items: Vec<u64> = vec![900];
        items.extend(100..=200);
        let (p, next) = paged_by_offset(&items, None);
        assert_eq!((p[0], next.as_deref()), (900, Some("50")));
        let cursor = next.and_then(|s| s.parse().ok());
        let gone: Vec<u64> = items
            .iter()
            .copied()
            .filter(|n| *n != p[PAGE - 1])
            .collect();
        let (p2, next) = paged_by_offset(&gone, cursor);
        assert_eq!((p2[0], p2.len(), next.as_deref()), (150, PAGE, Some("100")));
        let (p, next) = paged_by_offset(&items, Some(10_000));
        assert!(p.is_empty() && next.is_none());
    }

    #[test]
    fn a_visitor_never_gets_the_tab_for_you() {
        assert_eq!(tab_id(Some("foryou"), false), "unowned");
        assert_eq!(tab_id(None, true), "foryou");
        assert_eq!(tab_id(Some("ready"), false), "ready");
        assert_eq!(tab_id(Some("nonsense"), false), "unowned");
    }
}
