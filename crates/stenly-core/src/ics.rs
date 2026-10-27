//! Minimal RFC 5545 reader: enough for Google Calendar's secret iCal feed
//! (and most other providers). Recurrences are expanded with the `rrule` crate.

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Property {
    pub name: String,
    pub params: Vec<(String, String)>,
    pub value: String,
    /// The original (unfolded) line, needed to hand DTSTART/RRULE/EXDATE to `rrule`.
    pub raw: String,
}

impl Property {
    pub fn param(&self, key: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, Default)]
pub struct Component {
    pub kind: String,
    pub props: Vec<Property>,
    pub children: Vec<Component>,
}

impl Component {
    pub fn get(&self, name: &str) -> Option<&Property> {
        self.props.iter().find(|p| p.name.eq_ignore_ascii_case(name))
    }
    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Property> + 'a {
        self.props.iter().filter(move |p| p.name.eq_ignore_ascii_case(name))
    }
    pub fn text(&self, name: &str) -> Option<String> {
        self.get(name).map(|p| unescape(&p.value))
    }
}

fn unfold(input: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in input.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if (line.starts_with(' ') || line.starts_with('\t')) && !lines.is_empty() {
            lines.last_mut().unwrap().push_str(&line[1..]);
        } else if !line.is_empty() {
            lines.push(line.to_string());
        }
    }
    lines
}

fn parse_line(line: &str) -> Option<Property> {
    // name *(";" param) ":" value — colons inside quoted params don't count.
    let mut in_quotes = false;
    let mut split = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                split = Some(i);
                break;
            }
            _ => {}
        }
    }
    let split = split?;
    let (head, value) = (&line[..split], &line[split + 1..]);
    let mut parts = Vec::new();
    let mut cur = String::new();
    in_quotes = false;
    for c in head.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            ';' if !in_quotes => parts.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    parts.push(cur);
    let name = parts.remove(0).to_ascii_uppercase();
    let params = parts
        .into_iter()
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((k.to_ascii_uppercase(), v.to_string()))
        })
        .collect();
    Some(Property { name, params, value: value.to_string(), raw: line.to_string() })
}

pub fn parse(input: &str) -> Vec<Component> {
    let mut stack: Vec<Component> = Vec::new();
    let mut roots = Vec::new();
    for line in unfold(input) {
        let Some(prop) = parse_line(&line) else { continue };
        match prop.name.as_str() {
            "BEGIN" => stack.push(Component { kind: prop.value.to_ascii_uppercase(), ..Default::default() }),
            "END" => {
                if let Some(done) = stack.pop() {
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(done),
                        None => roots.push(done),
                    }
                }
            }
            _ => {
                if let Some(cur) = stack.last_mut() {
                    cur.props.push(prop);
                }
            }
        }
    }
    roots
}

pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A parsed date-time. `AllDay` events are not meetings for our purposes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IcsTime {
    At(DateTime<Utc>),
    AllDay(NaiveDate),
}

pub fn parse_time(p: &Property) -> Option<IcsTime> {
    let v = p.value.trim();
    if p.param("VALUE").is_some_and(|x| x.eq_ignore_ascii_case("DATE")) || v.len() == 8 {
        return NaiveDate::parse_from_str(v, "%Y%m%d").ok().map(IcsTime::AllDay);
    }
    if let Some(z) = v.strip_suffix('Z') {
        let naive = NaiveDateTime::parse_from_str(z, "%Y%m%dT%H%M%S").ok()?;
        return Some(IcsTime::At(Utc.from_utc_datetime(&naive)));
    }
    let naive = NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()?;
    let local = match p.param("TZID").and_then(|tz| tz.trim_matches('"').parse::<chrono_tz::Tz>().ok()) {
        Some(tz) => tz.from_local_datetime(&naive).earliest()?.with_timezone(&Utc),
        // floating time or unknown TZID (e.g. Windows names): treat as local
        None => Local.from_local_datetime(&naive).earliest()?.with_timezone(&Utc),
    };
    Some(IcsTime::At(local))
}

/// Expand a VEVENT's recurrence into start instants inside `[from, to)`.
/// Returns the plain DTSTART if the event doesn't recur.
pub fn occurrences(ev: &Component, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<DateTime<Utc>> {
    let Some(dtstart) = ev.get("DTSTART") else { return vec![] };
    let Some(IcsTime::At(start)) = parse_time(dtstart) else { return vec![] };
    let rrules: Vec<&Property> = ev.all("RRULE").collect();
    if rrules.is_empty() {
        return if start >= from && start < to { vec![start] } else { vec![] };
    }
    // Hand the original lines to `rrule`. Normalise DTSTART to UTC when its TZID
    // isn't an IANA name so the parser doesn't reject it.
    let dtstart_line = if dtstart.param("TZID").is_some_and(|t| t.trim_matches('"').parse::<chrono_tz::Tz>().is_err()) {
        format!("DTSTART:{}", start.format("%Y%m%dT%H%M%SZ"))
    } else {
        dtstart.raw.clone()
    };
    let mut text = dtstart_line;
    for r in &rrules {
        text.push('\n');
        text.push_str(&r.raw);
    }
    for ex in ev.all("EXDATE").chain(ev.all("RDATE")) {
        // rrule can't parse VALUE=DATE exdates against a datetime start; skip those.
        if ex.param("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE")) {
            continue;
        }
        text.push('\n');
        text.push_str(&ex.raw);
    }
    let set: rrule::RRuleSet = match text.parse() {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, rule = %text, "unparseable recurrence; using first instance only");
            return if start >= from && start < to { vec![start] } else { vec![] };
        }
    };
    let tz = rrule::Tz::UTC;
    set.after(from.with_timezone(&tz))
        .before(to.with_timezone(&tz))
        .all(500)
        .dates
        .into_iter()
        .map(|d| d.with_timezone(&Utc))
        .collect()
}

/// Group VEVENTs by UID and index RECURRENCE-ID overrides.
pub struct EventIndex<'a> {
    pub masters: Vec<&'a Component>,
    /// (uid, original start) -> override
    pub overrides: HashMap<(String, DateTime<Utc>), &'a Component>,
}

pub fn index_events(cal: &Component) -> EventIndex<'_> {
    let mut masters = Vec::new();
    let mut overrides = HashMap::new();
    for ev in cal.children.iter().filter(|c| c.kind == "VEVENT") {
        let uid = ev.text("UID").unwrap_or_default();
        match ev.get("RECURRENCE-ID").and_then(parse_time) {
            Some(IcsTime::At(rid)) => {
                overrides.insert((uid, rid), ev);
            }
            Some(IcsTime::AllDay(_)) => {}
            None => masters.push(ev),
        }
    }
    EventIndex { masters, overrides }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "BEGIN:VCALENDAR\r\nX-WR-CALNAME:me@example.com\r\nBEGIN:VEVENT\r\nDTSTART;TZID=America/New_York:20261019T100000\r\nDTEND;TZID=America/New_York:20261019T103000\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20261231T000000Z\r\nEXDATE;TZID=America/New_York:20261021T100000\r\nUID:abc@google.com\r\nSUMMARY:Weekly sync\\, team\r\nDESCRIPTION:Join with Google Meet: https://meet.google.com/abc-defg-hij\\nOr di\r\n al in\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn parses_and_expands() {
        let roots = parse(SAMPLE);
        let cal = &roots[0];
        let ev = &cal.children[0];
        assert_eq!(ev.text("SUMMARY").unwrap(), "Weekly sync, team");
        assert!(ev.text("DESCRIPTION").unwrap().ends_with("Or dial in"));
        let from = Utc.with_ymd_and_hms(2026, 10, 19, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 10, 27, 0, 0, 0).unwrap();
        let occ = occurrences(ev, from, to);
        // Mon 19th, (Wed 21st excluded), Mon 26th
        assert_eq!(occ.len(), 2, "{occ:?}");
        assert_eq!(occ[0], Utc.with_ymd_and_hms(2026, 10, 19, 14, 0, 0).unwrap());
        assert_eq!(occ[1], Utc.with_ymd_and_hms(2026, 10, 26, 14, 0, 0).unwrap());
    }
}
