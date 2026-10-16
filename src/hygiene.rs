//! Packaging hygiene: cheap checks on what the index sweep already reads.
//! No network, no build.

use crate::area::{Package, under};

/// The directories where a top-level package is most likely a program, with
/// a binary for `meta.mainProgram` and `versionCheckHook` to name.
const PROGRAM_DIRS: [&str; 5] = [
    "pkgs/by-name",
    "pkgs/applications",
    "pkgs/tools",
    "pkgs/servers",
    "pkgs/games",
];

/// A guess from the attribute and its file: the sweep does not build, so it
/// cannot see `bin/`.
pub fn program(attr: &str, p: &Package) -> bool {
    !attr.contains('.')
        && !p.pname.starts_with("lib")
        && p.file
            .as_deref()
            .is_some_and(|f| PROGRAM_DIRS.iter().any(|d| under(f, d)))
}

/// What the package lacks, in the words of the nixpkgs manual. `None` when
/// the index predates these facts.
pub fn gaps(attr: &str, p: &Package) -> Option<Vec<&'static str>> {
    let f = p.facts.as_ref()?;
    let mut out = Vec::new();
    match f.description.as_deref() {
        None => out.push("no meta.description"),
        Some(d) => {
            if d.trim_end().ends_with('.') {
                out.push("meta.description ends with a period");
            }
            let first = d.split_whitespace().next().unwrap_or("");
            if ["A", "An", "The"].contains(&first) {
                out.push("meta.description starts with an article");
            }
        }
    }
    if !f.homepage {
        out.push("no meta.homepage");
    }
    if !f.license {
        out.push("no meta.license");
    }
    if program(attr, p) {
        if !f.main_program {
            out.push("no meta.mainProgram");
        }
        if !f.version_check {
            out.push("no versionCheckHook");
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attrs::Facts;

    fn pkg(file: &str, facts: Facts) -> Package {
        Package {
            pname: "hello".into(),
            file: Some(file.into()),
            facts: Some(facts),
            ..Default::default()
        }
    }

    #[test]
    fn a_complete_program_has_no_gap() {
        let p = pkg(
            "pkgs/by-name/he/hello/package.nix",
            Facts {
                description: Some("Program that produces a familiar, friendly greeting".into()),
                homepage: true,
                license: true,
                main_program: true,
                version_check: true,
                ..Default::default()
            },
        );
        assert_eq!(gaps("hello", &p), Some(vec![]));
    }

    #[test]
    fn a_library_needs_no_main_program() {
        let p = pkg(
            "pkgs/development/libraries/hello/default.nix",
            Facts {
                description: Some("A greeting library.".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            gaps("hello", &p),
            Some(vec![
                "meta.description ends with a period",
                "meta.description starts with an article",
                "no meta.homepage",
                "no meta.license",
            ])
        );
        assert!(!program("python3Packages.hello", &p));
    }

    #[test]
    fn an_old_index_has_no_verdict() {
        assert_eq!(gaps("hello", &Package::default()), None);
    }
}
