//! Render a meeting into an Obsidian-friendly markdown note.

use crate::config::OutputConfig;
use crate::session::SessionMeta;
use crate::summary::Summary;
use crate::transcript::Transcript;
use crate::util::{collapse_tilde, expand_tilde, fmt_timestamp, render_template};
use anyhow::{Context, Result};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn yaml_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

pub fn render(meta: &SessionMeta, transcript: &Transcript, summary: Option<&Summary>, audio_dir: Option<&Path>, include_transcript: bool) -> String {
    let mut md = String::new();
    let minutes = meta.duration_mins().unwrap_or(0);

    // ---- frontmatter
    md.push_str("---\n");
    md.push_str("type: meeting\n");
    let _ = writeln!(md, "title: {}", yaml_str(&meta.title));
    let _ = writeln!(md, "date: {}", meta.started.format("%Y-%m-%dT%H:%M:%S%:z"));
    let _ = writeln!(md, "duration_min: {minutes}");
    if let Some(ev) = &meta.event {
        if !ev.attendees.is_empty() {
            md.push_str("attendees:\n");
            for a in &ev.attendees {
                if ev.calendar.as_deref().is_some_and(|me| a.email.eq_ignore_ascii_case(me)) {
                    continue;
                }
                let _ = writeln!(md, "  - {}", yaml_str(&format!("[[{}]]", a.display())));
            }
        }
        if let Some(link) = &ev.link {
            let _ = writeln!(md, "platform: {}", yaml_str(link.provider.label()));
            let _ = writeln!(md, "meeting_url: {}", yaml_str(&link.url));
        }
        if ev.recurring {
            let _ = writeln!(md, "series: {}", yaml_str(&crate::util::slug(&ev.title)));
        }
    }
    if let Some(s) = summary {
        if !s.tags.is_empty() {
            md.push_str("tags:\n  - meeting\n");
            for t in &s.tags {
                let _ = writeln!(md, "  - {}", yaml_str(&crate::util::slug(t)));
            }
        } else {
            md.push_str("tags:\n  - meeting\n");
        }
    } else {
        md.push_str("tags:\n  - meeting\n");
    }
    if let Some(dir) = audio_dir {
        let _ = writeln!(md, "audio: {}", yaml_str(&collapse_tilde(dir)));
    }
    md.push_str("generator: Stenly\n---\n\n");

    // ---- body
    let _ = writeln!(md, "# {}\n", meta.title);
    if let Some(s) = summary {
        if !s.summary.trim().is_empty() {
            let _ = writeln!(md, "## Summary\n\n{}\n", s.summary.trim());
        }
        for sec in &s.sections {
            if sec.bullets.is_empty() {
                continue;
            }
            let _ = writeln!(md, "### {}\n", sec.heading.trim());
            for b in &sec.bullets {
                let _ = writeln!(md, "- {}", b.text().trim());
                for sub in b.sub() {
                    let _ = writeln!(md, "    - {}", sub.trim());
                }
            }
            md.push('\n');
        }
        if !s.key_points.is_empty() {
            md.push_str("## Key points\n\n");
            for p in &s.key_points {
                let _ = writeln!(md, "- {p}");
            }
            md.push('\n');
        }
        if !s.decisions.is_empty() && s.sections.is_empty() {
            md.push_str("## Decisions\n\n");
            for d in &s.decisions {
                let _ = writeln!(md, "- {d}");
            }
            md.push('\n');
        }
        if !s.action_items.is_empty() {
            md.push_str("## Action items\n\n");
            for a in &s.action_items {
                match a.owner.as_deref().filter(|o| !o.is_empty()) {
                    Some(o) => {
                        let _ = writeln!(md, "- [ ] **{o}**: {}", a.task);
                    }
                    None => {
                        let _ = writeln!(md, "- [ ] {}", a.task);
                    }
                }
            }
            md.push('\n');
        }
    }
    if let Some(err) = &meta.error {
        let _ = writeln!(md, "> [!warning] {}\n", err.replace('\n', " "));
    }
    if summary.is_none() && !transcript.is_empty() && !meta.error.as_deref().is_some_and(|e| e.contains("Summary failed")) {
        md.push_str("> [!warning] **No summary for this meeting.** Choose **Summarize now** to write one.\n\n");
    }
    if include_transcript {
        if transcript.is_empty() {
            md.push_str("## Transcript\n\n_No speech detected._\n");
        } else {
            let collapsed = summary.is_some();
            let _ = writeln!(md, "## Transcript\n\n> [!quote]{} Transcript ({} words)", if collapsed { "-" } else { "+" }, transcript.word_count());
            for s in &transcript.segments {
                let _ = writeln!(md, "> **{}** `{}` {}\n>", s.label(), fmt_timestamp(s.start), s.text);
            }
        }
    }
    md
}

/// Pick a non-clobbering path for the note.
pub fn note_path(cfg: &OutputConfig, meta: &SessionMeta) -> PathBuf {
    let dir = expand_tilde(&cfg.notes_dir);
    let mut name = render_template(&cfg.filename, meta.started, &meta.title);
    if !name.ends_with(".md") {
        name.push_str(".md");
    }
    let mut path = dir.join(&name);
    let stem = name.trim_end_matches(".md").to_string();
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem} {n}.md"));
        n += 1;
    }
    path
}

pub fn write_note(path: &Path, content: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Append `- HH:MM [[Note]]` to the daily note, creating it if needed.
pub fn link_from_daily_note(cfg: &OutputConfig, meta: &SessionMeta, note: &Path) -> Result<Option<PathBuf>> {
    if cfg.daily_note.trim().is_empty() {
        return Ok(None);
    }
    let daily = expand_tilde(&render_template(&cfg.daily_note, meta.started, &meta.title));
    let stem = note.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let line = format!("- {} [[{}]]\n", meta.started.format("%H:%M"), stem);
    if let Some(dir) = daily.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let existing = std::fs::read_to_string(&daily).unwrap_or_default();
    if existing.contains(&format!("[[{stem}]]")) {
        return Ok(Some(daily));
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&line);
    std::fs::write(&daily, out)?;
    Ok(Some(daily))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Channel;
    use crate::summary::ActionItem;
    use crate::transcript::Segment;
    use chrono::{Local, TimeZone};

    /// Deserialize a minimal fixture so serde-defaulted SessionMeta additions
    /// don't require every renderer test to list storage implementation fields.
    fn test_meta(title: &str, started: chrono::DateTime<Local>, ended: Option<chrono::DateTime<Local>>) -> SessionMeta {
        serde_json::from_value(serde_json::json!({
            "title": title,
            "started": started,
            "ended": ended,
            "event": null,
            "state": "Done",
            "note_path": null,
            "error": null,
            "routes": [],
            "sends": [],
            "meeting_app": null,
        }))
        .unwrap()
    }

    #[test]
    fn renders_note() {
        let meta = test_meta("Design review", Local.with_ymd_and_hms(2026, 10, 20, 10, 0, 0).unwrap(), Some(Local.with_ymd_and_hms(2026, 10, 20, 10, 31, 0).unwrap()));
        let t = Transcript { segments: vec![Segment::new(Channel::Mic, 1.0, 2.0, "Hello")], ..Default::default() };
        let s = Summary {
            summary: "We reviewed.".into(),
            action_items: vec![ActionItem { owner: Some("Me".into()), task: "Ship it".into() }],
            tags: vec!["Design Systems".into()],
            ..Default::default()
        };
        let md = render(&meta, &t, Some(&s), None, true);
        assert!(md.starts_with("---\ntype: meeting\ntitle: \"Design review\"\n"));
        assert!(md.contains("duration_min: 31"));
        assert!(md.contains("  - \"design-systems\""));
        assert!(md.contains("- [ ] **Me**: Ship it"));
        assert!(md.contains("> **Me** `00:01` Hello"));
    }

    #[test]
    fn transcript_only_note_explains_how_to_generate_a_summary() {
        let meta = test_meta("Transcript only", Local::now(), None);
        let transcript = Transcript { segments: vec![Segment::new(Channel::Mic, 0.0, 2.0, "Useful transcript")], ..Default::default() };
        let md = render(&meta, &transcript, None, None, true);
        assert!(md.contains("**No summary for this meeting.**"));
        assert!(md.contains("**Summarize now**"));
        assert!(md.contains("## Transcript"));
    }
}
