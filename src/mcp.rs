//! A read-only MCP server: JSON-RPC 2.0 over the Streamable HTTP transport
//! of the MCP specification 2025-06-18. Each POST carries one message and
//! gets one JSON answer; there is no session and no server-sent stream.
//! Only a person who opted in, with their MCP token, gets an answer.

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

use crate::api::{Args, Cover::By, Fail};
use crate::web::{App, Shared};

const VERSIONS: [&str; 2] = ["2025-06-18", "2025-03-26"];

pub fn routes() -> Router<Shared> {
    Router::new().route("/mcp", post(mcp).get(no_stream).delete(no_stream))
}

async fn no_stream() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        "this server sends no stream; POST one JSON-RPC message",
    )
        .into_response()
}

fn plain(code: StatusCode, msg: &str) -> Response {
    let mut r = (code, msg.to_string()).into_response();
    if code == StatusCode::UNAUTHORIZED {
        r.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"maintainer-mode\""),
        );
    }
    r
}

/// A browser page on another site must not reach the endpoint: DNS
/// rebinding would hand it a token-less local server, and the spec asks
/// for the check.
fn origin_ok(app: &App, headers: &HeaderMap) -> bool {
    let Some(o) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let public = app.public_url.trim_end_matches('/');
    !public.is_empty() && o.trim_end_matches('/') == public
}

async fn mcp(State(app): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(login) = crate::sets::bearer(&headers).and_then(|t| crate::llm::mcp_login(&app, t))
    else {
        return plain(StatusCode::UNAUTHORIZED, "missing or unknown MCP token");
    };
    if !crate::llm::opted_in(&app, &login) {
        return plain(
            StatusCode::FORBIDDEN,
            "the LLM opt-in on /me/settings is off for this login",
        );
    }
    if !origin_ok(&app, &headers) {
        return plain(StatusCode::FORBIDDEN, "foreign Origin");
    }
    if let Some(v) = headers
        .get("mcp-protocol-version")
        .and_then(|v| v.to_str().ok())
    {
        if !VERSIONS.contains(&v) {
            return plain(
                StatusCode::BAD_REQUEST,
                &format!(
                    "unsupported MCP-Protocol-Version {v}; supported: {}",
                    VERSIONS.join(", ")
                ),
            );
        }
    }
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return reply(&error(Value::Null, -32700, &format!("parse error: {e}")));
        }
    };
    if !msg.is_object() || msg["jsonrpc"] != "2.0" {
        return reply(&error(
            Value::Null,
            -32600,
            "one JSON-RPC 2.0 object per request",
        ));
    }
    let Some(method) = msg["method"].as_str() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let Some(id) = msg.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let out = match handle(&app, &login, method, &msg["params"]).await {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, m)) => error(id, code, &m),
    };
    reply(&out)
}

fn reply(v: &Value) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], v.to_string()).into_response()
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

type Rpc = Result<Value, (i64, String)>;

async fn handle(app: &App, login: &str, method: &str, params: &Value) -> Rpc {
    match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or("");
            let version = if VERSIONS.contains(&asked) {
                asked
            } else {
                VERSIONS[0]
            };
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": "maintainer-mode",
                    "title": "Maintainer Mode",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": "Read-only data about nixpkgs: packages, NixOS modules, areas of the tree, \
                    their health, issues and pull requests, the review and merge queues, and what a login maintains. \
                    The tools named my_ and the tools needs, inbox and progress give the caller's own private data. \
                    Each tool gives the same JSON as one endpoint of the JSON API under /api/v1. \
                    No tool writes to GitHub.",
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = params["name"]
                .as_str()
                .ok_or((-32602, "tools/call needs a name".to_string()))?;
            let args = &params["arguments"];
            let result = call(app, login, name, args).await?;
            Ok(match result {
                Ok(v) => json!({
                    "content": [{ "type": "text", "text": v.to_string() }],
                    "isError": false,
                }),
                Err(m) => json!({
                    "content": [{ "type": "text", "text": m }],
                    "isError": true,
                }),
            })
        }
        _ => Err((-32601, format!("no method {method}"))),
    }
}

/// One tool per endpoint of `crate::api::ENDPOINTS` that names one.
fn tools() -> Vec<Value> {
    crate::api::served()
        .filter_map(|e| {
            let By((name, title)) = e.mcp else {
                return None;
            };
            Some(json!({
                "name": name,
                "title": title,
                "description": e.about,
                "inputSchema": crate::api::schema(e),
                "annotations": { "readOnlyHint": true, "openWorldHint": false },
            }))
        })
        .collect()
}

/// The outer error is a bad call; the inner one is a tool result that
/// says what was wrong or not found.
async fn call(
    app: &App,
    login: &str,
    name: &str,
    args: &Value,
) -> Result<Result<Value, String>, (i64, String)> {
    let e = crate::api::tool(name).ok_or((-32602, format!("no tool {name}")))?;
    let a: Args = match args {
        Value::Null => Args::default(),
        v => serde_json::from_value(v.clone()).map_err(|err| (-32602, err.to_string()))?,
    };
    Ok(crate::api::answer(app, e, &a, Some(login))
        .await
        .map(|a| a.value)
        .map_err(Fail::text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> Shared {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let events = std::env::temp_dir().join(format!(
            "mm-mcp-events-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::sync::Arc::new(App {
            store: crate::store::Store::open(std::path::Path::new(":memory:")).unwrap(),
            oauth: None,
            bot_token: None,
            index: Default::default(),
            snapshot: Default::default(),
            secure_cookies: false,
            health: Default::default(),
            hydra: Default::default(),
            data_dir: Default::default(),
            index_sets: vec![],
            metrics: Default::default(),
            packages: Default::default(),
            public_url: "https://mm.example".into(),
            channels: Default::default(),
            archive: Vec::new(),
            rescan: None,
            inbox: Default::default(),
            smtp: None,
            pr_cache: Default::default(),
            notifications: Default::default(),
            github_app: None,
            github_app_slug: None,
            events: std::sync::Arc::new(crate::events::Log::open(&events).unwrap()),
        })
    }

    async fn post(app: &Shared, token: Option<&str>, body: Value) -> (StatusCode, Value) {
        let mut h = HeaderMap::new();
        if let Some(t) = token {
            h.insert(
                header::AUTHORIZATION,
                format!("Bearer {t}").parse().unwrap(),
            );
        }
        let r = mcp(State(app.clone()), h, Bytes::from(body.to_string())).await;
        let code = r.status();
        let bytes = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn opt_in(app: &Shared, login: &str, token: &str) {
        app.store.set_setting(login, "llm_ok", "1").unwrap();
        app.store
            .set_setting(login, "mcp_token_hash", &crate::sets::token_hash(token))
            .unwrap();
    }

    #[tokio::test]
    async fn a_token_and_the_opt_in_are_both_needed() {
        let app = app();
        let list = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
        assert_eq!(
            post(&app, None, list.clone()).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            post(&app, Some("t"), list.clone()).await.0,
            StatusCode::UNAUTHORIZED
        );
        app.store
            .set_setting("alice", "mcp_token_hash", &crate::sets::token_hash("t"))
            .unwrap();
        assert_eq!(
            post(&app, Some("t"), list.clone()).await.0,
            StatusCode::FORBIDDEN
        );
        opt_in(&app, "alice", "t");
        let (code, v) = post(&app, Some("t"), list).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(
            v["result"]["tools"].as_array().unwrap().len(),
            crate::api::served().count()
        );
    }

    #[tokio::test]
    async fn the_handshake_and_a_call_answer_in_json_rpc() {
        let app = app();
        opt_in(&app, "alice", "t");
        let (_, v) = post(
            &app,
            Some("t"),
            json!({ "jsonrpc": "2.0", "id": "a", "method": "initialize",
                    "params": { "protocolVersion": "2099-01-01", "capabilities": {}, "clientInfo": { "name": "x", "version": "1" } } }),
        )
        .await;
        assert_eq!(v["id"], "a");
        assert_eq!(v["result"]["protocolVersion"], VERSIONS[0]);
        let (code, _) = post(
            &app,
            Some("t"),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        )
        .await;
        assert_eq!(code, StatusCode::ACCEPTED);
        app.store.watch("alice", "pkg:hello").unwrap();
        let (_, v) = post(
            &app,
            Some("t"),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "my_packages", "arguments": { "filter": "watch" } } }),
        )
        .await;
        let text: Value =
            serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text["items"][0]["key"], "pkg:hello");
        let (_, v) = post(
            &app,
            Some("t"),
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "package", "arguments": { "name": "nope" } } }),
        )
        .await;
        assert_eq!(v["result"]["isError"], true);
        let (_, v) = post(
            &app,
            Some("t"),
            json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": { "name": "needs", "arguments": { "mood": "mine" } } }),
        )
        .await;
        let text: Value =
            serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text["login"], "alice");
        assert_eq!(text["mood"], "mine");
        assert!(
            text["plans"]["pull_requests"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(text["plans"]["default"]["source"], "built_in");
        let (_, v) = post(
            &app,
            Some("t"),
            json!({ "jsonrpc": "2.0", "id": 4, "method": "resources/list" }),
        )
        .await;
        assert_eq!(v["error"]["code"], -32601);
    }

    #[test]
    fn only_this_site_may_be_the_origin() {
        let app = app();
        let mut h = HeaderMap::new();
        assert!(origin_ok(&app, &h));
        h.insert(header::ORIGIN, "https://mm.example".parse().unwrap());
        assert!(origin_ok(&app, &h));
        h.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        assert!(!origin_ok(&app, &h));
    }
}
