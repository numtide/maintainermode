//! What keeps the pages small and cheap to load again.

use axum::Router;
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use sha2::Digest;
use std::collections::BTreeSet;
use std::sync::LazyLock;

use crate::web::Shared;

const PRIMER_CSS: &str = include_str!(concat!(env!("PRIMER_DIR"), "/primer.css"));
const CLUB_CSS: &str = include_str!("club.css");
/// One line per icon: the name, a tab, then the inside of its `<svg>`.
const ICONS: &str = include_str!(concat!(env!("PRIMER_DIR"), "/icons.tsv"));

struct Asset {
    path: String,
    body: &'static str,
}

fn asset(name: &str, body: &'static str) -> Asset {
    let hash: String = sha2::Sha256::digest(body.as_bytes())
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect();
    Asset {
        path: format!("{name}.{hash}.css"),
        body,
    }
}

static PRIMER: LazyLock<Asset> = LazyLock::new(|| asset("primer", PRIMER_CSS));
static CLUB: LazyLock<Asset> = LazyLock::new(|| asset("club", CLUB_CSS));

/// The `<link>` tags of the two style sheets.
pub(crate) fn stylesheets() -> String {
    format!(
        "<link rel=\"stylesheet\" href=\"/static/{}\">\n<link rel=\"stylesheet\" href=\"/static/{}\">",
        PRIMER.path, CLUB.path
    )
}

pub(crate) fn routes() -> Router<Shared> {
    Router::new().route("/static/:file", get(stylesheet))
}

/// A name carries the hash of its content, so a browser may keep it for
/// a year: new content gets a new name.
async fn stylesheet(Path(file): Path<String>) -> Response {
    match [&*PRIMER, &*CLUB].into_iter().find(|a| a.path == file) {
        Some(a) => (
            [
                (header::CONTENT_TYPE, "text/css; charset=utf-8"),
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
        assert_ne!(asset("x", "a").path, asset("x", "b").path);
        assert!(stylesheets().contains(&format!("/static/{}", CLUB.path)));
    }
}
