//! A person's private package set: the nixpkgs attributes of their latest
//! uploaded report and of their scanned flakes. Only the owner reads it.
//! Nothing here feeds a public page, number or API.

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{Duration, Utc};
use serde_json::json;
use std::sync::{Arc, Mutex};

use crate::mine::MACHINES;
use crate::report::{MAX_PACKAGES, PublicList, Upload};
use crate::store::PackageSet;
use crate::ui::{Page, Section, section};
use crate::web::{App, Shared, ago, esc, icon, octicon, random_id};

pub const RETENTION_DAYS: i64 = 90;
const MAX_REPORT_BYTES: usize = 16 * 1024 * 1024;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/me/sets/delete", post(delete_set))
        .route("/me/upload-token", post(new_token))
        .route("/me/upload-token/delete", post(delete_token))
        .route(
            "/api/v1/reports",
            post(upload).layer(DefaultBodyLimit::max(MAX_REPORT_BYTES)),
        )
        .route("/api/v1/packages/index", get(public_index))
}

/// The public package list of one index revision, as JSON and as gzip.
pub struct Listing {
    pub list: PublicList,
    json: Bytes,
    gz: Bytes,
}

static LISTING: Mutex<Option<Arc<Listing>>> = Mutex::new(None);

/// The public package list of the current index, built once per revision.
/// `None` before the first index.
pub fn listing(app: &App) -> Option<Arc<Listing>> {
    let index = app.index.read().unwrap();
    if index.rev.is_empty() {
        return None;
    }
    let mut cache = LISTING.lock().unwrap();
    if let Some(l) = cache.as_ref().filter(|l| l.list.rev == index.rev) {
        return Some(l.clone());
    }
    let list = PublicList::of(&index);
    drop(index);
    let json = serde_json::to_vec(&list).ok()?;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    std::io::Write::write_all(&mut gz, &json).ok()?;
    let l = Arc::new(Listing {
        list,
        json: Bytes::from(json),
        gz: Bytes::from(gz.finish().ok()?),
    });
    *cache = Some(l.clone());
    Some(l)
}

async fn public_index(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(l) = tokio::task::spawn_blocking(move || listing(&app))
        .await
        .ok()
        .flatten()
    else {
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "the nixpkgs index is not built yet; try again later",
        );
    };
    let etag = format!("\"{}\"", l.list.rev);
    let fresh = headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|t| t.trim().trim_start_matches("W/") == etag);
    let common = [
        (header::ETAG, etag.clone()),
        (header::CACHE_CONTROL, "public, max-age=3600".to_string()),
        (header::VARY, "Accept-Encoding".to_string()),
    ];
    if fresh {
        return (StatusCode::NOT_MODIFIED, common).into_response();
    }
    let gzip = headers
        .get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|e| e.split(';').next().is_some_and(|e| e.trim() == "gzip"));
    let json = [(header::CONTENT_TYPE, "application/json")];
    if gzip {
        (
            common,
            json,
            [(header::CONTENT_ENCODING, "gzip")],
            l.gz.clone(),
        )
            .into_response()
    } else {
        (common, json, l.json.clone()).into_response()
    }
}

pub fn token_hash(token: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, token.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The attributes of a login's uploaded set and scanned flakes, for the
/// private feed.
pub fn set_attrs(app: &App, login: &str) -> Vec<String> {
    let mut all: std::collections::BTreeSet<String> = app
        .store
        .package_set(login)
        .ok()
        .flatten()
        .map(|s| s.attrs)
        .unwrap_or_default()
        .into_iter()
        .collect();
    for r in app.store.flake_refs(login).unwrap_or_default() {
        all.extend(r.attrs);
    }
    all.into_iter().collect()
}

pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

fn refuse(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

async fn upload(State(app): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(login) =
        bearer(&headers).and_then(|t| app.store.upload_login(&token_hash(t)).ok().flatten())
    else {
        return refuse(StatusCode::UNAUTHORIZED, "missing or unknown upload token");
    };
    let report: Upload = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return refuse(
                StatusCode::BAD_REQUEST,
                &format!("not a strict report: {e}"),
            );
        }
    };
    if report.packages.len() > MAX_PACKAGES {
        return refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            &format!("more than {MAX_PACKAGES} packages"),
        );
    }
    let app2 = app.clone();
    let Some(listing) = tokio::task::spawn_blocking(move || listing(&app2))
        .await
        .ok()
        .flatten()
    else {
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "the nixpkgs index is not built yet; try again later",
        );
    };
    let (attrs, stale) = match report.accepted(&listing.list) {
        Ok(r) => r,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, &e),
    };
    let dropped = stale + report.omitted.unknown;
    let versions = report
        .packages
        .iter()
        .filter(|p| attrs.contains(&p.attr))
        .filter_map(|p| Some((p.attr.clone(), p.version.clone()?)))
        .collect();
    let set = PackageSet {
        versions,
        attrs: attrs.into_iter().collect(),
        dropped,
        uploaded_at: Utc::now(),
        scanned_at: report
            .source
            .scanned_at
            .as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc).to_rfc3339()),
        nixpkgs_rev: report
            .source
            .nixpkgs_rev
            .clone()
            .filter(|r| r.len() <= 64 && r.chars().all(|c| c.is_ascii_hexdigit())),
    };
    if let Err(e) = app.store.put_package_set(&login, &set) {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
    }
    tracing::info!("package set of {login} replaced");
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "login": login,
            "kept": set.attrs.len(),
            "dropped": dropped,
            "expires_at": set.uploaded_at + Duration::days(RETENTION_DAYS),
        })),
    )
        .into_response()
}

/// The latest machine report: when it came, and a button that
/// deletes it.
pub(crate) fn summary(app: &App, login: &str) -> String {
    let Some(s) = app.store.package_set(login).ok().flatten() else {
        return String::new();
    };
    format!(
        "<div class=\"Box mb-3\"><div class=\"Box-row d-flex flex-items-center flex-wrap gap-2\"><div class=\"flex-auto\">\
         <strong>Latest report: uploaded {1} ago.</strong> {0} packages kept, {2} names dropped. Scanned {3}. nixpkgs {4}. Expires {5}.</div>\
         <form method=\"post\" action=\"/me/sets/delete\"><button class=\"btn btn-sm btn-danger\" type=\"submit\">Delete my report</button></form>\
         </div></div>",
        s.attrs.len(),
        ago(s.uploaded_at),
        s.dropped,
        esc(s
            .scanned_at
            .as_deref()
            .map(|t| t.get(..10).unwrap_or(t))
            .unwrap_or("at an unknown time")),
        esc(s
            .nixpkgs_rev
            .as_deref()
            .map(|r| r.get(..12).unwrap_or(r))
            .unwrap_or("unknown")),
        (s.uploaded_at + Duration::days(RETENTION_DAYS)).format("%Y-%m-%d"),
    )
}

/// The `#upload` section of `/me/settings`.
pub(crate) fn token_box(app: &App, login: &str) -> String {
    let state = match app.store.upload_token_at(login).ok().flatten() {
        Some(at) => format!(
            "<p>You made an upload token on {}. The server shows a token once and keeps only its hash.</p>\
             <form method=\"post\" action=\"/me/upload-token\" class=\"d-inline\"><button class=\"btn btn-sm\" type=\"submit\">Replace the token</button></form> \
             <form method=\"post\" action=\"/me/upload-token/delete\" class=\"d-inline\"><button class=\"btn btn-sm btn-danger\" type=\"submit\">Revoke the token</button></form>",
            esc(at.get(..10).unwrap_or(&at))
        ),
        None => "<form method=\"post\" action=\"/me/upload-token\"><button class=\"btn btn-sm btn-primary\" type=\"submit\">Make an upload token</button></form>".into(),
    };
    format!(
        "<div id=\"upload\">{}</div>",
        section(
            &format!("{} Upload token", icon(octicon!("key"))),
            None,
            None,
            &format!(
                "<div class=\"Box-row\"><p>Send the packages of your machines, and they show first in \
                 <a href=\"{MACHINES}\">On my machines</a> on the packages page. Only names that nixpkgs knows leave your machine.</p>\
                 <ol class=\"ml-3\">{}</ol>{state}</div>",
                steps(app.public_url.trim_end_matches('/'), None)
            )
        )
    )
}

const TOKEN_FILE: &str = "~/.config/maintainer-mode/upload-token";

/// The whole way from nothing to a report on the server. `token` is the
/// new token, shown once; without it, step 2 points to the button.
fn steps(base: &str, token: Option<&str>) -> String {
    let step = |title: &str, body: &str, cmd: &str| {
        let pre = if cmd.is_empty() {
            String::new()
        } else {
            format!("<pre class=\"p-2 mt-1 color-bg-subtle\">{}</pre>", esc(cmd))
        };
        format!("<li class=\"mb-3\"><strong>{title}</strong> {body}{pre}</li>")
    };
    let save = format!("mkdir -p ~/.config/maintainer-mode\n(umask 077; cat > {TOKEN_FILE})");
    let token_step = match token {
        Some(_) => step(
            "Save the token.",
            "Run this, paste the token from the box above, then press Enter and Ctrl-D. Only you can read the file.",
            &save,
        ),
        None => step(
            "Make an upload token",
            "with the button below. The server shows it once, with the command that saves it.",
            "",
        ),
    };
    [
        step(
            "Get the command line tool.",
            "It needs Nix with flakes. The first run builds it from source, which takes a few minutes.",
            "nix shell github:numtide/maintainermode",
        ),
        token_step,
        step(
            "Read the packages of your machine.",
            "Run it in the repository of your NixOS configuration, and replace HOST. Repeat <code>--output</code> for more machines. \
             It reads the closure on your machine and writes <code>maintainer-mode-scan.json</code>, which stays there.",
            "maintainer-mode scan --flake . --output nixosConfigurations.HOST.config.system.build.toplevel",
        ),
        step(
            "Send them.",
            "The tool downloads the public package list of the server, keeps only those names, and sends a version only when nixpkgs has it. \
             To read the report before you send it, run <code>export</code> first.",
            &format!(
                "maintainer-mode --server {base} export\nmaintainer-mode submit --to {base} --token-file {TOKEN_FILE}"
            ),
        ),
    ]
    .concat()
}

async fn new_token(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let token = random_id();
    if let Err(e) = app.store.set_upload_token(&login, &token_hash(&token)) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    let base = app.public_url.trim_end_matches('/');
    let out = format!(
        "<div class=\"Box mb-4\"><div class=\"Box-row\">\
         <p>Your new upload token. The server shows it only now. Any token you made before no longer works.</p>\
         <input class=\"form-control width-full mb-3 text-mono\" readonly value=\"{t}\" aria-label=\"upload token\">\
         <ol class=\"ml-3\">{s}</ol>\
         <p class=\"color-fg-muted\">You can also put the token in <code>MAINTAINER_MODE_UPLOAD_TOKEN</code> instead of a file.</p>\
         <a class=\"btn btn-sm\" href=\"/me/settings#upload\">Back to your settings</a></div></div>",
        t = esc(&token),
        s = steps(base, Some(&token)),
    );
    Page::new(
        Section::Personal(crate::ui::Personal::Settings),
        "Your upload token",
    )
    .icon(octicon!("key"))
    .sub("Copy it now. The server keeps only its hash, so this page cannot show it again.")
    .body(out)
    .render(&app, Some(&login))
}

async fn delete_token(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    if let Err(e) = app.store.delete_upload_token(&login) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    Redirect::to("/me/settings#upload").into_response()
}

async fn delete_set(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    if let Err(e) = app
        .store
        .delete_package_set(&login)
        .and_then(|()| app.store.delete_flake_refs(&login))
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    Redirect::to(MACHINES).into_response()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn the_steps_start_with_the_tool_and_name_the_server() {
        let s = steps("https://example.org", None);
        let tool = s.find("nix shell github:numtide/maintainermode").unwrap();
        assert!(tool < s.find("maintainer-mode scan").unwrap());
        assert!(s.contains("--server https://example.org export"));
        assert!(s.contains(
            "submit --to https://example.org --token-file ~/.config/maintainer-mode/upload-token"
        ));
        assert!(!s.contains("umask"));
        assert!(steps("https://example.org", Some("t")).contains("umask 077"));
    }

    #[test]
    fn a_token_hash_is_sha256_hex() {
        assert_eq!(
            token_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn only_a_bearer_header_carries_a_token() {
        let mut h = HeaderMap::new();
        assert_eq!(bearer(&h), None);
        h.insert(header::AUTHORIZATION, "Basic x".parse().unwrap());
        assert_eq!(bearer(&h), None);
        h.insert(header::AUTHORIZATION, "Bearer abc".parse().unwrap());
        assert_eq!(bearer(&h), Some("abc"));
    }

    pub(crate) fn app(attrs: &[&str]) -> Shared {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut index = crate::area::Index::default();
        if !attrs.is_empty() {
            // unique, so the shared `LISTING` cache never mixes two test apps
            index.rev = format!("{n:040}");
        }
        for a in attrs {
            index.pkgs.insert(a.to_string(), Default::default());
        }
        let events =
            std::env::temp_dir().join(format!("mm-sets-events-{}-{n}", std::process::id(),));
        std::sync::Arc::new(App {
            store: crate::store::Store::open(std::path::Path::new(":memory:")).unwrap(),
            oauth: None,
            bot_token: None,
            index: std::sync::RwLock::new(index),
            snapshot: Default::default(),
            secure_cookies: false,
            health: Default::default(),
            channels: Default::default(),
            hydra: Default::default(),
            data_dir: Default::default(),
            index_sets: vec![],
            metrics: Default::default(),
            packages: Default::default(),
            public_url: String::new(),
            github_app: None,
            github_app_slug: None,
            events: std::sync::Arc::new(crate::events::Log::open(&events).unwrap()),
            archive: Vec::new(),
            rescan: None,
            inbox: Default::default(),
            smtp: None,
            pr_cache: Default::default(),
            notifications: Default::default(),
        })
    }

    async fn post(app: &Shared, token: &str, body: serde_json::Value) -> StatusCode {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        upload(State(app.clone()), h, Bytes::from(body.to_string()))
            .await
            .status()
    }

    fn known(app: &Shared, attr: &str, pname: &str, version: &str) {
        let mut index = app.index.write().unwrap();
        let p = index.pkgs.entry(attr.into()).or_default();
        p.pname = pname.into();
        p.version = version.into();
    }

    fn report() -> serde_json::Value {
        json!({
            "schema": crate::report::SCHEMA,
            "packages": [
                { "attr": "hello", "pname": "hello", "version": "2.12.1" },
                { "attr": "our-private-daemon", "pname": "x", "version": null, "modified": true },
            ],
            "omitted": { "unknown": 3 },
        })
    }

    #[tokio::test]
    async fn an_upload_keeps_only_attributes_the_index_knows() {
        let app = app(&["hello"]);
        known(&app, "hello", "hello", "2.12.1");
        app.store
            .set_upload_token("alice", &token_hash("t"))
            .unwrap();
        assert_eq!(
            post(&app, "wrong", report()).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(post(&app, "t", report()).await, StatusCode::ACCEPTED);
        let set = app.store.package_set("alice").unwrap().unwrap();
        assert_eq!(set.attrs, vec!["hello"]);
        assert_eq!(
            set.versions.get("hello").map(String::as_str),
            Some("2.12.1")
        );
        assert_eq!(set.dropped, 4);
        assert_eq!(
            post(&app, "t", json!({ "schema": 99, "packages": [] })).await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn an_upload_that_is_not_strict_is_refused() {
        let app = app(&["hello"]);
        known(&app, "hello", "hello", "2.12.1");
        app.store
            .set_upload_token("alice", &token_hash("t"))
            .unwrap();
        let entry =
            |v: serde_json::Value| json!({ "schema": crate::report::SCHEMA, "packages": [v] });
        for body in [
            entry(json!({ "attr": "hello", "pname": "hello", "version": "1.2.3-acme-fix" })),
            entry(json!({ "attr": "hello", "pname": "hello", "version": null })),
            entry(
                json!({ "attr": "hello", "pname": "hello", "version": "2.12.1", "resolution": "attr" }),
            ),
            json!({ "schema": 1, "source": { "name": "/home/me/tree" }, "packages": [] }),
            json!({ "schema": 1, "packages": [{ "pname": "hello", "version": "2.12.1" }] }),
        ] {
            assert_eq!(post(&app, "t", body).await, StatusCode::BAD_REQUEST);
        }
        assert!(app.store.package_set("alice").unwrap().is_none());
        let ok =
            entry(json!({ "attr": "hello", "pname": "hello", "version": null, "modified": true }));
        assert_eq!(post(&app, "t", ok).await, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn an_upload_waits_for_the_index() {
        let app = app(&[]);
        app.store
            .set_upload_token("alice", &token_hash("t"))
            .unwrap();
        assert_eq!(
            post(&app, "t", report()).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(app.store.package_set("alice").unwrap().is_none());
    }

    #[tokio::test]
    async fn an_upload_has_a_package_limit() {
        let app = app(&["hello"]);
        app.store
            .set_upload_token("alice", &token_hash("t"))
            .unwrap();
        let packages: Vec<_> = (0..=MAX_PACKAGES)
            .map(|_| json!({ "attr": "hello", "pname": "", "version": "" }))
            .collect();
        let body = json!({ "schema": crate::report::SCHEMA, "packages": packages });
        assert_eq!(post(&app, "t", body).await, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn the_public_list_follows_the_index_revision() {
        let empty = app(&[]);
        let r = public_index(State(empty), HeaderMap::new()).await;
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);

        let app = app(&["hello", "python3Packages.requests"]);
        known(&app, "hello", "hello", "2.12.1");
        known(&app, "python3Packages.requests", "requests", "2.32.3");
        let rev = app.index.read().unwrap().rev.clone();
        let r = public_index(State(app.clone()), HeaderMap::new()).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()[header::ETAG], format!("\"{rev}\""));
        assert!(
            r.headers()[header::CACHE_CONTROL]
                .to_str()
                .unwrap()
                .starts_with("public")
        );
        let body = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        let list = PublicList::parse(&body).unwrap();
        assert_eq!(list.attrs["hello"], ("hello".into(), "2.12.1".into()));
        assert_eq!(list.pnames["requests"], vec!["python3Packages.requests"]);

        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT_ENCODING, "br, gzip".parse().unwrap());
        let r = public_index(State(app.clone()), h).await;
        assert_eq!(r.headers()[header::CONTENT_ENCODING], "gzip");
        let gz = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(PublicList::parse(&gz).unwrap().rev, rev);

        let mut h = HeaderMap::new();
        h.insert(header::IF_NONE_MATCH, format!("\"{rev}\"").parse().unwrap());
        let r = public_index(State(app.clone()), h).await;
        assert_eq!(r.status(), StatusCode::NOT_MODIFIED);
    }

    #[test]
    fn the_purge_deletes_only_expired_sets() {
        let app = app(&[]);
        let now = Utc::now();
        for (login, days) in [("old", RETENTION_DAYS + 1), ("new", RETENTION_DAYS - 1)] {
            let set = PackageSet {
                attrs: vec!["hello".into()],
                dropped: 0,
                uploaded_at: now - Duration::days(days),
                scanned_at: None,
                nixpkgs_rev: None,
                versions: Default::default(),
            };
            app.store.put_package_set(login, &set).unwrap();
        }
        let n = app
            .store
            .purge_package_sets(now - Duration::days(RETENTION_DAYS))
            .unwrap();
        assert_eq!(n, 1);
        assert!(app.store.package_set("old").unwrap().is_none());
        assert!(app.store.package_set("new").unwrap().is_some());
    }
}
