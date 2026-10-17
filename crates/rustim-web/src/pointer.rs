//! DOM events → the shared gesture vocabulary.
//!
//! The arithmetic a gesture performs lives in [`rustim_math::gesture`], shared
//! with the desktop host. What is browser-specific is only the *translation*:
//! a DOM `WheelEvent` reports `deltaY` with the opposite sign (positive is
//! scrolling down) and in three units, and a pointer's client coordinates are
//! CSS pixels against the canvas's bounding rectangle, while the engine lives
//! in the canvas's own pixel grid.
//!
//! Both translations are pure functions, deliberately: they are where a browser
//! port goes subtly wrong — a wheel that zooms the wrong way, a drag that pans
//! twice as far on a HiDPI screen — and neither needs a DOM to be tested.

use rustim_math::Rect;
use rustim_math::gesture::ScrollDelta;

/// `WheelEvent::deltaMode` values, named for the reader.
///
/// From the DOM specification: pixels, lines, pages. Any other value is treated
/// as pixels, the mode every browser actually uses for a trackpad and Chrome
/// uses for a wheel too.
pub const DOM_DELTA_PIXEL: u32 = 0;
/// See [`DOM_DELTA_PIXEL`].
pub const DOM_DELTA_LINE: u32 = 1;
/// See [`DOM_DELTA_PIXEL`].
pub const DOM_DELTA_PAGE: u32 = 2;

/// How many wheel notches one page-mode scroll is worth.
///
/// A page delta is "one screenful"; there is no standard line count for it, and
/// the two hosts must at least behave the same way on every platform. Ten lines
/// is a conventional page, and the value is named so it is a decision rather
/// than a magic number.
pub const PAGE_LINES: f64 = 10.0;

/// A DOM wheel event's vertical delta, in the gesture's sign and unit.
///
/// DOM's `deltaY` is positive when scrolling **down**; the gesture's positive
/// value means scrolling away from the user, which zooms in. Hence the flip —
/// without it, wheel-up would zoom out and the canvas would behave like no
/// other map or image viewer.
pub fn scroll_delta(delta_y: f64, delta_mode: u32) -> ScrollDelta {
    match delta_mode {
        DOM_DELTA_LINE => ScrollDelta::Lines(-delta_y),
        DOM_DELTA_PAGE => ScrollDelta::Lines(-delta_y * PAGE_LINES),
        // Pixels, and any mode a future browser invents: pixels is the least
        // surprising unit, and `zoom_factor` treats an unknown amount as the
        // identity only when it is non-finite, not merely unexpected.
        _ => ScrollDelta::Pixels(-delta_y),
    }
}

/// A client-space point in the canvas's own pixel grid.
///
/// `rect` is `canvas.getBoundingClientRect()` and `width`/`height` are the
/// canvas's `width`/`height` attributes. They differ whenever CSS scales the
/// canvas — the demo page caps it at `max-width: 100%` — and on a HiDPI display
/// the attributes are device pixels while the rectangle is CSS pixels. Dividing
/// by the rectangle and multiplying by the attribute is the only conversion
/// that is right in both cases.
///
/// `None` for a rectangle with no area (the canvas is `display: none`) or a
/// non-finite position: a degenerate layout is not an error a host can act on,
/// and inventing a point would pan or zoom from nowhere.
pub fn canvas_point(client: (f64, f64), rect: Rect, width: u16, height: u16) -> Option<(f64, f64)> {
    let rect_width = rect.width();
    let rect_height = rect.height();
    if !rect_width.is_finite()
        || !rect_height.is_finite()
        || rect_width <= 0.0
        || rect_height <= 0.0
    {
        return None;
    }
    let x = (client.0 - rect.x0) * f64::from(width) / rect_width;
    let y = (client.1 - rect.y0) * f64::from(height) / rect_height;
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    Some((x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wheel-up (`deltaY` negative in the DOM) zooms **in**, and the unit
    /// survives the adapter.
    #[test]
    fn dom_scrolls_are_flipped_into_the_gestures_sign() {
        assert_eq!(
            scroll_delta(-100.0, DOM_DELTA_PIXEL),
            ScrollDelta::Pixels(100.0)
        );
        assert_eq!(
            scroll_delta(100.0, DOM_DELTA_PIXEL),
            ScrollDelta::Pixels(-100.0)
        );
        assert_eq!(scroll_delta(-3.0, DOM_DELTA_LINE), ScrollDelta::Lines(3.0));
        assert_eq!(
            scroll_delta(1.0, DOM_DELTA_PAGE),
            ScrollDelta::Lines(-PAGE_LINES)
        );
        // An unknown mode is read as pixels rather than dropped.
        assert_eq!(scroll_delta(-2.0, 99), ScrollDelta::Pixels(2.0));
    }

    /// The client→canvas conversion accepts a scaled canvas and a HiDPI one.
    #[test]
    fn client_points_scale_into_canvas_pixels() {
        // Unscaled: 1280 CSS px wide, 1280 attribute px.
        let rect = Rect::new(0.0, 100.0, 1280.0, 820.0);
        assert_eq!(
            canvas_point((100.0, 300.0), rect, 1280, 720),
            Some((100.0, 200.0))
        );
        // CSS shrinks the canvas to half its pixels: one CSS px is two canvas
        // pixels.
        let rect = Rect::new(10.0, 20.0, 650.0, 380.0);
        assert_eq!(
            canvas_point((110.0, 120.0), rect, 1280, 720),
            Some((200.0, 200.0))
        );
        // HiDPI: the attributes are device pixels, the rectangle CSS pixels.
        let rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
        assert_eq!(
            canvas_point((640.0, 360.0), rect, 2560, 1440),
            Some((1280.0, 720.0))
        );
    }

    #[test]
    fn a_collapsed_canvas_has_no_point() {
        let flat = Rect::new(0.0, 0.0, 0.0, 720.0);
        assert_eq!(canvas_point((10.0, 10.0), flat, 1280, 720), None);
        let nan = Rect::new(f64::NAN, 0.0, 1280.0, 720.0);
        assert_eq!(canvas_point((10.0, 10.0), nan, 1280, 720), None);
    }
}
