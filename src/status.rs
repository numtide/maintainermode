//! What the server knows, how fresh it is, and what a forwarded webhook would
//! change. Everything here is read from the database and the snapshot; the
//! page asks no remote service.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::fmt::Write;

use crate::web::{App, Shared, ago, esc, page};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/sync", get(sync_page))
        .route("/api/v1/sync", get(api_sync))
}

fn time(app: &App, key: &str) -> Option<DateTime<Utc>> {
    app.store.get_time(key).ok().flatten()
}

fn has(app: &App, key: &str) -> bool {
    app.store.get(key).ok().flatten().is_some()
}

fn num(app: &App, key: &str) -> Option<i64> {
    app.store.get(key).ok().flatten()?.parse().ok()
}

fn last_event_at(app: &App) -> Option<DateTime<Utc>> {
    let head = app.events.head();
    let last = app
        .events
        .read(head.saturating_sub(1), head, &Default::default(), 1)
        .ok()?
        .pop()?;
    let v: Value = serde_json::from_str(&last.json).ok()?;
    DateTime::parse_from_rfc3339(v["created_at"].as_str()?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Counts of jobs only: no reference, login or package.
fn rescans(app: &App) -> Value {
    let (flakes, failing, last) = app.store.flake_ref_counts().unwrap_or_default();
    let queued = match (&app.rescan, time(app, "index_at")) {
        (Some(_), Some(at)) => app.store.flake_refs_due(at).map(|d| d.len()).unwrap_or(0),
        _ => 0,
    };
    json!({
        "enabled": app.rescan.is_some(),
        "flakes": flakes,
        "queued": queued,
        "failing": failing,
        "last_run": last,
    })
}

pub(crate) fn state(app: &App) -> Value {
    let s = app.snap();
    let (rev, pkgs, modules, teams, checked, with_gaps) = {
        let i = app.index.read().unwrap();
        let gaps: Vec<usize> = i
            .pkgs
            .iter()
            .filter_map(|(a, p)| crate::hygiene::gaps(a, p).map(|g| g.len()))
            .collect();
        (
            i.rev.clone(),
            i.pkgs.len(),
            i.modules.len(),
            i.teams.len(),
            gaps.len(),
            gaps.iter().filter(|n| **n > 0).count(),
        )
    };
    let h = app.health.read().unwrap().clone();
    let channels: Vec<Value> = app
        .channels
        .read()
        .unwrap()
        .list
        .iter()
        .map(|c| {
            json!({
                "channel": c.label(), "rev": (!c.rev.is_empty()).then_some(&c.rev),
                "read_at": c.read_at, "attributes": c.versions.len(),
            })
        })
        .collect();
    let sync_every = num(app, "sync_every").unwrap_or(600);
    let pass_seconds = num(app, "pass_seconds");
    json!({
        "pull_requests": {
            "stored": s.prs,
            "open_walk_done": has(app, "open_done"),
            "last_pass": time(app, "last_sync"),
            "up_to": time(app, "watermark"),
        },
        "issues": {
            "stored": s.issues.len(),
            "open_walk_done": has(app, "issues_open_done"),
            "up_to": time(app, "issues_watermark"),
        },
        "history": { "done": has(app, "history_done") },
        "index": {
            "rev": rev, "at": time(app, "index_at"),
            "packages": pkgs, "modules": modules, "teams": teams,
        },
        "packaging": { "checked": checked, "with_gaps": with_gaps },
        "channels": { "checked_at": time(app, "channels_checked_at"), "list": channels },
        "health": {
            "repology_at": h.repology_at, "outdated": h.upstream.len(),
            "tracker_at": h.tracker_at, "attributes_with_advisories": h.advisories.len(),
            "every_seconds": crate::health::PASS.as_secs(),
            "cves": h.cves.len(),
            "cves_with_score": h.cves.values().filter(|c| c.band.is_some()).count(),
            "sources": h.runs,
        },
        "events": {
            "stored": app.events.head(),
            "newest": last_event_at(app),
            "polling": app.sync_token.is_some() || app.github_app.is_some(),
        },
        "passes": {
            "every_seconds": sync_every,
            "last_ended": time(app, "pass_ended"),
            "last_seconds": pass_seconds,
            "worst_delay_seconds": pass_seconds.map(|p| sync_every + p),
        },
        "rescans": rescans(app),
        "tokens": {
            "server": app.sync_token.is_some() || app.github_app.is_some(),
            "lent": app.store.lent_tokens().map(|t| t.len()).unwrap_or(0),
        },
        "archive": app.archive.iter().map(|a| a.progress(Utc::now()).unwrap_or_else(|e| {
            json!({ "repo": a.repo, "last_error": format!("{e:#}") })
        })).collect::<Vec<_>>(),
    })
}

fn hours(h: f64) -> String {
    if h < 1.0 {
        "less than an hour".into()
    } else if h < 48.0 {
        format!("about {h:.0} hours")
    } else {
        format!("about {:.0} days", h / 24.0)
    }
}

fn archive_row(a: &Value) -> String {
    let mut parts = vec![format!(
        "{} issues and pull requests stored ({:.1} MB)",
        a["items"],
        a["bytes"].as_f64().unwrap_or(0.0) / 1e6
    )];
    match (a["top"].as_i64(), a["oldest"].as_i64()) {
        (None, _) => parts.push("waiting for the first listing".into()),
        (Some(_), None) => parts.push("recent updates first".into()),
        (Some(_), Some(o)) => parts.push(format!("oldest number reached #{o}")),
    }
    if let Some(p) = a["percent"].as_f64() {
        parts.push(format!("{p:.1} % of the repository"));
    }
    if a["complete"].as_bool() == Some(true) {
        parts.push("<span class=\"color-fg-success\">every number read</span>".into());
    } else if let Some(h) = a["hours_left"].as_f64() {
        parts.push(format!("{} left at the current pace", hours(h)));
    }
    parts.push(format!(
        "{} due · {} per pass, a pass every {} · the last ended {}",
        a["due"],
        a["items_per_pass"],
        minutes(crate::archive::PASS_EVERY as i64),
        when(&a["last_pass"])
    ));
    if let Some(n) = a["failed"].as_i64().filter(|n| *n > 0) {
        parts.push(format!(
            "<span class=\"color-fg-attention\">{n} items fail; the walk tries each again a day later</span>"
        ));
    }
    if let Some(e) = a["last_error"].as_str() {
        parts.push(format!(
            "<span class=\"color-fg-attention\">last error: {}</span>",
            esc(e)
        ));
    }
    parts.join(" · ")
}

async fn api_sync(State(app): State<Shared>) -> Json<Value> {
    Json(state(&app))
}

fn when(v: &Value) -> String {
    v.as_str()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| format!("{} ago", ago(t.with_timezone(&Utc))))
        .unwrap_or_else(|| "not yet".into())
}

fn done(v: &Value, yes: &str, no: &str) -> String {
    if v.as_bool() == Some(true) {
        format!("<span class=\"color-fg-success\">{yes}</span>")
    } else {
        format!("<span class=\"color-fg-attention\">{no}</span>")
    }
}

fn row(k: &str, v: &str) -> String {
    format!(
        "<div class=\"Box-row d-flex\"><div class=\"text-bold\" style=\"min-width:180px\">{k}</div><div class=\"flex-auto\">{v}</div></div>"
    )
}

/// The health sources that report a pass: key, row label, what they read.
const SOURCES: [(&str, &str, &str); 6] = [
    (
        "osv",
        "OSV",
        "the exact version of each PyPI, crates.io and npm package, 1000 to a request, once a day",
    ),
    (
        "osv-records",
        "OSV records",
        "the summary and severity of each match, 200 a pass",
    ),
    (
        "upstream",
        "Upstream tags and registries",
        "git tags over HTTPS, crates.io, PyPI and npm; 300 requests a pass, the packages people maintain or watch first; they overrule repology",
    ),
    (
        "cve",
        "Severity",
        "CVE Program records (CVSS and CISA exploitation), 300 a pass",
    ),
    (
        "nvd",
        "NVD",
        "CVSS for the CVEs without a score in their record, 50 a pass, only with an API key",
    ),
    (
        "epss",
        "EPSS",
        "FIRST's chance of exploitation, every tracked CVE once a day",
    ),
];

fn source(run: &Value, what: &str) -> String {
    let Some(at) = run.get("at") else {
        return format!("{what} · not run yet");
    };
    format!(
        "{what} · read {} · {}",
        when(at),
        if run["ok"].as_bool() == Some(true) {
            esc(run["line"].as_str().unwrap_or(""))
        } else {
            format!(
                "<span class=\"color-fg-attention\">{}</span>",
                esc(run["line"].as_str().unwrap_or(""))
            )
        }
    )
}

fn minutes(s: i64) -> String {
    if s < 120 {
        format!("{s} seconds")
    } else {
        format!("{} minutes", s / 60)
    }
}

async fn sync_page(State(app): State<Shared>, headers: HeaderMap) -> Html<String> {
    let viewer = app.viewer(&headers);
    let v = state(&app);
    let mut out = String::from(
        "<div class=\"Subhead\"><h1 class=\"Subhead-heading\">Sync status</h1>\
         <div class=\"Subhead-description\">What this server has read from GitHub and the other sources, and how fresh it is. \
         The same data is at <a href=\"/api/v1/sync\">/api/v1/sync</a>.</div></div>",
    );
    let p = &v["pull_requests"];
    let i = &v["issues"];
    let x = &v["index"];
    let h = &v["health"];
    let e = &v["events"];
    let pass = &v["passes"];
    out.push_str("<div class=\"Box mb-4\"><div class=\"Box-header text-bold\">GitHub</div>");
    out.push_str(&row(
        "Pull requests",
        &format!(
            "{} stored · {} · updates read up to {}",
            p["stored"],
            done(
                &p["open_walk_done"],
                "every open one read",
                "first walk of the open ones in progress"
            ),
            when(&p["up_to"])
        ),
    ));
    out.push_str(&row(
        "Issues",
        &format!(
            "{} stored · {} · updates read up to {}",
            i["stored"],
            done(
                &i["open_walk_done"],
                "every open one read",
                "first walk of the open ones in progress"
            ),
            when(&i["up_to"])
        ),
    ));
    out.push_str(&row(
        "History since 2020",
        &done(
            &v["history"]["done"],
            "complete",
            "still walking back; the state of nixpkgs fills in as it goes",
        ),
    ));
    out.push_str(&row(
        "Tokens",
        &format!(
            "{} · {} lent by people who signed in",
            if v["tokens"]["server"].as_bool() == Some(true) {
                "a server token"
            } else {
                "no server token"
            },
            v["tokens"]["lent"]
        ),
    ));
    out.push_str(&row(
        "Passes",
        &format!(
            "one every {} · the last took {} and ended {}",
            minutes(pass["every_seconds"].as_i64().unwrap_or(600)),
            pass["last_seconds"]
                .as_i64()
                .map(minutes)
                .unwrap_or_else(|| "(not measured yet)".into()),
            when(&pass["last_ended"])
        ),
    ));
    out.push_str("</div><div class=\"Box mb-4\"><div class=\"Box-header text-bold\">nixpkgs and package health</div>");
    out.push_str(&row(
        "Index",
        &format!(
            "<code>{}</code> · {} packages, {} NixOS modules, {} teams · built {}",
            esc(x["rev"]
                .as_str()
                .unwrap_or("")
                .get(..12)
                .unwrap_or("not built yet")),
            x["packages"],
            x["modules"],
            x["teams"],
            when(&x["at"])
        ),
    ));
    for c in v["channels"]["list"].as_array().into_iter().flatten() {
        out.push_str(&row(
            &format!("Channel {}", esc(c["channel"].as_str().unwrap_or(""))),
            &match c["rev"].as_str() {
                Some(rev) => format!(
                    "<code>{}</code> · {} attributes · this revision first seen {}",
                    esc(rev.get(..12).unwrap_or(rev)),
                    c["attributes"],
                    when(&c["read_at"])
                ),
                None => "not read yet: the channels wait for the first master index".into(),
            },
        ));
    }
    out.push_str(&row(
        "Upstream versions",
        &format!(
            "repology, read {} · {} attributes behind upstream",
            when(&h["repology_at"]),
            h["outdated"]
        ),
    ));
    out.push_str(&row(
        "Advisories",
        &format!(
            "security tracker, read {} · {} attributes with advisories",
            when(&h["tracker_at"]),
            h["attributes_with_advisories"]
        ),
    ));
    let pk = &v["packaging"];
    out.push_str(&row(
        "Packaging hygiene",
        &if pk["checked"].as_u64().unwrap_or(0) == 0 {
            "from the index sweep, no network · the next index reads it".to_string()
        } else {
            format!(
                "from the index sweep, no network · {} of {} packages have a gap in meta or no versionCheckHook",
                pk["with_gaps"], pk["checked"]
            )
        },
    ));
    for (key, label, what) in SOURCES {
        out.push_str(&row(label, &source(&h["sources"][key], what)));
    }
    out.push_str(&row(
        "Health task",
        &format!(
            "its own task, one pass every {}: a slow source does not delay the GitHub sync",
            minutes(h["every_seconds"].as_i64().unwrap_or(600))
        ),
    ));
    out.push_str(&row(
        "Hydra builds",
        "asked when a package or module page is shown, kept one hour",
    ));
    let r = &v["rescans"];
    out.push_str(&row(
        "Flake rescans",
        &if r["enabled"].as_bool() == Some(true) {
            format!(
                "{} flakes · {} queued · last scan {} · {} failing",
                r["flakes"],
                r["queued"],
                when(&r["last_run"]),
                r["failing"]
            )
        } else {
            "off on this server: people upload reports from their own machines".into()
        },
    ));
    out.push_str(
        "</div><div class=\"Box mb-4\"><div class=\"Box-header text-bold\">Event log</div>",
    );
    out.push_str(&row(
        "Events",
        &format!(
            "{} stored · newest from {} · {}",
            e["stored"],
            when(&e["newest"]),
            if e["polling"].as_bool() == Some(true) {
                "the Events API is polled, and GH Archive fills the gaps each hour"
            } else {
                "only GH Archive, each hour: there is no server token to poll with"
            }
        ),
    ));
    out.push_str("</div>");
    if let Some(list) = v["archive"].as_array().filter(|l| !l.is_empty()) {
        out.push_str(
            "<div class=\"Box mb-4\" id=\"archive\"><div class=\"Box-header text-bold\">Archive of issues and pull requests</div>",
        );
        for a in list {
            out.push_str(&row(
                &esc(a["repo"].as_str().unwrap_or("")),
                &archive_row(a),
            ));
        }
        out.push_str("</div>");
    }
    let worst = pass["worst_delay_seconds"].as_i64();
    let _ = write!(
        out,
        "<div class=\"Box\"><div class=\"Box-header text-bold\">Faster with forwarded events</div><div class=\"Box-row\">\
         <p>Today this server polls. A review, push or merge on GitHub shows here after the next pass: \
         up to <strong>{}</strong> later{}. The Events API keeps only the last 300 events, so a busy hour loses some until \
         GH Archive brings them, about an hour later.</p>\
         <p>If the NixOS organisation forwarded the nixpkgs webhooks here (an organisation webhook, or a GitHub App \
         installed on the repository):</p>\
         <ul class=\"ml-4\">\
         <li>each change would show in <strong>seconds</strong>, not minutes;</li>\
         <li>no event would be lost in a burst;</li>\
         <li>the queues and the review credit would update the moment a review lands;</li>\
         <li>the passes would only check, so the rate limit would go to the history and to the issue bodies.</li>\
         </ul>\
         <p class=\"color-fg-muted f6\">Maintainer Mode has no webhook receiver yet: GitHub sends webhooks only to the \
         administrators of a repository. See <a href=\"https://github.com/numtide/maintainermode/blob/main/docs/events.md\">docs/events.md</a>.</p>\
         </div></div>",
        worst
            .map(minutes)
            .unwrap_or_else(|| minutes(pass["every_seconds"].as_i64().unwrap_or(600))),
        if worst.is_some() {
            " (the interval plus the last pass)"
        } else {
            ""
        },
    );
    page(&app, viewer.as_deref(), "", &out)
}
