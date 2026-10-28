//! macOS control vocabulary, drawn in GPUI to match AppKit on macOS 26:
//! push buttons (6pt corners, 24pt regular / 20pt small), mini switches,
//! pop-up buttons that open a real NSMenu, System Settings-style grouped
//! forms with full-width separators, and sidebar rows.
#![allow(dead_code)]

use crate::icons::{self, Sym, icon};
use crate::theme;
use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, FontWeight, IntoElement, ParentElement, SharedString, Stateful, Styled,
    Window, div, prelude::*, px,
};

/// Window / page title (Large Title, bold).
pub fn page_title(text: impl Into<SharedString>) -> Div {
    div().text_size(px(theme::LARGE_TITLE)).line_height(px(32.)).font_weight(FontWeight::BOLD).text_color(theme::text()).child(text.into())
}

/// Kept for call sites that want a title at a specific size (SF Pro, bold).
pub fn serif_title(text: impl Into<SharedString>, size: f32) -> Div {
    div().text_size(px(size)).line_height(px(size * 1.2)).font_weight(FontWeight::BOLD).text_color(theme::text()).child(text.into())
}

/// Section heading above a grouped form: 13pt semibold, aligned with the
/// row text inside the box (box padding 4 + row padding 12 = 16).
pub fn caption(text: impl Into<SharedString>) -> Div {
    div().pl(px(16.)).pb(px(6.)).pt(px(24.)).text_size(px(theme::BODY)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(text.into())
}

/// Grouped form box (same grammar as the Home lists): 12pt corners, 4pt
/// inner padding, rows carry their own 12pt horizontal padding, hairlines
/// inset to the text column.
pub fn card(rows: Vec<AnyElement>) -> Div {
    let n = rows.len();
    let mut c = div().flex().flex_col().rounded(px(12.)).bg(theme::card()).p(px(4.));
    for (i, row) in rows.into_iter().enumerate() {
        c = c.child(div().px(px(12.)).child(row));
        if i + 1 < n {
            c = c.child(div().mx(px(12.)).h(px(1.)).bg(theme::divider()));
        }
    }
    c
}

/// Leading icon in a form row: System Settings' coloured rounded square, the
/// colour fixed per meaning so rows are recognisable at a glance.
pub fn icon_tile(s: Sym) -> Div {
    color_tile(s, tile_color(s), 22.)
}

/// One system colour per concept, used consistently across the app.
pub fn tile_color(s: Sym) -> gpui::Rgba {
    use icons::*;
    match s {
        x if x == MIC || x == SPEAKER || x == WAVE || x == RECORD => theme::red(),
        x if x == CALENDAR || x == USER_X => theme::orange(),
        x if x == VIDEO || x == LINK || x == GLOBE => theme::green(),
        x if x == SPARKLES || x == CPU => theme::indigo(),
        x if x == FILE_TEXT || x == FOLDER || x == ARCHIVE => theme::blue(),
        x if x == SHIELD || x == LOCK || x == ACCESS => theme::blue(),
        x if x == TRASH => theme::gray(),
        x if x == CHECK_CIRCLE => theme::green(),
        x if x == ALERT => theme::orange(),
        _ => theme::gray(),
    }
}

/// System Settings icon: white SF Symbol on a 20pt rounded square of a fixed
/// system colour (HIG: fixed sidebar colours are fine when they aid recognition).
pub fn color_tile(s: Sym, _color: gpui::Rgba, size: f32) -> Div {
    // macOS 26 "clear" icon style: one neutral glassy tile for every glyph.
    // Dark: graphite gradient, light top → near-black bottom, white glyph.
    // Light: pale silver gradient with a dark glyph. Both get a hairline rim.
    let (top, bottom, rim, glyph) = if theme::is_dark() {
        (gpui::rgb(0x3c3c3e), gpui::rgb(0x1c1c1e), gpui::rgba(0xffffff26), gpui::rgb(0xf2f2f7))
    } else {
        (gpui::rgb(0xffffff), gpui::rgb(0xe3e3e8), gpui::rgba(0x0000001f), gpui::rgb(0x1c1c1e))
    };
    div()
        .size(px(size))
        .flex_shrink_0()
        .rounded(px(size * 0.26))
        .bg(gpui::linear_gradient(180., gpui::linear_color_stop(top, 0.), gpui::linear_color_stop(bottom, 1.)))
        .border_1()
        .border_color(rim)
        .flex()
        .items_center()
        .justify_center()
        .child(icon(s, size * 0.58, glyph))
}

/// A form row: title / description ………… trailing control. 48pt min
/// height so single- and two-line rows share a rhythm; no leading icon (the
/// section heading already says what these are).
pub fn row(_icon: Option<Sym>, title: impl Into<SharedString>, desc: Option<SharedString>, trailing: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(16.))
        .min_h(px(48.))
        .py(px(9.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_size(px(theme::BODY)).text_color(theme::text()).child(title.into()))
                .when_some(desc, |d, s| d.child(div().text_size(px(theme::SUBHEAD)).line_height(px(15.)).text_color(theme::muted()).child(s))),
        )
        .child(div().flex_shrink_0().flex().items_center().gap(px(8.)).child(trailing))
}

/// A file path row: single line, start-ellipsis so the meaningful tail stays.
pub fn path_row(_icon: Option<Sym>, title: impl Into<SharedString>, path: impl Into<SharedString>, trailing: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(16.))
        .min_h(px(48.))
        .py(px(9.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_size(px(theme::BODY)).text_color(theme::text()).child(title.into()))
                .child(div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).overflow_hidden().whitespace_nowrap().text_ellipsis_start().child(path.into())),
        )
        .child(div().flex_shrink_0().flex().items_center().gap(px(8.)).child(trailing))
}

/// NSSwitch, mini size (HIG: mini switches in a grouped form so row heights
/// match other controls). Track fills with the accent when on.
pub fn toggle(id: impl Into<ElementId>, on: bool, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(32.))
        .h(px(18.))
        .flex_shrink_0()
        .rounded_full()
        .p(px(1.5))
        .flex()
        .when(on, |d| d.bg(theme::accent_fill()).justify_end())
        .when(!on, |d| d.bg(theme::toggle_off()).justify_start())
        .child(div().size(px(15.)).rounded_full().bg(gpui::rgb(0xffffff)).shadow(vec![gpui::BoxShadow { color: gpui::hsla(0., 0., 0., 0.25), offset: gpui::point(px(0.), px(0.5)), blur_radius: px(1.5), spread_radius: px(0.), inset: false }]))
        .on_click(f)
}

#[derive(Clone, Copy, PartialEq)]
pub enum Btn {
    /// Default button (Return): accent-filled.
    Primary,
    /// Accent-filled too; kept as an alias for "the one action this row wants".
    Accent,
    /// Standard push button.
    Ghost,
    /// Standard push button (macOS has no separate outlined style).
    Outline,
    /// Destructive: push button with red label.
    Danger,
}

pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: Btn) -> Stateful<Div> {
    button_base(id, kind).child(label.into())
}

fn button_base(id: impl Into<ElementId>, kind: Btn) -> Stateful<Div> {
    let d = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .gap(px(5.))
        .h(px(24.))
        .px(px(12.))
        // macOS 26 push buttons are capsules, matching the toolbar's Record capsule.
        .rounded_full()
        .text_size(px(theme::BODY))
        .flex_shrink_0()
        .whitespace_nowrap();
    match kind {
        Btn::Primary | Btn::Accent => d.bg(theme::accent_fill()).text_color(theme::on_accent()).shadow(bezel_shadow()).active(|s| s.opacity(0.85)),
        Btn::Ghost | Btn::Outline => d.bg(theme::button()).text_color(theme::text()).shadow(bezel_shadow()).active(|s| s.bg(theme::button_hover())),
        Btn::Danger => d.bg(theme::button()).text_color(theme::red()).shadow(bezel_shadow()).active(|s| s.bg(theme::button_hover())),
    }
}

/// AppKit push-button depth: a 0.5pt drop at low opacity (light mode only;
/// dark-mode bezels are flat).
fn bezel_shadow() -> Vec<gpui::BoxShadow> {
    vec![]
}

fn btn_icon_color(kind: Btn) -> gpui::Rgba {
    match kind {
        Btn::Primary | Btn::Accent => theme::on_accent(),
        Btn::Danger => theme::red(),
        _ => theme::text(),
    }
}

/// Push button with a trailing glyph.
pub fn button_icon(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: Btn, s: Sym) -> Stateful<Div> {
    button(id, label, kind).child(icon(s, 12., btn_icon_color(kind)))
}

/// Push button with a leading glyph (toolbar-ish "‹ Back").
pub fn button_lead_icon(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: Btn, s: Sym) -> Stateful<Div> {
    let label: SharedString = label.into();
    button_base(id, kind).pl(px(8.)).child(icon(s, 12., btn_icon_color(kind))).child(label)
}

/// Borderless toolbar button: glyph only, hover wash (NSToolbar item).
pub fn toolbar_button(id: impl Into<ElementId>, s: Sym, active: bool) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .when(active, |d| d.bg(theme::selected()))
        .when(!active, |d| d.hover(|s| s.bg(theme::hover())))
        .child(icon(s, 15., theme::muted()))
}

/// NSPopUpButton look: current value + up/down chevrons. Clicking opens a
/// real NSMenu (macOS) at the control, checked on the current value.
pub fn popup<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: &[(T, &'static str)],
    current: T,
    on_pick: impl Fn(T, &mut App) + 'static,
) -> Stateful<Div> {
    let label = options.iter().find(|(v, _)| *v == current).map(|(_, l)| *l).unwrap_or("—");
    let opts: Vec<(T, &'static str)> = options.to_vec();
    let sel = opts.iter().position(|(v, _)| *v == current);
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(24.))
        .pl(px(10.))
        .pr(px(5.))
        .rounded_full()
        .bg(theme::button())
        .shadow(bezel_shadow())
        .flex_shrink_0()
        .text_size(px(theme::BODY))
        .text_color(theme::text())
        .active(|s| s.bg(theme::button_hover()))
        .child(label)
        .child(icon(icons::CHEVRON_UPDOWN, 10., theme::muted()))
        .on_click({
            let on_pick = std::rc::Rc::new(on_pick);
            move |e, window, cx| {
                // Same as the meeting menu: run NSMenu's nested event loop with
                // nothing borrowed, or queued GPUI tasks panic inside it.
                let pos = e.mouse_position().unwrap_or_else(|| window.mouse_position());
                let (opts, on_pick) = (opts.clone(), on_pick.clone());
                let view = native_view(window);
                window
                    .spawn(cx, async move |cx| {
                        let labels: Vec<&str> = opts.iter().map(|(_, l)| *l).collect();
                        if let Some(i) = menu_in_view(view, &labels, sel, pos) {
                            if Some(i) != sel {
                                let _ = cx.update(|_, cx| on_pick(opts[i].0, cx));
                            }
                        }
                    })
                    .detach();
            }
        })
}

/// The window's native view, as an opaque handle for [`menu_in_view`].
pub fn native_view(window: &Window) -> Option<usize> {
    #[cfg(target_os = "macos")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let h = HasWindowHandle::window_handle(window).ok()?;
        if let RawWindowHandle::AppKit(a) = h.as_raw() {
            return Some(a.ns_view.as_ptr() as usize);
        }
        None
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        None
    }
}

/// Show a native menu in `view` at `pos` and return the picked index.
///
/// MUST be called with no GPUI borrow held (from an async task, outside
/// `cx.update`): NSMenu tracking runs a nested run loop that keeps draining
/// GPUI's main-thread queue, and any queued task that borrows the app while
/// we hold it aborts the process ("RefCell already borrowed").
pub fn menu_in_view(view: Option<usize>, items: &[&str], selected: Option<usize>, pos: gpui::Point<gpui::Pixels>) -> Option<usize> {
    #[cfg(target_os = "macos")]
    {
        let v = view?;
        stenly_platform::appkit_ui::popup_menu(v as *mut std::ffi::c_void, items, selected, f32::from(pos.x) as f64 - 10., f32::from(pos.y) as f64, 0.)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (view, pos);
        Some(selected.map(|i| (i + 1) % items.len()).unwrap_or(0))
    }
}

/// One row of a [`rich_menu_in_view`] menu.
#[derive(Debug, Clone, Default)]
pub struct MenuEntry {
    /// "-" = separator.
    pub title: String,
    pub subtitle: Option<String>,
    /// SF Symbol name (template-tinted, so neutral). Also a fallback if image decoding fails.
    pub symbol: Option<&'static str>,
    /// Bundled image bytes, rendered in their original colors instead of template-tinted.
    pub image: Option<&'static [u8]>,
    pub enabled: bool,
    pub checked: bool,
    pub header: bool,
}

impl MenuEntry {
    pub fn item(title: impl Into<String>) -> Self {
        Self { title: title.into(), enabled: true, ..Default::default() }
    }
    pub fn header(title: impl Into<String>) -> Self {
        Self { title: title.into(), header: true, ..Default::default() }
    }
    pub fn separator() -> Self {
        Self { title: "-".into(), ..Default::default() }
    }
}

/// Like [`menu_in_view`] with section headers, subtitles and icons. Same
/// rule: call it with no GPUI borrow held.
pub fn rich_menu_in_view(view: Option<usize>, entries: &[MenuEntry], pos: gpui::Point<gpui::Pixels>) -> Option<usize> {
    #[cfg(target_os = "macos")]
    {
        let v = view?;
        let native: Vec<stenly_platform::appkit_ui::MenuEntry> = entries
            .iter()
            .map(|e| stenly_platform::appkit_ui::MenuEntry {
                title: e.title.clone(),
                subtitle: e.subtitle.clone(),
                symbol: e.symbol.map(str::to_string),
                image: e.image,
                enabled: e.enabled,
                checked: e.checked,
                header: e.header,
            })
            .collect();
        stenly_platform::appkit_ui::popup_menu_rich(v as *mut std::ffi::c_void, &native, f32::from(pos.x) as f64 - 10., f32::from(pos.y) as f64)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (view, pos);
        entries.iter().position(|e| e.enabled && !e.header && e.title != "-")
    }
}

/// A row's "…" button: opens a native menu of the row's actions and calls
/// `on_pick` with the chosen index. List rows show at most one inline action;
/// everything else goes here (System Settings → Wi-Fi).
pub fn more_button(id: impl Into<ElementId>, entries: Vec<MenuEntry>, on_pick: impl Fn(usize, &mut Window, &mut App) + 'static) -> Stateful<Div> {
    let entries = std::rc::Rc::new(entries);
    let on_pick = std::rc::Rc::new(on_pick);
    icon_button(id, icons::ELLIPSIS, 22., 12., "More").on_click(move |e, window, cx| {
        cx.stop_propagation();
        // Open the NSMenu from a spawned task with nothing borrowed.
        let pos = e.mouse_position().unwrap_or_else(|| window.mouse_position());
        let view = native_view(window);
        let (entries, on_pick) = (entries.clone(), on_pick.clone());
        window
            .spawn(cx, async move |cx| {
                let Some(i) = rich_menu_in_view(view, &entries, pos) else { return };
                let _ = cx.update(|window, cx| on_pick(i, window, cx));
            })
            .detach();
    })
}

/// Open a native context/pop-up menu at `pos` (window coords). Returns the
/// chosen index. Falls back to cycling on platforms without NSMenu.
pub fn native_menu(window: &Window, items: &[&str], selected: Option<usize>, pos: gpui::Point<gpui::Pixels>) -> Option<usize> {
    #[cfg(target_os = "macos")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(h) = HasWindowHandle::window_handle(window) {
            if let RawWindowHandle::AppKit(a) = h.as_raw() {
                return stenly_platform::appkit_ui::popup_menu(a.ns_view.as_ptr(), items, selected, f32::from(pos.x) as f64 - 10., f32::from(pos.y) as f64, 0.);
            }
        }
        None
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (window, pos);
        Some(selected.map(|i| (i + 1) % items.len()).unwrap_or(0))
    }
}

/// Kept for compatibility: renders as a pop-up button (HIG: pop-up for a flat
/// list of mutually exclusive options in a form).
pub fn segmented<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: &[(T, &'static str)],
    current: T,
    on_pick: impl Fn(T, &mut App) + Clone + 'static,
) -> Stateful<Div> {
    popup(id, options, current, on_pick)
}

/// A spinning progress indicator.
pub fn spinner(id: impl Into<ElementId>, size: f32, color: gpui::Rgba) -> impl IntoElement {
    use gpui::{Animation, AnimationExt};
    let _ = color;
    // NSProgressIndicator (spinning) approximated with a rotating arc of dots.
    div().size(px(size)).child(
        div()
            .size(px(size))
            .rounded_full()
            .border_2()
            .border_color(theme::divider())
            // One slow breath every 2 s (it was a blink every ~0.45 s, which
            // read as flicker next to text that sits there for minutes).
            .with_animation(id, Animation::new(std::time::Duration::from_millis(2000)).repeat().with_easing(gpui::pulsating_between(0.0, 1.0)), move |d, t| {
                d.border_color(gpui::Rgba { a: 0.3 + 0.45 * t, ..theme::muted() })
            }),
    )
}

/// "Waiting…" status with a spinner (same footprint as a button).
pub fn pending(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(7.))
        .h(px(24.))
        .text_size(px(theme::BODY))
        .text_color(theme::muted())
        .child(spinner(id, 12., theme::muted()))
        .child(label.into())
}

/// Status text with a glyph ("✓ Allowed", "In use"): no pill — macOS forms
/// show status as plain secondary text beside an SF Symbol.
pub fn chip(label: impl Into<SharedString>, data: Option<Sym>, color: gpui::Rgba) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .h(px(24.))
        .text_size(px(theme::BODY))
        .text_color(theme::muted())
        .when_some(data, |d, i| d.child(crate::icons::icon_w(i, 11., crate::icons::Weight::Semibold, color)))
        .child(label.into())
}

/// A rounded tag with an × that removes it (vocabulary, your addresses).
pub fn removable_chip(id: impl Into<ElementId> + Clone, label: impl Into<SharedString>, on_remove: impl Fn(&mut gpui::App) + 'static) -> gpui::AnyElement {
    let id: ElementId = id.into();
    div()
        .id(id.clone())
        .flex()
        .items_center()
        .gap(px(4.))
        .h(px(22.))
        .pl(px(8.))
        .pr(px(4.))
        .rounded(px(11.))
        .bg(theme::inset())
        .border_1()
        .border_color(theme::divider())
        .text_size(px(theme::SUBHEAD))
        .child(label.into())
        .child(
            div()
                .id(ElementId::NamedChild(std::sync::Arc::new(id), "x".into()))
                .size(px(14.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .hover(|s| s.bg(theme::hover()))
                .child(crate::icons::icon(crate::icons::X, 8., theme::muted()))
                .on_click(move |_, _, cx| on_remove(cx)),
        )
        .into_any_element()
}

/// ●●●●○ 9.4 style score.
pub fn dots(n: u8, of: u8) -> Div {
    div().flex().gap(px(3.)).children((0..of).map(|i| div().size(px(6.)).rounded_full().bg(if i < n { theme::muted() } else { theme::toggle_off() })))
}

pub fn meta(s: Sym, text: impl Into<SharedString>) -> Div {
    div().flex().items_center().gap(px(3.)).text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(icon(s, 11., theme::muted())).child(text.into())
}

/// NSProgressIndicator (bar).
pub fn progress_bar(frac: f32, width: f32) -> Div {
    div()
        .w(px(width))
        .h(px(4.))
        .rounded_full()
        .bg(theme::toggle_off())
        .child(div().h_full().rounded_full().bg(theme::accent_fill()).w(px(width * frac.clamp(0.0, 1.0))))
}

/// NSLevelIndicator-ish meter (0..1, pre-smoothed).
pub fn meter_bar(frac: f32, width: f32) -> Div {
    div().w(px(width)).h(px(4.)).rounded_full().bg(theme::toggle_off()).child(div().h_full().rounded_full().bg(theme::green()).w(px(width * frac.clamp(0.0, 1.0))))
}

pub fn level_bar(db: f32, width: f32) -> Div {
    meter_bar(((db + 60.0) / 60.0).clamp(0.0, 1.0), width)
}

/// Text field chrome (NSTextField, rounded bezel).
pub fn text_field(child: impl IntoElement) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .h(px(24.))
        .px(px(7.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .bg(theme::inset())
        .border_1()
        .border_color(theme::divider())
        .text_size(px(theme::BODY))
        .child(child)
}

/// Day heading for a list of meetings: Today, Yesterday, "Wed, Oct 21", or
/// "Wed, Oct 21, 2025" for another year.
pub fn day_label(day: chrono::NaiveDate, today: chrono::NaiveDate) -> String {
    use chrono::Datelike;
    if day == today {
        "Today".into()
    } else if Some(day) == today.pred_opt() {
        "Yesterday".into()
    } else if day.year() == today.year() {
        day.format("%a, %b %-d").to_string()
    } else {
        day.format("%a, %b %-d, %Y").to_string()
    }
}

/// Small borderless icon button (row and card actions), with an accessible
/// label. Rounded-rect; call `.rounded_full()` on it for a circle.
pub fn icon_button(id: impl Into<ElementId>, s: Sym, size: f32, icon_size: f32, label: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(size))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .hover(|d| d.bg(theme::hover()))
        .active(|d| d.bg(theme::button_hover()))
        .role(gpui::Role::Button)
        .aria_label(label)
        .child(icon(s, icon_size, theme::muted()))
}
