mod area;
mod attrs;
mod dossier;
mod eval;
mod github;
mod ledger;
mod metrics;
mod model;
mod nixfresh;
mod queue;
mod store;
mod web;

use anyhow::{Context, Result, bail};
use chrono::{Duration, Utc};
use clap::{Parser, Subcommand};
use serde_json::Value;
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
        #[arg(long, default_value_t = 600)]
        sync_every: u64,
        /// A nixfresh instance whose public API supplies package health.
        #[arg(long, env = "MAINTAINER_MODE_NIXFRESH_URL")]
        nixfresh_url: Option<String>,
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

async fn background(app: Arc<App>, every: u64, history_since: chrono::NaiveDate) {
    loop {
        let index_at = app.store.get_time("index_at").ok().flatten();
        if index_at.is_none_or(|t| Utc::now() - t > Duration::hours(24)) {
            match web::refresh_index(&app).await {
                Ok(()) => tracing::info!("nixpkgs index refreshed"),
                Err(e) => tracing::warn!("nixpkgs index: {e:#}"),
            }
        }
        let mut tokens: Vec<String> = app.sync_token.iter().cloned().collect();
        tokens.extend(app.store.lent_tokens().unwrap_or_default());
        let mut client = github::Client::new(tokens);
        if client.has_token() {
            match github::sync(&app.store, &mut client, ledger::WINDOW_DAYS).await {
                Ok(n) => tracing::info!("synced {n} pull requests"),
                Err(e) => tracing::warn!("sync: {e:#}"),
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
        } else {
            tracing::info!("no token yet; the sync starts when someone logs in");
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

fn print_entries(v: &Value) {
    for e in v.as_array().into_iter().flatten() {
        println!(
            "#{:<7} {:<20} {:>6.2}  {}",
            e["number"],
            e["author"].as_str().unwrap_or(""),
            e["author_score"].as_f64().unwrap_or(0.0),
            e["title"].as_str().unwrap_or("")
        );
    }
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
            sync_every,
            nixfresh_url,
        } => {
            let store = Store::open(&db)?;
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
                store,
                oauth,
                sync_token: read_secret(&sync_token_file)?,
                snapshot: Default::default(),
                secure_cookies: public_url.starts_with("https://"),
                nixfresh: nixfresh_url.as_deref().map(nixfresh::Client::new),
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
                store,
                oauth: None,
                sync_token: None,
                snapshot: Default::default(),
                secure_cookies: false,
                nixfresh: None,
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
                store,
                oauth: None,
                sync_token: None,
                snapshot: Default::default(),
                secure_cookies: false,
                nixfresh: None,
                data_dir: data_dir(&db),
                index_sets: sets.clone(),
                metrics: Default::default(),
            };
            web::refresh_index(&app).await?;
            let index = app.index.read().unwrap();
            println!(
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
                println!("{v}");
            } else {
                print_entries(&v);
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
                println!("{v}");
            } else {
                print_entries(&v);
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
                println!("{v}");
                return Ok(0);
            }
            let d = &v["dossier"];
            println!(
                "{login}: {} merged unchanged after review, {} problems found, {} reviewed, listed {}",
                d["merged_unchanged"], d["problems_found"], d["reviewed"], v["person"]["listed"]
            );
            for r in d["recent"].as_array().into_iter().flatten() {
                println!(
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
                println!("{v}");
                return Ok(0);
            }
            let n = &v["now"];
            println!(
                "open {}  never reviewed {}  security {}  first-time {}  disclosed AI {}",
                n["open"], n["never_reviewed"], n["security"], n["first_time"], n["ai"]
            );
            println!(
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
                println!(
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
        Command::Check { login } => {
            let v = fetch(
                &cli.server,
                &format!("/api/v1/people/{}", github::urlencode(&login)),
            )
            .await?;
            let listed = v["person"]["listed"] == true;
            if cli.json {
                println!("{}", v["person"]);
            } else {
                println!("{login}: {}", if listed { "listed" } else { "not listed" });
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
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    };
    std::process::exit(code);
}
