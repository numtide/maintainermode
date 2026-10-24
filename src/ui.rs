//! The page skeleton and the parts every page is built from. A page is
//! data: a title, the tab it belongs to, optional sub-tabs, actions and a
//! body. One function turns it into HTML, so pages never copy the header.

use axum::http::header;
use axum::response::{IntoResponse, Response};

use crate::html::{Html, html};
use crate::web::{App, CONTRIBUTING, icon, octicon};

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) enum Section {
    #[default]
    Home,
    Review,
    Packages,
    Nixpkgs,
    /// A page of the personal menu: no top tab is current.
    Personal(Personal),
    /// No tab of the main navigation is current.
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Personal {
    Inbox,
    Progress,
    Runs,
    Settings,
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
    pub label: Html,
    pub count: Option<usize>,
    pub current: bool,
}

impl Tab {
    pub(crate) fn new(href: impl Into<String>, label: impl Into<Html>, current: bool) -> Self {
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

/// `title` is plain text: it fills `<title>` and the `h1`.
#[derive(Default)]
pub(crate) struct Page {
    pub title: String,
    pub section: Section,
    pub icon: &'static str,
    pub sub: Html,
    pub subnav: Vec<Tab>,
    pub actions: Html,
    pub body: Html,
    /// The `h1` in place of the title.
    pub heading: Html,
    /// No page header: the body brings its own `h1`.
    pub bare: bool,
    /// The words in the search box of the header.
    pub query: String,
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

    pub(crate) fn sub(mut self, html: impl Into<Html>) -> Self {
        self.sub = html.into();
        self
    }

    pub(crate) fn subnav(mut self, tabs: Vec<Tab>) -> Self {
        self.subnav = tabs;
        self
    }

    pub(crate) fn actions(mut self, html: impl Into<Html>) -> Self {
        self.actions = html.into();
        self
    }

    pub(crate) fn body(mut self, html: impl Into<Html>) -> Self {
        self.body = html.into();
        self
    }

    pub(crate) fn heading(mut self, html: impl Into<Html>) -> Self {
        self.heading = html.into();
        self
    }

    pub(crate) fn query(mut self, q: &str) -> Self {
        self.query = q.to_string();
        self
    }

    pub(crate) fn bare(mut self) -> Self {
        self.bare = true;
        self
    }

    /// A page for a person who signed in carries their login and inbox
    /// count, so no cache may keep it.
    pub(crate) fn render(self, app: &App, viewer: Option<&str>) -> Response {
        let html = document(app, viewer, &self);
        match viewer {
            Some(_) => ([(header::CACHE_CONTROL, "private, no-store")], html).into_response(),
            None => html.into_response(),
        }
    }
}

fn current(yes: bool) -> Html {
    if yes {
        Html::from(" aria-current=\"page\"")
    } else {
        Html::new()
    }
}

fn counter(n: Option<usize>) -> Html {
    n.map(|n| html!(" <span class=\"Counter\">{n}</span>"))
        .unwrap_or_default()
}

fn nav(section_now: Section) -> Html {
    NAV.iter()
        .map(|&(section, href, svg, label)| {
            let cur = current(section == section_now);
            html!(
                "<li><a class=\"site-tab\" href=\"{href}\"{cur}>{}<span>{label}</span></a></li>",
                icon(svg)
            )
        })
        .collect()
}

pub(crate) fn subnav(tabs: &[Tab]) -> Html {
    subnav_named(tabs, "Views")
}

pub(crate) fn subnav_named(tabs: &[Tab], name: &str) -> Html {
    if tabs.is_empty() {
        return Html::new();
    }
    let mut out = html!("<nav class=\"subnav mb-3\" aria-label=\"{name}\">");
    for t in tabs {
        let sel = if t.current { " selected" } else { "" };
        let cur = current(t.current);
        let count = counter(t.count);
        out.push(&html!(
            "<a class=\"SegmentedControl-item btn btn-sm{sel}\" href=\"{}\"{cur}>{}{count}</a>",
            t.href,
            t.label
        ));
    }
    out.push(&"</nav>".into());
    out
}

fn menu(app: &App, viewer: Option<&str>, section_now: Section) -> Html {
    let Some(v) = viewer else {
        return if app.oauth.is_some() {
            html!(
                "<a class=\"btn btn-sm btn-primary\" href=\"/auth/login\">{} Sign in with GitHub</a>",
                icon(octicon!("sign-in"))
            )
        } else {
            Html::new()
        };
    };
    let n = app.inbox.get(v);
    let bell = if n == 0 {
        Html::new()
    } else {
        html!(
            "<a class=\"Header-link d-flex flex-items-center mr-3\" href=\"/inbox\" aria-label=\"Inbox: {n} unread\">{}<span class=\"Counter ml-1\">{n}</span></a>",
            icon(octicon!("inbox"))
        )
    };
    let start = if crate::welcome::pending(app, v) {
        Html::from("<a href=\"/#start\">Get started</a>")
    } else {
        Html::new()
    };
    let items: Html = MENU
        .iter()
        .map(|&(item, href, svg, label)| {
            let cur = current(section_now == Section::Personal(item));
            let count = match item {
                Personal::Inbox if n > 0 => {
                    html!(" <span class=\"Counter\" title=\"unread in your inbox\">{n}</span>")
                }
                _ => Html::new(),
            };
            html!("<a href=\"{href}\"{cur}>{} {label}{count}</a>", icon(svg))
        })
        .collect();
    html!(
        "{bell}<details class=\"menu\"><summary class=\"Header-link\" aria-label=\"Your menu\">{person}<span class=\"menu-login\">{v}</span></summary>\
         <div class=\"menu-list\">{start}{items}\
         <form method=\"post\" action=\"/auth/logout\" class=\"menu-sep\"><button type=\"submit\">{out} Sign out</button></form>\
         </div></details>",
        person = icon(octicon!("person")),
        out = icon(octicon!("sign-out")),
    )
}

const MENU: [(Personal, &str, &str, &str); 4] = [
    (Personal::Inbox, "/inbox", octicon!("inbox"), "Inbox"),
    (
        Personal::Progress,
        "/me/progress",
        octicon!("pulse"),
        "Progress",
    ),
    (Personal::Runs, "/me/runs", octicon!("play"), "Runs"),
    (
        Personal::Settings,
        "/me/settings",
        octicon!("gear"),
        "Settings",
    ),
];

const ISSUES: &str = "https://github.com/numtide/maintainermode/issues";

pub(crate) const SEARCH_PLACEHOLDER: &str = "Search packages, modules, @people, #PRs, CVEs";

fn search_box(q: &str) -> Html {
    html!(
        "<form class=\"site-search\" role=\"search\" method=\"get\" action=\"/search\">{search}\
         <input class=\"site-q\" type=\"search\" name=\"q\" value=\"{q}\" placeholder=\"{SEARCH_PLACEHOLDER}\" aria-label=\"Search\" aria-keyshortcuts=\"/\" autocomplete=\"off\" data-search>\
         <kbd class=\"site-kbd\" aria-hidden=\"true\">/</kbd></form>\
         <a class=\"Header-link site-search-link\" href=\"/search\" aria-label=\"Search\">{search}</a>",
        search = icon(octicon!("search")),
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

fn document(app: &App, viewer: Option<&str>, p: &Page) -> Html {
    let h1 = if p.heading.is_empty() {
        Html::text(&p.title)
    } else {
        p.heading.clone()
    };
    let head = if p.bare {
        Html::new()
    } else {
        page_header(p.icon, h1, p.sub.clone(), p.actions.clone())
    };
    let title = if p.title.is_empty() || p.title == "Maintainer Mode" {
        Html::from("Maintainer Mode")
    } else {
        html!("{} · Maintainer Mode", p.title)
    };
    let club = icon(octicon!("people"));
    let menu = menu(app, viewer, p.section);
    let nav = nav(p.section);
    let subnav = subnav(&p.subnav);
    let search = search_box(&p.query);
    let body = &p.body;
    let sprite = crate::assets::sprite(&[&club, &menu, &nav, &search, &head, &subnav, body]);
    let css = crate::assets::stylesheets();
    let js = crate::assets::scripts();
    let nvd = if app.store.get("nvd_api").ok().flatten().is_some() {
        Html::from("<br>This product uses the NVD API but is not endorsed or certified by the NVD.")
    } else {
        Html::new()
    };
    html!(
        r##"<!doctype html>
<html lang="en" data-color-mode="auto" data-light-theme="light" data-dark-theme="dark">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
{css}
{js}
</head>
<body class="color-bg-default color-fg-default">
{sprite}<a class="skip-link" href="#main">Skip to content</a>
<header class="Header site-header">
  <a class="Header-link site-brand" href="/" aria-label="Maintainer Mode, home">{club}<span class="site-name">Maintainer Mode</span></a>
  <a class="site-beta" href="{ISSUES}" title="Work in progress: pages, numbers and links can change or break. Tell us what is wrong or missing.">beta</a>
  <nav class="site-nav" aria-label="Main"><ul>{nav}</ul></nav>
  {search}
  <div class="site-menu">{menu}</div>
</header>
<main id="main" class="container-lg px-3 py-4" tabindex="-1">{head}{subnav}{body}</main>
<footer class="container-lg px-3 py-4 f6 color-fg-muted border-top">
  <strong>Work in progress.</strong> Maintainer Mode changes every day: pages, numbers and links can change or break. <a href="{ISSUES}">Tell us what is wrong or missing</a>.<span class="keys-hint"> Press <kbd>?</kbd> for keyboard shortcuts.</span><br>
  Data from the last {window} days · {updated} · <a href="/state?tab=status">Data status</a> · <a href="{CONTRIBUTING}">nixpkgs contributor guide</a> · <a href="/state?tab=rules#the-rules">the rules</a> · <a href="/api/v1">API</a><br>
  A personal tool for nixpkgs maintenance, run in public. Not an official NixOS project, and not made by or affiliated with GitHub. Styled with Primer and Octicons (MIT), in the NixOS brand colors.{nvd}
</footer>
</body>
</html>"##,
        window = crate::ledger::WINDOW_DAYS,
        updated = updated(app),
    )
}

/// On a phone the actions wrap below the title.
pub(crate) fn page_header(
    svg: &str,
    title: impl Into<Html>,
    sub: impl Into<Html>,
    actions: impl Into<Html>,
) -> Html {
    let (title, sub, actions) = (title.into(), sub.into(), actions.into());
    let ic = if svg.is_empty() {
        Html::new()
    } else {
        html!("{} ", icon(svg))
    };
    let sub = if sub.is_empty() {
        sub
    } else {
        html!("<div class=\"Subhead-description\">{sub}</div>")
    };
    let actions = if actions.is_empty() {
        actions
    } else {
        html!("<div class=\"page-actions\">{actions}</div>")
    };
    html!(
        "<div class=\"Subhead page-head\"><div class=\"page-head-main\"><h1 class=\"Subhead-heading\">{ic}{title}</h1>{sub}</div>{actions}</div>"
    )
}

/// A box with an `h2`. An empty `body` shows "None." in its place; pass
/// [`empty`] for other words. `more` is a link: its href and its label, as
/// text.
pub(crate) fn section(
    title: impl Into<Html>,
    count: Option<usize>,
    more: Option<(&str, &str)>,
    body: impl Into<Html>,
) -> Html {
    let (title, body) = (title.into(), body.into());
    let count = counter(count);
    let more = more
        .map(|(href, label)| html!("<a class=\"f6 ml-2\" href=\"{href}\">{label}</a>"))
        .unwrap_or_default();
    let body = if body.is_empty() {
        empty("None.")
    } else {
        body
    };
    html!(
        "<section class=\"Box mb-4\"><div class=\"Box-header\"><h2 class=\"Box-title\">{title}{count}</h2>{more}</div>{body}</section>"
    )
}

pub(crate) fn empty(text: impl Into<Html>) -> Html {
    let text = text.into();
    html!("<div class=\"Box-row color-fg-muted\">{text}</div>")
}

pub(crate) fn kv(label: impl Into<Html>, value: impl Into<Html>) -> Html {
    let (label, value) = (label.into(), value.into());
    html!(
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

pub(crate) fn badge(tone: Tone, text: impl Into<Html>) -> Html {
    let text = text.into();
    let class = match tone {
        Tone::Danger => "Label Label--danger",
        Tone::Attention => "Label Label--attention",
        Tone::Success => "Label Label--success",
        Tone::Neutral => "Label",
    };
    html!("<span class=\"{class}\">{text}</span>")
}

/// A neutral callout. The yellow `flash` is for warnings only.
pub(crate) fn note(html: impl Into<Html>) -> Html {
    let html = html.into();
    html!("<div class=\"callout mb-3\">{html}</div>")
}

#[derive(Clone, Copy)]
pub(crate) struct Kind {
    svg: &'static str,
    class: &'static str,
    label: &'static str,
}

impl Kind {
    /// The icon of a pull request in GitHub's `state`: OPEN, CLOSED or MERGED.
    pub(crate) fn of_pr(state: &str) -> Kind {
        match state {
            "MERGED" => Kind::PR_MERGED,
            "CLOSED" => Kind::PR_CLOSED,
            _ => Kind::PR_OPEN,
        }
    }

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

/// One row for a pull request, an issue or a package. With an empty `href`,
/// `title` is used as it is, so it can bring its own link. `status` sits on
/// the right: the status icon must carry text or an `aria-label`.
pub(crate) fn item_row(
    kind: Option<Kind>,
    title: impl Into<Html>,
    href: &str,
    meta: impl Into<Html>,
    badges: &[Html],
    status: impl Into<Html>,
) -> Html {
    let (title, meta, status) = (title.into(), meta.into(), status.into());
    let ic = kind
        .map(|k| {
            html!(
                "<span class=\"mr-2 {}\" role=\"img\" aria-label=\"{}\">{}</span>",
                k.class,
                k.label,
                icon(k.svg)
            )
        })
        .unwrap_or_default();
    let title = if href.is_empty() {
        title
    } else {
        html!("<a class=\"Link--primary text-bold\" href=\"{href}\">{title}</a>")
    };
    let badges: Html = badges
        .iter()
        .filter(|b| !b.is_empty())
        .take(MAX_BADGES)
        .map(|b| html!(" {b}"))
        .collect();
    let meta = if meta.is_empty() {
        meta
    } else {
        html!("<div class=\"f6 color-fg-muted mt-1\">{meta}</div>")
    };
    html!(
        "<div class=\"Box-row d-flex flex-items-start\">{ic}<div class=\"flex-auto min-width-0 wb-break-word\">{title}{badges}{meta}</div>{status}</div>"
    )
}

/// `href` already holds its query string, or none; `after` is the cursor of
/// the next page.
pub(crate) fn pager(href: &str, after: Option<&str>) -> Html {
    let Some(after) = after else {
        return Html::new();
    };
    let sep = Html::from(if href.contains('?') { "&amp;" } else { "?" });
    let after = crate::github::urlencode(after);
    html!(
        "<nav class=\"d-flex flex-justify-center my-3\" aria-label=\"Pages\"><a class=\"btn btn-sm\" href=\"{href}{sep}after={after}\">Next page</a></nav>"
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
        assert!(!nav(Section::Other).contains("aria-current"));
    }

    #[test]
    fn a_personal_page_marks_its_menu_item_and_no_tab() {
        let app = crate::sets::tests::app(&[]);
        for (item, href, ..) in MENU {
            let s = Section::Personal(item);
            assert_eq!(nav(s).matches("aria-current").count(), 0);
            let m = menu(&app, Some("alice"), s);
            assert_eq!(m.matches("aria-current").count(), 1);
            assert!(m.contains(&format!("href=\"{href}\" aria-current=\"page\"")));
        }
        assert!(!menu(&app, Some("alice"), Section::Home).contains("aria-current"));
    }

    #[test]
    fn a_row_shows_at_most_three_badges() {
        let b: Vec<Html> = (0..5)
            .map(|i| badge(Tone::Neutral, Html::text(&i.to_string())))
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
