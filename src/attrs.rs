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
}

/// Cheap facts for packaging hygiene and for asking upstream directly. Each
/// is read on its own, so one that fails to evaluate keeps the package.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Facts {
    /// The first 200 characters.
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
  facts = p: meta: {
    description = safe null (
      let
        d = meta.description or null;
      in
      if builtins.isString d && d != "" then builtins.substring 0 200 d else null
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
    Ok(serde_json::from_str(&text)?)
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
        };
        let entries = [e("utillinux"), e("util-linux"), e("pkgsStatic.util-linux")];
        let i = Index::build(entries.iter());
        assert_eq!(i.resolve("util-linux"), Some("util-linux"));
    }
}
