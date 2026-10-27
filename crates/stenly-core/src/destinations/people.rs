//! Who was in a meeting, as destinations (and future CRM integrations) need
//! it: the other attendees, their email domains, and which of them are
//! outside your own organisation.

use crate::calendar::{Attendee, CalendarEvent};
use crate::session::SessionMeta;

/// Webmail domains: never a company, and two gmail users aren't colleagues.
const FREE_DOMAINS: &[&str] = &[
    "gmail.com", "googlemail.com", "outlook.com", "hotmail.com", "live.com", "msn.com", "yahoo.com", "yahoo.co.jp", "icloud.com", "me.com",
    "mac.com", "aol.com", "proton.me", "protonmail.com", "pm.me", "gmx.com", "gmx.de", "fastmail.com", "hey.com", "zoho.com", "yandex.ru",
    "qq.com", "163.com", "mail.com",
];

pub fn is_free_domain(domain: &str) -> bool {
    FREE_DOMAINS.contains(&domain.to_ascii_lowercase().as_str())
}

/// `ana@Acme.com` → `acme.com`.
pub fn domain(email: &str) -> Option<String> {
    let (_, d) = email.trim().rsplit_once('@')?;
    let d = d.trim().trim_end_matches('>').to_ascii_lowercase();
    (d.contains('.') && !d.is_empty()).then_some(d)
}

/// The host of a URL or bare domain, without `www.`: `https://www.acme.com/x` → `acme.com`.
pub fn host(s: &str) -> Option<String> {
    let s = s.trim();
    if s.contains('@') && !s.contains("://") {
        return domain(s);
    }
    let rest = s.split_once("://").map(|x| x.1).unwrap_or(s);
    let h = rest.split(['/', '?', '#']).next()?.split(':').next()?.to_ascii_lowercase();
    let h = h.strip_prefix("www.").unwrap_or(&h).to_string();
    (h.contains('.') && !h.contains(' ')).then_some(h)
}

/// `sub.acme.com` matches `acme.com` and vice versa.
pub fn same_org(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_ascii_lowercase(), b.to_ascii_lowercase());
    a == b || a.ends_with(&format!(".{b}")) || b.ends_with(&format!(".{a}"))
}

fn is_me(ev: &CalendarEvent, a: &Attendee) -> bool {
    ev.calendar.as_deref().is_some_and(|me| a.email.eq_ignore_ascii_case(me))
}

/// A display name without an embedded address: "Ana Lee <ana@x.com>" → "Ana Lee".
pub fn name(a: &Attendee) -> String {
    let n = a.display();
    let n = match n.split_once('<') {
        Some((before, _)) if !before.trim().is_empty() => before.trim().to_string(),
        _ => n.trim().to_string(),
    };
    n.trim_matches('"').to_string()
}

/// Your own email (the calendar account), if known.
pub fn my_email(ev: &CalendarEvent) -> Option<&str> {
    ev.calendar.as_deref().filter(|c| c.contains('@'))
}

/// Everyone but you.
pub fn others(meta: &SessionMeta) -> Vec<&Attendee> {
    let Some(ev) = &meta.event else { return vec![] };
    ev.attendees.iter().filter(|a| !is_me(ev, a) && !a.email.is_empty()).collect()
}

/// Attendees from outside your organisation. Without a known own address,
/// "external" means the guest list spans more than one company domain.
pub fn external(meta: &SessionMeta) -> Vec<&Attendee> {
    let Some(ev) = &meta.event else { return vec![] };
    let others = others(meta);
    match my_email(ev).and_then(domain) {
        Some(mine) if !is_free_domain(&mine) => others.into_iter().filter(|a| domain(&a.email).is_none_or(|d| !same_org(&d, &mine))).collect(),
        // a personal address: everyone else is external
        Some(_) => others,
        None => {
            let mut doms: Vec<String> = ev.attendees.iter().filter_map(|a| domain(&a.email)).collect();
            if let Some(o) = ev.organizer.as_ref().and_then(|o| domain(&o.email)) {
                doms.push(o);
            }
            doms.sort();
            doms.dedup();
            if doms.len() > 1 {
                let org = ev.organizer.as_ref().and_then(|o| domain(&o.email));
                others.into_iter().filter(|a| match (&org, domain(&a.email)) {
                    (Some(o), Some(d)) => !same_org(o, &d),
                    _ => true,
                }).collect()
            } else {
                vec![]
            }
        }
    }
}

/// Company domains of the external attendees (no webmail), deduplicated.
pub fn external_domains(meta: &SessionMeta) -> Vec<String> {
    let mut out: Vec<String> = external(meta).iter().filter_map(|a| domain(&a.email)).filter(|d| !is_free_domain(d)).collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destinations::testutil;

    #[test]
    fn domains_and_hosts() {
        assert_eq!(domain("Ana <ana@Acme.COM>").as_deref(), Some("acme.com"));
        assert_eq!(domain("nope"), None);
        assert_eq!(host("https://www.acme.com/about").as_deref(), Some("acme.com"));
        assert_eq!(host("example.com").as_deref(), Some("example.com"));
        assert_eq!(host("bob@beta.org").as_deref(), Some("beta.org"));
        assert_eq!(host("Acme Inc"), None);
        assert!(same_org("eu.acme.com", "acme.com"));
        assert!(!same_org("notacme.com", "acme.com"));
    }

    #[test]
    fn clean_names() {
        let a = |n: Option<&str>, e: &str| Attendee { name: n.map(str::to_string), email: e.into(), status: None };
        assert_eq!(name(&a(Some("Shelley Morgan <shelley@example.com>"), "shelley@example.com")), "Shelley Morgan");
        assert_eq!(name(&a(None, "bo@beta.io")), "bo");
        assert_eq!(name(&a(Some("\"Ana\""), "a@x.com")), "Ana");
    }

    #[test]
    fn external_attendees() {
        let meta = testutil::meta_with_attendees("me@example.com", &["me@example.com", "kim@example.com", "ana@acme.com", "joe@gmail.com"]);
        let ext: Vec<&str> = external(&meta).iter().map(|a| a.email.as_str()).collect();
        assert_eq!(ext, vec!["ana@acme.com", "joe@gmail.com"]);
        assert_eq!(external_domains(&meta), vec!["acme.com"]);
        let internal = testutil::meta_with_attendees("me@example.com", &["me@example.com", "kim@example.com"]);
        assert!(external(&internal).is_empty());
        // unknown owner: one domain = internal, mixed = external
        let unknown = testutil::meta_with_attendees("", &["a@x.com", "b@x.com"]);
        assert!(external(&unknown).is_empty());
        let mixed = testutil::meta_with_attendees("", &["a@x.com", "b@y.com"]);
        assert_eq!(external(&mixed).len(), 2);
        assert!(external(&testutil::meta("Ad hoc")).is_empty());
    }
}
