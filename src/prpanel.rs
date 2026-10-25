//! The decision panel at the top of `/pr/<n>`: what changes, how far it
//! rebuilds, what upstream says, the builds, the people, the advisories,
//! what the packaging lacks and what blocks a merge. Each block reads data
//! the server already has; a missing source says "unknown".

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

use crate::area::Index;
use crate::catalog::Subject;
use crate::committer::Committers;
use crate::health::{Health, Hydra, Last};
use crate::html::{Html, html};
use crate::model::Pr;
use crate::prpage::{File, Touched, ci_text, github_url};
use crate::web::{ago, icon, octicon, people};

use crate::web::NIXPKGS;
const MASS: &str =
    "https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md#changes-causing-mass-rebuilds";
/// nixpkgs asks to consider staging from this many rebuilds on.
const CONSIDER_STAGING: u64 = 500;
/// From this many rebuilds on, nixpkgs says a change must target staging.
const MASS_REBUILD: u64 = 1000;
const SHOWN_SUBJECTS: usize = 6;
const SHOWN_MAINTAINERS: usize = 8;
const SHOWN_HYDRA: usize = 3;
const CVE_CAP: usize = 20;
const REASON_CHARS: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    New,
    Update,
    Fix,
    Removal,
    Refactor,
    Module,
    Other,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::New => "new package",
            Kind::Update => "update",
            Kind::Fix => "fix",
            Kind::Removal => "removal",
            Kind::Refactor => "refactor",
            Kind::Module => "module change",
            Kind::Other => "change",
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Kind::New => "new",
            Kind::Update => "update",
            Kind::Fix => "fix",
            Kind::Removal => "removal",
            Kind::Refactor => "refactor",
            Kind::Module => "module",
            Kind::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Bump {
    pub name: String,
    pub from: String,
    pub to: String,
}

fn strip_prefix_tag(t: &str) -> &str {
    let t = t.trim();
    match t.strip_prefix('[').and_then(|r| r.split_once(']')) {
        Some((_, rest)) => rest.trim_start(),
        None => t,
    }
}

/// `hello: 2.12 -> 2.13`, also with `→` and a `[Backport …]` prefix.
pub(crate) fn bump(title: &str) -> Option<Bump> {
    let t = strip_prefix_tag(title);
    let (name, rest) = t.split_once(": ")?;
    let (before, after) = rest.split_once(" -> ").or_else(|| rest.split_once(" → "))?;
    let trim = |s: &str| {
        s.trim_matches(|c: char| ",;:()[]`'\"".contains(c))
            .to_string()
    };
    let from = trim(before.split_whitespace().last()?);
    let to = trim(after.split_whitespace().next()?);
    let name = name.trim();
    (!from.is_empty() && !to.is_empty() && !name.is_empty() && from != to).then(|| Bump {
        name: name.to_string(),
        from,
        to,
    })
}

fn is_package_file(path: &str) -> bool {
    path.starts_with("pkgs/") && (path.ends_with("/package.nix") || path.ends_with("/default.nix"))
}

pub(crate) fn kind(pr: &Pr, files: Option<&[File]>) -> Kind {
    let t = strip_prefix_tag(&pr.title).to_lowercase();
    let label = |l: &str| pr.labels.iter().any(|x| x == l);
    let rest = t.split_once(": ").map_or(t.as_str(), |(_, r)| r);
    if label("8.has: package (new)") || rest.starts_with("init at") || rest.contains(" init at ") {
        return Kind::New;
    }
    if bump(&pr.title).is_some() {
        return Kind::Update;
    }
    let removed = files.is_some_and(|f| {
        !f.is_empty()
            && f.iter()
                .any(|f| f.status == "removed" && is_package_file(&f.path))
            && f.iter().all(|f| f.status == "removed" || f.additions == 0)
    });
    if removed
        || ["remove", "drop", "delete"]
            .iter()
            .any(|w| rest.starts_with(w))
    {
        return Kind::Removal;
    }
    if label("8.has: package (update)") {
        return Kind::Update;
    }
    let all_nixos = !pr.files.is_empty() && pr.files.iter().all(|f| f.starts_with("nixos/"));
    if t.starts_with("nixos/") || label("8.has: module (update)") || all_nixos {
        return Kind::Module;
    }
    if rest.contains("fix") {
        return Kind::Fix;
    }
    if label("8.has: clean-up")
        || [
            "refactor",
            "clean up",
            "cleanup",
            "format",
            "treewide",
            "modernize",
            "move to by-name",
            "migrate to by-name",
        ]
        .iter()
        .any(|w| t.contains(w))
    {
        return Kind::Refactor;
    }
    Kind::Other
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rebuild {
    /// `linux`, `darwin`, `linux-stdenv`, `nixos-tests` and so on.
    pub platform: String,
    /// The count as the label writes it, like `501-1000`; empty when the
    /// label has none.
    pub count: String,
    pub min: u64,
}

impl Rebuild {
    fn stdenv(&self) -> bool {
        self.platform.ends_with("-stdenv")
    }

    pub(crate) fn mass(&self) -> bool {
        self.stdenv() || self.min >= MASS_REBUILD
    }

    fn large(&self) -> bool {
        self.mass() || self.min >= CONSIDER_STAGING
    }
}

/// The `10.rebuild-*` labels that the nixpkgs eval puts on a pull request.
pub(crate) fn rebuilds(labels: &[String]) -> Vec<Rebuild> {
    let mut out: Vec<Rebuild> = labels
        .iter()
        .filter_map(|l| {
            let rest = l.strip_prefix("10.rebuild-")?;
            let (platform, count) = match rest.split_once(": ") {
                Some((p, c)) => (p.trim(), c.trim()),
                None => (rest.trim(), ""),
            };
            let min = count
                .split(|c: char| !c.is_ascii_digit())
                .find(|s| !s.is_empty())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            Some(Rebuild {
                platform: platform.to_string(),
                count: count.to_string(),
                min,
            })
        })
        .collect();
    out.sort_by(|a, b| a.platform.cmp(&b.platform));
    out
}

/// CVE ids, upper case, in the order they first appear.
pub(crate) fn cves(text: &str) -> Vec<String> {
    let upper = text.to_ascii_uppercase();
    let b = upper.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while let Some(at) = upper[i..].find("CVE-") {
        let start = i + at;
        let mut j = start + 4;
        let year = b[j..].iter().take_while(|c| c.is_ascii_digit()).count();
        j += year;
        let ok_year = year == 4 && b.get(j) == Some(&b'-');
        let digits = if ok_year {
            b[j + 1..].iter().take_while(|c| c.is_ascii_digit()).count()
        } else {
            0
        };
        let before_ok = start == 0 || !b[start - 1].is_ascii_alphanumeric();
        if ok_year && digits >= 4 && before_ok {
            let id = upper[start..j + 1 + digits].to_string();
            if !out.contains(&id) {
                out.push(id);
            }
            i = j + 1 + digits;
        } else {
            i = start + 4;
        }
        if out.len() >= CVE_CAP {
            break;
        }
    }
    out
}

/// The numbers of a version, for its first parts.
fn numbers(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect()
}

/// The first number changes, or the second one while the first is 0.
pub(crate) fn major(from: &str, to: &str) -> bool {
    let (a, b) = (numbers(from), numbers(to));
    match (a.first(), b.first()) {
        (Some(0), Some(0)) => a.get(1) != b.get(1),
        (Some(x), Some(y)) => x != y,
        _ => false,
    }
}

/// The latest review state of each reviewer that still asks for changes.
pub(crate) fn changes_requested(pr: &Pr) -> Vec<String> {
    let mut reviews: Vec<&crate::model::Review> = pr
        .reviews
        .iter()
        .filter(|r| !r.author_is_bot && r.author != pr.author)
        .collect();
    reviews.sort_by_key(|r| r.submitted_at);
    let mut last: BTreeMap<&str, &str> = BTreeMap::new();
    for r in reviews {
        if matches!(
            r.state.as_str(),
            "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED"
        ) {
            last.insert(&r.author, &r.state);
        }
    }
    last.into_iter()
        .filter(|(_, s)| *s == "CHANGES_REQUESTED")
        .map(|(a, _)| a.to_string())
        .collect()
}

fn usual_base(base: &str) -> bool {
    let release = |rest: &str| {
        let mut p = rest.split('.');
        matches!((p.next(), p.next(), p.next()), (Some(y), Some(m), None)
            if y.len() == 2 && m.len() == 2 && y.bytes().chain(m.bytes()).all(|c| c.is_ascii_digit()))
    };
    matches!(
        base,
        "master" | "staging" | "staging-next" | "staging-nixos"
    ) || base.ends_with("-updates")
        || ["release-", "staging-next-", "staging-"]
            .iter()
            .any(|p| base.strip_prefix(p).is_some_and(release))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Level {
    Block,
    Warn,
}

/// What stops or slows a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Blocker {
    Merged,
    Closed,
    Draft,
    Conflict { base: String },
    ChecksFail,
    ChangesRequested { by: Vec<String> },
    MassRebuild { rebuild: Rebuild, base: String },
    LargeRebuild { rebuild: Rebuild, base: String },
    NixosTests,
    UnusualBase { base: String },
    ChecksPending,
}

const NIXOS_TESTS: &str = "https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md#changes-rebuilding-all-nixos-tests";

impl Blocker {
    pub(crate) fn level(&self) -> Level {
        match self {
            Blocker::LargeRebuild { .. }
            | Blocker::NixosTests
            | Blocker::UnusualBase { .. }
            | Blocker::ChecksPending => Level::Warn,
            _ => Level::Block,
        }
    }

    fn key(&self) -> &'static str {
        match self {
            Blocker::Merged => "merged",
            Blocker::Closed => "closed",
            Blocker::Draft => "draft",
            Blocker::Conflict { .. } => "conflict",
            Blocker::ChecksFail => "checks_fail",
            Blocker::ChangesRequested { .. } => "changes_requested",
            Blocker::MassRebuild { .. } => "mass_rebuild",
            Blocker::LargeRebuild { .. } => "large_rebuild",
            Blocker::NixosTests => "nixos_tests",
            Blocker::UnusualBase { .. } => "unusual_base",
            Blocker::ChecksPending => "checks_pending",
        }
    }

    pub(crate) fn html(&self) -> Html {
        match self {
            Blocker::Merged => "It is merged.".into(),
            Blocker::Closed => "It is closed.".into(),
            Blocker::Draft => "It is a draft.".into(),
            Blocker::Conflict { base } => {
                html!("It has a conflict with <span class=\"branch-name\">{base}</span>.")
            }
            Blocker::ChecksFail => "Checks fail.".into(),
            Blocker::ChangesRequested { by } => {
                html!("Changes requested by {}.", people(by))
            }
            Blocker::MassRebuild { rebuild, base } => html!(
                "A mass rebuild ({}) that targets <span class=\"branch-name\">{base}</span>: \
                 <a href=\"{MASS}\">nixpkgs sends it to staging</a>.",
                rebuild_words(rebuild)
            ),
            Blocker::LargeRebuild { rebuild, base } => html!(
                "{} rebuilds on <span class=\"branch-name\">{base}</span>: \
                 <a href=\"{MASS}\">nixpkgs asks to consider staging</a>.",
                rebuild_words(rebuild)
            ),
            Blocker::NixosTests => html!(
                "It rebuilds all NixOS tests: <a href=\"{NIXOS_TESTS}\">nixpkgs may send it to staging-nixos</a>."
            ),
            Blocker::UnusualBase { base } => html!(
                "The base branch <span class=\"branch-name\">{base}</span> is not master, staging or a release branch."
            ),
            Blocker::ChecksPending => "Checks are pending.".into(),
        }
    }

    pub(crate) fn text(&self) -> String {
        match self {
            Blocker::ChangesRequested { by } => format!("Changes requested by {}.", by.join(", ")),
            Blocker::MassRebuild { rebuild, base } => format!(
                "A mass rebuild ({}) that targets {base}: nixpkgs sends it to staging.",
                rebuild_words(rebuild)
            ),
            Blocker::LargeRebuild { rebuild, base } => format!(
                "{} rebuilds on {base}: nixpkgs asks to consider staging.",
                rebuild_words(rebuild)
            ),
            Blocker::NixosTests => {
                "It rebuilds all NixOS tests: nixpkgs may send it to staging-nixos.".into()
            }
            Blocker::Conflict { base } => format!("It has a conflict with {base}."),
            Blocker::UnusualBase { base } => {
                format!("The base branch {base} is not master, staging or a release branch.")
            }
            b => b.html().into_string(),
        }
    }
}

impl Serialize for Blocker {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("Blocker", 3)?;
        st.serialize_field("kind", self.key())?;
        st.serialize_field("level", &self.level())?;
        st.serialize_field("text", &self.text())?;
        st.end()
    }
}

/// What stops or slows a merge, the hard stops first.
pub(crate) fn blockers(pr: &Pr, rebuilds: &[Rebuild]) -> Vec<Blocker> {
    let mut out = Vec::new();
    if pr.state == "MERGED" {
        out.push(Blocker::Merged);
        return out;
    }
    if !pr.is_open() {
        out.push(Blocker::Closed);
        return out;
    }
    if pr.is_draft {
        out.push(Blocker::Draft);
    }
    let base = || pr.base.clone();
    if pr.mergeable == "CONFLICTING" {
        out.push(Blocker::Conflict { base: base() });
    }
    if matches!(pr.ci.as_deref(), Some("FAILURE" | "ERROR")) {
        out.push(Blocker::ChecksFail);
    }
    let asked = changes_requested(pr);
    if !asked.is_empty() {
        out.push(Blocker::ChangesRequested { by: asked });
    }
    let staging = pr.base.starts_with("staging");
    if !staging {
        if let Some(r) = rebuilds.iter().find(|r| r.mass()) {
            out.push(Blocker::MassRebuild {
                rebuild: r.clone(),
                base: base(),
            });
        } else if let Some(r) = rebuilds.iter().find(|r| r.large()) {
            out.push(Blocker::LargeRebuild {
                rebuild: r.clone(),
                base: base(),
            });
        }
        if rebuilds
            .iter()
            .any(|r| r.platform == "nixos-tests" && pr.base == "master")
        {
            out.push(Blocker::NixosTests);
        }
    }
    if !usual_base(&pr.base) {
        out.push(Blocker::UnusualBase { base: base() });
    }
    if matches!(pr.ci.as_deref(), Some("PENDING" | "EXPECTED")) {
        out.push(Blocker::ChecksPending);
    }
    out.sort_by_key(Blocker::level);
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Check {
    /// `Some(true)` passes, `Some(false)` fails, `None` is unknown.
    pub ok: Option<bool>,
    pub text: Html,
    pub guide: Option<&'static str>,
}

impl Check {
    fn guide_url(&self) -> Option<String> {
        self.guide.map(|g| {
            if g.starts_with("https://") {
                g.to_string()
            } else {
                format!("{NIXPKGS}/{g}")
            }
        })
    }
}

impl Serialize for Check {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("Check", 3)?;
        st.serialize_field("ok", &self.ok)?;
        st.serialize_field("text", &crate::html::to_text(&self.text))?;
        st.serialize_field("guide", &self.guide_url())?;
        st.end()
    }
}

fn check(ok: Option<bool>, text: impl Into<Html>, guide: Option<&'static str>) -> Check {
    Check {
        ok,
        text: text.into(),
        guide,
    }
}

const G_COMMIT: &str = "CONTRIBUTING.md#commit-conventions";
const G_META: &str = "pkgs/README.md#meta-attributes";
const G_SOURCES: &str = "pkgs/README.md#sources";
const G_TESTS: &str = "pkgs/README.md#package-tests";
const G_AI: &str = "CONTRIBUTING.md#automationai-policy";
const VERSION_CHECK: &str = "https://nixos.org/manual/nixpkgs/unstable/#versioncheckhook";

fn added(files: &[File]) -> impl Iterator<Item = &str> {
    files
        .iter()
        .filter_map(|f| f.patch.as_deref())
        .flat_map(str::lines)
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .map(|l| l[1..].trim())
}

/// The checks on the lines the diff adds.
pub(crate) fn diff_checks(kind: Kind, files: &[File]) -> Vec<Check> {
    let lines: Vec<&str> = added(files).collect();
    let has = |s: &str| lines.iter().any(|l| l.contains(s));
    let mut out = Vec::new();
    if has("fakeHash") || has("fakeSha256") {
        out.push(check(
            Some(false),
            "A <code>lib.fakeHash</code> is left in the diff.",
            Some(G_SOURCES),
        ));
    }
    let old_hash = lines.iter().any(|l| {
        l.split_once("sha256 = \"")
            .is_some_and(|(before, v)| !before.ends_with("cargo") && !v.starts_with("sha256-"))
    });
    if old_hash {
        out.push(check(
            Some(false),
            "A hash in the old form: use <code>hash = \"sha256-…\"</code>.",
            Some(G_SOURCES),
        ));
    }
    if has("cargoSha256") {
        out.push(check(
            Some(false),
            "<code>cargoSha256</code> is deprecated: use <code>cargoHash</code>.",
            Some(G_SOURCES),
        ));
    }
    if !old_hash && has("hash = \"sha256-") {
        out.push(check(
            Some(true),
            "The diff pins its hashes in SRI form.",
            Some(G_SOURCES),
        ));
    }
    let tests = has("passthru.tests") || has("nixosTests") || has("testers.") || has("tests = ");
    out.push(if tests {
        check(Some(true), "The diff adds or changes tests.", Some(G_TESTS))
    } else {
        check(
            None,
            "Tests: the diff adds none; the package may already have them.",
            Some(G_TESTS),
        )
    });
    if kind == Kind::New {
        for (word, what) in [
            ("description =", "meta.description"),
            ("homepage =", "meta.homepage"),
            ("license =", "meta.license"),
            ("maintainers =", "meta.maintainers"),
        ] {
            if !has(word) {
                out.push(check(
                    Some(false),
                    html!("The new package sets no <code>{what}</code>."),
                    Some(G_META),
                ));
            }
        }
        let by_name = files.iter().any(|f| f.path.starts_with("pkgs/by-name/"));
        if by_name && !has("versionCheckHook") {
            out.push(check(
                Some(false),
                "The diff adds no <code>versionCheckHook</code>; a program needs one.",
                Some(VERSION_CHECK),
            ));
        }
        if !has("platforms =") {
            out.push(check(
                Some(false),
                "The new package sets no <code>meta.platforms</code>.",
                Some(G_META),
            ));
        }
    }
    if has("versionCheckHook") {
        out.push(check(
            Some(true),
            "The diff adds <code>versionCheckHook</code>.",
            Some(VERSION_CHECK),
        ));
    }
    out
}

/// The facts the index reads from master for one package.
fn package_checks(attr: &str, p: &crate::area::Package) -> Vec<Check> {
    let mut out = Vec::new();
    let name = html!("<code>{attr}</code>");
    match crate::hygiene::gaps(attr, p) {
        None => out.push(check(
            None,
            html!("{name}: meta is not read yet."),
            Some(G_META),
        )),
        Some(g) if g.is_empty() => out.push(check(
            Some(true),
            html!(
                "{name}: meta is complete{}.",
                if crate::hygiene::program(attr, p) {
                    ", with versionCheckHook"
                } else {
                    ""
                }
            ),
            Some(G_META),
        )),
        Some(g) => {
            for gap in g {
                out.push(check(
                    Some(false),
                    html!("{name}: {gap}."),
                    Some(if gap.contains("versionCheckHook") {
                        VERSION_CHECK
                    } else {
                        G_META
                    }),
                ));
            }
        }
    }
    if let Some(m) = &p.meta {
        out.push(match &m.platforms {
            Some(pl) => check(
                Some(true),
                html!(
                    "{name}: meta.platforms names {} platform{}.",
                    pl.count,
                    if pl.count == 1 { "" } else { "s" }
                ),
                Some(G_META),
            ),
            None => check(
                Some(false),
                html!("{name}: no meta.platforms."),
                Some(G_META),
            ),
        });
    }
    out
}

fn title_checks(pr: &Pr) -> Vec<Check> {
    let mut out = vec![if crate::subject::of_title(&pr.title).is_empty() {
        check(
            Some(false),
            "The title does not start with <code>name:</code>, like <code>hello: 2.12 -&gt; 2.13</code>.",
            Some(G_COMMIT),
        )
    } else {
        check(
            Some(true),
            "The title names what it changes.",
            Some(G_COMMIT),
        )
    }];
    if pr.ai.as_deref() == Some(crate::github::OTHER_FORM) {
        out.push(check(
            Some(false),
            "AI use is attributed in a form nixpkgs does not accept: use an <code>Assisted-by:</code> trailer.",
            Some(G_AI),
        ));
    }
    out
}

/// What one GitHub release list tells about a version bump.
#[derive(Debug, Clone, Default)]
pub(crate) struct Notes {
    pub release_url: Option<String>,
    pub compare_url: Option<String>,
    pub breaking: bool,
}

pub(crate) struct Ctx<'a> {
    pub pr: &'a Pr,
    /// The description, when it could be read.
    pub body: Option<&'a str>,
    /// The changed files, or why they could not be read.
    pub files: Result<&'a [File], &'a str>,
    pub touched: &'a [Touched],
    pub index: &'a Index,
    pub health: &'a Health,
    pub committers: &'a Committers,
    pub author: &'a crate::profile::Facts,
    /// Per package: the last Hydra build a page view already read.
    pub hydra: &'a [(String, Option<Last>)],
    pub notes: Option<&'a Notes>,
    /// The packages of the viewer's machines; `None` for a visitor.
    pub mine: Option<&'a BTreeSet<String>>,
    /// Buttons another feature adds at the end of the actions row, like Next.
    pub extra_actions: Html,
    /// For a merged pull request, the branches that hold it, in the place
    /// of the blockers.
    pub reached: Option<Html>,
    /// Its place in the queues of Maintainer Mode, as a block.
    pub queue: Html,
}

/// A long error, like a failed connection with its URL, shows as a short
/// reason; the whole text is in the tooltip.
fn reason(why: &str) -> Html {
    if why.chars().count() <= REASON_CHARS {
        Html::text(why)
    } else {
        html!("<span title=\"{why}\">GitHub could not be read</span>")
    }
}

fn unknown(why: &str) -> Html {
    html!(
        "<span class=\"color-fg-muted\">unknown: {}</span>",
        reason(why)
    )
}

fn line(label: impl Into<Html>, value: impl Into<Html>) -> Html {
    let (label, value) = (label.into(), value.into());
    html!("<li><span class=\"decide-label\">{label}</span> {value}</li>")
}

fn block(id: &str, title: &str, lines: Html) -> Html {
    html!(
        "<div class=\"decide-block\" id=\"decide-{id}\"><h3 class=\"decide-title\">{title}</h3><ul class=\"decide-list\">{lines}</ul></div>"
    )
}

/// The platform and count of a rebuild label, as text.
fn rebuild_words(r: &Rebuild) -> String {
    if r.count.is_empty() {
        r.platform.clone()
    } else {
        format!("{} {}", r.platform, r.count)
    }
}

fn packages<'a>(c: &'a Ctx) -> impl Iterator<Item = &'a str> {
    c.touched.iter().filter_map(|t| match &t.subject {
        Subject::Pkg(a) => Some(a.as_str()),
        _ => None,
    })
}

/// The package a bump title names, else the only package it touches.
pub(crate) fn bumped<'a>(index: &'a Index, touched: &[Touched], b: &Bump) -> Option<&'a str> {
    if let Some((k, _)) = index.pkgs.get_key_value(b.name.as_str()) {
        return Some(k);
    }
    if let Some(a) = index.resolve(&b.name) {
        return index.pkgs.get_key_value(a).map(|(k, _)| k.as_str());
    }
    let mut pkgs = touched.iter().filter_map(|t| match &t.subject {
        Subject::Pkg(a) => index
            .pkgs
            .get_key_value(a.as_str())
            .map(|(k, _)| k.as_str()),
        _ => None,
    });
    let first = pkgs.next()?;
    pkgs.next().is_none().then_some(first)
}

fn change_block(c: &Ctx, kind: Kind, b: Option<&Bump>) -> Html {
    let pr = c.pr;
    let mut lines = Html::new();
    let mut what = html!("<span class=\"Label\">{}</span>", kind.word());
    if pr.base.starts_with("release-") {
        what.push(&html!(
            " <span class=\"Label\">backport to {}</span>",
            pr.base
        ));
    }
    if let Some(t) = &pr.treewide {
        what.push(&html!(" <span class=\"Label\">{}</span>", t.mark()));
    }
    lines.push(&line("Kind", what));
    if let Some(b) = b {
        let mut v = html!("<code>{}</code> → <code>{}</code>", b.from, b.to);
        if let Some(p) = bumped(c.index, c.touched, b).and_then(|a| c.index.pkgs.get(a)) {
            if !p.version.is_empty() && p.version != b.from {
                v.push(&html!(
                    " <span class=\"color-fg-attention f6\">master has {}</span>",
                    p.version
                ));
            }
        }
        lines.push(&line("Version", v));
    }
    let touched = if let Some(t) = crate::links::TooLarge::of(pr) {
        html!(
            "<span class=\"color-fg-attention\">none: it changes {} files, too many to link to packages and people</span>",
            crate::chart::thousands(t.files as i64)
        )
    } else if c.touched.is_empty() {
        unknown("no package or module in the index matches its files or title")
    } else {
        let mut s: Vec<Html> = c
            .touched
            .iter()
            .map(|t| {
                let version = match &t.subject {
                    Subject::Pkg(a) if b.is_none() => c
                        .index
                        .pkgs
                        .get(a)
                        .filter(|p| !p.version.is_empty())
                        .map(|p| html!(" <span class=\"color-fg-muted f6\">{}</span>", p.version))
                        .unwrap_or_default(),
                    _ => Html::new(),
                };
                let mine = Html::from(match (&t.subject, c.mine) {
                    (Subject::Pkg(a), Some(m)) if m.contains(a) => {
                        " <span class=\"Label Label--attention\">on your machines</span>"
                    }
                    _ => "",
                });
                html!(
                    "<a href=\"{}\">{}</a>{version}{mine}",
                    t.subject.href(),
                    t.subject.label()
                )
            })
            .collect();
        let rest = s.split_off(s.len().min(SHOWN_SUBJECTS));
        let unlisted = pr.treewide.map_or(0, |t| {
            (t.packages + t.modules).saturating_sub(c.touched.len())
        });
        let mut out = crate::html::join(s, ", ");
        if !rest.is_empty() {
            let tail = if unlisted > 0 {
                html!(" and {unlisted} more")
            } else {
                Html::new()
            };
            out.push(&html!(
                "<details><summary class=\"f6 color-fg-muted\">{} more</summary>{}{tail}</details>",
                rest.len() + unlisted,
                crate::html::join(rest, ", ")
            ));
        }
        out
    };
    lines.push(&line("Touches", touched));
    lines.push(&line(
        "Size",
        html!(
            "<span class=\"color-fg-success\">+{}</span> <span class=\"color-fg-danger\">−{}</span> in {} file{}",
            pr.additions,
            pr.deletions,
            pr.changed_files,
            if pr.changed_files == 1 { "" } else { "s" }
        ),
    ));
    if pr.ai.is_some() {
        lines.push(&line("AI use", ai_text(pr.ai.as_deref())));
    }
    if !pr.labels.is_empty() {
        lines.push(&html!(
            "<li><details><summary class=\"f6 color-fg-muted\">Labels ({})</summary>{}</details></li>",
            pr.labels.len(),
            crate::html::join(
                pr.labels
                    .iter()
                    .map(|l| html!("<span class=\"Label mr-1\">{l}</span>")),
                " "
            )
        ));
    }
    block("change", "What changes", lines)
}

fn ai_text(ai: Option<&str>) -> Html {
    match ai {
        None => "none disclosed in the last commit or the description".into(),
        Some(crate::github::OTHER_FORM) => {
            "attributed in a form the nixpkgs policy does not accept: not an <code>Assisted-by:</code> trailer"
                .into()
        }
        Some("disclosed") => "disclosed".into(),
        Some(t) => html!("disclosed: {t}"),
    }
}

/// The review guidance of the nixpkgs docs, for the checks no program runs.
fn guide() -> Html {
    let items: [(&str, String); 6] = [
        (
            "The commit messages follow the nixpkgs format, like <code>hello: 2.12 -&gt; 2.13</code> or <code>nixos/nginx: fix the test</code>.",
            format!("{NIXPKGS}/CONTRIBUTING.md#commit-conventions"),
        ),
        (
            "<code>meta</code> is complete: <code>description</code>, <code>homepage</code>, <code>license</code>, <code>maintainers</code>, <code>mainProgram</code>.",
            format!("{NIXPKGS}/pkgs/README.md#meta-attributes"),
        ),
        (
            "Sources are fixed-output and pinned by a hash. No <code>lib.fakeHash</code> is left.",
            format!("{NIXPKGS}/pkgs/README.md#sources"),
        ),
        (
            "A command-line tool has <code>versionCheckHook</code>, or another check that runs it.",
            "https://nixos.org/manual/nixpkgs/unstable/#versioncheckhook".to_string(),
        ),
        (
            "<code>meta.platforms</code> matches what upstream supports.",
            format!("{NIXPKGS}/pkgs/README.md#meta-attributes"),
        ),
        (
            "The tests run: <code>passthru.tests</code>, the NixOS test of a module, or nixpkgs-review.",
            format!("{NIXPKGS}/pkgs/README.md#package-tests"),
        ),
    ];
    let items: Html = items
        .iter()
        .map(|(text, href)| {
            html!(
                "<li>{} <a href=\"{href}\">Guide</a></li>",
                Html::from(*text)
            )
        })
        .collect();
    html!(
        "<li><details><summary class=\"f6 color-fg-muted\">Review checklist</summary>\
         <ul class=\"ml-3 mt-1\">{items}</ul>\
         <p class=\"f6 color-fg-muted mt-1\">From <a href=\"{NIXPKGS}/CONTRIBUTING.md#how-to-review-pull-requests\">CONTRIBUTING.md</a> \
         and <a href=\"{NIXPKGS}/pkgs/README.md#reviewing-contributions\">pkgs/README.md</a>.</p></details></li>"
    )
}

fn risk_block(pr: &Pr, rebuilds: &[Rebuild]) -> Html {
    let mut lines = Html::new();
    let counts = if rebuilds.is_empty() {
        unknown("no 10.rebuild label yet; the nixpkgs eval adds them")
    } else {
        crate::html::join(
            rebuilds.iter().map(|r| {
                let text = rebuild_words(r);
                if r.mass() {
                    html!("<span class=\"Label Label--danger\">{text}</span>")
                } else if r.large() {
                    html!("<span class=\"Label Label--attention\">{text}</span>")
                } else {
                    html!("<span class=\"Label\">{text}</span>")
                }
            }),
            " ",
        )
    };
    lines.push(&line("Rebuilds", counts));
    let mut base = html!("<span class=\"branch-name\">{}</span>", pr.base);
    if pr.base.starts_with("staging") {
        base.push(&Html::from(
            " <span class=\"color-fg-muted f6\">reaches master with the next staging cycle</span>",
        ));
    }
    lines.push(&line("Base", base));
    if rebuilds.iter().any(Rebuild::mass) {
        lines.push(&line(
            "Scale",
            "<span class=\"color-fg-danger\">mass rebuild</span>",
        ));
    }
    block("risk", "Risk", lines)
}

fn upstream_block(c: &Ctx, b: &Bump) -> Html {
    let mut lines = Html::new();
    let attr = bumped(c.index, c.touched, b);
    let p = attr.and_then(|a| c.index.pkgs.get(a));
    let mut links: Vec<Html> = Vec::new();
    if let Some(n) = c.notes {
        if let Some(u) = &n.release_url {
            links.push(html!("<a href=\"{u}\">release {}</a>", b.to));
        }
        if let Some(u) = &n.compare_url {
            links.push(html!("<a href=\"{u}\">compare {} → {}</a>", b.from, b.to));
        }
    }
    if let Some(log) = p
        .and_then(|p| p.meta.as_ref())
        .and_then(|m| m.changelog.first())
    {
        if log.contains(&b.from) && !log.contains(&b.to) {
            links.push(html!(
                "<a href=\"{}\">changelog</a>",
                log.replace(&b.from, &b.to)
            ));
        } else {
            links.push(html!(
                "<a href=\"{log}\">changelog</a> <span class=\"color-fg-muted f6\">as master names it</span>"
            ));
        }
    }
    let repo = p
        .and_then(|p| p.facts.as_ref())
        .and_then(|f| f.src_url.as_deref())
        .and_then(github_repo);
    if let Some(r) = &repo {
        if c.notes.is_none_or(|n| n.release_url.is_none()) {
            links.push(html!(
                "<a href=\"https://github.com/{r}/releases\">releases</a>"
            ));
        }
    }
    lines.push(&line(
        "Notes",
        if links.is_empty() {
            unknown("no meta.changelog and no GitHub source in the index")
        } else {
            crate::html::join(links, " · ")
        },
    ));
    let mut flags: Vec<Html> = Vec::new();
    if c.notes.is_some_and(|n| n.breaking) {
        flags.push(
            "<span class=\"Label Label--danger\">the release notes say breaking</span>".into(),
        );
    }
    if major(&b.from, &b.to) {
        flags.push("<span class=\"Label Label--attention\">major version: may break</span>".into());
    }
    if let Some((v, from)) = attr.and_then(|a| c.health.newest(a, &b.to)) {
        flags.push(html!(
            "<span class=\"color-fg-muted f6\">upstream also has {v} ({from})</span>"
        ));
    }
    if !flags.is_empty() {
        lines.push(&line("Flags", crate::html::join(flags, " ")));
    }
    block("upstream", "Upstream", lines)
}

/// `owner/repo` of a GitHub `src` URL.
pub(crate) fn github_repo(src_url: &str) -> Option<String> {
    match crate::upstream::origin(src_url)? {
        crate::upstream::Origin::Git(u) => u
            .strip_prefix("https://github.com/")
            .filter(|r| r.split('/').count() == 2)
            .map(String::from),
        _ => None,
    }
}

fn builds_block(c: &Ctx) -> Html {
    let mut lines = Html::new();
    lines.push(&line("Checks", ci_text(c.pr.ci.as_deref())));
    for (attr, last) in c.hydra.iter().take(SHOWN_HYDRA) {
        let job = Hydra::package_job(attr);
        let v = match last {
            Some(Last::Build(b)) => html!(
                "<a class=\"{}\" href=\"{}\">{}</a>{}",
                if b.ok {
                    "color-fg-success"
                } else {
                    "color-fg-danger"
                },
                b.url(),
                b.status,
                b.at.map(|t| html!(" <span class=\"color-fg-muted f6\">{} ago</span>", ago(t)))
                    .unwrap_or_default()
            ),
            Some(Last::Missing) => "<span class=\"color-fg-muted\">no finished build</span>".into(),
            _ => html!(
                "<span class=\"color-fg-muted\">unknown: no page read it in the last hour; <a href=\"{}\">see Hydra</a></span>",
                Hydra::job_url(&job)
            ),
        };
        lines.push(&line(html!("Hydra <code>{attr}</code>"), v));
    }
    block("builds", "Builds", lines)
}

fn people_block(c: &Ctx) -> Html {
    let pr = c.pr;
    let mut lines = Html::new();
    let author_l = pr.author.to_lowercase();
    let maintainers: Vec<String> = {
        let mut seen = BTreeSet::new();
        c.touched
            .iter()
            .flat_map(|t| t.maintainers.iter())
            .filter(|m| seen.insert(m.to_lowercase()))
            .cloned()
            .collect()
    };
    let mut facts: Vec<Html> = Vec::new();
    let first = crate::queue::is_first_contribution(pr);
    if pr.author_is_bot {
        facts.push("<span class=\"Label\">bot</span>".into());
    } else if first {
        facts.push("<span class=\"Label Label--attention\">first contribution</span>".into());
    } else if let Some(t) = c.author.first_pr_at.filter(|t| *t < pr.created_at) {
        facts.push(html!(
            "<span class=\"f6 color-fg-muted\">contributes since {}</span>",
            t.format("%Y").to_string()
        ));
    }
    if c.author.committer {
        facts.push("<span class=\"Label\">committer</span>".into());
    }
    if maintainers.iter().any(|m| m.to_lowercase() == author_l) {
        facts.push("<span class=\"Label Label--success\">maintains it</span>".into());
    } else if c.author.packages > 0 {
        facts.push(html!(
            "<span class=\"f6 color-fg-muted\">maintains {} package{}</span>",
            c.author.packages,
            if c.author.packages == 1 { "" } else { "s" }
        ));
    }
    lines.push(&line(
        "Author",
        html!(
            "<a href=\"/u/{0}\">{0}</a> {1}",
            pr.author,
            crate::html::join(facts, " ")
        ),
    ));
    let m = if crate::links::TooLarge::of(pr).is_some() {
        unknown("too large to link, see Touches")
    } else if c.touched.is_empty() {
        unknown("no package or module in the index matches")
    } else if maintainers.is_empty() {
        let teams: BTreeSet<&String> = c.touched.iter().flat_map(|t| t.teams.iter()).collect();
        if teams.is_empty() {
            "<span class=\"Label Label--attention\">no maintainer</span>".into()
        } else {
            html!(
                "teams {}",
                crate::html::join(teams.iter().map(|t| Html::text(t)), ", ")
            )
        }
    } else {
        let mut s = people(
            &maintainers
                .iter()
                .take(SHOWN_MAINTAINERS)
                .cloned()
                .collect::<Vec<_>>(),
        );
        if maintainers.len() > SHOWN_MAINTAINERS {
            s.push(&html!(
                " and {} more",
                maintainers.len() - SHOWN_MAINTAINERS
            ));
        }
        s
    };
    lines.push(&line("Maintainers", m));
    let approvals = crate::committer::approvals(pr);
    let a = if approvals.is_empty() {
        "<span class=\"color-fg-muted\">none on the current head</span>".into()
    } else {
        crate::html::join(
            approvals.iter().map(|(l, _)| {
                let mut tags = Html::new();
                if maintainers.iter().any(|m| m.eq_ignore_ascii_case(l))
                    || c.touched
                        .iter()
                        .any(|t| t.approved_by.iter().any(|x| x.eq_ignore_ascii_case(l)))
                {
                    tags.push(&Html::from(
                        " <span class=\"Label Label--success\">maintainer</span>",
                    ));
                }
                if c.committers.is(l) {
                    tags.push(&Html::from(" <span class=\"Label\">committer</span>"));
                }
                html!("<a href=\"/u/{0}\">{0}</a>{tags}", l)
            }),
            ", ",
        )
    };
    lines.push(&line("Approved", a));
    block("people", "People", lines)
}

/// A tracker advisory on a package, with what nixpkgs already fixes.
fn advisory_ref(c: &Ctx, attr: &str, a: &crate::health::Advisory) -> Html {
    use crate::affected::Verdict;
    let status = match c.health.verdict(attr, &a.code) {
        Verdict::Affects => Html::new(),
        Verdict::Unknown => " <span class=\"color-fg-muted f6\">range unknown</span>".into(),
        Verdict::Behind { channels, .. } => html!(
            " <span class=\"color-fg-attention f6\">fixed in master, still in {}</span>",
            channels
                .iter()
                .map(|l| l.channel.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Verdict::Fixed { fixed_in } => html!(
            " <span class=\"color-fg-success f6\">{}, the tracker issue can be closed</span>",
            fixed_in.as_deref().map_or_else(
                || "fixed everywhere".to_string(),
                |f| format!("fixed in {f}")
            )
        ),
    };
    html!(
        "<a href=\"{}\">{}</a> on <code>{attr}</code>{status}",
        a.url(),
        a.code
    )
}

/// The block, or nothing when no CVE, security label or open advisory
/// relates to the pull request.
fn security_block(c: &Ctx) -> Html {
    let mut text = c.pr.title.clone();
    if let Some(b) = c.body {
        text.push('\n');
        text.push_str(b);
    }
    let named = cves(&text);
    let label = c.pr.labels.iter().any(|l| l == crate::committer::SECURITY);
    let pkgs: Vec<&str> = packages(c).collect();
    let tracked: Vec<(&str, &crate::health::Advisory)> = pkgs
        .iter()
        .flat_map(|a| {
            c.health
                .advisories
                .get(*a)
                .into_iter()
                .flatten()
                .map(move |adv| (*a, adv))
        })
        .collect();
    let live = tracked
        .iter()
        .any(|(attr, a)| !c.health.verdict(attr, &a.code).fixed());
    if named.is_empty() && !label && !live {
        return Html::new();
    }
    let mut lines = Html::new();
    if label {
        lines.push(&line(
            "Label",
            "<span class=\"Label Label--danger\">security</span>",
        ));
    }
    let mut fixes: Vec<Html> = Vec::new();
    for id in &named {
        let w = crate::severity::worst(c.health, std::iter::once(id));
        let band = match w.band {
            Some(b) => html!(
                " <span class=\"Label {}\">{}{}</span>",
                b.label_class(),
                b.word(),
                w.cvss.map(|s| format!(" {s:.1}")).unwrap_or_default()
            ),
            None => " <span class=\"color-fg-muted f6\">severity unknown</span>".into(),
        };
        let adv = tracked
            .iter()
            .find(|(_, a)| a.cves.iter().any(|x| x.eq_ignore_ascii_case(id)))
            .map(|(attr, a)| html!(" · {}", advisory_ref(c, attr, a)))
            .unwrap_or_default();
        fixes.push(html!(
            "<a href=\"https://nvd.nist.gov/vuln/detail/{0}\">{0}</a>{band}{adv}",
            id
        ));
    }
    let fixes = if fixes.is_empty() {
        "<span class=\"color-fg-muted\">it names no CVE</span>".into()
    } else {
        crate::html::join(fixes, "<br>")
    };
    lines.push(&line("Fixes", fixes));
    let (fixed, other): (Vec<Html>, Vec<Html>) = {
        let (f, o): (Vec<_>, Vec<_>) = tracked
            .iter()
            .filter(|(_, a)| {
                !a.cves
                    .iter()
                    .any(|x| named.iter().any(|n| n.eq_ignore_ascii_case(x)))
            })
            .partition(|(attr, a)| c.health.verdict(attr, &a.code).fixed());
        let refs = |l: Vec<&(&str, &crate::health::Advisory)>| {
            l.into_iter()
                .map(|(attr, a)| advisory_ref(c, attr, a))
                .collect()
        };
        (refs(f), refs(o))
    };
    if !other.is_empty() {
        lines.push(&line("Not named", crate::html::join(other, ", ")));
    }
    if !fixed.is_empty() {
        lines.push(&line("Already fixed", crate::html::join(fixed, ", ")));
    }
    if c.body.is_none() {
        lines.push(&line(
            "Read",
            "<span class=\"color-fg-muted\">the title only: the description could not be read</span>",
        ));
    }
    if let Some(mine) = c.mine {
        let hit: Vec<&str> = pkgs.iter().copied().filter(|a| mine.contains(*a)).collect();
        if !hit.is_empty() {
            lines.push(&line(
                "You",
                html!(
                    "<span class=\"Label Label--attention\">on your machines: {}</span>",
                    hit.join(", ")
                ),
            ));
        }
    }
    block("security", "Security", lines)
}

fn check_li(c: &Check) -> Html {
    let (ic, class) = match c.ok {
        Some(true) => (octicon!("check-circle-fill"), "color-fg-success"),
        Some(false) => (octicon!("x-circle-fill"), "color-fg-danger"),
        None => (octicon!("circle"), "color-fg-muted"),
    };
    let guide = c
        .guide_url()
        .map(|href| html!(" <a class=\"f6\" href=\"{href}\">Guide</a>"))
        .unwrap_or_default();
    html!(
        "<li><span class=\"{class}\">{}</span> {}{guide}</li>",
        icon(ic),
        c.text
    )
}

pub(crate) fn checks(c: &Ctx, kind: Kind) -> Vec<Check> {
    checks_of(c.pr, c.files, c.touched, c.index, kind)
}

/// What the page and the API check on a pull request: its title, its diff
/// and the packages it touches, as on master.
pub(crate) fn checks_of(
    pr: &Pr,
    files: Result<&[File], &str>,
    touched: &[Touched],
    index: &Index,
    kind: Kind,
) -> Vec<Check> {
    let mut all = title_checks(pr);
    match files {
        Ok(f) => all.extend(diff_checks(kind, f)),
        Err(why) => all.push(check(
            None,
            html!("Checks on the diff: unknown, {}.", reason(why)),
            None,
        )),
    }
    let mut seen = 0;
    for t in touched {
        let Subject::Pkg(a) = &t.subject else {
            continue;
        };
        if let Some(p) = index.pkgs.get(a) {
            all.extend(package_checks(a, p));
            seen += 1;
        }
        if seen >= SHOWN_HYDRA {
            break;
        }
    }
    if kind == Kind::New && seen == 0 {
        all.push(check(
            None,
            "The index reads master: a new package is not in it yet.",
            None,
        ));
    }
    all
}

fn missing_block(list: &[Check]) -> Html {
    let failed: Vec<&Check> = list.iter().filter(|c| c.ok == Some(false)).collect();
    let open: Vec<&Check> = list.iter().filter(|c| c.ok.is_none()).collect();
    let passed: Vec<&Check> = list.iter().filter(|c| c.ok == Some(true)).collect();
    let mut lines: Html = failed
        .iter()
        .chain(open.iter())
        .map(|c| check_li(c))
        .collect();
    if failed.is_empty() && open.is_empty() {
        lines.push(&Html::from(
            "<li class=\"color-fg-success\">Nothing missing that these checks can see.</li>",
        ));
    }
    if !passed.is_empty() {
        lines.push(&html!(
            "<li><details><summary class=\"f6 color-fg-muted\">{} check{} pass</summary><ul class=\"decide-list\">{}</ul></details></li>",
            passed.len(),
            if passed.len() == 1 { "" } else { "s" },
            passed.iter().map(|c| check_li(c)).collect::<Html>()
        ));
    }
    lines.push(&guide());
    html!(
        "<div class=\"decide-block decide-wide\" id=\"decide-missing\"><h3 class=\"decide-title\">What is missing \
         <span class=\"text-normal\">· packages as on master</span></h3><ul class=\"decide-list\">{lines}</ul></div>"
    )
}

fn blockers_html(list: &[Blocker]) -> Html {
    if list.is_empty() {
        return html!(
            "<div class=\"Box-row color-fg-success\" id=\"decide-blockers\">{} No blocker found.</div>",
            icon(octicon!("check-circle-fill"))
        );
    }
    let items: Html = list
        .iter()
        .map(|b| match b.level() {
            Level::Block => html!(
                "<li><span class=\"color-fg-danger\">{}</span> {}</li>",
                icon(octicon!("x-circle-fill")),
                b.html()
            ),
            Level::Warn => html!(
                "<li><span class=\"color-fg-attention\">{}</span> {}</li>",
                icon(octicon!("dot-fill")),
                b.html()
            ),
        })
        .collect();
    html!(
        "<div class=\"Box-row\" id=\"decide-blockers\"><h3 class=\"decide-title\">Blockers</h3><ul class=\"decide-list\">{items}</ul></div>"
    )
}

fn actions_html(c: &Ctx) -> Html {
    if !c.pr.is_open() {
        return Html::new();
    }
    let review = format!("{}/files", github_url(c.pr.number));
    html!(
        "<div class=\"Box-footer d-flex flex-items-center flex-wrap gap-2\" id=\"decide-actions\">\
         <a class=\"btn btn-sm btn-primary\" href=\"{review}\" title=\"opens the diff on GitHub; choose Review changes, then Approve\">{} Approve on GitHub</a>\
         <a class=\"btn btn-sm\" href=\"{review}\" title=\"opens the diff on GitHub; choose Review changes, then Request changes\">{} Request changes on GitHub</a>\
         {}</div>",
        icon(octicon!("check-circle-fill")),
        icon(octicon!("file-diff")),
        c.extra_actions
    )
}

/// The panel's facts that do not depend on who asks, for the API.
pub(crate) fn summary(pr: &Pr, body: Option<&str>, files: Option<&[File]>) -> serde_json::Value {
    let rebuilds = rebuilds(&pr.labels);
    let mut text = pr.title.clone();
    if let Some(b) = body {
        text.push('\n');
        text.push_str(b);
    }
    serde_json::json!({
        "kind": kind(pr, files).key(),
        "bump": bump(&pr.title).map(|b| serde_json::json!({ "name": b.name, "from": b.from, "to": b.to })),
        "rebuilds": rebuilds.iter().map(|r| serde_json::json!({ "platform": r.platform, "count": r.count })).collect::<Vec<_>>(),
        "mass_rebuild": rebuilds.iter().any(Rebuild::mass),
        "cves": cves(&text),
        "changes_requested": changes_requested(pr),
    })
}

pub(crate) fn render(c: &Ctx) -> Html {
    let kind = kind(c.pr, c.files.ok());
    let b = bump(&c.pr.title);
    let rebuilds = rebuilds(&c.pr.labels);
    let mut out = Html::from(
        "<section class=\"Box mb-4\" id=\"decide\" aria-labelledby=\"decide-heading\">\
         <div class=\"Box-header\"><h2 class=\"Box-title\" id=\"decide-heading\">At a glance</h2></div>",
    );
    match c.reached.as_ref().filter(|_| c.pr.merged_at.is_some()) {
        Some(r) => out.push(&html!(
            "<div class=\"Box-row\" id=\"decide-reached\"><h3 class=\"decide-title\">Reached</h3>{r}</div>"
        )),
        None => out.push(&blockers_html(&blockers(c.pr, &rebuilds))),
    }
    out.push(&Html::from("<div class=\"decide-grid\">"));
    out.push(&change_block(c, kind, b.as_ref()));
    out.push(&risk_block(c.pr, &rebuilds));
    if let Some(b) = b.as_ref().filter(|_| kind == Kind::Update) {
        out.push(&upstream_block(c, b));
    }
    out.push(&builds_block(c));
    out.push(&people_block(c));
    out.push(&security_block(c));
    if c.pr.is_open() {
        out.push(&missing_block(&checks(c, kind)));
    }
    out.push(&c.queue);
    out.push(&Html::from("</div>"));
    out.push(&actions_html(c));
    out.push(&Html::from(
        "<div class=\"Box-footer f6 color-fg-muted\">Maintainer Mode writes nothing to nixpkgs. You approve or ask for changes on GitHub.</div></section>",
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::tests::{pr, review};

    fn file(path: &str, status: &str, patch: &str) -> File {
        File {
            path: path.into(),
            previous: None,
            status: status.into(),
            additions: patch.lines().filter(|l| l.starts_with('+')).count() as u64,
            deletions: patch.lines().filter(|l| l.starts_with('-')).count() as u64,
            patch: Some(patch.into()),
        }
    }

    #[test]
    fn a_title_gives_the_version_bump() {
        assert_eq!(
            bump("hello: 2.12 -> 2.13"),
            Some(Bump {
                name: "hello".into(),
                from: "2.12".into(),
                to: "2.13".into()
            })
        );
        let b =
            bump("[Backport release-25.05] python3Packages.foo: 1.0.0 → 1.1.0 (security)").unwrap();
        assert_eq!(
            (b.name.as_str(), b.from.as_str(), b.to.as_str()),
            ("python3Packages.foo", "1.0.0", "1.1.0")
        );
        assert_eq!(
            bump("curl: 8.9.1 -> 8.10.0, fixes CVE-2024-1"),
            Some(Bump {
                name: "curl".into(),
                from: "8.9.1".into(),
                to: "8.10.0".into()
            })
        );
        assert_eq!(bump("hello: init at 2.12"), None);
        assert_eq!(bump("hello: 1 -> 1"), None);
        assert_eq!(
            bump("treewide: a -> b ->"),
            Some(Bump {
                name: "treewide".into(),
                from: "a".into(),
                to: "b".into()
            })
        );
    }

    #[test]
    fn the_kind_comes_from_the_title_the_labels_and_the_files() {
        let mut p = pr(1, "alice", 0);
        let k = |p: &Pr, f: Option<&[File]>| kind(p, f);
        p.title = "hello: init at 2.12".into();
        assert_eq!(k(&p, None), Kind::New);
        p.title = "hello: 2.12 -> 2.13".into();
        assert_eq!(k(&p, None), Kind::Update);
        p.title = "hello: remove".into();
        assert_eq!(k(&p, None), Kind::Removal);
        p.title = "hello: tidy".into();
        let gone = [file("pkgs/by-name/he/hello/package.nix", "removed", "-x")];
        assert_eq!(k(&p, Some(&gone)), Kind::Removal);
        p.title = "nixos/nginx: add an option".into();
        assert_eq!(k(&p, None), Kind::Module);
        p.title = "hello: fix the build on darwin".into();
        assert_eq!(k(&p, None), Kind::Fix);
        p.title = "treewide: format".into();
        assert_eq!(k(&p, None), Kind::Refactor);
        p.title = "hello: tidy".into();
        assert_eq!(k(&p, None), Kind::Other);
        p.labels = vec!["8.has: package (new)".into()];
        assert_eq!(k(&p, None), Kind::New);
    }

    #[test]
    fn rebuild_labels_give_counts_per_platform() {
        let labels: Vec<String> = [
            "10.rebuild-linux: 1001-2500",
            "10.rebuild-darwin: 11-100",
            "10.rebuild-darwin-stdenv",
            "6.topic: python",
        ]
        .map(String::from)
        .to_vec();
        let r = rebuilds(&labels);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].platform, "darwin");
        assert_eq!(r[0].min, 11);
        assert!(!r[0].large());
        assert!(r[1].mass(), "a stdenv label is a mass rebuild");
        assert_eq!(r[2].min, 1001);
        assert!(r[2].mass());
        let mid = rebuilds(&["10.rebuild-linux: 501-1000".to_string()]);
        assert!(mid[0].large() && !mid[0].mass());
        let old = rebuilds(&["10.rebuild-linux: 501+".to_string()]);
        assert!(old[0].large());
        assert_eq!(rebuilds(&["10.rebuild-linux: 0".to_string()])[0].min, 0);
    }

    #[test]
    fn cve_ids_are_found_once_and_in_upper_case() {
        assert_eq!(
            cves(
                "fixes cve-2024-12345 and CVE-2023-0001; again CVE-2024-12345. not XCVE-2024-1111 nor CVE-24-1111 nor CVE-2024-12"
            ),
            ["CVE-2024-12345", "CVE-2023-0001"]
        );
        assert!(cves("CVE-").is_empty());
        assert!(cves("CVE-2024").is_empty());
    }

    #[test]
    fn a_major_version_is_the_first_number_or_the_second_after_zero() {
        assert!(major("1.9", "2.0"));
        assert!(!major("1.9", "1.10"));
        assert!(major("0.3.1", "0.4.0"));
        assert!(!major("0.3.1", "0.3.2"));
        assert!(!major("unstable", "x"));
    }

    #[test]
    fn blockers_list_the_hard_stops_first() {
        let mut p = pr(1, "alice", 0);
        assert!(blockers(&p, &[]).is_empty());
        p.ci = Some("PENDING".into());
        p.is_draft = true;
        p.mergeable = "CONFLICTING".into();
        let mut r = review(1, "bob", 1);
        r.state = "CHANGES_REQUESTED".into();
        let mut ok = review(2, "carol", 1);
        ok.state = "CHANGES_REQUESTED".into();
        let mut later = review(3, "carol", 2);
        later.state = "APPROVED".into();
        p.reviews = vec![r, ok, later];
        let r = rebuilds(&["10.rebuild-linux: 2501-5000".to_string()]);
        let b = blockers(&p, &r);
        let html: Vec<Html> = b.iter().map(Blocker::html).collect();
        let text: Vec<&str> = html.iter().map(Html::as_str).collect();
        assert!(text[0].contains("draft"), "{text:?}");
        assert!(text.iter().any(|t| t.contains("conflict")));
        assert!(
            text.iter()
                .any(|t| t.contains("Changes requested by <a href=\"/u/bob\">bob</a>."))
        );
        assert!(!text.iter().any(|t| t.contains("carol")));
        assert!(text.iter().any(|t| t.contains("mass rebuild")));
        assert_eq!(b.last().unwrap().level(), Level::Warn);
        assert_eq!(
            serde_json::json!(b[0]),
            serde_json::json!({ "kind": "draft", "level": "block", "text": "It is a draft." })
        );
        assert!(b.iter().any(|x| x.text() == "Changes requested by bob."));
        assert!(text.last().unwrap().contains("pending"));
        p.base = "staging".into();
        assert!(
            !blockers(&p, &r)
                .iter()
                .any(|b| matches!(b, Blocker::MassRebuild { .. }))
        );
        p.base = "my-branch".into();
        assert!(
            blockers(&p, &[])
                .iter()
                .any(|b| matches!(b, Blocker::UnusualBase { .. }))
        );
        for base in [
            "release-25.05",
            "staging-next",
            "staging-25.05",
            "haskell-updates",
            "staging-nixos",
        ] {
            assert!(usual_base(base), "{base}");
        }
        p.state = "MERGED".into();
        assert_eq!(blockers(&p, &r).len(), 1);
    }

    #[test]
    fn the_diff_checks_read_only_added_lines() {
        let f = [file(
            "pkgs/by-name/he/hello/package.nix",
            "added",
            "@@ -0,0 +1,9 @@\n+  src = fetchurl { sha256 = \"0abc\"; };\n+  cargoHash = lib.fakeHash;\n-  description = \"x\";\n+  meta.homepage = \"https://x\";",
        )];
        let c = diff_checks(Kind::New, &f);
        let failed: Vec<&str> = c
            .iter()
            .filter(|c| c.ok == Some(false))
            .map(|c| c.text.as_str())
            .collect();
        assert!(failed.iter().any(|t| t.contains("fakeHash")));
        assert!(failed.iter().any(|t| t.contains("old form")));
        assert!(
            failed.iter().any(|t| t.contains("meta.description")),
            "a removed line does not count"
        );
        assert!(failed.iter().any(|t| t.contains("versionCheckHook")));
        assert!(!failed.iter().any(|t| t.contains("meta.homepage")));
        let good = [file(
            "pkgs/by-name/he/hello/package.nix",
            "modified",
            "+  hash = \"sha256-AAAA\";\n+  nativeInstallCheckInputs = [ versionCheckHook ];\n+  passthru.tests = { };",
        )];
        let c = diff_checks(Kind::Update, &good);
        assert!(c.iter().all(|c| c.ok == Some(true)), "{c:?}");
    }

    #[test]
    fn a_package_link_is_a_github_repository() {
        assert_eq!(
            github_repo("https://github.com/curl/curl/archive/refs/tags/curl-8_10_0.tar.gz")
                .as_deref(),
            Some("curl/curl")
        );
        assert_eq!(
            github_repo("https://gitlab.com/a/b/-/archive/v1/b.tar.gz"),
            None
        );
        assert_eq!(github_repo("https://example.org/x.tar.gz"), None);
    }

    fn sample_ctx_parts() -> (Pr, Index, Health, Vec<Touched>) {
        let mut p = pr(7, "alice", 0);
        p.title = "curl: 8.9.1 -> 9.0.0".into();
        p.labels = vec![
            "10.rebuild-linux: 1001-2500".into(),
            "1.severity: security".into(),
        ];
        p.ci = Some("FAILURE".into());
        let mut index = Index::default();
        index.pkgs.insert(
            "curl".into(),
            crate::area::Package {
                pname: "curl".into(),
                version: "8.9.1".into(),
                maintainers: vec!["bob".into()],
                facts: Some(crate::attrs::Facts {
                    homepage: true,
                    license: true,
                    src_url: Some(
                        "https://github.com/curl/curl/archive/refs/tags/curl-8_9_1.tar.gz".into(),
                    ),
                    ..Default::default()
                }),
                meta: Some(crate::attrs::Meta {
                    changelog: vec!["https://curl.se/ch/8.9.1.html".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let mut h = Health::default();
        h.advisories.insert(
            "curl".into(),
            vec![crate::health::Advisory {
                code: "NIXPKGS-2026-1".into(),
                cves: vec!["CVE-2026-1111".into()],
                title: "x".into(),
                issue: None,
                ranges: vec![],
            }],
        );
        let touched = vec![Touched {
            subject: Subject::Pkg("curl".into()),
            flags: crate::catalog::Flags::of(
                &Default::default(),
                &index,
                &h,
                &Subject::Pkg("curl".into()),
            ),
            maintainers: vec!["bob".into()],
            teams: vec![],
            approved_by: vec![],
        }];
        (p, index, h, touched)
    }

    #[test]
    fn the_security_block_says_what_nixpkgs_already_fixes() {
        let (mut p, _, _, _) = sample_ctx_parts();
        p.title = "unbound: 1.26.1 -> 1.26.2".into();
        p.labels.clear();
        let (index, h, _, _) = crate::catalog::tests::unbound("1.26.1");
        let touched = vec![Touched {
            subject: Subject::Pkg("unbound".into()),
            flags: crate::catalog::Flags::of(
                &Default::default(),
                &index,
                &h,
                &Subject::Pkg("unbound".into()),
            ),
            maintainers: vec![],
            teams: vec![],
            approved_by: vec![],
        }];
        let committers = Committers::default();
        let author = crate::profile::Facts::default();
        let files: Vec<File> = vec![];
        let mut c = Ctx {
            pr: &p,
            body: Some("Update."),
            files: Ok(&files),
            touched: &touched,
            index: &index,
            health: &h,
            committers: &committers,
            author: &author,
            hydra: &[],
            notes: None,
            mine: None,
            extra_actions: Html::new(),
            reached: None,
            queue: Html::new(),
        };
        assert_eq!(security_block(&c).as_str(), "");
        let body = "Fixes CVE-2026-26001 again.";
        c.body = Some(body);
        let html = security_block(&c);
        assert!(
            html.contains("fixed in 1.26.1, the tracker issue can be closed"),
            "{html}"
        );
        let mut labelled = p.clone();
        labelled.labels = vec!["1.severity: security".into()];
        c.pr = &labelled;
        c.body = Some("Update.");
        let html = security_block(&c);
        assert!(html.contains("Already fixed"), "{html}");
        assert!(!html.contains("Not named"), "{html}");
    }

    #[test]
    fn the_panel_shows_its_blocks_in_order_and_nothing_private_to_a_visitor() {
        let (p, index, h, touched) = sample_ctx_parts();
        let committers = Committers::default();
        let author = crate::profile::Facts::default();
        let files: Vec<File> = vec![];
        let hydra = vec![("curl".to_string(), None)];
        let mine: BTreeSet<String> = ["curl".to_string()].into();
        let visitor = Ctx {
            pr: &p,
            body: Some("Fixes CVE-2026-1111."),
            files: Ok(&files),
            touched: &touched,
            index: &index,
            health: &h,
            committers: &committers,
            author: &author,
            hydra: &hydra,
            notes: None,
            mine: None,
            extra_actions: Html::new(),
            reached: None,
            queue: Html::new(),
        };
        let html = render(&visitor);
        let order = [
            "id=\"decide-blockers\"",
            "id=\"decide-change\"",
            "id=\"decide-risk\"",
            "id=\"decide-upstream\"",
            "id=\"decide-builds\"",
            "id=\"decide-people\"",
            "id=\"decide-security\"",
            "id=\"decide-missing\"",
            "id=\"decide-actions\"",
        ];
        let at: Vec<usize> = order
            .iter()
            .map(|o| {
                html.find(o)
                    .unwrap_or_else(|| panic!("{o} missing in {html}"))
            })
            .collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}");
        assert!(html.contains("<code>8.9.1</code> → <code>9.0.0</code>"));
        assert!(html.contains("Checks fail."));
        assert!(html.contains("mass rebuild"));
        assert!(html.contains("href=\"https://curl.se/ch/9.0.0.html\""));
        assert!(html.contains("href=\"https://github.com/curl/curl/releases\""));
        assert!(html.contains("major version"));
        assert!(html.contains("NIXPKGS-2026-1"));
        assert!(html.contains("unknown: no page read it in the last hour"));
        assert!(html.contains("<a href=\"/u/bob\">bob</a>"));
        assert!(html.contains("no meta.platforms") || html.contains("no meta.description"));
        let missing = &html[html.find("id=\"decide-missing\"").unwrap()..];
        let failed = missing.find("no meta.platforms").unwrap();
        let passed = missing.find("pass</summary>").unwrap();
        assert!(
            failed < passed,
            "failed checks come first, passed ones fold"
        );
        assert!(missing[passed..].contains("The title names what it changes."));
        for private in ["on your machines", "Next"] {
            assert!(!html.contains(private), "{private} shows to a visitor");
        }

        let signed_in = Ctx {
            mine: Some(&mine),
            extra_actions: "<a id=\"next\">Next</a>".into(),
            reached: None,
            queue: Html::new(),
            ..visitor
        };
        let html = render(&signed_in);
        assert!(html.contains("on your machines: curl"));
        let row = &html[html.find("id=\"decide-actions\"").unwrap()..];
        assert!(row.contains("id=\"next\""));
    }

    #[test]
    fn a_closed_pull_request_has_no_actions_and_a_plain_one_no_security_block() {
        let (mut p, index, h, _) = sample_ctx_parts();
        p.state = "CLOSED".into();
        p.labels.clear();
        p.title = "hello: tidy".into();
        let committers = Committers::default();
        let author = crate::profile::Facts::default();
        let c = Ctx {
            pr: &p,
            body: None,
            files: Err("this server reads GitHub only for people who sign in"),
            touched: &[],
            index: &index,
            health: &h,
            committers: &committers,
            author: &author,
            hydra: &[],
            notes: None,
            mine: None,
            extra_actions: Html::new(),
            reached: None,
            queue: Html::new(),
        };
        let html = render(&c);
        assert!(html.contains("It is closed."));
        assert!(!html.contains("decide-actions"));
        assert!(!html.contains("decide-security"));
        assert!(!html.contains("decide-upstream"));
        assert!(html.contains("unknown: no 10.rebuild label yet"));
        assert!(
            !html.contains("decide-missing"),
            "a closed pull request needs no checks"
        );
    }

    #[test]
    fn the_panel_marks_a_treewide_pull_request_once_and_a_too_large_one_in_touches() {
        let (mut p, index, h, _) = sample_ctx_parts();
        p.labels.clear();
        p.title = "treewide: use hash".into();
        p.treewide = crate::links::Treewide::of(340, 0);
        let touched: Vec<Touched> = (0..50)
            .map(|i| {
                let subject = Subject::Pkg(format!("p{i}"));
                Touched {
                    flags: crate::catalog::Flags::of(&Default::default(), &index, &h, &subject),
                    subject,
                    maintainers: vec![],
                    teams: vec![],
                    approved_by: vec![],
                }
            })
            .collect();
        let mut plain = p.clone();
        plain.treewide = None;
        let mut huge = plain.clone();
        let committers = Committers::default();
        let author = crate::profile::Facts::default();
        let c = Ctx {
            pr: &p,
            body: None,
            files: Err("not read"),
            touched: &touched,
            index: &index,
            health: &h,
            committers: &committers,
            author: &author,
            hydra: &[],
            notes: None,
            mine: None,
            extra_actions: Html::new(),
            reached: None,
            queue: Html::new(),
        };
        let html = render(&c).into_string();
        assert_eq!(html.matches("treewide · 340 packages").count(), 1);
        assert!(html.contains(">334 more</summary>"), "{html}");
        assert!(html.contains("p49</a> and 290 more</details>"));
        let c = Ctx { pr: &plain, ..c };
        assert!(render(&c).as_str().contains(">44 more</summary>"));
        huge.changed_files = 19_485;
        let c = Ctx {
            pr: &huge,
            touched: &[],
            ..c
        };
        assert!(render(&c).as_str().contains(
            "<span class=\"decide-label\">Touches</span> <span class=\"color-fg-attention\">none: it changes 19,485 files, too many to link to packages and people</span>"
        ));
    }
}
