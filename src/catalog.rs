//! A page per package, NixOS module and path area: health, then the issues
//! and pull requests attached to it. Anyone may read them; a person who
//! logged in can subscribe, and reads what changed in a private Atom feed.

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;

use crate::actions::Act;
use crate::affected::Verdict;
use crate::area::{Index, area_key, under};
use crate::attrs::{License, Meta};
use crate::channels::{Channel, Channels};
use crate::health::{Advisory, Health, Hydra, Last};
use crate::hygiene;
use crate::links::{module_key, module_name, pkg_key};
use crate::model::{Issue, Pr};
use crate::score::{self, Score};
use crate::severity::{self, Worst};
use crate::ui::{Page, Section, Tone, badge, empty, item_row, kv};
use crate::web::{Shared, Snapshot, ago, esc, icon, octicon, random_id};

const LIST: usize = 100;
const META_GUIDE: &str =
    "https://github.com/NixOS/nixpkgs/blob/master/pkgs/README.md#meta-attributes";
pub(crate) const FEED_DAYS: i64 = 30;
/// At most this many set events reach a feed, so the watched packages keep
/// their place in it.
const SET_EVENTS: usize = 50;
/// Directories a reader starts from; `ci/OWNERS` adds its own.
const TOP_AREAS: [&str; 14] = [
    "lib",
    "doc",
    "ci",
    "maintainers",
    "nixos/lib",
    "nixos/modules",
    "nixos/tests",
    "pkgs/stdenv",
    "pkgs/build-support",
    "pkgs/top-level",
    "pkgs/by-name",
    "pkgs/development/python-modules",
    "pkgs/development/haskell-modules",
    "pkgs/os-specific/linux",
];

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/packages", get(browse))
        .route("/p/:attr", get(package_page))
        .route("/m/*file", get(module_page))
        .route("/a/*path", get(area_page))
        .route("/me/watch", post(add_watch))
        .route("/me/watch/remove", post(remove_watch))
        .route("/me/watch/maintained", post(watch_maintained))
        .route("/me/feed", post(new_feed))
        .route("/feed/:token", get(feed))
        .route("/api/v1/packages/:attr", get(api_package))
        .route("/api/v1/modules/*file", get(api_module))
        .route("/api/v1/areas/*path", get(api_area))
        .route("/api/v1/find/*name", get(api_find))
}

/// Counted once per pull request, for every ancestor of every file it
/// changes, so an area's count is a lookup.
pub fn open_by_dir(prs: &[Pr]) -> HashMap<String, usize> {
    let mut out: HashMap<String, usize> = HashMap::new();
    for pr in prs.iter().filter(|p| p.is_open()) {
        let mut dirs: HashSet<&str> = HashSet::new();
        for f in &pr.files {
            let mut p = f.as_str();
            while let Some((parent, _)) = p.rsplit_once('/') {
                dirs.insert(parent);
                p = parent;
            }
        }
        for d in dirs {
            *out.entry(d.to_string()).or_default() += 1;
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Subject {
    Pkg(String),
    Module(String),
    Area(String),
}

fn valid_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 200
        && !p.contains("..")
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./+".contains(c))
}

pub(crate) fn valid_login(l: &str) -> bool {
    !l.is_empty() && l.len() <= 39 && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// A directory only when something lives under it, so a typo still lists
/// what contains it.
fn exact(s: &Snapshot, index: &Index, q: &str) -> Option<String> {
    match Subject::resolve(index, q)? {
        Subject::Area(p)
            if !s.open_by_dir.contains_key(&p)
                && !index.modules.keys().any(|f| under(f, &p))
                && !index
                    .pkgs
                    .values()
                    .any(|x| x.file.as_deref().is_some_and(|f| under(f, &p))) =>
        {
            None
        }
        sub => Some(sub.href()),
    }
}

/// `ci/OWNERS` names files too; their last segment has an extension.
fn is_file(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|last| last.find('.').is_some_and(|i| i > 0))
}

impl Subject {
    pub fn key(&self) -> String {
        match self {
            Subject::Pkg(a) => pkg_key(a),
            Subject::Module(f) => module_key(f),
            Subject::Area(p) => format!("area:{p}"),
        }
    }

    pub fn parse(key: &str) -> Option<Subject> {
        let (kind, rest) = key.split_once(':')?;
        if !valid_path(rest) {
            return None;
        }
        match kind {
            "pkg" => Some(Subject::Pkg(rest.into())),
            "module" => Some(Subject::Module(rest.into())),
            "area" => Some(Subject::Area(rest.into())),
            _ => None,
        }
    }

    /// What a person types: an attribute, a package name, `nixos/NAME`, a
    /// module file, or a path.
    pub fn resolve(index: &Index, input: &str) -> Option<Subject> {
        let s = input.trim().trim_matches('/');
        if !valid_path(s) {
            return None;
        }
        if let Some(attr) = index.resolve(s) {
            return Some(Subject::Pkg(attr.to_string()));
        }
        if index.modules.contains_key(s) {
            return Some(Subject::Module(s.to_string()));
        }
        if s.starts_with("nixos/") && !s.starts_with("nixos/modules/") {
            if let Some(f) = index.modules.keys().find(|f| module_name(f) == s) {
                return Some(Subject::Module(f.clone()));
            }
        }
        s.contains('/').then(|| Subject::Area(s.to_string()))
    }

    pub fn href(&self) -> String {
        match self {
            Subject::Pkg(a) => format!("/p/{}", esc(a)),
            Subject::Module(f) => format!("/m/{}", esc(f)),
            Subject::Area(p) => format!("/a/{}", esc(p)),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Subject::Pkg(a) => a.clone(),
            Subject::Module(f) => module_name(f),
            Subject::Area(p) => format!("{p}/"),
        }
    }

    pub(crate) fn icon(&self) -> String {
        match self {
            Subject::Pkg(_) => icon(octicon!("package")),
            Subject::Module(_) => icon(octicon!("gear")),
            Subject::Area(_) => icon(octicon!("file-directory")),
        }
    }
}

/// Pull requests and issues of a subject, newest first.
pub(crate) fn attached<'a>(
    s: &'a Snapshot,
    index: &Index,
    subject: &Subject,
) -> (Vec<&'a Pr>, Vec<&'a Issue>) {
    let pr = |n: &u64| s.pr_at.get(n).map(|&i| &s.pr_data[i]);
    let issue = |n: &u64| s.issue_at.get(n).map(|&i| &s.issues[i]);
    match subject {
        Subject::Pkg(_) | Subject::Module(_) => {
            let key = subject.key();
            (
                s.links
                    .prs
                    .get(&key)
                    .into_iter()
                    .flatten()
                    .filter_map(pr)
                    .collect(),
                s.links
                    .issues
                    .get(&key)
                    .into_iter()
                    .flatten()
                    .filter_map(issue)
                    .collect(),
            )
        }
        Subject::Area(path) => {
            let mut prs: Vec<&Pr> = s
                .pr_data
                .iter()
                .filter(|p| p.files.iter().any(|f| under(f, path)))
                .collect();
            prs.sort_by_key(|p| std::cmp::Reverse(p.number));
            let mut numbers: HashSet<u64> = HashSet::new();
            for (attr, p) in &index.pkgs {
                if p.file.as_deref().is_some_and(|f| under(f, path)) {
                    numbers.extend(s.links.issues.get(&pkg_key(attr)).into_iter().flatten());
                }
            }
            for f in index.modules.keys().filter(|f| under(f, path)) {
                numbers.extend(s.links.issues.get(&module_key(f)).into_iter().flatten());
            }
            for i in &s.issues {
                if i.mentions.iter().any(|m| under(m, path)) {
                    numbers.insert(i.number);
                }
            }
            let mut issues: Vec<&Issue> = numbers.iter().filter_map(issue).collect();
            issues.sort_by_key(|i| std::cmp::Reverse(i.number));
            (prs, issues)
        }
    }
}

/// Advisories whose nixpkgs issue is still open, or not synced yet. The sync
/// keeps only issues open or updated in its window, so once every open issue
/// is read, an issue it does not have closed long ago.
pub(crate) fn open_advisories<'a>(h: &'a Health, s: &Snapshot, attr: &str) -> Vec<&'a Advisory> {
    h.advisories
        .get(attr)
        .into_iter()
        .flatten()
        .filter(|a| match a.issue {
            None => true,
            Some(n) => match s.issue_at.get(&n) {
                Some(&i) => s.issues[i].is_open(),
                None => !s.issues_complete,
            },
        })
        .collect()
}

/// The open advisories with their verdict against master and the channels.
pub(crate) fn judged<'a>(
    h: &'a Health,
    s: &Snapshot,
    attr: &str,
) -> Vec<(&'a Advisory, &'a Verdict)> {
    open_advisories(h, s, attr)
        .into_iter()
        .map(|a| (a, h.verdict(attr, &a.code)))
        .collect()
}

/// The open advisories that can affect master: affected, or range unknown.
pub(crate) fn affecting<'a>(h: &'a Health, s: &Snapshot, attr: &str) -> Vec<&'a Advisory> {
    judged(h, s, attr)
        .into_iter()
        .filter(|(_, v)| v.open())
        .map(|(a, _)| a)
        .collect()
}

/// The newest upstream release and its source, unless nixpkgs already
/// carries it: under this attribute, or under another of the same package,
/// like `openssl_3`.
fn upstream_from<'a>(
    s: &Snapshot,
    index: &Index,
    h: &'a Health,
    attr: &str,
) -> Option<(&'a str, &'a str)> {
    let p = index.pkgs.get(attr)?;
    let (newest, from) = h.newest(attr, &p.version)?;
    (!s.versions.get(&p.pname).is_some_and(|v| v.contains(newest))).then_some((newest, from))
}

pub(crate) fn upstream<'a>(
    s: &Snapshot,
    index: &Index,
    h: &'a Health,
    attr: &str,
) -> Option<&'a str> {
    upstream_from(s, index, h, attr).map(|(v, _)| v)
}

/// The text of the upstream badge, the same on every page.
pub(crate) fn upstream_badge(version: &str) -> String {
    format!(
        "<span title=\"Upstream released a newer version than nixpkgs has\">{} available</span>",
        esc(version)
    )
}

fn score_label(s: &Score) -> String {
    let why = if s.reasons.is_empty() {
        "nothing to fix".to_string()
    } else {
        s.reasons
            .iter()
            .map(|r| format!("+{} {}", r.points, r.text))
            .collect::<Vec<_>>()
            .join("; ")
    };
    let why = format!("Maintainer Mode's own score: {why}");
    format!(
        "<span class=\"Label {}\" title=\"{}\">risk {}</span>",
        s.label_class(),
        esc(&why),
        s.risk
    )
}

/// The score of a package from everything the health sources know.
fn score_of(s: &Snapshot, index: &Index, h: &Health, attr: &str) -> Option<Score> {
    let p = index.pkgs.get(attr)?;
    let advisories = judged(h, s, attr);
    let osv: Vec<Worst> = h
        .osv_extra(attr)
        .into_iter()
        .map(|(_, v)| osv_worst(h, v))
        .collect();
    let gaps = hygiene::gaps(attr, p);
    Some(score::score(
        h,
        &score::Input {
            package: p,
            advisories: &advisories,
            osv: &osv,
            upstream: upstream(s, index, h, attr),
            hygiene: gaps.as_deref(),
        },
    ))
}

/// Packages with an open advisory, by the worst band among their advisories.
#[derive(Debug, Default, Serialize, PartialEq)]
pub(crate) struct Bands {
    pub(crate) critical: usize,
    pub(crate) high: usize,
    pub(crate) medium: usize,
    pub(crate) low: usize,
    pub(crate) unscored: usize,
}

/// All of nixpkgs in numbers, one package per package name. Public data
/// only: the index, the health sources and the pull requests.
#[derive(Debug, Default, Serialize, PartialEq)]
pub(crate) struct PackageStats {
    pub(crate) rev: String,
    pub(crate) packages: usize,
    pub(crate) behind_upstream: usize,
    pub(crate) with_advisories: usize,
    pub(crate) advisories_by_band: Bands,
    pub(crate) unmaintained: usize,
    pub(crate) broken: usize,
    pub(crate) insecure: usize,
    pub(crate) with_packaging_gaps: usize,
    pub(crate) open_bumps: usize,
    pub(crate) bumps_median_age_days: Option<f64>,
    /// Packages whose open advisories master already fixes, while a channel
    /// still carries an affected version.
    pub(crate) fixed_in_master_only: usize,
    /// Open tracker issues that master and every channel already fix.
    pub(crate) tracker_issues_to_close: Vec<ToClose>,
}

/// An open tracker issue that nixpkgs already fixes everywhere.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct ToClose {
    pub(crate) attr: String,
    pub(crate) code: String,
    pub(crate) title: String,
    pub(crate) issue: u64,
    pub(crate) fixed_in: Option<String>,
    /// The version in master.
    pub(crate) version: String,
}

/// Every attribute the tracker names, not one per package name: a triager
/// closes each issue.
fn to_close(s: &Snapshot, index: &Index, h: &Health) -> Vec<ToClose> {
    let mut out: Vec<ToClose> = Vec::new();
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    for attr in h.advisories.keys() {
        let Some(p) = index.pkgs.get(attr) else {
            continue;
        };
        for (a, v) in judged(h, s, attr) {
            let (Verdict::Fixed { fixed_in }, Some(issue)) = (v, a.issue) else {
                continue;
            };
            if seen.insert((attr, &a.code)) {
                out.push(ToClose {
                    attr: attr.clone(),
                    code: a.code.clone(),
                    title: a.title.clone(),
                    issue,
                    fixed_in: fixed_in.clone(),
                    version: p.version.clone(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.attr.cmp(&b.attr).then(a.code.cmp(&b.code)));
    out
}

pub(crate) fn package_stats(s: &Snapshot, index: &Index, h: &Health) -> PackageStats {
    let mut out = PackageStats {
        rev: index.rev.clone(),
        ..Default::default()
    };
    for attr in index.by_pname.values() {
        let Some(p) = index.pkgs.get(attr) else {
            continue;
        };
        out.packages += 1;
        if upstream(s, index, h, attr).is_some() {
            out.behind_upstream += 1;
        }
        let all = judged(h, s, attr);
        let open: Vec<&Advisory> = all
            .iter()
            .filter(|(_, v)| v.open())
            .map(|(a, _)| *a)
            .collect();
        let osv = h.osv_extra(attr);
        if open.is_empty()
            && osv.is_empty()
            && all.iter().any(|(_, v)| matches!(v, Verdict::Behind { .. }))
        {
            out.fixed_in_master_only += 1;
        }
        if !open.is_empty() || !osv.is_empty() {
            out.with_advisories += 1;
            let band = rated(h, &open)
                .into_iter()
                .map(|(_, w)| w.band)
                .chain(osv.into_iter().map(|(_, v)| osv_worst(h, v).band))
                .max()
                .flatten();
            let b = &mut out.advisories_by_band;
            *match band {
                Some(severity::Band::Critical) => &mut b.critical,
                Some(severity::Band::High) => &mut b.high,
                Some(severity::Band::Medium) => &mut b.medium,
                Some(severity::Band::Low) => &mut b.low,
                None => &mut b.unscored,
            } += 1;
        }
        if p.maintainers.is_empty() && p.teams.is_empty() {
            out.unmaintained += 1;
        }
        if let Some(f) = &p.facts {
            out.broken += usize::from(f.broken);
            out.insecure += usize::from(f.insecure);
        }
        if hygiene::gaps(attr, p).is_some_and(|g| !g.is_empty()) {
            out.with_packaging_gaps += 1;
        }
    }
    let bumps = crate::bumps::open(s, index, h);
    out.open_bumps = bumps.len();
    out.bumps_median_age_days = crate::bumps::median_age_days(&bumps, Utc::now());
    out.tracker_issues_to_close = to_close(s, index, h);
    out
}

pub(crate) struct Flags {
    pub(crate) upstream: Option<String>,
    /// Open advisories that can affect master, OSV matches included.
    pub(crate) advisories: usize,
    unmaintained: bool,
    pub(crate) open_prs: usize,
    pub(crate) open_issues: usize,
    /// Packages only.
    pub(crate) score: Option<Score>,
    /// The last Hydra build failed, as far as a page view learned it.
    pub(crate) build_failed: bool,
}

impl Flags {
    pub(crate) fn of(s: &Snapshot, index: &Index, h: &Health, subject: &Subject) -> Flags {
        let (prs, issues) = attached(s, index, subject);
        let (upstream, advisories, unmaintained, score) = match subject {
            Subject::Pkg(attr) => {
                let p = index.pkgs.get(attr);
                (
                    upstream(s, index, h, attr).map(String::from),
                    affecting(h, s, attr).len() + h.osv_extra(attr).len(),
                    p.is_some_and(|p| p.maintainers.is_empty() && p.teams.is_empty()),
                    score_of(s, index, h, attr),
                )
            }
            Subject::Module(f) => (
                None,
                0,
                index
                    .modules
                    .get(f)
                    .is_some_and(|m| m.maintainers.is_empty() && m.teams.is_empty()),
                None,
            ),
            Subject::Area(_) => (None, 0, false, None),
        };
        Flags {
            upstream,
            advisories,
            unmaintained,
            open_prs: prs.iter().filter(|p| p.is_open()).count(),
            open_issues: issues.iter().filter(|i| i.is_open()).count(),
            score,
            build_failed: false,
        }
    }

    pub(crate) fn risk(&self) -> u32 {
        self.score.as_ref().map_or(0, |s| s.risk)
    }

    /// Worst first: the risk, then what it is made of, then activity.
    pub(crate) fn rank(&self) -> impl Ord {
        (
            std::cmp::Reverse(self.risk()),
            std::cmp::Reverse(self.advisories),
            self.upstream.is_none(),
            std::cmp::Reverse(self.open_prs + self.open_issues),
        )
    }

    pub(crate) fn badges(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(s) = &self.score {
            out.push(score_label(s));
        }
        if self.advisories > 0 {
            out.push(badge(
                Tone::Danger,
                &format!(
                    "{} {} advisor{}",
                    icon(octicon!("shield")),
                    self.advisories,
                    if self.advisories == 1 { "y" } else { "ies" }
                ),
            ));
        }
        if self.build_failed {
            out.push(badge(
                Tone::Danger,
                &format!("{} build fails", icon(octicon!("x-circle-fill"))),
            ));
        }
        if let Some(u) = &self.upstream {
            out.push(badge(Tone::Attention, &upstream_badge(u)));
        }
        out
    }

    /// Counts that are zero are left out.
    pub(crate) fn meta(&self) -> String {
        let mut out = Vec::new();
        match self.open_prs {
            0 => {}
            1 => out.push("1 open PR".to_string()),
            n => out.push(format!("{n} open PRs")),
        }
        match self.open_issues {
            0 => {}
            1 => out.push("1 issue".to_string()),
            n => out.push(format!("{n} issues")),
        }
        if self.unmaintained {
            out.push("no maintainer".to_string());
        }
        out.join(" · ")
    }

    /// badges() puts the upstream badge last.
    pub(crate) fn html(&self) -> String {
        let mut out = self.badges();
        let upstream = self.upstream.as_ref().and_then(|_| out.pop());
        let meta = self.meta();
        if !meta.is_empty() {
            out.push(format!("<span class=\"f6 color-fg-muted\">{meta}</span>"));
        }
        out.extend(upstream);
        out.join(" · ")
    }

    pub(crate) fn json(&self) -> Value {
        json!({
            "risk": self.score.as_ref().map(|s| s.risk),
            "risk_band": self.score.as_ref().map(|s| s.band()),
            "risk_reasons": self.score.as_ref().map(|s| &s.reasons),
            "upstream": self.upstream,
            "open_advisories": self.advisories,
            "unmaintained": self.unmaintained,
            "open_prs": self.open_prs,
            "open_issues": self.open_issues,
        })
    }
}

pub(crate) fn pr_row(p: &Pr, act: Act) -> String {
    use crate::ui::Kind;
    let kind = match p.state.as_str() {
        "MERGED" => Kind::PR_MERGED,
        "CLOSED" => Kind::PR_CLOSED,
        _ => Kind::PR_OPEN,
    };
    let ci = match p.ci.as_deref() {
        Some("SUCCESS") if p.is_open() => " · checks pass",
        Some("FAILURE" | "ERROR") if p.is_open() => " · checks fail",
        _ => "",
    };
    let when = match (p.merged_at, p.closed_at) {
        (Some(t), _) => format!("merged {} ago", ago(t)),
        (None, Some(t)) => format!("closed {} ago", ago(t)),
        _ => format!("opened {} ago", ago(p.created_at)),
    };
    let badges = if p.is_draft {
        vec![badge(Tone::Neutral, "draft")]
    } else {
        Vec::new()
    };
    item_row(
        Some(kind),
        &crate::prpage::title_link(p.number, &esc(&p.title), "Link--primary text-bold"),
        "",
        &format!(
            "#{n} by <a href=\"/u/{a}\">{a}</a> · {when}{ci}",
            n = p.number,
            a = esc(&p.author)
        ),
        &badges,
        &if p.is_open() {
            act.buttons(p.number)
        } else {
            String::new()
        },
    )
}

fn issue_row(i: &Issue) -> String {
    use crate::ui::Kind;
    let when = match i.closed_at {
        Some(t) if !i.is_open() => format!("closed {} ago", ago(t)),
        _ => format!("opened {} ago", ago(i.created_at)),
    };
    let labels: Vec<String> = i
        .labels
        .iter()
        .filter(|l| !l.starts_with("0.kind:") && !l.starts_with("9."))
        .map(|l| badge(Tone::Neutral, &esc(l)))
        .collect();
    item_row(
        Some(if i.is_open() {
            Kind::ISSUE_OPEN
        } else {
            Kind::ISSUE_CLOSED
        }),
        &esc(&i.title),
        &esc(&i.url),
        &format!(
            "#{} by {} · {when} · {} comments",
            i.number,
            crate::web::person_link(&i.author),
            i.comments
        ),
        &labels,
        "",
    )
}

/// Open items in a box, closed ones folded under it.
fn section<T>(
    title: &str,
    items: &[&T],
    open: impl Fn(&T) -> bool,
    row: impl Fn(&T) -> String,
) -> String {
    let (o, c): (Vec<&&T>, Vec<&&T>) = items.iter().partition(|x| open(x));
    let mut body: String = o.iter().take(LIST).map(|x| row(x)).collect();
    if o.is_empty() {
        body = empty("None open.");
    }
    if o.len() > LIST {
        let _ = write!(
            body,
            "<div class=\"Box-footer f6 color-fg-muted\">{} more in the JSON API</div>",
            o.len() - LIST
        );
    }
    let mut out = crate::ui::section(title, Some(o.len()), None, &body);
    if !c.is_empty() {
        let _ = write!(
            out,
            "<details class=\"Box mb-4\"><summary class=\"Box-header\">Closed or merged in the last {} days <span class=\"Counter\">{}</span></summary>",
            crate::ledger::WINDOW_DAYS,
            c.len()
        );
        for x in c.iter().take(LIST) {
            out.push_str(&row(x));
        }
        out.push_str("</details>");
    }
    out
}

/// `after_title` is HTML that follows the link, such as a version.
pub(crate) fn subject_row(sub: &Subject, f: &Flags, after_title: &str, status: &str) -> String {
    item_row(
        None,
        &format!(
            "<a class=\"Link--primary text-bold\" href=\"{}\">{} {}</a>{after_title}",
            sub.href(),
            sub.icon(),
            esc(&sub.label())
        ),
        "",
        &f.meta(),
        &f.badges(),
        status,
    )
}

fn people_links(logins: &[String]) -> String {
    if logins.is_empty() {
        return "<span class=\"color-fg-muted\">none</span>".into();
    }
    logins
        .iter()
        .map(|l| crate::web::person_link(l))
        .collect::<Vec<_>>()
        .join(", ")
}

fn teams_html(index: &Index, teams: &[String]) -> String {
    teams
        .iter()
        .map(|t| {
            format!(
                "{} ({})",
                esc(t),
                people_links(index.teams.get(t).map(Vec::as_slice).unwrap_or(&[]))
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn source_link(rev: &str, file: &str) -> String {
    let r = if rev.is_empty() { "master" } else { rev };
    format!(
        "<a class=\"branch-name\" href=\"https://github.com/NixOS/nixpkgs/blob/{}/{f}\">{f}</a>",
        esc(r),
        f = esc(file)
    )
}

fn subscribe_button(watching: Option<bool>, subject: &Subject) -> String {
    match watching {
        None => "<a class=\"btn btn-sm\" href=\"/auth/login\">Sign in to watch</a>".into(),
        Some(false) => format!(
            "<form method=\"post\" action=\"/me/watch\" class=\"d-inline\"><input type=\"hidden\" name=\"key\" value=\"{}\">\
             <button class=\"btn btn-sm btn-primary\" type=\"submit\">{} Watch</button></form>",
            esc(&subject.key()),
            icon(octicon!("bell"))
        ),
        Some(true) => format!(
            "<form method=\"post\" action=\"/me/watch/remove\" class=\"d-inline\"><input type=\"hidden\" name=\"key\" value=\"{}\">\
             <button class=\"btn btn-sm\" type=\"submit\">Stop watching</button></form>",
            esc(&subject.key())
        ),
    }
}

fn watching(app: &crate::web::App, viewer: Option<&str>, subject: &Subject) -> Option<bool> {
    let v = viewer?;
    Some(
        app.store
            .watched(v)
            .unwrap_or_default()
            .contains(&subject.key()),
    )
}

fn build_row(label: &str, job: &str, last: &Last) -> String {
    let link = format!("<a href=\"{}\">{}</a>", esc(&Hydra::job_url(job)), esc(job));
    let body = match last {
        Last::Build(b) => format!(
            "<a href=\"{}\" class=\"{}\">{}</a> <span class=\"color-fg-muted\">{}</span>",
            b.url(),
            if b.ok {
                "color-fg-success"
            } else {
                "color-fg-danger"
            },
            b.status,
            b.at.map(|t| format!("{} ago", ago(t))).unwrap_or_default()
        ),
        Last::Missing => {
            format!("<span class=\"color-fg-muted\">no finished build of {link}</span>")
        }
        Last::NoAnswer => {
            format!("<span class=\"color-fg-muted\">Hydra did not answer; see {link}</span>")
        }
        Last::NotAsked => format!(
            "<span class=\"color-fg-muted\">not asked yet: this server asks Hydra a few times a minute; see {link}</span>"
        ),
    };
    kv(label, &body)
}

fn severity_html(w: &Worst) -> String {
    let mut out = String::new();
    if let Some(b) = w.band {
        let _ = write!(
            out,
            " <span class=\"Label {}\" title=\"CVSS base score{}\">{}{}</span>",
            b.label_class(),
            match w.from.as_deref() {
                Some("nvd") => ", from NVD",
                Some("cve") => ", from the CVE record",
                _ => "",
            },
            b.word(),
            w.cvss.map(|s| format!(" {s:.1}")).unwrap_or_default()
        );
    }
    match w.exploitation.as_deref() {
        Some("active") => out.push_str(
            " <span class=\"Label Label--danger\" title=\"CISA SSVC\">exploited in the wild</span>",
        ),
        Some("poc") => out.push_str(
            " <span class=\"Label Label--attention\" title=\"CISA SSVC\">public exploit</span>",
        ),
        _ => {}
    }
    if let Some(e) = w.epss {
        let _ = write!(
            out,
            " <span class=\"Label\" title=\"chance of exploitation in the next 30 days (EPSS){}\">EPSS {:.1}%</span>",
            w.percentile
                .map(|p| format!(", higher than {:.0}% of all CVEs", p * 100.0))
                .unwrap_or_default(),
            e * 100.0
        );
    }
    out
}

/// The tracker's advisories with the worst severity of their CVEs, the
/// worst first.
fn rated<'a>(h: &Health, list: &[&'a Advisory]) -> Vec<(&'a Advisory, Worst)> {
    let mut v: Vec<(&Advisory, Worst)> = list
        .iter()
        .map(|a| (*a, severity::worst(h, &a.cves)))
        .collect();
    v.sort_by(|(_, a), (_, b)| {
        b.band.cmp(&a.band).then(
            b.epss
                .partial_cmp(&a.epss)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    v
}

fn packaging_html(attr: &str, p: &crate::area::Package) -> String {
    let Some(gaps) = hygiene::gaps(attr, p) else {
        return "<span class=\"color-fg-muted f6\">not read yet: the next index update reads it</span>"
            .into();
    };
    let mut out: Vec<String> = Vec::new();
    if let Some(f) = &p.facts {
        if f.broken {
            out.push("<span class=\"Label Label--danger\">meta.broken</span>".into());
        }
        if f.insecure {
            out.push("<span class=\"Label Label--danger\">meta.knownVulnerabilities</span>".into());
        }
    }
    out.extend(
        gaps.iter()
            .map(|g| format!("<span class=\"Label Label--attention\">{}</span>", esc(g))),
    );
    if out.is_empty() {
        return "<span class=\"color-fg-success\">meta complete</span>".into();
    }
    out.join(" ")
}

/// An OSV match: its own band, or the worst of its CVEs when one is known.
pub(crate) fn osv_worst(h: &Health, v: Option<&crate::osv::Vuln>) -> Worst {
    let Some(v) = v else {
        return Worst::default();
    };
    let mut w = severity::worst(h, &v.cves);
    if w.band.is_none() {
        w.band = v.band;
    }
    w
}

/// Advisories whose nixpkgs issue closed in the synced window: fixed in
/// master, as far as the tracker knows.
fn closed_advisories<'a>(h: &'a Health, s: &Snapshot, attr: &str) -> Vec<&'a Advisory> {
    h.advisories
        .get(attr)
        .into_iter()
        .flatten()
        .filter(|a| {
            a.issue
                .and_then(|n| s.issue_at.get(&n))
                .is_some_and(|&i| !s.issues[i].is_open())
        })
        .collect()
}

fn channel_names(list: &[&Channel]) -> String {
    list.iter()
        .map(|c| c.label())
        .collect::<Vec<_>>()
        .join(", ")
}

fn hover(list: &[&Channel], attr: &str, master: &str) -> String {
    let has: Vec<String> = list
        .iter()
        .map(|c| {
            format!(
                "{} has {}",
                c.label(),
                c.versions.get(attr).map_or("?", String::as_str)
            )
        })
        .collect();
    format!("master has {master}; {}", has.join("; "))
}

fn backport_label(backport: &[&Channel], attr: &str, master: &str) -> String {
    if backport.is_empty() {
        return String::new();
    }
    format!(
        " <span class=\"Label Label--attention\" title=\"{}\">needs backport to {}</span>",
        esc(&hover(backport, attr, master)),
        esc(&channel_names(backport))
    )
}

fn channels_html(ch: &Channels, attr: &str, master: &str) -> String {
    let seen = ch.of(attr, master);
    if seen.is_empty() {
        return "<span class=\"color-fg-muted\">no channel read yet</span>".into();
    }
    let mut parts = vec![format!("master {}", esc(master))];
    for s in &seen {
        parts.push(format!(
            "<a href=\"{}\">{}</a> {}",
            esc(&s.channel.url()),
            esc(s.channel.label()),
            match s.version {
                Some(v) if s.behind =>
                    format!("<span class=\"color-fg-attention\">{}</span>", esc(v)),
                Some(v) => esc(v),
                None => "<span class=\"color-fg-muted\">absent</span>".into(),
            }
        ));
    }
    let behind: Vec<&Channel> = seen
        .iter()
        .filter(|s| s.behind)
        .map(|s| s.channel)
        .collect();
    let tail = if behind.is_empty() {
        " <span class=\"color-fg-muted f6\">no channel is behind master</span>".to_string()
    } else {
        format!(
            " <span class=\"Label Label--attention\">master is ahead of {}</span>",
            esc(&channel_names(&behind))
        )
    };
    format!("{}{tail}", parts.join(" · "))
}

fn issue_link(a: &Advisory) -> String {
    a.issue_url()
        .map(|u| {
            format!(
                " · <a href=\"{}\">#{}</a>",
                esc(&u),
                a.issue.unwrap_or_default()
            )
        })
        .unwrap_or_default()
}

/// What a verdict asks of a maintainer, as one line.
pub(crate) fn verdict_html(a: &Advisory, v: &Verdict, master: &str) -> String {
    match v {
        Verdict::Affects => format!(
            "<span class=\"color-fg-danger\">affects {}</span>",
            esc(master)
        ),
        Verdict::Unknown => "<span class=\"color-fg-attention\" title=\"The tracker gives no affected version that Maintainer Mode can read\">range unknown</span>".into(),
        Verdict::Behind { fixed_in, channels } => {
            let fixed = fixed_in
                .as_deref()
                .map_or_else(|| "fixed in master".to_string(), |f| format!("fixed in {} in master", esc(f)));
            let lags: Vec<String> = channels
                .iter()
                .map(|l| {
                    let what = format!("{} has {}", esc(&l.channel), esc(&l.version));
                    if l.stable {
                        format!("<span class=\"Label Label--attention\">needs backport: {what}</span>")
                    } else {
                        format!("<span class=\"color-fg-attention\">{what}</span>")
                    }
                })
                .collect();
            format!(
                "<span class=\"color-fg-success\">{fixed}</span> · {}",
                lags.join(" · ")
            )
        }
        Verdict::Fixed { fixed_in } => {
            let fixed = fixed_in
                .as_deref()
                .map_or_else(|| format!("not affected: master has {}", esc(master)), |f| format!("fixed in {}", esc(f)));
            let close = match (a.issue, a.issue_url()) {
                (Some(n), Some(u)) => format!(
                    "<a href=\"{}\">the tracker issue #{n} can be closed</a>",
                    esc(&u)
                ),
                _ => format!(
                    "<a href=\"{}\">the tracker advisory can be closed</a>",
                    esc(&a.url())
                ),
            };
            format!("<span class=\"color-fg-success\">{fixed}</span> · {close}")
        }
    }
}

fn verdict_order(v: &Verdict) -> u8 {
    match v {
        Verdict::Affects => 0,
        Verdict::Unknown => 1,
        Verdict::Behind { .. } => 2,
        Verdict::Fixed { .. } => 3,
    }
}

fn advisories_html(
    h: &Health,
    list: &[(&Advisory, &Verdict)],
    osv: &[(&str, Option<&crate::osv::Vuln>)],
    fixed: &[&Advisory],
    backport: &[&Channel],
    attr: &str,
    master: &str,
) -> String {
    let hint = backport_label(backport, attr, master);
    let fixed: Vec<&&Advisory> = fixed.iter().filter(|_| !hint.is_empty()).collect();
    if list.is_empty() && osv.is_empty() && fixed.is_empty() {
        return "<span class=\"color-fg-success\">none open</span>".into();
    }
    let fixed_html: String = fixed
        .iter()
        .map(|a| {
            format!(
                "<div><a href=\"{}\">{}</a> {} <span class=\"color-fg-muted f6\">fixed in master</span>{hint}</div>",
                esc(&a.url()),
                esc(&a.code),
                esc(&a.title),
            )
        })
        .collect();
    let advisories: Vec<&Advisory> = list.iter().map(|(a, _)| *a).collect();
    let mut rows: Vec<(&Advisory, Worst, &Verdict)> = rated(h, &advisories)
        .into_iter()
        .map(|(a, w)| {
            let v = list
                .iter()
                .find(|(x, _)| std::ptr::eq(*x, a))
                .map_or(&crate::affected::UNKNOWN, |(_, v)| *v);
            (a, w, v)
        })
        .collect();
    rows.sort_by_key(|(_, _, v)| verdict_order(v));
    let tracker = rows.into_iter().map(|(a, w, v)| {
        let class = if v.open() { " class=\"color-fg-danger\"" } else { "" };
        let cves = if a.cves.is_empty() {
            String::new()
        } else {
            format!("{} · ", esc(&a.cves.join(", ")))
        };
        format!(
            "<div><a{class} href=\"{}\">{}</a>{} {}{}<div class=\"f6 color-fg-muted\">{cves}{}</div></div>",
            esc(&a.url()),
            esc(&a.code),
            severity_html(&w),
            esc(&a.title),
            issue_link(a),
            verdict_html(a, v, master)
        )
    });
    let osv = osv.iter().map(|(id, v)| {
        format!(
            "<div><a class=\"color-fg-danger\" href=\"{}\">{}</a>{} {} <span class=\"f6 color-fg-muted\">(OSV, for this version)</span><div class=\"f6 color-fg-muted\">{}</div></div>",
            esc(&crate::osv::Vuln::url(id)),
            esc(id),
            severity_html(&osv_worst(h, *v)),
            esc(v.and_then(|v| v.summary.as_deref()).unwrap_or("")),
            esc(&v.map(|v| v.cves.join(", ")).unwrap_or_default())
        )
    });
    tracker
        .chain(osv)
        .chain(std::iter::once(fixed_html))
        .collect()
}

/// A link only for http and https: `meta` is nixpkgs code, and a
/// `javascript:` homepage must not run on this site.
fn url_html(u: &str) -> String {
    if u.starts_with("https://") || u.starts_with("http://") {
        format!("<a href=\"{0}\">{0}</a>", esc(u))
    } else {
        esc(u)
    }
}

fn urls_html(list: &[String]) -> String {
    list.iter()
        .map(|u| url_html(u))
        .collect::<Vec<_>>()
        .join("<br>")
}

fn license_html(l: &License) -> String {
    let name = match (&l.spdx, &l.name) {
        (Some(id), _) => format!(
            "<a href=\"https://spdx.org/licenses/{0}.html\">{0}</a>",
            esc(id)
        ),
        (None, Some(n)) => esc(n),
        (None, None) => "unnamed".into(),
    };
    if l.free || l.name.as_deref() == Some("unfree") {
        name
    } else {
        format!("{name} <span class=\"Label Label--attention\">unfree</span>")
    }
}

fn platforms_html(m: &Meta) -> String {
    let mut out = match &m.platforms {
        None => "<span class=\"color-fg-muted\">not set</span>".to_string(),
        Some(p) => {
            let mut kinds: Vec<&str> = Vec::new();
            if p.linux {
                kinds.push("Linux");
            }
            if p.darwin {
                kinds.push("Darwin");
            }
            format!(
                "{} platform{}{}",
                p.count,
                if p.count == 1 { "" } else { "s" },
                if kinds.is_empty() {
                    String::new()
                } else {
                    format!(": {}", kinds.join(", "))
                }
            )
        }
    };
    if !m.bad_platforms.is_empty() {
        let _ = write!(
            out,
            " · not on {}",
            m.bad_platforms
                .iter()
                .map(|p| format!("<code>{}</code>", esc(p)))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    out
}

fn codes(list: &[String]) -> String {
    list.iter()
        .map(|c| format!("<code>{}</code>", esc(c)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn status_html(f: Option<&crate::attrs::Facts>, m: &Meta) -> String {
    let mut out: Vec<String> = Vec::new();
    if f.is_some_and(|f| f.broken) {
        out.push("<span class=\"Label Label--danger\">broken</span>".into());
    }
    if !m.known_vulnerabilities.is_empty() {
        out.push(format!(
            "<span class=\"Label Label--danger\">insecure</span><ul class=\"f6 ml-3\">{}</ul>",
            m.known_vulnerabilities
                .iter()
                .map(|v| format!("<li class=\"pre-wrap\">{}</li>", esc(v.trim())))
                .collect::<String>()
        ));
    }
    if m.unfree() {
        out.push("<span class=\"Label Label--attention\">unfree</span>".into());
    }
    if out.is_empty() {
        return "<span class=\"color-fg-success\">not broken, not insecure, free</span>".into();
    }
    out.join(" ")
}

/// The "Package" box: what nixpkgs says about the package, as the sweep read it.
fn package_box(index: &Index, rev: &str, p: &crate::area::Package) -> String {
    let mut out = format!(
        "<div class=\"Box mb-4\"><div class=\"Box-header d-flex\"><span class=\"text-bold flex-auto\">Package</span>\
         <a class=\"f6\" href=\"{META_GUIDE}\">what each field means</a></div>"
    );
    let description = p.facts.as_ref().and_then(|f| f.description.as_deref());
    out.push_str(&kv(
        "Description",
        &description
            .map(esc)
            .unwrap_or_else(|| "<span class=\"color-fg-muted\">none</span>".into()),
    ));
    match &p.meta {
        None => out.push_str(&kv(
            "meta",
            "<span class=\"color-fg-muted f6\">not read yet: the next index update reads it</span>",
        )),
        Some(m) => {
            if let Some(d) = &m.long_description {
                out.push_str(&kv(
                    "Long description",
                    &format!("<div class=\"f6 pre-wrap\">{}</div>", esc(d.trim_end())),
                ));
            }
            let none = "<span class=\"color-fg-muted\">none</span>";
            out.push_str(&kv(
                "Homepage",
                &if m.homepage.is_empty() {
                    none.into()
                } else {
                    urls_html(&m.homepage)
                },
            ));
            if let Some(d) = &m.download_page {
                out.push_str(&kv("Download page", &url_html(d)));
            }
            if !m.changelog.is_empty() {
                out.push_str(&kv("Changelog", &urls_html(&m.changelog)));
            }
            out.push_str(&kv(
                "License",
                &if m.licenses.is_empty() {
                    none.into()
                } else {
                    m.licenses
                        .iter()
                        .map(license_html)
                        .collect::<Vec<_>>()
                        .join(", ")
                },
            ));
            out.push_str(&kv("Platforms", &platforms_html(m)));
            if let Some(b) = &m.main_program {
                out.push_str(&kv("Main program", &format!("<code>{}</code>", esc(b))));
            }
            out.push_str(&kv(
                "Source provenance",
                &if m.source_provenance.is_empty() {
                    "<span class=\"color-fg-muted\">not set: built from source</span>".into()
                } else {
                    codes(&m.source_provenance)
                },
            ));
            if !m.outputs.is_empty() {
                out.push_str(&kv("Outputs", &codes(&m.outputs)));
            }
            out.push_str(&kv("Status", &status_html(p.facts.as_ref(), m)));
            if let Some((file, line)) = m.file_line() {
                let r = if rev.is_empty() { "master" } else { rev };
                out.push_str(&kv(
                    "Position",
                    &format!(
                        "<a class=\"branch-name\" href=\"https://github.com/NixOS/nixpkgs/blob/{}/{}#L{line}\">{}:{line}</a>",
                        esc(r),
                        esc(file),
                        esc(file)
                    ),
                ));
            }
        }
    }
    out.push_str(&kv("Maintainers", &people_links(&p.maintainers)));
    if !p.teams.is_empty() {
        out.push_str(&kv("Teams", &teams_html(index, &p.teams)));
    }
    out.push_str("</div>");
    out
}

fn not_found(app: &crate::web::App, viewer: Option<&str>, what: &str) -> Response {
    let body = format!(
        "<div class=\"blankslate\"><p>{} is not in the nixpkgs index{}.</p><p><a href=\"/packages\">Search packages, modules and directories</a></p></div>",
        esc(what),
        if app.index.read().unwrap().rev.is_empty() {
            " yet: the first index is still being built"
        } else {
            ""
        }
    );
    (
        StatusCode::NOT_FOUND,
        Page::new(Section::Packages, "Not found")
            .body(body)
            .render(app, viewer),
    )
        .into_response()
}

async fn package_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    let viewer = app.viewer(&headers);
    let (attr, p, rev) = {
        let index = app.index.read().unwrap();
        let Some(attr) = index.resolve(&name).map(String::from) else {
            drop(index);
            return not_found(&app, viewer.as_deref(), &name);
        };
        let p = index.pkgs.get(&attr).cloned().unwrap_or_default();
        (attr, p, index.rev.clone())
    };
    if attr != name {
        return Redirect::to(&format!("/p/{attr}")).into_response();
    }
    let subject = Subject::Pkg(attr.clone());
    let job = Hydra::package_job(&attr);
    let build = app.hydra.last(&job).await;
    let next = crate::needs::here(&app, &headers, &subject.key());
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let (prs, issues) = attached(&s, &index, &subject);
    let page = Page::new(Section::Packages, attr.clone())
        .icon(octicon!("package"))
        .sub(format!(
            "{} {}{}",
            esc(&p.pname),
            esc(&p.version),
            p.file
                .as_deref()
                .map(|f| format!(" · {}", source_link(&rev, f)))
                .unwrap_or_default()
        ))
        .actions(subscribe_button(watching(&app, viewer.as_deref(), &subject), &subject) + &next);
    let mut out = String::new();
    let mut health = String::new();
    if let Some(sc) = score_of(&s, &index, &h, &attr) {
        let reasons: String = sc
            .reasons
            .iter()
            .map(|r| format!("<li>+{} {}</li>", r.points, esc(&r.text)))
            .collect();
        health.push_str(&kv(
            "Risk",
            &format!(
                "{} {}{}",
                score_label(&sc),
                sc.band(),
                if reasons.is_empty() {
                    String::new()
                } else {
                    format!("<ul class=\"f6 color-fg-muted ml-3\">{reasons}</ul>")
                }
            ),
        ));
    }
    let version = match upstream_from(&s, &index, &h, &attr) {
        Some((u, from)) => format!(
            "{} <span class=\"Label Label--attention\" title=\"Upstream released a newer version, from {}\">{} available</span>",
            esc(&p.version),
            esc(from),
            esc(u)
        ),
        None => format!(
            "{} <span class=\"color-fg-muted f6\">{}</span>",
            esc(&p.version),
            match h.direct.get(&attr) {
                Some(d) if d.latest.is_some() => format!(
                    "no newer release in {}",
                    if d.from == "tags" {
                        "its git tags".into()
                    } else {
                        esc(&d.from)
                    }
                ),
                _ if h.repology_at.is_some() => "no newer release known to repology".into(),
                _ => "no upstream check yet".into(),
            }
        ),
    };
    health.push_str(&kv("Version", &version));
    let ch = app.channels.read().unwrap().clone();
    health.push_str(&kv("Channels", &channels_html(&ch, &attr, &p.version)));
    health.push_str(&build_row("Hydra", &job, &build));
    health.push_str(&kv(
        "Advisories",
        &advisories_html(
            &h,
            &judged(&h, &s, &attr),
            &h.osv_extra(&attr),
            &closed_advisories(&h, &s, &attr),
            &ch.backport(&attr, &p.version),
            &attr,
            &p.version,
        ),
    ));
    health.push_str(&kv("Packaging", &packaging_html(&attr, &p)));
    if let Some(v) = viewer.as_deref() {
        health.push_str(&kv(
            "Update",
            &crate::actions::update_form(
                &attr,
                upstream(&s, &index, &h, &attr),
                crate::actions::ready(&app, v, crate::workflows::Needed::Update),
            ),
        ));
    }
    out.push_str(&crate::ui::section("Health", None, None, &health));
    out.push_str(&package_box(&index, &rev, &p));
    let act = Act::of(&app, viewer.as_deref());
    out.push_str(&crate::bumps::package_section(
        &s, &index, &h, &attr, &prs, act,
    ));
    out.push_str(&section(
        "Pull requests",
        &prs,
        |p| p.is_open(),
        |p| pr_row(p, act),
    ));
    out.push_str(&section("Issues", &issues, |i| i.is_open(), issue_row));
    page.body(out).render(&app, viewer.as_deref())
}

async fn module_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(file): Path<String>,
) -> Response {
    let viewer = app.viewer(&headers);
    let (m, rev) = {
        let index = app.index.read().unwrap();
        match index.modules.get(&file) {
            Some(m) => (m.clone(), index.rev.clone()),
            None => {
                drop(index);
                return not_found(&app, viewer.as_deref(), &file);
            }
        }
    };
    let subject = Subject::Module(file.clone());
    let name = module_name(&file);
    let job = Hydra::test_job(name.trim_start_matches("nixos/"));
    let build = app.hydra.last(&job).await;
    let s = app.snap();
    let index = app.index.read().unwrap();
    let (prs, issues) = attached(&s, &index, &subject);
    let page = Page::new(Section::Packages, name.clone())
        .icon(octicon!("gear"))
        .sub(format!("NixOS module · {}", source_link(&rev, &file)))
        .actions(subscribe_button(
            watching(&app, viewer.as_deref(), &subject),
            &subject,
        ));
    let mut out = String::new();
    let mut health = String::new();
    health.push_str(&build_row("NixOS test", &job, &build));
    health.push_str(&kv("Maintainers", &people_links(&m.maintainers)));
    if !m.teams.is_empty() {
        health.push_str(&kv("Teams", &teams_html(&index, &m.teams)));
    }
    health.push_str(&kv(
        "Directory",
        &format!(
            "<a href=\"/a/{0}\">{0}/</a>",
            esc(area_key(&file)
                .rsplit_once('/')
                .map_or("nixos/modules", |(d, _)| d))
        ),
    ));
    out.push_str(&crate::ui::section("Health", None, None, &health));
    let act = Act::of(&app, viewer.as_deref());
    out.push_str(&section(
        "Pull requests",
        &prs,
        |p| p.is_open(),
        |p| pr_row(p, act),
    ));
    out.push_str(&section("Issues", &issues, |i| i.is_open(), issue_row));
    page.body(out).render(&app, viewer.as_deref())
}

/// Packages and modules under a path, the ones that need attention first.
/// One row per package name and file: an alias shares both with its target,
/// while `python3Packages.protobuf` shares only the name with `protobuf`.
fn contents(index: &Index, s: &Snapshot, h: &Health, path: &str) -> Vec<(Subject, Flags)> {
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    let mut pkgs: Vec<&String> = Vec::new();
    for canonical in [true, false] {
        for (attr, p) in &index.pkgs {
            let Some(f) = p.file.as_deref().filter(|f| under(f, path)) else {
                continue;
            };
            if (index.by_pname.get(&p.pname) == Some(attr)) == canonical
                && seen.insert((&p.pname, f))
            {
                pkgs.push(attr);
            }
        }
    }
    let mut v: Vec<(Subject, Flags)> = pkgs
        .into_iter()
        .map(|attr| Subject::Pkg(attr.clone()))
        .chain(
            index
                .modules
                .keys()
                .filter(|f| under(f, path))
                .map(|f| Subject::Module(f.clone())),
        )
        .map(|sub| {
            let f = Flags::of(s, index, h, &sub);
            (sub, f)
        })
        .collect();
    v.sort_by_key(|(sub, f)| (f.rank(), sub.label()));
    v
}

fn subject_rows(rows: &[(Subject, Flags)]) -> String {
    let mut out = String::new();
    for (sub, f) in rows.iter().take(LIST) {
        out.push_str(&subject_row(sub, f, "", ""));
    }
    if rows.len() > LIST {
        let _ = write!(
            out,
            "<div class=\"Box-footer f6 color-fg-muted\">{} more in the JSON API</div>",
            rows.len() - LIST
        );
    }
    out
}

async fn area_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(path): Path<String>,
) -> Response {
    let viewer = app.viewer(&headers);
    let path = path.trim_matches('/').to_string();
    if !valid_path(&path) {
        return (StatusCode::BAD_REQUEST, "not a nixpkgs path").into_response();
    }
    let subject = Subject::Area(path.clone());
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let (prs, issues) = attached(&s, &index, &subject);
    let rows = contents(&index, &s, &h, &path);
    let owners: Vec<String> = index
        .owners
        .iter()
        .filter(|(p, _)| under(&path, p) || under(p, &path))
        .map(|(p, l)| format!("<a href=\"/a/{0}\">{0}</a> {1}", esc(p), people_links(l)))
        .collect();
    let parent = path
        .rsplit_once('/')
        .map(|(p, _)| format!("<a href=\"/a/{0}\">{0}/</a> · ", esc(p)))
        .unwrap_or_default();
    let page = Page::new(Section::Packages, format!("{path}/"))
        .icon(octicon!("file-directory"))
        .sub(format!(
            "{parent}{} packages and modules · {} open pull requests",
            rows.len(),
            s.open_by_dir.get(&path).copied().unwrap_or(0)
        ))
        .actions(subscribe_button(
            watching(&app, viewer.as_deref(), &subject),
            &subject,
        ));
    let mut out = String::new();
    let prefix = format!("{path}/");
    let mut children: Vec<(&String, &usize)> = s
        .open_by_dir
        .iter()
        .filter(|(d, _)| d.strip_prefix(&prefix).is_some_and(|r| !r.contains('/')))
        .collect();
    children.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    if !owners.is_empty() || !children.is_empty() {
        out.push_str("<div class=\"Box mb-4\">");
        if !owners.is_empty() {
            out.push_str(&kv("ci/OWNERS", &owners.join("<br>")));
        }
        if !children.is_empty() {
            let list: Vec<String> = children
                .iter()
                .take(40)
                .map(|(d, n)| {
                    format!(
                        "<a href=\"/a/{0}\">{1}</a>&nbsp;<span class=\"color-fg-muted\">{n}</span>",
                        esc(d),
                        esc(d.rsplit('/').next().unwrap_or(d))
                    )
                })
                .collect();
            out.push_str(&kv("Most open pull requests", &list.join(" · ")));
        }
        out.push_str("</div>");
    }
    if !rows.is_empty() {
        out.push_str(&crate::ui::section(
            "Packages and modules",
            Some(rows.len()),
            None,
            &subject_rows(&rows),
        ));
    }
    let act = Act::of(&app, viewer.as_deref());
    out.push_str(&section(
        "Pull requests",
        &prs,
        |p| p.is_open(),
        |p| pr_row(p, act),
    ));
    out.push_str(&section("Issues", &issues, |i| i.is_open(), issue_row));
    page.body(out).render(&app, viewer.as_deref())
}

#[derive(Deserialize)]
struct Search {
    q: Option<String>,
    filter: Option<String>,
    after: Option<u64>,
    /// One set of the machines filter.
    set: Option<String>,
}

/// Signed out, the catalog of nixpkgs. Signed in, a filter bar on top:
/// what is yours, ranked, or all of nixpkgs.
async fn browse(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(search): Query<Search>,
) -> Response {
    use crate::mine::{Filter, Interests};
    let viewer = app.viewer(&headers);
    let q = search.q.unwrap_or_default().trim().to_string();
    let Some(login) = viewer else {
        return match catalog_body(&app, &q, false) {
            Ok(body) => Page::new(Section::Packages, "Packages, modules and directories")
                .icon(octicon!("package"))
                .sub(CATALOG_SUB)
                .body(body)
                .render(&app, None),
            Err(r) => r.into_response(),
        };
    };
    let filter = Filter::parse(search.filter.as_deref(), !q.is_empty());
    let interests = Interests::of(&app, &login);
    let all = {
        let index = app.index.read().unwrap();
        index.by_pname.len() + index.modules.len()
    };
    let (tabs, step) = crate::mine::bar(&interests, filter, all);
    let page = Page::new(
        Section::Packages,
        format!(
            "Packages: {}",
            tabs.iter()
                .find(|t| t.current)
                .map_or("", |t| t.label.as_str())
        ),
    )
    .heading("Packages")
    .icon(octicon!("package"))
    .subnav(tabs);
    if filter == Filter::All {
        return match catalog_body(&app, &q, true) {
            Ok(body) => page
                .sub(CATALOG_SUB)
                .body(format!("{step}{body}"))
                .render(&app, Some(&login)),
            Err(r) => r.into_response(),
        };
    }
    let v = crate::mine::view(
        &app,
        &login,
        &interests,
        filter,
        &q,
        search.after,
        search.set.as_deref(),
    );
    page.sub(v.sub)
        .actions(v.actions)
        .body(format!("{step}{}", v.body))
        .render(&app, Some(&login))
}

const CATALOG_SUB: &str = "Each one has a page with its health, issues and pull requests. Watch the ones that are important to you.";

/// The catalog: a search box, then the directories or the hits. A name
/// that resolves, or `@login`, answers with a redirect.
fn catalog_body(app: &crate::web::App, q: &str, signed_in: bool) -> Result<String, Redirect> {
    let s = app.snap();
    if let Some(login) = q.strip_prefix('@').filter(|l| valid_login(l)) {
        return Err(Redirect::to(&format!("/u/{login}")));
    }
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    if let Some(href) = exact(&s, &index, q) {
        return Err(Redirect::to(&href));
    }
    let mut out = format!(
        "<form method=\"get\" action=\"/packages\" class=\"inline-form mb-4\">{}\
         <input class=\"form-control flex-auto\" name=\"q\" value=\"{}\" placeholder=\"hello, nixos/nginx, pkgs/development/python-modules, @login\" aria-label=\"search\"{}>\
         <button class=\"btn\" type=\"submit\">{} Search</button></form>",
        if signed_in {
            "<input type=\"hidden\" name=\"filter\" value=\"all\">"
        } else {
            ""
        },
        esc(q),
        if signed_in { "" } else { " autofocus" },
        icon(octicon!("search"))
    );
    if signed_in {
        out.push_str("<p><a href=\"/review?tab=bumps&amp;filter=mine\">Version bumps</a></p>");
    } else {
        out.push_str("<p><a href=\"/review?tab=bumps\">Version bumps</a></p>");
    }
    if q.is_empty() {
        let mut areas: Vec<String> = TOP_AREAS.iter().map(|a| a.to_string()).collect();
        areas.extend(
            index
                .owners
                .iter()
                .map(|(p, _)| p.clone())
                .filter(|p| !is_file(p)),
        );
        areas.sort();
        areas.dedup();
        let list: String = areas
            .iter()
            .map(|a| {
                let n = s.open_by_dir.get(a).copied().unwrap_or(0);
                let meta = match n {
                    0 => String::new(),
                    1 => "1 open pull request".to_string(),
                    n => format!("{n} open pull requests"),
                };
                item_row(
                    None,
                    &format!("{} {}/", icon(octicon!("file-directory")), esc(a)),
                    &format!("/a/{}", esc(a)),
                    &meta,
                    &[],
                    "",
                )
            })
            .collect();
        out.push_str(&crate::ui::section("Directories", None, None, &list));
        return Ok(out);
    }
    let rows: Vec<(Subject, Flags)> = search(&s, &index, q)
        .into_iter()
        .map(|sub| {
            let f = Flags::of(&s, &index, &h, &sub);
            (sub, f)
        })
        .collect();
    if rows.is_empty() {
        out.push_str("<div class=\"blankslate\"><p>Nothing matches.</p></div>");
    } else {
        let _ = write!(out, "<div class=\"Box\">{}</div>", subject_rows(&rows));
    }
    Ok(out)
}

/// What a name resolves to first, then packages, modules and busy
/// directories whose name contains it.
fn search(s: &Snapshot, index: &Index, q: &str) -> Vec<Subject> {
    let ql = q.to_lowercase();
    let mut hits: Vec<Subject> = Vec::new();
    if let Some(sub) = Subject::resolve(index, q) {
        hits.push(sub);
    }
    let mut pkgs: Vec<&String> = index
        .by_pname
        .iter()
        .filter(|(pname, _)| pname.to_lowercase().contains(&ql))
        .map(|(_, attr)| attr)
        .collect();
    pkgs.extend(
        index
            .pkgs
            .keys()
            .filter(|a| a.contains('.') && a.to_lowercase().contains(&ql)),
    );
    pkgs.sort_by_key(|a| (a.len(), a.as_str()));
    pkgs.dedup();
    hits.extend(pkgs.into_iter().take(50).map(|a| Subject::Pkg(a.clone())));
    let mut mods: Vec<&String> = index
        .modules
        .keys()
        .filter(|f| f.to_lowercase().contains(&ql))
        .collect();
    mods.sort_by_key(|f| (module_name(f).len(), f.as_str()));
    hits.extend(
        mods.into_iter()
            .take(30)
            .map(|f| Subject::Module(f.clone())),
    );
    let mut dirs: Vec<&String> = s
        .open_by_dir
        .keys()
        .filter(|d| d.to_lowercase().contains(&ql))
        .collect();
    dirs.sort_by_key(|d| (d.len(), d.as_str()));
    hits.extend(dirs.into_iter().take(20).map(|d| Subject::Area(d.clone())));
    let mut seen = HashSet::new();
    hits.retain(|h| seen.insert(h.clone()));
    hits
}

pub(crate) struct Maintained {
    pub(crate) packages: Vec<(Subject, Option<String>)>,
    pub(crate) modules: Vec<(Subject, Option<String>)>,
}

/// What a login maintains, directly or through a team (named in the second
/// field). One row per package name: the other attributes of the same
/// package are aliases or other package sets.
pub(crate) fn maintained(index: &Index, login: &str) -> Maintained {
    let l = login.to_lowercase();
    let teams: Vec<&String> = index
        .teams
        .iter()
        .filter(|(_, m)| m.contains(&l))
        .map(|(t, _)| t)
        .collect();
    let via = |maintainers: &[String], ts: &[String]| -> Option<Option<String>> {
        if maintainers.iter().any(|m| m.to_lowercase() == l) {
            return Some(None);
        }
        ts.iter()
            .find(|t| teams.contains(t))
            .map(|t| Some(t.clone()))
    };
    let mut packages: BTreeMap<String, (Subject, Option<String>)> = BTreeMap::new();
    for (attr, p) in &index.pkgs {
        if let Some(how) = via(&p.maintainers, &p.teams) {
            let canonical = index.by_pname.get(&p.pname).unwrap_or(attr);
            let key = if index.pkgs.get(canonical).is_some_and(|c| c.file == p.file) {
                canonical.clone()
            } else {
                attr.clone()
            };
            packages
                .entry(key.clone())
                .or_insert((Subject::Pkg(key), how));
        }
    }
    let modules = index
        .modules
        .iter()
        .filter_map(|(f, m)| {
            via(&m.maintainers, &m.teams).map(|how| (Subject::Module(f.clone()), how))
        })
        .collect();
    Maintained {
        packages: packages.into_values().collect(),
        modules,
    }
}

/// A small flag when a channel lags master, with both versions on hover.
pub(crate) fn lag_label(ch: &Channels, attr: &str, master: &str) -> String {
    let behind = ch.behind(attr, master);
    if behind.is_empty() {
        return String::new();
    }
    format!(
        " <span class=\"Label\" title=\"{}\">behind in {}</span>",
        esc(&hover(&behind, attr, master)),
        esc(&channel_names(&behind))
    )
}

fn maintained_rows(
    s: &Snapshot,
    index: &Index,
    h: &Health,
    ch: &Channels,
    list: &[(Subject, Option<String>)],
) -> String {
    let mut out = String::new();
    let mut rows: Vec<(&Subject, &Option<String>, Flags)> = list
        .iter()
        .map(|(sub, how)| (sub, how, Flags::of(s, index, h, sub)))
        .collect();
    rows.sort_by_key(|(sub, _, f)| (f.rank(), sub.label()));
    for (sub, how, f) in rows {
        let version = match sub {
            Subject::Pkg(a) => index
                .pkgs
                .get(a)
                .map(|p| {
                    format!(
                        " <span class=\"color-fg-muted f6\">{}</span>{}",
                        esc(&p.version),
                        lag_label(ch, a, &p.version)
                    )
                })
                .unwrap_or_default(),
            _ => String::new(),
        };
        let via = how
            .as_ref()
            .map(|t| format!(" <span class=\"Label\">via {}</span>", esc(t)))
            .unwrap_or_default();
        out.push_str(&subject_row(sub, &f, &format!("{version}{via}"), ""));
    }
    out
}

/// The Maintains tab of `/u/<login>`.
pub(crate) fn maintains_body(app: &crate::web::App, login: &str) -> String {
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let ch = app.channels.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let m = maintained(&index, login);
    let mut out = format!(
        "<p class=\"f6 color-fg-muted\">Packages and NixOS modules whose <code>meta.maintainers</code> or <code>meta.teams</code> name {0}, \
         at nixpkgs {1}. Change the list in nixpkgs, not here.</p>",
        esc(login),
        esc(if index.rev.is_empty() {
            "(index pending)"
        } else {
            &index.rev[..index.rev.len().min(12)]
        })
    );
    out.push_str(&crate::ui::section(
        &format!("{} Packages", icon(octicon!("package"))),
        Some(m.packages.len()),
        None,
        &maintained_rows(&s, &index, &h, &ch, &m.packages),
    ));
    out.push_str(&crate::ui::section(
        &format!("{} NixOS modules", icon(octicon!("gear"))),
        Some(m.modules.len()),
        None,
        &maintained_rows(&s, &index, &h, &ch, &m.modules),
    ));
    out
}

#[derive(Deserialize)]
struct WatchForm {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

async fn add_watch(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<WatchForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let subject = match (&f.key, &f.name) {
        (Some(k), _) => Subject::parse(k),
        (None, Some(n)) => Subject::resolve(&app.index.read().unwrap(), n),
        _ => None,
    };
    let Some(subject) = subject else {
        return (
            StatusCode::BAD_REQUEST,
            "not a package, a module or a nixpkgs path",
        )
            .into_response();
    };
    if let Err(e) = app.store.watch(&login, &subject.key()) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    crate::notify::recount_one(&app, &login);
    let back = if f.name.is_some() {
        "/packages?filter=watch".to_string()
    } else {
        subject.href()
    };
    Redirect::to(&back).into_response()
}

async fn remove_watch(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<WatchForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let Some(key) = f.key else {
        return (StatusCode::BAD_REQUEST, "no key").into_response();
    };
    if let Err(e) = app.store.unwatch(&login, &key) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    crate::notify::recount_one(&app, &login);
    let back = match Subject::parse(&key) {
        Some(s)
            if headers
                .get(header::REFERER)
                .and_then(|r| r.to_str().ok())
                .is_some_and(|r| !r.contains("/packages")) =>
        {
            s.href()
        }
        _ => "/packages?filter=watch".into(),
    };
    Redirect::to(&back).into_response()
}

async fn watch_maintained(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let m = maintained(&app.index.read().unwrap(), &login);
    for (sub, _) in m.packages.iter().chain(&m.modules) {
        if let Err(e) = app.store.watch(&login, &sub.key()) {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }
    crate::notify::recount_one(&app, &login);
    Redirect::to("/packages?filter=maintain").into_response()
}

async fn new_feed(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    if let Err(e) = app.store.set_feed_token(&login, &random_id()) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    Redirect::to("/me/settings#feed").into_response()
}

pub(crate) fn subscriptions(app: &crate::web::App, login: &str) -> Vec<Subject> {
    app.store
        .watched(login)
        .unwrap_or_default()
        .iter()
        .filter_map(|k| Subject::parse(k))
        .collect()
}

/// The `#feed` section of `/me/settings`.
pub(crate) fn feed_box(app: &crate::web::App, login: &str) -> String {
    let feed = match app.store.feed_token(login).ok().flatten() {
        Some(t) => format!(
            "<p>Add this link to a feed reader. It is private: anyone with it reads what you watch, and the advisories and releases of your package set.</p>\
             <input class=\"form-control width-full mb-2\" readonly value=\"{}/feed/{}\" aria-label=\"feed link\">\
             <form method=\"post\" action=\"/me/feed\"><button class=\"btn btn-sm\" type=\"submit\">Replace the link</button></form>",
            esc(app.public_url.trim_end_matches('/')),
            esc(&t)
        ),
        None => "<form method=\"post\" action=\"/me/feed\"><button class=\"btn btn-sm\" type=\"submit\">Make a feed link</button></form>"
            .into(),
    };
    format!(
        "<div id=\"feed\">{}</div>",
        crate::ui::section(
            &format!("{} Atom feed", icon(octicon!("rss"))),
            None,
            None,
            &format!("<div class=\"Box-row\">{feed}</div>")
        )
    )
}

pub(crate) struct Event {
    pub(crate) at: DateTime<Utc>,
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) subject: String,
}

/// What a login's feed and inbox show: the events of their subscriptions,
/// then those of their package set and of their own pull requests, newest
/// first.
pub(crate) fn person_events(app: &crate::web::App, login: &str) -> Vec<Event> {
    let subs = subscriptions(app, login);
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let set = crate::sets::set_attrs(app, login);
    let base = app.public_url.trim_end_matches('/');
    let index = app.index.read().unwrap();
    let since = Utc::now() - Duration::days(FEED_DAYS);
    let mut list = events(&s, &index, &h, &subs, since);
    let mut seen: HashSet<String> = list.iter().map(|e| e.id.clone()).collect();
    for mut e in set_events(&s, &index, &h, &set, since) {
        if seen.insert(e.id.clone()) {
            if e.url.is_empty() {
                e.url = format!("{base}/p/{}", e.subject);
            }
            list.push(e);
        }
    }
    let plans = crate::followup::Plans::of(app, login, s.committers.is(login));
    list.extend(crate::followup::events(
        &s.pr_data,
        login,
        &plans,
        Utc::now(),
        since,
    ));
    list.sort_by_key(|e| std::cmp::Reverse(e.at));
    list.truncate(200);
    list
}

fn events(
    s: &Snapshot,
    index: &Index,
    h: &Health,
    subs: &[Subject],
    since: DateTime<Utc>,
) -> Vec<Event> {
    let mut out: Vec<Event> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for sub in subs {
        let label = sub.label();
        let (prs, issues) = attached(s, index, sub);
        for p in prs {
            let mut push = |at: DateTime<Utc>, verb: &str| {
                if at >= since && seen.insert(format!("pr-{}-{verb}", p.number)) {
                    out.push(Event {
                        at,
                        id: format!("pr-{}-{verb}", p.number),
                        title: format!("{verb}: #{} {}", p.number, p.title),
                        url: p.url.clone(),
                        subject: label.clone(),
                    });
                }
            };
            push(p.created_at, "pull request opened");
            if let Some(t) = p.merged_at {
                push(t, "merged");
            } else if let Some(t) = p.closed_at.filter(|_| !p.is_open()) {
                push(t, "pull request closed");
            }
        }
        for i in issues {
            let mut push = |at: DateTime<Utc>, verb: &str| {
                if at >= since && seen.insert(format!("issue-{}-{verb}", i.number)) {
                    out.push(Event {
                        at,
                        id: format!("issue-{}-{verb}", i.number),
                        title: format!("{verb}: #{} {}", i.number, i.title),
                        url: i.url.clone(),
                        subject: label.clone(),
                    });
                }
            };
            push(i.created_at, "issue opened");
            if let Some(t) = i.closed_at.filter(|_| !i.is_open()) {
                push(t, "issue closed");
            }
        }
        if let Subject::Pkg(attr) = sub {
            for (a, _) in judged(h, s, attr).into_iter().filter(|(_, v)| !v.fixed()) {
                let at = a
                    .issue
                    .and_then(|n| s.issue_at.get(&n))
                    .map(|&i| s.issues[i].created_at);
                if let Some(at) = at.filter(|t| *t >= since) {
                    if seen.insert(format!("advisory-{}-{attr}", a.code)) {
                        out.push(Event {
                            at,
                            id: format!("advisory-{}-{attr}", a.code),
                            title: format!("advisory {}: {}", a.code, a.title),
                            url: a.url(),
                            subject: label.clone(),
                        });
                    }
                }
            }
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.at));
    out.truncate(200);
    out
}

/// Events for the packages of a private set: advisories and upstream
/// releases only. A closure holds thousands of packages, and every pull
/// request against them would drown the feed.
fn set_events(
    s: &Snapshot,
    index: &Index,
    h: &Health,
    attrs: &[String],
    since: DateTime<Utc>,
) -> Vec<Event> {
    let mut out = Vec::new();
    for attr in attrs {
        for (a, _) in judged(h, s, attr).into_iter().filter(|(_, v)| !v.fixed()) {
            let at = a
                .issue
                .and_then(|n| s.issue_at.get(&n))
                .map(|&i| s.issues[i].created_at);
            if let Some(at) = at.filter(|t| *t >= since) {
                out.push(Event {
                    at,
                    id: format!("advisory-{}-{attr}", a.code),
                    title: format!("advisory {}: {}", a.code, a.title),
                    url: a.url(),
                    subject: attr.clone(),
                });
            }
        }
        if let Some(v) = upstream(s, index, h, attr) {
            if let Some(at) = h.upstream_since(attr, v).filter(|t| *t >= since) {
                out.push(Event {
                    at,
                    id: format!("upstream-{attr}-{v}"),
                    title: format!("upstream released {v}"),
                    url: String::new(),
                    subject: attr.clone(),
                });
            }
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.at));
    out.truncate(SET_EVENTS);
    out
}

/// XML 1.0 has no escape for most control characters; one in a title would
/// make the whole feed unreadable.
fn xml(s: &str) -> String {
    let s: String = s
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
        .collect();
    esc(&s).replace('\'', "&apos;")
}

async fn feed(State(app): State<Shared>, Path(token): Path<String>) -> Response {
    let token = token.trim_end_matches(".atom");
    let Some(login) = app.store.feed_login(token).ok().flatten() else {
        return (StatusCode::NOT_FOUND, "no such feed").into_response();
    };
    let base = app.public_url.trim_end_matches('/');
    let list = person_events(&app, &login);
    let updated = list.first().map_or_else(Utc::now, |e| e.at);
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<feed xmlns=\"http://www.w3.org/2005/Atom\">\
         <title>Maintainer Mode: what {l} watches</title><id>{b}/feed/{l}</id><updated>{}</updated>\
         <link rel=\"alternate\" href=\"{b}/inbox\"/>",
        updated.to_rfc3339(),
        l = xml(&login),
        b = xml(base)
    );
    for e in list {
        let _ = write!(
            out,
            "<entry><id>{b}/e/{}</id><title>{}: {}</title><updated>{}</updated><link href=\"{}\"/><author><name>{}</name></author></entry>",
            xml(&e.id),
            xml(&e.subject),
            xml(&e.title),
            e.at.to_rfc3339(),
            xml(&e.url),
            xml(&e.subject),
            b = xml(base)
        );
    }
    out.push_str("</feed>");
    (
        [
            (header::CONTENT_TYPE, "application/atom+xml; charset=utf-8"),
            (header::CACHE_CONTROL, "private, max-age=300"),
        ],
        out,
    )
        .into_response()
}

fn pr_json(p: &Pr) -> Value {
    json!({
        "number": p.number, "title": p.title, "url": p.url, "author": p.author,
        "state": p.state, "draft": p.is_draft, "ci": p.ci,
        "created_at": p.created_at, "merged_at": p.merged_at, "closed_at": p.closed_at,
    })
}

fn issue_json(i: &Issue) -> Value {
    json!({
        "number": i.number, "title": i.title, "url": i.url, "author": i.author,
        "state": i.state, "labels": i.labels, "comments": i.comments,
        "created_at": i.created_at, "closed_at": i.closed_at,
    })
}

fn subject_json(app: &crate::web::App, subject: &Subject, extra: Value) -> Value {
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let (prs, issues) = attached(&s, &index, subject);
    let mut v = json!({
        "key": subject.key(),
        "name": subject.label(),
        "flags": Flags::of(&s, &index, &h, subject).json(),
        "prs": prs.iter().map(|p| pr_json(p)).collect::<Vec<_>>(),
        "issues": issues.iter().map(|i| issue_json(i)).collect::<Vec<_>>(),
    });
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    v
}

pub(crate) async fn package_value(app: &crate::web::App, name: &str) -> Option<Value> {
    let (attr, p) = {
        let index = app.index.read().unwrap();
        let a = index.resolve(name)?;
        (a.to_string(), index.pkgs.get(a).cloned())
    };
    let job = Hydra::package_job(&attr);
    let build = app.hydra.last(&job).await;
    let master = p.as_ref().map(|p| p.version.clone()).unwrap_or_default();
    let (advisories, osv, channels, backport) = {
        let s = app.snap();
        let h = app.health.read().unwrap().clone();
        let ch = app.channels.read().unwrap().clone();
        let open = judged(&h, &s, &attr);
        let closed: Vec<&String> = closed_advisories(&h, &s, &attr)
            .into_iter()
            .map(|a| &a.code)
            .collect();
        let behind = ch.backport(&attr, &master);
        let backport: Vec<Value> = ch
            .list
            .iter()
            .filter(|c| c.stable() && !c.rev.is_empty())
            .filter_map(|c| {
                let mut codes: Vec<&String> = if behind.iter().any(|b| b.key == c.key) {
                    closed.clone()
                } else {
                    Vec::new()
                };
                codes.extend(open.iter().filter_map(|(a, v)| match v {
                    Verdict::Behind { channels, .. }
                        if channels.iter().any(|l| l.channel == c.label()) =>
                    {
                        Some(&a.code)
                    }
                    _ => None,
                }));
                (!codes.is_empty()).then(|| json!({ "channel": c.label(), "version": c.versions.get(&attr), "advisories": codes }))
            })
            .collect();
        let channels: Vec<Value> = ch
            .of(&attr, &master)
            .iter()
            .map(|seen| {
                json!({
                    "channel": seen.channel.label(), "rev": seen.channel.rev,
                    "read_at": seen.channel.read_at, "version": seen.version,
                    "behind_master": seen.behind,
                })
            })
            .collect();
        let list: Vec<&Advisory> = open.iter().map(|(a, _)| *a).collect();
        let tracker: Vec<Value> = rated(&h, &list)
            .into_iter()
            .map(|(a, w)| {
                let mut v = json!(a);
                v["severity"] = json!(w);
                v["verdict"] = json!(h.verdict(&attr, &a.code));
                v
            })
            .collect();
        let osv: Vec<Value> = h
            .osv_extra(&attr)
            .into_iter()
            .map(|(id, v)| {
                json!({
                    "id": id, "url": crate::osv::Vuln::url(id),
                    "summary": v.and_then(|v| v.summary.clone()),
                    "cves": v.map(|v| v.cves.clone()).unwrap_or_default(),
                    "severity": osv_worst(&h, v),
                })
            })
            .collect();
        (tracker, osv, channels, backport)
    };
    let hygiene = p.as_ref().and_then(|p| hygiene::gaps(&attr, p));
    let bumps: Vec<u64> = {
        let s = app.snap();
        let index = app.index.read().unwrap();
        let (prs, _) = attached(&s, &index, &Subject::Pkg(attr.clone()));
        let pname = p.as_ref().map_or("", |p| p.pname.as_str());
        crate::bumps::of_package(&attr, pname, &prs)
            .iter()
            .map(|p| p.number)
            .collect()
    };
    let extra = json!({
        "attr": attr, "package": p, "hydra": build.build(), "hydra_state": build.state(), "hydra_job": job,
        "advisories": advisories, "osv": osv, "packaging_gaps": hygiene,
        "channels": channels, "needs_backport": backport, "version_bumps": bumps,
    });
    Some(subject_json(app, &Subject::Pkg(attr.clone()), extra))
}

async fn api_package(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    match package_value(&app, &name).await {
        Some(v) => Json(v).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "not in the index"})),
        )
            .into_response(),
    }
}

pub(crate) async fn module_value(app: &crate::web::App, file: &str) -> Option<Value> {
    let m = app.index.read().unwrap().modules.get(file).cloned()?;
    let job = Hydra::test_job(module_name(file).trim_start_matches("nixos/"));
    let build = app.hydra.last(&job).await;
    let extra = json!({ "file": file, "module": m, "test": build.build(), "test_state": build.state(), "test_job": job });
    Some(subject_json(app, &Subject::Module(file.to_string()), extra))
}

async fn api_module(State(app): State<Shared>, Path(file): Path<String>) -> Response {
    match module_value(&app, &file).await {
        Some(v) => Json(v).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "not a NixOS module in the index"})),
        )
            .into_response(),
    }
}

pub(crate) fn area_value(app: &crate::web::App, path: &str) -> Option<Value> {
    let path = path.trim_matches('/');
    if !valid_path(path) {
        return None;
    }
    let contents: Vec<Value> = {
        let s = app.snap();
        let h = app.health.read().unwrap().clone();
        let index = app.index.read().unwrap();
        contents(&index, &s, &h, path)
            .into_iter()
            .map(|(sub, f)| json!({ "key": sub.key(), "name": sub.label(), "flags": f.json() }))
            .collect()
    };
    Some(subject_json(
        app,
        &Subject::Area(path.to_string()),
        json!({ "contents": contents }),
    ))
}

async fn api_area(State(app): State<Shared>, Path(path): Path<String>) -> Response {
    match area_value(&app, &path) {
        Some(v) => Json(v).into_response(),
        None => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "not a nixpkgs path"})),
        )
            .into_response(),
    }
}

pub(crate) fn maintains_value(app: &crate::web::App, login: &str) -> Value {
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let ch = app.channels.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let m = maintained(&index, login);
    let rows = |list: &[(Subject, Option<String>)]| -> Vec<Value> {
        let mut rows: Vec<(&Subject, &Option<String>, Flags)> = list
            .iter()
            .map(|(sub, how)| (sub, how, Flags::of(&s, &index, &h, sub)))
            .collect();
        rows.sort_by_key(|(sub, _, f)| (f.rank(), sub.label()));
        rows.into_iter()
            .map(|(sub, how, f)| {
                let version = match sub {
                    Subject::Pkg(a) => index.pkgs.get(a).map(|p| p.version.clone()),
                    _ => None,
                };
                let behind: Vec<&str> = match (sub, &version) {
                    (Subject::Pkg(a), Some(v)) => {
                        ch.behind(a, v).iter().map(|c| c.label()).collect()
                    }
                    _ => vec![],
                };
                json!({
                    "key": sub.key(), "name": sub.label(), "version": version,
                    "via_team": how, "flags": f.json(),
                    "channels_behind": behind,
                })
            })
            .collect()
    };
    json!({
        "login": login,
        "rev": index.rev,
        "packages": rows(&m.packages),
        "modules": rows(&m.modules),
    })
}

fn subject_list(app: &crate::web::App, subjects: &[Subject]) -> Vec<Value> {
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    subjects
        .iter()
        .map(|sub| {
            json!({ "key": sub.key(), "name": sub.label(), "flags": Flags::of(&s, &index, &h, sub).json() })
        })
        .collect()
}

/// The hits of `/packages?q=`, as JSON.
pub(crate) fn search_value(app: &crate::web::App, q: &str) -> Value {
    let hits = {
        let s = app.snap();
        let index = app.index.read().unwrap();
        search(&s, &index, q.trim())
    };
    json!({ "query": q, "results": subject_list(app, &hits) })
}

pub(crate) fn watching_value(app: &crate::web::App, login: &str) -> Value {
    json!({ "login": login, "subscriptions": subject_list(app, &subscriptions(app, login)) })
}

/// Sends a name to the API of what it resolves to, so a client needs to know
/// only the name.
async fn api_find(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    match Subject::resolve(&app.index.read().unwrap(), &name) {
        Some(sub) => Redirect::temporary(&format!("/api/v1{}", api_path(&sub))).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "not a package, a module or a nixpkgs path"})),
        )
            .into_response(),
    }
}

fn api_path(sub: &Subject) -> String {
    match sub {
        Subject::Pkg(a) => format!("/packages/{a}"),
        Subject::Module(f) => format!("/modules/{f}"),
        Subject::Area(p) => format!("/areas/{p}"),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::area::{Module, Package};

    #[test]
    fn owners_files_are_not_directories() {
        assert!(is_file("doc/redirects.json"));
        assert!(is_file("default.nix"));
        assert!(!is_file("pkgs/development/python-modules"));
        assert!(!is_file(".github"));
        assert!(valid_login("zimbatm"));
        assert!(!valid_login("a/b"));
    }

    fn index() -> Index {
        let mut i = Index::default();
        for (attr, pname, m, t) in [
            ("hello", "hello", vec!["Alice"], vec![]),
            ("python3Packages.foo", "foo", vec![], vec!["py"]),
        ] {
            i.pkgs.insert(
                attr.into(),
                Package {
                    pname: pname.into(),
                    version: "1".into(),
                    file: Some(format!("pkgs/{pname}/default.nix")),
                    maintainers: m.into_iter().map(String::from).collect(),
                    teams: t.into_iter().map(String::from).collect(),
                    facts: None,
                    meta: None,
                },
            );
            i.by_pname.insert(pname.into(), attr.into());
        }
        i.modules.insert(
            "nixos/modules/services/misc/gitea.nix".into(),
            Module {
                maintainers: vec!["alice".into()],
                teams: vec![],
            },
        );
        i.teams.insert("py".into(), vec!["alice".into()]);
        i
    }

    #[test]
    fn input_resolves_to_a_package_a_module_or_an_area() {
        let i = index();
        assert_eq!(
            Subject::resolve(&i, "hello"),
            Some(Subject::Pkg("hello".into()))
        );
        assert_eq!(
            Subject::resolve(&i, "foo"),
            Some(Subject::Pkg("python3Packages.foo".into()))
        );
        assert_eq!(
            Subject::resolve(&i, "nixos/gitea"),
            Some(Subject::Module(
                "nixos/modules/services/misc/gitea.nix".into()
            ))
        );
        assert_eq!(
            Subject::resolve(&i, "pkgs/development/"),
            Some(Subject::Area("pkgs/development".into()))
        );
        assert_eq!(Subject::resolve(&i, "../etc"), None);
        assert_eq!(
            Subject::parse("pkg:hello"),
            Some(Subject::Pkg("hello".into()))
        );
        assert_eq!(Subject::parse("evil:x"), None);
    }

    #[test]
    fn a_login_maintains_directly_and_through_a_team() {
        let m = maintained(&index(), "ALICE");
        assert_eq!(
            m.packages,
            vec![
                (Subject::Pkg("hello".into()), None),
                (
                    Subject::Pkg("python3Packages.foo".into()),
                    Some("py".into())
                )
            ]
        );
        assert_eq!(m.modules.len(), 1);
    }

    fn issue(number: u64, state: &str) -> Issue {
        Issue {
            number,
            title: format!("issue {number}"),
            url: format!("https://github.com/NixOS/nixpkgs/issues/{number}"),
            author: "alice".into(),
            state: state.into(),
            created_at: crate::ledger::tests::t(0),
            updated_at: crate::ledger::tests::t(0),
            closed_at: None,
            labels: vec![],
            comments: 0,
            mentions: vec![],
        }
    }
    fn dated_issue(number: u64, created_at: DateTime<Utc>) -> Issue {
        Issue {
            number,
            title: String::new(),
            url: String::new(),
            author: String::new(),
            state: "OPEN".into(),
            created_at,
            updated_at: created_at,
            closed_at: None,
            labels: vec![],
            comments: 0,
            mentions: vec![],
        }
    }

    #[test]
    fn an_advisory_whose_issue_the_sync_never_saw_is_closed_once_the_sync_is_complete() {
        let mut h = Health::default();
        h.advisories.insert(
            "hello".into(),
            [(None, "A"), (Some(1), "B"), (Some(2), "C"), (Some(3), "D")]
                .into_iter()
                .map(|(issue, code)| Advisory {
                    code: code.into(),
                    cves: vec![],
                    title: code.into(),
                    issue,
                    ranges: vec![],
                })
                .collect(),
        );
        let mut s = Snapshot {
            issues: vec![issue(1, "OPEN"), issue(2, "CLOSED")],
            ..Default::default()
        };
        s.issue_at = [(1, 0), (2, 1)].into();
        let codes = |s: &Snapshot| -> Vec<String> {
            open_advisories(&h, s, "hello")
                .iter()
                .map(|a| a.code.clone())
                .collect()
        };
        assert_eq!(codes(&s), ["A", "B", "D"]);
        s.issues_complete = true;
        assert_eq!(codes(&s), ["A", "B"]);
    }

    #[test]
    fn an_area_lists_a_package_that_shares_its_name_with_one_elsewhere() {
        let mut i = Index::default();
        for (attr, file) in [
            (
                "protobuf",
                "pkgs/development/libraries/protobuf/default.nix",
            ),
            (
                "protobuf_3",
                "pkgs/development/libraries/protobuf/default.nix",
            ),
            (
                "python3Packages.protobuf",
                "pkgs/development/python-modules/protobuf/default.nix",
            ),
        ] {
            i.pkgs.insert(
                attr.into(),
                Package {
                    pname: "protobuf".into(),
                    file: Some(file.into()),
                    ..Default::default()
                },
            );
        }
        i.by_pname.insert("protobuf".into(), "protobuf".into());
        let s = Snapshot::default();
        let h = Health::default();
        let names = |path: &str| -> Vec<String> {
            contents(&i, &s, &h, path)
                .iter()
                .map(|(sub, _)| sub.label())
                .collect()
        };
        assert_eq!(
            names("pkgs/development/python-modules"),
            ["python3Packages.protobuf"]
        );
        assert_eq!(names("pkgs/development/libraries"), ["protobuf"]);
    }

    #[test]
    fn an_area_lists_the_riskiest_package_first() {
        let mut i = Index::default();
        for (attr, maintainers) in [("aaa", vec!["alice".to_string()]), ("zzz", vec![])] {
            i.pkgs.insert(
                attr.into(),
                Package {
                    pname: attr.into(),
                    version: "1.0".into(),
                    file: Some(format!("pkgs/by-name/{attr}/package.nix")),
                    maintainers,
                    ..Default::default()
                },
            );
            i.by_pname.insert(attr.into(), attr.into());
        }
        let s = Snapshot::default();
        let mut h = Health::default();
        h.upstream.insert("aaa".into(), "1.0-rc1".into());
        let rows = contents(&i, &s, &h, "pkgs/by-name");
        let order: Vec<(String, u32)> = rows
            .iter()
            .map(|(sub, f)| (sub.label(), f.risk()))
            .collect();
        // no maintainer outranks a prerelease repology reports, which counts for nothing
        assert_eq!(order, [("zzz".to_string(), 6), ("aaa".to_string(), 0)]);
        assert!(rows[0].1.html().contains("risk 6"));
    }

    #[test]
    fn a_fix_in_master_shows_until_the_stable_channel_has_it() {
        let a = |code: &str, issue| Advisory {
            code: code.into(),
            cves: vec![],
            title: code.into(),
            issue: Some(issue),
            ranges: vec![],
        };
        let mut h = Health::default();
        h.advisories
            .insert("cups".into(), vec![a("OPEN-1", 1), a("FIXED-2", 2)]);
        let mut s = Snapshot {
            issues: vec![issue(1, "OPEN"), issue(2, "CLOSED")],
            ..Default::default()
        };
        s.issue_at = [(1, 0), (2, 1)].into();
        let stable = Channel {
            key: "stable".into(),
            name: "nixos-26.05".into(),
            rev: "r".into(),
            read_at: None,
            versions: [("cups".to_string(), "2.4.19".to_string())].into(),
        };
        let ch = Channels { list: vec![stable] };
        let html = |master: &str| {
            advisories_html(
                &h,
                &judged(&h, &s, "cups"),
                &[],
                &closed_advisories(&h, &s, "cups"),
                &ch.backport("cups", master),
                "cups",
                master,
            )
        };
        let ahead = html("2.4.20");
        assert_eq!(ahead.matches("needs backport to nixos-26.05").count(), 1);
        let open = ahead.split("OPEN-1").nth(1).unwrap();
        assert!(
            !open
                .split("</div></div>")
                .next()
                .unwrap()
                .contains("backport")
        );
        assert!(ahead.contains("FIXED-2"));
        let level = html("2.4.19");
        assert!(!level.contains("needs backport"));
        assert!(!level.contains("FIXED-2"));
        assert!(channels_html(&ch, "cups", "2.4.20").contains("master is ahead of nixos-26.05"));
        assert!(lag_label(&ch, "cups", "2.4.19").is_empty());
    }

    /// unbound 1.26.1 in master and in both channels, with the tracker issue
    /// of "< 1.26.1" still open.
    pub(crate) fn unbound(stable: &str) -> (Index, Health, Snapshot, Channels) {
        let mut i = Index {
            rev: "r".into(),
            ..Default::default()
        };
        i.pkgs.insert(
            "unbound".into(),
            Package {
                pname: "unbound".into(),
                version: "1.26.1".into(),
                maintainers: vec!["alice".into()],
                ..Default::default()
            },
        );
        i.by_pname.insert("unbound".into(), "unbound".into());
        let mut h = Health::default();
        h.cves.insert(
            "CVE-2026-26001".into(),
            crate::severity::Cve {
                band: Some(severity::Band::Critical),
                cvss: Some(9.8),
                ..Default::default()
            },
        );
        h.advisories.insert(
            "unbound".into(),
            vec![Advisory {
                code: "NIXPKGS-2026-2615".into(),
                cves: vec!["CVE-2026-26001".into()],
                title: "unbound: security issues < 1.26.1".into(),
                issue: Some(564154),
                ranges: vec![crate::affected::Ranges {
                    cve: Some("CVE-2026-26001".into()),
                    constraints: vec![(
                        "affected".into(),
                        "==13fcb641ed33d1472e4ffdec2846180b15405053".into(),
                    )],
                }],
            }],
        );
        let mut s = Snapshot {
            issues: vec![issue(564154, "OPEN")],
            ..Default::default()
        };
        s.issue_at = [(564154, 0)].into();
        let channel = |key: &str, name: &str, v: &str| Channel {
            key: key.into(),
            name: name.into(),
            rev: format!("rev-{name}"),
            read_at: None,
            versions: [("unbound".to_string(), v.to_string())].into(),
        };
        let ch = Channels {
            list: vec![
                channel("nixos-unstable", "nixos-unstable", "1.26.1"),
                channel("stable", "nixos-26.05", stable),
            ],
        };
        h.assess(&i, &ch);
        (i, h, s, ch)
    }

    #[test]
    fn an_advisory_fixed_everywhere_scores_nothing_and_asks_to_close_the_issue() {
        let (i, h, s, ch) = unbound("1.26.1");
        assert_eq!(
            h.verdict("unbound", "NIXPKGS-2026-2615"),
            &Verdict::Fixed {
                fixed_in: Some("1.26.1".into())
            }
        );
        let f = Flags::of(&s, &i, &h, &Subject::Pkg("unbound".into()));
        assert_eq!(f.risk(), 0);
        assert_eq!(f.advisories, 0);
        assert!(!crate::mine::attention(&f));
        let html = advisories_html(
            &h,
            &judged(&h, &s, "unbound"),
            &[],
            &closed_advisories(&h, &s, "unbound"),
            &ch.backport("unbound", "1.26.1"),
            "unbound",
            "1.26.1",
        );
        assert!(html.contains(
            "fixed in 1.26.1</span> · <a href=\"https://github.com/NixOS/nixpkgs/issues/564154\">the tracker issue #564154 can be closed</a>"
        ));
        assert!(!html.contains("color-fg-danger\" href=\"https://tracker"));
        let p = package_stats(&s, &i, &h);
        assert_eq!(p.with_advisories, 0);
        assert_eq!(p.fixed_in_master_only, 0);
        assert_eq!(p.tracker_issues_to_close.len(), 1);
        assert_eq!(p.tracker_issues_to_close[0].issue, 564154);
        assert_eq!(
            p.tracker_issues_to_close[0].fixed_in.as_deref(),
            Some("1.26.1")
        );
    }

    #[test]
    fn an_advisory_fixed_only_in_master_asks_for_a_backport() {
        let (i, h, s, ch) = unbound("1.25.0");
        let f = Flags::of(&s, &i, &h, &Subject::Pkg("unbound".into()));
        assert_eq!(f.risk(), 20);
        assert_eq!(f.advisories, 0);
        let reasons = &f.score.as_ref().unwrap().reasons;
        assert_eq!(
            reasons[0].text,
            "1 advisory fixed in master, still in nixos-26.05"
        );
        let html = advisories_html(
            &h,
            &judged(&h, &s, "unbound"),
            &[],
            &[],
            &ch.backport("unbound", "1.26.1"),
            "unbound",
            "1.26.1",
        );
        assert!(html.contains("needs backport: nixos-26.05 has 1.25.0"));
        let p = package_stats(&s, &i, &h);
        assert_eq!(p.with_advisories, 0);
        assert_eq!(p.fixed_in_master_only, 1);
        assert!(p.tracker_issues_to_close.is_empty());
    }

    #[test]
    fn an_advisory_with_no_readable_range_stays_open() {
        let (i, mut h, s, ch) = unbound("1.26.1");
        let a = &mut h.advisories.get_mut("unbound").unwrap()[0];
        a.title = "unbound: a flaw".into();
        h.assess(&i, &ch);
        let f = Flags::of(&s, &i, &h, &Subject::Pkg("unbound".into()));
        assert_eq!(f.risk(), 40);
        assert_eq!(f.advisories, 1);
        assert_eq!(
            f.score.as_ref().unwrap().reasons[0].text,
            "1 open advisory, range unknown"
        );
        assert_eq!(package_stats(&s, &i, &h).with_advisories, 1);
    }

    #[test]
    fn package_statistics_count_one_package_per_name() {
        let mut i = Index {
            rev: "r".into(),
            ..Default::default()
        };
        for (attr, pname, broken) in [("a", "a", true), ("a_2", "a", false), ("b", "b", false)] {
            i.pkgs.insert(
                attr.into(),
                Package {
                    pname: pname.into(),
                    version: "1.0".into(),
                    maintainers: if attr == "b" {
                        vec!["alice".into()]
                    } else {
                        vec![]
                    },
                    facts: Some(crate::attrs::Facts {
                        broken,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            );
        }
        i.by_pname.insert("a".into(), "a".into());
        i.by_pname.insert("b".into(), "b".into());
        let mut h = Health::default();
        h.upstream.insert("b".into(), "2.0".into());
        h.advisories.insert(
            "a".into(),
            vec![Advisory {
                code: "X".into(),
                cves: vec![],
                title: String::new(),
                issue: None,
                ranges: vec![],
            }],
        );
        let mut bump = crate::ledger::tests::pr(1, "r-ryantm", 0);
        bump.title = "b: 1.0 -> 2.0".into();
        let s = Snapshot {
            pr_data: vec![bump],
            ..Default::default()
        };
        let p = package_stats(&s, &i, &h);
        assert_eq!(p.packages, 2);
        assert_eq!(p.behind_upstream, 1);
        assert_eq!(p.with_advisories, 1);
        assert_eq!(p.advisories_by_band.unscored, 1);
        assert_eq!(p.unmaintained, 1);
        assert_eq!(p.broken, 1);
        assert_eq!(p.with_packaging_gaps, 2);
        assert_eq!(p.open_bumps, 1);
    }

    #[test]
    fn the_package_box_escapes_meta_and_links_only_web_urls() {
        let p = Package {
            pname: "hello".into(),
            meta: Some(Meta {
                long_description: Some("<b>bold</b>\nline two".into()),
                homepage: vec!["javascript:alert(1)".into(), "https://example.org".into()],
                known_vulnerabilities: vec!["CVE-1 <x>".into()],
                position: Some("pkgs/hello/default.nix:7".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let html = package_box(&Index::default(), "abc", &p);
        assert!(html.contains("&lt;b&gt;bold&lt;/b&gt;"));
        assert!(!html.contains("href=\"javascript:"));
        assert!(html.contains("<a href=\"https://example.org\">"));
        assert!(html.contains("CVE-1 &lt;x&gt;"));
        assert!(html.contains("blob/abc/pkgs/hello/default.nix#L7"));
        let old = package_box(&Index::default(), "abc", &Package::default());
        assert!(old.contains("not read yet"));
    }

    #[test]
    fn feed_text_is_valid_xml() {
        assert_eq!(
            xml("a\u{1}b\u{7f} <'&\">\n"),
            "ab &lt;&apos;&amp;&quot;&gt;\n"
        );
    }

    #[test]
    fn a_reopened_issue_or_pull_request_is_not_reported_closed() {
        let mut i = issue(1, "OPEN");
        i.closed_at = Some(crate::ledger::tests::t(1));
        i.mentions = vec!["pkgs/foo/default.nix".into()];
        let mut p = crate::ledger::tests::pr(2, "bob", 0);
        p.closed_at = Some(crate::ledger::tests::t(1));
        p.files = vec!["pkgs/foo/default.nix".into()];
        let mut s = Snapshot {
            issues: vec![i],
            pr_data: vec![p],
            ..Default::default()
        };
        s.issue_at = [(1, 0)].into();
        s.pr_at = [(2, 0)].into();
        let e = events(
            &s,
            &Index::default(),
            &Health::default(),
            &[Subject::Area("pkgs/foo".into())],
            crate::ledger::tests::t(-1),
        );
        let ids: Vec<&str> = e.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["pr-2-pull request opened", "issue-1-issue opened"]);
    }

    #[test]
    fn open_pull_requests_count_for_every_directory_above_a_file() {
        let mut p = crate::ledger::tests::pr(1, "a", 0);
        p.files = vec!["pkgs/a/b.nix".into(), "pkgs/a/c.nix".into()];
        let d = open_by_dir(&[p]);
        assert_eq!(d["pkgs"], 1);
        assert_eq!(d["pkgs/a"], 1);
    }

    #[test]
    fn a_set_gets_only_new_advisories_and_new_releases() {
        let now = Utc::now();
        let since = now - Duration::days(FEED_DAYS);
        let mut i = Index::default();
        for a in ["fresh", "stale"] {
            i.pkgs.insert(
                a.into(),
                Package {
                    pname: a.into(),
                    ..Default::default()
                },
            );
        }
        let mut s = Snapshot::default();
        for (n, at) in [(1, now - Duration::days(1)), (2, now - Duration::days(60))] {
            s.issue_at.insert(n, s.issues.len());
            s.issues.push(dated_issue(n, at));
        }
        let mut h = Health::default();
        for (attr, n) in [("fresh", 1), ("stale", 2)] {
            h.advisories.insert(
                attr.into(),
                vec![Advisory {
                    code: format!("A-{n}"),
                    cves: vec![],
                    title: String::new(),
                    issue: Some(n),
                    ranges: vec![],
                }],
            );
        }
        h.set_upstream(
            [("stale".to_string(), "9".to_string())].into(),
            now - Duration::days(40),
        );
        h.set_upstream(
            [("stale", "9"), ("fresh", "2")]
                .map(|(a, v)| (a.to_string(), v.to_string()))
                .into(),
            now - Duration::days(2),
        );
        h.set_upstream(h.upstream.clone(), now);
        let attrs = vec!["fresh".to_string(), "stale".to_string()];
        let mut ids: Vec<String> = set_events(&s, &i, &h, &attrs, since)
            .into_iter()
            .map(|e| e.id)
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["advisory-A-1-fresh", "upstream-fresh-2"]);
    }

    #[test]
    fn a_large_set_does_not_flood_the_feed() {
        let now = Utc::now();
        let mut i = Index::default();
        let mut h = Health::default();
        h.set_upstream(
            [("seed".to_string(), "1".to_string())].into(),
            now - Duration::days(3),
        );
        let attrs: Vec<String> = (0..80).map(|n| format!("p{n}")).collect();
        for a in &attrs {
            i.pkgs.insert(
                a.clone(),
                Package {
                    pname: a.clone(),
                    ..Default::default()
                },
            );
        }
        h.set_upstream(attrs.iter().map(|a| (a.clone(), "2".into())).collect(), now);
        let s = Snapshot::default();
        let out = set_events(&s, &i, &h, &attrs, now - Duration::days(FEED_DAYS));
        assert_eq!(out.len(), SET_EVENTS);
    }

    #[test]
    fn build_row_tells_each_answer_apart() {
        let job = "nixpkgs/trunk/a<b>.x86_64-linux";
        let link = "<a href=\"https://hydra.nixos.org/job/nixpkgs/trunk/a&lt;b&gt;.x86_64-linux\">nixpkgs/trunk/a&lt;b&gt;.x86_64-linux</a>";
        for (last, text) in [
            (Last::NotAsked, "not asked yet"),
            (Last::Missing, "no finished build of"),
            (Last::NoAnswer, "Hydra did not answer"),
        ] {
            let row = build_row("Hydra", job, &last);
            assert!(row.contains(text), "{row}");
            assert!(row.contains(link), "{row}");
            assert!(!row.contains("a<b>"), "{row}");
        }
    }
}
