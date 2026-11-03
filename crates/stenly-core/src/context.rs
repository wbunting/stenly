//! Meeting context: background on a meeting's people, companies and topics,
//! from the calendar invite and (optionally) the context agent in
//! `stenly-agent`, which looks things up in past meetings, the notes vault,
//! Notion and the user's MCP servers before the meeting is summarized.
//!
//! Stored as `context.json` next to `meta.json`. The pipeline uses it to
//! spell names and terms ([`crate::vocabulary`]), to describe the people in
//! the meeting, and as a `{context}` block in the summary prompt. It is never
//! a record of the meeting itself: the prompt says the transcript wins.

use crate::calendar::CalendarEvent;
use crate::session::SessionMeta;
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FILE: &str = "context.json";

/// Longest context block put in a summary prompt (it is repeated in every
/// part of a long meeting, so it has to stay small).
pub const PROMPT_MAX_CHARS: usize = 3_500;
/// At most this many extra vocabulary terms come from the context.
const MAX_TERMS: usize = 40;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MeetingContext {
    /// What the meeting is likely about and what matters going in.
    pub brief: String,
    pub people: Vec<ContextPerson>,
    pub companies: Vec<ContextCompany>,
    /// Names and terms likely to be said, spelled exactly.
    pub terms: Vec<String>,
    /// Open questions, commitments and recent threads with these people.
    pub open_threads: Vec<String>,
    pub sources: Vec<SourceRef>,
    /// How it was gathered (absent for context taken only from the invite).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gathered: Option<Gathered>,
    /// The calendar event it was gathered for (pre-meeting cache only), so it
    /// can still be found when the event's id changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<EventStamp>,
}

/// Which calendar event a cached brief belongs to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventStamp {
    pub uid: String,
    pub title: String,
    pub start: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextPerson {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextCompany {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// What they do, the relationship and its stage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SourceRef {
    pub title: String,
    /// The tool it came from ("past_meetings_search", "notion_read", "kody__execute").
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Gathered {
    pub at: DateTime<Local>,
    pub model: String,
    pub secs: f32,
    /// Every tool call, in order.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Why it stopped early, if it did; what it found before that is kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<Stop>,
    /// Human-readable detail for the UI (the model's error, "found nothing").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    /// The model call failed (unreachable, bad key, unknown model…).
    ModelError,
    /// The turn or time budget ran out.
    Budget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Step {
    pub tool: String,
    /// Arguments, shortened for display.
    pub args: String,
    pub ok: bool,
    /// Characters returned to the model.
    pub chars: usize,
}

impl MeetingContext {
    /// A run that found nothing because it failed or ran out of time (as
    /// opposed to one that looked and found nothing): worth retrying.
    pub fn failed(&self) -> bool {
        self.is_empty() && self.gathered.as_ref().is_some_and(|g| g.stopped.is_some())
    }

    pub fn is_empty(&self) -> bool {
        self.brief.trim().is_empty() && self.people.is_empty() && self.companies.is_empty() && self.terms.is_empty() && self.open_threads.is_empty()
    }

    /// The prompt block for summaries ("" when there's nothing to say).
    pub fn prompt_block(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut b = String::from(
            "\nCONTEXT. Background on this meeting from the user's own records (the calendar invite, earlier meetings, \
documents), gathered before it was summarized. Use it to spell names, companies and products correctly, to understand \
who people are and what is at stake, and to attribute points. It is NOT a record of this meeting: put nothing in the \
notes that wasn't said in the transcript, and where the transcript and this background disagree, the transcript wins.\n",
        );
        let brief = self.brief.trim();
        if !brief.is_empty() {
            b.push_str(&format!("Background: {}\n", one_line(brief)));
        }
        if !self.people.is_empty() {
            b.push_str("People:\n");
            for p in &self.people {
                b.push_str(&format!("- {}\n", p.describe()));
            }
        }
        if !self.companies.is_empty() {
            b.push_str("Companies:\n");
            for c in &self.companies {
                let mut line = c.name.trim().to_string();
                if let Some(d) = c.domain.as_deref().filter(|d| !d.trim().is_empty()) {
                    line.push_str(&format!(" ({})", d.trim()));
                }
                if let Some(n) = c.notes.as_deref().filter(|n| !n.trim().is_empty()) {
                    line.push_str(&format!(": {}", one_line(n)));
                }
                b.push_str(&format!("- {line}\n"));
            }
        }
        if !self.open_threads.is_empty() {
            b.push_str("Open threads going in:\n");
            for t in &self.open_threads {
                b.push_str(&format!("- {}\n", one_line(t)));
            }
        }
        if b.len() > PROMPT_MAX_CHARS {
            const MORE: &str = " …\n";
            let mut cut = PROMPT_MAX_CHARS - MORE.len();
            while !b.is_char_boundary(cut) {
                cut -= 1;
            }
            b.truncate(cut);
            b.push_str(MORE);
        }
        b
    }

    /// Names and terms for the vocabulary: given terms plus company names.
    pub fn vocabulary_terms(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for t in self.terms.iter().chain(self.companies.iter().map(|c| &c.name)) {
            let t = t.trim();
            // a term is a name, not a phrase
            if t.is_empty() || t.split_whitespace().count() > 4 || out.iter().any(|o| o.eq_ignore_ascii_case(t)) {
                continue;
            }
            out.push(t.to_string());
            if out.len() >= MAX_TERMS {
                break;
            }
        }
        out
    }

    /// People's names, for the vocabulary.
    pub fn people_names(&self) -> Vec<String> {
        self.people.iter().map(|p| p.name.trim().to_string()).filter(|n| !n.is_empty()).collect()
    }

    /// The person with this name or address, if the context knows them.
    pub fn person(&self, name: &str, email: Option<&str>) -> Option<&ContextPerson> {
        let name = name.trim().to_lowercase();
        self.people.iter().find(|p| {
            let same_email = matches!((email, p.email.as_deref()), (Some(a), Some(b)) if a.eq_ignore_ascii_case(b.trim()));
            let pn = p.name.trim().to_lowercase();
            // "Sathiren" on a tile, "Sathiren Vale" in the context
            let same_name = !name.is_empty() && (pn == name || pn.split_whitespace().next() == Some(name.as_str()) || name.split_whitespace().next() == Some(pn.as_str()));
            same_email || same_name
        })
    }
}

impl ContextPerson {
    /// "Sathiren Vale, Product Lead at Example Labs: leads the pilot".
    pub fn describe(&self) -> String {
        let mut s = self.name.trim().to_string();
        match (self.role.as_deref().map(str::trim).filter(|r| !r.is_empty() && !is_unknown(r)), self.company.as_deref().map(str::trim).filter(|c| !c.is_empty())) {
            (Some(r), Some(c)) => s.push_str(&format!(", {r} at {c}")),
            (Some(r), None) => s.push_str(&format!(", {r}")),
            (None, Some(c)) => s.push_str(&format!(" ({c})")),
            (None, None) => {}
        }
        if let Some(n) = self.notes.as_deref().filter(|n| !n.trim().is_empty()) {
            s.push_str(&format!(": {}", one_line(n)));
        }
        s
    }

    /// Role and company only, for the attendee list in the prompt.
    pub fn short(&self) -> Option<String> {
        let role = self.role.as_deref().map(str::trim).filter(|r| !r.is_empty() && !is_unknown(r));
        let company = self.company.as_deref().map(str::trim).filter(|c| !c.is_empty());
        match (role, company) {
            (Some(r), Some(c)) => Some(format!("{r} at {c}")),
            (Some(r), None) => Some(r.to_string()),
            (None, Some(c)) => Some(c.to_string()),
            (None, None) => None,
        }
    }
}

/// "Unknown", "N/A", "Unknown — organizer": a model's placeholder, not a role.
fn is_unknown(s: &str) -> bool {
    let l = s.trim().to_lowercase();
    l.starts_with("unknown") || l.starts_with("n/a") || l == "none" || l == "not stated"
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Context gathered ahead of a calendar meeting, before any session exists:
/// `<data>/context/<event id>.json`. Copied into the session when recording starts.
pub fn event_cache_path(event_id: &str) -> PathBuf {
    cache_dir().join(cache_file_name(event_id))
}

fn cache_dir() -> PathBuf {
    crate::config::data_dir().join("context")
}

fn cache_file_name(event_id: &str) -> String {
    let safe: String = event_id.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).take(150).collect();
    format!("{safe}.json")
}

/// The context cached under exactly this event id. Prefer [`find_for_event`],
/// which also survives the event's id changing.
pub fn load_for_event(event_id: &str) -> Option<MeetingContext> {
    read_cached(&event_cache_path(event_id))
}

fn read_cached(path: &Path) -> Option<MeetingContext> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// How far an occurrence may move and still be "the same meeting" when its id
/// (`uid@start`) changed. Well under a day, so a daily stand-up's next
/// occurrence never borrows today's brief; a weekly series is nowhere near.
const MOVED_WITHIN_SECS: i64 = 6 * 3600;

/// The context gathered ahead of this calendar event, if any. Matches by id
/// first; failing that by the stamp saved with it, so a brief survives the
/// event being moved a few hours (its id embeds the start time) or being
/// re-issued under a new id by the calendar: same series uid and nearly the
/// same start, or the same title at exactly the same start. Another
/// occurrence of a recurring event never matches.
pub fn find_for_event(ev: &CalendarEvent) -> Option<MeetingContext> {
    find_in(&cache_dir(), ev)
}

fn find_in(dir: &Path, ev: &CalendarEvent) -> Option<MeetingContext> {
    if let Some(c) = read_cached(&dir.join(cache_file_name(&ev.id))) {
        return Some(c);
    }
    pick_moved(&read_all(dir), ev)
}

/// Every readable cached context in `dir`.
fn read_all(dir: &Path) -> Vec<MeetingContext> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "json")).filter_map(|e| read_cached(&e.path())).collect()
}

/// The stamped context that is the same meeting as `ev` under a different id.
fn pick_moved(all: &[MeetingContext], ev: &CalendarEvent) -> Option<MeetingContext> {
    let title = norm_title(&ev.title);
    let mut best: Option<(i64, &MeetingContext)> = None;
    for c in all {
        let Some(st) = &c.event else { continue };
        let delta = (st.start - ev.start).num_seconds().abs();
        let same_series = !ev.uid.is_empty() && st.uid == ev.uid && delta <= MOVED_WITHIN_SECS;
        let same_slot = delta == 0 && !title.is_empty() && norm_title(&st.title) == title;
        if (same_series || same_slot) && best.is_none_or(|(d, _)| delta < d) {
            best = Some((delta, c));
        }
    }
    best.map(|(_, c)| c.clone())
}

fn norm_title(t: &str) -> String {
    one_line(t).to_lowercase()
}

/// Cache context gathered ahead of a meeting. A failed run isn't cached, so
/// the meeting is gathered again once it has been recorded.
pub fn save_for_event(event_id: &str, ctx: &MeetingContext) -> anyhow::Result<()> {
    save_cached(&event_cache_path(event_id), ctx)
}

/// Like [`save_for_event`], stamping the event so the brief can be found again
/// if its id changes ([`find_for_event`]).
pub fn save_for_calendar_event(ev: &CalendarEvent, ctx: &MeetingContext) -> anyhow::Result<()> {
    save_in(&cache_dir(), ev, ctx)
}

fn save_in(dir: &Path, ev: &CalendarEvent, ctx: &MeetingContext) -> anyhow::Result<()> {
    let mut ctx = ctx.clone();
    ctx.event = Some(EventStamp { uid: ev.uid.clone(), title: ev.title.clone(), start: ev.start });
    save_cached(&dir.join(cache_file_name(&ev.id)), &ctx)
}

fn save_cached(p: &Path, ctx: &MeetingContext) -> anyhow::Result<()> {
    if ctx.failed() {
        return Ok(());
    }
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(ctx)?)?;
    std::fs::rename(tmp, p)?;
    Ok(())
}

/// Give a new session the context gathered for its calendar event (if any).
pub fn adopt_event_context(dir: &Path, meta: &SessionMeta) -> bool {
    let Some(ev) = &meta.event else { return false };
    if dir.join(FILE).exists() {
        return false;
    }
    match find_for_event(ev) {
        Some(mut c) if !c.failed() => {
            c.event = None;
            save(dir, &c).is_ok()
        }
        _ => false,
    }
}

// ---------------------------------------------------------------- pre-meeting brief

/// What was agreed at the last matching meeting.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LastMeeting {
    pub title: String,
    pub started: DateTime<Local>,
    pub action_items: Vec<crate::summary::ActionItem>,
}

/// Everything shown before a call: the gathered context plus the action items
/// from the last meeting with the same series or people.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PreBrief {
    #[serde(flatten)]
    pub context: MeetingContext,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_meeting: Option<LastMeeting>,
}

impl MeetingContext {
    /// Worth showing before a call: something was found and the run didn't fail.
    pub fn is_briefable(&self) -> bool {
        !self.is_empty() && !self.failed()
    }

    /// The brief in at most `max` characters of plain text, cut at a sentence
    /// or word boundary (for the "Record this meeting?" prompt).
    pub fn short_brief(&self, max: usize) -> String {
        let text = one_line(&self.brief);
        if text.chars().count() <= max {
            return text;
        }
        let cut: String = text.chars().take(max).collect();
        // prefer ending on a full sentence when most of the budget is used
        if let Some(i) = cut.rfind(['.', '!', '?']).filter(|i| *i >= max * 2 / 3) {
            return cut[..=i].to_string();
        }
        let cut = cut.rsplit_once(' ').map(|(a, _)| a).unwrap_or(&cut);
        format!("{}…", cut.trim_end_matches([',', ';', ':', ' ']))
    }
}

/// The brief for one calendar event, or `None` when nothing worth showing was
/// gathered for it. `audio_dir` is searched for the last matching meeting.
pub fn brief_for_event(ev: &CalendarEvent, audio_dir: &Path) -> Option<PreBrief> {
    briefs_for_events(std::slice::from_ref(ev), audio_dir).into_iter().next().map(|(_, b)| b)
}

/// Briefs for several events at once, keyed by event id. Sessions are only
/// read (once) if at least one event has a brief.
pub fn briefs_for_events(events: &[CalendarEvent], audio_dir: &Path) -> std::collections::HashMap<String, PreBrief> {
    briefs_in(&cache_dir(), events, audio_dir)
}

fn briefs_in(dir: &Path, events: &[CalendarEvent], audio_dir: &Path) -> std::collections::HashMap<String, PreBrief> {
    // the directory is only scanned (once) if some event isn't cached under its own id
    let mut scanned: Option<Vec<MeetingContext>> = None;
    let mut found: Vec<(&CalendarEvent, MeetingContext)> = vec![];
    for ev in events {
        let c = read_cached(&dir.join(cache_file_name(&ev.id))).or_else(|| pick_moved(scanned.get_or_insert_with(|| read_all(dir)), ev));
        if let Some(c) = c.filter(MeetingContext::is_briefable) {
            found.push((ev, c));
        }
    }
    let mut sessions: Option<Vec<(PathBuf, SessionMeta)>> = None;
    found
        .into_iter()
        .map(|(ev, mut context)| {
            context.event = None;
            let all = sessions.get_or_insert_with(|| crate::session::list_sessions(audio_dir, usize::MAX));
            (ev.id.clone(), PreBrief { context, last_meeting: last_meeting(ev, all) })
        })
        .collect()
}

/// The meeting a user is about to join: the first one that hasn't declined and
/// hasn't ended. `events` must be sorted by start.
pub fn next_meeting(events: &[CalendarEvent], now: DateTime<Utc>) -> Option<&CalendarEvent> {
    events.iter().find(|e| !e.declined && e.end > now)
}

/// The most recent finished meeting that belongs with `ev` (same calendar
/// series, else same title, else mostly the same other attendees), with its
/// action items. `None` when there is no such meeting or it had none.
pub fn last_meeting(ev: &CalendarEvent, sessions: &[(PathBuf, SessionMeta)]) -> Option<LastMeeting> {
    let title = norm_title(&ev.title);
    let mine = other_emails(ev);
    let rank = |m: &SessionMeta| -> Option<u8> {
        let past = m.event.as_ref();
        if !ev.uid.is_empty() && past.is_some_and(|p| p.uid == ev.uid) {
            return Some(0);
        }
        if !title.is_empty() && (norm_title(&m.title) == title || past.is_some_and(|p| norm_title(&p.title) == title)) {
            return Some(1);
        }
        let theirs = past.map(other_emails).unwrap_or_default();
        let shared = mine.intersection(&theirs).count();
        let union = mine.union(&theirs).count();
        (shared > 0 && shared * 2 >= union).then_some(2)
    };
    let (dir, meta) = sessions
        .iter()
        .filter(|(_, m)| m.state == crate::session::SessionState::Done && m.started.with_timezone(&Utc) < ev.start)
        .filter_map(|s| rank(&s.1).map(|r| (r, s)))
        // best kind of match first, then the most recent
        .min_by_key(|(r, (_, m))| (*r, std::cmp::Reverse(m.started)))
        .map(|(_, s)| s)?;
    let action_items: Vec<_> = crate::session::load_detail(dir).1?.action_items.into_iter().filter(|a| !a.task.trim().is_empty()).collect();
    (!action_items.is_empty()).then(|| LastMeeting { title: meta.title.clone(), started: meta.started, action_items })
}

/// Attendee addresses other than the calendar owner's, lowercased.
fn other_emails(ev: &CalendarEvent) -> std::collections::BTreeSet<String> {
    let me = ev.calendar.as_deref().unwrap_or_default();
    ev.attendees.iter().filter(|a| !a.email.eq_ignore_ascii_case(me)).map(|a| a.email.to_lowercase()).collect()
}

// ---------------------------------------------------------------- discovery hint

/// Marker that the "turn on Context" hint has been shown once.
fn hint_marker() -> PathBuf {
    crate::config::data_dir().join("hints").join("context-brief")
}

pub fn hint_seen() -> bool {
    hint_marker().exists()
}

/// Record that the hint was shown, so it never appears again.
pub fn mark_hint_seen() {
    let p = hint_marker();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, b"");
}

/// Drop cached event context older than a week.
pub fn prune_event_cache() {
    let Ok(rd) = std::fs::read_dir(crate::config::data_dir().join("context")) else { return };
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(7 * 86_400);
    for e in rd.flatten() {
        if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < cutoff) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

pub fn load(dir: &Path) -> Option<MeetingContext> {
    let bytes = std::fs::read(dir.join(FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn save(dir: &Path, ctx: &MeetingContext) -> anyhow::Result<()> {
    let tmp = dir.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(ctx)?)?;
    std::fs::rename(tmp, dir.join(FILE))?;
    Ok(())
}

/// What a session's pipeline uses: the gathered context, with the invite's
/// agenda as the brief when the agent didn't write one; or just the invite.
pub fn for_session(dir: &Path, meta: &SessionMeta) -> MeetingContext {
    let invite = from_invite(meta);
    match load(dir) {
        Some(mut c) => {
            if c.brief.trim().is_empty() {
                c.brief = invite.brief;
            }
            c
        }
        None => invite,
    }
}

/// Context from the calendar invite alone: its description, minus the
/// conferencing boilerplate (join links, dial-ins, passcodes).
pub fn from_invite(meta: &SessionMeta) -> MeetingContext {
    let brief = meta.event.as_ref().and_then(|e| e.description.as_deref()).map(clean_description).unwrap_or_default();
    MeetingContext { brief: brief.trim().to_string(), ..Default::default() }
}

/// An invite description without HTML and conferencing boilerplate, at most
/// ~1500 chars. Returns "" when nothing but boilerplate is left.
pub fn clean_description(desc: &str) -> String {
    let text = strip_html(desc);
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        let low = l.to_lowercase();
        // Teams / Zoom put their block under a rule of underscores or dashes; Google under "-::~:~::~".
        if (l.len() >= 8 && l.chars().all(|c| matches!(c, '_' | '-' | '─' | '=' | '~' | ':' | '*'))) || low.starts_with("-::~") {
            if out.iter().any(|o| !o.is_empty()) {
                break;
            }
            continue;
        }
        let boiler = [
            "microsoft teams", "join:", "join the meeting", "join zoom", "join with google meet", "join by phone", "meeting id", "passcode",
            "password:", "dial in", "dial-in", "dial by", "find a local number", "find your local number", "need help", "for organizers",
            "meeting options", "learn more", "privacy and security", "one tap mobile", "join instructions", "more phone numbers",
            "reset pin", "you have been invited", "is inviting you", "invitation from google calendar", "forwarding this invitation",
            "to stop receiving", "view all guest info", "reply for", "invitation from", "teams.microsoft.com", "zoom.us/", "meet.google.com",
            "webex.com", "aka.ms/", "tel:", "+1 ", "conference id",
        ];
        if boiler.iter().any(|b| low.contains(b)) {
            continue;
        }
        if l.is_empty() && out.last().is_some_and(|o: &String| o.is_empty()) {
            continue;
        }
        out.push(l.to_string());
    }
    let s = out.join("\n").trim().to_string();
    let s: String = s.chars().take(1500).collect();
    if s.chars().filter(|c| c.is_alphanumeric()).count() < 8 { String::new() } else { s }
}

/// Plain text from the HTML Google puts in descriptions.
fn strip_html(s: &str) -> String {
    if !s.contains('<') {
        return s.to_string();
    }
    let s = s.replace("<br>", "\n").replace("<br/>", "\n").replace("<br />", "\n").replace("</p>", "\n").replace("</li>", "\n").replace("</div>", "\n");
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_teams_boilerplate() {
        let d = "Agenda: pilot scope and pricing\n\n________________________________________________________________________________\nMicrosoft Teams meeting\nJoin: https://teams.microsoft.com/meet/4206?p=x\nMeeting ID: 420 678\nPasscode: Cf2\n________________________________\nNeed help?<https://aka.ms/JoinTeamsMeeting>";
        assert_eq!(clean_description(d), "Agenda: pilot scope and pricing");
        let only = " ________________________________________________________________________________ \nMicrosoft Teams meeting\nJoin: https://teams.microsoft.com/meet/4206\nMeeting ID: 420 678 740 942 342\nPasscode: Cf2Ft6jH\n";
        assert_eq!(clean_description(only), "");
    }

    #[test]
    fn strips_html() {
        let d = "<p>Discuss the <b>Q4</b> roadmap &amp; hiring</p><br>Bring numbers";
        assert_eq!(clean_description(d), "Discuss the Q4 roadmap & hiring\n\nBring numbers");
    }

    #[test]
    fn prompt_block_and_terms() {
        let c = MeetingContext {
            brief: "Pilot  review\nwith Example Labs.".into(),
            people: vec![ContextPerson { name: "Sathiren Vale".into(), role: Some("Product Lead".into()), company: Some("Example Labs".into()), ..Default::default() }],
            companies: vec![ContextCompany { name: "Example Labs".into(), domain: Some("guest.example.org".into()), notes: Some("prospect, evaluating a pilot".into()) }],
            terms: vec!["Vuon".into(), "vuon".into(), "a very long phrase that is not a name".into()],
            ..Default::default()
        };
        let b = c.prompt_block();
        assert!(b.contains("Background: Pilot review with Example Labs."));
        assert!(b.contains("- Sathiren Vale, Product Lead at Example Labs"));
        assert!(b.contains("- Example Labs (guest.example.org): prospect, evaluating a pilot"));
        assert!(b.contains("the transcript wins"));
        assert_eq!(c.vocabulary_terms(), vec!["Vuon".to_string(), "Example Labs".to_string()]);
        assert!(c.person("Sathiren", None).is_some());
        assert!(c.person("Someone", Some("x@y.z")).is_none());
        assert_eq!(MeetingContext::default().prompt_block(), "");
    }

    #[test]
    fn failed_runs_are_retried() {
        let g = |stopped: Option<Stop>| Gathered { at: chrono::Local::now(), model: "m".into(), secs: 1.0, steps: vec![], stopped, note: None };
        assert!(MeetingContext { gathered: Some(g(Some(Stop::ModelError))), ..Default::default() }.failed());
        assert!(MeetingContext { gathered: Some(g(Some(Stop::Budget))), ..Default::default() }.failed());
        // found nothing after looking: a real answer, not a failure
        assert!(!MeetingContext { gathered: Some(g(None)), ..Default::default() }.failed());
        // ran out of time but found something: keep it
        assert!(!MeetingContext { brief: "x".into(), gathered: Some(g(Some(Stop::Budget))), ..Default::default() }.failed());
    }

    #[test]
    fn prompt_block_is_capped() {
        let c = MeetingContext { open_threads: (0..200).map(|i| format!("thread number {i} with some words")).collect(), ..Default::default() };
        assert!(c.prompt_block().len() <= PROMPT_MAX_CHARS);
    }

    // ---- pre-meeting brief

    fn ev(uid: &str, title: &str, start: &str) -> CalendarEvent {
        let t = chrono::DateTime::parse_from_rfc3339(start).unwrap().to_utc();
        CalendarEvent {
            id: format!("{uid}@{}", t.timestamp()),
            uid: uid.into(),
            title: title.into(),
            start: t,
            end: t + chrono::Duration::minutes(30),
            link: None,
            organizer: None,
            attendees: vec![],
            description: None,
            calendar: Some("me@example.test".into()),
            declined: false,
            recurring: false,
            html_link: None,
            calendar_name: None,
            color: None,
        }
    }

    fn brief(text: &str) -> MeetingContext {
        MeetingContext { brief: text.into(), ..Default::default() }
    }

    #[test]
    fn brief_is_found_by_event_id_and_not_for_other_events() {
        let d = tempfile::tempdir().unwrap();
        let a = ev("pilot@example.test", "Pilot review", "2026-11-04T10:00:00Z");
        save_in(d.path(), &a, &brief("About the pilot.")).unwrap();
        assert_eq!(find_in(d.path(), &a).unwrap().brief, "About the pilot.");
        // a different meeting entirely
        assert!(find_in(d.path(), &ev("other@example.test", "Budget", "2026-11-04T10:00:00Z")).is_none());
        // an empty dir (never gathered)
        assert!(find_in(&d.path().join("missing"), &a).is_none());
    }

    #[test]
    fn brief_survives_the_event_id_changing() {
        let d = tempfile::tempdir().unwrap();
        let a = ev("pilot@example.test", "Pilot review", "2026-11-04T10:00:00Z");
        save_in(d.path(), &a, &brief("About the pilot.")).unwrap();
        // moved an hour later: the id embeds the start, so it changed
        let moved = ev("pilot@example.test", "Pilot review", "2026-11-04T11:00:00Z");
        assert_ne!(moved.id, a.id);
        assert_eq!(find_in(d.path(), &moved).unwrap().brief, "About the pilot.");
        // the calendar re-issued it under a new uid, same title and time
        let reissued = ev("new-uid@example.test", "  pilot   REVIEW ", "2026-11-04T10:00:00Z");
        assert_eq!(find_in(d.path(), &reissued).unwrap().brief, "About the pilot.");
        // same new uid but moved: no way to know it's the same meeting
        assert!(find_in(d.path(), &ev("new-uid@example.test", "Pilot review", "2026-11-04T10:30:00Z")).is_none());
    }

    #[test]
    fn recurring_occurrences_get_their_own_brief() {
        let d = tempfile::tempdir().unwrap();
        let mut mon = ev("standup@example.test", "Weekly sync", "2026-11-02T09:00:00Z");
        mon.recurring = true;
        let mut tue = ev("standup@example.test", "Weekly sync", "2026-11-03T09:00:00Z");
        tue.recurring = true;
        let mut next_week = ev("standup@example.test", "Weekly sync", "2026-11-09T09:00:00Z");
        next_week.recurring = true;
        save_in(d.path(), &mon, &brief("Monday's brief.")).unwrap();
        // same series uid, same title, but another day: not Monday's brief
        assert!(find_in(d.path(), &tue).is_none());
        assert!(find_in(d.path(), &next_week).is_none());
        save_in(d.path(), &tue, &brief("Tuesday's brief.")).unwrap();
        assert_eq!(find_in(d.path(), &tue).unwrap().brief, "Tuesday's brief.");
        assert_eq!(find_in(d.path(), &mon).unwrap().brief, "Monday's brief.");
        // an occurrence moved a little picks its own series' nearest brief
        let tue_late = ev("standup@example.test", "Weekly sync", "2026-11-03T10:00:00Z");
        assert_eq!(find_in(d.path(), &tue_late).unwrap().brief, "Tuesday's brief.");
    }

    #[test]
    fn failed_and_empty_runs_are_not_briefs() {
        let d = tempfile::tempdir().unwrap();
        let a = ev("a@example.test", "A", "2026-11-04T10:00:00Z");
        let failed = MeetingContext {
            gathered: Some(Gathered { at: chrono::Local::now(), model: "m".into(), secs: 1.0, steps: vec![], stopped: Some(Stop::ModelError), note: None }),
            ..Default::default()
        };
        // a failed run is never cached, so the meeting is gathered again later
        save_in(d.path(), &a, &failed).unwrap();
        assert!(find_in(d.path(), &a).is_none());
        // "looked and found nothing" is cached (don't re-run) but isn't shown
        let nothing = MeetingContext { gathered: Some(Gathered { at: chrono::Local::now(), model: "m".into(), secs: 1.0, steps: vec![], stopped: None, note: None }), ..Default::default() };
        save_in(d.path(), &a, &nothing).unwrap();
        assert!(find_in(d.path(), &a).is_some());
        assert!(briefs_in(d.path(), std::slice::from_ref(&a), d.path()).is_empty());
        save_in(d.path(), &a, &brief("Real.")).unwrap();
        assert_eq!(briefs_in(d.path(), std::slice::from_ref(&a), d.path())[&a.id].context.brief, "Real.");
    }

    fn session(root: &Path, name: &str, title: &str, started: &str, event: Option<CalendarEvent>, items: &[(&str, &str)]) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let started = chrono::DateTime::parse_from_rfc3339(started).unwrap().with_timezone(&Local);
        let meta = crate::session::SessionMeta {
            title: title.into(),
            started,
            ended: Some(started + chrono::Duration::minutes(30)),
            event,
            state: crate::session::SessionState::Done,
            note_path: None,
            error: None,
            routes: vec![],
            sends: vec![],
            shares: vec![],
            meeting_app: None,
            attended: None,
        };
        std::fs::write(dir.join("meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
        let summary = crate::summary::Summary {
            action_items: items.iter().map(|(o, t)| crate::summary::ActionItem { owner: (!o.is_empty()).then(|| o.to_string()), task: t.to_string() }).collect(),
            ..Default::default()
        };
        std::fs::write(dir.join("summary.json"), serde_json::to_vec(&summary).unwrap()).unwrap();
    }

    #[test]
    fn last_meetings_action_items_come_from_the_same_series() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let prev = ev("pilot@example.test", "Pilot review", "2026-10-28T10:00:00Z");
        session(root.path(), "a", "Pilot review", "2026-10-21T10:00:00Z", Some(ev("pilot@example.test", "Pilot review", "2026-10-21T10:00:00Z")), &[("Sam", "Old item")]);
        session(root.path(), "b", "Pilot review", "2026-10-28T10:00:00Z", Some(prev), &[("Sam", "Send the pilot scope"), ("", "Book a follow-up")]);
        // newer but unrelated, and one after the meeting being briefed
        session(root.path(), "c", "Budget", "2026-11-01T10:00:00Z", Some(ev("budget@example.test", "Budget", "2026-11-01T10:00:00Z")), &[("Kim", "Unrelated")]);
        session(root.path(), "d", "Pilot review", "2026-11-09T10:00:00Z", Some(ev("pilot@example.test", "Pilot review", "2026-11-09T10:00:00Z")), &[("Sam", "From the future")]);
        let next = ev("pilot@example.test", "Pilot review", "2026-11-04T10:00:00Z");
        save_in(cache.path(), &next, &brief("Pilot, week 3.")).unwrap();
        let b = briefs_in(cache.path(), std::slice::from_ref(&next), root.path()).remove(&next.id).unwrap();
        let last = b.last_meeting.unwrap();
        assert_eq!(last.title, "Pilot review");
        assert_eq!(last.action_items.iter().map(|a| a.task.as_str()).collect::<Vec<_>>(), vec!["Send the pilot scope", "Book a follow-up"]);
        // the cache stamp is internal and isn't part of what is shown
        assert!(b.context.event.is_none());
    }

    #[test]
    fn last_meeting_falls_back_to_title_then_people_and_may_be_absent() {
        let root = tempfile::tempdir().unwrap();
        let person = |e: &str| crate::calendar::Attendee { name: None, email: e.into(), status: None };
        let mut with_sathiren = ev("one-off@example.test", "Intro call", "2026-10-30T10:00:00Z");
        with_sathiren.attendees = vec![person("me@example.test"), person("sathiren@guest.example.org")];
        session(root.path(), "a", "Intro call", "2026-10-30T10:00:00Z", Some(with_sathiren), &[("Alex", "Share the deck")]);
        let all = crate::session::list_sessions(root.path(), usize::MAX);
        // same title, different uid
        let by_title = last_meeting(&ev("x@example.test", "intro CALL", "2026-11-04T10:00:00Z"), &all).unwrap();
        assert_eq!(by_title.action_items[0].task, "Share the deck");
        // different title, same other person
        let mut follow = ev("y@example.test", "Pilot kickoff", "2026-11-04T10:00:00Z");
        follow.attendees = vec![person("me@example.test"), person("sathiren@guest.example.org")];
        assert!(last_meeting(&follow, &all).is_some());
        // nothing in common
        assert!(last_meeting(&ev("z@example.test", "Dentist", "2026-11-04T10:00:00Z"), &all).is_none());
        // a matching meeting with no action items shows nothing
        session(root.path(), "b", "Retro", "2026-10-31T10:00:00Z", None, &[]);
        let all = crate::session::list_sessions(root.path(), usize::MAX);
        assert!(last_meeting(&ev("r@example.test", "Retro", "2026-11-04T10:00:00Z"), &all).is_none());
    }

    #[test]
    fn short_brief_cuts_cleanly() {
        let c = brief("Pilot review with Example Labs. They are deciding on a three-month pilot. Sathiren owes pricing.");
        assert_eq!(c.short_brief(200), "Pilot review with Example Labs. They are deciding on a three-month pilot. Sathiren owes pricing.");
        assert_eq!(c.short_brief(80), "Pilot review with Example Labs. They are deciding on a three-month pilot.");
        let words = brief("one two three four five six seven eight nine ten eleven twelve");
        let cut = words.short_brief(30);
        assert!(cut.ends_with('…') && cut.chars().count() <= 31, "{cut}");
        assert_eq!(brief("  \n ").short_brief(40), "");
    }
}
