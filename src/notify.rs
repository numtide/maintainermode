//! Notifications: the inbox at `/me/inbox`, its unread count in the
//! header, and an optional daily email digest. The events are those of the
//! private Atom feed. Only the owner sees their inbox, their choices and
//! their address.

use anyhow::{Context, bail};
use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Duration, Utc};
use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::{Mailbox, Message};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::extension::ClientId;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::RwLock;

use crate::catalog::{Event, head, person_events};
use crate::github::urlencode;
use crate::sets::token_hash;
use crate::store::Confirmation;
use crate::web::{App, Shared, ago, esc, icon, octicon, page, random_id};

const READ_AT: &str = "inbox_read_at";
const KINDS: &str = "notify_kinds";
const EMAIL: &str = "digest_email";
const DIGEST_AT: &str = "digest_at";
const TEST_AT: &str = "test_mail_at";
/// The address the person confirmed; the digest goes only to an address
/// equal to it.
const CONFIRMED: &str = "digest_confirmed";
const CONFIRM_HOURS: i64 = 24;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/me/inbox", get(inbox_page))
        .route("/me/inbox/read", post(mark_read))
        .route("/me/notify", post(save_notify))
        .route("/me/notify/test", post(test_mail))
        .route("/me/notify/confirm", post(resend_confirmation))
        .route("/confirm/:token", get(confirm_page).post(confirm))
        .route(
            "/unsubscribe/:token",
            get(unsubscribe_page).post(unsubscribe),
        )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    PullRequest,
    Issue,
    Advisory,
    Release,
}

impl Kind {
    pub const ALL: [Kind; 4] = [
        Kind::PullRequest,
        Kind::Issue,
        Kind::Advisory,
        Kind::Release,
    ];

    /// From the id that `catalog::events` and `catalog::set_events` give.
    pub fn of(id: &str) -> Kind {
        match id.split_once('-').map_or(id, |(k, _)| k) {
            "issue" => Kind::Issue,
            "advisory" => Kind::Advisory,
            "upstream" => Kind::Release,
            _ => Kind::PullRequest,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Kind::PullRequest => "pr",
            Kind::Issue => "issue",
            Kind::Advisory => "advisory",
            Kind::Release => "release",
        }
    }

    fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.key() == s)
    }

    fn label(self) -> &'static str {
        match self {
            Kind::PullRequest => "Pull requests",
            Kind::Issue => "Issues",
            Kind::Advisory => "Advisories",
            Kind::Release => "Releases",
        }
    }

    fn icon(self) -> String {
        match self {
            Kind::PullRequest => icon(octicon!("git-pull-request")),
            Kind::Issue => icon(octicon!("issue-opened")),
            Kind::Advisory => icon(octicon!("shield")),
            Kind::Release => icon(octicon!("tag")),
        }
    }
}

/// The kinds a person counts in the header and gets by email. All of them
/// until the person chooses.
pub fn kinds(app: &App, login: &str) -> Vec<Kind> {
    match app.store.setting(login, KINDS).ok().flatten() {
        None => Kind::ALL.to_vec(),
        Some(v) => v.split(',').filter_map(Kind::parse).collect(),
    }
}

pub(crate) fn read_at(app: &App, login: &str) -> Option<DateTime<Utc>> {
    app.store
        .setting(login, READ_AT)
        .ok()
        .flatten()
        .and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn unread(e: &Event, read_at: Option<DateTime<Utc>>) -> bool {
    read_at.is_none_or(|r| e.at > r)
}

pub fn unread_count(events: &[Event], read_at: Option<DateTime<Utc>>, kinds: &[Kind]) -> usize {
    events
        .iter()
        .filter(|e| unread(e, read_at) && kinds.contains(&Kind::of(&e.id)))
        .count()
}

/// Unread counts per login, for the header. The background pass fills it
/// after each recompute; a page view only reads it.
#[derive(Default)]
pub struct Counts(RwLock<HashMap<String, usize>>);

impl Counts {
    pub fn get(&self, login: &str) -> usize {
        self.0.read().unwrap().get(login).copied().unwrap_or(0)
    }

    fn set(&self, login: &str, n: usize) {
        self.0.write().unwrap().insert(login.to_string(), n);
    }
}

pub fn recount_one(app: &App, login: &str) {
    let n = unread_count(
        &person_events(app, login),
        read_at(app, login),
        &kinds(app, login),
    );
    app.inbox.set(login, n);
}

/// Everyone who signed in once; a login with nothing watched costs a few
/// small reads.
pub fn recount(app: &App) {
    for m in app.store.members().unwrap_or_default() {
        recount_one(app, &m.login);
    }
}

#[derive(Deserialize)]
struct InboxQuery {
    sub: Option<String>,
    kind: Option<String>,
}

fn inbox_href(sub: Option<&str>, kind: Option<Kind>) -> String {
    let mut q = Vec::new();
    if let Some(s) = sub {
        q.push(format!("sub={}", urlencode(s)));
    }
    if let Some(k) = kind {
        q.push(format!("kind={}", k.key()));
    }
    if q.is_empty() {
        "/me/inbox".into()
    } else {
        format!("/me/inbox?{}", q.join("&"))
    }
}

async fn inbox_page(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<InboxQuery>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let all = person_events(&app, &login);
    let read = read_at(&app, &login);
    let n = unread_count(&all, read, &kinds(&app, &login));
    app.inbox.set(&login, n);
    let sub = q.sub.as_deref().filter(|s| !s.is_empty());
    let kind = q.kind.as_deref().and_then(Kind::parse);
    let any_unread = all.iter().any(|e| unread(e, read));
    let mut out = head(
        &format!("{} Inbox", icon(octicon!("inbox"))),
        "What happened in the last 30 days to what you watch and to your package set, newest first. \
         These are the events of your Atom feed. Only you see this page.",
        if any_unread {
            "<form method=\"post\" action=\"/me/inbox/read\"><button class=\"btn btn-sm\" type=\"submit\">Mark all read</button></form>"
        } else {
            ""
        },
    );
    out.push_str("<div class=\"d-flex flex-wrap flex-items-center mb-3\" style=\"gap:8px\">");
    let sel = |on: bool| if on { " selected" } else { "" };
    let _ = write!(
        out,
        "<a class=\"SegmentedControl-item btn btn-sm{}\" href=\"{}\">All</a>",
        sel(kind.is_none()),
        esc(&inbox_href(sub, None))
    );
    for k in Kind::ALL {
        let _ = write!(
            out,
            "<a class=\"SegmentedControl-item btn btn-sm{}\" href=\"{}\">{} {}</a>",
            sel(kind == Some(k)),
            esc(&inbox_href(sub, Some(k))),
            k.icon(),
            k.label()
        );
    }
    let subjects: BTreeSet<&str> = all.iter().map(|e| e.subject.as_str()).collect();
    if !subjects.is_empty() {
        let _ = write!(
            out,
            "<form method=\"get\" action=\"/me/inbox\" class=\"d-flex\" style=\"gap:8px\">{}\
             <select class=\"form-select\" name=\"sub\" aria-label=\"subscription\"><option value=\"\">Every subscription</option>",
            kind.map(|k| format!(
                "<input type=\"hidden\" name=\"kind\" value=\"{}\">",
                k.key()
            ))
            .unwrap_or_default()
        );
        for s in &subjects {
            let _ = write!(
                out,
                "<option value=\"{v}\"{}>{v}</option>",
                if sub == Some(*s) { " selected" } else { "" },
                v = esc(s)
            );
        }
        out.push_str(
            "</select><button class=\"btn btn-sm\" type=\"submit\">Filter</button></form>",
        );
    }
    out.push_str("</div>");
    let shown: Vec<&Event> = all
        .iter()
        .filter(|e| sub.is_none_or(|s| e.subject == s))
        .filter(|e| kind.is_none_or(|k| Kind::of(&e.id) == k))
        .collect();
    if shown.is_empty() {
        out.push_str(
            "<div class=\"blankslate\"><p>Nothing here. Subscribe to packages on <a href=\"/watch\">/watch</a>, \
             or send your package set on <a href=\"/me/sets\">/me/sets</a>.</p></div>",
        );
    } else {
        out.push_str("<div class=\"Box\">");
        for e in shown {
            let new = unread(e, read);
            let _ = write!(
                out,
                "<div class=\"Box-row d-flex flex-items-start\"><span class=\"mr-2 color-fg-muted\">{}</span>\
                 <div class=\"flex-auto\">{}\
                 <div class=\"f6 color-fg-muted mt-1\">{} · {} ago</div></div>{}</div>",
                Kind::of(&e.id).icon(),
                crate::prpage::link(
                    &e.url,
                    &esc(&e.title),
                    if new {
                        "Link--primary text-bold"
                    } else {
                        "Link--primary"
                    }
                ),
                esc(&e.subject),
                ago(e.at),
                if new {
                    "<span class=\"Label Label--accent ml-2\">new</span>"
                } else {
                    ""
                }
            );
        }
        out.push_str("</div>");
    }
    page(&app, Some(&login), "", &out).into_response()
}

async fn mark_read(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    if let Err(e) = app
        .store
        .set_setting(&login, READ_AT, &Utc::now().to_rfc3339())
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    app.inbox.set(&login, 0);
    Redirect::to("/me/inbox").into_response()
}

fn mail_note(mail: Option<&str>) -> &'static str {
    match mail {
        Some("sent") => "The test email is on its way.",
        Some("failed") => "The server could not send the email. Try again later.",
        Some("wait") => "Wait 10 minutes between two test emails.",
        Some("invalid") => "Use an address like name@example.org, with no spaces and no name.",
        Some("none") => "Save an address first.",
        Some("confirm") => {
            "We sent a confirmation email. Open its link in the next 24 hours to start the digest."
        }
        Some("unconfirmed") => {
            "Confirm your address first: open the link in the confirmation email."
        }
        Some("limit") => {
            "The server reached its daily email limit, or you got 3 confirmation emails today. Try again tomorrow."
        }
        _ => "",
    }
}

fn stored_address(app: &App, login: &str) -> Option<String> {
    app.store
        .setting(login, EMAIL)
        .ok()
        .flatten()
        .filter(|a| !a.is_empty())
}

/// The digest address, only once its owner confirmed it.
fn confirmed_address(app: &App, login: &str) -> Option<String> {
    let address = stored_address(app, login)?;
    let confirmed = app.store.setting(login, CONFIRMED).ok().flatten()?;
    (confirmed == address).then_some(address)
}

/// The section of `/me/settings`. The address shows only here, to its owner.
pub fn settings_box(app: &App, login: &str, mail: Option<&str>) -> String {
    let on = kinds(app, login);
    let mut boxes = String::new();
    for k in Kind::ALL {
        let _ = write!(
            boxes,
            "<div class=\"form-checkbox my-1\"><label><input type=\"checkbox\" name=\"k_{key}\" value=\"1\"{}> {} {}</label></div>",
            if on.contains(&k) { " checked" } else { "" },
            k.icon(),
            k.label(),
            key = k.key()
        );
    }
    let address = stored_address(app, login).unwrap_or_default();
    let confirmed = confirmed_address(app, login).is_some();
    let state = match (address.is_empty(), confirmed) {
        (true, _) => "",
        (false, true) => " <span class=\"Label Label--success ml-1\">confirmed</span>",
        (false, false) => {
            " <span class=\"Label Label--attention ml-1\">pending confirmation</span>"
        }
    };
    let email = if app.smtp.is_some() {
        format!(
            "<label class=\"d-block mt-3\" for=\"email\">Daily email digest to</label>{state}\
             <input class=\"form-control width-full my-1\" type=\"email\" id=\"email\" name=\"email\" maxlength=\"254\" value=\"{}\" placeholder=\"name@example.org\">\
             <p class=\"note\">At most one email a day, with the unread events of these kinds since the last digest. \
             A new address gets a confirmation email first; the digest starts when you open its link. \
             Leave it empty to get no email. Only you see this address. Each email has a link to unsubscribe.</p>",
            esc(&address)
        )
    } else {
        "<p class=\"note mt-3\">Email is off on this server: its operator did not set up SMTP.</p>"
            .into()
    };
    let test = if app.smtp.is_none() || address.is_empty() {
        String::new()
    } else if !confirmed {
        format!(
            "<form method=\"post\" action=\"/me/notify/confirm\" class=\"mt-3\"><button class=\"btn btn-sm\" type=\"submit\">{} Send the confirmation email again</button></form>",
            icon(octicon!("mail"))
        )
    } else {
        let last = app
            .store
            .setting(login, DIGEST_AT)
            .ok()
            .flatten()
            .and_then(|t| DateTime::parse_from_rfc3339(&t).ok())
            .map(|t| {
                format!(
                    " The last digest went out {} ago.",
                    ago(t.with_timezone(&Utc))
                )
            })
            .unwrap_or_default();
        format!(
            "<form method=\"post\" action=\"/me/notify/test\" class=\"mt-3\"><button class=\"btn btn-sm\" type=\"submit\">{} Send me a test email</button>\
             <span class=\"f6 color-fg-muted ml-2\">{last}</span></form>",
            icon(octicon!("mail"))
        )
    };
    let note = match mail_note(mail) {
        "" => String::new(),
        n => format!("<div class=\"flash mb-3\">{n}</div>"),
    };
    format!(
        "<div class=\"Box mb-4\" id=\"notify\"><div class=\"Box-header text-bold\">{} Notifications</div><div class=\"Box-row\">{note}\
         <form method=\"post\" action=\"/me/notify\">\
         <p>Your <a href=\"/me/inbox\">inbox</a> shows every event. The unread count in the header, and the email digest, include only the kinds you choose here.</p>\
         {boxes}{email}<button class=\"btn btn-sm mt-2\" type=\"submit\">Save</button></form>{test}</div></div>",
        icon(octicon!("bell")),
    )
}

#[derive(Deserialize)]
struct NotifyForm {
    #[serde(default)]
    k_pr: Option<String>,
    #[serde(default)]
    k_issue: Option<String>,
    #[serde(default)]
    k_advisory: Option<String>,
    #[serde(default)]
    k_release: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

impl NotifyForm {
    fn chosen(&self) -> Vec<Kind> {
        [
            (Kind::PullRequest, &self.k_pr),
            (Kind::Issue, &self.k_issue),
            (Kind::Advisory, &self.k_advisory),
            (Kind::Release, &self.k_release),
        ]
        .into_iter()
        .filter(|(_, v)| v.as_deref() == Some("1"))
        .map(|(k, _)| k)
        .collect()
    }
}

async fn save_notify(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(f): Form<NotifyForm>,
) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let address = f.email.as_deref().map(str::trim);
    if app.smtp.is_some() && address.is_some_and(|a| !a.is_empty() && !valid_address(a)) {
        return Redirect::to("/me/settings?mail=invalid#notify").into_response();
    }
    let keys: Vec<&str> = f.chosen().into_iter().map(Kind::key).collect();
    if let Err(e) = app.store.set_setting(&login, KINDS, &keys.join(",")) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    recount_one(&app, &login);
    let mut note = "";
    let changed = |a: &str| stored_address(&app, &login).as_deref().unwrap_or("") != a;
    if let (Some(a), Some(smtp)) = (address.filter(|a| changed(a)), &app.smtp) {
        let saved =
            forget_address(&app, &login).and_then(|_| app.store.set_setting(&login, EMAIL, a));
        if let Err(e) = saved {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
        if !a.is_empty() {
            note = request_confirmation(&app, smtp, &login, a).await;
        }
    }
    settings_redirect(note)
}

fn settings_redirect(note: &str) -> Response {
    if note.is_empty() {
        Redirect::to("/me/settings#notify").into_response()
    } else {
        Redirect::to(&format!("/me/settings?mail={note}#notify")).into_response()
    }
}

/// Removes the address with everything that points to it.
fn forget_address(app: &App, login: &str) -> anyhow::Result<()> {
    app.store.set_setting(login, EMAIL, "")?;
    app.store.set_setting(login, CONFIRMED, "")?;
    app.store.delete_unsubscribes(login)?;
    app.store.delete_confirmations(login)
}

fn new_confirmation(
    app: &App,
    login: &str,
    address: &str,
    at: DateTime<Utc>,
) -> anyhow::Result<String> {
    let token = random_id();
    app.store
        .add_confirmation(login, address, &token_hash(&token), at)?;
    Ok(token)
}

/// Sends one confirmation email and gives the note for the settings page.
async fn request_confirmation(app: &App, smtp: &Smtp, login: &str, address: &str) -> &'static str {
    if !take_slot(app, smtp, login, Mail::Confirm) {
        return "limit";
    }
    let token = match new_confirmation(app, login, address, Utc::now()) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("confirmation of {login}: {e:#}");
            return "failed";
        }
    };
    let base = app.public_url.trim_end_matches('/');
    let body = format!(
        "Someone asked Maintainer Mode to send a daily digest to this address, for the GitHub account {login}.\n\n\
         To confirm, open this link in the next {CONFIRM_HOURS} hours and push the button:\n\
         {base}/confirm/{token}\n\n\
         If you did not ask for this, ignore this email. \
         Without a confirmation, Maintainer Mode sends no more email to this address.\n"
    );
    match deliver(
        smtp,
        address,
        "Maintainer Mode: confirm your address",
        body,
        None,
    )
    .await
    {
        Ok(()) => "confirm",
        Err(e) => {
            tracing::warn!("confirmation of {login}: {e:#}");
            "failed"
        }
    }
}

async fn resend_confirmation(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let Some(smtp) = &app.smtp else {
        return settings_redirect("off");
    };
    let Some(address) = stored_address(&app, &login) else {
        return settings_redirect("none");
    };
    if confirmed_address(&app, &login).is_some() {
        return settings_redirect("");
    }
    settings_redirect(request_confirmation(&app, smtp, &login, &address).await)
}

/// The confirmation of a token: known, younger than `CONFIRM_HOURS`, and
/// for the address the person holds now.
fn pending_confirmation(app: &App, token: &str, now: DateTime<Utc>) -> Option<Confirmation> {
    if !valid_token(token) {
        return None;
    }
    let c = app.store.confirmation(&token_hash(token)).ok().flatten()?;
    let fresh = now - c.created_at < Duration::hours(CONFIRM_HOURS);
    (fresh && stored_address(app, &c.login).as_deref() == Some(c.address.as_str())).then_some(c)
}

const UNKNOWN_CONFIRM: &str = "<div class=\"blankslate\"><p>This confirmation link is unknown, used or older than 24 hours. \
     Sign in and send a new one from your settings page.</p></div>";

/// A link scanner opens every link of an email, so a GET only asks.
async fn confirm_page(State(app): State<Shared>, Path(token): Path<String>) -> Response {
    if pending_confirmation(&app, &token, Utc::now()).is_none() {
        return unsubscribe_reply(&app, StatusCode::NOT_FOUND, UNKNOWN_CONFIRM);
    }
    let body = format!(
        "{}<form method=\"post\" action=\"/confirm/{}\"><button class=\"btn btn-primary\" type=\"submit\">Confirm</button></form>",
        head(
            "Confirm your address",
            "Start the daily email digest of Maintainer Mode at this address.",
            ""
        ),
        esc(&token)
    );
    unsubscribe_reply(&app, StatusCode::OK, &body)
}

async fn confirm(State(app): State<Shared>, Path(token): Path<String>) -> Response {
    let Some(c) = pending_confirmation(&app, &token, Utc::now()) else {
        return unsubscribe_reply(&app, StatusCode::NOT_FOUND, UNKNOWN_CONFIRM);
    };
    let done = app
        .store
        .set_setting(&c.login, CONFIRMED, &c.address)
        .and_then(|_| app.store.delete_confirmations(&c.login));
    if let Err(e) = done {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    unsubscribe_reply(
        &app,
        StatusCode::OK,
        &head(
            "Address confirmed",
            "The daily digest goes to this address from now on. Each email has a link to unsubscribe.",
            "",
        ),
    )
}

async fn test_mail(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(login) = app.viewer(&headers) else {
        return Redirect::to("/auth/login").into_response();
    };
    let back = |m: &str| Redirect::to(&format!("/me/settings?mail={m}#notify")).into_response();
    let Some(smtp) = &app.smtp else {
        return back("off");
    };
    if stored_address(&app, &login).is_none() {
        return back("none");
    }
    let Some(address) = confirmed_address(&app, &login) else {
        return back("unconfirmed");
    };
    let last = app
        .store
        .setting(&login, TEST_AT)
        .ok()
        .flatten()
        .and_then(|t| DateTime::parse_from_rfc3339(&t).ok());
    if last.is_some_and(|t| Utc::now() - t.with_timezone(&Utc) < Duration::minutes(10)) {
        return back("wait");
    }
    if let Err(e) = app
        .store
        .set_setting(&login, TEST_AT, &Utc::now().to_rfc3339())
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    if !take_slot(&app, smtp, &login, Mail::Test) {
        return back("limit");
    }
    let body = format!(
        "This is a test email from Maintainer Mode, sent because {login} asked for it on the settings page.\n\
         The daily digest comes from the same address.\n"
    );
    match send(
        &app,
        smtp,
        &login,
        &address,
        "Maintainer Mode: test email",
        body,
    )
    .await
    {
        Ok(()) => back("sent"),
        Err(e) => {
            tracing::warn!("test email of {login}: {e:#}");
            back("failed")
        }
    }
}

/// A plain address, `local@domain.tld`, with no display name. It refuses
/// spaces, quotes, angle brackets and control characters, so no header can
/// be injected through it.
pub fn valid_address(s: &str) -> bool {
    if s.len() > 254 {
        return false;
    }
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    let atext = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+-/=?^_`{|}~".contains(c);
    let local_ok = local.len() <= 64
        && local
            .split('.')
            .all(|p| !p.is_empty() && p.chars().all(atext));
    let labels: Vec<&str> = domain.split('.').collect();
    let domain_ok = labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    local_ok && domain_ok
}

/// The SMTP server for the digest. Email stays off unless the host and the
/// sender are both set.
#[derive(clap::Args)]
pub struct SmtpArgs {
    #[arg(long = "smtp-host", env = "MAINTAINER_MODE_SMTP_HOST")]
    host: Option<String>,
    /// 465 is TLS from the start; any other port must offer STARTTLS.
    #[arg(
        long = "smtp-port",
        env = "MAINTAINER_MODE_SMTP_PORT",
        default_value_t = 465
    )]
    port: u16,
    #[arg(long = "smtp-user", env = "MAINTAINER_MODE_SMTP_USER")]
    user: Option<String>,
    #[arg(
        long = "smtp-password-file",
        env = "MAINTAINER_MODE_SMTP_PASSWORD_FILE"
    )]
    password_file: Option<PathBuf>,
    /// The sender, like `Maintainer Mode <noreply@example.org>`.
    #[arg(long = "smtp-from", env = "MAINTAINER_MODE_SMTP_FROM")]
    from: Option<String>,
    /// Emails in 24 hours for the whole server, of every kind.
    #[arg(
        long = "mail-per-day",
        env = "MAINTAINER_MODE_MAIL_PER_DAY",
        default_value_t = 500
    )]
    mail_per_day: usize,
}

pub struct Smtp {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    pub per_day: usize,
}

const CONFIRMS_PER_DAY: usize = 3;

#[derive(Clone, Copy)]
enum Mail {
    Confirm,
    Test,
    Digest,
}

impl Mail {
    fn key(self) -> &'static str {
        match self {
            Mail::Confirm => "confirm",
            Mail::Test => "test",
            Mail::Digest => "digest",
        }
    }
}

/// False when the email would go over the server limit, or over the
/// confirmations a person gets in a day.
fn take_slot(app: &App, smtp: &Smtp, login: &str, kind: Mail) -> bool {
    let own_max = match kind {
        Mail::Confirm => CONFIRMS_PER_DAY,
        Mail::Test | Mail::Digest => usize::MAX,
    };
    match app
        .store
        .take_mail_slot(login, kind.key(), smtp.per_day, own_max, Utc::now())
    {
        Ok(true) => true,
        Ok(false) => {
            tracing::warn!("{} email skipped: over a daily email limit", kind.key());
            false
        }
        Err(e) => {
            tracing::warn!("email limit: {e:#}");
            false
        }
    }
}

impl SmtpArgs {
    pub fn config(&self, public_url: &str) -> anyhow::Result<Option<Smtp>> {
        let (host, from) = match (&self.host, &self.from) {
            (Some(h), Some(f)) => (h, f),
            (None, None) => return Ok(None),
            _ => bail!("set both MAINTAINER_MODE_SMTP_HOST and _FROM, or neither"),
        };
        let from: Mailbox = from
            .parse()
            .with_context(|| "MAINTAINER_MODE_SMTP_FROM is not an address")?;
        let mut b = if self.port == 465 {
            AsyncSmtpTransport::<Tokio1Executor>::relay(host)?
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)?
        }
        .port(self.port)
        .timeout(Some(std::time::Duration::from_secs(30)));
        match (&self.user, crate::read_secret(&self.password_file)?) {
            (Some(u), Some(p)) => b = b.credentials(Credentials::new(u.clone(), p)),
            (None, None) => {}
            _ => bail!("set both MAINTAINER_MODE_SMTP_USER and _PASSWORD_FILE, or neither"),
        }
        if let Some(name) = hello_name(public_url) {
            b = b.hello_name(ClientId::Domain(name));
        }
        tracing::info!("email digest through {host}:{}", self.port);
        Ok(Some(Smtp {
            transport: b.build(),
            per_day: self.mail_per_day,
            from,
        }))
    }
}

fn hello_name(public_url: &str) -> Option<String> {
    let rest = public_url.split_once("://").map_or(public_url, |(_, r)| r);
    let host = rest.split(['/', ':']).next()?;
    (host.contains('.') && host.chars().any(|c| c.is_ascii_alphabetic())).then(|| host.to_string())
}

const UNSUBSCRIBE_DAYS: i64 = 60;

fn valid_token(t: &str) -> bool {
    t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Every email gets a new token, so an old link still works after a newer
/// email, for `UNSUBSCRIBE_DAYS`.
async fn send(
    app: &App,
    smtp: &Smtp,
    login: &str,
    address: &str,
    subject: &str,
    body: String,
) -> anyhow::Result<()> {
    let token = random_id();
    app.store.add_unsubscribe(login, &token_hash(&token))?;
    let base = app.public_url.trim_end_matches('/');
    let unsubscribe = format!("{base}/unsubscribe/{token}");
    let body = format!(
        "{body}\n--\nYour inbox: {base}/me/inbox\nYour notification settings: {base}/me/settings#notify\n\
         Unsubscribe from these emails: {unsubscribe}\n"
    );
    deliver(smtp, address, subject, body, Some(&unsubscribe)).await
}

async fn deliver(
    smtp: &Smtp,
    address: &str,
    subject: &str,
    body: String,
    unsubscribe: Option<&str>,
) -> anyhow::Result<()> {
    if !valid_address(address) {
        bail!("the stored address is not valid");
    }
    let raw = |name: &'static str, value: String| {
        HeaderValue::new(HeaderName::new_from_ascii_str(name), value)
    };
    let mut message = Message::builder()
        .from(smtp.from.clone())
        .to(address.parse()?)
        .subject(subject)
        .header(ContentType::TEXT_PLAIN);
    if let Some(u) = unsubscribe {
        message = message
            .raw_header(raw("List-Unsubscribe", format!("<{u}>")))
            .raw_header(raw(
                "List-Unsubscribe-Post",
                "List-Unsubscribe=One-Click".into(),
            ));
    }
    let message = message
        .raw_header(raw("Auto-Submitted", "auto-generated".into()))
        .body(body)?;
    smtp.transport.send(message).await?;
    Ok(())
}

fn digest_at(app: &App, login: &str) -> Option<DateTime<Utc>> {
    app.store
        .setting(login, DIGEST_AT)
        .ok()
        .flatten()
        .and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// At most one digest per person and UTC day.
pub fn digest_due(last: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    last.is_none_or(|t| t.date_naive() < now.date_naive())
}

/// The unread events of the chosen kinds that no digest carried yet.
pub fn digest_selection<'a>(
    events: &'a [Event],
    read_at: Option<DateTime<Utc>>,
    last_digest: Option<DateTime<Utc>>,
    kinds: &[Kind],
) -> Vec<&'a Event> {
    events
        .iter()
        .filter(|e| unread(e, read_at) && last_digest.is_none_or(|t| e.at > t))
        .filter(|e| kinds.contains(&Kind::of(&e.id)))
        .collect()
}

const DIGEST_LINES: usize = 50;

fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn digest_body(list: &[&Event]) -> String {
    let mut out = format!(
        "{} new events on what you watch and on your package set:\n\n",
        list.len()
    );
    for e in list.iter().take(DIGEST_LINES) {
        let _ = writeln!(
            out,
            "- {}: {}\n  {}",
            one_line(&e.subject),
            one_line(&e.title),
            one_line(&e.url)
        );
    }
    if list.len() > DIGEST_LINES {
        let _ = writeln!(
            out,
            "\nand {} more in your inbox.",
            list.len() - DIGEST_LINES
        );
    }
    out
}

async fn digest_pass(app: &App, smtp: &Smtp) -> anyhow::Result<usize> {
    app.store
        .purge_unsubscribes(Utc::now() - Duration::days(UNSUBSCRIBE_DAYS))?;
    app.store
        .purge_confirmations(Utc::now() - Duration::hours(CONFIRM_HOURS))?;
    let mut sent = 0;
    for (login, address) in app.store.settings_with(EMAIL)? {
        if confirmed_address(app, &login).as_deref() != Some(address.as_str()) {
            continue;
        }
        let now = Utc::now();
        let last = digest_at(app, &login);
        if !digest_due(last, now) {
            continue;
        }
        let events = person_events(app, &login);
        let list = digest_selection(&events, read_at(app, &login), last, &kinds(app, &login));
        if list.is_empty() {
            continue;
        }
        if !take_slot(app, smtp, &login, Mail::Digest) {
            break;
        }
        let subject = format!("Maintainer Mode: {} new", list.len());
        match send(app, smtp, &login, &address, &subject, digest_body(&list)).await {
            Ok(()) => {
                app.store
                    .set_setting(&login, DIGEST_AT, &now.to_rfc3339())?;
                sent += 1;
            }
            Err(e) => tracing::warn!("digest of {login}: {e:#}"),
        }
    }
    Ok(sent)
}

/// The digest loop. It does nothing while email is off.
pub async fn run(app: Shared) {
    let Some(smtp) = &app.smtp else {
        return;
    };
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(15 * 60)).await;
        match digest_pass(&app, smtp).await {
            Ok(0) => {}
            Ok(n) => tracing::info!("{n} email digests sent"),
            Err(e) => tracing::warn!("email digests: {e:#}"),
        }
    }
}

fn unsubscribe_reply(app: &App, code: StatusCode, body: &str) -> Response {
    let Html(page) = page(app, None, "", body);
    (
        code,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        Html(page),
    )
        .into_response()
}

fn unsubscribe_login(app: &App, token: &str) -> Option<String> {
    if !valid_token(token) {
        return None;
    }
    app.store
        .unsubscribe_login(&token_hash(token))
        .ok()
        .flatten()
}

const UNKNOWN_LINK: &str = "<div class=\"blankslate\"><p>This unsubscribe link is unknown or too old. Sign in and empty the address on your settings page.</p></div>";

/// A link scanner opens every link of an email, so a GET only asks. The
/// one-click POST of RFC 8058 and the button below do the work.
async fn unsubscribe_page(State(app): State<Shared>, Path(token): Path<String>) -> Response {
    if unsubscribe_login(&app, &token).is_none() {
        return unsubscribe_reply(&app, StatusCode::NOT_FOUND, UNKNOWN_LINK);
    }
    let body = format!(
        "{}<form method=\"post\" action=\"/unsubscribe/{}\"><button class=\"btn btn-danger\" type=\"submit\">Unsubscribe</button></form>",
        head(
            "Unsubscribe",
            "Stop the email digest of Maintainer Mode. Your inbox on the site stays.",
            ""
        ),
        esc(&token)
    );
    unsubscribe_reply(&app, StatusCode::OK, &body)
}

async fn unsubscribe(State(app): State<Shared>, Path(token): Path<String>) -> Response {
    let Some(login) = unsubscribe_login(&app, &token) else {
        return unsubscribe_reply(&app, StatusCode::NOT_FOUND, UNKNOWN_LINK);
    };
    if let Err(e) = forget_address(&app, &login) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    unsubscribe_reply(
        &app,
        StatusCode::OK,
        &head(
            "Unsubscribed",
            "Maintainer Mode sends you no more email. The server deleted your address.",
            "",
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn event(id: &str, at: DateTime<Utc>) -> Event {
        Event {
            at,
            id: id.into(),
            title: String::new(),
            url: String::new(),
            subject: "hello".into(),
        }
    }

    #[test]
    fn the_kind_comes_from_the_event_id() {
        assert_eq!(Kind::of("pr-1-merged"), Kind::PullRequest);
        assert_eq!(Kind::of("issue-2-issue opened"), Kind::Issue);
        assert_eq!(Kind::of("advisory-NIXPKGS-1-hello"), Kind::Advisory);
        assert_eq!(Kind::of("upstream-hello-2.13"), Kind::Release);
        for k in Kind::ALL {
            assert_eq!(Kind::parse(k.key()), Some(k));
        }
    }

    #[test]
    fn the_unread_count_starts_after_the_read_time_and_keeps_the_chosen_kinds() {
        let now = Utc::now();
        let list = [
            event("pr-1-merged", now - Duration::hours(1)),
            event("issue-2-issue opened", now - Duration::hours(2)),
            event("upstream-hello-2", now - Duration::hours(3)),
            event("pr-3-pull request opened", now - Duration::days(2)),
        ];
        assert_eq!(unread_count(&list, None, &Kind::ALL), 4);
        let read = Some(now - Duration::days(1));
        assert_eq!(unread_count(&list, read, &Kind::ALL), 3);
        assert_eq!(unread_count(&list, read, &[Kind::PullRequest]), 1);
        assert_eq!(unread_count(&list, read, &[]), 0);
        assert_eq!(unread_count(&list, Some(now), &Kind::ALL), 0);
    }

    pub(crate) fn signed_in(app: &Shared, login: &str) -> HeaderMap {
        app.store.create_session("s", login, "t").unwrap();
        let mut h = HeaderMap::new();
        h.insert(axum::http::header::COOKIE, "session=s".parse().unwrap());
        h
    }

    #[tokio::test]
    async fn the_inbox_is_for_its_owner_and_marks_all_read() {
        let app = crate::sets::tests::app(&[]);
        let r = inbox_page(
            State(app.clone()),
            HeaderMap::new(),
            Query(InboxQuery {
                sub: None,
                kind: None,
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        let h = signed_in(&app, "alice");
        let r = inbox_page(
            State(app.clone()),
            h.clone(),
            Query(InboxQuery {
                sub: None,
                kind: Some("advisory".into()),
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        app.inbox.set("alice", 3);
        assert_eq!(app.inbox.get("alice"), 3);
        let r = mark_read(State(app.clone()), h).await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        assert_eq!(app.inbox.get("alice"), 0);
        assert!(read_at(&app, "alice").is_some());
    }

    #[test]
    fn an_address_is_plain_and_carries_no_header() {
        for ok in [
            "alice@example.org",
            "a.b+nix@mail.example.co.uk",
            "x_y-z@a-b.example",
        ] {
            assert!(valid_address(ok), "{ok}");
        }
        for bad in [
            "",
            "alice",
            "alice@",
            "@example.org",
            "alice@localhost",
            "alice@@example.org",
            "a@b@example.org",
            "alice @example.org",
            "alice@example.org\r\nBcc: victim@example.org",
            "alice@example.org\n",
            "Alice <alice@example.org>",
            "\"alice\"@example.org",
            "alice,bob@example.org",
            ".alice@example.org",
            "al..ice@example.org",
            "alice@-example.org",
            "alice@example..org",
            "alice@exa_mple.org",
        ] {
            assert!(!valid_address(bad), "{bad:?}");
        }
        assert!(!valid_address(&format!("{}@example.org", "a".repeat(65))));
        assert!(!valid_address(&format!(
            "a@{}.org",
            vec!["b".repeat(60); 5].join(".")
        )));
    }

    #[test]
    fn a_digest_carries_the_unread_events_since_the_last_digest() {
        let now = Utc::now();
        let list = [
            event("pr-1-merged", now - Duration::hours(1)),
            event("advisory-A-1-hello", now - Duration::hours(5)),
            event("issue-2-issue opened", now - Duration::hours(30)),
            event("upstream-hello-2", now - Duration::days(3)),
        ];
        let ids = |v: Vec<&Event>| v.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
        assert_eq!(digest_selection(&list, None, None, &Kind::ALL).len(), 4);
        assert_eq!(
            ids(digest_selection(
                &list,
                None,
                Some(now - Duration::days(1)),
                &Kind::ALL
            )),
            ["pr-1-merged", "advisory-A-1-hello"]
        );
        assert_eq!(
            ids(digest_selection(
                &list,
                Some(now - Duration::hours(2)),
                Some(now - Duration::days(1)),
                &Kind::ALL
            )),
            ["pr-1-merged"]
        );
        assert_eq!(
            ids(digest_selection(
                &list,
                None,
                None,
                &[Kind::Advisory, Kind::Release]
            )),
            ["advisory-A-1-hello", "upstream-hello-2"]
        );
        assert!(digest_selection(&list, Some(now), None, &Kind::ALL).is_empty());
    }

    #[test]
    fn a_digest_goes_out_once_a_day_at_most() {
        let now = DateTime::parse_from_rfc3339("2026-10-17T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(digest_due(None, now));
        assert!(!digest_due(Some(now - Duration::hours(9)), now));
        assert!(digest_due(Some(now - Duration::hours(11)), now));
    }

    #[test]
    fn a_digest_body_is_one_line_per_event_and_capped() {
        let now = Utc::now();
        let mut e = event("pr-1-merged", now);
        e.title = "merged: #1 a\nBcc: x".into();
        let many: Vec<Event> = (0..DIGEST_LINES + 3)
            .map(|n| event(&format!("pr-{n}-merged"), now))
            .collect();
        assert!(digest_body(&[&e]).contains("- hello: merged: #1 a Bcc: x\n"));
        let refs: Vec<&Event> = many.iter().collect();
        assert!(digest_body(&refs).contains("and 3 more in your inbox."));
    }

    #[test]
    fn the_hello_name_is_the_public_host() {
        assert_eq!(
            hello_name("https://maintainermode.org/").as_deref(),
            Some("maintainermode.org")
        );
        assert_eq!(hello_name("http://127.0.0.1:8790"), None);
        assert_eq!(hello_name("http://localhost:8790"), None);
    }

    #[test]
    fn smtp_is_off_without_a_host_and_refuses_half_a_setup() {
        let args = |host: Option<&str>, from: Option<&str>, user: Option<&str>| SmtpArgs {
            host: host.map(Into::into),
            port: 465,
            user: user.map(Into::into),
            password_file: None,
            from: from.map(Into::into),
            mail_per_day: 500,
        };
        assert!(args(None, None, None).config("").unwrap().is_none());
        assert!(
            args(Some("smtp.example.org"), None, None)
                .config("")
                .is_err()
        );
        assert!(
            args(Some("smtp.example.org"), Some("a@example.org"), Some("u"))
                .config("")
                .is_err()
        );
        assert!(
            args(Some("smtp.example.org"), Some("a@example.org"), None)
                .config("https://mm.example")
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn an_unsubscribe_link_works_once_and_a_bad_one_is_not_found() {
        let app = crate::sets::tests::app(&[]);
        app.store
            .set_setting("alice", EMAIL, "alice@example.org")
            .unwrap();
        let token = random_id();
        app.store
            .add_unsubscribe("alice", &token_hash(&token))
            .unwrap();
        let get = |t: &str| unsubscribe_page(State(app.clone()), Path(t.to_string()));
        assert_eq!(get("nope").await.status(), StatusCode::NOT_FOUND);
        assert_eq!(get(&random_id()).await.status(), StatusCode::NOT_FOUND);
        assert_eq!(get(&token).await.status(), StatusCode::OK);
        assert_eq!(
            app.store.setting("alice", EMAIL).unwrap().as_deref(),
            Some("alice@example.org")
        );
        let r = unsubscribe(State(app.clone()), Path(token.clone())).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            app.store.setting("alice", EMAIL).unwrap().as_deref(),
            Some("")
        );
        assert!(app.store.settings_with(EMAIL).unwrap().is_empty());
        let r = unsubscribe(State(app.clone()), Path(token)).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn without_smtp_the_address_is_not_kept() {
        let app = crate::sets::tests::app(&[]);
        let h = signed_in(&app, "alice");
        let form = NotifyForm {
            k_pr: None,
            k_issue: Some("1".into()),
            k_advisory: None,
            k_release: None,
            email: Some("alice@example.org".into()),
        };
        let r = save_notify(State(app.clone()), h.clone(), Form(form)).await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        assert_eq!(kinds(&app, "alice"), [Kind::Issue]);
        assert!(app.store.setting("alice", EMAIL).unwrap().is_none());
        let html = settings_box(&app, "alice", None);
        assert!(html.contains("Email is off on this server"));
        assert!(!html.contains("alice@example.org"));
        let r = test_mail(State(app.clone()), h).await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
    }

    #[tokio::test]
    async fn with_smtp_only_a_valid_address_is_kept() {
        let mut app = crate::sets::tests::app(&[]);
        std::sync::Arc::get_mut(&mut app).unwrap().smtp = SmtpArgs {
            host: Some("smtp.example.org".into()),
            port: 587,
            user: None,
            password_file: None,
            from: Some("Maintainer Mode <mm@example.org>".into()),
            mail_per_day: 0,
        }
        .config("https://mm.example")
        .unwrap();
        let h = signed_in(&app, "alice");
        let save = |email: &str| {
            let form = NotifyForm {
                k_pr: Some("1".into()),
                k_issue: None,
                k_advisory: None,
                k_release: None,
                email: Some(email.into()),
            };
            save_notify(State(app.clone()), h.clone(), Form(form))
        };
        let r = save("alice@example.org\r\nBcc: x@example.org").await;
        assert_eq!(
            r.headers()[header::LOCATION],
            "/me/settings?mail=invalid#notify"
        );
        assert!(app.store.setting("alice", EMAIL).unwrap().is_none());
        save(" alice@example.org ").await;
        assert_eq!(
            app.store.setting("alice", EMAIL).unwrap().as_deref(),
            Some("alice@example.org")
        );
        let html = settings_box(&app, "alice", Some("invalid"));
        assert!(html.contains("value=\"alice@example.org\""));
        assert!(html.contains("pending confirmation"));
        assert!(html.contains("Send the confirmation email again"));
        assert!(!html.contains("Send me a test email"));
        assert!(html.contains("with no spaces"));
        assert!(!settings_box(&app, "bob", None).contains("alice@example.org"));
        let r = test_mail(State(app.clone()), h.clone()).await;
        assert_eq!(
            r.headers()[header::LOCATION],
            "/me/settings?mail=unconfirmed#notify"
        );
        let token = new_confirmation(&app, "alice", "alice@example.org", Utc::now()).unwrap();
        confirm(State(app.clone()), Path(token)).await;
        let html = settings_box(&app, "alice", None);
        assert!(html.contains("confirmed"));
        assert!(html.contains("Send me a test email"));
        save("").await;
        assert!(app.store.settings_with(EMAIL).unwrap().is_empty());
        assert!(confirmed_address(&app, "alice").is_none());
    }

    #[tokio::test]
    async fn a_confirmation_link_works_once_for_a_day_and_only_for_the_current_address() {
        let app = crate::sets::tests::app(&[]);
        app.store
            .set_setting("alice", EMAIL, "alice@example.org")
            .unwrap();
        let now = Utc::now();
        let token = new_confirmation(&app, "alice", "alice@example.org", now).unwrap();
        let stored = app
            .store
            .confirmation(&token_hash(&token))
            .unwrap()
            .unwrap();
        assert_eq!(stored.address, "alice@example.org");
        assert!(app.store.confirmation(&token).unwrap().is_none());
        let get = |t: &str| confirm_page(State(app.clone()), Path(t.to_string()));
        let post = |t: &str| confirm(State(app.clone()), Path(t.to_string()));
        assert_eq!(get("nope").await.status(), StatusCode::NOT_FOUND);
        assert_eq!(get(&random_id()).await.status(), StatusCode::NOT_FOUND);
        assert_eq!(post(&random_id()).await.status(), StatusCode::NOT_FOUND);
        assert_eq!(get(&token).await.status(), StatusCode::OK);
        assert!(confirmed_address(&app, "alice").is_none());
        assert!(pending_confirmation(&app, &token, now + Duration::hours(23)).is_some());
        assert!(pending_confirmation(&app, &token, now + Duration::hours(25)).is_none());
        let old = new_confirmation(
            &app,
            "alice",
            "alice@example.org",
            now - Duration::hours(25),
        )
        .unwrap();
        assert_eq!(get(&old).await.status(), StatusCode::NOT_FOUND);
        assert_eq!(post(&old).await.status(), StatusCode::NOT_FOUND);
        let other = new_confirmation(&app, "alice", "old@example.org", now).unwrap();
        assert_eq!(post(&other).await.status(), StatusCode::NOT_FOUND);
        assert_eq!(post(&token).await.status(), StatusCode::OK);
        assert_eq!(
            confirmed_address(&app, "alice").as_deref(),
            Some("alice@example.org")
        );
        assert_eq!(post(&token).await.status(), StatusCode::NOT_FOUND);
        app.store
            .set_setting("alice", EMAIL, "new@example.org")
            .unwrap();
        assert!(confirmed_address(&app, "alice").is_none());
        app.store.purge_confirmations(now).unwrap();
        assert!(app.store.confirmation(&token_hash(&old)).unwrap().is_none());
    }

    #[test]
    fn the_email_limits_count_a_rolling_day_and_skip_without_a_login() {
        let s = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        let now = Utc::now();
        let take = |login: &str, kind: Mail, at: DateTime<Utc>| {
            let own = if matches!(kind, Mail::Confirm) {
                CONFIRMS_PER_DAY
            } else {
                usize::MAX
            };
            s.take_mail_slot(login, kind.key(), 5, own, at).unwrap()
        };
        let old = now - Duration::hours(25);
        for _ in 0..CONFIRMS_PER_DAY {
            assert!(take("alice", Mail::Confirm, old));
        }
        for _ in 0..CONFIRMS_PER_DAY {
            assert!(take("alice", Mail::Confirm, now));
        }
        assert!(!take("alice", Mail::Confirm, now));
        assert!(take("bob", Mail::Confirm, now));
        assert!(take("alice", Mail::Test, now));
        assert!(!take("carol", Mail::Digest, now));
        assert!(!take("carol", Mail::Confirm, now));
        assert_eq!(s.mail_counts(now - Duration::days(1)).unwrap(), (5, 3));
        for _ in 0..100 {
            assert!(!take("alice", Mail::Confirm, now));
        }
        assert_eq!(s.mail_counts(now - Duration::days(1)).unwrap(), (5, 103));
        assert!(take("alice", Mail::Confirm, now + Duration::hours(25)));
    }

    #[tokio::test]
    async fn over_the_server_limit_no_confirmation_goes_out() {
        let mut app = crate::sets::tests::app(&[]);
        std::sync::Arc::get_mut(&mut app).unwrap().smtp = SmtpArgs {
            host: Some("smtp.example.org".into()),
            port: 587,
            user: None,
            password_file: None,
            from: Some("mm@example.org".into()),
            mail_per_day: 0,
        }
        .config("https://mm.example")
        .unwrap();
        let h = signed_in(&app, "alice");
        let form = NotifyForm {
            k_pr: None,
            k_issue: None,
            k_advisory: None,
            k_release: None,
            email: Some("alice@example.org".into()),
        };
        let r = save_notify(State(app.clone()), h.clone(), Form(form)).await;
        assert_eq!(
            r.headers()[header::LOCATION],
            "/me/settings?mail=limit#notify"
        );
        let r = resend_confirmation(State(app.clone()), h).await;
        assert_eq!(
            r.headers()[header::LOCATION],
            "/me/settings?mail=limit#notify"
        );
        assert_eq!(
            app.store
                .mail_counts(Utc::now() - Duration::days(1))
                .unwrap(),
            (0, 2)
        );
        let sync = crate::status::state(&app);
        assert_eq!(sync["email"]["skipped_24h"], 2);
        assert!(!sync.to_string().contains("alice"));
    }

    #[test]
    fn a_filter_link_keeps_the_other_filter() {
        assert_eq!(inbox_href(None, None), "/me/inbox");
        assert_eq!(
            inbox_href(Some("pkgs/by-name/"), Some(Kind::Advisory)),
            "/me/inbox?sub=pkgs%2Fby-name%2F&kind=advisory"
        );
    }
}
