//! Dictation controller: global shortcut → record the mic → transcribe →
//! clean up → paste where the cursor is. Owns the hotkey listener and the HUD
//! state; the HUD window (`dictation_hud`) observes it.
//!
//! States: Idle → Recording → Transcribing → (Enhancing) → Idle. A meeting
//! recording can run at the same time: dictation opens its own mic stream.

use crate::controller::Controller;
use gpui::{App, AppContext, AsyncApp, Context, Entity, EventEmitter, Task};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use stenly_core::audio::{AudioChunk, Resampler, WavSink, rms_db};
use stenly_core::config::{Config, ShortcutMode, StreamMode};
use stenly_core::dictation::{self, Dictation, KeyAction, ShortcutState};
use stenly_core::platform::{AudioBackend, CaptureStream, Hotkey, HotkeyEvent, Hotkeys, TextInjector};

/// Longest single dictation (VoiceInk has no cap; we stop at 10 min so a
/// forgotten hands-free session doesn't record all afternoon).
const MAX_SECS: u64 = 600;

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Idle,
    /// `hands_free` = started by a tap (another tap stops), vs held.
    Recording { since: Instant, hands_free: bool },
    Transcribing,
    Enhancing,
    /// Briefly shown after an error or when nothing was heard.
    Notice(String),
}

pub enum DictationEvent {
    /// The HUD should appear / update / disappear.
    Changed,
}

pub struct DictationController {
    pub phase: Phase,
    /// Smoothed mic level, 0..1, for the HUD visualiser.
    level: Arc<AtomicU32>,
    pub hotkeys_error: Option<String>,
    pub last_error: Option<String>,
    controller: Entity<Controller>,
    audio: Arc<dyn AudioBackend>,
    hotkeys: Arc<dyn Hotkeys>,
    injector: Arc<dyn TextInjector>,
    states: [ShortcutState; 2],
    active_slot: Option<usize>,
    capture: Option<Capture>,
    update_activity: Option<stenly_core::update::Activity>,
    /// Bindings currently registered (to avoid re-registering on every config save).
    registered: Vec<Option<Hotkey>>,
    model_settings: Option<(bool, u64, String)>,
    /// Hotkey pump / permission retry (replaced when shortcuts change).
    listener: Option<Task<()>>,
    /// Recording ticker, processing pipeline, notice timer.
    work: Vec<Task<()>>,
}

struct Capture {
    samples: Arc<std::sync::Mutex<Vec<f32>>>,
    /// Transcribes while recording (live preview; only the tail is left at stop).
    live: Option<stenly_asr::stream::LiveDictation>,
    /// Batch dictation also loads during capture and pins the worker until stop.
    batch_model: Option<crossbeam_channel::Receiver<anyhow::Result<Arc<stenly_asr::ParakeetTranscriber>>>>,
    /// Owns the mic stream + mute guard; ends after `stop`.
    writer: std::thread::JoinHandle<Option<()>>,
    stop: crossbeam_channel::Sender<()>,
    app: Option<String>,
    /// Selected text when dictation started (context for AI polish).
    selected: Option<String>,
}

impl EventEmitter<DictationEvent> for DictationController {}

/// Stand-in sent back to the UI thread once the real stream is running.
struct NoopStream(String);
impl CaptureStream for NoopStream {
    fn describe(&self) -> String {
        self.0.clone()
    }
}

impl DictationController {
    pub fn new(controller: Entity<Controller>, audio: Arc<dyn AudioBackend>, hotkeys: Arc<dyn Hotkeys>, injector: Arc<dyn TextInjector>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            phase: Phase::Idle,
            level: Arc::new(AtomicU32::new(0)),
            hotkeys_error: None,
            last_error: None,
            controller: controller.clone(),
            audio,
            hotkeys,
            injector,
            states: Default::default(),
            active_slot: None,
            capture: None,
            update_activity: None,
            registered: vec![],
            model_settings: None,
            listener: None,
            work: vec![],
        };
        cx.observe(&controller, |this, _, cx| this.sync_config(cx)).detach();
        this.sync_config(cx);
        this
    }

    pub fn config(&self, cx: &App) -> Config {
        self.controller.read(cx).config.clone()
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    /// Live transcript while recording (committed, still-changing).
    pub fn preview(&self) -> Option<stenly_asr::stream::Preview> {
        let live = self.capture.as_ref()?.live.as_ref()?;
        live.preview.lock().ok().map(|p| p.clone())
    }

    pub fn is_busy(&self) -> bool {
        !matches!(self.phase, Phase::Idle | Phase::Notice(_))
    }

    /// (Re)register shortcuts after a settings change, and preload the model.
    fn sync_config(&mut self, cx: &mut Context<Self>) {
        let cfg = self.config(cx);
        let d = &cfg.dictation;
        let wanted: Vec<Option<Hotkey>> = if d.enabled {
            vec![Hotkey::parse(&d.shortcut), Hotkey::parse(&d.shortcut2), Hotkey::parse(&d.paste_last_shortcut)]
        } else {
            vec![]
        };
        // Apply the model policy independently of shortcuts: changing an ASR
        // model or the memory setting must not be skipped by the early return.
        let model_settings = (d.enabled, d.model_idle_secs, format!("{}|{}|{}|{}", cfg.asr.model, cfg.asr.provider, cfg.asr.threads, cfg.paths().models_dir.display()));
        if self.model_settings.as_ref() != Some(&model_settings) {
            self.model_settings = Some(model_settings);
            stenly_asr::DICTATION_ASR.set_idle_timeout(if !d.enabled { Some(Duration::ZERO) } else { (d.model_idle_secs > 0).then(|| Duration::from_secs(d.model_idle_secs)) });
            let asr = cfg.asr.clone();
            let models = cfg.paths().models_dir;
            let preload = d.enabled && d.model_idle_secs == 0;
            cx.background_spawn(async move {
                if preload {
                    if let Err(e) = stenly_asr::DICTATION_ASR.get(&asr, &models) {
                        tracing::warn!(error = %format!("{e:#}"), "dictation model not ready");
                    }
                }
            }).detach();
        }
        // Only re-register when the bindings change; a failed registration
        // retries on its own timer (below), not on every settings notify.
        if wanted == self.registered {
            return;
        }
        self.registered = wanted.clone();
        if !d.enabled {
            self.hotkeys.stop();
            self.hotkeys_error = None;
            cx.notify();
            return;
        }
        let (tx, rx) = crossbeam_channel::unbounded::<HotkeyEvent>();
        match self.hotkeys.watch(wanted, tx) {
            Ok(()) => {
                self.hotkeys_error = None;
                tracing::info!(shortcut = %d.shortcut, mode = ?d.mode, "dictation shortcuts active");
            }
            Err(e) => {
                let msg = format!("{e:#}");
                if self.hotkeys_error.as_deref() != Some(msg.as_str()) {
                    tracing::warn!(error = %msg, "dictation shortcuts unavailable; will retry");
                }
                self.hotkeys_error = Some(msg);
                // retry quietly: permission may be granted any moment
                let retry = cx.spawn(async move |this, cx: &mut AsyncApp| {
                    cx.background_executor().timer(Duration::from_secs(3)).await;
                    let _ = this.update(cx, |this, cx| {
                        if this.hotkeys_error.is_some() {
                            this.registered.clear();
                            this.sync_config(cx);
                        }
                    });
                });
                self.listener = Some(retry);
                cx.notify();
                return;
            }
        }
        // Pump hotkey events onto the main thread (12 ms poll ≈ imperceptible;
        // timestamps come from the events themselves, so timing stays exact).
        let pump = cx.spawn(async move |this, cx: &mut AsyncApp| {
            loop {
                let mut got = Vec::new();
                loop {
                    match rx.try_recv() {
                        Ok(e) => got.push(e),
                        Err(crossbeam_channel::TryRecvError::Empty) => break,
                        // listener re-registered with a new channel
                        Err(crossbeam_channel::TryRecvError::Disconnected) => return,
                    }
                }
                if !got.is_empty() && this.update(cx, |this, cx| got.into_iter().for_each(|e| this.on_key(e, cx))).is_err() {
                    return;
                }
                cx.background_executor().timer(Duration::from_millis(12)).await;
            }
        });
        self.listener = Some(pump);
        cx.notify();
    }

    fn on_key(&mut self, ev: HotkeyEvent, cx: &mut Context<Self>) {
        let cfg = self.config(cx);
        let d = &cfg.dictation;
        let (slot, pressed, at) = match ev {
            HotkeyEvent::Pressed { slot, at } => (slot as usize, true, at),
            HotkeyEvent::Released { slot, at } => (slot as usize, false, at),
        };
        match slot {
            3 if pressed => return self.cancel(cx),
            2 if pressed => return self.paste_last(cx),
            0 | 1 => {}
            _ => return,
        }
        // Released with at = -1: the modifier became part of a regular
        // shortcut (Right ⌘ + C). If that held key had just started a dictation,
        // cancel it quietly.
        if !pressed && at < 0.0 {
            self.states[slot].reset();
            if self.active_slot == Some(slot) && matches!(self.phase, Phase::Recording { hands_free: false, .. }) {
                if let Phase::Recording { since, .. } = self.phase {
                    if since.elapsed() < Duration::from_millis(800) {
                        return self.cancel_silently(cx);
                    }
                }
            }
            return;
        }
        let mode = if slot == 0 { d.mode } else { d.mode2 };
        let recording = matches!(self.phase, Phase::Recording { .. });
        if self.is_busy() && !recording {
            // still transcribing the last one: ignore, and don't let a half-seen
            // press/release pair confuse the tap/hold logic afterwards
            self.states[slot].reset();
            return;
        }
        let action = if pressed { self.states[slot].press(mode, at, recording) } else { self.states[slot].release(mode, at, recording) };
        match action {
            KeyAction::Start => {
                self.active_slot = Some(slot);
                self.start(cx);
            }
            KeyAction::Stop => self.stop(cx),
            KeyAction::Nothing => {
                // hybrid tap: mark the HUD as hands-free
                if !pressed && recording && mode == ShortcutMode::Hybrid {
                    if let Phase::Recording { since, .. } = self.phase {
                        self.phase = Phase::Recording { since, hands_free: true };
                        cx.notify();
                    }
                }
            }
        }
    }

    /// While a shortcut field is recording, ignore the global shortcuts.
    pub fn pause_shortcuts(&mut self, paused: bool, cx: &mut Context<Self>) {
        if paused {
            self.hotkeys.stop();
            self.registered.clear();
            self.listener = None;
        } else {
            self.sync_config(cx);
        }
    }

    /// Start recording (from a shortcut, the HUD or the menu).
    ///
    /// The HUD appears immediately; the mic opens on a background thread
    /// (CoreAudio device start can take 100–300 ms, Bluetooth longer) and the
    /// writer keeps everything from the first callback, so the first syllable
    /// isn't lost while the device spins up.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        let Some(activity) = stenly_core::update::Activity::begin() else {
            self.notice("An update is being installed. Try dictation again after it finishes.".into(), cx);
            return;
        };
        self.update_activity = Some(activity);
        let cfg = self.config(cx);
        let samples = Arc::new(std::sync::Mutex::new(Vec::<f32>::with_capacity(16_000 * 30)));
        let (stop_tx, stop_rx) = crossbeam_channel::bounded::<()>(1);
        let (stream_tx, stream_rx) = crossbeam_channel::bounded::<Result<Box<dyn CaptureStream>, String>>(1);
        let level = self.level.clone();
        let buf = samples.clone();
        let audio = self.audio.clone();
        let prio = cfg.audio.mic_priority.clone();
        let sounds = cfg.dictation.sounds;
        let mute_wanted = cfg.dictation.mute_system_audio;
        // Transcribe as we go (the model is kept warm; loading it, if needed,
        // happens on the worker while audio queues up).
        let live = (cfg.dictation.stream != StreamMode::Off).then(|| {
            let (asr_cfg, models) = (cfg.asr.clone(), cfg.paths().models_dir);
            let params = match cfg.dictation.stream {
                StreamMode::Overlap => stenly_asr::stream::StreamParams::OVERLAP,
                _ => stenly_asr::stream::StreamParams::PAUSE,
            };
            stenly_asr::stream::LiveDictation::start(move || stenly_asr::DICTATION_ASR.get(&asr_cfg, &models), params)
        });
        let batch_model = if live.is_none() {
            let (tx, rx) = crossbeam_channel::bounded(1);
            let (asr, models) = (cfg.asr.clone(), cfg.paths().models_dir);
            std::thread::spawn(move || { let _ = tx.send(stenly_asr::DICTATION_ASR.get(&asr, &models)); });
            Some(rx)
        } else { None };
        let live_tx = live.as_ref().map(|l| l.sender());
        let writer = std::thread::Builder::new()
            .name("stenly-dictation".into())
            .spawn(move || {
                let (tx, rx) = crossbeam_channel::bounded::<AudioChunk>(512);
                let stream = if prio.is_empty() { audio.start_mic(tx) } else { audio.start_mic_from(&prio, tx) };
                let stream = match stream {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = stream_tx.send(Err(format!("{e:#}")));
                        return None;
                    }
                };
                tracing::info!(mic = %stream.describe(), "dictation mic open");
                let _ = stream_tx.send(Ok(Box::new(NoopStream(stream.describe()))));
                if sounds {
                    stenly_platform::chime::play(stenly_platform::chime::Chime::Start);
                }
                // Mute other audio once the start chime has played.
                let mute = if mute_wanted {
                    std::thread::sleep(Duration::from_millis(220));
                    stenly_platform::mute_output()
                } else {
                    None
                };
                let mut rs = Resampler::default();
                let mut out = Vec::with_capacity(4096);
                let mut smooth = 0.0f32;
                let feed = move |out: &[f32]| {
                    if let Some(tx) = &live_tx {
                        let _ = tx.send(out.to_vec());
                    }
                };
                loop {
                    crossbeam_channel::select! {
                        recv(rx) -> msg => {
                            let Ok(chunk) = msg else { break };
                            out.clear();
                            rs.process(&chunk, &mut out);
                            let db = rms_db(&out);
                            let v = ((db + 55.0) / 40.0).clamp(0.0, 1.0);
                            smooth += (v - smooth) * if v > smooth { 0.55 } else { 0.18 };
                            level.store(smooth.to_bits(), Ordering::Relaxed);
                            if let Ok(mut b) = buf.lock() {
                                b.extend_from_slice(&out);
                            }
                            feed(&out);
                        }
                        recv(stop_rx) -> _ => {
                            // stop the device, then keep what it already delivered
                            drop(stream);
                            while let Ok(chunk) = rx.try_recv() {
                                out.clear();
                                rs.process(&chunk, &mut out);
                                if let Ok(mut b) = buf.lock() {
                                    b.extend_from_slice(&out);
                                }
                                feed(&out);
                            }
                            break;
                        }
                    }
                }
                level.store(0f32.to_bits(), Ordering::Relaxed);
                drop(mute);
                // end of audio for the live transcriber
                drop(feed);
                Some(())
            })
            .expect("spawn dictation writer");
        let app = self.injector.frontmost_app();
        let selected = if cfg.dictation.ai_cleanup { self.injector.selected_text() } else { None };
        if cfg.dictation.ai_cleanup {
            let c = cfg.clone();
            std::thread::spawn(move || dictation::prewarm_ai(&c));
        }
        self.capture = Some(Capture { samples, live, batch_model, writer, stop: stop_tx, app, selected });
        self.phase = Phase::Recording { since: Instant::now(), hands_free: false };
        self.hotkeys.set_capture_escape(true);
        // Mic failure → notice; HUD refresh + safety stop while recording.
        let t = cx.spawn(async move |this, cx: &mut AsyncApp| {
            let opened = cx.background_spawn(async move { stream_rx.recv_timeout(Duration::from_secs(8)) }).await;
            match opened {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    let _ = this.update(cx, |this, cx| {
                        this.abandon();
                        this.notice(format!("Microphone unavailable: {e}"), cx)
                    });
                    return;
                }
                Err(_) => {
                    let _ = this.update(cx, |this, cx| {
                        this.abandon();
                        this.notice("The microphone didn't start".into(), cx)
                    });
                    return;
                }
            }
            loop {
                cx.background_executor().timer(Duration::from_millis(33)).await;
                let keep = this.update(cx, |this, cx| {
                    if let Phase::Recording { since, .. } = this.phase {
                        if since.elapsed() > Duration::from_secs(MAX_SECS) {
                            this.stop(cx);
                            return false;
                        }
                        cx.notify();
                        true
                    } else {
                        false
                    }
                });
                if !matches!(keep, Ok(true)) {
                    break;
                }
            }
        });
        self.work.retain(|t| !t.is_ready());
        self.work.push(t);
        cx.emit(DictationEvent::Changed);
        cx.notify();
    }

    /// Drop the capture without processing (mic failed to open).
    fn abandon(&mut self) {
        if let Some(cap) = self.capture.take() {
            drop(cap.live);
            let _ = cap.stop.send(());
            let _ = cap.writer.join();
        }
    }

    /// Stop recording and process.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(cap) = self.capture.take() else { return };
        let started = match self.phase {
            Phase::Recording { since, .. } => since,
            _ => Instant::now(),
        };
        self.hotkeys.set_capture_escape(false);
        let cfg = self.config(cx);
        // Stop capture: drop the stream first (its callback stops feeding), then
        // let the writer drain.
        let Capture { samples, live, batch_model, writer, stop, app, selected } = cap;
        let _ = stop.send(());
        let _ = writer.join();
        if cfg.dictation.sounds {
            stenly_platform::chime::play(stenly_platform::chime::Chime::Stop);
        }
        let audio: Vec<f32> = std::mem::take(&mut *samples.lock().unwrap());
        let secs = audio.len() as f32 / 16_000.0;
        tracing::info!(secs, held = started.elapsed().as_secs_f32(), "dictation stopped");
        // An accidental tap (less than ~0.6 s of audio) is discarded without a
        // notice: pasting "Yeah." because you brushed Right ⌘ would be worse.
        if secs < 0.6 {
            drop(live);
            tracing::info!(secs, "dictation too short; discarded");
            self.phase = Phase::Idle;
            self.update_activity = None;
            self.active_slot = None;
            self.states.iter_mut().for_each(ShortcutState::reset);
            cx.emit(DictationEvent::Changed);
            cx.notify();
            return;
        }
        self.phase = Phase::Transcribing;
        cx.emit(DictationEvent::Changed);
        cx.notify();
        let injector = self.injector.clone();
        let task = cx.spawn(async move |this, cx: &mut AsyncApp| {
            let cfg2 = cfg.clone();
            let result = cx
                .background_spawn(async move {
                    let t0 = Instant::now();
                    // Streaming: only the last few seconds are left to decode.
                    let streamed = live.map(|l| l.finish()).and_then(|r| r.inspect_err(|e| tracing::warn!(error = %format!("{e:#}"), "live dictation failed; transcribing the clip")).ok());
                    let raw = match streamed {
                        Some((text, st)) => {
                            tracing::info!(decodes = st.decodes, live_secs = st.live_secs, max_live = st.max_live_decode, tail_audio = st.tail_audio, tail_secs = st.tail_secs, "dictation stream");
                            text
                        }
                        None => {
                            let asr = match batch_model {
                                Some(rx) => rx.recv().map_err(|_| anyhow::anyhow!("speech model loader stopped"))??,
                                None => stenly_asr::DICTATION_ASR.get(&cfg2.asr, &cfg2.paths().models_dir)?,
                            };
                            let text = asr.transcribe_clip(&audio);
                            asr.check_health()?;
                            text
                        },
                    };
                    let text = dictation::clean(&raw, &cfg2.dictation, &cfg2.asr.vocabulary);
                    tracing::info!(secs, took = t0.elapsed().as_secs_f32(), chars = text.len(), stream = ?cfg2.dictation.stream, "dictation transcribed");
                    anyhow::Ok((text, audio))
                })
                .await;
            let (text, audio) = match result {
                Ok(v) => v,
                Err(e) => {
                    let _ = this.update(cx, |this, cx| this.notice(format!("Transcription failed: {e:#}"), cx));
                    return;
                }
            };
            if text.trim().is_empty() {
                let _ = this.update(cx, |this, cx| this.notice_quiet(cx));
                return;
            }
            let mut enhanced = None;
            if cfg.dictation.ai_cleanup {
                let _ = this.update(cx, |this, cx| {
                    this.phase = Phase::Enhancing;
                    cx.notify();
                });
                let (c, t, sel) = (cfg.clone(), text.clone(), selected.clone());
                match cx.background_spawn(async move { dictation::ai_cleanup_with(&c, &t, sel.as_deref()) }).await {
                    Ok(t) => enhanced = Some(t),
                    Err(e) => tracing::warn!(error = %format!("{e:#}"), "AI clean-up failed; pasting the plain transcript"),
                }
            }
            let out = enhanced.clone().unwrap_or_else(|| text.clone());
            // Paste on a background thread (clipboard restore sleeps).
            let restore = cfg.dictation.restore_clipboard.then(|| Duration::from_secs_f32(cfg.dictation.restore_clipboard_secs.clamp(0.3, 10.0)));
            let auto_send = cfg.dictation.auto_send;
            let inj = injector.clone();
            let to_paste = format!("{out} ");
            // Dev/test guard: STENLY_PASTE_ONLY_INTO=<app name> pastes only when that
            // app has focus (automated tests must never type into the user's apps).
            if let Ok(only) = std::env::var("STENLY_PASTE_ONLY_INTO") {
                let front = injector.frontmost_app().unwrap_or_default();
                if front != only {
                    tracing::warn!(%front, %only, "test guard: not pasting into this app");
                    let _ = this.update(cx, |this, cx| this.notice(format!("(test) skipped paste into {front}"), cx));
                    return;
                }
            }
            tracing::info!(chars = to_paste.len(), ai = cfg.dictation.ai_cleanup, "dictation pasting");
            let pasted = cx
                .background_spawn(async move {
                    inj.paste(&to_paste, restore)?;
                    if auto_send {
                        std::thread::sleep(Duration::from_millis(180));
                        inj.press_return()?;
                    }
                    anyhow::Ok(())
                })
                .await;
            // History (with audio, for re-transcribing).
            let at = chrono::Local::now();
            let limit = cfg.dictation.history_limit;
            let audio_path = (limit > 0).then(|| dictation::audio_path(at));
            let entry = Dictation { at, duration: secs, text, enhanced, app, audio: audio_path.clone() };
            // Retain an additional lease through detached durable history/WAV
            // persistence, including paste-error paths.
            let persistence = stenly_core::update::Activity::begin();
            cx.background_spawn(async move {
                let _persistence = persistence;
                if let Some(p) = &audio_path {
                    if dictation::ensure_audio_dir().is_ok() {
                        if let Ok(mut w) = WavSink::create(p) {
                            let _ = w.write(&audio);
                            let _ = w.finish();
                        }
                    }
                }
                if let Err(e) = dictation::record(&entry, limit) {
                    tracing::warn!(error = %format!("{e:#}"), "couldn't save dictation history");
                }
            })
            .detach();
            if let Err(e) = &pasted {
                tracing::warn!(error = %format!("{e:#}"), "dictation paste failed");
            } else {
                tracing::info!("dictation pasted");
            }
            let _ = this.update(cx, |this, cx| match pasted {
                Ok(()) => {
                    this.phase = Phase::Idle;
                    this.update_activity = None;
                    this.active_slot = None;
                    cx.emit(DictationEvent::Changed);
                    cx.notify();
                }
                Err(e) => this.notice(format!("{e:#}. The text is on your clipboard."), cx),
            });
        });
        self.work.retain(|t| !t.is_ready());
        self.work.push(task);
    }

    /// Esc or the HUD's ✕: throw the recording away.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.phase, Phase::Recording { .. }) {
            return;
        }
        let sounds = self.config(cx).dictation.sounds;
        self.cancel_silently(cx);
        if sounds {
            stenly_platform::chime::play(stenly_platform::chime::Chime::Cancel);
        }
    }

    fn cancel_silently(&mut self, cx: &mut Context<Self>) {
        if let Some(Capture { writer, stop, live, .. }) = self.capture.take() {
            drop(live);
            let _ = stop.send(());
            let _ = writer.join();
        }
        self.hotkeys.set_capture_escape(false);
        self.states.iter_mut().for_each(ShortcutState::reset);
        self.active_slot = None;
        self.phase = Phase::Idle;
        self.update_activity = None;
        tracing::info!("dictation cancelled");
        cx.emit(DictationEvent::Changed);
        cx.notify();
    }

    /// Paste the most recent dictation again (⌥⌘V by default).
    pub fn paste_last(&mut self, cx: &mut Context<Self>) {
        let restore = {
            let d = self.config(cx).dictation;
            d.restore_clipboard.then(|| Duration::from_secs_f32(d.restore_clipboard_secs.clamp(0.3, 10.0)))
        };
        let inj = self.injector.clone();
        cx.background_spawn(async move {
            if let Some(d) = dictation::last() {
                // wait for the shortcut's own modifiers to be released
                std::thread::sleep(Duration::from_millis(120));
                if let Err(e) = inj.paste(&format!("{} ", d.output()), restore) {
                    tracing::warn!(error = %format!("{e:#}"), "paste last failed");
                }
            }
        })
        .detach();
    }

    /// Re-run a saved dictation through the current model + clean-up settings
    /// (after adding jargon or switching models). Updates history; returns the
    /// new text via the callback.
    pub fn retranscribe(&mut self, entry: Dictation, cx: &mut Context<Self>, done: impl FnOnce(Option<Dictation>, &mut App) + 'static) {
        let cfg = self.config(cx);
        let task = cx.spawn(async move |_, cx: &mut AsyncApp| {
            let r = cx
                .background_spawn(async move {
                    let path = entry.audio.clone().ok_or_else(|| anyhow::anyhow!("no audio saved for this dictation"))?;
                    let audio = stenly_core::audio::read_wav_16k_mono(&path)?;
                    let asr = stenly_asr::DICTATION_ASR.get(&cfg.asr, &cfg.paths().models_dir)?;
                    let text = dictation::clean(&asr.transcribe_clip(&audio), &cfg.dictation, &cfg.asr.vocabulary);
                    asr.check_health()?;
                    let enhanced = if cfg.dictation.ai_cleanup { dictation::ai_cleanup(&cfg, &text).ok() } else { None };
                    let new = Dictation { text, enhanced, ..entry };
                    dictation::update(&new)?;
                    anyhow::Ok(new)
                })
                .await;
            if let Err(e) = &r {
                tracing::warn!(error = %format!("{e:#}"), "re-transcribe failed");
            }
            let _ = cx.update(|cx| done(r.ok(), cx));
        });
        self.work.retain(|t| !t.is_ready());
        self.work.push(task);
    }

    /// Re-paste / copy an entry from history.
    pub fn copy_text(&self, text: &str) {
        let _ = self.injector.copy(text);
    }

    fn notice_quiet(&mut self, cx: &mut Context<Self>) {
        self.notice("Didn't catch that".into(), cx);
    }

    fn notice(&mut self, msg: String, cx: &mut Context<Self>) {
        self.hotkeys.set_capture_escape(false);
        self.active_slot = None;
        self.states.iter_mut().for_each(ShortcutState::reset);
        self.last_error = Some(msg.clone());
        self.phase = Phase::Notice(msg);
        self.update_activity = None;
        cx.emit(DictationEvent::Changed);
        cx.notify();
        let t = cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor().timer(Duration::from_millis(1800)).await;
            let _ = this.update(cx, |this, cx| {
                if matches!(this.phase, Phase::Notice(_)) {
                    this.phase = Phase::Idle;
                    cx.emit(DictationEvent::Changed);
                    cx.notify();
                }
            });
        });
        self.work.retain(|t| !t.is_ready());
        self.work.push(t);
    }
}

pub fn new(controller: Entity<Controller>, audio: Arc<dyn AudioBackend>, hotkeys: Arc<dyn Hotkeys>, injector: Arc<dyn TextInjector>, cx: &mut App) -> Entity<DictationController> {
    cx.new(|cx| DictationController::new(controller, audio, hotkeys, injector, cx))
}
