//! A closure's inventory, in the nixfresh report format (MIT): the client
//! writes it, redacts it and submits it; the server checks it again. Only a
//! package with a nixpkgs attribute leaves the owner's machine.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

pub const SCHEMA: u32 = 1;
pub const MAX_PACKAGES: usize = 50_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub schema: u32,
    #[serde(default)]
    pub client: String,
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub source: Source,
    pub packages: Vec<Found>,
    #[serde(default)]
    pub omitted: Omitted,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Source {
    /// What was scanned: a flake reference or a file. It may name a private
    /// tree, so redaction empties it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default)]
    pub system: String,
    #[serde(default)]
    pub nixpkgs_rev: Option<String>,
    /// When the closure was evaluated, so a stale report reads as stale.
    #[serde(default)]
    pub scanned_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Found {
    pub pname: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub attr: Option<String>,
    /// `attr` when a nixpkgs attribute matched. nixfresh also writes `flake`,
    /// `vendored` and `absent`; none of them leaves the client.
    #[serde(default)]
    pub resolution: String,
}

/// Names left out by the redaction, counted and not listed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Omitted {
    #[serde(default)]
    pub flake: usize,
    #[serde(default)]
    pub vendored: usize,
    #[serde(default)]
    pub absent: usize,
    #[serde(default)]
    pub unknown: usize,
}

impl Found {
    fn public(&self) -> bool {
        self.resolution == "attr" && self.attr.as_deref().is_some_and(|a| !a.is_empty())
    }
}

impl Report {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read report {}", path.display()))?;
        let report: Report = serde_json::from_str(&text).context("parse report")?;
        report.check()?;
        Ok(report)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("write {}", path.display()))
    }

    pub fn check(&self) -> Result<()> {
        if self.schema != SCHEMA {
            bail!("report schema {} is not {SCHEMA}", self.schema);
        }
        Ok(())
    }

    /// What may leave the machine: packages with a nixpkgs attribute. The rest
    /// is counted, and the source name goes, because a path or a flake
    /// reference can name a private tree.
    pub fn redacted(&self) -> Self {
        let mut out = self.clone();
        out.source.name.clear();
        let mut omitted = self.omitted.clone();
        out.packages.retain(|p| {
            if p.public() {
                return true;
            }
            match p.resolution.as_str() {
                "flake" => omitted.flake += 1,
                "vendored" => omitted.vendored += 1,
                "absent" => omitted.absent += 1,
                _ => omitted.unknown += 1,
            }
            false
        });
        out.omitted = omitted;
        out
    }

    /// The server's side of the same rule: it keeps an attribute only when
    /// its own nixpkgs index knows it, so a buggy or hostile client cannot
    /// store a private name. Returns the kept attributes and how many entries
    /// were dropped.
    pub fn accepted(&self, known: impl Fn(&str) -> bool) -> (BTreeSet<String>, usize) {
        let mut kept = BTreeSet::new();
        let mut dropped = 0;
        for p in &self.packages {
            match p.attr.as_deref() {
                Some(a) if known(a) => {
                    kept.insert(a.to_string());
                }
                _ => dropped += 1,
            }
        }
        (kept, dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(pname: &str, attr: Option<&str>) -> Found {
        Found {
            pname: pname.into(),
            version: "1".into(),
            attr: attr.map(str::to_string),
            resolution: if attr.is_some() { "attr" } else { "unknown" }.into(),
        }
    }

    fn report(packages: Vec<Found>) -> Report {
        Report {
            schema: SCHEMA,
            client: "maintainer-mode/test".into(),
            generated_at: "2026-10-01T00:00:00Z".into(),
            source: Source {
                name: "/home/me/private-tree".into(),
                system: "x86_64-linux".into(),
                nixpkgs_rev: None,
                scanned_at: None,
            },
            packages,
            omitted: Omitted::default(),
        }
    }

    #[test]
    fn redaction_keeps_attributes_and_counts_the_rest() {
        let mut r = report(vec![found("zlib", Some("zlib")), found("our-daemon", None)]);
        r.packages.push(Found {
            pname: "own".into(),
            version: "1".into(),
            attr: Some("packages.x86_64-linux.own".into()),
            resolution: "flake".into(),
        });
        let out = r.redacted();
        let names: Vec<&str> = out.packages.iter().map(|p| p.pname.as_str()).collect();
        assert_eq!(names, vec!["zlib"]);
        assert_eq!(out.omitted.unknown, 1);
        assert_eq!(out.omitted.flake, 1);
        assert!(out.source.name.is_empty());
        let text = serde_json::to_string(&out).unwrap();
        assert!(!text.contains("private-tree") && !text.contains("our-daemon"));
    }

    #[test]
    fn the_server_keeps_only_attributes_its_index_knows() {
        let r = report(vec![
            found("zlib", Some("zlib")),
            found("zlib", Some("zlib")),
            found("secret", Some("secret")),
            found("other", None),
        ]);
        let (kept, dropped) = r.accepted(|a| a == "zlib");
        assert_eq!(kept.into_iter().collect::<Vec<_>>(), vec!["zlib"]);
        assert_eq!(dropped, 2);
    }

    #[test]
    fn a_nixfresh_report_parses_and_a_wrong_schema_does_not() {
        let text = r#"{"schema":1,"client":"nixfresh/0.1.0","generated_at":"x",
            "source":{"name":"web","system":"x86_64-linux","nixpkgs_rev":null,"scanned_at":null,"drift":0},
            "packages":[{"pname":"7zz","version":"26.02","attr":"_7zz","resolution":"attr","reach":"runtime",
            "origin":null,"position":null,"src_url":null,"homepage":null,"license":null,"insecure":false,
            "broken":false,"known_vulnerabilities":[],"maintainers":[]}],
            "omitted":{"flake":0,"vendored":0,"absent":3,"unknown":0}}"#;
        let r: Report = serde_json::from_str(text).unwrap();
        assert!(r.check().is_ok());
        assert_eq!(r.packages[0].attr.as_deref(), Some("_7zz"));
        let mut r = r;
        r.schema = 2;
        assert!(r.check().is_err());
    }
}
