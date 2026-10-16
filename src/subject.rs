//! What a nixpkgs issue body names: paths, and the "Package name" field of
//! the issue forms in `.github/ISSUE_TEMPLATE`.

const PATH_PREFIXES: [&str; 9] = [
    "pkgs/",
    "nixos/modules/",
    "nixos/tests/",
    "nixos/lib/",
    "nixos/doc/",
    "lib/",
    "doc/",
    "ci/",
    "maintainers/",
];

const MAX_MENTIONS: usize = 20;

fn clean(word: &str) -> Option<String> {
    let w = word.trim_matches(|c: char| "`'\"()[]{},;.".contains(c));
    let ok = !w.is_empty()
        && w.len() <= 100
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.+/".contains(c));
    ok.then(|| w.to_string())
}

fn starts_path(before: &str) -> bool {
    match before.chars().last() {
        None => true,
        Some(c) if c.is_whitespace() || "`'\"([<".contains(c) => true,
        Some('/') => {
            let mut parts = before.rsplit('/');
            parts.next();
            parts.next();
            matches!(parts.next(), Some("blob" | "tree"))
        }
        _ => false,
    }
}

/// nixpkgs paths in a body, and the value of the update form's
/// "Package name" field.
pub fn of_body(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut lines = body.lines();
    while let Some(l) = lines.next() {
        if l.trim() == "### Package name" {
            if let Some(v) = lines
                .by_ref()
                .map(str::trim)
                .find(|v| !v.is_empty())
                .and_then(|v| v.split_whitespace().next())
                .and_then(clean)
            {
                out.push(v);
            }
        }
    }
    for (i, _) in body.char_indices() {
        if out.len() >= MAX_MENTIONS {
            break;
        }
        let rest = &body[i..];
        if !PATH_PREFIXES.iter().any(|p| rest.starts_with(p)) || !starts_path(&body[..i]) {
            continue;
        }
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || "-_./+".contains(c)))
            .unwrap_or(rest.len());
        let path = rest[..end].trim_end_matches(['.', '/']);
        if path.contains('/') && !out.iter().any(|o| o == path) {
            out.push(path.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_name_paths_and_the_form_field() {
        let body = "### Package name\n\nhello\n\nSee pkgs/by-name/he/hello/package.nix and \
                    https://github.com/NixOS/nixpkgs/blob/master/nixos/modules/services/x.nix.\n\
                    Not /usr/lib/foo nor glib/x.";
        assert_eq!(
            of_body(body),
            [
                "hello",
                "pkgs/by-name/he/hello/package.nix",
                "nixos/modules/services/x.nix"
            ]
        );
    }
}
