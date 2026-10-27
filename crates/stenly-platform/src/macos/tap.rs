//! System audio via a global Core Audio process tap wrapped in a private
//! aggregate device. Excludes our own process so UI sounds never loop back.

use anyhow::{Context, Result, anyhow};
use cidre::{arc, cat, cf, core_audio as ca, ns, os};
use crossbeam_channel::Sender;
use std::sync::mpsc;
use stenly_core::audio::AudioChunk;
use stenly_core::platform::CaptureStream;

/// Set once a tap delivers non-silent audio (= the user granted System Audio access).
static VERIFIED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn marker() -> std::path::PathBuf {
    stenly_core::config::data_dir().join(".system-audio-ok")
}

pub fn verified() -> bool {
    VERIFIED.load(std::sync::atomic::Ordering::Relaxed) || marker().exists()
}

fn mark_verified() {
    if !VERIFIED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        let _ = std::fs::create_dir_all(stenly_core::config::data_dir());
        let _ = std::fs::write(marker(), b"");
    }
}

/// Open a tap to trigger the consent prompt; keep it open (silently) until the
/// user has answered, up to 2 minutes, then close it.
pub fn probe() {
    let (tx, rx) = crossbeam_channel::unbounded();
    match start(tx) {
        Ok(s) => {
            let t0 = std::time::Instant::now();
            let mut last_ping = std::time::Instant::now() - std::time::Duration::from_secs(10);
            while t0.elapsed() < std::time::Duration::from_secs(120) {
                std::thread::sleep(std::time::Duration::from_millis(250));
                while rx.try_recv().is_ok() {}
                if verified() {
                    break;
                }
                // Our own process is excluded from the tap, so play a very quiet
                // tick via afplay (a separate process) every few seconds: once the
                // user approves, the tap hears it and we know access works.
                if last_ping.elapsed() > std::time::Duration::from_secs(3) {
                    last_ping = std::time::Instant::now();
                    let _ = std::process::Command::new("/usr/bin/afplay")
                        .args(["-v", "0.05", "/System/Library/Sounds/Tink.aiff"])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn();
                }
            }
            tracing::info!(secs = t0.elapsed().as_secs(), verified = verified(), "probe tap closed");
            drop(s);
        }
        Err(e) => tracing::warn!(error = %format!("{e:#}"), "probe tap failed"),
    }
}

struct Ctx {
    sink: Sender<AudioChunk>,
    channels: u16,
    interleaved: bool,
    /// The tap's ASBD rate goes stale when the output device changes rate
    /// mid-recording (AirPods drop from 48 kHz to a 16/24 kHz call profile the
    /// moment Zoom grabs the mic) — the tap keeps saying 48 kHz while frames
    /// arrive at 16 kHz. So measure the real rate from the timestamps.
    clock: RateClock,
}

/// Measures the true frame rate from the IO proc's input timestamps (sample
/// time vs host time). Mislabeled 16 kHz audio got "resampled" to a third of
/// its length and the writer padded the rest with silence against the wall
/// clock: 10 ms of sound, 20 ms of silence — choppy, chipmunked speech.
struct RateClock {
    anchor: Option<(f64, u64)>,
    rate: u32,
}

impl RateClock {
    const COMMON: [u32; 9] = [8_000, 11_025, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 96_000];

    fn new(rate: u32) -> Self {
        Self { anchor: None, rate }
    }

    fn observe(&mut self, sample_time: f64, host_time: u64) -> u32 {
        if !sample_time.is_finite() || host_time == 0 {
            return self.rate;
        }
        let Some((s0, h0)) = self.anchor else {
            self.anchor = Some((sample_time, host_time));
            return self.rate;
        };
        let secs = host_secs(host_time.wrapping_sub(h0));
        let frames = sample_time - s0;
        if host_time < h0 || frames < 0.0 || secs > 5.0 {
            // discontinuity (device restart / long idle): start over
            self.anchor = Some((sample_time, host_time));
        } else if secs >= 0.3 {
            let measured = frames / secs;
            if let Some(r) = Self::COMMON.iter().copied().find(|r| ((*r as f64 - measured).abs() / *r as f64) < 0.03) {
                if r != self.rate {
                    tracing::info!(from = self.rate, to = r, "system audio sample rate changed");
                    self.rate = r;
                }
            }
            self.anchor = Some((sample_time, host_time));
        }
        self.rate
    }
}

/// mach_absolute_time ticks → seconds.
fn host_secs(ticks: u64) -> f64 {
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }
    static TB: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    let ns_per_tick = *TB.get_or_init(|| {
        let mut t = Timebase { numer: 0, denom: 0 };
        unsafe { mach_timebase_info(&mut t) };
        if t.denom == 0 { 1.0 } else { t.numer as f64 / t.denom as f64 }
    });
    ticks as f64 * ns_per_tick / 1e9
}

extern "C" fn io_proc(
    _device: ca::Device,
    _now: &cat::AudioTimeStamp,
    input: &cat::AudioBufList<2>,
    input_time: &cat::AudioTimeStamp,
    _output: &mut cat::AudioBufList<2>,
    _output_time: &cat::AudioTimeStamp,
    ctx: Option<&mut Ctx>,
) -> os::Status {
    let Some(ctx) = ctx else { return Default::default() };
    let n_bufs = (input.number_buffers as usize).min(2);
    if n_bufs == 0 {
        return Default::default();
    }
    let bufs = &input.buffers[..n_bufs];
    let samples: Vec<f32> = if ctx.interleaved || n_bufs == 1 {
        let b = &bufs[0];
        if b.data.is_null() {
            return Default::default();
        }
        let n = b.data_bytes_size as usize / 4;
        unsafe { std::slice::from_raw_parts(b.data as *const f32, n) }.to_vec()
    } else {
        // planar → interleaved
        let n = bufs.iter().map(|b| b.data_bytes_size as usize / 4).min().unwrap_or(0);
        let planes: Vec<&[f32]> = bufs
            .iter()
            .filter(|b| !b.data.is_null())
            .map(|b| unsafe { std::slice::from_raw_parts(b.data as *const f32, n) })
            .collect();
        let mut out = Vec::with_capacity(n * planes.len());
        for i in 0..n {
            for p in &planes {
                out.push(p[i]);
            }
        }
        out
    };
    let channels = if ctx.interleaved || n_bufs == 1 { ctx.channels } else { n_bufs as u16 };
    // Without consent the tap delivers pure digital silence.
    if !VERIFIED.load(std::sync::atomic::Ordering::Relaxed) && samples.iter().any(|s| *s != 0.0) {
        mark_verified();
    }
    let rate = ctx.clock.observe(input_time.sample_time, input_time.host_time);
    let _ = ctx.sink.try_send(AudioChunk { samples, channels, sample_rate: rate });
    Default::default()
}

pub struct TapStream {
    desc: String,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CaptureStream for TapStream {
    fn describe(&self) -> String {
        self.desc.clone()
    }
}

impl Drop for TapStream {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn os_err(what: &str) -> impl FnOnce(os::Error) -> anyhow::Error + '_ {
    move |e| anyhow!("{what} failed: {e:?}")
}

/// Process objects to exclude from the tap: ourselves.
fn self_exclusion() -> arc::R<ns::Array<ns::Number>> {
    let pid = std::process::id() as i32;
    match ca::Process::with_pid(pid) {
        Ok(p) => ns::Array::from_slice(&[ns::Number::with_u32(p.0.0).as_ref()]),
        Err(_) => ns::Array::new(),
    }
}

pub fn start(sink: Sender<AudioChunk>) -> Result<Box<dyn CaptureStream>> {
    let (ready_tx, ready_rx) = mpsc::channel::<Result<String>>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    // All Core Audio objects live and die on this thread.
    let thread = std::thread::Builder::new().name("stenly-system-tap".into()).spawn(move || {
        let setup = || -> Result<_> {
            let output = ca::System::default_output_device().map_err(os_err("default output device"))?;
            let output_name = output.name().map(|n| n.to_string()).unwrap_or_else(|_| "output".into());

            let mut desc = ca::TapDesc::with_stereo_global_tap_excluding_processes(&self_exclusion());
            desc.set_name(Some(ns::str!(c"stenly system audio")));
            desc.set_private(true);
            let tap = desc.create_process_tap().map_err(os_err("creating process tap (check System Settings → Privacy → Screen & System Audio Recording)"))?;
            let tap_uid = tap.uid().map_err(os_err("tap uid"))?;
            let asbd = tap.asbd().map_err(os_err("tap format"))?;

            let sub_tap = cf::DictionaryOf::with_keys_values(
                &[ca::sub_device_keys::uid(), ca::sub_device_keys::drift_compensation()],
                &[tap_uid.as_type_ref(), cf::Boolean::value_true()],
            );
            let agg_uid = cf::Uuid::new().to_cf_string();
            // The aggregate holds only the tap, so the tap drives its own clock.
            // Clocking it from the default output device (as before) captured
            // nothing whenever that device wasn't running: e.g. the default
            // output was BlackHole while Zoom played to the speakers — a whole
            // 100-minute call recorded the far side as silence.
            let dict = cf::DictionaryOf::with_keys_values(
                &[
                    ca::aggregate_device_keys::is_private(),
                    ca::aggregate_device_keys::tap_auto_start(),
                    ca::aggregate_device_keys::name(),
                    ca::aggregate_device_keys::uid(),
                    ca::aggregate_device_keys::tap_list(),
                ],
                &[
                    cf::Boolean::value_true().as_type_ref(),
                    cf::Boolean::value_true(),
                    cf::str!(c"stenly tap"),
                    &agg_uid,
                    &cf::ArrayOf::from_slice(&[sub_tap.as_ref()]),
                ],
            );
            let agg = ca::AggregateDevice::with_desc(&dict).map_err(os_err("creating aggregate device"))?;
            let desc = format!(
                "system audio tap, all outputs; default {output_name} ({} Hz, {} ch)",
                asbd.sample_rate, asbd.channels_per_frame
            );
            Ok((tap, agg, asbd, desc))
        };
        let (tap, agg, asbd, desc) = match setup() {
            Ok(v) => v,
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        if !asbd.format_flags.contains(cat::AudioFormatFlags::IS_FLOAT) || asbd.bits_per_channel != 32 {
            let _ = ready_tx.send(Err(anyhow!("unexpected tap format {asbd:?}")));
            return;
        }
        let mut ctx = Box::new(Ctx {
            sink,
            channels: asbd.channels_per_frame as u16,
            interleaved: asbd.is_interleaved(),
            clock: RateClock::new(asbd.sample_rate as u32),
        });
        let proc_id = match agg.create_io_proc_id(io_proc, Some(&mut *ctx)) {
            Ok(id) => id,
            Err(e) => {
                let _ = ready_tx.send(Err(anyhow!("io proc: {e:?}")));
                return;
            }
        };
        let started = match ca::device_start(agg, Some(proc_id)) {
            Ok(s) => s,
            Err(e) => {
                let _ = ready_tx.send(Err(anyhow!("starting tap device: {e:?}")));
                return;
            }
        };
        let _ = ready_tx.send(Ok(desc));
        let _ = stop_rx.recv();
        drop(started); // AudioDeviceStop + destroy io proc
        drop(tap); // AudioHardwareDestroyProcessTap
        drop(ctx);
    })?;
    let desc = ready_rx.recv().context("tap thread died")??;
    Ok(Box::new(TapStream { desc, stop: Some(stop_tx), thread: Some(thread) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_clock_follows_device_rate_change() {
        let ticks_per_sec = 1.0 / host_secs(1);
        let mut c = RateClock::new(48_000);
        let (mut st, mut t) = (0.0f64, 1.0f64);
        let mut step = |c: &mut RateClock, rate: f64| {
            let r = c.observe(st, (t * ticks_per_sec) as u64);
            st += 512.0;
            t += 512.0 / rate;
            r
        };
        for _ in 0..60 {
            assert_eq!(step(&mut c, 48_000.0), 48_000);
        }
        let mut last = 0;
        for _ in 0..60 {
            last = step(&mut c, 16_000.0);
        }
        assert_eq!(last, 16_000, "AirPods call profile");
        for _ in 0..200 {
            last = step(&mut c, 48_000.0);
        }
        assert_eq!(last, 48_000, "back to music profile");
    }
}
