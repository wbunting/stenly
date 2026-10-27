//! Voice embeddings: grouping one channel's segments by voice, and a local
//! store of voiceprints for people you've named before.
//!
//! The neural network (TitaNet via sherpa-onnx) lives in `stenly-asr` behind
//! [`crate::transcript::Transcriber::embed_segments`]; everything here is plain
//! vector math so it can be unit-tested without models.
//!
//! Voiceprints never leave this Mac: `<data_dir>/voices.json` holds one
//! averaged 192-number embedding per person (no audio).

use crate::transcript::Segment;
use anyhow::Result;
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Cosine distance under which two clusters are the same voice. Tuned on a
/// real 2–6 person conference-room recording (0.5 split one person into
/// several; 0.8 merged distinct people).
pub const MERGE_DIST: f32 = 0.7;
/// A voice needs this much talk time (or this share) to be its own speaker.
const MIN_SPEAKER_SECS: f64 = 20.0;
/// Segments shorter than this get the label of their neighbours instead
/// ("Yeah.", "Mm-hmm" carry too little voice to embed reliably).
pub const MIN_EMBED_SECS: f64 = 1.0;
/// Centroid similarity at which a cluster is taken to be a stored voice.
/// TitaNet-small centroids of the same person across meetings score
/// 0.75–0.9; different people stay below 0.6 (see the PR for measurements).
pub const MATCH_SIM: f32 = 0.7;
/// …and it must beat the runner-up by this much.
const MATCH_MARGIN: f32 = 0.08;

pub fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    v.iter_mut().for_each(|x| *x /= n);
    v
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Mean of unit vectors, weighted, renormalised.
pub fn weighted_centroid<'a>(items: impl IntoIterator<Item = (&'a [f32], f64)>) -> Option<Vec<f32>> {
    let mut m: Vec<f32> = Vec::new();
    for (e, w) in items {
        if m.is_empty() {
            m = vec![0.0; e.len()];
        }
        for (a, b) in m.iter_mut().zip(e) {
            *a += b * w as f32;
        }
    }
    (!m.is_empty()).then(|| normalize(m))
}

/// Voices found in one channel.
#[derive(Debug, Clone, Default)]
pub struct Clusters {
    /// Speaker number per segment (numbered by first appearance), None when
    /// the channel had too little speech to tell voices apart.
    pub labels: Vec<Option<u32>>,
    /// Unit-length centroid per speaker number.
    pub centroids: Vec<Vec<f32>>,
    /// Seconds of embedded speech per speaker number.
    pub talk: Vec<f64>,
}

impl Clusters {
    pub fn count(&self) -> usize {
        self.centroids.len()
    }
}

/// Group `segs` (one channel, time order) by voice. `embs[i]` is segment i's
/// unit embedding, None when it was too short to embed.
pub fn cluster(segs: &[Segment], embs: &[Option<Vec<f32>>]) -> Clusters {
    let idx: Vec<usize> = (0..segs.len()).filter(|&i| embs.get(i).is_some_and(|e| e.is_some())).collect();
    let vecs: Vec<Vec<f32>> = idx.iter().map(|&i| embs[i].clone().unwrap_or_default()).collect();
    let durs: Vec<f64> = idx.iter().map(|&i| (segs[i].end - segs[i].start).max(0.0)).collect();
    if vecs.is_empty() {
        return Clusters { labels: vec![None; segs.len()], ..Default::default() };
    }
    if vecs.len() == 1 {
        return Clusters { labels: vec![Some(0); segs.len()], centroids: vec![vecs[0].clone()], talk: vec![durs[0]] };
    }

    let mut clusters = agglomerate(&vecs, MERGE_DIST);

    // fold tiny clusters (coughs, crosstalk) into their nearest real voice
    let talk = |c: &Vec<usize>| c.iter().map(|&k| durs[k]).sum::<f64>();
    let total: f64 = durs.iter().sum();
    let min_talk = MIN_SPEAKER_SECS.min(total * 0.03);
    let (big, small): (Vec<_>, Vec<_>) = clusters.drain(..).partition(|c| talk(c) >= min_talk);
    let mut clusters = if big.is_empty() { vec![small.into_iter().flatten().collect()] } else { big };
    let cents: Vec<Vec<f32>> = clusters.iter().map(|c| centroid(&vecs, c)).collect();
    let leftovers: Vec<usize> = (0..vecs.len()).filter(|k| !clusters.iter().any(|c| c.contains(k))).collect();
    for k in leftovers {
        let best = (0..clusters.len()).max_by(|&a, &b| dot(&vecs[k], &cents[a]).total_cmp(&dot(&vecs[k], &cents[b]))).unwrap_or(0);
        clusters[best].push(k);
    }

    // number speakers by first appearance
    let mut order: Vec<(usize, usize)> = clusters.iter().enumerate().map(|(ci, c)| (c.iter().map(|&k| idx[k]).min().unwrap_or(usize::MAX), ci)).collect();
    order.sort();
    let mut labels: Vec<Option<u32>> = vec![None; segs.len()];
    let mut centroids = Vec::new();
    let mut talks = Vec::new();
    for (n, (_, ci)) in order.iter().enumerate() {
        for &k in &clusters[*ci] {
            labels[idx[k]] = Some(n as u32);
        }
        centroids.push(centroid(&vecs, &clusters[*ci]));
        talks.push(talk(&clusters[*ci]));
    }
    // short segments inherit the nearest labelled neighbour in time
    let labelled: Vec<usize> = (0..segs.len()).filter(|&i| labels[i].is_some()).collect();
    for i in 0..segs.len() {
        if labels[i].is_none() {
            let near = labelled.iter().min_by(|&&a, &&b| {
                let d = |j: usize| (segs[j].start - segs[i].start).abs();
                d(a).total_cmp(&d(b))
            });
            if let Some(&j) = near {
                labels[i] = labels[j];
            }
        }
    }
    Clusters { labels, centroids, talk: talks }
}

fn centroid(embs: &[Vec<f32>], c: &[usize]) -> Vec<f32> {
    let mut m = vec![0.0; embs[0].len()];
    for &k in c {
        for (a, b) in m.iter_mut().zip(&embs[k]) {
            *a += b;
        }
    }
    normalize(m)
}

/// Average-link agglomerative clustering on cosine distance, O(n²) memory.
/// Merges the closest pair until no pair is closer than `max_dist`.
pub fn agglomerate(embs: &[Vec<f32>], max_dist: f32) -> Vec<Vec<usize>> {
    let n = embs.len();
    let mut sum = vec![vec![0f32; n]; n];
    for i in 0..n {
        for j in (i + 1)..n {
            let s = dot(&embs[i], &embs[j]);
            sum[i][j] = s;
            sum[j][i] = s;
        }
    }
    let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let mut alive: Vec<bool> = vec![true; n];
    loop {
        let mut best: Option<(f32, usize, usize)> = None;
        for i in 0..n {
            if !alive[i] {
                continue;
            }
            for j in (i + 1)..n {
                if !alive[j] {
                    continue;
                }
                let avg = sum[i][j] / (members[i].len() * members[j].len()) as f32;
                let d = 1.0 - avg;
                if best.is_none_or(|(bd, _, _)| d < bd) {
                    best = Some((d, i, j));
                }
            }
        }
        let Some((d, i, j)) = best else { break };
        if d > max_dist {
            break;
        }
        for k in 0..n {
            if k != i && k != j && alive[k] {
                sum[i][k] += sum[j][k];
                sum[k][i] = sum[i][k];
            }
        }
        let mj = std::mem::take(&mut members[j]);
        members[i].extend(mj);
        alive[j] = false;
    }
    members.into_iter().zip(alive).filter(|(_, a)| *a).map(|(m, _)| m).collect()
}

// ------------------------------------------------- speaker changes in a segment

/// Segments at least this long are checked for a second speaker.
pub const SPLIT_MIN_SECS: f64 = 6.0;
/// Pre-screen: windows across the segment this far apart (cosine distance)
/// mean the voice may change inside it, worth decoding words to find where.
pub const SPLIT_SCREEN_DIST: f32 = 0.45;
/// Each side of a split needs this much speech to embed.
const SPLIT_MIN_SIDE_SECS: f64 = 2.5;
/// Window embedded on each side of a candidate cut.
const SPLIT_WIN_SECS: f64 = 4.0;
/// The two sides must be this far apart (cosine distance) to be two people.
/// Same person across a pause: ≲ 0.35; two people on one codec: ≳ 0.6.
pub const SPLIT_DIST: f32 = 0.55;

/// A candidate cut: (time, window before it, window after it), in seconds.
pub type Cut = (f64, (f64, f64), (f64, f64));

/// Where to look for a speaker change in one segment: gaps between words
/// (≥ 0.2 s, or a sentence end), leaving enough speech on both sides.
/// Returns (cut time, left window, right window) per candidate.
pub fn split_candidates(seg: &Segment, words: &[crate::transcript::TimedWord]) -> Vec<Cut> {
    let mut out = Vec::new();
    for w in words.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        let pause = b.start - a.end;
        let sentence = a.text.ends_with(['.', '?', '!']);
        if pause < 0.2 && !sentence {
            continue;
        }
        let cut = (a.end + b.start) / 2.0;
        if cut - seg.start < SPLIT_MIN_SIDE_SECS || seg.end - cut < SPLIT_MIN_SIDE_SECS {
            continue;
        }
        out.push((cut, ((cut - SPLIT_WIN_SECS).max(seg.start), a.end), (b.start, (cut + SPLIT_WIN_SECS).min(seg.end))));
    }
    out
}

/// Windows across a segment for the cheap pre-screen: 3 s every 1.5 s.
pub fn screen_windows(seg: &Segment) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut a = seg.start;
    while a + 3.0 <= seg.end + 0.25 {
        out.push((a, (a + 3.0).min(seg.end)));
        a += 1.5;
    }
    out
}

/// Whether any two windows of a segment sound like different people.
pub fn voice_drifts(embs: &[Option<Vec<f32>>]) -> bool {
    let e: Vec<&Vec<f32>> = embs.iter().flatten().collect();
    e.iter().enumerate().any(|(i, a)| e[i + 1..].iter().any(|b| 1.0 - dot(a, b) >= SPLIT_SCREEN_DIST))
}

/// Pick the cut whose two sides sound most like different people, if any
/// is clearly so. `dists[i]` = cosine distance between candidate i's sides.
pub fn best_split(cands: &[Cut], dists: &[Option<f32>]) -> Option<f64> {
    cands.iter().zip(dists).filter_map(|(c, d)| d.map(|d| (c.0, d))).filter(|(_, d)| *d >= SPLIT_DIST).max_by(|a, b| a.1.total_cmp(&b.1)).map(|(t, _)| t)
}

/// Split one segment's text at `cut` using its timed words.
pub fn split_at(seg: &Segment, words: &[crate::transcript::TimedWord], cut: f64) -> Option<(Segment, Segment)> {
    let left: Vec<&str> = words.iter().filter(|w| w.end <= cut).map(|w| w.text.as_str()).collect();
    let right: Vec<&str> = words.iter().filter(|w| w.start >= cut).map(|w| w.text.as_str()).collect();
    if left.is_empty() || right.is_empty() {
        return None;
    }
    let join = |v: Vec<&str>| v.join(" ").replace(" ,", ",").replace(" .", ".").replace(" ?", "?").replace(" !", "!");
    let mut a = Segment::new(seg.channel, seg.start, cut, join(left));
    let mut b = Segment::new(seg.channel, cut, seg.end, join(right));
    a.speaker = seg.speaker;
    b.speaker = seg.speaker;
    Some((a, b))
}

// ---------------------------------------------------------------- voiceprints

/// The recorder's own voice is stored under this name.
pub const SELF_NAME: &str = "Me";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Voice {
    pub name: String,
    pub centroid: Vec<f32>,
    /// Seconds of speech averaged into the centroid (weights later updates).
    pub secs: f64,
    pub updated: DateTime<Local>,
    /// Their address, when an invite said who they are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Where the voiceprint came from, one entry per meeting voice, so a
    /// later rename can take a wrong one back out ([`VoiceStore::unenroll`]).
    /// Empty for voiceprints saved before this was tracked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<Sample>,
}

/// One meeting's contribution to a voiceprint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sample {
    /// `<session folder>#<channel>:<speaker>`, see [`sample_key`].
    pub key: String,
    pub centroid: Vec<f32>,
    pub secs: f64,
    /// Why it was saved: "user", "accessibility", "transcript" (a confident
    /// name from the conversation that matches the invite), "self".
    pub source: String,
    pub added: DateTime<Local>,
}

/// Identifies one voice of one meeting in [`Sample::key`].
pub fn sample_key(session: &str, channel: crate::platform::Channel, speaker: Option<u32>) -> String {
    format!("{session}#{}:{}", channel.speaker_label().to_lowercase(), speaker.map(|s| s.to_string()).unwrap_or_else(|| "-".into()))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct VoiceStore {
    #[serde(default)]
    pub voices: Vec<Voice>,
    /// Names Stenly worked out (from the invite) but isn't sure enough of to
    /// recognise anyone by. Never matched against; a second meeting that gives
    /// the same person the same voice turns both into a voiceprint ([`VoiceStore::corroborate`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<Pending>,
}

/// A voice that might be someone's, waiting for a second meeting to agree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pending {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub sample: Sample,
}

/// Unconfirmed voices kept (oldest dropped first).
const MAX_PENDING: usize = 64;

/// New speech is weighted against at most this much history, so a voiceprint
/// keeps adapting (new mic, a cold) instead of freezing after a few meetings.
const MAX_HISTORY_SECS: f64 = 600.0;
/// A single meeting counts for at most this much speech in a voiceprint, so
/// one long call doesn't drown out the others.
const MAX_SAMPLE_SECS: f64 = 300.0;
/// Meetings remembered per voice (oldest dropped first).
const MAX_SAMPLES: usize = 24;

/// "Shelley" ~ "Shelley Morgan" (a bare first name matches a full name with it).
fn loose_same_name(a: &str, b: &str) -> bool {
    let n = |s: &str| s.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()).filter(|w| !w.is_empty()).collect::<Vec<_>>();
    let (a, b) = (n(a), n(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b || (a.len() == 1 || b.len() == 1) && a[0] == b[0] && a[0].chars().count() >= 3
}

impl VoiceStore {
    pub fn default_path() -> PathBuf {
        crate::config::data_dir().join("voices.json")
    }

    pub fn load_from(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn load() -> Self {
        Self::load_from(&Self::default_path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::default_path())
    }

    /// Names of other people (not the recorder) with a stored voice.
    pub fn people(&self) -> impl Iterator<Item = &str> {
        self.voices.iter().map(|v| v.name.as_str()).filter(|n| *n != SELF_NAME)
    }

    /// The stored voice `c` (a unit centroid) belongs to, with its similarity.
    pub fn best_match(&self, c: &[f32]) -> Option<(&Voice, f32)> {
        self.best_match_with(c, MATCH_SIM)
    }

    /// Like [`best_match`](Self::best_match) with a custom similarity bar.
    pub fn best_match_with(&self, c: &[f32], min_sim: f32) -> Option<(&Voice, f32)> {
        let mut scored: Vec<(&Voice, f32)> = self.voices.iter().filter(|v| v.centroid.len() == c.len()).map(|v| (v, dot(&v.centroid, c))).collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        let (best, sim) = *scored.first()?;
        let runner = scored.get(1).map(|x| x.1).unwrap_or(-1.0);
        (sim >= min_sim && sim - runner >= MATCH_MARGIN).then_some((best, sim))
    }

    /// Fold `secs` of speech with unit centroid `c` into `name`'s voiceprint.
    pub fn enroll(&mut self, name: &str, c: &[f32], secs: f64) {
        let name = name.trim();
        if name.is_empty() || c.is_empty() || secs <= 0.0 {
            return;
        }
        let now = Local::now();
        match self.voices.iter_mut().find(|v| v.name.eq_ignore_ascii_case(name) && v.centroid.len() == c.len()) {
            Some(v) => {
                let old = v.secs.min(MAX_HISTORY_SECS);
                v.centroid = weighted_centroid([(v.centroid.as_slice(), old), (c, secs)]).unwrap_or_else(|| c.to_vec());
                v.secs = (v.secs + secs).min(MAX_HISTORY_SECS * 4.0);
                v.updated = now;
            }
            None => self.voices.push(Voice { name: name.to_string(), centroid: normalize(c.to_vec()), secs, updated: now, email: None, samples: vec![] }),
        }
    }

    /// Save one meeting voice as `name`'s, replacing what the same meeting
    /// voice contributed before (re-running naming doesn't count it twice)
    /// and taking it away from anyone else it was saved as. "Shelley" and
    /// "Shelley Morgan" are one voiceprint when the address or the voice
    /// agrees; the fuller name is kept. Returns the stored name.
    pub fn enroll_sample(&mut self, name: &str, email: Option<&str>, key: &str, c: &[f32], secs: f64, source: &str) -> Option<String> {
        let name = name.trim();
        if name.is_empty() || c.is_empty() || secs <= 0.0 {
            return None;
        }
        self.unenroll(key);
        let email = email.map(|e| e.trim().to_ascii_lowercase()).filter(|e| e.contains('@'));
        let c = normalize(c.to_vec());
        let hit = self.voices.iter().position(|v| v.centroid.len() == c.len() && v.name.eq_ignore_ascii_case(name)).or_else(|| {
            self.voices.iter().position(|v| {
                v.centroid.len() == c.len()
                    && v.name != SELF_NAME
                    && (email.is_some() && v.email == email || loose_same_name(&v.name, name) && dot(&v.centroid, &c) >= MATCH_SIM)
            })
        });
        let now = Local::now();
        let sample = Sample { key: key.to_string(), centroid: c.clone(), secs, source: source.to_string(), added: now };
        let i = match hit {
            Some(i) => {
                let v = &mut self.voices[i];
                if v.samples.is_empty() {
                    // a voiceprint from before samples were kept: keep it as one
                    v.samples.push(Sample { key: String::new(), centroid: v.centroid.clone(), secs: v.secs.min(MAX_HISTORY_SECS), source: "earlier".into(), added: v.updated });
                }
                if name.split_whitespace().count() > v.name.split_whitespace().count() {
                    v.name = name.to_string();
                }
                if v.email.is_none() {
                    v.email = email;
                }
                v.samples.push(sample);
                i
            }
            None => {
                self.voices.push(Voice { name: name.to_string(), centroid: c.clone(), secs, updated: now, email, samples: vec![sample] });
                self.voices.len() - 1
            }
        };
        // unconfirmed voices of this person from other meetings that sound the same are confirmed now
        let session = |k: &str| k.split('#').next().unwrap_or(k).to_string();
        let (vname, vemail, vc) = (self.voices[i].name.clone(), self.voices[i].email.clone(), c.clone());
        let mut k = 0;
        while k < self.pending.len() {
            let p = &self.pending[k];
            if session(&p.sample.key) != session(key)
                && p.sample.centroid.len() == vc.len()
                && (vemail.is_some() && p.email == vemail || loose_same_name(&p.name, &vname) || loose_same_name(&p.name, name))
                && dot(&p.sample.centroid, &vc) >= MATCH_SIM
            {
                let mut p = self.pending.remove(k);
                p.sample.source = "corroborated".into();
                self.voices[i].samples.push(p.sample);
            } else {
                k += 1;
            }
        }
        let v = &mut self.voices[i];
        v.recompute();
        Some(v.name.clone())
    }

    /// A name for a voice that isn't strong enough on its own. If another
    /// meeting already gave the same person (same name or address) a voice
    /// this close, both become their voiceprint and the stored name is
    /// returned; otherwise the voice waits in [`pending`](Self::pending).
    pub fn corroborate(&mut self, name: &str, email: Option<&str>, key: &str, c: &[f32], secs: f64, source: &str) -> Option<String> {
        let name = name.trim();
        if name.is_empty() || c.is_empty() || secs <= 0.0 {
            return None;
        }
        let c = normalize(c.to_vec());
        let email = email.map(|e| e.trim().to_ascii_lowercase()).filter(|e| e.contains('@'));
        self.pending.retain(|p| p.sample.key != key);
        let session = |k: &str| k.split('#').next().unwrap_or(k).to_string();
        let agree = self.pending.iter().position(|p| {
            session(&p.sample.key) != session(key)
                && p.sample.centroid.len() == c.len()
                && (email.is_some() && p.email == email || loose_same_name(&p.name, name))
                && dot(&p.sample.centroid, &c) >= MATCH_SIM
        });
        match agree {
            Some(i) => {
                let p = self.pending.remove(i);
                let first = self.enroll_sample(&p.name, p.email.as_deref(), &p.sample.key, &p.sample.centroid, p.sample.secs, "corroborated");
                let both = self.enroll_sample(name, email.as_deref(), key, &c, secs, "corroborated");
                tracing::debug!(?first, ?both, "two meetings agree on a voice");
                both
            }
            None => {
                self.pending.push(Pending { name: name.to_string(), email, sample: Sample { key: key.to_string(), centroid: c, secs, source: source.to_string(), added: Local::now() } });
                if self.pending.len() > MAX_PENDING {
                    self.pending.sort_by_key(|p| std::cmp::Reverse(p.sample.added));
                    self.pending.truncate(MAX_PENDING);
                }
                None
            }
        }
    }

    /// Take one meeting voice back out of whichever voiceprint it went into.
    /// Returns the name it was saved as.
    pub fn unenroll(&mut self, key: &str) -> Option<String> {
        self.pending.retain(|p| p.sample.key != key);
        let mut was = None;
        for v in self.voices.iter_mut() {
            let n = v.samples.len();
            v.samples.retain(|s| s.key != key);
            if v.samples.len() != n {
                was = Some(v.name.clone());
                v.recompute();
            }
        }
        // a voiceprint made only from samples that were all taken back
        self.voices.retain(|v| !(v.samples.is_empty() && v.secs <= 0.0));
        was
    }

    /// The store as it was without one meeting's voices, for matching that
    /// meeting: a re-run must not recognise a voice from itself.
    pub fn excluding_session(&self, session: &str) -> VoiceStore {
        let prefix = format!("{session}#");
        let mut s = self.clone();
        let keys: Vec<String> = s.voices.iter().flat_map(|v| v.samples.iter()).filter(|x| x.key.starts_with(&prefix)).map(|x| x.key.clone()).collect();
        for k in keys {
            s.unenroll(&k);
        }
        s
    }

    /// Which voiceprint (if any) one meeting voice was saved into.
    pub fn enrolled_as(&self, key: &str) -> Option<&Voice> {
        self.voices.iter().find(|v| v.samples.iter().any(|s| s.key == key))
    }

    /// Forget every voice, remembered or still waiting for a second meeting.
    pub fn clear(&mut self) {
        self.voices.clear();
        self.pending.clear();
    }

    /// Anything to forget: people's voiceprints or voices still waiting to be confirmed.
    pub fn is_empty(&self) -> bool {
        self.people().next().is_none() && self.pending.is_empty()
    }

    /// Forget someone's voice (e.g. a speaker was renamed away from them).
    pub fn forget(&mut self, name: &str) {
        self.voices.retain(|v| !v.name.eq_ignore_ascii_case(name));
        self.pending.retain(|p| !loose_same_name(&p.name, name));
    }
}

impl Voice {
    /// Rebuild the centroid from the samples (newest first, each meeting capped).
    fn recompute(&mut self) {
        if self.samples.is_empty() {
            self.secs = 0.0;
            return;
        }
        self.samples.sort_by_key(|x| std::cmp::Reverse(x.added));
        self.samples.truncate(MAX_SAMPLES);
        let mut budget = MAX_HISTORY_SECS * 4.0;
        let mut items: Vec<(&[f32], f64)> = Vec::new();
        for s in &self.samples {
            let w = s.secs.min(MAX_SAMPLE_SECS).min(budget);
            if w <= 0.0 {
                break;
            }
            budget -= w;
            items.push((s.centroid.as_slice(), w));
        }
        if let Some(c) = weighted_centroid(items) {
            self.centroid = c;
        }
        self.secs = self.samples.iter().map(|s| s.secs).sum::<f64>().min(MAX_HISTORY_SECS * 4.0);
        self.updated = self.samples.iter().map(|s| s.added).max().unwrap_or(self.updated);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Channel;

    fn v(a: f32, b: f32) -> Vec<f32> {
        normalize(vec![a, b, 0.0])
    }

    #[test]
    fn agglomerates_two_voices() {
        let embs = vec![v(1.0, 0.05), v(1.0, -0.05), v(1.0, 0.0), v(0.05, 1.0), v(-0.05, 1.0)];
        let mut c = agglomerate(&embs, 0.7);
        c.iter_mut().for_each(|x| x.sort());
        c.sort();
        assert_eq!(c, vec![vec![0, 1, 2], vec![3, 4]]);
    }

    #[test]
    fn clusters_label_by_first_appearance_and_fill_short_segments() {
        let seg = |s: f64, e: f64| Segment::new(Channel::System, s, e, "x");
        // B speaks first; a 0.5 s "yeah" (no embedding) sits next to A's turn
        let segs = vec![seg(0.0, 30.0), seg(31.0, 60.0), seg(61.0, 61.5), seg(62.0, 90.0)];
        let embs = vec![Some(v(0.0, 1.0)), Some(v(1.0, 0.0)), None, Some(v(1.0, 0.1))];
        let c = cluster(&segs, &embs);
        assert_eq!(c.count(), 2);
        assert_eq!(c.labels, vec![Some(0), Some(1), Some(1), Some(1)]);
        assert!(dot(&c.centroids[0], &v(0.0, 1.0)) > 0.99);
        assert!((c.talk[1] - 57.0).abs() < 1e-6);
    }

    #[test]
    fn store_matches_enrolls_and_adapts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("voices.json");
        let mut s = VoiceStore::default();
        s.enroll("Alex", &v(1.0, 0.0), 60.0);
        s.enroll("Sam", &v(0.0, 1.0), 60.0);
        s.save_to(&path).unwrap();
        let s = VoiceStore::load_from(&path);
        assert_eq!(s.voices.len(), 2);
        assert_eq!(s.best_match(&v(1.0, 0.2)).map(|(x, _)| x.name.as_str()), Some("Alex"));
        // halfway between two people: ambiguous, no match
        assert!(s.best_match(&v(1.0, 1.0)).is_none());
        let mut s = s;
        s.enroll("alex", &v(1.0, 0.3), 60.0);
        assert_eq!(s.voices.len(), 2, "same person, case-insensitive");
        assert!(s.voices[0].centroid[1] > 0.1, "centroid moved toward the new speech");
        s.forget("Sam");
        assert_eq!(s.people().collect::<Vec<_>>(), vec!["Alex"]);
    }

    #[test]
    fn samples_are_idempotent_merged_and_reversible() {
        let mut s = VoiceStore::default();
        let k1 = sample_key("call-a", Channel::System, Some(1));
        let k2 = sample_key("call-b", Channel::System, Some(0));
        // the first call names her "Shelley" (her address on that invite)
        assert_eq!(s.enroll_sample("Shelley", Some("shelley@first.example.com"), &k1, &v(1.0, 0.0), 100.0, "transcript").as_deref(), Some("Shelley"));
        // re-running naming on the same call doesn't add a second sample
        s.enroll_sample("Shelley", Some("shelley@first.example.com"), &k1, &v(1.0, 0.0), 100.0, "transcript");
        assert_eq!(s.voices[0].samples.len(), 1);
        // the next invite has her full name and another address: same voice, so one voiceprint, fuller name
        assert_eq!(s.enroll_sample("Shelley Morgan", Some("shelley@second.example.com"), &k2, &v(1.0, 0.1), 200.0, "transcript").as_deref(), Some("Shelley Morgan"));
        assert_eq!(s.voices.len(), 1);
        assert_eq!(s.voices[0].samples.len(), 2);
        // a different voice with the same first name is someone else
        let k3 = sample_key("other", Channel::System, None);
        s.enroll_sample("Shelley", None, &k3, &v(0.0, 1.0), 60.0, "transcript");
        assert_eq!(s.voices.len(), 2);
        // the user renames the call-b voice: it leaves Shelley's voiceprint
        assert_eq!(s.unenroll(&k2).as_deref(), Some("Shelley Morgan"));
        assert_eq!(s.voices[0].samples.len(), 1);
        assert!(s.enrolled_as(&k2).is_none());
        assert_eq!(s.enrolled_as(&k1).map(|v| v.name.as_str()), Some("Shelley Morgan"));
        // taking back the only sample removes the voiceprint
        s.unenroll(&k3);
        assert_eq!(s.voices.len(), 1);
    }

    #[test]
    fn two_meetings_must_agree_before_an_invite_guess_is_remembered() {
        let mut s = VoiceStore::default();
        let k1 = sample_key("call-a", Channel::System, Some(1));
        let k2 = sample_key("call-b", Channel::System, Some(0));
        assert_eq!(s.corroborate("Shelley", Some("shelley@first.example.com"), &k1, &v(1.0, 0.0), 100.0, "invite"), None);
        assert!(s.voices.is_empty() && s.pending.len() == 1, "waits, and is never matched");
        assert!(s.best_match(&v(1.0, 0.0)).is_none());
        // the same meeting again doesn't confirm itself
        assert_eq!(s.corroborate("Shelley", Some("shelley@first.example.com"), &k1, &v(1.0, 0.0), 100.0, "invite"), None);
        assert_eq!(s.pending.len(), 1);
        // a different-sounding "Shelley" elsewhere doesn't either
        assert_eq!(s.corroborate("Shelley", None, &sample_key("x", Channel::System, None), &v(0.0, 1.0), 100.0, "invite"), None);
        // another meeting, same person by name, same voice: both become her voiceprint
        assert_eq!(s.corroborate("Shelley Morgan", Some("shelley@second.example.com"), &k2, &v(1.0, 0.1), 250.0, "invite").as_deref(), Some("Shelley Morgan"));
        assert_eq!(s.voices.len(), 1);
        assert_eq!(s.voices[0].samples.len(), 2);
        assert_eq!(s.pending.len(), 1, "the other Shelley still waits");
        assert!(s.best_match(&v(1.0, 0.05)).is_some());
        // a confident name elsewhere confirms a waiting voice of the same person
        let mut s2 = VoiceStore::default();
        s2.corroborate("Andy", Some("andy@guest.example.org"), &k1, &v(0.0, 1.0), 100.0, "invite");
        s2.enroll_sample("Andy", Some("andy@guest.example.org"), &k2, &v(0.1, 1.0), 100.0, "transcript");
        assert!(s2.pending.is_empty());
        assert_eq!(s2.voices[0].samples.len(), 2);
        // matching a meeting again ignores what that meeting itself taught
        let only_b = s.excluding_session("call-a");
        assert_eq!(only_b.voices[0].samples.len(), 1);
        assert!(s.excluding_session("call-a").excluding_session("call-b").voices.is_empty());
        // "Forget Voices" leaves nothing that a later meeting could confirm
        let mut wiped = s.clone();
        assert!(!wiped.is_empty());
        wiped.clear();
        assert!(wiped.is_empty() && wiped.pending.is_empty());
        assert_eq!(wiped.corroborate("Shelley", None, &sample_key("later", Channel::System, None), &v(0.0, 1.0), 100.0, "invite"), None);
        assert!(wiped.voices.is_empty(), "the forgotten meeting can't confirm a new guess");
        // only pending voices still count as something to forget
        let mut only_pending = VoiceStore::default();
        only_pending.corroborate("Andy", None, &k1, &v(0.0, 1.0), 100.0, "invite");
        assert!(!only_pending.is_empty());
        // a rename takes a sample back out, pending or enrolled
        s.unenroll(&sample_key("x", Channel::System, None));
        assert!(s.pending.is_empty());
    }

    #[test]
    fn legacy_voiceprints_keep_their_history() {
        let mut s = VoiceStore::default();
        s.enroll("Alex", &v(1.0, 0.0), 120.0);
        let k = sample_key("m", Channel::System, None);
        s.enroll_sample("Alex", None, &k, &v(1.0, 0.2), 60.0, "user");
        assert_eq!(s.voices.len(), 1);
        assert_eq!(s.voices[0].samples.len(), 2, "the old centroid became a sample");
        s.unenroll(&k);
        assert_eq!(s.voices.len(), 1, "the earlier voiceprint stays");
        assert!(dot(&s.voices[0].centroid, &v(1.0, 0.0)) > 0.999);
    }

    #[test]
    fn splits_a_turn_where_the_voice_changes() {
        use crate::transcript::TimedWord;
        let w = |t: &str, s: f64, e: f64| TimedWord { text: t.into(), start: s, end: e };
        // "That makes sense. | Can I ask about caching?" — two people, one VAD segment
        let seg = Segment::new(Channel::System, 100.0, 110.0, "That makes sense. Can I ask about caching?");
        let words = vec![w("That", 100.0, 100.8), w("makes", 101.0, 101.8), w("sense.", 102.0, 103.4), w("Can", 103.5, 104.0), w("I", 104.1, 104.5), w("ask", 104.6, 105.5), w("about", 106.0, 107.0), w("caching?", 107.1, 109.5)];
        let c = split_candidates(&seg, &words);
        // the sentence end at 103.45 and the 0.5 s pause at 105.75; the pause at 101.9 is too near the start
        assert_eq!(c.iter().map(|x| (x.0 * 100.0).round() / 100.0).collect::<Vec<_>>(), vec![103.45, 105.75]);
        assert_eq!(best_split(&c, &[Some(0.8), Some(0.3)]), Some(c[0].0));
        assert_eq!(best_split(&c, &[Some(0.3), Some(0.2)]), None, "same voice on both sides");
        let (a, b) = split_at(&seg, &words, c[0].0).unwrap();
        assert_eq!((a.text.as_str(), b.text.as_str()), ("That makes sense.", "Can I ask about caching?"));
        assert_eq!((a.end, b.start), (c[0].0, c[0].0));
        // pre-screen windows cover the segment
        assert_eq!(screen_windows(&seg).len(), 5);
        assert!(voice_drifts(&[Some(v(1.0, 0.0)), None, Some(v(0.0, 1.0))]));
        assert!(!voice_drifts(&[Some(v(1.0, 0.0)), Some(v(1.0, 0.3))]));
    }

    #[test]
    fn missing_or_corrupt_store_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(VoiceStore::load_from(&dir.path().join("nope.json")).voices.is_empty());
        std::fs::write(dir.path().join("bad.json"), "{not json").unwrap();
        assert!(VoiceStore::load_from(&dir.path().join("bad.json")).voices.is_empty());
    }
}
