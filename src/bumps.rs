//! Version bumps: open pull requests titled `<name>: <old> -> <new>`, the
//! form of the nixpkgs commit conventions and of r-ryantm.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fmt::Write;

use crate::actions::Act;
use crate::area::{Index, under};
use crate::catalog::{Subject, pr_row, subscriptions, upstream};
use crate::health::Health;
use crate::links::pkg_key;
use crate::model::Pr;
use crate::ui::{Kind, Tone, badge, empty, item_row, pager, section};
use crate::web::{App, Snapshot, ago, esc};

const GUIDE: &str = "https://github.com/NixOS/nixpkgs/blob/master/pkgs/README.md#package-updates";

#[derive(Debug, PartialEq)]
pub struct Bump<'a> {
    pub name: &'a str,
    pub old: &'a str,
    pub new: &'a str,
}

/// `hello: 2.12 -> 2.13`, with `→` for the arrow, words after the new
/// version, and a `[Backport release-26.05]` prefix all allowed.
pub fn parse(title: &str) -> Option<Bump<'_>> {
    let mut t = title.trim();
    if t.starts_with('[') {
        t = t.split_once("] ")?.1.trim_start();
    }
    let (name, rest) = t.split_once(": ")?;
    if name.is_empty() || name.contains(char::is_whitespace) {
        return None;
    }
    let (old, new) = rest.split_once(" -> ").or_else(|| rest.split_once(" → "))?;
    let old = old.trim();
    let new = new
        .split_whitespace()
        .next()?
        .trim_end_matches([',', ';', ':']);
    if old.is_empty() || old.contains(char::is_whitespace) || new.is_empty() {
        return None;
    }
    Some(Bump { name, old, new })
}

/// People other than the author who approved, on any head.
fn approvals(pr: &Pr) -> usize {
    pr.reviews
        .iter()
        .filter(|r| r.state == "APPROVED" && !r.author_is_bot && r.author != pr.author)
        .map(|r| r.author.as_str())
        .collect::<HashSet<_>>()
        .len()
}

fn leaf(attr: &str) -> &str {
    attr.rsplit('.').next().unwrap_or(attr)
}

/// The attribute a bump is for: the name itself, or, for
/// `python312Packages.foo`, the package of that leaf when the pull request
/// already links to it.
fn package_of(s: &Snapshot, index: &Index, pr: &Pr, name: &str) -> Option<String> {
    if let Some(a) = index.resolve(name) {
        return Some(a.to_string());
    }
    let a = index.resolve(leaf(name))?;
    s.links
        .prs
        .get(&pkg_key(a))
        .is_some_and(|v| v.contains(&pr.number))
        .then(|| a.to_string())
}

pub(crate) struct Row<'a> {
    pub(crate) pr: &'a Pr,
    pub(crate) bump: Bump<'a>,
    pub(crate) attr: Option<String>,
    pub(crate) upstream: Option<String>,
    pub(crate) approvals: usize,
}

/// Every open bump, the oldest first.
pub(crate) fn open<'a>(s: &'a Snapshot, index: &Index, h: &Health) -> Vec<Row<'a>> {
    let mut rows: Vec<Row> = s
        .pr_data
        .iter()
        .filter(|p| p.is_open())
        .filter_map(|pr| {
            let bump = parse(&pr.title)?;
            let attr = package_of(s, index, pr, bump.name);
            let upstream = attr
                .as_deref()
                .and_then(|a| upstream(s, index, h, a))
                .map(String::from);
            Some(Row {
                pr,
                bump,
                attr,
                upstream,
                approvals: approvals(pr),
            })
        })
        .collect();
    rows.sort_by_key(|r| (r.pr.created_at, r.pr.number));
    rows
}

pub(crate) fn median_age_days(rows: &[Row], now: DateTime<Utc>) -> Option<f64> {
    let mut days: Vec<i64> = rows
        .iter()
        .map(|r| (now - r.pr.created_at).num_days())
        .collect();
    if days.is_empty() {
        return None;
    }
    days.sort_unstable();
    let n = days.len();
    Some(if n % 2 == 1 {
        days[n / 2] as f64
    } else {
        (days[n / 2 - 1] + days[n / 2]) as f64 / 2.0
    })
}

/// Only what the viewer maintains or watches. What they maintain comes from
/// `index.maintainers`, built once per index, not from a walk over every
/// package. A watched package counts by its file, so an alias and its target
/// count as one. Subscriptions are private: pass `watched` only for the
/// viewer.
pub(crate) fn mine<'a>(
    app: &App,
    index: &Index,
    login: &str,
    watched: bool,
    rows: Vec<Row<'a>>,
) -> Vec<Row<'a>> {
    let mut files: HashSet<String> = HashSet::new();
    let mut areas: Vec<String> = index
        .maintainers
        .get(&login.to_lowercase())
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default();
    let file_of = |attr: &str| index.pkgs.get(attr).and_then(|p| p.file.clone());
    let subs = if watched {
        subscriptions(app, login)
    } else {
        Vec::new()
    };
    for sub in subs {
        match sub {
            Subject::Pkg(a) => files.extend(file_of(&a)),
            Subject::Area(p) => areas.push(p),
            Subject::Module(_) => {}
        }
    }
    rows.into_iter()
        .filter(|r| {
            let file = r.attr.as_deref().and_then(file_of);
            file.is_some_and(|f| files.contains(&f) || areas.iter().any(|a| under(&f, a)))
                || r.pr.files.iter().any(|f| areas.iter().any(|a| under(f, a)))
        })
        .collect()
}

fn checks(pr: &Pr) -> &'static str {
    match pr.ci.as_deref() {
        Some("SUCCESS") => "<span class=\"color-fg-success\">checks pass</span>",
        Some("FAILURE" | "ERROR") => "<span class=\"color-fg-danger\">checks fail</span>",
        _ => "<span class=\"color-fg-muted\">checks pending</span>",
    }
}

fn upstream_html(r: &Row) -> String {
    let Some(u) = &r.upstream else {
        return String::new();
    };
    let (class, hint) = if u == r.bump.new {
        ("Label--success", "this pull request brings it")
    } else if crate::version::compare(u, r.bump.new) == std::cmp::Ordering::Greater {
        ("Label--attention", "newer than this pull request")
    } else {
        ("", "the newest release Maintainer Mode knows")
    };
    format!(
        "<span class=\"Label {class}\" title=\"{hint}\">upstream {}</span>",
        esc(u)
    )
}

fn row_html(r: &Row, act: Act) -> String {
    let p = r.pr;
    let package = r
        .attr
        .as_deref()
        .map(|a| format!(" · <a href=\"/p/{0}\">{0}</a>", esc(a)))
        .unwrap_or_default();
    let base = if p.base == "master" || p.base == "staging" {
        String::new()
    } else {
        format!(" · <span class=\"branch-name\">{}</span>", esc(&p.base))
    };
    let approved = match r.approvals {
        0 => "no approval".to_string(),
        1 => "1 approval".to_string(),
        n => format!("{n} approvals"),
    };
    let mut badges = Vec::new();
    if p.is_draft {
        badges.push(badge(Tone::Neutral, "draft"));
    }
    badges.push(upstream_html(r));
    item_row(
        Some(Kind::PR_OPEN),
        &crate::prpage::title_link(p.number, &esc(&p.title), "Link--primary text-bold"),
        "",
        &format!(
            "#{n} by <a href=\"/u/{a}\">{a}</a> · {age} old{package}{base} · {checks} · {approved}",
            n = p.number,
            a = esc(&p.author),
            age = ago(p.created_at),
            checks = checks(p),
        ),
        &badges,
        &act.buttons(p.number),
    )
}

/// The Bumps tab of `/review`. `rows` are all open bumps; `only_mine` keeps
/// what the viewer maintains or watches.
pub(crate) fn tab(
    app: &App,
    index: &Index,
    viewer: Option<&str>,
    mut rows: Vec<Row>,
    only_mine: bool,
    after: Option<u64>,
) -> String {
    let only_mine = only_mine && viewer.is_some();
    if let (true, Some(v)) = (only_mine, viewer) {
        rows = mine(app, index, v, true, rows);
    }
    let mut out = format!(
        "<p>Open pull requests with a title like <code>name: old -&gt; new</code>. Most come from r-ryantm. Oldest first. \
         Before you approve one, read <a href=\"{GUIDE}\">how nixpkgs reviews a package update</a>.</p>"
    );
    let href = if only_mine {
        "/review?tab=bumps&filter=mine"
    } else {
        "/review?tab=bumps"
    };
    if viewer.is_some() {
        let link = |on: bool, href: &str, label: &str| {
            if on {
                format!("<strong>{label}</strong>")
            } else {
                format!("<a href=\"{}\">{label}</a>", esc(href))
            }
        };
        let _ = write!(
            out,
            "<p class=\"f6\">Show: {} · {}</p>",
            link(!only_mine, "/review?tab=bumps", "all"),
            link(
                only_mine,
                "/review?tab=bumps&filter=mine",
                "what you maintain or watch"
            )
        );
    }
    let _ = write!(
        out,
        "<p class=\"f6 color-fg-muted\">{} open{}. <a href=\"/api/v1/prs?queue=bumps\">JSON</a></p>",
        rows.len(),
        median_age_days(&rows, Utc::now())
            .map(|d| format!(", median age {d} days"))
            .unwrap_or_default()
    );
    if rows.is_empty() {
        out.push_str("<div class=\"blankslate\"><p>No open version bump.</p></div>");
        return out;
    }
    let (page, next) = crate::review::paged(&rows, after, |r| r.pr.number);
    let act = Act::of(app, viewer);
    out.push_str("<div class=\"Box\">");
    for r in page {
        out.push_str(&row_html(r, act));
    }
    out.push_str("</div>");
    out.push_str(&pager(href, next.as_deref()));
    out
}

pub(crate) fn row_json(r: &Row) -> Value {
    let p = r.pr;
    json!({
        "number": p.number, "title": p.title, "url": p.url, "author": p.author,
        "base": p.base, "draft": p.is_draft, "ci": p.ci, "created_at": p.created_at,
        "name": r.bump.name, "old": r.bump.old, "new": r.bump.new,
        "package": r.attr, "upstream": r.upstream, "approvals": r.approvals,
    })
}

/// The open bumps of one package, from the pull requests attached to it.
pub(crate) fn of_package<'a>(attr: &str, pname: &str, prs: &[&'a Pr]) -> Vec<&'a Pr> {
    prs.iter()
        .copied()
        .filter(|p| p.is_open())
        .filter(|p| {
            parse(&p.title)
                .is_some_and(|b| b.name == attr || b.name == pname || leaf(b.name) == leaf(attr))
        })
        .collect()
}

pub(crate) fn package_section(
    s: &Snapshot,
    index: &Index,
    h: &Health,
    attr: &str,
    prs: &[&Pr],
    act: Act,
) -> String {
    let pname = index.pkgs.get(attr).map_or("", |p| p.pname.as_str());
    let list = of_package(attr, pname, prs);
    let mut body: String = list.iter().map(|p| pr_row(p, act)).collect();
    if list.is_empty() {
        let hint = match upstream(s, index, h, attr) {
            Some(u) => format!(
                " Upstream has {}: <a href=\"{GUIDE}\">how to update a package</a>.",
                esc(u)
            ),
            None => String::new(),
        };
        body = empty(&format!("None open.{hint}"));
    }
    section(
        "Version bumps",
        Some(list.len()),
        Some(("/review?tab=bumps", "all open bumps")),
        &body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_of_version_bumps() {
        let b = |old: &'static str, new: &'static str, name: &'static str| {
            Some(Bump { name, old, new })
        };
        assert_eq!(parse("hello: 2.12 -> 2.13"), b("2.12", "2.13", "hello"));
        assert_eq!(parse("hello: 2.12 → 2.13"), b("2.12", "2.13", "hello"));
        assert_eq!(
            parse("python312Packages.foo: 1.0 -> 1.1, fix tests"),
            b("1.0", "1.1", "python312Packages.foo")
        );
        assert_eq!(
            parse("[Backport release-26.05] curl: 8.20.0 -> 8.21.0"),
            b("8.20.0", "8.21.0", "curl")
        );
        assert_eq!(parse("hello: fix the build"), None);
        assert_eq!(parse("hello: from 1 -> 2"), None);
        assert_eq!(parse("treewide: a -> b"), b("a", "b", "treewide"));
        assert_eq!(parse("some words: 1 -> 2"), None);
    }

    #[test]
    fn a_bump_belongs_to_its_package_and_to_another_python_set() {
        let mut a = crate::ledger::tests::pr(1, "r-ryantm", 0);
        a.title = "hello: 1 -> 2".into();
        let mut b = crate::ledger::tests::pr(2, "alice", 0);
        b.title = "python312Packages.hello: 1 -> 2".into();
        let mut c = crate::ledger::tests::pr(3, "bob", 0);
        c.title = "hello: fix the build".into();
        let prs = [&a, &b, &c];
        let numbers: Vec<u64> = of_package("python3Packages.hello", "hello", &prs)
            .iter()
            .map(|p| p.number)
            .collect();
        assert_eq!(numbers, [1, 2]);
    }

    #[test]
    fn the_median_age_of_bumps() {
        let now = crate::ledger::tests::t(10);
        let prs: Vec<Pr> = [0, 2, 6]
            .into_iter()
            .map(|d| crate::ledger::tests::pr(d as u64, "a", d))
            .collect();
        let rows: Vec<Row> = prs
            .iter()
            .map(|pr| Row {
                pr,
                bump: Bump {
                    name: "x",
                    old: "1",
                    new: "2",
                },
                attr: None,
                upstream: None,
                approvals: approvals(pr),
            })
            .collect();
        assert_eq!(median_age_days(&rows, now), Some(8.0));
        assert_eq!(median_age_days(&[], now), None);
    }

    #[test]
    fn mine_keeps_the_bumps_of_what_the_viewer_maintains() {
        let app = crate::sets::tests::app(&["hello", "curl"]);
        let mut index = app.index.write().unwrap();
        index.pkgs.get_mut("hello").unwrap().file =
            Some("pkgs/by-name/he/hello/package.nix".into());
        index.pkgs.get_mut("curl").unwrap().file = Some("pkgs/by-name/cu/curl/package.nix".into());
        index
            .maintainers
            .entry("alice".into())
            .or_default()
            .insert("pkgs/by-name/he/hello".into());
        let prs: Vec<Pr> = (1..=2)
            .map(|n| crate::ledger::tests::pr(n, "r-ryantm", 0))
            .collect();
        let rows: Vec<Row> = prs
            .iter()
            .zip(["hello", "curl"])
            .map(|(pr, attr)| Row {
                pr,
                bump: Bump {
                    name: attr,
                    old: "1",
                    new: "2",
                },
                attr: Some(attr.into()),
                upstream: None,
                approvals: 0,
            })
            .collect();
        let kept: Vec<u64> = mine(&app, &index, "Alice", true, rows)
            .iter()
            .map(|r| r.pr.number)
            .collect();
        assert_eq!(kept, [1]);
    }
}
