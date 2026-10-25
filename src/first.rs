//! Packages for a first contribution, and packages that need help. A
//! newcomer does better on a small package with no maintainer and
//! something recent to do than on glibc, or on an issue that has been
//! stuck for years.

use chrono::{DateTime, Duration, Utc};

use crate::area::Index;
use crate::catalog::{Flags, Subject};
use crate::health::Health;
use crate::html::{Html, html};
use crate::ui::{empty, item_row};
use crate::web::{App, Snapshot, ago};
use crate::welcome::MAINTAINERS_README;

/// Older open items are stuck, not a place to start.
pub(crate) const RECENT_DAYS: i64 = 60;
/// More open items than this is a busy package.
const MAX_OPEN: usize = 3;
/// A high risk score means advisories that a newcomer should not own.
const MAX_RISK: u32 = 29;

pub(crate) enum Task {
    Review { number: u64, title: String },
    Fix { number: u64, title: String },
}

pub(crate) struct Pick {
    pub(crate) attr: String,
    pub(crate) flags: Flags,
    pub(crate) task: Task,
    pub(crate) at: DateTime<Utc>,
}

/// Newest task first. Only packages under `pkgs/by-name` count: core
/// packages with many dependents live elsewhere.
pub(crate) fn picks(s: &Snapshot, index: &Index, h: &Health, now: DateTime<Utc>) -> Vec<Pick> {
    let since = now - Duration::days(RECENT_DAYS);
    let mut out = Vec::new();
    let keys = s.links.prs.keys().chain(s.links.issues.keys());
    let mut seen = std::collections::HashSet::new();
    for key in keys {
        let Some(attr) = key.strip_prefix("pkg:") else {
            continue;
        };
        if !seen.insert(attr) {
            continue;
        }
        let Some(p) = index.pkgs.get(attr) else {
            continue;
        };
        if !p.maintainers.is_empty()
            || !p.teams.is_empty()
            || !p
                .file
                .as_deref()
                .is_some_and(|f| f.starts_with("pkgs/by-name/"))
            || p.facts.as_ref().is_some_and(|f| f.broken || f.insecure)
            || index.by_pname.get(&p.pname).is_some_and(|a| a != attr)
        {
            continue;
        }
        let sub = Subject::Pkg(attr.to_string());
        let (prs, issues) = crate::catalog::attached(s, index, &sub);
        let prs: Vec<_> = prs.into_iter().filter(|p| p.is_open()).collect();
        let issues: Vec<_> = issues.into_iter().filter(|i| i.is_open()).collect();
        if prs.len() + issues.len() > MAX_OPEN {
            continue;
        }
        let review = prs
            .iter()
            .filter(|p| !p.is_draft && p.created_at >= since && p.treewide.is_none())
            .max_by_key(|p| p.created_at)
            .map(|p| {
                (
                    p.created_at,
                    Task::Review {
                        number: p.number,
                        title: p.title.clone(),
                    },
                )
            });
        let fix = issues
            .iter()
            .filter(|i| i.created_at >= since)
            .max_by_key(|i| i.created_at)
            .map(|i| {
                (
                    i.created_at,
                    Task::Fix {
                        number: i.number,
                        title: i.title.clone(),
                    },
                )
            });
        let Some((at, task)) = review.into_iter().chain(fix).max_by_key(|(at, _)| *at) else {
            continue;
        };
        let flags = Flags::of(s, index, h, &sub);
        if flags.risk() > MAX_RISK {
            continue;
        }
        out.push(Pick {
            attr: attr.to_string(),
            flags,
            task,
            at,
        });
    }
    out.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.attr.cmp(&b.attr)));
    out
}

const SHOWN: usize = 8;

/// Every pick, for the API and the MCP tool.
pub(crate) fn rows(app: &App) -> Vec<serde_json::Value> {
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    picks(&s, &index, &h, Utc::now())
        .into_iter()
        .map(|p| {
            let (kind, number, title, url) = match p.task {
                Task::Review { number, title } => (
                    "review",
                    number,
                    title,
                    format!("https://github.com/NixOS/nixpkgs/pull/{number}"),
                ),
                Task::Fix { number, title } => (
                    "fix",
                    number,
                    title,
                    format!("https://github.com/NixOS/nixpkgs/issues/{number}"),
                ),
            };
            serde_json::json!({
                "attr": p.attr,
                "flags": p.flags.json(),
                "task": { "kind": kind, "number": number, "title": title, "url": url },
                "at": p.at,
            })
        })
        .collect()
}

/// The list of the home page for a visitor, and for a person who
/// maintains nothing.
pub(crate) fn section(app: &App) -> Html {
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let list = {
        let index = app.index.read().unwrap();
        picks(&s, &index, &h, Utc::now())
    };
    let rows: Html = list
        .iter()
        .take(SHOWN)
        .map(|p| {
            let sub = Subject::Pkg(p.attr.clone());
            let task = match &p.task {
                Task::Review { number, title } => html!(
                    "Review {}, opened {} ago",
                    crate::prpage::title_link(*number, html!("#{number} {title}"), ""),
                    ago(p.at)
                ),
                Task::Fix { number, title } => html!(
                    "Fix <a href=\"https://github.com/NixOS/nixpkgs/issues/{number}\">#{number} {title}</a>, opened {} ago",
                    ago(p.at)
                ),
            };
            item_row(
                None,
                html!(
                    "<a class=\"Link--primary text-bold\" href=\"{}\">{} {}</a>",
                    sub.href(),
                    sub.icon(),
                    sub.label()
                ),
                "",
                task,
                &p.flags.badges(),
                "",
            )
        })
        .collect();
    let body = if rows.is_empty() {
        empty(
            "No package fits right now. Review a <a href=\"/review?tab=unowned\">pull request with no maintainer</a> instead.",
        )
    } else {
        html!(
            "<div class=\"Box-row f6 color-fg-muted\">Small packages with no maintainer and an issue or a pull request from the last {RECENT_DAYS} days. \
             Fix it or review it, then add yourself to <code>meta.maintainers</code>. \
             Read <a href=\"{MAINTAINERS_README}\">maintainers/README.md</a> first.</div>{rows}"
        )
    };
    html!(
        "<div id=\"first\">{}</div>",
        crate::ui::section("A first package to work on", None, None, body)
    )
}

/// Packages with an open advisory in master, the highest risk first.
pub(crate) fn need_help(s: &Snapshot, index: &Index, h: &Health) -> Vec<(Subject, Flags)> {
    let mut help: Vec<(Subject, Flags)> = h
        .advisories
        .keys()
        .filter(|a| {
            index
                .pkgs
                .get(*a)
                .is_some_and(|p| index.by_pname.get(&p.pname) == Some(*a))
        })
        .map(|a| {
            let sub = Subject::Pkg(a.clone());
            let f = Flags::of(s, index, h, &sub);
            (sub, f)
        })
        .filter(|(_, f)| f.risk() > 0)
        .collect();
    help.sort_by_key(|(sub, f)| (f.rank(), sub.label()));
    help
}

/// The list of the home page for a person who maintains a little: work
/// that helps, and packages to adopt.
pub(crate) fn help_section(app: &App) -> Html {
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let list = {
        let index = app.index.read().unwrap();
        need_help(&s, &index, &h)
    };
    let body = if list.is_empty() {
        empty(if h.tracker_at.is_none() {
            "The health data is not read yet."
        } else {
            "No package has an open advisory right now."
        })
    } else {
        let rows: Html = list
            .iter()
            .take(SHOWN)
            .map(|(sub, f)| crate::catalog::subject_row(sub, f, "", ""))
            .collect();
        html!(
            "<div class=\"Box-row f6 color-fg-muted\">Packages with an open advisory in master, the highest risk first. \
             Fix one. When it has no maintainer, add yourself to <code>meta.maintainers</code>.</div>{rows}"
        )
    };
    html!(
        "<div id=\"help\">{}</div>",
        crate::ui::section("Packages that need help", None, None, body)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::area::Package;
    use crate::model::{Issue, Pr};

    fn pkg(file: &str, maintainers: &[&str]) -> Package {
        Package {
            pname: String::new(),
            version: "1.0".into(),
            file: Some(file.into()),
            maintainers: maintainers.iter().map(|m| m.to_string()).collect(),
            ..Default::default()
        }
    }

    fn issue(number: u64, days: i64, now: DateTime<Utc>) -> Issue {
        Issue {
            number,
            title: format!("issue {number}"),
            url: String::new(),
            author: "someone".into(),
            state: "OPEN".into(),
            created_at: now - Duration::days(days),
            updated_at: now - Duration::days(days),
            closed_at: None,
            labels: vec![],
            comments: 0,
            mentions: vec![],
        }
    }

    fn pr(number: u64, days: i64, now: DateTime<Utc>) -> Pr {
        let mut p = crate::ledger::tests::pr(number, "r-ryantm", 0);
        p.created_at = now - Duration::days(days);
        p
    }

    #[test]
    fn only_small_unmaintained_packages_with_a_recent_task() {
        let now = Utc::now();
        let mut index = Index::default();
        for (attr, file, m) in [
            ("tiny", "pkgs/by-name/ti/tiny/package.nix", &[][..]),
            ("old", "pkgs/by-name/ol/old/package.nix", &[][..]),
            (
                "glibc",
                "pkgs/development/libraries/glibc/default.nix",
                &[][..],
            ),
            ("kept", "pkgs/by-name/ke/kept/package.nix", &["alice"][..]),
            ("busy", "pkgs/by-name/bu/busy/package.nix", &[][..]),
            ("bump", "pkgs/by-name/bu/bump/package.nix", &[][..]),
        ] {
            index.pkgs.insert(attr.into(), pkg(file, m));
        }
        let mut s = Snapshot::default();
        let add_issue = |s: &mut Snapshot, attr: &str, i: Issue| {
            s.issue_at.insert(i.number, s.issues.len());
            s.links
                .issues
                .entry(format!("pkg:{attr}"))
                .or_default()
                .push(i.number);
            s.issues.push(i);
        };
        add_issue(&mut s, "tiny", issue(1, 10, now));
        add_issue(&mut s, "old", issue(2, 5 * 365, now));
        add_issue(&mut s, "glibc", issue(3, 2, now));
        add_issue(&mut s, "kept", issue(4, 2, now));
        for n in 10..14 {
            add_issue(&mut s, "busy", issue(n, 1, now));
        }
        let p = pr(20, 3, now);
        s.pr_at.insert(p.number, s.pr_data.len());
        s.links.prs.insert("pkg:bump".into(), vec![p.number]);
        s.pr_data.push(p);

        let got = picks(&s, &index, &Health::default(), now);
        let attrs: Vec<&str> = got.iter().map(|p| p.attr.as_str()).collect();
        assert_eq!(attrs, ["bump", "tiny"]);
        assert!(matches!(got[0].task, Task::Review { number: 20, .. }));
        assert!(matches!(got[1].task, Task::Fix { number: 1, .. }));
    }
}
