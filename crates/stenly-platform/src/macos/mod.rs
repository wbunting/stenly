//! macOS: Core Audio process tap for system audio (14.2+, audio-only permission,
//! no screen recording), cpal/CoreAudio for the mic, Core Audio process objects
//! for "who has the mic", AppKit for menubar-only + capture exclusion.

pub mod appkit_ui;
mod ax_tree;
mod calendar;
pub mod hotkey;
mod inject;
mod mic_activity;
pub mod screens;
pub(crate) mod shell;
pub(crate) mod tap;

use crate::Platform;
use crossbeam_channel::Sender;
use std::sync::Arc;
use stenly_core::audio::AudioChunk;
use stenly_core::platform::{AudioBackend, CaptureStream};

struct MacAudio;

impl AudioBackend for MacAudio {
    fn name(&self) -> &'static str {
        "coreaudio"
    }
    fn start_mic(&self, sink: Sender<AudioChunk>) -> anyhow::Result<Box<dyn CaptureStream>> {
        crate::cpal_mic::start_input_on_thread("mic", pick_mic, sink)
    }
    fn start_system(&self, sink: Sender<AudioChunk>) -> anyhow::Result<Box<dyn CaptureStream>> {
        tap::start(sink)
    }
    fn input_devices(&self) -> Vec<stenly_core::platform::InputDevice> {
        crate::cpal_mic::input_devices(&cpal::default_host())
    }
    fn start_mic_from(&self, priority: &[String], sink: Sender<AudioChunk>) -> anyhow::Result<Box<dyn CaptureStream>> {
        let priority = priority.to_vec();
        crate::cpal_mic::start_input_on_thread(
            "mic",
            move || match crate::cpal_mic::first_available(&cpal::default_host(), &priority) {
                Some(d) => Ok(d),
                None => pick_mic(),
            },
            sink,
        )
    }
}

/// The built-in mic is hardware-muted while the lid is closed (clamshell mode
/// with an external display), yet it stays the default input. Prefer the
/// default unless it's the built-in mic with the lid shut; then fall back to
/// another physical mic (display, USB, Bluetooth), skipping virtual devices.
fn pick_mic() -> anyhow::Result<cpal::Device> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let default = host.default_input_device();
    let name = |d: &cpal::Device| d.description().map(|x| x.name().to_string()).unwrap_or_default();
    let is_builtin = |n: &str| n.contains("MacBook") && n.contains("Microphone");
    let lid_closed = clamshell_closed();
    if let Some(d) = &default {
        if !(lid_closed && is_builtin(&name(d))) {
            return Ok(d.clone());
        }
        tracing::info!(default = %name(d), "lid closed: built-in mic is muted, looking for another mic");
    }
    let virtual_ = |n: &str| {
        let n = n.to_lowercase();
        ["blackhole", "zoomaudio", "loopback", "soundflower", "aggregate", "teams audio", "background music", "virtual"].iter().any(|v| n.contains(v))
    };
    let candidates: Vec<cpal::Device> = host.input_devices()?.filter(|d| {
        let n = name(d);
        !virtual_(&n) && !is_builtin(&n)
    }).collect();
    // Prefer display/USB mics, then anything else (Bluetooth last: it degrades output quality).
    let rank = |d: &cpal::Device| {
        let n = name(d).to_lowercase();
        if n.contains("display") || n.contains("usb") { 0 } else if n.contains("airpods") || n.contains("bluetooth") || n.contains("iphone") { 2 } else { 1 }
    };
    match candidates.into_iter().min_by_key(rank) {
        Some(d) => {
            tracing::info!(mic = %name(&d), "using fallback mic");
            Ok(d)
        }
        None => default.ok_or_else(|| anyhow::anyhow!("no microphone available")),
    }
}

/// `AppleClamshellState` from IOKit's power-management root domain.
pub fn clamshell_closed() -> bool {
    std::process::Command::new("/usr/sbin/ioreg")
        .args(["-r", "-k", "AppleClamshellState", "-d", "1"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("\"AppleClamshellState\" = Yes"))
        .unwrap_or(false)
}

pub fn platform() -> Platform {
    Platform {
        name: "macos",
        audio: Arc::new(MacAudio),
        mic_activity: Arc::new(mic_activity::CoreAudioMicActivity),
        shell: Box::new(shell::MacShell),
        hotkeys: Arc::new(hotkey::MacHotkeys::default()),
        injector: Arc::new(inject::MacInjector),
        meeting_ax: Some(Arc::new(ax_tree::MacMeetingAx::new())),
        calendar: Some(Arc::new(calendar::EventKitCalendar)),
    }
}

/// Mute the default output while dictating (other apps' audio would bleed
/// into an open mic). Returns a guard that restores the previous state.
pub fn mute_output() -> Option<MuteGuard> {
    use cidre::core_audio as ca;
    let dev = ca::System::default_output_device().ok()?;
    let addr = ca::PropSelector::DEVICE_MUTE.addr(ca::PropScope::OUTPUT, ca::PropElement::MAIN);
    let was: u32 = dev.prop(&addr).ok()?;
    if was == 1 {
        return None; // already muted: nothing to restore
    }
    dev.set_prop(&addr, &1u32).ok()?;
    Some(MuteGuard { dev_id: dev.0.0 })
}

pub struct MuteGuard {
    dev_id: u32,
}

impl Drop for MuteGuard {
    fn drop(&mut self) {
        use cidre::core_audio as ca;
        let addr = ca::PropSelector::DEVICE_MUTE.addr(ca::PropScope::OUTPUT, ca::PropElement::MAIN);
        let dev = ca::Device(ca::Obj(self.dev_id));
        let _ = dev.set_prop(&addr, &0u32);
    }
}
