//! What a person who signed in cares about: the packages of their machines,
//! what nixpkgs names them as a maintainer of, and what they watch. One
//! order ranks it everywhere: the packages list, the home page and the
//! review queue. Only the owner sees it.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;

use crate::area::Index;
use crate::catalog::{Flags, Subject, maintained, subject_row, subscriptions};
use crate::channels::Channels;
use crate::health::{Hydra, Last};
use crate::queue::Entry;
use crate::ui::{Tab, empty, pager, section};
use crate::web::{App, Snapshot, esc, icon, octicon};

pub(crate) const MACHINES: &str = "/packages?filter=machines";
const UPLOAD: &str = "/me/settings#upload";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Filter {
    Mine,
    Machines,
    Maintain,
    Watch,
    All,
}

impl Filter {
    const ALL: [Filter; 5] = [
        Filter::Mine,
        Filter::Machines,
        Filter::Maintain,
        Filter::Watch,
        Filter::All,
    ];

    /// With no filter, a search goes to all of nixpkgs, so the search box
    /// of the home page and old links keep working.
    pub(crate) fn parse(s: Option<&str>, searching: bool) -> Filter {
        match s {
            Some("mine") => Filter::Mine,
            Some("machines") => Filter::Machines,
            Some("maintain") => Filter::Maintain,
            Some("watch") => Filter::Watch,
            Some("all") => Filter::All,
            _ if searching => Filter::All,
            _ => Filter::Mine,
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Filter::Mine => "mine",
            Filter::Machines => "machines",
            Filter::Maintain => "maintain",
            Filter::Watch => "watch",
            Filter::All => "all",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Filter::Mine => "Mine",
            Filter::Machines => "On my machines",
            Filter::Maintain => "I maintain",
            Filter::Watch => "I watch",
            Filter::All => "All of nixpkgs",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Filter::Mine => "Your packages",
            Filter::Machines => "Packages on your machines",
            Filter::Maintain => "What you maintain",
            Filter::Watch => "What you watch",
            Filter::All => "All of nixpkgs",
        }
    }

    /// The marks of a row, less the one that this filter already says.
    fn marks(self, mut why: Why) -> Why {
        match self {
            Filter::Machines => why.machine = false,
            Filter::Maintain => why.maintain = false,
            Filter::Watch => why.watch = false,
            Filter::Mine | Filter::All => {}
        }
        why
    }

    fn keeps(self, why: Why) -> bool {
        match self {
            Filter::Mine | Filter::All => true,
            Filter::Machines => why.machine,
            Filter::Maintain => why.maintain,
            Filter::Watch => why.watch,
        }
    }
}

/// Why a row is in the list. More than one can be true.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct Why {
    pub(crate) machine: bool,
    pub(crate) maintain: bool,
    pub(crate) watch: bool,
}

impl Why {
    fn group(self) -> u8 {
        if self.machine {
            0
        } else if self.maintain {
            1
        } else if self.watch {
            2
        } else {
            3
        }
    }

    pub(crate) fn labels(self) -> String {
        let mut out = String::new();
        for (on, svg, text) in [
            (self.machine, octicon!("device-desktop"), "on your machines"),
            (self.maintain, octicon!("tools"), "you maintain"),
            (self.watch, octicon!("eye"), "you watch"),
        ] {
            if on {
                let _ = write!(
                    out,
                    " <span class=\"Label Label--accent why\">{} {text}</span>",
                    icon(svg)
                );
            }
        }
        out
    }
}

/// The one order of what is yours: the packages of your machines first,
/// then what you maintain, then what you watch. In each group, what needs
/// attention comes first.
pub(crate) fn order(why: Why, attention: bool) -> (u8, bool) {
    (why.group(), !attention)
}

pub(crate) struct Row {
    pub(crate) sub: Subject,
    pub(crate) why: Why,
    /// The team through which nixpkgs names the person, if any.
    pub(crate) via: Option<String>,
    pub(crate) flags: Flags,
}

/// An open advisory, a newer upstream release, a failing Hydra build or an
/// open pull request.
pub(crate) fn attention(f: &Flags) -> bool {
    f.advisories > 0 || f.upstream.is_some() || f.build_failed || f.open_prs > 0
}

/// One package name for an attribute of a machine report, as
/// [`maintained`] does, so an alias and its target share one row.
fn canonical(index: &Index, attr: &str) -> String {
    let Some(p) = index.pkgs.get(attr) else {
        return attr.to_string();
    };
    match index.by_pname.get(&p.pname) {
        Some(c) if index.pkgs.get(c).is_some_and(|cp| cp.file == p.file) => c.clone(),
        _ => attr.to_string(),
    }
}

/// What a person cares about, by subject key, with no health read yet.
pub(crate) struct Interests {
    pub(crate) items: BTreeMap<String, (Subject, Why, Option<String>)>,
    /// A machine report or a scanned flake exists.
    pub(crate) has_machines: bool,
}

impl Interests {
    pub(crate) fn of(app: &App, login: &str) -> Interests {
        let machines = crate::sets::set_attrs(app, login);
        let has_machines = app.store.package_set(login).ok().flatten().is_some()
            || !app.store.flake_refs(login).unwrap_or_default().is_empty();
        let watched = subscriptions(app, login);
        let index = app.index.read().unwrap();
        Interests::build(&index, login, &machines, &watched, has_machines)
    }

    pub(crate) fn build(
        index: &Index,
        login: &str,
        machines: &[String],
        watched: &[Subject],
        has_machines: bool,
    ) -> Interests {
        let mut items: BTreeMap<String, (Subject, Why, Option<String>)> = BTreeMap::new();
        for a in machines {
            let sub = Subject::Pkg(canonical(index, a));
            items
                .entry(sub.key())
                .or_insert((sub, Why::default(), None))
                .1
                .machine = true;
        }
        let m = maintained(index, login);
        for (sub, via) in m.packages.into_iter().chain(m.modules) {
            let e = items
                .entry(sub.key())
                .or_insert((sub, Why::default(), None));
            e.1.maintain = true;
            e.2 = via;
        }
        for sub in watched {
            items
                .entry(sub.key())
                .or_insert((sub.clone(), Why::default(), None))
                .1
                .watch = true;
        }
        Interests {
            items,
            has_machines,
        }
    }

    pub(crate) fn count(&self, f: Filter) -> usize {
        self.items.values().filter(|(_, w, _)| f.keeps(*w)).count()
    }

    /// The rows of one filter whose name holds `q`, in [`order`].
    pub(crate) fn rows(
        &self,
        s: &Snapshot,
        index: &Index,
        h: &crate::health::Health,
        hydra: Option<&Hydra>,
        f: Filter,
        q: &str,
    ) -> Vec<Row> {
        let ql = q.to_lowercase();
        let mut rows: Vec<Row> = self
            .items
            .values()
            .filter(|(_, w, _)| f.keeps(*w))
            .filter(|(sub, ..)| ql.is_empty() || sub.label().to_lowercase().contains(&ql))
            .map(|(sub, why, via)| {
                let mut flags = Flags::of(s, index, h, sub);
                if let (Some(hy), Subject::Pkg(a)) = (hydra, sub) {
                    flags.build_failed = matches!(
                        hy.known(&Hydra::package_job(a)),
                        Some(Last::Build(b)) if !b.ok
                    );
                }
                Row {
                    sub: sub.clone(),
                    why: *why,
                    via: via.clone(),
                    flags,
                }
            })
            .collect();
        rank(&mut rows);
        rows
    }
}

pub(crate) fn rank(rows: &mut [Row]) {
    rows.sort_by_cached_key(|r| {
        (
            order(r.why, attention(&r.flags)),
            r.flags.rank(),
            r.sub.label(),
        )
    });
}

/// The rows of [`Filter::Mine`] that need attention, for the home page.
pub(crate) fn needs_you(app: &App, login: &str) -> Vec<Row> {
    let i = Interests::of(app, login);
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let mut rows = i.rows(&s, &index, &h, Some(&app.hydra), Filter::Mine, "");
    rows.retain(|r| attention(&r.flags));
    rows
}

/// Pull requests for the For you tab of a person: those that touch the
/// packages of their machines, then what they maintain, own, changed or
/// tagged, then what they watch. The same [`order`] as the packages.
pub(crate) fn for_you<'a>(app: &App, s: &'a Snapshot, login: &str) -> Vec<&'a Entry> {
    let watched = app.store.watched(login).unwrap_or_default();
    let machines: HashSet<String> = {
        let index = app.index.read().unwrap();
        crate::sets::set_attrs(app, login)
            .iter()
            .map(|a| Subject::Pkg(canonical(&index, a)).key())
            .collect()
    };
    rank_prs(s, login, &watched, &machines)
}

pub(crate) fn rank_prs<'a>(
    s: &'a Snapshot,
    login: &str,
    watched: &[String],
    machine_keys: &HashSet<String>,
) -> Vec<&'a Entry> {
    let on_machines: HashSet<u64> = machine_keys
        .iter()
        .filter_map(|k| s.links.prs.get(k))
        .flatten()
        .copied()
        .collect();
    let interest = crate::web::Interest::of(s, Some(login), Some(watched));
    let mut list: Vec<(Why, &Entry)> = s
        .queues
        .review
        .iter()
        .filter(|e| e.author != login)
        .map(|e| {
            let why = Why {
                machine: on_machines.contains(&e.number),
                maintain: interest.maintains(e),
                watch: interest.watches(e),
            };
            (why, e)
        })
        .filter(|(w, _)| w.machine || w.maintain || w.watch)
        .collect();
    list.sort_by_key(|(w, e)| (order(*w, false), e.number));
    list.into_iter().map(|(_, e)| e).collect()
}

/// A row with its version, the team that names the person, and why it is
/// in the list. `status` sits on the right.
pub(crate) fn row_html(index: &Index, ch: &Channels, r: &Row, marks: Why, status: &str) -> String {
    let version = match &r.sub {
        Subject::Pkg(a) => index
            .pkgs
            .get(a)
            .filter(|p| !p.version.is_empty())
            .map(|p| {
                format!(
                    " <span class=\"color-fg-muted f6\">{}</span>{}",
                    esc(&p.version),
                    crate::catalog::lag_label(ch, a, &p.version)
                )
            })
            .unwrap_or_default(),
        _ => String::new(),
    };
    let via = r
        .via
        .as_ref()
        .map(|t| format!(" <span class=\"Label\">via {}</span>", esc(t)))
        .unwrap_or_default();
    subject_row(
        &r.sub,
        &r.flags,
        &format!("{version}{via}{}", marks.labels()),
        status,
    )
}

fn href(f: Filter, q: &str) -> String {
    if q.is_empty() {
        format!("/packages?filter={}", f.key())
    } else {
        format!(
            "/packages?filter={}&q={}",
            f.key(),
            crate::github::urlencode(q)
        )
    }
}

/// The filter bar, and the step that asks for a machine report in place
/// of the machines filter when there is none.
pub(crate) fn bar(i: &Interests, current: Filter, all: usize) -> (Vec<Tab>, String) {
    let mut tabs = Vec::new();
    for f in Filter::ALL {
        if f == Filter::Machines && !i.has_machines && current != f {
            continue;
        }
        let n = if f == Filter::All { all } else { i.count(f) };
        tabs.push(Tab::new(href(f, ""), f.label(), f == current).count(n));
    }
    let step = if i.has_machines {
        String::new()
    } else {
        format!(
            "<div class=\"callout step mb-3\">{} <a class=\"text-bold\" href=\"{UPLOAD}\">Upload the packages of your machines</a> \
             <span class=\"color-fg-muted\">to see them first here.</span></div>",
            icon(octicon!("upload"))
        )
    };
    (tabs, step)
}

fn search_form(f: Filter, q: &str) -> String {
    format!(
        "<form method=\"get\" action=\"/packages\" class=\"inline-form mb-3\">\
         <input type=\"hidden\" name=\"filter\" value=\"{}\">\
         <input class=\"form-control flex-auto\" type=\"search\" name=\"q\" value=\"{}\" placeholder=\"filter by name\" aria-label=\"filter by name\">\
         <button class=\"btn\" type=\"submit\">{} Filter</button></form>",
        f.key(),
        esc(q),
        icon(octicon!("search"))
    )
}

const NOTICE: &str = "<details class=\"callout mb-3\"><summary class=\"text-bold py-1\">What the server keeps, and why</summary>\
<p class=\"mt-2\">For your latest report, the server keeps the nixpkgs attributes that its index knows, the upload time, \
the scan time and the nixpkgs revision. It drops all other names. It keeps the hash of your upload token, \
not the token. Only you see this data, here, in your inbox and in your private feed. No public page, number or API reads it. \
There is no admin view. The data expires 90 days after the last upload.</p>\
<p class=\"mb-0\">For a public flake that you add below, the server keeps the reference, the output, the attributes from the last good scan, \
and the reason of the last failure. The server scans the flake only while you open your packages at least once in 90 days. \
These attributes expire 90 days after the last good scan or your last visit. \"Delete my report\" deletes your report and your flakes now.</p></details>";

/// What one filter of `/packages` shows for a person: `sub`, `actions`
/// and `body` are HTML.
pub(crate) struct View {
    pub(crate) sub: String,
    pub(crate) actions: String,
    pub(crate) body: String,
}

pub(crate) fn view(
    app: &App,
    login: &str,
    i: &Interests,
    f: Filter,
    q: &str,
    after: Option<u64>,
) -> View {
    let _ = app.store.touch_flake_refs(login);
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let ch = app.channels.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let rows = i.rows(&s, &index, &h, Some(&app.hydra), f, q);
    let mut out = String::new();
    let mut actions = String::new();
    match f {
        Filter::Machines => {
            out.push_str(&crate::sets::summary(app, login));
            out.push_str(NOTICE);
        }
        Filter::Maintain => {
            let missing = i
                .items
                .values()
                .filter(|(_, w, _)| w.maintain && !w.watch)
                .count();
            if missing > 0 {
                actions = format!(
                    "<form method=\"post\" action=\"/me/watch/maintained\" class=\"d-inline\">\
                     <button class=\"btn btn-sm btn-primary\" type=\"submit\">{} Watch everything I maintain</button></form>",
                    icon(octicon!("bell"))
                );
            }
        }
        Filter::Watch | Filter::Mine | Filter::All => {}
    }
    if !(rows.is_empty() && q.is_empty()) {
        out.push_str(&search_form(f, q));
    }
    let (page, next) = crate::review::paged_by_offset(&rows, after);
    let list: String = page
        .iter()
        .map(|r| {
            let status = if f == Filter::Watch && r.why.watch {
                format!(
                    "<form method=\"post\" action=\"/me/watch/remove\" class=\"d-inline ml-2\"><input type=\"hidden\" name=\"key\" value=\"{}\">\
                     <button class=\"btn btn-sm\" type=\"submit\">Stop watching</button></form>",
                    esc(&r.sub.key())
                )
            } else {
                String::new()
            };
            row_html(&index, &ch, r, f.marks(r.why), &status)
        })
        .collect();
    let blank = if !q.is_empty() {
        "Nothing matches.".to_string()
    } else {
        match f {
            Filter::Mine => format!(
                "Nothing yet. <a href=\"{UPLOAD}\">Upload the packages of your machines</a>, or watch a package in <a href=\"/packages?filter=watch\">I watch</a>."
            ),
            Filter::Machines if i.has_machines => {
                "Your report has no package that the nixpkgs index of this server knows.".into()
            }
            Filter::Machines => format!(
                "No report yet. Make an <a href=\"{UPLOAD}\">upload token</a>, then run <code>maintainer-mode scan</code> and \
                 <code>maintainer-mode submit</code> on your machine."
            ),
            Filter::Maintain => format!(
                "nixpkgs does not name you in <code>meta.maintainers</code> yet. Read <a href=\"{}\">maintainers/README.md</a> to become a maintainer.",
                crate::welcome::MAINTAINERS_README
            ),
            Filter::Watch | Filter::All => "You watch nothing yet. Write a name below, or press Watch on the page of a package, a module or a directory.".into(),
        }
    };
    out.push_str(&section(
        f.title(),
        Some(rows.len()),
        None,
        &if list.is_empty() { empty(&blank) } else { list },
    ));
    out.push_str(&pager(&href(f, q), next.as_deref()));
    if f == Filter::Watch {
        out.push_str(&section(
            "Watch more",
            None,
            None,
            "<div class=\"Box-row\"><form method=\"post\" action=\"/me/watch\" class=\"inline-form\">\
             <input class=\"form-control flex-auto\" name=\"name\" placeholder=\"hello, nixos/nginx or pkgs/development/python-modules\" aria-label=\"what to watch\">\
             <button class=\"btn btn-primary\" type=\"submit\">Watch</button></form></div>",
        ));
    }
    if f == Filter::Machines {
        out.push_str(&crate::rescan::panel(app, login));
    }
    let sub = match f {
        Filter::Mine => "The packages of your machines first, then what you maintain, then what you watch. \
                         In each group, those that need attention come first."
            .to_string(),
        Filter::Machines => "The packages of your latest machine report and of your scanned flakes. \
                             New advisories and upstream releases for them reach your inbox and your Atom feed."
            .to_string(),
        Filter::Maintain => format!(
            "Packages and NixOS modules whose <code>meta.maintainers</code> or <code>meta.teams</code> name you. \
             Change the list in nixpkgs. Everybody can see it on <a href=\"/u/{0}?tab=maintains\">your public profile</a>.",
            esc(login)
        ),
        Filter::Watch | Filter::All => "The packages, modules and directories you watch. \
                          Their news reaches your <a href=\"/inbox\">inbox</a> and your <a href=\"/me/settings#feed\">Atom feed</a>."
            .to_string(),
    };
    View {
        sub: format!("{sub} Only you see this list."),
        actions,
        body: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::area::Package;

    fn pkg(name: &str, maintainers: &[&str]) -> Package {
        Package {
            pname: name.into(),
            version: "1.0".into(),
            file: Some(format!("pkgs/by-name/{name}/package.nix")),
            maintainers: maintainers.iter().map(|m| m.to_string()).collect(),
            ..Default::default()
        }
    }

    fn index() -> Index {
        let mut index = Index::default();
        for (name, m) in [
            ("curl", &["alice"][..]),
            ("hello", &["alice"][..]),
            ("jq", &["bob"][..]),
            ("zlib", &["bob"][..]),
            ("vim", &["bob"][..]),
        ] {
            index.pkgs.insert(name.into(), pkg(name, m));
            index.by_pname.insert(name.into(), name.into());
        }
        index
            .pkgs
            .insert("hello-alias".into(), pkg("hello", &["alice"]));
        index
    }

    fn keys(rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| r.sub.label()).collect()
    }

    #[test]
    fn machines_come_first_then_maintained_then_watched() {
        let index = index();
        let i = Interests::build(
            &index,
            "alice",
            &["zlib".into(), "hello-alias".into()],
            &[Subject::Pkg("jq".into()), Subject::Pkg("curl".into())],
            true,
        );
        let s = Snapshot::default();
        let h = crate::health::Health::default();
        let rows = i.rows(&s, &index, &h, None, Filter::Mine, "");
        assert_eq!(keys(&rows), ["hello", "zlib", "curl", "jq"]);
        assert_eq!(
            rows[0].why,
            Why {
                machine: true,
                maintain: true,
                watch: false
            }
        );
        assert!(rows[2].why.maintain && rows[2].why.watch);
        assert_eq!(i.count(Filter::Mine), 4);
        assert_eq!(i.count(Filter::Machines), 2);
        assert_eq!(i.count(Filter::Maintain), 2);
        assert_eq!(i.count(Filter::Watch), 2);
        let watch = i.rows(&s, &index, &h, None, Filter::Watch, "");
        assert_eq!(keys(&watch), ["curl", "jq"]);
        let found = i.rows(&s, &index, &h, None, Filter::Mine, "LI");
        assert_eq!(keys(&found), ["zlib"]);
    }

    #[test]
    fn in_each_group_attention_comes_first() {
        let index = index();
        let i = Interests::build(
            &index,
            "nobody",
            &["vim".into(), "zlib".into(), "jq".into()],
            &[],
            true,
        );
        let s = Snapshot::default();
        let h = crate::health::Health::default();
        let mut rows = i.rows(&s, &index, &h, None, Filter::Mine, "");
        assert_eq!(keys(&rows), ["jq", "vim", "zlib"]);
        rows[2].flags.build_failed = true;
        rows[1].flags.open_prs = 1;
        rank(&mut rows);
        assert_eq!(keys(&rows), ["vim", "zlib", "jq"]);
        assert!(order(Why::default(), true) < order(Why::default(), false));
        let machine = Why {
            machine: true,
            ..Why::default()
        };
        let maintain = Why {
            maintain: true,
            ..Why::default()
        };
        assert!(order(machine, false) < order(maintain, true));
    }

    #[test]
    fn a_search_alone_goes_to_all_of_nixpkgs() {
        assert_eq!(Filter::parse(None, false), Filter::Mine);
        assert_eq!(Filter::parse(None, true), Filter::All);
        assert_eq!(Filter::parse(Some("watch"), true), Filter::Watch);
        assert_eq!(Filter::parse(Some("nonsense"), false), Filter::Mine);
        for f in Filter::ALL {
            assert_eq!(Filter::parse(Some(f.key()), false), f);
        }
    }

    #[test]
    fn without_a_report_the_bar_asks_for_one() {
        let index = index();
        let none = Interests::build(&index, "alice", &[], &[], false);
        let (tabs, step) = bar(&none, Filter::Mine, 7);
        assert_eq!(tabs.len(), 4);
        assert!(!tabs.iter().any(|t| t.href.contains("machines")));
        assert!(step.contains("href=\"/me/settings#upload\""));
        assert_eq!(tabs.last().unwrap().count, Some(7));
        let some = Interests::build(&index, "alice", &["vim".into()], &[], true);
        let (tabs, step) = bar(&some, Filter::Mine, 7);
        assert_eq!(tabs.len(), 5);
        assert!(step.is_empty());
        assert_eq!(tabs[1].count, Some(1));
    }

    #[test]
    fn a_row_says_why_it_is_there() {
        let index = index();
        let i = Interests::build(&index, "alice", &["hello".into()], &[], true);
        let s = Snapshot::default();
        let h = crate::health::Health::default();
        let rows = i.rows(&s, &index, &h, None, Filter::Mine, "");
        let html = row_html(&index, &Channels::default(), &rows[0], rows[0].why, "");
        let less = row_html(
            &index,
            &Channels::default(),
            &rows[0],
            Filter::Machines.marks(rows[0].why),
            "",
        );
        assert!(!less.contains("on your machines") && less.contains("you maintain"));
        assert!(html.contains("on your machines"));
        assert!(html.contains("you maintain"));
        assert!(!html.contains("you watch"));
        assert!(html.contains("href=\"/p/hello\""));
    }
}
