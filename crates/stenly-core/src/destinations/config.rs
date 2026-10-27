//! `[[destinations]]` entries in config.toml.
//!
//! ```toml
//! [[destinations]]
//! id = "notion-crm"
//! name = "Notion CRM"
//! kind = "notion"
//! [destinations.notion]
//! database_id = "…"
//! title_property = "Name"
//! date_property = "Date"
//! attendees_property = "Attendees"
//! [destinations.notion.relation]
//! property = "Company"
//! database_id = "…"
//! match_by = "domain"
//! [destinations.auto]
//! enabled = true
//! external_only = true
//! ```

use super::Locality;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A Markdown folder on this machine (Obsidian vaults included).
    #[default]
    Folder,
    Notion,
    Slack,
    Webhook,
    /// End-to-end encrypted Stenly share link.
    Share,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Folder, Kind::Notion, Kind::Slack, Kind::Webhook, Kind::Share];

    pub fn locality(self) -> Locality {
        match self {
            Kind::Folder => Locality::Local,
            Kind::Notion | Kind::Slack | Kind::Webhook | Kind::Share => Locality::Cloud,
        }
    }
    /// Who receives the note ("Sends to …").
    pub fn service(self) -> &'static str {
        match self {
            Kind::Folder => "a folder",
            Kind::Notion => "Notion",
            Kind::Slack => "Slack",
            Kind::Webhook => "your webhook",
            Kind::Share => "share.stenly.ai",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Kind::Folder => "Markdown folder",
            Kind::Notion => "Notion",
            Kind::Slack => "Slack",
            Kind::Webhook => "Webhook",
            Kind::Share => "Stenly share link",
        }
    }
    pub fn parse(s: &str) -> Option<Kind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "folder" | "markdown" | "obsidian" => Some(Kind::Folder),
            "notion" => Some(Kind::Notion),
            "slack" => Some(Kind::Slack),
            "webhook" | "http" => Some(Kind::Webhook),
            "share" | "stenly_share" | "stenly share link" => Some(Kind::Share),
            _ => None,
        }
    }
    /// What the secret is called in forms ("" = this kind has none).
    pub fn secret_label(self) -> &'static str {
        match self {
            Kind::Folder => "",
            Kind::Notion => "Integration token",
            Kind::Slack => "Webhook URL",
            Kind::Webhook => "URL",
            Kind::Share => "Upload token",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DestinationConfig {
    /// Stable id (also names its Keychain item). Lowercase-dashed.
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// Optional env var holding the credential instead of the Keychain
    /// (scripts, CI). The Keychain is used when this is empty or unset.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub secret_env: String,
    pub auto: AutoRule,
    #[serde(skip_serializing_if = "FolderOpts::is_default")]
    pub folder: FolderOpts,
    #[serde(skip_serializing_if = "NotionOpts::is_default")]
    pub notion: NotionOpts,
    #[serde(skip_serializing_if = "WebhookOpts::is_default")]
    pub webhook: WebhookOpts,
    #[serde(skip_serializing_if = "ShareOpts::is_default")]
    pub share: ShareOpts,
    /// A credential typed into the settings form but not saved yet (so Test
    /// works before Save). Never written to the config file.
    #[serde(skip)]
    pub secret_override: Option<Secret>,
    #[serde(skip)]
    pub signing_override: Option<Secret>,
}

/// A credential held in memory; `Debug` never prints it.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(•••)")
    }
}

impl DestinationConfig {
    pub fn new(kind: Kind, name: &str) -> Self {
        let mut d = Self { id: crate::util::slug(name), name: name.to_string(), kind, ..Default::default() };
        if kind == Kind::Notion {
            d.notion = NotionOpts::suggested();
        }
        if kind == Kind::Webhook {
            d.webhook.include_transcript = true;
        }
        if kind == Kind::Share {
            d.share = ShareOpts::suggested();
        }
        d
    }
}

/// "Send automatically after each meeting", with optional filters. All set
/// filters must match. Off by default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AutoRule {
    pub enabled: bool,
    /// Only meetings with at least one attendee outside your email domain.
    pub external_only: bool,
    /// Only meetings whose title contains one of these (case-insensitive).
    pub title_contains: Vec<String>,
    /// Only meetings from these calendars (calendar name, id or account email).
    pub calendars: Vec<String>,
}

impl AutoRule {
    pub fn has_filters(&self) -> bool {
        self.external_only || !self.title_contains.is_empty() || !self.calendars.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FolderOpts {
    /// Folder to write notes into (`~` allowed).
    pub path: String,
    /// File name template; empty = the `[output] filename`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub filename: String,
    /// Include the transcript (default: as in `[output]`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_transcript: Option<bool>,
}

impl FolderOpts {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct NotionOpts {
    /// The database pages are created in (id or URL).
    pub database_id: String,
    /// Title property ("" = detect the database's title column).
    pub title_property: String,
    /// Date property for the meeting start ("" = don't set).
    pub date_property: String,
    /// Attendees: a multi-select, rich-text, people or email property.
    pub attendees_property: String,
    /// Tags from the summary → a multi-select ("" = don't set).
    pub tags_property: String,
    /// Meeting URL → a url property ("" = don't set).
    pub url_property: String,
    /// Optional relation (e.g. Company / People) filled by matching attendees.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation: Option<RelationMatch>,
    /// Append the transcript (collapsed toggle) to the page.
    pub include_transcript: bool,
    /// API base URL (tests point this at a mock server).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub api_base: String,
}

impl NotionOpts {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
    /// Common property names, used as the starting point for a new destination.
    pub fn suggested() -> Self {
        Self { date_property: "Date".into(), attendees_property: "Attendees".into(), ..Default::default() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MatchBy {
    /// Attendee email domains (acme.com) against a domain/URL/email column
    /// of the related records.
    #[default]
    Domain,
    /// Attendee emails against an email column (a People database).
    Email,
    /// Attendee names against the related records' titles.
    Name,
}

impl MatchBy {
    pub fn label(self) -> &'static str {
        match self {
            MatchBy::Domain => "Email domain",
            MatchBy::Email => "Email address",
            MatchBy::Name => "Name",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RelationMatch {
    /// Relation property on the meetings database (e.g. "Company").
    pub property: String,
    /// The related database (id or URL); "" = the relation's own target.
    pub database_id: String,
    pub match_by: MatchBy,
    /// Column of the related database to match against ("" = Domain/Website/
    /// Email for domain matching, the title for names).
    pub match_property: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WebhookOpts {
    /// Send the transcript too (JSON `transcript` field).
    pub include_transcript: bool,
}

impl WebhookOpts {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// End-to-end-encrypted share server options. The optional upload token is a
/// destination secret and never appears here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShareOpts {
    pub server_url: String,
    /// Include the full speaker-attributed transcript (default on).
    pub include_transcript: bool,
    /// Opt in to a plaintext title for external link previews (default off).
    pub plaintext_title: bool,
    /// The server may clamp this to its policy.
    pub expiry_days: u32,
}
impl Default for ShareOpts {
    fn default() -> Self { Self { server_url: "https://share.stenly.ai".into(), include_transcript: true, plaintext_title: false, expiry_days: 90 } }
}
impl ShareOpts {
    fn is_default(&self) -> bool { *self == Self::default() }
    pub fn suggested() -> Self { Self::default() }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let mut d = DestinationConfig::new(Kind::Notion, "Notion CRM");
        d.notion.database_id = "abc".into();
        d.notion.relation = Some(RelationMatch { property: "Company".into(), database_id: "def".into(), ..Default::default() });
        d.auto.enabled = true;
        d.auto.title_contains = vec!["Acme".into()];
        let s = toml::to_string(&d).unwrap();
        assert!(s.contains("kind = \"notion\""));
        assert!(!s.contains("[folder]"));
        let back: DestinationConfig = toml::from_str(&s).unwrap();
        assert_eq!(back, d);
        // typed-in secrets never reach the file or Debug output
        d.secret_override = Some(Secret("ntn_supersecret".into()));
        assert!(!toml::to_string(&d).unwrap().contains("supersecret"));
        assert!(!format!("{d:?}").contains("supersecret"));
    }

    #[test]
    fn parses_hand_written() {
        let d: DestinationConfig = toml::from_str("id = \"s\"\nname = \"Slack\"\nkind = \"slack\"\n").unwrap();
        assert_eq!(d.kind, Kind::Slack);
        assert!(!d.auto.enabled);
        assert_eq!(Kind::parse("Obsidian"), Some(Kind::Folder));
        assert_eq!(Kind::Notion.locality(), Locality::Cloud);
        assert_eq!(Kind::Folder.locality(), Locality::Local);
    }
}
