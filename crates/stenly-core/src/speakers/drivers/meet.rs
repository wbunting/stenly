//! Google Meet in a browser.
//!
//! Where Meet puts things (not yet verified on a live call): participant
//! tiles carry known DOM classes; the self-view tile has its own class; a
//! speaking indicator element appears inside the speaker's tile. Meet's
//! class names change every few months; when they do, the generic driver
//! still catches "… is speaking" labels.

use super::{Capabilities, Driver};
use crate::speakers::ax::{AxNode, AxWindow, Observation, clean_name, is_browser, is_meet_pwa};

pub struct Meet;

const TILE: &[&str] = &["dkjMxf"];
const SELF: &[&str] = &["aGWPv"];
const SPEAKING: &[&str] = &["kssMZb"];

impl Driver for Meet {
    fn id(&self) -> &'static str {
        "meet"
    }
    fn label(&self) -> &'static str {
        "Google Meet"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities { roster: true, active_speaker: true, me: true, host: false, topic: false }
    }
    fn claims(&self, w: &AxWindow) -> bool {
        let t = w.title.to_lowercase();
        // The installed Meet web app is Meet whatever its window is called.
        is_meet_pwa(&w.app) || is_browser(&w.app) && (t.starts_with("meet") || t.contains("google meet") || t.contains("meet -"))
    }
    fn observe(&self, w: &AxWindow) -> Observation {
        parse(&w.root)
    }
}

fn parse(root: &AxNode) -> Observation {
    let mut o = Observation { app: "meet".into(), ..Default::default() };
    root.walk(&mut |n| {
        if !TILE.iter().any(|c| n.has_class(c)) {
            return;
        }
        // The name is the tile's first plain text that isn't a control label.
        let mut name: Option<String> = None;
        n.walk(&mut |c| {
            if name.is_none() && c.role() == "AXStaticText" {
                name = c.value.as_deref().or(c.title.as_deref()).and_then(clean_name);
            }
        });
        let Some(name) = name else { return };
        if SELF.iter().any(|c| n.any(&|x| x.has_class(c))) {
            o.me = Some(name.clone());
        }
        if SPEAKING.iter().any(|c| n.any(&|x| x.has_class(c))) {
            o.add_active(name);
        } else {
            o.add_roster(name);
        }
    });
    o
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;

    #[test]
    fn tiles_by_class() {
        let tile = |name: &str, speaking: bool, me: bool| {
            let mut kids = vec![text(name), text("more_vert")];
            if speaking {
                kids.push(class(node("AXGroup"), "kssMZb"));
            }
            if me {
                kids.push(class(node("AXGroup"), "aGWPv"));
            }
            with(class(node("AXGroup"), "dkjMxf"), kids)
        };
        let root = with(node("AXWebArea"), vec![tile("Ana Lima", true, false), tile("Will B", false, true)]);
        let o = super::super::observe(&[window("com.google.Chrome", "Meet - abc-defg-hij - Google Chrome", root)]);
        assert_eq!(o.app, "meet");
        assert_eq!(o.active, vec!["Ana Lima"]);
        assert_eq!(o.me.as_deref(), Some("Will B"));
    }

    /// The installed Meet web app: its own bundle id, and a window whose own
    /// title is empty (the platform layer fills it from the web area).
    #[test]
    fn installed_web_app_is_claimed_and_read() {
        let tile = |name: &str, speaking: bool| {
            let mut kids = vec![text(name)];
            if speaking {
                kids.push(class(node("AXGroup"), "kssMZb"));
            }
            with(class(node("AXGroup"), "dkjMxf"), kids)
        };
        let pwa = "com.google.Chrome.app.kjgfgldnnfoeklkmfkjfagphfepbbdan";
        for title in ["", "Meet - abc-defg-hij", "Meet"] {
            let root = with(node("AXWebArea"), vec![tile("Ana Lima", false), tile("Ben Ortiz", true)]);
            let w = window(pwa, title, root);
            assert_eq!(super::super::driver_for(&w).map(|d| d.id()), Some("meet"), "title {title:?}");
            let o = super::super::observe(&[w]);
            assert_eq!((o.app.as_str(), o.active.as_slice(), o.roster.len()), ("meet", &["Ben Ortiz".to_string()][..], 2));
        }
        // Another web app is not claimed just for being a web app.
        let other = window("com.google.Chrome.app.mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm", "", node("AXWebArea"));
        assert!(super::super::driver_for(&other).is_none());
    }
}
