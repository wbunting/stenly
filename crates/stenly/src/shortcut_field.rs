//! Shortcut picker (VoiceInk-style key caps). Click it, then press either a
//! lone modifier (tap Right ⌘, Fn, Right ⌥…) or a combo (⌃⇧Space). Esc
//! cancels, Delete clears. While recording, the global dictation listener is
//! paused by the parent so pressing the current shortcut doesn't dictate.
//!
//! GPUI reports modifiers without left/right, so a lone modifier is captured
//! from the platform's device flags via `ModifiersChangedEvent` + the side we
//! infer from which key went up last; when ambiguous we default to the right
//! side (the usual dictation key) and offer the left variants in the menu.

use crate::theme;
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, ModifiersChangedEvent, ParentElement,
    Render, SharedString, Styled, Window, div, prelude::*, px,
};
use stenly_core::platform::Hotkey;

pub enum ShortcutEvent {
    /// New binding (`None` = cleared).
    Changed(Option<Hotkey>),
    /// Recording started / stopped (pause the global listener meanwhile).
    Recording(bool),
}

pub struct ShortcutField {
    focus: FocusHandle,
    value: Option<Hotkey>,
    recording: bool,
    /// Modifiers seen during this recording, for lone-modifier capture.
    held: Vec<&'static str>,
    peak: Vec<&'static str>,
    /// Allow binding a lone modifier (dictation) vs combos only (paste last).
    allow_modifier_only: bool,
}

impl EventEmitter<ShortcutEvent> for ShortcutField {}

impl Focusable for ShortcutField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ShortcutField {
    pub fn new(value: Option<Hotkey>, allow_modifier_only: bool, cx: &mut Context<Self>) -> Self {
        Self { focus: cx.focus_handle(), value, recording: false, held: vec![], peak: vec![], allow_modifier_only }
    }

    #[allow(dead_code)]
    pub fn set_value(&mut self, v: Option<Hotkey>, cx: &mut Context<Self>) {
        if !self.recording && self.value != v {
            self.value = v;
            cx.notify();
        }
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.recording = true;
        self.held.clear();
        self.peak.clear();
        window.focus(&self.focus, cx);
        cx.emit(ShortcutEvent::Recording(true));
        cx.notify();
    }

    fn finish(&mut self, v: Option<Option<Hotkey>>, cx: &mut Context<Self>) {
        self.recording = false;
        self.held.clear();
        self.peak.clear();
        if let Some(v) = v {
            self.value = v.clone();
            cx.emit(ShortcutEvent::Changed(v));
        }
        cx.emit(ShortcutEvent::Recording(false));
        cx.notify();
    }

    fn mods_of(m: &gpui::Modifiers) -> Vec<&'static str> {
        let mut v = vec![];
        if m.control {
            v.push("ctrl");
        }
        if m.alt {
            v.push("alt");
        }
        if m.shift {
            v.push("shift");
        }
        if m.platform {
            v.push("cmd");
        }
        if m.function {
            v.push("fn");
        }
        v
    }

    fn on_mods(&mut self, e: &ModifiersChangedEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.recording {
            return;
        }
        let now = Self::mods_of(&e.modifiers);
        if now.len() > self.peak.len() {
            self.peak = now.clone();
        }
        // All released after exactly one modifier was held alone → lone modifier.
        if now.is_empty() && self.held.len() == 1 && self.peak.len() == 1 && self.allow_modifier_only {
            let name = match self.held[0] {
                "cmd" => "right_command",
                "alt" => "right_option",
                "ctrl" => "right_control",
                "shift" => "right_shift",
                "fn" => "fn",
                _ => "right_command",
            };
            self.finish(Some(Hotkey::parse(name)), cx);
            return;
        }
        self.held = now;
        cx.notify();
    }

    fn on_key(&mut self, e: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.recording {
            return;
        }
        cx.stop_propagation();
        let k = e.keystroke.key.to_lowercase();
        let m = &e.keystroke.modifiers;
        let none = !(m.control || m.alt || m.shift || m.platform);
        match k.as_str() {
            "escape" if none => return self.finish(None, cx),
            "backspace" | "delete" if none => return self.finish(Some(None), cx),
            _ => {}
        }
        // Combos need at least one modifier (a bare letter would fire while typing),
        // except function keys.
        let is_fkey = k.starts_with('f') && k[1..].parse::<u8>().is_ok();
        if none && !is_fkey {
            return;
        }
        let mut mods: Vec<String> = Self::mods_of(m).into_iter().filter(|x| *x != "fn").map(str::to_string).collect();
        mods.sort();
        let key = if k == " " { "space".to_string() } else { k };
        self.finish(Some(Some(Hotkey::Combo { modifiers: mods, key })), cx);
    }
}

/// Key caps for a binding: "Right ⌘" → [Right ⌘]; "⌃⇧Space" → [⌃][⇧][Space].
pub fn caps(h: &Hotkey) -> Vec<SharedString> {
    match h {
        Hotkey::Modifier(_) => vec![h.label().into()],
        Hotkey::Combo { .. } => {
            let label = h.label();
            let mut out: Vec<SharedString> = Vec::new();
            let mut rest = String::new();
            for c in label.chars() {
                if "⌃⌥⇧⌘".contains(c) && rest.is_empty() {
                    out.push(c.to_string().into());
                } else {
                    rest.push(c);
                }
            }
            if !rest.is_empty() {
                out.push(rest.into());
            }
            out
        }
    }
}

pub fn cap(label: impl Into<SharedString>) -> gpui::Div {
    div()
        .h(px(20.))
        .min_w(px(20.))
        .px(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .bg(theme::card())
        .border_1()
        .border_color(theme::divider())
        .text_size(px(theme::SUBHEAD))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text())
        .child(label.into())
}

impl Render for ShortcutField {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let recording = self.recording;
        let inner: gpui::AnyElement = if recording {
            let live = if self.held.is_empty() { "Press shortcut…".to_string() } else { self.held.iter().map(|m| match *m { "ctrl" => "⌃", "alt" => "⌥", "shift" => "⇧", "cmd" => "⌘", _ => "fn" }).collect::<Vec<_>>().join("") };
            div().px(px(4.)).text_size(px(theme::SUBHEAD)).text_color(theme::accent()).child(live).into_any_element()
        } else {
            match &self.value {
                Some(h) => div().flex().gap(px(3.)).children(caps(h).into_iter().map(cap)).into_any_element(),
                None => div().px(px(4.)).text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child("Record Shortcut").into_any_element(),
            }
        };
        div()
            .id("shortcut-field")
            .track_focus(&self.focus)
            .key_context("ShortcutField")
            .on_key_down(cx.listener(Self::on_key))
            .on_modifiers_changed(cx.listener(Self::on_mods))
            .h(px(28.))
            .min_w(px(96.))
            .px(px(4.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .bg(theme::inset())
            .border_1()
            .border_color(if recording { theme::accent() } else { theme::divider() })
            .cursor_pointer()
            .on_click(cx.listener(|this, _, window, cx| {
                if this.recording {
                    this.finish(None, cx);
                } else {
                    this.start(window, cx);
                }
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if this.recording {
                    this.finish(None, cx);
                }
            }))
            .child(inner)
    }
}
