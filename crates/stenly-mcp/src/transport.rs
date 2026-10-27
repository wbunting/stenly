//! stdio (newline-delimited JSON-RPC) and streamable-HTTP transports.

use crate::mcp::Server;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::net::SocketAddr;

pub const DEFAULT_PORT: u16 = 4791;

/// Serve MCP over stdin/stdout until stdin closes. Logs must go to stderr.
pub fn serve_stdio() -> Result<()> {
    let server = Server;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => server.handle(msg),
            Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } })),
        };
        if let Some(r) = reply {
            serde_json::to_writer(&mut stdout, &r)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// Streamable HTTP on `addr` (POST /mcp → JSON). Blocks forever.
pub fn serve_http(addr: SocketAddr) -> Result<()> {
    let http = tiny_http::Server::http(addr).map_err(|e| anyhow::anyhow!("binding {addr}: {e}"))?;
    tracing::info!(url = %format!("http://{addr}/mcp"), "stenly MCP listening");
    let server = Server;
    for req in http.incoming_requests() {
        if let Err(e) = handle_http(&server, req) {
            tracing::warn!(error = %format!("{e:#}"), "mcp http request failed");
        }
    }
    Ok(())
}

/// Run [`serve_http`] on a background thread (used by the menubar app).
pub fn spawn_http(addr: SocketAddr) -> Result<std::thread::JoinHandle<()>> {
    // Bind eagerly so "port in use" surfaces to the caller instead of a log line.
    let probe = std::net::TcpListener::bind(addr).with_context(|| format!("binding {addr}"))?;
    drop(probe);
    Ok(std::thread::Builder::new().name("stenly-mcp".into()).spawn(move || {
        if let Err(e) = serve_http(addr) {
            tracing::error!(error = %format!("{e:#}"), "stenly MCP server stopped");
        }
    })?)
}

fn header<'a>(req: &'a tiny_http::Request, name: &str) -> Option<&'a str> {
    req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str())
}

fn is_loopback_host(authority: &str) -> bool {
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// Browsers send Origin; anything not from localhost is a CSRF attempt.
fn origin_ok(origin: Option<&str>) -> bool {
    let Some(o) = origin else { return true };
    let Some(rest) = o.split_once("://").map(|(_, r)| r) else { return false };
    is_loopback_host(rest.split('/').next().unwrap_or(rest))
}

/// A non-loopback Host header means DNS rebinding.
fn host_ok(host: Option<&str>) -> bool {
    host.is_none_or(is_loopback_host)
}

fn respond(req: tiny_http::Request, status: u16, body: Option<&Value>) -> Result<()> {
    let ct = tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap();
    let resp = match body {
        Some(b) => tiny_http::Response::from_data(serde_json::to_vec(b)?).with_status_code(status).with_header(ct),
        None => tiny_http::Response::from_data(Vec::new()).with_status_code(status),
    };
    req.respond(resp)?;
    Ok(())
}

fn handle_http(server: &Server, mut req: tiny_http::Request) -> Result<()> {
    if !origin_ok(header(&req, "Origin")) || !host_ok(header(&req, "Host")) {
        return respond(req, 403, Some(&json!({ "error": "forbidden origin" })));
    }
    let path = req.url().split('?').next().unwrap_or("/").to_string();
    match (req.method(), path.as_str()) {
        (tiny_http::Method::Post, "/mcp" | "/") => {
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body)?;
            let msg: Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(e) => {
                    let err = json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } });
                    return respond(req, 400, Some(&err));
                }
            };
            match server.handle(msg) {
                Some(reply) => respond(req, 200, Some(&reply)),
                None => respond(req, 202, None), // notification / client response
            }
        }
        // No server-initiated messages, so no SSE stream; no sessions to delete.
        (tiny_http::Method::Get, "/mcp" | "/") => respond(req, 405, None),
        (tiny_http::Method::Delete, "/mcp" | "/") => respond(req, 405, None),
        (tiny_http::Method::Get, "/health") => respond(req, 200, Some(&json!({ "ok": true, "name": "stenly", "version": env!("CARGO_PKG_VERSION") }))),
        _ => respond(req, 404, None),
    }
}

#[cfg(test)]
mod tests {
    use super::{host_ok, origin_ok};

    #[test]
    fn origins() {
        assert!(origin_ok(None));
        assert!(origin_ok(Some("http://localhost:3000")));
        assert!(origin_ok(Some("http://127.0.0.1")));
        assert!(origin_ok(Some("http://[::1]:8080")));
        assert!(!origin_ok(Some("https://evil.example")));
        assert!(!origin_ok(Some("http://localhost.evil.example")));
        assert!(!origin_ok(Some("null")));
        assert!(host_ok(Some("127.0.0.1:4791")));
        assert!(host_ok(Some("localhost")));
        assert!(!host_ok(Some("attacker.example:4791")));
    }
}
