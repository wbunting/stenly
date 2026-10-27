//! Which display things are on: the call app's window (so the recording pill
//! shows up next to the meeting, like Granola), the pointer, and each
//! display's global origin. Read-only; callable from any thread.
//!
//! Coordinates are CoreGraphics global points: top-left origin at the
//! primary display's top-left corner (the same space as `move_window`).

use objc2_app_kit::NSRunningApplication;
use objc2_foundation::NSString;
use std::ffi::c_void;
use stenly_core::speakers::ax::{interesting_window, is_browser};

type CFTypeRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct CGSize {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetDisplaysWithPoint(p: CGPoint, max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGGetDisplaysWithRect(r: CGRect, max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayBounds(id: u32) -> CGRect;
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFTypeRef;
    fn CGRectMakeWithDictionaryRepresentation(dict: CFTypeRef, rect: *mut CGRect) -> bool;
    static kCGWindowOwnerPID: CFTypeRef;
    static kCGWindowLayer: CFTypeRef;
    static kCGWindowBounds: CFTypeRef;
    static kCGWindowIsOnscreen: CFTypeRef;
}
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
    fn AXUIElementCopyAttributeValue(el: CFTypeRef, attr: CFTypeRef, out: *mut CFTypeRef) -> i32;
    fn AXUIElementSetMessagingTimeout(el: CFTypeRef, secs: f32) -> i32;
    fn AXValueGetValue(v: CFTypeRef, kind: u32, out: *mut c_void) -> bool;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(b: CFTypeRef) -> bool;
    fn CFArrayGetCount(a: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFTypeRef, i: isize) -> CFTypeRef;
    fn CFDictionaryGetValue(d: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFNumberGetValue(n: CFTypeRef, kind: isize, out: *mut c_void) -> bool;
}

const AX_VALUE_CGPOINT: u32 = 1;
const AX_VALUE_CGSIZE: u32 = 2;
const CF_NUMBER_SINT64: isize = 4;
/// kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements: also lists
/// windows on other Spaces (a full-screen Zoom call lives in its own Space).
const WINDOW_LIST_ALL: u32 = 1 << 4;
/// Ignore palettes, toasts and the tiny always-on-top call widgets.
const MIN_W: f64 = 240.;
const MIN_H: f64 = 160.;

/// Owned CF object, released on drop.
struct Cf(CFTypeRef);
impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

fn cfstr(s: &str) -> Cf {
    Cf(objc2::rc::Retained::into_raw(NSString::from_str(s)) as CFTypeRef)
}

unsafe fn array_items(v: CFTypeRef) -> Vec<CFTypeRef> {
    unsafe {
        if v.is_null() || CFGetTypeID(v) != CFArrayGetTypeID() {
            return Vec::new();
        }
        (0..CFArrayGetCount(v)).map(|i| CFArrayGetValueAtIndex(v, i)).collect()
    }
}

/// The display containing global point (x, y).
pub fn display_at(x: f64, y: f64) -> Option<u32> {
    let (mut id, mut n) = (0u32, 0u32);
    let ok = unsafe { CGGetDisplaysWithPoint(CGPoint { x, y }, 1, &mut id, &mut n) } == 0;
    (ok && n > 0).then_some(id)
}

/// Top-left corner of a display in global points.
pub fn display_origin(id: u32) -> Option<(f64, f64)> {
    let b = unsafe { CGDisplayBounds(id) };
    (b.size.width > 0.).then_some((b.origin.x, b.origin.y))
}

/// The display the user's pointer is on.
pub fn pointer_display() -> Option<u32> {
    let (x, y) = super::appkit_ui::mouse_location()?;
    display_at(x, y)
}

/// The display showing the call: the main window of the app with bundle id /
/// process `app_id` (as reported by mic activity, so possibly a helper such
/// as `com.google.Chrome.helper`), or of process `pid`.
pub fn app_window_display(app_id: &str, pid: Option<i32>) -> Option<u32> {
    let (bundle, pids) = owning_app(app_id, pid);
    let r = objc2::rc::autoreleasepool(|_| ax_window(&bundle, &pids).or_else(|| cg_window(&pids)))?;
    rect_display(r)
}

fn rect_display(r: CGRect) -> Option<u32> {
    display_at(r.origin.x + r.size.width / 2., r.origin.y + r.size.height / 2.).or_else(|| {
        let (mut id, mut n) = (0u32, 0u32);
        let ok = unsafe { CGGetDisplaysWithRect(r, 1, &mut id, &mut n) } == 0;
        (ok && n > 0).then_some(id)
    })
}

/// Bundle id of the app that owns the windows, and its processes. The mic is
/// often held by a helper (`<app>.helper`, a Chromium renderer...), so walk
/// the bundle id up to the running app; fall back to the pid itself.
fn owning_app(app_id: &str, pid: Option<i32>) -> (String, Vec<i32>) {
    let mut pids: Vec<i32> = Vec::new();
    let mut bundle = app_id.to_string();
    if !app_id.starts_with("pid:") && app_id.contains('.') {
        let parts: Vec<&str> = app_id.split('.').collect();
        for n in (2..=parts.len()).rev() {
            let bid = parts[..n].join(".");
            let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(&bid));
            let found: Vec<i32> = apps.iter().map(|a| a.processIdentifier()).collect();
            if !found.is_empty() {
                bundle = bid;
                pids = found;
                break;
            }
        }
    }
    if let Some(p) = pid
        && !pids.contains(&p)
    {
        pids.push(p);
    }
    (bundle, pids)
}

/// First (frontmost) usable window via Accessibility. For browsers, only the
/// window whose active tab is the meeting.
fn ax_window(bundle: &str, pids: &[i32]) -> Option<CGRect> {
    if !unsafe { AXIsProcessTrusted() } {
        return None;
    }
    let (wins_a, title_a, pos_a, size_a, min_a) =
        (cfstr("AXWindows"), cfstr("AXTitle"), cfstr("AXPosition"), cfstr("AXSize"), cfstr("AXMinimized"));
    let browser = is_browser(bundle);
    for &pid in pids {
        let app = Cf(unsafe { AXUIElementCreateApplication(pid) });
        if app.0.is_null() {
            continue;
        }
        unsafe { AXUIElementSetMessagingTimeout(app.0, 0.25) };
        let mut wins: CFTypeRef = std::ptr::null();
        if unsafe { AXUIElementCopyAttributeValue(app.0, wins_a.0, &mut wins) } != 0 || wins.is_null() {
            continue;
        }
        let wins = Cf(wins);
        for w in unsafe { array_items(wins.0) } {
            let get = |attr: &Cf| {
                let mut v: CFTypeRef = std::ptr::null();
                (unsafe { AXUIElementCopyAttributeValue(w, attr.0, &mut v) } == 0 && !v.is_null()).then(|| Cf(v))
            };
            if get(&min_a).is_some_and(|m| unsafe { CFGetTypeID(m.0) == CFBooleanGetTypeID() && CFBooleanGetValue(m.0) }) {
                continue;
            }
            if browser {
                let title = get(&title_a)
                    .filter(|t| unsafe { CFGetTypeID(t.0) == CFStringGetTypeID() })
                    .map(|t| unsafe { &*(t.0 as *const NSString) }.to_string())
                    .unwrap_or_default();
                if !interesting_window(bundle, &title) {
                    continue;
                }
            }
            let (Some(p), Some(s)) = (get(&pos_a), get(&size_a)) else { continue };
            let mut r = CGRect::default();
            let ok = unsafe {
                AXValueGetValue(p.0, AX_VALUE_CGPOINT, (&raw mut r.origin).cast())
                    && AXValueGetValue(s.0, AX_VALUE_CGSIZE, (&raw mut r.size).cast())
            };
            if ok && r.size.width >= MIN_W && r.size.height >= MIN_H {
                return Some(r);
            }
        }
    }
    None
}

/// Frontmost normal window of `pids` from the window server (no permission
/// needed for bounds). Prefers on-screen windows.
fn cg_window(pids: &[i32]) -> Option<CGRect> {
    let list = Cf(unsafe { CGWindowListCopyWindowInfo(WINDOW_LIST_ALL, 0) });
    let num = |d: CFTypeRef, key: CFTypeRef| -> Option<i64> {
        let v = unsafe { CFDictionaryGetValue(d, key) };
        let mut out = 0i64;
        (!v.is_null() && unsafe { CFNumberGetValue(v, CF_NUMBER_SINT64, (&raw mut out).cast()) }).then_some(out)
    };
    let mut offscreen = None;
    for d in unsafe { array_items(list.0) } {
        let Some(pid) = num(d, unsafe { kCGWindowOwnerPID }) else { continue };
        if !pids.contains(&(pid as i32)) || num(d, unsafe { kCGWindowLayer }) != Some(0) {
            continue;
        }
        let b = unsafe { CFDictionaryGetValue(d, kCGWindowBounds) };
        let mut r = CGRect::default();
        if b.is_null() || !unsafe { CGRectMakeWithDictionaryRepresentation(b, &mut r) } || r.size.width < MIN_W || r.size.height < MIN_H {
            continue;
        }
        let on = unsafe { CFDictionaryGetValue(d, kCGWindowIsOnscreen) };
        if !on.is_null() && unsafe { CFGetTypeID(on) == CFBooleanGetTypeID() && CFBooleanGetValue(on) } {
            return Some(r);
        }
        offscreen.get_or_insert(r);
    }
    offscreen
}
