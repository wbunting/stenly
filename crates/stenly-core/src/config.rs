//! `config.toml`: the single source of truth for user settings.
//!
//! The settings UI edits this same file through [`Config::set_value`], which uses
//! `toml_edit` so comments and ordering written by hand survive.

use crate::util::{collapse_tilde, expand_tilde};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "stenly";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub app: AppConfig,
    /// Who the user is (speaker names, "Me" in notes).
    pub me: MeConfig,
    pub output: OutputConfig,
    pub asr: AsrConfig,
    pub llm: LlmConfig,
    pub calendar: CalendarConfig,
    pub detect: DetectConfig,
    pub mcp: McpConfig,
    pub audio: AudioConfig,
    pub dictation: DictationConfig,
    /// Where notes can be sent besides the notes folder (`[[destinations]]`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub destinations: Vec<crate::destinations::DestinationConfig>,
    pub templates: TemplatesConfig,
    /// Background on meetings from past notes, Notion and MCP servers (`[context]`).
    pub context: ContextConfig,
}

/// Settings → Context: the agent that gathers background on a meeting before
/// it is summarized, and the sources it may read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextConfig {
    /// Gather context automatically (before calendar meetings, and before
    /// summarizing any meeting that has none yet).
    pub enabled: bool,
    /// Minutes before a calendar meeting to start gathering.
    pub before_mins: i64,
    /// "summary" = the summary model; "api" = `[llm.api]`; or "custom" with
    /// `endpoint`/`model` below. The agent needs a model that calls tools well,
    /// which a Codex / Claude Code summary provider ("cli") can't do here.
    pub provider: String,
    pub endpoint: String,
    pub model: String,
    pub api_key_env: String,
    /// Model turns per meeting.
    pub max_turns: u32,
    /// Wall-clock budget per meeting.
    pub budget_secs: u64,
    /// Built-in sources.
    pub past_meetings: bool,
    pub vault: bool,
    /// The vault to search ("" = the Obsidian vault holding the notes folder).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub vault_dir: String,
    /// Search the Notion workspace (token from Settings → Context, or a Notion destination's).
    pub notion: bool,
    /// The user's MCP servers (`[[context.servers]]`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<McpServerConfig>,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            before_mins: 10,
            provider: "summary".into(),
            endpoint: String::new(),
            model: String::new(),
            api_key_env: String::new(),
            max_turns: 12,
            budget_secs: 120,
            past_meetings: true,
            vault: true,
            vault_dir: String::new(),
            notion: false,
            servers: Vec::new(),
        }
    }
}

/// One MCP server the context agent may call: a streamable-HTTP URL or a
/// command (stdio). OAuth tokens and bearer keys live outside this file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct McpServerConfig {
    /// Short name; tools are offered to the model as `<id>__<tool>`.
    pub id: String,
    pub enabled: bool,
    /// `https://host/mcp` for a remote server.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// argv for a local (stdio) server, e.g. ["npx", "-y", "@acme/mcp"].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
    /// Tools the agent may call. Empty = every tool the server marks read-only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// Extra guidance for the agent ("use search, then execute with a gmail snippet").
    #[serde(skip_serializing_if = "String::is_empty")]
    pub instructions: String,
}

impl ContextConfig {
    /// The model the agent runs on.
    pub fn target(&self, llm: &LlmConfig, keychain: impl FnOnce() -> Option<String>) -> LlmTarget {
        match self.provider.as_str() {
            "custom" => LlmTarget {
                endpoint: self.endpoint.trim().trim_end_matches('/').to_string(),
                model: self.model.trim().to_string(),
                api_key: (!self.api_key_env.is_empty()).then(|| std::env::var(&self.api_key_env).ok()).flatten(),
                reasoning_effort: String::new(),
                keep_alive: None,
                local: false,
                cli: None,
                backend: Backend::OpenAi,
            },
            "api" => LlmConfig { provider: "api".into(), ..llm.clone() }.target(keychain),
            _ => llm.target(keychain),
        }
    }
}

/// The person recording: finds them among a meeting app's tiles and gives
/// "Me" a real name. Empty fields are filled in from the calendar account
/// and the meeting app when `auto` is on (see `identity`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MeConfig {
    /// Full name, as meeting apps show it ("Alex Rowan").
    pub name: String,
    /// Every address you're invited with.
    pub emails: Vec<String>,
    /// Other names people see or call you by ("Alex", "Alexander Rowan").
    pub aliases: Vec<String>,
    /// What your own lines are labelled in transcripts and notes, and the
    /// owner of your action items. "" = your first name (from `name`, else
    /// the calendar account: alex@x.com → "Alex"); "Me" keeps the old label.
    pub label: String,
    /// Add what Stenly can tell on its own: calendar account addresses and
    /// the name the meeting app signs you in as.
    pub auto: bool,
}

impl Default for MeConfig {
    fn default() -> Self {
        Self { name: String::new(), emails: vec![], aliases: vec![], label: String::new(), auto: true }
    }
}

/// Note templates (files in `<config dir>/templates/`, see `library`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TemplatesConfig {
    /// Pick a template from the meeting's title and attendees (each template's
    /// `match:` keywords). Off = always use `default`.
    pub auto: bool,
    /// Template id for meetings nothing else matches.
    pub default: String,
}

impl Default for TemplatesConfig {
    fn default() -> Self {
        Self { auto: true, default: "general".into() }
    }
}

/// Settings shared by meetings and dictation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct AudioConfig {
    /// Microphones in order of preference (device id or name). The first one
    /// that's connected is used; empty = the system default (skipping a
    /// lid-closed built-in mic).
    pub mic_priority: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShortcutMode {
    /// Press to start, press again to stop.
    Toggle,
    /// Hold to talk, release to stop.
    PushToTalk,
    /// Quick tap = hands-free (tap again to stop); hold = push-to-talk.
    Hybrid,
}

/// Dictation: hold or tap a key anywhere, speak, and the text is typed into
/// whatever app has focus.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DictationConfig {
    pub enabled: bool,
    /// "right_command", "fn", "right_option", … or a combo like "ctrl+shift+space".
    pub shortcut: String,
    pub mode: ShortcutMode,
    /// Optional second shortcut (same behaviour).
    pub shortcut2: String,
    pub mode2: ShortcutMode,
    /// Paste the last dictation again ("" = off).
    pub paste_last_shortcut: String,
    /// Put the clipboard back after pasting.
    pub restore_clipboard: bool,
    pub restore_clipboard_secs: f32,
    /// Press Return after pasting (chat apps).
    pub auto_send: bool,
    /// Play start/stop sounds.
    pub sounds: bool,
    /// Mute other audio while dictating.
    pub mute_system_audio: bool,
    /// Drop filler words (um, uh) and bracketed noise tags.
    pub remove_fillers: bool,
    /// Rewrite with the summary model (Settings → Summaries) before pasting.
    pub ai_cleanup: bool,
    /// "clean" (fix grammar, keep voice) or "assistant" (answer the request) or a custom prompt.
    pub ai_prompt: String,
    /// Word replacements applied after transcription: "from" → "to". Comma-
    /// separated variants on the left ("vuon, view on" → "Vuon").
    pub replacements: Vec<Replacement>,
    /// Keep this many past dictations in history (0 = none).
    pub history_limit: usize,
    /// Transcribe while you talk (live preview, fast finish) and how to cut.
    pub stream: StreamMode,
    /// Show the live transcript above the recorder.
    pub live_preview: bool,
    /// Keep speech recognition ready (0), or unload its worker after this many
    /// idle seconds. Audio is buffered while a sleeping worker reloads.
    pub model_idle_secs: u64,
}

/// How dictation is transcribed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamMode {
    /// Overlapping windows stitched on word timestamps.
    Overlap,
    /// Cut at pauses; each phrase on its own.
    Pause,
    /// Record everything, transcribe after stop.
    Off,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Replacement {
    pub from: String,
    pub to: String,
}

impl Default for DictationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            shortcut: "right_command".into(),
            mode: ShortcutMode::Hybrid,
            shortcut2: String::new(),
            mode2: ShortcutMode::Hybrid,
            paste_last_shortcut: "alt+cmd+v".into(),
            restore_clipboard: true,
            restore_clipboard_secs: 1.5,
            auto_send: false,
            sounds: true,
            mute_system_audio: false,
            remove_fillers: true,
            ai_cleanup: false,
            ai_prompt: "clean".into(),
            replacements: Vec::new(),
            history_limit: 200,
            stream: StreamMode::Pause,
            live_preview: true,
            model_idle_secs: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct McpConfig {
    /// While the app runs, serve meetings to AI tools over MCP at
    /// http://127.0.0.1:<port>/mcp (localhost only). `stenly-mcp` (stdio) works regardless.
    pub http: bool,
    pub port: u16,
    /// Expose mutating MCP tools such as share_meeting. Off by default: MCP
    /// otherwise remains a read-only local archive.
    pub write_tools: bool,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self { http: true, port: 4791, write_tools: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AppConfig {
    /// First-run onboarding finished.
    pub onboarded: bool,
    /// Show the next meeting ("Design review in 5m") next to the menubar icon.
    pub show_next_in_menubar: bool,
    /// Start stenly when you log in.
    pub launch_at_login: bool,
    /// Opt-in release checks; no effect until a signed update feed is bundled.
    pub check_updates: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self { onboarded: false, show_next_in_menubar: true, launch_at_login: false, check_updates: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OutputConfig {
    /// Where meeting notes (markdown) are written. Usually a folder in your vault.
    pub notes_dir: String,
    /// Where recordings are kept. Can live inside or outside the vault.
    pub audio_dir: String,
    /// File name template. Placeholders: {date} {time} {title} {slug}.
    pub filename: String,
    /// Optional daily note to append a link to (same placeholders). Empty = off.
    pub daily_note: String,
    /// Days to keep audio after transcription. 0 = delete immediately; -1 = forever.
    pub keep_audio_days: i64,
    /// Include the full transcript in the note (collapsed callout).
    pub include_transcript: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AsrConfig {
    /// Model id; see `stenly models`.
    pub model: String,
    /// Where models are downloaded. Empty = platform cache dir.
    pub models_dir: String,
    /// onnxruntime execution provider: "cpu", "coreml" or "cuda".
    pub provider: String,
    pub threads: i32,
    /// Names, products and jargon that should be spelled exactly like this
    /// ("Vuon", "Viewon", "Kody"). Used to fix the transcript and the summary.
    pub vocabulary: Vec<String>,
    /// In-person meetings (everyone on one mic): tell voices apart as
    /// "Speaker 1/2/…". Local; downloads a 40 MB voice model on first use.
    pub separate_speakers: bool,
    /// Put real names on speakers: the meeting app's active-speaker highlight
    /// (Accessibility), voices you named before, the invite list, and the
    /// summary model for the rest.
    pub speaker_names: bool,
    /// Keep raw accessibility snapshots of the meeting app in the session
    /// folder (`ax/`), for building and checking meeting-app drivers.
    pub save_meeting_app_snapshots: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LlmConfig {
    pub enabled: bool,
    /// Which backend writes summaries: "ollama" (local, default), "api"
    /// (any OpenAI-compatible service configured in `[llm.api]`), "cli"
    /// (an installed Codex / Claude Code, see `[llm.cli]`) or "chatgpt"
    /// (the user's ChatGPT plan, after signing in; see `[llm.chatgpt]`).
    pub provider: String,
    /// Any OpenAI-compatible endpoint (Ollama, llama.cpp, LM Studio, vLLM, ...).
    pub endpoint: String,
    pub model: String,
    /// Name of an env var holding an API key, if the endpoint needs one.
    pub api_key_env: String,
    /// Optional path to a prompt template overriding the built-in one.
    pub prompt_file: String,
    /// Sent as `reasoning_effort` ("none" disables thinking on reasoning models
    /// like Nemotron 3 / Qwen 3 — much faster, plenty for summaries). "" = server default.
    pub reasoning_effort: String,
    pub timeout_secs: u64,
    /// Ollama `keep_alive`: how long the model stays in RAM after a summary.
    /// "0" = unload immediately (meetings are minutes apart). "" = don't send.
    pub keep_alive: String,
    /// Language to write notes and recipe results in ("" = the meeting's own).
    pub language: String,
    /// Hosted, OpenAI-compatible provider used when `provider = "api"`.
    pub api: ApiLlmConfig,
    /// The Codex / Claude Code command-line tool used when `provider = "cli"`.
    pub cli: CliLlmConfig,
    /// The user's ChatGPT plan, used when `provider = "chatgpt"`.
    pub chatgpt: ChatGptLlmConfig,
}

/// Summaries on the user's ChatGPT plan (Sign in with ChatGPT). The sign-in
/// itself is kept in the Keychain, never here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ChatGptLlmConfig {
    pub model: String,
    /// "low" | "medium" | "high" ("" = the model's default).
    pub reasoning_effort: String,
}

impl Default for ChatGptLlmConfig {
    fn default() -> Self {
        Self { model: crate::chatgpt::DEFAULT_MODEL.into(), reasoning_effort: crate::chatgpt::DEFAULT_EFFORT.into() }
    }
}

/// The wire protocol a target speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    /// OpenAI-compatible chat completions (Ollama, hosted APIs).
    #[default]
    OpenAi,
    /// The ChatGPT backend's Responses API, with the signed-in account ([`crate::chatgpt`]).
    ChatGpt,
}


/// Summaries through a coding-agent CLI the user is already signed in to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct CliLlmConfig {
    /// "codex" or "claude" ("" = whichever is installed).
    pub tool: String,
    /// Path to the executable ("" = look on PATH and in the usual install folders).
    pub path: String,
    /// Model name passed to the tool ("" = the tool's own default).
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ApiLlmConfig {
    /// Base URL ending in /v1, e.g. https://api.openai.com/v1,
    /// https://openrouter.ai/api/v1, https://api.anthropic.com/v1.
    pub endpoint: String,
    pub model: String,
    /// The API key itself. Prefer `api_key_env` or the app (which stores it in
    /// the macOS Keychain) if you sync this file anywhere.
    pub api_key: String,
    /// Name of an env var holding the key; used when `api_key` is empty.
    pub api_key_env: String,
    /// `reasoning_effort` for reasoning models ("" = provider default).
    pub reasoning_effort: String,
}

/// The endpoint/model/key a summary request should use, resolved from the
/// active provider.
#[derive(Debug, Clone, PartialEq)]
pub struct LlmTarget {
    pub endpoint: String,
    pub model: String,
    pub api_key: Option<String>,
    pub reasoning_effort: String,
    /// Ollama-only `keep_alive` (never sent to hosted APIs).
    pub keep_alive: Option<String>,
    pub local: bool,
    /// Set for `provider = "cli"`: run this tool instead of calling `endpoint`.
    pub cli: Option<crate::cli_llm::CliSpec>,
    /// The wire protocol (chat completions, or the ChatGPT Responses API).
    pub backend: Backend,
}

impl LlmConfig {
    pub fn uses_api(&self) -> bool {
        self.provider.eq_ignore_ascii_case("api")
    }

    /// Summaries are written by an installed Codex / Claude Code.
    pub fn uses_cli(&self) -> bool {
        self.provider.eq_ignore_ascii_case("cli")
    }

    /// Summaries are written by an Ollama-style server on this Mac (neither a hosted API nor a CLI).
    pub fn uses_ollama(&self) -> bool {
        !self.uses_api() && !self.uses_cli() && !self.uses_chatgpt()
    }

    pub fn uses_chatgpt(&self) -> bool {
        self.provider.eq_ignore_ascii_case("chatgpt")
    }

    /// Resolve where summaries go. `keychain` supplies a stored key for the
    /// hosted provider when the config holds none.
    pub fn target(&self, keychain: impl FnOnce() -> Option<String>) -> LlmTarget {
        if self.uses_chatgpt() {
            let c = &self.chatgpt;
            let model = Some(c.model.trim()).filter(|m| !m.is_empty()).unwrap_or(crate::chatgpt::DEFAULT_MODEL);
            return LlmTarget {
                endpoint: crate::chatgpt::BACKEND.into(),
                model: model.to_string(),
                // the signed-in account's token is fetched (and refreshed) per request
                api_key: None,
                reasoning_effort: c.reasoning_effort.trim().to_string(),
                keep_alive: None,
                local: false,
                backend: Backend::ChatGpt,
                cli: None,
            };
        }
        if self.uses_cli() {
            let c = &self.cli;
            // no tool chosen yet: the first one that is installed
            let tool = crate::cli_llm::Tool::parse(&c.tool)
                .or_else(|| crate::cli_llm::Tool::ALL.into_iter().find(|t| crate::cli_llm::find(*t, "").is_some()))
                .unwrap_or(crate::cli_llm::Tool::Codex);
            let model = c.model.trim().to_string();
            return LlmTarget {
                endpoint: String::new(),
                model: if model.is_empty() { tool.label().to_string() } else { model.clone() },
                api_key: None,
                reasoning_effort: String::new(),
                keep_alive: None,
                local: false,
                cli: Some(crate::cli_llm::CliSpec { tool, path: c.path.trim().to_string(), model }),
                backend: Backend::OpenAi,
            };
        }
        if self.uses_api() {
            let a = &self.api;
            let key = Some(a.api_key.trim().to_string())
                .filter(|k| !k.is_empty())
                .or_else(|| (!a.api_key_env.is_empty()).then(|| std::env::var(&a.api_key_env).ok()).flatten())
                .or_else(keychain);
            LlmTarget {
                endpoint: a.endpoint.trim().trim_end_matches('/').to_string(),
                model: a.model.trim().to_string(),
                api_key: key,
                reasoning_effort: a.reasoning_effort.clone(),
                keep_alive: None,
                local: false,
                cli: None,
                backend: Backend::OpenAi,
            }
        } else {
            LlmTarget {
                endpoint: self.endpoint.trim_end_matches('/').to_string(),
                model: self.model.clone(),
                api_key: (!self.api_key_env.is_empty()).then(|| std::env::var(&self.api_key_env).ok()).flatten(),
                reasoning_effort: self.reasoning_effort.clone(),
                keep_alive: Some(self.keep_alive.clone()).filter(|s| !s.is_empty()),
                local: true,
                cli: None,
                backend: Backend::OpenAi,
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CalendarConfig {
    /// Preferred source. An omitted value is legacy mode, preserving existing Kody and ICS configs after upgrade.
    #[serde(default)]
    pub source: CalendarSourceKind,
    /// Kody server used as the auth/routing layer for Google Calendar ("" = not connected).
    pub kody_url: String,
    /// Name of the Kody OAuth integration to read calendars with.
    pub kody_integration: String,
    /// Google calendar ids to watch. Empty = your primary calendar.
    pub calendar_ids: Vec<String>,
    /// Extra ICS feeds (Google "Secret address in iCal format", or any ICS URL/file).
    pub ics_urls: Vec<String>,
    /// Seconds before start to show the "record?" prompt.
    pub remind_before_secs: i64,
    /// Only prompt for events with a Zoom / Meet / Teams / ... link.
    pub require_meeting_link: bool,
    /// Event titles (case-insensitive substrings) that never prompt.
    pub ignore_titles: Vec<String>,
    /// Event titles (case-insensitive substrings) that start recording without asking.
    pub auto_record_titles: Vec<String>,
    /// How often to re-fetch calendars.
    pub refresh_mins: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DetectConfig {
    /// Prompt when a meeting app starts using the mic without a calendar event.
    pub ad_hoc_calls: bool,
    /// App ids / process names considered "meeting apps" for ad-hoc detection.
    pub meeting_apps: Vec<String>,
    /// Stop automatically after this many seconds of silence on both channels. 0 = off.
    pub auto_stop_silence_secs: u64,
    /// Stop when no meeting app holds the mic for this many seconds. 0 = off.
    pub auto_stop_mic_released_secs: u64,
}


impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            notes_dir: "~/Documents/stenly/Meetings".into(),
            audio_dir: "~/Documents/stenly/Recordings".into(),
            filename: "{date} {title}.md".into(),
            daily_note: String::new(),
            keep_audio_days: 30,
            include_transcript: true,
        }
    }
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            model: "parakeet-tdt-0.6b-v2-int8".into(),
            models_dir: String::new(),
            provider: "cpu".into(),
            threads: 4,
            vocabulary: Vec::new(),
            separate_speakers: true,
            speaker_names: true,
            save_meeting_app_snapshots: false,
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: "ollama".into(),
            endpoint: "http://localhost:11434/v1".into(),
            model: "qwen3.5:9b".into(),
            api_key_env: String::new(),
            prompt_file: String::new(),
            reasoning_effort: "none".into(),
            timeout_secs: 600,
            keep_alive: "0".into(),
            language: String::new(),
            api: ApiLlmConfig::default(),
            cli: CliLlmConfig::default(),
            chatgpt: ChatGptLlmConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CalendarSourceKind {
    Eventkit,
    Ics,
    Kody,
    #[default]
    Legacy,
}

impl CalendarSourceKind {
    pub fn uses_eventkit(&self) -> bool { matches!(self, Self::Eventkit) }
    pub fn uses_kody(&self) -> bool { matches!(self, Self::Kody | Self::Legacy) }
    /// ICS links are supplemental: adding one must not turn off EventKit or an existing Kody connection.
    pub fn uses_ics(&self) -> bool { true }
}

impl Default for CalendarConfig {
    fn default() -> Self {
        Self {
            // `default_config_toml` explicitly selects the platform default for new files.
            // Struct default stays Legacy so old partial TOML keeps its source.
            source: CalendarSourceKind::Legacy,
            kody_url: String::new(),
            kody_integration: "google".into(),
            calendar_ids: vec![],
            ics_urls: vec![],
            remind_before_secs: 60,
            require_meeting_link: true,
            ignore_titles: vec![],
            auto_record_titles: vec![],
            refresh_mins: 10,
        }
    }
}

impl Default for DetectConfig {
    fn default() -> Self {
        Self {
            ad_hoc_calls: true,
            meeting_apps: vec![
                // macOS bundle ids
                "us.zoom.xos".into(),
                "com.microsoft.teams2".into(),
                "com.microsoft.teams".into(),
                "com.tinyspeck.slackmacgap".into(),
                "com.cisco.webexmeetingsapp".into(),
                "com.hnc.Discord".into(),
                "com.apple.FaceTime".into(),
                // browsers (Google Meet / Teams web)
                "com.google.Chrome".into(),
                "company.thebrowser.Browser".into(),
                "com.apple.Safari".into(),
                "org.mozilla.firefox".into(),
                "com.brave.Browser".into(),
                "com.microsoft.edgemac".into(),
                "net.imput.helium".into(),
                "company.thebrowser.dia".into(),
                "com.vivaldi.Vivaldi".into(),
                "com.operasoftware.Opera".into(),
                "org.chromium.Chromium".into(),
                // Linux process / app names
                "zoom".into(),
                "teams-for-linux".into(),
                "slack".into(),
                "chrome".into(),
                "chromium".into(),
                "firefox".into(),
                "discord".into(),
            ],
            auto_stop_silence_secs: 180,
            auto_stop_mic_released_secs: 1,
        }
    }
}

/// Resolved, absolute, ready-to-use paths.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub models_dir: PathBuf,
    pub notes_dir: PathBuf,
    pub audio_dir: PathBuf,
    pub sessions_dir: PathBuf,
}

pub fn project_dirs() -> directories::ProjectDirs {
    directories::ProjectDirs::from("", "", APP_NAME).expect("no home directory")
}

/// `$STENLY_CONFIG` or the platform config dir.
pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("STENLY_CONFIG") {
        return expand_tilde(&p);
    }
    project_dirs().config_dir().join("config.toml")
}

pub fn data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("STENLY_DATA_DIR") {
        return expand_tilde(&p);
    }
    project_dirs().data_dir().to_path_buf()
}

impl Config {
    /// Load from disk, writing a commented default file on first run.
    pub fn load_or_init() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, default_config_toml())
                .with_context(|| format!("writing {}", path.display()))?;
            tracing::info!(path = %path.display(), "wrote default config");
        }
        Self::load(&path)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn paths(&self) -> Paths {
        let data = data_dir();
        let models_dir = if self.asr.models_dir.trim().is_empty() {
            project_dirs().cache_dir().join("models")
        } else {
            expand_tilde(&self.asr.models_dir)
        };
        Paths {
            config_file: config_path(),
            sessions_dir: data.join("sessions"),
            data_dir: data,
            models_dir,
            notes_dir: expand_tilde(&self.output.notes_dir),
            audio_dir: expand_tilde(&self.output.audio_dir),
        }
    }

    /// Edit a single dotted key (e.g. `output.notes_dir`) in the config file,
    /// preserving everything else, then return the reloaded config.
    pub fn set_value(key: &str, value: toml_edit::Value) -> Result<Self> {
        let path = config_path();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut doc: toml_edit::DocumentMut = text.parse().context("parsing config")?;
        let mut parts: Vec<&str> = key.split('.').collect();
        let leaf = parts.pop().context("empty key")?;
        let mut table = doc.as_table_mut();
        for p in parts {
            if !table.contains_key(p) {
                table.insert(p, toml_edit::Item::Table(toml_edit::Table::new()));
            }
            table = table[p].as_table_mut().with_context(|| format!("{p} is not a table"))?;
        }
        match table.get_mut(leaf).and_then(|i| i.as_value_mut()) {
            Some(existing) => {
                // keep the trailing comment / spacing of the existing entry
                let decor = existing.decor().clone();
                *existing = value;
                *existing.decor_mut() = decor;
            }
            None => {
                table.insert(leaf, toml_edit::Item::Value(value));
            }
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, doc.to_string())?;
        Self::load(&path)
    }

    /// Replace `[[context.servers]]` (the rest of the file is kept as written).
    pub fn save_context_servers(list: &[McpServerConfig]) -> Result<Self> {
        replace_tables(&config_path(), &["context", "servers"], list)
    }

    pub fn set_path(key: &str, path: &Path) -> Result<Self> {
        Self::set_value(key, collapse_tilde(path).into())
    }
}

/// Replace the array of tables at `table_path` (`["destinations"]`,
/// `["context", "servers"]`) with `items`, keeping everything else in the file
/// as written, write it atomically and return the reloaded config.
pub fn replace_tables<T: Serialize>(path: &Path, table_path: &[&str], items: &[T]) -> Result<Config> {
    let (leaf, parents) = table_path.split_last().context("empty table path")?;
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text.parse().context("parsing config")?;
    let mut table = doc.as_table_mut();
    for p in parents {
        if !table.contains_key(p) {
            table.insert(p, toml_edit::Item::Table(toml_edit::Table::new()));
        }
        table = table[*p].as_table_mut().with_context(|| format!("[{p}] is not a table"))?;
    }
    table.remove(leaf);
    if !items.is_empty() {
        let mut wrap = toml::Table::new();
        wrap.insert(leaf.to_string(), toml::Value::try_from(items).context("serialising settings")?);
        let fresh: toml_edit::DocumentMut = toml::to_string(&wrap).context("serialising settings")?.parse().context("serialising settings")?;
        if let Some(item) = fresh.as_table().get(leaf) {
            table.insert(leaf, item.clone());
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, path)?;
    Config::load(path)
}

pub fn default_config_toml() -> String {
    // Hand-written so users get documentation inline.
    r#"# stenly configuration. Edits here are picked up live; the settings window
# writes to this same file (comments are preserved).

[app]
onboarded = false
show_next_in_menubar = true
launch_at_login = false

[me]
# You, so meeting apps' tiles and invites can tell you from everyone else.
name = ""
emails = []
# other names you go by, e.g. ["Alex", "Alexander Rowan"]
aliases = []
# what your lines and action items are labelled in notes ("" = your first name; "Me" = the old label)
label = ""
# also use your calendar account and the name your meeting app signs you in as
auto = true

[output]
# Markdown notes go here: point this at a folder inside your Obsidian vault.
notes_dir = "~/Documents/stenly/Meetings"
# Raw recordings (mic.wav + system.wav per meeting).
audio_dir = "~/Documents/stenly/Recordings"
# Placeholders: {date} {time} {title} {slug}
filename = "{date} {title}.md"
# Append a "- [[note]]" link to this daily note ("" = off). e.g. "~/vault/Daily/{date}.md"
daily_note = ""
# Days to keep audio after transcription. 0 = delete right away, -1 = keep forever.
keep_audio_days = 30
include_transcript = true

[asr]
# English-only NVIDIA Parakeet TDT 0.6B v2 (int8 ONNX, ~650 MB, runs locally).
model = "parakeet-tdt-0.6b-v2-int8"
# "" = platform cache dir
models_dir = ""
# onnxruntime execution provider: cpu | coreml | cuda
provider = "cpu"
threads = 4
# Personal jargon: names and terms to always spell this way (also used by the summary).
vocabulary = []
# In-person meetings on one mic: label voices "Speaker 1/2/…" (local, 40 MB model)
separate_speakers = true
# Name speakers (meeting app's speaker highlight, remembered voices, invitees, summary model)
speaker_names = true
# keep raw accessibility snapshots of the meeting app in the session folder (debugging)
save_meeting_app_snapshots = false

[llm]
# Who writes summaries: "ollama" (on this Mac), "api" (a hosted model, see [llm.api]),
# "cli" (an installed Codex or Claude Code you're signed in to, see [llm.cli]) or
# "chatgpt" (your ChatGPT plan, after Sign in with ChatGPT; see [llm.chatgpt]).
# Pick a model in Settings → Summaries (Ollama models are downloaded for you).
enabled = true
provider = "ollama"
endpoint = "http://localhost:11434/v1"
model = "qwen3.5:9b"
# env var with an API key, if your endpoint needs one
api_key_env = ""
# optional custom prompt; {transcript} and {title} are substituted
prompt_file = ""
# "none" turns off thinking on reasoning models (fast); "low"/"medium"/"high" or "" for default
reasoning_effort = "none"
timeout_secs = 600
# Ollama: unload the model right after summarizing so it doesn't sit in RAM ("" = server default)
keep_alive = "0"
# write notes in this language, e.g. "English" or "Japanese" ("" = the meeting's language)
language = ""

[llm.api]
# Any OpenAI-compatible service, used when provider = "api". Examples:
#   OpenAI      https://api.openai.com/v1        gpt-5-mini
#   Anthropic   https://api.anthropic.com/v1     claude-sonnet-4-6
#   OpenRouter  https://openrouter.ai/api/v1     anthropic/claude-sonnet-4.6
#   Groq        https://api.groq.com/openai/v1   llama-3.3-70b-versatile
# Your transcript is sent to this service.
endpoint = ""
model = ""
# Key: set it in the app (kept in the macOS Keychain), here, or via an env var.
api_key = ""
api_key_env = ""
reasoning_effort = ""

[llm.cli]
# Used when provider = "cli": Stenly runs your own `codex exec` or `claude -p` (no tools,
# read-only, in an empty temporary folder). Your transcript is sent to OpenAI / Anthropic
# through that tool and billed to the account it is signed in to; Stenly never sees a token.
# "codex" or "claude" ("" = whichever is installed)
tool = ""
# full path if Stenly can't find it ("" = search PATH, ~/.local/bin, Homebrew, npm/bun/volta/fnm bins)
path = ""
# model name for the tool ("" = the tool's own default)
model = ""

[llm.chatgpt]
# Used when provider = "chatgpt". Sign in from Settings → Summaries; your ChatGPT
# plan pays and your transcript is sent to OpenAI under your account.
model = "gpt-6.1-sol"
# low | medium | high ("" = the model's default)
reasoning_effort = "low"

[templates]
# Note templates and recipes are Markdown files in templates/ and recipes/ next to
# this file. Pick a template from the meeting's title/attendees (each template's
# `match:` keywords); otherwise use `default`.
auto = true
default = "general"

[calendar]
# EventKit reads accounts already in Calendar.app on macOS; ICS is the Linux default.
source = "eventkit"
# Optional advanced connection via Kody. Empty = not connected.
kody_url = ""
kody_integration = "google"
# calendars to watch (ids); empty = primary
calendar_ids = []
# extra ICS feeds, e.g. Google → Settings → (calendar) → "Secret address in iCal format"
ics_urls = []
remind_before_secs = 60
# only events that look like meetings: a Zoom / Meet / Teams / ... link, or other guests invited
require_meeting_link = true
ignore_titles = []
# start recording without asking for events whose title contains one of these
auto_record_titles = []
refresh_mins = 10

[detect]
# prompt when a meeting app grabs the mic and there's no calendar event
ad_hoc_calls = true
auto_stop_silence_secs = 180
auto_stop_mic_released_secs = 1

[audio]
# Microphones in order of preference (name or id); the first connected one is
# used for meetings and dictation. Empty = system default.
mic_priority = []

[dictation]
# Press a key anywhere, speak, and the text is typed where your cursor is.
enabled = true
# right_command | right_option | left_option | right_control | fn | right_shift, or a combo like "ctrl+shift+space"
shortcut = "right_command"
# toggle | push_to_talk | hybrid (tap = hands-free, hold = push-to-talk)
mode = "hybrid"
shortcut2 = ""
mode2 = "hybrid"
paste_last_shortcut = "alt+cmd+v"
restore_clipboard = true
restore_clipboard_secs = 1.5
auto_send = false
sounds = true
mute_system_audio = false
remove_fillers = true
# rewrite with the summary model before pasting
ai_cleanup = false
ai_prompt = "clean"
replacements = []
history_limit = 200
# Speech worker: 0 keeps dictation ready; 300/600/1800 unload after idle seconds.
model_idle_secs = 0

[context]
# Before a meeting is summarized, look up its people, companies and open threads in
# your past meetings, notes vault, Notion and your own MCP servers (Settings → Context).
enabled = false
before_mins = 10
# the model that does the lookups: "summary" (same as summaries), "api" ([llm.api]) or "custom"
provider = "summary"
max_turns = 12
budget_secs = 120
past_meetings = true
vault = true
notion = false
# [[context.servers]]
# id = "linear"
# url = "https://mcp.linear.app/mcp"
# allow = []            # empty = every read-only tool

[mcp]
# Let AI tools (Claude Code, Codex, Cursor, pi, ...) read your meetings over MCP at
# http://127.0.0.1:4791/mcp while Stenly runs (localhost only; restart to apply).
# Register with every harness: `stenly-mcp install` (stdio; works even when the app is closed).
http = true
port = 4791
# Keep MCP read-only unless you explicitly allow actions such as share_meeting.
write_tools = false
"#
    .replace("source = \"eventkit\"", if cfg!(target_os = "macos") { "source = \"eventkit\"" } else { "source = \"ics\"" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_context_servers_only() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.toml");
        std::fs::write(&p, "# mine\n[context]\nenabled = true # keep\n[mcp]\nport = 1\n").unwrap();
        let s = McpServerConfig { id: "linear".into(), enabled: true, url: "https://mcp.linear.app/mcp".into(), ..Default::default() };
        let cfg = replace_tables(&p, &["context", "servers"], &[s.clone()]).unwrap();
        assert_eq!(cfg.context.servers, vec![s]);
        assert!(cfg.context.enabled && cfg.mcp.port == 1);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# mine") && text.contains("# keep"), "{text}");
        let cfg = replace_tables::<McpServerConfig>(&p, &["context", "servers"], &[]).unwrap();
        assert!(cfg.context.servers.is_empty() && cfg.context.enabled);
    }

    #[test]
    fn default_file_parses_to_defaults() {
        let parsed: Config = toml::from_str(&default_config_toml()).unwrap();
        let mut expected = Config::default();
        // The hand-written first-run file chooses the host's source; the Rust
        // struct default intentionally preserves old configs as Legacy.
        expected.calendar.source = parsed.calendar.source.clone();
        // meeting_apps isn't spelled out in the file; it falls back to the default.
        expected.detect.meeting_apps = parsed.detect.meeting_apps.clone();
        assert_eq!(parsed, expected);
    }

    #[test]
    fn chatgpt_target_is_additive() {
        let cfg: Config = toml::from_str("[llm]\nprovider = \"chatgpt\"\n").unwrap();
        let t = cfg.llm.target(|| Some("sk-should-not-be-used".into()));
        assert_eq!(t.backend, Backend::ChatGpt);
        assert_eq!(t.model, crate::chatgpt::DEFAULT_MODEL);
        assert_eq!(t.api_key, None);
        assert!(!t.local);
        // the context agent's "summary" target follows it
        assert_eq!(cfg.context.target(&cfg.llm, || None).backend, Backend::ChatGpt);
        // a custom model, and the other providers untouched
        let cfg: Config = toml::from_str("[llm]\nprovider = \"chatgpt\"\n[llm.chatgpt]\nmodel = \"gpt-6-luna\"\n").unwrap();
        assert_eq!(cfg.llm.target(|| None).model, "gpt-6-luna");
        let api: Config = toml::from_str("[llm]\nprovider = \"api\"\n[llm.api]\nendpoint = \"https://x/v1\"\nmodel = \"m\"\n").unwrap();
        assert_eq!(api.llm.target(|| Some("k".into())).backend, Backend::OpenAi);
        assert_eq!(Config::default().llm.target(|| None).backend, Backend::OpenAi);
        assert_eq!(api.context.target(&api.llm, || None).backend, Backend::OpenAi);
    }

    #[test]
    fn old_calendar_config_keeps_legacy_sources() {
        let cfg: Config = toml::from_str("[calendar]\nkody_url = \"https://kody.example.com\"\nics_urls = [\"https://example.com/calendar.ics\"]\n").unwrap();
        assert_eq!(cfg.calendar.source, CalendarSourceKind::Legacy);
        assert!(cfg.calendar.source.uses_kody());
        assert!(cfg.calendar.source.uses_ics());
        assert!(!cfg.calendar.source.uses_eventkit());
    }

    #[test]
    fn dictation_memory_policy_is_backward_compatible() {
        let defaults: Config = toml::from_str("").unwrap();
        assert_eq!(defaults.dictation.model_idle_secs, 0);
        let saving: Config = toml::from_str("[dictation]\nmodel_idle_secs = 300").unwrap();
        assert_eq!(saving.dictation.model_idle_secs, 300);
        assert!(toml::from_str::<Config>("[dictation]\nmodel_idle_secs = -1").is_err());
    }

    #[test]
    fn cli_provider_resolves_to_a_cli_target() {
        let cfg: Config = toml::from_str("[llm]\nprovider = \"cli\"\n[llm.cli]\ntool = \"claude\"\nmodel = \"sonnet\"\npath = \"/x/claude\"").unwrap();
        assert!(cfg.llm.uses_cli() && !cfg.llm.uses_api() && !cfg.llm.uses_ollama());
        let t = cfg.llm.target(|| None);
        let c = t.cli.expect("cli target");
        assert_eq!((c.tool, c.model.as_str(), c.path.as_str()), (crate::cli_llm::Tool::Claude, "sonnet", "/x/claude"));
        assert!(!t.local && t.api_key.is_none());
        // the other providers are untouched
        assert!(Config::default().llm.target(|| None).cli.is_none());
        let api: Config = toml::from_str("[llm]\nprovider = \"api\"").unwrap();
        assert!(api.llm.target(|| Some("k".into())).cli.is_none());
        // "Same as summaries" for the context agent inherits it; "api"/"custom" don't
        assert!(cfg.context.target(&cfg.llm, || None).cli.is_some());
        let mut ctx = cfg.clone();
        ctx.context.provider = "api".into();
        assert!(ctx.context.target(&ctx.llm, || None).cli.is_none());
        // the commented default file parses and keeps Ollama
        let d: Config = toml::from_str(&default_config_toml()).unwrap();
        assert!(d.llm.uses_ollama() && d.llm.cli == CliLlmConfig::default());
    }

    #[test]
    fn set_value_preserves_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, default_config_toml()).unwrap();
        // SAFETY: tests in this module don't run config_path concurrently with other env readers
        unsafe { std::env::set_var("STENLY_CONFIG", &path) };
        let cfg = Config::set_value("output.notes_dir", "~/vault/Meetings".into()).unwrap();
        assert_eq!(cfg.output.notes_dir, "~/vault/Meetings");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# Markdown notes go here"));
        assert!(text.contains(r#"notes_dir = "~/vault/Meetings""#));
        let cfg = Config::set_value("calendar.ics_urls", toml_edit::Value::Array(
            ["https://x/basic.ics"].into_iter().collect(),
        ))
        .unwrap();
        assert_eq!(cfg.calendar.ics_urls, vec!["https://x/basic.ics"]);
    }
}
