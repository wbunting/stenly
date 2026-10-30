//! Polar (polar.sh) licence keys through its public customer-portal API.
//!
//! These three endpoints take only the organization ID, which is public: the
//! app ships no API token. Polar is the merchant of record (it handles VAT),
//! and the licence-key benefit on the product sets the activation limit.

use super::{Activation, LicenseError, Provider};
use serde_json::{Value, json};
use std::time::Duration;

pub const PRODUCTION_API: &str = "https://api.polar.sh";
pub const SANDBOX_API: &str = "https://sandbox-api.polar.sh";

pub struct Polar {
    api: String,
    organization_id: String,
    agent: ureq::Agent,
}

impl Polar {
    pub fn new(api: &str, organization_id: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            api: api.trim_end_matches('/').to_string(),
            organization_id: organization_id.to_string(),
            agent,
        }
    }

    /// The organization and API chosen when the official app was built
    /// (`STENLY_POLAR_ORG_ID`, optional `STENLY_POLAR_API` for the sandbox).
    pub fn from_build() -> Option<Self> {
        let org = option_env!("STENLY_POLAR_ORG_ID").filter(|o| !o.trim().is_empty())?;
        let api = option_env!("STENLY_POLAR_API")
            .filter(|a| a.starts_with("https://"))
            .unwrap_or(PRODUCTION_API);
        Some(Self::new(api, org))
    }

    fn post(&self, path: &str, body: Value) -> Result<(u16, Value), LicenseError> {
        let url = format!("{}/v1/customer-portal/license-keys/{path}", self.api);
        let mut r = self
            .agent
            .post(&url)
            .send_json(&body)
            .map_err(|e| LicenseError::Offline(e.to_string()))?;
        let status = r.status().as_u16();
        let text = r.body_mut().read_to_string().unwrap_or_default();
        Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
    }
}

/// Map an error status. 4xx answers are definite; 429 and 5xx are not.
fn failure(status: u16, body: &Value) -> LicenseError {
    let detail = match &body["detail"] {
        Value::String(s) => s.clone(),
        other if !other.is_null() => other.to_string(),
        _ => String::new(),
    };
    match status {
        // "has reached its activation limit" / "only supports N activations"
        403 if {
            let d = detail.to_lowercase();
            d.contains("limit") || d.contains("activations")
        } =>
        {
            LicenseError::LimitReached
        }
        404 => LicenseError::Rejected(
            "That licence key isn't valid. Check it against your purchase email.".into(),
        ),
        403 => LicenseError::Rejected(
            "This licence key has been revoked, disabled or has expired.".into(),
        ),
        400 | 401 | 422 => LicenseError::Rejected(if detail.is_empty() {
            "That licence key isn't valid.".into()
        } else {
            format!("The licence server refused the key: {detail}")
        }),
        _ => LicenseError::Offline(format!("licence server returned {status}")),
    }
}

impl Provider for Polar {
    fn activate(&self, key: &str, label: &str) -> Result<Activation, LicenseError> {
        let (status, body) = self.post(
            "activate",
            json!({ "key": key, "organization_id": self.organization_id, "label": label }),
        )?;
        if status != 200 && status != 201 {
            return Err(failure(status, &body));
        }
        let id = body["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                LicenseError::Offline("unexpected reply from the licence server".into())
            })?;
        Ok(Activation {
            activation_id: id.to_string(),
        })
    }

    fn validate(&self, key: &str, activation_id: &str) -> Result<(), LicenseError> {
        let (status, body) = self.post("validate", json!({ "key": key, "organization_id": self.organization_id, "activation_id": activation_id }))?;
        if status != 200 {
            return Err(failure(status, &body));
        }
        // A 200 for a key that is no longer granted still doesn't count.
        match body["status"].as_str() {
            Some("granted") | None => Ok(()),
            Some(other) => Err(LicenseError::Rejected(format!(
                "This licence key is {other}."
            ))),
        }
    }

    fn deactivate(&self, key: &str, activation_id: &str) -> Result<(), LicenseError> {
        let (status, body) = self.post("deactivate", json!({ "key": key, "organization_id": self.organization_id, "activation_id": activation_id }))?;
        if status == 200 || status == 204 {
            return Ok(());
        }
        Err(failure(status, &body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A tiny Polar stand-in on 127.0.0.1: one key, an activation limit, and a
    /// log of the request bodies it received.
    struct Server {
        url: String,
        log: Arc<Mutex<Vec<(String, Value)>>>,
        revoked: Arc<Mutex<bool>>,
    }

    fn server(limit: usize, fail_with: Option<u16>) -> Server {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let revoked = Arc::new(Mutex::new(false));
        let (log2, rev2) = (log.clone(), revoked.clone());
        std::thread::spawn(move || {
            let mut active: Vec<String> = vec![];
            let mut n = 0;
            for s in l.incoming() {
                let Ok(mut s) = s else { break };
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut len = 0;
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h == "\r\n" || h.is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut buf = vec![0; len];
                r.read_exact(&mut buf).unwrap();
                let body: Value = serde_json::from_slice(&buf).unwrap_or(Value::Null);
                log2.lock().unwrap().push((path.clone(), body.clone()));
                let ok_key = body["key"] == "POLAR-KEY" && body["organization_id"] == "org-123";
                let revoked = *rev2.lock().unwrap();
                let (code, reply) = if let Some(c) = fail_with {
                    (c, json!({"detail": "boom"}))
                } else if path.ends_with("/activate") {
                    if !ok_key {
                        (404, json!({"detail": "License key not found."}))
                    } else if revoked {
                        (403, json!({"detail": "License key is revoked."}))
                    } else if active.len() >= limit {
                        (
                            403,
                            json!({"detail": "License key only supports 1 activations."}),
                        )
                    } else {
                        n += 1;
                        let id = format!("act-{n}");
                        active.push(id.clone());
                        (200, json!({"id": id, "label": body["label"]}))
                    }
                } else if path.ends_with("/validate") {
                    let id = body["activation_id"].as_str().unwrap_or("");
                    if ok_key && !revoked && active.iter().any(|a| a == id) {
                        (200, json!({"status": "granted"}))
                    } else {
                        (404, json!({"detail": "License key not found."}))
                    }
                } else if path.ends_with("/deactivate") {
                    let id = body["activation_id"].as_str().unwrap_or("").to_string();
                    if active.contains(&id) {
                        active.retain(|a| *a != id);
                        (204, Value::Null)
                    } else {
                        (404, json!({"detail": "not found"}))
                    }
                } else {
                    (404, Value::Null)
                };
                let text = if reply.is_null() {
                    String::new()
                } else {
                    reply.to_string()
                };
                let reason = if code < 300 { "OK" } else { "Error" };
                let _ = write!(
                    s,
                    "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
            }
        });
        Server { url, log, revoked }
    }

    #[test]
    fn activate_validate_deactivate_against_a_polar_shaped_server() {
        let s = server(1, None);
        let p = Polar::new(&s.url, "org-123");
        let a = p.activate("POLAR-KEY", "Stenly on macOS").unwrap();
        assert_eq!(a.activation_id, "act-1");
        p.validate("POLAR-KEY", &a.activation_id).unwrap();
        // one-Mac limit
        assert_eq!(
            p.activate("POLAR-KEY", "second Mac").unwrap_err(),
            LicenseError::LimitReached
        );
        p.deactivate("POLAR-KEY", &a.activation_id).unwrap();
        assert!(matches!(
            p.validate("POLAR-KEY", &a.activation_id),
            Err(LicenseError::Rejected(_))
        ));
        // the slot is free again
        assert_eq!(
            p.activate("POLAR-KEY", "second Mac").unwrap().activation_id,
            "act-2"
        );
        let log = s.log.lock().unwrap();
        assert_eq!(log[0].0, "/v1/customer-portal/license-keys/activate");
        assert_eq!(log[0].1["organization_id"], "org-123");
        assert_eq!(log[0].1["label"], "Stenly on macOS");
        assert_eq!(log[1].1["activation_id"], "act-1");
    }

    #[test]
    fn unknown_and_revoked_keys_are_definite_rejections() {
        let s = server(1, None);
        let p = Polar::new(&s.url, "org-123");
        assert!(matches!(
            p.activate("WRONG", "x"),
            Err(LicenseError::Rejected(_))
        ));
        *s.revoked.lock().unwrap() = true;
        assert!(
            matches!(p.activate("POLAR-KEY", "x"), Err(LicenseError::Rejected(m)) if m.contains("revoked"))
        );
    }

    #[test]
    fn server_errors_and_unreachable_hosts_are_offline_not_revocation() {
        let s = server(1, Some(503));
        let p = Polar::new(&s.url, "org-123");
        assert!(matches!(
            p.validate("POLAR-KEY", "act-1"),
            Err(LicenseError::Offline(_))
        ));
        assert!(matches!(
            Polar::new(&server(1, Some(429)).url, "org-123").validate("POLAR-KEY", "a"),
            Err(LicenseError::Offline(_))
        ));
        // nothing listening
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let dead = Polar::new(&format!("http://127.0.0.1:{port}"), "org-123");
        assert!(matches!(
            dead.validate("POLAR-KEY", "a"),
            Err(LicenseError::Offline(_))
        ));
    }

    /// Against the real Polar API (network): an unknown key is a definite
    /// rejection, never "offline". `cargo test -p stenly-core -- --ignored live_polar`
    #[test]
    #[ignore]
    fn live_polar_rejects_an_unknown_key() {
        let p = Polar::new(PRODUCTION_API, "a35b9620-afa6-4682-af79-888e861c1e5e");
        assert!(matches!(
            p.activate("STENLY-NOT-A-REAL-KEY", "probe"),
            Err(LicenseError::Rejected(_))
        ));
        assert!(matches!(
            p.validate("STENLY-NOT-A-REAL-KEY", "x"),
            Err(LicenseError::Rejected(_))
        ));
    }

    #[test]
    fn the_licence_manager_runs_end_to_end_over_http() {
        use super::super::{LicenseManager, Refresh, State, tests_support::MemVault};
        let s = server(1, None);
        let p = Polar::new(&s.url, "org-123");
        let dir = tempfile::tempdir().unwrap();
        let m = LicenseManager::new(
            Box::new(MemVault::default()),
            dir.path().join("trial.json"),
            true,
            10,
        );
        let now = chrono::Utc::now();
        assert_eq!(
            m.activate(&p, "POLAR-KEY", now).unwrap().state(),
            State::Licensed
        );
        assert_eq!(m.refresh(&p, now, true), Refresh::Validated);
        *s.revoked.lock().unwrap() = true;
        assert_eq!(m.refresh(&p, now, true), Refresh::Revoked);
    }
}
