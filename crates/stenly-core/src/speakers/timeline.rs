//! `speakers.jsonl`: who the meeting app showed as talking, over time.
//!
//! Written during the recording by [`Watcher`] (one line whenever the
//! active speaker or the roster changes, plus a heartbeat), read after it by
//! the naming pass. Times are seconds from the start of the recording, the
//! same clock as transcript segments.

use super::ax::{MeetingAxSource, Observation, observe};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub const FILE: &str = "speakers.jsonl";
/// Poll interval: fast enough to catch a "yeah" between two longer turns.
const POLL_MS: u64 = 250;
/// Rewrite an unchanged state this often, so a stalled poller shows as a gap.
const HEARTBEAT_SECS: f64 = 5.0;
/// A state is trusted for at most this long after it was observed.
const MAX_HOLD_SECS: f64 = 6.0;
/// Re-save a window's raw tree this often when snapshots are on.
const SNAPSHOT_EVERY_SECS: f64 = 300.0;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Seconds since the recording started.
    pub t: f64,
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub active: Vec<String>,
    #[serde(default)]
    pub roster: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub me: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}

impl Event {
    pub fn new(t: f64, o: &Observation) -> Self {
        Self { t, app: o.app.clone(), active: o.active.clone(), roster: o.roster.clone(), me: o.me.clone(), host: o.host.clone(), topic: o.topic.clone() }
    }
}

/// What the meeting app said about the meeting as a whole.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Summary {
    /// Driver id of the app seen most ("zoom").
    pub app: Option<String>,
    /// The signed-in user / self tile, as the app named it.
    pub me: Option<String>,
    pub host: Option<String>,
    pub topic: Option<String>,
    /// [`roster`].
    pub roster: Vec<String>,
}

/// Most-seen app, last-seen self/host/topic, the settled roster.
pub fn summarize(events: &[Event]) -> Summary {
    let mut apps: Vec<(String, usize)> = Vec::new();
    for e in events.iter().filter(|e| !e.app.is_empty() && e.app != "other") {
        match apps.iter_mut().find(|(a, _)| *a == e.app) {
            Some(x) => x.1 += 1,
            None => apps.push((e.app.clone(), 1)),
        }
    }
    apps.sort_by(|a, b| b.1.cmp(&a.1));
    Summary {
        app: apps.into_iter().next().map(|x| x.0),
        me: events.iter().rev().find_map(|e| e.me.clone()),
        host: events.iter().rev().find_map(|e| e.host.clone()),
        topic: events.iter().rev().find_map(|e| e.topic.clone()),
        roster: roster(events),
    }
}

pub fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

pub fn load(dir: &Path) -> Vec<Event> {
    let Ok(text) = std::fs::read_to_string(path(dir)) else { return Vec::new() };
    let mut v: Vec<Event> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    v.sort_by(|a, b| a.t.total_cmp(&b.t));
    v
}

/// Everyone the meeting app showed for a real part of the meeting (a tile
/// that flashed up once is not a participant), most-seen first.
pub fn roster(events: &[Event]) -> Vec<String> {
    let mut seen: Vec<(String, usize)> = Vec::new();
    let mut with_roster = 0;
    for e in events {
        with_roster += (!e.roster.is_empty()) as usize;
        for n in &e.roster {
            match seen.iter_mut().find(|(x, _)| x == n) {
                Some(s) => s.1 += 1,
                None => seen.push((n.clone(), 1)),
            }
        }
    }
    seen.retain(|(_, k)| *k * 10 >= with_roster || *k >= 20);
    seen.sort_by(|a, b| b.1.cmp(&a.1));
    seen.into_iter().map(|(n, _)| n).collect()
}

/// Seconds of [start, end) for which we have a fresh observation (the
/// watcher was running and the meeting window was readable).
pub fn covered(events: &[Event], start: f64, end: f64) -> f64 {
    let mut total = 0.0;
    for (i, e) in events.iter().enumerate() {
        if e.roster.is_empty() && e.active.is_empty() {
            continue; // window gone / closing line
        }
        let until = events.get(i + 1).map(|n| n.t).unwrap_or(e.t + POLL_MS as f64 / 1000.0).min(e.t + MAX_HOLD_SECS);
        let (a, b) = (e.t.max(start), until.min(end));
        if b > a {
            total += b - a;
        }
    }
    total
}

/// Seconds each name was shown as talking during [start, end).
pub fn overlap(events: &[Event], start: f64, end: f64) -> Vec<(String, f64)> {
    let mut out: Vec<(String, f64)> = Vec::new();
    for (i, e) in events.iter().enumerate() {
        let until = events.get(i + 1).map(|n| n.t).unwrap_or(e.t + POLL_MS as f64 / 1000.0).min(e.t + MAX_HOLD_SECS);
        let (a, b) = (e.t.max(start), until.min(end));
        if b <= a || e.active.is_empty() {
            continue;
        }
        // several highlighted at once share the time
        let share = (b - a) / e.active.len() as f64;
        for n in &e.active {
            match out.iter_mut().find(|(x, _)| x == n) {
                Some(s) => s.1 += share,
                None => out.push((n.clone(), share)),
            }
        }
    }
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out
}

/// Native call apps that keep the mic open after hanging up (Teams), so the
/// mic can't tell us the call ended. Zoom releases it on hang-up, and may
/// hide its tiles while screen sharing, so it isn't here. Browsers aren't
/// either: switching tabs hides a Meet call without ending it.
const KEEPS_MIC_AFTER_CALL: &[&str] = &["com.microsoft.teams2", "com.microsoft.teams"];

/// Whether such an app's call window is still showing anyone.
#[derive(Default)]
pub struct CallPresence {
    seen: AtomicBool,
    /// Unix ms of the last poll that saw people in a native call window.
    last_ms: std::sync::atomic::AtomicI64,
}

impl CallPresence {
    fn mark(&self) {
        self.seen.store(true, Ordering::Relaxed);
        self.last_ms.store(chrono::Utc::now().timestamp_millis(), Ordering::Relaxed);
    }

    /// Seconds since the call window last showed anyone; None until one has.
    pub fn gone_secs(&self) -> Option<f64> {
        self.seen
            .load(Ordering::Relaxed)
            .then(|| (chrono::Utc::now().timestamp_millis() - self.last_ms.load(Ordering::Relaxed)).max(0) as f64 / 1000.0)
    }
}

/// Polls meeting windows while a recording runs and appends to `speakers.jsonl`.
pub struct Watcher {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub presence: Arc<CallPresence>,
}

impl Watcher {
    pub fn start(source: Arc<dyn MeetingAxSource>, dir: &Path, started: DateTime<Local>) -> std::io::Result<Self> {
        Self::start_with(source, dir, started, None)
    }

    /// Like [`start`](Self::start); with `snapshots`, also saves the raw tree
    /// of each meeting window the first time its title is seen, and again
    /// every few minutes, there (`<window title>-<secs>.json`): material for
    /// driver fixtures (`stenly-cli speakers parse`).
    pub fn start_with(source: Arc<dyn MeetingAxSource>, dir: &Path, started: DateTime<Local>, snapshots: Option<PathBuf>) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let file = path(dir);
        let st = stop.clone();
        let presence = Arc::new(CallPresence::default());
        let pr = presence.clone();
        let thread = std::thread::Builder::new().name("stenly-speakers".into()).spawn(move || {
            let mut out = match std::fs::OpenOptions::new().create(true).append(true).open(&file) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(error = %e, "can't write speakers.jsonl");
                    return;
                }
            };
            let mut last: Option<(Observation, f64)> = None;
            let mut lines = 0usize;
            let mut saved: Vec<(String, f64)> = Vec::new();
            while !st.load(Ordering::Relaxed) {
                let t0 = std::time::Instant::now();
                let windows = source.snapshot();
                let o = observe(&windows);
                let call: Vec<_> = windows.iter().filter(|w| KEEPS_MIC_AFTER_CALL.contains(&w.app.as_str())).cloned().collect();
                if !call.is_empty() && !observe(&call).is_empty() {
                    pr.mark();
                }
                let t = (Local::now() - started).num_milliseconds() as f64 / 1000.0;
                if let Some(snap) = &snapshots {
                    save_snapshots(snap, &windows, t, &mut saved);
                }
                let changed = last.as_ref().is_none_or(|(p, at)| *p != o || t - at >= HEARTBEAT_SECS);
                // Nothing on screen and nothing before: don't write empty lines.
                if changed && !(o.is_empty() && last.as_ref().is_none_or(|(p, _)| p.is_empty())) {
                    let ev = Event::new((t * 1000.0).round() / 1000.0, &o);
                    if let Ok(line) = serde_json::to_string(&ev) {
                        let _ = writeln!(out, "{line}");
                        lines += 1;
                        if lines == 1 {
                            tracing::info!(app = %ev.app, people = ev.roster.len(), "meeting app speaker highlight found");
                        }
                    }
                    last = Some((o, t));
                } else if last.is_none() {
                    last = Some((o, t));
                }
                let spent = t0.elapsed().as_millis() as u64;
                std::thread::sleep(std::time::Duration::from_millis(POLL_MS.saturating_sub(spent).max(50)));
            }
            // close the last state so it doesn't extend past the end
            let t = (Local::now() - started).num_milliseconds() as f64 / 1000.0;
            if last.is_some_and(|(p, _)| !p.is_empty()) {
                let _ = writeln!(out, "{}", serde_json::json!({ "t": t, "active": [], "roster": [] }));
            }
        })?;
        Ok(Self { stop, thread: Some(thread), presence })
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Save each window's tree the first time its (app, title) shows up and then
/// every [`SNAPSHOT_EVERY_SECS`].
fn save_snapshots(dir: &Path, windows: &[super::ax::AxWindow], t: f64, saved: &mut Vec<(String, f64)>) {
    for w in windows {
        let key = format!("{} {}", w.app, w.title);
        let due = saved.iter().find(|(k, _)| *k == key).is_none_or(|(_, at)| t - at >= SNAPSHOT_EVERY_SECS);
        if !due {
            continue;
        }
        saved.retain(|(k, _)| *k != key);
        saved.push((key, t));
        let _ = std::fs::create_dir_all(dir);
        let name = format!("{}-{:05.0}.json", crate::util::sanitize_filename(&w.title).chars().take(60).collect::<String>(), t);
        if let Ok(json) = serde_json::to_vec_pretty(&[w]) {
            let _ = std::fs::write(dir.join(name), json);
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(t: f64, active: &[&str]) -> Event {
        Event { t, app: "teams".into(), active: active.iter().map(|s| s.to_string()).collect(), roster: vec!["A".into(), "B".into()], ..Default::default() }
    }

    #[test]
    fn overlap_counts_time_per_name() {
        let evs = vec![ev(0.0, &["A"]), ev(2.0, &["A", "B"]), ev(3.0, &[]), ev(5.0, &["B"]), ev(5.25, &[])];
        let o = overlap(&evs, 0.0, 10.0);
        assert_eq!(o[0].0, "A");
        assert!((o[0].1 - 2.5).abs() < 1e-9);
        assert!((o[1].1 - 0.75).abs() < 1e-9);
        // window inside one state
        assert!((overlap(&evs, 0.5, 1.5)[0].1 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn coverage_skips_gaps() {
        let evs = vec![ev(0.0, &["A"]), ev(2.0, &[]), ev(30.0, &["B"]), ev(31.0, &[])];
        // 0–2 and 2–8 (held 6 s), then a gap, then 30–31 and 31–31.25
        assert!((covered(&evs, 0.0, 40.0) - 9.25).abs() < 1e-9);
    }

    #[test]
    fn stale_states_expire() {
        // poller died after this line: don't credit A for a minute
        let evs = vec![ev(0.0, &["A"]), ev(60.0, &[])];
        assert!((overlap(&evs, 0.0, 60.0)[0].1 - MAX_HOLD_SECS).abs() < 1e-9);
    }

    #[test]
    fn watcher_writes_changes_only() {
        struct Fake(std::sync::Mutex<usize>);
        impl MeetingAxSource for Fake {
            fn snapshot(&self) -> Vec<crate::speakers::ax::AxWindow> {
                use crate::speakers::ax::*;
                let mut n = self.0.lock().unwrap();
                *n += 1;
                let desc = if *n < 3 { "Talking: Ann Lee" } else { "Talking: Bo Kim" };
                vec![AxWindow { app: "us.zoom.xos".into(), title: "Zoom Meeting".into(), root: AxNode { role: Some("AXStaticText".into()), value: Some(desc.into()), ..Default::default() } }]
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let w = Watcher::start(Arc::new(Fake(Default::default())), dir.path(), Local::now()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        w.stop();
        let evs = load(dir.path());
        let names: Vec<_> = evs.iter().filter(|e| !e.active.is_empty()).map(|e| e.active[0].as_str()).collect();
        assert_eq!(names, vec!["Ann Lee", "Bo Kim"], "{evs:?}");
        assert!(evs.last().unwrap().active.is_empty(), "closing line");
        assert_eq!(roster(&evs).len(), 2);
    }
}
