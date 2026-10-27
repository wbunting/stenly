//! Tiny synthesized UI sounds (dictation start / stop / cancel), played
//! through the default output with cpal. Synthesized so there are no audio
//! assets to license, and identical on every platform.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chime {
    /// Two rising notes: listening.
    Start,
    /// Two falling notes: got it.
    Stop,
    /// One low soft note: cancelled / nothing heard.
    Cancel,
}

/// Mono samples at `rate`: soft sine tones with a fast attack and an
/// exponential tail, about −20 dBFS so it never startles.
fn render(chime: Chime, rate: u32) -> Vec<f32> {
    let notes: &[(f32, f32, f32)] = match chime {
        // (start s, freq Hz, length s)
        Chime::Start => &[(0.0, 880.0, 0.09), (0.075, 1318.5, 0.16)],
        Chime::Stop => &[(0.0, 1318.5, 0.09), (0.075, 880.0, 0.16)],
        Chime::Cancel => &[(0.0, 523.25, 0.18)],
    };
    let total = notes.iter().map(|(s, _, l)| s + l).fold(0.0f32, f32::max) + 0.02;
    let n = (total * rate as f32) as usize;
    let mut out = vec![0.0f32; n];
    for &(start, f, len) in notes {
        let s0 = (start * rate as f32) as usize;
        let ln = (len * rate as f32) as usize;
        for i in 0..ln {
            let t = i as f32 / rate as f32;
            let attack = (t / 0.006).min(1.0);
            let decay = (-t / (len * 0.35)).exp();
            // a touch of the octave makes it bell-like rather than a test tone
            let v = (std::f32::consts::TAU * f * t).sin() + 0.25 * (std::f32::consts::TAU * 2.0 * f * t).sin();
            if s0 + i < n {
                out[s0 + i] += 0.08 * attack * decay * v;
            }
        }
    }
    out
}

/// Fire and forget.
pub fn play(chime: Chime) {
    std::thread::Builder::new()
        .name("stenly-chime".into())
        .spawn(move || {
            if let Err(e) = play_blocking(chime) {
                tracing::debug!(error = %format!("{e:#}"), "chime failed");
            }
        })
        .ok();
}

fn play_blocking(chime: Chime) -> anyhow::Result<()> {
    let host = cpal::default_host();
    let dev = host.default_output_device().ok_or_else(|| anyhow::anyhow!("no output device"))?;
    let cfg = dev.default_output_config()?;
    let rate = cfg.sample_rate();
    let channels = cfg.channels() as usize;
    let samples = render(chime, rate);
    let len = samples.len();
    let pos = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let p = pos.clone();
    let stream_cfg: cpal::StreamConfig = cfg.clone().into();
    let stream = match cfg.sample_format() {
        cpal::SampleFormat::F32 => dev.build_output_stream::<f32, _, _>(
            stream_cfg,
            move |out: &mut [f32], _| {
                for frame in out.chunks_mut(channels) {
                    let i = p.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let v = samples.get(i).copied().unwrap_or(0.0);
                    frame.iter_mut().for_each(|s| *s = v);
                }
            },
            |e| tracing::debug!(error = %e, "chime stream"),
            None,
        )?,
        cpal::SampleFormat::I16 => dev.build_output_stream::<i16, _, _>(
            stream_cfg,
            move |out: &mut [i16], _| {
                for frame in out.chunks_mut(channels) {
                    let i = p.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let v = samples.get(i).copied().unwrap_or(0.0);
                    frame.iter_mut().for_each(|s| *s = (v * i16::MAX as f32) as i16);
                }
            },
            |e| tracing::debug!(error = %e, "chime stream"),
            None,
        )?,
        f => anyhow::bail!("unsupported output format {f:?}"),
    };
    stream.play()?;
    let secs = len as f32 / rate as f32 + 0.05;
    std::thread::sleep(std::time::Duration::from_secs_f32(secs));
    drop(stream);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_quietly() {
        for c in [Chime::Start, Chime::Stop, Chime::Cancel] {
            let s = render(c, 48_000);
            assert!(s.len() > 4_000 && s.len() < 20_000);
            let peak = s.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(peak > 0.03 && peak < 0.2, "{peak}");
        }
    }
}
