//! Streaming dictation: transcribe while the user is still talking.
//!
//! Parakeet is an offline model (it sees a whole clip at once), so "streaming"
//! means re-decoding a window that slides along the recording:
//!
//! ```text
//!   committed text            │ cut          live edge
//!   ──────────────────────────┼──────────────┤
//!              ├─ left ctx ───┤  tentative   │← right margin
//!              └──── decoded window ─────────┘
//! ```
//!
//! Every `stride` seconds of new audio the window `[cut - left_context, now]`
//! is decoded. Parakeet returns a timestamp for every token, so words are placed
//! on the recording's timeline and stitched by time, not by text matching:
//! words before the cut were committed by an earlier window and are only
//! context here; words after it are new. Then the cut advances to a word
//! boundary that still has at least `right_margin` of audio after it, so no
//! committed word was decoded without hearing how it ends.
//!
//! * `left_context > 0` ("overlap"): the words right after a cut are decoded with
//!   the preceding sentence in view, so casing/punctuation stay sentence-aware.
//! * `left_context = 0` + `min_gap > 0` ("pause"): cut only in pauses and decode
//!   each phrase on its own (like meetings do).
//!
//! When the user stops, only `[cut - left_context, end]` is left to decode, so
//! the wait no longer grows with the length of the dictation.

pub const RATE: usize = 16_000;

/// A word on the recording's timeline (seconds). `text` keeps Parakeet's leading
/// space and any punctuation that follows the word.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Word {
    pub text: String,
    pub start: f32,
    pub end: f32,
}

impl Word {
    fn mid(&self) -> f32 {
        (self.start + self.end) / 2.0
    }
    /// Lowercase letters/digits only (for dedupe at a cut).
    fn norm(&self) -> String {
        self.text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
    }
}

/// Group Parakeet's sub-word tokens into words. A token that starts with a
/// space starts a word; others (word pieces, punctuation) extend the previous one.
pub fn group_words(tokens: &[String], timestamps: &[f32], durations: Option<&[f32]>, offset: f32) -> Vec<Word> {
    let mut out: Vec<Word> = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        let Some(&ts) = timestamps.get(i) else { break };
        let dur = durations.and_then(|d| d.get(i)).copied().unwrap_or(0.08).max(0.04);
        let (start, end) = (offset + ts, offset + ts + dur);
        match out.last_mut() {
            Some(w) if !t.starts_with(' ') => {
                w.text.push_str(t);
                // punctuation tokens are stamped where the decoder emitted them;
                // don't let them stretch the word into the following pause
                if t.chars().any(char::is_alphanumeric) {
                    w.end = w.end.max(end);
                }
            }
            _ => out.push(Word { text: t.clone(), start, end }),
        }
    }
    out
}

/// " Is" → " is", but leave "I", "I'm", acronyms and names with inner capitals.
fn decapitalize(word: &mut String) {
    let t = word.trim_start();
    let lead = word.len() - t.len();
    let letters: String = t.chars().filter(|c| c.is_alphabetic()).collect();
    let mut cs = letters.chars();
    let (Some(c0), rest) = (cs.next(), cs.as_str()) else { return };
    if !c0.is_uppercase() || rest.is_empty() && c0 == 'I' || t.starts_with("I'") || rest.chars().any(char::is_uppercase) {
        return;
    }
    let lower: String = c0.to_lowercase().collect();
    word.replace_range(lead..lead + c0.len_utf8(), &lower);
}

pub fn join(words: &[Word]) -> String {
    let s: String = words.iter().map(|w| w.text.as_str()).collect();
    s.trim().to_string()
}

#[derive(Clone, Copy, Debug)]
pub struct StreamParams {
    /// Seconds of already-committed audio decoded again before the cut.
    pub left_context: f32,
    /// Don't commit a word unless this much audio follows it.
    pub right_margin: f32,
    /// Look for the next cut in the last `search` seconds before the margin.
    pub search: f32,
    /// Only cut in a pause at least this long (0 = any word boundary).
    pub min_gap: f32,
    /// Decode again after this many seconds of new audio.
    pub stride: f32,
    /// Cut anyway (at the longest gap) once this much audio is uncommitted.
    pub max_pending: f32,
    /// Don't cut until this much audio is uncommitted (fewer seams).
    pub min_pending: f32,
    /// Only commit words that two consecutive decodes agree on.
    pub agree: bool,
}

impl StreamParams {
    /// Overlapping windows stitched on token timestamps.
    pub const OVERLAP: Self = Self { left_context: 4.0, right_margin: 1.5, search: 4.0, min_gap: 0.0, stride: 2.0, max_pending: 14.0, min_pending: 6.0, agree: true };
    /// Cut only at pauses; each phrase decoded on its own.
    pub const PAUSE: Self = Self { left_context: 0.0, right_margin: 1.0, search: 6.0, min_gap: 0.35, stride: 2.0, max_pending: 15.0, min_pending: 4.0, agree: true };
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StreamStats {
    pub decodes: u32,
    /// Seconds spent decoding while recording.
    pub live_secs: f32,
    pub max_live_decode: f32,
    /// Seconds of audio in the final (post-stop) decode, and how long it took.
    pub tail_audio: f32,
    pub tail_secs: f32,
}

pub struct Streamer<D: FnMut(&[f32]) -> Vec<Word>> {
    decode: D,
    pub params: StreamParams,
    audio: Vec<f32>,
    committed: Vec<Word>,
    /// Timeline position of the cut (seconds).
    cut: f32,
    tentative: Vec<Word>,
    /// Words after the cut from the previous decode (for agreement).
    prev: Vec<Word>,
    decoded_to: usize,
    pub stats: StreamStats,
}

impl<D: FnMut(&[f32]) -> Vec<Word>> Streamer<D> {
    pub fn new(params: StreamParams, decode: D) -> Self {
        Self { decode, params, audio: Vec::with_capacity(RATE * 60), committed: vec![], cut: 0.0, tentative: vec![], prev: vec![], decoded_to: 0, stats: StreamStats::default() }
    }

    pub fn push(&mut self, samples: &[f32]) {
        self.audio.extend_from_slice(samples);
    }

    pub fn audio(&self) -> &[f32] {
        &self.audio
    }

    pub fn into_audio(self) -> Vec<f32> {
        self.audio
    }

    fn secs(&self) -> f32 {
        self.audio.len() as f32 / RATE as f32
    }

    /// Samples needed before the next decode.
    pub fn next_ready_at(&self) -> usize {
        self.decoded_to + (self.params.stride * RATE as f32) as usize
    }

    /// Enough new audio for another decode?
    pub fn ready(&self) -> bool {
        self.audio.len() >= self.decoded_to + (self.params.stride * RATE as f32) as usize
    }

    /// Committed text and the current guess for the rest.
    pub fn preview(&self) -> (String, String) {
        (join(&self.committed), join(&self.tentative))
    }

    /// Decode `[cut - left_context, end]`; returns the words after the cut.
    fn decode_tail(&mut self, pad: bool) -> Vec<Word> {
        let from_t = (self.cut - self.params.left_context).max(0.0);
        let from = ((from_t * RATE as f32) as usize).min(self.audio.len());
        let mut window = self.audio[from..].to_vec();
        if pad {
            // a little trailing silence helps the decoder finish the last word
            window.extend(std::iter::repeat_n(0.0, RATE / 3));
        }
        let offset = from as f32 / RATE as f32;
        let words = (self.decode)(&window);
        let mut new: Vec<Word> = words
            .into_iter()
            .map(|mut w| {
                w.start += offset;
                w.end += offset;
                w
            })
            .filter(|w| w.mid() >= self.cut)
            .collect();
        // Same word on both sides of the cut (timestamps jitter by a frame): keep one.
        if let (Some(first), Some(last)) = (new.first(), self.committed.last()) {
            if first.norm() == last.norm() && (first.start - last.start).abs() < 0.35 {
                new.remove(0);
            }
        }
        // Decoded without the start of its sentence, Parakeet capitalises the
        // first word ("…basically Is the"). Undo that mid-sentence.
        if from_t > 0.0 || self.cut > 0.0 {
            if let (Some(first), Some(last)) = (new.first_mut(), self.committed.last()) {
                if !last.text.trim_end().ends_with(['.', '?', '!']) {
                    decapitalize(&mut first.text);
                }
            }
        }
        new
    }

    /// Run one decode if enough new audio arrived. Returns true if it decoded.
    pub fn step(&mut self) -> bool {
        if !self.ready() {
            return false;
        }
        self.decoded_to = self.audio.len();
        let t0 = std::time::Instant::now();
        let new = self.decode_tail(false);
        let took = t0.elapsed().as_secs_f32();
        self.stats.decodes += 1;
        self.stats.live_secs += took;
        self.stats.max_live_decode = self.stats.max_live_decode.max(took);

        let p = self.params;
        let edge = self.secs();
        let limit = edge - p.right_margin;
        // A cut is stable if the previous decode heard the same thing before it
        // (compared as letters only, so "server-only" == "server only").
        let prev = std::mem::take(&mut self.prev);
        let letters = |ws: &mut dyn Iterator<Item = &Word>| ws.map(Word::norm).collect::<String>();
        let agrees = |pos: f32, n: usize| !p.agree || (!prev.is_empty() && letters(&mut new[..n].iter()) == letters(&mut prev.iter().filter(|w| w.mid() < pos)) && prev.last().is_some_and(|w| w.end > pos));
        // Candidate cuts: after word j, if it ends before the margin.
        // (position of the cut, gap length, number of words committed)
        let mut cands: Vec<(f32, f32, usize)> = Vec::new();
        for j in 0..new.len() {
            if new[j].end > limit {
                break;
            }
            let next = new.get(j + 1).map_or(edge, |w| w.start);
            let gap = (next - new[j].end).max(0.0);
            let pos = new[j].end + (gap / 2.0).min(0.3);
            cands.push((pos, gap, j + 1));
        }
        let pending = edge - self.cut;
        let best = |c: &[(f32, f32, usize)]| c.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1).then(a.0.total_cmp(&b.0)));
        let stable: Vec<_> = cands.iter().copied().filter(|c| agrees(c.0, c.2)).collect();
        let near = |c: &(f32, f32, usize)| c.0 >= limit - p.search;
        let pick = if pending < p.min_pending {
            None
        } else {
            let good: Vec<_> = stable.iter().copied().filter(|c| near(c) && c.1 >= p.min_gap).collect();
            best(&good)
                // any agreed pause (pause mode): the latest one
                .or_else(|| stable.iter().copied().filter(|c| p.min_gap > 0.0 && c.1 >= p.min_gap).last())
                // overdue (e.g. decodes are slow and never agree): cut close to the
                // edge anyway, so the window, and the next decode, stays small
                .or_else(|| {
                    if pending <= p.max_pending {
                        return None;
                    }
                    let close: Vec<_> = cands.iter().copied().filter(near).collect();
                    best(&close).or_else(|| cands.last().copied())
                })
        };
        // Silence after the cut (no word starts before the margin): move the cut
        // up to the next speech so quiet stretches don't grow the window.
        let pick = pick.or_else(|| {
            let first = new.first().map_or(limit, |w| w.start - 0.3).min(limit);
            (cands.is_empty() && first - self.cut > 1.0).then_some((first, 0.0, 0))
        });
        tracing::debug!(
            window = %format!("{:.2}-{:.2}", (self.cut - p.left_context).max(0.0), edge),
            cut = self.cut,
            new = %new.iter().map(|w| format!("{}@{:.2}-{:.2}", w.text.trim(), w.start, w.end)).collect::<Vec<_>>().join(" "),
            pick = ?pick.map(|x| (x.0, x.2)),
            "stream step"
        );
        match pick {
            Some((pos, _, n)) => {
                self.committed.extend(new[..n].iter().cloned());
                self.tentative = new[n..].to_vec();
                self.cut = pos;
                // the next decode must agree with this guess (from a window
                // that starts elsewhere, which is the point)
                self.prev = self.tentative.clone();
            }
            None => {
                self.prev = new.clone();
                self.tentative = new;
            }
        }
        true
    }

    /// Recording stopped: decode what's left and return the whole text.
    pub fn finish(&mut self) -> String {
        let t0 = std::time::Instant::now();
        self.stats.tail_audio = self.secs() - (self.cut - self.params.left_context).max(0.0);
        let new = self.decode_tail(true);
        self.stats.tail_secs = t0.elapsed().as_secs_f32();
        self.committed.extend(new);
        self.tentative.clear();
        join(&self.committed)
    }
}

/// Text shown while dictating: `committed` won't change; `tentative` may.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Preview {
    pub committed: String,
    pub tentative: String,
}

/// A streaming transcription running on its own thread while the mic records.
/// Push 16 kHz audio with [`LiveDictation::sender`]; read [`LiveDictation::preview`]
/// any time; [`LiveDictation::finish`] after the mic stops.
pub struct LiveDictation {
    tx: Option<crossbeam_channel::Sender<Vec<f32>>>,
    thread: Option<std::thread::JoinHandle<anyhow::Result<(String, StreamStats)>>>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub preview: std::sync::Arc<std::sync::Mutex<Preview>>,
}

impl LiveDictation {
    /// `load` runs on the worker thread (the model is normally already warm);
    /// audio sent meanwhile queues up and is caught up on.
    pub fn start(load: impl FnOnce() -> anyhow::Result<std::sync::Arc<crate::ParakeetTranscriber>> + Send + 'static, params: StreamParams) -> Self {
        use std::sync::atomic::Ordering;
        let (tx, rx) = crossbeam_channel::unbounded::<Vec<f32>>();
        let preview = std::sync::Arc::new(std::sync::Mutex::new(Preview::default()));
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (pv, cancel) = (preview.clone(), cancelled.clone());
        let thread = std::thread::Builder::new()
            .name("stenly-dictation-asr".into())
            .spawn(move || {
                let asr = load()?;
                let mut s = asr.streamer(params);
                let mut open = true;
                while open {
                    if cancel.load(Ordering::Relaxed) {
                        anyhow::bail!("cancelled");
                    }
                    // wait for audio, then take everything that's queued so a
                    // slow decode never leaves us working on stale audio
                    match rx.recv() {
                        Ok(a) => s.push(&a),
                        Err(_) => open = false,
                    }
                    while let Ok(a) = rx.try_recv() {
                        s.push(&a);
                    }
                    if open && s.step() {
                        let (committed, tentative) = s.preview();
                        if let Ok(mut p) = pv.lock() {
                            *p = Preview { committed, tentative };
                        }
                    }
                }
                if cancel.load(Ordering::Relaxed) {
                    anyhow::bail!("cancelled");
                }
                let text = s.finish();
                asr.check_health()?;
                Ok((text, s.stats))
            })
            .expect("spawn dictation asr");
        Self { tx: Some(tx), thread: Some(thread), cancelled, preview }
    }

    pub fn sender(&self) -> crossbeam_channel::Sender<Vec<f32>> {
        self.tx.clone().expect("live dictation already finished")
    }

    /// After every sender is dropped (mic stopped): decode the rest. Returns
    /// the text and timings.
    pub fn finish(mut self) -> anyhow::Result<(String, StreamStats)> {
        drop(self.tx.take());
        let t = self.thread.take().expect("joined twice");
        t.join().map_err(|_| anyhow::anyhow!("dictation transcriber panicked"))?
    }
}

impl Drop for LiveDictation {
    fn drop(&mut self) {
        // dropped without finish (cancelled): stop at the next step, don't wait
        if self.thread.is_some() {
            self.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        drop(self.tx.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &[(&str, f32)]) -> (Vec<String>, Vec<f32>) {
        (s.iter().map(|x| x.0.to_string()).collect(), s.iter().map(|x| x.1).collect())
    }

    #[test]
    fn decapitalizes_mid_sentence_only_plain_words() {
        for (a, b) in [(" Is", " is"), (" Remaining,", " remaining,"), (" I", " I"), (" I'm", " I'm"), (" VOICEINK", " VOICEINK"), (" GitHub", " GitHub"), (" the", " the")] {
            let mut w = a.to_string();
            decapitalize(&mut w);
            assert_eq!(w, b);
        }
    }

    #[test]
    fn groups_pieces_and_punctuation() {
        let (t, ts) = toks(&[(" A", 0.24), ("re", 0.32), (" we", 0.48), (" t", 1.52), ("ri", 1.68), ("p", 1.76), ("?", 1.92)]);
        let w = group_words(&t, &ts, None, 10.0);
        assert_eq!(w.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), [" Are", " we", " trip?"]);
        assert!((w[0].start - 10.24).abs() < 1e-4);
        assert!(w[2].end < 11.9, "punctuation doesn't extend the word");
        assert_eq!(join(&w), "Are we trip?");
    }

    /// Fake decoder: a "recording" of words at known times; decoding a window
    /// returns the words fully inside it (relative times).
    fn script() -> Vec<Word> {
        (0..60).map(|i| Word { text: format!(" w{i}"), start: i as f32 * 0.5 + 0.1, end: i as f32 * 0.5 + 0.4 }).collect()
    }

    fn run(p: StreamParams) -> (String, StreamStats) {
        let words = script();
        let total = 31.0;
        let mut s = Streamer::new(p, |win: &[f32]| {
            // window position is recovered from a marker in the samples
            let off = win[0];
            let len = win.len() as f32 / RATE as f32;
            words
                .iter()
                .filter(|w| w.start >= off && w.end <= off + len)
                .map(|w| Word { text: w.text.clone(), start: w.start - off, end: w.end - off })
                .collect()
        });
        // samples carry their own timestamp so the fake decoder knows the offset
        let mut t = 0usize;
        while (t as f32) < total * RATE as f32 {
            let chunk: Vec<f32> = (t..t + 1600).map(|i| i as f32 / RATE as f32).collect();
            s.push(&chunk);
            t += 1600;
            s.step();
        }
        (s.finish(), s.stats)
    }

    #[test]
    fn overlap_stitch_has_no_gaps_or_dupes() {
        let (text, st) = run(StreamParams::OVERLAP);
        let want: Vec<String> = (0..60).map(|i| format!("w{i}")).collect();
        assert_eq!(text, want.join(" "));
        assert!(st.tail_audio < 10.0, "tail {}", st.tail_audio);
    }

    #[test]
    fn forced_cut_without_pauses() {
        // no gap ≥ 0.35 s in the script → pause mode must still cut (max_pending)
        let (text, st) = run(StreamParams::PAUSE);
        let want: Vec<String> = (0..60).map(|i| format!("w{i}")).collect();
        assert_eq!(text, want.join(" "));
        assert!(st.tail_audio < 20.0, "tail {}", st.tail_audio);
    }
}
