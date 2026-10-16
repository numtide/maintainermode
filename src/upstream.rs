//! The newest release from upstream itself: the git tags of the repository
//! that `src` comes from, or the registry that publishes the package. This
//! corrects repology, which reports what other distributions ship. Adapted
//! from nixfresh's `upstream.rs` and direct collector (MIT).

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
pub const BUDGET: usize = 300;
const PAUSE: Duration = Duration::from_millis(100);
/// How long an answer holds for a package someone maintains or watches, and
/// for the rest.
const FRESH_FIRST: i64 = 1;
const FRESH_REST: i64 = 7;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// crates.io, PyPI or npm: the publisher declared the version.
    Registry {
        ecosystem: &'static str,
        name: String,
    },
    /// A git remote, asked for its tags over smart HTTP: GitHub, GitLab,
    /// Codeberg, sourcehut, KDE and freedesktop alike, with no token.
    Git(String),
}

/// What a pass found for one package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Direct {
    /// `None`: the source did not answer, or its newest tag is older than
    /// nixpkgs, which means the tags were not read right.
    pub latest: Option<String>,
    /// `tags`, `crates.io`, `PyPI` or `npm`.
    pub from: String,
    #[serde(default)]
    pub failed: bool,
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
    let (ecosystem, name) =
        match (host, parts.as_slice()) {
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
pub fn origin(src_url: &str) -> Option<Origin> {
    if let Some(r) = registry(src_url) {
        return Some(r);
    }
    let url = src_url
        .strip_prefix("https://")
        .or(src_url.strip_prefix("http://"))?;
    let (host, path) = url.split_once('/')?;
    let git_host = host == "github.com"
        || host == "codeberg.org"
        || host.starts_with("gitlab.")
        || host == "git.sr.ht"
        || host == "invent.kde.org";
    if !git_host {
        return None;
    }
    let slug = match path
        .strip_prefix("api/v4/projects/")
        .and_then(|p| p.split_once('/'))
    {
        Some((slug, _)) => unescape(slug)?,
        // `/-/archive/` before `/archive/`, or a GitLab repository keeps a `/-`
        None => path
            .split_once("/-/archive/")
            .or_else(|| path.split_once("/-/releases/"))
            .or_else(|| path.split_once("/archive/"))
            .or_else(|| path.split_once("/releases/"))
            .or_else(|| path.split_once("/repository/"))
            .map(|(repo, _)| repo)
            .unwrap_or(path)
            .to_string(),
    };
    let repo = slug.trim_end_matches(".git").trim_end_matches('/');
    if repo.is_empty()
        || !repo.contains('/')
        || repo.contains("..")
        || repo.contains(['%', '?', '#'])
    {
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
    for prefix in named
        .iter()
        .map(String::as_str)
        .chain(["release-", "release/", "REL_", "v", "V"])
    {
        if let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped;
            break;
        }
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

/// The newest tag on the same numbering scheme as the version nixpkgs has.
pub fn newest(tags: &[String], pname: &str, current: &str) -> Option<String> {
    let ours = scale(current);
    // a repository that holds several projects names ours in its tags
    let named = |t: &&String| {
        !pname.is_empty()
            && ["-", "_", "/"]
                .iter()
                .any(|s| t.starts_with(&format!("{pname}{s}")))
    };
    let pool: Vec<&String> = if tags.iter().any(|t| named(&t)) {
        tags.iter().filter(named).collect()
    } else {
        tags.iter().collect()
    };
    pool.into_iter()
        .filter_map(|tag| version_of(tag, pname))
        .filter_map(|c| dotted(c, current))
        .filter(|c| ours.is_none() || scale(c) == ours)
        .max_by(|a, b| version::compare(a, b))
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
        _ => None,
    }
}

/// The newest release a registry names. No scheme filter: the publisher
/// declared it.
pub fn newest_published(ecosystem: &str, body: &str) -> Option<String> {
    match ecosystem {
        // one JSON object per line, oldest first; a yanked release is no target
        "crates.io" => body
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["yanked"].as_bool() != Some(true))
            .filter_map(|v| v["vers"].as_str().map(str::to_string))
            .filter(|v| version_of(v, "").is_some())
            .max_by(|a, b| version::compare(a, b)),
        // PyPI's own latest leaves out prereleases
        "PyPI" => serde_json::from_str::<Value>(body).ok()?["info"]["version"]
            .as_str()
            .map(str::to_string),
        "npm" => serde_json::from_str::<Value>(body).ok()?["latest"]
            .as_str()
            .map(str::to_string),
        _ => None,
    }
    .filter(|v| !v.is_empty())
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

async fn published(http: &reqwest::Client, ecosystem: &str, name: &str) -> Result<Option<String>> {
    let Some(url) = registry_url(ecosystem, name) else {
        return Ok(None);
    };
    let body = http
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(newest_published(ecosystem, &body))
}

/// One package to ask about.
pub struct Candidate {
    pub attr: String,
    pub pname: String,
    pub version: String,
    pub origin: Origin,
    /// Someone maintains or watches it.
    pub first: bool,
}

/// 1, 2, 4 … 32 days.
fn retry_days(failures: u32) -> i64 {
    FRESH_FIRST << failures.saturating_sub(1).min(5)
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
    let days = if d.failed {
        retry_days(d.failures)
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
    let http = reqwest::Client::builder()
        .user_agent("maintainer-mode (+https://github.com/numtide/maintainermode)")
        .timeout(Duration::from_secs(30))
        .build()
        .expect("http client");
    let total = candidates.len();
    let mut queue: Vec<(u8, DateTime<Utc>, Candidate)> = candidates
        .into_iter()
        .filter_map(|c| due(h, &c).map(|(rank, at)| (rank, at, c)))
        .collect();
    queue.sort_by(|a, b| (a.0, a.1, &a.2.attr).cmp(&(b.0, b.1, &b.2.attr)));
    let left = queue.len();
    let mut seen: HashMap<String, Result<Vec<String>, ()>> = HashMap::new();
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
                    Ok(t) => (Ok(newest(t, &c.pname, &c.version)), "tags"),
                    Err(()) => (Err(()), "tags"),
                }
            }
            Origin::Registry { ecosystem, name } => {
                asked.insert(key.clone());
                let got = published(&http, ecosystem, name).await.map_err(|e| {
                    tracing::debug!("{}: {e:#}", c.attr);
                });
                tokio::time::sleep(PAUSE).await;
                (got, *ecosystem)
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
                // newest tag older than nixpkgs: the tags were misread
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
                d.failed = true;
                d.failures += 1;
                d.checked_at = now;
                continue;
            }
        }
        let seen_at = seen_at(h, &c.attr, &latest, now);
        h.direct.insert(
            c.attr.clone(),
            Direct {
                failed: unreachable,
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
            origin("https://ftpmirror.gnu.org/bison/bison-3.8.2.tar.xz"),
            None
        );
        assert_eq!(origin("https://github.com/only-a-user"), None);
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
            failed,
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
        assert_eq!(retry_days(0), 1);
        assert_eq!(retry_days(1), 1);
        assert_eq!(retry_days(2), 2);
        assert_eq!(retry_days(4), 8);
        assert_eq!(retry_days(40), 32);
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
    }

    #[test]
    fn a_tag_is_not_always_a_version() {
        assert_eq!(version_of("v1.2.3", "zlib").as_deref(), Some("1.2.3"));
        assert_eq!(version_of("zlib-1.3.1", "zlib").as_deref(), Some("1.3.1"));
        assert_eq!(
            version_of("ghostpdl-10.07.1", "ghostscript").as_deref(),
            Some("10.07.1")
        );
        assert_eq!(version_of("1.2.3^{}", "x").as_deref(), Some("1.2.3"));
        assert_eq!(version_of("v2.0.0-rc1", "x"), None);
        assert_eq!(version_of("nightly", "x"), None);
        assert_eq!(version_of("debian/1.2.3", "x"), None);
        assert_eq!(version_of("942850058", "libwebsockets"), None);
        assert_eq!(version_of("2024.1", "x").as_deref(), Some("2024.1"));
    }

    #[test]
    fn the_newest_release_on_our_scheme_wins() {
        let tags: Vec<String> = ["v1.2.3", "v1.10.0", "v2.0.0-rc1", "2015-11-19-1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(newest(&tags, "thing", "1.9.0").as_deref(), Some("1.10.0"));
        let drm: Vec<String> = ["drm-5-0-0-20010105", "4.3.99.12", "libdrm-2.4.125"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            newest(&drm, "libdrm", "2.4.100").as_deref(),
            Some("2.4.125")
        );
        let curl = vec!["curl-8_21_0".to_string()];
        assert_eq!(newest(&curl, "curl", "8.20.0").as_deref(), Some("8.21.0"));
        assert_eq!(newest(&[], "thing", "1.0"), None);
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
        let index = "{\"vers\":\"1.0.0\"}\n{\"vers\":\"2.0.68\"}\n{\"vers\":\"2.0.99\",\"yanked\":true}\n{\"vers\":\"3.0.0-alpha.1\"}\n";
        assert_eq!(
            newest_published("crates.io", index).as_deref(),
            Some("2.0.68")
        );
        assert_eq!(
            newest_published("PyPI", "{\"info\":{\"version\":\"3.4.0\"}}").as_deref(),
            Some("3.4.0")
        );
        assert_eq!(
            newest_published("npm", "{\"latest\":\"5.1.2\",\"next\":\"6.0.0-rc.1\"}").as_deref(),
            Some("5.1.2")
        );
        assert_eq!(crates_path("syn"), "3/s/syn");
        assert_eq!(crates_path("Serde"), "se/rd/serde");
    }

    #[test]
    fn ls_refs_lines_become_tags() {
        let body = b"003d4cab85e15cc4ec92feada93c650f1f59c0a15a7f refs/tags/0.0.1\n0000";
        assert_eq!(parse_refs(body), vec!["0.0.1"]);
        assert!(parse_refs(b"zzzz").is_empty());
    }
}
