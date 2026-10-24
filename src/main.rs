mod actions;
mod affected;
mod api;
mod archive;
mod area;
mod assets;
mod attrs;
mod builds;
mod bumps;
mod catalog;
mod channels;
mod cli_login;
mod committer;
mod dossier;
#[cfg(test)]
mod e2e;
mod eval;
mod events;
mod first;
mod followup;
mod github;
mod github_app;
mod health;
mod home;
mod hygiene;
mod ledger;
mod links;
mod llm;
mod mcp;
mod me;
mod metrics;
mod mine;
mod model;
mod needs;
mod nixpkgs;
mod notifications;
mod notify;
mod osv;
mod profile;
mod progress;
mod prpage;
mod prpanel;
mod queue;
mod report;
mod rescan;
mod review;
mod scan;
mod score;
mod search;
mod secret;
mod sets;
mod severity;
mod status;
mod store;
mod subject;
mod trace;
mod ui;
mod upstream;
mod version;
mod web;
mod welcome;
mod workflows;

use anyhow::{Context, Result, bail};
use chrono::{Duration, Utc};
use clap::{Parser, Subcommand};
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::secret::Secret;
use crate::store::Store;
use crate::web::App;

/// Exit 3 means "not listed", not failure, so a script can tell a broken tool
/// apart from an answer it did not want.
const EXIT_NOT_LISTED: i32 = 3;

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
    /// nixpkgs channels to index next to master; `stable` is the newest
    /// stable channel.
    #[arg(
        long = "channel",
        global = true,
        env = "MAINTAINER_MODE_CHANNELS",
        value_delimiter = ',',
        default_value = "nixos-unstable,stable"
    )]
    channels: Vec<String>,
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
    /// The server that the client commands use.
    #[arg(
        long,
        global = true,
        env = "MAINTAINER_MODE_SERVER",
        default_value = sets::PUBLIC_SERVER
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
        /// A classic token of the bot account that watches nixpkgs, with the
        /// `notifications` scope. The sync uses it before any lent token, and
        /// the notifications poller is off without it.
        #[arg(long, env = "MAINTAINER_MODE_BOT_TOKEN_FILE")]
        bot_token_file: Option<PathBuf>,
        /// An NVD API key, for the CVEs whose record has no score. NVD is
        /// not asked without one.
        #[arg(long, env = "MAINTAINER_MODE_NVD_API_KEY_FILE")]
        nvd_api_key_file: Option<PathBuf>,
        /// A GitHub App whose installation token syncs first.
        #[arg(long, env = "GITHUB_APP_ID")]
        github_app_id: Option<String>,
        /// The App's PEM private key.
        #[arg(long, env = "GITHUB_APP_KEY_FILE")]
        github_app_key_file: Option<PathBuf>,
        /// Without it, the App's only installation is used.
        #[arg(long, env = "GITHUB_APP_INSTALLATION_ID")]
        github_app_installation_id: Option<u64>,
        /// The slug of the GitHub App that people install on their workflow
        /// repository, for the link on `/me/settings#workflows`.
        #[arg(long, env = "MAINTAINER_MODE_GITHUB_APP_SLUG")]
        github_app_slug: Option<String>,
        #[arg(long, default_value_t = 600)]
        sync_every: u64,
        #[command(flatten)]
        s3: Box<S3>,
        /// Repositories whose issues and pull requests the archive keeps in
        /// full; empty turns the archive off.
        #[arg(
            long = "archive-repo",
            env = "MAINTAINER_MODE_ARCHIVE_REPOS",
            value_delimiter = ',',
            default_value = archive::DEFAULT_REPO
        )]
        archive_repos: Vec<String>,
        /// Items the archive walk fetches per repository and pass.
        #[arg(long, env = "MAINTAINER_MODE_ARCHIVE_ITEMS", default_value_t = 300)]
        archive_items: usize,
        #[command(flatten)]
        rescan: Box<rescan::Args>,
        #[command(flatten)]
        smtp: Box<notify::SmtpArgs>,
    },
    /// The full copy of issues and pull requests
    Archive {
        #[command(subcommand)]
        command: ArchiveCommand,
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
    /// Open pull requests in one queue: foryou (needs --login or MAINTAINER_MODE_MCP_TOKEN), unowned,
    /// firsttime, bumps, all, or ready
    Prs {
        #[arg(long, default_value = "all", value_parser = api::QUEUES)]
        queue: String,
        /// With --queue ready
        #[arg(long, value_parser = api::SECTIONS)]
        section: Option<String>,
        #[arg(long)]
        login: Option<String>,
        #[arg(long, default_value_t = api::LIMIT)]
        limit: usize,
        /// The next_cursor of the page before
        #[arg(long)]
        cursor: Option<String>,
    },
    /// One person's review record: outcomes, not a rank
    Record { login: String },
    /// Exit 0 when the login is listed, 3 when it is not, 1 on error
    Check { login: String },
    /// A package, NixOS module or path: health, issues and pull requests
    Show { name: String },
    /// The packages and NixOS modules a login maintains
    Maintains { login: String },
    /// One pull request: its state, its checks and what it touches
    Pr { number: u64 },
    /// Packages, NixOS modules and paths that match a name
    Search { query: String },
    /// Small packages with no maintainer and a recent task, for a first
    /// contribution
    First {
        #[arg(long, default_value_t = api::LIMIT)]
        limit: usize,
    },
    /// What needs you, the most urgent first; needs MAINTAINER_MODE_MCP_TOKEN
    Needs {
        #[arg(long, value_parser = api::MOODS)]
        mood: Option<String>,
    },
    /// Your packages, as the filters of /packages; needs
    /// MAINTAINER_MODE_MCP_TOKEN
    Mine {
        #[arg(long, default_value = "mine", value_parser = api::FILTERS)]
        filter: String,
        /// With --filter machines: one of your sets
        #[arg(long)]
        set: Option<String>,
        /// With --set: one output label of that set, such as a host name
        #[arg(long)]
        output: Option<String>,
        /// Keep the names that hold this text
        #[arg(long)]
        query: Option<String>,
        #[arg(long, default_value_t = api::LIMIT)]
        limit: usize,
        /// The next_cursor of the page before
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Print the JSON of any GET endpoint, such as /api/v1/sets; a private
    /// one needs MAINTAINER_MODE_MCP_TOKEN
    Get { path: String },
    /// The state of nixpkgs, month by month
    Metrics {
        #[arg(long, default_value_t = 12)]
        months: usize,
    },
    /// Evaluate the closure of a flake output or of a file's attributes, on
    /// this machine, and write the full report
    Scan {
        #[arg(long, conflicts_with = "file")]
        flake: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        /// A flake output or a file attribute; repeat for several. Without
        /// it, a flake gives every output worth reading
        #[arg(long = "output", alias = "attr")]
        outputs: Vec<String>,
        /// The lock entry or npins pin of nixpkgs, when it is not `nixpkgs`
        #[arg(long)]
        nixpkgs_input: Option<String>,
        #[arg(long)]
        system: Option<String>,
        /// Read the built store paths instead of the derivations
        #[arg(long)]
        runtime: bool,
        /// An output to leave out, or the start of one, such as `checks`;
        /// repeat for several
        #[arg(long = "ignore")]
        ignore: Vec<String>,
        /// Experimental: also record the nixpkgs files that the outputs read,
        /// with iets. Local flakes only; the upload does not carry them
        #[arg(long)]
        files: bool,
        #[arg(long, default_value = SCAN_FILE)]
        out: PathBuf,
    },
    /// Write the strict report: only names in the public package list of
    /// --server
    Export {
        #[arg(long, default_value = SCAN_FILE)]
        report: PathBuf,
        /// Write here instead of stdout
        #[arg(long, short)]
        out: Option<PathBuf>,
        /// A saved copy of /api/v1/packages/index, instead of a download
        #[arg(long)]
        index_file: Option<PathBuf>,
        /// Also send the label of each output, such as host names, and the
        /// outputs that hold each package
        #[arg(long)]
        with_outputs: bool,
    },
    /// Show why the outputs of the last scan hold a package, with
    /// `nix why-depends`. It runs here and sends nothing
    Why {
        /// The package name, as the report has it, such as openssl
        pname: String,
        /// Only this output label, such as a host name
        #[arg(long)]
        output: Option<String>,
        #[arg(long, default_value = SCAN_FILE)]
        report: PathBuf,
    },
    /// Send the strict report to your package set on a server
    Submit {
        /// The server; --server when absent
        #[arg(long)]
        to: Option<String>,
        #[arg(long, default_value = SCAN_FILE)]
        report: PathBuf,
        /// File holding the upload token, instead of the one that `login`
        /// saves; MAINTAINER_MODE_UPLOAD_TOKEN wins over both
        #[arg(long)]
        token_file: Option<PathBuf>,
        /// A saved copy of /api/v1/packages/index, instead of a download
        #[arg(long)]
        index_file: Option<PathBuf>,
        /// The set this report replaces, such as `laptop`; the server uses
        /// `default` without it. Only this name goes, never the tree's.
        #[arg(long)]
        name: Option<String>,
        /// Make the set public on your profile: the public flake that you
        /// scanned, as REFERENCE#OUTPUT. Needs --name.
        #[arg(long, requires = "name")]
        public_flake: Option<String>,
        /// Also send the label of each output, such as host names, and the
        /// outputs that hold each package
        #[arg(long)]
        with_outputs: bool,
    },
    /// Get an upload token from --server: confirm a code on the site, and
    /// the token goes to ~/.config/maintainer-mode/upload-token. It
    /// replaces the token you had.
    Login,
}

#[derive(Subcommand)]
enum ArchiveCommand {
    /// Write the stored items as `<out>/<owner>/<name>/NNNNNNN.ndjson.zst`,
    /// one file per 10 000 numbers
    Export {
        #[arg(
            long,
            env = "MAINTAINER_MODE_DB",
            default_value = "maintainer-mode.sqlite"
        )]
        db: PathBuf,
        #[arg(long, default_value = archive::DEFAULT_REPO)]
        repo: String,
        #[arg(long)]
        out: PathBuf,
    },
}

const SCAN_FILE: &str = "maintainer-mode-scan.json";

fn print_omitted(r: &report::Upload) {
    eprintln!(
        "{} public packages, {} of them with a version that the list does not have; {} names kept back",
        r.packages.len(),
        r.modified(),
        r.omitted.unknown
    );
    if !r.outputs.is_empty() {
        eprintln!(
            "{} output labels: {}",
            r.outputs.len(),
            r.outputs.join(", ")
        );
    }
}

/// The public package list of the server the report goes to, or a saved copy.
async fn public_list(server: &str, file: Option<&std::path::Path>) -> Result<report::PublicList> {
    let bytes = match file {
        Some(f) => std::fs::read(f).with_context(|| format!("read {}", f.display()))?,
        None => {
            let url = format!("{}/api/v1/packages/index", server.trim_end_matches('/'));
            let resp = github::http().get(&url).send().await?;
            let status = resp.status();
            if !status.is_success() {
                let body: Value = resp.json().await.unwrap_or(Value::Null);
                return Err(api_error(&url, status, &body));
            }
            resp.bytes().await?.to_vec()
        }
    };
    report::PublicList::parse(&bytes)
}

async fn strict(
    server: &str,
    path: &std::path::Path,
    index_file: Option<&std::path::Path>,
    with_outputs: bool,
) -> Result<report::Upload> {
    let report = report::Report::load(path)?;
    if with_outputs && report.outputs.is_empty() {
        bail!(
            "{} has no outputs; scan again with this version",
            path.display()
        );
    }
    let list = public_list(server, index_file).await?;
    let upload = report.strict_with(&list, with_outputs);
    if upload.outputs.len() > report::MAX_OUTPUTS {
        bail!(
            "more than {} outputs; name fewer with --output",
            report::MAX_OUTPUTS
        );
    }
    for l in &upload.outputs {
        report::check_label(l).map_err(|e| anyhow::anyhow!("{l}: {e}"))?;
    }
    Ok(upload)
}

/// `REFERENCE#OUTPUT`, with the rules of a server rescan.
fn public_flake(s: &str) -> Result<report::PublicFlake> {
    let (reference, output) = s
        .split_once('#')
        .context("--public-flake is REFERENCE#OUTPUT, such as github:OWNER/REPO#nixosConfigurations.HOST.config.system.build.toplevel")?;
    rescan::check_reference(reference)
        .and_then(|()| rescan::check_output(output))
        .map_err(|e| anyhow::anyhow!("--public-flake: {e}"))?;
    Ok(report::PublicFlake {
        reference: reference.into(),
        output: output.into(),
    })
}

async fn submit(
    server: &str,
    path: &std::path::Path,
    token_file: Option<PathBuf>,
    index_file: Option<&std::path::Path>,
    name: Option<String>,
    flake: Option<String>,
    with_outputs: bool,
) -> Result<()> {
    if let Some(n) = &name {
        sets::check_name(n).map_err(|e| anyhow::anyhow!("--name: {e}"))?;
    }
    let flake = flake.as_deref().map(public_flake).transpose()?;
    let token = match std::env::var("MAINTAINER_MODE_UPLOAD_TOKEN") {
        Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => match token_file {
            Some(f) => read_secret(&Some(f))?.unwrap_or_default(),
            None => {
                let f = token_path()?;
                match std::fs::read_to_string(&f) {
                    Ok(t) => t.trim().to_string(),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        bail!("no upload token: run `maintainer-mode login` first")
                    }
                    Err(e) => return Err(e).with_context(|| format!("read {}", f.display())),
                }
            }
        },
    };
    let mut report = strict(server, path, index_file, with_outputs).await?;
    report.name = name;
    report.public_flake = flake;
    let url = format!("{}/api/v1/reports", server.trim_end_matches('/'));
    let resp = github::http()
        .post(&url)
        .bearer_auth(token)
        .json(&report)
        .send()
        .await?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(api_error(&url, status, &body));
    }
    print_omitted(&report);
    out!("{body}");
    Ok(())
}

fn token_path() -> Result<PathBuf> {
    let config = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config"),
    };
    Ok(config.join("maintainer-mode").join("upload-token"))
}

fn save_token(token: &str) -> Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let path = token_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("write {}", path.display()))?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    writeln!(f, "{token}")?;
    Ok(path)
}

/// The error of a server answer that is not a success, with its `error`.
fn api_error(url: &str, status: reqwest::StatusCode, body: &Value) -> anyhow::Error {
    anyhow::anyhow!(
        "{url}: {status}: {}",
        body["error"].as_str().unwrap_or("no reason given")
    )
}

async fn api_post(url: &str, body: &Value) -> Result<(reqwest::StatusCode, Value)> {
    let resp = github::http().post(url).json(body).send().await?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(api_error(url, status, &body));
    }
    Ok((status, body))
}

async fn login(server: &str) -> Result<()> {
    let base = server.trim_end_matches('/');
    let (_, start) = api_post(&format!("{base}/api/v1/login"), &Value::Null).await?;
    let device = start["device_code"].as_str().context("no device_code")?;
    let interval = start["interval"]
        .as_u64()
        .unwrap_or(cli_login::INTERVAL_SECONDS);
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(start["expires_in"].as_u64().unwrap_or(600));
    eprintln!(
        "Open {}\nand confirm the code {}.",
        start["url"].as_str().unwrap_or(base),
        start["user_code"].as_str().unwrap_or("?")
    );
    let poll = format!("{base}/api/v1/login/poll");
    let body = serde_json::json!({ "device_code": device });
    loop {
        if std::time::Instant::now() > deadline {
            bail!("the code expired; run login again");
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        let (status, v) = api_post(&poll, &body).await?;
        if status == reqwest::StatusCode::ACCEPTED {
            continue;
        }
        let token = v["token"].as_str().context("no token in the answer")?;
        let path = save_token(token)?;
        eprintln!(
            "Signed in as {}. The upload token is in {}.",
            v["login"].as_str().unwrap_or("?"),
            path.display()
        );
        return Ok(());
    }
}

/// Where the daily copy of the event log goes. Unset turns the copy off.
#[derive(clap::Args)]
struct S3 {
    /// An S3-compatible endpoint, such as a bunny.net Storage S3 endpoint.
    #[arg(long, env = "MAINTAINER_MODE_S3_ENDPOINT")]
    s3_endpoint: Option<String>,
    #[arg(long, env = "MAINTAINER_MODE_S3_BUCKET")]
    s3_bucket: Option<String>,
    #[arg(long, env = "MAINTAINER_MODE_S3_REGION")]
    s3_region: Option<String>,
    /// Holds `AWS_ACCESS_KEY_ID=…` and `AWS_SECRET_ACCESS_KEY=…` lines.
    #[arg(long, env = "MAINTAINER_MODE_S3_CREDENTIALS_FILE")]
    s3_credentials_file: Option<PathBuf>,
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
    github::settle_mergeable(&app.store, client, &app.merge_candidates()).await
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
        let pass_started = Utc::now();
        let index_at = app.store.get_time("index_at").ok().flatten();
        // an index from an older version lacks packages, or their meta; the
        // hour stops a sweep that keeps finding no meta from running each pass
        let (empty, no_meta) = {
            let i = app.index.read().unwrap();
            (
                i.pkgs.is_empty(),
                !i.pkgs.values().any(|p| p.meta.is_some()),
            )
        };
        let due = |h: i64| index_at.is_none_or(|t| Utc::now() - t > Duration::hours(h));
        if empty || (no_meta && due(1)) || due(24) {
            match web::refresh_index(&app).await {
                Ok(()) => {
                    tracing::info!("nixpkgs index refreshed");
                    if let Err(e) = app.recompute() {
                        tracing::warn!("recompute: {e:#}");
                    }
                }
                Err(e) => tracing::warn!("nixpkgs index: {e:#}"),
            }
        }
        match app
            .store
            .purge_package_sets(Utc::now() - Duration::days(sets::RETENTION_DAYS))
        {
            Ok(0) => {}
            Ok(n) => tracing::info!("{n} expired package sets deleted"),
            Err(e) => tracing::warn!("package sets: {e:#}"),
        }
        match app.store.purge_sessions() {
            Ok(0) => {}
            Ok(n) => tracing::info!("{n} expired sessions deleted"),
            Err(e) => tracing::warn!("sessions: {e:#}"),
        }
        match app
            .store
            .purge_flake_results(Utc::now() - Duration::days(sets::RETENTION_DAYS))
        {
            Ok(0) => {}
            Ok(n) => tracing::info!("{n} expired flake scans dropped"),
            Err(e) => tracing::warn!("flake scans: {e:#}"),
        }
        // Only the GitHub sync decides whether the host is offline: one other
        // host that stays down must not hold back the rest of the pass.
        let mut down = false;
        let mut tokens = Vec::new();
        if let Some(a) = &app.github_app {
            match a.token().await {
                Ok(t) => tokens.push(t),
                Err(e) => {
                    down = offline(&e);
                    tracing::warn!("GitHub App token: {e:#}");
                }
            }
        }
        tokens.extend(app.bot_token.iter().map(|s| s.get()));
        let server_tokens = tokens.clone();
        tokens.extend(app.store.lent_tokens().unwrap_or_default());
        let mut client = github::Client::new(tokens);
        let walk = notifications::walk_due(
            app.notifications.healthy(pass_started),
            app.store.get("open_done").ok().flatten().is_some()
                && app.store.get("issues_open_done").ok().flatten().is_some(),
            app.store.get_time("walk_at").ok().flatten(),
            pass_started,
        );
        if client.has_token() && !walk {
            tracing::info!("the notifications poller is healthy; the full walk waits");
        } else if client.has_token() {
            match github::sync(&app.store, &mut client, ledger::WINDOW_DAYS).await {
                Ok(n) => {
                    down = false;
                    let _ = app.store.set("walk_at", &pass_started.to_rfc3339());
                    tracing::info!("synced {n} pull requests");
                }
                Err(e) => {
                    down = offline(&e);
                    tracing::warn!("sync: {e:#}");
                }
            }
        } else if !down {
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
            if walk {
                match github::sync_issues(&app.store, &mut client, ledger::WINDOW_DAYS).await {
                    Ok(n) => tracing::info!("synced {n} issues"),
                    Err(e) => tracing::warn!("issues: {e:#}"),
                }
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
        if let Err(e) = committer::refresh_team(&app.store, server_tokens).await {
            tracing::info!("committers team: {e:#}");
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
        notify::recount(&app);
        let a = app.clone();
        match tokio::task::spawn_blocking(move || a.recompute_metrics()).await {
            Ok(Err(e)) => tracing::warn!("metrics: {e:#}"),
            Err(e) => tracing::warn!("metrics: {e}"),
            Ok(Ok(())) => {}
        }
        let _ = app.store.set("pass_ended", &Utc::now().to_rfc3339());
        let _ = app.store.set(
            "pass_seconds",
            &(Utc::now() - pass_started).num_seconds().to_string(),
        );
        tokio::time::sleep(std::time::Duration::from_secs(every)).await;
    }
}

/// The private endpoints take the MCP token as a bearer token.
async fn fetch(server: &str, path: &str) -> Result<Value> {
    let url = format!("{}{path}", server.trim_end_matches('/'));
    let mut req = github::http().get(&url);
    if let Some(t) = std::env::var("MAINTAINER_MODE_MCP_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty())
    {
        req = req.bearer_auth(t.trim());
    }
    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        let v: Value = resp.json().await.unwrap_or_default();
        return Err(api_error(&url, status, &v));
    }
    Ok(resp.json().await?)
}

fn query(pairs: &[(&str, Option<&str>)]) -> String {
    let q: Vec<String> = pairs
        .iter()
        .filter_map(|(k, v)| v.map(|v| format!("{k}={}", github::urlencode(v))))
        .collect();
    q.join("&")
}

fn print_entries(v: &Value) -> Result<()> {
    let waiting = v["queue"] == "firsttime";
    for e in v["items"].as_array().into_iter().flatten() {
        let age = e["created_at"]
            .as_str()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .filter(|_| waiting)
            .map(|t| format!("  (waiting {})", web::age_words(t.to_utc())))
            .unwrap_or_default();
        out!(
            "#{:<7} {:<20} {}{age}",
            e["number"],
            e["author"].as_str().unwrap_or(""),
            e["title"].as_str().unwrap_or("")
        );
    }
    if let Some(c) = v["next_cursor"].as_str() {
        eprintln!("{} in all; next page: --cursor {c}", v["total"]);
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
            bot_token_file,
            nvd_api_key_file,
            github_app_id,
            github_app_key_file,
            github_app_installation_id,
            github_app_slug,
            sync_every,
            s3,
            archive_repos,
            archive_items,
            rescan,
            smtp,
        } => {
            let rescan = rescan.config()?;
            let smtp = smtp.config(&public_url)?;
            let S3 {
                s3_endpoint,
                s3_bucket,
                s3_region,
                s3_credentials_file,
            } = *s3;
            let bucket = match (s3_endpoint, s3_bucket, s3_region, s3_credentials_file) {
                (Some(e), Some(b), Some(r), Some(c)) => Some(events::Bucket::new(&e, &b, &r, &c)?),
                (None, None, None, None) => None,
                _ => bail!(
                    "set all of MAINTAINER_MODE_S3_ENDPOINT, _BUCKET, _REGION and _CREDENTIALS_FILE, or none"
                ),
            };
            let store = Store::open(&db)?;
            // the NVD terms of use ask for a notice while the API is used
            match &nvd_api_key_file {
                Some(_) => store.set("nvd_api", "1")?,
                None => store.delete_key("nvd_api")?,
            }
            let github_app = match (github_app_id, Secret::open(github_app_key_file)?) {
                (Some(id), Some(key)) => Some(github_app::GithubApp::new(
                    id,
                    key,
                    github_app_installation_id,
                )?),
                (None, None) => None,
                _ => bail!("set both GITHUB_APP_ID and GITHUB_APP_KEY_FILE, or neither"),
            };
            if let Some(a) = &github_app {
                tracing::info!("GitHub App {} syncs first", a.id());
            }
            let events = Arc::new(events::Log::open(&data_dir(&db).join("events"))?);
            let oauth = match (client_id, Secret::open(client_secret_file)?) {
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
            let bot_token = Secret::open(bot_token_file)?.map(Arc::new);
            let app = Arc::new(App {
                index: RwLock::new(web::load_index(&store)),
                health: RwLock::new(Arc::new(web::load_health(&store))),
                channels: RwLock::new(Arc::new(channels::load(&store, &cli.channels))),
                store,
                oauth,
                bot_token: bot_token.clone(),
                github_app,
                github_app_slug: github_app_slug.filter(|s| !s.is_empty()),
                snapshot: Default::default(),
                secure_cookies: public_url.starts_with("https://"),
                hydra: Default::default(),
                public_url: public_url.clone(),
                data_dir: data_dir(&db),
                index_sets: sets.clone(),
                metrics: Default::default(),
                packages: Default::default(),
                events,
                archive: archive::open_all(
                    &data_dir(&db).join("archive"),
                    &archive_repos,
                    archive_items,
                )?,
                rescan,
                inbox: Default::default(),
                smtp,
                pr_cache: Default::default(),
                notifications: if bot_token.is_some() {
                    notifications::Status::on()
                } else {
                    Default::default()
                },
            });
            app.recompute()?;
            notify::recount(&app);
            let a = app.clone();
            tokio::task::spawn_blocking(move || a.recompute_metrics()).await??;
            app.store.set("sync_every", &sync_every.to_string())?;
            tokio::spawn(background(app.clone(), sync_every, cli.history_since));
            tokio::spawn(health::run(app.clone(), Secret::open(nvd_api_key_file)?));
            tokio::spawn(channels::run(app.clone()));
            tokio::spawn(builds::run(app.clone()));
            match bot_token {
                Some(t) => {
                    tokio::spawn(events::poll(app.events.clone(), t.clone()));
                    tracing::info!("notifications poller on");
                    tokio::spawn(notifications::run(app.clone(), t));
                }
                None => tracing::info!(
                    "no bot token: the event log takes GH Archive only, and the notifications poller is off"
                ),
            }
            tokio::spawn(events::archive(app.clone()));
            if !app.archive.is_empty() {
                tokio::spawn(archive::walk(app.clone()));
            }
            tokio::spawn(rescan::run(app.clone()));
            tokio::spawn(notify::run(app.clone()));
            if let Some(b) = bucket {
                tracing::info!("event copy to {}/{}", b.endpoint, b.bucket);
                tokio::spawn(events::copy(app.clone(), b));
            }
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
            let events = Arc::new(events::Log::open(&data_dir(&db).join("events"))?);
            let app = App {
                index: RwLock::new(web::load_index(&store)),
                health: RwLock::new(Arc::new(web::load_health(&store))),
                channels: RwLock::new(Arc::new(channels::load(&store, &cli.channels))),
                store,
                oauth: None,
                bot_token: None,
                github_app: None,
                github_app_slug: None,
                snapshot: Default::default(),
                secure_cookies: false,
                hydra: Default::default(),
                public_url: String::new(),
                data_dir: data_dir(&db),
                index_sets: sets.clone(),
                metrics: Default::default(),
                packages: Default::default(),
                events,
                archive: Vec::new(),
                rescan: None,
                inbox: Default::default(),
                smtp: None,
                pr_cache: Default::default(),
                notifications: Default::default(),
            };
            if index {
                web::refresh_index(&app).await?;
            }
            let mut client = github::Client::new(vec![token]);
            let n = github::sync(&app.store, &mut client, ledger::WINDOW_DAYS).await?;
            eprintln!("synced {n} pull requests");
            let n = github::sync_issues(&app.store, &mut client, ledger::WINDOW_DAYS).await?;
            eprintln!("synced {n} issues");
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
            let events = Arc::new(events::Log::open(&data_dir(&db).join("events"))?);
            let app = App {
                index: RwLock::new(web::load_index(&store)),
                health: RwLock::new(Arc::new(web::load_health(&store))),
                channels: RwLock::new(Arc::new(channels::load(&store, &cli.channels))),
                store,
                oauth: None,
                bot_token: None,
                github_app: None,
                github_app_slug: None,
                snapshot: Default::default(),
                secure_cookies: false,
                hydra: Default::default(),
                public_url: String::new(),
                data_dir: data_dir(&db),
                index_sets: sets.clone(),
                metrics: Default::default(),
                packages: Default::default(),
                events,
                archive: Vec::new(),
                rescan: None,
                inbox: Default::default(),
                smtp: None,
                pr_cache: Default::default(),
                notifications: Default::default(),
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
        Command::Prs {
            queue,
            section,
            login,
            limit,
            cursor,
        } => {
            let path = format!(
                "/api/v1/prs?limit={limit}&{}",
                query(&[
                    ("queue", Some(&queue)),
                    ("section", section.as_deref()),
                    ("login", login.as_deref()),
                    ("cursor", cursor.as_deref()),
                ])
            );
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
        Command::Pr { number } => {
            let v = fetch(&cli.server, &format!("/api/v1/prs/{number}")).await?;
            if cli.json {
                out!("{v}");
                return Ok(0);
            }
            out!(
                "#{} {}\n  by {}  {}  base {}  checks {}  queue {}",
                v["number"],
                v["title"].as_str().unwrap_or(""),
                v["author"].as_str().unwrap_or(""),
                v["state"].as_str().unwrap_or(""),
                v["base"].as_str().unwrap_or(""),
                v["ci"].as_str().unwrap_or("-"),
                v["queue"]["queue"].as_str().unwrap_or("-")
            );
            for t in v["touches"].as_array().into_iter().flatten() {
                out!("  touches {}", t["name"].as_str().unwrap_or(""));
            }
            out!("  {}", v["url"].as_str().unwrap_or(""));
            Ok(0)
        }
        Command::Search { query: q } => {
            let path = format!("/api/v1/search?{}", query(&[("q", Some(&q))]));
            let v = fetch(&cli.server, &path).await?;
            if cli.json {
                out!("{v}");
                return Ok(0);
            }
            for r in v["results"].as_array().into_iter().flatten() {
                out!(
                    "{:<40} risk {}",
                    r["key"].as_str().unwrap_or(""),
                    r["flags"]["risk"]
                );
            }
            for p in v["people"].as_array().into_iter().flatten() {
                out!("@{}", p["login"].as_str().unwrap_or(""));
            }
            for p in v["pull_requests"].as_array().into_iter().flatten() {
                out!(
                    "#{:<8} {:<7} {}",
                    p["number"],
                    p["state"].as_str().unwrap_or(""),
                    p["title"].as_str().unwrap_or("")
                );
            }
            for a in v["advisories"].as_array().into_iter().flatten() {
                let pkgs: Vec<&str> = a["packages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p.as_str())
                    .collect();
                out!(
                    "{:<24} affects {}",
                    a["id"].as_str().unwrap_or(""),
                    pkgs.join(", ")
                );
            }
            Ok(0)
        }
        Command::First { limit } => {
            let v = fetch(&cli.server, &format!("/api/v1/first?limit={limit}")).await?;
            if cli.json {
                out!("{v}");
                return Ok(0);
            }
            for p in v["items"].as_array().into_iter().flatten() {
                let t = &p["task"];
                out!(
                    "{:<30} {:<6} #{:<7} {}",
                    p["attr"].as_str().unwrap_or(""),
                    t["kind"].as_str().unwrap_or(""),
                    t["number"],
                    t["title"].as_str().unwrap_or("")
                );
            }
            Ok(0)
        }
        Command::Needs { mood } => {
            let path = format!("/api/v1/needs?{}", query(&[("mood", mood.as_deref())]));
            let v = fetch(&cli.server, &path).await?;
            if cli.json {
                out!("{v}");
                return Ok(0);
            }
            for i in v["items"].as_array().into_iter().flatten() {
                out!(
                    "{:<9} {}  {}",
                    i["cause"].as_str().unwrap_or(""),
                    i["title"].as_str().unwrap_or(""),
                    i["href"].as_str().unwrap_or("")
                );
            }
            Ok(0)
        }
        Command::Mine {
            filter,
            set,
            output,
            query: q,
            limit,
            cursor,
        } => {
            let path = format!(
                "/api/v1/mine?limit={limit}&{}",
                query(&[
                    ("filter", Some(&filter)),
                    ("set", set.as_deref()),
                    ("output", output.as_deref()),
                    ("q", q.as_deref()),
                    ("cursor", cursor.as_deref()),
                ])
            );
            let v = fetch(&cli.server, &path).await?;
            if cli.json {
                out!("{v}");
                return Ok(0);
            }
            for r in v["items"].as_array().into_iter().flatten() {
                let why: Vec<&str> = r["why"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                out!(
                    "{:<40} {:<24} risk {}",
                    r["key"].as_str().unwrap_or(""),
                    why.join(","),
                    r["flags"]["risk"]
                );
            }
            if let Some(c) = v["next_cursor"].as_str() {
                eprintln!("{} in all; next page: --cursor {c}", v["total"]);
            }
            Ok(0)
        }
        Command::Get { path } => {
            let path = if path.starts_with("/api/") {
                path
            } else {
                format!("/api/v1/{}", path.trim_start_matches('/'))
            };
            let v = fetch(&cli.server, &path).await?;
            out!("{v}");
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
            if let Some(r) = f["risk"].as_u64() {
                out!("  risk {r} ({})", f["risk_band"].as_str().unwrap_or(""));
                for reason in f["risk_reasons"].as_array().into_iter().flatten() {
                    out!(
                        "    +{} {}",
                        reason["points"],
                        reason["text"].as_str().unwrap_or("")
                    );
                }
            }
            if let Some(u) = f["upstream"].as_str() {
                out!("  upstream {u}");
            }
            for a in v["advisories"].as_array().into_iter().flatten() {
                out!(
                    "  advisory {}  {:<8} {}",
                    a["code"].as_str().unwrap_or(""),
                    a["severity"]["band"].as_str().unwrap_or("-"),
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
                &format!("/api/v1/people/{}", github::urlencode(&login)),
            )
            .await?;
            if cli.json {
                out!("{}", v["maintains"]);
                return Ok(0);
            }
            for kind in ["packages", "modules"] {
                for r in v["maintains"][kind].as_array().into_iter().flatten() {
                    let f = &r["flags"];
                    out!(
                        "{:<40} {:<14} risk {:<3} upstream {:<12} advisories {:<2} prs {:<3} issues {}",
                        r["name"].as_str().unwrap_or(""),
                        r["version"].as_str().unwrap_or(""),
                        f["risk"]
                            .as_u64()
                            .map(|r| r.to_string())
                            .unwrap_or("-".into()),
                        f["upstream"].as_str().unwrap_or("-"),
                        f["open_advisories"],
                        f["open_prs"],
                        f["open_issues"]
                    );
                }
            }
            Ok(0)
        }
        Command::Scan {
            flake,
            file,
            outputs,
            nixpkgs_input,
            system,
            runtime,
            ignore,
            files,
            out,
        } => {
            let mut target = scan::Target {
                flake,
                file,
                outputs,
                nixpkgs_input,
                system,
                runtime,
                ignore,
                discovered: false,
            };
            target.choose_outputs()?;
            let mut r = scan::scan(&target)?;
            if files {
                let t = trace::trace(&target)?;
                eprintln!(
                    "{} nixpkgs files, {} modules kept, {} module files dropped",
                    t.files.len(),
                    t.modules.len(),
                    t.modules_dropped
                );
                r.files = Some(t);
            }
            r.write(&out)?;
            eprintln!("{} packages -> {}", r.packages.len(), out.display());
            Ok(0)
        }
        Command::Export {
            report: path,
            out,
            index_file,
            with_outputs,
        } => {
            let r = strict(&cli.server, &path, index_file.as_deref(), with_outputs).await?;
            let text = serde_json::to_string_pretty(&r)?;
            match out {
                Some(o) => {
                    std::fs::write(&o, text).with_context(|| format!("write {}", o.display()))?;
                    print_omitted(&r);
                }
                None => out!("{text}"),
            }
            Ok(0)
        }
        Command::Submit {
            to,
            report: path,
            token_file,
            index_file,
            name,
            public_flake,
            with_outputs,
        } => {
            submit(
                to.as_deref().unwrap_or(&cli.server),
                &path,
                token_file,
                index_file.as_deref(),
                name,
                public_flake,
                with_outputs,
            )
            .await?;
            Ok(0)
        }
        Command::Login => {
            login(&cli.server).await?;
            Ok(0)
        }
        Command::Why {
            pname,
            output,
            report: path,
        } => {
            scan::why(&report::Report::load(&path)?, &pname, output.as_deref())?;
            Ok(0)
        }
        Command::Archive {
            command: ArchiveCommand::Export { db, repo, out },
        } => {
            let a = archive::Archive::open_existing(&data_dir(&db).join("archive"), &repo)?;
            let (items, files) = a.export(&out)?;
            eprintln!(
                "{items} items of {} in {files} files under {}",
                a.repo,
                out.display()
            );
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
    fn the_command_line_has_unique_arguments() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

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
