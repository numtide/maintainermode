//! The page of one directory of nixpkgs: who owns it, its busiest
//! subdirectories, the packages and modules under it, and the pull
//! requests and issues that touch it.

use super::*;

fn subject_rows_of<S: serde::Serializer>(
    rows: &[(Subject, Flags)],
    s: S,
) -> Result<S::Ok, S::Error> {
    s.collect_seq(rows.iter().map(|(sub, f)| SubjectRow::new(sub, f)))
}

/// What `/a/<path>` shows and `/api/v1/areas/<path>` gives.
#[derive(Serialize)]
pub(crate) struct AreaView<'a> {
    #[serde(flatten)]
    pub(crate) subject: SubjectView<'a>,
    #[serde(serialize_with = "subject_rows_of")]
    pub(crate) contents: Vec<(Subject, Flags)>,
    #[serde(skip)]
    path: &'a str,
    /// The `ci/OWNERS` lines above or under the path.
    #[serde(skip)]
    owners: Vec<&'a (String, Vec<String>)>,
    /// The direct subdirectories, the most open pull requests first.
    #[serde(skip)]
    children: Vec<(&'a String, usize)>,
    #[serde(skip)]
    open: usize,
}

/// `path` has no leading or trailing slash.
pub(crate) fn data<'a>(
    s: &'a Snapshot,
    h: &Health,
    index: &'a Index,
    path: &'a str,
) -> AreaView<'a> {
    let owners = index
        .owners
        .iter()
        .filter(|(p, _)| under(path, p) || under(p, path))
        .collect();
    let prefix = format!("{path}/");
    let mut children: Vec<(&String, usize)> = s
        .open_by_dir
        .iter()
        .filter(|(d, _)| d.strip_prefix(&prefix).is_some_and(|r| !r.contains('/')))
        .map(|(d, n)| (d, *n))
        .collect();
    children.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    AreaView {
        subject: SubjectView::of(s, index, h, &Subject::Area(path.to_string())),
        contents: contents(index, s, h, path),
        path,
        owners,
        children,
        open: s.open_by_dir.get(path).copied().unwrap_or(0),
    }
}

pub(crate) fn view(v: &AreaView, actions: Html, act: Act) -> Page {
    let path = v.path;
    let rows = &v.contents;
    let owners: Vec<String> = v
        .owners
        .iter()
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
            v.open
        ))
        .actions(actions);
    let mut out = String::new();
    if !owners.is_empty() || !v.children.is_empty() {
        out.push_str("<div class=\"Box mb-4\">");
        if !owners.is_empty() {
            out.push_str(&kv("ci/OWNERS", owners.join("<br>")));
        }
        if !v.children.is_empty() {
            let list: Vec<String> = v
                .children
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
            out.push_str(&kv("Most open pull requests", list.join(" · ")));
        }
        out.push_str("</div>");
    }
    if !rows.is_empty() {
        out.push_str(&crate::ui::section(
            "Packages and modules",
            Some(rows.len()),
            None,
            subject_rows(rows),
        ));
    }
    out.push_str(&v.subject.sections(act));
    page.body(out)
}

pub(super) async fn page(
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
    let actions = Html::from(subscribe_button(
        watching(&app, viewer.as_deref(), &subject),
        &subject,
    ));
    let act = Act::of(&app, viewer.as_deref());
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let page = view(&data(&s, &h, &index, &path), actions, act);
    drop(index);
    page.render(&app, viewer.as_deref())
}

pub(crate) fn value(app: &crate::web::App, path: &str) -> Option<Value> {
    let path = path.trim_matches('/');
    if !valid_path(path) {
        return None;
    }
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    Some(json!(data(&s, &h, &index, path)))
}
