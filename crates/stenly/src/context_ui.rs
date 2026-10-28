//! Context in the app: Settings → Context (the agent's model, built-in
//! sources, integrations and the user's own MCP servers), and the Context
//! card on a meeting (what was found, re-gather).
//!
//! Every source carries the same "On this Mac" / "Sends to …" label the
//! destinations use, so it's clear what leaves the machine.

use crate::controller::Controller;
use crate::destinations_ui::privacy_label;
use crate::icons::{self, icon};
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, Btn};
use gpui::{AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement, SharedString, Styled, Subscription, Window, div, prelude::*, px};
use stenly_core::config::McpServerConfig;
use stenly_core::destinations::Locality;

pub struct ContextUi {
    controller: Entity<Controller>,
    /// "Add Server" field: a URL or a command line.
    add: Entity<TextInput>,
    notion_token: Entity<TextInput>,
    model: Entity<TextInput>,
    endpoint: Entity<TextInput>,
    /// Server whose tool list is expanded.
    expanded: Option<String>,
    _subs: Vec<Subscription>,
}

impl ContextUi {
    pub fn new(controller: Entity<Controller>, cx: &mut Context<Self>) -> Self {
        let add = cx.new(|cx| TextInput::new(cx, "https://example.com/mcp  or  npx -y @acme/mcp-server"));
        let notion_token = cx.new(|cx| TextInput::new(cx, "Notion integration token (ntn_…)").masked());
        let (m, e) = {
            let c = &controller.read(cx).config.context;
            (c.model.clone(), c.endpoint.clone())
        };
        let model = cx.new(|cx| {
            let mut t = TextInput::new(cx, "Model, e.g. claude-sonnet-4-6");
            t.set_text(m, cx);
            t
        });
        let endpoint = cx.new(|cx| {
            let mut t = TextInput::new(cx, "https://api.example.com/v1");
            t.set_text(e, cx);
            t
        });
        let mut subs = vec![cx.observe(&controller, |_, _, cx| cx.notify())];
        subs.push(cx.subscribe(&add, |this: &mut Self, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Submit) {
                this.add_server(cx);
            }
        }));
        subs.push(cx.subscribe(&notion_token, |this: &mut Self, input, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Submit) {
                let tok = input.read(cx).text().trim().to_string();
                if !tok.is_empty() {
                    let r = stenly_core::secrets::set_context_secret("notion", &tok);
                    input.update(cx, |t, cx| t.set_text("", cx));
                    this.controller.update(cx, |c, cx| {
                        match r {
                            Ok(()) => c.set("context.notion", true, cx),
                            Err(e) => c.last_error = Some(format!("Couldn't save the token: {e:#}")),
                        }
                        c.check_context_sources(cx);
                    });
                }
            }
        }));
        for (input, key) in [(model.clone(), "context.model"), (endpoint.clone(), "context.endpoint")] {
            subs.push(cx.subscribe(&input, move |this: &mut Self, input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Submit) {
                    let v = input.read(cx).text().trim().to_string();
                    this.controller.update(cx, |c, cx| c.set(key, v.as_str(), cx));
                }
            }));
        }
        Self { controller, add, notion_token, model, endpoint, expanded: None, _subs: subs }
    }

    /// Check sources the first time the page is shown.
    pub fn on_open(&mut self, cx: &mut Context<Self>) {
        let c = self.controller.read(cx);
        if c.context.sources.is_none() && !c.context.checking {
            self.controller.update(cx, |c, cx| c.check_context_sources(cx));
        }
    }

    fn servers(&self, cx: &Context<Self>) -> Vec<McpServerConfig> {
        stenly_agent::servers(&self.controller.read(cx).config)
    }

    fn save(&mut self, list: Vec<McpServerConfig>, cx: &mut Context<Self>) {
        self.controller.update(cx, |c, cx| c.save_context_servers(list, cx));
    }

    fn edit_server(&mut self, id: &str, cx: &mut Context<Self>, f: impl FnOnce(&mut McpServerConfig)) {
        let mut list = self.servers(cx);
        if let Some(s) = list.iter_mut().find(|s| s.id == id) {
            f(s);
        }
        self.save(list, cx);
    }

    fn add_server(&mut self, cx: &mut Context<Self>) {
        let raw = self.add.read(cx).text().trim().to_string();
        if raw.is_empty() {
            return;
        }
        let mut list = self.servers(cx);
        let s = server_from_input(&raw, &list);
        list.push(s);
        self.add.update(cx, |t, cx| t.set_text("", cx));
        self.save(list, cx);
    }

    fn import(&mut self, cx: &mut Context<Self>) {
        let mut list = self.servers(cx);
        let found = import_candidates();
        let mut added = 0;
        for mut s in found {
            if list.iter().any(|x| x.id == s.id || (!s.url.is_empty() && x.url == s.url) || (!s.command.is_empty() && x.command == s.command)) {
                continue;
            }
            s.enabled = false;
            list.push(s);
            added += 1;
        }
        self.controller.update(cx, |c, cx| {
            if added == 0 {
                c.last_error = Some("No new MCP servers found in Claude, Cursor, pi or Codex settings.".into());
                cx.notify();
            }
        });
        if added > 0 {
            self.save(list, cx);
        }
    }

    fn page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let c = self.controller.read(cx);
        let cfg = c.config.clone();
        let st = &c.context;
        let checking = st.checking;
        let sources = st.sources.clone().unwrap_or_default();
        let signing = st.signing_in.clone();
        let facts = st.facts.clone().unwrap_or_default();
        let ctrl = self.controller.clone();
        let status_of = |id: &str| sources.iter().find(|s| s.id == id).cloned();

        // ---- general
        let enabled = cfg.context.enabled;
        let target = cfg.context.target(&cfg.llm, || None);
        let provider = match cfg.context.provider.as_str() {
            "api" => 1,
            "custom" => 2,
            _ => 0,
        };
        let c1 = ctrl.clone();
        let mut general = vec![
            ui::row(
                None,
                "Gather context",
                Some("Before each calendar meeting, and before notes are written, look up who's in it and what's open with them.".into()),
                ui::toggle("ctx-on", enabled, move |_, _, cx| c1.update(cx, |c, cx| c.set("context.enabled", !enabled, cx))),
            )
            .into_any_element(),
        ];
        let c2 = ctrl.clone();
        general.push(
            ui::row(
                None,
                "Model",
                Some(SharedString::from(match provider {
                    0 if target.cli.is_some() => format!("{} can't call tools, so it can't gather context. Choose Hosted API or Custom.", target.cli.as_ref().map(|c| c.tool.label()).unwrap_or_default()),
                    0 => format!("Same as summaries · {}", if target.model.is_empty() { "not set up".into() } else { target.model.clone() }),
                    1 => format!("Your hosted API · {}", if target.model.is_empty() { "not set up".into() } else { target.model.clone() }),
                    _ => "Needs a model that calls tools reliably (small local models often don't)".into(),
                })),
                ui::popup("ctx-provider", &[(0u8, "Same as Summaries"), (1, "Hosted API"), (2, "Custom…")], provider, move |v, cx| {
                    c2.update(cx, |c, cx| c.set("context.provider", ["summary", "api", "custom"][v as usize], cx))
                }),
            )
            .into_any_element(),
        );
        if provider == 2 {
            general.push(field_row("Endpoint", ui::text_field(self.endpoint.clone())));
            general.push(field_row("Model", ui::text_field(self.model.clone())));
        }
        let c3 = ctrl.clone();
        let mins = cfg.context.before_mins;
        general.push(
            ui::row(
                None,
                "Start before a meeting",
                None,
                ui::popup("ctx-before", &[(5i64, "5 minutes"), (10, "10 minutes"), (30, "30 minutes"), (60, "1 hour")], [5, 10, 30, 60].into_iter().min_by_key(|m| (m - mins).abs()).unwrap_or(10), move |v, cx| {
                    c3.update(cx, |c, cx| c.set("context.before_mins", v, cx))
                }),
            )
            .into_any_element(),
        );

        // ---- built in
        let toggle_row = |id: &'static str, title: &str, desc: String, l: Locality, on: bool, key: &'static str| {
            let c = ctrl.clone();
            source_row(title, desc, l, ui::toggle(id, on, move |_, _, cx| c.update(cx, |c, cx| c.set(key, !on, cx))).into_any_element())
        };
        let vault = facts.vault_root.clone();
        let builtin = vec![
            toggle_row("ctx-past", "Past meetings", "Notes and transcripts from earlier meetings with the same people".into(), Locality::Local, cfg.context.past_meetings, "context.past_meetings"),
            toggle_row(
                "ctx-vault",
                "Notes vault",
                if vault.as_os_str().is_empty() { "People, company and project notes in your vault".into() } else { format!("People, company and project notes in {}", stenly_core::util::collapse_tilde(&vault)) },
                Locality::Local,
                cfg.context.vault,
                "context.vault",
            ),
        ];

        // ---- integrations
        let has_notion = facts.has_notion;
        let notion_trailing: AnyElement = if has_notion {
            let c = ctrl.clone();
            let on = cfg.context.notion;
            ui::toggle("ctx-notion", on, move |_, _, cx| c.update(cx, |c, cx| c.set("context.notion", !on, cx))).into_any_element()
        } else {
            div().w(px(260.)).child(ui::text_field(self.notion_token.clone())).into_any_element()
        };
        let notion_desc = if has_notion {
            match status_of("notion") {
                Some(s) if s.error.is_some() => sentence(s.error.as_deref().unwrap_or_default()),
                _ if !facts.own_notion => "Searches pages shared with your Notion destination's integration".into(),
                _ => "Searches pages and databases shared with the integration".into(),
            }
        } else {
            "Paste an internal integration token and press Return; share the pages it may read with it in Notion".into()
        };
        let integrations = vec![source_row("Notion", notion_desc, Locality::Cloud, notion_trailing)];

        // ---- MCP servers
        let servers = self.servers(cx);
        let mut server_rows: Vec<AnyElement> = Vec::new();
        for s in &servers {
            let st = status_of(&s.id);
            let l = if stenly_agent::is_local_server(s) { Locality::Local } else { Locality::Cloud };
            let kody = s.id == stenly_agent::mcp::KODY_ID;
            let what = if kody {
                "Uses your Calendar sign-in".to_string()
            } else if !s.command.is_empty() {
                s.command.join(" ")
            } else {
                s.url.clone()
            };
            let detail = match (&st, s.enabled) {
                (_, false) => what.clone(),
                (Some(x), true) if x.error.is_some() => sentence(x.error.as_deref().unwrap_or_default()),
                (Some(x), true) => {
                    let n = x.tools.len();
                    let skipped = if x.skipped.is_empty() { String::new() } else { format!(" · {} not allowed", x.skipped.len()) };
                    format!("{n} tool{}{skipped}", if n == 1 { "" } else { "s" })
                }
                (None, true) => what.clone(),
            };
            let needs_sign_in = st.as_ref().is_some_and(|x| x.needs_sign_in) || (s.enabled && !kody && !s.url.is_empty() && !facts.signed_in.contains(&s.id) && st.as_ref().is_some_and(|x| x.error.is_some()));
            let id = s.id.clone();
            let mut trailing = div().flex().items_center().gap(px(8.));
            if needs_sign_in && !kody {
                let (c, id2, url) = (ctrl.clone(), id.clone(), s.url.clone());
                trailing = trailing.child(if signing.contains(&id) {
                    ui::pending(SharedString::from(format!("ctx-si-{id}")), "Waiting for browser…").into_any_element()
                } else {
                    ui::button(SharedString::from(format!("ctx-signin-{id}")), "Sign In…", Btn::Accent).on_click(move |_, _, cx| c.update(cx, |c, cx| c.sign_in_context_server(id2.clone(), url.clone(), cx))).into_any_element()
                });
            }
            let me = cx.entity();
            let id3 = id.clone();
            // One inline action: while it needs signing in, that's Sign In… (its tools can't load anyway).
            trailing = trailing.when(!needs_sign_in || kody, |t| t.child(ui::button(SharedString::from(format!("ctx-tools-{id}")), if self.expanded.as_deref() == Some(id.as_str()) { "Done" } else { "Tools…" }, Btn::Ghost).on_click(move |_, _, cx| {
                me.update(cx, |s, cx| {
                    s.expanded = if s.expanded.as_deref() == Some(id3.as_str()) { None } else { Some(id3.clone()) };
                    cx.notify();
                })
            })));
            let me = cx.entity();
            let (id4, on) = (id.clone(), s.enabled);
            trailing = trailing.child(ui::toggle(SharedString::from(format!("ctx-srv-{id}")), on, move |_, _, cx| me.update(cx, |s, cx| s.edit_server(&id4, cx, |x| x.enabled = !on))));
            server_rows.push(source_row(&s.id, detail, l, trailing.into_any_element()));
            if self.expanded.as_deref() == Some(s.id.as_str()) {
                server_rows.push(self.tools_panel(s, st.as_ref(), cx));
            }
        }
        server_rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .py(px(9.))
                .child(ui::text_field(self.add.clone()))
                .child(ui::button("ctx-add", "Add Server", Btn::Ghost).on_click(cx.listener(|this, _, _, cx| this.add_server(cx))))
                .child(ui::button("ctx-import", "Import…", Btn::Ghost).on_click(cx.listener(|this, _, _, cx| this.import(cx))))
                .into_any_element(),
        );

        let c4 = ctrl.clone();
        div()
            .flex()
            .flex_col()
            .child(ui::caption("Before Each Meeting"))
            .child(ui::card(general))
            .child(ui::caption("Built In"))
            .child(ui::card(builtin))
            .child(ui::caption("Integrations"))
            .child(ui::card(integrations))
            .child(
                div()
                    .flex()
                    .items_end()
                    .child(div().flex_1().child(ui::caption("Your MCP Servers")))
                    .child(div().pb(px(4.)).child(if checking {
                        ui::pending("ctx-check", "Checking…").into_any_element()
                    } else {
                        ui::button("ctx-recheck", "Check Sources", Btn::Ghost).on_click(move |_, _, cx| c4.update(cx, |c, cx| c.check_context_sources(cx))).into_any_element()
                    })),
            )
            .child(ui::card(server_rows))
            .child(
                div()
                    .pt(px(10.))
                    .px(px(16.))
                    .text_size(px(theme::SUBHEAD))
                    .line_height(px(16.))
                    .text_color(theme::muted())
                    .child("Only read-only tools are used unless you allow others under Tools…. Your transcript is never sent to these sources; the model reads what they return."),
            )
            .into_any_element()
    }

    /// Per-tool allow list for one server.
    fn tools_panel(&self, s: &McpServerConfig, st: Option<&stenly_agent::SourceStatus>, cx: &mut Context<Self>) -> AnyElement {
        let Some(st) = st else {
            return div().py(px(8.)).text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child("Turn it on and choose Check Sources to see its tools.").into_any_element();
        };
        if let Some(e) = &st.error {
            return div().py(px(8.)).text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(sentence(e)).into_any_element();
        }
        let prefix = format!("{}__", stenly_agent::tools::mcp_tool_name(&s.id, "").trim_end_matches('_'));
        let mut names: Vec<(String, bool)> = st.tools.iter().map(|t| (t.strip_prefix(&prefix).unwrap_or(t).to_string(), true)).collect();
        names.extend(st.skipped.iter().map(|t| (t.clone(), false)));
        names.sort();
        let mut col = div().flex().flex_col().pb(px(8.)).pl(px(12.));
        for (name, on) in names {
            let me = cx.entity();
            let (id, n) = (s.id.clone(), name.clone());
            let current = s.allow.clone();
            let ro_default = s.allow.is_empty();
            let enabled_now: Vec<String> = st.tools.iter().map(|t| t.strip_prefix(&prefix).unwrap_or(t).to_string()).collect();
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(30.))
                    .child(div().flex_1().text_size(px(theme::BODY)).child(name.clone()))
                    .child(div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(if on { "allowed" } else if ro_default { "may change things" } else { "off" }))
                    .child(ui::toggle(SharedString::from(format!("ctx-t-{}-{}", s.id, name)), on, move |_, _, cx| {
                        me.update(cx, |this, cx| {
                            // first edit: start from what's allowed now (the read-only set)
                            let mut allow = if current.is_empty() { enabled_now.clone() } else { current.clone() };
                            if on {
                                allow.retain(|a| a != &n);
                            } else if !allow.contains(&n) {
                                allow.push(n.clone());
                            }
                            // an empty list would mean "read-only default" again; keep a marker instead
                            if allow.is_empty() {
                                allow.push("-".into());
                            }
                            this.edit_server(&id, cx, |x| x.allow = allow);
                        })
                    })),
            );
        }
        col.into_any_element()
    }
}

impl gpui::Render for ContextUi {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.page(cx)
    }
}

fn field_row(label: &str, field: impl IntoElement) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(16.))
        .min_h(px(44.))
        .child(div().w(px(110.)).text_size(px(theme::BODY)).child(label.to_string()))
        .child(div().flex_1().flex().child(field))
        .into_any_element()
}

fn source_row(title: &str, desc: String, l: Locality, trailing: AnyElement) -> AnyElement {
    let caption = match l {
        Locality::Local => stenly_core::destinations::privacy_caption(l, ""),
        Locality::Cloud => "Over the internet".into(),
    };
    div()
        .flex()
        .items_center()
        .gap(px(16.))
        .min_h(px(52.))
        .py(px(9.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(div().flex().items_center().gap(px(8.)).child(div().text_size(px(theme::BODY)).child(title.to_string())).child(privacy_label(l, caption)))
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(desc)),
        )
        .child(div().flex_shrink_0().flex().items_center().gap(px(8.)).child(trailing))
        .into_any_element()
}

fn sentence(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// A server from what was typed: a URL, or a command line.
pub fn server_from_input(raw: &str, existing: &[McpServerConfig]) -> McpServerConfig {
    let raw = raw.trim();
    let is_url = raw.starts_with("http://") || raw.starts_with("https://");
    let base = if is_url {
        let host = raw.split("://").nth(1).unwrap_or(raw).split(['/', ':']).next().unwrap_or("server");
        // mcp.linear.app → linear; api.acme.com → acme
        let parts: Vec<&str> = host.split('.').filter(|p| !matches!(*p, "mcp" | "api" | "www" | "com" | "app" | "io" | "ai" | "dev" | "net" | "org")).collect();
        parts.first().copied().unwrap_or(host).to_string()
    } else {
        // npx -y @acme/foo-mcp → foo-mcp
        let last = raw.split_whitespace().filter(|w| !w.starts_with('-')).last().unwrap_or("server");
        last.rsplit('/').next().unwrap_or(last).trim_start_matches('@').to_string()
    };
    let mut id: String = base.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect();
    if id.is_empty() {
        id = "server".into();
    }
    let mut unique = id.clone();
    let mut n = 2;
    while existing.iter().any(|s| s.id == unique) {
        unique = format!("{id}-{n}");
        n += 1;
    }
    McpServerConfig {
        id: unique,
        enabled: true,
        url: if is_url { raw.to_string() } else { String::new() },
        command: if is_url { vec![] } else { raw.split_whitespace().map(str::to_string).collect() },
        ..Default::default()
    }
}

/// MCP servers configured in Claude Desktop / Claude Code / Cursor / pi.
pub fn import_candidates() -> Vec<McpServerConfig> {
    let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) else { return vec![] };
    let files = [
        home.join(".pi/agent/mcp.json"),
        home.join(".cursor/mcp.json"),
        home.join(".claude.json"),
        home.join("Library/Application Support/Claude/claude_desktop_config.json"),
        home.join(".config/Claude/claude_desktop_config.json"),
    ];
    let mut out: Vec<McpServerConfig> = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        for (name, s) in v["mcpServers"].as_object().into_iter().flatten() {
            // Stenly's own server is not context for itself
            if name == "stenly" || out.iter().any(|o| &o.id == name) {
                continue;
            }
            let url = s["url"].as_str().unwrap_or_default().to_string();
            let mut command: Vec<String> = s["command"].as_str().map(|c| vec![c.to_string()]).unwrap_or_default();
            command.extend(s["args"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(str::to_string)));
            if url.is_empty() && command.is_empty() {
                continue;
            }
            let env = s["env"].as_object().map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect()).unwrap_or_default();
            out.push(McpServerConfig { id: name.clone(), enabled: false, url, command: if s["url"].is_string() { vec![] } else { command }, env, ..Default::default() });
        }
    }
    out
}

// ---------------------------------------------------------------- meeting view

/// The Context card on a meeting: what the agent found, and Gather / Re-gather.
pub fn meeting_card(controller: &Entity<Controller>, dir: &std::path::Path, meta: &stenly_core::session::SessionMeta, open: bool, on_toggle: impl Fn(&mut gpui::App) + 'static, cx: &mut gpui::App) -> Option<AnyElement> {
    let c = controller.read(cx);
    let key = dir.to_string_lossy().to_string();
    let running = c.context.is_running(&key);
    let log: Vec<String> = c.context.log(&key).map(|l| l.to_vec()).unwrap_or_default();
    let err = c.context.error(&key).map(str::to_string);
    let enabled = c.config.context.enabled;
    // read in the background when the meeting opens (Controller::load_meeting_context)
    let saved = c.context.loaded.get(dir).cloned().flatten();
    let invite = stenly_core::context::from_invite(meta);
    if saved.is_none() && !running && !enabled && invite.is_empty() {
        return None;
    }
    let ctrl = controller.clone();
    let d = dir.to_path_buf();
    let action: AnyElement = if running {
        ui::pending("ctx-gathering", log.last().cloned().unwrap_or_else(|| "Gathering context…".into())).into_any_element()
    } else {
        ui::button("ctx-gather", if saved.is_some() { "Gather Again" } else { "Gather Context" }, Btn::Ghost)
            .on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.gather_context(d.clone(), true, cx)))
            .into_any_element()
    };
    let shown = saved.clone().unwrap_or(invite);
    let summary_line = match (&saved, shown.is_empty()) {
        (Some(s), _) => {
            let g = s.gathered.as_ref();
            let n = s.people.len();
            let mut parts = vec![];
            if n > 0 {
                parts.push(format!("{n} {}", if n == 1 { "person" } else { "people" }));
            }
            if !s.companies.is_empty() {
                parts.push(format!("{} {}", s.companies.len(), if s.companies.len() == 1 { "company" } else { "companies" }));
            }
            if !s.terms.is_empty() {
                parts.push(format!("{} terms", s.terms.len()));
            }
            let what = if parts.is_empty() { "Nothing found".to_string() } else { parts.join(" · ") };
            match g {
                Some(g) => format!("{what} · {} lookups with {}", g.steps.len(), g.model),
                None => what,
            }
        }
        (None, false) => "From the invite".into(),
        (None, true) => "Not gathered yet".into(),
    };
    let header = div()
        .id("ctx-card-head")
        .flex()
        .items_center()
        .gap(px(6.))
        .child(icon(if open { icons::CHEVRON_DOWN } else { icons::CHEVRON_RIGHT }, 10., theme::muted()))
        .child(div().text_size(px(theme::BODY)).font_weight(FontWeight::SEMIBOLD).child("Context"))
        .child(div().text_size(px(theme::BODY)).text_color(theme::muted()).child(summary_line))
        .on_click(move |_, _, cx| on_toggle(cx));
    let mut card = div().mt(px(14.)).px(px(14.)).py(px(10.)).rounded(px(10.)).bg(theme::card()).flex().flex_col().gap(px(6.)).child(div().flex().items_center().child(div().flex_1().child(header)).child(action));
    if let Some(e) = err.filter(|_| !running) {
        card = card.child(div().flex().items_center().gap(px(5.)).text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(icon(icons::ALERT, 11., theme::amber())).child(sentence(&e)));
    }
    if open {
        let line = |t: String| div().text_size(px(theme::BODY)).line_height(px(19.)).text_color(theme::text()).child(t);
        let label = |t: &str| div().pt(px(6.)).text_size(px(theme::SUBHEAD)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted()).child(t.to_string());
        if !shown.brief.trim().is_empty() {
            card = card.child(line(shown.brief.trim().to_string()));
        }
        if !shown.people.is_empty() {
            card = card.child(label("People"));
            for p in &shown.people {
                card = card.child(line(format!("• {}", p.describe())));
            }
        }
        if !shown.companies.is_empty() {
            card = card.child(label("Companies"));
            for co in &shown.companies {
                card = card.child(line(format!("• {}{}", co.name, co.notes.as_deref().map(|n| format!(": {n}")).unwrap_or_default())));
            }
        }
        if !shown.open_threads.is_empty() {
            card = card.child(label("Open going in"));
            for t in &shown.open_threads {
                card = card.child(line(format!("• {t}")));
            }
        }
        if !shown.terms.is_empty() {
            card = card.child(label("Spellings")).child(div().text_size(px(theme::BODY)).text_color(theme::muted()).child(shown.terms.join(", ")));
        }
        if let Some(g) = shown.gathered.as_ref().filter(|g| !g.steps.is_empty()) {
            let tools: Vec<String> = g.steps.iter().map(|s| format!("{}{}", s.tool, if s.ok { "" } else { " ✗" })).collect();
            card = card.child(div().pt(px(4.)).text_size(px(theme::SUBHEAD)).text_color(theme::faint()).child(format!("Looked at: {}", tools.join(", "))));
        }
    }
    Some(card.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_from_input() {
        let s = server_from_input("https://mcp.linear.app/mcp", &[]);
        assert_eq!((s.id.as_str(), s.url.as_str()), ("linear", "https://mcp.linear.app/mcp"));
        let s = server_from_input("npx -y @modelcontextprotocol/server-github", &[McpServerConfig { id: "server-github".into(), ..Default::default() }]);
        assert_eq!(s.id, "server-github-2");
        assert_eq!(s.command, vec!["npx", "-y", "@modelcontextprotocol/server-github"]);
        assert!(s.url.is_empty());
    }
}
