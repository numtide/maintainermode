//! What a query means for packages: `is:outdated`, `-has:maintainer`,
//! `in:python3Packages`, `by:login`, `team:name`, `sort:az`. The words are
//! nixkeeper's where the meaning is the same, so one query works on both
//! sites, and `is:unmaintained` is also `-has:maintainer`. A word this
//! site does not know is a [`Problem`], shown under the box; the rest of
//! the query still applies.
//!
//! [`Traits`] is what the filter reads of a package: the facts of the
//! health sources, made once per index and health refresh for every
//! package, so a query over all of nixpkgs scans a map in memory.

use std::collections::{HashMap, HashSet};

use crate::affected::Verdict;
use crate::area::{Index, Package};
use crate::health::{Advisory, Health};
use crate::query::{self, Problem, Term};
use crate::score;
use crate::severity::Band;
use crate::web::Snapshot;

/// `is:` words, each one trait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Is {
    Outdated,
    Vulnerable,
    Backport,
    Broken,
    Insecure,
    Unfree,
    Unmaintained,
    Failed,
    Blocked,
}

impl Is {
    pub(crate) const ALL: [Is; 9] = [
        Is::Outdated,
        Is::Vulnerable,
        Is::Backport,
        Is::Broken,
        Is::Insecure,
        Is::Unfree,
        Is::Unmaintained,
        Is::Failed,
        Is::Blocked,
    ];

    pub(crate) fn word(self) -> &'static str {
        match self {
            Is::Outdated => "outdated",
            Is::Vulnerable => "vulnerable",
            Is::Backport => "backport",
            Is::Broken => "broken",
            Is::Insecure => "insecure",
            Is::Unfree => "unfree",
            Is::Unmaintained => "unmaintained",
            Is::Failed => "failed",
            Is::Blocked => "blocked",
        }
    }

    pub(crate) fn about(self) -> &'static str {
        match self {
            Is::Outdated => "upstream released a newer version than nixpkgs has",
            Is::Vulnerable => "an open advisory or an OSV match can affect master",
            Is::Backport => {
                "master fixes its advisories, a channel still carries an affected version"
            }
            Is::Broken => "meta.broken",
            Is::Insecure => "meta.knownVulnerabilities is set",
            Is::Unfree => "an unfree license",
            Is::Unmaintained => "no meta.maintainers and no meta.teams; also -has:maintainer",
            Is::Failed => {
                "its last Hydra build failed, where the server knows one: the packages people maintain or watch, and the release jobset of Zero Hydra Failures"
            }
            Is::Blocked => {
                "Hydra did not build it because a dependency failed, on the release jobset of Zero Hydra Failures"
            }
        }
    }

    fn parse(v: &str) -> Option<Is> {
        Some(match v {
            "outdated" => Is::Outdated,
            "vulnerable" => Is::Vulnerable,
            "backport" => Is::Backport,
            "broken" => Is::Broken,
            "insecure" => Is::Insecure,
            "unfree" => Is::Unfree,
            "unmaintained" => Is::Unmaintained,
            // nixkeeper's: "failed" is a build or an update failure there,
            // "builds" a build failure; here both are the build.
            "failed" | "builds" => Is::Failed,
            "blocked" => Is::Blocked,
            _ => return None,
        })
    }
}

/// `has:` words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Has {
    Maintainer,
    Team,
    Pr,
    Issue,
    Gaps,
    Advisory,
    Newer,
}

impl Has {
    pub(crate) const ALL: [Has; 7] = [
        Has::Maintainer,
        Has::Team,
        Has::Pr,
        Has::Issue,
        Has::Gaps,
        Has::Advisory,
        Has::Newer,
    ];

    pub(crate) fn word(self) -> &'static str {
        match self {
            Has::Maintainer => "maintainer",
            Has::Team => "team",
            Has::Pr => "pr",
            Has::Issue => "issue",
            Has::Gaps => "gaps",
            Has::Advisory => "advisory",
            Has::Newer => "newer",
        }
    }

    pub(crate) fn about(self) -> &'static str {
        match self {
            Has::Maintainer => "someone in meta.maintainers",
            Has::Team => "a team in meta.teams",
            Has::Pr => "an open pull request touches it",
            Has::Issue => "an open issue names it",
            Has::Gaps => "packaging gaps: the Packaging row of its page",
            Has::Advisory => "the same as is:vulnerable",
            Has::Newer => "the same as is:outdated",
        }
    }

    fn parse(v: &str) -> Option<Has> {
        Some(match v {
            "maintainer" => Has::Maintainer,
            "team" => Has::Team,
            "pr" => Has::Pr,
            "issue" | "issues" => Has::Issue,
            "gaps" | "gap" => Has::Gaps,
            "advisory" | "advisories" => Has::Advisory,
            "newer" => Has::Newer,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Sort {
    /// The highest risk first, as the lists of the site.
    #[default]
    Pressing,
    Az,
}

impl Sort {
    const WORDS: [&'static str; 2] = ["pressing", "az"];
}

/// The keys a query can hold here. The ones of nixkeeper that this site
/// has no data for yet (`age:`, `on:`, `build:`, `update:`, `status:`,
/// `ofteam:`) are named in the message, so a person knows it is not a typo.
const KEYS: [&str; 8] = [
    "is",
    "has",
    "in",
    "by",
    "team",
    "maintainer",
    "sort",
    "order",
];
const NOT_YET: [&str; 6] = ["age", "on", "build", "update", "status", "ofteam"];

/// A query, read for packages.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Filter {
    pub is: Vec<(bool, Is)>,
    pub has: Vec<(bool, Has)>,
    /// A package set: `python3Packages`, or `top-level` for an attribute
    /// with no dot.
    pub in_set: Option<String>,
    /// Maintainers it must name, lowercase.
    pub by: Vec<String>,
    /// Maintainers it must not name, lowercase.
    pub not_by: Vec<String>,
    pub team: Option<String>,
    pub not_team: Vec<String>,
    /// `team:none`: no team.
    pub no_team: bool,
    pub sort: Sort,
    pub reverse: bool,
    /// The words of a name, lowercase.
    pub text: String,
    pub problems: Vec<Problem>,
}

impl Filter {
    pub(crate) fn parse(q: &str) -> Filter {
        let query = query::parse(q);
        let mut f = Filter {
            text: query.text.to_lowercase(),
            ..Default::default()
        };
        for t in &query.terms {
            f.read(t);
        }
        f
    }

    /// Only text: the query has no qualifier that this site knows.
    pub(crate) fn is_plain(&self) -> bool {
        self.is.is_empty()
            && self.has.is_empty()
            && self.in_set.is_none()
            && self.by.is_empty()
            && self.not_by.is_empty()
            && self.team.is_none()
            && self.not_team.is_empty()
            && !self.no_team
            && self.sort == Sort::Pressing
            && !self.reverse
    }

    /// The filter asks about Hydra, so the sets of failing builds are needed.
    pub(crate) fn asks_hydra(&self) -> bool {
        self.is
            .iter()
            .any(|(_, i)| matches!(i, Is::Failed | Is::Blocked))
    }

    fn read(&mut self, t: &Term) {
        let key = t.key.as_str();
        if NOT_YET.contains(&key) {
            self.problems
                .push(Problem::new(&t.raw, format!("{key}: is not here yet")));
            return;
        }
        if !KEYS.contains(&key) {
            self.problems
                .push(Problem::of_key(t, format!("no {key}: qualifier"), KEYS));
            return;
        }
        // A key still waiting for its value: nothing to say.
        if t.value.is_empty() {
            return;
        }
        let v = t.value.as_str();
        match key {
            "is" | "has" => {
                let known = match key {
                    "is" => Is::parse(&v.to_lowercase()).map(|i| self.is.push((t.neg, i))),
                    _ => Has::parse(&v.to_lowercase()).map(|h| self.has.push((t.neg, h))),
                };
                if known.is_none() {
                    let word = t.word();
                    let sign = if t.neg { "-" } else { "" };
                    let words: Vec<String> = Is::ALL
                        .iter()
                        .map(|i| format!("{sign}is:{}", i.word()))
                        .chain(Has::ALL.iter().map(|h| format!("{sign}has:{}", h.word())))
                        .collect();
                    let p = Problem::new(&t.raw, format!("no {word}"))
                        .suggest(&word, words.iter().map(String::as_str));
                    self.problems.push(p);
                }
            }
            "in" => {
                if t.neg {
                    self.problems
                        .push(Problem::new(&t.raw, "-in: leaves nothing out"));
                } else if valid_set(v) {
                    self.in_set = Some(v.to_string());
                } else {
                    self.problems
                        .push(Problem::new(&t.raw, format!("no in:{v}")));
                }
            }
            "by" | "maintainer" => {
                let login = v.trim_start_matches('@').to_lowercase();
                if !crate::catalog::valid_login(&login) {
                    self.problems
                        .push(Problem::new(&t.raw, format!("no {key}:{v}")));
                } else if t.neg {
                    self.not_by.push(login);
                } else {
                    self.by.push(login);
                }
            }
            "team" => {
                if v.eq_ignore_ascii_case("none") {
                    if t.neg {
                        self.problems
                            .push(Problem::new(&t.raw, "-team:none leaves nothing out"));
                    } else {
                        self.no_team = true;
                    }
                } else if t.neg {
                    self.not_team.push(v.to_string());
                } else {
                    self.team = Some(v.to_string());
                }
            }
            "sort" => match v.to_lowercase().as_str() {
                "pressing" => self.sort = Sort::Pressing,
                "az" => self.sort = Sort::Az,
                other => self.problems.push(Problem::of_value(
                    t,
                    format!("no sort:{other} here"),
                    Sort::WORDS,
                )),
            },
            "order" => {
                if v.eq_ignore_ascii_case("reverse") {
                    self.reverse = true;
                } else {
                    self.problems.push(Problem::of_value(
                        t,
                        format!("order:{v}: only order:reverse"),
                        ["reverse"],
                    ));
                }
            }
            _ => {}
        }
    }

    /// Whether a package with these traits stays in the list. `failing`
    /// says what Hydra knows, when the filter asks ([`Filter::asks_hydra`]).
    pub(crate) fn keeps(
        &self,
        attr: &str,
        p: &Package,
        t: &Traits,
        failing: Option<&Failing>,
    ) -> bool {
        if !self.text.is_empty()
            && !attr.to_lowercase().contains(&self.text)
            && !p.pname.to_lowercase().contains(&self.text)
        {
            return false;
        }
        for &(neg, i) in &self.is {
            let on = match i {
                Is::Outdated => t.outdated,
                Is::Vulnerable => t.vulnerable,
                Is::Backport => t.backport,
                Is::Broken => t.broken,
                Is::Insecure => t.insecure,
                Is::Unfree => t.unfree,
                Is::Unmaintained => t.unmaintained,
                Is::Failed => failing.is_some_and(|f| f.failed.contains(attr)),
                Is::Blocked => failing.is_some_and(|f| f.blocked.contains(attr)),
            };
            if on == neg {
                return false;
            }
        }
        for &(neg, h) in &self.has {
            let on = match h {
                Has::Maintainer => !p.maintainers.is_empty(),
                Has::Team => !p.teams.is_empty(),
                Has::Pr => t.open_prs > 0,
                Has::Issue => t.open_issues > 0,
                Has::Gaps => t.gaps,
                Has::Advisory => t.vulnerable,
                Has::Newer => t.outdated,
            };
            if on == neg {
                return false;
            }
        }
        if let Some(set) = &self.in_set {
            if !in_set(attr, set) {
                return false;
            }
        }
        if !self.by.is_empty() || !self.not_by.is_empty() {
            let names = |l: &str| p.maintainers.iter().any(|m| m.eq_ignore_ascii_case(l));
            if !self.by.iter().all(|l| names(l)) || self.not_by.iter().any(|l| names(l)) {
                return false;
            }
        }
        let has_team = |name: &str| p.teams.iter().any(|t| t.eq_ignore_ascii_case(name));
        if self.team.as_deref().is_some_and(|name| !has_team(name)) {
            return false;
        }
        if self.not_team.iter().any(|name| has_team(name)) {
            return false;
        }
        if self.no_team && !p.teams.is_empty() {
            return false;
        }
        true
    }

    /// The key of the order of a row: the worst first, or the name.
    pub(crate) fn rank(&self, attr: &str, t: &Traits) -> impl Ord {
        match self.sort {
            Sort::Pressing => (t.rank(), attr.to_lowercase()),
            Sort::Az => (Traits::default().rank(), attr.to_lowercase()),
        }
    }
}

fn valid_set(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.+-".contains(c))
}

/// The set of an attribute: what comes before its first dot, or
/// `top-level`. `python3Packages` is any `python3NNPackages`, as nixpkgs
/// writes it, and as nixkeeper reads it.
pub(crate) fn in_set(attr: &str, set: &str) -> bool {
    let own = attr.split_once('.').map_or("top-level", |(s, _)| s);
    if own.eq_ignore_ascii_case(set) {
        return true;
    }
    set.eq_ignore_ascii_case("python3Packages")
        && own
            .strip_prefix("python3")
            .and_then(|r| r.strip_suffix("Packages"))
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// What the filter reads of one package. Made in the background for every
/// package name; see [`crate::catalog::package_stats`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Traits {
    pub outdated: bool,
    pub vulnerable: bool,
    pub backport: bool,
    pub unmaintained: bool,
    pub broken: bool,
    pub insecure: bool,
    pub unfree: bool,
    pub gaps: bool,
    pub open_prs: u32,
    pub open_issues: u32,
    /// Open advisories that can affect master, OSV matches included.
    pub advisories: u32,
    pub risk: u32,
    /// The worst band among the open advisories, when there is one:
    /// `Some(None)` is an advisory with no score.
    pub band: Option<Option<Band>>,
}

impl Traits {
    /// The same order as [`crate::catalog::Flags::rank`].
    pub(crate) fn rank(
        &self,
    ) -> (
        std::cmp::Reverse<u32>,
        std::cmp::Reverse<u32>,
        bool,
        std::cmp::Reverse<u32>,
    ) {
        (
            std::cmp::Reverse(self.risk),
            std::cmp::Reverse(self.advisories),
            !self.outdated,
            std::cmp::Reverse(self.open_prs + self.open_issues),
        )
    }

    /// The traits of a package of the index, read from the health sources.
    pub(crate) fn of(s: &Snapshot, index: &Index, h: &Health, attr: &str) -> Option<Traits> {
        let p = index.pkgs.get(attr)?;
        let upstream = crate::catalog::upstream(s, index, h, attr);
        let all = crate::catalog::judged(h, s, attr);
        let open: Vec<&Advisory> = all
            .iter()
            .filter(|(_, v)| v.open())
            .map(|(a, _)| *a)
            .collect();
        let osv = h.osv_extra(attr);
        let vulnerable = !open.is_empty() || !osv.is_empty();
        let band = vulnerable.then(|| {
            crate::catalog::package::rated(h, &open)
                .into_iter()
                .map(|(_, w)| w.band)
                .chain(
                    osv.iter()
                        .map(|(_, v)| crate::catalog::package::osv_worst(h, *v).band),
                )
                .max()
                .flatten()
        });
        let osv_worst: Vec<crate::severity::Worst> = osv
            .iter()
            .map(|(_, v)| crate::catalog::package::osv_worst(h, *v))
            .collect();
        let gaps = crate::hygiene::gaps(attr, p);
        let score = score::score(
            h,
            &score::Input {
                package: p,
                advisories: &all,
                osv: &osv_worst,
                upstream,
                hygiene: gaps.as_deref(),
            },
        );
        let key = crate::links::pkg_key(attr);
        let open_prs = s
            .links
            .prs
            .get(&key)
            .into_iter()
            .flatten()
            .filter_map(|n| s.pr_at.get(n).map(|&i| &s.pr_data[i]))
            .filter(|p| p.is_open())
            .count();
        let open_issues = s
            .links
            .issues
            .get(&key)
            .into_iter()
            .flatten()
            .filter_map(|n| s.issue_at.get(n).map(|&i| &s.issues[i]))
            .filter(|i| i.is_open())
            .count();
        Some(Traits {
            outdated: upstream.is_some(),
            vulnerable,
            backport: !vulnerable && all.iter().any(|(_, v)| matches!(v, Verdict::Behind { .. })),
            unmaintained: p.maintainers.is_empty() && p.teams.is_empty(),
            broken: p.facts.as_ref().is_some_and(|f| f.broken),
            insecure: p.facts.as_ref().is_some_and(|f| f.insecure),
            unfree: p.meta.as_ref().is_some_and(|m| m.unfree()),
            gaps: gaps.is_some_and(|g| !g.is_empty()),
            open_prs: open_prs as u32,
            open_issues: open_issues as u32,
            advisories: (open.len() + osv.len()) as u32,
            risk: score.risk,
            band,
        })
    }
}

/// What Hydra knows of failing builds, for `is:failed` and `is:blocked`:
/// the tracked packages whose last build is red on a jobset the site
/// follows, and the jobs of the Zero Hydra Failures snapshot.
#[derive(Debug, Default)]
pub(crate) struct Failing {
    pub failed: HashSet<String>,
    pub blocked: HashSet<String>,
}

impl Failing {
    pub(crate) fn gather(hydra: &crate::health::Hydra, index: &Index) -> Failing {
        let mut out = Failing::default();
        let tracked = hydra.builds.tracked();
        out.failed
            .extend(hydra.builds.red(tracked.iter().cloned()).into_keys());
        let zhf = hydra.zhf.snapshot();
        for item in crate::zhf::items(&zhf, index, &hydra.builds) {
            let Some(attr) = item.kind.attr().filter(|_| item.indexed) else {
                continue;
            };
            if item.failed {
                out.failed.insert(attr.to_string());
            } else {
                out.blocked.insert(attr.to_string());
            }
        }
        out
    }
}

/// The packages of the trait table that a filter keeps, in its order:
/// one per package name, the canonical attribute.
pub(crate) fn select<'a>(
    f: &Filter,
    index: &'a Index,
    traits: &'a HashMap<String, Traits>,
    failing: Option<&Failing>,
) -> Vec<(&'a String, &'a Traits)> {
    let mut rows: Vec<(&String, &Traits)> = traits
        .iter()
        .filter(|(attr, t)| {
            index
                .pkgs
                .get(*attr)
                .is_some_and(|p| f.keeps(attr, p, t, failing))
        })
        .collect();
    rows.sort_by_cached_key(|(attr, t)| f.rank(attr, t));
    if f.reverse {
        rows.reverse();
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(maintainers: &[&str], teams: &[&str]) -> Package {
        Package {
            pname: "hello".into(),
            version: "1".into(),
            file: None,
            maintainers: maintainers.iter().map(|s| s.to_string()).collect(),
            teams: teams.iter().map(|s| s.to_string()).collect(),
            facts: None,
            meta: None,
        }
    }

    #[test]
    fn the_words_of_nixkeeper_read_here() {
        let f = Filter::parse(
            "is:outdated -has:pr -has:maintainer in:python3Packages by:@Alice -team:\"Security review\" team:none sort:az order:reverse wine",
        );
        assert!(f.problems.is_empty(), "{:?}", f.problems);
        assert_eq!(f.is, [(false, Is::Outdated)]);
        assert_eq!(f.has, [(true, Has::Pr), (true, Has::Maintainer)]);
        assert_eq!(f.in_set.as_deref(), Some("python3Packages"));
        assert_eq!(f.by, ["alice"]);
        assert_eq!(f.not_team, ["Security review"]);
        assert!(f.no_team);
        assert_eq!(f.sort, Sort::Az);
        assert!(f.reverse);
        assert_eq!(f.text, "wine");
        assert!(!f.is_plain());
        assert!(Filter::parse("hello world").is_plain());
    }

    #[test]
    fn a_word_it_does_not_know_is_a_problem_and_the_rest_applies() {
        let f = Filter::parse("is:outdatd colour:red age:1y -in:x is:broken");
        assert_eq!(f.is, [(false, Is::Broken)]);
        let seen: Vec<(&str, &str, Option<&str>)> = f
            .problems
            .iter()
            .map(|p| {
                (
                    p.token.as_str(),
                    p.message.as_str(),
                    p.suggestion.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("is:outdatd", "no is:outdatd", Some("is:outdated")),
                ("colour:red", "no colour: qualifier", None),
                ("age:1y", "age: is not here yet", None),
                ("-in:x", "-in: leaves nothing out", None),
            ]
        );
        let f = Filter::parse("sort:oldest ha:pr order:reverse order:revers");
        assert_eq!(f.problems[0].suggestion, None);
        assert_eq!(f.problems[1].suggestion.as_deref(), Some("has:pr"));
        assert_eq!(f.problems[2].suggestion.as_deref(), Some("order:reverse"));
        assert!(f.reverse);
    }

    #[test]
    fn keeps_by_traits_and_people() {
        let t = Traits {
            outdated: true,
            open_prs: 1,
            ..Default::default()
        };
        let p = pkg(&["Alice"], &["python"]);
        let keep = |q: &str| Filter::parse(q).keeps("python312Packages.hello", &p, &t, None);
        assert!(keep("is:outdated"));
        assert!(!keep("-is:outdated"));
        assert!(keep("has:pr has:newer -is:broken"));
        assert!(!keep("is:vulnerable"));
        assert!(keep("by:alice"));
        assert!(!keep("-maintainer:alice"));
        assert!(keep("has:maintainer has:team"));
        assert!(!keep("is:unmaintained"));
        assert!(keep("team:Python"));
        assert!(!keep("team:none"));
        assert!(keep("in:python3Packages"));
        assert!(keep("in:python312Packages"));
        assert!(!keep("in:top-level"));
        assert!(keep("hel"));
        assert!(!keep("xyz"));
        assert!(!keep("is:failed"));
        let mut failing = Failing::default();
        failing.failed.insert("python312Packages.hello".into());
        assert!(Filter::parse("is:failed").keeps(
            "python312Packages.hello",
            &p,
            &t,
            Some(&failing)
        ));
    }

    #[test]
    fn sets() {
        assert!(in_set("hello", "top-level"));
        assert!(in_set("python313Packages.x", "python3Packages"));
        assert!(!in_set("python3Packages.x", "python313Packages"));
        assert!(in_set("haskellPackages.x", "haskellpackages"));
    }
}
