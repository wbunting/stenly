//! The dictation recorder: a small dark capsule floating at the bottom centre
//! of the screen while you dictate (VoiceInk's "mini recorder"). Never takes
//! focus, so the text still lands in the app you were typing in.
//!
//!   [✕]   ▂▃▅▇▅▃▂▃▅▃▂▁   0:07   [■]      recording
//!         Transcribing ⋯                  processing
//!         ✨ Polishing ⋯                   AI clean-up
//!         Didn't catch that              notice

use crate::dictation::{DictationController, Phase};
use crate::theme;
use gpui::{
    App, Bounds, Context, Entity, FontWeight, IntoElement, MouseButton, ParentElement, Render, Styled, Subscription, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point, prelude::*, px, rgb, size,
};
use std::time::Duration;
use stenly_core::platform::DesktopShell;

/// Transparent margin so the capsule's shadow isn't clipped.
const MARGIN: f32 = 12.;
const CAP_W: f32 = 212.;
const CAP_H: f32 = 44.;
/// Live transcript box above the capsule (the window is always this tall; the
/// area is transparent and click-through until there's text to show).
const PREVIEW_W: f32 = 420.;
const PREVIEW_H: f32 = 84.;
const PREVIEW_GAP: f32 = 8.;
const W: f32 = PREVIEW_W + MARGIN * 2.;
/// Window height without / with the preview. The window grows upward (AppKit
/// keeps the bottom edge when resizing), and is only tall while there's text.
const H: f32 = CAP_H + MARGIN * 2.;
const H_TALL: f32 = H + PREVIEW_H + PREVIEW_GAP;
/// Characters of transcript kept in the preview (it shows the latest lines).
const PREVIEW_CHARS: usize = 240;
/// Gap above the Dock / bottom of the usable screen.
const BOTTOM_GAP: f32 = 28.;
const BARS: usize = 14;

pub struct DictationHud {
    dictation: Entity<DictationController>,
    /// Per-bar heights 0..1, eased toward targets for fluid motion.
    bars: [f32; BARS],
    /// Fixed per-bar sensitivity so the waveform has shape (like VoiceInk).
    sens: [f32; BARS],
    tick: u32,
    /// Whether the window is currently ordered in.
    shown: bool,
    /// Window currently has room for the preview.
    tall: bool,
    /// Bottom edge (AppKit y) to restore once a resize lands: gpui resizes
    /// asynchronously and keeps the top edge, but the HUD should grow upward.
    keep_bottom: Option<(f64, f32)>,
    _sub: Subscription,
}

pub fn is_open(cx: &App) -> bool {
    cx.windows().iter().any(|w| w.downcast::<DictationHud>().is_some())
}

/// Show / hide the (persistent) HUD window. Created on first use and then
/// only ordered in and out, never closed: closing a window hands key focus to
/// another Stenly window and pulls focus away from where the user is typing.
pub fn set_visible(dictation: Entity<DictationController>, shell: &dyn DesktopShell, visible: bool, cx: &mut App) {
    let existing = cx.windows().into_iter().find_map(|w| w.downcast::<DictationHud>());
    match existing {
        Some(h) => {
            let _ = h.update(cx, |hud, window, cx| {
                let show_now = visible && !hud.shown;
                hud.shown = visible;
                if show_now && hud.tall {
                    // compact again before hud_origin positions it below
                    hud.tall = false;
                    hud.keep_bottom = None;
                    window.resize(size(px(W), px(H)));
                }
                if let Ok(raw) = raw_window_handle::HasWindowHandle::window_handle(window) {
                    if let raw_window_handle::RawWindowHandle::AppKit(a) = raw.as_raw() {
                        if show_now {
                            // follow the pointer to whichever screen the user is on now
                            if let Some(o) = hud_origin(cx) {
                                shell.move_window(a.ns_view.as_ptr(), f32::from(o.x) as f64, f32::from(o.y) as f64);
                            }
                        }
                        shell.set_floating_visible(a.ns_view.as_ptr(), visible);
                    }
                }
                cx.notify();
            });
        }
        None if visible => show(dictation, shell, cx),
        None => {}
    }
}

fn hud_origin(cx: &App) -> Option<gpui::Point<gpui::Pixels>> {
    let d = pointer_display(cx).or_else(|| cx.primary_display())?;
    let b = d.visible_bounds();
    Some(point(b.origin.x + (b.size.width - px(W)) / 2., b.origin.y + b.size.height - px(H + BOTTOM_GAP - MARGIN)))
}

#[allow(dead_code)]
pub fn close(cx: &mut App) {
    for w in cx.windows() {
        if let Some(h) = w.downcast::<DictationHud>() {
            let _ = h.update(cx, |_, window, _| window.remove_window());
        }
    }
}

pub fn show(dictation: Entity<DictationController>, shell: &dyn DesktopShell, cx: &mut App) {
    if is_open(cx) {
        return;
    }
    // The screen with the mouse is where the user is working; fall back to primary.
    let display = pointer_display(cx).or_else(|| cx.primary_display());
    let origin = display
        .as_ref()
        .map(|d| {
            let b = d.visible_bounds();
            point(b.origin.x + (b.size.width - px(W)) / 2., b.origin.y + b.size.height - px(H + BOTTOM_GAP - MARGIN))
        })
        .unwrap_or(point(px(600.), px(800.)));
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds { origin, size: size(px(W), px(H)) })),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: true,
        is_resizable: false,
        is_minimizable: false,
        display_id: display.map(|d| d.id()),
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
            let sub = cx.observe(&dictation, |_: &mut DictationHud, _, cx| cx.notify());
            // ~45 fps animation while visible.
            cx.spawn_in(window, async move |this, cx| loop {
                cx.background_executor().timer(Duration::from_millis(22)).await;
                if this.update_in(cx, |h, window, cx| h.animate(window, cx)).is_err() {
                    break;
                }
            })
            .detach();
            // Stable pseudo-random sensitivities (centre bars a little taller).
            let mut sens = [0.0; BARS];
            for (i, s) in sens.iter_mut().enumerate() {
                let centre = 1.0 - ((i as f32 - (BARS as f32 - 1.) / 2.).abs() / (BARS as f32 / 2.)) * 0.45;
                let jitter = [0.82, 1.18, 0.95, 1.3, 0.74, 1.12, 1.0, 1.22, 0.88, 1.08, 0.79, 1.25, 0.92, 1.05][i % 14];
                *s = centre * jitter;
            }
            DictationHud { dictation, bars: [0.0; BARS], sens, tick: 0, shown: true, tall: false, keep_bottom: None, _sub: sub }
        })
    });
    if let Err(e) = res {
        tracing::error!(error = %e, "failed to open dictation HUD");
    }
}

fn ns_view(window: &Window) -> Option<*mut std::ffi::c_void> {
    let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        raw_window_handle::RawWindowHandle::AppKit(a) => Some(a.ns_view.as_ptr()),
        _ => None,
    }
}

pub(crate) fn pointer_display(cx: &App) -> Option<std::rc::Rc<dyn gpui::PlatformDisplay>> {
    #[cfg(target_os = "macos")]
    {
        let (x, y) = stenly_platform::appkit_ui::mouse_location()?;
        let p = point(px(x as f32), px(y as f32));
        cx.displays().into_iter().find(|d| d.bounds().contains(&p))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = cx;
        None
    }
}

impl DictationHud {
    fn animate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.shown {
            return;
        }
        let want_tall = self.has_preview(cx);
        if want_tall != self.tall {
            self.tall = want_tall;
            let h = if want_tall { H_TALL } else { H };
            #[cfg(target_os = "macos")]
            {
                self.keep_bottom = ns_view(window).and_then(stenly_platform::appkit_ui::window_bottom).map(|y| (y, h));
            }
            window.resize(size(px(W), px(h)));
        }
        #[cfg(target_os = "macos")]
        if let Some((y, h)) = self.keep_bottom {
            if (f32::from(window.bounds().size.height) - h).abs() < 0.5 {
                if let Some(v) = ns_view(window) {
                    stenly_platform::appkit_ui::set_window_bottom(v, y);
                }
                self.keep_bottom = None;
            }
        }
        self.tick = self.tick.wrapping_add(1);
        let d = self.dictation.read(cx);
        let recording = matches!(d.phase, Phase::Recording { .. });
        let level = if recording { d.level() } else { 0.0 };
        // Noise gate like VoiceInk's 0.3 threshold, rescaled.
        let gated = ((level - 0.12) / 0.88).max(0.0);
        for i in 0..BARS {
            // gentle travelling ripple so speech looks alive, not a flat bar graph
            let wobble = 0.75 + 0.25 * ((self.tick as f32 * 0.21) + i as f32 * 0.9).sin();
            let target = (gated * self.sens[i] * wobble).min(1.0);
            let k = if target > self.bars[i] { 0.45 } else { 0.16 };
            self.bars[i] += (target - self.bars[i]) * k;
        }
        cx.notify();
    }
}

impl DictationHud {
    fn has_preview(&self, cx: &App) -> bool {
        let d = self.dictation.read(cx);
        matches!(d.phase, Phase::Recording { .. })
            && d.config(cx).dictation.live_preview
            && d.preview().is_some_and(|p| !p.committed.is_empty() || !p.tentative.is_empty())
    }
}

fn dots(tick: u32) -> impl IntoElement {
    // three dots breathing in sequence
    div().flex().gap(px(3.)).children((0..3).map(move |i| {
        let phase = ((tick as f32 * 0.12) - i as f32 * 0.7).sin() * 0.5 + 0.5;
        div().size(px(4.)).rounded_full().bg(gpui::white()).opacity(0.3 + phase * 0.6)
    }))
}

impl Render for DictationHud {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let d = self.dictation.read(cx);
        let phase = d.phase.clone();
        let dict = self.dictation.clone();
        let dict2 = self.dictation.clone();
        let white = gpui::white();
        let dim = gpui::Rgba { r: 1., g: 1., b: 1., a: 0.55 };
        let tnum = gpui::FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]));

        let bars = div().flex().items_center().gap(px(2.5)).h(px(24.)).children(self.bars.iter().map(|v| {
            let h = 3.5 + v * 20.5;
            div().w(px(3.)).h(px(h)).rounded(px(1.5)).bg(white).opacity(0.55 + v * 0.45)
        }));

        let round_btn = |id: &'static str| {
            div()
                .id(id)
                .size(px(26.))
                .flex_none()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        };

        let content = match &phase {
            Phase::Recording { since, hands_free } => {
                let secs = since.elapsed().as_secs();
                let time = format!("{}:{:02}", secs / 60, secs % 60);
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(9.))
                    .size_full()
                    // cancel
                    .child(
                        round_btn("hud-cancel")
                            .hover(|s| s.bg(gpui::Rgba { r: 1., g: 1., b: 1., a: 0.12 }))
                            .on_click(move |_, _, cx| dict.update(cx, |d, cx| d.cancel(cx)))
                            .child(crate::icons::icon(crate::icons::X, 10., dim)),
                    )
                    .child(div().flex_1().flex().justify_center().child(bars))
                    .child(div().w(px(30.)).text_size(px(12.)).font_features(tnum).text_color(dim).child(time))
                    // stop (shown always; hands-free mode is the main reason to click it)
                    .child({
                        let _ = hands_free;
                        round_btn("hud-stop")
                            .bg(rgb(0xff453a))
                            .hover(|s| s.bg(rgb(0xff6961)))
                            .on_click(move |_, _, cx| dict2.update(cx, |d, cx| d.stop(cx)))
                            .child(div().size(px(9.)).rounded(px(2.)).bg(white))
                    })
            }
            Phase::Transcribing | Phase::Enhancing => div()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .size_full()
                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(white).child(if phase == Phase::Enhancing { "Polishing" } else { "Transcribing" }))
                .child(dots(self.tick)),
            Phase::Notice(msg) => div()
                .flex()
                .items_center()
                .justify_center()
                .px(px(14.))
                .size_full()
                .child(div().text_size(px(12.)).text_color(dim).truncate().child(msg.clone())),
            Phase::Idle => div().size_full(),
        };

        // only once the window has grown to fit it (resizes happen in animate)
        let preview = if self.tall { d.preview() } else { None };
        let preview_box = preview.map(|p| {
            let (text, split) = preview_text(&p.committed, &p.tentative, PREVIEW_CHARS);
            let len = text.len();
            let styled = gpui::StyledText::new(text).with_highlights([(
                split..len,
                gpui::HighlightStyle { color: Some(gpui::hsla(0., 0., 1., 0.5)), ..Default::default() },
            )]);
            div()
                .id("dictation-preview")
                .max_w(px(PREVIEW_W))
                .max_h(px(PREVIEW_H))
                .overflow_hidden()
                .flex()
                .flex_col()
                .justify_end()
                .px(px(14.))
                .py(px(10.))
                .rounded(px(14.))
                .bg(gpui::Rgba { r: 0.08, g: 0.08, b: 0.09, a: 0.94 })
                .border_1()
                .border_color(gpui::Rgba { r: 1., g: 1., b: 1., a: 0.16 })
                .font_family(theme::SANS)
                .text_size(px(13.))
                .line_height(px(18.))
                .text_color(white)
                .child(styled)
        });

        let capsule = div()
            .id("dictation-capsule")
            .w(px(CAP_W))
            .h(px(CAP_H))
            .rounded_full()
            // VoiceInk-dark regardless of system appearance: it floats over any app.
            .bg(gpui::Rgba { r: 0.08, g: 0.08, b: 0.09, a: 0.94 })
            .border_1()
            .border_color(gpui::Rgba { r: 1., g: 1., b: 1., a: 0.16 })
            .shadow(vec![gpui::BoxShadow {
                color: gpui::hsla(0., 0., 0., 0.35),
                offset: point(px(0.), px(4.)),
                blur_radius: px(12.),
                spread_radius: px(0.),
                inset: false,
            }])
            .font_family(theme::SANS)
            .cursor_grab()
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .child(content);
        div()
            .size_full()
            .p(px(MARGIN))
            .flex()
            .flex_col()
            .items_center()
            .justify_end()
            .gap(px(PREVIEW_GAP))
            .children(preview_box)
            .child(capsule)
    }
}

/// The tail of the transcript that fits the preview: `(text, split)` where
/// `text[split..]` is the part that may still change.
fn preview_text(committed: &str, tentative: &str, max: usize) -> (String, usize) {
    let mut text = committed.to_string();
    if !committed.is_empty() && !tentative.is_empty() {
        text.push(' ');
    }
    let mut split = text.len();
    text.push_str(tentative);
    if text.len() > max {
        // cut at a word boundary
        let mut from = text.len() - max;
        while !text.is_char_boundary(from) {
            from += 1;
        }
        // mid-word: start at the next word (a cut right at a word start keeps it)
        if let Some(sp) = text[from..].find(' ').filter(|_| !text[..from].ends_with(' ')) {
            from += sp + 1;
        }
        text = format!("…{}", &text[from..]);
        split = (split + "…".len()).saturating_sub(from).max("…".len());
    }
    (text, split)
}

#[cfg(test)]
mod tests {
    #[test]
    fn preview_keeps_the_tail_and_the_split() {
        // the last 13 bytes are exactly "four five six"
        let (t, s) = super::preview_text("one two three four", "five six", 13);
        assert_eq!(t, "…four five six");
        assert_eq!(&t[s..], "five six");
        // one less cuts into "four": start at the next whole word
        let (t, s) = super::preview_text("one two three four", "five six", 12);
        assert_eq!(t, "…five six");
        assert_eq!(&t[s..], "five six");
        let (t, s) = super::preview_text("", "hello", 100);
        assert_eq!((t.as_str(), s), ("hello", 0));
        let (t, s) = super::preview_text("all settled", "", 100);
        assert_eq!(&t[s..], "");
    }
}
