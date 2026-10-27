//! One driver per meeting app.
//!
//! Every app shows the same things in different places: who is in the call,
//! who is talking, which tile is you, who the host is and what the meeting
//! is called. A driver knows where its app puts them and says which it can
//! read ([`Capabilities`]); the naming pipeline only relies on what the
//! active driver claims. Adding an app = a module here + an entry in
//! [`DRIVERS`].
//!
//! Status per app (verified = seen in a tree captured from a live call):
//!
//! | app     | roster | speaking | you | host | topic |
//! |---------|--------|----------|-----|------|-------|
//! | Teams   | ✓ verified | ✓ verified | – | – | ✓ (window title) |
//! | Zoom    | ✓ verified (gallery) | label/tile, unverified | ✓ verified | "(Host)" tag, unverified | – |
//! | Meet    | unverified | unverified | unverified | – | – |
//! | generic | – | screen-reader labels | – | – | – |
//!
//! `stenly-cli speakers dump <file>` during a call captures a tree to turn
//! "unverified" into a fixture.

pub mod generic;
pub mod meet;
pub mod teams;
pub mod zoom;

use super::ax::{AxWindow, Observation};

/// What a driver can read from its app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Everyone in the call (visible tiles, or the participants panel).
    pub roster: bool,
    /// Who is talking right now.
    pub active_speaker: bool,
    /// Which tile / account is the local user.
    pub me: bool,
    /// The host / organiser.
    pub host: bool,
    /// The meeting's topic.
    pub topic: bool,
}

pub trait Driver: Sync {
    /// Stable id written to `speakers.jsonl` ("zoom", "teams", …).
    fn id(&self) -> &'static str;
    /// Name for people ("Zoom").
    fn label(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    /// Whether this window belongs to the driver's app.
    fn claims(&self, w: &AxWindow) -> bool;
    /// What the window shows. Called only for windows the driver claims.
    fn observe(&self, w: &AxWindow) -> Observation;
}

/// App-specific drivers, most specific first; [`generic::Generic`] runs on
/// whatever none of them claims, and on claimed windows that came up empty.
pub static DRIVERS: &[&dyn Driver] = &[&zoom::Zoom, &teams::Teams, &meet::Meet];

/// The driver for a window, if an app-specific one claims it.
pub fn driver_for(w: &AxWindow) -> Option<&'static dyn Driver> {
    DRIVERS.iter().copied().find(|d| d.claims(w))
}

/// Look up a driver by id ("zoom").
pub fn by_id(id: &str) -> Option<&'static dyn Driver> {
    DRIVERS.iter().copied().chain(std::iter::once(&generic::Generic as &dyn Driver)).find(|d| d.id() == id)
}

/// Driver for an app that holds the mic (bundle id / process name), for
/// labelling the session before any window was read.
pub fn for_app_id(app_id: &str) -> Option<&'static dyn Driver> {
    let id = app_id.to_lowercase();
    if id.contains("zoom") {
        by_id("zoom")
    } else if id.contains("teams") {
        by_id("teams")
    } else {
        None
    }
}

/// Merge every window of one snapshot into one observation.
pub fn observe(windows: &[AxWindow]) -> Observation {
    let mut out = Observation::default();
    for w in windows {
        let d = driver_for(w);
        let mut o = d.map(|d| d.observe(w)).unwrap_or_default();
        if o.is_empty() && o.me.is_none() && o.host.is_none() {
            let g = generic::Generic.observe(w);
            if !g.is_empty() {
                o = g;
            }
        }
        if o.app.is_empty() {
            o.app = d.map(|d| d.id()).unwrap_or("other").to_string();
        }
        merge(&mut out, o);
    }
    out
}

fn merge(out: &mut Observation, o: Observation) {
    out.me = out.me.take().or(o.me);
    out.host = out.host.take().or(o.host);
    out.topic = out.topic.take().or(o.topic);
    if o.roster.is_empty() && o.active.is_empty() {
        return;
    }
    if out.app.is_empty() {
        out.app = o.app;
    }
    for n in o.roster {
        out.add_roster(n);
    }
    for n in o.active {
        out.add_active(n);
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::super::ax::{AxNode, AxWindow};

    pub fn node(role: &str) -> AxNode {
        AxNode { role: Some(role.into()), ..Default::default() }
    }
    pub fn text(v: &str) -> AxNode {
        AxNode { role: Some("AXStaticText".into()), value: Some(v.into()), ..Default::default() }
    }
    pub fn desc(role: &str, d: &str) -> AxNode {
        AxNode { role: Some(role.into()), desc: Some(d.into()), ..Default::default() }
    }
    pub fn with(mut n: AxNode, kids: Vec<AxNode>) -> AxNode {
        n.children = kids;
        n
    }
    pub fn class(mut n: AxNode, c: &str) -> AxNode {
        n.classes.push(c.into());
        n
    }
    pub fn window(app: &str, title: &str, root: AxNode) -> AxWindow {
        AxWindow { app: app.into(), title: title.into(), root }
    }
    pub fn fixture(name: &str) -> Vec<AxWindow> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ax").join(name);
        serde_json::from_slice(&std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    #[test]
    fn every_driver_declares_itself() {
        for d in DRIVERS {
            assert!(!d.id().is_empty() && !d.label().is_empty());
            assert!(d.capabilities() != Capabilities::default(), "{} reads nothing", d.id());
            assert_eq!(by_id(d.id()).map(|x| x.id()), Some(d.id()));
        }
        assert_eq!(for_app_id("us.zoom.xos").map(|d| d.id()), Some("zoom"));
        assert_eq!(for_app_id("com.microsoft.teams2").map(|d| d.id()), Some("teams"));
        assert!(for_app_id("com.google.Chrome").is_none(), "a browser could be anything");
    }

    #[test]
    fn windows_go_to_their_driver() {
        let zoom = window("us.zoom.xos", "Zoom Meeting", node("AXWindow"));
        let teams = window("com.microsoft.teams2", "Call", node("AXWindow"));
        let meet = window("com.google.Chrome", "Meet - abc-defg-hij - Google Chrome", node("AXWindow"));
        let teams_web = window("net.imput.helium", "Sync | Microsoft Teams - Helium", node("AXWindow"));
        let other = window("com.tinyspeck.slackmacgap", "Huddle", node("AXWindow"));
        assert_eq!(driver_for(&zoom).map(|d| d.id()), Some("zoom"));
        assert_eq!(driver_for(&teams).map(|d| d.id()), Some("teams"));
        assert_eq!(driver_for(&teams_web).map(|d| d.id()), Some("teams"));
        assert_eq!(driver_for(&meet).map(|d| d.id()), Some("meet"));
        assert!(driver_for(&other).is_none());
    }

    #[test]
    fn snapshot_merges_windows() {
        // Zoom's meeting window has the tiles, its main window the account
        let o = observe(&fixture("zoom-two-tiles.json"));
        assert_eq!(o.app, "zoom");
        assert_eq!(o.roster, vec!["Riley Chen", "Jordan Avery"]);
        assert_eq!(o.me.as_deref(), Some("Jordan Avery"));
    }

    #[test]
    fn generic_labels_fill_in_for_any_app() {
        let root = with(node("AXWindow"), vec![desc("AXGroup", "Maya Ito is speaking")]);
        let o = observe(&[window("com.tinyspeck.slackmacgap", "Huddle", root)]);
        assert_eq!(o.app, "other");
        assert_eq!(o.active, vec!["Maya Ito"]);
    }
}
