use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use stenly_core::config::{self, Config};
use stenly_core::daemon::{Progress, process_session};
use stenly_core::platform::Channel;
use stenly_core::session::{Recording, list_sessions, pending_sessions};

#[derive(Parser)]
#[command(name = "stenly-cli", about = "Local meeting recorder → Obsidian notes")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Record mic + system audio until Ctrl-C (or --secs), then transcribe and write the note.
    Record {
        #[arg(short, long, default_value = "Meeting")]
        title: String,
        #[arg(long)]
        secs: Option<u64>,
        /// Only record; don't transcribe.
        #[arg(long)]
        no_process: bool,
        /// Transcribe while recording (as the app does).
        #[arg(long)]
        live: bool,
    },
    /// Transcribe a WAV file and print the transcript.
    Transcribe { wav: PathBuf },
    /// Show (and with --apply, import) VoiceInk's vocabulary, replacements, mic priority and history.
    ImportVoiceink {
        #[arg(long)]
        apply: bool,
    },
    /// Run a WAV through the dictation pipeline (clip ASR + clean-up [+ AI]) and print the result.
    Dictate {
        wav: PathBuf,
        /// Also run AI clean-up with the summary model.
        #[arg(long)]
        ai: bool,
    },
    /// Compare dictation strategies on WAVs: one pass after stop vs streaming
    /// (overlap / pause). Prints the wait after stop and writes each text to --out.
    DictateBench {
        wavs: Vec<PathBuf>,
        #[arg(long, default_value = "/tmp/stenly-bench")]
        out: PathBuf,
        /// Override overlap params, e.g. "left_context=6,agree=0"
        #[arg(long)]
        overlap: Option<String>,
        /// Override pause params, e.g. "min_gap=0.5"
        #[arg(long)]
        pause: Option<String>,
    },
    /// (Re)process a session directory, or all pending sessions.
    Process {
        dir: Option<PathBuf>,
        /// Write the note with this template (id or name); saved for the session.
        #[arg(long)]
        template: Option<String>,
    },
    /// List note templates; with a session, show which one it would use.
    Templates { session: Option<String> },
    /// List recipes (saved prompts you can run on a meeting).
    Recipes,
    /// Run a recipe on a meeting and print the result.
    Recipe {
        /// Recipe id or name, e.g. follow-up-email or "TL;DR for Slack".
        name: String,
        /// Session directory, its name, a unique title fragment, or "latest".
        #[arg(default_value = "latest")]
        session: String,
        /// Keep the result with the meeting (shown in the app).
        #[arg(long)]
        save: bool,
        /// Also add it to the meeting's note under this heading (default: the recipe's).
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        into_note: Option<String>,
    },
    /// Show upcoming calendar meetings that would trigger a prompt.
    Upcoming {
        #[arg(long, default_value_t = 24)]
        hours: i64,
        /// Include events that would be filtered out.
        #[arg(long)]
        all: bool,
    },
    /// Sign in to Kody (Google Calendar routing) in your browser.
    KodyLogin {
        #[arg(default_value = "")]
        url: String,
    },
    /// List Google calendars available through Kody.
    KodyCalendars,
    /// Sign in with ChatGPT in your browser; summaries then use your ChatGPT plan.
    ChatgptLogin,
    /// Forget the ChatGPT sign-in.
    ChatgptLogout,
    /// Show the ChatGPT sign-in and send one tiny test request.
    ChatgptStatus,
    /// Which apps are currently using the microphone (and which display their window is on).
    MicUsers,
    /// Debug: which display an app's main window is on (bundle id or pid:N).
    #[command(hide = true)]
    WindowDisplay { app_id: String },
    /// List recent sessions.
    Sessions,
    /// Print config path + resolved paths.
    Config,
    /// Download / list ASR models.
    Models {
        #[arg(long)]
        download: bool,
    },
    /// Download a summary model through Ollama (e.g. qwen3:4b).
    Pull { model: String },
    /// Send a meeting's note to a destination (see `destinations`).
    /// SESSION is a session folder, "last", or part of a meeting title.
    Send {
        session: String,
        /// Destination name or id.
        #[arg(long)]
        to: String,
    },
    /// Create/update an end-to-end encrypted share link for a meeting.
    Share {
        /// Session folder, "latest", or a unique title fragment.
        #[arg(default_value = "latest")]
        session: String,
        /// Revoke the existing link instead of creating/updating it.
        #[arg(long)]
        revoke: bool,
        /// A configured Stenly share-link destination (id or name).
        #[arg(long)]
        to: Option<String>,
    },
    /// List destinations, or test one with --test <name>.
    Destinations {
        #[arg(long)]
        test: Option<String>,
    },
    /// Speaker names: inspect meeting apps, (re)name a session's speakers.
    #[command(subcommand)]
    Speakers(SpeakersCmd),
    /// Free trial and licence for the official signed app (source builds never need one).
    #[command(subcommand)]
    License(LicenseCmd),
    /// Meeting context: background on a meeting from past meetings, the vault, Notion and MCP servers.
    #[command(subcommand)]
    Context(ContextCmd),
}

#[derive(Subcommand)]
enum LicenseCmd {
    /// Show the trial or licence state.
    Status {
        /// Re-check the licence with the server now.
        #[arg(long)]
        check: bool,
    },
    /// Activate a licence key on this Mac (uses one activation).
    Activate {
        /// The key from your purchase email. Omit to read it from STENLY_LICENSE_KEY or stdin.
        key: Option<String>,
    },
    /// Release this Mac's activation so the key can be used on another Mac.
    Deactivate,
}

#[derive(Subcommand)]
enum ContextCmd {
    /// Gather context for a session (dir, or "latest") and save context.json.
    Gather {
        session: String,
        /// Then re-summarize the meeting with it.
        #[arg(long)]
        process: bool,
    },
    /// Gather context for the next calendar meeting (prints it; writes nothing).
    Next,
    /// Show a session's context and the summary prompt block made from it.
    Show { session: String },
    /// List context sources and the tools each offers.
    Sources,
    /// Sign in to an MCP server listed in [[context.servers]] (OAuth in the browser).
    Login { id: String },
    /// Forget an MCP server's sign-in / key.
    Logout { id: String },
}

#[derive(Subcommand)]
enum SpeakersCmd {
    /// Print who the open meeting apps show as talking (one line per change).
    Watch {
        #[arg(long, default_value_t = 30)]
        secs: u64,
        /// Also append to <dir>/speakers.jsonl, as during a recording.
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Save one accessibility snapshot of open meeting windows (test fixture).
    Dump { out: PathBuf },
    /// The meeting-app drivers and what each can read.
    Drivers,
    /// Who Stenly thinks you are, and who was in a session's meeting.
    Who { dir: Option<PathBuf> },
    /// Parse a saved snapshot and print what it shows.
    Parse { file: PathBuf },
    /// Name the speakers of a processed session (rewrites transcript + note; no new summary).
    Name {
        dir: PathBuf,
        /// Forget earlier names and splits first.
        #[arg(long)]
        reset: bool,
        /// Leave the note file alone (only transcript.json and voiceprints change).
        #[arg(long)]
        no_note: bool,
    },
    /// Rename one voice in a session: `rename <dir> them 2 "Alex"` (speaker numbers as shown).
    Rename { dir: PathBuf, channel: String, speaker: Option<u32>, name: String },
    /// List remembered voices.
    Voices,
    /// Forget a remembered voice, or (with --session) what one meeting taught.
    Forget {
        name: Option<String>,
        #[arg(long)]
        session: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let cfg = Config::load_or_init()?;
    let paths = cfg.paths();
    let platform = stenly_platform::detect();

    match cli.cmd {
        Cmd::Record { title, secs, no_process, live } => {
            let lic = stenly_core::license::LicenseManager::system();
            if let Some(msg) = lic.snapshot(chrono::Utc::now()).blocked_message() {
                anyhow::bail!("{msg}");
            }
            let (ltx, lrx) = crossbeam_channel::unbounded();
            let rec = Recording::start_with_live(platform.audio.as_ref(), &paths.audio_dir, &title, None, live.then_some(ltx))?;
            let live = if live { Some(stenly_asr::LiveSession::start_from(&cfg.asr, &paths.models_dir, &rec.dir, lrx)?) } else { None };
            eprintln!("recording to {}", rec.dir.display());
            for r in &rec.meta.routes {
                eprintln!("  {r}");
            }
            if let Some(e) = &rec.meta.error {
                eprintln!("  warning: {e}");
            }
            let stop = Arc::new(AtomicBool::new(false));
            let s = stop.clone();
            ctrlc::set_handler(move || s.store(true, Ordering::Relaxed))?;
            let started = std::time::Instant::now();
            while !stop.load(Ordering::Relaxed) && secs.is_none_or(|s| started.elapsed().as_secs() < s) {
                std::thread::sleep(std::time::Duration::from_millis(500));
                eprint!(
                    "\r  {:>5.0}s  mic {:>6.1} dB   system {:>6.1} dB ",
                    started.elapsed().as_secs_f32(),
                    rec.levels.get(Channel::Mic),
                    rec.levels.get(Channel::System)
                );
            }
            eprintln!();
            let (dir, meta) = rec.stop()?;
            let id = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            lic.record_meeting(&id, meta.ended.map(|e| (e - meta.started).num_seconds()).unwrap_or(0));
            if let Some(l) = live {
                let t0 = std::time::Instant::now();
                let segs = l.finish()?;
                eprintln!("live transcript: {} segments, finished {:.1}s after stop", segs.len(), t0.elapsed().as_secs_f32());
                stenly_asr::mark_live_complete(&dir, &segs)?;
            }
            if !no_process {
                run_process(&cfg, &dir)?;
            }
        }
        Cmd::Transcribe { wav } => {
            let mut t = stenly_asr::ParakeetTranscriber::new(&cfg.asr, &paths.models_dir)?;
            let samples = stenly_core::audio::read_wav_16k_mono(&wav)?;
            let t0 = std::time::Instant::now();
            let segs = stenly_core::transcript::Transcriber::transcribe(&mut t, &samples, Channel::Mic, &mut |_| {})?;
            let audio_secs = samples.len() as f64 / 16_000.0;
            for s in &segs {
                println!("[{} - {}] {}", stenly_core::util::fmt_timestamp(s.start), stenly_core::util::fmt_timestamp(s.end), s.text);
            }
            eprintln!("{audio_secs:.1}s audio in {:.2}s ({:.0}x realtime)", t0.elapsed().as_secs_f64(), audio_secs / t0.elapsed().as_secs_f64());
        }
        Cmd::ImportVoiceink { apply } => {
            let r = stenly_core::dictation::import_voiceink(apply, cfg.dictation.history_limit.max(1000))?;
            println!("vocabulary:   {}", r.vocabulary.join(", "));
            println!("replacements: {}", r.replacements.iter().map(|x| format!("{} → {}", x.from, x.to)).collect::<Vec<_>>().join("; "));
            println!("mic priority: {}", r.mic_priority.join(" > "));
            println!("dictations:   {}", r.dictations);
            if !apply {
                println!("(dry run; pass --apply to import)");
            }
        }
        Cmd::DictateBench { wavs, out, overlap, pause } => {
            use stenly_asr::stream::StreamParams;
            let parse = |s: Option<String>, mut p: StreamParams| -> Result<StreamParams> {
                for kv in s.iter().flat_map(|s| s.split(',')).filter(|s| !s.is_empty()) {
                    let (k, v) = kv.split_once('=').context("expected key=value")?;
                    let v: f32 = v.trim().parse()?;
                    match k.trim() {
                        "left_context" => p.left_context = v,
                        "right_margin" => p.right_margin = v,
                        "search" => p.search = v,
                        "min_gap" => p.min_gap = v,
                        "stride" => p.stride = v,
                        "max_pending" => p.max_pending = v,
                        "min_pending" => p.min_pending = v,
                        "agree" => p.agree = v != 0.0,
                        k => anyhow::bail!("unknown param {k}"),
                    }
                }
                Ok(p)
            };
            let modes = [("overlap", parse(overlap, StreamParams::OVERLAP)?), ("pause", parse(pause, StreamParams::PAUSE)?)];
            std::fs::create_dir_all(&out)?;
            let asr = stenly_asr::ParakeetTranscriber::new(&cfg.asr, &paths.models_dir)?;
            asr.warm_up();
            println!("{:<28} {:>6} {:>9} | {:>22} | {:>22}", "file", "audio", "batch", "overlap wait (tail, max)", "pause wait (tail, max)");
            for wav in &wavs {
                let samples = stenly_core::audio::read_wav_16k_mono(wav)?;
                let name = wav.file_stem().unwrap_or_default().to_string_lossy().to_string();
                // the one-pass baseline is slow; reuse it from an earlier run
                let bpath = out.join(format!("{name}.batch.txt"));
                let batch_t = if bpath.exists() {
                    f32::NAN
                } else {
                    let t0 = std::time::Instant::now();
                    let batch = asr.transcribe_clip(&samples);
                    asr.check_health()?;
                    std::fs::write(&bpath, &batch)?;
                    t0.elapsed().as_secs_f32()
                };
                let mut cols = Vec::new();
                for (m, p) in modes {
                    // Simulated real time: audio "arrives" at 1× while each decode
                    // runs, so a slow decode means a bigger backlog next time and,
                    // at the end, waiting for the decode in flight.
                    let mut s = asr.streamer(p);
                    let total = samples.len();
                    // the clock counts samples (integer) so it always advances
                    let (mut fed, mut clock) = (0usize, 0usize);
                    loop {
                        let avail = clock.min(total);
                        s.push(&samples[fed..avail]);
                        fed = avail;
                        if fed == total {
                            break;
                        }
                        if s.ready() {
                            let t0 = std::time::Instant::now();
                            s.step();
                            clock += (t0.elapsed().as_secs_f64() * 16_000.0) as usize + 1;
                        } else {
                            clock = clock.max(s.next_ready_at());
                        }
                    }
                    let in_flight = clock.saturating_sub(total) as f32 / 16_000.0;
                    let text = s.finish();
                    asr.check_health()?;
                    let st = s.stats;
                    std::fs::write(out.join(format!("{name}.{m}.txt")), &text)?;
                    cols.push(format!("{:>5.2}s ({:>4.1}s aud, {:.2}s)", in_flight + st.tail_secs, st.tail_audio, st.max_live_decode));
                }
                println!("{:<28} {:>5.0}s {:>8.2}s | {} | {}", &name[..name.len().min(28)], samples.len() as f32 / 16_000.0, batch_t, cols[0], cols[1]);
            }
        }
        Cmd::Dictate { wav, ai } => {
            let t0 = std::time::Instant::now();
            let asr = stenly_asr::ParakeetTranscriber::new(&cfg.asr, &paths.models_dir)?;
            asr.warm_up();
            let load = t0.elapsed();
            let samples = stenly_core::audio::read_wav_16k_mono(&wav)?;
            let t1 = std::time::Instant::now();
            let raw = asr.transcribe_clip(&samples);
            asr.check_health()?;
            let asr_t = t1.elapsed();
            let text = stenly_core::dictation::clean(&raw, &cfg.dictation, &cfg.asr.vocabulary);
            eprintln!("load {:.2}s · {:.1}s audio transcribed in {:.2}s", load.as_secs_f32(), samples.len() as f32 / 16_000.0, asr_t.as_secs_f32());
            eprintln!("raw:   {raw}");
            println!("{text}");
            if ai {
                let t2 = std::time::Instant::now();
                let mut c = cfg.clone();
                c.dictation.ai_cleanup = true;
                match stenly_core::dictation::ai_cleanup(&c, &text) {
                    Ok(t) => {
                        eprintln!("ai ({:.1}s):", t2.elapsed().as_secs_f32());
                        println!("{t}");
                    }
                    Err(e) => eprintln!("ai failed: {e:#}"),
                }
            }
        }
        Cmd::Process { dir, template } => {
            if let Some(t) = template {
                let d = dir.as_ref().context("--template needs a session directory")?;
                let e = stenly_core::library::Library::templates().find(&t)?;
                stenly_core::recipe::set_chosen_template(d, Some(&e.id))?;
                // re-summarize from the existing transcript
                let mut m = stenly_core::session::SessionMeta::load(d)?;
                if m.state == stenly_core::session::SessionState::Done {
                    m.state = stenly_core::session::SessionState::Transcribed;
                    m.save(d)?;
                }
                eprintln!("template: {}", e.name);
            }
            let dirs = match dir {
                Some(d) => vec![d],
                None => pending_sessions(&paths.audio_dir),
            };
            if dirs.is_empty() {
                eprintln!("nothing to process");
            }
            for d in dirs {
                run_process(&cfg, &d)?;
            }
        }
        Cmd::Templates { session } => {
            let lib = stenly_core::library::Library::templates();
            let pick = match &session {
                Some(q) => {
                    let d = stenly_core::recipe::resolve_session(&paths.audio_dir, q)?;
                    let m = stenly_core::session::SessionMeta::load(&d)?;
                    Some(stenly_core::recipe::template_for(&cfg, &lib, &d, &m).id)
                }
                None => None,
            };
            for e in lib.all() {
                let mark = if pick.as_deref() == Some(e.id.as_str()) { "●" } else if e.id == cfg.templates.default { "○" } else { " " };
                println!("{mark} {:<20} {:<20} {}", e.id, e.name, e.description);
            }
            eprintln!("({}; ○ = default{})", lib.dir.display(), if pick.is_some() { ", ● = used for this meeting" } else { "" });
        }
        Cmd::Recipes => {
            let lib = stenly_core::library::Library::recipes();
            for e in lib.all() {
                println!("{:<30} {}", e.id, e.description);
            }
            eprintln!("({})", lib.dir.display());
        }
        Cmd::Recipe { name, session, save, into_note } => {
            let recipe = stenly_core::library::Library::recipes().find(&name)?;
            let dir = stenly_core::recipe::resolve_session(&paths.audio_dir, &session)?;
            let target = cfg.llm.target(stenly_core::secrets::llm_api_key);
            eprintln!("running “{}” on {} with {}…", recipe.name, dir.file_name().unwrap_or_default().to_string_lossy(), target.model);
            let t0 = std::time::Instant::now();
            let r = stenly_core::recipe::run(&cfg, &target, &recipe, &dir)?;
            println!("{}", r.text);
            eprintln!("({:.1}s)", t0.elapsed().as_secs_f32());
            if save || into_note.is_some() {
                stenly_core::recipe::push_result(&dir, r)?;
            }
            if let Some(h) = into_note {
                let h = if h.trim().is_empty() { stenly_core::recipe::default_heading(&recipe) } else { h };
                let note = stenly_core::recipe::save_to_note(&dir, &recipe.id, &h)?;
                eprintln!("saved under “{h}” in {}", note.display());
            }
        }
        Cmd::Speakers(c) => speakers_cmd(&cfg, &platform, c)?,
        Cmd::Context(c) => context_cmd(&cfg, c)?,
        Cmd::Upcoming { hours, all } => {
            let now = chrono::Utc::now();
            let mut c = cfg.calendar.clone();
            if all {
                c.require_meeting_link = false;
                c.ignore_titles.clear();
            }
            let to = now + chrono::Duration::hours(hours);
            if c.source.uses_eventkit() && platform.calendar.is_none() {
                eprintln!("EventKit is only available on macOS; use calendar.source = \"ics\" and calendar.ics_urls in {}", paths.config_file.display());
            } else if !c.source.uses_eventkit() && c.ics_urls.is_empty() && (!c.source.uses_kody() || c.kody_url.is_empty()) {
                eprintln!("no calendar connected; add an ICS URL in Settings → Calendar or set calendar.ics_urls in {}", paths.config_file.display());
            }
            let (mut events, mut errors) = stenly_core::calendar::upcoming(&c, now, to);
            if c.source.uses_eventkit() {
                match &platform.calendar {
                    Some(source) => match source.events(&c.calendar_ids, now, to) {
                        Ok(mut more) => events.append(&mut more),
                        Err(e) => errors.push(format!("Calendars on this Mac: {e:#}")),
                    },
                    None => errors.push("Calendars on this Mac are unavailable on this platform.".into()),
                }
            }
            let events = stenly_core::calendar::filter_events(&c, events);
            for e in errors {
                eprintln!("error: {e}");
            }
            for e in events {
                println!(
                    "{}  {:<40}  {}{}",
                    e.start.with_timezone(&chrono::Local).format("%a %H:%M"),
                    e.title,
                    e.link.as_ref().map(|l| l.provider.label()).unwrap_or("-"),
                    if e.declined { "  (declined)" } else { "" }
                );
            }
        }
        Cmd::KodyLogin { url } => {
            let url = if url.is_empty() { cfg.calendar.kody_url.clone() } else { url };
            anyhow::ensure!(!url.is_empty(), "pass the Kody URL, e.g. stenly-cli kody-login https://kody.example.com");
            let auth = stenly_core::kody::sign_in(&url, &|u| {
                eprintln!("opening browser… if nothing happens, visit:\n{u}");
                let _ = std::process::Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" }).arg(u).status();
            })?;
            Config::set_value("calendar.kody_url", auth.base_url.as_str().into())?;
            eprintln!("connected to {}", auth.base_url);
        }
        Cmd::ChatgptLogin => {
            let acct = stenly_core::chatgpt::sign_in(&|u| {
                eprintln!("opening browser… if nothing happens, visit:\n{u}");
                let _ = std::process::Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" }).arg(u).status();
            }, &|| false)?;
            Config::set_value("llm.provider", "chatgpt".into())?;
            eprintln!("signed in as {}; summaries now use ChatGPT ({})", acct.label(), cfg.llm.chatgpt.model);
        }
        Cmd::ChatgptLogout => {
            stenly_core::chatgpt::sign_out()?;
            eprintln!("signed out of ChatGPT");
        }
        Cmd::License(cmd) => license_cmd(cmd)?,
        Cmd::ChatgptStatus => {
            let acct = stenly_core::chatgpt::signed_in().context("not signed in; run `stenly-cli chatgpt-login`")?;
            let mut llm = cfg.llm.clone();
            llm.provider = "chatgpt".into();
            let target = llm.target(|| None);
            eprintln!("signed in as {} · provider {} · model {}", acct.label(), cfg.llm.provider, target.model);
            let ms = stenly_core::summary::ping(&target)?;
            eprintln!("ChatGPT answered in {:.1} s", ms as f32 / 1000.0);
        }
        Cmd::KodyCalendars => {
            let mut c = stenly_core::kody::KodyClient::from_saved().context("not signed in; run `stenly-cli kody-login`")?;
            for cal in stenly_core::kody::list_calendars(&mut c, &cfg.calendar.kody_integration)? {
                let on = cfg.calendar.calendar_ids.contains(&cal.id) || (cfg.calendar.calendar_ids.is_empty() && cal.primary);
                println!("{} {:<32} {}", if on { "●" } else { "○" }, cal.name, cal.id);
            }
        }
        Cmd::Pull { model } => {
            let mut last = String::new();
            stenly_core::ollama::pull(&cfg.llm.endpoint, &model, &mut |f, s| {
                let line = if f >= 0.0 { format!("{s} {:.0}%", f * 100.0) } else { s.to_string() };
                if line != last {
                    eprint!("\r{line:<60}");
                    last = line;
                }
            })?;
            eprintln!("\rpulled {model}{:<50}", "");
        }
        Cmd::MicUsers => {
            for u in platform.mic_activity.mic_users()? {
                let display = platform.mic_activity.window_display(&u).map(|d| format!("display {d}")).unwrap_or_default();
                println!("{:>7}  {}  {display}", u.pid.map(|p| p.to_string()).unwrap_or_default(), u.app_id);
            }
        }
        Cmd::WindowDisplay { app_id } => {
            let pid = app_id.strip_prefix("pid:").and_then(|p| p.parse().ok());
            let u = stenly_core::platform::MicUser { pid, app_id };
            println!("{:?}", platform.mic_activity.window_display(&u));
        }
        Cmd::Sessions => {
            for (dir, m) in list_sessions(&paths.audio_dir, 20) {
                println!("{}  {:?}  {}  {}", m.started.format("%Y-%m-%d %H:%M"), m.state, m.title, dir.display());
            }
        }
        Cmd::Config => {
            println!("platform:  {} ({})", platform.name, platform.audio.name());
            println!("config:    {}", paths.config_file.display());
            println!("notes:     {}", paths.notes_dir.display());
            println!("audio:     {}", paths.audio_dir.display());
            println!("models:    {}", paths.models_dir.display());
            println!("data:      {}", paths.data_dir.display());
            let _ = config::APP_NAME;
        }
        Cmd::Models { download } => {
            for m in stenly_asr::models::MODELS {
                let installed = stenly_asr::models::is_installed(m, &paths.models_dir);
                println!("{} {:<28} ~{} MB  {}", if installed { "✓" } else { " " }, m.id, m.approx_mb, m.description);
            }
            if download {
                let spec = stenly_asr::models::find(&cfg.asr.model).context("configured model not in catalog")?;
                stenly_asr::models::ensure(spec, &paths.models_dir, &mut |p| eprint!("\r{:.0}%", p * 100.0))?;
                stenly_asr::models::ensure_vad(&paths.models_dir)?;
                eprintln!("\rdone");
            }
        }
        Cmd::Send { session, to } => {
            let dir = find_session(&paths.audio_dir, &session)?;
            let dest = stenly_core::destinations::find(&cfg, &to).with_context(|| format!("no destination “{to}” (try `stenly-cli destinations`)"))?;
            eprintln!("sending {} → {} ({})", dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), dest.name(), dest.caption());
            let (rec, delivered) = stenly_core::destinations::send_session(dest.as_ref(), &dir, false)?;
            if let Some(md) = delivered.clipboard {
                println!("{md}");
            }
            if let Some(d) = &rec.detail {
                eprintln!("  {d}");
            }
            if let Some(u) = &rec.url {
                println!("{u}");
            }
        }
        Cmd::Share { session, revoke, to } => {
            let dir = find_session(&paths.audio_dir, &session)?;
            let destination = match to {
                Some(to) => {
                    let d = cfg.destinations.iter().find(|d| d.kind == stenly_core::destinations::Kind::Share && (d.id == to || d.name.eq_ignore_ascii_case(&to)))
                        .with_context(|| format!("no Stenly share-link destination named “{to}”"))?;
                    d.clone()
                }
                None => stenly_core::destinations::share_config(&cfg),
            };
            if revoke {
                stenly_core::share::revoke(&destination, &dir)?;
                println!("revoked");
            } else {
                let dest = stenly_core::destinations::build(&cfg, &destination);
                let _ = stenly_core::destinations::send_session(dest.as_ref(), &dir, false)?;
                println!("{}", stenly_core::share::link_for(&dir, Some(&destination.id))?.context("share key is missing from the Keychain")?);
            }
        }
        Cmd::Destinations { test } => match test {
            Some(name) => {
                let dest = stenly_core::destinations::find(&cfg, &name).with_context(|| format!("no destination “{name}”"))?;
                println!("{}", dest.test()?);
            }
            None => {
                for d in stenly_core::destinations::all(&cfg) {
                    let state = match d.ready() {
                        Ok(()) => stenly_core::destinations::rules::describe(d.auto()),
                        Err(e) => format!("not set up: {e:#}"),
                    };
                    println!("{:<18} {:<22} {:<28} {}", d.id(), d.name(), d.caption(), state);
                }
            }
        },
    }
    Ok(())
}

/// A session folder, "last", or (part of) a meeting title, newest first.
fn find_session(audio_dir: &std::path::Path, key: &str) -> Result<PathBuf> {
    let p = PathBuf::from(key);
    if p.join("meta.json").exists() {
        return Ok(p);
    }
    let sessions = list_sessions(audio_dir, usize::MAX);
    if key == "last" || key == "latest" {
        return sessions.into_iter().find(|(_, m)| m.state == stenly_core::session::SessionState::Done).map(|(d, _)| d).context("no finished meetings yet");
    }
    let k = key.to_lowercase();
    sessions
        .into_iter()
        .find(|(d, m)| m.title.to_lowercase().contains(&k) || d.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase() == k))
        .map(|(d, _)| d)
        .with_context(|| format!("no meeting matching “{key}” (see `stenly-cli sessions`)"))
}

fn print_agent_event(e: stenly_agent::agent::Event) {
    use stenly_agent::agent::Event;
    match e {
        Event::Thinking { turn, max } => eprintln!("  · turn {turn}/{max}"),
        Event::Tool { name, source, args } => eprintln!("    → {name} [{source}] {args}"),
        Event::ToolDone { name, ok, chars } => eprintln!("    {} {name} ({chars} chars)", if ok { "✓" } else { "✗" }),
        Event::Said(t) => eprintln!("    “{}”", t.chars().take(200).collect::<String>()),
    }
}

fn print_context(c: &stenly_core::context::MeetingContext) {
    println!("{}", serde_json::to_string_pretty(c).unwrap_or_default());
    if let Some(g) = &c.gathered {
        eprintln!("{} via {} in {:.1}s, {} tool calls{}", if c.is_empty() { "nothing found" } else { "gathered" }, g.model, g.secs, g.steps.len(), g.note.as_deref().map(|n| format!(" ({n})")).unwrap_or_default());
    }
}

fn context_cmd(cfg: &Config, c: ContextCmd) -> Result<()> {
    let paths = cfg.paths();
    match c {
        ContextCmd::Gather { session, process } => {
            let dir = stenly_core::recipe::resolve_session(&paths.audio_dir, &session)?;
            let t = cfg.context.target(&cfg.llm, stenly_core::secrets::llm_api_key);
            eprintln!("gathering context for {} with {}…", dir.file_name().unwrap_or_default().to_string_lossy(), t.model);
            let ctx = stenly_agent::gather_for_session(cfg, &dir, &mut print_agent_event)?;
            print_context(&ctx);
            eprintln!("saved {}", dir.join(stenly_core::context::FILE).display());
            if process {
                let mut meta = stenly_core::session::SessionMeta::load(&dir)?;
                if meta.state == stenly_core::session::SessionState::Done {
                    meta.state = stenly_core::session::SessionState::Transcribed;
                    meta.save(&dir)?;
                }
                run_process(cfg, &dir)?;
            }
        }
        ContextCmd::Next => {
            let now = chrono::Utc::now();
            let (events, _) = stenly_core::calendar::upcoming(&cfg.calendar, now - chrono::Duration::minutes(30), now + chrono::Duration::hours(24));
            let ev = events.into_iter().find(|e| !e.declined).context("no meeting in the next 24 hours")?;
            eprintln!("gathering context for “{}” ({})…", ev.title, ev.start.with_timezone(&chrono::Local).format("%a %H:%M"));
            let seed = stenly_agent::seed_for_event(cfg, Some(&ev));
            let scope = stenly_agent::tools::Scope { before: Some(ev.start.with_timezone(&chrono::Local)), ..Default::default() };
            let ctx = stenly_agent::gather(cfg, &seed, &scope, &mut print_agent_event);
            print_context(&ctx);
        }
        ContextCmd::Show { session } => {
            let dir = stenly_core::recipe::resolve_session(&paths.audio_dir, &session)?;
            let meta = stenly_core::session::SessionMeta::load(&dir)?;
            let ctx = stenly_core::context::for_session(&dir, &meta);
            print_context(&ctx);
            println!("\n--- summary prompt block ---{}", ctx.prompt_block());
        }
        ContextCmd::Sources => {
            let t = cfg.context.target(&cfg.llm, stenly_core::secrets::llm_api_key);
            println!("model: {} at {}{}", t.model, t.endpoint, if cfg.context.enabled { "" } else { "  (automatic gathering is off: [context] enabled = false)" });
            let set = stenly_agent::toolset(cfg, &Default::default());
            for s in &set.sources {
                let place = if s.locality.is_local() { "this Mac" } else { "cloud" };
                match &s.error {
                    Some(e) => println!("✗ {:<14} {place:<9} {e}{}", s.id, if s.needs_sign_in { "  → stenly-cli context login <id>" } else { "" }),
                    None => println!("✓ {:<14} {place:<9} {}{}", s.id, s.tools.join(", "), if s.skipped.is_empty() { String::new() } else { format!("  (not allowed: {})", s.skipped.join(", ")) }),
                }
            }
            for s in stenly_agent::servers(cfg).iter().filter(|s| !s.enabled) {
                println!("○ {:<14} off", s.id);
            }
        }
        ContextCmd::Login { id } => {
            let s = stenly_agent::servers(cfg).into_iter().find(|s| s.id == id).with_context(|| format!("no [[context.servers]] entry with id {id:?}"))?;
            anyhow::ensure!(!s.url.is_empty(), "{id} has no url (local servers need no sign-in; Kody uses the Calendar sign-in)");
            stenly_agent::oauth::sign_in(&s.id, &s.url, &|u| {
                eprintln!("opening browser… if nothing happens, visit:\n{u}");
                let _ = std::process::Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" }).arg(u).status();
            })?;
            eprintln!("signed in to {id}");
        }
        ContextCmd::Logout { id } => {
            stenly_agent::oauth::sign_out(&id);
            eprintln!("forgot {id}'s sign-in");
        }
    }
    Ok(())
}

fn run_process(cfg: &Config, dir: &std::path::Path) -> Result<()> {
    eprintln!("processing {}", dir.display());
    let mut t = stenly_asr::ParakeetTranscriber::new(&cfg.asr, &cfg.paths().models_dir)?;
    let t0 = std::time::Instant::now();
    let out = process_session(cfg, dir, &mut t, &mut |p| match p {
        Progress::Transcribing { channel, fraction } => eprint!("\r  transcribing {:<6} {:>3.0}%", channel.file_stem(), fraction * 100.0),
        Progress::SeparatingSpeakers => eprint!("\n  separating speakers…"),
        Progress::IdentifyingSpeakers => eprint!("\n  naming speakers…"),
        Progress::Summarizing => eprint!("\n  summarizing…"),
        Progress::Writing => eprintln!("\n  writing note"),
    })?;
    eprintln!("  {} words in {:.1}s → {}", out.transcript.word_count(), t0.elapsed().as_secs_f32(), out.note.display());
    for (name, err) in &out.sent {
        match err {
            None => eprintln!("  sent to {name}"),
            Some(e) => eprintln!("  sending to {name} failed: {e}"),
        }
    }
    Ok(())
}

fn speakers_cmd(cfg: &Config, platform: &stenly_platform::Platform, c: SpeakersCmd) -> Result<()> {
    use stenly_core::speakers::{ax, pipeline, timeline};
    let source = || platform.meeting_ax.clone().context("reading meeting apps isn't supported on this platform");
    match c {
        SpeakersCmd::Watch { secs, dir } => {
            let src = source()?;
            let w = match &dir {
                Some(d) => Some(timeline::Watcher::start(src.clone(), d, chrono::Local::now())?),
                None => None,
            };
            let t0 = std::time::Instant::now();
            let mut last: Option<ax::Observation> = None;
            while t0.elapsed().as_secs() < secs {
                let s0 = std::time::Instant::now();
                let o = ax::observe(&src.snapshot());
                if last.as_ref() != Some(&o) {
                    println!("{:>7.2}s  {:<6} talking: {:<30} roster: {}  ({} ms)", t0.elapsed().as_secs_f32(), o.app, o.active.join(", "), o.roster.join(", "), s0.elapsed().as_millis());
                    last = Some(o);
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            if let Some(w) = w {
                w.stop();
            }
        }
        SpeakersCmd::Dump { out } => {
            let t0 = std::time::Instant::now();
            let wins = source()?.snapshot();
            std::fs::write(&out, serde_json::to_vec_pretty(&wins)?)?;
            let nodes: usize = wins.iter().map(|w| { let mut n = 0; w.root.walk(&mut |_| n += 1); n }).sum();
            eprintln!("{} windows, {nodes} nodes in {} ms → {}", wins.len(), t0.elapsed().as_millis(), out.display());
            println!("{}", serde_json::to_string_pretty(&ax::observe(&wins))?);
        }
        SpeakersCmd::Drivers => {
            use stenly_core::speakers::drivers::DRIVERS;
            let y = |b: bool| if b { "✓" } else { "–" };
            println!("{:<18} {:>6} {:>8} {:>3} {:>4} {:>5}", "app", "roster", "speaking", "you", "host", "topic");
            for d in DRIVERS {
                let c = d.capabilities();
                println!("{:<18} {:>6} {:>8} {:>3} {:>4} {:>5}", d.label(), y(c.roster), y(c.active_speaker), y(c.me), y(c.host), y(c.topic));
            }
        }
        SpeakersCmd::Who { dir } => {
            let meta = dir.as_ref().map(|d| stenly_core::session::SessionMeta::load(d)).transpose()?;
            let events = dir.as_ref().map(|d| timeline::load(d)).unwrap_or_default();
            let app = timeline::summarize(&events);
            let ev = meta.as_ref().and_then(|m| m.event.as_ref());
            let mut me = stenly_core::identity::Me::resolve(&cfg.me, ev, app.me.as_deref());
            me.locate(&app.roster);
            println!("you:      {}", me.display().unwrap_or("(unknown: set Settings → You)"));
            println!("  names:  {}", me.names.join(", "));
            println!("  emails: {}", me.emails.join(", "));
            if let Some(t) = &me.tile {
                println!("  tile:   {t}");
            }
            if let Some(m) = &meta {
                println!("meeting:  {}  ({})", m.title, m.platform_label().unwrap_or("unknown app"));
                println!("app saw:  roster {:?}, host {:?}, topic {:?}, you {:?}  ({} timeline lines)", app.roster, app.host, app.topic, app.me, events.len());
                let people = stenly_core::speakers::participants::Participants::gather(&me, ev, &app);
                for p in &people.people {
                    println!("  {:<28} {:<28} {:?}", p.name, p.email.as_deref().unwrap_or(""), p.sources);
                }
                if let Some(p) = people.only_other() {
                    println!("one-to-one with {}", p.name);
                }
            }
        }
        SpeakersCmd::Parse { file } => {
            let wins: Vec<ax::AxWindow> = serde_json::from_slice(&std::fs::read(&file)?)?;
            println!("{}", serde_json::to_string_pretty(&ax::observe(&wins))?);
        }
        SpeakersCmd::Name { dir, reset, no_note } => {
            let meta = stenly_core::session::SessionMeta::load(&dir)?;
            let tf = dir.join("transcript.json");
            let mut t: stenly_core::transcript::Transcript = serde_json::from_slice(&std::fs::read(&tf)?)?;
            if reset {
                t.speakers.clear();
                for s in t.segments.iter_mut() {
                    s.name = None;
                    if !t.in_person {
                        s.speaker = None;
                    }
                }
                let _ = std::fs::remove_file(dir.join(pipeline::SESSION_VOICES));
            }
            let raw = std::fs::read(dir.join(stenly_core::daemon::RAW_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok());
            let mut asr = stenly_asr::ParakeetTranscriber::new(&cfg.asr, &cfg.paths().models_dir)?;
            let t0 = std::time::Instant::now();
            pipeline::identify(cfg, &dir, &meta, &mut t, raw, &mut asr)?;
            std::fs::write(&tf, serde_json::to_vec_pretty(&t)?)?;
            eprintln!("named in {:.1}s", t0.elapsed().as_secs_f32());
            if !no_note && pipeline::refresh_session_outputs(cfg, &dir, &t)? {
                eprintln!("note rewritten");
            }
            for i in &t.speakers {
                let link = match (&i.person, &i.email) {
                    (Some(p), Some(e)) => format!("= {p} <{e}>"),
                    (Some(p), None) => format!("= {p}"),
                    (None, Some(e)) => format!("<{e}>"),
                    _ => String::new(),
                };
                println!(
                    "{:<10} {:>6.0}s  {:<16} {:<12} {:.2}  {link}",
                    stenly_core::transcript::unnamed_label(i.channel, i.speaker),
                    i.talk_secs,
                    i.name.as_deref().unwrap_or("-"),
                    i.source.map(|s| format!("{s:?}")).unwrap_or_else(|| "-".into()),
                    i.confidence
                );
            }
        }
        SpeakersCmd::Rename { dir, channel, speaker, name } => {
            let ch = match channel.to_lowercase().as_str() {
                "me" | "mic" => Channel::Mic,
                _ => Channel::System,
            };
            pipeline::rename_in_session(cfg, &dir, ch, speaker.map(|n| n.saturating_sub(1)), &name)?;
            eprintln!("renamed");
        }
        SpeakersCmd::Voices => {
            let store = stenly_core::voices::VoiceStore::load();
            for v in &store.voices {
                println!("{:<24} {:>6.0}s  updated {}  {}", v.name, v.secs, v.updated.format("%Y-%m-%d %H:%M"), v.email.as_deref().unwrap_or(""));
                for x in &v.samples {
                    println!("    {:<12} {:>6.0}s  {}", x.source, x.secs, if x.key.is_empty() { "(before per-meeting tracking)" } else { &x.key });
                }
            }
            if !store.pending.is_empty() {
                println!("waiting for a second meeting to agree (not used for matching):");
                for p in &store.pending {
                    println!("    {:<20} {:<12} {:>6.0}s  {}", p.name, p.sample.source, p.sample.secs, p.sample.key);
                }
            }
        }
        SpeakersCmd::Forget { name, session } => {
            let mut s = stenly_core::voices::VoiceStore::load();
            match session {
                // one meeting's voices only (e.g. a call where a guess was wrong);
                // with a name, only what it added to that person's voiceprint
                Some(dir) => {
                    let prefix = format!("{}#", pipeline::session_id(&dir));
                    let keys: Vec<String> = s
                        .voices
                        .iter()
                        .filter(|v| name.as_deref().is_none_or(|n| v.name.eq_ignore_ascii_case(n)))
                        .flat_map(|v| v.samples.iter())
                        .chain(s.pending.iter().filter(|p| name.as_deref().is_none_or(|n| p.name.eq_ignore_ascii_case(n))).map(|p| &p.sample))
                        .filter(|x| x.key.starts_with(&prefix))
                        .map(|x| x.key.clone())
                        .collect();
                    for k in &keys {
                        match s.unenroll(k) {
                            Some(n) => eprintln!("took {k} out of {n}"),
                            None => eprintln!("dropped unconfirmed {k}"),
                        }
                    }
                    if keys.is_empty() {
                        eprintln!("nothing remembered from that meeting");
                    }
                }
                None => {
                    let name = name.context("give a name, or --session <dir>")?;
                    let before = s.voices.len();
                    s.forget(&name);
                    eprintln!("forgot {} voice(s)", before - s.voices.len());
                }
            }
            s.save()?;
        }
    }
    Ok(())
}

fn license_cmd(cmd: LicenseCmd) -> Result<()> {
    use stenly_core::license::{LicenseManager, State};
    let m = LicenseManager::system();
    let now = chrono::Utc::now;
    let provider = || stenly_core::license::default_provider().context("licensing isn't set up in this build");
    let show = |m: &LicenseManager| {
        let s = m.snapshot(now());
        println!("{}", s.headline());
        match s.state() {
            State::Open => println!("This build doesn't enforce a licence (source builds are free)."),
            State::Licensed => {
                let l = s.license.as_ref().unwrap();
                println!("key:           {}", l.masked_key);
                println!("last checked:  {}", l.last_validated.format("%Y-%m-%d"));
                println!("works offline: {} more days without a check", l.grace_days_left);
            }
            State::Trial | State::Expired => {
                println!("meetings:      {} of {} free meetings used", s.used.min(s.free_meetings), s.free_meetings);
                if let Some(msg) = s.blocked_message() {
                    println!("{msg}");
                } else if let Some(url) = stenly_core::license::checkout_url() {
                    println!("buy:           {url}");
                }
            }
        }
    };
    match cmd {
        LicenseCmd::Status { check } => {
            if check {
                let r = m.refresh(&*provider()?, now(), true);
                eprintln!("check: {r:?}");
            }
            show(&m);
        }
        LicenseCmd::Activate { key } => {
            let key = match key.or_else(|| std::env::var("STENLY_LICENSE_KEY").ok()) {
                Some(k) => k,
                None => {
                    eprintln!("Licence key: ");
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line)?;
                    line
                }
            };
            m.activate(&*provider()?, &key, now())?;
            eprintln!("activated");
            show(&m);
        }
        LicenseCmd::Deactivate => {
            m.deactivate(&*provider()?, now())?;
            eprintln!("deactivated; this key can be activated on another Mac");
            show(&m);
        }
    }
    Ok(())
}
