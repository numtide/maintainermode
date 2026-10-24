//! What the server knows, how fresh it is, and what a forwarded webhook would
//! change. Everything here is read from the database and the snapshot; the
//! page asks no remote service.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::fmt::Write;

use crate::ui::kv;
use crate::web::{App, ago, esc};

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

fn live(app: &App) -> Value {
    let (open, checked, oldest) = app.store.live_progress().unwrap_or_default();
    json!({
        "open_pull_requests": open,
        "checked": checked,
        "oldest_check": oldest,
        "per_pass": crate::LIVE_BATCH,
    })
}

/// Counts of jobs only: no reference, login or package.
fn rescans(app: &App) -> Value {
    let (flakes, failing, last) = app.store.flake_ref_counts().unwrap_or_default();
    let queued = match (&app.rescan, time(app, "index_at")) {
        (Some(_), Some(at)) => app
            .store
            .flake_refs_due(at, crate::rescan::seen_since())
            .map(|d| d.len())
            .unwrap_or(0),
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

/// Counts of emails only: no address or login.
fn email(app: &App) -> Value {
    let (sent, skipped) = app
        .store
        .mail_counts(Utc::now() - chrono::Duration::days(1))
        .unwrap_or_default();
    json!({
        "enabled": app.smtp.is_some(),
        "per_day": app.smtp.as_ref().map(|s| s.per_day),
        "sent_24h": sent,
        "skipped_24h": skipped,
    })
}

fn notifications(app: &App, sync_every: i64) -> Value {
    let now = Utc::now();
    let mut v = app.notifications.json(now);
    v["walk_every_seconds"] = json!(if app.notifications.healthy(now) {
        crate::notifications::WALK_EVERY_WHEN_HEALTHY.max(sync_every)
    } else {
        sync_every
    });
    v
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
        "live": live(app),
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
            "polling": app.bot_token.is_some(),
        },
        "passes": {
            "every_seconds": sync_every,
            "last_ended": time(app, "pass_ended"),
            "last_seconds": pass_seconds,
            "worst_delay_seconds": pass_seconds.map(|p| sync_every + p),
        },
        "notifications": notifications(app, sync_every),
        "hydra": app.hydra.stats(),
        "hydra_builds": app.hydra.builds.stats(),
        "rescans": rescans(app),
        "email": email(app),
        "tokens": {
            "server": app.bot_token.is_some() || app.github_app.is_some(),
            "bot": app.bot_token.is_some(),
            "app": app.github_app.is_some(),
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
    if a["open_complete"].as_bool() == Some(false) {
        parts.push(if a["open_listed"].as_bool() == Some(true) {
            "open items listed, not all stored yet".into()
        } else {
            "listing the open items".into()
        });
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
        "{} due · {} per run, a run every {} · the last ended {}",
        a["due"],
        a["items_per_pass"],
        minutes(crate::archive::PASS_EVERY as i64),
        when(&a["last_pass"])
    ));
    if let Some(n) = a["failed"].as_i64().filter(|n| *n > 0) {
        parts.push(format!(
            "<span class=\"color-fg-attention\">{n} items fail; the server tries each again an hour later, then a day later</span>"
        ));
    }
    if let Some(s) = a["shadow"].as_object() {
        let mut fields: Vec<&str> = s["examples"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|e| e["fields"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .collect();
        fields.sort_unstable();
        fields.dedup();
        parts.push(format!(
            "projection check: {} of {} rows differ from the sync{} ({} not comparable), {}",
            s["differ"],
            s["compared"],
            if fields.is_empty() {
                String::new()
            } else {
                format!(" in {}", esc(&fields.join(", ")))
            },
            s["skipped"],
            when(&s["at"])
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
        "the summary and severity of each match, 200 a run",
    ),
    (
        "upstream",
        "Upstream tags and registries",
        "git tags over HTTPS, crates.io, PyPI and npm; 300 requests a run, the packages people maintain or watch first; they overrule repology",
    ),
    (
        "cve",
        "Severity",
        "CVE Program records (CVSS and CISA exploitation), 300 a run",
    ),
    (
        "nvd",
        "NVD",
        "CVSS for the CVEs without a score in their record, 50 a run, only with an API key",
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

/// The Data status tab of `/state`.
pub(crate) fn body(app: &App) -> String {
    let v = state(app);
    let mut out = String::new();
    let p = &v["pull_requests"];
    let i = &v["issues"];
    let x = &v["index"];
    let h = &v["health"];
    let e = &v["events"];
    let pass = &v["passes"];
    out.push_str("<div class=\"Box mb-4\"><div class=\"Box-header\"><h2 class=\"Box-title\">GitHub</h2></div>");
    out.push_str(&kv(
        "Pull requests",
        format!(
            "{} stored · {} · updates read up to {}",
            p["stored"],
            done(
                &p["open_walk_done"],
                "every open one read",
                "first read of the open ones in progress"
            ),
            when(&p["up_to"])
        ),
    ));
    out.push_str(&kv(
        "Issues",
        format!(
            "{} stored · {} · updates read up to {}",
            i["stored"],
            done(
                &i["open_walk_done"],
                "every open one read",
                "first read of the open ones in progress"
            ),
            when(&i["up_to"])
        ),
    ));
    let l = &v["live"];
    out.push_str(&kv(
        "Mergeable and checks",
        format!(
            "{} of {} open pull requests checked · {} per update, the oldest check first · oldest check {}",
            l["checked"],
            l["open_pull_requests"],
            l["per_pass"],
            when(&l["oldest_check"])
        ),
    ));
    out.push_str(&kv(
        "History since 2020",
        done(
            &v["history"]["done"],
            "complete",
            "in progress: the numbers on the state of nixpkgs grow while it reads",
        ),
    ));
    out.push_str(&kv(
        "Tokens",
        format!(
            "{} · {} · {} lent by people who signed in",
            if v["tokens"]["app"].as_bool() == Some(true) {
                "a GitHub App token"
            } else {
                "no GitHub App"
            },
            if v["tokens"]["bot"].as_bool() == Some(true) {
                "the bot token"
            } else {
                "no bot token"
            },
            v["tokens"]["lent"]
        ),
    ));
    out.push_str(&kv(
        "Updates",
        format!(
            "one every {} · the last took {} and ended {}",
            minutes(pass["every_seconds"].as_i64().unwrap_or(600)),
            pass["last_seconds"]
                .as_i64()
                .map(minutes)
                .unwrap_or_else(|| "(not measured yet)".into()),
            when(&pass["last_ended"])
        ),
    ));
    let n = &v["notifications"];
    let on = n["enabled"].as_bool() == Some(true);
    let healthy = n["healthy"].as_bool() == Some(true);
    out.push_str(&kv(
        "Notifications",
        &if on {
            notifications_row(n)
        } else {
            "off: this server has no token of a bot account, so a change waits for the next update"
                .to_string()
        },
    ));
    out.push_str(&kv(
        "Full walk",
        format!(
            "every update reads the pull requests and issues that changed since the last walk, \
             one every {} · {}",
            minutes(n["walk_every_seconds"].as_i64().unwrap_or(600)),
            if healthy {
                "the notifications poller is healthy, so the walk is only the safety net"
                    .to_string()
            } else {
                format!(
                    "it runs every {} while the notifications poller is healthy",
                    minutes(
                        crate::notifications::WALK_EVERY_WHEN_HEALTHY
                            .max(n["walk_every_seconds"].as_i64().unwrap_or(600))
                    )
                )
            }
        ),
    ));
    out.push_str("</div><div class=\"Box mb-4\"><div class=\"Box-header\"><h2 class=\"Box-title\">nixpkgs and package health</h2></div>");
    out.push_str(&kv(
        "Index",
        format!(
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
        out.push_str(&kv(
            format!("Channel {}", esc(c["channel"].as_str().unwrap_or(""))),
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
    out.push_str(&kv(
        "Upstream versions",
        format!(
            "repology, read {} · {} attributes behind upstream",
            when(&h["repology_at"]),
            h["outdated"]
        ),
    ));
    out.push_str(&kv(
        "Advisories",
        format!(
            "security tracker, read {} · {} attributes with advisories",
            when(&h["tracker_at"]),
            h["attributes_with_advisories"]
        ),
    ));
    let pk = &v["packaging"];
    out.push_str(&kv(
        "Packaging hygiene",
        &if pk["checked"].as_u64().unwrap_or(0) == 0 {
            "from the nixpkgs index, with no network access · the next index reads it".to_string()
        } else {
            format!(
                "from the nixpkgs index, with no network access · {} of {} packages have a gap in meta or no versionCheckHook",
                pk["with_gaps"], pk["checked"]
            )
        },
    ));
    for (key, label, what) in SOURCES {
        out.push_str(&kv(label, source(&h["sources"][key], what)));
    }
    out.push_str(&kv(
        "Health task",
        format!(
            "its own task, one run every {}: a slow source does not delay the GitHub updates",
            minutes(h["every_seconds"].as_i64().unwrap_or(600))
        ),
    ));
    let hy = &v["hydra"];
    out.push_str(&kv(
        "Hydra builds",
        format!(
            "asked when a package or module page is shown, at most {} at a time and {} a minute, kept one hour · \
             {} jobs kept · {} page views not asked since the start",
            hy["concurrent"], hy["per_minute"], hy["cached"], hy["not_asked"]
        ),
    ));
    let b = &v["hydra_builds"];
    out.push_str(&kv(
        "Hydra build watch",
        format!(
            "{} packages that someone maintains or watches · the feed of finished builds every {} ({} requests, {} rows, last {}) · \
             one job at a time, {} a minute, each job once a day ({} requests) · {} failed requests · {} state changes since the start · {} cache.nixos.org requests, {} paths known",
            b["tracked"],
            minutes(b["feed_every_seconds"].as_i64().unwrap_or(300)),
            b["feed_requests"],
            b["feed_rows"],
            when(&b["feed_at"]),
            b["sweep_per_minute"],
            b["sweep_requests"],
            b["failed_requests"],
            b["changes"],
            b["cache_requests"],
            b["cache_known"]
        ),
    ));
    let r = &v["rescans"];
    out.push_str(&kv(
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
        "</div><div class=\"Box mb-4\"><div class=\"Box-header\"><h2 class=\"Box-title\">Event log</h2></div>",
    );
    out.push_str(&kv(
        "Events",
        format!(
            "{} stored · newest from {} · {}",
            e["stored"],
            when(&e["newest"]),
            if e["polling"].as_bool() == Some(true) {
                "the Events API is polled, and GH Archive fills the gaps each hour"
            } else {
                "only GH Archive, each hour: there is no bot token to poll with"
            }
        ),
    ));
    let m = &v["email"];
    out.push_str("</div><div class=\"Box mb-4\"><div class=\"Box-header\"><h2 class=\"Box-title\">Email</h2></div>");
    out.push_str(&kv(
        "Notification emails",
        &if m["enabled"].as_bool() == Some(true) {
            let skipped = m["skipped_24h"].as_u64().unwrap_or(0);
            format!(
                "{} sent in the last 24 hours, at most {} · {}",
                m["sent_24h"],
                m["per_day"],
                if skipped == 0 {
                    "none skipped".to_string()
                } else {
                    format!(
                        "<span class=\"color-fg-attention\">{skipped} skipped over a daily limit</span>"
                    )
                }
            )
        } else {
            "off on this server: its operator did not set up SMTP".into()
        },
    ));
    out.push_str("</div>");
    if let Some(list) = v["archive"].as_array().filter(|l| !l.is_empty()) {
        out.push_str(
            "<div class=\"Box mb-4\" id=\"archive\"><div class=\"Box-header\"><h2 class=\"Box-title\">Archive of issues and pull requests</h2></div>",
        );
        for a in list {
            out.push_str(&kv(esc(a["repo"].as_str().unwrap_or("")), archive_row(a)));
        }
        out.push_str("</div>");
    }
    let worst = pass["worst_delay_seconds"].as_i64();
    let walk_delay = format!(
        "up to <strong>{}</strong> later{}",
        worst
            .map(minutes)
            .unwrap_or_else(|| minutes(pass["every_seconds"].as_i64().unwrap_or(600))),
        if worst.is_some() {
            " (the interval plus the duration of the last update)"
        } else {
            ""
        },
    );
    let now = if healthy {
        format!(
            "<p>A bot account watches nixpkgs, and GitHub notifies it of each issue and pull request that changes. \
             This server reads those notifications every {}, and reads each changed item again at once. \
             A review, push or merge on GitHub shows here in <strong>about a minute</strong>{}. \
             The full walk runs every {}, for anything the notifications miss.</p>",
            minutes(n["interval_seconds"].as_i64().unwrap_or(60)),
            n["average_delay_seconds"]
                .as_i64()
                .map(|d| format!(" ({} on average in the last hour)", minutes(d)))
                .unwrap_or_default(),
            minutes(n["walk_every_seconds"].as_i64().unwrap_or(1800))
        )
    } else if on {
        format!(
            "<p>A bot account watches nixpkgs, but the poll of its notifications fails now. \
             Until it works again, a review, push or merge on GitHub shows here after the next update: {walk_delay}.</p>"
        )
    } else {
        format!(
            "<p>This server walks the updates on GitHub. A review, push or merge shows here after the next update: \
             {walk_delay}. With the token of a bot account that watches nixpkgs, it shows in about a minute.</p>"
        )
    };
    let _ = write!(
        out,
        "<div class=\"Box\"><div class=\"Box-header\"><h2 class=\"Box-title\">How fast changes show here</h2></div><div class=\"Box-row\">\
         {now}\
         <p>The Events API keeps only the last 300 events, so a busy hour loses some events of the event log until \
         GH Archive brings them, about an hour later.</p>\
         <p>If the NixOS organisation forwarded the nixpkgs webhooks here (an organisation webhook, or a GitHub App \
         installed on the repository):</p>\
         <ul class=\"ml-4\">\
         <li>each change would show in <strong>seconds</strong>;</li>\
         <li>no event would be lost in a burst;</li>\
         <li>the updates would only check, so the rate limit would go to the older data and to the issue bodies.</li>\
         </ul>\
         <p class=\"color-fg-muted f6\">Maintainer Mode has no webhook receiver yet: GitHub sends webhooks only to the \
         administrators of a repository. See <a href=\"https://github.com/numtide/maintainermode/blob/main/docs/events.md\">docs/events.md</a>.</p>\
         </div></div>"
    );
    out
}

fn notifications_row(n: &Value) -> String {
    let mut parts = vec![format!(
        "the notifications of the bot account, read every {} · the last read {}",
        minutes(n["interval_seconds"].as_i64().unwrap_or(60)),
        when(&n["last_poll"])
    )];
    parts.push(format!(
        "{} threads in the last hour",
        n["threads_last_hour"]
    ));
    if let Some(d) = n["average_delay_seconds"].as_i64() {
        parts.push(format!(
            "{} on average from a change on GitHub to its read here",
            minutes(d)
        ));
    }
    match n["errors_last_hour"].as_u64().unwrap_or(0) {
        0 => parts.push("no errors in the last hour".into()),
        e => parts.push(format!(
            "<span class=\"color-fg-attention\">{e} errors in the last hour; the last: {}</span>",
            esc(n["last_error"]["message"].as_str().unwrap_or(""))
        )),
    }
    if n["healthy"].as_bool() != Some(true) {
        parts.push(
            "<span class=\"color-fg-attention\">not healthy: every update walks in full</span>"
                .into(),
        );
    }
    parts.join(" · ")
}
