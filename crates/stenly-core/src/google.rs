//! Native Google Calendar: sign in with Google (OAuth 2.0 installed-app flow,
//! PKCE, loopback redirect), keep the tokens in the Keychain, list the user's
//! calendars and fetch their events. No Calendar.app, Kody or ICS link needed.
//!
//! Structure (so Microsoft Graph can be added later as a sibling module):
//! - [`Credentials`] / [`Endpoints`]: where the OAuth client comes from and
//!   which servers to talk to (a loopback mock in tests).
//! - [`TokenStore`]: where signed-in accounts live (Keychain, or memory).
//! - [`Client`]: sign-in, refresh, calendar list and event fetch.
//! - [`map_event`]: Google event JSON → [`CalendarEvent`], shared with Kody.
//!
//! The only scope requested is `calendar.readonly`. It is the narrowest scope
//! that can both list calendars and read their events with attendees and
//! conference data (`calendar.events.readonly` cannot list calendars).

use crate::calendar::{Attendee, CalendarEvent, CalendarInfo};
use crate::config::CalendarConfig;
use crate::oauth::{self, form, urlenc};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

pub const SCOPE: &str = "https://www.googleapis.com/auth/calendar.readonly";

// ------------------------------------------------------------------ errors

#[derive(Debug, thiserror::Error)]
pub enum GoogleError {
    #[error("Google sign-in isn't set up in this build of Stenly.")]
    NotConfigured,
    #[error("Sign-in expired for {0}. Sign in with Google again.")]
    Reauth(String),
    #[error("Can't reach Google. Check your internet connection.")]
    Unreachable,
    #[error("Sign-in was cancelled.")]
    Denied,
    #[error(
        "Stenly needs permission to see your calendars. Sign in again and leave the calendar permission ticked."
    )]
    MissingScope,
    #[error("Google returned an error ({status}): {message}")]
    Api { status: u16, message: String },
}

/// The message says a stored sign-in no longer works (revoked, expired, or an
/// app still in Google's "testing" mode after 7 days).
pub fn is_reauth(msg: &str) -> bool {
    msg.contains("Sign-in expired for")
}

/// The message says Google couldn't be reached at all (as opposed to one
/// calendar being unreadable).
pub fn is_unreachable(msg: &str) -> bool {
    msg.contains("Can't reach Google")
}

// ------------------------------------------------------------- credentials

/// The OAuth client. A Desktop-app client's "secret" is not confidential
/// (Google documents this), but it stays out of the repository: it is baked in
/// at build time from `STENLY_GOOGLE_CLIENT_ID` / `STENLY_GOOGLE_CLIENT_SECRET`
/// (see `build.rs` and the release workflow). The same variables at runtime
/// override the build-time values, for development.
#[derive(Clone)]
pub struct Credentials {
    pub client_id: String,
    pub client_secret: String,
}

impl Credentials {
    pub fn from_build() -> Option<Self> {
        fn pick(runtime: &str, built: Option<&'static str>) -> String {
            std::env::var(runtime)
                .ok()
                .or_else(|| built.map(str::to_string))
                .unwrap_or_default()
                .trim()
                .to_string()
        }
        let client_id = pick(
            "STENLY_GOOGLE_CLIENT_ID",
            option_env!("STENLY_GOOGLE_CLIENT_ID"),
        );
        (!client_id.is_empty()).then(|| Self {
            client_id,
            client_secret: pick(
                "STENLY_GOOGLE_CLIENT_SECRET",
                option_env!("STENLY_GOOGLE_CLIENT_SECRET"),
            ),
        })
    }
}

/// Whether this build can sign in with Google. The sign-in buttons are hidden
/// when it can't.
pub fn configured() -> bool {
    Credentials::from_build().is_some()
}

// --------------------------------------------------------------- endpoints

#[derive(Debug, Clone)]
pub struct Endpoints {
    pub auth: String,
    pub token: String,
    pub revoke: String,
    /// Calendar API root, without a trailing slash (`…/calendar/v3`).
    pub api: String,
    /// Talking to a local mock server, never to Google.
    pub mock: bool,
}

impl Endpoints {
    pub fn google() -> Self {
        Self {
            auth: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            revoke: "https://oauth2.googleapis.com/revoke".into(),
            api: "https://www.googleapis.com/calendar/v3".into(),
            mock: false,
        }
    }

    /// A mock OAuth + Calendar server at `base` (`http://127.0.0.1:<port>`).
    pub fn mock(base: &str) -> Self {
        let base = base.trim_end_matches('/');
        Self {
            auth: format!("{base}/o/oauth2/v2/auth"),
            token: format!("{base}/token"),
            revoke: format!("{base}/revoke"),
            api: format!("{base}/calendar/v3"),
            mock: true,
        }
    }

    /// Google, unless `STENLY_GOOGLE_MOCK` points at a loopback address. Any
    /// other value is ignored so the variable can never redirect real tokens
    /// to another host.
    pub fn from_env() -> Self {
        match std::env::var("STENLY_GOOGLE_MOCK") {
            Ok(base) if is_loopback(&base) => Self::mock(&base),
            Ok(_) => {
                tracing::warn!("ignoring STENLY_GOOGLE_MOCK: only loopback addresses are accepted");
                Self::google()
            }
            Err(_) => Self::google(),
        }
    }
}

fn is_loopback(base: &str) -> bool {
    let Some(rest) = base.strip_prefix("http://") else {
        return false;
    };
    let host = if rest.starts_with('[') {
        rest.split(']').next().map(|h| format!("{h}]"))
    } else {
        rest.split([':', '/']).next().map(str::to_string)
    };
    matches!(host.as_deref(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

// ------------------------------------------------------------------ tokens

/// One signed-in Google account. Never printed: `Debug` hides the tokens.
#[derive(Clone, Serialize, Deserialize)]
pub struct Account {
    pub email: String,
    pub refresh_token: String,
    #[serde(default)]
    pub access_token: String,
    /// Unix seconds.
    #[serde(default)]
    pub expires_at: i64,
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("email", &self.email)
            .finish_non_exhaustive()
    }
}

pub trait TokenStore: Send + Sync {
    fn load(&self) -> Vec<Account>;
    fn save(&self, accounts: &[Account]) -> Result<()>;
}

#[derive(Serialize, Deserialize, Default)]
struct Blob {
    accounts: Vec<Account>,
}

/// The login Keychain on macOS (0600 files elsewhere), one item per profile:
/// an isolated profile (`STENLY_CONFIG`) never sees the everyday app's accounts.
pub struct KeychainStore;

impl TokenStore for KeychainStore {
    fn load(&self) -> Vec<Account> {
        crate::secrets::google_accounts()
            .and_then(|s| serde_json::from_str::<Blob>(&s).ok())
            .map(|b| b.accounts)
            .unwrap_or_default()
    }
    fn save(&self, accounts: &[Account]) -> Result<()> {
        if accounts.is_empty() {
            return crate::secrets::set_google_accounts("");
        }
        crate::secrets::set_google_accounts(&serde_json::to_string(&Blob {
            accounts: accounts.to_vec(),
        })?)
    }
}

/// In-memory store for tests.
#[derive(Default)]
pub struct MemoryStore(pub Mutex<Vec<Account>>);

impl TokenStore for MemoryStore {
    fn load(&self) -> Vec<Account> {
        self.0.lock().unwrap().clone()
    }
    fn save(&self, accounts: &[Account]) -> Result<()> {
        *self.0.lock().unwrap() = accounts.to_vec();
        Ok(())
    }
}

/// Emails of the accounts signed in on this profile.
pub fn signed_in_accounts() -> Vec<String> {
    KeychainStore.load().into_iter().map(|a| a.email).collect()
}

// ------------------------------------------------------------ calendar keys

/// Calendars are identified in config and in [`CalendarInfo::id`] by
/// `account|calendar id`: the same calendar can be visible from two accounts.
pub fn key(account: &str, calendar_id: &str) -> String {
    format!("{account}|{calendar_id}")
}

pub fn split_key(key: &str) -> Option<(&str, &str)> {
    key.split_once('|')
}

/// Keys to watch right after signing in: the account's primary calendar.
pub fn default_selection(calendars: &[CalendarInfo], account: &str) -> Vec<String> {
    calendars
        .iter()
        .filter(|c| {
            c.primary
                && c.account
                    .as_deref()
                    .is_some_and(|a| a.eq_ignore_ascii_case(account))
        })
        .map(|c| c.id.clone())
        .collect()
}

// -------------------------------------------------------------------- HTTP

fn agent() -> &'static ureq::Agent {
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(8)))
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into()
    })
}

fn transport(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Timeout(_)
        | ureq::Error::ConnectionFailed
        | ureq::Error::HostNotFound
        | ureq::Error::Io(_) => GoogleError::Unreachable.into(),
        e => e.into(),
    }
}

/// `{"error": {"message": …}}` (Calendar API) or `{"error": "…", "error_description": …}` (OAuth).
fn api_message(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    v["error"]["message"]
        .as_str()
        .or(v["error_description"].as_str())
        .or(v["error"].as_str())
        .unwrap_or("no details")
        .chars()
        .take(200)
        .collect()
}

#[derive(Deserialize)]
struct TokenResp {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
}

// ------------------------------------------------------------------ client

pub struct Client {
    creds: Option<Credentials>,
    ep: Endpoints,
    store: Arc<dyn TokenStore>,
}

/// Serialises read-modify-write of the stored accounts within this process
/// (the app's calendar loop, a sign-in and a sign-out can overlap). Another
/// process (the CLI, the MCP server) can still race, but it only ever
/// refreshes an access token, which is harmless to repeat.
static STORE_LOCK: Mutex<()> = Mutex::new(());

impl Client {
    pub fn new(creds: Option<Credentials>, ep: Endpoints, store: Arc<dyn TokenStore>) -> Self {
        Self { creds, ep, store }
    }

    /// The real thing: build-time (or dev override) credentials, the Keychain
    /// and Google (or the loopback mock named by `STENLY_GOOGLE_MOCK`).
    pub fn system() -> Self {
        Self::new(
            Credentials::from_build(),
            Endpoints::from_env(),
            Arc::new(KeychainStore),
        )
    }

    fn creds(&self) -> Result<&Credentials> {
        self.creds
            .as_ref()
            .ok_or_else(|| GoogleError::NotConfigured.into())
    }

    /// Load the stored accounts fresh, apply `f`, save. Saving failures are
    /// logged rather than failing a fetch that already has its token.
    fn update<R>(&self, f: impl FnOnce(&mut Vec<Account>) -> R) -> R {
        let _guard = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut accts = self.store.load();
        let r = f(&mut accts);
        if let Err(e) = self.store.save(&accts) {
            tracing::warn!(error = %format!("{e:#}"), "couldn't save Google sign-in");
        }
        r
    }

    pub fn accounts(&self) -> Vec<String> {
        self.store.load().into_iter().map(|a| a.email).collect()
    }

    // ---- sign-in

    /// Interactive sign-in. `open_url` receives the Google consent URL (the
    /// app opens the browser); blocks until the browser is redirected back to
    /// our loopback listener, `cancelled()` turns true, or five minutes pass.
    /// Returns the account's email.
    pub fn sign_in(&self, open_url: &dyn Fn(&str), cancelled: &dyn Fn() -> bool) -> Result<String> {
        let creds = self.creds()?;
        let (listener, redirect_uri) = oauth::loopback()?;
        let oauth::Pkce {
            verifier,
            challenge,
            state,
        } = oauth::pkce();
        let url = format!(
            "{}?{}",
            self.ep.auth,
            form(&[
                ("client_id", &creds.client_id),
                ("redirect_uri", &redirect_uri),
                ("response_type", "code"),
                ("scope", SCOPE),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
                ("state", &state),
                // A refresh token every time, and the account chooser so a
                // second account can be added.
                ("access_type", "offline"),
                ("prompt", "consent select_account"),
            ])
        );
        if self.ep.mock && std::env::var_os("STENLY_GOOGLE_MOCK_AUTO").is_some() {
            // Test runs: play the browser ourselves instead of opening one.
            let url = url.clone();
            std::thread::spawn(move || {
                let _ = agent().get(&url).call();
            });
        } else {
            open_url(&url);
        }
        let code = oauth::wait_for_code_at(
            &listener,
            "/callback",
            &state,
            "Google",
            Duration::from_secs(300),
            cancelled,
        )
        .map_err(|e| {
            if e.to_string().contains("access_denied") {
                GoogleError::Denied.into()
            } else {
                e
            }
        })?;

        let tok = self.token_request(
            &[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", &redirect_uri),
                ("client_id", &creds.client_id),
                ("client_secret", &creds.client_secret),
                ("code_verifier", &verifier),
            ],
            None,
        )?;
        if !tok
            .scope
            .as_deref()
            .unwrap_or(SCOPE)
            .split_whitespace()
            .any(|s| s == SCOPE)
        {
            return Err(GoogleError::MissingScope.into());
        }
        let email = self.primary_email(&tok.access_token)?;
        let expires_at = Utc::now().timestamp() + tok.expires_in.unwrap_or(3600);
        self.update(|accts| {
            let existing = accts.iter().position(|a| a.email.eq_ignore_ascii_case(&email));
            let refresh = tok.refresh_token.clone().or_else(|| existing.map(|i| accts[i].refresh_token.clone()));
            let Some(refresh_token) = refresh else { return Err(anyhow::anyhow!("Google didn't return a refresh token. Remove Stenly at myaccount.google.com/permissions and sign in again.")) };
            let acct = Account { email: email.clone(), refresh_token, access_token: tok.access_token.clone(), expires_at };
            match existing {
                Some(i) => accts[i] = acct,
                None => accts.push(acct),
            }
            Ok(())
        })?;
        Ok(email)
    }

    /// Forget an account and ask Google to revoke its grant (best effort).
    pub fn sign_out(&self, email: &str) {
        let refresh = self.update(|accts| {
            let i = accts
                .iter()
                .position(|a| a.email.eq_ignore_ascii_case(email))?;
            Some(accts.remove(i).refresh_token)
        });
        if let Some(rt) = refresh {
            let _ = agent()
                .post(&self.ep.revoke)
                .content_type("application/x-www-form-urlencoded")
                .send(form(&[("token", &rt)]));
        }
    }

    fn token_request(&self, pairs: &[(&str, &str)], account: Option<&str>) -> Result<TokenResp> {
        // An empty secret (a public-client setup) is simply omitted.
        let pairs: Vec<(&str, &str)> = pairs
            .iter()
            .copied()
            .filter(|(k, v)| !(*k == "client_secret" && v.is_empty()))
            .collect();
        let mut resp = agent()
            .post(&self.ep.token)
            .content_type("application/x-www-form-urlencoded")
            .send(form(&pairs))
            .map_err(transport)?;
        let status = resp.status().as_u16();
        let body = resp.body_mut().read_to_string().map_err(transport)?;
        if status == 200 {
            return serde_json::from_str(&body).context("Google's token reply wasn't understood");
        }
        let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        match (v["error"].as_str(), account) {
            (Some("invalid_grant"), Some(a)) => Err(GoogleError::Reauth(a.to_string()).into()),
            (Some("invalid_grant"), None) => Err(anyhow::anyhow!(
                "Google rejected the sign-in code. Try again."
            )),
            _ => Err(GoogleError::Api {
                status,
                message: api_message(&body),
            }
            .into()),
        }
    }

    /// A current access token for `email`, refreshed when it is about to
    /// expire (or always with `force`) and saved.
    pub fn access_token(&self, email: &str, force: bool) -> Result<String> {
        let (refresh, fresh) = self
            .store
            .load()
            .iter()
            .find(|a| a.email.eq_ignore_ascii_case(email))
            .map(|a| {
                let ok = !force
                    && !a.access_token.is_empty()
                    && Utc::now().timestamp() < a.expires_at - 120;
                (a.refresh_token.clone(), ok.then(|| a.access_token.clone()))
            })
            .ok_or_else(|| GoogleError::Reauth(email.to_string()))?;
        if let Some(t) = fresh {
            return Ok(t);
        }
        let creds = self.creds()?;
        let tok = self.token_request(
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", &refresh),
                ("client_id", &creds.client_id),
                ("client_secret", &creds.client_secret),
            ],
            Some(email),
        )?;
        let expires_at = Utc::now().timestamp() + tok.expires_in.unwrap_or(3600);
        self.update(|accts| {
            if let Some(a) = accts
                .iter_mut()
                .find(|a| a.email.eq_ignore_ascii_case(email))
            {
                a.access_token = tok.access_token.clone();
                a.expires_at = expires_at;
                if let Some(rt) = &tok.refresh_token {
                    a.refresh_token = rt.clone();
                }
            }
        });
        Ok(tok.access_token)
    }

    // ---- Calendar API

    fn get_with(&self, token: &str, path: &str, query: &[(&str, &str)]) -> Result<(u16, String)> {
        let url = format!("{}{path}?{}", self.ep.api, form(query));
        let mut resp = agent()
            .get(&url)
            .header("authorization", &format!("Bearer {token}"))
            .call()
            .map_err(transport)?;
        let status = resp.status().as_u16();
        Ok((status, resp.body_mut().read_to_string().map_err(transport)?))
    }

    /// GET a Calendar API path as `email`, retrying once with a forced refresh
    /// on 401 (a token revoked or rotated since it was cached).
    fn api_get(&self, email: &str, path: &str, query: &[(&str, &str)]) -> Result<Value> {
        let mut token = self.access_token(email, false)?;
        for attempt in 0..2 {
            let (status, body) = self.get_with(&token, path, query)?;
            match status {
                200..=299 => {
                    return serde_json::from_str(&body).context("Google's reply wasn't understood");
                }
                401 if attempt == 0 => token = self.access_token(email, true)?,
                401 => return Err(GoogleError::Reauth(email.to_string()).into()),
                _ => {
                    return Err(GoogleError::Api {
                        status,
                        message: api_message(&body),
                    }
                    .into());
                }
            }
        }
        unreachable!("loop returns on both attempts")
    }

    /// The account's email: the id of its primary calendar.
    fn primary_email(&self, access_token: &str) -> Result<String> {
        let (status, body) = self.get_with(
            access_token,
            "/users/me/calendarList",
            &[("maxResults", "250"), ("fields", "items(id,primary)")],
        )?;
        if status != 200 {
            return Err(GoogleError::Api {
                status,
                message: api_message(&body),
            }
            .into());
        }
        let v: Value = serde_json::from_str(&body)?;
        v["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| c["primary"].as_bool() == Some(true))
            .and_then(|c| c["id"].as_str())
            .map(str::to_string)
            .context("couldn't tell which Google account signed in")
    }

    fn calendar_items(&self, email: &str) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        for _ in 0..10 {
            let mut q = vec![
                ("maxResults", "250"),
                (
                    "fields",
                    "nextPageToken,items(id,summary,summaryOverride,primary,backgroundColor,accessRole)",
                ),
            ];
            if let Some(p) = &page {
                q.push(("pageToken", p));
            }
            let v = self.api_get(email, "/users/me/calendarList", &q)?;
            out.extend(v["items"].as_array().cloned().unwrap_or_default());
            match v["nextPageToken"].as_str() {
                Some(p) => page = Some(p.to_string()),
                None => break,
            }
        }
        // Free/busy-only calendars can't show event details.
        out.retain(|c| c["accessRole"].as_str() != Some("freeBusyReader"));
        Ok(out)
    }

    /// Every calendar of every signed-in account, plus per-account problems.
    pub fn list_calendars(&self) -> (Vec<CalendarInfo>, Vec<String>) {
        let mut out = Vec::new();
        let mut errors = Vec::new();
        for acct in self.accounts() {
            match self.calendar_items(&acct) {
                Ok(items) => out.extend(items.iter().map(|c| info(&acct, c))),
                Err(e) => errors.push(account_error(&acct, &e)),
            }
        }
        (out, errors)
    }

    /// Events in `[from, to)` from the calendars named in `selected` (see
    /// [`key`]). With nothing selected at all (a fresh sign-in), each account's
    /// primary calendar; otherwise exactly the selection, so turning off every
    /// calendar of one account really stops reading it.
    pub fn fetch_events(
        &self,
        selected: &[String],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> (Vec<CalendarEvent>, Vec<String>) {
        let mut events = Vec::new();
        let mut errors = Vec::new();
        for acct in self.accounts() {
            let items = match self.calendar_items(&acct) {
                Ok(i) => i,
                Err(e) => {
                    errors.push(account_error(&acct, &e));
                    continue;
                }
            };
            let wanted: Vec<&str> = selected
                .iter()
                .filter_map(|k| split_key(k))
                .filter(|(a, _)| a.eq_ignore_ascii_case(&acct))
                .map(|(_, id)| id)
                .collect();
            let primary_only = selected.is_empty();
            for cal in items.iter().filter(|c| {
                if primary_only {
                    c["primary"].as_bool() == Some(true)
                } else {
                    c["id"].as_str().is_some_and(|id| wanted.contains(&id))
                }
            }) {
                let name = cal["summaryOverride"]
                    .as_str()
                    .or(cal["summary"].as_str())
                    .unwrap_or("Calendar")
                    .to_string();
                match self.calendar_events(&acct, cal, from, to) {
                    Ok(mut evs) => events.append(&mut evs),
                    Err(e) => errors.push(calendar_error(&acct, &name, &e)),
                }
            }
        }
        (events, errors)
    }

    fn calendar_events(
        &self,
        acct: &str,
        cal: &Value,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<CalendarEvent>> {
        let id = cal["id"].as_str().context("calendar without an id")?;
        let name = cal["summaryOverride"]
            .as_str()
            .or(cal["summary"].as_str())
            .unwrap_or(id);
        let path = format!("/calendars/{}/events", urlenc(id));
        let (min, max) = (from.to_rfc3339(), to.to_rfc3339());
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        for _ in 0..10 {
            let mut q = vec![
                ("timeMin", min.as_str()),
                ("timeMax", max.as_str()),
                ("singleEvents", "true"),
                ("orderBy", "startTime"),
                ("maxResults", "250"),
                ("fields", EVENT_FIELDS),
            ];
            if let Some(p) = &page {
                q.push(("pageToken", p));
            }
            let v = self.api_get(acct, &path, &q)?;
            for e in v["items"].as_array().into_iter().flatten() {
                let mut e = e.clone();
                e["calendarId"] = id.into();
                e["calendarName"] = name.into();
                e["calendarColor"] = cal["backgroundColor"].clone();
                e["accountEmail"] = acct.into();
                out.extend(map_event(&e));
            }
            match v["nextPageToken"].as_str() {
                Some(p) => page = Some(p.to_string()),
                None => break,
            }
        }
        Ok(out)
    }
}

const EVENT_FIELDS: &str = "nextPageToken,items(id,iCalUID,status,summary,description,location,start,end,hangoutLink,htmlLink,conferenceData(entryPoints(entryPointType,uri)),attendees(email,displayName,responseStatus,self,resource,organizer),organizer(email,displayName),recurringEventId,transparency,eventType)";

fn info(account: &str, c: &Value) -> CalendarInfo {
    let id = c["id"].as_str().unwrap_or_default();
    CalendarInfo {
        id: key(account, id),
        name: c["summaryOverride"]
            .as_str()
            .or(c["summary"].as_str())
            .unwrap_or(id)
            .to_string(),
        primary: c["primary"].as_bool() == Some(true),
        color: c["backgroundColor"].as_str().map(str::to_string),
        account: Some(account.to_string()),
    }
}

fn account_error(account: &str, e: &anyhow::Error) -> String {
    format!("Google Calendar ({account}): {e}")
}

fn calendar_error(account: &str, name: &str, e: &anyhow::Error) -> String {
    match e.downcast_ref::<GoogleError>() {
        // Google answers 404 (not 403) for a calendar that is in your list but
        // whose events you can't read, e.g. a Workspace calendar shared at
        // free/busy level.
        Some(GoogleError::Api { status: 404, .. }) => format!(
            "Google won't share event details for “{name}” with {account}. Turn it off, or ask its owner to share full details."
        ),
        Some(GoogleError::Api { status: 403, .. }) => {
            format!("Google won't share “{name}” with Stenly. Turn it off, or sign in again.")
        }
        Some(GoogleError::Api { status, .. }) => {
            format!("Google returned an error ({status}) for “{name}”.")
        }
        Some(other) => account_error(account, &anyhow::anyhow!("{other}")),
        None => format!("Couldn't load “{name}”."),
    }
}

// ---------------------------------------------------------------- pipeline

/// Whether any Google account is signed in on this profile.
pub fn has_accounts() -> bool {
    !signed_in_accounts().is_empty()
}

/// Events for the signed-in accounts, for [`crate::calendar::upcoming`]. Quiet
/// (no events, no errors) when nothing is signed in.
pub fn upcoming(
    cfg: &CalendarConfig,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> (Vec<CalendarEvent>, Vec<String>) {
    upcoming_with(&Client::system(), cfg, from, to)
}

pub fn upcoming_with(
    client: &Client,
    cfg: &CalendarConfig,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> (Vec<CalendarEvent>, Vec<String>) {
    if client.accounts().is_empty() {
        return (vec![], vec![]);
    }
    if client.creds.is_none() {
        return (vec![], vec![GoogleError::NotConfigured.to_string()]);
    }
    client.fetch_events(&cfg.google_calendar_ids, from, to)
}

// ------------------------------------------------------------------ events

fn parse_dt(v: &Value) -> Option<DateTime<Utc>> {
    v["dateTime"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}

/// Google Calendar API event JSON → [`CalendarEvent`]. All-day, cancelled and
/// out-of-office / focus / birthday events are skipped. The caller may add
/// `calendarId`, `calendarName`, `calendarColor` and `accountEmail` to the JSON.
///
/// The call link comes from `conferenceData` first (Meet, and the Zoom and
/// Teams add-ons), then `hangoutLink`, `location` and `description`.
pub fn map_event(e: &Value) -> Option<CalendarEvent> {
    if e["status"].as_str() == Some("cancelled") {
        return None;
    }
    if matches!(
        e["eventType"].as_str(),
        Some("outOfOffice" | "focusTime" | "workingLocation" | "birthday")
    ) {
        return None;
    }
    let start = parse_dt(&e["start"])?;
    let end = parse_dt(&e["end"]).unwrap_or(start + chrono::Duration::minutes(30));
    let conf: Vec<String> = e["conferenceData"]["entryPoints"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["entryPointType"].as_str() == Some("video"))
        .filter_map(|p| p["uri"].as_str().map(str::to_string))
        .collect();
    let hangout = e["hangoutLink"].as_str();
    let location = e["location"].as_str();
    let description = e["description"].as_str();
    let link = crate::meeting_url::find_meeting_link(
        conf.iter()
            .map(String::as_str)
            .chain([hangout, location, description].into_iter().flatten()),
    );
    let attendees: Vec<Attendee> = e["attendees"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["resource"].as_bool() != Some(true))
        .map(|a| Attendee {
            name: a["displayName"].as_str().map(str::to_string),
            email: a["email"].as_str().unwrap_or_default().to_string(),
            status: a["responseStatus"].as_str().map(|s| {
                match s {
                    "accepted" => "ACCEPTED",
                    "declined" => "DECLINED",
                    "tentative" => "TENTATIVE",
                    _ => "NEEDS-ACTION",
                }
                .to_string()
            }),
        })
        .collect();
    let me = e["attendees"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["self"].as_bool() == Some(true));
    let declined = me.is_some_and(|a| a["responseStatus"].as_str() == Some("declined"));
    let uid = e["iCalUID"]
        .as_str()
        .or(e["id"].as_str())
        .unwrap_or_default()
        .to_string();
    Some(CalendarEvent {
        id: format!("{uid}@{}", start.timestamp()),
        uid,
        title: e["summary"]
            .as_str()
            .unwrap_or("Untitled meeting")
            .to_string(),
        start,
        end,
        link,
        organizer: e["organizer"]["email"].as_str().map(|em| Attendee {
            name: e["organizer"]["displayName"].as_str().map(str::to_string),
            email: em.to_string(),
            status: None,
        }),
        attendees,
        description: description.map(str::to_string),
        // "You": your own attendee entry, else the signed-in account, else the
        // calendar id (Kody).
        calendar: me
            .and_then(|a| a["email"].as_str())
            .or(e["accountEmail"].as_str())
            .or(e["calendarId"].as_str())
            .map(str::to_string),
        declined,
        recurring: e["recurringEventId"].is_string(),
        html_link: e["htmlLink"].as_str().map(str::to_string),
        calendar_name: e["calendarName"].as_str().map(str::to_string),
        color: e["calendarColor"].as_str().map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting_url::Provider;
    use serde_json::json;

    fn ev(extra: Value) -> Value {
        let mut base = json!({
            "id": "e1", "iCalUID": "e1@google.com", "status": "confirmed", "summary": "Roadmap sync",
            "start": {"dateTime": "2026-11-03T10:00:00+09:00"}, "end": {"dateTime": "2026-11-03T10:30:00+09:00"},
            "accountEmail": "me@example.com", "calendarId": "me@example.com", "calendarName": "Work", "calendarColor": "#039be5",
        });
        for (k, v) in extra.as_object().unwrap() {
            base[k] = v.clone();
        }
        base
    }

    #[test]
    fn conference_data_beats_free_text() {
        let e = ev(json!({
            "location": "https://zoom.us/j/1111111111",
            "conferenceData": {"entryPoints": [{"entryPointType": "phone", "uri": "tel:+15550100"}, {"entryPointType": "video", "uri": "https://meet.google.com/abc-defg-hij"}]},
        }));
        let m = map_event(&e).unwrap();
        assert_eq!(m.link.as_ref().unwrap().provider, Provider::GoogleMeet);
        assert_eq!(m.calendar_name.as_deref(), Some("Work"));
        assert_eq!(m.color.as_deref(), Some("#039be5"));
    }

    #[test]
    fn finds_zoom_teams_and_hangout_links() {
        let zoom = map_event(&ev(json!({"conferenceData": {"entryPoints": [{"entryPointType": "video", "uri": "https://us02web.zoom.us/j/81234567890?pwd=x"}]}}))).unwrap();
        assert_eq!(zoom.link.unwrap().provider, Provider::Zoom);
        let teams = map_event(&ev(json!({"description": "Join: https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc%40thread.v2/0"}))).unwrap();
        assert_eq!(teams.link.unwrap().provider, Provider::Teams);
        let loc = map_event(&ev(
            json!({"location": "https://us02web.zoom.us/j/81234567890"}),
        ))
        .unwrap();
        assert_eq!(loc.link.unwrap().provider, Provider::Zoom);
        let hang = map_event(&ev(
            json!({"hangoutLink": "https://meet.google.com/xyz-abcd-efg"}),
        ))
        .unwrap();
        assert_eq!(hang.link.unwrap().provider, Provider::GoogleMeet);
        assert!(map_event(&ev(json!({}))).unwrap().link.is_none());
    }

    #[test]
    fn attendees_declines_and_identity() {
        let e = ev(json!({
            "organizer": {"email": "alex@example.com", "displayName": "Alex Example"},
            "attendees": [
                {"email": "alex@example.com", "displayName": "Alex Example", "responseStatus": "accepted", "organizer": true},
                {"email": "me@example.com", "self": true, "responseStatus": "declined"},
                {"email": "room-4@resource.calendar.google.com", "resource": true},
                {"email": "sam@example.com", "responseStatus": "tentative"},
            ],
            "recurringEventId": "r1",
        }));
        let m = map_event(&e).unwrap();
        assert_eq!(m.attendees.len(), 3);
        assert_eq!(m.attendees[2].status.as_deref(), Some("TENTATIVE"));
        assert!(m.declined && m.recurring);
        assert_eq!(m.organizer.unwrap().display(), "Alex Example");
        assert_eq!(m.calendar.as_deref(), Some("me@example.com"));
        // Not on the invite (a shared calendar): "you" is the signed-in account.
        let shared = map_event(&ev(json!({"calendarId": "team@group.calendar.google.com", "attendees": [{"email": "sam@example.com"}]}))).unwrap();
        assert_eq!(shared.calendar.as_deref(), Some("me@example.com"));
        assert!(crate::calendar::has_guests(&shared));
    }

    #[test]
    fn skips_non_meetings() {
        assert!(map_event(&ev(json!({"status": "cancelled"}))).is_none());
        assert!(map_event(&ev(json!({"eventType": "focusTime"}))).is_none());
        assert!(
            map_event(&ev(
                json!({"start": {"date": "2026-11-03"}, "end": {"date": "2026-11-04"}})
            ))
            .is_none()
        );
    }

    #[test]
    fn keys_round_trip() {
        let k = key("me@example.com", "team@group.calendar.google.com");
        assert_eq!(
            split_key(&k),
            Some(("me@example.com", "team@group.calendar.google.com"))
        );
        let cals = vec![
            CalendarInfo {
                id: key("a@example.com", "a@example.com"),
                name: "A".into(),
                primary: true,
                color: None,
                account: Some("a@example.com".into()),
            },
            CalendarInfo {
                id: key("a@example.com", "x"),
                name: "X".into(),
                primary: false,
                color: None,
                account: Some("a@example.com".into()),
            },
            CalendarInfo {
                id: key("b@example.com", "b@example.com"),
                name: "B".into(),
                primary: true,
                color: None,
                account: Some("b@example.com".into()),
            },
        ];
        assert_eq!(
            default_selection(&cals, "A@example.com"),
            vec![key("a@example.com", "a@example.com")]
        );
    }

    #[test]
    fn mock_endpoints_are_loopback_only() {
        assert!(is_loopback("http://127.0.0.1:4000"));
        assert!(is_loopback("http://localhost:4000/x"));
        assert!(is_loopback("http://[::1]:4000"));
        assert!(!is_loopback("http://127.0.0.1.evil.example:4000"));
        assert!(!is_loopback("https://127.0.0.1:4000"));
        assert!(!is_loopback("http://evil.example"));
    }

    #[test]
    fn account_debug_hides_tokens() {
        let a = Account {
            email: "me@example.com".into(),
            refresh_token: "rt-secret".into(),
            access_token: "at-secret".into(),
            expires_at: 1,
        };
        let s = format!("{a:?}");
        assert!(s.contains("me@example.com") && !s.contains("secret"), "{s}");
    }

    #[test]
    fn error_messages_are_classified() {
        assert!(is_reauth(
            &GoogleError::Reauth("me@example.com".into()).to_string()
        ));
        assert!(is_unreachable(&GoogleError::Unreachable.to_string()));
        assert!(!is_reauth(&GoogleError::Unreachable.to_string()));
    }
}
