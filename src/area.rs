//! Who knows which part of nixpkgs. Three derived sources, unioned:
//! `meta.maintainers` from evaluating nixpkgs, `ci/OWNERS`, and the
//! directories a person changed in merged pull requests over the last year.
//! Opt-in tags go on top.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use crate::attrs;
use crate::model::Pr;
use crate::store::Member;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Index {
    pub rev: String,
    /// login → paths whose `meta.maintainers` names them.
    pub maintainers: BTreeMap<String, BTreeSet<String>>,
    /// path prefix → logins, from `ci/OWNERS`.
    pub owners: Vec<(String, Vec<String>)>,
    /// pname → the area of the file that defines it.
    #[serde(default)]
    pub packages: BTreeMap<String, String>,
    /// pname → the attribute that spells it.
    #[serde(default)]
    pub by_pname: BTreeMap<String, String>,
    /// attribute path → what nixpkgs says about it.
    #[serde(default)]
    pub pkgs: BTreeMap<String, Package>,
    /// module file → its maintainers. Every file of `module-list.nix` is here,
    /// with or without one.
    #[serde(default)]
    pub modules: BTreeMap<String, Module>,
    /// team `shortName` → member logins.
    #[serde(default)]
    pub teams: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Package {
    pub pname: String,
    pub version: String,
    pub file: Option<String>,
    pub maintainers: Vec<String>,
    pub teams: Vec<String>,
    #[serde(default)]
    pub facts: Option<attrs::Facts>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Module {
    pub maintainers: Vec<String>,
    pub teams: Vec<String>,
}

impl Index {
    pub fn team_members(&self, teams: &[String]) -> Vec<String> {
        let mut out: Vec<String> = teams
            .iter()
            .filter_map(|t| self.teams.get(t))
            .flatten()
            .cloned()
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// The attribute a name means: itself, or the attribute that spells the
    /// package name.
    pub fn resolve(&self, name: &str) -> Option<&str> {
        match self.pkgs.get_key_value(name) {
            Some((attr, _)) => Some(attr),
            None => self.by_pname.get(name).map(String::as_str),
        }
    }
}

/// The area a file's maintainers cover: the package directory for a
/// `default.nix` or `package.nix`, the file itself otherwise, because a
/// NixOS module directory holds many unrelated modules.
pub fn area_key(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, "default.nix" | "package.nix")) => dir.to_string(),
        _ => path.to_string(),
    }
}

pub fn under(path: &str, prefix: &str) -> bool {
    !prefix.is_empty()
        && path.starts_with(prefix)
        && (path.len() == prefix.len() || path.as_bytes()[prefix.len()] == b'/')
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

/// What the NixOS module system and `lib.teams` say, from one evaluation.
#[derive(Debug, Default, Deserialize)]
pub struct Nixos {
    pub modules: BTreeMap<String, Module>,
    pub teams: BTreeMap<String, Vec<String>>,
}

pub fn from_entries(rev: &str, entries: &[attrs::Entry], owners: &str, nixos: Nixos) -> Index {
    let mut maintainers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut packages = BTreeMap::new();
    let mut by_pname = BTreeMap::new();
    let mut pkgs = BTreeMap::new();
    let index = attrs::Index::build(entries);
    let members = |logins: &[String], teams: &[String]| -> Vec<String> {
        let mut all: Vec<String> = logins
            .iter()
            .chain(teams.iter().filter_map(|t| nixos.teams.get(t)).flatten())
            .map(|l| l.to_lowercase())
            .collect();
        all.sort();
        all.dedup();
        all
    };
    for e in entries {
        pkgs.insert(
            e.attr.clone(),
            Package {
                pname: e.pname.clone(),
                version: e.version.clone(),
                file: e.file.clone(),
                maintainers: e.maintainers.clone(),
                teams: e.teams.clone(),
                facts: e.facts.clone(),
            },
        );
        let Some(file) = &e.file else {
            continue;
        };
        let area = area_key(file);
        for login in members(&e.maintainers, &e.teams) {
            maintainers.entry(login).or_default().insert(area.clone());
        }
        if index.resolve(&e.pname) == Some(e.attr.as_str()) {
            packages.insert(e.pname.clone(), area);
            by_pname.insert(e.pname.clone(), e.attr.clone());
        }
    }
    for (file, m) in &nixos.modules {
        for login in members(&m.maintainers, &m.teams) {
            maintainers.entry(login).or_default().insert(area_key(file));
        }
    }
    Index {
        rev: rev.to_string(),
        maintainers,
        owners: parse_owners(owners),
        packages,
        by_pname,
        pkgs,
        modules: nixos.modules,
        teams: nixos.teams,
    }
}

const NIXOS_EXPR: &str = r#"
let
  input = builtins.fromJSON (builtins.readFile ./input.json);
  path = input.path;
  lib = import (path + "/lib");
  eval = import (path + "/nixos/lib/eval-config.nix") {
    inherit (input) system;
    modules = [ ];
  };
  rel = f: lib.removePrefix (path + "/") (toString f);
  logins = ms: builtins.filter (x: x != null) (map (m: m.github or null) ms);
  maintainers = lib.mapAttrs' (f: ms: lib.nameValuePair (rel f) (logins ms)) eval.config.meta.maintainers;
  teams = lib.mapAttrs' (f: ts: lib.nameValuePair (rel f) (map (t: t.shortName or "") ts)) (
    eval.config.meta.teams or { }
  );
  listed = map rel (
    builtins.filter (m: builtins.isPath m || builtins.isString m) (
      import (path + "/nixos/modules/module-list.nix")
    )
  );
  files = lib.unique (listed ++ builtins.attrNames maintainers ++ builtins.attrNames teams);
in
{
  modules = lib.genAttrs (builtins.filter (f: lib.hasPrefix "nixos/" f) files) (f: {
    maintainers = maintainers.${f} or [ ];
    teams = teams.${f} or [ ];
  });
  teams = lib.mapAttrs' (
    _: t: lib.nameValuePair (t.shortName or "") (map lib.toLower (logins (t.members or [ ])))
  ) lib.teams;
}
"#;

/// One NixOS evaluation with no configuration: enough to read every module's
/// `meta.maintainers` and `meta.teams`, and it takes seconds, not minutes.
pub fn sweep_nixos(nixpkgs: &Path, system: &str) -> Result<Nixos> {
    let input = serde_json::json!({
        "path": nixpkgs.to_string_lossy(),
        "system": system,
    });
    let text = crate::eval::eval_with_input("nixos", NIXOS_EXPR, &input)?;
    Ok(serde_json::from_str(&text)?)
}

async fn master_rev(http: &reqwest::Client) -> Result<String> {
    let sha = http
        .get("https://api.github.com/repos/NixOS/nixpkgs/commits/master")
        .header("Accept", "application/vnd.github.sha")
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    anyhow::ensure!(sha.len() == 40, "unexpected master revision {sha:?}");
    Ok(sha)
}

/// Unpacks a nixpkgs tarball into `dir`, dropping the top directory GitHub
/// names after the revision.
fn unpack(bytes: &[u8], dir: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let rel: PathBuf = path.components().skip(1).collect();
        if rel.as_os_str().is_empty() {
            continue;
        }
        entry.unpack(dir.join(rel))?;
    }
    Ok(())
}

/// Sweeps nixpkgs master with the configured evaluator. The source is a
/// tarball unpacked under `data`, not a flake reference, so the evaluator
/// needs no daemon and writes nothing to a store.
pub async fn fetch(data: &Path, sets: &[String]) -> Result<Index> {
    let http = crate::github::http();
    let rev = master_rev(&http).await?;
    let dir = data.join(format!("nixpkgs-{rev}"));
    if !dir.join("default.nix").exists() {
        let bytes = http
            .get(format!(
                "https://codeload.github.com/NixOS/nixpkgs/tar.gz/{rev}"
            ))
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await
            .context("nixpkgs tarball")?;
        let tmp = data.join(format!("nixpkgs-{rev}.partial"));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        let t = tmp.clone();
        tokio::task::spawn_blocking(move || unpack(&bytes, &t)).await??;
        std::fs::rename(&tmp, &dir)?;
    }
    for old in std::fs::read_dir(data)?.flatten() {
        let name = old.file_name().to_string_lossy().to_string();
        if name.starts_with("nixpkgs-") && old.path() != dir {
            let _ = std::fs::remove_dir_all(old.path());
        }
    }
    let owners = std::fs::read_to_string(dir.join("ci/OWNERS")).unwrap_or_default();
    let sets = sets.to_vec();
    let d = dir.clone();
    let entries = tokio::task::spawn_blocking(move || -> Result<Vec<attrs::Entry>> {
        let mut all = Vec::new();
        for set in &sets {
            let started = std::time::Instant::now();
            let found = attrs::sweep(&d, "x86_64-linux", set)?;
            tracing::info!(
                "swept {:?} with {}: {} derivations in {:.1}s",
                if set.is_empty() {
                    "top level"
                } else {
                    set.as_str()
                },
                crate::eval::evaluator(),
                found.len(),
                started.elapsed().as_secs_f64()
            );
            all.extend(found);
        }
        Ok(all)
    })
    .await??;
    let d = dir.clone();
    let nixos = tokio::task::spawn_blocking(move || {
        let started = std::time::Instant::now();
        let n = sweep_nixos(&d, "x86_64-linux");
        if let Ok(n) = &n {
            tracing::info!(
                "swept NixOS: {} modules, {} teams in {:.1}s",
                n.modules.len(),
                n.teams.len(),
                started.elapsed().as_secs_f64()
            );
        }
        n
    })
    .await?
    .unwrap_or_else(|e| {
        tracing::warn!("NixOS modules: {e:#}");
        Nixos::default()
    });
    Ok(from_entries(&rev, &entries, &owners, nixos))
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

    #[test]
    fn a_team_member_maintains_the_team_packages_and_modules() {
        let e = attrs::Entry {
            attr: "nginx".into(),
            pname: "nginx".into(),
            version: "1".into(),
            file: Some("pkgs/servers/http/nginx/default.nix".into()),
            maintainers: vec![],
            teams: vec!["web".into()],
            facts: None,
        };
        let nixos = Nixos {
            modules: [(
                "nixos/modules/services/web-servers/nginx/default.nix".to_string(),
                Module {
                    maintainers: vec!["Alice".into()],
                    teams: vec![],
                },
            )]
            .into(),
            teams: [("web".to_string(), vec!["bob".to_string()])].into(),
        };
        let i = from_entries("r", &[e], "", nixos);
        assert!(i.maintainers["bob"].contains("pkgs/servers/http/nginx"));
        assert!(i.maintainers["alice"].contains("nixos/modules/services/web-servers/nginx"));
        assert_eq!(i.resolve("nginx"), Some("nginx"));
    }
}
