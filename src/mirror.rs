//! A git copy of each archived repository: every branch and tag, and the
//! head of every pull request. A forced update leaves the old commit in the
//! reflog, so the copy keeps the versions that a force push replaced.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

use crate::archive::Archive;
use crate::events::{Filter, Log};
use crate::web::App;

pub const BRANCHES_EVERY: StdDuration = StdDuration::from_secs(300);
pub const FULL_EVERY_HOURS: i64 = 6;
const RETRY_HOURS: i64 = 1;
const SETTLE: StdDuration = StdDuration::from_secs(10);
const BATCH: usize = 200;

const BRANCHES: [&str; 2] = ["+refs/heads/*:refs/heads/*", "+refs/tags/*:refs/tags/*"];
const HEADS: &str = "+refs/pull/*/head:refs/pull/*/head";

const CONFIG: [(&str, &str); 9] = [
    ("core.logAllRefUpdates", "always"),
    // the reflog is the only thing that keeps a replaced head
    ("gc.reflogExpire", "never"),
    ("gc.reflogExpireUnreachable", "never"),
    ("gc.auto", "0"),
    ("maintenance.auto", "false"),
    // one thread keeps index-pack of nixpkgs near 800 MB
    ("pack.threads", "1"),
    ("pack.windowMemory", "256m"),
    ("repack.writeBitmaps", "false"),
    ("fetch.writeCommitGraph", "false"),
];

const FULL: &str = "git_full_at";
const TRIED: &str = "git_tried_at";
const FETCHED: &str = "git_fetched_at";
const ERROR: &str = "git_error";
const REPLACED: &str = "git_replaced";
const BYTES: &str = "git_bytes";
const COUNTS: &str = "git_counts";

pub struct Mirror {
    pub dir: PathBuf,
    url: String,
}

#[derive(Debug, Default, PartialEq)]
pub struct Fetched {
    pub new: usize,
    pub moved: usize,
    /// Refs that moved to a commit that does not contain the old one.
    pub forced: usize,
}

impl Fetched {
    fn add(&mut self, o: Fetched) {
        self.new += o.new;
        self.moved += o.moved;
        self.forced += o.forced;
    }

    fn parse(porcelain: &str) -> Self {
        let mut f = Self::default();
        for line in porcelain.lines() {
            match line.chars().next() {
                Some('*') => f.new += 1,
                Some(' ') => f.moved += 1,
                Some('+') => f.forced += 1,
                _ => {}
            }
        }
        f
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Counts {
    pub branches: usize,
    pub tags: usize,
    pub heads: usize,
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run git")?;
    // the kernel kills git, not the server, when the service runs out of
    // memory; git starts index-pack later, which inherits the score
    let _ = std::fs::write(format!("/proc/{}/oom_score_adj", child.id()), "1000");
    let out = child.wait_with_output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("");
        bail!("git {}: {}", args[0], last.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

impl Mirror {
    pub fn of(dir: &Path, repo: &str) -> Self {
        Self {
            dir: dir.join(format!("{repo}.git")),
            url: format!("https://github.com/{repo}.git"),
        }
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        git(&self.dir, args)
    }

    pub fn init(&self) -> Result<()> {
        if self.dir.join("HEAD").exists() {
            return Ok(());
        }
        let parent = self.dir.parent().context("git copy without a parent")?;
        std::fs::create_dir_all(parent)?;
        let tmp = self.dir.with_extension("init");
        let _ = std::fs::remove_dir_all(&tmp);
        let path = tmp.to_str().context("path is not UTF-8")?;
        git(
            parent,
            &["init", "--quiet", "--bare", "--ref-format=reftable", path],
        )?;
        for (k, v) in CONFIG {
            git(&tmp, &["config", k, v])?;
        }
        std::fs::rename(&tmp, &self.dir)?;
        Ok(())
    }

    fn fetch(&self, refspecs: &[String]) -> Result<Fetched> {
        let mut args = vec![
            "fetch",
            "--porcelain",
            "--no-tags",
            "--no-write-fetch-head",
            "--no-auto-maintenance",
            &self.url,
        ];
        args.extend(refspecs.iter().map(String::as_str));
        Ok(Fetched::parse(&self.run(&args)?))
    }

    pub fn branches(&self) -> Result<Fetched> {
        self.fetch(&BRANCHES.map(String::from))
    }

    /// An issue, or a pull request that GitHub deleted, has no head ref and
    /// fails its whole batch, so a failed batch is fetched again one by one.
    pub fn heads(&self, numbers: &[u64]) -> Result<Fetched> {
        let mut all = Fetched::default();
        for chunk in numbers.chunks(BATCH) {
            let specs: Vec<String> = chunk
                .iter()
                .map(|n| format!("+refs/pull/{n}/head:refs/pull/{n}/head"))
                .collect();
            match self.fetch(&specs) {
                Ok(f) => all.add(f),
                Err(e) if specs.len() == 1 && !missing(&e) => return Err(e),
                Err(_) if specs.len() == 1 => {}
                Err(_) => {
                    for s in specs {
                        match self.fetch(std::slice::from_ref(&s)) {
                            Ok(f) => all.add(f),
                            Err(e) if missing(&e) => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
            }
        }
        Ok(all)
    }

    /// One fetch for all refs: into an empty copy, GitHub then sends one
    /// complete pack. A later fetch gets a thin pack, and index-pack stores
    /// a second full copy of each delta base next to it.
    pub fn full(&self) -> Result<Fetched> {
        // a fetch that a restart stopped leaves its partial pack; only this
        // task runs git here, one command at a time
        if let Ok(d) = std::fs::read_dir(self.dir.join("objects").join("pack")) {
            for e in d.flatten() {
                if e.file_name().to_string_lossy().starts_with("tmp_") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        let mut specs = BRANCHES.map(String::from).to_vec();
        specs.push(HEADS.into());
        let f = self.fetch(&specs)?;
        self.pack()?;
        Ok(f)
    }

    /// Merges the loose objects and the small packs, and leaves the large
    /// pack as it is. A merge does not write again an object that the large
    /// pack holds, so the second copies of the bases go. No prune and no
    /// gc: the copy deletes no object.
    pub fn pack(&self) -> Result<()> {
        self.run(&["repack", "--geometric=2", "-d", "--write-midx", "-q"])?;
        self.run(&[
            "commit-graph",
            "write",
            "--reachable",
            "--split",
            "--no-progress",
        ])?;
        Ok(())
    }

    pub fn counts(&self) -> Result<Counts> {
        let out = self.run(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads",
            "refs/tags",
            "refs/pull",
        ])?;
        let mut c = Counts::default();
        for r in out.lines() {
            if r.starts_with("refs/heads/") {
                c.branches += 1;
            } else if r.starts_with("refs/tags/") {
                c.tags += 1;
            } else if r.starts_with("refs/pull/") {
                c.heads += 1;
            }
        }
        Ok(c)
    }

    /// Of the branches `names`, those that exist and those that contain
    /// `commit`; None when the copy does not have the commit yet.
    pub fn branches_with(
        &self,
        commit: &str,
        names: &[String],
    ) -> Result<Option<(Vec<String>, Vec<String>)>> {
        if !commit.bytes().all(|b| b.is_ascii_hexdigit()) || commit.is_empty() {
            bail!("not a commit id: {commit:?}");
        }
        let refs: Vec<String> = names.iter().map(|n| format!("refs/heads/{n}")).collect();
        let list = |with: Option<&str>| -> Result<Vec<String>> {
            let contains = with.map(|c| format!("--contains={c}"));
            let mut args = vec!["for-each-ref", "--format=%(refname:strip=2)"];
            args.extend(contains.as_deref());
            args.extend(refs.iter().map(String::as_str));
            Ok(self.run(&args)?.lines().map(String::from).collect())
        };
        if self
            .run(&["cat-file", "-e", &format!("{commit}^{{commit}}")])
            .is_err()
        {
            return Ok(None);
        }
        Ok(Some((list(None)?, list(Some(commit))?)))
    }

    pub fn bytes(&self) -> Result<u64> {
        let out = self.run(&["count-objects", "-v"])?;
        Ok(out
            .lines()
            .filter_map(|l| l.split_once(": "))
            .filter(|(k, _)| matches!(*k, "size" | "size-pack" | "size-garbage"))
            .filter_map(|(_, v)| v.trim().parse::<u64>().ok())
            .sum::<u64>()
            * 1024)
    }
}

fn missing(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains("couldn't find remote ref")
}

fn time(a: &Archive, key: &str) -> Result<Option<DateTime<Utc>>> {
    Ok(a.get(key)?
        .and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
        .map(|t| t.with_timezone(&Utc)))
}

/// After a failure, the next full fetch waits [`RETRY_HOURS`], so that a
/// first copy that runs out of memory does not download nixpkgs again on
/// each pass.
fn full_due(full: Option<DateTime<Utc>>, tried: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    full.is_none_or(|t| now - t >= Duration::hours(FULL_EVERY_HOURS))
        && tried
            .is_none_or(|t| full.is_some_and(|f| f >= t) || now - t >= Duration::hours(RETRY_HOURS))
}

fn pass(a: &Archive, m: &Mirror, branches: bool, numbers: &[u64]) -> Result<()> {
    let now = Utc::now();
    let full = time(a, FULL)?;
    let fetched = (|| -> Result<Option<Fetched>> {
        m.init()?;
        if full_due(full, time(a, TRIED)?, now) {
            a.set(TRIED, &now.to_rfc3339())?;
            let _one = crate::eval::SWEEP.lock().unwrap_or_else(|e| e.into_inner());
            let f = m.full()?;
            let c = m.counts()?;
            a.set(
                COUNTS,
                &json!({ "branches": c.branches, "tags": c.tags, "heads": c.heads }).to_string(),
            )?;
            a.set(FULL, &Utc::now().to_rfc3339())?;
            return Ok(Some(f));
        }
        // until one full fetch ends, the branches are part of a download
        // of gigabytes that must not run outside the sweep lock
        if full.is_none() {
            return Ok(None);
        }
        let mut f = Fetched::default();
        if branches {
            f.add(m.branches()?);
        }
        if !numbers.is_empty() {
            f.add(m.heads(numbers)?);
        }
        Ok(Some(f))
    })();
    match fetched {
        Ok(None) => Ok(()),
        Ok(Some(f)) => {
            a.set(FETCHED, &Utc::now().to_rfc3339())?;
            a.set(ERROR, "")?;
            if f.forced > 0 {
                let n: usize = a.get(REPLACED)?.and_then(|v| v.parse().ok()).unwrap_or(0);
                a.set(REPLACED, &(n + f.forced).to_string())?;
            }
            a.set(BYTES, &m.bytes()?.to_string())?;
            Ok(())
        }
        Err(e) => {
            a.set(ERROR, &format!("{e:#}"))?;
            Err(e)
        }
    }
}

/// Pull requests with an event after `after`, and whether a push came.
fn hints(log: &Log, after: u64, upto: u64) -> Result<(Vec<u64>, bool)> {
    let kind = |k: &str| Filter {
        kind: Some(k.into()),
        ..Filter::default()
    };
    let pushed = !log.read(after, upto, &kind("PushEvent"), 1)?.is_empty();
    let mut numbers = BTreeSet::new();
    let mut from = after;
    loop {
        let got = log.read(from, upto, &kind("PullRequestEvent"), 5000)?;
        for e in &got {
            let v: Value = serde_json::from_str(&e.json).unwrap_or_default();
            let p = &v["payload"];
            if let Some(n) = p["number"]
                .as_u64()
                .or_else(|| p["pull_request"]["number"].as_u64())
            {
                numbers.insert(n);
            }
        }
        match got.last() {
            Some(e) if got.len() == 5000 => from = e.seq,
            _ => break,
        }
    }
    Ok((numbers.into_iter().collect(), pushed))
}

pub async fn run(app: Arc<App>, dir: PathBuf) {
    let copies: Vec<(Arc<Archive>, Arc<Mirror>)> = app
        .archive
        .iter()
        .filter(|a| !a.is_private())
        .map(|a| (a.clone(), Arc::new(Mirror::of(&dir, &a.repo))))
        .collect();
    if copies.is_empty() {
        return;
    }
    let mut wake = app.events.subscribe();
    let mut seen = app.events.head();
    let mut branches_at: Option<Instant> = None;
    loop {
        let head = app.events.head();
        let (numbers, pushed) = hints(&app.events, seen, head).unwrap_or_else(|e| {
            tracing::warn!("git copy: read the event log: {e:#}");
            (Vec::new(), false)
        });
        seen = head;
        let branches = pushed || branches_at.is_none_or(|t| t.elapsed() >= BRANCHES_EVERY);
        if branches {
            branches_at = Some(Instant::now());
        }
        for (a, m) in &copies {
            let numbers = if a.repo.eq_ignore_ascii_case(crate::events::REPO) {
                numbers.clone()
            } else {
                Vec::new()
            };
            let (a2, m2) = (a.clone(), m.clone());
            match tokio::task::spawn_blocking(move || pass(&a2, &m2, branches, &numbers)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("git copy of {}: {e:#}", a.repo),
                Err(e) => tracing::warn!("git copy of {}: {e}", a.repo),
            }
        }
        let wait =
            BRANCHES_EVERY.saturating_sub(branches_at.map_or(BRANCHES_EVERY, |t| t.elapsed()));
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            Ok(()) = wake.changed() => tokio::time::sleep(SETTLE).await,
        }
    }
}

/// The state of the copy for `/api/v1/sync`, or null on a server without it.
pub fn status(a: &Archive) -> Result<Value> {
    let Some(tried) = a.get(TRIED)? else {
        return Ok(Value::Null);
    };
    let counts: Value = a
        .get(COUNTS)?
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or(Value::Null);
    let num = |k: &str| -> Result<u64> { Ok(a.get(k)?.and_then(|v| v.parse().ok()).unwrap_or(0)) };
    Ok(json!({
        "branches": counts["branches"],
        "tags": counts["tags"],
        "pull_request_heads": counts["heads"],
        "replaced_heads": num(REPLACED)?,
        "bytes": num(BYTES)?,
        "full_at": a.get(FULL)?,
        "tried_at": tried,
        "fetched_at": a.get(FETCHED)?,
        "branches_every_seconds": BRANCHES_EVERY.as_secs(),
        "full_every_hours": FULL_EVERY_HOURS,
        "last_error": a.get(ERROR)?.filter(|e| !e.is_empty()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(h: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-25T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::hours(h)
    }

    #[test]
    fn a_full_fetch_comes_every_six_hours_and_an_hour_after_a_failure() {
        assert!(full_due(None, None, at(0)));
        assert!(!full_due(None, Some(at(0)), at(0)));
        assert!(full_due(None, Some(at(0)), at(1)));
        assert!(!full_due(Some(at(1)), Some(at(0)), at(6)));
        assert!(full_due(Some(at(1)), Some(at(0)), at(7)));
        assert!(!full_due(Some(at(1)), Some(at(7)), at(7)));
        assert!(full_due(Some(at(1)), Some(at(7)), at(8)));
    }

    #[test]
    fn porcelain_counts_new_moved_and_forced_refs() {
        let out = "* 0000 aaaa refs/pull/1/head\n  aaaa bbbb refs/heads/master\n+ bbbb cccc refs/pull/2/head\n= cccc cccc refs/tags/x\n";
        assert_eq!(
            Fetched::parse(out),
            Fetched {
                new: 1,
                moved: 1,
                forced: 1
            }
        );
    }

    struct Up(PathBuf);

    impl Up {
        fn new(name: &str) -> Self {
            let d = std::env::temp_dir().join(format!("mm-mirror-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            let up = Up(d);
            up.git(&["init", "--quiet", "--initial-branch=master", "up"]);
            up
        }

        fn git(&self, args: &[&str]) -> String {
            let mut a = vec!["-c", "user.name=t", "-c", "user.email=t@example.org"];
            a.extend(args);
            git(&self.0, &a).unwrap().trim().to_string()
        }

        fn commit(&self, msg: &str) -> String {
            self.git(&["-C", "up", "commit", "--quiet", "--allow-empty", "-m", msg]);
            self.git(&["-C", "up", "rev-parse", "HEAD"])
        }

        fn mirror(&self) -> Mirror {
            Mirror {
                dir: self.0.join("copy").join("o").join("r.git"),
                url: format!("file://{}", self.0.join("up").display()),
            }
        }
    }

    impl Drop for Up {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_copy_keeps_a_head_that_a_force_push_replaced() {
        let up = Up::new("force");
        let first = up.commit("a");
        up.git(&["-C", "up", "tag", "v1"]);
        up.git(&["-C", "up", "update-ref", "refs/pull/1/head", &first]);
        up.git(&["-C", "up", "update-ref", "refs/pull/1/merge", &first]);
        let m = up.mirror();
        m.init().unwrap();
        m.init().unwrap();
        assert_eq!(m.full().unwrap().new, 3);
        assert_eq!(
            m.counts().unwrap(),
            Counts {
                branches: 1,
                tags: 1,
                heads: 1
            }
        );
        up.git(&["-C", "up", "checkout", "--quiet", "--orphan", "other"]);
        let second = up.commit("b");
        up.git(&["-C", "up", "update-ref", "refs/pull/1/head", &second]);
        up.git(&["-C", "up", "update-ref", "refs/pull/2/head", &second]);
        let f = m.heads(&[1, 2, 3]).unwrap();
        assert_eq!((f.new, f.forced), (1, 1));
        m.pack().unwrap();
        assert_eq!(m.run(&["cat-file", "-t", &first]).unwrap().trim(), "commit");
        assert_eq!(
            m.run(&["rev-parse", "refs/pull/1/head"]).unwrap().trim(),
            second
        );
        assert_eq!(
            m.run(&["rev-parse", "refs/pull/1/head@{1}"])
                .unwrap()
                .trim(),
            first
        );
        assert!(m.bytes().unwrap() > 0);
    }
}
