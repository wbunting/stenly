//! Small floating windows (dictation HUD, recording pill, prompts).
//!
//! On macOS these are borderless panels positioned by origin. On Wayland a
//! client cannot place its own windows, and compositors are free to resize
//! and re-place ordinary ones (Hyprland's floating workspaces stretched them
//! to the whole screen), so they open as `wlr-layer-shell` overlay surfaces
//! anchored to a screen edge instead. Compositors without layer-shell (GNOME)
//! and X11 fall back to the ordinary pop-up window.

use gpui::{App, Entity, Pixels, Render, Window, WindowHandle, WindowKind, WindowOptions};

/// App id (Wayland) / WM_CLASS (X11) of the pop-up-window fallback, separate
/// from the main window's "stenly" so window managers can float them without
/// touching the main window (for example Hyprland: `class = "^stenly-overlay$"`).
pub const APP_ID: &str = "stenly-overlay";

/// Where an overlay sits on its screen (used for layer-shell surfaces only;
/// pop-up windows keep the origin in their `WindowOptions`).
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(all(target_os = "linux", feature = "wayland")), allow(dead_code))]
pub enum Placement {
    /// Centred horizontally, `bottom` above the bottom edge.
    BottomCentre { bottom: Pixels },
    /// Centred vertically, `right` in from the right edge (may be negative).
    RightCentre { right: Pixels },
    /// Top-right corner.
    TopRight { top: Pixels, right: Pixels },
}

#[cfg(all(target_os = "linux", feature = "wayland"))]
fn layer_kind(name: &str, placement: Placement) -> WindowKind {
    use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
    let zero = gpui::px(0.);
    // CSS order: top, right, bottom, left.
    let (anchor, margin) = match placement {
        Placement::BottomCentre { bottom } => (Anchor::BOTTOM, (zero, zero, bottom, zero)),
        Placement::RightCentre { right } => (Anchor::RIGHT, (zero, right, zero, zero)),
        Placement::TopRight { top, right } => {
            (Anchor::TOP | Anchor::RIGHT, (top, right, zero, zero))
        }
    };
    WindowKind::LayerShell(LayerShellOptions {
        namespace: format!("stenly-{name}"),
        layer: Layer::Overlay,
        anchor,
        exclusive_zone: None,
        exclusive_edge: None,
        margin: Some(margin),
        // Never take keyboard focus: dictated text must land in the app the
        // user is typing in. Buttons still get pointer clicks.
        keyboard_interactivity: KeyboardInteractivity::None,
    })
}

/// Open an overlay. `opts(kind)` builds the window options for a window kind:
/// `WindowKind::PopUp` everywhere except Wayland compositors with layer-shell.
pub fn open<V: 'static + Render>(
    cx: &mut App,
    name: &str,
    placement: Placement,
    opts: impl Fn(WindowKind) -> WindowOptions,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> anyhow::Result<WindowHandle<V>> {
    let mut build = Some(build);
    #[cfg(all(target_os = "linux", feature = "wayland"))]
    if cx.compositor_name() == "Wayland" {
        match cx.open_window(opts(layer_kind(name, placement)), |w, cx| {
            (build.take().expect("built once"))(w, cx)
        }) {
            Ok(h) => return Ok(h),
            // The surface failed before the view was built (no layer-shell): fall back.
            Err(e) if build.is_some() => {
                tracing::info!(error = %e, overlay = name, "no layer-shell; using a pop-up window")
            }
            Err(e) => return Err(e),
        }
    }
    let _ = (name, placement);
    cx.open_window(opts(WindowKind::PopUp), |w, cx| {
        (build.take().expect("built once"))(w, cx)
    })
}
