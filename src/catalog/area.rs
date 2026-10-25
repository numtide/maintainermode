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

pub(crate) fn view(v: &AreaView, actions: Html) -> Page {
    let path = v.path;
    let rows = &v.contents;
    let owners: Vec<Html> = v
        .owners
        .iter()
        .map(|(p, l)| html!("<a href=\"/a/{0}\">{0}</a> {1}", p, people_links(l)))
        .collect();
    let parent = path
        .rsplit_once('/')
        .map(|(p, _)| html!("<a href=\"/a/{0}\">{0}/</a> · ", p))
        .unwrap_or_default();
    let page = Page::new(Section::Packages, format!("{path}/"))
        .icon(octicon!("file-directory"))
        .sub(html!(
            "{parent}{} packages and modules · {} open pull requests",
            rows.len(),
            v.open
        ))
        .actions(actions);
    let mut out = Html::new();
    if !owners.is_empty() || !v.children.is_empty() {
        out.push(&Html::from("<div class=\"Box mb-4\">"));
        if !owners.is_empty() {
            out.push(&kv("ci/OWNERS", crate::html::join(owners, "<br>")));
        }
        if !v.children.is_empty() {
            let list = v.children.iter().take(40).map(|(d, n)| {
                html!(
                    "<a href=\"/a/{d}\">{}</a>&nbsp;<span class=\"color-fg-muted\">{n}</span>",
                    d.rsplit('/').next().unwrap_or(d)
                )
            });
            out.push(&kv(
                "Most open pull requests",
                crate::html::join(list, " · "),
            ));
        }
        out.push(&Html::from("</div>"));
    }
    if !rows.is_empty() {
        out.push(&crate::ui::section(
            "Packages and modules",
            Some(rows.len()),
            None,
            subject_rows(rows),
        ));
    }
    out.push(&v.subject.sections());
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
    let actions = subscribe_button(watching(&app, viewer.as_deref(), &subject), &subject);
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let page = view(&data(&s, &h, &index, &path), actions);
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
