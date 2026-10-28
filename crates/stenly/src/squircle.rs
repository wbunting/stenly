//! Continuous ("squircle") corners, the curve AppKit uses for macOS 26 glass
//! panels, windows and controls. GPUI's `rounded(..)` only draws circular
//! arcs, so a surface here is a canvas that paints a superellipse path behind
//! its parent's content.
//!
//! Use it as the first child of a `.relative()` element:
//!
//! ```ignore
//! div().relative().child(squircle::surface(Surface::fill(theme::button()).radius(12.))).child("Label")
//! ```

use gpui::{App, Bounds, HitboxBehavior, MouseDownEvent, MouseUpEvent, PathBuilder, Pixels, Point, Rgba, canvas, div, point, prelude::*, px};
use std::cell::Cell;

/// Superellipse exponent for each corner: 2 is a circular arc, higher is
/// squarer. With EXTENT below, 4 puts the corner's midpoint close to where a
/// circular arc of the nominal radius would, like Apple's continuous corner.
const EXPONENT: f32 = 4.;
/// A continuous corner spans further along each edge than a circular arc of
/// the same nominal radius (Apple's curve: ~1.53x), so it reads as the same
/// size while easing into the straight edge.
const EXTENT: f32 = 1.528;
const STEPS: usize = 24;

thread_local! {
    /// Whether the left mouse button is down, for pressed fills.
    static MOUSE_DOWN: Cell<bool> = const { Cell::new(false) };
}

/// How a squircle surface looks in each state.
#[derive(Clone, Copy)]
pub struct Surface {
    /// Nominal corner radius (as you'd pass to `rounded`). `None` = as round
    /// as the shape allows (a continuous capsule / circle).
    pub radius: Option<f32>,
    pub fill: Option<Rgba>,
    pub hover: Option<Rgba>,
    pub pressed: Option<Rgba>,
    pub border: Option<Rgba>,
}

impl Surface {
    pub fn fill(fill: impl Into<Rgba>) -> Self {
        Self {
            radius: None,
            fill: Some(fill.into()),
            hover: None,
            pressed: None,
            border: None,
        }
    }
    /// No resting fill; only hover/pressed washes.
    pub fn clear() -> Self {
        Self {
            radius: None,
            fill: None,
            hover: None,
            pressed: None,
            border: None,
        }
    }
    pub fn radius(mut self, r: f32) -> Self {
        self.radius = Some(r);
        self
    }
    pub fn hover(mut self, c: impl Into<Rgba>) -> Self {
        self.hover = Some(c.into());
        self
    }
    pub fn pressed(mut self, c: impl Into<Rgba>) -> Self {
        self.pressed = Some(c.into());
        self
    }
    pub fn border(mut self, c: impl Into<Rgba>) -> Self {
        self.border = Some(c.into());
        self
    }
}

/// A canvas filling its (relative) parent with the surface's shape.
pub fn surface(s: Surface) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, _cx: &mut App| {
            let hovered = hitbox.is_hovered(window);
            let pressed = hovered && MOUSE_DOWN.with(Cell::get);
            // Repaint when hover or press changes so the wash follows the mouse.
            if s.hover.is_some() || s.pressed.is_some() {
                let hb = hitbox.clone();
                window.on_mouse_event(move |_: &gpui::MouseMoveEvent, phase, window, _| {
                    if phase == gpui::DispatchPhase::Capture && hb.is_hovered(window) != hovered {
                        window.refresh();
                    }
                });
                window.on_mouse_event(move |e: &MouseDownEvent, phase, window, _| {
                    if phase == gpui::DispatchPhase::Capture && e.button == gpui::MouseButton::Left {
                        MOUSE_DOWN.with(|d| d.set(true));
                        window.refresh();
                    }
                });
                window.on_mouse_event(move |e: &MouseUpEvent, phase, window, _| {
                    if phase == gpui::DispatchPhase::Capture && e.button == gpui::MouseButton::Left {
                        MOUSE_DOWN.with(|d| d.set(false));
                        window.refresh();
                    }
                });
            }
            let fill = if pressed {
                s.pressed.or(s.hover).or(s.fill)
            } else if hovered {
                s.hover.or(s.fill)
            } else {
                s.fill
            };
            let radius = s.radius.unwrap_or(f32::INFINITY);
            if let Some(c) = fill {
                if let Ok(p) = path(PathBuilder::fill(), bounds, radius, 0.).build() {
                    window.paint_path(p, c);
                }
            }
            if let Some(c) = s.border {
                // Hairline centred half a point inside the edge, like a 1pt CSS border.
                if let Ok(p) = path(PathBuilder::stroke(px(1.)), bounds, radius, 0.5).build() {
                    window.paint_path(p, c);
                }
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Wrap `el` in a box drawn with `s` (for surfaces behind an existing control).
pub fn behind(el: impl IntoElement, s: Surface) -> gpui::Div {
    div().relative().child(surface(s)).child(el)
}

/// Superellipse-cornered rectangle, `inset` points inside `bounds`.
fn path(mut b: PathBuilder, bounds: Bounds<Pixels>, radius: f32, inset: f32) -> PathBuilder {
    let x0 = f32::from(bounds.origin.x) + inset;
    let y0 = f32::from(bounds.origin.y) + inset;
    let w = f32::from(bounds.size.width) - inset * 2.;
    let h = f32::from(bounds.size.height) - inset * 2.;
    let want = radius * EXTENT;
    let limit = (w / 2.).min(h / 2.).max(0.);
    let e = want.min(limit);
    // Keep the continuous curve even when the corner is clamped (capsules,
    // circles): easing back to a circular arc there made "round" controls
    // indistinguishable from GPUI's rounded_full().
    let n = EXPONENT;
    let (x1, y1) = (x0 + w, y0 + h);
    // Corner centres (where the corner's two straight edges would meet the
    // curve's axes) and the quadrant each corner sweeps, clockwise from top-right.
    let corners = [
        ((x1 - e, y0 + e), (1., -1.), 0.),
        ((x1 - e, y1 - e), (1., 1.), 1.),
        ((x0 + e, y1 - e), (-1., 1.), 2.),
        ((x0 + e, y0 + e), (-1., -1.), 3.),
    ];
    let mut first = true;
    let pt = |x: f32, y: f32| -> Point<Pixels> { point(px(x), px(y)) };
    for ((cx, cy), (sx, sy), q) in corners {
        for i in 0..=STEPS {
            // Parameter over the quarter, oriented so the outline runs clockwise.
            let t = i as f32 / STEPS as f32 * std::f32::consts::FRAC_PI_2;
            let t = if q as i32 % 2 == 0 { std::f32::consts::FRAC_PI_2 - t } else { t };
            let (c, s) = (t.cos(), t.sin());
            let x = cx + sx * e * c.max(0.).powf(2. / n);
            let y = cy + sy * e * s.max(0.).powf(2. / n);
            if first {
                b.move_to(pt(x, y));
                first = false;
            } else {
                b.line_to(pt(x, y));
            }
        }
    }
    b.close();
    b
}
