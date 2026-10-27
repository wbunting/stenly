//! Global shortcuts via a CoreGraphics event tap.
//!
//! One tap on its own thread watches key down/up and modifier changes for the
//! whole session. Modifier-only bindings (Right ⌘, Fn, …) are matched by the
//! physical key code on `flagsChanged`, the way VoiceInk does it; combos by
//! key code + exact modifier set on `keyDown`/`keyUp`. Events carry their own
//! timestamps so tap-vs-hold doesn't depend on how fast our thread runs.
//!
//! The tap is *active* (not listen-only) so a combo's keystroke and Esc while
//! dictating can be swallowed instead of reaching the frontmost app. Needs
//! Accessibility (or Input Monitoring) permission; without it creation fails
//! and `watch` returns an error the app turns into a prompt.

use anyhow::{Result, bail};
use crossbeam_channel::Sender;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use stenly_core::platform::{Hotkey, HotkeyEvent, Hotkeys};

type CFMachPortRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type TapCallback = extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(tap: u32, place: u32, options: u32, mask: u64, cb: TapCallback, info: *mut c_void) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventGetTimestamp(event: CGEventRef) -> u64;
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFMachPortCreateRunLoopSource(alloc: *const c_void, port: CFMachPortRef, order: isize) -> CFRunLoopSourceRef;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: *const c_void);
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: CFRunLoopRef);
    fn CFRelease(cf: *const c_void);
    static kCFRunLoopCommonModes: *const c_void;
}

const KEY_DOWN: u32 = 10;
const KEY_UP: u32 = 11;
const FLAGS_CHANGED: u32 = 12;
const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const TAP_DISABLED_BY_USER: u32 = 0xFFFF_FFFF;
const FIELD_KEYCODE: u32 = 9;
const FIELD_AUTOREPEAT: u32 = 8;

const F_SHIFT: u64 = 0x0002_0000;
const F_CTRL: u64 = 0x0004_0000;
const F_ALT: u64 = 0x0008_0000;
const F_CMD: u64 = 0x0010_0000;
const F_FN: u64 = 0x0080_0000;
/// Device-dependent (left/right) bits from IOLLEvent.h.
const NX_DEVICELCTLKEYMASK: u64 = 0x0000_0001;
const NX_DEVICELSHIFTKEYMASK: u64 = 0x0000_0002;
const NX_DEVICERSHIFTKEYMASK: u64 = 0x0000_0004;
const NX_DEVICELCMDKEYMASK: u64 = 0x0000_0008;
const NX_DEVICERCMDKEYMASK: u64 = 0x0000_0010;
const NX_DEVICELALTKEYMASK: u64 = 0x0000_0020;
const NX_DEVICERALTKEYMASK: u64 = 0x0000_0040;
const NX_DEVICERCTLKEYMASK: u64 = 0x0000_2000;

const ESCAPE: i64 = 0x35;

/// (key code, device bit) of each modifier-only binding.
fn modifier_key(name: &str) -> Option<(i64, u64)> {
    Some(match name {
        "right_command" => (0x36, NX_DEVICERCMDKEYMASK),
        "left_command" => (0x37, NX_DEVICELCMDKEYMASK),
        "right_option" => (0x3D, NX_DEVICERALTKEYMASK),
        "left_option" => (0x3A, NX_DEVICELALTKEYMASK),
        "right_control" => (0x3E, NX_DEVICERCTLKEYMASK),
        "left_control" => (0x3B, NX_DEVICELCTLKEYMASK),
        "right_shift" => (0x3C, NX_DEVICERSHIFTKEYMASK),
        "left_shift" => (0x38, NX_DEVICELSHIFTKEYMASK),
        "fn" => (0x3F, F_FN),
        _ => return None,
    })
}

/// US-layout virtual key codes for combo keys.
pub(crate) fn key_code(name: &str) -> Option<i64> {
    let n = name.to_lowercase();
    let letters = "asdfhgzxcv_bqweryt123465=97-80]ou[ip_lj'k;\\,/nm.";
    if n.len() == 1 {
        let c = n.chars().next()?;
        if c == '_' {
            return None;
        }
        return letters.find(c).map(|i| i as i64).or(match c {
            '`' => Some(0x32),
            _ => None,
        });
    }
    Some(match n.as_str() {
        "space" => 0x31,
        "return" | "enter" => 0x24,
        "tab" => 0x30,
        "escape" | "esc" => ESCAPE,
        "delete" | "backspace" => 0x33,
        "f1" => 0x7A,
        "f2" => 0x78,
        "f3" => 0x63,
        "f4" => 0x76,
        "f5" => 0x60,
        "f6" => 0x61,
        "f7" => 0x62,
        "f8" => 0x64,
        "f9" => 0x65,
        "f10" => 0x6D,
        "f11" => 0x67,
        "f12" => 0x6F,
        "f13" => 0x69,
        "f14" => 0x6B,
        "f15" => 0x71,
        "f16" => 0x6A,
        "f17" => 0x40,
        "f18" => 0x4F,
        "f19" => 0x50,
        "f20" => 0x5A,
        _ => return None,
    })
}

fn combo_mask(mods: &[String]) -> u64 {
    mods.iter().fold(0, |m, x| {
        m | match x.as_str() {
            "cmd" => F_CMD,
            "ctrl" => F_CTRL,
            "alt" => F_ALT,
            "shift" => F_SHIFT,
            _ => 0,
        }
    })
}

#[derive(Clone)]
enum Bind {
    Modifier { code: i64, bit: u64 },
    Combo { code: i64, mask: u64 },
}

struct Shared {
    binds: Vec<Option<Bind>>,
    /// Slot currently held (so its release is matched, and repeats ignored).
    held: Vec<bool>,
    sink: Option<Sender<HotkeyEvent>>,
    port: CFMachPortRef,
}

unsafe impl Send for Shared {}

struct Ctx {
    shared: Arc<Mutex<Shared>>,
    capture_escape: Arc<AtomicBool>,
}

/// Seconds since boot, from the event's own timestamp (ns).
fn secs(event: CGEventRef) -> f64 {
    unsafe { CGEventGetTimestamp(event) as f64 / 1e9 }
}

extern "C" fn callback(_proxy: CGEventTapProxy, kind: u32, event: CGEventRef, info: *mut c_void) -> CGEventRef {
    let ctx = unsafe { &*(info as *const Ctx) };
    let Ok(mut sh) = ctx.shared.lock() else { return event };
    if kind == TAP_DISABLED_BY_TIMEOUT || kind == TAP_DISABLED_BY_USER {
        // macOS turns a tap off if a callback stalls; turn it straight back on.
        if !sh.port.is_null() {
            unsafe { CGEventTapEnable(sh.port, true) };
        }
        tracing::warn!("hotkey tap was disabled by the system; re-enabled");
        return event;
    }
    let Some(sink) = sh.sink.clone() else { return event };
    let code = unsafe { CGEventGetIntegerValueField(event, FIELD_KEYCODE) };
    let flags = unsafe { CGEventGetFlags(event) };
    let at = secs(event);
    let mut swallow = false;
    for slot in 0..sh.binds.len() {
        let Some(b) = sh.binds[slot].clone() else { continue };
        match (b, kind) {
            (Bind::Modifier { code: c, bit }, FLAGS_CHANGED) if c == code => {
                let down = flags & bit != 0;
                if down != sh.held[slot] {
                    sh.held[slot] = down;
                    let ev = if down { HotkeyEvent::Pressed { slot: slot as u8, at } } else { HotkeyEvent::Released { slot: slot as u8, at } };
                    let _ = sink.try_send(ev);
                }
            }
            // Another key pressed while a modifier-only binding is held means the
            // user is typing a shortcut (Right ⌘+C), not dictating: cancel it.
            (Bind::Modifier { .. }, KEY_DOWN) if sh.held[slot] => {
                sh.held[slot] = false;
                let _ = sink.try_send(HotkeyEvent::Released { slot: slot as u8, at: -1.0 });
            }
            (Bind::Combo { code: c, mask }, KEY_DOWN) if c == code && (flags & (F_CMD | F_CTRL | F_ALT | F_SHIFT)) == mask => {
                swallow = true;
                let repeat = unsafe { CGEventGetIntegerValueField(event, FIELD_AUTOREPEAT) } != 0;
                if !repeat && !sh.held[slot] {
                    sh.held[slot] = true;
                    let _ = sink.try_send(HotkeyEvent::Pressed { slot: slot as u8, at });
                }
            }
            (Bind::Combo { code: c, .. }, KEY_UP) if c == code && sh.held[slot] => {
                swallow = true;
                sh.held[slot] = false;
                let _ = sink.try_send(HotkeyEvent::Released { slot: slot as u8, at });
            }
            _ => {}
        }
    }
    // Esc cancels a running dictation (reported as slot 3).
    if code == ESCAPE && ctx.capture_escape.load(Ordering::Relaxed) && (kind == KEY_DOWN || kind == KEY_UP) {
        if kind == KEY_DOWN {
            let _ = sink.try_send(HotkeyEvent::Pressed { slot: 3, at });
        }
        swallow = true;
    }
    if swallow { std::ptr::null_mut() } else { event }
}

pub struct MacHotkeys {
    shared: Arc<Mutex<Shared>>,
    capture_escape: Arc<AtomicBool>,
    started: Mutex<Option<usize>>, // run loop pointer, as usize
}

impl Default for MacHotkeys {
    fn default() -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared { binds: vec![], held: vec![], sink: None, port: std::ptr::null_mut() })),
            capture_escape: Arc::new(AtomicBool::new(false)),
            started: Mutex::new(None),
        }
    }
}

/// Whether we may listen to keyboard events (Accessibility or Input Monitoring).
pub fn has_access() -> bool {
    let listen = unsafe { CGPreflightListenEventAccess() };
    listen || crate::macos::shell::ax_trusted()
}

pub fn request_access() {
    unsafe {
        CGRequestListenEventAccess();
    }
}

impl MacHotkeys {
    fn ensure_tap(&self) -> Result<()> {
        let mut started = self.started.lock().unwrap();
        if started.is_some() {
            return Ok(());
        }
        let ctx = Box::into_raw(Box::new(Ctx { shared: self.shared.clone(), capture_escape: self.capture_escape.clone() })) as usize;
        let shared = self.shared.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Result<usize, String>>();
        std::thread::Builder::new().name("stenly-hotkeys".into()).spawn(move || unsafe {
            let mask: u64 = (1 << KEY_DOWN) | (1 << KEY_UP) | (1 << FLAGS_CHANGED);
            // kCGSessionEventTap, head insert, default (active) tap.
            let port = CGEventTapCreate(1, 0, 0, mask, callback, ctx as *mut c_void);
            if port.is_null() {
                drop(Box::from_raw(ctx as *mut Ctx));
                let _ = tx.send(Err("macOS refused the keyboard shortcut listener (Accessibility permission needed)".into()));
                return;
            }
            shared.lock().unwrap().port = port;
            let source = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
            let rl = CFRunLoopGetCurrent();
            CFRunLoopAddSource(rl, source, kCFRunLoopCommonModes);
            CGEventTapEnable(port, true);
            let _ = tx.send(Ok(rl as usize));
            CFRunLoopRun();
            CFRelease(source);
            CFRelease(port);
            shared.lock().unwrap().port = std::ptr::null_mut();
            drop(Box::from_raw(ctx as *mut Ctx));
        })?;
        match rx.recv() {
            Ok(Ok(rl)) => {
                *started = Some(rl);
                Ok(())
            }
            Ok(Err(e)) => bail!(e),
            Err(_) => bail!("hotkey thread exited"),
        }
    }
}

impl Hotkeys for MacHotkeys {
    fn watch(&self, bindings: Vec<Option<Hotkey>>, sink: Sender<HotkeyEvent>) -> Result<()> {
        let binds: Vec<Option<Bind>> = bindings
            .iter()
            .map(|b| match b {
                Some(Hotkey::Modifier(m)) => modifier_key(m).map(|(code, bit)| Bind::Modifier { code, bit }),
                Some(Hotkey::Combo { modifiers, key }) => key_code(key).map(|code| Bind::Combo { code, mask: combo_mask(modifiers) }),
                None => None,
            })
            .collect();
        {
            let mut sh = self.shared.lock().unwrap();
            sh.held = vec![false; binds.len()];
            sh.binds = binds;
            sh.sink = Some(sink);
        }
        self.ensure_tap()
    }

    fn stop(&self) {
        let mut sh = self.shared.lock().unwrap();
        sh.binds.clear();
        sh.sink = None;
        drop(sh);
        if let Some(rl) = self.started.lock().unwrap().take() {
            unsafe { CFRunLoopStop(rl as CFRunLoopRef) };
        }
    }

    fn set_capture_escape(&self, on: bool) {
        self.capture_escape.store(on, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_codes() {
        assert_eq!(key_code("a"), Some(0x00));
        assert_eq!(key_code("v"), Some(0x09));
        assert_eq!(key_code("space"), Some(0x31));
        assert_eq!(key_code("m"), Some(0x2E));
        assert_eq!(key_code("1"), Some(0x12));
        assert_eq!(key_code("f5"), Some(0x60));
        assert_eq!(key_code("_"), None);
    }
}

/// Dev/test: post a physical-looking Right ⌘ press/release through the HID
/// event stream (needs Accessibility), to exercise the real tap end to end.
pub fn post_right_command(down: bool) {
    type CGEventSourceRef = *mut c_void;
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceCreate(state: i32) -> CGEventSourceRef;
        fn CGEventCreateKeyboardEvent(source: CGEventSourceRef, key: u16, down: bool) -> CGEventRef;
        fn CGEventSetFlags(event: CGEventRef, flags: u64);
        fn CGEventSetType(event: CGEventRef, kind: u32);
        fn CGEventPost(tap: u32, event: CGEventRef);
    }
    unsafe {
        let src = CGEventSourceCreate(1);
        let e = CGEventCreateKeyboardEvent(src, 0x36, down);
        if e.is_null() {
            return;
        }
        CGEventSetType(e, FLAGS_CHANGED);
        CGEventSetFlags(e, if down { F_CMD | NX_DEVICERCMDKEYMASK | 0x100 } else { 0x100 });
        CGEventPost(0, e);
        CFRelease(e);
        if !src.is_null() {
            CFRelease(src);
        }
    }
}

/// Dev/test: post a key down+up (virtual key code), optionally with ⌘ held.
pub fn post_key(code: u16, cmd: bool) {
    type CGEventSourceRef = *mut c_void;
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceCreate(state: i32) -> CGEventSourceRef;
        fn CGEventCreateKeyboardEvent(source: CGEventSourceRef, key: u16, down: bool) -> CGEventRef;
        fn CGEventSetFlags(event: CGEventRef, flags: u64);
        fn CGEventPost(tap: u32, event: CGEventRef);
    }
    unsafe {
        let src = CGEventSourceCreate(1);
        for down in [true, false] {
            let e = CGEventCreateKeyboardEvent(src, code, down);
            if e.is_null() {
                continue;
            }
            if cmd {
                CGEventSetFlags(e, F_CMD | NX_DEVICERCMDKEYMASK | 0x100);
            }
            CGEventPost(0, e);
            CFRelease(e);
        }
        if !src.is_null() {
            CFRelease(src);
        }
    }
}
