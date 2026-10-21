//! One number per package: how much it needs attention. Additive, capped per
//! cause, and every point carries its reason.

use serde::Serialize;

use crate::affected::Verdict;
use crate::area::Package;
use crate::health::{Advisory, Health};
use crate::severity::{self, Band, Worst};

const ADVISORY_CAP: u32 = 60;
const HYGIENE_CAP: u32 = 3;

#[derive(Debug, Clone, Serialize)]
pub struct Reason {
    pub points: u32,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Score {
    pub risk: u32,
    pub reasons: Vec<Reason>,
}

impl Score {
    fn add(&mut self, points: u32, text: String) {
        if points > 0 {
            self.risk += points;
            self.reasons.push(Reason { points, text });
        }
    }

    pub fn band(&self) -> &'static str {
        match self.risk {
            0 => "ok",
            1..=14 => "low",
            15..=29 => "medium",
            30..=49 => "high",
            _ => "critical",
        }
    }

    pub fn label_class(&self) -> &'static str {
        match self.risk {
            0 => "Label--success",
            1..=14 => "",
            15..=29 => "Label--attention",
            _ => "Label--danger",
        }
    }
}

fn advisory_points(band: Option<Band>) -> u32 {
    match band {
        Some(Band::Critical) => 40,
        Some(Band::High) => 25,
        Some(Band::Medium) => 10,
        Some(Band::Low) => 3,
        None => 5,
    }
}

fn major(v: &str) -> &str {
    v.split(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())
        .unwrap_or("")
}

/// The facts a score reads, gathered by the caller.
pub struct Input<'a> {
    pub package: &'a Package,
    /// The open tracker advisories with their verdicts.
    pub advisories: &'a [(&'a Advisory, &'a Verdict)],
    /// OSV matches the tracker does not name, with their worst severity.
    pub osv: &'a [Worst],
    pub upstream: Option<&'a str>,
    pub hygiene: Option<&'a [&'static str]>,
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn channels_of(list: &[&Verdict]) -> String {
    let mut names: Vec<&str> = list
        .iter()
        .flat_map(|v| match v {
            Verdict::Behind { channels, .. } => {
                channels.iter().map(|l| l.channel.as_str()).collect()
            }
            _ => Vec::new(),
        })
        .collect();
    names.sort_unstable();
    names.dedup();
    names.join(", ")
}

pub fn score(h: &Health, i: &Input) -> Score {
    let mut s = Score::default();
    let p = i.package;
    let live: Vec<(&Advisory, &Verdict, Worst)> = i
        .advisories
        .iter()
        .filter(|(_, v)| !v.fixed())
        .map(|(a, v)| (*a, *v, severity::worst(h, &a.cves)))
        .collect();
    let group = |keep: fn(&Verdict) -> bool| -> (usize, u32) {
        live.iter()
            .filter(|(_, v, _)| keep(v))
            .fold((0, 0), |(n, pts), (_, _, w)| {
                (n + 1, pts + advisory_points(w.band))
            })
    };
    let (affects, affects_pts) = group(|v| matches!(v, Verdict::Affects));
    let osv_pts: u32 = i.osv.iter().map(|w| advisory_points(w.band)).sum();
    let (unknown, unknown_pts) = group(|v| matches!(v, Verdict::Unknown));
    let (behind, behind_pts) = group(|v| matches!(v, Verdict::Behind { .. }));
    let lagging: Vec<&Verdict> = live
        .iter()
        .map(|(_, v, _)| *v)
        .filter(|v| matches!(v, Verdict::Behind { .. }))
        .collect();
    let mut room = ADVISORY_CAP;
    let mut take = |s: &mut Score, points: u32, text: String| {
        let points = points.min(room);
        room -= points;
        s.add(points, text);
    };
    if affects > 0 {
        take(
            &mut s,
            affects_pts,
            format!(
                "{} {} {}",
                plural(affects, "open advisory", "open advisories"),
                if affects == 1 { "affects" } else { "affect" },
                p.version
            ),
        );
    }
    if !i.osv.is_empty() {
        take(
            &mut s,
            osv_pts,
            format!(
                "{} for {}",
                plural(i.osv.len(), "OSV advisory", "OSV advisories"),
                p.version
            ),
        );
    }
    if unknown > 0 {
        take(
            &mut s,
            unknown_pts,
            format!(
                "{}, range unknown",
                plural(unknown, "open advisory", "open advisories")
            ),
        );
    }
    if behind > 0 {
        take(
            &mut s,
            behind_pts.div_ceil(2),
            format!(
                "{} fixed in master, still in {}",
                plural(behind, "advisory", "advisories"),
                channels_of(&lagging)
            ),
        );
    }
    let worst: Vec<(String, Worst)> = live
        .iter()
        .map(|(a, _, w)| (a.code.clone(), w.clone()))
        .chain(
            i.osv
                .iter()
                .map(|w| ("an OSV advisory".to_string(), w.clone())),
        )
        .collect();
    // exploitation and likelihood count once, on the worst advisory
    if let Some((id, _)) = worst
        .iter()
        .find(|(_, w)| w.exploitation.as_deref() == Some("active"))
    {
        s.add(20, format!("{id} is exploited in the wild (CISA)"));
    } else if let Some((id, _)) = worst
        .iter()
        .find(|(_, w)| w.exploitation.as_deref() == Some("poc"))
    {
        s.add(8, format!("{id} has a public exploit (CISA)"));
    }
    if let Some((id, e)) = worst
        .iter()
        .filter_map(|(id, w)| w.epss.map(|e| (id, e)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
    {
        let points = match e {
            e if e >= 0.1 => 10,
            e if e >= 0.01 => 4,
            _ => 0,
        };
        s.add(
            points,
            format!(
                "{id} has a {:.0}% chance of exploitation in 30 days (EPSS)",
                e * 100.0
            ),
        );
    }
    if let Some(f) = &p.facts {
        if f.insecure {
            s.add(15, "marked insecure: meta.knownVulnerabilities".into());
        }
        if f.broken {
            s.add(5, "marked broken".into());
        }
    }
    if let Some(u) = i.upstream {
        if major(u) != major(&p.version) {
            s.add(
                12,
                format!("{} is behind upstream {u}, a major release", p.version),
            );
        } else {
            s.add(8, format!("{} is behind upstream {u}", p.version));
        }
    }
    if p.maintainers.is_empty() && p.teams.is_empty() {
        s.add(6, "no maintainer".into());
    }
    if let Some(g) = i.hygiene.filter(|g| !g.is_empty()) {
        s.add((g.len() as u32).min(HYGIENE_CAP), g.join(", "));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::severity::Cve;

    #[test]
    fn the_reasons_add_up_to_the_risk() {
        let mut h = Health::default();
        h.cves.insert(
            "CVE-1".into(),
            Cve {
                band: Some(Band::High),
                epss: Some(0.2),
                exploitation: Some("active".into()),
                ..Default::default()
            },
        );
        let a = Advisory {
            code: "NIXPKGS-1".into(),
            cves: vec!["CVE-1".into()],
            title: String::new(),
            issue: None,
            ranges: vec![],
        };
        let p = Package {
            pname: "x".into(),
            version: "1.2".into(),
            ..Default::default()
        };
        let s = score(
            &h,
            &Input {
                package: &p,
                advisories: &[(&a, &Verdict::Affects)],
                osv: &[Worst::default()],
                upstream: Some("2.0"),
                hygiene: Some(&["no meta.homepage"]),
            },
        );
        // 25 + 5 advisories, 20 exploited, 10 EPSS, 12 major, 6 no maintainer, 1 hygiene
        assert_eq!(s.risk, 79);
        assert_eq!(s.reasons.iter().map(|r| r.points).sum::<u32>(), s.risk);
        assert_eq!(s.band(), "critical");
    }

    fn advisory_risk(v: Verdict) -> Score {
        let mut h = Health::default();
        h.cves.insert(
            "CVE-1".into(),
            Cve {
                band: Some(Band::Critical),
                ..Default::default()
            },
        );
        let a = Advisory {
            code: "NIXPKGS-2026-2615".into(),
            cves: vec!["CVE-1".into()],
            title: "unbound: security issues < 1.26.1".into(),
            issue: Some(564154),
            ranges: vec![],
        };
        let p = Package {
            pname: "unbound".into(),
            version: "1.26.1".into(),
            maintainers: vec!["alice".into()],
            ..Default::default()
        };
        score(
            &h,
            &Input {
                package: &p,
                advisories: &[(&a, &v)],
                osv: &[],
                upstream: None,
                hygiene: Some(&[]),
            },
        )
    }

    #[test]
    fn an_advisory_fixed_everywhere_scores_nothing() {
        let s = advisory_risk(Verdict::Fixed {
            fixed_in: Some("1.26.1".into()),
        });
        assert_eq!(s.risk, 0);
        assert!(s.reasons.is_empty());
    }

    #[test]
    fn an_advisory_fixed_only_in_master_scores_half_and_names_the_channel() {
        let s = advisory_risk(Verdict::Behind {
            fixed_in: Some("1.26.1".into()),
            channels: vec![crate::affected::Lag {
                channel: "nixos-26.05".into(),
                version: "1.25.0".into(),
                stable: true,
            }],
        });
        assert_eq!(s.risk, 20);
        assert_eq!(
            s.reasons[0].text,
            "1 advisory fixed in master, still in nixos-26.05"
        );
    }

    #[test]
    fn an_advisory_with_an_unknown_range_keeps_its_points() {
        let s = advisory_risk(Verdict::Unknown);
        assert_eq!(s.risk, 40);
        assert_eq!(s.reasons[0].text, "1 open advisory, range unknown");
        let s = advisory_risk(Verdict::Affects);
        assert_eq!(s.risk, 40);
        assert_eq!(s.reasons[0].text, "1 open advisory affects 1.26.1");
    }

    #[test]
    fn a_kept_package_scores_nothing() {
        let p = Package {
            version: "1".into(),
            maintainers: vec!["alice".into()],
            ..Default::default()
        };
        let s = score(
            &Health::default(),
            &Input {
                package: &p,
                advisories: &[],
                osv: &[],
                upstream: None,
                hygiene: Some(&[]),
            },
        );
        assert_eq!(s.risk, 0);
        assert_eq!(s.band(), "ok");
    }
}
