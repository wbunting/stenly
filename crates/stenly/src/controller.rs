//! App state + behaviour, independent of any view. Owns the recording, runs
//! the scheduler, kicks off background processing. Views observe it.

use chrono::{DateTime, Local, Utc};
use gpui::{App, AppContext, AsyncApp, Context, Entity, EventEmitter, Task};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use stenly_core::calendar::{self, CalendarEvent, CalendarInfo};
use stenly_core::config::Config;
use stenly_core::daemon::{self, Progress};
use stenly_core::platform::{AudioBackend, CalendarSource, DesktopShell, MicActivity, MicUser, Permission, PermissionState, TrayState};
use stenly_core::scheduler::{Action, Prompt, RecordingInfo, Scheduler, Tick};
use stenly_core::session::{Recording, SessionMeta, list_sessions, pending_sessions};

#[derive(Clone, Debug)]
pub enum Status {
    Idle,
    Recording { title: String, since: DateTime<Local> },
}

#[derive(Clone, Debug)]
pub struct Job {
    pub title: String,
    pub progress: String,
}

/// A model download in flight (speech or summary).
#[derive(Clone, Debug)]
pub struct Download {
    pub id: String,
    pub fraction: f32,
    pub status: String,
}

#[derive(Clone, Debug, Default)]
pub enum KodyState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Failed(String),
}

#[derive(Clone, Debug, Default)]
pub struct OllamaState {
    pub running: bool,
    pub installed: Vec<String>,
    pub checked: bool,
}

pub enum ControllerEvent {
    /// Show the "record this?" popup.
    Prompt(Prompt),
    /// Ask whether to stop the current recording.
    SuggestStop(String),
    /// A note was written.
    NoteReady(PathBuf),
    /// Close any open "record?" / "stop?" popups.
    DismissPopups,
}

pub struct Controller {
    pub config: Config,
    pub status: Status,
    pub events: Vec<CalendarEvent>,
    pub calendar_errors: Vec<String>,
    pub calendar_fetched: Option<DateTime<Utc>>,
    pub mic_users: Vec<MicUser>,
    /// Display showing the call app's window (platform display id), so the
    /// recording pill and prompts appear on the same screen as the meeting.
    pub call_display: Option<u64>,
    pub jobs: Vec<Job>,
    pub recent: Vec<(PathBuf, SessionMeta)>,
    /// Invalidation only: full-library views must not inherit the Recent cap.
    pub library_revision: u64,
    pub last_error: Option<String>,
    pub update_status: String,
    pub update_download: Option<String>,
    pub levels: Option<Arc<stenly_core::session::Levels>>,
    recording: Option<Recording>,
    update_activity: Option<stenly_core::update::Activity>,
    /// Transcribes the current recording as it happens.
    live: Option<stenly_asr::LiveSession>,
    /// Live transcript so far (for the UI); shared with the live worker.
    pub live_segments: Option<std::sync::Arc<std::sync::Mutex<Vec<stenly_core::transcript::Segment>>>>,
    /// Reads who's talking from the meeting app (speaker names), if supported.
    pub meeting_ax: Option<Arc<dyn stenly_core::speakers::ax::MeetingAxSource>>,
    /// Logs the meeting app's speaker highlight while recording.
    speaker_watch: Option<stenly_core::speakers::timeline::Watcher>,
    scheduler: Scheduler,
    audio: Arc<dyn AudioBackend>,
    mic_activity: Arc<dyn MicActivity>,
    processing_queue: Vec<PathBuf>,
    processing: bool,
    /// The session being processed right now (not in the queue any more).
    processing_dir: Option<PathBuf>,
    pub shell: std::rc::Rc<dyn DesktopShell>,
    pub downloads: Vec<Download>,
    pub ollama: OllamaState,
    pub kody: KodyState,
    pub calendars: Vec<CalendarInfo>,
    calendar_source: Option<Arc<dyn CalendarSource>>,
    /// None = not loaded yet; Some(Err) = last attempt failed.
    pub calendars_status: Option<Result<(), String>>,
    calendars_loading: bool,
    calendars_retry: Option<Task<()>>,
    pub permissions: Vec<(Permission, PermissionState)>,
    /// Permissions with a request in flight (prompt open / waiting for the OS).
    pub pending_permissions: Vec<Permission>,
    /// Hosted summary API: key present in the Keychain, and the last test.
    pub api_key_saved: bool,
    /// Result of the last VoiceInk import (shown on the Dictation page).
    pub import_note: Option<String>,
    pub api_test: ApiTest,
    /// Codex / Claude Code found on this Mac (Settings → Summaries, onboarding), and the last test of the chosen one.
    pub cli_tools: Vec<stenly_core::cli_llm::Detected>,
    pub cli_checked: bool,
    pub cli_test: ApiTest,
    pub chatgpt: ChatGptState,
    /// Set to abandon a sign-in that's waiting for the browser.
    chatgpt_cancel: Arc<std::sync::atomic::AtomicBool>,
    /// Latest Recent-list read (see [`Controller::refresh_recent`]).
    recent_task: Option<Task<()>>,
    /// A recording that has stopped but is still being finalized.
    finishing: Option<PathBuf>,
    /// Context gathering (Settings → Context; the meeting view's Context card).
    pub context: ContextState,
    _tasks: Vec<Task<()>>,
}

/// Context gathering in the app: runs in flight, the live tool log of each,
/// source status for Settings, sign-ins in progress.
#[derive(Default)]
pub struct ContextState {
    /// Session dirs or `event:<id>` keys being gathered, with their live log.
    pub running: Vec<(String, Vec<String>)>,
    /// Calendar events already gathered (or tried) this run.
    pub done_events: std::collections::HashSet<String>,
    /// Last failure per key.
    pub errors: Vec<(String, String)>,
    /// Settings: last source check (None = not checked yet).
    pub sources: Option<Vec<stenly_agent::SourceStatus>>,
    pub checking: bool,
    /// Server ids with a browser sign-in open.
    pub signing_in: Vec<String>,
    /// Facts the Settings page shows that need the Keychain or the disk,
    /// read off the main thread by [`Controller::check_context_sources`].
    pub facts: Option<ContextFacts>,
    /// A source check was asked for while one was running.
    pub recheck: bool,
    /// `context.json` per session dir, loaded off the main thread (the meeting
    /// view's Context card). `None` inside = the file doesn't exist.
    pub loaded: std::collections::HashMap<PathBuf, Option<stenly_core::context::MeetingContext>>,
}

#[derive(Debug, Clone, Default)]
pub struct ContextFacts {
    /// A Notion token is available (Settings → Context's own, or a destination's).
    pub has_notion: bool,
    /// The token is Settings → Context's own (not borrowed from a destination).
    pub own_notion: bool,
    /// Server ids with a stored sign-in or key.
    pub signed_in: Vec<String>,
    pub vault_root: PathBuf,
}

impl ContextState {
    pub fn is_running(&self, key: &str) -> bool {
        self.running.iter().any(|(k, _)| k == key)
    }
    pub fn log(&self, key: &str) -> Option<&[String]> {
        self.running.iter().find(|(k, _)| k == key).map(|(_, l)| l.as_slice())
    }
    pub fn error(&self, key: &str) -> Option<&str> {
        self.errors.iter().find(|(k, _)| k == key).map(|(_, e)| e.as_str())
    }
}

/// Keychain and disk lookups for Settings → Context (run in the background).
fn context_facts(cfg: &Config) -> ContextFacts {
    ContextFacts {
        own_notion: stenly_core::secrets::context_secret("notion").is_some(),
        has_notion: stenly_agent::tools::notion_token(cfg).is_some(),
        signed_in: stenly_agent::servers(cfg).into_iter().filter(|s| stenly_agent::oauth::has_sign_in(&s.id)).map(|s| s.id).collect(),
        vault_root: stenly_agent::tools::vault_root(cfg),
    }
}

/// One line for the live log ("Searching past meetings: guest.example.org").
pub fn describe_event(e: &stenly_agent::agent::Event) -> Option<String> {
    use stenly_agent::agent::Event;
    match e {
        Event::Tool { name, source, args } => {
            let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
            let what = ["who", "query", "path", "id"].iter().find_map(|k| v[*k].as_str().map(str::to_string)).unwrap_or_else(|| args.chars().take(60).collect());
            let verb = match name.as_str() {
                "past_meetings_with" => "Earlier meetings with",
                "past_meetings_search" => "Searching earlier meetings for",
                "past_meeting_get" => "Reading meeting",
                "vault_search" => "Searching notes for",
                "vault_read" => "Reading note",
                "notion_search" => "Searching Notion for",
                "notion_read" => "Reading Notion page",
                _ => return Some(format!("{source}: {}", name.split("__").last().unwrap_or(name))),
            };
            Some(format!("{verb} {what}"))
        }
        _ => None,
    }
}

/// Sign in with ChatGPT (Settings → Summaries, onboarding).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ChatGptState {
    #[default]
    SignedOut,
    /// The browser is open; waiting for it to come back.
    SigningIn,
    /// "alex@example.com · Plus"
    SignedIn(String),
    Failed(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum ApiTest {
    #[default]
    Idle,
    Running,
    Ok(String),
    Failed(String),
}

impl EventEmitter<ControllerEvent> for Controller {}

/// Display of the meeting app holding the mic. A native call app (Zoom,
/// Teams) wins over a browser, which may hold the mic for something else.
fn call_window_display(mic: &dyn MicActivity, apps: &[String], users: &[MicUser]) -> Option<u64> {
    use stenly_core::scheduler::{is_browser, is_meeting_app_in};
    let mut meeting: Vec<&MicUser> = users.iter().filter(|u| is_meeting_app_in(apps, u)).collect();
    meeting.sort_by_key(|u| is_browser(&u.app_id));
    meeting.into_iter().find_map(|u| mic.window_display(u))
}

impl Controller {
    pub fn new(
        config: Config,
        audio: Arc<dyn AudioBackend>,
        mic_activity: Arc<dyn MicActivity>,
        shell: std::rc::Rc<dyn DesktopShell>,
        calendar_source: Option<Arc<dyn CalendarSource>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            config,
            status: Status::Idle,
            events: vec![],
            calendar_errors: vec![],
            calendar_fetched: None,
            mic_users: vec![],
            call_display: None,
            jobs: vec![],
            recent: vec![],
            library_revision: 0,
            last_error: None,
            update_status: String::new(),
            update_download: None,
            levels: None,
            recording: None,
            update_activity: None,
            meeting_ax: None,
            speaker_watch: None,
            live: None,
            live_segments: None,
            scheduler: Scheduler::default(),
            audio,
            mic_activity,
            processing_queue: vec![],
            processing_dir: None,
            processing: false,
            shell,
            downloads: vec![],
            ollama: OllamaState::default(),
            kody: if stenly_core::kody::KodyAuth::load().is_some() { KodyState::Connected } else { KodyState::Disconnected },
            calendars: vec![],
            calendar_source,
            calendars_status: None,
            calendars_loading: false,
            calendars_retry: None,
            permissions: vec![],
            pending_permissions: vec![],
            api_key_saved: stenly_core::secrets::llm_api_key().is_some(),
            import_note: None,
            api_test: ApiTest::Idle,
            cli_tools: vec![],
            cli_checked: false,
            cli_test: ApiTest::Idle,
            chatgpt: match stenly_core::chatgpt::signed_in() {
                Some(a) => ChatGptState::SignedIn(a.label()),
                None => ChatGptState::SignedOut,
            },
            chatgpt_cancel: Arc::default(),
            recent_task: None,
            finishing: None,
            context: ContextState::default(),
            _tasks: vec![],
        };
        this.refresh_recent(cx);
        this.refresh_permissions();
        // Dev: STENLY_FAKE_PENDING=1 shows the pending state for screenshots.
        if std::env::var_os("STENLY_FAKE_PENDING").is_some() {
            this.pending_permissions = vec![Permission::SystemAudio];
        }
        this.refresh_ollama(cx);
        this.refresh_cli_tools(cx);
        if this.uses_eventkit() || matches!(this.kody, KodyState::Connected) {
            this.load_calendars(cx);
        }
        this.start_loops(cx);
        // Recover anything left over from a crash / quit mid-processing, and
        // prune old audio. Both read every meeting's files: off the main thread.
        let cfg = this.config.clone();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let pending = cx
                .background_spawn(async move {
                    let pending = pending_sessions(&cfg.paths().audio_dir);
                    daemon::prune_audio(&cfg);
                    pending
                })
                .await;
            let _ = this.update(cx, |c, cx| {
                for d in pending {
                    // (a recording started meanwhile is also "pending": skip it)
                    let live = c.recording.as_ref().is_some_and(|r| r.dir == d) || c.finishing.as_ref() == Some(&d);
                    if !live && !c.processing_queue.contains(&d) && c.processing_dir.as_ref() != Some(&d) {
                        c.processing_queue.push(d);
                    }
                }
                c.pump_processing(cx);
            });
        })
        .detach();
        this
    }

    fn start_loops(&mut self, cx: &mut Context<Self>) {
        // Scheduler tick: mic activity + prompts. Every 500 ms while recording so
        // "call ended" is noticed within ~1 s of the meeting app hanging up;
        // every 2 s otherwise.
        let tick = cx.spawn(async move |this, cx: &mut AsyncApp| {
            let mut due: Option<(std::time::Instant, Duration)> = None;
            loop {
                // Watchdog: this loop runs on the main thread, so arriving far
                // later than scheduled means the UI was frozen that long.
                if let Some((at, wait)) = due {
                    let late = at.elapsed().saturating_sub(wait);
                    if late > Duration::from_secs(2) {
                        tracing::warn!(secs = late.as_secs_f32(), "main thread was blocked");
                    }
                }
                let Ok((mic, apps)) = this.read_with(cx, |c, _| (c.mic_activity.clone(), c.config.detect.meeting_apps.clone())) else { return };
                let (users, call_display) = cx
                    .background_spawn(async move {
                        let users = mic.mic_users().unwrap_or_default();
                        // Dev: STENLY_CALL_APP=<bundle id> treats that app's window as the call.
                        let d = match std::env::var("STENLY_CALL_APP") {
                            Ok(app_id) => mic.window_display(&MicUser { pid: None, app_id }),
                            Err(_) => call_window_display(mic.as_ref(), &apps, &users),
                        };
                        (users, d)
                    })
                    .await;
                let Ok(recording) = this.update(cx, |c, cx| {
                    if c.call_display != call_display {
                        tracing::info!(?call_display, "call window display");
                        c.call_display = call_display;
                    }
                    c.on_tick(users, cx);
                    c.is_recording()
                }) else {
                    return;
                };
                let wait = Duration::from_millis(if recording { 500 } else { 2000 });
                due = Some((std::time::Instant::now(), wait));
                cx.background_executor().timer(wait).await;
            }
        });
        // Calendar refresh.
        let cal = cx.spawn(async move |this, cx: &mut AsyncApp| {
            loop {
                let Ok((cfg, source)) = this.read_with(cx, |c, _| (c.config.calendar.clone(), c.calendar_source.clone())) else { return };
                let mins = cfg.refresh_mins.max(1) as u64;
                let now = Utc::now();
                let (events, errors) = cx
                    .background_spawn(async move {
                        let from = now - chrono::Duration::hours(1);
                        let to = now + chrono::Duration::days(7);
                        let (mut events, mut errors) = calendar::upcoming(&cfg, from, to);
                        if cfg.source.uses_eventkit() {
                            match source {
                                Some(source) => match source.events(&cfg.calendar_ids, from, to) {
                                    Ok(mut eventkit) => events.append(&mut eventkit),
                                    Err(e) => errors.push(format!("Calendars on this Mac: {e:#}")),
                                },
                                None => errors.push("Calendars on this Mac are unavailable on this platform. Use an ICS calendar link instead.".into()),
                            }
                        }
                        (calendar::filter_events(&cfg, events), errors)
                    })
                    .await;
                let failed = !errors.is_empty();
                // Errors that mean "couldn't reach the source at all"; per-calendar
                // problems (one 404'd calendar) still count as a completed sync.
                let unreachable = errors.iter().any(|e| e.starts_with("Google Calendar (via Kody)") || e.contains("isn't signed in"));
                if this
                    .update(cx, |c, cx| {
                        // A failed fetch shouldn't wipe out the meetings we already know about.
                        if !(unreachable && events.is_empty()) {
                            c.events = events;
                        }
                        c.calendar_errors = errors;
                        if !unreachable {
                            c.calendar_fetched = Some(Utc::now());
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
                let wait = if failed { 60 } else { mins * 60 };
                cx.background_executor().timer(Duration::from_secs(wait)).await;
            }
        });
        // Context ahead of calendar meetings (Settings → Context).
        let ctx = cx.spawn(async move |this, cx: &mut AsyncApp| {
            stenly_core::context::prune_event_cache();
            loop {
                cx.background_executor().timer(Duration::from_secs(60)).await;
                if this.update(cx, |c, cx| c.gather_upcoming(cx)).is_err() {
                    return;
                }
            }
        });
        self._tasks = vec![tick, cal, ctx];
    }

    /// Start gathering for calendar meetings that begin within `before_mins`.
    fn gather_upcoming(&mut self, cx: &mut Context<Self>) {
        let c = &self.config.context;
        if !c.enabled || !self.config.llm.enabled {
            return;
        }
        let now = Utc::now();
        let soon: Vec<CalendarEvent> = self
            .events
            .iter()
            .filter(|e| !e.declined && e.start > now && e.start - now <= chrono::Duration::minutes(c.before_mins.max(1)))
            .filter(|e| !self.context.done_events.contains(&e.id) && stenly_core::context::load_for_event(&e.id).is_none())
            .take(2)
            .cloned()
            .collect();
        for ev in soon {
            self.context.done_events.insert(ev.id.clone());
            self.gather_event(ev, cx);
        }
    }

    fn gather_event(&mut self, ev: CalendarEvent, cx: &mut Context<Self>) {
        let key = format!("event:{}", ev.id);
        self.context.running.push((key.clone(), vec![]));
        let cfg = self.config.clone();
        let (tx, rx) = crossbeam_channel::unbounded::<String>();
        let id = ev.id.clone();
        let work = cx.background_spawn(async move {
            let seed = stenly_agent::seed_for_event(&cfg, Some(&ev));
            let scope = stenly_agent::tools::Scope { before: Some(ev.start.with_timezone(&Local)), ..Default::default() };
            let ctx = stenly_agent::gather(&cfg, &seed, &scope, &mut |e| {
                if let Some(l) = describe_event(&e) {
                    let _ = tx.send(l);
                }
            });
            stenly_core::context::save_for_event(&id, &ctx).map(|_| ctx)
        });
        self.track_context(key, work, rx, None, cx);
    }

    /// Gather (again) for a recorded meeting, then rewrite its note.
    pub fn gather_context(&mut self, dir: PathBuf, then_reprocess: bool, cx: &mut Context<Self>) {
        let key = dir.to_string_lossy().to_string();
        if self.context.is_running(&key) {
            return;
        }
        self.context.running.push((key.clone(), vec![]));
        self.context.errors.retain(|(k, _)| k != &key);
        let cfg = self.config.clone();
        let (tx, rx) = crossbeam_channel::unbounded::<String>();
        let d = dir.clone();
        let work = cx.background_spawn(async move {
            stenly_agent::gather_for_session(&cfg, &d, &mut |e| {
                if let Some(l) = describe_event(&e) {
                    let _ = tx.send(l);
                }
            })
        });
        self.track_context(key, work, rx, Some((dir, then_reprocess)), cx);
        cx.notify();
    }

    /// Follow a gather: stream its log into the UI; when it ends, record the
    /// outcome, refresh the meeting's cached context and (if asked) rewrite its note.
    fn track_context(
        &mut self,
        key: String,
        work: Task<anyhow::Result<stenly_core::context::MeetingContext>>,
        rx: crossbeam_channel::Receiver<String>,
        session: Option<(PathBuf, bool)>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let k = key.clone();
            let poll = async {
                loop {
                    cx.background_executor().timer(Duration::from_millis(250)).await;
                    let lines: Vec<String> = rx.try_iter().collect();
                    if !lines.is_empty() {
                        let _ = this.update(cx, |c, cx| {
                            if let Some((_, log)) = c.context.running.iter_mut().find(|(x, _)| x == &k) {
                                log.extend(lines);
                            }
                            cx.notify();
                        });
                    }
                }
            };
            let result = futures_race(work, poll).await;
            let _ = this.update(cx, |c, cx| {
                c.context.running.retain(|(x, _)| x != &key);
                match result {
                    Ok(ctx) => {
                        if let Some(note) = ctx.gathered.as_ref().and_then(|g| g.note.clone()).filter(|_| ctx.is_empty()) {
                            c.context.errors.push((key.clone(), note));
                        }
                        if let Some((dir, reprocess)) = session.clone() {
                            c.context.loaded.insert(dir.clone(), Some(ctx.clone()));
                            if reprocess && !ctx.failed() {
                                c.reprocess(dir, cx);
                            }
                        }
                    }
                    Err(e) => c.context.errors.push((key.clone(), format!("{e:#}"))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Settings → Context: connect every source and list its tools.
    pub fn check_context_sources(&mut self, cx: &mut Context<Self>) {
        // the cheap Keychain / disk facts refresh right away, even mid-check
        self.refresh_context_facts(cx);
        self.connect_context_sources(cx);
    }

    /// The slow half of a source check: connect every source and list its tools.
    fn connect_context_sources(&mut self, cx: &mut Context<Self>) {
        if self.context.checking {
            // settings changed during a check: run one more when it ends
            self.context.recheck = true;
            return;
        }
        self.context.checking = true;
        let cfg = self.config.clone();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            // facts come from refresh_context_facts; this only connects sources
            let sources = cx.background_spawn(async move { stenly_agent::toolset(&cfg, &Default::default()).sources }).await;
            let _ = this.update(cx, |c, cx| {
                c.context.checking = false;
                c.context.sources = Some(sources);
                if std::mem::take(&mut c.context.recheck) {
                    c.connect_context_sources(cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Notion token, sign-ins and vault path, read in the background.
    pub fn refresh_context_facts(&mut self, cx: &mut Context<Self>) {
        let cfg = self.config.clone();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let facts = cx.background_spawn(async move { context_facts(&cfg) }).await;
            let _ = this.update(cx, |c, cx| {
                c.context.facts = Some(facts);
                cx.notify();
            });
        })
        .detach();
    }

    /// The meeting view's context: read `context.json` in the background the
    /// first time a meeting is shown (iCloud reads can block).
    pub fn load_meeting_context(&mut self, dir: &std::path::Path, cx: &mut Context<Self>) {
        if self.context.loaded.contains_key(dir) {
            return;
        }
        // placeholder until the read finishes, so it isn't started twice
        self.context.loaded.insert(dir.to_path_buf(), None);
        self.reload_meeting_context(dir, cx);
    }

    /// Re-read a meeting's `context.json` in the background (it may have
    /// changed); the cached copy stays on screen until the read finishes.
    pub fn reload_meeting_context(&mut self, dir: &std::path::Path, cx: &mut Context<Self>) {
        let d = dir.to_path_buf();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let d2 = d.clone();
            let ctx = cx.background_spawn(async move { stenly_core::context::load(&d2) }).await;
            let _ = this.update(cx, |c, cx| {
                c.context.loaded.insert(d, ctx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Browser sign-in for an MCP server.
    pub fn sign_in_context_server(&mut self, id: String, url: String, cx: &mut Context<Self>) {
        if self.context.signing_in.contains(&id) {
            return;
        }
        self.context.signing_in.push(id.clone());
        let (url_tx, url_rx) = crossbeam_channel::bounded::<String>(1);
        let sid = id.clone();
        let work = cx.background_spawn(async move {
            stenly_agent::oauth::sign_in(&sid, &url, &|u| {
                let _ = url_tx.send(u.to_string());
            })
        });
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let opener = async {
                loop {
                    cx.background_executor().timer(Duration::from_millis(100)).await;
                    if let Ok(u) = url_rx.try_recv() {
                        let _ = cx.update(|cx| cx.open_url(&u));
                    }
                }
            };
            let result = futures_race(work, opener).await;
            let _ = this.update(cx, |c, cx| {
                c.context.signing_in.retain(|x| x != &id);
                if let Err(e) = result {
                    c.last_error = Some(format!("Couldn't sign in to {id}: {e:#}"));
                } else {
                    cx.activate(true);
                }
                c.check_context_sources(cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Replace `[[context.servers]]`.
    pub fn save_context_servers(&mut self, list: Vec<stenly_core::config::McpServerConfig>, cx: &mut Context<Self>) {
        match Config::save_context_servers(&list) {
            Ok(cfg) => {
                // reload_config refreshes the facts (the server list changed)
                self.reload_config(cfg, cx);
                self.connect_context_sources(cx);
            }
            Err(e) => self.last_error = Some(format!("Couldn't save settings: {e:#}")),
        }
        cx.notify();
    }

    pub fn reload_config(&mut self, config: Config, cx: &mut Context<Self>) {
        let calendar_changed = config.calendar != self.config.calendar;
        // the vault path and Notion token come from these (Settings → Context shows them)
        let facts_changed = config.output.notes_dir != self.config.output.notes_dir
            || config.context.vault_dir != self.config.context.vault_dir
            || config.destinations != self.config.destinations
            || config.context.servers != self.config.context.servers;
        self.config = config;
        if facts_changed && self.context.facts.is_some() {
            self.refresh_context_facts(cx);
        }
        if calendar_changed {
            self.start_loops(cx);
        }
        self.refresh_recent(cx);
        cx.notify();
    }

    #[allow(dead_code)]
    pub fn refresh_calendar(&mut self, cx: &mut Context<Self>) {
        self.start_loops(cx);
    }

    /// Re-read the Recent list off the main thread: the recordings folder may
    /// be in iCloud Drive, where reading an evicted meta.json downloads it
    /// first (a second or more each), which froze the app when a call ended.
    fn refresh_recent(&mut self, cx: &mut Context<Self>) {
        let root = self.config.paths().audio_dir;
        // Replacing the task drops (cancels) an older, slower read.
        self.recent_task = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            let recent = cx.background_spawn(async move { list_sessions(&root, 30) }).await;
            let _ = this.update(cx, |c, cx| {
                c.recent = recent;
                c.library_revision = c.library_revision.wrapping_add(1);
                cx.notify();
            });
        }));
    }

    fn on_tick(&mut self, users: Vec<MicUser>, cx: &mut Context<Self>) {
        self.mic_users = users;
        let rec_event = self.recording.as_ref().and_then(|r| r.meta.event.clone());
        let recording = self.recording.as_ref().map(|r| RecordingInfo {
            event: rec_event.as_ref(),
            started: r.meta.started.with_timezone(&Utc),
            secs_since_voice: r.levels.secs_since_voice(),
            call_window_gone_secs: self.speaker_watch.as_ref().and_then(|w| w.presence.gone_secs()),
        });
        let actions = self.scheduler.tick(
            &self.config,
            Tick { now: Utc::now(), events: &self.events, mic_users: &self.mic_users, recording },
        );
        for a in actions {
            tracing::info!(action = ?a, "scheduler action");
            match a {
                Action::ShowPrompt(p) => cx.emit(ControllerEvent::Prompt(p)),
                Action::AutoStart(ev) => self.start_recording(Some(ev), cx),
                Action::SuggestStop { reason } => cx.emit(ControllerEvent::SuggestStop(reason)),
                Action::AutoStop { reason } => {
                    tracing::info!(%reason, "stopping recording automatically");
                    self.stop_recording(cx);
                    cx.emit(ControllerEvent::DismissPopups);
                }
                Action::DismissPrompt => cx.emit(ControllerEvent::DismissPopups),
            }
        }
        cx.notify();
    }

    pub fn snooze(&mut self, p: &Prompt) {
        self.scheduler.snooze(p);
    }

    /// True while a stopped recording is being finalized or a
    /// transcription/summary job is running.
    pub fn is_processing(&self) -> bool {
        self.processing || self.finishing.is_some()
    }

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    pub fn next_event(&self) -> Option<&CalendarEvent> {
        let now = Utc::now();
        self.events.iter().find(|e| e.end > now)
    }

    /// Short text next to the menubar icon.
    pub fn tray_title(&self) -> Option<String> {
        match &self.status {
            Status::Recording { since, .. } => {
                let m = (Local::now() - *since).num_minutes();
                Some(if m < 60 { format!("{m}m") } else { format!("{}h{:02}", m / 60, m % 60) })
            }
            Status::Idle if self.config.app.show_next_in_menubar => {
                let e = self.next_event()?;
                let mins = (e.start - Utc::now()).num_minutes();
                if !(-5..=180).contains(&mins) {
                    return None;
                }
                let mut t: String = e.title.chars().take(22).collect();
                if e.title.chars().count() > 22 {
                    t.push('…');
                }
                Some(if mins <= 0 { format!("{t} · now") } else if mins < 60 { format!("{t} in {mins}m") } else { format!("{t} in {}h{:02}", mins / 60, mins % 60) })
            }
            _ => None,
        }
    }

    pub fn tray_state(&self) -> TrayState {
        match &self.status {
            Status::Recording { title, since } => TrayState::Recording { title: title.clone(), since: *since },
            Status::Idle => match self.jobs.first() {
                Some(j) => TrayState::Processing { title: j.title.clone() },
                None => TrayState::Idle {
                    next: self.next_event().map(|e| format!("{} {}", e.start.with_timezone(&Local).format("%H:%M"), e.title)),
                },
            },
        }
    }

    pub fn start_recording(&mut self, event: Option<CalendarEvent>, cx: &mut Context<Self>) {
        if self.recording.is_some() {
            return;
        }
        let Some(activity) = stenly_core::update::Activity::begin() else {
            self.last_error = Some("An update is being installed. Try recording again after it finishes.".into());
            cx.notify();
            return;
        };
        let title = event
            .as_ref()
            .map(|e| e.title.clone())
            .unwrap_or_else(|| format!("Meeting {}", Local::now().format("%H:%M")));
        // Transcribe during the meeting so the note is ready moments after it ends.
        // Needs the model on disk; without it we fall back to transcribing afterwards.
        let paths = self.config.paths();
        let live_dir = paths.audio_dir.clone();
        let model_ready = stenly_asr::models::find(&self.config.asr.model).is_some_and(|m| stenly_asr::models::is_installed(m, &paths.models_dir));
        let (live_tx, live_rx) = crossbeam_channel::unbounded();
        // the call app holding the mic right now (Zoom, Teams…), for an ad-hoc call's platform
        let meeting_app = self.mic_users.iter().find_map(|u| stenly_core::speakers::drivers::for_app_id(&u.app_id)).map(|d| d.id().to_string());
        match Recording::start_full(self.audio.as_ref(), &self.config.audio.mic_priority, &live_dir, &title, event, model_ready.then_some(live_tx)) {
            Ok(mut rec) => {
                self.update_activity = Some(activity);
                if stenly_core::context::adopt_event_context(&rec.dir, &rec.meta) {
                    tracing::info!("using context gathered before the meeting");
                }
                if meeting_app.is_some() {
                    rec.meta.meeting_app = meeting_app;
                    let _ = rec.meta.save(&rec.dir);
                }
                if model_ready {
                    match stenly_asr::LiveSession::start_from(&self.config.asr, &paths.models_dir, &rec.dir, live_rx) {
                        Ok(l) => {
                            self.live_segments = Some(l.segments.clone());
                            self.live = Some(l);
                        }
                        Err(e) => tracing::warn!(error = %format!("{e:#}"), "live transcription unavailable; will transcribe after"),
                    }
                }
                if self.config.asr.speaker_names {
                    if let Some(src) = self.meeting_ax.clone() {
                        if !self.shell.permission(Permission::Accessibility).eq(&PermissionState::Granted) {
                            tracing::warn!("Accessibility not allowed: speaker names won't use the meeting app (Settings → Permissions)");
                        }
                        let snapshots = self.config.asr.save_meeting_app_snapshots.then(|| rec.dir.join("ax"));
                        match stenly_core::speakers::timeline::Watcher::start_with(src, &rec.dir, rec.meta.started, snapshots) {
                            Ok(w) => self.speaker_watch = Some(w),
                            Err(e) => tracing::warn!(error = %e, "speaker highlight watcher failed"),
                        }
                    }
                }
                self.status = Status::Recording { title, since: rec.meta.started };
                self.levels = Some(rec.levels.clone());
                if let Some(e) = &rec.meta.error {
                    self.last_error = Some(e.clone());
                }
                self.recording = Some(rec);
            }
            Err(e) => self.last_error = Some(format!("{e:#}")),
        }
        cx.notify();
    }

    /// Start without an event, but adopt a calendar event that's live right now.
    pub fn start_now(&mut self, cx: &mut Context<Self>) {
        let now = Utc::now();
        let live = self
            .events
            .iter()
            .find(|e| e.start - chrono::Duration::minutes(5) <= now && now < e.end)
            .cloned();
        self.start_recording(live, cx);
    }

    pub fn stop_recording(&mut self, cx: &mut Context<Self>) {
        let Some(rec) = self.recording.take() else { return };
        let activity = self.update_activity.take();
        let watch = self.speaker_watch.take();
        self.status = Status::Idle;
        self.levels = None;
        let live = self.live.take();
        self.live_segments = None;
        // Everything below blocks (joining the capture, speaker and writer
        // threads, saving meta.json, decoding the last phrase), so it runs off
        // the main thread: the UI flips to "Finishing…" at once.
        self.finishing = Some(rec.dir.clone());
        self.jobs = vec![Job { title: rec.meta.title.clone(), progress: "Finishing transcript…".into() }];
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let r = cx
                .background_spawn(async move {
                    if let Some(w) = watch {
                        w.stop();
                    }
                    let (dir, _) = rec.stop()?;
                    // Writers are joined, so the live worker has all the audio:
                    // let it decode the last phrase.
                    if let Some(live) = live
                        && let Err(e) = live.finish().and_then(|segs| stenly_asr::mark_live_complete(&dir, &segs))
                    {
                        tracing::warn!(error = %format!("{e:#}"), "live transcript unusable; transcribing from the recording");
                    }
                    // Keep the capture lease until audio writers and the last
                    // live phrase are durably finalized, even if UI tasks drop.
                    drop(activity);
                    anyhow::Ok(dir)
                })
                .await;
            let _ = this.update(cx, |c, cx| {
                c.finishing = None;
                c.jobs.clear();
                match r {
                    Ok(dir) => {
                        c.processing_queue.push(dir);
                        c.pump_processing(cx);
                    }
                    Err(e) => c.last_error = Some(format!("{e:#}")),
                }
                c.refresh_recent(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Whether a meeting can be trashed: not mid-recording or mid-processing.
    pub fn can_trash(&self, dir: &std::path::Path) -> bool {
        self.recording.as_ref().is_none_or(|r| r.dir != dir) && self.processing_dir.as_deref() != Some(dir) && self.finishing.as_deref() != Some(dir)
    }

    /// Move a meeting (audio, transcript, summary) to Stenly's Trash. The
    /// note written elsewhere (Obsidian, destinations) is left alone. Returns
    /// where it went, for Undo.
    pub fn trash_meeting(&mut self, dir: PathBuf, cx: &mut Context<Self>) -> anyhow::Result<PathBuf> {
        anyhow::ensure!(self.can_trash(&dir), "this meeting is still recording or processing");
        self.processing_queue.retain(|d| d != &dir);
        let to = stenly_core::session::trash_session(&self.config.paths().audio_dir, &dir)?;
        tracing::info!(dir = %dir.display(), "meeting moved to trash");
        self.refresh_recent(cx);
        cx.notify();
        Ok(to)
    }

    /// Put a trashed meeting back. Returns its folder.
    pub fn restore_meeting(&mut self, trashed: PathBuf, cx: &mut Context<Self>) -> anyhow::Result<PathBuf> {
        let dir = stenly_core::session::restore_session(&self.config.paths().audio_dir, &trashed)?;
        tracing::info!(dir = %dir.display(), "meeting restored from trash");
        // Trashing dropped it from the processing queue; pick it back up if
        // it never finished (what launch does via pending_sessions).
        if SessionMeta::load(&dir).is_ok_and(|m| m.state.is_pending()) {
            self.reprocess(dir.clone(), cx);
        }
        self.refresh_recent(cx);
        cx.notify();
        Ok(dir)
    }

    /// Trashed meetings, most recently trashed first.
    pub fn trash(&self) -> Vec<(PathBuf, SessionMeta)> {
        stenly_core::session::list_trash(&self.config.paths().audio_dir)
    }

    /// Permanently delete one trashed meeting.
    pub fn delete_trashed(&mut self, trashed: PathBuf, cx: &mut Context<Self>) -> anyhow::Result<()> {
        stenly_core::session::delete_trashed(&self.config.paths().audio_dir, &trashed)?;
        tracing::info!(dir = %trashed.display(), "trashed meeting deleted");
        cx.notify();
        Ok(())
    }

    /// Permanently delete everything in the Trash.
    pub fn empty_trash(&mut self, cx: &mut Context<Self>) -> anyhow::Result<usize> {
        let n = stenly_core::session::empty_trash(&self.config.paths().audio_dir)?;
        tracing::info!(n, "trash emptied");
        cx.notify();
        Ok(n)
    }

    pub fn reprocess(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if !self.processing_queue.contains(&dir) {
            self.processing_queue.push(dir);
        }
        self.pump_processing(cx);
    }

    /// Process queued sessions one at a time on a background thread.
    fn pump_processing(&mut self, cx: &mut Context<Self>) {
        if self.processing || self.processing_queue.is_empty() {
            return;
        }
        let dir = self.processing_queue.remove(0);
        // From memory: reading meta.json here can block on iCloud Drive.
        let title = self
            .recent
            .iter()
            .find(|(d, _)| *d == dir)
            .map(|(_, m)| m.title.clone())
            .or_else(|| dir.file_name().map(|n| n.to_string_lossy().splitn(3, ' ').nth(2).unwrap_or_default().to_string()))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Meeting".into());
        self.processing = true;
        self.processing_dir = Some(dir.clone());
        self.jobs = vec![Job { title: title.clone(), progress: "Loading model…".into() }];
        let cfg = self.config.clone();
        let (tx, rx) = crossbeam_channel::unbounded::<String>();
        tracing::info!(dir = %dir.display(), "processing started");
        let work = cx.background_spawn(async move {
            // Background on the meeting first (Settings → Context): it helps
            // spelling, speaker names and the summary. Never blocks the note.
            if cfg.context.enabled && cfg.llm.enabled && stenly_agent::needs_context(&dir) {
                let _ = tx.send("Gathering context…".into());
                match stenly_agent::gather_for_session(&cfg, &dir, &mut |e| {
                    if let Some(line) = describe_event(&e) {
                        let _ = tx.send(format!("Context: {line}"));
                    }
                }) {
                    Ok(c) => tracing::info!(people = c.people.len(), terms = c.terms.len(), "context gathered"),
                    Err(e) => tracing::warn!(error = %format!("{e:#}"), "context gathering failed"),
                }
            }
            let mut t = stenly_asr::ParakeetTranscriber::new(&cfg.asr, &cfg.paths().models_dir)?;
            tracing::info!("model loaded; transcribing");
            daemon::process_session(&cfg, &dir, &mut t, &mut |p| {
                let _ = tx.send(match p {
                    // Mic is transcribed first, then system audio: show one overall %.
                    Progress::Transcribing { channel, fraction } => {
                        let overall = match channel {
                            stenly_core::platform::Channel::Mic => fraction * 0.5,
                            stenly_core::platform::Channel::System => 0.5 + fraction * 0.5,
                        };
                        format!("Transcribing… {:.0}%", overall * 100.0)
                    }
                    Progress::SeparatingSpeakers => "Telling voices apart…".into(),
                    Progress::IdentifyingSpeakers => "Naming speakers…".into(),
                    Progress::Summarizing => "Writing summary…".into(),
                    Progress::Writing => "Writing note…".into(),
                });
            })
        });
        let task = cx.spawn(async move |this, cx: &mut AsyncApp| {
            let poll = async {
                loop {
                    cx.background_executor().timer(Duration::from_millis(250)).await;
                    let mut last = None;
                    while let Ok(m) = rx.try_recv() {
                        last = Some(m);
                    }
                    if let Some(m) = last {
                        let _ = this.update(cx, |c, cx| {
                            if let Some(j) = c.jobs.first_mut() {
                                j.progress = m;
                            }
                            cx.notify();
                        });
                    }
                }
            };
            let result = futures_race(work, poll).await;
            let _ = this.update(cx, |c, cx| {
                c.processing = false;
                // processing may have gathered context: re-read it for the meeting view
                let completed_dir = c.processing_dir.take();
                if let Some(d) = &completed_dir {
                    c.reload_meeting_context(d, cx);
                }
                c.jobs.clear();
                match result {
                    Ok(p) => {
                        // Automatic sends (Settings → Destinations) that failed: say so; Retry is on the meeting.
                        let failed: Vec<String> = p.sent.iter().filter_map(|(n, e)| e.as_ref().map(|e| format!("{n}: {e}"))).collect();
                        if !failed.is_empty() {
                            c.last_error = Some(format!("Couldn't send “{title}” to {}", failed.join("; ")));
                        }
                        if let Some(dir) = completed_dir {
                            cx.emit(ControllerEvent::NoteReady(dir));
                        }
                    }
                    Err(e) => c.last_error = Some(format!("Processing “{title}” failed: {e:#}")),
                }
                c.refresh_recent(cx);
                c.pump_processing(cx);
                cx.notify();
            });
        });
        task.detach();
        cx.notify();
    }
}

/// Run `main` to completion while also driving `side` (which never finishes).
async fn futures_race<T>(main: impl std::future::Future<Output = T>, side: impl std::future::Future<Output = ()>) -> T {
    use std::pin::pin;
    use std::task::Poll;
    let mut main = pin!(main);
    let mut side = pin!(side);
    std::future::poll_fn(move |cx| {
        if let Poll::Ready(v) = main.as_mut().poll(cx) {
            return Poll::Ready(v);
        }
        let _ = side.as_mut().poll(cx);
        Poll::Pending
    })
    .await
}

pub fn new(
    cx: &mut App,
    config: Config,
    audio: Arc<dyn AudioBackend>,
    mic: Arc<dyn MicActivity>,
    shell: std::rc::Rc<dyn DesktopShell>,
    calendar_source: Option<Arc<dyn CalendarSource>>,
) -> Entity<Controller> {
    cx.new(|cx| Controller::new(config, audio, mic, shell, calendar_source, cx))
}

// ---------------------------------------------------------------- setup / settings
impl Controller {
    /// Write one config key and apply it.
    pub fn set(&mut self, key: &str, value: impl Into<toml_edit::Value>, cx: &mut Context<Self>) {
        match Config::set_value(key, value.into()) {
            Ok(cfg) => self.reload_config(cfg, cx),
            Err(e) => self.last_error = Some(format!("Couldn't save settings: {e:#}")),
        }
        cx.notify();
    }

    pub fn set_path(&mut self, key: &str, path: &std::path::Path, cx: &mut Context<Self>) {
        match Config::set_path(key, path) {
            Ok(cfg) => self.reload_config(cfg, cx),
            Err(e) => self.last_error = Some(format!("Couldn't save settings: {e:#}")),
        }
        cx.notify();
    }

    /// Append to a list setting, skipping entries already there (any case).
    pub fn add_to_list(&mut self, key: &str, new: &[String], cx: &mut Context<Self>) {
        let mut items: Vec<String> = match key {
            "me.emails" => self.config.me.emails.clone(),
            "me.aliases" => self.config.me.aliases.clone(),
            "asr.vocabulary" => self.config.asr.vocabulary.clone(),
            _ => return,
        };
        for n in new {
            let n = n.trim();
            if !n.is_empty() && !items.iter().any(|x| x.eq_ignore_ascii_case(n)) {
                items.push(n.to_string());
            }
        }
        self.set_list(key, &items, cx);
    }

    /// Addresses the calendars Stenly reads are signed in with, not yet in Settings → You.
    pub fn suggested_me_emails(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let known = |e: &str| self.config.me.emails.iter().any(|x| x.eq_ignore_ascii_case(e));
        let accounts = self.calendars.iter().filter_map(|c| c.account.clone().or_else(|| c.primary.then(|| c.id.clone())));
        let owners = self.events.iter().filter_map(|e| e.calendar.clone());
        for e in accounts.chain(owners) {
            let e = e.trim().to_ascii_lowercase();
            if e.contains('@') && !e.ends_with("calendar.google.com") && !known(&e) && !out.contains(&e) {
                out.push(e);
            }
        }
        out
    }

    pub fn set_list(&mut self, key: &str, items: &[String], cx: &mut Context<Self>) {
        let arr: toml_edit::Array = items.iter().map(String::as_str).collect();
        self.set(key, toml_edit::Value::Array(arr), cx);
    }

    // ---- permissions
    pub fn refresh_permissions(&mut self) {
        self.permissions = [Permission::Microphone, Permission::SystemAudio, Permission::Accessibility]
            .into_iter()
            .map(|p| (p, self.shell.permission(p)))
            .collect();
    }

    pub fn permission(&self, p: Permission) -> PermissionState {
        if p == Permission::Calendars {
            return self.calendar_source.as_ref().map(|source| source.permission()).unwrap_or(PermissionState::NotApplicable);
        }
        self.permissions.iter().find(|(x, _)| *x == p).map(|(_, s)| *s).unwrap_or(PermissionState::NotDetermined)
    }

    pub fn request_permission(&mut self, p: Permission, cx: &mut Context<Self>) {
        let before = self.permission(p);
        tracing::info!(permission = ?p, ?before, "requesting permission");
        if !self.pending_permissions.contains(&p) {
            self.pending_permissions.push(p);
        }
        cx.notify();
        if p == Permission::Calendars {
            if let Some(source) = &self.calendar_source { source.request_permission(); }
        } else {
            self.shell.request_permission(p);
        }
        // The OS prompt is async; poll a few times so the UI flips to "Granted".
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            // Poll for up to 5 minutes: the user may take a while with the prompt
            // or System Settings. Window activation also refreshes (see views).
            // Accessibility opens System Settings with no prompt to wait on; stop
            // showing "Waiting…" after a while so the user can click again.
            let max_iters = if p == Permission::Accessibility { 75 } else { 300 };
            for _ in 0..max_iters {
                cx.background_executor().timer(Duration::from_millis(400)).await;
                let done = this
                    .update(cx, |c, cx| {
                        c.refresh_permissions();
                        cx.notify();
                        let now = c.permission(p);
                        if now != before {
                            tracing::info!(permission = ?p, ?now, "permission changed");
                            // The system prompt took focus; come back to the front.
                            cx.activate(true);
                        }
                        if p == Permission::Calendars && now == PermissionState::Granted && now != before {
                            c.load_calendars(cx);
                            c.refresh_calendar(cx);
                        }
                        matches!(now, PermissionState::Granted | PermissionState::Denied) && now != before
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
            let _ = this.update(cx, |c, cx| {
                c.pending_permissions.retain(|x| *x != p);
                cx.notify();
            });
        })
        .detach();
    }

    // ---- speech models
    pub fn asr_installed(&self, id: &str) -> bool {
        stenly_asr::models::find(id).is_some_and(|m| stenly_asr::models::is_installed(m, &self.config.paths().models_dir))
    }

    pub fn download(&self, id: &str) -> Option<&Download> {
        self.downloads.iter().find(|d| d.id == id)
    }

    fn set_progress(&mut self, id: &str, fraction: f32, status: &str) {
        if let Some(d) = self.downloads.iter_mut().find(|d| d.id == id) {
            if fraction >= 0.0 {
                d.fraction = fraction;
            }
            d.status = status.to_string();
        }
    }

    pub fn download_asr(&mut self, id: String, cx: &mut Context<Self>) {
        if self.download(&id).is_some() {
            return;
        }
        let Some(spec) = stenly_asr::models::find(&id) else { return };
        let dir = self.config.paths().models_dir;
        self.downloads.push(Download { id: id.clone(), fraction: 0.0, status: "Starting…".into() });
        let (tx, rx) = crossbeam_channel::unbounded::<f32>();
        let work = cx.background_spawn(async move {
            stenly_asr::models::ensure_vad(&dir)?;
            stenly_asr::models::ensure(spec, &dir, &mut |f| {
                let _ = tx.send(f);
            })
            .map(|_| ())
        });
        self.track(id, work, move || {
            let mut last = None;
            while let Ok(f) = rx.try_recv() {
                last = Some(f);
            }
            last.map(|f| (f, "Downloading".to_string()))
        }, cx);
    }

    pub fn delete_asr(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(spec) = stenly_asr::models::find(id) {
            let _ = stenly_asr::models::remove(spec, &self.config.paths().models_dir);
        }
        cx.notify();
    }

    /// Drive a background download, polling `progress` for UI updates.
    fn track(
        &mut self,
        id: String,
        work: Task<anyhow::Result<()>>,
        mut progress: impl FnMut() -> Option<(f32, String)> + 'static,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let poll = async {
                loop {
                    cx.background_executor().timer(Duration::from_millis(200)).await;
                    if let Some((f, s)) = progress() {
                        let id = id.clone();
                        let _ = this.update(cx, |c, cx| {
                            c.set_progress(&id, f, &s);
                            cx.notify();
                        });
                    }
                }
            };
            let result = futures_race(work, poll).await;
            let _ = this.update(cx, |c, cx| {
                c.downloads.retain(|d| d.id != id);
                if let Err(e) = result {
                    c.last_error = Some(format!("Download of {id} failed: {e:#}"));
                }
                c.refresh_ollama(cx);
                cx.notify();
            });
        })
        .detach();
    }

    // ---- summary models (Ollama)
    pub fn refresh_ollama(&mut self, cx: &mut Context<Self>) {
        let endpoint = self.config.llm.endpoint.clone();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (running, installed) = cx
                .background_spawn(async move {
                    let running = stenly_core::ollama::is_running(&endpoint);
                    let installed = if running {
                        stenly_core::ollama::installed(&endpoint).map(|v| v.into_iter().map(|m| m.name).collect()).unwrap_or_default()
                    } else {
                        vec![]
                    };
                    (running, installed)
                })
                .await;
            let _ = this.update(cx, |c, cx| {
                c.ollama = OllamaState { running, installed, checked: true };
                cx.notify();
            });
        })
        .detach();
    }

    pub fn llm_installed(&self, id: &str) -> bool {
        self.ollama.installed.iter().any(|m| stenly_core::ollama::same_model(m, id))
    }

    pub fn pull_llm(&mut self, id: String, cx: &mut Context<Self>) {
        if self.download(&id).is_some() {
            return;
        }
        let endpoint = self.config.llm.endpoint.clone();
        self.downloads.push(Download { id: id.clone(), fraction: 0.0, status: "Starting…".into() });
        let (tx, rx) = crossbeam_channel::unbounded::<(f32, String)>();
        let model = id.clone();
        let work = cx.background_spawn(async move {
            stenly_core::ollama::pull(&endpoint, &model, &mut |f, s| {
                let _ = tx.send((f, s.to_string()));
            })
        });
        self.track(id, work, move || {
            let mut last = None;
            while let Ok(v) = rx.try_recv() {
                last = Some(v);
            }
            last.map(|(f, s)| {
                let label = if s.starts_with("pulling") { "Downloading".to_string() } else { capitalize(&s) };
                (f, label)
            })
        }, cx);
    }

    pub fn delete_llm(&mut self, id: String, cx: &mut Context<Self>) {
        let endpoint = self.config.llm.endpoint.clone();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let r = cx.background_spawn(async move { stenly_core::ollama::delete(&endpoint, &id) }).await;
            let _ = this.update(cx, |c, cx| {
                if let Err(e) = r {
                    c.last_error = Some(format!("Couldn't remove model: {e:#}"));
                }
                c.refresh_ollama(cx);
            });
        })
        .detach();
    }

    // ---- dictation settings
    /// Word replacements live in an array of tables: rewrite the whole list.
    pub fn set_replacements(&mut self, reps: &[stenly_core::config::Replacement], cx: &mut Context<Self>) {
        let mut arr = toml_edit::Array::new();
        for r in reps {
            let mut t = toml_edit::InlineTable::new();
            t.insert("from", r.from.as_str().into());
            t.insert("to", r.to.as_str().into());
            arr.push(toml_edit::Value::InlineTable(t));
        }
        self.set("dictation.replacements", toml_edit::Value::Array(arr), cx);
    }

    pub fn add_replacement(&mut self, from: &str, to: &str, cx: &mut Context<Self>) {
        let mut reps = self.config.dictation.replacements.clone();
        // same target: merge the heard-as variants ("view on" + "vuong" → "Vuon")
        if let Some(r) = reps.iter_mut().find(|r| r.to == to) {
            let mut v: Vec<String> = r.from.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
            for f in from.split(',').map(str::trim).filter(|x| !x.is_empty()) {
                if !v.iter().any(|x| x.eq_ignore_ascii_case(f)) {
                    v.push(f.to_string());
                }
            }
            r.from = v.join(", ");
        } else {
            reps.push(stenly_core::config::Replacement { from: from.to_string(), to: to.to_string() });
        }
        self.set_replacements(&reps, cx);
    }

    pub fn remove_replacement(&mut self, i: usize, cx: &mut Context<Self>) {
        let mut reps = self.config.dictation.replacements.clone();
        if i < reps.len() {
            reps.remove(i);
            self.set_replacements(&reps, cx);
        }
    }

    /// Bring over VoiceInk's vocabulary (→ jargon), replacements, mic priority
    /// and dictation history. Merges; never removes anything.
    pub fn import_voiceink(&mut self, cx: &mut Context<Self>) {
        let limit = self.config.dictation.history_limit.max(1000);
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let r = cx.background_spawn(async move { stenly_core::dictation::import_voiceink(true, limit) }).await;
            let _ = this.update(cx, |c, cx| match r {
                Ok(imp) => {
                    let mut vocab = c.config.asr.vocabulary.clone();
                    for w in &imp.vocabulary {
                        if !vocab.iter().any(|v| v.eq_ignore_ascii_case(w)) {
                            vocab.push(w.clone());
                        }
                    }
                    c.set_list("asr.vocabulary", &vocab, cx);
                    for rep in &imp.replacements {
                        c.add_replacement(&rep.from, &rep.to, cx);
                    }
                    if c.config.audio.mic_priority.is_empty() && !imp.mic_priority.is_empty() {
                        c.set_mic_priority(&imp.mic_priority, cx);
                    }
                    if limit > c.config.dictation.history_limit {
                        c.set("dictation.history_limit", limit as i64, cx);
                    }
                    c.import_note = Some(format!(
                        "Imported {} words, {} replacements{} and {} dictations from VoiceInk.",
                        imp.vocabulary.len(),
                        imp.replacements.len(),
                        if imp.mic_priority.is_empty() { String::new() } else { ", your microphone order".into() },
                        imp.dictations
                    ));
                    cx.notify();
                }
                Err(e) => {
                    c.import_note = Some(format!("Couldn't import: {e:#}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Microphone priority (shared by meetings + dictation).
    pub fn set_mic_priority(&mut self, ids: &[String], cx: &mut Context<Self>) {
        self.set_list("audio.mic_priority", ids, cx);
    }

    pub fn input_devices(&self) -> Vec<stenly_core::platform::InputDevice> {
        self.audio.input_devices()
    }

    // ---- hosted summary API
    /// Store (or with "" remove) the API key in the Keychain.
    pub fn set_api_key(&mut self, key: &str, cx: &mut Context<Self>) {
        match stenly_core::secrets::set_llm_api_key(key) {
            Ok(()) => self.api_key_saved = !key.trim().is_empty(),
            Err(e) => self.last_error = Some(format!("{e:#}")),
        }
        self.api_test = ApiTest::Idle;
        cx.notify();
    }

    /// Send a tiny request to the configured API and report what happened.
    pub fn test_api(&mut self, cx: &mut Context<Self>) {
        let mut llm = self.config.llm.clone();
        llm.provider = "api".into();
        let target = llm.target(stenly_core::secrets::llm_api_key);
        self.api_test = ApiTest::Running;
        cx.notify();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let r = cx.background_spawn(async move { stenly_core::summary::ping(&target) }).await;
            let _ = this.update(cx, |c, cx| {
                c.api_test = match r {
                    Ok(ms) => ApiTest::Ok(format!("Connected · answered in {:.1} s", ms as f32 / 1000.0)),
                    Err(e) => ApiTest::Failed(format!("{e:#}")),
                };
                cx.notify();
            });
        })
        .detach();
    }

    // ---- Codex / Claude Code as the summary model
    /// Look for `codex` and `claude` (off the UI thread: it asks each for its version).
    pub fn refresh_cli_tools(&mut self, cx: &mut Context<Self>) {
        let cfg = self.config.llm.cli.clone();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let found = cx.background_spawn(async move { stenly_core::cli_llm::detect(&cfg) }).await;
            let _ = this.update(cx, |c, cx| {
                c.cli_tools = found;
                c.cli_checked = true;
                cx.notify();
            });
        })
        .detach();
    }

    pub fn cli_tool(&self, tool: stenly_core::cli_llm::Tool) -> Option<&stenly_core::cli_llm::Detected> {
        self.cli_tools.iter().find(|d| d.tool == tool)
    }

    /// Write summaries with this tool.
    pub fn use_cli_tool(&mut self, tool: stenly_core::cli_llm::Tool, cx: &mut Context<Self>) {
        if self.config.llm.cli.tool != tool.id() {
            self.set("llm.cli.tool", tool.id(), cx);
            // a model name for one tool means nothing to the other
            self.set("llm.cli.model", "", cx);
        }
        self.set("llm.provider", "cli", cx);
        self.cli_test = ApiTest::Idle;
        cx.notify();
    }

    /// Ask the chosen tool a tiny question: installed, signed in, answering.
    pub fn test_cli(&mut self, cx: &mut Context<Self>) {
        let mut llm = self.config.llm.clone();
        llm.provider = "cli".into();
        let target = llm.target(|| None);
        self.cli_test = ApiTest::Running;
        cx.notify();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let r = cx.background_spawn(async move { stenly_core::summary::ping(&target) }).await;
            let _ = this.update(cx, |c, cx| {
                c.cli_test = match r {
                    Ok(ms) => ApiTest::Ok(format!("Signed in · answered in {:.1} s", ms as f32 / 1000.0)),
                    Err(e) => ApiTest::Failed(format!("{e:#}")),
                };
                cx.notify();
            });
        })
        .detach();
    }

    // ---- Sign in with ChatGPT
    /// Open the browser to sign in; on success, summaries use the ChatGPT plan.
    pub fn sign_in_chatgpt(&mut self, cx: &mut Context<Self>) {
        if self.chatgpt == ChatGptState::SigningIn {
            return;
        }
        self.chatgpt = ChatGptState::SigningIn;
        self.api_test = ApiTest::Idle;
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.chatgpt_cancel = cancel.clone();
        cx.notify();
        let (url_tx, url_rx) = crossbeam_channel::bounded::<String>(1);
        let work = cx.background_spawn(async move {
            stenly_core::chatgpt::sign_in(&|u| {
                let _ = url_tx.send(u.to_string());
            }, &|| cancel.load(std::sync::atomic::Ordering::Relaxed))
        });
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let opener = async {
                loop {
                    cx.background_executor().timer(Duration::from_millis(100)).await;
                    if let Ok(u) = url_rx.try_recv() {
                        let _ = cx.update(|cx| cx.open_url(&u));
                    }
                }
            };
            let result = futures_race(work, opener).await;
            let _ = this.update(cx, |c, cx| {
                match result {
                    Ok(account) => {
                        c.chatgpt = ChatGptState::SignedIn(account.label());
                        c.set("llm.provider", "chatgpt", cx);
                        cx.activate(true);
                    }
                    Err(e) if format!("{e}").contains("cancelled") => c.chatgpt = ChatGptState::SignedOut,
                    Err(e) => c.chatgpt = ChatGptState::Failed(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn cancel_chatgpt_sign_in(&mut self, cx: &mut Context<Self>) {
        self.chatgpt_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        self.chatgpt = ChatGptState::SignedOut;
        cx.notify();
    }

    /// Forget the sign-in. Summaries stay on "chatgpt" (and say to sign in)
    /// until the user picks another provider.
    pub fn sign_out_chatgpt(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = stenly_core::chatgpt::sign_out() {
            self.last_error = Some(format!("{e:#}"));
        }
        self.chatgpt = ChatGptState::SignedOut;
        self.api_test = ApiTest::Idle;
        cx.notify();
    }

    /// One tiny request on the ChatGPT plan.
    pub fn test_chatgpt(&mut self, cx: &mut Context<Self>) {
        let mut llm = self.config.llm.clone();
        llm.provider = "chatgpt".into();
        let target = llm.target(|| None);
        self.api_test = ApiTest::Running;
        cx.notify();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let r = cx.background_spawn(async move { stenly_core::summary::ping(&target) }).await;
            let _ = this.update(cx, |c, cx| {
                c.api_test = match r {
                    Ok(ms) => ApiTest::Ok(format!("Connected · answered in {:.1} s", ms as f32 / 1000.0)),
                    Err(e) => {
                        if stenly_core::chatgpt::signed_in().is_none() {
                            c.chatgpt = ChatGptState::SignedOut;
                        }
                        ApiTest::Failed(format!("{e:#}"))
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }

    // ---- Calendar sources
    pub fn uses_eventkit(&self) -> bool {
        self.config.calendar.source.uses_eventkit() && self.calendar_source.is_some()
    }

    pub fn supports_eventkit(&self) -> bool {
        self.calendar_source.is_some()
    }

    /// Change source without carrying calendar identifiers across providers.
    /// EventKit identifiers and Google ids are unrelated, so stale selection
    /// would otherwise silently produce an empty event list after a switch.
    pub fn set_calendar_source(&mut self, source: &str, cx: &mut Context<Self>) {
        self.calendars.clear();
        self.calendars_status = None;
        self.set_list("calendar.calendar_ids", &[], cx);
        self.set("calendar.source", source, cx);
        self.load_calendars(cx);
        self.refresh_calendar(cx);
    }

    pub fn request_calendar_access(&mut self, cx: &mut Context<Self>) {
        self.request_permission(Permission::Calendars, cx);
    }

    // ---- Kody / Google Calendar
    pub fn connect_kody(&mut self, url: String, cx: &mut Context<Self>) {
        self.kody = KodyState::Connecting;
        cx.notify();
        let (url_tx, url_rx) = crossbeam_channel::bounded::<String>(1);
        let base = url.clone();
        let work = cx.background_spawn(async move {
            stenly_core::kody::sign_in(&base, &|u| {
                let _ = url_tx.send(u.to_string());
            })
        });
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            // Open the browser as soon as the authorize URL is ready.
            let opener = async {
                loop {
                    cx.background_executor().timer(Duration::from_millis(100)).await;
                    if let Ok(u) = url_rx.try_recv() {
                        let _ = cx.update(|cx| cx.open_url(&u));
                    }
                }
            };
            let result = futures_race(work, opener).await;
            let _ = this.update(cx, |c, cx| {
                match result {
                    Ok(auth) => {
                        c.kody = KodyState::Connected;
                        c.set("calendar.kody_url", auth.base_url.as_str(), cx);
                        c.set_calendar_source("kody", cx);
                        cx.activate(true);
                    }
                    Err(e) => c.kody = KodyState::Failed(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn disconnect_kody(&mut self, cx: &mut Context<Self>) {
        stenly_core::kody::KodyAuth::delete();
        self.kody = KodyState::Disconnected;
        self.calendars.clear();
        self.set("calendar.kody_url", "", cx);
        if self.calendar_source.is_some() {
            self.set_calendar_source("eventkit", cx);
        }
    }

    pub fn load_calendars(&mut self, cx: &mut Context<Self>) {
        // Single flight: Refresh / Retry / window focus / the 30 s retry can all
        // call this; never stack up parallel requests against a slow server.
        if self.calendars_loading {
            return;
        }
        self.calendars_loading = true;
        let integration = self.config.calendar.kody_integration.clone();
        let eventkit = self.uses_eventkit().then(|| self.calendar_source.clone()).flatten();
        tracing::info!(source = if eventkit.is_some() { "EventKit" } else { "Kody" }, "loading calendars");
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let r = cx
                .background_spawn(async move {
                    if let Some(source) = eventkit {
                        source.calendars()
                    } else {
                        let mut client = stenly_core::kody::KodyClient::from_saved().ok_or_else(|| anyhow::anyhow!("not signed in"))?;
                        stenly_core::kody::list_calendars(&mut client, &integration)
                    }
                })
                .await;
            let _ = this.update(cx, |c, cx| {
                c.calendars_loading = false;
                match r {
                    Ok(cals) => {
                        tracing::info!(count = cals.len(), "calendars loaded");
                        c.calendars_status = Some(Ok(()));
                        // First connection: watch the primary calendar.
                        if c.config.calendar.calendar_ids.is_empty() {
                            // EventKit has no portable primary-calendar marker. Persist
                            // its initial all-calendars selection, so UI toggles have a
                            // precise list to add/remove from.
                            let selected: Vec<String> = if c.uses_eventkit() {
                                cals.iter().map(|x| x.id.clone()).collect()
                            } else {
                                cals.iter().filter(|x| x.primary).map(|x| x.id.clone()).collect()
                            };
                            if !selected.is_empty() {
                                c.set_list("calendar.calendar_ids", &selected, cx);
                            }
                        }
                        c.calendars = cals;
                    }
                    Err(e) => {
                        tracing::warn!(error = %format!("{e:#}"), "loading calendars failed; retrying in 30s");
                        // Keep the session (the server may just be down); show the error
                        // and retry quietly in the background.
                        c.calendars_status = Some(Err(format!("{e:#}")));
                        // One pending retry at most (replacing drops the old one).
                        c.calendars_retry = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
                            cx.background_executor().timer(Duration::from_secs(30)).await;
                            let _ = this.update(cx, |c, cx| {
                                if c.calendars.is_empty() && matches!(c.calendars_status, Some(Err(_))) && (c.uses_eventkit() || matches!(c.kody, KodyState::Connected)) {
                                    c.load_calendars(cx);
                                }
                            });
                        }));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn toggle_calendar(&mut self, id: &str, cx: &mut Context<Self>) {
        let mut ids = self.config.calendar.calendar_ids.clone();
        if let Some(i) = ids.iter().position(|x| x == id) {
            // Empty means implicit-all during first EventKit setup. Once a
            // selection exists, retain one item rather than making a last
            // toggle invert into every calendar.
            if self.uses_eventkit() && ids.len() == 1 {
                return;
            }
            ids.remove(i);
        } else {
            ids.push(id.to_string());
        }
        self.set_list("calendar.calendar_ids", &ids, cx);
    }

    pub fn set_launch_at_login(&mut self, on: bool, cx: &mut Context<Self>) {
        match self.shell.set_launch_at_login(on) {
            Ok(()) => self.set("app.launch_at_login", on, cx),
            Err(e) => {
                self.last_error = Some(format!("{e:#}"));
                cx.notify();
            }
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}
