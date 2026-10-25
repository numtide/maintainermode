//! The page of one NixOS module: its test, its maintainers, and the pull
//! requests and issues attached to it.

use super::*;
use crate::area::Module;

/// The last build of the NixOS test of a module. The API gives it as
/// `test`, `test_state` and `test_job`.
pub(crate) struct Test {
    pub(crate) job: String,
    pub(crate) last: Last,
}

impl Serialize for Test {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(3))?;
        m.serialize_entry("test", &self.last.build())?;
        m.serialize_entry("test_state", self.last.state())?;
        m.serialize_entry("test_job", &self.job)?;
        m.end()
    }
}

/// What `/m/<file>` shows and `/api/v1/modules/<file>` gives.
#[derive(Serialize)]
pub(crate) struct ModuleView<'a> {
    #[serde(flatten)]
    pub(crate) subject: SubjectView<'a>,
    pub(crate) file: &'a str,
    pub(crate) module: &'a Module,
    #[serde(flatten)]
    pub(crate) test: Test,
}

/// The job of the NixOS test of a module file.
fn test_job(file: &str) -> String {
    Hydra::test_job(module_name(file).trim_start_matches("nixos/"))
}

pub(crate) fn data<'a>(
    s: &'a Snapshot,
    h: &Health,
    index: &'a Index,
    file: &'a str,
    module: &'a Module,
    last: Last,
) -> ModuleView<'a> {
    ModuleView {
        subject: SubjectView::of(s, index, h, &Subject::Module(file.to_string())),
        file,
        module,
        test: Test {
            job: test_job(file),
            last,
        },
    }
}

pub(crate) fn view(index: &Index, v: &ModuleView, actions: Html) -> Page {
    let (file, m) = (v.file, v.module);
    let page = Page::new(Section::Packages, module_name(file))
        .icon(octicon!("gear"))
        .sub(html!("NixOS module · {}", source_link(&index.rev, file)))
        .actions(actions);
    let mut out = Html::new();
    let mut health = Html::new();
    health.push(&build_row("NixOS test", &v.test.job, &v.test.last));
    health.push(&kv("Maintainers", people_links(&m.maintainers)));
    if !m.teams.is_empty() {
        health.push(&kv("Teams", teams_html(index, &m.teams)));
    }
    health.push(&kv(
        "Directory",
        html!(
            "<a href=\"/a/{0}\">{0}/</a>",
            file.rsplit_once('/').map_or("nixos/modules", |(d, _)| d)
        ),
    ));
    out.push(&crate::ui::section("Health", None, None, health));
    out.push(&v.subject.sections());
    page.body(out)
}

pub(super) async fn page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Path(file): Path<String>,
) -> Response {
    let viewer = app.viewer(&headers);
    let known = app.index.read().unwrap().modules.contains_key(&file);
    if !known {
        return not_found(&app, viewer.as_deref(), &file);
    }
    let subject = Subject::Module(file.clone());
    let last = app.hydra.last(&test_job(&file)).await;
    let actions = subscribe_button(watching(&app, viewer.as_deref(), &subject), &subject);
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let Some(m) = index.modules.get(&file) else {
        drop(index);
        return not_found(&app, viewer.as_deref(), &file);
    };
    let page = view(&index, &data(&s, &h, &index, &file, m, last), actions);
    drop(index);
    page.render(&app, viewer.as_deref())
}

/// `name` is a module file or `nixos/NAME`.
pub(crate) async fn value(app: &crate::web::App, name: &str) -> Option<Value> {
    let file = {
        let index = app.index.read().unwrap();
        let file = match Subject::resolve(&index, name) {
            Some(Subject::Module(m)) => m,
            _ => name.to_string(),
        };
        index.modules.contains_key(&file).then_some(file)?
    };
    let last = app.hydra.last(&test_job(&file)).await;
    let s = app.snap();
    let h = app.health.read().unwrap().clone();
    let index = app.index.read().unwrap();
    let m = index.modules.get(&file)?;
    Some(json!(data(&s, &h, &index, &file, m, last)))
}
