//! The full list of paths that a pull request changes, from the git copy.
//! The archive keeps the first 50 files of a pull request; the copy has
//! every commit, so `git diff-tree` gives the rest. Only a cut list is
//! computed, once per head, in the task of the copy. The pages use it from
//! their next recompute on.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::{Duration, Instant};

use crate::mirror::Mirror;
use crate::model::Pr;
use crate::web::App;

/// The paths kept of one pull request; a longer list stays cut there.
pub const MAX: usize = 10_000;
const PER_PASS: usize = 50;
const LIMIT: Duration = Duration::from_secs(20);
/// A head that the copy does not have yet is asked for again after this.
const RETRY: Duration = Duration::from_secs(600);

#[derive(Default)]
struct Lists {
    full: HashMap<String, Arc<Vec<String>>>,
    missed: HashMap<String, Instant>,
}

static LISTS: LazyLock<RwLock<Lists>> = LazyLock::new(Default::default);

/// GitHub counts more files than the archived list holds. A row with the
/// full list in it stays cut, so that its list stays.
pub fn cut(pr: &Pr) -> bool {
    pr.changed_files > crate::project::FILES as u64 || pr.changed_files > pr.files.len() as u64
}

/// The key of the list, and the commit to compare the head with: the base
/// branch, or the first parent of the merge commit, because the base
/// branch holds the head of a merged pull request.
fn key(pr: &Pr) -> Option<(String, String)> {
    let against = if pr.merged_at.is_some() {
        let c = pr.merge_commit.as_deref()?;
        if c.is_empty() || !c.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        format!("{c}^1")
    } else {
        format!("refs/heads/{}", pr.base)
    };
    Some((format!("{} {against}", pr.head_oid), against))
}

/// The full list of a cut pull request, when the copy gave it.
pub fn full(pr: &Pr) -> Option<Arc<Vec<String>>> {
    if !cut(pr) {
        return None;
    }
    let (k, _) = key(pr)?;
    LISTS.read().unwrap().full.get(&k).cloned()
}

/// The list has every path that the pull request changes.
pub fn complete(pr: &Pr) -> bool {
    !cut(pr) || full(pr).is_some_and(|l| l.len() < MAX)
}

/// Puts the full lists in the rows, before the pages are computed from
/// them.
pub fn apply(prs: &mut [Pr]) {
    let l = LISTS.read().unwrap();
    if l.full.is_empty() {
        return;
    }
    for pr in prs.iter_mut().filter(|p| cut(p)) {
        if let Some(list) = key(pr).and_then(|(k, _)| l.full.get(&k)) {
            pr.files = list.to_vec();
        }
    }
}

/// Computes up to [`PER_PASS`] lists that the pull requests of the pages
/// miss, open ones first, and drops the lists that no page needs.
pub fn fill(app: &App, m: &Mirror) -> usize {
    let s = app.snap();
    let mut prs: Vec<&Pr> = s.pr_data.iter().filter(|p| cut(p)).collect();
    prs.sort_by_key(|p| !p.is_open());
    let wanted: Vec<(String, String, &str)> = prs
        .iter()
        .filter_map(|p| key(p).map(|(k, against)| (k, against, p.head_oid.as_str())))
        .collect();
    let keys: HashSet<&str> = wanted.iter().map(|(k, _, _)| k.as_str()).collect();
    let todo: Vec<&(String, String, &str)> = {
        let mut l = LISTS.write().unwrap();
        l.full.retain(|k, _| keys.contains(k.as_str()));
        l.missed
            .retain(|k, t| keys.contains(k.as_str()) && t.elapsed() < RETRY);
        wanted
            .iter()
            .filter(|(k, _, _)| !l.full.contains_key(k) && !l.missed.contains_key(k))
            .take(PER_PASS)
            .collect()
    };
    let mut added = 0;
    for (k, against, head) in todo {
        let got = m.changed(against, head, MAX, LIMIT);
        let mut l = LISTS.write().unwrap();
        match got {
            Ok(Some(list)) => {
                l.full.insert(k.clone(), Arc::new(list));
                added += 1;
            }
            Ok(None) => {
                l.missed.insert(k.clone(), Instant::now());
            }
            Err(e) => {
                tracing::warn!("git copy: the files of {head}: {e:#}");
                l.missed.insert(k.clone(), Instant::now());
            }
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::tests::pr;

    #[test]
    fn only_a_cut_list_has_a_key_and_a_merged_one_compares_with_the_first_parent() {
        let mut p = pr(7, "alice", 0);
        p.files = vec!["a".into()];
        p.changed_files = 1;
        assert!(!cut(&p));
        p.changed_files = 2;
        assert!(cut(&p));
        p.files = (0..60).map(|i| format!("f{i}")).collect();
        p.changed_files = 60;
        assert!(cut(&p), "a row with the full list in it stays cut");
        p.head_oid = "aa".into();
        p.base = "staging".into();
        assert_eq!(
            key(&p),
            Some(("aa refs/heads/staging".into(), "refs/heads/staging".into()))
        );
        p.merged_at = Some(chrono::Utc::now());
        assert_eq!(key(&p), None);
        p.merge_commit = Some("bb".into());
        assert_eq!(key(&p), Some(("aa bb^1".into(), "bb^1".into())));
        p.merge_commit = Some("--all".into());
        assert_eq!(key(&p), None);
    }

    #[test]
    fn the_copy_gives_every_path_of_a_pull_request() {
        let d = std::env::temp_dir().join(format!("mm-changed-{}", std::process::id()));
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
        git(&["init", "--quiet", "--initial-branch=master", "w"]);
        let w = |args: &[&str]| -> String {
            let mut a = vec!["-C", "w"];
            a.extend(args);
            git(&a)
        };
        std::fs::write(d.join("w/base.nix"), "a").unwrap();
        w(&["add", "."]);
        w(&["commit", "--quiet", "-m", "base"]);
        w(&["checkout", "--quiet", "-b", "pr"]);
        for i in 0..60 {
            let dir = d.join(format!("w/pkgs/p{i:02}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("package.nix"), "x").unwrap();
        }
        std::fs::write(d.join("w/odd name ü.nix"), "y").unwrap();
        std::fs::remove_file(d.join("w/base.nix")).unwrap();
        w(&["add", "-A"]);
        w(&["commit", "--quiet", "-m", "pr"]);
        let head = w(&["rev-parse", "HEAD"]);
        w(&["checkout", "--quiet", "master"]);
        std::fs::write(d.join("w/later.nix"), "z").unwrap();
        w(&["add", "."]);
        w(&["commit", "--quiet", "-m", "later"]);
        w(&["merge", "--quiet", "--no-ff", "-m", "merge", "pr"]);
        let merge = w(&["rev-parse", "HEAD"]);
        let m = Mirror::of(&d, "o/r");
        m.init().unwrap();
        git(&[
            "-C",
            m.dir.to_str().unwrap(),
            "fetch",
            "--quiet",
            "../../w",
            "+refs/heads/*:refs/heads/*",
        ]);
        let all = m
            .changed("refs/heads/master~1", &head, MAX, LIMIT)
            .unwrap()
            .unwrap();
        assert_eq!(all.len(), 62);
        assert!(all.contains(&"base.nix".to_string()));
        assert!(all.contains(&"odd name ü.nix".to_string()));
        assert!(all.contains(&"pkgs/p59/package.nix".to_string()));
        assert!(!all.contains(&"later.nix".to_string()));
        let merged = m
            .changed(&format!("{merge}^1"), &head, MAX, LIMIT)
            .unwrap()
            .unwrap();
        assert_eq!(merged, all);
        assert_eq!(
            m.changed("refs/heads/master~1", &head, 5, LIMIT)
                .unwrap()
                .unwrap()
                .len(),
            5
        );
        assert_eq!(
            m.changed("refs/heads/gone", &head, MAX, LIMIT).unwrap(),
            None
        );
        assert_eq!(
            m.changed("refs/heads/master", &"0".repeat(40), MAX, LIMIT)
                .unwrap(),
            None
        );
        assert!(m.changed("--all", &head, MAX, LIMIT).is_err());
        let mut p = pr(9, "alice", 0);
        p.files = all[..50].to_vec();
        p.changed_files = 62;
        p.head_oid = head.clone();
        p.base = "master".into();
        assert!(full(&p).is_none() && !complete(&p));
        let (k, _) = key(&p).unwrap();
        LISTS.write().unwrap().full.insert(k, Arc::new(all.clone()));
        let mut rows = vec![p.clone()];
        apply(&mut rows);
        assert_eq!(rows[0].files, all);
        assert!(complete(&rows[0]));
        let _ = std::fs::remove_dir_all(&d);
    }
}
