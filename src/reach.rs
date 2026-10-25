//! The branches and channels that hold a merged pull request, read from the
//! git copy. The flow is the one of the nixpkgs CONTRIBUTING.md, "Staging"
//! and "Branches": staging, staging-next, master or release-YY.MM, then the
//! channel branches, which move with their channels.

use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::mirror::Mirror;
use crate::model::Pr;
use crate::web::App;

/// The copy fetches the branches every 5 minutes, so an older answer is
/// still close.
const KEEP: Duration = Duration::from_secs(60);
const KEPT: usize = 2000;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Step {
    pub branch: String,
    pub channel: bool,
    pub contains: bool,
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
            })
            .collect(),
    })
}

static CACHE: LazyLock<Mutex<HashMap<String, (Instant, Reach)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// None for a pull request that is not merged, or on a server without the
/// git copy.
pub async fn of(app: &App, pr: &Pr) -> Option<Reach> {
    pr.merged_at?;
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
    let r =
        tokio::task::spawn_blocking(move || read(&dir, crate::events::REPO, &commit, &base)).await;
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
}
