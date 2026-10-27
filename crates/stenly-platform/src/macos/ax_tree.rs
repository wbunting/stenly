//! Read-only snapshots of meeting apps' accessibility trees, for telling who
//! is talking (see `stenly_core::speakers::ax`). Needs the Accessibility
//! permission Stenly already has for dictation. Never clicks, focuses or
//! changes anything in the meeting app; the one attribute it sets,
//! `AXManualAccessibility`, asks Chromium/Electron to expose their web
//! content to assistive apps (what VoiceOver does).

use objc2_app_kit::NSRunningApplication;
use objc2_foundation::NSString;
use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use stenly_core::speakers::ax::{AxNode, AxWindow, MeetingAxSource, interesting_window, is_browser, meeting_app_ids};

type CFTypeRef = *const c_void;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
    fn AXUIElementCopyAttributeValue(el: CFTypeRef, attr: CFTypeRef, out: *mut CFTypeRef) -> i32;
    fn AXUIElementCopyMultipleAttributeValues(el: CFTypeRef, attrs: CFTypeRef, options: u32, out: *mut CFTypeRef) -> i32;
    fn AXUIElementSetAttributeValue(el: CFTypeRef, attr: CFTypeRef, value: CFTypeRef) -> i32;
    fn AXUIElementSetMessagingTimeout(el: CFTypeRef, secs: f32) -> i32;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(a: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFTypeRef, i: isize) -> CFTypeRef;
    fn CFArrayCreate(alloc: CFTypeRef, values: *const CFTypeRef, n: isize, callbacks: *const c_void) -> CFTypeRef;
    static kCFTypeArrayCallBacks: c_void;
    static kCFBooleanTrue: CFTypeRef;
}

/// Attributes read per element, in one round trip.
const ATTRS: [&str; 7] = ["AXRole", "AXTitle", "AXDescription", "AXValue", "AXDOMIdentifier", "AXDOMClassList", "AXChildren"];
const MAX_DEPTH: usize = 80;
const MAX_NODES: usize = 25_000;
/// Per-window walk budget, so a huge page can't stall the poller.
const BUDGET: Duration = Duration::from_millis(400);

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

unsafe fn to_string(v: CFTypeRef) -> Option<String> {
    unsafe {
        if v.is_null() || CFGetTypeID(v) != CFStringGetTypeID() {
            return None;
        }
        let ns: &NSString = &*(v as *const NSString);
        let s = ns.to_string();
        let s = s.trim();
        (!s.is_empty()).then(|| s.chars().take(300).collect())
    }
}

/// Borrowed items of a CFArray (valid while the array lives).
unsafe fn array_items(v: CFTypeRef) -> Vec<CFTypeRef> {
    unsafe {
        if v.is_null() || CFGetTypeID(v) != CFArrayGetTypeID() {
            return Vec::new();
        }
        (0..CFArrayGetCount(v)).map(|i| CFArrayGetValueAtIndex(v, i)).collect()
    }
}

struct Walker {
    attrs: Cf,
    nodes: usize,
    deadline: Instant,
    /// For browsers: skip the window chrome (toolbars, tab strip) and only
    /// keep branches that lead to web content.
    web_only: bool,
}

impl Walker {
    fn new(web_only: bool) -> Self {
        let names: Vec<Cf> = ATTRS.iter().map(|a| cfstr(a)).collect();
        let raw: Vec<CFTypeRef> = names.iter().map(|c| c.0).collect();
        let attrs = Cf(unsafe { CFArrayCreate(std::ptr::null(), raw.as_ptr(), raw.len() as isize, &raw const kCFTypeArrayCallBacks) });
        Self { attrs, nodes: 0, deadline: Instant::now() + BUDGET, web_only }
    }

    fn walk(&mut self, el: CFTypeRef, depth: usize, in_web: bool) -> Option<AxNode> {
        if self.nodes >= MAX_NODES || Instant::now() > self.deadline {
            return None;
        }
        self.nodes += 1;
        let mut out: CFTypeRef = std::ptr::null();
        // Missing attributes come back as AXValue error objects (not strings), which we skip.
        if unsafe { AXUIElementCopyMultipleAttributeValues(el, self.attrs.0, 0, &mut out) } != 0 || out.is_null() {
            return None;
        }
        let vals = Cf(out);
        let v = unsafe { array_items(vals.0) };
        let get = |i: usize| v.get(i).copied().unwrap_or(std::ptr::null());
        let mut n = AxNode {
            role: unsafe { to_string(get(0)) },
            title: unsafe { to_string(get(1)) },
            desc: unsafe { to_string(get(2)) },
            value: unsafe { to_string(get(3)) },
            dom_id: unsafe { to_string(get(4)) },
            classes: unsafe { array_items(get(5)) }.into_iter().filter_map(|c| unsafe { to_string(c) }).collect(),
            children: Vec::new(),
        };
        let in_web = in_web || n.role.as_deref() == Some("AXWebArea");
        if depth < MAX_DEPTH {
            for child in unsafe { array_items(get(6)) } {
                if let Some(c) = self.walk(child, depth + 1, in_web) {
                    if self.web_only && !in_web && !c.any(&|x| x.role.as_deref() == Some("AXWebArea")) {
                        continue;
                    }
                    n.children.push(c);
                }
            }
        }
        // Text fields can hold whole documents; we only need labels.
        if n.role.as_deref().is_some_and(|r| r == "AXTextArea" || r == "AXTextField") {
            n.value = None;
        }
        Some(n)
    }
}

pub struct MacMeetingAx {
    /// Processes already asked to expose their web content.
    enabled: Mutex<HashSet<i32>>,
}

impl MacMeetingAx {
    pub fn new() -> Self {
        Self { enabled: Mutex::new(HashSet::new()) }
    }

    fn app_windows(&self, bundle: &str, pid: i32, out: &mut Vec<AxWindow>) {
        let app = Cf(unsafe { AXUIElementCreateApplication(pid) });
        if app.0.is_null() {
            return;
        }
        unsafe { AXUIElementSetMessagingTimeout(app.0, 0.25) };
        let browser = is_browser(bundle);
        let electron = matches!(bundle, "com.microsoft.teams2" | "com.microsoft.teams" | "com.tinyspeck.slackmacgap");
        if (browser || electron) && self.enabled.lock().is_ok_and(|mut s| s.insert(pid)) {
            let attr = cfstr("AXManualAccessibility");
            unsafe { AXUIElementSetAttributeValue(app.0, attr.0, kCFBooleanTrue) };
        }
        let attr = cfstr("AXWindows");
        let mut wins: CFTypeRef = std::ptr::null();
        if unsafe { AXUIElementCopyAttributeValue(app.0, attr.0, &mut wins) } != 0 || wins.is_null() {
            return;
        }
        let wins = Cf(wins);
        let title_attr = cfstr("AXTitle");
        for w in unsafe { array_items(wins.0) } {
            let mut t: CFTypeRef = std::ptr::null();
            let title = if unsafe { AXUIElementCopyAttributeValue(w, title_attr.0, &mut t) } == 0 {
                let t = Cf(t);
                unsafe { to_string(t.0) }.unwrap_or_default()
            } else {
                String::new()
            };
            if !interesting_window(bundle, &title) {
                continue;
            }
            let mut walker = Walker::new(browser);
            if let Some(root) = walker.walk(w, 0, false) {
                out.push(AxWindow { app: bundle.to_string(), title, root });
            }
        }
    }
}

impl Default for MacMeetingAx {
    fn default() -> Self {
        Self::new()
    }
}

impl MeetingAxSource for MacMeetingAx {
    fn snapshot(&self) -> Vec<AxWindow> {
        if !crate::macos::shell::ax_trusted() {
            return Vec::new();
        }
        let mut out = Vec::new();
        objc2::rc::autoreleasepool(|_| {
            for bundle in meeting_app_ids() {
                let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle));
                for app in apps.iter() {
                    self.app_windows(bundle, app.processIdentifier(), &mut out);
                }
            }
        });
        out
    }
}
