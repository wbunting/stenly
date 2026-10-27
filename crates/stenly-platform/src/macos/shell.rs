//! AppKit bits gpui doesn't expose: accessory activation policy (no Dock icon),
//! `NSWindowSharingNone` so popups don't show in screen shares, permission
//! status/requests, and launch-at-login via SMAppService.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSWindow, NSWindowSharingType, NSWorkspace};
use objc2_foundation::{NSString, NSURL};
use stenly_core::platform::{DesktopShell, Permission, PermissionState};

pub struct MacShell;

fn open_url(url: &str) {
    if let Some(u) = NSURL::URLWithString(&NSString::from_str(url)) {
        NSWorkspace::sharedWorkspace().openURL(&u);
    }
}

fn mic_state() -> PermissionState {
    use cidre::av;
    match av::CaptureDevice::authorization_status_for_media_type(av::MediaType::audio()) {
        Ok(av::AuthorizationStatus::Authorized) => PermissionState::Granted,
        Ok(av::AuthorizationStatus::Denied | av::AuthorizationStatus::Restricted) => PermissionState::Denied,
        _ => PermissionState::NotDetermined,
    }
}

// TCC's preflight check (private but stable since 10.14; the same call Apple's
// own apps and every "system audio recorder" use, since AudioCapture has no
// public status API). 0 = granted, 1 = denied, 2 = not determined.
// Private framework: resolved at runtime so a future macOS without it degrades
// gracefully instead of failing to launch.
type PreflightFn = unsafe extern "C" fn(*const std::ffi::c_void, *const std::ffi::c_void) -> i32;

unsafe extern "C" {
    fn dlopen(path: *const std::ffi::c_char, mode: i32) -> *mut std::ffi::c_void;
    fn dlsym(handle: *mut std::ffi::c_void, symbol: *const std::ffi::c_char) -> *mut std::ffi::c_void;
}

fn preflight_fn() -> Option<PreflightFn> {
    static F: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    let addr = *F.get_or_init(|| unsafe {
        let h = dlopen(c"/System/Library/PrivateFrameworks/TCC.framework/TCC".as_ptr(), 1 /* RTLD_LAZY */);
        if h.is_null() {
            tracing::warn!("TCC.framework unavailable; system audio status will be inferred");
            return None;
        }
        let f = dlsym(h, c"TCCAccessPreflight".as_ptr());
        (!f.is_null()).then_some(f as usize)
    });
    addr.map(|a| unsafe { std::mem::transmute::<usize, PreflightFn>(a) })
}

fn tcc_preflight(service: &str) -> Option<PermissionState> {
    use objc2_core_foundation::CFString;
    let f = preflight_fn()?;
    let s = CFString::from_str(service);
    let r = unsafe { f(&*s as *const CFString as *const _, std::ptr::null()) };
    static LAST: std::sync::Mutex<Option<i32>> = std::sync::Mutex::new(None);
    if let Ok(mut last) = LAST.lock() {
        if *last != Some(r) {
            tracing::info!(service, result = r, "TCC preflight changed");
            *last = Some(r);
        }
    }
    match r {
        0 => Some(PermissionState::Granted),
        1 => Some(PermissionState::Denied),
        2 => Some(PermissionState::NotDetermined),
        _ => None,
    }
}

/// System audio has no reliable status API: TCC's preflight answers "granted"
/// for AudioCapture regardless (observed on macOS 26 right after a reset), and
/// a denied tap simply delivers digital silence. So the truth is what we hear:
/// granted once any tap has delivered non-silent audio.
fn system_audio_state() -> PermissionState {
    if crate::macos::tap::verified() {
        return PermissionState::Granted;
    }
    // Still useful for the explicit "denied" case.
    match tcc_preflight("kTCCServiceAudioCapture") {
        Some(PermissionState::Denied) => PermissionState::Denied,
        _ => PermissionState::NotDetermined,
    }
}

impl DesktopShell for MacShell {
    fn set_background_app(&self) {
        let Some(mtm) = MainThreadMarker::new() else {
            tracing::warn!("set_background_app called off the main thread");
            return;
        };
        NSApplication::sharedApplication(mtm).setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    }

    fn set_foreground_app(&self) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        NSApplication::sharedApplication(mtm).setActivationPolicy(NSApplicationActivationPolicy::Regular);
    }

    /// GPUI supplies no Touch Bar. Give each view/window an explicit empty one so
    /// AppKit doesn't go looking via KVO on `nextResponder`; on macOS 26 that
    /// lookup throws NSRangeException when windows close (aborting the app).
    fn configure_window(&self, raw: *mut std::ffi::c_void) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        if raw.is_null() {
            return;
        }
        let view = raw as *mut objc2_app_kit::NSView;
        unsafe {
            let bar = objc2_app_kit::NSTouchBar::new(mtm);
            (*view).setTouchBar(Some(&bar));
            if let Some(window) = (*view).window() {
                window.setTouchBar(Some(&bar));
            }
        }
    }

    fn show_floating(&self, raw: *mut std::ffi::c_void) {
        if raw.is_null() || MainThreadMarker::new().is_none() {
            return;
        }
        let view = raw as *mut objc2_app_kit::NSView;
        unsafe {
            if let Some(window) = (*view).window() {
                // setStyleMask/orderFront synchronously invoke GPUI's resize and
                // visibility callbacks. The caller still holds App/Window here.
                // Retain the native window until the next main-queue turn rather
                // than retaining a borrowed NSView pointer across that boundary.
                extern "C-unwind" fn configure(context: *mut objc2::rc::Retained<NSWindow>) {
                    let window = unsafe { Box::from_raw(context) };
                    // A prompt may have been dismissed before this task runs.
                    if !window.isVisible() { return; }
                    // NSPanels hide when their app is inactive; stenly is never the
                    // active app during a call, so keep it visible and on top of
                    // everything (including full-screen meeting windows).
                    window.setHidesOnDeactivate(false);
                    // gpui gives title-less windows a Titled|FullSizeContentView frame;
                    // on macOS 26 that still draws the frame's top edge and titlebar
                    // separator as thin lines across our transparent content. Strip
                    // the frame: borderless + non-activating panel only.
                    window.setStyleMask(
                        objc2_app_kit::NSWindowStyleMask::Borderless | objc2_app_kit::NSWindowStyleMask::NonactivatingPanel,
                    );
                    window.setOpaque(false);
                    window.setBackgroundColor(Some(&objc2_app_kit::NSColor::clearColor()));
                    // Our content draws its own rounded shape; the system shadow
                    // would outline the rectangular window around it.
                    window.setHasShadow(false);
                    window.setCanHide(false);
                    window.setLevel(25 /* NSStatusWindowLevel */);
                    // A non-activating panel that only becomes key when a control in
                    // it needs typing: clicking the HUD never takes focus away from
                    // the app you're dictating into.
                    if let Some(panel) = window.downcast_ref::<objc2_app_kit::NSPanel>() {
                        panel.setBecomesKeyOnlyIfNeeded(true);
                        panel.setFloatingPanel(true);
                    }
                    window.setCollectionBehavior(
                        objc2_app_kit::NSWindowCollectionBehavior::CanJoinAllSpaces
                            | objc2_app_kit::NSWindowCollectionBehavior::FullScreenAuxiliary
                            | objc2_app_kit::NSWindowCollectionBehavior::Stationary,
                    );
                    window.orderFrontRegardless();
                }
                cidre::dispatch::Queue::main().async_f(Box::into_raw(Box::new(window)), configure);
            }
        }
    }

    fn set_floating_visible(&self, raw: *mut std::ffi::c_void, visible: bool) {
        if raw.is_null() || MainThreadMarker::new().is_none() {
            return;
        }
        let view = raw as *mut objc2_app_kit::NSView;
        unsafe {
            if let Some(window) = (*view).window() {
                // Never orderOut: removing a window from the screen list makes the
                // window server re-pick a key window and briefly pulls focus away
                // from the app being dictated into. Fade + click-through instead.
                window.setAlphaValue(if visible { 1.0 } else { 0.0 });
                window.setIgnoresMouseEvents(!visible);
                if visible {
                    window.orderFrontRegardless();
                }
            }
        }
    }

    fn move_window(&self, raw: *mut std::ffi::c_void, x: f64, y: f64) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        if raw.is_null() {
            return;
        }
        let view = raw as *mut objc2_app_kit::NSView;
        unsafe {
            if let Some(window) = (*view).window() {
                // AppKit is bottom-left origin relative to the primary screen.
                let primary_h = objc2_app_kit::NSScreen::screens(mtm).iter().next().map(|s| s.frame().size.height).unwrap_or(0.);
                let h = window.frame().size.height;
                window.setFrameOrigin(objc2_foundation::NSPoint::new(x, primary_h - y - h));
            }
        }
    }

    fn hide_from_capture(&self, raw: *mut std::ffi::c_void) {
        // Dev: STENLY_CAPTURABLE=1 keeps floating windows visible to screenshots.
        if raw.is_null() || MainThreadMarker::new().is_none() || std::env::var_os("STENLY_CAPTURABLE").is_some() {
            return;
        }
        // `raw` is an NSView* from raw-window-handle; walk up to its window.
        let view = raw as *mut objc2_app_kit::NSView;
        unsafe {
            if let Some(window) = (*view).window() {
                let window: &NSWindow = &window;
                window.setSharingType(NSWindowSharingType::None);
            }
        }
    }

    fn supports_hide_from_capture(&self) -> bool {
        true
    }

    fn permission(&self, p: Permission) -> PermissionState {
        let r = match p {
            Permission::Microphone => mic_state(),
            Permission::SystemAudio => system_audio_state(),
            Permission::Calendars => PermissionState::NotApplicable,
            Permission::Accessibility => {
                if unsafe { objc2_application_services::AXIsProcessTrusted() } { PermissionState::Granted } else { PermissionState::NotDetermined }
            }
        };
        tracing::trace!(permission = ?p, state = ?r, "permission state");
        r
    }

    fn request_permission(&self, p: Permission) {
        match p {
            Permission::Microphone => match mic_state() {
                PermissionState::Denied => self.open_permission_settings(p),
                PermissionState::Granted => {}
                _ => {
                    // The proper consent API: shows the prompt without opening the
                    // mic (no orange indicator). The UI re-polls for the result.
                    use cidre::{av, blocks};
                    let mut block = blocks::SendBlock::<fn(bool)>::new1(|granted: bool| {
                        tracing::info!(granted, "microphone permission answered");
                    });
                    if let Err(e) = av::CaptureDevice::request_access_for_media_type_ch(av::MediaType::audio(), &mut block) {
                        tracing::warn!(error = ?e, "requestAccess failed; opening System Settings");
                        self.open_permission_settings(p);
                    }
                }
            },
            Permission::Calendars => {},
            Permission::SystemAudio => match system_audio_state() {
                PermissionState::Denied => self.open_permission_settings(p),
                PermissionState::Granted => {}
                _ => {
                    // No request API: creating a tap is what makes macOS ask. The
                    // probe then confirms access by hearing a quiet tick (tap::probe).
                    tracing::info!("requesting system audio access via a probe tap");
                    std::thread::spawn(crate::macos::tap::probe);
                }
            },
            Permission::Accessibility => {
                // Shows the system "Stenly would like to control this computer"
                // sheet with a button straight to the right pane, and adds
                // Stenly to the list (toggled off) so the user only flips it.
                if !ax_trusted() {
                    crate::macos::hotkey::request_access();
                    prompt_accessibility();
                }
            }
        }
    }

    fn open_permission_settings(&self, p: Permission) {
        let anchor = match p {
            Permission::Microphone => "Privacy_Microphone",
            Permission::SystemAudio => "Privacy_AudioCapture",
            Permission::Calendars => "Privacy_Calendars",
            Permission::Accessibility => "Privacy_Accessibility",
        };
        open_url(&format!("x-apple.systempreferences:com.apple.preference.security?{anchor}"));
    }

    fn set_launch_at_login(&self, enabled: bool) -> anyhow::Result<()> {
        use objc2_service_management::SMAppService;
        let svc = unsafe { SMAppService::mainAppService() };
        let r = unsafe { if enabled { svc.registerAndReturnError() } else { svc.unregisterAndReturnError() } };
        r.map_err(|e| anyhow::anyhow!("{} (launch at login needs the bundled stenly.app)", e.localizedDescription()))
    }
}

/// Accessibility (needed to post ⌘V into other apps and for the key tap).
pub fn ax_trusted() -> bool {
    unsafe { objc2_application_services::AXIsProcessTrusted() }
}

/// AXIsProcessTrustedWithOptions({kAXTrustedCheckOptionPrompt: true}).
fn prompt_accessibility() {
    use std::ffi::c_void;
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
        static kAXTrustedCheckOptionPrompt: *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFDictionaryCreate(a: *const c_void, k: *const *const c_void, v: *const *const c_void, n: isize, kcb: *const c_void, vcb: *const c_void) -> *const c_void;
        fn CFRelease(cf: *const c_void);
        static kCFBooleanTrue: *const c_void;
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
    }
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt];
        let vals = [kCFBooleanTrue];
        let d = CFDictionaryCreate(std::ptr::null(), keys.as_ptr(), vals.as_ptr(), 1, &kCFTypeDictionaryKeyCallBacks as *const _ as *const c_void, &kCFTypeDictionaryValueCallBacks as *const _ as *const c_void);
        AXIsProcessTrustedWithOptions(d);
        if !d.is_null() {
            CFRelease(d);
        }
    }
}
