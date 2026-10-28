//! macOS visual language: the system's semantic colours (sampled from AppKit's
//! dynamic colours on macOS 26, light + dark), SF Pro via the system font, and
//! the user's accent colour from System Settings. Follows the system
//! appearance; nothing here is hard-wired to one mode.
//!
//! Functions return the colour for the current appearance, so call sites stay
//! `theme::label()` etc. and re-render when the appearance flips.
#![allow(dead_code)]

use gpui::{App, Hsla, Rgba, WindowAppearance, rgb, rgba};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub const SANS: &str = ".SystemUIFont";

static DARK: AtomicBool = AtomicBool::new(true);
/// 0xRRGGBB of the user's accent, or u32::MAX for "multicolour" (use ours).
static ACCENT: AtomicU32 = AtomicU32::new(u32::MAX);

pub fn load_fonts(_cx: &mut App) {}

/// Re-read the system appearance + accent colour. Call at startup and from
/// window appearance observers.
pub fn sync(cx: &App) {
    // Dev: STENLY_APPEARANCE=light|dark forces one (screenshots).
    let dark = match std::env::var("STENLY_APPEARANCE").as_deref() {
        Ok("light") => false,
        Ok("dark") => true,
        _ => matches!(cx.window_appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark),
    };
    DARK.store(dark, Ordering::Relaxed);
    // Dev: STENLY_ACCENT=multicolor|graphite (screenshots).
    if std::env::var("STENLY_ACCENT").as_deref() == Ok("multicolor") {
        ACCENT.store(u32::MAX, Ordering::Relaxed);
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let v = stenly_platform::appkit_ui::accent_color()
            .map(|[r, g, b]| ((r * 255.).round() as u32) << 16 | ((g * 255.).round() as u32) << 8 | (b * 255.).round() as u32)
            .unwrap_or(u32::MAX);
        ACCENT.store(v, Ordering::Relaxed);
    }
}

pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

pub fn pick(light: Rgba, dark: Rgba) -> Rgba {
    if is_dark() { dark } else { light }
}

/// Accent when the user chose "Multicolour": macOS's default system blue.
fn brand_accent() -> Rgba {
    blue()
}

// ---------------------------------------------------------------- surfaces
/// windowBackgroundColor
pub fn bg() -> Rgba {
    pick(rgb(0xffffff), rgb(0x1e1e1e))
}
/// Sidebar tint drawn over the blurred window material.
pub fn sidebar() -> Rgba {
    pick(rgba(0xe8e8e8cc), rgba(0x2b2b2bb3))
}
/// Source-list selection (unemphasized: Stenly's window is rarely key while
/// you're in a call, and the emphasized accent fill is loud for navigation).
pub fn sidebar_selection() -> Rgba {
    pick(rgba(0x0000001a), rgba(0xffffff1f))
}
/// Grouped-form box (System Settings' rounded sections).
pub fn card() -> Rgba {
    pick(rgb(0xf6f6f6), rgb(0x262626))
}
pub fn card_hover() -> Rgba {
    pick(rgb(0xeeeeee), rgb(0x2c2c2c))
}
/// Text field / inset well background.
pub fn inset() -> Rgba {
    pick(rgb(0xffffff), rgba(0xffffff0d))
}
/// quaternarySystemFill-ish hover wash.
pub fn hover() -> Rgba {
    pick(rgba(0x0000000a), rgba(0xffffff0d))
}
/// unemphasizedSelectedContentBackground.
pub fn selected() -> Rgba {
    pick(rgba(0x00000014), rgba(0xffffff1a))
}
/// Box border (System Settings group outline).
pub fn border() -> Rgba {
    pick(rgba(0x00000012), rgba(0xffffff0f))
}
/// separatorColor
pub fn divider() -> Rgba {
    pick(rgba(0x0000001a), rgba(0xffffff19))
}
/// Search palette: an opaque floating panel, so content never shows through.
pub fn palette_bg() -> Rgba {
    pick(rgb(0xfbfbfb), rgb(0x2a2a2a))
}
/// Menus, popovers, floating panels.
pub fn popup_bg() -> Rgba {
    pick(rgba(0xf5f5f5f2), rgba(0x262626f2))
}

// ---------------------------------------------------------------- text
/// labelColor (85% opaque).
pub fn text() -> Rgba {
    pick(rgba(0x000000d9), rgba(0xffffffd9))
}
/// secondaryLabelColor (contrast-checked: ≥5:1 on window + group backgrounds).
pub fn muted() -> Rgba {
    pick(rgba(0x00000094), rgba(0xffffff8c))
}
/// tertiaryLabelColor, nudged up so it still clears 4.5:1 for real text.
pub fn faint() -> Rgba {
    pick(rgba(0x00000080), rgba(0xffffff73))
}
pub fn text_faint() -> Rgba {
    faint()
}

// ---------------------------------------------------------------- accent
/// controlAccentColor: the user's System Settings accent, or Stenly's own
/// when they chose Multicolour. Used for the default button, selection,
/// switches, links and focus — and nothing decorative.
pub fn accent() -> Rgba {
    match ACCENT.load(Ordering::Relaxed) {
        u32::MAX => brand_accent(),
        v => rgb(v),
    }
}

/// Fill for the default button / on switches: the accent, darkened until white
/// text on it clears 4.5:1 (graphite #8C8C8C → ~#6E6E6E, like AppKit's).
pub fn accent_fill() -> Rgba {
    let mut c = accent();
    for _ in 0..12 {
        if contrast(c, rgb(0xffffff)) >= 4.5 {
            break;
        }
        c = Rgba { r: c.r * 0.92, g: c.g * 0.92, b: c.b * 0.92, a: 1. };
    }
    c
}

fn lum(c: Rgba) -> f32 {
    let f = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
}
fn contrast(a: Rgba, b: Rgba) -> f32 {
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}
pub fn accent_strong() -> Rgba {
    accent()
}
pub fn accent_tint() -> Rgba {
    let a = accent();
    Rgba { a: if is_dark() { 0.22 } else { 0.14 }, ..a }
}
/// Text on an accent-filled control.
pub fn on_accent() -> Rgba {
    rgb(0xffffff)
}
/// Text drawn *in* the accent colour on a plain background (links, "In use").
/// Graphite/grey accents are too light for text, so fall back to the label.
pub fn accent_text() -> Rgba {
    let a = accent();
    let chroma = (a.r - a.g).abs().max((a.g - a.b).abs()).max((a.r - a.b).abs());
    if chroma < 0.08 { text() } else { a }
}

// ---------------------------------------------------------------- system colours
pub fn red() -> Rgba {
    pick(rgb(0xff383c), rgb(0xff4245))
}
pub fn red_tint() -> Rgba {
    Rgba { a: 0.12, ..red() }
}
pub fn green() -> Rgba {
    pick(rgb(0x34c759), rgb(0x30d158))
}
pub fn amber() -> Rgba {
    pick(rgb(0xc77c00), rgb(0xff9230))
}
pub fn amber_tint() -> Rgba {
    Rgba { a: 0.12, ..pick(rgb(0xff8d28), rgb(0xff9230)) }
}
pub fn blue() -> Rgba {
    pick(rgb(0x0088ff), rgb(0x0091ff))
}
pub fn orange() -> Rgba {
    pick(rgb(0xff8d28), rgb(0xff9230))
}
pub fn yellow() -> Rgba {
    pick(rgb(0xffcc00), rgb(0xffd600))
}
pub fn mint() -> Rgba {
    pick(rgb(0x00c8b3), rgb(0x00dac3))
}
pub fn teal() -> Rgba {
    pick(rgb(0x00c3d0), rgb(0x00d2e0))
}
pub fn cyan() -> Rgba {
    pick(rgb(0x00c0e8), rgb(0x3cd3fe))
}
pub fn indigo() -> Rgba {
    pick(rgb(0x6155f5), rgb(0x6d7cff))
}
pub fn purple() -> Rgba {
    pick(rgb(0xcb30e0), rgb(0xdb34f2))
}
pub fn pink() -> Rgba {
    pick(rgb(0xff2d55), rgb(0xff375f))
}
pub fn gray() -> Rgba {
    pick(rgb(0x8e8e93), rgb(0x98989d))
}

// ---------------------------------------------------------------- controls
/// Push-button face (controlColor over the window).
pub fn button() -> Rgba {
    pick(rgba(0x0000000f), rgba(0xffffff1f))
}
pub fn button_hover() -> Rgba {
    pick(rgba(0x00000017), rgba(0xffffff29))
}
pub fn on_button() -> Rgba {
    text()
}
/// Switch track when off.
pub fn toggle_off() -> Rgba {
    pick(rgba(0x0000001f), rgba(0xffffff26))
}
pub fn selection() -> Hsla {
    let a = accent();
    Rgba { a: 0.35, ..a }.into()
}

/// Parse "#rrggbb" (Google calendar colours).
pub fn hex(s: &str) -> Rgba {
    let v = u32::from_str_radix(s.trim_start_matches('#'), 16).unwrap_or(0x888888);
    rgb(v)
}

// ---------------------------------------------------------------- type scale
// macOS built-in text styles (points): large title 26, title1 22, title2 17,
// title3 15, headline 13 bold, body 13, callout 12, subheadline 11,
// footnote 10, caption 10.
pub const LARGE_TITLE: f32 = 26.;
pub const TITLE1: f32 = 22.;
pub const TITLE2: f32 = 17.;
pub const TITLE3: f32 = 15.;
pub const BODY: f32 = 13.;
pub const CALLOUT: f32 = 12.;
pub const SUBHEAD: f32 = 11.;
pub const FOOTNOTE: f32 = 10.;
