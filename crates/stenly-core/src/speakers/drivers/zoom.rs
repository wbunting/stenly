//! Zoom (native macOS app, and the web client by window title).
//!
//! Where Zoom puts things:
//! - Gallery tiles describe themselves as "<name>, Computer audio unmuted,
//!   Video on" (verified, `zoom-two-tiles.json`). The participants panel
//!   uses the same shape per row. A name may carry "(Host)", "(Co-host)",
//!   "(Me)" or "(Host, me)".
//! - The account button in Zoom's main window: "Zoom, <name>, In a Zoom
//!   Meeting, Pro account" — the signed-in user (verified).
//! - Speaker view: "Talking: <name>"; tiles may add "Active speaker". Not yet
//!   seen in a captured tree: in gallery view Zoom marks nobody.

use super::{Capabilities, Driver};
use crate::speakers::ax::{AxNode, AxWindow, Observation, clean_name, is_browser, name_tags};

pub struct Zoom;

/// Zoom's "Talking: <name>" label, in the languages Zoom ships.
const TALKING: &[&str] = &["Talking:", "Speaking:", "Sprechen:", "Spricht:", "Aan het spreken:", "Conversazione:", "Hablando:", "Falando:", "La parole est à :", "Mówi:", "Konuşuyor:"];
/// Labels Zoom puts on the active speaker's tile.
const ACTIVE: &[&str] = &["Active speaker", "Active Speaker", "Aktiver Sprecher", "Intervenant actif", "Orador activo", "Orador ativo", "Oratore attivo", "Actieve spreker"];

impl Driver for Zoom {
    fn id(&self) -> &'static str {
        "zoom"
    }
    fn label(&self) -> &'static str {
        "Zoom"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities { roster: true, active_speaker: true, me: true, host: true, topic: false }
    }
    fn claims(&self, w: &AxWindow) -> bool {
        w.app == "us.zoom.xos" || (is_browser(&w.app) && w.title.to_lowercase().contains("zoom"))
    }
    fn observe(&self, w: &AxWindow) -> Observation {
        // Zoom's main window lists other meetings ("Host: …" on history and
        // upcoming cards); only the meeting's own windows describe this one.
        let main_window = w.title.starts_with("Zoom Workplace") || w.title == "Zoom";
        parse(&w.root, !main_window)
    }
}

fn parse(root: &AxNode, meeting_window: bool) -> Observation {
    let mut o = Observation { app: "zoom".into(), ..Default::default() };
    root.walk(&mut |n| {
        for t in n.texts() {
            for p in TALKING {
                if let Some(rest) = t.strip_prefix(p) {
                    if let Some(name) = clean_name(rest) {
                        o.add_active(name);
                    }
                }
            }
            // "Host: Shelley Morgan" (meeting information)
            if let Some(rest) = t.strip_prefix("Host:").filter(|_| meeting_window) {
                if o.host.is_none() {
                    o.host = clean_name(rest);
                }
            }
        }
        // Account button in the main window: "Zoom, <name>, In a Zoom Meeting, …".
        if let Some(d) = n.desc.as_deref() {
            let parts: Vec<&str> = d.split(',').map(str::trim).collect();
            if parts.len() >= 3 && parts[0] == "Zoom" && parts[2].contains("Zoom") {
                o.me = clean_name(parts[1]).or(o.me.take());
                return;
            }
        }
        // Tile / participants row: "<name> (Host, me), Computer audio unmuted, …".
        if let Some(d) = n.desc.as_deref().or(n.title.as_deref()) {
            let parts: Vec<&str> = d.split(',').map(str::trim).collect();
            let (head, rest) = split_name(&parts);
            let is_tile = !rest.is_empty() && rest.iter().any(|p| p.to_lowercase().contains("audio") || p.to_lowercase().contains("video"));
            if is_tile {
                let (name, tags) = name_tags(&head);
                if let Some(name) = name {
                    if tags.iter().any(|t| t == "me") {
                        o.me = Some(name.clone());
                    }
                    if tags.iter().any(|t| t == "host") && o.host.is_none() {
                        o.host = Some(name.clone());
                    }
                    if rest.iter().any(|p| ACTIVE.contains(p)) {
                        o.add_active(name);
                    } else {
                        o.add_roster(name);
                    }
                }
            }
        }
    });
    o
}

/// "(Host, me)" contains a comma: ["Ann (Host", "me)", "Computer audio…"] →
/// ("Ann (Host, me)", ["Computer audio…"]).
fn split_name<'a>(parts: &[&'a str]) -> (String, Vec<&'a str>) {
    let Some(first) = parts.first() else { return (String::new(), vec![]) };
    if first.contains('(') && !first.contains(')') {
        if let Some(close) = parts.iter().position(|p| p.ends_with(')')) {
            return (parts[..=close].join(", "), parts[close + 1..].to_vec());
        }
    }
    (first.to_string(), parts[1..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    #[test]
    fn fixture_roster_and_self() {
        // Captured from a live two-person Zoom call (names replaced). Gallery
        // tiles carry no speaking flag; the account button names the user.
        let o = super::super::observe(&fixture("zoom-two-tiles.json"));
        assert_eq!(o.app, "zoom");
        assert_eq!(o.roster, vec!["Riley Chen", "Jordan Avery"]);
        assert!(o.active.is_empty());
        assert_eq!(o.me.as_deref(), Some("Jordan Avery"));
    }

    #[test]
    fn talking_label_and_gallery() {
        let root = with(
            node("AXWindow"),
            vec![
                text("Talking: Priya Natarajan"),
                desc("AXGroup", "Priya Natarajan, Computer audio unmuted, Video on"),
                desc("AXGroup", "Tom Ward (Me), Computer audio muted"),
            ],
        );
        let o = Zoom.observe(&window("us.zoom.xos", "Zoom Meeting", root));
        assert_eq!(o.active, vec!["Priya Natarajan"]);
        assert_eq!(o.roster, vec!["Priya Natarajan", "Tom Ward"]);
        assert_eq!(o.me.as_deref(), Some("Tom Ward"));
    }

    #[test]
    fn host_from_tags_and_meeting_info() {
        let root = with(
            node("AXWindow"),
            vec![
                desc("AXTabGroup", "Shelley Morgan (Host), Computer audio unmuted, Video on"),
                desc("AXTabGroup", "Alex Rowan (Co-host, me), Computer audio unmuted"),
            ],
        );
        let o = Zoom.observe(&window("us.zoom.xos", "Zoom Meeting", root));
        assert_eq!(o.host.as_deref(), Some("Shelley Morgan"));
        assert_eq!(o.me.as_deref(), Some("Alex Rowan"));
        assert_eq!(o.roster, vec!["Shelley Morgan", "Alex Rowan"]);

        let o = Zoom.observe(&window("us.zoom.xos", "Meeting Information", with(node("AXWindow"), vec![text("Host: Shelley Morgan")])));
        assert_eq!(o.host.as_deref(), Some("Shelley Morgan"));
        assert!(o.roster.is_empty());
        // the main window's meeting history is about other meetings
        let o = Zoom.observe(&window("us.zoom.xos", "Zoom Workplace", with(node("AXWindow"), vec![text("Host: Someone Else")])));
        assert_eq!(o.host, None);
    }
}
