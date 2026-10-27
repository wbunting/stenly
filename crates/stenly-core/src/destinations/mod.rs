//! Destinations: where a finished meeting note can be sent.
//!
//! Every destination says up front whether it is [`Locality::Local`] (the note
//! stays on this machine) or [`Locality::Cloud`] (it goes to a third-party
//! service), and the UI shows that label wherever the destination appears.
//!
//! Built in:
//! - the notes folder from `[output]` (Markdown / Obsidian, the original behaviour)
//! - "Copy as Markdown"
//!
//! Configured in `config.toml` as `[[destinations]]` tables:
//! - `folder`: another Markdown folder (local)
//! - `notion`: a page in a Notion database (cloud)
//! - `slack`: a Slack incoming webhook (cloud)
//! - `webhook`: a JSON POST to any URL (cloud)
//!
//! Credentials (Notion token, Slack/webhook URLs, signing secrets) never go in
//! the config file: they live in the Keychain under the destination's id
//! (see [`crate::secrets::destination_secret`]).
//!
//! Each send is recorded in the session's `meta.json` ([`SendRecord`]), which
//! makes sending idempotent: re-sending a meeting updates the Notion page it
//! created instead of creating another one.
//!
//! Adding a CRM (HubSpot, Attio, …) means one more [`Kind`], its options
//! struct, and a [`Destination`] impl; [`people`] already has the attendee /
//! company-domain helpers such an integration needs.

pub mod config;
mod folder;
pub(crate) mod http;
pub mod markdown;
pub mod notion;
pub mod people;
pub mod rules;
mod slack;
mod share;
mod webhook;

pub use config::{AutoRule, DestinationConfig, FolderOpts, Kind, MatchBy, NotionOpts, RelationMatch, Secret, ShareOpts, WebhookOpts};

use crate::config::Config;
use crate::session::SessionMeta;
use crate::summary::Summary;
use crate::transcript::Transcript;
use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Id of the built-in notes folder (`[output] notes_dir`).
pub const NOTES_FOLDER_ID: &str = "notes-folder";
/// Id of the built-in "Copy as Markdown".
pub const CLIPBOARD_ID: &str = "clipboard";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Locality {
    /// The note stays on this machine.
    Local,
    /// The note is sent to a third-party service.
    Cloud,
}

impl Locality {
    pub fn is_local(self) -> bool {
        self == Locality::Local
    }
}

/// The caption shown next to a destination everywhere: "On this Mac" or
/// "Sends to Notion".
pub fn privacy_caption(locality: Locality, service: &str) -> String {
    match locality {
        Locality::Local if cfg!(target_os = "macos") => "On this Mac".into(),
        Locality::Local => "On this computer".into(),
        Locality::Cloud => format!("Sends to {service}"),
    }
}

/// Everything a destination may need about one meeting.
#[derive(Debug, Clone)]
pub struct Payload {
    pub dir: PathBuf,
    pub meta: SessionMeta,
    pub transcript: Transcript,
    pub summary: Option<Summary>,
}

impl Payload {
    pub fn load(dir: &Path) -> Result<Self> {
        let meta = SessionMeta::load(dir).with_context(|| format!("no meeting at {}", dir.display()))?;
        let (transcript, summary) = crate::session::load_detail(dir);
        Ok(Self { dir: dir.to_path_buf(), meta, transcript: transcript.unwrap_or_default(), summary })
    }

    /// Stable id for this meeting (its folder name), sent to webhooks.
    pub fn session_id(&self) -> String {
        self.dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    }

    /// Note body as Markdown without frontmatter, title or transcript: what
    /// cloud destinations get as the page / message body.
    pub fn body_markdown(&self) -> String {
        let md = crate::note::render(&self.meta, &self.transcript, self.summary.as_ref(), None, false);
        strip_note_chrome(&md)
    }

    /// Full note including frontmatter (what the notes folder holds).
    pub fn full_markdown(&self, cfg: &Config) -> String {
        let keep_audio = cfg.output.keep_audio_days != 0;
        crate::note::render(&self.meta, &self.transcript, self.summary.as_ref(), keep_audio.then_some(self.dir.as_path()), cfg.output.include_transcript)
    }

    /// Plain "[00:01] You: hello" transcript.
    pub fn transcript_text(&self) -> String {
        self.transcript
            .segments
            .iter()
            .map(|s| format!("[{}] {}: {}", crate::util::fmt_timestamp(s.start), speaker_name(s), s.text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub(crate) fn speaker_name(s: &crate::transcript::Segment) -> String {
    if let Some(n) = s.name.as_deref().filter(|n| !n.trim().is_empty() && *n != crate::voices::SELF_NAME) {
        return n.to_string();
    }
    match s.speaker {
        Some(n) => format!("Speaker {}", n + 1),
        None if s.channel == crate::platform::Channel::Mic => "You".into(),
        None => "Them".into(),
    }
}

/// Drop YAML frontmatter and the leading `# Title` line.
fn strip_note_chrome(md: &str) -> String {
    let mut rest = md;
    if let Some(r) = rest.strip_prefix("---\n")
        && let Some(end) = r.find("\n---\n")
    {
        rest = &r[end + 5..];
    }
    let rest = rest.trim_start();
    let rest = match rest.strip_prefix("# ") {
        Some(r) => r.split_once('\n').map(|x| x.1).unwrap_or(""),
        None => rest,
    };
    rest.trim().to_string()
}

/// What a successful send produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Delivered {
    /// Where to see it (Notion page URL, file path as file:// …).
    pub url: Option<String>,
    /// The remote object to update next time (Notion page id, file path).
    pub remote_id: Option<String>,
    /// Human-readable extra ("Linked to Acme Corp").
    pub detail: Option<String>,
    /// For "Copy as Markdown": the caller puts this on the clipboard.
    pub clipboard: Option<String>,
}

/// One send attempt, kept in `meta.json` under `sends`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendRecord {
    /// Destination id.
    pub destination: String,
    pub at: DateTime<Local>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Sent by a rule rather than by hand.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto: bool,
}

/// The latest attempt for `id`, if any.
pub fn last_send<'a>(meta: &'a SessionMeta, id: &str) -> Option<&'a SendRecord> {
    meta.sends.iter().filter(|s| s.destination == id).max_by_key(|s| s.at)
}

/// The remote object a previous successful send created (to update in place).
pub fn last_remote_id<'a>(meta: &'a SessionMeta, id: &str) -> Option<&'a str> {
    meta.sends.iter().filter(|s| s.destination == id && s.ok).max_by_key(|s| s.at).and_then(|s| s.remote_id.as_deref())
}

/// "Sent 10:42 AM" / "Failed Oct 21, 9:05 AM" for a send record.
pub fn describe_send(r: &SendRecord, now: DateTime<Local>) -> String {
    let when = if r.at.date_naive() == now.date_naive() {
        r.at.format("%-I:%M %p").to_string()
    } else if r.at.format("%Y").to_string() == now.format("%Y").to_string() {
        r.at.format("%b %-d, %-I:%M %p").to_string()
    } else {
        r.at.format("%b %-d, %Y").to_string()
    };
    if r.ok { format!("Sent {when}") } else { format!("Failed {when}") }
}

/// History kept per destination per meeting.
const HISTORY_PER_DESTINATION: usize = 10;

/// Merge two send histories (append-only, so a union), oldest first, capped
/// per destination. Used when saving `meta.json` so a send recorded while the
/// pipeline was rewriting the meeting isn't lost.
pub fn merge_sends(a: &[SendRecord], b: &[SendRecord]) -> Vec<SendRecord> {
    let mut out: Vec<SendRecord> = a.to_vec();
    for r in b {
        if !out.iter().any(|x| x.destination == r.destination && x.at == r.at) {
            out.push(r.clone());
        }
    }
    out.sort_by_key(|r| r.at);
    let mut kept: Vec<SendRecord> = Vec::with_capacity(out.len());
    for r in out.into_iter().rev() {
        if kept.iter().filter(|k| k.destination == r.destination).count() < HISTORY_PER_DESTINATION {
            kept.push(r);
        }
    }
    kept.reverse();
    kept
}

/// A place notes can go.
pub trait Destination: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn kind(&self) -> Kind;
    fn locality(&self) -> Locality {
        self.kind().locality()
    }
    /// "Notion", "Slack", "your webhook", or the folder name.
    fn service(&self) -> String {
        self.kind().service().to_string()
    }
    /// "On this Mac" / "Sends to Notion".
    fn caption(&self) -> String {
        privacy_caption(self.locality(), &self.service())
    }
    /// A re-send replaces what the last send created (Notion page, file)
    /// rather than posting again (Slack, webhook).
    fn updates_in_place(&self) -> bool;
    /// Whether it's ready to use (credentials present, required fields set).
    fn ready(&self) -> Result<()>;
    /// Check credentials / reachability without sending a meeting.
    fn test(&self) -> Result<String>;
    /// Send one meeting. `previous` is the remote id from the last successful
    /// send (to update instead of duplicating).
    fn send(&self, payload: &Payload, previous: Option<&str>) -> Result<Delivered>;
    /// The rule for sending automatically after each meeting.
    fn auto(&self) -> &AutoRule;
    /// Records history in `meta.json` (not the clipboard).
    fn records(&self) -> bool {
        true
    }
}

/// Every destination available: built-ins first, then configured ones.
pub fn all(cfg: &Config) -> Vec<Box<dyn Destination>> {
    let mut out: Vec<Box<dyn Destination>> = vec![Box::new(folder::NotesFolder::new(cfg)), Box::new(Clipboard::default())];
    for d in &cfg.destinations {
        out.push(build(cfg, d));
    }
    out
}

/// Turn one `[[destinations]]` entry into a destination.
pub fn build(cfg: &Config, d: &DestinationConfig) -> Box<dyn Destination> {
    match d.kind {
        Kind::Folder => Box::new(folder::Folder::new(cfg, d.clone())),
        Kind::Notion => Box::new(notion::NotionDestination::new(d.clone())),
        Kind::Slack => Box::new(slack::Slack::new(d.clone())),
        Kind::Webhook => Box::new(webhook::Webhook::new(d.clone())),
        Kind::Share => Box::new(share::ShareDestination::new(d.clone())),
    }
}

/// Look a destination up by id or (case-insensitive) name.
/// The configured share server, or the hosted default when none was added in
/// Settings. This makes the primary Share button useful before setup.
pub fn share_config(cfg: &Config) -> DestinationConfig {
    cfg.destinations.iter().find(|d| d.kind == Kind::Share).cloned().unwrap_or_else(|| DestinationConfig::new(Kind::Share, "Stenly share link"))
}

pub fn find(cfg: &Config, key: &str) -> Option<Box<dyn Destination>> {
    let key = key.trim();
    all(cfg).into_iter().find(|d| d.id() == key || d.name().eq_ignore_ascii_case(key) || crate::util::slug(d.name()) == crate::util::slug(key))
}

/// The credential for a configured destination: `secret_env` (handy for
/// scripts and tests) or the Keychain item named after its id.
pub fn secret_for(d: &DestinationConfig, suffix: &str) -> Option<String> {
    let typed = match suffix {
        "" => d.secret_override.as_ref(),
        "signing" => d.signing_override.as_ref(),
        _ => None,
    };
    if let Some(s) = typed.map(|s| s.0.trim()).filter(|s| !s.is_empty()) {
        return Some(s.to_string());
    }
    let env_name = if suffix.is_empty() { d.secret_env.clone() } else { String::new() };
    if !env_name.trim().is_empty()
        && let Ok(v) = std::env::var(env_name.trim())
        && !v.trim().is_empty()
    {
        return Some(v.trim().to_string());
    }
    crate::secrets::destination_secret(&secret_account(&d.id, suffix))
}

/// Keychain account for a destination's secret (`id` or `id.suffix`).
pub fn secret_account(id: &str, suffix: &str) -> String {
    if suffix.is_empty() { id.to_string() } else { format!("{id}.{suffix}") }
}

/// Built-in: put the note on the clipboard (the UI/CLI does the copying).
#[derive(Default)]
struct Clipboard {
    auto: AutoRule,
}

impl Destination for Clipboard {
    fn id(&self) -> &str {
        CLIPBOARD_ID
    }
    fn name(&self) -> &str {
        "Copy as Markdown"
    }
    fn kind(&self) -> Kind {
        Kind::Folder
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }
    fn service(&self) -> String {
        "the clipboard".into()
    }
    fn updates_in_place(&self) -> bool {
        true
    }
    fn ready(&self) -> Result<()> {
        Ok(())
    }
    fn test(&self) -> Result<String> {
        Ok("Ready".into())
    }
    fn send(&self, p: &Payload, _previous: Option<&str>) -> Result<Delivered> {
        let md = format!("# {}\n\n{}\n", p.meta.title, p.body_markdown());
        Ok(Delivered { clipboard: Some(md), ..Default::default() })
    }
    fn auto(&self) -> &AutoRule {
        &self.auto
    }
    fn records(&self) -> bool {
        false
    }
}

/// Serialises read-modify-write of `meta.json` within this process.
static META_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Send one meeting to one destination and record the outcome in its
/// `meta.json`. Returns the record (also on failure: `ok = false`).
pub fn send_session(dest: &dyn Destination, dir: &Path, auto: bool) -> Result<(SendRecord, Delivered)> {
    let payload = Payload::load(dir)?;
    let previous = last_remote_id(&payload.meta, dest.id()).map(str::to_string);
    let result = dest.ready().and_then(|_| dest.send(&payload, previous.as_deref()));
    let record = match &result {
        Ok(d) => SendRecord {
            destination: dest.id().to_string(),
            at: Local::now(),
            ok: true,
            url: d.url.clone(),
            remote_id: d.remote_id.clone().or(previous.clone()),
            detail: d.detail.clone(),
            error: None,
            auto,
        },
        Err(e) => SendRecord {
            destination: dest.id().to_string(),
            at: Local::now(),
            ok: false,
            url: None,
            remote_id: None,
            detail: None,
            error: Some(format!("{e:#}")),
            auto,
        },
    };
    if dest.records() {
        let _g = META_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut meta = SessionMeta::load(dir)?;
        meta.sends.push(record.clone());
        meta.save(dir)?;
    }
    match result {
        Ok(d) => {
            tracing::info!(destination = dest.id(), auto, "note sent");
            Ok((record, d))
        }
        Err(e) => {
            tracing::warn!(destination = dest.id(), auto, error = %format!("{e:#}"), "send failed");
            Err(e)
        }
    }
}

/// After a meeting is processed: send it everywhere a rule says to. Returns
/// (destination name, outcome) for each attempt.
pub fn auto_send(cfg: &Config, dir: &Path) -> Vec<(String, Result<SendRecord>)> {
    let Ok(meta) = SessionMeta::load(dir) else { return vec![] };
    let mut out = Vec::new();
    for d in all(cfg) {
        if !rules::should_auto_send(d.as_ref(), &meta) {
            continue;
        }
        let r = send_session(d.as_ref(), dir, true).map(|(r, _)| r);
        out.push((d.name().to_string(), r));
    }
    out
}

/// A new destination id from its name, unique among `existing`.
pub fn new_id(name: &str, existing: &[DestinationConfig]) -> String {
    let base = match crate::util::slug(name) {
        s if s.is_empty() => "destination".to_string(),
        s => s,
    };
    let taken = |id: &str| id == NOTES_FOLDER_ID || id == CLIPBOARD_ID || existing.iter().any(|d| d.id == id);
    if !taken(&base) {
        return base;
    }
    (2..).map(|n| format!("{base}-{n}")).find(|id| !taken(id)).unwrap_or(base)
}

/// Replace the `[[destinations]]` tables in config.toml (everything else in
/// the file is kept as written) and return the reloaded config.
pub fn save_all(list: &[DestinationConfig]) -> Result<Config> {
    save_all_at(&crate::config::config_path(), list)
}

fn save_all_at(path: &Path, list: &[DestinationConfig]) -> Result<Config> {
    crate::config::replace_tables(path, &["destinations"], list)
}

/// Add or replace (by id) one destination.
pub fn upsert(cfg: &Config, d: DestinationConfig) -> Result<Config> {
    let mut list = cfg.destinations.clone();
    match list.iter_mut().find(|x| x.id == d.id) {
        Some(x) => *x = d,
        None => list.push(d),
    }
    save_all(&list)
}

/// Remove a destination and its stored credentials.
pub fn remove(cfg: &Config, id: &str) -> Result<Config> {
    let list: Vec<DestinationConfig> = cfg.destinations.iter().filter(|d| d.id != id).cloned().collect();
    for suffix in ["", "signing"] {
        let _ = crate::secrets::set_destination_secret(&secret_account(id, suffix), "");
    }
    save_all(&list)
}

#[cfg(test)]
pub(crate) mod testutil;

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(dest: &str, mins: i64, ok: bool) -> SendRecord {
        use chrono::TimeZone;
        SendRecord {
            destination: dest.into(),
            at: Local.with_ymd_and_hms(2026, 10, 20, 10, 0, 0).unwrap() + chrono::Duration::minutes(mins),
            ok,
            url: None,
            remote_id: ok.then(|| format!("page-{mins}")),
            detail: None,
            error: (!ok).then(|| "boom".into()),
            auto: false,
        }
    }

    #[test]
    fn merges_histories_without_duplicates() {
        let a = vec![rec("n", 1, true), rec("s", 2, false)];
        let b = vec![rec("n", 1, true), rec("n", 3, false)];
        let m = merge_sends(&a, &b);
        assert_eq!(m.len(), 3);
        assert!(m.windows(2).all(|w| w[0].at <= w[1].at));
    }

    #[test]
    fn caps_history_per_destination() {
        let a: Vec<SendRecord> = (0..25).map(|i| rec("n", i, true)).chain([rec("s", 0, true)]).collect();
        let m = merge_sends(&a, &[]);
        assert_eq!(m.iter().filter(|r| r.destination == "n").count(), HISTORY_PER_DESTINATION);
        assert_eq!(m.iter().filter(|r| r.destination == "s").count(), 1);
        // newest kept
        assert_eq!(m.last().unwrap().at, rec("n", 24, true).at);
    }

    #[test]
    fn last_remote_id_skips_failures() {
        let mut meta = testutil::meta("Sync");
        meta.sends = vec![rec("n", 1, true), rec("n", 2, false)];
        assert_eq!(last_remote_id(&meta, "n"), Some("page-1"));
        assert!(!last_send(&meta, "n").unwrap().ok);
        assert_eq!(last_remote_id(&meta, "x"), None);
    }

    #[test]
    fn ids_are_unique_slugs() {
        let mut d = DestinationConfig::new(Kind::Notion, "Notion CRM");
        d.id = "notion-crm".into();
        assert_eq!(new_id("Notion CRM", &[d.clone()]), "notion-crm-2");
        assert_eq!(new_id("Slack #sales", &[d]), "slack-sales");
        assert_eq!(new_id("!!!", &[]), "destination");
        assert_eq!(new_id("Clipboard", &[]), "clipboard-2");
    }

    #[test]
    fn body_markdown_has_no_frontmatter_or_transcript() {
        let p = testutil::payload();
        let md = p.body_markdown();
        assert!(!md.contains("---"));
        assert!(!md.starts_with("# "));
        assert!(md.contains("## Summary"));
        assert!(!md.contains("Transcript"));
    }

    #[test]
    fn saving_keeps_the_rest_of_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, crate::config::default_config_toml()).unwrap();
        let mut n = DestinationConfig::new(Kind::Notion, "Notion CRM");
        n.notion.database_id = "abc".into();
        let s = DestinationConfig::new(Kind::Slack, "Slack");
        let cfg = save_all_at(&path, &[n.clone(), s]).unwrap();
        assert_eq!(cfg.destinations.len(), 2);
        assert_eq!(cfg.destinations[0], n);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# Markdown notes go here"));
        assert!(text.contains("[[destinations]]"));
        let cfg = save_all_at(&path, &[]).unwrap();
        assert!(cfg.destinations.is_empty());
        assert!(!std::fs::read_to_string(&path).unwrap().contains("destinations"));
    }

    #[test]
    fn describes_sends() {
        let r = rec("n", 42, true);
        assert_eq!(describe_send(&r, r.at + chrono::Duration::hours(1)), "Sent 10:42 AM");
        assert_eq!(describe_send(&rec("n", 0, false), r.at + chrono::Duration::days(2)), "Failed Oct 20, 10:00 AM");
        assert_eq!(describe_send(&r, r.at + chrono::Duration::days(400)), "Sent Oct 20, 2026");
    }

    #[test]
    fn typed_secret_wins() {
        let mut d = DestinationConfig::new(Kind::Slack, "S");
        d.secret_override = Some(Secret("  https://hooks.slack.com/x ".into()));
        assert_eq!(secret_for(&d, "").as_deref(), Some("https://hooks.slack.com/x"));
        assert_eq!(secret_for(&d, "signing"), crate::secrets::destination_secret("s.signing"));
    }

    #[test]
    fn captions() {
        assert!(privacy_caption(Locality::Local, "x").starts_with("On this"));
        assert_eq!(privacy_caption(Locality::Cloud, "Notion"), "Sends to Notion");
    }

    #[test]
    fn send_records_history_and_reuses_remote_id() {
        let dir = tempfile::tempdir().unwrap();
        testutil::write_session(dir.path());
        struct Fake(std::sync::Mutex<Vec<Option<String>>>, AutoRule);
        impl Destination for Fake {
            fn id(&self) -> &str {
                "fake"
            }
            fn name(&self) -> &str {
                "Fake"
            }
            fn kind(&self) -> Kind {
                Kind::Notion
            }
            fn updates_in_place(&self) -> bool {
                true
            }
            fn ready(&self) -> Result<()> {
                Ok(())
            }
            fn test(&self) -> Result<String> {
                Ok(String::new())
            }
            fn send(&self, _p: &Payload, previous: Option<&str>) -> Result<Delivered> {
                self.0.lock().unwrap().push(previous.map(str::to_string));
                Ok(Delivered { remote_id: Some(previous.unwrap_or("new-page").to_string()), url: Some("https://x/new-page".into()), ..Default::default() })
            }
            fn auto(&self) -> &AutoRule {
                &self.1
            }
        }
        let f = Fake(Default::default(), AutoRule::default());
        send_session(&f, dir.path(), false).unwrap();
        send_session(&f, dir.path(), false).unwrap();
        assert_eq!(*f.0.lock().unwrap(), vec![None, Some("new-page".to_string())]);
        let meta = SessionMeta::load(dir.path()).unwrap();
        assert_eq!(meta.sends.len(), 2);
        assert!(meta.sends.iter().all(|s| s.ok && s.remote_id.as_deref() == Some("new-page")));
        // the pipeline rewriting meta.json from an older copy keeps the history
        let mut stale = meta.clone();
        stale.sends.clear();
        stale.save(dir.path()).unwrap();
        assert_eq!(SessionMeta::load(dir.path()).unwrap().sends.len(), 2);
    }
}
