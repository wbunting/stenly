//! Speaker-attributed transcripts.

use crate::platform::Channel;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub channel: Channel,
    /// Seconds from the start of the recording.
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// Voice cluster within the channel (0, 1, 2…), from speaker separation.
    /// None when the channel wasn't separated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<u32>,
    /// Who this is ("Sarah Chen"), once speakers were identified or renamed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// How a speaker's name was found, most to least reliable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NameSource {
    /// The user named them in Stenly.
    User,
    /// The meeting app's active-speaker highlight (Accessibility).
    Accessibility,
    /// Matched a voice the user named in an earlier meeting.
    Voiceprint,
    /// The only other person invited to a one-to-one call.
    Calendar,
    /// Suggested by the language model from the invite + transcript cues.
    Llm,
    /// The person who recorded (their own mic on a call), shown by their name.
    Recorder,
}

/// One voice in the transcript and what we know about who it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakerInfo {
    pub channel: Channel,
    /// Voice cluster within the channel; None = the whole channel is one voice.
    pub speaker: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<NameSource>,
    /// 0–1; only names we're confident about are applied.
    #[serde(default)]
    pub confidence: f32,
    /// Seconds of speech.
    #[serde(default)]
    pub talk_secs: f64,
    /// The invited person this voice is, when that's a different name from
    /// what they're called in the meeting ("Sathiren Vale" for "Seth").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    /// Their address on the invite (or remembered with their voice).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub segments: Vec<Segment>,
    /// The meeting happened in one room: everyone spoke into the mic and the
    /// call-audio channel stayed silent.
    #[serde(default)]
    pub in_person: bool,
    /// Voices found and their names. Empty until speakers were identified.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub speakers: Vec<SpeakerInfo>,
}

impl Segment {
    pub fn new(channel: Channel, start: f64, end: f64, text: impl Into<String>) -> Self {
        Self { channel, start, end, text: text.into(), speaker: None, name: None }
    }

    /// The speaker's name, else "Speaker 2" once voices were separated, else "Me"/"Them".
    pub fn label(&self) -> String {
        if let Some(n) = self.name.as_deref().filter(|n| !n.trim().is_empty()) {
            return n.to_string();
        }
        unnamed_label(self.channel, self.speaker)
    }

    /// "Speaker [mm:ss]: text", one line of LLM input.
    pub fn plain_line(&self) -> String {
        format!("{} [{}]: {}", self.label(), crate::util::fmt_timestamp(self.start), self.text)
    }

    /// Who said it, for display: like [`label`](Self::label) but the recorder is "You".
    pub fn display_label(&self) -> String {
        match self.label() {
            l if l == crate::voices::SELF_NAME && self.channel == Channel::Mic => "You".into(),
            l => l,
        }
    }
}

/// Turns 16 kHz mono speech into timestamped text. Implemented by `stenly-asr`.
pub trait Transcriber: Send {
    /// Transcribe a whole recording; returns segments with times relative to `samples[0]`.
    fn transcribe(&mut self, samples: &[f32], channel: Channel, progress: &mut dyn FnMut(f32)) -> anyhow::Result<Vec<Segment>>;

    /// Tell apart the voices in one channel's segments (sets `Segment::speaker`).
    /// Returns the number of speakers found; 0 = not supported / not enough speech.
    fn separate_speakers(&mut self, segs: &mut [Segment], audio: &[f32]) -> anyhow::Result<usize> {
        let embs = self.embed_segments(segs, audio)?;
        if embs.iter().flatten().count() < 2 {
            return Ok(0);
        }
        let c = crate::voices::cluster(segs, &embs);
        for (s, l) in segs.iter_mut().zip(&c.labels) {
            s.speaker = *l;
        }
        Ok(c.count())
    }

    /// One unit-length voice embedding per segment (None for segments too
    /// short to tell a voice from). Empty = not supported.
    fn embed_segments(&mut self, _segs: &[Segment], _audio: &[f32]) -> anyhow::Result<Vec<Option<Vec<f32>>>> {
        Ok(Vec::new())
    }

    /// Re-decode one segment's audio into timed words (times from the start
    /// of the recording), to split it where the speaker changes. Empty = not supported.
    fn words(&mut self, _seg: &Segment, _audio: &[f32]) -> Vec<TimedWord> {
        Vec::new()
    }
}

/// A word and when it was said (seconds from the start of the recording).
#[derive(Debug, Clone, PartialEq)]
pub struct TimedWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// "Speaker 2" for a separated voice, else the channel's "Me"/"Them".
pub fn unnamed_label(channel: Channel, speaker: Option<u32>) -> String {
    match speaker {
        Some(n) => format!("Speaker {}", n + 1),
        None => channel.speaker_label().to_string(),
    }
}

impl Transcript {
    /// Merge both channels into one chronological conversation, gluing
    /// consecutive segments from the same speaker into turns.
    pub fn from_channels(mut segs: Vec<Segment>) -> Self {
        segs.retain(|s| !s.text.trim().is_empty());
        segs.sort_by(|a, b| a.start.total_cmp(&b.start));
        let mut out: Vec<Segment> = Vec::new();
        for s in segs {
            if let Some(last) = out.last_mut()
                && last.channel == s.channel && last.speaker == s.speaker && last.name == s.name && s.start - last.end < 2.5 && last.end - last.start < 60.0 {
                    last.text.push(' ');
                    last.text.push_str(s.text.trim());
                    last.end = last.end.max(s.end);
                    continue;
                }
            out.push(Segment { text: s.text.trim().to_string(), ..s });
        }
        Self { segments: out, in_person: false, speakers: Vec::new() }
    }

    /// Info for a voice, if it was identified.
    pub fn speaker_info(&self, channel: Channel, speaker: Option<u32>) -> Option<&SpeakerInfo> {
        self.speakers.iter().find(|s| s.channel == channel && s.speaker == speaker)
    }

    /// Distinct voices in order of first appearance: (channel, cluster, label).
    pub fn voices(&self) -> Vec<(Channel, Option<u32>, String)> {
        let mut out: Vec<(Channel, Option<u32>, String)> = Vec::new();
        for s in &self.segments {
            if !out.iter().any(|(c, k, _)| *c == s.channel && *k == s.speaker) {
                out.push((s.channel, s.speaker, s.label()));
            }
        }
        out
    }

    /// Whether any voice besides the recorder has a real name.
    pub fn has_names(&self) -> bool {
        self.segments.iter().any(|s| {
            s.name.as_deref().is_some_and(|n| n != crate::voices::SELF_NAME) && self.speaker_info(s.channel, s.speaker).is_none_or(|i| i.source != Some(NameSource::Recorder))
        })
    }

    /// What the recorder's own lines are labelled: their name ("Alex") once
    /// speakers were identified, else "Me".
    pub fn recorder_label(&self) -> String {
        self.speakers
            .iter()
            .find(|i| i.source == Some(NameSource::Recorder))
            .and_then(|i| i.name.clone())
            .unwrap_or_else(|| crate::voices::SELF_NAME.to_string())
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn word_count(&self) -> usize {
        self.segments.iter().map(|s| s.text.split_whitespace().count()).sum()
    }

    /// Plain "Speaker [mm:ss]: text" lines, used as LLM input.
    pub fn to_plain(&self) -> String {
        self.segments.iter().map(Segment::plain_line).collect::<Vec<_>>().join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaves_and_merges() {
        let seg = |c, s: f64, e: f64, t: &str| Segment::new(c, s, e, t);
        let t = Transcript::from_channels(vec![
            seg(Channel::System, 0.0, 2.0, "Hi there."),
            seg(Channel::Mic, 2.5, 3.5, "Hey!"),
            seg(Channel::System, 4.0, 5.0, "So,"),
            seg(Channel::System, 5.5, 7.0, "about the plan."),
            seg(Channel::Mic, 9.0, 9.5, "  "),
        ]);
        let lines: Vec<_> = t.segments.iter().map(|s| (s.channel, s.text.as_str())).collect();
        assert_eq!(lines, [(Channel::System, "Hi there."), (Channel::Mic, "Hey!"), (Channel::System, "So, about the plan.")]);
        assert_eq!(t.word_count(), 7);
    }
}
