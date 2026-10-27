//! AppKit pieces the GPUI UI borrows so it reads as a real Mac app:
//! SF Symbols rasterised by AppKit, the user's system accent colour, and
//! native NSMenu pop-ups for pickers (HIG: pop-up buttons for a flat list of
//! mutually exclusive options).

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::NSObject;
use std::cell::Cell;
use objc2_app_kit::{
    NSBitmapFormat, NSBitmapImageRep, NSColor, NSColorSpace, NSDeviceRGBColorSpace, NSFontWeight, NSGraphicsContext, NSImage,
    NSImageSymbolConfiguration, NSMenu, NSMenuItem, NSView,
};
use objc2_foundation::{NSArray, NSData, NSPoint, NSRect, NSSize, NSString};

/// An SF Symbol tinted `rgba` (0..1), drawn at `point_size` with `weight`
/// (-1..1, AppKit NSFontWeight scale) into a straight-alpha RGBA8 buffer at
/// `scale`× (Retina). Returns (width_px, height_px, rgba).
pub fn render_symbol(name: &str, point_size: f64, weight: f64, rgba: [f64; 4], scale: f64) -> Option<(u32, u32, Vec<u8>)> {
    MainThreadMarker::new()?;
    unsafe {
        let base = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)?;
        let color = NSColor::colorWithSRGBRed_green_blue_alpha(rgba[0], rgba[1], rgba[2], rgba[3]);
        let size_cfg = NSImageSymbolConfiguration::configurationWithPointSize_weight(point_size, weight as NSFontWeight);
        // Two-layer palette: primary tint + white for the secondary layer, so
        // filled symbols (checkmark.circle.fill) keep their knocked-out glyph.
        // Filled circles need two layers (white glyph knocked out of the tint);
        // everything else is drawn as a monochrome template and tinted below.
        let two_tone = name.ends_with(".fill") && name.contains("circle");
        let colors = if two_tone {
            vec![NSColor::colorWithSRGBRed_green_blue_alpha(1., 1., 1., 1.), color]
        } else {
            vec![NSColor::colorWithSRGBRed_green_blue_alpha(0., 0., 0., 1.)]
        };
        let color_cfg = NSImageSymbolConfiguration::configurationWithPaletteColors(&NSArray::from_retained_slice(&colors));
        let cfg = size_cfg.configurationByApplyingConfiguration(&color_cfg);
        let img = base.imageWithSymbolConfiguration(&cfg)?;
        let sz: NSSize = img.size();
        let (w, h) = ((sz.width * scale).ceil().max(1.0) as isize, (sz.height * scale).ceil().max(1.0) as isize);
        let rep = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            w,
            h,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            NSBitmapFormat::empty(),
            w * 4,
            32,
        )?;
        rep.setSize(sz);
        let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&ctx));
        img.drawInRect(NSRect::new(NSPoint::new(0., 0.), sz));
        ctx.flushGraphics();
        NSGraphicsContext::restoreGraphicsState_class();
        let data = rep.bitmapData();
        if data.is_null() {
            return None;
        }
        let stride = rep.bytesPerRow() as usize;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h as usize {
            let line = std::slice::from_raw_parts(data.add(row * stride), w as usize * 4);
            // AppKit hands back premultiplied RGBA; GPUI wants straight alpha.
            for px in line.chunks_exact(4) {
                let a = px[3] as u32;
                if a == 0 {
                    out.extend_from_slice(&[0, 0, 0, 0]);
                } else if !two_tone {
                    // Template: coverage → alpha of the requested tint.
                    let c = |v: f64| (v.clamp(0., 1.) * 255.).round() as u8;
                    out.extend_from_slice(&[c(rgba[0]), c(rgba[1]), c(rgba[2]), (a as f64 * rgba[3]).round() as u8]);
                } else {
                    let un = |c: u8| ((c as u32 * 255 + a / 2) / a).min(255) as u8;
                    out.extend_from_slice(&[un(px[0]), un(px[1]), un(px[2]), px[3]]);
                }
            }
        }
        Some((w as u32, h as u32, out))
    }
}

/// System Settings → Appearance → Accent colour, as sRGB 0..1. `None` when the
/// user picked "Multicolour" (the app's own accent should be used then).
pub fn accent_color() -> Option<[f32; 3]> {
    // -1 = graphite … 6 = pink; key absent = multicolour (app accent).
    let multicolour = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleAccentColor"])
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true);
    if multicolour {
        return None;
    }
    let c = NSColor::controlAccentColor().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
    Some([c.redComponent() as f32, c.greenComponent() as f32, c.blueComponent() as f32])
}

struct PickIvars {
    picked: Cell<isize>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop impl.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "StenlyMenuPick"]
    #[ivars = PickIvars]
    struct MenuPick;

    impl MenuPick {
        #[unsafe(method(pick:))]
        fn pick(&self, sender: &NSMenuItem) {
            self.ivars().picked.set(sender.tag());
        }
    }
);

impl MenuPick {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PickIvars { picked: Cell::new(-1) });
        unsafe { msg_send![super(this), init] }
    }
}

/// Show a native pop-up menu at `(x, y)` (window points, top-left origin) in
/// the window that owns `ns_view`, with `selected` checked and positioned under
/// the pointer like an NSPopUpButton. Blocks in AppKit's menu tracking loop;
/// returns the chosen index.
pub fn popup_menu(ns_view: *mut std::ffi::c_void, items: &[&str], selected: Option<usize>, x: f64, y: f64, min_width: f64) -> Option<usize> {
    let mtm = MainThreadMarker::new()?;
    if ns_view.is_null() {
        return None;
    }
    unsafe {
        let view: &NSView = &*(ns_view as *const NSView);
        let target = MenuPick::new(mtm);
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        menu.setMinimumWidth(min_width);
        let mut made: Vec<Retained<NSMenuItem>> = Vec::new();
        for (i, label) in items.iter().enumerate() {
            if *label == "-" {
                let sep = NSMenuItem::separatorItem(mtm);
                menu.addItem(&sep);
                made.push(sep);
                continue;
            }
            let item = NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(label), Some(sel!(pick:)), &NSString::from_str(""));
            item.setTarget(Some(&*(&*target as *const MenuPick as *const AnyObject)));
            item.setTag(i as isize);
            item.setEnabled(true);
            if Some(i) == selected {
                item.setState(1);
            }
            menu.addItem(&item);
            made.push(item);
        }
        // NSView coords may be bottom-up; convert from our top-left points.
        let b = view.bounds();
        let py = if view.isFlipped() { y } else { b.size.height - y };
        let positioning = selected.and_then(|i| made.get(i)).map(|r| &**r);
        menu.popUpMenuPositioningItem_atLocation_inView(positioning, NSPoint::new(x, py), Some(view));
        // The item's action fires synchronously before tracking returns.
        let picked = target.ivars().picked.get();
        (picked >= 0 && (picked as usize) < items.len()).then_some(picked as usize)
    }
}


/// One row of a [`popup_menu_rich`] menu.
#[derive(Debug, Clone, Default)]
pub struct MenuEntry {
    /// "-" = separator.
    pub title: String,
    /// Second line in smaller grey text (macOS 14.4+).
    pub subtitle: Option<String>,
    /// SF Symbol shown before the title (template, so it's tinted like text).
    pub symbol: Option<String>,
    /// Bundled image data, taking precedence over the SF Symbol. Never template-tinted.
    pub image: Option<&'static [u8]>,
    pub enabled: bool,
    pub checked: bool,
    /// A non-interactive section header (macOS 14+).
    pub header: bool,
}

/// Like [`popup_menu`], with section headers, subtitles, icons and disabled
/// rows. Returns the index of the chosen entry.
pub fn popup_menu_rich(ns_view: *mut std::ffi::c_void, entries: &[MenuEntry], x: f64, y: f64) -> Option<usize> {
    let mtm = MainThreadMarker::new()?;
    if ns_view.is_null() {
        return None;
    }
    unsafe {
        let view: &NSView = &*(ns_view as *const NSView);
        let target = MenuPick::new(mtm);
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        let mut made: Vec<Retained<NSMenuItem>> = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            if e.title == "-" {
                let sep = NSMenuItem::separatorItem(mtm);
                menu.addItem(&sep);
                made.push(sep);
                continue;
            }
            if e.header {
                let h = NSMenuItem::sectionHeaderWithTitle(&NSString::from_str(&e.title), mtm);
                menu.addItem(&h);
                made.push(h);
                continue;
            }
            let item = NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(&e.title), Some(sel!(pick:)), &NSString::from_str(""));
            item.setTarget(Some(&*(&*target as *const MenuPick as *const AnyObject)));
            item.setTag(i as isize);
            item.setEnabled(e.enabled);
            if e.checked {
                item.setState(1);
            }
            if let Some(sub) = &e.subtitle {
                item.setSubtitle(Some(&NSString::from_str(sub)));
            }
            if let Some(img) = e.image.and_then(|bytes| NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(bytes))) {
                // A fixed point size keeps high-resolution logos aligned with native
                // menu glyphs. Keep their colors, including while highlighted.
                img.setSize(NSSize::new(16., 16.));
                img.setTemplate(false);
                item.setImage(Some(&img));
            } else if let Some(sym) = &e.symbol
                && let Some(img) = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(sym), None)
            {
                img.setTemplate(true);
                item.setImage(Some(&img));
            }
            menu.addItem(&item);
            made.push(item);
        }
        let b = view.bounds();
        let py = if view.isFlipped() { y } else { b.size.height - y };
        menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(x, py), Some(view));
        let picked = target.ivars().picked.get();
        (picked >= 0 && (picked as usize) < entries.len()).then_some(picked as usize)
    }
}

/// Give the window an empty unified NSToolbar (once). On macOS 26 that's what
/// makes AppKit use the larger, concentric "toolbar window" corner radius
/// (26pt instead of 16pt) so the inset glass sidebar's corners nest properly.
/// The toolbar has no items, is transparent (full-size content), and keeps the
/// traffic lights where they were.
pub fn ensure_unified_toolbar(ns_view: *mut std::ffi::c_void) {
    use objc2_app_kit::{NSToolbar, NSWindowToolbarStyle};
    let Some(mtm) = MainThreadMarker::new() else { return };
    if ns_view.is_null() {
        return;
    }
    unsafe {
        let view: &NSView = &*(ns_view as *const NSView);
        let Some(win) = view.window() else { return };
        if win.toolbar().is_some() {
            return;
        }
        let tb = NSToolbar::initWithIdentifier(NSToolbar::alloc(mtm), &NSString::from_str("stenly.main"));
        tb.setAllowsUserCustomization(false);
        win.setToolbar(Some(&tb));
        win.setToolbarStyle(NSWindowToolbarStyle::Unified);
    }
}

/// The window's actual corner radius (macOS 26: 16pt plain titled, 26pt with a
/// toolbar). Private `_cornerRadius`, read defensively; None if unavailable.
pub fn window_corner_radius(ns_view: *mut std::ffi::c_void) -> Option<f64> {
    MainThreadMarker::new()?;
    if ns_view.is_null() {
        return None;
    }
    unsafe {
        let view: &NSView = &*(ns_view as *const NSView);
        let win = view.window()?;
        let sel = objc2::sel!(_cornerRadius);
        let responds: bool = msg_send![&*win, respondsToSelector: sel];
        if !responds {
            return None;
        }
        let r: f64 = msg_send![&*win, _cornerRadius];
        (r > 0.).then_some(r)
    }
}

/// NSGlassEffectView exists (macOS 26+).
pub fn glass_available() -> bool {
    objc2::runtime::AnyClass::get(c"NSGlassEffectView").is_some()
}

/// One rect of real macOS 26 Liquid Glass placed *behind* GPUI's Metal view.
/// Coordinates are window points, top-left origin (GPUI's space).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub radius: f64,
    /// `true` = Clear variant (more see-through), `false` = Regular.
    pub clear: bool,
}

/// Sync the window's glass views to `rects` (create / move / remove). GPUI
/// must leave these regions transparent for the glass to show through.
/// Returns false when NSGlassEffectView isn't available (pre-macOS 26).
pub fn sync_glass(ns_view: *mut std::ffi::c_void, rects: &[GlassRect]) -> bool {
    use objc2_app_kit::{NSAutoresizingMaskOptions, NSGlassEffectView, NSGlassEffectViewStyle, NSUserInterfaceItemIdentification, NSWindowOrderingMode};
    let Some(mtm) = MainThreadMarker::new() else { return false };
    if ns_view.is_null() || objc2::runtime::AnyClass::get(c"NSGlassEffectView").is_none() {
        return false;
    }
    unsafe {
        let gpui_view: &NSView = &*(ns_view as *const NSView);
        let Some(container) = gpui_view.superview() else { return false };
        // GPUI's transparent window clears the NSWindow background; restore the
        // dynamic system window background (follows light/dark by itself) so
        // only the glass regions are see-through.
        if let Some(win) = gpui_view.window() {
            let bg = NSColor::windowBackgroundColor();
            if win.backgroundColor() != bg {
                win.setBackgroundColor(Some(&bg));
            }
        }
        // Our glass views are tagged by identifier so we can find them again.
        let ident = NSString::from_str("stenly.glass");
        let mut existing: Vec<Retained<NSView>> = container
            .subviews()
            .iter()
            .filter(|v| v.identifier().is_some_and(|i| i.to_string() == "stenly.glass"))
            .collect();
        let height = container.bounds().size.height;
        let flipped = container.isFlipped();
        for (i, r) in rects.iter().enumerate() {
            let y = if flipped { r.y } else { height - r.y - r.h };
            let frame = NSRect::new(NSPoint::new(r.x, y), NSSize::new(r.w, r.h));
            let view: Retained<NSView> = if i < existing.len() {
                existing[i].clone()
            } else {
                let g = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), frame);
                g.setIdentifier(Some(&ident));
                let v: Retained<NSView> = Retained::into_super(g);
                container.addSubview_positioned_relativeTo(&v, NSWindowOrderingMode::Below, Some(gpui_view));
                existing.push(v.clone());
                v
            };
            view.setFrame(frame);
            view.setAutoresizingMask(NSAutoresizingMaskOptions::empty());
            if let Some(g) = view.downcast_ref::<NSGlassEffectView>() {
                g.setCornerRadius(r.radius);
                g.setStyle(if r.clear { NSGlassEffectViewStyle::Clear } else { NSGlassEffectViewStyle::Regular });
            }
            view.setHidden(false);
        }
        for v in existing.iter().skip(rects.len()) {
            v.removeFromSuperview();
        }
    }
    true
}

/// Global mouse position in top-left-origin screen points (for placing HUDs
/// on the screen the user is working on).
pub fn mouse_location() -> Option<(f64, f64)> {
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(source: *const std::ffi::c_void) -> *mut std::ffi::c_void;
        fn CGEventGetLocation(e: *mut std::ffi::c_void) -> CGPoint;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const std::ffi::c_void);
    }
    unsafe {
        let e = CGEventCreate(std::ptr::null());
        if e.is_null() {
            return None;
        }
        let p = CGEventGetLocation(e); // already top-left origin, global
        CFRelease(e);
        Some((p.x, p.y))
    }
}

/// AppKit y of the bottom edge of the window owning `ns_view` (bottom-left
/// origin), to put it back after a resize that kept the top edge instead.
pub fn window_bottom(ns_view: *mut std::ffi::c_void) -> Option<f64> {
    if ns_view.is_null() || MainThreadMarker::new().is_none() {
        return None;
    }
    let view = ns_view as *mut NSView;
    unsafe { (*view).window().map(|w| w.frame().origin.y) }
}

pub fn set_window_bottom(ns_view: *mut std::ffi::c_void, y: f64) {
    if ns_view.is_null() || MainThreadMarker::new().is_none() {
        return;
    }
    let view = ns_view as *mut NSView;
    unsafe {
        if let Some(w) = (*view).window() {
            let f = w.frame();
            if (f.origin.y - y).abs() > 0.5 {
                w.setFrameOrigin(NSPoint::new(f.origin.x, y));
            }
        }
    }
}
