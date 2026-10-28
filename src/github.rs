use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use std::time::Duration as StdDuration;

use crate::metrics::{Facts, Seen, areas};
use crate::model::{Issue, Pr, Review, Void};
use crate::store::Store;

const OWNER: &str = "NixOS";
const REPO: &str = "nixpkgs";
const GRAPHQL: &str = "https://api.github.com/graphql";

pub struct Client {
    http: reqwest::Client,
    tokens: Vec<String>,
    current: usize,
    endpoint: String,
    /// The tokens are lent, and the work is shared: each token pays at most
    /// `LENT_SHARE` of its budget in one rate limit window.
    lent: bool,
}

/// The part of a lent token's budget that shared work may use in one rate
/// limit window, as a divisor: a tenth.
const LENT_SHARE: i64 = 10;

/// Per lent token hash: the reset time of the window it was first seen in
/// for shared work, and the budget below which shared work stops using it.
static LENT_FLOORS: std::sync::Mutex<std::collections::BTreeMap<String, (String, i64)>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// True when shared work has used its share of `token` in this window. An
/// answer with no rate limit counts as used up.
fn lent_share_used(token: &str, rate: &Value) -> bool {
    let (Some(remaining), Some(reset)) = (rate["remaining"].as_i64(), rate["resetAt"].as_str())
    else {
        return true;
    };
    let mut floors = LENT_FLOORS.lock().unwrap();
    let e = floors
        .entry(crate::sets::token_hash(token))
        .or_insert_with(|| (String::new(), 0));
    if e.0 != reset {
        *e = (reset.to_string(), remaining - remaining / LENT_SHARE);
    }
    remaining < e.1
}

pub enum Outcome {
    Data(Value),
    /// The token was revoked or expired; the session holding it is dropped.
    BadToken(String),
}

/// The error of a call whose token has no budget left: the client loop
/// moves to the next token on it.
#[derive(Debug)]
struct Exhausted;

impl std::fmt::Display for Exhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the GitHub token has no budget left")
    }
}

impl std::error::Error for Exhausted {}

impl Client {
    pub fn new(tokens: Vec<String>) -> Self {
        Self {
            http: http(),
            tokens,
            current: 0,
            endpoint: GRAPHQL.into(),
            lent: false,
        }
    }

    /// A client for shared work on tokens that people lent: see
    /// `LENT_SHARE`. Each query must ask for `rateLimit { remaining resetAt }`.
    pub fn lent(tokens: Vec<String>) -> Self {
        Self {
            lent: true,
            ..Self::new(tokens)
        }
    }

    pub fn is_lent(&self) -> bool {
        self.lent
    }

    #[cfg(test)]
    pub fn at(endpoint: &str, tokens: Vec<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            ..Self::new(tokens)
        }
    }

    pub fn has_token(&self) -> bool {
        self.current < self.tokens.len()
    }

    /// Runs one query, moving to the next lent token when the current one is
    /// rejected or nearly out of budget.
    pub async fn graphql(&mut self, store: &Store, query: &str, vars: Value) -> Result<Value> {
        self.graphql_allowing(store, query, vars, &[]).await
    }

    /// As `graphql`, but errors of the `allowed` types leave their field
    /// null instead of failing the query.
    pub async fn graphql_allowing(
        &mut self,
        store: &Store,
        query: &str,
        vars: Value,
        allowed: &[&str],
    ) -> Result<Value> {
        loop {
            let Some(token) = self.tokens.get(self.current).cloned() else {
                bail!(
                    "no GitHub token with budget left; sync resumes when someone logs in or the limit resets"
                );
            };
            match graphql_call(&self.http, &self.endpoint, &token, query, &vars, allowed).await {
                Err(e) if e.is::<Exhausted>() => self.current += 1,
                Err(e) => return Err(e),
                Ok(Outcome::BadToken(t)) => {
                    store.forget_token(&t)?;
                    self.current += 1;
                }
                Ok(Outcome::Data(v)) => {
                    let remaining = v["rateLimit"]["remaining"].as_i64().unwrap_or(i64::MAX);
                    if remaining < 200 || (self.lent && lent_share_used(&token, &v["rateLimit"])) {
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
        .user_agent("maintainer-mode")
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
    graphql_call(http, GRAPHQL, token, query, vars, &[]).await
}

/// A primary limit answers 200 with a `RATE_LIMITED` error; a secondary
/// limit answers 403 or 429.
fn exhausted(status: reqwest::StatusCode, remaining: Option<&str>, body: &Value) -> bool {
    let limited = matches!(status.as_u16(), 403 | 429) && remaining.is_none_or(|r| r == "0");
    limited
        || body["errors"]
            .as_array()
            .is_some_and(|e| e.iter().any(|e| e["type"] == "RATE_LIMITED"))
}

async fn graphql_call(
    http: &reqwest::Client,
    endpoint: &str,
    token: &str,
    query: &str,
    vars: &Value,
    allowed: &[&str],
) -> Result<Outcome> {
    let resp = http
        .post(endpoint)
        .bearer_auth(token)
        .json(&json!({ "query": query, "variables": vars }))
        .send()
        .await
        .inspect_err(|_| crate::telemetry::graphql_answer(token, None))?;
    crate::telemetry::graphql_answer(token, Some(&resp));
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(Outcome::BadToken(token.to_string()));
    }
    let status = resp.status();
    let remaining = resp
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body: Value = match resp.json().await {
        Ok(b) => b,
        Err(_) if matches!(status.as_u16(), 403 | 429) => Value::Null,
        Err(e) => return Err(e).context("graphql response"),
    };
    if exhausted(status, remaining.as_deref(), &body) {
        return Err(Exhausted.into());
    }
    if let Some(errors) = body.get("errors") {
        let all_allowed = errors.as_array().is_some_and(|e| {
            e.iter()
                .all(|e| e["type"].as_str().is_some_and(|t| allowed.contains(&t)))
        });
        if !all_allowed || body["data"].is_null() {
            bail!("graphql {status}: {errors}");
        }
    }
    if !status.is_success() {
        bail!("graphql {status}: {body}");
    }
    Ok(Outcome::Data(body["data"].clone()))
}

pub(crate) fn time(v: &Value) -> Option<DateTime<Utc>> {
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

/// The cursor of the next page of a connection, if it has one.
fn next_cursor(conn: &Value) -> Option<String> {
    conn["pageInfo"]["hasNextPage"]
        .as_bool()
        .unwrap_or(false)
        .then(|| conn["pageInfo"]["endCursor"].as_str().map(String::from))
        .flatten()
}

/// A committer voids a review with a line `maintainer-mode: void <review link>`
/// in a comment on the same pull request.
fn parse_void(body: &str) -> Option<String> {
    body.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("maintainer-mode: void")?.trim();
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

/// A pull request in the shape that `project::pr` cuts from an archived
/// item.
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
    let mut runs = Vec::new();
    for c in nodes(&n["comments"]) {
        let Some(at) = time(&c["createdAt"]) else {
            continue;
        };
        let (who, _) = login(&c["author"]);
        if who == author {
            author_activity.push(at);
        }
        let url = c["url"].as_str().unwrap_or("");
        if let Some(run) = c["body"]
            .as_str()
            .and_then(|b| crate::built::parse(b, &who, url, at))
        {
            runs.push(run);
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
        merged_by: n["mergedBy"]["login"].as_str().map(String::from),
        merge_commit: n["mergeCommit"]["oid"].as_str().map(String::from),
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
        runs,
        closed_at: time(&n["closedAt"]),
        additions: n["additions"].as_u64().unwrap_or(0),
        deletions: n["deletions"].as_u64().unwrap_or(0),
        changed_files: n["changedFiles"].as_u64().unwrap_or(0),
        labels: labels(n),
        association: n["authorAssociation"].as_str().unwrap_or("").to_string(),
        ai: ai_marker(n),
        treewide: None,
    })
}

fn labels(n: &Value) -> Vec<String> {
    nodes(&n["labels"])
        .filter_map(|l| l["name"].as_str().map(String::from))
        .collect()
}

pub const OTHER_FORM: &str = "other-form";

const MODEL_HINTS: [&str; 10] = [
    "claude", "copilot", "openai", "gpt", "codex", "gemini", "cursor", "deepseek", "glm", "z.ai",
];

/// The nixpkgs automation/AI policy requires an `Assisted-by:` trailer, and a
/// bot labels such pull requests `llm-assisted`. Attribution in any other form,
/// such as `Co-authored-by:`, does not satisfy the policy and is reported as
/// OTHER_FORM. Only the last commit is read, and undisclosed use cannot be
/// seen, so every count built on this is a floor.
fn ai_marker(n: &Value) -> Option<String> {
    let message = n["commits"]["nodes"][0]["commit"]["message"]
        .as_str()
        .unwrap_or("");
    let body = n["body"].as_str().unwrap_or("");
    let mut other = false;
    for line in message.lines().chain(body.lines()) {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("Assisted-by:") {
            return Some(tool_name(rest));
        }
        let lower = l.to_lowercase();
        if lower.contains("generated with [claude code]")
            || (lower.starts_with("co-authored-by:")
                && MODEL_HINTS.iter().any(|h| lower.contains(h)))
        {
            other = true;
        }
    }
    if labels(n).iter().any(|l| l == "llm-assisted") {
        return Some("disclosed".into());
    }
    other.then(|| OTHER_FORM.into())
}
/// `Claude Code (Claude Opus 5.5)`, `codex-cli 0.162.1 (deepseek-flash)`,
/// `opencode (model glm-5.3-flash)`, and the Linux kernel form
/// `claude:claude-opus-5-5`: the tool is the text before the first version,
/// bracket or colon.
fn tool_name(trailer: &str) -> String {
    let head = trailer.split(['(', '<', ':', '[']).next().unwrap_or("");
    let name = head
        .split_whitespace()
        .take_while(|w| !w.starts_with(|c: char| c.is_ascii_digit()))
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase();
    if name.is_empty() {
        "disclosed".into()
    } else {
        name
    }
}

pub fn normalize_issue(n: &Value) -> Option<Issue> {
    Some(Issue {
        number: n["number"].as_u64()?,
        title: n["title"].as_str()?.to_string(),
        url: n["url"].as_str()?.to_string(),
        author: login(&n["author"]).0,
        state: n["state"].as_str()?.to_string(),
        created_at: time(&n["createdAt"])?,
        updated_at: time(&n["updatedAt"])?,
        closed_at: time(&n["closedAt"]),
        labels: labels(n),
        comments: n["comments"]["totalCount"].as_u64().unwrap_or(0),
        mentions: crate::subject::of_body(n["body"].as_str().unwrap_or("")),
    })
}

/// Pull requests and issues by number, with the fields of the walks, in one
/// query. A number that GitHub does not know is left out.
pub(crate) fn facts(n: &Value) -> Option<Facts> {
    let (author, author_is_bot) = login(&n["author"]);
    let files: Vec<String> = nodes(&n["files"])
        .filter_map(|f| f["path"].as_str().map(String::from))
        .collect();
    Some(Facts {
        number: n["number"].as_u64()?,
        reviews: nodes(&n["reviews"])
            .filter_map(|r| {
                let (by, bot) = login(&r["author"]);
                (!bot && by != author).then_some(Seen {
                    by,
                    at: time(&r["submittedAt"])?,
                    state: r["state"].as_str().unwrap_or("").to_string(),
                })
            })
            .collect(),
        author,
        author_is_bot,
        title: n["title"].as_str().unwrap_or("").to_string(),
        association: n["authorAssociation"].as_str().unwrap_or("").to_string(),
        state: n["state"].as_str()?.to_string(),
        is_draft: n["isDraft"].as_bool().unwrap_or(false),
        base: n["baseRefName"].as_str().unwrap_or("").to_string(),
        created_at: time(&n["createdAt"])?,
        updated_at: time(&n["updatedAt"])?,
        closed_at: time(&n["closedAt"]),
        merged_at: time(&n["mergedAt"]),
        merged_by: n["mergedBy"]["login"].as_str().map(String::from),
        additions: n["additions"].as_u64().unwrap_or(0),
        deletions: n["deletions"].as_u64().unwrap_or(0),
        changed_files: n["changedFiles"].as_u64().unwrap_or(0),
        labels: labels(n),
        areas: areas(&files),
        ai: ai_marker(n),
    })
}

fn live_of(pr: &Value, at: DateTime<Utc>) -> Option<crate::store::Live> {
    Some(crate::store::Live {
        number: pr["number"].as_u64()?,
        head_oid: pr["headRefOid"].as_str()?.to_string(),
        mergeable: pr["mergeable"].as_str().unwrap_or("UNKNOWN").to_string(),
        ci: pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"]
            .as_str()
            .map(String::from),
        association: pr["authorAssociation"].as_str().unwrap_or("").to_string(),
        checked_at: at,
    })
}

/// Reads the live fields of `numbers` into `pr_live`, one aliased field
/// each. GitHub computes `mergeable` lazily, and the read itself starts the
/// computation, so an UNKNOWN settles by the next read. Returns the number
/// of rows written.
pub async fn refresh_live(store: &Store, client: &mut Client, numbers: &[u64]) -> Result<usize> {
    let mut written = 0;
    for chunk in numbers.chunks(50) {
        let fields: String = chunk
            .iter()
            .map(|n| {
                format!(
                    "p{n}: pullRequest(number: {n}) {{ number headRefOid mergeable authorAssociation \
                     commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ state }} }} }} }} }} "
                )
            })
            .collect();
        let query = format!(
            "{{ rateLimit {{ remaining resetAt }} repository(owner: \"{OWNER}\", name: \"{REPO}\") {{ {fields} }} }}"
        );
        let data = client
            .graphql_allowing(store, &query, Value::Null, &["NOT_FOUND"])
            .await?;
        let now = Utc::now();
        let rows: Vec<crate::store::Live> = data["repository"]
            .as_object()
            .into_iter()
            .flat_map(|o| o.values())
            .filter_map(|pr| live_of(pr, now))
            .collect();
        store.put_live(&rows)?;
        written += rows.len();
    }
    Ok(written)
}

/// Pages of 100 pull requests that [`committed_paths`] reads at most.
const COMMITTED_PAGES: usize = 5;

/// Directories a person changed in merged pull requests over the last year,
/// read with that person's own token, from the newest 500 pull requests.
pub async fn committed_paths(token: &str, login: &str) -> Result<Vec<String>> {
    let since = (Utc::now() - Duration::days(365)).format("%Y-%m-%d");
    let query = "query($q: String!, $after: String) { search(query: $q, type: ISSUE, first: 100, after: $after) {
        pageInfo { hasNextPage endCursor } nodes {
        ... on PullRequest { files(first: 30) { nodes { path } } } } } }";
    let q = format!("repo:{OWNER}/{REPO} is:pr is:merged author:{login} merged:>={since}");
    let http = http();
    let mut out: Vec<String> = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..COMMITTED_PAGES {
        let vars = json!({ "q": q, "after": after });
        let Outcome::Data(data) = graphql_once(&http, token, query, &vars).await? else {
            bail!("token rejected");
        };
        let search = &data["search"];
        out.extend(
            nodes(search)
                .flat_map(|pr| nodes(&pr["files"]))
                .filter_map(|f| f["path"].as_str())
                .map(|p| p.rsplit_once('/').map_or("", |(d, _)| d).to_string())
                .filter(|d| !d.is_empty()),
        );
        after = next_cursor(search);
        if after.is_none() {
            break;
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

pub struct OAuth {
    pub client_id: String,
    pub client_secret: crate::secret::Secret,
    pub public_url: String,
}

impl OAuth {
    pub fn callback(&self) -> String {
        format!("{}/auth/callback", self.public_url.trim_end_matches('/'))
    }

    /// No scope: the token reads public data, and starts workflows only in
    /// the repositories where the person installed the GitHub App.
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
                ("client_secret", self.client_secret.get().as_str()),
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

    /// A GraphQL stand-in: token `limited` is out of budget the way
    /// `status` says, any other token gets data.
    async fn graphql(status: &'static str, body: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/graphql", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let mut buf = [0u8; 8192];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let (status, body) = if req.contains("bearer limited") {
                    (status, body)
                } else {
                    (
                        "200 OK",
                        r#"{"data":{"ok":1,"rateLimit":{"remaining":4000}}}"#,
                    )
                };
                let _ = s
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\nx-ratelimit-remaining: 0\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            }
        });
        url
    }

    #[tokio::test]
    async fn a_token_out_of_budget_hands_over_to_the_next() {
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        for (status, body) in [
            (
                "200 OK",
                r#"{"data":null,"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]}"#,
            ),
            ("403 Forbidden", r#"{"message":"secondary rate limit"}"#),
            ("429 Too Many Requests", "slow down"),
        ] {
            let url = graphql(status, body).await;
            let mut c = Client::at(&url, vec!["limited".into(), "fresh".into()]);
            let v = c.graphql(&store, "{ ok }", Value::Null).await.unwrap();
            assert_eq!(v["ok"], 1, "{status}");
            assert_eq!(c.current, 1, "{status}");
        }
        let url = graphql("429 Too Many Requests", "slow down").await;
        let mut c = Client::at(&url, vec!["limited".into()]);
        assert!(c.graphql(&store, "{ ok }", Value::Null).await.is_err());
        assert!(!c.has_token());
    }

    #[test]
    fn a_lent_token_pays_a_tenth_of_its_budget_per_window() {
        let rate =
            |remaining: i64, reset: &str| json!({ "remaining": remaining, "resetAt": reset });
        let t = "lent-share-test-token";
        assert!(!lent_share_used(t, &rate(5000, "r1")));
        assert!(!lent_share_used(t, &rate(4501, "r1")));
        assert!(lent_share_used(t, &rate(4499, "r1")));
        assert!(
            !lent_share_used(t, &rate(4400, "r2")),
            "a new window starts again"
        );
        assert!(
            lent_share_used(t, &json!({})),
            "no rate limit counts as used up"
        );
    }

    #[test]
    fn void_line_names_a_review() {
        assert_eq!(
            parse_void("thanks\nmaintainer-mode: void https://github.com/NixOS/nixpkgs/pull/1#pullrequestreview-123\n").as_deref(),
            Some("123")
        );
        assert_eq!(
            parse_void("maintainer-mode: void 456").as_deref(),
            Some("456")
        );
        assert_eq!(parse_void("maintainer-mode: void please"), None);
    }

    #[test]
    fn trailer_names_the_tool() {
        assert_eq!(tool_name(" Claude Code (Claude Opus 5.5)"), "claude-code");
        assert_eq!(tool_name("codex-cli 0.162.1 (deepseek-flash)"), "codex-cli");
        assert_eq!(tool_name("opencode (model glm-5.3-flash)"), "opencode");
        assert_eq!(tool_name("Claude-Code"), "claude-code");
        assert_eq!(tool_name("claude:claude-opus-5-5 [nix]"), "claude");
        assert_eq!(tool_name(""), "disclosed");
    }

    #[test]
    fn disclosure_from_trailer_or_label() {
        let n = json!({ "commits": { "nodes": [{ "commit": {
            "message": "foo: 1 -> 2\n\nAssisted-by: pi 1.2.0 (deepseek-flash)" } }] } });
        assert_eq!(ai_marker(&n).as_deref(), Some("pi"));
        let n = json!({ "body": "x", "labels": { "nodes": [{ "name": "llm-assisted" }] } });
        assert_eq!(ai_marker(&n).as_deref(), Some("disclosed"));
        assert_eq!(ai_marker(&json!({ "body": "plain" })), None);
        let n = json!({ "body": "Co-Authored-By: Claude <noreply@anthropic.com>" });
        assert_eq!(ai_marker(&n).as_deref(), Some(OTHER_FORM));
        let n = json!({ "body": "Co-authored-by: Jane <jane@example.org>" });
        assert_eq!(ai_marker(&n), None);
    }

    #[test]
    fn hunk_last_line_decides() {
        assert!(on_change("@@ -1,2 +1,2 @@\n foo\n+bar"));
        assert!(!on_change("@@ -1,2 +1,2 @@\n+bar\n foo"));
    }
}
