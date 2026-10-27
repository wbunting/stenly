//! A small MCP client: streamable HTTP (with OAuth or a bearer key) and stdio
//! (a child process speaking newline-delimited JSON-RPC). Enough for
//! `initialize`, `tools/list` and `tools/call`; no runtime beyond std + ureq.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use stenly_core::config::McpServerConfig;

pub const PROTOCOL: &str = "2025-06-18";
/// The built-in Kody entry reuses the Calendar sign-in.
pub const KODY_ID: &str = "kody";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoteTool {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(default)]
    pub annotations: Value,
}

impl RemoteTool {
    pub fn read_only(&self) -> bool {
        self.annotations["readOnlyHint"].as_bool() == Some(true)
    }
}

/// Which tools of a server the agent may call.
pub fn allowed(server: &McpServerConfig, tool: &RemoteTool) -> bool {
    if server.allow.is_empty() { tool.read_only() } else { server.allow.iter().any(|a| a == &tool.name) }
}

pub enum Transport {
    Http(HttpTransport),
    Stdio(StdioTransport),
}

pub struct McpClient {
    pub id: String,
    transport: Transport,
    next_id: AtomicU64,
    pub instructions: String,
}

impl McpClient {
    /// Connect and initialize (`initialize` + `notifications/initialized`).
    pub fn connect(server: &McpServerConfig) -> Result<Self> {
        let transport = if !server.command.is_empty() {
            Transport::Stdio(StdioTransport::spawn(&server.command, &server.env)?)
        } else if !server.url.trim().is_empty() {
            Transport::Http(HttpTransport::new(&server.id, server.url.trim())?)
        } else if server.id == KODY_ID {
            let (url, _) = stenly_core::kody::access_token(false).context("Kody isn't connected (Settings → Calendar)")?;
            Transport::Http(HttpTransport::new(&server.id, &url)?)
        } else {
            bail!("{}: no url or command", server.id);
        };
        let mut c = Self { id: server.id.clone(), transport, next_id: AtomicU64::new(1), instructions: String::new() };
        let init = c.request(
            "initialize",
            json!({ "protocolVersion": PROTOCOL, "capabilities": {}, "clientInfo": { "name": "stenly", "version": env!("CARGO_PKG_VERSION") } }),
        )?;
        c.instructions = init["instructions"].as_str().unwrap_or_default().to_string();
        c.notify("notifications/initialized", json!({}))?;
        Ok(c)
    }

    pub fn list_tools(&self) -> Result<Vec<RemoteTool>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..10 {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let r = self.request("tools/list", params)?;
            for t in r["tools"].as_array().into_iter().flatten() {
                if let Ok(t) = serde_json::from_value::<RemoteTool>(t.clone()) {
                    out.push(t);
                }
            }
            match r["nextCursor"].as_str() {
                Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
                _ => break,
            }
        }
        Ok(out)
    }

    /// Call a tool; returns its text (and structured content, if any) as one string.
    pub fn call_tool(&self, name: &str, args: Value) -> Result<String> {
        let r = self.request("tools/call", json!({ "name": name, "arguments": args }))?;
        let mut text: Vec<String> = r["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| match c["type"].as_str() {
                Some("text") => c["text"].as_str().map(str::to_string),
                Some("resource") => c["resource"]["text"].as_str().map(str::to_string),
                Some(other) => Some(format!("[{other} content omitted]")),
                None => None,
            })
            .collect();
        if text.is_empty() && !r["structuredContent"].is_null() {
            text.push(r["structuredContent"].to_string());
        }
        let joined = text.join("\n");
        if r["isError"].as_bool() == Some(true) {
            bail!("{}", if joined.trim().is_empty() { "the tool failed".to_string() } else { joined });
        }
        Ok(joined)
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let reply = match &self.transport {
            Transport::Http(h) => h.send(&msg, true)?,
            Transport::Stdio(s) => s.send(&msg, Some(id))?,
        }
        .with_context(|| format!("{}: no reply to {method}", self.id))?;
        if let Some(e) = reply.get("error") {
            bail!("{}: {}", self.id, e["message"].as_str().map(str::to_string).unwrap_or_else(|| e.to_string()));
        }
        Ok(reply["result"].clone())
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        match &self.transport {
            Transport::Http(h) => {
                let _ = h.send(&msg, false);
            }
            Transport::Stdio(s) => {
                s.send(&msg, None)?;
            }
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ HTTP

pub struct HttpTransport {
    id: String,
    url: String,
    session: Mutex<Option<String>>,
    token: Mutex<Option<String>>,
}

impl HttpTransport {
    fn new(id: &str, url: &str) -> Result<Self> {
        let token = crate::oauth::token_for(id, false).ok().flatten();
        Ok(Self { id: id.to_string(), url: url.to_string(), session: Mutex::new(None), token: Mutex::new(token) })
    }

    fn post(&self, msg: &Value) -> Result<(u16, Option<String>, String)> {
        let mut req = ureq::post(&self.url)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", PROTOCOL)
            .config()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(8)))
            .timeout_global(Some(Duration::from_secs(60)))
            .build();
        if let Some(t) = self.token.lock().unwrap().clone() {
            req = req.header("authorization", &format!("Bearer {t}"));
        }
        if let Some(s) = self.session.lock().unwrap().clone() {
            req = req.header("mcp-session-id", &s);
        }
        let mut resp = req.send_json(msg).map_err(|e| match e {
            ureq::Error::Timeout(_) => anyhow::anyhow!("{} didn't answer in time", self.id),
            ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => anyhow::anyhow!("can't reach {} at {}", self.id, self.url),
            e => anyhow::Error::from(e),
        })?;
        let status = resp.status().as_u16();
        let sid = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()).map(str::to_string);
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        Ok((status, sid, text))
    }

    /// POST one message; returns the JSON-RPC reply when `want_reply`.
    fn send(&self, msg: &Value, want_reply: bool) -> Result<Option<Value>> {
        let (mut status, mut sid, mut text) = self.post(msg)?;
        if status == 401 {
            // expired: refresh (or, for Kody, take a fresh token from the Calendar sign-in) and retry once
            match crate::oauth::token_for(&self.id, true).ok().flatten() {
                Some(t) => {
                    *self.token.lock().unwrap() = Some(t);
                    (status, sid, text) = self.post(msg)?;
                }
                None => return Err(NeedsSignIn(self.id.clone()).into()),
            }
            if status == 401 {
                return Err(NeedsSignIn(self.id.clone()).into());
            }
        }
        if let Some(s) = sid.take() {
            *self.session.lock().unwrap() = Some(s);
        }
        if !(200..300).contains(&status) {
            bail!("{} answered HTTP {status}: {}", self.id, text.chars().take(200).collect::<String>());
        }
        if !want_reply {
            return Ok(None);
        }
        let id = msg.get("id").cloned();
        Ok(parse_http_body(&text, id.as_ref()))
    }
}

/// Plain JSON, or SSE with `data:` lines (the reply is the one with our id).
pub fn parse_http_body(text: &str, id: Option<&Value>) -> Option<Value> {
    let t = text.trim_start();
    if t.starts_with('{') {
        return serde_json::from_str(t).ok();
    }
    let mut data = String::new();
    let mut found = None;
    let mut flush = |data: &mut String| {
        if !data.is_empty() {
            if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                if found.is_none() && (id.is_none() || v.get("id") == id) && (v.get("result").is_some() || v.get("error").is_some()) {
                    found = Some(v);
                }
            }
            data.clear();
        }
    };
    for line in text.lines() {
        if let Some(d) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(d.trim_start());
        } else if line.trim().is_empty() {
            flush(&mut data);
        }
    }
    flush(&mut data);
    found
}

/// The server wants the user to sign in (Settings → Context → Sign In).
#[derive(Debug)]
pub struct NeedsSignIn(pub String);

impl std::fmt::Display for NeedsSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sign in to use {}", self.0)
    }
}

impl std::error::Error for NeedsSignIn {}

// ------------------------------------------------------------------ stdio

type Pending = Arc<Mutex<HashMap<u64, std::sync::mpsc::Sender<Value>>>>;

/// A child process speaking newline-delimited JSON-RPC. Requests may be in
/// flight from several threads at once (the agent runs a turn's tool calls in
/// parallel): a reader thread hands each reply to the request with its id.
pub struct StdioTransport {
    child: Mutex<Child>,
    stdin: Mutex<Box<dyn Write + Send>>,
    pending: Pending,
}

impl StdioTransport {
    fn spawn(argv: &[String], env: &std::collections::BTreeMap<String, String>) -> Result<Self> {
        let (cmd, args) = argv.split_first().context("empty command")?;
        let mut c = Command::new(resolve(cmd));
        c.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        // apps launched from Finder get a bare PATH; add the usual places for npx / uvx / bun
        let path = std::env::var("PATH").unwrap_or_default();
        let home = std::env::var("HOME").unwrap_or_default();
        c.env("PATH", format!("{path}:/opt/homebrew/bin:/usr/local/bin:{home}/.local/bin:{home}/.bun/bin:{home}/.cargo/bin"));
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.spawn().with_context(|| format!("starting {cmd}"))?;
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let pending: Pending = Arc::default();
        spawn_reader(Box::new(stdout), pending.clone());
        Ok(Self { child: Mutex::new(child), stdin: Mutex::new(Box::new(stdin)), pending })
    }

    fn send(&self, msg: &Value, id: Option<u64>) -> Result<Option<Value>> {
        send_routed(&self.stdin, &self.pending, msg, id, Duration::from_secs(60))
    }
}

/// Read replies and hand each to the waiting request with its id (anything
/// else — notifications, server requests, log noise — is ignored).
fn spawn_reader(stdout: Box<dyn std::io::Read + Send>, pending: Pending) {
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(l) = line else { break };
            let Ok(v) = serde_json::from_str::<Value>(&l) else { continue };
            // server→client requests (ping, sampling…) carry their own ids from
            // the server's counter: only responses are routed to our requests
            if v.get("method").is_some() || (v.get("result").is_none() && v.get("error").is_none()) {
                continue;
            }
            let Some(id) = v.get("id").and_then(Value::as_u64) else { continue };
            if let Some(tx) = pending.lock().unwrap().remove(&id) {
                let _ = tx.send(v);
            }
        }
        // the server exited: fail everything still waiting
        pending.lock().unwrap().clear();
    });
}

/// Register for the reply before writing, so a fast reply can't be missed.
fn send_routed(stdin: &Mutex<Box<dyn Write + Send>>, pending: &Pending, msg: &Value, id: Option<u64>, timeout: Duration) -> Result<Option<Value>> {
    let rx = id.map(|id| {
        let (tx, rx) = std::sync::mpsc::channel();
        pending.lock().unwrap().insert(id, tx);
        rx
    });
    {
        let mut w = stdin.lock().unwrap();
        let mut line = serde_json::to_vec(msg)?;
        line.push(b'\n');
        if let Err(e) = w.write_all(&line).and_then(|_| w.flush()) {
            if let Some(id) = id {
                pending.lock().unwrap().remove(&id);
            }
            return Err(anyhow::Error::from(e).context("the server isn't running"));
        }
    }
    let (Some(id), Some(rx)) = (id, rx) else { return Ok(None) };
    match rx.recv_timeout(timeout) {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            pending.lock().unwrap().remove(&id);
            match e {
                std::sync::mpsc::RecvTimeoutError::Timeout => bail!("the server didn't answer in time"),
                std::sync::mpsc::RecvTimeoutError::Disconnected => bail!("the server exited"),
            }
        }
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// `npx` → `/opt/homebrew/bin/npx` when PATH (from Finder) doesn't have it.
fn resolve(cmd: &str) -> String {
    if cmd.contains('/') {
        return stenly_core::util::expand_tilde(cmd).to_string_lossy().into_owned();
    }
    let home = std::env::var("HOME").unwrap_or_default();
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", &format!("{home}/.local/bin"), &format!("{home}/.bun/bin"), &format!("{home}/.cargo/bin")] {
        let p = std::path::Path::new(dir).join(cmd);
        if p.exists() {
            return p.to_string_lossy().into_owned();
        }
    }
    cmd.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sse_and_json() {
        let sse = "event: message\nid: x:1\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"ok\":true}}\n\n";
        assert_eq!(parse_http_body(sse, Some(&json!(2))).unwrap()["result"]["ok"], true);
        assert!(parse_http_body(sse, Some(&json!(3))).is_none());
        assert_eq!(parse_http_body("{\"id\":1,\"result\":{}}", None).unwrap()["id"], 1);
    }

    /// Two requests in flight at once, answered out of order: each gets its own reply.
    #[test]
    fn stdio_routes_concurrent_replies() {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        struct W(std::sync::mpsc::Sender<Vec<u8>>);
        impl Write for W {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                let _ = self.0.send(b.to_vec());
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        // a fake server: collect two requests, answer the second first
        let (out_r, mut out_w) = std::io::pipe().unwrap();
        std::thread::spawn(move || {
            let a: Value = serde_json::from_slice(&req_rx.recv().unwrap()).unwrap();
            let b: Value = serde_json::from_slice(&req_rx.recv().unwrap()).unwrap();
            for v in [&b, &a] {
                writeln!(out_w, "{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}}").unwrap();
                // a server request whose id collides with ours must not be taken for a reply
                writeln!(out_w, "{}", json!({"jsonrpc": "2.0", "id": v["id"], "method": "ping"})).unwrap();
                writeln!(out_w, "{}", json!({"jsonrpc": "2.0", "id": v["id"], "result": {"echo": v["params"]["n"]}})).unwrap();
            }
        });
        let stdin: Mutex<Box<dyn Write + Send>> = Mutex::new(Box::new(W(req_tx)));
        let pending: Pending = Arc::default();
        spawn_reader(Box::new(out_r), pending.clone());
        let t = Duration::from_secs(5);
        let (r1, r2) = std::thread::scope(|sc| {
            let h1 = sc.spawn(|| send_routed(&stdin, &pending, &json!({"jsonrpc":"2.0","id":1,"method":"x","params":{"n":1}}), Some(1), t));
            std::thread::sleep(Duration::from_millis(50));
            let h2 = sc.spawn(|| send_routed(&stdin, &pending, &json!({"jsonrpc":"2.0","id":2,"method":"x","params":{"n":2}}), Some(2), t));
            (h1.join().unwrap().unwrap().unwrap(), h2.join().unwrap().unwrap().unwrap())
        });
        assert_eq!(r1["result"]["echo"], 1);
        assert_eq!(r2["result"]["echo"], 2);
        assert!(pending.lock().unwrap().is_empty());
    }

    #[test]
    fn allow_list_defaults_to_read_only() {
        let ro = RemoteTool { name: "search".into(), title: None, description: String::new(), input_schema: json!({}), annotations: json!({"readOnlyHint": true}) };
        let rw = RemoteTool { name: "execute".into(), annotations: json!({"readOnlyHint": false}), ..ro.clone() };
        let mut s = McpServerConfig { id: "k".into(), ..Default::default() };
        assert!(allowed(&s, &ro) && !allowed(&s, &rw));
        s.allow = vec!["execute".into()];
        assert!(!allowed(&s, &ro) && allowed(&s, &rw));
    }
}
