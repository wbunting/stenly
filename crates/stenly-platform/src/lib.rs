//! OS backends for the traits in `stenly_core::platform`.
//!
//! App code calls [`detect`] once and only ever sees trait objects. Adding a
//! platform (or a Linux desktop flavour) means adding a module here and a
//! branch in `detect`; nothing else changes.

use std::sync::Arc;
use stenly_core::platform::{AudioBackend, CalendarSource, DesktopShell, Hotkeys, MicActivity, TextInjector};

mod cpal_mic;
pub mod updater;
/// Synthesized UI sounds.
pub mod chime;
#[allow(dead_code)]
mod nohotkeys;

#[cfg(target_os = "macos")]
mod macos;
/// Native macOS UI helpers (SF Symbols, accent colour, pop-up menus).
#[cfg(target_os = "macos")]
pub use macos::appkit_ui;
/// Which display a window / the pointer is on.
#[cfg(target_os = "macos")]
pub use macos::screens;
/// Keyboard-listening permission helpers (dictation shortcuts).
#[cfg(target_os = "macos")]
pub use macos::hotkey::{has_access as hotkeys_allowed, post_key, post_right_command, request_access as request_hotkey_access};
#[cfg(target_os = "linux")]
mod linux;

pub struct Platform {
    pub name: &'static str,
    pub audio: Arc<dyn AudioBackend>,
    pub mic_activity: Arc<dyn MicActivity>,
    pub shell: Box<dyn DesktopShell>,
    /// Global shortcuts (dictation).
    pub hotkeys: Arc<dyn Hotkeys>,
    /// Typing text into the focused app.
    pub injector: Arc<dyn TextInjector>,
    /// Who's talking in a meeting app (accessibility tree), when supported.
    pub meeting_ax: Option<Arc<dyn stenly_core::speakers::ax::MeetingAxSource>>,
    pub calendar: Option<Arc<dyn CalendarSource>>,
}

pub fn detect() -> Platform {
    #[cfg(target_os = "macos")]
    let mut p = macos::platform();
    #[cfg(target_os = "linux")]
    let mut p = linux::platform();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let mut p = fallback::platform();
    // Dev/test: STENLY_FAKE_MIC=<wav> plays a file as the microphone, in real
    // time, so dictation can be tested end to end with known speech.
    if let Some(wav) = std::env::var_os("STENLY_FAKE_MIC") {
        tracing::warn!(wav = ?wav, "using a WAV file as the microphone");
        p.audio = std::sync::Arc::new(fake_mic::FakeMic { inner: p.audio, wav: wav.into() });
    }
    p
}

mod fake_mic {
    use crossbeam_channel::Sender;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use stenly_core::audio::AudioChunk;
    use stenly_core::platform::{AudioBackend, CaptureStream};

    pub struct FakeMic {
        pub inner: Arc<dyn AudioBackend>,
        pub wav: std::path::PathBuf,
    }

    struct Playing(Arc<AtomicBool>);
    impl CaptureStream for Playing {
        fn describe(&self) -> String {
            "fake mic (WAV)".into()
        }
    }
    impl Drop for Playing {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    impl AudioBackend for FakeMic {
        fn name(&self) -> &'static str {
            "fake mic"
        }
        fn start_mic(&self, sink: Sender<AudioChunk>) -> anyhow::Result<Box<dyn CaptureStream>> {
            let samples = stenly_core::audio::read_wav_16k_mono(&self.wav)?;
            let stop = Arc::new(AtomicBool::new(false));
            let s = stop.clone();
            std::thread::spawn(move || {
                // 10 ms chunks at 16 kHz, paced against the wall clock; silence after the file ends
                let t0 = std::time::Instant::now();
                let mut i = 0usize;
                while !s.load(Ordering::Relaxed) {
                    let chunk: Vec<f32> = (i..i + 160).map(|k| samples.get(k).copied().unwrap_or(0.0)).collect();
                    i += 160;
                    if sink.send(AudioChunk { samples: chunk, channels: 1, sample_rate: 16_000 }).is_err() {
                        break;
                    }
                    let due = std::time::Duration::from_secs_f64(i as f64 / 16_000.0);
                    if let Some(wait) = due.checked_sub(t0.elapsed()) {
                        std::thread::sleep(wait);
                    }
                }
            });
            Ok(Box::new(Playing(stop)))
        }
        fn start_mic_from(&self, _priority: &[String], sink: Sender<AudioChunk>) -> anyhow::Result<Box<dyn CaptureStream>> {
            self.start_mic(sink)
        }
        fn start_system(&self, sink: Sender<AudioChunk>) -> anyhow::Result<Box<dyn CaptureStream>> {
            self.inner.start_system(sink)
        }
        fn input_devices(&self) -> Vec<stenly_core::platform::InputDevice> {
            self.inner.input_devices()
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod fallback {
    use super::*;
    use stenly_core::platform::{MicUser};

    struct NoMic;
    impl MicActivity for NoMic {
        fn mic_users(&self) -> anyhow::Result<Vec<MicUser>> {
            Ok(vec![])
        }
    }
    struct NoShell;
    impl DesktopShell for NoShell {}

    struct MicOnly;
    impl AudioBackend for MicOnly {
        fn name(&self) -> &'static str {
            "cpal (mic only)"
        }
        fn start_mic(&self, sink: crossbeam_channel::Sender<stenly_core::audio::AudioChunk>) -> anyhow::Result<Box<dyn stenly_core::platform::CaptureStream>> {
            crate::cpal_mic::start_default_input(sink)
        }
        fn start_system(&self, _sink: crossbeam_channel::Sender<stenly_core::audio::AudioChunk>) -> anyhow::Result<Box<dyn stenly_core::platform::CaptureStream>> {
            anyhow::bail!("system audio capture is not implemented on this platform")
        }
    }

    pub fn platform() -> Platform {
        Platform {
            name: "generic",
            audio: Arc::new(MicOnly),
            mic_activity: Arc::new(NoMic),
            shell: Box::new(NoShell),
            hotkeys: Arc::new(crate::nohotkeys::NoHotkeys),
            injector: Arc::new(crate::nohotkeys::ClipboardOnly),
            meeting_ax: None,
            calendar: None,
        }
    }
}

/// Guard that unmutes the system output when dropped.
pub struct OutputMute(#[allow(dead_code)] Box<dyn std::any::Any + Send>);

/// Mute other audio (dictation). None if unsupported or already muted.
pub fn mute_output() -> Option<OutputMute> {
    #[cfg(target_os = "macos")]
    {
        macos::mute_output().map(|g| OutputMute(Box::new(g)))
    }
    #[cfg(target_os = "linux")]
    {
        linux::mute_output().map(|g| OutputMute(Box::new(g)))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}
