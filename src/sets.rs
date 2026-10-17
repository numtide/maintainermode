//! A person's private package set: the nixpkgs attributes of their latest
//! uploaded report, and the packages they watch. Only the owner reads it.
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
use std::collections::BTreeMap;
use std::fmt::Write;

use crate::catalog::{Flags, Subject, subject_row};
use crate::report::{MAX_PACKAGES, Report};
use crate::store::PackageSet;
use crate::ui::{Page, Section, empty, section};
use crate::web::{App, Shared, ago, esc, icon, octicon, random_id};

pub const RETENTION_DAYS: i64 = 90;
const MAX_REPORT_BYTES: usize = 16 * 1024 * 1024;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/me/sets", get(panel))
        .route("/me/sets/delete", post(delete_set))
        .route("/me/upload-token", post(new_token))
        .route("/me/upload-token/delete", post(delete_token))
        .route(
            "/api/v1/reports",
            post(upload).layer(DefaultBodyLimit::max(MAX_REPORT_BYTES)),
        )
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

/// Where a row of the panel comes from.
#[derive(Default)]
struct Origin {
    uploaded: bool,
    watched: bool,
    flake: bool,
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
    let report: Report = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, &format!("unreadable report: {e}")),
    };
    if let Err(e) = report.check() {
        return refuse(StatusCode::BAD_REQUEST, &e.to_string());
    }
    if report.packages.len() > MAX_PACKAGES {
        return refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            &format!("more than {MAX_PACKAGES} packages"),
        );
    }
    let (attrs, dropped) = {
        let index = app.index.read().unwrap();
        if index.rev.is_empty() {
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "the nixpkgs index is not built yet; try again later",
            );
        }
        report.accepted(|a| index.pkgs.contains_key(a))
    };
    let set = PackageSet {
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

const NOTICE: &str = "<div class=\"flash mb-4\"><strong>What the server keeps, and why.</strong> \
For your latest report, it keeps the nixpkgs attributes that its own index knows, the time of the upload, \
the time of the scan and the nixpkgs revision. It drops every other name. It keeps the hash of your upload token, \
not the token. It uses the set for this page and for your private feed only: no public page, number or API reads it, \
and there is no admin view. The set expires 90 days after the last upload. For a public flake that you add below, \
it keeps the reference, the output, the same attributes from the last good scan, and the reason of the last failure; \
the server scans it only while you open this page at least once in 90 days, and those attributes expire 90 days \
after the last good scan or your last visit. The button below deletes the set and your flakes now.</div>";

async fn panel(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let _ = app.store.touch_flake_refs(&login);
    let set = app.store.package_set(&login).ok().flatten();
    let watched: Vec<String> = app
        .store
        .watched(&login)
        .unwrap_or_default()
        .iter()
        .filter_map(|k| match Subject::parse(k) {
            Some(Subject::Pkg(a)) => Some(a),
            _ => None,
        })
        .collect();
    let mut out = String::from(NOTICE);
    let mut from: BTreeMap<String, Origin> = BTreeMap::new();
    for a in set.iter().flat_map(|s| &s.attrs) {
        from.entry(a.clone()).or_default().uploaded = true;
    }
    for a in &watched {
        from.entry(a.clone()).or_default().watched = true;
    }
    for r in app.store.flake_refs(&login).unwrap_or_default() {
        for a in r.attrs {
            from.entry(a).or_default().flake = true;
        }
    }
    out.push_str(&summary(set.as_ref()));
    out.push_str(&crate::rescan::panel(&app, &login));
    out.push_str(&rows(&app, &from));
    out.push_str(&token_box(&app, &login));
    Page::new(Section::Home, "Your package set")
        .icon(octicon!("package"))
        .sub(
            "The packages you run, from your latest report, and the packages you watch. \
             Only you see this page. New advisories and upstream releases for them reach your private Atom feed.",
        )
        .body(out)
        .render(&app, Some(&login))
}

fn summary(set: Option<&PackageSet>) -> String {
    let Some(s) = set else {
        return "<div class=\"Box mb-4\"><div class=\"Box-row color-fg-muted\">No report yet. \
                Make an upload token below, then run <code>maintainer-mode scan</code> and \
                <code>maintainer-mode submit</code> on your machine.</div></div>"
            .into();
    };
    format!(
        "<div class=\"Box mb-4\"><div class=\"Box-row d-flex flex-items-center\"><div class=\"flex-auto\">\
         {} attributes from the report uploaded {} ago, {} entries dropped. Scanned {}. nixpkgs {}. Expires {}.</div>\
         <form method=\"post\" action=\"/me/sets/delete\"><button class=\"btn btn-sm btn-danger\" type=\"submit\">Delete my set</button></form>\
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

fn rows(app: &App, from: &BTreeMap<String, Origin>) -> String {
    if from.is_empty() {
        return String::new();
    }
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let mut list: Vec<(Subject, Flags, &Origin)> = from
        .iter()
        .map(|(a, src)| {
            let sub = Subject::Pkg(a.clone());
            let f = Flags::of(&s, &index, &h, &sub);
            (sub, f, src)
        })
        .collect();
    list.sort_by_key(|(sub, f, _)| (f.rank(), sub.label()));
    let (attention, rest): (Vec<_>, Vec<_>) = list
        .iter()
        .partition(|(_, f, _)| f.advisories > 0 || f.upstream.is_some() || f.risk() >= 15);
    let row = |(sub, f, o): &&(Subject, Flags, &Origin)| {
        let mut from = Vec::new();
        if o.watched {
            from.push("watched");
        }
        if o.uploaded {
            from.push("in your report");
        }
        if o.flake {
            from.push("in a scanned flake");
        }
        subject_row(
            sub,
            f,
            &format!(
                " <span class=\"f6 color-fg-muted\">{}</span>",
                from.join(" · ")
            ),
            "",
        )
    };
    let attention_rows: String = attention.iter().map(row).collect();
    let mut out = section(
        "Needs attention: an open advisory or a newer upstream release",
        Some(attention.len()),
        None,
        &if attention_rows.is_empty() {
            empty("Nothing.")
        } else {
            attention_rows
        },
    );
    let _ = write!(
        out,
        "<details class=\"Box mb-4\"><summary class=\"Box-header\">The rest of the set <span class=\"Counter\">{}</span></summary>",
        rest.len()
    );
    for r in &rest {
        out.push_str(&row(r));
    }
    out.push_str("</details>");
    out
}

fn token_box(app: &App, login: &str) -> String {
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
        "<div id=\"token\">{}</div>",
        section(
            &format!("{} Upload token", icon(octicon!("key"))),
            None,
            None,
            &format!("<div class=\"Box-row\">{state}</div>")
        )
    )
}

fn usage(base: &str) -> String {
    format!(
        "maintainer-mode scan --flake . --output nixosConfigurations.HOST.config.system.build.toplevel\n\
         maintainer-mode export --out report.json\n\
         maintainer-mode submit --to {base} --token-file ~/.config/maintainer-mode/upload-token"
    )
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
         <input class=\"form-control width-full mb-2 text-mono\" readonly value=\"{t}\" aria-label=\"upload token\">\
         <p>Put it in a file that only you can read, or in <code>MAINTAINER_MODE_UPLOAD_TOKEN</code>. Then, on your machine:</p>\
         <pre class=\"p-2 color-bg-subtle\">{u}</pre>\
         <p>Any token you made before no longer works.</p>\
         <a class=\"btn btn-sm\" href=\"/me/sets\">Back to your package set</a></div></div>",
        t = esc(&token),
        u = esc(&usage(base)),
    );
    Page::new(Section::Home, "Your upload token")
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
    Redirect::to("/me/sets#token").into_response()
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
    Redirect::to("/me/sets").into_response()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

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
        let mut index = crate::area::Index::default();
        if !attrs.is_empty() {
            index.rev = "0".repeat(40);
        }
        for a in attrs {
            index.pkgs.insert(a.to_string(), Default::default());
        }
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let events = std::env::temp_dir().join(format!(
            "mm-sets-events-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::sync::Arc::new(App {
            store: crate::store::Store::open(std::path::Path::new(":memory:")).unwrap(),
            oauth: None,
            sync_token: None,
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
            events: std::sync::Arc::new(crate::events::Log::open(&events).unwrap()),
            archive: Vec::new(),
            rescan: None,
            inbox: Default::default(),
            smtp: None,
            pr_cache: Default::default(),
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

    fn report() -> serde_json::Value {
        json!({
            "schema": crate::report::SCHEMA,
            "packages": [
                { "pname": "hello", "attr": "hello", "resolution": "attr" },
                { "pname": "secret", "attr": "our-private-daemon", "resolution": "attr" },
                { "pname": "nameless", "resolution": "flake" },
            ],
        })
    }

    #[tokio::test]
    async fn an_upload_keeps_only_attributes_the_index_knows() {
        let app = app(&["hello"]);
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
        assert_eq!(set.dropped, 2);
        assert_eq!(
            post(&app, "t", json!({ "schema": 99, "packages": [] })).await,
            StatusCode::BAD_REQUEST
        );
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
            .map(|_| json!({ "pname": "hello", "attr": "hello", "resolution": "attr" }))
            .collect();
        let body = json!({ "schema": crate::report::SCHEMA, "packages": packages });
        assert_eq!(post(&app, "t", body).await, StatusCode::PAYLOAD_TOO_LARGE);
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
