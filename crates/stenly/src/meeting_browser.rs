//! Local meeting library. Disk reads run off the UI thread; only visible rows
//! are rendered. Searching lives in the search palette (`search_palette.rs`).
use crate::{theme, ui};
use gpui::{AnyElement, AsyncApp, Context, EventEmitter, Task, Window, div, prelude::*, px};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
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
    rows: Vec<Row>,
    limit: usize,
    more: bool,
    loading: bool,
    task: Option<Task<()>>,
    cancelled: Arc<AtomicBool>,
}

impl EventEmitter<OpenMeeting> for MeetingBrowser {}

impl MeetingBrowser {
    pub fn new(config: Config, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            config,
            rows: vec![],
            limit: PAGE,
            more: false,
            loading: true,
            task: None,
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        this.load(cx);
        this
    }

    pub fn refresh(&mut self, config: Config, cx: &mut Context<Self>) {
        self.config = config;
        self.load(cx);
    }

    fn load(&mut self, cx: &mut Context<Self>) {
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
            let rows = cx
                .background_spawn(async move {
                    let store = Store::new(config);
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
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(10.))
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
                            .child(if self.rows.is_empty() {
                                if self.loading {
                                    "Loading meetings…"
                                } else {
                                    "Notes from your meetings will appear here."
                                }
                                .to_string()
                            } else {
                                format!("{} meetings{}", self.rows.len(), if self.more { " shown" } else { "" })
                            }),
                    )
                    .when(self.more && !self.loading, |d| {
                        d.child(
                            ui::button("more-meetings", "Show older meetings", ui::Btn::Ghost)
                                .role(gpui::Role::Button)
                                .aria_label("Show older meetings")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.limit += PAGE;
                                    this.load(cx);
                                })),
                        )
                    }),
            )
            .when(!self.rows.is_empty(), |d| {
                d.child(
                    // Keep the rectangular scroll viewport inside the rounded
                    // card, including when a row is partially scrolled out.
                    div().rounded(px(12.)).bg(theme::card()).p(px(4.)).child(
                        gpui::uniform_list(
                            "meeting-library",
                            self.rows.len(),
                            cx.processor(
                                |this, range: std::ops::Range<usize>, _: &mut Window, cx| {
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
                                                // GPUI overflow masks are rectangular, so the
                                                // hover surface needs its own concentric corners.
                                                .rounded(px(8.))
                                                .px(px(10.))
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
                                },
                            ),
                        )
                        .h(px(388.)),
                    ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, point};

    #[gpui::test]
    fn hover_stays_inside_rounded_card_at_both_ends(cx: &mut TestAppContext) {
        for count in [1, 3, 12] {
            let (_, cx) = cx.add_window_view(move |_, cx| {
                let mut browser = MeetingBrowser::new(Config::default(), cx);
                // Cancel the disk task before it runs; all rows are authored here.
                browser.cancelled.store(true, Ordering::Relaxed);
                browser.task = None;
                browser.loading = false;
                browser.rows = (0..count)
                    .map(|i| Row {
                        dir: PathBuf::from(format!("synthetic-{i}")),
                        title: format!("Synthetic meeting {i}"),
                        detail: "Synthetic fixture".into(),
                        segment: None,
                    })
                    .collect();
                browser
            });
            cx.run_until_parked();
            let card = cx.update(|window, _| {
                window
                    .painted_quads()
                    .into_iter()
                    .find(|q| q.background == theme::card().into())
                    .expect("meeting card is painted")
            });
            let scale = cx.update(|window, _| window.scale_factor());
            assert_eq!(card.bounds.size.height.0, 396. * scale);
            let x = card.bounds.origin.x.0 / scale + 20.;
            for bottom in [false, true] {
                let y = if bottom {
                    card.bounds.origin.y.0 / scale + 4. + (count as f32 * 66.).min(388.) - 10.
                } else {
                    card.bounds.origin.y.0 / scale + 14.
                };
                let position = point(px(x), px(y));
                if bottom && count > 6 {
                    cx.simulate_event(ScrollWheelEvent {
                        position,
                        delta: ScrollDelta::Pixels(point(px(0.), px(-10_000.))),
                        ..Default::default()
                    });
                }
                cx.simulate_mouse_move(position, None, Modifiers::none());
                cx.run_until_parked();
                cx.update(|window, _| {
                    let hover = window
                        .painted_quads()
                        .into_iter()
                        .find(|q| q.background == theme::selected().into())
                        .expect("hover highlight is painted");
                    for radius in [
                        hover.corner_radii.top_left,
                        hover.corner_radii.top_right,
                        hover.corner_radii.bottom_left,
                        hover.corner_radii.bottom_right,
                    ] {
                        assert_eq!(radius.0, 8. * scale);
                    }
                    let visible = hover.bounds.intersect(&hover.content_mask.bounds);
                    assert!(visible.origin.x.0 >= card.bounds.origin.x.0 + 4. * scale);
                    assert!(visible.origin.y.0 >= card.bounds.origin.y.0 + 4. * scale);
                    assert!(visible.right().0 <= card.bounds.right().0 - 4. * scale);
                    assert!(visible.bottom().0 <= card.bounds.bottom().0 - 4. * scale);
                });
            }
        }
    }
}
