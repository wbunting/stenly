//! Destinations in the app: the "Send to…" menu and status line on a
//! meeting, and Settings → Destinations (add / edit / test / remove).
//!
//! Destination menus use service logos; privacy captions retain neutral
//! laptop/cloud icons so identity and data locality stay distinct.

use crate::controller::Controller;
use crate::icons::{self, icon};
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, Btn, MenuEntry};
use chrono::Local;
use gpui::{AnyElement, App, ClipboardItem, Context, Entity, IntoElement, ParentElement, PathPromptOptions, Render, SharedString, Styled, Subscription, Window, div, prelude::*, px};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use stenly_core::destinations::{self as dest, Destination, DestinationConfig, Kind, Locality, MatchBy, RelationMatch, Secret, SendRecord, notion};
use stenly_core::session::SessionMeta;

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Probe {
    #[default]
    Idle,
    Running,
    Ok(String),
    Failed(String),
}

/// The add/edit form.
struct Editor {
    cfg: DestinationConfig,
    is_new: bool,
    secret_saved: bool,
    signing_saved: bool,
    schema: Option<notion::Schema>,
    related: Option<notion::Schema>,
    loading: Option<&'static str>,
    test: Probe,
}

pub struct DestUi {
    controller: Entity<Controller>,
    /// (session dir, destination id) sends in flight.
    sending: HashSet<(PathBuf, String)>,
    /// Last Test result per destination id (Settings list).
    tests: HashMap<String, Probe>,
    editor: Option<Editor>,
    name: Entity<TextInput>,
    secret: Entity<TextInput>,
    signing: Entity<TextInput>,
    database: Entity<TextInput>,
    titles: Entity<TextInput>,
    calendars: Entity<TextInput>,
    _subs: Vec<Subscription>,
}

pub fn locality_icon(l: Locality) -> icons::Sym {
    match l {
        Locality::Local => icons::LAPTOP,
        Locality::Cloud => icons::CLOUD,
    }
}

const NOTION_LOGO: &[u8] = include_bytes!("../assets/services/notion.png");
const SLACK_LOGO: &[u8] = include_bytes!("../assets/services/slack.png");

/// Shared by Add Destination and Send to…; bundled logos also work offline.
fn destination_entry(kind: Kind, title: String, subtitle: String) -> MenuEntry {
    let (image, symbol) = match kind {
        Kind::Notion => (Some(NOTION_LOGO), "cloud"),
        Kind::Slack => (Some(SLACK_LOGO), "cloud"),
        Kind::Webhook => (None, "link"),
        Kind::Share => (None, "link"),
        Kind::Folder => (None, "folder"),
    };
    MenuEntry { title, subtitle: Some(subtitle), image, symbol: Some(symbol), enabled: true, ..Default::default() }
}

/// "☁ Sends to Notion" / "💻 On this Mac": the label shown everywhere.
pub fn privacy_label(l: Locality, caption: impl Into<SharedString>) -> gpui::Div {
    div().flex().items_center().gap(px(4.)).text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(icon(locality_icon(l), 10., theme::muted())).child(caption.into())
}

/// Errors read as sentences in the UI ("add the token…" → "Add the token…").
fn sentence(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn split_list(s: &str) -> Vec<String> {
    s.split([',', ';']).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect()
}

/// Open what a send produced: a web page, or the written file.
fn open_result(r: &SendRecord, cx: &mut App) {
    match (&r.url, &r.remote_id) {
        (Some(u), Some(path)) if u.starts_with("file://") => cx.open_with_system(Path::new(path)),
        (Some(u), _) => cx.open_url(u),
        _ => {}
    }
}

/// Status notices for the window's bottom-right corner.
pub enum DestEvent {
    Notice(SharedString, crate::toast::ToastKind),
}

impl gpui::EventEmitter<DestEvent> for DestUi {}

impl DestUi {
    pub fn new(controller: Entity<Controller>, cx: &mut Context<Self>) -> Self {
        let field = |cx: &mut Context<Self>, ph: &str| {
            let ph = ph.to_string();
            cx.new(move |cx| TextInput::new(cx, ph))
        };
        let name = field(cx, "Name");
        let secret = cx.new(|cx| TextInput::new(cx, "").masked());
        let signing = cx.new(|cx| TextInput::new(cx, "Optional").masked());
        let database = field(cx, "Paste a database link, or choose one");
        let titles = field(cx, "Any title (or e.g. Acme, Demo)");
        let calendars = field(cx, "Any calendar");
        let mut subs = vec![cx.observe(&controller, |_, _, cx| cx.notify())];
        // A pasted database link loads its columns on Return or when focus leaves.
        subs.push(cx.subscribe(&database, |this: &mut Self, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Submit) {
                this.database_changed(cx);
            }
        }));
        Self { controller, sending: HashSet::new(), tests: HashMap::new(), editor: None, name, secret, signing, database, titles, calendars, _subs: subs }
    }

    /// Hook up focus-loss handlers that need a window (called once by the main window).
    pub fn attach(this: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let db = this.read(cx).database.clone();
        let fh = gpui::Focusable::focus_handle(db.read(cx), cx);
        this.update(cx, |s, cx| {
            let sub = cx.on_blur(&fh, window, |this, _, cx| this.database_changed(cx));
            s._subs.push(sub);
        });
    }

    fn set_flash(&mut self, msg: impl Into<SharedString>, cx: &mut Context<Self>) {
        cx.emit(DestEvent::Notice(msg.into(), crate::toast::ToastKind::Done));
    }

    fn set_flash_error(&mut self, msg: impl Into<SharedString>, cx: &mut Context<Self>) {
        cx.emit(DestEvent::Notice(msg.into(), crate::toast::ToastKind::Error));
    }

    /// Name of the destination a send is in flight to, for the status corner.
    pub fn sending_label(&self, cx: &App) -> Option<String> {
        let (_, id) = self.sending.iter().next()?;
        let cfg = self.controller.read(cx).config.clone();
        dest::all(&cfg).iter().find(|d| d.id() == id).map(|d| d.name().to_string())
    }

    // ------------------------------------------------------------- sending
    pub fn send(&mut self, dir: PathBuf, id: String, cx: &mut Context<Self>) {
        let cfg = self.controller.read(cx).config.clone();
        let Some(d) = dest::find(&cfg, &id) else { return };
        let key = (dir.clone(), id.clone());
        if !self.sending.insert(key.clone()) {
            return;
        }
        let name = d.name().to_string();
        let is_share = d.kind() == Kind::Share;
        let destination_id = d.id().to_string();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let send_dir = dir.clone();
            let r = cx.background_spawn(async move { dest::send_session(d.as_ref(), &send_dir, false) }).await;
            let _ = this.update(cx, |this, cx| {
                this.sending.remove(&key);
                match r {
                    Ok((_, delivered)) => {
                        if let Some(md) = delivered.clipboard {
                            cx.write_to_clipboard(ClipboardItem::new_string(md));
                            this.set_flash("Copied as Markdown", cx);
                        } else if is_share {
                            match stenly_core::share::link_for(&dir, Some(&destination_id)) {
                                Ok(Some(link)) => { cx.write_to_clipboard(ClipboardItem::new_string(link)); this.set_flash("Private link copied", cx); }
                                _ => this.set_flash("Private link created", cx),
                            }
                        } else {
                            this.set_flash(format!("Sent to {name}"), cx);
                        }
                    }
                    Err(e) => this.set_flash_error(format!("Couldn't send to {name}: {}", format!("{e:#}").chars().take(80).collect::<String>()), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ------------------------------------------------------------- editor
    pub fn open_editor(&mut self, cfg: DestinationConfig, is_new: bool, cx: &mut Context<Self>) {
        let from_env = (!cfg.secret_env.trim().is_empty() && std::env::var(cfg.secret_env.trim()).is_ok_and(|v| !v.trim().is_empty())).then(|| cfg.secret_env.trim().to_string());
        let secret_saved = !is_new && dest::secret_for(&cfg, "").is_some();
        let signing_saved = !is_new && stenly_core::secrets::destination_secret(&dest::secret_account(&cfg.id, "signing")).is_some();
        let secret_ph = match (cfg.kind, secret_saved) {
            (_, true) if from_env.is_some() => format!("From ${} · type to use another", from_env.unwrap_or_default()),
            (_, true) => "Saved in Keychain · type to replace".to_string(),
            (Kind::Notion, false) => "ntn_…".to_string(),
            (Kind::Slack, false) => "https://hooks.slack.com/services/…".to_string(),
            (Kind::Share, false) => "Optional".to_string(),
            (_, false) => "https://…".to_string(),
        };
        self.secret = cx.new(|cx| TextInput::new(cx, secret_ph).masked());
        self.signing = cx.new(|cx| TextInput::new(cx, if signing_saved { "Saved in Keychain · type to replace" } else { "Optional" }).masked());
        self.name.update(cx, |t, cx| t.set_text(cfg.name.clone(), cx));
        self.database.update(cx, |t, cx| t.set_text(if cfg.kind == Kind::Share { cfg.share.server_url.clone() } else { cfg.notion.database_id.clone() }, cx));
        self.titles.update(cx, |t, cx| t.set_text(cfg.auto.title_contains.join(", "), cx));
        self.calendars.update(cx, |t, cx| t.set_text(cfg.auto.calendars.join(", "), cx));
        let load = cfg.kind == Kind::Notion && !cfg.notion.database_id.is_empty();
        self.editor = Some(Editor { cfg, is_new, secret_saved, signing_saved, schema: None, related: None, loading: None, test: Probe::Idle });
        if load {
            self.load_schema(false, cx);
        }
        cx.notify();
    }

    /// Toolbar title while a destination is being edited.
    pub fn editor_title(&self) -> Option<SharedString> {
        let e = self.editor.as_ref()?;
        Some(if e.is_new { format!("New {} Destination", e.cfg.kind.label()).into() } else { e.cfg.name.clone().into() })
    }

    pub fn close_editor(&mut self, cx: &mut Context<Self>) {
        self.editor = None;
        cx.notify();
    }

    /// The form's current values (typed secrets held in memory only).
    fn draft(&self, cx: &App) -> Option<DestinationConfig> {
        let e = self.editor.as_ref()?;
        let mut c = e.cfg.clone();
        let name = self.name.read(cx).text().trim().to_string();
        c.name = if name.is_empty() { c.kind.label().to_string() } else { name };
        let database = self.database.read(cx).text().trim().to_string();
        if c.kind == Kind::Share { c.share.server_url = database; } else { c.notion.database_id = database; }
        c.auto.title_contains = split_list(self.titles.read(cx).text());
        c.auto.calendars = split_list(self.calendars.read(cx).text());
        let typed = self.secret.read(cx).text().trim().to_string();
        c.secret_override = (!typed.is_empty()).then_some(Secret(typed));
        let sig = self.signing.read(cx).text().trim().to_string();
        c.signing_override = (!sig.is_empty()).then_some(Secret(sig));
        Some(c)
    }

    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut DestinationConfig)) {
        if let Some(e) = &mut self.editor {
            f(&mut e.cfg);
            e.test = Probe::Idle;
        }
        cx.notify();
    }

    fn token(&self, cx: &App) -> Option<(String, String)> {
        let d = self.draft(cx)?;
        Some((d.notion.api_base.clone(), dest::secret_for(&d, "")?))
    }

    fn database_changed(&mut self, cx: &mut Context<Self>) {
        let Some(e) = &self.editor else { return };
        let typed = self.database.read(cx).text().trim().to_string();
        let have = e.schema.as_ref().map(|s| s.id.clone());
        if e.cfg.kind != Kind::Notion || typed.is_empty() || notion::parse_id(&typed) == have {
            return;
        }
        self.load_schema(true, cx);
    }

    /// Read the chosen database's columns; `suggest` fills in the mapping.
    fn load_schema(&mut self, suggest: bool, cx: &mut Context<Self>) {
        let Some((base, token)) = self.token(cx) else {
            if let Some(e) = &mut self.editor {
                e.test = Probe::Failed("Add the integration token first.".into());
            }
            cx.notify();
            return;
        };
        let db = self.database.read(cx).text().trim().to_string();
        if let Some(e) = &mut self.editor {
            e.loading = Some("Reading the database…");
            e.test = Probe::Idle;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = cx.background_spawn(async move { notion::database_schema(&base, &token, &db) }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(e) = &mut this.editor else { return };
                e.loading = None;
                match r {
                    Ok(s) => {
                        if suggest || e.cfg.notion.title_property.is_empty() {
                            notion::suggest_mapping(&mut e.cfg.notion, &s);
                        }
                        e.cfg.notion.database_id = s.id.clone();
                        let title = s.title.clone();
                        e.schema = Some(s);
                        e.test = Probe::Ok(format!("Using “{title}”"));
                        this.database.update(cx, |t, cx| t.set_text(e.cfg.notion.database_id.clone(), cx));
                        this.load_related(cx);
                    }
                    Err(err) => {
                        e.schema = None;
                        e.test = Probe::Failed(sentence(&format!("{err:#}")));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Columns of the database the relation points at (for "Match against").
    fn load_related(&mut self, cx: &mut Context<Self>) {
        let Some(e) = &self.editor else { return };
        let Some(rel) = e.cfg.notion.relation.clone() else { return };
        let target = match notion::parse_id(&rel.database_id) {
            Some(t) => t,
            None => match e.schema.as_ref().and_then(|s| s.relations.iter().find(|(n, _)| *n == rel.property).map(|(_, d)| d.clone())) {
                Some(t) => t,
                None => return,
            },
        };
        let Some((base, token)) = self.token(cx) else { return };
        cx.spawn(async move |this, cx| {
            let r = cx.background_spawn(async move { notion::database_schema(&base, &token, &target) }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(e) = &mut this.editor {
                    e.related = r.ok();
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn test_draft(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.draft(cx) else { return };
        let cfg = self.controller.read(cx).config.clone();
        if let Some(e) = &mut self.editor {
            e.test = Probe::Running;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = cx.background_spawn(async move { dest::build(&cfg, &d).test() }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(e) = &mut this.editor {
                    e.test = match r {
                        Ok(m) => Probe::Ok(m),
                        Err(err) => Probe::Failed(sentence(&format!("{err:#}"))),
                    };
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn test_saved(&mut self, id: String, cx: &mut Context<Self>) {
        let cfg = self.controller.read(cx).config.clone();
        let Some(d) = dest::find(&cfg, &id) else { return };
        self.tests.insert(id.clone(), Probe::Running);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = cx.background_spawn(async move { d.test() }).await;
            let _ = this.update(cx, |this, cx| {
                this.tests.insert(id, match r {
                    Ok(m) => Probe::Ok(m),
                    Err(e) => Probe::Failed(sentence(&format!("{e:#}"))),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(mut d) = self.draft(cx) else { return };
        let is_new = self.editor.as_ref().is_some_and(|e| e.is_new);
        let cfg = self.controller.read(cx).config.clone();
        if is_new {
            d.id = dest::new_id(&d.name, &cfg.destinations);
        }
        if d.kind == Kind::Folder && d.folder.path.trim().is_empty() {
            if let Some(e) = &mut self.editor {
                e.test = Probe::Failed("Choose a folder.".into());
            }
            cx.notify();
            return;
        }
        let secrets = [("", d.secret_override.take()), ("signing", d.signing_override.take())];
        for (suffix, s) in secrets {
            if let Some(Secret(v)) = s
                && let Err(e) = stenly_core::secrets::set_destination_secret(&dest::secret_account(&d.id, suffix), &v)
            {
                if let Some(ed) = &mut self.editor {
                    ed.test = Probe::Failed(sentence(&format!("{e:#}")));
                }
                cx.notify();
                return;
            }
        }
        let id = d.id.clone();
        match dest::upsert(&cfg, d) {
            Ok(c) => {
                self.controller.update(cx, |ctrl, cx| ctrl.reload_config(c, cx));
                self.editor = None;
                self.tests.remove(&id);
            }
            Err(e) => {
                if let Some(ed) = &mut self.editor {
                    ed.test = Probe::Failed(format!("Couldn't save: {e:#}"));
                }
            }
        }
        cx.notify();
    }

    fn remove(&mut self, id: String, cx: &mut Context<Self>) {
        let cfg = self.controller.read(cx).config.clone();
        match dest::remove(&cfg, &id) {
            Ok(c) => self.controller.update(cx, |ctrl, cx| ctrl.reload_config(c, cx)),
            Err(e) => self.controller.update(cx, |ctrl, cx| {
                ctrl.last_error = Some(format!("Couldn't remove the destination: {e:#}"));
                cx.notify();
            }),
        }
        self.editor = None;
        cx.notify();
    }
}

/// The Send to… menu: local destinations first, then cloud ones, each with
/// its privacy caption and last send. `ids[i]`: destination id, "" for
/// "Manage Destinations…", None for headers/separators.
fn send_menu(cfg: &stenly_core::Config, meta: &SessionMeta) -> (Vec<MenuEntry>, Vec<Option<String>>) {
    let all = dest::all(cfg);
    let now = Local::now();
    let mut entries: Vec<MenuEntry> = Vec::new();
    let mut ids: Vec<Option<String>> = Vec::new();
    for (header, loc) in [("On this Mac", Locality::Local), ("Other services", Locality::Cloud)] {
        let group: Vec<&Box<dyn Destination>> = all.iter().filter(|d| d.locality() == loc).collect();
        if group.is_empty() {
            continue;
        }
        if !entries.is_empty() {
            entries.push(MenuEntry::separator());
            ids.push(None);
        }
        entries.push(MenuEntry::header(if cfg!(target_os = "macos") { header } else { if loc.is_local() { "On this computer" } else { header } }));
        ids.push(None);
        for d in group {
            let ready = d.ready();
            let last = dest::last_send(meta, d.id());
            let mut sub = d.caption();
            match (&ready, last) {
                (Err(_), _) => sub.push_str(" · Not set up"),
                (Ok(_), Some(r)) => sub = format!("{sub} · {}", dest::describe_send(r, now)),
                _ => {}
            }
            let title = match last {
                Some(r) if r.ok && d.updates_in_place() && ready.is_ok() => format!("{} (Update)", d.name()),
                Some(r) if !r.ok && ready.is_ok() => format!("{} (Retry)", d.name()),
                _ => d.name().to_string(),
            };
            let mut entry = destination_entry(d.kind(), title, sub);
            if d.id() == dest::CLIPBOARD_ID {
                entry.symbol = Some("doc.on.doc");
            }
            entry.enabled = ready.is_ok();
            entry.checked = ready.is_ok() && last.is_some_and(|r| r.ok);
            entries.push(entry);
            ids.push(Some(d.id().to_string()));
        }
    }
    entries.push(MenuEntry::separator());
    ids.push(None);
    entries.push(MenuEntry::item("Manage Destinations…"));
    ids.push(Some(String::new()));

    (entries, ids)
}

/// Dev: STENLY_EDIT_DESTINATION=<id | notion | slack | webhook | folder>
/// opens Settings → Destinations' editor (an existing one, or a new one of that kind).
pub fn dev_open_editor(ui: &Entity<DestUi>, key: &str, cx: &mut App) {
    let cfg = ui.read(cx).controller.read(cx).config.clone();
    let (d, is_new) = match cfg.destinations.iter().find(|d| d.id == key) {
        Some(d) => (d.clone(), false),
        None => {
            let Some(kind) = Kind::parse(key) else { return };
            let mut d = DestinationConfig::new(kind, kind.label());
            d.id = dest::new_id(kind.label(), &cfg.destinations);
            (d, true)
        }
    };
    ui.update(cx, |s, cx| s.open_editor(d, is_new, cx));
}

/// Dev: STENLY_SEND_MENU=1 with STENLY_OPEN_MEETING opens the Send to… menu
/// (for screenshots of the native menu).
pub fn dev_open_menu(ui: &Entity<DestUi>, dir: PathBuf, window: &mut Window, cx: &mut App) {
    let cfg = ui.read(cx).controller.read(cx).config.clone();
    let Ok(meta) = SessionMeta::load(&dir) else { return };
    let (entries, _) = send_menu(&cfg, &meta);
    window
        .spawn(cx, async move |cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(1500)).await;
            let Ok(view) = cx.update(|w, _| ui::native_view(w)) else { return };
            let w = cx.update(|w, _| f32::from(w.viewport_size().width)).unwrap_or(900.);
            let picked = ui::rich_menu_in_view(view, &entries, gpui::point(px(w - 200.), px(56.)));
            tracing::info!(?picked, "send menu test closed");
        })
        .detach();
}

// ------------------------------------------------------------- meeting view

/// "Send to…" toolbar button for the open meeting. Its progress and result
/// show in the window's bottom-right corner, so the toolbar never shifts.
pub fn send_button(ui: &Entity<DestUi>, dir: PathBuf, meta: &SessionMeta, cx: &App) -> AnyElement {
    let s = ui.read(cx);
    let cfg = s.controller.read(cx).config.clone();
    let (entries, ids) = send_menu(&cfg, meta);
    let ui = ui.clone();
    div()
        .flex()
        .items_center()
        .child(
            ui::toolbar_push(ui::button_lead_icon("send-to", "Send to…", Btn::Ghost, icons::PAPERPLANE)).on_click(move |e, window, cx| {
                let pos = e.mouse_position().unwrap_or_else(|| window.mouse_position());
                let view = ui::native_view(window);
                let (entries, ids, ui, dir) = (entries.clone(), ids.clone(), ui.clone(), dir.clone());
                window
                    .spawn(cx, async move |cx| {
                        let Some(i) = ui::rich_menu_in_view(view, &entries, pos) else { return };
                        let Some(id) = ids.get(i).cloned().flatten() else { return };
                        let _ = cx.update(|_, cx| {
                            if id.is_empty() {
                                crate::main_window::open_destinations(cx);
                            } else {
                                ui.update(cx, |s, cx| s.send(dir, id, cx));
                            }
                        });
                    })
                    .detach();
            }),
        )
        .into_any_element()
}

/// One line under the meeting's details: where it was sent, with links and Retry.
pub fn status_strip(ui: &Entity<DestUi>, dir: &Path, meta: &SessionMeta, cx: &App) -> Option<AnyElement> {
    let s = ui.read(cx);
    let cfg = s.controller.read(cx).config.clone();
    let now = Local::now();
    let items: Vec<AnyElement> = dest::all(&cfg)
        .into_iter()
        .filter_map(|d| {
            let r = dest::last_send(meta, d.id())?.clone();
            let id = d.id().to_string();
            let busy = s.sending.contains(&(dir.to_path_buf(), id.clone()));
            let label = format!("{} · {}", d.name(), dest::describe_send(&r, now));
            let el = div()
                .id(SharedString::from(format!("sent-{id}")))
                .flex()
                .items_center()
                .gap(px(4.))
                .text_size(px(theme::BODY))
                .text_color(theme::muted())
                .child(if r.ok { icons::icon_w(icons::CHECK, 10., icons::Weight::Semibold, theme::muted()) } else { icon(icons::ALERT, 11., theme::amber()) })
                .child(icon(locality_icon(d.locality()), 11., theme::muted()))
                .child(label);
            let el: AnyElement = if busy {
                el.child(ui::spinner(SharedString::from(format!("sent-spin-{id}")), 10., theme::muted())).into_any_element()
            } else if r.ok && r.url.is_some() && d.kind() != Kind::Share {
                let rec = r.clone();
                el.cursor_pointer().hover(|x| x.text_color(theme::text())).child(icon(icons::EXTERNAL, 10., theme::muted())).on_click(move |_, _, cx| open_result(&rec, cx)).into_any_element()
            } else if !r.ok {
                let (ui, dir) = (ui.clone(), dir.to_path_buf());
                let tip = r.error.clone().unwrap_or_default();
                el.child(
                    div()
                        .id(SharedString::from(format!("retry-{id}")))
                        .text_color(theme::accent_text())
                        .cursor_pointer()
                        .child("Retry")
                        .on_click(move |_, _, cx| ui.update(cx, |s, cx| s.send(dir.clone(), id.clone(), cx))),
                )
                .tooltip(move |_, cx| cx.new(|_| crate::destinations_ui::Tip(tip.clone().into())).into())
                .into_any_element()
            } else {
                el.into_any_element()
            };
            Some(el)
        })
        .collect();
    if items.is_empty() {
        return None;
    }
    Some(div().flex().flex_wrap().gap_x(px(14.)).gap_y(px(4.)).pt(px(6.)).children(items).into_any_element())
}

/// Error tooltip for a failed send.
pub struct Tip(SharedString);
impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(360.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(6.))
            .bg(theme::popup_bg())
            .border_1()
            .border_color(theme::divider())
            .shadow_md()
            .text_size(px(theme::SUBHEAD))
            .text_color(theme::text())
            .font_family(theme::SANS)
            .child(self.0.clone())
    }
}

// ------------------------------------------------------------- settings page

fn probe_line(p: &Probe, idle: impl Into<SharedString>) -> AnyElement {
    match p {
        Probe::Idle => div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(idle.into()).into_any_element(),
        Probe::Running => ui::pending("dest-probe", "Testing…").into_any_element(),
        Probe::Ok(m) => div().flex().items_center().gap(px(5.)).text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(icon(icons::CHECK, 11., theme::green())).child(m.clone()).into_any_element(),
        Probe::Failed(e) => div()
            .flex()
            .items_start()
            .gap(px(5.))
            .text_size(px(theme::SUBHEAD))
            .line_height(px(15.))
            .text_color(theme::muted())
            .child(div().pt(px(1.)).child(icon(icons::ALERT, 11., theme::amber())))
            .child(div().flex_1().min_w_0().child(e.clone()))
            .into_any_element(),
    }
}

/// Pop-up button over runtime strings (column names). `options[0]` may be
/// a "Don't set" entry.
fn string_popup(id: impl Into<SharedString>, options: Vec<String>, current: Option<usize>, on_pick: impl Fn(usize, &mut App) + 'static) -> AnyElement {
    let label = current.and_then(|i| options.get(i).cloned()).unwrap_or_else(|| "Choose…".into());
    let on_pick = std::rc::Rc::new(on_pick);
    div()
        .id(id.into())
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(24.))
        .max_w(px(220.))
        .pl(px(10.))
        .pr(px(5.))
        .rounded(px(6.))
        .bg(theme::button())
        .flex_shrink_0()
        .text_size(px(theme::BODY))
        .text_color(theme::text())
        .active(|s| s.bg(theme::button_hover()))
        .child(div().min_w_0().truncate().child(label))
        .child(icon(icons::CHEVRON_UPDOWN, 10., theme::muted()))
        .on_click(move |e, window, cx| {
            let pos = e.mouse_position().unwrap_or_else(|| window.mouse_position());
            let view = ui::native_view(window);
            let (opts, on_pick) = (options.clone(), on_pick.clone());
            window
                .spawn(cx, async move |cx| {
                    let labels: Vec<&str> = opts.iter().map(String::as_str).collect();
                    if let Some(i) = ui::menu_in_view(view, &labels, current, pos)
                        && Some(i) != current
                    {
                        let _ = cx.update(|_, cx| on_pick(i, cx));
                    }
                })
                .detach();
        })
        .into_any_element()
}

fn field_row(label: &'static str, child: impl IntoElement) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .min_h(px(44.))
        .child(div().w(px(120.)).flex_shrink_0().text_size(px(theme::BODY)).child(label))
        .child(div().flex_1().min_w_0().flex().items_center().gap(px(8.)).child(child))
        .into_any_element()
}

fn help(text: impl Into<SharedString>) -> AnyElement {
    div().pb(px(10.)).text_size(px(theme::SUBHEAD)).line_height(px(15.)).text_color(theme::muted()).child(text.into()).into_any_element()
}

const NONE: &str = "Don't set";

impl DestUi {
    fn list_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let cfg = self.controller.read(cx).config.clone();
        let all = dest::all(&cfg);
        let row = |d: &dyn Destination, cx: &mut Context<Self>| -> AnyElement {
            let id = d.id().to_string();
            let builtin = id == dest::NOTES_FOLDER_ID || id == dest::CLIPBOARD_ID;
            let probe = self.tests.get(&id).cloned().unwrap_or_default();
            let ready = d.ready();
            let rule = stenly_core::destinations::rules::describe(d.auto());
            let desc: AnyElement = div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(privacy_label(d.locality(), d.caption()))
                .when(id == dest::NOTES_FOLDER_ID, |x| {
                    x.child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis_start().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(d.service()))
                })
                .when(!builtin, |x| {
                    x.child(match (&ready, &probe) {
                        (_, Probe::Idle) if ready.is_err() => probe_line(&Probe::Failed(sentence(&format!("{:#}", ready.as_ref().unwrap_err()))), ""),
                        (_, Probe::Idle) => div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(rule).into_any_element(),
                        (_, p) => probe_line(p, ""),
                    })
                })
                .when(id == dest::NOTES_FOLDER_ID, |x| x.child(div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child("Written after every meeting")))
                .into_any_element();
            let trailing: AnyElement = if builtin {
                ui::chip("Built In", None, theme::faint()).into_any_element()
            } else {
                let (m_id, me) = (id.clone(), cx.entity().downgrade());
                let dcfg = cfg.destinations.iter().find(|x| x.id == id).cloned();
                // One inline action; the rest in "…" (System Settings → Wi-Fi).
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(ui::button(SharedString::from(format!("dedit-{id}")), "Edit…", Btn::Ghost).on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(d) = dcfg.clone() {
                            this.open_editor(d, false, cx);
                        }
                    })))
                    .child(ui::more_button(
                        SharedString::from(format!("dmore-{id}")),
                        vec![MenuEntry::item("Test Connection"), MenuEntry::separator(), MenuEntry { symbol: Some("trash"), ..MenuEntry::item("Remove") }],
                        move |k, _, cx| {
                            let id = m_id.clone();
                            let _ = me.update(cx, |this, cx| match k {
                                0 => this.test_saved(id, cx),
                                2 => this.remove(id, cx),
                                _ => {}
                            });
                        },
                    ))
                    .into_any_element()
            };
            div()
                .flex()
                .items_center()
                .gap(px(16.))
                .min_h(px(52.))
                .py(px(9.))
                .child(div().flex_1().min_w_0().flex().flex_col().gap(px(3.)).child(div().text_size(px(theme::BODY)).child(d.name().to_string())).child(desc))
                .child(div().flex_shrink_0().flex().items_center().gap(px(8.)).child(trailing))
                .into_any_element()
        };
        let local: Vec<AnyElement> = all.iter().filter(|d| d.locality().is_local()).map(|d| row(d.as_ref(), cx)).collect();
        let cloud: Vec<AnyElement> = all.iter().filter(|d| !d.locality().is_local()).map(|d| row(d.as_ref(), cx)).collect();
        let add = {
            let this = cx.entity();
            let kinds: Vec<(Kind, &'static str)> = vec![(Kind::Notion, "Notion"), (Kind::Slack, "Slack"), (Kind::Webhook, "Webhook"), (Kind::Folder, "Markdown Folder")];
            ui::button_icon("dest-add", "Add Destination", Btn::Ghost, icons::CHEVRON_DOWN).on_click(move |e, window, cx| {
                let pos = e.mouse_position().unwrap_or_else(|| window.mouse_position());
                let view = ui::native_view(window);
                let (this, kinds) = (this.clone(), kinds.clone());
                window
                    .spawn(cx, async move |cx| {
                        let entries: Vec<MenuEntry> = kinds
                            .iter()
                            .map(|(k, l)| destination_entry(*k, (*l).into(), dest::privacy_caption(k.locality(), k.service())))
                            .collect();
                        let Some(i) = ui::rich_menu_in_view(view, &entries, pos) else { return };
                        let kind = kinds[i].0;
                        let _ = cx.update(|_, cx| {
                            this.update(cx, |s, cx| {
                                let existing = s.controller.read(cx).config.destinations.clone();
                                let mut d = DestinationConfig::new(kind, kind.label());
                                d.id = dest::new_id(kind.label(), &existing);
                                s.open_editor(d, true, cx)
                            })
                        });
                    })
                    .detach();
            })
        };
        div()
            .flex()
            .flex_col()
            .child(ui::caption("On This Mac"))
            .child(ui::card(local))
            .child(ui::caption("Other Services"))
            .child(if cloud.is_empty() {
                ui::card(vec![div().py(px(14.)).text_size(px(theme::BODY)).text_color(theme::muted()).child("Notion, Slack and webhooks receive your notes over the internet. Nothing is sent until you add one.").into_any_element()])
            } else {
                ui::card(cloud)
            })
            .child(div().pt(px(12.)).flex().justify_end().child(add))
            .into_any_element()
    }

    fn editor_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(e) = &self.editor else { return div().into_any_element() };
        let d = &e.cfg;
        let kind = d.kind;
        let me = cx.entity();
        let header = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(privacy_label(kind.locality(), match kind {
                Kind::Folder => dest::privacy_caption(Locality::Local, ""),
                k => format!("{} · your notes and attendee names leave this Mac", dest::privacy_caption(Locality::Cloud, k.service())),
            }));

        let mut general = vec![field_row("Name", ui::text_field(self.name.clone()))];
        let mut extra: Vec<(&'static str, Vec<AnyElement>)> = Vec::new();
        let toggle = |id: &'static str, on: bool, f: fn(&mut DestinationConfig, bool), me: &Entity<Self>| {
            let me = me.clone();
            ui::toggle(id, on, move |_, _, cx| me.update(cx, |s, cx| s.edit(cx, |c| f(c, !on)))).into_any_element()
        };
        match kind {
            Kind::Folder => {
                let path = if d.folder.path.is_empty() { "No folder chosen".to_string() } else { stenly_core::util::collapse_tilde(&stenly_core::util::expand_tilde(&d.folder.path)) };
                let me2 = me.clone();
                general.push(
                    ui::path_row(
                        None,
                        "Folder",
                        path,
                        ui::button("dest-folder", "Choose…", Btn::Outline).on_click(move |_, _, cx| {
                            let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Choose".into()) });
                            let me = me2.clone();
                            cx.spawn(async move |cx| {
                                if let Ok(Ok(Some(paths))) = rx.await
                                    && let Some(p) = paths.into_iter().next()
                                {
                                    let _ = me.update(cx, |s, cx| s.edit(cx, |c| c.folder.path = stenly_core::util::collapse_tilde(&p)));
                                }
                            })
                            .detach();
                        }),
                    )
                    .into_any_element(),
                );
                let on = d.folder.include_transcript.unwrap_or(self.controller.read(cx).config.output.include_transcript);
                general.push(ui::row(None, "Include transcript", None, toggle("dest-ft", on, |c, v| c.folder.include_transcript = Some(v), &me)).into_any_element());
            }
            Kind::Share => {
                general.push(field_row("Server URL", ui::text_field(self.database.clone())));
                general.push(field_row("Upload token", ui::text_field(self.secret.clone())));
                general.push(ui::row(None, "Include transcript", Some("Encrypted with the note before upload".into()), toggle("dest-st", d.share.include_transcript, |c, v| c.share.include_transcript = v, &me)).into_any_element());
                general.push(ui::row(None, "Plaintext title for previews", Some("Off keeps the meeting title private from link-preview services".into()), toggle("dest-sp", d.share.plaintext_title, |c, v| c.share.plaintext_title = v, &me)).into_any_element());
                let days = d.share.expiry_days;
                let me2 = me.clone();
                general.push(ui::row(None, "Expires", Some("The server may enforce a shorter maximum".into()), ui::popup("dest-share-expiry", &[(7u32, "7 days"), (30, "30 days"), (90, "90 days"), (365, "1 year")], [7,30,90,365].into_iter().min_by_key(|v| (*v as i64 - days as i64).abs()).unwrap_or(90), move |v, cx| me2.update(cx, |s, cx| s.edit(cx, |c| c.share.expiry_days = v)))) .into_any_element());
                general.push(help("Your note is encrypted on this Mac. The key stays in your Keychain and is only added after # in the link you copy."));
            }
            Kind::Slack | Kind::Webhook => {
                general.push(field_row(kind.secret_label(), ui::text_field(self.secret.clone())));
                if kind == Kind::Webhook {
                    general.push(field_row("Signing secret", ui::text_field(self.signing.clone())));
                    general.push(ui::row(None, "Include transcript", Some("Adds every line of the transcript to the JSON".into()), toggle("dest-wt", d.webhook.include_transcript, |c, v| c.webhook.include_transcript = v, &me)).into_any_element());
                    general.push(help("Stenly POSTs JSON (type “stenly.meeting.v1”: title, times, attendees, summary, Markdown, transcript). With a signing secret, X-Stenly-Signature carries an HMAC-SHA256 of the body. The URL and secret are kept in your Keychain."));
                } else {
                    general.push(help("In Slack: Apps → Incoming Webhooks → Add to a channel, then paste the URL. It's kept in your Keychain."));
                }
            }
            Kind::Notion => {
                general.push(field_row("Integration token", ui::text_field(self.secret.clone())));
                general.push(help("Create an internal integration at notion.so/my-integrations, copy its secret here, then share your meetings database (and any related database) with it: ••• → Connections. The token is kept in your Keychain."));
                let me2 = me.clone();
                general.push(field_row(
                    "Database",
                    div().flex_1().min_w_0().flex().items_center().gap(px(8.)).child(ui::text_field(self.database.clone())).child(ui::button("dest-dbs", "Choose…", Btn::Outline).on_click(move |ev, window, cx| {
                        let pos = ev.mouse_position().unwrap_or_else(|| window.mouse_position());
                        let view = ui::native_view(window);
                        let me = me2.clone();
                        let Some((base, token)) = me.read(cx).token(cx) else {
                            me.update(cx, |s, cx| {
                                if let Some(e) = &mut s.editor {
                                    e.test = Probe::Failed("Add the integration token first.".into());
                                }
                                cx.notify();
                            });
                            return;
                        };
                        me.update(cx, |s, cx| {
                            if let Some(e) = &mut s.editor {
                                e.loading = Some("Finding databases…");
                            }
                            cx.notify();
                        });
                        window
                            .spawn(cx, async move |cx| {
                                let r = cx.background_executor().spawn(async move { notion::list_databases(&base, &token) }).await;
                                let _ = cx.update(|_, cx| me.update(cx, |s, cx| {
                                    if let Some(e) = &mut s.editor {
                                        e.loading = None;
                                    }
                                    cx.notify();
                                }));
                                match r {
                                    Ok(list) if !list.is_empty() => {
                                        let labels: Vec<&str> = list.iter().map(|(_, t)| t.as_str()).collect();
                                        if let Some(i) = ui::menu_in_view(view, &labels, None, pos) {
                                            let id = list[i].0.clone();
                                            let _ = cx.update(|_, cx| me.update(cx, |s, cx| {
                                                s.database.update(cx, |t, cx| t.set_text(id, cx));
                                                s.load_schema(true, cx);
                                            }));
                                        }
                                    }
                                    Ok(_) => {
                                        let _ = cx.update(|_, cx| me.update(cx, |s, cx| {
                                            if let Some(e) = &mut s.editor {
                                                e.test = Probe::Failed("No databases are shared with this integration yet (••• → Connections in Notion).".into());
                                            }
                                            cx.notify();
                                        }));
                                    }
                                    Err(err) => {
                                        let _ = cx.update(|_, cx| me.update(cx, |s, cx| {
                                            if let Some(e) = &mut s.editor {
                                                e.test = Probe::Failed(sentence(&format!("{err:#}")));
                                            }
                                            cx.notify();
                                        }));
                                    }
                                }
                            })
                            .detach();
                    })),
                ));
                general.push(ui::row(None, "Include transcript", Some("In a collapsed toggle at the end of the page".into()), toggle("dest-nt", d.notion.include_transcript, |c, v| c.notion.include_transcript = v, &me)).into_any_element());

                if let Some(s) = &e.schema {
                    let map = |label: &'static str, id: &'static str, types: &[&str], cur: &str, set: fn(&mut DestinationConfig, String)| -> AnyElement {
                        let mut opts = vec![NONE.to_string()];
                        opts.extend(s.of(types));
                        let current = if cur.is_empty() { Some(0) } else { opts.iter().position(|o| o == cur) };
                        let me = me.clone();
                        let o2 = opts.clone();
                        let missing = !cur.is_empty() && current.is_none();
                        field_row(
                            label,
                            div().flex().items_center().gap(px(8.)).child(string_popup(id, opts, current, move |i, cx| {
                                let v = if i == 0 { String::new() } else { o2[i].clone() };
                                me.update(cx, |s, cx| s.edit(cx, |c| set(c, v)))
                            }))
                            .when(missing, |x| x.child(div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(format!("“{cur}” isn't in this database")))),
                        )
                    };
                    let n = &d.notion;
                    let title_col = s.of(&["title"]).into_iter().next().unwrap_or_default();
                    let mut rows = vec![field_row("Title", div().text_size(px(theme::BODY)).text_color(theme::muted()).child(format!("{title_col} ← meeting title")))];
                    rows.push(map("Date", "nm-date", &["date"], &n.date_property, |c, v| c.notion.date_property = v));
                    rows.push(map("Attendees", "nm-att", &["multi_select", "rich_text", "people", "email"], &n.attendees_property, |c, v| c.notion.attendees_property = v));
                    rows.push(map("Tags", "nm-tags", &["multi_select"], &n.tags_property, |c, v| c.notion.tags_property = v));
                    rows.push(map("Meeting link", "nm-url", &["url"], &n.url_property, |c, v| c.notion.url_property = v));
                    extra.push(("Properties", rows));

                    // Relation: link the page to CRM records by attendee.
                    let rel = n.relation.clone();
                    let mut rel_opts = vec!["Off".to_string()];
                    rel_opts.extend(s.relations.iter().map(|(name, _)| name.clone()));
                    let cur = match &rel {
                        Some(r) if !r.property.is_empty() => rel_opts.iter().position(|o| *o == r.property),
                        _ => Some(0),
                    };
                    let (me_r, o2) = (me.clone(), rel_opts.clone());
                    let mut rrows = vec![field_row(
                        "Link to",
                        string_popup("nm-rel", rel_opts, cur, move |i, cx| {
                            let v = (i > 0).then(|| o2[i].clone());
                            me_r.update(cx, |s, cx| {
                                s.edit(cx, |c| {
                                    c.notion.relation = v.map(|p| RelationMatch { property: p, match_by: c.notion.relation.as_ref().map(|r| r.match_by).unwrap_or_default(), ..Default::default() })
                                });
                                if let Some(e) = &mut s.editor {
                                    e.related = None;
                                }
                                s.load_related(cx);
                            })
                        }),
                    )];
                    if s.relations.is_empty() {
                        rrows.push(help("This database has no relation columns. Add one (e.g. Company → your Companies database) to link meetings to records."));
                    }
                    if let Some(r) = rel.filter(|r| !r.property.is_empty()) {
                        let by = [MatchBy::Domain, MatchBy::Email, MatchBy::Name];
                        let by_opts: Vec<String> = by.iter().map(|b| b.label().to_string()).collect();
                        let me_b = me.clone();
                        rrows.push(field_row(
                            "Match by",
                            string_popup("nm-by", by_opts, by.iter().position(|b| *b == r.match_by), move |i, cx| {
                                me_b.update(cx, |s, cx| s.edit(cx, |c| {
                                    if let Some(r) = &mut c.notion.relation {
                                        r.match_by = by[i];
                                        r.match_property.clear();
                                    }
                                }))
                            }),
                        ));
                        let related_title = e.related.as_ref().map(|x| x.title.clone()).unwrap_or_else(|| "the related database".into());
                        match &e.related {
                            Some(rs) => {
                                let mut opts = vec!["Automatic".to_string()];
                                let types: &[&str] = match r.match_by {
                                    MatchBy::Domain => &["url", "rich_text", "email", "title"],
                                    MatchBy::Email => &["email", "rich_text"],
                                    MatchBy::Name => &["title", "rich_text"],
                                };
                                opts.extend(rs.of(types));
                                let cur = if r.match_property.is_empty() { Some(0) } else { opts.iter().position(|o| *o == r.match_property) };
                                let (me_m, o3) = (me.clone(), opts.clone());
                                rrows.push(field_row(
                                    "Match against",
                                    string_popup("nm-mp", opts, cur, move |i, cx| {
                                        let v = if i == 0 { String::new() } else { o3[i].clone() };
                                        me_m.update(cx, |s, cx| s.edit(cx, |c| {
                                            if let Some(r) = &mut c.notion.relation {
                                                r.match_property = v;
                                            }
                                        }))
                                    }),
                                ));
                            }
                            None => rrows.push(field_row("Match against", ui::pending("nm-rel-load", "Reading related database…"))),
                        }
                        rrows.push(help(match r.match_by {
                            MatchBy::Domain => format!("Guests' company email domains (acme.com) are looked up in the website, domain or email column of {related_title}. Gmail and other personal addresses are ignored."),
                            MatchBy::Email => format!("Each guest's email address is looked up in the email column of {related_title}."),
                            MatchBy::Name => format!("Each guest's name is looked up in the names in {related_title}."),
                        }));
                    }
                    extra.push(("Link Records", rrows));
                }
            }
        }

        // Rules
        let a = &d.auto;
        let mut rules = vec![ui::row(None, "Send automatically after each meeting", Some("Off: only when you choose Send to… on a meeting".into()), toggle("dest-auto", a.enabled, |c, v| c.auto.enabled = v, &me)).into_any_element()];
        if a.enabled {
            rules.push(ui::row(None, "Only with people outside my company", Some("At least one guest from another email domain".into()), toggle("dest-ext", a.external_only, |c, v| c.auto.external_only = v, &me)).into_any_element());
            rules.push(field_row("Title contains", ui::text_field(self.titles.clone())));
            // Only the calendars Stenly watches (Settings → Calendar).
            let c = self.controller.read(cx);
            let ids = &c.config.calendar.calendar_ids;
            let cals: Vec<String> = c.calendars.iter().filter(|cal| ids.contains(&cal.id) || (ids.is_empty() && cal.primary)).map(|cal| cal.name.clone()).collect();
            rules.push(field_row("Calendars", ui::text_field(self.calendars.clone())));
            if !cals.is_empty() {
                rules.push(help(format!("Separate several with commas. Calendars you watch: {}", cals.join(", "))));
            }
        }

        let status: AnyElement = match e.loading {
            Some(l) => ui::pending("dest-loading", l).into_any_element(),
            None => probe_line(
                &e.test,
                match (kind, e.secret_saved) {
                    (Kind::Folder, _) => "",
                    (_, true) => "Credentials saved in your Keychain.",
                    _ => "Credentials are stored in your Keychain, never in the config file.",
                },
            ),
        };
        let is_new = e.is_new;
        let id = d.id.clone();
        let footer = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .min_h(px(44.))
            .child(div().flex_1().min_w_0().child(status))
            .when(!is_new, |x| x.child(ui::button("dest-rm", "Remove", Btn::Danger).on_click(cx.listener(move |this, _, _, cx| this.remove(id.clone(), cx)))))
            .child(ui::button("dest-test", "Test", Btn::Outline).on_click(cx.listener(|this, _, window, cx| {
                window.blur(cx);
                this.test_draft(cx)
            })))
            .child(ui::button("dest-cancel", "Cancel", Btn::Ghost).on_click(cx.listener(|this, _, _, cx| this.close_editor(cx))))
            .child(ui::button("dest-save", if is_new { "Add" } else { "Save" }, Btn::Primary).on_click(cx.listener(|this, _, window, cx| {
                window.blur(cx);
                this.save(cx)
            })));

        let _ = e.signing_saved;
        let mut page = div()
            .flex()
            .flex_col()
            .child(div().pt(px(8.)).child(header))
            .child(ui::caption(kind.label()))
            .child(ui::card(general));
        for (title, rows) in extra {
            page = page.child(ui::caption(title)).child(ui::card(rows));
        }
        page.child(ui::caption("When to Send")).child(ui::card(rules)).child(div().pt(px(12.)).child(footer)).into_any_element()
    }
}

impl Render for DestUi {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.editor.is_some() { self.editor_page(cx) } else { self.list_page(cx) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_entries_use_bundled_logos_and_preserve_privacy_captions() {
        for (kind, logo) in [(Kind::Notion, NOTION_LOGO), (Kind::Slack, SLACK_LOGO)] {
            let caption = dest::privacy_caption(kind.locality(), kind.service());
            let entry = destination_entry(kind, kind.label().into(), caption.clone());
            assert_eq!(entry.image, Some(logo));
            assert!(logo.starts_with(b"\x89PNG\r\n\x1a\n"));
            assert_eq!(entry.subtitle.as_deref(), Some(caption.as_str()));
            assert!(entry.enabled);
            assert!(!entry.header);
        }
    }

    #[test]
    fn unbranded_destinations_use_semantic_symbols() {
        for (kind, symbol) in [(Kind::Webhook, "link"), (Kind::Folder, "folder")] {
            let entry = destination_entry(kind, kind.label().into(), String::new());
            assert!(entry.image.is_none());
            assert_eq!(entry.symbol, Some(symbol));
        }
    }
}
