//! Decides *when* to nag. Pure state machine fed with "now", calendar events,
//! mic activity and recording state, so it is fully unit-testable.

use crate::calendar::{CalendarEvent, auto_record};
use crate::config::Config;
use crate::platform::MicUser;
use chrono::{DateTime, Duration, Utc};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub enum Prompt {
    /// A calendar meeting is about to start (or just started).
    Upcoming(CalendarEvent),
    /// A meeting app grabbed the mic with no matching calendar event.
    AdHoc { app_id: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    ShowPrompt(Prompt),
    /// Start without asking (auto_record_titles matched).
    AutoStart(CalendarEvent),
    /// Recording should probably end (silence / past the scheduled end): ask.
    SuggestStop { reason: String },
    /// The call app hung up: stop now (no prompt; the call is clearly over).
    AutoStop { reason: String },
    /// A pending ad-hoc "you're on a call" prompt is stale (call ended): dismiss it.
    DismissPrompt,
}

#[derive(Default)]
pub struct Scheduler {
    prompted: HashSet<String>,
    adhoc_prompted_until: Option<DateTime<Utc>>,
    mic_released_since: Option<DateTime<Utc>>,
    saw_call_app: bool,
    /// The call app seen during this recording was a browser (Meet / Teams
    /// web): browsers briefly drop the mic on mute or device switches, so they
    /// get a longer grace than native call apps, which release it on hang-up.
    call_in_browser: bool,
    stop_suggested: bool,
}

pub struct Tick<'a> {
    pub now: DateTime<Utc>,
    pub events: &'a [CalendarEvent],
    pub mic_users: &'a [MicUser],
    pub recording: Option<RecordingInfo<'a>>,
}

pub struct RecordingInfo<'a> {
    pub event: Option<&'a CalendarEvent>,
    pub started: DateTime<Utc>,
    pub secs_since_voice: i64,
    /// Seconds since the call window of an app that keeps the mic after
    /// hanging up (Teams) last showed anyone; None if none has.
    pub call_window_gone_secs: Option<f64>,
}

/// The call window has shown nobody for this long: the call is over. Mid-call
/// blips (a tile re-layout, a view switch) last well under a second.
pub const CALL_WINDOW_GONE_SECS: f64 = 4.0;
/// ...and nobody has been heard for this long.
pub const CALL_ENDED_QUIET_SECS: i64 = 3;

fn is_meeting_app(cfg: &Config, u: &MicUser) -> bool {
    is_meeting_app_in(&cfg.detect.meeting_apps, u)
}

/// Whether `u` is one of `apps` (bundle ids / process names; helpers such as
/// `com.google.Chrome.helper` count as their app).
pub fn is_meeting_app_in(apps: &[String], u: &MicUser) -> bool {
    let id = u.app_id.to_lowercase();
    apps.iter().any(|a| {
        let a = a.to_lowercase();
        id == a || id.starts_with(&format!("{a}.")) || id.rsplit('/').next() == Some(a.as_str())
    })
}

/// Browser (or browser helper) by bundle id / process name.
pub fn is_browser(app_id: &str) -> bool {
    let id = app_id.to_lowercase();
    ["chrome", "chromium", "safari", "firefox", "thebrowser", "brave", "edgemac", "edge", "helium", "vivaldi", "opera"].iter().any(|b| id.contains(b))
}

impl Scheduler {
    /// Forget a prompt so it can be shown again (e.g. "remind me in 2 min").
    pub fn snooze(&mut self, prompt: &Prompt) {
        match prompt {
            Prompt::Upcoming(e) => {
                self.prompted.remove(&e.id);
            }
            Prompt::AdHoc { .. } => self.adhoc_prompted_until = None,
        }
    }

    pub fn tick(&mut self, cfg: &Config, t: Tick<'_>) -> Vec<Action> {
        let mut actions = Vec::new();
        let meeting_mic: Vec<&MicUser> = t.mic_users.iter().filter(|u| is_meeting_app(cfg, u)).collect();

        if let Some(rec) = &t.recording {
            self.adhoc_prompted_until = None;
            // ---- auto-stop
            if !self.stop_suggested {
                let mut reason = None;
                let mut hard_stop = false;
                let silence = cfg.detect.auto_stop_silence_secs as i64;
                if silence > 0 && rec.secs_since_voice >= silence && (t.now - rec.started).num_seconds() >= silence {
                    reason = Some(format!("no audio for {} min", silence / 60));
                }
                let released = cfg.detect.auto_stop_mic_released_secs as i64;
                let released = if self.call_in_browser { released.max(4) } else { released };
                if released > 0 {
                    if meeting_mic.is_empty() {
                        let since = *self.mic_released_since.get_or_insert(t.now);
                        // Only meaningful if a call app had the mic at some point (it is
                        // normal not to see one for in-person meetings).
                        if (t.now - since).num_seconds() >= released && self.saw_call_app {
                            reason = Some("call ended".into());
                            hard_stop = true;
                        }
                    } else {
                        self.saw_call_app = true;
                        self.call_in_browser = meeting_mic.iter().all(|u| is_browser(&u.app_id));
                        self.mic_released_since = None;
                    }
                }
                // Some apps keep the mic after hanging up (Teams); the call
                // window closing is the other sure sign. Also require a
                // moment of quiet, so a window that blanks while screen
                // sharing doesn't end a call people are still talking in.
                if rec.call_window_gone_secs.is_some_and(|g| g >= CALL_WINDOW_GONE_SECS) && rec.secs_since_voice >= CALL_ENDED_QUIET_SECS {
                    reason = Some("call ended".into());
                    hard_stop = true;
                }
                if let Some(ev) = rec.event
                    && !hard_stop
                    && t.now > ev.end + Duration::minutes(10) && rec.secs_since_voice > 60 {
                        reason = Some("meeting time is over".into());
                    }
                if let Some(reason) = reason {
                    self.stop_suggested = true;
                    actions.push(if hard_stop { Action::AutoStop { reason } } else { Action::SuggestStop { reason } });
                }
            }
            return actions;
        }
        self.stop_suggested = false;
        self.saw_call_app = false;
        self.call_in_browser = false;
        self.mic_released_since = None;

        // ---- calendar prompts
        let lead = Duration::seconds(cfg.calendar.remind_before_secs.max(0));
        let mut calendar_hit = false;
        for ev in t.events {
            let window_open = t.now >= ev.start - lead && t.now < ev.start + Duration::minutes(10).min(ev.end - ev.start);
            if !window_open {
                continue;
            }
            calendar_hit = true;
            if self.prompted.insert(ev.id.clone()) {
                actions.push(if auto_record(&cfg.calendar, ev) {
                    Action::AutoStart(ev.clone())
                } else {
                    Action::ShowPrompt(Prompt::Upcoming(ev.clone()))
                });
            }
        }

        // ---- ad-hoc calls (mic grabbed by a meeting app, nothing on the calendar)
        if cfg.detect.ad_hoc_calls && !calendar_hit {
            if let Some(u) = meeting_mic.first() {
                if self.adhoc_prompted_until.is_none_or(|until| t.now >= until) {
                    // don't re-prompt for the same continuous call
                    self.adhoc_prompted_until = Some(DateTime::<Utc>::MAX_UTC);
                    actions.push(Action::ShowPrompt(Prompt::AdHoc { app_id: u.app_id.clone() }));
                }
            } else if self.adhoc_prompted_until == Some(DateTime::<Utc>::MAX_UTC) {
                // call ended; close its prompt and allow one for the next call after a grace period
                self.adhoc_prompted_until = Some(t.now + Duration::seconds(30));
                actions.push(Action::DismissPrompt);
            }
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting_url::{MeetingLink, Provider};
    use chrono::TimeZone;

    fn ev(id: &str, start: DateTime<Utc>, mins: i64) -> CalendarEvent {
        CalendarEvent {
            id: id.into(),
            uid: id.into(),
            title: format!("Event {id}"),
            start,
            end: start + Duration::minutes(mins),
            link: Some(MeetingLink { provider: Provider::Zoom, url: "https://zoom.us/j/1".into() }),
            organizer: None,
            attendees: vec![],
            description: None,
            calendar: None,
            declined: false,
            recurring: false,
            html_link: None,
            calendar_name: None,
            color: None,
        }
    }

    #[test]
    fn prompts_once_in_window() {
        let cfg = Config::default();
        let mut s = Scheduler::default();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 20, 10, 0, 0).unwrap();
        let events = [ev("a", t0, 30)];
        let at = |secs: i64| t0 + Duration::seconds(secs);
        let tick = |s: &mut Scheduler, now| s.tick(&cfg, Tick { now, events: &events, mic_users: &[], recording: None });
        assert!(tick(&mut s, at(-120)).is_empty());
        assert!(matches!(tick(&mut s, at(-59)).as_slice(), [Action::ShowPrompt(Prompt::Upcoming(_))]));
        assert!(tick(&mut s, at(-30)).is_empty());
        assert!(tick(&mut s, at(700)).is_empty());
    }

    #[test]
    fn adhoc_and_autostop() {
        let cfg = Config::default();
        let mut s = Scheduler::default();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 20, 10, 0, 0).unwrap();
        let zoom = [MicUser { pid: Some(1), app_id: "us.zoom.xos".into() }];
        let other = [MicUser { pid: Some(2), app_id: "com.apple.VoiceMemos".into() }];
        assert!(s.tick(&cfg, Tick { now: t0, events: &[], mic_users: &other, recording: None }).is_empty());
        let a = s.tick(&cfg, Tick { now: t0, events: &[], mic_users: &zoom, recording: None });
        assert_eq!(a, vec![Action::ShowPrompt(Prompt::AdHoc { app_id: "us.zoom.xos".into() })]);
        assert!(s.tick(&cfg, Tick { now: t0 + Duration::seconds(5), events: &[], mic_users: &zoom, recording: None }).is_empty());

        // now recording; zoom hangs up → stop ~1 s later (native apps release the
        // mic exactly when the call ends)
        let rec = || Some(RecordingInfo { event: None, started: t0, secs_since_voice: 0, call_window_gone_secs: None });
        let ms = |m: i64| t0 + Duration::milliseconds(m);
        assert!(s.tick(&cfg, Tick { now: ms(60_000), events: &[], mic_users: &zoom, recording: rec() }).is_empty());
        assert!(s.tick(&cfg, Tick { now: ms(70_000), events: &[], mic_users: &[], recording: rec() }).is_empty());
        assert!(s.tick(&cfg, Tick { now: ms(70_500), events: &[], mic_users: &[], recording: rec() }).is_empty());
        let a = s.tick(&cfg, Tick { now: ms(71_000), events: &[], mic_users: &[], recording: rec() });
        assert!(matches!(a.as_slice(), [Action::AutoStop { .. }]), "{a:?}");
    }

    #[test]
    fn browser_call_gets_longer_grace() {
        let cfg = Config::default();
        let mut s = Scheduler::default();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 20, 10, 0, 0).unwrap();
        let meet = [MicUser { pid: Some(3), app_id: "com.google.Chrome.helper".into() }];
        let rec = || Some(RecordingInfo { event: None, started: t0, secs_since_voice: 0, call_window_gone_secs: None });
        let at = |sec: i64| t0 + Duration::seconds(sec);
        assert!(s.tick(&cfg, Tick { now: at(10), events: &[], mic_users: &meet, recording: rec() }).is_empty());
        // a 2 s blip (mute / device switch) must not end the recording
        assert!(s.tick(&cfg, Tick { now: at(20), events: &[], mic_users: &[], recording: rec() }).is_empty());
        assert!(s.tick(&cfg, Tick { now: at(22), events: &[], mic_users: &[], recording: rec() }).is_empty());
        assert!(s.tick(&cfg, Tick { now: at(23), events: &[], mic_users: &meet, recording: rec() }).is_empty());
        // a real hang-up does, after 4 s
        assert!(s.tick(&cfg, Tick { now: at(30), events: &[], mic_users: &[], recording: rec() }).is_empty());
        let a = s.tick(&cfg, Tick { now: at(34), events: &[], mic_users: &[], recording: rec() });
        assert!(matches!(a.as_slice(), [Action::AutoStop { .. }]), "{a:?}");
    }

    #[test]
    fn call_window_closing_stops_even_if_the_app_keeps_the_mic() {
        // Teams keeps the mic after hanging up; its call window closes.
        let cfg = Config::default();
        let mut s = Scheduler::default();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 20, 10, 0, 0).unwrap();
        let teams = [MicUser { pid: Some(4), app_id: "com.microsoft.teams2".into() }];
        let rec = |gone: Option<f64>, quiet: i64| Some(RecordingInfo { event: None, started: t0, secs_since_voice: quiet, call_window_gone_secs: gone });
        let at = |sec: i64| t0 + Duration::seconds(sec);
        assert!(s.tick(&cfg, Tick { now: at(10), events: &[], mic_users: &teams, recording: rec(None, 0) }).is_empty());
        // a tile re-layout blanks the window for a moment: keep going
        assert!(s.tick(&cfg, Tick { now: at(20), events: &[], mic_users: &teams, recording: rec(Some(0.5), 5) }).is_empty());
        // window blank while people are still talking (screen share): keep going
        assert!(s.tick(&cfg, Tick { now: at(25), events: &[], mic_users: &teams, recording: rec(Some(10.0), 0) }).is_empty());
        let a = s.tick(&cfg, Tick { now: at(30), events: &[], mic_users: &teams, recording: rec(Some(CALL_WINDOW_GONE_SECS), CALL_ENDED_QUIET_SECS) });
        assert!(matches!(a.as_slice(), [Action::AutoStop { .. }]), "{a:?}");
    }

    #[test]
    fn browser_bundle_matches() {
        let cfg = Config::default();
        assert!(is_meeting_app(&cfg, &MicUser { pid: None, app_id: "com.google.Chrome.helper".into() }));
        assert!(is_meeting_app(&cfg, &MicUser { pid: None, app_id: "/usr/lib/firefox/firefox".into() }));
        assert!(!is_meeting_app(&cfg, &MicUser { pid: None, app_id: "com.apple.Siri".into() }));
        // Teams / Meet in Helium (its helper processes hold the mic)
        assert!(is_meeting_app(&cfg, &MicUser { pid: None, app_id: "net.imput.helium.helper".into() }));
        assert!(is_browser("net.imput.helium.helper"));
    }
}
