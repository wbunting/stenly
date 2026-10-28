//! Post-recording pipeline: WAVs → transcript → summary → note. Pure logic;
//! the caller decides which thread to run it on.

use crate::audio::read_wav_16k_mono;
use crate::config::Config;
use crate::note;
use crate::platform::Channel;
use crate::session::{SessionMeta, SessionState, wav_path};
use crate::summary;
use crate::transcript::{Segment, Transcriber, Transcript};
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Unmerged ASR segments (both channels), kept for speaker separation.
pub const RAW_FILE: &str = "segments.raw.json";

#[derive(Debug, Clone)]
pub enum Progress {
    Transcribing { channel: Channel, fraction: f32 },
    SeparatingSpeakers,
    IdentifyingSpeakers,
    Summarizing,
    Writing,
}

pub struct Processed {
    pub note: PathBuf,
    pub transcript: Transcript,
    pub summary: Option<summary::Summary>,
    /// Automatic sends (destination name, error if it failed).
    pub sent: Vec<(String, Option<String>)>,
}

/// Process one session directory end-to-end. Idempotent: re-running a `Done`
/// session rewrites its note (useful after changing models / prompts).
pub fn process_session(
    cfg: &Config,
    dir: &Path,
    transcriber: &mut dyn Transcriber,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<Processed> {
    let mut meta = SessionMeta::load(dir)?;
    if meta.state == SessionState::Recording {
        // crashed mid-recording: whatever reached disk is what we have
        meta.ended = meta.ended.or_else(|| std::fs::metadata(wav_path(dir, Channel::Mic)).ok().and_then(|m| m.modified().ok()).map(Into::into));
        meta.state = SessionState::Recorded;
        meta.save(dir)?;
    }
    // the meeting app as its windows showed it (beats the guess from who held the mic)
    if let Some(app) = crate::speakers::timeline::summarize(&crate::speakers::timeline::load(dir)).app {
        if meta.meeting_app.as_deref() != Some(app.as_str()) {
            meta.meeting_app = Some(app);
            meta.save(dir)?;
        }
    }

    let transcript_file = dir.join("transcript.json");
    // background on the meeting: the gathered context.json, or the invite's agenda
    let ctx = crate::context::for_session(dir, &meta);
    // the user's terms plus the invitees' names, names the context found and built-in AI/tech names
    let vocab = crate::vocabulary::Vocabulary::for_session_with(cfg, &meta, Some(&ctx));
    let fix = |t: &mut Transcript| {
        let changes = vocab.apply(t);
        if !changes.is_empty() {
            tracing::info!(changes = changes.len(), "vocabulary corrections");
            for c in &changes {
                tracing::debug!(rule = c.rule, from = %c.from, to = %c.to, "vocabulary");
            }
        }
    };
    let transcript = match std::fs::read(&transcript_file).ok().and_then(|b| serde_json::from_slice::<Transcript>(&b).ok()) {
        Some(mut t) if meta.state != SessionState::Recorded => {
            // terms added since the transcript was made apply on re-summarize too
            fix(&mut t);
            t
        }
        _ if meta.state == SessionState::Recorded && dir.join("live.complete.json").exists() => {
            // Transcribed live during the meeting: nothing left to decode.
            let segs: Vec<Segment> = serde_json::from_slice(&std::fs::read(dir.join("live.complete.json"))?)?;
            let _ = std::fs::write(dir.join(RAW_FILE), serde_json::to_vec(&segs)?);
            let mut t = Transcript::from_channels(segs);
            fix(&mut t);
            std::fs::write(&transcript_file, serde_json::to_vec_pretty(&t)?)?;
            meta.state = SessionState::Transcribed;
            meta.save(dir)?;
            tracing::info!("using the live transcript");
            t
        }
        _ => {
            let mut segs: Vec<Segment> = Vec::new();
            for ch in [Channel::Mic, Channel::System] {
                let path = wav_path(dir, ch);
                if !path.exists() {
                    continue;
                }
                let samples = match read_wav_16k_mono(&path) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(error = %e, path = %path.display(), "unreadable channel, skipping");
                        continue;
                    }
                };
                let mut cb = |f: f32| on_progress(Progress::Transcribing { channel: ch, fraction: f });
                segs.extend(transcriber.transcribe(&samples, ch, &mut cb)?);
            }
            let _ = std::fs::write(dir.join(RAW_FILE), serde_json::to_vec(&segs)?);
            let mut t = Transcript::from_channels(segs);
            fix(&mut t);
            std::fs::write(&transcript_file, serde_json::to_vec_pretty(&t)?)?;
            meta.state = SessionState::Transcribed;
            meta.save(dir)?;
            t
        }
    };

    // In-person meeting: everything is on the mic and the call side is silent.
    // Tell voices apart so notes aren't "Me said everything".
    let transcript = {
        let mut t = transcript;
        let mic_words: usize = t.segments.iter().filter(|s| s.channel == Channel::Mic).map(|s| s.text.split_whitespace().count()).sum();
        let sys_words: usize = t.segments.iter().filter(|s| s.channel == Channel::System).map(|s| s.text.split_whitespace().count()).sum();
        let in_person = mic_words >= 200 && sys_words * 20 < mic_words;
        let already = t.segments.iter().any(|s| s.speaker.is_some());
        if in_person && !t.in_person {
            t.in_person = true;
        }
        if in_person && !already && cfg.asr.separate_speakers {
            on_progress(Progress::SeparatingSpeakers);
            match read_wav_16k_mono(&wav_path(dir, Channel::Mic)) {
                Ok(audio) => {
                    // Separate on the raw speech segments (≤25 s, cut at pauses) —
                    // merged turns run up to a minute and mix several voices.
                    let raw: Option<Vec<Segment>> = std::fs::read(dir.join(RAW_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok());
                    let mut mic: Vec<Segment> = match raw {
                        Some(r) => {
                            let mut r: Vec<Segment> = r.into_iter().filter(|s| s.channel == Channel::Mic && !s.text.trim().is_empty()).collect();
                            r.sort_by(|a, b| a.start.total_cmp(&b.start));
                            let mut tmp = Transcript { segments: r, in_person: true, ..Default::default() };
                            vocab.apply(&mut tmp);
                            tmp.segments
                        }
                        None => t.segments.iter().filter(|s| s.channel == Channel::Mic).cloned().collect(),
                    };
                    match transcriber.separate_speakers(&mut mic, &audio) {
                        Ok(n) if n >= 2 => {
                            tracing::info!(speakers = n, "separated speakers");
                            let mut rest: Vec<Segment> = t.segments.into_iter().filter(|s| s.channel != Channel::Mic).collect();
                            rest.extend(mic);
                            t = Transcript::from_channels(rest);
                            t.in_person = true;
                        }
                        Ok(n) => tracing::info!(speakers = n, "one voice; not labelling speakers"),
                        Err(e) => tracing::warn!(error = %format!("{e:#}"), "speaker separation failed"),
                    }
                }
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "no mic audio for speaker separation"),
            }
            std::fs::write(&transcript_file, serde_json::to_vec_pretty(&t)?)?;
        }
        t
    };

    // Real names for the voices (meeting app highlight, voiceprints, invitees, model).
    let transcript = {
        let mut t = transcript;
        if cfg.asr.speaker_names && !t.is_empty() {
            on_progress(Progress::IdentifyingSpeakers);
            let raw: Option<Vec<Segment>> = std::fs::read(dir.join(RAW_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok());
            match crate::speakers::pipeline::identify(cfg, dir, &meta, &mut t, raw, transcriber) {
                Ok(()) => std::fs::write(&transcript_file, serde_json::to_vec_pretty(&t)?)?,
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "naming speakers failed"),
            }
        }
        t
    };

    // Drop stale summary errors from a previous run; capture warnings stay.
    meta.error = meta.error.take().and_then(|e| {
        let kept: Vec<&str> = e.split("; ").filter(|p| !p.starts_with("Summary failed")).collect();
        (!kept.is_empty()).then(|| kept.join("; "))
    });
    // Keep a previously generated note if a later retry cannot reach its
    // backend. Regenerate is often used to change a template or gather context;
    // a transient failure must not erase an otherwise useful meeting summary.
    let previous_summary = std::fs::read(dir.join("summary.json")).ok().and_then(|b| serde_json::from_slice::<summary::Summary>(&b).ok());
    let summary = if cfg.llm.enabled && transcript.word_count() >= 20 {
        let target = cfg.llm.target(crate::secrets::llm_api_key);
        // The configured local endpoint may be Ollama or any OpenAI-compatible
        // server such as llama.cpp, LM Studio or vLLM. Probe /v1/models, where
        // any HTTP response means the server is reachable; only skip a true
        // connection failure rather than spending the full summary timeout.
        if target.local && !crate::ollama::endpoint_reachable(&target.endpoint) {
            tracing::info!(endpoint = %target.endpoint, "summary backend unavailable; writing transcript-only note");
            let msg = "Summary failed: no summary model is available. Set up a model in Settings → Summaries, then choose Summarize now.".to_string();
            meta.error = Some(match meta.error.take() {
                Some(prev) => format!("{prev}; {msg}"),
                None => msg,
            });
            previous_summary.clone()
        } else {
            on_progress(Progress::Summarizing);
        let lib = crate::library::Library::templates();
        let tpl = crate::recipe::template_for(cfg, &lib, dir, &meta);
        tracing::info!(template = %tpl.id, "summarizing");
        let app = crate::speakers::timeline::summarize(&crate::speakers::timeline::load(dir));
        let mut me = crate::identity::Me::resolve(&cfg.me, meta.event.as_ref(), app.me.as_deref());
        me.locate(&app.roster);
        // everyone else in the meeting, so points and action items get names even when the transcript says "Them"
        let gathered = crate::speakers::participants::Participants::gather(&me, meta.event.as_ref(), &app);
        let not_present: Vec<String> = gathered.absent().iter().map(|p| p.name.clone()).collect();
        let people: Vec<String> = gathered
            .present()
            .iter()
            .map(|p| {
                let d = summary::describe_person(&p.name, p.email.as_deref(), &me.emails);
                // "Sathiren Vale (guest, guest.example.org; Product Lead at Example Labs)"
                match ctx.person(&p.name, p.email.as_deref()).and_then(|c| c.short()) {
                    Some(role) if d.ends_with(')') => format!("{}; {role})", &d[..d.len() - 1]),
                    Some(role) => format!("{d} ({role})"),
                    None => d,
                }
            })
            .collect();
        let terms = vocab.hint_terms(&transcript);
        let context = ctx.prompt_block();
        if !context.is_empty() {
            tracing::info!(chars = context.len(), gathered = ctx.gathered.is_some(), "summary context");
        }
        let opts = summary::Options {
            vocabulary: &terms,
            template: crate::recipe::template_instructions(&tpl),
            language: &cfg.llm.language,
            me: me.display(),
            people: &people,
            not_present: &not_present,
            context: &context,
        };
            match summary::summarize_opts(&cfg.llm, &target, &opts, &meta.title, &transcript) {
                Ok(mut s) => {
                    s.template = Some(tpl.id.clone());
                    meta.attended = Some(attended(&gathered, &s.absent));
                    summary::own_action_items(&mut s, &transcript.recorder_label());
                    Some(s)
                }
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "summary failed; writing transcript-only note");
                    let msg = format!("Summary failed: {e:#}");
                    meta.error = Some(match meta.error.take() {
                        Some(prev) => format!("{prev}; {msg}"),
                        None => msg,
                    });
                    previous_summary.clone()
                }
            }
        }
    } else {
        previous_summary
    };
    if let Some(sm) = &summary {
        let _ = std::fs::write(dir.join("summary.json"), serde_json::to_vec_pretty(sm)?);
    }
    // Name untitled meetings ("Meeting 15:20"): the model's title, or failing
    // that a few words from the transcript. Calendar titles are kept.
    if meta.event.is_none() && summary::is_placeholder_title(&meta.title) {
        let from_model = summary
            .as_ref()
            .and_then(|s| s.title.clone())
            .map(|t| t.trim().trim_matches('"').to_string())
            .filter(|t| !t.is_empty() && !summary::is_placeholder_title(t));
        if let Some(t) = from_model.or_else(|| summary::fallback_title(&transcript)) {
            tracing::info!(title = %t, "titled meeting");
            meta.title = t;
        }
    }

    on_progress(Progress::Writing);
    let keep_audio = cfg.output.keep_audio_days != 0;
    // Reuse the existing note file unless the meeting was (re)titled: then
    // write under the new name and remove the old "Meeting 15 20.md".
    let fresh = note::note_path(&cfg.output, &meta);
    let path = match &meta.note_path {
        Some(p) if p.exists() => {
            let stem = |p: &Path| p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let wanted = stem(&fresh);
            if stem(p) == wanted || wanted.starts_with(&format!("{} ", stem(p))) {
                p.clone()
            } else {
                let _ = std::fs::remove_file(p);
                fresh
            }
        }
        _ => fresh,
    };
    let md = note::render(&meta, &transcript, summary.as_ref(), keep_audio.then_some(dir), cfg.output.include_transcript);
    note::write_note(&path, &md)?;
    meta.note_path = Some(path.clone());
    meta.save(dir)?;
    // Recipe results the user saved into the note survive a rewrite.
    if let Err(e) = crate::recipe::reapply_saved(dir) {
        tracing::warn!(error = %format!("{e:#}"), "couldn't restore saved recipe results");
    }
    if let Err(e) = note::link_from_daily_note(&cfg.output, &meta, &path) {
        tracing::warn!(error = %e, "daily note link failed");
    }
    meta.note_path = Some(path.clone());
    meta.state = SessionState::Done;
    meta.save(dir)?;
    if !keep_audio {
        for ch in [Channel::Mic, Channel::System] {
            let _ = std::fs::remove_file(wav_path(dir, ch));
        }
    }
    tracing::info!(note = %path.display(), words = transcript.word_count(), "meeting note written");
    // Destinations with "send automatically" rules that match this meeting.
    let sent = crate::destinations::auto_send(cfg, dir).into_iter().map(|(name, r)| (name, r.err().map(|e| format!("{e:#}")))).collect();
    Ok(Processed { note: path, transcript, summary, sent })
}

/// Who was in the meeting, for the header, note and share page: the people
/// the meeting app showed (or, without its roster, everyone invited), minus
/// anyone the summary found the conversation says wasn't there.
fn attended(people: &crate::speakers::participants::Participants, absent: &[String]) -> Vec<crate::session::Attended> {
    use crate::speakers::participants::same_person;
    people
        .present()
        .into_iter()
        // the model may add a description or the full name in brackets:
        // "Shelley Morgan (colleague of the recorder)", "Shel (Shelley Morgan)"
        .filter(|p| !absent.iter().any(|a| a.split(['(', ')', ';', ',']).any(|n| same_person(n, &p.name))))
        .map(|p| crate::session::Attended { name: p.name.clone(), email: p.email.clone() })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Local;
    use tempfile::tempdir;

    struct NoopTranscriber;

    /// Deserialize a session fixture so new serde-defaulted metadata fields do
    /// not force every pipeline test to know about them.
    fn test_meta(title: &str) -> SessionMeta {
        serde_json::from_value(serde_json::json!({
            "title": title,
            "started": Local::now(),
            "ended": Local::now(),
            "event": null,
            "state": "Transcribed",
            "note_path": null,
            "error": null,
            "routes": [],
            "sends": [],
            "meeting_app": null,
        }))
        .unwrap()
    }

    #[test]
    fn attended_drops_who_the_conversation_says_was_absent() {
        use crate::speakers::{participants::Participants, timeline};
        let me = crate::identity::Me::from_config(&crate::config::MeConfig { name: "Alex Rowan".into(), emails: vec!["alex@example.com".into()], aliases: vec![], label: String::new(), auto: true });
        // no roster read: everyone invited counts, until the summary says otherwise
        let mut ev = crate::destinations::testutil::event("alex@example.com", &["alex@example.com", "shelley@example.com", "andy@example.org"]);
        ev.attendees[1].name = Some("Shelley Morgan".into());
        ev.attendees[2].name = Some("Andy Reed".into());
        let p = Participants::gather(&me, Some(&ev), &timeline::Summary::default());
        let names = |a: Vec<crate::session::Attended>| a.into_iter().map(|a| a.name).collect::<Vec<_>>();
        assert_eq!(names(attended(&p, &[])), vec!["Shelley Morgan", "Andy Reed"]);
        assert_eq!(names(attended(&p, &["Shelley Morgan (colleague of the recorder)".into()])), vec!["Andy Reed"]);
        assert_eq!(names(attended(&p, &["Andy".into()])), vec!["Shelley Morgan"]);
        assert_eq!(names(attended(&p, &["Andee (Andy Reed)".into()])), vec!["Shelley Morgan"]);
    }

    impl Transcriber for NoopTranscriber {
        fn transcribe(&mut self, _: &[f32], _: Channel, _: &mut dyn FnMut(f32)) -> anyhow::Result<Vec<Segment>> {
            Ok(vec![])
        }
    }

    #[test]
    fn unavailable_ollama_writes_a_transcript_note_without_waiting_for_summary() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("meeting");
        std::fs::create_dir_all(&dir).unwrap();
        let meta = test_meta("No model needed");
        meta.save(&dir).unwrap();
        let transcript = Transcript::from_channels(vec![Segment::new(Channel::Mic, 0.0, 20.0, "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty")]);
        std::fs::write(dir.join("transcript.json"), serde_json::to_vec(&transcript).unwrap()).unwrap();

        let mut cfg = Config::default();
        cfg.output.notes_dir = tmp.path().join("notes").to_string_lossy().to_string();
        cfg.output.audio_dir = tmp.path().join("recordings").to_string_lossy().to_string();
        cfg.llm.endpoint = "http://127.0.0.1:9/v1".into();
        cfg.asr.speaker_names = false;
        let mut asr = NoopTranscriber;
        let mut progress = vec![];
        let out = process_session(&cfg, &dir, &mut asr, &mut |p| progress.push(p)).unwrap();

        assert!(out.summary.is_none());
        assert!(!progress.iter().any(|p| matches!(p, Progress::Summarizing)));
        let note = std::fs::read_to_string(out.note).unwrap();
        assert!(note.contains("no summary model is available"));
        assert!(!note.contains("No summary for this meeting"));
        assert!(note.contains("## Transcript"));
        let saved = SessionMeta::load(&dir).unwrap();
        assert_eq!(saved.state, SessionState::Done);
        assert!(saved.error.unwrap_or_default().contains("no summary model is available"));
    }

    #[test]
    fn failed_retry_preserves_an_existing_summary() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("meeting");
        std::fs::create_dir_all(&dir).unwrap();
        let meta = test_meta("Keep this summary");
        meta.save(&dir).unwrap();
        let transcript = Transcript::from_channels(vec![Segment::new(Channel::Mic, 0.0, 20.0, "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty")]);
        std::fs::write(dir.join("transcript.json"), serde_json::to_vec(&transcript).unwrap()).unwrap();
        let existing = summary::Summary { summary: "Already useful.".into(), ..Default::default() };
        std::fs::write(dir.join("summary.json"), serde_json::to_vec(&existing).unwrap()).unwrap();

        let mut cfg = Config::default();
        cfg.output.notes_dir = tmp.path().join("notes").to_string_lossy().to_string();
        cfg.output.audio_dir = tmp.path().join("recordings").to_string_lossy().to_string();
        cfg.llm.endpoint = "http://127.0.0.1:9/v1".into();
        cfg.asr.speaker_names = false;
        let mut asr = NoopTranscriber;
        let out = process_session(&cfg, &dir, &mut asr, &mut |_| {}).unwrap();

        assert_eq!(out.summary, Some(existing));
        let saved: summary::Summary = serde_json::from_slice(&std::fs::read(dir.join("summary.json")).unwrap()).unwrap();
        assert_eq!(saved.summary, "Already useful.");
    }
}

/// Delete audio older than `keep_audio_days` from finished sessions.
pub fn prune_audio(cfg: &Config) {
    if cfg.output.keep_audio_days < 0 {
        return;
    }
    let cutoff = chrono::Local::now() - chrono::Duration::days(cfg.output.keep_audio_days);
    for (dir, meta) in crate::session::list_sessions(&cfg.paths().audio_dir, usize::MAX) {
        if meta.state == SessionState::Done && meta.started < cutoff {
            for ch in [Channel::Mic, Channel::System] {
                let _ = std::fs::remove_file(wav_path(&dir, ch));
            }
        }
    }
}
