//! The token of a GitHub App installation, which syncs first. The App signs a short JWT with
//! its private key and trades it for an installation token, which expires
//! after one hour.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::secret::Secret;

const API: &str = "https://api.github.com";
/// A sync pass holds its token for the whole pass, so a token is renewed
/// while a pass can still finish inside its lifetime.
const RENEW_BEFORE: i64 = 30;

pub struct GithubApp {
    id: String,
    key: Secret,
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

fn encoding_key(key: &Secret) -> Result<EncodingKey> {
    EncodingKey::from_rsa_pem(key.get().as_bytes()).context("GitHub App private key")
}

fn fresh(expires_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    expires_at - now > Duration::minutes(RENEW_BEFORE)
}

/// People install the same App on their own accounts to start workflows; a
/// token of their installation must not sync.
fn owned(installations: &[Value], owner: &str) -> Option<u64> {
    installations
        .iter()
        .find(|i| {
            i["account"]["login"]
                .as_str()
                .is_some_and(|l| l.eq_ignore_ascii_case(owner))
        })
        .and_then(|i| i["id"].as_u64())
}

impl GithubApp {
    pub fn new(id: String, key: Secret, installation: Option<u64>) -> Result<Self> {
        encoding_key(&key)?;
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
            &encoding_key(&self.key)?,
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
        let app = self.get(jwt, "/app").await?;
        let owner = app["owner"]["login"]
            .as_str()
            .context("GitHub App owner")?
            .to_string();
        for page in 1.. {
            let list = self
                .get(jwt, &format!("/app/installations?per_page=100&page={page}"))
                .await?;
            let list = list.as_array().cloned().unwrap_or_default();
            if let Some(id) = owned(&list, &owner) {
                return Ok(id);
            }
            if list.len() < 100 {
                break;
            }
        }
        bail!(
            "GitHub App {} has no installation on its owner {owner}; install it there or set GITHUB_APP_INSTALLATION_ID",
            self.id
        )
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

    #[test]
    fn installation_of_the_owner_only() {
        let list = serde_json::json!([
            {"id": 1, "account": {"login": "alice"}},
            {"id": 2, "account": {"login": "Acme"}},
        ]);
        let list = list.as_array().unwrap();
        assert_eq!(owned(list, "acme"), Some(2));
        assert_eq!(owned(&list[..1], "acme"), None);
    }
}
