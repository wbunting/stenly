//! Platform-agnostic core of stenly.
//!
//! Nothing in this crate touches an OS API. Everything platform-specific
//! (audio capture, mic-activity detection, tray, window tweaks) is described by
//! the traits in [`platform`] and implemented in `stenly-platform`. Speech
//! recognition is behind [`transcript::Transcriber`] and lives in `stenly-asr`.

pub mod audio;
pub mod calendar;
pub mod chatgpt;
pub mod cli_llm;
pub mod config;
pub mod context;
pub mod daemon;
pub mod destinations;
pub mod dictation;
pub mod ics;
pub mod identity;
pub mod kody;
pub mod library;
pub mod oauth;
pub mod obsidian;
pub mod ollama;
pub mod meeting_url;
pub mod model_scores;
pub mod note;
pub mod platform;
pub mod recipe;
pub mod scheduler;
pub mod secrets;
pub mod session;
pub mod share;
pub mod speakers;
pub mod summary;
pub mod transcript;
pub mod util;
pub mod update;
pub mod vocabulary;
pub mod voices;

pub use config::Config;
