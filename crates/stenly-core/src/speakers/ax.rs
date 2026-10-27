//! A meeting app's window, read through Accessibility.
//!
//! The platform layer snapshots the windows of running meeting apps into
//! plain [`AxNode`] trees. Each app has a driver in
//! [`super::drivers`] that knows where that app shows the roster, the
//! active speaker, your own tile, the host and the meeting topic, and says
//! which of those it can read ([`super::drivers::Capabilities`]). Everything
//! here is pure so drivers are tested on captured dumps (`tests/fixtures/ax/`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AxNode {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, rename = "domId", skip_serializing_if = "Option::is_none")]
    pub dom_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub classes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<AxNode>,
}

impl AxNode {
    pub fn role(&self) -> &str {
        self.role.as_deref().unwrap_or("")
    }
    pub fn has_class(&self, c: &str) -> bool {
        self.classes.iter().any(|x| x == c)
    }
    /// Depth-first, self included.
    pub fn walk(&self, f: &mut dyn FnMut(&AxNode)) {
        f(self);
        for c in &self.children {
            c.walk(f);
        }
    }
    pub fn any(&self, pred: &dyn Fn(&AxNode) -> bool) -> bool {
        pred(self) || self.children.iter().any(|c| c.any(pred))
    }
    pub fn find(&self, pred: &dyn Fn(&AxNode) -> bool) -> Option<&AxNode> {
        if pred(self) {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(pred))
    }
    /// title, desc and value, non-empty.
    pub fn texts(&self) -> impl Iterator<Item = &str> {
        [&self.title, &self.desc, &self.value].into_iter().filter_map(|s| s.as_deref()).map(str::trim).filter(|s| !s.is_empty())
    }
}

/// One window of a meeting app, as captured by the platform layer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AxWindow {
    /// Bundle id (macOS) or app id.
    pub app: String,
    #[serde(default)]
    pub title: String,
    pub root: AxNode,
}

/// Snapshots meeting windows. Implemented per OS in `stenly-platform`.
pub trait MeetingAxSource: Send + Sync {
    /// Trees of the meeting windows open right now; empty when there are none
    /// or Accessibility isn't allowed.
    fn snapshot(&self) -> Vec<AxWindow>;
}

/// What one snapshot says.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    /// "teams", "zoom", "meet", "slack" or "other".
    pub app: String,
    /// Everyone whose tile is visible.
    pub roster: Vec<String>,
    /// Who is highlighted as speaking.
    pub active: Vec<String>,
    /// The local user's tile, when the app marks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub me: Option<String>,
    /// Who hosts / organised the meeting, when the app shows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The meeting's topic, when the app shows one (not a generic window title).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}

impl Observation {
    pub fn is_empty(&self) -> bool {
        self.roster.is_empty() && self.active.is_empty()
    }
    pub(crate) fn add_roster(&mut self, n: String) {
        if !self.roster.contains(&n) {
            self.roster.push(n);
        }
    }
    pub(crate) fn add_active(&mut self, n: String) {
        self.add_roster(n.clone());
        if !self.active.contains(&n) {
            self.active.push(n);
        }
    }
}

const BROWSERS: &[&str] = &[
    "com.google.Chrome",
    "com.google.Chrome.canary",
    "company.thebrowser.Browser",
    "company.thebrowser.dia",
    "net.imput.helium",
    "com.apple.Safari",
    "com.brave.Browser",
    "com.microsoft.edgemac",
    "org.mozilla.firefox",
    "com.vivaldi.Vivaldi",
    "com.operasoftware.Opera",
    "org.chromium.Chromium",
];

/// Apps whose windows the platform layer should snapshot.
pub fn meeting_app_ids() -> Vec<&'static str> {
    let mut v = vec!["us.zoom.xos", "com.microsoft.teams2", "com.microsoft.teams", "com.tinyspeck.slackmacgap"];
    v.extend_from_slice(BROWSERS);
    v
}

pub fn is_browser(app: &str) -> bool {
    BROWSERS.contains(&app)
}

/// Whether a window is worth walking (browsers: only meeting tabs; the
/// window title is the active tab's title).
pub fn interesting_window(app: &str, title: &str) -> bool {
    if !is_browser(app) {
        return true;
    }
    let t = title.to_lowercase();
    t.contains("microsoft teams") || t.starts_with("meet") || t.contains("meet -") || t.contains("google meet") || t.contains("zoom") || t.contains("huddle")
}

/// Parse every window of one snapshot into one observation.
pub fn observe(windows: &[AxWindow]) -> Observation {
    super::drivers::observe(windows)
}

/// Words apps put in brackets after a name: "Shelley Morgan (Host, me)".
const NAME_TAGS: &[&str] = &["host", "co-host", "cohost", "me", "you", "guest", "unverified", "external", "organizer", "organiser", "presenter", "participant"];

/// A name and the lower-case tags after it: "Ann Lee (Host, me)" → ("Ann Lee", ["host", "me"]).
pub fn name_tags(raw: &str) -> (Option<String>, Vec<String>) {
    let mut s = raw.trim().to_string();
    let mut tags = Vec::new();
    loop {
        let Some(open) = s.rfind(" (").filter(|_| s.ends_with(')')) else { break };
        let inner: Vec<String> = s[open + 2..s.len() - 1].split(',').map(|t| t.trim().to_lowercase()).collect();
        if inner.is_empty() || !inner.iter().all(|t| NAME_TAGS.contains(&t.as_str())) {
            break;
        }
        tags.extend(inner);
        s.truncate(open);
    }
    for (suffix, tag) in [(" Unverified", "unverified"), (", host", "host"), (", Host", "host")] {
        if let Some(x) = s.strip_suffix(suffix) {
            s = x.trim().to_string();
            tags.push(tag.into());
        }
    }
    (clean_name(&s), tags)
}

/// Strip decorations apps add to names on tiles.
pub fn clean_name(raw: &str) -> Option<String> {
    let mut s = raw.trim().to_string();
    loop {
        let Some(open) = s.rfind(" (").filter(|_| s.ends_with(')')) else { break };
        if !s[open + 2..s.len() - 1].split(',').all(|t| NAME_TAGS.contains(&t.trim().to_lowercase().as_str())) {
            break;
        }
        s.truncate(open);
    }
    for suffix in [" Unverified", ", host", ", Host"] {
        if let Some(x) = s.strip_suffix(suffix) {
            s = x.trim().to_string();
        }
    }
    let s = s.trim_matches(|c: char| c == ',' || c.is_whitespace()).to_string();
    let lower = s.to_lowercase();
    let junk = s.is_empty()
        || s.chars().count() > 60
        || s.contains('\n')
        || !s.chars().any(char::is_alphabetic)
        || ["you", "me", "participants", "chat", "more", "mute", "unmute", "raise", "react", "view", "share", "leave", "camera", "mic", "people"].contains(&lower.as_str());
    (!junk).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("Shelley Unverified").as_deref(), Some("Shelley"));
        assert_eq!(clean_name(" Ann Lee (Guest) ").as_deref(), Some("Ann Lee"));
        assert_eq!(clean_name("Alex Rowan (Host, me)").as_deref(), Some("Alex Rowan"));
        assert_eq!(clean_name("Ann (Product)").as_deref(), Some("Ann (Product)"), "only known tags are stripped");
        assert_eq!(clean_name("Mute"), None);
        assert_eq!(clean_name("12:30"), None);
    }

    #[test]
    fn name_tags_are_read() {
        let (n, t) = name_tags("Shelley Morgan (Host, me)");
        assert_eq!(n.as_deref(), Some("Shelley Morgan"));
        assert_eq!(t, vec!["host", "me"]);
        let (n, t) = name_tags("Riley Chen Unverified");
        assert_eq!(n.as_deref(), Some("Riley Chen"));
        assert_eq!(t, vec!["unverified"]);
    }

    #[test]
    fn browser_windows_filtered_by_title() {
        assert!(interesting_window("net.imput.helium", "Connect | Microsoft Teams - Camera and microphone recording - Helium"));
        assert!(interesting_window("com.google.Chrome", "Meet - abc-defg-hij - Google Chrome"));
        assert!(!interesting_window("com.google.Chrome", "Inbox (3) - Gmail"));
        assert!(interesting_window("us.zoom.xos", "anything"));
    }
}
