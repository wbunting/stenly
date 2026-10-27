//! Personal jargon: make the transcript use the user's spellings.
//!
//! Parakeet (a transducer) can't take a custom vocabulary, so this corrects the
//! text afterwards. Three kinds of term:
//!
//! - **The user's vocabulary** (`asr.vocabulary`). Same letters with different
//!   spacing/case ("View-On" → "Viewon"), a close sound-alike ("Vuong",
//!   "Viewon" → "Vuon"), and a coined name split into ordinary words ("view on"
//!   → "Vuon") when the surrounding words say it's a name, not the phrase.
//! - **The people in the meeting** (calendar invitees, the user's own name), so
//!   "Setarmpu" in a call with Sathiren becomes "Sathiren".
//! - **A short built-in list of AI/tech names** that speech recognition
//!   reliably mangles ("clawed" → "Claude", "NCP gateways" → "MCP gateways").
//!   These only fire on a known mishearing *and* a context gate, so "cloud"
//!   and "the cat clawed" stay put.
//!
//! It is deliberately conservative: precision beats recall. Ordinary English
//! words (a 60k-word dictionary) are never fuzzy-matched, people's names are
//! never replaced by other people's names, and every rewrite can be listed
//! ([`Vocabulary::apply`] returns them) to check it.

use crate::calendar::CalendarEvent;
use crate::transcript::Transcript;
use std::collections::HashSet;
use std::sync::OnceLock;

/// One rewrite, for reports and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Index into `Transcript::segments`.
    pub segment: usize,
    pub from: String,
    pub to: String,
    /// Which rule fired ("exact", "sound-alike", "split", "name", "built-in", …).
    pub rule: &'static str,
}

/// (from, to, rule) for one rewrite inside a segment.
type Rewrite = (String, String, &'static str);

/// The terms to enforce for one transcript.
#[derive(Debug, Clone, Default)]
pub struct Vocabulary {
    terms: Vec<Term>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// `asr.vocabulary`
    User,
    /// someone in this meeting
    Person,
    /// [`KNOWN`]
    Builtin,
}

#[derive(Debug, Clone)]
struct Term {
    text: String,
    /// lowercase letters/digits only
    squashed: String,
    key: String,
    /// each word, squashed
    words: Vec<String>,
    source: Source,
    /// Every word is ordinary English ("Forest", "May"): only spacing/hyphen
    /// fixes of a capitalised span, never fuzzy matches or lowercase rewrites.
    english: bool,
    /// built-in: known mishearings and their gates
    rules: &'static [Rule],
    /// built-in: also match a capitalised multi-word sound-alike ("Yan Lukun")
    fuzzy: bool,
    hint: Option<&'static str>,
    /// position in the input, for stable prompts
    order: usize,
}

impl Vocabulary {
    /// The user's terms plus the built-in AI/tech names.
    pub fn new(terms: &[String]) -> Self {
        Self::build(terms, &[])
    }

    /// For one meeting: also the invitees' names and the user's own.
    pub fn for_meeting(terms: &[String], event: Option<&CalendarEvent>, me: Option<&str>) -> Self {
        let mut people: Vec<String> = Vec::new();
        if let Some(ev) = event {
            people.extend(crate::speakers::invitees(ev));
        }
        people.extend(me.map(str::to_string));
        Self::build(terms, &people)
    }

    fn build(user: &[String], people: &[String]) -> Self {
        let mut terms: Vec<Term> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut add = |mut t: Term, terms: &mut Vec<Term>| {
            if seen.insert(t.squashed.clone()) {
                t.order = terms.len();
                terms.push(t);
            }
        };
        for u in user {
            if let Some(t) = Term::new(u, Source::User) {
                add(t, &mut terms);
            }
        }
        for p in people {
            let p = p.trim();
            // "Sathiren Vale" → also "Sathiren" and "Vale" on their own
            let parts: Vec<&str> = p.split_whitespace().collect();
            let mut variants = vec![p.to_string()];
            if parts.len() > 1 {
                variants.extend(parts.iter().map(|s| s.to_string()));
            }
            for v in variants {
                // a person's name only counts if it looks like one ("Info" from info@ doesn't)
                if !v.starts_with(char::is_uppercase) || v.contains('@') || MAILBOXES.contains(&squash(&v).as_str()) {
                    continue;
                }
                if let Some(t) = Term::new(&v, Source::Person) {
                    add(t, &mut terms);
                }
            }
        }
        for k in KNOWN {
            let mut t = Term::new(k.term, Source::Builtin).expect("built-in term");
            t.rules = k.rules;
            t.fuzzy = k.fuzzy;
            t.hint = Some(k.hint);
            add(t, &mut terms);
        }
        // longer terms first so "Viewon Cloud" wins over "Viewon"
        terms.sort_by_key(|t| std::cmp::Reverse(t.squashed.len()));
        Self { terms }
    }

    /// For a recorded session: the user's terms, the invitees and the user,
    /// plus (with `ctx`) the names and terms the meeting's context found ([`crate::context`]): people as people, companies
    /// and products as the user's own terms. The user's terms come first.
    pub fn for_session_with(cfg: &crate::config::Config, meta: &crate::session::SessionMeta, ctx: Option<&crate::context::MeetingContext>) -> Self {
        let me = crate::identity::Me::resolve(&cfg.me, meta.event.as_ref(), None);
        let Some(ctx) = ctx else { return Self::for_meeting(&cfg.asr.vocabulary, meta.event.as_ref(), me.display()) };
        let mut terms = cfg.asr.vocabulary.clone();
        terms.extend(ctx.vocabulary_terms());
        let mut people: Vec<String> = meta.event.as_ref().map(crate::speakers::invitees).unwrap_or_default();
        people.extend(me.display().map(str::to_string));
        people.extend(ctx.people_names());
        Self::build(&terms, &people)
    }

    /// For a session directory: its context file is read when there is one.
    pub fn for_session_dir(cfg: &crate::config::Config, meta: &crate::session::SessionMeta, dir: &std::path::Path) -> Self {
        Self::for_session_with(cfg, meta, crate::context::load(dir).as_ref())
    }

    /// Rewrite `t` in place; returns every change made.
    pub fn apply(&self, t: &mut Transcript) -> Vec<Change> {
        let texts: Vec<String> = t.segments.iter().map(|s| s.text.clone()).collect();
        let changes = self.correct_all(&texts);
        let mut out = Vec::new();
        for (i, (text, ch)) in changes.into_iter().enumerate() {
            t.segments[i].text = text;
            out.extend(ch.into_iter().map(|(from, to, rule)| Change { segment: i, from, to, rule }));
        }
        out
    }

    /// Correct one piece of text on its own (dictation, tests).
    pub fn correct(&self, text: &str) -> String {
        self.correct_all(&[text.to_string()]).pop().map(|(t, _)| t).unwrap_or_default()
    }

    fn correct_all(&self, texts: &[String]) -> Vec<(String, Vec<Rewrite>)> {
        let segs: Vec<Seg> = texts.iter().map(|t| Seg::new(t)).collect();
        // the two turns either side, for context gates
        let near = |r: std::ops::Range<usize>| -> Vec<String> { segs[r].iter().flat_map(|s| s.lower.iter().cloned()).collect() };
        let around: Vec<(Vec<String>, Vec<String>)> = (0..segs.len()).map(|i| (near(i.saturating_sub(2)..i), near((i + 1).min(segs.len())..(i + 3).min(segs.len())))).collect();
        let ctx = |i: usize| Ctx { seg: &segs[i], before: &around[i].0, after: &around[i].1 };
        // Pass 1: which terms the transcript names outright ("Vuon", "Viewon",
        // "Claude"). A split phrase ("view on") or a weak mishearing ("Quad")
        // is only read as the term when it is.
        let none = vec![false; self.terms.len()];
        let mut evidence = none.clone();
        for i in 0..segs.len() {
            for (_, _, ti, rule) in self.scan(&ctx(i), &none) {
                if matches!(rule, "exact" | "sound-alike" | "built-in") {
                    evidence[ti] = true;
                }
            }
        }
        (0..segs.len())
            .map(|i| {
                let c = ctx(i);
                let text = c.seg.text;
                let mut out = String::with_capacity(text.len());
                let mut cursor = 0;
                let mut changes = Vec::new();
                for (s, e, ti, rule) in self.scan(&c, &evidence) {
                    let span = &text[s..e];
                    let to = match_possessive(span, &self.terms[ti].text);
                    if to == span {
                        continue;
                    }
                    out.push_str(&text[cursor..s]);
                    out.push_str(&to);
                    changes.push((span.to_string(), to, rule));
                    cursor = e;
                }
                out.push_str(&text[cursor..]);
                (out, changes)
            })
            .collect()
    }

    /// Matches in one segment: (byte start, byte end, term index, rule).
    fn scan(&self, c: &Ctx, evidence: &[bool]) -> Vec<(usize, usize, usize, &'static str)> {
        let seg = c.seg;
        let mut out = Vec::new();
        let mut i = 0;
        while i < seg.toks.len() {
            match self.find(c, i, evidence) {
                Some((n, ti, rule)) => {
                    out.push((seg.toks[i].0, seg.toks[i + n - 1].1, ti, rule));
                    i += n;
                }
                None => i += 1,
            }
        }
        out
    }

    /// The term matching at token `i`: (tokens consumed, term index, rule).
    /// Exact spellings (and the built-ins' known mishearings) of every term
    /// are tried before any sound-alike, so a loose match on one term never
    /// pre-empts another term written correctly ("Satyam" stays Satyam when
    /// both Satyam and Sathiren are in the meeting).
    fn find(&self, c: &Ctx, i: usize, evidence: &[bool]) -> Option<(usize, usize, &'static str)> {
        self.terms.iter().enumerate().find_map(|(ti, t)| self.find_exact(c, i, ti, t, evidence)).or_else(|| {
            self.terms
                .iter()
                .enumerate()
                // a word that is (part of) another term is never a sound-alike of
                // this one; its own words are fine ("Yann Lukun" → "Yann LeCun")
                .filter(|&(ti, _)| !self.is_other_term_word(c.seg.word(i), ti))
                .find_map(|(ti, t)| self.find_alike(c, i, ti, t, evidence))
        })
    }

    /// The word spells a term other than `ti`, or a word of one.
    fn is_other_term_word(&self, w: &str, ti: usize) -> bool {
        let sq = squash(strip_possessive(w));
        self.terms.iter().enumerate().any(|(oi, t)| oi != ti && (t.squashed == sq || t.words.contains(&sq)))
    }

    /// Same letters as the term, or a built-in's known mishearing.
    fn find_exact(&self, c: &Ctx, i: usize, ti: usize, t: &Term, evidence: &[bool]) -> Option<(usize, usize, &'static str)> {
        let seg = c.seg;
        if t.source == Source::Builtin {
            return t
                .rules
                .iter()
                .find(|r| {
                    let n = r.heard.split(' ').count();
                    i + n <= seg.toks.len() && seg.joinable(i, n) && r.heard_matches(seg, i, n) && r.gate(c, i, n, t, evidence[ti])
                })
                .map(|r| (r.heard.split(' ').count(), ti, "built-in"));
        }
        // try joining up to words+1 transcript words (ASR splits "Viewon" into "view on")
        for n in (1..=(t.words.len() + 1).min(3)).rev() {
            if i + n > seg.toks.len() || !seg.joinable(i, n) {
                continue;
            }
            let sq = squash(strip_possessive(seg.span(i, n)));
            if sq != t.squashed {
                continue;
            }
            if t.words.len() == 1 && n == 2 && (0..n).all(|k| is_english(seg.word(i + k))) {
                // "view on" for "Viewon": same letters, but still a real phrase
                if split_score(c, i, true, evidence[ti]) >= SPLIT_THRESHOLD {
                    return Some((n, ti, "split"));
                }
                continue;
            }
            let capitalized = (0..n).all(|k| seg.word(i + k).starts_with(char::is_uppercase));
            if t.english && t.words.len() == 1 && !capitalized {
                // "forest" is not the person Forest, "may" not May
                continue;
            }
            return Some((n, ti, "exact"));
        }
        None
    }

    /// A sound-alike, misspelling or split-up version of the term.
    fn find_alike(&self, c: &Ctx, i: usize, ti: usize, t: &Term, evidence: &[bool]) -> Option<(usize, usize, &'static str)> {
        let seg = c.seg;
        let n_words = t.words.len();
        if n_words > 1 && (t.source != Source::Builtin || t.fuzzy) && multi_word_alike(seg, i, t) {
            return Some((n_words, ti, "sound-alike"));
        }
        if t.source == Source::Builtin || n_words > 1 {
            return None;
        }
        let base = strip_possessive(seg.word(i));
        if t.source == Source::Person && name_alike(seg, i, t) {
            return Some((1, ti, "name"));
        }
        // a misspelt name is written as a name: "Vuong", not "vuln"
        if base.starts_with(char::is_uppercase) && fuzzy(base, &t.squashed, &t.key) && !is_known_name(base) {
            return Some((1, ti, "sound-alike"));
        }
        if double_letters_alike(seg, i, t) {
            return Some((1, ti, "spelling"));
        }
        // a coined name split into a sound-alike phrase: "Vuon" heard as "view on"
        if t.source == Source::User && !t.english && i + 2 <= seg.toks.len() && seg.joinable(i, 2) && !self.is_other_term_word(seg.word(i + 1), ti) {
            let sq = squash(seg.span(i, 2));
            if sounds_like(&sq, &t.squashed, &t.key) && split_score(c, i, false, evidence[ti]) >= SPLIT_THRESHOLD {
                return Some((2, ti, "split"));
            }
        }
        None
    }

    /// The terms to put in a summary prompt for `t` (already corrected with
    /// [`Vocabulary::apply`]): the user's vocabulary, the people, and the
    /// built-in names this transcript mentions or still mishears (with how
    /// they tend to be misheard).
    pub fn hint_terms(&self, t: &Transcript) -> Vec<String> {
        let segs: Vec<Seg> = t.segments.iter().map(|s| Seg::new(&s.text)).collect();
        let mentions = |phrase: &str| {
            let words: Vec<String> = phrase.split(' ').map(squash).collect();
            let same = |w: &str, k: usize| squash(w) == words[k] || squash(strip_possessive(w)) == words[k];
            segs.iter().any(|s| (0..s.toks.len()).any(|i| i + words.len() <= s.toks.len() && (0..words.len()).all(|k| same(s.word(i + k), k))))
        };
        let mut out: Vec<String> = Vec::new();
        for src in [Source::User, Source::Person, Source::Builtin] {
            let mut of_src: Vec<&Term> = self.terms.iter().filter(|x| x.source == src).collect();
            // the order they were given in (`terms` is sorted longest-first)
            of_src.sort_by_key(|t| t.order);
            for term in of_src {
                match src {
                    // first/last names on their own are covered by the full name
                    Source::Person if self.terms.iter().any(|o| o.source == Source::Person && o.words.len() > 1 && o.words.contains(&term.squashed)) => {}
                    Source::Builtin => {
                        if mentions(&term.text) || term.rules.iter().any(|r| r.hint_worthy() && mentions(r.heard)) {
                            out.push(format!("{} ({})", term.text, term.hint.unwrap_or_default()));
                        }
                    }
                    _ => out.push(term.text.clone()),
                }
            }
        }
        out
    }
}

impl Term {
    fn new(text: &str, source: Source) -> Option<Self> {
        let text = text.trim();
        if text.chars().filter(|c| c.is_alphanumeric()).count() < 3 {
            return None;
        }
        let words: Vec<String> = text.split_whitespace().map(squash).filter(|w| !w.is_empty()).collect();
        Some(Self {
            text: text.to_string(),
            squashed: squash(text),
            key: phonetic(&squash(text)),
            english: words.iter().all(|w| is_english(w)),
            words,
            source,
            rules: &[],
            fuzzy: false,
            hint: None,
            order: 0,
        })
    }
}

// ---------------------------------------------------------------- tokens

/// One segment's words: byte spans plus lowercase copies.
struct Seg<'a> {
    text: &'a str,
    toks: Vec<(usize, usize)>,
    lower: Vec<String>,
}

impl<'a> Seg<'a> {
    fn new(text: &'a str) -> Self {
        let mut toks = Vec::new();
        let mut start = None;
        for (i, c) in text.char_indices() {
            let w = c.is_alphanumeric() || c == '\'' || c == '-';
            match (w, start) {
                (true, None) => start = Some(i),
                (false, Some(s)) => {
                    toks.push((s, i));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            toks.push((s, text.len()));
        }
        let lower = toks.iter().map(|&(s, e)| text[s..e].to_lowercase()).collect();
        Self { text, toks, lower }
    }

    fn word(&self, i: usize) -> &'a str {
        let (s, e) = self.toks[i];
        &self.text[s..e]
    }

    fn span(&self, i: usize, n: usize) -> &'a str {
        &self.text[self.toks[i].0..self.toks[i + n - 1].1]
    }

    /// Only plain spaces between tokens i..i+n (no punctuation).
    fn joinable(&self, i: usize, n: usize) -> bool {
        n == 1 || self.span(i, n).chars().all(|c| c.is_alphanumeric() || c == ' ' || c == '-' || c == '\'')
    }

    /// What separates token `i` from the one before it.
    fn gap_before(&self, i: usize) -> &'a str {
        if i == 0 { "" } else { &self.text[self.toks[i - 1].1..self.toks[i].0] }
    }

    /// Token `i` starts a sentence (segment start or after . ? !).
    fn sentence_start(&self, i: usize) -> bool {
        i == 0 || self.gap_before(i).contains(['.', '?', '!'])
    }

    /// The previous word, skipping fillers; `None` across a clause break.
    fn prev(&self, i: usize) -> Option<(usize, &str)> {
        let mut j = i;
        while j > 0 {
            if self.gap_before(j).contains(['.', '?', '!', ',', ';', ':']) {
                return None;
            }
            j -= 1;
            if !FILLERS.contains(&self.lower[j].as_str()) {
                return Some((j, self.lower[j].as_str()));
            }
        }
        None
    }

    /// The next word after token `i`, skipping fillers; `None` at a clause
    /// break or the end of the segment.
    fn next(&self, i: usize) -> Option<(usize, &str)> {
        let mut j = i + 1;
        while j < self.toks.len() {
            if self.gap_before(j).contains(['.', '?', '!', ',', ';', ':']) {
                return None;
            }
            if !FILLERS.contains(&self.lower[j].as_str()) {
                return Some((j, self.lower[j].as_str()));
            }
            j += 1;
        }
        None
    }
}

/// A segment and the lowercase words of its neighbours (the previous and
/// next turn), for context gates.
struct Ctx<'a> {
    seg: &'a Seg<'a>,
    before: &'a [String],
    after: &'a [String],
}

impl Ctx<'_> {
    /// Lowercase words within `w` of the span `i..i+n` (excluding it),
    /// running into the neighbouring turns.
    fn window(&self, i: usize, n: usize, w: usize) -> impl Iterator<Item = &str> {
        let cur = &self.seg.lower;
        let lo = i.saturating_sub(w);
        let hi = (i + n + w).min(cur.len());
        let from_before = w.saturating_sub(i).min(self.before.len());
        let from_after = (i + n + w).saturating_sub(cur.len()).min(self.after.len());
        self.before[self.before.len() - from_before..]
            .iter()
            .chain(&cur[lo..i])
            .chain(&cur[i + n..hi])
            .chain(&self.after[..from_after])
            .map(String::as_str)
    }
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn strip_possessive(s: &str) -> &str {
    s.strip_suffix("'s").or_else(|| s.strip_suffix("’s")).unwrap_or(s)
}

/// Keep a trailing "'s" the ASR attached ("Vuong's" → "Vuon's"), unless it
/// was part of the term all along ("I C's" → "ICs").
fn match_possessive(span: &str, term: &str) -> String {
    if strip_possessive(span) != span && !term.ends_with("'s") && squash(span) != squash(term) { format!("{term}'s") } else { term.to_string() }
}

// ---------------------------------------------------------------- word lists

/// Shared inboxes, not people ("info@acme.com" → "Info").
const MAILBOXES: &[&str] = &[
    "info", "hello", "hi", "team", "sales", "support", "help", "admin", "contact", "office", "noreply", "notifications", "calendar", "meetings", "events",
    "hr", "jobs", "careers", "billing", "accounts", "ops", "marketing", "partners", "press", "bookings", "reception",
];

use crate::dictation::FILLERS;

/// Words a fuzzy match must never eat that the dictionary doesn't cover.
const COMMON: &[&str] = &["von", "cody", "yeah", "okay", "ok", "gonna", "wanna", "gotta", "kinda", "sorta"];

/// Ordinary English (SCOWL size 50, ~60k words), lowercase.
fn english() -> &'static HashSet<&'static str> {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| word_list(include_str!("../data/english-words.txt")))
}

/// Common first names and surnames (SCOWL), lowercase.
fn known_names() -> &'static HashSet<&'static str> {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| word_list(include_str!("../data/proper-names.txt")))
}

fn word_list(s: &'static str) -> HashSet<&'static str> {
    s.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect()
}

fn is_english(w: &str) -> bool {
    let l = strip_possessive(w).to_lowercase();
    english().contains(l.as_str()) || COMMON.contains(&l.as_str())
}

fn is_known_name(w: &str) -> bool {
    known_names().contains(strip_possessive(w).to_lowercase().as_str())
}

// ---------------------------------------------------------------- sound-alikes

/// Rough phonetic key: collapse vowels and sound-alike consonants so
/// "vuon"/"vuong"/"voon" and "kody"/"kodi"/"codey" land close together.
fn phonetic(s: &str) -> String {
    let mut out = String::new();
    let mut last = '\0';
    for (i, c) in s.chars().enumerate() {
        let m = match c {
            'a' | 'e' | 'i' | 'o' | 'u' | 'y' | 'h' | 'w' => {
                if i == 0 {
                    'a'
                } else {
                    continue;
                }
            }
            'c' | 'k' | 'q' | 'g' => 'k',
            'b' | 'p' => 'p',
            'd' | 't' => 't',
            'f' | 'v' => 'f',
            's' | 'z' | 'x' => 's',
            'm' | 'n' => 'n',
            c => c,
        };
        if m != last {
            out.push(m);
        }
        last = m;
    }
    out
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + (ca != *cb) as usize).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// A single transcript word that is a misspelling or sound-alike of a term
/// (`term` squashed, `key` its phonetic key).
fn fuzzy(word: &str, term: &str, key: &str) -> bool {
    let sq = squash(word);
    // ALL-CAPS ("VON") is the recogniser guessing at an acronym, not the word
    let shouting = word.len() >= 3 && word.chars().all(|c| c.is_uppercase());
    if sq.len() < 3 || english().contains(sq.as_str()) || (!shouting && COMMON.contains(&sq.as_str())) || sq.chars().next() != term.chars().next() {
        return false;
    }
    let diff = (sq.len() as i64 - term.len() as i64).abs();
    let spelled = edit_distance(&sq, term);
    // close spelling ("Vuong" for "Vuon", "Parakit" for "Parakeet")
    if diff <= 1 && spelled <= if term.len() >= 6 { 2 } else { 1 } {
        return true;
    }
    // same sound, different spelling ("Viewon"/"Vueon" for "Vuon")
    sounds_like(&sq, term, key)
}

/// Same phonetic skeleton, same first letter, similar length.
fn sounds_like(word: &str, term: &str, key: &str) -> bool {
    let diff = (word.len() as i64 - term.len() as i64).abs();
    word.chars().next() == term.chars().next() && diff <= 2 && key.len() >= 2 && phonetic(word) == key && edit_distance(word, term) <= 3
}

/// A looser match for the name of someone in the meeting ("Setarmpu" for
/// "Sathiren"): an unknown capitalised word, not English and not another
/// name, with nearly the same consonants.
fn name_alike(seg: &Seg, i: usize, t: &Term) -> bool {
    let w = strip_possessive(seg.word(i));
    let sq = squash(w);
    if t.english || t.key.len() < 3 || sq.len() < 4 || !w.starts_with(char::is_uppercase) || is_english(w) || is_known_name(w) {
        return false;
    }
    if sq.chars().next() != t.squashed.chars().next() || (sq.len() as i64 - t.squashed.len() as i64).abs() > 2 {
        return false;
    }
    let k = phonetic(&sq);
    // the same consonant skeleton, give or take one sound at the end
    // ("Setarmpu" s-t-r-n-p ~ "Sathiren" s-t-r-n)
    let (short, long) = if k.len() <= t.key.len() { (&k, &t.key) } else { (&t.key, &k) };
    long.len() - short.len() <= 1 && long.starts_with(short.as_str()) && edit_distance(&sq, &t.squashed) <= t.squashed.len() * 3 / 4
}

/// "Forest" for the person "Forrest": a capitalised word mid-sentence that
/// only differs by a doubled letter.
fn double_letters_alike(seg: &Seg, i: usize, t: &Term) -> bool {
    let w = strip_possessive(seg.word(i));
    if t.source == Source::Builtin || t.english || !w.starts_with(char::is_uppercase) || seg.sentence_start(i) || squash(w).len() < 4 {
        return false;
    }
    let undouble = |s: &str| {
        let mut v: Vec<char> = squash(s).chars().collect();
        v.dedup();
        v.into_iter().collect::<String>()
    };
    squash(w) != t.squashed && undouble(w) == undouble(&t.text)
}

/// A capitalised multi-word sound-alike of a multi-word name ("Yan Lukun"
/// for "Yann LeCun", "Yves Dornica" for "Eve Dornica").
fn multi_word_alike(seg: &Seg, i: usize, t: &Term) -> bool {
    let n = t.words.len();
    if i + n > seg.toks.len() || !seg.joinable(i, n) {
        return false;
    }
    let words: Vec<String> = (0..n).map(|k| squash(strip_possessive(seg.word(i + k)))).collect();
    if !(0..n).all(|k| seg.word(i + k).starts_with(char::is_uppercase)) || words == t.words {
        return false;
    }
    let alike = |w: &str, tw: &str| w == tw || sounds_like(w, tw, &phonetic(tw)) || (w.len() >= 4 && fuzzy(w, tw, &phonetic(tw)));
    // the surname is distinctive: when it matches exactly, the first name only
    // needs to sound roughly the same ("Yves" / "Eve")
    let last_exact = words[n - 1] == t.words[n - 1] && t.words[n - 1].len() >= 5 && !is_english(&t.words[n - 1]);
    (0..n).all(|k| {
        alike(&words[k], &t.words[k]) || (last_exact && k < n - 1 && {
            let (a, b) = (phonetic(&words[k]), phonetic(&t.words[k]));
            a.chars().next() == b.chars().next() && edit_distance(&a, &b) <= 1
        })
    }) && (0..n).any(|k| !is_english(&words[k]))
}

// ---------------------------------------------------------------- "view on" → "Vuon"

const SPLIT_THRESHOLD: i32 = 3;

/// Whether the two words at `i` ("view on") are a split-up name rather than
/// the English phrase. ≥ [`SPLIT_THRESHOLD`] = the name.
///
/// "my view on it" / "a view on the bay" / "turn the view on": English.
/// "replace X with view on such that", "goes to view on.", "into View On and":
/// the name. Clues: what follows (an object of "on" means English; a verb,
/// preposition or the end of the clause means a name), what precedes it, a
/// capital mid-sentence, the same letters as the term, and whether the
/// transcript names the term outright elsewhere.
fn split_score(c: &Ctx, i: usize, exact_letters: bool, evidence: bool) -> i32 {
    let seg = c.seg;
    let prev = seg.prev(i);
    let next = seg.next(i + 1);
    // "my view on", "a different view on"
    if prev.is_some_and(|(_, p)| OPINION.contains(&p)) {
        return i32::MIN;
    }
    // "we pronounce it view view on"
    if prev.is_some_and(|(_, p)| p == "view") {
        return i32::MIN;
    }
    // "turn the view on", "keep that view on"
    let pp = prev.and_then(|(j, _)| seg.prev(j));
    if [prev, pp].iter().flatten().any(|(_, w)| PHRASAL.contains(w)) {
        return i32::MIN;
    }
    // "within View On You get…": a capitalised "You"/"The" mid-turn is the
    // recogniser starting a new sentence it forgot to punctuate
    let capital_name = seg.word(i).starts_with(char::is_uppercase) && !seg.sentence_start(i);
    let next = next.filter(|&(j, w)| !(capital_name && OBJECT.contains(&w) && seg.word(j).starts_with(char::is_uppercase) && !PLACES.contains(&w)));
    let mut score = 0;
    match next {
        // "view on the bay", "view on it", "view on LinkedIn"
        Some((_, w)) if OBJECT.contains(&w) => return i32::MIN,
        // "a view on to the street" (onto)
        Some((j, "to")) if seg.next(j).is_some_and(|(_, w)| OBJECT.contains(&w)) => return i32::MIN,
        // "with view on such that"
        Some((j, "such")) => {
            if seg.next(j).is_some_and(|(_, w)| w == "that") {
                score += 2
            }
        }
        Some((_, w)) if FOLLOWS_NAME.contains(&w) => score += 2,
        Some(_) => {}
        // end of clause: "goes to view on."
        None => score += 2,
    }
    if prev.is_some_and(|(_, p)| BEFORE_NAME.contains(&p)) {
        score += 1;
    }
    if seg.word(i).starts_with(char::is_uppercase) && !seg.sentence_start(i) {
        score += 2;
    }
    score + exact_letters as i32 + evidence as i32
}

/// Before "view on" these make it the English phrase.
const OPINION: &[&str] = &[
    "my", "your", "his", "her", "our", "their", "its", "whose", "one's", "different", "own", "personal", "overall", "honest", "general", "broader", "clear",
    "nice", "good", "great", "better", "best", "clearer", "full", "partial", "bird's-eye", "point",
];
/// "turn the view on" — the view being switched on.
const PHRASAL: &[&str] = &[
    "turn", "turned", "turns", "turning", "switch", "switched", "switches", "toggle", "toggled", "keep", "kept", "leave", "left", "put", "set", "flip", "flipped",
];
/// An object of "on": the English phrase ("view on the bay").
const OBJECT: &[&str] = &[
    "the", "a", "an", "this", "that", "these", "those", "it", "them", "him", "her", "me", "us", "you", "my", "your", "his", "its", "our", "their", "what",
    "how", "whether", "why", "which", "who", "whom", "some", "any", "each", "every", "all", "both", "either", "neither", "one", "two", "three", "several",
    "many", "most", "linkedin", "twitter", "x", "youtube", "screen", "demand", "mobile", "desktop", "top",
];
/// Objects of "on" that are names, so capitalised anyway.
const PLACES: &[&str] = &["linkedin", "twitter", "x", "youtube"];
/// After a name, not after "view on": verbs, prepositions, conjunctions.
const FOLLOWS_NAME: &[&str] = &[
    "in", "into", "to", "as", "for", "with", "from", "within", "inside", "via", "through", "at", "by", "instead", "because", "so", "and", "or", "but", "then",
    "that's", "which", "who", "where", "when", "is", "isn't", "has", "hasn't", "does", "doesn't", "did", "can", "can't", "could", "will", "won't", "would",
    "should", "might", "may", "was", "wasn't", "were", "becomes", "became", "gives", "makes", "helps", "lets", "needs", "sits", "works", "runs", "uses",
    "connects", "handles", "knows", "already", "also", "too", "internally", "basically", "actually", "now", "here", "there", "today", "first", "itself",
    "yeah",
];
/// Before a product name: "replace it with Vuon", "connect to Vuon".
const BEFORE_NAME: &[&str] = &["with", "to", "into", "in", "within", "via", "using", "use", "like", "through", "from", "about", "called", "onto"];

// ---------------------------------------------------------------- built-ins

/// A known mishearing of a built-in term and when to trust it.
#[derive(Debug)]
struct Rule {
    /// lowercase words as transcribed ("clawed", "open router", "i c's")
    heard: &'static str,
    /// an AI word nearby (model, Bedrock, LLM, prompt, …)
    ai: bool,
    /// an AI or data/tech word nearby
    tech: bool,
    /// every heard word capitalised ("Open Router", "Quad")
    cap: bool,
    /// the term itself is written correctly nearby ("ICs" … "Isis")
    near: bool,
    /// `ai` is also met when the transcript names the term elsewhere
    seen: bool,
    /// not at the start of a sentence
    mid: bool,
    /// the word just before or after is one of these
    beside: &'static [&'static str],
    /// the next word is one of these
    next: &'static [&'static str],
    /// the previous word is one of these
    prev: &'static [&'static str],
    not_next: &'static [&'static str],
    not_prev: &'static [&'static str],
}

const ANY: Rule = Rule { heard: "", ai: false, tech: false, cap: false, near: false, seen: false, mid: false, beside: &[], next: &[], prev: &[], not_next: &[], not_prev: &[] };

impl Rule {
    fn heard_matches(&self, seg: &Seg, i: usize, n: usize) -> bool {
        self.heard.split(' ').enumerate().all(|(k, h)| {
            let w = &seg.lower[i + k];
            w == h || (k == n - 1 && strip_possessive(w) == h)
        })
    }

    fn gate(&self, c: &Ctx, i: usize, n: usize, t: &Term, evidence: bool) -> bool {
        let seg = c.seg;
        if self.mid && seg.sentence_start(i) {
            return false;
        }
        let prev = seg.prev(i).map(|(_, w)| w);
        let next = seg.next(i + n - 1).map(|(_, w)| w);
        if self.cap && !(0..n).all(|k| seg.word(i + k).starts_with(char::is_uppercase)) {
            return false;
        }
        if next.is_some_and(|w| self.not_next.contains(&w)) || prev.is_some_and(|w| self.not_prev.contains(&w)) {
            return false;
        }
        if !self.next.is_empty() && !next.is_some_and(|w| self.next.contains(&w)) {
            return false;
        }
        if !self.prev.is_empty() && !prev.is_some_and(|w| self.prev.contains(&w)) {
            return false;
        }
        if !self.beside.is_empty() && ![prev, next].iter().flatten().any(|w| self.beside.contains(w)) {
            return false;
        }
        if self.ai && !(self.seen && evidence) && !c.window(i, n, 12).any(|w| AI.contains(&w)) {
            return false;
        }
        if self.tech && !c.window(i, n, 15).any(|w| AI.contains(&w) || TECH.contains(&w)) {
            return false;
        }
        if self.near {
            let me = t.text.to_lowercase();
            if !c.window(i, n, 40).any(|w| strip_possessive(w) == me) {
                return false;
            }
        }
        true
    }

    /// Seeing this in a transcript is worth a line in the summary prompt even
    /// when the gate didn't fire ("Hayreach", "I C's"; not "cloud", "quad").
    fn hint_worthy(&self) -> bool {
        !self.heard.split(' ').all(is_english)
    }
}

struct Known {
    term: &'static str,
    hint: &'static str,
    rules: &'static [Rule],
    fuzzy: bool,
}

const AI: &[&str] = &[
    "ai", "model", "models", "llm", "llms", "gpt", "chatgpt", "openai", "anthropic", "bedrock", "prompt", "prompts", "agent", "agents", "agentic", "gemini",
    "sonnet", "opus", "haiku", "mcp", "openrouter", "inference", "chatbot", "copilot", "claude", "codex", "vertex", "sagemaker", "tokens", "embeddings",
    "rag", "fine-tune", "fine-tuned", "fine-tuning",
];
const TECH: &[&str] = &[
    "data", "warehouse", "sql", "query", "queries", "databricks", "bigquery", "snowflake", "dbt", "semantic", "schema", "schemas", "tables", "pipeline",
    "pipelines", "etl", "redshift", "postgres", "api", "apis", "analytics", "lakehouse", "catalog", "gateway", "gateways", "server", "servers", "router",
    "routing", "aws", "azure", "gcp", "bi", "looker", "tableau", "cortex",
];
const MODEL_NAMES: &[&str] = &["gpt", "gpt-4", "gpt-4o", "gpt-5", "chatgpt", "openai", "gemini", "grok", "deepseek", "mistral", "llama", "anthropic", "sonnet", "opus", "codex", "copilot"];
const MCP_NOUNS: &[&str] = &["server", "servers", "gateway", "gateways", "tool", "tools", "client", "clients", "protocol", "integration", "integrations", "connector", "connectors"];

/// Common AI/tech names that speech recognition mangles. Each mishearing has
/// a gate; most need the right neighbours. Keep this short and conservative.
static KNOWN: &[Known] = &[
    Known {
        term: "Claude",
        hint: "Anthropic's AI model; sometimes transcribed \"clawed\" or \"Quad\"",
        fuzzy: false,
        rules: &[
            Rule { heard: "claude", ..ANY },
            // "the clawed model on Bedrock", not "the cat clawed at it"
            Rule {
                heard: "clawed",
                ai: true,
                not_next: &["back", "at", "its", "his", "her", "their", "my", "your", "our", "through", "way", "out", "up", "into", "off"],
                not_prev: &["it", "he", "she", "they", "cat", "cats", "dog", "dogs", "bear", "we", "i", "had", "was", "were", "been"],
                ..ANY
            },
            // "an instance of Quad", in a call that says Claude elsewhere
            // "just use clawed for the draft", in a call that says Claude elsewhere
            Rule { heard: "clawed", ai: true, seen: true, prev: &["use", "using", "used", "ask", "asking", "asked", "via", "tried", "try", "trying"], ..ANY },
            Rule { heard: "quad", ai: true, seen: true, cap: true, mid: true, not_next: &["core", "cores", "bike", "bikes", "copter", "copters", "gpu", "gpus", "chart"], ..ANY },
            // "cloud" is usually cloud: only in a list of models ("GPT cloud")
            Rule {
                heard: "cloud",
                beside: MODEL_NAMES,
                not_next: &[
                    "provider", "providers", "native", "connect", "storage", "data", "warehouse", "infrastructure", "environment", "account", "accounts",
                    "platform", "cost", "costs", "credits", "region", "instance", "hosting", "service", "services", "security", "console", "computing", "run",
                    "functions", "sql", "spend", "bill", "vendor", "vendors",
                ],
                ..ANY
            },
        ],
    },
    Known {
        term: "MCP",
        hint: "Model Context Protocol; sometimes transcribed \"NCP\"",
        fuzzy: false,
        rules: &[Rule { heard: "mcp", ..ANY }, Rule { heard: "ncp", next: MCP_NOUNS, ..ANY }, Rule { heard: "mpc", next: MCP_NOUNS, ..ANY }],
    },
    Known {
        term: "OpenRouter",
        hint: "an LLM router/API",
        fuzzy: false,
        rules: &[
            Rule { heard: "openrouter", ..ANY },
            Rule { heard: "open router", cap: true, ..ANY },
            Rule { heard: "open router", tech: true, not_prev: &["an"], not_next: &["port", "ports", "settings", "config", "admin", "page"], ..ANY },
        ],
    },
    Known {
        term: "OpenAI",
        hint: "the AI company",
        fuzzy: false,
        rules: &[Rule { heard: "openai", ..ANY }, Rule { heard: "open ai", cap: true, ..ANY }],
    },
    Known { term: "ChatGPT", hint: "OpenAI's chatbot", fuzzy: false, rules: &[Rule { heard: "chatgpt", ..ANY }, Rule { heard: "chat gpt", ..ANY }] },
    Known { term: "Anthropic", hint: "the AI company", fuzzy: false, rules: &[Rule { heard: "anthropic", ai: true, ..ANY }] },
    Known {
        term: "Databricks",
        hint: "data platform; sometimes transcribed \"Databreak\"",
        fuzzy: false,
        rules: &[
            Rule { heard: "databricks", ..ANY },
            Rule { heard: "databrick", ..ANY },
            Rule { heard: "data bricks", ..ANY },
            Rule { heard: "databreak", ..ANY },
            Rule { heard: "databreaks", ..ANY },
        ],
    },
    Known {
        term: "Snowflake",
        hint: "the data warehouse",
        fuzzy: false,
        // not the modelling term ("a star schema vs a snowflake schema")
        rules: &[Rule { heard: "snowflake", tech: true, not_next: &["schema", "schemas", "model", "models", "design", "designs", "pattern", "patterns", "dimension", "dimensions", "structure"], ..ANY }],
    },
    Known {
        term: "dbt",
        hint: "the data transformation tool, written lowercase",
        fuzzy: false,
        rules: &[Rule {
            heard: "dbt",
            beside: &[
                "semantic", "semantics", "model", "models", "cloud", "core", "project", "projects", "metrics", "labs", "snowflake", "databricks", "bigquery",
                "looker", "cube", "sql", "transformations", "jobs",
            ],
            ..ANY
        }],
    },
    Known { term: "BigQuery", hint: "Google's data warehouse", fuzzy: false, rules: &[Rule { heard: "bigquery", ..ANY }, Rule { heard: "big query", cap: true, ..ANY }] },
    Known { term: "DeepSeek", hint: "the AI lab and model", fuzzy: false, rules: &[Rule { heard: "deepseek", ..ANY }, Rule { heard: "deep seek", cap: true, ..ANY }] },
    Known { term: "LangChain", hint: "the LLM framework", fuzzy: false, rules: &[Rule { heard: "langchain", ..ANY }, Rule { heard: "lang chain", ..ANY }] },
    Known { term: "GitHub", hint: "code hosting", fuzzy: false, rules: &[Rule { heard: "github", ..ANY }, Rule { heard: "git hub", ..ANY }] },
    Known {
        term: "HeyReach",
        hint: "LinkedIn outreach tool; sometimes transcribed \"Hayreach\"",
        fuzzy: false,
        rules: &[
            Rule { heard: "heyreach", ..ANY },
            Rule { heard: "hayreach", ..ANY },
            Rule { heard: "hay reach", ..ANY },
            Rule { heard: "hey reach", cap: true, ..ANY },
        ],
    },
    Known {
        term: "ICs",
        hint: "individual contributors; sometimes transcribed \"I C's\" or \"Isis\"",
        fuzzy: false,
        rules: &[Rule { heard: "i c's", ..ANY }, Rule { heard: "i cs", ..ANY }, Rule { heard: "isis", near: true, ..ANY }],
    },
    Known { term: "Jevons", hint: "as in the Jevons paradox; sometimes transcribed \"Jev\"", fuzzy: false, rules: &[Rule { heard: "jev", cap: true, mid: true, ai: true, ..ANY }] },
    Known { term: "Yann LeCun", hint: "AI researcher", fuzzy: true, rules: &[] },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(s: &str, terms: &[&str]) -> String {
        Vocabulary::new(&terms.iter().map(|t| t.to_string()).collect::<Vec<_>>()).correct(s)
    }

    fn fix_with_people(s: &str, terms: &[&str], people: &[&str]) -> String {
        let p: Vec<String> = people.iter().map(|t| t.to_string()).collect();
        Vocabulary::build(&terms.iter().map(|t| t.to_string()).collect::<Vec<_>>(), &p).correct(s)
    }

    /// Several turns, as one transcript.
    fn fix_turns(turns: &[&str], terms: &[&str]) -> Vec<String> {
        let mut t = Transcript {
            segments: turns.iter().map(|s| crate::transcript::Segment::new(crate::platform::Channel::Mic, 0.0, 0.0, *s)).collect(),
            ..Default::default()
        };
        Vocabulary::new(&terms.iter().map(|t| t.to_string()).collect::<Vec<_>>()).apply(&mut t);
        t.segments.into_iter().map(|s| s.text).collect()
    }

    #[test]
    fn joins_split_words() {
        assert_eq!(fix("let us check how view on handles it", &["Viewon"]), "let us check how Viewon handles it");
        assert_eq!(fix("the export runs through View-On first", &["Viewon"]), "the export runs through Viewon first");
        assert_eq!(fix("ask stenly about it", &["Stenly"]), "ask Stenly about it");
    }

    #[test]
    fn fixes_sound_alikes() {
        assert_eq!(fix("we tested Vuong today", &["Vuon"]), "we tested Vuon today");
        assert_eq!(fix("Vuong's settings", &["Vuon"]), "Vuon's settings");
        assert_eq!(fix("then sync with Kodi first", &["Kody"]), "then sync with Kody first");
        assert_eq!(fix("Parakit is fast", &["Parakeet"]), "Parakeet is fast");
        // the recogniser's all-caps guess at an acronym
        assert_eq!(fix("it was slow before VON comes online", &["Vuon"]), "it was slow before Vuon comes online");
        assert_eq!(fix("a von Neumann machine", &["Vuon"]), "a von Neumann machine");
        // lowercase jargon isn't a misspelt name
        assert_eq!(fix("security scans, vuln reports and patching", &["Vuon"]), "security scans, vuln reports and patching");
    }

    #[test]
    fn leaves_ordinary_words_alone() {
        assert_eq!(fix("a nice view on the bay, a few on the list", &["Vuon"]), "a nice view on the bay, a few on the list");
        assert_eq!(fix("the code body", &["Kody"]), "the code body");
        assert_eq!(fix("my view on it", &["Vuon"]), "my view on it");
        assert_eq!(fix("my view on it", &["Viewon"]), "my view on it");
        // don't join across punctuation
        assert_eq!(fix("the view. On Monday", &["Viewon"]), "the view. On Monday");
    }

    #[test]
    fn synthetic_transcript_lines() {
        let t = ["Vuon", "Dylan Hartley", "Shelley", "Forest"];
        // Synthetic example: the company is Vuon, pronounced "view-on"
        assert_eq!(fix("put the Viewon files in there", &t), "put the Vuon files in there");
        assert_eq!(fix("the Viewon settings. And it will", &t), "the Vuon settings. And it will");
        assert_eq!(fix("this Vueon export tool", &t), "this Vuon export tool");
        assert_eq!(fix("the export runs through View on first", &t), "the export runs through Vuon first");
        // ordinary English stays
        assert_eq!(fix("let us check how view on works, my view on it", &t), "let us check how view on works, my view on it");
        assert_eq!(fix("that's the one", &t), "that's the one");
        assert_eq!(fix("a viewer opens", &t), "a viewer opens");
        // "Shelley" never eats "surely"
        assert_eq!(fix("surely that works", &t), "surely that works");
    }

    #[test]
    fn multi_word_terms() {
        assert_eq!(fix("try the snow flake cortex demo", &["Snowflake Cortex"]), "try the Snowflake Cortex demo");
    }

    /// "view on" spoken as the product (synthetic lines).
    #[test]
    fn view_on_as_the_name() {
        let v = &["Vuon"];
        // the transcript names Vuon outright elsewhere
        let call = |line: &str| fix_turns(&["Vuon came up earlier in the call.", line], v).pop().unwrap();
        assert_eq!(call("we could replace the old spreadsheet with view on such that everyone sees it"), "we could replace the old spreadsheet with Vuon such that everyone sees it");
        assert_eq!(call("Okay, so then it goes to view on."), "Okay, so then it goes to Vuon.");
        assert_eq!(call("the glossary probably needs to live in view on in some form."), "the glossary probably needs to live in Vuon in some form.");
        assert_eq!(call("Um the notes in view on to be able to search them"), "Um the notes in Vuon to be able to search them");
        assert_eq!(call("they would connect to view on as a plugin"), "they would connect to Vuon as a plugin");
        assert_eq!(call("a tool in the middle, which is view on, to help with that."), "a tool in the middle, which is Vuon, to help with that.");
        assert_eq!(call("something like a view on to basically check the draft"), "something like a Vuon to basically check the draft");
        assert_eq!(call("hire a team that is like building a view on internally."), "hire a team that is like building a Vuon internally.");
        assert_eq!(call("so uh view on has a simple setup"), "so uh Vuon has a simple setup");
        assert_eq!(call("importing the files into View On and it can"), "importing the files into Vuon and it can");
        assert_eq!(call("Got it. And then so from a storage perspective within View On"), "Got it. And then so from a storage perspective within Vuon");
        assert_eq!(fix("we want to sync to View on that's the plan.", v), "we want to sync to Vuon that's the plan.");
        assert_eq!(call("from a storage perspective within View On You keep a copy"), "from a storage perspective within Vuon You keep a copy");
        // single-token sound-alikes, as before
        assert_eq!(fix("Viewon ships a CLI and the script calls ViewOn through it", v), "Vuon ships a CLI and the script calls Vuon through it");
    }

    /// "view on" as ordinary English, even in a call about Vuon.
    #[test]
    fn view_on_as_english() {
        let v = &["Vuon"];
        let call = |line: &str| fix_turns(&["Vuon came up earlier in the call.", line], v).pop().unwrap();
        for s in [
            "what's your view on pricing?",
            "I have a different view on that.",
            "a nice view on the bay",
            "my view on it is simple",
            "you can view on demand",
            "files you can view on your phone",
            "turn the view on and off",
            "keep that view on.",
            "a view on to the street",
            "We can give you a view on an account.",
            "we should fix the view on LinkedIn.",
            "his view on whether it works",
            "the view on The Strand",
        ] {
            assert_eq!(call(s), s);
        }
        // without the name elsewhere, a lowercase "view on" needs more than a verb after it
        assert_eq!(fix("let us check how view on works", v), "let us check how view on works");
    }

    #[test]
    fn claude_not_cloud() {
        assert_eq!(fix("In the demo, Bedrock hosts the clawed model.", &[]), "In the demo, Bedrock hosts the Claude model.");
        assert_eq!(fix("so you set it up. uh clawed on bedrock works", &[]), "so you set it up. uh Claude on bedrock works");
        assert_eq!(fix("The team runs an instance of Quad on Bedrock as the model", &[]), "The team runs an instance of Claude on Bedrock as the model");
        assert_eq!(fix("comparing GPT cloud and Gemini outputs", &[]), "comparing GPT Claude and Gemini outputs");
        // the real word
        assert_eq!(fix("it's a direct cloud connection for the whole office", &[]), "it's a direct cloud connection for the whole office");
        assert_eq!(fix("we host it in the cloud, like Databricks or Snowflake do", &[]), "we host it in the cloud, like Databricks or Snowflake do");
        assert_eq!(fix("a private cloud connect as opposed to on-prem", &[]), "a private cloud connect as opposed to on-prem");
        assert_eq!(fix("GPT cloud provider costs", &[]), "GPT cloud provider costs");
        assert_eq!(fix("the cat clawed at the model railway", &[]), "the cat clawed at the model railway");
        assert_eq!(fix("the bonus was clawed back from the agents", &[]), "the bonus was clawed back from the agents");
        // no AI nearby: leave it
        assert_eq!(fix("The team runs an instance of Quad there as well.", &[]), "The team runs an instance of Quad there as well.");
        assert_eq!(fix("the dog clawed the door", &[]), "the dog clawed the door");
        // "use clawed" only once the call has said Claude
        let out = fix_turns(&["The agent runs the clawed model on Bedrock.", "Next we covered the schedule.", "a", "b", "I could just use clawed for the draft."], &[]);
        assert_eq!(out[4], "I could just use Claude for the draft.");
        assert_eq!(fix("I could just use clawed for the draft.", &[]), "I could just use clawed for the draft.");
    }

    #[test]
    fn built_in_tech_names() {
        assert_eq!(fix("think of it as a open router for NCP servers.", &[]), "think of it as a OpenRouter for MCP servers.");
        assert_eq!(fix("an NCP certificate", &[]), "an NCP certificate");
        assert_eq!(fix("the export from Hayreach finished", &[]), "the export from HeyReach finished");
        assert_eq!(fix("open Hayreach's settings page", &[]), "open HeyReach's settings page");
        assert_eq!(fix("the team has four I C's I think", &[]), "the team has four ICs I think");
        assert_eq!(fix("we load the DBT models into the warehouse", &[]), "we load the dbt models into the warehouse");
        assert_eq!(fix("she was offered DBT for anxiety", &[]), "she was offered DBT for anxiety");
        assert_eq!(fix("tools like Databreak, Snowflake, that's easy", &[]), "tools like Databricks, Snowflake, that's easy");
        assert_eq!(fix("pick Snowflake and a BI tool or snowflake and a warehouse", &[]), "pick Snowflake and a BI tool or Snowflake and a warehouse");
        assert_eq!(fix("a snowflake fell on the window", &[]), "a snowflake fell on the window");
        assert_eq!(fix("a star schema or a snowflake schema in the warehouse", &[]), "a star schema or a snowflake schema in the warehouse");
        assert_eq!(fix("normalised into a snowflake model with dimension tables", &[]), "normalised into a snowflake model with dimension tables");
        assert_eq!(fix("a classic Jev paradox for AI models", &[]), "a classic Jevons paradox for AI models");
        assert_eq!(fix("the old Jev bridge near the station", &[]), "the old Jev bridge near the station");
        assert_eq!(fix("Yan Lukun gave the keynote.", &[]), "Yann LeCun gave the keynote.");
        // first name right, surname misheard
        assert_eq!(fix("a talk by Yann Lukun today", &[]), "a talk by Yann LeCun today");
        assert_eq!(fix("because Yan Lacoon is on the panel.", &[]), "because Yann LeCun is on the panel.");
        assert_eq!(fix("an open router in the office", &[]), "an open router in the office");
    }

    #[test]
    fn isis_only_next_to_ics() {
        let out = fix_turns(&["We hired two ICs.", "Okay, got it.", "Next quarter. Or two more Isis."], &[]);
        assert_eq!(out[2], "Next quarter. Or two more ICs.");
        assert_eq!(fix("the temple of Isis in Egypt", &[]), "the temple of Isis in Egypt");
    }

    #[test]
    fn people_in_the_meeting() {
        let people = &["Sathiren Vale", "Shelley Morgan", "Andy"];
        assert_eq!(fix_with_people("She's like, Setarmpu, can you share the doc.", &[], people), "She's like, Sathiren, can you share the doc.");
        assert_eq!(fix_with_people("and Yves Dornica, she's joining us", &["Eve Dornica"], &[]), "and Eve Dornica, she's joining us");
        assert_eq!(fix_with_people("and Eve Dornika, she's joining us", &["Eve Dornica"], &[]), "and Eve Dornica, she's joining us");
        // ordinary words and other people's names stay
        assert_eq!(fix_with_people("surely, Seth was asking about it", &[], people), "surely, Seth was asking about it");
        assert_eq!(fix_with_people("the station and the sandy beach", &[], people), "the station and the sandy beach");
        assert_eq!(fix_with_people("And Shelley said", &[], people), "And Shelley said");
    }

    #[test]
    fn names_never_replace_other_terms() {
        // two invitees whose names sound alike
        let people = &["Sathiren Vale", "Satyam Rao"];
        assert_eq!(fix_with_people("Satyam thinks Setarmpu has the file", &[], people), "Satyam thinks Sathiren has the file");
        // an exact user term beats a loose invitee match
        assert_eq!(fix_with_people("ask Stenly about it", &["Stenly"], &["Sathiren"]), "ask Stenly about it");
        // and a longer term's sound-alike never eats a shorter exact one
        assert_eq!(fix("Vuon and Vuong", &["Vuon", "Vuongo"]), "Vuon and Vuongo");
        assert_eq!(fix("Kody and Kodiak and Kodi", &["Kody", "Kodiak"]), "Kody and Kodiak and Kody");
        // similar invitee names, the longer one tried first
        let pair = &["Deepika Rao", "Deepak Shah"];
        assert_eq!(fix_with_people("Deepak said he'd send it", &[], pair), "Deepak said he'd send it");
        assert_eq!(fix_with_people("Deepak's notes and Deepika's", &[], pair), "Deepak's notes and Deepika's");
        assert_eq!(fix_with_people("Deepka said she'd send it", &[], pair), "Deepika said she'd send it");
    }

    #[test]
    fn forrest_not_forest() {
        let t = &["Forrest"];
        assert_eq!(fix("Okay, Forest and I can pair on that.", t), "Okay, Forrest and I can pair on that.");
        assert_eq!(fix("after Forest's holiday", t), "after Forrest's holiday");
        // sentence-initial "Forest" might be a mishearing of anything ("Forget about…")
        assert_eq!(fix("never mind. Forest about the old plan", t), "never mind. Forest about the old plan");
        assert_eq!(fix("a walk in the forest", t), "a walk in the forest");
        // a term that is an ordinary word never rewrites the lowercase word
        assert_eq!(fix("a walk in the forest", &["Forest"]), "a walk in the forest");
    }

    #[test]
    fn meeting_vocabulary_from_invite() {
        use crate::calendar::Attendee;
        let a = |name: Option<&str>, email: &str| Attendee { name: name.map(str::to_string), email: email.into(), status: Some("ACCEPTED".into()) };
        let ev = CalendarEvent {
            id: "x".into(),
            uid: "x".into(),
            title: "Connect".into(),
            start: chrono::Utc::now(),
            end: chrono::Utc::now(),
            link: None,
            organizer: Some(a(Some("Sathiren Vale"), "sathiren.vale@guest.example.org")),
            attendees: vec![a(None, "alex@example.com"), a(Some("Shelley Morgan <shelley@example.com>"), "shelley@example.com"), a(None, "info@acme.com")],
            description: None,
            calendar: Some("alex@example.com".into()),
            declined: false,
            recurring: false,
            html_link: None,
            calendar_name: None,
            color: None,
        };
        let v = Vocabulary::for_meeting(&["Vuon".into()], Some(&ev), None);
        assert_eq!(v.correct("Thanks, Setarmpu, for the notes"), "Thanks, Sathiren, for the notes");
        let mut t = Transcript { segments: vec![crate::transcript::Segment::new(crate::platform::Channel::Mic, 0.0, 0.0, "we tried the clawed model on Bedrock")], ..Default::default() };
        // callers hint from the corrected transcript
        v.apply(&mut t);
        let hint = v.hint_terms(&t);
        assert!(hint.contains(&"Vuon".to_string()), "{hint:?}");
        assert!(hint.contains(&"Sathiren Vale".to_string()), "{hint:?}");
        assert!(hint.contains(&"Shelley Morgan".to_string()), "{hint:?}");
        assert!(!hint.iter().any(|h| h == "Sathiren" || h == "Info"), "{hint:?}");
        assert!(hint.iter().any(|h| h.starts_with("Claude (")), "{hint:?}");
        assert!(!hint.iter().any(|h| h.starts_with("HeyReach")), "{hint:?}");
    }

    #[test]
    fn reports_changes() {
        let mut t = Transcript { segments: vec![crate::transcript::Segment::new(crate::platform::Channel::Mic, 0.0, 0.0, "the Viewon settings")], ..Default::default() };
        let ch = Vocabulary::new(&["Vuon".into()]).apply(&mut t);
        assert_eq!(ch, vec![Change { segment: 0, from: "Viewon".into(), to: "Vuon".into(), rule: "sound-alike" }]);
    }
}
