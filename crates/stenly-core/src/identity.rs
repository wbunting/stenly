//! Who "Me" is: the person recording.
//!
//! Comes from Settings → You (`[me]`: name, emails, aliases). With `auto` on,
//! what Stenly can tell by itself is added for the meeting at hand: the
//! calendar account the invite came from (and the name it has on the
//! invite), and the name the meeting app says you're signed in as. Nothing
//! inferred is written back to the config; a hosted version could fill the
//! same fields from the account instead.

use crate::calendar::CalendarEvent;
use crate::config::MeConfig;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Me {
    /// Name to show for "Me" (config name, else the best inferred one).
    pub name: Option<String>,
    /// Lower-case addresses.
    pub emails: Vec<String>,
    /// Every name you go by: config name, aliases, inferred names.
    pub names: Vec<String>,
    /// Your tile in this meeting's roster, once found ([`Me::locate`]).
    pub tile: Option<String>,
    /// `[me] label` when set.
    label_cfg: Option<String>,
    /// The calendar account the invite came to (for a name when nothing else gives one).
    account: Option<String>,
}

impl Me {
    /// From the config only.
    pub fn from_config(cfg: &MeConfig) -> Self {
        let mut me = Me::default();
        let name = cfg.name.trim();
        if !name.is_empty() {
            me.name = Some(name.to_string());
            me.add_name(name);
        }
        for a in &cfg.aliases {
            me.add_name(a);
        }
        for e in &cfg.emails {
            me.add_email(e);
        }
        me.label_cfg = Some(cfg.label.trim().to_string()).filter(|l| !l.is_empty());
        me
    }

    /// Config plus what this meeting says about you (when `cfg.auto`).
    pub fn resolve(cfg: &MeConfig, ev: Option<&CalendarEvent>, app_me: Option<&str>) -> Self {
        let mut me = Self::from_config(cfg);
        if !cfg.auto {
            return me;
        }
        if let Some(n) = app_me.and_then(crate::speakers::ax::clean_name) {
            me.add_name(&n);
            me.tile = Some(n.clone());
            me.name.get_or_insert(n);
        }
        if let Some(ev) = ev {
            if let Some(owner) = ev.calendar.as_deref().filter(|c| c.contains('@')) {
                me.add_email(owner);
                me.account = Some(owner.trim().to_ascii_lowercase());
            }
            // your own entry on the invite carries your display name
            for a in ev.attendees.iter().chain(ev.organizer.iter()) {
                if me.is_email(&a.email) {
                    if let Some(n) = a.name.as_deref().map(|n| n.split('<').next().unwrap_or(n).trim().trim_matches('"')).filter(|n| !n.is_empty() && !n.contains('@')) {
                        me.add_name(n);
                        me.name.get_or_insert_with(|| n.to_string());
                    }
                }
            }
        }
        me
    }

    /// Just a name (tests, CLI).
    pub fn named(name: &str) -> Self {
        Self::from_config(&MeConfig { name: name.into(), ..Default::default() })
    }

    pub fn is_known(&self) -> bool {
        !self.names.is_empty() || self.tile.is_some()
    }

    pub fn display(&self) -> Option<&str> {
        self.name.as_deref().or(self.tile.as_deref())
    }

    /// What your own lines and action items are labelled: `[me] label`, else
    /// your first name (Settings → You, the invite, the meeting app, or the
    /// calendar account: alex@example.com → "Alex"), else "Me".
    pub fn label(&self) -> String {
        if let Some(l) = &self.label_cfg {
            return l.clone();
        }
        let first = |n: &str| {
            let n = crate::speakers::ax::clean_name(n).unwrap_or_else(|| n.to_string());
            n.split('(').next().unwrap_or(&n).split_whitespace().next().map(str::to_string)
        };
        if let Some(f) = self.display().and_then(first).filter(|f| f.chars().count() >= 2 && f.chars().all(|c| c.is_alphabetic() || c == '-' || c == '\'')) {
            return f;
        }
        if let Some(f) = self.account.as_deref().or(self.emails.first().map(String::as_str)).and_then(name_from_email) {
            return f;
        }
        crate::voices::SELF_NAME.to_string()
    }

    fn add_name(&mut self, n: &str) {
        let n = n.trim();
        if !n.is_empty() && !self.names.iter().any(|x| norm(x) == norm(n)) {
            self.names.push(n.to_string());
        }
    }

    fn add_email(&mut self, e: &str) {
        let e = e.trim().trim_matches(['<', '>']).to_ascii_lowercase();
        if e.contains('@') && !self.emails.contains(&e) {
            self.emails.push(e);
        }
    }

    pub fn is_email(&self, e: &str) -> bool {
        let e = e.trim().trim_matches(['<', '>']).to_ascii_lowercase();
        self.emails.contains(&e)
    }

    /// Whether a name a meeting app or invite shows is you. Once your tile in
    /// this meeting is known, only that exact name is.
    pub fn is(&self, shown: &str) -> bool {
        if let Some(t) = &self.tile {
            return norm(t) == norm(shown);
        }
        self.matches(shown)
    }

    /// Name-only match: exact (ignoring case, punctuation and decorations
    /// like "(Host)"), or "Alex R" for "Alex Rowan". A bare first name
    /// only counts when it is one of your aliases.
    fn matches(&self, shown: &str) -> bool {
        let s = norm(shown);
        if s.is_empty() {
            return false;
        }
        let st: Vec<&str> = s.split(' ').collect();
        self.names.iter().any(|n| {
            let n = norm(n);
            if n == s {
                return true;
            }
            let nt: Vec<&str> = n.split(' ').collect();
            // "alex r" ~ "alex rowan"
            st.len() == 2 && nt.len() >= 2 && st[0] == nt[0] && st[1].chars().count() == 1 && nt.last().is_some_and(|l| l.starts_with(st[1]))
        })
    }

    /// Find yourself in a meeting app's roster: the one entry that is you.
    /// Prefers an exact match when several look like you ("Alex" and "Alex Rowan").
    pub fn find_in<'a>(&self, roster: &'a [String]) -> Option<&'a String> {
        if let Some(t) = &self.tile {
            return roster.iter().find(|r| norm(r) == norm(t));
        }
        let hits: Vec<&String> = roster.iter().filter(|r| self.matches(r)).collect();
        match hits.len() {
            0 => None,
            1 => Some(hits[0]),
            _ => {
                // your full name beats an alias ("Alex Rowan" over "Alex")
                if let Some(full) = self.name.as_deref().and_then(|n| hits.iter().find(|r| norm(r) == norm(n))) {
                    return Some(*full);
                }
                let exact: Vec<&&String> = hits.iter().filter(|r| self.names.iter().any(|n| norm(n) == norm(r))).collect();
                (exact.len() == 1).then(|| *exact[0])
            }
        }
    }

    /// Pin your tile in this meeting (exact matching from here on).
    pub fn locate(&mut self, roster: &[String]) {
        if self.tile.is_none() {
            self.tile = self.find_in(roster).cloned();
        }
        if self.name.is_none() {
            self.name = self.tile.clone();
        }
    }
}

/// A first name from an address: "alex@example.com" → "Alex", "alex.rowan@x" →
/// "Alex". None for role or opaque addresses ("info@", "wb2@").
pub fn name_from_email(e: &str) -> Option<String> {
    let local = e.split('@').next()?.split('+').next()?;
    let first = local.split(['.', '_', '-']).next()?;
    const ROLES: &[&str] = &["info", "admin", "hello", "team", "contact", "support", "sales", "office", "mail", "me", "hi"];
    if first.chars().count() < 2 || !first.chars().all(|c| c.is_ascii_alphabetic()) || ROLES.contains(&first.to_ascii_lowercase().as_str()) {
        return None;
    }
    let mut c = first.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase())
}

/// "Alex Rowan (Host, me)" → "alex rowan".
fn norm(s: &str) -> String {
    let base = crate::speakers::ax::clean_name(s).unwrap_or_else(|| s.to_string());
    let base = base.split('(').next().unwrap_or(&base);
    base.chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::Attendee;

    fn cfg() -> MeConfig {
        MeConfig { name: "Alex Rowan".into(), emails: vec!["Alex@Example.com".into()], aliases: vec!["Alex".into(), "Alexander Rowan".into()], label: String::new(), auto: true }
    }

    fn r(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn matches_names_and_aliases() {
        let me = Me::from_config(&cfg());
        assert!(me.is("alex rowan"));
        assert!(me.is("Alex Rowan (Host)"));
        assert!(me.is("Alexander Rowan"));
        assert!(me.is("Alex R."));
        assert!(me.is("Alex"), "explicit alias");
        assert!(!me.is("Alex Smith"));
        assert!(!me.is("Shelley Morgan"));
        assert!(me.is_email("alex@example.com"));
        assert_eq!(me.display(), Some("Alex Rowan"));
        // a bare first name isn't you unless it's an alias
        let plain = Me::named("Alex Rowan");
        assert!(!plain.is("Alex"));
    }

    #[test]
    fn finds_self_in_roster() {
        let me = Me::from_config(&cfg());
        assert_eq!(me.find_in(&r(&["Shelley Morgan", "Alex Rowan"])).map(String::as_str), Some("Alex Rowan"));
        // "Alex" (alias) and "Alex Rowan": the exact full name wins
        assert_eq!(me.find_in(&r(&["Alex", "Alex Rowan", "Shelley Morgan"])).map(String::as_str), Some("Alex Rowan"));
        assert_eq!(me.find_in(&r(&["Shelley Morgan"])), None);
        // once located, only that tile is you (the other Alex isn't)
        let mut me = me;
        me.locate(&r(&["Alex", "Alex Rowan"]));
        assert!(me.is("Alex Rowan"));
        assert!(!me.is("Alex"));
    }

    #[test]
    fn label_from_config_invite_or_account() {
        assert_eq!(Me::from_config(&cfg()).label(), "Alex");
        assert_eq!(Me::from_config(&MeConfig { label: "Alexander".into(), ..cfg() }).label(), "Alexander");
        assert_eq!(Me::from_config(&MeConfig { label: "Me".into(), ..cfg() }).label(), "Me", "keeps the old label");
        assert_eq!(Me::default().label(), "Me");
        // nothing configured; the invite came to alex@example.com and has no display name for it
        let ev = CalendarEvent {
            id: "x".into(),
            uid: "x".into(),
            title: "t".into(),
            start: chrono::Utc::now(),
            end: chrono::Utc::now(),
            link: None,
            organizer: Some(Attendee { name: None, email: "shelley@example.com".into(), status: None }),
            attendees: vec![Attendee { name: None, email: "alex@example.com".into(), status: None }, Attendee { name: None, email: "andy@partner.example.org".into(), status: None }],
            description: None,
            calendar: Some("alex@example.com".into()),
            declined: false,
            recurring: false,
            html_link: None,
            calendar_name: None,
            color: None,
        };
        let me = Me::resolve(&MeConfig::default(), Some(&ev), None);
        assert_eq!(me.label(), "Alex");
        assert_eq!(name_from_email("info@x.com"), None);
        assert_eq!(name_from_email("sathiren.vale@guest.example.org").as_deref(), Some("Sathiren"));
    }

    #[test]
    fn auto_adds_calendar_account_and_app_name() {
        let ev = CalendarEvent {
            id: "x".into(),
            uid: "x".into(),
            title: "t".into(),
            start: chrono::Utc::now(),
            end: chrono::Utc::now(),
            link: None,
            organizer: None,
            attendees: vec![Attendee { name: Some("Jordan Avery".into()), email: "jordan@x.com".into(), status: None }],
            description: None,
            calendar: Some("jordan@x.com".into()),
            declined: false,
            recurring: false,
            html_link: None,
            calendar_name: None,
            color: None,
        };
        let empty = MeConfig::default();
        let me = Me::resolve(&empty, Some(&ev), None);
        assert_eq!(me.display(), Some("Jordan Avery"));
        assert!(me.is_email("jordan@x.com"));
        assert!(me.is("Jordan Avery"));
        assert_eq!(me.label(), "Jordan");
        // the meeting app's signed-in name pins the tile
        let me = Me::resolve(&empty, None, Some("J. Avery"));
        assert_eq!(me.tile.as_deref(), Some("J. Avery"));
        assert_eq!(me.label(), "Me", "an initial isn't a name to label lines with");
        // auto off: config only
        let me = Me::resolve(&MeConfig { auto: false, ..Default::default() }, Some(&ev), Some("J. Avery"));
        assert!(!me.is_known());
    }
}
