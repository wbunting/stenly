//! Menubar (macOS) / StatusNotifierItem (Linux) icon, behind the core `Tray` trait.

use crossbeam_channel::Sender;
use stenly_core::platform::{Tray, TrayCommand, TrayState};

/// Fallback for builds without the `tray` feature (or when the desktop has no tray).
pub struct NoTray;
impl Tray for NoTray {
    fn set_state(&mut self, _: &TrayState) {}
}

#[cfg(feature = "tray")]
pub use real::TrayIconTray;

#[cfg(feature = "tray")]
mod real {
    use super::*;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    pub struct TrayIconTray {
        icon: TrayIcon,
        status: MenuItem,
        toggle: MenuItem,
        recording: bool,
        menu: Menu,
        update: MenuItem,
        update_shown: bool,
        shortcut_status: MenuItem,
        show_blocker: MenuItem,
        title: Option<String>,
        blocked: bool,
    }

    /// Draw the icon at runtime (no assets): a ring when idle, a filled red dot when recording.
    fn icon(recording: bool) -> Icon {
        const S: u32 = 36;
        let mut rgba = vec![0u8; (S * S * 4) as usize];
        let c = (S as f32 - 1.0) / 2.0;
        for y in 0..S {
            for x in 0..S {
                let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
                let i = ((y * S + x) * 4) as usize;
                let (r, g, b, a) = if recording {
                    let a = (11.5 - d).clamp(0.0, 1.0);
                    (235, 64, 52, a)
                } else {
                    // ring + small center dot, template-style black
                    let ring = 1.0 - ((d - 11.0).abs() - 1.6).clamp(0.0, 1.0);
                    let dot = (4.5 - d).clamp(0.0, 1.0);
                    let v = if cfg!(target_os = "macos") { 0 } else { 200 };
                    (v, v, v, ring.max(dot))
                };
                rgba[i..i + 4].copy_from_slice(&[r, g, b, (a * 255.0) as u8]);
            }
        }
        Icon::from_rgba(rgba, S, S).expect("valid icon")
    }

    impl TrayIconTray {
        pub fn new(commands: Sender<TrayCommand>) -> anyhow::Result<Self> {
            let status = MenuItem::new("Stenly", false, None);
            let toggle = MenuItem::new("Start recording", true, None);
            let dictate = MenuItem::new("Start dictation", true, None);
            let shortcut_status = MenuItem::new("Dictation shortcut settings…", true, None);
            let show_blocker = MenuItem::new("Show blocking app", false, None);
            let history = MenuItem::new("Dictations…", true, None);
            let open = MenuItem::new("Open Stenly…", true, None);
            let notes = MenuItem::new("Open notes folder", true, None);
            let quit = MenuItem::new("Quit Stenly", true, None);
            // Not in the menu until a newer release is found (see set_update).
            let update = MenuItem::new("Update available", true, None);
            let menu = Menu::new();
            menu.append(&status)?;
            menu.append(&PredefinedMenuItem::separator())?;
            menu.append(&toggle)?;
            menu.append(&dictate)?;
            menu.append(&shortcut_status)?;
            menu.append(&show_blocker)?;
            menu.append(&PredefinedMenuItem::separator())?;
            menu.append(&history)?;
            menu.append(&open)?;
            menu.append(&notes)?;
            menu.append(&PredefinedMenuItem::separator())?;
            menu.append(&quit)?;

            let ids = (toggle.id().clone(), open.id().clone(), notes.id().clone(), quit.id().clone(), dictate.id().clone(), history.id().clone(), status.id().clone(), update.id().clone());
            let shortcut_ids = (shortcut_status.id().clone(), show_blocker.id().clone());
            let toggle_state = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let ts = toggle_state.clone();
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                let cmd = if e.id == ids.0 {
                    if ts.load(std::sync::atomic::Ordering::Relaxed) { TrayCommand::StopRecording } else { TrayCommand::StartRecording }
                } else if e.id == ids.1 {
                    TrayCommand::OpenWindow
                } else if e.id == ids.2 {
                    TrayCommand::OpenNotesFolder
                } else if e.id == ids.3 {
                    TrayCommand::Quit
                } else if e.id == ids.4 {
                    TrayCommand::ToggleDictation
                } else if e.id == shortcut_ids.0 {
                    TrayCommand::OpenDictationSettings
                } else if e.id == shortcut_ids.1 {
                    TrayCommand::ShowShortcutBlocker
                } else if e.id == ids.5 {
                    TrayCommand::OpenDictations
                } else if e.id == ids.6 {
                    TrayCommand::OpenNextMeeting
                } else if e.id == ids.7 {
                    TrayCommand::OpenUpdate
                } else {
                    return;
                };
                let _ = commands.send(cmd);
            }));
            TOGGLE_STATE.set(toggle_state).ok();

            let builder = TrayIconBuilder::new();
            // Template icons (auto light/dark menubar tint) are a macOS concept.
            #[cfg(target_os = "macos")]
            let builder = builder.with_icon_templated(icon(false));
            #[cfg(not(target_os = "macos"))]
            let builder = builder.with_icon(icon(false));
            let icon = builder
                .with_tooltip("Stenly")
                .with_menu(Box::new(menu.clone()))
                .build()?;
            Ok(Self { icon, status, toggle, recording: false, menu, update, update_shown: false, shortcut_status, show_blocker, title: None, blocked: false })
        }
    }

    static TOGGLE_STATE: std::sync::OnceLock<std::sync::Arc<std::sync::atomic::AtomicBool>> = std::sync::OnceLock::new();

    impl Tray for TrayIconTray {
        fn set_title(&mut self, title: Option<&str>) {
            self.title = title.map(str::to_owned);
            let label = if self.blocked {
                Some(" Dictation shortcut blocked".to_string())
            } else {
                title.map(|t| format!(" {t}"))
            };
            #[cfg(target_os = "macos")]
            self.icon.set_title(label);
            let _ = self.icon.set_tooltip(Some(if self.blocked {
                "Stenly — dictation shortcut blocked"
            } else {
                title.unwrap_or("Stenly")
            }));
        }

        fn set_shortcut_block(
            &mut self,
            block: Option<&stenly_core::keyboard_guard::ShortcutBlock>,
        ) {
            self.blocked = block.is_some();
            self.shortcut_status.set_text(
                block
                    .map(|b| b.title())
                    .unwrap_or_else(|| "Dictation shortcut settings…".into()),
            );
            self.show_blocker
                .set_enabled(block.is_some_and(|b| b.owner.is_some()));
            let title = self.title.clone();
            self.set_title(title.as_deref());
        }

        fn set_update(&mut self, version: Option<&str>) {
            match version {
                Some(v) => {
                    self.update.set_text(format!("Update available: {v}"));
                    if !self.update_shown {
                        // Right under the status line, above the first separator.
                        self.update_shown = self.menu.insert(&self.update, 1).is_ok();
                    }
                }
                None if self.update_shown => {
                    let _ = self.menu.remove(&self.update);
                    self.update_shown = false;
                }
                None => {}
            }
        }

        fn set_state(&mut self, state: &TrayState) {
            // Only the "Next: …" line does anything: it opens that meeting's brief.
            let mut clickable = false;
            let (status, recording) = match state {
                TrayState::Idle { next: Some(n), brief } => {
                    clickable = true;
                    (if *brief { format!("Next: {n} · View brief") } else { format!("Next: {n}") }, false)
                }
                TrayState::Idle { next: None, .. } => ("No meetings coming up".to_string(), false),
                TrayState::Recording { title, since } => {
                    let mins = (chrono::Local::now() - *since).num_minutes();
                    (format!("● Recording “{title}” — {mins} min"), true)
                }
                TrayState::Processing { title } => (format!("Transcribing “{title}”…"), false),
            };
            self.status.set_text(status);
            self.status.set_enabled(clickable);
            self.toggle.set_text(if recording { "Stop recording" } else { "Start recording" });
            if let Some(ts) = TOGGLE_STATE.get() {
                ts.store(recording, std::sync::atomic::Ordering::Relaxed);
            }
            if recording != self.recording {
                self.recording = recording;
                #[cfg(target_os = "macos")]
                let _ = if recording { self.icon.set_icon(Some(icon(true))) } else { self.icon.set_icon_templated(Some(icon(false))) };
                #[cfg(not(target_os = "macos"))]
                let _ = self.icon.set_icon(Some(icon(recording)));
            }
        }
    }
}
