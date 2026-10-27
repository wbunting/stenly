//! Voice embeddings for transcript segments (NeMo TitaNet-small via
//! sherpa-onnx). Grouping them into speakers and matching them to saved
//! voiceprints is plain vector math in `stenly_core::voices`.
//!
//! One embedding per transcript segment (not per pyannote frame) keeps labels
//! aligned with the text: ~1 s for a 100-minute meeting's ~1,500 segments on
//! an M-series Mac.

use anyhow::{Context, Result};
use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};
use std::path::{Path, PathBuf};
use stenly_core::transcript::Segment;
use stenly_core::voices::{MIN_EMBED_SECS, normalize};

const EMBED_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/nemo_en_titanet_small.onnx";
const EMBED_FILE: &str = "nemo_en_titanet_small.onnx";
const RATE: i32 = 16_000;

pub fn ensure_model(models_dir: &Path) -> Result<PathBuf> {
    let p = models_dir.join(EMBED_FILE);
    if !p.is_file() {
        std::fs::create_dir_all(models_dir)?;
        crate::models::download_to(EMBED_URL, &p)?;
    }
    Ok(p)
}

/// One unit-length voice embedding per segment of one channel (`audio` is
/// that channel's 16 kHz samples). Segments too short to carry a voice get None.
pub fn embed(segs: &[Segment], audio: &[f32], models_dir: &Path) -> Result<Vec<Option<Vec<f32>>>> {
    Ok(Extractor::new(models_dir)?.embed(segs, audio))
}

/// Cached only inside the speech process across bounded embedding batches.
pub(crate) struct Extractor(SpeakerEmbeddingExtractor);
impl Extractor {
    pub(crate) fn new(models_dir: &Path) -> Result<Self> {
        let model = ensure_model(models_dir)?;
        Ok(Self(
            SpeakerEmbeddingExtractor::create(&SpeakerEmbeddingExtractorConfig {
                model: Some(model.to_string_lossy().into_owned()),
                num_threads: 4,
                debug: false,
                provider: Some("cpu".into()),
            })
            .context("loading speaker model")?,
        ))
    }
    pub(crate) fn embed(&self, segs: &[Segment], audio: &[f32]) -> Vec<Option<Vec<f32>>> {
        segs.iter().map(|s| embed_one(&self.0, s, audio)).collect()
    }
}

fn embed_one(ex: &SpeakerEmbeddingExtractor, s: &Segment, audio: &[f32]) -> Option<Vec<f32>> {
    if s.end - s.start < MIN_EMBED_SECS {
        return None;
    }
    let a = (s.start * RATE as f64) as usize;
    let b = ((s.end * RATE as f64) as usize).min(audio.len());
    if a >= b || b - a < (MIN_EMBED_SECS * RATE as f64) as usize / 2 {
        return None;
    }
    let stream = ex.create_stream()?;
    stream.accept_waveform(RATE, &audio[a..b]);
    stream.input_finished();
    if !ex.is_ready(&stream) {
        return None;
    }
    ex.compute(&stream).map(normalize)
}
