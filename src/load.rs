//! What maintaining a package costs, and how that load spreads over the
//! people who maintain nixpkgs. A package or a module costs 1, plus the
//! commits of the last year on master that changed its file or directory.
//! The packages that share one file or directory share its commits, and
//! the people of a package share its cost.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, RwLock};
use std::time::Duration as StdDuration;

use crate::area::{Index, area_key};
use crate::metrics::{FEWEST_PEOPLE, SMALLEST_GROUP};
use crate::mirror::Mirror;
use crate::web::App;

pub const DAYS: i64 = 365;
const EVERY_HOURS: i64 = 24;
const RETRY_HOURS: i64 = 1;
/// One year of nixpkgs took 47 s on maintainermode.org.
const LIMIT: StdDuration = StdDuration::from_secs(900);

#[derive(Default)]
struct Changes {
    since: Option<DateTime<Utc>>,
    tried: Option<DateTime<Utc>>,
    by_key: HashMap<String, u32>,
}

static CHANGES: LazyLock<RwLock<Changes>> = LazyLock::new(Default::default);

struct Thing {
    key: Option<String>,
    people: Vec<String>,
}

fn people(index: &Index, maintainers: &[String], teams: &[String]) -> Vec<String> {
    let mut out: Vec<String> = maintainers
        .iter()
        .cloned()
        .chain(index.team_members(teams))
        .map(|m| m.to_lowercase())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// One package per package name, as on `/state`, then each module file.
fn things(index: &Index) -> Vec<Thing> {
    let pkgs = index
        .by_pname
        .values()
        .filter_map(|a| index.pkgs.get(a))
        .map(|p| Thing {
            key: p.file.as_deref().map(area_key),
            people: people(index, &p.maintainers, &p.teams),
        });
    let modules = index.modules.iter().map(|(file, m)| Thing {
        key: Some(file.clone()),
        people: people(index, &m.maintainers, &m.teams),
    });
    pkgs.chain(modules).collect()
}

/// Counts the commits again once a day, in the task of the git copy.
pub fn refresh(app: &App, m: &Mirror) {
    let now = Utc::now();
    {
        let c = CHANGES.read().unwrap();
        let every = if c.since.is_some() {
            EVERY_HOURS
        } else {
            RETRY_HOURS
        };
        if c.tried.is_some_and(|t| now - t < Duration::hours(every)) {
            return;
        }
    }
    let keys: HashSet<String> = things(&app.index.read().unwrap())
        .into_iter()
        .filter_map(|t| t.key)
        .collect();
    if keys.is_empty() {
        return;
    }
    CHANGES.write().unwrap().tried = Some(now);
    let since = now - Duration::days(DAYS);
    match m.touched("master", since, &keys, LIMIT) {
        Ok(by_key) => {
            {
                let mut c = CHANGES.write().unwrap();
                c.since = Some(since);
                c.by_key = by_key;
            }
            app.recompute_packages();
        }
        Err(e) => tracing::warn!("git copy: the commits of the last year: {e:#}"),
    }
}

/// A count of people below [`FEWEST_PEOPLE`], and a share of fewer than
/// [`SMALLEST_GROUP`] people, are null, as on the rest of `/state`.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct Bucket {
    /// Packages and modules per maintainer, such as `4-10`.
    pub(crate) maintains: &'static str,
    pub(crate) maintainers: Option<usize>,
    /// The part of the whole load that these maintainers carry, 0 to 1.
    pub(crate) load_share: Option<f64>,
}

/// How the packages and modules of nixpkgs spread over the people who
/// maintain them. Team members count as maintainers.
#[derive(Debug, Default, Serialize, PartialEq)]
pub(crate) struct Spread {
    /// The first day of the counted commits; None on a server without the
    /// git copy, where each package and module costs 1.
    pub(crate) since: Option<DateTime<Utc>>,
    pub(crate) maintainers: usize,
    /// Packages and modules with 0, 1, 2, and 3 or more maintainers.
    pub(crate) by_maintainers: [usize; 4],
    pub(crate) buckets: Vec<Bucket>,
    pub(crate) median_load: Option<f64>,
    /// The fewest maintainers that carry half of the load together.
    pub(crate) half_load_maintainers: Option<usize>,
    /// Never public: the load of one person is a score.
    #[serde(skip)]
    pub(crate) load: HashMap<String, f64>,
    #[serde(skip)]
    median: f64,
}

const BUCKETS: [(usize, usize, &str); 6] = [
    (1, 1, "1"),
    (2, 3, "2-3"),
    (4, 10, "4-10"),
    (11, 30, "11-30"),
    (31, 100, "31-100"),
    (101, usize::MAX, "101+"),
];

pub(crate) fn spread(index: &Index) -> Spread {
    let c = CHANGES.read().unwrap();
    spread_of(&things(index), c.since, &c.by_key)
}

fn spread_of(
    things: &[Thing],
    since: Option<DateTime<Utc>>,
    changes: &HashMap<String, u32>,
) -> Spread {
    let mut sharing: HashMap<&str, usize> = HashMap::new();
    for k in things.iter().filter_map(|t| t.key.as_deref()) {
        *sharing.entry(k).or_default() += 1;
    }
    let mut by_maintainers = [0; 4];
    let mut load: HashMap<String, f64> = HashMap::new();
    let mut count: HashMap<&str, usize> = HashMap::new();
    for t in things {
        by_maintainers[t.people.len().min(3)] += 1;
        if t.people.is_empty() {
            continue;
        }
        let commits = t
            .key
            .as_deref()
            .and_then(|k| Some(f64::from(*changes.get(k)?) / sharing[k] as f64))
            .unwrap_or(0.0);
        let share = (1.0 + commits) / t.people.len() as f64;
        for p in &t.people {
            *load.entry(p.clone()).or_default() += share;
            *count.entry(p).or_default() += 1;
        }
    }
    let total: f64 = load.values().sum();
    let mut loads: Vec<f64> = load.values().copied().collect();
    loads.sort_by(f64::total_cmp);
    let median = match loads.len() {
        0 => 0.0,
        n if n % 2 == 1 => loads[n / 2],
        n => (loads[n / 2 - 1] + loads[n / 2]) / 2.0,
    };
    let mut carried = 0.0;
    let half = loads
        .iter()
        .rev()
        .take_while(|l| {
            let before = carried;
            carried += *l;
            before < total / 2.0
        })
        .count();
    let buckets = BUCKETS
        .iter()
        .map(|&(lo, hi, maintains)| {
            let inside: Vec<&str> = count
                .iter()
                .filter(|(_, n)| (lo..=hi).contains(*n))
                .map(|(p, _)| *p)
                .collect();
            let carried: f64 = inside.iter().map(|p| load[*p]).sum();
            Bucket {
                maintains,
                maintainers: (inside.len() >= FEWEST_PEOPLE).then_some(inside.len()),
                load_share: (inside.len() >= SMALLEST_GROUP && total > 0.0)
                    .then(|| carried / total),
            }
        })
        .collect();
    Spread {
        since,
        maintainers: load.len(),
        by_maintainers,
        buckets,
        median_load: (loads.len() >= SMALLEST_GROUP).then_some((median * 10.0).round() / 10.0),
        half_load_maintainers: (half >= FEWEST_PEOPLE).then_some(half),
        load,
        median,
    }
}

/// The person maintains nothing, or carries less than the median load.
pub(crate) fn light(app: &App, login: &str) -> bool {
    let p = app.packages.read().unwrap().clone();
    let s = &p.maintainers;
    s.load
        .get(&login.to_lowercase())
        .is_none_or(|l| *l < s.median)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thing(key: &str, people: &[&str]) -> Thing {
        Thing {
            key: Some(key.into()),
            people: people.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn people_share_a_package_and_packages_share_a_file() {
        let things = [
            thing("pkgs/a", &["ann"]),
            thing("pkgs/b", &["ann", "bob"]),
            thing("pkgs/shared.nix", &["cat"]),
            thing("pkgs/shared.nix", &["cat"]),
            thing("pkgs/c", &[]),
        ];
        let changes: HashMap<String, u32> = [("pkgs/a", 9), ("pkgs/b", 3), ("pkgs/shared.nix", 4)]
            .map(|(k, n)| (k.to_string(), n))
            .into();
        let s = spread_of(&things, None, &changes);
        assert_eq!(s.maintainers, 3);
        assert_eq!(s.by_maintainers, [1, 3, 1, 0]);
        assert_eq!(s.load["ann"], 10.0 + 2.0);
        assert_eq!(s.load["bob"], 2.0);
        assert_eq!(s.load["cat"], 3.0 + 3.0);
        assert_eq!(s.median, 6.0);
        assert_eq!(s.median_load, None, "three people give no median");
        assert_eq!(s.half_load_maintainers, None, "one person is not shown");
        assert_eq!(s.buckets[1].maintainers, None);
    }

    #[test]
    fn large_groups_show_and_without_commits_each_package_costs_one() {
        let names: Vec<String> = (0..40).map(|i| format!("p{i}")).collect();
        let mut things: Vec<Thing> = names.iter().map(|n| thing(n, &[n])).collect();
        things.extend(names[..20].iter().map(|n| thing(&format!("{n}/x"), &[n])));
        let s = spread_of(&things, None, &HashMap::new());
        assert_eq!(s.load["p0"], 2.0);
        assert_eq!(s.load["p39"], 1.0);
        assert_eq!(s.median_load, Some(1.5));
        assert_eq!(s.half_load_maintainers, Some(15));
        assert_eq!(s.buckets[0].maintainers, Some(20));
        assert_eq!(s.buckets[0].load_share, Some(20.0 / 60.0));
        assert_eq!(s.buckets[1].load_share, Some(40.0 / 60.0));
        assert_eq!(s.buckets[2].maintainers, None);
        assert_eq!(spread_of(&[], None, &HashMap::new()).median, 0.0);
    }
}
