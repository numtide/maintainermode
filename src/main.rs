mod actions;
mod area;
mod attrs;
mod catalog;
mod dossier;
mod eval;
mod github;
mod github_app;
mod health;
mod ledger;
mod links;
mod metrics;
mod model;
mod queue;
mod store;
mod subject;
mod web;
mod workflows;

use anyhow::{Context, Result, bail};
use chrono::{Duration, Utc};
use clap::{Parser, Subcommand};
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::store::Store;
use crate::web::App;

/// Exit 3 means "not listed", not failure, so a script can tell a broken tool
/// apart from an answer it did not want.
const EXIT_NOT_LISTED: i32 = 3;

#[derive(Parser)]
#[command(
    name = "maintainer-mode",
    version,
    about = "Review a nixpkgs pull request, get yours on the list"
)]
struct Cli {
    /// The Nix evaluator: iets in a deployment, plain nix anywhere else.
    #[arg(
        long,
        global = true,
        env = "MAINTAINER_MODE_EVALUATOR",
        default_value = "nix"
    )]
    evaluator: String,
    /// Package sets to sweep for meta.maintainers; `pkgs` is the top level.
    #[arg(
        long = "index-set",
        global = true,
        env = "MAINTAINER_MODE_INDEX_SETS",
        value_delimiter = ',',
        default_value = "pkgs,python3Packages"
    )]
    index_sets: Vec<String>,
    /// How far back the history walk for the metrics goes.
    #[arg(
        long,
        global = true,
        env = "MAINTAINER_MODE_HISTORY_SINCE",
        default_value = "2020-01-01"
    )]
    history_since: chrono::NaiveDate,
    #[arg(long, global = true)]
    json: bool,
    /// Where the query commands read from.
    #[arg(
        long,
        global = true,
        env = "MAINTAINER_MODE_SERVER",
        default_value = "http://127.0.0.1:8790"
    )]
    server: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Web page, JSON API and the background sync
    Serve {
        #[arg(long, env = "MAINTAINER_MODE_LISTEN", default_value = "127.0.0.1:8790")]
        listen: String,
        #[arg(
            long,
            env = "MAINTAINER_MODE_DB",
            default_value = "maintainer-mode.sqlite"
        )]
        db: PathBuf,
        /// The URL people reach; the OAuth callback is `<url>/auth/callback`.
        #[arg(
            long,
            env = "MAINTAINER_MODE_PUBLIC_URL",
            default_value = "http://127.0.0.1:8790"
        )]
        public_url: String,
        #[arg(long, env = "GITHUB_CLIENT_ID")]
        client_id: Option<String>,
        #[arg(long, env = "GITHUB_CLIENT_SECRET_FILE")]
        client_secret_file: Option<PathBuf>,
        /// A token owned by the deployment, used before any lent token.
        #[arg(long, env = "MAINTAINER_MODE_SYNC_TOKEN_FILE")]
        sync_token_file: Option<PathBuf>,
        /// A GitHub App whose installation token is the first sync token.
        #[arg(long, env = "GITHUB_APP_ID")]
        github_app_id: Option<String>,
        /// The App's PEM private key.
        #[arg(long, env = "GITHUB_APP_KEY_FILE")]
        github_app_key_file: Option<PathBuf>,
        /// Without it, the App's only installation is used.
        #[arg(long, env = "GITHUB_APP_INSTALLATION_ID")]
        github_app_installation_id: Option<u64>,
        #[arg(long, default_value_t = 600)]
        sync_every: u64,
    },
    /// One sync pass into a local database, then exit
    Sync {
        #[arg(
            long,
            env = "MAINTAINER_MODE_DB",
            default_value = "maintainer-mode.sqlite"
        )]
        db: PathBuf,
        #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
        token: String,
        #[arg(long)]
        index: bool,
        /// Also walk the history for the metrics, to the end.
        #[arg(long)]
        history: bool,
    },
    /// Sweep nixpkgs master with the evaluator and store the index
    Sweep {
        #[arg(
            long,
            env = "MAINTAINER_MODE_DB",
            default_value = "maintainer-mode.sqlite"
        )]
        db: PathBuf,
    },
    /// The ready-to-merge queue
    Merge,
    /// The review queue: mine (needs --login), unowned, or all
    Review {
        #[arg(long, default_value = "all", value_parser = ["mine", "unowned", "all"])]
        bucket: String,
        #[arg(long)]
        login: Option<String>,
    },
    /// One person's review record: outcomes, not a rank
    Record { login: String },
    /// Exit 0 when the login is listed, 3 when it is not, 1 on error
    Check { login: String },
    /// A package, NixOS module or path: health, issues and pull requests
    Show { name: String },
    /// The packages and NixOS modules a login maintains
    Maintains { login: String },
    /// The state of nixpkgs, month by month
    Metrics {
        #[arg(long, default_value_t = 12)]
        months: usize,
    },
}

fn read_secret(path: &Option<PathBuf>) -> Result<Option<String>> {
    path.as_ref()
        .map(|p| {
            std::fs::read_to_string(p)
                .with_context(|| format!("read {}", p.display()))
                .map(|s| s.trim().to_string())
        })
        .transpose()
}

pub async fn refresh_member(app: &App, login: &str, token: &str) -> Result<()> {
    let paths = github::committed_paths(token, login).await?;
    app.store.put_member(login, &paths)?;
    app.recompute()
}

async fn settle(app: &App, client: &mut github::Client) -> Result<usize> {
    app.recompute()?;
    let n = github::settle_mergeable(&app.store, client, &app.merge_candidates()).await?;
    Ok(n)
}

/// About ten minutes of walking per pass; the full walk back to 2020 takes
/// several hours.
const HISTORY_PAGES_PER_PASS: usize = 100;

fn since(date: chrono::NaiveDate) -> chrono::DateTime<Utc> {
    date.and_hms_opt(0, 0, 0).unwrap().and_utc()
}

/// A connect error means the host has no network yet, as on first boot.
fn offline(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_connect)
    })
}

/// Seconds before the next pass after `failures` offline passes in a row.
fn retry_wait(failures: u32, every: u64) -> u64 {
    (5u64 << failures.min(6)).min(300).min(every)
}

async fn background(app: Arc<App>, every: u64, history_since: chrono::NaiveDate) {
    let mut failures = 0;
    loop {
        let mut down = false;
        let mut warn = |what: &str, e: anyhow::Error| {
            down |= offline(&e);
            tracing::warn!("{what}: {e:#}");
        };
        let index_at = app.store.get_time("index_at").ok().flatten();
        if index_at.is_none_or(|t| Utc::now() - t > Duration::hours(24)) {
            match web::refresh_index(&app).await {
                Ok(()) => tracing::info!("nixpkgs index refreshed"),
                Err(e) => warn("nixpkgs index", e),
            }
        }
        if web::health_due(&app) {
            match web::refresh_health(&app).await {
                Ok(()) => tracing::info!("package health read"),
                Err(e) => warn("package health", e),
            }
        }
        let mut tokens = Vec::new();
        if let Some(a) = &app.github_app {
            match a.token().await {
                Ok(t) => tokens.push(t),
                Err(e) => warn("GitHub App token", e),
            }
        }
        tokens.extend(app.sync_token.iter().cloned());
        tokens.extend(app.store.lent_tokens().unwrap_or_default());
        let mut client = github::Client::new(tokens);
        if client.has_token() {
            match github::sync(&app.store, &mut client, ledger::WINDOW_DAYS).await {
                Ok(n) => tracing::info!("synced {n} pull requests"),
                Err(e) => warn("sync", e),
            }
        } else {
            tracing::info!("no token yet; the sync starts when someone logs in");
        }
        if down {
            let wait = retry_wait(failures, every);
            failures = failures.saturating_add(1);
            tracing::info!("network unreachable; next pass in {wait} s");
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            continue;
        }
        failures = 0;
        if client.has_token() {
            match github::sync_issues(&app.store, &mut client, ledger::WINDOW_DAYS).await {
                Ok(n) => tracing::info!("synced {n} issues"),
                Err(e) => tracing::warn!("issues: {e:#}"),
            }
            if let Err(e) = settle(&app, &mut client).await {
                tracing::warn!("mergeable: {e:#}");
            }
            match github::history(
                &app.store,
                &mut client,
                since(history_since),
                HISTORY_PAGES_PER_PASS,
            )
            .await
            {
                Ok(0) => {}
                Ok(n) => tracing::info!("history: {n} pull requests"),
                Err(e) => tracing::warn!("history: {e:#}"),
            }
        }
        for m in app.store.members().unwrap_or_default() {
            if Utc::now() - m.refreshed_at < Duration::hours(24) {
                continue;
            }
            if let Ok(Some(token)) = app.store.token_for(&m.login) {
                if let Err(e) = refresh_member(&app, &m.login, &token).await {
                    tracing::warn!("area of {}: {e:#}", m.login);
                }
            }
        }
        if let Err(e) = app.recompute() {
            tracing::warn!("recompute: {e:#}");
        }
        let a = app.clone();
        match tokio::task::spawn_blocking(move || a.recompute_metrics()).await {
            Ok(Err(e)) => tracing::warn!("metrics: {e:#}"),
            Err(e) => tracing::warn!("metrics: {e}"),
            Ok(Ok(())) => {}
        }
        tokio::time::sleep(std::time::Duration::from_secs(every)).await;
    }
}

async fn fetch(server: &str, path: &str) -> Result<Value> {
    let url = format!("{}{path}", server.trim_end_matches('/'));
    let resp = github::http().get(&url).send().await?.error_for_status()?;
    Ok(resp.json().await?)
}

/// Stdout closed by the reader, as `| head` does: not a failure.
#[derive(Debug)]
struct StdoutClosed;

impl std::fmt::Display for StdoutClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("stdout closed")
    }
}

impl std::error::Error for StdoutClosed {}

fn stdout_error(e: std::io::Error) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::BrokenPipe {
        StdoutClosed.into()
    } else {
        anyhow::Error::new(e).context("write to stdout")
    }
}

/// `println!` panics when stdout is closed.
macro_rules! out {
    ($($arg:tt)*) => {
        writeln!(std::io::stdout(), $($arg)*).map_err(stdout_error)?
    };
}

fn print_entries(v: &Value) -> Result<()> {
    for e in v.as_array().into_iter().flatten() {
        out!(
            "#{:<7} {:<20} {:>6.2}  {}",
            e["number"],
            e["author"].as_str().unwrap_or(""),
            e["author_score"].as_f64().unwrap_or(0.0),
            e["title"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

fn data_dir(db: &std::path::Path) -> PathBuf {
    match db.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

async fn run(cli: Cli) -> Result<i32> {
    eval::set_evaluator(&cli.evaluator);
    let sets: Vec<String> = cli
        .index_sets
        .iter()
        .map(|s| {
            if s == "pkgs" {
                String::new()
            } else {
                s.clone()
            }
        })
        .collect();
    match cli.command {
        Command::Serve {
            listen,
            db,
            public_url,
            client_id,
            client_secret_file,
            sync_token_file,
            github_app_id,
            github_app_key_file,
            github_app_installation_id,
            sync_every,
        } => {
            let store = Store::open(&db)?;
            let github_app = match (github_app_id, read_secret(&github_app_key_file)?) {
                (Some(id), Some(pem)) => Some(github_app::GithubApp::new(
                    id,
                    &pem,
                    github_app_installation_id,
                )?),
                (None, None) => None,
                _ => bail!("set both GITHUB_APP_ID and GITHUB_APP_KEY_FILE, or neither"),
            };
            if let Some(a) = &github_app {
                tracing::info!("sync token from GitHub App {}", a.id());
            }
            let oauth = match (client_id, read_secret(&client_secret_file)?) {
                (Some(id), Some(secret)) => Some(github::OAuth {
                    client_id: id,
                    client_secret: secret,
                    public_url: public_url.clone(),
                }),
                (None, None) => None,
                _ => bail!("set both GITHUB_CLIENT_ID and GITHUB_CLIENT_SECRET_FILE, or neither"),
            };
            if let Some(o) = &oauth {
                tracing::info!("OAuth callback: {}", o.callback());
            }
            let app = Arc::new(App {
                index: RwLock::new(web::load_index(&store)),
                health: RwLock::new(Arc::new(web::load_health(&store))),
                store,
                oauth,
                sync_token: read_secret(&sync_token_file)?,
                github_app,
                snapshot: Default::default(),
                secure_cookies: public_url.starts_with("https://"),
                hydra: Default::default(),
                public_url: public_url.clone(),
                data_dir: data_dir(&db),
                index_sets: sets.clone(),
                metrics: Default::default(),
            });
            app.recompute()?;
            let a = app.clone();
            tokio::task::spawn_blocking(move || a.recompute_metrics()).await??;
            tokio::spawn(background(app.clone(), sync_every, cli.history_since));
            let listener = tokio::net::TcpListener::bind(&listen).await?;
            tracing::info!("listening on {listen}");
            let router = web::router(app)
                .layer(tower_http::compression::CompressionLayer::new())
                .layer(tower_http::trace::TraceLayer::new_for_http());
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
            Ok(0)
        }
        Command::Sync {
            db,
            token,
            index,
            history,
        } => {
            let store = Store::open(&db)?;
            let app = App {
                index: RwLock::new(web::load_index(&store)),
                health: RwLock::new(Arc::new(web::load_health(&store))),
                store,
                oauth: None,
                sync_token: None,
                github_app: None,
                snapshot: Default::default(),
                secure_cookies: false,
                hydra: Default::default(),
                public_url: String::new(),
                data_dir: data_dir(&db),
                index_sets: sets.clone(),
                metrics: Default::default(),
            };
            if index {
                web::refresh_index(&app).await?;
            }
            let mut client = github::Client::new(vec![token]);
            let n = github::sync(&app.store, &mut client, ledger::WINDOW_DAYS).await?;
            eprintln!("synced {n} pull requests");
            eprintln!(
                "settled {} merge candidates",
                settle(&app, &mut client).await?
            );
            while history && app.store.get("history_done")?.is_none() {
                let n =
                    github::history(&app.store, &mut client, since(cli.history_since), 50).await?;
                eprintln!("history: {n} pull requests");
            }
            Ok(0)
        }
        Command::Sweep { db } => {
            let store = Store::open(&db)?;
            let app = App {
                index: RwLock::new(web::load_index(&store)),
                health: RwLock::new(Arc::new(web::load_health(&store))),
                store,
                oauth: None,
                sync_token: None,
                github_app: None,
                snapshot: Default::default(),
                secure_cookies: false,
                hydra: Default::default(),
                public_url: String::new(),
                data_dir: data_dir(&db),
                index_sets: sets.clone(),
                metrics: Default::default(),
            };
            web::refresh_index(&app).await?;
            let index = app.index.read().unwrap();
            out!(
                "{} maintainers, {} packages, {} owner lines at {}",
                index.maintainers.len(),
                index.packages.len(),
                index.owners.len(),
                index.rev
            );
            Ok(0)
        }
        Command::Merge => {
            let v = fetch(&cli.server, "/api/v1/queue/merge").await?;
            if cli.json {
                out!("{v}");
            } else {
                print_entries(&v)?;
            }
            Ok(0)
        }
        Command::Review { bucket, login } => {
            if bucket == "mine" && login.is_none() {
                bail!("--bucket mine needs --login");
            }
            let mut path = format!("/api/v1/queue/review?bucket={bucket}");
            if let Some(l) = login {
                path.push_str(&format!("&login={}", github::urlencode(&l)));
            }
            let v = fetch(&cli.server, &path).await?;
            if cli.json {
                out!("{v}");
            } else {
                print_entries(&v)?;
            }
            Ok(0)
        }
        Command::Record { login } => {
            let v = fetch(
                &cli.server,
                &format!("/api/v1/people/{}", github::urlencode(&login)),
            )
            .await?;
            if cli.json {
                out!("{v}");
                return Ok(0);
            }
            let d = &v["dossier"];
            out!(
                "{login}: {} merged unchanged after review, {} problems found, {} reviewed, listed {}",
                d["merged_unchanged"],
                d["problems_found"],
                d["reviewed"],
                v["person"]["listed"]
            );
            for r in d["recent"].as_array().into_iter().flatten() {
                out!(
                    "  {}  #{:<7} {:<22} {}",
                    r["at"].as_str().unwrap_or("").get(..10).unwrap_or(""),
                    r["pr"],
                    r["outcome"].as_str().unwrap_or(""),
                    r["pr_title"].as_str().unwrap_or("")
                );
            }
            Ok(0)
        }
        Command::Metrics { months } => {
            let v = fetch(&cli.server, "/api/v1/metrics").await?;
            if cli.json {
                out!("{v}");
                return Ok(0);
            }
            let n = &v["now"];
            out!(
                "open {}  never reviewed {}  security {}  first-time {}  disclosed AI {}",
                n["open"],
                n["never_reviewed"],
                n["security"],
                n["first_time"],
                n["ai"]
            );
            out!(
                "{:<8} {:>6} {:>6} {:>6} {:>6} {:>8} {:>7} {:>9} {:>9} {:>7}",
                "month",
                "opened",
                "merged",
                "closed",
                "AI",
                "open-end",
                "review",
                "1st-rev h",
                "merge d",
                "in 7d"
            );
            let all = v["months"].as_array().cloned().unwrap_or_default();
            for m in all.iter().rev().take(months).rev() {
                let f = |k: &str| m[k].as_f64().map_or("-".into(), |x| format!("{x}"));
                out!(
                    "{:<8} {:>6} {:>6} {:>6} {:>6} {:>8} {:>7} {:>9} {:>9} {:>7}",
                    m["month"].as_str().unwrap_or(""),
                    m["opened"].to_string(),
                    m["merged"].to_string(),
                    m["closed_unmerged"].to_string(),
                    m["opened_ai"].to_string(),
                    m["open_at_end"].to_string(),
                    m["reviews"].to_string(),
                    f("first_review_hours_p50"),
                    f("merge_days_p50"),
                    m["reviewed_within_7d"]
                        .as_f64()
                        .map_or("-".into(), |x| format!("{:.0}%", x * 100.0)),
                );
            }
            Ok(0)
        }
        Command::Show { name } => {
            let v = fetch(&cli.server, &format!("/api/v1/find/{name}")).await?;
            if cli.json {
                out!("{v}");
                return Ok(0);
            }
            let f = &v["flags"];
            out!(
                "{}  ({})",
                v["name"].as_str().unwrap_or(&name),
                v["key"].as_str().unwrap_or("")
            );
            if let Some(p) = v["package"].as_object() {
                out!(
                    "  version {}  maintainers {}",
                    p["version"].as_str().unwrap_or("?"),
                    p["maintainers"]
                );
            }
            if let Some(u) = f["upstream"].as_str() {
                out!("  upstream {u}");
            }
            for a in v["advisories"].as_array().into_iter().flatten() {
                out!(
                    "  advisory {}  {}",
                    a["code"].as_str().unwrap_or(""),
                    a["title"].as_str().unwrap_or("")
                );
            }
            for (k, label) in [("hydra", "hydra"), ("test", "nixos test")] {
                if let Some(b) = v[k].as_object() {
                    out!(
                        "  {label} {}  https://hydra.nixos.org/build/{}",
                        b["status"].as_str().unwrap_or("?"),
                        b["id"]
                    );
                }
            }
            for (k, label) in [("prs", "pr"), ("issues", "issue")] {
                for i in v[k]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|i| i["state"] == "OPEN")
                    .take(20)
                {
                    out!(
                        "  {label:<5} #{:<7} {}",
                        i["number"],
                        i["title"].as_str().unwrap_or("")
                    );
                }
            }
            Ok(0)
        }
        Command::Maintains { login } => {
            let v = fetch(
                &cli.server,
                &format!("/api/v1/maintainers/{}", github::urlencode(&login)),
            )
            .await?;
            if cli.json {
                out!("{v}");
                return Ok(0);
            }
            for kind in ["packages", "modules"] {
                for r in v[kind].as_array().into_iter().flatten() {
                    let f = &r["flags"];
                    out!(
                        "{:<40} {:<14} upstream {:<12} advisories {:<2} prs {:<3} issues {}",
                        r["name"].as_str().unwrap_or(""),
                        r["version"].as_str().unwrap_or(""),
                        f["upstream"].as_str().unwrap_or("-"),
                        f["open_advisories"],
                        f["open_prs"],
                        f["open_issues"]
                    );
                }
            }
            Ok(0)
        }
        Command::Check { login } => {
            let v = fetch(
                &cli.server,
                &format!("/api/v1/people/{}", github::urlencode(&login)),
            )
            .await?;
            let listed = v["person"]["listed"] == true;
            if cli.json {
                out!("{}", v["person"]);
            } else {
                out!("{login}: {}", if listed { "listed" } else { "not listed" });
            }
            Ok(if listed { 0 } else { EXIT_NOT_LISTED })
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let code = match run(Cli::parse()).await {
        Ok(c) => c,
        Err(e) if e.is::<StdoutClosed>() => 0,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_backs_off_and_caps() {
        let waits: Vec<u64> = (0..9).map(|f| retry_wait(f, 600)).collect();
        assert_eq!(waits, [5, 10, 20, 40, 80, 160, 300, 300, 300]);
        assert_eq!(retry_wait(8, 60), 60);
    }

    #[tokio::test]
    async fn refused_connection_is_offline() {
        let e = github::http()
            .get("http://127.0.0.1:1/")
            .send()
            .await
            .unwrap_err();
        assert!(offline(&anyhow::Error::new(e).context("sync")));
        assert!(!offline(&anyhow::anyhow!("graphql 502 Bad Gateway")));
    }
}
