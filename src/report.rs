//! A closure's inventory.
//! What leaves the machine is the strict `Upload`: only public attributes, with
//! a version only when the public list has that exact version. The server
//! checks the same rules again.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::area::Index;

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
    /// Experimental, from `scan --files`. It stays on this machine: the upload
    /// does not carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<crate::trace::FileTrace>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Source {
    /// What was scanned. It may name a private tree, so it never leaves the
    /// machine.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default)]
    pub system: String,
    #[serde(default)]
    pub nixpkgs_rev: Option<String>,
    #[serde(default)]
    pub scanned_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Found {
    pub pname: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub version: String,
    /// A report can name the attribute. It counts only when the
    /// public list has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attr: Option<String>,
}

fn null_as_empty<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

/// The public package list of an index, as `GET /api/v1/packages/index`
/// sends it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PublicList {
    pub rev: String,
    /// attribute → (pname, version)
    pub attrs: BTreeMap<String, (String, String)>,
    /// pname → its attributes, the canonical one first
    #[serde(default)]
    pub pnames: BTreeMap<String, Vec<String>>,
}

impl PublicList {
    pub fn of(index: &Index) -> Self {
        let attrs: BTreeMap<String, (String, String)> = index
            .pkgs
            .iter()
            .map(|(a, p)| (a.clone(), (p.pname.clone(), p.version.clone())))
            .collect();
        let mut list = PublicList {
            rev: index.rev.clone(),
            attrs,
            pnames: BTreeMap::new(),
        };
        for (pname, attr) in &index.by_pname {
            if list.attrs.contains_key(attr) {
                list.pnames.insert(pname.clone(), vec![attr.clone()]);
            }
        }
        list.fill_pnames();
        list
    }

    /// Adds every attribute to the list of its pname, after the ones already
    /// there.
    pub fn fill_pnames(&mut self) {
        for (attr, (pname, _)) in &self.attrs {
            if pname.is_empty() {
                continue;
            }
            let v = self.pnames.entry(pname.clone()).or_default();
            if !v.contains(attr) {
                v.push(attr.clone());
            }
        }
        self.pnames
            .retain(|_, v| !v.is_empty() && v.iter().all(|a| self.attrs.contains_key(a)));
    }

    /// Reads a list saved with `curl`, plain or gzip.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut list: PublicList = if bytes.starts_with(&[0x1f, 0x8b]) {
            serde_json::from_reader(flate2::read::GzDecoder::new(bytes))
        } else {
            serde_json::from_slice(bytes)
        }
        .context("parse the public package list")?;
        if list.rev.is_empty() || list.attrs.is_empty() {
            bail!("the public package list is empty; the server has no index yet");
        }
        list.fill_pnames();
        Ok(list)
    }

    /// The public attributes that a closure entry can be, best first.
    fn candidates(&self, f: &Found) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        let mut push = |a: &str| {
            if let Some((k, _)) = self.attrs.get_key_value(a) {
                if !out.contains(&k.as_str()) {
                    out.push(k.as_str());
                }
            }
        };
        if let Some(a) = f.attr.as_deref() {
            push(a);
        }
        for a in self.pnames.get(&f.pname).into_iter().flatten() {
            push(a);
        }
        push(&f.pname);
        push(&f.pname.to_ascii_lowercase());
        push(&format!("python3Packages.{}", f.pname));
        out
    }
}

/// What leaves the machine. Every field is listed here; the server refuses
/// any other.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upload {
    pub schema: u32,
    #[serde(default)]
    pub client: String,
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub source: UploadSource,
    pub packages: Vec<Entry>,
    #[serde(default)]
    pub omitted: Omitted,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadSource {
    #[serde(default)]
    pub system: String,
    #[serde(default)]
    pub nixpkgs_rev: Option<String>,
    #[serde(default)]
    pub scanned_at: Option<String>,
    /// The revision of the public list the client filtered with.
    #[serde(default)]
    pub index_rev: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub attr: String,
    /// The pname of the public list, not the one of the closure.
    pub pname: String,
    /// `None` when the closure has a version that the list does not have.
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub modified: bool,
}

/// Closure entries with no public name: counted, never listed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Omitted {
    #[serde(default)]
    pub unknown: usize,
}

impl Report {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read report {}", path.display()))?;
        let report: Report = serde_json::from_str(&text).context("parse report")?;
        if report.schema != SCHEMA {
            bail!("report schema {} is not {SCHEMA}", report.schema);
        }
        Ok(report)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("write {}", path.display()))
    }

    /// Keeps the entries whose name is in the public list. A version goes
    /// only when the list has it for that attribute; any other version
    /// becomes `null` with `modified`. The source name never goes.
    pub fn strict(&self, list: &PublicList) -> Upload {
        let mut kept = BTreeSet::new();
        let mut unknown = 0;
        for f in &self.packages {
            let candidates = list.candidates(f);
            let exact = candidates.iter().find(|a| {
                let v = &list.attrs[**a].1;
                !v.is_empty() && *v == f.version
            });
            let entry = match (exact, candidates.first()) {
                (Some(a), _) => Entry {
                    attr: a.to_string(),
                    pname: list.attrs[*a].0.clone(),
                    version: Some(f.version.clone()),
                    modified: false,
                },
                (None, Some(a)) => Entry {
                    attr: a.to_string(),
                    pname: list.attrs[*a].0.clone(),
                    version: None,
                    modified: true,
                },
                (None, None) => {
                    unknown += 1;
                    continue;
                }
            };
            kept.insert(entry);
        }
        Upload {
            schema: SCHEMA,
            client: format!("maintainer-mode/{}", env!("CARGO_PKG_VERSION")),
            generated_at: self.generated_at.clone(),
            source: UploadSource {
                system: self.source.system.clone(),
                nixpkgs_rev: self.source.nixpkgs_rev.clone(),
                scanned_at: self.source.scanned_at.clone(),
                index_rev: Some(list.rev.clone()),
            },
            packages: kept.into_iter().collect(),
            omitted: Omitted { unknown },
        }
    }
}

impl Upload {
    pub fn modified(&self) -> usize {
        self.packages.iter().filter(|p| p.modified).count()
    }

    /// The server's side of the rule. An attribute that the index does not
    /// have is dropped and counted, because the index can change between the
    /// download of the list and the upload. A version that the index does not
    /// have for a known attribute is an error.
    pub fn accepted(&self, list: &PublicList) -> Result<(BTreeSet<String>, usize), String> {
        if self.schema != SCHEMA {
            return Err(format!("report schema {} is not {SCHEMA}", self.schema));
        }
        let stale = self
            .source
            .index_rev
            .as_deref()
            .is_some_and(|r| r != list.rev);
        let again = if stale {
            format!(
                "; the index changed to {}: download the list again",
                list.rev
            )
        } else {
            String::new()
        };
        let mut kept = BTreeSet::new();
        let mut dropped = 0;
        for p in &self.packages {
            let Some((pname, version)) = list.attrs.get(&p.attr) else {
                dropped += 1;
                continue;
            };
            match (&p.version, p.modified) {
                (Some(_), true) => {
                    return Err(format!(
                        "{}: a modified entry must have the version null",
                        p.attr
                    ));
                }
                (None, false) => {
                    return Err(format!(
                        "{}: the version is null, so \"modified\" must be true",
                        p.attr
                    ));
                }
                (Some(v), false) if v != version => {
                    return Err(format!(
                        "{}: the index does not have version {v} (it has {version}); send null with \"modified\": true{again}",
                        p.attr
                    ));
                }
                _ => {}
            }
            if &p.pname != pname {
                return Err(format!(
                    "{}: the pname must be {pname}, the one of the index{again}",
                    p.attr
                ));
            }
            kept.insert(p.attr.clone());
        }
        Ok((kept, dropped))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(pname: &str, version: &str) -> Found {
        Found {
            pname: pname.into(),
            version: version.into(),
            attr: None,
        }
    }

    fn list() -> PublicList {
        let mut l = PublicList {
            rev: "r1".into(),
            ..Default::default()
        };
        for (a, p, v) in [
            ("hello", "hello", "2.12.1"),
            ("openssl", "openssl", "3.4.0"),
            ("openssl_3_5", "openssl", "3.5.1"),
            ("python3Packages.requests", "requests", "2.32.3"),
        ] {
            l.attrs.insert(a.into(), (p.into(), v.into()));
        }
        l.fill_pnames();
        l
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
            files: None,
        }
    }

    #[test]
    fn a_patched_version_becomes_modified() {
        let up = report(vec![
            found("hello", "1.2.3-acme-fix"),
            found("hello", "2.12.1"),
        ])
        .strict(&list());
        assert_eq!(
            up.packages,
            vec![
                Entry {
                    attr: "hello".into(),
                    pname: "hello".into(),
                    version: None,
                    modified: true
                },
                Entry {
                    attr: "hello".into(),
                    pname: "hello".into(),
                    version: Some("2.12.1".into()),
                    modified: false
                },
            ]
        );
        let text = serde_json::to_string(&up).unwrap();
        assert!(!text.contains("acme"));
        assert!(text.contains("\"version\":null,\"modified\":true"));
    }

    #[test]
    fn an_unknown_name_is_dropped_and_counted() {
        let up = report(vec![
            found("our-private-daemon", "1.0"),
            found("requests", "2.32.3"),
            found("openssl", "3.5.1"),
        ])
        .strict(&list());
        let attrs: Vec<&str> = up.packages.iter().map(|p| p.attr.as_str()).collect();
        assert_eq!(attrs, vec!["openssl_3_5", "python3Packages.requests"]);
        assert_eq!(up.omitted.unknown, 1);
        assert_eq!(up.source.index_rev.as_deref(), Some("r1"));
        let text = serde_json::to_string(&up).unwrap();
        assert!(!text.contains("private"));
    }

    #[test]
    fn a_named_attribute_counts_only_when_public() {
        let mut f = found("own", "1");
        f.attr = Some("packages.x86_64-linux.own".into());
        let up = report(vec![f]).strict(&list());
        assert!(up.packages.is_empty());
        assert_eq!(up.omitted.unknown, 1);
    }

    #[test]
    fn the_server_refuses_a_report_that_is_not_strict() {
        let l = list();
        let ok = report(vec![found("hello", "9"), found("zlib", "1")]).strict(&l);
        let (kept, dropped) = ok.accepted(&l).unwrap();
        assert_eq!(kept.into_iter().collect::<Vec<_>>(), vec!["hello"]);
        assert_eq!(dropped, 0);

        let mut bad = ok.clone();
        bad.packages[0].version = Some("1.2.3-acme-fix".into());
        bad.packages[0].modified = false;
        let e = bad.accepted(&l).unwrap_err();
        assert!(e.contains("does not have version 1.2.3-acme-fix"), "{e}");

        let mut bad = ok.clone();
        bad.packages[0].pname = "private".into();
        assert!(bad.accepted(&l).is_err());

        for text in [
            r#"{"schema":1,"source":{"name":"/home/me/tree"},"packages":[]}"#,
            r#"{"schema":1,"packages":[{"attr":"hello","pname":"hello","version":null,"modified":true,"origin":"x"}]}"#,
            r#"{"schema":1,"packages":[],"omitted":{"flake":1}}"#,
            r#"{"schema":1,"packages":[],"extra":1}"#,
        ] {
            assert!(serde_json::from_str::<Upload>(text).is_err(), "{text}");
        }
    }

    #[test]
    fn the_list_round_trips_plain_and_gzip() {
        use std::io::Write;
        let l = list();
        let text = serde_json::to_vec(&l).unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&text).unwrap();
        for bytes in [text.clone(), gz.finish().unwrap()] {
            let back = PublicList::parse(&bytes).unwrap();
            assert_eq!(back.pnames["openssl"], vec!["openssl", "openssl_3_5"]);
        }
        assert!(PublicList::parse(br#"{"rev":"","attrs":{}}"#).is_err());
    }

    #[test]
    fn an_older_client_report_parses() {
        let text = r#"{"schema":1,"client":"scanner/0.1.0","generated_at":"x",
            "source":{"name":"web","system":"x86_64-linux","nixpkgs_rev":null,"scanned_at":null,"drift":0},
            "packages":[{"pname":"7zz","version":"26.02","attr":"_7zz","resolution":"attr","reach":"runtime"},
            {"pname":"x","version":null}],
            "omitted":{"flake":0,"vendored":0,"absent":3,"unknown":0}}"#;
        let r: Report = serde_json::from_str(text).unwrap();
        assert_eq!(r.packages[0].attr.as_deref(), Some("_7zz"));
        assert_eq!(r.packages[1].version, "");
    }
}
