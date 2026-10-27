//! "Send automatically after each meeting" rules.

use super::{AutoRule, Destination};
use crate::session::{SessionMeta, SessionState};

/// Does `rule` select this meeting? (Ignores `enabled`.)
pub fn matches(rule: &AutoRule, meta: &SessionMeta) -> bool {
    if rule.external_only && super::people::external(meta).is_empty() {
        return false;
    }
    let terms: Vec<&str> = rule.title_contains.iter().map(|t| t.trim()).filter(|t| !t.is_empty()).collect();
    if !terms.is_empty() {
        let title = meta.title.to_lowercase();
        if !terms.iter().any(|t| title.contains(&t.to_lowercase())) {
            return false;
        }
    }
    let cals: Vec<&str> = rule.calendars.iter().map(|t| t.trim()).filter(|t| !t.is_empty()).collect();
    if !cals.is_empty() {
        let Some(ev) = &meta.event else { return false };
        let names = [ev.calendar_name.as_deref(), ev.calendar.as_deref()];
        if !cals.iter().any(|c| names.iter().flatten().any(|n| n.eq_ignore_ascii_case(c))) {
            return false;
        }
    }
    true
}

/// Plain-language summary of a rule ("After meetings with external guests").
pub fn describe(rule: &AutoRule) -> String {
    if !rule.enabled {
        return "Only when you choose Send to…".into();
    }
    let mut parts = Vec::new();
    if rule.external_only {
        parts.push("with people outside your company".to_string());
    }
    let titles: Vec<String> = rule.title_contains.iter().filter(|t| !t.trim().is_empty()).map(|t| format!("“{}”", t.trim())).collect();
    if !titles.is_empty() {
        parts.push(format!("titled {}", titles.join(" or ")));
    }
    let cals: Vec<String> = rule.calendars.iter().filter(|t| !t.trim().is_empty()).map(|t| t.trim().to_string()).collect();
    if !cals.is_empty() {
        parts.push(format!("on {}", cals.join(" or ")));
    }
    if parts.is_empty() { "Automatically after every meeting".into() } else { format!("Automatically after meetings {}", parts.join(", ")) }
}

/// Should this finished meeting go to `dest` without being asked?
/// Only once per destination: a meeting re-processed later (Redo Notes)
/// isn't sent again automatically, and a meeting with no notes isn't sent.
pub fn should_auto_send(dest: &dyn Destination, meta: &SessionMeta) -> bool {
    let rule = dest.auto();
    rule.enabled
        && dest.records()
        && meta.state == SessionState::Done
        && !meta.sends.iter().any(|s| s.destination == dest.id() && s.auto)
        && matches(rule, meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destinations::{DestinationConfig, Kind, SendRecord, testutil};

    #[test]
    fn filters() {
        let mut meta = testutil::meta_with_attendees("me@example.com", &["me@example.com", "ana@acme.com"]);
        meta.title = "Acme weekly".into();
        let mut r = AutoRule { enabled: true, ..Default::default() };
        assert!(matches(&r, &meta));
        r.external_only = true;
        assert!(matches(&r, &meta));
        r.title_contains = vec!["standup".into(), "ACME".into()];
        assert!(matches(&r, &meta));
        r.title_contains = vec!["standup".into()];
        assert!(!matches(&r, &meta));
        r.title_contains.clear();
        r.calendars = vec!["Work".into()];
        assert!(!matches(&r, &meta));
        meta.event.as_mut().unwrap().calendar_name = Some("work".into());
        assert!(matches(&r, &meta));
        let internal = testutil::meta_with_attendees("me@example.com", &["me@example.com", "kim@example.com"]);
        assert!(!matches(&AutoRule { external_only: true, ..Default::default() }, &internal));
        // calendar filter with no calendar event never matches
        assert!(!matches(&AutoRule { calendars: vec!["Work".into()], ..Default::default() }, &testutil::meta("x")));
    }

    #[test]
    fn auto_sends_once() {
        let mut cfg = DestinationConfig::new(Kind::Slack, "Slack");
        cfg.auto.enabled = true;
        let dest = crate::destinations::build(&crate::Config::default(), &cfg);
        let mut meta = testutil::meta("Sync");
        assert!(should_auto_send(dest.as_ref(), &meta));
        meta.state = SessionState::Transcribed;
        assert!(!should_auto_send(dest.as_ref(), &meta));
        meta.state = SessionState::Done;
        meta.sends.push(SendRecord { destination: "slack".into(), at: chrono::Local::now(), ok: false, url: None, remote_id: None, detail: None, error: Some("x".into()), auto: true });
        assert!(!should_auto_send(dest.as_ref(), &meta));
        cfg.auto.enabled = false;
        let off = crate::destinations::build(&crate::Config::default(), &cfg);
        assert!(!should_auto_send(off.as_ref(), &testutil::meta("Sync")));
    }

    #[test]
    fn descriptions() {
        assert_eq!(describe(&AutoRule::default()), "Only when you choose Send to…");
        assert_eq!(describe(&AutoRule { enabled: true, ..Default::default() }), "Automatically after every meeting");
        let r = AutoRule { enabled: true, external_only: true, title_contains: vec!["Acme".into()], calendars: vec![] };
        assert_eq!(describe(&r), "Automatically after meetings with people outside your company, titled “Acme”");
    }
}
