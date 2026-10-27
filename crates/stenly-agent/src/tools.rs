//! Tools the context agent can call. Built in: past meetings (the same store
//! `stenly-mcp` serves), the notes vault, Notion. Plus every allowed tool of
//! the user's MCP servers, named `<server>__<tool>`.

use crate::llm::ToolSpec;
use crate::mcp::{McpClient, RemoteTool};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use stenly_core::config::Config;
use stenly_mcp::store::{self, Store};

/// Longest tool result handed back to the model.
pub const MAX_RESULT_CHARS: usize = 8_000;

/// What the sources may show for one meeting: only what existed before it.
/// The meeting's own session and note are never background, and when an old
/// meeting is re-gathered, later meetings and their notes are left out too.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    /// The session being prepared.
    pub session: Option<PathBuf>,
    /// Its note in the vault.
    pub note: Option<PathBuf>,
    /// Its start: meetings (and Stenly notes) from then on are hidden.
    pub before: Option<chrono::DateTime<chrono::Local>>,
}

impl Scope {
    fn hides_meeting(&self, m: &store::Meeting) -> bool {
        self.session.as_deref() == Some(m.dir.as_path()) || self.before.is_some_and(|b| m.meta.started >= b)
    }

    /// A meeting note (frontmatter `type: meeting` or `generator: Stenly`)
    /// dated at or after this meeting's start, or the meeting's own note.
    /// Note dates are whole seconds, so a minute's slack covers this meeting.
    fn hides_note(&self, path: &Path, text: &str) -> bool {
        if self.note.as_deref().is_some_and(|n| same_file(n, path)) {
            return true;
        }
        let Some(before) = self.before else { return false };
        let Some(fm) = text.strip_prefix("---\n").and_then(|t| t.split("\n---").next()) else { return false };
        let is_meeting = fm.lines().any(|l| matches!(l.trim(), "generator: Stenly" | "type: meeting"));
        if !is_meeting {
            return false;
        }
        fm.lines()
            .find_map(|l| l.strip_prefix("date:"))
            .and_then(|d| chrono::DateTime::parse_from_rfc3339(d.trim().trim_matches('"')).ok())
            .is_some_and(|d| d >= before - chrono::Duration::seconds(60))
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn call(&self, args: &Value) -> Result<String>;
    /// The source it belongs to ("Past meetings", "Notion", "kody").
    fn source(&self) -> String;
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

fn s<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty())
}

fn n(args: &Value, k: &str, default: usize, max: usize) -> usize {
    args.get(k).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).map(|v| v as usize).unwrap_or(default).clamp(1, max)
}

pub fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n[… truncated, {} more characters]", &text[..cut], text.len() - cut)
}

// ------------------------------------------------------------ past meetings

/// Recorded meetings, minus the one being prepared (its own transcript is not
/// background, and isn't sent to the tools).
pub struct PastMeetings {
    store: Store,
    scope: Scope,
}

impl PastMeetings {
    pub fn new(cfg: &Config, scope: &Scope) -> Self {
        Self { store: Store::new(cfg.clone()), scope: scope.clone() }
    }

    fn meetings(&self) -> Vec<store::Meeting> {
        self.store.meetings().into_iter().filter(|m| !self.scope.hides_meeting(m)).collect()
    }
}

pub struct PastSearch(pub Arc<PastMeetings>);
pub struct PastWith(pub Arc<PastMeetings>);
pub struct PastGet(pub Arc<PastMeetings>);

impl Tool for PastSearch {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "past_meetings_search".into(),
            description: "Full-text search over the user's earlier recorded meetings (titles, summaries, transcripts). All words must appear. Returns snippets with meeting ids.".into(),
            parameters: obj(json!({ "query": { "type": "string" }, "limit": { "type": "integer", "default": 15 } }), &["query"]),
        }
    }
    fn call(&self, args: &Value) -> Result<String> {
        let q = s(args, "query").context("query is required")?;
        let visible: std::collections::HashSet<String> = self.0.meetings().into_iter().map(|m| m.id).collect();
        let limit = n(args, "limit", 15, 40);
        let hits: Vec<_> = self.0.store.search(q, None, self.0.scope.before, usize::MAX).into_iter().filter(|h| visible.contains(&h.meeting_id)).take(limit).collect();
        if hits.is_empty() {
            return Ok(format!("No earlier meetings mention {q:?}."));
        }
        Ok(serde_json::to_string_pretty(&hits)?)
    }
    fn source(&self) -> String {
        "Past meetings".into()
    }
}

impl Tool for PastWith {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "past_meetings_with".into(),
            description: "Earlier recorded meetings with a person or company, newest first: who was there, the summary, decisions and action items. Match by email, email domain (\"acme.com\"), or name.".into(),
            parameters: obj(json!({ "who": { "type": "string", "description": "Email, domain or name." }, "limit": { "type": "integer", "default": 4 } }), &["who"]),
        }
    }
    fn call(&self, args: &Value) -> Result<String> {
        let who = s(args, "who").context("who is required")?.to_lowercase();
        let limit = n(args, "limit", 4, 10);
        let mut out = Vec::new();
        for m in self.0.meetings() {
            // the invite and title first; transcripts (large, maybe on iCloud) only as a last resort
            let att = store::attendees(&m.meta).join(" ").to_lowercase();
            let found = att.contains(&who) || m.meta.title.to_lowercase().contains(&who) || named_in(&m, &who);
            if !found {
                continue;
            }
            let mut v = store::meeting_brief(&m);
            if let Some(sm) = Store::summary(&m) {
                v["summary"] = json!(sm.summary);
                let points: Vec<String> = sm.sections.iter().flat_map(|s| s.bullets.iter().map(|b| format!("{}: {}", s.heading, b.text()))).take(12).collect();
                v["points"] = json!(points);
                v["decisions"] = json!(sm.decisions);
                v["action_items"] = json!(sm.action_items.iter().map(|a| format!("{}{}", a.owner.as_deref().map(|o| format!("{o}: ")).unwrap_or_default(), a.task)).collect::<Vec<_>>());
            }
            out.push(v);
            if out.len() >= limit {
                break;
            }
        }
        if out.is_empty() {
            return Ok(format!("No earlier meetings with {who:?}."));
        }
        Ok(serde_json::to_string_pretty(&out)?)
    }
    fn source(&self) -> String {
        "Past meetings".into()
    }
}

/// Is `who` named in a meeting's summary (attributed points, action-item
/// owners) or, failing that, among its transcript's named voices? The summary
/// is small; the transcript is only read when there is no summary.
fn named_in(m: &store::Meeting, who: &str) -> bool {
    match Store::summary(m) {
        Some(sm) => serde_json::to_string(&sm).unwrap_or_default().to_lowercase().contains(who),
        None => Store::transcript(m).is_some_and(|t| t.speakers.iter().filter_map(|s| s.name.as_deref()).any(|n| n.to_lowercase().contains(who))),
    }
}

impl Tool for PastGet {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "past_meeting_get".into(),
            description: "One earlier meeting's details and full summary by id (from past_meetings_search / past_meetings_with).".into(),
            parameters: obj(json!({ "id": { "type": "string" } }), &["id"]),
        }
    }
    fn call(&self, args: &Value) -> Result<String> {
        let m = self.0.store.resolve(s(args, "id").context("id is required")?)?;
        if self.0.scope.hides_meeting(&m) {
            bail!("that meeting isn't available: only meetings from before this one are background");
        }
        Ok(serde_json::to_string_pretty(&store::meeting_detail(&m))?)
    }
    fn source(&self) -> String {
        "Past meetings".into()
    }
}

// ------------------------------------------------------------ notes vault

/// The Obsidian vault holding the notes folder, else the notes folder itself.
pub fn vault_root(cfg: &Config) -> PathBuf {
    if !cfg.context.vault_dir.trim().is_empty() {
        return stenly_core::util::expand_tilde(&cfg.context.vault_dir);
    }
    let notes = cfg.paths().notes_dir;
    stenly_core::obsidian::vaults().into_iter().map(|v| v.path).find(|v| notes.starts_with(v)).unwrap_or(notes)
}

pub struct VaultSearch {
    pub root: PathBuf,
    pub scope: Scope,
}
pub struct VaultRead {
    pub root: PathBuf,
    pub scope: Scope,
}

fn md_files(root: &Path, max: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "node_modules" {
                continue;
            }
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(p),
                Ok(t) if t.is_file() && name.ends_with(".md") => out.push(p),
                _ => {}
            }
            if out.len() >= max {
                return out;
            }
        }
    }
    out
}

impl Tool for VaultSearch {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "vault_search".into(),
            description: "Search the user's notes vault (Obsidian Markdown: people, companies, projects, daily notes, meeting notes). All words must appear in the file. Notes whose name matches rank first.".into(),
            parameters: obj(json!({ "query": { "type": "string" }, "limit": { "type": "integer", "default": 10 } }), &["query"]),
        }
    }
    fn call(&self, args: &Value) -> Result<String> {
        let q = s(args, "query").context("query is required")?;
        let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
        let mut hits: Vec<(usize, String, String)> = Vec::new();
        for p in md_files(&self.root, 30_000) {
            let Ok(meta) = std::fs::metadata(&p) else { continue };
            if meta.len() > 2_000_000 {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let lower = text.to_lowercase();
            let rel = p.strip_prefix(&self.root).unwrap_or(&p).to_string_lossy().to_string();
            let name = rel.to_lowercase();
            // a person's note often only names them in its file name
            if !words.iter().all(|w| lower.contains(w.as_str()) || name.contains(w.as_str())) || self.scope.hides_note(&p, &text) {
                continue;
            }
            let score = words.iter().filter(|w| name.contains(w.as_str())).count() * 100 + words.iter().map(|w| lower.matches(w.as_str()).count()).sum::<usize>().min(99);
            let pos = words.iter().find_map(|w| lower.find(w.as_str())).unwrap_or(0);
            let start = text[..pos].char_indices().rev().nth(160).map(|(i, _)| i).unwrap_or(0);
            let snippet: String = text[start..].chars().take(360).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
            hits.push((score, rel, snippet));
        }
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let limit = n(args, "limit", 10, 30);
        if hits.is_empty() {
            return Ok(format!("No notes mention {q:?}."));
        }
        Ok(hits.into_iter().take(limit).map(|(_, p, s)| format!("- {p}\n  {s}")).collect::<Vec<_>>().join("\n"))
    }
    fn source(&self) -> String {
        "Notes vault".into()
    }
}

impl Tool for VaultRead {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "vault_read".into(),
            description: "Read one note from the vault by its path (as vault_search shows it).".into(),
            parameters: obj(json!({ "path": { "type": "string" } }), &["path"]),
        }
    }
    fn call(&self, args: &Value) -> Result<String> {
        let rel = s(args, "path").context("path is required")?;
        let p = self.root.join(rel.trim_start_matches('/'));
        let p = p.canonicalize().with_context(|| format!("no note at {rel}"))?;
        let root = self.root.canonicalize().unwrap_or_else(|_| self.root.clone());
        anyhow::ensure!(p.starts_with(&root) && p.extension().is_some_and(|e| e == "md"), "only Markdown notes inside the vault can be read");
        let text = std::fs::read_to_string(&p)?;
        anyhow::ensure!(!self.scope.hides_note(&p, &text), "that is this meeting's own note (or a later meeting's); it isn't background");
        Ok(clip(&text, MAX_RESULT_CHARS))
    }
    fn source(&self) -> String {
        "Notes vault".into()
    }
}

// ------------------------------------------------------------ Notion

/// The Notion token: Settings → Context's own, else a Notion destination's.
pub fn notion_token(cfg: &Config) -> Option<String> {
    stenly_core::secrets::context_secret("notion").or_else(|| {
        cfg.destinations.iter().filter(|d| d.kind == stenly_core::destinations::Kind::Notion).find_map(|d| stenly_core::destinations::secret_for(d, ""))
    })
}

pub struct NotionSearch(pub Arc<stenly_core::destinations::notion::Client>);
pub struct NotionRead(pub Arc<stenly_core::destinations::notion::Client>);

fn notion_title(v: &Value) -> String {
    use stenly_core::destinations::notion::plain;
    if v["title"].is_array() {
        return plain(&v["title"]);
    }
    v["properties"].as_object().into_iter().flatten().find(|(_, p)| p["type"] == "title").map(|(_, p)| plain(&p["title"])).unwrap_or_default()
}

impl Tool for NotionSearch {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "notion_search".into(),
            description: "Search the user's Notion pages and databases shared with Stenly by title (people, companies, CRM records, project docs). Returns ids, titles and URLs; read one with notion_read.".into(),
            parameters: obj(json!({ "query": { "type": "string" }, "limit": { "type": "integer", "default": 10 } }), &["query"]),
        }
    }
    fn call(&self, args: &Value) -> Result<String> {
        let q = s(args, "query").context("query is required")?;
        let r = self.0.call("POST", "/search", Some(&json!({ "query": q, "page_size": n(args, "limit", 10, 25) })))?;
        let rows: Vec<Value> = r["results"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| json!({ "id": p["id"], "type": p["object"], "title": notion_title(p), "url": p["url"], "edited": p["last_edited_time"] }))
            .collect();
        if rows.is_empty() {
            return Ok(format!("Nothing in Notion matches {q:?} (only pages shared with the integration are visible)."));
        }
        Ok(serde_json::to_string_pretty(&rows)?)
    }
    fn source(&self) -> String {
        "Notion".into()
    }
}

impl Tool for NotionRead {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "notion_read".into(),
            description: "Read a Notion page (its properties and text) by id or URL.".into(),
            parameters: obj(json!({ "id": { "type": "string" } }), &["id"]),
        }
    }
    fn call(&self, args: &Value) -> Result<String> {
        use stenly_core::destinations::notion::{parse_id, plain};
        let raw = s(args, "id").context("id is required")?;
        let id = parse_id(raw).unwrap_or_else(|| raw.to_string());
        let page = self.0.call("GET", &format!("/pages/{id}"), None)?;
        let mut out = format!("# {}\n{}\n", notion_title(&page), page["url"].as_str().unwrap_or_default());
        for (k, p) in page["properties"].as_object().into_iter().flatten() {
            let t = p["type"].as_str().unwrap_or_default();
            let v = match t {
                "title" | "rich_text" => plain(&p[t]),
                "select" | "status" => p[t]["name"].as_str().unwrap_or_default().to_string(),
                "multi_select" => p[t].as_array().into_iter().flatten().filter_map(|o| o["name"].as_str()).collect::<Vec<_>>().join(", "),
                "email" | "url" | "phone_number" => p[t].as_str().unwrap_or_default().to_string(),
                "number" => p[t].as_f64().map(|n| n.to_string()).unwrap_or_default(),
                "date" => p[t]["start"].as_str().unwrap_or_default().to_string(),
                "people" => p[t].as_array().into_iter().flatten().filter_map(|u| u["name"].as_str()).collect::<Vec<_>>().join(", "),
                _ => String::new(),
            };
            if !v.trim().is_empty() && t != "title" {
                out.push_str(&format!("{k}: {v}\n"));
            }
        }
        let blocks = self.0.call("GET", &format!("/blocks/{id}/children?page_size=100"), None)?;
        for b in blocks["results"].as_array().into_iter().flatten() {
            let t = b["type"].as_str().unwrap_or_default();
            let text = plain(&b[t]["rich_text"]);
            if text.trim().is_empty() {
                continue;
            }
            let prefix = match t {
                "heading_1" => "# ",
                "heading_2" => "## ",
                "heading_3" => "### ",
                "bulleted_list_item" | "numbered_list_item" | "to_do" => "- ",
                _ => "",
            };
            out.push_str(&format!("{prefix}{text}\n"));
        }
        Ok(clip(&out, MAX_RESULT_CHARS))
    }
    fn source(&self) -> String {
        "Notion".into()
    }
}

// ------------------------------------------------------------ MCP

pub struct McpTool {
    pub client: Arc<McpClient>,
    pub tool: RemoteTool,
    pub name: String,
}

/// `<server>__<tool>`, limited to what function names allow.
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    let clean = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect::<String>();
    let mut n = format!("{}__{}", clean(server), clean(tool));
    n.truncate(64);
    n
}

impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        let mut params = self.tool.input_schema.clone();
        if !params.is_object() {
            params = json!({ "type": "object", "properties": {} });
        }
        // some providers reject these JSON-schema keys in function parameters
        if let Some(o) = params.as_object_mut() {
            o.remove("$schema");
            o.entry("type").or_insert(json!("object"));
        }
        let mut desc = self.tool.description.clone();
        if desc.len() > 1500 {
            desc = clip(&desc, 1500);
        }
        ToolSpec { name: self.name.clone(), description: format!("[{}] {desc}", self.client.id), parameters: params }
    }
    fn call(&self, args: &Value) -> Result<String> {
        let args = if args.is_object() { args.clone() } else { json!({}) };
        Ok(clip(&self.client.call_tool(&self.tool.name, args)?, MAX_RESULT_CHARS))
    }
    fn source(&self) -> String {
        self.client.id.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_search_and_read() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("People")).unwrap();
        std::fs::create_dir_all(d.path().join(".obsidian")).unwrap();
        std::fs::write(d.path().join("People/Sathiren Vale.md"), "Product Lead at Example Labs. Wants a pilot in November.").unwrap();
        std::fs::write(d.path().join("Daily.md"), "Call with example labs went fine").unwrap();
        std::fs::write(d.path().join(".obsidian/x.md"), "example labs hidden").unwrap();
        let s = VaultSearch { root: d.path().into(), scope: Scope::default() };
        let out = s.call(&json!({"query": "example labs"})).unwrap();
        assert!(out.contains("People/Sathiren Vale.md"));
        assert!(!out.contains(".obsidian"));
        let out = s.call(&json!({"query": "sathiren"})).unwrap();
        assert!(out.starts_with("- People/Sathiren Vale.md"), "{out}");
        let r = VaultRead { root: d.path().into(), scope: Scope::default() };
        assert!(r.call(&json!({"path": "People/Sathiren Vale.md"})).unwrap().contains("pilot"));
        assert!(r.call(&json!({"path": "../../etc/passwd"})).is_err());
    }

    #[test]
    fn scope_hides_own_and_later_meeting_notes() {
        use chrono::TimeZone;
        let d = tempfile::tempdir().unwrap();
        let note = |date: &str| format!("---\ntype: meeting\ntitle: \"Example Labs\"\ndate: {date}\ngenerator: Stenly\n---\n\n# Example Labs\nExample Labs pilot");
        std::fs::write(d.path().join("own.md"), "Example Labs pilot notes typed by hand").unwrap();
        // an older note without `generator:`, dated to the second while the meeting began a fraction later
        std::fs::write(d.path().join("same-start.md"), "---\ntype: meeting\ndate: 2026-10-23T07:29:00+09:00\n---\nExample Labs call").unwrap();
        std::fs::write(d.path().join("earlier.md"), note("2026-10-20T10:00:00+09:00")).unwrap();
        std::fs::write(d.path().join("later.md"), note("2026-10-25T10:00:00+09:00")).unwrap();
        std::fs::write(d.path().join("person.md"), "Example Labs: Sathiren's company").unwrap();
        let scope = Scope { session: None, note: Some(d.path().join("own.md")), before: Some(chrono::Local.with_ymd_and_hms(2026, 10, 23, 7, 29, 0).unwrap() + chrono::Duration::milliseconds(92)) };
        let out = VaultSearch { root: d.path().into(), scope: scope.clone() }.call(&json!({"query": "example labs"})).unwrap();
        assert!(out.contains("earlier.md") && out.contains("person.md"), "{out}");
        assert!(!out.contains("own.md") && !out.contains("later.md") && !out.contains("same-start.md"), "{out}");
        let r = VaultRead { root: d.path().into(), scope };
        assert!(r.call(&json!({"path": "own.md"})).is_err());
        assert!(r.call(&json!({"path": "later.md"})).is_err());
        assert!(r.call(&json!({"path": "earlier.md"})).is_ok());
    }

    #[test]
    fn tool_names() {
        assert_eq!(mcp_tool_name("my server", "get.issue"), "my_server__get_issue");
        assert!(mcp_tool_name(&"x".repeat(80), "y").len() <= 64);
    }
}
