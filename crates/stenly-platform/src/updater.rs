//! Optional native updater boundary. Unconfigured builds never contact a feed.
#[cfg(all(target_os = "macos", stenly_sparkle))]
mod native {
    use std::sync::Mutex;
    static INSTALL: Mutex<Option<stenly_core::update::Installation>> = Mutex::new(None);
    unsafe extern "C" {
        fn stenly_updater_start(
            enabled: bool,
            acquire: extern "C" fn() -> bool,
            release: extern "C" fn(),
            busy: extern "C" fn() -> bool,
            testing: bool,
        ) -> i32;
        fn stenly_updater_enabled(enabled: bool);
        fn stenly_updater_check();
        fn stenly_updater_error() -> std::ffi::c_long;
        fn stenly_updater_available_version() -> *const std::ffi::c_char;
        fn stenly_updater_available_url() -> *const std::ffi::c_char;
    }
    extern "C" fn acquire() -> bool {
        let Ok(mut held) = INSTALL.lock() else {
            return false;
        };
        if held.is_some() {
            return true;
        }
        *held = stenly_core::update::Installation::begin();
        held.is_some()
    }
    extern "C" fn release() {
        if let Ok(mut held) = INSTALL.lock() {
            *held = None;
        }
    }
    extern "C" fn busy() -> bool {
        stenly_core::update::busy()
    }
    pub fn start(enabled: bool) -> i32 {
        unsafe { stenly_updater_start(enabled, acquire, release, busy, false) }
    }
    pub fn enabled(value: bool) {
        unsafe { stenly_updater_enabled(value) }
    }
    pub fn check() {
        unsafe { stenly_updater_check() }
    }
    pub fn available() -> Option<(String, String)> {
        unsafe {
            let version = stenly_updater_available_version();
            let url = stenly_updater_available_url();
            if version.is_null() || url.is_null() {
                return None;
            }
            Some((
                std::ffi::CStr::from_ptr(version)
                    .to_string_lossy()
                    .into_owned(),
                std::ffi::CStr::from_ptr(url).to_string_lossy().into_owned(),
            ))
        }
    }
    pub fn error() -> i64 {
        unsafe { stenly_updater_error() as i64 }
    }
}

/// 0 = not bundled/configured, 1 = started, -1 = failed. Main-thread only.
pub fn start(enabled: bool) -> i32 {
    #[cfg(all(target_os = "macos", stenly_sparkle))]
    {
        return native::start(enabled);
    }
    #[cfg(not(all(target_os = "macos", stenly_sparkle)))]
    {
        let _ = enabled;
        0
    }
}
pub fn set_enabled(enabled: bool) {
    #[cfg(all(target_os = "macos", stenly_sparkle))]
    native::enabled(enabled);
    #[cfg(not(all(target_os = "macos", stenly_sparkle)))]
    let _ = enabled;
}
pub fn check() {
    #[cfg(all(target_os = "macos", stenly_sparkle))]
    native::check();
}
pub fn last_error_code() -> i64 {
    #[cfg(all(target_os = "macos", stenly_sparkle))]
    {
        return native::error();
    }
    #[cfg(not(all(target_os = "macos", stenly_sparkle)))]
    0
}

/// Probing result only: normal Cargo builds never offer native installation.
pub fn available() -> Option<(String, String)> {
    #[cfg(all(target_os = "macos", stenly_sparkle))]
    {
        return native::available();
    }
    #[cfg(not(all(target_os = "macos", stenly_sparkle)))]
    None
}
