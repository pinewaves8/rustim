//! Pointer gestures, as pure state and arithmetic over a camera.
//!
//! A pointer is a *view control*: dragging pans, scrolling zooms about the
//! cursor. Both hosts — the desktop window and the browser canvas — receive
//! these events from different APIs with different units and signs, and both
//! have to produce the same two numbers. That shared arithmetic lives here, in
//! the kernel, because it is pure: no winit, no DOM, no I/O, and therefore
//! testable without a window and reusable by a third host.
//!
//! The split is:
//!
//! - [`Camera::panned`] and [`Camera::zoomed_about`] are the geometry, on the
//!   camera type itself.
//! - [`ScrollDelta`], [`zoom_factor`] and [`clamped_zoom_factor`] turn a scroll
//!   into a multiplicative zoom factor, keeping the platform's unit and
//!   enforcing the shared gesture range.
//! - [`Drag`] is the button state and the previous cursor position.
//! - [`Pointer`] composes the three: it owns the cursor, the drag and the camera
//!   the current gesture has built up, and answers the camera a host should
//!   write after each event.
//!
//! # Why the zoom is multiplicative
//!
//! `factor = exp(k · delta)`. Equal scroll steps multiply the zoom by equal
//! ratios, so ten notches in and ten out return to the start and a notch feels
//! the same at every scale; adding a constant would be a huge jump zoomed out
//! and invisible zoomed in. Notches and pixels are not convertible — the OS
//! decides how many pixels a notch is — so each keeps its own rate.
//!
//! # Why the range is applied to the factor
//!
//! A host clamps to [`MIN_ZOOM`]..=[`MAX_ZOOM`]. Clamping the *resulting zoom*
//! would move the centre and break the cursor-anchor invariant
//! ([`Camera::zoomed_about`]'s whole point), so [`clamped_zoom_factor`]
//! re-expresses the factor so the product lands on the limit exactly. The
//! command layer still accepts any finite positive zoom: this range is a
//! gesture's, not a script's.

use crate::{Camera, Viewport};

/// A wheel or trackpad scroll, reduced to the one axis zoom reads.
///
/// A wheel emits *notches* while a trackpad emits *pixels*, and the number of
/// pixels in a notch is an OS setting, not a constant. So the delta keeps its
/// unit and the conversion to a zoom factor happens in [`zoom_factor`], where
/// each unit gets its own rate.
///
/// The sign convention is "positive is away from the user", i.e. zoom in. That
/// is winit's `LineDelta` convention; a DOM `WheelEvent`'s `deltaY` is the
/// opposite (positive is scrolling down), and a browser host flips it before
/// constructing this type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrollDelta {
    /// Wheel notches.
    Lines(f64),
    /// Platform pixels.
    Pixels(f64),
}

impl ScrollDelta {
    /// The vertical amount, in this variant's own unit.
    pub fn y(self) -> f64 {
        match self {
            Self::Lines(y) | Self::Pixels(y) => y,
        }
    }
}

/// Zoom change per wheel notch: one notch is `e^0.25 ≈ 1.284`, about 28%.
pub const WHEEL_ZOOM_PER_LINE: f64 = 0.25;

/// Zoom change per platform pixel of trackpad scroll.
///
/// A trackpad sends many small events where a wheel sends one, so the rate is
/// correspondingly smaller; 100 px of two-finger travel is `e^0.2 ≈ 1.22`.
pub const WHEEL_ZOOM_PER_PIXEL: f64 = 0.002;

/// The gesture range. `0.05` shows 160 world units, `64` shows an eighth.
///
/// A *gesture* stays reversible inside this range; without it a stuck wheel
/// reaches the floating point floor and the scene is gone. A script may still
/// ask the command layer for any positive zoom.
pub const MIN_ZOOM: f64 = 0.05;
/// See [`MIN_ZOOM`].
pub const MAX_ZOOM: f64 = 64.0;

/// A scroll's zoom **factor** — multiplicative, not additive.
///
/// A non-finite amount returns `1.0`, the identity, rather than poisoning the
/// camera — the same posture [`Drag`] has toward malformed positions.
pub fn zoom_factor(delta: ScrollDelta) -> f64 {
    let rate = match delta {
        ScrollDelta::Lines(_) => WHEEL_ZOOM_PER_LINE,
        ScrollDelta::Pixels(_) => WHEEL_ZOOM_PER_PIXEL,
    };
    let exponent = delta.y() * rate;
    if exponent.is_finite() {
        exponent.exp()
    } else {
        1.0
    }
}

/// Re-express `factor` so that `current * factor` lands inside the gesture range.
///
/// Returns `1.0` — no zoom — for a camera or factor the multiplication cannot
/// survive, and when the camera is already at the limit in the direction the
/// scroll asks for. The result is a factor, not a zoom, because
/// [`Camera::zoomed_about`] is what applies it and it must keep the point under
/// the cursor invariant.
pub fn clamped_zoom_factor(current: f64, factor: f64) -> f64 {
    if !current.is_finite() || current <= 0.0 || !factor.is_finite() || factor <= 0.0 {
        return 1.0;
    }
    (current * factor).clamp(MIN_ZOOM, MAX_ZOOM) / current
}

/// The left button's drag state, as much as panning needs.
///
/// `last` is **not** cleared on [`Self::press`]: the last cursor position a
/// platform reported is normally the position of the press itself, so keeping
/// it means the first movement after the button goes down produces a correct
/// delta instead of being swallowed as the anchor. It is cleared on release,
/// because a drag that ends off-window must not leave a stale anchor for the
/// next one.
#[derive(Debug, Default, Clone, Copy)]
pub struct Drag {
    active: bool,
    last: Option<(f64, f64)>,
}

impl Drag {
    pub fn new() -> Self {
        Self::default()
    }

    /// The button went down.
    pub fn press(&mut self) {
        self.active = true;
    }

    /// The button came up, or focus was lost.
    pub fn release(&mut self) {
        self.active = false;
        self.last = None;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Feed one cursor position; answer the screen-space delta to pan by.
    ///
    /// `None` when the button is not down, or when this is the first position
    /// the drag has seen — there is nothing to subtract from yet, and guessing
    /// would jump the scene by an arbitrary amount.
    pub fn moved(&mut self, at: (f64, f64)) -> Option<(f64, f64)> {
        let previous = self.last.replace(at);
        if !self.active {
            return None;
        }
        let (x0, y0) = previous?;
        Some((at.0 - x0, at.1 - y0))
    }
}

/// A host's pointer, from the gesture's point of view.
///
/// It owns the cursor (wheel events carry no position of their own), the drag
/// state, and the camera this gesture has built up since the last frame. A
/// host feeds it events and writes whatever camera it answers with.
///
/// # Why the pending camera exists
///
/// A cursor can move several times between two frames — a 1000 Hz mouse, a slow
/// frame — and each delta has to apply to the camera the *earlier delta of that
/// frame* produced. Reading the camera still on screen every time keeps only
/// the last move and makes a fast drag trail the pointer. [`Self::end_frame`]
/// clears the pending camera once the host has applied the commands the
/// gesture produced; after that a gesture continues from the live camera again.
#[derive(Debug, Default, Clone, Copy)]
pub struct Pointer {
    cursor: Option<(f64, f64)>,
    drag: Drag,
    pending: Option<Camera>,
}

impl Pointer {
    pub fn new() -> Self {
        Self::default()
    }

    /// A left-button drag, in screen pixels; the content follows the pointer.
    pub fn pan(
        &mut self,
        delta_px: (f64, f64),
        viewport: &Viewport,
        live: Camera,
    ) -> Option<Camera> {
        let base = self.pending.unwrap_or(live);
        self.record(base, base.panned(viewport, delta_px))
    }

    /// Feed one cursor position. Answers a camera when the pointer is dragging.
    pub fn cursor_moved(
        &mut self,
        at: (f64, f64),
        viewport: &Viewport,
        live: Camera,
    ) -> Option<Camera> {
        self.cursor = Some(at);
        let delta = self.drag.moved(at)?;
        self.pan(delta, viewport, live)
    }

    /// A scroll about the last cursor position.
    ///
    /// `None` before the pointer has ever entered, or when the scroll changes
    /// nothing (a malformed delta, or a zoom already at the range's limit).
    pub fn scroll(
        &mut self,
        delta: ScrollDelta,
        viewport: &Viewport,
        live: Camera,
    ) -> Option<Camera> {
        let at = self.cursor?;
        let base = self.pending.unwrap_or(live);
        let factor = clamped_zoom_factor(base.zoom, zoom_factor(delta));
        if factor == 1.0 {
            return None;
        }
        self.record(base, base.zoomed_about(viewport, factor, at))
    }

    fn record(&mut self, base: Camera, next: Camera) -> Option<Camera> {
        if next == base {
            return None;
        }
        self.pending = Some(next);
        Some(next)
    }

    /// The button went down.
    pub fn press(&mut self) {
        self.drag.press();
    }

    /// The button came up, focus was lost, or the pointer left the window.
    pub fn release(&mut self) {
        self.drag.release();
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_active()
    }

    /// The last position the pointer reported, if any.
    pub fn cursor(&self) -> Option<(f64, f64)> {
        self.cursor
    }

    /// Forget the pending camera once the frame has applied it.
    pub fn end_frame(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp() -> Viewport {
        Viewport::default()
    }

    /// A drag reports a delta only while the button is held, and the delta is
    /// the pointer's own screen-space travel.
    #[test]
    fn a_drag_pans_only_while_the_button_is_down() {
        let mut drag = Drag::new();
        assert_eq!(drag.moved((100.0, 100.0)), None, "not dragging yet");
        assert_eq!(drag.moved((110.0, 105.0)), None);

        drag.press();
        // The press did not clear the anchor, so the first move after it is
        // measured from where the pointer already was.
        assert_eq!(drag.moved((120.0, 100.0)), Some((10.0, -5.0)));
        assert!(drag.is_active());

        drag.release();
        assert_eq!(drag.moved((200.0, 200.0)), None);
        assert!(!drag.is_active());
    }

    /// A drag that goes down before the platform has reported a position has no
    /// anchor; the first movement establishes one instead of panning by the
    /// distance from the origin.
    #[test]
    fn a_drag_with_no_previous_position_does_not_jump() {
        let mut drag = Drag::new();
        drag.press();
        assert_eq!(drag.moved((640.0, 360.0)), None);
        assert_eq!(drag.moved((641.0, 361.0)), Some((1.0, 1.0)));
    }

    /// Two moves inside one frame compose; the second starts from the camera
    /// the first produced, not from the one still on screen.
    #[test]
    fn a_gesture_continues_from_the_previous_one_within_a_frame() {
        let live = Camera::default();
        let mut pointer = Pointer::new();
        pointer.press();
        assert_eq!(pointer.cursor_moved((100.0, 100.0), &vp(), live), None);
        let first = pointer
            .cursor_moved((190.0, 100.0), &vp(), live)
            .expect("the first drag pans");
        // 90 px at zoom 1 is one world unit, and dragging right moves the
        // content right, so the centre moves left.
        assert_eq!(first.center, (-1.0, 0.0));
        // The second 90 px continues from `first`, not from `live`.
        let second = pointer
            .cursor_moved((280.0, 100.0), &vp(), live)
            .expect("the second drag pans");
        assert_eq!(second.center, (-2.0, 0.0));
        // A frame boundary forgets the pending camera; the next gesture starts
        // from what the host says is on screen.
        pointer.end_frame();
        let third = pointer
            .cursor_moved((370.0, 100.0), &vp(), live)
            .expect("the third drag pans");
        assert_eq!(third.center, (-1.0, 0.0));
    }

    /// A wheel before the pointer has ever been seen has no anchor to zoom
    /// about, and is ignored rather than zooming about an invented position.
    #[test]
    fn a_scroll_without_a_cursor_is_ignored() {
        let mut pointer = Pointer::new();
        assert_eq!(pointer.cursor(), None);
        assert_eq!(
            pointer.scroll(ScrollDelta::Lines(1.0), &vp(), Camera::default()),
            None
        );
    }

    /// Zooming about the cursor keeps the world point under it, and the drag
    /// after a zoom uses the zoomed scale.
    #[test]
    fn a_scroll_keeps_the_point_under_the_cursor() {
        let mut pointer = Pointer::new();
        let camera = Camera::default();
        pointer.cursor_moved((1000.0, 120.0), &vp(), camera);
        let anchor = vp().to_world(&camera, (1000.0, 120.0));

        let zoomed = pointer
            .scroll(ScrollDelta::Lines(1.0), &vp(), camera)
            .expect("one notch zooms in");
        assert!((zoomed.zoom - 0.25f64.exp()).abs() < 1e-12);
        let after = vp().to_world(&zoomed, (1000.0, 120.0));
        assert!((after.0 - anchor.0).abs() < 1e-12);
        assert!((after.1 - anchor.1).abs() < 1e-12);

        // The pan that follows uses the camera the scroll built.
        pointer.end_frame();
        pointer.press();
        let panned = pointer
            .cursor_moved((1090.0, 120.0), &vp(), zoomed)
            .expect("a drag after the zoom pans");
        assert!(panned.zoom == zoomed.zoom);
        // Dragging right moves the centre left, and at the zoomed scale 90 px
        // is less than a world unit.
        assert!(panned.center.0 - zoomed.center.0 < 0.0);
        assert!(panned.center.0 - zoomed.center.0 > -1.0);
    }

    #[test]
    fn malformed_scrolls_are_the_identity() {
        let mut pointer = Pointer::new();
        pointer.cursor_moved((640.0, 360.0), &vp(), Camera::default());
        assert_eq!(
            pointer.scroll(ScrollDelta::Lines(f64::NAN), &vp(), Camera::default()),
            None
        );
        assert_eq!(
            pointer.scroll(ScrollDelta::Pixels(f64::INFINITY), &vp(), Camera::default()),
            None
        );
    }

    /// Wheel and pixel scrolls keep their unit, and zoom is multiplicative.
    #[test]
    fn wheel_zoom_is_multiplicative() {
        let one = zoom_factor(ScrollDelta::Lines(1.0));
        let two = zoom_factor(ScrollDelta::Lines(2.0));
        assert!((two - one * one).abs() < 1e-12);
        assert!(one > 1.0, "a notch away from the user zooms in");

        let pixel = zoom_factor(ScrollDelta::Pixels(125.0));
        assert!(pixel > 1.0);
        assert!(zoom_factor(ScrollDelta::Pixels(-125.0)) < 1.0);

        // In and out by the same amount returns to 1.0 to within rounding.
        let round_trip =
            zoom_factor(ScrollDelta::Lines(3.0)) * zoom_factor(ScrollDelta::Lines(-3.0));
        assert!((round_trip - 1.0).abs() < 1e-15, "{round_trip}");
    }

    /// The gesture range is reached from both directions and never overshoots.
    #[test]
    fn the_pointer_zoom_is_clamped_to_its_range() {
        assert_eq!(clamped_zoom_factor(1.0, 2.0), 2.0);
        // At the top, zooming in is a no-op...
        let at_top = clamped_zoom_factor(MAX_ZOOM, 2.0);
        assert_eq!(at_top, 1.0);
        assert_eq!(MAX_ZOOM * at_top, MAX_ZOOM);
        // ...and from just below it, the factor is cut to land exactly on the
        // limit rather than beyond it.
        let near = clamped_zoom_factor(MAX_ZOOM / 2.0, 4.0);
        assert_eq!(near, 2.0);
        assert_eq!(clamped_zoom_factor(MIN_ZOOM, 0.5), 1.0);
        let near_bottom = clamped_zoom_factor(MIN_ZOOM * 2.0, 0.25);
        assert_eq!(near_bottom, 0.5);
        // Nonsense in either argument is "do not zoom".
        assert_eq!(clamped_zoom_factor(0.0, 2.0), 1.0);
        assert_eq!(clamped_zoom_factor(-1.0, 2.0), 1.0);
        assert_eq!(clamped_zoom_factor(1.0, f64::NAN), 1.0);
        assert_eq!(clamped_zoom_factor(1.0, 0.0), 1.0);
    }

    /// Releasing the button (or losing the pointer) ends the drag but keeps
    /// the cursor, so a wheel after the release still has an anchor.
    #[test]
    fn release_keeps_the_cursor_for_the_wheel() {
        let mut pointer = Pointer::new();
        pointer.cursor_moved((500.0, 300.0), &vp(), Camera::default());
        pointer.press();
        assert!(pointer.is_dragging());
        pointer.release();
        assert!(!pointer.is_dragging());
        assert_eq!(pointer.cursor(), Some((500.0, 300.0)));
        assert!(
            pointer
                .scroll(ScrollDelta::Lines(1.0), &vp(), Camera::default())
                .is_some()
        );
    }
}
