mod area;
mod github;
mod ledger;
mod model;
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
    name = "review-club",
    version,
    about = "Review a nixpkgs pull request, get yours on the list"
)]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    /// Where the query commands read from.
    #[arg(
        long,
        global = true,
        env = "REVIEW_CLUB_SERVER",
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
        #[arg(long, env = "REVIEW_CLUB_LISTEN", default_value = "127.0.0.1:8790")]
        listen: String,
        #[arg(long, env = "REVIEW_CLUB_DB", default_value = "review-club.sqlite")]
        db: PathBuf,
        /// The URL people reach; the OAuth callback is `<url>/auth/callback`.
        #[arg(
            long,
            env = "REVIEW_CLUB_PUBLIC_URL",
            default_value = "http://127.0.0.1:8790"
        )]
        public_url: String,
        #[arg(long, env = "GITHUB_CLIENT_ID")]
        client_id: Option<String>,
        #[arg(long, env = "GITHUB_CLIENT_SECRET_FILE")]
        client_secret_file: Option<PathBuf>,
        /// A token owned by the deployment, used before any lent token.
        #[arg(long, env = "REVIEW_CLUB_SYNC_TOKEN_FILE")]
        sync_token_file: Option<PathBuf>,
        #[arg(long, default_value_t = 600)]
        sync_every: u64,
    },
    /// One sync pass into a local database, then exit
    Sync {
        #[arg(long, env = "REVIEW_CLUB_DB", default_value = "review-club.sqlite")]
        db: PathBuf,
        #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
        token: String,
        #[arg(long)]
        index: bool,
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
    /// One person's ledger, or everyone's
    Ledger { login: Option<String> },
    /// Exit 0 when the login is listed, 3 when it is not, 1 on error
    Check { login: String },
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

async fn background(app: Arc<App>, every: u64) {
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

async fn run(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::Serve {
            listen,
            db,
            public_url,
            client_id,
            client_secret_file,
            sync_token_file,
            sync_every,
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
            });
            app.recompute()?;
            tokio::spawn(background(app.clone(), sync_every));
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
        Command::Sync { db, token, index } => {
            let store = Store::open(&db)?;
            let app = App {
                index: RwLock::new(web::load_index(&store)),
                store,
                oauth: None,
                sync_token: None,
                snapshot: Default::default(),
                secure_cookies: false,
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
        Command::Ledger { login } => {
            let path = match &login {
                Some(l) => format!("/api/v1/people/{}", github::urlencode(l)),
                None => "/api/v1/people".to_string(),
            };
            let v = fetch(&cli.server, &path).await?;
            if cli.json {
                println!("{v}");
            } else if login.is_some() {
                let p = &v["person"];
                println!(
                    "{}  credit {:.2}  authors {}  listed {}  reciprocity {:.0}%",
                    p["login"].as_str().unwrap_or(""),
                    p["score"].as_f64().unwrap_or(0.0),
                    p["counterparties"],
                    p["listed"],
                    p["reciprocity"].as_f64().unwrap_or(0.0) * 100.0
                );
                for c in v["credits"].as_array().into_iter().flatten() {
                    println!(
                        "  {:.2}  #{:<7} {}  {}",
                        c["value"].as_f64().unwrap_or(0.0),
                        c["pr"],
                        c["review_url"].as_str().unwrap_or(""),
                        c["dropped"]["reason"].as_str().unwrap_or("")
                    );
                }
            } else {
                for p in v.as_array().into_iter().flatten().take(50) {
                    println!(
                        "{:<24} {:>6.2}  {}",
                        p["login"].as_str().unwrap_or(""),
                        p["score"].as_f64().unwrap_or(0.0),
                        if p["listed"] == true { "listed" } else { "" }
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
