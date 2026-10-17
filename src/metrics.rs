//! The state of nixpkgs in numbers: inflow, outflow, backlog, review latency,
//! who carries the load and how much disclosed AI work arrives. Aggregates
//! only; no figure here names a person.

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::github::OTHER_FORM;
use crate::model::Pr;

/// One pull request reduced to what the metrics read. The history walk stores
/// these for pull requests outside the ledger's window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Facts {
    pub number: u64,
    pub author: String,
    pub author_is_bot: bool,
    pub association: String,
    pub state: String,
    pub is_draft: bool,
    pub base: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub merged_at: Option<DateTime<Utc>>,
    pub merged_by: Option<String>,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub labels: Vec<String>,
    pub areas: Vec<String>,
    pub ai: Option<String>,
    /// Reviews by people other than the author; bots are left out.
    pub reviews: Vec<Seen>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Seen {
    pub by: String,
    pub at: DateTime<Utc>,
    pub state: String,
}

impl From<&Pr> for Facts {
    fn from(p: &Pr) -> Self {
        let closed_at = p
            .closed_at
            .or(p.merged_at)
            .or_else(|| (p.state == "CLOSED").then_some(p.updated_at));
        Facts {
            number: p.number,
            author: p.author.clone(),
            author_is_bot: p.author_is_bot,
            association: p.association.clone(),
            state: p.state.clone(),
            is_draft: p.is_draft,
            base: p.base.clone(),
            created_at: p.created_at,
            updated_at: p.updated_at,
            closed_at,
            merged_at: p.merged_at,
            merged_by: p.merged_by.clone(),
            additions: p.additions,
            deletions: p.deletions,
            changed_files: p.changed_files,
            labels: p.labels.clone(),
            areas: areas(&p.files),
            ai: p.ai.clone(),
            reviews: p
                .reviews
                .iter()
                .filter(|r| !r.author_is_bot)
                .map(|r| Seen {
                    by: r.author.clone(),
                    at: r.submitted_at,
                    state: r.state.clone(),
                })
                .collect(),
        }
    }
}

const AREAS: [(&str, &str); 13] = [
    ("pkgs/by-name/", "packages: by-name"),
    ("pkgs/development/python-modules/", "packages: python"),
    ("pkgs/development/", "packages: development"),
    ("pkgs/stdenv/", "stdenv"),
    ("pkgs/top-level/", "packages: top-level"),
    ("pkgs/", "packages: other"),
    ("nixos/modules/", "nixos: modules"),
    ("nixos/tests/", "nixos: tests"),
    ("nixos/", "nixos: other"),
    ("lib/", "lib"),
    ("doc/", "doc"),
    ("maintainers/", "maintainers"),
    ("ci/", "ci"),
];

pub fn areas(files: &[String]) -> Vec<String> {
    let mut out: Vec<String> = files
        .iter()
        .map(|f| {
            AREAS
                .iter()
                .find(|(prefix, _)| f.starts_with(prefix))
                .map_or(
                    if f.starts_with(".github/") {
                        "ci"
                    } else {
                        "other"
                    },
                    |(_, a)| a,
                )
                .to_string()
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Two sources describe one pull request: the full sync and the history walk.
/// The one GitHub updated last wins.
pub fn merge(history: Vec<Facts>, prs: &[Pr]) -> Vec<Facts> {
    let mut by: HashMap<u64, Facts> = history.into_iter().map(|f| (f.number, f)).collect();
    for p in prs {
        if by
            .get(&p.number)
            .is_none_or(|h| h.updated_at <= p.updated_at)
        {
            by.insert(p.number, Facts::from(p));
        }
    }
    by.into_values().collect()
}

impl Facts {
    fn has(&self, label: &str) -> bool {
        self.labels.iter().any(|l| l == label)
    }

    fn first_time(&self) -> bool {
        matches!(
            self.association.as_str(),
            "FIRST_TIME_CONTRIBUTOR" | "FIRST_TIMER"
        ) || self.has("12.first-time contribution")
    }

    fn security(&self) -> bool {
        self.has("1.severity: security")
    }

    fn new_package(&self) -> bool {
        self.has("8.has: package (new)")
    }

    fn backport(&self) -> bool {
        self.base.starts_with("release-")
    }

    fn mass_rebuild(&self) -> bool {
        self.labels.iter().any(|l| {
            l.starts_with("10.rebuild-linux: ") && (l.ends_with("501+") || l.contains("5001"))
        })
    }

    fn approved(&self) -> bool {
        self.reviews.iter().any(|r| r.state == "APPROVED")
    }

    fn first_review(&self) -> Option<DateTime<Utc>> {
        self.reviews.iter().map(|r| r.at).min()
    }

    fn open_at(&self, t: DateTime<Utc>) -> bool {
        self.created_at <= t && self.closed_at.is_none_or(|c| c > t)
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Month {
    pub month: String,
    pub opened: usize,
    pub opened_by_bots: usize,
    pub opened_first_time: usize,
    /// Pull requests with an `Assisted-by:` trailer or the `llm-assisted` label.
    pub opened_ai: usize,
    /// AI attribution that does not meet the policy's required form.
    pub opened_ai_other_form: usize,
    pub opened_security: usize,
    pub opened_new_package: usize,
    pub opened_backport: usize,
    pub opened_mass_rebuild: usize,
    pub authors: usize,
    /// Authors whose first pull request in the synced history falls in this month.
    pub new_authors: usize,
    pub merged: usize,
    pub merged_bot_authored: usize,
    pub closed_unmerged: usize,
    /// Open at the end of the month. A floor before the history walk's start.
    pub open_at_end: usize,
    pub reviews: usize,
    pub reviewers: usize,
    pub approvals: usize,
    pub mergers: usize,
    pub self_merged: usize,
    pub merged_without_approval: usize,
    /// Days from opening to merge, for human-authored pull requests merged this month.
    pub merge_days_p50: Option<f64>,
    pub merge_days_p90: Option<f64>,
    pub merge_days_bots_p50: Option<f64>,
    /// Hours to the first review by someone else, for human-authored pull
    /// requests opened this month that got one.
    pub first_review_hours_p50: Option<f64>,
    pub first_review_hours_p90: Option<f64>,
    /// Share of human-authored pull requests opened this month that had a
    /// review within seven days; null until the month is seven days past.
    pub reviewed_within_7d: Option<f64>,
    /// Of the pull requests opened this month and closed since, the share merged.
    pub merge_rate: Option<f64>,
    pub merge_rate_first_time: Option<f64>,
    pub merge_rate_ai: Option<f64>,
    pub still_open: usize,
    pub median_lines_changed: Option<f64>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Now {
    pub open: usize,
    pub drafts: usize,
    pub by_bots: usize,
    pub first_time: usize,
    pub ai: usize,
    pub security: usize,
    pub security_age_days_p50: Option<f64>,
    /// Human-authored, not a draft, and nobody else has reviewed it.
    pub never_reviewed: usize,
    pub needs_reviewer_label: usize,
    pub merge_bot_eligible_label: usize,
    pub age: BTreeMap<&'static str, usize>,
    pub base: BTreeMap<&'static str, usize>,
    pub age_days_p50: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AreaRow {
    pub area: String,
    pub open: usize,
    pub opened_365d: usize,
    pub merged_365d: usize,
    pub merge_days_p50_365d: Option<f64>,
    pub never_reviewed: usize,
}

/// How few people carry the work over the last 365 days: the smallest number
/// who together did half, or 80%, of it. No names.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Load {
    pub total: usize,
    pub people: usize,
    pub people_for_half: usize,
    pub people_for_80: usize,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Concentration {
    pub merges: Load,
    pub reviews: Load,
    pub authored: Load,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Report {
    pub generated_at: Option<DateTime<Utc>>,
    pub pull_requests: usize,
    pub earliest_created: Option<DateTime<Utc>>,
    pub now: Now,
    pub months: Vec<Month>,
    pub areas: Vec<AreaRow>,
    pub concentration_365d: Concentration,
    /// Disclosed AI tools on pull requests opened in the last 365 days.
    pub ai_tools_365d: Vec<(String, usize)>,
    /// Lower-case login → when its first pull request in the data opened.
    #[serde(skip)]
    pub first_pr: HashMap<String, DateTime<Utc>>,
}

fn percentile(v: &mut [f64], p: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let i = ((v.len() - 1) as f64 * p).round() as usize;
    Some((v[i] * 10.0).round() / 10.0)
}

fn share(n: usize, of: usize) -> Option<f64> {
    (of > 0).then(|| ((n as f64 / of as f64) * 1000.0).round() / 1000.0)
}

fn days(a: DateTime<Utc>, b: DateTime<Utc>) -> f64 {
    (b - a).num_minutes() as f64 / 1440.0
}

fn key(t: DateTime<Utc>) -> (i32, u32) {
    (t.year(), t.month())
}

fn month_end(y: i32, m: u32) -> DateTime<Utc> {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    Utc.with_ymd_and_hms(ny, nm, 1, 0, 0, 0).unwrap() - Duration::seconds(1)
}

fn load(counts: HashMap<&str, usize>) -> Load {
    let mut v: Vec<usize> = counts.into_values().collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    let total: usize = v.iter().sum();
    let needed = |frac: f64| {
        let mut acc = 0;
        v.iter()
            .take_while(|n| {
                let before = acc;
                acc += **n;
                (before as f64) < total as f64 * frac
            })
            .count()
    };
    Load {
        total,
        people: v.len(),
        people_for_half: needed(0.5),
        people_for_80: needed(0.8),
    }
}

#[derive(Default)]
struct Acc<'a> {
    m: Month,
    authors: HashSet<&'a str>,
    reviewers: HashSet<&'a str>,
    mergers: HashSet<&'a str>,
    merge_days: Vec<f64>,
    merge_days_bots: Vec<f64>,
    first_review: Vec<f64>,
    human_opened: usize,
    reviewed_7d: usize,
    closed: (usize, usize),
    closed_first_time: (usize, usize),
    closed_ai: (usize, usize),
    lines: Vec<f64>,
}

pub fn compute(facts: &[Facts], now: DateTime<Utc>) -> Report {
    let Some(start) = facts.iter().map(|f| f.created_at).min() else {
        return Report::default();
    };
    let mut acc: BTreeMap<(i32, u32), Acc> = BTreeMap::new();
    let (mut y, mut m) = key(start);
    while (y, m) <= key(now) {
        acc.insert((y, m), Acc::default());
        (y, m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    }

    let mut first_seen: HashMap<&str, DateTime<Utc>> = HashMap::new();
    for f in facts.iter().filter(|f| !f.author_is_bot) {
        let e = first_seen.entry(f.author.as_str()).or_insert(f.created_at);
        *e = (*e).min(f.created_at);
    }
    let mut first_pr: HashMap<String, DateTime<Utc>> = HashMap::new();
    for (login, t) in &first_seen {
        if let Some(a) = acc.get_mut(&key(*t)) {
            a.m.new_authors += 1;
        }
        let e = first_pr.entry(login.to_lowercase()).or_insert(*t);
        *e = (*e).min(*t);
    }

    for f in facts {
        if let Some(a) = acc.get_mut(&key(f.created_at)) {
            a.m.opened += 1;
            a.m.opened_by_bots += f.author_is_bot as usize;
            a.m.opened_first_time += f.first_time() as usize;
            a.m.opened_ai += f.ai.as_deref().is_some_and(|t| t != OTHER_FORM) as usize;
            a.m.opened_ai_other_form += (f.ai.as_deref() == Some(OTHER_FORM)) as usize;
            a.m.opened_security += f.security() as usize;
            a.m.opened_new_package += f.new_package() as usize;
            a.m.opened_backport += f.backport() as usize;
            a.m.opened_mass_rebuild += f.mass_rebuild() as usize;
            a.m.still_open += (f.state == "OPEN") as usize;
            a.authors.insert(&f.author);
            if f.additions + f.deletions > 0 {
                a.lines.push((f.additions + f.deletions) as f64);
            }
            if !f.author_is_bot {
                if let Some(r) = f.first_review() {
                    a.first_review
                        .push((r - f.created_at).num_minutes() as f64 / 60.0);
                }
                if now - f.created_at >= Duration::days(7) {
                    a.human_opened += 1;
                    a.reviewed_7d += f
                        .first_review()
                        .is_some_and(|r| r - f.created_at <= Duration::days(7))
                        as usize;
                }
            }
            if f.state != "OPEN" {
                let merged = f.merged_at.is_some() as usize;
                a.closed.0 += merged;
                a.closed.1 += 1;
                if f.first_time() {
                    a.closed_first_time.0 += merged;
                    a.closed_first_time.1 += 1;
                }
                if f.ai.is_some() {
                    a.closed_ai.0 += merged;
                    a.closed_ai.1 += 1;
                }
            }
        }
        if let Some(t) = f.merged_at {
            if let Some(a) = acc.get_mut(&key(t)) {
                a.m.merged += 1;
                if f.author_is_bot {
                    a.m.merged_bot_authored += 1;
                    a.merge_days_bots.push(days(f.created_at, t));
                } else {
                    a.merge_days.push(days(f.created_at, t));
                }
                if let Some(by) = &f.merged_by {
                    a.mergers.insert(by);
                    a.m.self_merged += (*by == f.author) as usize;
                }
                a.m.merged_without_approval += !f.approved() as usize;
            }
        } else if let Some(t) = f.closed_at {
            if let Some(a) = acc.get_mut(&key(t)) {
                a.m.closed_unmerged += 1;
            }
        }
        for r in &f.reviews {
            if let Some(a) = acc.get_mut(&key(r.at)) {
                a.m.reviews += 1;
                a.m.approvals += (r.state == "APPROVED") as usize;
                a.reviewers.insert(&r.by);
            }
        }
    }

    let ends: Vec<((i32, u32), DateTime<Utc>)> = acc
        .keys()
        .map(|&(y, m)| ((y, m), month_end(y, m).min(now)))
        .collect();
    for f in facts {
        for (k, end) in &ends {
            if *end < f.created_at {
                continue;
            }
            if f.closed_at.is_some_and(|c| c <= *end) {
                break;
            }
            if f.open_at(*end) {
                acc.get_mut(k).unwrap().m.open_at_end += 1;
            }
        }
    }

    let months = acc
        .into_iter()
        .map(|((y, m), mut a)| {
            a.m.month = format!("{y}-{m:02}");
            a.m.authors = a.authors.len();
            a.m.reviewers = a.reviewers.len();
            a.m.mergers = a.mergers.len();
            a.m.merge_days_p50 = percentile(&mut a.merge_days, 0.5);
            a.m.merge_days_p90 = percentile(&mut a.merge_days, 0.9);
            a.m.merge_days_bots_p50 = percentile(&mut a.merge_days_bots, 0.5);
            a.m.first_review_hours_p50 = percentile(&mut a.first_review, 0.5);
            a.m.first_review_hours_p90 = percentile(&mut a.first_review, 0.9);
            a.m.reviewed_within_7d = share(a.reviewed_7d, a.human_opened);
            a.m.merge_rate = share(a.closed.0, a.closed.1);
            a.m.merge_rate_first_time = share(a.closed_first_time.0, a.closed_first_time.1);
            a.m.merge_rate_ai = share(a.closed_ai.0, a.closed_ai.1);
            a.m.median_lines_changed = percentile(&mut a.lines, 0.5);
            a.m
        })
        .collect();

    let year_ago = now - Duration::days(365);
    Report {
        generated_at: Some(now),
        pull_requests: facts.len(),
        earliest_created: Some(start),
        now: snapshot(facts, now),
        months,
        areas: area_rows(facts, year_ago),
        concentration_365d: concentration(facts, year_ago),
        ai_tools_365d: ai_tools(facts, year_ago),
        first_pr,
    }
}

fn snapshot(facts: &[Facts], now: DateTime<Utc>) -> Now {
    let mut n = Now::default();
    let mut ages = Vec::new();
    let mut security_ages = Vec::new();
    for f in facts.iter().filter(|f| f.state == "OPEN") {
        let age = days(f.created_at, now);
        n.open += 1;
        n.drafts += f.is_draft as usize;
        n.by_bots += f.author_is_bot as usize;
        n.first_time += f.first_time() as usize;
        n.ai += f.ai.is_some() as usize;
        n.needs_reviewer_label += f.has("9.needs: reviewer") as usize;
        n.merge_bot_eligible_label += f.has("2.status: merge-bot eligible") as usize;
        if f.security() {
            n.security += 1;
            security_ages.push(age);
        }
        if !f.author_is_bot && !f.is_draft && f.reviews.is_empty() {
            n.never_reviewed += 1;
        }
        let bucket = match age {
            a if a < 7.0 => "1: under a week",
            a if a < 30.0 => "2: under a month",
            a if a < 90.0 => "3: under three months",
            a if a < 365.0 => "4: under a year",
            _ => "5: a year or more",
        };
        *n.age.entry(bucket).or_default() += 1;
        let base = match f.base.as_str() {
            "master" => "master",
            b if b.starts_with("staging") => "staging",
            b if b.starts_with("release-") => "release",
            _ => "other",
        };
        *n.base.entry(base).or_default() += 1;
        ages.push(age);
    }
    n.age_days_p50 = percentile(&mut ages, 0.5);
    n.security_age_days_p50 = percentile(&mut security_ages, 0.5);
    n
}

fn area_rows(facts: &[Facts], since: DateTime<Utc>) -> Vec<AreaRow> {
    let mut rows: BTreeMap<&str, (AreaRow, Vec<f64>)> = BTreeMap::new();
    for f in facts {
        for a in &f.areas {
            let (row, merge_days) = rows.entry(a).or_insert_with(|| {
                (
                    AreaRow {
                        area: a.clone(),
                        open: 0,
                        opened_365d: 0,
                        merged_365d: 0,
                        merge_days_p50_365d: None,
                        never_reviewed: 0,
                    },
                    Vec::new(),
                )
            });
            if f.state == "OPEN" {
                row.open += 1;
                if !f.author_is_bot && !f.is_draft && f.reviews.is_empty() {
                    row.never_reviewed += 1;
                }
            }
            row.opened_365d += (f.created_at >= since) as usize;
            if let Some(t) = f.merged_at.filter(|t| *t >= since) {
                row.merged_365d += 1;
                merge_days.push(days(f.created_at, t));
            }
        }
    }
    rows.into_values()
        .map(|(mut row, mut d)| {
            row.merge_days_p50_365d = percentile(&mut d, 0.5);
            row
        })
        .collect()
}

fn concentration(facts: &[Facts], since: DateTime<Utc>) -> Concentration {
    let mut merges: HashMap<&str, usize> = HashMap::new();
    let mut reviews: HashMap<&str, usize> = HashMap::new();
    let mut authored: HashMap<&str, usize> = HashMap::new();
    for f in facts {
        if let (Some(t), Some(by)) = (f.merged_at, &f.merged_by) {
            if t >= since {
                *merges.entry(by).or_default() += 1;
            }
        }
        for r in f.reviews.iter().filter(|r| r.at >= since) {
            *reviews.entry(&r.by).or_default() += 1;
        }
        if f.created_at >= since && !f.author_is_bot {
            *authored.entry(&f.author).or_default() += 1;
        }
    }
    Concentration {
        merges: load(merges),
        reviews: load(reviews),
        authored: load(authored),
    }
}

fn ai_tools(facts: &[Facts], since: DateTime<Utc>) -> Vec<(String, usize)> {
    let mut by: HashMap<&str, usize> = HashMap::new();
    for f in facts.iter().filter(|f| f.created_at >= since) {
        if let Some(t) = &f.ai {
            *by.entry(t).or_default() += 1;
        }
    }
    let mut v: Vec<(String, usize)> = by.into_iter().map(|(k, n)| (k.to_string(), n)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::tests::{pr, review, t};

    #[test]
    fn backlog_counts_what_was_open_at_month_end() {
        let mut a = pr(1, "alice", 0);
        a.state = "MERGED".into();
        a.merged_at = Some(t(40));
        a.merged_by = Some("carol".into());
        let b = pr(2, "bob", 5);
        let r = compute(&[Facts::from(&a), Facts::from(&b)], t(70));
        let june = r.months.iter().find(|m| m.month == "2026-06").unwrap();
        let july = r.months.iter().find(|m| m.month == "2026-07").unwrap();
        assert_eq!((june.opened, june.open_at_end), (2, 2));
        assert_eq!((july.merged, july.open_at_end), (1, 1));
        assert_eq!(july.merge_days_p50, Some(40.0));
        assert_eq!(july.merged_without_approval, 1);
        assert_eq!(r.now.open, 1);
    }

    #[test]
    fn first_review_and_merge_rate() {
        let mut a = pr(1, "alice", 0);
        a.reviews.push(review(10, "bob", 2));
        a.state = "CLOSED".into();
        a.closed_at = Some(t(3));
        a.association = "FIRST_TIME_CONTRIBUTOR".into();
        let r = compute(&[Facts::from(&a)], t(20));
        let june = &r.months[0];
        assert_eq!(june.first_review_hours_p50, Some(48.0));
        assert_eq!(june.reviewed_within_7d, Some(1.0));
        assert_eq!(june.merge_rate_first_time, Some(0.0));
        assert_eq!(june.closed_unmerged, 1);
    }

    #[test]
    fn load_counts_people_not_names() {
        let mut c = HashMap::new();
        c.insert("a", 6);
        c.insert("b", 3);
        c.insert("c", 1);
        let l = load(c);
        assert_eq!(
            (l.total, l.people, l.people_for_half, l.people_for_80),
            (10, 3, 1, 2)
        );
    }

    #[test]
    fn newest_source_wins() {
        let mut old = Facts::from(&pr(1, "alice", 0));
        old.state = "OPEN".into();
        let mut p = pr(1, "alice", 0);
        p.state = "MERGED".into();
        p.updated_at = t(9);
        assert_eq!(merge(vec![old], &[p])[0].state, "MERGED");
    }

    #[test]
    fn paths_map_to_areas() {
        let files = vec![
            "pkgs/by-name/he/hello/package.nix".to_string(),
            "nixos/modules/x.nix".to_string(),
            ".github/workflows/a.yml".to_string(),
        ];
        assert_eq!(
            areas(&files),
            vec!["ci", "nixos: modules", "packages: by-name"]
        );
    }
}
