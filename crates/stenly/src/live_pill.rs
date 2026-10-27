//! Floating "recording" capsule (Notion-style): a slim vertical pill docked
//! to the right edge of the screen, vertically centred: app icon, a live
//! level meter, elapsed time and a stop button. Non-activating, hidden from
//! screen share, draggable, visible over full-screen meeting windows. Shown
//! for the whole recording, on the screen the call is on (like Granola), and
//! follows the call if its window moves to another screen.

use crate::controller::{Controller, Status};
use crate::icons;
use crate::theme;
use chrono::Local;
use gpui::{
    App, Bounds, Context, DisplayId, Entity, FontWeight, IntoElement, MouseButton, ParentElement, Pixels, PlatformDisplay, Point,
    Render, Styled, Subscription, Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point, prelude::*,
    px, size,
};
use std::rc::Rc;
use std::time::Duration;
use stenly_core::platform::{Channel, DesktopShell};

/// Transparent margin around the capsule so its rounded ends and shadow are
/// never clipped by the window edge.
const MARGIN: f32 = 10.;
/// Capsule width (points).
const PILL_W: f32 = 48.;
const W: f32 = PILL_W + MARGIN * 2.;
const H: f32 = 196. + MARGIN * 2.;
/// Gap between the capsule and the screen edge.
const EDGE_GAP: f32 = 6.;
/// Waveform bars (horizontal row, like the dictation HUD / Granola).
const BARS: usize = 5;
/// Waveform box height (points); bars grow from the centre line.
const WAVE_H: f32 = 26.;
/// Fixed per-bar sensitivity so the waveform has shape (centre tallest).
const SENS: [f32; BARS] = [0.55, 0.85, 1.0, 0.8, 0.6];

pub struct LivePill {
    controller: Entity<Controller>,
    /// Per-bar heights 0..1, eased toward targets for fluid motion.
    bars: [f32; BARS],
    level: f32,
    tick: u32,
    /// Display the pill was last placed on.
    display: Option<DisplayId>,
    _sub: Subscription,
}

/// Where floating recording UI should go: the screen showing the call app's
/// window, else the screen with the pointer, else the primary one.
pub fn target_display(controller: &Entity<Controller>, cx: &App) -> Option<Rc<dyn PlatformDisplay>> {
    controller
        .read(cx)
        .call_display
        .and_then(|id| cx.find_display(DisplayId::new(id)))
        .or_else(|| crate::dictation_hud::pointer_display(cx))
        .or_else(|| cx.primary_display())
}

/// Docked to the right edge, vertically centred in the usable area (clear of
/// the menu bar, Dock and the "Record?" prompt in the top-right corner).
/// Display-local coordinates.
fn pill_origin(d: &dyn PlatformDisplay) -> Point<Pixels> {
    let b = d.visible_bounds();
    point(b.origin.x + b.size.width - px(W - MARGIN + EDGE_GAP), b.origin.y + (b.size.height - px(H)) / 2.)
}

fn level(db: f32) -> f32 {
    ((db + 55.0) / 45.0).clamp(0.0, 1.0)
}

pub fn is_open(cx: &App) -> bool {
    cx.windows().iter().any(|w| w.downcast::<LivePill>().is_some())
}

pub fn close(cx: &mut App) {
    for w in cx.windows() {
        if let Some(h) = w.downcast::<LivePill>() {
            let _ = h.update(cx, |_, window, _| window.remove_window());
        }
    }
}

pub fn show(controller: Entity<Controller>, shell: &dyn DesktopShell, cx: &mut App) {
    if is_open(cx) {
        return;
    }
    let display = target_display(&controller, cx);
    let origin = display.as_ref().map(|d| pill_origin(d.as_ref())).unwrap_or(point(px(80.), px(200.)));
    let display_id = display.as_ref().map(|d| d.id());
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds { origin, size: size(px(W), px(H)) })),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: true,
        is_resizable: false,
        is_minimizable: false,
        display_id,
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some("stenly".into()),
        ..Default::default()
    };
    let res = cx.open_window(opts, |window, cx| {
        crate::configure_native(window, shell);
        if let Ok(h) = raw_window_handle::HasWindowHandle::window_handle(window) {
            if let raw_window_handle::RawWindowHandle::AppKit(a) = h.as_raw() {
                shell.hide_from_capture(a.ns_view.as_ptr());
                shell.show_floating(a.ns_view.as_ptr());
            }
        }
        cx.new(|cx| {
            let sub = cx.observe_in(&controller, window, |p: &mut LivePill, ctrl, window, cx| {
                p.follow_call(&ctrl, window, cx);
                cx.notify();
            });
            // ~45 fps waveform (same cadence as the dictation HUD).
            cx.spawn_in(window, async move |this, cx| loop {
                cx.background_executor().timer(Duration::from_millis(22)).await;
                let alive = this.update(cx, |p, cx| {
                    let lv = p.controller.read(cx).levels.clone();
                    let v = lv.map(|l| level(l.get(Channel::Mic)).max(level(l.get(Channel::System)))).unwrap_or(0.);
                    p.level += (v - p.level) * if v > p.level { 0.5 } else { 0.12 };
                    p.tick = p.tick.wrapping_add(1);
                    // Noise gate so room hiss doesn't keep the bars twitching.
                    let gated = ((p.level - 0.08) / 0.92).max(0.0);
                    for i in 0..BARS {
                        // travelling ripple so speech looks alive, not a flat bar graph
                        let wobble = 0.7 + 0.3 * ((p.tick as f32 * 0.23) + i as f32 * 1.1).sin();
                        let target = (gated * SENS[i] * wobble * 1.25).min(1.0);
                        let k = if target > p.bars[i] { 0.45 } else { 0.16 };
                        p.bars[i] += (target - p.bars[i]) * k;
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            })
            .detach();
            LivePill { controller, bars: [0.0; BARS], level: 0., tick: 0, display: display_id, _sub: sub }
        })
    });
    if let Err(e) = res {
        tracing::error!(error = %e, "failed to open live pill");
    }
}

impl LivePill {
    /// The call window moved to another screen (or the call app only just
    /// grabbed the mic): bring the pill along. Within one screen the pill stays
    /// wherever the user dragged it. Reopened rather than moved: a window moved
    /// programmatically between screens of different scale keeps rendering at
    /// the old scale.
    fn follow_call(&mut self, ctrl: &Entity<Controller>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = ctrl.read(cx).call_display.map(DisplayId::new) else { return };
        if self.display == Some(id) || cx.find_display(id).is_none() {
            return;
        }
        self.display = Some(id);
        tracing::info!(display = ?id, "moving live pill to the call's screen");
        window.remove_window();
        let ctrl = ctrl.clone();
        cx.defer(move |cx| {
            let shell = ctrl.read(cx).shell.clone();
            show(ctrl, shell.as_ref(), cx);
        });
    }
}

impl Render for LivePill {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let secs = match &self.controller.read(cx).status {
            Status::Recording { since, .. } => (Local::now() - *since).num_seconds().max(0),
            Status::Idle => 0,
        };
        // Compact: m:ss, or h:mm once past the hour (fits the 48pt width).
        let time = if secs >= 3600 { format!("{}:{:02}", secs / 3600, (secs % 3600) / 60) } else { format!("{}:{:02}", secs / 60, secs % 60) };
        // Live waveform: green bars that grow from the centre line with
        // loudness (max of You / Them). At rest they sit as short stubs so the
        // capsule still reads as "recording".
        let wave = div().h(px(WAVE_H)).flex().items_center().gap(px(3.)).children(self.bars.iter().map(|v| {
            let h = 5. + v * (WAVE_H - 5.);
            div().w(px(3.5)).h(px(h)).rounded(px(1.75)).bg(theme::green()).opacity(0.55 + v * 0.45)
        }));
        let ctrl = self.controller.clone();
        let ctrl_open = self.controller.clone();
        let pill = div()
            .id("pill")
            .w(px(PILL_W))
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_between()
            .py(px(8.))
            .rounded_full()
            .bg(theme::popup_bg())
            .border_1()
            .border_color(theme::divider())
            .shadow(vec![gpui::BoxShadow {
                color: gpui::hsla(0., 0., 0., if theme::is_dark() { 0.5 } else { 0.18 }),
                offset: point(px(0.), px(4.)),
                blur_radius: px(10.),
                spread_radius: px(0.),
                inset: false,
            }])
            .text_color(theme::text())
            .font_family(theme::SANS)
            .cursor_grab()
            // drag anywhere on the capsule to move it
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .on_click(move |e, _, cx| {
                if e.click_count() == 2 {
                    crate::main_window::open(ctrl_open.clone(), Some(crate::main_window::Page::Home), cx);
                }
            })
            // app icon (double-click the capsule to open Stenly)
            .child(div().pt(px(1.)).child(icons::logo(30.)))
            // live meter
            .child(wave)
            // elapsed time
            .child(
                div()
                    .text_size(px(theme::SUBHEAD))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::muted())
                    .font_features(gpui::FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)])))
                    .child(time),
            )
            // stop: red square in a soft red well (the destructive action)
            .child(
                div()
                    .id("pill-stop")
                    .size(px(36.))
                    .flex_none()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme::red_tint())
                    .hover(|s| s.bg(gpui::Rgba { a: 0.24, ..theme::red() }))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, _, cx| {
                        ctrl.update(cx, |c, cx| c.stop_recording(cx));
                    })
                    .child(div().size(px(12.)).rounded(px(3.)).bg(theme::red())),
            );
        div().size_full().p(px(MARGIN)).child(pill)
    }
}
