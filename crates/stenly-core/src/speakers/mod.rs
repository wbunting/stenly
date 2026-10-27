//! Real names for the voices in a transcript.
//!
//! Sources, most reliable first (a voice keeps the best name it gets):
//! 1. the user renamed it in Stenly ([`rename`]);
//! 2. the meeting app's active-speaker highlight, read through
//!    Accessibility during the call ([`timeline`]): each "Them" voice cluster
//!    gets the name that was highlighted most while it spoke;
//! 3. a voice the user named in an earlier meeting ([`crate::voices`]);
//! 4. a one-to-one call: the only other person in the meeting
//!    ([`participants`]: the invite, the meeting app's roster and host);
//! 5. transcript cues ("Andy, have people started…" answered by that voice)
//!    and a language-model pass over the invite list and transcript — only
//!    confident suggestions are applied ([`llm`]);
//! 6. elimination: when one invitee is left and one voice is unnamed, that
//!    voice is them ([`name_by_elimination`]).
//!
//! Spoken names are linked to the invite ("Seth" is Sathiren Vale,
//! [`link_to_invitees`]). Names from the conversation that are confident and
//! match the invite are remembered as voiceprints, so a later call without
//! an invite still knows the voice ([`pipeline`]).
//!
//! "Me" is [`crate::identity::Me`]: Settings → You plus what the calendar
//! account and the meeting app say. The recorder's own lines are labelled
//! with their name ([`name_recorder`]). Each meeting app is read by its own
//! driver ([`drivers`]).
//!
//! Everything runs on this Mac except step 5 when the user chose a hosted
//! summary model (the transcript already goes there for the summary).

pub mod ax;
pub mod drivers;
pub mod llm;
pub mod participants;
pub mod pipeline;
pub mod timeline;

use crate::calendar::CalendarEvent;
use crate::identity::Me;
use crate::platform::Channel;
use crate::transcript::{NameSource, Segment, SpeakerInfo, Transcript};
use crate::voices::{Clusters, SELF_NAME, VoiceStore};

/// Of the time a voice was highlighted at all, this share must be one name…
const AX_MIN_DOMINANCE: f64 = 0.6;
/// …it must be lit for this share of the voice's observed talk time (apps
/// flicker the indicator with the audio level, so this is well below 1)…
const AX_MIN_SHARE: f64 = 0.12;
/// …and for this many seconds, to take the highlighted name.
const AX_MIN_SECS: f64 = 3.0;
/// LLM suggestions below this confidence are ignored.
pub const LLM_MIN_CONFIDENCE: f32 = 0.75;
/// Confidence of a name found by elimination (one voice, one invitee left).
pub const ELIMINATION_CONFIDENCE: f32 = 0.7;
/// A voice needs this much speech to count as a person for elimination
/// (tiny clusters are crosstalk or a mis-split).
const ELIMINATION_MIN_SECS: f64 = 20.0;

/// Per-voice embeddings from the pipeline, used for voiceprints.
#[derive(Debug, Clone, Default)]
pub struct VoiceData {
    /// (channel, cluster) → unit centroid, seconds of speech.
    pub centroids: Vec<(Channel, Option<u32>, Vec<f32>, f64)>,
}

impl VoiceData {
    pub fn add_clusters(&mut self, ch: Channel, c: &Clusters) {
        for (i, (cent, talk)) in c.centroids.iter().zip(&c.talk).enumerate() {
            self.centroids.push((ch, Some(i as u32), cent.clone(), *talk));
        }
    }
    pub fn get(&self, ch: Channel, speaker: Option<u32>) -> Option<(&[f32], f64)> {
        self.centroids.iter().find(|(c, s, _, _)| *c == ch && *s == speaker).map(|(_, _, v, t)| (v.as_slice(), *t))
    }
}

/// Make sure every voice in the transcript has a [`SpeakerInfo`] entry, keeping
/// names already there (e.g. from a rename) and adding talk time.
pub fn sync_infos(t: &mut Transcript) {
    let mut infos: Vec<SpeakerInfo> = Vec::new();
    for s in &t.segments {
        let dur = (s.end - s.start).max(0.0);
        match infos.iter_mut().find(|i| i.channel == s.channel && i.speaker == s.speaker) {
            Some(i) => i.talk_secs += dur,
            None => {
                let prev = t.speaker_info(s.channel, s.speaker);
                infos.push(SpeakerInfo {
                    channel: s.channel,
                    speaker: s.speaker,
                    name: prev.and_then(|p| p.name.clone()),
                    source: prev.and_then(|p| p.source),
                    confidence: prev.map(|p| p.confidence).unwrap_or(0.0),
                    talk_secs: dur,
                    person: prev.and_then(|p| p.person.clone()),
                    email: prev.and_then(|p| p.email.clone()),
                })
            }
        }
    }
    t.speakers = infos;
}

/// Write names from `t.speakers` onto the segments.
pub fn apply_names(t: &mut Transcript) {
    for s in t.segments.iter_mut() {
        s.name = t.speakers.iter().find(|i| i.channel == s.channel && i.speaker == s.speaker).and_then(|i| i.name.clone());
    }
}

fn source_rank(s: Option<NameSource>) -> u8 {
    match s {
        Some(NameSource::User) => 6,
        Some(NameSource::Recorder) => 5,
        Some(NameSource::Accessibility) => 4,
        Some(NameSource::Voiceprint) => 3,
        Some(NameSource::Calendar) => 2,
        Some(NameSource::Llm) => 1,
        None => 0,
    }
}

/// Set a name unless a more reliable source already named this voice.
fn propose(info: &mut SpeakerInfo, name: &str, source: NameSource, confidence: f32) -> bool {
    let name = name.trim();
    if name.is_empty() || source_rank(Some(source)) <= source_rank(info.source) && info.name.is_some() {
        return false;
    }
    if info.name.as_deref() != Some(name) {
        info.person = None;
        info.email = None;
    }
    info.name = Some(name.to_string());
    info.source = Some(source);
    info.confidence = confidence;
    true
}

/// The recorder's own voice: their mic on a call, or (in a room) the voice
/// that matched their voiceprint. Labelled with [`Me::label`] ("Alex")
/// instead of "Me", unless the user chose to keep "Me".
pub fn name_recorder(t: &mut Transcript, me: &Me) -> bool {
    let label = me.label();
    let in_person = t.in_person;
    let Some(info) = t.speakers.iter_mut().find(|i| {
        if in_person {
            i.name.as_deref() == Some(SELF_NAME) || i.source == Some(NameSource::Recorder)
        } else {
            i.channel == Channel::Mic && i.speaker.is_none()
        }
    }) else {
        return false;
    };
    if info.source == Some(NameSource::User) {
        return false;
    }
    if label == SELF_NAME {
        // the old label: no name (in a room, keep the voiceprint's "Me")
        if info.source == Some(NameSource::Recorder) {
            info.name = in_person.then(|| SELF_NAME.to_string());
            info.source = in_person.then_some(NameSource::Voiceprint);
        }
        return false;
    }
    info.name = Some(label);
    info.source = Some(NameSource::Recorder);
    info.confidence = 1.0;
    info.person = me.name.clone().filter(|n| Some(n) != info.name.as_ref());
    info.email = None;
    true
}

/// Names from the meeting app's speaker highlight: each voice gets the name
/// shown most while it was talking. A name goes to one voice only.
pub fn name_from_timeline(t: &mut Transcript, events: &[timeline::Event], me: &Me) -> usize {
    if events.is_empty() {
        return 0;
    }
    // (voice index, name, overlap secs, score)
    let mut votes: Vec<(usize, String, f64, f64)> = Vec::new();
    for (vi, info) in t.speakers.iter().enumerate() {
        if info.channel != Channel::System && !t.in_person {
            continue; // the mic is the user; calls only name the far end
        }
        let mut per: Vec<(String, f64)> = Vec::new();
        let mut seen = 0.0;
        for s in t.segments.iter().filter(|s| s.channel == info.channel && s.speaker == info.speaker) {
            seen += timeline::covered(events, s.start, s.end);
            for (n, secs) in timeline::overlap(events, s.start, s.end) {
                if me.is(&n) {
                    continue;
                }
                match per.iter_mut().find(|(x, _)| *x == n) {
                    Some(p) => p.1 += secs,
                    None => per.push((n, secs)),
                }
            }
        }
        let lit: f64 = per.iter().map(|(_, s)| s).sum();
        for (n, secs) in per {
            let dominance = if lit > 0.0 { secs / lit } else { 0.0 };
            let share = if seen > 0.0 { secs / seen } else { 0.0 };
            if secs >= AX_MIN_SECS && dominance >= AX_MIN_DOMINANCE && share >= AX_MIN_SHARE {
                votes.push((vi, n, secs, dominance));
            }
        }
    }
    // best pairs first; each voice and each name used once
    votes.sort_by(|a, b| b.2.total_cmp(&a.2));
    let mut used_v: Vec<usize> = Vec::new();
    let mut used_n: Vec<String> = Vec::new();
    let mut named = 0;
    for (vi, n, _, dominance) in votes {
        if used_v.contains(&vi) || used_n.contains(&n) {
            continue;
        }
        used_v.push(vi);
        used_n.push(n.clone());
        if propose(&mut t.speakers[vi], &n, NameSource::Accessibility, dominance as f32) {
            named += 1;
        }
    }
    named
}

/// One other person in the meeting and one far-end voice: that voice is
/// them. The people come from every source ([`participants`]), so this
/// works from the invite, from the meeting app's tiles (Zoom shows no
/// speaking flag in gallery view, but always the tiles), or both.
pub fn name_from_participants(t: &mut Transcript, people: &participants::Participants) -> usize {
    if t.in_person {
        return 0;
    }
    let far: Vec<usize> = (0..t.speakers.len()).filter(|&i| t.speakers[i].channel == Channel::System).collect();
    let Some(p) = people.only_other() else { return 0 };
    if far.len() != 1 {
        return 0;
    }
    let (source, conf) = if p.from_app() { (NameSource::Accessibility, 0.9) } else { (NameSource::Calendar, 0.8) };
    propose(&mut t.speakers[far[0]], &p.name, source, conf) as usize
}

/// Move single segments to another named voice when the meeting app clearly
/// showed that person talking through it (clustering by voice alone mixes up
/// people on the same codec). Run on raw, unmerged segments. Returns how many moved.
pub fn refine_with_timeline(segs: &mut [Segment], speakers: &[SpeakerInfo], events: &[timeline::Event]) -> usize {
    let voice_of = |name: &str| speakers.iter().find(|i| i.source == Some(NameSource::Accessibility) && i.name.as_deref() == Some(name)).map(|i| (i.channel, i.speaker));
    let mut moved = 0;
    for s in segs.iter_mut() {
        let dur = s.end - s.start;
        if dur < 1.0 {
            continue;
        }
        let ov = timeline::overlap(events, s.start, s.end);
        let lit: f64 = ov.iter().map(|(_, x)| x).sum();
        let Some((top, secs)) = ov.first() else { continue };
        if *secs < dur * 0.5 || *secs < lit * 0.8 {
            continue;
        }
        if let Some((ch, sp)) = voice_of(top) {
            if ch == s.channel && sp != s.speaker {
                s.speaker = sp;
                moved += 1;
            }
        }
    }
    moved
}

/// Names from stored voiceprints.
pub fn name_from_voiceprints(t: &mut Transcript, voices: &VoiceData, store: &VoiceStore) -> usize {
    let mut named = 0;
    let mut used: Vec<String> = t.speakers.iter().filter_map(|i| i.name.clone()).collect();
    for info in t.speakers.iter_mut() {
        // the recorder's own mic channel on a call is always "Me"
        if info.channel == Channel::Mic && !t.in_person {
            continue;
        }
        let Some((c, _)) = voices.get(info.channel, info.speaker) else { continue };
        let Some((v, sim)) = store.best_match(c) else { continue };
        let name = if v.name == SELF_NAME { SELF_NAME.to_string() } else { v.name.clone() };
        if used.iter().any(|u| u.eq_ignore_ascii_case(&name)) && info.name.as_deref() != Some(&name) {
            continue;
        }
        if propose(info, &name, NameSource::Voiceprint, sim) {
            used.push(name);
            named += 1;
        }
    }
    named
}

/// The invited person a voice's name refers to: same address, same name or
/// first name ("Shelley" → "Shelley Morgan"), or a nickname ("Seth" →
/// "Sathiren Vale") when exactly one unmatched invitee fits and nobody in
/// the meeting is called by that invitee's own first name.
fn invitee_for<'a>(t: &Transcript, people: &'a participants::Participants, vi: usize, taken: &[String]) -> Option<&'a participants::Person> {
    let info = &t.speakers[vi];
    let name = info.name.as_deref()?;
    if let Some(p) = people.find(info.person.as_deref().unwrap_or(name), info.email.as_deref()) {
        return Some(p);
    }
    let text = t.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ");
    let said = |first: &str| regex::Regex::new(&format!(r"(?i)\b{}\b", regex::escape(first))).is_ok_and(|r| r.is_match(&text));
    let fits: Vec<&participants::Person> = people
        .present()
        .into_iter()
        .filter(|p| !taken.iter().any(|t| t.eq_ignore_ascii_case(&p.name)))
        .filter(|p| participants::nickname_of(name, &p.name))
        .filter(|p| p.name.split_whitespace().next().is_some_and(|f| !said(f)))
        .collect();
    (fits.len() == 1).then(|| fits[0])
}

/// Far-end voices (or everyone, in a room) other than the recorder.
fn others(t: &Transcript) -> Vec<usize> {
    (0..t.speakers.len())
        .filter(|&i| {
            let s = &t.speakers[i];
            s.source != Some(NameSource::Recorder) && s.name.as_deref() != Some(SELF_NAME) && (if t.in_person { true } else { s.channel == Channel::System })
        })
        .collect()
}

/// Tie named voices to the people on the invite: `person` gets the invite's
/// name when it differs from the label, `email` their address. Returns how many were linked.
pub fn link_to_invitees(t: &mut Transcript, people: &participants::Participants) -> usize {
    let mut taken: Vec<String> = Vec::new();
    let mut linked = 0;
    // exact matches first, so nicknames only pick among who's left
    for pass in 0..2 {
        for vi in others(t) {
            if t.speakers[vi].name.is_none() || (pass == 1 && t.speakers[vi].email.is_some()) {
                continue;
            }
            let exact = {
                let i = &t.speakers[vi];
                people.find(i.person.as_deref().unwrap_or(i.name.as_deref().unwrap_or_default()), i.email.as_deref()).cloned()
            };
            let p = if pass == 0 { exact } else { invitee_for(t, people, vi, &taken).cloned() };
            let Some(p) = p else { continue };
            if taken.iter().any(|x| x.eq_ignore_ascii_case(&p.name)) {
                continue;
            }
            taken.push(p.name.clone());
            let i = &mut t.speakers[vi];
            if pass == 1 {
                tracing::info!(spoken = ?i.name, invitee = %p.name, "linked a spoken name to the invite");
            }
            i.person = (i.name.as_deref() != Some(p.name.as_str())).then(|| p.name.clone());
            i.email = p.email.clone();
            linked += 1;
        }
    }
    linked
}

/// One voice left unnamed and one person on the invite not matched to any
/// voice: that voice is them. Only when every named voice is accounted for
/// on the invite (an unmatched name could be that person under a nickname).
pub fn name_by_elimination(t: &mut Transcript, people: &participants::Participants) -> usize {
    if people.is_empty() {
        return 0;
    }
    let voices = others(t);
    let unnamed: Vec<usize> = voices.iter().copied().filter(|&i| t.speakers[i].name.is_none() && t.speakers[i].talk_secs >= ELIMINATION_MIN_SECS).collect();
    if unnamed.len() != 1 {
        return 0;
    }
    let named: Vec<&SpeakerInfo> = voices.iter().map(|&i| &t.speakers[i]).filter(|i| i.name.is_some()).collect();
    if named.iter().any(|i| i.email.is_none() && people.find(i.person.as_deref().or(i.name.as_deref()).unwrap_or_default(), None).is_none()) {
        return 0;
    }
    let left: Vec<&participants::Person> = people
        .present()
        .into_iter()
        .filter(|p| !named.iter().any(|i| i.email.is_some() && i.email == p.email || [i.name.as_deref(), i.person.as_deref()].into_iter().flatten().any(|n| participants::same_person(n, &p.name))))
        .collect();
    if left.len() != 1 {
        return 0;
    }
    let p = left[0].clone();
    let info = &mut t.speakers[unnamed[0]];
    if !propose(info, &p.name, NameSource::Calendar, ELIMINATION_CONFIDENCE) {
        return 0;
    }
    info.email = p.email.clone();
    tracing::info!(name = %p.name, "named the last voice after the last invitee");
    1
}

/// The other people invited (excluding the calendar owner and declines).
pub fn invitees(ev: &CalendarEvent) -> Vec<String> {
    let me = ev.calendar.as_deref();
    let mut v: Vec<String> = Vec::new();
    for a in ev.attendees.iter().chain(ev.organizer.iter()) {
        if me.is_some_and(|m| a.email.eq_ignore_ascii_case(m)) || a.status.as_deref() == Some("DECLINED") {
            continue;
        }
        let n = person_name(a);
        if !v.iter().any(|x| x.eq_ignore_ascii_case(&n)) {
            v.push(n);
        }
    }
    v
}

/// "Shelley Morgan <shelley@x>" → "Shelley Morgan"; "andy@x.com" → "Andy".
pub fn person_name(a: &crate::calendar::Attendee) -> String {
    if let Some(n) = a.name.as_deref() {
        let n = n.split('<').next().unwrap_or(n).trim().trim_matches('"');
        if !n.is_empty() && !n.contains('@') {
            return n.to_string();
        }
    }
    let local = a.email.split('@').next().unwrap_or(&a.email);
    local
        .split(['.', '_', '-'])
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Who the labels in a named transcript are, for the summary prompt.
pub fn summary_hint(t: &Transcript, me: Option<&str>) -> String {
    let mut named: Vec<String> = Vec::new();
    let mut unnamed = false;
    let recorder = t.recorder_label();
    for (ch, sp, _) in t.voices() {
        let info = t.speaker_info(ch, sp);
        if info.is_some_and(|i| i.source == Some(NameSource::Recorder)) {
            continue;
        }
        match info.and_then(|i| i.name.as_ref()) {
            Some(n) if n == crate::voices::SELF_NAME => {}
            Some(n) => {
                let i = info.expect("named");
                let mut x = n.clone();
                if let Some(p) = &i.person {
                    x.push_str(&format!(" ({p} on the invite)"));
                }
                if matches!(i.source, Some(NameSource::Llm | NameSource::Calendar)) && i.confidence < 0.9 {
                    x.push_str(" (name inferred)");
                }
                named.push(x);
            }
            None if ch == Channel::Mic && sp.is_none() && !t.in_person => {}
            None => unnamed = true,
        }
    }
    let who = match me {
        Some(m) if m != recorder => format!("{m}, "),
        _ => String::new(),
    };
    let mut h = format!(
        "Lines are labelled with the speaker's name. \"{recorder}\" is {who}the person who recorded it{}. Other speakers: {}.",
        if t.in_person { " (in the room)" } else { "" },
        if named.is_empty() { "not identified".to_string() } else { named.join(", ") }
    );
    if unnamed {
        h.push_str(" Labels like \"Speaker 2\" or \"Them\" are people who couldn't be identified.");
    }
    h.push_str(&format!(" Use these names in bullets and action-item owners; the recorder's action items are owned by \"{recorder}\"."));
    h
}

/// Rename one voice everywhere in the transcript. Returns the old label.
pub fn rename(t: &mut Transcript, channel: Channel, speaker: Option<u32>, name: &str) -> Option<String> {
    sync_infos(t);
    let info = t.speakers.iter_mut().find(|i| i.channel == channel && i.speaker == speaker)?;
    let old = info.name.clone().unwrap_or_else(|| crate::transcript::unnamed_label(channel, speaker));
    let name = name.trim();
    if name.is_empty() {
        info.name = None;
        info.source = None;
        info.confidence = 0.0;
    } else {
        if info.name.as_deref() != Some(name) {
            info.person = None;
            info.email = None;
        }
        info.name = Some(name.to_string());
        info.source = Some(NameSource::User);
        info.confidence = 1.0;
    }
    apply_names(t);
    Some(old)
}

/// Raw (unmerged) segments of one channel, split into far-end voices.
/// Returns the clusters and writes `speaker` on the segments when there are 2+.
pub fn split_channel(segs: &mut [Segment], embs: &[Option<Vec<f32>>]) -> Clusters {
    let c = crate::voices::cluster(segs, embs);
    if c.count() >= 2 {
        for (s, l) in segs.iter_mut().zip(&c.labels) {
            s.speaker = *l;
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::Attendee;

    fn seg(ch: Channel, s: f64, e: f64, sp: Option<u32>) -> Segment {
        let mut x = Segment::new(ch, s, e, "words here");
        x.speaker = sp;
        x
    }

    fn call() -> Transcript {
        let mut t = Transcript::from_channels(vec![
            seg(Channel::System, 0.0, 10.0, Some(0)),
            seg(Channel::Mic, 11.0, 15.0, None),
            seg(Channel::System, 16.0, 30.0, Some(1)),
            seg(Channel::System, 31.0, 40.0, Some(0)),
        ]);
        sync_infos(&mut t);
        t
    }

    fn ev(t: f64, active: &[&str]) -> timeline::Event {
        timeline::Event { t, app: "teams".into(), active: active.iter().map(|s| s.to_string()).collect(), roster: vec![], ..Default::default() }
    }

    #[test]
    fn timeline_names_far_end_voices() {
        let mut t = call();
        // A lit while voice 0 talks, B while voice 1 talks; the user's own tile lights during the mic turn
        let evs = vec![ev(0.0, &["Ann"]), ev(10.0, &["Me Myself"]), ev(16.0, &["Bo"]), ev(30.5, &["Ann"]), ev(40.0, &[])];
        let n = name_from_timeline(&mut t, &evs, &Me::named("Me Myself"));
        assert_eq!(n, 2);
        apply_names(&mut t);
        let labels: Vec<String> = t.segments.iter().map(|s| s.label()).collect();
        assert_eq!(labels, vec!["Ann", "Me", "Bo", "Ann"]);
    }

    #[test]
    fn timeline_without_split_names_single_far_voice() {
        let mut t = Transcript::from_channels(vec![seg(Channel::System, 0.0, 10.0, None), seg(Channel::System, 20.0, 30.0, None)]);
        sync_infos(&mut t);
        let evs = vec![ev(0.0, &["Ann"]), ev(9.0, &[]), ev(20.0, &["Ann"]), ev(30.0, &[])];
        assert_eq!(name_from_timeline(&mut t, &evs, &Me::default()), 1);
        assert_eq!(t.speakers[0].name.as_deref(), Some("Ann"));
    }

    #[test]
    fn weak_overlap_is_ignored() {
        let mut t = call();
        // Bo flickers for 1 s during voice 1's 14 s
        let evs = vec![ev(16.0, &["Bo"]), ev(17.0, &[])];
        assert_eq!(name_from_timeline(&mut t, &evs, &Me::default()), 0);
        // two people lit about equally over one voice: not dominant enough
        let mut t = call();
        let evs = vec![ev(16.0, &["Bo"]), ev(21.0, &["Cy"]), ev(26.0, &[])];
        assert_eq!(name_from_timeline(&mut t, &evs, &Me::default()), 0);
    }

    #[test]
    fn share_counts_only_observed_time() {
        // the watcher started late: voice 1 (16–30 s) was only observed from 28 s
        let mut t = call();
        let evs = vec![ev(28.0, &["Bo"]), ev(29.0, &[]), ev(30.0, &[])];
        assert_eq!(name_from_timeline(&mut t, &evs, &Me::default()), 0, "1 s isn't enough evidence");
        let evs = vec![ev(25.0, &["Bo"]), ev(29.0, &[]), ev(30.0, &[])];
        assert_eq!(name_from_timeline(&mut t, &evs, &Me::default()), 1);
    }

    #[test]
    fn refinement_moves_clearly_highlighted_segments() {
        let mut t = call();
        name_from_timeline(&mut t, &[ev(0.0, &["Ann"]), ev(5.0, &["Ann"]), ev(10.0, &[]), ev(16.0, &["Bo"]), ev(21.0, &["Bo"]), ev(26.0, &["Bo"]), ev(30.0, &[])], &Me::default());
        // a raw segment clustered as voice 0 (Ann) while Bo was clearly lit
        let mut raw = vec![seg(Channel::System, 50.0, 54.0, Some(0)), seg(Channel::System, 60.0, 64.0, Some(0))];
        let evs = vec![ev(50.0, &["Bo"]), ev(54.0, &[]), ev(60.0, &["Bo"]), ev(60.5, &[])];
        assert_eq!(refine_with_timeline(&mut raw, &t.speakers, &evs), 1);
        assert_eq!(raw[0].speaker, Some(1));
        assert_eq!(raw[1].speaker, Some(0), "a brief flicker isn't enough");
    }

    #[test]
    fn user_rename_wins_and_sticks() {
        let mut t = call();
        // (the watcher re-writes a state every 5 s; a state older than 6 s is not trusted)
        let evs = [ev(0.0, &["Ann"]), ev(5.0, &["Ann"]), ev(10.0, &[])];
        name_from_timeline(&mut t, &evs, &Me::default());
        assert_eq!(rename(&mut t, Channel::System, Some(0), "Annabel").as_deref(), Some("Ann"));
        // a later automatic pass doesn't override the user
        name_from_timeline(&mut t, &evs, &Me::default());
        sync_infos(&mut t);
        apply_names(&mut t);
        assert_eq!(t.segments[0].label(), "Annabel");
        assert_eq!(t.speaker_info(Channel::System, Some(0)).unwrap().source, Some(NameSource::User));
        // clearing a name falls back to the label
        rename(&mut t, Channel::System, Some(0), "");
        assert_eq!(t.segments[0].label(), "Speaker 1");
    }

    fn one_far_voice() -> Transcript {
        let mut t = Transcript::from_channels(vec![seg(Channel::System, 0.0, 10.0, None), seg(Channel::Mic, 11.0, 15.0, None)]);
        sync_infos(&mut t);
        t
    }

    #[test]
    fn one_to_one_from_the_invite() {
        let person = |n: Option<&str>, e: &str| Attendee { name: n.map(Into::into), email: e.into(), status: Some("ACCEPTED".into()) };
        let mut e = crate::calendar::CalendarEvent {
            id: "x".into(),
            uid: "x".into(),
            title: "1:1".into(),
            start: chrono::Utc::now(),
            end: chrono::Utc::now(),
            link: None,
            organizer: None,
            attendees: vec![person(None, "me@x.com"), person(Some("Shelley Morgan <shelley@example.com>"), "shelley@example.com")],
            description: None,
            calendar: Some("me@x.com".into()),
            declined: false,
            recurring: false,
            html_link: None,
            calendar_name: None,
            color: None,
        };
        assert_eq!(invitees(&e), vec!["Shelley Morgan"]);
        let me = Me::resolve(&Default::default(), Some(&e), None);
        let mut t = one_far_voice();
        let people = participants::Participants::gather(&me, Some(&e), &Default::default());
        assert_eq!(name_from_participants(&mut t, &people), 1);
        assert_eq!(t.speakers[0].name.as_deref(), Some("Shelley Morgan"));
        assert_eq!(t.speakers[0].source, Some(NameSource::Calendar));
        // three people: no guess
        e.attendees.push(person(None, "andy.smith@partner.example.org"));
        assert_eq!(invitees(&e)[1], "Andy Smith");
        let mut t = one_far_voice();
        let people = participants::Participants::gather(&me, Some(&e), &Default::default());
        assert_eq!(name_from_participants(&mut t, &people), 0);
    }

    #[test]
    fn one_to_one_from_the_meeting_app() {
        let app = timeline::Summary { roster: vec!["Riley Chen".into(), "Jordan Avery".into()], ..Default::default() };
        let mut t = one_far_voice();
        let people = participants::Participants::gather(&Me::default(), None, &app);
        assert_eq!(name_from_participants(&mut t, &people), 0, "don't know which tile is the user");
        let people = participants::Participants::gather(&Me::named("Jordan Avery"), None, &app);
        assert_eq!(name_from_participants(&mut t, &people), 1);
        assert_eq!(t.speakers[0].name.as_deref(), Some("Riley Chen"));
        assert_eq!(t.speakers[0].source, Some(NameSource::Accessibility));
    }

    #[test]
    fn ad_hoc_zoom_named_from_host() {
        // ad-hoc call: no calendar event, Zoom showed Shelley as host
        let app = timeline::Summary { host: Some("Shelley Morgan".into()), app: Some("zoom".into()), ..Default::default() };
        let me = Me::from_config(&crate::config::MeConfig { name: "Alex Rowan".into(), ..Default::default() });
        let mut t = one_far_voice();
        let people = participants::Participants::gather(&me, None, &app);
        assert_eq!(name_from_participants(&mut t, &people), 1);
        assert_eq!(t.speakers[0].name.as_deref(), Some("Shelley Morgan"));
        // the user hosting doesn't name anyone
        let app = timeline::Summary { host: Some("Alex Rowan".into()), ..Default::default() };
        let mut t = one_far_voice();
        assert_eq!(name_from_participants(&mut t, &participants::Participants::gather(&me, None, &app)), 0);
    }

    #[test]
    fn summary_hint_lists_names() {
        let mut t = call();
        rename(&mut t, Channel::System, Some(0), "Ann");
        let h = summary_hint(&t, Some("Alex Rowan"));
        assert!(h.contains("\"Me\" is Alex Rowan, the person who recorded it."), "{h}");
        assert!(h.contains("Other speakers: Ann."), "{h}");
        assert!(h.contains("couldn't be identified"), "Speaker 2 is unnamed: {h}");
        assert!(t.to_plain().starts_with("Ann [00:00]"));
        // with the recorder named, their label is what the model sees and uses
        assert!(name_recorder(&mut t, &Me::named("Alex Rowan")));
        apply_names(&mut t);
        let h = summary_hint(&t, Some("Alex Rowan"));
        assert!(h.contains("\"Alex\" is Alex Rowan, the person who recorded it."), "{h}");
        assert!(h.contains("owned by \"Alex\""), "{h}");
        assert!(t.to_plain().contains("\nAlex [00:11]"), "{}", t.to_plain());
    }

    #[test]
    fn voiceprints_name_known_voices() {
        let mut t = call();
        let mut vd = VoiceData::default();
        vd.centroids.push((Channel::System, Some(0), crate::voices::normalize(vec![1.0, 0.0, 0.0]), 19.0));
        vd.centroids.push((Channel::System, Some(1), crate::voices::normalize(vec![0.0, 1.0, 0.0]), 14.0));
        let mut store = VoiceStore::default();
        store.enroll("Alex", &[1.0, 0.05, 0.0], 60.0);
        store.enroll("Sam", &[0.0, 0.0, 1.0], 60.0);
        assert_eq!(name_from_voiceprints(&mut t, &vd, &store), 1);
        assert_eq!(t.speaker_info(Channel::System, Some(0)).unwrap().name.as_deref(), Some("Alex"));
        assert!(t.speaker_info(Channel::System, Some(1)).unwrap().name.is_none());
    }

    // ---- synthetic call scenarios (invented people, no transcript text)

    fn invite(owner: &str, organizer: Option<(&str, &str)>, people: &[(Option<&str>, &str, &str)]) -> crate::calendar::CalendarEvent {
        crate::calendar::CalendarEvent {
            id: "x".into(),
            uid: "x".into(),
            title: "call".into(),
            start: chrono::Utc::now(),
            end: chrono::Utc::now(),
            link: None,
            organizer: organizer.map(|(n, e)| Attendee { name: (!n.is_empty()).then(|| n.into()), email: e.into(), status: None }),
            attendees: people.iter().map(|(n, e, st)| Attendee { name: n.map(Into::into), email: (*e).into(), status: Some((*st).into()) }).collect(),
            description: None,
            calendar: Some(owner.into()),
            declined: false,
            recurring: false,
            html_link: None,
            calendar_name: None,
            color: None,
        }
    }

    fn two_far_voices(secs0: f64, secs1: f64) -> Transcript {
        let mut t = Transcript::from_channels(vec![seg(Channel::System, 0.0, secs0, Some(0)), seg(Channel::Mic, secs0 + 1.0, secs0 + 30.0, None), seg(Channel::System, secs0 + 31.0, secs0 + 31.0 + secs1, Some(1))]);
        sync_infos(&mut t);
        t
    }

    fn gather(ev: &crate::calendar::CalendarEvent) -> (Me, participants::Participants) {
        let me = Me::resolve(&Default::default(), Some(ev), None);
        let p = participants::Participants::gather(&me, Some(ev), &Default::default());
        (me, p)
    }

    /// Zoom call, invite with bare addresses (organizer shelley@, andy@, the
    /// user). Cues name Shelley; Andy is the one voice and invitee left.
    #[test]
    fn last_voice_is_the_last_invitee() {
        let ev = invite("alex@example.com", Some(("", "shelley@example.com")), &[(None, "shelley@example.com", "ACCEPTED"), (None, "alex@example.com", "ACCEPTED"), (None, "andy@partner.example.org", "ACCEPTED")]);
        let (me, people) = gather(&ev);
        assert_eq!(people.names(), vec!["Shelley", "Andy"]);
        let mut t = two_far_voices(520.0, 110.0);
        propose(&mut t.speakers[2], "Shelley", NameSource::Llm, 0.8);
        assert_eq!(link_to_invitees(&mut t, &people), 1);
        assert_eq!(t.speakers[2].email.as_deref(), Some("shelley@example.com"));
        assert_eq!(name_by_elimination(&mut t, &people), 1);
        let andy = t.speaker_info(Channel::System, Some(0)).unwrap();
        assert_eq!((andy.name.as_deref(), andy.source, andy.email.as_deref()), (Some("Andy"), Some(NameSource::Calendar), Some("andy@partner.example.org")));
        assert!(andy.confidence < 0.8, "medium confidence");
        // the recorder is "Alex" (from alex@example.com), not "Me"
        assert!(name_recorder(&mut t, &me));
        apply_names(&mut t);
        assert_eq!(t.segments.iter().map(|s| s.label()).collect::<Vec<_>>(), vec!["Andy", "Alex", "Shelley"]);
    }

    #[test]
    fn elimination_needs_exactly_one_of_each() {
        let ev = invite("me@x.com", None, &[(Some("Ann Lee"), "ann@x.com", "ACCEPTED"), (Some("Bo Chu"), "bo@x.com", "ACCEPTED"), (Some("Cy Dee"), "cy@x.com", "ACCEPTED")]);
        let (_, people) = gather(&ev);
        // three invitees, one named, one unnamed voice: two people left → no guess
        let mut t = two_far_voices(100.0, 100.0);
        propose(&mut t.speakers[0], "Ann", NameSource::Llm, 0.9);
        link_to_invitees(&mut t, &people);
        assert_eq!(name_by_elimination(&mut t, &people), 0);
        // a named voice that isn't on the invite might be the "missing" invitee under a nickname
        let ev = invite("me@x.com", None, &[(Some("Ann Lee"), "ann@x.com", "ACCEPTED"), (Some("Bo Chu"), "bo@x.com", "ACCEPTED")]);
        let (_, people) = gather(&ev);
        let mut t = two_far_voices(100.0, 100.0);
        propose(&mut t.speakers[0], "Zed", NameSource::Llm, 0.9);
        link_to_invitees(&mut t, &people);
        assert_eq!(name_by_elimination(&mut t, &people), 0);
        // a tiny voice doesn't count as a person
        let mut t = two_far_voices(100.0, 5.0);
        propose(&mut t.speakers[0], "Ann", NameSource::Llm, 0.9);
        link_to_invitees(&mut t, &people);
        assert_eq!(name_by_elimination(&mut t, &people), 0);
    }

    /// Teams call organised by "Sathiren Vale", also inviting "Shelley Morgan
    /// <shelley@…>" (no answer). People call him "Seth"; Shelley is greeted
    /// by name but her own voice has no cue that singles her out.
    #[test]
    fn nickname_links_to_invitee_and_frees_the_other() {
        let ev = invite(
            "alex@example.com",
            Some(("Sathiren Vale", "sathiren.vale@guest.example.org")),
            &[(None, "alex@example.com", "ACCEPTED"), (Some("Shelley Morgan <shelley@example.com>"), "shelley@example.com", "NEEDS-ACTION"), (Some("Sathiren Vale"), "sathiren.vale@guest.example.org", "ACCEPTED")],
        );
        let (me, people) = gather(&ev);
        let mut t = two_far_voices(300.0, 2000.0);
        propose(&mut t.speakers[2], "Seth", NameSource::Llm, 0.95);
        assert_eq!(link_to_invitees(&mut t, &people), 1);
        let seth = t.speaker_info(Channel::System, Some(1)).unwrap();
        assert_eq!((seth.name.as_deref(), seth.person.as_deref(), seth.email.as_deref()), (Some("Seth"), Some("Sathiren Vale"), Some("sathiren.vale@guest.example.org")));
        assert_eq!(name_by_elimination(&mut t, &people), 1);
        assert_eq!(t.speaker_info(Channel::System, Some(0)).unwrap().name.as_deref(), Some("Shelley Morgan"));
        name_recorder(&mut t, &me);
        apply_names(&mut t);
        let h = summary_hint(&t, me.display());
        assert!(h.contains("Seth (Sathiren Vale on the invite)"), "{h}");
        assert!(h.contains("\"Alex\" is the person who recorded it"), "{h}");
    }

    #[test]
    fn nickname_link_needs_an_unambiguous_invitee() {
        // "Sam" could be either Samantha or Samuel
        let ev = invite("me@x.com", None, &[(Some("Samantha Ruiz"), "sr@x.com", "ACCEPTED"), (Some("Samuel Ode"), "so@x.com", "ACCEPTED")]);
        let (_, people) = gather(&ev);
        let mut t = two_far_voices(100.0, 100.0);
        propose(&mut t.speakers[0], "Sam", NameSource::Llm, 0.9);
        assert_eq!(link_to_invitees(&mut t, &people), 0);
        // and when the invitee's own first name is said in the call, "Seth" is someone else
        let ev = invite("me@x.com", None, &[(Some("Sathiren Vale"), "sk@x.com", "ACCEPTED")]);
        let (_, people) = gather(&ev);
        let mut t = Transcript::from_channels(vec![seg(Channel::System, 0.0, 30.0, Some(0)), Segment::new(Channel::Mic, 31.0, 40.0, "Sathiren couldn't make it today")]);
        sync_infos(&mut t);
        propose(&mut t.speakers[0], "Seth", NameSource::Llm, 0.9);
        assert_eq!(link_to_invitees(&mut t, &people), 0);
    }

    #[test]
    fn recorder_label_can_stay_me() {
        let me = Me::from_config(&crate::config::MeConfig { name: "Alex Rowan".into(), label: "Me".into(), ..Default::default() });
        let mut t = call();
        assert!(!name_recorder(&mut t, &me));
        apply_names(&mut t);
        assert_eq!(t.segments[1].label(), "Me");
        assert_eq!(t.recorder_label(), "Me");
        // switching the label later renames, and back again
        let mut t = call();
        name_recorder(&mut t, &Me::named("Alex Rowan"));
        assert_eq!(t.recorder_label(), "Alex");
        name_recorder(&mut t, &me);
        assert_eq!(t.recorder_label(), "Me");
        assert!(!t.has_names(), "the recorder's own name isn't a far-end name");
        // a user rename of the mic wins
        let mut t = call();
        rename(&mut t, Channel::Mic, None, "Alexander");
        name_recorder(&mut t, &Me::named("Alex Rowan"));
        assert_eq!(t.recorder_label(), "Me", "a user-named mic isn't the automatic recorder label");
        apply_names(&mut t);
        assert_eq!(t.segments[1].label(), "Alexander");
    }
}
