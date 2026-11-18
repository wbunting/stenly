//! Linux core: works on any desktop/compositor. System audio comes from the
//! default sink's monitor (PipeWire or PulseAudio), the mic from the default
//! source, and "who is on a call" from the sound server's client list.
//!
//! Desktop-specific niceties (tray flavour, layer-shell popups, hiding from
//! screencasts) are layered on top in the app via separate features, not here.

mod hotkey;
mod inject;
mod mic_activity;
mod raise;
mod setup;

use crate::Platform;
use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait};
use crossbeam_channel::Sender;
use std::sync::Arc;
use stenly_core::audio::AudioChunk;
use stenly_core::platform::{AudioBackend, CaptureStream, DesktopShell, SetupCheck};

struct LinuxAudio;

/// Prefer PipeWire, then PulseAudio, then whatever cpal defaults to (ALSA).
fn host() -> cpal::Host {
    #[allow(unused_mut)]
    let mut preferred: Vec<cpal::HostId> = Vec::new();
    #[cfg(feature = "pipewire")]
    preferred.push(cpal::HostId::PipeWire);
    #[cfg(feature = "pulseaudio")]
    preferred.push(cpal::HostId::PulseAudio);
    for id in preferred {
        if let Ok(h) = cpal::host_from_id(id) {
            return h;
        }
    }
    cpal::default_host()
}

/// The monitor of the default output: PipeWire exposes it as the default
/// sink opened for capture; Pulse as a `*.monitor` source.
fn system_device() -> Result<cpal::Device> {
    let host = host();
    let devices: Vec<cpal::Device> = host.input_devices().context("listing input devices")?.collect();
    let name_of = |d: &cpal::Device| d.description().map(|x| x.name().to_lowercase()).unwrap_or_default();
    let id_of = |d: &cpal::Device| d.id().map(|x| x.to_string().to_lowercase()).unwrap_or_default();
    // PipeWire: cpal's "sink_default" capture-able node.
    if let Some(d) = devices.iter().find(|d| id_of(d).ends_with("sink_default")) {
        return Ok(d.clone());
    }
    // Pulse / ALSA-pulse plugin: the monitor of the default sink.
    if let Some(out) = host.default_output_device() {
        let out_name = name_of(&out);
        if let Some(d) = devices.iter().find(|d| {
            let n = name_of(d);
            n.contains("monitor") && (n.contains(&out_name) || out_name.is_empty())
        }) {
            return Ok(d.clone());
        }
    }
    devices
        .into_iter()
        .find(|d| name_of(d).contains("monitor") || id_of(d).contains("monitor"))
        .context("no monitor source found; build with --features pipewire or pulseaudio, or enable a PipeWire/Pulse monitor")
}

impl AudioBackend for LinuxAudio {
    fn name(&self) -> &'static str {
        "linux-cpal"
    }
    fn start_mic(&self, sink: Sender<AudioChunk>) -> Result<Box<dyn CaptureStream>> {
        crate::cpal_mic::start_input_on_thread("mic", default_mic, sink)
    }
    fn start_system(&self, sink: Sender<AudioChunk>) -> Result<Box<dyn CaptureStream>> {
        crate::cpal_mic::start_input_on_thread("system", system_device, sink)
    }
    fn input_devices(&self) -> Vec<stenly_core::platform::InputDevice> {
        let host = host();
        let mics = microphones(&host);
        let default = default_source_id(&mics);
        crate::cpal_mic::describe_inputs(mics, default.as_deref())
    }
    fn start_mic_from(&self, priority: &[String], sink: Sender<AudioChunk>) -> Result<Box<dyn CaptureStream>> {
        let priority = priority.to_vec();
        crate::cpal_mic::start_input_on_thread(
            "mic",
            move || match crate::cpal_mic::first_in(microphones(&host()), &priority) {
                Some(d) => Ok(d),
                None => default_mic(),
            },
            sink,
        )
    }
}

/// Real microphones. PipeWire also offers every output (and cpal's synthetic
/// `default_sink`) as a capture device, which records what you hear; hide
/// those, monitors, and the synthetic `default_input` (the default source is
/// marked on its real device instead).
fn microphones(host: &cpal::Host) -> Vec<cpal::Device> {
    let pipewire = host.id().name().eq_ignore_ascii_case("pipewire");
    let Ok(devs) = host.input_devices() else { return vec![] };
    devs.filter(|d| {
        let Ok(desc) = d.description() else { return false };
        let id = d.id().map(|i| i.to_string()).unwrap_or_default();
        is_microphone(pipewire, desc.direction(), &id, desc.name())
    })
    .collect()
}

fn is_microphone(pipewire: bool, direction: cpal::DeviceDirection, id: &str, name: &str) -> bool {
    let id = id.to_lowercase();
    // Only PipeWire reports outputs as duplex capture devices; ALSA duplex
    // cards are real microphones.
    (!pipewire || direction == cpal::DeviceDirection::Input)
        && !id.ends_with("input_default")
        && !id.contains("monitor")
        && !name.to_lowercase().contains("monitor")
}

/// The sound server's default source, as the real device so logs and the
/// settings page name it; cpal's generic default when it can't be resolved.
fn default_mic() -> Result<cpal::Device> {
    let host = host();
    let mics = microphones(&host);
    if let Some(id) = default_source_id(&mics) {
        if let Some(d) = mics.into_iter().find(|d| d.id().is_ok_and(|i| i.to_string() == id)) {
            return Ok(d);
        }
    }
    host.default_input_device().context("no default microphone")
}

/// cpal id of the sound server's default source, matched by node name.
fn default_source_id(mics: &[cpal::Device]) -> Option<String> {
    let out = std::process::Command::new("pactl").arg("get-default-source").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let node = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if node.is_empty() {
        return None;
    }
    mics.iter().filter_map(|d| d.id().ok().map(|i| i.to_string())).find(|id| id.rsplit_once(':').map(|(_, n)| n) == Some(node.as_str()))
}

/// Generic shell: no Dock to hide from, no portable screencast exclusion.
struct LinuxShell;
impl DesktopShell for LinuxShell {
    fn setup_checks(&self) -> Vec<SetupCheck> {
        setup::checks()
    }
    fn raise_window(&self, title: &str) {
        raise::raise_window(title)
    }
}

pub fn platform() -> Platform {
    Platform {
        name: "linux",
        audio: Arc::new(LinuxAudio),
        mic_activity: Arc::new(mic_activity::SoundServerMicActivity),
        shell: Box::new(LinuxShell),
        hotkeys: Arc::new(hotkey::LinuxHotkeys::default()),
        injector: Arc::new(inject::LinuxInjector),
        // AT-SPI could expose browser meeting tiles too; not implemented yet.
        meeting_ax: None,
        calendar: None,
    }
}

/// Mute the default sink while dictating; the guard unmutes.
pub fn mute_output() -> Option<MuteGuard> {
    let muted = |out: &str| out.contains("MUTED") || out.to_lowercase().contains("mute: yes");
    if let Ok(o) = std::process::Command::new("wpctl").args(["get-volume", "@DEFAULT_AUDIO_SINK@"]).output() {
        if o.status.success() {
            if muted(&String::from_utf8_lossy(&o.stdout)) {
                return None;
            }
            std::process::Command::new("wpctl").args(["set-mute", "@DEFAULT_AUDIO_SINK@", "1"]).status().ok()?;
            return Some(MuteGuard { wpctl: true });
        }
    }
    let o = std::process::Command::new("pactl").args(["get-sink-mute", "@DEFAULT_SINK@"]).output().ok()?;
    if muted(&String::from_utf8_lossy(&o.stdout)) {
        return None;
    }
    std::process::Command::new("pactl").args(["set-sink-mute", "@DEFAULT_SINK@", "1"]).status().ok()?;
    Some(MuteGuard { wpctl: false })
}

pub struct MuteGuard {
    wpctl: bool,
}

impl Drop for MuteGuard {
    fn drop(&mut self) {
        let _ = if self.wpctl {
            std::process::Command::new("wpctl").args(["set-mute", "@DEFAULT_AUDIO_SINK@", "0"]).status()
        } else {
            std::process::Command::new("pactl").args(["set-sink-mute", "@DEFAULT_SINK@", "0"]).status()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::is_microphone;
    use cpal::DeviceDirection::{Duplex, Input, Output};

    #[test]
    fn only_real_sources_are_microphones() {
        assert!(is_microphone(true, Input, "pipewire:alsa_input.usb-display.mono-fallback", "Studio Display XDR Mono"));
        assert!(!is_microphone(true, Input, "pipewire:input_default", "default_input"));
        assert!(!is_microphone(true, Duplex, "pipewire:sink_default", "default_sink"));
        assert!(!is_microphone(true, Duplex, "pipewire:alsa_output.usb-display.analog-stereo", "Studio Display XDR Analog Stereo"));
        assert!(!is_microphone(true, Output, "pipewire:output_default", "default_output"));
        assert!(!is_microphone(false, Input, "pulseaudio:alsa_output.x.monitor", "Monitor of Speakers"));
        assert!(is_microphone(false, Duplex, "alsa:hw:1,0", "USB Audio"));
    }
}
