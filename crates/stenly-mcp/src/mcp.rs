//! MCP (Model Context Protocol) over JSON-RPC 2.0: tools + resources, read-only.
//! Transport-agnostic: [`Server::handle`] takes one parsed message and returns
//! the response (None for notifications).

use crate::store::{self, Store, meeting_brief, meeting_detail, transcript_page};
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

pub const SUPPORTED_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
const URI_PREFIX: &str = "stenly://meetings/";

const INSTRUCTIONS: &str = "Stenly records the user's calls locally (mic = \"Me\", system audio = \"Them\") and \
transcribes them on-device. Use list_meetings / search_meetings to find a call, get_meeting for its summary, \
action items and attendees, and get_transcript for the full speaker-attributed transcript (paged for long calls). \
Meeting ids are session directory names; \"latest\" means the most recent call, and a unique title fragment also works. \
\"Them\" can be several people: the transcript does not separate remote speakers. \
list_recipes / run_recipe run the user's saved prompts (follow-up email, action items by owner, coach me, …) on one meeting \
with their own summary model.";

pub struct Server;

fn ok(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err(id: &Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

impl Server {
    /// Handle one JSON-RPC message (or a batch). Returns None when nothing should be sent back.
    pub fn handle(&self, msg: Value) -> Option<Value> {
        if let Value::Array(batch) = msg {
            let out: Vec<Value> = batch.into_iter().filter_map(|m| self.handle(m)).collect();
            return (!out.is_empty()).then_some(Value::Array(out));
        }
        let id = msg.get("id").cloned();
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // a response from the client (we never send requests) or garbage
            return id.map(|id| err(&id, -32600, "invalid request: missing method"));
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = id else {
            tracing::debug!(method, "notification");
            return None;
        };
        tracing::debug!(method, "request");
        Some(match self.dispatch(method, &params) {
            Ok(v) => ok(&id, v),
            Err(RpcError::MethodNotFound) => err(&id, -32601, format!("method not found: {method}")),
            Err(RpcError::InvalidParams(e)) => err(&id, -32602, e),
            Err(RpcError::Internal(e)) => err(&id, -32603, format!("{e:#}")),
        })
    }

    fn dispatch(&self, method: &str, params: &Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => {
                let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(SUPPORTED_VERSIONS[0]);
                let version = SUPPORTED_VERSIONS.iter().find(|v| **v == requested).copied().unwrap_or(SUPPORTED_VERSIONS[0]);
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {}, "resources": {} },
                    "serverInfo": { "name": "stenly", "title": "Stenly meetings", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_defs() })),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).ok_or_else(|| RpcError::InvalidParams("missing tool name".into()))?;
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                if !tool_defs().iter().any(|t| t["name"] == name) {
                    return Err(RpcError::InvalidParams(format!("unknown tool: {name}")));
                }
                // Tool failures are reported in-band so the model can see and recover.
                Ok(match call_tool(name, &args) {
                    Ok(content) => json!({ "content": [{ "type": "text", "text": content }], "isError": false }),
                    Err(e) => json!({ "content": [{ "type": "text", "text": format!("Error: {e:#}") }], "isError": true }),
                })
            }
            "resources/list" => list_resources().map_err(RpcError::Internal),
            "resources/templates/list" => Ok(json!({ "resourceTemplates": [
                { "uriTemplate": format!("{URI_PREFIX}{{id}}/transcript"), "name": "Meeting transcript", "mimeType": "text/markdown",
                  "description": "Full speaker-attributed transcript of one recorded call." },
                { "uriTemplate": format!("{URI_PREFIX}{{id}}/note"), "name": "Meeting note", "mimeType": "text/markdown",
                  "description": "The Obsidian markdown note (summary, action items, transcript)." },
            ]})),
            "resources/read" => {
                let uri = params.get("uri").and_then(Value::as_str).ok_or_else(|| RpcError::InvalidParams("missing uri".into()))?;
                read_resource(uri).map_err(RpcError::Internal)
            }
            "prompts/list" => Ok(json!({ "prompts": [] })),
            "logging/setLevel" => Ok(json!({})),
            _ => Err(RpcError::MethodNotFound),
        }
    }
}

enum RpcError {
    MethodNotFound,
    InvalidParams(String),
    Internal(anyhow::Error),
}

fn tool_defs() -> Vec<Value> {
    let ro = json!({ "readOnlyHint": true, "openWorldHint": false });
    let id_prop = json!({ "type": "string", "description": "Meeting id from list_meetings/search_meetings, \"latest\", or a unique title fragment." });
    let date = |what: &str| json!({ "type": "string", "description": format!("{what} (YYYY-MM-DD or RFC 3339, local time).") });
    let mut tools = vec![
        json!({
            "name": "list_meetings",
            "title": "List recorded meetings",
            "description": "Recorded calls, newest first, with ids, titles, times, durations and attendees. Filter by date range or a title substring.",
            "inputSchema": { "type": "object", "properties": {
                "since": date("Only meetings starting on/after this"),
                "until": date("Only meetings starting before the end of this day"),
                "title": { "type": "string", "description": "Case-insensitive title substring." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 20 },
            }},
            "annotations": ro,
        }),
        json!({
            "name": "get_meeting",
            "title": "Meeting details + summary",
            "description": "Metadata, calendar event, attendees and the AI summary (topic sections, key points, decisions, action items) for one meeting. Does not include the transcript.",
            "inputSchema": { "type": "object", "properties": { "id": id_prop }, "required": ["id"] },
            "annotations": ro,
        }),
        json!({
            "name": "get_transcript",
            "title": "Meeting transcript",
            "description": "Speaker-attributed transcript (\"Me\" / \"Them\" with mm:ss timestamps). Long calls are paged: if the result says it was truncated, call again with the given offset.",
            "inputSchema": { "type": "object", "properties": {
                "id": id_prop,
                "offset": { "type": "integer", "minimum": 0, "default": 0, "description": "Segment index to start from." },
                "max_chars": { "type": "integer", "minimum": 2000, "maximum": 400000, "default": 80000 },
                "speaker": { "type": "string", "description": "Only this speaker's turns: \"Me\", \"Them\" (everyone remote), or a speaker's name." },
            }, "required": ["id"] },
            "annotations": ro,
        }),
        json!({
            "name": "get_note",
            "title": "Meeting note (markdown)",
            "description": "The meeting's markdown note exactly as it is in the user's Obsidian vault (including any edits they made).",
            "inputSchema": { "type": "object", "properties": { "id": id_prop }, "required": ["id"] },
            "annotations": ro,
        }),
        json!({
            "name": "search_meetings",
            "title": "Search meetings",
            "description": "Full-text search across titles, summaries and transcripts. All words must appear (case-insensitive). Returns matching snippets with meeting id and timestamp.",
            "inputSchema": { "type": "object", "properties": {
                "query": { "type": "string" },
                "since": date("Only meetings starting on/after this"),
                "until": date("Only meetings starting before the end of this day"),
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 30 },
            }, "required": ["query"] },
            "annotations": ro,
        }),
        json!({
            "name": "list_recipes",
            "title": "List recipes",
            "description": "The user's recipes: saved prompts (follow-up email, TL;DR for Slack, action items by owner, coach me, …) that run on one meeting. Use run_recipe to run one.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": ro,
        }),
        json!({
            "name": "run_recipe",
            "title": "Run a recipe on a meeting",
            "description": "Run one of the user's recipes on a meeting with Stenly's summary model (local Ollama by default) and return the Markdown result. Takes from a few seconds to a couple of minutes. You can usually do the same yourself from get_meeting + get_transcript; use this when the user asks for their recipe.",
            "inputSchema": { "type": "object", "properties": {
                "recipe": { "type": "string", "description": "Recipe id or name from list_recipes." },
                "id": id_prop,
                "extra": { "type": "string", "description": "Optional extra instructions appended to the recipe (e.g. what the help doc should focus on)." },
                "save": { "type": "boolean", "default": false, "description": "Keep the result with the meeting so it shows in the Stenly app." },
            }, "required": ["recipe", "id"] },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false },
        }),
        json!({
            "name": "upcoming_meetings",
            "title": "Upcoming calendar meetings",
            "description": "Calendar events in the next N hours from the calendars Stenly watches (useful to prep for a call with past transcripts).",
            "inputSchema": { "type": "object", "properties": {
                "hours": { "type": "integer", "minimum": 1, "maximum": 336, "default": 24 },
            }},
            "annotations": { "readOnlyHint": true, "openWorldHint": true },
        }),
    ];
    if write_tools_enabled() {
        tools.push(json!({
            "name": "share_meeting",
            "title": "Create a private share link",
            "description": "Encrypt a meeting note locally, upload only ciphertext, and return a share link. This is a write action; it is shown only when [mcp] write_tools = true.",
            "inputSchema": { "type": "object", "properties": {
                "id": id_prop,
                "destination": { "type": "string", "description": "Optional configured Stenly share-link destination id or name." }
            }, "required": ["id"] },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": true }
        }));
    }
    tools
}

fn write_tools_enabled() -> bool {
    stenly_core::config::Config::load_or_init().map(|c| c.mcp.write_tools).unwrap_or(false)
}

fn str_arg<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn int_arg(args: &Value, k: &str, default: u64, max: u64) -> u64 {
    args.get(k).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).unwrap_or(default).min(max)
}

type Range = (Option<chrono::DateTime<chrono::Local>>, Option<chrono::DateTime<chrono::Local>>);

fn range(args: &Value) -> Result<Range> {
    let since = str_arg(args, "since").map(|s| store::parse_date(s, false)).transpose()?;
    let until = str_arg(args, "until").map(|s| store::parse_date(s, true)).transpose()?;
    Ok((since, until))
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

pub fn call_tool(name: &str, args: &Value) -> Result<String> {
    let store = Store::from_config()?;
    match name {
        "list_meetings" => {
            let (since, until) = range(args)?;
            let title = str_arg(args, "title").map(str::to_lowercase);
            let limit = int_arg(args, "limit", 20, 500) as usize;
            let all = store.meetings();
            let total = all.len();
            let list: Vec<Value> = all
                .iter()
                .filter(|m| store::in_range(&m.meta, since, until))
                .filter(|m| title.as_ref().is_none_or(|t| m.meta.title.to_lowercase().contains(t.as_str())))
                .take(limit)
                .map(meeting_brief)
                .collect();
            if total == 0 {
                return Ok(format!("No meetings recorded yet (looking in {}).", store.audio_dir.display()));
            }
            Ok(pretty(&json!({ "meetings": list, "total_recorded": total })))
        }
        "get_meeting" => {
            let m = store.resolve(str_arg(args, "id").context("id is required")?)?;
            Ok(pretty(&meeting_detail(&m)))
        }
        "get_transcript" => {
            let m = store.resolve(str_arg(args, "id").context("id is required")?)?;
            let t = Store::transcript(&m).with_context(|| format!("{} has no transcript yet (state: {:?})", m.id, m.meta.state))?;
            let offset = int_arg(args, "offset", 0, u64::MAX) as usize;
            let max = int_arg(args, "max_chars", 80_000, 400_000).max(2_000) as usize;
            Ok(transcript_page(&m, &t, offset, max, str_arg(args, "speaker")).text)
        }
        "get_note" => {
            let m = store.resolve(str_arg(args, "id").context("id is required")?)?;
            let (path, text) = store.note(&m)?;
            Ok(match path {
                Some(p) => format!("<!-- {} -->\n{text}", p.display()),
                None => text,
            })
        }
        "search_meetings" => {
            let q = str_arg(args, "query").context("query is required")?;
            let (since, until) = range(args)?;
            let hits = store.search(q, since, until, int_arg(args, "limit", 30, 200) as usize);
            if hits.is_empty() {
                return Ok(format!("No matches for {q:?}."));
            }
            Ok(pretty(&json!({ "hits": hits })))
        }
        "upcoming_meetings" => {
            let now = chrono::Utc::now();
            let hours = int_arg(args, "hours", 24, 336) as i64;
            let mut c = store.cfg.calendar.clone();
            c.require_meeting_link = false;
            if c.ics_urls.is_empty() && c.kody_url.is_empty() {
                return Ok("No calendar connected in Stenly (Settings → Calendar).".into());
            }
            let (events, errors) = stenly_core::calendar::upcoming(&c, now, now + chrono::Duration::hours(hours));
            let list: Vec<Value> = events
                .iter()
                .map(|e| {
                    json!({
                        "title": e.title,
                        "start": e.start.with_timezone(&chrono::Local).to_rfc3339(),
                        "end": e.end.with_timezone(&chrono::Local).to_rfc3339(),
                        "attendees": e.attendees.iter().map(|a| a.display()).collect::<Vec<_>>(),
                        "platform": e.link.as_ref().map(|l| l.provider.label()),
                        "declined": e.declined,
                        "recurring": e.recurring,
                    })
                })
                .collect();
            Ok(pretty(&json!({ "events": list, "errors": errors })))
        }
        "list_recipes" => {
            let lib = stenly_core::library::Library::recipes();
            let list: Vec<Value> = lib.all().iter().map(|e| json!({ "id": e.id, "name": e.name, "description": e.description })).collect();
            Ok(pretty(&json!({ "recipes": list, "folder": lib.dir.display().to_string() })))
        }
        "share_meeting" => {
            anyhow::ensure!(write_tools_enabled(), "write tools are disabled; set [mcp] write_tools = true in Stenly settings/config first");
            let m = store.resolve(str_arg(args, "id").context("id is required")?)?;
            let destination = match str_arg(args, "destination") {
                Some(name) => store.cfg.destinations.iter().find(|d| d.kind == stenly_core::destinations::Kind::Share && (d.id == name || d.name.eq_ignore_ascii_case(name))).cloned().context("no Stenly share-link destination with that name")?,
                None => stenly_core::destinations::share_config(&store.cfg),
            };
            let dest = stenly_core::destinations::build(&store.cfg, &destination);
            let _ = stenly_core::destinations::send_session(dest.as_ref(), &m.dir, false)?;
            stenly_core::share::link_for(&m.dir, Some(&destination.id))?.context("share key is missing from the Keychain")
        }
        "run_recipe" => {
            let mut recipe = stenly_core::library::Library::recipes().find(str_arg(args, "recipe").context("recipe is required")?)?;
            if let Some(extra) = str_arg(args, "extra") {
                recipe.body = format!("{}\n\nAlso: {extra}", recipe.body.trim());
            }
            let m = store.resolve(str_arg(args, "id").context("id is required")?)?;
            let target = store.cfg.llm.target(stenly_core::secrets::llm_api_key);
            let r = stenly_core::recipe::run(&store.cfg, &target, &recipe, &m.dir)?;
            let text = r.text.clone();
            if args.get("save").and_then(Value::as_bool).unwrap_or(false) {
                stenly_core::recipe::push_result(&m.dir, r)?;
            }
            Ok(text)
        }
        other => Err(anyhow!("unknown tool {other}")),
    }
}

fn list_resources() -> Result<Value> {
    let store = Store::from_config()?;
    let mut out = Vec::new();
    for m in store.meetings().into_iter().take(50) {
        if !m.dir.join("transcript.json").exists() {
            continue;
        }
        let when = m.meta.started.format("%Y-%m-%d %H:%M");
        out.push(json!({
            "uri": format!("{URI_PREFIX}{}/transcript", encode(&m.id)),
            "name": format!("{when} {} — transcript", m.meta.title),
            "mimeType": "text/markdown",
        }));
        out.push(json!({
            "uri": format!("{URI_PREFIX}{}/note", encode(&m.id)),
            "name": format!("{when} {} — note", m.meta.title),
            "mimeType": "text/markdown",
        }));
    }
    Ok(json!({ "resources": out }))
}

fn read_resource(uri: &str) -> Result<Value> {
    let rest = uri.strip_prefix(URI_PREFIX).with_context(|| format!("unknown resource {uri}"))?;
    let (id, kind) = rest.rsplit_once('/').with_context(|| format!("bad resource uri {uri}"))?;
    let id = decode(id);
    let store = Store::from_config()?;
    let m = store.resolve(&id)?;
    let text = match kind {
        "transcript" => {
            let t = Store::transcript(&m).context("no transcript yet")?;
            transcript_page(&m, &t, 0, usize::MAX, None).text
        }
        "note" => store.note(&m)?.1,
        _ => anyhow::bail!("unknown resource kind {kind:?}"),
    };
    Ok(json!({ "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": text }] }))
}

/// Percent-encode everything but unreserved chars (ids contain spaces).
pub fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipe_tools_are_listed() {
        let defs = tool_defs();
        let names: Vec<&str> = defs.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"list_recipes") && names.contains(&"run_recipe"));
        let run = defs.iter().find(|t| t["name"] == "run_recipe").unwrap();
        assert_eq!(run["inputSchema"]["required"], json!(["recipe", "id"]));
        assert_eq!(run["annotations"]["readOnlyHint"], json!(false));
    }
}
