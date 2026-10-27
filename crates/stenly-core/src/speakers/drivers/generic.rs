//! Any app: elements labelled for screen readers, "<name> is speaking" /
//! "Speaking: <name>" (Slack huddles, and Meet when its classes change).

use super::{Capabilities, Driver};
use crate::speakers::ax::{AxWindow, Observation, clean_name};

pub struct Generic;

impl Driver for Generic {
    fn id(&self) -> &'static str {
        "other"
    }
    fn label(&self) -> &'static str {
        "Meeting app"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities { active_speaker: true, ..Default::default() }
    }
    fn claims(&self, _: &AxWindow) -> bool {
        true
    }
    fn observe(&self, w: &AxWindow) -> Observation {
        let mut o = Observation { app: "other".into(), ..Default::default() };
        w.root.walk(&mut |n| {
            for t in n.texts() {
                if t.len() > 80 {
                    continue;
                }
                let lower = t.to_lowercase();
                if let Some(suffix) = [" is speaking", " is talking"].into_iter().find(|s| lower.ends_with(s)) {
                    if let Some(name) = clean_name(&t[..t.len() - suffix.len()]) {
                        o.add_active(name);
                    }
                }
                for prefix in ["speaking:", "talking:", "now speaking:"] {
                    if lower.starts_with(prefix) {
                        if let Some(name) = clean_name(&t[prefix.len()..]) {
                            o.add_active(name);
                        }
                    }
                }
            }
        });
        o
    }
}
