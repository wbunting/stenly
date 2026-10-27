//! Sign-in for remote MCP servers, as the MCP authorization spec describes:
//! protected-resource metadata → authorization-server metadata → dynamic
//! client registration → authorization code + PKCE on a loopback redirect.
//! Tokens are kept in the Keychain (`ai.stenly.context`, `mcp.<id>.oauth`).
//!
//! Kody is special-cased: it reuses the Calendar sign-in (`kody-auth.json`).
//! A server can also use a plain bearer key (`mcp.<id>` in the Keychain).

use anyhow::{Context, Result, bail};
use stenly_core::oauth::{form, same_scheme};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Tokens {
    token_endpoint: String,
    client_id: String,
    resource: String,
    access_token: String,
    refresh_token: Option<String>,
    /// Unix seconds.
    expires_at: i64,
}

fn account(id: &str) -> String {
    format!("mcp.{id}.oauth")
}

pub fn key_account(id: &str) -> String {
    format!("mcp.{id}")
}

/// Bearer token for a server: Kody's sign-in, a stored key, or OAuth tokens
/// (refreshed when expiring, or always with `force`). `None` = none stored.
pub fn token_for(id: &str, force: bool) -> Result<Option<String>> {
    if id == crate::mcp::KODY_ID {
        return Ok(Some(stenly_core::kody::access_token(force)?.1));
    }
    if let Some(k) = stenly_core::secrets::context_secret(&key_account(id)) {
        return Ok(Some(k));
    }
    let Some(raw) = stenly_core::secrets::context_secret(&account(id)) else { return Ok(None) };
    let mut t: Tokens = serde_json::from_str(&raw).context("stored sign-in is unreadable; sign in again")?;
    if force || chrono::Utc::now().timestamp() > t.expires_at - 120 {
        let Some(rt) = t.refresh_token.clone() else { return Ok(None) };
        let r = post_form(&t.token_endpoint, &[("grant_type", "refresh_token"), ("refresh_token", &rt), ("client_id", &t.client_id), ("resource", &t.resource)])?;
        t.access_token = r["access_token"].as_str().context("no access_token in refresh")?.to_string();
        if let Some(n) = r["refresh_token"].as_str() {
            t.refresh_token = Some(n.to_string());
        }
        t.expires_at = chrono::Utc::now().timestamp() + r["expires_in"].as_i64().unwrap_or(3600);
        stenly_core::secrets::set_context_secret(&account(id), &serde_json::to_string(&t)?)?;
    }
    Ok(Some(t.access_token))
}

pub fn has_sign_in(id: &str) -> bool {
    id == crate::mcp::KODY_ID && stenly_core::kody::KodyAuth::load().is_some()
        || stenly_core::secrets::context_secret(&account(id)).is_some()
        || stenly_core::secrets::context_secret(&key_account(id)).is_some()
}

pub fn sign_out(id: &str) {
    let _ = stenly_core::secrets::set_context_secret(&account(id), "");
    let _ = stenly_core::secrets::set_context_secret(&key_account(id), "");
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(8)))
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build()
        .into()
}

fn get_json(url: &str) -> Option<Value> {
    let mut r = agent().get(url).call().ok()?;
    if !(200..300).contains(&r.status().as_u16()) {
        return None;
    }
    r.body_mut().read_json().ok()
}

fn post_form(url: &str, pairs: &[(&str, &str)]) -> Result<Value> {
    let mut r = agent().post(url).content_type("application/x-www-form-urlencoded").send(form(pairs)).with_context(|| format!("calling {url}"))?;
    let status = r.status().as_u16();
    let text = r.body_mut().read_to_string().unwrap_or_default();
    if !(200..300).contains(&status) {
        bail!("sign-in failed (HTTP {status}): {}", text.chars().take(200).collect::<String>());
    }
    serde_json::from_str(&text).context("token endpoint didn't return JSON")
}

/// "linear" → "Linear", for the sign-in page.
fn id_label(id: &str) -> String {
    let mut c = id.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn origin(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("https", url));
    format!("{scheme}://{}", rest.split('/').next().unwrap_or(rest))
}

/// Where to sign in for the MCP server at `url`: (auth-server metadata,
/// resource, scopes the resource asks for).
fn discover(url: &str) -> Result<(Value, String, String)> {
    // 1. protected-resource metadata: from the 401's WWW-Authenticate, else well-known
    let mut prm_urls: Vec<String> = Vec::new();
    if let Ok(mut r) = agent().post(url).header("accept", "application/json, text/event-stream").send_json(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":crate::mcp::PROTOCOL,"capabilities":{},"clientInfo":{"name":"stenly","version":"0"}}})) {
        let www = r.headers().get("www-authenticate").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
        let _ = r.body_mut().read_to_string();
        if let Some(i) = www.find("resource_metadata=\"") {
            let rest = &www[i + 19..];
            if let Some(j) = rest.find('"') {
                prm_urls.push(rest[..j].to_string());
            }
        }
    }
    let o = origin(url);
    let path = url.strip_prefix(&o).unwrap_or("");
    prm_urls.push(format!("{o}/.well-known/oauth-protected-resource{path}"));
    prm_urls.push(format!("{o}/.well-known/oauth-protected-resource"));
    let prm = prm_urls.iter().find_map(|u| get_json(u));
    let resource = prm.as_ref().and_then(|p| p["resource"].as_str().map(str::to_string)).unwrap_or_else(|| url.to_string());
    let scopes = prm.as_ref().and_then(|p| p["scopes_supported"].as_array()).map(|s| s.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_default();
    let issuer = prm.as_ref().and_then(|p| p["authorization_servers"][0].as_str().map(str::to_string)).unwrap_or_else(|| o.clone());
    // 2. authorization-server metadata (RFC 8414, then OpenID)
    let io = origin(&issuer);
    let ipath = issuer.strip_prefix(&io).unwrap_or("").trim_end_matches('/');
    let candidates = [
        format!("{io}/.well-known/oauth-authorization-server{ipath}"),
        format!("{io}/.well-known/openid-configuration{ipath}"),
        format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/')),
        format!("{io}/.well-known/oauth-authorization-server"),
    ];
    let mut meta = candidates.iter().find_map(|u| get_json(u)).with_context(|| format!("{url} doesn't advertise how to sign in (no OAuth metadata)"))?;
    anyhow::ensure!(meta["authorization_endpoint"].is_string() && meta["token_endpoint"].is_string(), "incomplete OAuth metadata");
    // behind a TLS proxy, servers advertise http:// endpoints for an https host
    for k in ["authorization_endpoint", "token_endpoint", "registration_endpoint"] {
        if let Some(u) = meta[k].as_str() {
            meta[k] = Value::String(same_scheme(&issuer, u));
        }
    }
    Ok((meta, resource, scopes))
}

/// Interactive sign-in: opens the browser (`open_url`) and waits up to five
/// minutes for the redirect to a loopback listener.
pub fn sign_in(id: &str, url: &str, open_url: &dyn Fn(&str)) -> Result<()> {
    let (meta, resource, resource_scopes) = discover(url)?;
    let (listener, redirect_uri) = stenly_core::oauth::loopback()?;
    let reg_url = meta["registration_endpoint"].as_str().context("this server doesn't let apps register; add an API key instead")?;
    let client_id = stenly_core::oauth::register_client(&agent(), reg_url, "Stenly", &redirect_uri)?;
    let stenly_core::oauth::Pkce { verifier, challenge, state } = stenly_core::oauth::pkce();
    let mut params: Vec<(&str, &str)> = vec![
        ("response_type", "code"),
        ("client_id", &client_id),
        ("redirect_uri", &redirect_uri),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", &state),
        ("resource", &resource),
    ];
    // the resource's own scopes (MCP spec); the server's full list only as a fallback
    let scope = if resource_scopes.is_empty() { meta["scopes_supported"].as_array().map(|s| s.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_default() } else { resource_scopes };
    if !scope.is_empty() {
        params.push(("scope", &scope));
    }
    let auth = meta["authorization_endpoint"].as_str().unwrap_or_default();
    open_url(&format!("{auth}{}{}", if auth.contains('?') { "&" } else { "?" }, form(&params)));

    let code = stenly_core::oauth::wait_for_code(&listener, &state, &id_label(id))?;
    let token_endpoint = meta["token_endpoint"].as_str().unwrap_or_default().to_string();
    let tok = post_form(&token_endpoint, &[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", &redirect_uri),
        ("client_id", &client_id),
        ("code_verifier", &verifier),
        ("resource", &resource),
    ])?;
    let t = Tokens {
        token_endpoint,
        client_id,
        resource,
        access_token: tok["access_token"].as_str().context("no access_token")?.to_string(),
        refresh_token: tok["refresh_token"].as_str().map(str::to_string),
        expires_at: chrono::Utc::now().timestamp() + tok["expires_in"].as_i64().unwrap_or(3600),
    };
    stenly_core::secrets::set_context_secret(&account(id), &serde_json::to_string(&t)?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn origin_and_form() {
        assert_eq!(super::origin("https://mcp.linear.app/mcp"), "https://mcp.linear.app");
    }
}
