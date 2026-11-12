use std::path::{Path, PathBuf};

/// Expand a leading `~` to the home directory.
pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) {
            return home.join(rest);
        }
    } else if p == "~"
        && let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) {
            return home;
        }
    PathBuf::from(p)
}

/// Collapse the home directory back to `~` for display / config writing.
pub fn collapse_tilde(p: &Path) -> String {
    if let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
        && let Ok(rest) = p.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    p.display().to_string()
}

/// Make a string safe to use as a file name on every platform (and in Obsidian links).
pub fn sanitize_filename(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' | '^' | '[' | ']' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches(|c: char| c == '.' || c.is_whitespace());
    let mut out: String = trimmed.chars().take(120).collect();
    if out.is_empty() {
        out = "Meeting".into();
    }
    out
}

/// Lowercase-dashed slug.
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

pub fn fmt_timestamp(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
}

/// Render `{date}`, `{time}`, `{title}`, `{slug}` placeholders.
pub fn render_template(tpl: &str, at: chrono::DateTime<chrono::Local>, title: &str) -> String {
    tpl.replace("{date}", &at.format("%Y-%m-%d").to_string())
        .replace("{time}", &at.format("%H%M").to_string())
        .replace("{title}", &sanitize_filename(title))
        .replace("{slug}", &slug(title))
}

/// UI copy is written for macOS ("this Mac", "Keychain", "menu bar"). Returns
/// it unchanged there and reworded for other desktops.
pub fn os_copy(s: &'static str) -> std::borrow::Cow<'static, str> {
    if cfg!(target_os = "macos") { s.into() } else { desktop_copy(s).into() }
}

/// [`os_copy`]'s rewording for non-macOS desktops, where secrets live in a
/// 0600 file next to the config and the app sits in the system tray.
pub fn desktop_copy(s: &str) -> String {
    const PRIVATE: &str = "in a private file only you can read";
    let rules: [(&str, &str); 15] = [
        ("in your macOS Keychain", PRIVATE),
        ("in your Keychain", PRIVATE),
        ("in the Keychain", PRIVATE),
        ("Saved in Keychain", "Saved privately"),
        ("stored in Keychain", "stored privately"),
        ("On This Mac", "On This Computer"),
        ("This Mac", "This computer"),
        ("this Mac", "this computer"),
        ("your Mac", "your computer"),
        ("busy Mac", "busy computer"),
        ("another Mac", "another computer"),
        ("one Mac", "one computer"),
        ("your menu bar", "your system tray"),
        ("the menu bar", "the system tray"),
        ("in menu bar", "in the system tray"),
    ];
    rules.iter().fold(s.to_string(), |acc, (from, to)| acc.replace(from, to))
}

#[cfg(test)]
mod tests {
    #[test]
    fn desktop_copy_rewords_mac_terms() {
        assert_eq!(desktop_copy("Meeting notes that write themselves, on your Mac."), "Meeting notes that write themselves, on your computer.");
        assert_eq!(desktop_copy("Summaries stay on this Mac."), "Summaries stay on this computer.");
        assert_eq!(desktop_copy("On This Mac"), "On This Computer");
        assert_eq!(desktop_copy("Keys are stored in your macOS Keychain, not the config file."), "Keys are stored in a private file only you can read, not the config file.");
        assert_eq!(desktop_copy("Saved in Keychain · type to replace"), "Saved privately · type to replace");
        assert_eq!(desktop_copy("API key (stored in Keychain)"), "API key (stored privately)");
        assert_eq!(desktop_copy("Start quietly in the menu bar"), "Start quietly in the system tray");
        assert_eq!(desktop_copy("Show next meeting in menu bar"), "Show next meeting in the system tray");
        assert_eq!(desktop_copy("Deactivate to move the licence to another Mac."), "Deactivate to move the licence to another computer.");
        assert_eq!(desktop_copy("Key ab12 · one Mac · updates included."), "Key ab12 · one computer · updates included.");
        assert_eq!(desktop_copy("No Mac words here"), "No Mac words here");
    }

    use super::*;

    #[test]
    fn sanitizes() {
        assert_eq!(sanitize_filename("Weekly: sync / planning?"), "Weekly sync planning");
        assert_eq!(sanitize_filename("..."), "Meeting");
        assert_eq!(slug("Weekly: Sync / Planning"), "weekly-sync-planning");
        assert_eq!(fmt_timestamp(3725.0), "1:02:05");
        assert_eq!(fmt_timestamp(65.4), "01:05");
    }
}
