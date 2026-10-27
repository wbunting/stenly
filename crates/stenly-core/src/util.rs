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

#[cfg(test)]
mod tests {
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
