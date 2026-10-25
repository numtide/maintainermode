//! The whole router, over HTTP, for a person who signed in.

use crate::web::Shared;
use reqwest::header::{CACHE_CONTROL, COOKIE, LOCATION};
use reqwest::{Client, StatusCode};

pub(crate) struct Site {
    pub base: String,
    pub client: Client,
}

pub(crate) async fn serve(app: Shared) -> Site {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, crate::web::router(app))
            .await
            .unwrap();
    });
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    Site { base, client }
}

pub(crate) struct Answer {
    pub status: StatusCode,
    pub cache: Option<String>,
    pub location: Option<String>,
    pub body: String,
}

impl Site {
    async fn answer(&self, rb: reqwest::RequestBuilder) -> Answer {
        let r = rb.send().await.unwrap();
        let h = |n| {
            r.headers()
                .get(n)
                .and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok())
                .map(str::to_string)
        };
        let cache = h(CACHE_CONTROL);
        let location = h(LOCATION);
        Answer {
            status: r.status(),
            cache,
            location,
            body: r.text().await.unwrap(),
        }
    }

    pub async fn get(&self, path: &str, session: Option<&str>) -> Answer {
        let mut rb = self.client.get(format!("{}{path}", self.base));
        if let Some(s) = session {
            rb = rb.header(COOKIE, format!("session={s}"));
        }
        self.answer(rb).await
    }

    pub async fn post(&self, path: &str, session: &str, form: &[(&str, &str)]) -> Answer {
        let rb = self
            .client
            .post(format!("{}{path}", self.base))
            .header(COOKIE, format!("session={session}"))
            .form(form);
        self.answer(rb).await
    }
}

const SID: &str = "e2e-session";

fn signed_in_app() -> Shared {
    let app = crate::sets::tests::app(&["hello"]);
    {
        let mut index = app.index.write().unwrap();
        index.by_pname.insert("hello".into(), "hello".into());
        index.pkgs.insert(
            "hello".into(),
            crate::area::Package {
                pname: "hello".into(),
                version: "2.12".into(),
                file: Some("pkgs/by-name/he/hello/package.nix".into()),
                maintainers: vec!["alice".into()],
                ..Default::default()
            },
        );
    }
    app.store
        .create_session(SID, "alice", "dummy-token")
        .unwrap();
    app.recompute().unwrap();
    app
}

fn private(a: &Answer, what: &str) {
    assert_eq!(
        a.cache.as_deref(),
        Some("private, no-store"),
        "{what}: Cache-Control"
    );
}

#[tokio::test]
async fn every_signed_in_page_answers_and_stays_private() {
    let app = signed_in_app();
    let site = serve(app.clone()).await;
    for path in [
        "/",
        "/inbox",
        "/packages?filter=mine",
        "/packages?filter=machines",
        "/packages?filter=maintain",
        "/packages?filter=watch",
        "/packages?filter=all",
        "/me/settings",
        "/me/progress",
        "/review",
        "/review?tab=ready",
        "/review?tab=bumps",
        "/review?tab=bumps&filter=mine&sort=newest&checks=pass",
        "/review?tab=all",
        "/u/alice",
        "/u/alice?tab=reviews",
        "/u/alice?tab=maintains",
        "/packages",
        "/p/hello",
        "/state",
    ] {
        let a = site.get(path, Some(SID)).await;
        assert_eq!(a.status, StatusCode::OK, "{path}: {}", a.body);
        private(&a, path);
        assert!(a.body.contains("menu-login\">alice<"), "{path}: menu");
    }
}

#[tokio::test]
async fn a_visitor_gets_one_intro_one_list_and_one_row_of_links() {
    let site = serve(signed_in_app()).await;
    let a = site.get("/", None).await;
    assert_eq!(a.status, StatusCode::OK);
    assert_eq!(a.body.matches("<h1").count(), 1, "{}", a.body);
    assert_eq!(a.body.matches("class=\"Box-title\"").count(), 1);
    assert!(a.body.contains("id=\"intro\""));
    assert!(
        a.body
            .contains("Box-title\">A first package to work on</h2>")
    );
    assert!(a.body.contains("class=\"search-inline"));
    let links = a
        .body
        .split("aria-label=\"More in nixpkgs\">")
        .nth(1)
        .and_then(|s| s.split("</nav>").next())
        .expect("a row of links");
    for href in ["/review?tab=unowned", "/packages", "/state"] {
        assert!(
            links.contains(&format!("class=\"btn btn-sm\" href=\"{href}\"")),
            "{href}"
        );
    }
    assert!(!a.body.contains("Hide"));
}

#[tokio::test]
async fn text_from_github_and_people_is_escaped() {
    let app = signed_in_app();
    {
        let mut index = app.index.write().unwrap();
        let p = index.pkgs.get_mut("hello").unwrap();
        p.facts = Some(crate::attrs::Facts {
            description: Some("<x> says hello".into()),
            ..Default::default()
        });
    }
    let mut p = crate::ledger::tests::pr(7, "alice", 0);
    p.title = "hello: <x> \"q\" & more".into();
    p.state = "OPEN".into();
    p.labels = vec!["<x>".into()];
    p.base = "<x>".into();
    app.store.put_prs(&[p]).unwrap();
    app.recompute().unwrap();
    let site = serve(app.clone()).await;
    site.post("/me/tags", SID, &[("tags", "<x>")]).await;
    for (path, raw) in [
        ("/pr/7", "hello: <x>"),
        ("/u/alice", "hello: <x>"),
        ("/", "hello: <x>"),
        ("/p/hello", "<x> says hello"),
        ("/me/settings", "<x></textarea>"),
    ] {
        let a = site.get(path, Some(SID)).await;
        assert_eq!(a.status, StatusCode::OK, "{path}");
        assert!(!a.body.contains(raw), "{path} has raw {raw}");
        assert!(!a.body.contains("<x>"), "{path} has <x>");
        assert!(a.body.contains("&lt;x&gt;"), "{path} has no escaped <x>");
    }
}

#[tokio::test]
async fn me_pages_show_their_parts() {
    let app = signed_in_app();
    let site = serve(app.clone()).await;
    for (filter, label) in [
        ("mine", "All"),
        ("machines", "On my machines"),
        ("maintain", "I maintain"),
        ("watch", "I watch"),
    ] {
        let a = site
            .get(&format!("/packages?filter={filter}"), Some(SID))
            .await;
        assert!(
            a.body.contains(&format!(
                "href=\"/packages?filter={filter}\" aria-current=\"page\">{label} <span class=\"Counter\">"
            )),
            "{filter}: {}",
            a.body
        );
        assert!(a.body.contains("href=\"/packages\" aria-current=\"page\""));
        assert!(a.body.contains("href=\"/packages?filter=all\">"));
        assert!(!a.body.contains(">All of nixpkgs <span class=\"Counter\">"));
    }
    for filter in ["mine", "machines"] {
        let a = site
            .get(&format!("/packages?filter={filter}"), Some(SID))
            .await;
        assert!(
            a.body
                .contains("href=\"/me/settings#upload\">Upload the packages of your machines"),
            "{filter}: {}",
            a.body
        );
    }
    let a = site.get("/packages?filter=all", Some(SID)).await;
    assert!(
        a.body.contains("Box-title\">Directories</h2>"),
        "{}",
        a.body
    );
    assert!(a.body.contains("href=\"/packages\">"));
    assert!(!a.body.contains("aria-label=\"Views\""));
    let a = site.get("/packages", Some(SID)).await;
    assert!(
        a.body
            .contains("href=\"/packages?filter=mine\" aria-current=\"page\">All")
    );
    assert!(a.body.contains("href=\"/p/hello\""), "{}", a.body);
    assert!(a.body.contains("you maintain"));
    let a = site.get("/packages?q=hel", Some(SID)).await;
    assert_eq!(a.location.as_deref(), Some("/search?q=hel"));
    let a = site.get("/search?q=hel", Some(SID)).await;
    assert!(a.body.contains("href=\"/p/hello\""), "{}", a.body);
    assert!(a.body.contains("value=\"hel\""));
    let a = site.get("/search?q=%40alice", None).await;
    assert_eq!(a.location.as_deref(), Some("/u/alice"));
    let a = site.get("/search?q=%23123", None).await;
    assert_eq!(a.location.as_deref(), Some("/pr/123"));
    let a = site.get("/packages", None).await;
    assert!(!a.body.contains("filter="), "a visitor sees no filter bar");
    assert!(!a.body.contains("Upload the packages"));
    let a = site.get("/inbox", Some(SID)).await;
    assert!(a.body.contains("<a href=\"/inbox\" aria-current=\"page\">"));
    assert!(!a.body.contains("site-tab\" href=\"/\" aria-current"));
    let a = site.get("/me/inbox", Some(SID)).await;
    assert_eq!(a.location.as_deref(), Some("/inbox"));
    let a = site.get("/me/settings", Some(SID)).await;
    assert!(
        a.body
            .contains("<a href=\"/me/settings\" aria-current=\"page\">")
    );
    for id in [
        "notifications",
        "feed",
        "upload",
        "api",
        "tags",
        "goal",
        "start",
    ] {
        assert!(a.body.contains(&format!(" id=\"{id}\">")), "#{id}");
    }
    let a = site.get("/review", Some(SID)).await;
    assert!(
        a.body.contains("<option value=\"foryou\" selected>For you"),
        "{}",
        a.body
    );
    assert!(!a.body.contains("aria-label=\"Views\""));
    assert!(!a.body.contains("<details class=\"review-more\" open>"));
    let a = site.get("/review?tab=ready", Some(SID)).await;
    assert!(
        a.body
            .contains("href=\"/review?tab=ready\" aria-current=\"page\">Ready to merge")
    );
    assert!(a.body.contains("href=\"/review\">Needs review"));
    let a = site
        .post(
            "/me/review/hide",
            SID,
            &[("number", "42"), ("back", "/review?tab=all&checks=fail")],
        )
        .await;
    assert_eq!(a.location.as_deref(), Some("/review?tab=all&checks=fail"));
    assert!(app.store.hidden_prs("alice").unwrap().contains(&42));
    let a = site
        .get("/review?tab=all&hidden=1&checks=fail", Some(SID))
        .await;
    assert!(a.body.contains("Hidden by you"), "{}", a.body);
    assert!(a.body.contains("<option value=\"fail\" selected>"));
    assert!(a.body.contains("<details class=\"review-more\" open>"));
    let a = site
        .post(
            "/me/review/hide",
            SID,
            &[("number", "42"), ("hide", "0"), ("back", "//evil.example")],
        )
        .await;
    assert_eq!(a.location.as_deref(), Some("/review"));
    assert!(app.store.hidden_prs("alice").unwrap().is_empty());
    let a = site.get("/me/progress", Some(SID)).await;
    assert!(a.body.contains("Your weekly goal"));
}

#[tokio::test]
async fn forms_change_what_the_pages_show() {
    let app = signed_in_app();
    let site = serve(app.clone()).await;

    let a = site.post("/me/watch", SID, &[("name", "hello")]).await;
    assert_eq!(a.status, StatusCode::SEE_OTHER, "{}", a.body);
    private(&a, "watch");
    assert_eq!(a.location.as_deref(), Some("/packages?filter=watch"));
    assert_eq!(app.store.watched("alice").unwrap(), vec!["pkg:hello"]);
    let a = site.get("/packages?filter=watch", Some(SID)).await;
    assert!(a.body.contains("value=\"pkg:hello\""), "{}", a.body);

    let a = site
        .post("/me/watch/remove", SID, &[("key", "pkg:hello")])
        .await;
    assert_eq!(a.status, StatusCode::SEE_OTHER);
    assert_eq!(a.location.as_deref(), Some("/packages?filter=watch"));
    assert!(app.store.watched("alice").unwrap().is_empty());

    let a = site.post("/me/inbox/read", SID, &[]).await;
    assert_eq!(a.status, StatusCode::SEE_OTHER);
    private(&a, "inbox/read");

    let a = site
        .post(
            "/me/tags",
            SID,
            &[("tags", "pkgs/development/python-modules/\n\n")],
        )
        .await;
    assert_eq!(a.status, StatusCode::SEE_OTHER);
    assert_eq!(a.location.as_deref(), Some("/me/settings#tags"));
    let a = site.get("/me/settings", Some(SID)).await;
    assert!(
        a.body
            .contains(">pkgs/development/python-modules</textarea>")
    );

    let a = site.post("/me/progress", SID, &[("goal", "3")]).await;
    assert_eq!(a.status, StatusCode::SEE_OTHER);
    let a = site.get("/me/progress", Some(SID)).await;
    assert!(a.body.contains("of your goal of 3 this week"), "{}", a.body);
    let a = site.post("/me/progress", SID, &[("goal", "many")]).await;
    assert_eq!(a.status, StatusCode::BAD_REQUEST);

    let a = site.post("/me/feed", SID, &[]).await;
    assert_eq!(a.status, StatusCode::SEE_OTHER);
    let a = site.get("/me/settings", Some(SID)).await;
    let feed = a
        .body
        .split("/feed/")
        .nth(1)
        .and_then(|s| s.split(['"', '<']).next())
        .expect("a feed link")
        .to_string();
    let a = site.get(&format!("/feed/{feed}"), None).await;
    assert_eq!(a.status, StatusCode::OK);
    assert!(a.body.starts_with("<?xml"));
    assert!(
        a.body
            .contains("<feed xmlns=\"http://www.w3.org/2005/Atom\">")
    );

    let a = site.post("/me/upload-token", SID, &[]).await;
    assert_eq!(a.status, StatusCode::OK);
    private(&a, "upload-token");
    assert!(a.body.contains("aria-label=\"upload token\""));
}

#[tokio::test]
async fn public_pages_keep_private_data_out() {
    let app = signed_in_app();
    let site = serve(app.clone()).await;
    site.post("/me/watch", SID, &[("key", "area:pkgs/secret-watch")])
        .await;
    site.post("/me/feed", SID, &[]).await;
    site.post("/me/upload-token", SID, &[]).await;
    site.post("/me/tags", SID, &[("tags", "pkgs/secret-tag")])
        .await;
    let feed = app
        .store
        .feed_token("alice")
        .unwrap()
        .expect("a feed token");
    for path in [
        "/u/alice",
        "/u/alice?tab=reviews",
        "/u/alice?tab=maintains",
        "/api/v1/people/alice",
        "/p/hello",
    ] {
        let a = site.get(path, None).await;
        assert_eq!(a.status, StatusCode::OK, "{path}");
        for secret in [
            "secret-watch",
            "dummy-token",
            "pkgs/secret-tag",
            feed.as_str(),
            SID,
        ] {
            assert!(!a.body.contains(secret), "{path} shows {secret}");
        }
        assert_ne!(a.cache.as_deref(), Some("private, no-store"), "{path}");
    }
}

#[tokio::test]
async fn settings_tokens_and_sign_out() {
    let app = signed_in_app();
    let site = serve(app.clone()).await;
    let a = site
        .post("/me/watch", SID, &[("key", "area:pkgs/development")])
        .await;
    assert_eq!(a.location.as_deref(), Some("/a/pkgs/development"));
    let a = site
        .post("/me/watch", SID, &[("name", "no-such-thing")])
        .await;
    assert_eq!(a.status, StatusCode::BAD_REQUEST);
    private(&a, "a refused watch");
    let a = site.post("/me/watch/maintained", SID, &[]).await;
    assert_eq!(a.location.as_deref(), Some("/packages?filter=maintain"));
    assert!(
        app.store
            .watched("alice")
            .unwrap()
            .contains(&"pkg:hello".to_string())
    );
    let a = site.get("/packages?filter=maintain", Some(SID)).await;
    assert!(a.body.contains("href=\"/p/hello\""));

    site.post("/me/tags", SID, &[("tags", "<script>x</script>")])
        .await;
    let a = site.get("/me/settings", Some(SID)).await;
    assert!(!a.body.contains("<script>x"));
    assert!(a.body.contains("&lt;script&gt;x&lt;"));

    let a = site.post("/me/api-token", SID, &[]).await;
    assert_eq!(a.status, StatusCode::OK);
    private(&a, "api-token");
    let token = a
        .body
        .split("readonly value=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap()
        .to_string();
    assert_eq!(
        crate::apitoken::login_of(&app, &token).as_deref(),
        Some("alice")
    );
    let a = site.post("/me/api-token/delete", SID, &[]).await;
    assert_eq!(a.location.as_deref(), Some("/me/settings#api"));
    assert!(crate::apitoken::login_of(&app, &token).is_none());
    let a = site.post("/me/upload-token/delete", SID, &[]).await;
    assert_eq!(a.location.as_deref(), Some("/me/settings#upload"));
    let a = site.post("/me/start", SID, &[("done", "1")]).await;
    assert_eq!(a.status, StatusCode::SEE_OTHER);

    let a = site.post("/auth/logout", SID, &[]).await;
    assert_eq!(a.location.as_deref(), Some("/"));
    let a = site.get("/inbox", Some(SID)).await;
    assert_eq!(a.status, StatusCode::SEE_OTHER);
    let a = site.get("/review", Some(SID)).await;
    assert!(!a.body.contains("menu-login"));
}
