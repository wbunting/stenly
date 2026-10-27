//! Model catalog + first-run download into the cache dir.

use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub struct ModelSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub language: &'static str,
    /// 1–5 dots (and the x/10 rating shown next to them).
    pub speed: f32,
    pub accuracy: f32,
    pub description: &'static str,
    /// Individual files on Hugging Face (resumable, no tar/bzip2 dependency).
    pub base_url: &'static str,
    pub encoder: &'static str,
    pub decoder: &'static str,
    pub joiner: &'static str,
    pub approx_mb: u32,
}

pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "parakeet-tdt-0.6b-v2-int8",
        name: "Parakeet V2",
        language: "English",
        speed: 9.9,
        accuracy: 9.4,
        description: "NVIDIA's Parakeet V2, optimized for lightning-fast English transcription.",
        base_url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8/resolve/main",
        encoder: "encoder.int8.onnx",
        decoder: "decoder.int8.onnx",
        joiner: "joiner.int8.onnx",
        approx_mb: 660,
    },
    ModelSpec {
        id: "parakeet-tdt-0.6b-v3-int8",
        name: "Parakeet V3",
        language: "Multilingual",
        speed: 9.9,
        accuracy: 9.4,
        description: "Parakeet V3 with English and 25 European languages.",
        base_url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/resolve/main",
        encoder: "encoder.int8.onnx",
        decoder: "decoder.int8.onnx",
        joiner: "joiner.int8.onnx",
        approx_mb: 670,
    },
];

/// Silero VAD v5. v4 (`silero_vad.onnx`) badly under-detects speech that has
/// been through a call codec (Zoom/Meet far-end audio): on a real 98-min call
/// it marked <5% of the remote side as speech, so almost none of it was
/// transcribed. v5 scores the same audio ~0.8.
const VAD_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad_v5.onnx";
const VAD_FILE: &str = "silero_vad_v5.onnx";

pub fn find(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.id == id)
}

/// Directory name used by the upstream sherpa-onnx release tarballs, so a
/// manually extracted archive is picked up too.
fn dir_for(spec: &ModelSpec, root: &Path) -> PathBuf {
    root.join(format!("sherpa-onnx-nemo-{}", spec.id))
}

pub fn is_installed(spec: &ModelSpec, root: &Path) -> bool {
    let d = dir_for(spec, root);
    [spec.encoder, spec.decoder, spec.joiner, "tokens.txt"].iter().all(|f| d.join(f).is_file())
}

fn download(url: &str, dest: &Path, on_progress: &mut dyn FnMut(f32)) -> Result<()> {
    let tmp = dest.with_extension("part");
    tracing::info!(%url, "downloading");
    let resp = ureq::get(url).call().with_context(|| format!("GET {url}"))?;
    let total: Option<u64> = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
    let mut reader = resp.into_body().into_reader();
    let mut file = std::fs::File::create(&tmp)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut done: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        if let Some(t) = total {
            on_progress(done as f32 / t as f32);
        }
    }
    file.sync_all()?;
    if let Some(t) = total
        && done != t {
            bail!("short download for {url}: {done}/{t} bytes");
        }
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// Make sure the model files exist, downloading them if needed. Returns the model dir.
pub fn ensure(spec: &ModelSpec, root: &Path, on_progress: &mut dyn FnMut(f32)) -> Result<PathBuf> {
    let dir = dir_for(spec, root);
    if is_installed(spec, root) {
        return Ok(dir);
    }
    std::fs::create_dir_all(&dir)?;
    let files = [spec.encoder, spec.decoder, spec.joiner, "tokens.txt"];
    for (i, f) in files.iter().enumerate() {
        let dest = dir.join(f);
        if dest.is_file() {
            continue;
        }
        let base = i as f32 / files.len() as f32;
        download(&format!("{}/{f}", spec.base_url), &dest, &mut |p| on_progress(base + p / files.len() as f32))?;
    }
    Ok(dir)
}

pub fn remove(spec: &ModelSpec, root: &Path) -> Result<()> {
    let d = dir_for(spec, root);
    if d.exists() {
        std::fs::remove_dir_all(d)?;
    }
    Ok(())
}

/// Download a single file (resumable via .part rename).
pub fn download_to(url: &str, dest: &Path) -> Result<()> {
    download(url, dest, &mut |_| {})
}

pub fn ensure_vad(root: &Path) -> Result<PathBuf> {
    // v4 model from older versions; no longer used
    let _ = std::fs::remove_file(root.join("silero_vad.onnx"));
    let p = root.join(VAD_FILE);
    if !p.is_file() {
        std::fs::create_dir_all(root)?;
        download(VAD_URL, &p, &mut |_| {})?;
    }
    Ok(p)
}
