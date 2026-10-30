//! Settings → Licence: free-trial progress, Buy, enter a key, deactivate.
//!
//! Layout never shifts: the status card always has the same rows, the notice
//! line is always present (empty when there is nothing to say) and buttons
//! swap in place rather than appear and disappear.

use crate::controller::LicenseNotice;
use crate::icons;
use crate::main_window::MainWindow;
use crate::theme;
use crate::ui::{self, Btn};
use gpui::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px,
};
use stenly_core::license::{self, State};

impl MainWindow {
    pub(crate) fn license_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = self.controller().read(cx);
        let snap = c.license.clone();
        let busy = c.license_busy;
        let notice = c.license_notice.clone();
        let ctrl = self.controller().clone();
        let state = snap.state();
        let buy_url = license::checkout_url();

        let (glyph, tint) = match state {
            State::Licensed => (icons::CHECK_CIRCLE, theme::green()),
            State::Expired => (icons::ALERT, theme::amber()),
            State::Trial | State::Open => (icons::CLOCK, theme::muted()),
        };
        let detail: SharedString = match state {
            State::Open => "This build of Stenly is free to use and doesn't need a licence. Thanks for building it yourself.".into(),
            State::Licensed => {
                let l = snap.license.as_ref().expect("licensed has a licence");
                format!("Key {} · one Mac · updates included. Checked {}.", l.masked_key, l.last_validated.with_timezone(&chrono::Local).format("%b %-d")).into()
            }
            State::Trial => "After that, a one-time $10 licence keeps recording on this Mac. Updates are included.".into(),
            State::Expired => "New meetings can't be recorded until you add a licence. Your notes, search, sharing and dictation keep working.".into(),
        };
        let status_row = ui::row(
            None,
            snap.headline(),
            Some(detail),
            div()
                .w(px(20.))
                .flex()
                .justify_end()
                .child(icons::icon(glyph, 15., tint)),
        )
        .into_any_element();

        // Trial meter: a fixed-height row in every state (empty when not on trial).
        let meter_row = {
            let frac = if snap.free_meetings == 0 {
                1.0
            } else {
                (snap.used.min(snap.free_meetings) as f32) / snap.free_meetings as f32
            };
            let text: SharedString = match state {
                State::Trial | State::Expired => format!(
                    "{} of {} free meetings used",
                    snap.used.min(snap.free_meetings),
                    snap.free_meetings
                )
                .into(),
                State::Licensed => format!("{} meetings recorded on the trial", snap.used).into(),
                State::Open => "".into(),
            };
            ui::row(
                None,
                "Free meetings",
                None,
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .text_size(px(theme::SUBHEAD))
                            .text_color(theme::muted())
                            .child(text),
                    )
                    .child(ui::progress_bar(frac, 90.)),
            )
            .into_any_element()
        };

        // Buy: opens the hosted checkout in the browser.
        let buy_row = {
            let desc: SharedString = if buy_url.is_some() {
                "$10 once, for one Mac. Updates included. Pay with a card or Apple Pay; VAT is handled for you.".into()
            } else {
                "Checkout isn't available in this build.".into()
            };
            let btn = match buy_url {
                Some(url) => ui::button(
                    "license-buy",
                    "Buy — $10",
                    if state == State::Licensed {
                        Btn::Ghost
                    } else {
                        Btn::Primary
                    },
                )
                .on_click(move |_, _, cx| cx.open_url(url))
                .into_any_element(),
                None => ui::button("license-buy", "Buy — $10", Btn::Ghost)
                    .opacity(0.45)
                    .into_any_element(),
            };
            ui::row(None, "Buy a licence", Some(desc), btn).into_any_element()
        };

        // Key entry (or the active key with Deactivate): same row height both ways.
        let key_row = if state == State::Licensed {
            let ctrl = ctrl.clone();
            ui::row(
                None,
                "This Mac",
                Some("Deactivate to move the licence to another Mac.".into()),
                if busy {
                    ui::pending("license-deactivating", "Working…").into_any_element()
                } else {
                    ui::button("license-deactivate", "Deactivate", Btn::Danger)
                        .on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.deactivate_license(cx)))
                        .into_any_element()
                },
            )
            .into_any_element()
        } else {
            let input = self.license_key.clone();
            let ctrl2 = ctrl.clone();
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .min_h(px(48.))
                .py(px(9.))
                .child(
                    div()
                        .w(px(84.))
                        .flex_shrink_0()
                        .text_size(px(theme::BODY))
                        .child("Licence key"),
                )
                .child(ui::text_field(self.license_key.clone()))
                .child(if busy {
                    ui::pending("license-activating", "Checking…").into_any_element()
                } else {
                    ui::button("license-activate", "Activate", Btn::Ghost)
                        .on_click(move |_, _, cx| {
                            let key = input.read(cx).text().trim().to_string();
                            ctrl2.update(cx, |c, cx| c.activate_license(key, cx));
                        })
                        .into_any_element()
                })
                .into_any_element()
        };

        // One always-present line for the outcome of the last action.
        let (note_glyph, note_text): (Option<(icons::Sym, gpui::Rgba)>, SharedString) =
            match &notice {
                Some(LicenseNotice::Ok(m)) => {
                    (Some((icons::CHECK, theme::green())), m.clone().into())
                }
                Some(LicenseNotice::Failed(m)) => {
                    (Some((icons::ALERT, theme::amber())), m.clone().into())
                }
                Some(LicenseNotice::Blocked(m)) => {
                    (Some((icons::ALERT, theme::amber())), m.clone().into())
                }
                None => (None, "".into()),
            };
        let notice_line = div()
            .min_h(px(34.))
            .px(px(16.))
            .pt(px(8.))
            .flex()
            .items_start()
            .gap(px(6.))
            .text_size(px(theme::SUBHEAD))
            .line_height(px(15.))
            .text_color(theme::muted())
            .when_some(note_glyph, |d, (g, col)| {
                d.child(div().pt(px(2.)).child(icons::icon(g, 11., col)))
            })
            .child(div().flex_1().min_w_0().child(note_text));

        let mut rows = vec![status_row];
        if state != State::Open {
            rows.push(meter_row);
            rows.push(buy_row);
            rows.push(key_row);
        }
        div()
            .flex()
            .flex_col()
            .child(ui::caption("Stenly licence"))
            .child(ui::card(rows))
            .when(state != State::Open, |d| d.child(notice_line))
            .child(ui::caption("Build it yourself"))
            .child(ui::card(vec![
                ui::row(
                    None,
                    "Free and open source",
                    Some("Stenly's source is MIT licensed. Building it yourself never needs a licence. The $10 licence is for the signed, notarized app with updates.".into()),
                    ui::button("license-source", "View source", Btn::Ghost).on_click(|_, _, cx| cx.open_url("https://github.com/wbunting/stenly")),
                )
                .into_any_element(),
            ]))
            .into_any_element()
    }
}
