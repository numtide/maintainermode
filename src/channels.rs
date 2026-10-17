//! nixpkgs channels next to master: the revision each channel is at and the
//! version of each attribute there. A fix merged in master reaches people only
//! once a channel has it, and the stable channel only after a backport.

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use crate::web::App;

const CHANNELS: &str = "https://channels.nixos.org";
/// The configured name that means the newest stable channel.
pub const STABLE: &str = "stable";
/// The last name `stable` tries, when no newer one exists.
const FALLBACK_STABLE: &str = "nixos-26.05";
const EVERY: std::time::Duration = std::time::Duration::from_secs(3600);
/// A channel waits while the master index is this old: the master sweep is
/// close, and two sweeps at once double the peak memory.
const MASTER_DUE: i64 = 22;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Channel {
    /// As configured: a channel name, or `stable`.
    pub key: String,
    /// The channel it resolved to, like `nixos-26.05`.
    pub name: String,
    pub rev: String,
    pub read_at: Option<DateTime<Utc>>,
    /// attribute → version.
    #[serde(default)]
    pub versions: HashMap<String, String>,
}

impl Channel {
    pub fn stable(&self) -> bool {
        !self.name.contains("unstable")
    }

    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.key
        } else {
            &self.name
        }
    }

    pub fn url(&self) -> String {
        format!("https://github.com/NixOS/nixpkgs/tree/{}", self.rev)
    }
}

/// In the configured order. A channel not read yet has no revision.
#[derive(Debug, Default)]
pub struct Channels {
    pub list: Vec<Channel>,
}

/// One channel's view of an attribute, against master.
pub struct Seen<'a> {
    pub channel: &'a Channel,
    pub version: Option<&'a str>,
    /// master carries a newer version than this channel.
    pub behind: bool,
}

impl Channels {
    pub fn of<'a>(&'a self, attr: &str, master: &str) -> Vec<Seen<'a>> {
        self.list
            .iter()
            .filter(|c| !c.rev.is_empty())
            .map(|c| {
                let version = c.versions.get(attr).map(String::as_str);
                Seen {
                    channel: c,
                    version,
                    behind: version.is_some_and(|v| compare(master, v) == Ordering::Greater),
                }
            })
            .collect()
    }

    /// The channels whose version of `attr` is older than master's.
    pub fn behind(&self, attr: &str, master: &str) -> Vec<&Channel> {
        self.of(attr, master)
            .into_iter()
            .filter(|s| s.behind)
            .map(|s| s.channel)
            .collect()
    }

    /// The stable channels that still need a backport of master's version.
    pub fn backport(&self, attr: &str, master: &str) -> Vec<&Channel> {
        self.behind(attr, master)
            .into_iter()
            .filter(|c| c.stable())
            .collect()
    }
}

/// The parts of a version: runs of digits and runs of other characters,
/// split at `.` and `-`, as `builtins.splitVersion` does.
fn parts(v: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None;
    let mut digit = false;
    for (i, c) in v.char_indices() {
        if c == '.' || c == '-' {
            if let Some(s) = start.take() {
                out.push(&v[s..i]);
            }
            continue;
        }
        match start {
            Some(s) if c.is_ascii_digit() != digit => {
                out.push(&v[s..i]);
                start = Some(i);
            }
            None => start = Some(i),
            _ => {}
        }
        digit = c.is_ascii_digit();
    }
    if let Some(s) = start {
        out.push(&v[s..]);
    }
    out
}

fn number(p: &str) -> bool {
    !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())
}

/// `builtins.compareVersions`, part by part.
fn less(a: &str, b: &str) -> bool {
    match (number(a), number(b)) {
        (true, true) => {
            let (a, b) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
            (a.len(), a) < (b.len(), b)
        }
        _ if a.is_empty() && number(b) => true,
        _ if a == "pre" && b != "pre" => true,
        _ if b == "pre" => false,
        (_, true) => true,
        (true, _) => false,
        _ => a < b,
    }
}

pub fn compare(a: &str, b: &str) -> Ordering {
    let (pa, pb) = (parts(a), parts(b));
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or("");
        let y = pb.get(i).copied().unwrap_or("");
        if less(x, y) {
            return Ordering::Less;
        }
        if less(y, x) {
            return Ordering::Greater;
        }
    }
    Ordering::Equal
}

/// The stable releases out by a date, newest first. A release ships at the
/// end of its month; its channel exists earlier, as a beta.
fn stable_candidates(today: chrono::NaiveDate) -> Vec<String> {
    let y = today.year() % 100;
    [(y, 11), (y, 5), (y - 1, 11), (y - 1, 5)]
        .into_iter()
        .filter(|&(yy, mm)| (yy, mm) < (y, today.month() as i32))
        .map(|(yy, mm)| format!("nixos-{yy:02}.{mm:02}"))
        .collect()
}

/// The first name that exists, with its revision. Only a 404 moves on to
/// an older one: a timeout on the newest must not make `stable` go back.
async fn first_found<F, Fut>(names: Vec<String>, mut probe: F) -> Result<Option<(String, String)>>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<String>>>,
{
    for name in names {
        if let Some(rev) = probe(name.clone()).await? {
            return Ok(Some((name, rev)));
        }
    }
    Ok(None)
}

/// The channel name a key means, and its revision.
async fn resolve(http: &reqwest::Client, key: &str) -> Result<(String, String)> {
    let names = if key == STABLE {
        stable_candidates(Utc::now().date_naive())
            .into_iter()
            .chain(std::iter::once(FALLBACK_STABLE.to_string()))
            .collect()
    } else {
        vec![key.to_string()]
    };
    first_found(names, |name| async move { revision(http, &name).await })
        .await?
        .with_context(|| format!("no channel found for {key}"))
}

async fn revision(http: &reqwest::Client, name: &str) -> Result<Option<String>> {
    let resp = http
        .get(format!("{CHANNELS}/{name}/git-revision"))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let rev = resp.error_for_status()?.text().await?.trim().to_string();
    anyhow::ensure!(
        rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit()),
        "unexpected revision {rev:?} for {name}"
    );
    Ok(Some(rev))
}

const VERSIONS_EXPR: &str = r#"
let
  input = builtins.fromJSON (builtins.readFile ./input.json);
  pkgs = import input.path {
    inherit (input) system;
    overlays = [ ];
    config = {
      allowUnfree = true;
      allowBroken = true;
      allowInsecurePredicate = _: true;
    };
  };
  lib = pkgs.lib;
  root = if input.set == "" then pkgs else lib.attrByPath (lib.splitString "." input.set) null pkgs;
  prefix = if input.set == "" then "" else input.set + ".";
  version =
    name:
    let
      v = builtins.tryEval (
        let
          p = builtins.getAttr name root;
          x = p.version or (builtins.parseDrvName (p.name or "")).version;
        in
        if lib.isDerivation p && builtins.isString x then builtins.seq x x else null
      );
    in
    if v.success then v.value else null;
in
if !(builtins.isAttrs root) then
  { }
else
  lib.filterAttrs (_: v: v != null) (
    lib.listToAttrs (
      map (n: {
        name = prefix + n;
        value = version n;
      }) (builtins.attrNames root)
    )
  )
"#;

/// attribute → version, one set at a time, under the sweep lock.
fn sweep(dir: &std::path::Path, sets: &[String]) -> Result<HashMap<String, String>> {
    let _one = crate::eval::SWEEP.lock().unwrap_or_else(|e| e.into_inner());
    let mut out = HashMap::new();
    for set in sets {
        let input = serde_json::json!({
            "path": dir.to_string_lossy(),
            "system": "x86_64-linux",
            "set": set,
        });
        let text = crate::eval::eval_with_input("channel", VERSIONS_EXPR, &input)?;
        let found: HashMap<String, String> = serde_json::from_str(&text)?;
        out.extend(found);
    }
    Ok(out)
}

pub fn load(store: &crate::store::Store, keys: &[String]) -> Channels {
    let mut stored: HashMap<String, Channel> = store
        .channels()
        .unwrap_or_default()
        .into_iter()
        .map(|c| (c.key.clone(), c))
        .collect();
    Channels {
        list: keys
            .iter()
            .filter(|k| !k.is_empty())
            .map(|k| {
                stored.remove(k).unwrap_or_else(|| Channel {
                    key: k.clone(),
                    ..Default::default()
                })
            })
            .collect(),
    }
}

/// Reads one channel when its revision moved. Returns whether it swept.
async fn refresh(app: &App, key: &str) -> Result<bool> {
    let http = crate::github::http();
    let (name, rev) = resolve(&http, key).await?;
    let current = app.channels.read().unwrap().clone();
    if current
        .list
        .iter()
        .any(|c| c.key == key && c.name == name && c.rev == rev)
    {
        return Ok(false);
    }
    let dir = app.data_dir.join(format!("channel-{name}-{rev}"));
    let started = std::time::Instant::now();
    crate::area::download(&http, &rev, &dir).await?;
    let sets = app.index_sets.clone();
    let d = dir.clone();
    let versions = tokio::task::spawn_blocking(move || sweep(&d, &sets)).await?;
    let _ = std::fs::remove_dir_all(&dir);
    let versions = versions.with_context(|| format!("sweep {name}"))?;
    tracing::info!(
        "swept channel {name} at {}: {} attributes in {:.1}s",
        &rev[..12],
        versions.len(),
        started.elapsed().as_secs_f64()
    );
    let channel = Channel {
        key: key.to_string(),
        name,
        rev,
        read_at: Some(Utc::now()),
        versions,
    };
    app.store.put_channel(&channel)?;
    let list = current
        .list
        .iter()
        .map(|c| {
            if c.key == key {
                channel.clone()
            } else {
                c.clone()
            }
        })
        .collect();
    *app.channels.write().unwrap() = Arc::new(Channels { list });
    app.reassess();
    crate::assets::touch();
    Ok(true)
}

fn master_due(app: &App) -> bool {
    app.store
        .get_time("index_at")
        .ok()
        .flatten()
        .is_none_or(|t| Utc::now() - t > Duration::hours(MASTER_DUE))
}

/// Its own task, so a channel download and sweep never delays the pull
/// request sync. Channels go one after the other, never next to master.
pub async fn run(app: Arc<App>) {
    let keys: Vec<String> = app
        .channels
        .read()
        .unwrap()
        .list
        .iter()
        .map(|c| c.key.clone())
        .collect();
    if let Err(e) = app.store.keep_channels(&keys) {
        tracing::warn!("channels: {e:#}");
    }
    loop {
        if master_due(&app) {
            tracing::info!("channels wait for the master index");
        } else {
            for old in std::fs::read_dir(&app.data_dir)
                .into_iter()
                .flatten()
                .flatten()
            {
                if old.file_name().to_string_lossy().starts_with("channel-") {
                    let _ = std::fs::remove_dir_all(old.path());
                }
            }
            for key in &keys {
                if master_due(&app) {
                    tracing::info!("channel {key} waits for the master index");
                    break;
                }
                if let Err(e) = refresh(&app, key).await {
                    tracing::warn!("channel {key}: {e:#}");
                }
            }
            let _ = app
                .store
                .set("channels_checked_at", &Utc::now().to_rfc3339());
        }
        tokio::time::sleep(EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_nix_does() {
        assert_eq!(compare("1.2.10", "1.2.9"), Ordering::Greater);
        assert_eq!(compare("2.4.19", "2.4.19"), Ordering::Equal);
        assert_eq!(compare("1.0pre1", "1.0"), Ordering::Less);
        assert_eq!(compare("1.0", "1.0a"), Ordering::Less);
        assert_eq!(compare("1.0.1", "1.0"), Ordering::Greater);
        assert_eq!(
            compare("0-unstable-2026-10-01", "0-unstable-2026-09-30"),
            Ordering::Greater
        );
        assert_eq!(compare("1.10", "1.9"), Ordering::Greater);
        assert_eq!(parts("1.2-rc3"), ["1", "2", "rc", "3"]);
    }

    #[test]
    fn the_stable_channel_is_looked_for_newest_first() {
        let d = |s: &str| s.parse::<chrono::NaiveDate>().unwrap();
        assert_eq!(
            stable_candidates(d("2026-10-16")),
            ["nixos-26.05", "nixos-25.11", "nixos-25.05"]
        );
        assert_eq!(stable_candidates(d("2026-11-20"))[0], "nixos-26.05");
        assert_eq!(
            stable_candidates(d("2026-12-01")),
            ["nixos-26.11", "nixos-26.05", "nixos-25.11", "nixos-25.05"]
        );
    }

    #[tokio::test]
    async fn only_a_missing_channel_moves_stable_to_an_older_one() {
        let names = || vec!["nixos-26.11".to_string(), "nixos-26.05".to_string()];
        let probe = |missing: bool| {
            move |name: String| async move {
                match name.as_str() {
                    "nixos-26.11" if missing => Ok(None),
                    "nixos-26.11" => anyhow::bail!("timeout"),
                    _ => Ok(Some(format!("rev-{name}"))),
                }
            }
        };
        assert_eq!(
            first_found(names(), probe(true)).await.unwrap(),
            Some(("nixos-26.05".into(), "rev-nixos-26.05".into()))
        );
        assert!(first_found(names(), probe(false)).await.is_err());
        assert_eq!(first_found(vec![], probe(true)).await.unwrap(), None);
    }

    #[test]
    fn channels_load_in_the_configured_order_and_old_ones_go() {
        let store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        for key in ["old", STABLE] {
            store
                .put_channel(&Channel {
                    key: key.into(),
                    name: "nixos-26.05".into(),
                    rev: "r".into(),
                    ..Default::default()
                })
                .unwrap();
        }
        let keys = vec![
            "nixos-unstable".to_string(),
            STABLE.to_string(),
            String::new(),
        ];
        store.keep_channels(&keys).unwrap();
        assert_eq!(store.channels().unwrap().len(), 1);
        let c = load(&store, &keys);
        let labels: Vec<&str> = c.list.iter().map(Channel::label).collect();
        assert_eq!(labels, ["nixos-unstable", "nixos-26.05"]);
        assert!(c.list[0].rev.is_empty());
    }

    #[test]
    fn master_ahead_of_a_stable_channel_needs_a_backport() {
        let ch = |key: &str, name: &str, v: &str| Channel {
            key: key.into(),
            name: name.into(),
            rev: "r".into(),
            read_at: None,
            versions: [("cups".to_string(), v.to_string())].into(),
        };
        let c = Channels {
            list: vec![
                ch("nixos-unstable", "nixos-unstable", "2.4.20"),
                ch(STABLE, "nixos-26.05", "2.4.19"),
                Channel {
                    key: "nixos-25.11".into(),
                    ..Default::default()
                },
            ],
        };
        let names = |v: Vec<&Channel>| v.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(c.behind("cups", "2.4.20")), ["nixos-26.05"]);
        assert_eq!(names(c.backport("cups", "2.4.20")), ["nixos-26.05"]);
        assert_eq!(names(c.behind("cups", "2.4.21")).len(), 2);
        assert!(c.backport("cups", "2.4.19").is_empty());
        assert!(c.backport("absent", "1").is_empty());
        assert_eq!(c.of("cups", "2.4.20").len(), 2);
    }
}
