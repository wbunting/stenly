//! Icons are SF Symbols, rendered by AppKit at the right point size, weight
//! and tint and cached as images (HIG: use SF Symbols for interface icons).
//! Each `Sym` also carries a Lucide outline used on platforms without SF
//! Symbols (Linux), so call sites are platform-neutral.
#![allow(dead_code)]

use gpui::{AnyElement, Hsla, IntoElement, ParentElement, Rgba, Styled, px, svg};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Sym {
    /// SF Symbols name.
    pub sf: &'static str,
    /// Lucide fallback (inline SVG).
    lucide: &'static [u8],
}

macro_rules! lucide {
    ($body:literal) => {
        concat!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">"##,
            $body,
            "</svg>"
        )
        .as_bytes()
    };
}
macro_rules! sym {
    ($name:ident, $sf:literal, $body:literal) => {
        pub const $name: Sym = Sym { sf: $sf, lucide: lucide!($body) };
    };
}

sym!(HOME, "house", r#"<path d="M15 21v-8a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1v8"/><path d="M3 10a2 2 0 0 1 .709-1.528l7-5.999a2 2 0 0 1 2.582 0l7 5.999A2 2 0 0 1 21 10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>"#);
sym!(SLIDERS, "slider.horizontal.3", r#"<path d="M10 5H3"/><path d="M12 19H3"/><path d="M14 3v4"/><path d="M16 17v4"/><path d="M21 12h-9"/><path d="M21 19h-5"/><path d="M21 5h-7"/><path d="M8 10v4"/><path d="M8 12H3"/>"#);
sym!(CALENDAR, "calendar", r#"<path d="M8 2v4"/><path d="M16 2v4"/><rect width="18" height="18" x="3" y="4" rx="2"/><path d="M3 10h18"/>"#);
sym!(CPU, "cpu", r#"<rect width="16" height="16" x="4" y="4" rx="2"/><rect width="6" height="6" x="9" y="9" rx="1"/><path d="M15 2v2"/><path d="M15 20v2"/><path d="M2 15h2"/><path d="M2 9h2"/><path d="M20 15h2"/><path d="M20 9h2"/><path d="M9 2v2"/><path d="M9 20v2"/>"#);
sym!(FOLDER, "folder", r#"<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/>"#);
sym!(SHIELD, "hand.raised.fill", r#"<path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z"/>"#);
sym!(MIC, "mic", r#"<path d="M12 19v3"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><rect x="9" y="2" width="6" height="13" rx="3"/>"#);
sym!(SPEAKER, "speaker.wave.2", r#"<path d="M11 4.702a.705.705 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.705.705 0 0 0 11 19.298z"/><path d="M16 9a5 5 0 0 1 0 6"/><path d="M19.364 18.364a9 9 0 0 0 0-12.728"/>"#);
sym!(ACCESS, "accessibility", r#"<circle cx="16" cy="4" r="1"/><path d="m18 19 1-7-6 1"/><path d="m5 8 3-3 5.5 3-2.36 3.5"/>"#);
sym!(CHEVRON_RIGHT, "chevron.right", r#"<path d="m9 18 6-6-6-6"/>"#);
sym!(CHEVRON_LEFT, "chevron.left", r#"<path d="m15 18-6-6 6-6"/>"#);
sym!(CHEVRON_DOWN, "chevron.down", r#"<path d="m6 9 6 6 6-6"/>"#);
sym!(CHEVRON_UPDOWN, "chevron.up.chevron.down", r#"<path d="m7 15 5 5 5-5"/><path d="m7 9 5-5 5 5"/>"#);
sym!(CHECK, "checkmark", r#"<path d="M20 6 9 17l-5-5"/>"#);
sym!(CHECK_CIRCLE, "checkmark.circle.fill", r#"<circle cx="12" cy="12" r="10"/><path d="m9 12 2 2 4-4"/>"#);
sym!(DOWNLOAD, "arrow.down.circle", r#"<path d="M12 15V3"/><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><path d="m7 10 5 5 5-5"/>"#);
sym!(TRASH, "trash", r#"<path d="M10 11v6"/><path d="M14 11v6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/><path d="M3 6h18"/><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>"#);
sym!(UNDO, "arrow.uturn.backward", r#"<path d="M9 14 4 9l5-5"/><path d="M4 9h10.5a5.5 5.5 0 0 1 5.5 5.5a5.5 5.5 0 0 1-5.5 5.5H11"/>"#);
sym!(LINK, "link", r#"<path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/>"#);
sym!(VIDEO, "video", r#"<path d="m16 13 5.223 3.482a.5.5 0 0 0 .777-.416V7.87a.5.5 0 0 0-.752-.432L16 10.5"/><rect x="2" y="6" width="14" height="12" rx="2"/>"#);
sym!(USER_X, "video.slash", r#"<path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="m17 8 5 5"/><path d="m22 8-5 5"/>"#);
sym!(MENUBAR, "menubar.rectangle", r#"<rect width="18" height="18" x="3" y="3" rx="2"/><path d="M3 9h18"/>"#);
sym!(POWER, "power", r#"<path d="M12 2v10"/><path d="M18.4 6.6a9 9 0 1 1-12.77.04"/>"#);
sym!(FILE_TEXT, "doc.text", r#"<path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"/><path d="M14 2v4a2 2 0 0 0 2 2h4"/><path d="M16 13H8"/><path d="M16 17H8"/>"#);
sym!(ARCHIVE, "books.vertical", r#"<rect width="20" height="5" x="2" y="3" rx="1"/><path d="M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8"/><path d="M10 12h4"/>"#);
sym!(SPARKLES, "sparkles", r#"<path d="M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0z"/>"#);
sym!(WAVE, "waveform", r#"<path d="M2 13a2 2 0 0 0 2-2V7a2 2 0 0 1 4 0v13a2 2 0 0 0 4 0V4a2 2 0 0 1 4 0v13a2 2 0 0 0 4 0v-4a2 2 0 0 1 2-2"/>"#);
sym!(HARD_DRIVE, "internaldrive", r#"<line x1="22" x2="2" y1="12" y2="12"/><path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"/>"#);
sym!(GLOBE, "globe", r#"<circle cx="12" cy="12" r="10"/><path d="M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20"/><path d="M2 12h20"/>"#);
sym!(REFRESH, "arrow.clockwise", r#"<path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"/><path d="M21 3v5h-5"/>"#);
sym!(EXTERNAL, "arrow.up.forward.app", r#"<path d="M15 3h6v6"/><path d="M10 14 21 3"/><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h3"/>"#);
sym!(CODE, "chevron.left.forwardslash.chevron.right", r#"<path d="m16 18 6-6-6-6"/><path d="m8 6-6 6 6 6"/>"#);
sym!(CIRCLE, "circle", r#"<circle cx="12" cy="12" r="9"/>"#);
sym!(USERS, "person.2", r#"<path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/>"#);
sym!(TAG, "tag", r#"<path d="M12.586 2.586A2 2 0 0 0 11.172 2H4a2 2 0 0 0-2 2v7.172a2 2 0 0 0 .586 1.414l8.704 8.704a2.426 2.426 0 0 0 3.42 0l6.58-6.58a2.426 2.426 0 0 0 0-3.42z"/>"#);
sym!(MORE, "ellipsis.circle", r#"<circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/><circle cx="5" cy="12" r="1"/>"#);
sym!(COPY, "doc.on.doc", r#"<rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/>"#);
sym!(LOADER, "arrow.triangle.2.circlepath", r#"<path d="M21 12a9 9 0 1 1-6.219-8.56"/>"#);
sym!(ALERT, "exclamationmark.triangle", r#"<circle cx="12" cy="12" r="10"/><path d="M12 8v4"/><path d="M12 16h.01"/>"#);
sym!(RECORD, "record.circle", r#"<circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="4"/>"#);
sym!(STOP, "stop.fill", r#"<rect x="6" y="6" width="12" height="12" rx="2"/>"#);
sym!(GEAR, "gearshape", r#"<circle cx="12" cy="12" r="3"/><path d="M12 2v3"/><path d="M12 19v3"/>"#);
sym!(ELLIPSIS, "ellipsis", r#"<circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/><circle cx="5" cy="12" r="1"/>"#);
sym!(PLUS, "plus", r#"<path d="M12 5v14"/><path d="M5 12h14"/>"#);
sym!(X, "xmark", r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#);
sym!(KEYBOARD, "keyboard", r#"<path d="M10 8h.01"/><path d="M12 12h.01"/><path d="M14 8h.01"/><path d="M16 12h.01"/><path d="M18 8h.01"/><path d="M6 8h.01"/><path d="M7 16h10"/><path d="M8 12h.01"/><rect width="20" height="16" x="2" y="4" rx="2"/>"#);
sym!(CLOCK, "clock", r#"<circle cx="12" cy="12" r="10"/><path d="M12 6v6l4 2"/>"#);
sym!(ARROW_UP, "chevron.up", r#"<path d="m18 15-6-6-6 6"/>"#);
sym!(QUOTE, "text.quote", r#"<path d="M17 6H3"/><path d="M21 12H8"/><path d="M21 18H8"/><path d="M3 12v6"/>"#);
sym!(SEARCH, "magnifyingglass", r#"<circle cx="11" cy="11" r="8"/><path d="m21 21-4.34-4.34"/>"#);
sym!(SIDEBAR, "sidebar.left", r#"<rect width="18" height="18" x="3" y="3" rx="2"/><path d="M9 3v18"/>"#);
sym!(RECIPE, "text.badge.star", r#"<path d="M3 6h11"/><path d="M3 12h8"/><path d="M3 18h6"/><path d="m17.5 11 1.4 2.8 3.1.5-2.2 2.2.5 3.1-2.8-1.5-2.8 1.5.5-3.1-2.2-2.2 3.1-.5z"/>"#);
sym!(LOCK, "lock", r#"<rect width="18" height="11" x="3" y="11" rx="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/>"#);
sym!(PAPERPLANE, "paperplane", r#"<path d="M14.536 21.686a.5.5 0 0 0 .937-.024l6.5-19a.496.496 0 0 0-.635-.635l-19 6.5a.5.5 0 0 0-.024.937l7.93 3.18a2 2 0 0 1 1.112 1.11z"/><path d="m21.854 2.147-10.94 10.939"/>"#);
sym!(LAPTOP, "laptopcomputer", r#"<path d="M18 5a2 2 0 0 1 2 2v8.526a2 2 0 0 0 .212.897l1.068 2.127a1 1 0 0 1-.9 1.45H3.62a1 1 0 0 1-.9-1.45l1.068-2.127A2 2 0 0 0 4 15.526V7a2 2 0 0 1 2-2z"/><path d="M20.054 15.987H3.946"/>"#);
sym!(CLOUD, "cloud", r#"<path d="M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9Z"/>"#);

/// SF Symbols weight on AppKit's NSFontWeight scale.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Weight {
    Regular,
    Medium,
    Semibold,
}

impl Weight {
    fn ns(self) -> f64 {
        match self {
            Weight::Regular => 0.0,
            Weight::Medium => 0.23,
            Weight::Semibold => 0.3,
        }
    }
}

type Key = (&'static str, u32, Weight, u32);
static CACHE: Mutex<Option<HashMap<Key, Option<Arc<gpui::RenderImage>>>>> = Mutex::new(None);

fn rgba_u32(c: Rgba) -> u32 {
    let b = |v: f32| (v.clamp(0., 1.) * 255.).round() as u32;
    b(c.r) << 24 | b(c.g) << 16 | b(c.b) << 8 | b(c.a)
}

#[cfg(target_os = "macos")]
fn render(s: Sym, size: f32, weight: Weight, c: Rgba) -> Option<Arc<gpui::RenderImage>> {
    let key = (s.sf, (size * 4.) as u32, weight, rgba_u32(c));
    let mut guard = CACHE.lock().ok()?;
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(v) = cache.get(&key) {
        return v.clone();
    }
    let img = stenly_platform::appkit_ui::render_symbol(s.sf, size as f64, weight.ns(), [c.r as f64, c.g as f64, c.b as f64, c.a as f64], 2.0).and_then(|(w, h, mut px)| {
        // GPUI's image pipeline wants BGRA.
        for p in px.chunks_exact_mut(4) {
            p.swap(0, 2);
        }
        let buf = image::RgbaImage::from_raw(w, h, px)?;
        let frame = image::Frame::new(buf);
        Some(Arc::new(gpui::RenderImage::new(smallvec::smallvec![frame])))
    });
    cache.insert(key, img.clone());
    img
}

#[cfg(not(target_os = "macos"))]
fn render(_: Sym, _: f32, _: Weight, _: Rgba) -> Option<Arc<gpui::RenderImage>> {
    None
}

/// An icon at `size` points tinted `color`, regular weight.
pub fn icon(s: Sym, size: f32, color: impl Into<Rgba>) -> AnyElement {
    icon_w(s, size, Weight::Regular, color)
}

pub fn icon_w(s: Sym, size: f32, weight: Weight, color: impl Into<Rgba>) -> AnyElement {
    let color: Rgba = color.into();
    match render(s, size, weight, color) {
        // SF Symbols have their own (non-square) metrics; let the image size itself
        // from its 2× pixels and centre it in a square box like a toolbar glyph.
        Some(img) => {
            let sz = img.size(0);
            let (w, h) = (sz.width.0 as f32 / 2.0, sz.height.0 as f32 / 2.0);
            gpui::div()
                .flex_shrink_0()
                .size(px(size * 1.25))
                .flex()
                .items_center()
                .justify_center()
                .child(gpui::img(img).w(px(w)).h(px(h)))
                .into_any_element()
        }
        None => {
            let hsla: Hsla = color.into();
            svg().data(s.lucide).size(px(size)).flex_shrink_0().text_color(hsla).into_any_element()
        }
    }
}

/// The app icon (full colour PNG).
pub fn logo(size: f32) -> gpui::Img {
    use std::sync::LazyLock;
    static LOGO: LazyLock<Arc<gpui::Image>> =
        LazyLock::new(|| Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, include_bytes!("../assets/logo-192.png").to_vec())));
    gpui::img(LOGO.clone()).size(px(size)).flex_shrink_0()
}
