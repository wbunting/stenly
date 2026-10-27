//! Microphone capture through cpal (CoreAudio / PipeWire / Pulse / ALSA).
//! cpal's `Stream` is `!Send` on some hosts, so it lives on its own thread.

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::Sender;
use stenly_core::audio::AudioChunk;
use stenly_core::platform::CaptureStream;

pub struct ThreadStream {
    pub desc: String,
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CaptureStream for ThreadStream {
    fn describe(&self) -> String {
        self.desc.clone()
    }
}

impl Drop for ThreadStream {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Build an input stream on a dedicated thread. `pick` selects the device.
pub fn start_input_on_thread(
    label: &'static str,
    pick: impl FnOnce() -> Result<cpal::Device> + Send + 'static,
    sink: Sender<AudioChunk>,
) -> Result<Box<dyn CaptureStream>> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<String>>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::Builder::new().name(format!("stenly-{label}-capture")).spawn(move || {
        let run = || -> Result<(cpal::Stream, String)> {
            let device = pick()?;
            let name = device.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "unknown".into());
            let config = device.default_input_config().context("no default input config")?;
            let rate = config.sample_rate();
            let channels = config.channels();
            let fmt = config.sample_format();
            let stream_cfg: cpal::StreamConfig = config.into();
            let err_fn = move |e: cpal::Error| tracing::warn!(error = %e, "{label} stream error");
            macro_rules! build {
                ($t:ty, $conv:expr) => {{
                    let sink = sink.clone();
                    device.build_input_stream::<$t, _, _>(
                        stream_cfg.clone(),
                        move |data: &[$t], _| {
                            let samples: Vec<f32> = data.iter().map($conv).collect();
                            let _ = sink.try_send(AudioChunk { samples, channels, sample_rate: rate });
                        },
                        err_fn,
                        None,
                    )?
                }};
            }
            let stream = match fmt {
                cpal::SampleFormat::F32 => build!(f32, |s: &f32| *s),
                cpal::SampleFormat::I16 => build!(i16, |s: &i16| *s as f32 / 32768.0),
                cpal::SampleFormat::I32 => build!(i32, |s: &i32| *s as f32 / 2147483648.0),
                cpal::SampleFormat::U16 => build!(u16, |s: &u16| (*s as f32 - 32768.0) / 32768.0),
                other => return Err(anyhow!("unsupported sample format {other:?}")),
            };
            stream.play()?;
            Ok((stream, format!("{name} ({rate} Hz, {channels} ch)")))
        };
        match run() {
            Ok((stream, desc)) => {
                let _ = ready_tx.send(Ok(desc));
                let _ = stop_rx.recv(); // blocks until the handle is dropped
                drop(stream);
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        }
    })?;
    let desc = ready_rx.recv().context("capture thread died")??;
    Ok(Box::new(ThreadStream { desc, stop: Some(stop_tx), thread: Some(thread) }))
}

#[allow(dead_code)]
pub fn start_default_input(sink: Sender<AudioChunk>) -> Result<Box<dyn CaptureStream>> {
    start_input_on_thread(
        "mic",
        || cpal::default_host().default_input_device().context("no default microphone"),
        sink,
    )
}

/// Loopback / app-provided devices that never carry the user's voice.
pub fn is_virtual(name: &str) -> bool {
    let n = name.to_lowercase();
    ["blackhole", "zoomaudio", "loopback", "soundflower", "aggregate", "teams audio", "background music", "virtual", "microsoft teams", "monitor of"]
        .iter()
        .any(|v| n.contains(v))
}

pub fn input_devices(host: &cpal::Host) -> Vec<stenly_core::platform::InputDevice> {
    let default = host.default_input_device().and_then(|d| d.id().ok()).map(|i| i.to_string());
    let Ok(devs) = host.input_devices() else { return vec![] };
    let mut out: Vec<stenly_core::platform::InputDevice> = devs
        .filter_map(|d| {
            let id = d.id().ok()?.to_string();
            let name = d.description().map(|x| x.name().to_string()).unwrap_or_else(|_| id.clone());
            Some(stenly_core::platform::InputDevice { is_virtual: is_virtual(&name), is_default: Some(&id) == default.as_ref(), id, name })
        })
        .collect();
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// First device in `priority` (matched by id, then by exact name, then by
/// name prefix) that's connected right now.
pub fn first_available(host: &cpal::Host, priority: &[String]) -> Option<cpal::Device> {
    if priority.is_empty() {
        return None;
    }
    let devs: Vec<cpal::Device> = host.input_devices().ok()?.collect();
    let id = |d: &cpal::Device| d.id().map(|i| i.to_string()).unwrap_or_default();
    let name = |d: &cpal::Device| d.description().map(|x| x.name().to_string()).unwrap_or_default();
    for want in priority {
        let w = want.trim();
        if w.is_empty() {
            continue;
        }
        if let Some(d) = devs.iter().find(|d| id(d) == w).or_else(|| devs.iter().find(|d| name(d) == w)).or_else(|| devs.iter().find(|d| name(d).to_lowercase().starts_with(&w.to_lowercase()))) {
            tracing::info!(mic = %name(d), "using prioritised mic");
            return Some(d.clone());
        }
    }
    None
}
