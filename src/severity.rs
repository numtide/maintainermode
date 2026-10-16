//! How bad and how likely: CVSS for the CVEs the security tracker names,
//! from the CVE Program's own records and, with a key, from NVD; and the
//! EPSS score of FIRST. Adapted from nixfresh's cve, nvd and epss collectors
//! (MIT).

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::time::Duration;

use crate::health::{Health, Run};

/// One file per CVE, in the repository that CISA's enrichment lands in. No
/// key and no quota; NVD answers the same behind 5 requests in 30 seconds.
const RECORDS: &str = "https://raw.githubusercontent.com/CVEProject/cvelistV5/main/cves";
const RECORD_BUDGET: usize = 300;
const RECORD_PAUSE: Duration = Duration::from_millis(100);
const NVD: &str = "https://services.nvd.nist.gov/rest/json/cves/2.0";
const NVD_BUDGET: usize = 50;
/// NVD allows 50 requests in 30 seconds with a key.
const NVD_PAUSE: Duration = Duration::from_millis(700);
const EPSS: &str = "https://api.first.org/data/v1/epss";
const EPSS_CHUNK: usize = 100;
const EPSS_PAUSE: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Band {
    Low,
    Medium,
    High,
    Critical,
}

impl Band {
    pub fn parse(s: &str) -> Option<Band> {
        match s.to_ascii_lowercase().as_str() {
            "critical" => Some(Band::Critical),
            "high" => Some(Band::High),
            "medium" | "moderate" => Some(Band::Medium),
            "low" => Some(Band::Low),
            _ => None,
        }
    }

    pub fn of_score(score: f64) -> Option<Band> {
        match score {
            s if s >= 9.0 => Some(Band::Critical),
            s if s >= 7.0 => Some(Band::High),
            s if s >= 4.0 => Some(Band::Medium),
            s if s > 0.0 => Some(Band::Low),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Band::Low => "low",
            Band::Medium => "medium",
            Band::High => "high",
            Band::Critical => "critical",
        }
    }

    pub fn label_class(self) -> &'static str {
        match self {
            Band::Critical | Band::High => "Label--danger",
            Band::Medium => "Label--attention",
            Band::Low => "",
        }
    }
}

/// What is known about one CVE.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Cve {
    pub band: Option<Band>,
    pub cvss: Option<f64>,
    /// `cve` or `nvd`: where the score comes from.
    #[serde(default)]
    pub from: Option<String>,
    /// CISA's SSVC decision: `active` is exploited in the wild, `poc` has a
    /// public exploit.
    #[serde(default)]
    pub exploitation: Option<String>,
    /// The probability of exploitation in the next 30 days.
    #[serde(default)]
    pub epss: Option<f64>,
    #[serde(default)]
    pub percentile: Option<f64>,
    #[serde(default)]
    pub record_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub nvd_at: Option<DateTime<Utc>>,
}

/// The worst of the CVEs of one advisory.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Worst {
    pub band: Option<Band>,
    pub cvss: Option<f64>,
    pub epss: Option<f64>,
    pub percentile: Option<f64>,
    pub exploitation: Option<String>,
}

pub fn worst<'a>(h: &Health, cves: impl IntoIterator<Item = &'a String>) -> Worst {
    let mut w = Worst::default();
    for c in cves.into_iter().filter_map(|id| h.cves.get(id)) {
        if c.band > w.band {
            w.band = c.band;
            w.cvss = c.cvss;
        }
        if c.epss > w.epss {
            w.epss = c.epss;
            w.percentile = c.percentile;
        }
        let rank = |e: &Option<String>| match e.as_deref() {
            Some("active") => 2,
            Some("poc") => 1,
            _ => 0,
        };
        if rank(&c.exploitation) > rank(&w.exploitation) {
            w.exploitation = c.exploitation.clone();
        }
    }
    w
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("maintainer-mode (+https://github.com/numtide/maintainermode)")
        .timeout(Duration::from_secs(30))
        .build()
        .expect("http client")
}

/// The CVEs of the tracker advisories and of the OSV matches.
fn tracked(h: &Health) -> BTreeSet<String> {
    h.advisories
        .values()
        .flatten()
        .flat_map(|a| a.cves.iter())
        .chain(h.osv_vulns.values().flat_map(|v| v.cves.iter()))
        .filter(|c| c.starts_with("CVE-"))
        .cloned()
        .collect()
}

/// `CVE-2026-19499` lives at `2026/19xxx/CVE-2026-19499.json`.
fn record_path(id: &str) -> Option<String> {
    let mut parts = id.split('-');
    if parts.next()? != "CVE" {
        return None;
    }
    let year = parts.next()?;
    let number = parts.next()?;
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    if !digits(year) || !digits(number) || parts.next().is_some() {
        return None;
    }
    let bucket = match number.len() {
        0..=3 => "0".to_string(),
        n => number[..n - 3].to_string(),
    };
    Some(format!("{year}/{bucket}xxx/{id}.json"))
}

/// The first CVSS of the CNA, then of each ADP, newest version first.
fn record_score(v: &Value) -> (Option<Band>, Option<f64>, Option<String>) {
    let containers: Vec<&Value> = std::iter::once(&v["containers"]["cna"])
        .chain(v["containers"]["adp"].as_array().into_iter().flatten())
        .collect();
    let mut score = (None, None);
    'found: for c in &containers {
        for m in c["metrics"].as_array().into_iter().flatten() {
            for k in ["cvssV4_0", "cvssV3_1", "cvssV3_0"] {
                let cvss = &m[k];
                let base = cvss["baseScore"].as_f64();
                let band = cvss["baseSeverity"]
                    .as_str()
                    .and_then(Band::parse)
                    .or_else(|| base.and_then(Band::of_score));
                if band.is_some() {
                    score = (band, base);
                    break 'found;
                }
            }
        }
    }
    let exploitation = containers
        .iter()
        .flat_map(|c| c["metrics"].as_array().into_iter().flatten())
        .filter(|m| m["other"]["type"] == "ssvc")
        .flat_map(|m| {
            m["other"]["content"]["options"]
                .as_array()
                .into_iter()
                .flatten()
        })
        .find_map(|o| o["Exploitation"].as_str())
        .map(str::to_ascii_lowercase);
    (score.0, score.1, exploitation)
}

fn nvd_score(v: &Value) -> (Option<Band>, Option<f64>) {
    let m = &v["vulnerabilities"][0]["cve"]["metrics"];
    for k in [
        "cvssMetricV40",
        "cvssMetricV31",
        "cvssMetricV30",
        "cvssMetricV2",
    ] {
        let first = &m[k][0];
        let base = first["cvssData"]["baseScore"].as_f64();
        let band = first["cvssData"]["baseSeverity"]
            .as_str()
            .or(first["baseSeverity"].as_str())
            .and_then(Band::parse)
            .or_else(|| base.and_then(Band::of_score));
        if band.is_some() {
            return (band, base);
        }
    }
    (None, None)
}

fn days(t: Option<DateTime<Utc>>, n: i64) -> bool {
    t.is_none_or(|t| Utc::now() - t > chrono::Duration::days(n))
}

/// New CVEs first, then the ones without a score, which a later CISA
/// enrichment may give one, then the rest after 90 days.
fn due(h: &Health) -> Vec<String> {
    let mut never = Vec::new();
    let mut unscored = Vec::new();
    let mut old = Vec::new();
    for id in tracked(h) {
        match h.cves.get(&id) {
            None => never.push(id),
            Some(c) if c.record_at.is_none() => never.push(id),
            Some(c) if c.band.is_none() && days(c.record_at, 7) => unscored.push(id),
            Some(c) if days(c.record_at, 90) => old.push(id),
            _ => {}
        }
    }
    never.into_iter().chain(unscored).chain(old).collect()
}

pub async fn records(h: &mut Health) -> Run {
    let http = http();
    let due = due(h);
    let (mut read, mut missing) = (0, 0);
    for id in due.iter().take(RECORD_BUDGET) {
        let Some(path) = record_path(id) else {
            continue;
        };
        let resp = match http.get(format!("{RECORDS}/{path}")).send().await {
            Ok(r) => r,
            Err(e) => return Run::failed(format!("CVE records: {e}")),
        };
        // a CVE that is reserved but not published has no record yet
        let record = if resp.status() == reqwest::StatusCode::NOT_FOUND {
            None
        } else {
            match resp.error_for_status() {
                Ok(r) => match r.json::<Value>().await {
                    Ok(v) => Some(v),
                    Err(e) => return Run::failed(format!("CVE record {id}: {e}")),
                },
                Err(e) => return Run::failed(format!("CVE records: {e}")),
            }
        };
        let c = h.cves.entry(id.clone()).or_default();
        c.record_at = Some(Utc::now());
        match record {
            None => missing += 1,
            Some(v) => {
                let (band, cvss, exploitation) = record_score(&v);
                if c.from.as_deref() != Some("nvd") || band.is_some() {
                    c.band = band;
                    c.cvss = cvss;
                    c.from = band.map(|_| "cve".to_string());
                }
                c.exploitation = exploitation;
                read += 1;
            }
        }
        tokio::time::sleep(RECORD_PAUSE).await;
    }
    let scored = h.cves.values().filter(|c| c.band.is_some()).count();
    Run::ok(format!(
        "{read} records read, {missing} not published · {scored} of {} CVEs have a score · {} left",
        tracked(h).len(),
        due.len().saturating_sub(RECORD_BUDGET)
    ))
}

/// Only for a CVE whose record has no score, and only with a key: without
/// one NVD allows 5 requests in 30 seconds.
pub async fn nvd(h: &mut Health, key: Option<&str>) -> Run {
    let Some(key) = key else {
        return Run::ok("off: no API key".into());
    };
    let http = http();
    let due: Vec<String> = tracked(h)
        .into_iter()
        .filter(|id| {
            h.cves
                .get(id)
                .is_some_and(|c| c.record_at.is_some() && c.band.is_none() && days(c.nvd_at, 30))
        })
        .collect();
    let mut scored = 0;
    for id in due.iter().take(NVD_BUDGET) {
        let resp = http
            .get(format!("{NVD}?cveId={id}"))
            .header("apiKey", key)
            .send()
            .await
            .and_then(|r| r.error_for_status());
        let v: Value = match resp {
            Ok(r) => match r.json().await {
                Ok(v) => v,
                Err(e) => return Run::failed(format!("NVD: {e}")),
            },
            Err(e) => return Run::failed(format!("NVD: {e}")),
        };
        let c = h.cves.entry(id.clone()).or_default();
        c.nvd_at = Some(Utc::now());
        let (band, cvss) = nvd_score(&v);
        if band.is_some() {
            c.band = band;
            c.cvss = cvss;
            c.from = Some("nvd".into());
            scored += 1;
        }
        tokio::time::sleep(NVD_PAUSE).await;
    }
    Run::ok(format!(
        "{scored} of {} asked got a score · {} left",
        due.len().min(NVD_BUDGET),
        due.len().saturating_sub(NVD_BUDGET)
    ))
}

async fn epss_chunk(http: &reqwest::Client, ids: &[String]) -> Result<Vec<(String, f64, f64)>> {
    let v: Value = http
        .get(format!("{EPSS}?cve={}&limit={EPSS_CHUNK}", ids.join(",")))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let num = |x: &Value| {
        x.as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .or(x.as_f64())
    };
    Ok(v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            Some((
                d["cve"].as_str()?.to_string(),
                num(&d["epss"])?,
                num(&d["percentile"])?,
            ))
        })
        .collect())
}

/// Every tracked CVE, 100 to a request, once a day: the scores move daily.
pub async fn epss(h: &mut Health) -> Run {
    let http = http();
    let ids: Vec<String> = tracked(h).into_iter().collect();
    let mut scored = 0;
    for chunk in ids.chunks(EPSS_CHUNK) {
        let got = match epss_chunk(&http, chunk).await {
            Ok(g) => g,
            Err(e) => return Run::failed(format!("EPSS: {e:#}")),
        };
        for (id, epss, percentile) in got {
            let c = h.cves.entry(id).or_default();
            c.epss = Some(epss);
            c.percentile = Some(percentile);
            scored += 1;
        }
        tokio::time::sleep(EPSS_PAUSE).await;
    }
    Run::ok(format!("{scored} of {} CVEs scored", ids.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_cve_has_a_place_in_the_record_tree() {
        assert_eq!(
            record_path("CVE-2026-19499").as_deref(),
            Some("2026/19xxx/CVE-2026-19499.json")
        );
        assert_eq!(
            record_path("CVE-2021-44").as_deref(),
            Some("2021/0xxx/CVE-2021-44.json")
        );
        assert_eq!(record_path("GHSA-xxxx"), None);
        assert_eq!(record_path("CVE-2026-1/../x"), None);
    }

    #[test]
    fn the_cna_score_comes_before_the_adp_one() {
        let v = json!({"containers": {
            "cna": {"metrics": [{"cvssV3_1": {"baseScore": 9.8, "baseSeverity": "CRITICAL"}}]},
            "adp": [{"metrics": [
                {"cvssV3_1": {"baseScore": 5.0}},
                {"other": {"type": "ssvc", "content": {"options": [{"Exploitation": "Active"}]}}}
            ]}]
        }});
        assert_eq!(
            record_score(&v),
            (Some(Band::Critical), Some(9.8), Some("active".into()))
        );
        let adp_only = json!({"containers": {"cna": {}, "adp": [{"metrics": [{"cvssV3_1": {"baseScore": 5.0}}]}]}});
        assert_eq!(
            record_score(&adp_only),
            (Some(Band::Medium), Some(5.0), None)
        );
    }

    #[test]
    fn nvd_names_the_band_in_either_place() {
        let v = json!({"vulnerabilities": [{"cve": {"metrics": {
            "cvssMetricV2": [{"cvssData": {"baseScore": 7.5}, "baseSeverity": "HIGH"}]
        }}}]});
        assert_eq!(nvd_score(&v), (Some(Band::High), Some(7.5)));
    }

    #[test]
    fn moderate_is_medium() {
        assert_eq!(Band::parse("MODERATE"), Some(Band::Medium));
        assert!(Band::Critical > Band::Low);
    }
}
