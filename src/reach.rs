//! The branches and channels that hold a merged pull request, and since
//! when, read from the git copy. The flow is the one of the nixpkgs
//! CONTRIBUTING.md, "Staging" and "Branches": staging, staging-next, master
//! or release-YY.MM, then the channel branches, which move with their
//! channels.

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::mirror::{Logged, Mirror};
use crate::model::Pr;
use crate::web::App;

/// The copy fetches the branches every 5 minutes, so an older answer is
/// still close.
const KEEP: Duration = Duration::from_secs(60);
const KEPT: usize = 2000;
/// A time once found never changes, so it stays until the map is full.
const KNOWN: usize = 20_000;
/// The `git merge-base` calls and the time that one view may spend on the
/// times of its steps.
const PROBES: usize = 40;
const SPEND: Duration = Duration::from_secs(5);
/// The clocks of GitHub and of the server can differ a little.
const SLACK: i64 = 120;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Step {
    pub branch: String,
    pub channel: bool,
    pub contains: bool,
    /// When the copy first saw the commit in the branch. None when that is
    /// not known: the branch held it at the start of its reflog, or the view
    /// spent its git calls.
    pub at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Reach {
    pub commit: String,
    /// False until the copy has fetched the merge commit.
    pub in_copy: bool,
    pub steps: Vec<Step>,
}

/// "" for unstable, or the version of a release such as "26.05".
fn version(rest: &str) -> Option<&str> {
    if rest.is_empty() {
        return Some("");
    }
    let v = rest.strip_prefix('-')?;
    let (a, b) = v.split_once('.')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    (digits(a) && digits(b)).then_some(v)
}

/// The branches after `base`, `base` first, and whether each is a channel.
pub fn path(base: &str) -> Vec<(String, bool)> {
    let after = |prefix: &str| base.strip_prefix(prefix).and_then(version);
    let (chain, v): (&[&str], &str) = if let Some(v) = after("staging-next") {
        (&["staging-next"], v)
    } else if let Some(v) = after("staging-nixos") {
        (&["staging-nixos"], v)
    } else if let Some(v) = after("staging") {
        (&["staging", "staging-next"], v)
    } else if base == "master" {
        (&[], "")
    } else if let Some(v) = base.strip_prefix("release").and_then(version) {
        (&[], v)
    } else {
        (&[], "")
    };
    let named = |b: &str| {
        if v.is_empty() {
            b.to_string()
        } else {
            format!("{b}-{v}")
        }
    };
    let mut out: Vec<(String, bool)> = chain.iter().map(|b| (named(b), false)).collect();
    let trunk = if v.is_empty() {
        "master".to_string()
    } else {
        format!("release-{v}")
    };
    if chain.is_empty() && base != trunk {
        out.push((base.to_string(), false));
    }
    out.push((trunk, false));
    let channels = if v.is_empty() {
        [
            "nixos-unstable-small".to_string(),
            "nixpkgs-unstable".to_string(),
            "nixos-unstable".to_string(),
        ]
    } else {
        [
            format!("nixos-{v}-small"),
            format!("nixpkgs-{v}-darwin"),
            format!("nixos-{v}"),
        ]
    };
    out.extend(channels.into_iter().map(|c| (c, true)));
    out
}

/// Branches that the copy does not have, such as a staging-next-YY.MM
/// after its release, are left out.
pub fn read(dir: &Path, repo: &str, commit: &str, base: &str) -> anyhow::Result<Reach> {
    let steps = path(base);
    let names: Vec<String> = steps.iter().map(|(b, _)| b.clone()).collect();
    let Some((exist, with)) = Mirror::of(dir, repo).branches_with(commit, &names)? else {
        return Ok(Reach {
            commit: commit.to_string(),
            in_copy: false,
            steps: Vec::new(),
        });
    };
    Ok(Reach {
        commit: commit.to_string(),
        in_copy: true,
        steps: steps
            .into_iter()
            .filter(|(b, _)| exist.contains(b))
            .map(|(b, channel)| Step {
                contains: with.contains(&b),
                branch: b,
                channel,
                at: None,
            })
            .collect(),
    })
}

/// What one view may still spend.
struct Spend {
    probes: usize,
    until: Instant,
}

#[derive(Debug)]
struct Spent;

impl std::fmt::Display for Spent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the view spent its git calls")
    }
}

impl std::error::Error for Spent {}

impl Spend {
    fn left(&self) -> anyhow::Result<Duration> {
        let left = self.until.saturating_duration_since(Instant::now());
        if self.probes == 0 || left.is_zero() {
            return Err(Spent.into());
        }
        Ok(left)
    }
}

fn probe(m: &Mirror, of: &str, commit: &str, spend: &mut Spend) -> anyhow::Result<bool> {
    let left = spend.left()?;
    spend.probes -= 1;
    m.holds(of, commit, left)
}

/// The index of the oldest entry that holds the commit. Within a run of
/// fast-forwards, an entry that holds it is followed by entries that hold
/// it, so a gallop from the start of each run, then a bisection, finds it
/// with few calls. A forced update starts a new run.
fn oldest(
    entries: &[Logged],
    mut holds: impl FnMut(&str) -> anyhow::Result<bool>,
) -> anyhow::Result<Option<usize>> {
    let mut a = 0;
    while a < entries.len() {
        let mut b = a;
        while b + 1 < entries.len() && entries[b + 1].ff {
            b += 1;
        }
        let (mut lo, mut i, mut step) = (a, a, 1);
        loop {
            if holds(&entries[i].commit)? {
                let mut hi = i;
                while lo < hi {
                    let mid = lo + (hi - lo) / 2;
                    if holds(&entries[mid].commit)? {
                        hi = mid;
                    } else {
                        lo = mid + 1;
                    }
                }
                return Ok(Some(hi));
            }
            if i == b {
                break;
            }
            lo = i + 1;
            i = (i + step).min(b);
            step *= 2;
        }
        a = b + 1;
    }
    Ok(None)
}

/// The unix time of the oldest reflog entry of `branch` that holds
/// `commit`, or None when that is the first entry of the reflog: then the
/// branch held the commit before the copy saw it change.
fn first_held(
    m: &Mirror,
    commit: &str,
    branch: &str,
    since: i64,
    spend: &mut Spend,
) -> anyhow::Result<Option<i64>> {
    let mut log = m.reflog(branch, since, spend.left()?)?;
    let mut found = oldest(&log.entries, |of| probe(m, of, commit, spend))?;
    if found == Some(0) && !log.whole {
        // the branch held the commit before the merge, so the window is wrong
        log = m.reflog(branch, i64::MIN, spend.left()?)?;
        found = oldest(&log.entries, |of| probe(m, of, commit, spend))?;
    }
    Ok(found
        .filter(|&i| i > 0 || !log.whole)
        .map(|i| log.entries[i].at))
}

static TIMES: LazyLock<Mutex<HashMap<String, Option<i64>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Sets `at` on each step that holds `commit`. A pull request merged
/// before the reflogs start gets no times.
fn times(
    m: &Mirror,
    commit: &str,
    merged: DateTime<Utc>,
    logs_from: DateTime<Utc>,
    steps: &mut [Step],
) {
    if merged < logs_from {
        return;
    }
    let mut spend = Spend {
        probes: PROBES,
        until: Instant::now() + SPEND,
    };
    for s in steps.iter_mut().filter(|s| s.contains) {
        let key = format!("{commit} {}", s.branch);
        let known = TIMES.lock().unwrap().get(&key).copied();
        let at = match known {
            Some(at) => at,
            None => {
                match first_held(m, commit, &s.branch, merged.timestamp() - SLACK, &mut spend) {
                    Ok(at) => {
                        let mut t = TIMES.lock().unwrap();
                        if t.len() >= KNOWN {
                            t.clear();
                        }
                        t.insert(key, at);
                        at
                    }
                    Err(e) => {
                        if !e.is::<Spent>() {
                            tracing::warn!("reach of {commit} in {}: {e:#}", s.branch);
                        }
                        None
                    }
                }
            }
        };
        s.at = at.and_then(|t| DateTime::from_timestamp(t, 0));
    }
}

static CACHE: LazyLock<Mutex<HashMap<String, (Instant, Reach)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// None for a pull request that is not merged, or on a server without the
/// git copy.
pub async fn of(app: &App, pr: &Pr) -> Option<Reach> {
    let merged = pr.merged_at?;
    let commit = pr.merge_commit.clone()?;
    let dir: PathBuf = app.git_dir.clone()?;
    let key = format!("{commit} {}", pr.base);
    let hit = CACHE
        .lock()
        .unwrap()
        .get(&key)
        .filter(|(at, _)| at.elapsed() < KEEP)
        .map(|(_, r)| r.clone());
    if hit.is_some() {
        return hit;
    }
    let base = pr.base.clone();
    let archive = app.nixpkgs_archive().cloned();
    let r = tokio::task::spawn_blocking(move || {
        let mut r = read(&dir, crate::events::REPO, &commit, &base)?;
        let from = match archive {
            Some(a) if r.in_copy => crate::mirror::logs_from(&a).unwrap_or(None),
            _ => None,
        };
        if let Some(from) = from {
            let m = Mirror::of(&dir, crate::events::REPO);
            times(&m, &commit, merged, from, &mut r.steps);
        }
        anyhow::Ok(r)
    })
    .await;
    match r {
        Ok(Ok(r)) => {
            let mut c = CACHE.lock().unwrap();
            if c.len() >= KEPT {
                c.clear();
            }
            c.insert(key, (Instant::now(), r.clone()));
            Some(r)
        }
        Ok(Err(e)) => {
            tracing::warn!("reach of #{}: {e:#}", pr.number);
            None
        }
        Err(e) => {
            tracing::warn!("reach of #{}: {e}", pr.number);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(base: &str) -> Vec<String> {
        path(base)
            .into_iter()
            .map(|(b, c)| if c { format!("({b})") } else { b })
            .collect()
    }

    #[test]
    fn each_base_flows_to_its_trunk_then_its_channels() {
        assert_eq!(
            names("master"),
            [
                "master",
                "(nixos-unstable-small)",
                "(nixpkgs-unstable)",
                "(nixos-unstable)"
            ]
        );
        assert_eq!(names("staging")[..3], ["staging", "staging-next", "master"]);
        assert_eq!(names("staging-next")[..2], ["staging-next", "master"]);
        assert_eq!(names("staging-nixos")[..2], ["staging-nixos", "master"]);
        assert_eq!(
            names("staging-26.05"),
            [
                "staging-26.05",
                "staging-next-26.05",
                "release-26.05",
                "(nixos-26.05-small)",
                "(nixpkgs-26.05-darwin)",
                "(nixos-26.05)"
            ]
        );
        assert_eq!(
            names("release-26.05")[..2],
            ["release-26.05", "(nixos-26.05-small)"]
        );
        assert_eq!(names("haskell-updates")[..2], ["haskell-updates", "master"]);
        assert_eq!(names("staging-foo")[..2], ["staging-foo", "master"]);
    }

    #[test]
    fn the_copy_says_which_branches_hold_the_commit() {
        let d = std::env::temp_dir().join(format!("mm-reach-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let git = |args: &[&str]| -> String {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&d)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.org"])
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "--quiet", "--bare", "o/r.git"]);
        let repo = d.join("o/r.git");
        let r = repo.to_str().unwrap();
        let tree = git(&["-C", r, "hash-object", "-t", "tree", "-w", "--stdin"]);
        let old = git(&["-C", r, "commit-tree", &tree, "-m", "old"]);
        let merge = git(&["-C", r, "commit-tree", &tree, "-p", &old, "-m", "merge"]);
        git(&["-C", r, "update-ref", "refs/heads/master", &merge]);
        git(&["-C", r, "update-ref", "refs/heads/nixos-unstable", &old]);
        git(&["-C", r, "update-ref", "refs/heads/nixpkgs-unstable", &merge]);
        let reach = read(&d, "o/r", &merge, "master").unwrap();
        assert!(reach.in_copy);
        let got: Vec<(&str, bool)> = reach
            .steps
            .iter()
            .map(|s| (s.branch.as_str(), s.contains))
            .collect();
        assert_eq!(
            got,
            [
                ("master", true),
                ("nixpkgs-unstable", true),
                ("nixos-unstable", false)
            ]
        );
        let missing = read(&d, "o/r", &"0".repeat(40), "master").unwrap();
        assert!(!missing.in_copy && missing.steps.is_empty());
        assert!(read(&d, "o/r", "--all", "master").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn logged(ff: &[bool]) -> Vec<Logged> {
        ff.iter()
            .enumerate()
            .map(|(i, &ff)| Logged {
                at: i as i64,
                commit: format!("{i:x}"),
                ff,
            })
            .collect()
    }

    /// The first index found, and the entries asked about.
    fn search(entries: &[Logged], held: impl Fn(usize) -> bool) -> (Option<usize>, Vec<usize>) {
        let mut asked = Vec::new();
        let found = oldest(entries, |c| {
            let i = usize::from_str_radix(c, 16).unwrap();
            asked.push(i);
            Ok(held(i))
        })
        .unwrap();
        (found, asked)
    }

    #[test]
    fn a_gallop_from_the_merge_finds_the_first_entry_that_holds_the_commit() {
        let mut ff = vec![true; 1000];
        ff[0] = false;
        let entries = logged(&ff);
        let (found, asked) = search(&entries, |i| i >= 2);
        assert_eq!(found, Some(2));
        assert_eq!(asked, [0, 1, 3, 2]);
        let (found, asked) = search(&entries, |i| i >= 700);
        assert_eq!(found, Some(700));
        assert!(asked.len() <= 22, "{asked:?}");
        assert_eq!(search(&entries, |_| false).0, None);
        assert_eq!(search(&entries, |_| true), (Some(0), vec![0]));
    }

    #[test]
    fn a_forced_update_starts_a_new_search() {
        // held from 3, lost by the forced update at 6, held again from 8
        let entries = logged(&[false, true, true, true, true, true, false, true, true, true]);
        assert_eq!(
            search(&entries, |i| (3..6).contains(&i) || i >= 8).0,
            Some(3)
        );
        // the first run never held it
        assert_eq!(search(&entries, |i| i >= 7).0, Some(7));
    }

    #[test]
    fn the_reflog_says_when_each_branch_first_held_the_commit() {
        let d = std::env::temp_dir().join(format!("mm-reach-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let m = Mirror::of(&d, "o/r");
        m.init().unwrap();
        let t0: i64 = 1_790_000_000;
        let git = |at: i64, args: &[&str]| -> String {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&m.dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.org"])
                .args(args)
                .env("GIT_COMMITTER_DATE", format!("@{} +0000", t0 + at))
                .env("GIT_AUTHOR_DATE", format!("@{} +0000", t0 + at))
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let tree = git(0, &["hash-object", "-t", "tree", "-w", "--stdin"]);
        let commit = |at: i64, parent: Option<&str>, msg: &str| -> String {
            let mut args = vec!["commit-tree", tree.as_str(), "-m", msg];
            if let Some(p) = parent {
                args.extend(["-p", p]);
            }
            git(at, &args)
        };
        let set = |at: i64, branch: &str, c: &str, how: &str| {
            let msg = format!("fetch: {how}");
            git(
                at,
                &["update-ref", "-m", &msg, &format!("refs/heads/{branch}"), c],
            );
        };
        let old = commit(0, None, "reflog old");
        let other = commit(0, None, "reflog other");
        let old2 = commit(1800, Some(&old), "reflog old2");
        let merge = commit(2500, Some(&old2), "reflog merge");
        let later = commit(3400, Some(&merge), "reflog later");
        set(1000, "master", &old, "storing head");
        set(2000, "master", &old2, "fast-forward");
        set(3000, "master", &merge, "fast-forward");
        set(4000, "master", &later, "fast-forward");
        set(1000, "nixos-unstable", &old, "storing head");
        set(5000, "nixos-unstable", &later, "fast-forward");
        set(1000, "staging-next", &old, "storing head");
        set(3000, "staging-next", &merge, "fast-forward");
        set(3500, "staging-next", &other, "forced-update");
        set(4500, "staging-next", &later, "forced-update");
        set(6000, "nixpkgs-unstable", &later, "storing head");
        set(1000, "early", &old, "storing head");
        set(1500, "early", &merge, "fast-forward");
        set(2600, "early", &later, "fast-forward");
        let at = |s: i64| DateTime::from_timestamp(t0 + s, 0).unwrap();
        let step = |b: &str| Step {
            branch: b.into(),
            channel: false,
            contains: true,
            at: None,
        };
        let names = [
            "master",
            "nixos-unstable",
            "staging-next",
            "nixpkgs-unstable",
            "early",
        ];
        let mut steps: Vec<Step> = names.iter().map(|b| step(b)).collect();
        times(&m, &merge, at(2500), at(1000), &mut steps);
        let got: Vec<Option<DateTime<Utc>>> = steps.iter().map(|s| s.at).collect();
        assert_eq!(
            got,
            [
                Some(at(3000)),
                Some(at(5000)),
                Some(at(3000)),
                None,
                Some(at(1500))
            ]
        );
        let mut again: Vec<Step> = names.iter().map(|b| step(b)).collect();
        times(&m, &merge, at(2500), at(1000), &mut again);
        assert_eq!(again, steps);
        let mut before: Vec<Step> = names.iter().map(|b| step(b)).collect();
        times(&m, &later, at(900), at(1000), &mut before);
        assert!(before.iter().all(|s| s.at.is_none()));
        let _ = std::fs::remove_dir_all(&d);
    }
}
