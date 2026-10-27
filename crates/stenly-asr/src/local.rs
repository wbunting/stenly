//! Inference implementation. Only the stenly-speech worker constructs this type.
use crate::{models, speakers, stream};
use anyhow::{Context, Result};
use sherpa_onnx::{
    OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
    SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};
use stenly_core::{
    config::AsrConfig,
    platform::Channel,
    transcript::{Segment, Transcriber},
};
const RATE: i32 = 16_000;
const VAD_WINDOW: usize = 512;
static DECODING: std::sync::Mutex<()> = std::sync::Mutex::new(());
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
pub struct ParakeetTranscriber {
    recognizer: OfflineRecognizer,
    vad_cfg: VadModelConfig,
    pub(crate) models_dir: std::path::PathBuf,
}

fn s(p: &Path) -> Option<String> {
    Some(p.to_string_lossy().into_owned())
}

impl ParakeetTranscriber {
    pub fn new(cfg: &AsrConfig, models_dir: &Path) -> Result<Self> {
        let spec = models::find(&cfg.model)
            .with_context(|| format!("unknown asr.model {:?}", cfg.model))?;
        let dir = models::ensure(spec, models_dir, &mut |_| {})?;
        let vad = models::ensure_vad(models_dir)?;

        let mut rc = OfflineRecognizerConfig::default();
        rc.model_config = OfflineModelConfig {
            transducer: OfflineTransducerModelConfig {
                encoder: s(&dir.join(spec.encoder)),
                decoder: s(&dir.join(spec.decoder)),
                joiner: s(&dir.join(spec.joiner)),
            },
            tokens: s(&dir.join("tokens.txt")),
            model_type: Some("nemo_transducer".into()),
            num_threads: cfg.threads.max(1),
            provider: Some(cfg.provider.clone()),
            ..Default::default()
        };
        rc.decoding_method = Some("greedy_search".into());
        let recognizer = OfflineRecognizer::create(&rc)
            .with_context(|| format!("loading {} with provider {}", spec.id, cfg.provider))?;

        let vad_cfg = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: s(&vad),
                threshold: 0.45,
                min_silence_duration: 0.4,
                min_speech_duration: 0.25,
                window_size: VAD_WINDOW as i32,
                // Parakeet handles long inputs fine; cap segments to keep latency bounded.
                max_speech_duration: 25.0,
            },
            sample_rate: RATE,
            num_threads: 1,
            provider: Some("cpu".into()),
            ..Default::default()
        };
        Ok(Self {
            recognizer,
            vad_cfg,
            models_dir: models_dir.to_path_buf(),
        })
    }

    /// Transcribe a short clip (dictation) as one text. Clips up to ~20 s go
    /// through Parakeet in a single pass (best punctuation and casing);
    /// longer ones are cut at the quietest moment near each 20 s mark.
    pub fn transcribe_clip(&self, samples: &[f32]) -> String {
        const MAX: usize = 20 * RATE as usize;
        let samples = trim_silence(samples);
        if samples.len() < (RATE as usize) / 4 {
            return String::new();
        }
        let mut out: Vec<String> = Vec::new();
        let mut start = 0;
        while start < samples.len() {
            let end = if samples.len() - start <= MAX + RATE as usize * 3 {
                samples.len()
            } else {
                quietest_cut(samples, start + MAX - RATE as usize * 3, start + MAX)
            };
            let mut chunk = samples[start..end].to_vec();
            // a little trailing silence helps the decoder finish the last word
            chunk.extend(std::iter::repeat_n(0.0, RATE as usize / 3));
            let t = self.decode(&chunk);
            if !t.is_empty() {
                out.push(t);
            }
            start = end;
        }
        out.join(" ")
    }

    /// Load and warm the model (first decode allocates); call once at startup.
    pub fn warm_up(&self) {
        let _ = self.decode(&vec![0.0; RATE as usize / 2]);
    }

    fn decode(&self, samples: &[f32]) -> String {
        self.decode_result(samples)
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default()
    }

    /// Decode a clip into timed words (times relative to the clip start).
    pub fn decode_words(&self, samples: &[f32]) -> Vec<stream::Word> {
        let Some(r) = self.decode_result(samples) else {
            return vec![];
        };
        let Some(ts) = r.timestamps.as_deref() else {
            return vec![];
        };
        stream::group_words(&r.tokens, ts, r.durations.as_deref(), 0.0)
    }

    fn decode_result(&self, samples: &[f32]) -> Option<sherpa_onnx::OfflineRecognizerResult> {
        if SHUTTING_DOWN.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        let _busy = DECODING.lock();
        if SHUTTING_DOWN.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(RATE, samples);
        self.recognizer.decode(&stream);
        stream.get_result()
    }
}

/// Drop leading/trailing silence (keeps 150 ms of padding).
fn trim_silence(s: &[f32]) -> &[f32] {
    const WIN: usize = 320; // 20 ms
    let loud =
        |w: &[f32]| (w.iter().map(|x| x * x).sum::<f32>() / w.len().max(1) as f32).sqrt() > 0.004;
    let first = s.chunks(WIN).position(loud);
    let last = s.chunks(WIN).rposition(loud);
    match (first, last) {
        (Some(f), Some(l)) => {
            let pad = RATE as usize * 15 / 100;
            let a = (f * WIN).saturating_sub(pad);
            let b = ((l + 1) * WIN + pad).min(s.len());
            &s[a..b]
        }
        _ => &s[..0],
    }
}

/// Index of the quietest 100 ms window between `from` and `to`.
fn quietest_cut(s: &[f32], from: usize, to: usize) -> usize {
    const WIN: usize = 1600;
    let to = to.min(s.len());
    let mut best = (f32::MAX, to);
    let mut i = from;
    while i + WIN <= to {
        let e: f32 = s[i..i + WIN].iter().map(|x| x * x).sum();
        if e < best.0 {
            best = (e, i + WIN / 2);
        }
        i += WIN / 2;
    }
    best.1
}

impl Transcriber for ParakeetTranscriber {
    fn transcribe(
        &mut self,
        samples: &[f32],
        channel: Channel,
        progress: &mut dyn FnMut(f32),
    ) -> Result<Vec<Segment>> {
        let vad = VoiceActivityDetector::create(&self.vad_cfg, 60.0).context("creating VAD")?;
        let mut out = Vec::new();
        let total = samples.len().max(1);
        let emit = |vad: &VoiceActivityDetector, out: &mut Vec<Segment>| {
            while let Some(seg) = vad.front() {
                let start = seg.start() as f64 / RATE as f64;
                let audio = seg.samples();
                let end = start + audio.len() as f64 / RATE as f64;
                // pad a little so word onsets/offsets aren't clipped
                let text = self.decode(audio);
                if !text.is_empty() {
                    out.push(Segment::new(channel, start, end, text));
                }
                vad.pop();
            }
        };
        for (i, window) in samples.chunks(VAD_WINDOW).enumerate() {
            if SHUTTING_DOWN.load(std::sync::atomic::Ordering::SeqCst) {
                anyhow::bail!("shutting down; will resume on next launch");
            }
            vad.accept_waveform(window);
            if vad.detected() || !vad.is_empty() {
                emit(&vad, &mut out);
            }
            if i % 2000 == 0 {
                progress((i * VAD_WINDOW) as f32 / total as f32);
            }
        }
        vad.flush();
        emit(&vad, &mut out);
        progress(1.0);
        Ok(out)
    }

    fn embed_segments(&mut self, segs: &[Segment], audio: &[f32]) -> Result<Vec<Option<Vec<f32>>>> {
        speakers::embed(segs, audio, &self.models_dir)
    }

    fn words(&mut self, seg: &Segment, audio: &[f32]) -> Vec<stenly_core::transcript::TimedWord> {
        let a = ((seg.start * RATE as f64) as usize).min(audio.len());
        let b = ((seg.end * RATE as f64) as usize).min(audio.len());
        if b <= a {
            return Vec::new();
        }
        let mut clip = audio[a..b].to_vec();
        clip.extend(std::iter::repeat_n(0.0, RATE as usize / 3));
        self.decode_words(&clip)
            .into_iter()
            .map(|w| stenly_core::transcript::TimedWord {
                text: w.text,
                start: seg.start + w.start as f64,
                end: seg.start + w.end as f64,
            })
            .collect()
    }
}

/// Incremental transcription of one channel while it's being recorded.
///
/// Feed 16 kHz mono audio as it arrives; Silero VAD cuts speech segments and
/// Parakeet decodes each one as soon as the speaker pauses, so by the time the
/// meeting ends only the last unfinished phrase is left to transcribe.
pub struct LiveChannel {
    asr: Arc<ParakeetTranscriber>,
    vad: VoiceActivityDetector,
    channel: Channel,
    pending: Vec<f32>,
}

impl ParakeetTranscriber {
    pub fn live(self: &Arc<Self>, channel: Channel) -> Result<LiveChannel> {
        Ok(LiveChannel {
            asr: self.clone(),
            vad: VoiceActivityDetector::create(&self.vad_cfg, 60.0).context("creating VAD")?,
            channel,
            pending: Vec::new(),
        })
    }
}

impl LiveChannel {
    /// Push audio; returns any segments finished by it.
    pub fn push(&mut self, samples: &[f32]) -> Vec<Segment> {
        self.pending.extend_from_slice(samples);
        let mut out = Vec::new();
        let full = self.pending.len() / VAD_WINDOW * VAD_WINDOW;
        let ready: Vec<f32> = self.pending.drain(..full).collect();
        for window in ready.chunks(VAD_WINDOW) {
            self.vad.accept_waveform(window);
            if self.vad.detected() || !self.vad.is_empty() {
                self.drain(&mut out);
            }
        }
        out
    }

    /// End of the recording: flush the last partial segment.
    pub fn finish(mut self) -> Vec<Segment> {
        let mut out = Vec::new();
        if !self.pending.is_empty() {
            let mut last = std::mem::take(&mut self.pending);
            last.resize(VAD_WINDOW, 0.0);
            self.vad.accept_waveform(&last);
        }
        self.vad.flush();
        self.drain(&mut out);
        out
    }

    fn drain(&mut self, out: &mut Vec<Segment>) {
        while let Some(seg) = self.vad.front() {
            let start = seg.start() as f64 / RATE as f64;
            let audio = seg.samples();
            let end = start + audio.len() as f64 / RATE as f64;
            let text = self.asr.decode(audio);
            if !text.is_empty() {
                out.push(Segment::new(self.channel, start, end, text));
            }
            self.vad.pop();
        }
    }
}
