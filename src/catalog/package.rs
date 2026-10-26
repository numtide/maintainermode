//! The page of one package: its health, its builds, its advisories, and
//! the pull requests and issues attached to it.

use super::*;
use crate::area::Package;
use crate::builds::Jobset;

/// The last build of a package on one jobset.
#[derive(Clone)]
pub(crate) struct HydraBuild {
    pub(crate) jobset: Jobset,
    pub(crate) job: String,
    pub(crate) last: Last,
    /// Hydra no longer builds the job.
    pub(crate) dead: bool,
}

impl Serialize for HydraBuild {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let b = self.last.build();
        json!({
            "channel": self.jobset.label(), "jobset": self.jobset.key(), "job": self.job,
            "state": self.last.state(), "build": b, "dead_job": self.dead,
            "passed": b.map(|b| crate::builds::passed(&b.history).0),
            "counted": b.map(|b| crate::builds::passed(&b.history).1),
        })
        .serialize(s)
    }
}

/// The last build of a package on master and on each channel's jobset.
/// A page view passes `page`, so a slow Hydra does not hold the page.
async fn channel_builds(
    app: &crate::web::App,
    attr: &str,
    page: Option<&Shared>,
) -> Vec<HydraBuild> {
    let jobsets = app.hydra.builds.jobsets();
    let got = futures_util::future::join_all(jobsets.iter().map(|j| async {
        let job = j.path(attr);
        let last = match page {
            Some(shared) => crate::health::Hydra::last_for_page(shared, &job).await,
            None => app.hydra.last(&job).await,
        };
        (j.clone(), job, last)
    }))
    .await;
    let now = chrono::Utc::now();
    got.into_iter()
        .map(|(jobset, job, last)| {
            let version = version_in(app, &jobset, attr);
            let dead = last
                .build()
                .is_some_and(|b| crate::builds::dead(&b.history, version.as_deref(), now));
            HydraBuild {
                jobset,
                job,
                last,
                dead,
            }
        })
        .collect()
}

/// The version of `attr` in the nixpkgs that a jobset builds: master's
/// index, or the channel's.
fn version_in(app: &crate::web::App, j: &crate::builds::Jobset, attr: &str) -> Option<String> {
    if j.key() == crate::builds::Jobset::master().key() {
        return app
            .index
            .read()
            .unwrap()
            .pkgs
            .get(attr)
            .map(|p| p.version.clone());
    }
    app.channels
        .read()
        .unwrap()
        .list
        .iter()
        .find(|c| crate::builds::Jobset::of_channel(&c.name).is_some_and(|x| x.key() == j.key()))
        .and_then(|c| c.versions.get(attr).cloned())
}

#[derive(Clone, Serialize)]
pub(crate) struct CacheRow {
    pub(crate) channel: String,
    pub(crate) rev: String,
    #[serde(rename = "out_path")]
    pub(crate) path: String,
    /// None until the server asked cache.nixos.org.
    pub(crate) cached: Option<bool>,
}

/// Whether each channel's output of `attr` is in cache.nixos.org. The
/// path does not depend on the attribute name, so it works for an alias
/// too. Only packages that someone maintains or watches have a path.
pub(crate) fn binary_cache(app: &crate::web::App, attr: &str) -> Vec<CacheRow> {
    let channels = app.channels.read().unwrap().clone();
    channels
        .list
        .iter()
        .filter_map(|c| {
            let path = c.out_paths.get(attr)?;
            Some(CacheRow {
                channel: c.label().to_string(),
                rev: c.rev.clone(),
                path: path.clone(),
                cached: app.hydra.builds.cached(path),
            })
        })
        .collect()
}

fn cache_html(rows: &[CacheRow]) -> Html {
    crate::html::join(
        rows.iter().map(|c| {
            let (class, word) = match c.cached {
                Some(true) => ("color-fg-success", "built"),
                Some(false) => ("color-fg-attention", "not in the cache"),
                None => ("color-fg-muted", "not asked yet"),
            };
            html!(
                "{} <span class=\"{class}\" title=\"{}\">{word}</span>",
                c.channel,
                c.path
            )
        }),
        " · ",
    )
}

fn dead_row(label: &str, job: &str, last: &Last) -> Html {
    let since = last
        .build()
        .and_then(|b| b.history.iter().filter_map(|p| p.at).max())
        .map(|t| t.format("%B %Y").to_string())
        .unwrap_or_default();
    kv(
        Html::text(label),
        html!(
            "<span class=\"color-fg-muted\">Hydra stopped building the job <a href=\"{}\">{job}</a>: its last build is from {since}. nixpkgs builds this package under another job name.</span>",
            Hydra::job_url(job)
        ),
    )
}

/// The last builds as a strip of cells, the oldest first, and how many
/// passed.
fn history_strip(job: &str, h: &[crate::builds::Past]) -> Html {
    if h.len() < 2 {
        return Html::new();
    }
    let (ok, told) = crate::builds::passed(h);
    let cells: Html = h
        .iter()
        .rev()
        .map(|p| {
            let class = match p.state {
                Some("passes") => "ok",
                None => "skip",
                Some(_) => "bad",
            };
            let when =
                p.at.map(|t| format!(", queued {} ago", ago(t)))
                    .unwrap_or_default();
            html!(
                "<span class=\"build-cell build-{class}\" title=\"{}{when}\"></span>",
                p.status
            )
        })
        .collect();
    html!(
        "<div class=\"mt-1\"><a class=\"build-strip\" href=\"{}\" aria-label=\"{ok} of {told} of the last builds passed: open the job on Hydra\">{cells}</a> \
         <span class=\"f6 color-fg-muted text-nowrap\">{ok} of {told} passed</span></div>",
        Hydra::job_url(job)
    )
}

pub(super) fn build_row(label: &str, job: &str, last: &Last) -> Html {
    let link = html!("<a href=\"{}\">{job}</a>", Hydra::job_url(job));
    let body = match last {
        Last::Build(b) => html!(
            "<a href=\"{}\" class=\"{}\">{}</a> <span class=\"color-fg-muted\">{}</span>{}",
            b.url(),
            if b.ok {
                "color-fg-success"
            } else {
                "color-fg-danger"
            },
            b.status,
            b.at.map(|t| format!("queued {} ago", ago(t)))
                .unwrap_or_default(),
            history_strip(job, &b.history)
        ),
        Last::Missing => {
            html!("<span class=\"color-fg-muted\">no finished build of {link}</span>")
        }
        Last::NoAnswer => {
            html!("<span class=\"color-fg-muted\">Hydra did not answer; see {link}</span>")
        }
        Last::Asking => html!(
            "<span class=\"color-fg-muted\">Hydra has not answered yet: reload in a few seconds, or see {link}</span>"
        ),
        Last::NotAsked => html!(
            "<span class=\"color-fg-muted\">not asked yet: this server asks Hydra a few times a minute; see {link}</span>"
        ),
    };
    kv(Html::text(label), body)
}

fn severity_html(w: &Worst) -> Html {
    let mut out = Html::new();
    if let Some(b) = w.band {
        out.push(&html!(
            " <span class=\"Label {}\" title=\"CVSS base score{}\">{}{}</span>",
            b.label_class(),
            match w.from.as_deref() {
                Some("nvd") => ", from NVD",
                Some("cve") => ", from the CVE record",
                _ => "",
            },
            b.word(),
            w.cvss.map(|s| format!(" {s:.1}")).unwrap_or_default()
        ));
    }
    match w.exploitation.as_deref() {
        Some("active") => out.push(&Html::from(
            " <span class=\"Label Label--danger\" title=\"CISA SSVC\">exploited in the wild</span>",
        )),
        Some("poc") => out.push(&Html::from(
            " <span class=\"Label Label--attention\" title=\"CISA SSVC\">public exploit</span>",
        )),
        _ => {}
    }
    if let Some(e) = w.epss {
        out.push(&html!(
            " <span class=\"Label\" title=\"chance of exploitation in the next 30 days (EPSS){}\">EPSS {:.1}%</span>",
            w.percentile
                .map(|p| format!(", higher than {:.0}% of all CVEs", p * 100.0))
                .unwrap_or_default(),
            e * 100.0
        ));
    }
    out
}

/// The tracker's advisories with the worst severity of their CVEs, the
/// worst first.
pub(super) fn rated<'a>(h: &Health, list: &[&'a Advisory]) -> Vec<(&'a Advisory, Worst)> {
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

fn packaging_html(attr: &str, p: &crate::area::Package) -> Html {
    let Some(gaps) = hygiene::gaps(attr, p) else {
        return "<span class=\"color-fg-muted f6\">not read yet: the next index update reads it</span>"
            .into();
    };
    let mut out: Vec<Html> = Vec::new();
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
            .map(|g| html!("<span class=\"Label Label--attention\">{g}</span>")),
    );
    if out.is_empty() {
        return "<span class=\"color-fg-success\">meta complete</span>".into();
    }
    crate::html::join(out, " ")
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
pub(super) fn closed_advisories<'a>(h: &'a Health, s: &Snapshot, attr: &str) -> Vec<&'a Advisory> {
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

pub(super) fn channel_names(list: &[&Channel]) -> String {
    list.iter()
        .map(|c| c.label())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn hover(list: &[&Channel], attr: &str, master: &str) -> String {
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

fn backport_label(backport: &[&Channel], attr: &str, master: &str) -> Html {
    if backport.is_empty() {
        return Html::new();
    }
    html!(
        " <span class=\"Label Label--attention\" title=\"{}\">needs backport to {}</span>",
        hover(backport, attr, master),
        channel_names(backport)
    )
}

pub(super) fn channels_html(ch: &Channels, attr: &str, master: &str) -> Html {
    let seen = ch.of(attr, master);
    if seen.is_empty() {
        return "<span class=\"color-fg-muted\">no channel read yet</span>".into();
    }
    let mut parts = vec![html!("master {master}")];
    for s in &seen {
        parts.push(html!(
            "<a href=\"{}\">{}</a> {}",
            s.channel.url(),
            s.channel.label(),
            match s.version {
                Some(v) if s.behind => html!("<span class=\"color-fg-attention\">{v}</span>"),
                Some(v) => Html::text(v),
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
        Html::from(" <span class=\"color-fg-muted f6\">no channel is behind master</span>")
    } else {
        html!(
            " <span class=\"Label Label--attention\">master is ahead of {}</span>",
            channel_names(&behind)
        )
    };
    html!("{}{tail}", crate::html::join(parts, " · "))
}

fn issue_link(a: &Advisory) -> Html {
    a.issue_url()
        .map(|u| html!(" · <a href=\"{u}\">#{}</a>", a.issue.unwrap_or_default()))
        .unwrap_or_default()
}

/// What a verdict asks of a maintainer, as one line.
fn verdict_html(a: &Advisory, v: &Verdict, master: &str) -> Html {
    match v {
        Verdict::Affects => html!("<span class=\"color-fg-danger\">affects {master}</span>"),
        Verdict::Unknown => "<span class=\"color-fg-attention\" title=\"The tracker gives no affected version that Maintainer Mode can read\">range unknown</span>".into(),
        Verdict::Behind { fixed_in, channels } => {
            let fixed = fixed_in
                .as_deref()
                .map_or_else(|| "fixed in master".to_string(), |f| format!("fixed in {f} in master"));
            let lags = channels.iter().map(|l| {
                let what = format!("{} has {}", l.channel, l.version);
                if l.stable {
                    html!("<span class=\"Label Label--attention\">needs backport: {what}</span>")
                } else {
                    html!("<span class=\"color-fg-attention\">{what}</span>")
                }
            });
            html!(
                "<span class=\"color-fg-success\">{fixed}</span> · {}",
                crate::html::join(lags, " · ")
            )
        }
        Verdict::Fixed { fixed_in } => {
            let fixed = fixed_in
                .as_deref()
                .map_or_else(|| format!("not affected: master has {master}"), |f| format!("fixed in {f}"));
            let close = match (a.issue, a.issue_url()) {
                (Some(n), Some(u)) => {
                    html!("<a href=\"{u}\">the tracker issue #{n} can be closed</a>")
                }
                _ => html!(
                    "<a href=\"{}\">the tracker advisory can be closed</a>",
                    a.url()
                ),
            };
            html!("<span class=\"color-fg-success\">{fixed}</span> · {close}")
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

pub(super) fn advisories_html(
    h: &Health,
    list: &[(&Advisory, &Verdict)],
    osv: &[(&str, Option<&crate::osv::Vuln>)],
    fixed: &[&Advisory],
    backport: &[&Channel],
    attr: &str,
    master: &str,
) -> Html {
    let hint = backport_label(backport, attr, master);
    let fixed: Vec<&&Advisory> = fixed.iter().filter(|_| !hint.is_empty()).collect();
    if list.is_empty() && osv.is_empty() && fixed.is_empty() {
        return "<span class=\"color-fg-success\">none open</span>".into();
    }
    let fixed_html: Html = fixed
        .iter()
        .map(|a| {
            html!(
                "<div><a href=\"{}\">{}</a> {} <span class=\"color-fg-muted f6\">fixed in master</span>{hint}</div>",
                a.url(),
                a.code,
                a.title,
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
        let class = Html::from(if v.open() { " class=\"color-fg-danger\"" } else { "" });
        let cves = if a.cves.is_empty() {
            Html::new()
        } else {
            html!("{} · ", a.cves.join(", "))
        };
        html!(
            "<div><a{class} href=\"{}\">{}</a>{} {}{}<div class=\"f6 color-fg-muted\">{cves}{}</div></div>",
            a.url(),
            a.code,
            severity_html(&w),
            a.title,
            issue_link(a),
            verdict_html(a, v, master)
        )
    });
    let osv = osv.iter().map(|(id, v)| {
        html!(
            "<div><a class=\"color-fg-danger\" href=\"{}\">{id}</a>{} {} <span class=\"f6 color-fg-muted\">(OSV, for this version)</span><div class=\"f6 color-fg-muted\">{}</div></div>",
            crate::osv::Vuln::url(id),
            severity_html(&osv_worst(h, *v)),
            v.and_then(|v| v.summary.as_deref()).unwrap_or(""),
            v.map(|v| v.cves.join(", ")).unwrap_or_default()
        )
    });
    tracker
        .chain(osv)
        .chain(std::iter::once(fixed_html))
        .collect()
}

/// A link only for http and https: `meta` is nixpkgs code, and a
/// `javascript:` homepage must not run on this site.
fn url_html(u: &str) -> Html {
    if u.starts_with("https://") || u.starts_with("http://") {
        html!("<a href=\"{0}\">{0}</a>", u)
    } else {
        Html::text(u)
    }
}

fn urls_html(list: &[String]) -> Html {
    crate::html::join(list.iter().map(|u| url_html(u)), "<br>")
}

fn license_html(l: &License) -> Html {
    let name = match (&l.spdx, &l.name) {
        (Some(id), _) => html!("<a href=\"https://spdx.org/licenses/{0}.html\">{0}</a>", id),
        (None, Some(n)) => Html::text(n),
        (None, None) => "unnamed".into(),
    };
    if l.free || l.name.as_deref() == Some("unfree") {
        name
    } else {
        html!("{name} <span class=\"Label Label--attention\">unfree</span>")
    }
}

fn platforms_html(m: &Meta) -> Html {
    let mut out = match &m.platforms {
        None => "<span class=\"color-fg-muted\">not set</span>".into(),
        Some(p) => {
            let mut kinds: Vec<&str> = Vec::new();
            if p.linux {
                kinds.push("Linux");
            }
            if p.darwin {
                kinds.push("Darwin");
            }
            html!(
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
        out.push(&html!(
            " · not on {}",
            crate::html::join(
                m.bad_platforms.iter().map(|p| html!("<code>{p}</code>")),
                ", "
            )
        ));
    }
    out
}

fn codes(list: &[String]) -> Html {
    crate::html::join(list.iter().map(|c| html!("<code>{c}</code>")), " ")
}

fn status_html(f: Option<&crate::attrs::Facts>, m: &Meta) -> Html {
    let mut out: Vec<Html> = Vec::new();
    if f.is_some_and(|f| f.broken) {
        out.push("<span class=\"Label Label--danger\">broken</span>".into());
    }
    if !m.known_vulnerabilities.is_empty() {
        out.push(html!(
            "<span class=\"Label Label--danger\">insecure</span><ul class=\"f6 ml-3\">{}</ul>",
            m.known_vulnerabilities
                .iter()
                .map(|v| html!("<li class=\"pre-wrap\">{}</li>", v.trim()))
                .collect::<Html>()
        ));
    }
    if m.unfree() {
        out.push("<span class=\"Label Label--attention\">unfree</span>".into());
    }
    if out.is_empty() {
        return "<span class=\"color-fg-success\">not broken, not insecure, free</span>".into();
    }
    crate::html::join(out, " ")
}

/// The "Package" box: what nixpkgs says about the package, as the sweep read it.
pub(super) fn package_box(index: &Index, rev: &str, p: &crate::area::Package) -> Html {
    let mut out = html!(
        "<div class=\"Box mb-4\"><div class=\"Box-header d-flex\"><span class=\"text-bold flex-auto\">Package</span>\
         <a class=\"f6\" href=\"{META_GUIDE}\">what each field means</a></div>"
    );
    let description = p.facts.as_ref().and_then(|f| f.description.as_deref());
    out.push(&kv(
        "Description",
        description
            .map(Html::text)
            .unwrap_or_else(|| "<span class=\"color-fg-muted\">none</span>".into()),
    ));
    match &p.meta {
        None => out.push(&kv(
            "meta",
            "<span class=\"color-fg-muted f6\">not read yet: the next index update reads it</span>",
        )),
        Some(m) => {
            if let Some(d) = &m.long_description {
                out.push(&kv(
                    "Long description",
                    html!("<div class=\"f6 pre-wrap\">{}</div>", d.trim_end()),
                ));
            }
            let none = "<span class=\"color-fg-muted\">none</span>";
            out.push(&kv(
                "Homepage",
                if m.homepage.is_empty() {
                    none.into()
                } else {
                    urls_html(&m.homepage)
                },
            ));
            if let Some(d) = &m.download_page {
                out.push(&kv("Download page", url_html(d)));
            }
            if !m.changelog.is_empty() {
                out.push(&kv("Changelog", urls_html(&m.changelog)));
            }
            out.push(&kv(
                "License",
                if m.licenses.is_empty() {
                    none.into()
                } else {
                    crate::html::join(m.licenses.iter().map(license_html), ", ")
                },
            ));
            out.push(&kv("Platforms", platforms_html(m)));
            if let Some(b) = &m.main_program {
                out.push(&kv("Main program", html!("<code>{b}</code>")));
            }
            out.push(&kv(
                "Source provenance",
                if m.source_provenance.is_empty() {
                    "<span class=\"color-fg-muted\">not set: built from source</span>".into()
                } else {
                    codes(&m.source_provenance)
                },
            ));
            if !m.outputs.is_empty() {
                out.push(&kv("Outputs", codes(&m.outputs)));
            }
            out.push(&kv("Status", status_html(p.facts.as_ref(), m)));
            if let Some((file, line)) = m.file_line() {
                let r = if rev.is_empty() { "master" } else { rev };
                out.push(&kv(
                    "Position",
                    html!(
                        "<a class=\"branch-name\" href=\"https://github.com/NixOS/nixpkgs/blob/{r}/{file}#L{line}\">{file}:{line}</a>"
                    ),
                ));
            }
        }
    }
    out.push(&kv("Maintainers", people_links(&p.maintainers)));
    if !p.teams.is_empty() {
        out.push(&kv("Teams", teams_html(index, &p.teams)));
    }
    out.push(&Html::from("</div>"));
    out
}

/// The snapshots a package page reads. None of them is the store.
pub(crate) struct Inputs<'a> {
    pub(crate) s: &'a Snapshot,
    pub(crate) h: &'a Health,
    pub(crate) ch: &'a Channels,
    pub(crate) index: &'a Index,
}

/// An open advisory as the API gives it.
#[derive(Serialize)]
pub(crate) struct Tracked<'a> {
    #[serde(flatten)]
    advisory: &'a Advisory,
    severity: Worst,
    verdict: &'a Verdict,
}

/// An OSV record that no advisory of the tracker covers.
#[derive(Serialize)]
pub(crate) struct OsvRow<'a> {
    id: &'a str,
    url: String,
    summary: Option<&'a str>,
    cves: Vec<String>,
    severity: Worst,
}

/// The version of the package in one channel.
#[derive(Serialize)]
pub(crate) struct ChannelRow<'a> {
    channel: &'a str,
    rev: &'a str,
    read_at: Option<DateTime<Utc>>,
    version: Option<&'a str>,
    behind_master: bool,
}

/// A stable channel that needs advisory fixes that master has.
#[derive(Serialize)]
pub(crate) struct BackportRow<'a> {
    channel: &'a str,
    version: Option<&'a String>,
    advisories: Vec<&'a String>,
}

/// The builds of a package: master first, then one per channel jobset.
/// The API gives the first as `hydra`, `hydra_state` and `hydra_job`.
pub(crate) struct Builds(pub(crate) Vec<HydraBuild>);

impl Serialize for Builds {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let first = self.0.first();
        let mut m = s.serialize_map(Some(4))?;
        m.serialize_entry("hydra", &first.and_then(|b| b.last.build()))?;
        m.serialize_entry("hydra_state", &first.map(|b| b.last.state()))?;
        m.serialize_entry("hydra_job", &first.map(|b| &b.job))?;
        m.serialize_entry("hydra_channels", &self.0)?;
        m.end()
    }
}

/// What `/p/<attr>` shows and `/api/v1/packages/<name>` gives.
#[derive(Serialize)]
pub(crate) struct PackageView<'a> {
    #[serde(flatten)]
    pub(crate) subject: SubjectView<'a>,
    pub(crate) attr: &'a str,
    pub(crate) package: Option<&'a Package>,
    #[serde(flatten)]
    pub(crate) builds: Builds,
    pub(crate) binary_cache: Vec<CacheRow>,
    pub(crate) advisories: Vec<Tracked<'a>>,
    pub(crate) osv: Vec<OsvRow<'a>>,
    pub(crate) packaging_gaps: Option<Vec<&'static str>>,
    pub(crate) channels: Vec<ChannelRow<'a>>,
    pub(crate) needs_backport: Vec<BackportRow<'a>>,
    pub(crate) version_bumps: Vec<u64>,
    #[serde(skip)]
    open: Vec<(&'a Advisory, &'a Verdict)>,
    #[serde(skip)]
    osv_extra: Vec<(&'a str, Option<&'a crate::osv::Vuln>)>,
    #[serde(skip)]
    closed: Vec<&'a Advisory>,
    #[serde(skip)]
    backport: Vec<&'a Channel>,
}

pub(crate) fn data<'a>(
    i: &Inputs<'a>,
    attr: &'a str,
    builds: Vec<HydraBuild>,
    binary_cache: Vec<CacheRow>,
) -> PackageView<'a> {
    let (s, h, ch) = (i.s, i.h, i.ch);
    let package = i.index.pkgs.get(attr);
    let master = package.map_or("", |p| p.version.as_str());
    let subject = SubjectView::of(s, i.index, h, &Subject::Pkg(attr.to_string()));
    let open = judged(h, s, attr);
    let closed = closed_advisories(h, s, attr);
    let backport = ch.backport(attr, master);
    let closed_codes: Vec<&String> = closed.iter().map(|a| &a.code).collect();
    let needs_backport = ch
        .list
        .iter()
        .filter(|c| c.stable() && !c.rev.is_empty())
        .filter_map(|c| {
            let mut codes: Vec<&String> = if backport.iter().any(|b| b.key == c.key) {
                closed_codes.clone()
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
            (!codes.is_empty()).then(|| BackportRow {
                channel: c.label(),
                version: c.versions.get(attr),
                advisories: codes,
            })
        })
        .collect();
    let channels = ch
        .of(attr, master)
        .iter()
        .map(|seen| ChannelRow {
            channel: seen.channel.label(),
            rev: &seen.channel.rev,
            read_at: seen.channel.read_at,
            version: seen.version,
            behind_master: seen.behind,
        })
        .collect();
    let list: Vec<&Advisory> = open.iter().map(|(a, _)| *a).collect();
    let advisories = rated(h, &list)
        .into_iter()
        .map(|(a, severity)| Tracked {
            advisory: a,
            severity,
            verdict: h.verdict(attr, &a.code),
        })
        .collect();
    let osv_extra = h.osv_extra(attr);
    let osv = osv_extra
        .iter()
        .map(|&(id, v)| OsvRow {
            id,
            url: crate::osv::Vuln::url(id),
            summary: v.and_then(|v| v.summary.as_deref()),
            cves: v.map(|v| v.cves.clone()).unwrap_or_default(),
            severity: osv_worst(h, v),
        })
        .collect();
    let pname = package.map_or("", |p| p.pname.as_str());
    let version_bumps = crate::bumps::of_package(attr, pname, &subject.prs)
        .iter()
        .map(|p| p.number)
        .collect();
    PackageView {
        attr,
        package,
        builds: Builds(builds),
        binary_cache,
        advisories,
        osv,
        packaging_gaps: package.and_then(|p| hygiene::gaps(attr, p)),
        channels,
        needs_backport,
        version_bumps,
        open,
        osv_extra,
        closed,
        backport,
        subject,
    }
}

/// What a page of a package shows only to the person who looks at it.
pub(crate) struct Personal {
    /// Watch, then Next.
    pub(crate) actions: Html,
}

pub(crate) fn view(i: &Inputs, v: &PackageView, personal: Personal) -> Page {
    let (s, h, ch, index) = (i.s, i.h, i.ch, i.index);
    let attr = v.attr;
    let default = Package::default();
    let p = v.package.unwrap_or(&default);
    let page = Page::new(Section::Packages, attr)
        .icon(octicon!("package"))
        .sub(html!(
            "{} {}{}",
            p.pname,
            p.version,
            p.file
                .as_deref()
                .map(|f| html!(" · {}", source_link(&index.rev, f)))
                .unwrap_or_default()
        ))
        .actions(personal.actions);
    let mut out = Html::new();
    let mut health = Html::new();
    if let Some(sc) = score_of(s, index, h, attr) {
        let reasons: Html = sc
            .reasons
            .iter()
            .map(|r| html!("<li>+{} {}</li>", r.points, r.text))
            .collect();
        health.push(&kv(
            "Risk",
            html!(
                "{} {}{}",
                score_label(&sc),
                sc.band(),
                if reasons.is_empty() {
                    Html::new()
                } else {
                    html!("<ul class=\"f6 color-fg-muted ml-3\">{reasons}</ul>")
                }
            ),
        ));
    }
    let version = match upstream_from(s, index, h, attr) {
        Some((u, from)) => html!(
            "{} <span class=\"Label Label--attention\" title=\"Upstream released a newer version, from {from}\">{u} available</span>",
            p.version
        ),
        None => html!(
            "{} <span class=\"color-fg-muted f6\">{}</span>",
            p.version,
            match h.direct.get(attr) {
                Some(d) if d.latest.is_some() => format!(
                    "no newer release in {}",
                    if d.from == "tags" {
                        "its git tags"
                    } else {
                        d.from.as_str()
                    }
                ),
                _ if h.repology_at.is_some() => "no newer release known to repology".into(),
                _ => "no upstream check yet".into(),
            }
        ),
    };
    health.push(&kv("Version", version));
    health.push(&kv("Channels", channels_html(ch, attr, &p.version)));
    for b in &v.builds.0 {
        let label = if b.jobset.key() == Jobset::master().key() {
            "Hydra".to_string()
        } else {
            format!("Hydra, {}", b.jobset.label())
        };
        health.push(&if b.dead {
            dead_row(&label, &b.job, &b.last)
        } else {
            build_row(&label, &b.job, &b.last)
        });
    }
    if !v.binary_cache.is_empty() {
        health.push(&kv("Binary cache", cache_html(&v.binary_cache)));
    }
    health.push(&kv(
        "Advisories",
        advisories_html(
            h,
            &v.open,
            &v.osv_extra,
            &v.closed,
            &v.backport,
            attr,
            &p.version,
        ),
    ));
    health.push(&kv("Packaging", packaging_html(attr, p)));
    out.push(&crate::ui::section("Health", None, None, health));
    out.push(&package_box(index, &index.rev, p));
    out.push(&crate::bumps::package_section(
        s,
        index,
        h,
        attr,
        &v.subject.prs,
    ));
    out.push(&v.subject.sections());
    page.body(out)
}

pub(super) async fn page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    let viewer = app.viewer(&headers);
    let resolved = app.index.read().unwrap().resolve(&name).map(String::from);
    let Some(attr) = resolved else {
        return not_found(&app, viewer.as_deref(), &name);
    };
    if attr != name {
        return Redirect::to(&format!("/p/{attr}")).into_response();
    }
    let subject = Subject::Pkg(attr.clone());
    let builds = channel_builds(&app, &attr, Some(&app)).await;
    let cache = binary_cache(&app, &attr);
    let next = crate::needs::here(&app, &headers, &subject.key());
    let watch = subscribe_button(watching(&app, viewer.as_deref(), &subject), &subject);
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let ch = app.channels.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let personal = Personal {
        actions: html!("{watch}{next}"),
    };
    let i = Inputs {
        s: &s,
        h: &h,
        ch: &ch,
        index: &index,
    };
    let page = view(&i, &data(&i, &attr, builds, cache), personal);
    drop(index);
    page.render(&app, viewer.as_deref())
}

pub(crate) async fn value(app: &crate::web::App, name: &str) -> Option<Value> {
    let attr = app.index.read().unwrap().resolve(name)?.to_string();
    let builds = channel_builds(app, &attr, None).await;
    let cache = binary_cache(app, &attr);
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let ch = app.channels.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let i = Inputs {
        s: &s,
        h: &h,
        ch: &ch,
        index: &index,
    };
    Some(json!(data(&i, &attr, builds, cache)))
}
