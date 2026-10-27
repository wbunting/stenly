//! Stenly's context agent, in plain Rust: a tool-calling loop over any
//! OpenAI-compatible model that gathers background on a meeting (who the people
//! are, their companies, the relationship, what's open) from:
//!
//! - built-in sources: earlier meetings, the notes vault (both on this Mac);
//! - native integrations: Notion;
//! - the user's MCP servers (streamable HTTP with OAuth / a key, or stdio).
//!
//! The result is `context.json` in the session directory, read by the
//! summary pipeline ([`stenly_core::context`]). Gathering never blocks a note:
//! failures leave a note on the (possibly empty) context.

pub mod agent;
pub mod llm;
pub mod mcp;
pub mod oauth;
pub mod tools;

use agent::{Budget, Event, Seed};
use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use stenly_core::calendar::CalendarEvent;
use stenly_core::config::{Config, McpServerConfig};
use stenly_core::context::MeetingContext;
use stenly_core::destinations::Locality;
use stenly_core::session::SessionMeta;
use tools::Tool;

/// One source as Settings shows it.
#[derive(Debug, Clone)]
pub struct SourceStatus {
    pub id: String,
    pub locality: Locality,
    /// Tools offered to the model.
    pub tools: Vec<String>,
    /// Tools the server has but the agent may not call (not read-only / not allowed).
    pub skipped: Vec<String>,
    pub error: Option<String>,
    pub needs_sign_in: bool,
}

/// The tools for one run, plus per-source status and extra guidance.
pub struct Toolset {
    pub tools: Vec<Box<dyn Tool>>,
    pub sources: Vec<SourceStatus>,
    /// Server instructions / user notes, appended to the system prompt.
    pub guidance: String,
}

/// The servers to connect: the configured ones, plus Kody when the Calendar
/// is connected through it and no `kody` entry says otherwise.
pub fn servers(cfg: &Config) -> Vec<McpServerConfig> {
    let mut list = cfg.context.servers.clone();
    if !list.iter().any(|s| s.id == mcp::KODY_ID) && !cfg.calendar.kody_url.trim().is_empty() && stenly_core::kody::KodyAuth::load().is_some() {
        list.push(kody_default());
    }
    list
}

/// Kody as a context source: off until the user turns it on, and then only its
/// read-only `search` unless they allow more.
pub fn kody_default() -> McpServerConfig {
    McpServerConfig {
        id: mcp::KODY_ID.into(),
        enabled: false,
        instructions: "Kody is the user's own automation server. `search` finds capabilities and saved packages (e.g. Gmail / Calendar / Drive helpers); `execute` runs a small ESM module that imports one (only if it is allowed). Use them to read recent email threads with the attendees.".into(),
        ..Default::default()
    }
}

/// A server on this machine (a command, or a loopback URL).
pub fn is_local_server(s: &McpServerConfig) -> bool {
    if !s.command.is_empty() {
        return true;
    }
    let u = s.url.to_lowercase();
    u.starts_with("http://127.0.0.1") || u.starts_with("http://localhost") || u.starts_with("http://[::1]")
}

/// Connect every enabled source. Slow or broken servers are reported, not fatal.
pub fn toolset(cfg: &Config, scope: &tools::Scope) -> Toolset {
    let c = &cfg.context;
    let mut tools: Vec<Box<dyn Tool>> = Vec::new();
    let mut sources = Vec::new();
    let mut guidance = String::new();
    let mut add = |tools: &mut Vec<Box<dyn Tool>>, id: &str, l: Locality, new: Vec<Box<dyn Tool>>| {
        sources.push(SourceStatus { id: id.into(), locality: l, tools: new.iter().map(|t| t.spec().name).collect(), skipped: vec![], error: None, needs_sign_in: false });
        tools.extend(new);
    };
    if c.past_meetings {
        let p = Arc::new(tools::PastMeetings::new(cfg, scope));
        add(&mut tools, "past_meetings", Locality::Local, vec![Box::new(tools::PastWith(p.clone())), Box::new(tools::PastSearch(p.clone())), Box::new(tools::PastGet(p))]);
    }
    if c.vault {
        let root = tools::vault_root(cfg);
        if root.is_dir() {
            add(&mut tools, "vault", Locality::Local, vec![Box::new(tools::VaultSearch { root: root.clone(), scope: scope.clone() }), Box::new(tools::VaultRead { root, scope: scope.clone() })]);
        }
    }
    if c.notion {
        match tools::notion_token(cfg) {
            Some(tok) => {
                let client = Arc::new(stenly_core::destinations::notion::Client::new("", &tok));
                add(&mut tools, "notion", Locality::Cloud, vec![Box::new(tools::NotionSearch(client.clone())), Box::new(tools::NotionRead(client))]);
            }
            None => sources.push(SourceStatus { id: "notion".into(), locality: Locality::Cloud, tools: vec![], skipped: vec![], error: Some("no Notion token".into()), needs_sign_in: true }),
        }
    }
    // MCP servers connect in parallel (each can take a few seconds)
    let enabled: Vec<McpServerConfig> = servers(cfg).into_iter().filter(|s| s.enabled).collect();
    let connected: Vec<(McpServerConfig, Result<(Arc<mcp::McpClient>, Vec<mcp::RemoteTool>)>)> = std::thread::scope(|sc| {
        let hs: Vec<_> = enabled
            .iter()
            .map(|s| {
                let s = s.clone();
                sc.spawn(move || {
                    let r = mcp::McpClient::connect(&s).and_then(|c| {
                        let list = c.list_tools()?;
                        Ok((Arc::new(c), list))
                    });
                    (s, r)
                })
            })
            .collect();
        hs.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    for (s, r) in connected {
        let l = if is_local_server(&s) { Locality::Local } else { Locality::Cloud };
        match r {
            Ok((client, list)) => {
                let (ok, skipped): (Vec<_>, Vec<_>) = list.into_iter().partition(|t| mcp::allowed(&s, t));
                if !client.instructions.trim().is_empty() {
                    guidance.push_str(&format!("[{}] {}\n", s.id, tools::clip(client.instructions.trim(), 1200)));
                }
                if !s.instructions.trim().is_empty() {
                    guidance.push_str(&format!("[{}] {}\n", s.id, s.instructions.trim()));
                }
                let new: Vec<Box<dyn Tool>> = ok
                    .into_iter()
                    .map(|t| Box::new(tools::McpTool { name: tools::mcp_tool_name(&s.id, &t.name), client: client.clone(), tool: t }) as Box<dyn Tool>)
                    .collect();
                sources.push(SourceStatus { id: s.id.clone(), locality: l, tools: new.iter().map(|t| t.spec().name).collect(), skipped: skipped.into_iter().map(|t| t.name).collect(), error: None, needs_sign_in: false });
                tools.extend(new);
            }
            Err(e) => {
                let needs = e.downcast_ref::<mcp::NeedsSignIn>().is_some();
                tracing::warn!(server = %s.id, error = %format!("{e:#}"), "context source unavailable");
                sources.push(SourceStatus { id: s.id.clone(), locality: l, tools: vec![], skipped: vec![], error: Some(format!("{e:#}")), needs_sign_in: needs });
            }
        }
    }
    Toolset { tools, sources, guidance }
}

/// What the agent is told about a recorded (or recording) session.
pub fn seed_for_session(cfg: &Config, meta: &SessionMeta) -> Seed {
    let mut s = seed_for_event(cfg, meta.event.as_ref());
    s.title = meta.title.clone();
    s.when = meta.started.format("%A %Y-%m-%d %H:%M").to_string();
    if s.platform.is_none() {
        s.platform = meta.platform_label().map(str::to_string);
    }
    s
}

/// What the agent is told about a calendar event (before it starts).
pub fn seed_for_event(cfg: &Config, ev: Option<&CalendarEvent>) -> Seed {
    let me = stenly_core::identity::Me::resolve(&cfg.me, ev, None);
    let me_line = match (me.display(), me.emails.first()) {
        (Some(n), Some(e)) => format!("{n} <{e}>"),
        (Some(n), None) => n.to_string(),
        (None, Some(e)) => e.clone(),
        (None, None) => String::new(),
    };
    let Some(ev) = ev else { return Seed { me: me_line, ..Default::default() } };
    let attendees = ev
        .attendees
        .iter()
        .filter(|a| !me.emails.iter().any(|m| a.email.eq_ignore_ascii_case(m)) && !ev.calendar.as_deref().is_some_and(|c| a.email.eq_ignore_ascii_case(c)))
        .map(|a| {
            let name = stenly_core::speakers::person_name(a);
            let who = stenly_core::summary::describe_person(&name, Some(&a.email), &me.emails);
            let declined = if a.status.as_deref() == Some("DECLINED") { ", declined" } else { "" };
            format!("{who} <{}>{declined}", a.email)
        })
        .collect();
    Seed {
        title: ev.title.clone(),
        when: ev.start.with_timezone(&chrono::Local).format("%A %Y-%m-%d %H:%M").to_string(),
        me: me_line,
        attendees,
        organizer: ev.organizer.as_ref().map(|o| format!("{} <{}>", stenly_core::speakers::person_name(o), o.email)),
        description: ev.description.as_deref().map(stenly_core::context::clean_description).unwrap_or_default(),
        platform: ev.link.as_ref().map(|l| l.provider.label().to_string()),
        recurring: ev.recurring,
    }
}

/// The model the agent runs on.
pub fn model(cfg: &Config) -> llm::OpenAiChat {
    llm::OpenAiChat { target: cfg.context.target(&cfg.llm, stenly_core::secrets::llm_api_key), timeout_secs: 120 }
}

/// Gather context for a session directory and save it as `context.json`.
pub fn gather_for_session(cfg: &Config, dir: &Path, on_event: &mut dyn FnMut(Event)) -> Result<MeetingContext> {
    let meta = SessionMeta::load(dir).context("reading meta.json")?;
    let seed = seed_for_session(cfg, &meta);
    let scope = tools::Scope { session: Some(dir.to_path_buf()), note: meta.note_path.clone(), before: Some(meta.started) };
    let ctx = gather(cfg, &seed, &scope, on_event);
    stenly_core::context::save(dir, &ctx)?;
    Ok(ctx)
}

/// Gather context for a seed (no files written). `scope` keeps the meeting's
/// own (and later) material out of the sources.
pub fn gather(cfg: &Config, seed: &Seed, scope: &tools::Scope, on_event: &mut dyn FnMut(Event)) -> MeetingContext {
    let set = toolset(cfg, scope);
    let mut guidance = set.guidance.clone();
    let down: Vec<String> = set.sources.iter().filter_map(|s| s.error.as_ref().map(|e| format!("{} ({e})", s.id))).collect();
    if !down.is_empty() {
        tracing::info!(sources = ?down, "some context sources are unavailable");
        guidance.push_str(&format!("Unavailable right now: {}.\n", down.join("; ")));
    }
    tracing::info!(tools = set.tools.len(), model = %cfg.context.target(&cfg.llm, || None).model, "gathering context");
    let budget = Budget { max_turns: cfg.context.max_turns.max(2), time: Duration::from_secs(cfg.context.budget_secs.max(20)) };
    agent::run(&model(cfg), &set.tools, seed, &guidance, &budget, on_event)
}

/// Sessions ready for context: none gathered yet, or the last run failed.
pub fn needs_context(dir: &Path) -> bool {
    stenly_core::context::load(dir).is_none_or(|c| c.failed())
}
