//! Kody connector: stenly as an MCP client of a Kody server (OAuth 2.1 + PKCE,
//! dynamic client registration). Calendar data is fetched by running a small
//! Code Mode snippet through Kody's `execute` tool against the user's existing
//! `google` integration, so stenly never holds Google credentials.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KodyAuth {
    pub base_url: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds.
    pub expires_at: i64,
}

pub fn auth_path() -> PathBuf {
    crate::config::data_dir().join("kody-auth.json")
}

impl KodyAuth {
    pub fn load() -> Option<Self> {
        let text = std::fs::read_to_string(auth_path()).ok()?;
        serde_json::from_str(&text).ok()
    }
    pub fn save(&self) -> Result<()> {
        let p = auth_path();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, serde_json::to_vec_pretty(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
    pub fn delete() {
        let _ = std::fs::remove_file(auth_path());
    }
}

use crate::oauth::{form, same_scheme};

#[derive(Deserialize)]
struct AsMeta {
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
}

#[derive(Deserialize)]
struct TokenResp {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

fn normalize_base(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    let u = u.strip_suffix("/mcp").unwrap_or(u);
    if u.starts_with("http") { u.to_string() } else { format!("https://{u}") }
}

/// Every Kody request gets a bounded connect + total timeout so a dead or
/// wedged server can never hang a worker forever.
fn agent() -> &'static ureq::Agent {
    static A: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    A.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_connect(Some(std::time::Duration::from_secs(5)))
            .timeout_global(Some(std::time::Duration::from_secs(20)))
            .build()
            .into()
    })
}

fn discover(base: &str) -> Result<AsMeta> {
    let mut meta: AsMeta = agent()
        .get(&format!("{base}/.well-known/oauth-authorization-server"))
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .call()
        .with_context(|| format!("couldn't reach Kody at {base} (is it running / are you on the tailnet?)"))?
        .body_mut()
        .read_json()
        .context("not a Kody server (no OAuth metadata)")?;
    // Behind a TLS-terminating proxy (Tailscale Funnel / Serve) Kody advertises
    // `http://` endpoints. POSTing there gets a 301 to https, which HTTP clients
    // follow as a GET → 405, so every token refresh failed. Use the scheme we
    // actually reached the server on.
    meta.authorization_endpoint = same_scheme(base, &meta.authorization_endpoint);
    meta.token_endpoint = same_scheme(base, &meta.token_endpoint);
    meta.registration_endpoint = meta.registration_endpoint.map(|u| same_scheme(base, &u));
    Ok(meta)
}

/// Interactive sign-in. `open_url` is called with the authorize URL (the app opens
/// the browser); blocks until the browser redirects back to our loopback listener.
pub fn sign_in(base_url: &str, open_url: &dyn Fn(&str)) -> Result<KodyAuth> {
    let base = normalize_base(base_url);
    let meta = discover(&base)?;
    let (listener, redirect_uri) = crate::oauth::loopback()?;
    let reg_url = meta.registration_endpoint.context("server doesn't support client registration")?;
    let client_id = crate::oauth::register_client(agent(), &reg_url, "stenly", &redirect_uri)?;
    let crate::oauth::Pkce { verifier, challenge, state } = crate::oauth::pkce();
    let resource = format!("{base}/mcp");
    let auth_url = format!(
        "{}?{}",
        meta.authorization_endpoint,
        form(&[
            ("response_type", "code"),
            ("client_id", &client_id),
            ("redirect_uri", &redirect_uri),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("state", &state),
            ("scope", "openid profile email"),
            ("resource", &resource),
        ])
    );
    open_url(&auth_url);
    let code = crate::oauth::wait_for_code(&listener, &state, "Kody")?;

    let tok: TokenResp = agent()
        .post(&meta.token_endpoint)
        .content_type("application/x-www-form-urlencoded")
        .send(form(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &redirect_uri),
            ("client_id", &client_id),
            ("code_verifier", &verifier),
            ("resource", &resource),
        ]))?
        .body_mut()
        .read_json()
        .context("token exchange failed")?;
    let auth = KodyAuth {
        base_url: base,
        client_id,
        redirect_uri,
        access_token: tok.access_token,
        refresh_token: tok.refresh_token,
        expires_at: chrono::Utc::now().timestamp() + tok.expires_in.unwrap_or(3600),
    };
    auth.save()?;
    Ok(auth)
}

fn refresh(auth: &mut KodyAuth) -> Result<()> {
    let rt = auth.refresh_token.clone().context("session expired; sign in again")?;
    let meta = discover(&auth.base_url)?;
    let tok: TokenResp = agent()
        .post(&meta.token_endpoint)
        .content_type("application/x-www-form-urlencoded")
        .send(form(&[("grant_type", "refresh_token"), ("refresh_token", &rt), ("client_id", &auth.client_id), ("resource", &format!("{}/mcp", auth.base_url))]))
        .context("refreshing Kody session (sign in again if this persists)")?
        .body_mut()
        .read_json()?;
    auth.access_token = tok.access_token;
    if tok.refresh_token.is_some() {
        auth.refresh_token = tok.refresh_token;
    }
    auth.expires_at = chrono::Utc::now().timestamp() + tok.expires_in.unwrap_or(3600);
    auth.save()
}

/// A current access token for the saved Kody sign-in (refreshed when it is
/// about to expire, or always with `force`). Lets other MCP clients (the
/// context agent) reuse the Calendar connection.
pub fn access_token(force: bool) -> Result<(String, String)> {
    let mut auth = KodyAuth::load().context("not signed in to Kody")?;
    if force || chrono::Utc::now().timestamp() > auth.expires_at - 120 {
        refresh(&mut auth)?;
    }
    Ok((format!("{}/mcp", auth.base_url), auth.access_token))
}

/// Minimal streamable-HTTP MCP client for one `tools/call`.
pub struct KodyClient {
    auth: KodyAuth,
}

impl KodyClient {
    pub fn from_saved() -> Option<Self> {
        KodyAuth::load().map(|auth| Self { auth })
    }

    pub fn base_url(&self) -> &str {
        &self.auth.base_url
    }

    fn ensure_fresh(&mut self) -> Result<()> {
        if chrono::Utc::now().timestamp() > self.auth.expires_at - 120 {
            refresh(&mut self.auth)?;
        }
        Ok(())
    }

    fn rpc(&self, session: Option<&str>, body: Value) -> Result<(Option<String>, Option<Value>)> {
        let url = format!("{}/mcp", self.auth.base_url);
        let mut req = agent()
            .post(&url)
            .header("authorization", &format!("Bearer {}", self.auth.access_token))
            .header("accept", "application/json, text/event-stream")
            .config()
            .http_status_as_error(false)
            .timeout_connect(Some(std::time::Duration::from_secs(8)))
            .timeout_global(Some(std::time::Duration::from_secs(45)))
            .build();
        if let Some(s) = session {
            req = req.header("mcp-session-id", s);
        }
        let mut resp = req.send_json(&body).map_err(|e| match e {
            ureq::Error::Timeout(_) | ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => {
                anyhow::anyhow!("can't reach Kody at {} (is it running, and are you on the tailnet?)", self.auth.base_url)
            }
            e => e.into(),
        })?;
        let status = resp.status().as_u16();
        if status == 401 {
            bail!("unauthorized");
        }
        let sid = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()).map(str::to_string);
        let text = resp.body_mut().read_to_string()?;
        if !(200..300).contains(&status) {
            if (500..600).contains(&status) {
                bail!("Kody is having trouble right now (error {status}). Try again in a minute.");
            }
            bail!("Kody returned an error ({status}): {}", text.chars().take(200).collect::<String>());
        }
        // Either plain JSON or SSE with `data:` lines.
        let msg = if text.trim_start().starts_with('{') {
            serde_json::from_str(&text).ok()
        } else {
            text.lines().filter_map(|l| l.strip_prefix("data:")).filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok()).find(|v| v.get("id").is_some())
        };
        Ok((sid, msg))
    }

    /// Run a Code Mode module through Kody `execute` and return its `result`.
    pub fn execute(&mut self, code: &str, params: Value) -> Result<Value> {
        self.ensure_fresh()?;
        let call = |this: &Self| -> Result<Value> {
            let (sid, _) = this.rpc(
                None,
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"stenly","version":env!("CARGO_PKG_VERSION")}}}),
            )?;
            let sid = sid.as_deref();
            let _ = this.rpc(sid, json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
            let (_, msg) = this.rpc(
                sid,
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"execute","arguments":{"code":code,"params":params}}}),
            )?;
            let msg = msg.context("empty response from Kody")?;
            if let Some(err) = msg.get("error") {
                bail!("Kody error: {err}");
            }
            let result = &msg["result"];
            if result["isError"].as_bool() == Some(true) {
                let text = result["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join(" ");
                bail!("{}", text.trim());
            }
            Ok(result["structuredContent"]["result"].clone())
        };
        match call(self) {
            Err(e) if e.to_string() == "unauthorized" => {
                refresh(&mut self.auth)?;
                call(self)
            }
            r => r,
        }
    }
}

/// Code Mode module run inside Kody. Uses the user's `google` integration.
/// Reads every Google account connected in Kody (`google`, `google-work`, …),
/// so a personal and a Workspace account both show up. `integration` names the
/// preferred one; any other integration whose name starts with "google" is
/// included too. Calendars are keyed by id (unique across accounts).
const CALENDAR_JS: &str = r#"import { kody, createAuthenticatedFetch } from "kody:runtime"
export default async function main({ integration, timeMin, timeMax, calendarIds, listOnly }) {
  let names = [integration]
  try {
    const l = await kody.integrationList({})
    for (const i of l.integrations || []) if (/^google/.test(i.name) && !names.includes(i.name)) names.push(i.name)
  } catch (e) {}
  const accounts = []
  const calendars = []
  const accountErrors = []
  for (const name of names) {
    let f
    try { f = await createAuthenticatedFetch(name) } catch (e) { if (name === integration) throw e; accountErrors.push({ integration: name, error: String(e).slice(0, 200) }); continue }
    let r
    try { r = await f("https://www.googleapis.com/calendar/v3/users/me/calendarList?maxResults=250&fields=items(id,summary,summaryOverride,primary,backgroundColor,selected,accessRole)") }
    catch (e) { if (name === integration) throw e; accountErrors.push({ integration: name, error: String(e).slice(0, 200) }); continue }
    if (!r.ok) { if (name === integration) throw new Error(`Google ${r.status}: ${(await r.text()).slice(0,200)}`); accountErrors.push({ integration: name, status: r.status }); continue }
    const list = await r.json()
    const items = list.items || []
    const primary = items.find(c => c.primary)
    accounts.push({ integration: name, email: primary ? primary.id : null, f })
    // The same calendar is often visible from several accounts (your work
    // calendar added to your personal account, and vice versa). Read it through
    // the account with the most access: its owner first (an account's own
    // primary calendar), then writer, then reader. Google labels a Workspace
    // calendar seen from a personal account "reader" even when only free/busy
    // is actually shared, so ownership has to win.
    const rank = c => (c.primary ? 4 : 0) + ({ owner: 3, writer: 2, reader: 1, freeBusyReader: 0 }[c.accessRole] || 0)
    for (const c of items) {
      const entry = { id: c.id, name: c.summaryOverride || c.summary, primary: !!c.primary, color: c.backgroundColor || null, selected: !!c.selected, account: primary ? primary.id : name, integration: name, accessRole: c.accessRole, rank: rank(c) }
      const i = calendars.findIndex(x => x.id === c.id)
      if (i < 0) calendars.push(entry)
      else if (entry.rank > calendars[i].rank) calendars[i] = { ...entry, primary: calendars[i].primary || entry.primary }
    }
  }
  if (listOnly) return { calendars: calendars.map(({ accessRole, rank, ...c }) => c), events: [], accountErrors }
  const ids = calendarIds && calendarIds.length ? calendarIds : calendars.filter(c => c.primary && c.integration === integration).map(c => c.id)
  const events = []
  for (const id of ids) {
    const cal = calendars.find(c => c.id === id)
    const acct = accounts.find(a => a.integration === (cal ? cal.integration : integration)) || accounts[0]
    if (!acct) continue
    const q = new URLSearchParams({ timeMin, timeMax, singleEvents: "true", orderBy: "startTime", maxResults: "250",
      fields: "items(id,iCalUID,status,summary,description,location,start,end,hangoutLink,htmlLink,conferenceData(entryPoints(entryPointType,uri)),attendees(email,displayName,responseStatus,self,resource,organizer),organizer(email,displayName),recurringEventId,transparency,eventType)" })
    let r
    try { r = await acct.f(`https://www.googleapis.com/calendar/v3/calendars/${encodeURIComponent(id)}/events?${q}`) }
    catch (e) { events.push({ calendarId: id, calendarName: cal ? cal.name : id, error: String(e).slice(0, 200) }); continue }
    if (!r.ok) {
      events.push({ calendarId: id, calendarName: cal ? cal.name : id, status: r.status, error: `HTTP ${r.status}` })
      continue
    }
    const d = await r.json()
    for (const e of d.items || []) events.push({ ...e, calendarId: id, calendarName: cal ? cal.name : id, calendarColor: cal ? cal.color : null })
  }
  return { calendars: calendars.map(({ accessRole, rank, ...c }) => c), events, accountErrors }
}"#;

pub fn list_calendars(client: &mut KodyClient, integration: &str) -> Result<Vec<crate::calendar::CalendarInfo>> {
    let v = client.execute(CALENDAR_JS, json!({ "integration": integration, "listOnly": true }))?;
    Ok(serde_json::from_value(v["calendars"].clone()).context("unexpected calendar list")?)
}

/// Fetch events for the chosen calendars (empty = primary) and map to CalendarEvent.
pub fn fetch_events(
    client: &mut KodyClient,
    integration: &str,
    calendar_ids: &[String],
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) -> Result<(Vec<crate::calendar::CalendarEvent>, Vec<String>)> {
    let v = client.execute(
        CALENDAR_JS,
        json!({ "integration": integration, "timeMin": from.to_rfc3339(), "timeMax": to.to_rfc3339(), "calendarIds": calendar_ids }),
    )?;
    let mut errors = Vec::new();
    let mut out = Vec::new();
    for e in v["events"].as_array().into_iter().flatten() {
        if e.get("error").is_some() {
            let name = e["calendarName"].as_str().or(e["calendarId"].as_str()).unwrap_or("A calendar");
            errors.push(match e["status"].as_u64() {
                // Google answers 404 (not 403) for a calendar that's in your list but
                // whose events you can't read, e.g. a Workspace calendar shared with
                // your personal account at free/busy level, or blocked by the org.
                Some(404) => format!("Google won't share event details for “{name}” with this account. Sign in to Kody with that account, share the calendar with full details, or turn it off."),
                Some(401 | 403) => format!("Google won't share “{name}” with Kody. Reconnect, or turn it off."),
                Some(s) => format!("Google returned an error ({s}) for “{name}”."),
                None => format!("Couldn't load “{name}”."),
            });
            continue;
        }
        if let Some(ev) = map_google_event(e) {
            out.push(ev);
        }
    }
    Ok((out, errors))
}

fn parse_dt(v: &Value) -> Option<chrono::DateTime<chrono::Utc>> {
    v["dateTime"].as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.with_timezone(&chrono::Utc))
}

/// Google Calendar API event JSON → CalendarEvent. All-day / cancelled / OOO events are skipped.
pub fn map_google_event(e: &Value) -> Option<crate::calendar::CalendarEvent> {
    use crate::calendar::{Attendee, CalendarEvent};
    if e["status"].as_str() == Some("cancelled") {
        return None;
    }
    if matches!(e["eventType"].as_str(), Some("outOfOffice" | "focusTime" | "workingLocation" | "birthday")) {
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
        conf.iter().map(String::as_str).chain([hangout, location, description].into_iter().flatten()),
    );
    let attendees: Vec<Attendee> = e["attendees"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["resource"].as_bool() != Some(true))
        .map(|a| Attendee {
            name: a["displayName"].as_str().map(str::to_string),
            email: a["email"].as_str().unwrap_or_default().to_string(),
            status: a["responseStatus"].as_str().map(|s| match s {
                "accepted" => "ACCEPTED",
                "declined" => "DECLINED",
                "tentative" => "TENTATIVE",
                _ => "NEEDS-ACTION",
            }
            .to_string()),
        })
        .collect();
    let me = e["attendees"].as_array().into_iter().flatten().find(|a| a["self"].as_bool() == Some(true));
    let declined = me.is_some_and(|a| a["responseStatus"].as_str() == Some("declined"));
    let uid = e["iCalUID"].as_str().or(e["id"].as_str()).unwrap_or_default().to_string();
    Some(CalendarEvent {
        id: format!("{uid}@{}", start.timestamp()),
        uid,
        title: e["summary"].as_str().unwrap_or("Untitled meeting").to_string(),
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
        calendar: me.and_then(|a| a["email"].as_str()).map(str::to_string).or_else(|| e["calendarId"].as_str().map(str::to_string)),
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

    #[test]
    fn maps_google_json() {
        // Synthetic fixture: example.com identities and placeholder meeting IDs.
        let e = json!({
            "id": "x", "iCalUID": "x@google.com", "status": "confirmed", "summary": "Weekly Sync",
            "start": {"dateTime": "2026-10-23T16:15:00+09:00"}, "end": {"dateTime": "2026-10-23T16:45:00+09:00"},
            "location": "https://zoom.us/j/1111111111",
            "conferenceData": {"entryPoints": [{"entryPointType": "video", "uri": "https://zoom.us/j/2222222222"}]},
            "attendees": [{"email": "me@example.com", "self": true, "responseStatus": "accepted"}, {"email": "alex@example.com", "displayName": "Alex Example"}, {"email": "room-1@resource.calendar.google.com", "resource": true}],
            "recurringEventId": "r"
        });
        let ev = map_google_event(&e).unwrap();
        // conferenceData wins over the (also call-shaped) location.
        assert_eq!(ev.link.unwrap().url, "https://zoom.us/j/2222222222");
        assert_eq!(ev.attendees.len(), 2);
        assert!(ev.attendees.iter().all(|a| !a.email.contains("resource.calendar")));
        assert_eq!(ev.calendar.as_deref(), Some("me@example.com"));
        assert!(ev.recurring && !ev.declined);
        assert_eq!(ev.start.timestamp(), chrono::DateTime::parse_from_rfc3339("2026-10-23T07:15:00Z").unwrap().timestamp());
        assert!(map_google_event(&json!({"status":"confirmed","start":{"date":"2026-10-23"}})).is_none());
    }

    #[test]
    fn b64() {
        assert_eq!(crate::oauth::b64url(b"hello?"), "aGVsbG8_");
        assert_eq!(crate::oauth::b64url(&[0xff, 0xfe]), "__4");
    }
}
