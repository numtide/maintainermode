//! One place to review: the pull requests that need a review, with one
//! Show choice, and Ready to merge for committers.

use axum::Router;
use axum::extract::{Form, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::fmt::Write;

use crate::html::{Html, html};
use crate::queue::Entry;
use crate::ui::{Page, Section, Tab, empty, pager, section};
use crate::web::{CONTRIBUTING, Shared, octicon, review_bucket, review_rows};

pub(crate) const PAGE: usize = 50;

/// The values of `tab`: the choices of Show, then Ready to merge.
pub(crate) const TABS: [(&str, &str); 6] = [
    ("foryou", "For you"),
    ("unowned", "No maintainer"),
    ("firsttime", "First-time authors"),
    ("bumps", "Version bumps"),
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
    label: Option<String>,
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

const IN_THE_QUEUES: &str =
    "<a href=\"/state?tab=rules#2-whose-pull-requests-appear\">in the queues</a>";

const TREEWIDE_LINK: &str = "<a href=\"/state?tab=rules#treewide-pull-requests\">Treewide</a>";

const CHECKLIST: &str = "<details class=\"review-check\"><summary>What to check</summary><ul class=\"ml-3 mt-1\">\
<li>Is <code>meta</code> complete: description, homepage, license, maintainers, mainProgram?</li>\
<li>Are the hashes fixed-output and pinned?</li>\
<li>Are the <code>platforms</code> correct?</li>\
<li>Does a CLI have <code>versionCheckHook</code>?</li></ul>\
<p class=\"mt-1 mb-0\">Write each finding as a comment on the changed line.</p></details>";

/// One sentence above the rows of a Show choice.
fn intro(tab: &str) -> Html {
    let queues = Html::from(IN_THE_QUEUES);
    match tab {
        "foryou" => html!(
            "Pull requests {queues} that touch your machines come first, then what you maintain, then what you watch. {} pull requests come last.",
            Html::from(TREEWIDE_LINK)
        ),
        "unowned" => html!(
            "Pull requests {queues} with no maintainer, team or <code>ci/OWNERS</code> line: a first review needs no special knowledge.{}",
            Html::from(CHECKLIST)
        ),
        "firsttime" => {
            "First contributions with no review yet: a first answer decides if a new contributor stays.".into()
        }
        "bumps" => html!(
            "Titles like <code>name: old -&gt; new</code>, most from r-ryantm. <a href=\"{}\">How to review an update</a>",
            crate::bumps::GUIDE
        ),
        _ => html!("Every pull request {queues} that is not ready to merge."),
    }
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
    let (sub, body) = if tab == "ready" {
        (
            html!(
                "Pull requests that wait for a committer. \
                 <a href=\"{CONTRIBUTING}#how-to-merge-pull-requests-yourself\">How nixpkgs merges</a>"
            ),
            q.section
                .as_deref()
                .and_then(|id| crate::committer::board_page(&app, &s, v, id, q.after))
                .unwrap_or_else(|| crate::committer::ready_tab(&app, &s, v, q.after)),
        )
    } else {
        (
            html!(
                "Pull requests that wait for a review. \
                 <a href=\"{CONTRIBUTING}#how-to-review-pull-requests\">How nixpkgs reviews</a>"
            ),
            needs_review(&app, &s, v, tab, &q),
        )
    };
    let tabs = if tab == "ready" || v.is_some_and(|l| s.committers.is(l)) {
        vec![
            Tab::new("/review", "Needs review", tab != "ready"),
            Tab::new("/review?tab=ready", "Ready to merge", tab == "ready"),
        ]
    } else {
        Vec::new()
    };
    Page::new(Section::Review, "Review")
        .icon(octicon!("eye"))
        .sub(sub)
        .subnav(tabs)
        .body(body)
        .render(&app, v)
}

/// The list of one Show choice, under the bar.
fn needs_review(
    app: &crate::web::App,
    s: &crate::web::Snapshot,
    v: Option<&str>,
    tab: &str,
    q: &ReviewQuery,
) -> Html {
    let opts = Opts {
        sort: q.sort.clone(),
        checks: q.checks.clone(),
        reviews: q.reviews.clone(),
        conflicts: q.conflicts.clone(),
        base: q.base.clone(),
        label: q.label.clone(),
        q: q.q.clone(),
    };
    let bumps = tab == "bumps";
    let only_mine = bumps && v.is_some() && q.filter.as_deref() == Some("mine");
    let show_hidden = !bumps && v.is_some() && q.hidden.as_deref() == Some("1");
    let here = format!(
        "/review?tab={tab}{}{}{}",
        if only_mine { "&filter=mine" } else { "" },
        if show_hidden { "&hidden=1" } else { "" },
        opts.query()
    );
    let filtered = opts.active() || only_mine;
    let none = if filtered {
        "Nothing matches these filters."
    } else {
        "Nothing here."
    };
    let label = TABS.iter().find(|t| t.0 == tab).map_or("All", |t| t.1);
    let hidden = v
        .map(|l| app.store.hidden_prs(l).unwrap_or_default())
        .unwrap_or_default();
    let choices = choices(app, s, v, tab, &opts, &hidden);
    if bumps {
        let index = app.index.read().unwrap();
        let h = app.health.read().unwrap();
        let mut rows = crate::bumps::open(s, &index, &h);
        if let (true, Some(l)) = (only_mine, v) {
            rows = crate::bumps::mine(app, &index, l, true, rows);
        }
        let bases = bases(&rows);
        let labels = labels(&rows);
        let rows = opts.apply(rows);
        let (page, next) = if opts.sorted() {
            paged_by_offset(&rows, q.after)
        } else {
            paged(&rows, q.after, |r| r.pr.number)
        };
        let body = if page.is_empty() {
            empty(if filtered {
                none
            } else {
                "No open version bump."
            })
        } else {
            crate::bumps::rows(page)
        };
        return html!(
            "{}{}{}",
            bar(
                &opts,
                tab,
                v.is_some(),
                false,
                only_mine,
                &bases,
                &labels,
                choices
            ),
            section(
                label,
                Some(rows.len()),
                None,
                html!(
                    "<div class=\"Box-row f6 color-fg-muted\">{}</div>{body}",
                    intro(tab)
                )
            ),
            pager(&here, next.as_deref())
        );
    }
    let src = source(app, s, v, true, tab);
    let list = ReviewList::new(&src, &hidden, show_hidden, &opts);
    let entries = list.entries();
    let (page, next) = if tab == "foryou" || opts.sorted() {
        paged_by_offset(&entries, q.after)
    } else {
        paged(&entries, q.after, |e| e.number)
    };
    let why_of: std::collections::HashMap<u64, &[String]> =
        list.rows.iter().map(|r| (r.entry.number, r.why)).collect();
    let rows = if !page.is_empty() {
        review_rows(page, tab == "firsttime", |e| {
            let why = why_of
                .get(&e.number)
                .map(|w| why_line(w))
                .unwrap_or_default();
            let end = if v.is_some() {
                hide_button(e.number, show_hidden, &here, true)
            } else {
                Html::new()
            };
            (why, end)
        })
    } else if tab == "foryou" && !filtered && !show_hidden {
        empty(
            "Nothing for you now. The pull requests with <a href=\"/review?tab=unowned\">no maintainer</a> need reviews too.",
        )
    } else {
        empty(none)
    };
    let more_hidden;
    let more = if show_hidden {
        Some((format!("/review?tab={tab}"), "back to the list"))
    } else if list.hidden > 0 {
        more_hidden = format!("{} hidden by you", list.hidden);
        Some((format!("/review?tab={tab}&hidden=1"), more_hidden.as_str()))
    } else {
        None
    };
    let (title, lead): (&str, Html) = if show_hidden {
        (
            "Hidden by you",
            "Pull requests that you hid. Push <strong>Show again</strong> to put one back in your lists.".into(),
        )
    } else {
        (label, intro(tab))
    };
    html!(
        "{}{}{}",
        bar(
            &opts,
            tab,
            v.is_some(),
            show_hidden,
            false,
            &list.bases,
            &list.labels,
            choices
        ),
        section(
            title,
            Some(entries.len()),
            more.as_ref().map(|(h, l)| (h.as_str(), *l)),
            html!("<div class=\"Box-row f6 color-fg-muted\">{lead}</div>{rows}")
        ),
        pager(&here, next.as_deref())
    )
}

pub(crate) const SORTS: [&str; 3] = ["oldest", "newest", "updated"];
pub(crate) const CHECKS: [&str; 3] = ["pass", "fail", "pending"];
pub(crate) const REVIEWS: [&str; 2] = ["none", "some"];
pub(crate) const CONFLICTS: [&str; 1] = ["hide"];

/// What the filters and the sorts read of a row.
pub(crate) struct Facts<'a> {
    number: u64,
    title: &'a str,
    ci: Option<&'a str>,
    reviewed: bool,
    conflict: bool,
    base: &'a str,
    labels: &'a [String],
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// A row that the filter bar can narrow and order.
pub(crate) trait Filtered {
    fn facts(&self) -> Facts<'_>;
}

impl Filtered for Entry {
    fn facts(&self) -> Facts<'_> {
        Facts {
            number: self.number,
            title: &self.title,
            ci: self.ci.as_deref(),
            reviewed: !self.reviewers.is_empty(),
            conflict: self.mergeable == "CONFLICTING",
            base: &self.base,
            labels: &self.labels,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

impl Filtered for crate::bumps::Row<'_> {
    fn facts(&self) -> Facts<'_> {
        let p = self.pr;
        Facts {
            number: p.number,
            title: &p.title,
            ci: p.ci.as_deref(),
            reviewed: p
                .reviews
                .iter()
                .any(|r| r.author != p.author && !r.author_is_bot),
            conflict: p.mergeable == "CONFLICTING",
            base: &p.base,
            labels: &p.labels,
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

impl<T: Filtered + ?Sized> Filtered for &T {
    fn facts(&self) -> Facts<'_> {
        (**self).facts()
    }
}

/// How a person narrows and orders a review list. Each field is `None` for
/// "any", and an unknown value counts as `None`: the API checks the values
/// before.
#[derive(Debug, Default, Clone)]
pub(crate) struct Opts {
    pub sort: Option<String>,
    pub checks: Option<String>,
    pub reviews: Option<String>,
    pub conflicts: Option<String>,
    pub base: Option<String>,
    pub label: Option<String>,
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

    fn keeps(&self, f: &Facts) -> bool {
        let checks = match Self::get(&self.checks, &CHECKS) {
            None => true,
            Some("pass") => f.ci == Some("SUCCESS"),
            Some("fail") => matches!(f.ci, Some("FAILURE" | "ERROR")),
            Some(_) => !matches!(f.ci, Some("SUCCESS" | "FAILURE" | "ERROR")),
        };
        let reviews = match Self::get(&self.reviews, &REVIEWS) {
            None => true,
            Some("none") => !f.reviewed,
            Some(_) => f.reviewed,
        };
        let conflicts = Self::get(&self.conflicts, &CONFLICTS).is_none() || !f.conflict;
        let base = Self::get(&self.base, &[]).is_none_or(|b| f.base == b);
        let label = Self::get(&self.label, &[])
            .is_none_or(|l| f.labels.iter().any(|x| x.eq_ignore_ascii_case(l)));
        let words = self
            .words()
            .is_none_or(|w| crate::search::at_word(&f.title.to_lowercase(), &w));
        checks && reviews && conflicts && base && label && words
    }

    /// With no sort asked, the list keeps the order of its Show choice.
    pub(crate) fn apply<T: Filtered>(&self, list: Vec<T>) -> Vec<T> {
        let mut out: Vec<T> = list
            .into_iter()
            .filter(|e| self.keeps(&e.facts()))
            .collect();
        let at = |e: &T| {
            let f = e.facts();
            (f.created_at, f.updated_at, f.number)
        };
        match self.sort() {
            Some("oldest") => out.sort_by_key(|e| {
                let (c, _, n) = at(e);
                (c, n)
            }),
            Some("newest") => out.sort_by_key(|e| {
                let (c, _, n) = at(e);
                std::cmp::Reverse((c, n))
            }),
            Some("updated") => out.sort_by_key(|e| {
                let (_, u, n) = at(e);
                std::cmp::Reverse((u, n))
            }),
            _ => {}
        }
        out
    }

    pub(crate) fn sorted(&self) -> bool {
        self.sort().is_some()
    }

    /// A filter of "More filters" is set.
    fn more(&self) -> bool {
        self.sorted()
            || Self::get(&self.checks, &CHECKS).is_some()
            || Self::get(&self.reviews, &REVIEWS).is_some()
            || Self::get(&self.conflicts, &CONFLICTS).is_some()
            || Self::get(&self.base, &[]).is_some()
            || Self::get(&self.label, &[]).is_some()
    }

    pub(crate) fn active(&self) -> bool {
        self.more() || self.words().is_some()
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
            ("label", Self::get(&self.label, &[])),
            ("q", Self::get(&self.q, &[])),
        ] {
            if let Some(v) = v {
                let _ = write!(out, "&{name}={}", crate::github::urlencode(v));
            }
        }
        out
    }
}

/// The Show choices as chips, each with the size of its list once the
/// pull requests you hid are out. Changing the choice keeps the filters.
fn choices(
    app: &crate::web::App,
    s: &crate::web::Snapshot,
    v: Option<&str>,
    tab: &str,
    o: &Opts,
    hidden: &std::collections::HashSet<u64>,
) -> Vec<Tab> {
    TABS.iter()
        .filter(|(id, _)| *id != "ready" && (v.is_some() || *id != "foryou"))
        .map(|&(id, label)| {
            let n = if id == "bumps" {
                let index = app.index.read().unwrap();
                let h = app.health.read().unwrap();
                crate::bumps::open(s, &index, &h).len()
            } else {
                source(app, s, v, true, id)
                    .iter()
                    .filter(|(e, _)| !hidden.contains(&e.number))
                    .count()
            };
            Tab::new(format!("/review?tab={id}{}", o.query()), label, id == tab).count(n)
        })
        .collect()
}

/// The bar above a list: the Show chips and the title search, then the
/// other filters, folded until one is set. `bases` and `labels` are those
/// of the list, so each choice has rows. `only_mine` is the `filter=mine` of
/// Version bumps.
#[allow(clippy::too_many_arguments)]
fn bar(
    o: &Opts,
    tab: &str,
    signed_in: bool,
    hidden: bool,
    only_mine: bool,
    bases: &[String],
    labels: &[(String, usize)],
    choices: Vec<Tab>,
) -> Html {
    let select = |name: &str, label: &str, now: Option<&str>, choices: &[(&str, &str)]| {
        let mut s = html!(
            "<select class=\"form-select select-sm\" name=\"{name}\" aria-label=\"{label}\">"
        );
        for (value, text) in choices {
            let sel = if Some(*value) == now || (value.is_empty() && now.is_none()) {
                " selected"
            } else {
                ""
            };
            s.push(&html!("<option value=\"{value}\"{sel}>{text}</option>"));
        }
        s.push(&Html::from("</select>"));
        s
    };
    let mut base_choices: Vec<(&str, &str)> = vec![("", "Any base branch")];
    base_choices.extend(bases.iter().map(|b| (b.as_str(), b.as_str())));
    let whose = if tab == "bumps" && signed_in {
        select(
            "filter",
            "Whose",
            only_mine.then_some("mine"),
            &[("", "Any package"), ("mine", "What you maintain or watch")],
        )
    } else {
        Html::new()
    };
    let options: Html = labels
        .iter()
        .map(|(l, n)| html!("<option value=\"{l}\" label=\"{l} ({n})\"></option>"))
        .collect();
    let label = html!(
        "<input class=\"form-control input-sm\" type=\"search\" name=\"label\" list=\"review-labels\" value=\"{}\" placeholder=\"Label\" aria-label=\"Label\">\
         <datalist id=\"review-labels\">{options}</datalist>",
        Opts::get(&o.label, &[]).unwrap_or("")
    );
    let more = html!(
        "{}{}{}{}{}{label}{whose}",
        select(
            "sort",
            "Sort",
            o.sort(),
            &[
                ("", "Default order"),
                ("oldest", "Oldest first"),
                ("newest", "Newest first"),
                ("updated", "Recently updated")
            ]
        ),
        select(
            "checks",
            "Checks",
            Opts::get(&o.checks, &CHECKS),
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
            Opts::get(&o.reviews, &REVIEWS),
            &[
                ("", "Any reviews"),
                ("none", "No review yet"),
                ("some", "Reviewed")
            ]
        ),
        select(
            "conflicts",
            "Conflicts",
            Opts::get(&o.conflicts, &CONFLICTS),
            &[("", "Any conflicts"), ("hide", "No conflicts")]
        ),
        select(
            "base",
            "Base branch",
            Opts::get(&o.base, &[]),
            &base_choices
        ),
    );
    let mut keep = vec![("tab", tab.to_string())];
    if hidden {
        keep.push(("hidden", "1".to_string()));
    }
    crate::ui::FilterBar {
        choices,
        action: "/review",
        keep,
        placeholder: "Words in the title",
        q: o.q.clone().unwrap_or_default(),
        more,
        more_open: o.more() || only_mine,
        clear: (o.active() || only_mine)
            .then(|| format!("/review?tab={tab}{}", if hidden { "&hidden=1" } else { "" })),
        ..Default::default()
    }
    .render()
}

/// The base branches of `list`, the most common first.
fn bases<T: Filtered>(list: &[T]) -> Vec<String> {
    let mut n: std::collections::HashMap<&str, usize> = Default::default();
    for e in list {
        *n.entry(e.facts().base).or_default() += 1;
    }
    let mut out: Vec<(&str, usize)> = n.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    out.into_iter().map(|(b, _)| b.to_string()).collect()
}

/// The labels of `list` with their counts, the most common first.
fn labels<T: Filtered>(list: &[T]) -> Vec<(String, usize)> {
    let mut n: std::collections::HashMap<&str, usize> = Default::default();
    for e in list {
        for l in e.facts().labels {
            *n.entry(l).or_default() += 1;
        }
    }
    let mut out: Vec<(String, usize)> = n.into_iter().map(|(l, c)| (l.to_string(), c)).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// A safe place to go back to after a form: a path of this site.
pub(crate) fn back_to(s: Option<&str>) -> &str {
    s.filter(|b| crate::web::local_path(b)).unwrap_or("/review")
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
pub(crate) fn hide_button(number: u64, hidden: bool, back: &str, key: bool) -> Html {
    let (value, label, title) = if hidden {
        ("0", "Show again", "Put it back in your review lists")
    } else {
        ("1", "Hide", "Hide it from your review lists")
    };
    let hotkey = if key {
        html!(" data-hotkey=\"e\" data-hotkey-label=\"{label}: {title}\"")
    } else {
        Html::new()
    };
    html!(
        "<form method=\"post\" action=\"/me/review/hide\" class=\"d-inline ml-2\">\
         <input type=\"hidden\" name=\"number\" value=\"{number}\"><input type=\"hidden\" name=\"hide\" value=\"{value}\">\
         <input type=\"hidden\" name=\"back\" value=\"{back}\">\
         <button class=\"btn btn-sm\" type=\"submit\" title=\"{title}\"{hotkey}>{label}</button></form>"
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
pub(crate) fn why_line(why: &[String]) -> Html {
    if why.is_empty() {
        return Html::new();
    }
    html!(
        "<div class=\"mt-1 color-fg-default\">{} {}</div>",
        crate::web::icon(octicon!("info")),
        why.join(" · ")
    )
}

/// The pull requests of a review queue, with why each one is there.
/// `own` is true when `login` asks for their own queue: only then do
/// their subscriptions, machines and private tags count.
pub(crate) fn source<'a>(
    app: &crate::web::App,
    s: &'a crate::web::Snapshot,
    login: Option<&str>,
    own: bool,
    queue: &str,
) -> Vec<(&'a Entry, Vec<String>)> {
    match login {
        Some(l) if own && crate::web::bucket_name(queue) == Some("foryou") => {
            crate::mine::for_you_why(app, s, l)
        }
        _ => review_bucket(s, login, None, queue)
            .into_iter()
            .map(|e| (e, Vec::new()))
            .collect(),
    }
}

#[derive(serde::Serialize)]
pub(crate) struct Row<'a> {
    #[serde(flatten)]
    pub entry: &'a Entry,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    pub why: &'a [String],
}

/// One review list, as both `/review` and `/api/v1/prs` show it.
pub(crate) struct ReviewList<'a> {
    pub rows: Vec<Row<'a>>,
    /// The rows that the hidden filter removed, before `Opts`.
    pub hidden: usize,
    /// The base branches after the hidden filter, before `Opts`.
    pub bases: Vec<String>,
    /// The labels after the hidden filter, before `Opts`.
    pub labels: Vec<(String, usize)>,
}

impl<'a> ReviewList<'a> {
    pub(crate) fn new(
        source: &'a [(&'a Entry, Vec<String>)],
        hidden: &std::collections::HashSet<u64>,
        show_hidden: bool,
        opts: &Opts,
    ) -> Self {
        let n_hidden = source
            .iter()
            .filter(|(e, _)| hidden.contains(&e.number))
            .count();
        let kept: Vec<&(&Entry, Vec<String>)> = source
            .iter()
            .filter(|(e, _)| hidden.contains(&e.number) == show_hidden)
            .collect();
        let entries: Vec<&Entry> = kept.iter().map(|(e, _)| *e).collect();
        let bases = bases(&entries);
        let labels = labels(&entries);
        let why: std::collections::HashMap<u64, &[String]> =
            kept.iter().map(|(e, w)| (e.number, w.as_slice())).collect();
        let rows = opts
            .apply(kept.iter().map(|(e, _)| *e).collect())
            .into_iter()
            .map(|e| Row {
                entry: e,
                why: why.get(&e.number).copied().unwrap_or(&[]),
            })
            .collect();
        ReviewList {
            rows,
            hidden: n_hidden,
            bases,
            labels,
        }
    }

    pub(crate) fn entries(&self) -> Vec<&'a Entry> {
        self.rows.iter().map(|r| r.entry).collect()
    }
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
    fn a_review_list_keeps_why_through_the_sort_and_counts_hidden() {
        let a = entry(1, "SUCCESS", false, "master", "a");
        let b = entry(2, "SUCCESS", false, "staging", "b");
        let c = entry(3, "SUCCESS", false, "master", "c");
        let src = vec![
            (&a, vec!["you watch: a".to_string()]),
            (&b, vec![]),
            (&c, vec!["you maintain: c".to_string()]),
        ];
        let hidden = [2].into_iter().collect();
        let opts = Opts {
            sort: Some("newest".into()),
            ..Default::default()
        };
        let list = ReviewList::new(&src, &hidden, false, &opts);
        assert_eq!(list.hidden, 1);
        assert_eq!(list.bases, ["master"]);
        let rows: Vec<(u64, &[String])> =
            list.rows.iter().map(|r| (r.entry.number, r.why)).collect();
        assert_eq!(rows[0], (3, &["you maintain: c".to_string()][..]));
        assert_eq!(rows[1].0, 1);
        let v = serde_json::to_value(&list.rows).unwrap();
        assert_eq!(v[1]["why"][0], "you watch: a");
        assert_eq!(v[0]["number"], 3);
        assert!(v[0]["treewide"].is_null());
        let shown = ReviewList::new(&src, &hidden, true, &Opts::default());
        assert_eq!(shown.entries().len(), 1);
        assert!(
            serde_json::to_value(&shown.rows).unwrap()[0]
                .get("why")
                .is_none()
        );
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
            labels: vec![],
            treewide: None,
            too_large: None,
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
    fn the_filters_narrow_and_order_the_version_bumps_too() {
        let mut prs: Vec<crate::model::Pr> = (1..=3)
            .map(|n| crate::ledger::tests::pr(n, "r-ryantm", n as i64))
            .collect();
        prs[0].title = "hello: 1 -> 2".into();
        prs[1].title = "curl: 8.1 -> 8.2".into();
        prs[1].ci = Some("FAILURE".into());
        prs[2].title = "jq: 1.7 -> 1.8".into();
        prs[2].base = "staging".into();
        let mut bot = crate::ledger::tests::review(1, "bot", 1);
        bot.author_is_bot = true;
        prs[0].reviews.push(bot);
        prs[2]
            .reviews
            .push(crate::ledger::tests::review(2, "alice", 4));
        let rows = || -> Vec<crate::bumps::Row> {
            prs.iter()
                .map(|pr| crate::bumps::Row {
                    pr,
                    bump: crate::bumps::parse(&pr.title).unwrap(),
                    attr: None,
                    upstream: None,
                    approvals: 0,
                })
                .collect()
        };
        let numbers =
            |o: Opts| -> Vec<u64> { o.apply(rows()).iter().map(|r| r.pr.number).collect() };
        assert_eq!(numbers(Opts::default()), [1, 2, 3]);
        let o = |f: fn(&mut Opts)| {
            let mut o = Opts::default();
            f(&mut o);
            o
        };
        assert_eq!(numbers(o(|o| o.checks = Some("fail".into()))), [2]);
        assert_eq!(numbers(o(|o| o.reviews = Some("none".into()))), [1, 2]);
        assert_eq!(numbers(o(|o| o.base = Some("staging".into()))), [3]);
        assert_eq!(numbers(o(|o| o.q = Some("cur".into()))), [2]);
        assert_eq!(numbers(o(|o| o.sort = Some("newest".into()))), [3, 2, 1]);
        assert_eq!(bases(&rows()), ["master", "staging"]);
    }

    #[test]
    fn the_bar_folds_the_filters_that_are_not_set() {
        let show = || {
            vec![
                Tab::new("/review?tab=unowned", "No maintainer", true).count(3),
                Tab::new("/review?tab=all", "All", false).count(9),
            ]
        };
        let none = bar(
            &Opts::default(),
            "unowned",
            false,
            false,
            false,
            &[],
            &[],
            show(),
        );
        assert!(none.contains(
            "href=\"/review?tab=unowned\" aria-current=\"page\">No maintainer <span class=\"Counter\">3</span>"
        ));
        assert!(none.contains("<input type=\"hidden\" name=\"tab\" value=\"unowned\">"));
        assert!(none.contains("<details class=\"filter-more\">"));
        assert!(!none.contains("name=\"filter\"") && !none.contains("Clear"));
        let sorted = Opts {
            sort: Some("newest".into()),
            ..Default::default()
        };
        let set = bar(&sorted, "foryou", true, false, false, &[], &[], show());
        assert!(set.contains("<details class=\"filter-more\" open>") && set.contains("Clear"));
        let words = Opts {
            q: Some("curl".into()),
            ..Default::default()
        };
        let searched = bar(&words, "all", true, false, false, &[], &[], show());
        assert!(searched.contains("<details class=\"filter-more\">") && searched.contains("Clear"));
        let mine = bar(
            &Opts::default(),
            "bumps",
            true,
            false,
            true,
            &[],
            &[],
            show(),
        );
        assert!(mine.contains("<option value=\"mine\" selected>"));
        assert!(mine.contains("<details class=\"filter-more\" open>"));
        assert!(
            !bar(
                &Opts::default(),
                "bumps",
                false,
                false,
                false,
                &[],
                &[],
                show()
            )
            .contains("name=\"filter\"")
        );
    }

    #[test]
    fn a_form_goes_back_only_to_this_site() {
        assert_eq!(back_to(Some("/review?tab=all")), "/review?tab=all");
        assert_eq!(back_to(Some("//evil.example")), "/review");
        assert_eq!(back_to(Some("https://evil.example")), "/review");
        assert_eq!(back_to(Some("/\\evil.example")), "/review");
        assert_eq!(back_to(Some("/\n")), "/review");
        assert_eq!(back_to(Some("/\t/evil.example")), "/review");
        assert_eq!(
            back_to(Some("/review?q=a%20b&sort=new")),
            "/review?q=a%20b&sort=new"
        );
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

    #[test]
    fn a_label_keeps_the_pull_requests_that_carry_it_in_any_case() {
        let mut a = entry(1, "SUCCESS", false, "master", "a");
        a.labels = vec!["6.topic: python".into()];
        let b = entry(2, "SUCCESS", false, "master", "b");
        let list: Vec<&Entry> = vec![&a, &b];
        let o = Opts {
            label: Some("6.Topic: Python".into()),
            ..Default::default()
        };
        let kept: Vec<u64> = o.apply(list.clone()).iter().map(|e| e.number).collect();
        assert_eq!(kept, [1]);
        assert!(o.query().contains("&label="));
        let counts = labels(&list);
        assert_eq!(counts, [("6.topic: python".to_string(), 1)]);
        let html = bar(&o, "all", true, false, false, &[], &counts, Vec::new()).to_string();
        assert!(html.contains("<details class=\"filter-more\" open>"));
        assert!(html.contains("<option value=\"6.topic: python\" label=\"6.topic: python (1)\">"));
        assert!(html.contains("name=\"label\" list=\"review-labels\" value=\"6.Topic: Python\""));
    }
}
