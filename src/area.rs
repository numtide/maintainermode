//! Who knows which part of nixpkgs. Three derived sources, unioned:
//! `meta.maintainers` read from the nixpkgs source, `ci/OWNERS`, and the
//! directories a person changed in merged pull requests over the last year.
//! Opt-in tags go on top.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Read;

use crate::model::Pr;
use crate::store::Member;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Index {
    pub rev: String,
    /// login → paths whose `meta.maintainers` names them.
    pub maintainers: BTreeMap<String, BTreeSet<String>>,
    /// path prefix → logins, from `ci/OWNERS`.
    pub owners: Vec<(String, Vec<String>)>,
}

/// The area a file's maintainers cover: the package directory for a
/// `default.nix` or `package.nix`, the file itself otherwise, because a
/// NixOS module directory holds many unrelated modules.
fn area_key(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, "default.nix" | "package.nix")) => dir.to_string(),
        _ => path.to_string(),
    }
}

fn under(path: &str, prefix: &str) -> bool {
    !prefix.is_empty()
        && path.starts_with(prefix)
        && (path.len() == prefix.len() || path.as_bytes()[prefix.len()] == b'/')
}

/// handle → GitHub login, from `maintainers/maintainer-list.nix`.
fn parse_maintainer_list(src: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut handle: Option<String> = None;
    for line in src.lines() {
        if let Some(rest) = line.strip_prefix("  ") {
            if !rest.starts_with(' ') {
                if let Some((h, tail)) = rest.split_once(" = ") {
                    if tail.trim_start().starts_with('{') {
                        handle = Some(h.trim_matches('"').to_string());
                        continue;
                    }
                }
            }
        }
        let t = line.trim();
        if let (Some(h), Some(v)) = (&handle, t.strip_prefix("github = \"")) {
            if let Some(login) = v.strip_suffix("\";") {
                out.insert(h.clone(), login.to_lowercase());
            }
        }
    }
    out
}

/// Handles named in `maintainers = …;`. Reads the common spellings
/// (`with lib.maintainers; [ a b ]`, `[ lib.maintainers.a ]`); a team or a
/// computed list is skipped, so an area can be missing but never invented.
fn parse_maintainers(src: &str, handles: &HashMap<String, String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = src;
    while let Some(i) = rest.find("maintainers") {
        rest = &rest[i + "maintainers".len()..];
        let Some(after) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let mut body = after.trim_start();
        if body.starts_with("with ") {
            body = body.split_once(';').map_or("", |(_, b)| b).trim_start();
        }
        let Some(list) = body.strip_prefix('[') else {
            continue;
        };
        let Some((list, _)) = list.split_once(']') else {
            continue;
        };
        for tok in list.split_whitespace() {
            let h = tok
                .trim_start_matches("lib.maintainers.")
                .trim_start_matches("maintainers.");
            if let Some(login) = handles.get(h) {
                out.insert(login.clone());
            }
        }
    }
    out
}

fn parse_owners(src: &str) -> Vec<(String, Vec<String>)> {
    src.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let pattern = parts.next()?;
            let prefix = pattern
                .trim_start_matches('/')
                .split('*')
                .next()?
                .trim_end_matches('/')
                .to_string();
            let logins: Vec<String> = parts
                .filter_map(|p| p.strip_prefix('@'))
                .filter(|p| !p.contains('/'))
                .map(str::to_lowercase)
                .collect();
            (!prefix.is_empty() && !logins.is_empty()).then_some((prefix, logins))
        })
        .collect()
}

/// Builds the index from a nixpkgs source tarball (`.tar.gz`).
pub fn from_tarball(rev: &str, bytes: &[u8]) -> Result<Index> {
    let mut files: Vec<(String, String)> = Vec::new();
    let mut list = String::new();
    let mut owners = String::new();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().to_string();
        let Some((_, rel)) = path.split_once('/') else {
            continue;
        };
        let rel = rel.to_string();
        let wanted = rel == "maintainers/maintainer-list.nix"
            || rel == "ci/OWNERS"
            || (rel.ends_with(".nix") && (rel.starts_with("pkgs/") || rel.starts_with("nixos/")));
        if !wanted {
            continue;
        }
        let mut s = String::new();
        if entry.read_to_string(&mut s).is_err() {
            continue;
        }
        match rel.as_str() {
            "maintainers/maintainer-list.nix" => list = s,
            "ci/OWNERS" => owners = s,
            _ if s.contains("maintainers") => files.push((rel, s)),
            _ => {}
        }
    }
    anyhow::ensure!(!list.is_empty(), "no maintainer-list.nix in the tarball");
    let handles = parse_maintainer_list(&list);
    let mut maintainers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (path, src) in &files {
        for login in parse_maintainers(src, &handles) {
            maintainers.entry(login).or_default().insert(area_key(path));
        }
    }
    Ok(Index {
        rev: rev.to_string(),
        maintainers,
        owners: parse_owners(&owners),
    })
}

pub async fn fetch(rev: &str) -> Result<Index> {
    let url = format!("https://codeload.github.com/NixOS/nixpkgs/tar.gz/{rev}");
    let bytes = crate::github::http()
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await
        .context("nixpkgs tarball")?;
    let rev = rev.to_string();
    tokio::task::spawn_blocking(move || from_tarball(&rev, &bytes)).await?
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Area {
    pub maintains: Vec<String>,
    pub owns: Vec<String>,
    pub committed: Vec<String>,
    pub tags: Vec<String>,
}

impl Area {
    pub fn of(member: &Member, index: &Index) -> Self {
        let login = member.login.to_lowercase();
        Area {
            maintains: index
                .maintainers
                .get(&login)
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default(),
            owns: index
                .owners
                .iter()
                .filter(|(_, l)| l.contains(&login))
                .map(|(p, _)| p.clone())
                .collect(),
            committed: member.committed.clone(),
            tags: member.tags.clone(),
        }
    }

    fn prefixes(&self) -> impl Iterator<Item = &String> {
        self.maintains
            .iter()
            .chain(&self.owns)
            .chain(&self.committed)
            .chain(&self.tags)
    }

    pub fn covers(&self, files: &[String]) -> bool {
        files.iter().any(|f| self.prefixes().any(|p| under(f, p)))
    }
}

/// Paths with a maintainer or an owner, flattened for lookups by ancestor.
pub struct Ownership {
    known: bool,
    paths: HashSet<String>,
    owner_prefixes: Vec<String>,
}

impl Ownership {
    pub fn new(index: &Index) -> Self {
        Ownership {
            known: !index.rev.is_empty(),
            paths: index.maintainers.values().flatten().cloned().collect(),
            owner_prefixes: index.owners.iter().map(|(p, _)| p.clone()).collect(),
        }
    }

    /// No maintainer and no owner for any file the pull request touches. This
    /// reads public nixpkgs data only, so the ledger may use it.
    pub fn orphan(&self, pr: &Pr) -> bool {
        if !self.known || pr.files.is_empty() {
            return false;
        }
        !pr.files.iter().any(|f| {
            let mut p = f.as_str();
            loop {
                if self.paths.contains(p) {
                    return true;
                }
                match p.rsplit_once('/') {
                    Some((parent, _)) => p = parent,
                    None => break,
                }
            }
            self.owner_prefixes.iter().any(|o| under(f, o))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_maintainer_spellings() {
        let list = "{\n  alice = {\n    github = \"Alice\";\n  };\n  \"bob-2\" = {\n    email = \"x\";\n    github = \"bob\";\n  };\n}\n";
        let h = parse_maintainer_list(list);
        assert_eq!(h["alice"], "alice");
        assert_eq!(h["bob-2"], "bob");
        let a = parse_maintainers(
            "meta.maintainers = with lib.maintainers; [ alice bob-2 ];",
            &h,
        );
        assert_eq!(a.len(), 2);
        let b = parse_maintainers(
            "maintainers = [ lib.maintainers.alice ] ++ lib.teams.x.members;",
            &h,
        );
        assert_eq!(b.into_iter().collect::<Vec<_>>(), vec!["alice"]);
    }

    #[test]
    fn owners_lines_become_prefixes() {
        let o = parse_owners(
            "# c\n/pkgs/stdenv @NixOS/stdenv @Alice\n/lib/*.nix @bob\n/doc/ @NixOS/docs\n",
        );
        assert_eq!(
            o,
            vec![
                ("pkgs/stdenv".into(), vec!["alice".into()]),
                ("lib".into(), vec!["bob".into()])
            ]
        );
    }

    #[test]
    fn prefix_match_respects_segments() {
        assert!(under(
            "pkgs/by-name/he/hello/package.nix",
            "pkgs/by-name/he/hello"
        ));
        assert!(!under(
            "pkgs/by-name/he/hello2/package.nix",
            "pkgs/by-name/he/hello"
        ));
        assert_eq!(
            area_key("pkgs/by-name/he/hello/package.nix"),
            "pkgs/by-name/he/hello"
        );
        assert_eq!(
            area_key("nixos/modules/services/x.nix"),
            "nixos/modules/services/x.nix"
        );
    }
}
