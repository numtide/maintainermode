//! A person's named package sets: the nixpkgs attributes of their uploaded
//! reports and of their scanned flakes. A public flake set shows on the
//! profile. Only the owner reads a private set: on their pages, and in
//! their private API and MCP answers.

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Form, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::json;
use std::fmt::Write;
use std::sync::{Arc, Mutex};

use crate::mine::MACHINES;
use crate::report::{Found, MAX_PACKAGES, PublicList, Report, Source, Upload};
use crate::store::{DEFAULT_SET, MAX_SETS, NamedSet, PackageSet, Put};
use crate::ui::{Page, Section, Tab, section};
use crate::web::{App, Shared, ago, esc, icon, octicon, random_id};

pub const RETENTION_DAYS: i64 = 90;
pub const MAX_NAME: usize = 32;
const MAX_REPORT_BYTES: usize = 16 * 1024 * 1024;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/me/sets/delete", post(delete_set))
        .route("/me/sets/public", post(set_public))
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

/// A set name: 1 to 32 lower-case letters, digits and `-_.`, starting
/// with a letter or a digit. Safe in a URL, in HTML and in a file name.
pub fn check_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "a set name has 1 to {MAX_NAME} characters: a-z, 0-9 and - _ . and starts with a letter or a digit"
        ))
    }
}

/// The attributes of all sets of a login together: the union that Mine,
/// For you, Needs you and the private feed use.
pub fn set_attrs(app: &App, login: &str) -> Vec<String> {
    let all: std::collections::BTreeSet<String> = app
        .store
        .sets(login)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| s.data)
        .flat_map(|d| d.attrs)
        .collect();
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
    let name = report.name.as_deref().unwrap_or(DEFAULT_SET);
    if let Err(e) = check_name(name) {
        return refuse(StatusCode::BAD_REQUEST, &e);
    }
    if let Some(f) = &report.public_flake {
        if let Err(e) = crate::rescan::check_reference(&f.reference)
            .and_then(|()| crate::rescan::check_output(&f.output))
        {
            return refuse(StatusCode::BAD_REQUEST, &format!("not a public flake: {e}"));
        }
    }
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
    let flake = report
        .public_flake
        .as_ref()
        .map(|f| (f.reference.as_str(), f.output.as_str()));
    match app.store.put_package_set(&login, name, &set, flake) {
        Ok(Put::Stored) => {}
        Ok(Put::Scanned) => {
            return refuse(
                StatusCode::CONFLICT,
                &format!("the set {name} is a flake that the server scans; choose another name"),
            );
        }
        Ok(Put::Full) => {
            return refuse(
                StatusCode::CONFLICT,
                &format!("you have {MAX_SETS} sets; delete one first"),
            );
        }
        Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
    tracing::info!("a package set of {login} replaced");
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "login": login,
            "name": name,
            "public": flake.is_some(),
            "kept": set.attrs.len(),
            "dropped": dropped,
            "expires_at": set.uploaded_at + Duration::days(RETENTION_DAYS),
        })),
    )
        .into_response()
}

/// The named sets of their owner, private ones too, as the box "Your
/// sets" shows them.
pub(crate) fn value(app: &App, login: &str) -> serde_json::Value {
    let scans: std::collections::HashMap<String, crate::store::FlakeRef> = app
        .store
        .flake_refs(login)
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.name.clone(), r))
        .collect();
    let sets: Vec<serde_json::Value> = app
        .store
        .sets(login)
        .unwrap_or_default()
        .into_iter()
        .map(|s| {
            let scan = scans.get(&s.name).map(|r| {
                json!({ "last_at": r.last_at, "scanned_at": r.scanned_at, "error": r.error })
            });
            let source = match (&s.flake, s.rescan) {
                (Some(_), true) => "server_scan",
                (Some(_), false) => "flake_upload",
                (None, _) => "upload",
            };
            json!({
                "name": s.name,
                "source": source,
                "public": s.public,
                "reference": s.flake.as_ref().map(|f| &f.0),
                "output": s.flake.as_ref().map(|f| &f.1),
                "uploaded_at": s.data.as_ref().map(|d| d.uploaded_at),
                "scanned_at": s.data.as_ref().and_then(|d| d.scanned_at.clone()),
                "expires_at": s.data.as_ref().map(|d| d.uploaded_at + Duration::days(RETENTION_DAYS)),
                "nixpkgs_rev": s.data.as_ref().and_then(|d| d.nixpkgs_rev.clone()),
                "dropped": s.data.as_ref().map_or(0, |d| d.dropped),
                "packages": s.data.map(|d| d.attrs).unwrap_or_default(),
                "scan": scan,
            })
        })
        .collect();
    json!({ "login": login, "sets": sets })
}

/// The address of the machines filter for one set, or for all of them.
pub(crate) fn machines_href(set: Option<&str>) -> String {
    match set {
        Some(n) => format!("{MACHINES}&set={}", crate::github::urlencode(n)),
        None => MACHINES.to_string(),
    }
}

fn kind(s: &NamedSet) -> &'static str {
    match (&s.flake, s.rescan) {
        (Some(_), true) => "server scan",
        (Some(_), false) => "flake upload",
        (None, _) => "upload",
    }
}

/// The facts of one set, in one line.
fn facts(s: &NamedSet) -> String {
    let Some(d) = &s.data else {
        return "No packages yet.".into();
    };
    let mut out = format!(
        "{} packages, {} names dropped. {} {} ago.",
        d.attrs.len(),
        d.dropped,
        if s.rescan { "Scanned" } else { "Uploaded" },
        ago(d.uploaded_at)
    );
    if !s.rescan {
        let _ = write!(
            out,
            " Scanned {}.",
            esc(d
                .scanned_at
                .as_deref()
                .map(|t| t.get(..10).unwrap_or(t))
                .unwrap_or("at an unknown time"))
        );
    }
    let _ = write!(
        out,
        " nixpkgs {}. Expires {}.",
        esc(d
            .nixpkgs_rev
            .as_deref()
            .map(|r| r.get(..12).unwrap_or(r))
            .unwrap_or("unknown")),
        (d.uploaded_at + Duration::days(RETENTION_DAYS)).format("%Y-%m-%d"),
    );
    out
}

fn small_form(action: &str, name: &str, extra: &str, class: &str, label: &str) -> String {
    format!(
        "<form method=\"post\" action=\"{action}\" class=\"d-inline\"><input type=\"hidden\" name=\"name\" value=\"{}\">{extra}\
         <button class=\"btn btn-sm{class}\" type=\"submit\">{label}</button></form>",
        esc(name)
    )
}

/// The sets box of the machines filter: a picker, then one row per set
/// with its facts, its public switch and its Delete button. `current` is
/// the set the list shows; `None` shows all of them.
pub(crate) fn summary(app: &App, login: &str, current: Option<&str>) -> String {
    let sets = app.store.sets(login).unwrap_or_default();
    if sets.is_empty() {
        return String::new();
    }
    let scans: std::collections::HashMap<String, crate::store::FlakeRef> = app
        .store
        .flake_refs(login)
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.name.clone(), r))
        .collect();
    let index_at = app.store.get_time("index_at").ok().flatten();
    let count = |s: &NamedSet| s.data.as_ref().map_or(0, |d| d.attrs.len());
    let mut tabs = vec![Tab::new(machines_href(None), "All sets", current.is_none())];
    for s in &sets {
        tabs.push(
            Tab::new(
                machines_href(Some(&s.name)),
                &s.name,
                current == Some(s.name.as_str()),
            )
            .count(count(s)),
        );
    }
    let mut out = format!(
        "<div class=\"Box mb-3\" id=\"sets\"><div class=\"Box-header\"><h2 class=\"Box-title\">{} Your sets</h2></div>\
         <div class=\"Box-row pb-0\">{}</div>",
        icon(octicon!("device-desktop")),
        crate::ui::subnav(&tabs).replacen("aria-label=\"Views\"", "aria-label=\"Sets\"", 1)
    );
    for s in &sets {
        let mut labels = format!("<span class=\"Label\">{}</span>", kind(s));
        if s.public {
            let _ = write!(
                labels,
                " <a class=\"Label Label--accent tap-label\" href=\"/u/{}?tab=flakes\">on your profile</a>",
                crate::github::urlencode(login)
            );
        } else {
            labels.push_str(" <span class=\"Label\">private</span>");
        }
        let flake = s
            .flake
            .as_ref()
            .map(|(r, o)| {
                format!(
                    "<div class=\"f6 set-ref\"><code>{}#{}</code></div>",
                    esc(r),
                    esc(o)
                )
            })
            .unwrap_or_default();
        let scan = scans
            .get(&s.name)
            .map(|r| {
                format!(
                    "<div class=\"f6 color-fg-muted\">{}</div>",
                    crate::rescan::state(r, index_at)
                )
            })
            .unwrap_or_default();
        let switch = match (&s.flake, s.public) {
            (Some(_), true) => small_form(
                "/me/sets/public",
                &s.name,
                "<input type=\"hidden\" name=\"public\" value=\"0\">",
                "",
                "Make private",
            ),
            (Some(_), false) => small_form(
                "/me/sets/public",
                &s.name,
                "<input type=\"hidden\" name=\"public\" value=\"1\">",
                "",
                "Show on my profile",
            ),
            (None, _) => String::new(),
        };
        let _ = write!(
            out,
            "<div class=\"Box-row d-flex flex-items-center flex-wrap gap-2\"><div class=\"flex-auto set-ref\">\
             <a class=\"text-bold\" href=\"{}\">{}</a> {labels}\
             <div class=\"f6 color-fg-muted\">{}</div>{flake}{scan}</div>\
             <div class=\"d-flex flex-wrap gap-2\">{switch}{}</div></div>",
            esc(&machines_href(Some(&s.name))),
            esc(&s.name),
            facts(s),
            small_form("/me/sets/delete", &s.name, "", " btn-danger", "Delete"),
        );
    }
    out.push_str("</div>");
    out
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
                 <a href=\"{MACHINES}\">On my machines</a> on the packages page. Only names that nixpkgs knows leave your machine.</p></div>\
                 {}<div class=\"Box-row\"><ol class=\"ml-3\">{}</ol>{state}</div>",
                shared(app),
                steps(app.public_url.trim_end_matches('/'), None)
            )
        )
    )
}

/// A made-up closure through the real filter, so the sample cannot drift
/// from what `submit` sends.
fn sample() -> (Report, Upload) {
    let found = |pname: &str, version: &str| Found {
        pname: pname.into(),
        version: version.into(),
        attr: None,
    };
    let rev = "5f2c9e148a3b";
    let at = "2026-10-20T08:00:00Z";
    let report = Report {
        schema: crate::report::SCHEMA,
        client: String::new(),
        generated_at: at.into(),
        source: Source {
            name: "acme-prod".into(),
            system: "x86_64-linux".into(),
            nixpkgs_rev: Some(rev.into()),
            scanned_at: Some(at.into()),
        },
        packages: vec![
            found("openssl", "3.0.15"),
            found("curl", "8.11.1-acme-fix"),
            found("acme-billing", "2.4.0"),
        ],
        files: None,
    };
    let mut list = PublicList {
        rev: rev.into(),
        attrs: [("openssl", "3.0.15"), ("curl", "8.11.1")]
            .into_iter()
            .map(|(a, v)| (a.to_string(), (a.to_string(), v.to_string())))
            .collect(),
        pnames: Default::default(),
    };
    list.fill_pnames();
    let mut upload = report.strict(&list);
    upload.client = "maintainer-mode/VERSION".into();
    upload.name = Some("laptop".into());
    (report, upload)
}

/// What leaves the machine, what the server keeps and who sees it.
fn shared(app: &App) -> String {
    let (report, upload) = sample();
    let fate = |f: &Found| match upload.packages.iter().find(|e| e.pname == f.pname) {
        Some(e) if !e.modified => "sent with its version, because nixpkgs has this version",
        Some(_) => "sent without its version, marked <code>modified</code>",
        None => "not sent, only counted",
    };
    let rows: String = report
        .packages
        .iter()
        .map(|f| {
            format!(
                "<li><code>{} {}</code>: {}</li>",
                esc(&f.pname),
                esc(&f.version),
                fate(f)
            )
        })
        .collect();
    let json = serde_json::to_string_pretty(&upload).unwrap_or_default();
    let host = app
        .public_url
        .split_once("://")
        .map_or(app.public_url.as_str(), |(_, h)| h)
        .trim_end_matches('/');
    format!(
        "<div class=\"Box-row\" id=\"shared\"><h3 class=\"f5 mb-2\">What leaves your machine</h3>\
         <p>An example. The scan of the machine <code>{name}</code> finds three packages:</p>\
         <ul class=\"ml-3 mb-2\">{rows}<li>the name <code>{name}</code>: never sent. The set name <code>laptop</code> is what you type with <code>--name</code>.</li></ul>\
         <p>The tool then sends only this. No path, no store hash, no host name:</p>\
         <pre class=\"p-2 color-bg-subtle f6 overflow-auto\">{json}</pre></div>\
         <div class=\"Box-row\"><h3 class=\"f5 mb-2\">What the server keeps, and who sees it</h3><ul class=\"ml-3 mb-2\">\
         <li>It keeps the set name, the package names, the versions that nixpkgs has, the count of names not sent, the scan time and the nixpkgs revision.</li>\
         <li>Only you see it, when you sign in. It never shows on public pages or in public numbers. Only a public flake that you mark with <code>--public-flake</code> shows on your profile.</li>\
         <li>The admin of {host} can read the server database.</li>\
         <li>The server deletes it {RETENTION_DAYS} days after the upload. <em>Delete my report</em> on <a href=\"{MACHINES}\">On my machines</a> deletes it now.</li></ul>\
         <p class=\"mb-0\"><strong>The risk.</strong> The report tells which public packages run on your machines, and their versions. \
         A person who reads it can find the known advisories that can affect you. \
         Send only machines that you can share at that level.</p></div>",
        name = esc(&report.source.name),
        json = esc(&json),
        host = esc(host),
    )
}

const TOKEN_FILE: &str = "~/.config/maintainer-mode/upload-token";
pub(crate) const PUBLIC_SERVER: &str = "https://maintainermode.org";

/// The whole way from nothing to a report on the server. `token` is the
/// new token, shown once; without it, step 2 is `login`.
fn steps(base: &str, token: Option<&str>) -> String {
    let server = if base == PUBLIC_SERVER {
        String::new()
    } else {
        format!(" --server {base}")
    };
    let step = |title: &str, body: &str, cmd: &str| {
        let pre = if cmd.is_empty() {
            String::new()
        } else {
            format!(
                "<pre class=\"p-2 mt-1 color-bg-subtle overflow-auto\">{}</pre>",
                esc(cmd)
            )
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
            "Sign in.",
            &format!(
                "The tool shows a link and a code. Open the link, check the code and confirm. \
                 The tool saves an upload token in <code>{TOKEN_FILE}</code>, which only you can read. \
                 It replaces any token you made before. For CI, the button below makes a token to put in <code>MAINTAINER_MODE_UPLOAD_TOKEN</code>."
            ),
            &format!("maintainer-mode{server} login"),
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
            "Run it in the repository of your NixOS configuration. It reads the outputs of the flake that this machine can evaluate: \
             your machines, your packages, your devShells and your checks. It writes <code>maintainer-mode-scan.json</code>, which stays there. \
             For one machine only, name it with <code>--output nixosConfigurations.HOST.config.system.build.toplevel</code>.",
            "maintainer-mode scan --flake .",
        ),
        step(
            "Send them.",
            "The tool downloads the public package list of the server, keeps only those names, and sends a version only when nixpkgs has it. \
             <code>export</code> prints the exact report that <code>submit</code> sends. Read it first. \
             Give each flake its own set with <code>--name</code>, such as <code>--name home</code>. A new upload replaces the set of that name.",
            &format!(
                "maintainer-mode{server} export\nmaintainer-mode{server} submit --name home"
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
         <p class=\"color-fg-muted\">You can also put the token in <code>MAINTAINER_MODE_UPLOAD_TOKEN</code> instead of a file. \
         <a href=\"/me/settings#shared\">What leaves your machine</a> shows an example report and the risk.</p>\
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

#[derive(Deserialize)]
struct NameForm {
    /// With no name, every set goes.
    name: Option<String>,
}

async fn delete_set(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<NameForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let done = match f.name.as_deref() {
        Some(n) => app.store.delete_package_set(&login, n),
        None => app.store.delete_package_sets(&login),
    };
    if let Err(e) = done {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    Redirect::to(MACHINES).into_response()
}

#[derive(Deserialize)]
struct PublicForm {
    name: String,
    public: u8,
}

/// Only a set whose flake reference passes the public-reference check
/// can show on the profile.
async fn set_public(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<PublicForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let public = f.public == 1;
    if public {
        let flake = app
            .store
            .sets(&login)
            .unwrap_or_default()
            .into_iter()
            .find(|s| s.name == f.name)
            .and_then(|s| s.flake);
        let Some((reference, output)) = flake else {
            return (
                StatusCode::BAD_REQUEST,
                "only a public flake can show on your profile",
            )
                .into_response();
        };
        if let Err(e) = crate::rescan::check_reference(&reference)
            .and_then(|()| crate::rescan::check_output(&output))
        {
            return (StatusCode::BAD_REQUEST, e).into_response();
        }
    }
    match app.store.set_public(&login, &f.name, public) {
        Ok(true) => Redirect::to(&machines_href(Some(&f.name))).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no flake set of that name").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn the_steps_start_with_the_tool_and_name_the_server() {
        let s = steps("https://example.org", None);
        let tool = s.find("nix shell github:numtide/maintainermode").unwrap();
        assert!(tool < s.find("maintainer-mode scan").unwrap());
        assert!(s.contains("--server https://example.org login"));
        assert!(s.contains("--server https://example.org export"));
        assert!(s.contains("--server https://example.org submit --name home"));
        assert!(!s.contains("umask"));
        assert!(steps("https://example.org", Some("t")).contains("umask 077"));
        let public = steps(PUBLIC_SERVER, None);
        assert!(public.contains("maintainer-mode login"));
        assert!(!public.contains("--server"));
    }

    #[test]
    fn the_sample_shows_each_rule_of_the_strict_filter() {
        let (_, upload) = sample();
        let json = serde_json::to_string(&upload).unwrap();
        assert!(json.contains(r#""attr":"openssl","pname":"openssl","version":"3.0.15""#));
        assert!(json.contains(r#""attr":"curl","pname":"curl","version":null,"modified":true"#));
        assert!(!json.contains("acme"));
        assert_eq!(upload.omitted.unknown, 1);
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
        let set = app
            .store
            .package_set("alice", DEFAULT_SET)
            .unwrap()
            .unwrap();
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
        assert!(
            app.store
                .package_set("alice", DEFAULT_SET)
                .unwrap()
                .is_none()
        );
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
        assert!(
            app.store
                .package_set("alice", DEFAULT_SET)
                .unwrap()
                .is_none()
        );
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

    fn data(attrs: &[&str], days: i64) -> PackageSet {
        PackageSet {
            attrs: attrs.iter().map(|a| a.to_string()).collect(),
            dropped: 0,
            uploaded_at: Utc::now() - Duration::days(days),
            scanned_at: None,
            nixpkgs_rev: None,
            versions: Default::default(),
        }
    }

    #[test]
    fn the_purge_deletes_only_expired_sets() {
        let app = app(&[]);
        let s = &app.store;
        for (name, days) in [("old", RETENTION_DAYS + 1), ("new", RETENTION_DAYS - 1)] {
            s.put_package_set("alice", name, &data(&["hello"], days), None)
                .unwrap();
        }
        let n = s
            .purge_package_sets(Utc::now() - Duration::days(RETENTION_DAYS))
            .unwrap();
        assert_eq!(n, 1);
        assert!(s.package_set("alice", "old").unwrap().is_none());
        assert!(s.package_set("alice", "new").unwrap().is_some());
    }

    #[test]
    fn a_set_name_is_short_and_safe() {
        for ok in ["default", "laptop", "web-1", "a", "x86_64.box", "1st"] {
            assert_eq!(check_name(ok), Ok(()), "{ok}");
        }
        for bad in [
            "",
            "Laptop",
            "-x",
            ".hidden",
            "a b",
            "a/b",
            "a#b",
            "<b>",
            "é",
            &"x".repeat(MAX_NAME + 1),
        ] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }

    fn named(name: &str, attrs: &[&str]) -> serde_json::Value {
        let packages: Vec<_> = attrs
            .iter()
            .map(|a| json!({ "attr": a, "pname": a, "version": "1.0" }))
            .collect();
        json!({ "schema": crate::report::SCHEMA, "name": name, "packages": packages })
    }

    fn uploader(attrs: &[&str]) -> Shared {
        let app = app(attrs);
        for a in attrs {
            known(&app, a, a, "1.0");
        }
        app.store
            .set_upload_token("alice", &token_hash("t"))
            .unwrap();
        app
    }

    #[tokio::test]
    async fn each_name_has_its_own_set_and_its_own_delete() {
        let app = uploader(&["hello", "jq", "curl"]);
        let s = &app.store;
        assert_eq!(
            post(&app, "t", named("laptop", &["hello"])).await,
            StatusCode::ACCEPTED
        );
        assert_eq!(
            post(&app, "t", named("web-1", &["jq"])).await,
            StatusCode::ACCEPTED
        );
        assert_eq!(
            post(&app, "t", named("laptop", &["curl"])).await,
            StatusCode::ACCEPTED
        );
        let names: Vec<String> = s
            .sets("alice")
            .unwrap()
            .into_iter()
            .map(|x| x.name)
            .collect();
        assert_eq!(names, ["laptop", "web-1"]);
        assert_eq!(
            s.package_set("alice", "laptop").unwrap().unwrap().attrs,
            ["curl"]
        );
        assert_eq!(set_attrs(&app, "alice"), ["curl", "jq"]);
        for bad in ["Laptop", "../x", ""] {
            assert_eq!(
                post(&app, "t", named(bad, &["hello"])).await,
                StatusCode::BAD_REQUEST
            );
        }
        s.delete_package_set("alice", "laptop").unwrap();
        assert_eq!(set_attrs(&app, "alice"), ["jq"]);
        s.delete_package_sets("alice").unwrap();
        assert!(s.sets("alice").unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_name_of_a_scanned_flake_and_the_set_limit_are_kept() {
        let app = uploader(&["hello"]);
        let s = &app.store;
        assert!(matches!(
            s.add_flake_ref("alice", "web", "github:o/r", "x", false, 3)
                .unwrap(),
            crate::store::AddFlake::Added
        ));
        assert_eq!(
            post(&app, "t", named("web", &["hello"])).await,
            StatusCode::CONFLICT
        );
        for n in 1..MAX_SETS {
            s.put_package_set("alice", &format!("m{n}"), &data(&["hello"], 0), None)
                .unwrap();
        }
        assert_eq!(
            post(&app, "t", named("one-more", &["hello"])).await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            post(&app, "t", named("m1", &["hello"])).await,
            StatusCode::ACCEPTED
        );
    }

    #[tokio::test]
    async fn only_a_public_reference_makes_a_public_set() {
        let app = uploader(&["hello"]);
        let s = &app.store;
        let mut body = named("site", &["hello"]);
        body["public_flake"] = json!({ "reference": "https://127.0.0.1/x.tar.gz", "output": "x" });
        assert_eq!(post(&app, "t", body.clone()).await, StatusCode::BAD_REQUEST);
        body["public_flake"] = json!({ "reference": "github:o/r", "output": "x", "path": "/home" });
        assert_eq!(post(&app, "t", body.clone()).await, StatusCode::BAD_REQUEST);
        body["public_flake"] =
            json!({ "reference": "github:o/r", "output": "packages.x86_64-linux.default" });
        assert_eq!(post(&app, "t", body).await, StatusCode::ACCEPTED);
        assert_eq!(
            post(&app, "t", named("laptop", &["hello"])).await,
            StatusCode::ACCEPTED
        );
        let public: Vec<String> = s
            .public_sets("alice")
            .unwrap()
            .into_iter()
            .map(|x| x.name)
            .collect();
        assert_eq!(public, ["site"]);
        assert!(!s.set_public("alice", "laptop", true).unwrap());
        assert!(s.set_public("alice", "site", false).unwrap());
        assert!(s.public_sets("alice").unwrap().is_empty());
        assert!(s.set_public("alice", "site", true).unwrap());
        assert_eq!(
            post(&app, "t", named("site", &["hello"])).await,
            StatusCode::ACCEPTED
        );
        assert!(
            s.public_sets("alice").unwrap().is_empty(),
            "a private upload replaces the flake"
        );
    }

    async fn page(app: &Shared, set: Option<&str>) -> String {
        let i = crate::mine::Interests::of(app, "alice");
        let v = crate::mine::view(
            app,
            "alice",
            &i,
            crate::mine::Filter::Machines,
            "",
            None,
            set,
        );
        v.body
    }

    #[tokio::test]
    async fn the_filter_shows_one_set_or_all_of_them() {
        let app = uploader(&["hello", "jq"]);
        assert_eq!(
            post(&app, "t", named("laptop", &["hello"])).await,
            StatusCode::ACCEPTED
        );
        assert_eq!(
            post(&app, "t", named("web-1", &["jq"])).await,
            StatusCode::ACCEPTED
        );
        let all = page(&app, None).await;
        assert!(all.contains("href=\"/p/hello\"") && all.contains("href=\"/p/jq\""));
        assert!(all.contains("href=\"/packages?filter=machines&amp;set=web-1\""));
        assert!(all.contains("aria-current=\"page\">All sets"));
        let one = page(&app, Some("laptop")).await;
        assert!(one.contains("href=\"/p/hello\"") && !one.contains("href=\"/p/jq\""));
        assert!(one.contains("Packages of laptop"));
        assert!(one.contains("name=\"set\" value=\"laptop\""));
        let unknown = page(&app, Some("nope")).await;
        assert!(
            unknown.contains("href=\"/p/jq\"")
                && unknown.contains("aria-current=\"page\">All sets")
        );
        let i = crate::mine::Interests::of(&app, "alice");
        assert_eq!(i.count(crate::mine::Filter::Machines), 2);
    }

    #[tokio::test]
    async fn a_private_set_never_reaches_the_profile() {
        let app = uploader(&["hello", "jq"]);
        assert_eq!(
            post(&app, "t", named("secret-box", &["jq"])).await,
            StatusCode::ACCEPTED
        );
        let v = crate::profile::profile_value(&app, "alice");
        assert_eq!(v["flakes"], json!([]));
        let mut body = named("site", &["hello"]);
        body["public_flake"] = json!({ "reference": "github:o/r", "output": "x" });
        assert_eq!(post(&app, "t", body).await, StatusCode::ACCEPTED);
        let v = crate::profile::profile_value(&app, "alice");
        assert_eq!(v["flakes"][0]["name"], "site");
        assert_eq!(v["flakes"][0]["packages"], json!(["hello"]));
        assert_eq!(v["flakes"][0]["source"], "upload");
        let text = v.to_string();
        assert!(!text.contains("secret-box") && !text.contains("jq"));
        let person = crate::api::person_value(&app, "alice").to_string();
        assert!(!person.contains("secret-box") && person.contains("\"site\""));
    }
}
