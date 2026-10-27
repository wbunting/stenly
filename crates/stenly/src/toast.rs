//! Transient status notices ("Private link copied", "Couldn't revoke link")
//! shown in a stack in the window's bottom-right corner. Toolbars never grow or
//! shrink for them, so buttons can't move under the pointer.

use crate::icons::{self, icon};
use crate::theme;
use crate::ui;
use gpui::{AnyElement, Animation, AnimationExt, ElementId, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToastKind {
    /// Something finished ("Copied"): a checkmark.
    Done,
    /// Something started ("Rewriting notes…"): no glyph.
    Info,
    /// Something failed: a warning glyph, shown for longer.
    Error,
}

impl ToastKind {
    pub fn duration(self) -> Duration {
        if hold() {
            return Duration::from_secs(600);
        }
        match self {
            ToastKind::Error => Duration::from_millis(4500),
            _ => Duration::from_millis(2200),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Toast {
    /// Bumped for every notice, so a newer one restarts the animation and an
    /// older timer can't dismiss it.
    pub id: u64,
    pub kind: ToastKind,
    pub msg: SharedString,
    /// Fading out before removal.
    pub leaving: bool,
}

/// Dev: STENLY_TOAST_HOLD=1 keeps notices up, unanimated (for screenshots).
fn hold() -> bool {
    std::env::var_os("STENLY_TOAST_HOLD").is_some()
}

/// How long the fade-out runs before the toast is removed.
pub const FADE_OUT: Duration = Duration::from_millis(160);

/// A popover-material card: optional glyph, message, optional trailing control.
pub fn card(id: ElementId, glyph: Option<AnyElement>, msg: SharedString, trailing: Option<AnyElement>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(gpui::Role::Status)
        .aria_label(msg.clone())
        .occlude()
        .flex()
        .items_center()
        .gap(px(8.))
        .min_h(px(34.))
        .max_w(px(360.))
        .pl(px(12.))
        .pr(px(if trailing.is_some() { 6. } else { 14. }))
        .py(px(6.))
        .rounded(px(10.))
        // Opaque: popover material is translucent, and text scrolls under this.
        .bg(gpui::Rgba { a: 1.0, ..theme::popup_bg() })
        .border_1()
        .border_color(theme::divider())
        .shadow_lg()
        .text_size(px(theme::CALLOUT))
        .text_color(theme::text())
        .when_some(glyph, |d, g| d.child(g))
        .child(div().min_w_0().flex_1().child(msg))
        .when_some(trailing, |d, t| d.child(t))
}

/// `card` with a slide-up/fade-in on arrival and a fade-out while leaving.
pub fn animated(el: gpui::Stateful<gpui::Div>, id: u64, leaving: bool) -> AnyElement {
    if hold() {
        return el.into_any_element();
    }
    if leaving {
        el.with_animation(ElementId::NamedInteger("toast-out".into(), id), Animation::new(FADE_OUT).with_easing(gpui::ease_in_out), |d, t| d.opacity(1. - t))
            .into_any_element()
    } else {
        el.with_animation(ElementId::NamedInteger("toast-in".into(), id), Animation::new(Duration::from_millis(200)).with_easing(gpui::ease_out_quint()), |d, t| {
            d.opacity(t).relative().top(px(8. * (1. - t)))
        })
        .into_any_element()
    }
}

pub fn render(t: &Toast) -> AnyElement {
    let glyph = match t.kind {
        ToastKind::Done => Some(icon(icons::CHECK, 12., theme::muted())),
        ToastKind::Info => None,
        ToastKind::Error => Some(icon(icons::ALERT, 12., theme::amber())),
    };
    animated(card(ElementId::NamedInteger("toast".into(), t.id), glyph, t.msg.clone(), None), t.id, t.leaving)
}

/// "Sending to Notion…" while a destination send is in flight.
pub fn pending(label: SharedString) -> AnyElement {
    card("toast-pending".into(), Some(ui::spinner("toast-spin", 11., theme::muted()).into_any_element()), label, None).into_any_element()
}
