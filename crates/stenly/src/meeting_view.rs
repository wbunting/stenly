//! Read view for one meeting (Granola-style): serif title, metadata chips,
//! topic sections with nested bullets, decisions, action items, transcript.

use crate::icons::{self, icon};
use crate::theme;
use crate::ui;
use gpui::{AnyElement, AppContext, Div, FontWeight, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px};
use std::path::PathBuf;
use stenly_core::platform::Channel;
use stenly_core::session::{SessionMeta, SessionState};
use stenly_core::summary::Summary;
use stenly_core::transcript::Transcript;
use stenly_core::util::fmt_timestamp;

#[derive(Clone)]
pub struct MeetingDetail {
    pub dir: PathBuf,
    pub meta: SessionMeta,
    pub transcript: Option<Transcript>,
    pub summary: Option<Summary>,
    /// Recipes run on this meeting (newest last).
    pub results: Vec<stenly_core::recipe::RecipeResult>,
}

impl MeetingDetail {
    pub fn load(dir: PathBuf) -> Option<Self> {
        let meta = SessionMeta::load(&dir).ok()?;
        let (transcript, summary) = stenly_core::session::load_detail(&dir);
        let results = stenly_core::recipe::load_results(&dir);
        Some(Self { dir, meta, transcript, summary, results })
    }
}

fn chip(data: crate::icons::Sym, text: impl Into<SharedString>) -> Div {
    div().flex().items_center().gap(px(4.)).text_size(px(theme::BODY)).text_color(theme::muted()).child(icon(data, 12., theme::muted())).child(text.into())
}

fn heading(text: impl Into<SharedString>) -> Div {
    div().pt(px(22.)).pb(px(4.)).text_size(px(theme::TITLE3)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(text.into())
}

fn bullet(text: impl Into<SharedString>, level: u8) -> Div {
    let (dot, indent): (Div, f32) = if level == 0 {
        (div().mt(px(8.)).size(px(5.)).rounded_full().bg(theme::muted()), 0.)
    } else {
        (div().mt(px(8.)).size(px(5.)).rounded_full().border_1().border_color(theme::muted()), 18.)
    };
    div()
        .w_full()
        .flex()
        .items_start()
        .gap(px(10.))
        .pl(px(indent))
        .py(px(3.))
        .child(dot.flex_shrink_0())
        .child(div().flex_1().min_w_0().text_size(px(14.)).line_height(px(21.)).text_color(theme::text()).child(text.into()))
}

fn task(owner: Option<&str>, text: &str) -> Div {
    let owner = owner.filter(|o| !o.is_empty()).map(|o| match o {
        "Me" => "You".to_string(),
        o => o.to_string(),
    });
    let styled = match &owner {
        Some(o) => gpui::StyledText::new(format!("{o}: {text}")).with_highlights([(0..o.len() + 1, gpui::HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() })]),
        None => gpui::StyledText::new(text.to_string()),
    };
    div()
        .w_full()
        .flex()
        .items_start()
        .gap(px(10.))
        .py(px(4.))
        // NSButton checkbox: 14pt, 3.5pt corners.
        .child(div().mt(px(3.5)).size(px(14.)).flex_shrink_0().rounded(px(3.5)).border_1().border_color(theme::faint()).bg(theme::inset()))
        .child(div().flex_1().min_w_0().text_size(px(14.)).line_height(px(21.)).text_color(theme::text()).child(styled))
}

/// `speakers` and `tools` (template + recipe pickers) go under the title;
/// `extras` (recipe results) between the notes and the transcript.
pub fn render(
    d: &MeetingDetail,
    show_transcript: bool,
    matched_segment: Option<(usize, gpui::ScrollAnchor)>,
    speakers: Option<AnyElement>,
    on_toggle_transcript: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
    summary_action: &'static str,
    on_summarize: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
    tools: Option<AnyElement>,
    extras: Vec<AnyElement>,
) -> AnyElement {
    let m = &d.meta;
    let title = m.title.clone();
    let mins = m.duration_mins().unwrap_or(0);
    let attendees = m.others().len();
    let platform = m.event.as_ref().and_then(|e| e.link.as_ref()).map(|l| l.provider.label());

    let mut body = div().w_full().flex().flex_col();

    // ---- summary
    match (&d.summary, m.state) {
        (Some(s), _) => {
            if !s.summary.trim().is_empty() {
                body = body.child(div().w_full().pt(px(16.)).text_size(px(14.)).line_height(px(21.)).text_color(theme::muted()).child(s.summary.trim().to_string()));
            }
            for sec in s.sections.iter().filter(|x| !x.bullets.is_empty()) {
                body = body.child(heading(sec.heading.trim().to_string()));
                for b in &sec.bullets {
                    body = body.child(bullet(b.text().trim().to_string(), 0));
                    for sub in b.sub() {
                        body = body.child(bullet(sub.trim().to_string(), 1));
                    }
                }
            }
            if s.sections.is_empty() && !s.key_points.is_empty() {
                body = body.child(heading("Key points"));
                for p in &s.key_points {
                    body = body.child(bullet(p.clone(), 0));
                }
            }
            if !s.decisions.is_empty() && s.sections.is_empty() {
                body = body.child(heading("Decisions"));
                for p in &s.decisions {
                    body = body.child(bullet(p.clone(), 0));
                }
            }
            if !s.action_items.is_empty() {
                body = body.child(heading("Action items"));
                for a in &s.action_items {
                    body = body.child(task(a.owner.as_deref(), &a.task));
                }
            }
        }
        (None, SessionState::Done) => {
            let words = d.transcript.as_ref().map(|t| t.word_count()).unwrap_or(0);
            body = body.child(
                div()
                    .mt(px(18.))
                    .px(px(14.))
                    .py(px(12.))
                    .rounded(px(10.))
                    .bg(theme::card())
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(ui::icon_tile(if words == 0 { icons::MIC } else { icons::SPARKLES }))
                    .child(
                        div().flex_1().flex().flex_col().gap(px(1.)).child(div().text_size(px(theme::BODY)).font_weight(FontWeight::MEDIUM).child(if words == 0 { "No speech was captured" } else { "Summary not generated" })).child(
                            div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(if words == 0 {
                                "Nothing intelligible was heard on either channel."
                            } else {
                                "Your transcript and note were saved. Set up a model whenever you’re ready."
                            }),
                        ),
                    )
                    .when(words > 0, |d| d.child(ui::button("summarize-now", summary_action, crate::ui::Btn::Outline).on_click(on_summarize))),
            );
        }
        (None, _) => {
            body = body.child(
                div()
                    .mt(px(22.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .text_size(px(14.))
                    .text_color(theme::muted())
                    .child(ui::spinner("detail-spin", 15., theme::muted()))
                    .child(if m.state == SessionState::Recording { "Recording in progress…" } else { "Writing up your notes…" }),
            );
        }
    }

    body = body.children(extras);

    // ---- transcript
    if let Some(t) = d.transcript.as_ref().filter(|t| !t.is_empty()) {
        body = body.child(
            div()
                .id("toggle-transcript")
                .mt(px(26.))
                .flex()
                .items_center()
                .gap(px(4.))
                .text_size(px(theme::BODY))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(icon(if show_transcript { icons::CHEVRON_DOWN } else { icons::CHEVRON_RIGHT }, 10., theme::muted()))
                .child(format!("Transcript"))
                .child(div().font_weight(FontWeight::NORMAL).text_color(theme::muted()).child(format!("{} words", t.word_count())))
                .on_click(on_toggle_transcript),
        );
        if show_transcript {
            let mut prev: Option<(Channel, Option<u32>)> = None;
            let mut tr = div().flex().flex_col().gap(px(2.)).pt(px(10.));
            let voices = t.voices();
            for (index, s) in t.segments.iter().enumerate() {
                let key = (s.channel, s.speaker);
                let new_speaker = prev != Some(key);
                prev = Some(key);
                let who = s.display_label();
                let color = voice_color(&voices, s.channel, s.speaker);
                tr = tr.child(
                    div().id(("transcript-segment", index))
                        .when(matched_segment.as_ref().is_some_and(|(i, _)| *i == index), |d| d.bg(theme::selected()).anchor_scroll(matched_segment.as_ref().map(|(_, a)| a.clone())))
                        .flex()
                        .gap(px(12.))
                        .pt(px(if new_speaker { 10. } else { 0. }))
                        .child(div().w(px(42.)).flex_shrink_0().pt(px(2.)).text_size(px(theme::SUBHEAD)).text_color(theme::faint()).font_features(gpui::FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]))).child(fmt_timestamp(s.start)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .when(new_speaker, |d| {
                                    d.child(div().text_size(px(theme::SUBHEAD)).font_weight(FontWeight::SEMIBOLD).text_color(color).child(who.clone()))
                                })
                                .child(div().min_w_0().text_size(px(theme::BODY)).line_height(px(19.)).text_color(theme::text()).child(s.text.clone())),
                        ),
                );
            }
            body = body.child(tr);
        }
    }

    div()
        .w_full()
        .flex()
        .flex_col()
        .pt(px(4.))
        .child(div().text_size(px(theme::LARGE_TITLE)).line_height(px(32.)).font_weight(FontWeight::BOLD).text_color(theme::text()).child(title))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_x(px(14.))
                .gap_y(px(4.))
                .pt(px(6.))
                .child(chip(icons::CALENDAR, format!("{}{}", m.started.format("%b %-d · %-I:%M %p"), if mins > 0 { format!(" · {mins} min") } else { String::new() })))
                .when(attendees > 0, |d| d.child(chip(icons::USERS, attendees.to_string())))
                .when_some(platform, |d, p| d.child(chip(icons::VIDEO, p)))
                .when_some(d.summary.as_ref().filter(|s| !s.tags.is_empty()), |d, s| d.child(chip(icons::TAG, s.tags.iter().take(3).cloned().collect::<Vec<_>>().join(", ")))),
        )
        .children(speakers)
        .children(tools)
        .child(body)
        .into_any_element()
}

/// One colour per voice, in order of first appearance; the recorder is always green.
fn voice_color(voices: &[(Channel, Option<u32>, String)], ch: Channel, sp: Option<u32>) -> gpui::Rgba {
    if ch == Channel::Mic && sp.is_none() {
        return theme::green();
    }
    const PALETTE: [fn() -> gpui::Rgba; 6] = [theme::blue, theme::orange, theme::indigo, theme::pink, theme::teal, theme::amber];
    let i = voices.iter().filter(|(c, s, _)| !(*c == Channel::Mic && s.is_none())).position(|(c, s, _)| *c == ch && *s == sp).unwrap_or(0);
    PALETTE[i % PALETTE.len()]()
}

// ---------------------------------------------------------------- speakers strip

/// Who spoke in this meeting, with rename. Lives as its own entity so the
/// meeting view stays a plain function of `MeetingDetail`.
pub struct SpeakerEditor {
    dir: PathBuf,
    transcript: Option<Transcript>,
    /// Voice index whose field should open on next render (dev hook).
    pending_edit: Option<usize>,
    /// The voice being renamed, and its text field.
    editing: Option<(Channel, Option<u32>, gpui::Entity<crate::text_input::TextInput>)>,
    error: Option<SharedString>,
    _subs: Vec<gpui::Subscription>,
}

pub enum SpeakerEditorEvent {
    /// A speaker was renamed on disk; reload the meeting.
    Renamed,
}

impl gpui::EventEmitter<SpeakerEditorEvent> for SpeakerEditor {}

impl SpeakerEditor {
    pub fn new(dir: PathBuf) -> Self {
        // Dev: STENLY_EDIT_SPEAKER=n opens the rename field of the n-th voice (1-based).
        let pending_edit = std::env::var("STENLY_EDIT_SPEAKER").ok().and_then(|v| v.parse::<usize>().ok()).map(|n| n.saturating_sub(1));
        Self { dir, transcript: None, pending_edit, editing: None, error: None, _subs: Vec::new() }
    }

    /// The transcript the strip shows (cheap to call every frame).
    pub fn set_transcript(&mut self, t: Option<Transcript>, cx: &mut gpui::Context<Self>) {
        if self.transcript != t {
            self.transcript = t;
            cx.notify();
        }
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    fn start_edit(&mut self, ch: Channel, sp: Option<u32>, current: Option<String>, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        let input = cx.new(|cx| {
            let mut t = crate::text_input::TextInput::new(cx, "Name");
            if let Some(c) = current {
                t.set_text(c, cx);
            }
            t
        });
        let sub = cx.subscribe(&input, move |this: &mut Self, input, ev: &crate::text_input::InputEvent, cx| {
            if matches!(ev, crate::text_input::InputEvent::Submit) {
                let name = input.read(cx).text().trim().to_string();
                this.commit(ch, sp, name, cx);
            }
        });
        let fh = gpui::Focusable::focus_handle(input.read(cx), cx);
        window.focus(&fh, cx);
        let blur = cx.on_blur(&fh, window, |this: &mut Self, _, cx| {
            // clicking elsewhere cancels, like Finder's rename
            this.editing = None;
            cx.notify();
        });
        self._subs = vec![sub, blur];
        self.editing = Some((ch, sp, input));
        self.error = None;
        cx.notify();
    }

    fn commit(&mut self, ch: Channel, sp: Option<u32>, name: String, cx: &mut gpui::Context<Self>) {
        self.editing = None;
        self._subs.clear();
        let dir = self.dir.clone();
        let cfg = stenly_core::config::Config::load(&stenly_core::config::config_path()).unwrap_or_default();
        match stenly_core::speakers::pipeline::rename_in_session(&cfg, &dir, ch, sp, &name) {
            Ok(()) => cx.emit(SpeakerEditorEvent::Renamed),
            Err(e) => self.error = Some(format!("Couldn't rename: {e:#}").into()),
        }
        cx.notify();
    }

    /// Whether there's anything to show: more than "You" and "Them".
    pub fn has_strip(t: &Transcript) -> bool {
        let voices = t.voices();
        !voices.is_empty() && (voices.len() > 2 || t.segments.iter().any(|s| s.speaker.is_some() || s.name.is_some()))
    }

    /// The strip under the title: one pill per voice; click to rename.
    fn render_strip(&self, t: &Transcript, cx: &mut gpui::Context<Self>) -> AnyElement {
        let voices = t.voices();
        if !Self::has_strip(t) {
            return div().into_any_element();
        }
        let mut row = div().flex().flex_wrap().items_center().gap_x(px(6.)).gap_y(px(6.)).pt(px(12.));
        for (i, (ch, sp, _)) in voices.iter().enumerate() {
            let (ch, sp) = (*ch, *sp);
            let info0 = t.speaker_info(ch, sp);
            // you: your mic on a call, or (in a room) the voice labelled as you
            let me = (ch == Channel::Mic && sp.is_none() && !t.in_person) || info0.is_some_and(|i| i.source == Some(stenly_core::transcript::NameSource::Recorder));
            let label = t.segments.iter().find(|s| s.channel == ch && s.speaker == sp).map(|s| s.display_label()).unwrap_or_default();
            let info = t.speaker_info(ch, sp);
            let guessed = info.is_some_and(|i| matches!(i.source, Some(stenly_core::transcript::NameSource::Llm)));
            let color = voice_color(&voices, ch, sp);
            let editing = self.editing.as_ref().filter(|(c, s, _)| *c == ch && *s == sp).map(|(_, _, e)| e.clone());
            let pill = div()
                .id(("speaker", i))
                .h(px(24.))
                .flex()
                .items_center()
                .gap(px(6.))
                .pl(px(8.))
                .pr(px(10.))
                .rounded(px(12.))
                .bg(theme::inset())
                .border_1()
                .border_color(theme::divider())
                .text_size(px(theme::CALLOUT))
                .child(div().size(px(7.)).rounded_full().flex_shrink_0().bg(color));
            let el = match editing {
                Some(input) => pill.w(px(160.)).child(div().flex_1().min_w_0().child(input)).into_any_element(),
                None if me => pill
                    .text_color(theme::text())
                    .child(label)
                    .tooltip(|_, cx| cx.new(|_| Tip("You. Change how your lines are labelled in Settings → You → Shown as.".into())).into())
                    .into_any_element(),
                None => {
                    let current = info.and_then(|i| i.name.clone());
                    let tip: SharedString = match info.and_then(|i| i.source) {
                        Some(stenly_core::transcript::NameSource::Accessibility) => "Named from the meeting app. Click to rename.".into(),
                        Some(stenly_core::transcript::NameSource::Voiceprint) => "Recognised from an earlier meeting. Click to rename.".into(),
                        Some(stenly_core::transcript::NameSource::Calendar) => "The other person invited. Click to rename.".into(),
                        Some(stenly_core::transcript::NameSource::Llm) => "Suggested from the conversation. Click to confirm or rename.".into(),
                        Some(stenly_core::transcript::NameSource::User | stenly_core::transcript::NameSource::Recorder) => "Click to rename.".into(),
                        None => "Click to name this speaker.".into(),
                    };
                    let tip: SharedString = match info.and_then(|i| i.person.as_deref()) {
                        Some(p) => format!("{p} on the invite. {tip}").into(),
                        None => tip,
                    };
                    pill.cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .text_color(theme::text())
                        .child(label)
                        .when(guessed, |d| d.child(div().text_color(theme::faint()).child("?")))
                        .tooltip(move |_, cx| cx.new(|_| Tip(tip.clone())).into())
                        .on_click(cx.listener(move |this, _, window, cx| this.start_edit(ch, sp, current.clone(), window, cx)))
                        .into_any_element()
                }
            };
            row = row.child(el);
        }
        if let Some(e) = &self.error {
            row = row.child(div().text_size(px(theme::SUBHEAD)).text_color(theme::red()).child(e.clone()));
        }
        row.into_any_element()
    }
}

impl gpui::Render for SpeakerEditor {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let Some(t) = self.transcript.clone() else { return div().into_any_element() };
        if let Some(n) = self.pending_edit.take() {
            if let Some((ch, sp, _)) = t.voices().get(n).cloned() {
                let current = t.speaker_info(ch, sp).and_then(|i| i.name.clone());
                self.start_edit(ch, sp, current, window, cx);
            }
        }
        self.render_strip(&t, cx)
    }
}

struct Tip(SharedString);
impl gpui::Render for Tip {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(320.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(6.))
            .bg(theme::popup_bg())
            .border_1()
            .border_color(theme::divider())
            .shadow_md()
            .text_size(px(theme::SUBHEAD))
            .text_color(theme::text())
            .font_family(theme::SANS)
            .child(self.0.clone())
    }
}

/// Plain-text / Markdown versions for the clipboard.
pub fn notes_markdown(d: &MeetingDetail) -> String {
    use std::fmt::Write;
    let mut md = String::new();
    let _ = writeln!(md, "# {}\n", d.meta.title);
    let _ = writeln!(md, "{}\n", d.meta.started.format("%a %b %-d, %Y · %-I:%M %p"));
    match &d.summary {
        Some(s) => {
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
            if s.sections.is_empty() {
                for p in &s.key_points {
                    let _ = writeln!(md, "- {p}");
                }
            }
            if !s.action_items.is_empty() {
                md.push_str("### Action items\n");
                for a in &s.action_items {
                    match a.owner.as_deref().filter(|o| !o.is_empty()) {
                        Some(o) => {
                            let _ = writeln!(md, "- [ ] {}: {}", if o == "Me" { "You" } else { o }, a.task);
                        }
                        None => {
                            let _ = writeln!(md, "- [ ] {}", a.task);
                        }
                    }
                }
            }
        }
        None => md.push_str("_No AI notes for this meeting._\n"),
    }
    md.trim_end().to_string()
}

pub fn transcript_text(d: &MeetingDetail) -> String {
    let Some(t) = &d.transcript else { return String::new() };
    t.segments
        .iter()
        .map(|s| format!("[{}] {}: {}", fmt_timestamp(s.start), s.display_label(), s.text))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn obsidian_url(d: &MeetingDetail) -> Option<String> {
    d.meta.note_path.as_ref().map(|p| format!("obsidian://open?path={}", urlencode(&p.to_string_lossy())))
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
