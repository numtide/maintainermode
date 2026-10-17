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

use crate::web::{App, Shared};

const VERSIONS: [&str; 2] = ["2025-06-18", "2025-03-26"];
const LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;

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

fn tool(name: &str, title: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required },
        "annotations": { "readOnlyHint": true, "openWorldHint": false },
    })
}

fn limit_prop() -> Value {
    json!({ "type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "description": format!("at most this many rows, default {LIMIT}") })
}

fn tools() -> Vec<Value> {
    let s = |d: &str| json!({ "type": "string", "description": d });
    vec![
        tool(
            "search",
            "Search packages, modules and areas",
            "Find nixpkgs packages, NixOS modules and areas of the tree by name, with their health flags.",
            json!({ "query": s("an attribute, a package name, nixos/NAME, a module file or a path") }),
            &["query"],
        ),
        tool(
            "package",
            "A package",
            "One nixpkgs package: version, newest upstream release, last Hydra build, open advisories, maintainers, and its issues and pull requests.",
            json!({ "name": s("an attribute or a package name, like hello or python3Packages.requests") }),
            &["name"],
        ),
        tool(
            "module",
            "A NixOS module",
            "One NixOS module: its NixOS test on Hydra, maintainers, and its issues and pull requests.",
            json!({ "file": s("the module file, like nixos/modules/services/web-servers/nginx/default.nix, or nixos/NAME") }),
            &["file"],
        ),
        tool(
            "area",
            "An area of nixpkgs",
            "A path of the nixpkgs tree: the packages and modules under it with their flags, and its issues and pull requests.",
            json!({ "path": s("a nixpkgs path, like pkgs/development/python-modules") }),
            &["path"],
        ),
        tool(
            "maintains",
            "What a login maintains",
            "The packages and NixOS modules whose meta.maintainers or meta.teams name a GitHub login, with their flags.",
            json!({ "login": s("a GitHub login") }),
            &["login"],
        ),
        tool(
            "queue_merge",
            "Ready to merge",
            "Pull requests ready to merge: checks pass, no conflict, an approval on the head, and a review that earned credit. Best first.",
            json!({ "limit": limit_prop() }),
            &[],
        ),
        tool(
            "queue_review",
            "Needs review",
            "Listed pull requests that need review, oldest first. bucket: foryou (the area of login, and the caller's subscriptions when login is the caller; the default), unowned, or all. mine is an alias of foryou.",
            json!({
                "bucket": { "type": "string", "enum": ["foryou", "mine", "unowned", "all"] },
                "login": s("whose area for bucket=foryou; default the caller"),
                "limit": limit_prop(),
            }),
            &[],
        ),
        tool(
            "watching",
            "What I watch",
            "The caller's subscriptions on Maintainer Mode, with their health flags.",
            json!({}),
            &[],
        ),
        tool(
            "sync_status",
            "Sync status",
            "What the server has read from GitHub and the other sources, and how fresh it is.",
            json!({}),
            &[],
        ),
    ]
}

fn arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, (i64, String)> {
    args[key]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or((-32602, format!("argument {key} is missing")))
}

fn limit(args: &Value) -> usize {
    args["limit"]
        .as_u64()
        .map_or(LIMIT, |n| (n as usize).clamp(1, MAX_LIMIT))
}

/// The outer error is a bad call; the inner one is a tool result that
/// says what was not found.
async fn call(
    app: &App,
    login: &str,
    name: &str,
    args: &Value,
) -> Result<Result<Value, String>, (i64, String)> {
    let found = |v: Option<Value>, what: &str| {
        v.ok_or_else(|| format!("{what} is not in the nixpkgs index"))
    };
    Ok(match name {
        "search" => Ok(crate::catalog::search_value(app, arg(args, "query")?)),
        "package" => {
            let n = arg(args, "name")?;
            found(crate::catalog::package_value(app, n).await, n)
        }
        "module" => {
            let f = arg(args, "file")?;
            let file = match crate::catalog::Subject::resolve(&app.index.read().unwrap(), f) {
                Some(crate::catalog::Subject::Module(m)) => m,
                _ => f.to_string(),
            };
            found(crate::catalog::module_value(app, &file).await, f)
        }
        "area" => {
            let p = arg(args, "path")?;
            crate::catalog::area_value(app, p).ok_or_else(|| format!("{p} is not a nixpkgs path"))
        }
        "maintains" => Ok(crate::catalog::maintains_value(app, arg(args, "login")?)),
        "queue_merge" => {
            let s = app.snap();
            let q = &s.queues.merge;
            Ok(
                json!({ "total": q.len(), "entries": q.iter().take(limit(args)).collect::<Vec<_>>() }),
            )
        }
        "queue_review" => {
            let asked = args["bucket"].as_str().unwrap_or("foryou");
            let Some(bucket) = crate::web::bucket_name(asked) else {
                return Err((-32602, format!("no bucket {asked}")));
            };
            let who = args["login"].as_str().unwrap_or(login);
            let watched = if who == login {
                crate::web::watched_of(app, Some(login))
            } else {
                Vec::new()
            };
            let s = app.snap();
            let q = crate::web::review_bucket(&s, Some(who), &watched, bucket);
            Ok(json!({
                "bucket": bucket, "login": who, "total": q.len(),
                "entries": q.iter().take(limit(args)).collect::<Vec<_>>(),
            }))
        }
        "watching" => Ok(crate::catalog::watching_value(app, login)),
        "sync_status" => Ok(crate::status::state(app)),
        _ => return Err((-32602, format!("no tool {name}"))),
    })
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
            sync_token: None,
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
            github_app: None,
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
        assert_eq!(v["result"]["tools"].as_array().unwrap().len(), 9);
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
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "watching", "arguments": {} } }),
        )
        .await;
        let text: Value =
            serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text["subscriptions"][0]["key"], "pkg:hello");
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
