//! Search palette: a floating panel over the main window (⌘F / ⌘K, or the
//! magnifier beside the traffic lights). Empty query lists recent meetings;
//! typing searches titles, attendees, notes and transcripts off the UI thread.
use crate::icons::{self, icon};
use crate::meeting_browser::OpenMeeting;
use crate::text_input::{self, InputEvent, TextInput};
use crate::{theme, ui};
use gpui::{
    App, AsyncApp, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding, ScrollHandle,
    Subscription, Task, Window, actions, div, prelude::*, px,
};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use stenly_core::config::Config;
use stenly_mcp::store::Store;

actions!(search_palette, [Dismiss]);

const RECENT: usize = 8;
const LIMIT: usize = 40;
const PANEL_RADIUS: f32 = 12.;
const LIST_PAD: f32 = 6.;

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Dismiss, Some("SearchPalette"))]);
}

pub enum PaletteEvent {
    Open(OpenMeeting),
    Dismiss,
}

#[derive(Clone)]
struct Hit {
    dir: PathBuf,
    title: String,
    day: String,
    /// Where it matched: None for recents, else "title", "summary", "note", "transcript".
    field: Option<&'static str>,
    snippet: Option<String>,
    segment: Option<usize>,
}

pub struct SearchPalette {
    config: Config,
    pub(crate) query: Entity<TextInput>,
    hits: Vec<Hit>,
    selected: usize,
    loading: bool,
    task: Option<Task<()>>,
    cancelled: Arc<AtomicBool>,
    scroll: ScrollHandle,
    _sub: Subscription,
}

impl EventEmitter<PaletteEvent> for SearchPalette {}

impl Focusable for SearchPalette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }
}

impl SearchPalette {
    pub fn new(config: Config, cx: &mut Context<Self>) -> Self {
        let query =
            cx.new(|cx| TextInput::new(cx, "Search meetings, attendees, notes or transcripts"));
        let sub = cx.subscribe(&query, |this: &mut Self, _, ev: &InputEvent, cx| match ev {
            InputEvent::Changed => this.load(true, cx),
            InputEvent::Submit => this.confirm(cx),
        });
        let mut this = Self {
            config,
            query,
            hits: vec![],
            selected: 0,
            loading: true,
            task: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            scroll: ScrollHandle::new(),
            _sub: sub,
        };
        this.load(false, cx);
        this
    }

    fn query_text(&self, cx: &App) -> String {
        self.query.read(cx).text().trim().to_string()
    }

    fn load(&mut self, debounce: bool, cx: &mut Context<Self>) {
        let query = self.query_text(cx);
        let config = self.config.clone();
        self.cancelled.store(true, Ordering::Relaxed);
        self.cancelled = Arc::new(AtomicBool::new(false));
        let cancelled = self.cancelled.clone();
        self.loading = true;
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            if debounce {
                cx.background_executor()
                    .timer(Duration::from_millis(160))
                    .await;
            }
            let hits = cx
                .background_spawn(async move {
                    let today = chrono::Local::now().date_naive();
                    let store = Store::new(config);
                    if query.is_empty() {
                        store
                            .meetings_cancellable(|| cancelled.load(Ordering::Relaxed))
                            .into_iter()
                            .take(RECENT)
                            .map(|m| Hit {
                                title: m.meta.title.clone(),
                                day: ui::day_label(m.meta.started.date_naive(), today),
                                dir: m.dir,
                                field: None,
                                snippet: None,
                                segment: None,
                            })
                            .collect::<Vec<_>>()
                    } else {
                        let mut hits: Vec<Hit> = store
                            .search_cancellable(&query, None, None, LIMIT, || {
                                cancelled.load(Ordering::Relaxed)
                            })
                            .into_iter()
                            .map(|h| Hit {
                                dir: store.audio_dir.join(&h.meeting_id),
                                day: chrono::NaiveDateTime::parse_from_str(
                                    &h.date,
                                    "%Y-%m-%d %H:%M",
                                )
                                .map(|d| ui::day_label(d.date(), today))
                                .unwrap_or_else(|_| h.date.clone()),
                                title: h.title,
                                snippet: (h.field != "title").then(|| {
                                    let text =
                                        h.text.split_whitespace().collect::<Vec<_>>().join(" ");
                                    match h.at {
                                        Some(at) => format!("{at} · {text}"),
                                        None => text,
                                    }
                                }),
                                field: Some(h.field),
                                segment: h.segment_index,
                            })
                            .collect();
                        // Title matches first (as their own section), then mentions.
                        hits.sort_by_key(|h| h.field != Some("title"));
                        hits
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.hits = hits;
                this.selected = 0;
                this.loading = false;
                this.task = None;
                this.scroll.scroll_to_item(0);
                cx.notify();
            });
        }));
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(h) = self.hits.get(self.selected) {
            cx.emit(PaletteEvent::Open(OpenMeeting {
                dir: h.dir.clone(),
                segment: h.segment,
            }));
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.hits.is_empty() {
            return;
        }
        let n = self.hits.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
        if let Some(child) = self.child_index(self.selected) {
            self.scroll.scroll_to_item(child);
        }
        cx.notify();
    }

    /// Section of a hit, shown as a header above the first hit of each.
    fn section(&self, h: &Hit) -> &'static str {
        match h.field {
            None => "Recent meetings",
            Some("title") => "Meetings",
            Some(_) => "Notes & transcripts",
        }
    }

    /// Position of hit `i` among the list's children (headers included).
    fn child_index(&self, i: usize) -> Option<usize> {
        let mut child = 0;
        let mut last = None;
        for (j, h) in self.hits.iter().enumerate() {
            let s = self.section(h);
            if last != Some(s) {
                child += 1;
                last = Some(s);
            }
            if j == i {
                return Some(child);
            }
            child += 1;
        }
        None
    }
}

impl Drop for SearchPalette {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl Render for SearchPalette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let searching = !self.query_text(cx).is_empty();
        let mut list: Vec<gpui::AnyElement> = vec![];
        let mut last = None;
        for (i, h) in self.hits.iter().enumerate() {
            let s = self.section(h);
            if last != Some(s) {
                last = Some(s);
                list.push(
                    div()
                        .px(px(10.))
                        .pt(px(if i == 0 { 2. } else { 10. }))
                        .pb(px(4.))
                        .text_size(px(theme::SUBHEAD))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::faint())
                        .child(s)
                        .into_any_element(),
                );
            }
            let active = i == self.selected;
            let glyph = match h.field {
                None | Some("title") => icons::WAVE,
                Some("transcript") => icons::QUOTE,
                Some(_) => icons::FILE_TEXT,
            };
            list.push(
                div()
                    .id(("hit", i))
                    .role(gpui::Role::Button)
                    .aria_label(format!("Open {}", h.title))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(10.))
                    .py(px(7.))
                    // Concentric with the panel (12pt corner, 6pt list padding).
                    .rounded(px(theme::concentric(PANEL_RADIUS, LIST_PAD)))
                    .cursor_pointer()
                    .when(active, |d| d.bg(theme::selected()))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.selected != i {
                            this.selected = i;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = i;
                        this.confirm(cx);
                    }))
                    .child(
                        div()
                            .w(px(18.))
                            .flex()
                            .justify_center()
                            .flex_shrink_0()
                            .child(icon(glyph, 14., theme::muted())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .child(
                                div()
                                    .text_size(px(theme::BODY))
                                    .text_color(theme::text())
                                    .truncate()
                                    .child(h.title.clone()),
                            )
                            .when_some(h.snippet.clone(), |d, s| {
                                d.child(
                                    div()
                                        .text_size(px(theme::SUBHEAD))
                                        .text_color(theme::muted())
                                        .truncate()
                                        .child(s),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(theme::SUBHEAD))
                            .text_color(theme::faint())
                            .child(h.day.clone()),
                    )
                    .into_any_element(),
            );
        }
        let status = if self.hits.is_empty() {
            Some(if self.loading {
                "Searching this Mac…"
            } else if searching {
                "No matching meetings."
            } else {
                "Notes from your meetings will appear here."
            })
        } else {
            None
        };
        div()
            .id("search-palette")
            .key_context("SearchPalette")
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(PaletteEvent::Dismiss)))
            // Up/Down move the selection instead of the field's caret.
            .capture_action(cx.listener(|this, _: &text_input::Up, _, cx| {
                this.move_selection(-1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &text_input::Down, _, cx| {
                this.move_selection(1, cx);
                cx.stop_propagation();
            }))
            .occlude()
            // Clicks inside the panel must not reach the dismissing backdrop.
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .w(px(560.))
            .max_w_full()
            .flex()
            .flex_col()
            .rounded(px(PANEL_RADIUS))
            .bg(theme::palette_bg())
            .border_1()
            .border_color(theme::divider())
            .shadow_lg()
            .font_family(theme::SANS)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(46.))
                    .px(px(16.))
                    .border_b_1()
                    .border_color(theme::divider())
                    .text_size(px(theme::TITLE3))
                    .child(icon(icons::SEARCH, 15., theme::muted()))
                    .child(div().flex_1().min_w_0().child(self.query.clone())),
            )
            .child(
                div()
                    .id("search-results")
                    .max_h(px(380.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .p(px(LIST_PAD))
                    .when_some(status, |d, s| {
                        d.child(
                            div()
                                .px(px(10.))
                                .py(px(10.))
                                .text_size(px(theme::BODY))
                                .text_color(theme::muted())
                                .child(s),
                        )
                    })
                    .children(list),
            )
    }
}
