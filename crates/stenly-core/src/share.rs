//! Private, end-to-end encrypted meeting-share links.
//!
//! The share service only receives an AES-GCM envelope. The 256-bit key stays
//! in the OS keychain and is appended as the URL fragment when it is copied.
use crate::destinations::{self, DestinationConfig, Payload};
use crate::session::{SessionMeta, ShareRecord};
use anyhow::{Context, Result};
use aes_gcm::{Aes256Gcm, KeyInit, aead::{Aead, Payload as AeadPayload}};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::Path;

const AAD: &[u8] = b"stenly-share-v1";
const VERSION: u8 = 1;

#[derive(Serialize)]
struct Plaintext<'a> {
    v: u8,
    title: &'a str,
    date: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_secs: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attendees: Vec<Person>,
    markdown: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    transcript: Option<Vec<Line>>,
    app: String,
}
#[derive(Serialize)] struct Person { name: String, email: String }
#[derive(Serialize)] struct Line { speaker: String, start: f64, text: String }

/// Build the v1 plaintext JSON from a session.
pub fn plaintext(payload: &Payload, include_transcript: bool) -> Result<Vec<u8>> {
    // who was there, not everyone invited
    let attendees = payload.meta.others().into_iter().map(|a| Person { name: a.name, email: a.email.unwrap_or_default() }).collect();
    let transcript: Option<Vec<Line>> = include_transcript.then(|| payload.transcript.segments.iter().map(|s| Line {
        speaker: crate::destinations::speaker_name(s), start: s.start, text: s.text.clone(),
    }).collect());
    let p = Plaintext {
        v: VERSION, title: &payload.meta.title, date: payload.meta.started.to_rfc3339(),
        duration_secs: payload.meta.ended.map(|e| (e - payload.meta.started).num_seconds().max(0)),
        attendees, markdown: payload.body_markdown(), transcript,
        app: format!("stenly {}", env!("CARGO_PKG_VERSION")),
    };
    Ok(serde_json::to_vec(&p)?)
}

/// Encrypt plaintext into `[version | nonce | ciphertext+tag]`.
pub fn encrypt(key: &[u8; 32], nonce: &[u8; 12], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("AES-256 key length");
    let encrypted = cipher.encrypt(nonce.into(), AeadPayload { msg: plaintext, aad: AAD })
        .map_err(|_| anyhow::anyhow!("couldn't encrypt share"))?;
    let mut envelope = Vec::with_capacity(13 + encrypted.len());
    envelope.push(VERSION); envelope.extend_from_slice(nonce); envelope.extend_from_slice(&encrypted);
    Ok(envelope)
}
pub fn decrypt(key: &[u8; 32], envelope: &[u8]) -> Result<Vec<u8>> {
    anyhow::ensure!(envelope.len() >= 29 && envelope[0] == VERSION, "unsupported encrypted share");
    let cipher = Aes256Gcm::new_from_slice(key).expect("AES-256 key length");
    cipher.decrypt((&envelope[1..13]).into(), AeadPayload { msg: &envelope[13..], aad: AAD })
        .map_err(|_| anyhow::anyhow!("couldn't decrypt share"))
}
fn random<const N: usize>() -> Result<[u8; N]> { let mut b = [0; N]; getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("getting secure random bytes: {e}"))?; Ok(b) }
fn key_account(secret_id: &str, what: &str) -> String { format!("share.{secret_id}.{what}") }
fn secret(secret_id: &str, what: &str) -> Option<String> { crate::secrets::destination_secret(&key_account(secret_id, what)) }
fn save_secret(secret_id: &str, what: &str, value: &str) -> Result<()> { crate::secrets::set_destination_secret(&key_account(secret_id, what), value) }
fn random_id() -> Result<String> { Ok(URL_SAFE_NO_PAD.encode(random::<16>()?)) }

fn base(d: &DestinationConfig) -> Result<String> {
    let u = d.share.server_url.trim().trim_end_matches('/');
    anyhow::ensure!(u.starts_with("https://") || u.starts_with("http://127.0.0.1") || u.starts_with("http://localhost"), "the share server URL should start with https://");
    Ok(u.to_string())
}
fn error(r: &crate::destinations::http::Response, action: &str) -> anyhow::Error {
    let parsed = r.json();
    let message = parsed.get("message").and_then(Value::as_str).unwrap_or(&r.text);
    anyhow::anyhow!("share server couldn't {action} ({}): {}", r.status, crate::destinations::http::snippet(message))
}
fn request(d: &DestinationConfig, method: &str, url: &str, owner: Option<&str>, body: Option<&Value>) -> Result<crate::destinations::http::Response> {
    let mut headers = Vec::new();
    if let Some(token) = destinations::secret_for(d, "") { headers.push(("Authorization", format!("Bearer {token}"))); }
    if let Some(token) = owner { headers.push(("X-Stenly-Owner-Token", token.to_string())); }
    let refs: Vec<(&str, &str)> = headers.iter().map(|(k,v)| (*k, v.as_str())).collect();
    crate::destinations::http::request(method, url, &refs, body, "the share server")
}

/// Upload or replace a share. A re-share retains its generated key and server id.
pub fn upload(d: &DestinationConfig, payload: &Payload) -> Result<ShareRecord> {
    let previous = payload.meta.shares.iter().find(|s| s.destination == d.id).cloned();
    // An expired local record is necessarily no longer usable. Forget it before
    // uploading, so Share always recovers to a new link instead of getting stuck.
    if previous.as_ref().and_then(|s| s.expires_at).is_some_and(|at| at <= Utc::now()) {
        forget_local(&payload.dir, previous.as_ref().expect("checked above"))?;
        return upload_fresh(d, payload);
    }
    let Some(old) = previous else { return upload_fresh(d, payload) };
    let Some(key_b64) = secret(&old.secret_id, "key") else {
        forget_local(&payload.dir, &old)?;
        return upload_fresh(d, payload);
    };
    let Some(owner) = secret(&old.secret_id, "owner") else {
        forget_local(&payload.dir, &old)?;
        return upload_fresh(d, payload);
    };
    let key = decode_key(&key_b64)?;
    let body = upload_body(d, payload, &key)?;
    let response = request(d, "PUT", &format!("{}/api/v1/shares/{}", base(d)?, old.id), Some(&owner), Some(&body))?;
    // A server may discard expired shares or be restarted. The old URL cannot
    // be recovered, so create a new share/key rather than trapping the meeting.
    if matches!(response.status, 404 | 410) {
        forget_local(&payload.dir, &old)?;
        return upload_fresh(d, payload);
    }
    if response.status != 200 { return Err(error(&response, "save this share")); }
    record_response(d, &payload.dir, &old.secret_id, &key_b64, &owner, response)
}

fn upload_fresh(d: &DestinationConfig, payload: &Payload) -> Result<ShareRecord> {
    let secret_id = random_id()?;
    let key_b64 = URL_SAFE_NO_PAD.encode(random::<32>()?);
    // Fail before POST if the Keychain cannot keep the sole decryption key.
    save_secret(&secret_id, "key", &key_b64)?;
    let key = decode_key(&key_b64)?;
    let response = request(d, "POST", &format!("{}/api/v1/shares", base(d)?), None, Some(&upload_body(d, payload, &key)?))?;
    if response.status != 201 { save_secret(&secret_id, "key", "")?; return Err(error(&response, "save this share")); }
    let owner = response.json()["owner_token"].as_str().map(str::to_string).context("share server returned no owner token")?;
    if let Err(e) = save_secret(&secret_id, "owner", &owner) {
        let response_json = response.json();
        let id = response_json["id"].as_str().unwrap_or_default();
        let _ = request(d, "DELETE", &format!("{}/api/v1/shares/{id}", base(d)?), Some(&owner), None);
        let _ = save_secret(&secret_id, "key", "");
        return Err(e);
    }
    record_response(d, &payload.dir, &secret_id, &key_b64, &owner, response)
}

fn decode_key(key_b64: &str) -> Result<[u8; 32]> {
    URL_SAFE_NO_PAD.decode(key_b64).context("stored share key is corrupt")?.try_into().map_err(|_| anyhow::anyhow!("stored share key has wrong length"))
}
fn upload_body(d: &DestinationConfig, payload: &Payload, key: &[u8; 32]) -> Result<Value> {
    let nonce = random::<12>()?;
    let mut body = json!({"ciphertext": URL_SAFE_NO_PAD.encode(encrypt(key, &nonce, &plaintext(payload, d.share.include_transcript)?)?), "expires_in_days": d.share.expiry_days});
    if d.share.plaintext_title { body["title"] = Value::String(payload.meta.title.clone()); }
    Ok(body)
}
fn record_response(d: &DestinationConfig, _dir: &Path, secret_id: &str, key_b64: &str, existing_owner: &str, response: crate::destinations::http::Response) -> Result<ShareRecord> {
    let v = response.json();
    let id = v["id"].as_str().context("share server returned no id")?.to_string();
    let share_url = v["url"].as_str().context("share server returned no url")?.to_string();
    let owner = v["owner_token"].as_str().unwrap_or(existing_owner);
    save_secret(secret_id, "key", key_b64)?;
    save_secret(secret_id, "owner", owner)?;
    let expires_at = v["expires_at"].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|d| d.with_timezone(&Utc));
    Ok(ShareRecord { destination: d.id.clone(), secret_id: secret_id.to_string(), id, url: share_url, expires_at })
}
fn forget_local(dir: &Path, record: &ShareRecord) -> Result<()> {
    SessionMeta::remove_share(dir, &record.destination)?;
    save_secret(&record.secret_id, "key", "")?;
    save_secret(&record.secret_id, "owner", "")?;
    Ok(())
}

pub fn public_link(_dir: &Path, record: &ShareRecord) -> Result<String> {
    let key = secret(&record.secret_id, "key").context("share key is missing from the Keychain")?;
    Ok(format!("{}#{}", record.url, key))
}
pub fn record(dir: &Path, record: ShareRecord) -> Result<()> {
    SessionMeta::replace_share(dir, record)
}
pub fn link_for(dir: &Path, destination: Option<&str>) -> Result<Option<String>> {
    let meta = SessionMeta::load(dir)?;
    let record = match destination { Some(id) => meta.shares.iter().find(|s| s.destination == id), None => meta.shares.first() };
    record.map(|s| public_link(dir, s)).transpose()
}
pub fn revoke(d: &DestinationConfig, dir: &Path) -> Result<()> {
    let meta = SessionMeta::load(dir)?;
    let record = meta.shares.iter().find(|s| s.destination == d.id).context("this meeting has no active share link")?.clone();
    let owner = secret(&record.secret_id, "owner");
    let response = match owner {
        Some(owner) => request(d, "DELETE", &format!("{}/api/v1/shares/{}", base(d)?, record.id), Some(&owner), None)?,
        // The local link cannot be used without its owner token, so still let
        // Revoke repair the local state even if the Keychain was reset.
        None => { forget_local(dir, &record)?; return Ok(()) }
    };
    if !matches!(response.status, 204 | 404 | 410) { return Err(error(&response, "revoke this share")); }
    // 404/410 mean it is already absent/revoked at the server; the safe local
    // outcome is identical to a successful DELETE.
    forget_local(dir, &record)
}
pub fn test(d: &DestinationConfig) -> Result<String> {
    let r = request(d, "GET", &format!("{}/api/v1/info", base(d)?), None, None)?;
    if r.status == 200 { Ok(format!("Connected to {}", r.json()["name"].as_str().unwrap_or("share server"))) } else { Err(error(&r, "connect")) }
}

/// The deliberately user-triggered message for attendees. It gives a short
/// readable digest plus the private link; nothing is ever sent automatically.
pub fn attendee_message(payload: &Payload, link: &str) -> String {
    let mut out = format!("Notes from {}\n\n", payload.meta.title);
    if let Some(summary) = &payload.summary {
        if !summary.summary.trim().is_empty() { out.push_str(summary.summary.trim()); out.push_str("\n\n"); }
        if !summary.action_items.is_empty() {
            out.push_str("Action items\n");
            for a in &summary.action_items {
                let owner = a.owner.as_deref().filter(|x| !x.is_empty()).map(|x| format!("{x}: ")).unwrap_or_default();
                out.push_str(&format!("- {owner}{}\n", a.task));
            }
            out.push('\n');
        }
    }
    out.push_str("Private meeting notes: "); out.push_str(link);
    out
}

/// RFC 3986 component encoding, suitable for a mailto query parameter.
pub fn percent_encode(s: &str) -> String {
    s.bytes().map(|b| match b { b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(), _ => format!("%{b:02X}") }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contract_vector() {
        let k: [u8; 32] = (0u8..32).collect::<Vec<_>>().try_into().unwrap();
        let n = [0xa0,0xa1,0xa2,0xa3,0xa4,0xa5,0xa6,0xa7,0xa8,0xa9,0xaa,0xab];
        let p = br##"{"v":1,"title":"Test standup","markdown":"# Test standup\n\n- Ship it"}"##;
        let got = URL_SAFE_NO_PAD.encode(encrypt(&k, &n, p).unwrap());
        assert_eq!(got, "AaChoqOkpaanqKmqq506Cg9_-i6dFgzzv2JY-vwkySpkssQ2DfJqU_Zdh1dsswQsm8BVPR9lvifoXR_wjWdoMikMtG8OHTBXMIlQ1tjdzKUGRIeZOWidVgcd55Jbwe3HNthABw");
        assert_eq!(decrypt(&k, &URL_SAFE_NO_PAD.decode(got).unwrap()).unwrap(), p);
    }
    #[test]
    fn plaintext_includes_requested_transcript() {
        let p = crate::destinations::testutil::payload();
        let value: Value = serde_json::from_slice(&plaintext(&p, true).unwrap()).unwrap();
        assert_eq!(value["transcript"][0]["text"], "Hello");
        assert_eq!(value["transcript"][0]["speaker"], "You");
    }

    #[test]
    fn attendees_are_who_came_once_known() {
        let mut p = crate::destinations::testutil::payload();
        p.meta = crate::destinations::testutil::meta_with_attendees("me@example.com", &["me@example.com", "andy@example.org", "sam@example.org"]);
        let names = |p: &Payload| {
            let v: Value = serde_json::from_slice(&plaintext(p, false).unwrap()).unwrap();
            v["attendees"].as_array().map(|a| a.iter().map(|x| x["email"].as_str().unwrap().to_string()).collect::<Vec<_>>()).unwrap_or_default()
        };
        // before summarizing: the invite, without the user
        assert_eq!(names(&p), vec!["andy@example.org", "sam@example.org"]);
        p.meta.attended = Some(vec![crate::session::Attended { name: "Andy".into(), email: Some("andy@example.org".into()) }]);
        assert_eq!(names(&p), vec!["andy@example.org"]);
    }
}
