//! Reads one package from a nixfresh instance's public API. nixfresh stays a
//! read-only dataset that holds no account and no credential; every write
//! path for a person (logins, watches, later notifications) lives here.

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, Serialize)]
pub struct Health {
    pub tracked: bool,
    pub version: Option<String>,
    pub upstream: Option<String>,
    pub open_advisories: Vec<String>,
    /// Open nixpkgs pull requests and issues nixfresh matched to the package.
    pub items: Vec<(String, String)>,
}

pub struct Client {
    base: String,
    http: reqwest::Client,
    cache: Mutex<HashMap<String, (Instant, Health)>>,
}

const TTL: Duration = Duration::from_secs(600);

impl Client {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .user_agent("maintainer-mode")
                .timeout(Duration::from_secs(5))
                .build()
                .expect("http client"),
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub async fn health(&self, pname: &str) -> Result<Health> {
        if let Some((at, h)) = self.cache.lock().unwrap().get(pname) {
            if at.elapsed() < TTL {
                return Ok(h.clone());
            }
        }
        let url = format!(
            "{}/api/v1/packages/{}",
            self.base,
            crate::github::urlencode(pname)
        );
        let v: Value = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let h = parse(&v);
        self.cache
            .lock()
            .unwrap()
            .insert(pname.to_string(), (Instant::now(), h.clone()));
        Ok(h)
    }
}

/// The endpoint returns every build of the package across sources; the
/// first one carries the fields shown here.
fn parse(v: &Value) -> Health {
    let Some(p) = v.as_array().and_then(|a| a.first()) else {
        return Health::default();
    };
    let s = |v: &Value| v.as_str().map(String::from);
    Health {
        tracked: true,
        version: s(&p["version"]),
        upstream: s(&p["upstream"]["latest"]),
        open_advisories: p["advisories"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|a| a["state"] == "affected" && a["triage"].is_null())
            .filter_map(|a| s(&a["advisory"]["id"]))
            .collect(),
        items: p["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| {
                i["state"]
                    .as_str()
                    .is_none_or(|s| s.eq_ignore_ascii_case("open"))
            })
            .filter_map(|i| Some((s(&i["url"])?, s(&i["title"])?)))
            .collect(),
    }
}
