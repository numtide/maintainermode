//! Experimental, local only: the nixpkgs files that the scanned outputs read.
//! iets records every file an evaluation reads, for its evaluation cache; the
//! scan reads that record back. A NixOS evaluation imports every module of
//! `module-list.nix`, so a module file stays only when the module signal of
//! `trace.nix` keeps it. Nothing here goes into the upload yet; see
//! `docs/file-matching.md`.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::area::{area_key, under};
use crate::eval::{private_dir, run_plain};
use crate::scan::{Target, locked_nixpkgs};

const IETS: &str = "iets";
const EXPR: &str = include_str!("trace.nix");
const MAX_HOPS: usize = 8;
const MODULES: &str = "nixos/modules/";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileTrace {
    /// Paths relative to the nixpkgs root, sorted. A file is a file the
    /// evaluation read; a directory is a tree it copied to the store.
    pub files: Vec<String>,
    /// The module files of the module signal.
    #[serde(default)]
    pub modules: Vec<String>,
    /// Module files in the trace that the module signal did not keep.
    #[serde(default)]
    pub modules_dropped: usize,
}

#[derive(Deserialize)]
struct Answer {
    nixpkgs: String,
    outputs: Vec<OutputAnswer>,
}

#[derive(Deserialize)]
struct OutputAnswer {
    #[serde(default)]
    modules: Option<Signal>,
}

#[derive(Deserialize)]
struct Signal {
    files: Vec<String>,
}

pub fn trace(target: &Target) -> Result<FileTrace> {
    if target.file.is_some() {
        bail!("--files reads flakes only");
    }
    let Some(root) = target.local_root() else {
        bail!("--files needs a local flake");
    };
    let root = std::fs::canonicalize(&root)
        .with_context(|| format!("find the flake at {}", root.display()))?;
    let input = target
        .nixpkgs_input_name(&root)
        .context("no nixpkgs input in flake.lock; name it with --nixpkgs-input")?;
    if locked_nixpkgs(&root, &input).is_none() {
        bail!(
            "the input {input} is not github:NixOS/nixpkgs locked to a commit; \
             its files may not be public, so --files keeps none"
        );
    }
    let outputs: Vec<Vec<String>> = target.outputs.iter().map(|o| attr_path(o)).collect();

    let dir = private_dir("trace")?;
    let result = run(&dir, &root, &input, &outputs);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn run(dir: &Path, root: &Path, input: &str, outputs: &[Vec<String>]) -> Result<FileTrace> {
    let request = serde_json::json!({
        "flake": root.to_string_lossy(),
        "nixpkgs": input,
        "maxHops": MAX_HOPS,
        "outputs": outputs,
    });
    std::fs::write(dir.join("input.json"), serde_json::to_vec(&request)?)?;
    let expr = dir.join("trace.nix");
    std::fs::write(&expr, EXPR)?;
    let cache = dir.join("cache");
    let cache_arg = cache.to_string_lossy();
    let expr_arg = expr.to_string_lossy();
    let text = run_plain(
        IETS,
        &[
            "eval",
            "--impure",
            "--raw",
            "--cache-dir",
            &cache_arg,
            "--file",
            &expr_arg,
        ],
    )?;
    let answer: Answer = serde_json::from_str(&text).context("parse the trace answer")?;
    let entry = sole_entry(&cache.join("trace"))?;
    let dump = run_plain(IETS, &["cache", "dump", &entry.to_string_lossy()])?;
    let mut kept = BTreeSet::new();
    for o in &answer.outputs {
        for f in o.modules.iter().flat_map(|s| &s.files) {
            if is_clean(f) {
                kept.insert(f.clone());
            }
        }
    }
    let read = nixpkgs_paths(&dump, &answer.nixpkgs);
    Ok(select(read, kept))
}

/// The one entry of a cache directory that only this run used.
fn sole_entry(dir: &Path) -> Result<PathBuf> {
    let mut found = Vec::new();
    for e in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let p = e?.path().join("entry");
        if p.is_file() {
            found.push(p);
        }
    }
    match found.len() {
        1 => Ok(found.remove(0)),
        n => bail!("{n} trace entries in {}, expected one", dir.display()),
    }
}

/// The paths under `root` that the read set of `iets cache dump` names as a
/// file read or a tree copied. A `stat` or `dir` line only probes a path.
pub(crate) fn nixpkgs_paths(dump: &str, root: &str) -> BTreeSet<String> {
    let prefix = format!("{}/", root.trim_end_matches('/'));
    let mut out = BTreeSet::new();
    for line in dump.lines() {
        let path = if let Some(rest) = line.strip_prefix("file ") {
            let mut it = rest.rsplitn(3, ' ');
            let (_hash, _size) = (it.next(), it.next());
            it.next()
        } else if let Some(rest) = line.strip_prefix("tree ") {
            rest.rsplit_once(" accepted=").map(|(p, _)| p)
        } else {
            None
        };
        if let Some(rel) = path.and_then(|p| p.strip_prefix(prefix.as_str())) {
            if is_clean(rel) {
                out.insert(rel.to_string());
            }
        }
    }
    out
}

/// A plain relative path: no root, no `.` or `..` part, no control character.
fn is_clean(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 512
        && !p.starts_with('/')
        && p.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
        && !p.chars().any(char::is_control)
}

/// Keeps every path outside `nixos/modules/`, and a module path only inside
/// the area of a module that the signal kept.
pub(crate) fn select(read: BTreeSet<String>, kept: BTreeSet<String>) -> FileTrace {
    let areas: Vec<String> = kept.iter().map(|k| area_key(k)).collect();
    let mut dropped = 0;
    let files: Vec<String> = read
        .into_iter()
        .filter(|p| {
            let keep = !p.starts_with(MODULES) || areas.iter().any(|a| under(p, a));
            dropped += usize::from(!keep);
            keep
        })
        .collect();
    FileTrace {
        files,
        modules: kept.into_iter().collect(),
        modules_dropped: dropped,
    }
}

/// `a."b.c".d` is `["a", "b.c", "d"]`.
pub(crate) fn attr_path(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            '.' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/nix/store/r5l65vg4gi4dnx54zkwagfb8jmlqniy5-source";

    #[test]
    fn the_dump_gives_files_and_copied_trees_under_nixpkgs_only() {
        let dump = format!(
            "file /home/me/flake/flake.nix 556 7fab5e536213a09c\n\
             tree /home/me/flake accepted=all\n\
             stat {ROOT}/lib follow=true Directory\n\
             dir {ROOT}/pkgs/by-name 03c143850c9d26a3\n\
             file {ROOT}/lib/modules.nix 82375 dd298134bd2f7e88\n\
             file {ROOT}/pkgs/a b/x.nix 10 0000000000000000\n\
             tree {ROOT}/pkgs/servers/http/nginx/nix-etag-1.15.4.patch accepted=all\n\
             tree {ROOT}/pkgs/x/src accepted=3\n\
             file {ROOT}/../etc/passwd 1 0000000000000000\n"
        );
        let got: Vec<String> = nixpkgs_paths(&dump, ROOT).into_iter().collect();
        assert_eq!(
            got,
            vec![
                "lib/modules.nix",
                "pkgs/a b/x.nix",
                "pkgs/servers/http/nginx/nix-etag-1.15.4.patch",
                "pkgs/x/src",
            ]
        );
    }

    #[test]
    fn a_module_stays_only_inside_the_area_of_a_kept_module() {
        let read: BTreeSet<String> = [
            "lib/modules.nix",
            "nixos/modules/services/web-servers/nginx/default.nix",
            "nixos/modules/services/web-servers/nginx/vhost-options.nix",
            "nixos/modules/services/web-servers/apache-httpd/default.nix",
            "nixos/modules/security/acme/default.nix",
            "nixos/modules/module-list.nix",
            "pkgs/servers/http/nginx/generic.nix",
        ]
        .map(String::from)
        .into();
        let kept: BTreeSet<String> = [
            "nixos/modules/services/web-servers/nginx/default.nix",
            "nixos/modules/security/acme",
        ]
        .map(String::from)
        .into();
        let t = select(read, kept);
        assert_eq!(
            t.files,
            vec![
                "lib/modules.nix",
                "nixos/modules/security/acme/default.nix",
                "nixos/modules/services/web-servers/nginx/default.nix",
                "nixos/modules/services/web-servers/nginx/vhost-options.nix",
                "pkgs/servers/http/nginx/generic.nix",
            ]
        );
        assert_eq!(t.modules_dropped, 2);
    }

    #[test]
    fn an_attribute_path_splits_outside_quotes() {
        assert_eq!(
            attr_path("nixosConfigurations.\"web.example\".config"),
            vec!["nixosConfigurations", "web.example", "config"]
        );
        assert_eq!(attr_path("packages.x86_64-linux.hello").len(), 3);
    }

    #[test]
    fn a_path_must_be_plain_and_relative() {
        assert!(is_clean("pkgs/by-name/he/hello/package.nix"));
        for bad in ["", "/etc/passwd", "a/../b", "a//b", "./a", "a/\nb"] {
            assert!(!is_clean(bad), "{bad:?}");
        }
    }
}
