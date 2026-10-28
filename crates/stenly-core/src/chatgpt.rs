//! "Sign in with ChatGPT": summaries paid for by the user's ChatGPT plan
//! (Plus/Pro/…) instead of an API key.
//!
//! The sign-in is the public OAuth/PKCE browser flow that OpenAI's Codex CLI
//! uses with ChatGPT accounts (openai/codex, Apache-2.0); this is our own
//! implementation of the same protocol on top of [`crate::oauth`]. Tokens live
//! in the Keychain ([`crate::secrets`]), never in config.toml, and are refreshed
//! automatically. Stenly never reads another app's credentials (such as
//! `~/.codex/auth.json`).
//!
//! Requests go to the ChatGPT Codex backend, which speaks the Responses API
//! (streamed as server-sent events), not chat completions: [`respond`] adapts.
//! OpenAI has no official third-party program for this, so every failure maps
//! to a plain message that tells the user what to do instead.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

/// OpenAI's sign-in server.
pub const ISSUER: &str = "https://auth.openai.com";
/// The public OAuth client of the ChatGPT-account sign-in (Codex CLI's).
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// The backend that serves ChatGPT-plan requests (Responses API).
pub const BACKEND: &str = "https://chatgpt.com/backend-api/codex";
/// Summaries need a good writer more than a deep thinker: the current
/// workhorse model, with little reasoning (it's faster and spends less of the
/// plan's usage). `llm.chatgpt.model` / `reasoning_effort` change it.
pub const DEFAULT_MODEL: &str = "gpt-6.1-sol";
pub const DEFAULT_EFFORT: &str = "low";
/// Loopback ports the client's redirect URI is registered for.
const PORTS: &[u16] = &[1455, 1457];
const CALLBACK: &str = "/auth/callback";
const SCOPE: &str = "openid profile email offline_access";
/// Sent as `originator`, so OpenAI can tell these requests come from Stenly.
const ORIGINATOR: &str = "stenly";
/// Refresh when the access token has less than this left.
const REFRESH_MARGIN_SECS: i64 = 300;

/// A failure retrying won't fix: signed out, plan limits, a blocked sign-in.
/// [`crate::summary`] doesn't retry these.
#[derive(Debug)]
pub struct Fatal(pub String);

impl std::fmt::Display for Fatal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Fatal {}

fn fatal(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Fatal(msg.into()))
}

const SIGN_IN_AGAIN: &str = "your ChatGPT sign-in has expired or was revoked. Sign in again in Settings → Summaries";
const NOT_SIGNED_IN: &str = "not signed in to ChatGPT. Sign in in Settings → Summaries";
const BLOCKED: &str = "OpenAI refused this ChatGPT sign-in for Stenly (it may have blocked sign-ins from apps other than its own). Choose another summary provider in Settings → Summaries";

// ---------------------------------------------------------------- tokens

/// The stored sign-in. Serialized as JSON into one Keychain item.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub id_token: String,
    /// ChatGPT account (workspace) id, sent as `chatgpt-account-id`.
    #[serde(default)]
    pub account_id: Option<String>,
    /// Unix seconds when `access_token` expires (0 = unknown).
    #[serde(default)]
    pub expires_at: i64,
}

// Keep tokens out of logs and panics.
impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens").field("account_id", &self.account_id).field("expires_at", &self.expires_at).finish_non_exhaustive()
    }
}

/// Who is signed in, for the settings UI (read from the stored id token; no network).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Account {
    pub email: Option<String>,
    /// "plus", "pro", "team", … as ChatGPT reports it.
    pub plan: Option<String>,
}

impl Account {
    /// "alex@example.com · Plus"
    pub fn label(&self) -> String {
        let plan = self.plan.as_deref().filter(|p| !p.is_empty()).map(|p| {
            let mut c = p.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        });
        match (&self.email, plan) {
            (Some(e), Some(p)) => format!("{e} · {p}"),
            (Some(e), None) => e.clone(),
            (None, Some(p)) => format!("ChatGPT {p}"),
            (None, None) => "Signed in".into(),
        }
    }
}

/// The claims of a JWT (no signature check: we only read our own tokens).
fn claims(jwt: &str) -> Value {
    use base64::Engine as _;
    let Some(payload) = jwt.split('.').nth(1) else { return Value::Null };
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

fn auth_claims(jwt: &str) -> Value {
    claims(jwt)["https://api.openai.com/auth"].clone()
}

impl Tokens {
    fn from_exchange(r: TokenResponse, previous: Option<&Tokens>) -> Result<Self> {
        let access_token = r.access_token.context("the sign-in server returned no access token")?;
        let refresh_token = r.refresh_token.or_else(|| previous.map(|p| p.refresh_token.clone())).filter(|t| !t.is_empty()).context("the sign-in server returned no refresh token")?;
        let id_token = r.id_token.or_else(|| previous.map(|p| p.id_token.clone())).unwrap_or_default();
        let account = |jwt: &str| auth_claims(jwt)["chatgpt_account_id"].as_str().map(str::to_string);
        let account_id = account(&id_token).or_else(|| account(&access_token)).or_else(|| previous.and_then(|p| p.account_id.clone()));
        let now = chrono::Utc::now().timestamp();
        let expires_at = claims(&access_token)["exp"].as_i64().or(r.expires_in.map(|s| now + s)).unwrap_or(now + 3600);
        Ok(Self { access_token, refresh_token, id_token, account_id, expires_at })
    }

    pub fn account(&self) -> Account {
        let id = claims(&self.id_token);
        let auth = &id["https://api.openai.com/auth"];
        let plan = auth["chatgpt_plan_type"].as_str().map(str::to_string).or_else(|| auth_claims(&self.access_token)["chatgpt_plan_type"].as_str().map(str::to_string));
        Account { email: id["email"].as_str().map(str::to_string), plan }
    }

    fn stale(&self) -> bool {
        self.expires_at > 0 && chrono::Utc::now().timestamp() > self.expires_at - REFRESH_MARGIN_SECS
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<i64>,
}

/// Where the sign-in is kept. The Keychain in the app; memory in tests.
pub trait Store: Sync {
    fn load(&self) -> Option<Tokens>;
    fn save(&self, t: Option<&Tokens>) -> Result<()>;
}

/// The Keychain item of this profile (see [`crate::secrets::chatgpt_tokens`]).
pub struct Keychain;

impl Store for Keychain {
    fn load(&self) -> Option<Tokens> {
        serde_json::from_str(&crate::secrets::chatgpt_tokens()?).ok()
    }
    fn save(&self, t: Option<&Tokens>) -> Result<()> {
        let json = match t {
            Some(t) => serde_json::to_string(t)?,
            None => String::new(),
        };
        crate::secrets::set_chatgpt_tokens(&json)
    }
}

/// The sign-in server and token store a request uses.
pub struct Auth<'a> {
    pub issuer: String,
    pub store: &'a dyn Store,
}

impl Auth<'static> {
    pub fn live() -> Self {
        Auth { issuer: ISSUER.into(), store: &Keychain }
    }
}

/// Whether this profile is signed in (Keychain only; no network).
pub fn signed_in() -> Option<Account> {
    Keychain.load().map(|t| t.account())
}

/// Forget the sign-in.
pub fn sign_out() -> Result<()> {
    Keychain.save(None)
}

/// One refresh at a time in this process: the server rotates refresh tokens,
/// so two parallel refreshes (summary parts run in parallel) would revoke each other.
static REFRESH: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).build().into()
}

impl Auth<'_> {
    /// A current access token and account id, refreshed first when it is
    /// about to expire (or always with `force`).
    pub fn access(&self, force: bool) -> Result<Tokens> {
        let _one = REFRESH.lock().unwrap_or_else(|e| e.into_inner());
        let t = self.store.load().ok_or_else(|| fatal(NOT_SIGNED_IN))?;
        if !force && !t.stale() {
            return Ok(t);
        }
        match self.refresh(&t) {
            Ok(fresh) => {
                self.store.save(Some(&fresh))?;
                Ok(fresh)
            }
            Err(e) => {
                // another process (the app vs stenly-cli) may have refreshed first
                match self.store.load() {
                    Some(now) if now.refresh_token != t.refresh_token => Ok(now),
                    _ => Err(e),
                }
            }
        }
    }

    fn refresh(&self, t: &Tokens) -> Result<Tokens> {
        let url = format!("{}/oauth/token", self.issuer.trim_end_matches('/'));
        let mut r = agent(Duration::from_secs(30))
            .post(&url)
            .send_json(json!({ "client_id": CLIENT_ID, "grant_type": "refresh_token", "refresh_token": t.refresh_token }))
            .context("couldn't reach ChatGPT to renew the sign-in")?;
        let status = r.status().as_u16();
        let text = r.body_mut().read_to_string().unwrap_or_default();
        if !(200..300).contains(&status) {
            // never log the body: the server may echo the token
            tracing::warn!(status, "ChatGPT token refresh failed");
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let code = v["error"]["code"].as_str().or(v["error"].as_str()).or(v["code"].as_str()).unwrap_or("").to_ascii_lowercase();
            if status == 401 || status == 400 && (code == "invalid_grant" || code.starts_with("refresh_token_")) || code.starts_with("refresh_token_") {
                return Err(fatal(SIGN_IN_AGAIN));
            }
            if status == 403 {
                return Err(fatal(BLOCKED));
            }
            anyhow::bail!("couldn't renew the ChatGPT sign-in (HTTP {status})");
        }
        let resp: TokenResponse = serde_json::from_str(&text).context("the sign-in server's reply wasn't JSON")?;
        Tokens::from_exchange(resp, Some(t))
    }
}

// ---------------------------------------------------------------- sign-in

/// The browser URL that starts the sign-in.
fn authorize_url(issuer: &str, redirect_uri: &str, challenge: &str, state: &str) -> String {
    format!(
        "{}/oauth/authorize?{}",
        issuer.trim_end_matches('/'),
        crate::oauth::form(&[
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", redirect_uri),
            ("scope", SCOPE),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("state", state),
            ("originator", ORIGINATOR),
        ])
    )
}

/// Exchange the authorization code for tokens.
fn exchange(issuer: &str, code: &str, redirect_uri: &str, verifier: &str) -> Result<Tokens> {
    let url = format!("{}/oauth/token", issuer.trim_end_matches('/'));
    let mut r = agent(Duration::from_secs(30))
        .post(&url)
        .content_type("application/x-www-form-urlencoded")
        .send(crate::oauth::form(&[("grant_type", "authorization_code"), ("client_id", CLIENT_ID), ("code", code), ("redirect_uri", redirect_uri), ("code_verifier", verifier)]))
        .context("couldn't reach ChatGPT to finish signing in")?;
    let status = r.status().as_u16();
    let text = r.body_mut().read_to_string().unwrap_or_default();
    if !(200..300).contains(&status) {
        tracing::warn!(status, "ChatGPT code exchange failed");
        if status == 403 {
            return Err(fatal(BLOCKED));
        }
        anyhow::bail!("ChatGPT sign-in failed (HTTP {status}). Try again");
    }
    Tokens::from_exchange(serde_json::from_str(&text).context("the sign-in server's reply wasn't JSON")?, None)
}

/// Interactive sign-in: `open_url` gets the authorize URL (the app opens the
/// browser); blocks until the browser comes back to the loopback listener,
/// then stores the tokens. `cancelled` aborts the wait (a Cancel button).
pub fn sign_in(open_url: &dyn Fn(&str), cancelled: &dyn Fn() -> bool) -> Result<Account> {
    sign_in_with(&Auth::live(), open_url, cancelled)
}

pub fn sign_in_with(auth: &Auth, open_url: &dyn Fn(&str), cancelled: &dyn Fn() -> bool) -> Result<Account> {
    let (listener, redirect_uri) = crate::oauth::loopback_fixed(PORTS, CALLBACK)?;
    let crate::oauth::Pkce { verifier, challenge, state } = crate::oauth::pkce();
    open_url(&authorize_url(&auth.issuer, &redirect_uri, &challenge, &state));
    let code = crate::oauth::wait_for_code_at(&listener, CALLBACK, &state, "ChatGPT", Duration::from_secs(600), cancelled)?;
    drop(listener);
    let t = exchange(&auth.issuer, &code, &redirect_uri, &verifier)?;
    auth.store.save(Some(&t))?;
    Ok(t.account())
}

// ---------------------------------------------------------------- Responses API

/// One function call the model asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub call_id: String,
    pub name: String,
    /// JSON text.
    pub arguments: String,
}

/// What a response produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reply {
    pub text: String,
    pub calls: Vec<Call>,
}

/// A Responses API `message` input item.
pub fn message(role: &str, text: &str) -> Value {
    let kind = if role == "assistant" { "output_text" } else { "input_text" };
    json!({ "type": "message", "role": role, "content": [{ "type": kind, "text": text }] })
}

/// A plain prompt: optional system/developer instructions and one user turn.
pub fn complete(target: &crate::config::LlmTarget, system: Option<&str>, user: &str, timeout_secs: u64) -> Result<String> {
    let mut input = Vec::new();
    if let Some(s) = system.filter(|s| !s.trim().is_empty()) {
        input.push(message("developer", s));
    }
    input.push(message("user", user));
    Ok(respond(target, input, &[], timeout_secs)?.text)
}

/// One model turn on the ChatGPT backend. `input` holds Responses items
/// ([`message`], `function_call`, `function_call_output`); `tools` are
/// Responses function tools (`{"type":"function","name",…}`).
pub fn respond(target: &crate::config::LlmTarget, input: Vec<Value>, tools: &[Value], timeout_secs: u64) -> Result<Reply> {
    respond_with(&Auth::live(), target, input, tools, timeout_secs)
}

pub fn respond_with(auth: &Auth, target: &crate::config::LlmTarget, input: Vec<Value>, tools: &[Value], timeout_secs: u64) -> Result<Reply> {
    anyhow::ensure!(!target.model.is_empty(), "no ChatGPT model set (Settings → Summaries)");
    let base = if target.endpoint.is_empty() { BACKEND } else { target.endpoint.as_str() };
    let url = format!("{}/responses", base.trim_end_matches('/'));
    let mut body = json!({
        "model": target.model,
        "input": input,
        "stream": true,
        "store": false,
        "include": [],
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.to_vec());
        body["tool_choice"] = json!("auto");
        body["parallel_tool_calls"] = json!(false);
    }
    let effort = target.reasoning_effort.trim();
    if !effort.is_empty() && effort != "none" {
        body["reasoning"] = json!({ "effort": effort });
    }
    let session = crate::oauth::b64url(&crate::oauth::random_bytes(12));
    let mut tokens = auth.access(false)?;
    for attempt in 0..2 {
        let mut req = agent(Duration::from_secs(timeout_secs.max(30)))
            .post(&url)
            .header("Authorization", &format!("Bearer {}", tokens.access_token))
            .header("Accept", "text/event-stream")
            .header("originator", ORIGINATOR)
            .header("User-Agent", &format!("stenly/{}", env!("CARGO_PKG_VERSION")))
            .header("session_id", &session);
        if let Some(a) = &tokens.account_id {
            req = req.header("chatgpt-account-id", a);
        }
        let mut resp = req.send_json(&body).context("couldn't reach ChatGPT")?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            let reader = std::io::BufReader::new(resp.body_mut().as_reader());
            return read_stream(reader, &target.model);
        }
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        if status == 401 && attempt == 0 {
            // the token may have been revoked early: renew once and retry
            tokens = auth.access(true)?;
            continue;
        }
        return Err(http_error(status, &text, &target.model));
    }
    Err(fatal(SIGN_IN_AGAIN))
}

/// Parse the server-sent events of a streamed response.
fn read_stream(reader: impl std::io::BufRead, model: &str) -> Result<Reply> {
    let mut reply = Reply::default();
    let mut deltas = String::new();
    let mut message_text = String::new();
    let mut data = String::new();
    let mut done = false;
    for line in reader.lines() {
        let line = line.context("the ChatGPT stream broke off")?;
        if let Some(d) = line.strip_prefix("data:") {
            data.push_str(d.trim_start());
            continue;
        }
        if !line.is_empty() || data.is_empty() {
            continue;
        }
        let ev: Value = match serde_json::from_str(&std::mem::take(&mut data)) {
            Ok(v) => v,
            Err(_) => continue, // e.g. "[DONE]"
        };
        match ev["type"].as_str().unwrap_or("") {
            "response.output_text.delta" => deltas.push_str(ev["delta"].as_str().unwrap_or("")),
            "response.output_item.done" => {
                let item = &ev["item"];
                match item["type"].as_str() {
                    Some("message") => {
                        for c in item["content"].as_array().into_iter().flatten() {
                            if c["type"] == "output_text" {
                                message_text.push_str(c["text"].as_str().unwrap_or(""));
                            }
                        }
                    }
                    Some("function_call") => reply.calls.push(Call {
                        call_id: item["call_id"].as_str().or(item["id"].as_str()).unwrap_or("").to_string(),
                        name: item["name"].as_str().unwrap_or("").to_string(),
                        arguments: item["arguments"].as_str().map(str::to_string).unwrap_or_else(|| item["arguments"].to_string()),
                    }),
                    _ => {}
                }
            }
            "response.completed" => {
                done = true;
                break;
            }
            "response.incomplete" => {
                let reason = ev["response"]["incomplete_details"]["reason"].as_str().unwrap_or("unknown");
                if reason == "max_output_tokens" && !(message_text.is_empty() && deltas.is_empty()) {
                    done = true;
                    break;
                }
                anyhow::bail!("ChatGPT stopped before finishing ({reason})");
            }
            "response.failed" => return Err(api_error(&ev["response"]["error"], model)),
            "error" => return Err(api_error(if ev["error"].is_object() { &ev["error"] } else { &ev }, model)),
            _ => {}
        }
    }
    anyhow::ensure!(done, "the ChatGPT stream ended before the answer was complete");
    reply.text = if message_text.is_empty() { deltas } else { message_text };
    Ok(reply)
}

/// A user-facing error for an `error` object from the backend.
fn api_error(e: &Value, model: &str) -> anyhow::Error {
    let code = e["code"].as_str().or(e["type"].as_str()).unwrap_or("").to_string();
    let msg = e["message"].as_str().unwrap_or("").to_string();
    match code.as_str() {
        "usage_limit_reached" | "rate_limit_exceeded" => fatal(usage_limit(e)),
        "usage_not_included" => fatal(NOT_INCLUDED),
        "insufficient_quota" => fatal("your ChatGPT account has no usage left for this. Check your plan at chatgpt.com"),
        "context_length_exceeded" => anyhow::anyhow!("the meeting is too long for {model}"),
        "invalid_prompt" | "content_filter" => fatal(format!("ChatGPT declined this request{}", if msg.is_empty() { String::new() } else { format!(": {msg}") })),
        _ if !msg.is_empty() => anyhow::anyhow!("ChatGPT: {}", msg.chars().take(200).collect::<String>()),
        _ => anyhow::anyhow!("ChatGPT failed ({})", if code.is_empty() { "no reason given" } else { &code }),
    }
}

const NOT_INCLUDED: &str = "your ChatGPT plan doesn't include this (it needs Plus, Pro, Business, Edu or Enterprise). Upgrade at chatgpt.com, or choose another summary provider";

fn usage_limit(e: &Value) -> String {
    let when = e["resets_at"].as_i64().and_then(|s| chrono::DateTime::from_timestamp(s, 0)).map(|t| {
        let t = t.with_timezone(&chrono::Local);
        let today = chrono::Local::now().date_naive();
        if t.date_naive() == today { t.format(" It resets at %H:%M.").to_string() } else { t.format(" It resets %a %-d %b at %H:%M.").to_string() }
    });
    let plan = e["plan_type"].as_str().map(|p| format!(" ({p})")).unwrap_or_default();
    format!("you've reached your ChatGPT plan's usage limit{plan}.{} Summaries will work again then, or choose another provider in Settings → Summaries", when.unwrap_or_default())
}

/// A user-facing error for a non-2xx reply.
fn http_error(status: u16, body: &str, model: &str) -> anyhow::Error {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let e = if v["error"].is_object() { v["error"].clone() } else { v.clone() };
    let detail = e["detail"].as_str().or(v["detail"].as_str()).or(e["message"].as_str()).unwrap_or("").to_string();
    let low = detail.to_lowercase();
    // never log the body wholesale; the status and the server's own message are enough
    tracing::warn!(status, message = %detail.chars().take(200).collect::<String>(), "ChatGPT request failed");
    let html = body.trim_start().starts_with('<');
    match status {
        401 => fatal(SIGN_IN_AGAIN),
        403 => fatal(BLOCKED),
        429 => match e["type"].as_str().or(e["code"].as_str()) {
            Some("usage_not_included") => fatal(NOT_INCLUDED),
            Some("usage_limit_reached") => fatal(usage_limit(&e)),
            _ => anyhow::anyhow!("ChatGPT is rate limiting requests; try again in a minute"),
        },
        404 => fatal(format!("model “{model}” isn't available on ChatGPT (check llm.chatgpt.model)")),
        400 if low.contains("model") && (low.contains("not supported") || low.contains("not found") || low.contains("does not exist")) => {
            fatal(format!("model “{model}” isn't available with your ChatGPT plan ({})", detail.chars().take(160).collect::<String>()))
        }
        400 | 422 if low.contains("instructions") || low.contains("client") || low.contains("originator") => fatal(BLOCKED),
        _ if html => fatal(BLOCKED),
        _ if !detail.is_empty() => anyhow::anyhow!("ChatGPT answered HTTP {status}: {}", detail.chars().take(200).collect::<String>()),
        _ => anyhow::anyhow!("ChatGPT answered HTTP {status}"),
    }
}

/// One tiny request: checks the sign-in, the plan and the model. Returns latency in ms.
pub fn ping(target: &crate::config::LlmTarget) -> Result<u128> {
    let t0 = std::time::Instant::now();
    complete(target, None, "Reply with OK.", 60)?;
    Ok(t0.elapsed().as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Backend, LlmTarget};
    use crate::destinations::testutil::MockServer;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Mem(Mutex<Option<Tokens>>);
    impl Store for Mem {
        fn load(&self) -> Option<Tokens> {
            self.0.lock().unwrap().clone()
        }
        fn save(&self, t: Option<&Tokens>) -> Result<()> {
            *self.0.lock().unwrap() = t.cloned();
            Ok(())
        }
    }

    fn jwt(claims: Value) -> String {
        use base64::Engine as _;
        let e = |v: &Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string());
        format!("{}.{}.sig", e(&json!({"alg": "none"})), e(&claims))
    }

    fn id_token() -> String {
        jwt(json!({"email": "test@example.com", "https://api.openai.com/auth": {"chatgpt_account_id": "acct_1", "chatgpt_plan_type": "plus"}}))
    }

    fn tokens(exp: i64) -> Tokens {
        Tokens { access_token: jwt(json!({"exp": exp})), refresh_token: "rt-old".into(), id_token: id_token(), account_id: Some("acct_1".into()), expires_at: exp }
    }

    fn target(url: &str) -> LlmTarget {
        LlmTarget { endpoint: url.into(), model: "gpt-test".into(), api_key: None, reasoning_effort: "low".into(), keep_alive: None, local: false, cli: None, backend: Backend::ChatGpt }
    }

    fn sse(events: &[Value]) -> String {
        events.iter().map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap())).collect()
    }

    fn text_reply(text: &str) -> String {
        sse(&[
            json!({"type": "response.created", "response": {"id": "r1"}}),
            json!({"type": "response.output_text.delta", "delta": text}),
            json!({"type": "response.output_item.done", "item": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}}),
            json!({"type": "response.completed", "response": {"id": "r1"}}),
        ])
    }

    #[test]
    fn authorize_url_has_pkce_and_codex_flow_params() {
        let u = authorize_url(ISSUER, "http://127.0.0.1:1455/auth/callback", "CH", "ST");
        assert!(u.starts_with("https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_EMoamEEZ73f0CkXaXp7hrann&"));
        assert!(u.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback"));
        assert!(u.contains("code_challenge=CH&code_challenge_method=S256"));
        assert!(u.contains("scope=openid%20profile%20email%20offline_access"));
        assert!(u.contains("state=ST") && u.contains("originator=stenly"));
    }

    #[test]
    fn exchanges_the_code_and_reads_the_account() {
        let id = id_token();
        let access = jwt(json!({"exp": 4_000_000_000i64}));
        let (i2, a2) = (id.clone(), access.clone());
        let srv = MockServer::start(move |_| (200, json!({"id_token": i2, "access_token": a2, "refresh_token": "rt-1"}).to_string()));
        let t = exchange(&srv.url, "the-code", "http://127.0.0.1:1455/auth/callback", "the-verifier").unwrap();
        let req = &srv.requests()[0];
        assert_eq!(req.path, "/oauth/token");
        assert_eq!(req.header("content-type"), Some("application/x-www-form-urlencoded"));
        for p in ["grant_type=authorization_code", "client_id=app_EMoamEEZ73f0CkXaXp7hrann", "code=the-code", "code_verifier=the-verifier", "redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback"] {
            assert!(req.body.contains(p), "{p} missing");
        }
        assert_eq!(t.account_id.as_deref(), Some("acct_1"));
        assert_eq!(t.expires_at, 4_000_000_000);
        assert_eq!(t.account(), Account { email: Some("test@example.com".into()), plan: Some("plus".into()) });
        assert_eq!(t.account().label(), "test@example.com · Plus");
        // the token never shows in Debug output
        assert!(!format!("{t:?}").contains(&access));
    }

    #[test]
    fn full_sign_in_through_the_loopback() {
        let id = id_token();
        let srv = MockServer::start(move |_| (200, json!({"id_token": id, "access_token": jwt(json!({"exp": 4_000_000_000i64})), "refresh_token": "rt-1"}).to_string()));
        let store = Mem::default();
        let auth = Auth { issuer: srv.url.clone(), store: &store };
        // the "browser": follow the authorize URL's redirect with a code
        let open = |u: &str| {
            let q = u.split_once('?').unwrap().1.to_string();
            std::thread::spawn(move || {
                use std::io::{Read, Write};
                let get = |k: &str| q.split('&').find_map(|kv| kv.strip_prefix(&format!("{k}="))).map(crate::meeting_url::percent_decode_pub).unwrap();
                let (redirect, state) = (get("redirect_uri"), get("state"));
                let hostport = redirect.trim_start_matches("http://").split('/').next().unwrap().to_string();
                let mut s = std::net::TcpStream::connect(hostport).unwrap();
                write!(s, "GET /auth/callback?code=c0de&state={state} HTTP/1.1\r\n\r\n").unwrap();
                let mut out = String::new();
                let _ = s.read_to_string(&mut out);
                assert!(out.contains("Stenly is connected to ChatGPT"));
            });
        };
        match sign_in_with(&auth, &open, &|| false) {
            Ok(acct) => {
                assert_eq!(acct.email.as_deref(), Some("test@example.com"));
                assert_eq!(store.load().unwrap().refresh_token, "rt-1");
                assert!(srv.requests()[0].body.contains("code=c0de"));
            }
            // a real sign-in (or another test run) holding both ports isn't a failure of this code
            Err(e) if format!("{e:#}").contains("in use") => eprintln!("skipped: {e:#}"),
            Err(e) => panic!("{e:#}"),
        }
    }

    #[test]
    fn refreshes_a_stale_token_once_and_keeps_rotating() {
        let srv = MockServer::start(|_| (200, json!({"access_token": jwt(json!({"exp": 4_000_000_000i64})), "refresh_token": "rt-new"}).to_string()));
        let store = Mem::default();
        store.save(Some(&tokens(chrono::Utc::now().timestamp() + 60))).unwrap();
        let auth = Auth { issuer: srv.url.clone(), store: &store };
        let t = auth.access(false).unwrap();
        assert_eq!(t.refresh_token, "rt-new");
        assert_eq!(t.expires_at, 4_000_000_000);
        // the id token and account survive a refresh that doesn't repeat them
        assert_eq!(t.account().plan.as_deref(), Some("plus"));
        assert_eq!(t.account_id.as_deref(), Some("acct_1"));
        let req = &srv.requests()[0];
        let body: Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body, json!({"client_id": CLIENT_ID, "grant_type": "refresh_token", "refresh_token": "rt-old"}));
        // fresh now: no second refresh
        auth.access(false).unwrap();
        assert_eq!(srv.requests().len(), 1);
    }

    #[test]
    fn revoked_refresh_token_asks_to_sign_in_again() {
        let srv = MockServer::start(|_| (400, json!({"error": {"code": "refresh_token_invalidated", "message": "x"}}).to_string()));
        let store = Mem::default();
        store.save(Some(&tokens(0 + 1))).unwrap();
        let auth = Auth { issuer: srv.url.clone(), store: &store };
        let e = auth.access(false).unwrap_err();
        assert!(e.downcast_ref::<Fatal>().is_some());
        assert!(format!("{e:#}").contains("Sign in again"), "{e:#}");
        // not signed in at all
        let empty = Mem::default();
        let e = Auth { issuer: srv.url.clone(), store: &empty }.access(false).unwrap_err();
        assert!(format!("{e}").contains("not signed in"));
    }

    #[test]
    fn responses_adapter_streams_text() {
        let srv = MockServer::start(|r| if r.path == "/responses" { (200, text_reply("{\"title\": \"Plan\"}")) } else { (404, String::new()) });
        let store = Mem::default();
        store.save(Some(&tokens(4_000_000_000))).unwrap();
        let auth = Auth { issuer: "http://unused.invalid".into(), store: &store };
        let input = vec![message("developer", "Be brief."), message("user", "Synthetic transcript: Alex: we ship Friday.")];
        let r = respond_with(&auth, &target(&srv.url), input, &[], 30).unwrap();
        assert_eq!(r.text, "{\"title\": \"Plan\"}");
        assert!(r.calls.is_empty());
        let req = &srv.requests()[0];
        assert_eq!(req.header("chatgpt-account-id"), Some("acct_1"));
        assert_eq!(req.header("originator"), Some("stenly"));
        assert!(req.header("authorization").unwrap().starts_with("Bearer "));
        let body: Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["model"], "gpt-test");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["reasoning"]["effort"], "low");
        assert_eq!(body["input"][0]["role"], "developer");
        assert_eq!(body["input"][1]["content"][0]["type"], "input_text");
        assert!(body.get("messages").is_none() && body.get("temperature").is_none());
    }

    #[test]
    fn responses_adapter_returns_tool_calls() {
        let srv = MockServer::start(|_| {
            (200, sse(&[
                json!({"type": "response.output_item.done", "item": {"type": "function_call", "call_id": "call_9", "name": "past__search", "arguments": "{\"q\":\"Example Labs\"}"}}),
                json!({"type": "response.completed", "response": {}}),
            ]))
        });
        let store = Mem::default();
        store.save(Some(&tokens(4_000_000_000))).unwrap();
        let auth = Auth { issuer: "http://unused.invalid".into(), store: &store };
        let tool = json!({"type": "function", "name": "past__search", "description": "d", "parameters": {"type": "object"}});
        let r = respond_with(&auth, &target(&srv.url), vec![message("user", "who?")], &[tool], 30).unwrap();
        assert_eq!(r.calls, vec![Call { call_id: "call_9".into(), name: "past__search".into(), arguments: "{\"q\":\"Example Labs\"}".into() }]);
        let body: Value = serde_json::from_str(&srv.requests()[0].body).unwrap();
        assert_eq!(body["tools"][0]["name"], "past__search");
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn a_401_refreshes_and_retries_once() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = calls.clone();
        let srv = MockServer::start(move |r| match r.path.as_str() {
            "/oauth/token" => (200, json!({"access_token": jwt(json!({"exp": 4_000_000_000i64})), "refresh_token": "rt-2"}).to_string()),
            _ if c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 => (401, "{}".into()),
            _ => (200, text_reply("OK")),
        });
        let store = Mem::default();
        store.save(Some(&tokens(4_000_000_000))).unwrap();
        let auth = Auth { issuer: srv.url.clone(), store: &store };
        assert_eq!(respond_with(&auth, &target(&srv.url), vec![message("user", "hi")], &[], 30).unwrap().text, "OK");
        assert_eq!(store.load().unwrap().refresh_token, "rt-2");
    }

    fn err_for(status: u16, body: Value) -> String {
        let b = body.to_string();
        let srv = MockServer::start(move |r| if r.path == "/oauth/token" { (401, "{}".into()) } else { (status, b.clone()) });
        let store = Mem::default();
        store.save(Some(&tokens(4_000_000_000))).unwrap();
        let auth = Auth { issuer: srv.url.clone(), store: &store };
        let e = respond_with(&auth, &target(&srv.url), vec![message("user", "hi")], &[], 30).unwrap_err();
        assert!(e.downcast_ref::<Fatal>().is_some(), "{e:#} should be fatal");
        format!("{e:#}")
    }

    #[test]
    fn maps_failures_to_clear_messages() {
        assert!(err_for(429, json!({"error": {"type": "usage_limit_reached", "plan_type": "plus", "resets_at": 4_000_000_000i64}})).contains("usage limit (plus)"));
        assert!(err_for(429, json!({"error": {"type": "usage_not_included"}})).contains("doesn't include"));
        assert!(err_for(403, json!({"detail": "nope"})).contains("refused this ChatGPT sign-in"));
        assert!(err_for(400, json!({"detail": "The 'gpt-test' model is not supported when using Codex with a ChatGPT account."})).contains("isn't available with your ChatGPT plan"));
        // a 401 that a refresh can't fix
        assert!(err_for(401, json!({})).contains("Sign in again"));
    }

    #[test]
    fn stream_failures_are_reported() {
        let failed = sse(&[json!({"type": "response.failed", "response": {"error": {"code": "usage_limit_reached", "message": "limit"}}})]);
        let e = read_stream(std::io::Cursor::new(failed), "m").unwrap_err();
        assert!(e.downcast_ref::<Fatal>().is_some() && format!("{e}").contains("usage limit"));
        let cut = sse(&[json!({"type": "response.output_text.delta", "delta": "half"})]);
        assert!(format!("{}", read_stream(std::io::Cursor::new(cut), "m").unwrap_err()).contains("before the answer was complete"));
        // deltas alone are enough when no final message item is sent
        let deltas = sse(&[json!({"type": "response.output_text.delta", "delta": "O"}), json!({"type": "response.output_text.delta", "delta": "K"}), json!({"type": "response.completed"})]);
        assert_eq!(read_stream(std::io::Cursor::new(deltas), "m").unwrap().text, "OK");
    }
}
