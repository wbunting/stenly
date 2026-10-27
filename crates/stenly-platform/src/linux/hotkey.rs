//! Global shortcuts on Linux by reading keyboard events from /dev/input.
//!
//! Works the same on X11 and every Wayland compositor (which deliberately
//! offer no global key grab to ordinary clients). Needs read access to
//! /dev/input/event* — the user must be in the `input` group (the settings
//! page says so when it fails). Events are only observed, never swallowed.
//!
//! Each `input_event` on 64-bit Linux is 24 bytes: timeval (16), type (u16),
//! code (u16), value (i32). We watch EV_KEY (type 1): value 1 = down,
//! 0 = up, 2 = auto-repeat.

use anyhow::{Result, bail};
use crossbeam_channel::Sender;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use stenly_core::platform::{Hotkey, HotkeyEvent, Hotkeys};

const EV_KEY: u16 = 1;
const KEY_ESC: u16 = 1;
const KEY_LEFTCTRL: u16 = 29;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_RIGHTSHIFT: u16 = 54;
const KEY_LEFTALT: u16 = 56;
const KEY_RIGHTCTRL: u16 = 97;
const KEY_RIGHTALT: u16 = 100;
const KEY_LEFTMETA: u16 = 125;
const KEY_RIGHTMETA: u16 = 126;

fn modifier_code(name: &str) -> Option<u16> {
    Some(match name {
        "right_command" => KEY_RIGHTMETA,
        "left_command" => KEY_LEFTMETA,
        "right_option" => KEY_RIGHTALT,
        "left_option" => KEY_LEFTALT,
        "right_control" => KEY_RIGHTCTRL,
        "left_control" => KEY_LEFTCTRL,
        "right_shift" => KEY_RIGHTSHIFT,
        "left_shift" => KEY_LEFTSHIFT,
        _ => return None, // "fn" is handled by keyboard firmware on Linux
    })
}

/// Linux input key codes (US layout) for combo keys.
pub(crate) fn key_code(name: &str) -> Option<u16> {
    let n = name.to_lowercase();
    if n.len() == 1 {
        let c = n.chars().next()?;
        let row = |s: &str, base: u16| s.find(c).map(|i| base + i as u16);
        return row("1234567890", 2)
            .or_else(|| row("qwertyuiop", 16))
            .or_else(|| row("asdfghjkl", 30))
            .or_else(|| row("zxcvbnm", 44))
            .or_else(|| match c {
                '-' => Some(12),
                '=' => Some(13),
                '[' => Some(26),
                ']' => Some(27),
                ';' => Some(39),
                '\'' => Some(40),
                '`' => Some(41),
                '\\' => Some(43),
                ',' => Some(51),
                '.' => Some(52),
                '/' => Some(53),
                _ => None,
            });
    }
    Some(match n.as_str() {
        "space" => 57,
        "return" | "enter" => 28,
        "tab" => 15,
        "escape" | "esc" => KEY_ESC,
        "backspace" | "delete" => 14,
        f if f.starts_with('f') => {
            let k: u16 = f[1..].parse().ok()?;
            match k {
                1..=10 => 58 + k,
                11 => 87,
                12 => 88,
                13..=24 => 183 + (k - 13),
                _ => return None,
            }
        }
        _ => return None,
    })
}

#[derive(Clone)]
enum Bind {
    Modifier(u16),
    Combo { code: u16, ctrl: bool, alt: bool, shift: bool, meta: bool },
}

#[derive(Default)]
struct Mods {
    ctrl: u8,
    alt: u8,
    shift: u8,
    meta: u8,
}

struct State {
    binds: Vec<Option<Bind>>,
    held: Vec<bool>,
    sink: Option<Sender<HotkeyEvent>>,
    mods: Mods,
}

pub struct LinuxHotkeys {
    state: Arc<Mutex<State>>,
    capture_escape: Arc<AtomicBool>,
    started: AtomicBool,
    generation: Arc<AtomicU64>,
}

impl Default for LinuxHotkeys {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(State { binds: vec![], held: vec![], sink: None, mods: Mods::default() })),
            capture_escape: Arc::new(AtomicBool::new(false)),
            started: AtomicBool::new(false),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// Keyboards: event devices that report EV_KEY with letter keys.
fn keyboards() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/dev/input/by-path").or_else(|_| std::fs::read_dir("/dev/input")) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if name.ends_with("-event-kbd") || (name.starts_with("event") && is_keyboard(&p)) {
            out.push(std::fs::canonicalize(&p).unwrap_or(p));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// /sys/class/input/eventN/device/capabilities/key has bits for KEY_A etc.
fn is_keyboard(dev: &std::path::Path) -> bool {
    let Some(n) = dev.file_name().map(|n| n.to_string_lossy().to_string()) else { return false };
    let caps = std::fs::read_to_string(format!("/sys/class/input/{n}/device/capabilities/key")).unwrap_or_default();
    // KEY_A (30) .. KEY_Z live in the lowest 64-bit word; a keyboard sets many of them.
    caps.split_whitespace().last().and_then(|w| u64::from_str_radix(w, 16).ok()).is_some_and(|w| (w >> 30) & 1 == 1 && (w >> 44) & 1 == 1)
}

fn handle(st: &mut State, code: u16, value: i32, at: f64, esc: &AtomicBool) {
    let down = value != 0;
    match code {
        KEY_LEFTCTRL | KEY_RIGHTCTRL => st.mods.ctrl = st.mods.ctrl.saturating_add_signed(if value == 1 { 1 } else if value == 0 { -1 } else { 0 }),
        KEY_LEFTALT | KEY_RIGHTALT => st.mods.alt = st.mods.alt.saturating_add_signed(if value == 1 { 1 } else if value == 0 { -1 } else { 0 }),
        KEY_LEFTSHIFT | KEY_RIGHTSHIFT => st.mods.shift = st.mods.shift.saturating_add_signed(if value == 1 { 1 } else if value == 0 { -1 } else { 0 }),
        KEY_LEFTMETA | KEY_RIGHTMETA => st.mods.meta = st.mods.meta.saturating_add_signed(if value == 1 { 1 } else if value == 0 { -1 } else { 0 }),
        _ => {}
    }
    let Some(sink) = st.sink.clone() else { return };
    for slot in 0..st.binds.len() {
        let Some(b) = st.binds[slot].clone() else { continue };
        match b {
            Bind::Modifier(c) if c == code && value != 2 => {
                if down != st.held[slot] {
                    st.held[slot] = down;
                    let _ = sink.try_send(if down { HotkeyEvent::Pressed { slot: slot as u8, at } } else { HotkeyEvent::Released { slot: slot as u8, at } });
                }
            }
            // another key while a lone modifier is held = a shortcut, not dictation
            Bind::Modifier(_) if value == 1 && st.held[slot] => {
                st.held[slot] = false;
                let _ = sink.try_send(HotkeyEvent::Released { slot: slot as u8, at: -1.0 });
            }
            Bind::Combo { code: c, ctrl, alt, shift, meta } if c == code => {
                let m = &st.mods;
                let mods_ok = (m.ctrl > 0) == ctrl && (m.alt > 0) == alt && (m.shift > 0) == shift && (m.meta > 0) == meta;
                if value == 1 && mods_ok && !st.held[slot] {
                    st.held[slot] = true;
                    let _ = sink.try_send(HotkeyEvent::Pressed { slot: slot as u8, at });
                } else if value == 0 && st.held[slot] {
                    st.held[slot] = false;
                    let _ = sink.try_send(HotkeyEvent::Released { slot: slot as u8, at });
                }
            }
            _ => {}
        }
    }
    if code == KEY_ESC && value == 1 && esc.load(Ordering::Relaxed) {
        let _ = sink.try_send(HotkeyEvent::Pressed { slot: 3, at });
    }
}

impl Hotkeys for LinuxHotkeys {
    fn watch(&self, bindings: Vec<Option<Hotkey>>, sink: Sender<HotkeyEvent>) -> Result<()> {
        let binds: Vec<Option<Bind>> = bindings
            .iter()
            .map(|b| match b {
                Some(Hotkey::Modifier(m)) => modifier_code(m).map(Bind::Modifier),
                Some(Hotkey::Combo { modifiers, key }) => key_code(key).map(|code| Bind::Combo {
                    code,
                    ctrl: modifiers.iter().any(|m| m == "ctrl"),
                    alt: modifiers.iter().any(|m| m == "alt"),
                    shift: modifiers.iter().any(|m| m == "shift"),
                    meta: modifiers.iter().any(|m| m == "cmd"),
                }),
                None => None,
            })
            .collect();
        {
            let mut st = self.state.lock().unwrap();
            st.held = vec![false; binds.len()];
            st.binds = binds;
            st.sink = Some(sink);
        }
        if self.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let devs = keyboards();
        let mut opened = 0;
        let mut denied = false;
        let generation = self.generation.load(Ordering::SeqCst);
        for dev in devs {
            match std::fs::File::open(&dev) {
                Ok(mut f) => {
                    opened += 1;
                    let state = self.state.clone();
                    let esc = self.capture_escape.clone();
                    let gen_ = self.generation.clone();
                    std::thread::Builder::new().name("stenly-keys".into()).spawn(move || {
                        let mut buf = [0u8; 24];
                        while gen_.load(Ordering::SeqCst) == generation {
                            if f.read_exact(&mut buf).is_err() {
                                break; // unplugged
                            }
                            let sec = i64::from_ne_bytes(buf[0..8].try_into().unwrap());
                            let usec = i64::from_ne_bytes(buf[8..16].try_into().unwrap());
                            let typ = u16::from_ne_bytes(buf[16..18].try_into().unwrap());
                            let code = u16::from_ne_bytes(buf[18..20].try_into().unwrap());
                            let value = i32::from_ne_bytes(buf[20..24].try_into().unwrap());
                            if typ != EV_KEY {
                                continue;
                            }
                            let at = sec as f64 + usec as f64 / 1e6;
                            if let Ok(mut st) = state.lock() {
                                handle(&mut st, code, value, at, &esc);
                            }
                        }
                    })?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => denied = true,
                Err(_) => {}
            }
        }
        if opened == 0 {
            self.started.store(false, Ordering::SeqCst);
            if denied {
                bail!("can't read the keyboard: add yourself to the `input` group (sudo usermod -aG input $USER) and log in again");
            }
            bail!("no keyboard found under /dev/input");
        }
        Ok(())
    }

    fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.started.store(false, Ordering::SeqCst);
        let mut st = self.state.lock().unwrap();
        st.binds.clear();
        st.sink = None;
    }

    fn set_capture_escape(&self, on: bool) {
        self.capture_escape.store(on, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        assert_eq!(key_code("a"), Some(30));
        assert_eq!(key_code("v"), Some(47));
        assert_eq!(key_code("space"), Some(57));
        assert_eq!(key_code("f1"), Some(59));
        assert_eq!(key_code("f12"), Some(88));
        assert_eq!(key_code("1"), Some(2));
    }

    #[test]
    fn modifier_tap_and_combo() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut st = State {
            binds: vec![Some(Bind::Modifier(KEY_RIGHTMETA)), Some(Bind::Combo { code: 47, ctrl: false, alt: true, shift: false, meta: true })],
            held: vec![false, false],
            sink: Some(tx),
            mods: Mods::default(),
        };
        let esc = AtomicBool::new(false);
        handle(&mut st, KEY_RIGHTMETA, 1, 1.0, &esc);
        handle(&mut st, KEY_RIGHTMETA, 0, 1.2, &esc);
        assert!(matches!(rx.try_recv(), Ok(HotkeyEvent::Pressed { slot: 0, .. })));
        assert!(matches!(rx.try_recv(), Ok(HotkeyEvent::Released { slot: 0, .. })));
        // alt+meta+v
        handle(&mut st, KEY_LEFTALT, 1, 2.0, &esc);
        handle(&mut st, KEY_LEFTMETA, 1, 2.0, &esc);
        handle(&mut st, 47, 1, 2.1, &esc);
        // meta press also counts for slot 0? No: LEFTMETA isn't RIGHTMETA
        assert!(matches!(rx.try_recv(), Ok(HotkeyEvent::Pressed { slot: 1, .. })));
    }
}
