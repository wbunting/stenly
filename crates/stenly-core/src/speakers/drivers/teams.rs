//! Microsoft Teams: the desktop app (new Teams, `com.microsoft.teams2`) and
//! Teams in a browser, which render the same React UI.
//!
//! Where Teams puts things (verified on a live 3-person web call in Helium,
//! `teams-web-*.json`):
//! - Participant tiles are `AXMenuItem`s titled with the name and holding a
//!   `video-stream-renderer-wrapper`; the title may add "Unverified".
//! - While someone's audio is active a child of their tile carries the
//!   `vdi-frame-occlusion` class.
//! - The call's window / tab title is "<meeting> | <org> | Microsoft Teams".

use super::{Capabilities, Driver};
use crate::speakers::ax::{AxNode, AxWindow, Observation, clean_name, is_browser};

pub struct Teams;

/// Class Teams puts on a tile's audio indicator while that person speaks.
const SPEAKING_CLASS: &str = "vdi-frame-occlusion";

impl Driver for Teams {
    fn id(&self) -> &'static str {
        "teams"
    }
    fn label(&self) -> &'static str {
        "Microsoft Teams"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities { roster: true, active_speaker: true, me: false, host: false, topic: true }
    }
    fn claims(&self, w: &AxWindow) -> bool {
        matches!(w.app.as_str(), "com.microsoft.teams2" | "com.microsoft.teams") || (is_browser(&w.app) && w.title.to_lowercase().contains("microsoft teams"))
    }
    fn observe(&self, w: &AxWindow) -> Observation {
        let mut o = parse(&w.root);
        o.topic = topic(&w.title);
        o
    }
}

fn parse(root: &AxNode) -> Observation {
    let mut o = Observation { app: "teams".into(), ..Default::default() };
    root.walk(&mut |n| {
        if n.role() != "AXMenuItem" {
            return;
        }
        let Some(title) = n.title.as_deref() else { return };
        // A tile holds a video renderer; menu items in real menus don't.
        let is_tile = n.any(&|c| c.dom_id.as_deref() == Some("video-stream-renderer-wrapper"));
        if !is_tile {
            return;
        }
        // The tile's own label is the plain name; the title may add "Unverified".
        let label = n.find(&|c| c.role() == "AXStaticText" && c.value.as_deref().is_some_and(|v| title.starts_with(v.trim())));
        let name = label.and_then(|l| l.value.as_deref()).unwrap_or(title);
        let Some(name) = clean_name(name) else { return };
        if n.any(&|c| c.has_class(SPEAKING_CLASS)) {
            o.add_active(name);
        } else {
            o.add_roster(name);
        }
    });
    o
}

/// "Sync | Acme - Work | Microsoft Teams - Camera … - Helium" → "Sync".
fn topic(title: &str) -> Option<String> {
    let (head, _) = title.split_once(" | ")?;
    let head = head.trim();
    let generic = ["chat", "calendar", "activity", "teams", "meeting", "call", "microsoft teams"];
    (!head.is_empty() && !generic.contains(&head.to_lowercase().as_str())).then(|| head.to_string())
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    #[test]
    fn web_fixture_finds_tiles_and_speaker() {
        // Captured from a live Teams web call (names replaced). Sam is talking.
        let o = super::super::observe(&fixture("teams-web-speaking.json"));
        assert_eq!(o.app, "teams");
        assert_eq!(o.roster, vec!["Jordan Avery", "Sam Okafor", "Riley Chen"]);
        assert_eq!(o.active, vec!["Sam Okafor"]);
        assert_eq!(o.topic.as_deref(), Some("Sync"));
        // a moment later, nobody remote is talking
        let o = super::super::observe(&fixture("teams-web-quiet.json"));
        assert_eq!(o.roster.len(), 3);
        assert!(o.active.is_empty(), "{:?}", o.active);
    }

    #[test]
    fn ignores_real_menus() {
        let menu = with(node("AXMenu"), vec![AxNode { role: Some("AXMenuItem".into()), title: Some("Turn camera off".into()), ..Default::default() }]);
        assert!(Teams.observe(&window("com.microsoft.teams2", "Call | Microsoft Teams", menu)).is_empty());
    }

    #[test]
    fn topic_from_title() {
        assert_eq!(topic("Connect | Example Labs - Vuon | Microsoft Teams").as_deref(), Some("Connect"));
        assert_eq!(topic("Chat | Microsoft Teams"), None);
        assert_eq!(topic("Microsoft Teams"), None);
    }
}
