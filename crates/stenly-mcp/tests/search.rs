use chrono::{Duration, Local};
use stenly_core::{
    config::Config,
    platform::Channel,
    session::{SessionMeta, SessionState},
    transcript::{Segment, Transcript},
};
use stenly_mcp::store::Store;

#[test]
fn large_library_keeps_old_meetings_and_exact_transcript_positions() {
    let root = tempfile::tempdir().unwrap();
    let now = Local::now();
    for i in 0..605 {
        let dir = root.path().join(format!("session-{i:04}"));
        std::fs::create_dir(&dir).unwrap();
        let meta: SessionMeta = serde_json::from_value(serde_json::json!({
            "title": format!("Synthetic meeting {i:04}"), "started": now - Duration::days(i), "ended": now - Duration::days(i) + Duration::minutes(30),
            "event": null, "state": "Done", "note_path": null, "error": null, "routes": []
        })).unwrap();
        meta.save(&dir).unwrap();
        if i == 604 {
            let t = Transcript {
                segments: vec![
                    Segment::new(Channel::Mic, 0., 10., "ordinary opening"),
                    Segment::new(
                        Channel::System,
                        84.25,
                        90.,
                        "The cobalt migration has approval",
                    ),
                ],
                ..Default::default()
            };
            std::fs::write(dir.join("transcript.json"), serde_json::to_vec(&t).unwrap()).unwrap();
            std::fs::write(
                dir.join("summary.json"),
                r#"{"summary":"A durable archive for violet projects"}"#,
            )
            .unwrap();
        }
    }
    let mut cfg = Config::default();
    cfg.output.audio_dir = root.path().to_string_lossy().into();
    let store = Store::new(cfg);
    let all = store.meetings();
    assert_eq!(all.len(), 605);
    assert_eq!(all.last().unwrap().id, "session-0604");
    assert_eq!(all.last().unwrap().meta.state, SessionState::Done);
    let hit = store.search("COBALT migration", None, None, 50);
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].meeting_id, "session-0604");
    assert_eq!(hit[0].segment_index, Some(1));
    assert_eq!(hit[0].at.as_deref(), Some("01:24"));
    assert_eq!(store.search("violet", None, None, 50)[0].field, "summary");
    assert_eq!(store.search("0604", None, None, 50)[0].field, "title");
    assert!(store.search("", None, None, 50).is_empty());
    assert!(store.search("meeting", None, None, 0).is_empty());
    assert_eq!(store.search("meeting", None, None, 51).len(), 51);
    assert!(store.search("not-present", None, None, 50).is_empty());
    // Nothing is modified or indexed outside the chosen recording library.
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 605);
}

#[test]
fn attendees_are_searched_without_requiring_a_transcript() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("invite");
    std::fs::create_dir(&dir).unwrap();
    let now = Local::now();
    let meta: SessionMeta = serde_json::from_value(serde_json::json!({
        "title":"Synthetic invite", "started":now, "ended":now, "state":"Done", "note_path":null, "error":null,"routes":[],
        "event": {"id":"synthetic", "uid":"synthetic", "title":"Synthetic invite", "start":now, "end":now,
        "link":null,"organizer":null,"attendees":[{"name":"Avery Example", "email":"avery@example.test", "status":null}],
        "description":null,"calendar":null,"declined":false,"recurring":false}
    })).unwrap();
    meta.save(&dir).unwrap();
    let mut cfg = Config::default();
    cfg.output.audio_dir = root.path().to_string_lossy().into();
    let store = Store::new(cfg);
    let hits = store.search("AVERY", None, None, 50);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].field, "attendees");
    assert_eq!(hits[0].segment_index, None);
    assert_eq!(store.search("example.test", None, None, 50).len(), 1);
}

#[test]
fn superseded_scans_stop_between_files() {
    use std::cell::Cell;
    let root = tempfile::tempdir().unwrap();
    for i in 0..20 {
        let dir = root.path().join(format!("fixture-{i}"));
        std::fs::create_dir(&dir).unwrap();
        let meta: SessionMeta = serde_json::from_value(serde_json::json!({
            "title":"Synthetic", "started":Local::now(),"ended":null,"event":null,
            "state":"Done","note_path":null,"error":null,"routes":[]
        }))
        .unwrap();
        meta.save(&dir).unwrap();
    }
    let mut cfg = Config::default();
    cfg.output.audio_dir = root.path().to_string_lossy().into();
    let store = Store::new(cfg);
    let checks = Cell::new(0);
    let cancelled = || {
        let n = checks.get() + 1;
        checks.set(n);
        n > 3
    };
    assert!(store.meetings_cancellable(cancelled).is_empty());
    assert_eq!(checks.get(), 4);
    assert!(
        store
            .search_cancellable("synthetic", None, None, 50, || true)
            .is_empty()
    );
    assert_eq!(store.meetings().len(), 20);
}

#[test]
fn cancellation_checks_nonmatching_transcript_segments() {
    use std::cell::Cell;
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("long-transcript");
    std::fs::create_dir(&dir).unwrap();
    let meta: SessionMeta = serde_json::from_value(serde_json::json!({
        "title":"Synthetic", "started":Local::now(),"ended":null,"event":null,
        "state":"Done","note_path":null,"error":null,"routes":[]
    }))
    .unwrap();
    meta.save(&dir).unwrap();
    let t = Transcript {
        segments: (0..100)
            .map(|i| {
                Segment::new(
                    Channel::Mic,
                    i as f64,
                    i as f64 + 1.,
                    "ordinary unrelated words",
                )
            })
            .collect(),
        ..Default::default()
    };
    std::fs::write(dir.join("transcript.json"), serde_json::to_vec(&t).unwrap()).unwrap();
    let mut cfg = Config::default();
    cfg.output.audio_dir = root.path().to_string_lossy().into();
    let store = Store::new(cfg);
    let checks = Cell::new(0);
    let hits = store.search_cancellable("no-such-needle", None, None, 50, || {
        let n = checks.get() + 1;
        checks.set(n);
        n >= 10
    });
    assert!(hits.is_empty());
    // Entry + metadata/file-boundary checks alone are <10. Reaching exactly
    // ten proves even nonmatching transcript segments observe cancellation.
    assert_eq!(checks.get(), 10);
}
