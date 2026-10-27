//! Read-only view over stenly's recordings: one meeting per session directory
//! (`<audio_dir>/<date time title>/meta.json`, `transcript.json`, `summary.json`)
//! plus the markdown note it produced.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, NaiveDate, TimeZone};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use stenly_core::config::Config;
use stenly_core::session::SessionMeta;
use stenly_core::summary::Summary;
use stenly_core::transcript::Transcript;
use stenly_core::util::fmt_timestamp;

pub struct Store {
    pub cfg: Config,
    pub audio_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Meeting {
    /// Session directory name; stable even when the meeting is retitled.
    pub id: String,
    pub dir: PathBuf,
    pub meta: SessionMeta,
}

#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub meeting_id: String,
    pub title: String,
    pub date: String,
    /// Where it matched: "title", "summary", "note" or "transcript".
    pub field: &'static str,
    /// Transcript position (mm:ss) for transcript hits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// Exact segment for in-app navigation (timestamps are display-only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    pub text: String,
}

pub struct TranscriptPage {
    pub text: String,
    pub next_offset: Option<usize>,
}

impl Store {
    /// Current on-disk config (re-read on every call so Settings changes apply live).
    /// Missing config → defaults; never writes the file.
    pub fn from_config() -> Result<Self> {
        let path = stenly_core::config::config_path();
        let cfg = if path.exists() { Config::load(&path)? } else { Config::default() };
        Ok(Self::new(cfg))
    }

    pub fn new(cfg: Config) -> Self {
        let audio_dir = cfg.paths().audio_dir;
        Self { cfg, audio_dir }
    }

    /// All meetings, newest first.
    pub fn meetings(&self) -> Vec<Meeting> {
        self.meetings_cancellable(|| false)
    }

    /// Check cancellation between metadata reads; an in-flight OS/iCloud read
    /// cannot be interrupted, but superseded work must not start another one.
    pub fn meetings_cancellable(&self, cancelled: impl Fn() -> bool) -> Vec<Meeting> {
        let Ok(entries) = std::fs::read_dir(&self.audio_dir) else { return vec![] };
        let mut meetings = Vec::new();
        for entry in entries.flatten() {
            if cancelled() { return vec![]; }
            let dir = entry.path();
            if let Ok(meta) = SessionMeta::load(&dir) {
                if let Some(id) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) {
                    meetings.push(Meeting { id, dir, meta });
                }
            }
        }
        meetings.sort_by(|a, b| b.meta.started.cmp(&a.meta.started));
        meetings
    }

    /// Resolve an id: exact session id, `latest`, or a case-insensitive
    /// substring of the id / title that matches exactly one meeting.
    pub fn resolve(&self, id: &str) -> Result<Meeting> {
        let id = id.trim();
        if id.is_empty() {
            bail!("meeting id is empty");
        }
        let all = self.meetings();
        if id.eq_ignore_ascii_case("latest") || id.eq_ignore_ascii_case("last") {
            return all.into_iter().next().context("no meetings recorded yet");
        }
        if let Some(m) = all.iter().find(|m| m.id == id) {
            return Ok(m.clone());
        }
        let needle = id.to_lowercase();
        let hits: Vec<&Meeting> = all
            .iter()
            .filter(|m| m.id.to_lowercase().contains(&needle) || m.meta.title.to_lowercase().contains(&needle))
            .collect();
        match hits.as_slice() {
            [one] => Ok((*one).clone()),
            [] => bail!("no meeting matches {id:?}; call list_meetings for ids"),
            many => bail!(
                "{id:?} matches {} meetings: {}",
                many.len(),
                many.iter().take(8).map(|m| format!("{:?}", m.id)).collect::<Vec<_>>().join(", ")
            ),
        }
    }

    pub fn transcript(m: &Meeting) -> Option<Transcript> {
        std::fs::read(m.dir.join("transcript.json")).ok().and_then(|b| serde_json::from_slice(&b).ok())
    }

    pub fn summary(m: &Meeting) -> Option<Summary> {
        std::fs::read(m.dir.join("summary.json")).ok().and_then(|b| serde_json::from_slice(&b).ok())
    }

    /// The note as it is now (the user may have edited it in Obsidian), or a
    /// freshly rendered one if the file is gone.
    pub fn note(&self, m: &Meeting) -> Result<(Option<PathBuf>, String)> {
        if let Some(p) = &m.meta.note_path
            && let Ok(text) = std::fs::read_to_string(p)
        {
            return Ok((Some(p.clone()), text));
        }
        let t = Self::transcript(m).context("meeting has no note and no transcript yet")?;
        let s = Self::summary(m);
        Ok((None, stenly_core::note::render(&m.meta, &t, s.as_ref(), None, true)))
    }

    pub fn search(&self, query: &str, since: Option<DateTime<Local>>, until: Option<DateTime<Local>>, limit: usize) -> Vec<SearchHit> {
        self.search_cancellable(query, since, until, limit, || false)
    }

    pub fn search_cancellable(&self, query: &str, since: Option<DateTime<Local>>, until: Option<DateTime<Local>>, limit: usize, cancelled: impl Fn() -> bool) -> Vec<SearchHit> {
        if limit == 0 || cancelled() { return vec![]; }
        let terms: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();
        if terms.is_empty() {
            return vec![];
        }
        let matches = |text: &str| {
            let lower = text.to_lowercase();
            terms.iter().all(|t| lower.contains(t.as_str()))
        };
        let mut hits = Vec::new();
        for m in self.meetings_cancellable(&cancelled).into_iter().filter(|m| in_range(&m.meta, since, until)) {
            if cancelled() { return vec![]; }
            let date = m.meta.started.format("%Y-%m-%d %H:%M").to_string();
            let hit = |field, at: Option<f64>, speaker, text: &str| SearchHit {
                meeting_id: m.id.clone(),
                title: m.meta.title.clone(),
                date: date.clone(),
                field,
                at: at.map(fmt_timestamp),
                speaker,
                segment_index: None,
                text: snippet(text, &terms[0], 320),
            };
            if matches(&m.meta.title) {
                hits.push(hit("title", None, None, &m.meta.title));
            }
            let people = attendees(&m.meta).join(" · ");
            if matches(&people) {
                hits.push(hit("attendees", None, None, &people));
            }
            if cancelled() { return vec![]; }
            if let Some(s) = Self::summary(&m) {
                let text = summary_text(&s);
                if matches(&text) {
                    hits.push(hit("summary", None, None, &text));
                }
            }
            if cancelled() { return vec![]; }
            if let Some(t) = Self::transcript(&m) {
                for (index, seg) in t.segments.iter().enumerate() {
                    if cancelled() { return vec![]; }
                    if !matches(&seg.text) { continue; }
                    let mut found = hit("transcript", Some(seg.start), Some(seg.label()), &seg.text);
                    found.segment_index = Some(index);
                    hits.push(found);
                    if hits.len() >= limit { break; }
                }
            }
            if hits.len() >= limit {
                break;
            }
        }
        hits.truncate(limit);
        hits
    }
}

pub fn in_range(meta: &SessionMeta, since: Option<DateTime<Local>>, until: Option<DateTime<Local>>) -> bool {
    since.is_none_or(|s| meta.started >= s) && until.is_none_or(|u| meta.started < u)
}

/// `YYYY-MM-DD` (local midnight) or RFC 3339.
pub fn parse_date(s: &str, end_of_day: bool) -> Result<DateTime<Local>> {
    let s = s.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Local));
    }
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").with_context(|| format!("bad date {s:?}; use YYYY-MM-DD"))?;
    let d = if end_of_day { d.succ_opt().unwrap_or(d) } else { d };
    Local
        .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .context("invalid local date")
}

fn summary_text(s: &Summary) -> String {
    let mut parts = vec![s.summary.clone()];
    for sec in &s.sections {
        parts.push(sec.heading.clone());
        for b in &sec.bullets {
            parts.push(b.text().to_string());
            parts.extend(b.sub().iter().cloned());
        }
    }
    parts.extend(s.key_points.iter().cloned());
    parts.extend(s.decisions.iter().cloned());
    parts.extend(s.action_items.iter().map(|a| a.task.clone()));
    parts.retain(|p| !p.trim().is_empty());
    parts.join(" · ")
}

/// ~`max` chars of `text` centred on the first occurrence of `term`.
fn snippet(text: &str, term: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let lower: Vec<char> = text.to_lowercase().chars().collect();
    let needle: Vec<char> = term.chars().collect();
    let pos = (0..lower.len().saturating_sub(needle.len()))
        .find(|&i| lower.get(i..i + needle.len()) == Some(needle.as_slice()))
        .unwrap_or(0);
    let start = pos.saturating_sub(max / 3);
    let end = (start + max).min(chars.len());
    let start = end.saturating_sub(max);
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out
}

pub fn duration_min(meta: &SessionMeta) -> Option<i64> {
    meta.duration_mins()
}

pub fn attendees(meta: &SessionMeta) -> Vec<String> {
    let Some(ev) = &meta.event else { return vec![] };
    ev.attendees
        .iter()
        .filter(|a| !ev.calendar.as_deref().is_some_and(|me| a.email.eq_ignore_ascii_case(me)))
        .map(|a| match &a.name {
            Some(n) => format!("{n} <{}>", a.email),
            None => a.email.clone(),
        })
        .collect()
}

/// Compact listing entry.
pub fn meeting_brief(m: &Meeting) -> Value {
    let meta = &m.meta;
    let mut v = json!({
        "id": m.id,
        "title": meta.title,
        "start": meta.started.to_rfc3339(),
        "duration_min": duration_min(meta),
        "state": format!("{:?}", meta.state),
        "has_transcript": m.dir.join("transcript.json").exists(),
        "has_summary": m.dir.join("summary.json").exists(),
    });
    let att = attendees(meta);
    if !att.is_empty() {
        v["attendees"] = json!(att);
    }
    if let Some(p) = meta.platform_label() {
        v["platform"] = json!(p);
    }
    v
}

/// Full detail: metadata + summary (no transcript).
pub fn meeting_detail(m: &Meeting) -> Value {
    let mut v = meeting_brief(m);
    let meta = &m.meta;
    if let Some(e) = meta.ended {
        v["end"] = json!(e.to_rfc3339());
    }
    if let Some(t) = Store::transcript(m) {
        v["words"] = json!(t.word_count());
        v["segments"] = json!(t.segments.len());
    }
    if let Some(ev) = &meta.event {
        let mut e = json!({ "calendar_title": ev.title, "recurring": ev.recurring });
        if let Some(o) = &ev.organizer {
            e["organizer"] = json!(o.email);
        }
        if let Some(l) = &ev.link {
            e["meeting_url"] = json!(l.url);
        }
        if let Some(d) = ev.description.as_deref().filter(|d| !d.trim().is_empty()) {
            e["description"] = json!(d.chars().take(2000).collect::<String>());
        }
        v["calendar_event"] = e;
    }
    if let Some(s) = Store::summary(m) {
        v["summary"] = serde_json::to_value(s).unwrap_or(Value::Null);
    }
    if let Some(p) = &meta.note_path {
        v["note_path"] = json!(p);
    }
    v["session_dir"] = json!(m.dir);
    if let Some(err) = &meta.error {
        v["warning"] = json!(err);
    }
    v
}

/// Speaker-attributed transcript text, paged by segment so long calls fit a context window.
pub fn transcript_page(m: &Meeting, t: &Transcript, offset: usize, max_chars: usize, speaker: Option<&str>) -> TranscriptPage {
    let meta = &m.meta;
    let mut text = String::new();
    if offset == 0 {
        text.push_str(&format!(
            "# {}\n{} · {} min · {} words · \"{}\" = the person who recorded, \"Them\" = everyone else on the call (named when Stenly could tell who spoke)\n\n",
            meta.title,
            meta.started.format("%Y-%m-%d %H:%M"),
            duration_min(meta).map(|d| d.to_string()).unwrap_or_else(|| "?".into()),
            t.word_count(),
            t.recorder_label()
        ));
    }
    let mut next = None;
    for (i, seg) in t.segments.iter().enumerate().skip(offset) {
        let label = seg.label();
        if speaker.is_some_and(|s| !(label.eq_ignore_ascii_case(s) || seg.channel.speaker_label().eq_ignore_ascii_case(s))) {
            continue;
        }
        let line = format!("{label} [{}]: {}\n", fmt_timestamp(seg.start), seg.text);
        if text.len() + line.len() > max_chars && i > offset {
            next = Some(i);
            break;
        }
        text.push_str(&line);
    }
    if let Some(n) = next {
        text.push_str(&format!(
            "\n[truncated at segment {n} of {}; call get_transcript again with offset={n} for the rest]\n",
            t.segments.len()
        ));
    }
    TranscriptPage { text, next_offset: next }
}
