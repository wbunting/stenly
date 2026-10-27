//! Local meeting library. Disk reads run off the UI thread; only visible rows
//! are rendered. Transcript/summary bodies are read only for search or opening.
use crate::text_input::{InputEvent, TextInput};
use crate::{theme, ui};
use gpui::{
    AnyElement, AsyncApp, Context, Entity, EventEmitter, Focusable, Subscription, Task, Window,
    div, prelude::*, px,
};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use stenly_core::config::Config;
use stenly_mcp::store::Store;

const PAGE: usize = 50;

pub struct OpenMeeting {
    pub dir: PathBuf,
    pub segment: Option<usize>,
}

struct Row {
    dir: PathBuf,
    title: String,
    detail: String,
    segment: Option<usize>,
}

pub struct MeetingBrowser {
    config: Config,
    query: Entity<TextInput>,
    rows: Vec<Row>,
    limit: usize,
    more: bool,
    loading: bool,
    task: Option<Task<()>>,
    cancelled: Arc<AtomicBool>,
    _sub: Subscription,
}

impl EventEmitter<OpenMeeting> for MeetingBrowser {}

impl MeetingBrowser {
    pub fn new(config: Config, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| TextInput::new(cx, "Search title, attendees, notes or transcript"));
        let sub = cx.subscribe(&query, |this: &mut Self, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Changed) {
                this.limit = PAGE;
                this.rows.clear();
                this.load(true, cx);
            }
        });
        let mut this = Self {
            config,
            query,
            rows: vec![],
            limit: PAGE,
            more: false,
            loading: true,
            task: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            _sub: sub,
        };
        this.load(false, cx);
        this
    }

    pub fn refresh(&mut self, config: Config, cx: &mut Context<Self>) {
        self.config = config;
        self.load(false, cx);
    }

    pub fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.query.read(cx).focus_handle(cx), cx);
    }

    fn load(&mut self, debounce: bool, cx: &mut Context<Self>) {
        let query = self.query.read(cx).text().trim().to_string();
        let config = self.config.clone();
        let limit = self.limit;
        self.cancelled.store(true, Ordering::Relaxed);
        self.cancelled = Arc::new(AtomicBool::new(false));
        let cancelled = self.cancelled.clone();
        self.loading = true;
        cx.notify();
        // Dropping the old task cancels its delivery, so an older query cannot
        // overwrite newer results even if an iCloud read finishes much later.
        self.task = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            if debounce {
                cx.background_executor()
                    .timer(Duration::from_millis(220))
                    .await;
            }
            let rows = cx
                .background_spawn(async move {
                    let store = Store::new(config);
                    if query.is_empty() {
                        store
                            .meetings_cancellable(|| cancelled.load(Ordering::Relaxed))
                            .into_iter()
                            .take(limit + 1)
                            .map(|m| Row {
                                title: m.meta.title.clone(),
                                detail: format!(
                                    "{} · {}",
                                    m.meta.started.format("%b %-d, %Y · %-I:%M %p"),
                                    match m.meta.state {
                                        stenly_core::session::SessionState::Done => "Note ready",
                                        stenly_core::session::SessionState::Failed => "Failed",
                                        stenly_core::session::SessionState::Recording =>
                                            "Recording",
                                        _ => "Processing",
                                    }
                                ),
                                dir: m.dir,
                                segment: None,
                            })
                            .collect::<Vec<_>>()
                    } else {
                        store
                            .search_cancellable(&query, None, None, limit + 1, || {
                                cancelled.load(Ordering::Relaxed)
                            })
                            .into_iter()
                            .map(|h| Row {
                                dir: store.audio_dir.join(h.meeting_id),
                                title: h.title,
                                detail: format!(
                                    "{} · {}{} — {}",
                                    h.date,
                                    h.field,
                                    h.at.map(|at| format!(" · {at}")).unwrap_or_default(),
                                    h.text.replace('\n', " ")
                                ),
                                segment: h.segment_index,
                            })
                            .collect()
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.more = rows.len() > limit;
                this.rows = rows.into_iter().take(limit).collect();
                this.loading = false;
                this.task = None;
                cx.notify();
            });
        }));
    }
}

impl Drop for MeetingBrowser {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl Render for MeetingBrowser {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let searching = !self.query.read(cx).text().trim().is_empty();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(ui::text_field(self.query.clone()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(10.))
                    .child(
                        div()
                            .text_size(px(theme::SUBHEAD))
                            .text_color(theme::muted())
                            .child(if self.loading {
                                "Searching this Mac…".to_string()
                            } else if self.rows.is_empty() {
                                if searching {
                                    "No matching meetings."
                                } else {
                                    "Notes from your meetings will appear here."
                                }
                                .to_string()
                            } else {
                                format!(
                                    "{} {}{}",
                                    self.rows.len(),
                                    if searching {
                                        if self.rows.len() == 1 {
                                            "match"
                                        } else {
                                            "matches"
                                        }
                                    } else {
                                        "meetings"
                                    },
                                    if self.more { " shown" } else { "" }
                                )
                            }),
                    )
                    .when(self.more && !self.loading, |d| {
                        d.child(
                            ui::button(
                                "more-meetings",
                                if searching {
                                    "Show more matches"
                                } else {
                                    "Show older meetings"
                                },
                                ui::Btn::Ghost,
                            )
                            .role(gpui::Role::Button)
                            .aria_label(if searching {
                                "Show more matches"
                            } else {
                                "Show older meetings"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.limit += PAGE;
                                this.load(false, cx);
                            })),
                        )
                    }),
            )
            .when(!self.rows.is_empty(), |d| {
                d.child(
                    gpui::uniform_list(
                        "meeting-library",
                        self.rows.len(),
                        cx.processor(|this, range: std::ops::Range<usize>, _: &mut Window, cx| {
                            range
                                .map(|i| {
                                    let row = &this.rows[i];
                                    let (dir, segment) = (row.dir.clone(), row.segment);
                                    div()
                                        .id(i)
                                        .role(gpui::Role::Button)
                                        .aria_label(format!("Open {}", row.title))
                                        .aria_description(row.detail.clone())
                                        .w_full()
                                        .h(px(66.))
                                        .px(px(14.))
                                        .flex()
                                        .flex_col()
                                        .justify_center()
                                        .gap(px(3.))
                                        .cursor_pointer()
                                        .hover(|d| d.bg(theme::selected()))
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.emit(OpenMeeting {
                                                dir: dir.clone(),
                                                segment,
                                            })
                                        }))
                                        .child(
                                            div()
                                                .text_size(px(theme::BODY))
                                                .font_weight(gpui::FontWeight::MEDIUM)
                                                .truncate()
                                                .child(row.title.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(theme::SUBHEAD))
                                                .text_color(theme::muted())
                                                .truncate()
                                                .child(row.detail.clone()),
                                        )
                                        .into_any_element()
                                })
                                .collect::<Vec<AnyElement>>()
                        }),
                    )
                    .h(px(396.))
                    .rounded(px(12.))
                    .bg(theme::card()),
                )
            })
    }
}
