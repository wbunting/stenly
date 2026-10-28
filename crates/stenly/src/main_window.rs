//! Main window: sidebar + Home (coming up, recent notes) + Settings pages.

use crate::controller::{Controller, KodyState, Status};
use crate::icons::{self, icon};
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, Btn};
use chrono::{Local, Utc};
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, Entity, FontWeight, IntoElement, ParentElement, PathPromptOptions,
    Render, SharedString, Styled, Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions, div,
    prelude::*, px, size,
};
use gpui::Focusable;
use std::collections::HashSet;
use std::time::Duration;
use stenly_core::platform::{Permission, PermissionState};
use stenly_core::util::collapse_tilde;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Page {
    Home,
    Preferences,
    You,
    Calendar,
    Transcription,
    Summaries,
    Dictation,
    History,
    Microphones,
    Notes,
    Destinations,
    Context,
    Permissions,
    Templates,
    Recipes,
    Trash,
}

pub struct MainWindow {
    controller: Entity<Controller>,
    pub(crate) browser: Entity<crate::meeting_browser::MeetingBrowser>,
    /// Search palette (⌘F / ⌘K or the magnifier by the traffic lights), while open.
    search: Option<(Entity<crate::search_palette::SearchPalette>, Subscription)>,
    pub(crate) meeting_match: Option<usize>,
    pub(crate) meeting_scroll: gpui::ScrollHandle,
    pub(crate) match_anchor: gpui::ScrollAnchor,
    match_pending: bool,
    /// Smoothed meter values (0..1) for You / Them.
    meter: [f32; 2],
    pub(crate) page: Page,
    /// Open meeting (read view), shown instead of the page body.
    pub(crate) meeting: Option<crate::meeting_view::MeetingDetail>,
    /// In-flight reload of the open meeting, and when it was last read.
    meeting_task: Option<Task<()>>,
    meeting_checked: std::time::Instant,
    /// Last live transcript read, shown while the transcriber holds the lock.
    live_cache: std::cell::RefCell<Vec<stenly_core::transcript::Segment>>,
    pub(crate) show_transcript: bool,
    /// Speaker list + rename for the open meeting.
    speaker_editor: Option<(Entity<crate::meeting_view::SpeakerEditor>, Subscription)>,
    /// "…" menu on the meeting view.
    menu_open: bool,
    /// Calendar event whose details card is open (Coming Up → click), and where.
    event_card: Option<(stenly_core::calendar::CalendarEvent, gpui::Point<gpui::Pixels>)>,
    /// Brief status notice in the bottom-right corner ("Copied").
    toast: Option<crate::toast::Toast>,
    toast_seq: u64,
    /// One encrypted share upload or revoke is in flight for the open meeting.
    sharing: bool,
    /// The meeting just moved to the Trash (its new folder), for Undo.
    pub(crate) undo_trash: Option<(std::path::PathBuf, std::time::Instant)>,
    /// Trash page contents, refreshed when it opens or changes.
    pub(crate) trash: Vec<(std::path::PathBuf, stenly_core::session::SessionMeta)>,
    kody_url: Entity<TextInput>,
    ics_url: Entity<TextInput>,
    api_endpoint: Entity<TextInput>,
    api_model: Entity<TextInput>,
    api_key: Entity<TextInput>,
    jargon: Entity<TextInput>,
    /// Settings → You.
    me_name: Entity<TextInput>,
    me_email: Entity<TextInput>,
    me_alias: Entity<TextInput>,
    me_label: Entity<TextInput>,
    /// Dictation settings.
    pub(crate) sc_main: Entity<crate::shortcut_field::ShortcutField>,
    pub(crate) sc_second: Entity<crate::shortcut_field::ShortcutField>,
    pub(crate) sc_paste: Entity<crate::shortcut_field::ShortcutField>,
    pub(crate) rep_from: Entity<TextInput>,
    pub(crate) rep_to: Entity<TextInput>,
    pub(crate) history: Vec<stenly_core::dictation::Dictation>,
    pub(crate) history_query: Entity<TextInput>,
    /// Saved timestamps keep disclosure state stable when the list is filtered.
    pub(crate) expanded_dictations: HashSet<chrono::DateTime<Local>>,
    pub(crate) overflowing_dictations: HashSet<chrono::DateTime<Local>>,
    pub(crate) mics: Vec<stenly_core::platform::InputDevice>,
    /// Send to… and Settings → Destinations.
    pub(crate) dest_ui: Entity<crate::destinations_ui::DestUi>,
    pub(crate) context_ui: Entity<crate::context_ui::ContextUi>,
    /// The Context card on the open meeting is expanded.
    pub(crate) context_open: bool,
    /// Templates & recipes: running recipes, the library editor.
    pub(crate) library: crate::library_pages::LibraryState,
    _subs: Vec<Subscription>,
}

pub fn open(controller: Entity<Controller>, page: Option<Page>, cx: &mut App) -> Option<WindowHandle<MainWindow>> {
    for w in cx.windows() {
        if let Some(h) = w.downcast::<MainWindow>() {
            let _ = h.update(cx, |this, window, cx| {
                if let Some(p) = page {
                    this.page = p;
                }
                window.activate_window();
                cx.notify();
            });
            cx.activate(true);
            return Some(h);
        }
    }
    // Dev: STENLY_WINDOW_SIZE=WxH for screenshots.
    let (w, h) = std::env::var("STENLY_WINDOW_SIZE")
        .ok()
        .and_then(|v| v.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))))
        .unwrap_or((980., 700.));
    let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Stenly".into()),
                    appears_transparent: true,
                    // Unified toolbar height (52pt): lights centred in it.
                    traffic_light_position: Some(gpui::point(px(18.), px(18.))),
                }),
                // Lets the sidebar show the system's translucent material.
                // STENLY_GLASS=1: experimental real Liquid Glass (NSGlassEffectView)
                // behind GPUI; the window must be transparent for it to show.
                window_background: if glass_enabled() { gpui::WindowBackgroundAppearance::Transparent } else { gpui::WindowBackgroundAppearance::Blurred },
                window_min_size: Some(size(px(760.), px(520.))),
                app_id: Some("stenly".into()),
                ..Default::default()
            },
            |window, cx| {
                crate::configure_native(window, controller.read(cx).shell.as_ref());
                cx.new(|cx| {
                    let sub = cx.observe(&controller, |_: &mut MainWindow, _, cx| cx.notify());
                    let appearance = cx.observe_window_appearance(window, |_: &mut MainWindow, _, cx| {
                        theme::sync(cx);
                        cx.notify();
                    });
                    let ctrl_act = controller.clone();
                    let act = cx.observe_window_activation(window, move |this: &mut MainWindow, window, cx| {
                        if window.is_window_active() {
                            if this.page == Page::Home && this.meeting.is_none() {
                                let cfg = ctrl_act.read(cx).config.clone();
                                this.browser.update(cx, |b, cx| b.refresh(cfg, cx));
                            }
                            ctrl_act.update(cx, |c, cx| {
                                c.refresh_permissions();
                                cx.notify();
                            });
                        }
                    });
                    cx.spawn_in(window, async move |this, cx| {
                        let mut tick = 0u32;
                        loop {
                            // ~30 fps while recording (meters), 2 fps otherwise.
                            cx.background_executor().timer(Duration::from_millis(33)).await;
                            tick = tick.wrapping_add(1);
                            let alive = this.update(cx, |this, cx| {
                                let c = this.controller.read(cx);
                                let recording = c.is_recording();
                                if recording {
                                    if let Some(l) = c.levels.clone() {
                                        let target = |db: f32| ((db + 60.0) / 50.0).clamp(0.0, 1.0);
                                        for (i, ch) in [stenly_core::platform::Channel::Mic, stenly_core::platform::Channel::System].into_iter().enumerate() {
                                            let t = target(l.get(ch));
                                            let m = &mut this.meter[i];
                                            // fast attack, slow release
                                            *m += (t - *m) * if t > *m { 0.6 } else { 0.12 };
                                        }
                                    }
                                    cx.notify();
                                } else if tick % 15 == 0 && !c.downloads.is_empty() {
                                    cx.notify();
                                }
                            });
                            if alive.is_err() {
                                break;
                            }
                        }
                    })
                    .detach();
                    let kody_default = {
                        let c = controller.read(cx);
                        if c.config.calendar.kody_url.is_empty() { default_kody_url() } else { c.config.calendar.kody_url.clone() }
                    };
                    let kody_url = cx.new(|cx| {
                        let mut t = TextInput::new(cx, "https://your-kody.example.com");
                        t.set_text(kody_default, cx);
                        t
                    });
                    let ics_url = cx.new(|cx| TextInput::new(cx, "https://calendar.google.com/calendar/ical/…/basic.ics"));
                    let ctrl = controller.clone();
                    let ics_sub = cx.subscribe(&ics_url, move |this: &mut MainWindow, input, ev: &InputEvent, cx| {
                        if matches!(ev, InputEvent::Submit) {
                            let url = input.read(cx).text().trim().to_string();
                            if !url.is_empty() {
                                let mut urls = ctrl.read(cx).config.calendar.ics_urls.clone();
                                if !urls.contains(&url) {
                                    urls.push(url);
                                }
                                ctrl.update(cx, |c, cx| c.set_list("calendar.ics_urls", &urls, cx));
                                input.update(cx, |t, cx| t.set_text("", cx));
                            }
                            let _ = this;
                        }
                    });
                    // Dev: STENLY_PAGE=home|preferences|calendar|models|notes|permissions
                    let env_page = std::env::var("STENLY_PAGE").ok().and_then(|p| match p.as_str() {
                        "home" => Some(Page::Home),
                        "preferences" => Some(Page::Preferences),
                        "you" | "me" => Some(Page::You),
                        "calendar" => Some(Page::Calendar),
                        "models" | "transcription" => Some(Page::Transcription),
                        "summaries" => Some(Page::Summaries),
                        "dictation" => Some(Page::Dictation),
                        "history" => Some(Page::History),
                        "microphones" | "audio" => Some(Page::Microphones),
                        "notes" => Some(Page::Notes),
                        "destinations" => Some(Page::Destinations),
                        "context" => Some(Page::Context),
                        "permissions" => Some(Page::Permissions),
                        "templates" => Some(Page::Templates),
                        "recipes" => Some(Page::Recipes),
                        "trash" => Some(Page::Trash),
                        _ => None,
                    });
                    // Summary API fields: saved on Enter or when focus leaves the field.
                    let (api_cfg, has_key) = {
                        let c = controller.read(cx);
                        (c.config.llm.api.clone(), c.api_key_saved)
                    };
                    let field = |cx: &mut Context<MainWindow>, ph: &str, text: &str| {
                        let ph = ph.to_string();
                        let text = text.to_string();
                        cx.new(move |cx| {
                            let mut t = TextInput::new(cx, ph);
                            t.set_text(text, cx);
                            t
                        })
                    };
                    let api_endpoint = field(cx, "https://api.openai.com/v1", &api_cfg.endpoint);
                    let api_model = field(cx, "gpt-5-mini", &api_cfg.model);
                    let api_key = cx.new(|cx| TextInput::new(cx, if has_key { "Saved in Keychain · type to replace" } else { "sk-…" }).masked());
                    let jargon = cx.new(|cx| TextInput::new(cx, "Add a term and press Return"));
                    let me_cfg = controller.read(cx).config.me.clone();
                    let me_name = field(cx, "Your name, as meeting apps show it", &me_cfg.name);
                    let me_email = cx.new(|cx| TextInput::new(cx, "Add an address and press Return"));
                    let me_alias = cx.new(|cx| TextInput::new(cx, "Add a name and press Return"));
                    let me_label = field(cx, "Your first name", &me_cfg.label);
                    let mut subs = vec![sub, ics_sub, act, appearance];
                    {
                        // name: saved on Enter or when focus leaves the field
                        let ctrl = controller.clone();
                        let input = me_name.clone();
                        let save = move |cx: &mut App| {
                            let v = input.read(cx).text().trim().to_string();
                            if v != ctrl.read(cx).config.me.name {
                                ctrl.update(cx, |c, cx| c.set("me.name", v.as_str(), cx));
                            }
                        };
                        let save2 = save.clone();
                        subs.push(cx.subscribe(&me_name, move |_, _, ev: &InputEvent, cx| {
                            if matches!(ev, InputEvent::Submit) {
                                save(cx)
                            }
                        }));
                        let fh = me_name.read(cx).focus_handle(cx);
                        subs.push(cx.on_blur(&fh, window, move |_, _, cx| save2(cx)));
                    }
                    {
                        // how your lines are labelled: saved like the name
                        let ctrl = controller.clone();
                        let input = me_label.clone();
                        let save = move |cx: &mut App| {
                            let v = input.read(cx).text().trim().to_string();
                            if v != ctrl.read(cx).config.me.label {
                                ctrl.update(cx, |c, cx| c.set("me.label", v.as_str(), cx));
                            }
                        };
                        let save2 = save.clone();
                        subs.push(cx.subscribe(&me_label, move |_, _, ev: &InputEvent, cx| {
                            if matches!(ev, InputEvent::Submit) {
                                save(cx)
                            }
                        }));
                        let fh = me_label.read(cx).focus_handle(cx);
                        subs.push(cx.on_blur(&fh, window, move |_, _, cx| save2(cx)));
                    }
                    for (input, key) in [(me_email.clone(), "me.emails"), (me_alias.clone(), "me.aliases")] {
                        let ctrl = controller.clone();
                        subs.push(cx.subscribe(&input, move |_, input, ev: &InputEvent, cx| {
                            if !matches!(ev, InputEvent::Submit) {
                                return;
                            }
                            // "a@example.com, b@y.com" adds two
                            let new: Vec<String> = input.read(cx).text().split([',', ';']).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
                            if new.is_empty() {
                                return;
                            }
                            ctrl.update(cx, |c, cx| c.add_to_list(key, &new, cx));
                            input.update(cx, |t, cx| t.set_text("", cx));
                        }));
                    }
                    for (input, key) in [(api_endpoint.clone(), "llm.api.endpoint"), (api_model.clone(), "llm.api.model")] {
                        let save = {
                            let ctrl = controller.clone();
                            let input = input.clone();
                            move |cx: &mut App| {
                                let v = input.read(cx).text().trim().to_string();
                                let cur = {
                                    let c = &ctrl.read(cx).config.llm.api;
                                    if key.ends_with("endpoint") { c.endpoint.clone() } else { c.model.clone() }
                                };
                                if v != cur {
                                    ctrl.update(cx, |c, cx| {
                                        c.set(key, v.as_str(), cx);
                                        c.api_test = crate::controller::ApiTest::Idle;
                                    });
                                }
                            }
                        };
                        let save2 = save.clone();
                        subs.push(cx.subscribe(&input, move |_, _, ev: &InputEvent, cx| {
                            if matches!(ev, InputEvent::Submit) {
                                save(cx)
                            }
                        }));
                        let fh = input.read(cx).focus_handle(cx);
                        subs.push(cx.on_blur(&fh, window, move |_, _, cx| save2(cx)));
                    }
                    {
                        let ctrl = controller.clone();
                        let save = move |input: &Entity<TextInput>, cx: &mut App| {
                            let v = input.read(cx).text().trim().to_string();
                            if !v.is_empty() {
                                ctrl.update(cx, |c, cx| c.set_api_key(&v, cx));
                                input.update(cx, |t, cx| t.set_text("", cx));
                            }
                        };
                        let save2 = save.clone();
                        subs.push(cx.subscribe(&api_key, move |_, input, ev: &InputEvent, cx| {
                            if matches!(ev, InputEvent::Submit) {
                                save(&input, cx)
                            }
                        }));
                        let fh = api_key.read(cx).focus_handle(cx);
                        let k = api_key.clone();
                        subs.push(cx.on_blur(&fh, window, move |_, _, cx| save2(&k, cx)));
                    }
                    {
                        let ctrl = controller.clone();
                        subs.push(cx.subscribe(&jargon, move |_, input, ev: &InputEvent, cx| {
                            if matches!(ev, InputEvent::Submit) {
                                // "Vuon, Viewon" adds two terms
                                let new: Vec<String> = input.read(cx).text().split([',', ';']).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
                                if new.is_empty() {
                                    return;
                                }
                                let mut terms = ctrl.read(cx).config.asr.vocabulary.clone();
                                for t in new {
                                    if !terms.iter().any(|x| x.eq_ignore_ascii_case(&t)) {
                                        terms.push(t);
                                    }
                                }
                                ctrl.update(cx, |c, cx| c.set_list("asr.vocabulary", &terms, cx));
                                input.update(cx, |t, cx| t.set_text("", cx));
                            }
                        }));
                    }
                    // Dictation shortcut fields: save on change; pause the global
                    // listener while recording so pressing the key doesn't dictate.
                    let dcfg = controller.read(cx).config.dictation.clone();
                    let sc = |cx: &mut Context<MainWindow>, v: &str, lone: bool| {
                        let h = stenly_core::platform::Hotkey::parse(v);
                        cx.new(|cx| crate::shortcut_field::ShortcutField::new(h, lone, cx))
                    };
                    let sc_main = sc(cx, &dcfg.shortcut, true);
                    let sc_second = sc(cx, &dcfg.shortcut2, true);
                    let sc_paste = sc(cx, &dcfg.paste_last_shortcut, false);
                    for (field, key) in [(sc_main.clone(), "dictation.shortcut"), (sc_second.clone(), "dictation.shortcut2"), (sc_paste.clone(), "dictation.paste_last_shortcut")] {
                        let ctrl = controller.clone();
                        subs.push(cx.subscribe(&field, move |_, _, ev: &crate::shortcut_field::ShortcutEvent, cx| match ev {
                            crate::shortcut_field::ShortcutEvent::Changed(h) => {
                                let v = h.as_ref().map(|h| h.to_config()).unwrap_or_default();
                                ctrl.update(cx, |c, cx| c.set(key, v.as_str(), cx));
                            }
                            crate::shortcut_field::ShortcutEvent::Recording(on) => {
                                if let Some(d) = crate::dictation_entity() {
                                    d.update(cx, |d, cx| d.pause_shortcuts(*on, cx));
                                }
                            }
                        }));
                    }
                    let rep_from = cx.new(|cx| TextInput::new(cx, "Heard as (e.g. view on, vuong)"));
                    let rep_to = cx.new(|cx| TextInput::new(cx, "Write as (e.g. Vuon)"));
                    {
                        let (ctrl, from, to) = (controller.clone(), rep_from.clone(), rep_to.clone());
                        let add = move |cx: &mut App| {
                            let f = from.read(cx).text().trim().to_string();
                            let t = to.read(cx).text().trim().to_string();
                            if f.is_empty() || t.is_empty() {
                                return;
                            }
                            ctrl.update(cx, |c, cx| c.add_replacement(&f, &t, cx));
                            from.update(cx, |x, cx| x.set_text("", cx));
                            to.update(cx, |x, cx| x.set_text("", cx));
                        };
                        let add2 = add.clone();
                        subs.push(cx.subscribe(&rep_from, move |_, _, ev: &InputEvent, cx| {
                            if matches!(ev, InputEvent::Submit) {
                                add(cx)
                            }
                        }));
                        subs.push(cx.subscribe(&rep_to, move |_, _, ev: &InputEvent, cx| {
                            if matches!(ev, InputEvent::Submit) {
                                add2(cx)
                            }
                        }));
                    }
                    let browser = cx.new(|cx| crate::meeting_browser::MeetingBrowser::new(controller.read(cx).config.clone(), cx));
                    subs.push(cx.subscribe(&browser, |this: &mut MainWindow, _, ev: &crate::meeting_browser::OpenMeeting, cx| {
                        this.open_search_hit(ev, cx);
                    }));
                    let mut previous_revision = controller.read(cx).library_revision;
                    subs.push(cx.observe(&controller, move |this: &mut MainWindow, ctrl, cx| {
                        let revision = ctrl.read(cx).library_revision;
                        if revision != previous_revision {
                            previous_revision = revision;
                            let cfg = ctrl.read(cx).config.clone();
                            this.browser.update(cx, |b, cx| b.refresh(cfg, cx));
                        }
                    }));
                    let meeting_scroll = gpui::ScrollHandle::new();
                    let match_anchor = gpui::ScrollAnchor::for_handle(meeting_scroll.clone());
                    let history_query = cx.new(|cx| TextInput::new(cx, "Search dictations"));
                    subs.push(cx.observe(&history_query, |_, _, cx| cx.notify()));
                    let dest_ui = {
                        let ctrl = controller.clone();
                        cx.new(|cx| crate::destinations_ui::DestUi::new(ctrl, cx))
                    };
                    crate::destinations_ui::DestUi::attach(&dest_ui, window, cx);
                    subs.push(cx.observe(&dest_ui, |_, _, cx| cx.notify()));
                    subs.push(cx.subscribe(&dest_ui, |this: &mut MainWindow, _, ev: &crate::destinations_ui::DestEvent, cx| match ev {
                        crate::destinations_ui::DestEvent::Notice(msg, kind) => this.notify_toast(msg.clone(), *kind, cx),
                    }));
                    let context_ui = {
                        let ctrl = controller.clone();
                        cx.new(|cx| crate::context_ui::ContextUi::new(ctrl, cx))
                    };
                    subs.push(cx.observe(&context_ui, |_, _, cx| cx.notify()));
                    if page.or(env_page) == Some(Page::Context) {
                        context_ui.update(cx, |u, cx| u.on_open(cx));
                    }
                    // Opened straight onto the Trash (STENLY_PAGE=trash): load it now.
                    let trash = if page.or(env_page) == Some(Page::Trash) { controller.read(cx).trash() } else { vec![] };
                    let mut mw = MainWindow {
                        controller,
                        browser,
                        search: None,
                        meeting_match: None,
                        meeting_scroll,
                        match_anchor,
                        match_pending: false,
                        meter: [0.0; 2],
                        page: page.or(env_page).unwrap_or(Page::Home),
                        meeting: None,
                        meeting_task: None,
                        meeting_checked: std::time::Instant::now(),
                        live_cache: Default::default(),
                        show_transcript: false,
                        speaker_editor: None,
                        menu_open: false,
                        event_card: None,
                        toast: None,
                        toast_seq: 0,
                        sharing: false,
                        undo_trash: None,
                        trash,
                        kody_url,
                        ics_url,
                        api_endpoint,
                        api_model,
                        api_key,
                        jargon,
                        me_name,
                        me_email,
                        me_alias,
                        me_label,
                        sc_main,
                        sc_second,
                        sc_paste,
                        rep_from,
                        rep_to,
                        history: stenly_core::dictation::history(),
                        history_query,
                        expanded_dictations: HashSet::new(),
                        overflowing_dictations: HashSet::new(),
                        mics: vec![],
                        dest_ui,
                        context_ui,
                        context_open: std::env::var_os("STENLY_CONTEXT_OPEN").is_some(),
                        library: Default::default(),
                        _subs: subs,
                    };
                    if matches!(mw.page, Page::Microphones) {
                        mw.refresh_mics(cx);
                    }
                    if let Ok(k) = std::env::var("STENLY_EDIT_DESTINATION") {
                        mw.page = Page::Destinations;
                        crate::destinations_ui::dev_open_editor(&mw.dest_ui, &k, cx);
                    }
                    // Dev: STENLY_FLASH=<message> shows a status notice (prefix "!" for an error).
                    if let Ok(m) = std::env::var("STENLY_FLASH") {
                        match m.strip_prefix('!') {
                            Some(e) => mw.flash_error(e.to_string(), cx),
                            None => mw.flash(m, cx),
                        }
                    }
                    // Dev: STENLY_OPEN_MEETING=<session dir> opens a meeting directly.
                    if let Ok(d) = std::env::var("STENLY_OPEN_MEETING") {
                        let d: std::path::PathBuf = d.into();
                        mw.controller.update(cx, |c, cx| c.load_meeting_context(&d, cx));
                        // Dev: STENLY_GATHER_AGAIN=1 presses the open meeting's Gather Again.
                        if std::env::var_os("STENLY_GATHER_AGAIN").is_some() {
                            let d2 = d.clone();
                            mw.controller.update(cx, |c, cx| c.gather_context(d2, true, cx));
                        }
                        mw.meeting = crate::meeting_view::MeetingDetail::load(d);
                        mw.show_transcript = std::env::var_os("STENLY_SHOW_TRANSCRIPT").is_some();
                        mw.menu_open = std::env::var_os("STENLY_MENU_OPEN").is_some();
                        // Dev: STENLY_SEND_TO=<destination id> sends the open meeting on launch.
                        if let (Ok(id), Some(m)) = (std::env::var("STENLY_SEND_TO"), &mw.meeting) {
                            let dir = m.dir.clone();
                            mw.dest_ui.update(cx, |d, cx| d.send(dir, id, cx));
                        }
                        // Dev: STENLY_TRASH=1 moves the open meeting to the Trash on
                        // launch (what the ••• menu's Move to Trash does).
                        if std::env::var_os("STENLY_TRASH").is_some()
                            && let Some(dir) = mw.meeting.take().map(|m| m.dir)
                        {
                            match mw.controller.update(cx, |c, cx| c.trash_meeting(dir, cx)) {
                                Ok(t) => mw.offer_undo_trash(t, cx),
                                Err(e) => tracing::warn!(error = %e, "dev trash failed"),
                            }
                        }
                        if std::env::var_os("STENLY_SEND_MENU").is_some() {
                            if let Some(m) = &mw.meeting { crate::destinations_ui::dev_open_menu(&mw.dest_ui, m.dir.clone(), window, cx); }
                        }
                        // Dev: STENLY_BACK_AFTER_MS=n leaves the meeting after n ms (like the back button).
                        if let Some(ms) = std::env::var("STENLY_BACK_AFTER_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
                            cx.spawn(async move |this, cx| {
                                cx.background_executor().timer(Duration::from_millis(ms)).await;
                                let _ = this.update(cx, |this, cx| {
                                    this.meeting = None;
                                    cx.notify();
                                });
                            })
                            .detach();
                        }
                    }
                    // Dev: STENLY_MENU_TEST=1 opens a native menu the way the "…" button
                    // does (from a spawned task), to check nothing panics while
                    // NSMenu's nested run loop drains GPUI's queue.
                    if std::env::var_os("STENLY_MENU_TEST").is_some() {
                        cx.spawn_in(window, async move |_this, cx| {
                            cx.background_executor().timer(Duration::from_millis(1500)).await;
                            let Ok(view) = cx.update(|w, _| ui::native_view(w)) else { return };
                            let picked = ui::menu_in_view(view, &["Copy Notes", "Copy Transcript", "-", "Show in Finder"], None, gpui::point(px(700.), px(60.)));
                            tracing::info!(?picked, "menu test closed");
                        })
                        .detach();
                    }
                    // Dev: STENLY_EVENT_CARD=1 (with STENLY_DEMO_EVENTS) opens the first event's card.
                    if std::env::var_os("STENLY_EVENT_CARD").is_some() {
                        if let Some(ev) = demo_events(Utc::now()).into_iter().nth(2) {
                            mw.event_card = Some((ev, gpui::point(px(420.), px(330.))));
                        }
                    }
                    // Dev: STENLY_SEARCH=<query> opens the search palette with that query.
                    if let Ok(q) = std::env::var("STENLY_SEARCH") {
                        mw.open_search(window, cx);
                        if let Some((p, _)) = &mw.search {
                            let input = p.read(cx).query.clone();
                            input.update(cx, |i, cx| i.set_text(q, cx));
                        }
                    }
                    mw
                })
            },
        )
        .ok()?;
    cx.activate(true);
    Some(handle)
}

/// macOS 26 toolbar item: a 32pt round button. Drawn entirely by GPUI (a
/// soft fill + hairline that reads like the system's glass buttons) rather
/// than a native NSGlassEffectView: those live outside GPUI's frame and lag a
/// frame behind, which left a ghost circle behind after the button went away.
fn glass_icon_button(id: &'static str, s: crate::icons::Sym) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(32.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(theme::button())
        .border_1()
        .border_color(theme::divider())
        .hover(|s| s.bg(theme::button_hover()))
        .active(|s| s.bg(theme::button_hover()))
        .child(crate::icons::icon_w(s, 14., crate::icons::Weight::Medium, theme::text()))
}

/// A capsule surface behind a control (Record button, recording capsule),
/// drawn by GPUI so it can never outlive the control.
fn glass_spot(el: impl IntoElement, radius: f32) -> gpui::Div {
    div().rounded(px(radius)).bg(theme::button()).border_1().border_color(theme::divider()).child(el)
}

/// Open Settings → Destinations (from a meeting's Send to… menu).
pub fn open_destinations(cx: &mut App) {
    for w in cx.windows() {
        if let Some(h) = w.downcast::<MainWindow>() {
            let _ = h.update(cx, |this, _, cx| this.go(Page::Destinations, cx));
        }
    }
}

/// Calendar descriptions are often HTML (Google); show them as plain text.
fn strip_html(s: &str) -> String {
    let s = s.replace("<br>", "\n").replace("<br/>", "\n").replace("<br />", "\n").replace("</p>", "\n");
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'")
}

/// Dev: STENLY_DEMO_EVENTS=1 fills Coming Up with sample meetings (snapshots).
fn demo_events(now: chrono::DateTime<Utc>) -> Vec<stenly_core::calendar::CalendarEvent> {
    use stenly_core::calendar::{Attendee, CalendarEvent};
    use stenly_core::meeting_url::{MeetingLink, Provider};
    let mk = |title: &str, start_min: i64, len: i64, prov: Option<Provider>, who: &[&str], color: &str| CalendarEvent {
        id: title.into(),
        uid: title.into(),
        title: title.into(),
        start: now + chrono::Duration::minutes(start_min),
        end: now + chrono::Duration::minutes(start_min + len),
        link: prov.map(|p| MeetingLink { provider: p, url: "https://example.com".into() }),
        organizer: who.first().map(|n| Attendee { name: Some(n.to_string()), email: format!("{}@example.com", n.split(' ').next().unwrap_or("").to_lowercase()), status: None }),
        attendees: who
            .iter()
            .map(|n| Attendee { name: Some(n.to_string()), email: format!("{}@example.com", n.split(' ').next().unwrap_or("").to_lowercase()), status: None })
            .chain(std::iter::once(Attendee { name: Some("Alexander Rowan".into()), email: "me@example.com".into(), status: None }))
            .collect(),
        description: Some("Hi team! Sending this invite so we can align on priorities and next steps for the pilot.".into()),
        calendar: Some("me@example.com".into()),
        declined: false,
        recurring: false,
        html_link: Some("https://calendar.google.com".into()),
        calendar_name: Some("alex@example.com".into()),
        color: Some(color.into()),
    };
    let day = 24 * 60;
    vec![
        mk("Design review", -10, 45, Some(Provider::GoogleMeet), &["Yumi Tanaka", "Sam Lee"], "#3f51b5"),
        mk("Weekly sync", 50, 30, Some(Provider::Zoom), &["Alex Kim", "Priya Shah", "Jordan Ruiz"], "#33b679"),
        mk("Andy, Example Partners / Shelley, Vuon", day + 30, 30, None, &["Andy Reed", "Shelley Morgan"], "#33b679"),
        mk("Seth, Example Labs / Vuon AI", day + 120, 60, Some(Provider::Zoom), &["Seth Vale", "Shelley Morgan"], "#33b679"),
        mk("Customer call — Acme onboarding and next steps for the pilot", 4 * day, 60, Some(Provider::Teams), &["Dana Whitfield", "Chris Moore"], "#f4511e"),
    ]
}

/// Titlebar search button: vertically centred on the traffic lights (placed at
/// y=18, 14pt tall), right-aligned with the sidebar's rows.
const SEARCH_BUTTON_TOP: f32 = 11.;
const SEARCH_BUTTON_SIZE: f32 = 28.;

/// Sidebar column width (incl. the glass panel's inset), in points.
const SIDEBAR_W: f32 = 216.;
/// Gap between the window edge and the floating sidebar panel (macOS 26: 8pt).
const SIDEBAR_INSET: f32 = 8.;
/// Rows sit this far inside the glass panel, on every side (incl. the bottom),
/// so their corners are concentric with the panel's.
const SIDEBAR_PAD: f32 = 10.;
/// Window corner with the unified toolbar (macOS 26), and the glass panel's
/// concentric corner inside it.
const TOOLBAR_WINDOW_RADIUS: f32 = 26.;
const SIDEBAR_PANEL_RADIUS: f32 = TOOLBAR_WINDOW_RADIUS - SIDEBAR_INSET;
/// Fallback window corner radius (macOS 26, titled window without an
/// NSToolbar, which is what GPUI creates). The real value is read at runtime.
const WINDOW_RADIUS: f32 = 16.;

/// Real Liquid Glass (NSGlassEffectView behind GPUI) on macOS 26+.
/// STENLY_GLASS=0 forces the pre-26 blurred-sidebar fallback.
pub fn glass_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        if std::env::var("STENLY_GLASS").as_deref() == Ok("0") {
            return false;
        }
        #[cfg(target_os = "macos")]
        {
            stenly_platform::appkit_ui::glass_available()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    })
}

/// The Kody instance this machine's agents already use, if any.
pub fn default_kody_url() -> String {
    std::env::var("STENLY_KODY_URL").unwrap_or_else(|_| {
        let p = directories::BaseDirs::new().map(|b| b.home_dir().join(".pi/agent/mcp.json"));
        p.and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v["mcpServers"]["kody"]["url"].as_str().map(|s| s.trim_end_matches("/mcp").to_string()))
            .unwrap_or_default()
    })
}

/// Source-list row, measured against a native NSOutlineView(.sourceList) on
/// macOS 26: 24pt rows (+4pt rhythm), 8pt-radius selection, 15pt glyph in the
/// label colour, 6pt glyph→label gap. The selection lives inside the panel's
/// own 10pt inset so it never touches the glass edge.
fn nav_item(id: &'static str, label: &'static str, data: crate::icons::Sym, active: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(28.))
        .px(px(8.))
        .rounded(px(theme::concentric(SIDEBAR_PANEL_RADIUS, SIDEBAR_PAD)))
        .text_size(px(theme::BODY))
        .text_color(theme::text())
        .when(active, |d| d.bg(theme::sidebar_selection()))
        .when(!active, |d| d.hover(|s| s.bg(theme::hover())))
        .child(div().w(px(18.)).flex().justify_center().child(icon(data, 14., if active { theme::text() } else { theme::muted() })))
        .child(label)
}

impl MainWindow {
    fn show_event(&mut self, ev: stenly_core::calendar::CalendarEvent, window: &mut Window, cx: &mut Context<Self>) {
        let pos = window.mouse_position();
        self.event_card = match &self.event_card {
            Some((open, _)) if open.id == ev.id => None,
            _ => Some((ev, pos)),
        };
        cx.notify();
    }

    /// Granola-style event card: title, when, description, open in calendar, guests.
    fn event_card(&self, ev: &stenly_core::calendar::CalendarEvent, _cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let s = ev.start.with_timezone(&Local);
        let e = ev.end.with_timezone(&Local);
        let when = format!("{} · {} – {}", s.format("%a, %b %-d"), s.format("%-I:%M"), e.format("%-I:%M %p"));
        let desc: Option<String> = ev.description.as_deref().map(strip_html).map(|d| d.trim().to_string()).filter(|d| !d.is_empty()).map(|d| {
            let one: String = d.split_whitespace().collect::<Vec<_>>().join(" ");
            if one.chars().count() > 180 { format!("{}…", one.chars().take(180).collect::<String>().trim_end()) } else { one }
        });
        let me = ev.calendar.clone().unwrap_or_default();
        let mut guests: Vec<stenly_core::calendar::Attendee> = ev.attendees.clone();
        // organizer first, then you last (like Granola)
        guests.sort_by_key(|a| (a.email.eq_ignore_ascii_case(&me), ev.organizer.as_ref().is_none_or(|o| !o.email.eq_ignore_ascii_case(&a.email))));
        let link = ev.html_link.clone();
        let join = ev.link.clone();
        let bar = ev.color.as_deref().map(theme::hex).unwrap_or(theme::accent());
        let guest_rows: Vec<AnyElement> = guests
            .iter()
            .take(12)
            .map(|a| {
                let name = a.display();
                let initial = name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
                let is_me = a.email.eq_ignore_ascii_case(&me);
                let declined = a.status.as_deref() == Some("DECLINED");
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(30.))
                    .child(div().size(px(22.)).flex_shrink_0().rounded_full().bg(theme::inset()).border_1().border_color(theme::divider()).flex().items_center().justify_center().text_size(px(11.)).text_color(theme::muted()).child(initial))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(theme::BODY)).when(declined, |d| d.text_color(theme::faint()).line_through()).child(name))
                    .when(is_me, |d| d.child(div().text_size(px(theme::SUBHEAD)).text_color(theme::faint()).child("(me)")))
                    .into_any_element()
            })
            .collect();
        let more = guests.len().saturating_sub(12);
        div()
            .id("event-card")
            .w(px(320.))
            .rounded(px(12.))
            // opaque: this sits over text (popover material without real vibrancy)
            .bg(theme::pick(gpui::rgb(0xfbfbfb), gpui::rgb(0x2a2a2a)))
            .border_1()
            .border_color(theme::divider())
            .shadow_lg()
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .p(px(14.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap(px(10.))
                            .child(div().mt(px(3.)).size(px(10.)).flex_shrink_0().rounded(px(3.)).bg(bar))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .child(div().text_size(px(theme::BODY)).font_weight(FontWeight::SEMIBOLD).child(ev.title.clone()))
                                    .child(div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(when))
                                    .when_some(ev.calendar_name.clone(), |d, n| d.child(div().text_size(px(theme::SUBHEAD)).text_color(theme::faint()).truncate().child(n))),
                            ),
                    )
                    .when_some(desc, |d, t| d.child(div().text_size(px(theme::SUBHEAD)).line_height(px(16.)).text_color(theme::muted()).child(t)))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .when_some(join, |d, l| {
                                let url = l.url.clone();
                                d.child(ui::button_lead_icon("ev-join", format!("Join {}", l.provider.label()), Btn::Primary, icons::VIDEO).on_click(move |_, _, cx| cx.open_url(&url)))
                            })
                            .when_some(link, |d, u| d.child(ui::button_lead_icon("ev-open", "Open in Google Calendar", Btn::Outline, icons::CALENDAR).on_click(move |_, _, cx| cx.open_url(&u)))),
                    ),
            )
            .when(!guest_rows.is_empty(), |d| {
                d.child(div().h(px(1.)).bg(theme::divider())).child(
                    div()
                        .px(px(14.))
                        .py(px(8.))
                        .flex()
                        .flex_col()
                        .child(div().pb(px(4.)).text_size(px(theme::SUBHEAD)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted()).child(format!("{} guests", guests.len())))
                        .children(guest_rows)
                        .when(more > 0, |d| d.child(div().text_size(px(theme::SUBHEAD)).text_color(theme::faint()).child(format!("and {more} more")))),
                )
            })
    }

    /// Open a meeting from the library or search, jumping to the matched segment.
    fn open_search_hit(&mut self, ev: &crate::meeting_browser::OpenMeeting, cx: &mut Context<Self>) {
        self.open_meeting(ev.dir.clone(), cx);
        self.meeting_match = ev.segment;
        self.show_transcript = ev.segment.is_some();
        self.match_pending = ev.segment.is_some();
    }

    /// Show the search palette (or focus it again if it's already open).
    pub(crate) fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::search_palette::{PaletteEvent, SearchPalette};
        if self.search.is_none() {
            let cfg = self.controller.read(cx).config.clone();
            let palette = cx.new(|cx| SearchPalette::new(cfg, cx));
            let sub = cx.subscribe_in(&palette, window, |this: &mut MainWindow, _, ev: &PaletteEvent, window, cx| {
                if let PaletteEvent::Open(hit) = ev {
                    this.open_search_hit(hit, cx);
                }
                this.close_search(window, cx);
            });
            self.search = Some((palette, sub));
        }
        if let Some((p, _)) = &self.search {
            window.focus(&p.read(cx).focus_handle(cx), cx);
        }
        cx.notify();
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.take().is_some() {
            window.blur(cx);
            cx.notify();
        }
    }

    pub fn open_meeting(&mut self, dir: std::path::PathBuf, cx: &mut Context<Self>) {
        self.show_transcript = false;
        self.meeting_match = None;
        self.match_pending = false;
        self.meeting_scroll.set_offset(gpui::point(px(0.), px(0.)));
        self.meeting = None;
        let d = dir.clone();
        self.controller.update(cx, |c, cx| c.load_meeting_context(&d, cx));
        // Read off the main thread (iCloud Drive may download the files first).
        self.meeting_task = Some(cx.spawn(async move |this, cx| {
            let m = cx.background_spawn(async move { crate::meeting_view::MeetingDetail::load(dir) }).await;
            let _ = this.update(cx, |this, cx| {
                this.meeting_task = None;
                this.meeting_checked = std::time::Instant::now();
                this.meeting = m;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Settings → Transcription → Speakers: names on/off + remembered voices.
    fn speaker_names_row(&self, cfg: &stenly_core::config::Config) -> AnyElement {
        let ctrl = self.controller.clone();
        let on = cfg.asr.speaker_names;
        let store = stenly_core::voices::VoiceStore::load();
        let voices = store.people().count();
        let anything = !store.is_empty();
        let desc: SharedString = if voices > 0 {
            format!("From the meeting app's speaker highlight, the invite and the conversation. Remembers {voices} voice{} you've named (on this Mac).", if voices == 1 { "" } else { "s" }).into()
        } else {
            "From the meeting app's speaker highlight, the invite and the conversation. Voices you name are remembered on this Mac.".into()
        };
        // also shown when only unconfirmed voices are kept (first calls named from the invite)
        let forget = anything.then(|| {
            ui::button("forget-voices", "Forget Voices", Btn::Ghost).on_click(|_, _, cx| {
                let mut s = stenly_core::voices::VoiceStore::load();
                s.clear();
                let _ = s.save();
                cx.refresh_windows();
            })
        });
        ui::row(
            None,
            "Name speakers",
            Some(desc),
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .children(forget)
                .child(ui::toggle("speaker-names", on, move |_, _, cx| ctrl.update(cx, |c, cx| c.set("asr.speaker_names", !on, cx)))),
        )
        .into_any_element()
    }

    /// Speakers strip of the open meeting (rename on click).
    pub(crate) fn speaker_strip(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.meeting.as_ref()?;
        let t = m.transcript.clone()?;
        if !self.controller.read(cx).config.asr.speaker_names {
            return None;
        }
        let stale = self.speaker_editor.as_ref().is_none_or(|(e, _)| e.read(cx).dir() != m.dir.as_path());
        if stale {
            let ed = cx.new(|_| crate::meeting_view::SpeakerEditor::new(m.dir.clone()));
            let sub = cx.subscribe(&ed, |this: &mut Self, _, _: &crate::meeting_view::SpeakerEditorEvent, cx| {
                this.refresh_meeting(true, cx);
                cx.notify();
            });
            self.speaker_editor = Some((ed, sub));
        }
        let (ed, _) = self.speaker_editor.as_ref()?;
        if !crate::meeting_view::SpeakerEditor::has_strip(&t) {
            return None;
        }
        ed.update(cx, |e, cx| e.set_transcript(Some(t), cx));
        Some(ed.clone().into_any_element())
    }

    /// Reload the open meeting (e.g. after processing finished) on a
    /// background thread: its files may be in iCloud Drive, where a read can
    /// wait on a download. At most once a second unless `force`d.
    fn refresh_meeting(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(m) = &self.meeting else { return };
        if !force && (self.meeting_task.is_some() || self.meeting_checked.elapsed() < Duration::from_secs(1)) {
            return;
        }
        let dir = m.dir.clone();
        self.meeting_checked = std::time::Instant::now();
        self.meeting_task = Some(cx.spawn(async move |this, cx| {
            let d = dir.clone();
            let fresh = cx.background_spawn(async move { crate::meeting_view::MeetingDetail::load(d) }).await;
            let _ = this.update(cx, |this, cx| {
                this.meeting_task = None;
                this.meeting_checked = std::time::Instant::now();
                if let Some(fresh) = fresh.filter(|_| this.meeting.as_ref().is_some_and(|m| m.dir == dir)) {
                    this.meeting = Some(fresh);
                    cx.notify();
                }
            });
        }));
    }

    pub(crate) fn share_meeting(&mut self, dir: std::path::PathBuf, cx: &mut Context<Self>) {
        if self.sharing { return; }
        let cfg = self.controller.read(cx).config.clone();
        let destination = stenly_core::destinations::share_config(&cfg);
        self.sharing = true;
        let destination_id = destination.id.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let send_dir = dir.clone();
            let result = cx.background_spawn(async move {
                let dest = stenly_core::destinations::build(&cfg, &destination);
                stenly_core::destinations::send_session(dest.as_ref(), &send_dir, false)
            }).await;
            let _ = this.update(cx, |this, cx| {
                this.sharing = false;
                match result {
                    Ok((_, _)) => {
                        match stenly_core::share::link_for(&dir, Some(&destination_id)) {
                            Ok(Some(link)) => { cx.write_to_clipboard(ClipboardItem::new_string(link)); this.flash("Private link copied", cx); }
                            _ => this.flash("Private link created", cx),
                        }
                        this.refresh_meeting(true, cx);
                    }
                    Err(e) => {
                        tracing::warn!(error = %format!("{e:#}"), "share failed");
                        this.flash_error("Couldn't create link", cx);
                    }
                }
                cx.notify();
            });
        }).detach();
    }

    fn send_notes_to_attendees(&mut self, meeting: &crate::meeting_view::MeetingDetail, cx: &mut Context<Self>) {
        let emails: Vec<String> = meeting.meta.event.as_ref().map(|e| e.attendees.iter().filter(|a| e.calendar.as_deref().is_none_or(|me| !a.email.eq_ignore_ascii_case(me))).map(|a| a.email.clone()).collect()).unwrap_or_default();
        if emails.is_empty() { self.flash_error("No attendee email addresses", cx); return; }
        let link = match stenly_core::share::link_for(&meeting.dir, None) { Ok(Some(v)) => v, _ => { self.flash_error("Create a share link first", cx); return; } };
        let payload = match stenly_core::destinations::Payload::load(&meeting.dir) { Ok(v) => v, Err(_) => { self.flash_error("Couldn't read meeting", cx); return; } };
        let body = stenly_core::share::attendee_message(&payload, &link);
        let url = format!("mailto:{}?subject={}&body={}", emails.join(","), stenly_core::share::percent_encode(&format!("Notes from {}", meeting.meta.title)), stenly_core::share::percent_encode(&body));
        cx.open_url(&url);
    }

    fn copy_for_slack(&mut self, meeting: &crate::meeting_view::MeetingDetail, cx: &mut Context<Self>) {
        let link = match stenly_core::share::link_for(&meeting.dir, None) { Ok(Some(v)) => v, _ => { self.flash_error("Create a share link first", cx); return; } };
        let payload = match stenly_core::destinations::Payload::load(&meeting.dir) { Ok(v) => v, Err(_) => { self.flash_error("Couldn't read meeting", cx); return; } };
        cx.write_to_clipboard(ClipboardItem::new_string(stenly_core::share::attendee_message(&payload, &link)));
        self.flash("Notes copied for Slack", cx);
    }

    pub(crate) fn revoke_share(&mut self, dir: std::path::PathBuf, cx: &mut Context<Self>) {
        if self.sharing { return; }
        let cfg = self.controller.read(cx).config.clone();
        let destination = stenly_core::destinations::share_config(&cfg);
        self.sharing = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { stenly_core::share::revoke(&destination, &dir) }).await;
            let _ = this.update(cx, |this, cx| {
                this.sharing = false;
                match result {
                    Ok(()) => { this.flash("Share link revoked", cx); this.refresh_meeting(true, cx); }
                    Err(e) => { tracing::warn!(error = %format!("{e:#}"), "share revoke failed"); this.flash_error("Couldn't revoke link", cx); }
                }
                cx.notify();
            });
        }).detach();
    }

    /// A brief notice in the bottom-right corner: "…" reads as started,
    /// anything else as done.
    pub(crate) fn flash(&mut self, msg: impl Into<SharedString>, cx: &mut Context<Self>) {
        let msg = msg.into();
        let kind = if msg.ends_with('…') { crate::toast::ToastKind::Info } else { crate::toast::ToastKind::Done };
        self.notify_toast(msg, kind, cx);
    }

    /// A failure notice, shown a little longer with a warning glyph.
    pub(crate) fn flash_error(&mut self, msg: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.notify_toast(msg.into(), crate::toast::ToastKind::Error, cx);
    }

    pub(crate) fn notify_toast(&mut self, msg: SharedString, kind: crate::toast::ToastKind, cx: &mut Context<Self>) {
        self.toast_seq += 1;
        let id = self.toast_seq;
        self.toast = Some(crate::toast::Toast { id, kind, msg, leaving: false });
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(kind.duration()).await;
            let still = this.update(cx, |this, cx| match this.toast.as_mut() {
                Some(t) if t.id == id => {
                    t.leaving = true;
                    cx.notify();
                    true
                }
                _ => false,
            });
            if !matches!(still, Ok(true)) {
                return;
            }
            cx.background_executor().timer(crate::toast::FADE_OUT).await;
            let _ = this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|t| t.id == id) {
                    this.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Bottom-right stack: Undo for a trashed meeting, an in-flight send, the
    /// latest notice. Overlaid on the content so nothing in the toolbar moves.
    fn toast_stack(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sending = self.dest_ui.read(cx).sending_label(cx);
        if self.undo_trash.is_none() && sending.is_none() && self.toast.is_none() {
            return None;
        }
        Some(
            div()
                .absolute()
                .right(px(16.))
                .bottom(px(16.))
                .flex()
                .flex_col()
                .items_end()
                .gap(px(8.))
                .when(self.undo_trash.is_some(), |d| d.child(self.undo_trash_toast(cx)))
                .when_some(sending, |d, n| d.child(crate::toast::pending(format!("Sending to {n}…").into())))
                .when_some(self.toast.as_ref(), |d, t| d.child(crate::toast::render(t)))
                .into_any_element(),
        )
    }

    fn share_button(&self, meeting: &crate::meeting_view::MeetingDetail, cx: &mut Context<Self>) -> AnyElement {
        let dir = meeting.dir.clone();
        let active = !meeting.meta.shares.is_empty();
        let sharing = self.sharing;
        let weak = cx.entity().downgrade();
        let label = if sharing { "Sharing…" } else if active { "Shared" } else { "Share" };
        let icon = if active { icons::LINK } else { icons::CLOUD };
        let button = ui::button_lead_icon("share", label, if active { Btn::Outline } else { Btn::Primary }, icon)
            .when(!sharing, |b| b.on_click(move |_, _, cx| {
                if let Some(w) = weak.upgrade() { w.update(cx, |this, cx| this.share_meeting(dir.clone(), cx)); }
            }))
            .when(sharing, |b| b.opacity(0.55));
        if !active { return button.into_any_element(); }
        let email_meeting = meeting.clone();
        let slack_meeting = meeting.clone();
        let copy_dir = meeting.dir.clone();
        let revoke_dir = meeting.dir.clone();
        let weak = cx.entity().downgrade();
        div().flex().items_center().gap(px(5.)).child(button).child({
            let weak = cx.entity().downgrade();
            ui::icon_button("share-email", icons::PAPERPLANE, 28., 13., "Send notes to attendees").rounded_full().on_click(move |_, _, cx| {
                if let Some(w) = weak.upgrade() { w.update(cx, |this, cx| this.send_notes_to_attendees(&email_meeting, cx)); }
            })
        }).child({
            let weak = cx.entity().downgrade();
            ui::icon_button("share-slack", icons::COPY, 28., 13., "Copy notes for Slack").rounded_full().on_click(move |_, _, cx| {
                if let Some(w) = weak.upgrade() { w.update(cx, |this, cx| this.copy_for_slack(&slack_meeting, cx)); }
            })
        }).child(
            ui::icon_button("share-copy", icons::LINK, 28., 13., "Copy share link").rounded_full().on_click(move |_, _, cx| {
                match stenly_core::share::link_for(&copy_dir, None) {
                    Ok(Some(link)) => { cx.write_to_clipboard(ClipboardItem::new_string(link)); if let Some(w) = weak.upgrade() { w.update(cx, |this, cx| this.flash("Private link copied", cx)); } }
                    Ok(None) | Err(_) => if let Some(w) = weak.upgrade() { w.update(cx, |this, cx| this.flash_error("Share key unavailable", cx)); },
                }
            }),
        ).child({
            let weak = cx.entity().downgrade();
            ui::icon_button("share-revoke", icons::TRASH, 28., 13., "Revoke share link").rounded_full().on_click(move |_, _, cx| {
                if let Some(w) = weak.upgrade() { w.update(cx, |this, cx| this.revoke_share(revoke_dir.clone(), cx)); }
            })
        }).into_any_element()
    }

    /// "…" toolbar button that opens a native NSMenu of note actions.
    fn meeting_menu(
        &self,
        m: crate::meeting_view::MeetingDetail,
        dir: std::path::PathBuf,
        ctrl: Entity<Controller>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let notes_md = crate::meeting_view::notes_markdown(&m);
        let transcript = crate::meeting_view::transcript_text(&m);
        let obsidian = crate::meeting_view::obsidian_url(&m);
        let note_path = m.meta.note_path.clone();
        let reveal_target = note_path.clone().unwrap_or_else(|| dir.clone());

        #[derive(Clone, Copy)]
        enum A {
            CopyNotes,
            CopyTranscript,
            CopyPath,
            Obsidian,
            Reveal,
            Redo,
            Trash,
        }
        let mut items: Vec<(&'static str, Option<A>)> = vec![("Copy Notes", Some(A::CopyNotes))];
        if !transcript.is_empty() {
            items.push(("Copy Transcript", Some(A::CopyTranscript)));
        }
        if note_path.is_some() {
            items.push(("Copy Note Path", Some(A::CopyPath)));
        }
        items.push(("-", None));
        if obsidian.is_some() {
            items.push(("Open in Obsidian", Some(A::Obsidian)));
        }
        items.push(("Show in Finder", Some(A::Reveal)));
        items.push(("-", None));
        items.push(("Redo Notes", Some(A::Redo)));
        items.push(("-", None));
        items.push(("Move to Trash", Some(A::Trash)));

        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(glass_icon_button("more", icons::ELLIPSIS).role(gpui::Role::Button).aria_label("Meeting actions").on_click(cx.listener(move |_this, e: &gpui::ClickEvent, window, cx| {
                // NSMenu tracking runs a nested event loop that keeps draining
                // GPUI's main-thread queue; opening it inside this listener (while
                // the app is borrowed) made any queued task panic with "already
                // borrowed". Open it on the next turn with nothing borrowed.
                let pos = e.mouse_position().unwrap_or_else(|| window.mouse_position());
                // Not while it's still recording or being processed.
                let can_trash = ctrl.read(cx).can_trash(&dir);
                let items = items.clone();
                let (notes_md, transcript, note_path, obsidian, reveal_target, dir, ctrl) =
                    (notes_md.clone(), transcript.clone(), note_path.clone(), obsidian.clone(), reveal_target.clone(), dir.clone(), ctrl.clone());
                let view = ui::native_view(window);
                cx.spawn_in(window, async move |this, cx| {
                    let entries: Vec<ui::MenuEntry> = items
                        .iter()
                        .map(|(l, a)| match (l, a) {
                            (_, None) => ui::MenuEntry::separator(),
                            (l, Some(A::Trash)) => ui::MenuEntry { enabled: can_trash, ..ui::MenuEntry::item(*l) },
                            (l, _) => ui::MenuEntry::item(*l),
                        })
                        .collect();
                    let Some(i) = ui::rich_menu_in_view(view, &entries, pos) else { return };
                    let _ = this.update(cx, |this, cx| match items[i].1 {
                    Some(A::CopyNotes) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(notes_md.clone()));
                        this.flash("Notes copied", cx);
                    }
                    Some(A::CopyTranscript) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(transcript.clone()));
                        this.flash("Transcript copied", cx);
                    }
                    Some(A::CopyPath) => {
                        if let Some(p) = &note_path {
                            cx.write_to_clipboard(ClipboardItem::new_string(p.to_string_lossy().to_string()));
                            this.flash("Path copied", cx);
                        }
                    }
                    Some(A::Obsidian) => {
                        if let Some(u) = &obsidian {
                            cx.open_url(u);
                        }
                    }
                    Some(A::Reveal) => cx.reveal_path(&reveal_target),
                    Some(A::Redo) => {
                        let dir = dir.clone();
                        ctrl.update(cx, |c, cx| c.reprocess(dir, cx));
                        this.flash("Rewriting notes…", cx);
                    }
                    Some(A::Trash) => {
                        let title = this.meeting.as_ref().map(|m| m.meta.title.clone()).unwrap_or_default();
                        match ctrl.update(cx, |c, cx| c.trash_meeting(dir.clone(), cx)) {
                            Ok(trashed) => {
                                // The note is gone: back to the list, with Undo.
                                this.meeting = None;
                                this.offer_undo_trash(trashed, cx);
                            }
                            Err(e) => this.trash_error(&format!("Couldn't move “{title}” to the Trash"), e, cx),
                        }
                    }
                    None => {}
                    });
                })
                .detach();
            })))
            .into_any_element()
    }

    /// Fake recording for snapshots (STENLY_FAKE_RECORDING=1).
    fn effective_status(&self, cx: &App) -> Status {
        match std::env::var_os("STENLY_FAKE_RECORDING") {
            Some(_) => Status::Recording { title: "Weekly sync".into(), since: Local::now() - chrono::Duration::seconds(754) },
            None => self.controller.read(cx).status.clone(),
        }
    }

    /// The Record button. While recording it morphs (width + corners) into a
    /// capsule: pulsing dot, timer, you/them levels and a stop button, in the
    /// same spot so nothing else on the page moves.
    fn record_control(&self, cx: &mut Context<Self>) -> AnyElement {
        use gpui::{Animation, AnimationExt};
        const IDLE_W: f32 = 96.;
        let ctrl = self.controller.clone();
        match self.effective_status(cx) {
            Status::Idle if glass_enabled() => glass_spot(
                div()
                    .id("record-now")
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(6.))
                    .h(px(30.))
                    .px(px(14.))
                    .rounded_full()
                    .text_size(px(theme::BODY))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text())
                    .hover(|s| s.bg(theme::hover()))
                    .child(icon(icons::RECORD, 13., theme::red()))
                    .child("Record")
                    .on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.start_now(cx))),
                15.,
            )
            .into_any_element(),
            Status::Idle => ui::button_lead_icon("record-now", "Record", Btn::Primary, icons::RECORD)
                .w(px(IDLE_W))
                .on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.start_now(cx)))
                .into_any_element(),
            Status::Recording { title, since } => {
                let c = self.controller.read(cx);
                let secs = (Local::now() - since).num_seconds().max(0);
                let time = if secs >= 3600 { format!("{}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60) } else { format!("{}:{:02}", secs / 60, secs % 60) };
                // After 15 s, flag a channel that has delivered only digital silence.
                let dead: Vec<&'static str> = c
                    .levels
                    .as_ref()
                    .filter(|_| secs > 15)
                    .map(|l| {
                        [(stenly_core::platform::Channel::Mic, "your microphone"), (stenly_core::platform::Channel::System, "system audio")]
                            .into_iter()
                            .filter(|(ch, _)| !l.alive(*ch))
                            .map(|(_, n)| n)
                            .collect()
                    })
                    .unwrap_or_default();
                let warn: Option<String> = (!dead.is_empty()).then(|| {
                    if dead == ["system audio"] {
                        "No sound from other apps yet. That's normal until someone speaks; if a call is already going, check Privacy → System Audio.".into()
                    } else {
                        format!("No sound from {} yet. It may be muted or unavailable (laptop lid closed?).", dead.join(" or "))
                    }
                });
                let tip: SharedString = match &warn {
                    Some(w) => format!("Recording “{title}”. {w}").into(),
                    None => format!("Recording “{title}”").into(),
                };
                let dot = if warn.is_some() { theme::amber() } else { theme::red() };
                let meter = |v: f32| div().w(px(3.)).h(px(12.)).rounded_full().bg(theme::toggle_off()).flex().flex_col().justify_end().child(div().w_full().h(px(12. * v.clamp(0.08, 1.))).rounded_full().bg(theme::green()));
                let tnum = gpui::FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]));
                let contents = div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .child(div().size(px(8.)).rounded_full().bg(dot).with_animation(
                        "rec-pulse",
                        Animation::new(Duration::from_millis(1600)).repeat().with_easing(gpui::pulsating_between(0.45, 1.0)),
                        |d, t| d.opacity(t),
                    ))
                    .child(div().font_features(tnum).child(time))
                    .child(div().flex().items_end().gap(px(2.)).child(meter(self.meter[0])).child(meter(self.meter[1])))
                    // Fade the live readout in as the button opens up.
                    .with_animation("rec-contents", Animation::new(Duration::from_millis(260)).with_easing(gpui::ease_in_out), |d, t| d.opacity(t));
                let capsule = div()
                    .id("record-now")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(30.))
                    .min_w(px(IDLE_W))
                    .pl(px(12.))
                    .pr(px(3.))
                    .rounded_full()
                    .when(!glass_enabled(), |d| d.bg(theme::button()))
                    .text_size(px(theme::BODY))
                    .text_color(theme::text())
                    .flex_shrink_0()
                    .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
                    .child(contents)
                    .child(
                        div()
                            .id("rec-stop")
                            .size(px(24.))
                            .flex_shrink_0()
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(theme::hover())
                            .hover(|s| s.bg(theme::button_hover()))
                            .child(icon(icons::STOP, 9., theme::text()))
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                ctrl.update(cx, |c, cx| c.stop_recording(cx))
                            }),
                    );
                if glass_enabled() { glass_spot(capsule, 15.).into_any_element() } else { capsule.into_any_element() }
            }
        }
    }

    /// While recording: the last few lines transcribed so far, both sides,
    /// newest at the bottom (like a chat).
    fn live_transcript(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = self.controller.read(cx);
        let segs = if std::env::var_os("STENLY_FAKE_RECORDING").is_some() {
            use stenly_core::platform::Channel;
            use stenly_core::transcript::Segment;
            let sg = |ch, start: f64, t: &str| Segment::new(ch, start, start + 3., t);
            vec![
                sg(Channel::System, 700., "Okay, so for the garden project we're aiming for the second week of the month."),
                sg(Channel::Mic, 706., "That works. I'll have the planting list ready by then."),
                sg(Channel::System, 712., "Great. Can we check the sample budget spreadsheet before we order anything?"),
                sg(Channel::Mic, 719., "Yes, I'll go through it this week and send a summary."),
            ]
        } else {
            // try_lock: never wait on the transcriber from the UI thread
            match c.live_segments.as_ref()?.try_lock() {
                Ok(s) => {
                    let s = s.clone();
                    self.live_cache.replace(s.clone());
                    s
                }
                Err(_) => self.live_cache.borrow().clone(),
            }
        };
        let mut segs: Vec<_> = segs.into_iter().collect();
        segs.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
        let n = segs.len();
        let last: Vec<_> = segs.into_iter().skip(n.saturating_sub(4)).collect();
        let rows: Vec<AnyElement> = last
            .iter()
            .map(|sg| {
                let me = sg.channel == stenly_core::platform::Channel::Mic;
                div()
                    .flex()
                    .gap(px(10.))
                    .py(px(5.))
                    .child(div().w(px(40.)).flex_shrink_0().text_size(px(theme::SUBHEAD)).text_color(theme::faint()).child(if me { "You" } else { "Them" }))
                    .child(div().flex_1().min_w_0().text_size(px(theme::BODY)).line_height(px(18.)).text_color(theme::text()).child(sg.text.clone()))
                    .into_any_element()
            })
            .collect();
        Some(
            div()
                .rounded(px(12.))
                .bg(theme::card())
                .px(px(16.))
                .py(px(10.))
                .when(rows.is_empty(), |d| d.child(div().py(px(5.)).text_size(px(theme::BODY)).text_color(theme::muted()).child("Listening… words appear here a few seconds after each pause.")))
                .children(rows)
                .into_any_element(),
        )
    }

    /// Background work in the toolbar's trailing edge (writing up notes).
    fn toolbar_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = self.controller.read(cx);
        match (&c.status, c.jobs.first()) {
            (Status::Idle, Some(j)) => {
                let tip: SharedString = format!("Writing up “{}”", j.title).into();
                div()
                    .id("job-status")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(26.))
                    .text_size(px(theme::CALLOUT))
                    .text_color(theme::muted())
                    .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
                    .child(ui::spinner("job-spin", 11., theme::muted()))
                    .child(j.progress.clone())
                    .into_any_element()
            }
            _ => div().into_any_element(),
        }
    }

    /// Experimental: place real Liquid Glass behind the sidebar panel.
    fn sync_glass(&self, window: &Window) {
        #[cfg(target_os = "macos")]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let h = f32::from(window.viewport_size().height) as f64;
            let inset = SIDEBAR_INSET as f64;
            // Concentric with the window's real corner: R_inner = R_window − inset.
            let win_r = (|| {
                use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                let wh = HasWindowHandle::window_handle(window).ok()?;
                let RawWindowHandle::AppKit(a) = wh.as_raw() else { return None };
                stenly_platform::appkit_ui::ensure_unified_toolbar(a.ns_view.as_ptr());
                stenly_platform::appkit_ui::window_corner_radius(a.ns_view.as_ptr())
            })()
            .unwrap_or(WINDOW_RADIUS as f64);
            let rects_r = (win_r - inset).max(4.);
            let rects = vec![stenly_platform::appkit_ui::GlassRect { x: inset, y: inset, w: SIDEBAR_W as f64 - inset, h: h - inset * 2., radius: rects_r, clear: false }];
            if let Ok(wh) = HasWindowHandle::window_handle(window) {
                if let RawWindowHandle::AppKit(a) = wh.as_raw() {
                    stenly_platform::appkit_ui::sync_glass(a.ns_view.as_ptr(), &rects);
                }
            }

        }
    }

    /// The open section's name for the toolbar, and whether it's a sub-page
    /// (an editor) that shows a Back button.
    fn toolbar_title(&self, cx: &App) -> (SharedString, bool) {
        use stenly_core::library::Kind;
        let sub = match self.page {
            Page::Templates => self.library_editor_title(Kind::Template),
            Page::Recipes => self.library_editor_title(Kind::Recipe),
            Page::Destinations => self.dest_ui.read(cx).editor_title(),
            _ => None,
        };
        if let Some(t) = sub {
            return (t, true);
        }
        let t = match self.page {
            Page::Home => "Meetings",
            Page::Preferences => "General",
            Page::You => "You",
            Page::Calendar => "Calendar",
            Page::Transcription => "Transcription",
            Page::Dictation => "Dictation",
            Page::History => "Dictations",
            Page::Microphones => "Microphones",
            Page::Summaries => "Summaries",
            Page::Notes => "Notes & Storage",
            Page::Destinations => "Destinations",
            Page::Context => "Context",
            Page::Permissions => "Privacy",
            Page::Templates => "Templates",
            Page::Recipes => "Recipes",
            Page::Trash => "Trash",
        };
        (t.into(), false)
    }

    fn close_sub_page(&mut self, cx: &mut Context<Self>) {
        match self.page {
            Page::Destinations => self.dest_ui.update(cx, |d, cx| d.close_editor(cx)),
            _ => self.close_library_editor(cx),
        }
        cx.notify();
    }

    pub(crate) fn go(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        self.meeting = None;
        self.meeting_task = None;
        self.match_pending = false;
        if page == Page::Home {
            let cfg = self.controller.read(cx).config.clone();
            self.browser.update(cx, |b, cx| b.refresh(cfg, cx));
        }
        self.dest_ui.update(cx, |d, cx| d.close_editor(cx));
        if matches!(page, Page::Microphones) {
            self.refresh_mics(cx);
        }
        if matches!(page, Page::History) {
            self.history = stenly_core::dictation::history();
        }
        if matches!(page, Page::Trash) {
            self.refresh_trash(cx);
        }
        if matches!(page, Page::Context) {
            self.context_ui.update(cx, |u, cx| u.on_open(cx));
        }
        if matches!(page, Page::Permissions | Page::Transcription | Page::Summaries | Page::Dictation) {
            self.controller.update(cx, |c, cx| {
                c.refresh_permissions();
                c.refresh_ollama(cx);
            });
        }
        cx.notify();
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let page = self.page;
        let in_meeting = self.meeting.is_some();
        let item = |id, label, data, p: Page, cx: &mut Context<Self>| {
            nav_item(id, label, data, page == p && !(in_meeting && p != Page::Home)).on_click(cx.listener(move |this, _, _, cx| this.go(p, cx)))
        };
        // Group header: 11pt semibold tertiary label, aligned to the row text.
        let section = |t: &'static str| div().pl(px(8.)).pt(px(14.)).pb(px(4.)).text_size(px(theme::SUBHEAD)).font_weight(FontWeight::SEMIBOLD).text_color(theme::faint()).child(t);
        let glass = glass_enabled();
        div()
            .w(px(SIDEBAR_W))
            .h_full()
            .flex_shrink_0()
            // Real glass: transparent over an inset NSGlassEffectView panel
            // (SIDEBAR_INSET from the window edge, concentric corners).
            // Fallback: blurred window + tint, full height with a hairline.
            .when(!glass, |d| d.bg(theme::sidebar()).border_r_1().border_color(theme::divider()))
            .flex()
            .flex_col()
            .gap(px(2.))
            // Rows sit 10pt inside the panel (panel itself is 8pt from the edge).
            .px(px(if glass { SIDEBAR_INSET + SIDEBAR_PAD } else { SIDEBAR_PAD }))
            // Below the traffic lights (titlebar band is 52pt).
            .pt(px(60.))
            // Same gap to the panel's bottom edge as to its sides (concentric Trash row).
            .pb(px(if glass { SIDEBAR_INSET + SIDEBAR_PAD } else { SIDEBAR_PAD }))
            .child(item("nav-home", "Meetings", icons::WAVE, Page::Home, cx))
            .child(item("nav-history", "Dictations", icons::QUOTE, Page::History, cx))
            .child(section("Settings"))
            .child(item("nav-prefs", "General", icons::GEAR, Page::Preferences, cx))
            .child(item("nav-you", "You", icons::USERS, Page::You, cx))
            .child(item("nav-cal", "Calendar", icons::CALENDAR, Page::Calendar, cx))
            .child(item("nav-dictation", "Dictation", icons::KEYBOARD, Page::Dictation, cx))
            .child(item("nav-mics", "Microphones", icons::MIC, Page::Microphones, cx))
            .child(item("nav-asr", "Transcription", icons::WAVE, Page::Transcription, cx))
            .child(item("nav-llm", "Summaries", icons::SPARKLES, Page::Summaries, cx))
            .child(item("nav-templates", "Templates", icons::FILE_TEXT, Page::Templates, cx))
            .child(item("nav-recipes", "Recipes", icons::RECIPE, Page::Recipes, cx))
            .child(item("nav-notes", "Notes & Storage", icons::FOLDER, Page::Notes, cx))
            .child(item("nav-context", "Context", icons::USERS, Page::Context, cx))
            .child(item("nav-dest", "Destinations", icons::PAPERPLANE, Page::Destinations, cx))
            .child(item("nav-perms", "Privacy", icons::SHIELD, Page::Permissions, cx))
            // Trash sits at the foot of the sidebar, out of the way (Granola, Finder).
            .child(div().flex_1())
            .child(item("nav-trash", "Trash", icons::TRASH, Page::Trash, cx))
            .into_any_element()
    }

    pub(crate) fn controller(&self) -> &Entity<Controller> {
        &self.controller
    }

    // ------------------------------------------------------------- Home
    fn home(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = self.controller.read(cx);
        let now = Utc::now();
        let ctrl = self.controller.clone();

        // ---- coming up (Granola's layout): one block per day with the date
        // as a large numeral on the left, then that day's meetings — a bar in
        // the calendar's colour, title, time range. Record appears on hover.
        let mut upcoming: Vec<_> = c.events.iter().filter(|e| e.end > now).take(7).cloned().collect();
        if std::env::var_os("STENLY_DEMO_EVENTS").is_some() {
            upcoming = demo_events(now);
        }
        let recording = c.is_recording();
        let tnum2 = || gpui::FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]));
        let today_d = Local::now().date_naive();
        // group by local day
        let mut days: Vec<(chrono::NaiveDate, Vec<stenly_core::calendar::CalendarEvent>)> = vec![];
        for e in upcoming {
            let d = e.start.with_timezone(&Local).date_naive().max(today_d);
            match days.last_mut() {
                Some((day, v)) if *day == d => v.push(e),
                _ => days.push((d, vec![e])),
            }
        }
        let n_days = days.len();
        let mut up_rows: Vec<AnyElement> = vec![];
        let mut idx = 0usize;
        for (di, (day, evs)) in days.into_iter().enumerate() {
            let is_today = day == today_d;
            let date_col = div()
                .w(px(108.))
                .flex_shrink_0()
                .flex()
                .items_start()
                .gap(px(8.))
                .pt(px(6.))
                .child(div().text_size(px(30.)).line_height(px(32.)).font_weight(FontWeight::LIGHT).font_features(tnum2()).text_color(theme::text()).child(day.format("%-d").to_string()))
                .child(
                    div()
                        .pt(px(2.))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .text_size(px(theme::SUBHEAD))
                                .line_height(px(14.))
                                .text_color(theme::text())
                                .child(day.format("%B").to_string())
                                .when(is_today, |d| d.child(div().size(px(5.)).rounded_full().bg(theme::red()))),
                        )
                        .child(div().text_size(px(theme::SUBHEAD)).line_height(px(14.)).text_color(theme::muted()).child(day.format("%a").to_string())),
                );
            let mut items: Vec<AnyElement> = vec![];
            for e in evs {
                let i = idx;
                idx += 1;
                let live = e.is_live_at(now);
                let ctrl = ctrl.clone();
                let ev = e.clone();
                let local_s = e.start.with_timezone(&Local);
                let local_e = e.end.with_timezone(&Local);
                // "10:00 – 10:30 PM" (meridiem once, like Granola)
                let range = if local_s.format("%p").to_string() == local_e.format("%p").to_string() {
                    format!("{} – {} {}", local_s.format("%-I:%M"), local_e.format("%-I:%M"), local_e.format("%p"))
                } else {
                    format!("{} – {}", local_s.format("%-I:%M %p"), local_e.format("%-I:%M %p"))
                };
                let when = if live { format!("Now · until {}", local_e.format("%-I:%M %p")) } else { range };
                let bar = if live { theme::red() } else { e.color.as_deref().map(theme::hex).unwrap_or(theme::accent()) };
                items.push(
                    div()
                        .id(("ev", i))
                        .group("ev")
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .pl(px(8.))
                        .pr(px(6.))
                        .py(px(6.))
                        .rounded(px(8.))
                        .active(|s| s.bg(theme::selected()))
                        .on_click({
                            let ev = e.clone();
                            cx.listener(move |this, _, window, cx| this.show_event(ev.clone(), window, cx))
                        })
                        .child(div().w(px(3.)).h(px(34.)).flex_shrink_0().rounded_full().bg(bar))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(1.))
                                .child(div().text_size(px(theme::BODY)).font_weight(FontWeight::MEDIUM).truncate().child(e.title.clone()))
                                .child(div().text_size(px(theme::SUBHEAD)).font_features(tnum2()).text_color(if live { theme::red() } else { theme::muted() }).truncate().child(when)),
                        )
                        .when(!recording, |d| {
                            d.child(
                                div().when(!live, |d| d.invisible().group_hover("ev", |s| s.visible())).child(
                                    ui::button(("rec", i), "Record", if live { Btn::Primary } else { Btn::Ghost }).on_click(move |_, _, cx| {
                                        cx.stop_propagation();
                                        let ev = ev.clone();
                                        ctrl.update(cx, |c, cx| c.start_recording(Some(ev), cx))
                                    }),
                                ),
                            )
                        })
                        .into_any_element(),
                );
            }
            up_rows.push(
                div()
                    .relative()
                    .flex()
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(10.))
                    .child(date_col)
                    .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.)).children(items))
                    .when(di + 1 < n_days, |d| d.child(div().absolute().bottom_0().left(px(12.)).right(px(12.)).h(px(1.)).bg(theme::divider())))
                    .into_any_element(),
            );
        }

        let no_calendar = !c.uses_eventkit() && c.config.calendar.kody_url.is_empty() && c.config.calendar.ics_urls.is_empty();
        let (empty_title, empty_desc) = if no_calendar {
            ("No calendar connected", "Connect one and Stenly will offer to record a minute before each call.")
        } else if !c.calendar_errors.is_empty() && c.events.is_empty() {
            ("Can't reach your calendar", "Stenly will keep trying. You can still record any call.")
        } else if c.calendar_fetched.is_none() {
            ("Checking your calendar…", "")
        } else {
            ("No meetings in the next week", "")
        };
        // Empty: one quiet line inside the box, no icon; an action only when
        // there's something to do.
        let coming_up = div()
            .rounded(px(12.))
            .bg(theme::card())
            .p(px(4.))
            .when(up_rows.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .px(px(12.))
                        .min_h(px(52.))
                        .py(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(div().text_size(px(theme::BODY)).text_color(theme::muted()).child(empty_title))
                                .when(!empty_desc.is_empty(), |d| d.child(div().text_size(px(theme::SUBHEAD)).text_color(theme::faint()).child(empty_desc))),
                        )
                        .when(no_calendar, |d| d.child(ui::button("go-cal", "Connect…", Btn::Ghost).on_click(cx.listener(|this, _, _, cx| this.go(Page::Calendar, cx))))),
                )
            })
            .children(up_rows);

        let section = |t: &'static str| div().pl(px(16.)).pt(px(20.)).pb(px(7.)).text_size(px(theme::BODY)).font_weight(FontWeight::SEMIBOLD).child(t);
        div()
            .flex()
            .flex_col()
            .when_some(self.live_transcript(cx), |d, t| d.child(section("Live Transcript")).child(t))
            .child(section("Coming Up"))
            .child(coming_up)
            .child(section("All meetings"))
            .child(self.browser.clone())
            .into_any_element()
    }

    // ------------------------------------------------------------- Preferences
    fn preferences(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = self.controller.read(cx);
        let cfg = c.config.clone();
        let ctrl = self.controller.clone();
        let tog = |id: &'static str, key: &'static str, on: bool| {
            let ctrl = ctrl.clone();
            ui::toggle(id, on, move |_, _, cx| ctrl.update(cx, |c, cx| c.set(key, !on, cx))).into_any_element()
        };
        let login = {
            let ctrl = ctrl.clone();
            let on = cfg.app.launch_at_login;
            ui::toggle("login", on, move |_, _, cx| ctrl.update(cx, |c, cx| c.set_launch_at_login(!on, cx))).into_any_element()
        };
        let silence = cfg.detect.auto_stop_silence_secs;
        let silence_btn = {
            let ctrl = ctrl.clone();
            ui::popup("silence", &[(0u64, "Never"), (120, "After 2 minutes"), (180, "After 3 minutes"), (300, "After 5 minutes")], silence, move |v, cx| {
                ctrl.update(cx, |c, cx| c.set("detect.auto_stop_silence_secs", v as i64, cx))
            })
            .into_any_element()
        };
        let remind = cfg.calendar.remind_before_secs;
        let remind_btn = {
            let ctrl = ctrl.clone();
            ui::popup("remind", &[(0i64, "When it starts"), (60, "1 minute before"), (120, "2 minutes before"), (300, "5 minutes before")], remind, move |v, cx| {
                ctrl.update(cx, |c, cx| c.set("calendar.remind_before_secs", v, cx))
            })
            .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .child(ui::caption("Startup"))
            .child(ui::card(vec![
                ui::row(Some(icons::MENUBAR), "Show next meeting in menu bar", Some("Your next meeting and time until it starts, next to the Stenly icon".into()), tog("menubar", "app.show_next_in_menubar", cfg.app.show_next_in_menubar)).into_any_element(),
                ui::row(Some(icons::POWER), "Open at login", Some("Start quietly in the menu bar".into()), login).into_any_element(),
            ]))
            .child(ui::caption("Meetings"))
            .child(ui::card(vec![
                ui::row(Some(icons::CALENDAR), "Remind me", Some("When to ask “Record this meeting?”".into()), remind_btn).into_any_element(),
                ui::row(Some(icons::VIDEO), "Detect unscheduled calls", Some("Ask to record when Zoom, Meet or Teams starts using the mic".into()), tog("adhoc", "detect.ad_hoc_calls", cfg.detect.ad_hoc_calls)).into_any_element(),
                ui::row(Some(icons::WAVE), "Stop recording", Some("When nobody has spoken for a while".into()), silence_btn).into_any_element(),
            ]))
            .child(ui::caption("Notes"))
            .child(ui::card(vec![
                ui::row(Some(icons::SPARKLES), "AI summaries", Some("Summary, decisions and action items at the top of every note".into()), tog("llm", "llm.enabled", cfg.llm.enabled)).into_any_element(),
                ui::row(Some(icons::FILE_TEXT), "Include transcript", Some("A collapsed transcript at the end of each note".into()), tog("transcript", "output.include_transcript", cfg.output.include_transcript)).into_any_element(),
            ]))
            .child(ui::caption("Updates"))
            .child(ui::card(vec![
                ui::row(None, "Release notifications", Some("Notification-only; no in-app installation. No meeting content or system profile is sent. Turning off stops new checks; requests already started may finish.".into()), {
                    let ctrl = ctrl.clone();
                    let on = cfg.app.check_updates;
                    ui::toggle("updates", on, move |_, _, cx| ctrl.update(cx, |c, cx| c.set("app.check_updates", !on, cx)))
                        .role(gpui::Role::Switch).aria_label("Release notifications").aria_toggled(if on { gpui::Toggled::True } else { gpui::Toggled::False })
                }).into_any_element(),
                ui::row(None, "Installed version", Some(format!("{} · {}", env!("CARGO_PKG_VERSION"), c.update_status).into()), {
                    let ctrl = ctrl.clone();
                    let download = c.update_download.clone();
                    if let Some(url) = download {
                        ui::button("update-download", "View release", Btn::Ghost).on_click(move |_, _, cx| cx.open_url(&url)).into_any_element()
                    } else {
                        ui::button("check-updates", "Check now", Btn::Ghost).role(gpui::Role::Button).aria_label("Check now").on_click(move |_, _, cx| crate::updates::check(ctrl.clone(), cx)).into_any_element()
                    }
                }).into_any_element(),
            ]))
            .into_any_element()
    }

    // ------------------------------------------------------------- You
    fn you_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = self.controller.read(cx);
        let me = c.config.me.clone();
        let suggested = c.suggested_me_emails();
        let ctrl = self.controller.clone();
        let chips = |key: &'static str, items: &[String], ctrl: &Entity<Controller>| -> Vec<AnyElement> {
            items
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let ctrl = ctrl.clone();
                    let all = items.to_vec();
                    ui::removable_chip((key, i), t.clone(), move |cx| {
                        let mut v = all.clone();
                        v.remove(i);
                        ctrl.update(cx, |c, cx| c.set_list(key, &v, cx))
                    })
                })
                .collect()
        };
        let list_row = |title: &'static str, desc: &'static str, chips: Vec<AnyElement>, input: Entity<TextInput>| {
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .py(px(12.))
                .child(div().flex().flex_col().gap(px(2.)).child(div().text_size(px(theme::BODY)).child(title)).child(div().text_size(px(theme::SUBHEAD)).line_height(px(15.)).text_color(theme::muted()).child(desc)))
                .when(!chips.is_empty(), |d| d.child(div().flex().flex_wrap().gap(px(6.)).children(chips)))
                .child(div().flex().child(ui::text_field(input)))
                .into_any_element()
        };
        let name_row = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .min_h(px(48.))
            .child(div().w(px(84.)).flex_shrink_0().text_size(px(theme::BODY)).child("Name"))
            .child(ui::text_field(self.me_name.clone()))
            .into_any_element();
        let label_row = ui::row(
            None,
            "Shown as",
            Some("Your lines and action items in transcripts and notes. Empty = your first name; \"Me\" keeps the old label.".into()),
            div().w(px(180.)).child(ui::text_field(self.me_label.clone())),
        )
        .into_any_element();
        let suggestions: Option<AnyElement> = (!suggested.is_empty()).then(|| {
            let rows: Vec<AnyElement> = suggested
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let ctrl = ctrl.clone();
                    let e2 = e.clone();
                    ui::row(
                        None,
                        e.clone(),
                        Some("A calendar Stenly reads is signed in with this address".into()),
                        ui::button(("me-add", i), "Add", Btn::Outline).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.add_to_list("me.emails", std::slice::from_ref(&e2), cx))),
                    )
                    .into_any_element()
                })
                .collect();
            div().child(ui::caption("Found on this Mac")).child(ui::card(rows)).into_any_element()
        });
        let auto = {
            let ctrl = ctrl.clone();
            let on = me.auto;
            ui::row(
                None,
                "Fill in the rest automatically",
                Some("For each meeting, also use the calendar account the invite came to and the name your meeting app signs you in as. Nothing is saved here.".into()),
                ui::toggle("me-auto", on, move |_, _, cx| ctrl.update(cx, |c, cx| c.set("me.auto", !on, cx))),
            )
            .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .child(ui::caption("Who you are"))
            .child(ui::card(vec![
                name_row,
                list_row("Email addresses", "Every address you get invites at, work and personal.", chips("me.emails", &me.emails, &ctrl), self.me_email.clone()),
                list_row("Other names", "Names you show up as or get called, e.g. a nickname or a shortened surname.", chips("me.aliases", &me.aliases, &ctrl), self.me_alias.clone()),
                label_row,
            ]))
            .children(suggestions)
            .child(ui::caption("Automatic"))
            .child(ui::card(vec![auto]))
            .into_any_element()
    }

    // ------------------------------------------------------------- Calendar
    fn calendar(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = self.controller.read(cx);
        let cfg = c.config.clone();
        let ctrl = self.controller.clone();
        let kody_state = c.kody.clone();
        let eventkit = c.uses_eventkit();
        let supports_eventkit = c.supports_eventkit();
        let calendar_permission = c.permission(stenly_core::platform::Permission::Calendars);
        let calendars = c.calendars.clone();
        let cal_status = c.calendars_status.clone();
        let fetched = c.calendar_fetched;
        let errors = c.calendar_errors.clone();
        // One plain-language problem statement instead of raw error chains.
        let kody_err = cal_status.clone().and_then(|s| s.err()).filter(|_| calendars.is_empty());
        let failing = kody_err.is_some() || errors.iter().any(|e| e.starts_with("Google Calendar (via Kody)"));

        let connect_row: AnyElement = if eventkit {
            match calendar_permission {
                stenly_core::platform::PermissionState::Granted => {
                    let ctrl2 = ctrl.clone();
                    ui::row(Some(icons::CALENDAR), "Calendars on this Mac", Some(match fetched {
                        Some(t) => format!("Synced at {}", t.with_timezone(&Local).format("%-I:%M %p")),
                        None => "Syncing…".into(),
                    }.into()), ui::button("eventkit-refresh", "Refresh", Btn::Ghost).on_click(move |_, _, cx| ctrl2.update(cx, |c, cx| { c.load_calendars(cx); c.refresh_calendar(cx) }))).into_any_element()
                }
                stenly_core::platform::PermissionState::Denied => ui::row(Some(icons::CALENDAR), "Calendar access is off", Some("Allow Stenly in System Settings to read the accounts already in Calendar.app.".into()), ui::button("eventkit-settings", "Open Settings", Btn::Accent).on_click(move |_, _, cx| ctrl.update(cx, |c, _| c.shell.open_permission_settings(stenly_core::platform::Permission::Calendars)))).into_any_element(),
                _ => ui::row(Some(icons::CALENDAR), "Use the calendars on this Mac", Some("Google, iCloud and Exchange accounts already in Calendar.app. Nothing leaves your Mac.".into()), ui::button("eventkit-allow", "Allow Calendar Access", Btn::Accent).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.request_calendar_access(cx)))).into_any_element(),
            }
        } else { match &kody_state {
            KodyState::Connected => {
                let ctrl2 = ctrl.clone();
                ui::row(
                    Some(icons::LINK),
                    "Google Calendar via Kody",
                    Some(
                        match (fetched, failing) {
                            (_, true) => "Can't reach Kody right now".to_string(),
                            (Some(t), false) => format!("Synced at {}", t.with_timezone(&Local).format("%-I:%M %p")),
                            (None, false) => "Syncing…".to_string(),
                        }
                        .into(),
                    ),
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(ui::button("cal-refresh", if failing { "Try Again" } else { "Refresh" }, Btn::Ghost).on_click({
                            let ctrl = ctrl.clone();
                            move |_, _, cx| ctrl.update(cx, |c, cx| {
                                c.load_calendars(cx);
                                c.refresh_calendar(cx)
                            })
                        }))
                        .child(ui::more_button("kody-more", vec![ui::MenuEntry::item("Disconnect")], move |_, _, cx| ctrl2.update(cx, |c, cx| c.disconnect_kody(cx)))),
                )
                .into_any_element()
            }
            state => {
                let busy = matches!(state, KodyState::Connecting);
                let input = self.kody_url.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .py(px(14.))
                    .child(ui::row(
                        Some(icons::LINK),
                        "Google Calendar via Kody",
                        Some("Kody holds your Google sign-in; Stenly only asks it for upcoming events.".into()),
                        ui::button("kody-connect", if busy { "Waiting for browser…" } else { "Connect" }, Btn::Accent).on_click(move |_, _, cx| {
                            if busy {
                                return;
                            }
                            let url = input.read(cx).text().to_string();
                            ctrl.update(cx, |c, cx| c.connect_kody(url, cx))
                        }),
                    ).py(px(0.)))
                    .child(
                        div()
                            .ml(px(46.))
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(div().text_size(px(theme::BODY)).text_color(theme::muted()).child("Kody URL:"))
                            .child(ui::text_field(self.kody_url.clone())),
                    )
                    .when_some(if let KodyState::Failed(e) = state { Some(e.clone()) } else { None }, |d, e| d.child(div().ml(px(32.)).text_size(px(theme::SUBHEAD)).text_color(theme::red()).child(e)))
                    .into_any_element()
            }
        }};

        let cal_rows: Vec<AnyElement> = calendars
            .iter()
            .enumerate()
            .map(|(i, cal)| {
                let on = cfg.calendar.calendar_ids.contains(&cal.id) || (cfg.calendar.calendar_ids.is_empty() && (cal.primary || eventkit));
                let id = cal.id.clone();
                let ctrl = self.controller.clone();
                // A per-calendar sync problem shows on that calendar's own row.
                let problem = on.then(|| errors.iter().find(|e| e.contains(&format!("“{}”", cal.name))).cloned()).flatten();
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .min_h(px(40.))
                    .py(px(8.))
                    .child(div().size(px(10.)).flex_shrink_0().rounded_full().bg(cal.color.as_deref().map(theme::hex).unwrap_or(theme::muted())))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap(px(6.))
                                    .child(div().text_size(px(theme::BODY)).truncate().child(cal.name.clone()))
                                    .when(cal.primary, |d| d.child(div().text_size(px(theme::SUBHEAD)).text_color(theme::faint()).child("Primary"))),
                            )
                            .when_some(problem.clone(), |d, p| {
                                d.child(div().flex().items_center().gap(px(4.)).text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(icon(icons::ALERT, 10., theme::amber())).child(if p.contains("won't share event details") { "Events aren't shared with this account" } else { "Couldn't load events" }))
                            }),
                    )
                    .child(ui::toggle(("cal", i), on, move |_, _, cx| {
                        let id = id.clone();
                        ctrl.update(cx, |c, cx| c.toggle_calendar(&id, cx))
                    }))
                    .into_any_element()
            })
            .collect();

        let ics_rows: Vec<AnyElement> = cfg
            .calendar
            .ics_urls
            .iter()
            .enumerate()
            .map(|(i, u)| {
                let ctrl = self.controller.clone();
                let all = cfg.calendar.ics_urls.clone();
                ui::row(
                    Some(icons::GLOBE),
                    stenly_core::calendar::redact(u),
                    None,
                    ui::button(("ics-rm", i), "Remove", Btn::Ghost).on_click(move |_, _, cx| {
                        let mut v = all.clone();
                        v.remove(i);
                        ctrl.update(cx, |c, cx| c.set_list("calendar.ics_urls", &v, cx))
                    }),
                )
                .into_any_element()
            })
            .chain(std::iter::once(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_h(px(48.))
                    .py(px(10.))
                    .child(ui::text_field(self.ics_url.clone()))
                    .child({
                        let input = self.ics_url.clone();
                        let ctrl = self.controller.clone();
                        ui::button("ics-add", "Add", Btn::Ghost).on_click(move |_, _, cx| {
                            let url = input.read(cx).text().trim().to_string();
                            if url.is_empty() {
                                return;
                            }
                            let mut urls = ctrl.read(cx).config.calendar.ics_urls.clone();
                            if !urls.contains(&url) {
                                urls.push(url);
                            }
                            ctrl.update(cx, |c, cx| c.set_list("calendar.ics_urls", &urls, cx));
                            input.update(cx, |t, cx| t.set_text("", cx));
                        })
                    })
                    .into_any_element(),
            ))
            .collect();

        let ctrl = self.controller.clone();
        div()
            .flex()
            .flex_col()
            .child(ui::caption(if eventkit { "Calendars on this Mac" } else { "Connection" }))
            .child(ui::card(vec![connect_row]))
            .when(!eventkit && supports_eventkit, |d| d.child(ui::caption("On this Mac")).child(ui::card(vec![{
                let ctrl = self.controller.clone();
                ui::row(Some(icons::CALENDAR), "Use the calendars on this Mac", Some("Switch back to the accounts already in Calendar.app.".into()), ui::button("eventkit-use", "Use This Mac", Btn::Ghost).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.set_calendar_source("eventkit", cx)))).into_any_element()
            }])))
            .when(!calendars.is_empty(), |d| d.child(ui::caption("Calendars")).child(ui::card(cal_rows)))
            // Non-Kody problems (an ICS feed) still surface, one line each.
            .children(errors.into_iter()
                // Fresh EventKit installs have not asked yet; the access button is
                // the actionable state, not an alarming failed-sync message.
                .filter(|e| !(eventkit && calendar_permission == stenly_core::platform::PermissionState::NotDetermined && e.starts_with("Calendars on this Mac:")))
                .filter(|e| !e.starts_with("Google Calendar (via Kody)"))
                .filter(|e| !calendars.iter().any(|c| e.contains(&format!("“{}”", c.name))))
                .take(3).map(|e| {
                    div().flex().items_start().gap(px(6.)).px(px(2.)).pt(px(8.)).text_size(px(theme::SUBHEAD)).line_height(px(15.)).text_color(theme::muted()).child(icon(icons::ALERT, 11., theme::amber())).child(div().flex_1().min_w_0().child(e))
                }))
            .child(ui::caption("Display"))
            .child(ui::card(vec![
                ui::row(
                    Some(icons::USER_X),
                    "Show all events",
                    Some("Include events with no call link and no other guests, like focus blocks".into()),
                    {
                        let on = !cfg.calendar.require_meeting_link;
                        ui::toggle("nolink", on, move |_, _, cx| ctrl.update(cx, |c, cx| c.set("calendar.require_meeting_link", on, cx)))
                    },
                )
                .into_any_element(),
            ]))
            .child(ui::caption("ICS calendar link"))
            .child(div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).pb(px(8.)).child("Paste a private or shared ICS URL when Calendar.app isn't an option."))
            .child(ui::card(ics_rows))
            .into_any_element()
    }

    // ------------------------------------------------------------- AI models
    /// Transcription page (`asr = true`) or Summaries page.
    fn models(&self, asr: bool, cx: &mut Context<Self>) -> AnyElement {
        let c = self.controller.read(cx);
        let cfg = c.config.clone();
        let ollama = c.ollama.clone();

        let asr_cards: Vec<AnyElement> = stenly_asr::models::MODELS
            .iter()
            .map(|m| {
                let installed = c.asr_installed(m.id);
                let active = cfg.asr.model == m.id;
                let dl = c.download(m.id).cloned();
                let ctrl = self.controller.clone();
                let id = m.id.to_string();
                let trailing: AnyElement = if let Some(d) = dl {
                    div().flex().flex_col().items_end().gap(px(6.)).child(div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(format!("{} {:.0}%", d.status, d.fraction * 100.0))).child(ui::progress_bar(d.fraction, 140.)).into_any_element()
                } else if active && installed {
                    ui::chip("In Use", Some(icons::CHECK), theme::green()).into_any_element()
                } else if installed {
                    let ctrl2 = ctrl.clone();
                    let id2 = id.clone();
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(ui::button(SharedString::from(format!("use-{id}")), "Use", Btn::Primary).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.set("asr.model", id.as_str(), cx))))
                        .child(ui::more_button(SharedString::from(format!("rm-{id2}")), vec![ui::MenuEntry { symbol: Some("trash"), ..ui::MenuEntry::item("Remove Download") }], move |_, _, cx| ctrl2.update(cx, |c, cx| c.delete_asr(&id2, cx))))
                        .into_any_element()
                } else {
                    ui::button(SharedString::from(format!("dl-{id}")), "Download", Btn::Ghost)
                        .on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.download_asr(id.clone(), cx)))
                        .into_any_element()
                };
                model_card(
                    m.name,
                    if m.id.contains("v2") { Some("Recommended") } else { None },
                    vec![],
                    &format!("{} · {} MB", m.language, m.approx_mb),
                    trailing,
                    active && installed,
                )
            })
            .collect();

        let llm_cards: Vec<AnyElement> = stenly_core::ollama::CATALOG
            .iter()
            .map(|m| {
                let installed = c.llm_installed(m.id);
                let active = stenly_core::ollama::same_model(&cfg.llm.model, m.id);
                let dl = c.download(m.id).cloned();
                let ctrl = self.controller.clone();
                let id = m.id.to_string();
                let trailing: AnyElement = if let Some(d) = dl {
                    div().flex().flex_col().items_end().gap(px(6.)).child(div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(if d.fraction > 0.0 { format!("{} {:.0}%", d.status, d.fraction * 100.0) } else { d.status.clone() })).child(ui::progress_bar(d.fraction, 140.)).into_any_element()
                } else if !ollama.running {
                    div().into_any_element()
                } else if active && installed {
                    ui::chip("In Use", Some(icons::CHECK), theme::green()).into_any_element()
                } else if installed {
                    let ctrl2 = ctrl.clone();
                    let id2 = id.clone();
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(ui::button(SharedString::from(format!("use-{id}")), "Use", Btn::Primary).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.set("llm.model", id.as_str(), cx))))
                        .child(ui::more_button(SharedString::from(format!("rm-{id2}")), vec![ui::MenuEntry { symbol: Some("trash"), ..ui::MenuEntry::item("Remove Download") }], move |_, _, cx| ctrl2.update(cx, |c, cx| c.delete_llm(id2.clone(), cx))))
                        .into_any_element()
                } else {
                    ui::button(SharedString::from(format!("dl-{id}")), "Download", Btn::Ghost)
                        .on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.pull_llm(id.clone(), cx)))
                        .into_any_element()
                };
                let score = stenly_core::model_scores::score_for(m.id).map(|s| format!("Notes score {} · ", stenly_core::model_scores::label(s))).unwrap_or_default();
                model_card(
                    m.name,
                    if m.recommended { Some("Recommended") } else { None },
                    vec![],
                    &format!("{score}{} · {:.1} GB · {}", m.vendor, m.size_gb, m.blurb.trim_end_matches('.')),
                    trailing,
                    active && installed,
                )
            })
            .collect();

        // Custom / already-installed Ollama models that aren't in the catalog.
        let extra: Vec<String> = ollama.installed.iter().filter(|m| stenly_core::ollama::find(m).is_none()).cloned().collect();

        let ollama_banner: AnyElement = if !ollama.checked {
            div().into_any_element()
        } else if !ollama.running {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(14.))
                .py(px(12.))
                .rounded(px(10.))
                .bg(theme::card())
                .child(icon(icons::ALERT, 15., theme::amber()))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().text_size(px(theme::BODY)).font_weight(FontWeight::MEDIUM).child("Ollama isn't running"))
                        .child(div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child("Summaries run through Ollama. Install and open it, then check again.")),
                )
                .child(ui::button_icon("get-ollama", "Get Ollama", Btn::Primary, icons::EXTERNAL).on_click(|_, _, cx| cx.open_url("https://ollama.com/download")))
                .child({
                    let ctrl = self.controller.clone();
                    ui::button("recheck", "Check again", Btn::Ghost).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.refresh_ollama(cx)))
                })
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let uses_api = cfg.llm.uses_api();
        let source_row = {
            let ctrl = self.controller.clone();
            ui::row(
                None,
                "Write summaries with",
                Some(if uses_api { "A hosted model. Your transcript is sent to it; audio never leaves this Mac.".into() } else { "A model running in Ollama on this Mac. Nothing leaves it.".into() }),
                ui::popup("llm-source", &[(false, "Ollama on this Mac"), (true, "An API (OpenAI-compatible)")], uses_api, move |api, cx| {
                    ctrl.update(cx, |c, cx| c.set("llm.provider", if api { "api" } else { "ollama" }, cx))
                }),
            )
            .into_any_element()
        };

        let api_card = uses_api.then(|| self.api_card(&cfg, cx));
        let jargon_card = self.jargon_card(&cfg);

        if asr {
            let speakers_row = {
                let ctrl = self.controller.clone();
                let on = cfg.asr.separate_speakers;
                ui::row(
                    None,
                    "Tell speakers apart in the room",
                    Some("When everyone shares one mic (no call), label voices Speaker 1, 2, 3…".into()),
                    ui::toggle("sep-speakers", on, move |_, _, cx| ctrl.update(cx, |c, cx| c.set("asr.separate_speakers", !on, cx))),
                )
                .into_any_element()
            };
            return div()
                .flex()
                .flex_col()
                .child(ui::caption("Model"))
                .child(ui::card(asr_cards))
                .child(ui::caption("Personal Jargon"))
                .child(jargon_card)
                .child(ui::caption("Speakers"))
                .child(ui::card(vec![speakers_row, self.speaker_names_row(&cfg)]))
                .into_any_element();
        }
        div()
            .flex()
            .flex_col()
            .child(ui::caption("Model"))
            .child(ui::card(vec![source_row]))
            .when_some(api_card, |d, card| d.child(ui::caption("API")).child(card))
            .when(!uses_api, |d| d.child(ui::caption("Ollama")).child(ollama_banner).child(div().pt(px(if ollama.running { 0. } else { 8. })).child(ui::card(llm_cards))))
            .when(!uses_api && !extra.is_empty(), |d| {
                let rows = extra
                    .into_iter()
                    .enumerate()
                    .map(|(i, m)| {
                        let active = stenly_core::ollama::same_model(&cfg.llm.model, &m);
                        let ctrl = self.controller.clone();
                        let id = m.clone();
                        ui::row(
                            Some(icons::CODE),
                            m.clone(),
                            Some(match stenly_core::model_scores::score_for(&m) {
                                Some(sc) => format!("Installed in Ollama · Notes score {}", stenly_core::model_scores::label(sc)).into(),
                                None => "Installed in Ollama".into(),
                            }),
                            if active {
                                ui::chip("In Use", Some(icons::CHECK), theme::green()).into_any_element()
                            } else {
                                ui::button(("use-extra", i), "Use", Btn::Ghost).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.set("llm.model", id.as_str(), cx))).into_any_element()
                            },
                        )
                        .into_any_element()
                    })
                    .collect();
                d.child(ui::caption("Other Ollama Models")).child(ui::card(rows))
            })
            .into_any_element()
    }

    fn api_card(&self, cfg: &stenly_core::config::Config, cx: &mut Context<Self>) -> gpui::Div {
        let c = self.controller.read(cx);
        let test = c.api_test.clone();
        let key_saved = c.api_key_saved;
        let api = cfg.llm.api.clone();
        // Presets fill in the base URL and a sensible model; any OpenAI-compatible URL works.
        const PRESETS: &[(&str, &str, &str)] = &[
            ("OpenAI", "https://api.openai.com/v1", "gpt-5-mini"),
            ("Anthropic", "https://api.anthropic.com/v1", "claude-sonnet-4-6"),
            ("OpenRouter", "https://openrouter.ai/api/v1", "anthropic/claude-sonnet-4.6"),
            ("Groq", "https://api.groq.com/openai/v1", "openai/gpt-oss-120b"),
            ("Google Gemini", "https://generativelanguage.googleapis.com/v1beta/openai", "gemini-2.5-flash"),
            ("Custom", "", ""),
        ];
        let cur = PRESETS.iter().position(|(_, u, _)| !u.is_empty() && api.endpoint.trim_end_matches('/') == *u).unwrap_or(PRESETS.len() - 1);
        let preset_row = {
            let ctrl = self.controller.clone();
            let (ep, md) = (self.api_endpoint.clone(), self.api_model.clone());
            let opts: Vec<(usize, &'static str)> = PRESETS.iter().enumerate().map(|(i, p)| (i, p.0)).collect();
            ui::row(
                None,
                "Provider",
                None,
                ui::popup("api-preset", &opts, cur, move |i, cx| {
                    let (_, url, model) = PRESETS[i];
                    if url.is_empty() {
                        return;
                    }
                    ep.update(cx, |t, cx| t.set_text(url, cx));
                    md.update(cx, |t, cx| t.set_text(model, cx));
                    ctrl.update(cx, |c, cx| {
                        c.set("llm.api.endpoint", url, cx);
                        c.set("llm.api.model", model, cx);
                        c.api_test = crate::controller::ApiTest::Idle;
                    });
                }),
            )
            .into_any_element()
        };
        let field_row = |label: &'static str, input: Entity<TextInput>| {
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .min_h(px(44.))
                .child(div().w(px(84.)).flex_shrink_0().text_size(px(theme::BODY)).child(label))
                .child(ui::text_field(input))
                .into_any_element()
        };
        let key_row = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .min_h(px(44.))
            .child(div().w(px(84.)).flex_shrink_0().text_size(px(theme::BODY)).child("API key"))
            .child(ui::text_field(self.api_key.clone()))
            .when(key_saved, |d| {
                let ctrl = self.controller.clone();
                d.child(ui::button("api-key-rm", "Remove", Btn::Ghost).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.set_api_key("", cx))))
            })
            .into_any_element();
        let status: AnyElement = match &test {
            crate::controller::ApiTest::Idle => div().text_color(theme::muted()).child(if key_saved || !api.api_key.is_empty() || !api.api_key_env.is_empty() { "Key saved in your Keychain." } else { "Keys are stored in your macOS Keychain, not the config file." }).into_any_element(),
            crate::controller::ApiTest::Running => ui::pending("api-testing", "Testing…").into_any_element(),
            crate::controller::ApiTest::Ok(m) => div().flex().items_center().gap(px(5.)).text_color(theme::muted()).child(icon(icons::CHECK, 11., theme::green())).child(m.clone()).into_any_element(),
            crate::controller::ApiTest::Failed(e) => div().flex().items_center().gap(px(5.)).text_color(theme::muted()).child(icon(icons::ALERT, 11., theme::amber())).child(div().flex_1().min_w_0().child(e.clone())).into_any_element(),
        };
        let test_row = {
            let ctrl = self.controller.clone();
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .min_h(px(44.))
                .child(div().flex_1().min_w_0().text_size(px(theme::SUBHEAD)).child(status))
                .child(ui::button("api-test", "Test", Btn::Outline).on_click(move |_, window, cx| {
                    // commit whatever is typed before testing
                    window.blur(cx);
                    let ctrl = ctrl.clone();
                    cx.defer(move |cx| ctrl.update(cx, |c, cx| c.test_api(cx)));
                }))
                .into_any_element()
        };
        // How the chosen model did on Stenly's meeting-notes eval, plus the best scored hosted models.
        let score_row = {
            let mine = stenly_core::model_scores::score_for(&api.model);
            let best: Vec<String> = stenly_core::model_scores::SCORES.iter().filter(|s| !s.local).take(4).map(|s| format!("{} {:.1}", s.id, s.score)).collect();
            let text = match mine {
                Some(sc) => format!("Notes score {} on Stenly's meeting eval ({}). Top scored: {}.", stenly_core::model_scores::label(sc), stenly_core::model_scores::MEASURED, best.join(", ")),
                None => format!("No notes score for this model yet. Top scored on Stenly's meeting eval ({}): {}.", stenly_core::model_scores::MEASURED, best.join(", ")),
            };
            div().min_h(px(36.)).py(px(8.)).text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(text).into_any_element()
        };
        ui::card(vec![preset_row, field_row("Base URL", self.api_endpoint.clone()), field_row("Model", self.api_model.clone()), key_row, test_row, score_row])
    }

    fn jargon_card(&self, cfg: &stenly_core::config::Config) -> gpui::Div {
        let terms = cfg.asr.vocabulary.clone();
        let chips: Vec<AnyElement> = terms
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let ctrl = self.controller.clone();
                let all = terms.clone();
                ui::removable_chip(("jargon", i), t.clone(), move |cx| {
                    let mut v = all.clone();
                    v.remove(i);
                    ctrl.update(cx, |c, cx| c.set_list("asr.vocabulary", &v, cx))
                })
            })
            .collect();
        ui::card(vec![
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .py(px(12.))
                .child(div().text_size(px(theme::SUBHEAD)).line_height(px(15.)).text_color(theme::muted()).child("Names, products and terms you say in meetings. Stenly spells them this way in transcripts and summaries."))
                .when(!chips.is_empty(), |d| d.child(div().flex().flex_wrap().gap(px(6.)).children(chips)))
                .child(div().flex().child(ui::text_field(self.jargon.clone())))
                .into_any_element(),
        ])
    }

    // ------------------------------------------------------------- Notes & storage
    fn notes(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = self.controller.read(cx);
        let cfg = c.config.clone();
        let paths = cfg.paths();
        let pick = |id: &'static str, key: &'static str, cx: &mut Context<Self>| {
            let ctrl = self.controller.clone();
            ui::button(id, "Change…", Btn::Outline)
                .on_click(cx.listener(move |_, _, _, cx| {
                    let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Choose".into()) });
                    let ctrl = ctrl.clone();
                    cx.spawn(async move |_, cx| {
                        if let Ok(Ok(Some(paths))) = rx.await {
                            if let Some(p) = paths.into_iter().next() {
                                let _ = ctrl.update(cx, |c, cx| c.set_path(key, &p, cx));
                            }
                        }
                    })
                    .detach();
                }))
                .into_any_element()
        };
        let vaults = stenly_core::obsidian::vaults();
        let vault_rows: Vec<AnyElement> = vaults
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let target = v.path.join("Meetings");
                let active = paths.notes_dir.starts_with(&v.path);
                let ctrl = self.controller.clone();
                ui::path_row(
                    Some(icons::ARCHIVE),
                    if v.name.eq_ignore_ascii_case("obsidian") { "Obsidian vault".to_string() } else { v.name.clone() },
                    collapse_tilde(&v.path),
                    if active {
                        ui::chip("Saving Here", Some(icons::CHECK), theme::green()).into_any_element()
                    } else {
                        ui::button(("vault", i), "Use this vault", Btn::Accent)
                            .on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.set_path("output.notes_dir", &target, cx)))
                            .into_any_element()
                    },
                )
                .into_any_element()
            })
            .collect();
        let keep = cfg.output.keep_audio_days;
        let keep_btn = {
            let ctrl = self.controller.clone();
            // Normalize odd hand-edited values onto the nearest choice for display.
            let cur = match keep {
                k if k < 0 => -1,
                0 => 0,
                1..=7 => 7,
                _ => 30,
            };
            ui::popup("keep", &[(0i64, "Delete after notes"), (7, "For 7 days"), (30, "For 30 days"), (-1, "Forever")], cur, move |v, cx| {
                ctrl.update(cx, |c, cx| c.set("output.keep_audio_days", v, cx))
            })
            .into_any_element()
        };
        let cfg_path = paths.config_file.clone();
        let notes_dir = paths.notes_dir.clone();
        div()
            .flex()
            .flex_col()
            .when(!vault_rows.is_empty(), |d| d.child(ui::caption("Obsidian")).child(ui::card(vault_rows)))
            .child(ui::caption("Locations"))
            .child(ui::card(vec![
                ui::path_row(
                    Some(icons::FILE_TEXT),
                    "Notes folder",
                    collapse_tilde(&paths.notes_dir),
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(ui::button("open-notes", "Open", Btn::Ghost).on_click(move |_, _, cx| {
                            let _ = std::fs::create_dir_all(&notes_dir);
                            cx.open_with_system(&notes_dir)
                        }))
                        .child(pick("pick-notes", "output.notes_dir", cx)),
                )
                .into_any_element(),
                ui::path_row(Some(icons::WAVE), "Recordings folder", collapse_tilde(&paths.audio_dir), pick("pick-audio", "output.audio_dir", cx)).into_any_element(),
                ui::row(Some(icons::TRASH), "Keep recordings", Some("Audio is only needed to redo notes".into()), keep_btn).into_any_element(),
            ]))
            .child(ui::caption("Advanced"))
            .child(ui::card(vec![
                ui::path_row(
                    Some(icons::CODE),
                    "Config file",
                    collapse_tilde(&cfg_path),
                    ui::button("edit-config", "Open", Btn::Ghost).on_click(move |_, _, cx| cx.open_with_system(&cfg_path)),
                )
                .into_any_element(),
            ]))
            .into_any_element()
    }

    // ------------------------------------------------------------- Permissions
    fn permissions(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = permission_rows(&self.controller, cx);
        div()
            .flex()
            .flex_col()
            .child(ui::caption("Required"))
            .child(ui::card(rows.0))
            .when(!rows.1.is_empty(), |d| d.child(ui::caption("Optional")).child(ui::card(rows.1)))
            .into_any_element()
    }
}

/// (required rows, optional rows) — shared with onboarding.
pub fn permission_rows(controller: &Entity<Controller>, cx: &App) -> (Vec<AnyElement>, Vec<AnyElement>) {
    let c = controller.read(cx);
    let make = |p: Permission, data: crate::icons::Sym, title: &'static str, desc: &'static str| {
        let state = c.permission(p);
        let ctrl = controller.clone();
        let waiting = c.pending_permissions.contains(&p) && state != PermissionState::Granted;
        let trailing: AnyElement = match state {
            _ if waiting => ui::pending(SharedString::from(format!("perm-wait-{p:?}")), match p {
                Permission::Accessibility => "Opening Settings…",
                Permission::SystemAudio => "Waiting…",
                _ => "Waiting…",
            })
            .into_any_element(),
            PermissionState::Granted => ui::chip("Allowed", Some(icons::CHECK), theme::green()).into_any_element(),
            PermissionState::NotApplicable => ui::chip("Not needed", None, theme::faint()).into_any_element(),
            PermissionState::Denied => ui::button(SharedString::from(format!("perm-{p:?}")), "Open Settings", Btn::Accent)
                .on_click(move |_, _, cx| ctrl.update(cx, |c, _| c.shell.open_permission_settings(p)))
                .into_any_element(),
            PermissionState::NotDetermined => ui::button(SharedString::from(format!("perm-{p:?}")), "Allow", Btn::Accent)
                .on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.request_permission(p, cx)))
                .into_any_element(),
        };
        ui::row(Some(data), title, Some(desc.into()), trailing).into_any_element()
    };
    (
        vec![
            make(Permission::Microphone, icons::MIC, "Microphone", "Hear your side of the conversation"),
            make(Permission::SystemAudio, icons::SPEAKER, "System audio", "Hear everyone else on the call. No screen recording, no bots"),
        ],
        vec![make(Permission::Accessibility, icons::ACCESS, "Accessibility", "Dictation: listen for your shortcut and type text where your cursor is. Meetings: see who's talking in Zoom, Meet and Teams")],
    )
}

fn model_card(name: &'static str, badge: Option<&'static str>, _metas: Vec<AnyElement>, detail: &str, trailing: AnyElement, _active: bool) -> AnyElement {
    // Same two-line row as every other list: name (+ quiet "Recommended"),
    // one secondary line, control on the right.
    let title = match badge {
        Some(b) => div().flex().items_baseline().gap(px(6.)).child(div().text_size(px(theme::BODY)).child(name)).child(div().text_size(px(theme::SUBHEAD)).text_color(theme::faint()).child(b)),
        None => div().text_size(px(theme::BODY)).child(name),
    };
    div()
        .flex()
        .items_center()
        .gap(px(16.))
        .min_h(px(48.))
        .py(px(9.))
        .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.)).child(title).child(div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).truncate().child(detail.to_string())))
        .child(div().flex_shrink_0().flex().items_center().gap(px(8.)).child(trailing))
        .into_any_element()
}

/// Native-looking tooltip (yellow-less macOS style: popover material, 11pt).
struct Tip(SharedString);
impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(320.))
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

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.meeting.is_some() {
            self.refresh_meeting(false, cx);
        }
        let status = self.toolbar_status(cx);
        let header: AnyElement = match &self.meeting {
            Some(m) => {
                let dir = m.dir.clone();
                let ctrl = self.controller.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .w_full()
                    .child(glass_icon_button("back", icons::CHEVRON_LEFT).on_click(cx.listener(|this, _, _, cx| {
                        this.meeting = None;
                        cx.notify();
                    })))
                    .child(div().flex_1())
                    .child(status)
                    .child(self.share_button(m, cx))
                    .child(crate::destinations_ui::send_button(&self.dest_ui, dir.clone(), &m.meta, cx))
                    .child(self.meeting_menu(m.clone(), dir, ctrl, cx))
                    .into_any_element()
            }
            None => {
                // System Settings grammar: the section's name lives in the
                // toolbar, after a Back button when a sub-page is open.
                let (title, back) = self.toolbar_title(cx);
                div()
                .flex()
                .items_center()
                .gap(px(10.))
                .w_full()
                .when(back, |d| {
                    d.child(glass_icon_button("page-back", icons::CHEVRON_LEFT).on_click(cx.listener(|this, _, _, cx| this.close_sub_page(cx))))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .when(!back, |d| d.pl(px(12.)))
                        .text_size(px(theme::TITLE3))
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme::text())
                        .truncate()
                        .child(title),
                )
                .child(status)
                // Record / live capsule lives in the toolbar's trailing corner,
                // where the meeting view keeps its ••• menu.
                .when(self.page == Page::Home, |d| d.child(self.record_control(cx)))
                .into_any_element()
            }
        };
        let body = if let Some(m) = self.meeting.clone() {
            self.meeting_body(&m, cx)
        } else {
            match self.page {
            Page::Home => self.home(cx),
            Page::Preferences => self.preferences(cx),
            Page::You => self.you_page(cx),
            Page::Calendar => self.calendar(cx),
            Page::Transcription => self.models(true, cx),
            Page::Dictation => self.dictation_page(cx),
            Page::History => self.history_page(cx),
            Page::Microphones => self.microphones_page(cx),
            Page::Summaries => self.models(false, cx),
            Page::Notes => self.notes(cx),
            Page::Destinations => self.dest_ui.clone().into_any_element(),
            Page::Context => self.context_ui.clone().into_any_element(),
            Page::Permissions => self.permissions(cx),
            Page::Templates => self.library_page(stenly_core::library::Kind::Template, cx),
            Page::Recipes => self.library_page(stenly_core::library::Kind::Recipe, cx),
            Page::Trash => self.trash_page(cx),
            }
        };
        let in_meeting = self.meeting.is_some();
        if in_meeting && self.match_pending {
            self.match_anchor.scroll_to(window, cx);
            window.on_next_frame(|window, _| window.refresh());
            self.match_pending = false;
        }
        let err = self.controller.read(cx).last_error.clone();
        let ctrl = self.controller.clone();
        if glass_enabled() {
            self.sync_glass(window);
        }
        let toasts = self.toast_stack(cx);
        div()
            .size_full()
            .flex()
            .text_color(theme::text())
            .font_family(theme::SANS)
            .text_size(px(theme::BODY))
            .child(self.sidebar(cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .when(!glass_enabled(), |d| d.bg(theme::bg()))
                    // Unified toolbar: draggable, 52pt, hairline only once content scrolls under it.
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .h(px(52.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .px(px(16.))
                            .window_control_area(gpui::WindowControlArea::Drag)
                            .child(header),
                    )
                    .child(
                        div()
                            // separate scroll state per view, so Back lands at the top of the list
                            .id(if in_meeting { "content-meeting" } else { "content-page" })
                            .flex_1()
                            .overflow_y_scroll()
                            .when(in_meeting, |d| d.track_scroll(&self.meeting_scroll))
                            .child(
                                div()
                                    .mx_auto()
                                    .w_full()
                                    .max_w(px(if in_meeting { 700. } else { 640. }))
                                    .px(px(28.))
                                    .pt(px(4.))
                                    .pb(px(40.))
                                    .when_some(err, |d, e| {
                                        d.child(
                                            div()
                                                .id("err")
                                                .mb(px(14.))
                                                .flex()
                                                .items_center()
                                                .gap(px(10.))
                                                .px(px(12.))
                                                .py(px(9.))
                                                .rounded(px(10.))
                                                .bg(theme::card())
                                                .text_size(px(theme::BODY))
                                                .child(icon(icons::ALERT, 14., theme::amber()))
                                                .child(div().flex_1().min_w_0().child(e.clone()))
                                                .child(ui::button("dismiss-err", "Dismiss", Btn::Ghost).on_click(move |_, _, cx| {
                                                    cx.write_to_clipboard(ClipboardItem::new_string(e.clone()));
                                                    ctrl.update(cx, |c, cx| {
                                                        c.last_error = None;
                                                        cx.notify();
                                                    })
                                                })),
                                        )
                                    })
                                    .child(body),
                            ),
                    )
                    // Deferred: painted after the content, so its text can't draw over the cards.
                    .when_some(toasts, |d, t| d.child(gpui::deferred(t).with_priority(1))),
            )
            // Search sits in the titlebar band, just right of the traffic lights (Granola).
            .child(
                div()
                    .absolute()
                    .top(px(SEARCH_BUTTON_TOP))
                    // Same right edge as the selection highlight in the sidebar.
                    .left(px(SIDEBAR_W - if glass_enabled() { SIDEBAR_INSET + SIDEBAR_PAD } else { SIDEBAR_PAD } - SEARCH_BUTTON_SIZE))
                    .child(
                        div()
                            .id("titlebar-search")
                            .role(gpui::Role::Button)
                            .aria_label("Search meetings")
                            .size(px(SEARCH_BUTTON_SIZE))
                            .flex()
                            .items_center()
                            .justify_center()
                            // Circular, like the traffic lights beside it; also ~concentric
                            // with the panel corner it sits 3pt below.
                            .rounded_full()
                            .when(self.search.is_some(), |d| d.bg(theme::selected()))
                            .hover(|d| d.bg(theme::hover()))
                            .active(|d| d.bg(theme::selected()))
                            .tooltip(|_, cx| cx.new(|_| Tip("Search  ⌘F".into())).into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.search.is_some() {
                                    this.close_search(window, cx);
                                } else {
                                    this.open_search(window, cx);
                                }
                            }))
                            .child(crate::icons::icon_w(icons::SEARCH, 14., crate::icons::Weight::Medium, theme::muted())),
                    ),
            )
            .when_some(self.search.as_ref().map(|(p, _)| p.clone()), |d, palette| {
                d.child(gpui::deferred(
                    div()
                        .id("search-backdrop")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .flex()
                        .justify_center()
                        .items_start()
                        .pt(px(56.))
                        .px(px(24.))
                        .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, window, cx| this.close_search(window, cx)))
                        .child(palette),
                ).with_priority(2))
            })
            .when_some(self.event_card.clone(), |d, (ev, pos)| {
                let card = self.event_card(&ev, cx);
                d.child(
                    div()
                        .id("event-card-backdrop")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                            this.event_card = None;
                            cx.notify();
                        }))
                        .child(gpui::deferred(gpui::anchored().position(pos).snap_to_window_with_margin(px(12.)).child(div().mt(px(8.)).child(card)))),
                )
            })
    }
}
