//! What a nixpkgs title or issue body is about, by the conventions of the
//! repository: `PACKAGENAME: …`, `nixos/MODULENAME: …`, and the issue forms
//! in `.github/ISSUE_TEMPLATE`.

const FORM_PREFIXES: [&str; 6] = [
    "build failure:",
    "update request:",
    "unreproducible package:",
    "package request:",
    "module request:",
    "broken package:",
];

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

/// A nixpkgs path, as opposed to a name like `nixos/nginx`.
pub fn is_path(s: &str) -> bool {
    PATH_PREFIXES.iter().any(|p| s.starts_with(p))
}

fn clean(word: &str) -> Option<String> {
    let w = word.trim_matches(|c: char| "`'\"()[]{},;.".contains(c));
    let ok = !w.is_empty()
        && w.len() <= 100
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.+/".contains(c));
    ok.then(|| w.to_string())
}

/// The names a title is about: `hello` for `hello: 2.12 -> 2.13`,
/// `nixos/nginx` for a module, and `foo`, `bar` for `foo, bar: …`.
pub fn of_title(title: &str) -> Vec<String> {
    let t = title.trim();
    let lower = t.to_lowercase();
    for p in FORM_PREFIXES {
        if lower.starts_with(p) {
            return t
                .get(p.len()..)
                .unwrap_or("")
                .split_whitespace()
                .next()
                .and_then(clean)
                .into_iter()
                .collect();
        }
    }
    let Some((head, _)) = t.split_once(": ") else {
        return Vec::new();
    };
    if head.contains(char::is_whitespace) && !head.contains(", ") {
        return Vec::new();
    }
    head.split(", ").filter_map(clean).collect()
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
        if !is_path(rest) || !starts_path(&body[..i]) {
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
    fn titles_follow_the_nixpkgs_conventions() {
        assert_eq!(of_title("hello: 2.12 -> 2.13"), ["hello"]);
        assert_eq!(of_title("nixos/nginx: add an option"), ["nixos/nginx"]);
        assert_eq!(
            of_title("python3Packages.requests: fix tests"),
            ["python3Packages.requests"]
        );
        assert_eq!(of_title("Build failure: hello"), ["hello"]);
        assert_eq!(of_title("Update Request: hello 2.12 → 2.13"), ["hello"]);
        assert_eq!(of_title("foo, bar: drop"), ["foo", "bar"]);
        assert!(of_title("Some general question: how?").is_empty());
        assert!(of_title("no colon at all").is_empty());
    }

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
