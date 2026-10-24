//! What a person who signed in cares about: the packages of their machines,
//! what nixpkgs names them as a maintainer of, and what they watch. One
//! order ranks it everywhere: the packages list, the home page and the
//! review queue. Only the owner sees it.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::area::Index;
use crate::catalog::{Flags, Subject, maintained, subject_row, subscriptions};
use crate::channels::Channels;
use crate::health::{Hydra, Last};
use crate::html::{Html, html};
use crate::queue::Entry;
use crate::ui::{Tab, empty, pager, section};
use crate::web::{App, Snapshot, icon, octicon};

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
    pub(crate) const ALL: [Filter; 5] = [
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

    pub(crate) fn labels(self) -> Html {
        let mut out = Html::new();
        for (on, svg, text) in [
            (self.machine, octicon!("device-desktop"), "on your machines"),
            (self.maintain, octicon!("tools"), "you maintain"),
            (self.watch, octicon!("eye"), "you watch"),
        ] {
            if on {
                out.push(&html!(
                    " <span class=\"Label Label--accent\">{} {text}</span>",
                    icon(svg)
                ));
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
    /// The (set, output label) pairs that hold it.
    pub(crate) hosts: Vec<(String, String)>,
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
    /// Package attribute → the version the machine report gave.
    pub(crate) versions: HashMap<String, String>,
    /// Set name → the subject keys of its packages.
    pub(crate) sets: BTreeMap<String, HashSet<String>>,
    /// Subject key → the (set, output label) pairs that hold it, from the
    /// uploads with `--with-outputs`.
    pub(crate) hosts: HashMap<String, Vec<(String, String)>>,
}

impl Interests {
    pub(crate) fn of(app: &App, login: &str) -> Interests {
        let sets = app.store.sets(login).unwrap_or_default();
        let has_machines = !sets.is_empty();
        let mut machines: Vec<String> = sets
            .iter()
            .filter_map(|s| s.data.as_ref())
            .flat_map(|d| d.attrs.iter().cloned())
            .collect();
        machines.sort();
        machines.dedup();
        let watched = subscriptions(app, login);
        let index = app.index.read().unwrap();
        let mut i = Interests::build(&index, login, &machines, &watched, has_machines);
        for s in sets {
            let Some(d) = s.data else {
                i.sets.insert(s.name, HashSet::new());
                continue;
            };
            for (a, v) in d.versions {
                i.versions.entry(canonical(&index, &a)).or_insert(v);
            }
            for a in d.outputs.of.keys() {
                let hosts = i
                    .hosts
                    .entry(Subject::Pkg(canonical(&index, a)).key())
                    .or_default();
                for l in d.outputs.labels_of(a) {
                    let h = (s.name.clone(), l.to_string());
                    if !hosts.contains(&h) {
                        hosts.push(h);
                    }
                }
            }
            let keys = d
                .attrs
                .iter()
                .map(|a| Subject::Pkg(canonical(&index, a)).key())
                .collect();
            i.sets.insert(s.name, keys);
        }
        i
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
            versions: HashMap::new(),
            sets: BTreeMap::new(),
            hosts: HashMap::new(),
        }
    }

    pub(crate) fn count(&self, f: Filter) -> usize {
        self.items.values().filter(|(_, w, _)| f.keeps(*w)).count()
    }

    /// The rows of one filter whose name holds `q`, in [`order`].
    /// [`Interests::rows`], narrowed to one set and one output of it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn picked(
        &self,
        s: &Snapshot,
        index: &Index,
        h: &crate::health::Health,
        hydra: Option<&Hydra>,
        f: Filter,
        q: &str,
        set: Option<&str>,
        output: Option<&str>,
    ) -> Vec<Row> {
        let mut rows = self.rows(s, index, h, hydra, f, q);
        if let Some(name) = set {
            let keys = &self.sets[name];
            rows.retain(|r| keys.contains(&r.sub.key()));
            for r in &mut rows {
                r.hosts.retain(|(s, _)| s == name);
            }
        }
        if let (Some(name), Some(o)) = (set, output) {
            rows.retain(|r| r.hosts.iter().any(|(s, l)| s == name && l == o));
        }
        rows
    }

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
                    flags.build_failed = hy.builds.red_on(a).is_some()
                        || matches!(
                            hy.known(&Hydra::package_job(a)),
                            Some(Last::Build(b)) if !b.ok
                        );
                }
                Row {
                    sub: sub.clone(),
                    why: *why,
                    via: via.clone(),
                    flags,
                    hosts: self.hosts.get(&sub.key()).cloned().unwrap_or_default(),
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
    for_you_why(app, s, login)
        .into_iter()
        .map(|(e, _)| e)
        .collect()
}

/// [`for_you`], with why each pull request is there.
pub(crate) fn for_you_why<'a>(
    app: &App,
    s: &'a Snapshot,
    login: &str,
) -> Vec<(&'a Entry, Vec<String>)> {
    let watched = app.store.watched(login).unwrap_or_default();
    let machines: HashSet<String> = {
        let index = app.index.read().unwrap();
        crate::sets::set_attrs(app, login)
            .iter()
            .map(|a| Subject::Pkg(canonical(&index, a)).key())
            .collect()
    };
    rank_prs_why(s, login, &watched, &machines)
}

#[cfg(test)]
pub(crate) fn rank_prs<'a>(
    s: &'a Snapshot,
    login: &str,
    watched: &[String],
    machine_keys: &HashSet<String>,
) -> Vec<&'a Entry> {
    rank_prs_why(s, login, watched, machine_keys)
        .into_iter()
        .map(|(e, _)| e)
        .collect()
}

pub(crate) fn rank_prs_why<'a>(
    s: &'a Snapshot,
    login: &str,
    watched: &[String],
    machine_keys: &HashSet<String>,
) -> Vec<(&'a Entry, Vec<String>)> {
    let mut on_machines: HashMap<u64, Vec<String>> = HashMap::new();
    for k in machine_keys {
        for n in s.links.prs.get(k).into_iter().flatten() {
            if let Some(sub) = Subject::parse(k) {
                on_machines.entry(*n).or_default().push(sub.label());
            }
        }
    }
    let interest = crate::web::Interest::of(s, Some(login), Some(watched));
    let mut list: Vec<(Why, &Entry, Vec<String>)> = s
        .queues
        .review
        .iter()
        .filter(|e| e.author != login)
        .map(|e| {
            let why = Why {
                machine: on_machines.contains_key(&e.number),
                maintain: interest.maintains(e),
                watch: interest.watches(e),
            };
            let mut lines = Vec::new();
            if let Some(names) = on_machines.get(&e.number) {
                let mut names = names.clone();
                names.sort();
                lines.push(format!("on your machines: {}", crate::web::some_of(names)));
            }
            lines.extend(interest.why(e));
            (why, e, lines)
        })
        .filter(|(w, _, _)| w.machine || w.maintain || w.watch)
        .collect();
    list.sort_by_key(|(w, e, _)| (order(*w, false), e.number));
    list.into_iter().map(|(_, e, l)| (e, l)).collect()
}

/// A row with its version, the team that names the person, and why it is
/// in the list. `status` sits on the right.
pub(crate) fn row_html(
    index: &Index,
    ch: &Channels,
    r: &Row,
    marks: Why,
    status: impl Into<Html>,
) -> Html {
    let status: Html = status.into();
    let version = match &r.sub {
        Subject::Pkg(a) => index
            .pkgs
            .get(a)
            .filter(|p| !p.version.is_empty())
            .map(|p| {
                html!(
                    " <span class=\"color-fg-muted f6\">{}</span>{}",
                    p.version,
                    crate::catalog::lag_label(ch, a, &p.version)
                )
            })
            .unwrap_or_default(),
        _ => Html::new(),
    };
    let via = r
        .via
        .as_ref()
        .map(|t| html!(" <span class=\"Label\">via {t}</span>"))
        .unwrap_or_default();
    subject_row(
        &r.sub,
        &r.flags,
        html!("{version}{via}{}{}", marks.labels(), hosts_html(&r.hosts)),
        &status,
    )
}

const HOSTS_SHOWN: usize = 3;

/// Where a package runs, "on web-1, laptop and 2 more", each a link to the
/// filter of that output.
pub(crate) fn hosts_html(hosts: &[(String, String)]) -> Html {
    if hosts.is_empty() {
        return Html::new();
    }
    let mut links = Html::new();
    for (i, (s, l)) in hosts.iter().take(HOSTS_SHOWN).enumerate() {
        if i > 0 {
            links.push(&Html::from(", "));
        }
        links.push(&html!(
            "<a class=\"Link--secondary\" href=\"{}\">{l}</a>",
            href(Filter::Machines, "", Some(s), Some(l))
        ));
    }
    let more = match hosts.len().saturating_sub(HOSTS_SHOWN) {
        0 => Html::new(),
        n => html!(" and {n} more"),
    };
    html!(" <span class=\"f6 color-fg-muted\">on {links}{more}</span>")
}

fn href(f: Filter, q: &str, set: Option<&str>, output: Option<&str>) -> String {
    let mut out = format!("/packages?filter={}", f.key());
    if let Some(s) = set {
        out.push_str("&set=");
        out.push_str(&crate::github::urlencode(s));
    }
    if let Some(o) = output {
        out.push_str("&output=");
        out.push_str(&crate::github::urlencode(o));
    }
    if !q.is_empty() {
        out.push_str("&q=");
        out.push_str(&crate::github::urlencode(q));
    }
    out
}

/// The filter bar, and the step that asks for a machine report in place
/// of the machines filter when there is none.
pub(crate) fn bar(i: &Interests, current: Filter, all: usize) -> (Vec<Tab>, Html) {
    let mut tabs = Vec::new();
    for f in Filter::ALL {
        if f == Filter::Machines && !i.has_machines && current != f {
            continue;
        }
        let n = if f == Filter::All { all } else { i.count(f) };
        tabs.push(Tab::new(href(f, "", None, None), f.label(), f == current).count(n));
    }
    let step = if i.has_machines {
        Html::new()
    } else {
        html!(
            "<div class=\"callout mb-3\">{} <a class=\"text-bold\" href=\"{UPLOAD}\">Upload the packages of your machines</a> \
             <span class=\"color-fg-muted\">to see them first here.</span></div>",
            icon(octicon!("upload"))
        )
    };
    (tabs, step)
}

fn search_form(f: Filter, q: &str, set: Option<&str>, output: Option<&str>) -> Html {
    let hidden = |name: &str, v: Option<&str>| {
        v.map(|v| html!("<input type=\"hidden\" name=\"{name}\" value=\"{v}\">"))
            .unwrap_or_default()
    };
    let set = html!("{}{}", hidden("set", set), hidden("output", output));
    html!(
        "<form method=\"get\" action=\"/packages\" class=\"inline-form mb-3\">\
         <input type=\"hidden\" name=\"filter\" value=\"{}\">{set}\
         <input class=\"form-control flex-auto\" type=\"search\" name=\"q\" value=\"{q}\" placeholder=\"filter by name\" aria-label=\"filter by name\">\
         <button class=\"btn\" type=\"submit\">{} Filter</button></form>",
        f.key(),
        icon(octicon!("search"))
    )
}

const NOTICE: &str = "<details class=\"callout mb-3\"><summary class=\"text-bold py-1\">What the server keeps, and why</summary>\
<p class=\"mt-2\">For each set, the server keeps its name, the nixpkgs attributes that its index knows, the upload time, \
the scan time and the nixpkgs revision. It drops all other names. The name is only what you give with <code>--name</code>. \
With <code>--with-outputs</code>, it also keeps the label of each output, such as a host name, and the outputs that hold each package. \
It keeps the hash of your upload token, not the token. Only you see a private set: here, in your inbox, in your private feed, and in the private answers of the API and the MCP server. \
No public page, number or API reads it. There is no admin view. A set expires 90 days after its last upload.</p>\
<p class=\"mb-0\">A public flake set also keeps its flake reference and output. When you show it on your profile, everybody can see \
its name, its reference, its packages and its output labels there. For a flake that the server scans, the server also keeps the reason of the last failure. \
It scans the flake only while you open your packages at least once in 90 days. \
These attributes expire 90 days after the last good scan or your last visit. \"Delete\" deletes one set now.</p></details>";

/// What one filter of `/packages` shows for a person: `sub`, `actions`
/// and `body` are HTML.
pub(crate) struct View {
    pub(crate) sub: Html,
    pub(crate) actions: Html,
    pub(crate) body: Html,
}

/// The outputs of one set, each with its count of packages, as links to
/// the filter of each one. Nothing for a set without labels.
fn outputs_nav(i: &Interests, set: &str, current: Option<&str>) -> Html {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for (s, l) in i.hosts.values().flatten() {
        if s == set {
            *counts.entry(l.as_str()).or_default() += 1;
        }
    }
    if counts.is_empty() {
        return Html::new();
    }
    let mut links = vec![(
        href(Filter::Machines, "", Some(set), None),
        "All outputs".to_string(),
        None,
        current.is_none(),
    )];
    links.extend(counts.into_iter().map(|(l, n)| {
        (
            href(Filter::Machines, "", Some(set), Some(l)),
            l.to_string(),
            Some(n),
            current == Some(l),
        )
    }));
    let chips: Html = links
        .into_iter()
        .map(|(href, label, n, on)| {
            let accent = if on { " Label--accent" } else { "" };
            let cur = if on { " aria-current=\"page\"" } else { "" };
            let count = n
                .map(|n| html!(" <span class=\"Counter\">{n}</span>"))
                .unwrap_or_default();
            html!(
                "<a class=\"Label tap-label{accent}\" href=\"{href}\"{}>{label}{count}</a>",
                Html::from(cur)
            )
        })
        .collect();
    html!(
        "<nav class=\"d-flex flex-wrap flex-items-center gap-2 mb-3\" aria-label=\"Outputs of {set}\">\
         <span class=\"f6 color-fg-muted\">{} Where it runs:</span>{chips}</nav>",
        icon(octicon!("device-desktop"))
    )
}

/// One set of the machines filter, and one output label of that set.
#[derive(Clone, Copy, Default)]
pub(crate) struct Pick<'a> {
    pub(crate) set: Option<&'a str>,
    pub(crate) output: Option<&'a str>,
}

/// An unknown set or `None` shows all of them; an unknown output shows the
/// whole set.
pub(crate) fn view(
    app: &App,
    login: &str,
    i: &Interests,
    f: Filter,
    q: &str,
    after: Option<u64>,
    pick: Pick,
) -> View {
    let _ = app.store.touch_flake_refs(login);
    let set = pick
        .set
        .filter(|n| f == Filter::Machines && i.sets.contains_key(*n));
    let output = pick.output.filter(|o| {
        set.is_some_and(|s| {
            i.hosts
                .values()
                .flatten()
                .any(|(hs, hl)| hs == s && hl == o)
        })
    });
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let ch = app.channels.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let rows = i.picked(&s, &index, &h, Some(&app.hydra), f, q, set, output);
    let mut out = Html::new();
    let mut actions = Html::new();
    match f {
        Filter::Machines => {
            out.push(&crate::sets::summary(app, login, set));
            out.push(&Html::from(NOTICE));
            if let Some(name) = set {
                out.push(&outputs_nav(i, name, output));
            }
        }
        Filter::Maintain => {
            let missing = i
                .items
                .values()
                .filter(|(_, w, _)| w.maintain && !w.watch)
                .count();
            if missing > 0 {
                actions = html!(
                    "<form method=\"post\" action=\"/me/watch/maintained\" class=\"d-inline\">\
                     <button class=\"btn btn-sm btn-primary\" type=\"submit\">{} Watch everything I maintain</button></form>",
                    icon(octicon!("bell"))
                );
            }
        }
        Filter::Watch | Filter::Mine | Filter::All => {}
    }
    if !(rows.is_empty() && q.is_empty()) {
        out.push(&search_form(f, q, set, output));
    }
    let (page, next) = crate::review::paged_by_offset(&rows, after);
    let list: Html = page
        .iter()
        .map(|r| {
            let status = if f == Filter::Watch && r.why.watch {
                html!(
                    "<form method=\"post\" action=\"/me/watch/remove\" class=\"d-inline ml-2\"><input type=\"hidden\" name=\"key\" value=\"{}\">\
                     <button class=\"btn btn-sm\" type=\"submit\">Stop watching</button></form>",
                    r.sub.key()
                )
            } else {
                Html::new()
            };
            row_html(&index, &ch, r, f.marks(r.why), status)
        })
        .collect();
    let blank: Html = if !q.is_empty() {
        "Nothing matches.".into()
    } else {
        match f {
            Filter::Mine => html!(
                "Nothing yet. <a href=\"{UPLOAD}\">Upload the packages of your machines</a>, or watch a package in <a href=\"/packages?filter=watch\">I watch</a>."
            ),
            Filter::Machines if i.has_machines => {
                "This set has no package that the nixpkgs index of this server knows.".into()
            }
            Filter::Machines => html!(
                "No report yet. Make an <a href=\"{UPLOAD}\">upload token</a>, then run <code>maintainer-mode scan</code> and \
                 <code>maintainer-mode submit</code> on your machine."
            ),
            Filter::Maintain => html!(
                "nixpkgs does not name you in <code>meta.maintainers</code> yet. Read <a href=\"{}\">maintainers/README.md</a> to become a maintainer.",
                crate::welcome::MAINTAINERS_README
            ),
            Filter::Watch | Filter::All => "You watch nothing yet. Write a name below, or press Watch on the page of a package, a module or a directory.".into(),
        }
    };
    let title = match (set, output) {
        (Some(n), Some(o)) => html!("Packages of {n} on {o}"),
        (Some(n), None) => html!("Packages of {n}"),
        (None, _) => f.title().into(),
    };
    out.push(&section(
        title,
        Some(rows.len()),
        None,
        if list.is_empty() { empty(blank) } else { list },
    ));
    out.push(&pager(&href(f, q, set, output), next.as_deref()));
    if f == Filter::Watch {
        out.push(&section(
            "Watch more",
            None,
            None,
            "<div class=\"Box-row\"><form method=\"post\" action=\"/me/watch\" class=\"inline-form\">\
             <input class=\"form-control flex-auto\" name=\"name\" placeholder=\"hello, nixos/nginx or pkgs/development/python-modules\" aria-label=\"what to watch\">\
             <button class=\"btn btn-primary\" type=\"submit\">Watch</button></form></div>",
        ));
    }
    if f == Filter::Machines {
        out.push(&crate::rescan::panel(app, login));
    }
    let sub: Html = match f {
        Filter::Mine => "The packages of your machines first, then what you maintain, then what you watch. \
                         In each group, those that need attention come first."
            .into(),
        Filter::Machines => "The packages of your sets: your machine reports and your flakes. \
                             Pick one set, or see all of them. A set that you show on your profile is public. \
                             New advisories and upstream releases for them reach your inbox and your Atom feed."
            .into(),
        Filter::Maintain => html!(
            "Packages and NixOS modules whose <code>meta.maintainers</code> or <code>meta.teams</code> name you. \
             Change the list in nixpkgs. Everybody can see it on <a href=\"/u/{login}?tab=maintains\">your public profile</a>."
        ),
        Filter::Watch | Filter::All => "The packages, modules and directories you watch. \
                          Their news reaches your <a href=\"/inbox\">inbox</a> and your <a href=\"/me/settings#feed\">Atom feed</a>."
            .into(),
    };
    View {
        sub: html!("{sub} Only you see this list."),
        actions,
        body: out,
    }
}

#[derive(serde::Serialize)]
struct MineItem<'a> {
    #[serde(flatten)]
    row: crate::catalog::SubjectRow,
    version: Option<String>,
    machine_version: Option<String>,
    why: Vec<&'static str>,
    via_team: Option<&'a str>,
    outputs: Vec<serde_json::Value>,
}

/// What [`view`] lists, as JSON: one filter of `/packages` for its owner.
/// `Filter::All` is the public catalog, so it is not here.
pub(crate) fn value(
    app: &App,
    login: &str,
    f: Filter,
    q: &str,
    pick: Pick,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<serde_json::Value, String> {
    if f == Filter::All {
        return Err("filter=all is the public catalog: use /api/v1/search".into());
    }
    let i = Interests::of(app, login);
    if let Some(n) = pick.set.filter(|n| !i.sets.contains_key(*n)) {
        return Err(format!("no set {n}"));
    }
    let set = pick.set.filter(|_| f == Filter::Machines);
    let output = match (set, pick.output) {
        (_, None) => None,
        (None, Some(_)) => return Err("output needs filter=machines and set".into()),
        (Some(n), Some(o)) => {
            if !i.hosts.values().flatten().any(|(s, l)| s == n && l == o) {
                return Err(format!("the set {n} has no output {o}"));
            }
            Some(o)
        }
    };
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let rows = i.picked(&s, &index, &h, Some(&app.hydra), f, q, set, output);
    let items: Vec<MineItem> = rows
        .iter()
        .map(|r| {
            let why: Vec<&str> = [
                (r.why.machine, "machine"),
                (r.why.maintain, "maintain"),
                (r.why.watch, "watch"),
            ]
            .into_iter()
            .filter_map(|(on, k)| on.then_some(k))
            .collect();
            let (version, machine_version) = match &r.sub {
                Subject::Pkg(a) => (
                    index.pkgs.get(a).map(|p| p.version.clone()),
                    i.versions.get(a).cloned(),
                ),
                _ => (None, None),
            };
            MineItem {
                row: crate::catalog::SubjectRow::new(&r.sub, &r.flags),
                version,
                machine_version,
                why,
                via_team: r.via.as_deref(),
                outputs: r
                    .hosts
                    .iter()
                    .map(|(set, output)| serde_json::json!({ "set": set, "output": output }))
                    .collect(),
            }
        })
        .collect();
    let mut out = crate::api::page(&items, limit, cursor)?;
    out["login"] = serde_json::json!(login);
    out["filter"] = serde_json::json!(f.key());
    out["set"] = serde_json::json!(set);
    out["output"] = serde_json::json!(output);
    out["sets"] = serde_json::json!(i.sets.keys().collect::<Vec<_>>());
    out["counts"] = Filter::ALL
        .iter()
        .filter(|f| **f != Filter::All)
        .map(|f| (f.key().to_string(), serde_json::json!(i.count(*f))))
        .collect::<serde_json::Map<_, _>>()
        .into();
    Ok(out)
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
