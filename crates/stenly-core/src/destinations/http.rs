//! Small JSON-over-HTTP helper shared by the cloud destinations: bounded
//! timeouts, readable errors, and no credentials in error messages.

use anyhow::{Result, bail};
use serde_json::Value;

fn agent() -> &'static ureq::Agent {
    static A: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    A.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_connect(Some(std::time::Duration::from_secs(10)))
            .timeout_global(Some(std::time::Duration::from_secs(45)))
            .http_status_as_error(false)
            .build()
            .into()
    })
}

pub struct Response {
    pub status: u16,
    pub text: String,
}

impl Response {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or(Value::Null)
    }
}

/// `service` names the other end in errors ("Notion", "Slack").
pub fn request(method: &str, url: &str, headers: &[(&str, &str)], body: Option<&Value>, service: &str) -> Result<Response> {
    let a = agent();
    let fail = |e: ureq::Error| -> anyhow::Error {
        match e {
            ureq::Error::Timeout(_) => anyhow::anyhow!("{service} didn't answer in time"),
            ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => anyhow::anyhow!("can't reach {service} (are you online?)"),
            ureq::Error::BadUri(_) | ureq::Error::Http(_) => anyhow::anyhow!("the {service} address isn't a valid URL"),
            e => anyhow::anyhow!("{service}: {e}"),
        }
    };
    macro_rules! hdrs {
        ($r:expr) => {{
            let mut r = $r;
            for (k, v) in headers {
                r = r.header(*k, *v);
            }
            r
        }};
    }
    let mut resp = match (method, body) {
        ("GET", _) => hdrs!(a.get(url)).call(),
        ("DELETE", _) => hdrs!(a.delete(url)).call(),
        ("PATCH", Some(b)) => hdrs!(a.patch(url)).send_json(b),
        ("PUT", Some(b)) => hdrs!(a.put(url)).send_json(b),
        ("POST", Some(b)) => hdrs!(a.post(url)).send_json(b),
        ("POST", None) => hdrs!(a.post(url)).send_empty(),
        (m, _) => bail!("unsupported method {m}"),
    }
    .map_err(fail)?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    Ok(Response { status, text })
}

/// Hide everything after the host of a secret URL (Slack/webhook paths are
/// the credential): `https://hooks.slack.com/…`.
pub fn redact_url(url: &str) -> String {
    let u = url.trim();
    match u.split_once("://") {
        Some((scheme, rest)) => {
            let host = rest.split(['/', '?']).next().unwrap_or("");
            if rest.len() > host.len() { format!("{scheme}://{host}/…") } else { format!("{scheme}://{host}") }
        }
        None => "…".into(),
    }
}

pub fn snippet(s: &str) -> String {
    let t: String = s.chars().take(200).collect();
    t.replace('\n', " ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn redacts() {
        assert_eq!(super::redact_url("https://hooks.slack.com/services/T0/B0/xyz"), "https://hooks.slack.com/…");
        assert_eq!(super::redact_url("http://127.0.0.1:9"), "http://127.0.0.1:9");
    }
}
