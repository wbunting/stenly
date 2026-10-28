//! Speaker-name suggestions from the summary model, for voices nothing else
//! could name: it reads the invite list and the transcript ("thanks, Sarah",
//! "Alex, what do you think?") and proposes a name per label with a
//! confidence. Only confident, consistent suggestions are applied.

use super::participants::Participants;
use super::{LLM_MIN_CONFIDENCE, propose};
use crate::identity::Me;
#[allow(unused_imports)]
use crate::transcript::SpeakerInfo;
use crate::calendar::CalendarEvent;
use crate::config::LlmTarget;
use crate::transcript::{NameSource, Transcript, unnamed_label};
use anyhow::Result;
use serde::Deserialize;

const SYSTEM: &str = "You identify who is speaking in a meeting transcript. Speakers are labelled by voice \
(e.g. \"Speaker 2\", \"Them\"). Work out each unnamed label's real first name or full name from evidence in the \
transcript: people addressing each other by name, introductions, sign-offs, someone saying their own name, and the \
list of invited people. Rules: only use a name with clear evidence; a person addressed by name is usually NOT the \
one speaking that line but the one who answers next; never give two labels the same name; never name a label after \
the person who recorded (\"Me\"/\"You\"). If unsure, leave it out or use a low confidence.";

/// Transcript lines sent to the model (start, which is where people are introduced, plus the end).
const MAX_CHARS: usize = 24_000;

#[derive(Debug, Deserialize, PartialEq)]
pub struct Suggestion {
    pub label: String,
    pub name: String,
    #[serde(default)]
    pub confidence: f32,
    #[serde(default)]
    pub evidence: String,
}

pub fn build_prompt(t: &Transcript, ev: Option<&CalendarEvent>, people: &Participants, me: &Me) -> Option<String> {
    // On calls the mic is the recorder, not someone to identify.
    let unnamed: Vec<String> = t
        .speakers
        .iter()
        .filter(|i| i.name.is_none() && i.talk_secs >= 5.0 && (t.in_person || i.channel != crate::platform::Channel::Mic))
        .map(|i| unnamed_label(i.channel, i.speaker))
        .collect();
    if unnamed.is_empty() {
        return None;
    }
    let mut plain = t.to_plain();
    if plain.len() > MAX_CHARS {
        let head: String = plain.chars().take(MAX_CHARS * 2 / 3).collect();
        let tail: String = plain.chars().rev().take(MAX_CHARS / 3).collect::<Vec<_>>().into_iter().rev().collect();
        plain = format!("{head}\n[…]\n{tail}");
    }
    let people = people.names();
    let known: Vec<String> = t.speakers.iter().filter_map(|i| i.name.clone()).collect();
    let mut p = String::new();
    if let Some(e) = ev {
        p.push_str(&format!("Meeting: {}\n", e.title));
    }
    if !people.is_empty() {
        p.push_str(&format!("In the meeting (besides the recorder): {}\n", people.join(", ")));
    }
    if let Some(name) = me.display() {
        p.push_str(&format!("The recorder (\"{}\") is {name}.\n", t.recorder_label()));
    }
    if !known.is_empty() {
        p.push_str(&format!("Already identified: {}\n", known.join(", ")));
    }
    let example = unnamed.first().cloned().unwrap_or_default();
    p.push_str(&format!(
        "Transcript:\n{plain}\n\n---\nWho is each of these labels: {}?\nAnswer with ONLY this JSON (one entry per label you can identify; leave out labels you can't):\n{{\"speakers\": [{{\"label\": \"{example}\", \"name\": \"<their name>\", \"confidence\": <0.0-1.0>, \"evidence\": \"<short quote showing it>\"}}]}}",
        unnamed.join(", ")
    ));
    Some(p)
}

pub fn parse(text: &str) -> Vec<Suggestion> {
    #[derive(Deserialize)]
    struct Out {
        #[serde(default)]
        speakers: Vec<Suggestion>,
    }
    let s = text.trim();
    let s = crate::summary::strip_think(s);
    let (Some(a), Some(b)) = (s.find('{'), s.rfind('}')) else { return Vec::new() };
    serde_json::from_str::<Out>(&s[a..=b]).map(|o| o.speakers).unwrap_or_default()
}

/// Apply confident suggestions to unnamed voices. Returns how many were named.
pub fn apply(t: &mut Transcript, suggestions: &[Suggestion], me: &Me) -> usize {
    let mut named = 0;
    let taken = |t: &Transcript, n: &str| t.speakers.iter().any(|i| i.name.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(n)));
    // a name proposed for two labels is not trustworthy for either
    let dup = |n: &str| suggestions.iter().filter(|s| s.name.trim().eq_ignore_ascii_case(n.trim())).count() > 1;
    for s in suggestions {
        let name = s.name.trim();
        let lower = name.to_lowercase();
        if s.confidence < LLM_MIN_CONFIDENCE
            || name.is_empty()
            || ["unknown", "me", "you", "them", "n/a", "none"].contains(&lower.as_str())
            || lower.starts_with("speaker")
            || me.is(name)
            || dup(name)
            || taken(t, name)
        {
            continue;
        }
        let Some(i) = t.speakers.iter().position(|i| i.name.is_none() && unnamed_label(i.channel, i.speaker).eq_ignore_ascii_case(s.label.trim())) else {
            continue;
        };
        let (ch, sp) = (t.speakers[i].channel, t.speakers[i].speaker);
        if !supported(t, ch, sp, name) {
            tracing::debug!(label = %s.label, name, "suggestion has no direct evidence in the transcript; ignored");
            continue;
        }
        if propose(&mut t.speakers[i], name, NameSource::Llm, s.confidence) {
            named += 1;
        }
    }
    named
}

fn intro_re(first: &str) -> regex::Regex {
    let n = regex::escape(first);
    regex::Regex::new(&format!(r"(?i)\b(i'm|i am|this is|my name is|my name's)\s+{n}\b|\b{n}\s+here\b")).expect("regex")
}

fn vocative_re(first: &str) -> regex::Regex {
    let n = regex::escape(first);
    // "Andy, have people…", "thanks Andy", "…, Andy?", "are you Andy going to…", "Okay Shelley, do you…"
    regex::Regex::new(&format!(
        r"(?i)(^|[.?!]\s+){n}\s*[,?]|\b(thanks|thank you|hi|hey|hello|over to you|go ahead|morning|cheers),?\s+{n}\b|,\s*{n}\s*[?.!]?\s*$|,\s*{n}[?.!,]|\b(are|did|do|have|can|could|would|will|were|should)\s+you,?\s+{n}\b|\b(okay|ok|so|well|and|but|yeah|alright|right|now|sorry)\s*,?\s+{n}\s*,"
    ))
    .expect("regex")
}

/// Evidence points that voice (ch, sp) is `name`: 2 per self-introduction,
/// 1 per time someone else addresses `name` and this voice answers next.
pub fn evidence(t: &Transcript, ch: crate::platform::Channel, sp: Option<u32>, name: &str) -> u32 {
    let first = name.split_whitespace().next().unwrap_or(name);
    if first.chars().count() < 2 {
        return 0;
    }
    let (intro, vocative) = (intro_re(first), vocative_re(first));
    let segs = &t.segments;
    let is_voice = |k: usize| segs[k].channel == ch && segs[k].speaker == sp;
    let mut score = 0;
    for (k, s) in segs.iter().enumerate() {
        if is_voice(k) && intro.is_match(&s.text) {
            score += 2;
        }
        if !is_voice(k) && vocative.is_match(&s.text) {
            // the next turn by someone other than the speaker answers. On a
            // call the recorder is never the one being found, so a quick
            // "Hey." from them before the answer is skipped.
            let quick_me = |j: usize| !t.in_person && segs[j].channel == crate::platform::Channel::Mic && segs[j].end - segs[j].start < 2.5 && segs[j].text.split_whitespace().count() <= 6;
            if let Some(next) = (k + 1..segs.len().min(k + 5)).find(|&j| !(segs[j].channel == s.channel && segs[j].speaker == s.speaker) && !quick_me(j)) {
                if is_voice(next) {
                    score += 1;
                }
            }
        }
    }
    score
}

/// Whether the transcript itself backs "this voice is `name`". Small models
/// happily name a speaker after someone they only talk *about* ("no, it was
/// Alex"), so suggestions without evidence are dropped.
pub fn supported(t: &Transcript, ch: crate::platform::Channel, sp: Option<u32>, name: &str) -> bool {
    evidence(t, ch, sp, name) > 0
}

/// Names from the transcript alone (no model): for each candidate name, the
/// voice with clearly the most evidence takes it. Candidates are invitees plus
/// names people introduce themselves with.
pub fn name_from_cues(t: &mut Transcript, candidates: &[String], me: &Me) -> usize {
    let intro_any = regex::Regex::new(r"\b(?:I'm|I am|this is|my name is|my name's)\s+([A-Z][a-z]{2,})\b|\b([A-Z][a-z]{2,}) here\b").expect("regex");
    // "Karen, is …" / "thanks Karen": capitalised words used to address someone
    let addressed = regex::Regex::new(r"(?:^|[.?!]\s+)([A-Z][a-z]{2,}),\s|\b(?:[Tt]hanks|[Tt]hank you|[Hh]i|[Hh]ey|[Hh]ello),?\s+([A-Z][a-z]{2,})\b").expect("regex");
    let all_text: String = t.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ");
    // a real name is never written in lower case ("great", "yeah" are)
    let proper = |n: &str| !regex::Regex::new(&format!(r"\b{}\b", regex::escape(&n.to_lowercase()))).is_ok_and(|r| r.is_match(&all_text));
    const NOT_NAMES: &[&str] = &["Sure", "Just", "Not", "Going", "Here", "Really", "Glad", "Happy", "Sorry", "Good", "Fine", "Still", "Actually", "Also", "Trying", "Looking", "Working", "Back", "Done", "Okay", "Yeah", "Yes", "Great", "Right", "Well", "Perfect", "Cool", "Thanks", "Everyone", "Guys", "Folks", "Team", "All", "Awesome", "Nice", "Sounds", "Exactly", "Totally", "Interesting", "Absolutely", "Definitely", "Amazing", "Wow", "Hmm", "Alright", "Anyway", "Honestly", "Basically", "Correct", "Got", "Thank"];
    let mut names: Vec<String> = candidates.to_vec();
    for s in &t.segments {
        for c in intro_any.captures_iter(&s.text).chain(addressed.captures_iter(&s.text)) {
            let n = c.get(1).or(c.get(2)).map(|m| m.as_str().to_string()).unwrap_or_default();
            if !n.is_empty() && !NOT_NAMES.contains(&n.as_str()) && proper(&n) && !names.iter().any(|x| x.split_whitespace().next() == Some(n.as_str())) {
                names.push(n);
            }
        }
    }
    let mut named = 0;
    for name in names {
        let first = name.split_whitespace().next().unwrap_or(&name).to_string();
        // the recorder's own name ("thanks, Alex") points at the mic, which is never named here
        if me.is(&name) || me.names.iter().any(|m| m.split_whitespace().next().is_some_and(|f| f.eq_ignore_ascii_case(&first))) {
            continue;
        }
        if t.speakers.iter().any(|i| i.name.as_deref().is_some_and(|n| n.split_whitespace().next().is_some_and(|f| f.eq_ignore_ascii_case(&first)))) {
            continue;
        }
        let mut scored: Vec<(usize, u32)> = (0..t.speakers.len())
            .filter(|&i| t.in_person || t.speakers[i].channel != crate::platform::Channel::Mic)
            .map(|i| (i, evidence(t, t.speakers[i].channel, t.speakers[i].speaker, &name)))
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1));
        let Some(&(best, score)) = scored.first() else { continue };
        let runner = scored.get(1).map(|x| x.1).unwrap_or(0);
        tracing::debug!(name, score, runner, voice = %unnamed_label(t.speakers[best].channel, t.speakers[best].speaker), "name cue evidence");
        if score >= 2 && score >= runner * 3 && t.speakers[best].name.is_none() {
            let conf = (0.7 + 0.05 * score as f32).min(0.95);
            if propose(&mut t.speakers[best], &name, NameSource::Llm, conf) {
                named += 1;
            }
        }
    }
    named
}

/// Ask the model. Errors are for logging; naming just doesn't happen.
pub fn suggest(target: &LlmTarget, t: &Transcript, ev: Option<&CalendarEvent>, people: &Participants, me: &Me, timeout_secs: u64) -> Result<Vec<Suggestion>> {
    let Some(user) = build_prompt(t, ev, people, me) else { return Ok(Vec::new()) };
    let text = chat_json(target, SYSTEM, &user, timeout_secs)?;
    tracing::debug!(reply = %text.chars().take(2000).collect::<String>(), "speaker name reply");
    Ok(parse(&text))
}

/// One OpenAI-compatible chat call in JSON mode (falls back to plain prompting
/// for backends that reject `response_format`). Returns the message text.
fn chat_json(target: &LlmTarget, system: &str, user: &str, timeout_secs: u64) -> Result<String> {
    if let Some(spec) = &target.cli {
        return Ok(crate::cli_llm::run(spec, Some(system), user, timeout_secs.max(30))?);
    }
    anyhow::ensure!(!target.endpoint.is_empty() && !target.model.is_empty(), "no AI model set up");
    let call = |json_mode: bool| -> Result<serde_json::Value, ureq::Error> {
        let mut req = ureq::post(&format!("{}/chat/completions", target.endpoint)).config().timeout_global(Some(std::time::Duration::from_secs(timeout_secs.max(30)))).build();
        if let Some(key) = &target.api_key {
            req = req.header("Authorization", &format!("Bearer {key}"));
        }
        let mut body = serde_json::json!({
            "model": target.model,
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
            "temperature": 0.1,
            "stream": false,
        });
        if json_mode {
            body["response_format"] = serde_json::json!({"type": "json_object"});
        }
        if !target.reasoning_effort.is_empty() {
            body["reasoning_effort"] = serde_json::json!(target.reasoning_effort);
        }
        if target.local {
            if let Some(k) = &target.keep_alive {
                body["keep_alive"] = serde_json::json!(k);
            }
            let need = ((system.len() + user.len()) as f64 / 3.2) as u32 + 2048;
            body["options"] = serde_json::json!({ "num_ctx": need.next_power_of_two().clamp(8192, 65_536) });
        }
        req.send_json(&body)?.body_mut().read_json()
    };
    let v = match call(true) {
        Err(ureq::Error::StatusCode(c)) if (400..=501).contains(&c) && ![401, 403, 404, 429].contains(&c) => call(false),
        r => r,
    }?;
    Ok(v["choices"][0]["message"]["content"].as_str().unwrap_or_default().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Channel;
    use crate::transcript::Segment;

    fn t() -> Transcript {
        let seg = |ch, s: f64, sp: Option<u32>, text: &str| {
            let mut x = Segment::new(ch, s, s + 6.0, text);
            x.speaker = sp;
            x
        };
        let mut t = Transcript::from_channels(vec![
            seg(Channel::System, 0.0, Some(0), "Hi all, Dana here from finance."),
            seg(Channel::Mic, 7.0, None, "Thanks Dana. Lee, can you go next?"),
            seg(Channel::System, 14.0, Some(1), "Sure, so on the platform side…"),
        ]);
        super::super::sync_infos(&mut t);
        t
    }

    #[test]
    fn prompt_lists_unnamed_labels() {
        let p = build_prompt(&t(), None, &Participants::default(), &Me::named("Alex")).unwrap();
        assert!(p.contains("Who is each of these labels: Speaker 1, Speaker 2?"), "{p}");
        assert!(p.contains("The recorder (\"Me\") is Alex."));
        assert!(p.contains("Dana here"));
    }

    #[test]
    fn parses_fenced_and_thinking_output() {
        let s = parse("<think>hmm</think>```json\n{\"speakers\":[{\"label\":\"Speaker 1\",\"name\":\"Dana\",\"confidence\":0.9,\"evidence\":\"Dana here\"}]}\n```");
        assert_eq!(s, vec![Suggestion { label: "Speaker 1".into(), name: "Dana".into(), confidence: 0.9, evidence: "Dana here".into() }]);
        assert!(parse("no json").is_empty());
    }

    #[test]
    fn evidence_must_be_in_the_transcript() {
        let tr = t();
        // "Dana here" from Speaker 1 itself
        assert!(supported(&tr, Channel::System, Some(0), "Dana"));
        // "Lee, can you go next?" from Me, and Speaker 2 answers
        assert!(supported(&tr, Channel::System, Some(1), "Lee Park"));
        // "Thanks Dana." is answered by Speaker 2, so only the introduction counts
        assert_eq!(evidence(&tr, Channel::System, Some(0), "Dana"), 2);
        // talked about, never addressed: not evidence
        let seg = |ch, s: f64, sp: Option<u32>, text: &str| {
            let mut x = Segment::new(ch, s, s + 6.0, text);
            x.speaker = sp;
            x
        };
        let tr = Transcript::from_channels(vec![
            seg(Channel::Mic, 0.0, Some(0), "Was it Glen who sent it? No, no, it was Alex."),
            seg(Channel::Mic, 10.0, Some(1), "Right, Alex sent the deck."),
        ]);
        assert!(!supported(&tr, Channel::Mic, Some(0), "Alex"));
        assert!(!supported(&tr, Channel::Mic, Some(1), "Alex"));
    }

    #[test]
    fn cues_alone_name_voices() {
        let seg = |ch, s: f64, sp: Option<u32>, text: &str| {
            let mut x = Segment::new(ch, s, s + 6.0, text);
            x.speaker = sp;
            x
        };
        let mut tr = Transcript::from_channels(vec![
            seg(Channel::Mic, 0.0, None, "Hi both, shall we start?"),
            seg(Channel::System, 7.0, Some(0), "Sure. Hi, I'm Daniel from the platform team."),
            seg(Channel::System, 14.0, Some(1), "Thanks Daniel. We need two more weeks."),
            seg(Channel::Mic, 21.0, None, "Karen, is two weeks firm?"),
            seg(Channel::System, 28.0, Some(1), "We could ship the first screen on time."),
            seg(Channel::Mic, 35.0, None, "Great. Karen, can you send the designs?"),
            seg(Channel::System, 42.0, Some(1), "Yes, by Wednesday."),
        ]);
        super::super::sync_infos(&mut tr);
        assert_eq!(name_from_cues(&mut tr, &[], &Me::named("Samantha")), 2);
        assert_eq!(tr.speaker_info(Channel::System, Some(0)).unwrap().name.as_deref(), Some("Daniel"));
        assert_eq!(tr.speaker_info(Channel::System, Some(1)).unwrap().name.as_deref(), Some("Karen"));
    }

    fn seg(ch: Channel, s: f64, sp: Option<u32>, text: &str) -> Segment {
        let mut x = Segment::new(ch, s, s + 6.0, text);
        x.speaker = sp;
        x
    }

    /// Shape of a typical Zoom call (invite: the user, "shelley@…", "andy@…"):
    /// Shelley asks Andy questions mid-sentence ("are you Andy going to…")
    /// and by name at the start of a turn; Andy answers each time.
    #[test]
    fn person_addressed_by_name_who_answers_is_named() {
        let mut tr = Transcript::from_channels(vec![
            seg(Channel::System, 0.0, Some(0), "We host all of it in one place, nothing separate."),
            seg(Channel::Mic, 7.0, None, "Okay, makes sense."),
            seg(Channel::System, 14.0, Some(1), "And then are you Andy like going to pull in documents too?"),
            seg(Channel::System, 21.0, Some(0), "Interesting question. Right now only photos."),
            seg(Channel::System, 28.0, Some(0), "It's tough, Shelley, because it's all about trust."),
            seg(Channel::System, 35.0, Some(1), "Yeah, that's the hard part."),
            seg(Channel::System, 42.0, Some(1), "Andy, have people started using it yet?"),
            seg(Channel::System, 49.0, Some(0), "About a hundred last month."),
            seg(Channel::System, 56.0, Some(0), "Yes, Shelley. Great point."),
            seg(Channel::System, 63.0, Some(1), "Thanks."),
        ]);
        super::super::sync_infos(&mut tr);
        let named = name_from_cues(&mut tr, &["Shelley".into(), "Andy".into()], &Me::named("Alex Rowan"));
        assert_eq!(named, 2);
        assert_eq!(tr.speaker_info(Channel::System, Some(0)).unwrap().name.as_deref(), Some("Andy"));
        assert_eq!(tr.speaker_info(Channel::System, Some(1)).unwrap().name.as_deref(), Some("Shelley"));
    }

    /// A late joiner greets everyone; the recorder's quick "Hey." comes
    /// before the greeted person answers.
    #[test]
    fn quick_reply_from_the_recorder_is_skipped() {
        let mut tr = Transcript::from_channels(vec![
            seg(Channel::System, 0.0, Some(0), "Hi Seth. How are you?"),
            Segment::new(Channel::Mic, 6.5, 7.5, "Hey."),
            seg(Channel::System, 8.0, Some(1), "Good, sorry I'm late, a client call ran over."),
            seg(Channel::System, 15.0, Some(0), "So in your opinion, Seth, what would make this useful?"),
            seg(Channel::System, 22.0, Some(1), "Architecture, mostly."),
        ]);
        super::super::sync_infos(&mut tr);
        assert_eq!(evidence(&tr, Channel::System, Some(1), "Seth"), 2);
        assert_eq!(name_from_cues(&mut tr, &["Sathiren Vale".into()], &Me::named("Alex Rowan")), 1);
        assert_eq!(tr.speaker_info(Channel::System, Some(1)).unwrap().name.as_deref(), Some("Seth"));
    }

    #[test]
    fn applies_only_confident_unique_names() {
        let mut tr = t();
        let sug = vec![
            Suggestion { label: "Speaker 1".into(), name: "Dana".into(), confidence: 0.92, evidence: String::new() },
            Suggestion { label: "Speaker 2".into(), name: "Lee".into(), confidence: 0.6, evidence: String::new() },
            Suggestion { label: "Me".into(), name: "Alex".into(), confidence: 0.99, evidence: String::new() },
        ];
        assert_eq!(apply(&mut tr, &sug, &Me::named("Alex")), 1);
        assert_eq!(tr.speaker_info(Channel::System, Some(0)).unwrap().name.as_deref(), Some("Dana"));
        assert!(tr.speaker_info(Channel::System, Some(1)).unwrap().name.is_none());
        // the same name for two labels: neither
        let mut tr = t();
        let sug = vec![
            Suggestion { label: "Speaker 1".into(), name: "Dana".into(), confidence: 0.9, evidence: String::new() },
            Suggestion { label: "Speaker 2".into(), name: "dana".into(), confidence: 0.9, evidence: String::new() },
        ];
        assert_eq!(apply(&mut tr, &sug, &Me::default()), 0);
    }
}
