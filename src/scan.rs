//! The client: reads the closure of a flake output or of a file's attributes
//! on the owner's machine. `export` and `submit` match the names against the
//! server's public list (see `report.rs`). Taken from nixfresh `nix.rs` (MIT)
//! and trimmed. It always runs
//! plain `nix`: `derivation show` and `getFlake` are not part of the
//! `nix eval --file` contract that a swept evaluator has to answer.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::Deserialize;
use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::eval::run_with;
use crate::report::{Found, Report, SCHEMA, Source};

const NIX: &str = "nix";

pub struct Target {
    pub flake: Option<String>,
    pub file: Option<PathBuf>,
    pub outputs: Vec<String>,
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
pub(crate) fn nixpkgs_rev_named(root: &Path, input: Option<&str>) -> Option<String> {
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

/// Only the fields the scan reads. The output of `derivation show` can come
/// from a third party, so nothing else is kept in memory, not even as a
/// `serde_json::Value`.
#[derive(Debug, Deserialize)]
struct Derivation {
    #[serde(default)]
    name: String,
    #[serde(default)]
    env: Env,
    #[serde(default, deserialize_with = "any_fixed_output")]
    outputs: bool,
}

#[derive(Debug, Default, Deserialize)]
struct Env {
    #[serde(default, deserialize_with = "text_or_none")]
    pname: Option<String>,
    #[serde(default, deserialize_with = "text_or_none")]
    version: Option<String>,
    #[serde(default, rename = "outputHash", deserialize_with = "present")]
    output_hash: bool,
}

#[derive(Deserialize)]
struct Output {
    #[serde(default)]
    hash: Option<IgnoredAny>,
    #[serde(default, rename = "hashAlgo")]
    hash_algo: Option<IgnoredAny>,
}

fn present<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    IgnoredAny::deserialize(d).map(|_| true)
}

fn any_fixed_output<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = bool;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a map of outputs")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<bool, A::Error> {
            let mut fixed = false;
            while let Some((_, o)) = m.next_entry::<IgnoredAny, Output>()? {
                fixed |= o.hash.is_some() || o.hash_algo.is_some();
            }
            Ok(fixed)
        }
    }
    d.deserialize_map(V)
}

/// A string, or `None` for any other JSON value.
fn text_or_none<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = Option<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("any value")
        }
        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
            Ok(Some(v.to_string()))
        }
        fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<Self::Value, A::Error> {
            while s.next_element::<IgnoredAny>()?.is_some() {}
            Ok(None)
        }
        fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
            while m.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(None)
        }
    }
    d.deserialize_any(V)
}

/// Fixed-output derivations are fetches, and a build without a version is
/// infrastructure that no advisory or release feed speaks about.
fn is_package(name: &str, version: &str, fixed_output: bool) -> bool {
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
    closure_pairs(nix(&args)?.as_bytes())
}

/// The packages, as (pname, version), in the output of `nix derivation show -r`.
pub(crate) fn closure_pairs(text: &[u8]) -> Result<Vec<(String, String)>> {
    closure_pairs_at_most(text, usize::MAX)
}

/// Reads the closure in one pass and keeps only distinct packages; it stops
/// past `max` of them.
pub(crate) fn closure_pairs_at_most(text: &[u8], max: usize) -> Result<Vec<(String, String)>> {
    let mut found = Closure {
        max,
        pairs: BTreeSet::new(),
    };
    let mut de = serde_json::Deserializer::from_slice(text);
    TopLevel(&mut found)
        .deserialize(&mut de)
        .and_then(|()| de.end())
        .context("parse derivation show output")?;
    Ok(found.pairs.into_iter().collect())
}

struct Closure {
    max: usize,
    pairs: BTreeSet<(String, String)>,
}

impl Closure {
    fn add<E: de::Error>(&mut self, path: &str, drv: Derivation) -> Result<(), E> {
        let name = if drv.name.is_empty() {
            strip_store_path(path)
        } else {
            drv.name
        };
        let (fallback_pname, fallback_version) = split_name(&name);
        let pname = drv.env.pname.unwrap_or(fallback_pname);
        let version = drv.env.version.unwrap_or(fallback_version);
        if is_package(&name, &version, drv.env.output_hash || drv.outputs) {
            self.pairs.insert((pname, version));
            if self.pairs.len() > self.max {
                return Err(E::custom(format!("more than {} packages", self.max)));
            }
        }
        Ok(())
    }
}

/// nix 2.35 wraps the map in { derivations, version }; older nix returns it
/// bare. A store path is never `derivations` or `version`.
struct TopLevel<'a>(&'a mut Closure);

impl<'de> DeserializeSeed<'de> for TopLevel<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for TopLevel<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a map of derivations")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<(), A::Error> {
        while let Some(key) = m.next_key::<String>()? {
            match key.as_ref() {
                "derivations" => m.next_value_seed(Drvs(&mut *self.0))?,
                "version" => {
                    m.next_value::<IgnoredAny>()?;
                }
                path => {
                    let drv = m.next_value::<Derivation>()?;
                    self.0.add(path, drv)?;
                }
            }
        }
        Ok(())
    }
}

struct Drvs<'a>(&'a mut Closure);

impl<'de> DeserializeSeed<'de> for Drvs<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for Drvs<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a map of derivations")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<(), A::Error> {
        while let Some(path) = m.next_key::<String>()? {
            let drv = m.next_value::<Derivation>()?;
            self.0.add(&path, drv)?;
        }
        Ok(())
    }
}

/// Store paths, not derivations: the outputs must be built or substituted.
fn runtime_closure(target: &Target) -> Result<Vec<(String, String)>> {
    let mut args: Vec<String> = vec!["path-info".into(), "-r".into()];
    args.extend(target.installables());
    Ok(nix(&args)?
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| split_name(&strip_store_path(l)))
        .filter(|(name, version)| is_package(name, version, false))
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
    let mut pairs = if target.runtime {
        runtime_closure(target)?
    } else {
        derivation_closure(target)?
    };
    pairs.retain(|(p, _)| !p.is_empty());
    pairs.sort();
    pairs.dedup();
    let system = target.system.clone().unwrap_or_else(|| {
        format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS).replace("macos", "darwin")
    });
    let packages = pairs
        .into_iter()
        .map(|(pname, version)| Found {
            pname,
            version,
            attr: None,
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
            nixpkgs_rev: target.nixpkgs_rev(),
            scanned_at: Some(now),
        },
        packages,
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
        assert!(is_package("zlib-1.3.1", "1.3.1", false));
        assert!(!is_package("hex2_linker-0-footer.M1", "0-footer.M1", false));
        assert!(!is_package("bootstrap-stage0-stdenv-linux", "1", false));
        assert!(!is_package("stdenv-linux", "", false));
    }

    #[test]
    fn the_closure_reads_from_old_and_new_nix() {
        let drv = serde_json::json!({
            "x-zlib-1.3.1.drv": { "name": "zlib-1.3.1", "env": { "pname": "zlib", "version": "1.3.1" } },
            "y-source.drv": { "name": "source", "env": { "outputHash": "h" } }
        });
        let new = serde_json::json!({ "derivations": drv, "version": 4 });
        for v in [drv, new] {
            let pairs = closure_pairs(v.to_string().as_bytes()).unwrap();
            assert_eq!(pairs, vec![("zlib".to_string(), "1.3.1".to_string())]);
        }
    }

    #[test]
    fn the_closure_ignores_odd_fields_and_stops_past_the_limit() {
        let drv = serde_json::json!({ "derivations": {
            "a-zlib-1.3.1.drv": {
                "name": "zlib-1.3.1",
                "args": ["", "", ""],
                "env": { "pname": ["not", "text"], "version": 1, "big": { "x": [] } },
                "outputs": { "out": { "path": "/nix/store/x" } }
            },
            "b-src-1.drv": { "name": "src-1", "outputs": { "out": { "hashAlgo": "sha256" } } },
            "c-curl-8.drv": { "name": "curl-8", "env": { "outputHash": null } },
            "d-zlib-1.3.1.drv": { "name": "zlib-1.3.1" },
            "e-curl-8.drv": { "name": "curl-8" }
        }, "version": 4 })
        .to_string();
        let pairs = closure_pairs_at_most(drv.as_bytes(), 2).unwrap();
        assert_eq!(
            pairs,
            vec![
                ("curl".to_string(), "8".to_string()),
                ("zlib".to_string(), "1.3.1".to_string())
            ]
        );
        assert!(closure_pairs_at_most(drv.as_bytes(), 1).is_err());
        assert!(closure_pairs(b"{} trailing").is_err());
    }

    #[test]
    fn a_local_flake_has_a_root_and_a_remote_one_does_not() {
        let t = |flake: &str| Target {
            flake: Some(flake.into()),
            file: None,
            outputs: vec!["x".into()],
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
