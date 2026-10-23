//! What keeps the pages small and cheap to load again: the icon sprite,
//! style sheets under content-hash names, and `ETag`s on public pages.

use axum::Router;
use axum::extract::{Path, Request};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use sha2::Digest;
use std::collections::BTreeSet;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::web::Shared;

const PRIMER_CSS: &str = include_str!(concat!(env!("PRIMER_DIR"), "/primer.css"));
const CLUB_CSS: &str = include_str!("club.css");
const KEYS_JS: &str = include_str!("keys.js");
/// One line per icon: the name, a tab, then the inside of its `<svg>`.
const ICONS: &str = include_str!(concat!(env!("PRIMER_DIR"), "/icons.tsv"));

struct Asset {
    path: String,
    body: &'static str,
    mime: &'static str,
}

fn asset(name: &str, ext: &str, mime: &'static str, body: &'static str) -> Asset {
    let hash: String = sha2::Sha256::digest(body.as_bytes())
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect();
    Asset {
        path: format!("{name}.{hash}.{ext}"),
        body,
        mime,
    }
}

const CSS: &str = "text/css; charset=utf-8";

static PRIMER: LazyLock<Asset> = LazyLock::new(|| asset("primer", "css", CSS, PRIMER_CSS));
static CLUB: LazyLock<Asset> = LazyLock::new(|| asset("club", "css", CSS, CLUB_CSS));
static KEYS: LazyLock<Asset> =
    LazyLock::new(|| asset("keys", "js", "text/javascript; charset=utf-8", KEYS_JS));

/// The `<link>` tags of the two style sheets.
pub(crate) fn stylesheets() -> String {
    format!(
        "<link rel=\"stylesheet\" href=\"/static/{}\">\n<link rel=\"stylesheet\" href=\"/static/{}\">",
        PRIMER.path, CLUB.path
    )
}

/// The keyboard shortcuts of every page.
pub(crate) fn scripts() -> String {
    format!("<script src=\"/static/{}\" defer></script>", KEYS.path)
}

pub(crate) fn routes() -> Router<Shared> {
    Router::new().route("/static/:file", get(stylesheet))
}

/// A name carries the hash of its content, so a browser may keep it for
/// a year: new content gets a new name.
async fn stylesheet(Path(file): Path<String>) -> Response {
    match [&*PRIMER, &*CLUB, &*KEYS]
        .into_iter()
        .find(|a| a.path == file)
    {
        Some(a) => (
            [
                (header::CONTENT_TYPE, a.mime),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            ],
            a.body,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The hidden sprite with each icon the parts of a page use, for
/// `<use href="#i-…">`.
pub(crate) fn sprite(parts: &[&str]) -> String {
    let used: BTreeSet<&str> = parts
        .iter()
        .flat_map(|html| html.split("href=\"#i-").skip(1))
        .filter_map(|rest| rest.split_once('"').map(|(name, _)| name))
        .collect();
    if used.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" aria-hidden=\"true\" style=\"display:none\">",
    );
    for line in ICONS.lines() {
        if let Some((name, inner)) = line.split_once('\t') {
            if used.contains(name) {
                out.push_str(&format!(
                    "<symbol id=\"i-{name}\" viewBox=\"0 0 16 16\">{inner}</symbol>"
                ));
            }
        }
    }
    out.push_str("</svg>");
    out
}

/// Milliseconds of the last change to the data the public pages show.
static DATA_AT: AtomicI64 = AtomicI64::new(0);

/// Call after a change to the shared data, so public pages get a new tag.
pub(crate) fn touch() {
    let now = chrono::Utc::now().timestamp_millis();
    let _ = DATA_AT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |t| {
        Some(now.max(t + 1))
    });
}

/// These pages read live state that no `touch` follows: the task status,
/// the pull requests read from GitHub on demand, and the Hydra result on
/// the package and module pages.
fn untagged(path: &str, query: Option<&str>) -> bool {
    let status = path == "/state" && query.is_some_and(|q| q.split('&').any(|p| p == "tab=status"));
    status
        || path == "/me"
        || path == "/inbox"
        || [
            "/me/", "/sync", "/pr/", "/p/", "/m/", "/claude/", "/auth/", "/api/", "/static/",
        ]
        .iter()
        .any(|p| path.starts_with(p))
}

/// The data time, the cookies, which change what a page shows to a visitor
/// who did not sign in, and a five-minute slot, so the "N min ago" texts
/// stay close to the truth.
fn tag(headers: &HeaderMap) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for v in headers.get_all(header::COOKIE) {
        for b in v.as_bytes() {
            h = (h ^ u64::from(*b)).wrapping_mul(0x100000001b3);
        }
    }
    let slot = chrono::Utc::now().timestamp() / 300;
    format!("W/\"{:x}-{slot:x}-{h:x}\"", DATA_AT.load(Ordering::Relaxed))
}

/// A public page carries an `ETag`; a request that sends it back gets a
/// 304 and no body. A page for a person who signed in never does: it says
/// `no-store`.
pub(crate) async fn etag(req: Request, next: Next) -> Response {
    let get = matches!(*req.method(), Method::GET | Method::HEAD);
    let public = get
        && !untagged(req.uri().path(), req.uri().query())
        && crate::web::cookie(req.headers(), "session").is_none();
    if !public {
        return next.run(req).await;
    }
    let tag = tag(req.headers());
    let fresh = req
        .headers()
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|t| t.trim() == tag);
    if fresh {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, tag),
                (header::CACHE_CONTROL, "no-cache".into()),
            ],
        )
            .into_response();
    }
    let mut res = next.run(req).await;
    let h = res.headers();
    let html = h
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));
    if res.status() == StatusCode::OK
        && html
        && !h.contains_key(header::CACHE_CONTROL)
        && !h.contains_key(header::SET_COOKIE)
    {
        if let Ok(v) = HeaderValue::from_str(&tag) {
            res.headers_mut().insert(header::ETAG, v);
            res.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sprite_holds_each_used_icon_once() {
        let eye = crate::web::icon(crate::web::octicon!("eye"));
        let gear = crate::web::icon(crate::web::octicon!("gear"));
        let s = sprite(&[&eye, &format!("{eye}{gear}")]);
        assert_eq!(s.matches("<symbol").count(), 2);
        assert!(s.contains("id=\"i-eye\"") && s.contains("<path"));
        assert!(sprite(&["<p>no icon</p>"]).is_empty());
    }

    #[test]
    fn every_icon_has_a_symbol() {
        for line in ICONS.lines() {
            let (name, inner) = line.split_once('\t').unwrap();
            assert!(!name.is_empty() && inner.starts_with('<'), "{line}");
        }
    }

    #[test]
    fn a_stylesheet_name_changes_with_its_content() {
        assert_ne!(
            asset("x", "css", CSS, "a").path,
            asset("x", "css", CSS, "b").path
        );
        assert!(stylesheets().contains(&format!("/static/{}", CLUB.path)));
    }

    #[test]
    fn the_tag_follows_the_data_and_the_cookies() {
        let none = HeaderMap::new();
        let mut intro = HeaderMap::new();
        intro.insert(header::COOKIE, "intro=hidden".parse().unwrap());
        let before = tag(&none);
        assert_ne!(before, tag(&intro));
        touch();
        assert_ne!(before, tag(&none));
    }

    #[test]
    fn pages_that_ask_hydra_live_have_no_tag() {
        assert!(untagged("/p/cups", None) && untagged("/m/nixos/modules/a.nix", None));
        assert!(untagged("/state", Some("tab=status")) && !untagged("/state", Some("tab=rules")));
        assert!(!untagged("/packages", None) && !untagged("/review", None));
    }
}
