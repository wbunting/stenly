//! The seam between app logic and the operating system.
//!
//! App logic depends only on these traits. `stenly-platform` provides one
//! implementation per OS (and per desktop flavour on Linux), selected at
//! runtime by `stenly_platform::detect()`.

use crate::audio::AudioChunk;
use crate::calendar::{CalendarEvent, CalendarInfo};
use chrono::{DateTime, Utc};
use crossbeam_channel::Sender;

/// Which side of the conversation a stream carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Channel {
    /// Local microphone: the user.
    Mic,
    /// System output: everyone else on the call.
    System,
}

impl Channel {
    pub fn speaker_label(self) -> &'static str {
        match self {
            Channel::Mic => "Me",
            Channel::System => "Them",
        }
    }
    pub fn file_stem(self) -> &'static str {
        match self {
            Channel::Mic => "mic",
            Channel::System => "system",
        }
    }
}

/// A running capture. Dropping it stops the stream.
pub trait CaptureStream: Send {
    /// Human readable description of what is being captured (device / route).
    fn describe(&self) -> String;
}

/// A microphone the user can pick.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InputDevice {
    /// Stable identifier across reboots (CoreAudio UID / PipeWire node name).
    pub id: String,
    pub name: String,
    /// Loopback / aggregate / app-provided devices (BlackHole, ZoomAudioDevice…).
    pub is_virtual: bool,
    pub is_default: bool,
}

/// Opens audio streams. Implementations deliver interleaved f32 chunks at
/// whatever native rate the device runs; the core resamples to 16 kHz mono.
pub trait AudioBackend: Send + Sync {
    fn name(&self) -> &'static str;
    /// Start capturing the default microphone (or the best one: see `start_mic_from`).
    fn start_mic(&self, sink: Sender<AudioChunk>) -> anyhow::Result<Box<dyn CaptureStream>>;
    /// Microphones currently connected.
    fn input_devices(&self) -> Vec<InputDevice> {
        Vec::new()
    }
    /// Start the first available mic in `priority` (device ids or names),
    /// falling back to `start_mic` when none of them is connected.
    fn start_mic_from(&self, priority: &[String], sink: Sender<AudioChunk>) -> anyhow::Result<Box<dyn CaptureStream>> {
        let _ = priority;
        self.start_mic(sink)
    }
    /// Start capturing everything the machine plays (the remote side of a call).
    fn start_system(&self, sink: Sender<AudioChunk>) -> anyhow::Result<Box<dyn CaptureStream>>;
}

/// A key the user can bind for dictation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Hotkey {
    /// A modifier pressed on its own (VoiceInk's default style): "right_command",
    /// "right_option", "left_option", "right_control", "left_control",
    /// "right_shift", "fn".
    Modifier(String),
    /// A key combination: modifiers + a key, e.g. ("ctrl+shift", "space").
    Combo { modifiers: Vec<String>, key: String },
}

impl Hotkey {
    /// Parse "right_command", "fn", "ctrl+shift+space", "alt+cmd+v".
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_lowercase();
        if s.is_empty() || s == "none" {
            return None;
        }
        const MODS: &[&str] = &["right_command", "right_option", "left_option", "right_control", "left_control", "right_shift", "left_shift", "fn"];
        if MODS.contains(&s.as_str()) {
            return Some(Hotkey::Modifier(s));
        }
        let parts: Vec<&str> = s.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
        let (key, mods) = parts.split_last()?;
        let norm = |m: &str| match m {
            "cmd" | "command" | "super" | "meta" | "⌘" => Some("cmd"),
            "ctrl" | "control" | "⌃" => Some("ctrl"),
            "alt" | "option" | "opt" | "⌥" => Some("alt"),
            "shift" | "⇧" => Some("shift"),
            _ => None,
        };
        let mut modifiers: Vec<String> = mods.iter().map(|m| norm(m).map(str::to_string)).collect::<Option<_>>()?;
        modifiers.sort();
        modifiers.dedup();
        Some(Hotkey::Combo { modifiers, key: key.to_string() })
    }

    pub fn to_config(&self) -> String {
        match self {
            Hotkey::Modifier(m) => m.clone(),
            Hotkey::Combo { modifiers, key } => {
                let mut v = modifiers.clone();
                v.push(key.clone());
                v.join("+")
            }
        }
    }

    /// "Right ⌘", "⌃⇧Space".
    pub fn label(&self) -> String {
        match self {
            Hotkey::Modifier(m) => match m.as_str() {
                "right_command" => "Right ⌘".into(),
                "right_option" => "Right ⌥".into(),
                "left_option" => "Left ⌥".into(),
                "right_control" => "Right ⌃".into(),
                "left_control" => "Left ⌃".into(),
                "right_shift" => "Right ⇧".into(),
                "left_shift" => "Left ⇧".into(),
                "fn" => "Fn".into(),
                o => o.into(),
            },
            Hotkey::Combo { modifiers, key } => {
                let order = ["ctrl", "alt", "shift", "cmd"];
                let sym = |m: &str| match m {
                    "ctrl" => "⌃",
                    "alt" => "⌥",
                    "shift" => "⇧",
                    "cmd" => "⌘",
                    _ => "",
                };
                let mut out = String::new();
                for o in order {
                    if modifiers.iter().any(|m| m == o) {
                        out.push_str(sym(o));
                    }
                }
                let k = match key.as_str() {
                    "space" => "Space".to_string(),
                    "return" | "enter" => "↩".to_string(),
                    "escape" | "esc" => "Esc".to_string(),
                    "tab" => "⇥".to_string(),
                    k if k.len() == 1 => k.to_uppercase(),
                    k => {
                        let mut c = k.chars();
                        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
                    }
                };
                out + &k
            }
        }
    }
}

/// What a global hotkey did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HotkeyEvent {
    /// Binding slot (0 = dictate, 1 = second dictate key, 2 = paste last, 3 = cancel)
    /// went down / up. `at` is a monotonic timestamp in seconds from the event itself.
    Pressed { slot: u8, at: f64 },
    Released { slot: u8, at: f64 },
}

/// System-wide keyboard shortcuts that work while other apps are focused.
pub trait Hotkeys: Send + Sync {
    /// Replace the watched bindings (index = slot). Events arrive on `sink`
    /// from a background thread. Returns an error if the OS refuses (no
    /// permission): the app shows how to grant it.
    fn watch(&self, bindings: Vec<Option<Hotkey>>, sink: Sender<HotkeyEvent>) -> anyhow::Result<()>;
    /// Stop watching (e.g. dictation disabled).
    fn stop(&self) {}
    /// Whether Esc should be swallowed while a dictation is in progress.
    fn set_capture_escape(&self, _on: bool) {}
}

/// Puts text into whatever app has keyboard focus.
pub trait TextInjector: Send + Sync {
    /// Paste `text` at the cursor. When `restore_clipboard_after` is Some,
    /// the previous clipboard comes back after that delay.
    fn paste(&self, text: &str, restore_clipboard_after: Option<std::time::Duration>) -> anyhow::Result<()>;
    /// Press Return (auto-send in chat apps).
    fn press_return(&self) -> anyhow::Result<()>;
    /// Copy without pasting.
    fn copy(&self, text: &str) -> anyhow::Result<()>;
    /// Name of the app that has focus (for history / per-app behaviour).
    fn frontmost_app(&self) -> Option<String> {
        None
    }
    /// Text currently selected in the focused app (context for "rewrite
    /// this" / "reply to this" in AI polish). Never touches the clipboard.
    fn selected_text(&self) -> Option<String> {
        None
    }
}

/// A process that currently holds the microphone open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicUser {
    pub pid: Option<i32>,
    /// Bundle id (macOS) or binary / application name (Linux).
    pub app_id: String,
}

/// Answers "is someone on a call right now?" without any audio access.
pub trait MicActivity: Send + Sync {
    /// Processes other than stenly that currently capture from an input device.
    fn mic_users(&self) -> anyhow::Result<Vec<MicUser>>;
    /// Platform id of the display showing `u`'s main window (for meeting
    /// apps: where the call is), so floating UI can appear next to it.
    /// Called off the main thread.
    fn window_display(&self, _u: &MicUser) -> Option<u64> {
        None
    }
}

/// Calendar data exposed by the host OS. Implementations live exclusively in
/// `stenly-platform`; scheduling and mapping consumers stay platform agnostic.
pub trait CalendarSource: Send + Sync {
    fn name(&self) -> &'static str;
    fn permission(&self) -> PermissionState;
    fn request_permission(&self);
    fn calendars(&self) -> anyhow::Result<Vec<CalendarInfo>>;
    fn events(&self, calendar_ids: &[String], from: DateTime<Utc>, to: DateTime<Utc>) -> anyhow::Result<Vec<CalendarEvent>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Microphone,
    /// Capturing other apps' audio (macOS: "System Audio Recording Only").
    SystemAudio,
    /// Read Calendar.app accounts through the platform calendar source.
    Calendars,
    /// Optional: lets the app read window titles (speaker names in calls).
    Accessibility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionState {
    Granted,
    Denied,
    /// Never asked yet (or the OS can't tell us until we try).
    NotDetermined,
    /// Not a thing on this platform.
    NotApplicable,
}

/// Desktop integration that cannot be expressed through gpui itself.
/// Every method has a no-op default so a minimal Linux build works anywhere.
pub trait DesktopShell {
    /// Hide from Dock / taskbar (menubar-only app).
    fn set_background_app(&self) {}
    /// Behave like a normal app (Dock icon, keeps focus across system prompts)
    /// while a window that needs attention, e.g. onboarding, is open.
    fn set_foreground_app(&self) {}
    /// Exclude a native window from screenshots / screen sharing, if supported.
    /// `raw` is the platform window handle (NSWindow* on macOS).
    fn hide_from_capture(&self, _raw: *mut std::ffi::c_void) {}
    /// Per-window native fixes, called once for every window we open.
    /// `raw` is the platform view handle (NSView* on macOS).
    fn configure_window(&self, _raw: *mut std::ffi::c_void) {}
    /// Make a floating notification window visible over other apps even when
    /// stenly isn't the active app, without stealing focus.
    fn show_floating(&self, _raw: *mut std::ffi::c_void) {}
    /// Hide / re-show a floating window without closing it. Closing a window
    /// makes AppKit pick another window of ours to focus, which can steal
    /// focus from the app the user is typing in.
    fn set_floating_visible(&self, _raw: *mut std::ffi::c_void, _visible: bool) {}
    /// Move a window so its top-left is at `(x, y)` in top-left-origin global
    /// screen points (gpui's coordinate space).
    fn move_window(&self, _raw: *mut std::ffi::c_void, _x: f64, _y: f64) {}
    fn supports_hide_from_capture(&self) -> bool {
        false
    }
    fn permission(&self, _p: Permission) -> PermissionState {
        PermissionState::NotApplicable
    }
    /// Trigger the OS prompt (if any). Must be called on the main thread.
    fn request_permission(&self, _p: Permission) {}
    /// Open the relevant OS settings page.
    fn open_permission_settings(&self, _p: Permission) {}
    /// Register / unregister to start at login.
    fn set_launch_at_login(&self, _enabled: bool) -> anyhow::Result<()> {
        anyhow::bail!("launch at login isn't supported on this platform yet")
    }
}

/// Status shown in the tray / menubar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayState {
    Idle { next: Option<String> },
    Recording { title: String, since: chrono::DateTime<chrono::Local> },
    Processing { title: String },
}

/// Commands the tray can emit back to the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayCommand {
    StartRecording,
    StopRecording,
    OpenWindow,
    OpenNotesFolder,
    /// Start (or stop) a dictation from the menu.
    ToggleDictation,
    OpenDictations,
    Quit,
}

pub trait Tray {
    fn set_state(&mut self, state: &TrayState);
    /// Text next to the icon (macOS menubar title / SNI tooltip).
    fn set_title(&mut self, _title: Option<&str>) {}
}

#[cfg(test)]
mod hotkey_tests {
    use super::Hotkey;

    #[test]
    fn parses_and_labels() {
        assert_eq!(Hotkey::parse("right_command"), Some(Hotkey::Modifier("right_command".into())));
        assert_eq!(Hotkey::parse("none"), None);
        let h = Hotkey::parse("Shift+Ctrl+Space").unwrap();
        assert_eq!(h.to_config(), "ctrl+shift+space");
        assert_eq!(h.label(), "⌃⇧Space");
        assert_eq!(Hotkey::parse("alt+cmd+v").unwrap().label(), "⌥⌘V");
        assert_eq!(Hotkey::parse("fn").unwrap().label(), "Fn");
        assert!(Hotkey::parse("hyper+x").is_none());
    }
}
