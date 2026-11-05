//! The newest release from upstream itself: the git tags of the repository
//! that `src` comes from, or the registry that publishes the package. This
//! corrects repology, which reports what other distributions ship.

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::health::{Health, Run};
use crate::version;

/// Requests a pass may make: about 1,800 an hour at most.
const BUDGET: usize = 300;
const PAUSE: Duration = Duration::from_millis(100);
/// How long an answer holds for a package someone maintains or watches, and
/// for the rest.
const FRESH_FIRST: i64 = 1;
const FRESH_REST: i64 = 7;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// crates.io, PyPI, npm, Hackage, GNOME or GNU: the publisher lists
    /// the releases.
    Registry {
        ecosystem: &'static str,
        name: String,
    },
    /// A git remote, asked for its tags over smart HTTP: GitHub, GitLab,
    /// Codeberg, sourcehut, KDE and freedesktop alike, and any host whose
    /// `src` URL has the shape of a forge, with no token.
    Git(String),
}

/// What a pass found for one package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Direct {
    /// `None`: the source did not answer, or its tags do not reach the
    /// version nixpkgs has (the newest tag is older, or the oldest tag is
    /// newer), which means the tags were not read right.
    pub latest: Option<String>,
    /// `tags`, `crates.io`, `PyPI` or `npm`.
    pub from: String,
    /// Failures in a row: each one doubles the wait before the next try.
    #[serde(default)]
    pub failures: u32,
    pub checked_at: DateTime<Utc>,
    /// When `latest` was first seen.
    pub seen_at: DateTime<Utc>,
}

fn registry(src_url: &str) -> Option<Origin> {
    let url = src_url
        .strip_prefix("https://")
        .or(src_url.strip_prefix("http://"))
        .or(src_url.strip_prefix("mirror://"))?;
    let (host, path) = url.split_once('/')?;
    let parts: Vec<&str> = path.split('/').collect();
    let valid = |s: &&str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.@".contains(c))
    };
    // `hlint-3.10.tar.gz` names hlint: the name ends before `-<digit>`
    let stem = |file: &str| -> Option<String> {
        let file = file.split_once(".tar").map_or(file, |(s, _)| s);
        let at = file
            .match_indices('-')
            .find(|(i, _)| file[i + 1..].starts_with(|c: char| c.is_ascii_digit()))
            .map(|(i, _)| i)?;
        Some(file[..at].to_string())
    };
    let (ecosystem, name) =
        match (host, parts.as_slice()) {
            ("gnome", ["sources", n, ..]) | ("download.gnome.org", ["sources", n, ..]) => {
                ("GNOME", n.to_string())
            }
            ("gnu", [n, ..]) | ("ftpmirror.gnu.org", [n, ..]) | ("ftp.gnu.org", ["gnu", n, ..]) => {
                ("GNU", n.to_string())
            }
            ("hackage", [file]) => ("Hackage", stem(file)?),
            ("hackage.haskell.org", ["package", _, file]) => ("Hackage", stem(file)?),
            // `packages/<source or python tag>/<first letter>/<name>/<file>`
            (
                "files.pythonhosted.org" | "pypi.io" | "pypi.python.org",
                ["packages", _, l, n, ..],
            ) if l.len() == 1 => ("PyPI", n.to_string()),
            ("pypi", [l, n, ..]) if l.len() == 1 => ("PyPI", n.to_string()),
            ("crates.io", ["api", "v1", "crates", n, ..])
            | ("static.crates.io", ["crates", n, ..]) => ("crates.io", n.to_string()),
            ("registry.npmjs.org", [n, "-", ..]) => ("npm", n.to_string()),
            ("registry.npmjs.org", [scope, n, "-", ..]) if scope.starts_with('@') => {
                ("npm", format!("{scope}/{n}"))
            }
            _ => return None,
        };
    name.split('/')
        .all(|p| valid(&p))
        .then_some(Origin::Registry { ecosystem, name })
}

/// `%2F` and the like, in the project slug of a GitLab API URL.
fn unescape(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hex = [bytes.next()?, bytes.next()?];
            out.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            out.push(b);
        }
    }
    String::from_utf8(out).ok()
}

/// The repository or registry behind a `src` URL. nixpkgs' fetchFromGitHub
/// gives `https://github.com/owner/repo/archive/refs/tags/v1.2.3.tar.gz`, and
/// fetchFromGitLab `https://gitlab.com/api/v4/projects/owner%2Frepo/repository/archive.tar.gz?sha=v1.2.3`.
/// On a known forge any path names the repository; elsewhere the path
/// must have the shape of one (a GitLab API or `/-/archive/` URL, a Gitea
/// `/archive/` URL, or a `.git` remote), so a plain download site is not
/// asked for tags.
pub fn origin(src_url: &str) -> Option<Origin> {
    if let Some(r) = registry(src_url) {
        return Some(r);
    }
    let url = src_url
        .strip_prefix("https://")
        .or(src_url.strip_prefix("http://"))?;
    let (host, path) = url.split_once('/')?;
    let known = host == "github.com"
        || host == "codeberg.org"
        || host.starts_with("gitlab.")
        || host == "git.sr.ht"
        || host == "invent.kde.org";
    // sourcehut's Mercurial side speaks no git
    if host == "hg.sr.ht" {
        return None;
    }
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let slug = match path
        .strip_prefix("api/v4/projects/")
        .and_then(|p| p.split_once('/'))
    {
        Some((slug, _)) => unescape(slug)?,
        // `/-/archive/` before `/archive/`, or a GitLab repository keeps a `/-`
        None => {
            let cut = path
                .split_once("/-/archive/")
                .or_else(|| path.split_once("/-/releases/"))
                .or_else(|| path.split_once("/archive/"))
                .or_else(|| path.split_once("/releases/"))
                .or_else(|| path.split_once("/repository/"))
                .map(|(repo, _)| repo);
            match cut {
                Some(repo) => repo.to_string(),
                None if known || path.trim_end_matches('/').ends_with(".git") => path.to_string(),
                None => return None,
            }
        }
    };
    let repo = slug.trim_end_matches('/').trim_end_matches(".git");
    let segment = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.~+".contains(c))
    };
    if repo.is_empty() || !repo.contains('/') || !repo.split('/').all(segment) {
        return None;
    }
    Some(Origin::Git(format!("https://{host}/{repo}")))
}

/// nix-update's list of prerelease words.
const PRERELEASE: [&str; 9] = [
    "alpha",
    "beta",
    "canary",
    "nightly",
    "prerelease",
    "preview",
    "rc",
    "snapshot",
    "dev",
];

/// Prefixes that say "release" in any repository.
const RELEASE: [&str; 3] = ["release-", "release/", "REL_"];

/// A tag that is a version and nothing else: `1.2.3`, `v1.2.3` or
/// `release/v1.2.3`. When a repository has such tags, a `word-1.2` tag
/// names something else (`hello_1.39` next to `v0.12.0`).
fn plain(tag: &str) -> bool {
    let tag = tag.trim();
    let tag = tag.strip_suffix("^{}").unwrap_or(tag);
    let tag = RELEASE
        .iter()
        .find_map(|p| tag.strip_prefix(p))
        .unwrap_or(tag);
    tag.strip_prefix(['v', 'V'])
        .unwrap_or(tag)
        .starts_with(|c: char| c.is_ascii_digit())
}

/// The version a tag names, or `None` when it names none: tags carry
/// prefixes (`v1.2.3`, `release-1.2.3`, `openssl-3.5.0`), and some are not
/// releases at all.
pub fn version_of(tag: &str, pname: &str) -> Option<String> {
    let tag = tag.trim();
    let tag = tag.strip_suffix("^{}").unwrap_or(tag);
    let mut rest = tag;
    let named = if pname.is_empty() {
        vec![]
    } else {
        vec![
            format!("{pname}-"),
            format!("{pname}_"),
            format!("{pname}/"),
        ]
    };
    for prefix in named.iter().map(String::as_str).chain(RELEASE) {
        if let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped;
            break;
        }
    }
    // `v1.2.3`, and `release/v1.2.3` or `foo-v1.2.3` alike
    rest = rest.strip_prefix(['v', 'V']).unwrap_or(rest);
    // `someone/fix-a-thing-3-05.02.2026` is a branch kept as a tag
    if rest.contains('/') {
        return None;
    }
    // `ghostpdl-10.07.1` in the ghostscript repository: cut at the first
    // digit after a `-` or `_`, but not `libx11-1.8` into `11-1.8`
    if !rest.starts_with(|c: char| c.is_ascii_digit()) {
        match rest.char_indices().find(|(_, c)| c.is_ascii_digit()) {
            Some((at, _)) if at > 0 && matches!(rest.as_bytes()[at - 1], b'-' | b'_') => {
                rest = &rest[at..];
            }
            _ => return None,
        }
    }
    let lower = rest.to_ascii_lowercase();
    if PRERELEASE.iter().any(|p| lower.contains(p)) {
        return None;
    }
    if !rest
        .chars()
        .all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c == '_')
    {
        return None;
    }
    // a five-digit first part is a date or a commit counter
    if scale(rest).is_none_or(|n| n > 4) {
        return None;
    }
    Some(rest.to_string())
}

/// The number of digits of the first number: 2024.1 and 1.2 are two
/// numbering schemes.
fn scale(v: &str) -> Option<usize> {
    v.split(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())
        .map(str::len)
}

/// The leading numbers of a version: `1.18.8` gives 1, 18, 8; `3.12.15b`
/// gives 3, 12, 15.
fn numbers(v: &str) -> Vec<u64> {
    version::chunks(v)
        .iter()
        .map_while(|c| c.parse().ok())
        .collect()
}

/// The series a package tracks when nixpkgs carries a newer series of the
/// same source under another attribute: `python312` 3.12.15 next to
/// `python313` tracks 3.12, `wesnoth` 1.18.8 next to `wesnoth-devel`
/// 1.19.28 tracks 1.18, `nodejs_20` next to `nodejs_22` tracks 20. The
/// series is the version up to the first number where the newer sibling
/// differs; a sibling that differs in the last number only is a newer
/// patch of the same series and says nothing. With no newer sibling: none.
pub fn series<'a>(version: &str, siblings: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let ours = numbers(version);
    if ours.len() < 2 {
        return None;
    }
    let mut depth: Option<usize> = None;
    for s in siblings {
        if version::compare(s, version) != Ordering::Greater {
            continue;
        }
        let theirs = numbers(s);
        let k = ours
            .iter()
            .zip(&theirs)
            .position(|(a, b)| a != b)
            .unwrap_or(ours.len().min(theirs.len()));
        if k + 1 < ours.len() {
            depth = Some(depth.map_or(k, |d| d.min(k)));
        }
    }
    depth.map(|k| {
        ours[..=k]
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".")
    })
}

/// Whether a version is on a series: its leading numbers start with it.
pub fn in_series(v: &str, series: &str) -> bool {
    let s = numbers(series);
    let n = numbers(v);
    n.len() >= s.len() && n[..s.len()] == s[..]
}

/// attribute → the series it tracks, for every package that nixpkgs carries
/// next to a newer series from the same file.
pub fn series_map(index: &crate::area::Index) -> HashMap<String, String> {
    let mut by_file: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
    for (attr, p) in &index.pkgs {
        if let Some(f) = &p.file {
            if !p.version.is_empty() {
                by_file.entry(f).or_default().push((attr, &p.version));
            }
        }
    }
    let mut out = HashMap::new();
    for group in by_file.values() {
        if group.len() < 2 {
            continue;
        }
        for (attr, v) in group {
            let others = group.iter().filter(|(a, _)| a != attr).map(|(_, w)| *w);
            if let Some(s) = series(v, others) {
                out.insert(attr.to_string(), s);
            }
        }
    }
    out
}

/// nixpkgs has no release of it, only a snapshot: `0-unstable-2024-01-01`.
fn snapshot(current: &str) -> bool {
    current.starts_with("0-unstable") || current.starts_with("unstable-")
}

/// The newest tag on the same numbering scheme as the version nixpkgs has.
/// The tags must reach the version nixpkgs has: a repository whose oldest
/// tag is already newer than nixpkgs never tagged what nixpkgs ships, so
/// its tags count something else (`trunk-5.5` of an OS that bundles ldacbt
/// 2.0.72), and one whose newest tag is older was misread. A snapshot has
/// no release to reach, so any tag counts.
///
/// The tags that name the package are asked first (a repository that holds
/// several projects), then the bare versions, then all: the first pool
/// that reaches nixpkgs answers.
pub fn newest(tags: &[String], pname: &str, current: &str, series: Option<&str>) -> Option<String> {
    let named = |t: &&String| {
        !pname.is_empty()
            && ["-", "_", "/"]
                .iter()
                .any(|s| t.starts_with(&format!("{pname}{s}")))
    };
    let pools: [Vec<&String>; 3] = [
        tags.iter().filter(named).collect(),
        tags.iter().filter(|t| plain(t)).collect(),
        tags.iter().collect(),
    ];
    pools
        .into_iter()
        .find_map(|pool| newest_of(&pool, pname, current, series))
}

fn newest_of(pool: &[&String], pname: &str, current: &str, series: Option<&str>) -> Option<String> {
    let ours = scale(current);
    let versions: Vec<String> = pool
        .iter()
        .filter_map(|tag| version_of(tag, pname))
        .filter_map(|c| dotted(c, current))
        .filter(|c| ours.is_none() || scale(c) == ours)
        .filter(|c| series.is_none_or(|s| in_series(c, s)))
        .collect();
    if !snapshot(current)
        && !versions
            .iter()
            .any(|v| version::compare(v, current) != Ordering::Greater)
    {
        return None;
    }
    versions
        .into_iter()
        .max_by(|a, b| version::compare(a, b))
        .filter(|v| version::compare(v, current) != Ordering::Less)
}

/// `curl-8_21_0` names 8.21.0, but `drm-5-0-0-20010105` is no libdrm 2.4:
/// when nixpkgs writes dots, a tag must have them or underscores in their
/// place.
fn dotted(candidate: String, current: &str) -> Option<String> {
    if !current.contains('.') || candidate.contains('.') {
        return Some(candidate);
    }
    candidate.contains('_').then(|| candidate.replace('_', "."))
}

/// Whether a version from elsewhere, like repology, is a newer release of
/// the same numbering scheme.
pub fn newer(candidate: &str, current: &str) -> bool {
    let lower = candidate.to_ascii_lowercase();
    !PRERELEASE.iter().any(|p| lower.contains(p))
        && (scale(current).is_none() || scale(candidate) == scale(current))
        && version::compare(candidate, current) == Ordering::Greater
}

/// One- and two-letter crate names get a numeric bucket, three letters
/// `3/<first>`, the rest two pairs of letters.
pub fn crates_path(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    match chars.len() {
        0 => lower,
        1 => format!("1/{lower}"),
        2 => format!("2/{lower}"),
        3 => format!("3/{}/{lower}", chars[0]),
        _ => format!("{}{}/{}{}/{lower}", chars[0], chars[1], chars[2], chars[3]),
    }
}

fn registry_url(ecosystem: &str, name: &str) -> Option<String> {
    match ecosystem {
        "crates.io" => Some(format!("https://index.crates.io/{}", crates_path(name))),
        "PyPI" => Some(format!("https://pypi.org/pypi/{name}/json")),
        "npm" => Some(format!(
            "https://registry.npmjs.org/-/package/{}/dist-tags",
            name.replace('/', "%2f")
        )),
        "Hackage" => Some(format!(
            "https://hackage.haskell.org/package/{name}/preferred.json"
        )),
        "GNOME" => Some(format!(
            "https://download.gnome.org/sources/{name}/cache.json"
        )),
        "GNU" => Some(format!("https://ftp.gnu.org/gnu/{name}/")),
        _ => None,
    }
}

/// The releases a registry lists, as it lists them: crates.io, PyPI and
/// npm name the newest themselves; Hackage and GNOME list every version;
/// GNU is a directory of tarballs, whose stems read like tags.
pub fn published_versions(ecosystem: &str, body: &str) -> Vec<String> {
    let json = || serde_json::from_str::<Value>(body).ok();
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    };
    match ecosystem {
        // one JSON object per line, oldest first; a yanked release is no target
        "crates.io" => body
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["yanked"].as_bool() != Some(true))
            .filter_map(|v| v["vers"].as_str().map(str::to_string))
            .collect(),
        // PyPI's own latest leaves out prereleases
        "PyPI" => json()
            .and_then(|v| v["info"]["version"].as_str().map(str::to_string))
            .into_iter()
            .collect(),
        "npm" => json()
            .and_then(|v| v["latest"].as_str().map(str::to_string))
            .into_iter()
            .collect(),
        "Hackage" => json()
            .map(|v| strings(&v["normal-version"]))
            .unwrap_or_default(),
        // `[format, {name: {version: {files}}}, {name: [versions]}, ...]`
        "GNOME" => json()
            .and_then(|v| v.get(2)?.as_object()?.values().next().map(strings))
            .unwrap_or_default(),
        "GNU" => body
            .split("href=\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .filter_map(|href| {
                let name = href.split(['?', '#']).next()?;
                // a tarball, or a directory of one release (gcc-15.1.0/)
                let stem = name.strip_suffix('/').or_else(|| {
                    [".tar.gz", ".tar.xz", ".tar.bz2", ".tar.lz", ".tgz", ".zip"]
                        .iter()
                        .find_map(|ext| name.strip_suffix(ext))
                })?;
                (!stem.is_empty() && !stem.contains('/') && !stem.starts_with('.'))
                    .then(|| stem.to_string())
            })
            .collect(),
        _ => Vec::new(),
    }
    .into_iter()
    .filter(|v| !v.is_empty())
    .collect()
}

/// GNOME's old numbering: below 40, an odd minor is a development release
/// (2.89.x before 2.90). A package whose nixpkgs version has an even minor
/// follows that policy; one on an odd minor (dconf 0.49.0) does not.
fn gnome_stable(current: &str, candidate: &str) -> bool {
    let minor = |v: &str| -> Option<(u64, u64)> {
        let mut it = v.split('.');
        Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
    };
    match (minor(current), minor(candidate)) {
        (Some((major, ours)), Some((_, theirs))) if major < 40 && ours % 2 == 0 => theirs % 2 == 0,
        _ => true,
    }
}

/// The newest release among what a registry lists. crates.io, PyPI and
/// npm declared it, so no scheme filter, but off the series it says
/// nothing about this package; Hackage, GNOME and GNU list everything, so
/// the same rules as for tags apply.
pub fn newest_published(
    ecosystem: &str,
    versions: &[String],
    name: &str,
    current: &str,
    series: Option<&str>,
) -> Option<String> {
    let on_series = |v: &String| series.is_none_or(|s| in_series(v, s));
    match ecosystem {
        "crates.io" => versions
            .iter()
            .filter(|v| version_of(v, "").is_some())
            .max_by(|a, b| version::compare(a, b))
            .filter(|v| on_series(v))
            .cloned(),
        "PyPI" | "npm" => versions.first().filter(|v| on_series(v)).cloned(),
        "GNOME" => {
            let stable: Vec<String> = versions
                .iter()
                .filter(|v| gnome_stable(current, v))
                .cloned()
                .collect();
            newest(&stable, name, current, series)
        }
        _ => newest(versions, name, current, series),
    }
}

/// `<4 hex length><payload>` lines; `0000` ends the list.
fn parse_refs(body: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while rest.len() >= 4 {
        let Some(len) = std::str::from_utf8(&rest[..4])
            .ok()
            .and_then(|s| usize::from_str_radix(s, 16).ok())
        else {
            break;
        };
        if len == 0 {
            break;
        }
        if len < 4 || len > rest.len() {
            rest = &rest[4.min(rest.len())..];
            continue;
        }
        let line = String::from_utf8_lossy(&rest[4..len]);
        if let Some(tag) = line
            .split_whitespace()
            .nth(1)
            .and_then(|r| r.strip_prefix("refs/tags/"))
        {
            out.push(tag.to_string());
        }
        rest = &rest[len..];
    }
    out
}

/// `git ls-remote --tags`, over protocol v2 with a single request.
async fn tags(http: &reqwest::Client, repo: &str) -> Result<Vec<String>> {
    const BODY: &[u8] = b"0014command=ls-refs\n0001001aref-prefix refs/tags/\n0000";
    // GitLab wants the `.git`, sourcehut does not take it
    let mut last = None;
    for url in [format!("{repo}.git"), repo.to_string()] {
        let resp = http
            .post(format!("{url}/git-upload-pack"))
            .header("Content-Type", "application/x-git-upload-pack-request")
            .header("Accept", "application/x-git-upload-pack-result")
            .header("Git-Protocol", "version=2")
            .body(BODY)
            .send()
            .await?;
        if resp.status().is_success() {
            return Ok(parse_refs(&resp.bytes().await?));
        }
        last = Some(resp.status());
    }
    bail!(
        "{repo}: {}",
        last.map(|s| s.to_string()).unwrap_or_default()
    )
}

async fn published(http: &reqwest::Client, ecosystem: &str, name: &str) -> Result<Vec<String>> {
    let Some(url) = registry_url(ecosystem, name) else {
        return Ok(Vec::new());
    };
    let body = http
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(published_versions(ecosystem, &body))
}

/// One package to ask about.
pub struct Candidate {
    pub attr: String,
    pub pname: String,
    pub version: String,
    pub origin: Origin,
    /// Someone maintains or watches it.
    pub first: bool,
    /// The series it tracks, when nixpkgs carries a newer one next to it.
    pub series: Option<String>,
}

/// When a package first gets `latest`. A first answer, or the first one
/// after a failure or an unclear answer, is not news, unless repology saw
/// it first.
fn seen_at(h: &Health, attr: &str, latest: &Option<String>, now: DateTime<Utc>) -> DateTime<Utc> {
    match h.direct.get(attr) {
        Some(d) if d.latest == *latest => d.seen_at,
        Some(d) if d.latest.is_some() => now,
        _ => latest
            .as_ref()
            .and_then(|v| h.upstream_since(attr, v))
            .unwrap_or(DateTime::UNIX_EPOCH),
    }
}

fn due(h: &Health, c: &Candidate) -> Option<(u8, DateTime<Utc>)> {
    let rank = u8::from(!c.first);
    let Some(d) = h.direct.get(&c.attr) else {
        return Some((rank, DateTime::UNIX_EPOCH));
    };
    let days = if d.failures > 0 {
        crate::health::backoff_days(d.failures)
    } else if c.first {
        FRESH_FIRST
    } else {
        FRESH_REST
    };
    (Utc::now() - d.checked_at > chrono::Duration::days(days)).then_some((rank, d.checked_at))
}

/// The packages someone maintains or watches first, then the ones never
/// asked, then the oldest answers. Several packages share a repository, so
/// a pass asks each remote once.
pub async fn pass(h: &mut Health, candidates: Vec<Candidate>) -> Run {
    let http = crate::health::http(Duration::from_secs(30));
    let total = candidates.len();
    if total > 0 {
        let live: HashSet<&str> = candidates.iter().map(|c| c.attr.as_str()).collect();
        h.direct.retain(|a, _| live.contains(a.as_str()));
    }
    let mut queue: Vec<(u8, DateTime<Utc>, Candidate)> = candidates
        .into_iter()
        .filter_map(|c| due(h, &c).map(|(rank, at)| (rank, at, c)))
        .collect();
    queue.sort_by(|a, b| (a.0, a.1, &a.2.attr).cmp(&(b.0, b.1, &b.2.attr)));
    let left = queue.len();
    let mut seen: HashMap<String, Result<Vec<String>, ()>> = HashMap::new();
    let mut registries: HashMap<String, Result<Vec<String>, ()>> = HashMap::new();
    let mut asked: HashSet<String> = HashSet::new();
    let (mut behind, mut current, mut unclear, mut failed, mut done) = (0, 0, 0, 0, 0);
    for (_, _, c) in queue {
        let key = match &c.origin {
            Origin::Git(repo) => repo.clone(),
            Origin::Registry { ecosystem, name } => format!("{ecosystem}:{name}"),
        };
        if !asked.contains(&key) && asked.len() >= BUDGET {
            break;
        }
        let (answer, from) = match &c.origin {
            Origin::Git(repo) => {
                if !seen.contains_key(repo) {
                    asked.insert(key.clone());
                    let got = tags(&http, repo).await.map_err(|e| {
                        tracing::debug!("{}: {e:#}", c.attr);
                    });
                    seen.insert(repo.clone(), got);
                    tokio::time::sleep(PAUSE).await;
                }
                match &seen[repo] {
                    Ok(t) => (
                        Ok(newest(t, &c.pname, &c.version, c.series.as_deref())),
                        "tags",
                    ),
                    Err(()) => (Err(()), "tags"),
                }
            }
            Origin::Registry { ecosystem, name } => {
                if !registries.contains_key(&key) {
                    asked.insert(key.clone());
                    let got = published(&http, ecosystem, name).await.map_err(|e| {
                        tracing::debug!("{}: {e:#}", c.attr);
                    });
                    registries.insert(key.clone(), got);
                    tokio::time::sleep(PAUSE).await;
                }
                let answer = match &registries[&key] {
                    Ok(v) => Ok(newest_published(
                        ecosystem,
                        v,
                        name,
                        &c.version,
                        c.series.as_deref(),
                    )),
                    Err(()) => Err(()),
                };
                (answer, *ecosystem)
            }
        };
        done += 1;
        let unreachable = answer.is_err();
        let latest = match answer {
            Err(()) => {
                failed += 1;
                None
            }
            Ok(None) => {
                unclear += 1;
                None
            }
            Ok(Some(v)) => match version::compare(&v, &c.version) {
                Ordering::Greater => {
                    behind += 1;
                    Some(v)
                }
                Ordering::Equal => {
                    current += 1;
                    Some(v)
                }
                // `newest` never answers this; kept for a registry that does
                Ordering::Less => {
                    unclear += 1;
                    None
                }
            },
        };
        if failed == done && done >= 10 {
            return Run::failed(format!("the first {done} requests failed"));
        }
        let now = Utc::now();
        if unreachable {
            if let Some(d) = h.direct.get_mut(&c.attr) {
                d.failures += 1;
                d.checked_at = now;
                continue;
            }
        }
        let seen_at = seen_at(h, &c.attr, &latest, now);
        h.direct.insert(
            c.attr.clone(),
            Direct {
                failures: u32::from(unreachable),
                latest,
                from: from.to_string(),
                checked_at: now,
                seen_at,
            },
        );
    }
    Run::ok(format!(
        "{} remotes and registries asked · {done} packages: {behind} behind, {current} current, {unclear} unclear, {failed} unreachable · {} of {total} still due",
        asked.len(),
        left - done
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fetch_url_names_its_repository() {
        assert_eq!(
            origin("https://github.com/curl/curl/archive/refs/tags/curl-8_21_0.tar.gz"),
            Some(Origin::Git("https://github.com/curl/curl".into()))
        );
        assert_eq!(
            origin("https://gitlab.freedesktop.org/mesa/mesa/-/archive/25.2.0/mesa-25.2.0.tar.gz"),
            Some(Origin::Git(
                "https://gitlab.freedesktop.org/mesa/mesa".into()
            ))
        );
        assert_eq!(
            origin("https://git.sr.ht/~sircmpwn/scdoc/archive/1.11.3.tar.gz"),
            Some(Origin::Git("https://git.sr.ht/~sircmpwn/scdoc".into()))
        );
        assert_eq!(
            origin("https://downloads.xiph.org/releases/flac/flac-1.5.0.tar.xz"),
            None
        );
        assert_eq!(origin("https://github.com/only-a-user"), None);
        // any host, when the URL has the shape of a forge
        assert_eq!(
            origin("https://gitea.nouspiro.space/nou/libXISF/archive/v0.2.14.tar.gz"),
            Some(Origin::Git(
                "https://gitea.nouspiro.space/nou/libXISF".into()
            ))
        );
        assert_eq!(
            origin("https://0xacab.org/leap/bitmask-vpn.git"),
            Some(Origin::Git("https://0xacab.org/leap/bitmask-vpn".into()))
        );
        assert_eq!(
            origin(
                "https://salsa.debian.org/api/v4/projects/clint%2Ffakeroot/repository/archive.tar.gz?sha=225d"
            ),
            Some(Origin::Git(
                "https://salsa.debian.org/clint/fakeroot".into()
            ))
        );
        assert_eq!(
            origin("https://github.com/freebsd/freebsd-src.git"),
            Some(Origin::Git("https://github.com/freebsd/freebsd-src".into()))
        );
        // a download site is no forge
        assert_eq!(
            origin("https://mupdf.com/downloads/archive/mupdf-1.28.5-source.tar.gz"),
            None
        );
        assert_eq!(
            origin("https://brltty.app/archive/brltty-6.9.1.tar.gz"),
            None
        );
        assert_eq!(
            origin("https://www.hepforge.org/archive/applgrid/applgrid-1.6.27.tgz"),
            None
        );
        assert_eq!(origin("https://example.org/owner/repo/file.tar.gz"), None);
        assert_eq!(
            origin("https://hg.sr.ht/~scoopta/glpaper/archive/abc.tar.gz"),
            None
        );
    }

    #[test]
    fn a_gitlab_api_url_names_its_project() {
        assert_eq!(
            origin(
                "https://gitlab.freedesktop.org/api/v4/projects/xorg%2Flib%2Flibx11/repository/archive.tar.gz?sha=libX11-1.8.12"
            ),
            Some(Origin::Git(
                "https://gitlab.freedesktop.org/xorg/lib/libx11".into()
            ))
        );
        assert_eq!(
            origin(
                "https://gitlab.com/api/v4/projects/inkscape%2Finkscape%2Ecom/repository/archive.tar.gz?sha=v1"
            ),
            Some(Origin::Git(
                "https://gitlab.com/inkscape/inkscape.com".into()
            ))
        );
        assert_eq!(
            origin("https://gitlab.com/api/v4/projects/12345/repository/archive.tar.gz"),
            None
        );
        assert_eq!(
            origin("https://gitlab.com/api/v4/projects/a%2/repository/archive.tar.gz"),
            None
        );
    }

    #[test]
    fn only_a_change_after_a_good_answer_is_news() {
        let now = Utc::now();
        let earlier = now - chrono::Duration::days(3);
        let mut h = Health::default();
        let entry = |latest: Option<&str>, failed| Direct {
            latest: latest.map(String::from),
            from: "tags".into(),
            failures: u32::from(failed),
            checked_at: earlier,
            seen_at: earlier,
        };
        let v2 = Some("2.0".to_string());
        assert_eq!(seen_at(&h, "a", &v2, now), DateTime::UNIX_EPOCH);
        h.direct.insert("a".into(), entry(None, true));
        assert_eq!(seen_at(&h, "a", &v2, now), DateTime::UNIX_EPOCH);
        h.direct.insert("a".into(), entry(None, false));
        assert_eq!(seen_at(&h, "a", &v2, now), DateTime::UNIX_EPOCH);
        h.direct.insert("a".into(), entry(Some("1.0"), false));
        assert_eq!(seen_at(&h, "a", &v2, now), now);
        h.direct.insert("a".into(), entry(Some("2.0"), false));
        assert_eq!(seen_at(&h, "a", &v2, now), earlier);
    }

    #[test]
    fn a_failing_origin_waits_longer_each_time() {
        use crate::health::backoff_days;
        assert_eq!(backoff_days(0), 1);
        assert_eq!(backoff_days(1), 1);
        assert_eq!(backoff_days(2), 2);
        assert_eq!(backoff_days(4), 8);
        assert_eq!(backoff_days(40), 32);
    }

    #[test]
    fn a_registry_url_names_its_package() {
        let r = |e, n: &str| {
            Some(Origin::Registry {
                ecosystem: e,
                name: n.into(),
            })
        };
        assert_eq!(
            origin("mirror://pypi/r/requests/requests-2.32.3.tar.gz"),
            r("PyPI", "requests")
        );
        assert_eq!(
            origin(
                "https://files.pythonhosted.org/packages/source/r/requests/requests-2.32.3.tar.gz"
            ),
            r("PyPI", "requests")
        );
        assert_eq!(
            origin(
                "https://files.pythonhosted.org/packages/ab/cd/0123456789/requests-2.32.3.tar.gz"
            ),
            None
        );
        assert_eq!(
            origin("https://crates.io/api/v1/crates/syn/2.0.68/download"),
            r("crates.io", "syn")
        );
        assert_eq!(
            origin("https://registry.npmjs.org/left-pad/-/left-pad-1.3.0.tgz"),
            r("npm", "left-pad")
        );
        assert_eq!(
            origin("https://registry.npmjs.org/@angular/cli/-/cli-17.0.0.tgz"),
            r("npm", "@angular/cli")
        );
        assert_eq!(
            origin("mirror://hackage/hlint-3.10.tar.gz"),
            r("Hackage", "hlint")
        );
        assert_eq!(
            origin("mirror://hackage/jacinda-3.3.0.6.tar.gz"),
            r("Hackage", "jacinda")
        );
        assert_eq!(
            origin("https://hackage.haskell.org/package/niv-0.2.22/niv-0.2.22.tar.gz"),
            r("Hackage", "niv")
        );
        assert_eq!(
            origin(
                "mirror://gnome/sources/gnome-control-center/50/gnome-control-center-50.4.tar.xz"
            ),
            r("GNOME", "gnome-control-center")
        );
        assert_eq!(origin("mirror://gnu/apl/apl-1.9.tar.gz"), r("GNU", "apl"));
        assert_eq!(
            origin("https://ftpmirror.gnu.org/glibc/glibc-2.44.tar.xz"),
            r("GNU", "glibc")
        );
        assert_eq!(
            origin("https://ftp.gnu.org/gnu/bash/bash-5.3.tar.gz"),
            r("GNU", "bash")
        );
    }

    #[test]
    fn a_tag_is_not_always_a_version() {
        assert_eq!(version_of("v1.2.3", "zlib").as_deref(), Some("1.2.3"));
        assert_eq!(version_of("zlib-1.3.1", "zlib").as_deref(), Some("1.3.1"));
        assert_eq!(version_of("zlib-v1.3.1", "zlib").as_deref(), Some("1.3.1"));
        assert_eq!(version_of("release/v4.3.0", "x").as_deref(), Some("4.3.0"));
        assert_eq!(
            version_of("ghostpdl-10.07.1", "ghostscript").as_deref(),
            Some("10.07.1")
        );
        assert_eq!(version_of("1.2.3^{}", "x").as_deref(), Some("1.2.3"));
        assert_eq!(version_of("v2.0.0-rc1", "x"), None);
        assert_eq!(version_of("nightly", "x"), None);
        assert_eq!(version_of("debian/1.2.3", "x"), None);
        assert_eq!(
            version_of("someone/fix-a-thing-3-05.02.2026", "kotlin"),
            None
        );
        assert_eq!(version_of("release/1.2.3", "x").as_deref(), Some("1.2.3"));
        assert_eq!(
            version_of("kotlin/1.2.3", "kotlin").as_deref(),
            Some("1.2.3")
        );
        assert_eq!(version_of("942850058", "libwebsockets"), None);
        assert_eq!(version_of("2024.1", "x").as_deref(), Some("2024.1"));
    }

    #[test]
    fn a_package_next_to_a_newer_series_tracks_its_own() {
        assert_eq!(series("1.18.8", ["1.19.28"]).as_deref(), Some("1.18"));
        assert_eq!(
            series("3.12.15", ["3.13.11", "3.14.2"]).as_deref(),
            Some("3.12")
        );
        assert_eq!(
            series("20.19.0", ["22.4.0", "24.1.0"]).as_deref(),
            Some("20")
        );
        // the newest series tracks nothing
        assert_eq!(series("1.19.28", ["1.18.8"]), None);
        // a sibling on a newer patch of the same series says nothing
        assert_eq!(series("1.2.3", ["1.2.5"]), None);
        // an alias with the same version says nothing
        assert_eq!(series("1.2.3", ["1.2.3"]), None);
        assert_eq!(series("1.18", ["1.19"]), None);
        assert_eq!(series("1.18", ["2.0"]).as_deref(), Some("1"));
        assert_eq!(series("7", ["8"]), None);
        assert_eq!(series("0-unstable-2024-01-01", ["1.0.0"]), None);
        assert!(in_series("1.18.9", "1.18"));
        assert!(in_series("1.18", "1.18"));
        assert!(!in_series("1.19.29", "1.18"));
        assert!(!in_series("1.180.0", "1.18"));
        assert!(!in_series("1", "1.18"));
        let tags: Vec<String> = ["1.18.8", "1.18.9", "1.19.29"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            newest(&tags, "wesnoth", "1.18.8", None).as_deref(),
            Some("1.19.29")
        );
        assert_eq!(
            newest(&tags, "wesnoth", "1.18.8", Some("1.18")).as_deref(),
            Some("1.18.9")
        );
        assert_eq!(
            newest(&tags, "wesnoth", "1.18.9", Some("1.18")).as_deref(),
            Some("1.18.9")
        );
    }

    #[test]
    fn the_newest_release_on_our_scheme_wins() {
        let tags: Vec<String> = ["v1.2.3", "v1.10.0", "v2.0.0-rc1", "2015-11-19-1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            newest(&tags, "thing", "1.9.0", None).as_deref(),
            Some("1.10.0")
        );
        let drm: Vec<String> = [
            "drm-5-0-0-20010105",
            "4.3.99.12",
            "libdrm-2.4.100",
            "libdrm-2.4.125",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            newest(&drm, "libdrm", "2.4.100", None).as_deref(),
            Some("2.4.125")
        );
        let curl = vec!["curl-8_20_0".to_string(), "curl-8_21_0".to_string()];
        assert_eq!(
            newest(&curl, "curl", "8.20.0", None).as_deref(),
            Some("8.21.0")
        );
        assert_eq!(newest(&[], "thing", "1.0", None), None);
    }

    #[test]
    fn tags_that_never_reach_nixpkgs_count_for_nothing() {
        // the OS that bundles ldacbt tags its own releases
        let vela: Vec<String> = ["dev_20250801", "trunk-5.2", "trunk-5.4", "trunk-5.5"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(newest(&vela, "ldacbt", "2.0.72", None), None);
        let tags: Vec<String> = ["v1.0", "v2.0"].iter().map(|s| s.to_string()).collect();
        assert_eq!(newest(&tags, "thing", "1.0", None).as_deref(), Some("2.0"));
        assert_eq!(newest(&tags, "thing", "2.0", None).as_deref(), Some("2.0"));
        assert_eq!(newest(&tags, "thing", "0.9", None), None);
        // an unstable version after a release still reaches that release
        let tags: Vec<String> = ["v1.2.3", "v1.3.0"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            newest(&tags, "thing", "1.2.3-unstable-2025-01-01", None).as_deref(),
            Some("1.3.0")
        );
        // a snapshot of a project that has a release has nothing to reach
        assert_eq!(
            newest(&tags, "thing", "0-unstable-2022-08-14", None).as_deref(),
            Some("1.3.0")
        );
    }

    #[test]
    fn plain_tags_outrank_tags_with_a_word() {
        let tags = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mrustc = tags(&["v0.11.2", "v0.12.0", "hello_1.39", "rust1.90_rustc"]);
        assert_eq!(
            newest(&mrustc, "mrustc", "0.12.0", None).as_deref(),
            Some("0.12.0")
        );
        let arx = tags(&["1.2", "1.2.1", "ArxFatalis-1.21"]);
        assert_eq!(
            newest(&arx, "arx-libertatis", "1.2.1", None).as_deref(),
            Some("1.2.1")
        );
        // one odd tag that names the package does not hide the bare ones
        let rabbit = tags(&["rabbitmq-c-v0.3.0", "v0.15.0", "v0.18.1"]);
        assert_eq!(
            newest(&rabbit, "rabbitmq-c", "0.15.0", None).as_deref(),
            Some("0.18.1")
        );
        // a release prefix is as plain as a `v`
        let approxmc = tags(&["4.1.24", "release/4.2.0", "release/v4.4.0", "cav2020-paper"]);
        assert_eq!(
            newest(&approxmc, "approxmc", "4.1.24", None).as_deref(),
            Some("4.4.0")
        );
        // no plain tags: the words count
        let rel = tags(&["build-1.2", "build-1.3"]);
        assert_eq!(newest(&rel, "thing", "1.2", None).as_deref(), Some("1.3"));
        // tags that name the package still come first
        let gs = tags(&[
            "ghostpdl-10.07.1",
            "ghostscript-10.03.0",
            "ghostscript-10.05.1",
            "10.03.0",
        ]);
        assert_eq!(
            newest(&gs, "ghostscript", "10.03.0", None).as_deref(),
            Some("10.05.1")
        );
    }

    #[test]
    fn repology_is_believed_only_for_a_newer_release() {
        assert!(newer("1.10", "1.9"));
        assert!(!newer("2.0-rc1", "1.9"));
        assert!(!newer("20240101", "1.9"));
        assert!(!newer("1.9", "1.9"));
    }

    #[test]
    fn each_registry_declares_its_latest_differently() {
        let pick = |eco: &str, body: &str, name: &str, current: &str| {
            newest_published(eco, &published_versions(eco, body), name, current, None)
        };
        let index = "{\"vers\":\"1.0.0\"}\n{\"vers\":\"2.0.68\"}\n{\"vers\":\"2.0.99\",\"yanked\":true}\n{\"vers\":\"3.0.0-alpha.1\"}\n";
        assert_eq!(
            pick("crates.io", index, "syn", "1.0.0").as_deref(),
            Some("2.0.68")
        );
        assert_eq!(
            pick("PyPI", "{\"info\":{\"version\":\"3.4.0\"}}", "x", "3.0").as_deref(),
            Some("3.4.0")
        );
        assert_eq!(
            pick(
                "npm",
                "{\"latest\":\"5.1.2\",\"next\":\"6.0.0-rc.1\"}",
                "x",
                "5.0.0"
            )
            .as_deref(),
            Some("5.1.2")
        );
        assert_eq!(crates_path("syn"), "3/s/syn");
        assert_eq!(crates_path("Serde"), "se/rd/serde");
        // Hackage lists every version, newest first
        let hackage =
            "{\"deprecated-version\":[\"2.1.19\"],\"normal-version\":[\"3.10\",\"3.8\",\"3.6.1\"]}";
        assert_eq!(
            pick("Hackage", hackage, "hlint", "3.8").as_deref(),
            Some("3.10")
        );
        // GNOME: `[format, {files}, {name: [versions]}]`; the prereleases
        // and, below 40 on an even minor, the odd minors do not count
        let gnome = "[4,{\"gnome-control-center\":{}},{\"gnome-control-center\":[\"50.4\",\"51.alpha\",\"51.beta\",\"51.rc.1\",\"51.0\",\"51.1\"]}]";
        assert_eq!(
            pick("GNOME", gnome, "gnome-control-center", "50.4").as_deref(),
            Some("51.1")
        );
        let glib = "[4,{\"glib\":{}},{\"glib\":[\"2.86.0\",\"2.86.1\",\"2.89.4\",\"2.90.0\"]}]";
        assert_eq!(
            pick("GNOME", glib, "glib", "2.86.0").as_deref(),
            Some("2.90.0")
        );
        let glib = "[4,{\"glib\":{}},{\"glib\":[\"2.86.0\",\"2.86.1\",\"2.87.0\"]}]";
        assert_eq!(
            pick("GNOME", glib, "glib", "2.86.0").as_deref(),
            Some("2.86.1")
        );
        let dconf = "[4,{\"dconf\":{}},{\"dconf\":[\"0.48.0\",\"0.49.0\",\"0.49.1\"]}]";
        assert_eq!(
            pick("GNOME", dconf, "dconf", "0.49.0").as_deref(),
            Some("0.49.1")
        );
        // GNU: an Apache listing of tarballs, signatures and release dirs
        let gnu = "<a href=\"../\">..</a> <a href=\"autoconf-2.72.tar.xz\">x</a> <a href=\"autoconf-2.72.tar.xz.sig\">s</a> <a href=\"autoconf-2.73.tar.gz\">x</a> <a href=\"autoconf-latest.tar.gz\">l</a> <a href=\"gcc-15.1.0/\">d</a>";
        assert_eq!(
            published_versions("GNU", gnu),
            [
                "autoconf-2.72",
                "autoconf-2.73",
                "autoconf-latest",
                "gcc-15.1.0"
            ]
        );
        assert_eq!(
            pick("GNU", gnu, "autoconf", "2.72").as_deref(),
            Some("2.73")
        );
        assert_eq!(pick("GNU", gnu, "gcc", "14.2.0").as_deref(), None);
        assert_eq!(pick("GNU", gnu, "gcc", "15.1.0").as_deref(), Some("15.1.0"));
    }

    #[test]
    fn ls_refs_lines_become_tags() {
        let body = b"003d4cab85e15cc4ec92feada93c650f1f59c0a15a7f refs/tags/0.0.1\n0000";
        assert_eq!(parse_refs(body), vec!["0.0.1"]);
        assert!(parse_refs(b"zzzz").is_empty());
    }
}
