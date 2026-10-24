//! What one login maintains: the Maintains tab of `/u/<login>` and the
//! `maintains` part of `/api/v1/people/<login>`.

use super::*;

/// A package or a module that names the login, directly or through a team.
#[derive(Serialize)]
pub(crate) struct MaintainedRow<'a> {
    #[serde(flatten)]
    row: SubjectRow,
    version: Option<&'a str>,
    via_team: Option<&'a str>,
    #[serde(serialize_with = "labels")]
    channels_behind: Vec<&'a Channel>,
    #[serde(skip)]
    sub: &'a Subject,
    #[serde(skip)]
    flags: Flags,
}

fn labels<S: serde::Serializer>(list: &[&Channel], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(list.iter().map(|c| c.label()))
}

#[derive(Serialize)]
pub(crate) struct MaintainsView<'a> {
    login: &'a str,
    rev: &'a str,
    packages: Vec<MaintainedRow<'a>>,
    modules: Vec<MaintainedRow<'a>>,
}

fn rows<'a>(
    s: &Snapshot,
    index: &'a Index,
    h: &Health,
    ch: &'a Channels,
    list: &'a [(Subject, Option<String>)],
) -> Vec<MaintainedRow<'a>> {
    let mut rows: Vec<MaintainedRow> = list
        .iter()
        .map(|(sub, how)| {
            let flags = Flags::of(s, index, h, sub);
            let version = match sub {
                Subject::Pkg(a) => index.pkgs.get(a).map(|p| p.version.as_str()),
                _ => None,
            };
            let channels_behind = match (sub, version) {
                (Subject::Pkg(a), Some(v)) => ch.behind(a, v),
                _ => Vec::new(),
            };
            MaintainedRow {
                row: SubjectRow::new(sub, &flags),
                version,
                via_team: how.as_deref(),
                channels_behind,
                sub,
                flags,
            }
        })
        .collect();
    rows.sort_by_key(|r| (r.flags.rank(), r.sub.label()));
    rows
}

pub(crate) fn data<'a>(
    s: &Snapshot,
    index: &'a Index,
    h: &Health,
    ch: &'a Channels,
    login: &'a str,
    m: &'a Maintained,
) -> MaintainsView<'a> {
    MaintainsView {
        login,
        rev: &index.rev,
        packages: rows(s, index, h, ch, &m.packages),
        modules: rows(s, index, h, ch, &m.modules),
    }
}

fn rows_html(rows: &[MaintainedRow]) -> String {
    let mut out = String::new();
    for r in rows {
        let version = match (r.sub, r.version) {
            (Subject::Pkg(a), Some(v)) => format!(
                " <span class=\"color-fg-muted f6\">{}</span>{}",
                esc(v),
                lag_of(&r.channels_behind, a, v)
            ),
            _ => String::new(),
        };
        let via = r
            .via_team
            .map(|t| format!(" <span class=\"Label\">via {}</span>", esc(t)))
            .unwrap_or_default();
        out.push_str(&subject_row(
            r.sub,
            &r.flags,
            &format!("{version}{via}"),
            "",
        ));
    }
    out
}

pub(crate) fn view(v: &MaintainsView) -> String {
    let mut out = format!(
        "<p class=\"f6 color-fg-muted\">Packages and NixOS modules whose <code>meta.maintainers</code> or <code>meta.teams</code> name {0}, \
         at nixpkgs {1}. Change the list in nixpkgs, not here.</p>",
        esc(v.login),
        esc(if v.rev.is_empty() {
            "(index pending)"
        } else {
            &v.rev[..v.rev.len().min(12)]
        })
    );
    out.push_str(&crate::ui::section(
        format!("{} Packages", icon(octicon!("package"))),
        Some(v.packages.len()),
        None,
        rows_html(&v.packages),
    ));
    out.push_str(&crate::ui::section(
        format!("{} NixOS modules", icon(octicon!("gear"))),
        Some(v.modules.len()),
        None,
        rows_html(&v.modules),
    ));
    out
}

/// The Maintains tab of `/u/<login>`.
pub(crate) fn body(app: &crate::web::App, login: &str) -> String {
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let ch = app.channels.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let m = maintained(&index, login);
    view(&data(&s, &index, &h, &ch, login, &m))
}

pub(crate) fn value(app: &crate::web::App, login: &str) -> Value {
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let ch = app.channels.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let m = maintained(&index, login);
    json!(data(&s, &index, &h, &ch, login, &m))
}
