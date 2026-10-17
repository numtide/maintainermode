//! Server-side rescans of a public flake reference. This evaluates
//! third-party code, so each step runs as its own process in a bubblewrap
//! sandbox, under a memory and a time limit. Off by default; see
//! docs/rescan.md for the threat model.

use anyhow::{Context, Result, anyhow, bail};
use axum::Router;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::time::Instant;

use crate::report::{Found, MAX_PACKAGES, Report, SCHEMA, Source};
use crate::store::{FlakeRef, PackageSet};
use crate::web::{App, Shared, ago, esc, icon, octicon};

const MAX_REFERENCE: usize = 256;
const MAX_OUTPUT_NAME: usize = 256;
const MAX_STDOUT: u64 = 512 << 20;
const MAX_STDERR: usize = 8 << 10;
const MAX_ERROR: usize = 2000;
const MAX_LOCK: u64 = 1 << 20;
const WORK: &str = "/work";

#[derive(clap::Args)]
pub struct Args {
    /// Scan registered public flakes on the server, in a bubblewrap
    /// sandbox. Off by default; see docs/rescan.md.
    #[arg(
        long = "rescan",
        env = "MAINTAINER_MODE_RESCAN",
        value_parser = clap::builder::BoolishValueParser::new()
    )]
    enable: bool,
    #[arg(
        long = "rescan-bwrap",
        env = "MAINTAINER_MODE_RESCAN_BWRAP",
        default_value = "bwrap"
    )]
    bwrap: String,
    #[arg(
        long = "rescan-prlimit",
        env = "MAINTAINER_MODE_RESCAN_PRLIMIT",
        default_value = "prlimit"
    )]
    prlimit: String,
    /// Plain nix, not iets: the sandbox runs `flake prefetch`, `flake
    /// archive` and `derivation show`. It must live in /nix/store.
    #[arg(
        long = "rescan-nix",
        env = "MAINTAINER_MODE_RESCAN_NIX",
        default_value = "nix"
    )]
    nix: String,
    #[arg(
        long = "rescan-max-flakes",
        env = "MAINTAINER_MODE_RESCAN_MAX_FLAKES",
        default_value_t = 3
    )]
    max_flakes: usize,
    #[arg(
        long = "rescan-per-day",
        env = "MAINTAINER_MODE_RESCAN_PER_DAY",
        default_value_t = 4
    )]
    per_day: usize,
    /// Scans in 24 hours for all people together.
    #[arg(
        long = "rescan-per-day-total",
        env = "MAINTAINER_MODE_RESCAN_PER_DAY_TOTAL",
        default_value_t = 100
    )]
    per_day_total: usize,
    /// Wall-clock limit of one scan, all steps together.
    #[arg(
        long = "rescan-timeout",
        env = "MAINTAINER_MODE_RESCAN_TIMEOUT",
        default_value_t = 900
    )]
    timeout_seconds: u64,
    /// Address-space limit of each sandboxed process.
    #[arg(
        long = "rescan-memory-mb",
        env = "MAINTAINER_MODE_RESCAN_MEMORY_MB",
        default_value_t = 3072
    )]
    memory_mb: u64,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub bwrap: String,
    pub prlimit: String,
    pub nix: PathBuf,
    pub max_flakes: usize,
    pub per_day: usize,
    pub per_day_total: usize,
    pub timeout: std::time::Duration,
    pub memory_mb: u64,
}

impl Args {
    pub fn config(&self) -> Result<Option<Config>> {
        if !self.enable {
            return Ok(None);
        }
        let nix = which(&self.nix)
            .and_then(|p| p.canonicalize().ok())
            .with_context(|| format!("find {} for the rescans", self.nix))?;
        if !nix.starts_with("/nix/store/") {
            bail!(
                "the nix for the rescans is {}; it must be in /nix/store, which the sandbox sees",
                nix.display()
            );
        }
        if self.max_flakes == 0
            || self.per_day == 0
            || self.per_day_total == 0
            || self.timeout_seconds == 0
        {
            bail!("the rescan limits must be above zero");
        }
        Ok(Some(Config {
            bwrap: self.bwrap.clone(),
            prlimit: self.prlimit.clone(),
            nix,
            max_flakes: self.max_flakes,
            per_day: self.per_day,
            per_day_total: self.per_day_total,
            timeout: std::time::Duration::from_secs(self.timeout_seconds),
            memory_mb: self.memory_mb.max(256),
        }))
    }
}

fn which(program: &str) -> Option<PathBuf> {
    if program.contains('/') {
        return Some(PathBuf::from(program));
    }
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| Path::new(d).join(program))
        .find(|p| p.is_file())
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/me/sets/flakes", post(add))
        .route("/me/sets/flakes/delete", post(remove))
}

fn name_ok(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// A DNS name under a public top-level domain. No IP literal, port or
/// user: a fetch must not reach the host itself or its private network by
/// name. A public name can still resolve to a private address; see
/// docs/rescan.md.
fn public_host(host: &str) -> bool {
    const PRIVATE: [&str; 13] = [
        "localhost",
        "local",
        "localdomain",
        "internal",
        "intranet",
        "lan",
        "home",
        "corp",
        "arpa",
        "test",
        "invalid",
        "example",
        "onion",
    ];
    let host = host.to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    let tld = labels.last().copied().unwrap_or("");
    host.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        && tld.chars().all(|c| c.is_ascii_alphabetic())
        && !PRIVATE.contains(&tld)
}

/// `github:OWNER/REPO`, `github:OWNER/REPO/REF`, or `https://HOST/PATH`.
/// No query and no fragment: the server adds the hash it pins.
pub fn check_reference(r: &str) -> Result<(), String> {
    if r.len() > MAX_REFERENCE {
        return Err(format!(
            "a reference has at most {MAX_REFERENCE} characters"
        ));
    }
    if let Some(rest) = r.strip_prefix("github:") {
        let parts: Vec<&str> = rest.split('/').collect();
        let owner = parts[0];
        let ok = matches!(parts.len(), 2 | 3)
            && name_ok(owner, 39)
            && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && name_ok(parts[1], 100)
            && parts.get(2).is_none_or(|p| name_ok(p, 100));
        return if ok {
            Ok(())
        } else {
            Err("write github:OWNER/REPO or github:OWNER/REPO/BRANCH, with no query".into())
        };
    }
    if let Some(rest) = r.strip_prefix("https://") {
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        if !public_host(host) {
            return Err("the host must be a public DNS name, with no port and no user".into());
        }
        let ok = path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '/'))
            && !path.split('/').any(|s| s == "." || s == "..");
        return if ok {
            Ok(())
        } else {
            Err("the path of the URL can hold letters, digits and - _ . ~ / only".into())
        };
    }
    Err("use github:OWNER/REPO or an https:// URL".into())
}

/// An attribute path such as `nixosConfigurations.web.config.system.build.toplevel`.
pub fn check_output(o: &str) -> Result<(), String> {
    let segments: Vec<&str> = o.split('.').collect();
    let ok = o.len() <= MAX_OUTPUT_NAME
        && segments.len() <= 32
        && segments.iter().all(|s| {
            !s.is_empty()
                && !s.starts_with('-')
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '\''))
        });
    if ok {
        Ok(())
    } else {
        Err(
            "the output is an attribute path: names of letters, digits and - _ ' joined by dots"
                .into(),
        )
    }
}

fn https_ok(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    url.len() <= 2048
        && public_host(host)
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Submodules and LFS make git fetch URLs that the lock file does not name,
/// so the server cannot check them.
fn git_follows_urls(locked: &Value, url: &str) -> bool {
    let set = |k: &str| locked.get(k).is_some_and(|v| v != &Value::Bool(false));
    let in_query = url.split_once('?').is_some_and(|(_, q)| {
        q.split('&')
            .any(|p| matches!(p.split('=').next(), Some("submodules" | "lfs")))
    });
    set("submodules") || set("lfs") || in_query
}

/// Every input that the lock file names must be one the sandbox may fetch:
/// a forge archive, or an https URL on a public host.
pub fn check_lock(text: &str) -> Result<(), String> {
    let lock: Value =
        serde_json::from_str(text).map_err(|e| format!("flake.lock does not parse: {e}"))?;
    let nodes = lock["nodes"].as_object().ok_or("flake.lock has no nodes")?;
    let root = lock["root"].as_str().unwrap_or("root");
    for (name, node) in nodes {
        if name == root {
            continue;
        }
        let locked = &node["locked"];
        let kind = locked["type"].as_str().unwrap_or("");
        let text = |k: &str| locked[k].as_str();
        let ok = match kind {
            "github" | "gitlab" | "sourcehut" => {
                locked.get("host").is_none()
                    && ["owner", "repo"]
                        .iter()
                        .all(|k| text(k).is_some_and(|v| name_ok(v.trim_start_matches('~'), 100)))
                    && text("rev").is_some_and(|r| r.chars().all(|c| c.is_ascii_hexdigit()))
            }
            "tarball" | "file" => text("url").is_some_and(https_ok),
            "git" => text("url").is_some_and(|u| https_ok(u) && !git_follows_urls(locked, u)),
            "path" => text("path")
                .is_some_and(|p| !p.starts_with('/') && !p.split('/').any(|s| s == "..")),
            _ => false,
        };
        if !ok {
            let kind = if kind.is_empty() { "unlocked" } else { kind };
            return Err(format!(
                "flake.lock input {name:?} is a {kind} input that the sandbox does not fetch"
            ));
        }
    }
    Ok(())
}

/// The reference, held to the tree that the first step fetched and checked.
fn pinned(reference: &str, prefetch: &Value) -> Result<String> {
    let hash = prefetch["hash"]
        .as_str()
        .filter(|h| {
            h.starts_with("sha256-")
                && h.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '+' | '/' | '='))
        })
        .context("nix flake prefetch gave no hash")?;
    let base = if reference.starts_with("github:") {
        let locked = &prefetch["locked"];
        let field = |k: &str| {
            locked[k]
                .as_str()
                .filter(|v| name_ok(v, 100))
                .with_context(|| format!("nix flake prefetch gave no {k}"))
        };
        let rev = field("rev")?;
        if !rev.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("nix flake prefetch gave a revision that is not a commit");
        }
        format!("github:{}/{}/{rev}", field("owner")?, field("repo")?)
    } else {
        reference.to_string()
    };
    Ok(format!("{base}?narHash={}", crate::github::urlencode(hash)))
}

/// What the sandbox takes from the host, read once per scan.
struct Host {
    path: String,
    ca: Option<PathBuf>,
    resolv: Option<PathBuf>,
}

impl Host {
    fn read(nix: &Path) -> Self {
        let mut dirs: Vec<String> = nix
            .parent()
            .map(|d| d.to_string_lossy().into_owned())
            .into_iter()
            .collect();
        for d in std::env::var("PATH").unwrap_or_default().split(':') {
            if d.starts_with("/nix/store/") && !dirs.iter().any(|x| x == d) {
                dirs.push(d.to_string());
            }
        }
        let ca = ["NIX_SSL_CERT_FILE", "SSL_CERT_FILE"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .chain(["/etc/ssl/certs/ca-certificates.crt".to_string()])
            .find_map(|p| std::fs::canonicalize(p).ok());
        Host {
            path: dirs.join(":"),
            ca,
            resolv: std::fs::canonicalize("/etc/resolv.conf").ok(),
        }
    }
}

const CA_INSIDE: &str = "/etc/ssl/certs/ca-bundle.crt";

/// The full command line of one sandboxed nix step. `net` shares the host
/// network; without it the sandbox has only a loopback device.
fn argv(cfg: &Config, host: &Host, work: &Path, net: bool, nix_args: &[&str]) -> Vec<String> {
    let mut a: Vec<String> = vec![
        cfg.prlimit.clone(),
        format!("--as={}", cfg.memory_mb << 20),
        format!("--fsize={}", 4u64 << 30),
        format!("--cpu={}", cfg.timeout.as_secs()),
        "--core=0".into(),
        "--nofile=4096".into(),
        "--".into(),
        cfg.bwrap.clone(),
    ];
    let mut push = |xs: &[&str]| a.extend(xs.iter().map(|s| s.to_string()));
    push(&[
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
        "--ro-bind",
        "/nix/store",
        "/nix/store",
        "--ro-bind",
        "/nix/var/nix/daemon-socket",
        "/nix/var/nix/daemon-socket",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
    ]);
    let work = work.to_string_lossy();
    push(&["--bind", &work, WORK, "--chdir", WORK]);
    if net {
        push(&["--share-net"]);
        if let Some(r) = &host.resolv {
            push(&["--ro-bind", &r.to_string_lossy(), "/etc/resolv.conf"]);
        }
        if let Some(c) = &host.ca {
            push(&["--ro-bind", &c.to_string_lossy(), CA_INSIDE]);
        }
    }
    let nix = cfg.nix.to_string_lossy();
    push(&[
        "--",
        &nix,
        "--extra-experimental-features",
        "nix-command flakes",
        "--store",
        "daemon",
        "--option",
        "accept-flake-config",
        "false",
        "--option",
        "allow-import-from-derivation",
        "false",
        "--option",
        "flake-registry",
        "",
    ]);
    push(nix_args);
    a
}

fn env(host: &Host) -> Vec<(&'static str, String)> {
    vec![
        ("PATH", host.path.clone()),
        ("HOME", WORK.into()),
        ("XDG_CACHE_HOME", format!("{WORK}/cache")),
        ("XDG_CONFIG_HOME", format!("{WORK}/config")),
        ("XDG_DATA_HOME", format!("{WORK}/data")),
        ("XDG_STATE_HOME", format!("{WORK}/state")),
        ("TMPDIR", "/tmp".into()),
        ("NIX_SSL_CERT_FILE", CA_INSIDE.into()),
    ]
}

fn tail(stderr: &[u8], lines: usize) -> String {
    let text = String::from_utf8_lossy(stderr);
    let mut last: Vec<&str> = text
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(lines)
        .collect();
    last.reverse();
    last.join("\n")
}

async fn sandboxed(
    cfg: &Config,
    host: &Host,
    work: &Path,
    net: bool,
    nix_args: &[&str],
    deadline: Instant,
) -> Result<Vec<u8>> {
    let a = argv(cfg, host, work, net, nix_args);
    let mut child = tokio::process::Command::new(&a[0])
        .args(&a[1..])
        .env_clear()
        .envs(env(host))
        .current_dir(work)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("run {}", a[0]))?;
    let mut stdout = child.stdout.take().context("no stdout")?;
    let mut stderr = child.stderr.take().context("no stderr")?;
    let run = async {
        let out = async {
            let mut buf = Vec::new();
            (&mut stdout)
                .take(MAX_STDOUT + 1)
                .read_to_end(&mut buf)
                .await?;
            if buf.len() as u64 > MAX_STDOUT {
                bail!("the output is larger than {} MiB", MAX_STDOUT >> 20);
            }
            Ok(buf)
        };
        let err = async {
            let mut kept = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let n = stderr.read(&mut chunk).await?;
                if n == 0 {
                    break;
                }
                kept.extend_from_slice(&chunk[..n]);
                if kept.len() > MAX_STDERR {
                    kept.drain(..kept.len() - MAX_STDERR);
                }
            }
            Ok::<_, anyhow::Error>(kept)
        };
        let (out, err) = tokio::try_join!(out, err)?;
        let status = child.wait().await?;
        Ok::<_, anyhow::Error>((status, out, err))
    };
    let (status, out, err) = match tokio::time::timeout_at(deadline, run).await {
        Ok(r) => r?,
        Err(_) => bail!("stopped at the limit of {} s", cfg.timeout.as_secs()),
    };
    if !status.success() {
        bail!("{status}: {}", tail(&err, 12));
    }
    Ok(out)
}

/// The server side of the upload rule, for a closure the sandbox read: the
/// same strict filter as the client, then the same check as an upload.
fn accept(
    app: &App,
    pairs: Vec<(String, String)>,
    nixpkgs_rev: Option<String>,
) -> Result<PackageSet> {
    let Some(listing) = crate::sets::listing(app) else {
        bail!("the nixpkgs index is not built yet");
    };
    let now = Utc::now();
    let report = Report {
        schema: SCHEMA,
        client: format!("maintainer-mode/{}", env!("CARGO_PKG_VERSION")),
        generated_at: now.to_rfc3339(),
        source: Source {
            name: String::new(),
            system: String::new(),
            nixpkgs_rev,
            scanned_at: Some(now.to_rfc3339()),
        },
        packages: pairs
            .into_iter()
            .map(|(pname, version)| Found {
                pname,
                version,
                attr: None,
            })
            .collect(),
    }
    .strict(&listing.list);
    let (attrs, stale) = report.accepted(&listing.list).map_err(|e| anyhow!(e))?;
    Ok(PackageSet {
        attrs: attrs.into_iter().collect(),
        dropped: stale + report.omitted.unknown,
        uploaded_at: now,
        scanned_at: Some(now.to_rfc3339()),
        nixpkgs_rev: report.source.nixpkgs_rev,
    })
}

async fn scan_in(app: &Arc<App>, cfg: &Config, r: &FlakeRef, work: &Path) -> Result<PackageSet> {
    check_reference(&r.reference).map_err(|e| anyhow!(e))?;
    check_output(&r.output).map_err(|e| anyhow!(e))?;
    let deadline = Instant::now() + cfg.timeout;
    let host = Host::read(&cfg.nix);
    let pre = sandboxed(
        cfg,
        &host,
        work,
        true,
        &["flake", "prefetch", "--json", &r.reference],
        deadline,
    )
    .await
    .context("fetch the flake")?;
    let pre: Value = serde_json::from_slice(&pre).context("read nix flake prefetch")?;
    let root = pre["storePath"]
        .as_str()
        .map(PathBuf::from)
        .filter(|p| {
            p.starts_with("/nix/store/")
                && p.components().count() == 4
                && !p.to_string_lossy().contains("..")
        })
        .context("nix flake prefetch gave no store path")?;
    let lock = {
        use std::io::Read;
        let mut text = String::new();
        std::fs::File::open(root.join("flake.lock"))
            .context("the flake has no flake.lock; commit one")?
            .take(MAX_LOCK)
            .read_to_string(&mut text)?;
        text
    };
    check_lock(&lock).map_err(|e| anyhow!(e))?;
    let pinned = pinned(&r.reference, &pre)?;
    sandboxed(
        cfg,
        &host,
        work,
        true,
        &[
            "flake",
            "archive",
            "--json",
            "--no-update-lock-file",
            &pinned,
        ],
        deadline,
    )
    .await
    .context("fetch the inputs of the flake")?;
    let installable = format!("{pinned}#{}", r.output);
    let out = sandboxed(
        cfg,
        &host,
        work,
        false,
        &[
            "derivation",
            "show",
            "-r",
            "--offline",
            "--no-update-lock-file",
            "--option",
            "substitute",
            "false",
            &installable,
        ],
        deadline,
    )
    .await
    .context("evaluate the output")?;
    let pairs =
        tokio::task::spawn_blocking(move || crate::scan::closure_pairs_at_most(&out, MAX_PACKAGES))
            .await??;
    let rev = crate::scan::nixpkgs_rev_named(&root, None)
        .filter(|r| r.len() <= 64 && r.chars().all(|c| c.is_ascii_hexdigit()));
    let app = Arc::clone(app);
    tokio::task::spawn_blocking(move || accept(&app, pairs, rev)).await?
}

pub async fn scan(app: &Arc<App>, cfg: &Config, r: &FlakeRef) -> Result<PackageSet> {
    let work = crate::eval::private_dir("rescan")?;
    let result = scan_in(app, cfg, r, &work).await;
    let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(work)).await;
    result
}

fn short(e: &anyhow::Error) -> String {
    let text = format!("{e:#}");
    match text.char_indices().nth(MAX_ERROR) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text,
    }
}

/// A flake whose owner has not visited the My machines tab of `/me` for the 90 days of an
/// upload is not scanned any more.
pub fn seen_since() -> chrono::DateTime<Utc> {
    Utc::now() - chrono::Duration::days(crate::sets::RETENTION_DAYS)
}

/// One scan at a time, in order: never scanned first, then the oldest.
/// A reference is due once per nixpkgs index; a person over the daily
/// limit waits for the next day.
async fn pass(app: &Arc<App>, cfg: &Config) -> Result<()> {
    if app.index.read().unwrap().rev.is_empty() {
        return Ok(());
    }
    let Some(index_at) = app.store.get_time("index_at")? else {
        return Ok(());
    };
    for r in app.store.flake_refs_due(index_at, seen_since())? {
        let day = Utc::now() - chrono::Duration::days(1);
        if app.store.rescan_runs_total_since(day)? >= cfg.per_day_total {
            break;
        }
        if app.store.rescan_runs_since(&r.login, day)? >= cfg.per_day {
            continue;
        }
        app.store.log_rescan_run(&r.login)?;
        match scan(app, cfg, &r).await {
            Ok(set) => {
                app.store.finish_flake_ref(r.id, Ok(&set))?;
                tracing::info!(
                    "flake {} of {}: {} attributes",
                    r.id,
                    r.login,
                    set.attrs.len()
                );
            }
            Err(e) => {
                app.store.finish_flake_ref(r.id, Err(&short(&e)))?;
                tracing::info!("flake {} of {}: scan failed", r.id, r.login);
            }
        }
    }
    Ok(())
}

/// Its own task, so a slow scan never holds back the sync.
pub async fn run(app: Arc<App>) {
    let Some(cfg) = app.rescan.clone() else {
        return;
    };
    tracing::info!(
        "flake rescans on: {} flakes a person, {} scans a day, {} in all, {} s, {} MiB",
        cfg.max_flakes,
        cfg.per_day,
        cfg.per_day_total,
        cfg.timeout.as_secs(),
        cfg.memory_mb
    );
    loop {
        if let Err(e) = pass(&app, &cfg).await {
            tracing::warn!("flake rescans: {e:#}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

#[derive(Deserialize)]
struct AddForm {
    reference: String,
    output: String,
}

async fn add(State(app): State<Shared>, headers: HeaderMap, Form(f): Form<AddForm>) -> Response {
    let Some(cfg) = &app.rescan else {
        return (
            StatusCode::FORBIDDEN,
            "this server does not scan flakes; upload a report instead",
        )
            .into_response();
    };
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let (reference, output) = (f.reference.trim(), f.output.trim());
    if let Err(e) = check_reference(reference).and_then(|()| check_output(output)) {
        return (StatusCode::BAD_REQUEST, e).into_response();
    }
    match app
        .store
        .add_flake_ref(&login, reference, output, cfg.max_flakes)
    {
        Ok(true) => Redirect::to("/packages?filter=machines#flakes").into_response(),
        Ok(false) => (
            StatusCode::FORBIDDEN,
            format!("at most {} flakes a person", cfg.max_flakes),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct RemoveForm {
    id: i64,
}

async fn remove(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<RemoveForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    if let Err(e) = app.store.delete_flake_ref(&login, f.id) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    Redirect::to("/packages?filter=machines#flakes").into_response()
}

fn state(r: &FlakeRef, index_at: Option<chrono::DateTime<Utc>>) -> String {
    let again = r.last_at.is_none_or(|l| index_at.is_some_and(|i| l < i));
    let mut s = match (&r.error, r.last_at, r.scanned_at) {
        (Some(e), Some(at), _) => format!(
            "<span class=\"color-fg-danger\">failed {} ago</span> \
             <details class=\"d-inline\"><summary class=\"f6\">why</summary><pre class=\"p-2 color-bg-subtle f6 pre-wrap\">{}</pre></details>",
            ago(at),
            esc(e)
        ),
        (_, _, Some(at)) => format!(
            "{} attributes, {} entries dropped, scanned {} ago",
            r.attrs.len(),
            r.dropped,
            ago(at)
        ),
        _ => "not scanned yet".into(),
    };
    if again {
        s.push_str(" · <span class=\"Label\">queued</span>");
    }
    if r.error.is_some() && r.scanned_at.is_some() {
        let _ = write!(
            s,
            " · keeps the {} attributes of the last good scan",
            r.attrs.len()
        );
    }
    s
}

/// The flakes box of the machines filter of /packages.
pub fn panel(app: &App, login: &str) -> String {
    let refs = app.store.flake_refs(login).unwrap_or_default();
    let index_at = app.store.get_time("index_at").ok().flatten();
    let mut out = format!(
        "<div class=\"Box mb-4\" id=\"flakes\"><div class=\"Box-header\"><h2 class=\"Box-title\">{} Public flakes that the server scans</h2></div>",
        icon(octicon!("shield"))
    );
    for r in &refs {
        let _ = write!(
            out,
            "<div class=\"Box-row d-flex flex-items-center flex-wrap gap-2\">\
             <div class=\"flex-auto\"><code>{}#{}</code><div class=\"f6 color-fg-muted\">{}</div></div>\
             <form method=\"post\" action=\"/me/sets/flakes/delete\"><input type=\"hidden\" name=\"id\" value=\"{}\">\
             <button class=\"btn btn-sm\" type=\"submit\">Remove</button></form></div>",
            esc(&r.reference),
            esc(&r.output),
            state(r, index_at),
            r.id
        );
    }
    match &app.rescan {
        None => out.push_str(
            "<div class=\"Box-row color-fg-muted\">This server does not scan flakes: the operator has not turned \
             server rescans on. Scan on your machine and upload the report, as above.</div>",
        ),
        Some(cfg) if refs.len() >= cfg.max_flakes => {
            let _ = write!(
                out,
                "<div class=\"Box-row color-fg-muted\">You have the most flakes a person can have here: {}.</div>",
                cfg.max_flakes
            );
        }
        Some(cfg) => {
            let _ = write!(
                out,
                "<div class=\"Box-row\"><form method=\"post\" action=\"/me/sets/flakes\" class=\"d-flex flex-wrap gap-2\">\
                 <input class=\"form-control flex-auto\" name=\"reference\" required maxlength=\"{MAX_REFERENCE}\" \
                 placeholder=\"github:OWNER/REPO\" aria-label=\"flake reference\">\
                 <input class=\"form-control flex-auto\" name=\"output\" required maxlength=\"{MAX_OUTPUT_NAME}\" \
                 placeholder=\"nixosConfigurations.HOST.config.system.build.toplevel\" aria-label=\"flake output\">\
                 <button class=\"btn btn-sm\" type=\"submit\">Add</button></form>\
                 <p class=\"f6 color-fg-muted mt-2 mb-0\">A public <code>github:OWNER/REPO</code>, <code>github:OWNER/REPO/BRANCH</code> \
                 or <code>https://</code> tarball, with a <code>flake.lock</code>. After each nixpkgs index, the server evaluates the \
                 output in a sandbox, with no build, and keeps only the nixpkgs attributes, as for an upload. \
                 At most {} flakes and {} scans a day for you. \
                 See <a href=\"https://github.com/numtide/maintainermode/blob/main/docs/rescan.md\">docs/rescan.md</a>.</p></div>",
                cfg.max_flakes, cfg.per_day
            );
        }
    }
    out.push_str("</div>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_strict_github_and_https_references_pass() {
        for ok in [
            "github:numtide/maintainermode",
            "github:NixOS/nixpkgs/nixos-26.05",
            "https://codeberg.org/someone/tree/archive/main.tar.gz",
        ] {
            assert_eq!(check_reference(ok), Ok(()), "{ok}");
        }
        for bad in [
            "",
            "github:o/r?dir=sub",
            "github:o/r/a/b",
            "github:../r",
            "github:o_x/r",
            "github:o/r#x",
            "path:/var/lib/maintainer-mode",
            "/etc",
            "git+ssh://git@github.com/o/r",
            "git+https://github.com/o/r",
            "http://example.org/x.tar.gz",
            "https://127.0.0.1/x.tar.gz",
            "https://localhost/x",
            "https://metadata.internal/x",
            "https://user@codeberg.org/x",
            "https://codeberg.org:8443/x",
            "https://codeberg.org/a/../b",
            "https://codeberg.org/x?narHash=y",
            "https://[::1]/x",
        ] {
            assert!(check_reference(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_output_is_a_plain_attribute_path() {
        assert!(check_output("nixosConfigurations.web-1.config.system.build.toplevel").is_ok());
        assert!(check_output("packages.x86_64-linux.default").is_ok());
        for bad in ["", "a..b", "a b", "a#b", "${x}", "-a", "a.\"b\""] {
            assert!(check_output(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_lock_names_only_inputs_the_sandbox_may_fetch() {
        let lock = |input: Value| {
            serde_json::json!({
                "nodes": { "root": { "inputs": { "x": "x" } }, "x": { "locked": input } },
                "root": "root", "version": 7
            })
            .to_string()
        };
        let rev = "0d6d739a833ba9a9c93034862e4a7b061a062a1c";
        for ok in [
            serde_json::json!({ "type": "github", "owner": "NixOS", "repo": "nixpkgs", "rev": rev }),
            serde_json::json!({ "type": "sourcehut", "owner": "~sir", "repo": "x", "rev": rev }),
            serde_json::json!({ "type": "tarball", "url": "https://channels.nixos.org/nixos-26.05/nixexprs.tar.xz" }),
            serde_json::json!({ "type": "git", "url": "https://codeberg.org/o/r?ref=main&rev=abc" }),
            serde_json::json!({ "type": "git", "url": "https://codeberg.org/o/r", "submodules": false }),
            serde_json::json!({ "type": "path", "path": "./sub" }),
        ] {
            assert_eq!(check_lock(&lock(ok.clone())), Ok(()), "{ok}");
        }
        for bad in [
            serde_json::json!({ "type": "github", "owner": "o", "repo": "r", "rev": rev, "host": "git.corp.internal" }),
            serde_json::json!({ "type": "github", "owner": "o", "repo": "r" }),
            serde_json::json!({ "type": "git", "url": "ssh://git@github.com/o/r" }),
            serde_json::json!({ "type": "git", "url": "file:///var/lib/maintainer-mode" }),
            serde_json::json!({ "type": "tarball", "url": "http://10.0.0.1/x.tar.gz" }),
            serde_json::json!({ "type": "tarball", "url": "https://169.254.169.254/latest" }),
            serde_json::json!({ "type": "path", "path": "/var/lib/maintainer-mode" }),
            serde_json::json!({ "type": "path", "path": "../up" }),
            serde_json::json!({ "type": "indirect", "id": "nixpkgs" }),
            serde_json::json!({ "type": "mercurial", "url": "https://hg.example.org/x" }),
            serde_json::json!({ "type": "git", "url": "https://codeberg.org/o/r", "submodules": true }),
            serde_json::json!({ "type": "git", "url": "https://codeberg.org/o/r", "lfs": true }),
            serde_json::json!({ "type": "git", "url": "https://codeberg.org/o/r?rev=abc&submodules=1" }),
        ] {
            assert!(check_lock(&lock(bad.clone())).is_err(), "{bad}");
        }
        assert!(check_lock("{}").is_err());
    }

    #[test]
    fn the_pin_holds_the_reference_to_the_fetched_tree() {
        let pre = serde_json::json!({
            "hash": "sha256-C3alQ2MEt0zVrUSho5NUDo3jmhbff72++bfwoqT9Omo=",
            "locked": { "owner": "numtide", "repo": "treefmt-nix", "rev": "0d6d739a833ba9a9c93034862e4a7b061a062a1c", "type": "github" },
            "storePath": "/nix/store/mk5plyv9rrsck1mz8yjra5zphcjai2lj-source"
        });
        assert_eq!(
            pinned("github:numtide/treefmt-nix", &pre).unwrap(),
            "github:numtide/treefmt-nix/0d6d739a833ba9a9c93034862e4a7b061a062a1c?narHash=sha256-C3alQ2MEt0zVrUSho5NUDo3jmhbff72%2B%2BbfwoqT9Omo%3D"
        );
        assert_eq!(
            pinned("https://codeberg.org/o/r/archive/main.tar.gz", &pre).unwrap(),
            "https://codeberg.org/o/r/archive/main.tar.gz?narHash=sha256-C3alQ2MEt0zVrUSho5NUDo3jmhbff72%2B%2BbfwoqT9Omo%3D"
        );
        assert!(pinned("github:o/r", &serde_json::json!({ "hash": "md5-x" })).is_err());
    }

    fn cfg() -> Config {
        Config {
            bwrap: "/bin/bwrap".into(),
            prlimit: "/bin/prlimit".into(),
            nix: "/nix/store/x-nix/bin/nix".into(),
            max_flakes: 3,
            per_day: 4,
            per_day_total: 100,
            timeout: std::time::Duration::from_secs(900),
            memory_mb: 4096,
        }
    }

    #[test]
    fn the_evaluation_has_no_network_and_no_state() {
        let host = Host {
            path: "/nix/store/x-nix/bin".into(),
            ca: Some("/etc/static/ssl/ca.crt".into()),
            resolv: Some("/run/systemd/resolve/stub-resolv.conf".into()),
        };
        let work = Path::new("/tmp/maintainer-mode-rescan-1");
        let eval = argv(&cfg(), &host, work, false, &["derivation", "show"]);
        let fetch = argv(&cfg(), &host, work, true, &["flake", "archive"]);
        assert_eq!(eval[0], "/bin/prlimit");
        assert!(eval.contains(&"--as=4294967296".to_string()));
        assert!(eval.contains(&"--unshare-all".to_string()));
        assert!(!eval.contains(&"--share-net".to_string()));
        assert!(!eval.iter().any(|a| a.contains("resolv")));
        assert!(fetch.contains(&"--share-net".to_string()));
        for a in [&eval, &fetch] {
            assert!(
                !a.iter()
                    .any(|x| x.contains("/var/lib") || x.contains("credentials"))
            );
            let at = a
                .iter()
                .position(|x| x == "allow-import-from-derivation")
                .unwrap();
            assert_eq!(a[at + 1], "false");
            let at = a.iter().position(|x| x == "accept-flake-config").unwrap();
            assert_eq!(a[at + 1], "false");
            let store = a.iter().position(|x| x == "/nix/store").unwrap();
            assert_eq!(a[store - 1], "--ro-bind");
            let bind = a.iter().position(|x| x == "--bind").unwrap();
            assert_eq!(a[bind + 1], work.to_string_lossy());
        }
        let vars: Vec<&str> = env(&host).iter().map(|(k, _)| *k).collect();
        assert!(!vars.contains(&"GITHUB_TOKEN") && !vars.contains(&"CREDENTIALS_DIRECTORY"));
    }

    #[tokio::test]
    async fn adding_a_flake_is_refused_while_rescans_are_off() {
        let app = crate::sets::tests::app(&["hello"]);
        let form = Form(AddForm {
            reference: "github:o/r".into(),
            output: "packages.x86_64-linux.default".into(),
        });
        let r = add(State(app.clone()), HeaderMap::new(), form).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        assert!(
            app.store
                .flake_refs_due(Utc::now(), seen_since())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn flakes_are_limited_and_due_once_per_index() {
        let app = crate::sets::tests::app(&["hello"]);
        let s = &app.store;
        assert!(s.add_flake_ref("alice", "github:o/a", "x", 2).unwrap());
        assert!(s.add_flake_ref("alice", "github:o/b", "x", 2).unwrap());
        assert!(!s.add_flake_ref("alice", "github:o/c", "x", 2).unwrap());
        assert!(s.add_flake_ref("bob", "github:o/c", "x", 2).unwrap());
        let before = Utc::now() - chrono::Duration::hours(1);
        let due = s.flake_refs_due(before, seen_since()).unwrap();
        assert_eq!(due.len(), 3);
        let set = PackageSet {
            attrs: vec!["hello".into()],
            dropped: 4,
            uploaded_at: Utc::now(),
            scanned_at: None,
            nixpkgs_rev: None,
        };
        s.finish_flake_ref(due[0].id, Ok(&set)).unwrap();
        s.finish_flake_ref(due[1].id, Err("no flake.lock")).unwrap();
        assert_eq!(s.flake_refs_due(before, seen_since()).unwrap().len(), 1);
        let later = Utc::now() + chrono::Duration::hours(1);
        assert_eq!(s.flake_refs_due(later, seen_since()).unwrap().len(), 3);
        assert_eq!(s.flake_ref_counts().unwrap().1, 1);
        assert_eq!(crate::sets::set_attrs(&app, &due[0].login), vec!["hello"]);
        s.log_rescan_run("alice").unwrap();
        s.log_rescan_run("alice").unwrap();
        assert_eq!(s.rescan_runs_since("alice", before).unwrap(), 2);
        assert_eq!(s.rescan_runs_since("bob", before).unwrap(), 0);
        s.log_rescan_run("bob").unwrap();
        assert_eq!(s.rescan_runs_total_since(before).unwrap(), 3);
        assert_eq!(s.purge_flake_results(later).unwrap(), 1);
        assert!(crate::sets::set_attrs(&app, &due[0].login).is_empty());
        s.delete_flake_refs("alice").unwrap();
        assert_eq!(s.flake_ref_counts().unwrap().0, 1);
    }

    #[test]
    fn an_owner_who_stays_away_stops_the_scans_and_loses_the_attributes() {
        let app = crate::sets::tests::app(&["hello"]);
        let s = &app.store;
        assert!(s.add_flake_ref("alice", "github:o/a", "x", 2).unwrap());
        let id = s.flake_refs("alice").unwrap()[0].id;
        let set = PackageSet {
            attrs: vec!["hello".into()],
            dropped: 0,
            uploaded_at: Utc::now() + chrono::Duration::hours(2),
            scanned_at: None,
            nixpkgs_rev: None,
        };
        s.finish_flake_ref(id, Ok(&set)).unwrap();
        let after_visit = Utc::now() + chrono::Duration::hours(1);
        let index_at = after_visit + chrono::Duration::days(1);
        assert_eq!(s.flake_refs_due(index_at, seen_since()).unwrap().len(), 1);
        assert!(s.flake_refs_due(index_at, after_visit).unwrap().is_empty());
        assert_eq!(s.purge_flake_results(after_visit).unwrap(), 1);
        assert!(crate::sets::set_attrs(&app, "alice").is_empty());
        assert_eq!(s.purge_flake_results(after_visit).unwrap(), 0);
        s.touch_flake_refs("alice").unwrap();
        let seen = s.flake_refs_due(index_at, Utc::now() - chrono::Duration::minutes(1));
        assert_eq!(seen.unwrap().len(), 1);
    }

    #[test]
    fn a_scanned_closure_keeps_only_attributes_the_index_knows() {
        let app = crate::sets::tests::app(&["hello", "python3Packages.requests"]);
        let pairs = [
            ("hello", "2.12"),
            ("requests", "2.32"),
            ("our-private-daemon", "1.0"),
        ]
        .iter()
        .map(|(p, v)| (p.to_string(), v.to_string()))
        .collect();
        let set = accept(&app, pairs, None).unwrap();
        assert_eq!(set.attrs, vec!["hello", "python3Packages.requests"]);
        assert_eq!(set.dropped, 1);
        let empty = crate::sets::tests::app(&[]);
        assert!(accept(&empty, vec![], None).is_err());
    }

    #[test]
    fn a_long_error_is_cut() {
        let e = anyhow!("{}", "x".repeat(MAX_ERROR * 2));
        assert_eq!(short(&e).chars().count(), MAX_ERROR + 1);
        assert_eq!(tail(b"a\n\nb\nc\n", 2), "b\nc");
    }
}
