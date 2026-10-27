//! Summary-model catalog + a tiny Ollama client (status, installed models, pull
//! with progress, delete). The catalog is curated for meeting summaries.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::io::{BufRead, BufReader};

#[derive(Debug, Clone, Copy)]
pub struct LlmSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub vendor: &'static str,
    pub size_gb: f32,
    /// Peak RAM while summarizing (approx.).
    pub ram_gb: f32,
    /// 1–5 dots.
    pub speed: u8,
    pub quality: u8,
    pub blurb: &'static str,
    pub recommended: bool,
}

pub const CATALOG: &[LlmSpec] = &[
    LlmSpec {
        id: "qwen3.5:9b",
        name: "Qwen3.5 9B",
        vendor: "Alibaba",
        size_gb: 6.6,
        ram_gb: 9.0,
        speed: 4,
        quality: 4,
        blurb: "Best on-device notes. Needs 16 GB RAM.",
        recommended: true,
    },
    LlmSpec {
        id: "qwen3:4b",
        name: "Qwen3 4B",
        vendor: "Alibaba",
        size_gb: 2.5,
        ram_gb: 3.5,
        speed: 5,
        quality: 4,
        blurb: "Small and fast. Misses a lot on long calls.",
        recommended: false,
    },
    LlmSpec {
        id: "gemma3:4b",
        name: "Gemma 3 4B",
        vendor: "Google",
        size_gb: 3.3,
        ram_gb: 4.5,
        speed: 4,
        quality: 4,
        blurb: "Readable, but drops and garbles facts.",
        recommended: false,
    },
    LlmSpec {
        id: "llama3.2:3b",
        name: "Llama 3.2 3B",
        vendor: "Meta",
        size_gb: 2.0,
        ram_gb: 3.0,
        speed: 5,
        quality: 3,
        blurb: "Smallest and fastest. Can fail on long calls.",
        recommended: false,
    },
    LlmSpec {
        id: "qwen3:8b",
        name: "Qwen3 8B",
        vendor: "Alibaba",
        size_gb: 5.2,
        ram_gb: 7.0,
        speed: 4,
        quality: 5,
        blurb: "Bigger Qwen3. Needs 16 GB RAM.",
        recommended: false,
    },
    LlmSpec {
        id: "nemotron-3-nano",
        name: "Nemotron 3 Nano",
        vendor: "NVIDIA",
        size_gb: 24.0,
        ram_gb: 18.0,
        speed: 3,
        quality: 5,
        blurb: "30B mixture-of-experts. Holds ~18 GB.",
        recommended: false,
    },
];

pub fn find(id: &str) -> Option<&'static LlmSpec> {
    let base = id.strip_suffix(":latest").unwrap_or(id);
    CATALOG.iter().find(|m| m.id == base || m.id.strip_suffix(":latest") == Some(base))
}

/// `http://localhost:11434/v1` → `http://localhost:11434`
pub fn api_base(endpoint: &str) -> String {
    let e = endpoint.trim().trim_end_matches('/');
    e.strip_suffix("/v1").unwrap_or(e).to_string()
}

pub fn is_running(endpoint: &str) -> bool {
    ureq::get(&format!("{}/api/version", api_base(endpoint)))
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(2)))
        .build()
        .call()
        .is_ok()
}

/// Is an OpenAI-compatible endpoint reachable? Any HTTP response means the
/// server answered; authentication and model validation belong to the summary
/// request, while connection failures should let the pipeline save its note.
pub fn endpoint_reachable(endpoint: &str) -> bool {
    match ureq::get(&format!("{}/models", endpoint.trim_end_matches('/')))
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(2)))
        .build()
        .call()
    {
        Ok(_) | Err(ureq::Error::StatusCode(_)) => true,
        Err(_) => false,
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstalledModel {
    pub name: String,
    pub size: u64,
}

pub fn installed(endpoint: &str) -> Result<Vec<InstalledModel>> {
    #[derive(Deserialize)]
    struct Tags {
        models: Vec<InstalledModel>,
    }
    let t: Tags = ureq::get(&format!("{}/api/tags", api_base(endpoint)))
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(5)))
        .build()
        .call()?
        .body_mut()
        .read_json()?;
    Ok(t.models)
}

pub fn same_model(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.strip_suffix(":latest").unwrap_or(s).to_string();
    norm(a) == norm(b)
}

/// Pull a model, reporting progress 0..1. Blocking.
pub fn pull(endpoint: &str, model: &str, on_progress: &mut dyn FnMut(f32, &str)) -> Result<()> {
    let resp = ureq::post(&format!("{}/api/pull", api_base(endpoint)))
        .config()
        .timeout_global(None)
        .build()
        .send_json(serde_json::json!({ "model": model, "stream": true }))
        .context("Ollama isn't reachable")?;
    let reader = BufReader::new(resp.into_body().into_reader());
    for line in reader.lines() {
        let line = line?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if let Some(err) = v["error"].as_str() {
            bail!("{err}");
        }
        let status = v["status"].as_str().unwrap_or_default();
        let frac = match (v["completed"].as_f64(), v["total"].as_f64()) {
            (Some(c), Some(t)) if t > 0.0 => (c / t) as f32,
            _ => -1.0,
        };
        on_progress(frac, status);
        if status == "success" {
            return Ok(());
        }
    }
    Ok(())
}

pub fn delete(endpoint: &str, model: &str) -> Result<()> {
    ureq::delete(&format!("{}/api/delete", api_base(endpoint)))
        .force_send_body()
        .send_json(serde_json::json!({ "model": model }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn openai_endpoint_is_reachable_even_when_models_is_rejected() {
        let server = crate::destinations::testutil::MockServer::start(|_| (404, "{}".into()));
        assert!(super::endpoint_reachable(&format!("{}/v1", server.url)));
        assert!(!super::endpoint_reachable("http://127.0.0.1:9/v1"));
    }
}
