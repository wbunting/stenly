//! Speech recognition client. Model weights and inference live in stenly-speech,
//! never in the GUI or CLI process. Microphone capture stays in the parent.
pub mod models;
pub mod speakers;
pub mod stream;
mod local;
mod worker;
pub use worker::{ParakeetTranscriber, SharedTranscriber, DICTATION_ASR, run_worker, request_shutdown, shutdown_and_wait};
use anyhow::Result;
use stenly_core::{config::AsrConfig, platform::Channel, transcript::Segment};
use std::path::Path;

/// Transcribes a recording while it happens. Audio arrives on a channel from
/// the session writers; finished segments are appended to `<dir>/live.json`
/// (so a crash keeps them). Call [`LiveSession::finish`] after the recording
/// stops: it flushes the last phrase and returns the whole transcript.
pub struct LiveSession {
    tx: Option<crossbeam_channel::Sender<(Channel, Vec<f32>)>>,
    thread: Option<std::thread::JoinHandle<Result<Vec<Segment>>>>,
    pub segments: std::sync::Arc<std::sync::Mutex<Vec<Segment>>>,
}

pub const LIVE_FILE: &str = "live.json";

impl LiveSession {
    pub fn start(cfg: &AsrConfig, models_dir: &Path, dir: &Path) -> Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded::<(Channel, Vec<f32>)>();
        let mut s = Self::start_from(cfg, models_dir, dir, rx)?;
        s.tx = Some(tx);
        Ok(s)
    }

    /// Start on an existing receiver (the recording owns the senders; the
    /// worker ends when they're all dropped).
    pub fn start_from(cfg: &AsrConfig, models_dir: &Path, dir: &Path, rx: crossbeam_channel::Receiver<(Channel, Vec<f32>)>) -> Result<Self> {
        let segments = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Segment>::new()));
        let (cfg, models_dir, dir, segs) = (cfg.clone(), models_dir.to_path_buf(), dir.to_path_buf(), segments.clone());
        let thread = std::thread::Builder::new().name("stenly-live-asr".into()).spawn(move || -> Result<Vec<Segment>> {
            let asr = ParakeetTranscriber::new(&cfg, &models_dir)?;
            let mut mic = asr.live(Channel::Mic)?;
            let mut sys = asr.live(Channel::System)?;
            // Serialize under the lock, write after releasing it: the UI locks
            // `segments` to draw the live transcript, and a write into iCloud
            // Drive can block for many seconds.
            let save = |segs: &std::sync::Mutex<Vec<Segment>>| {
                let bytes = segs.lock().ok().and_then(|s| serde_json::to_vec(&*s).ok());
                if let Some(b) = bytes {
                    let _ = std::fs::write(dir.join(LIVE_FILE), b);
                }
            };
            // Ends when every sender is dropped (recording stopped).
            for (ch, audio) in rx.iter() {
                let new = match ch {
                    Channel::Mic => mic.push(&audio),
                    Channel::System => sys.push(&audio),
                };
                asr.check_health()?;
                if !new.is_empty() {
                    segs.lock().map(|mut s| s.extend(new)).ok();
                    save(&segs);
                }
            }
            let tail: Vec<Segment> = mic.finish().into_iter().chain(sys.finish()).collect();
            asr.check_health()?;
            segs.lock().map(|mut s| s.extend(tail)).ok();
            save(&segs);
            Ok(segs.lock().map(|s| s.clone()).unwrap_or_default())
        })?;
        Ok(Self { tx: None, thread: Some(thread), segments })
    }


    /// Wait for the backlog to drain and the last phrases to decode.
    pub fn finish(mut self) -> Result<Vec<Segment>> {
        drop(self.tx.take());
        self.thread.take().map(|t| t.join().map_err(|_| anyhow::anyhow!("live transcriber panicked"))?).unwrap_or_else(|| Ok(vec![]))
    }
}

/// Record that the live transcript covers the whole recording, so processing
/// can skip transcription.
pub fn mark_live_complete(dir: &Path, segs: &[Segment]) -> Result<()> {
    std::fs::write(dir.join("live.complete.json"), serde_json::to_vec(segs)?)?;
    let _ = std::fs::remove_file(dir.join(LIVE_FILE));
    Ok(())
}
