//! Google Meet in a browser.
//!
//! Where Meet puts things (not yet verified on a live call): participant
//! tiles carry known DOM classes; the self-view tile has its own class; a
//! speaking indicator element appears inside the speaker's tile. Meet's
//! class names change every few months; when they do, the generic driver
//! still catches "… is speaking" labels.

use super::{Capabilities, Driver};
use crate::speakers::ax::{AxNode, AxWindow, Observation, clean_name, is_browser};

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
        is_browser(&w.app) && (t.starts_with("meet") || t.contains("google meet") || t.contains("meet -"))
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
}
