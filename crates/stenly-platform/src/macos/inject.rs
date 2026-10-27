//! Typing dictated text into the frontmost app: put it on the pasteboard
//! (marked transient so clipboard managers skip it), post ⌘V, then put the
//! user's previous clipboard back — every type of every item — after a delay.

use anyhow::{Result, bail};
use objc2::rc::Retained;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSWorkspace};
use objc2_foundation::{NSArray, NSData, NSString};
use std::ffi::c_void;
use std::time::Duration;
use stenly_core::platform::TextInjector;

type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceCreate(state: i32) -> CGEventSourceRef;
    fn CGEventCreateKeyboardEvent(source: CGEventSourceRef, key: u16, down: bool) -> CGEventRef;
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CGEventSourceFlagsState(state: i32) -> u64;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

const KEY_V: u16 = 0x09;
const KEY_CMD: u16 = 0x37;
const KEY_RETURN: u16 = 0x24;
const FLAG_CMD: u64 = 0x0010_0000;
/// kCGEventSourceStateHIDSystemState
const HID_STATE: i32 = 1;
/// kCGHIDEventTap
const HID_TAP: u32 = 0;

/// Wait (up to 1.5 s) until the user has let go of ⌘ ⌥ ⌃ ⇧: a paste posted
/// while e.g. Right ⌥ is still physically down arrives as ⌥⌘V, which apps
/// ignore or treat as something else.
fn wait_for_modifiers_released() {
    const MODS: u64 = 0x0002_0000 | 0x0004_0000 | 0x0008_0000 | 0x0010_0000;
    let t0 = std::time::Instant::now();
    while unsafe { CGEventSourceFlagsState(HID_STATE) } & MODS != 0 && t0.elapsed() < Duration::from_millis(1500) {
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Post a key stroke (down + up), with ⌘ held when `cmd`.
fn key_stroke(key: u16, cmd: bool) -> Result<()> {
    unsafe {
        let src = CGEventSourceCreate(HID_STATE);
        let make = |k: u16, down: bool| {
            let e = CGEventCreateKeyboardEvent(src, k, down);
            if !e.is_null() && cmd {
                CGEventSetFlags(e, FLAG_CMD);
            }
            e
        };
        let mut evs: Vec<CGEventRef> = Vec::new();
        if cmd {
            evs.push(make(KEY_CMD, true));
        }
        evs.push(make(key, true));
        evs.push(make(key, false));
        if cmd {
            let up = CGEventCreateKeyboardEvent(src, KEY_CMD, false);
            evs.push(up);
        }
        if evs.iter().any(|e| e.is_null()) {
            for e in evs.into_iter().filter(|e| !e.is_null()) {
                CFRelease(e);
            }
            if !src.is_null() {
                CFRelease(src);
            }
            bail!("couldn't create keyboard events");
        }
        for e in evs {
            CGEventPost(HID_TAP, e);
            CFRelease(e);
        }
        if !src.is_null() {
            CFRelease(src);
        }
    }
    Ok(())
}

/// Every type of every pasteboard item, so the restore is lossless (images,
/// rich text, file references).
struct Snapshot(Vec<Vec<(Retained<NSString>, Retained<NSData>)>>);
unsafe impl Send for Snapshot {}

fn snapshot(pb: &NSPasteboard) -> Snapshot {
    let mut items = Vec::new();
    if let Some(list) = pb.pasteboardItems() {
        for item in list.iter() {
            let mut kinds = Vec::new();
            for t in item.types().iter() {
                if let Some(d) = item.dataForType(&t) {
                    kinds.push((t.clone(), d));
                }
            }
            items.push(kinds);
        }
    }
    Snapshot(items)
}

fn restore(pb: &NSPasteboard, snap: Snapshot) {
    pb.clearContents();
    if snap.0.is_empty() {
        return;
    }
    let items: Vec<Retained<NSPasteboardItem>> = snap
        .0
        .into_iter()
        .map(|kinds| {
            let it = NSPasteboardItem::new();
            for (t, d) in kinds {
                it.setData_forType(&d, &t);
            }
            it
        })
        .collect();
    let refs: Vec<&objc2::runtime::ProtocolObject<dyn objc2_app_kit::NSPasteboardWriting>> =
        items.iter().map(|i| objc2::runtime::ProtocolObject::from_ref(&**i)).collect();
    let arr = NSArray::from_slice(&refs);
    pb.writeObjects(&arr);
}

fn set_text(pb: &NSPasteboard, text: &str, transient: bool) {
    pb.clearContents();
    unsafe {
        pb.setString_forType(&NSString::from_str(text), NSPasteboardTypeString);
    }
    // nspasteboard.org conventions: who wrote it, and "don't keep this in history".
    pb.setString_forType(&NSString::from_str("ai.stenly.app"), &NSString::from_str("org.nspasteboard.source"));
    if transient {
        pb.setData_forType(Some(&NSData::new()), &NSString::from_str("org.nspasteboard.TransientType"));
    }
}

/// NSPasteboard is thread-safe and CGEventPost needs no main thread, so the
/// injector runs on whatever background thread calls it. (Hopping to the main
/// queue synchronously deadlocked while the UI thread awaited the paste.)
fn on_main<T>(f: impl FnOnce() -> T) -> T {
    objc2::rc::autoreleasepool(|_| f())
}

pub struct MacInjector;

impl TextInjector for MacInjector {
    fn paste(&self, text: &str, restore_after: Option<Duration>) -> Result<()> {
        if !crate::macos::shell::ax_trusted() {
            // Still useful: leave the text on the clipboard for a manual ⌘V.
            set_text(&NSPasteboard::generalPasteboard(), text, false);
            bail!("Stenly needs Accessibility permission to type into other apps");
        }
        let text = text.to_string();
        let snap = on_main(move || {
            let pb = NSPasteboard::generalPasteboard();
            let snap = restore_after.map(|_| snapshot(&pb));
            set_text(&pb, &text, restore_after.is_some());
            snap
        });
        // Give the pasteboard a moment to settle before the target app reads it.
        std::thread::sleep(Duration::from_millis(40));
        wait_for_modifiers_released();
        key_stroke(KEY_V, true)?;
        if let (Some(delay), Some(snap)) = (restore_after, snap) {
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                on_main(move || restore(&NSPasteboard::generalPasteboard(), snap));
            });
        }
        Ok(())
    }

    fn press_return(&self) -> Result<()> {
        wait_for_modifiers_released();
        key_stroke(KEY_RETURN, false)
    }

    fn copy(&self, text: &str) -> Result<()> {
        let text = text.to_string();
        on_main(move || set_text(&NSPasteboard::generalPasteboard(), &text, false));
        Ok(())
    }

    fn selected_text(&self) -> Option<String> {
        ax_selected_text()
    }

    fn frontmost_app(&self) -> Option<String> {
        on_main(|| {
            let ws = NSWorkspace::sharedWorkspace();
            ws.frontmostApplication().and_then(|a| a.localizedName()).map(|n| n.to_string())
        })
    }
}

/// kAXFocusedUIElement → kAXSelectedText via the Accessibility API.
fn ax_selected_text() -> Option<String> {
    use std::ffi::c_void;
    type CFTypeRef = *const c_void;
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXUIElementCreateSystemWide() -> CFTypeRef;
        fn AXUIElementCopyAttributeValue(el: CFTypeRef, attr: CFTypeRef, out: *mut CFTypeRef) -> i32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: CFTypeRef);
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFStringGetTypeID() -> usize;
    }
    if !crate::macos::shell::ax_trusted() {
        return None;
    }
    unsafe {
        let sys = AXUIElementCreateSystemWide();
        if sys.is_null() {
            return None;
        }
        let attr = |name: &str| objc2::rc::Retained::into_raw(NSString::from_str(name)) as CFTypeRef;
        let focused_attr = attr("AXFocusedUIElement");
        let mut focused: CFTypeRef = std::ptr::null();
        let ok = AXUIElementCopyAttributeValue(sys, focused_attr, &mut focused) == 0 && !focused.is_null();
        CFRelease(focused_attr);
        CFRelease(sys);
        if !ok {
            return None;
        }
        let sel_attr = attr("AXSelectedText");
        let mut sel: CFTypeRef = std::ptr::null();
        let got = AXUIElementCopyAttributeValue(focused, sel_attr, &mut sel) == 0 && !sel.is_null();
        CFRelease(sel_attr);
        CFRelease(focused);
        if !got {
            return None;
        }
        let out = if CFGetTypeID(sel) == CFStringGetTypeID() {
            let ns: &NSString = &*(sel as *const NSString);
            Some(ns.to_string())
        } else {
            None
        };
        CFRelease(sel);
        out.map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s.len() < 20_000)
    }
}
