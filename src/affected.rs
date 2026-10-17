//! Which versions an advisory affects. The tracker copies the affected
//! ranges of each CVE record; when they do not say, the advisory title often
//! does (`unbound: security issues < 1.26.1`). Every doubt gives `Unknown`:
//! a wrong "not affected" hides a real advisory.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

use crate::version::compare;

/// The ranges of one CVE for one attribute, as the tracker writes them:
/// `["affected", "==< 2.4.20"]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ranges {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cve: Option<String>,
    pub constraints: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    Affected,
    NotAffected,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Any,
    All(Vec<(Op, String)>),
}

/// A version names a release: a digit first, no `unstable`, no commit hash.
fn clean(v: &str) -> Option<String> {
    let v = v.trim().trim_end_matches(['.', ',', ';', ')']);
    let v = match v.strip_prefix(['v', 'V']) {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => rest,
        _ => v,
    };
    let hash = v
        .split(['.', '-', '+'])
        .any(|p| p.len() >= 12 && p.chars().all(|c| c.is_ascii_hexdigit()));
    (v.starts_with(|c: char| c.is_ascii_digit())
        && !hash
        && !v.contains(char::is_whitespace)
        && !v.contains("unstable"))
    .then(|| v.to_string())
}

fn op_part(s: &str) -> Option<(Op, String)> {
    let s = s.trim();
    let (op, rest) = [
        ("<=", Op::Le),
        (">=", Op::Ge),
        ("=<", Op::Le),
        ("<", Op::Lt),
        (">", Op::Gt),
        ("=", Op::Eq),
    ]
    .into_iter()
    .find_map(|(p, op)| s.strip_prefix(p).map(|r| (op, r)))
    .unwrap_or((Op::Eq, s));
    Some((op, clean(rest)?))
}

/// The forms the tracker writes: `<1.0`, `=<1.0`, `==1.0`, `==< 1.0`,
/// `==<= 1.0`, `=== 1.0`, `==>= 1.0, < 1.2`, `==1.0 to 1.2` and `*`.
fn parse(s: &str) -> Option<Expr> {
    let s = s.trim();
    if s == "*" {
        return Some(Expr::Any);
    }
    if let Some(v) = s.strip_prefix("==") {
        if let Some((a, b)) = v.split_once(" to ") {
            return Some(Expr::All(vec![(Op::Ge, clean(a)?), (Op::Le, clean(b)?)]));
        }
        return v
            .split(',')
            .map(op_part)
            .collect::<Option<Vec<_>>>()
            .map(Expr::All);
    }
    Some(Expr::All(vec![op_part(s)?]))
}

/// The first number of a calendar version has four digits or more; a bound
/// of another scheme says nothing about it.
fn same_scheme(a: &str, b: &str) -> bool {
    let wide = |v: &str| v.chars().take_while(char::is_ascii_digit).count() >= 4;
    wide(a) == wide(b)
}

impl Expr {
    fn matches(&self, v: &str) -> Option<bool> {
        let Expr::All(parts) = self else {
            return Some(true);
        };
        let mut all = true;
        for (op, bound) in parts {
            if !same_scheme(v, bound) {
                return None;
            }
            let o = compare(v, bound);
            all &= match op {
                Op::Lt => o == Ordering::Less,
                Op::Le => o != Ordering::Greater,
                Op::Gt => o == Ordering::Greater,
                Op::Ge => o != Ordering::Less,
                Op::Eq => o == Ordering::Equal,
            };
        }
        Some(all)
    }

    fn exact(&self) -> bool {
        matches!(self, Expr::All(p) if p.iter().all(|(op, _)| *op == Op::Eq))
    }

    fn below(&self) -> impl Iterator<Item = &str> {
        let parts = match self {
            Expr::All(p) => p.as_slice(),
            Expr::Any => &[],
        };
        parts
            .iter()
            .filter(|(op, _)| *op == Op::Lt)
            .map(|(_, v)| v.as_str())
    }
}

/// One CVE's verdict for a version. An affected range that matches wins;
/// then an exact unaffected version. A list of exact affected versions
/// never clears a version it does not name: such lists often name only the
/// version someone tested.
pub fn check_constraints(list: &[(String, String)], v: &str) -> Check {
    let mut affected = 0;
    let mut doubt = false;
    let mut exact_only = true;
    let mut cleared = false;
    for (status, range) in list {
        let e = parse(range);
        match status.as_str() {
            "affected" => {
                affected += 1;
                match e.as_ref().map(|e| (e.matches(v), e.exact())) {
                    Some((Some(true), _)) => return Check::Affected,
                    Some((Some(false), exact)) => exact_only &= exact,
                    _ => doubt = true,
                }
            }
            "unaffected" => {
                if let Some(e @ Expr::All(_)) = &e {
                    cleared |= e.matches(v) == Some(true);
                }
            }
            _ => {
                if e.is_none_or(|e| e.matches(v) != Some(false)) {
                    doubt = true;
                }
            }
        }
    }
    if cleared {
        Check::NotAffected
    } else if doubt || affected == 0 || exact_only {
        Check::Unknown
    } else {
        Check::NotAffected
    }
}

fn bare(word: &str) -> &str {
    word.trim_matches(|c: char| matches!(c, ':' | ',' | ';' | '(' | ')' | '"' | '\''))
}

/// `< X`, `<= X`, `before X` and `through X` in a title, when a word before
/// it names the attribute: a title about another product says nothing
/// about its version.
pub fn title_constraint(title: &str, attr: &str) -> Option<(String, String)> {
    let words: Vec<&str> = title.split_whitespace().collect();
    (1..words.len()).find_map(|i| {
        let op = match words[i - 1] {
            "<" | "before" => "<",
            "<=" | "through" => "=<",
            _ => return None,
        };
        let v = clean(words[i])?;
        words[..i - 1]
            .iter()
            .any(|w| same_name(bare(w), attr))
            .then(|| ("affected".to_string(), format!("{op}{v}")))
    })
}

/// A version the title names as affected, not as the fix, and that `v`
/// has not passed: `PyMuPDF 1.28.2 Path Traversal` against ranges that say
/// `=<1.8.2`. The ranges are then not trusted.
fn title_contradicts(title: &str, v: &str) -> bool {
    let words: Vec<&str> = title.split_whitespace().collect();
    words.iter().enumerate().any(|(i, w)| {
        let fix = i > 0 && matches!(words[i - 1], "<" | "before");
        !fix && clean(bare(w))
            .is_some_and(|t| same_scheme(v, &t) && compare(v, &t) != Ordering::Greater)
    })
}

/// The verdict of one advisory for one version: affected when one CVE is.
/// A CVE whose ranges do not decide falls back on the title.
pub fn check(ranges: &[Ranges], title: &str, attr: &str, v: &str) -> Check {
    let Some(v) = clean(v) else {
        return Check::Unknown;
    };
    let title_rows: Vec<(String, String)> = title_constraint(title, attr).into_iter().collect();
    let by_title = || check_constraints(&title_rows, &v);
    let mut all_clear = true;
    if ranges.is_empty() {
        match by_title() {
            Check::NotAffected => {}
            c => return c,
        }
    }
    for r in ranges {
        let c = match check_constraints(&r.constraints, &v) {
            Check::Unknown => by_title(),
            c => c,
        };
        match c {
            Check::Affected => return Check::Affected,
            Check::Unknown => all_clear = false,
            Check::NotAffected => {}
        }
    }
    if all_clear && !title_contradicts(title, &v) {
        Check::NotAffected
    } else {
        Check::Unknown
    }
}

/// The release that fixed `v`: the highest `<` bound at or below it.
pub fn fixed_in(ranges: &[Ranges], title: &str, attr: &str, v: &str) -> Option<String> {
    let title = title_constraint(title, attr);
    let rows = ranges
        .iter()
        .flat_map(|r| r.constraints.iter())
        .chain(title.iter())
        .filter(|(s, _)| s == "affected");
    let mut best: Option<String> = None;
    for (_, range) in rows {
        let Some(e) = parse(range) else { continue };
        for b in e.below() {
            if compare(b, v) != Ordering::Greater
                && best
                    .as_deref()
                    .is_none_or(|x| compare(b, x) == Ordering::Greater)
            {
                best = Some(b.to_string());
            }
        }
    }
    best
}

/// A channel that still carries an affected version.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Lag {
    pub channel: String,
    pub version: String,
    pub stable: bool,
}

/// An open advisory against what nixpkgs ships.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status")]
pub enum Verdict {
    #[serde(rename = "affects_master")]
    Affects,
    /// Master is fixed; these channels are not.
    #[serde(rename = "fixed_in_master")]
    Behind {
        fixed_in: Option<String>,
        channels: Vec<Lag>,
    },
    /// Master and every channel read are fixed: the tracker issue can close.
    #[serde(rename = "fixed_everywhere")]
    Fixed { fixed_in: Option<String> },
    #[serde(rename = "unknown_range")]
    Unknown,
}

pub static UNKNOWN: Verdict = Verdict::Unknown;

impl Verdict {
    /// Master can be affected.
    pub fn open(&self) -> bool {
        matches!(self, Verdict::Affects | Verdict::Unknown)
    }

    pub fn fixed(&self) -> bool {
        matches!(self, Verdict::Fixed { .. })
    }
}

/// `channels` holds the version of each channel read; one that lacks the
/// attribute is left out. No channel read means master decides alone.
pub fn judge(
    ranges: &[Ranges],
    title: &str,
    attr: &str,
    master: &str,
    channels: &[(&str, &str, bool)],
) -> Verdict {
    match check(ranges, title, attr, master) {
        Check::Affected => return Verdict::Affects,
        Check::Unknown => return Verdict::Unknown,
        Check::NotAffected => {}
    }
    let fixed_in = fixed_in(ranges, title, attr, master);
    let mut lags = Vec::new();
    for (channel, version, stable) in channels {
        match check(ranges, title, attr, version) {
            Check::Affected => lags.push(Lag {
                channel: channel.to_string(),
                version: version.to_string(),
                stable: *stable,
            }),
            Check::Unknown => return Verdict::Unknown,
            Check::NotAffected => {}
        }
    }
    if lags.is_empty() {
        Verdict::Fixed { fixed_in }
    } else {
        Verdict::Behind {
            fixed_in,
            channels: lags,
        }
    }
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

/// Suffixes of an attribute that keep the upstream numbering, after an
/// optional major version: `ffmpeg_9-headless`, `unbound-with-systemd`.
const VARIANTS: &[&str] = &[
    "",
    "full",
    "headless",
    "minimal",
    "unwrapped",
    "withsystemd",
    "x",
    "bin",
    "lts",
    "small",
    "nox",
];

/// A product name and an attribute name the same package: equal, or one
/// with a major version or a build variant added. `immich-wallpaper` is
/// not `immich`, and `pylookyloo` is not `lookyloo`.
pub fn same_name(name: &str, attr: &str) -> bool {
    let last = |s: &str| norm(s.rsplit('.').next().unwrap_or(s));
    let (n, a) = (last(name), last(attr));
    if n.is_empty() || a.is_empty() {
        return false;
    }
    let rest = match (a.strip_prefix(&n), n.strip_prefix(&a)) {
        (Some(r), _) | (_, Some(r)) => r,
        _ => return false,
    };
    VARIANTS.contains(&rest.trim_start_matches(|c: char| c.is_ascii_digit()))
}

/// The products of a CVE that name this attribute. None is a valid answer:
/// the range of another product must not judge this one.
pub fn products_for(attr: &str, names: &[&str]) -> Vec<usize> {
    names
        .iter()
        .enumerate()
        .filter(|(_, n)| same_name(n, attr))
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(list: &[(&str, &str)], v: &str) -> Check {
        let list: Vec<(String, String)> = list
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        check_constraints(&list, v)
    }

    use Check::*;

    #[test]
    fn less_than() {
        assert_eq!(c(&[("affected", "<3.2.0")], "3.1.9"), Affected);
        assert_eq!(c(&[("affected", "<3.2.0")], "3.2.0"), NotAffected);
        assert_eq!(c(&[("affected", "<v1.19.2")], "1.19.1"), Affected);
        assert_eq!(c(&[("affected", "<1.1.1zj")], "1.1.1zi"), Affected);
        assert_eq!(c(&[("affected", "<7.1.2-32")], "7.1.2-35"), NotAffected);
    }

    #[test]
    fn less_or_equal() {
        assert_eq!(c(&[("affected", "=<2.3.3")], "2.3.3"), Affected);
        assert_eq!(c(&[("affected", "=<2.3.3")], "2.3.4"), NotAffected);
        assert_eq!(c(&[("affected", "=<v1.36.4")], "1.36.5"), NotAffected);
        assert_eq!(c(&[("affected", "==<= 13.14.2")], "13.14.2"), Affected);
    }

    #[test]
    fn a_range_in_the_version_field() {
        assert_eq!(c(&[("affected", "==< 2.4.20")], "2.4.19"), Affected);
        assert_eq!(c(&[("affected", "==< 2.4.20")], "2.4.20"), NotAffected);
        let r = [
            ("affected", "==>= 4.3.0, < 4.3.3"),
            ("affected", "==>= 4.2.0, < 4.2.9"),
        ];
        assert_eq!(c(&r, "4.3.2"), Affected);
        assert_eq!(c(&r, "4.2.9"), NotAffected);
        assert_eq!(c(&r, "4.3.3"), NotAffected);
        assert_eq!(c(&r, "4.1.0"), NotAffected);
        assert_eq!(
            c(&[("affected", "==>= 2.0.0b1, < 2.44.0")], "2.43.1"),
            Affected
        );
        assert_eq!(c(&[("affected", "==1.0 to 1.2")], "1.1"), Affected);
        assert_eq!(c(&[("affected", "==1.0 to 1.2")], "1.3"), NotAffected);
    }

    #[test]
    fn exact_versions_only_mark_what_they_name() {
        let r = [
            ("affected", "==1.2.0"),
            ("affected", "==1.2.2"),
            ("affected", "==1.2.1"),
        ];
        assert_eq!(c(&r, "1.2.2"), Affected);
        assert_eq!(c(&r, "1.2.3"), Unknown);
        assert_eq!(c(&[("affected", "=== 1.7.2")], "1.7.2"), Affected);
        assert_eq!(c(&[("affected", "=== 1.7.2")], "1.7.3"), Unknown);
    }

    #[test]
    fn an_unaffected_version_clears_only_itself() {
        let r = [("unaffected", "==3.2.0"), ("affected", "<3.2.0")];
        assert_eq!(c(&r, "3.2.0"), NotAffected);
        assert_eq!(c(&r, "3.1"), Affected);
        assert_eq!(c(&[("unaffected", "==3.2.0")], "3.3.0"), Unknown);
        assert_eq!(c(&[("unaffected", "==3.2.0")], "3.2.0"), NotAffected);
        assert_eq!(c(&[("unaffected", "*")], "3.2.0"), Unknown);
    }

    #[test]
    fn what_cannot_be_read_stays_unknown() {
        let go = [
            ("affected", "==< 1.80.0-v2.20.0.20260317120401-d90285929ca0"),
            ("affected", "==>= 4.0.0-rc.1, < 4.12.3"),
        ];
        assert_eq!(c(&go, "2.71.7"), Unknown);
        assert_eq!(c(&[("affected", "*")], "1.0"), Affected);
        assert_eq!(
            c(
                &[("affected", "==13fcb641ed33d1472e4ffdec2846180b15405053")],
                "1.0"
            ),
            Unknown
        );
        assert_eq!(
            c(&[("affected", "==dev (commit 38b21f78)")], "1.0"),
            Unknown
        );
        assert_eq!(c(&[("affected", "=<LC1.2")], "1.0"), Unknown);
        assert_eq!(c(&[("unknown", "=<v1.33.13")], "1.0"), Unknown);
        assert_eq!(c(&[("unknown", "=<v1.33.13")], "1.40"), Unknown);
        assert_eq!(
            c(&[("affected", "<1.0"), ("unknown", "=<v1.33.13")], "1.40"),
            NotAffected
        );
        assert_eq!(c(&[], "1.0"), Unknown);
        assert_eq!(
            c(&[("affected", "<1.0"), ("affected", "==abi-16.26")], "2.0"),
            Unknown
        );
        assert_eq!(c(&[("affected", "<2.0")], "20240101"), Unknown);
    }

    #[test]
    fn the_title_decides_when_the_ranges_do_not() {
        let tc = title_constraint;
        assert_eq!(
            tc("unbound: security issues < 1.26.1", "unbound-with-systemd"),
            Some(("affected".into(), "<1.26.1".into()))
        );
        assert_eq!(
            tc(
                "python3Packages.gitpython: security issues < 3.2.0",
                "python314Packages.gitpython"
            ),
            Some(("affected".into(), "<3.2.0".into()))
        );
        assert_eq!(
            tc("Flower before 2.2.0 CSRF via Worker", "flower"),
            Some(("affected".into(), "<2.2.0".into()))
        );
        assert_eq!(
            tc("foo through 3.1, bar", "foo"),
            Some(("affected".into(), "=<3.1".into()))
        );
        assert_eq!(tc("Lookyloo < 1.40 zip bomb", "pylookyloo"), None);
        assert_eq!(tc("Praat Model Loader out-of-bounds write", "praat"), None);
        let hash = Ranges {
            cve: None,
            constraints: vec![(
                "affected".into(),
                "==13fcb641ed33d1472e4ffdec2846180b15405053".into(),
            )],
        };
        let t = "gstreamer: security issues < 1.28.5 (backport needed)";
        let one = std::slice::from_ref(&hash);
        assert_eq!(check(one, t, "gstreamer", "1.28.5"), NotAffected);
        assert_eq!(check(one, t, "gstreamer", "1.28.4"), Affected);
        assert_eq!(check(one, t, "gst-plugins-bad", "1.28.5"), Unknown);
        assert_eq!(
            check(one, "no version here", "gstreamer", "1.28.5"),
            Unknown
        );
        assert_eq!(check(&[], t, "gstreamer", "1.28.5"), NotAffected);
    }

    #[test]
    fn a_title_that_names_an_affected_version_overrules_the_ranges() {
        let r = Ranges {
            cve: None,
            constraints: vec![("affected".into(), "=<1.8.2".into())],
        };
        let t = "PyMuPDF 1.28.2 Path Traversal via extract_objects()";
        assert_eq!(
            check(std::slice::from_ref(&r), t, "pymupdf", "1.27.2.3"),
            Unknown
        );
        assert_eq!(check(&[r], t, "pymupdf", "1.28.3"), NotAffected);
        let unbound = "unbound: security issues < 1.26.1";
        assert_eq!(check(&[], unbound, "unbound", "1.26.1"), NotAffected);
    }

    #[test]
    fn one_affected_cve_is_enough() {
        let r = |s: &str| Ranges {
            cve: None,
            constraints: vec![("affected".into(), s.into())],
        };
        assert_eq!(check(&[r("<1.0"), r("<2.0")], "", "x", "1.5"), Affected);
        assert_eq!(check(&[r("<1.0"), r("==1.5")], "", "x", "1.6"), Unknown);
        assert_eq!(check(&[r("<1.0"), r("<1.2")], "", "x", "1.6"), NotAffected);
    }

    #[test]
    fn versions_compare_the_nix_way_with_prereleases_first() {
        let lt = |v: &str, b: &str| c(&[("affected", &format!("<{b}"))], v);
        assert_eq!(lt("1.26.1", "1.26.1"), NotAffected);
        assert_eq!(lt("1.10", "1.9"), NotAffected);
        assert_eq!(lt("1.9", "1.10"), Affected);
        assert_eq!(lt("1.0-rc1", "1.0"), Affected);
        assert_eq!(lt("1.0rc1", "1.0"), Affected);
        assert_eq!(check(&[], "x < 1.0", "x", "unstable-2024-01-01"), Unknown);
        assert_eq!(check(&[], "x < 1.0", "x", "0-unstable-2024-01-01"), Unknown);
        assert_eq!(check(&[], "x < 1.0", "x", "v0.9"), Affected);
    }

    #[test]
    fn the_fix_is_the_highest_bound_at_or_below() {
        let r = Ranges {
            cve: None,
            constraints: vec![
                ("affected".into(), "==>= 4.3.0, < 4.3.3".into()),
                ("affected".into(), "==>= 4.2.0, < 4.2.9".into()),
            ],
        };
        let one = std::slice::from_ref(&r);
        assert_eq!(fixed_in(one, "", "x", "4.3.5").as_deref(), Some("4.3.3"));
        assert_eq!(fixed_in(one, "", "x", "4.2.9").as_deref(), Some("4.2.9"));
        assert_eq!(fixed_in(&[r], "", "x", "4.1"), None);
        assert_eq!(
            fixed_in(
                &[],
                "unbound: security issues < 1.26.1",
                "unbound",
                "1.26.1"
            )
            .as_deref(),
            Some("1.26.1")
        );
    }

    #[test]
    fn a_verdict_reads_master_then_each_channel() {
        let t = "unbound: security issues < 1.26.1";
        let u = "unbound";
        assert_eq!(judge(&[], t, u, "1.26.0", &[]), Verdict::Affects);
        assert_eq!(
            judge(
                &[],
                t,
                u,
                "1.26.1",
                &[
                    ("nixos-unstable", "1.26.1", false),
                    ("nixos-26.05", "1.26.1", true)
                ]
            ),
            Verdict::Fixed {
                fixed_in: Some("1.26.1".into())
            }
        );
        assert_eq!(
            judge(&[], t, u, "1.26.1", &[("nixos-26.05", "1.25.0", true)]),
            Verdict::Behind {
                fixed_in: Some("1.26.1".into()),
                channels: vec![Lag {
                    channel: "nixos-26.05".into(),
                    version: "1.25.0".into(),
                    stable: true
                }]
            }
        );
        assert_eq!(
            judge(&[], "unbound: a flaw", u, "1.26.1", &[]),
            Verdict::Unknown
        );
        assert_eq!(
            judge(
                &[],
                t,
                u,
                "1.26.1",
                &[("nixos-26.05", "unstable-2024-01-01", true)]
            ),
            Verdict::Unknown
        );
    }

    #[test]
    fn a_product_judges_only_the_attribute_it_names() {
        assert_eq!(
            products_for("python3Packages.gitpython", &["GitPython", "other"]),
            vec![0]
        );
        assert_eq!(products_for("openssl_3", &["OpenSSL"]), vec![0]);
        assert_eq!(products_for("ffmpeg_9-headless", &["FFmpeg"]), vec![0]);
        assert_eq!(products_for("unbound-with-systemd", &["Unbound"]), vec![0]);
        assert_eq!(products_for("etcd_3_6", &["etcd"]), vec![0]);
        assert_eq!(
            products_for("cups", &["libcups", "cups-filters"]),
            Vec::<usize>::new()
        );
        assert_eq!(
            products_for("gnomeExtensions.immich-wallpaper", &["immich"]),
            Vec::<usize>::new()
        );
        assert_eq!(
            products_for("python3Packages.pylookyloo", &["lookyloo"]),
            Vec::<usize>::new()
        );
    }
}
