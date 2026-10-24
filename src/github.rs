use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use std::time::Duration as StdDuration;

use crate::metrics::{Facts, Seen, areas};
use crate::model::{Issue, Pr, Review, Void};
use crate::store::Store;

const OWNER: &str = "NixOS";
const REPO: &str = "nixpkgs";
/// GitHub times out on large nested pages and answers with an HTML error page
/// rather than JSON. A failed page is asked again, smaller.
const PAGES: [i64; 4] = [25, 10, 5, 5];

const PR_FIELDS: &str = r#"
  number title url isDraft state createdAt updatedAt mergedAt closedAt mergeable headRefOid baseRefName
  additions deletions changedFiles authorAssociation body
  mergedBy { login }
  author { __typename login }
  labels(first: 30) { nodes { name } }
  commits(last: 1) { nodes { commit { message statusCheckRollup { state } } } }
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

const GRAPHQL: &str = "https://api.github.com/graphql";

pub struct Client {
    http: reqwest::Client,
    tokens: Vec<String>,
    current: usize,
    endpoint: String,
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
        }
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
        .await?;
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
        merged_by: n["mergedBy"]["login"].as_str().map(String::from),
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
        closed_at: time(&n["closedAt"]),
        additions: n["additions"].as_u64().unwrap_or(0),
        deletions: n["deletions"].as_u64().unwrap_or(0),
        changed_files: n["changedFiles"].as_u64().unwrap_or(0),
        labels: labels(n),
        association: n["authorAssociation"].as_str().unwrap_or("").to_string(),
        ai: ai_marker(n),
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

async fn page_of(
    client: &mut Client,
    store: &Store,
    open_only: bool,
    after: Option<&str>,
    n: i64,
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
        "owner": OWNER, "name": REPO, "n": n, "after": after,
        "states": if open_only { json!(["OPEN"]) } else { Value::Null },
        "field": if open_only { "CREATED_AT" } else { "UPDATED_AT" },
    });
    let data = client.graphql(store, &query, vars).await?;
    let conn = &data["repository"]["pullRequests"];
    let prs = nodes(conn).filter_map(normalize).collect();
    Ok((prs, next_cursor(conn)))
}

async fn page(
    client: &mut Client,
    store: &Store,
    open_only: bool,
    after: Option<&str>,
) -> Result<(Vec<Pr>, Option<String>)> {
    let mut last = None;
    for n in PAGES {
        match page_of(client, store, open_only, after, n).await {
            Ok(v) => return Ok(v),
            Err(e) if client.has_token() => {
                tracing::warn!("page of {n}: {e:#}");
                last = Some(e);
                tokio::time::sleep(StdDuration::from_secs(5)).await;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.expect("at least one attempt"))
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

const ISSUE_FIELDS: &str = r#"
  number title url state createdAt updatedAt closedAt body
  author { login }
  labels(first: 20) { nodes { name } }
  comments { totalCount }
"#;
const ISSUE_PAGES: [i64; 3] = [100, 50, 20];

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
pub async fn by_number(
    store: &Store,
    client: &mut Client,
    prs: &[u64],
    issues: &[u64],
) -> Result<(Vec<Pr>, Vec<Issue>)> {
    if prs.is_empty() && issues.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut fields = String::new();
    for n in prs {
        fields.push_str(&format!(
            "p{n}: pullRequest(number: {n}) {{ {PR_FIELDS} }} "
        ));
    }
    for n in issues {
        fields.push_str(&format!("i{n}: issue(number: {n}) {{ {ISSUE_FIELDS} }} "));
    }
    let query = format!(
        "{{ rateLimit {{ cost remaining resetAt }} repository(owner: \"{OWNER}\", name: \"{REPO}\") {{ {fields} }} }}"
    );
    let data = client
        .graphql_allowing(store, &query, Value::Null, &["NOT_FOUND"])
        .await?;
    let repo = &data["repository"];
    Ok((
        prs.iter()
            .filter_map(|n| normalize(&repo[format!("p{n}")]))
            .collect(),
        issues
            .iter()
            .filter_map(|n| normalize_issue(&repo[format!("i{n}")]))
            .collect(),
    ))
}

async fn issue_page(
    client: &mut Client,
    store: &Store,
    open_only: bool,
    after: Option<&str>,
) -> Result<(Vec<Issue>, Option<String>)> {
    let mut last = None;
    for n in ISSUE_PAGES {
        let query = format!(
            "query($owner: String!, $name: String!, $n: Int!, $after: String, $states: [IssueState!], $field: IssueOrderField!) {{
               rateLimit {{ cost remaining resetAt }}
               repository(owner: $owner, name: $name) {{
                 issues(first: $n, after: $after, states: $states, orderBy: {{ field: $field, direction: DESC }}) {{
                   pageInfo {{ hasNextPage endCursor }}
                   nodes {{ {ISSUE_FIELDS} }}
                 }}
               }}
             }}"
        );
        let vars = json!({
            "owner": OWNER, "name": REPO, "n": n, "after": after,
            "states": if open_only { json!(["OPEN"]) } else { Value::Null },
            "field": if open_only { "CREATED_AT" } else { "UPDATED_AT" },
        });
        match client.graphql(store, &query, vars).await {
            Ok(data) => {
                let conn = &data["repository"]["issues"];
                let issues = nodes(conn).filter_map(normalize_issue).collect();
                return Ok((issues, next_cursor(conn)));
            }
            Err(e) if client.has_token() => {
                tracing::warn!("issue page of {n}: {e:#}");
                last = Some(e);
                tokio::time::sleep(StdDuration::from_secs(5)).await;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.expect("at least one attempt"))
}

/// The same two walks as `sync`, over issues, with their own cursors.
pub async fn sync_issues(store: &Store, client: &mut Client, window_days: i64) -> Result<usize> {
    let mut count = 0;
    if store.get("issues_open_done")?.is_none() {
        let mut cursor = store.get("issues_open_cursor")?;
        loop {
            let (issues, next) = issue_page(client, store, true, cursor.as_deref()).await?;
            count += issues.len();
            store.put_issues(&issues)?;
            match next {
                Some(c) => {
                    store.set("issues_open_cursor", &c)?;
                    cursor = Some(c);
                }
                None => {
                    store.set("issues_open_done", "1")?;
                    break;
                }
            }
        }
    }
    let started = match store.get_time("issues_recent_started")? {
        Some(t) if store.get("issues_recent_cursor")?.is_some() => t,
        _ => {
            let now = Utc::now();
            store.set("issues_recent_started", &now.to_rfc3339())?;
            now
        }
    };
    let floor = store
        .get_time("issues_watermark")?
        .unwrap_or_else(|| Utc::now() - Duration::days(window_days));
    let mut cursor = store.get("issues_recent_cursor")?;
    loop {
        let (issues, next) = issue_page(client, store, false, cursor.as_deref()).await?;
        let reached = issues.iter().any(|i| i.updated_at < floor);
        count += issues.len();
        store.put_issues(&issues)?;
        match next {
            Some(c) if !reached => {
                store.set("issues_recent_cursor", &c)?;
                cursor = Some(c);
            }
            _ => break,
        }
    }
    store.set(
        "issues_watermark",
        &(started - Duration::minutes(2)).to_rfc3339(),
    )?;
    store.delete_key("issues_recent_cursor")?;
    Ok(count)
}

const HISTORY_FIELDS: &str = r#"
  number state isDraft createdAt updatedAt closedAt mergedAt baseRefName authorAssociation
  additions deletions changedFiles
  author { __typename login } mergedBy { login }
  labels(first: 30) { nodes { name } }
  files(first: 50) { nodes { path } }
  commits(last: 1) { nodes { commit { message } } }
  reviews(first: 10) { nodes { state submittedAt author { __typename login } } }
"#;

/// GitHub answers 502 after 10 s. A page of 100 with these nested fields
/// passes that on older pull requests; the description alone adds 2 to 3 s to
/// a page of 50, so the history reads the AI trailer from the commit only.
const HISTORY_PAGES: [i64; 3] = [50, 25, 10];
/// After a 502 the walk keeps the smaller page for this many pages; slow
/// stretches of history come in runs.
const HISTORY_CALM: usize = 20;

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

async fn history_page(
    client: &mut Client,
    store: &Store,
    after: Option<&str>,
    level: &mut usize,
) -> Result<(Vec<Facts>, Option<String>)> {
    let query = format!(
        "query($owner: String!, $name: String!, $n: Int!, $after: String) {{
           rateLimit {{ cost remaining resetAt }}
           repository(owner: $owner, name: $name) {{
             pullRequests(first: $n, after: $after, orderBy: {{ field: CREATED_AT, direction: DESC }}) {{
               pageInfo {{ hasNextPage endCursor }}
               nodes {{ {HISTORY_FIELDS} }}
             }}
           }}
         }}"
    );
    let mut last = None;
    for (i, n) in HISTORY_PAGES.iter().enumerate().skip(*level) {
        let vars = json!({ "owner": OWNER, "name": REPO, "n": n, "after": after });
        match client.graphql(store, &query, vars).await {
            Ok(data) => {
                let conn = &data["repository"]["pullRequests"];
                *level = i;
                return Ok((nodes(conn).filter_map(facts).collect(), next_cursor(conn)));
            }
            Err(e) if client.has_token() => {
                tracing::warn!("history page of {n}: {e:#}");
                last = Some(e);
                tokio::time::sleep(StdDuration::from_secs(1)).await;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.expect("at least one attempt"))
}

/// Walks every pull request created since `since`, newest first, for the
/// metrics. It stops after `max_pages` and resumes from a stored cursor, so a
/// long first walk never holds up the regular sync. Later changes reach the
/// metrics through the regular sync.
pub async fn history(
    store: &Store,
    client: &mut Client,
    since: DateTime<Utc>,
    max_pages: usize,
) -> Result<usize> {
    let goal = since.to_rfc3339();
    if store.get("history_done")?.as_deref() == Some(goal.as_str()) {
        return Ok(0);
    }
    if store.get("history_since")?.as_deref() != Some(goal.as_str()) {
        store.delete_key("history_cursor")?;
        store.set("history_since", &goal)?;
    }
    let mut cursor = store.get("history_cursor")?;
    let mut count = 0;
    let mut level = 0;
    let mut calm = 0;
    for _ in 0..max_pages {
        let (page, next) = history_page(client, store, cursor.as_deref(), &mut level).await?;
        calm = if level == 0 { 0 } else { calm + 1 };
        if calm >= HISTORY_CALM {
            level -= 1;
            calm = 0;
        }
        let reached = page.iter().any(|f| f.created_at < since);
        count += page.len();
        store.put_facts(&page)?;
        match next {
            Some(c) if !reached => {
                store.set("history_cursor", &c)?;
                cursor = Some(c);
            }
            _ => {
                store.set("history_done", &goal)?;
                store.delete_key("history_cursor")?;
                break;
            }
        }
    }
    Ok(count)
}

/// GitHub computes `mergeable` lazily: a bulk read mostly returns UNKNOWN, and
/// the read itself starts the computation. Asking again for the few merge
/// candidates, one aliased field each, settles them for the next pass.
pub async fn settle_mergeable(
    store: &Store,
    client: &mut Client,
    numbers: &[u64],
) -> Result<usize> {
    let mut settled = 0;
    for chunk in numbers.chunks(50) {
        let fields: String = chunk
            .iter()
            .map(|n| format!("p{n}: pullRequest(number: {n}) {{ number mergeable }} "))
            .collect();
        let query = format!(
            "{{ rateLimit {{ remaining }} repository(owner: \"{OWNER}\", name: \"{REPO}\") {{ {fields} }} }}"
        );
        let data = client
            .graphql_allowing(store, &query, Value::Null, &["NOT_FOUND"])
            .await?;
        for pr in data["repository"]
            .as_object()
            .into_iter()
            .flat_map(|o| o.values())
        {
            if let (Some(n), Some(m)) = (pr["number"].as_u64(), pr["mergeable"].as_str()) {
                if m != "UNKNOWN" {
                    store.set_mergeable(n, m)?;
                    settled += 1;
                }
            }
        }
    }
    Ok(settled)
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
