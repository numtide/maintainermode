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
            let keys = self
                .by_name
                .get(n)
                .or_else(|| self.by_name.get(&n.to_lowercase()));
            if let Some(keys) = keys {
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
    for pr in prs.iter().filter(|p| TooLarge::of(p).is_none()) {
        let mut keys = Vec::new();
        look.names(&subject::of_title(&pr.title), &mut keys);
        look.paths(&pr.files, &mut keys);
        add(&mut links.prs, keys, pr.number);
    }
    for i in issues {
        let mut keys = Vec::new();
        look.names(&subject::of_title(&i.title), &mut keys);
        let (paths, names): (Vec<String>, Vec<String>) = i
            .mentions
            .iter()
            .cloned()
            .partition(|m| subject::is_path(m));
        look.names(&names, &mut keys);
        look.paths(&paths, &mut keys);
        add(&mut links.issues, keys, i.number);
    }
    for v in links.prs.values_mut().chain(links.issues.values_mut()) {
        v.sort_unstable_by(|a, b| b.cmp(a));
    }
    links
}

/// From this many packages and modules on, a pull request is treewide. Each
/// attribute counts, so a Python application counts two times; the limit
/// stays well above an update of 20 Python libraries.
pub const TREEWIDE: usize = 40;

/// From this many changed files on, a pull request is too large to link: no
/// package, module, directory or person gets it. The git copy keeps no more
/// paths, so a longer list is an arbitrary part. A wrong base, a bad merge or
/// a reformat of all of nixpkgs goes above.
pub const TOO_LARGE: usize = crate::changed::MAX;

/// The packages and modules that a treewide pull request touches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Treewide {
    pub packages: usize,
    pub modules: usize,
}

fn count(n: usize, word: &str) -> String {
    let s = if n == 1 { "" } else { "s" };
    format!("{} {word}{s}", crate::chart::thousands(n as i64))
}

impl Treewide {
    /// The one test of a treewide pull request.
    pub fn of(packages: usize, modules: usize) -> Option<Treewide> {
        (packages + modules >= TREEWIDE).then_some(Treewide { packages, modules })
    }

    /// "treewide · 340 packages": the mark of its rows.
    pub fn mark(&self) -> String {
        let what = match (self.packages, self.modules) {
            (p, 0) => count(p, "package"),
            (0, m) => count(m, "module"),
            (p, m) => format!("{} and {}", count(p, "package"), count(m, "module")),
        };
        format!("treewide · {what}")
    }
}

/// A pull request that is too large to link, and the files it changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TooLarge {
    pub files: u64,
}

impl TooLarge {
    /// The one test of a pull request that is too large to link: GitHub's
    /// count, or a list that the git copy cut.
    pub fn of(pr: &Pr) -> Option<TooLarge> {
        let files = pr.changed_files.max(pr.files.len() as u64);
        (files >= TOO_LARGE as u64).then_some(TooLarge { files })
    }

    /// "too large · 19,485 files": the mark of its rows.
    pub fn mark(&self) -> String {
        format!("too large · {}", count(self.files as usize, "file"))
    }
}

/// Sets [`Pr::treewide`] from the links of each pull request.
pub fn mark_treewide(links: &Links, prs: &mut [Pr]) {
    let mut counts: HashMap<u64, (usize, usize)> = HashMap::new();
    for (key, numbers) in &links.prs {
        let module = key.starts_with("module:");
        for n in numbers {
            let c = counts.entry(*n).or_default();
            if module {
                c.1 += 1;
            } else {
                c.0 += 1;
            }
        }
    }
    for p in prs {
        p.treewide = counts
            .get(&p.number)
            .and_then(|&(packages, modules)| Treewide::of(packages, modules));
    }
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
    fn an_issue_links_by_a_module_name_in_its_body() {
        let i = Issue {
            number: 9,
            title: "something is off".into(),
            url: String::new(),
            author: "alice".into(),
            state: "OPEN".into(),
            created_at: crate::ledger::tests::t(0),
            updated_at: crate::ledger::tests::t(0),
            closed_at: None,
            labels: vec![],
            comments: 0,
            mentions: subject::of_body("### Package name\n\nnixos/nginx\n"),
        };
        let l = build(&index(), &[], &[i]);
        assert_eq!(
            l.issues["module:nixos/modules/services/web-servers/nginx/default.nix"],
            [9]
        );
    }

    #[test]
    fn a_pull_request_links_by_file_and_by_title() {
        let mut a = pr(1, "alice", 0);
        a.files = vec!["pkgs/by-name/he/hello/package.nix".into()];
        let mut b = pr(2, "bob", 0);
        b.title = "nixos/nginx: fix".into();
        let mut d = pr(4, "dave", 0);
        d.title = "Hello: a capital first letter".into();
        let mut c = pr(3, "carol", 0);
        c.files = vec!["nixos/tests/nginx.nix".into()];
        let l = build(&index(), &[a, b, c, d], &[]);
        assert_eq!(l.prs["pkg:hello"], [4, 1]);
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
    fn a_pull_request_is_treewide_from_40_packages_and_modules() {
        assert_eq!(Treewide::of(39, 0), None);
        assert_eq!(
            Treewide::of(30, 10),
            Some(Treewide {
                packages: 30,
                modules: 10
            })
        );
        let mark = |p, m| {
            Treewide {
                packages: p,
                modules: m,
            }
            .mark()
        };
        assert_eq!(mark(340, 0), "treewide · 340 packages");
        assert_eq!(mark(0, 41), "treewide · 41 modules");
        assert_eq!(mark(39, 1), "treewide · 39 packages and 1 module");

        let mut links = Links::default();
        for i in 0..39 {
            links.prs.insert(pkg_key(&format!("p{i}")), vec![2, 1]);
        }
        links
            .prs
            .insert(module_key("nixos/modules/services/a.nix"), vec![1]);
        let mut prs = vec![pr(1, "alice", 0), pr(2, "bob", 0), pr(3, "carol", 0)];
        prs[2].treewide = Treewide::of(50, 0);
        mark_treewide(&links, &mut prs);
        assert_eq!(
            prs[0].treewide,
            Some(Treewide {
                packages: 39,
                modules: 1
            })
        );
        assert_eq!(prs[1].treewide, None, "39 packages are not treewide");
        assert_eq!(prs[2].treewide, None, "each recompute decides again");
    }

    #[test]
    fn a_pull_request_too_large_to_link_links_to_nothing() {
        let hello = "pkgs/by-name/he/hello/package.nix".to_string();
        let mut big = pr(1, "alice", 0);
        big.title = "hello: 2.12 -> 2.13".into();
        big.files = vec![hello.clone()];
        big.changed_files = 19_485;
        let mut cut = pr(2, "bob", 0);
        cut.files = vec![hello.clone(); TOO_LARGE];
        cut.changed_files = 6_000;
        let mut wide = pr(3, "carol", 0);
        wide.files = vec![hello; 50];
        wide.changed_files = 2_914;
        assert_eq!(TooLarge::of(&big), Some(TooLarge { files: 19_485 }));
        assert_eq!(TooLarge::of(&cut), Some(TooLarge { files: 10_000 }));
        assert_eq!(TooLarge::of(&wide), None);
        assert_eq!(
            TooLarge { files: 19_485 }.mark(),
            "too large · 19,485 files"
        );
        let l = build(&index(), &[big, cut, wide], &[]);
        assert_eq!(l.prs["pkg:hello"], [3]);
        let t = Treewide {
            packages: 3_400,
            modules: 0,
        };
        assert_eq!(t.mark(), "treewide · 3,400 packages");
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
