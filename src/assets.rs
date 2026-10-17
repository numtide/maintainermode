//! What keeps the pages small and cheap to load again.

use std::collections::BTreeSet;

/// One line per icon: the name, a tab, then the inside of its `<svg>`.
const ICONS: &str = include_str!(concat!(env!("PRIMER_DIR"), "/icons.tsv"));

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
}
