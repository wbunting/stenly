//! Calendar events from ICS feeds (Google Calendar's secret iCal address to start).

use crate::config::CalendarConfig;
use crate::ics::{self, Component, IcsTime};
use crate::meeting_url::{MeetingLink, find_meeting_link};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attendee {
    pub name: Option<String>,
    pub email: String,
    /// ACCEPTED / DECLINED / TENTATIVE / NEEDS-ACTION
    pub status: Option<String>,
}

impl Attendee {
    pub fn display(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.email.split('@').next().unwrap_or(&self.email).to_string())
    }
}

/// A selectable calendar from a remote provider or the host operating system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalendarInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub account: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalendarEvent {
    /// Stable per-occurrence id: `uid@start`.
    pub id: String,
    pub uid: String,
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub link: Option<MeetingLink>,
    pub organizer: Option<Attendee>,
    pub attendees: Vec<Attendee>,
    pub description: Option<String>,
    /// Calendar owner (X-WR-CALNAME, usually the account email for Google).
    pub calendar: Option<String>,
    /// The calendar owner declined this occurrence.
    pub declined: bool,
    pub recurring: bool,
    /// Link to the event in the calendar's web UI ("Open in Google Calendar").
    #[serde(default)]
    pub html_link: Option<String>,
    /// Source calendar's display name and colour (#rrggbb), for the event card.
    #[serde(default)]
    pub calendar_name: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
}

impl CalendarEvent {
    pub fn is_live_at(&self, t: DateTime<Utc>) -> bool {
        self.start <= t && t < self.end
    }
}

fn parse_person(p: &ics::Property) -> Option<Attendee> {
    let email = p.value.trim();
    let email = email.strip_prefix("mailto:").or_else(|| email.strip_prefix("MAILTO:")).unwrap_or(email);
    if email.is_empty() {
        return None;
    }
    Some(Attendee {
        name: p.param("CN").map(|s| s.trim_matches('"').to_string()).filter(|n| n != email),
        email: email.to_string(),
        status: p.param("PARTSTAT").map(str::to_ascii_uppercase),
    })
}

fn build_event(
    master: &Component,
    occ: &Component,
    start: DateTime<Utc>,
    duration: Duration,
    calendar: Option<&str>,
) -> Option<CalendarEvent> {
    if occ.text("STATUS").is_some_and(|s| s.eq_ignore_ascii_case("CANCELLED")) {
        return None;
    }
    if occ.text("TRANSP").is_some_and(|s| s.eq_ignore_ascii_case("TRANSPARENT"))
        && occ.get("ATTENDEE").is_none()
    {
        // "free" blocks without guests (focus time, reminders) aren't meetings
        return None;
    }
    let uid = master.text("UID").unwrap_or_default();
    let title = occ.text("SUMMARY").unwrap_or_else(|| "Untitled meeting".into());
    let description = occ.text("DESCRIPTION");
    let location = occ.text("LOCATION");
    // Google puts Meet links in X-GOOGLE-CONFERENCE; others in URL / LOCATION / DESCRIPTION.
    let conf = occ.text("X-GOOGLE-CONFERENCE");
    let url = occ.text("URL");
    let link = find_meeting_link(
        [conf.as_deref(), url.as_deref(), location.as_deref(), description.as_deref()]
            .into_iter()
            .flatten(),
    );
    let attendees: Vec<Attendee> = occ.all("ATTENDEE").filter_map(parse_person).collect();
    let organizer = occ.get("ORGANIZER").and_then(parse_person);
    let declined = calendar.is_some_and(|me| {
        attendees
            .iter()
            .any(|a| a.email.eq_ignore_ascii_case(me) && a.status.as_deref() == Some("DECLINED"))
    });
    // An override may move the occurrence; its own DTSTART/DTEND win.
    let (start, end) = match (occ.get("DTSTART").and_then(ics::parse_time), occ.get("DTEND").and_then(ics::parse_time)) {
        (Some(IcsTime::At(s)), Some(IcsTime::At(e))) if !std::ptr::eq(occ, master) => (s, e),
        (Some(IcsTime::At(s)), None) if !std::ptr::eq(occ, master) => (s, s + duration),
        _ => (start, start + duration),
    };
    Some(CalendarEvent {
        id: format!("{uid}@{}", start.timestamp()),
        uid,
        title,
        start,
        end,
        link,
        organizer,
        attendees,
        description,
        calendar: calendar.map(str::to_string),
        declined,
        recurring: master.get("RRULE").is_some(),
        html_link: None,
        calendar_name: None,
        color: None,
    })
}

fn event_duration(ev: &Component) -> Duration {
    let start = ev.get("DTSTART").and_then(ics::parse_time);
    let end = ev.get("DTEND").and_then(ics::parse_time);
    match (start, end) {
        (Some(IcsTime::At(s)), Some(IcsTime::At(e))) if e > s => e - s,
        _ => ev
            .text("DURATION")
            .and_then(|d| parse_duration(&d))
            .unwrap_or(Duration::minutes(30)),
    }
}

/// Subset of RFC 5545 DURATION: `PT1H30M`, `P1D`, `PT45M`.
fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim().trim_start_matches('+');
    let s = s.strip_prefix('P')?;
    let mut total = Duration::zero();
    let mut num = String::new();
    for c in s.chars() {
        match c {
            '0'..='9' => num.push(c),
            'T' => {}
            'W' | 'D' | 'H' | 'M' | 'S' => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += match c {
                    'W' => Duration::weeks(n),
                    'D' => Duration::days(n),
                    'H' => Duration::hours(n),
                    'M' => Duration::minutes(n),
                    _ => Duration::seconds(n),
                };
            }
            _ => return None,
        }
    }
    Some(total)
}

/// All timed events in `[from, to)` from one ICS document.
pub fn events_from_ics(text: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<CalendarEvent> {
    let mut out = Vec::new();
    for cal in ics::parse(text).iter().filter(|c| c.kind == "VCALENDAR") {
        let calendar = cal.text("X-WR-CALNAME");
        let idx = ics::index_events(cal);
        // Look back by the longest plausible meeting so in-progress events are kept.
        let lookback = from - Duration::hours(12);
        for master in &idx.masters {
            let duration = event_duration(master);
            let uid = master.text("UID").unwrap_or_default();
            for start in ics::occurrences(master, lookback, to) {
                let occ = idx.overrides.get(&(uid.clone(), start)).copied().unwrap_or(master);
                if let Some(ev) = build_event(master, occ, start, duration, calendar.as_deref())
                    && ev.end > from && ev.start < to {
                        out.push(ev);
                    }
            }
        }
        // Overrides whose master is outside the window (moved occurrences).
        for ((uid, _), occ) in &idx.overrides {
            if let Some(IcsTime::At(s)) = occ.get("DTSTART").and_then(ics::parse_time) {
                if s >= to || s < lookback || out.iter().any(|e| &e.uid == uid && e.start == s) {
                    continue;
                }
                let duration = event_duration(occ);
                if let Some(ev) = build_event(occ, occ, s, duration, calendar.as_deref())
                    && ev.end > from {
                        out.push(ev);
                    }
            }
        }
    }
    out.sort_by_key(|e| e.start);
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// Fetch an ICS feed. `webcal://` is treated as `https://`; local paths are read directly.
pub fn fetch_ics(url: &str) -> Result<String> {
    let url = url.trim();
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::read_to_string(path).with_context(|| format!("reading {path}"));
    }
    if !url.contains("://") {
        let p = crate::util::expand_tilde(url);
        return std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()));
    }
    let url = url.replacen("webcal://", "https://", 1);
    let body = ureq::get(&url)
        .header("User-Agent", "stenly/0.1")
        .config()
        .timeout_connect(Some(std::time::Duration::from_secs(5)))
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        .build()
        .call()
        .with_context(|| format!("fetching calendar {}", redact(&url)))?
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_string()?;
    Ok(body)
}

/// Google's secret ICS address is a credential; never log it in full.
pub fn redact(url: &str) -> String {
    match url.find("/private-") {
        Some(i) => format!("{}/private-…", &url[..i]),
        None => url.chars().take(48).collect(),
    }
}

/// Apply shared scheduling filters after one or more calendar sources have
/// contributed events. Platform APIs stay outside `stenly-core`.
pub fn filter_events(cfg: &CalendarConfig, mut events: Vec<CalendarEvent>) -> Vec<CalendarEvent> {
    events.retain(|e| wants_prompt(cfg, e));
    events.sort_by_key(|e| e.start);
    events.dedup_by(|a, b| a.uid == b.uid && a.start == b.start);
    drop_companions(&mut events);
    events
}

/// Events from configured network feeds in `[from, to)`, filtered by config.
pub fn upcoming(cfg: &CalendarConfig, from: DateTime<Utc>, to: DateTime<Utc>) -> (Vec<CalendarEvent>, Vec<String>) {
    let mut events = Vec::new();
    let mut errors = Vec::new();
    if cfg.source.uses_kody() && !cfg.kody_url.trim().is_empty() {
        match crate::kody::KodyClient::from_saved() {
            Some(mut client) => match crate::kody::fetch_events(&mut client, &cfg.kody_integration, &cfg.calendar_ids, from, to) {
                Ok((evs, errs)) => {
                    events.extend(evs);
                    errors.extend(errs);
                }
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "kody calendar fetch failed");
                    errors.push(format!("Google Calendar (via Kody): {e:#}"));
                }
            },
            None => errors.push("Kody isn't signed in. Reconnect in Settings → Calendar.".into()),
        }
    }
    for url in cfg.source.uses_ics().then_some(&cfg.ics_urls).into_iter().flatten() {
        match fetch_ics(url) {
            Ok(text) => events.extend(events_from_ics(&text, from, to)),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "calendar fetch failed");
                errors.push(format!("{e:#}"));
            }
        }
    }
    (filter_events(cfg, events), errors)
}

/// Companion entries ("NOTES - Chase/Intuit call", a prep block) sit at the
/// same time as the real call but have no call link of their own. When a
/// linked meeting overlaps one, show only the meeting.
fn drop_companions(events: &mut Vec<CalendarEvent>) {
    let linked: Vec<(DateTime<Utc>, DateTime<Utc>)> = events.iter().filter(|e| e.link.is_some()).map(|e| (e.start, e.end)).collect();
    events.retain(|e| e.link.is_some() || !linked.iter().any(|(s, en)| *s < e.end && e.start < *en && (*s - e.start).num_minutes().abs() <= 5));
}

pub fn wants_prompt(cfg: &CalendarConfig, e: &CalendarEvent) -> bool {
    if e.declined {
        return false;
    }
    // "Looks like a meeting": a call link, or someone besides you invited.
    // Many real calls (phone, link sent by email, an external host's Zoom) have
    // no link in the invite but do have guests; solo blocks ("Sleep",
    // "Focus") have neither.
    if cfg.require_meeting_link && e.link.is_none() && !has_guests(e) {
        return false;
    }
    let title = e.title.to_lowercase();
    !cfg.ignore_titles.iter().any(|t| !t.is_empty() && title.contains(&t.to_lowercase()))
}

/// Someone other than the calendar owner is on the invite.
pub fn has_guests(e: &CalendarEvent) -> bool {
    let me = e.calendar.as_deref();
    e.attendees.iter().filter(|a| a.status.as_deref() != Some("DECLINED")).any(|a| me.is_none_or(|m| !a.email.eq_ignore_ascii_case(m)))
}

pub fn auto_record(cfg: &CalendarConfig, e: &CalendarEvent) -> bool {
    let title = e.title.to_lowercase();
    cfg.auto_record_titles.iter().any(|t| !t.is_empty() && title.contains(&t.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting_url::Provider;
    use chrono::TimeZone;

    fn google_like() -> String {
        // Shape of a real Google "secret iCal" export (trimmed).
        [
            "BEGIN:VCALENDAR",
            "PRODID:-//Google Inc//Google Calendar 70.9054//EN",
            "X-WR-CALNAME:will@example.com",
            "BEGIN:VEVENT",
            "DTSTART:20261020T130000Z",
            "DTEND:20261020T133000Z",
            "UID:meet1@google.com",
            "ORGANIZER;CN=Alice:mailto:alice@example.com",
            "ATTENDEE;CUTYPE=INDIVIDUAL;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED;CN=Alice",
            " ;X-NUM-GUESTS=0:mailto:alice@example.com",
            "ATTENDEE;CUTYPE=INDIVIDUAL;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED;CN=will@",
            " example.com;X-NUM-GUESTS=0:mailto:will@example.com",
            "X-GOOGLE-CONFERENCE:https://meet.google.com/abc-defg-hij",
            "DESCRIPTION:-::~:~::~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~:~::~:~::-\\nJoin with Google Meet: https://meet.google.com/abc-defg-hij",
            "SUMMARY:Design review",
            "STATUS:CONFIRMED",
            "END:VEVENT",
            "BEGIN:VEVENT",
            "DTSTART:20261020T150000Z",
            "DTEND:20261020T160000Z",
            "UID:zoom1@google.com",
            "LOCATION:https://us02web.zoom.us/j/81234567890?pwd=xyz",
            "ATTENDEE;PARTSTAT=DECLINED;CN=will@example.com:mailto:will@example.com",
            "SUMMARY:Vendor call",
            "END:VEVENT",
            "BEGIN:VEVENT",
            "DTSTART:20261020T170000Z",
            "DTEND:20261020T180000Z",
            "UID:lunch@google.com",
            "SUMMARY:Lunch",
            "END:VEVENT",
            "BEGIN:VEVENT",
            "DTSTART;VALUE=DATE:20261020",
            "DTEND;VALUE=DATE:20261021",
            "UID:allday@google.com",
            "SUMMARY:Offsite https://meet.google.com/xxx-yyyy-zzz",
            "END:VEVENT",
            "BEGIN:VEVENT",
            "DTSTART;TZID=Asia/Tokyo:20261019T090000",
            "DTEND;TZID=Asia/Tokyo:20261019T093000",
            "RRULE:FREQ=DAILY;COUNT=5",
            "UID:standup@google.com",
            "SUMMARY:Standup",
            "DESCRIPTION:Microsoft Teams meeting\\nJoin: https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc%40thread.v2/0",
            "END:VEVENT",
            "BEGIN:VEVENT",
            "DTSTART;TZID=Asia/Tokyo:20261020T100000",
            "DTEND;TZID=Asia/Tokyo:20261020T103000",
            "RECURRENCE-ID;TZID=Asia/Tokyo:20261020T090000",
            "UID:standup@google.com",
            "SUMMARY:Standup (moved)",
            "DESCRIPTION:https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc%40thread.v2/0",
            "END:VEVENT",
            "END:VCALENDAR",
        ]
        .join("\r\n")
    }

    #[test]
    fn google_feed() {
        let from = Utc.with_ymd_and_hms(2026, 10, 19, 22, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 10, 21, 0, 0, 0).unwrap();
        let evs = events_from_ics(&google_like(), from, to);
        let titles: Vec<_> = evs.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["Standup (moved)", "Design review", "Vendor call", "Lunch"], "{evs:#?}");

        let standup = &evs[0];
        assert_eq!(standup.start, Utc.with_ymd_and_hms(2026, 10, 20, 1, 0, 0).unwrap());
        assert_eq!(standup.link.as_ref().unwrap().provider, Provider::Teams);
        assert!(standup.recurring);

        let review = &evs[1];
        assert_eq!(review.link.as_ref().unwrap().provider, Provider::GoogleMeet);
        assert_eq!(review.attendees.len(), 2);
        assert_eq!(review.attendees[0].display(), "Alice");
        assert_eq!(review.organizer.as_ref().unwrap().email, "alice@example.com");

        assert!(evs[2].declined);

        let cfg = CalendarConfig::default();
        let wanted: Vec<_> = evs.iter().filter(|e| wants_prompt(&cfg, e)).map(|e| e.title.as_str()).collect();
        assert_eq!(wanted, ["Standup (moved)", "Design review"]);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("PT1H30M"), Some(Duration::minutes(90)));
        assert_eq!(parse_duration("P1D"), Some(Duration::days(1)));
    }
}
