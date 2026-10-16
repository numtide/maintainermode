//! Version order the way nix sees it, with one change: a prerelease loses to
//! its release. Taken from nixfresh (MIT).

use std::cmp::Ordering;

/// Split into digit and non-digit chunks, compare numbers as numbers, and
/// let a missing chunk lose to a present one unless that one is a prerelease.
pub fn compare(a: &str, b: &str) -> Ordering {
    let mut left = chunks(a).into_iter();
    let mut right = chunks(b).into_iter();
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (l, r) => match cmp_chunk(&l.unwrap_or_default(), &r.unwrap_or_default()) {
                Ordering::Equal => continue,
                other => return other,
            },
        }
    }
}

fn chunks(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut digits = false;
    for c in v.chars() {
        if c == '.' || c == '-' || c == '_' {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            continue;
        }
        let is_digit = c.is_ascii_digit();
        if !current.is_empty() && is_digit != digits {
            out.push(std::mem::take(&mut current));
        }
        digits = is_digit;
        current.push(c);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// nix treats only `pre` as a prerelease, so 1.0-rc1 would beat 1.0 and hide
/// a pending upgrade.
fn cmp_chunk(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let pre = |s: &str| matches!(s, "pre" | "rc" | "alpha" | "beta" | "dev");
    match (pre(a), pre(b)) {
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(l), Ok(r)) => l.cmp(&r),
        (Ok(_), Err(_)) => Ordering::Greater,
        (Err(_), Ok(_)) => Ordering::Less,
        (Err(_), Err(_)) => a.cmp(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_numerically() {
        assert_eq!(compare("1.10", "1.9"), Ordering::Greater);
        assert_eq!(compare("3.0.14", "3.0.14"), Ordering::Equal);
        assert_eq!(compare("1.2.3", "1.2.3.1"), Ordering::Less);
    }

    #[test]
    fn prereleases_sort_below_releases() {
        assert_eq!(compare("1.0-rc1", "1.0"), Ordering::Less);
        assert_eq!(compare("2.0-beta", "1.9"), Ordering::Greater);
    }
}
