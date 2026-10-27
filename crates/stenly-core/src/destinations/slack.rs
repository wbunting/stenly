//! Slack via an incoming webhook (https://api.slack.com/messaging/webhooks).
//! The webhook URL is the credential, so it lives in the Keychain.

use super::http::{redact_url, request, snippet};
use super::markdown::{chunk_lines, to_slack};
use super::{AutoRule, Delivered, Destination, DestinationConfig, Kind, Payload};
use anyhow::{Result, bail};
use serde_json::{Value, json};

pub struct Slack {
    d: DestinationConfig,
}

impl Slack {
    pub fn new(d: DestinationConfig) -> Self {
        Self { d }
    }
    fn url(&self) -> Result<String> {
        let Some(u) = super::secret_for(&self.d, "") else { bail!("add the Slack webhook URL for “{}”", self.d.name) };
        if !(u.starts_with("https://") || u.starts_with("http://127.0.0.1") || u.starts_with("http://localhost")) {
            bail!("the Slack webhook URL should start with https://");
        }
        Ok(u)
    }
    fn post(&self, body: &Value) -> Result<()> {
        let url = self.url()?;
        let r = request("POST", &url, &[], Some(body), "Slack")?;
        match r.status {
            200..=299 => Ok(()),
            403 | 404 | 410 => bail!("Slack rejected the webhook ({}: {}); it may have been revoked", r.status, snippet(&r.text)),
            s => bail!("Slack returned {s} from {}: {}", redact_url(&url), snippet(&r.text)),
        }
    }
}

/// The message: title + date line, then the note body in sections.
pub fn message(p: &Payload) -> Value {
    let title = &p.meta.title;
    let when = p.meta.started.format("%a %b %-d, %Y · %-I:%M %p").to_string();
    let attendees: Vec<String> = super::people::others(&p.meta).iter().map(|a| super::people::name(a)).collect();
    let mut context = when.clone();
    if !attendees.is_empty() {
        context.push_str(&format!(" · {}", attendees.join(", ")));
    }
    let body = to_slack(&p.body_markdown());
    let mut blocks = vec![
        json!({ "type": "header", "text": { "type": "plain_text", "text": title.chars().take(150).collect::<String>(), "emoji": false } }),
        json!({ "type": "context", "elements": [{ "type": "mrkdwn", "text": context }] }),
    ];
    // Slack: ≤50 blocks, ≤3000 chars per section.
    for chunk in chunk_lines(&body, 2900).into_iter().take(45) {
        blocks.push(json!({ "type": "section", "text": { "type": "mrkdwn", "text": chunk } }));
    }
    json!({ "text": format!("{title} ({when})"), "blocks": blocks, "unfurl_links": false })
}

impl Destination for Slack {
    fn id(&self) -> &str {
        &self.d.id
    }
    fn name(&self) -> &str {
        &self.d.name
    }
    fn kind(&self) -> Kind {
        Kind::Slack
    }
    fn updates_in_place(&self) -> bool {
        false
    }
    fn ready(&self) -> Result<()> {
        self.url().map(|_| ())
    }
    fn test(&self) -> Result<String> {
        // Incoming webhooks have no read-only endpoint: an empty payload is
        // rejected with "no_text" (400) when the hook is valid, 403/404 when not.
        let url = self.url()?;
        let r = request("POST", &url, &[], Some(&json!({})), "Slack")?;
        match (r.status, r.text.trim()) {
            (400, "no_text") | (400, "invalid_payload") | (200..=299, _) => Ok("Webhook is valid".into()),
            (403 | 404 | 410, t) => bail!("Slack doesn't recognise this webhook ({t})"),
            (s, t) => bail!("Slack returned {s}: {}", snippet(t)),
        }
    }
    fn send(&self, p: &Payload, _previous: Option<&str>) -> Result<Delivered> {
        self.post(&message(p))?;
        Ok(Delivered { detail: Some("Posted".into()), ..Default::default() })
    }
    fn auto(&self) -> &AutoRule {
        &self.d.auto
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destinations::testutil::{self, MockServer};

    #[test]
    fn posts_blocks() {
        let srv = MockServer::start(|r| if r.body == "{}" { (400, "no_text".into()) } else { (200, "ok".into()) });
        let var = "STENLY_TEST_SLACK_URL";
        unsafe { std::env::set_var(var, format!("{}/services/T/B/x", srv.url)) };
        let mut d = DestinationConfig::new(Kind::Slack, "Sales");
        d.secret_env = var.into();
        let s = Slack::new(d);
        assert_eq!(s.test().unwrap(), "Webhook is valid");
        s.send(&testutil::payload(), None).unwrap();
        let req = srv.requests().pop().unwrap();
        assert_eq!(req.path, "/services/T/B/x");
        let v = req.json();
        assert_eq!(v["blocks"][0]["text"]["text"], "Acme pilot");
        assert!(v["blocks"][1]["elements"][0]["text"].as_str().unwrap().contains("Ana, Bo"));
        let body = v["blocks"][2]["text"]["text"].as_str().unwrap();
        assert!(body.contains("*Summary*"));
        assert!(body.contains("☐ *Me*: Send the contract"));
    }

    #[test]
    fn revoked_hook_is_an_error() {
        let srv = MockServer::start(|_| (404, "no_service".into()));
        let var = "STENLY_TEST_SLACK_URL_404";
        unsafe { std::env::set_var(var, &srv.url) };
        let mut d = DestinationConfig::new(Kind::Slack, "Sales");
        d.secret_env = var.into();
        let e = Slack::new(d).send(&testutil::payload(), None).unwrap_err();
        assert!(format!("{e:#}").contains("revoked"));
    }
}
