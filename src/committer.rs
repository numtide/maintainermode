//! Who has merge rights, from public data, and the view for them.

use anyhow::{Result, bail};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use crate::area::Index;
use crate::catalog::Subject;
use crate::ledger::Ledger;
use crate::links::Links;
use crate::model::Pr;
use crate::queue::{self, Entry};
use crate::store::Store;
use crate::ui::{Kind, empty, item_row, note, pager, section};
use crate::web::{Snapshot, ago, ci_label, entry_rows, esc, person_link};

pub const TEAM: &str = "NixOS/nixpkgs-committers";
const MERGED_DAYS: i64 = 365;
pub const SECURITY: &str = "1.severity: security";
/// Rows of each board list on the first page of the tab.
const PREVIEW: usize = 5;
/// Rows of "Ready to merge" on the first page; later pages hold `review::PAGE`.
const FIRST: usize = 20;
const GUIDE: &str = "https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md";

/// Logins are kept in lower case: `meta.maintainers` and GitHub disagree on case.
#[derive(Debug, Default)]
pub struct Committers {
    logins: HashSet<String>,
    pub from_merges: usize,
    pub from_team: usize,
    pub team_at: Option<DateTime<Utc>>,
}

impl Committers {
    /// `history` holds the merges outside the ledger's window: (merged_at, merged_by).
    pub fn build(
        prs: &[Pr],
        history: &[(DateTime<Utc>, String)],
        team: &[String],
        team_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Self {
        let since = now - Duration::days(MERGED_DAYS);
        let merged: HashSet<String> = prs
            .iter()
            .filter_map(|p| p.merged_at.zip(p.merged_by.as_deref()))
            .chain(history.iter().map(|(at, by)| (*at, by.as_str())))
            .filter(|(at, _)| *at >= since)
            .map(|(_, by)| by.to_lowercase())
            .collect();
        let team: HashSet<String> = team.iter().map(|l| l.to_lowercase()).collect();
        Committers {
            from_merges: merged.len(),
            from_team: team.len(),
            logins: merged.union(&team).cloned().collect(),
            team_at,
        }
    }

    pub fn load(store: &Store, prs: &[Pr], now: DateTime<Utc>) -> Self {
        let team: Vec<String> = store
            .get("committers_team")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let at = store.get_time("committers_team_read").ok().flatten();
        let history = store.merges().unwrap_or_default();
        Self::build(prs, &history, &team, at, now)
    }

    pub fn is(&self, login: &str) -> bool {
        self.logins.contains(&login.to_lowercase())
    }

    pub fn len(&self) -> usize {
        self.logins.len()
    }
}

/// Reads the team once a day with the server's own tokens. A token that may
/// not see the team is normal: the merges in the synced data still count.
pub async fn refresh_team(store: &Store, tokens: Vec<String>) -> Result<()> {
    let tried = store.get_time("committers_team_tried")?;
    if tokens.is_empty() || tried.is_some_and(|t| Utc::now() - t < Duration::hours(24)) {
        return Ok(());
    }
    store.set("committers_team_tried", &Utc::now().to_rfc3339())?;
    let (org, slug) = TEAM.split_once('/').unwrap();
    let query = "query($org: String!, $slug: String!, $after: String) { rateLimit { remaining }
        organization(login: $org) { team(slug: $slug) {
          members(first: 100, after: $after, membership: ALL) { nodes { login } pageInfo { hasNextPage endCursor } } } } }";
    let mut client = crate::github::Client::new(tokens);
    let mut after = serde_json::Value::Null;
    let mut logins = Vec::new();
    for _ in 0..50 {
        let data = client
            .graphql(
                store,
                query,
                json!({ "org": org, "slug": slug, "after": after }),
            )
            .await?;
        let members = &data["organization"]["team"]["members"];
        if members.is_null() {
            bail!("{TEAM} is not visible to the server token");
        }
        logins.extend(
            members["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|n| n["login"].as_str())
                .map(str::to_string),
        );
        if members["pageInfo"]["hasNextPage"] != true {
            store.set("committers_team", &serde_json::to_string(&logins)?)?;
            store.set("committers_team_read", &Utc::now().to_rfc3339())?;
            return Ok(());
        }
        after = members["pageInfo"]["endCursor"].clone();
    }
    bail!("{TEAM}: too many pages")
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    #[serde(flatten)]
    pub entry: Entry,
    /// When the approval it waits on was given.
    pub waiting_since: DateTime<Utc>,
    pub why: String,
    /// `why` with each login linked to its profile.
    #[serde(skip)]
    pub why_html: String,
}

/// Public data only: every viewer gets the same board.
#[derive(Debug, Default, Serialize)]
pub struct Board {
    pub maintainer_approved: Vec<Row>,
    pub security: Vec<Row>,
    pub backports: Vec<Row>,
    pub waiting: Vec<Row>,
}

/// Looser than the merge queue, because a committer judges the rest: a known
/// conflict stops it, an unknown `mergeable` does not, and no credit is needed.
fn ready(e: &Entry) -> bool {
    e.ci.as_deref() == Some("SUCCESS") && e.mergeable != "CONFLICTING" && !e.approved_by.is_empty()
}

/// Approvals on the current head by someone other than the author, oldest first.
pub(crate) fn approvals(pr: &Pr) -> Vec<(&str, DateTime<Utc>)> {
    let mut out: Vec<(&str, DateTime<Utc>)> = pr
        .reviews
        .iter()
        .filter(|r| {
            r.state == "APPROVED"
                && r.commit_oid.as_deref() == Some(pr.head_oid.as_str())
                && r.author != pr.author
                && !r.author_is_bot
        })
        .map(|r| (r.author.as_str(), r.submitted_at))
        .collect();
    out.sort_by_key(|(_, t)| *t);
    out
}

fn maintainers_of(index: &Index, key: &str) -> Vec<String> {
    let found = match Subject::parse(key) {
        Some(Subject::Pkg(a)) => index.pkgs.get(&a).map(|p| (&p.maintainers, &p.teams)),
        Some(Subject::Module(f)) => index.modules.get(&f).map(|m| (&m.maintainers, &m.teams)),
        _ => None,
    };
    let Some((logins, teams)) = found else {
        return Vec::new();
    };
    logins
        .iter()
        .cloned()
        .chain(index.team_members(teams))
        .map(|l| l.to_lowercase())
        .collect()
}

pub fn board(prs: &[Pr], ledger: &Ledger, index: &Index, links: &Links) -> Board {
    let credited = queue::credited(ledger);
    let open: Vec<&Pr> = prs.iter().filter(|p| p.is_open() && !p.is_draft).collect();
    let numbers: HashSet<u64> = open.iter().map(|p| p.number).collect();
    let mut subjects: HashMap<u64, Vec<&str>> = HashMap::new();
    for (key, ns) in &links.prs {
        for n in ns.iter().filter(|n| numbers.contains(n)) {
            subjects.entry(*n).or_default().push(key);
        }
    }
    let mut b = Board::default();
    for pr in open {
        let e = queue::entry(pr, ledger, &credited);
        if e.mergeable == "CONFLICTING" {
            continue;
        }
        let approved = approvals(pr);
        let Some(&(_, first)) = approved.first() else {
            continue;
        };
        let mut keys = subjects.get(&pr.number).cloned().unwrap_or_default();
        keys.sort_unstable();
        let mut by_maintainer: Vec<(&str, String)> = Vec::new();
        let mut since: Option<DateTime<Utc>> = None;
        for key in keys {
            let ms = maintainers_of(index, key);
            for (login, at) in approved
                .iter()
                .filter(|(l, _)| ms.contains(&l.to_lowercase()))
            {
                let label = Subject::parse(key).map(|s| s.label()).unwrap_or_default();
                let said = (*login, label);
                if !by_maintainer.contains(&said) {
                    by_maintainer.push(said);
                }
                since = Some(since.map_or(*at, |s| s.min(*at)));
            }
        }
        let logins: Vec<&str> = approved.iter().map(|(l, _)| *l).collect();
        if let Some(at) = since {
            let said = |html: bool| {
                let mut out = by_maintainer
                    .iter()
                    .take(3)
                    .map(|(l, label)| {
                        if html {
                            format!("{} for {}", person_link(l), esc(label))
                        } else {
                            format!("{l} for {label}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                if by_maintainer.len() > 3 {
                    let _ = write!(out, " and {} more", by_maintainer.len() - 3);
                }
                format!("A maintainer approved the current head: {out}.")
            };
            b.maintainer_approved.push(Row {
                entry: e.clone(),
                waiting_since: at,
                why: said(false),
                why_html: said(true),
            });
        }
        if !ready(&e) {
            continue;
        }
        let approval = format!("checks pass, approved by {}", logins.join(", "));
        let approval_html = format!(
            "checks pass, approved by {}",
            logins
                .iter()
                .map(|l| person_link(l))
                .collect::<Vec<_>>()
                .join(", ")
        );
        if pr.labels.iter().any(|l| l == SECURITY) {
            b.security.push(Row {
                entry: e.clone(),
                waiting_since: first,
                why: format!("Labelled \"{SECURITY}\"; {approval}."),
                why_html: format!("Labelled \"{}\"; {approval_html}.", esc(SECURITY)),
            });
        }
        if pr.base.starts_with("release-") {
            b.backports.push(Row {
                entry: e.clone(),
                waiting_since: first,
                why: format!("A backport to {}; {approval}.", pr.base),
                why_html: format!("A backport to {}; {approval_html}.", esc(&pr.base)),
            });
        }
        b.waiting.push(Row {
            entry: e,
            waiting_since: first,
            why: format!("Ready: {approval}."),
            why_html: format!("Ready: {approval_html}."),
        });
    }
    for rows in [
        &mut b.maintainer_approved,
        &mut b.security,
        &mut b.backports,
        &mut b.waiting,
    ] {
        rows.sort_by_key(|r| (r.waiting_since, r.entry.number));
    }
    b
}

/// Why a ready pull request belongs to this committer, if it does. Reads the
/// private subscriptions, so it runs only for the viewer's own page.
fn yours(app: &crate::web::App, s: &Snapshot, login: &str) -> impl Fn(&Entry) -> Option<String> {
    let area = s.areas.get(login).cloned();
    let mut watched: HashMap<u64, String> = HashMap::new();
    let mut paths: Vec<(String, String)> = Vec::new();
    for sub in crate::catalog::subscriptions(app, login) {
        match &sub {
            Subject::Area(p) => paths.push((p.clone(), sub.label())),
            _ => {
                for n in s.links.prs.get(&sub.key()).into_iter().flatten() {
                    watched.entry(*n).or_insert_with(|| sub.label());
                }
            }
        }
    }
    move |e: &Entry| {
        if area.as_ref().is_some_and(|a| a.covers(&e.files)) {
            return Some("In your area.".to_string());
        }
        if let Some(l) = watched.get(&e.number) {
            return Some(format!("You watch {l}."));
        }
        paths
            .iter()
            .find(|(p, _)| e.files.iter().any(|f| crate::area::under(f, p)))
            .map(|(_, l)| format!("You watch {l}."))
    }
}

struct Line<'a> {
    entry: &'a Entry,
    /// HTML.
    why: String,
    since: Option<DateTime<Utc>>,
}

fn lines(list: &[Line], empty_text: &str, act: crate::actions::Act) -> String {
    if list.is_empty() {
        return empty(empty_text);
    }
    let mut out = String::new();
    for l in list {
        let e = l.entry;
        let mut meta = format!(
            "{}<br>#{} by <a href=\"/u/{a}\">{a}</a> · {} old · <span class=\"branch-name\">{}</span>",
            l.why,
            e.number,
            ago(e.created_at),
            esc(&e.base),
            a = esc(&e.author)
        );
        if let Some(t) = l.since {
            let _ = write!(meta, " · waiting {} since the approval", ago(t));
        }
        out.push_str(&item_row(
            Some(Kind::PR_OPEN),
            &crate::prpage::title_link(e.number, &esc(&e.title), "Link--primary text-bold"),
            "",
            &meta,
            &[],
            &format!(
                "<span class=\"ml-2\">{}</span>{}",
                ci_label(e),
                act.buttons(e.number)
            ),
        ));
    }
    out
}

fn rows_of(rows: &[Row]) -> Vec<Line<'_>> {
    rows.iter()
        .map(|r| Line {
            entry: &r.entry,
            why: r.why_html.clone(),
            since: Some(r.waiting_since),
        })
        .collect()
}

/// The Ready to merge tab of `/review`. Its lists are public. A committer
/// who signed in also gets "In your areas", which reads their private
/// subscriptions.
pub(crate) fn ready_tab(
    app: &crate::web::App,
    s: &Snapshot,
    viewer: Option<&str>,
    after: Option<u64>,
) -> String {
    let act = crate::actions::Act::of(app, viewer);
    let first = after.is_none();
    let mut out = format!(
        "<p class=\"f6\">Maintainer Mode never merges. Each row links to GitHub, and you decide. \
         Read <a href=\"{GUIDE}#how-to-merge-pull-requests-yourself\">how nixpkgs merges</a>. \
         <a href=\"/state?tab=rules\">The rules</a> of this site only decide which pull requests are in the queues.</p>"
    );
    match viewer.filter(|v| s.committers.is(v)) {
        Some(login) if first => {
            let list = in_your_areas(app, s, login);
            let more = (list.len() > PREVIEW).then_some(("/review?tab=ready&amp;section=yours", "see all"));
            out.push_str(&yours_section(&list[..list.len().min(PREVIEW)], list.len(), more, act));
        }
        None if first => out.push_str(&note(&format!(
            "A committer sees the section \"In your areas\" here. Maintainer Mode counts a login as a committer \
             when it merged a pull request in the last {MERGED_DAYS} days, or when it is in the team {TEAM}."
        ))),
        _ => {}
    }
    let all: Vec<&Entry> = s.queues.merge.iter().collect();
    let (page, next) = if first {
        let page = &all[..all.len().min(FIRST)];
        (page, (all.len() > FIRST).then(|| FIRST.to_string()))
    } else {
        crate::review::paged_by_offset(&all, after)
    };
    let rows = if page.is_empty() {
        empty("Nothing is ready yet.")
    } else {
        entry_rows(page, act)
    };
    out.push_str(&section(
        "Ready to merge",
        Some(all.len()),
        None,
        &format!(
            "<div class=\"Box-row f6 color-fg-muted\">Checks pass, there is no conflict, someone approved the current head, \
             and a review on it led to a change, a reply or a merge. Authors with more recent reviews of this kind come first.</div>{rows}"
        ),
    ));
    out.push_str(&pager("/review?tab=ready", next.as_deref()));
    if !first {
        return out;
    }
    for (id, title, sub, rows, empty) in board_lists(&s.board) {
        let href = format!("/review?tab=ready&amp;section={id}");
        let more = (rows.len() > PREVIEW).then_some((href.as_str(), "see all"));
        out.push_str(&section(
            title,
            Some(rows.len()),
            more,
            &format!(
                "<div class=\"Box-row f6 color-fg-muted\">{sub}</div>{}",
                lines(&rows_of(&rows[..rows.len().min(PREVIEW)]), empty, act)
            ),
        ));
    }
    out.push_str(&api_links());
    out
}

type List<'a> = (
    &'static str,
    &'static str,
    &'static str,
    &'a [Row],
    &'static str,
);

/// The four lists of the board: id, title, intro, rows and the text when it
/// is empty. The ids are the `section` values of the API.
fn board_lists(b: &Board) -> [List<'_>; 4] {
    [
        (
            "maintainer_approved",
            "Approved by a maintainer",
            "An approval on the current head by a login in the <code>meta.maintainers</code> or <code>meta.teams</code> of a package or module it changes. No known conflict. Longest wait first.",
            &b.maintainer_approved,
            "No pull request waits with a maintainer's approval.",
        ),
        (
            "security",
            "Security fixes",
            "Labelled <code>1.severity: security</code>, checks pass, no known conflict, and the current head is approved.",
            &b.security,
            "No security fix is ready.",
        ),
        (
            "backports",
            "Backports",
            "To a <code>release-*</code> branch, checks pass, no known conflict, and the current head is approved.",
            &b.backports,
            "No backport is ready.",
        ),
        (
            "waiting",
            "Waiting longest",
            "Checks pass, no known conflict, and the current head is approved. Oldest approval first.",
            &b.waiting,
            "Nothing waits.",
        ),
    ]
}

/// One list of the board on its own page, paged by offset. `None` when no
/// list has that id.
pub(crate) fn board_page(
    app: &crate::web::App,
    s: &Snapshot,
    viewer: Option<&str>,
    id: &str,
    after: Option<u64>,
) -> Option<String> {
    let act = crate::actions::Act::of(app, viewer);
    let back = "<p class=\"f6\"><a href=\"/review?tab=ready\">Back to Ready to merge</a></p>";
    if id == "yours" {
        let login = viewer.filter(|v| s.committers.is(v))?;
        let list = in_your_areas(app, s, login);
        let (page, next) = crate::review::paged_by_offset(&list, after);
        return Some(format!(
            "{back}{}{}",
            yours_section(page, list.len(), None, act),
            pager("/review?tab=ready&section=yours", next.as_deref())
        ));
    }
    let (id, title, sub, rows, empty_text) =
        board_lists(&s.board).into_iter().find(|l| l.0 == id)?;
    let (page, next) = crate::review::paged_by_offset(rows, after);
    Some(format!(
        "{back}{}{}",
        section(
            title,
            Some(rows.len()),
            None,
            &format!(
                "<div class=\"Box-row f6 color-fg-muted\">{sub}</div>{}",
                lines(&rows_of(page), empty_text, act)
            ),
        ),
        pager(&format!("/review?tab=ready&section={id}"), next.as_deref())
    ))
}

/// The `section` values of `/review?tab=ready`: the board, and "yours"
/// for a committer.
#[cfg(test)]
pub(crate) fn sections() -> Vec<&'static str> {
    let mut out = vec!["yours"];
    out.extend(board_lists(&Board::default()).map(|l| l.0));
    out
}

/// The ready pull requests in a committer's areas, with why. Private:
/// subscriptions count.
pub(crate) fn yours_list<'a>(
    app: &crate::web::App,
    s: &'a Snapshot,
    login: &str,
) -> Vec<(&'a Entry, String)> {
    let mine = yours(app, s, login);
    s.queues
        .merge
        .iter()
        .filter(|e| e.author != login)
        .filter_map(|e| mine(e).map(|why| (e, why)))
        .collect()
}

fn in_your_areas<'a>(app: &crate::web::App, s: &'a Snapshot, login: &str) -> Vec<Line<'a>> {
    yours_list(app, s, login)
        .into_iter()
        .map(|(entry, why)| Line {
            why: esc(&why),
            entry,
            since: None,
        })
        .collect()
}

fn yours_section(
    page: &[Line],
    total: usize,
    more: Option<(&str, &str)>,
    act: crate::actions::Act,
) -> String {
    section(
        "In your areas",
        Some(total),
        more,
        &format!(
            "<div class=\"Box-row f6 color-fg-muted\">Ready pull requests that touch your areas or what you watch. \
             Only you see this section.</div>{}",
            lines(
                page,
                "Nothing ready touches your areas or what you watch.",
                act
            )
        ),
    )
}

fn api_links() -> String {
    let links: Vec<String> = crate::api::SECTIONS
        .iter()
        .filter(|s| **s != "yours")
        .map(|s| {
            format!("<a href=\"/api/v1/prs?queue=ready&amp;section={s}\"><code>{s}</code></a>")
        })
        .collect();
    format!(
        "<p class=\"f6\">The lists without the personal part are in the JSON API, one section each: {}.</p>",
        links.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::area::Package;
    use crate::ledger::tests::{pr, review, t};

    fn approve(p: &mut Pr, id: u64, who: &str, day: i64) {
        let mut r = review(id, who, day);
        r.state = "APPROVED".into();
        p.reviews.push(r);
    }

    #[test]
    fn the_board_says_why_each_pull_request_is_there() {
        let mut index = Index::default();
        index.pkgs.insert(
            "hello".into(),
            Package {
                pname: "hello".into(),
                file: Some("pkgs/by-name/he/hello/package.nix".into()),
                maintainers: vec!["Mai".into()],
                ..Default::default()
            },
        );
        let file = vec!["pkgs/by-name/he/hello/package.nix".to_string()];
        let mut by_maintainer = pr(1, "alice", 0);
        by_maintainer.files = file.clone();
        approve(&mut by_maintainer, 1, "mai", 2);
        let mut security = pr(2, "bob", 0);
        security.labels = vec![SECURITY.into()];
        approve(&mut security, 2, "carol", 1);
        let mut backport = pr(3, "bob", 0);
        backport.base = "release-26.05".into();
        approve(&mut backport, 3, "carol", 3);
        let mut conflict = pr(4, "bob", 0);
        conflict.files = file;
        conflict.mergeable = "CONFLICTING".into();
        approve(&mut conflict, 4, "mai", 1);
        let mut stale = pr(5, "bob", 0);
        approve(&mut stale, 5, "carol", 1);
        stale.reviews[0].commit_oid = Some("old".into());
        let prs = [by_maintainer, security, backport, conflict, stale];
        let links = crate::links::build(&index, &prs, &[]);
        let ledger = crate::ledger::compute(&prs, |_| false, t(5));
        let b = board(&prs, &ledger, &index, &links);
        let nums = |rows: &[Row]| rows.iter().map(|r| r.entry.number).collect::<Vec<_>>();
        assert_eq!(nums(&b.maintainer_approved), [1]);
        assert_eq!(
            b.maintainer_approved[0].why,
            "A maintainer approved the current head: mai for hello."
        );
        assert!(
            b.maintainer_approved[0]
                .why_html
                .contains("<a href=\"/u/mai\">mai</a> for hello")
        );
        assert_eq!(nums(&b.security), [2]);
        assert_eq!(nums(&b.backports), [3]);
        assert_eq!(nums(&b.waiting), [2, 1, 3]);
    }

    #[test]
    fn a_recent_merge_or_the_team_gives_merge_rights() {
        let mut old = pr(1, "alice", 0);
        old.merged_at = Some(t(0));
        old.merged_by = Some("Olda".into());
        let mut new = pr(2, "alice", 300);
        new.merged_at = Some(t(300));
        new.merged_by = Some("Newb".into());
        let c = Committers::build(&[old, new], &[], &["Teamy".into()], None, t(400));
        assert!(c.is("newb"));
        assert!(c.is("TEAMY"));
        assert!(!c.is("olda"));
        assert!(!c.is("alice"));
        assert_eq!((c.from_merges, c.from_team, c.len()), (1, 1, 2));
    }

    #[test]
    fn a_merge_in_the_history_walk_gives_merge_rights() {
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let mut walked = pr(1, "alice", 200);
        walked.merged_at = Some(t(200));
        walked.merged_by = Some("Histo".into());
        let mut gone = pr(2, "alice", 0);
        gone.merged_at = Some(t(0));
        gone.merged_by = Some("Olda".into());
        let open = pr(3, "alice", 0);
        let facts: Vec<crate::metrics::Facts> = [&walked, &gone, &open].map(Into::into).into();
        store.put_facts(&facts).unwrap();
        let c = Committers::load(&store, &[], t(400));
        assert!(c.is("histo"));
        assert!(!c.is("olda"));
        assert_eq!(c.from_merges, 1);
    }

    #[test]
    fn the_ready_tab_shows_a_preview_and_each_list_pages_on_its_own() {
        let app = crate::sets::tests::app(&[]);
        let mut s = Snapshot::default();
        for n in 1..=60 {
            let e = queue::entry(&pr(n, "alice", 0), &s.ledger, &Default::default());
            s.queues.merge.push(e.clone());
            s.board.security.push(Row {
                entry: e,
                waiting_since: t(0),
                why: String::new(),
                why_html: String::new(),
            });
        }
        let count = |html: &str| html.matches("class=\"Box-row").count();
        let tab = ready_tab(&app, &s, None, None);
        let main = tab.split("Ready to merge").nth(1).unwrap();
        assert_eq!(
            count(main.split("Approved by a maintainer").next().unwrap()),
            1 + FIRST
        );
        let security = tab.split("Security fixes").nth(1).unwrap();
        let security = security.split("Backports").next().unwrap();
        assert_eq!(count(security), 1 + PREVIEW);
        assert!(tab.contains("href=\"/review?tab=ready&amp;section=security\">see all</a>"));
        assert!(tab.contains("/review?tab=ready&amp;after=20\">Next page"));
        let page = board_page(&app, &s, None, "security", None).unwrap();
        assert_eq!(count(&page), 1 + crate::review::PAGE);
        assert!(page.contains("/review?tab=ready&amp;section=security&amp;after=50\">Next page"));
        let last = board_page(&app, &s, None, "security", Some(50)).unwrap();
        assert_eq!(count(&last), 1 + 10);
        assert!(!last.contains("Next page"));
        assert!(board_page(&app, &s, None, "yours", None).is_none());
        assert!(board_page(&app, &s, None, "nonsense", None).is_none());
    }
}
