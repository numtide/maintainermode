//! The state of nixpkgs in numbers: inflow, outflow, backlog, review latency,
//! who carries the load and how much disclosed AI work arrives. Aggregates
//! only; no figure here names a person.

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::github::OTHER_FORM;
use crate::model::{Pr, UPDATE_BOT, automation};

/// One pull request reduced to what the metrics read. The history walk stores
/// these for pull requests outside the ledger's window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Facts {
    pub number: u64,
    #[serde(default)]
    pub title: String,
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
            title: p.title.clone(),
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
                .filter(|r| !r.bot())
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

/// The first month with GitHub reviews for the whole month; GitHub added
/// reviews on 2016-09-14. Review numbers before it are reviews added years
/// later.
pub const REVIEWS_FROM: (i32, u32) = (2016, 10);

/// Groups smaller than this give no share, and a count of people below
/// [`FEWEST_PEOPLE`] is not shown, so that no figure points at a person.
pub(crate) const SMALLEST_GROUP: usize = 20;
pub(crate) const FEWEST_PEOPLE: usize = 3;

impl Facts {
    pub fn bot(&self) -> bool {
        self.author_is_bot || automation(&self.author)
    }

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

    /// The label went missing on most new packages from 2025-01 to 2026-02;
    /// the title convention `<name>: init at <version>` did not.
    fn new_package(&self) -> bool {
        self.has("8.has: package (new)") || self.title.contains(": init at ")
    }

    fn bump(&self) -> bool {
        crate::bumps::parse(&self.title).is_some()
    }

    fn backport(&self) -> bool {
        self.base.starts_with("release-")
    }

    /// Any `10.rebuild-linux: <low>-<high>` or `<low>+` bucket from 501 up.
    fn mass_rebuild(&self) -> bool {
        self.labels.iter().any(|l| {
            l.strip_prefix("10.rebuild-linux: ").is_some_and(|b| {
                b.split(['-', '+'])
                    .next()
                    .and_then(|n| n.trim().parse::<u32>().ok())
                    .is_some_and(|n| n >= 501)
            })
        })
    }

    fn approved(&self) -> bool {
        self.reviews.iter().any(|r| r.state == "APPROVED")
    }

    fn first_review(&self) -> Option<DateTime<Utc>> {
        self.reviews.iter().map(|r| r.at).min()
    }

    /// A disclosure the policy accepts; other forms, such as a
    /// `Co-authored-by:` trailer, count apart.
    fn ai(&self) -> bool {
        self.ai.as_deref().is_some_and(|t| t != OTHER_FORM)
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Month {
    pub month: String,
    pub opened: usize,
    /// Bots, as [`automation`] and GitHub tell them.
    pub opened_by_bots: usize,
    pub opened_by_update_bot: usize,
    /// By people, with a title like `name: 1.0 -> 1.1`.
    pub opened_bumps_by_people: usize,
    pub opened_first_time: usize,
    /// Pull requests with an `Assisted-by:` trailer or the `llm-assisted` label.
    pub opened_ai: usize,
    /// AI attribution that does not meet the policy's required form.
    pub opened_ai_other_form: usize,
    pub opened_security: usize,
    pub opened_new_package: usize,
    pub opened_backport: usize,
    pub opened_mass_rebuild: usize,
    /// People who opened a pull request; bots are left out.
    pub authors: usize,
    /// Authors whose first pull request in the synced history falls in this month.
    pub new_authors: usize,
    pub merged: usize,
    pub merged_bot_authored: usize,
    /// Merges that a bot did, such as the merge bot for a maintainer.
    pub merged_by_bot: usize,
    pub closed_unmerged: usize,
    /// Merged or closed this month, more than 365 days after opening.
    pub closed_after_365d: usize,
    /// Open at the end of the month.
    pub open_at_end: usize,
    pub open_at_end_bots: usize,
    pub open_at_end_age: Ages,
    pub reviews: usize,
    pub reviewers: usize,
    pub approvals: usize,
    /// People who merged; bots are left out.
    pub mergers: usize,
    /// Merges per person who merged this month.
    pub merges_per_merger_p50: Option<f64>,
    pub merges_per_merger_p90: Option<f64>,
    /// The fewest people who did half of the merges, or of the reviews, in
    /// the 12 months that end with this one; null below three.
    pub merge_people_for_half_12m: Option<usize>,
    pub review_people_for_half_12m: Option<usize>,
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
    /// Of the human-authored pull requests opened this month, the share
    /// merged within 7, 30 and 90 days, closed with no merge within 90
    /// days, and still open after 365 days. The still-open ones count, so a
    /// recent month does not look faster than it is; each share is null
    /// until its days have passed for the whole month.
    pub merged_within_7d: Option<f64>,
    pub merged_within_30d: Option<f64>,
    pub merged_within_90d: Option<f64>,
    pub closed_within_90d: Option<f64>,
    pub open_after_365d: Option<f64>,
    /// Of the authors whose first pull request opened this month, the share
    /// whose first pull request merged within 90 days.
    pub new_authors_merged_within_90d: Option<f64>,
    /// Of those authors, the share who opened another pull request within
    /// 365 days of the first: when the first merged within 90 days, and
    /// when it did not.
    pub new_authors_back_within_365d_merged: Option<f64>,
    pub new_authors_back_within_365d_unmerged: Option<f64>,
}

/// Open pull requests by age: under 7 days, 7 to 30, 30 to 90, 90 to 365,
/// and 365 or more.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Ages {
    pub under_week: usize,
    pub under_month: usize,
    pub under_quarter: usize,
    pub under_year: usize,
    pub year_or_more: usize,
}

impl Ages {
    fn add(&mut self, days: f64) {
        match days {
            d if d < 7.0 => self.under_week += 1,
            d if d < 30.0 => self.under_month += 1,
            d if d < 90.0 => self.under_quarter += 1,
            d if d < 365.0 => self.under_year += 1,
            _ => self.year_or_more += 1,
        }
    }
}

/// What a kind of pull request becomes in its first 90 days.
#[derive(Debug, Clone, Serialize)]
pub struct KindRow {
    pub kind: &'static str,
    pub pull_requests: usize,
    pub merged_within_7d: f64,
    pub merged_within_90d: f64,
    pub closed_within_90d: f64,
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
    /// Of the human-authored pull requests opened from 395 to 30 days ago,
    /// the share merged within 30 days.
    pub merged_within_30d: Option<f64>,
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
    /// Kinds of pull requests opened in the 12 months that end 90 days or
    /// more ago; a kind with fewer than 20 is left out.
    pub kinds: Vec<KindRow>,
    pub kinds_from: Option<String>,
    pub kinds_to: Option<String>,
    /// The first month of `new_authors` and the newcomer shares: in the
    /// first year of the data, every author looks new.
    pub newcomers_from: Option<String>,
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

/// [`percentile`] and [`share`] of a month: null below [`SMALLEST_GROUP`].
fn sample(v: &mut [f64], p: f64) -> Option<f64> {
    (v.len() >= SMALLEST_GROUP)
        .then(|| percentile(v, p))
        .flatten()
}

fn rate(n: usize, of: usize) -> Option<f64> {
    (of >= SMALLEST_GROUP).then(|| share(n, of)).flatten()
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
    merges_by: HashMap<&'a str, usize>,
    reviews_by: HashMap<&'a str, usize>,
    merge_days: Vec<f64>,
    merge_days_bots: Vec<f64>,
    first_review: Vec<f64>,
    human_opened: usize,
    reviewed_7d: usize,
    closed: (usize, usize),
    closed_first_time: (usize, usize),
    closed_ai: (usize, usize),
    lines: Vec<f64>,
    cohort: Cohort,
    newcomers: Newcomers,
}

/// What became of the human-authored pull requests opened in one month.
#[derive(Default)]
struct Cohort {
    opened: usize,
    merged_7d: usize,
    merged_30d: usize,
    merged_90d: usize,
    closed_90d: usize,
    open_365d: usize,
}

#[derive(Default)]
struct Newcomers {
    merged_90d: usize,
    merged: (usize, usize),
    unmerged: (usize, usize),
}

fn within(f: &Facts, t: Option<DateTime<Utc>>, d: i64) -> bool {
    t.is_some_and(|t| t - f.created_at <= Duration::days(d))
}

fn merged_within(f: &Facts, d: i64) -> bool {
    within(f, f.merged_at, d)
}

fn closed_within(f: &Facts, d: i64) -> bool {
    f.merged_at.is_none() && within(f, f.closed_at, d)
}

/// `n` of `of` once `d` days have passed since `end`, the end of the month.
fn after(n: usize, of: usize, end: DateTime<Utc>, d: i64, now: DateTime<Utc>) -> Option<f64> {
    (end + Duration::days(d) <= now)
        .then(|| rate(n, of))
        .flatten()
}

fn people(l: Load) -> Option<usize> {
    (l.people_for_half >= FEWEST_PEOPLE).then_some(l.people_for_half)
}

fn summed(window: &[&HashMap<&str, usize>]) -> Load {
    let mut sum: HashMap<&str, usize> = HashMap::new();
    for m in window {
        for (who, n) in *m {
            *sum.entry(*who).or_default() += n;
        }
    }
    load(sum)
}

fn month_start(y: i32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, 1, 0, 0, 0).unwrap()
}

fn shift((y, m): (i32, u32), by: i32) -> (i32, u32) {
    let i = y * 12 + m as i32 - 1 + by;
    (i.div_euclid(12), i.rem_euclid(12) as u32 + 1)
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

    let mut by_author: HashMap<&str, (&Facts, Option<DateTime<Utc>>)> = HashMap::new();
    for f in facts.iter().filter(|f| !f.bot()) {
        let e = by_author.entry(f.author.as_str()).or_insert((f, None));
        if f.created_at < e.0.created_at {
            e.1 = Some(e.1.map_or(e.0.created_at, |s| s.min(e.0.created_at)));
            e.0 = f;
        } else if f.number != e.0.number {
            e.1 = Some(e.1.map_or(f.created_at, |s| s.min(f.created_at)));
        }
    }
    let mut first_pr: HashMap<String, DateTime<Utc>> = HashMap::new();
    let mut firsts: HashSet<u64> = HashSet::new();
    for (login, (first, second)) in &by_author {
        firsts.insert(first.number);
        if let Some(a) = acc.get_mut(&key(first.created_at)) {
            a.m.new_authors += 1;
            let merged = merged_within(first, 90);
            let back = second.is_some_and(|s| s - first.created_at <= Duration::days(365));
            a.newcomers.merged_90d += merged as usize;
            let group = if merged {
                &mut a.newcomers.merged
            } else {
                &mut a.newcomers.unmerged
            };
            group.0 += back as usize;
            group.1 += 1;
        }
        let e = first_pr
            .entry(login.to_lowercase())
            .or_insert(first.created_at);
        *e = (*e).min(first.created_at);
    }

    for f in facts {
        let bot = f.bot();
        if let Some(a) = acc.get_mut(&key(f.created_at)) {
            a.m.opened += 1;
            a.m.opened_by_bots += bot as usize;
            a.m.opened_by_update_bot += f.author.eq_ignore_ascii_case(UPDATE_BOT) as usize;
            a.m.opened_bumps_by_people += (!bot && f.bump()) as usize;
            a.m.opened_first_time += f.first_time() as usize;
            a.m.opened_ai += f.ai() as usize;
            a.m.opened_ai_other_form += (f.ai.as_deref() == Some(OTHER_FORM)) as usize;
            a.m.opened_security += f.security() as usize;
            a.m.opened_new_package += f.new_package() as usize;
            a.m.opened_backport += f.backport() as usize;
            a.m.opened_mass_rebuild += f.mass_rebuild() as usize;
            a.m.still_open += (f.state == "OPEN") as usize;
            if !bot {
                a.authors.insert(&f.author);
            }
            if f.additions + f.deletions > 0 {
                a.lines.push((f.additions + f.deletions) as f64);
            }
            if !bot {
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
                let c = &mut a.cohort;
                c.opened += 1;
                c.merged_7d += merged_within(f, 7) as usize;
                c.merged_30d += merged_within(f, 30) as usize;
                c.merged_90d += merged_within(f, 90) as usize;
                c.closed_90d += closed_within(f, 90) as usize;
                c.open_365d += !within(f, f.closed_at, 365) as usize;
            }
            if f.state != "OPEN" {
                let merged = f.merged_at.is_some() as usize;
                a.closed.0 += merged;
                a.closed.1 += 1;
                if f.first_time() {
                    a.closed_first_time.0 += merged;
                    a.closed_first_time.1 += 1;
                }
                if f.ai() {
                    a.closed_ai.0 += merged;
                    a.closed_ai.1 += 1;
                }
            }
        }
        if let Some(t) = f.closed_at.filter(|_| !within(f, f.closed_at, 365)) {
            if let Some(a) = acc.get_mut(&key(t)) {
                a.m.closed_after_365d += 1;
            }
        }
        if let Some(t) = f.merged_at {
            if let Some(a) = acc.get_mut(&key(t)) {
                a.m.merged += 1;
                if bot {
                    a.m.merged_bot_authored += 1;
                    a.merge_days_bots.push(days(f.created_at, t));
                } else {
                    a.merge_days.push(days(f.created_at, t));
                }
                match f.merged_by.as_deref() {
                    Some(by) if automation(by) => a.m.merged_by_bot += 1,
                    Some(by) => {
                        *a.merges_by.entry(by).or_default() += 1;
                        a.m.self_merged += (by == f.author) as usize;
                    }
                    None => {}
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
                *a.reviews_by.entry(&r.by).or_default() += 1;
            }
        }
    }

    let ends: Vec<((i32, u32), DateTime<Utc>)> = acc
        .keys()
        .map(|&(y, m)| ((y, m), month_end(y, m).min(now)))
        .collect();
    for f in facts {
        let bot = f.bot();
        for (k, end) in &ends {
            if *end < f.created_at {
                continue;
            }
            if f.closed_at.is_some_and(|c| c <= *end) {
                break;
            }
            let m = &mut acc.get_mut(k).unwrap().m;
            m.open_at_end += 1;
            m.open_at_end_bots += bot as usize;
            m.open_at_end_age.add(days(f.created_at, *end));
        }
    }

    let keys: Vec<(i32, u32)> = acc.keys().copied().collect();
    let merges: Vec<&HashMap<&str, usize>> = acc.values().map(|a| &a.merges_by).collect();
    let reviews: Vec<&HashMap<&str, usize>> = acc.values().map(|a| &a.reviews_by).collect();
    let mut people_12m: Vec<(Option<usize>, Option<usize>)> = Vec::new();
    for (i, k) in keys.iter().enumerate() {
        if i + 1 < 12 || month_end(k.0, k.1) > now {
            people_12m.push((None, None));
            continue;
        }
        let window = i + 1 - 12..=i;
        people_12m.push((
            people(summed(&merges[window.clone()])),
            (keys[i + 1 - 12] >= REVIEWS_FROM)
                .then(|| people(summed(&reviews[window])))
                .flatten(),
        ));
    }

    let newcomers_from = shift(key(start), 12);
    let months = acc
        .into_iter()
        .zip(people_12m)
        .map(|(((y, m), mut a), (merge_people, review_people))| {
            let end = month_end(y, m);
            let reviews_on = (y, m) >= REVIEWS_FROM;
            a.m.month = format!("{y}-{m:02}");
            a.m.authors = a.authors.len();
            a.m.reviewers = a.reviewers.len();
            a.m.mergers = a.merges_by.len();
            let mut per: Vec<f64> = a.merges_by.values().map(|n| *n as f64).collect();
            a.m.merges_per_merger_p50 = sample(&mut per, 0.5);
            a.m.merges_per_merger_p90 = sample(&mut per, 0.9);
            a.m.merge_people_for_half_12m = merge_people;
            a.m.review_people_for_half_12m = review_people;
            a.m.merge_days_p50 = sample(&mut a.merge_days, 0.5);
            a.m.merge_days_p90 = sample(&mut a.merge_days, 0.9);
            a.m.merge_days_bots_p50 = sample(&mut a.merge_days_bots, 0.5);
            if reviews_on {
                a.m.first_review_hours_p50 = sample(&mut a.first_review, 0.5);
                a.m.first_review_hours_p90 = sample(&mut a.first_review, 0.9);
                a.m.reviewed_within_7d = rate(a.reviewed_7d, a.human_opened);
            }
            a.m.merge_rate = rate(a.closed.0, a.closed.1);
            a.m.merge_rate_first_time = rate(a.closed_first_time.0, a.closed_first_time.1);
            a.m.merge_rate_ai = rate(a.closed_ai.0, a.closed_ai.1);
            a.m.median_lines_changed = sample(&mut a.lines, 0.5);
            let c = &a.cohort;
            a.m.merged_within_7d = after(c.merged_7d, c.opened, end, 7, now);
            a.m.merged_within_30d = after(c.merged_30d, c.opened, end, 30, now);
            a.m.merged_within_90d = after(c.merged_90d, c.opened, end, 90, now);
            a.m.closed_within_90d = after(c.closed_90d, c.opened, end, 90, now);
            a.m.open_after_365d = after(c.open_365d, c.opened, end, 365, now);
            if (y, m) >= newcomers_from {
                let n = &a.newcomers;
                a.m.new_authors_merged_within_90d =
                    after(n.merged_90d, a.m.new_authors, end, 90, now);
                a.m.new_authors_back_within_365d_merged =
                    after(n.merged.0, n.merged.1, end, 365, now);
                a.m.new_authors_back_within_365d_unmerged =
                    after(n.unmerged.0, n.unmerged.1, end, 365, now);
            }
            a.m
        })
        .collect();

    let year_ago = now - Duration::days(365);
    let (kinds, kinds_from, kinds_to) = kinds(facts, &firsts, now);
    Report {
        generated_at: Some(now),
        pull_requests: facts.len(),
        earliest_created: Some(start),
        now: snapshot(facts, now),
        months,
        areas: area_rows(facts, year_ago, now),
        concentration_365d: concentration(facts, year_ago),
        ai_tools_365d: ai_tools(facts, year_ago),
        kinds,
        kinds_from,
        kinds_to,
        newcomers_from: Some(format!("{}-{:02}", newcomers_from.0, newcomers_from.1)),
        first_pr,
    }
}

type Kind = (&'static str, fn(&Facts, bool) -> bool);

/// The second argument says whether the pull request is its author's first.
const KINDS: [Kind; 15] = [
    ("all by people", |f, _| !f.bot()),
    ("the first of a new author", |f, first| !f.bot() && first),
    ("from the update bot", |f, _| {
        f.author.eq_ignore_ascii_case(UPDATE_BOT)
    }),
    ("version update by a person", |f, _| !f.bot() && f.bump()),
    ("new package", |f, _| !f.bot() && f.new_package()),
    ("NixOS module, no package", |f, _| {
        !f.bot()
            && f.areas.iter().any(|a| a == "nixos: modules")
            && !f.areas.iter().any(|a| a.starts_with("packages"))
    }),
    ("security fix", |f, _| !f.bot() && f.security()),
    ("backport", |f, _| !f.bot() && f.backport()),
    ("documentation only", |f, _| !f.bot() && f.areas == ["doc"]),
    ("lib or stdenv", |f, _| {
        !f.bot() && f.areas.iter().any(|a| a == "lib" || a == "stdenv")
    }),
    ("mass rebuild", |f, _| !f.bot() && f.mass_rebuild()),
    ("up to 10 lines", |f, _| {
        !f.bot() && f.additions + f.deletions <= 10
    }),
    ("11 to 100 lines", |f, _| {
        !f.bot() && (11..=100).contains(&(f.additions + f.deletions))
    }),
    ("101 to 1,000 lines", |f, _| {
        !f.bot() && (101..=1000).contains(&(f.additions + f.deletions))
    }),
    ("over 1,000 lines", |f, _| {
        !f.bot() && f.additions + f.deletions > 1000
    }),
];

/// [`KINDS`] over the pull requests opened in the 12 months that end 90 days
/// or more before `now`, so that every one has had its 90 days.
fn kinds(
    facts: &[Facts],
    firsts: &HashSet<u64>,
    now: DateTime<Utc>,
) -> (Vec<KindRow>, Option<String>, Option<String>) {
    let t = now - Duration::days(90);
    let last = if month_end(key(t).0, key(t).1) <= t {
        key(t)
    } else {
        shift(key(t), -1)
    };
    let first = shift(last, -11);
    let (from, end) = (month_start(first.0, first.1), month_end(last.0, last.1));
    let window: Vec<&Facts> = facts
        .iter()
        .filter(|f| f.created_at >= from && f.created_at <= end)
        .collect();
    let rows = KINDS
        .iter()
        .filter_map(|(kind, is)| {
            let of: Vec<&&Facts> = window
                .iter()
                .filter(|f| is(f, firsts.contains(&f.number)))
                .collect();
            let n = of.len();
            let part =
                |p: fn(&Facts) -> bool| share(of.iter().filter(|f| p(f)).count(), n).unwrap_or(0.0);
            (n >= SMALLEST_GROUP).then(|| KindRow {
                kind,
                pull_requests: n,
                merged_within_7d: part(|f| merged_within(f, 7)),
                merged_within_90d: part(|f| merged_within(f, 90)),
                closed_within_90d: part(|f| closed_within(f, 90)),
            })
        })
        .collect();
    (
        rows,
        Some(from.format("%Y-%m").to_string()),
        Some(end.format("%Y-%m").to_string()),
    )
}

fn snapshot(facts: &[Facts], now: DateTime<Utc>) -> Now {
    let mut n = Now::default();
    let mut ages = Vec::new();
    let mut security_ages = Vec::new();
    for f in facts.iter().filter(|f| f.state == "OPEN") {
        let age = days(f.created_at, now);
        n.open += 1;
        n.drafts += f.is_draft as usize;
        n.by_bots += f.bot() as usize;
        n.first_time += f.first_time() as usize;
        n.ai += f.ai() as usize;
        n.needs_reviewer_label += f.has("9.needs: reviewer") as usize;
        n.merge_bot_eligible_label += f.has("2.status: merge-bot eligible") as usize;
        if f.security() {
            n.security += 1;
            security_ages.push(age);
        }
        if !f.bot() && !f.is_draft && f.reviews.is_empty() {
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

/// A row, the days to merge, and (merged within 30 days, of).
type AreaAcc = (AreaRow, Vec<f64>, (usize, usize));

fn area_rows(facts: &[Facts], since: DateTime<Utc>, now: DateTime<Utc>) -> Vec<AreaRow> {
    let cohort = (since - Duration::days(30), now - Duration::days(30));
    let mut rows: BTreeMap<&str, AreaAcc> = BTreeMap::new();
    for f in facts {
        for a in &f.areas {
            let (row, merge_days, merged_30d) = rows.entry(a).or_insert_with(|| {
                (
                    AreaRow {
                        area: a.clone(),
                        open: 0,
                        opened_365d: 0,
                        merged_365d: 0,
                        merge_days_p50_365d: None,
                        merged_within_30d: None,
                        never_reviewed: 0,
                    },
                    Vec::new(),
                    (0, 0),
                )
            });
            if f.state == "OPEN" {
                row.open += 1;
                if !f.bot() && !f.is_draft && f.reviews.is_empty() {
                    row.never_reviewed += 1;
                }
            }
            row.opened_365d += (f.created_at >= since) as usize;
            if let Some(t) = f.merged_at.filter(|t| *t >= since) {
                row.merged_365d += 1;
                merge_days.push(days(f.created_at, t));
            }
            if !f.bot() && f.created_at >= cohort.0 && f.created_at < cohort.1 {
                merged_30d.0 += merged_within(f, 30) as usize;
                merged_30d.1 += 1;
            }
        }
    }
    rows.into_values()
        .map(|(mut row, mut d, (k, of))| {
            row.merge_days_p50_365d = percentile(&mut d, 0.5);
            row.merged_within_30d = rate(k, of);
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
            if t >= since && !automation(by) {
                *merges.entry(by).or_default() += 1;
            }
        }
        for r in f.reviews.iter().filter(|r| r.at >= since) {
            *reviews.entry(&r.by).or_default() += 1;
        }
        if f.created_at >= since && !f.bot() {
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
        assert_eq!(
            july.merge_days_p50, None,
            "one merge is too few for a median"
        );
        assert_eq!(july.merged_without_approval, 1);
        assert_eq!(r.now.open, 1);
    }

    #[test]
    fn first_review_and_merge_rate() {
        let f: Vec<Facts> = (0..20)
            .map(|i| {
                let mut a = pr(i, &format!("a{i}"), 0);
                a.reviews.push(review(10, "bob", 2));
                a.state = "CLOSED".into();
                a.closed_at = Some(t(3));
                a.association = "FIRST_TIME_CONTRIBUTOR".into();
                Facts::from(&a)
            })
            .collect();
        let june = &compute(&f, t(20)).months[0];
        assert_eq!(june.first_review_hours_p50, Some(48.0));
        assert_eq!(june.reviewed_within_7d, Some(1.0));
        assert_eq!(june.merge_rate_first_time, Some(0.0));
        assert_eq!(june.closed_unmerged, 20);
        let one = compute(&f[..1], t(20));
        assert_eq!(one.months[0].reviewed_within_7d, None, "one is too few");
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

    fn merged(n: u64, author: &str, created: i64, days: i64, by: &str) -> Facts {
        let mut p = pr(n, author, created);
        p.state = "MERGED".into();
        p.merged_at = Some(t(created + days));
        p.merged_by = Some(by.into());
        Facts::from(&p)
    }

    fn june(r: &Report) -> &Month {
        r.months.iter().find(|m| m.month == "2026-06").unwrap()
    }

    #[test]
    fn the_update_bot_and_the_merge_bot_are_bots() {
        let mut f: Vec<Facts> = (0..5)
            .map(|i| merged(i, UPDATE_BOT, 1, 1, "nixpkgs-merge-bot"))
            .collect();
        f.push(merged(9, "alice", 1, 1, "carol"));
        let m = june(&compute(&f, t(60))).clone();
        assert_eq!(
            (m.opened_by_bots, m.opened_by_update_bot, m.authors),
            (5, 5, 1)
        );
        assert_eq!((m.merged_by_bot, m.mergers), (5, 1));
    }

    #[test]
    fn a_month_counts_its_open_pull_requests_once_its_days_passed() {
        let mut f = Vec::new();
        for i in 0..30 {
            let a = format!("a{i}");
            f.push(match i {
                0..15 => merged(i, &a, 1, 5, "carol"),
                15..20 => merged(i, &a, 1, 40, "carol"),
                _ => Facts::from(&pr(i, &a, 1)),
            });
        }
        let early = compute(&f, t(20));
        assert_eq!(june(&early).merged_within_7d, None);
        let r = compute(&f, t(200));
        let m = june(&r);
        assert_eq!(m.merged_within_7d, Some(0.5));
        assert_eq!(m.merged_within_90d, Some(0.667));
        assert_eq!(m.open_after_365d, None);
        assert_eq!(m.open_at_end_age.under_month, 15);
        assert_eq!(r.months.last().unwrap().open_at_end_age.under_year, 10);
    }

    #[test]
    fn new_authors_come_back_by_whether_their_first_merged() {
        let mut f = vec![Facts::from(&pr(1, "old", -400))];
        for i in 0..40u64 {
            let a = format!("n{i}");
            f.push(if i < 20 {
                merged(10 + i, &a, 1, 3, "carol")
            } else {
                Facts::from(&pr(10 + i, &a, 1))
            });
            if i % 2 == 0 && i < 20 || i % 4 == 0 && i >= 20 {
                f.push(Facts::from(&pr(100 + i, &a, 200)));
            }
        }
        let r = compute(&f, t(500));
        let m = june(&r);
        assert_eq!(m.new_authors, 40);
        assert_eq!(m.new_authors_merged_within_90d, Some(0.5));
        assert_eq!(m.new_authors_back_within_365d_merged, Some(0.5));
        assert_eq!(m.new_authors_back_within_365d_unmerged, Some(0.25));
        let first = r.months.first().unwrap();
        assert!(first.month < *r.newcomers_from.as_ref().unwrap());
        assert_eq!(first.new_authors_merged_within_90d, None);
    }

    #[test]
    fn kinds_take_twelve_months_that_had_90_days() {
        let f = vec![Facts::from(&pr(1, "alice", 0))];
        let r = compute(&f, "2026-12-15T00:00:00Z".parse().unwrap());
        assert_eq!(r.kinds_from.as_deref(), Some("2025-09"));
        assert_eq!(r.kinds_to.as_deref(), Some("2026-08"));
        assert!(r.kinds.is_empty(), "a kind with fewer than 20 is left out");
    }

    #[test]
    fn labels_and_titles_give_the_kind() {
        let mut p = pr(1, "alice", 0);
        p.labels = vec!["10.rebuild-linux: 1001-2500".into()];
        p.title = "hello: init at 1.0".into();
        let f = Facts::from(&p);
        assert!(f.mass_rebuild() && f.new_package() && !f.bump());
        p.labels = vec!["10.rebuild-linux: 101-500".into()];
        p.title = "hello: 1.0 -> 1.1".into();
        let f = Facts::from(&p);
        assert!(!f.mass_rebuild() && !f.new_package() && f.bump());
    }

    #[test]
    fn fewer_than_three_people_is_not_shown() {
        let f: Vec<Facts> = (0..40)
            .map(|i| merged(i, &format!("a{i}"), i as i64 * 10 - 400, 1, "carol"))
            .collect();
        let r = compute(&f, t(60));
        assert!(
            r.months
                .iter()
                .all(|m| m.merge_people_for_half_12m.is_none())
        );
    }
}
