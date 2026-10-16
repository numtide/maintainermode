//! Advisories from OSV for the packages whose ecosystem is known: a source
//! from PyPI, crates.io or npm, or an attribute of a Python package set.
//! OSV matches the name and the exact version nixpkgs ships, so a match is
//! precise. Adapted from nixfresh's osv collector (MIT).

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

use crate::health::{Health, Run};
use crate::severity::Band;
use crate::upstream::Origin;

const API: &str = "https://api.osv.dev/v1";
const BATCH: usize = 1000;
const DETAIL_BUDGET: usize = 200;
const PAUSE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Vuln {
    #[serde(default)]
    pub summary: Option<String>,
    /// The CVE ids among its aliases.
    #[serde(default)]
    pub cves: Vec<String>,
    /// Every alias: OSV lists one vulnerability under GHSA and PYSEC ids.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// GitHub's band, from `database_specific.severity`.
    #[serde(default)]
    pub band: Option<Band>,
}

impl Vuln {
    pub fn url(id: &str) -> String {
        format!("https://osv.dev/vulnerability/{id}")
    }
}

/// One package to ask about.
pub struct Query {
    pub attr: String,
    pub ecosystem: &'static str,
    pub name: String,
    pub version: String,
}

/// The ecosystem of an attribute: its registry source, or its Python set.
pub fn query(attr: &str, pname: &str, version: &str, src_url: Option<&str>) -> Option<Query> {
    if version.is_empty() {
        return None;
    }
    let (ecosystem, name) = match src_url.and_then(crate::upstream::origin) {
        Some(Origin::Registry { ecosystem, name }) => (ecosystem, name),
        _ => {
            let set = attr.split_once('.')?.0;
            (set.starts_with("python") && set.ends_with("Packages")).then_some(())?;
            ("PyPI", pname.to_string())
        }
    };
    Some(Query {
        attr: attr.to_string(),
        ecosystem,
        name,
        version: version.to_string(),
    })
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("maintainer-mode (+https://github.com/numtide/maintainermode)")
        .timeout(Duration::from_secs(60))
        .build()
        .expect("http client")
}

async fn batch(http: &reqwest::Client, chunk: &[Query]) -> Result<Vec<Vec<String>>> {
    let body = json!({
        "queries": chunk.iter().map(|q| json!({
            "package": { "name": q.name, "ecosystem": q.ecosystem },
            "version": q.version,
        })).collect::<Vec<_>>()
    });
    let v: Value = http
        .post(format!("{API}/querybatch"))
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(v["results"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            r["vulns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x["id"].as_str().map(String::from))
                .collect()
        })
        .collect())
}

fn vuln(v: &Value) -> Vuln {
    let mut cves: Vec<String> = v["aliases"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(std::iter::once(&v["id"]))
        .filter_map(|a| a.as_str())
        .filter(|a| a.starts_with("CVE-"))
        .map(String::from)
        .collect();
    cves.sort();
    cves.dedup();
    Vuln {
        summary: v["summary"]
            .as_str()
            .or(v["details"].as_str())
            .map(|s| s.chars().take(300).collect()),
        cves,
        aliases: v["aliases"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str().map(String::from))
            .collect(),
        band: v["database_specific"]["severity"]
            .as_str()
            .and_then(Band::parse),
    }
}

/// Every known package once a day, 1000 to a request.
pub async fn matches(h: &mut Health, queries: Vec<Query>) -> Run {
    let http = http();
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for chunk in queries.chunks(BATCH) {
        let results = match batch(&http, chunk).await {
            Ok(r) => r,
            Err(e) => return Run::failed(format!("OSV: {e:#}")),
        };
        for (q, mut ids) in chunk.iter().zip(results) {
            ids.sort();
            if !ids.is_empty() {
                found.insert(q.attr.clone(), ids);
            }
        }
        tokio::time::sleep(PAUSE).await;
    }
    let n = found.values().map(Vec::len).sum::<usize>();
    let line = format!(
        "{} packages asked · {} with advisories, {n} matches",
        queries.len(),
        found.len()
    );
    h.osv = found;
    h.osv_at = Some(Utc::now());
    Run::ok(line)
}

/// The record of each matched id, once: 200 a pass.
pub async fn details(h: &mut Health) -> Run {
    let http = http();
    let mut due: Vec<String> = h
        .osv
        .values()
        .flatten()
        .filter(|id| !h.osv_vulns.contains_key(*id))
        .cloned()
        .collect();
    due.sort();
    due.dedup();
    let mut read = 0;
    for id in due.iter().take(DETAIL_BUDGET) {
        let got = async {
            anyhow::Ok(
                http.get(format!("{API}/vulns/{id}"))
                    .send()
                    .await?
                    .error_for_status()?
                    .json::<Value>()
                    .await?,
            )
        };
        match got.await {
            Ok(v) => {
                h.osv_vulns.insert(id.clone(), vuln(&v));
                read += 1;
            }
            Err(e) => return Run::failed(format!("OSV records: {e:#}")),
        }
        tokio::time::sleep(PAUSE).await;
    }
    Run::ok(format!(
        "{read} records read · {} left",
        due.len().saturating_sub(read)
    ))
}

/// When this was last asked, for the daily check.
pub fn due(at: Option<DateTime<Utc>>) -> bool {
    at.is_none_or(|t| Utc::now() - t > chrono::Duration::hours(24))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ecosystem_comes_from_the_source_or_the_set() {
        let q = query(
            "python3Packages.requests",
            "requests",
            "2.32",
            Some("https://github.com/psf/requests/archive/v2.32.tar.gz"),
        )
        .unwrap();
        assert_eq!((q.ecosystem, q.name.as_str()), ("PyPI", "requests"));
        let q = query(
            "ripgrep",
            "ripgrep",
            "14",
            Some("https://crates.io/api/v1/crates/ripgrep/14.0.0/download"),
        )
        .unwrap();
        assert_eq!(q.ecosystem, "crates.io");
        assert!(
            query(
                "hello",
                "hello",
                "2.12",
                Some("mirror://gnu/hello/hello-2.12.tar.gz")
            )
            .is_none()
        );
        assert!(query("python3Packages.x", "x", "", None).is_none());
    }

    #[test]
    fn a_record_keeps_its_cve_aliases_and_band() {
        let v = json!({
            "id": "GHSA-xxxx", "aliases": ["CVE-2024-1", "PYSEC-1"],
            "summary": "bad", "database_specific": {"severity": "MODERATE"}
        });
        let r = vuln(&v);
        assert_eq!(r.cves, vec!["CVE-2024-1"]);
        assert_eq!(r.aliases, vec!["CVE-2024-1", "PYSEC-1"]);
        assert_eq!(r.band, Some(Band::Medium));
        assert_eq!(r.summary.as_deref(), Some("bad"));
    }
}
