//! Google sign-in and calendar sync against a local mock OAuth + Calendar server.
#[path = "support/mock_google.rs"]
mod mock_google;

use chrono::{Duration, Utc};
use mock_google::{CLIENT_ID, CLIENT_SECRET, Mock};
use std::sync::Arc;
use stenly_core::calendar::{self, CalendarEvent};
use stenly_core::config::CalendarConfig;
use stenly_core::google::{self, Client, Credentials, Endpoints, MemoryStore, TokenStore};
use stenly_core::meeting_url::Provider;

fn creds() -> Option<Credentials> {
    Some(Credentials {
        client_id: CLIENT_ID.into(),
        client_secret: CLIENT_SECRET.into(),
    })
}

fn setup() -> (Mock, Client, Arc<MemoryStore>) {
    let mock = Mock::start();
    mock.seed(Utc::now());
    let store = Arc::new(MemoryStore::default());
    let client = Client::new(creds(), Endpoints::mock(&mock.base), store.clone());
    (mock, client, store)
}

/// Sign in, with a thread playing the user's browser: it opens the consent
/// URL, follows the mock's redirect to our loopback listener, and returns.
fn sign_in(client: &Client) -> anyhow::Result<String> {
    client.sign_in(
        &|url| {
            let url = url.to_string();
            std::thread::spawn(move || {
                let agent: ureq::Agent = ureq::Agent::config_builder()
                    .http_status_as_error(false)
                    .max_redirects(5)
                    .build()
                    .into();
                let _ = agent.get(&url).call();
            });
        },
        &|| false,
    )
}

fn window() -> (chrono::DateTime<Utc>, chrono::DateTime<Utc>) {
    let now = Utc::now();
    (now - Duration::hours(1), now + Duration::days(1))
}

fn titles(evs: &[CalendarEvent]) -> Vec<&str> {
    evs.iter().map(|e| e.title.as_str()).collect()
}

#[test]
fn sign_in_with_pkce_stores_tokens_and_lists_calendars() {
    let (mock, client, store) = setup();
    let email = sign_in(&client).unwrap();
    assert_eq!(email, "avery@example.com");

    mock.with(|s| {
        let a = &s.auth_requests[0];
        assert_eq!(
            a["scope"],
            "https://www.googleapis.com/auth/calendar.readonly"
        );
        assert_eq!(a["code_challenge_method"], "S256");
        assert_eq!(a["access_type"], "offline");
        assert!(a["prompt"].contains("select_account"));
        assert_eq!(s.token_requests, ["authorization_code"]);
    });
    let saved = store.load();
    assert_eq!(saved.len(), 1);
    assert!(saved[0].refresh_token.starts_with("rt-") && saved[0].access_token.starts_with("at-"));

    let (cals, errors) = client.list_calendars();
    assert!(errors.is_empty(), "{errors:?}");
    // The free/busy-only calendar is not offered.
    let names: Vec<_> = cals.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["avery@example.com", "Family"]);
    assert!(cals[0].primary && cals[0].id == google::key("avery@example.com", "avery@example.com"));
    assert_eq!(cals[1].account.as_deref(), Some("avery@example.com"));
    assert_eq!(cals[1].color.as_deref(), Some("#33b679"));
}

#[test]
fn fetches_events_with_attendees_and_conference_links() {
    let (_mock, client, _store) = setup();
    sign_in(&client).unwrap();
    let (from, to) = window();

    // Nothing selected: the primary calendar only.
    let (evs, errors) = client.fetch_events(&[], from, to);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(titles(&evs), ["Design review", "Vendor call (Zoom)"]);

    let review = &evs[0];
    assert_eq!(review.link.as_ref().unwrap().provider, Provider::GoogleMeet);
    assert_eq!(
        review.link.as_ref().unwrap().url,
        "https://meet.google.com/abc-defg-hij"
    );
    assert_eq!(review.organizer.as_ref().unwrap().display(), "Sam Example");
    // resource rooms are not people
    assert_eq!(review.attendees.len(), 2);
    assert_eq!(
        review.description.as_deref(),
        Some("Agenda: <b>mockups</b>")
    );
    assert_eq!(review.calendar.as_deref(), Some("avery@example.com"));
    assert_eq!(
        review.html_link.as_deref(),
        Some("https://calendar.google.com/calendar/event?eid=g1")
    );
    assert_eq!(review.calendar_name.as_deref(), Some("avery@example.com"));
    assert_eq!(review.color.as_deref(), Some("#039be5"));
    // from the location field
    assert_eq!(evs[1].link.as_ref().unwrap().provider, Provider::Zoom);

    // Toggling on the second calendar adds its events (a Teams link in the description).
    let sel = [
        google::key("avery@example.com", "avery@example.com"),
        google::key("avery@example.com", "family@group.calendar.example.com"),
    ];
    let (evs, _) = client.fetch_events(&sel, from, to);
    assert_eq!(
        titles(&evs),
        [
            "Design review",
            "Vendor call (Zoom)",
            "Family dinner planning"
        ]
    );
    assert_eq!(evs[2].link.as_ref().unwrap().provider, Provider::Teams);

    // Toggling the primary off leaves only the other one.
    let (evs, _) = client.fetch_events(&sel[1..], from, to);
    assert_eq!(titles(&evs), ["Family dinner planning"]);
}

#[test]
fn two_accounts_feed_the_same_pipeline() {
    let (_mock, client, store) = setup();
    assert_eq!(sign_in(&client).unwrap(), "avery@example.com");
    assert_eq!(sign_in(&client).unwrap(), "avery.work@example.org");
    assert_eq!(store.load().len(), 2);
    // Signing the same account in again replaces it instead of duplicating it.
    assert_eq!(sign_in(&client).unwrap(), "avery@example.com");
    assert_eq!(store.load().len(), 2);

    let (cals, errors) = client.list_calendars();
    assert!(errors.is_empty());
    assert_eq!(cals.len(), 4);

    let (from, to) = window();
    let (evs, errors) = client.fetch_events(&[], from, to);
    assert!(errors.is_empty(), "{errors:?}");
    let mut t = titles(&evs);
    t.sort();
    // all-day events are skipped; the declined one is kept but marked
    assert_eq!(
        t,
        [
            "Declined offsite",
            "Design review",
            "Vendor call (Zoom)",
            "Weekly planning"
        ]
    );
    let declined = evs.iter().find(|e| e.title == "Declined offsite").unwrap();
    assert!(declined.declined && declined.calendar.as_deref() == Some("avery.work@example.org"));
    let weekly = evs.iter().find(|e| e.title == "Weekly planning").unwrap();
    assert!(weekly.recurring);
    assert_eq!(weekly.link.as_ref().unwrap().provider, Provider::Zoom);

    // The shared pipeline filters exactly as it does for EventKit and ICS:
    // declined events don't prompt; a solo block with no link and no guests wouldn't either.
    let cfg = CalendarConfig::default();
    let kept = calendar::filter_events(&cfg, evs);
    let mut t = titles(&kept);
    t.sort();
    assert_eq!(
        t,
        ["Design review", "Vendor call (Zoom)", "Weekly planning"]
    );

    // Selection is per account: an account with nothing selected is not read.
    let sel = [google::key(
        "avery.work@example.org",
        "team@group.calendar.example.com",
    )];
    let (evs, _) = client.fetch_events(&sel, from, to);
    assert_eq!(titles(&evs), ["Team retro"]);
}

#[test]
fn sign_out_forgets_the_account_and_revokes_the_grant() {
    let (mock, client, store) = setup();
    sign_in(&client).unwrap();
    let rt = store.load()[0].refresh_token.clone();
    client.sign_out("AVERY@example.com");
    assert!(store.load().is_empty() && client.accounts().is_empty());
    assert_eq!(mock.state.lock().unwrap().revoked, [rt]);
    let (from, to) = window();
    assert!(client.fetch_events(&[], from, to).0.is_empty());
}

#[test]
fn refreshes_expired_tokens_automatically() {
    let (mock, client, store) = setup();
    sign_in(&client).unwrap();
    let (from, to) = window();

    // Locally expired: refreshed before the first request.
    let mut accts = store.load();
    accts[0].expires_at = Utc::now().timestamp() - 10;
    let old = accts[0].access_token.clone();
    store.save(&accts).unwrap();
    let (evs, errors) = client.fetch_events(&[], from, to);
    assert!(errors.is_empty() && !evs.is_empty(), "{errors:?}");
    assert_ne!(store.load()[0].access_token, old);
    assert!(store.load()[0].expires_at > Utc::now().timestamp() + 3000);
    assert_eq!(
        mock.state.lock().unwrap().token_requests,
        ["authorization_code", "refresh_token"]
    );

    // Revoked server-side while still "valid" locally: a 401 triggers one refresh and a retry.
    mock.expire_access_tokens();
    let (evs, errors) = client.fetch_events(&[], from, to);
    assert!(errors.is_empty() && !evs.is_empty(), "{errors:?}");
    assert_eq!(
        mock.state
            .lock()
            .unwrap()
            .token_requests
            .iter()
            .filter(|t| *t == "refresh_token")
            .count(),
        2
    );
}

#[test]
fn a_revoked_sign_in_asks_to_sign_in_again_without_losing_the_account() {
    let (mock, client, store) = setup();
    sign_in(&client).unwrap();
    mock.revoke_refresh_tokens();
    let mut accts = store.load();
    accts[0].expires_at = 0;
    store.save(&accts).unwrap();
    let (from, to) = window();
    let (evs, errors) = client.fetch_events(&[], from, to);
    assert!(evs.is_empty());
    assert_eq!(errors.len(), 1);
    assert!(
        google::is_reauth(&errors[0]) && errors[0].contains("avery@example.com"),
        "{errors:?}"
    );
    assert!(!google::is_unreachable(&errors[0]));
    // Kept, so the UI can show it as "needs sign-in" rather than silently dropping it.
    assert_eq!(client.accounts(), ["avery@example.com"]);
}

#[test]
fn one_unreadable_calendar_does_not_hide_the_others() {
    let (mock, client, _store) = setup();
    sign_in(&client).unwrap();
    mock.with(|s| {
        s.unreadable
            .push("family@group.calendar.example.com".into())
    });
    let sel = [
        google::key("avery@example.com", "avery@example.com"),
        google::key("avery@example.com", "family@group.calendar.example.com"),
    ];
    let (from, to) = window();
    let (evs, errors) = client.fetch_events(&sel, from, to);
    assert_eq!(titles(&evs), ["Design review", "Vendor call (Zoom)"]);
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].contains("“Family”") && !google::is_unreachable(&errors[0]),
        "{errors:?}"
    );
}

#[test]
fn pages_through_calendars_and_events() {
    let (mock, client, _store) = setup();
    sign_in(&client).unwrap();
    mock.with(|s| s.page_size = 1);
    let (cals, _) = client.list_calendars();
    assert_eq!(
        cals.len(),
        2,
        "free/busy calendar on page 3 is still filtered out"
    );
    let (from, to) = window();
    let (evs, _) = client.fetch_events(&[], from, to);
    assert_eq!(evs.len(), 2);
}

#[test]
fn denied_consent_missing_scope_and_unconfigured_builds_fail_clearly() {
    // User clicks "Cancel" on Google's consent page.
    let (mock, client, store) = setup();
    mock.with(|s| s.accounts = vec!["!deny".into()]);
    let err = sign_in(&client).unwrap_err().to_string();
    assert!(err.contains("cancelled"), "{err}");
    assert!(store.load().is_empty());

    // Consent screen where the calendar checkbox was unticked.
    let (mock, client, store) = setup();
    mock.with(|s| s.grant_scope = Some("openid email".into()));
    let err = sign_in(&client).unwrap_err().to_string();
    assert!(err.contains("permission to see your calendars"), "{err}");
    assert!(store.load().is_empty());

    // A build without the Google client id.
    let mock = Mock::start();
    let client = Client::new(
        None,
        Endpoints::mock(&mock.base),
        Arc::new(MemoryStore::default()),
    );
    assert!(
        sign_in(&client)
            .unwrap_err()
            .to_string()
            .contains("isn't set up")
    );
    assert!(mock.state.lock().unwrap().auth_requests.is_empty());
}

#[test]
fn google_unreachable_is_reported_as_unreachable() {
    // A signed-in account (valid-looking token) whose server is a closed port.
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_base = format!("http://{}", dead.local_addr().unwrap());
    drop(dead);
    let store = Arc::new(MemoryStore::default());
    store
        .save(&[google::Account {
            email: "avery@example.com".into(),
            refresh_token: "rt".into(),
            access_token: "at".into(),
            expires_at: Utc::now().timestamp() + 3000,
        }])
        .unwrap();
    let offline = Client::new(creds(), Endpoints::mock(&dead_base), store);
    let (from, to) = window();
    let (evs, errors) = offline.fetch_events(&[], from, to);
    assert!(evs.is_empty());
    assert!(google::is_unreachable(&errors[0]), "{errors:?}");
}

#[test]
fn google_events_merge_with_ics_in_one_filtered_list() {
    let (_mock, client, _store) = setup();
    sign_in(&client).unwrap();
    let (from, to) = window();
    let ics = tempfile::NamedTempFile::new().unwrap();
    let start = (Utc::now() + Duration::hours(2)).format("%Y%m%dT%H%M%SZ");
    let end = (Utc::now() + Duration::hours(3)).format("%Y%m%dT%H%M%SZ");
    std::fs::write(
        ics.path(),
        format!("BEGIN:VCALENDAR\r\nX-WR-CALNAME:me@example.com\r\nBEGIN:VEVENT\r\nDTSTART:{start}\r\nDTEND:{end}\r\nUID:ics1\r\nSUMMARY:From the ICS feed\r\nLOCATION:https://zoom.us/j/999\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"),
    )
    .unwrap();
    // (`calendar::upcoming` reads the real credential store, which a test must
    // not depend on; it is the same two calls: ICS parsing plus `upcoming_with`.)
    let cfg = CalendarConfig {
        ics_urls: vec![ics.path().display().to_string()],
        ..Default::default()
    };
    let mut evs =
        calendar::events_from_ics(&calendar::fetch_ics(&cfg.ics_urls[0]).unwrap(), from, to);
    assert_eq!(titles(&evs), ["From the ICS feed"]);
    evs.extend(google::upcoming_with(&client, &cfg, from, to).0);
    let kept = calendar::filter_events(&cfg, evs);
    assert_eq!(
        titles(&kept),
        ["Design review", "From the ICS feed", "Vendor call (Zoom)"]
    );
}
