//! Who has merge rights, from public data, and the view for them.

use anyhow::{Result, bail};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};

use crate::area::Index;
use crate::catalog::Subject;
use crate::html::{Html, html};
use crate::ledger::Ledger;
use crate::links::Links;
use crate::model::Pr;
use crate::queue::{self, Entry};
use crate::store::Store;
use crate::ui::{Kind, empty, item_row, pager, section};
use crate::web::{Snapshot, ago, ci_label, entry_rows, person_link};

pub const TEAM: &str = "NixOS/nixpkgs-committers";
const MERGED_DAYS: i64 = 365;
pub const SECURITY: &str = "1.severity: security";
/// Rows of the list on the first page of the tab; later pages hold `review::PAGE`.
const FIRST: usize = 20;

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
    #[serde(skip)]
    pub reason: Reason,
}

/// Why a pull request is on the board: [`Reason::text`] for the API,
/// [`Reason::html`] for the page. Each list holds the approvers.
#[derive(Debug, Clone)]
pub enum Reason {
    /// (login, subject label) of each maintainer who approved.
    Maintainer(Vec<(String, String)>),
    Security(Vec<String>),
    Backport(Vec<String>),
    Ready(Vec<String>),
}

impl Reason {
    fn say(&self, base: &str, person: fn(&str) -> Html) -> Html {
        let approval = |logins: &[String]| {
            html!(
                "checks pass, approved by {}",
                crate::html::join(logins.iter().map(|l| person(l)), ", ")
            )
        };
        match self {
            Reason::Maintainer(by) => {
                let mut out = crate::html::join(
                    by.iter()
                        .take(3)
                        .map(|(l, label)| html!("{} for {label}", person(l))),
                    ", ",
                );
                if by.len() > 3 {
                    out.push(&html!(" and {} more", by.len() - 3));
                }
                html!("A maintainer approved the current head: {out}.")
            }
            Reason::Security(l) => html!("Labelled \"{SECURITY}\"; {}.", approval(l)),
            Reason::Backport(l) => html!("A backport to {base}; {}.", approval(l)),
            Reason::Ready(l) => html!("Ready: {}.", approval(l)),
        }
    }

    pub fn text(&self, base: &str) -> String {
        crate::html::to_text(&self.say(base, Html::text))
    }

    pub(crate) fn html(&self, base: &str) -> Html {
        self.say(base, person_link)
    }
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

/// Approvals on the current head by someone other than the author, oldest
/// first, one per login.
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
    let mut seen = HashSet::new();
    out.retain(|(l, _)| seen.insert(l.to_lowercase()));
    out
}

pub(crate) fn maintainers_of(index: &Index, key: &str) -> Vec<String> {
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
        let logins: Vec<String> = approved.iter().map(|(l, _)| l.to_string()).collect();
        let row = |reason: Reason, since: DateTime<Utc>| Row {
            entry: e.clone(),
            waiting_since: since,
            why: reason.text(&pr.base),
            reason,
        };
        if let Some(at) = since {
            let by = by_maintainer
                .iter()
                .map(|(l, label)| (l.to_string(), label.clone()))
                .collect();
            b.maintainer_approved.push(row(Reason::Maintainer(by), at));
        }
        if !ready(&e) {
            continue;
        }
        if pr.labels.iter().any(|l| l == SECURITY) {
            b.security
                .push(row(Reason::Security(logins.clone()), first));
        }
        if pr.base.starts_with("release-") {
            b.backports
                .push(row(Reason::Backport(logins.clone()), first));
        }
        b.waiting.push(row(Reason::Ready(logins), first));
    }
    for rows in [
        &mut b.maintainer_approved,
        &mut b.security,
        &mut b.backports,
        &mut b.waiting,
    ] {
        rows.sort_by_key(|r| (r.entry.treewide.is_some(), r.waiting_since, r.entry.number));
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
    why: Html,
    since: Option<DateTime<Utc>>,
}

/// The filter bar reads the entry of a row.
impl crate::review::Filtered for Line<'_> {
    fn facts(&self) -> crate::review::Facts<'_> {
        self.entry.facts()
    }
}

impl crate::review::Filtered for Row {
    fn facts(&self) -> crate::review::Facts<'_> {
        self.entry.facts()
    }
}

fn lines(list: &[Line], empty_text: &'static str) -> Html {
    if list.is_empty() {
        return empty(empty_text);
    }
    let mut out = Html::new();
    for l in list {
        let e = l.entry;
        let mut meta = html!(
            "{}<br>#{} by <a href=\"/u/{a}\">{a}</a> · {} old · <span class=\"branch-name\">{}</span>",
            l.why,
            e.number,
            ago(e.created_at),
            e.base,
            a = e.author
        );
        if let Some(t) = l.since {
            meta.push(&html!(" · waiting {} since the approval", ago(t)));
        }
        let badges: Vec<Html> = e
            .treewide
            .as_ref()
            .map(crate::web::treewide_badge)
            .into_iter()
            .collect();
        out.push(&item_row(
            Some(Kind::PR_OPEN),
            crate::prpage::title_link(e.number, Html::text(&e.title), "Link--primary text-bold"),
            "",
            meta,
            &badges,
            html!("<span class=\"ml-2\">{}</span>", ci_label(e)),
        ));
    }
    out
}

fn rows_of<'a>(rows: &[&'a Row]) -> Vec<Line<'a>> {
    rows.iter()
        .map(|r| Line {
            entry: &r.entry,
            why: r.reason.html(&r.entry.base),
            since: Some(r.waiting_since),
        })
        .collect()
}

/// The first page of the landing list holds [`FIRST`] rows; `after`
/// pages the rest by [`crate::review::PAGE`].
fn page_first<T>(items: &[T], after: Option<u64>) -> (&[T], Option<String>) {
    match after {
        None => (
            &items[..items.len().min(FIRST)],
            (items.len() > FIRST).then(|| FIRST.to_string()),
        ),
        Some(_) => crate::review::paged_by_offset(items, after),
    }
}

/// The chips above the ready lists: the queue, "In your areas" for a
/// committer, and the lists of the board that have rows. `q` keeps the
/// set filters.
fn sections_nav(s: &Snapshot, yours: Option<usize>, current: &str, q: &str) -> Html {
    use crate::ui::Tab;
    let mut links = Vec::new();
    if let Some(n) = yours {
        links.push(
            Tab::new(
                format!("/review?tab=ready{q}"),
                "In your areas",
                current == "yours",
            )
            .count(n),
        );
    }
    let ready_href = if yours.is_some() {
        format!("/review?tab=ready&section=ready{q}")
    } else {
        format!("/review?tab=ready{q}")
    };
    links.push(
        Tab::new(ready_href, "Ready to merge", current == "ready").count(s.queues.merge.len()),
    );
    for (id, title, rows, _) in board_lists(&s.board) {
        if rows.is_empty() && current != id {
            continue;
        }
        links.push(
            Tab::new(
                format!("/review?tab=ready&section={id}{q}"),
                title,
                current == id,
            )
            .count(rows.len()),
        );
    }
    crate::ui::chips("Lists", "Lists:".into(), &links)
}

/// The text of an empty list: the filters ate the rows, or the list is
/// empty on its own.
fn empty_text(o: &crate::review::Opts, otherwise: &'static str) -> &'static str {
    if o.active() {
        "Nothing matches these filters."
    } else {
        otherwise
    }
}

/// The Ready to merge tab of `/review`: the chips of its lists, the
/// filter bar, then one list. The list is "In your areas" for a
/// committer who signed in and has ready rows there, else the queue.
pub(crate) fn ready_tab(
    app: &crate::web::App,
    s: &Snapshot,
    viewer: Option<&str>,
    o: &crate::review::Opts,
    after: Option<u64>,
) -> Html {
    let committer = viewer.filter(|v| s.committers.is(v));
    let yours = committer
        .map(|l| in_your_areas(app, s, l))
        .unwrap_or_default();
    let q = o.query();
    let mut out = Html::new();
    if after.is_none() {
        let lead = match committer {
            None => Html::from(
                " A committer sees the section \"In your areas\" here. \
                 <a href=\"/state?tab=docs&amp;doc=personal#for-committers\">Who is a committer</a>",
            ),
            Some(_) if yours.is_empty() => {
                Html::from(" Nothing ready touches your areas or what you watch.")
            }
            Some(_) => Html::new(),
        };
        out.push(&html!(
            "<p class=\"f6\">Maintainer Mode never merges: each row links to GitHub, and you decide.{lead}</p>"
        ));
    }
    if yours.is_empty() {
        let all: Vec<&Entry> = s.queues.merge.iter().collect();
        out.push(&sections_nav(s, committer.map(|_| 0), "ready", &q));
        out.push(&crate::review::ready_bar(
            o,
            None,
            &crate::review::bases(&all),
            &crate::review::label_counts(&all),
        ));
        let list = o.apply(all);
        let (page, next) = page_first(&list, after);
        out.push(&queue_section(
            page,
            list.len(),
            empty_text(o, "Nothing is ready yet."),
        ));
        out.push(&pager(&format!("/review?tab=ready{q}"), next.as_deref()));
    } else {
        out.push(&sections_nav(s, Some(yours.len()), "yours", &q));
        out.push(&crate::review::ready_bar(
            o,
            None,
            &crate::review::bases(&yours),
            &crate::review::label_counts(&yours),
        ));
        let list = o.apply(yours);
        let (page, next) = page_first(&list, after);
        out.push(&yours_section(
            page,
            list.len(),
            empty_text(o, "Nothing ready touches your areas or what you watch."),
        ));
        out.push(&pager(
            &format!("/review?tab=ready&section=yours{q}"),
            next.as_deref(),
        ));
    }
    out
}

/// A list with its title, its count and one sentence above the rows.
fn list_section(title: &'static str, total: usize, intro: &'static str, rows: Html) -> Html {
    section(
        title,
        Some(total),
        None,
        html!(
            "<div class=\"Box-row f6 color-fg-muted\">{}</div>{rows}",
            Html::from(intro)
        ),
    )
}

fn queue_section(page: &[&Entry], total: usize, none: &'static str) -> Html {
    let rows = if page.is_empty() {
        empty(none)
    } else {
        entry_rows(page)
    };
    list_section(
        "Ready to merge",
        total,
        "Checks pass, no conflict, an approval on the current head, and a \
         <a href=\"/state?tab=rules#3-ready-to-merge\">counted review</a>.",
        rows,
    )
}

type List<'a> = (&'static str, &'static str, &'a [Row], &'static str);

/// The four lists of the board: id, title, rows and the text when it is
/// empty. The ids are the `section` values of the API.
fn board_lists(b: &Board) -> [List<'_>; 4] {
    [
        (
            "maintainer_approved",
            "Approved by a maintainer",
            &b.maintainer_approved,
            "No pull request waits with a maintainer's approval.",
        ),
        (
            "security",
            "Security fixes",
            &b.security,
            "No security fix is ready.",
        ),
        (
            "backports",
            "Backports",
            &b.backports,
            "No backport is ready.",
        ),
        ("waiting", "Waiting longest", &b.waiting, "Nothing waits."),
    ]
}

/// Each row of the board says why it is there. This sentence says the rest.
const BOARD_INTRO: &str = "There is no known conflict. The longest wait comes first.";

/// One list of the tab on its own page, paged by offset. `None` when no
/// list has that id.
pub(crate) fn board_page(
    app: &crate::web::App,
    s: &Snapshot,
    viewer: Option<&str>,
    id: &str,
    o: &crate::review::Opts,
    after: Option<u64>,
) -> Option<Html> {
    let committer = viewer.filter(|v| s.committers.is(v));
    let q = o.query();
    let here = format!("/review?tab=ready&section={id}{q}");
    let yours_len = committer.map(|l| yours_list(app, s, l).len());
    let bar = |list_bases: &[String], list_labels: &[(String, usize)]| {
        html!(
            "{}{}",
            sections_nav(s, yours_len, id, &q),
            crate::review::ready_bar(o, Some(id), list_bases, list_labels)
        )
    };
    let (head, list, next) = match id {
        "ready" => {
            let all: Vec<&Entry> = s.queues.merge.iter().collect();
            let head = bar(
                &crate::review::bases(&all),
                &crate::review::label_counts(&all),
            );
            let rows = o.apply(all);
            let (page, next) = crate::review::paged_by_offset(&rows, after);
            (
                head,
                queue_section(page, rows.len(), empty_text(o, "Nothing is ready yet.")),
                next,
            )
        }
        "yours" => {
            let login = committer?;
            let list = in_your_areas(app, s, login);
            let head = bar(
                &crate::review::bases(&list),
                &crate::review::label_counts(&list),
            );
            let rows = o.apply(list);
            let (page, next) = crate::review::paged_by_offset(&rows, after);
            (
                head,
                yours_section(
                    page,
                    rows.len(),
                    empty_text(o, "Nothing ready touches your areas or what you watch."),
                ),
                next,
            )
        }
        _ => {
            let (_, title, rows, none) = board_lists(&s.board).into_iter().find(|l| l.0 == id)?;
            let all: Vec<&Row> = rows.iter().collect();
            let head = bar(
                &crate::review::bases(&all),
                &crate::review::label_counts(&all),
            );
            let rows = o.apply(all);
            let (page, next) = crate::review::paged_by_offset(&rows, after);
            (
                head,
                list_section(
                    title,
                    rows.len(),
                    BOARD_INTRO,
                    lines(&rows_of(page), empty_text(o, none)),
                ),
                next,
            )
        }
    };
    Some(html!("{head}{list}{}", pager(&here, next.as_deref())))
}

/// The ready pull requests in a committer's areas, with why. Private:
/// subscriptions count.
pub(crate) fn yours_list<'a>(
    app: &crate::web::App,
    s: &'a Snapshot,
    login: &str,
) -> Vec<(&'a Entry, String)> {
    let mine = yours(app, s, login);
    let mut out: Vec<(&Entry, String)> = s
        .queues
        .merge
        .iter()
        .filter(|e| e.author != login)
        .filter_map(|e| mine(e).map(|why| (e, why)))
        .collect();
    out.sort_by_key(|(e, _)| e.treewide.is_some());
    out
}

fn in_your_areas<'a>(app: &crate::web::App, s: &'a Snapshot, login: &str) -> Vec<Line<'a>> {
    yours_list(app, s, login)
        .into_iter()
        .map(|(entry, why)| Line {
            why: Html::text(&why),
            entry,
            since: None,
        })
        .collect()
}

fn yours_section(page: &[Line], total: usize, none: &'static str) -> Html {
    list_section(
        "In your areas",
        total,
        "Only you see this section: ready pull requests in your areas and in what you watch.",
        lines(page, none),
    )
}

#[cfg(test)]
pub(crate) mod tests {
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
                .reason
                .html(&b.maintainer_approved[0].entry.base)
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
    fn the_ready_tab_shows_one_list_and_links_to_the_others() {
        let app = crate::sets::tests::app(&[]);
        let mut s = Snapshot::default();
        for n in 1..=60 {
            let e = queue::entry(&pr(n, "alice", 0), &s.ledger, &Default::default());
            s.queues.merge.push(e.clone());
            s.board.security.push(Row {
                entry: e,
                waiting_since: t(0),
                why: String::new(),
                reason: Reason::Ready(vec![]),
            });
        }
        let count = |html: &str| html.matches("class=\"Box-row").count();
        let tab = ready_tab(&app, &s, None, &Default::default(), None);
        assert_eq!(tab.matches("<section").count(), 1);
        assert_eq!(count(&tab), 1 + FIRST);
        assert!(tab.contains("A committer sees the section"));
        assert!(tab.contains(
            "href=\"/review?tab=ready&amp;section=security\">Security fixes <span class=\"Counter\">60</span>"
        ));
        assert!(!tab.contains("Approved by a maintainer") && !tab.contains("section=ready\""));
        assert!(tab.contains("/review?tab=ready&amp;after=20\">Next page"));
        let next = ready_tab(&app, &s, None, &Default::default(), Some(20));
        assert_eq!(count(&next), 1 + 40);
        assert!(!next.contains("Other lists") && !next.contains("never merges"));
        let page = board_page(&app, &s, None, "security", &Default::default(), None).unwrap();
        assert_eq!(count(&page), 1 + crate::review::PAGE);
        assert!(page.contains("/review?tab=ready&amp;section=security&amp;after=50\">Next page"));
        let last = board_page(&app, &s, None, "security", &Default::default(), Some(50)).unwrap();
        assert_eq!(count(&last), 1 + 10);
        assert!(!last.contains("Next page"));
        let queue = board_page(&app, &s, None, "ready", &Default::default(), Some(50)).unwrap();
        assert!(queue.contains("Ready to merge <span class=\"Counter\">60</span>"));
        assert_eq!(count(&queue), 1 + 10);
        let narrowed = crate::review::Opts {
            q: Some("pr 7".into()),
            ..Default::default()
        };
        let page = board_page(&app, &s, None, "security", &narrowed, None).unwrap();
        assert_eq!(count(&page), 1 + 1, "the filters narrow the board lists");
        assert!(page.contains("Security fixes <span class=\"Counter\">1</span>"));
        assert!(
            page.contains("href=\"/review?tab=ready&amp;q=pr%207\"")
                && page.contains("href=\"/review?tab=ready&amp;section=security&amp;q=pr%207\""),
            "the chips keep the filters"
        );
        assert!(page.contains("Clear"));
        let none = board_page(
            &app,
            &s,
            None,
            "security",
            &crate::review::Opts {
                q: Some("nonsense".into()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(none.contains("Nothing matches these filters."));
        let sorted = crate::review::Opts {
            sort: Some("newest".into()),
            ..Default::default()
        };
        let tab = ready_tab(&app, &s, None, &sorted, None);
        assert!(tab.contains("/review?tab=ready&amp;sort=newest&amp;after=20\">Next page"));
        assert!(board_page(&app, &s, None, "yours", &Default::default(), None).is_none());
        assert!(board_page(&app, &s, None, "nonsense", &Default::default(), None).is_none());
    }

    #[test]
    fn a_committer_sees_the_list_of_their_areas_first() {
        let app = crate::sets::tests::app(&[]);
        let mut s = Snapshot {
            committers: Committers::build(&[], &[], &["carol".into()], None, t(0)),
            ..Default::default()
        };
        s.areas.insert(
            "carol".into(),
            crate::area::Area {
                maintains: vec!["pkgs/by-name/he".into()],
                ..Default::default()
            },
        );
        for n in 1..=30 {
            let mut p = pr(n, "alice", 0);
            if n % 10 == 0 {
                p.files = vec!["pkgs/by-name/he/hello/package.nix".into()];
            }
            s.queues
                .merge
                .push(queue::entry(&p, &s.ledger, &Default::default()));
        }
        let tab = ready_tab(&app, &s, Some("carol"), &Default::default(), None);
        assert_eq!(tab.matches("<section").count(), 1);
        assert!(tab.contains("In your areas <span class=\"Counter\">3</span>"));
        assert!(tab.contains(
            "href=\"/review?tab=ready&amp;section=ready\">Ready to merge <span class=\"Counter\">30</span>"
        ));
        assert!(!tab.contains("A committer sees the section") && !tab.contains("Next page"));
        let page = board_page(&app, &s, Some("carol"), "yours", &Default::default(), None).unwrap();
        assert!(page.contains("In your areas <span class=\"Counter\">3</span>"));
        s.areas.clear();
        let tab = ready_tab(&app, &s, Some("carol"), &Default::default(), None);
        assert!(tab.contains("Nothing ready touches your areas"));
        assert!(tab.contains("Ready to merge <span class=\"Counter\">30</span></h2>"));
        assert!(!tab.contains("Other lists"));
    }

    /// The `section` values of `/review?tab=ready`.
    pub(crate) fn sections() -> Vec<&'static str> {
        let mut out = vec!["ready", "yours"];
        out.extend(board_lists(&Board::default()).map(|l| l.0));
        out
    }
}
