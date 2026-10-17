//! Needs you: the open things that wait for one person, one action each,
//! the most urgent first. The inbox says what happened; this says what is
//! still open. Only the owner sees it.

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use crate::area::Index;
use crate::catalog::Subject;
use crate::health::{Health, Hydra, Last};
use crate::mine::{Interests, Why};
use crate::model::Pr;
use crate::severity::Band;
use crate::ui::{Tone, badge};
use crate::web::{App, Shared, Snapshot, ago, esc, icon, octicon};

const SHOWN: usize = 20;
const MOOD_COOKIE: &str = "mood";
const APPROVED_WAIT: i64 = 3;
const REVIEW_WAIT: i64 = 7;
const EPSS_HIGH: f64 = 0.1;
const PINGS: usize = 5;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/needs/next", get(next))
        .route("/needs/snooze", post(snooze))
        .route("/api/v1/needs", get(api))
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub(crate) enum Mood {
    #[default]
    All,
    Mine,
    Machines,
    Review,
    Maintain,
}

impl Mood {
    pub(crate) const ALL: [Mood; 5] = [
        Mood::All,
        Mood::Mine,
        Mood::Machines,
        Mood::Review,
        Mood::Maintain,
    ];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Mood::All => "all",
            Mood::Mine => "mine",
            Mood::Machines => "machines",
            Mood::Review => "review",
            Mood::Maintain => "maintain",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Mood::All => "All",
            Mood::Mine => "My pull requests",
            Mood::Machines => "My machines",
            Mood::Review => "Review",
            Mood::Maintain => "Maintain",
        }
    }

    pub(crate) fn parse(s: Option<&str>) -> Option<Mood> {
        Mood::ALL.into_iter().find(|m| Some(m.key()) == s)
    }

    /// The query wins; then the cookie; then all.
    pub(crate) fn of(query: Option<&str>, headers: &HeaderMap) -> Mood {
        Mood::parse(query)
            .or_else(|| Mood::parse(crate::web::cookie(headers, MOOD_COOKIE).as_deref()))
            .unwrap_or_default()
    }
}

/// Why an item is open, in the order of urgency.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Cause {
    /// A critical or high advisory on a package of the person's machines.
    Advisory,
    /// Someone reviewed the person's pull request after their last action.
    Reviewed,
    /// The person's pull request cannot go on: checks, conflict, or no merge.
    Blocked,
    /// The person's pull request has had no review for a week.
    Waiting,
    /// A pull request on the person's machines or on what they maintain.
    Review,
    /// A package the person maintains has a new release or a failed build.
    Package,
}

#[derive(Clone, Debug)]
pub(crate) struct Item {
    pub(crate) key: String,
    pub(crate) cause: Cause,
    /// Plain text.
    pub(crate) title: String,
    /// Plain text: what happened, and what to do.
    pub(crate) detail: String,
    pub(crate) href: String,
    /// Plain text: the words of the primary action.
    pub(crate) action: String,
    /// Changes when the state behind the item changes.
    pub(crate) fingerprint: String,
    pub(crate) moods: Vec<Mood>,
    pub(crate) labels: Vec<(Tone, String)>,
    pub(crate) at: Option<DateTime<Utc>>,
    /// A second link: the advisory, for example.
    pub(crate) more: Option<(String, String)>,
    rank: (u8, i64, String),
}

impl Item {
    pub(crate) fn fits(&self, mood: Mood) -> bool {
        mood == Mood::All || self.moods.contains(&mood)
    }

    /// The page an item opens, so a package page finds its advisory item too.
    fn page(&self) -> &str {
        page_of(&self.key)
    }
}

fn page_of(key: &str) -> &str {
    key.strip_prefix("adv:")
        .or_else(|| key.strip_prefix("pkg:"))
        .unwrap_or(key)
}

pub(crate) fn fingerprint(parts: &[&str]) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, parts.join("\n").as_bytes());
    d.as_ref()[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// What the items are made from. Built by [`Inputs::read`] on a request,
/// and by hand in the tests.
pub(crate) struct Inputs<'a> {
    pub(crate) login: &'a str,
    pub(crate) s: &'a Snapshot,
    pub(crate) index: &'a Index,
    pub(crate) h: &'a Health,
    pub(crate) interests: &'a Interests,
    /// Attributes whose last Hydra build failed, as far as the cache knows.
    pub(crate) failed: &'a HashSet<String>,
    pub(crate) now: DateTime<Utc>,
}

/// Every open item, snoozed or not, in the order of urgency.
pub(crate) fn collect(i: &Inputs) -> Vec<Item> {
    let mut out = Vec::new();
    for (sub, why, _) in i.interests.items.values() {
        let Subject::Pkg(attr) = sub else { continue };
        if why.machine {
            out.extend(advisory(i, attr, *why));
        }
        if why.maintain {
            out.extend(package(i, attr, *why));
        }
    }
    let login = i.login;
    for pr in
        i.s.pr_data
            .iter()
            .filter(|p| p.is_open() && p.author.eq_ignore_ascii_case(login))
    {
        out.extend(own(i, pr));
    }
    out.extend(review(i));
    out.sort_by(|a, b| (a.cause, &a.rank).cmp(&(b.cause, &b.rank)));
    out
}

struct Serious {
    id: String,
    url: String,
    band: Option<Band>,
    cvss: Option<f64>,
    epss: Option<f64>,
    active: bool,
    /// Why it reaches the machine, as a sentence with a leading space.
    note: String,
}

impl Serious {
    fn of(id: &str, url: String, w: &crate::severity::Worst) -> Option<Serious> {
        let high = w.band >= Some(Band::High) || w.cvss.is_some_and(|c| c >= 7.0);
        let likely = w.epss.is_some_and(|e| e >= EPSS_HIGH);
        let active = w.exploitation.as_deref() == Some("active");
        (high || likely || active).then(|| Serious {
            id: id.to_string(),
            url,
            band: w.band,
            cvss: w.cvss,
            epss: w.epss,
            active,
            note: String::new(),
        })
    }

    fn sort_key(&self) -> (std::cmp::Reverse<Option<Band>>, std::cmp::Reverse<i64>) {
        (
            std::cmp::Reverse(self.band),
            std::cmp::Reverse((self.epss.unwrap_or(0.0) * 10_000.0) as i64),
        )
    }
}

/// Whether an open advisory reaches a machine: the reported version
/// decides when the report gives one and the ranges can tell; otherwise the
/// verdict on master and the channels does. A fix only in master still
/// counts, because a machine with no reported version can run a channel.
fn on_machine(
    attr: &str,
    a: &crate::health::Advisory,
    v: &crate::affected::Verdict,
    mine: Option<&String>,
) -> Option<String> {
    use crate::affected::{Check, Verdict};
    if let Some(ver) = mine {
        match crate::affected::check(&a.ranges, &a.title, attr, ver) {
            Check::Affected => return Some(format!(" Your version {ver} is affected.")),
            Check::NotAffected => return None,
            Check::Unknown => {}
        }
    }
    match v {
        Verdict::Fixed { .. } => None,
        Verdict::Behind { fixed_in, channels } => Some(format!(
            " Fixed in master{}; {} still affected.",
            fixed_in
                .as_deref()
                .map(|f| format!(" ({f})"))
                .unwrap_or_default(),
            channels
                .iter()
                .map(|l| format!("{} {}", l.channel, l.version))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        Verdict::Affects | Verdict::Unknown => Some(String::new()),
    }
}

fn advisory(i: &Inputs, attr: &str, why: Why) -> Option<Item> {
    let mine = i.interests.versions.get(attr);
    let mut list: Vec<Serious> = crate::catalog::judged(i.h, i.s, attr)
        .into_iter()
        .filter_map(|(a, v)| {
            let note = on_machine(attr, a, v, mine)?;
            let mut s = Serious::of(&a.code, a.url(), &crate::severity::worst(i.h, &a.cves))?;
            s.note = note;
            Some(s)
        })
        .chain(i.h.osv_extra(attr).into_iter().filter_map(|(id, v)| {
            Serious::of(
                id,
                crate::osv::Vuln::url(id),
                &crate::catalog::osv_worst(i.h, v),
            )
        }))
        .collect();
    if list.is_empty() {
        return None;
    }
    list.sort_by_key(Serious::sort_key);
    let top = &list[0];
    let mut labels = Vec::new();
    if let Some(b) = top.band {
        labels.push((
            Tone::Danger,
            format!(
                "{}{}",
                b.word(),
                top.cvss.map(|c| format!(" {c:.1}")).unwrap_or_default()
            ),
        ));
    }
    if top.active {
        labels.push((Tone::Danger, "exploited in the wild".into()));
    }
    if let Some(e) = top.epss.filter(|e| *e >= EPSS_HIGH) {
        labels.push((Tone::Attention, format!("EPSS {:.0}%", e * 100.0)));
    }
    let mut ids: Vec<&str> = list.iter().map(|s| s.id.as_str()).collect();
    let others = if ids.len() > 1 {
        format!(" and {} more", ids.len() - 1)
    } else {
        String::new()
    };
    let mut detail = format!("On your machines.{}{}", top.note, fix_hint(i, attr));
    if why.maintain {
        detail.push_str(" You maintain it.");
    }
    let more = Some((top.url.clone(), top.id.clone()));
    ids.sort_unstable();
    let mut moods = vec![Mood::Machines];
    if why.maintain {
        moods.push(Mood::Maintain);
    }
    Some(Item {
        key: format!("adv:{attr}"),
        cause: Cause::Advisory,
        title: format!("{attr}: {}{others}", top.id),
        detail,
        href: Subject::Pkg(attr.to_string()).href(),
        action: "Open the package".into(),
        fingerprint: fingerprint(&ids),
        moods,
        labels,
        at: None,
        more,
        rank: (
            top.band.map_or(9, |b| 9 - b as u8),
            -((top.epss.unwrap_or(0.0) * 10_000.0) as i64),
            attr.to_string(),
        ),
    })
}

/// The fixed version is not in the tracker data; an open pull request on
/// the package, or a newer upstream release, is the next best hint.
fn fix_hint(i: &Inputs, attr: &str) -> String {
    let open: Vec<u64> =
        i.s.links
            .prs
            .get(&crate::links::pkg_key(attr))
            .into_iter()
            .flatten()
            .copied()
            .filter(|n| i.s.pr_at.get(n).is_some_and(|&x| i.s.pr_data[x].is_open()))
            .collect();
    let mut out = String::new();
    if let Some(n) = open.iter().max() {
        let _ = write!(out, " Open pull request: #{n}.");
    }
    if let Some(v) = crate::catalog::upstream(i.s, i.index, i.h, attr) {
        let _ = write!(out, " Upstream has {v}.");
    }
    out
}

fn package(i: &Inputs, attr: &str, why: Why) -> Option<Item> {
    let up = crate::catalog::upstream(i.s, i.index, i.h, attr);
    let failed = i.failed.contains(attr);
    if up.is_none() && !failed {
        return None;
    }
    let mut labels = Vec::new();
    let mut detail = String::from("You maintain it.");
    if failed {
        labels.push((Tone::Danger, "Hydra build fails".into()));
        detail.push_str(" The last Hydra build failed.");
    }
    if let Some(v) = up {
        labels.push((Tone::Attention, format!("{v} available")));
        let _ = write!(detail, " Upstream released {v}.");
    }
    let version = i.index.pkgs.get(attr).map_or("", |p| p.version.as_str());
    Some(Item {
        key: format!("pkg:{attr}"),
        cause: Cause::Package,
        title: if version.is_empty() {
            attr.to_string()
        } else {
            format!("{attr} {version}")
        },
        detail,
        href: Subject::Pkg(attr.to_string()).href(),
        action: if failed {
            "Look at the build".into()
        } else {
            "Update it".into()
        },
        fingerprint: fingerprint(&[up.unwrap_or(""), if failed { "failed" } else { "" }]),
        moods: vec![Mood::Maintain],
        labels,
        at: None,
        more: None,
        rank: (
            crate::mine::order(why, true).0,
            i64::from(!failed),
            attr.to_string(),
        ),
    })
}

/// The last time the author pushed or commented.
fn last_act(pr: &Pr) -> DateTime<Utc> {
    pr.pushes
        .iter()
        .chain(&pr.author_activity)
        .copied()
        .chain(std::iter::once(pr.created_at))
        .max()
        .unwrap_or(pr.created_at)
}

fn others(pr: &Pr) -> impl Iterator<Item = &crate::model::Review> {
    pr.reviews.iter().filter(move |r| {
        !r.author_is_bot && !r.author.eq_ignore_ascii_case(&pr.author) && r.state != "DISMISSED"
    })
}

fn verb(state: &str) -> &'static str {
    match state {
        "APPROVED" => "approved",
        "CHANGES_REQUESTED" => "requested changes",
        _ => "commented",
    }
}

fn own(i: &Inputs, pr: &Pr) -> Option<Item> {
    let n = pr.number;
    let mk = |cause: Cause, labels: Vec<(Tone, String)>, detail: String, fp: &[&str], at| Item {
        key: format!("pr:{n}"),
        cause,
        title: format!("#{n} {}", pr.title),
        detail,
        href: format!("/pr/{n}"),
        action: "Open the pull request".into(),
        fingerprint: fingerprint(fp),
        moods: vec![Mood::Mine],
        labels,
        at: Some(at),
        more: None,
        rank: (0, at.timestamp(), format!("{n:012}")),
    };
    let acted = last_act(pr);
    let approvals = crate::committer::approvals(pr);
    let first_ok = approvals.first().map(|(_, t)| *t);
    let changes_after =
        |t: DateTime<Utc>| others(pr).any(|r| r.state == "CHANGES_REQUESTED" && r.submitted_at > t);
    let approved_long = first_ok.filter(|t| {
        !pr.is_draft && i.now - *t >= Duration::days(APPROVED_WAIT) && !changes_after(*t)
    });
    let new: Vec<&crate::model::Review> = others(pr).filter(|r| r.submitted_at > acted).collect();
    if let Some(last) = new.iter().max_by_key(|r| r.submitted_at) {
        let only_approvals = new.iter().all(|r| r.state == "APPROVED");
        if !(only_approvals && approved_long.is_some()) {
            let tone = match last.state.as_str() {
                "CHANGES_REQUESTED" => Tone::Danger,
                "APPROVED" => Tone::Success,
                _ => Tone::Neutral,
            };
            let mut who: Vec<&str> = new.iter().map(|r| r.author.as_str()).collect();
            who.sort_unstable();
            who.dedup();
            let detail = format!(
                "{} {} {} ago, after your last push or comment.{}",
                last.author,
                verb(&last.state),
                ago(last.submitted_at),
                if who.len() > 1 {
                    format!(
                        " Also from: {}.",
                        who.iter()
                            .filter(|w| **w != last.author)
                            .copied()
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                } else {
                    String::new()
                }
            );
            let at = last.submitted_at.to_rfc3339();
            let mut it = mk(
                Cause::Reviewed,
                vec![(tone, verb(&last.state).to_string())],
                detail,
                &["reviewed", &at, &pr.head_oid],
                last.submitted_at,
            );
            it.rank.1 = -last.submitted_at.timestamp();
            return Some(it);
        }
    }
    if matches!(pr.ci.as_deref(), Some("FAILURE" | "ERROR")) {
        return Some(mk(
            Cause::Blocked,
            vec![(Tone::Danger, "checks fail".into())],
            "The checks fail on the last push. Fix them, then push again.".into(),
            &["checks", &pr.head_oid],
            acted,
        ));
    }
    if pr.mergeable == "CONFLICTING" {
        return Some(mk(
            Cause::Blocked,
            vec![(Tone::Attention, "merge conflict".into())],
            format!(
                "It has a conflict with {}. Rebase it, then push again.",
                pr.base
            ),
            &["conflict", &pr.head_oid],
            acted,
        ));
    }
    if let Some(t) = approved_long {
        let by: Vec<&str> = approvals.iter().map(|(l, _)| *l).collect();
        let at = t.to_rfc3339();
        return Some(mk(
            Cause::Blocked,
            vec![(Tone::Attention, format!("approved {} ago", ago(t)))],
            format!(
                "Approved by {}, and not merged. Ask a committer to merge it.",
                by.join(", ")
            ),
            &["approved", &at, &pr.head_oid],
            t,
        ));
    }
    if !pr.is_draft
        && others(pr).next().is_none()
        && i.now - pr.created_at >= Duration::days(REVIEW_WAIT)
    {
        let ping = maintainers(i.s, i.index, pr);
        let detail = if ping.is_empty() {
            format!(
                "Opened {} ago, and no review yet. No maintainer is named for its files: ask for a review on Discourse or Matrix.",
                ago(pr.created_at)
            )
        } else {
            format!(
                "Opened {} ago, and no review yet. Ask its maintainers: {}.",
                ago(pr.created_at),
                ping.iter()
                    .map(|l| format!("@{l}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        return Some(mk(
            Cause::Waiting,
            vec![(
                Tone::Attention,
                format!("no review for {}", ago(pr.created_at)),
            )],
            detail,
            &["waiting", &pr.head_oid],
            pr.created_at,
        ));
    }
    None
}

/// The maintainers of what a pull request touches, less its author.
pub(crate) fn maintainers(s: &Snapshot, index: &Index, pr: &Pr) -> Vec<String> {
    let mut keys: Vec<&String> = s
        .links
        .prs
        .iter()
        .filter(|(_, ns)| ns.contains(&pr.number))
        .map(|(k, _)| k)
        .collect();
    keys.sort();
    let mut out: Vec<String> = Vec::new();
    for k in keys {
        let (m, t) = match Subject::parse(k) {
            Some(Subject::Pkg(a)) => index
                .pkgs
                .get(&a)
                .map(|p| (p.maintainers.clone(), p.teams.clone())),
            Some(Subject::Module(f)) => index
                .modules
                .get(&f)
                .map(|m| (m.maintainers.clone(), m.teams.clone())),
            _ => None,
        }
        .unwrap_or_default();
        for l in m.into_iter().chain(index.team_members(&t)) {
            if !l.eq_ignore_ascii_case(&pr.author) && !out.contains(&l) {
                out.push(l);
            }
        }
    }
    out.truncate(PINGS);
    out
}

fn review(i: &Inputs) -> Vec<Item> {
    let linked = |pick: fn(&Why) -> bool| -> HashSet<u64> {
        i.interests
            .items
            .iter()
            .filter(|(_, (_, w, _))| pick(w))
            .filter_map(|(k, _)| i.s.links.prs.get(k))
            .flatten()
            .copied()
            .collect()
    };
    let on_machines = linked(|w| w.machine);
    let maintained = linked(|w| w.maintain);
    let mut list: Vec<(Why, &crate::queue::Entry)> =
        i.s.queues
            .review
            .iter()
            .filter(|e| !e.author.eq_ignore_ascii_case(i.login))
            .map(|e| {
                let why = Why {
                    machine: on_machines.contains(&e.number),
                    maintain: maintained.contains(&e.number),
                    watch: false,
                };
                (why, e)
            })
            .filter(|(w, _)| w.machine || w.maintain)
            .collect();
    list.sort_by_key(|(w, e)| (crate::mine::order(*w, false), e.number));
    list.into_iter()
        .enumerate()
        .filter_map(|(pos, (why, e))| {
            let pr = i.s.pr_at.get(&e.number).map(|&x| &i.s.pr_data[x])?;
            let mine = pr
                .reviews
                .iter()
                .filter(|r| r.author.eq_ignore_ascii_case(i.login))
                .map(|r| r.submitted_at)
                .max();
            if mine.is_some_and(|t| pr.pushes.iter().all(|p| *p <= t)) {
                return None;
            }
            let mut moods = vec![Mood::Review];
            if why.machine {
                moods.push(Mood::Machines);
            }
            let mut labels: Vec<(Tone, String)> = Vec::new();
            if why.machine {
                labels.push((Tone::Neutral, "on your machines".into()));
            }
            if why.maintain {
                labels.push((Tone::Neutral, "you maintain".into()));
            }
            let detail = format!(
                "#{} by {}, {} old.{}",
                e.number,
                e.author,
                ago(e.created_at),
                if mine.is_some() {
                    " New commits since your review."
                } else if e.reviewers.is_empty() {
                    " No review yet."
                } else {
                    ""
                }
            );
            Some(Item {
                key: format!("pr:{}", e.number),
                cause: Cause::Review,
                title: e.title.clone(),
                detail,
                href: format!("/pr/{}", e.number),
                action: "Review it".into(),
                fingerprint: fingerprint(&["review", &pr.head_oid]),
                moods,
                labels,
                at: Some(e.created_at),
                more: None,
                rank: (0, pos as i64, String::new()),
            })
        })
        .collect()
}

/// The items to show: those not snoozed at their current fingerprint.
pub(crate) fn visible(items: Vec<Item>, snoozed: &HashMap<String, String>) -> Vec<Item> {
    items
        .into_iter()
        .filter(|it| snoozed.get(&it.key) != Some(&it.fingerprint))
        .collect()
}

pub(crate) fn counts(items: &[Item]) -> Vec<(Mood, usize)> {
    Mood::ALL
        .into_iter()
        .map(|m| (m, items.iter().filter(|it| it.fits(m)).count()))
        .collect()
}

/// The item after the one whose page `from` names, in one mood. With no
/// `from`, or one that left the list, the first item that is not `from`.
pub(crate) fn next_after<'a>(
    items: &'a [Item],
    mood: Mood,
    from: Option<&str>,
) -> Option<&'a Item> {
    let list: Vec<&Item> = items.iter().filter(|it| it.fits(mood)).collect();
    let Some(from) = from.filter(|f| !f.is_empty()) else {
        return list.first().copied();
    };
    let here = page_of(from);
    match list.iter().rposition(|it| it.page() == here) {
        Some(p) => list[p + 1..].iter().find(|it| it.page() != here).copied(),
        None => list.into_iter().find(|it| it.page() != here),
    }
}

fn read_inputs<T>(app: &App, login: &str, f: impl FnOnce(&Inputs<'_>) -> T) -> T {
    let interests = Interests::of(app, login);
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let failed: HashSet<String> = interests
        .items
        .values()
        .filter(|(_, w, _)| w.maintain)
        .filter_map(|(sub, ..)| match sub {
            Subject::Pkg(a) => Some(a),
            _ => None,
        })
        .filter(
            |a| matches!(app.hydra.known(&Hydra::package_job(a)), Some(Last::Build(b)) if !b.ok),
        )
        .cloned()
        .collect();
    f(&Inputs {
        login,
        s: &s,
        index: &index,
        h: &h,
        interests: &interests,
        failed: &failed,
        now: Utc::now(),
    })
}

/// Every open item of a person, and the ones to show.
pub(crate) fn of(app: &App, login: &str) -> (Vec<Item>, Vec<Item>) {
    let all = read_inputs(app, login, collect);
    let keys: Vec<String> = all.iter().map(|it| it.key.clone()).collect();
    let _ = app.store.needs_forget(login, &keys);
    let snoozed = app.store.needs_snoozed(login).unwrap_or_default();
    let shown = visible(all.clone(), &snoozed);
    (all, shown)
}

fn session_hash(headers: &HeaderMap) -> Option<String> {
    crate::web::cookie(headers, "session").map(|id| crate::sets::token_hash(&id))
}

pub(crate) fn mood_cookie(app: &App, mood: Mood) -> String {
    crate::web::set_cookie(app, MOOD_COOKIE, mood.key(), 365 * 86400)
}

/// Keys of the list and of the pages reached from it. It reads only the
/// marks it finds, and skips keys typed into a field.
const KEYS: &str = r#"<script>(function(){var L=[].slice.call(document.querySelectorAll(".needs-item")),i=-1;function sel(n){if(!L.length)return false;i=Math.max(0,Math.min(L.length-1,n));L.forEach(function(e,k){e.classList.toggle("needs-sel",k===i)});L[i].focus();return true}document.addEventListener("keydown",function(e){if(e.ctrlKey||e.metaKey||e.altKey||e.defaultPrevented)return;var t=e.target,k=e.key,a=null,f=null;if(t&&(t.isContentEditable||/^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName)))return;if(k==="Enter"&&t&&t.closest&&t.closest("a,button,summary"))return;if(k==="j"){if(!sel(i+1))return}else if(k==="k"){if(!sel(i<0?0:i-1))return}else if(k==="o"||k==="Enter"){a=i>=0?L[i].querySelector("a.needs-open"):null;if(!a)return;location.href=a.href}else if(k==="n"){a=document.querySelector("a[data-needs-next]");if(!a)return;location.href=a.href}else if(k==="s"){f=i>=0?L[i].querySelector("form.needs-snooze"):document.querySelector("form.needs-snooze-here");if(!f)return;f.submit()}else return;e.preventDefault()})})();</script>"#;

const HINT: &str = "<span class=\"needs-keys f6 color-fg-muted\">Keys: <kbd>j</kbd> <kbd>k</kbd> <kbd>o</kbd> <kbd>n</kbd> <kbd>s</kbd></span>";

fn cause_icon(c: Cause) -> (&'static str, &'static str, &'static str) {
    match c {
        Cause::Advisory => (octicon!("shield"), "color-fg-danger", "advisory"),
        Cause::Reviewed => (octicon!("comment"), "color-fg-accent", "new review"),
        Cause::Blocked => (octicon!("skip"), "color-fg-danger", "blocked"),
        Cause::Waiting => (
            octicon!("inbox"),
            "color-fg-attention",
            "waiting for review",
        ),
        Cause::Review => (octicon!("eye"), "color-fg-open", "to review"),
        Cause::Package => (octicon!("package"), "color-fg-attention", "package"),
    }
}

fn snooze_form(it: &Item, mood: Mood, then: &str, class: &str, label: &str) -> String {
    format!(
        "<form method=\"post\" action=\"/needs/snooze\" class=\"{class}\">\
         <input type=\"hidden\" name=\"key\" value=\"{}\"><input type=\"hidden\" name=\"fingerprint\" value=\"{}\">\
         <input type=\"hidden\" name=\"mood\" value=\"{}\"><input type=\"hidden\" name=\"then\" value=\"{then}\">\
         <button class=\"btn btn-sm\" type=\"submit\" title=\"Hide it until it changes\">{label}</button></form>",
        esc(&it.key),
        esc(&it.fingerprint),
        mood.key()
    )
}

fn row(it: &Item, mood: Mood) -> String {
    let (svg, class, label) = cause_icon(it.cause);
    let labels: String = it
        .labels
        .iter()
        .map(|(t, l)| format!(" {}", badge(*t, &esc(l))))
        .collect();
    let more = it
        .more
        .as_ref()
        .map(|(href, text)| format!(" · <a href=\"{}\">{}</a>", esc(href), esc(text)))
        .unwrap_or_default();
    format!(
        "<div class=\"Box-row d-flex flex-items-start needs-item\" tabindex=\"-1\" data-key=\"{key}\">\
         <span class=\"mr-2 {class}\" role=\"img\" aria-label=\"{label}\">{ic}</span>\
         <div class=\"flex-auto min-width-0 wb-break-word\">\
         <a class=\"Link--primary text-bold needs-open\" href=\"{href}\">{title}</a>{labels}\
         <div class=\"f6 color-fg-muted mt-1\">{detail}{more}</div></div>{snooze}</div>",
        key = esc(&it.key),
        ic = icon(svg),
        href = esc(&it.href),
        title = esc(&it.title),
        detail = esc(&it.detail),
        snooze = snooze_form(it, mood, "home", "needs-snooze ml-2", "Snooze"),
    )
}

fn chips(counts: &[(Mood, usize)], mood: Mood) -> String {
    let tabs: Vec<crate::ui::Tab> = counts
        .iter()
        .map(|(m, n)| {
            crate::ui::Tab::new(format!("/?mood={}", m.key()), m.label(), *m == mood).count(*n)
        })
        .collect();
    crate::ui::subnav(&tabs).replacen("aria-label=\"Views\"", "aria-label=\"Moods\"", 1)
}

fn empty_text(mood: Mood) -> &'static str {
    match mood {
        Mood::All => {
            "Nothing needs you now. Critical advisories on your machines, reviews on your pull requests, and pull requests on what you maintain show here."
        }
        Mood::Mine => "None of your open pull requests waits for you.",
        Mood::Machines => {
            "No serious advisory and no open pull request on the packages of your machines. <a href=\"/me/settings#upload\">Upload a new report</a> when your machines change."
        }
        Mood::Review => {
            "No pull request on your machines or on what you maintain waits for your review. The pull requests with <a href=\"/review?tab=unowned\">no maintainer</a> need reviews too."
        }
        Mood::Maintain => {
            "The packages you maintain have no newer release and no known failed build."
        }
    }
}

/// The top of the home page of a person who signed in.
pub(crate) fn section(app: &App, login: &str, mood: Mood, headers: &HeaderMap) -> String {
    let (all, shown) = of(app, login);
    let done = session_hash(headers)
        .and_then(|s| {
            let keys: Vec<String> = all.iter().map(|it| it.key.clone()).collect();
            app.store.needs_session(&s, &keys).ok()
        })
        .unwrap_or(0);
    render(&shown, mood, done)
}

fn render(shown: &[Item], mood: Mood, done: usize) -> String {
    let counts = counts(shown);
    let list: Vec<&Item> = shown.iter().filter(|it| it.fits(mood)).collect();
    let rows: String = list.iter().take(SHOWN).map(|it| row(it, mood)).collect();
    let body = if rows.is_empty() {
        crate::ui::empty(empty_text(mood))
    } else {
        rows
    };
    let start = if list.is_empty() {
        String::new()
    } else {
        format!(
            "<a class=\"btn btn-sm btn-primary ml-2\" data-needs-next href=\"/needs/next?mood={}\">Start</a>",
            mood.key()
        )
    };
    let mut foot = Vec::new();
    if list.len() > SHOWN {
        foot.push(format!(
            "{} more. Press Next on each page to go through all of them.",
            list.len() - SHOWN
        ));
    }
    if matches!(mood, Mood::All | Mood::Review) {
        foot.push(
            "Pull requests on what you watch are in <a href=\"/review?tab=foryou\">For you</a>."
                .into(),
        );
    }
    if done > 0 {
        foot.push(format!("{done} done this session."));
    }
    let foot = format!(
        "<div class=\"Box-footer d-flex flex-wrap flex-items-center gap-2 f6 color-fg-muted\"><span class=\"flex-auto\">{}</span>{HINT}</div>",
        foot.join(" ")
    );
    format!(
        "<div id=\"needs\">{}<section class=\"Box mb-4\"><div class=\"Box-header d-flex flex-items-center\">\
         <h2 class=\"Box-title flex-auto\">{} Needs you <span class=\"Counter\">{}</span></h2>{start}</div>{body}{foot}</section>{KEYS}</div>",
        chips(&counts, mood),
        icon(octicon!("checklist")),
        list.len(),
    )
}

/// A small Next button for a page reached from Needs you, with Snooze
/// when the page is an open item. None when nothing comes next.
pub fn next_link(app: &App, login: &str, mood: Mood, current_key: &str) -> Option<String> {
    let (_, shown) = of(app, login);
    let here = shown
        .iter()
        .find(|it| it.fits(mood) && it.key == current_key)
        .or_else(|| {
            shown
                .iter()
                .find(|it| it.fits(mood) && it.page() == page_of(current_key))
        });
    let next = next_after(&shown, mood, Some(current_key));
    if next.is_none() && here.is_none() {
        return None;
    }
    let mut out = String::from("<span class=\"needs-nav d-inline-flex flex-items-center gap-2\">");
    if let Some(it) = here {
        out.push_str(&snooze_form(
            it,
            mood,
            "next",
            "needs-snooze-here d-inline",
            "Snooze",
        ));
    }
    let _ = write!(
        out,
        "<a class=\"btn btn-sm\" data-needs-next href=\"/needs/next?mood={}&amp;from={}\" title=\"The next item of Needs you\">Next {}</a>",
        mood.key(),
        crate::github::urlencode(current_key),
        icon(octicon!("arrow-right"))
    );
    out.push_str(HINT);
    out.push_str("</span>");
    out.push_str(KEYS);
    Some(out)
}

/// [`next_link`] for the person and the mood of a request, or nothing.
pub(crate) fn here(app: &App, headers: &HeaderMap, current_key: &str) -> String {
    let Some(login) = app.viewer(headers) else {
        return String::new();
    };
    next_link(app, &login, Mood::of(None, headers), current_key).unwrap_or_default()
}

#[derive(Deserialize)]
struct NextQuery {
    mood: Option<String>,
    from: Option<String>,
}

async fn next(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<NextQuery>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let mood = Mood::of(q.mood.as_deref(), &headers);
    let (_, shown) = of(&app, &login);
    let to = next_after(&shown, mood, q.from.as_deref())
        .map(|it| it.href.clone())
        .unwrap_or_else(|| format!("/?mood={}#needs", mood.key()));
    (
        [(header::SET_COOKIE, mood_cookie(&app, mood))],
        Redirect::to(&to),
    )
        .into_response()
}

#[derive(Deserialize)]
struct SnoozeForm {
    key: String,
    fingerprint: String,
    mood: Option<String>,
    then: Option<String>,
}

async fn snooze(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<SnoozeForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    if f.key.len() > 300 || f.fingerprint.len() > 64 || f.key.is_empty() {
        return (StatusCode::BAD_REQUEST, "no such item").into_response();
    }
    if let Err(e) = app.store.needs_snooze(&login, &f.key, &f.fingerprint) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    let mood = Mood::of(f.mood.as_deref(), &headers);
    let to = if f.then.as_deref() == Some("next") {
        format!(
            "/needs/next?mood={}&from={}",
            mood.key(),
            crate::github::urlencode(&f.key)
        )
    } else {
        format!("/?mood={}#needs", mood.key())
    };
    Redirect::to(&to).into_response()
}

#[derive(Deserialize)]
struct ApiQuery {
    mood: Option<String>,
}

async fn api(State(app): State<Shared>, headers: HeaderMap, Query(q): Query<ApiQuery>) -> Response {
    let private = [(header::CACHE_CONTROL, "private, no-store")];
    let Some(login) = app.viewer(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            private,
            Json(json!({ "error": "sign in first" })),
        )
            .into_response();
    };
    let mood = Mood::parse(q.mood.as_deref()).unwrap_or_default();
    let (_, shown) = of(&app, &login);
    let counts: serde_json::Map<String, serde_json::Value> = counts(&shown)
        .into_iter()
        .map(|(m, n)| (m.key().to_string(), json!(n)))
        .collect();
    let items: Vec<serde_json::Value> = shown
        .iter()
        .filter(|it| it.fits(mood))
        .map(|it| {
            json!({
                "key": it.key,
                "cause": it.cause,
                "title": it.title,
                "detail": it.detail,
                "href": it.href,
                "action": it.action,
                "fingerprint": it.fingerprint,
                "moods": it.moods.iter().map(|m| m.key()).collect::<Vec<_>>(),
                "labels": it.labels.iter().map(|(_, l)| l).collect::<Vec<_>>(),
                "at": it.at,
                "link": it.more.as_ref().map(|(href, _)| href),
            })
        })
        .collect();
    (
        private,
        Json(json!({
            "login": login,
            "mood": mood.key(),
            "counts": counts,
            "items": items,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::area::Package;
    use crate::health::Advisory;
    use crate::model::Review;
    use crate::queue::Entry;
    use crate::severity::Cve;

    fn now() -> DateTime<Utc> {
        "2026-10-17T12:00:00Z".parse().unwrap()
    }

    fn ago_days(d: i64) -> DateTime<Utc> {
        now() - Duration::days(d)
    }

    fn pr(n: u64, author: &str, opened: i64) -> Pr {
        Pr {
            number: n,
            title: format!("change {n}"),
            url: format!("https://github.com/NixOS/nixpkgs/pull/{n}"),
            author: author.into(),
            author_is_bot: false,
            state: "OPEN".into(),
            is_draft: false,
            base: "master".into(),
            created_at: ago_days(opened),
            updated_at: ago_days(0),
            merged_at: None,
            merged_by: None,
            mergeable: "MERGEABLE".into(),
            head_oid: format!("head{n}"),
            files: vec![],
            ci: Some("SUCCESS".into()),
            reviews: vec![],
            author_activity: vec![],
            pushes: vec![ago_days(opened)],
            voids: vec![],
            closed_at: None,
            additions: 1,
            deletions: 1,
            changed_files: 1,
            labels: vec![],
            association: String::new(),
            ai: None,
        }
    }

    fn rev(p: &Pr, author: &str, state: &str, at: DateTime<Utc>) -> Review {
        Review {
            url: format!("{}#pullrequestreview-{}", p.url, at.timestamp()),
            author: author.into(),
            author_is_bot: false,
            state: state.into(),
            submitted_at: at,
            commit_oid: Some(p.head_oid.clone()),
            inline_on_change: false,
        }
    }

    fn entry(p: &Pr) -> Entry {
        Entry {
            number: p.number,
            title: p.title.clone(),
            url: p.url.clone(),
            author: p.author.clone(),
            author_score: 0.0,
            base: p.base.clone(),
            created_at: p.created_at,
            updated_at: p.updated_at,
            ci: p.ci.clone(),
            mergeable: p.mergeable.clone(),
            reviewers: p.reviews.iter().map(|r| r.author.clone()).collect(),
            approved_by: vec![],
            credited_by: vec![],
            files: p.files.clone(),
            unowned: false,
        }
    }

    struct World {
        s: Snapshot,
        index: Index,
        h: Health,
        machines: Vec<String>,
        versions: HashMap<String, String>,
        failed: HashSet<String>,
    }

    impl World {
        fn new() -> World {
            let mut index = Index::default();
            for (name, m) in [
                ("curl", &[][..]),
                ("hello", &["alice", "carol"][..]),
                ("jq", &["alice"][..]),
            ] {
                index.pkgs.insert(
                    name.into(),
                    Package {
                        pname: name.into(),
                        version: "1.0".into(),
                        file: Some(format!("pkgs/by-name/{name}/package.nix")),
                        maintainers: m.iter().map(|x| x.to_string()).collect(),
                        ..Default::default()
                    },
                );
                index.by_pname.insert(name.into(), name.into());
            }
            World {
                s: Snapshot::default(),
                index,
                h: Health::default(),
                machines: vec!["curl".into(), "hello".into()],
                failed: HashSet::new(),
                versions: HashMap::new(),
            }
        }

        fn add(&mut self, p: Pr, touches: &[&str]) {
            for t in touches {
                self.s
                    .links
                    .prs
                    .entry(crate::links::pkg_key(t))
                    .or_default()
                    .push(p.number);
            }
            if p.author != "alice" {
                self.s.queues.review.push(entry(&p));
            }
            self.s.pr_at.insert(p.number, self.s.pr_data.len());
            self.s.pr_data.push(p);
        }

        fn advise(&mut self, attr: &str, code: &str, cve: &str, c: Cve) {
            self.h
                .advisories
                .entry(attr.into())
                .or_default()
                .push(Advisory {
                    code: code.into(),
                    cves: vec![cve.into()],
                    title: "a flaw".into(),
                    issue: None,
                    ranges: vec![],
                });
            self.h.cves.insert(cve.into(), c);
        }

        fn items(&self) -> Vec<Item> {
            let mut interests = Interests::build(&self.index, "alice", &self.machines, &[], true);
            interests.versions = self.versions.clone();
            collect(&Inputs {
                login: "alice",
                s: &self.s,
                index: &self.index,
                h: &self.h,
                interests: &interests,
                failed: &self.failed,
                now: now(),
            })
        }
    }

    fn keys(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.key.as_str()).collect()
    }

    fn find<'a>(items: &'a [Item], key: &str) -> &'a Item {
        items.iter().find(|i| i.key == key).expect(key)
    }

    fn critical() -> Cve {
        Cve {
            band: Some(Band::Critical),
            cvss: Some(9.8),
            ..Default::default()
        }
    }

    #[test]
    fn a_serious_advisory_on_a_machine_is_an_item() {
        let mut w = World::new();
        w.advise("curl", "NIXPKGS-2026-1", "CVE-2026-1", critical());
        w.advise(
            "hello",
            "NIXPKGS-2026-2",
            "CVE-2026-2",
            Cve {
                band: Some(Band::Low),
                epss: Some(0.001),
                ..Default::default()
            },
        );
        w.advise("jq", "NIXPKGS-2026-3", "CVE-2026-3", critical());
        let items = w.items();
        assert_eq!(keys(&items), ["adv:curl"]);
        let it = &items[0];
        assert_eq!(it.cause, Cause::Advisory);
        assert_eq!(it.href, "/p/curl");
        assert!(it.labels.iter().any(|(_, l)| l == "critical 9.8"));
        assert_eq!(it.moods, [Mood::Machines]);
        assert_eq!(
            it.more.as_ref().map(|(_, t)| t.as_str()),
            Some("NIXPKGS-2026-1")
        );

        w.h.cves.get_mut("CVE-2026-2").unwrap().epss = Some(0.4);
        let items = w.items();
        let hello = find(&items, "adv:hello");
        assert!(hello.labels.iter().any(|(_, l)| l == "EPSS 40%"));
        assert_eq!(hello.moods, [Mood::Machines, Mood::Maintain]);
        assert_eq!(keys(&items), ["adv:curl", "adv:hello"]);
    }

    #[test]
    fn an_advisory_fixed_everywhere_is_no_item() {
        let (index, h, s, _) = crate::catalog::tests::unbound("1.26.1");
        let mut w = World::new();
        w.index = index;
        w.h = h;
        w.s = s;
        w.machines = vec!["unbound".into()];
        assert!(keys(&w.items()).is_empty());
    }

    #[test]
    fn the_reported_version_decides_before_master() {
        let (index, h, s, _) = crate::catalog::tests::unbound("1.26.1");
        let mut w = World::new();
        w.index = index;
        w.h = h;
        w.s = s;
        w.machines = vec!["unbound".into()];
        w.versions.insert("unbound".into(), "1.26.0".into());
        let items = w.items();
        assert_eq!(keys(&items), ["adv:unbound"]);
        assert!(items[0].detail.contains("Your version 1.26.0 is affected."));
        w.versions.insert("unbound".into(), "1.26.1".into());
        assert!(w.items().is_empty());
        w.h.advisories.get_mut("unbound").unwrap()[0].title = "unbound: a flaw".into();
        let ch = crate::channels::Channels::default();
        w.h.assess(&w.index, &ch);
        assert_eq!(keys(&w.items()), ["adv:unbound"]);
    }

    #[test]
    fn a_fix_only_in_master_stays_an_item_for_an_unknown_version() {
        let (index, h, s, _) = crate::catalog::tests::unbound("1.25.0");
        let mut w = World::new();
        w.index = index;
        w.h = h;
        w.s = s;
        w.machines = vec!["unbound".into()];
        let items = w.items();
        assert_eq!(keys(&items), ["adv:unbound"]);
        assert!(
            items[0]
                .detail
                .contains("Fixed in master (1.26.1); nixos-26.05 1.25.0 still affected."),
            "{}",
            items[0].detail
        );
    }

    #[test]
    fn a_review_after_the_last_push_is_an_item() {
        let mut w = World::new();
        let mut p = pr(1, "alice", 10);
        p.pushes = vec![ago_days(5)];
        let r = rev(&p, "bob", "CHANGES_REQUESTED", ago_days(1));
        p.reviews.push(r);
        w.add(p.clone(), &[]);
        let items = w.items();
        let it = find(&items, "pr:1");
        assert_eq!(it.cause, Cause::Reviewed);
        assert!(it.detail.starts_with("bob requested changes"));
        assert_eq!(it.moods, [Mood::Mine]);

        let mut w = World::new();
        p.author_activity.push(now() - Duration::hours(2));
        w.add(p, &[]);
        assert!(w.items().is_empty());
    }

    #[test]
    fn a_blocked_pull_request_is_an_item() {
        let mut w = World::new();
        let mut failing = pr(1, "alice", 2);
        failing.ci = Some("FAILURE".into());
        let mut conflict = pr(2, "alice", 2);
        conflict.mergeable = "CONFLICTING".into();
        let mut approved = pr(3, "alice", 9);
        let a = rev(&approved, "bob", "APPROVED", ago_days(4));
        approved.reviews.push(a);
        let mut fresh = pr(4, "alice", 9);
        let a = rev(&fresh, "bob", "APPROVED", ago_days(1));
        fresh.reviews.push(a);
        for p in [failing, conflict, approved, fresh] {
            w.add(p, &[]);
        }
        let items = w.items();
        assert_eq!(find(&items, "pr:1").labels[0].1, "checks fail");
        assert_eq!(find(&items, "pr:2").labels[0].1, "merge conflict");
        let a = find(&items, "pr:3");
        assert_eq!(a.cause, Cause::Blocked);
        assert!(a.detail.contains("Approved by bob"));
        assert_eq!(find(&items, "pr:4").cause, Cause::Reviewed);
    }

    #[test]
    fn a_week_with_no_review_names_the_maintainers() {
        let mut w = World::new();
        w.add(pr(1, "alice", 8), &["hello"]);
        w.add(pr(2, "alice", 3), &["hello"]);
        let mut draft = pr(3, "alice", 30);
        draft.is_draft = true;
        w.add(draft, &[]);
        w.add(pr(4, "alice", 8), &["curl"]);
        let items = w.items();
        assert_eq!(keys(&items), ["pr:1", "pr:4"]);
        let it = find(&items, "pr:1");
        assert_eq!(it.cause, Cause::Waiting);
        assert!(it.detail.contains("@carol"), "{}", it.detail);
        assert!(!it.detail.contains("@alice"));
        assert!(find(&items, "pr:4").detail.contains("No maintainer"));
    }

    #[test]
    fn pull_requests_to_review_come_machines_first() {
        let mut w = World::new();
        w.add(pr(11, "erin", 1), &["jq"]);
        w.add(pr(10, "dave", 1), &["curl"]);
        w.add(pr(12, "frank", 1), &[]);
        let items = w.items();
        assert_eq!(keys(&items), ["pr:10", "pr:11"]);
        assert_eq!(items[0].moods, [Mood::Review, Mood::Machines]);
        assert_eq!(items[1].moods, [Mood::Review]);
        assert_eq!(items[0].cause, Cause::Review);

        let i = w.s.pr_at[&11];
        let r = rev(&w.s.pr_data[i], "alice", "COMMENTED", ago_days(1));
        w.s.pr_data[i].reviews.push(r);
        assert_eq!(keys(&w.items()), ["pr:10"]);
        w.s.pr_data[i].pushes.push(now());
        assert_eq!(keys(&w.items()), ["pr:10", "pr:11"]);
        assert!(find(&w.items(), "pr:11").detail.contains("New commits"));
    }

    #[test]
    fn a_maintained_package_with_a_release_or_a_failed_build_is_an_item() {
        let mut w = World::new();
        w.h.upstream.insert("jq".into(), "2.0".into());
        w.failed.insert("hello".into());
        w.failed.insert("curl".into());
        let items = w.items();
        assert_eq!(keys(&items), ["pkg:hello", "pkg:jq"]);
        assert!(find(&items, "pkg:jq").labels[0].1.contains("2.0 available"));
        assert_eq!(find(&items, "pkg:hello").action, "Look at the build");
        assert!(items.iter().all(|i| i.moods == [Mood::Maintain]));
    }

    fn mixed() -> Vec<Item> {
        let mut w = World::new();
        w.h.upstream.insert("jq".into(), "2.0".into());
        w.add(pr(10, "dave", 1), &["curl"]);
        w.add(pr(1, "alice", 8), &["hello"]);
        let mut failing = pr(2, "alice", 2);
        failing.ci = Some("ERROR".into());
        w.add(failing, &[]);
        let mut reviewed = pr(3, "alice", 2);
        let r = rev(&reviewed, "bob", "COMMENTED", ago_days(1));
        reviewed.reviews.push(r);
        w.add(reviewed, &[]);
        w.advise("curl", "NIXPKGS-2026-1", "CVE-2026-1", critical());
        w.items()
    }

    #[test]
    fn the_most_urgent_comes_first() {
        let items = mixed();
        assert_eq!(
            keys(&items),
            ["adv:curl", "pr:3", "pr:2", "pr:1", "pr:10", "pkg:jq"]
        );
        let causes: Vec<Cause> = items.iter().map(|i| i.cause).collect();
        let mut sorted = causes.clone();
        sorted.sort();
        assert_eq!(causes, sorted);
    }

    #[test]
    fn a_mood_keeps_its_own_items() {
        let items = mixed();
        let c: HashMap<Mood, usize> = counts(&items).into_iter().collect();
        assert_eq!(c[&Mood::All], 6);
        assert_eq!(c[&Mood::Mine], 3);
        assert_eq!(c[&Mood::Machines], 2);
        assert_eq!(c[&Mood::Review], 1);
        assert_eq!(c[&Mood::Maintain], 1);
        let html = render(&items, Mood::Mine, 0);
        assert!(html.contains("href=\"/pr/3\""));
        assert!(!html.contains("href=\"/p/jq\""));
        assert!(
            html.contains(
                "aria-current=\"page\">My pull requests <span class=\"Counter\">3</span>"
            )
        );
        assert_eq!(Mood::parse(Some("review")), Some(Mood::Review));
        assert_eq!(Mood::parse(Some("nonsense")), None);
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, "mood=maintain".parse().unwrap());
        assert_eq!(Mood::of(None, &h), Mood::Maintain);
        assert_eq!(Mood::of(Some("mine"), &h), Mood::Mine);
        assert_eq!(Mood::of(None, &HeaderMap::new()), Mood::All);
    }

    #[test]
    fn a_snoozed_item_comes_back_when_it_changes() {
        let mut w = World::new();
        let mut p = pr(1, "alice", 10);
        let r = rev(&p, "bob", "COMMENTED", ago_days(2));
        p.reviews.push(r);
        w.add(p, &[]);
        let before = w.items();
        let snoozed: HashMap<String, String> =
            [("pr:1".to_string(), before[0].fingerprint.clone())].into();
        assert!(visible(before.clone(), &snoozed).is_empty());
        let i = w.s.pr_at[&1];
        let r = rev(&w.s.pr_data[i], "carol", "CHANGES_REQUESTED", ago_days(1));
        w.s.pr_data[i].reviews.push(r);
        let after = w.items();
        assert_ne!(after[0].fingerprint, before[0].fingerprint);
        assert_eq!(keys(&visible(after, &snoozed)), ["pr:1"]);
        assert_eq!(fingerprint(&["a", "b"]), fingerprint(&["a", "b"]));
        assert_ne!(fingerprint(&["a", "b"]), fingerprint(&["ab"]));
    }

    #[test]
    fn next_walks_one_mood() {
        let items = mixed();
        let key = |it: Option<&Item>| it.map(|i| i.key.clone());
        assert_eq!(
            key(next_after(&items, Mood::All, None)),
            Some("adv:curl".into())
        );
        assert_eq!(
            key(next_after(&items, Mood::All, Some("adv:curl"))),
            Some("pr:3".into())
        );
        assert_eq!(
            key(next_after(&items, Mood::All, Some("pkg:curl"))),
            Some("pr:3".into())
        );
        assert_eq!(
            key(next_after(&items, Mood::Mine, Some("pr:3"))),
            Some("pr:2".into())
        );
        assert_eq!(key(next_after(&items, Mood::Mine, Some("pr:1"))), None);
        assert_eq!(
            key(next_after(&items, Mood::Machines, Some("adv:curl"))),
            Some("pr:10".into())
        );
        assert_eq!(
            key(next_after(&items, Mood::Mine, Some("pr:999"))),
            Some("pr:3".into())
        );
        assert_eq!(
            key(next_after(&items, Mood::Maintain, Some("pkg:jq"))),
            None
        );
    }

    #[test]
    fn the_list_works_without_script() {
        let items = mixed();
        let html = render(&items, Mood::All, 2);
        assert!(html.contains("2 done this session."));
        assert_eq!(
            html.matches("action=\"/needs/snooze\"").count(),
            items.len()
        );
        assert!(html.contains("data-needs-next href=\"/needs/next?mood=all\""));
        assert!(html.contains("Keys: <kbd>j</kbd>"));
        assert!(!render(&items, Mood::All, 0).contains("done this session"));
        let none = render(&[], Mood::Review, 0);
        assert!(none.contains("no maintainer"));
        assert!(!none.contains("data-needs-next href"));
    }

    #[test]
    fn the_store_keeps_snoozes_and_counts_what_left() {
        let store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        store.needs_snooze("alice", "pr:1", "f1").unwrap();
        store.needs_snooze("alice", "pr:1", "f2").unwrap();
        store.needs_snooze("alice", "pr:2", "f3").unwrap();
        assert_eq!(store.needs_snoozed("alice").unwrap()["pr:1"], "f2");
        assert!(store.needs_snoozed("bob").unwrap().is_empty());
        store.needs_forget("alice", &["pr:1".into()]).unwrap();
        assert_eq!(store.needs_snoozed("alice").unwrap().len(), 1);
        let k = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(store.needs_session("s1", &k(&["a", "b", "c"])).unwrap(), 0);
        assert_eq!(store.needs_session("s1", &k(&["b"])).unwrap(), 2);
        assert_eq!(store.needs_session("s1", &k(&["a", "b"])).unwrap(), 1);
        assert_eq!(store.needs_session("s2", &k(&[])).unwrap(), 0);
    }
}
