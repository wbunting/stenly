//! Speaker identification step of the post-recording pipeline, and renames.

use super::participants::Participants;
use super::{VoiceData, apply_names, link_to_invitees, name_by_elimination, name_from_participants, name_from_timeline, name_from_voiceprints, name_recorder, refine_with_timeline, sync_infos, timeline};
use crate::audio::read_wav_16k_mono;
use crate::config::Config;
use crate::identity::Me;
use crate::platform::Channel;
use crate::session::{SessionMeta, wav_path};
use crate::transcript::{NameSource, Segment, SpeakerInfo, Transcriber, Transcript};
use crate::voices::{self, SELF_NAME, VoiceStore};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Per-session voice centroids (so a rename can update voiceprints without
/// re-reading audio). Never contains audio.
pub const SESSION_VOICES: &str = "voiceprints.json";
/// Far-end speech needed before trying to tell remote voices apart.
const MIN_FAR_SECS: f64 = 30.0;
/// Speech needed before a voice is saved as someone's voiceprint.
const MIN_ENROLL_SECS: f64 = 20.0;
/// Embeddings per voice when computing centroids of an existing transcript.
const MAX_EMBED_PER_VOICE: usize = 40;
/// A name from the conversation (cues or the model) is remembered as a
/// voiceprint only at this confidence, and only when it is someone on the
/// invite or matches the voice already remembered under that name.
const ENROLL_TRANSCRIPT_CONFIDENCE: f32 = 0.8;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredVoice {
    channel: Channel,
    speaker: Option<u32>,
    centroid: Vec<f32>,
    secs: f64,
}

fn save_voice_data(dir: &Path, vd: &VoiceData) {
    let v: Vec<StoredVoice> = vd.centroids.iter().map(|(c, s, v, t)| StoredVoice { channel: *c, speaker: *s, centroid: v.clone(), secs: *t }).collect();
    let _ = std::fs::write(dir.join(SESSION_VOICES), serde_json::to_vec(&v).unwrap_or_default());
}

fn load_voice_data(dir: &Path) -> Option<VoiceData> {
    let v: Vec<StoredVoice> = serde_json::from_slice(&std::fs::read(dir.join(SESSION_VOICES)).ok()?).ok()?;
    Some(VoiceData { centroids: v.into_iter().map(|s| (s.channel, s.speaker, s.centroid, s.secs)).collect() })
}

/// How many remote people there were, when the meeting app or the invite
/// can say (and we know which of them is the user).
fn expected_far_people(people: &Participants, me: &Me) -> Option<usize> {
    if !me.is_known() || people.is_empty() {
        return None;
    }
    Some(people.len())
}

/// Split the far end of a call into voices. `raw` = unmerged segments of the
/// System channel. Returns the voices found (centroids per speaker number,
/// or a single `None` voice).
fn split_far_end(
    raw: &mut [Segment],
    embs: &[Option<Vec<f32>>],
    events: &[timeline::Event],
    expected: Option<usize>,
    me: &Me,
    store: &VoiceStore,
) -> VoiceData {
    let mut vd = VoiceData::default();
    let total: f64 = raw.iter().map(|s| s.end - s.start).sum();
    let all = voices::weighted_centroid(raw.iter().zip(embs).filter_map(|(s, e)| e.as_deref().map(|e| (e, s.end - s.start))));
    let single = |vd: &mut VoiceData, raw: &mut [Segment]| {
        raw.iter_mut().for_each(|s| s.speaker = None);
        if let Some(c) = all.clone() {
            vd.centroids.push((Channel::System, None, c, total));
        }
    };
    if expected == Some(1) {
        single(&mut vd, raw);
        return vd;
    }
    // Known voices: when segments clearly match two or more people the user
    // named before, label by voiceprint (works for short calls too).
    if let Some(known) = split_by_voiceprints(raw, embs, store) {
        return known;
    }
    if total < MIN_FAR_SECS {
        single(&mut vd, raw);
        return vd;
    }
    let c = voices::cluster(raw, embs);
    if c.count() >= 2 {
        for (s, l) in raw.iter_mut().zip(&c.labels) {
            s.speaker = *l;
        }
        vd.add_clusters(Channel::System, &c);
        return vd;
    }
    // One cluster, but the meeting app showed several people talking: seed
    // voices from the highlight and assign the rest by nearest voice.
    if let Some(seeded) = seed_from_timeline(raw, embs, events, me) {
        return seeded;
    }
    single(&mut vd, raw);
    vd
}

/// Cut long segments where the voice clearly changes. Returns how many were cut.
/// Cheap first: short windows of every long segment are embedded in one
/// batch; only segments whose voice drifts are re-decoded for word times.
fn split_speaker_changes(segs: &mut Vec<Segment>, audio: &[f32], transcriber: &mut dyn Transcriber) -> usize {
    let long: Vec<usize> = (0..segs.len()).filter(|&i| segs[i].end - segs[i].start >= voices::SPLIT_MIN_SECS).collect();
    if long.is_empty() {
        return 0;
    }
    let wins: Vec<Vec<(f64, f64)>> = long.iter().map(|&i| voices::screen_windows(&segs[i])).collect();
    let flat: Vec<Segment> = long.iter().zip(&wins).flat_map(|(&i, w)| w.iter().map(move |(a, b)| (i, *a, *b))).map(|(i, a, b)| Segment::new(segs[i].channel, a, b, "")).collect();
    let embs = transcriber.embed_segments(&flat, audio).unwrap_or_default();
    if embs.len() != flat.len() {
        return 0;
    }
    let mut k = 0;
    let mut suspects: Vec<usize> = Vec::new();
    for (&i, w) in long.iter().zip(&wins) {
        if voices::voice_drifts(&embs[k..k + w.len()]) {
            suspects.push(i);
        }
        k += w.len();
    }
    tracing::debug!(long = long.len(), suspects = suspects.len(), "segments that may hold two voices");
    // word times and candidate cuts for the suspects, then both sides of every cut in one batch
    let mut plans: Vec<(usize, Vec<crate::transcript::TimedWord>, Vec<voices::Cut>)> = Vec::new();
    for i in suspects {
        let words = transcriber.words(&segs[i], audio);
        let cands = voices::split_candidates(&segs[i], &words);
        if !cands.is_empty() {
            plans.push((i, words, cands));
        }
    }
    let sides: Vec<Segment> = plans.iter().flat_map(|(i, _, c)| {
        let ch = segs[*i].channel;
        c.iter().flat_map(move |(_, l, r)| [Segment::new(ch, l.0, l.1, ""), Segment::new(ch, r.0, r.1, "")])
    }).collect();
    let side_embs = transcriber.embed_segments(&sides, audio).unwrap_or_default();
    if side_embs.len() != sides.len() {
        return 0;
    }
    let mut k = 0;
    let mut cuts: Vec<(usize, Segment, Segment)> = Vec::new();
    for (i, words, cands) in &plans {
        let dists: Vec<Option<f32>> = (0..cands.len())
            .map(|j| match (&side_embs[k + 2 * j], &side_embs[k + 2 * j + 1]) {
                (Some(a), Some(b)) => Some(1.0 - voices::dot(a, b)),
                _ => None,
            })
            .collect();
        k += 2 * cands.len();
        if let Some((a, b)) = voices::best_split(cands, &dists).and_then(|t| voices::split_at(&segs[*i], words, t)) {
            cuts.push((*i, a, b));
        }
    }
    let n = cuts.len();
    for (i, a, b) in cuts.into_iter().rev() {
        segs.splice(i..=i, [a, b]);
    }
    n
}

/// Silence between two segments (0 when they overlap).
fn gap(a: &Segment, b: &Segment) -> f64 {
    (b.start - a.end).max(a.start - b.end).max(0.0)
}

/// Label each segment with the stored voice it matches; succeeds when 2+
/// different people cover most of the far end.
fn split_by_voiceprints(raw: &mut [Segment], embs: &[Option<Vec<f32>>], store: &VoiceStore) -> Option<VoiceData> {
    if store.people().count() < 2 {
        return None;
    }
    let total: f64 = raw.iter().map(|s| s.end - s.start).sum();
    let mut who: Vec<Option<String>> = vec![None; raw.len()];
    let mut covered = 0.0;
    for (i, e) in embs.iter().enumerate() {
        // single segments are noisier than centroids: a slightly lower bar, still with a margin
        if let Some((v, _)) = e.as_deref().and_then(|e| store.best_match_with(e, voices::MATCH_SIM - 0.1)) {
            if v.name != SELF_NAME {
                who[i] = Some(v.name.clone());
                covered += raw[i].end - raw[i].start;
            }
        }
    }
    let mut names: Vec<String> = Vec::new();
    for n in who.iter().flatten() {
        if !names.contains(n) {
            names.push(n.clone());
        }
    }
    if names.len() < 2 || covered < total * 0.6 {
        return None;
    }
    // unmatched segments: nearest matched neighbour in time
    let matched: Vec<usize> = (0..raw.len()).filter(|&i| who[i].is_some()).collect();
    for i in 0..raw.len() {
        if who[i].is_none() {
            if let Some(&j) = matched.iter().min_by(|&&a, &&b| gap(&raw[a], &raw[i]).total_cmp(&gap(&raw[b], &raw[i]))) {
                who[i] = who[j].clone();
            }
        }
    }
    let mut vd = VoiceData::default();
    for (k, n) in names.iter().enumerate() {
        let pick = || raw.iter().zip(embs).zip(&who).filter(|(_, w)| w.as_deref() == Some(n.as_str()));
        let secs = pick().map(|((s, _), _)| s.end - s.start).sum();
        if let Some(c) = voices::weighted_centroid(pick().filter_map(|((s, e), _)| e.as_deref().map(|e| (e, s.end - s.start)))) {
            vd.centroids.push((Channel::System, Some(k as u32), c, secs));
        }
    }
    for (s, w) in raw.iter_mut().zip(&who) {
        s.speaker = w.as_ref().and_then(|w| names.iter().position(|n| n == w)).map(|k| k as u32);
    }
    tracing::info!(voices = names.len(), "far-end voices told apart by remembered voices");
    Some(vd)
}

/// Label segments by the name highlighted during most of them, then give
/// the unlabelled ones the nearest labelled voice.
fn seed_from_timeline(raw: &mut [Segment], embs: &[Option<Vec<f32>>], events: &[timeline::Event], me: &Me) -> Option<VoiceData> {
    if events.is_empty() {
        return None;
    }
    let total: f64 = raw.iter().map(|s| s.end - s.start).sum();
    let mut names: Vec<String> = Vec::new();
    let mut lab: Vec<Option<usize>> = vec![None; raw.len()];
    for (i, s) in raw.iter().enumerate() {
        let dur = s.end - s.start;
        let ov = timeline::overlap(events, s.start, s.end);
        let Some((n, secs)) = ov.into_iter().find(|(n, _)| !me.is(n)) else { continue };
        if secs < dur * 0.5 {
            continue;
        }
        let k = names.iter().position(|x| *x == n).unwrap_or_else(|| {
            names.push(n);
            names.len() - 1
        });
        lab[i] = Some(k);
    }
    // each name must account for a real share of the far-end talk
    let talk = |k: usize| raw.iter().zip(&lab).filter(|(_, l)| **l == Some(k)).map(|(s, _)| s.end - s.start).sum::<f64>();
    let keep: Vec<usize> = (0..names.len()).filter(|&k| talk(k) >= (total * 0.1).max(5.0)).collect();
    if keep.len() < 2 {
        return None;
    }
    let cents: Vec<Option<Vec<f32>>> = keep
        .iter()
        .map(|&k| voices::weighted_centroid(raw.iter().zip(embs).zip(&lab).filter(|(_, l)| **l == Some(k)).filter_map(|((s, e), _)| e.as_deref().map(|e| (e, s.end - s.start)))))
        .collect();
    let mut out_lab: Vec<Option<u32>> = vec![None; raw.len()];
    for i in 0..raw.len() {
        out_lab[i] = match lab[i].and_then(|k| keep.iter().position(|&x| x == k)) {
            Some(j) => Some(j as u32),
            None => embs[i].as_ref().and_then(|e| {
                (0..keep.len()).filter(|&j| cents[j].is_some()).max_by(|&a, &b| voices::dot(e, cents[a].as_ref().unwrap()).total_cmp(&voices::dot(e, cents[b].as_ref().unwrap()))).map(|j| j as u32)
            }),
        };
    }
    // the rest (too short to embed): nearest labelled neighbour in time
    let labelled: Vec<usize> = (0..raw.len()).filter(|&i| out_lab[i].is_some()).collect();
    for i in 0..raw.len() {
        if out_lab[i].is_none() {
            if let Some(&j) = labelled.iter().min_by(|&&a, &&b| (raw[a].start - raw[i].start).abs().total_cmp(&(raw[b].start - raw[i].start).abs())) {
                out_lab[i] = out_lab[j];
            }
        }
    }
    // renumber by first appearance
    let mut order: Vec<u32> = Vec::new();
    for l in out_lab.iter().flatten() {
        if !order.contains(l) {
            order.push(*l);
        }
    }
    let mut vd = VoiceData::default();
    for (s, l) in raw.iter_mut().zip(&out_lab) {
        s.speaker = l.and_then(|l| order.iter().position(|&x| x == l)).map(|p| p as u32);
    }
    for (new, old) in order.iter().enumerate() {
        if let Some(c) = &cents[*old as usize] {
            let secs = raw.iter().filter(|s| s.speaker == Some(new as u32)).map(|s| s.end - s.start).sum();
            vd.centroids.push((Channel::System, Some(new as u32), c.clone(), secs));
        }
    }
    tracing::info!(voices = order.len(), "far-end voices seeded from the meeting app's speaker highlight");
    Some(vd)
}

/// Centroids for the voices already in a transcript (after a separate split, or on re-runs).
fn voice_data_from_transcript(t: &Transcript, dir: &Path, transcriber: &mut dyn Transcriber, skip: &VoiceData) -> VoiceData {
    let mut vd = skip.clone();
    for ch in [Channel::Mic, Channel::System] {
        let voices: Vec<Option<u32>> = {
            let mut v: Vec<Option<u32>> = Vec::new();
            for s in t.segments.iter().filter(|s| s.channel == ch) {
                if !v.contains(&s.speaker) && vd.get(ch, s.speaker).is_none() {
                    v.push(s.speaker);
                }
            }
            v
        };
        if voices.is_empty() {
            continue;
        }
        let Ok(audio) = read_wav_16k_mono(&wav_path(dir, ch)) else { continue };
        for sp in voices {
            let mut segs: Vec<Segment> = t.segments.iter().filter(|s| s.channel == ch && s.speaker == sp && s.end - s.start >= voices::MIN_EMBED_SECS).cloned().collect();
            segs.sort_by(|a, b| (b.end - b.start).total_cmp(&(a.end - a.start)));
            segs.truncate(MAX_EMBED_PER_VOICE);
            let secs: f64 = t.segments.iter().filter(|s| s.channel == ch && s.speaker == sp).map(|s| s.end - s.start).sum();
            let Ok(embs) = transcriber.embed_segments(&segs, &audio) else { continue };
            if let Some(c) = voices::weighted_centroid(segs.iter().zip(&embs).filter_map(|(s, e)| e.as_deref().map(|e| (e, s.end - s.start)))) {
                vd.centroids.push((ch, sp, c, secs));
            }
        }
    }
    vd
}

/// Replace the far-end segments of `t` with `far` (raw, labelled) and re-merge.
fn rebuild_far(vocab: &crate::vocabulary::Vocabulary, t: &mut Transcript, far: Vec<Segment>) {
    let mut tmp = Transcript { segments: far, ..Default::default() };
    vocab.apply(&mut tmp);
    let mut all: Vec<Segment> = t.segments.iter().filter(|s| s.channel != Channel::System).cloned().collect();
    all.extend(tmp.segments);
    let (in_person, speakers) = (t.in_person, std::mem::take(&mut t.speakers));
    *t = Transcript::from_channels(all);
    t.in_person = in_person;
    t.speakers = speakers;
}

/// Name the voices in `t`. `raw` = unmerged ASR segments (both channels) when available.
pub fn identify(cfg: &Config, dir: &Path, meta: &SessionMeta, t: &mut Transcript, raw: Option<Vec<Segment>>, transcriber: &mut dyn Transcriber) -> Result<()> {
    let first = t.speakers.is_empty();
    let events = timeline::load(dir);
    let app = timeline::summarize(&events);
    let mut me = Me::resolve(&cfg.me, meta.event.as_ref(), app.me.as_deref());
    me.locate(&app.roster);
    let people = Participants::gather(&me, meta.event.as_ref(), &app);
    tracing::info!(
        me = ?me.display(),
        app = ?app.app,
        host = ?app.host,
        people = ?people.names(),
        "who was in the meeting"
    );
    let mut vd = load_voice_data(dir).unwrap_or_default();
    let vocab = crate::vocabulary::Vocabulary::for_session_dir(cfg, meta, dir);

    // 1. calls: split "Them" into voices (once; later runs keep the numbering
    //    so renames stay attached)
    let t0 = std::time::Instant::now();
    let far_unsplit = !t.in_person && t.segments.iter().any(|s| s.channel == Channel::System) && t.segments.iter().all(|s| s.channel != Channel::System || s.speaker.is_none());
    if first && far_unsplit {
        let mut far: Vec<Segment> = match &raw {
            Some(r) => r.iter().filter(|s| s.channel == Channel::System && !s.text.trim().is_empty()).cloned().collect(),
            None => t.segments.iter().filter(|s| s.channel == Channel::System).cloned().collect(),
        };
        far.sort_by(|a, b| a.start.total_cmp(&b.start));
        if let Ok(audio) = read_wav_16k_mono(&wav_path(dir, Channel::System)) {
            // a long segment can hold two people ("Okay, makes sense. | Can I
            // ask…"): cut it where the voice changes before telling voices apart
            if !(expected_far_people(&people, &me) == Some(1)) {
                let n = split_speaker_changes(&mut far, &audio, transcriber);
                if n > 0 {
                    tracing::info!(split = n, secs = t0.elapsed().as_secs_f32(), "split segments at speaker changes");
                }
            }
            let embs = transcriber.embed_segments(&far, &audio).unwrap_or_default();
            if embs.len() == far.len() && !embs.is_empty() {
                let expected = expected_far_people(&people, &me);
                let far_vd = split_far_end(&mut far, &embs, &events, expected, &me, &VoiceStore::load().excluding_session(&session_id(dir)));
                let voices = far_vd.centroids.len();
                vd.centroids.retain(|(c, _, _, _)| *c != Channel::System);
                vd.centroids.extend(far_vd.centroids);
                if far.iter().any(|s| s.speaker.is_some()) {
                    tracing::info!(voices, expected = ?expected, secs = t0.elapsed().as_secs_f32(), "told remote voices apart");
                    rebuild_far(&vocab, t, far.clone());
                    // Voices are named by the highlight; segments the highlight
                    // clearly gives to someone else move to that person's voice.
                    if !events.is_empty() {
                        sync_infos(t);
                        name_from_timeline(t, &events, &me);
                        let moved = refine_with_timeline(&mut far, &t.speakers, &events);
                        if moved > 0 {
                            tracing::info!(moved, "moved segments to the voice the meeting app highlighted");
                            // centroids from the embeddings we already have
                            vd.centroids.retain(|(c, _, _, _)| *c != Channel::System);
                            let mut ids: Vec<Option<u32>> = far.iter().map(|s| s.speaker).collect();
                            ids.dedup();
                            ids.sort();
                            ids.dedup();
                            for sp in ids {
                                let pick = || far.iter().zip(&embs).filter(move |(s, _)| s.speaker == sp);
                                let secs = pick().map(|(s, _)| s.end - s.start).sum();
                                if let Some(c) = voices::weighted_centroid(pick().filter_map(|(s, e)| e.as_deref().map(|e| (e, s.end - s.start)))) {
                                    vd.centroids.push((Channel::System, sp, c, secs));
                                }
                            }
                            rebuild_far(&vocab, t, far);
                        }
                    }
                }
            }
        }
    }
    sync_infos(t);

    // 2. centroids for every voice (self-enrollment, voiceprint matching)
    vd = voice_data_from_transcript(t, dir, transcriber, &vd);
    tracing::debug!(secs = t0.elapsed().as_secs_f32(), "voice centroids");
    vd.centroids.retain(|(c, s, _, _)| t.speakers.iter().any(|i| i.channel == *c && i.speaker == *s));
    save_voice_data(dir, &vd);

    // 3. names
    let mut store = VoiceStore::load();
    let ax = name_from_timeline(t, &events, &me);
    let vp = name_from_voiceprints(t, &vd, &store.excluding_session(&session_id(dir)));
    let cal = name_from_participants(t, &people);
    let mut cues = super::llm::name_from_cues(t, &people.names(), &me);
    let mut llm = 0;
    let unnamed_far = t.speakers.iter().any(|i| i.name.is_none() && (t.in_person || i.channel == Channel::System) && i.talk_secs >= 5.0);
    if unnamed_far && cfg.llm.enabled && t.word_count() >= 50 {
        let target = cfg.llm.target(crate::secrets::llm_api_key);
        match super::llm::suggest(&target, t, meta.event.as_ref(), &people, &me, cfg.llm.timeout_secs.min(180)) {
            Ok(s) => {
                tracing::debug!(suggestions = ?s, "speaker name suggestions");
                llm = super::llm::apply(t, &s, &me);
            }
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "speaker name suggestions failed"),
        }
    }
    // spoken names → the invite ("Seth" is Sathiren Vale), then the last
    // voice for the last invitee, then cues again for what's left
    link_to_invitees(t, &people);
    let elim = name_by_elimination(t, &people);
    if elim > 0 {
        cues += super::llm::name_from_cues(t, &people.names(), &me);
        link_to_invitees(t, &people);
    }
    name_recorder(t, &me);
    apply_names(t);
    let _ = (ax, vp, cal, elim);
    let by = |src: NameSource| t.speakers.iter().filter(|i| i.source == Some(src)).count();
    tracing::info!(
        accessibility = by(NameSource::Accessibility),
        voiceprint = by(NameSource::Voiceprint),
        calendar = by(NameSource::Calendar),
        cues,
        llm,
        user = by(NameSource::User),
        voices = t.speakers.len(),
        "named speakers"
    );

    // 4. remember voices: the user's own mic on calls, and people named by
    //    strong evidence (see [`enroll_reason`])
    if remember_voices(t, &vd, &mut store, &session_id(dir)) {
        if let Err(e) = store.save() {
            tracing::warn!(error = %e, "saving voiceprints failed");
        }
    }
    Ok(())
}

/// Save this meeting's voices that are solid enough into the store, and
/// take back what a re-run no longer supports. Returns whether it changed.
pub fn remember_voices(t: &Transcript, vd: &VoiceData, store: &mut VoiceStore, session: &str) -> bool {
    let mut changed = false;
    // "consistent with an earlier meeting" means another meeting, not this one's earlier run
    let others = store.excluding_session(session);
    for info in &t.speakers {
        let Some((c, secs)) = vd.get(info.channel, info.speaker) else { continue };
        if secs < MIN_ENROLL_SECS {
            continue;
        }
        let key = voices::sample_key(session, info.channel, info.speaker);
        if info.channel == Channel::Mic && !t.in_person && info.speaker.is_none() {
            store.enroll_sample(SELF_NAME, None, &key, c, secs, "self");
            changed = true;
        } else if let Some(why) = enroll_reason(info, c, &others) {
            // the invite's fuller form of the same name ("Shelley" → "Shelley
            // Morgan"); a nickname stays what people call them ("Seth")
            let label = info.name.as_deref().unwrap_or_default();
            let name = info.person.as_deref().filter(|p| super::participants::same_person(label, p)).unwrap_or(label);
            let stored = store.enroll_sample(name, info.email.as_deref(), &key, c, secs, why);
            tracing::info!(name, stored = ?stored, why, secs = secs as u32, "remembered a voice");
            changed = true;
        } else if let Some(why) = pending_reason(info) {
            // the invite's name for this voice, not proven: remembered only if another meeting agrees
            let name = info.person.as_deref().unwrap_or(info.name.as_deref().unwrap_or_default());
            if store.enrolled_as(&key).is_some_and(|v| !super::participants::same_person(&v.name, name)) {
                store.unenroll(&key);
            }
            if let Some(stored) = store.corroborate(name, info.email.as_deref(), &key, c, secs, why) {
                tracing::info!(name, stored, why, secs = secs as u32, "remembered a voice another meeting agrees on");
            }
            changed = true;
        } else if store.enrolled_as(&key).is_some_and(|v| {
            // a re-run named this voice differently (or not at all): take back what it taught before
            info.name.as_deref().is_none_or(|n| !super::participants::same_person(&v.name, n) && info.person.as_deref().is_none_or(|p| !super::participants::same_person(&v.name, p)))
        }) {
            store.unenroll(&key);
            changed = true;
        }
    }
    changed
}

/// A name worth holding until another meeting agrees: names from the invite
/// (one-to-one, last one left) and decent names from the conversation that
/// match someone on the invite.
fn pending_reason(info: &SpeakerInfo) -> Option<&'static str> {
    if info.name.as_deref().is_none_or(|n| n == SELF_NAME) || info.email.is_none() {
        return None;
    }
    match info.source? {
        NameSource::Calendar => Some("invite"),
        NameSource::Llm if info.confidence >= super::LLM_MIN_CONFIDENCE => Some("transcript"),
        _ => None,
    }
}

/// The session's folder name: identifies its voices in the voiceprint store.
pub fn session_id(dir: &Path) -> String {
    dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.display().to_string())
}

/// Whether a named voice is solid enough to remember, and why. Conservative:
/// a wrong voiceprint names the wrong person in later meetings. Every saved
/// voice can be taken back ([`rename_in_session`], `speakers forget`).
fn enroll_reason(info: &SpeakerInfo, c: &[f32], store: &VoiceStore) -> Option<&'static str> {
    let name = info.name.as_deref()?;
    if name == SELF_NAME || info.source == Some(NameSource::Recorder) {
        return None;
    }
    // the same name already has this voice: a consistent second meeting
    let consistent = || {
        store.voices.iter().any(|v| {
            (super::participants::same_person(&v.name, name) || info.person.as_deref().is_some_and(|p| super::participants::same_person(&v.name, p)) || info.email.is_some() && v.email == info.email)
                && v.centroid.len() == c.len()
                && voices::dot(&v.centroid, c) >= voices::MATCH_SIM
        })
    };
    match info.source? {
        NameSource::User => Some("user"),
        NameSource::Accessibility if info.confidence >= 0.6 => Some("accessibility"),
        // a confident name from the conversation that is someone on the invite
        NameSource::Llm if info.confidence >= ENROLL_TRANSCRIPT_CONFIDENCE && (info.email.is_some() || consistent()) => Some("transcript"),
        // invite-only names (one-to-one, last one left) when the voice agrees with an earlier meeting
        NameSource::Calendar if consistent() => Some("invite"),
        NameSource::Voiceprint if info.confidence >= voices::MATCH_SIM + 0.1 => Some("voiceprint"),
        _ => None,
    }
}

/// Rename one voice in a finished session: transcript, summary, note and the
/// voiceprint store. Doesn't call the model.
pub fn rename_in_session(cfg: &Config, dir: &Path, channel: Channel, speaker: Option<u32>, name: &str) -> Result<()> {
    let tf = dir.join("transcript.json");
    let mut t: Transcript = serde_json::from_slice(&std::fs::read(&tf).context("reading transcript")?)?;
    let old = super::rename(&mut t, channel, speaker, name).context("no such speaker")?;
    std::fs::write(&tf, serde_json::to_vec_pretty(&t)?)?;
    let new = t.speaker_info(channel, speaker).and_then(|i| i.name.clone()).unwrap_or_else(|| crate::transcript::unnamed_label(channel, speaker));

    // the summary was written with the old label
    let sf = dir.join("summary.json");
    let mut summary: Option<crate::summary::Summary> = std::fs::read(&sf).ok().and_then(|b| serde_json::from_slice(&b).ok());
    if let Some(s) = summary.as_mut() {
        if old != new && !matches!(old.as_str(), "Me" | "Them" | "You") {
            rename_in_summary(s, &old, &new);
            std::fs::write(&sf, serde_json::to_vec_pretty(s)?)?;
        }
    }

    rewrite_note(cfg, dir, &t, summary.as_ref())?;

    // remember the voice for next time, replacing whatever this voice was
    // remembered as before (a wrong guess the user just corrected)
    if cfg.asr.speaker_names {
        let mut store = VoiceStore::load();
        if remember_rename(&mut store, dir, &t, channel, speaker, name) {
            store.save()?;
        }
    }
    Ok(())
}

/// A user rename in the voiceprint store: this voice leaves whatever it was
/// saved as (a wrong guess) and, with a name, becomes that person's.
fn remember_rename(store: &mut VoiceStore, dir: &Path, t: &Transcript, channel: Channel, speaker: Option<u32>, name: &str) -> bool {
    let key = voices::sample_key(&session_id(dir), channel, speaker);
    // the recorder's own voice keeps its voiceprint ("Me"); a label for it is not another person
    let recorder = channel == Channel::Mic && speaker.is_none() && !t.in_person || store.enrolled_as(&key).is_some_and(|v| v.name == SELF_NAME);
    if recorder {
        return false;
    }
    let mut changed = store.unenroll(&key).is_some();
    if !name.trim().is_empty() {
        if let Some((c, secs)) = load_voice_data(dir).as_ref().and_then(|vd| vd.get(channel, speaker).map(|(c, s)| (c.to_vec(), s))) {
            store.enroll_sample(name.trim(), None, &key, &c, secs.max(MIN_ENROLL_SECS), "user");
            changed = true;
        }
    }
    changed
}

/// Re-render a finished session's note in place from `t` and `summary`
/// (the whole file is rewritten; saved recipe results are put back).
pub fn rewrite_note(cfg: &Config, dir: &Path, t: &Transcript, summary: Option<&crate::summary::Summary>) -> Result<bool> {
    let meta = SessionMeta::load(dir)?;
    let Some(p) = meta.note_path.as_ref().filter(|p| p.exists()) else { return Ok(false) };
    let keep_audio = cfg.output.keep_audio_days != 0;
    let md = crate::note::render(&meta, t, summary, keep_audio.then_some(dir), cfg.output.include_transcript);
    crate::note::write_note(p, &md)?;
    // recipe results the user added to the note
    crate::recipe::reapply_saved(dir)?;
    Ok(true)
}

/// After naming a finished session again: the summary's "Me"/"You" action
/// items go to the recorder's label, then the note is re-rendered.
pub fn refresh_session_outputs(cfg: &Config, dir: &Path, t: &Transcript) -> Result<bool> {
    let sf = dir.join("summary.json");
    let mut summary: Option<crate::summary::Summary> = std::fs::read(&sf).ok().and_then(|b| serde_json::from_slice(&b).ok());
    if let Some(s) = summary.as_mut() {
        let before = serde_json::to_vec(s)?;
        crate::summary::own_action_items(s, &t.recorder_label());
        if serde_json::to_vec(s)? != before {
            std::fs::write(&sf, serde_json::to_vec_pretty(s)?)?;
        }
    }
    rewrite_note(cfg, dir, t, summary.as_ref())
}

/// Replace a whole-word speaker label in the summary text.
pub fn rename_in_summary(s: &mut crate::summary::Summary, old: &str, new: &str) {
    let re = regex::Regex::new(&format!(r"\b{}\b", regex::escape(old))).expect("valid regex");
    let fix = |x: &mut String| {
        if x.contains(old) {
            *x = re.replace_all(x, regex::NoExpand(new)).into_owned();
        }
    };
    fix(&mut s.summary);
    for sec in s.sections.iter_mut() {
        for b in sec.bullets.iter_mut() {
            match b {
                crate::summary::Bullet::Plain(t) => fix(t),
                crate::summary::Bullet::Nested { text, sub } => {
                    fix(text);
                    sub.iter_mut().for_each(&fix);
                }
            }
        }
    }
    s.key_points.iter_mut().for_each(&fix);
    s.decisions.iter_mut().for_each(&fix);
    for a in s.action_items.iter_mut() {
        fix(&mut a.task);
        if let Some(o) = a.owner.as_mut() {
            fix(o);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(s: f64, e: f64) -> Segment {
        Segment::new(Channel::System, s, e, "words")
    }
    fn e(a: f32, b: f32) -> Option<Vec<f32>> {
        Some(voices::normalize(vec![a, b, 0.0]))
    }
    fn ev(t: f64, active: &[&str]) -> timeline::Event {
        timeline::Event { t, app: "teams".into(), active: active.iter().map(|s| s.to_string()).collect(), roster: vec!["Ann".into(), "Bo".into(), "Me Myself".into()], ..Default::default() }
    }

    #[test]
    fn far_end_splits_two_clear_voices() {
        let mut raw = vec![seg(0.0, 20.0), seg(21.0, 40.0), seg(41.0, 60.0), seg(61.0, 80.0)];
        let embs = vec![e(1.0, 0.0), e(0.0, 1.0), e(1.0, 0.1), e(0.1, 1.0)];
        let vd = split_far_end(&mut raw, &embs, &[], None, &Me::default(), &VoiceStore::default());
        assert_eq!(vd.centroids.len(), 2);
        assert_eq!(raw.iter().map(|s| s.speaker).collect::<Vec<_>>(), vec![Some(0), Some(1), Some(0), Some(1)]);
    }

    #[test]
    fn one_expected_person_stays_one_voice() {
        let mut raw = vec![seg(0.0, 20.0), seg(21.0, 40.0)];
        let embs = vec![e(1.0, 0.0), e(0.0, 1.0)];
        let vd = split_far_end(&mut raw, &embs, &[], Some(1), &Me::default(), &VoiceStore::default());
        assert_eq!(vd.centroids.len(), 1);
        assert!(raw.iter().all(|s| s.speaker.is_none()));
    }

    #[test]
    fn similar_voices_are_seeded_from_the_highlight() {
        // codec-flattened voices that clustering can't tell apart…
        let mut raw = vec![seg(0.0, 10.0), seg(11.0, 20.0), seg(21.0, 30.0), seg(31.0, 40.0), seg(41.0, 42.0)];
        let embs = vec![e(1.0, 0.30), e(1.0, 0.20), e(1.0, 0.31), e(1.0, 0.19), None];
        // …but the app lit Ann, Bo, (nothing), Bo
        let evs = vec![ev(0.0, &["Ann"]), ev(10.0, &[]), ev(11.0, &["Bo"]), ev(20.0, &[]), ev(31.0, &["Bo"]), ev(40.5, &[])];
        let vd = split_far_end(&mut raw, &embs, &evs, Some(2), &Me::named("Me Myself"), &VoiceStore::default());
        assert_eq!(vd.centroids.len(), 2);
        // segment 3 had no highlight: nearest voice is Ann's; the 1 s one inherits its neighbour
        assert_eq!(raw.iter().map(|s| s.speaker).collect::<Vec<_>>(), vec![Some(0), Some(1), Some(0), Some(1), Some(1)]);
    }

    #[test]
    fn short_call_split_by_remembered_voices() {
        let mut store = VoiceStore::default();
        store.enroll("Ann", &[1.0, 0.0, 0.0], 60.0);
        store.enroll("Bo", &[0.0, 1.0, 0.0], 60.0);
        // 12 s of far-end speech: too little to cluster, enough to match
        let mut raw = vec![seg(0.0, 4.0), seg(5.0, 9.0), seg(9.2, 9.7), seg(11.0, 15.0)];
        let embs = vec![e(1.0, 0.1), e(0.1, 1.0), None, e(1.0, 0.0)];
        let vd = split_far_end(&mut raw, &embs, &[], None, &Me::default(), &store);
        assert_eq!(vd.centroids.len(), 2);
        assert_eq!(raw.iter().map(|s| s.speaker).collect::<Vec<_>>(), vec![Some(0), Some(1), Some(1), Some(0)]);
    }

    #[test]
    fn summary_labels_follow_renames() {
        let mut s = crate::summary::Summary {
            summary: "Speaker 2 proposed a pilot; Speaker 21 disagreed.".into(),
            action_items: vec![crate::summary::ActionItem { owner: Some("Speaker 2".into()), task: "Send Speaker 2's deck".into() }],
            ..Default::default()
        };
        rename_in_summary(&mut s, "Speaker 2", "Alex");
        assert_eq!(s.summary, "Alex proposed a pilot; Speaker 21 disagreed.");
        assert_eq!(s.action_items[0].owner.as_deref(), Some("Alex"));
        assert_eq!(s.action_items[0].task, "Send Alex's deck");
    }

    // ---- three synthetic calls in a row (voices are low-dimensional stand-ins
    // for TitaNet centroids, chosen so the same person across calls is very
    // similar and different people are far apart)

    use crate::speakers::{link_to_invitees, name_by_elimination, name_recorder, participants::Participants};
    use crate::transcript::SpeakerInfo;

    fn info(ch: Channel, sp: Option<u32>, secs: f64) -> SpeakerInfo {
        SpeakerInfo { channel: ch, speaker: sp, name: None, source: None, confidence: 0.0, talk_secs: secs, person: None, email: None }
    }

    fn named(mut i: SpeakerInfo, n: &str, src: NameSource, conf: f32) -> SpeakerInfo {
        i.name = Some(n.into());
        i.source = Some(src);
        i.confidence = conf;
        i
    }

    fn vdata(v: &[(Channel, Option<u32>, Vec<f32>, f64)]) -> VoiceData {
        VoiceData { centroids: v.iter().map(|(c, s, e, t)| (*c, *s, voices::normalize(e.clone()), *t)).collect() }
    }

    fn invite(owner: &str, org: Option<(&str, &str)>, people: &[(Option<&str>, &str)]) -> crate::calendar::CalendarEvent {
        use crate::calendar::Attendee;
        crate::calendar::CalendarEvent {
            id: "x".into(),
            uid: "x".into(),
            title: "t".into(),
            start: chrono::Utc::now(),
            end: chrono::Utc::now(),
            link: None,
            organizer: org.map(|(n, e)| Attendee { name: (!n.is_empty()).then(|| n.into()), email: e.into(), status: None }),
            attendees: people.iter().map(|(n, e)| Attendee { name: n.map(Into::into), email: (*e).into(), status: Some("ACCEPTED".into()) }).collect(),
            description: None,
            calendar: Some(owner.into()),
            declined: false,
            recurring: false,
            html_link: None,
            calendar_name: None,
            color: None,
        }
    }

    const ME_V: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
    const SHELLEY: [f32; 4] = [1.0, 0.0, 0.0, 0.0];
    const SHELLEY_2: [f32; 4] = [1.0, 0.25, 0.0, 0.1];
    const ANDY: [f32; 4] = [0.0, 1.0, 0.0, 0.2];
    const SETH: [f32; 4] = [0.0, 0.1, 1.0, 0.0];

    /// Run the naming steps that don't need audio, as `identify` does.
    fn name_call(t: &mut Transcript, vd: &VoiceData, store: &mut VoiceStore, ev: Option<&crate::calendar::CalendarEvent>, session: &str) {
        let me = Me::resolve(&crate::config::MeConfig::default(), ev, None);
        let people = Participants::gather(&me, ev, &Default::default());
        name_from_voiceprints(t, vd, &store.excluding_session(session));
        name_from_participants(t, &people);
        link_to_invitees(t, &people);
        name_by_elimination(t, &people);
        name_recorder(t, &me);
        remember_voices(t, vd, store, session);
    }

    #[test]
    fn a_voice_named_on_one_call_is_known_on_the_next() {
        let mut store = VoiceStore::default();

        // call A (Zoom): cues name Shelley (0.8), elimination names Andy
        let e1 = invite("alex@example.com", Some(("", "shelley@example.com")), &[(None, "shelley@example.com"), (None, "alex@example.com"), (None, "andy@partner.example.org")]);
        let mut t1 = Transcript { speakers: vec![info(Channel::System, Some(0), 540.0), info(Channel::Mic, None, 600.0), named(info(Channel::System, Some(1), 118.0), "Shelley", NameSource::Llm, 0.8)], ..Default::default() };
        let vd1 = vdata(&[(Channel::System, Some(0), ANDY.to_vec(), 520.0), (Channel::Mic, None, ME_V.to_vec(), 600.0), (Channel::System, Some(1), SHELLEY.to_vec(), 109.0)]);
        name_call(&mut t1, &vd1, &mut store, Some(&e1), "call-a");
        assert_eq!(t1.speakers[0].name.as_deref(), Some("Andy"));
        // Shelley: a confident name that is on the invite → remembered. Andy:
        // named by elimination only → not (yet) remembered.
        let people: Vec<&str> = store.people().collect();
        assert_eq!(people, vec!["Shelley"]);

        // call B (Teams): the model names Seth (0.95); Shelley is now known by voice
        let e2 = invite("alex@example.com", Some(("Sathiren Vale", "sk@guest.example.org")), &[(None, "alex@example.com"), (Some("Shelley Morgan <shelley@example.com>"), "shelley@example.com"), (Some("Sathiren Vale"), "sk@guest.example.org")]);
        let mut t2 = Transcript { speakers: vec![info(Channel::Mic, None, 1464.0), info(Channel::System, Some(0), 299.0), named(info(Channel::System, Some(1), 2349.0), "Seth", NameSource::Llm, 0.95)], ..Default::default() };
        let vd2 = vdata(&[(Channel::Mic, None, ME_V.to_vec(), 1464.0), (Channel::System, Some(0), SHELLEY_2.to_vec(), 253.0), (Channel::System, Some(1), SETH.to_vec(), 2069.0)]);
        name_call(&mut t2, &vd2, &mut store, Some(&e2), "call-b");
        let s = t2.speaker_info(Channel::System, Some(0)).unwrap();
        assert_eq!((s.name.as_deref(), s.source), (Some("Shelley"), Some(NameSource::Voiceprint)));
        let seth = t2.speaker_info(Channel::System, Some(1)).unwrap();
        assert_eq!(seth.person.as_deref(), Some("Sathiren Vale"));
        // one Shelley voiceprint (now under her full name), Seth remembered with his invite address
        let names: Vec<&str> = store.people().collect();
        assert_eq!(names, vec!["Shelley Morgan", "Seth"], "{:#?}", store.voices.iter().map(|v| (&v.name, v.samples.len())).collect::<Vec<_>>());
        assert_eq!(store.voices.iter().find(|v| v.name == "Shelley Morgan").unwrap().samples.len(), 2);

        // call C: no invite, one far voice → her voiceprint
        let mut t3 = Transcript { speakers: vec![info(Channel::Mic, None, 1688.0), info(Channel::System, None, 1503.0)], ..Default::default() };
        let vd3 = vdata(&[(Channel::Mic, None, ME_V.to_vec(), 1688.0), (Channel::System, None, [1.0, 0.1, 0.05, 0.1].to_vec(), 1241.0)]);
        name_call(&mut t3, &vd3, &mut store, None, "call-c");
        let s = t3.speaker_info(Channel::System, None).unwrap();
        assert_eq!((s.name.as_deref(), s.source), (Some("Shelley Morgan"), Some(NameSource::Voiceprint)));
        // the recorder has no invite here: still "Me" without a configured name
        assert_eq!(t3.recorder_label(), "Me");

        // re-running a call doesn't count it twice
        let before = store.clone();
        name_call(&mut t2, &vd2, &mut store, Some(&e2), "call-b");
        assert_eq!(store.voices.iter().map(|v| v.samples.len()).collect::<Vec<_>>(), before.voices.iter().map(|v| v.samples.len()).collect::<Vec<_>>());
    }

    #[test]
    fn guesses_off_the_invite_are_not_remembered() {
        let mut store = VoiceStore::default();
        // a confident model name, but nobody by that name was invited
        let e = invite("me@x.com", None, &[(Some("Ann Lee"), "ann@x.com"), (Some("Bo Chu"), "bo@x.com")]);
        let mut t = Transcript { speakers: vec![named(info(Channel::System, Some(0), 200.0), "Zed", NameSource::Llm, 0.95), info(Channel::System, Some(1), 200.0)], ..Default::default() };
        let vd = vdata(&[(Channel::System, Some(0), ANDY.to_vec(), 200.0), (Channel::System, Some(1), SETH.to_vec(), 200.0)]);
        name_call(&mut t, &vd, &mut store, Some(&e), "m");
        assert_eq!(store.people().count(), 0);
        // nor a low-confidence one that is on the invite
        let mut t = Transcript { speakers: vec![named(info(Channel::System, Some(0), 200.0), "Ann", NameSource::Llm, 0.76), info(Channel::System, Some(1), 10.0)], ..Default::default() };
        name_call(&mut t, &vd, &mut store, Some(&e), "m");
        assert_eq!(store.people().count(), 0);
    }

    #[test]
    fn rename_takes_a_wrong_voiceprint_back() {
        let dir = tempfile::tempdir().unwrap();
        let sess = dir.path().join("2026-01-01 0900 call");
        std::fs::create_dir_all(&sess).unwrap();
        let mut t = Transcript {
            segments: vec![Segment { speaker: Some(0), ..seg(0.0, 30.0) }],
            speakers: vec![named(info(Channel::System, Some(0), 30.0), "Shelley", NameSource::Llm, 0.9)],
            ..Default::default()
        };
        t.speakers[0].email = Some("shelley@x.com".into());
        let vd = vdata(&[(Channel::System, Some(0), ANDY.to_vec(), 30.0)]);
        save_voice_data(&sess, &vd);
        let mut store = VoiceStore::default();
        assert!(remember_voices(&t, &vd, &mut store, &session_id(&sess)));
        assert_eq!(store.people().collect::<Vec<_>>(), vec!["Shelley"]);
        // the user says that voice was Andy: Shelley's voiceprint loses it, Andy's gets it
        super::super::rename(&mut t, Channel::System, Some(0), "Andy");
        assert!(remember_rename(&mut store, &sess, &t, Channel::System, Some(0), "Andy"));
        assert_eq!(store.people().collect::<Vec<_>>(), vec!["Andy"]);
        // a later automatic run keeps the user's name in the store
        assert!(remember_voices(&t, &vd, &mut store, &session_id(&sess)));
        assert_eq!(store.people().collect::<Vec<_>>(), vec!["Andy"]);
        assert_eq!(store.voices[0].samples.len(), 1);
        // clearing the name forgets what that voice taught
        super::super::rename(&mut t, Channel::System, Some(0), "");
        assert!(remember_rename(&mut store, &sess, &t, Channel::System, Some(0), ""));
        assert_eq!(store.people().count(), 0);
        // renaming the recorder's own voice (in a room) doesn't make a second voiceprint of them
        let kme = voices::sample_key(&session_id(&sess), Channel::Mic, Some(1));
        store.enroll_sample(SELF_NAME, None, &kme, &SETH, 60.0, "self");
        let vd_me = vdata(&[(Channel::Mic, Some(1), SETH.to_vec(), 60.0)]);
        save_voice_data(&sess, &vd_me);
        let room = Transcript { in_person: true, ..Default::default() };
        assert!(!remember_rename(&mut store, &sess, &room, Channel::Mic, Some(1), "Alexander"));
        assert_eq!(store.people().count(), 0);
        assert_eq!(store.enrolled_as(&kme).map(|v| v.name.as_str()), Some(SELF_NAME));
        save_voice_data(&sess, &vd);
        // and a re-run that no longer names the voice takes its sample back too
        let mut t = Transcript { speakers: vec![named(info(Channel::System, Some(0), 30.0), "Ann", NameSource::User, 1.0)], ..Default::default() };
        remember_voices(&t, &vd, &mut store, "m2");
        assert_eq!(store.people().count(), 1);
        t.speakers[0].name = None;
        t.speakers[0].source = None;
        assert!(remember_voices(&t, &vd, &mut store, "m2"));
        assert_eq!(store.people().count(), 0);
    }
}
