//! Meeting context: background on a meeting's people, companies and topics,
//! from the calendar invite and (optionally) the context agent in
//! `stenly-agent`, which looks things up in past meetings, the notes vault,
//! Notion and the user's MCP servers before the meeting is summarized.
//!
//! Stored as `context.json` next to `meta.json`. The pipeline uses it to
//! spell names and terms ([`crate::vocabulary`]), to describe the people in
//! the meeting, and as a `{context}` block in the summary prompt. It is never
//! a record of the meeting itself: the prompt says the transcript wins.

use crate::session::SessionMeta;
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::path::Path;

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
pub fn event_cache_path(event_id: &str) -> std::path::PathBuf {
    let safe: String = event_id.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).take(150).collect();
    crate::config::data_dir().join("context").join(format!("{safe}.json"))
}

pub fn load_for_event(event_id: &str) -> Option<MeetingContext> {
    serde_json::from_slice(&std::fs::read(event_cache_path(event_id)).ok()?).ok()
}

/// Cache context gathered ahead of a meeting. A failed run isn't cached, so
/// the meeting is gathered again once it has been recorded.
pub fn save_for_event(event_id: &str, ctx: &MeetingContext) -> anyhow::Result<()> {
    if ctx.failed() {
        return Ok(());
    }
    let p = event_cache_path(event_id);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&p, serde_json::to_vec_pretty(ctx)?)?;
    Ok(())
}

/// Give a new session the context gathered for its calendar event (if any).
pub fn adopt_event_context(dir: &Path, meta: &SessionMeta) -> bool {
    let Some(ev) = &meta.event else { return false };
    if dir.join(FILE).exists() {
        return false;
    }
    match load_for_event(&ev.id) {
        Some(c) if !c.failed() => save(dir, &c).is_ok(),
        _ => false,
    }
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
}
