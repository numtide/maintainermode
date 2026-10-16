//! The client: evaluates the closure of a flake output or of a file's
//! attributes on the owner's machine and matches each package to a nixpkgs
//! attribute. Taken from nixfresh `nix.rs` (MIT) and trimmed. It always runs
//! plain `nix`: `derivation show` and `getFlake` are not part of the
//! `nix eval --file` contract that a swept evaluator has to answer.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::eval::{eval_with_input_using, run_with};
use crate::report::{Found, Omitted, Report, SCHEMA, Source};

const NIX: &str = "nix";

pub struct Target {
    pub flake: Option<String>,
    pub file: Option<PathBuf>,
    pub outputs: Vec<String>,
    /// A nixpkgs path or flake reference; by default the one the tree pins.
    pub nixpkgs: Option<String>,
    pub nixpkgs_input: Option<String>,
    pub system: Option<String>,
    pub runtime: bool,
}

impl Target {
    fn root(&self) -> Option<PathBuf> {
        if let Some(f) = &self.file {
            return Some(
                f.parent()
                    .map_or_else(|| PathBuf::from("."), Path::to_path_buf),
            );
        }
        let flake = self.flake.as_deref()?;
        let local = flake.strip_prefix("path:").unwrap_or(flake);
        let local = local.split('?').next().unwrap_or(local);
        (!local.contains(':')).then(|| PathBuf::from(local))
    }

    fn name(&self) -> String {
        match (&self.file, &self.flake) {
            (Some(f), _) => f.to_string_lossy().into_owned(),
            (None, Some(f)) => f.clone(),
            _ => String::new(),
        }
    }

    fn installables(&self) -> Vec<String> {
        match (&self.file, &self.flake) {
            (Some(file), _) => {
                let mut args = vec!["--file".into(), file.to_string_lossy().into_owned()];
                args.extend(self.outputs.iter().cloned());
                args
            }
            (None, Some(flake)) => self
                .outputs
                .iter()
                .map(|o| format!("{flake}#{o}"))
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn nixpkgs_rev(&self) -> Option<String> {
        let root = self.root()?;
        if self.file.is_some() {
            return npins_rev(&root, self.nixpkgs_input.as_deref());
        }
        nixpkgs_rev_named(&root, self.nixpkgs_input.as_deref())
    }
}

/// The nixpkgs revision a flake pins. `input` names the lock entry when the
/// flake does not call it `nixpkgs`.
fn nixpkgs_rev_named(root: &Path, input: Option<&str>) -> Option<String> {
    let text = std::fs::read_to_string(root.join("flake.lock")).ok()?;
    let lock: serde_json::Value = serde_json::from_str(&text).ok()?;
    let nodes = lock.get("nodes")?.as_object()?;
    let name = match input {
        Some(name) => name.to_string(),
        None => sole_nixpkgs(nodes)?,
    };
    let key = follow(nodes, "root", &name, 0)?;
    nodes
        .get(&key)?
        .get("locked")?
        .get("rev")?
        .as_str()
        .map(str::to_string)
}

/// `nixpkgs` when the root declares it, otherwise the only input whose name
/// starts with it. With several candidates, it does not guess.
fn sole_nixpkgs(nodes: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    let inputs = nodes.get("root")?.get("inputs")?.as_object()?;
    if inputs.contains_key("nixpkgs") {
        return Some("nixpkgs".into());
    }
    let mut candidates = inputs.keys().filter(|k| k.starts_with("nixpkgs"));
    let first = candidates.next()?;
    candidates.next().is_none().then(|| first.clone())
}

/// An input is a node name or a follow path, walked from the root node the
/// way nix walks it.
fn follow(
    nodes: &serde_json::Map<String, serde_json::Value>,
    from: &str,
    name: &str,
    depth: usize,
) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let value = nodes.get(from)?.get("inputs")?.get(name)?;
    if let Some(node) = value.as_str() {
        return Some(node.to_string());
    }
    let mut current = "root".to_string();
    for segment in value.as_array()? {
        current = follow(nodes, &current, segment.as_str()?, depth + 1)?;
    }
    Some(current)
}

/// A Git pin carries `revision`. A Channel pin hides the commit in the last
/// dotted field of its release directory:
/// `…/nixos-26.05.1947.a0374025a863/nixexprs.tar.xz`.
fn npins_rev(root: &Path, input: Option<&str>) -> Option<String> {
    let text = std::fs::read_to_string(root.join("npins/sources.json")).ok()?;
    let pins: serde_json::Value = serde_json::from_str(&text).ok()?;
    let pin = pins.get("pins")?.get(input.unwrap_or("nixpkgs"))?;
    if let Some(rev) = pin.get("revision").and_then(|v| v.as_str()) {
        return Some(rev.to_string());
    }
    let url = pin.get("url").and_then(|v| v.as_str())?;
    url.split('/').find_map(release_rev)
}

fn release_rev(segment: &str) -> Option<String> {
    let tail = segment.rsplit_once('.')?.1;
    let hex = tail.len() >= 7 && tail.chars().all(|c| c.is_ascii_hexdigit());
    hex.then(|| tail.to_string())
}

/// The first dash-separated part that starts with a digit begins the version.
fn split_name(name: &str) -> (String, String) {
    let parts: Vec<&str> = name.split('-').collect();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 && part.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return (parts[..i].join("-"), parts[i..].join("-"));
        }
    }
    (name.to_string(), String::new())
}

fn strip_store_path(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    let base = base.strip_suffix(".drv").unwrap_or(base);
    match base.split_once('-') {
        Some((hash, rest)) if hash.len() == 32 => rest.to_string(),
        _ => base.to_string(),
    }
}

#[derive(Debug, Deserialize)]
struct Derivation {
    #[serde(default)]
    name: String,
    #[serde(default)]
    env: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    outputs: BTreeMap<String, Output>,
}

#[derive(Debug, Deserialize)]
struct Output {
    #[serde(default)]
    hash: Option<String>,
    #[serde(default, rename = "hashAlgo")]
    hash_algo: Option<String>,
}

/// Fixed-output derivations are fetches, and a build without a version is
/// infrastructure that no advisory or release feed speaks about.
fn is_package(name: &str, version: &str, drv: Option<&Derivation>) -> bool {
    let fixed_output = drv.is_some_and(|d| {
        d.env.contains_key("outputHash")
            || d.outputs
                .values()
                .any(|o| o.hash.is_some() || o.hash_algo.is_some())
    });
    if fixed_output || version.is_empty() {
        return false;
    }
    const SUFFIXES: [&str; 22] = [
        ".patch", ".diff", ".tar", ".gz", ".xz", ".bz2", ".zip", ".json", ".toml", ".lock", ".sh",
        ".nix", ".tgz", ".tbz2", ".zst", ".crate", ".whl", ".hex0", ".hex1", ".hex2", ".m1", ".m2",
    ];
    let lower = name.to_ascii_lowercase();
    if SUFFIXES.iter().any(|s| lower.ends_with(s)) {
        return false;
    }
    const INFRA: [&str; 2] = ["bootstrap-stage", "tinycc-boot"];
    if INFRA.iter().any(|p| lower.starts_with(p)) {
        return false;
    }
    !matches!(lower.as_str(), "source" | "patches" | "env" | "builder")
}

fn nix(args: &[String]) -> Result<String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_with(NIX, &refs)
}

fn derivation_closure(target: &Target) -> Result<Vec<(String, String)>> {
    let mut args: Vec<String> = vec!["derivation".into(), "show".into(), "-r".into()];
    args.extend(target.installables());
    let raw: serde_json::Value =
        serde_json::from_str(&nix(&args)?).context("parse derivation show output")?;
    // nix 2.35 wraps the map in { derivations, version }; older nix returns it bare.
    let map = raw.get("derivations").cloned().unwrap_or(raw);
    let drvs: BTreeMap<String, Derivation> =
        serde_json::from_value(map).context("read derivation map")?;
    let mut out = Vec::new();
    for (path, drv) in drvs {
        let name = if drv.name.is_empty() {
            strip_store_path(&path)
        } else {
            drv.name.clone()
        };
        let text = |k: &str| drv.env.get(k).and_then(|v| v.as_str()).map(str::to_string);
        let (fallback_pname, fallback_version) = split_name(&name);
        let pname = text("pname").unwrap_or(fallback_pname);
        let version = text("version").unwrap_or(fallback_version);
        if is_package(&name, &version, Some(&drv)) {
            out.push((pname, version));
        }
    }
    Ok(out)
}

/// Store paths, not derivations: the outputs must be built or substituted.
fn runtime_closure(target: &Target) -> Result<Vec<(String, String)>> {
    let mut args: Vec<String> = vec!["path-info".into(), "-r".into()];
    args.extend(target.installables());
    Ok(nix(&args)?
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| split_name(&strip_store_path(l)))
        .filter(|(name, version)| is_package(name, version, None))
        .collect())
}

const PROBE_EXPR: &str = r#"
let
  input = builtins.fromJSON (builtins.readFile ./input.json);
  src = if input.flake then (builtins.getFlake input.nixpkgs).outPath else input.nixpkgs;
  pkgs = import src {
    system = if input.system == "" then builtins.currentSystem else input.system;
    overlays = [ ];
    config = {
      allowUnfree = true;
      allowBroken = true;
      allowInsecurePredicate = _: true;
    };
  };
  at =
    path: value:
    if path == [ ] then
      value
    else if builtins.isAttrs value && builtins.hasAttr (builtins.head path) value then
      at (builtins.tail path) (builtins.getAttr (builtins.head path) value)
    else
      null;
  isDrv =
    name:
    let
      v = builtins.tryEval (
        let
          p = at (builtins.filter builtins.isString (builtins.split "\\." name)) pkgs;
        in
        builtins.isAttrs p && (p.type or "") == "derivation"
      );
    in
    v.success && v.value;
in
builtins.filter isDrv input.names
"#;

/// Attribute paths to try for a package name, best first.
fn candidates(pname: &str) -> Vec<String> {
    let mut out = vec![pname.to_string(), pname.to_ascii_lowercase()];
    out.push(format!("python3Packages.{pname}"));
    out.dedup();
    out
}

fn probe(nixpkgs: &str, system: &str, names: &[String]) -> Result<BTreeSet<String>> {
    if names.is_empty() {
        return Ok(BTreeSet::new());
    }
    let is_flake = nixpkgs.contains(':');
    let path = if is_flake {
        nixpkgs.to_string()
    } else {
        std::fs::canonicalize(nixpkgs)
            .with_context(|| format!("nixpkgs at {nixpkgs}"))?
            .to_string_lossy()
            .into_owned()
    };
    let input = serde_json::json!({
        "nixpkgs": path,
        "flake": is_flake,
        "system": system,
        "names": names,
    });
    let text = eval_with_input_using(NIX, "probe", PROBE_EXPR, &input)?;
    Ok(serde_json::from_str::<Vec<String>>(&text)
        .context("parse probe output")?
        .into_iter()
        .collect())
}

pub fn scan(target: &Target) -> Result<Report> {
    if target.flake.is_none() && target.file.is_none() {
        bail!("name a flake with --flake or a file with --file");
    }
    if target.flake.is_some() && target.outputs.is_empty() {
        bail!(
            "name at least one flake output, like nixosConfigurations.<host>.config.system.build.toplevel"
        );
    }
    let rev = target.nixpkgs_rev();
    let nixpkgs = match (&target.nixpkgs, &rev) {
        (Some(n), _) => n.clone(),
        (None, Some(r)) => format!("github:NixOS/nixpkgs/{r}"),
        (None, None) => "flake:nixpkgs".to_string(),
    };
    let mut pairs = if target.runtime {
        runtime_closure(target)?
    } else {
        derivation_closure(target)?
    };
    pairs.retain(|(p, _)| !p.is_empty());
    pairs.sort();
    pairs.dedup();
    let mut names: Vec<String> = pairs.iter().flat_map(|(p, _)| candidates(p)).collect();
    names.sort();
    names.dedup();
    let system = target.system.clone().unwrap_or_else(|| {
        format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS).replace("macos", "darwin")
    });
    let known = probe(&nixpkgs, &system, &names)?;
    let packages = pairs
        .into_iter()
        .map(|(pname, version)| {
            let attr = candidates(&pname).into_iter().find(|c| known.contains(c));
            Found {
                resolution: if attr.is_some() { "attr" } else { "unknown" }.into(),
                pname,
                version,
                attr,
            }
        })
        .collect();
    let now = Utc::now().to_rfc3339();
    Ok(Report {
        schema: SCHEMA,
        client: format!("maintainer-mode/{}", env!("CARGO_PKG_VERSION")),
        generated_at: now.clone(),
        source: Source {
            name: target.name(),
            system,
            nixpkgs_rev: rev,
            scanned_at: Some(now),
        },
        packages,
        omitted: Omitted::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_input_that_follows_another_flake_still_names_its_node() {
        let lock = serde_json::json!({
            "nodes": {
                "root": { "inputs": { "other": "other", "nixpkgs": ["other", "nixpkgs"] } },
                "other": { "inputs": { "nixpkgs": "nixpkgs" }, "locked": { "rev": "aaa" } },
                "nixpkgs": { "locked": { "rev": "bbb" } }
            }
        });
        let nodes = lock["nodes"].as_object().unwrap();
        assert_eq!(
            follow(nodes, "root", "nixpkgs", 0).as_deref(),
            Some("nixpkgs")
        );
        assert_eq!(follow(nodes, "root", "absent", 0), None);
    }

    #[test]
    fn two_nixpkgs_inputs_need_one_named() {
        let lock = serde_json::json!({
            "nodes": {
                "root": { "inputs": { "nixpkgs-a": "nixpkgs-a", "nixpkgs-b": "nixpkgs-b" } },
            }
        });
        assert_eq!(sole_nixpkgs(lock["nodes"].as_object().unwrap()), None);
        let lock = serde_json::json!({
            "nodes": { "root": { "inputs": { "nixpkgs-unstable": "x", "utils": "y" } } }
        });
        assert_eq!(
            sole_nixpkgs(lock["nodes"].as_object().unwrap()).as_deref(),
            Some("nixpkgs-unstable")
        );
    }

    #[test]
    fn an_npins_channel_pin_hides_its_commit_in_the_url() {
        assert_eq!(
            release_rev("nixos-26.05.1947.a0374025a863").as_deref(),
            Some("a0374025a863")
        );
        assert_eq!(release_rev("nixexprs.tar.xz"), None);
    }

    #[test]
    fn names_split_and_infrastructure_is_not_a_package() {
        assert_eq!(
            split_name("python3.12-requests-2.32.3"),
            ("python3.12-requests".into(), "2.32.3".into())
        );
        assert_eq!(
            strip_store_path("/nix/store/2x905q8m3w6wj9kh1k2agzqx9x0ksapl-zlib-1.3.1.drv"),
            "zlib-1.3.1"
        );
        assert!(is_package("zlib-1.3.1", "1.3.1", None));
        assert!(!is_package("hex2_linker-0-footer.M1", "0-footer.M1", None));
        assert!(!is_package("bootstrap-stage0-stdenv-linux", "1", None));
        assert!(!is_package("stdenv-linux", "", None));
    }

    #[test]
    fn a_local_flake_has_a_root_and_a_remote_one_does_not() {
        let t = |flake: &str| Target {
            flake: Some(flake.into()),
            file: None,
            outputs: vec!["x".into()],
            nixpkgs: None,
            nixpkgs_input: None,
            system: None,
            runtime: false,
        };
        assert_eq!(t(".").root(), Some(PathBuf::from(".")));
        assert_eq!(
            t("path:/srv/tree?x=1").root(),
            Some(PathBuf::from("/srv/tree"))
        );
        assert_eq!(t("github:o/r").root(), None);
        assert_eq!(t(".").installables(), vec![".#x"]);
    }
}
