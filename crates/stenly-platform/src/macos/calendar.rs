//! EventKit adapter; core receives only its platform-neutral calendar structs.

use anyhow::{Context, Result};
use block2::RcBlock;
use chrono::{DateTime, Utc};
use objc2::rc::{autoreleasepool, Retained};
use objc2::{runtime::Bool, AnyThread};
use objc2_event_kit::{
    EKAuthorizationStatus, EKEntityType, EKEvent, EKEventStatus, EKEventStore, EKParticipant,
    EKParticipantStatus,
};
use objc2_foundation::{NSArray, NSDate, NSError, NSString};
use stenly_core::calendar::{Attendee, CalendarEvent, CalendarInfo};
use stenly_core::meeting_url::find_meeting_link;
use stenly_core::platform::{CalendarSource, PermissionState};

pub struct EventKitCalendar;

fn store() -> Retained<EKEventStore> {
    unsafe { EKEventStore::init(EKEventStore::alloc()) }
}

fn string(s: &NSString) -> String {
    s.to_string()
}

fn maybe_string(s: Option<Retained<NSString>>) -> Option<String> {
    s.map(|s| string(&s)).filter(|s| !s.trim().is_empty())
}

fn utc(date: &NSDate) -> Option<DateTime<Utc>> {
    let seconds = date.timeIntervalSince1970();
    DateTime::from_timestamp(seconds.floor() as i64, (seconds.fract() * 1_000_000_000.) as u32)
}

#[derive(Clone, Debug)]
struct ParticipantData {
    name: Option<String>,
    address: String,
    status: Option<String>,
    is_current_user: bool,
}

impl ParticipantData {
    fn attendee(&self) -> Option<Attendee> {
        let email = self
            .address
            .strip_prefix("mailto:")
            .or_else(|| self.address.strip_prefix("MAILTO:"))
            .unwrap_or(&self.address)
            .trim()
            .to_string();
        (!email.is_empty()).then(|| Attendee {
            name: self.name.clone(),
            email,
            status: self.status.clone(),
        })
    }
}

fn participant(p: &EKParticipant) -> ParticipantData {
    let address = unsafe { p.URL() }
        .absoluteString()
        .map(|s| string(&s))
        .unwrap_or_default();
    let status = match unsafe { p.participantStatus() } {
        EKParticipantStatus::Accepted => Some("ACCEPTED".into()),
        EKParticipantStatus::Declined => Some("DECLINED".into()),
        EKParticipantStatus::Tentative => Some("TENTATIVE".into()),
        EKParticipantStatus::Pending => Some("NEEDS-ACTION".into()),
        _ => None,
    };
    ParticipantData {
        name: maybe_string(unsafe { p.name() }),
        address,
        status,
        is_current_user: unsafe { p.isCurrentUser() },
    }
}

#[derive(Debug)]
struct EventData {
    id: String,
    title: String,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    location: Option<String>,
    notes: Option<String>,
    url: Option<String>,
    organizer: Option<ParticipantData>,
    attendees: Vec<ParticipantData>,
    calendar_name: String,
    recurring: bool,
}

/// The pure mapper lets the interesting EventKit data-shape stay unit-testable
/// without a macOS Calendar account or Objective-C objects.
fn map_event_data(event: EventData) -> CalendarEvent {
    let me = event.attendees.iter().find(|person| person.is_current_user);
    CalendarEvent {
        id: format!("{}@{}", event.id, event.start.timestamp()),
        uid: event.id,
        title: if event.title.trim().is_empty() { "Untitled meeting".into() } else { event.title },
        start: event.start,
        end: event.end,
        link: find_meeting_link(
            [event.url.as_deref(), event.location.as_deref(), event.notes.as_deref()]
                .into_iter()
                .flatten(),
        ),
        organizer: event.organizer.and_then(|person| person.attendee()),
        attendees: event.attendees.iter().filter_map(ParticipantData::attendee).collect(),
        description: event.notes,
        // EventKit does not expose a calendar owner's email. Current-user
        // attendee is the closest equivalent for guest filtering and naming.
        calendar: me.and_then(|person| person.attendee().map(|person| person.email)),
        declined: me.is_some_and(|person| person.status.as_deref() == Some("DECLINED")),
        recurring: event.recurring,
        // EventKit's URL commonly is the video-conference URL, not a calendar UI URL.
        html_link: None,
        calendar_name: Some(event.calendar_name),
        color: None,
    }
}

fn map_event(event: &EKEvent) -> Option<CalendarEvent> {
    if unsafe { event.isAllDay() } || unsafe { event.status() } == EKEventStatus::Canceled {
        return None;
    }
    let start = utc(unsafe { event.startDate() }.as_ref())?;
    let end = utc(unsafe { event.endDate() }.as_ref())?;
    let calendar = unsafe { event.calendar() }?;
    let id = maybe_string(unsafe { event.calendarItemExternalIdentifier() })
        .unwrap_or_else(|| string(unsafe { event.calendarItemIdentifier() }.as_ref()));
    let attendees = unsafe { event.attendees() }
        .as_ref()
        .into_iter()
        .flat_map(|people| people.iter())
        .map(|person| participant(person.as_ref()))
        .collect();
    let url = unsafe { event.URL() }
        .and_then(|url| url.absoluteString())
        .map(|url| string(&url));
    Some(map_event_data(EventData {
        id,
        title: string(unsafe { event.title() }.as_ref()),
        start,
        end,
        location: maybe_string(unsafe { event.location() }),
        notes: maybe_string(unsafe { event.notes() }),
        url,
        organizer: unsafe { event.organizer() }.map(|person| participant(person.as_ref())),
        attendees,
        calendar_name: string(unsafe { calendar.title() }.as_ref()),
        recurring: unsafe { event.hasRecurrenceRules() } || unsafe { event.isDetached() },
    }))
}

fn permission_state() -> PermissionState {
    match unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) } {
        EKAuthorizationStatus::FullAccess => PermissionState::Granted,
        EKAuthorizationStatus::Denied
        | EKAuthorizationStatus::Restricted
        | EKAuthorizationStatus::WriteOnly => PermissionState::Denied,
        _ => PermissionState::NotDetermined,
    }
}

impl CalendarSource for EventKitCalendar {
    fn name(&self) -> &'static str {
        "EventKit"
    }

    fn permission(&self) -> PermissionState {
        permission_state()
    }

    fn request_permission(&self) {
        let store = store();
        let keep = store.clone();
        let completion = RcBlock::new(move |granted: Bool, error: *mut NSError| {
            if granted.as_bool() {
                tracing::info!("EventKit calendar access granted");
            } else if !error.is_null() {
                tracing::warn!(error = %unsafe { &*error }.localizedDescription(), "EventKit calendar access denied");
            }
            // Keep the EventKit store alive until the asynchronous completion.
            let _ = &keep;
        });
        unsafe { store.requestFullAccessToEventsWithCompletion(RcBlock::into_raw(completion)) };
    }

    fn calendars(&self) -> Result<Vec<CalendarInfo>> {
        anyhow::ensure!(
            permission_state() == PermissionState::Granted,
            "Calendar access is not allowed. Allow it in System Settings → Privacy & Security → Calendars."
        );
        autoreleasepool(|_| {
            let store = store();
            Ok(unsafe { store.calendarsForEntityType(EKEntityType::Event) }
                .iter()
                .map(|calendar| CalendarInfo {
                    id: string(unsafe { calendar.calendarIdentifier() }.as_ref()),
                    name: string(unsafe { calendar.title() }.as_ref()),
                    primary: false,
                    color: None,
                    account: None,
                })
                .collect())
        })
    }

    fn events(&self, ids: &[String], from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<CalendarEvent>> {
        anyhow::ensure!(
            permission_state() == PermissionState::Granted,
            "Calendar access is not allowed. Allow it in System Settings → Privacy & Security → Calendars."
        );
        autoreleasepool(|_| {
            let store = store();
            let all = unsafe { store.calendarsForEntityType(EKEntityType::Event) };
            let selected: Vec<Retained<_>> = all
                .iter()
                .filter(|calendar| {
                    ids.is_empty()
                        || ids.iter().any(|id| id == &string(unsafe { calendar.calendarIdentifier() }.as_ref()))
                })
                .collect();
            let refs: Vec<_> = selected.iter().map(|calendar| calendar.as_ref()).collect();
            let calendars = NSArray::from_slice(&refs);
            let start = NSDate::dateWithTimeIntervalSince1970(from.timestamp_millis() as f64 / 1_000.);
            let end = NSDate::dateWithTimeIntervalSince1970(to.timestamp_millis() as f64 / 1_000.);
            let predicate = unsafe { store.predicateForEventsWithStartDate_endDate_calendars(&start, &end, Some(&calendars)) };
            Ok::<Vec<CalendarEvent>, anyhow::Error>(
                unsafe { store.eventsMatchingPredicate(&predicate) }
                    .iter()
                    .filter_map(|event| map_event(event.as_ref()))
                    .collect(),
            )
        })
        .context("reading events from EventKit")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use stenly_core::meeting_url::Provider;

    #[test]
    fn maps_attendees_and_meeting_url_from_notes() {
        let event = map_event_data(EventData {
            id: "series-id".into(),
            title: "Planning".into(),
            start: Utc.with_ymd_and_hms(2026, 10, 23, 7, 15, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 10, 23, 7, 45, 0).unwrap(),
            location: Some("Room — https://meet.google.com/room-link".into()),
            notes: Some("Backup https://us06web.zoom.us/j/8652".into()),
            url: None,
            organizer: Some(ParticipantData { name: Some("Alice".into()), address: "mailto:alice@example.com".into(), status: None, is_current_user: false }),
            attendees: vec![
                ParticipantData { name: Some("Will".into()), address: "mailto:will@example.com".into(), status: Some("ACCEPTED".into()), is_current_user: true },
                ParticipantData { name: Some("Nicole".into()), address: "mailto:nicole@example.com".into(), status: Some("TENTATIVE".into()), is_current_user: false },
            ],
            calendar_name: "Work".into(),
            recurring: true,
        });
        assert_eq!(event.link.unwrap().provider, Provider::GoogleMeet);
        assert_eq!(event.attendees[1].name.as_deref(), Some("Nicole"));
        assert_eq!(event.attendees[1].email, "nicole@example.com");
        assert_eq!(event.organizer.unwrap().email, "alice@example.com");
        assert_eq!(event.calendar.as_deref(), Some("will@example.com"));
        assert!(event.recurring);
    }

    #[test]
    fn finds_meeting_url_in_notes_and_keeps_decline() {
        let event = map_event_data(EventData {
            id: "event".into(), title: "Call".into(), start: Utc::now(), end: Utc::now(),
            location: None, notes: Some("Join https://teams.microsoft.com/l/meetup-join/abc".into()), url: None,
            organizer: None,
            attendees: vec![ParticipantData { name: None, address: "mailto:will@example.com".into(), status: Some("DECLINED".into()), is_current_user: true }],
            calendar_name: "Personal".into(), recurring: false,
        });
        assert_eq!(event.link.unwrap().provider, Provider::Teams);
        assert!(event.declined);
    }
}
