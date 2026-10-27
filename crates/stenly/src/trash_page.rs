//! Trash: meetings moved out of Recent, kept until you empty the Trash.
//! Restore puts one back; delete (one, or Empty Trash) is permanent and asks
//! first. Notes already saved elsewhere (Obsidian, destinations) aren't touched.

use crate::main_window::MainWindow;
use crate::theme;
use crate::ui::{self, Btn};
use chrono::Local;
use gpui::{AnyElement, Context, FontWeight, IntoElement, ParentElement, PromptLevel, Styled, div, prelude::*, px};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long Undo stays offered after moving a meeting to the Trash.
const UNDO_FOR: Duration = Duration::from_secs(8);

const KEPT_ELSEWHERE: &str = "Notes already saved to Obsidian or a destination are kept.";

impl MainWindow {
    pub(crate) fn refresh_trash(&mut self, cx: &mut Context<Self>) {
        self.trash = self.controller().read(cx).trash();
        // Put back or deleted from here: there's nothing left to undo.
        if self.undo_trash.as_ref().is_some_and(|(p, _)| !p.exists()) {
            self.undo_trash = None;
        }
        cx.notify();
    }

    /// "Moved to Trash · Undo" in the bottom-right corner for a few seconds.
    pub(crate) fn offer_undo_trash(&mut self, trashed: PathBuf, cx: &mut Context<Self>) {
        self.undo_trash = Some((trashed, Instant::now()));
        if self.page == crate::main_window::Page::Trash {
            self.refresh_trash(cx);
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_FOR + Duration::from_millis(100)).await;
            let _ = this.update(cx, |this, cx| {
                if this.undo_trash.as_ref().is_some_and(|(_, t)| t.elapsed() >= UNDO_FOR) {
                    this.undo_trash = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn undo_trash_toast(&self, cx: &mut Context<Self>) -> AnyElement {
        let undo = ui::button("undo-trash", "Undo", Btn::Ghost).on_click(cx.listener(|this, _, _, cx| {
            let Some((trashed, _)) = this.undo_trash.take() else { return };
            let ctrl = this.controller().clone();
            match ctrl.update(cx, |c, cx| c.restore_meeting(trashed, cx)) {
                Ok(dir) => this.open_meeting(dir, cx),
                Err(e) => this.trash_error("Couldn't restore the meeting", e, cx),
            }
            if this.page == crate::main_window::Page::Trash {
                this.refresh_trash(cx);
            }
            cx.notify();
        }));
        crate::toast::card("toast-undo-trash".into(), Some(crate::icons::icon(crate::icons::TRASH, 12., theme::muted())), "Moved to Trash".into(), Some(undo.into_any_element()))
            .into_any_element()
    }

    pub(crate) fn trash_error(&mut self, what: &str, e: anyhow::Error, cx: &mut Context<Self>) {
        let msg = format!("{what}: {e:#}");
        self.controller().update(cx, |c, cx| {
            c.last_error = Some(msg);
            cx.notify();
        });
    }

    pub(crate) fn trash_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let today = Local::now().date_naive();
        let n = self.trash.len();
        let rows: Vec<AnyElement> = self
            .trash
            .iter()
            .enumerate()
            .map(|(i, (path, m))| {
                let started = m.started.with_timezone(&Local);
                let day = started.date_naive();
                let date = ui::day_label(day, today);
                let mins = m.duration_mins().unwrap_or(0);
                let detail = match mins {
                    0 => format!("{date} · {}", started.format("%-I:%M %p")),
                    n => format!("{date} · {} · {n} min", started.format("%-I:%M %p")),
                };
                let restore = {
                    let path = path.clone();
                    ui::button(("trash-restore", i), "Put Back", Btn::Ghost).on_click(cx.listener(move |this, _, _, cx| {
                        let ctrl = this.controller().clone();
                        match ctrl.update(cx, |c, cx| c.restore_meeting(path.clone(), cx)) {
                            Ok(_) => this.flash("Put back in Meetings", cx),
                            Err(e) => this.trash_error("Couldn't put the meeting back", e, cx),
                        }
                        this.refresh_trash(cx);
                    }))
                };
                let delete = {
                    let path = path.clone();
                    let title = m.title.clone();
                    let weak = cx.entity().downgrade();
                    ui::more_button(("trash-more", i), vec![ui::MenuEntry { symbol: Some("trash"), ..ui::MenuEntry::item("Delete Permanently…") }], move |_, window, cx| {
                        let answer = window.prompt(
                            PromptLevel::Warning,
                            &format!("Delete “{title}” permanently?"),
                            Some(&format!("Its recording, transcript and notes in Stenly will be deleted. You can't undo this. {KEPT_ELSEWHERE}")),
                            &["Delete", "Cancel"],
                            cx,
                        );
                        let path = path.clone();
                        let weak = weak.clone();
                        cx.spawn(async move |cx| {
                            if answer.await != Ok(0) {
                                return;
                            }
                            let _ = weak.update(cx, |this, cx| {
                                let ctrl = this.controller().clone();
                                if let Err(e) = ctrl.update(cx, |c, cx| c.delete_trashed(path, cx)) {
                                    this.trash_error("Couldn't delete the meeting", e, cx);
                                }
                                this.refresh_trash(cx);
                            });
                        })
                        .detach();
                    })
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .min_h(px(52.))
                    .py(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().text_size(px(theme::BODY)).font_weight(FontWeight::MEDIUM).truncate().child(m.title.clone()))
                            .child(div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).truncate().child(detail)),
                    )
                    .child(div().flex().items_center().gap(px(6.)).child(restore).child(delete))
                    .into_any_element()
            })
            .collect();

        let empty_btn = ui::button("empty-trash", "Empty Trash…", Btn::Danger).on_click(cx.listener(move |_this, _, window, cx| {
            let what = if n == 1 { "the meeting".to_string() } else { format!("all {n} meetings") };
            let answer = window.prompt(
                PromptLevel::Warning,
                &format!("Permanently delete {what} in the Trash?"),
                Some(&format!("Recordings, transcripts and notes in Stenly will be deleted. You can't undo this. {KEPT_ELSEWHERE}")),
                &["Empty Trash", "Cancel"],
                cx,
            );
            cx.spawn(async move |this, cx| {
                if answer.await != Ok(0) {
                    return;
                }
                let _ = this.update(cx, |this, cx| {
                    let ctrl = this.controller().clone();
                    match ctrl.update(cx, |c, cx| c.empty_trash(cx)) {
                        Ok(_) => this.flash("Trash emptied", cx),
                        Err(e) => this.trash_error("Couldn't empty the Trash", e, cx),
                    }
                    this.undo_trash = None;
                    this.refresh_trash(cx);
                });
            })
            .detach();
        }));

        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .pt(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(div().flex_1().min_w_0().text_size(px(theme::BODY)).text_color(theme::muted()).child("Meetings stay here until you empty the Trash."))
                    .when(n > 0, |d| d.child(empty_btn)),
            )
            .child(div().h(px(20.)))
            .when(n == 0, |d| {
                d.child(
                    div()
                        .rounded(px(12.))
                        .bg(theme::card())
                        .p(px(4.))
                        .child(div().px(px(12.)).py(px(10.)).text_size(px(theme::BODY)).text_color(theme::muted()).child("The Trash is empty.")),
                )
            })
            .when(n > 0, |d| d.child(ui::card(rows)))
            .into_any_element()
    }
}

