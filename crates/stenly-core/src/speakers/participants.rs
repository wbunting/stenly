//! Who was in the meeting, from every source we have, one entry per person.
//!
//! Sources: the calendar invite (attendees, organiser), the meeting app's
//! tiles and participants list, the host it shows. The user is left out
//! ([`Me`]). Names from different sources are matched loosely ("Shelley"
//! on a tile, "Shelley Morgan <shelley@x>" on the invite), and the fullest
//! name wins. Naming steps pick from this list instead of each reading one
//! source on its own; future sources (a meeting's own participant report,
//! a CRM) only need to add entries here.

use super::{person_name, timeline};
use crate::calendar::CalendarEvent;
use crate::identity::Me;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Accepted / tentative / no answer on the invite.
    Invite,
    /// The invite's organiser.
    Organizer,
    /// A tile or participants-list row in the meeting app.
    App,
    /// The meeting app shows them as host.
    Host,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    pub name: String,
    pub email: Option<String>,
    pub sources: Vec<Source>,
}

impl Person {
    /// Seen in the meeting itself (not only invited).
    pub fn from_app(&self) -> bool {
        self.sources.iter().any(|s| matches!(s, Source::App | Source::Host))
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Participants {
    pub people: Vec<Person>,
    /// The meeting app showed who was there (so absent invitees didn't come).
    pub app_roster: bool,
}

impl Participants {
    pub fn gather(me: &Me, ev: Option<&CalendarEvent>, app: &timeline::Summary) -> Self {
        let mut p = Participants::default();
        // the meeting app first: it says who actually came
        for n in &app.roster {
            if !me.is(n) {
                p.add(n, None, Source::App);
            }
        }
        p.app_roster = !app.roster.is_empty();
        if let Some(h) = app.host.as_deref().filter(|h| !me.is(h)) {
            p.add(h, None, Source::Host);
        }
        if let Some(ev) = ev {
            for a in &ev.attendees {
                if me.is_email(&a.email) || a.status.as_deref() == Some("DECLINED") {
                    continue;
                }
                let n = person_name(a);
                if me.is(&n) {
                    continue;
                }
                p.add(&n, Some(&a.email), Source::Invite);
            }
            if let Some(o) = ev.organizer.as_ref().filter(|o| !me.is_email(&o.email)) {
                let n = person_name(o);
                if !me.is(&n) {
                    p.add(&n, Some(&o.email), Source::Organizer);
                }
            }
        }
        p
    }

    fn add(&mut self, name: &str, email: Option<&str>, src: Source) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let email = email.map(|e| e.trim().to_ascii_lowercase()).filter(|e| e.contains('@'));
        let hit = self.people.iter_mut().find(|p| email.is_some() && p.email == email || same_person(&p.name, name));
        match hit {
            Some(p) => {
                if name.split_whitespace().count() > p.name.split_whitespace().count() {
                    p.name = name.to_string();
                }
                if p.email.is_none() {
                    p.email = email;
                }
                if !p.sources.contains(&src) {
                    p.sources.push(src);
                }
            }
            None => self.people.push(Person { name: name.to_string(), email, sources: vec![src] }),
        }
    }

    /// People who were (probably) there: the meeting app's roster when it
    /// showed one, else everyone invited.
    pub fn present(&self) -> Vec<&Person> {
        if self.app_roster {
            self.people.iter().filter(|p| p.from_app()).collect()
        } else {
            self.people.iter().collect()
        }
    }

    /// Invitees the meeting app never showed (empty without a roster: then
    /// we can't tell who came).
    pub fn absent(&self) -> Vec<&Person> {
        if !self.app_roster {
            return vec![];
        }
        self.people.iter().filter(|p| !p.from_app()).collect()
    }

    /// The other person in a one-to-one, when the sources agree there was
    /// exactly one. An invite alone only counts when we know which attendee
    /// is the user (else "two attendees" could be the user plus one other).
    pub fn only_other(&self) -> Option<&Person> {
        let present = self.present();
        (present.len() == 1).then(|| present[0])
    }

    /// Names for transcript cues and the language model.
    pub fn names(&self) -> Vec<String> {
        self.present().iter().map(|p| p.name.clone()).collect()
    }

    pub fn host(&self) -> Option<&Person> {
        self.people.iter().find(|p| p.sources.contains(&Source::Host))
    }

    pub fn len(&self) -> usize {
        self.present().len()
    }

    pub fn is_empty(&self) -> bool {
        self.present().is_empty()
    }
}

impl Participants {
    /// The person a name (or address) refers to: same address, same name, or
    /// a first name of theirs ("Shelley" → "Shelley Morgan").
    pub fn find(&self, name: &str, email: Option<&str>) -> Option<&Person> {
        let email = email.map(|e| e.trim().to_ascii_lowercase());
        let present = self.present();
        present.iter().find(|p| email.is_some() && p.email == email).or_else(|| present.iter().find(|p| same_person(&p.name, name))).copied()
    }
}

/// Whether `spoken` ("Seth", "Bob") could be what people call `full`
/// ("Sathiren Vale", "Robert Reed"): a known nickname, the start of their
/// first name ("Sam" → "Samantha"), or the same initial. Only a hint: callers
/// also need the rest of the evidence (one person left, a name cue) to agree.
pub fn nickname_of(spoken: &str, full: &str) -> bool {
    let w = |s: &str| s.split_whitespace().next().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()).unwrap_or_default();
    let (a, b) = (w(spoken), w(full));
    if a.chars().count() < 2 || b.chars().count() < 2 || spoken.split_whitespace().count() > 1 {
        return false;
    }
    if a == b || b.starts_with(&a) {
        return true;
    }
    const PAIRS: &[(&str, &str)] = &[
        ("bill", "william"), ("will", "william"), ("liam", "william"), ("bob", "robert"), ("rob", "robert"), ("bobby", "robert"), ("dick", "richard"),
        ("rick", "richard"), ("jim", "james"), ("jimmy", "james"), ("jack", "john"), ("johnny", "john"), ("ted", "edward"), ("ed", "edward"),
        ("ned", "edward"), ("harry", "henry"), ("hank", "henry"), ("chuck", "charles"), ("charlie", "charles"), ("tony", "anthony"),
        ("peggy", "margaret"), ("maggie", "margaret"), ("meg", "margaret"), ("liz", "elizabeth"), ("beth", "elizabeth"), ("betty", "elizabeth"),
        ("kate", "katherine"), ("katie", "katherine"), ("kathy", "katherine"), ("sue", "susan"), ("mike", "michael"), ("mick", "michael"),
        ("dave", "david"), ("steve", "stephen"), ("steve", "steven"), ("andy", "andrew"), ("drew", "andrew"), ("alex", "alexander"),
        ("alex", "alexandra"), ("sasha", "alexander"), ("nick", "nicholas"), ("chris", "christopher"), ("matt", "matthew"), ("pat", "patrick"),
        ("pat", "patricia"), ("tom", "thomas"), ("tommy", "thomas"), ("joe", "joseph"), ("sam", "samuel"), ("ben", "benjamin"), ("dan", "daniel"),
        ("danny", "daniel"), ("greg", "gregory"), ("jeff", "jeffrey"), ("jen", "jennifer"), ("jenny", "jennifer"), ("abby", "abigail"),
    ];
    if PAIRS.iter().any(|(n, f)| *n == a && *f == b) {
        return true;
    }
    // same first letter and the spoken name's consonants, in order, in the
    // full one: "Seth" → "SaTHiren", not "SHeLLey"
    let cons = |w: &str| w.chars().skip(1).filter(|c| c.is_alphabetic() && !"aeiouy".contains(*c)).collect::<Vec<_>>();
    let (ca, cb) = (cons(&a), cons(&b));
    let mut it = cb.iter();
    a.chars().next() == b.chars().next() && !ca.is_empty() && ca.iter().all(|c| it.any(|x| x == c))
}

/// "Shelley" ~ "Shelley Morgan", "shelley morgan" ~ "Shelley Morgan", "S. Morgan" ≁ "Shelley Morgan".
pub fn same_person(a: &str, b: &str) -> bool {
    let n = |s: &str| s.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()).filter(|w| !w.is_empty()).collect::<Vec<_>>();
    let (a, b) = (n(a), n(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    // a bare first name matches a full name with that first name
    (a.len() == 1 || b.len() == 1) && a[0] == b[0] && a[0].chars().count() >= 3
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::Attendee;
    use crate::config::MeConfig;

    fn ev(owner: &str, people: &[(&str, &str)]) -> CalendarEvent {
        CalendarEvent {
            id: "x".into(),
            uid: "x".into(),
            title: "t".into(),
            start: chrono::Utc::now(),
            end: chrono::Utc::now(),
            link: None,
            organizer: None,
            attendees: people.iter().map(|(n, e)| Attendee { name: (!n.is_empty()).then(|| n.to_string()), email: e.to_string(), status: Some("ACCEPTED".into()) }).collect(),
            description: None,
            calendar: Some(owner.into()),
            declined: false,
            recurring: false,
            html_link: None,
            calendar_name: None,
            color: None,
        }
    }

    fn alex() -> Me {
        Me::from_config(&MeConfig { name: "Alex Rowan".into(), emails: vec!["alex@example.com".into(), "alex@work.example.org".into()], aliases: vec![], label: String::new(), auto: true })
    }

    fn app(roster: &[&str], host: Option<&str>) -> timeline::Summary {
        timeline::Summary { roster: roster.iter().map(|s| s.to_string()).collect(), host: host.map(Into::into), ..Default::default() }
    }

    #[test]
    fn ad_hoc_zoom_with_host_only() {
        // no invite, no tiles read, but Zoom showed the host
        let p = Participants::gather(&alex(), None, &app(&[], Some("Shelley Morgan")));
        assert_eq!(p.only_other().map(|p| p.name.as_str()), Some("Shelley Morgan"));
    }

    #[test]
    fn tiles_and_invite_merge() {
        let e = ev("alex@example.com", &[("", "alex@example.com"), ("Shelley Morgan", "shelley@example.com")]);
        let p = Participants::gather(&alex(), Some(&e), &app(&["Alex Rowan", "Shelley"], Some("Shelley")));
        assert_eq!(p.people.len(), 1, "{:?}", p.people);
        let s = &p.people[0];
        assert_eq!(s.name, "Shelley Morgan");
        assert_eq!(s.email.as_deref(), Some("shelley@example.com"));
        assert!(s.from_app());
        assert_eq!(p.host().map(|h| h.name.as_str()), Some("Shelley Morgan"));
    }

    #[test]
    fn invitees_who_did_not_come_are_dropped() {
        let e = ev("alex@example.com", &[("Shelley Morgan", "shelley@example.com"), ("Andy Reed", "andy@x.com")]);
        let p = Participants::gather(&alex(), Some(&e), &app(&["Alex Rowan", "Shelley Morgan"], None));
        assert_eq!(p.names(), vec!["Shelley Morgan"]);
        assert_eq!(p.absent().iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["Andy Reed"]);
        assert!(p.only_other().is_some());
        // without the app's roster, both invitees count
        let p = Participants::gather(&alex(), Some(&e), &app(&[], None));
        assert_eq!(p.len(), 2);
        assert!(p.absent().is_empty());
        assert!(p.only_other().is_none());
    }

    #[test]
    fn me_from_any_of_my_addresses() {
        // the invite went to my other address; the calendar owner is a third
        let e = ev("alex@personal.example.org", &[("Alex Rowan", "alex@example.com"), ("Andy", "andy@partner.example.org")]);
        let p = Participants::gather(&alex(), Some(&e), &app(&[], None));
        assert_eq!(p.names(), vec!["Andy"]);
    }

    #[test]
    fn unknown_user_keeps_everyone() {
        // nothing says who the user is: a two-person roster is not a 1:1 guess
        let p = Participants::gather(&Me::default(), None, &app(&["Riley Chen", "Jordan Avery"], None));
        assert_eq!(p.len(), 2);
        assert!(p.only_other().is_none());
    }

    #[test]
    fn loose_name_matching() {
        assert!(same_person("Shelley", "Shelley Morgan"));
        assert!(same_person("shelley morgan", "Shelley Morgan"));
        assert!(!same_person("Shelley Morgan", "Shelley Chen"));
        assert!(!same_person("Al", "Al Smith"), "too short to trust");
    }

    #[test]
    fn nicknames() {
        assert!(nickname_of("Seth", "Sathiren Vale"));
        assert!(nickname_of("Bob", "Robert Reed"));
        assert!(nickname_of("Sam", "Samantha Ruiz"));
        assert!(!nickname_of("Andy", "Shelley Morgan"));
        assert!(!nickname_of("Seth", "Shelley Morgan"), "same initial isn't enough");
        assert!(!nickname_of("Seth Vale", "Sathiren Vale"), "a full name isn't a nickname");
        let e = ev("alex@example.com", &[("", "andy@partner.example.org"), ("Shelley Morgan", "shelley@example.com")]);
        let p = Participants::gather(&alex(), Some(&e), &app(&[], None));
        assert_eq!(p.find("Shelley", None).map(|p| p.name.as_str()), Some("Shelley Morgan"));
        assert_eq!(p.find("Someone", Some("ANDY@partner.example.org")).map(|p| p.name.as_str()), Some("Andy"));
        assert!(p.find("Seth", None).is_none());
    }
}
