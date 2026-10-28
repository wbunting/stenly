//! The pre-meeting nudge: a small borderless panel in the top-right corner.
//! Non-activating (doesn't steal focus from your call) and, on macOS, excluded
//! from screen capture so other participants never see it.

use crate::controller::Controller;
use crate::theme;
use chrono::Local;
use gpui::{
    App, Bounds, Context, Entity, FontWeight, IntoElement, MouseButton, ParentElement, Render, Styled, Task, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point, prelude::*, px, size,
};
use std::time::Duration;
use stenly_core::platform::DesktopShell;
use stenly_core::scheduler::Prompt;

pub enum PopupKind {
    Start(Prompt),
    Stop(String),
    /// A finished note is ready. This is deliberately a non-activating nudge:
    /// sharing or opening requires a user click.
    NoteReady { dir: std::path::PathBuf, title: String },
}

pub struct Popup {
    kind: PopupKind,
    controller: Entity<Controller>,
    _dismiss: Task<()>,
}

/// How long a "record this?" nudge stays up before quietly going away.
const START_PROMPT_SECS: u64 = 15;
const NOTE_READY_SECS: u64 = 25;

const W: f32 = 360.;
const H: f32 = 124.;

pub fn show(kind: PopupKind, controller: Entity<Controller>, shell: &dyn DesktopShell, cx: &mut App) {
    // One corner prompt at a time: a note-ready nudge must never cover the
    // time-sensitive Record prompt, nor stack under a backlog's later nudges.
    let existing: Vec<_> = cx.windows().into_iter().filter_map(|w| w.downcast::<Popup>()).collect();
    let has_start = existing.iter().any(|h| h.read(cx).is_ok_and(|p| matches!(p.kind, PopupKind::Start(_))));
    if matches!(&kind, PopupKind::NoteReady { .. }) && has_start {
        return;
    }
    for h in existing {
        let _ = h.update(cx, |_, window, _| window.remove_window());
    }
    // Same screen as the call (and the recording pill).
    let display = crate::live_pill::target_display(&controller, cx);
    let origin = display
        .as_ref()
        .map(|d| {
            let b = d.visible_bounds();
            point(b.origin.x + b.size.width - px(W + 16.), b.origin.y + px(12.))
        })
        .unwrap_or(point(px(80.), px(40.)));
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds { origin, size: size(px(W), px(H)) })),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id: display.map(|d| d.id()),
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some("stenly".into()),
        ..Default::default()
    };
    let res = cx.open_window(opts, |window, cx| {
        crate::configure_native(window, shell);
        if let Ok(h) = raw_window_handle::HasWindowHandle::window_handle(window)
            && let raw_window_handle::RawWindowHandle::AppKit(a) = h.as_raw() {
                shell.hide_from_capture(a.ns_view.as_ptr());
                shell.show_floating(a.ns_view.as_ptr());
            }
        cx.new(|cx| {
            // Auto-dismiss: start prompts are a brief nudge; ignoring one means "not now".
            let secs = match &kind {
                PopupKind::Start(_) => START_PROMPT_SECS,
                PopupKind::Stop(_) => 90,
                PopupKind::NoteReady { .. } => NOTE_READY_SECS,
            };
            let auto_stop = matches!(kind, PopupKind::Stop(_));
            let ctrl = controller.clone();
            let dismiss = cx.spawn_in(window, async move |_, cx| {
                cx.background_executor().timer(Duration::from_secs(secs)).await;
                let _ = cx.update(|window, cx| {
                    // Ignoring a "stop?" prompt means yes, stop.
                    if auto_stop {
                        ctrl.update(cx, |c, cx| c.stop_recording(cx));
                    }
                    window.remove_window();
                });
            });
            Popup { kind, controller, _dismiss: dismiss }
        })
    });
    if let Err(e) = res {
        tracing::error!(error = %e, "failed to open popup");
    }
}

/// Close every popup (call ended).
pub fn close_all(cx: &mut App) {
    let popups: Vec<_> = cx.windows().into_iter().filter_map(|w| w.downcast::<Popup>()).collect();
    for h in popups {
        let _ = h.update(cx, |_, window, _| window.remove_window());
    }
}

/// Close corner prompts when recording starts (e.g. from the tray), so no
/// note-ready nudge sits over the live-call corner.
pub fn close_start_prompts(cx: &mut App) {
    let popups: Vec<_> = cx.windows().into_iter().filter_map(|w| w.downcast::<Popup>()).collect();
    for h in popups {
        let is_start = h.read(cx).is_ok_and(|p| matches!(p.kind, PopupKind::Start(_) | PopupKind::NoteReady { .. }));
        if is_start {
            let _ = h.update(cx, |_, window, _| window.remove_window());
        }
    }
}

fn button(id: &'static str, label: &'static str, primary: bool) -> gpui::Stateful<gpui::Div> {
    crate::ui::button(id, label, if primary { crate::ui::Btn::Primary } else { crate::ui::Btn::Ghost })
}

fn snooze(controller: &Entity<Controller>, prompt: &Prompt, cx: &mut App) {
    controller.update(cx, |c, _| c.snooze(prompt));
    let ctrl = controller.clone();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_secs(120)).await;
        let _ = ctrl.update(cx, |_, cx| cx.notify());
    })
    .detach();
}

/// Joining is deliberately separate from recording: hand the original URL
/// (including passcodes/query parameters) to the OS, then dismiss the nudge.
fn join_button(prompt: &Prompt) -> Option<gpui::Stateful<gpui::Div>> {
    let Prompt::Upcoming(event) = prompt else { return None };
    let link = event.link.as_ref()?;
    let url = link.url.clone();
    let label = format!("Join {}", link.provider.label());
    // Lead with the call app's own logo (Zoom, Meet, Teams) so the target is
    // recognisable at a glance; other providers keep the neutral video glyph.
    let button = match crate::icons::provider_logo(link.provider, 16.) {
        Some(logo) => crate::ui::button_lead_image("join-meeting", label, crate::ui::Btn::Primary, logo.rounded(px(3.))),
        None => crate::ui::button_lead_icon("join-meeting", label, crate::ui::Btn::Primary, crate::icons::VIDEO),
    };
    Some(button.on_click(move |_, window, cx| {
        cx.open_url(&url);
        window.remove_window();
    }))
}

impl Render for Popup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (heading, sub, actions): (String, String, Vec<gpui::AnyElement>) = match &self.kind {
            PopupKind::Start(p) => {
                let (heading, sub) = match p {
                    Prompt::Upcoming(e) => {
                        let secs = (e.start - chrono::Utc::now()).num_seconds();
                        let when = if secs > 15 {
                            format!("starts in {} min", (secs + 59) / 60)
                        } else if secs > -60 {
                            "starting now".into()
                        } else {
                            format!("started {} min ago", -secs / 60)
                        };
                        let via = e.link.as_ref().map(|l| format!(" · {}", l.provider.label())).unwrap_or_default();
                        (e.title.clone(), format!("{} · {when}{via}", e.start.with_timezone(&Local).format("%H:%M")))
                    }
                    Prompt::AdHoc { app_id } => ("Looks like you're on a call".into(), friendly_app(app_id)),
                };
                let prompt = p.clone();
                let prompt2 = p.clone();
                let ctrl = self.controller.clone();
                let ctrl2 = self.controller.clone();

                (
                    heading,
                    sub,
                    {
                        let is_adhoc = matches!(p, Prompt::AdHoc { .. });
                        let join = join_button(p);
                        let has_join = join.is_some();
                        let mut v = vec![button("record", "Record", !has_join)
                            .on_click(move |_, window, cx| {
                                let ev = match &prompt {
                                    Prompt::Upcoming(e) => Some(e.clone()),
                                    Prompt::AdHoc { .. } => None,
                                };
                                ctrl.update(cx, |c, cx| match ev {
                                    Some(e) => c.start_recording(Some(e), cx),
                                    None => c.start_now(cx),
                                });
                                window.remove_window();
                            })
                            .into_any_element()];
                        if let Some(join) = join {
                            v.push(join.into_any_element());
                            // Keep the provider label readable, even for Microsoft
                            // Teams. Secondary actions live in a native menu rather
                            // than squeezing four buttons into the nudge.
                            v.push(crate::ui::more_button(
                                "start-more",
                                vec![crate::ui::MenuEntry::item("Remind me in 2 min"), crate::ui::MenuEntry::item("Not now")],
                                move |i, window, cx| {
                                    if i == 0 {
                                        snooze(&ctrl2, &prompt2, cx);
                                    }
                                    window.remove_window();
                                },
                            ).into_any_element());
                        } else {
                            // Preserve the existing actions for calls without a link.
                            if !is_adhoc {
                                v.push(
                                    button("later", "In 2 min", false)
                                        .on_click(move |_, window, cx| {
                                            snooze(&ctrl2, &prompt2, cx);
                                            window.remove_window();
                                        })
                                        .into_any_element(),
                                );
                            }
                            v.push(button("skip", "Not now", false).on_click(|_, window, _| window.remove_window()).into_any_element());
                        }
                        v
                    }
                )
            }
            PopupKind::NoteReady { dir, title } => {
                let open_dir = dir.clone();
                let share_dir = dir.clone();
                let ctrl = self.controller.clone();
                let ctrl2 = self.controller.clone();
                (
                    format!("Notes ready: {title}"),
                    "Review the note, or create a private link to send to attendees.".into(),
                    vec![
                        crate::ui::icon_button("note-ready-close", crate::icons::X, 26., 11., "Dismiss")
                            .rounded_full()
                            .on_click(|_, window, _| window.remove_window())
                            .into_any_element(),
                        button("note-open", "Open", false)
                            .on_click(move |_, window, cx| {
                                if let Some(h) = crate::main_window::open(ctrl.clone(), None, cx) {
                                    let _ = h.update(cx, |w, _, cx| w.open_meeting(open_dir.clone(), cx));
                                }
                                window.remove_window();
                            })
                            .into_any_element(),
                        button("note-share", "Share with attendees", true)
                            .on_click(move |_, window, cx| {
                                if let Some(h) = crate::main_window::open(ctrl2.clone(), None, cx) {
                                    let _ = h.update(cx, |w, _, cx| {
                                        w.open_meeting(share_dir.clone(), cx);
                                        w.share_meeting(share_dir.clone(), cx);
                                    });
                                }
                                window.remove_window();
                            })
                            .into_any_element(),
                    ],
                )
            }
            PopupKind::Stop(reason) => {
                let ctrl = self.controller.clone();
                (
                    "Stop recording?".into(),
                    format!("{reason}. Recording stops in 90 seconds unless you keep going."),
                    vec![
                        button("stop", "Stop now", true)
                            .on_click(move |_, window, cx| {
                                ctrl.update(cx, |c, cx| c.stop_recording(cx));
                                window.remove_window();
                            })
                            .into_any_element(),
                        button("keep", "Keep recording", false)
                            .on_click(|_, window, _| window.remove_window())
                            .into_any_element(),
                    ],
                )
            }
        };
        let _ = cx;
        div()
            .size_full()
            .p(px(12.))
            .flex()
            .flex_col()
            .justify_between()
            .bg(theme::popup_bg())
            .border_1()
            .border_color(theme::divider())
            .rounded(px(18.))
            .text_size(px(theme::BODY))
            .text_color(theme::text())
            .font_family(theme::SANS)
            .on_mouse_down(MouseButton::Left, |_, _, _| {})
            .child(
                div()
                    .flex()
                    .gap(px(10.))
                    .items_start()
                    // Notifications lead with the app icon.
                    .child(crate::icons::logo(32.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .gap(px(1.))
                            .child(div().text_size(px(theme::BODY)).font_weight(FontWeight::SEMIBOLD).truncate().child(heading))
                            .child(div().text_size(px(theme::BODY)).line_height(px(16.)).text_color(theme::muted()).line_clamp(2).child(sub)),
                    ),
            )
            .child(div().flex().gap(px(8.)).justify_end().children(actions))
    }
}

fn friendly_app(app_id: &str) -> String {
    let id = app_id.to_lowercase();
    let name = if id.contains("zoom") {
        "Zoom"
    } else if id.contains("teams") {
        "Microsoft Teams"
    } else if id.contains("slack") {
        "Slack"
    } else if id.contains("chrome") || id.contains("chromium") {
        "Chrome"
    } else if id.contains("firefox") {
        "Firefox"
    } else if id.contains("safari") {
        "Safari"
    } else if id.contains("thebrowser") {
        "Arc"
    } else if id.contains("facetime") {
        "FaceTime"
    } else if id.contains("discord") {
        "Discord"
    } else {
        return format!("{app_id} is using the microphone");
    };
    format!("{name} is using the microphone")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext};

    fn upcoming(url: &str) -> Prompt {
        let from = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap().to_utc();
        let ics = format!("BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:join-test\nSUMMARY:Team sync\nDTSTART:20260101T000000Z\nDTEND:20260101T003000Z\nLOCATION:{url}\nEND:VEVENT\nEND:VCALENDAR");
        Prompt::Upcoming(stenly_core::calendar::events_from_ics(&ics, from, from + chrono::Duration::hours(1)).remove(0))
    }

    struct JoinTest(Prompt);

    impl Render for JoinTest {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(W)).h(px(H)).p(px(12.)).flex().items_end().justify_end().gap(px(8.))
                .child(button("record", "Record", false))
                .children(join_button(&self.0))
                .child(crate::ui::icon_button("more", crate::icons::ELLIPSIS, 22., 12., "More"))
        }
    }

    // Exercise the rendered click handler without opening a real meeting app
    // or capturing any audio. GPUI's test platform intercepts the OS handoff.
    #[gpui::test]
    fn join_opens_original_url_and_dismisses(cx: &mut TestAppContext) {
        for url in [
            "https://us02web.zoom.us/j/81234567890?pwd=abc&from=calendar",
            "https://meet.google.com/abc-defg-hij",
            "https://teams.microsoft.com/l/meetup-join/meeting/0?context=%7b%7d",
        ] {
            let prompt = upcoming(url);
            let (_, vcx) = cx.add_window_view(move |_, _| JoinTest(prompt));
            vcx.run_until_parked();
            vcx.simulate_click(point(px(230.), px(H - 24.)), Modifiers::none());
            vcx.run_until_parked();
            assert_eq!(vcx.opened_url().as_deref(), Some(url));
            vcx.cx.update(|cx| assert!(cx.windows().is_empty()));
        }
    }

    #[test]
    fn no_join_for_linkless_or_ad_hoc_meetings() {
        assert!(join_button(&upcoming("")).is_none());
        assert!(join_button(&Prompt::AdHoc { app_id: "us.zoom.xos".into() }).is_none());
    }
}
