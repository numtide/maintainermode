use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use std::time::Duration as StdDuration;

use crate::model::{Pr, Review, Void};
use crate::store::Store;

const OWNER: &str = "NixOS";
const REPO: &str = "nixpkgs";
const PAGE: i64 = 25;

const PR_FIELDS: &str = r#"
  number title url isDraft state createdAt updatedAt mergedAt mergeable headRefOid baseRefName
  author { __typename login }
  commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
  files(first: 50) { nodes { path } }
  reviews(first: 30) { nodes {
    url state submittedAt author { __typename login } commit { oid }
    comments(first: 5) { nodes { diffHunk } }
  } }
  comments(last: 30) { nodes { url createdAt body authorAssociation author { login } } }
  timelineItems(last: 30, itemTypes: [PULL_REQUEST_COMMIT, HEAD_REF_FORCE_PUSHED_EVENT]) { nodes {
    __typename
    ... on PullRequestCommit { commit { committedDate } }
    ... on HeadRefForcePushedEvent { createdAt }
  } }
"#;

pub struct Client {
    http: reqwest::Client,
    tokens: Vec<String>,
    current: usize,
}

pub enum Outcome {
    Data(Value),
    /// The token was revoked or expired; the session holding it is dropped.
    BadToken(String),
}

impl Client {
    pub fn new(tokens: Vec<String>) -> Self {
        Self {
            http: http(),
            tokens,
            current: 0,
        }
    }

    pub fn has_token(&self) -> bool {
        self.current < self.tokens.len()
    }

    /// Runs one query, moving to the next lent token when the current one is
    /// rejected or nearly out of budget.
    pub async fn graphql(&mut self, store: &Store, query: &str, vars: Value) -> Result<Value> {
        loop {
            let Some(token) = self.tokens.get(self.current).cloned() else {
                bail!(
                    "no GitHub token with budget left; sync resumes when someone logs in or the limit resets"
                );
            };
            match graphql_once(&self.http, &token, query, &vars).await? {
                Outcome::BadToken(t) => {
                    store.forget_token(&t)?;
                    self.current += 1;
                }
                Outcome::Data(v) => {
                    let remaining = v["rateLimit"]["remaining"].as_i64().unwrap_or(i64::MAX);
                    if remaining < 200 {
                        self.current += 1;
                    }
                    return Ok(v);
                }
            }
        }
    }
}

pub fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("review-club")
        .timeout(StdDuration::from_secs(120))
        .build()
        .expect("http client")
}

pub async fn graphql_once(
    http: &reqwest::Client,
    token: &str,
    query: &str,
    vars: &Value,
) -> Result<Outcome> {
    let resp = http
        .post("https://api.github.com/graphql")
        .bearer_auth(token)
        .json(&json!({ "query": query, "variables": vars }))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(Outcome::BadToken(token.to_string()));
    }
    let status = resp.status();
    let body: Value = resp.json().await.context("graphql response")?;
    if let Some(errors) = body.get("errors") {
        bail!("graphql {status}: {errors}");
    }
    if !status.is_success() {
        bail!("graphql {status}: {body}");
    }
    Ok(Outcome::Data(body["data"].clone()))
}

fn time(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn login(v: &Value) -> (String, bool) {
    let login = v["login"].as_str().unwrap_or("ghost").to_string();
    let bot = v["__typename"] == "Bot" || login.ends_with("[bot]");
    (login, bot)
}

fn nodes(v: &Value) -> impl Iterator<Item = &Value> {
    v["nodes"].as_array().into_iter().flatten()
}

/// A committer voids a review with a line `review-club: void <review link>`
/// in a comment on the same pull request.
fn parse_void(body: &str) -> Option<String> {
    body.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("review-club: void")?.trim();
        let id = rest.rsplit("pullrequestreview-").next()?;
        let id: String = id.chars().take_while(|c| c.is_ascii_digit()).collect();
        (!id.is_empty()).then_some(id)
    })
}

/// `diffHunk` ends with the line the comment sits on.
fn on_change(hunk: &str) -> bool {
    hunk.lines()
        .last()
        .is_some_and(|l| l.starts_with('+') || l.starts_with('-'))
}

pub fn normalize(n: &Value) -> Option<Pr> {
    let (author, author_is_bot) = login(&n["author"]);
    let mut reviews = Vec::new();
    let mut author_activity = Vec::new();
    for r in nodes(&n["reviews"]) {
        let Some(at) = time(&r["submittedAt"]) else {
            continue;
        };
        let (who, bot) = login(&r["author"]);
        if who == author {
            author_activity.push(at);
            continue;
        }
        reviews.push(Review {
            url: r["url"].as_str()?.to_string(),
            author: who,
            author_is_bot: bot,
            state: r["state"].as_str().unwrap_or("").to_string(),
            submitted_at: at,
            commit_oid: r["commit"]["oid"].as_str().map(String::from),
            inline_on_change: nodes(&r["comments"])
                .any(|c| c["diffHunk"].as_str().is_some_and(on_change)),
        });
    }
    let mut voids = Vec::new();
    for c in nodes(&n["comments"]) {
        let Some(at) = time(&c["createdAt"]) else {
            continue;
        };
        let (who, _) = login(&c["author"]);
        if who == author {
            author_activity.push(at);
        }
        let committer = matches!(
            c["authorAssociation"].as_str(),
            Some("OWNER" | "MEMBER" | "COLLABORATOR")
        );
        if let (true, Some(id)) = (committer, c["body"].as_str().and_then(parse_void)) {
            voids.push(Void {
                review_id: id,
                by: who,
                url: c["url"].as_str().unwrap_or("").to_string(),
                at,
            });
        }
    }
    let pushes = nodes(&n["timelineItems"])
        .filter_map(|t| time(&t["commit"]["committedDate"]).or_else(|| time(&t["createdAt"])))
        .collect();
    Some(Pr {
        number: n["number"].as_u64()?,
        title: n["title"].as_str()?.to_string(),
        url: n["url"].as_str()?.to_string(),
        author,
        author_is_bot,
        state: n["state"].as_str()?.to_string(),
        is_draft: n["isDraft"].as_bool().unwrap_or(false),
        base: n["baseRefName"].as_str().unwrap_or("").to_string(),
        created_at: time(&n["createdAt"])?,
        updated_at: time(&n["updatedAt"])?,
        merged_at: time(&n["mergedAt"]),
        mergeable: n["mergeable"].as_str().unwrap_or("UNKNOWN").to_string(),
        head_oid: n["headRefOid"].as_str().unwrap_or("").to_string(),
        files: nodes(&n["files"])
            .filter_map(|f| f["path"].as_str().map(String::from))
            .collect(),
        ci: n["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"]
            .as_str()
            .map(String::from),
        reviews,
        author_activity,
        pushes,
        voids,
    })
}

async fn page(
    client: &mut Client,
    store: &Store,
    open_only: bool,
    after: Option<&str>,
) -> Result<(Vec<Pr>, Option<String>)> {
    let query = format!(
        "query($owner: String!, $name: String!, $n: Int!, $after: String, $states: [PullRequestState!], $field: IssueOrderField!) {{
           rateLimit {{ cost remaining resetAt }}
           repository(owner: $owner, name: $name) {{
             pullRequests(first: $n, after: $after, states: $states, orderBy: {{ field: $field, direction: DESC }}) {{
               pageInfo {{ hasNextPage endCursor }}
               nodes {{ {PR_FIELDS} }}
             }}
           }}
         }}"
    );
    let vars = json!({
        "owner": OWNER, "name": REPO, "n": PAGE, "after": after,
        "states": if open_only { json!(["OPEN"]) } else { Value::Null },
        "field": if open_only { "CREATED_AT" } else { "UPDATED_AT" },
    });
    let data = client.graphql(store, &query, vars).await?;
    let conn = &data["repository"]["pullRequests"];
    let prs = nodes(conn).filter_map(normalize).collect();
    let next = conn["pageInfo"]["hasNextPage"]
        .as_bool()
        .unwrap_or(false)
        .then(|| conn["pageInfo"]["endCursor"].as_str().map(String::from))
        .flatten();
    Ok((prs, next))
}

/// Brings the store up to date. Two walks, both resumable from a stored cursor:
/// every open pull request once, then everything updated since the last pass,
/// newest first, stopping at the watermark.
pub async fn sync(store: &Store, client: &mut Client, window_days: i64) -> Result<usize> {
    let mut count = 0;
    if store.get("open_done")?.is_none() {
        let mut cursor = store.get("open_cursor")?;
        loop {
            let (prs, next) = page(client, store, true, cursor.as_deref()).await?;
            count += prs.len();
            store.put_prs(&prs)?;
            match next {
                Some(c) => {
                    store.set("open_cursor", &c)?;
                    cursor = Some(c);
                }
                None => {
                    store.set("open_done", "1")?;
                    break;
                }
            }
        }
    }

    let started = match store.get_time("recent_started")? {
        Some(t) if store.get("recent_cursor")?.is_some() => t,
        _ => {
            let now = Utc::now();
            store.set("recent_started", &now.to_rfc3339())?;
            now
        }
    };
    let floor = store
        .get_time("watermark")?
        .unwrap_or_else(|| Utc::now() - Duration::days(window_days));
    let mut cursor = store.get("recent_cursor")?;
    loop {
        let (prs, next) = page(client, store, false, cursor.as_deref()).await?;
        let reached = prs.iter().any(|p| p.updated_at < floor);
        count += prs.len();
        store.put_prs(&prs)?;
        match next {
            Some(c) if !reached => {
                store.set("recent_cursor", &c)?;
                cursor = Some(c);
            }
            _ => break,
        }
    }
    // updates that land during the walk move to the top behind us; starting
    // the next pass at our own start time picks them up
    store.set("watermark", &(started - Duration::minutes(2)).to_rfc3339())?;
    store.delete_key("recent_cursor")?;
    store.set("last_sync", &Utc::now().to_rfc3339())?;
    Ok(count)
}

/// Directories a person changed in merged pull requests over the last year,
/// read with that person's own token.
pub async fn committed_paths(token: &str, login: &str) -> Result<Vec<String>> {
    let since = (Utc::now() - Duration::days(365)).format("%Y-%m-%d");
    let query = "query($q: String!) { search(query: $q, type: ISSUE, first: 100) { nodes {
        ... on PullRequest { files(first: 30) { nodes { path } } } } } }";
    let vars = json!({ "q": format!("repo:{OWNER}/{REPO} is:pr is:merged author:{login} merged:>={since}") });
    let Outcome::Data(data) = graphql_once(&http(), token, query, &vars).await? else {
        bail!("token rejected");
    };
    let mut out: Vec<String> = nodes(&data["search"])
        .flat_map(|pr| nodes(&pr["files"]))
        .filter_map(|f| f["path"].as_str())
        .map(|p| p.rsplit_once('/').map_or("", |(d, _)| d).to_string())
        .filter(|d| !d.is_empty())
        .collect();
    out.sort();
    out.dedup();
    Ok(out)
}

pub struct OAuth {
    pub client_id: String,
    pub client_secret: String,
    pub public_url: String,
}

impl OAuth {
    pub fn callback(&self) -> String {
        format!("{}/auth/callback", self.public_url.trim_end_matches('/'))
    }

    /// No scope: the token reads public data only, which is all the club needs.
    pub fn authorize_url(&self, state: &str) -> String {
        format!(
            "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&state={}&allow_signup=false",
            self.client_id,
            urlencode(&self.callback()),
            state
        )
    }

    pub async fn exchange(&self, code: &str) -> Result<(String, String)> {
        let http = http();
        let resp: Value = http
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("code", code),
                ("redirect_uri", &self.callback()),
            ])
            .send()
            .await?
            .json()
            .await?;
        let Some(token) = resp["access_token"].as_str() else {
            bail!("oauth exchange failed: {}", resp["error"]);
        };
        let Outcome::Data(me) =
            graphql_once(&http, token, "{ viewer { login } }", &Value::Null).await?
        else {
            bail!("fresh token rejected");
        };
        let login = me["viewer"]["login"]
            .as_str()
            .context("viewer login")?
            .to_string();
        Ok((login, token.to_string()))
    }
}

pub fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn void_line_names_a_review() {
        assert_eq!(
            parse_void("thanks\nreview-club: void https://github.com/NixOS/nixpkgs/pull/1#pullrequestreview-123\n").as_deref(),
            Some("123")
        );
        assert_eq!(parse_void("review-club: void 456").as_deref(), Some("456"));
        assert_eq!(parse_void("review-club: void please"), None);
    }

    #[test]
    fn hunk_last_line_decides() {
        assert!(on_change("@@ -1,2 +1,2 @@\n foo\n+bar"));
        assert!(!on_change("@@ -1,2 +1,2 @@\n+bar\n foo"));
    }
}
