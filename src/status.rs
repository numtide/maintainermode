//! What the server knows, how fresh it is, and what a forwarded webhook would
//! change. Everything here is read from the database and the snapshot; the
//! page asks no remote service.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::html::{Html, html};
use crate::ui::kv;
use crate::web::{App, ago};

fn time(app: &App, key: &str) -> Option<DateTime<Utc>> {
    app.store.get_time(key).ok().flatten()
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
        "pull_requests": { "stored": s.prs },
        "issues": { "stored": s.issues.len() },
        "history": { "done": app.history_complete() },
        "rows_from_archive_at": time(app, crate::archive::PROJECTED),
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
        "notifications": app.notifications.json(Utc::now()),
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

fn archive_row(a: &Value) -> Html {
    let mut parts = vec![html!(
        "{} issues and pull requests stored ({:.1} MB)",
        a["items"],
        a["bytes"].as_f64().unwrap_or(0.0) / 1e6
    )];
    match (a["top"].as_i64(), a["oldest"].as_i64()) {
        (None, _) => parts.push("waiting for the first listing".into()),
        (Some(_), None) => parts.push("recent updates first".into()),
        (Some(_), Some(o)) => parts.push(html!("oldest number reached #{o}")),
    }
    if a["open_complete"].as_bool() == Some(false) {
        parts.push(if a["open_listed"].as_bool() == Some(true) {
            "open items listed, not all stored yet".into()
        } else {
            "listing the open items".into()
        });
    }
    if let Some(p) = a["percent"].as_f64() {
        parts.push(html!("{p:.1} % of the repository"));
    }
    if a["complete"].as_bool() == Some(true) {
        parts.push("<span class=\"color-fg-success\">every number read</span>".into());
    } else if let Some(h) = a["hours_left"].as_f64() {
        parts.push(html!("{} left at the current pace", hours(h)));
    }
    parts.push(html!(
        "{} due · {} per run, a run every {} · the last ended {}",
        a["due"],
        a["items_per_pass"],
        minutes(crate::archive::PASS_EVERY as i64),
        when(&a["last_pass"])
    ));
    if let Some(n) = a["failed"].as_i64().filter(|n| *n > 0) {
        parts.push(html!(
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
        parts.push(html!(
            "projection check: {} of {} rows differ from the sync{} ({} not comparable), {}",
            s["differ"],
            s["compared"],
            if fields.is_empty() {
                String::new()
            } else {
                format!(" in {}", fields.join(", "))
            },
            s["skipped"],
            when(&s["at"])
        ));
    }
    if let Some(e) = a["last_error"].as_str() {
        parts.push(html!(
            "<span class=\"color-fg-attention\">last error: {e}</span>"
        ));
    }
    crate::html::join(parts, " · ")
}

fn when(v: &Value) -> String {
    v.as_str()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| format!("{} ago", ago(t.with_timezone(&Utc))))
        .unwrap_or_else(|| "not yet".into())
}

fn done(v: &Value, yes: &str, no: &str) -> Html {
    if v.as_bool() == Some(true) {
        html!("<span class=\"color-fg-success\">{yes}</span>")
    } else {
        html!("<span class=\"color-fg-attention\">{no}</span>")
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

fn source(run: &Value, what: &str) -> Html {
    let Some(at) = run.get("at") else {
        return html!("{what} · not run yet");
    };
    let line = run["line"].as_str().unwrap_or("");
    html!(
        "{what} · read {} · {}",
        when(at),
        if run["ok"].as_bool() == Some(true) {
            Html::text(line)
        } else {
            html!("<span class=\"color-fg-attention\">{line}</span>")
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
pub(crate) fn body(app: &App) -> Html {
    let v = state(app);
    let mut out = Html::new();
    let p = &v["pull_requests"];
    let i = &v["issues"];
    let x = &v["index"];
    let h = &v["health"];
    let e = &v["events"];
    let pass = &v["passes"];
    out.push(&Html::from(
        "<div class=\"Box mb-4\"><div class=\"Box-header\"><h2 class=\"Box-title\">GitHub</h2></div>",
    ));
    out.push(&kv(
        "Pull requests",
        html!(
            "{} stored: the open ones and those updated in the last {} days, from the archive",
            p["stored"],
            crate::ledger::WINDOW_DAYS
        ),
    ));
    out.push(&kv(
        "Issues",
        html!(
            "{} stored: the open ones and those updated in the last {} days, from the archive",
            i["stored"],
            crate::ledger::WINDOW_DAYS
        ),
    ));
    let l = &v["live"];
    out.push(&kv(
        "Mergeable and checks",
        html!(
            "{} of {} open pull requests checked · {} per upkeep run, the oldest check first · oldest check {}",
            l["checked"],
            l["open_pull_requests"],
            l["per_pass"],
            when(&l["oldest_check"])
        ),
    ));
    out.push(&kv(
        "History since 2020",
        done(
            &v["history"]["done"],
            "complete",
            "in progress: the archive walks back through history, and the numbers on the state of nixpkgs grow while it reads",
        ),
    ));
    out.push(&kv(
        "Tokens",
        html!(
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
    out.push(&kv(
        "Upkeep",
        html!(
            "the index, the expiry of old data and the live refresh, one run every {} · the last took {} and ended {}",
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
    out.push(&kv(
        "Notifications",
        if on {
            notifications_row(n)
        } else {
            "off: this server has no token of a bot account, so a change waits for the next archive pass"
                .into()
        },
    ));
    out.push(&Html::from(
        "</div><div class=\"Box mb-4\"><div class=\"Box-header\"><h2 class=\"Box-title\">nixpkgs and package health</h2></div>",
    ));
    out.push(&kv(
        "Index",
        html!(
            "<code>{}</code> · {} packages, {} NixOS modules, {} teams · built {}",
            x["rev"]
                .as_str()
                .unwrap_or("")
                .get(..12)
                .unwrap_or("not built yet"),
            x["packages"],
            x["modules"],
            x["teams"],
            when(&x["at"])
        ),
    ));
    for c in v["channels"]["list"].as_array().into_iter().flatten() {
        out.push(&kv(
            html!("Channel {}", c["channel"].as_str().unwrap_or("")),
            match c["rev"].as_str() {
                Some(rev) => html!(
                    "<code>{}</code> · {} attributes · this revision first seen {}",
                    rev.get(..12).unwrap_or(rev),
                    c["attributes"],
                    when(&c["read_at"])
                ),
                None => "not read yet: the channels wait for the first master index".into(),
            },
        ));
    }
    out.push(&kv(
        "Upstream versions",
        html!(
            "repology, read {} · {} attributes behind upstream",
            when(&h["repology_at"]),
            h["outdated"]
        ),
    ));
    out.push(&kv(
        "Advisories",
        html!(
            "security tracker, read {} · {} attributes with advisories",
            when(&h["tracker_at"]),
            h["attributes_with_advisories"]
        ),
    ));
    let pk = &v["packaging"];
    out.push(&kv(
        "Packaging hygiene",
        if pk["checked"].as_u64().unwrap_or(0) == 0 {
            "from the nixpkgs index, with no network access · the next index reads it".into()
        } else {
            html!(
                "from the nixpkgs index, with no network access · {} of {} packages have a gap in meta or no versionCheckHook",
                pk["with_gaps"],
                pk["checked"]
            )
        },
    ));
    for (key, label, what) in SOURCES {
        out.push(&kv(label, source(&h["sources"][key], what)));
    }
    out.push(&kv(
        "Health task",
        html!(
            "its own task, one run every {}: a slow source does not delay the archive or the upkeep",
            minutes(h["every_seconds"].as_i64().unwrap_or(600))
        ),
    ));
    let hy = &v["hydra"];
    out.push(&kv(
        "Hydra builds",
        html!(
            "asked when a package or module page is shown, at most {} at a time and {} a minute, kept one hour · \
             {} jobs kept · {} page views not asked since the start",
            hy["concurrent"],
            hy["per_minute"],
            hy["cached"],
            hy["not_asked"]
        ),
    ));
    let b = &v["hydra_builds"];
    out.push(&kv(
        "Hydra build watch",
        html!(
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
    out.push(&kv(
        "Flake rescans",
        if r["enabled"].as_bool() == Some(true) {
            html!(
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
    out.push(&Html::from(
        "</div><div class=\"Box mb-4\"><div class=\"Box-header\"><h2 class=\"Box-title\">Event log</h2></div>",
    ));
    out.push(&kv(
        "Events",
        html!(
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
    out.push(&Html::from(
        "</div><div class=\"Box mb-4\"><div class=\"Box-header\"><h2 class=\"Box-title\">Email</h2></div>",
    ));
    out.push(&kv(
        "Notification emails",
        if m["enabled"].as_bool() == Some(true) {
            let skipped = m["skipped_24h"].as_u64().unwrap_or(0);
            html!(
                "{} sent in the last 24 hours, at most {} · {}",
                m["sent_24h"],
                m["per_day"],
                if skipped == 0 {
                    Html::from("none skipped")
                } else {
                    html!(
                        "<span class=\"color-fg-attention\">{skipped} skipped over a daily limit</span>"
                    )
                }
            )
        } else {
            "off on this server: its operator did not set up SMTP".into()
        },
    ));
    out.push(&Html::from("</div>"));
    if let Some(list) = v["archive"].as_array().filter(|l| !l.is_empty()) {
        out.push(&Html::from(
            "<div class=\"Box mb-4\" id=\"archive\"><div class=\"Box-header\"><h2 class=\"Box-title\">Archive of issues and pull requests</h2></div>",
        ));
        for a in list {
            out.push(&kv(
                Html::text(a["repo"].as_str().unwrap_or("")),
                archive_row(a),
            ));
        }
        out.push(&Html::from("</div>"));
    }
    let walk_delay = html!(
        "up to <strong>{}</strong> later (the archive lists the updates on each pass)",
        minutes(crate::archive::PASS_EVERY as i64)
    );
    let now = if healthy {
        html!(
            "<p>A bot account watches nixpkgs, and GitHub notifies it of each issue and pull request that changes. \
             This server reads those notifications every {}, and the archive reads each changed item again within seconds. \
             A review, push or merge on GitHub shows here in <strong>about a minute</strong>{}. \
             Each archive pass also lists the updates, every {}, for anything the notifications miss.</p>",
            minutes(n["interval_seconds"].as_i64().unwrap_or(60)),
            n["average_delay_seconds"]
                .as_i64()
                .map(|d| format!(" ({} on average in the last hour)", minutes(d)))
                .unwrap_or_default(),
            minutes(crate::archive::PASS_EVERY as i64)
        )
    } else if on {
        html!(
            "<p>A bot account watches nixpkgs, but the poll of its notifications fails now. \
             Until it works again, a review, push or merge on GitHub shows here {walk_delay}.</p>"
        )
    } else {
        html!(
            "<p>The archive lists the updates on GitHub. A review, push or merge shows here \
             {walk_delay}. With the token of a bot account that watches nixpkgs, it shows in about a minute.</p>"
        )
    };
    out.push(&html!(
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
    ));
    out
}

fn notifications_row(n: &Value) -> Html {
    let mut parts = vec![html!(
        "the notifications of the bot account, read every {} · the last read {}",
        minutes(n["interval_seconds"].as_i64().unwrap_or(60)),
        when(&n["last_poll"])
    )];
    parts.push(html!("{} threads in the last hour", n["threads_last_hour"]));
    if let Some(d) = n["average_delay_seconds"].as_i64() {
        parts.push(html!(
            "{} on average from a change on GitHub to its mark here",
            minutes(d)
        ));
    }
    match n["errors_last_hour"].as_u64().unwrap_or(0) {
        0 => parts.push("no errors in the last hour".into()),
        e => parts.push(html!(
            "<span class=\"color-fg-attention\">{e} errors in the last hour; the last: {}</span>",
            n["last_error"]["message"].as_str().unwrap_or("")
        )),
    }
    if n["healthy"].as_bool() != Some(true) {
        parts.push(
            "<span class=\"color-fg-attention\">not healthy: a change waits for the next archive pass</span>"
                .into(),
        );
    }
    crate::html::join(parts, " · ")
}
