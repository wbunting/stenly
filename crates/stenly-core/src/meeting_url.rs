//! Find video-call links in calendar events.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    Zoom,
    GoogleMeet,
    Teams,
    Webex,
    Whereby,
    Around,
    Slack,
    Chime,
    Jitsi,
    Discord,
    Other,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::Zoom => "Zoom",
            Provider::GoogleMeet => "Google Meet",
            Provider::Teams => "Microsoft Teams",
            Provider::Webex => "Webex",
            Provider::Whereby => "Whereby",
            Provider::Around => "Around",
            Provider::Slack => "Slack Huddle",
            Provider::Chime => "Amazon Chime",
            Provider::Jitsi => "Jitsi",
            Provider::Discord => "Discord",
            Provider::Other => "Call",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingLink {
    pub provider: Provider,
    pub url: String,
}

static URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https?://[^\s<>"'\\)\]}]+"#).unwrap());

/// Classify one URL. Returns `None` if it's not a call link.
pub fn classify(url: &str) -> Option<Provider> {
    let lower = url.to_ascii_lowercase();
    let rest = lower.split_once("://")?.1;
    let (host, path) = rest.split_once('/').map(|(h, p)| (h, p)).unwrap_or((rest, ""));
    let host = host.split('@').next_back()?.split(':').next()?;
    let on = |d: &str| host == d || host.ends_with(&format!(".{d}"));

    if (on("zoom.us") || on("zoom.com") || on("zoomgov.com"))
        && (path.starts_with("j/") || path.starts_with("my/") || path.starts_with("s/") || path.starts_with("w/") || path.contains("/j/"))
    {
        return Some(Provider::Zoom);
    }
    if host == "meet.google.com" && path.len() > 3 && !path.starts_with("_meet") {
        return Some(Provider::GoogleMeet);
    }
    if (host == "teams.microsoft.com" && (path.starts_with("l/meetup-join") || path.starts_with("meet/")))
        || (host == "teams.live.com" && path.starts_with("meet/"))
    {
        return Some(Provider::Teams);
    }
    if on("webex.com") && (path.contains("j.php") || path.starts_with("meet/") || path.contains("/meet/") || path.starts_with("join/")) {
        return Some(Provider::Webex);
    }
    if on("whereby.com") && !path.is_empty() {
        return Some(Provider::Whereby);
    }
    if on("around.co") && !path.is_empty() {
        return Some(Provider::Around);
    }
    if on("slack.com") && path.contains("huddle") {
        return Some(Provider::Slack);
    }
    if host == "chime.aws" && !path.is_empty() {
        return Some(Provider::Chime);
    }
    if host == "meet.jit.si" && !path.is_empty() {
        return Some(Provider::Jitsi);
    }
    if (host == "discord.gg" || on("discord.com")) && !path.is_empty() {
        return Some(Provider::Discord);
    }
    None
}

/// Google wraps links in description HTML as `https://www.google.com/url?q=<encoded>`.
fn unwrap_google_redirect(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://www.google.com/url?") || lower.starts_with("https://google.com/url?")) {
        return None;
    }
    let query = url.split_once('?')?.1;
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("q="))
        .map(percent_decode)
}

pub fn percent_decode_pub(s: &str) -> String {
    percent_decode(s)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn clean(url: &str) -> String {
    url.replace("&amp;", "&")
        .trim_end_matches(['.', ',', ';', ':', '!', '?', '>'])
        .to_string()
}

/// Find the first call link in any of the given fields, in priority order
/// (pass explicit conference fields first, free text last).
pub fn find_meeting_link<'a>(fields: impl IntoIterator<Item = &'a str>) -> Option<MeetingLink> {
    for text in fields {
        for m in URL_RE.find_iter(text) {
            let raw = clean(m.as_str());
            let url = unwrap_google_redirect(&raw).map(|u| clean(&u)).unwrap_or(raw);
            if let Some(provider) = classify(&url) {
                return Some(MeetingLink { provider, url });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_providers() {
        let cases = [
            ("https://us02web.zoom.us/j/81234567890?pwd=abc", Provider::Zoom),
            ("https://zoom.us/my/someone", Provider::Zoom),
            ("https://meet.google.com/abc-defg-hij", Provider::GoogleMeet),
            ("https://teams.microsoft.com/l/meetup-join/19%3ameeting_x%40thread.v2/0?context=%7b%7d", Provider::Teams),
            ("https://teams.microsoft.com/meet/123456789?p=abc", Provider::Teams),
            ("https://teams.live.com/meet/9371234", Provider::Teams),
            ("https://acme.webex.com/acme/j.php?MTID=m123", Provider::Webex),
            ("https://whereby.com/standup", Provider::Whereby),
            ("https://app.slack.com/huddle/T0/C0", Provider::Slack),
            ("https://chime.aws/1234567890", Provider::Chime),
            ("https://meet.jit.si/MyRoom", Provider::Jitsi),
        ];
        for (url, p) in cases {
            assert_eq!(classify(url), Some(p), "{url}");
        }
        assert_eq!(classify("https://zoom.us/pricing"), None);
        assert_eq!(classify("https://meet.google.com/"), None);
        assert_eq!(classify("https://docs.google.com/document/d/1"), None);
        assert_eq!(classify("https://evilzoom.us/j/1"), None);
    }

    #[test]
    fn finds_in_description_html() {
        let desc = r#"Join: <a href="https://www.google.com/url?q=https%3A%2F%2Fus06web.zoom.us%2Fj%2F123%3Fpwd%3Dx&amp;sa=D">link</a>. Agenda: https://docs.google.com/x"#;
        let link = find_meeting_link([desc]).unwrap();
        assert_eq!(link.provider, Provider::Zoom);
        assert_eq!(link.url, "https://us06web.zoom.us/j/123?pwd=x");
    }

    #[test]
    fn priority_order() {
        let link = find_meeting_link(["", "https://meet.google.com/aaa-bbbb-ccc", "https://zoom.us/j/1"]).unwrap();
        assert_eq!(link.provider, Provider::GoogleMeet);
        assert!(find_meeting_link(["nothing here", "https://example.com"]).is_none());
    }
}
