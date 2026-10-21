//! The client: reads the closure of a flake output or of a file's attributes
//! on the owner's machine. `export` and `submit` match the names against the
//! server's public list (see `report.rs`). It always runs plain `nix`:
//! `derivation show` and `getFlake` are not part of the `nix eval --file`
//! contract that a swept evaluator has to answer.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::Deserialize;
use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::eval::{private_dir, run_with, stream_with};
use crate::report::{Found, MAX_PACKAGES, Report, SCHEMA, ScanOutput, Source};

const NIX: &str = "nix";
const DISCOVER: &str = include_str!("discover.nix");

#[derive(Clone)]
pub struct Target {
    pub flake: Option<String>,
    pub file: Option<PathBuf>,
    pub outputs: Vec<String>,
    pub nixpkgs_input: Option<String>,
    pub system: Option<String>,
    pub runtime: bool,
    /// Outputs to leave out, as `--ignore` gives them: an attribute path, or
    /// the start of one, such as `checks`.
    pub ignore: Vec<String>,
    /// The flake gave the outputs, not `--output`. Only then does an output
    /// that fails get skipped: a named one that fails would make a smaller
    /// report, and its upload would replace the whole set.
    pub discovered: bool,
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

    /// One nix call for each output: a name to show, and the arguments that
    /// name it. They go one by one, so that an output which does not evaluate
    /// on this machine costs its own closure only.
    fn installables(&self) -> Vec<(String, Vec<String>)> {
        match (&self.file, &self.flake) {
            (Some(file), _) => {
                let path = file.to_string_lossy().into_owned();
                let file_arg = |attr: Option<&String>| {
                    let mut args = vec!["--file".to_string(), path.clone()];
                    args.extend(attr.cloned());
                    args
                };
                if self.outputs.is_empty() {
                    return vec![(path.clone(), file_arg(None))];
                }
                self.outputs
                    .iter()
                    .map(|a| (a.clone(), file_arg(Some(a))))
                    .collect()
            }
            (None, Some(flake)) => self
                .outputs
                .iter()
                .map(|o| (o.clone(), vec![format!("{flake}#{o}")]))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The outputs the scan reads. Without `--output`, a flake gives every
    /// output worth reading: the packages, devShells and checks of this
    /// system, and the systems that the configurations build. It evaluates
    /// names only. `--ignore` then leaves outputs out, whether the flake gave
    /// them or `--output` named them.
    pub fn choose_outputs(&mut self) -> Result<()> {
        let discovered = self.discover()?;
        let before = self.outputs.len();
        self.outputs.retain(|o| !ignored(o, &self.ignore));
        let gone = before - self.outputs.len();
        if gone > 0 {
            eprintln!("{gone} {} ignored", outputs(gone));
        }
        // a file with no attribute reads the whole file, so here it must stop
        if self.outputs.is_empty() && (self.flake.is_some() || gone > 0) {
            bail!("--ignore leaves no output to read");
        }
        if let Some(flake) = discovered {
            let n = self.outputs.len();
            eprintln!("{n} {} to read in {flake}", outputs(n));
        }
        Ok(())
    }

    /// Asks the flake for its outputs, when `--output` named none. It answers
    /// the flake it asked, and nothing when it asked nothing.
    fn discover(&mut self) -> Result<Option<String>> {
        if self.flake.is_none() || !self.outputs.is_empty() {
            return Ok(None);
        }
        let flake = self.flake_ref()?;
        let request = serde_json::json!({
            "flake": flake,
            "system": self.system.clone().unwrap_or_else(this_system),
            "runtime": self.runtime,
        });
        let dir = private_dir("discover")?;
        let answer = run_discover(&dir, &request);
        let _ = std::fs::remove_dir_all(&dir);
        let paths: Vec<Vec<String>> =
            serde_json::from_str(&answer?).context("parse the outputs of the flake")?;
        self.outputs = paths.iter().map(|p| attr_path(p)).collect();
        self.discovered = true;
        if self.outputs.is_empty() {
            bail!(
                "{flake} has no package, devShell, check or configuration for this system; \
                 name an output with --output"
            );
        }
        Ok(Some(flake))
    }

    /// What `builtins.getFlake` takes: a local tree by absolute path, or the
    /// flake reference as it was given.
    fn flake_ref(&self) -> Result<String> {
        let flake = self.flake.clone().unwrap_or_default();
        let Some(root) = self.local_root() else {
            return Ok(flake);
        };
        let root = std::fs::canonicalize(&root)
            .with_context(|| format!("find the flake at {}", root.display()))?;
        Ok(root.to_string_lossy().into_owned())
    }

    pub(crate) fn local_root(&self) -> Option<PathBuf> {
        self.flake.as_ref().and(self.root())
    }

    /// The flake input that is nixpkgs: `--nixpkgs-input`, or the one the lock
    /// file makes plain.
    pub(crate) fn nixpkgs_input_name(&self, root: &Path) -> Option<String> {
        if let Some(name) = &self.nixpkgs_input {
            return Some(name.clone());
        }
        let lock = read_lock(root)?;
        sole_nixpkgs(lock.get("nodes")?.as_object()?)
    }

    pub fn nixpkgs_rev(&self) -> Option<String> {
        let root = self.root()?;
        if self.file.is_some() {
            return npins_rev(&root, self.nixpkgs_input.as_deref());
        }
        nixpkgs_rev_named(&root, self.nixpkgs_input.as_deref())
    }
}

/// Evaluates `discover.nix` against the request. The expression is plain nix:
/// `builtins.getFlake` needs `--impure` for a local tree, and no evaluator
/// other than nix promises it.
fn run_discover(dir: &Path, request: &serde_json::Value) -> Result<String> {
    std::fs::write(dir.join("input.json"), serde_json::to_vec(request)?)?;
    let expr = dir.join("discover.nix");
    std::fs::write(&expr, DISCOVER)?;
    run_with(
        NIX,
        &[
            "eval",
            "--json",
            "--impure",
            "--file",
            &expr.to_string_lossy(),
        ],
    )
}

/// The segments of an attribute path, joined the way an installable writes
/// them: a name that is not plain, such as `me@box`, keeps its quotes.
fn attr_path(segments: &[String]) -> String {
    segments
        .iter()
        .map(|s| {
            let plain = !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '\''));
            if plain {
                s.clone()
            } else {
                format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// An output goes when `--ignore` names it, or names a path it sits under:
/// `checks` leaves every check, `checks.x86_64-linux.vm` leaves that one. The
/// paths compare segment by segment, so `checks.a` does not leave
/// `checks."a.b"`, and the quotes around a name such as `me@box` are optional.
fn ignored(output: &str, ignore: &[String]) -> bool {
    let output = crate::trace::attr_path(output);
    ignore
        .iter()
        .any(|i| output.starts_with(&crate::trace::attr_path(i)))
}

fn outputs(n: usize) -> &'static str {
    if n == 1 { "output" } else { "outputs" }
}

/// The system of this machine, as nix names it.
fn this_system() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS).replace("macos", "darwin")
}

/// The nixpkgs revision a flake pins. `input` names the lock entry when the
/// flake does not call it `nixpkgs`.
pub(crate) fn nixpkgs_rev_named(root: &Path, input: Option<&str>) -> Option<String> {
    let lock = read_lock(root)?;
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

fn read_lock(root: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(root.join("flake.lock")).ok()?;
    serde_json::from_str(&text).ok()
}

/// The commit of `input` when the lock pins it to public nixpkgs: the
/// `NixOS/nixpkgs` repository on GitHub. A fork or a local tree is `None`.
pub(crate) fn locked_nixpkgs(root: &Path, input: &str) -> Option<String> {
    locked_nixpkgs_in(&read_lock(root)?, input)
}

fn locked_nixpkgs_in(lock: &serde_json::Value, input: &str) -> Option<String> {
    let nodes = lock.get("nodes")?.as_object()?;
    let locked = nodes
        .get(&follow(nodes, "root", input, 0)?)?
        .get("locked")?;
    let field = |k: &str| locked.get(k).and_then(|v| v.as_str());
    let public = field("type") == Some("github")
        && field("owner").is_some_and(|o| o.eq_ignore_ascii_case("nixos"))
        && field("repo") == Some("nixpkgs");
    let rev = field("rev")?;
    (public && rev.len() == 40).then(|| rev.to_string())
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

/// Reads `nix derivation show -r` as it prints it. The closure of a system is
/// hundreds of megabytes of JSON and a few thousand distinct packages: only
/// the packages stay. Evaluation is enough; nothing is built.
fn derivation_closure(installable: &[String], found: &mut Closure) -> Result<()> {
    let mut args: Vec<String> = vec!["derivation".into(), "show".into(), "-r".into()];
    args.extend_from_slice(installable);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    stream_with(NIX, &refs, |out| {
        found.read(serde_json::Deserializer::from_reader(out))
    })
}

/// Reads the closure in one pass and keeps only distinct packages; it stops
/// past `max` of them.
pub(crate) fn closure_pairs_at_most(text: &[u8], max: usize) -> Result<Vec<(String, String)>> {
    let mut found = Closure::new(max);
    found.read(serde_json::Deserializer::from_slice(text))?;
    Ok(found.pairs.into_keys().collect())
}

/// The paths of one package that `why` tries. More would only repeat the
/// same answer for a package that a closure builds many times.
const MAX_PATHS: usize = 4;

#[derive(Default)]
struct Seen {
    outputs: BTreeSet<usize>,
    paths: Vec<String>,
}

struct Closure {
    max: usize,
    pairs: BTreeMap<(String, String), Seen>,
    /// The output that the scan reads now.
    output: usize,
}

/// nix 2.35 names a derivation without the store directory.
fn store_path(path: &str) -> String {
    if path.starts_with('/') {
        return path.to_string();
    }
    let dir = std::env::var("NIX_STORE_DIR").unwrap_or_else(|_| "/nix/store".into());
    format!("{}/{path}", dir.trim_end_matches('/'))
}

impl Closure {
    fn new(max: usize) -> Self {
        Closure {
            max,
            pairs: BTreeMap::new(),
            output: 0,
        }
    }

    fn insert(&mut self, pname: String, version: String, path: &str) {
        let seen = self.pairs.entry((pname, version)).or_default();
        seen.outputs.insert(self.output);
        let path = store_path(path);
        if seen.paths.len() < MAX_PATHS && !seen.paths.contains(&path) {
            seen.paths.push(path);
        }
    }

    /// Reads a map of derivations, from a file in memory or from the pipe of a
    /// running nix, and adds its packages to the set.
    fn read<'de, R: serde_json::de::Read<'de>>(
        &mut self,
        mut de: serde_json::Deserializer<R>,
    ) -> Result<()> {
        TopLevel(self)
            .deserialize(&mut de)
            .and_then(|()| de.end())
            .context("parse derivation show output")
    }

    /// A built store path, which carries the name and nothing else.
    fn add_path(&mut self, path: &str) {
        let (name, version) = split_name(&strip_store_path(path));
        if is_package(&name, &version, false) {
            self.insert(name, version, path);
        }
    }

    /// The set holds what the server takes, and no more.
    fn room(&self) -> Result<()> {
        if self.pairs.len() > self.max {
            bail!("more than {} packages", self.max);
        }
        Ok(())
    }

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
            self.insert(pname, version, path);
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
fn runtime_closure(installable: &[String], found: &mut Closure) -> Result<()> {
    let mut args: Vec<String> = vec!["path-info".into(), "-r".into()];
    args.extend_from_slice(installable);
    for line in nix(&args)?.lines().filter(|l| !l.is_empty()) {
        found.add_path(line);
    }
    found.room()
}

/// The packages of every output, as one set. Each output is its own nix call:
/// one that does not evaluate on this machine is named and skipped, and the
/// others still make a report. It fails only when no output could be read.
fn closure_of(target: &Target, installables: &[(String, Vec<String>)]) -> Result<Closure> {
    let mut found = Closure::new(MAX_PACKAGES);
    let mut skipped = 0;
    for (n, (name, args)) in installables.iter().enumerate() {
        found.output = n;
        let before = found.pairs.len();
        let read = if target.runtime {
            runtime_closure(args, &mut found)
        } else {
            derivation_closure(args, &mut found)
        };
        match read {
            // the cap is about the whole scan, so it stops it
            Err(e) if found.pairs.len() > found.max => {
                return Err(e.context("too many packages for a report; name fewer with --output"));
            }
            Err(e) if !target.discovered => {
                return Err(e.context(format!("read {name}")));
            }
            Err(e) => {
                skipped += 1;
                eprintln!("skip {name}: {e:#}");
            }
            Ok(()) => eprintln!(
                "{name}: {} packages, {} new",
                found.pairs.len(),
                found.pairs.len() - before
            ),
        }
    }
    if skipped > 0 {
        eprintln!(
            "{} of {} outputs read, {skipped} skipped",
            installables.len() - skipped,
            installables.len()
        );
    }
    if skipped == installables.len() && skipped > 0 {
        bail!("no output could be read");
    }
    Ok(found)
}

/// The short name of an output: the configuration name for a system or a
/// home, the attribute path without the system for the rest, the file name
/// for a whole file. Any character that a label cannot hold becomes `_`.
fn label(output: &str, whole_file: bool) -> String {
    let segments = crate::trace::attr_path(output);
    let text = match segments.as_slice() {
        _ if whole_file => Path::new(output)
            .file_name()
            .map_or_else(|| output.to_string(), |f| f.to_string_lossy().into_owned()),
        [kind, name, ..]
            if matches!(
                kind.as_str(),
                "nixosConfigurations" | "darwinConfigurations" | "homeConfigurations"
            ) =>
        {
            name.clone()
        }
        [kind, system, rest @ ..]
            if !rest.is_empty()
                && matches!(
                    kind.as_str(),
                    "packages" | "devShells" | "checks" | "apps" | "legacyPackages"
                )
                && system.contains('-') =>
        {
            format!("{kind}.{}", rest.join("."))
        }
        _ => segments.join("."),
    };
    let mut clean: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | '+') {
                c
            } else {
                '_'
            }
        })
        .collect();
    clean.truncate(crate::report::MAX_LABEL);
    if clean.is_empty() {
        clean.push('_');
    }
    clean
}

pub fn scan(target: &Target) -> Result<Report> {
    if target.flake.is_none() && target.file.is_none() {
        bail!("name a flake with --flake or a file with --file");
    }
    if target.flake.is_some() && target.outputs.is_empty() {
        bail!("no output to read; name one with --output");
    }
    let installables = target.installables();
    let found = closure_of(target, &installables)?;
    let whole_file = target.file.is_some() && target.outputs.is_empty();
    let outputs = installables
        .into_iter()
        .map(|(name, installable)| ScanOutput {
            label: label(&name, whole_file),
            installable,
        })
        .collect();

    let system = target.system.clone().unwrap_or_else(this_system);
    let packages = found
        .pairs
        .into_iter()
        .filter(|((p, _), _)| !p.is_empty())
        .map(|((pname, version), seen)| Found {
            pname,
            version,
            attr: None,
            outputs: seen.outputs.into_iter().collect(),
            paths: seen.paths,
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
        files: None,
        outputs,
    })
}

/// Prints why each output of the report holds the package: the chain that
/// `nix why-depends` finds from the output to a path of the package. `only`
/// is a label. It reads the local report and runs nix here; nothing goes
/// to a server.
pub fn why(report: &Report, pname: &str, only: Option<&str>) -> Result<()> {
    let found: Vec<&Found> = report
        .packages
        .iter()
        .filter(|f| f.pname == pname)
        .collect();
    if found.is_empty() {
        bail!("{pname} is not in the report");
    }
    if report.outputs.is_empty() || found.iter().all(|f| f.paths.is_empty()) {
        bail!("the report has no outputs or paths; scan again with this version");
    }
    let labels: BTreeSet<&str> = found
        .iter()
        .flat_map(|f| &f.outputs)
        .filter_map(|i| report.outputs.get(*i))
        .map(|o| o.label.as_str())
        .collect();
    if let Some(o) = only.filter(|o| !labels.contains(o)) {
        bail!(
            "{pname} is not in {o}; it is in {}",
            labels.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    let mut shown = 0;
    for (i, out) in report.outputs.iter().enumerate() {
        if only.is_some_and(|o| o != out.label) {
            continue;
        }
        let here: Vec<&&Found> = found.iter().filter(|f| f.outputs.contains(&i)).collect();
        if here.is_empty() {
            continue;
        }
        let derivation = here
            .iter()
            .flat_map(|f| &f.paths)
            .any(|p| p.ends_with(".drv"));
        let mut args = vec!["path-info".to_string()];
        if derivation {
            args.push("--derivation".into());
        }
        args.extend(out.installable.iter().cloned());
        let top = match nix(&args) {
            Ok(t) => t.lines().next().unwrap_or_default().to_string(),
            Err(e) => {
                eprintln!("skip {}: {e:#}", out.label);
                continue;
            }
        };
        for f in here {
            let chain = f.paths.iter().find_map(|p| {
                let mut args = vec!["why-depends".to_string()];
                if p.ends_with(".drv") {
                    args.push("--derivation".into());
                }
                args.extend([top.clone(), p.clone()]);
                nix(&args).ok().filter(|t| !t.trim().is_empty())
            });
            match chain {
                Some(c) => {
                    println!(
                        "{} holds {}-{}:\n{}",
                        out.label,
                        f.pname,
                        f.version,
                        c.trim_end()
                    );
                    shown += 1;
                }
                None => eprintln!(
                    "{}: no chain to {}-{}; the output changed since the scan?",
                    out.label, f.pname, f.version
                ),
            }
        }
    }
    if shown == 0 {
        bail!("no chain found; scan again");
    }
    Ok(())
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
    fn only_a_github_nixpkgs_pin_is_public() {
        let rev = "7fd4004d0511991fff3a0524856a9be09e28c5f0";
        let lock = |owner: &str, kind: &str| {
            serde_json::json!({ "nodes": {
                "root": { "inputs": { "nixpkgs": "nixpkgs" } },
                "nixpkgs": { "locked": { "type": kind, "owner": owner, "repo": "nixpkgs", "rev": rev } }
            }})
        };
        assert_eq!(
            locked_nixpkgs_in(&lock("NixOS", "github"), "nixpkgs").as_deref(),
            Some(rev)
        );
        assert_eq!(locked_nixpkgs_in(&lock("me", "github"), "nixpkgs"), None);
        assert_eq!(locked_nixpkgs_in(&lock("NixOS", "path"), "nixpkgs"), None);
        assert_eq!(locked_nixpkgs_in(&lock("NixOS", "github"), "other"), None);
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
            let pairs = closure_pairs_at_most(v.to_string().as_bytes(), usize::MAX).unwrap();
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
        assert!(closure_pairs_at_most(b"{} trailing", usize::MAX).is_err());
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
            ignore: Vec::new(),
            discovered: false,
        };
        assert_eq!(t(".").root(), Some(PathBuf::from(".")));
        assert_eq!(
            t("path:/srv/tree?x=1").root(),
            Some(PathBuf::from("/srv/tree"))
        );
        assert_eq!(t("github:o/r").root(), None);
        assert_eq!(
            t(".").installables(),
            vec![("x".to_string(), vec![".#x".to_string()])]
        );
    }

    #[test]
    fn a_discovered_name_that_is_not_plain_keeps_its_quotes() {
        let path = |s: &[&str]| attr_path(&s.iter().map(|p| (*p).to_string()).collect::<Vec<_>>());
        assert_eq!(
            path(&["packages", "x86_64-linux", "hello"]),
            "packages.x86_64-linux.hello"
        );
        assert_eq!(
            path(&["homeConfigurations", "me@box", "activationPackage"]),
            "homeConfigurations.\"me@box\".activationPackage"
        );
        assert_eq!(path(&["checks", "a.b"]), "checks.\"a.b\"");
    }

    #[test]
    fn ignore_leaves_one_output_or_every_output_under_a_path() {
        let ignore = vec![
            "checks".to_string(),
            "packages.x86_64-linux.broken".to_string(),
            "homeConfigurations.me@box".to_string(),
        ];
        assert!(ignored("checks.x86_64-linux.vm", &ignore));
        assert!(ignored("checks", &ignore));
        assert!(ignored("packages.x86_64-linux.broken", &ignore));
        // the quotes of an installable are not part of the name
        assert!(ignored(
            "homeConfigurations.\"me@box\".activationPackage",
            &ignore
        ));
        // a path is whole: checks does not leave checksummer
        assert!(!ignored("checksummer.x86_64-linux.vm", &ignore));
        assert!(!ignored("packages.x86_64-linux.broken-not", &ignore));
        assert!(!ignored("packages.x86_64-linux.hello", &ignore));
        assert!(!ignored("packages.x86_64-linux.hello", &[]));
        assert!(!ignored("checks.\"a.b\"", &["packages.a".to_string()]));
        assert!(!ignored("packages.\"a.b\"", &["packages.a".to_string()]));
        assert!(ignored(
            "packages.\"a.b\"",
            &["packages.\"a.b\"".to_string()]
        ));
    }

    #[test]
    fn ignoring_every_attribute_of_a_file_does_not_read_the_whole_file() {
        let mut t = Target {
            flake: None,
            file: Some(PathBuf::from("host.nix")),
            outputs: vec!["top".into()],
            nixpkgs_input: None,
            system: None,
            runtime: false,
            ignore: vec!["top".into()],
            discovered: false,
        };
        assert!(t.choose_outputs().is_err());
    }

    #[test]
    fn every_output_is_its_own_call_and_a_file_without_an_attribute_is_one() {
        let target = |outputs: Vec<String>| Target {
            flake: None,
            file: Some(PathBuf::from("host.nix")),
            outputs,
            nixpkgs_input: None,
            system: None,
            runtime: false,
            ignore: Vec::new(),
            discovered: false,
        };
        let calls = target(vec!["top".into(), "other".into()]).installables();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "top");
        assert_eq!(calls[0].1, vec!["--file", "host.nix", "top"]);
        assert_eq!(calls[1].1, vec!["--file", "host.nix", "other"]);
        let whole = target(Vec::new()).installables();
        assert_eq!(whole.len(), 1);
        assert_eq!(whole[0].1, vec!["--file", "host.nix"]);
    }

    #[test]
    fn the_set_takes_store_paths_and_stops_past_the_cap() {
        let mut found = Closure::new(1);
        found.add_path("/nix/store/2x905q8m3w6wj9kh1k2agzqx9x0ksapl-zlib-1.3.1");
        // the same package twice is one entry: the set unions as it reads
        found.add_path("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-zlib-1.3.1");
        found.add_path("/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-stdenv-linux");
        assert_eq!(found.pairs.len(), 1);
        assert!(found.room().is_ok());
        found.add_path("/nix/store/cccccccccccccccccccccccccccccccc-curl-8.11.1");
        assert!(found.room().is_err());
    }

    #[test]
    fn a_label_is_the_host_or_the_path_without_the_system() {
        for (output, want) in [
            (
                "nixosConfigurations.web-1.config.system.build.toplevel",
                "web-1",
            ),
            (
                "darwinConfigurations.mac.config.system.build.toplevel",
                "mac",
            ),
            ("homeConfigurations.\"me@box\".activationPackage", "me@box"),
            ("devShells.x86_64-linux.default", "devShells.default"),
            ("checks.aarch64-darwin.\"a.b\"", "checks.a.b"),
            ("packages.x86_64-linux.hello", "packages.hello"),
            ("top", "top"),
            ("nixosConfigurations.\"a b/c\".config", "a_b_c"),
        ] {
            assert_eq!(label(output, false), want, "{output}");
            assert_eq!(crate::report::check_label(&label(output, false)), Ok(()));
        }
        assert_eq!(label("/home/me/hosts/web.nix", true), "web.nix");
        assert_eq!(
            label(&"x".repeat(100), false).len(),
            crate::report::MAX_LABEL
        );
    }

    #[test]
    fn each_package_keeps_its_outputs_and_a_few_paths() {
        let mut found = Closure::new(usize::MAX);
        found.add_path("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-zlib-1.3.1");
        found.output = 2;
        let drv = serde_json::json!({ "derivations": {
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-zlib-1.3.1.drv": { "name": "zlib-1.3.1", "env": { "pname": "zlib", "version": "1.3.1" } }
        }, "version": 4 });
        found
            .read(serde_json::Deserializer::from_slice(
                drv.to_string().as_bytes(),
            ))
            .unwrap();
        let seen = &found.pairs[&("zlib".to_string(), "1.3.1".to_string())];
        assert_eq!(seen.outputs.iter().copied().collect::<Vec<_>>(), [0, 2]);
        assert_eq!(
            seen.paths,
            [
                "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-zlib-1.3.1",
                "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-zlib-1.3.1.drv"
            ]
        );
        for n in 0..10 {
            found.add_path(&format!("/nix/store/{n:032}-zlib-1.3.1"));
        }
        assert_eq!(found.pairs.values().next().unwrap().paths.len(), MAX_PATHS);
    }
}
