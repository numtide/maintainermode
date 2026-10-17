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
        "/me?tab=maintain",
        "/me?tab=watch",
        "/me?tab=machines",
        "/me/settings",
        "/me/runs",
        "/me/progress",
        "/review",
        "/review?tab=ready",
        "/review?tab=bumps",
        "/review?tab=all",
        "/u/alice",
        "/u/alice/maintains",
        "/packages",
        "/p/hello",
        "/state",
    ] {
        let a = site.get(path, Some(SID)).await;
        assert_eq!(a.status, StatusCode::OK, "{path}: {}", a.body);
        private(&a, path);
        assert!(a.body.contains("menu-login\">alice<"), "{path}: menu");
    }
    let a = site.get("/actions", Some(SID)).await;
    assert_eq!(a.location.as_deref(), Some("/me/runs"));
}

#[tokio::test]
async fn me_pages_show_their_parts() {
    let site = serve(signed_in_app()).await;
    for (tab, label) in [
        ("maintain", "Maintain"),
        ("watch", "Watch"),
        ("machines", "My machines"),
    ] {
        let a = site.get(&format!("/me?tab={tab}"), Some(SID)).await;
        assert!(
            a.body.contains(&format!(
                "href=\"/me?tab={tab}\" aria-current=\"page\">{label}"
            )),
            "{tab}"
        );
    }
    let a = site.get("/inbox", Some(SID)).await;
    assert!(a.body.contains("<a href=\"/inbox\" aria-current=\"page\">"));
    assert!(
        !a.body
            .contains("UnderlineNav-item\" href=\"/\" aria-current")
    );
    let a = site.get("/me/inbox", Some(SID)).await;
    assert_eq!(a.location.as_deref(), Some("/inbox"));
    let a = site.get("/me/settings", Some(SID)).await;
    assert!(
        a.body
            .contains("<a href=\"/me/settings\" aria-current=\"page\">")
    );
    for id in [
        "email",
        "feed",
        "upload",
        "llm",
        "mcp",
        "workflows",
        "tags",
        "goal",
        "start",
    ] {
        assert!(a.body.contains(&format!("<div id=\"{id}\">")), "#{id}");
    }
    let a = site.get("/review", Some(SID)).await;
    assert!(
        a.body
            .contains("selected\" href=\"/review?tab=foryou\" aria-current=\"page\">For you"),
        "{}",
        a.body
    );
    let a = site.get("/review?tab=ready", Some(SID)).await;
    assert!(
        a.body
            .contains("href=\"/review?tab=ready\" aria-current=\"page\">Ready to merge")
    );
    assert!(a.body.contains("href=\"/review?tab=foryou\">For you"));
    let a = site.get("/me/runs", Some(SID)).await;
    assert!(a.body.contains("Runs"), "{}", a.body);
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
    assert_eq!(a.location.as_deref(), Some("/me?tab=watch"));
    assert_eq!(app.store.watched("alice").unwrap(), vec!["pkg:hello"]);
    let a = site.get("/me?tab=watch", Some(SID)).await;
    assert!(a.body.contains("value=\"pkg:hello\""), "{}", a.body);

    let a = site
        .post("/me/watch/remove", SID, &[("key", "pkg:hello")])
        .await;
    assert_eq!(a.status, StatusCode::SEE_OTHER);
    assert_eq!(a.location.as_deref(), Some("/me?tab=watch"));
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

    let a = site.post("/me/settings/llm", SID, &[("llm_ok", "1")]).await;
    assert_eq!(a.status, StatusCode::SEE_OTHER);
    assert!(crate::llm::opted_in(&app, "alice"));

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
    app.store
        .set_setting("alice", crate::notify::EMAIL, "alice@example.org")
        .unwrap();
    assert_eq!(
        app.store
            .setting("alice", crate::notify::EMAIL)
            .unwrap()
            .as_deref(),
        Some("alice@example.org")
    );
    let feed = app
        .store
        .feed_token("alice")
        .unwrap()
        .expect("a feed token");
    for path in [
        "/u/alice",
        "/u/alice/maintains",
        "/api/v1/people/alice",
        "/p/hello",
    ] {
        let a = site.get(path, None).await;
        assert_eq!(a.status, StatusCode::OK, "{path}");
        for secret in [
            "secret-watch",
            "alice@example.org",
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
        .post(
            "/me/settings/workflows",
            SID,
            &[
                ("review_repo", "alice/review"),
                ("update_repo", "alice/update"),
                ("nixpkgs_fork", "alice/nixpkgs"),
            ],
        )
        .await;
    assert_eq!(a.location.as_deref(), Some("/me/settings#workflows"));
    let a = site.get("/me/settings", Some(SID)).await;
    assert!(a.body.contains("value=\"alice/review\""));

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
    assert_eq!(a.location.as_deref(), Some("/me?tab=maintain"));
    assert!(
        app.store
            .watched("alice")
            .unwrap()
            .contains(&"pkg:hello".to_string())
    );
    let a = site.get("/me?tab=maintain", Some(SID)).await;
    assert!(a.body.contains("href=\"/p/hello\""));

    site.post("/me/tags", SID, &[("tags", "<script>x</script>")])
        .await;
    let a = site.get("/me/settings", Some(SID)).await;
    assert!(!a.body.contains("<script>x"));
    assert!(a.body.contains("&lt;script&gt;x&lt;"));

    let a = site.post("/me/mcp-token", SID, &[]).await;
    assert_eq!(a.location.as_deref(), Some("/me/settings#llm"));
    site.post("/me/settings/llm", SID, &[("llm_ok", "1")]).await;
    let a = site.post("/me/mcp-token", SID, &[]).await;
    assert_eq!(a.status, StatusCode::OK);
    private(&a, "mcp-token");
    let a = site.get("/claude/pr/1", Some(SID)).await;
    assert_eq!(a.status, StatusCode::NOT_FOUND);
    private(&a, "claude/pr/1");
    let a = site.post("/me/settings/llm", SID, &[]).await;
    assert_eq!(a.status, StatusCode::SEE_OTHER);
    assert!(!crate::llm::opted_in(&app, "alice"));
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
