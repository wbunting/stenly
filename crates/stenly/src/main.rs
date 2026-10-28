//! stenly: invisible local-first meeting recorder.
//!
//! This crate is only wiring + views. Behaviour lives in `stenly-core`, OS code
//! in `stenly-platform`, ASR in `stenly-asr`.

mod context_ui;
mod controller;
mod destinations_ui;
mod dictation;
mod dictation_hud;
mod dictation_pages;
mod icons;
mod library_pages;
mod live_pill;
mod main_window;
mod meeting_view;
mod meeting_browser;
mod menus;
mod onboarding;
mod popup;
mod search_palette;
mod shortcut_field;
mod squircle;
mod text_input;
mod theme;
mod toast;
mod trash_page;
mod tray;
mod ui;
mod updates;

use controller::ControllerEvent;
use gpui::{App, AsyncApp, QuitMode};
use std::rc::Rc;
use std::time::Duration;
use stenly_core::config::Config;
use stenly_core::platform::{Tray, TrayCommand};

fn main() {
    init_logging();

    let config = match Config::load_or_init() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Stenly: {e:#}");
            std::process::exit(1);
        }
    };
    if let Err(e) = single_instance_lock(&config) {
        eprintln!("Stenly: {e:#}");
        std::process::exit(0);
    }
    let show_window = std::env::args().any(|a| a == "--window") || std::env::var_os("STENLY_SHOW_WINDOW").is_some();

    let app = gpui_platform::application().with_quit_mode(QuitMode::Explicit);
    app.on_reopen(|cx| {
        if let Some(ctrl) = REOPEN.with(|r| r.borrow().clone()) {
            main_window::open(ctrl, None, cx);
        }
    });
    app.run(move |cx: &mut App| {
        let platform = stenly_platform::detect();
        // Regular app with a Dock icon (like Granola) plus a menubar item.
        // Closing the window keeps stenly running in the menubar.
        platform.shell.set_foreground_app();
        let shell: Rc<dyn stenly_core::platform::DesktopShell> = Rc::from(platform.shell);
        tracing::info!(platform = platform.name, audio = platform.audio.name(), "Stenly starting");
        theme::load_fonts(cx);
        if let Ok(a) = std::env::var("STENLY_APPEARANCE") {
            cx.set_window_appearance(Some(if a == "light" { gpui::WindowAppearance::Light } else { gpui::WindowAppearance::Dark }));
        }
        theme::sync(cx);
        // Any quit path (Cmd-Q, Dock, tray, logout): stop and reap speech workers.
        cx.on_app_quit(|_| {
            // Inference is isolated: killing a worker cannot tear down the GUI's
            // runtime while a decode is in flight.
            stenly_asr::shutdown_and_wait();
            async {}
        })
        .detach();
        text_input::bind_keys(cx);
        search_palette::bind_keys(cx);

        // Local MCP endpoint so AI tools can pull meetings while the app runs.
        if config.mcp.http {
            let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, config.mcp.port));
            match stenly_mcp::spawn_http(addr) {
                Ok(_) => tracing::info!(url = %format!("http://{addr}/mcp"), "MCP server listening"),
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "MCP server not started"),
            }
        }

        let controller = controller::new(cx, config, platform.audio.clone(), platform.mic_activity.clone(), shell.clone(), platform.calendar.clone());
        controller.update(cx, |c, _| c.meeting_ax = platform.meeting_ax.clone());
        menus::init(controller.clone(), cx);
        updates::init(controller.clone(), cx);

        // Dictation: global shortcut → HUD → paste at cursor.
        let dictation = dictation::new(controller.clone(), platform.audio.clone(), platform.hotkeys.clone(), platform.injector.clone(), cx);
        DICTATION.with(|d| *d.borrow_mut() = Some(dictation.clone()));
        {
            let shell = shell.clone();
            let dict = dictation.clone();
            // Reconcile the HUD window with the phase on the next turn (never
            // open/close windows inside the event that changed it), and keep
            // the window across quick stop→start bursts instead of churning it.
            let pending = std::rc::Rc::new(std::cell::Cell::new(false));
            cx.subscribe(&dictation, move |_, _ev: &dictation::DictationEvent, cx| {
                if pending.replace(true) {
                    return;
                }
                let (dict, shell, pending) = (dict.clone(), shell.clone(), pending.clone());
                cx.spawn(async move |cx: &mut AsyncApp| {
                    cx.background_executor().timer(Duration::from_millis(16)).await;
                    let _ = cx.update(|cx| {
                        pending.set(false);
                        let visible = !matches!(dict.read(cx).phase, dictation::Phase::Idle);
                        if std::env::var_os("STENLY_NO_HUD").is_none() {
                            dictation_hud::set_visible(dict.clone(), shell.as_ref(), visible, cx);
                        }
                    });
                })
                .detach();
            })
            .detach();
        }

        // Popups + notifications driven by controller events.
        {
            let shell = shell.clone();
            let ctrl = controller.clone();
            cx.subscribe(&controller, move |_, ev: &ControllerEvent, cx| match ev {
                ControllerEvent::Prompt(p) => {
                    let (p, ctrl, shell) = (p.clone(), ctrl.clone(), shell.clone());
                    cx.defer(move |cx| popup::show(popup::PopupKind::Start(p), ctrl, shell.as_ref(), cx));
                }
                ControllerEvent::SuggestStop(reason) => {
                    let (reason, ctrl, shell) = (reason.clone(), ctrl.clone(), shell.clone());
                    cx.defer(move |cx| popup::show(popup::PopupKind::Stop(reason), ctrl, shell.as_ref(), cx));
                }
                ControllerEvent::NoteReady(dir) => {
                    tracing::info!(session = %dir.display(), "note ready");
                    // Never steal focus when a note finishes. The popup is
                    // non-activating and hidden from screen sharing; Open and
                    // Share with attendees remain explicit user actions.
                    if !ctrl.read(cx).is_recording()
                        && let Ok(meta) = stenly_core::session::SessionMeta::load(dir)
                    {
                        let (ctrl, shell, dir) = (ctrl.clone(), shell.clone(), dir.clone());
                        let title = meta.title;
                        cx.spawn(async move |cx: &mut AsyncApp| {
                            // Opening a native popup during the controller event
                            // can re-enter GPUI while it is borrowed. Wait one
                            // turn; this remains non-activating.
                            cx.background_executor().timer(Duration::from_millis(16)).await;
                            let _ = cx.update(|cx| popup::show(popup::PopupKind::NoteReady { dir, title }, ctrl, shell.as_ref(), cx));
                        })
                        .detach();
                    }
                },
                ControllerEvent::DismissPopups => cx.defer(popup::close_all),
            })
            .detach();
        }

        // Tray: commands come in on a channel, state goes out on every controller change.
        let (tray_tx, tray_rx) = crossbeam_channel::unbounded::<TrayCommand>();
        let tray: Rc<std::cell::RefCell<Box<dyn Tray>>> = Rc::new(std::cell::RefCell::new(make_tray(tray_tx)));
        {
            let tray = tray.clone();
            let pill_shell = shell.clone();
            let last = std::cell::RefCell::new(None);
            cx.observe(&controller, move |ctrl, cx| {
                let c = ctrl.read(cx);
                let state = c.tray_state();
                let title = c.tray_title();
                let recording = c.is_recording();
                if matches!(state, stenly_core::platform::TrayState::Recording { .. }) {
                    cx.defer(popup::close_start_prompts);
                }
                if recording && !live_pill::is_open(cx) {
                    let ctrl = ctrl.clone();
                    let shell = pill_shell.clone();
                    // defer: can't open a window from inside an observer callback's borrow
                    cx.defer(move |cx| live_pill::show(ctrl, shell.as_ref(), cx));
                } else if !recording && live_pill::is_open(cx) && std::env::var_os("STENLY_DEMO_POPUPS").is_none() {
                    cx.defer(live_pill::close);
                }
                let key = (state.clone(), title);
                if last.borrow().as_ref() != Some(&key) {
                    let mut t = tray.borrow_mut();
                    t.set_state(&state);
                    t.set_title(key.1.as_deref());
                    *last.borrow_mut() = Some(key);
                }
            })
            .detach();
        }
        {
            let ctrl = controller.clone();
            cx.spawn(async move |cx: &mut AsyncApp| {
                loop {
                    cx.background_executor().timer(Duration::from_millis(150)).await;
                    while let Ok(cmd) = tray_rx.try_recv() {
                        let ctrl = ctrl.clone();
                        cx.update(|cx| match cmd {
                            TrayCommand::StartRecording => ctrl.update(cx, |c, cx| c.start_now(cx)),
                            TrayCommand::StopRecording => ctrl.update(cx, |c, cx| c.stop_recording(cx)),
                            TrayCommand::OpenWindow => {
                                main_window::open(ctrl, None, cx);
                            }
                            TrayCommand::OpenNotesFolder => {
                                let dir = ctrl.read(cx).config.paths().notes_dir;
                                let _ = std::fs::create_dir_all(&dir);
                                cx.open_with_system(&dir);
                            }
                            TrayCommand::ToggleDictation => {
                                if let Some(d) = dictation_entity() {
                                    d.update(cx, |d, cx| if matches!(d.phase, dictation::Phase::Recording { .. }) { d.stop(cx) } else { d.start(cx) });
                                }
                            }
                            TrayCommand::OpenDictations => {
                                main_window::open(ctrl.clone(), Some(main_window::Page::History), cx);
                            }
                            TrayCommand::Quit => {
                                ctrl.update(cx, |c, cx| c.stop_recording(cx));
                                let ctrl = ctrl.clone();
                                cx.spawn(async move |cx: &mut AsyncApp| safe_quit(ctrl, cx).await).detach();
                            }
                        });
                    }
                }
            })
            .detach();
        }

        // Live config reload when the file changes on disk (hand edits).
        watch_config(controller.clone(), cx);

        // Launching the app again (Finder / Spotlight / `open`) → open the window.
        REOPEN.with(|r| *r.borrow_mut() = Some(controller.clone()));

        #[cfg(feature = "snapshot")]
        snapshot::start(cx);

        // Dev: STENLY_STRESS_WINDOWS=N opens/closes the main window + a popup N times
        // (regression test for the AppKit Touch Bar / AccessKit KVO abort).
        if let Some(n) = std::env::var("STENLY_STRESS_WINDOWS").ok().and_then(|v| v.parse::<usize>().ok()) {
            let ctrl = controller.clone();
            let shell = shell.clone();
            cx.spawn(async move |cx: &mut AsyncApp| {
                for i in 0..n {
                    let _ = cx.update(|cx| {
                        main_window::open(ctrl.clone(), None, cx);
                        popup::show(popup::PopupKind::Stop("stress test".into()), ctrl.clone(), shell.as_ref(), cx);
                        live_pill::show(ctrl.clone(), shell.as_ref(), cx);
                    });
                    cx.background_executor().timer(Duration::from_millis(200)).await;
                    // Late discovery of the call display exercises follow_call,
                    // without opening a microphone or reading private sessions.
                    let _ = ctrl.update(cx, |c, cx| {
                        let displays = cx.displays();
                        c.call_display = displays.get(i % displays.len().max(1)).map(|d| u64::from(d.id()));
                        cx.notify();
                    });
                    cx.background_executor().timer(Duration::from_millis(200)).await;
                    let _ = cx.update(|cx| {
                        for w in cx.windows() {
                            let _ = w.update(cx, |_, window, _| window.remove_window());
                        }
                    });
                    cx.background_executor().timer(Duration::from_millis(300)).await;
                    tracing::info!(i, "stress cycle");
                }
                tracing::info!("stress done");
                let _ = cx.update(|cx| cx.quit());
            })
            .detach();
        }

        // Dev: STENLY_NOTE_READY=<session dir> shows the non-activating
        // post-processing share affordance without needing a real transcription.
        if let Ok(dir) = std::env::var("STENLY_NOTE_READY") {
            let dir = std::path::PathBuf::from(dir);
            if let Ok(meta) = stenly_core::session::SessionMeta::load(&dir) {
                let ctrl = controller.clone();
                let shell = shell.clone();
                cx.spawn(async move |cx: &mut AsyncApp| {
                    cx.background_executor().timer(Duration::from_millis(16)).await;
                    let _ = cx.update(|cx| popup::show(popup::PopupKind::NoteReady { dir, title: meta.title }, ctrl, shell.as_ref(), cx));
                })
                .detach();
            }
        }

        // Dev: STENLY_DEMO_POPUPS=1 shows the stop prompt + live pill (for snapshots).
        if std::env::var_os("STENLY_DEMO_POPUPS").is_some() {
            let ctrl = controller.clone();
            let shell = shell.clone();
            cx.defer(move |cx| {
                popup::show(popup::PopupKind::Stop("Nobody has spoken for 3 minutes".into()), ctrl.clone(), shell.as_ref(), cx);
                live_pill::show(ctrl, shell.as_ref(), cx);
            });
        }

        // Dev: STENLY_TEST_RECORD=<secs> records via the normal GUI path, then quits.
        if let Some(secs) = std::env::var("STENLY_TEST_RECORD").ok().and_then(|v| v.parse::<u64>().ok()) {
            let ctrl = controller.clone();
            cx.spawn(async move |cx: &mut AsyncApp| {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                ctrl.update(cx, |c, cx| c.start_now(cx));
                let _ = std::process::Command::new("/usr/bin/say").arg("gui record probe, one two three four five").spawn();
                for _ in 0..secs * 2 {
                    cx.background_executor().timer(Duration::from_millis(500)).await;
                    let l = ctrl.read_with(cx, |c, _| c.levels.as_ref().map(|l| (l.get(stenly_core::platform::Channel::Mic), l.get(stenly_core::platform::Channel::System))));
                    tracing::info!(?l, "test-record levels");
                }
                ctrl.update(cx, |c, cx| c.stop_recording(cx));
                safe_quit(ctrl, cx).await;
            })
            .detach();
        }

        // Dev: STENLY_TEST_DICTATE=<secs> starts a dictation, stops it after <secs>
        // (as if the shortcut were tapped twice) and logs the result.
        if let Some(secs) = std::env::var("STENLY_TEST_DICTATE").ok().and_then(|v| v.parse::<f32>().ok()) {
            let d = dictation.clone();
            cx.spawn(async move |cx: &mut AsyncApp| {
                cx.background_executor().timer(Duration::from_millis(2500)).await;
                let _ = d.update(cx, |d, cx| d.start(cx));
                cx.background_executor().timer(Duration::from_secs_f32(secs)).await;
                let _ = d.update(cx, |d, cx| d.stop(cx));
            })
            .detach();
        }

        // Dev: STENLY_TEST_HOTKEY=tap|hold posts Right ⌘ through the system event
        // stream (macOS, needs Accessibility) to test the real shortcut path:
        // tap = press/release, then tap again 6 s later; hold = hold for 6 s.
        #[cfg(target_os = "macos")]
        if let Ok(kind) = std::env::var("STENLY_TEST_HOTKEY") {
            std::thread::spawn(move || {
                let p = stenly_platform::post_right_command;
                std::thread::sleep(Duration::from_millis(3500));
                if kind == "hold" {
                    p(true);
                    std::thread::sleep(Duration::from_secs(6));
                    p(false);
                } else if kind == "esc" {
                    // tap to start, then Esc after 3 s: nothing should be pasted
                    p(true);
                    std::thread::sleep(Duration::from_millis(120));
                    p(false);
                    std::thread::sleep(Duration::from_secs(3));
                    stenly_platform::post_key(0x35, false);
                } else if kind == "keysonly" {
                    // same Right ⌘ burst, but dictation is disabled: isolates the
                    // effect of the posted key events themselves
                    for i in 0..12u64 {
                        p(true);
                        std::thread::sleep(Duration::from_millis(40 + (i * 37) % 120));
                        p(false);
                        std::thread::sleep(Duration::from_millis(80 + (i * 53) % 220));
                    }
                } else if kind == "spam" {
                    // 12 rapid taps 80-300 ms apart: must never wedge, paste garbage, or crash
                    for i in 0..12u64 {
                        p(true);
                        std::thread::sleep(Duration::from_millis(40 + (i * 37) % 120));
                        p(false);
                        std::thread::sleep(Duration::from_millis(80 + (i * 53) % 220));
                    }
                    std::thread::sleep(Duration::from_secs(2));
                    // then a normal tap / speak / tap must still work
                    p(true);
                    std::thread::sleep(Duration::from_millis(100));
                    p(false);
                    std::thread::sleep(Duration::from_secs(5));
                    p(true);
                    std::thread::sleep(Duration::from_millis(100));
                    p(false);
                } else if kind == "combo" {
                    // Right ⌘ held as part of ⌘C: must not dictate
                    p(true);
                    std::thread::sleep(Duration::from_millis(150));
                    stenly_platform::post_key(0x08, true);
                    std::thread::sleep(Duration::from_millis(100));
                    p(false);
                } else {
                    p(true);
                    std::thread::sleep(Duration::from_millis(120));
                    p(false);
                    std::thread::sleep(Duration::from_secs(6));
                    p(true);
                    std::thread::sleep(Duration::from_millis(120));
                    p(false);
                }
                tracing::info!(%kind, "test hotkey posted");
            });
        }

        // Dev: STENLY_TEST_PASTE="text" pastes after 4 s via the real injector
        // (logs the frontmost app first). For testing paste into a scratch app.
        if let Ok(text) = std::env::var("STENLY_TEST_PASTE") {
            let inj = platform.injector.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(4));
                let front = inj.frontmost_app().unwrap_or_default();
                let only = std::env::var("STENLY_PASTE_ONLY_INTO").unwrap_or_default();
                if !only.is_empty() && front != only {
                    tracing::warn!(%front, "test paste skipped: wrong app in front");
                    return;
                }
                let r = inj.paste(&text, Some(Duration::from_millis(1500)));
                tracing::info!(%front, ok = r.is_ok(), err = ?r.err().map(|e| e.to_string()), "test paste");
            });
        }

        // Dev: STENLY_QUIT_AFTER_MS=<ms> quits (like Cmd-Q) shortly after launch.
        if let Some(ms) = std::env::var("STENLY_QUIT_AFTER_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
            cx.spawn(async move |cx: &mut AsyncApp| {
                cx.background_executor().timer(Duration::from_millis(ms)).await;
                tracing::info!("dev quit");
                cx.update(|cx| cx.quit());
            })
            .detach();
        }

        let onboarded = controller.read(cx).config.app.onboarded;
        if !onboarded || std::env::var_os("STENLY_ONBOARDING").is_some() {
            onboarding::open(controller.clone(), cx);
        } else if show_window || !cfg!(feature = "tray") {
            main_window::open(controller.clone(), None, cx);
        }
        // Re-check the menubar title every 30 s ("in 5m" → "in 4m").
        {
            let ctrl = controller.clone();
            cx.spawn(async move |cx: &mut AsyncApp| loop {
                cx.background_executor().timer(Duration::from_secs(30)).await;
                ctrl.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
        // Keep the tray alive for the life of the app.
        std::mem::forget(tray);
    });
}

#[cfg(feature = "snapshot")]
mod snapshot {
    use gpui::{App, AsyncApp};
    use std::time::Duration;

    pub fn start(cx: &mut App) {
        let Some(dir) = std::env::var_os("STENLY_SNAPSHOT").map(std::path::PathBuf::from) else { return };
        let _ = std::fs::create_dir_all(&dir);
        cx.spawn(async move |cx: &mut AsyncApp| loop {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = cx.update(|cx| {
                for (i, w) in cx.windows().into_iter().enumerate() {
                    if !cx.windows().contains(&w) {
                        continue;
                    }
                    let kind = if w.downcast::<crate::live_pill::LivePill>().is_some() {
                        "pill"
                    } else if w.downcast::<crate::popup::Popup>().is_some() {
                        "popup"
                    } else if w.downcast::<crate::onboarding::Onboarding>().is_some() {
                        "onboarding"
                    } else {
                        "main"
                    };
                    let _ = w.update(cx, |_, window, _| {
                        match window.render_to_image() {
                            Ok(img) => {
                                let _ = img.save(dir.join(format!("{kind}-{i}.png")));
                            }
                            Err(e) => tracing::warn!(error = %e, "snapshot failed"),
                        }
                    });
                }
            });
        })
        .detach();
    }
}

/// stderr when run from a terminal; also ~/Library/Logs/stenly.log (macOS) or
/// $XDG_STATE_HOME/stenly/stenly.log so bundled-app issues can be diagnosed.
fn init_logging() {
    use tracing_subscriber::prelude::*;
    let directives = std::env::var("RUST_LOG").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| "info,sherpa=warn".into());
    let filter = || tracing_subscriber::EnvFilter::new(&directives);
    let log_path = directories::BaseDirs::new().map(|b| {
        if cfg!(target_os = "macos") {
            b.home_dir().join("Library/Logs/stenly.log")
        } else {
            b.state_dir().unwrap_or(b.data_local_dir()).join("stenly/stenly.log")
        }
    });
    let file = log_path.and_then(|p| {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        // keep the log small: truncate if over 5 MB
        if std::fs::metadata(&p).is_ok_and(|m| m.len() > 5 << 20) {
            let _ = std::fs::remove_file(&p);
        }
        std::fs::OpenOptions::new().create(true).append(true).open(p).ok()
    });
    let stderr = tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_filter(filter());
    let file_layer = file.map(|f| tracing_subscriber::fmt::layer().with_ansi(false).with_writer(std::sync::Mutex::new(f)).with_filter(filter()));
    tracing_subscriber::registry().with(stderr).with(file_layer).init();
}

/// Quit promptly. Unfinished processing remains pending for the next launch;
/// inference workers can be stopped without running destructors in the UI.
pub(crate) async fn safe_quit(ctrl: gpui::Entity<controller::Controller>, cx: &mut AsyncApp) {
    // Immediate feedback: windows go away even if processing takes a moment to stop.
    cx.update(|cx| {
        for w in cx.windows() {
            let _ = w.update(cx, |_, window, _| window.remove_window());
        }
    });
    stenly_asr::request_shutdown();
    let t0 = std::time::Instant::now();
    while ctrl.read_with(cx, |c, _| c.is_processing()) && t0.elapsed() < Duration::from_secs(5) {
        cx.background_executor().timer(Duration::from_millis(100)).await;
    }
    // on_app_quit also reaps any remaining workers.
    cx.update(|cx| cx.quit());
}

/// Native per-window setup (see `DesktopShell::configure_window`).
pub fn configure_native(window: &gpui::Window, shell: &dyn stenly_core::platform::DesktopShell) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    if let Ok(h) = HasWindowHandle::window_handle(window) {
        if let RawWindowHandle::AppKit(a) = h.as_raw() {
            shell.configure_window(a.ns_view.as_ptr());
        }
    }
}

thread_local! {
    static REOPEN: std::cell::RefCell<Option<gpui::Entity<controller::Controller>>> = const { std::cell::RefCell::new(None) };
    /// The dictation controller, for settings / menus that need it.
    pub static DICTATION: std::cell::RefCell<Option<gpui::Entity<dictation::DictationController>>> = const { std::cell::RefCell::new(None) };
}

pub fn dictation_entity() -> Option<gpui::Entity<dictation::DictationController>> {
    DICTATION.with(|d| d.borrow().clone())
}

fn make_tray(tx: crossbeam_channel::Sender<TrayCommand>) -> Box<dyn Tray> {
    #[cfg(feature = "tray")]
    match tray::TrayIconTray::new(tx) {
        Ok(t) => {
            tracing::info!("menubar item created");
            return Box::new(t);
        }
        Err(e) => tracing::warn!(error = %e, "no tray available; use the window"),
    }
    #[cfg(not(feature = "tray"))]
    let _ = tx;
    Box::new(tray::NoTray)
}

fn watch_config(controller: gpui::Entity<controller::Controller>, cx: &mut App) {
    use notify::{RecursiveMode, Watcher};
    let path = stenly_core::config::config_path();
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut watcher = match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok_and(|e| e.kind.is_modify() || e.kind.is_create()) {
            let _ = tx.send(());
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(error = %e, "config watcher unavailable");
            return;
        }
    };
    // Watch the directory: editors replace files atomically.
    if let Some(dir) = path.parent() {
        let _ = watcher.watch(dir, RecursiveMode::NonRecursive);
    }
    cx.spawn(async move |cx: &mut AsyncApp| {
        let _watcher = watcher;
        loop {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            if rx.try_recv().is_err() {
                continue;
            }
            while rx.try_recv().is_ok() {}
            match Config::load(&path) {
                Ok(cfg) => {
                    let changed = controller.read_with(cx, |c, _| c.config != cfg);
                    if changed {
                        tracing::info!("config reloaded");
                        controller.update(cx, |c, cx| c.reload_config(cfg, cx));
                    }
                }
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "ignoring invalid config"),
            }
        }
    })
    .detach();
}

/// One stenly per user: two would double-record and double-prompt.
fn single_instance_lock(config: &Config) -> anyhow::Result<()> {
    use std::io::Write;
    let dir = config.paths().data_dir;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("stenly.pid");
    if let Ok(pid) = std::fs::read_to_string(&path)
        && let Ok(pid) = pid.trim().parse::<i32>()
            && pid != std::process::id() as i32 && process_alive(pid) {
                anyhow::bail!("already running (pid {pid})");
            }
    let mut f = std::fs::File::create(&path)?;
    write!(f, "{}", std::process::id())?;
    Ok(())
}

fn process_alive(pid: i32) -> bool {
    // `kill -0` without a libc dependency.
    std::process::Command::new("kill").args(["-0", &pid.to_string()]).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
}
