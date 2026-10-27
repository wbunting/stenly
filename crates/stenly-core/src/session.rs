//! One recording: capture both channels to disk, then process into a note.
//!
//! Layout on disk (`<audio_dir>/<date time slug>/`):
//!   meta.json   — what's being recorded + state, rewritten on each transition
//!   mic.wav     — 16 kHz mono, written incrementally (crash-safe)
//!   system.wav  — 16 kHz mono, written incrementally (crash-safe)
//!
//! A session left in `Recording` state after a crash is recovered on next
//! launch by simply processing whatever audio made it to disk.

use crate::audio::{AudioChunk, Resampler, TARGET_RATE, WavSink, rms_db};
use crate::calendar::CalendarEvent;
use crate::platform::{AudioBackend, CaptureStream, Channel};
use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use crossbeam_channel::{Receiver, unbounded};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    Recording,
    Recorded,
    Transcribed,
    Done,
    Failed,
}

impl SessionState {
    /// Not finished processing yet: left `Recording` by a crash, or
    /// `Recorded` / `Transcribed` and waiting for the next step.
    pub fn is_pending(self) -> bool {
        matches!(self, Self::Recording | Self::Recorded | Self::Transcribed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareRecord {
    /// Destination id, allowing several self-hosted servers per meeting.
    pub destination: String,
    /// Random ASCII-safe namespace for this share's Keychain credentials.
    /// Unlike the session directory, it survives title and Trash/Restore renames.
    pub secret_id: String,
    pub id: String,
    /// Server URL without the #key fragment.
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub title: String,
    pub started: DateTime<Local>,
    pub ended: Option<DateTime<Local>>,
    pub event: Option<CalendarEvent>,
    pub state: SessionState,
    pub note_path: Option<PathBuf>,
    pub error: Option<String>,
    /// Which capture routes were used (for debugging).
    pub routes: Vec<String>,
    /// Where the note was sent ("Send to…" and automatic rules), oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sends: Vec<crate::destinations::SendRecord>,
    /// Active private share links. Their encryption keys and owner tokens live
    /// in the OS Keychain, never in this file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shares: Vec<ShareRecord>,
    /// The meeting app the call ran in (driver id, "zoom"): from the app
    /// holding the mic when recording started, or the windows read during it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meeting_app: Option<String>,
}

impl SessionMeta {
    /// Length in whole minutes, rounded; `None` until the recording ends.
    pub fn duration_mins(&self) -> Option<i64> {
        self.ended.map(|e| ((e - self.started).num_seconds().max(0) + 30) / 60)
    }

    /// "Zoom" / "Microsoft Teams": the meeting app, else the calendar link's provider.
    pub fn platform_label(&self) -> Option<&'static str> {
        self.meeting_app
            .as_deref()
            .and_then(crate::speakers::drivers::by_id)
            .map(|d| d.label())
            .or_else(|| self.event.as_ref().and_then(|e| e.link.as_ref()).map(|l| l.provider.label()))
    }
}

impl SessionMeta {
    pub fn load(dir: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(dir.join("meta.json"))?;
        Ok(serde_json::from_str(&text)?)
    }
    pub fn save(&self, dir: &Path) -> Result<()> {
        // Send history is append-only: keep records another writer (a send
        // finishing while the pipeline rewrites this file) saved meanwhile.
        let on_disk = Self::load(dir).ok();
        let on_disk_sends = on_disk.as_ref().map(|m| m.sends.as_slice()).unwrap_or_default();
        let mut out = self.clone();
        out.sends = crate::destinations::merge_sends(on_disk_sends, &self.sends);
        // Share mutations have their own locked read-modify-write helpers.
        // A long-running pipeline may hold stale metadata, so it must always
        // preserve the disk's current share list rather than resurrecting or
        // replacing a share that was changed while notes were being rewritten.
        out.shares = on_disk.map(|m| m.shares).unwrap_or_default();
        Self::write(&out, dir)
    }

    fn write(meta: &Self, dir: &Path) -> Result<()> {
        let tmp = dir.join("meta.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(meta)?)?;
        std::fs::rename(tmp, dir.join("meta.json"))?;
        Ok(())
    }

    /// Atomically replace one share record without putting its key in metadata.
    pub fn replace_share(dir: &Path, share: ShareRecord) -> Result<()> {
        let _guard = SHARE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut meta = Self::load(dir)?;
        meta.shares.retain(|s| s.destination != share.destination);
        meta.shares.push(share);
        Self::write(&meta, dir)
    }

    /// Atomically forget a server-side share after a successful or already-gone revoke.
    pub fn remove_share(dir: &Path, destination: &str) -> Result<()> {
        let _guard = SHARE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut meta = Self::load(dir)?;
        meta.shares.retain(|s| s.destination != destination);
        Self::write(&meta, dir)
    }
}

static SHARE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn wav_path(dir: &Path, ch: Channel) -> PathBuf {
    dir.join(format!("{}.wav", ch.file_stem()))
}

/// Live levels, readable from the UI thread.
pub struct Levels {
    mic_db: AtomicU32,
    system_db: AtomicU32,
    /// Any non-zero sample seen on the channel (pure digital silence means the
    /// device is muted / unavailable, not just a quiet room).
    mic_alive: AtomicBool,
    system_alive: AtomicBool,
    /// Unix seconds when either channel last had speech-level audio.
    last_voice: std::sync::atomic::AtomicI64,
    /// Unix ms of the last level update per channel.
    mic_updated: std::sync::atomic::AtomicI64,
    system_updated: std::sync::atomic::AtomicI64,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Default for Levels {
    fn default() -> Self {
        let silent = (-120f32).to_bits();
        Self {
            mic_db: AtomicU32::new(silent),
            system_db: AtomicU32::new(silent),
            mic_alive: AtomicBool::new(false),
            system_alive: AtomicBool::new(false),
            last_voice: std::sync::atomic::AtomicI64::new(chrono::Utc::now().timestamp()),
            mic_updated: std::sync::atomic::AtomicI64::new(0),
            system_updated: std::sync::atomic::AtomicI64::new(0),
        }
    }
}

impl Levels {
    /// Current level in dBFS. A channel that hasn't delivered audio for 300 ms
    /// (macOS taps go quiet when nothing plays) reads as silence, not its last value.
    pub fn get(&self, ch: Channel) -> f32 {
        let (db, updated) = match ch {
            Channel::Mic => (&self.mic_db, &self.mic_updated),
            Channel::System => (&self.system_db, &self.system_updated),
        };
        let age = now_ms() - updated.load(Ordering::Relaxed);
        if age > 300 {
            return -120.0;
        }
        f32::from_bits(db.load(Ordering::Relaxed))
    }
    fn set(&self, ch: Channel, db: f32) {
        let (slot, updated) = match ch {
            Channel::Mic => (&self.mic_db, &self.mic_updated),
            Channel::System => (&self.system_db, &self.system_updated),
        };
        slot.store(db.to_bits(), Ordering::Relaxed);
        updated.store(now_ms(), Ordering::Relaxed);
        if db > -50.0 {
            self.last_voice.store(chrono::Utc::now().timestamp(), Ordering::Relaxed);
        }
    }
    pub fn alive(&self, ch: Channel) -> bool {
        match ch {
            Channel::Mic => self.mic_alive.load(Ordering::Relaxed),
            Channel::System => self.system_alive.load(Ordering::Relaxed),
        }
    }
    fn mark_alive(&self, ch: Channel) {
        match ch {
            Channel::Mic => self.mic_alive.store(true, Ordering::Relaxed),
            Channel::System => self.system_alive.store(true, Ordering::Relaxed),
        }
    }
    pub fn secs_since_voice(&self) -> i64 {
        chrono::Utc::now().timestamp() - self.last_voice.load(Ordering::Relaxed)
    }
}

struct Writer {
    handle: JoinHandle<Result<f64>>,
    _stream: Box<dyn CaptureStream>,
}

/// An in-progress recording. Call [`Recording::stop`] to finish.
pub struct Recording {
    pub dir: PathBuf,
    pub meta: SessionMeta,
    pub levels: Arc<Levels>,
    writers: Vec<(Channel, Writer)>,
    stop: Arc<AtomicBool>,
}

/// 16 kHz mono audio of one channel, in order, as it's written to disk.
pub type LiveSink = crossbeam_channel::Sender<(Channel, Vec<f32>)>;

fn spawn_writer(ch: Channel, path: PathBuf, rx: Receiver<AudioChunk>, levels: Arc<Levels>, stop: Arc<AtomicBool>, live: Option<LiveSink>) -> Result<JoinHandle<Result<f64>>> {
    let mut sink = WavSink::create(&path)?;
    Ok(std::thread::Builder::new().name(format!("stenly-{}", ch.file_stem())).spawn(move || {
        let mut rs = Resampler::default();
        let mut buf = Vec::with_capacity(8192);
        let mut meter = Vec::with_capacity(4096);
        // Some backends (e.g. macOS process taps) deliver nothing while the
        // machine is silent. Pad with zeros against the wall clock so both
        // channels share one timeline. Allow some slack for device latency.
        let t0 = std::time::Instant::now();
        let slack = (TARGET_RATE / 2) as u64;
        let mut written: u64 = 0;
        let live = live.as_ref();
        let pad_to_clock = |sink: &mut WavSink, written: &mut u64, slack: u64| -> Result<()> {
            let due = (t0.elapsed().as_secs_f64() * TARGET_RATE as f64) as u64;
            if due > *written + slack {
                let gap = (due - slack - *written) as usize;
                let zeros = vec![0.0; gap];
                sink.write(&zeros)?;
                if let Some(l) = live {
                    let _ = l.send((ch, zeros));
                }
                *written += gap as u64;
            }
            Ok(())
        };
        loop {
            let chunk = match rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(c) => c,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    pad_to_clock(&mut sink, &mut written, slack)?;
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    continue;
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            };
            // "Alive" means audible sound (> -60 dBFS), not a few LSBs of
            // dither: a dead tap once delivered ±3/32768 blips and hid the warning.
            if !levels.alive(ch) && chunk.samples.iter().any(|s| s.abs() > 0.001) {
                levels.mark_alive(ch);
                tracing::info!(channel = ?ch, "first audio received");
            }
            buf.clear();
            rs.process(&chunk, &mut buf);
            pad_to_clock(&mut sink, &mut written, slack + buf.len() as u64)?;
            sink.write(&buf)?;
            if let Some(l) = live {
                let _ = l.send((ch, buf.clone()));
            }
            written += buf.len() as u64;
            meter.extend_from_slice(&buf);
            if meter.len() >= 800 {
                levels.set(ch, rms_db(&meter));
                meter.clear();
            }
            if stop.load(Ordering::Relaxed) && rx.is_empty() {
                break;
            }
        }
        pad_to_clock(&mut sink, &mut written, 0)?;
        let secs = sink.duration_secs();
        sink.finish()?;
        Ok(secs)
    })?)
}

impl Recording {
    pub fn start(backend: &dyn AudioBackend, audio_root: &Path, title: &str, event: Option<CalendarEvent>) -> Result<Self> {
        Self::start_with_live(backend, audio_root, title, event, None)
    }

    /// Like [`start`], also streaming each channel's 16 kHz audio to `live`
    /// (for transcribing during the meeting).
    pub fn start_with_live(backend: &dyn AudioBackend, audio_root: &Path, title: &str, event: Option<CalendarEvent>, live: Option<LiveSink>) -> Result<Self> {
        Self::start_full(backend, &[], audio_root, title, event, live)
    }

    /// Start with a microphone priority list (Settings → Microphones).
    pub fn start_full(backend: &dyn AudioBackend, mic_priority: &[String], audio_root: &Path, title: &str, event: Option<CalendarEvent>, live: Option<LiveSink>) -> Result<Self> {
        let started = Local::now();
        let dir_name = format!("{} {}", started.format("%Y-%m-%d %H%M"), crate::util::sanitize_filename(title));
        let dir = audio_root.join(dir_name);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

        let levels = Arc::new(Levels::default());
        let stop = Arc::new(AtomicBool::new(false));
        let mut writers = Vec::new();
        let mut routes = Vec::new();
        let mut errors = Vec::new();
        for ch in [Channel::Mic, Channel::System] {
            let (tx, rx) = unbounded();
            let started = match ch {
                Channel::Mic if !mic_priority.is_empty() => backend.start_mic_from(mic_priority, tx),
                Channel::Mic => backend.start_mic(tx),
                Channel::System => backend.start_system(tx),
            };
            match started {
                Ok(stream) => {
                    routes.push(format!("{}: {}", ch.file_stem(), stream.describe()));
                    let handle = spawn_writer(ch, wav_path(&dir, ch), rx, levels.clone(), stop.clone(), live.clone())?;
                    writers.push((ch, Writer { handle, _stream: stream }));
                }
                Err(e) => {
                    tracing::error!(channel = ?ch, error = %format!("{e:#}"), "capture failed");
                    errors.push(format!("{} capture failed: {e:#}", ch.file_stem()));
                }
            }
        }
        if writers.is_empty() {
            let _ = std::fs::remove_dir_all(&dir);
            anyhow::bail!("could not capture any audio: {}", errors.join("; "));
        }
        let meta = SessionMeta {
            title: title.to_string(),
            started,
            ended: None,
            event,
            state: SessionState::Recording,
            note_path: None,
            error: if errors.is_empty() { None } else { Some(errors.join("; ")) },
            routes,
            sends: vec![],
            shares: vec![],
            meeting_app: None,
        };
        meta.save(&dir)?;
        tracing::info!(dir = %dir.display(), "recording started");
        Ok(Self { dir, meta, levels, writers, stop })
    }

    pub fn channels(&self) -> Vec<Channel> {
        self.writers.iter().map(|(c, _)| *c).collect()
    }

    /// Stop capture, finalize WAVs and mark the session `Recorded`.
    pub fn stop(mut self) -> Result<(PathBuf, SessionMeta)> {
        self.stop.store(true, Ordering::Relaxed);
        for (ch, w) in self.writers.drain(..) {
            drop(w._stream); // stop the OS stream first so the channel drains
            match w.handle.join() {
                Ok(Ok(secs)) => tracing::info!(channel = ?ch, secs, "channel finalized"),
                Ok(Err(e)) => tracing::error!(channel = ?ch, error = %e, "writer failed"),
                Err(_) => tracing::error!(channel = ?ch, "writer panicked"),
            }
        }
        self.meta.ended = Some(Local::now());
        self.meta.state = SessionState::Recorded;
        self.meta.save(&self.dir)?;
        Ok((self.dir.clone(), self.meta.clone()))
    }
}

/// Sessions that need processing (left `Recording` by a crash, or `Recorded`/`Transcribed`).
pub fn pending_sessions(audio_root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(audio_root) else { return vec![] };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            SessionMeta::load(p).is_ok_and(|m| m.state.is_pending())
        })
        .collect();
    out.sort();
    out
}

/// Recent sessions, newest first.
pub fn list_sessions(audio_root: &Path, limit: usize) -> Vec<(PathBuf, SessionMeta)> {
    let Ok(rd) = std::fs::read_dir(audio_root) else { return vec![] };
    let mut out: Vec<(PathBuf, SessionMeta)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| SessionMeta::load(&e.path()).ok().map(|m| (e.path(), m)))
        .collect();
    out.sort_by(|a, b| b.1.started.cmp(&a.1.started));
    out.truncate(limit);
    out
}

/// Load a finished session's transcript + summary (if any) for display.
pub fn load_detail(dir: &Path) -> (Option<crate::transcript::Transcript>, Option<crate::summary::Summary>) {
    let t = std::fs::read(dir.join("transcript.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let s = std::fs::read(dir.join("summary.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
    (t, s)
}

// ---- Trash
//
// Trashed meetings move to `<audio_dir>/.trash/<folder>`: a rename, so it's
// instant and Restore puts the folder back exactly as it was. Nothing in the
// app (Recent, MCP, CLI, processing) looks inside `.trash`, since it has no
// meta.json of its own. Meetings stay there until the Trash is emptied.

const TRASH: &str = ".trash";
const TRASHED_AT: &str = ".trashed-at";

pub fn trash_dir(audio_root: &Path) -> PathBuf {
    audio_root.join(TRASH)
}

/// `dir/name`, or `dir/name 2`, `dir/name 3`… if that's taken.
fn free_name(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let base = name.to_string_lossy();
    (2..).map(|n| dir.join(format!("{base} {n}"))).find(|p| !p.exists()).expect("unbounded")
}

fn is_child_of(path: &Path, parent: &Path) -> bool {
    path.parent().is_some_and(|p| p == parent) && path.file_name().is_some()
}

/// Move a session folder into the Trash. Returns where it went.
pub fn trash_session(audio_root: &Path, dir: &Path) -> Result<PathBuf> {
    anyhow::ensure!(is_child_of(dir, audio_root), "{} isn't in the recordings folder", dir.display());
    anyhow::ensure!(dir.join("meta.json").exists(), "{} isn't a meeting", dir.display());
    let trash = trash_dir(audio_root);
    std::fs::create_dir_all(&trash).context("creating the Trash folder")?;
    let to = free_name(&trash, dir.file_name().unwrap_or_default());
    std::fs::rename(dir, &to).with_context(|| format!("moving {} to the Trash", dir.display()))?;
    // Best effort: only used to sort the Trash.
    let _ = std::fs::write(to.join(TRASHED_AT), Local::now().to_rfc3339());
    Ok(to)
}

/// Put a trashed session back in the recordings folder. Returns its new path.
pub fn restore_session(audio_root: &Path, trashed: &Path) -> Result<PathBuf> {
    anyhow::ensure!(is_child_of(trashed, &trash_dir(audio_root)), "{} isn't in the Trash", trashed.display());
    let to = free_name(audio_root, trashed.file_name().unwrap_or_default());
    std::fs::rename(trashed, &to).with_context(|| format!("restoring {}", trashed.display()))?;
    let _ = std::fs::remove_file(to.join(TRASHED_AT));
    Ok(to)
}

/// Permanently delete one trashed session.
pub fn delete_trashed(audio_root: &Path, trashed: &Path) -> Result<()> {
    anyhow::ensure!(is_child_of(trashed, &trash_dir(audio_root)), "{} isn't in the Trash", trashed.display());
    std::fs::remove_dir_all(trashed).with_context(|| format!("deleting {}", trashed.display()))
}

/// Permanently delete everything in the Trash. Returns how many meetings went.
pub fn empty_trash(audio_root: &Path) -> Result<usize> {
    let trash = trash_dir(audio_root);
    let Ok(rd) = std::fs::read_dir(&trash) else { return Ok(0) };
    let mut n = 0;
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        if p.is_dir() {
            std::fs::remove_dir_all(&p).with_context(|| format!("deleting {}", p.display()))?;
            n += 1;
        } else {
            let _ = std::fs::remove_file(&p);
        }
    }
    Ok(n)
}

/// Trashed sessions, most recently trashed first.
pub fn list_trash(audio_root: &Path) -> Vec<(PathBuf, SessionMeta)> {
    let Ok(rd) = std::fs::read_dir(trash_dir(audio_root)) else { return vec![] };
    let trashed_at = |p: &Path| -> Option<DateTime<Local>> {
        let s = std::fs::read_to_string(p.join(TRASHED_AT)).ok()?;
        DateTime::parse_from_rfc3339(s.trim()).ok().map(|d| d.with_timezone(&Local))
    };
    let mut out: Vec<(PathBuf, SessionMeta, Option<DateTime<Local>>)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| SessionMeta::load(&e.path()).ok().map(|m| (e.path(), m)))
        .map(|(p, m)| {
            let t = trashed_at(&p);
            (p, m, t)
        })
        .collect();
    out.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.started.cmp(&a.1.started)));
    out.into_iter().map(|(p, m, _)| (p, m)).collect()
}

#[cfg(test)]
mod trash_tests {
    use super::*;

    #[test]
    fn stale_save_keeps_current_share_changes() {
        let root = tempfile::tempdir().unwrap();
        let dir = session(root.path(), "stale");
        let stale = SessionMeta::load(&dir).unwrap();
        SessionMeta::replace_share(&dir, ShareRecord { destination: "share".into(), secret_id: "fresh".into(), id: "new".into(), url: "https://share.test/s/new".into(), expires_at: None }).unwrap();
        stale.save(&dir).unwrap();
        assert_eq!(SessionMeta::load(&dir).unwrap().shares[0].id, "new");
        SessionMeta::remove_share(&dir, "share").unwrap();
        stale.save(&dir).unwrap();
        assert!(SessionMeta::load(&dir).unwrap().shares.is_empty());
    }

    #[test]
    fn removing_a_share_stays_removed() {
        let root = tempfile::tempdir().unwrap();
        let dir = session(root.path(), "share");
        SessionMeta::replace_share(&dir, ShareRecord { destination: "share".into(), secret_id: "test-secret".into(), id: "id".into(), url: "https://share.test/s/id".into(), expires_at: None }).unwrap();
        SessionMeta::remove_share(&dir, "share").unwrap();
        assert!(SessionMeta::load(&dir).unwrap().shares.is_empty());
    }

    fn session(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let meta = SessionMeta {
            title: name.into(),
            started: Local::now(),
            ended: None,
            event: None,
            state: SessionState::Done,
            note_path: None,
            error: None,
            routes: vec![],
            sends: vec![],
            shares: vec![],
            meeting_app: None,
        };
        meta.save(&dir).unwrap();
        std::fs::write(dir.join("mic.wav"), b"audio").unwrap();
        dir
    }

    #[test]
    fn trash_restore_delete_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let a = session(root, "a");
        let b = session(root, "b");

        let ta = trash_session(root, &a).unwrap();
        assert!(!a.exists() && ta.join("mic.wav").exists());
        // Hidden from everything that lists meetings.
        assert_eq!(list_sessions(root, 10).len(), 1);
        assert!(pending_sessions(root).is_empty());
        assert_eq!(list_trash(root).len(), 1);

        // Restore puts it back where it was.
        assert_eq!(restore_session(root, &ta).unwrap(), a);
        assert!(!a.join(TRASHED_AT).exists());
        assert_eq!(list_sessions(root, 10).len(), 2);
        assert!(list_trash(root).is_empty());

        // A name clash in the Trash gets a suffix instead of overwriting.
        let ta = trash_session(root, &a).unwrap();
        let a2 = session(root, "a");
        let ta2 = trash_session(root, &a2).unwrap();
        assert_ne!(ta, ta2);
        assert_eq!(list_trash(root).len(), 2);

        delete_trashed(root, &ta).unwrap();
        assert!(!ta.exists());
        trash_session(root, &b).unwrap();
        assert_eq!(empty_trash(root).unwrap(), 2);
        assert!(list_trash(root).is_empty() && list_sessions(root, 10).is_empty());

        // Only paths in the right place are accepted.
        let c = session(root, "c");
        assert!(delete_trashed(root, &c).is_err());
        assert!(restore_session(root, &c).is_err());
        assert!(trash_session(&root.join("elsewhere"), &c).is_err());
        assert!(c.exists());
    }

    #[test]
    fn restored_unprocessed_meeting_is_pending_again() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let d = session(root, "queued");
        let mut m = SessionMeta::load(&d).unwrap();
        m.state = SessionState::Recorded;
        m.save(&d).unwrap();
        assert_eq!(pending_sessions(root), vec![d.clone()]);

        let t = trash_session(root, &d).unwrap();
        assert!(pending_sessions(root).is_empty(), "the Trash isn't processed");
        let back = restore_session(root, &t).unwrap();
        assert!(SessionMeta::load(&back).unwrap().state.is_pending());
        assert_eq!(pending_sessions(root), vec![back]);
        assert!(!SessionState::Done.is_pending() && !SessionState::Failed.is_pending());
    }
}
