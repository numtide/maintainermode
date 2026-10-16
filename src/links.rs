//! Which pull requests and issues belong to which package or module. A pull
//! request belongs by the files it changes and by its title; an issue by its
//! title and by the names and paths its body gives.

use std::collections::{HashMap, HashSet};

use crate::area::{Index, area_key};
use crate::model::{Issue, Pr};
use crate::subject;

/// A file shared by more packages than this (`all-packages.nix`, a generic
/// builder) says nothing about which one a change is for.
const SHARED_AREA: usize = 25;
/// A name that many attributes answer to is too vague to link by.
const VAGUE_NAME: usize = 5;

pub fn pkg_key(attr: &str) -> String {
    format!("pkg:{attr}")
}

pub fn module_key(file: &str) -> String {
    format!("module:{file}")
}

/// File names that say what a file is to its directory, not what it is.
const GENERIC_STEMS: [&str; 10] = [
    "default", "client", "server", "module", "options", "common", "service", "daemon", "agent",
    "worker",
];

/// The names a module goes by in titles: its file name, and its directory
/// name when the file is one of several parts of one service.
fn module_names(file: &str) -> (String, Option<String>) {
    let (dir, f) = file.rsplit_once('/').unwrap_or(("", file));
    let stem = f.trim_end_matches(".nix");
    let parent = dir.rsplit('/').next().filter(|d| !d.is_empty());
    match parent {
        Some(p) if GENERIC_STEMS.contains(&stem) => (format!("nixos/{p}"), None),
        Some(p) if dir.starts_with("nixos/modules/") && dir.matches('/').count() >= 3 => {
            (format!("nixos/{stem}"), Some(format!("nixos/{p}")))
        }
        _ => (format!("nixos/{stem}"), None),
    }
}

/// `nixos/nginx` for `nixos/modules/services/web-servers/nginx/default.nix`.
pub fn module_name(file: &str) -> String {
    module_names(file).0
}

#[derive(Default)]
pub struct Links {
    pub prs: HashMap<String, Vec<u64>>,
    pub issues: HashMap<String, Vec<u64>>,
}

struct Lookup {
    by_area: HashMap<String, Vec<String>>,
    by_name: HashMap<String, Vec<String>>,
}

impl Lookup {
    fn new(index: &Index) -> Self {
        let mut by_area: HashMap<String, Vec<String>> = HashMap::new();
        let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
        for (attr, p) in &index.pkgs {
            let key = pkg_key(attr);
            if let Some(f) = &p.file {
                by_area.entry(area_key(f)).or_default().push(key.clone());
            }
            by_name.entry(attr.clone()).or_default().push(key.clone());
            if let Some((_, leaf)) = attr.rsplit_once('.') {
                by_name.entry(leaf.to_string()).or_default().push(key);
            }
        }
        for (pname, attr) in &index.by_pname {
            by_name
                .entry(pname.clone())
                .or_default()
                .push(pkg_key(attr));
        }
        by_area.retain(|_, v| v.len() <= SHARED_AREA);
        let names: Vec<(String, (String, Option<String>))> = index
            .modules
            .keys()
            .map(|f| (f.clone(), module_names(f)))
            .collect();
        let primary: HashSet<&String> = names.iter().map(|(_, (n, _))| n).collect();
        for (file, (name, alias)) in &names {
            let key = module_key(file);
            by_area.entry(area_key(file)).or_default().push(key.clone());
            by_name.entry(name.clone()).or_default().push(key.clone());
            if let Some(a) = alias.as_ref().filter(|a| !primary.contains(a)) {
                by_name.entry(a.clone()).or_default().push(key);
            }
        }
        for v in by_name.values_mut() {
            v.sort();
            v.dedup();
        }
        by_name.retain(|_, v| v.len() <= VAGUE_NAME);
        Lookup { by_area, by_name }
    }

    fn names(&self, names: &[String], out: &mut Vec<String>) {
        for n in names {
            if let Some(keys) = self.by_name.get(n) {
                out.extend(keys.iter().cloned());
            }
        }
    }

    fn paths(&self, paths: &[String], out: &mut Vec<String>) {
        for f in paths {
            if let Some(test) = f
                .strip_prefix("nixos/tests/")
                .and_then(|t| t.split('/').next())
            {
                self.names(&[format!("nixos/{}", test.trim_end_matches(".nix"))], out);
            }
            let mut p = f.as_str();
            loop {
                if let Some(keys) = self.by_area.get(p) {
                    out.extend(keys.iter().cloned());
                }
                match p.rsplit_once('/') {
                    Some((parent, _)) => p = parent,
                    None => break,
                }
            }
        }
    }
}

fn add(map: &mut HashMap<String, Vec<u64>>, mut keys: Vec<String>, number: u64) {
    keys.sort();
    keys.dedup();
    for k in keys {
        map.entry(k).or_default().push(number);
    }
}

pub fn build(index: &Index, prs: &[Pr], issues: &[Issue]) -> Links {
    let look = Lookup::new(index);
    let mut links = Links::default();
    for pr in prs {
        let mut keys = Vec::new();
        look.names(&subject::of_title(&pr.title), &mut keys);
        look.paths(&pr.files, &mut keys);
        add(&mut links.prs, keys, pr.number);
    }
    for i in issues {
        let mut keys = Vec::new();
        look.names(&subject::of_title(&i.title), &mut keys);
        let (paths, names): (Vec<String>, Vec<String>) =
            i.mentions.iter().cloned().partition(|m| m.contains('/'));
        look.names(&names, &mut keys);
        look.paths(&paths, &mut keys);
        add(&mut links.issues, keys, i.number);
    }
    for v in links.prs.values_mut().chain(links.issues.values_mut()) {
        v.sort_unstable_by(|a, b| b.cmp(a));
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::area::{Module, Package};
    use crate::ledger::tests::pr;

    fn index() -> Index {
        let mut i = Index::default();
        i.pkgs.insert(
            "hello".into(),
            Package {
                pname: "hello".into(),
                version: "2.12".into(),
                file: Some("pkgs/by-name/he/hello/package.nix".into()),
                ..Default::default()
            },
        );
        i.by_pname.insert("hello".into(), "hello".into());
        i.modules.insert(
            "nixos/modules/services/web-servers/nginx/default.nix".into(),
            Module::default(),
        );
        i.modules.insert(
            "nixos/modules/services/web-servers/nginx/tailscale-auth.nix".into(),
            Module::default(),
        );
        i
    }

    #[test]
    fn a_pull_request_links_by_file_and_by_title() {
        let mut a = pr(1, "alice", 0);
        a.files = vec!["pkgs/by-name/he/hello/package.nix".into()];
        let mut b = pr(2, "bob", 0);
        b.title = "nixos/nginx: fix".into();
        let mut c = pr(3, "carol", 0);
        c.files = vec!["nixos/tests/nginx.nix".into()];
        let l = build(&index(), &[a, b, c], &[]);
        assert_eq!(l.prs["pkg:hello"], [1]);
        assert!(
            !l.prs
                .contains_key("module:nixos/modules/services/web-servers/nginx/tailscale-auth.nix")
        );
        assert_eq!(
            l.prs["module:nixos/modules/services/web-servers/nginx/default.nix"],
            [3, 2]
        );
    }

    #[test]
    fn module_names_follow_the_title_convention() {
        assert_eq!(
            module_name("nixos/modules/services/web-servers/nginx/default.nix"),
            "nixos/nginx"
        );
        assert_eq!(
            module_name("nixos/modules/services/misc/gitea.nix"),
            "nixos/gitea"
        );
        assert_eq!(
            module_name("nixos/modules/services/computing/foldingathome/client.nix"),
            "nixos/foldingathome"
        );
        assert_eq!(
            module_names("nixos/modules/services/monitoring/prometheus/exporters/node.nix"),
            (
                "nixos/node".to_string(),
                Some("nixos/exporters".to_string())
            )
        );
    }
}
