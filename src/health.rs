//! Package health from public sources: the newest upstream version from
//! repology, advisories from the nixpkgs security tracker, and the last build
//! on Hydra. The first two are bulk sweeps, kept a day; Hydra is asked per
//! page view and cached. Adapted from nixfresh's collectors (MIT).

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::web::App;

const REPOLOGY: &str = "https://repology.org/api/v1/projects";
const REPOLOGY_REPO: &str = "nix_unstable";
/// repology asks for no more than one request a second.
const REPOLOGY_PAUSE: Duration = Duration::from_millis(1100);
const REPOLOGY_PAGES: usize = 100;
const TRACKER: &str = "https://tracker.security.nixos.org/api/v1/issues";
const TRACKER_PAGES: usize = 200;
/// The tracker answers 429 after about 30 quick pages.
const TRACKER_PAUSE: Duration = Duration::from_millis(2500);
const HYDRA: &str = "https://hydra.nixos.org";
const HYDRA_TTL: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Advisory {
    /// `NIXPKGS-2026-3072`.
    pub code: String,
    pub cves: Vec<String>,
    pub title: String,
    /// The nixpkgs issue the tracker opened for it.
    pub issue: Option<u64>,
}

impl Advisory {
    pub fn url(&self) -> String {
        format!("https://tracker.security.nixos.org/issue/{}", self.code)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Health {
    /// attribute → newest upstream version, only where nixpkgs is behind.
    pub upstream: BTreeMap<String, String>,
    /// attribute → advisories the tracker attaches to it.
    pub advisories: BTreeMap<String, Vec<Advisory>>,
    pub repology_at: Option<DateTime<Utc>>,
    pub tracker_at: Option<DateTime<Utc>>,
    /// attribute → the upstream version and when a refresh first saw it, so
    /// a daily refresh does not make an old release new again.
    #[serde(default)]
    pub upstream_seen: BTreeMap<String, (String, DateTime<Utc>)>,
    /// CVE id → severity and EPSS, for the CVEs the tracker names.
    #[serde(default)]
    pub cves: BTreeMap<String, crate::severity::Cve>,
    #[serde(default)]
    pub epss_at: Option<DateTime<Utc>>,
    /// attribute → the newest release from its git tags or its registry.
    #[serde(default)]
    pub direct: BTreeMap<String, crate::upstream::Direct>,
    /// attribute → the OSV ids that match the version nixpkgs ships.
    #[serde(default)]
    pub osv: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub osv_vulns: BTreeMap<String, crate::osv::Vuln>,
    /// OSV id → failures in a row and the last one, for the backoff.
    #[serde(default)]
    pub osv_failed: BTreeMap<String, (u32, DateTime<Utc>)>,
    #[serde(default)]
    pub osv_at: Option<DateTime<Utc>>,
    /// source → what its last pass did.
    #[serde(default)]
    pub runs: BTreeMap<String, Run>,
}

/// What one source did on its last pass, for `/sync`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub at: DateTime<Utc>,
    pub ok: bool,
    pub line: String,
}

impl Run {
    pub fn ok(line: String) -> Run {
        Run {
            at: Utc::now(),
            ok: true,
            line,
        }
    }

    pub fn failed(line: String) -> Run {
        Run {
            at: Utc::now(),
            ok: false,
            line,
        }
    }
}

impl Health {
    /// The first fill dates every release to the epoch: a backlog is not news.
    pub fn set_upstream(&mut self, upstream: BTreeMap<String, String>, now: DateTime<Utc>) {
        let first = self.upstream_seen.is_empty();
        let mut seen = std::mem::take(&mut self.upstream_seen);
        self.upstream_seen = upstream
            .iter()
            .map(|(attr, v)| {
                let at = match seen.remove(attr) {
                    Some((old, at)) if old == *v => at,
                    _ if first => DateTime::UNIX_EPOCH,
                    _ => now,
                };
                (attr.clone(), (v.clone(), at))
            })
            .collect();
        self.upstream = upstream;
        self.repology_at = Some(now);
    }

    /// When `version` of `attr` was first seen upstream.
    pub fn upstream_since(&self, attr: &str, version: &str) -> Option<DateTime<Utc>> {
        let direct = self
            .direct
            .get(attr)
            .filter(|d| d.latest.as_deref() == Some(version))
            .map(|d| d.seen_at);
        let repology = self
            .upstream_seen
            .get(attr)
            .filter(|(v, _)| v == version)
            .map(|(_, at)| *at);
        direct.into_iter().chain(repology).min()
    }

    /// OSV matches the tracker does not already name by a CVE.
    pub fn osv_extra(&self, attr: &str) -> Vec<(&str, Option<&crate::osv::Vuln>)> {
        let tracked: std::collections::HashSet<&String> = self
            .advisories
            .get(attr)
            .into_iter()
            .flatten()
            .flat_map(|a| a.cves.iter())
            .collect();
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut out = Vec::new();
        for id in self.osv.get(attr).into_iter().flatten() {
            let v = self.osv_vulns.get(id);
            if seen.contains(id.as_str())
                || v.is_some_and(|v| {
                    v.cves.iter().any(|c| tracked.contains(c))
                        || v.aliases.iter().any(|a| seen.contains(a.as_str()))
                })
            {
                continue;
            }
            seen.insert(id);
            seen.extend(
                v.into_iter()
                    .flat_map(|v| v.aliases.iter().map(String::as_str)),
            );
            out.push((id.as_str(), v));
        }
        out
    }

    /// The newest upstream release when it is newer than `current`, and
    /// where it comes from. The package's own tags or registry overrule
    /// repology; repology counts only for a release on the same numbering
    /// scheme.
    pub fn newest(&self, attr: &str, current: &str) -> Option<(&str, &str)> {
        if let Some(d) = self.direct.get(attr) {
            if let Some(v) = &d.latest {
                return (crate::version::compare(v, current) == std::cmp::Ordering::Greater)
                    .then_some((v.as_str(), d.from.as_str()));
            }
        }
        let r = self.upstream.get(attr)?;
        crate::upstream::newer(r, current).then_some((r.as_str(), "repology"))
    }
}

/// The health sources run in their own task, so a slow source never holds
/// back the pull request and issue sync.
pub const PASS: Duration = Duration::from_secs(600);

fn stale(t: Option<DateTime<Utc>>) -> bool {
    t.is_none_or(|t| Utc::now() - t > chrono::Duration::hours(24))
}

fn publish(app: &App, h: &Health) {
    match serde_json::to_string(h) {
        Ok(s) => {
            if let Err(e) = app.store.set("health", &s) {
                tracing::warn!("health: {e:#}");
            }
        }
        Err(e) => tracing::warn!("health: {e:#}"),
    }
    *app.health.write().unwrap() = Arc::new(h.clone());
    app.recompute_packages();
}

/// One attribute per package name, plus every attribute someone maintains
/// or watches, with a source that can be asked directly.
fn candidates(app: &App) -> Vec<crate::upstream::Candidate> {
    use crate::catalog::Subject;
    let index = app.index.read().unwrap();
    let mut first: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in app.store.members().unwrap_or_default() {
        for (sub, _) in crate::catalog::maintained(&index, &m.login).packages {
            if let Subject::Pkg(a) = sub {
                first.insert(a);
            }
        }
    }
    for key in app.store.watched_by_anyone().unwrap_or_default() {
        if let Some(Subject::Pkg(a)) = Subject::parse(&key) {
            first.insert(a);
        }
    }
    let attrs: std::collections::BTreeSet<&String> =
        index.by_pname.values().chain(first.iter()).collect();
    attrs
        .into_iter()
        .filter_map(|attr| {
            let p = index.pkgs.get(attr)?;
            let url = p.facts.as_ref()?.src_url.as_deref()?;
            Some(crate::upstream::Candidate {
                attr: attr.clone(),
                pname: p.pname.clone(),
                version: p.version.clone(),
                origin: crate::upstream::origin(url)?,
                first: first.contains(attr),
            })
        })
        .filter(|c| !c.version.is_empty())
        .collect()
}

fn osv_queries(app: &App) -> Vec<crate::osv::Query> {
    let index = app.index.read().unwrap();
    index
        .pkgs
        .iter()
        .filter_map(|(attr, p)| {
            let url = p.facts.as_ref().and_then(|f| f.src_url.as_deref());
            crate::osv::query(attr, &p.pname, &p.version, url)
        })
        .collect()
}

fn note(h: &mut Health, source: &str, r: Run) {
    if r.ok {
        tracing::info!("{source}: {}", r.line);
    } else {
        tracing::warn!("{source}: {}", r.line);
    }
    h.runs.insert(source.to_string(), r);
}

/// Reads only the sources that are due, so a source that is down is asked
/// again on the next pass while the others keep their day.
pub async fn run(app: Arc<App>, nvd_key: Option<String>) {
    let mut h: Health = (**app.health.read().unwrap()).clone();
    loop {
        if stale(h.repology_at) {
            match repology().await {
                Ok(u) => {
                    h.set_upstream(u, Utc::now());
                    publish(&app, &h);
                }
                Err(e) => tracing::warn!("repology: {e:#}"),
            }
        }
        if stale(h.tracker_at) {
            match tracker().await {
                Ok(a) => {
                    h.advisories = a;
                    h.tracker_at = Some(Utc::now());
                    publish(&app, &h);
                }
                Err(e) => tracing::warn!("security tracker: {e:#}"),
            }
        }
        let r = crate::severity::records(&mut h).await;
        note(&mut h, "cve", r);
        publish(&app, &h);
        let r = crate::severity::nvd(&mut h, nvd_key.as_deref()).await;
        note(&mut h, "nvd", r);
        if h.tracker_at.is_some() && stale(h.epss_at) {
            let r = crate::severity::epss(&mut h).await;
            if r.ok {
                h.epss_at = Some(r.at);
            }
            note(&mut h, "epss", r);
        }
        publish(&app, &h);
        if crate::osv::due(h.osv_at) {
            let q = osv_queries(&app);
            if !q.is_empty() {
                let r = crate::osv::matches(&mut h, q).await;
                note(&mut h, "osv", r);
            }
        }
        if !h.osv.is_empty() {
            let r = crate::osv::details(&mut h).await;
            note(&mut h, "osv-records", r);
        }
        publish(&app, &h);
        let c = candidates(&app);
        let r = if c.is_empty() {
            Run::ok("waiting for an index with source URLs".into())
        } else {
            crate::upstream::pass(&mut h, c).await
        };
        note(&mut h, "upstream", r);
        publish(&app, &h);
        tokio::time::sleep(PASS).await;
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("maintainer-mode (+https://github.com/numtide/maintainermode)")
        .timeout(Duration::from_secs(60))
        .build()
        .expect("http client")
}

/// Every project repology calls outdated in nixpkgs unstable, page by page.
/// The cursor is the last project name of a page.
pub async fn repology() -> Result<BTreeMap<String, String>> {
    let http = http();
    let mut out = BTreeMap::new();
    let mut cursor = String::new();
    for _ in 0..REPOLOGY_PAGES {
        let url = if cursor.is_empty() {
            format!("{REPOLOGY}/?inrepo={REPOLOGY_REPO}&outdated=1")
        } else {
            format!(
                "{REPOLOGY}/{}/?inrepo={REPOLOGY_REPO}&outdated=1",
                crate::github::urlencode(&cursor)
            )
        };
        let page: BTreeMap<String, Vec<Value>> = http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let count = page.len();
        for (project, entries) in &page {
            let Some(newest) = entries
                .iter()
                .find(|e| e["status"] == "newest")
                .and_then(|e| e["version"].as_str())
            else {
                continue;
            };
            for e in entries
                .iter()
                .filter(|e| e["repo"] == REPOLOGY_REPO && e["status"] == "outdated")
            {
                if let Some(attr) = e["srcname"].as_str() {
                    out.insert(attr.to_string(), newest.to_string());
                }
            }
            if *project > cursor {
                cursor = project.clone();
            }
        }
        if count < 200 {
            break;
        }
        tokio::time::sleep(REPOLOGY_PAUSE).await;
    }
    Ok(out)
}

fn issue_number(url: &str) -> Option<u64> {
    url.strip_prefix("https://github.com/NixOS/nixpkgs/issues/")?
        .parse()
        .ok()
}

/// Every tracker issue with its suggestions; each names the attributes it
/// affects.
pub async fn tracker() -> Result<BTreeMap<String, Vec<Advisory>>> {
    let http = http();
    let mut out: BTreeMap<String, Vec<Advisory>> = BTreeMap::new();
    let mut url = format!("{TRACKER}?expand=suggestions&limit=100");
    for n in 0..TRACKER_PAGES {
        let fetched = async {
            anyhow::Ok(
                http.get(&url)
                    .send()
                    .await?
                    .error_for_status()?
                    .json::<Value>()
                    .await?,
            )
        };
        // the newest issues come first, so a throttled sweep keeps what it read
        let page = match fetched.await {
            Ok(p) => p,
            Err(e) if n > 0 => {
                tracing::warn!("security tracker stopped at page {n}: {e:#}");
                break;
            }
            Err(e) => return Err(e),
        };
        for issue in page["results"].as_array().into_iter().flatten() {
            let Some(code) = issue["code"].as_str() else {
                continue;
            };
            let suggestions = issue["suggestions"].as_array().cloned().unwrap_or_default();
            let mut cves: Vec<String> = suggestions
                .iter()
                .filter_map(|s| s["cve_id"].as_str().map(String::from))
                .collect();
            cves.sort();
            cves.dedup();
            let advisory = Advisory {
                code: code.to_string(),
                cves,
                title: issue["title"].as_str().unwrap_or(code).to_string(),
                issue: issue["github_issue_url"].as_str().and_then(issue_number),
            };
            let mut attrs: Vec<&str> = suggestions
                .iter()
                .filter_map(|s| s["packages"].as_object())
                .flat_map(|p| p.keys().map(String::as_str))
                .collect();
            attrs.sort();
            attrs.dedup();
            for a in attrs {
                out.entry(a.to_string()).or_default().push(advisory.clone());
            }
        }
        tokio::time::sleep(TRACKER_PAUSE).await;
        match page["next"].as_str() {
            Some(next) => url = next.replacen("http://", "https://", 1),
            None => break,
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct Build {
    pub id: u64,
    pub ok: bool,
    /// Hydra's words for `buildstatus`.
    pub status: &'static str,
    pub at: Option<DateTime<Utc>>,
}

impl Build {
    pub fn url(&self) -> String {
        format!("{HYDRA}/build/{}", self.id)
    }
}

fn status_word(code: i64) -> &'static str {
    match code {
        0 => "succeeded",
        1 => "failed",
        2 => "dependency failed",
        3 | 4 => "aborted",
        6 => "failed with output",
        7 => "timed out",
        10 => "log limit exceeded",
        11 => "output limit exceeded",
        _ => "failed",
    }
}

/// The last finished Hydra build of a job, cached an hour.
pub struct Hydra {
    http: reqwest::Client,
    cache: Mutex<HashMap<String, (Instant, Option<Build>)>>,
}

impl Default for Hydra {
    fn default() -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent("maintainer-mode")
                .timeout(Duration::from_secs(5))
                .build()
                .expect("http client"),
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl Hydra {
    pub fn package_job(attr: &str) -> String {
        format!("nixpkgs/unstable/{attr}.x86_64-linux")
    }

    pub fn test_job(name: &str) -> String {
        format!("nixos/trunk-combined/nixos.tests.{name}.x86_64-linux")
    }

    pub fn job_url(job: &str) -> String {
        format!("{HYDRA}/job/{job}")
    }

    /// `None` when the job does not exist or Hydra does not answer in time.
    pub async fn last(&self, job: &str) -> Option<Build> {
        if let Some((at, b)) = self.cache.lock().unwrap().get(job) {
            if at.elapsed() < HYDRA_TTL {
                return b.clone();
            }
        }
        let resp = self
            .http
            .get(format!("{}/latest-finished", Self::job_url(job)))
            .header("Accept", "application/json")
            .send()
            .await
            .ok()?;
        let b = if resp.status().is_success() {
            resp.json::<Value>().await.ok().and_then(|v| {
                let code = v["buildstatus"].as_i64()?;
                Some(Build {
                    id: v["id"].as_u64()?,
                    ok: code == 0,
                    status: status_word(code),
                    at: v["stoptime"]
                        .as_i64()
                        .or(v["timestamp"].as_i64())
                        .and_then(|t| DateTime::from_timestamp(t, 0)),
                })
            })
        } else if resp.status() == reqwest::StatusCode::NOT_FOUND {
            None
        } else {
            return None;
        };
        self.cache
            .lock()
            .unwrap()
            .insert(job.to_string(), (Instant::now(), b.clone()));
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tracker_issue_link_gives_the_issue_number() {
        assert_eq!(
            issue_number("https://github.com/NixOS/nixpkgs/issues/574783"),
            Some(574783)
        );
        assert_eq!(issue_number("https://example.org/1"), None);
    }

    #[test]
    fn an_osv_match_shows_once_and_not_beside_its_tracker_advisory() {
        let mut h = Health::default();
        h.advisories.insert(
            "requests".into(),
            vec![Advisory {
                code: "NIXPKGS-1".into(),
                cves: vec!["CVE-1".into()],
                title: String::new(),
                issue: None,
            }],
        );
        h.osv.insert(
            "requests".into(),
            vec!["GHSA-a".into(), "GHSA-b".into(), "PYSEC-b".into()],
        );
        let v = |cves: &[&str], aliases: &[&str]| crate::osv::Vuln {
            cves: cves.iter().map(|s| s.to_string()).collect(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        h.osv_vulns
            .insert("GHSA-a".into(), v(&["CVE-1"], &["CVE-1"]));
        h.osv_vulns.insert("GHSA-b".into(), v(&[], &[]));
        h.osv_vulns.insert("PYSEC-b".into(), v(&[], &["GHSA-b"]));
        let ids: Vec<&str> = h
            .osv_extra("requests")
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(ids, vec!["GHSA-b"]);
    }
}
