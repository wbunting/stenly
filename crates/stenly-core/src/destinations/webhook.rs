//! Generic webhook: POST the meeting as JSON.
//!
//! Body (`stenly.meeting.v1`):
//! ```json
//! { "type": "stenly.meeting.v1", "id": "<session folder>", "title": "...",
//!   "started": "RFC 3339", "ended": "...", "duration_min": 31,
//!   "attendees": [{"name": "...", "email": "...", "external": true}],
//!   "meeting_url": "...", "calendar": "...", "tags": [...],
//!   "summary": {...}, "markdown": "...", "transcript": [{...}], "resend": false }
//! ```
//! If a signing secret is stored, `X-Stenly-Signature: sha256=<hex HMAC of
//! the body>` is added so receivers can verify the sender.

use super::http::{redact_url, request, snippet};
use super::{AutoRule, Delivered, Destination, DestinationConfig, Kind, Payload};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub struct Webhook {
    d: DestinationConfig,
}

impl Webhook {
    pub fn new(d: DestinationConfig) -> Self {
        Self { d }
    }
    fn url(&self) -> Result<String> {
        let Some(u) = super::secret_for(&self.d, "") else { bail!("add the webhook URL for “{}”", self.d.name) };
        if !(u.starts_with("https://") || u.starts_with("http://")) {
            bail!("the webhook URL should start with https://");
        }
        Ok(u)
    }
    fn post(&self, body: &Value) -> Result<super::http::Response> {
        let url = self.url()?;
        let raw = serde_json::to_string(body)?;
        let sig = super::secret_for(&self.d, "signing").map(|k| format!("sha256={}", hex(&hmac_sha256(k.as_bytes(), raw.as_bytes()))));
        let mut headers: Vec<(&str, &str)> = vec![("User-Agent", "Stenly")];
        if let Some(s) = &sig {
            headers.push(("X-Stenly-Signature", s));
        }
        let r = request("POST", &url, &headers, Some(body), "the webhook")?;
        if !r.ok() {
            bail!("{} returned {}: {}", redact_url(&url), r.status, snippet(&r.text));
        }
        Ok(r)
    }
}

pub fn body(p: &Payload, include_transcript: bool, resend: bool) -> Value {
    let m = &p.meta;
    let ev = m.event.as_ref();
    let external: Vec<String> = super::people::external(m).iter().map(|a| a.email.to_ascii_lowercase()).collect();
    let attendees: Vec<Value> = super::people::others(m)
        .iter()
        .map(|a| json!({ "name": super::people::name(a), "email": a.email, "external": external.contains(&a.email.to_ascii_lowercase()) }))
        .collect();
    let mut v = json!({
        "type": "stenly.meeting.v1",
        "id": p.session_id(),
        "title": m.title,
        "started": m.started.to_rfc3339(),
        "ended": m.ended.map(|e| e.to_rfc3339()),
        "duration_min": m.duration_mins(),
        "attendees": attendees,
        "meeting_url": ev.and_then(|e| e.link.as_ref()).map(|l| l.url.clone()),
        "calendar": ev.and_then(|e| e.calendar_name.clone().or_else(|| e.calendar.clone())),
        "calendar_event_id": ev.map(|e| e.uid.clone()),
        "tags": p.summary.as_ref().map(|s| s.tags.clone()).unwrap_or_default(),
        "summary": p.summary,
        "markdown": p.body_markdown(),
        "resend": resend,
    });
    if include_transcript {
        v["transcript"] = Value::Array(
            p.transcript
                .segments
                .iter()
                .map(|s| json!({ "start": s.start, "end": s.end, "speaker": super::speaker_name(s), "text": s.text }))
                .collect(),
        );
    }
    v
}

impl Destination for Webhook {
    fn id(&self) -> &str {
        &self.d.id
    }
    fn name(&self) -> &str {
        &self.d.name
    }
    fn kind(&self) -> Kind {
        Kind::Webhook
    }
    fn service(&self) -> String {
        match self.url().ok().and_then(|u| super::people::host(&u)) {
            Some(h) => h,
            None => "your webhook".into(),
        }
    }
    fn updates_in_place(&self) -> bool {
        false
    }
    fn ready(&self) -> Result<()> {
        self.url().map(|_| ())
    }
    fn test(&self) -> Result<String> {
        let r = self.post(&json!({ "type": "stenly.test", "message": "Test from Stenly" }))?;
        Ok(format!("Answered {}", r.status))
    }
    fn send(&self, p: &Payload, previous: Option<&str>) -> Result<Delivered> {
        let resend = previous.is_some();
        let r = self.post(&body(p, self.d.webhook.include_transcript, resend))?;
        // Receivers may answer {"url": "..."} to link back to what they created.
        let j = r.json();
        let url = j["url"].as_str().filter(|u| u.starts_with("http")).map(str::to_string);
        let id = j["id"].as_str().map(str::to_string).or_else(|| Some(p.session_id()));
        Ok(Delivered { url, remote_id: id, detail: Some(format!("Delivered ({})", r.status)), clipboard: None })
    }
    fn auto(&self) -> &AutoRule {
        &self.d.auto
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// HMAC-SHA256 (RFC 2104).
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destinations::testutil::{self, MockServer};

    #[test]
    fn hmac_matches_rfc4231() {
        // RFC 4231 test case 2
        assert_eq!(hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }

    #[test]
    fn posts_meeting_json() {
        let srv = MockServer::start(|_| (200, r#"{"url":"https://crm.example/m/1","id":"m1"}"#.into()));
        let var = "STENLY_TEST_WEBHOOK_URL";
        unsafe { std::env::set_var(var, format!("{}/hook", srv.url)) };
        let mut d = DestinationConfig::new(Kind::Webhook, "CRM hook");
        d.secret_env = var.into();
        let w = Webhook::new(d);
        let out = w.send(&testutil::payload(), None).unwrap();
        assert_eq!(out.url.as_deref(), Some("https://crm.example/m/1"));
        assert_eq!(out.remote_id.as_deref(), Some("m1"));
        let req = srv.requests().pop().unwrap();
        assert_eq!(req.method, "POST");
        let v = req.json();
        assert_eq!(v["type"], "stenly.meeting.v1");
        assert_eq!(v["title"], "Acme pilot");
        assert_eq!(v["duration_min"], 31);
        assert_eq!(v["attendees"].as_array().unwrap().len(), 2);
        assert_eq!(v["attendees"][0]["external"], true);
        assert_eq!(v["transcript"][1]["speaker"], "Them");
        assert_eq!(v["summary"]["action_items"][0]["task"], "Send the contract");
        assert_eq!(v["resend"], false);
        assert!(req.header("X-Stenly-Signature").is_none());
    }

    #[test]
    fn server_error_is_reported_without_the_url_path() {
        let srv = MockServer::start(|_| (500, "oops".into()));
        let var = "STENLY_TEST_WEBHOOK_500";
        unsafe { std::env::set_var(var, format!("{}/secret-token-path", srv.url)) };
        let mut d = DestinationConfig::new(Kind::Webhook, "Hook");
        d.secret_env = var.into();
        let e = format!("{:#}", Webhook::new(d).send(&testutil::payload(), None).unwrap_err());
        assert!(e.contains("500"));
        assert!(!e.contains("secret-token-path"));
    }
}
