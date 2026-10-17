//! nixpkgs swept once per revision: every derivation with its package name,
//! the file that defines it, and its maintainers' GitHub logins. Taken from
//! nixfresh (MIT); the sweep expression also reads `meta.position` and
//! `meta.maintainers`, so paths map to packages and to people in one eval.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::eval::eval_with_input;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub attr: String,
    #[serde(default)]
    pub pname: String,
    #[serde(default)]
    pub version: String,
    /// Relative to the nixpkgs root, without the line number.
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub maintainers: Vec<String>,
    /// `shortName` of each team in `meta.teams`.
    #[serde(default)]
    pub teams: Vec<String>,
    /// `None` in an index swept before these were read.
    #[serde(default)]
    pub facts: Option<Facts>,
    #[serde(default)]
    pub meta: Option<Meta>,
}

/// Cheap facts for packaging hygiene and for asking upstream directly. Each
/// is read on its own, so one that fails to evaluate keeps the package.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Facts {
    /// The first 500 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub homepage: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub license: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub main_program: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub version_check: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub broken: bool,
    /// `meta.knownVulnerabilities` is not empty.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub insecure: bool,
    /// The first URL of `src`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src_url: Option<String>,
}

/// The rest of `meta`, for the package page. Lists stay short: `platforms`
/// is a summary, not the list.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    /// The first 2000 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub homepage: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_page: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changelog: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub licenses: Vec<License>,
    /// `None` when `meta.platforms` is not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Platforms>,
    /// The first 10; a pattern that is not a system string is `pattern`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bad_platforms: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_program: Option<String>,
    /// `shortName` of each entry of `meta.sourceProvenance`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_provenance: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub known_vulnerabilities: Vec<String>,
    /// `file:line`, relative to the nixpkgs root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct License {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spdx: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default = "free_by_default")]
    pub free: bool,
}

fn free_by_default() -> bool {
    true
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Platforms {
    pub count: usize,
    /// x86_64 or aarch64.
    pub linux: bool,
    pub darwin: bool,
}

impl Meta {
    pub fn unfree(&self) -> bool {
        self.licenses.iter().any(|l| !l.free)
    }

    pub fn file_line(&self) -> Option<(&str, u32)> {
        let (file, line) = self.position.as_deref()?.rsplit_once(':')?;
        Some((file, line.parse().ok()?))
    }
}

/// nixpkgs keyed by package name instead of by attribute path.
#[derive(Debug, Default)]
pub struct Index {
    by_pname: HashMap<String, Vec<String>>,
}

/// One pname can own many attributes: `util-linux` is `eject`, `logger`, `more`,
/// `mount`, `utillinux` and `util-linux` at once. The attribute that spells the
/// package name is the one the security tracker and `meta.maintainers` are
/// written against, so name agreement outranks everything; depth and length only
/// break a tie between equally good names.
fn rank(pname: &str, attr: &str) -> (u8, usize, usize) {
    let leaf = attr.rsplit('.').next().unwrap_or(attr);
    let squash = |s: &str| {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect::<String>()
    };
    let class = if leaf == pname {
        0
    } else if leaf.eq_ignore_ascii_case(pname) {
        1
    } else if squash(leaf) == squash(pname) {
        2
    } else {
        3
    };
    (class, attr.matches('.').count(), attr.len())
}

impl Index {
    pub fn build<'a>(entries: impl IntoIterator<Item = &'a Entry>) -> Self {
        let mut index = Self::default();
        for e in entries {
            if e.pname.is_empty() {
                continue;
            }
            index
                .by_pname
                .entry(e.pname.clone())
                .or_default()
                .push(e.attr.clone());
        }
        for (pname, attrs) in index.by_pname.iter_mut() {
            attrs.sort_by(|a, b| rank(pname, a).cmp(&rank(pname, b)).then(a.cmp(b)));
        }
        index
    }

    pub fn resolve(&self, pname: &str) -> Option<&str> {
        self.by_pname
            .get(pname)
            .and_then(|attrs| attrs.first())
            .map(String::as_str)
    }
}

const INDEX_EXPR: &str = r#"
let
  input = builtins.fromJSON (builtins.readFile ./input.json);
  pkgs = import input.path {
    inherit (input) system;
    overlays = [ ];
    config = {
      allowUnfree = true;
      allowBroken = true;
      allowInsecurePredicate = _: true;
    };
  };
  lib = pkgs.lib;
  root = if input.set == "" then pkgs else lib.attrByPath (lib.splitString "." input.set) null pkgs;
  prefix = if input.set == "" then "" else input.set + ".";
  relative =
    pos:
    let
      file = builtins.head (lib.splitString ":" pos);
    in
    lib.removePrefix (input.path + "/") file;
  safe =
    default: v:
    let
      r = builtins.tryEval (builtins.deepSeq v v);
    in
    if r.success then r.value else default;
  hook = d: builtins.isAttrs d && (d.name or null) == "version-check-hook";
  # substring counts bytes: a cut inside a UTF-8 character fails the JSON
  # output of the whole set, past tryEval, so Rust cuts on characters
  bounded = s: if builtins.stringLength s <= 65536 then s else null;
  facts = p: meta: {
    description = safe null (
      let
        d = meta.description or null;
      in
      if builtins.isString d && d != "" then bounded d else null
    );
    homepage = safe false (meta ? homepage);
    license = safe false (meta ? license);
    main_program = safe false (meta ? mainProgram);
    # the inputs are read only when the install check runs: forcing every
    # package's inputs costs 500 MB more
    version_check = safe false (
      (p.doInstallCheck or false) == true
      && builtins.any hook ((p.nativeBuildInputs or [ ]) ++ (p.nativeInstallCheckInputs or [ ]))
    );
    broken = safe false ((meta.broken or false) == true);
    insecure = safe false ((meta.knownVulnerabilities or [ ]) != [ ]);
    src_url = safe null (
      let
        one = p.src.url or "";
        urls = p.src.urls or [ ];
        u =
          if builtins.isString one && one != "" then
            one
          else if builtins.isList urls && urls != [ ] then
            builtins.head urls
          else
            null;
      in
      if builtins.isString u && u != "" then u else null
    );
  };
  text = v: if builtins.isString v && v != "" then v else null;
  urls =
    v:
    if builtins.isString v then
      [ v ]
    else if builtins.isList v then
      builtins.filter builtins.isString v
    else
      [ ];
  license =
    l:
    if builtins.isAttrs l then
      {
        spdx = l.spdxId or null;
        name = l.shortName or l.fullName or null;
        free = (l.free or true) == true;
      }
    else if builtins.isString l then
      {
        spdx = null;
        name = l;
        free = true;
      }
    else
      null;
  # only what is already in meta: no output path, no drvPath
  meta' = p: meta: {
    long_description = safe null (
      let
        d = text (meta.longDescription or null);
      in
      if d == null then null else bounded d
    );
    homepage = safe [ ] (urls (meta.homepage or [ ]));
    download_page = safe null (text (meta.downloadPage or null));
    changelog = safe [ ] (urls (meta.changelog or [ ]));
    licenses = safe [ ] (
      builtins.filter (l: l != null) (
        map license (lib.toList (meta.license or [ ]))
      )
    );
    platforms = safe null (
      let
        ps = meta.platforms or null;
      in
      if builtins.isList ps then
        {
          count = builtins.length ps;
          linux = builtins.elem "x86_64-linux" ps || builtins.elem "aarch64-linux" ps;
          darwin = builtins.elem "aarch64-darwin" ps || builtins.elem "x86_64-darwin" ps;
        }
      else
        null
    );
    bad_platforms = safe [ ] (
      map (x: if builtins.isString x then x else "pattern") (lib.take 10 (meta.badPlatforms or [ ]))
    );
    main_program = safe null (text (meta.mainProgram or null));
    source_provenance = safe [ ] (map (s: s.shortName or "unknown") (meta.sourceProvenance or [ ]));
    outputs = safe [ ] (builtins.filter builtins.isString (p.outputs or [ ]));
    known_vulnerabilities = safe [ ] (
      builtins.filter builtins.isString (meta.knownVulnerabilities or [ ])
    );
    position = safe null (
      if builtins.isString (meta.position or null) then
        lib.removePrefix (input.path + "/") meta.position
      else
        null
    );
  };
  entry =
    name:
    let
      value = builtins.tryEval (
        let
          p = builtins.getAttr name root;
          meta = p.meta or { };
          parsed = builtins.parseDrvName (p.name or "");
          out = {
            attr = prefix + name;
            pname = p.pname or parsed.name;
            version = p.version or parsed.version;
            file = if meta ? position then relative meta.position else null;
            maintainers = builtins.filter (x: x != null) (
              map (m: m.github or null) (meta.maintainers or [ ])
            );
            teams = map (t: t.shortName or "") (meta.teams or [ ]);
            facts = facts p meta;
            meta = meta' p meta;
          };
        in
        if !(lib.isDerivation p) then null else builtins.deepSeq out out
      );
    in
    if value.success then value.value else null;
in
if !(builtins.isAttrs root) then
  [ ]
else
  builtins.filter (x: x != null) (map entry (builtins.attrNames root))
"#;

/// One eval per set keeps the peak at one set's worth of nixpkgs: top level
/// alone is 25k derivations and 2 GB.
pub fn sweep(nixpkgs: &Path, system: &str, set: &str) -> Result<Vec<Entry>> {
    let input = serde_json::json!({
        "path": nixpkgs.to_string_lossy(),
        "system": system,
        "set": set,
    });
    let text = eval_with_input("index", INDEX_EXPR, &input)?;
    let mut entries: Vec<Entry> = serde_json::from_str(&text)?;
    for e in &mut entries {
        e.shorten();
    }
    Ok(entries)
}

impl Entry {
    fn shorten(&mut self) {
        if let Some(d) = self.facts.as_mut().and_then(|f| f.description.as_mut()) {
            cut(d, 500);
        }
        if let Some(d) = self.meta.as_mut().and_then(|m| m.long_description.as_mut()) {
            cut(d, 2000);
        }
    }
}

fn cut(s: &mut String, chars: usize) {
    if let Some((i, _)) = s.char_indices().nth(chars) {
        s.truncate(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_agreement_outranks_depth() {
        let e = |attr: &str| Entry {
            attr: attr.into(),
            pname: "util-linux".into(),
            version: String::new(),
            file: None,
            maintainers: vec![],
            teams: vec![],
            facts: None,
            meta: None,
        };
        let entries = [e("utillinux"), e("util-linux"), e("pkgsStatic.util-linux")];
        let i = Index::build(entries.iter());
        assert_eq!(i.resolve("util-linux"), Some("util-linux"));
    }

    #[test]
    fn long_texts_are_cut_on_characters() {
        let mut e: Entry =
            serde_json::from_str(r#"{"attr":"x","facts":{"description":"short é"},"meta":{}}"#)
                .unwrap();
        e.meta.as_mut().unwrap().long_description = Some("é".repeat(2001));
        e.shorten();
        let long = e.meta.unwrap().long_description.unwrap();
        assert_eq!(long.chars().count(), 2000);
        assert_eq!(long.len(), 4000);
        assert_eq!(e.facts.unwrap().description.as_deref(), Some("short é"));
    }

    #[test]
    fn meta_reads_from_the_sweep_and_an_old_entry_still_loads() {
        let old: Entry = serde_json::from_str(r#"{"attr":"hello","pname":"hello"}"#).unwrap();
        assert_eq!(old.meta, None);
        let e: Entry = serde_json::from_str(
            r#"{"attr":"hello","meta":{"bad_platforms":[],"download_page":null,
            "licenses":[{"free":true,"name":"gpl3Plus","spdx":"GPL-3.0-or-later"},{"free":false,"name":"unfree","spdx":null}],
            "platforms":{"count":79,"darwin":true,"linux":true},
            "position":"pkgs/by-name/he/hello/package.nix:58"}}"#,
        )
        .unwrap();
        let m = e.meta.unwrap();
        assert!(m.unfree());
        assert_eq!(
            m.file_line(),
            Some(("pkgs/by-name/he/hello/package.nix", 58))
        );
        let stored = serde_json::to_string(&m).unwrap();
        assert!(!stored.contains("download_page") && !stored.contains("bad_platforms"));
    }
}
