//! The sync token of a GitHub App installation. The App signs a short JWT with
//! its private key and trades it for an installation token, which expires
//! after one hour.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;

const API: &str = "https://api.github.com";
/// A sync pass holds its token for the whole pass, so a token is renewed
/// while a pass can still finish inside its lifetime.
const RENEW_BEFORE: i64 = 30;

pub struct GithubApp {
    id: String,
    key: EncodingKey,
    installation: Option<u64>,
    http: reqwest::Client,
    cached: Mutex<Option<(String, DateTime<Utc>)>>,
}

#[derive(Serialize)]
struct Claims {
    iat: i64,
    exp: i64,
    iss: String,
}

/// GitHub rejects an `iat` in the future and an `exp` more than ten minutes
/// ahead; the margins absorb clock drift.
fn claims(id: &str, now: DateTime<Utc>) -> Claims {
    Claims {
        iat: (now - Duration::seconds(60)).timestamp(),
        exp: (now + Duration::minutes(9)).timestamp(),
        iss: id.to_string(),
    }
}

fn fresh(expires_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    expires_at - now > Duration::minutes(RENEW_BEFORE)
}

impl GithubApp {
    pub fn new(id: String, pem: &str, installation: Option<u64>) -> Result<Self> {
        let key = EncodingKey::from_rsa_pem(pem.as_bytes()).context("GitHub App private key")?;
        Ok(Self {
            id,
            key,
            installation,
            http: crate::github::http(),
            cached: Mutex::new(None),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    fn jwt(&self) -> Result<String> {
        let header = Header::new(Algorithm::RS256);
        Ok(jsonwebtoken::encode(
            &header,
            &claims(&self.id, Utc::now()),
            &self.key,
        )?)
    }

    async fn get(&self, jwt: &str, path: &str) -> Result<Value> {
        let resp = self
            .http
            .get(format!("{API}{path}"))
            .bearer_auth(jwt)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?;
        let status = resp.status();
        let body: Value = resp.json().await.context("GitHub App response")?;
        if !status.is_success() {
            bail!("GET {path}: {status}: {body}");
        }
        Ok(body)
    }

    async fn installation(&self, jwt: &str) -> Result<u64> {
        if let Some(id) = self.installation {
            return Ok(id);
        }
        let list = self.get(jwt, "/app/installations").await?;
        let ids: Vec<u64> = list
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| i["id"].as_u64())
            .collect();
        match ids.as_slice() {
            [id] => Ok(*id),
            _ => bail!(
                "GitHub App {} has {} installations; set GITHUB_APP_INSTALLATION_ID",
                self.id,
                ids.len()
            ),
        }
    }

    /// The cached installation token, renewed when it nears its expiry.
    pub async fn token(&self) -> Result<String> {
        let mut cached = self.cached.lock().await;
        if let Some((token, expires_at)) = cached.as_ref() {
            if fresh(*expires_at, Utc::now()) {
                return Ok(token.clone());
            }
        }
        let jwt = self.jwt()?;
        let installation = self.installation(&jwt).await?;
        let path = format!("/app/installations/{installation}/access_tokens");
        let resp = self
            .http
            .post(format!("{API}{path}"))
            .bearer_auth(&jwt)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?;
        let status = resp.status();
        let body: Value = resp.json().await.context("installation token response")?;
        if !status.is_success() {
            bail!("POST {path}: {status}: {body}");
        }
        let token = body["token"]
            .as_str()
            .context("installation token")?
            .to_string();
        let expires_at = body["expires_at"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_else(|| Utc::now() + Duration::hours(1));
        *cached = Some((token.clone(), expires_at));
        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jwt_window_fits_github_limits() {
        let now = Utc::now();
        let c = claims("42", now);
        assert!(c.iat < now.timestamp());
        assert!(c.exp - now.timestamp() <= 600);
        assert_eq!(c.iss, "42");
    }

    #[test]
    fn token_renewed_before_expiry() {
        let now = Utc::now();
        assert!(fresh(now + Duration::minutes(59), now));
        assert!(!fresh(now + Duration::minutes(20), now));
        assert!(!fresh(now - Duration::minutes(1), now));
    }
}
