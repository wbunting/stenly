//! OAuth 2.1 pieces shared by every browser sign-in (Kody for the calendar,
//! MCP servers for context): dynamic client registration, PKCE, and the
//! loopback listener that receives the authorization code.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::net::TcpListener;
use std::time::Duration;

pub fn b64url(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        if c.len() > 1 {
            out.push(T[(n >> 6) as usize & 63] as char);
        }
        if c.len() > 2 {
            out.push(T[n as usize & 63] as char);
        }
    }
    out
}

pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    getrandom::fill(&mut v).expect("os rng");
    v
}

pub fn urlenc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `application/x-www-form-urlencoded` (also used for query strings).
pub fn form(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", urlenc(k), urlenc(v))).collect::<Vec<_>>().join("&")
}

/// `http://host/x` → `https://host/x` when `base` is https on the same host.
/// Behind a TLS-terminating proxy (Tailscale Funnel / Serve) servers advertise
/// `http://` endpoints; POSTing there gets a 301 that clients follow as a GET.
pub fn same_scheme(base: &str, url: &str) -> String {
    match (base.strip_prefix("https://"), url.strip_prefix("http://")) {
        (Some(b), Some(rest)) if rest.split('/').next() == b.split('/').next().and_then(|h| h.split('/').next()) => format!("https://{rest}"),
        _ => url.to_string(),
    }
}

/// A PKCE pair plus `state`.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
    pub state: String,
}

pub fn pkce() -> Pkce {
    use sha2::Digest;
    let verifier = b64url(&random_bytes(32));
    let challenge = b64url(&sha2::Sha256::digest(verifier.as_bytes()));
    Pkce { verifier, challenge, state: b64url(&random_bytes(16)) }
}

/// A loopback listener and its redirect URI (`http://127.0.0.1:<port>/callback`).
pub fn loopback() -> Result<(TcpListener, String)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let uri = format!("http://127.0.0.1:{}/callback", listener.local_addr()?.port());
    Ok((listener, uri))
}

/// Dynamic client registration (RFC 7591) for a public client; returns the client id.
pub fn register_client(agent: &ureq::Agent, registration_endpoint: &str, client_name: &str, redirect_uri: &str) -> Result<String> {
    let reg: Value = agent
        .post(registration_endpoint)
        .send_json(json!({
            "client_name": client_name,
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        }))
        .context("registering with the server")?
        .body_mut()
        .read_json()
        .context("registration reply wasn't JSON")?;
    Ok(reg["client_id"].as_str().context("registration gave no client_id")?.to_string())
}

/// Wait (up to five minutes) for the browser's redirect to `/callback` and
/// return the authorization code. `service` names the other side on the
/// success page and in errors ("Kody").
pub fn wait_for_code(listener: &TcpListener, state: &str, service: &str) -> Result<String> {
    use std::io::{BufRead, BufReader, Write};
    listener.set_nonblocking(true)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    loop {
        if std::time::Instant::now() > deadline {
            bail!("sign-in timed out");
        }
        let (mut stream, _) = match listener.accept() {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(150));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        stream.set_nonblocking(false)?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line)?;
        let path = line.split_whitespace().nth(1).unwrap_or("");
        if !path.starts_with("/callback") {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n");
            continue;
        }
        let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
        let get = |k: &str| query.split('&').find_map(|kv| kv.strip_prefix(&format!("{k}="))).map(crate::meeting_url::percent_decode_pub);
        let ok = get("state").as_deref() == Some(state) && get("code").is_some();
        let page = if ok {
            format!("<html><body style=\"font-family:-apple-system;background:#1f1f1f;color:#eee;display:flex;align-items:center;justify-content:center;height:100vh;margin:0\"><div style=\"text-align:center\"><h2 style=\"font-weight:500\">Stenly is connected to {service}</h2><p style=\"color:#999\">You can close this tab.</p></div></body></html>")
        } else {
            "<html><body>Sign-in failed. Return to Stenly and try again.</body></html>".to_string()
        };
        let _ = write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{page}", page.len());
        if let Some(err) = get("error") {
            bail!("{service} declined: {err}");
        }
        if ok {
            return Ok(get("code").unwrap_or_default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodings() {
        assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
        assert_eq!(form(&[("a b", "c&d")]), "a%20b=c%26d");
        let p = pkce();
        assert_eq!(p.verifier.len(), 43);
        assert_ne!(p.verifier, p.challenge);
    }

    #[test]
    fn upgrades_same_host_only() {
        let b = "https://kody.example.ts.net";
        assert_eq!(same_scheme(b, "http://kody.example.ts.net/oauth/token"), "https://kody.example.ts.net/oauth/token");
        assert_eq!(same_scheme(b, "http://other.host/oauth/token"), "http://other.host/oauth/token");
        assert_eq!(same_scheme("http://localhost:3742", "http://localhost:3742/t"), "http://localhost:3742/t");
    }

    #[test]
    fn loopback_receives_code() {
        let (l, uri) = loopback().unwrap();
        let port = uri.split(':').nth(2).unwrap().split('/').next().unwrap().to_string();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            std::thread::sleep(Duration::from_millis(100));
            let mut s = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
            s.write_all(b"GET /callback?code=abc%20d&state=xyz HTTP/1.1\r\n\r\n").unwrap();
            let mut out = String::new();
            let _ = s.read_to_string(&mut out);
        });
        assert_eq!(wait_for_code(&l, "xyz", "Test").unwrap(), "abc d");
    }
}
