//! The JSON API: its index, the one list of pull requests, and people.
//! The other endpoints live next to the pages they mirror; `ENDPOINTS`
//! lists them all.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::web::{App, Shared};

pub(crate) const LIMIT: usize = 50;
pub(crate) const MAX_LIMIT: usize = 200;
pub(crate) const QUEUES: [&str; 5] = ["foryou", "unowned", "bumps", "all", "ready"];
pub(crate) const SECTIONS: [&str; 5] = [
    "ready",
    "maintainer_approved",
    "security",
    "backports",
    "waiting",
];

const ENDPOINTS: &[(&str, &str)] = &[
    (
        "/api/v1/prs",
        "open pull requests in one queue: queue=foryou|unowned|bumps|all|ready, section= (with queue=ready), login=, limit=, cursor=",
    ),
    ("/api/v1/prs/<number>", "one pull request"),
    (
        "/api/v1/people/<login>",
        "the public profile of a login: facts, open pull requests, review record, and what it maintains",
    ),
    (
        "/api/v1/needs",
        "what waits for you, the most urgent first, with your session only: mood=all|mine|machines|review|maintain",
    ),
    (
        "/api/v1/packages/index",
        "the public package list of the current index: pname and version of each attribute, gzip",
    ),
    (
        "/api/v1/packages/<attr>",
        "one package: health, issues and pull requests",
    ),
    ("/api/v1/modules/<file>", "one NixOS module"),
    ("/api/v1/areas/<path>", "one path of the nixpkgs tree"),
    (
        "/api/v1/find/<name>",
        "redirects a name to its package, module or area",
    ),
    ("/api/v1/metrics", "the state of nixpkgs, month by month"),
    ("/api/v1/status", "the counts and the rules of the queues"),
    (
        "/api/v1/sync",
        "what the server has read, and how fresh it is",
    ),
    (
        "/api/v1/events",
        "the event log, as Server-Sent Events: type=, number=, actor=, after=",
    ),
    (
        "/api/v1/archive/<owner>/<repo>/<number>",
        "one stored issue or pull request",
    ),
    (
        "POST /api/v1/reports",
        "upload a strict package report, with an upload token",
    ),
];

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/v1", get(index))
        .route("/api/v1/", get(index))
        .route("/api/v1/prs", get(api_prs))
        .route("/api/v1/people/:login", get(api_person))
}

async fn index() -> Json<Value> {
    let endpoints: Vec<Value> = ENDPOINTS
        .iter()
        .map(|(path, what)| json!({ "path": path, "description": what }))
        .collect();
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "paging": format!(
            "lists return at most limit items (default {LIMIT}, at most {MAX_LIMIT}); pass next_cursor as cursor for the next page"
        ),
        "endpoints": endpoints,
    }))
}

/// `cursor` is the offset of the first item. It is opaque to clients.
pub(crate) fn page<T: Serialize>(
    items: &[T],
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<Value, String> {
    let start = match cursor.map(str::trim).filter(|c| !c.is_empty()) {
        None => 0,
        Some(c) => c.parse::<usize>().map_err(|_| format!("bad cursor {c}"))?,
    };
    let limit = limit.unwrap_or(LIMIT).clamp(1, MAX_LIMIT);
    let end = start.saturating_add(limit).min(items.len());
    let slice = items.get(start..end).unwrap_or(&[]);
    Ok(json!({
        "total": items.len(),
        "items": slice,
        "next_cursor": (end < items.len()).then(|| end.to_string()),
    }))
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct PrQuery {
    pub queue: Option<String>,
    pub section: Option<String>,
    pub login: Option<String>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

pub(crate) struct Prs {
    pub value: Value,
    /// The caller's subscriptions shaped the answer.
    pub private: bool,
}

/// `caller` is the session or the MCP token. Subscriptions are private, so
/// they count only when `login` is the caller.
pub(crate) fn prs_value(app: &App, q: &PrQuery, caller: Option<&str>) -> Result<Prs, String> {
    let queue = q.queue.as_deref().unwrap_or("all");
    if !QUEUES.contains(&queue) {
        return Err(format!(
            "no queue {queue}; use one of {}",
            QUEUES.join(", ")
        ));
    }
    let login = q
        .login
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .or(caller.filter(|_| queue == "foryou"));
    let own = login.is_some() && login == caller;
    let s = app.snap();
    let mut out;
    let mut private = false;
    match queue {
        "ready" => {
            let section = q.section.as_deref().unwrap_or("ready");
            let b = &s.board;
            out = match section {
                "ready" => page(&s.queues.merge, q.limit, q.cursor.as_deref()),
                "maintainer_approved" => page(&b.maintainer_approved, q.limit, q.cursor.as_deref()),
                "security" => page(&b.security, q.limit, q.cursor.as_deref()),
                "backports" => page(&b.backports, q.limit, q.cursor.as_deref()),
                "waiting" => page(&b.waiting, q.limit, q.cursor.as_deref()),
                _ => {
                    return Err(format!(
                        "no section {section}; use one of {}",
                        SECTIONS.join(", ")
                    ));
                }
            }?;
            out["section"] = json!(section);
        }
        "bumps" => {
            let h = app.health.read().unwrap().clone();
            let index = app.index.read().unwrap();
            let mut rows = crate::bumps::open(&s, &index, &h);
            if let Some(l) = login {
                rows = crate::bumps::mine(app, &index, l, own, rows);
                private = own;
            }
            let items: Vec<Value> = rows.iter().map(crate::bumps::row_json).collect();
            out = page(&items, q.limit, q.cursor.as_deref())?;
            out["median_age_days"] =
                json!(crate::bumps::median_age_days(&rows, chrono::Utc::now()));
            out["login"] = json!(login);
        }
        _ => {
            if queue == "foryou" && login.is_none() {
                return Err("queue=foryou needs login, or a session".into());
            }
            private = own;
            let watched = crate::web::watched_of(app, login.filter(|_| own));
            let entries =
                crate::web::review_bucket(&s, login, own.then_some(watched.as_slice()), queue);
            out = page(&entries, q.limit, q.cursor.as_deref())?;
            out["login"] = json!(login);
        }
    }
    out["queue"] = json!(queue);
    Ok(Prs {
        value: out,
        private,
    })
}

async fn api_prs(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<PrQuery>,
) -> Response {
    let viewer = app.viewer(&headers);
    match prs_value(&app, &q, viewer.as_deref()) {
        Ok(p) => {
            let cache = if p.private {
                "private, no-store"
            } else {
                "no-cache"
            };
            ([(header::CACHE_CONTROL, cache)], Json(p.value)).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e }))).into_response(),
    }
}

/// Public data only: the same answer for every caller.
pub(crate) fn person_value(app: &App, login: &str) -> Value {
    let s = app.snap();
    let person = s
        .ledger
        .people
        .get(login)
        .cloned()
        .unwrap_or(crate::ledger::Person {
            login: login.to_string(),
            ..Default::default()
        });
    let credits: Vec<_> = s.ledger.credits_of(login).cloned().collect();
    let dossier = crate::dossier::build(&s.pr_data, login);
    let area = s.areas.get(login).map(crate::area::Area::public);
    drop(s);
    let mut maintains = crate::catalog::maintains_value(app, login);
    if let Some(m) = maintains.as_object_mut() {
        m.remove("login");
    }
    let profile = crate::profile::profile_value(app, login);
    json!({
        "login": login,
        "facts": profile["facts"],
        "open_prs": profile["open_prs"],
        "recent_prs": profile["recent_prs"],
        "areas": profile["areas"],
        "person": person,
        "dossier": dossier,
        "credits": credits,
        "area": area,
        "maintains": maintains,
    })
}

async fn api_person(State(app): State<Shared>, Path(login): Path<String>) -> Json<Value> {
    Json(person_value(&app, &login))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_carries_the_cursor_of_the_next() {
        let items: Vec<u32> = (0..120).collect();
        let p = page(&items, None, None).unwrap();
        assert_eq!(p["total"], 120);
        assert_eq!(p["items"].as_array().unwrap().len(), LIMIT);
        assert_eq!(p["next_cursor"], "50");
        let p = page(&items, Some(100), Some("50")).unwrap();
        assert_eq!(p["items"][0], 50);
        assert_eq!(p["items"].as_array().unwrap().len(), 70);
        assert!(p["next_cursor"].is_null());
        let p = page(&items, Some(10), Some("500")).unwrap();
        assert_eq!(p["items"].as_array().unwrap().len(), 0);
        assert!(page(&items, None, Some("x")).is_err());
        assert_eq!(
            page(&(0..300).collect::<Vec<u32>>(), Some(9999), None).unwrap()["items"]
                .as_array()
                .unwrap()
                .len(),
            MAX_LIMIT
        );
    }

    #[test]
    fn foryou_needs_a_login_and_an_unknown_queue_fails() {
        let app = crate::sets::tests::app(&[]);
        let q = PrQuery {
            queue: Some("foryou".into()),
            ..Default::default()
        };
        assert!(prs_value(&app, &q, None).is_err());
        let p = prs_value(&app, &q, Some("alice")).unwrap();
        assert!(p.private);
        assert_eq!(p.value["items"], json!([]));
        let q = PrQuery {
            queue: Some("foryou".into()),
            login: Some("bob".into()),
            ..Default::default()
        };
        assert!(!prs_value(&app, &q, Some("alice")).unwrap().private);
        let q = PrQuery {
            queue: Some("bumps".into()),
            ..Default::default()
        };
        let p = prs_value(&app, &q, Some("alice")).unwrap();
        assert!(!p.private);
        assert!(p.value["login"].is_null());
        let q = PrQuery {
            queue: Some("bumps".into()),
            login: Some("alice".into()),
            ..Default::default()
        };
        assert!(prs_value(&app, &q, Some("alice")).unwrap().private);
        let q = PrQuery {
            queue: Some("nope".into()),
            ..Default::default()
        };
        assert!(prs_value(&app, &q, None).is_err());
        let q = PrQuery {
            queue: Some("ready".into()),
            section: Some("security".into()),
            ..Default::default()
        };
        assert_eq!(
            prs_value(&app, &q, None).unwrap().value["section"],
            "security"
        );
    }

    #[test]
    fn the_public_record_has_no_tags() {
        let app = crate::sets::tests::app(&[]);
        app.store
            .set_tags("alice", &["pkgs/secret".into()])
            .unwrap();
        app.recompute().unwrap();
        let v = person_value(&app, "alice");
        assert!(v["area"].is_object());
        assert!(!v.to_string().contains("pkgs/secret"));
    }
}
