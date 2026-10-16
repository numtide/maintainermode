//! Package health from public sources: the newest upstream version from
//! repology, advisories from the nixpkgs security tracker, and the last build
//! on Hydra. The first two are bulk sweeps, kept a day; Hydra is asked per
//! page view and cached. Adapted from nixfresh's collectors (MIT).

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const REPOLOGY: &str = "https://repology.org/api/v1/projects";
const REPOLOGY_REPO: &str = "nix_unstable";
/// repology asks for no more than one request a second.
const REPOLOGY_PAUSE: Duration = Duration::from_millis(1100);
const REPOLOGY_PAGES: usize = 100;
const TRACKER: &str = "https://tracker.security.nixos.org/api/v1/issues";
const TRACKER_PAGES: usize = 200;
const TRACKER_PAUSE: Duration = Duration::from_millis(1000);
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

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Health {
    /// attribute → newest upstream version, only where nixpkgs is behind.
    pub upstream: BTreeMap<String, String>,
    /// attribute → advisories the tracker attaches to it.
    pub advisories: BTreeMap<String, Vec<Advisory>>,
    pub repology_at: Option<DateTime<Utc>>,
    pub tracker_at: Option<DateTime<Utc>>,
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
}
