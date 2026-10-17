//! The page skeleton and the parts every page is built from. A page is
//! data: a title, the tab it belongs to, optional sub-tabs, actions and a
//! body. One function turns it into HTML, so pages never copy the header.

use axum::http::header;
use axum::response::{Html, IntoResponse, Response};
use std::fmt::Write;

use crate::web::{App, CONTRIBUTING, esc, icon, octicon};

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) enum Section {
    #[default]
    Home,
    Review,
    Packages,
    Nixpkgs,
}

pub(crate) const NAV: [(Section, &str, &str, &str); 4] = [
    (Section::Home, "/", octicon!("home"), "Home"),
    (Section::Review, "/review", octicon!("eye"), "Review"),
    (
        Section::Packages,
        "/packages",
        octicon!("package"),
        "Packages",
    ),
    (Section::Nixpkgs, "/state", octicon!("graph"), "nixpkgs"),
];

pub(crate) struct Tab {
    pub href: String,
    pub label: String,
    pub count: Option<usize>,
    pub current: bool,
}

impl Tab {
    pub(crate) fn new(href: impl Into<String>, label: impl Into<String>, current: bool) -> Self {
        Self {
            href: href.into(),
            label: label.into(),
            count: None,
            current,
        }
    }

    pub(crate) fn count(mut self, n: usize) -> Self {
        self.count = Some(n);
        self
    }
}

/// `title` is plain text: it fills `<title>` and the `h1`. `sub`, `actions`
/// and `body` are HTML.
#[derive(Default)]
pub(crate) struct Page {
    pub title: String,
    pub section: Section,
    pub icon: &'static str,
    pub sub: String,
    pub subnav: Vec<Tab>,
    pub actions: String,
    pub body: String,
    /// HTML for the `h1` in place of the escaped title.
    pub heading: String,
}

impl Page {
    pub(crate) fn new(section: Section, title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            section,
            ..Self::default()
        }
    }

    pub(crate) fn icon(mut self, svg: &'static str) -> Self {
        self.icon = svg;
        self
    }

    pub(crate) fn sub(mut self, html: impl Into<String>) -> Self {
        self.sub = html.into();
        self
    }

    pub(crate) fn subnav(mut self, tabs: Vec<Tab>) -> Self {
        self.subnav = tabs;
        self
    }

    pub(crate) fn actions(mut self, html: impl Into<String>) -> Self {
        self.actions = html.into();
        self
    }

    pub(crate) fn body(mut self, html: impl Into<String>) -> Self {
        self.body = html.into();
        self
    }

    pub(crate) fn heading(mut self, html: impl Into<String>) -> Self {
        self.heading = html.into();
        self
    }

    /// A page for a person who signed in carries their login and inbox
    /// count, so no cache may keep it.
    pub(crate) fn render(self, app: &App, viewer: Option<&str>) -> Response {
        let html = Html(document(app, viewer, &self));
        match viewer {
            Some(_) => ([(header::CACHE_CONTROL, "private, no-store")], html).into_response(),
            None => html.into_response(),
        }
    }
}

fn nav(current: Section) -> String {
    let mut out = String::new();
    for (section, href, svg, label) in NAV {
        let cur = if section == current {
            " aria-current=\"page\""
        } else {
            ""
        };
        let _ = write!(
            out,
            "<li class=\"d-inline-flex\"><a class=\"UnderlineNav-item\" href=\"{href}\"{cur}>{}<span>{label}</span></a></li>",
            icon(svg)
        );
    }
    out
}

pub(crate) fn subnav(tabs: &[Tab]) -> String {
    if tabs.is_empty() {
        return String::new();
    }
    let mut out = String::from("<nav class=\"subnav mb-3\" aria-label=\"Views\">");
    for t in tabs {
        let (sel, cur) = if t.current {
            (" selected", " aria-current=\"page\"")
        } else {
            ("", "")
        };
        let count = t
            .count
            .map(|n| format!(" <span class=\"Counter\">{n}</span>"))
            .unwrap_or_default();
        let _ = write!(
            out,
            "<a class=\"SegmentedControl-item btn btn-sm{sel}\" href=\"{}\"{cur}>{}{count}</a>",
            esc(&t.href),
            t.label
        );
    }
    out.push_str("</nav>");
    out
}

fn menu(app: &App, viewer: Option<&str>) -> String {
    let Some(v) = viewer else {
        return if app.oauth.is_some() {
            format!(
                "<a class=\"btn btn-sm btn-primary\" href=\"/auth/login\">{} Sign in with GitHub</a>",
                icon(octicon!("sign-in"))
            )
        } else {
            String::new()
        };
    };
    let unread = match app.inbox.get(v) {
        0 => String::new(),
        n => format!(" <span class=\"Counter\" title=\"unread in your inbox\">{n}</span>"),
    };
    let start = if crate::welcome::pending(app, v) {
        "<a href=\"/#start\">Get started</a>"
    } else {
        ""
    };
    format!(
        "<details class=\"menu\"><summary class=\"Header-link\" aria-label=\"Your menu\">{person}<span class=\"menu-login\">{v}</span>{unread}</summary>\
         <div class=\"menu-list\">{start}\
         <a href=\"/me\">{pkg} My packages</a>\
         <a href=\"/me/progress\">{flame} Progress</a>\
         <a href=\"/me/runs\">{play} Runs</a>\
         <a href=\"/me/settings\">{gear} Settings</a>\
         <form method=\"post\" action=\"/auth/logout\" class=\"menu-sep\"><button type=\"submit\">{out} Sign out</button></form>\
         </div></details>",
        person = icon(octicon!("person")),
        pkg = icon(octicon!("package")),
        flame = icon(octicon!("flame")),
        play = icon(octicon!("play")),
        gear = icon(octicon!("gear")),
        out = icon(octicon!("sign-out")),
        v = esc(v),
    )
}

fn updated(app: &App) -> String {
    match app.store.get_time("last_sync").ok().flatten() {
        Some(t) => {
            let m = (chrono::Utc::now() - t).num_minutes().max(0);
            if m < 120 {
                format!("updated {m} min ago")
            } else {
                format!("updated {} ago", crate::web::ago(t))
            }
        }
        None => "not updated yet".into(),
    }
}

fn document(app: &App, viewer: Option<&str>, p: &Page) -> String {
    let h1 = if p.heading.is_empty() {
        esc(&p.title)
    } else {
        p.heading.clone()
    };
    let head = page_header(p.icon, &h1, &p.sub, &p.actions);
    let title = if p.title.is_empty() || p.title == "Maintainer Mode" {
        "Maintainer Mode".to_string()
    } else {
        format!("{} · Maintainer Mode", esc(&p.title))
    };
    let club = icon(octicon!("people"));
    let menu = menu(app, viewer);
    let nav = nav(p.section);
    let subnav = subnav(&p.subnav);
    let sprite = crate::assets::sprite(&[&club, &menu, &nav, &head, &subnav, &p.body]);
    format!(
        r#"<!doctype html>
<html lang="en" data-color-mode="auto" data-light-theme="light" data-dark-theme="dark">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<link rel="stylesheet" href="/static/primer.css">
<link rel="stylesheet" href="/static/club.css">
</head>
<body class="color-bg-default color-fg-default">
{sprite}<header class="Header">
  <div class="Header-item Header-item--full"><a class="Header-link f4 d-flex flex-items-center" href="/">{club} <span class="ml-2">Maintainer Mode</span></a></div>
  <div class="Header-item mr-0">{menu}</div>
</header>
<div class="flux px-3 py-1 f6" role="note"><strong>Work in progress.</strong> Maintainer Mode changes every day: pages, numbers and links can change or break. <a href="https://github.com/numtide/maintainermode/issues">Tell us what is wrong or missing</a>.</div>
<nav class="UnderlineNav px-3" aria-label="Main"><ul class="UnderlineNav-body list-style-none">{nav}</ul></nav>
<main class="container-lg px-3 py-4">{head}{subnav}{body}</main>
<footer class="container-lg px-3 py-4 f6 color-fg-muted border-top">
  Data from the last {window} days · {updated} · <a href="/state?tab=status">Data status</a> · <a href="{CONTRIBUTING}">nixpkgs contributor guide</a> · <a href="/state?tab=rules#the-rules">the rules</a> · <a href="/api/v1/status">API</a><br>
  A personal tool for nixpkgs maintenance, run in public. Not an official NixOS project, and not made by or affiliated with GitHub. Styled with Primer and Octicons (MIT), in the NixOS brand colors.{nvd}
</footer>
</body>
</html>"#,
        body = p.body,
        window = crate::ledger::WINDOW_DAYS,
        updated = updated(app),
        nvd = if app.store.get("nvd_api").ok().flatten().is_some() {
            "<br>This product uses the NVD API but is not endorsed or certified by the NVD."
        } else {
            ""
        },
    )
}

/// `title` is HTML. On a phone the actions wrap below the title.
pub(crate) fn page_header(svg: &str, title: &str, sub: &str, actions: &str) -> String {
    let ic = if svg.is_empty() {
        String::new()
    } else {
        format!("{} ", icon(svg))
    };
    let sub = if sub.is_empty() {
        String::new()
    } else {
        format!("<div class=\"Subhead-description\">{sub}</div>")
    };
    let actions = if actions.is_empty() {
        String::new()
    } else {
        format!("<div class=\"page-actions\">{actions}</div>")
    };
    format!(
        "<div class=\"Subhead page-head\"><div class=\"page-head-main\"><h1 class=\"Subhead-heading\">{ic}{title}</h1>{sub}</div>{actions}</div>"
    )
}

/// A box with an `h2`. An empty `body` shows "None." in its place; pass
/// [`empty`] for other words.
pub(crate) fn section(
    title: &str,
    count: Option<usize>,
    more: Option<(&str, &str)>,
    body: &str,
) -> String {
    let count = count
        .map(|n| format!(" <span class=\"Counter\">{n}</span>"))
        .unwrap_or_default();
    let more = more
        .map(|(href, label)| format!("<a class=\"f6 ml-2\" href=\"{href}\">{label}</a>"))
        .unwrap_or_default();
    let body = if body.is_empty() {
        empty("None.")
    } else {
        body.to_string()
    };
    format!(
        "<section class=\"Box mb-4\"><div class=\"Box-header\"><h2 class=\"Box-title\">{title}{count}</h2>{more}</div>{body}</section>"
    )
}

pub(crate) fn empty(text: &str) -> String {
    format!("<div class=\"Box-row color-fg-muted\">{text}</div>")
}

pub(crate) fn kv(label: &str, value: &str) -> String {
    format!(
        "<div class=\"Box-row kv\"><div class=\"kv-label\">{label}</div><div class=\"flex-auto min-width-0\">{value}</div></div>"
    )
}

/// Each tone has one meaning on every page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    /// A risk.
    Danger,
    /// Act now.
    Attention,
    /// Done.
    Success,
    /// Information.
    Neutral,
}

/// `text` is HTML.
pub(crate) fn badge(tone: Tone, text: &str) -> String {
    let class = match tone {
        Tone::Danger => "Label Label--danger",
        Tone::Attention => "Label Label--attention",
        Tone::Success => "Label Label--success",
        Tone::Neutral => "Label",
    };
    format!("<span class=\"{class}\">{text}</span>")
}

/// A neutral callout. The yellow `flash` is for warnings only.
pub(crate) fn note(html: &str) -> String {
    format!("<div class=\"callout mb-3\">{html}</div>")
}

#[derive(Clone, Copy)]
pub(crate) struct Kind {
    svg: &'static str,
    class: &'static str,
    label: &'static str,
}

impl Kind {
    pub(crate) const PR_OPEN: Kind = Kind {
        svg: octicon!("git-pull-request"),
        class: "color-fg-open",
        label: "open pull request",
    };
    pub(crate) const PR_MERGED: Kind = Kind {
        svg: octicon!("git-merge"),
        class: "color-fg-done",
        label: "merged pull request",
    };
    pub(crate) const PR_CLOSED: Kind = Kind {
        svg: octicon!("git-pull-request-closed"),
        class: "color-fg-danger",
        label: "closed pull request",
    };
    pub(crate) const ISSUE_OPEN: Kind = Kind {
        svg: octicon!("issue-opened"),
        class: "color-fg-open",
        label: "open issue",
    };
    pub(crate) const ISSUE_CLOSED: Kind = Kind {
        svg: octicon!("issue-closed"),
        class: "color-fg-done",
        label: "closed issue",
    };
}

pub(crate) const MAX_BADGES: usize = 3;

/// One row for a pull request, an issue or a package. `title` is HTML; with
/// an empty `href` it is used as it is, so it can bring its own link.
/// `status` sits on the right: the status icon must carry text or an
/// `aria-label`.
pub(crate) fn item_row(
    kind: Option<Kind>,
    title: &str,
    href: &str,
    meta: &str,
    badges: &[String],
    status: &str,
) -> String {
    let ic = kind
        .map(|k| {
            format!(
                "<span class=\"mr-2 {}\" role=\"img\" aria-label=\"{}\">{}</span>",
                k.class,
                k.label,
                icon(k.svg)
            )
        })
        .unwrap_or_default();
    let title = if href.is_empty() {
        title.to_string()
    } else {
        format!("<a class=\"Link--primary text-bold\" href=\"{href}\">{title}</a>")
    };
    let badges: String = badges
        .iter()
        .filter(|b| !b.is_empty())
        .take(MAX_BADGES)
        .map(|b| format!(" {b}"))
        .collect();
    let meta = if meta.is_empty() {
        String::new()
    } else {
        format!("<div class=\"f6 color-fg-muted mt-1\">{meta}</div>")
    };
    format!(
        "<div class=\"Box-row d-flex flex-items-start\">{ic}<div class=\"flex-auto min-width-0\">{title}{badges}{meta}</div>{status}</div>"
    )
}

/// `href` already holds its query string, or none; `after` is the cursor of
/// the next page.
pub(crate) fn pager(href: &str, after: Option<&str>) -> String {
    let Some(after) = after else {
        return String::new();
    };
    let sep = if href.contains('?') { "&amp;" } else { "?" };
    format!(
        "<nav class=\"d-flex flex-justify-center my-3\" aria-label=\"Pages\"><a class=\"btn btn-sm\" href=\"{}{sep}after={}\">Next page</a></nav>",
        esc(href),
        crate::github::urlencode(after)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_has_one_tab() {
        for s in [
            Section::Home,
            Section::Review,
            Section::Packages,
            Section::Nixpkgs,
        ] {
            assert_eq!(NAV.iter().filter(|(n, ..)| *n == s).count(), 1);
            assert_eq!(nav(s).matches("aria-current").count(), 1);
        }
    }

    #[test]
    fn a_row_shows_at_most_three_badges() {
        let b: Vec<String> = (0..5)
            .map(|i| badge(Tone::Neutral, &i.to_string()))
            .collect();
        let row = item_row(Some(Kind::PR_OPEN), "t", "/pr/1", "", &b, "");
        assert_eq!(row.matches("class=\"Label\"").count(), MAX_BADGES);
        assert!(row.contains("aria-label=\"open pull request\""));
    }

    #[test]
    fn the_header_escapes_nothing_twice() {
        let h = page_header("", "a &amp; b", "", "");
        assert!(h.contains(">a &amp; b</h1>"));
        assert!(!h.contains("page-actions"));
    }

    #[test]
    fn a_pager_keeps_the_query() {
        assert!(pager("/review?tab=all", Some("12")).contains("/review?tab=all&amp;after=12"));
        assert!(pager("/review", Some("12")).contains("/review?after=12"));
        assert!(pager("/review", None).is_empty());
    }

    #[test]
    fn an_empty_section_says_so() {
        assert!(section("X", Some(0), None, "").contains("None."));
    }
}
