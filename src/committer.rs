//! Who has merge rights, from public data, and the view for them.

use anyhow::{Result, bail};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use crate::area::Index;
use crate::catalog::Subject;
use crate::ledger::Ledger;
use crate::links::Links;
use crate::model::Pr;
use crate::queue::{self, Entry};
use crate::store::Store;
use crate::web::{Shared, Snapshot, ago, ci_label, esc, icon, octicon, page};

pub const TEAM: &str = "NixOS/nixpkgs-committers";
const MERGED_DAYS: i64 = 365;
pub const SECURITY: &str = "1.severity: security";
const SECTION: usize = 50;
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
        let mut by_maintainer: Vec<String> = Vec::new();
        let mut since: Option<DateTime<Utc>> = None;
        for key in keys {
            let ms = maintainers_of(index, key);
            for (login, at) in approved
                .iter()
                .filter(|(l, _)| ms.contains(&l.to_lowercase()))
            {
                let label = Subject::parse(key).map(|s| s.label()).unwrap_or_default();
                let said = format!("{login} for {label}");
                if !by_maintainer.contains(&said) {
                    by_maintainer.push(said);
                }
                since = Some(since.map_or(*at, |s| s.min(*at)));
            }
        }
        let logins: Vec<&str> = approved.iter().map(|(l, _)| *l).collect();
        if let Some(at) = since {
            let mut why = by_maintainer
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            if by_maintainer.len() > 3 {
                let _ = write!(why, " and {} more", by_maintainer.len() - 3);
            }
            b.maintainer_approved.push(Row {
                entry: e.clone(),
                waiting_since: at,
                why: format!("A maintainer approved the current head: {why}."),
            });
        }
        if !ready(&e) {
            continue;
        }
        let approval = format!("checks pass, approved by {}", logins.join(", "));
        if pr.labels.iter().any(|l| l == SECURITY) {
            b.security.push(Row {
                entry: e.clone(),
                waiting_since: first,
                why: format!("Labelled \"{SECURITY}\"; {approval}."),
            });
        }
        if pr.base.starts_with("release-") {
            b.backports.push(Row {
                entry: e.clone(),
                waiting_since: first,
                why: format!("A backport to {}; {approval}.", pr.base),
            });
        }
        b.waiting.push(Row {
            entry: e,
            waiting_since: first,
            why: format!("Ready: {approval}."),
        });
    }
    for rows in [
        &mut b.maintainer_approved,
        &mut b.security,
        &mut b.backports,
        &mut b.waiting,
    ] {
        rows.sort_by_key(|r| (r.waiting_since, r.entry.number));
        rows.truncate(SECTION);
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
    why: String,
    since: Option<DateTime<Utc>>,
}

fn lines(list: &[Line], empty: &str, act: crate::actions::Act) -> String {
    if list.is_empty() {
        return format!("<div class=\"blankslate\"><p>{empty}</p></div>");
    }
    let mut out = String::from("<div class=\"Box mb-4\">");
    for l in list {
        let e = l.entry;
        let mut meta = format!(
            "#{} by <a href=\"/u/{a}\">{a}</a> · {} old · <span class=\"branch-name\">{}</span>",
            e.number,
            ago(e.created_at),
            esc(&e.base),
            a = esc(&e.author)
        );
        if let Some(t) = l.since {
            let _ = write!(meta, " · waiting {} since the approval", ago(t));
        }
        let _ = write!(
            out,
            "<div class=\"Box-row d-flex flex-items-start\">\
               <span class=\"mr-2 color-fg-open\">{pr}</span>\
               <div class=\"flex-auto\">\
                 {title}\
                 <div class=\"f6 mt-1\">{why}</div>\
                 <div class=\"f6 color-fg-muted mt-1\">{meta}</div>\
               </div>\
               <span class=\"ml-2\">{ci}</span>{act}\
             </div>",
            pr = icon(octicon!("git-pull-request")),
            title = crate::prpage::title_link(e.number, &esc(&e.title), "Link--primary text-bold"),
            why = esc(&l.why),
            ci = ci_label(e),
            act = act.buttons(e.number),
        );
    }
    out.push_str("</div>");
    out
}

fn rows_of(rows: &[Row]) -> Vec<Line<'_>> {
    rows.iter()
        .map(|r| Line {
            entry: &r.entry,
            why: r.why.clone(),
            since: Some(r.waiting_since),
        })
        .collect()
}

fn guide() -> String {
    format!(
        "<p class=\"f6\">Maintainer Mode never merges: each row links to GitHub, and you decide. \
         nixpkgs says how to review and merge in its <a href=\"{GUIDE}\">contributor guide</a>; \
         <a href=\"/rules\">the rules</a> of this site only decide which pull requests get listed.</p>"
    )
}

async fn committer_page(State(app): State<Shared>, headers: HeaderMap) -> Html<String> {
    let viewer = app.viewer(&headers);
    let s = app.snap();
    let mut out = crate::catalog::head(
        &format!("{} For committers", icon(octicon!("key"))),
        "What waits for someone with merge rights: the ready queue in your areas first, \
         pull requests a maintainer approved, security fixes, backports, and what waits longest.",
        "<a class=\"btn btn-sm\" href=\"/api/v1/committer\">JSON</a>",
    );
    let Some(login) = viewer.as_deref().filter(|v| s.committers.is(v)) else {
        let _ = write!(
            out,
            "<div class=\"flash mb-3\">This view is for people with merge rights in nixpkgs. \
             Maintainer Mode counts a login as a committer when it merged a pull request in the last {MERGED_DAYS} days, \
             or when it is in the team {TEAM}. {}</div>{}\
             <p class=\"f6\">The lists without the personal part are in <a href=\"/api/v1/committer\">the JSON API</a>, \
             and the <a href=\"/merge\">ready-to-merge queue</a> is open to all.</p>",
            if viewer.is_some() {
                "Your login has not merged a pull request in the synced data."
            } else {
                "Sign in to see it."
            },
            guide()
        );
        return page(&app, viewer.as_deref(), "committer", &out);
    };
    out.push_str(&guide());
    let act = crate::actions::Act::of(&app, Some(login));
    let mine = yours(&app, &s, login);
    let mut first: Vec<Line> = Vec::new();
    let mut rest: Vec<Line> = Vec::new();
    for e in s.queues.merge.iter().filter(|e| e.author != login) {
        match mine(e) {
            Some(why) => first.push(Line {
                entry: e,
                why,
                since: None,
            }),
            None => rest.push(Line {
                entry: e,
                why: "Ready to merge, outside your areas and subscriptions.".into(),
                since: None,
            }),
        }
    }
    let ready_n = first.len() + rest.len();
    first.extend(rest);
    first.truncate(SECTION * 2);
    let b = &s.board;
    for (title, sub, list, empty) in [
        (
            format!("Ready to merge ({ready_n})"),
            "The <a href=\"/merge\">ready-to-merge queue</a>: what touches your areas and subscriptions comes first, then the rest.",
            first,
            "Nothing is ready yet.",
        ),
        (
            "Approved by a maintainer".to_string(),
            "An approval on the current head by a login in the <code>meta.maintainers</code> or <code>meta.teams</code> of a package or module it changes. No known conflict. Longest wait first.",
            rows_of(&b.maintainer_approved),
            "No pull request waits with a maintainer's approval.",
        ),
        (
            "Security fixes".to_string(),
            "Labelled <code>1.severity: security</code>, checks pass, no known conflict, and the current head is approved.",
            rows_of(&b.security),
            "No security fix is ready.",
        ),
        (
            "Backports".to_string(),
            "To a <code>release-*</code> branch, checks pass, no known conflict, and the current head is approved.",
            rows_of(&b.backports),
            "No backport is ready.",
        ),
        (
            "Waiting longest".to_string(),
            "Checks pass, no known conflict, and the current head is approved. Oldest approval first.",
            rows_of(&b.waiting),
            "Nothing waits.",
        ),
    ] {
        let _ = write!(
            out,
            "<h2 class=\"f4 mt-4 mb-1\">{title}</h2><p class=\"f6 color-fg-muted mb-2\">{sub}</p>{}",
            lines(&list, empty, act)
        );
    }
    page(&app, viewer.as_deref(), "committer", &out)
}

/// Public data only, the same for every caller: the personal order reads
/// private subscriptions, so it stays on the page.
async fn api_committer(State(app): State<Shared>) -> Json<Value> {
    let s = app.snap();
    Json(json!({
        "ready": s.queues.merge,
        "maintainer_approved": s.board.maintainer_approved,
        "security": s.board.security,
        "backports": s.board.backports,
        "waiting": s.board.waiting,
        "committers": {
            "logins": s.committers.len(),
            "merged_last_365_days": s.committers.from_merges,
            "team": TEAM,
            "team_members": s.committers.from_team,
        },
    }))
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/committer", get(committer_page))
        .route("/api/v1/committer", get(api_committer))
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
}
