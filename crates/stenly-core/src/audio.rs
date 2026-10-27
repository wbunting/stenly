//! Audio plumbing shared by every backend: chunks in, 16 kHz mono WAV out.

use anyhow::{Context, Result};
use std::path::Path;

pub const TARGET_RATE: u32 = 16_000;

/// Interleaved f32 samples straight from a capture backend.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    pub samples: Vec<f32>,
    pub channels: u16,
    pub sample_rate: u32,
}

/// Streaming mono downmix + windowed-sinc resampler to [`TARGET_RATE`].
/// Handles any input rate and rate changes mid-stream (device switches).
pub struct Resampler {
    in_rate: u32,
    /// Fractional read position into `hist`, in input samples.
    pos: f64,
    hist: Vec<f32>,
    cutoff: f64,
}

const HALF_TAPS: usize = 16;

impl Default for Resampler {
    fn default() -> Self {
        Self { in_rate: 0, pos: HALF_TAPS as f64, hist: vec![0.0; HALF_TAPS], cutoff: 1.0 }
    }
}

impl Resampler {
    fn reset(&mut self, rate: u32) {
        self.in_rate = rate;
        self.pos = HALF_TAPS as f64;
        self.hist = vec![0.0; HALF_TAPS];
        // Low-pass a little under the output Nyquist when downsampling.
        self.cutoff = (TARGET_RATE as f64 / rate as f64).min(1.0) * 0.92;
    }

    pub fn process(&mut self, chunk: &AudioChunk, out: &mut Vec<f32>) {
        if chunk.sample_rate == 0 || chunk.channels == 0 {
            return;
        }
        if chunk.sample_rate != self.in_rate {
            self.reset(chunk.sample_rate);
        }
        let ch = chunk.channels as usize;
        self.hist.extend(chunk.samples.chunks_exact(ch).map(|f| f.iter().sum::<f32>() / ch as f32));
        if self.in_rate == TARGET_RATE {
            out.extend_from_slice(&self.hist[HALF_TAPS..]);
            self.hist.drain(..self.hist.len() - HALF_TAPS);
            return;
        }
        let step = self.in_rate as f64 / TARGET_RATE as f64;
        let fc = self.cutoff;
        let span = (HALF_TAPS as f64 / fc).ceil() as isize;
        while self.pos + (span as f64) < self.hist.len() as f64 {
            let center = self.pos.floor() as isize;
            let frac = self.pos - center as f64;
            let mut acc = 0.0f64;
            let mut wsum = 0.0f64;
            for k in -span + 1..=span {
                let idx = center + k;
                if idx < 0 {
                    continue;
                }
                let x = k as f64 - frac;
                let t = x * fc;
                let sinc = if t.abs() < 1e-9 { 1.0 } else { (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t) };
                // Hann window over the kernel span
                let w = 0.5 + 0.5 * (std::f64::consts::PI * x / span as f64).cos();
                let c = sinc * w;
                acc += c * self.hist[idx as usize] as f64;
                wsum += c;
            }
            out.push(if wsum.abs() > 1e-9 { (acc / wsum) as f32 } else { 0.0 });
            self.pos += step;
        }
        // keep enough history for the next call
        let keep_from = (self.pos.floor() as isize - span).max(0) as usize;
        self.hist.drain(..keep_from);
        self.pos -= keep_from as f64;
    }
}

/// Root-mean-square level of a block, in dBFS.
pub fn rms_db(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return -120.0;
    }
    let ms = samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32;
    10.0 * ms.max(1e-12).log10()
}

/// Incrementally written 16 kHz mono 16-bit WAV. The header is patched on
/// every flush so a crash leaves a playable / transcribable file.
pub struct WavSink {
    writer: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
    samples_written: u64,
    since_flush: usize,
}

impl WavSink {
    pub fn create(path: &Path) -> Result<Self> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: TARGET_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(path, spec).with_context(|| format!("creating {}", path.display()))?;
        Ok(Self { writer, samples_written: 0, since_flush: 0 })
    }

    pub fn write(&mut self, samples: &[f32]) -> Result<()> {
        for &s in samples {
            self.writer.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
        }
        self.samples_written += samples.len() as u64;
        self.since_flush += samples.len();
        if self.since_flush >= TARGET_RATE as usize * 5 {
            self.writer.flush()?;
            self.since_flush = 0;
        }
        Ok(())
    }

    pub fn duration_secs(&self) -> f64 {
        self.samples_written as f64 / TARGET_RATE as f64
    }

    pub fn finish(self) -> Result<()> {
        self.writer.finalize()?;
        Ok(())
    }
}

/// Load a WAV as 16 kHz mono f32 (resampling / downmixing if needed).
pub fn read_wav_16k_mono(path: &Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().filter_map(Result::ok).collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().filter_map(Result::ok).map(|s| s as f32 * scale).collect()
        }
    };
    if spec.sample_rate == TARGET_RATE && spec.channels == 1 {
        return Ok(samples);
    }
    let mut rs = Resampler::default();
    let mut out = Vec::new();
    rs.process(&AudioChunk { samples, channels: spec.channels, sample_rate: spec.sample_rate }, &mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f32, secs: f32, ch: u16) -> AudioChunk {
        let n = (rate as f32 * secs) as usize;
        let mut samples = Vec::with_capacity(n * ch as usize);
        for i in 0..n {
            let v = (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin() * 0.5;
            for _ in 0..ch {
                samples.push(v);
            }
        }
        AudioChunk { samples, channels: ch, sample_rate: rate }
    }

    #[test]
    fn resamples_48k_stereo_in_pieces() {
        let full = sine(48_000, 440.0, 1.0, 2);
        let mut rs = Resampler::default();
        let mut out = Vec::new();
        for piece in full.samples.chunks(2 * 512) {
            rs.process(&AudioChunk { samples: piece.to_vec(), channels: 2, sample_rate: 48_000 }, &mut out);
        }
        assert!((out.len() as i64 - 16_000).abs() < 40, "{}", out.len());
        // amplitude preserved, no aliasing blowups
        let peak = out[2000..14000].iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.03, "{peak}");
    }

    #[test]
    fn rejects_above_nyquist() {
        // 12 kHz tone at 44.1k must be removed when going to 16k (Nyquist 8k).
        let mut rs = Resampler::default();
        let mut out = Vec::new();
        rs.process(&sine(44_100, 12_000.0, 0.5, 1), &mut out);
        let level = rms_db(&out[1000..out.len() - 1000]);
        assert!(level < -30.0, "{level}");
    }
}
