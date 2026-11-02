//! A mock Google OAuth + Calendar server on 127.0.0.1, for tests and for
//! driving the app without a Google account (`cargo run -p stenly-core
//! --example mock-google`). Synthetic data only: example.com identities and
//! placeholder meeting ids.
//!
//! It enforces what the real servers do that matters here: PKCE (S256)
//! verification, the exact redirect URI, the client secret, bearer tokens that
//! expire or are revoked (401), refresh tokens that are revoked
//! (`invalid_grant`), paging, and 404 for unreadable calendars.
#![allow(dead_code)]

use serde_json::{Value, json};
use sha2::Digest;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

pub const CLIENT_ID: &str = "mock-client-id.apps.example.com";
pub const CLIENT_SECRET: &str = "mock-client-secret";

#[derive(Default)]
pub struct State {
    /// Accounts handed out, in order, by successive sign-ins (cycling).
    pub accounts: Vec<String>,
    pub next_account: usize,
    /// auth code → (account, code_challenge, redirect_uri, scope)
    codes: HashMap<String, (String, String, String, String)>,
    /// access token → account
    access: HashMap<String, String>,
    /// refresh token → account
    refresh: HashMap<String, String>,
    /// account → calendars (Google calendarList items)
    pub calendars: HashMap<String, Vec<Value>>,
    /// (account, calendar id) → events (Google event resources)
    pub events: HashMap<(String, String), Vec<Value>>,
    /// Seconds an access token lives.
    pub expires_in: i64,
    /// Calendars whose events answer 404.
    pub unreadable: Vec<String>,
    /// Scope granted to the app regardless of what it asked for (None = as asked).
    pub grant_scope: Option<String>,
    pub counter: u64,
    // what the tests assert on
    pub token_requests: Vec<String>,
    pub auth_requests: Vec<HashMap<String, String>>,
    pub api_requests: Vec<String>,
    pub revoked: Vec<String>,
    pub page_size: usize,
}

pub struct Mock {
    pub base: String,
    pub state: Arc<Mutex<State>>,
}

impl Mock {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State {
            expires_in: 3600,
            page_size: 250,
            ..Default::default()
        }));
        let st = state.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let st = st.clone();
                std::thread::spawn(move || {
                    let _ = handle(conn, &st);
                });
            }
        });
        Mock { base, state }
    }

    pub fn with(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.lock().unwrap());
    }

    /// Two example accounts with calendars and events around `now`.
    pub fn seed(&self, now: chrono::DateTime<chrono::Utc>) {
        self.with(|s| {
            s.accounts = vec!["avery@example.com".into(), "avery.work@example.org".into()];
            for (acct, extra) in [("avery@example.com", "family@group.calendar.example.com"), ("avery.work@example.org", "team@group.calendar.example.com")] {
                s.calendars.insert(
                    acct.into(),
                    vec![
                        json!({"id": acct, "summary": acct, "primary": true, "backgroundColor": "#039be5", "accessRole": "owner"}),
                        json!({"id": extra, "summary": if extra.starts_with("family") { "Family" } else { "Team" }, "summaryOverride": null, "backgroundColor": if extra.starts_with("family") { "#33b679" } else { "#f6bf26" }, "accessRole": "reader"}),
                        json!({"id": format!("busy-{extra}"), "summary": "Someone else (free/busy)", "accessRole": "freeBusyReader"}),
                    ],
                );
            }
            let at = |mins: i64| (now + chrono::Duration::minutes(mins)).to_rfc3339();
            let mut put = |acct: &str, cal: &str, e: Value| s.events.entry((acct.into(), cal.into())).or_default().push(e);
            put(
                "avery@example.com",
                "avery@example.com",
                json!({"id": "g1", "iCalUID": "g1@google.com", "status": "confirmed", "summary": "Design review",
                    "start": {"dateTime": at(30)}, "end": {"dateTime": at(60)},
                    "hangoutLink": "https://meet.google.com/abc-defg-hij",
                    "conferenceData": {"entryPoints": [{"entryPointType": "video", "uri": "https://meet.google.com/abc-defg-hij"}, {"entryPointType": "phone", "uri": "tel:+15550100"}]},
                    "description": "Agenda: <b>mockups</b>", "htmlLink": "https://calendar.google.com/calendar/event?eid=g1",
                    "organizer": {"email": "sam@example.com", "displayName": "Sam Example"},
                    "attendees": [
                        {"email": "sam@example.com", "displayName": "Sam Example", "responseStatus": "accepted", "organizer": true},
                        {"email": "avery@example.com", "self": true, "responseStatus": "accepted"},
                        {"email": "room-2@resource.calendar.google.com", "resource": true, "responseStatus": "accepted"}]}),
            );
            put(
                "avery@example.com",
                "avery@example.com",
                json!({"id": "g2", "iCalUID": "g2@google.com", "status": "confirmed", "summary": "Vendor call (Zoom)",
                    "start": {"dateTime": at(180)}, "end": {"dateTime": at(210)},
                    "location": "https://us02web.zoom.us/j/81234567890?pwd=placeholder",
                    "attendees": [{"email": "pat@example.net", "displayName": "Pat Placeholder", "responseStatus": "needsAction"}, {"email": "avery@example.com", "self": true, "responseStatus": "accepted"}]}),
            );
            put(
                "avery@example.com",
                "family@group.calendar.example.com",
                json!({"id": "g3", "iCalUID": "g3@google.com", "status": "confirmed", "summary": "Family dinner planning",
                    "start": {"dateTime": at(300)}, "end": {"dateTime": at(330)}, "description": "https://teams.microsoft.com/l/meetup-join/19%3ameeting_placeholder%40thread.v2/0"}),
            );
            put(
                "avery.work@example.org",
                "avery.work@example.org",
                json!({"id": "w1", "iCalUID": "w1@google.com", "status": "confirmed", "summary": "Weekly planning",
                    "start": {"dateTime": at(90)}, "end": {"dateTime": at(120)}, "recurringEventId": "w1-series",
                    "conferenceData": {"entryPoints": [{"entryPointType": "video", "uri": "https://example.zoom.us/j/5550001111"}]},
                    "attendees": [{"email": "avery.work@example.org", "self": true, "responseStatus": "accepted"}, {"email": "jordan@example.org", "displayName": "Jordan Example", "responseStatus": "accepted"}]}),
            );
            put(
                "avery.work@example.org",
                "avery.work@example.org",
                json!({"id": "w2", "iCalUID": "w2@google.com", "status": "confirmed", "summary": "Declined offsite",
                    "start": {"dateTime": at(240)}, "end": {"dateTime": at(270)}, "hangoutLink": "https://meet.google.com/zzz-zzzz-zzz",
                    "attendees": [{"email": "avery.work@example.org", "self": true, "responseStatus": "declined"}, {"email": "jordan@example.org"}]}),
            );
            put(
                "avery.work@example.org",
                "avery.work@example.org",
                json!({"id": "w3", "iCalUID": "w3@google.com", "status": "confirmed", "summary": "All-day offsite", "start": {"date": "2026-11-03"}, "end": {"date": "2026-11-04"}}),
            );
            put(
                "avery.work@example.org",
                "team@group.calendar.example.com",
                json!({"id": "w4", "iCalUID": "w4@google.com", "status": "confirmed", "summary": "Team retro",
                    "start": {"dateTime": at(400)}, "end": {"dateTime": at(430)}, "hangoutLink": "https://meet.google.com/qqq-rrrr-sss",
                    "attendees": [{"email": "jordan@example.org"}]}),
            );
        });
    }

    /// Forget every access token, as if they had all expired or been revoked.
    pub fn expire_access_tokens(&self) {
        self.with(|s| s.access.clear());
    }

    /// Revoke every refresh token (the user removed access in their Google account).
    pub fn revoke_refresh_tokens(&self) {
        self.with(|s| {
            s.refresh.clear();
            s.access.clear();
        });
    }
}

// ------------------------------------------------------------------- HTTP

struct Req {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: String,
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'?'));
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| p.split_once('=').unwrap_or((p, "")))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect()
}

fn read_req(stream: &mut TcpStream) -> Option<Req> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next()?.split_whitespace();
    let (method, target) = (first.next()?.to_string(), first.next()?.to_string());
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body =
        String::from_utf8_lossy(&buf[head_end..(head_end + len).min(buf.len())]).into_owned();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    Some(Req {
        method,
        path: decode(path),
        query: parse_query(query),
        headers,
        body,
    })
}

fn respond(stream: &mut TcpStream, status: u16, extra: &str, body: &str) {
    let reason = match status {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n{body}",
        body.len()
    );
}

fn json_resp(stream: &mut TcpStream, status: u16, v: Value) {
    respond(stream, status, "", &v.to_string());
}

fn handle(mut stream: TcpStream, st: &Arc<Mutex<State>>) -> Option<()> {
    let req = read_req(&mut stream)?;
    let mut s = st.lock().unwrap();
    let p = req.path.as_str();
    match (req.method.as_str(), p) {
        ("GET", "/o/oauth2/v2/auth") => {
            let q = &req.query;
            s.auth_requests.push(q.clone());
            let ok = q.get("client_id").map(String::as_str) == Some(CLIENT_ID)
                && q.get("response_type").map(String::as_str) == Some("code")
                && q.get("code_challenge_method").map(String::as_str) == Some("S256")
                && q.get("redirect_uri").is_some_and(|r| {
                    r.starts_with("http://127.0.0.1:") && r.ends_with("/callback")
                })
                && q.get("state").is_some_and(|v| !v.is_empty())
                && q.get("code_challenge").is_some_and(|v| v.len() >= 43);
            if !ok {
                json_resp(&mut stream, 400, json!({"error": "invalid_request"}));
                return Some(());
            }
            let redirect = q["redirect_uri"].clone();
            let state = q["state"].clone();
            if s.accounts.is_empty() {
                s.accounts.push("avery@example.com".into());
            }
            if q.get("deny").is_some() || s.accounts[0] == "!deny" {
                let loc = format!("{redirect}?error=access_denied&state={}", urlenc(&state));
                respond(&mut stream, 302, &format!("location: {loc}\r\n"), "");
                return Some(());
            }
            let acct = s.accounts[s.next_account % s.accounts.len()].clone();
            s.next_account += 1;
            s.counter += 1;
            let code = format!("code-{}", s.counter);
            let scope = q.get("scope").cloned().unwrap_or_default();
            s.codes.insert(
                code.clone(),
                (acct, q["code_challenge"].clone(), redirect.clone(), scope),
            );
            let loc = format!("{redirect}?code={code}&state={}", urlenc(&state));
            respond(&mut stream, 302, &format!("location: {loc}\r\n"), "");
        }
        ("POST", "/token") => {
            let f = parse_query(&req.body);
            s.token_requests
                .push(f.get("grant_type").cloned().unwrap_or_default());
            if f.get("client_id").map(String::as_str) != Some(CLIENT_ID)
                || f.get("client_secret").map(String::as_str) != Some(CLIENT_SECRET)
            {
                json_resp(&mut stream, 401, json!({"error": "invalid_client"}));
                return Some(());
            }
            let expires_in = s.expires_in;
            match f.get("grant_type").map(String::as_str) {
                Some("authorization_code") => {
                    let Some((acct, challenge, redirect, scope)) =
                        f.get("code").and_then(|c| s.codes.remove(c))
                    else {
                        json_resp(
                            &mut stream,
                            400,
                            json!({"error": "invalid_grant", "error_description": "Bad Request"}),
                        );
                        return Some(());
                    };
                    let verifier = f.get("code_verifier").cloned().unwrap_or_default();
                    let computed =
                        stenly_core::oauth::b64url(&sha2::Sha256::digest(verifier.as_bytes()));
                    if computed != challenge || f.get("redirect_uri") != Some(&redirect) {
                        json_resp(
                            &mut stream,
                            400,
                            json!({"error": "invalid_grant", "error_description": "Bad Request"}),
                        );
                        return Some(());
                    }
                    s.counter += 1;
                    let (at, rt) = (format!("at-{}", s.counter), format!("rt-{}", s.counter));
                    s.access.insert(at.clone(), acct.clone());
                    s.refresh.insert(rt.clone(), acct);
                    let scope = s.grant_scope.clone().unwrap_or(scope);
                    json_resp(
                        &mut stream,
                        200,
                        json!({"access_token": at, "refresh_token": rt, "expires_in": expires_in, "scope": scope, "token_type": "Bearer"}),
                    );
                }
                Some("refresh_token") => {
                    let Some(acct) = f
                        .get("refresh_token")
                        .and_then(|t| s.refresh.get(t))
                        .cloned()
                    else {
                        json_resp(
                            &mut stream,
                            400,
                            json!({"error": "invalid_grant", "error_description": "Token has been expired or revoked."}),
                        );
                        return Some(());
                    };
                    s.counter += 1;
                    let at = format!("at-{}", s.counter);
                    s.access.insert(at.clone(), acct);
                    // Google doesn't return a new refresh token on refresh.
                    json_resp(
                        &mut stream,
                        200,
                        json!({"access_token": at, "expires_in": expires_in, "scope": "https://www.googleapis.com/auth/calendar.readonly", "token_type": "Bearer"}),
                    );
                }
                _ => json_resp(&mut stream, 400, json!({"error": "unsupported_grant_type"})),
            }
        }
        ("POST", "/revoke") => {
            let f = parse_query(&req.body);
            if let Some(t) = f.get("token") {
                s.revoked.push(t.clone());
                s.refresh.remove(t);
            }
            json_resp(&mut stream, 200, json!({}));
        }
        ("GET", path) if path.starts_with("/calendar/v3/") => {
            s.api_requests.push(format!(
                "{path}?{}",
                req.headers
                    .get("authorization")
                    .map(|_| "auth")
                    .unwrap_or("noauth")
            ));
            let token = req
                .headers
                .get("authorization")
                .and_then(|h| h.strip_prefix("Bearer "))
                .unwrap_or("");
            let Some(acct) = s.access.get(token).cloned() else {
                json_resp(
                    &mut stream,
                    401,
                    json!({"error": {"code": 401, "message": "Invalid Credentials", "status": "UNAUTHENTICATED"}}),
                );
                return Some(());
            };
            let rest = &path["/calendar/v3".len()..];
            if rest == "/users/me/calendarList" {
                let items = s.calendars.get(&acct).cloned().unwrap_or_default();
                let skip: usize = req
                    .query
                    .get("pageToken")
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(0);
                let page: Vec<Value> = items.iter().skip(skip).take(s.page_size).cloned().collect();
                let mut body = json!({"items": page});
                if skip + s.page_size < items.len() {
                    body["nextPageToken"] = (skip + s.page_size).to_string().into();
                }
                json_resp(&mut stream, 200, body);
            } else if let Some(cal) = rest
                .strip_prefix("/calendars/")
                .and_then(|r| r.strip_suffix("/events"))
            {
                if s.unreadable.iter().any(|u| u == cal) {
                    json_resp(
                        &mut stream,
                        404,
                        json!({"error": {"code": 404, "message": "Not Found"}}),
                    );
                    return Some(());
                }
                let parse = |k: &str| {
                    req.query
                        .get(k)
                        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
                };
                let (min, max) = (parse("timeMin"), parse("timeMax"));
                let all = s
                    .events
                    .get(&(acct, cal.to_string()))
                    .cloned()
                    .unwrap_or_default();
                let items: Vec<Value> = all
                    .into_iter()
                    .filter(|e| {
                        let Some(start) = e["start"]["dateTime"]
                            .as_str()
                            .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
                        else {
                            return true;
                        };
                        min.is_none_or(|m| start >= m) && max.is_none_or(|m| start < m)
                    })
                    .collect();
                let skip: usize = req
                    .query
                    .get("pageToken")
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(0);
                let page: Vec<Value> = items.iter().skip(skip).take(s.page_size).cloned().collect();
                let mut body = json!({"items": page});
                if skip + s.page_size < items.len() {
                    body["nextPageToken"] = (skip + s.page_size).to_string().into();
                }
                json_resp(&mut stream, 200, body);
            } else {
                json_resp(
                    &mut stream,
                    404,
                    json!({"error": {"code": 404, "message": "Not Found"}}),
                );
            }
        }
        _ => json_resp(&mut stream, 404, json!({"error": "not_found"})),
    }
    Some(())
}

fn urlenc(s: &str) -> String {
    stenly_core::oauth::urlenc(s)
}
