//! Running the prompt library against one meeting: choosing a note template,
//! running a recipe (a saved prompt) and keeping its results, optionally
//! saved into the note under a heading.
//!
//! Per-session files (next to `meta.json`):
//!   template       the template id chosen in the app ("Regenerate with …")
//!   recipes.json   recipe results, newest last

use crate::config::{Config, LlmTarget};
use crate::library::{Entry, Library, Origin};
use crate::session::SessionMeta;
use crate::summary::Summary;
use crate::transcript::Transcript;
use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const TEMPLATE_FILE: &str = "template";
pub const RESULTS_FILE: &str = "recipes.json";

// ---------------------------------------------------------------- templates

/// Words of `s`, lowercased, split on anything that isn't a letter or digit.
fn words(s: &str) -> Vec<String> {
    s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// Does `keyword` occur in `text` as whole words? The last word may carry a
/// short suffix ("demo" matches "demos", "retro" matches "retros").
fn has_phrase(text: &[String], keyword: &str) -> bool {
    let kw = words(keyword);
    if kw.is_empty() || kw.len() > text.len() {
        return false;
    }
    text.windows(kw.len()).any(|w| {
        w.iter().zip(&kw).enumerate().all(|(i, (t, k))| {
            if i + 1 < kw.len() || k.chars().all(|c| c.is_ascii_digit()) {
                t == k
            } else {
                t == k || (t.starts_with(k.as_str()) && t.len() <= k.len() + 2 && k.len() >= 4)
            }
        })
    })
}

/// What auto-selection looks at.
#[derive(Debug, Clone, Default)]
pub struct MeetingInfo<'a> {
    pub title: &'a str,
    pub description: &'a str,
    /// Attendee emails, including the user's own.
    pub attendees: Vec<&'a str>,
    /// The user's own address (the calendar it came from).
    pub me: Option<&'a str>,
}

impl<'a> MeetingInfo<'a> {
    pub fn from_meta(meta: &'a SessionMeta) -> Self {
        match &meta.event {
            Some(ev) => Self {
                title: &meta.title,
                description: ev.description.as_deref().unwrap_or(""),
                attendees: ev.attendees.iter().map(|a| a.email.as_str()).collect(),
                me: ev.calendar.as_deref(),
            },
            None => Self { title: &meta.title, ..Default::default() },
        }
    }
}

fn domain(email: &str) -> Option<String> {
    email.rsplit_once('@').map(|(_, d)| d.trim().to_lowercase()).filter(|d| !d.is_empty())
}

const PERSONAL_DOMAINS: &[&str] = &["gmail.com", "googlemail.com", "icloud.com", "me.com", "outlook.com", "hotmail.com", "yahoo.com", "proton.me", "protonmail.com"];

/// Pick a template id for a meeting from each template's `match:` keywords
/// (longest keyword wins; the title counts before the invite's description),
/// then `two_person: true` templates for a call with one colleague. Falls back
/// to `default`.
pub fn auto_template(templates: &[Entry], default: &str, info: &MeetingInfo) -> String {
    let fallback = || if templates.iter().any(|t| t.id == default) { default.to_string() } else { "general".to_string() };
    if crate::summary::is_placeholder_title(info.title) && info.description.is_empty() && info.attendees.is_empty() {
        return fallback();
    }
    for text in [info.title, info.description] {
        let w = words(text);
        let best = templates
            .iter()
            .flat_map(|t| t.list("match").into_iter().map(move |k| (t, k)))
            .filter(|(_, k)| has_phrase(&w, k))
            .max_by_key(|(_, k)| words(k).iter().map(|x| x.len() + 1).sum::<usize>());
        if let Some((t, _)) = best {
            return t.id.clone();
        }
    }
    // Exactly two people from the same (work) domain: a 1:1.
    let people: Vec<&str> = {
        let mut v: Vec<&str> = info.attendees.iter().copied().filter(|e| !e.is_empty() && !e.contains("resource.calendar.google.com")).collect();
        v.sort_unstable();
        v.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        v
    };
    if people.len() == 2 {
        let doms: Vec<Option<String>> = people.iter().map(|e| domain(e)).collect();
        let same = doms[0].is_some() && doms[0] == doms[1] && !PERSONAL_DOMAINS.contains(&doms[0].as_deref().unwrap_or(""));
        let includes_me = info.me.is_none_or(|me| people.iter().any(|p| p.eq_ignore_ascii_case(me)));
        if same && includes_me {
            if let Some(t) = templates.iter().find(|t| t.field("two_person").is_some_and(|v| v == "true")) {
                return t.id.clone();
            }
        }
    }
    fallback()
}

/// The template chosen for a session in the app, if any.
pub fn chosen_template(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join(TEMPLATE_FILE)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn set_chosen_template(dir: &Path, id: Option<&str>) -> Result<()> {
    let p = dir.join(TEMPLATE_FILE);
    match id {
        Some(id) => std::fs::write(&p, format!("{id}\n"))?,
        None => {
            if p.exists() {
                std::fs::remove_file(&p)?
            }
        }
    }
    Ok(())
}

/// The template a session's note should be written with: the one picked in the
/// app, else the automatic choice.
pub fn template_for(cfg: &Config, lib: &Library, dir: &Path, meta: &SessionMeta) -> Entry {
    let all = lib.all_with_hidden();
    let visible: Vec<Entry> = all.iter().filter(|e| !e.hidden).cloned().collect();
    let id = chosen_template(dir).filter(|id| all.iter().any(|e| &e.id == id)).unwrap_or_else(|| {
        if cfg.templates.auto { auto_template(&visible, &cfg.templates.default, &MeetingInfo::from_meta(meta)) } else { cfg.templates.default.clone() }
    });
    all.into_iter().find(|e| e.id == id).or_else(|| lib.get("general")).unwrap_or_else(|| Entry::new("general", "General"))
}

/// The prompt instructions for a template. The unchanged built-in General
/// adds nothing (the default prompt already is "General").
pub fn template_instructions(t: &Entry) -> Option<(&str, &str)> {
    (!(t.id == "general" && t.origin == Origin::Builtin) && !t.body.trim().is_empty()).then_some((t.name.as_str(), t.body.as_str()))
}

// ---------------------------------------------------------------- recipe runs

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecipeResult {
    /// Recipe id.
    pub recipe: String,
    pub name: String,
    pub text: String,
    pub model: String,
    pub created: DateTime<Local>,
    /// Heading it was saved under in the note (None = not in the note).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_heading: Option<String>,
}

pub fn load_results(dir: &Path) -> Vec<RecipeResult> {
    std::fs::read(dir.join(RESULTS_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_results(dir: &Path, results: &[RecipeResult]) -> Result<()> {
    let p = dir.join(RESULTS_FILE);
    if results.is_empty() {
        if p.exists() {
            std::fs::remove_file(p)?;
        }
        return Ok(());
    }
    let tmp = dir.join(format!("{RESULTS_FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(results)?)?;
    std::fs::rename(tmp, p)?;
    Ok(())
}

/// Keep the latest result per recipe (re-running replaces it).
pub fn push_result(dir: &Path, r: RecipeResult) -> Result<()> {
    let mut all = load_results(dir);
    let keep_heading = all.iter().find(|x| x.recipe == r.recipe).and_then(|x| x.saved_heading.clone());
    all.retain(|x| x.recipe != r.recipe);
    let had_saved = keep_heading.is_some();
    all.push(RecipeResult { saved_heading: keep_heading, ..r });
    save_results(dir, &all)?;
    if had_saved {
        // the note shows the old text under that heading: update it
        reapply_saved(dir)?;
    }
    Ok(())
}

pub fn remove_result(dir: &Path, recipe: &str) -> Result<()> {
    let mut all = load_results(dir);
    let removed: Vec<RecipeResult> = all.iter().filter(|x| x.recipe == recipe).cloned().collect();
    all.retain(|x| x.recipe != recipe);
    save_results(dir, &all)?;
    // take it out of the note too
    if let Some(h) = removed.iter().find_map(|r| r.saved_heading.clone()) {
        if let Some(note) = SessionMeta::load(dir).ok().and_then(|m| m.note_path).filter(|p| p.exists()) {
            let md = std::fs::read_to_string(&note)?;
            crate::note::write_note(&note, &remove_section(&md, &h))?;
        }
    }
    Ok(())
}

fn trim_transcript(plain: String, max: usize) -> String {
    if plain.len() <= max {
        return plain;
    }
    let head: String = plain.chars().take(max * 2 / 3).collect();
    let tail: String = plain.chars().rev().take(max / 3).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}\n[... middle of transcript omitted ...]\n{tail}")
}

/// The AI notes as compact Markdown, for recipe context.
pub fn summary_markdown(s: &Summary) -> String {
    use std::fmt::Write;
    let mut md = String::new();
    if !s.summary.trim().is_empty() {
        let _ = writeln!(md, "{}\n", s.summary.trim());
    }
    for sec in s.sections.iter().filter(|x| !x.bullets.is_empty()) {
        let _ = writeln!(md, "### {}", sec.heading.trim());
        for b in &sec.bullets {
            let _ = writeln!(md, "- {}", b.text().trim());
            for sub in b.sub() {
                let _ = writeln!(md, "    - {}", sub.trim());
            }
        }
        md.push('\n');
    }
    if !s.decisions.is_empty() {
        md.push_str("### Decisions\n");
        for d in &s.decisions {
            let _ = writeln!(md, "- {d}");
        }
        md.push('\n');
    }
    if !s.action_items.is_empty() {
        md.push_str("### Action items\n");
        for a in &s.action_items {
            let _ = writeln!(md, "- {}{}", a.owner.as_deref().filter(|o| !o.is_empty()).map(|o| format!("{o}: ")).unwrap_or_default(), a.task);
        }
    }
    md.trim_end().to_string()
}

/// Everything the model needs to run a recipe: (system, user) messages.
pub fn recipe_prompt(recipe: &Entry, meta: &SessionMeta, transcript: &Transcript, summary: Option<&Summary>, vocabulary: &[String], language: &str, me: Option<&str>) -> (String, String) {
    let mut ctx = String::new();
    ctx.push_str("You help the user get work done with a meeting they recorded. You have the meeting's details, its AI-written notes and its automatic transcript. Carry out the user's request (a saved \"recipe\") for this one meeting.\n\n");
    ctx.push_str("Rules:\n- Use only what is in this meeting. Never invent names, numbers, dates or commitments; if something the request needs is missing, say so briefly or mark it TODO.\n- The transcript comes from speech recognition and may contain mistakes; infer intent sensibly.\n- Reply with the result itself in Markdown, ready to paste: no preamble (\"Here is…\"), no closing remarks, no code fence around the whole answer.\n");
    ctx.push_str(&format!("- {}\n", crate::summary::speakers_hint(transcript, me)));
    ctx.push_str(&crate::summary::vocabulary_hint(vocabulary));
    ctx.push_str(&crate::summary::language_hint(language));
    ctx.push_str("\nMEETING\n");
    ctx.push_str(&format!("Title: {}\n", meta.title));
    ctx.push_str(&format!("When: {}\n", meta.started.format("%A %-d %B %Y, %H:%M")));
    if let Some(mins) = meta.duration_mins() {
        ctx.push_str(&format!("Length: {mins} min\n"));
    }
    if let Some(ev) = &meta.event {
        let my_domain = ev.calendar.as_deref().and_then(domain);
        let (mut colleagues, mut guests) = (Vec::new(), Vec::new());
        for a in ev.attendees.iter().filter(|a| ev.calendar.as_deref().is_none_or(|me| !a.email.eq_ignore_ascii_case(me))) {
            let who = match &a.name {
                Some(n) => format!("{n} <{}>", a.email),
                None => a.email.clone(),
            };
            let same = my_domain.is_some() && domain(&a.email) == my_domain && !PERSONAL_DOMAINS.contains(&my_domain.as_deref().unwrap_or(""));
            if same { colleagues.push(who) } else { guests.push(who) }
        }
        if !colleagues.is_empty() {
            ctx.push_str(&format!("The user's colleagues (same organisation, on the user's side): {}\n", colleagues.join(", ")));
        }
        if !guests.is_empty() {
            ctx.push_str(&format!("{}: {}\n", if colleagues.is_empty() && my_domain.is_none() { "Other attendees" } else { "Outside guests" }, guests.join(", ")));
        }
        if !colleagues.is_empty() && !guests.is_empty() {
            ctx.push_str("\"Them\" in the transcript can be colleagues or guests; tell them apart from context, and don't treat colleagues as the customer.\n");
        }
        if let Some(me) = &ev.calendar {
            ctx.push_str(&format!("The user (\"Me\") is {me}\n"));
        }
    }
    if let Some(s) = summary {
        let md = summary_markdown(s);
        if !md.is_empty() {
            ctx.push_str(&format!("\nNOTES (AI-written from the transcript)\n{md}\n"));
        }
    }
    ctx.push_str(&format!("\nTRANSCRIPT\n{}\n", trim_transcript(transcript.to_plain(), 110_000)));
    let user = format!("Recipe \"{}\":\n\n{}", recipe.name, recipe.body.trim());
    (ctx, user)
}

/// Strip reasoning, a wrapping code fence and chatty preambles.
pub fn clean_output(content: &str) -> String {
    let mut s = crate::summary::strip_think(content).trim();
    for fence in ["```markdown", "```md", "```"] {
        if let Some(inner) = s.strip_prefix(fence).and_then(|r| r.trim_end().strip_suffix("```")) {
            if !inner.contains("```") {
                s = inner.trim();
            }
            break;
        }
    }
    s.to_string()
}

/// Run a recipe on a session. Blocking; returns the result (not yet stored).
pub fn run(cfg: &Config, target: &LlmTarget, recipe: &Entry, dir: &Path) -> Result<RecipeResult> {
    anyhow::ensure!(target.cli.is_some() || (!target.endpoint.is_empty() && !target.model.is_empty()), "no AI model set up (Settings → Summaries)");
    let meta = SessionMeta::load(dir).context("reading meta.json")?;
    let (transcript, summary) = crate::session::load_detail(dir);
    let transcript = transcript.filter(|t| !t.is_empty()).context("this meeting has no transcript yet")?;
    let me = crate::identity::Me::resolve(&cfg.me, meta.event.as_ref(), None);
    let terms = crate::vocabulary::Vocabulary::for_session_dir(cfg, &meta, dir).hint_terms(&transcript);
    let (system, user) = recipe_prompt(recipe, &meta, &transcript, summary.as_ref(), &terms, &cfg.llm.language, me.display());
    let text = chat(target, &system, &user, cfg.llm.timeout_secs.max(60))?;
    anyhow::ensure!(!text.trim().is_empty(), "the model returned nothing");
    Ok(RecipeResult { recipe: recipe.id.clone(), name: recipe.name.clone(), text, model: target.model.clone(), created: Local::now(), saved_heading: None })
}

/// One chat completion, plain text out.
pub fn chat(target: &LlmTarget, system: &str, user: &str, timeout_secs: u64) -> Result<String> {
    if let Some(spec) = &target.cli {
        return Ok(clean_output(&crate::cli_llm::run(spec, Some(system), user, timeout_secs)?));
    }
    let url = format!("{}/chat/completions", target.endpoint);
    let mut body = serde_json::json!({
        "model": target.model,
        "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
        "temperature": 0.3,
        "stream": false,
    });
    if !target.reasoning_effort.is_empty() {
        body["reasoning_effort"] = serde_json::json!(target.reasoning_effort);
    }
    if target.local {
        if let Some(k) = &target.keep_alive {
            body["keep_alive"] = serde_json::json!(k);
        }
        body["options"] = serde_json::json!({ "num_ctx": crate::summary::num_ctx_for(&format!("{system}{user}")) });
    }
    let mut req = ureq::post(&url).config().timeout_global(Some(std::time::Duration::from_secs(timeout_secs))).build();
    if let Some(key) = &target.api_key {
        req = req.header("Authorization", &format!("Bearer {key}"));
    }
    let v: serde_json::Value = req
        .send_json(&body)
        .map_err(|e| match e {
            ureq::Error::StatusCode(401 | 403) => anyhow::anyhow!("the API key was rejected"),
            ureq::Error::StatusCode(404) => anyhow::anyhow!("model “{}” not found at {}", target.model, target.endpoint),
            ureq::Error::StatusCode(429) => anyhow::anyhow!("rate limited or out of credits"),
            e => anyhow::Error::from(e),
        })
        .with_context(|| format!("calling {url} (model {})", target.model))?
        .body_mut()
        .read_json()?;
    Ok(clean_output(v["choices"][0]["message"]["content"].as_str().unwrap_or_default()))
}

// ---------------------------------------------------------------- saving into the note

fn heading_level(line: &str) -> Option<usize> {
    let n = line.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&n).then_some(n).filter(|_| line[n..].starts_with(' '))
}

/// Shift Markdown headings so the biggest becomes `###` (they sit under a `##`).
pub fn demote_headings(text: &str) -> String {
    let mut fence = false;
    let mut min = 7;
    for l in text.lines() {
        if l.trim_start().starts_with("```") {
            fence = !fence;
        } else if !fence {
            if let Some(n) = heading_level(l) {
                min = min.min(n);
            }
        }
    }
    if min >= 3 {
        return text.to_string();
    }
    let shift = 3 - min;
    fence = false;
    text.lines()
        .map(|l| {
            if l.trim_start().starts_with("```") {
                fence = !fence;
            }
            match heading_level(l).filter(|_| !fence) {
                Some(n) => format!("{}{}", "#".repeat((n + shift).min(6)), &l[n..]),
                None => l.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The byte range of `## heading` and its body (up to the next `#`/`##` heading).
fn section_range(md: &str, heading: &str) -> Option<(usize, usize)> {
    let mut pos = 0;
    let mut start = None;
    let mut fence = false;
    for line in md.split_inclusive('\n') {
        let l = line.trim_end();
        if l.trim_start().starts_with("```") {
            fence = !fence;
        }
        if !fence {
            if let Some(s) = start {
                if heading_level(l).is_some_and(|n| n <= 2) {
                    return Some((s, pos));
                }
            } else if l.strip_prefix("## ").is_some_and(|h| h.trim().eq_ignore_ascii_case(heading.trim())) {
                start = Some(pos);
            }
        }
        pos += line.len();
    }
    start.map(|s| (s, md.len()))
}

/// Put `## heading` + `body` into a note: replacing a section with that
/// heading, else before the transcript, else at the end.
pub fn insert_section(md: &str, heading: &str, body: &str) -> String {
    let block = format!("## {}\n\n{}\n\n", heading.trim(), demote_headings(body.trim()));
    if let Some((a, b)) = section_range(md, heading) {
        return format!("{}{block}{}", &md[..a], &md[b..]);
    }
    if let Some((a, _)) = section_range(md, "Transcript") {
        return format!("{}{block}{}", &md[..a], &md[a..]);
    }
    let mut out = md.trim_end().to_string();
    out.push_str("\n\n");
    out.push_str(block.trim_end());
    out.push('\n');
    out
}

pub fn remove_section(md: &str, heading: &str) -> String {
    match section_range(md, heading) {
        Some((a, b)) => format!("{}{}", &md[..a], &md[b..]),
        None => md.to_string(),
    }
}

/// Headings Stenly writes itself; a recipe can't take them over.
const RESERVED_HEADINGS: &[&str] = &["Summary", "Action items", "Decisions", "Key points", "Transcript"];

pub fn is_reserved_heading(h: &str) -> bool {
    RESERVED_HEADINGS.iter().any(|r| r.eq_ignore_ascii_case(h.trim()))
}

/// Save a recipe result into the meeting's note under `heading`.
pub fn save_to_note(dir: &Path, recipe: &str, heading: &str) -> Result<PathBuf> {
    anyhow::ensure!(!heading.trim().is_empty(), "the heading is empty");
    anyhow::ensure!(!is_reserved_heading(heading), "“{}” is one of the note's own sections; pick another heading", heading.trim());
    let meta = SessionMeta::load(dir)?;
    let note = meta.note_path.clone().filter(|p| p.exists()).context("this meeting has no note file yet")?;
    let mut all = load_results(dir);
    let r = all.iter_mut().find(|r| r.recipe == recipe).context("no result to save")?;
    let md = std::fs::read_to_string(&note)?;
    let md = match r.saved_heading.as_deref().filter(|h| !h.eq_ignore_ascii_case(heading)) {
        Some(old) => remove_section(&md, old),
        None => md,
    };
    crate::note::write_note(&note, &insert_section(&md, heading, &r.text))?;
    r.saved_heading = Some(heading.trim().to_string());
    save_results(dir, &all)?;
    Ok(note)
}

/// After a note is (re)written, put saved recipe results back in.
pub fn reapply_saved(dir: &Path) -> Result<()> {
    let saved: Vec<RecipeResult> = load_results(dir).into_iter().filter(|r| r.saved_heading.is_some()).collect();
    if saved.is_empty() {
        return Ok(());
    }
    let Some(note) = SessionMeta::load(dir)?.note_path.filter(|p| p.exists()) else { return Ok(()) };
    let mut md = std::fs::read_to_string(&note)?;
    for r in &saved {
        md = insert_section(&md, r.saved_heading.as_deref().unwrap_or(&r.name), &r.text);
    }
    crate::note::write_note(&note, &md)
}

/// The heading a recipe's result goes under in the note.
pub fn default_heading(recipe: &Entry) -> String {
    let h = recipe.field("heading").map(str::to_string).unwrap_or_else(|| recipe.name.clone());
    if is_reserved_heading(&h) { format!("{h} ({})", recipe.name) } else { h }
}

// ---------------------------------------------------------------- sessions

/// A session by path, directory name, "latest", or a unique title fragment.
pub fn resolve_session(audio_dir: &Path, query: &str) -> Result<PathBuf> {
    let q = query.trim();
    let p = crate::util::expand_tilde(q);
    if p.join("meta.json").exists() {
        return Ok(p);
    }
    let all = crate::session::list_sessions(audio_dir, usize::MAX);
    if q.eq_ignore_ascii_case("latest") || q.eq_ignore_ascii_case("last") {
        return all.into_iter().find(|(d, _)| d.join("transcript.json").exists()).map(|(d, _)| d).context("no transcribed meetings yet");
    }
    if let Some((d, _)) = all.iter().find(|(d, _)| d.file_name().is_some_and(|n| n.to_string_lossy() == q)) {
        return Ok(d.clone());
    }
    let ql = q.to_lowercase();
    let hits: Vec<&(PathBuf, SessionMeta)> = all.iter().filter(|(d, m)| m.title.to_lowercase().contains(&ql) || d.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().contains(&ql))).collect();
    match hits.as_slice() {
        [one] => Ok(one.0.clone()),
        [] => anyhow::bail!("no meeting matches {q:?}"),
        many => anyhow::bail!("{q:?} matches {} meetings; use the directory name, e.g. {:?}", many.len(), many[0].0.file_name().unwrap_or_default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Kind;

    fn info<'a>(title: &'a str, attendees: Vec<&'a str>) -> MeetingInfo<'a> {
        MeetingInfo { title, description: "", attendees, me: Some("alex@example.com") }
    }

    #[test]
    fn picks_templates_from_titles() {
        let lib = Library::at(Kind::Template, "/nonexistent");
        let all = lib.all();
        let pick = |t: &str| auto_template(&all, "general", &info(t, vec![]));
        assert_eq!(pick("Alex / Shelley 1:1"), "one-on-one");
        assert_eq!(pick("will<>andy 1-1"), "one-on-one");
        assert_eq!(pick("Daily Standup"), "stand-up");
        assert_eq!(pick("Eng stand-up"), "stand-up");
        assert_eq!(pick("Acme <> Vuon demo"), "sales-call");
        assert_eq!(pick("Product demos"), "sales-call");
        assert_eq!(pick("Customer interview: Jane"), "user-interview");
        assert_eq!(pick("Interview: Senior Engineer"), "hiring-interview");
        assert_eq!(pick("Sprint 14 retro"), "retro");
        assert_eq!(pick("Incident post-mortem"), "retro");
        assert_eq!(pick("Seed pitch with Sequoia"), "pitch");
        assert_eq!(pick("Weekly sync"), "team-meeting");
        assert_eq!(pick("Project Atlas kickoff"), "project-kickoff");
        assert_eq!(pick("Discovery call – Acme"), "customer-discovery");
        // no false hits on fragments
        assert_eq!(pick("Design review"), "general");
        assert_eq!(pick("Meeting 15:20"), "general");
        assert_eq!(pick("Retrofit planning"), "general");
        assert_eq!(pick("svc migration"), "general");
        assert_eq!(auto_template(&all, "team-meeting", &info("Design review", vec![])), "team-meeting");
        assert_eq!(auto_template(&all, "deleted-one", &info("Design review", vec![])), "general");
    }

    #[test]
    fn two_colleagues_is_a_one_on_one() {
        let lib = Library::at(Kind::Template, "/nonexistent");
        let all = lib.all();
        assert_eq!(auto_template(&all, "general", &info("Roadmap", vec!["alex@example.com", "jo@example.com"])), "one-on-one");
        // external guest, a crowd, or personal addresses: not a 1:1
        assert_eq!(auto_template(&all, "general", &info("Roadmap", vec!["alex@example.com", "andy@guest.example.org"])), "general");
        assert_eq!(auto_template(&all, "general", &info("Roadmap", vec!["alex@example.com", "jo@example.com", "al@example.com"])), "general");
        assert_eq!(auto_template(&all, "general", &info("Roadmap", vec!["a@gmail.com", "b@gmail.com"])), "general");
        // the title still wins
        assert_eq!(auto_template(&all, "general", &info("Sprint retro", vec!["alex@example.com", "jo@example.com"])), "retro");
    }

    #[test]
    fn description_counts_after_title() {
        let lib = Library::at(Kind::Template, "/nonexistent");
        let all = lib.all();
        let i = MeetingInfo { title: "Jane Doe", description: "Phone screen for the Senior Engineer role", attendees: vec![], me: None };
        assert_eq!(auto_template(&all, "general", &i), "hiring-interview");
    }

    #[test]
    fn general_builtin_adds_no_instructions() {
        let lib = Library::at(Kind::Template, "/nonexistent");
        assert!(template_instructions(&lib.get("general").unwrap()).is_none());
        assert!(template_instructions(&lib.get("retro").unwrap()).is_some());
        let mut g = lib.get("general").unwrap();
        g.origin = Origin::Modified;
        assert!(template_instructions(&g).is_some());
    }

    #[test]
    fn sections_in_notes() {
        let note = "---\ntitle: x\n---\n\n# X\n\n## Summary\n\nHi.\n\n## Transcript\n\n> stuff\n";
        let a = insert_section(note, "Follow-up email", "# Subject\nHello\n## Body\n- x");
        assert!(a.contains("## Summary\n\nHi.\n\n## Follow-up email\n\n### Subject\nHello\n#### Body\n- x\n\n## Transcript"), "{a}");
        // replace in place
        let b = insert_section(&a, "Follow-up email", "Shorter.");
        assert!(b.contains("## Follow-up email\n\nShorter.\n\n## Transcript"), "{b}");
        assert_eq!(b.matches("Follow-up email").count(), 1);
        assert_eq!(section_range(&b, "FOLLOW-UP EMAIL"), section_range(&b, "Follow-up email"));
        // remove
        assert_eq!(remove_section(&b, "Follow-up email"), note);
        // no transcript: append
        let c = insert_section("# X\n\nbody\n", "TL;DR", "short");
        assert_eq!(c, "# X\n\nbody\n\n## TL;DR\n\nshort\n");
        // headings inside code fences are left alone
        assert_eq!(demote_headings("# A\n```\n# not a heading\n```"), "### A\n```\n# not a heading\n```");
    }

    #[test]
    fn recipe_context_separates_colleagues() {
        let ev: crate::calendar::CalendarEvent = serde_json::from_value(serde_json::json!({
            "id": "x", "uid": "x", "title": "Andy / Shelley", "start": "2026-10-22T21:00:00Z", "end": "2026-10-22T21:30:00Z",
            "attendees": [{"name": null, "email": "alex@example.com", "status": null}, {"name": null, "email": "shelley@example.com", "status": null}, {"name": "Andy", "email": "andy@partner.example.org", "status": null}],
            "calendar": "alex@example.com", "declined": false, "recurring": false, "link": null, "organizer": null, "description": null
        }))
        .unwrap();
        let meta = SessionMeta { title: "Andy / Shelley".into(), started: Local::now(), ended: None, event: Some(ev), state: crate::session::SessionState::Done, note_path: None, error: None, routes: vec![], sends: vec![], shares: vec![], meeting_app: None };
        let t = Transcript { segments: vec![crate::transcript::Segment::new(crate::platform::Channel::Mic, 0., 1., "hi")], in_person: false, ..Default::default() };
        let r = Library::at(Kind::Recipe, "/nonexistent").get("follow-up-email").unwrap();
        let (sys, user) = recipe_prompt(&r, &meta, &t, None, &[], "Japanese", None);
        assert!(sys.contains("colleagues (same organisation, on the user's side): shelley@example.com"), "{sys}");
        assert!(sys.contains("Outside guests: Andy <andy@partner.example.org>"));
        assert!(sys.contains("The user (\"Me\") is alex@example.com"));
        assert!(sys.contains("in Japanese"));
        assert!(sys.contains("Me [00:00]: hi"));
        assert!(user.starts_with("Recipe \"Follow-up email\":"));
    }

    #[test]
    fn cleans_model_output() {
        assert_eq!(clean_output("<think>\nhm\n</think>\n\n```markdown\n## Hi\n- x\n```"), "## Hi\n- x");
        assert_eq!(clean_output("plain"), "plain");
        assert_eq!(clean_output("```\na\n```\ntext\n```\nb\n```"), "```\na\n```\ntext\n```\nb\n```");
    }

    #[test]
    fn results_and_note_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("note.md");
        std::fs::write(&note, "# M\n\n## Transcript\n\n> hi\n").unwrap();
        let meta = SessionMeta {
            title: "M".into(),
            started: Local::now(),
            ended: None,
            event: None,
            state: crate::session::SessionState::Done,
            note_path: Some(note.clone()),
            error: None,
            routes: vec![],
            sends: vec![],
            shares: vec![],
            meeting_app: None,
        };
        meta.save(dir.path()).unwrap();
        let r = |text: &str| RecipeResult { recipe: "tldr".into(), name: "TL;DR".into(), text: text.into(), model: "m".into(), created: Local::now(), saved_heading: None };
        push_result(dir.path(), r("one")).unwrap();
        save_to_note(dir.path(), "tldr", "TL;DR").unwrap();
        assert!(std::fs::read_to_string(&note).unwrap().contains("## TL;DR\n\none\n\n## Transcript"));
        // re-running updates the saved copy in the note
        push_result(dir.path(), r("two")).unwrap();
        assert_eq!(load_results(dir.path()).len(), 1);
        let md = std::fs::read_to_string(&note).unwrap();
        assert!(md.contains("## TL;DR\n\ntwo\n") && !md.contains("one"), "{md}");
        // the note gets rewritten from scratch (regenerate): saved sections come back
        std::fs::write(&note, "# M\n\n## Transcript\n\n> hi\n").unwrap();
        reapply_saved(dir.path()).unwrap();
        assert!(std::fs::read_to_string(&note).unwrap().contains("## TL;DR\n\ntwo"));
        assert!(save_to_note(dir.path(), "tldr", "action items").is_err());
        remove_result(dir.path(), "tldr").unwrap();
        assert_eq!(std::fs::read_to_string(&note).unwrap(), "# M\n\n## Transcript\n\n> hi\n");
        assert!(!dir.path().join(RESULTS_FILE).exists());
    }

    #[test]
    fn chosen_template_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(chosen_template(dir.path()), None);
        set_chosen_template(dir.path(), Some("retro")).unwrap();
        assert_eq!(chosen_template(dir.path()).as_deref(), Some("retro"));
        set_chosen_template(dir.path(), None).unwrap();
        assert_eq!(chosen_template(dir.path()), None);
    }
}
