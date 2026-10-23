//! One place to review: the queues as tabs of `/review`.

use axum::Router;
use axum::extract::{Form, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use std::fmt::Write;

use crate::queue::Entry;
use crate::ui::{Page, Section, Tab, empty, pager, section};
use crate::web::{Shared, esc, octicon, review_bucket, review_rows, watched_of};

pub(crate) const PAGE: usize = 50;
const NIXPKGS: &str = "https://github.com/NixOS/nixpkgs/blob/master";

pub(crate) const TABS: [(&str, &str); 6] = [
    ("foryou", "For you"),
    ("unowned", "No maintainer"),
    ("firsttime", "First-timers"),
    ("bumps", "Bumps"),
    ("all", "All"),
    ("ready", "Ready to merge"),
];

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/review", get(review_page))
        .route("/me/review/hide", post(hide))
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
    section: Option<String>,
    hidden: Option<String>,
    sort: Option<String>,
    checks: Option<String>,
    reviews: Option<String>,
    conflicts: Option<String>,
    base: Option<String>,
    q: Option<String>,
}

fn tab_id(asked: Option<&str>, signed_in: bool) -> &'static str {
    match asked {
        Some("foryou") if signed_in => "foryou",
        Some("unowned") => "unowned",
        Some("firsttime") => "firsttime",
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
Write each finding as a comment on the changed line.</div>";

fn first_timers() -> String {
    format!(
        "<div class=\"callout mb-3\">First contributions that no one but the author has reviewed, the oldest first. \
         A first answer decides if a new contributor stays: start at the top. \
         <a href=\"{}#how-to-review-pull-requests\">How to review</a></div>",
        crate::web::CONTRIBUTING
    )
}

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
    let foryou = v
        .map(|l| crate::mine::for_you_why(&app, &s, l))
        .unwrap_or_default();
    let hidden = v
        .map(|l| app.store.hidden_prs(l).unwrap_or_default())
        .unwrap_or_default();
    let show_hidden = v.is_some() && q.hidden.as_deref() == Some("1");
    let opts = Opts {
        sort: q.sort.clone(),
        checks: q.checks.clone(),
        reviews: q.reviews.clone(),
        conflicts: q.conflicts.clone(),
        base: q.base.clone(),
        q: q.q.clone(),
    };
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
            "ready" => q
                .section
                .as_deref()
                .and_then(|id| crate::committer::board_page(&app, &s, v, id, q.after))
                .unwrap_or_else(|| crate::committer::ready_tab(&app, &s, v, q.after)),
            _ => {
                let mut entries: Vec<&Entry> = if tab == "foryou" {
                    foryou.iter().map(|(e, _)| *e).collect()
                } else {
                    review_bucket(&s, v, v.map(|_| watched.as_slice()), tab)
                };
                let n_hidden = entries
                    .iter()
                    .filter(|e| hidden.contains(&e.number))
                    .count();
                entries.retain(|e| hidden.contains(&e.number) == show_hidden);
                let bases = bases(&entries);
                let entries = opts.apply(entries);
                let (page, next) = if tab == "foryou" || opts.sorted() {
                    paged_by_offset(&entries, q.after)
                } else {
                    paged(&entries, q.after, |e| e.number)
                };
                let first = first_timers();
                let (title, intro) = match tab {
                    "foryou" => (
                        "For you",
                        "<p>Pull requests that touch the packages of <a href=\"/packages?filter=machines\">your machines</a> come first. Then those that touch what you maintain (<code>meta.maintainers</code>, <code>meta.teams</code>), \
                         own in <code>ci/OWNERS</code>, changed this year or tagged. Then those on what you <a href=\"/packages?filter=watch\">watch</a>. Oldest first in each group. Each row tells why it is here.</p>",
                    ),
                    "unowned" => ("No maintainer", NO_MAINTAINER),
                    "firsttime" => ("First-timers", first.as_str()),
                    _ => (
                        "All",
                        "<p>Every pull request <a href=\"/state?tab=rules#the-rules\">in the queues</a> that is not ready to merge. Oldest first.</p>",
                    ),
                };
                let title = if show_hidden { "Hidden by you" } else { title };
                let here = format!(
                    "/review?tab={tab}{}{}",
                    if show_hidden { "&hidden=1" } else { "" },
                    opts.query()
                );
                let why_of: std::collections::HashMap<u64, &Vec<String>> =
                    foryou.iter().map(|(e, w)| (e.number, w)).collect();
                let rows = if page.is_empty() {
                    empty(if opts.active() {
                        "Nothing matches these filters."
                    } else {
                        "Nothing here."
                    })
                } else {
                    review_rows(page, act, tab == "firsttime", |e| {
                        let why = if tab == "foryou" {
                            why_of
                                .get(&e.number)
                                .map(|w| why_line(w))
                                .unwrap_or_default()
                        } else {
                            String::new()
                        };
                        let end = if v.is_some() {
                            hide_button(e.number, show_hidden, &here, true)
                        } else {
                            String::new()
                        };
                        (why, end)
                    })
                };
                let more_hidden;
                let more = if show_hidden {
                    Some((format!("/review?tab={tab}"), "back to the list"))
                } else if n_hidden > 0 {
                    more_hidden = format!("{n_hidden} hidden by you");
                    Some((
                        format!("/review?tab={tab}&amp;hidden=1"),
                        more_hidden.as_str(),
                    ))
                } else {
                    None
                };
                let intro = if show_hidden {
                    "<p>Pull requests you hid. They come back when you press Show again, and leave this list when they close.</p>"
                } else {
                    intro
                };
                format!(
                    "{intro}{}{}{}",
                    opts.bar(tab, show_hidden, &bases),
                    section(
                        title,
                        Some(entries.len()),
                        more.as_ref().map(|(h, l)| (h.as_str(), *l)),
                        &rows
                    ),
                    pager(&here, next.as_deref())
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
            "foryou" => foryou
                .iter()
                .filter(|(e, _)| !hidden.contains(&e.number))
                .count(),
            _ => review_bucket(&s, v, v.map(|_| watched.as_slice()), id)
                .iter()
                .filter(|e| !hidden.contains(&e.number))
                .count(),
        };
        tabs.push(Tab::new(format!("/review?tab={id}"), label, id == tab).count(count));
    }
    let sub = if tab == "ready" {
        format!(
            "Pull requests that wait for a committer. \
             <a href=\"{NIXPKGS}/CONTRIBUTING.md#how-to-merge-pull-requests-yourself\">How nixpkgs merges</a>"
        )
    } else if matches!(tab, "firsttime" | "bumps") {
        format!(
            "Pull requests that wait for a review. \
             <a href=\"{NIXPKGS}/CONTRIBUTING.md#how-to-review-pull-requests\">How nixpkgs reviews</a>"
        )
    } else {
        format!(
            "Pull requests that wait for a review. Each author is <a href=\"/state?tab=rules#2-whose-pull-requests-appear\">in the queues</a>: \
             they reviewed the pull requests of {} or more other authors in the last {} days. \
             <a href=\"{NIXPKGS}/CONTRIBUTING.md#how-to-review-pull-requests\">How nixpkgs reviews</a>",
            crate::ledger::MIN_COUNTERPARTIES,
            crate::ledger::WINDOW_DAYS
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

pub(crate) const SORTS: [&str; 3] = ["oldest", "newest", "updated"];
pub(crate) const CHECKS: [&str; 3] = ["pass", "fail", "pending"];
pub(crate) const REVIEWS: [&str; 2] = ["none", "some"];
pub(crate) const CONFLICTS: [&str; 1] = ["hide"];

/// How a person narrows and orders a review list. Each field is `None` for
/// "any", and an unknown value counts as `None`: the API checks the values
/// before.
#[derive(Debug, Default, Clone, Deserialize)]
pub(crate) struct Opts {
    pub sort: Option<String>,
    pub checks: Option<String>,
    pub reviews: Option<String>,
    pub conflicts: Option<String>,
    pub base: Option<String>,
    pub q: Option<String>,
}

impl Opts {
    fn get<'a>(o: &'a Option<String>, allowed: &[&str]) -> Option<&'a str> {
        o.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && (allowed.is_empty() || allowed.contains(s)))
    }

    fn sort(&self) -> Option<&str> {
        Self::get(&self.sort, &SORTS)
    }

    fn words(&self) -> Option<String> {
        Self::get(&self.q, &[]).map(str::to_lowercase)
    }

    pub(crate) fn keeps(&self, e: &Entry) -> bool {
        let checks = match Self::get(&self.checks, &CHECKS) {
            None => true,
            Some("pass") => e.ci.as_deref() == Some("SUCCESS"),
            Some("fail") => matches!(e.ci.as_deref(), Some("FAILURE" | "ERROR")),
            Some(_) => !matches!(e.ci.as_deref(), Some("SUCCESS" | "FAILURE" | "ERROR")),
        };
        let reviews = match Self::get(&self.reviews, &REVIEWS) {
            None => true,
            Some("none") => e.reviewers.is_empty(),
            Some(_) => !e.reviewers.is_empty(),
        };
        let conflicts =
            Self::get(&self.conflicts, &CONFLICTS).is_none() || e.mergeable != "CONFLICTING";
        let base = Self::get(&self.base, &[]).is_none_or(|b| e.base == b);
        let words = self
            .words()
            .is_none_or(|w| crate::search::at_word(&e.title.to_lowercase(), &w));
        checks && reviews && conflicts && base && words
    }

    /// With no sort asked, the list keeps the order of its tab.
    pub(crate) fn apply<'a>(&self, list: Vec<&'a Entry>) -> Vec<&'a Entry> {
        let mut out: Vec<&Entry> = list.into_iter().filter(|e| self.keeps(e)).collect();
        match self.sort() {
            Some("oldest") => out.sort_by_key(|e| (e.created_at, e.number)),
            Some("newest") => out.sort_by_key(|e| std::cmp::Reverse((e.created_at, e.number))),
            Some("updated") => out.sort_by_key(|e| std::cmp::Reverse((e.updated_at, e.number))),
            _ => {}
        }
        out
    }

    pub(crate) fn sorted(&self) -> bool {
        self.sort().is_some()
    }

    pub(crate) fn active(&self) -> bool {
        self.sorted()
            || Self::get(&self.checks, &CHECKS).is_some()
            || Self::get(&self.reviews, &REVIEWS).is_some()
            || Self::get(&self.conflicts, &CONFLICTS).is_some()
            || Self::get(&self.base, &[]).is_some()
            || self.words().is_some()
    }

    /// `&name=value` for each field that is set, to keep in links. Not
    /// escaped for HTML.
    pub(crate) fn query(&self) -> String {
        let mut out = String::new();
        for (name, v) in [
            ("sort", self.sort()),
            ("checks", Self::get(&self.checks, &CHECKS)),
            ("reviews", Self::get(&self.reviews, &REVIEWS)),
            ("conflicts", Self::get(&self.conflicts, &CONFLICTS)),
            ("base", Self::get(&self.base, &[])),
            ("q", Self::get(&self.q, &[])),
        ] {
            if let Some(v) = v {
                let _ = write!(out, "&{name}={}", crate::github::urlencode(v));
            }
        }
        out
    }

    /// The filter bar above a list. `bases` are the base branches of the
    /// list, so each choice has rows.
    fn bar(&self, tab: &str, hidden: bool, bases: &[&str]) -> String {
        let select = |name: &str, label: &str, now: Option<&str>, choices: &[(&str, &str)]| {
            let mut s = format!(
                "<select class=\"form-select select-sm\" name=\"{name}\" aria-label=\"{label}\">"
            );
            for (value, text) in choices {
                let sel = if Some(*value) == now || (value.is_empty() && now.is_none()) {
                    " selected"
                } else {
                    ""
                };
                let _ = write!(
                    s,
                    "<option value=\"{}\"{sel}>{}</option>",
                    esc(value),
                    esc(text)
                );
            }
            s.push_str("</select>");
            s
        };
        let mut base_choices: Vec<(&str, &str)> = vec![("", "Any base branch")];
        base_choices.extend(bases.iter().map(|b| (*b, *b)));
        let clear = if self.active() {
            format!(
                " <a class=\"btn btn-sm btn-invisible\" href=\"/review?tab={tab}{}\">Clear</a>",
                if hidden { "&amp;hidden=1" } else { "" }
            )
        } else {
            String::new()
        };
        format!(
            "<form class=\"review-bar mb-3\" method=\"get\" action=\"/review\" role=\"search\" aria-label=\"Sort and filter\">\
             <input type=\"hidden\" name=\"tab\" value=\"{tab}\">{}\
             <input class=\"form-control input-sm\" type=\"search\" name=\"q\" value=\"{}\" placeholder=\"Words in the title\" aria-label=\"Words in the title\">\
             {}{}{}{}{}\
             <button class=\"btn btn-sm\" type=\"submit\">Apply</button>{clear}</form>",
            if hidden {
                "<input type=\"hidden\" name=\"hidden\" value=\"1\">"
            } else {
                ""
            },
            esc(self.q.as_deref().unwrap_or("")),
            select(
                "sort",
                "Sort",
                self.sort(),
                &[
                    ("", "Tab order"),
                    ("oldest", "Oldest first"),
                    ("newest", "Newest first"),
                    ("updated", "Recently updated")
                ]
            ),
            select(
                "checks",
                "Checks",
                Self::get(&self.checks, &CHECKS),
                &[
                    ("", "Any checks"),
                    ("pass", "Checks pass"),
                    ("fail", "Checks fail"),
                    ("pending", "Checks pending")
                ]
            ),
            select(
                "reviews",
                "Reviews",
                Self::get(&self.reviews, &REVIEWS),
                &[
                    ("", "Any reviews"),
                    ("none", "No review yet"),
                    ("some", "Reviewed")
                ]
            ),
            select(
                "conflicts",
                "Conflicts",
                Self::get(&self.conflicts, &CONFLICTS),
                &[("", "With conflicts"), ("hide", "No conflicts")]
            ),
            select(
                "base",
                "Base branch",
                Self::get(&self.base, &[]),
                &base_choices
            ),
        )
    }
}

/// The base branches of `list`, the most common first.
fn bases<'a>(list: &[&'a Entry]) -> Vec<&'a str> {
    let mut n: std::collections::HashMap<&str, usize> = Default::default();
    for e in list {
        *n.entry(e.base.as_str()).or_default() += 1;
    }
    let mut out: Vec<(&str, usize)> = n.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    out.into_iter().map(|(b, _)| b).collect()
}

/// A safe place to go back to after a form: a path of this site.
pub(crate) fn back_to(s: Option<&str>) -> &str {
    s.filter(|b| b.starts_with('/') && !b.starts_with("//") && !b.contains('\\'))
        .unwrap_or("/review")
}

#[derive(Deserialize)]
struct HideForm {
    number: u64,
    hide: Option<String>,
    back: Option<String>,
}

async fn hide(State(app): State<Shared>, headers: HeaderMap, Form(f): Form<HideForm>) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let hidden = f.hide.as_deref() != Some("0");
    if let Err(e) = app.store.hide_pr(&login, f.number, hidden) {
        tracing::warn!("hide #{}: {e:#}", f.number);
    }
    Redirect::to(back_to(f.back.as_deref())).into_response()
}

/// The Hide button of a row, or Show again for a hidden one. `key` gives
/// it the `e` hotkey; a pull request page leaves `e` to Snooze.
pub(crate) fn hide_button(number: u64, hidden: bool, back: &str, key: bool) -> String {
    let (value, label, title) = if hidden {
        ("0", "Show again", "Put it back in your review lists")
    } else {
        ("1", "Hide", "Hide it from your review lists")
    };
    let hotkey = if key {
        format!(" data-hotkey=\"e\" data-hotkey-label=\"{label}: {title}\"")
    } else {
        String::new()
    };
    format!(
        "<form method=\"post\" action=\"/me/review/hide\" class=\"d-inline ml-2\">\
         <input type=\"hidden\" name=\"number\" value=\"{number}\"><input type=\"hidden\" name=\"hide\" value=\"{value}\">\
         <input type=\"hidden\" name=\"back\" value=\"{}\">\
         <button class=\"btn btn-sm\" type=\"submit\" title=\"{title}\"{hotkey}>{label}</button></form>",
        esc(back)
    )
}

/// Forgets the hidden pull requests that closed or left the synced data.
pub(crate) fn purge(app: &crate::web::App) {
    let s = app.snap();
    if s.pr_data.is_empty() {
        return;
    }
    let open: std::collections::HashSet<u64> = s
        .pr_data
        .iter()
        .filter(|p| p.is_open())
        .map(|p| p.number)
        .collect();
    if let Err(e) = app.store.purge_hidden_prs(&open) {
        tracing::warn!("hidden pull requests: {e:#}");
    }
}

/// Why a row of For you is there: "you maintain: hello · you watch: nginx".
pub(crate) fn why_line(why: &[String]) -> String {
    if why.is_empty() {
        return String::new();
    }
    format!(
        "<div class=\"mt-1 color-fg-default\">{} {}</div>",
        crate::web::icon(octicon!("info")),
        esc(&why.join(" · "))
    )
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

    fn entry(number: u64, ci: &str, reviewed: bool, base: &str, title: &str) -> Entry {
        let t = chrono::DateTime::from_timestamp(number as i64 * 1000, 0).unwrap();
        Entry {
            number,
            title: title.into(),
            url: String::new(),
            author: "a".into(),
            author_score: 0.0,
            base: base.into(),
            created_at: t,
            updated_at: t + chrono::Duration::seconds(10_000 - number as i64 * 2000),
            ci: Some(ci.into()),
            mergeable: if number == 3 {
                "CONFLICTING"
            } else {
                "MERGEABLE"
            }
            .into(),
            reviewers: if reviewed { vec!["r".into()] } else { vec![] },
            approved_by: vec![],
            credited_by: vec![],
            files: vec![],
            unowned: false,
        }
    }

    #[test]
    fn the_filters_narrow_and_the_sort_orders() {
        let all = [
            entry(1, "SUCCESS", false, "master", "hello: 1 -> 2"),
            entry(2, "FAILURE", true, "staging", "curl: fix shell completions"),
            entry(3, "PENDING", false, "master", "jq: 1.7 -> 1.8"),
        ];
        let list: Vec<&Entry> = all.iter().collect();
        let numbers =
            |o: Opts| -> Vec<u64> { o.apply(list.clone()).iter().map(|e| e.number).collect() };
        let o = |f: fn(&mut Opts)| {
            let mut o = Opts::default();
            f(&mut o);
            o
        };
        assert_eq!(numbers(Opts::default()), [1, 2, 3]);
        assert_eq!(numbers(o(|o| o.checks = Some("fail".into()))), [2]);
        assert_eq!(numbers(o(|o| o.checks = Some("pending".into()))), [3]);
        assert_eq!(numbers(o(|o| o.reviews = Some("none".into()))), [1, 3]);
        assert_eq!(numbers(o(|o| o.conflicts = Some("hide".into()))), [1, 2]);
        assert_eq!(numbers(o(|o| o.base = Some("staging".into()))), [2]);
        assert_eq!(numbers(o(|o| o.q = Some("hel".into()))), [1]);
        assert_eq!(numbers(o(|o| o.sort = Some("newest".into()))), [3, 2, 1]);
        assert_eq!(numbers(o(|o| o.sort = Some("updated".into()))), [1, 2, 3]);
        assert_eq!(
            numbers(o(|o| o.checks = Some("nonsense".into()))),
            [1, 2, 3]
        );
        let o = o(|o| {
            o.sort = Some("newest".into());
            o.q = Some("a b".into());
        });
        assert_eq!(o.query(), "&sort=newest&q=a%20b");
        assert!(o.active() && o.sorted());
        assert_eq!(bases(&list), ["master", "staging"]);
    }

    #[test]
    fn a_form_goes_back_only_to_this_site() {
        assert_eq!(back_to(Some("/review?tab=all")), "/review?tab=all");
        assert_eq!(back_to(Some("//evil.example")), "/review");
        assert_eq!(back_to(Some("https://evil.example")), "/review");
        assert_eq!(back_to(Some("/\\evil.example")), "/review");
        assert_eq!(back_to(None), "/review");
    }

    #[test]
    fn a_visitor_never_gets_the_tab_for_you() {
        assert_eq!(tab_id(Some("foryou"), false), "unowned");
        assert_eq!(tab_id(None, true), "foryou");
        assert_eq!(tab_id(Some("ready"), false), "ready");
        assert_eq!(tab_id(Some("firsttime"), false), "firsttime");
        assert_eq!(tab_id(Some("nonsense"), false), "unowned");
    }
}
