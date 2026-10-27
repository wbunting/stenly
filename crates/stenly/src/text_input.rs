//! A small text input supporting single- and multi-line editing, selection, clipboard,
//! IME (via `EntityInputHandler`), soft wrapping and password masking.
//!
//! GPUI has no built-in text field, so this is adapted from GPUI's `input.rs` example and
//! extended to multi-line text with wrapping.
#![allow(dead_code)]

use std::{ops::Range, rc::Rc};

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    SharedString, Size, Style, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine,
    actions, div, fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;
use crate::theme;



actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
        Submit,
        Newline,
        WordLeft,
        WordRight,
        DeleteWordLeft,
    ]
);

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("TextInput");
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("up", Up, ctx),
        KeyBinding::new("down", Down, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("alt-left", WordLeft, ctx),
        KeyBinding::new("alt-right", WordRight, ctx),
        KeyBinding::new("alt-backspace", DeleteWordLeft, ctx),
        KeyBinding::new("cmd-a", SelectAll, ctx),
        KeyBinding::new("cmd-v", Paste, ctx),
        KeyBinding::new("cmd-c", Copy, ctx),
        KeyBinding::new("cmd-x", Cut, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("cmd-left", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new("cmd-right", End, ctx),
        KeyBinding::new("enter", Submit, ctx),
        KeyBinding::new("shift-enter", Newline, ctx),
        KeyBinding::new("alt-enter", Newline, ctx),
    ]);
}

pub enum InputEvent {
    /// Enter was pressed.
    Submit,
    /// The text changed (typing, paste, delete...).
    Changed,
}

pub struct TextInput {
    focus_handle: FocusHandle,
    content: String,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    multiline: bool,
    masked: bool,
    last_layout: Option<Rc<InputLayout>>,
    is_selecting: bool,
    /// How far the text is scrolled inside the field (x for single-line, y for multi-line),
    /// so the cursor stays visible when the text is larger than the field.
    scroll: Point<Pixels>,
    /// Multi-line fields grow up to this many lines, then scroll.
    max_lines: usize,
    /// Return inserts a newline instead of submitting (multi-line editors).
    return_is_newline: bool,
}

/// What the element painted last frame; used for mouse hit-testing and IME bounds.
struct InputLayout {
    lines: Vec<WrappedLine>,
    /// Byte offset of the start of each `\n`-separated line in the (display) text.
    line_starts: Vec<usize>,
    /// Y offset of the start of each `\n`-separated line, relative to the element.
    line_tops: Vec<Pixels>,
    line_height: Pixels,
    /// Where the text is drawn: the element origin minus the scroll offset.
    origin: Point<Pixels>,
}

impl EventEmitter<InputEvent> for TextInput {}

impl TextInput {
    pub fn new(cx: &mut Context<Self>, placeholder: impl Into<SharedString>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: String::new(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            multiline: false,
            masked: false,
            last_layout: None,
            is_selecting: false,
            scroll: Point::default(),
            max_lines: 10,
            return_is_newline: false,
        }
    }

    /// A multi-line text editor: Return inserts a newline, grows to `lines` lines.
    pub fn editor(mut self, lines: usize) -> Self {
        self.multiline = true;
        self.return_is_newline = true;
        self.max_lines = lines.max(1);
        self
    }

    pub fn multiline(mut self) -> Self {
        self.multiline = true;
        self
    }

    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.content = text.into();
        let end = self.content.len();
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    /// Put the cursor at the start (and scroll there), e.g. after loading a document.
    pub fn move_to_start(&mut self, cx: &mut Context<Self>) {
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.scroll = Point::default();
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.set_text("", cx);
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.clamp(offset);
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.clamp(offset);
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }

    fn previous_word(&self, offset: usize) -> usize {
        self.content
            .unicode_word_indices()
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_word(&self, offset: usize) -> usize {
        self.content
            .unicode_word_indices()
            .find_map(|(idx, word)| (idx + word.len() > offset).then_some(idx + word.len()))
            .unwrap_or(self.content.len())
    }

    // --- actions -------------------------------------------------------------------

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous_word(self.cursor_offset()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word(self.cursor_offset()), cx);
    }

    fn vertical(&mut self, direction: f32, cx: &mut Context<Self>) {
        let Some(layout) = &self.last_layout else {
            return;
        };
        let cursor = self.display_offset_for_content(self.cursor_offset());
        let Some(pos) = layout.position_for_index(cursor) else {
            return;
        };
        let target = point(pos.x, pos.y + layout.line_height * direction + px(1.));
        let index = if target.y < Pixels::ZERO {
            0
        } else {
            self.content_offset_for_layout(layout.index_for_position(target, usize::MAX))
        };
        self.move_to(index, cx);
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(-1., cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.vertical(1., cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        cx.notify();
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let line_start = self.content[..self.cursor_offset()]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        self.move_to(line_start, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let line_end = self.content[cursor..]
            .find('\n')
            .map(|i| cursor + i)
            .unwrap_or(self.content.len());
        self.move_to(line_end, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_word_left(
        &mut self,
        _: &DeleteWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_word(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        if self.return_is_newline {
            self.replace_text_in_range(None, "\n", window, cx);
            return;
        }
        cx.emit(InputEvent::Submit);
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            self.replace_text_in_range(None, "\n", window, cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = if self.multiline {
                text.replace("\r\n", "\n")
            } else {
                text.replace(['\r', '\n'], " ")
            };
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.masked {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.masked {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn index_for_mouse(&self, position: Point<Pixels>) -> usize {
        let Some(layout) = &self.last_layout else {
            return 0;
        };
        let local = position - layout.origin;
        if local.y < Pixels::ZERO {
            return 0;
        }
        self.content_offset_for_layout(layout.index_for_position(local, usize::MAX))
    }

    /// Converts an offset into the last painted layout into a valid offset into `content`.
    /// The layout shows the placeholder when empty and bullets when masked.
    fn content_offset_for_layout(&self, offset: usize) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        self.clamp(self.content_offset_for_display(offset))
    }

    /// Clamps to the content length and snaps down to a char boundary.
    fn clamp(&self, mut offset: usize) -> usize {
        offset = offset.min(self.content.len());
        while !self.content.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        self.is_selecting = true;
        let index = self.index_for_mouse(event.position);
        if event.modifiers.shift {
            self.select_to(index, cx);
        } else if event.click_count == 2 {
            let start = self.previous_word(index + 1).min(index);
            let end = self.next_word(index);
            self.selected_range = start..end;
            self.selection_reversed = false;
            cx.notify();
        } else if event.click_count >= 3 {
            self.selected_range = 0..self.content.len();
            cx.notify();
        } else {
            self.move_to(index, cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse(event.position), cx);
        }
    }

    // --- utf16 helpers (the platform IME speaks UTF-16) ---------------------------

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for ch in self.content.chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16 = 0;
        let mut utf8 = 0;
        for ch in self.content.chars() {
            if utf8 >= offset {
                break;
            }
            utf8 += ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        utf16
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    /// The text that's actually drawn (bullets when masked).
    fn display_text(&self) -> String {
        if self.masked {
            "•".repeat(self.content.chars().count())
        } else {
            self.content.clone()
        }
    }

    /// Map a byte offset in `content` to one in `display_text`.
    fn display_offset_for_content(&self, offset: usize) -> usize {
        if self.masked {
            self.content[..self.clamp(offset)].chars().count() * '•'.len_utf8()
        } else {
            offset
        }
    }

    fn content_offset_for_display(&self, offset: usize) -> usize {
        if self.masked {
            let n = offset / '•'.len_utf8();
            self.content
                .char_indices()
                .nth(n)
                .map(|(i, _)| i)
                .unwrap_or(self.content.len())
        } else {
            offset.min(self.content.len())
        }
    }
}

impl InputLayout {
    /// Position of a byte offset (in display text), relative to the element origin.
    fn position_for_index(&self, index: usize) -> Option<Point<Pixels>> {
        let line_ix = self
            .line_starts
            .iter()
            .rposition(|&start| start <= index)
            .unwrap_or(0);
        let line = self.lines.get(line_ix)?;
        let local = index - self.line_starts[line_ix];
        let p = line.position_for_index(local.min(line.len()), self.line_height)?;
        Some(point(p.x, p.y + self.line_tops[line_ix]))
    }

    fn index_for_position(&self, position: Point<Pixels>, content_len: usize) -> usize {
        let line_ix = self
            .line_tops
            .iter()
            .rposition(|&top| top <= position.y)
            .unwrap_or(0);
        let Some(line) = self.lines.get(line_ix) else {
            return content_len;
        };
        let local = point(position.x, position.y - self.line_tops[line_ix]);
        let ix = match line.closest_index_for_position(local, self.line_height) {
            Ok(ix) | Err(ix) => ix,
        };
        self.line_starts[line_ix] + ix.min(line.len())
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|r| self.range_to_utf16(r))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let new_text = if self.multiline {
            new_text.to_string()
        } else {
            new_text.replace('\n', "")
        };
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content.replace_range(range.clone(), &new_text);
        let cursor = range.start + new_text.len();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content.replace_range(range.clone(), new_text);
        self.marked_range =
            (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map(|r| r.start + range.start..r.end + range.start)
            .unwrap_or_else(|| {
                let end = range.start + new_text.len();
                end..end
            });
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let start = layout.position_for_index(self.display_offset_for_content(range.start))?;
        let end = layout.position_for_index(self.display_offset_for_content(range.end))?;
        Some(Bounds::from_corners(
            layout.origin + start,
            layout.origin + point(end.x, end.y + layout.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let layout = self.last_layout.as_ref()?;
        let local = point - layout.origin;
        let ix = self.content_offset_for_layout(layout.index_for_position(local, usize::MAX));
        Some(self.offset_to_utf16(ix))
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("text-input")
            .key_context("TextInput")
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::newline))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .w_full()
            .child(TextElement { input: cx.entity() })
    }
}

// ---------------------------------------------------------------------------------------
// The element that shapes, wraps and paints the text
// ---------------------------------------------------------------------------------------

struct TextElement {
    input: Entity<TextInput>,
}

struct Prepaint {
    layout: Rc<InputLayout>,
    cursor: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

/// Shapes `text` into wrapped lines; returns the lines, each line's start offset, and
/// each line's top offset.
fn shape(
    text: &str,
    runs: &[TextRun],
    width: Option<Pixels>,
    window: &mut Window,
) -> (Vec<WrappedLine>, Vec<usize>, Vec<Pixels>, Pixels) {
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    let line_height = window.line_height();
    let lines: Vec<WrappedLine> = window
        .text_system()
        .shape_text(
            SharedString::from(text.to_string()),
            font_size,
            runs,
            width,
            None,
        )
        .map(|l| l.into_iter().collect())
        .unwrap_or_default();
    let mut starts = Vec::with_capacity(lines.len());
    let mut tops = Vec::with_capacity(lines.len());
    let mut offset = 0;
    let mut top = Pixels::ZERO;
    for line in &lines {
        starts.push(offset);
        tops.push(top);
        offset += line.len() + 1;
        top += line_height * (line.wrap_boundaries().len() + 1) as f32;
    }
    (lines, starts, tops, top.max(line_height))
}

/// Returns a scroll offset that keeps the cursor (at `cursor`, a line tall) inside a
/// viewport of `viewport` size, changing `current` as little as possible and never
/// scrolling past the content.
fn scroll_to_reveal(
    current: Point<Pixels>,
    cursor: Point<Pixels>,
    line_height: Pixels,
    content: Size<Pixels>,
    viewport: Size<Pixels>,
) -> Point<Pixels> {
    // Leave room for the cursor bar at the right edge.
    let margin = px(2.);
    let mut scroll = current;

    let max_x = (content.width + margin - viewport.width).max(Pixels::ZERO);
    if cursor.x - scroll.x > viewport.width - margin {
        scroll.x = cursor.x - viewport.width + margin;
    }
    if cursor.x < scroll.x {
        scroll.x = cursor.x;
    }
    scroll.x = scroll.x.clamp(Pixels::ZERO, max_x);

    let max_y = (content.height - viewport.height).max(Pixels::ZERO);
    if cursor.y + line_height - scroll.y > viewport.height {
        scroll.y = cursor.y + line_height - viewport.height;
    }
    if cursor.y < scroll.y {
        scroll.y = cursor.y;
    }
    scroll.y = scroll.y.clamp(Pixels::ZERO, max_y);
    scroll
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let input = self.input.read(cx);
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        if !input.multiline {
            style.size.height = window.line_height().into();
            return (window.request_layout(style, [], cx), ());
        }
        // Multi-line: height depends on the wrapped text at the available width.
        let text = input.display_text();
        let text = if text.is_empty() {
            input.placeholder.to_string()
        } else {
            text
        };
        let run = window.text_style().to_run(text.len());
        let max_height = window.line_height() * input.max_lines as f32;
        let layout_id =
            window.request_measured_layout(style, move |known, available, window, _| {
                let width = known.width.or(match available.width {
                    gpui::AvailableSpace::Definite(w) => Some(w),
                    _ => None,
                });
                let (_, _, _, height) = shape(&text, std::slice::from_ref(&run), width, window);
                size(width.unwrap_or(px(100.)), height.min(max_height))
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let input = self.input.read(cx);
        let style = window.text_style();
        let content = input.display_text();
        let (text, color) = if content.is_empty() {
            (input.placeholder.to_string(), theme::text_faint().into())
        } else {
            (content, style.color)
        };
        let base = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = match input
            .marked_range
            .as_ref()
            .filter(|_| !input.content.is_empty())
        {
            Some(marked) => {
                let (s, e) = (
                    input.display_offset_for_content(marked.start),
                    input.display_offset_for_content(marked.end),
                );
                vec![
                    TextRun {
                        len: s,
                        ..base.clone()
                    },
                    TextRun {
                        len: e - s,
                        underline: Some(UnderlineStyle {
                            color: Some(base.color),
                            thickness: px(1.),
                            wavy: false,
                        }),
                        ..base.clone()
                    },
                    TextRun {
                        len: text.len() - e,
                        ..base
                    },
                ]
                .into_iter()
                .filter(|r| r.len > 0)
                .collect()
            }
            None => vec![base],
        };
        let wrap = input.multiline.then_some(bounds.size.width);
        let (lines, line_starts, line_tops, content_height) = shape(&text, &runs, wrap, window);
        let line_height = window.line_height();
        let has_text = !input.content.is_empty();
        let sel = input.display_offset_for_content(input.selected_range.start)
            ..input.display_offset_for_content(input.selected_range.end);
        let cursor_ix = input.display_offset_for_content(input.cursor_offset());

        // Keep the cursor inside the visible area.
        let unscrolled = InputLayout {
            lines,
            line_starts,
            line_tops,
            line_height,
            origin: bounds.origin,
        };
        let cursor_pos = if has_text {
            unscrolled.position_for_index(cursor_ix).unwrap_or_default()
        } else {
            Point::default()
        };
        let content_width = if input.multiline || !has_text {
            Pixels::ZERO
        } else {
            unscrolled
                .lines
                .first()
                .map(|l| l.width())
                .unwrap_or_default()
        };
        let scroll = scroll_to_reveal(
            input.scroll,
            cursor_pos,
            line_height,
            size(content_width, content_height),
            bounds.size,
        );
        let layout = Rc::new(InputLayout {
            origin: bounds.origin - scroll,
            ..unscrolled
        });
        let mut selections = Vec::new();
        let mut cursor = None;
        if sel.is_empty() || !has_text {
            cursor = Some(fill(
                Bounds::new(layout.origin + cursor_pos, size(px(1.5), line_height)),
                theme::accent(),
            ));
        } else {
            // One highlight rect per visual row the selection touches.
            let start = layout.position_for_index(sel.start).unwrap_or_default();
            let end = layout.position_for_index(sel.end).unwrap_or_default();
            let mut y = start.y;
            while y <= end.y {
                let x0 = if y == start.y { start.x } else { Pixels::ZERO };
                let x1 = if y == end.y { end.x } else { bounds.size.width };
                selections.push(fill(
                    Bounds::from_corners(
                        layout.origin + point(x0, y),
                        layout.origin + point(x1.max(x0 + px(4.)), y + line_height),
                    ),
                    theme::selection(),
                ));
                y += line_height;
            }
        }
        self.input.update(cx, |input, _| input.scroll = scroll);
        Prepaint {
            layout,
            cursor,
            selections,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        // Never draw outside the field, whatever the text length.
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for quad in prepaint.selections.drain(..) {
                window.paint_quad(quad);
            }
            let layout = &prepaint.layout;
            for (line, top) in layout.lines.iter().zip(&layout.line_tops) {
                let _ = line.paint(
                    layout.origin + point(Pixels::ZERO, *top),
                    layout.line_height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            if focus_handle.is_focused(window)
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
        });
        let layout = prepaint.layout.clone();
        self.input
            .update(cx, |input, _| input.last_layout = Some(layout));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    fn setup(
        cx: &mut TestAppContext,
        multiline: bool,
    ) -> (Entity<TextInput>, &mut VisualTestContext) {
        cx.update(bind_keys);
        let (input, cx) = cx.add_window_view(|_, cx| {
            let input = TextInput::new(cx, "placeholder");
            if multiline { input.multiline() } else { input }
        });
        input.update_in(cx, |input, window, cx| {
            window.focus(&input.focus_handle, cx)
        });
        (input, cx)
    }

    #[gpui::test]
    fn typing_editing_and_submit(cx: &mut TestAppContext) {
        let (input, cx) = setup(cx, true);
        let submitted = std::rc::Rc::new(std::cell::Cell::new(0));
        let s = submitted.clone();
        cx.update(|_, cx| {
            cx.subscribe(&input, move |_, event, _| {
                if let InputEvent::Submit = event {
                    s.set(s.get() + 1);
                }
            })
            .detach();
        });

        cx.simulate_input("hello");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("wörld");
        input.read_with(cx, |i, _| assert_eq!(i.text(), "hello\nwörld"));

        cx.simulate_keystrokes("backspace backspace");
        input.read_with(cx, |i, _| assert_eq!(i.text(), "hello\nwör"));

        cx.simulate_keystrokes("alt-backspace");
        input.read_with(cx, |i, _| assert_eq!(i.text(), "hello\n"));

        cx.simulate_keystrokes("cmd-a");
        cx.simulate_input("x");
        input.read_with(cx, |i, _| assert_eq!(i.text(), "x"));

        cx.simulate_keystrokes("enter");
        assert_eq!(submitted.get(), 1);
        input.read_with(cx, |i, _| assert_eq!(i.text(), "x"));
    }

    #[gpui::test]
    fn single_line_ignores_newlines(cx: &mut TestAppContext) {
        let (input, cx) = setup(cx, false);
        cx.simulate_input("a");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("b");
        input.read_with(cx, |i, _| assert_eq!(i.text(), "ab"));
        cx.simulate_keystrokes("left left");
        cx.simulate_input("_");
        input.read_with(cx, |i, _| assert_eq!(i.text(), "_ab"));
        cx.simulate_keystrokes("end");
        cx.simulate_input("!");
        input.read_with(cx, |i, _| assert_eq!(i.text(), "_ab!"));
    }

    #[gpui::test]
    fn grapheme_aware_backspace(cx: &mut TestAppContext) {
        let (input, cx) = setup(cx, false);
        input.update_in(cx, |i, _, cx| i.set_text("a👍🏽", cx));
        cx.simulate_keystrokes("backspace");
        input.read_with(cx, |i, _| assert_eq!(i.text(), "a"));
    }

    /// Regression: clicking an empty field hit-tested against the *placeholder* layout and
    /// stored an offset past the end of the (empty) content, crashing on the next paint.
    #[gpui::test]
    fn clicking_empty_and_masked_fields_does_not_panic(cx: &mut TestAppContext) {
        use gpui::{Modifiers, point};
        for masked in [false, true] {
            cx.update(bind_keys);
            let (input, vcx) = cx.add_window_view(move |_, cx| {
                let input = TextInput::new(cx, "A long placeholder text");
                if masked { input.masked() } else { input }
            });
            vcx.run_until_parked();
            for x in [5., 60., 150., 400.] {
                vcx.simulate_click(point(px(x), px(5.)), Modifiers::none());
                vcx.run_until_parked();
                input.read_with(vcx, |i, _| assert_eq!(i.selected_range, 0..0));
            }
            vcx.simulate_input("pä🙂");
            vcx.simulate_click(point(px(400.), px(5.)), Modifiers::none());
            vcx.run_until_parked();
            input.read_with(vcx, |i, _| {
                assert!(i.selected_range.end <= i.text().len());
                assert!(i.text().is_char_boundary(i.selected_range.end));
            });
            vcx.simulate_keystrokes("up down backspace");
            vcx.run_until_parked();
        }
    }

    #[test]
    fn scroll_keeps_cursor_visible() {
        let lh = px(20.);
        let viewport = size(px(100.), px(20.));
        let content = size(px(400.), px(20.));
        // Cursor at the far right of long single-line text scrolls left.
        let s = scroll_to_reveal(
            Point::default(),
            point(px(400.), px(0.)),
            lh,
            content,
            viewport,
        );
        assert_eq!(s.x, px(302.));
        // Moving back inside the viewport keeps the scroll.
        let s2 = scroll_to_reveal(s, point(px(350.), px(0.)), lh, content, viewport);
        assert_eq!(s2.x, s.x);
        // Moving before the viewport scrolls back.
        let s3 = scroll_to_reveal(s, point(px(10.), px(0.)), lh, content, viewport);
        assert_eq!(s3.x, px(10.));
        // Short text never scrolls.
        let s4 = scroll_to_reveal(
            point(px(50.), px(0.)),
            point(px(30.), px(0.)),
            lh,
            size(px(30.), px(20.)),
            viewport,
        );
        assert_eq!(s4, Point::default());

        // Multi-line: a 10-line viewport over 30 lines follows the cursor down and up.
        let viewport = size(px(300.), px(200.));
        let content = size(px(0.), px(600.));
        let s = scroll_to_reveal(
            Point::default(),
            point(px(0.), px(580.)),
            lh,
            content,
            viewport,
        );
        assert_eq!(s.y, px(400.));
        let s = scroll_to_reveal(s, point(px(0.), px(100.)), lh, content, viewport);
        assert_eq!(s.y, px(100.));
    }

    /// Typing far more text than fits must keep the cursor (and text) inside the field.
    #[gpui::test]
    fn long_text_scrolls_inside_field(cx: &mut TestAppContext) {
        cx.update(bind_keys);
        let (input, vcx) = cx.add_window_view(|_, cx| TextInput::new(cx, "p").masked());
        input.update_in(vcx, |i, window, cx| window.focus(&i.focus_handle, cx));
        vcx.simulate_input(&"x".repeat(400));
        vcx.run_until_parked();
        input.read_with(vcx, |i, _| {
            let layout = i.last_layout.as_ref().expect("painted");
            let field = layout.origin + i.scroll; // element origin
            assert!(i.scroll.x > Pixels::ZERO, "scrolled to follow the cursor");
            let cursor = layout
                .position_for_index(i.display_offset_for_content(i.cursor_offset()))
                .unwrap();
            let cursor_x = layout.origin.x + cursor.x;
            assert!(cursor_x >= field.x);
        });
        vcx.simulate_keystrokes("home");
        vcx.run_until_parked();
        input.read_with(vcx, |i, _| assert_eq!(i.scroll.x, Pixels::ZERO));
    }

    #[test]
    fn masked_offsets_roundtrip() {
        // Masking maps every char to a 3-byte bullet; offsets must map back exactly.
        let content = "pä🙂s";
        let to_display = |offset: usize| content[..offset].chars().count() * '•'.len_utf8();
        for (i, _) in content.char_indices() {
            let d = to_display(i);
            let back = content
                .char_indices()
                .nth(d / '•'.len_utf8())
                .map(|(j, _)| j)
                .unwrap_or(content.len());
            assert_eq!(back, i);
        }
    }
}
