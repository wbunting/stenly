//! Meeting summaries via any OpenAI-compatible chat endpoint (Ollama by default).

use crate::config::LlmConfig;
use crate::transcript::Transcript;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Summary {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: String,
    /// Granola-style topic sections with (optionally nested) bullets.
    #[serde(default)]
    pub sections: Vec<Section>,
    #[serde(default)]
    pub key_points: Vec<String>,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub action_items: Vec<ActionItem>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Id of the note template it was written with (`None` = General).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Section {
    #[serde(default)]
    pub heading: String,
    #[serde(default)]
    pub bullets: Vec<Bullet>,
}

/// A bullet; models sometimes return plain strings, sometimes {text, sub}.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Bullet {
    Plain(String),
    Nested {
        text: String,
        #[serde(default)]
        sub: Vec<String>,
    },
}

impl Bullet {
    pub fn text(&self) -> &str {
        match self {
            Bullet::Plain(t) => t,
            Bullet::Nested { text, .. } => text,
        }
    }
    pub fn sub(&self) -> &[String] {
        match self {
            Bullet::Plain(_) => &[],
            Bullet::Nested { sub, .. } => sub,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ActionItem {
    #[serde(default)]
    pub owner: Option<String>,
    pub task: String,
}

const DEFAULT_PROMPT: &str = r#"You are an expert meeting note-taker. Below is {source} of a meeting. {title_hint}
{speakers}
The transcript may contain recognition errors; infer intent sensibly and never invent facts.

Write notes the way a sharp colleague would, grouped into topic sections with short
headings. {detail}
Each bullet is a self-contained point that carries the substance (a full statement with
the actual value or conclusion in it, NOT a bare label like "Launch date" with the value
hidden in a sub-bullet). Put the specifics that back a point (numbers, examples, options
considered, reasons, who raised it) in its "sub" list; a bullet with nothing to add has no "sub".
Keep concrete numbers, names, dates, companies, products and competitors. Leave out
greetings, scheduling chatter and small talk unless it matters to the business discussed.
Fix obvious transcription errors of product and model names when the context makes
the intended word clear.
{faithfulness}{context}{vocabulary}{template}{language}

Respond with ONLY a JSON object (no prose, no code fence) with these keys. The
angle-bracket text describes each value; replace it with real content from this meeting:
{
  "title": <3-6 word title naming the main topic actually discussed; no dates or times>,
  "summary": <1-2 sentence overview of what was discussed and decided>,
  "sections": [
    {"heading": <short topic name>, "bullets": [{"text": <one self-contained point>, "sub": [<supporting detail>, ...]}, ...]},
    ...
  ],
  "decisions": [<a decision the participants explicitly made>, ...],
  "action_items": [{"owner": <who will do it, by name; "Them" or a speaker label only when it can't be told>, "task": <what they will do>}, ...],
  "tags": [<one-or-two-word topic>, ...]
}
Use empty arrays when there is nothing to report.

Transcript:
{transcript}"#;

/// Notes on one part of a long meeting; merged by [`DEFAULT_PROMPT`] afterwards.
/// Plain Markdown rather than JSON: nothing to fail parsing, and small local
/// models write denser notes this way.
const PART_PROMPT: &str = r#"You are taking detailed notes on one part of a long meeting; notes on the other parts are taken separately and merged afterwards. This is part {part} of {parts}, covering {from}-{to} of the recording. {title_hint}
{speakers}
The transcript may contain recognition errors; infer intent sensibly and never invent facts.

Capture EVERY substantive point made in this part: facts, numbers, names, companies,
products, competitors, opinions and who holds them, questions raised, options, plans,
decisions and commitments. Dense, specific working notes: a point that is left out here
is lost from the final notes. Leave out greetings and small talk unless it matters to the
business discussed. A topic that started in an earlier part may continue here; just note
what is said in this part.
{faithfulness}{context}{vocabulary}
Reply with the notes only, in this Markdown format, no preamble:
## <topic>
- <a full statement with the specifics in it, and who said it when that matters>
  - <supporting detail>
## Decisions
- <only what the participants explicitly agreed>
## Action items
- <owner's name>: <what they will do>
Leave out the Decisions and Action items headings when there are none.

Transcript (part {part} of {parts}):
{transcript}"#;

/// Rules that keep the notes true to what was said. Part of every summary prompt.
const FAITHFULNESS: &str = "
FAITHFULNESS. These rules beat everything else:
- Quote numbers exactly as spoken: \"about 20%\" stays \"about 20%\", never a range, a rounder or bigger number, or a number nobody said.
- Keep who said what. A recommendation, opinion, example or claim belongs to the person who made it; an example the recorder gave is not the guest's recommendation.
- A suggestion, idea or \"we could\" is not a decision. Decisions are only what the participants explicitly agreed or committed to.
- Never upgrade or downgrade a claim: \"maybe\" stays tentative, \"needs\" stays required, a \"no\" stays a \"no\". If a point was corrected later in the meeting, report the corrected version.
- Action items are only things someone said they (or someone else) will do; don't turn discussion topics into tasks.
";

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    temperature: f32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keep_alive: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<serde_json::Value>,
}

/// Context window to request from Ollama: the prompt (~3.5 chars/token for
/// transcripts) plus room for a long JSON answer, rounded up to a power of two.
pub(crate) fn num_ctx_for(prompt: &str) -> u32 {
    let need = (prompt.len() as f64 / 3.2) as u32 + 8192;
    need.next_power_of_two().clamp(8192, 131_072)
}

/// Who the labels in the transcript are.
pub(crate) fn speakers_hint(t: &Transcript, me: Option<&str>) -> String {
    if t.has_names() {
        return crate::speakers::summary_hint(t, me);
    }
    let separated = t.segments.iter().any(|s| s.speaker.is_some());
    match (t.in_person, separated) {
        (true, true) => "This was an IN-PERSON meeting: several people in one room, recorded on a single microphone. \
Voices were told apart automatically and labelled \"Speaker 1\", \"Speaker 2\", … — the labels may occasionally be wrong \
and one of them is the person who recorded it. Use people's real names when the conversation reveals them (e.g. someone is \
addressed by name); otherwise refer to roles or \"the team\". Never attribute everything to one person."
            .to_string(),
        (true, false) => "This was an IN-PERSON meeting: several people in one room, recorded on a single microphone, so \
every line is labelled \"Me\" even though different people are speaking. Work out from context who is saying what and \
use their names when the conversation reveals them. Don't attribute every statement or action item to the recorder."
            .to_string(),
        _ => {
            let label = t.recorder_label();
            let recorder = match me {
                Some(m) if m != label => format!(" ({m})"),
                _ => String::new(),
            };
            format!("\"{label}\" is the person who recorded it{recorder}; \"Them\" is everyone else on the call (possibly several people). The recorder's action items are owned by \"{label}\".")
        }
    }
}

/// Action items the model gave to "Me"/"You"/"I" belong to the recorder: use
/// the label their lines carry ("Alex"). No-op while that label is "Me".
pub fn own_action_items(s: &mut Summary, recorder: &str) {
    if recorder == crate::voices::SELF_NAME {
        return;
    }
    for a in s.action_items.iter_mut() {
        if let Some(o) = a.owner.as_mut().filter(|o| matches!(o.trim().to_lowercase().as_str(), "me" | "you" | "i" | "myself" | "the recorder" | "recorder")) {
            *o = recorder.to_string();
        }
    }
}

pub(crate) fn vocabulary_hint(words: &[String]) -> String {
    let words: Vec<&str> = words.iter().map(|w| w.trim()).filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return String::new();
    }
    format!(
        "\nVOCABULARY. The speakers use these names and terms, and speech recognition often mishears them as sound-alikes \
(e.g. a company \"Vuon\" transcribed as \"Viewon\", \"View on\" or \"Vueon\"; a person \"Forrest\" as \"Forest\"). Whenever the \
transcript has a word or phrase that sounds like one of these, write the term exactly as spelled here, in every heading, bullet, \
title and action item. A note in parentheses says what the term is or how it tends to be misheard; it is not part of the name. \
Don't change words that are plainly meant as ordinary English (\"cloud\" is usually the cloud, not Claude): {}.\n",
        words.join("; ")
    )
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}
#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
}
#[derive(Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
}

/// Longest text sent in one request. Transcripts longer than
/// [`CHUNK_OVER_CHARS`] are summarized part by part, so this only binds the
/// merge step of extremely long meetings (and is logged when it does).
const MAX_TRANSCRIPT_CHARS: usize = 120_000;
/// Meetings longer than this (by time or by text) are summarized in parts and
/// merged: in one pass, models cover the start well and skim or drop the rest.
const CHUNK_OVER_SECS: f64 = 25.0 * 60.0;
const CHUNK_OVER_CHARS: usize = 36_000;
/// Size of one part: about 12 minutes of talk, cut between turns.
const PART_SECS: f64 = 12.0 * 60.0;
const PART_MAX_CHARS: usize = 20_000;
/// Parts summarized at once by a hosted model.
const PARALLEL_PARTS: usize = 4;
/// Pause before retrying a part whose call failed.
const PART_RETRY_SECS: u64 = 5;
/// A part whose notes failed is given to the merge as transcript, up to this
/// long (parts are cut at about [`PART_MAX_CHARS`], so this keeps them whole).
const PART_FALLBACK_CHARS: usize = PART_MAX_CHARS * 3 / 2;

pub fn summarize(cfg: &LlmConfig, title: &str, transcript: &Transcript) -> Result<Summary> {
    summarize_with(cfg, &cfg.target(crate::secrets::llm_api_key), &[], title, transcript)
}

/// Summarize with an explicit target and the user's vocabulary (names,
/// products, jargon the transcript probably misspells).
pub fn summarize_with(cfg: &LlmConfig, target: &crate::config::LlmTarget, vocabulary: &[String], title: &str, transcript: &Transcript) -> Result<Summary> {
    summarize_opts(cfg, target, &Options { vocabulary, ..Default::default() }, title, transcript)
}

/// Extra instructions for one summary.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options<'a> {
    /// Names, products and jargon the transcript probably misspells.
    pub vocabulary: &'a [String],
    /// Note template: (name, instructions). `None` = the default layout.
    pub template: Option<(&'a str, &'a str)>,
    /// Write the notes in this language ("" = the meeting's own language).
    pub language: &'a str,
    /// The recorder's name, so "Me" becomes a person in the notes.
    pub me: Option<&'a str>,
    /// Everyone else known to be in the meeting (invite, meeting app), for
    /// attributing points and action items when the transcript only says "Them".
    pub people: &'a [String],
    /// Background on the meeting ([`crate::context::MeetingContext::prompt_block`]); "" = none.
    pub context: &'a str,
}

/// The prompt block for a note template.
pub fn template_hint(name: &str, instructions: &str) -> String {
    let body = instructions.trim();
    if body.is_empty() {
        return String::new();
    }
    format!(
        "\nNOTE TEMPLATE \"{name}\". The user chose this template for the notes; follow it, and where it differs from the \
guidance above, it wins. Its \"## \" headings are the sections to write, in this order, used as the \"heading\" values \
(replace text in <angle brackets> with the real name, e.g. a person or topic, and repeat that section as needed). The line \
under each heading says what belongs there. Only put a point in a section if it is genuinely about that section's topic \
(e.g. a \"Pricing\" section holds only prices, budgets and terms that were actually discussed). Leave a section out \
entirely when nothing in the meeting fits it; never fill it with points that belong elsewhere. Points that fit no section \
go in one extra section with a fitting heading. Apply any style rules it gives to every bullet.\n<template>\n{body}\n</template>\n"
    )
}

pub fn language_hint(language: &str) -> String {
    let l = language.trim();
    if l.is_empty() {
        return String::new();
    }
    format!("\nLANGUAGE. Write the title, summary, headings, bullets, decisions, action items and tags in {l}, whatever language the meeting was in. Keep names, product names and quotes as spoken.\n")
}

/// "Andy (guest, partner.example.org)" / "Shelley (colleague of the recorder)":
/// which side of the table someone is on, from their email domain.
pub fn describe_person(name: &str, email: Option<&str>, my_emails: &[String]) -> String {
    use crate::destinations::people::{domain, is_free_domain, same_org};
    let org = |e: &str| domain(e).filter(|d| !is_free_domain(d));
    let Some(d) = email.and_then(org) else { return name.to_string() };
    if my_emails.iter().filter_map(|e| org(e)).any(|m| same_org(&m, &d)) {
        format!("{name} (colleague of the recorder)")
    } else {
        format!("{name} (guest, {d})")
    }
}

/// Who to name in the notes, and how to attribute points and action items.
pub(crate) fn attribution_hint(t: &Transcript, me: Option<&str>, people: &[String]) -> String {
    let mut h = String::from("\nATTRIBUTION. ");
    // `people` leaves the recorder out (see `Participants::gather`)
    let others: Vec<&str> = people.iter().map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
    match (me, others.is_empty()) {
        (Some(me), false) => h.push_str(&format!("People in this meeting: {me} (who recorded it, \"Me\"), {}. ", others.join(", "))),
        (Some(me), true) => h.push_str(&format!("{me} recorded it (\"Me\"). ")),
        (None, false) => h.push_str(&format!("Other people in this meeting: {}. ", others.join(", "))),
        (None, true) => {}
    }
    if !t.has_names() {
        h.push_str(if others.len() > 1 {
            "\"Them\" lines can be any of the others: tell who is talking from context (names used, self-introductions, their company or role) and name them only when you are sure. "
        } else {
            "Call \"Them\" by name only when the conversation makes it certain who is speaking (they introduce themselves or are addressed by name); someone the speakers talk about is usually NOT the person speaking. When unsure, keep \"Them\". "
        });
    }
    h.push_str("Name people (first names are fine) when it matters who said, proposed or wants something. ");
    // the recorder's lines are labelled "Me", or with their name once speakers were identified
    let label = t.recorder_label();
    let mine = if label == crate::voices::SELF_NAME { "a \"Me\" line".to_string() } else { format!("a \"{label}\" line") };
    h.push_str("Every action item's owner is the person who will do it: whoever says \"I'll create the tickets\" owns that task, ");
    match me {
        Some(me) => {
            let first = me.split_whitespace().next().unwrap_or(me);
            h.push_str(&format!("so \"I'll …\" on {mine} is {first}'s task and \"I'll …\" on a \"Them\" line is not. Write \"{first}\" for the recorder, never \"Me\" or \"You\""));
        }
        None => h.push_str(&format!("so \"I'll …\" on {mine} is the recorder's (\"{label}\") task and \"I'll …\" on a \"Them\" line is not")),
    }
    h.push_str(". Likewise, facts about a guest's company (its users, stack, plans) belong to that company, not to the recorder's.\n");
    h
}

/// How much to write, scaled to the length of the meeting.
pub(crate) fn detail_hint(minutes: f64) -> String {
    let m = minutes.round().max(1.0) as u32;
    if m < 8 {
        return format!("This is a short meeting (about {m} min): one or two sections with only the points that matter.");
    }
    let sections = (m / 9 + 2).clamp(3, 10);
    let (lo, hi) = (m * 2 / 5 + 4, m * 7 / 10 + 6);
    format!(
        "This meeting ran about {m} min, so write about {lo}-{hi} points in total (bullets plus sub-bullets) in up to {sections} sections, \
as many as the content supports: long meetings need long notes, and every substantive topic, fact, number and name should make it \
in. Never pad with filler or repeat a point."
    )
}

/// Length of a transcript in minutes (first word to last).
fn minutes(t: &Transcript) -> f64 {
    match (t.segments.first(), t.segments.last()) {
        (Some(a), Some(b)) => (b.end - a.start).max(0.0) / 60.0,
        _ => 0.0,
    }
}

/// Split a long transcript into parts of about [`PART_SECS`] / [`PART_MAX_CHARS`]
/// each, evenly sized, cut between turns (preferably where the speaker changes).
/// Returns segment index ranges covering every segment exactly once.
pub(crate) fn split_parts(t: &Transcript) -> Vec<std::ops::Range<usize>> {
    let segs = &t.segments;
    if segs.is_empty() {
        return Vec::new();
    }
    let len = |s: &crate::transcript::Segment| s.plain_line().len() + 1;
    let chars: usize = segs.iter().map(len).sum();
    let secs = minutes(t) * 60.0;
    let n = ((secs / PART_SECS).ceil() as usize).max(chars.div_ceil(PART_MAX_CHARS)).max(1);
    if n == 1 {
        return std::iter::once(0..segs.len()).collect();
    }
    let (want_secs, want_chars) = (secs / n as f64, chars / n);
    let mut out = Vec::new();
    let (mut start, mut part_chars) = (0, 0);
    for i in 0..segs.len() {
        part_chars += len(&segs[i]);
        let Some(next) = segs.get(i + 1) else { break };
        let full = |k: f64| next.start - segs[start].start >= want_secs * k || part_chars as f64 >= want_chars as f64 * k;
        // at the target size, cut at a change of speaker; past 1.2x, cut anywhere
        let speaker_change = next.label() != segs[i].label();
        if out.len() + 1 < n && (full(1.0) && speaker_change || full(1.2)) {
            out.push(start..i + 1);
            start = i + 1;
            part_chars = 0;
        }
    }
    out.push(start..segs.len());
    out
}

/// Fill the summary prompt's placeholders. `prompt` is [`DEFAULT_PROMPT`] or
/// the user's `llm.prompt_file`; a custom prompt without the template/language
/// placeholders still gets them, just before the transcript.
fn fill_prompt(prompt: &str, h: &Hints, source: &str, detail: &str, body: &str) -> String {
    let mut prompt = prompt.to_string();
    for (ph, hint) in [("{context}", &h.context), ("{template}", &h.template), ("{language}", &h.language)] {
        if !prompt.contains(ph) && !hint.is_empty() {
            prompt = match prompt.rfind("Transcript:") {
                Some(i) => format!("{}{ph}\n{}", &prompt[..i], &prompt[i..]),
                None => format!("{ph}\n{prompt}"),
            };
        }
    }
    prompt
        .replace("{source}", source)
        .replace("{detail}", detail)
        .replace("{faithfulness}", FAITHFULNESS)
        .replace("{context}", &h.context)
        .replace("{template}", &h.template)
        .replace("{language}", &h.language)
        .replace("{speakers}", &h.speakers)
        .replace("{title_hint}", &h.title)
        .replace("{title}", &h.raw_title)
        .replace("{vocabulary}", &h.vocabulary)
        .replace("{transcript}", body)
}

/// Prompt for the notes on one part of a long meeting.
fn part_prompt(h: &Hints, part: usize, parts: usize, segs: &[crate::transcript::Segment]) -> String {
    let body = segs.iter().map(|s| s.plain_line()).collect::<Vec<_>>().join("\n");
    let from = segs.first().map(|s| crate::util::fmt_timestamp(s.start)).unwrap_or_default();
    let to = segs.last().map(|s| crate::util::fmt_timestamp(s.end)).unwrap_or_default();
    PART_PROMPT
        .replace("{part}", &(part + 1).to_string())
        .replace("{parts}", &parts.to_string())
        .replace("{from}", &from)
        .replace("{to}", &to)
        .replace("{faithfulness}", FAITHFULNESS)
        .replace("{context}", &h.context)
        .replace("{speakers}", &h.speakers)
        .replace("{title_hint}", &h.title)
        .replace("{vocabulary}", &h.vocabulary)
        .replace("{transcript}", &body)
}

/// The per-meeting blocks shared by every prompt.
struct Hints {
    raw_title: String,
    title: String,
    speakers: String,
    vocabulary: String,
    template: String,
    language: String,
    context: String,
}

impl Hints {
    fn new(opts: &Options, title: &str, transcript: &Transcript) -> Self {
        let title_hint = if is_placeholder_title(title) {
            "It has no title yet: write one from the content.".to_string()
        } else {
            format!("Its calendar title is \"{title}\"; keep it as the title unless it says nothing about the content.")
        };
        Hints {
            raw_title: title.to_string(),
            title: title_hint,
            speakers: format!("{}{}", speakers_hint(transcript, opts.me), attribution_hint(transcript, opts.me, opts.people)),
            vocabulary: vocabulary_hint(opts.vocabulary),
            template: opts.template.map(|(n, b)| template_hint(n, b)).unwrap_or_default(),
            language: language_hint(opts.language),
            context: opts.context.to_string(),
        }
    }
}

/// Whether a transcript is summarized in parts (see [`CHUNK_OVER_SECS`]).
pub fn needs_parts(t: &Transcript) -> bool {
    minutes(t) * 60.0 > CHUNK_OVER_SECS || t.to_plain().len() > CHUNK_OVER_CHARS
}

/// Keep `text` within `max` chars, saying so in the log: never cut silently.
fn cap(text: String, max: usize, what: &str) -> String {
    if text.len() <= max {
        return text;
    }
    tracing::warn!(chars = text.len(), max, "{what} is too long for one request; keeping the start and end, the middle is left out of the summary");
    let head: String = text.chars().take(max * 2 / 3).collect();
    let tail: String = text.chars().rev().take(max / 3).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}\n[... middle omitted: too long ...]\n{tail}")
}

/// Summarize with every option (template, language, vocabulary). Long
/// meetings are summarized part by part (dense notes per ~12 minutes), then
/// merged into one set of notes, so the end of the meeting isn't lost.
pub fn summarize_opts(cfg: &LlmConfig, target: &crate::config::LlmTarget, opts: &Options, title: &str, transcript: &Transcript) -> Result<Summary> {
    if target.endpoint.is_empty() || target.model.is_empty() {
        anyhow::bail!("no summary model set up (Settings → Summaries)");
    }
    let base = if cfg.prompt_file.trim().is_empty() {
        DEFAULT_PROMPT.to_string()
    } else {
        std::fs::read_to_string(crate::util::expand_tilde(&cfg.prompt_file)).context("reading llm.prompt_file")?
    };
    let h = Hints::new(opts, title, transcript);
    let detail = detail_hint(minutes(transcript));
    let prompt = if !needs_parts(transcript) {
        let plain = cap(transcript.to_plain(), MAX_TRANSCRIPT_CHARS, "the transcript");
        fill_prompt(&base, &h, "an automatic transcript", &detail, &plain)
    } else {
        let parts = split_parts(transcript);
        tracing::info!(parts = parts.len(), minutes = minutes(transcript).round(), "long meeting: summarizing in parts");
        // keep a local model loaded between parts; the last call applies keep_alive
        let warm = crate::config::LlmTarget { keep_alive: target.local.then(|| "5m".to_string()), ..target.clone() };
        let n = parts.len();
        let head = |i: usize| {
            let segs = &transcript.segments[parts[i].clone()];
            let (from, to) = (segs.first().map_or(0.0, |s| s.start), segs.last().map_or(0.0, |s| s.end));
            format!("=== Part {} of {n} ({}-{}) ===", i + 1, crate::util::fmt_timestamp(from), crate::util::fmt_timestamp(to))
        };
        // a part without notes: the merge reads its transcript instead
        let fallback = |i: usize| {
            let raw = transcript.segments[parts[i].clone()].iter().map(|s| s.plain_line()).collect::<Vec<_>>().join("\n");
            format!("{} (raw transcript: notes unavailable)\n{}\n\n", head(i), cap(raw, PART_FALLBACK_CHARS, "a part's transcript"))
        };
        let one = |i: usize| -> Result<String> {
            let text = part_notes(cfg, &warm, &part_prompt(&h, i, n, &transcript.segments[parts[i].clone()]))?;
            tracing::debug!(part = i + 1, chars = text.len(), "part notes");
            Ok(format!("{}\n{text}\n\n", head(i)))
        };
        // hosted APIs take a few parts at once; a local model runs one request at a time anyway
        let at_once = if target.local { 1 } else { PARALLEL_PARTS };
        let mut notes = String::new();
        let mut failed: Vec<anyhow::Error> = Vec::new();
        for batch in (0..n).collect::<Vec<_>>().chunks(at_once) {
            let done: Vec<(usize, Result<String>)> = std::thread::scope(|sc| {
                let hs: Vec<_> = batch.iter().map(|&i| (i, sc.spawn(move || one(i)))).collect();
                hs.into_iter().map(|(i, h)| (i, h.join().unwrap_or_else(|_| Err(anyhow::anyhow!("summarizing part {} panicked", i + 1))))).collect()
            });
            for (i, r) in done {
                match r {
                    Ok(text) => notes.push_str(&text),
                    // one failed call degrades the notes instead of losing the whole summary
                    Err(e) => {
                        tracing::warn!(part = i + 1, error = %format!("{e:#}"), "no notes for this part; the merge reads its transcript instead");
                        notes.push_str(&fallback(i));
                        failed.push(e);
                    }
                }
            }
        }
        if failed.len() == n {
            // every part failed (a bad key, an unknown model, the server down): report why
            return Err(failed.swap_remove(0).context("the summary model failed on every part of the meeting"));
        }
        let notes = cap(notes, MAX_TRANSCRIPT_CHARS, "the notes on the meeting's parts");
        let source = "detailed notes taken part by part from the automatic transcript (the meeting was too long to read in one go). \
Merge them into one set of notes: a topic discussed in several parts becomes one section, duplicates are said once, and every \
specific point, number and name in the notes is kept (don't shorten by dropping details). Order sections by importance, not by part, \
and never mention the parts in the notes";
        // a custom prompt that doesn't say what it is reading still learns it
        let body = if base.contains("{source}") { notes.trim_end().to_string() } else { format!("(The meeting was long, so these are {source}.)\n\n{}", notes.trim_end()) };
        fill_prompt(&base, &h, source, &detail, &body)
    };
    let content = chat(cfg, target, &prompt, true)?;
    parse_summary(&content).with_context(|| format!("model returned unparseable output: {}", content.chars().take(300).collect::<String>()))
}

/// Notes on one part, retried once after a pause (a timeout or rate limit is
/// often gone a moment later). A bad key or unknown model fails at once.
fn part_notes(cfg: &LlmConfig, target: &crate::config::LlmTarget, prompt: &str) -> Result<String> {
    let mut last = None;
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(PART_RETRY_SECS));
        }
        match chat(cfg, target, prompt, false) {
            Ok(text) => return Ok(strip_think(&text).trim().to_string()),
            Err(e) if is_permanent(&e) => return Err(e),
            Err(e) => {
                tracing::debug!(error = %format!("{e:#}"), attempt, "part notes failed");
                last = Some(e);
            }
        }
    }
    Err(last.expect("two attempts"))
}

/// Errors a retry won't fix (marked [`Permanent`] by [`chat`]).
fn is_permanent(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Permanent>().is_some()
}

/// The answer after a reasoning model's `<think>…</think>` block.
pub fn strip_think(s: &str) -> &str {
    match s.rfind("</think>") {
        Some(i) => &s[i + 8..],
        None => s,
    }
}

/// One chat completion for a summary prompt; returns the reply text.
fn chat(cfg: &LlmConfig, target: &crate::config::LlmTarget, prompt: &str, json: bool) -> Result<String> {
    let url = format!("{}/chat/completions", target.endpoint);
    let call = |json_mode: bool| -> Result<ChatResponse, ureq::Error> {
        let body = ChatRequest {
            model: &target.model,
            messages: vec![ChatMessage { role: "user", content: prompt }],
            temperature: 0.2,
            stream: false,
            response_format: json_mode.then(|| serde_json::json!({"type": "json_object"})),
            reasoning_effort: Some(target.reasoning_effort.as_str()).filter(|s| !s.is_empty()),
            keep_alive: target.keep_alive.as_deref(),
            // Ollama defaults to a small context window and silently drops the
            // start of long transcripts; ask for room for the whole meeting.
            options: target.local.then(|| serde_json::json!({ "num_ctx": num_ctx_for(prompt) })),
        };
        let mut req = ureq::post(&url).config().timeout_global(Some(std::time::Duration::from_secs(cfg.timeout_secs.max(30)))).build();
        if let Some(key) = &target.api_key {
            req = req.header("Authorization", &format!("Bearer {key}"));
        }
        req.send_json(&body)?.body_mut().read_json()
    };
    // JSON mode helps when supported; some backends (e.g. Ollama's MLX runner)
    // reject it with 4xx/501, so fall back to plain prompting.
    let resp = match call(json) {
        Err(ureq::Error::StatusCode(code)) if json && (400..=501).contains(&code) && code != 401 && code != 403 && code != 404 => {
            tracing::debug!(code, "endpoint rejected response_format; retrying without");
            call(false)
        }
        r => r,
    }
    .map_err(|e| match e {
        ureq::Error::StatusCode(401 | 403) => anyhow::Error::new(Permanent("the API key was rejected".into())),
        ureq::Error::StatusCode(404) => anyhow::Error::new(Permanent(format!("model “{}” not found at {}", target.model, target.endpoint))),
        ureq::Error::StatusCode(429) => anyhow::anyhow!("rate limited or out of credits"),
        e => anyhow::Error::from(e),
    })
    .with_context(|| format!("calling {url} (model {})", target.model))?;
    Ok(resp.choices.into_iter().next().and_then(|c| c.message.content).unwrap_or_default())
}

/// A failure a retry won't fix: the key was rejected or the model doesn't exist.
#[derive(Debug)]
struct Permanent(String);

impl std::fmt::Display for Permanent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Permanent {}

/// One short rewrite: system + user message → text. Used by dictation's AI
/// clean-up. Asks for `{"text": …}` in JSON mode: small local reasoning models
/// (qwen3) otherwise narrate their thinking into the answer even with
/// reasoning off; JSON mode keeps them to the point (~1.5 s on an M3).
pub fn complete(target: &crate::config::LlmTarget, system: &str, user: &str, timeout_secs: u64) -> Result<String> {
    anyhow::ensure!(!target.endpoint.is_empty() && !target.model.is_empty(), "no AI model set up (Settings → Summaries)");
    let system = format!("{system}\n\nReply with exactly one JSON object with a single key, \"text\", whose value is your output: {{\"text\": \"...\"}}. No other keys.");
    let call = |json_mode: bool| -> Result<serde_json::Value, ureq::Error> {
        let mut req = ureq::post(&format!("{}/chat/completions", target.endpoint))
            .config()
            .timeout_global(Some(std::time::Duration::from_secs(timeout_secs)))
            .build();
        if let Some(key) = &target.api_key {
            req = req.header("Authorization", &format!("Bearer {key}"));
        }
        let mut body = serde_json::json!({
            "model": target.model,
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
            "temperature": 0.2,
            "max_tokens": 2000,
            "stream": false,
        });
        if json_mode {
            body["response_format"] = serde_json::json!({"type": "json_object"});
        }
        if !target.reasoning_effort.is_empty() {
            body["reasoning_effort"] = serde_json::json!(target.reasoning_effort);
        } else if target.local {
            body["reasoning_effort"] = serde_json::json!("none");
        }
        if target.local {
            // dictation is bursty: keep the model warm for a few minutes
            body["keep_alive"] = serde_json::json!("10m");
        }
        req.send_json(&body)?.body_mut().read_json()
    };
    let v = match call(true) {
        Err(ureq::Error::StatusCode(c)) if (400..=501).contains(&c) && c != 401 && c != 403 && c != 404 && c != 429 => call(false),
        r => r,
    }
    .map_err(|e| match e {
        ureq::Error::StatusCode(401 | 403) => anyhow::anyhow!("the API key was rejected"),
        ureq::Error::StatusCode(404) => anyhow::anyhow!("model “{}” not found", target.model),
        ureq::Error::StatusCode(429) => anyhow::anyhow!("rate limited or out of credits"),
        e => anyhow::Error::from(e),
    })?;
    let content = v["choices"][0]["message"]["content"].as_str().unwrap_or_default();
    let content = strip_think(content);
    // {"text": "..."}; tolerate code fences and plain text from APIs without JSON mode
    let s = content.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
    let text = match (s.find('{'), s.rfind('}')) {
        (Some(a), Some(b)) if a < b => serde_json::from_str::<serde_json::Value>(&s[a..=b]).ok().and_then(|v| {
            v["text"].as_str().map(str::to_string).or_else(|| {
                // a model that echoed the input structure: take the transcript-ish field
                ["TRANSCRIPT", "transcript", "output", "result", "cleaned"].iter().find_map(|k| v[*k].as_str().map(str::to_string))
            })
        }),
        _ => None,
    }
    .unwrap_or_else(|| s.to_string());
    Ok(text.trim().to_string())
}

/// One tiny request to check endpoint, model and key. Returns latency in ms.
pub fn ping(target: &crate::config::LlmTarget) -> Result<u128> {
    if target.endpoint.is_empty() {
        anyhow::bail!("enter the API's base URL");
    }
    if target.model.is_empty() {
        anyhow::bail!("enter a model name");
    }
    let t0 = std::time::Instant::now();
    let mut req = ureq::post(&format!("{}/chat/completions", target.endpoint))
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        .http_status_as_error(false)
        .build();
    if let Some(key) = &target.api_key {
        req = req.header("Authorization", &format!("Bearer {key}"));
    }
    let body = serde_json::json!({
        "model": target.model,
        "messages": [{"role": "user", "content": "Reply with OK."}],
        "max_tokens": 5,
        "stream": false,
    });
    let mut r = req.send_json(&body).map_err(|e| anyhow::Error::from(e).context("couldn't reach the API"))?;
    let code = r.status().as_u16();
    let text = r.body_mut().read_to_string().unwrap_or_default();
    if (200..300).contains(&code) {
        let v: serde_json::Value = serde_json::from_str(&text).context("the reply wasn't OpenAI-compatible JSON")?;
        anyhow::ensure!(v["choices"].is_array(), "the reply wasn't an OpenAI-compatible chat completion");
        return Ok(t0.elapsed().as_millis());
    }
    // Providers disagree on codes (Gemini says 400 for a bad key); read the message too.
    let msg = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| {
            let e = if v.is_array() { v[0]["error"].clone() } else { v["error"].clone() };
            e["message"].as_str().or(e.as_str()).map(str::to_string)
        })
        .unwrap_or_default();
    let low = msg.to_lowercase();
    let key_problem = matches!(code, 401 | 403) || low.contains("api key") || low.contains("api_key") || low.contains("authentication");
    if key_problem {
        anyhow::bail!(if target.api_key.is_some() { "the API key was rejected" } else { "this API needs a key" });
    }
    if code == 404 || low.contains("model") && (low.contains("not found") || low.contains("does not exist")) {
        anyhow::bail!("not found: check the base URL (usually ends in /v1) and the model name");
    }
    if code == 429 {
        anyhow::bail!("rate limited or out of credits");
    }
    let short: String = msg.chars().take(140).collect();
    anyhow::bail!(if short.is_empty() { format!("the API answered with HTTP {code}") } else { format!("HTTP {code}: {short}") })
}

/// "Meeting 15:20" and similar auto-generated names.
pub fn is_placeholder_title(t: &str) -> bool {
    let t = t.trim();
    t.is_empty() || (t.starts_with("Meeting ") && t[8..].chars().all(|c| c.is_ascii_digit() || c == ':'))
}

/// Cheap fallback title from the transcript when no summary model ran:
/// the first several meaningful words of the longest early turn.
pub fn fallback_title(transcript: &Transcript) -> Option<String> {
    // Too little speech to say what it was about: keep the time-based name.
    if transcript.word_count() < 40 {
        return None;
    }
    const FILLER: &[&str] = &["um", "uh", "like", "so", "okay", "ok", "yeah", "yes", "right", "hmm", "mm", "oh", "well"];
    let seg = transcript.segments.iter().take(8).max_by_key(|s| s.text.split_whitespace().count())?;
    let words: Vec<&str> = seg
        .text
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\''))
        .filter(|w| !w.is_empty() && !FILLER.contains(&w.to_lowercase().as_str()))
        .take(6)
        .collect();
    if words.len() < 3 {
        return None;
    }
    let mut t = words.join(" ");
    if let Some(f) = t.get(..1) {
        t = f.to_uppercase() + &t[1..];
    }
    Some(format!("{t}…"))
}

/// Accept bare JSON, fenced JSON, or JSON preceded by reasoning (`<think>…</think>`).
pub fn parse_summary(content: &str) -> Result<Summary> {
    let s = strip_think(content);
    let start = s.find('{').context("no JSON object")?;
    let end = s.rfind('}').context("no JSON object")?;
    let sm: Summary = serde_json::from_str(&s[start..=end])?;
    // Some models echo the schema instead of filling it in.
    let echoed = |t: &str| t.contains("<") && t.contains(">") || t.starts_with("3-6 word") || t == "1-2 sentence overview";
    if echoed(&sm.summary) || sm.title.as_deref().is_some_and(echoed) {
        anyhow::bail!("the model returned the template instead of a summary");
    }
    Ok(sm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_titles() {
        assert!(is_placeholder_title("Meeting 15:20"));
        assert!(!is_placeholder_title("Weekly sync"));
        assert!(!is_placeholder_title("Meeting with Alice"));
    }

    #[test]
    fn parses_sections() {
        let s = parse_summary(r#"{"sections":[{"heading":"Launch","bullets":["plain",{"text":"nested","sub":["a","b"]}]}]}"#).unwrap();
        assert_eq!(s.sections[0].bullets[0].text(), "plain");
        assert_eq!(s.sections[0].bullets[1].sub(), ["a", "b"]);
    }

    use crate::platform::Channel;
    use crate::transcript::Segment;

    /// A call alternating Me/Them every 30 s for `mins` minutes.
    fn call(mins: usize) -> Transcript {
        let segs = (0..mins * 2)
            .map(|i| {
                let ch = if i % 2 == 0 { Channel::Mic } else { Channel::System };
                Segment::new(ch, i as f64 * 30.0, i as f64 * 30.0 + 28.0, format!("point {i} about the plan and the numbers we discussed"))
            })
            .collect();
        Transcript { segments: segs, ..Default::default() }
    }

    #[test]
    fn short_calls_are_one_pass() {
        assert!(!needs_parts(&call(20)));
        let t = call(10);
        assert_eq!(split_parts(&t), vec![0..t.segments.len()]);
    }

    #[test]
    fn long_calls_split_on_turns_and_cover_everything() {
        let t = call(75);
        assert!(needs_parts(&t));
        let parts = split_parts(&t);
        // ~12 min each
        assert_eq!(parts.len(), 7, "{parts:?}");
        // contiguous, every segment exactly once
        assert_eq!(parts[0].start, 0);
        assert_eq!(parts.last().unwrap().end, t.segments.len());
        for w in parts.windows(2) {
            assert_eq!(w[0].end, w[1].start);
        }
        // evenly sized, and cut where the speaker changes
        for r in &parts {
            let secs = t.segments[r.end - 1].end - t.segments[r.start].start;
            assert!((8.0 * 60.0..=14.5 * 60.0).contains(&secs), "{r:?}: {secs}s");
            if r.end < t.segments.len() {
                assert_ne!(t.segments[r.end - 1].label(), t.segments[r.end].label());
            }
        }
    }

    #[test]
    fn wordy_calls_split_by_size_too() {
        // 20 minutes, but far more text than one part holds
        let mut t = call(20);
        for s in &mut t.segments {
            s.text = "word ".repeat(200);
        }
        assert!(needs_parts(&t));
        let parts = split_parts(&t);
        assert_eq!(parts.len(), 3);
        for r in parts {
            let chars: usize = t.segments[r].iter().map(|s| s.plain_line().len() + 1).sum();
            assert!(chars <= PART_MAX_CHARS * 13 / 10, "{chars}");
        }
    }

    #[test]
    fn one_giant_turn_is_one_part() {
        let t = Transcript { segments: vec![Segment::new(Channel::Mic, 0.0, 3600.0, "x ".repeat(50_000))], ..Default::default() };
        assert_eq!(split_parts(&t), vec![0..1]);
        assert!(split_parts(&Transcript::default()).is_empty());
    }

    #[test]
    fn detail_scales_with_length() {
        assert!(detail_hint(4.0).contains("short meeting"));
        let n = |h: String| h.split(" points").next().unwrap().rsplit(' ').next().unwrap().split('-').map(|x| x.parse::<u32>().unwrap()).collect::<Vec<_>>();
        let (short, long) = (n(detail_hint(20.0)), n(detail_hint(75.0)));
        assert!(long[0] > short[1], "{short:?} {long:?}");
        assert!(detail_hint(75.0).contains("up to 10 sections"));
    }

    fn hints(t: &Transcript, opts: &Options) -> Hints {
        Hints::new(opts, "Connect | Example Labs - Vuon", t)
    }

    #[test]
    fn prompt_has_faithfulness_detail_and_people() {
        let t = call(20);
        let people = vec!["Shelley Morgan".to_string(), "Sathiren Vale".to_string()];
        let opts = Options { me: Some("Alex Rowan"), people: &people, ..Default::default() };
        let p = fill_prompt(DEFAULT_PROMPT, &hints(&t, &opts), "an automatic transcript", &detail_hint(20.0), &t.to_plain());
        assert!(p.contains("Quote numbers exactly as spoken"));
        assert!(p.contains("is not a decision"));
        assert!(p.contains("about 20 min"));
        assert!(p.contains("Alex Rowan (who recorded it, \"Me\"), Shelley Morgan, Sathiren Vale"), "{p}");
        assert!(p.contains("Write \"Alex\" for the recorder"));
        assert!(p.contains("on a \"Me\" line is Alex's task"));
        assert!(p.contains("tell who is talking from context"));
        assert!(!p.contains("most bullets have no sub-bullets"));
        assert!(p.ends_with(&t.to_plain()));
        for ph in ["{source}", "{detail}", "{faithfulness}", "{speakers}", "{vocabulary}", "{template}", "{language}", "{title_hint}", "{context}"] {
            assert!(!p.contains(ph), "{ph} left in prompt");
        }
    }

    #[test]
    fn context_block_in_every_prompt() {
        let t = call(20);
        let ctx = "\nCONTEXT. Background ...\nPeople:\n- Sathiren Vale, Product Lead at Example Labs\n";
        let opts = Options { context: ctx, ..Default::default() };
        let h = hints(&t, &opts);
        let p = fill_prompt(DEFAULT_PROMPT, &h, "an automatic transcript", "", &t.to_plain());
        let (faith, c, out) = (p.find("FAITHFULNESS").unwrap(), p.find("CONTEXT.").unwrap(), p.find("Respond with ONLY").unwrap());
        assert!(faith < c && c < out, "context sits after the faithfulness rules");
        assert!(part_prompt(&h, 0, 2, &t.segments).contains("Product Lead at Example Labs"));
        // a custom prompt without {context} still gets it before the transcript
        let p = fill_prompt("Summarize.\nTranscript:\n{transcript}", &h, "x", "", "BODY");
        assert!(p.find("CONTEXT.").unwrap() < p.find("Transcript:").unwrap());
        // and no context means no block
        let p = fill_prompt(DEFAULT_PROMPT, &hints(&t, &Options::default()), "x", "", "BODY");
        assert!(!p.contains("CONTEXT."));
    }

    #[test]
    fn template_still_wins_and_custom_prompts_get_hints() {
        let t = call(20);
        let opts = Options { template: Some(("Sales call", "## Pricing\nPrices discussed.")), ..Default::default() };
        let h = hints(&t, &opts);
        let p = fill_prompt(DEFAULT_PROMPT, &h, "x", "", "T");
        let (tpl, faith) = (p.find("NOTE TEMPLATE \"Sales call\"").unwrap(), p.find("FAITHFULNESS").unwrap());
        assert!(faith < tpl, "template comes after the general guidance so it wins");
        assert!(p.contains("## Pricing"));
        // a custom prompt without placeholders still gets the template, before the transcript
        let p = fill_prompt("Summarize {title}.\nTranscript:\n{transcript}", &h, "x", "", "BODY");
        assert!(p.starts_with("Summarize Connect | Example Labs - Vuon."));
        assert!(p.find("NOTE TEMPLATE").unwrap() < p.find("Transcript:").unwrap());
        assert!(p.ends_with("BODY"));
    }

    #[test]
    fn people_are_described_by_side() {
        let mine = vec!["alex@example.com".to_string(), "alex@personal.example.org".to_string()];
        assert_eq!(describe_person("Shelley", Some("shelley@example.com"), &mine), "Shelley (colleague of the recorder)");
        assert_eq!(describe_person("Andy", Some("andy@partner.example.org"), &mine), "Andy (guest, partner.example.org)");
        assert_eq!(describe_person("Bob", Some("bob@gmail.com"), &mine), "Bob");
        assert_eq!(describe_person("Ann", None, &mine), "Ann");
        // same company on a subdomain; more webmail; bracketed addresses
        assert_eq!(describe_person("Ana", Some("ana@eu.example.com"), &mine), "Ana (colleague of the recorder)");
        assert_eq!(describe_person("Kim", Some("kim@fastmail.com"), &mine), "Kim");
        assert_eq!(describe_person("Lee", Some("<lee@acme.com>"), &mine), "Lee (guest, acme.com)");
    }

    #[test]
    fn every_part_failing_reports_the_cause() {
        let srv = crate::destinations::testutil::MockServer::start(|_| (401, r#"{"error":{"message":"nope"}}"#.into()));
        let target = crate::config::LlmTarget { endpoint: format!("{}/v1", srv.url), model: "m".into(), api_key: Some("bad".into()), reasoning_effort: String::new(), keep_alive: None, local: false };
        let t = call(75);
        let parts = split_parts(&t).len();
        let e = summarize_opts(&LlmConfig::default(), &target, &Options::default(), "Sync", &t).unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("failed on every part"), "{msg}");
        assert!(msg.contains("the API key was rejected"), "the real cause reaches the user: {msg}");
        // a rejected key isn't retried: one call per part, and no merge call
        assert_eq!(srv.requests().len(), parts);
    }

    #[test]
    fn permanent_errors_skip_the_retry() {
        let key = anyhow::Error::new(Permanent("the API key was rejected".into())).context("calling h (model m)");
        assert!(is_permanent(&key));
        assert_eq!(format!("{key:#}"), "calling h (model m): the API key was rejected");
        // the same words without the type are not permanent: the check doesn't read text
        assert!(!is_permanent(&anyhow::anyhow!("the API key was rejected")));
        assert!(!is_permanent(&anyhow::anyhow!("rate limited or out of credits")));
    }

    #[test]
    fn named_transcripts_use_names() {
        let mut t = call(20);
        crate::speakers::rename(&mut t, Channel::System, None, "Shelley");
        assert!(t.has_names());
        let opts = Options { me: Some("Alex Rowan"), ..Default::default() };
        let h = hints(&t, &opts);
        assert!(h.speakers.contains("Other speakers: Shelley"), "{}", h.speakers);
        assert!(!h.speakers.contains("tell who is talking from context"));
        assert!(!h.speakers.contains("someone the speakers talk about"), "names known: no guessing rule for Them");
        // no names known at all: still says who owns action items
        let h = hints(&call(20), &Options::default());
        assert!(h.speakers.contains("owner is the person who will do it"));
        assert!(h.speakers.contains("the recorder's (\"Me\") task"));
        assert!(h.speakers.contains("When unsure, keep \"Them\""));
    }

    #[test]
    fn recorder_label_used_for_their_lines() {
        // speaker naming labels the recorder's own lines "Alex" instead of "Me"
        let mut t = call(20);
        crate::speakers::sync_infos(&mut t);
        let me = crate::identity::Me::from_config(&crate::config::MeConfig { name: "Alex Rowan".into(), ..Default::default() });
        assert!(crate::speakers::name_recorder(&mut t, &me));
        assert_eq!(t.recorder_label(), "Alex");
        let h = attribution_hint(&t, Some("Alex Rowan"), &[]);
        assert!(h.contains("on a \"Alex\" line is Alex's task"), "{h}");
        assert!(!h.contains("\"Me\" line"), "{h}");
    }

    #[test]
    fn part_prompts_cover_their_slice() {
        let t = call(75);
        let parts = split_parts(&t);
        let h = hints(&t, &Options::default());
        let r = parts[2].clone();
        let p = part_prompt(&h, 2, parts.len(), &t.segments[r.clone()]);
        assert!(p.contains(&format!("part 3 of {}", parts.len())));
        assert!(p.contains(&t.segments[r.start].plain_line()));
        assert!(p.contains(&t.segments[r.end - 1].plain_line()));
        assert!(!p.contains(&t.segments[r.end].plain_line()));
        assert!(p.contains("Quote numbers exactly as spoken"));
        assert!(p.contains(&crate::util::fmt_timestamp(t.segments[r.start].start)));
    }

    #[test]
    fn cap_keeps_short_text_and_marks_cuts() {
        assert_eq!(cap("abc".into(), 10, "t"), "abc");
        let c = cap("a".repeat(100), 30, "t");
        assert!(c.contains("middle omitted"));
        assert!(c.len() < 80);
    }

    #[test]
    fn num_ctx_leaves_room_for_the_answer() {
        assert_eq!(num_ctx_for(""), 8192);
        let p = "x".repeat(60_000);
        assert!(num_ctx_for(&p) as usize >= 60_000 / 3 + 8192);
    }

    #[test]
    fn parses_variants() {
        let s = parse_summary("<think>hmm {not json}</think>\n```json\n{\"summary\":\"x\",\"action_items\":[{\"task\":\"t\"}]}\n```").unwrap();
        assert_eq!(s.summary, "x");
        assert_eq!(s.action_items[0].task, "t");
        assert!(s.decisions.is_empty());
    }

    #[test]
    fn recorder_owns_me_action_items() {
        let mut s = Summary {
            action_items: vec![
                ActionItem { owner: Some("Me".into()), task: "a".into() },
                ActionItem { owner: Some("You".into()), task: "b".into() },
                ActionItem { owner: Some("Shelley".into()), task: "c".into() },
                ActionItem { owner: None, task: "d".into() },
            ],
            ..Default::default()
        };
        own_action_items(&mut s, "Me");
        assert_eq!(s.action_items[0].owner.as_deref(), Some("Me"), "label kept as Me: unchanged");
        own_action_items(&mut s, "Alex");
        let owners: Vec<Option<&str>> = s.action_items.iter().map(|a| a.owner.as_deref()).collect();
        assert_eq!(owners, vec![Some("Alex"), Some("Alex"), Some("Shelley"), None]);
    }
}

#[cfg(test)]
mod ping_tests {
    use crate::config::LlmTarget;

    fn t(endpoint: &str, key: Option<&str>) -> LlmTarget {
        LlmTarget { endpoint: endpoint.into(), model: "x".into(), api_key: key.map(Into::into), reasoning_effort: String::new(), keep_alive: None, local: false }
    }

    /// Hits real public endpoints with a bogus key; run with `--ignored` when online.
    #[test]
    #[ignore]
    fn bad_keys_read_as_bad_keys() {
        for ep in ["https://api.anthropic.com/v1", "https://api.openai.com/v1", "https://openrouter.ai/api/v1", "https://generativelanguage.googleapis.com/v1beta/openai"] {
            let e = super::ping(&t(ep, Some("invalid"))).unwrap_err().to_string();
            assert_eq!(e, "the API key was rejected", "{ep}");
        }
    }
}
