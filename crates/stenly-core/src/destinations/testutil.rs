//! Fixtures and a tiny in-process HTTP server for destination tests.

use super::Payload;
use crate::calendar::{Attendee, CalendarEvent};
use crate::platform::Channel;
use crate::session::{SessionMeta, SessionState};
use crate::summary::{ActionItem, Bullet, Section, Summary};
use crate::transcript::{Segment, Transcript};
use chrono::{Local, TimeZone, Utc};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub fn meta(title: &str) -> SessionMeta {
    SessionMeta {
        title: title.into(),
        started: Local.with_ymd_and_hms(2026, 10, 20, 10, 0, 0).unwrap(),
        ended: Some(Local.with_ymd_and_hms(2026, 10, 20, 10, 31, 0).unwrap()),
        event: None,
        state: SessionState::Done,
        note_path: None,
        error: None,
        routes: vec![],
        sends: vec![],
        shares: vec![],
        meeting_app: None,
    }
}

pub fn event(owner: &str, emails: &[&str]) -> CalendarEvent {
    CalendarEvent {
        id: "e1".into(),
        uid: "e1".into(),
        title: "Meeting".into(),
        start: Utc.with_ymd_and_hms(2026, 10, 20, 1, 0, 0).unwrap(),
        end: Utc.with_ymd_and_hms(2026, 10, 20, 1, 30, 0).unwrap(),
        link: None,
        organizer: None,
        attendees: emails
            .iter()
            .map(|e| Attendee { name: Some(display(e)), email: e.to_string(), status: Some("ACCEPTED".into()) })
            .collect(),
        description: None,
        calendar: (!owner.is_empty()).then(|| owner.to_string()),
        declined: false,
        recurring: false,
        html_link: None,
        calendar_name: None,
        color: None,
    }
}

/// "ana@acme.com" → "Ana".
fn display(email: &str) -> String {
    let local = email.split('@').next().unwrap_or(email);
    let mut c = local.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

pub fn meta_with_attendees(owner: &str, emails: &[&str]) -> SessionMeta {
    let mut m = meta("Acme sync");
    m.event = Some(event(owner, emails));
    m
}

pub fn summary() -> Summary {
    Summary {
        summary: "We agreed on the **pilot** scope.".into(),
        sections: vec![Section {
            heading: "Pilot".into(),
            bullets: vec![Bullet::Nested { text: "Start in November".into(), sub: vec!["Two teams".into()] }],
        }],
        action_items: vec![ActionItem { owner: Some("Me".into()), task: "Send the contract".into() }],
        tags: vec!["Sales".into()],
        ..Default::default()
    }
}

pub fn transcript() -> Transcript {
    Transcript { segments: vec![Segment::new(Channel::Mic, 1.0, 2.0, "Hello"), Segment::new(Channel::System, 3.0, 4.0, "Hi there")], in_person: false, ..Default::default() }
}

pub fn payload() -> Payload {
    let mut m = meta_with_attendees("me@example.com", &["me@example.com", "ana@acme.com", "bo@beta.io"]);
    m.title = "Acme pilot".into();
    Payload { dir: "/tmp/2026-10-20 1000 Acme pilot".into(), meta: m, transcript: transcript(), summary: Some(summary()) }
}

/// A finished session on disk (meta + transcript + summary).
pub fn write_session(dir: &Path) {
    let p = payload();
    p.meta.save(dir).unwrap();
    std::fs::write(dir.join("transcript.json"), serde_json::to_vec(&p.transcript).unwrap()).unwrap();
    std::fs::write(dir.join("summary.json"), serde_json::to_vec(&p.summary).unwrap()).unwrap();
}

#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Req {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

type Handler = dyn Fn(&Req) -> (u16, String) + Send + Sync;

/// Serves HTTP/1.1 on 127.0.0.1 with `handler`, recording every request.
pub struct MockServer {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Req>>>,
}

impl MockServer {
    pub fn start(handler: impl Fn(&Req) -> (u16, String) + Send + Sync + 'static) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests: Arc<Mutex<Vec<Req>>> = Arc::default();
        let handler: Arc<Handler> = Arc::new(handler);
        let reqs = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (handler, reqs) = (handler.clone(), reqs.clone());
                std::thread::spawn(move || serve(stream, &*handler, &reqs));
            }
        });
        Self { url, requests }
    }

    pub fn requests(&self) -> Vec<Req> {
        self.requests.lock().unwrap().clone()
    }
}

fn serve(stream: std::net::TcpStream, handler: &Handler, reqs: &Mutex<Vec<Req>>) {
    let mut writer = stream.try_clone().unwrap();
    let mut r = BufReader::new(stream);
    // one request per connection (the response says `Connection: close`)
    {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
        let mut headers = Vec::new();
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                headers.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        let len = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse::<usize>().ok()).unwrap_or(0);
        let mut body = vec![0; len];
        if r.read_exact(&mut body).is_err() {
            return;
        }
        let req = Req { method, path, headers, body: String::from_utf8_lossy(&body).to_string() };
        let (status, resp) = handler(&req);
        reqs.lock().unwrap().push(req);
        let out = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}", resp.len());
        let _ = writer.write_all(out.as_bytes());
        let _ = writer.flush();
    }
}
