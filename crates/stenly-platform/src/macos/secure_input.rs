//! Read-only Secure Event Input diagnostics. Never disables protection or
//! reads keyboard events, field contents, window titles, or browser state.
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
use std::ffi::c_void;
use stenly_core::keyboard_guard::{SecureInputOwner, ShortcutBlock};

type CFRef = *const c_void;
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> bool;
}
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IORegistryGetRootEntry(main_port: u32) -> u32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: CFRef,
        allocator: CFRef,
        options: u32,
    ) -> CFRef;
    fn IOObjectRelease(object: u32) -> i32;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: CFRef);
    fn CFGetTypeID(value: CFRef) -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFArrayGetCount(array: CFRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFRef, index: isize) -> CFRef;
    fn CFDictionaryGetValue(dict: CFRef, key: CFRef) -> CFRef;
    fn CFNumberGetValue(number: CFRef, kind: isize, value: *mut c_void) -> bool;
    static kCFBooleanTrue: CFRef;
}

fn key(value: &str) -> objc2::rc::Retained<objc2_core_foundation::CFString> {
    objc2_core_foundation::CFString::from_str(value).into()
}
fn raw(value: &objc2_core_foundation::CFString) -> CFRef {
    value as *const _ as CFRef
}

/// IOConsoleUsers is undocumented, so attribution is best-effort. Only accept
/// the on-console session, a positive PID, and exactly one distinct candidate.
/// The public Carbon query remains the authority for whether a lock exists.
fn owner_pid() -> Option<i32> {
    unsafe {
        let root = IORegistryGetRootEntry(0);
        if root == 0 {
            return None;
        }
        let property = key("IOConsoleUsers");
        let users = IORegistryEntryCreateCFProperty(root, raw(&property), std::ptr::null(), 0);
        IOObjectRelease(root);
        if users.is_null() {
            return None;
        }
        let result = (|| {
            if CFGetTypeID(users) != CFArrayGetTypeID() {
                return None;
            }
            let console = key("kCGSSessionOnConsoleKey");
            let pid_key = key("kCGSSessionSecureInputPID");
            let mut candidate = None;
            for index in 0..CFArrayGetCount(users) {
                let session = CFArrayGetValueAtIndex(users, index);
                if session.is_null() || CFGetTypeID(session) != CFDictionaryGetTypeID() {
                    continue;
                }
                if CFDictionaryGetValue(session, raw(&console)) != kCFBooleanTrue {
                    continue;
                }
                let number = CFDictionaryGetValue(session, raw(&pid_key));
                if number.is_null() || CFGetTypeID(number) != CFNumberGetTypeID() {
                    continue;
                }
                let mut pid: i64 = 0;
                // kCFNumberSInt64Type = 4; reject truncation, sentinels, ambiguity.
                if !CFNumberGetValue(number, 4, &mut pid as *mut _ as *mut c_void) {
                    continue;
                }
                let Ok(pid) = i32::try_from(pid) else {
                    continue;
                };
                if pid <= 0 {
                    continue;
                }
                if candidate.is_some_and(|other| other != pid) {
                    return None;
                }
                candidate = Some(pid);
            }
            candidate
        })();
        CFRelease(users);
        result
    }
}

pub fn status() -> Option<ShortcutBlock> {
    if !unsafe { IsSecureEventInputEnabled() } {
        return None;
    }
    let owner = objc2::rc::autoreleasepool(|_| {
        let pid = owner_pid()?;
        let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
        let name = app.localizedName()?.to_string();
        if name.trim().is_empty() || app.isTerminated() {
            return None;
        }
        Some(SecureInputOwner {
            pid,
            name,
            bundle_id: app.bundleIdentifier().map(|s| s.to_string()),
        })
    });
    // The lock can disappear while resolving attribution. Never show a stale lock.
    unsafe { IsSecureEventInputEnabled() }.then_some(ShortcutBlock { owner })
}

/// Explicit user action only; revalidate PID *and* bundle before activating.
/// Never terminate the app, dismiss prompts, or disable Secure Event Input.
pub fn show_owner(expected: &SecureInputOwner) {
    let Some(current) = status().and_then(|s| s.owner) else {
        return;
    };
    if current.pid != expected.pid || current.bundle_id != expected.bundle_id {
        return;
    }
    if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(current.pid) {
        app.activateWithOptions(NSApplicationActivationOptions::empty());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn read_only_probe_handles_current_system_state() {
        // The OS state can change between calls, so never compare two samples.
        if let Some(owner) = super::status().and_then(|block| block.owner) {
            assert!(owner.pid > 0);
            assert!(!owner.name.trim().is_empty());
        }
    }
}
