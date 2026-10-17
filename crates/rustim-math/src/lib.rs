//! Points, affine transforms, Bezier maths and rate functions.
//!
//! This crate is the bottom of the stack and the *semantic kernel*: it must stay
//! free of rendering backends, platform code and I/O. That constraint is a
//! day-one guard, not a style preference — it is what lets the same kernel be
//! reused by the exporter, the accessibility layer, the AI command layer and,
//! eventually, a `wasm32-unknown-unknown` build (see `方案建议.md` §10.6).
//!
//! Geometry vocabulary is `kurbo`'s, re-exported here so the rest of the
//! workspace depends on one version of it. `kurbo` is pure maths with no
//! platform code.
//!
//! [`bezier`] and [`space_ops`] are literal ports of `manim.utils.bezier` and
//! `manim.utils.space_ops`, checked case by case against Manim 0.22 in
//! `tests/manim_parity.rs`. They exist so that one scene description produces the
//! same figure here as in Manim — which only holds if the arithmetic rounds the
//! same way, so those two modules reproduce Manim's rounding paths and not just
//! its formulas.

pub mod bezier;
pub mod rate;
pub mod space_ops;
pub mod text;

pub use text::TextGlyph;

pub use kurbo::{Affine, BezPath, PathEl, Point, Rect, Vec2};

/// The 2D affine transform carried by every [`rustim_mobject::Mobject`].
///
/// Deliberately an alias rather than a newtype: Manim bakes transforms into the
/// point arrays, and our central kernel change is to *defer* them instead
/// (`方案建议.md` §4.2). Using the geometry library's own type keeps that
/// deferred transform interchangeable with everything `kurbo` already does.
pub type Affine2 = Affine;

/// Which part of the world fills the window.
///
/// [`Viewport`] is the *window* — how many physical pixels there are. This is the
/// *view* — where those pixels are looking. The two were one struct until the
/// camera became a scene object; they are separate now because they have
/// different lifetimes: the window changes when the user resizes it, the view
/// changes when a scene pans or an animation moves it.
///
/// The camera is part of `rustim_mobject::SceneTree` rather than of the context
/// for one reason: `&mut SceneTree` is the only mutable world an
/// [`rustim_anim::Animation`] receives, and a camera move has to be an ordinary
/// animation. Manim's camera frame is a mobject for the same reason — it is
/// something `self.play` can animate.
///
/// The fields are public data with one invariant, checked at the command layer
/// where values arrive from outside the program: **`zoom` is finite and positive**.
/// A non-positive zoom makes [`Self::visible_height`] degenerate and the
/// projection collapse; a scene that constructs such a camera directly gets the
/// same answer it would get from `Scale(0.0)` — a degenerate picture — because
/// the engine does not second-guess code.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// The world-space point that maps to the centre of the frame.
    pub center: (f64, f64),
    /// How much larger than the default frame the world is drawn. `2.0` shows a
    /// window half as tall, so everything appears twice as big.
    pub zoom: f64,
}

impl Camera {
    /// How many world units tall the frame is at `zoom = 1.0`.
    ///
    /// Manim's default `frame_height`, which is why scene code in this workspace
    /// can use numbers like `3.0` and `4.0` for the legs of a Pythagorean
    /// triangle. Named for its unit on purpose: [`Viewport::frame_height`] is
    /// pixels, and two fields with the same word and different units is a trap.
    pub const WORLD_HEIGHT: f64 = 8.0;

    pub fn new(center: (f64, f64), zoom: f64) -> Self {
        Self { center, zoom }
    }

    /// How many world units tall the visible window is.
    pub fn visible_height(&self) -> f64 {
        Self::WORLD_HEIGHT / self.zoom
    }
}

/// `zoom = 1.0` by hand, not by derive.
///
/// A derived `Default` would give `0.0` — a camera that divides by zero — and
/// it would do so silently, because `Default` is what `SceneTree` starts with.
impl Default for Camera {
    fn default() -> Self {
        Self {
            center: (0.0, 0.0),
            zoom: 1.0,
        }
    }
}

/// Manim's coordinate convention, made explicit.
///
/// Manim is Y-up with the origin at the centre of the frame; vello and every
/// windowing API we target are Y-down with the origin at the top-left. The
/// tutorial's sketches have no transform layer at all and position objects in
/// raw pixels, which silently flips every figure (see `CLAUDE.md`, "Known issues
/// in the tutorial sketches"). This struct is the fix.
///
/// It knows only the frame size in pixels. Which part of the world fills it is
/// [`Camera`]'s business, and every projection method below therefore takes one:
/// there is deliberately no camera-free `affine()`, because a caller that forgot
/// to fold the camera in would draw a correct-looking picture of the wrong view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// Frame size in physical pixels.
    pub frame_width: u16,
    pub frame_height: u16,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            frame_width: 1280,
            frame_height: 720,
        }
    }
}

impl Viewport {
    /// Pixels per world unit. Uniform on both axes, so circles stay round.
    pub fn pixels_per_unit(&self, camera: &Camera) -> f64 {
        f64::from(self.frame_height) / camera.visible_height()
    }

    /// The world→screen transform: Y-up centred world space to Y-down
    /// top-left-origin screen space.
    ///
    /// The default camera reproduces the pre-camera projection *exactly*: with
    /// `zoom = 1.0`, `visible_height()` is `8.0 / 1.0`, which is bit-for-bit
    /// `8.0`, so the scale is the same `720 / 8` as before. Every golden image
    /// depends on that, and on the operation order staying as it is.
    pub fn affine(&self, camera: &Camera) -> Affine2 {
        let s = self.pixels_per_unit(camera);
        Affine::translate((
            f64::from(self.frame_width) / 2.0,
            f64::from(self.frame_height) / 2.0,
        )) * Affine::scale_non_uniform(s, -s)
            * Affine::translate((-camera.center.0, -camera.center.1))
    }

    /// The inverse, for turning a pointer position back into world coordinates.
    pub fn to_world(&self, camera: &Camera, screen: (f64, f64)) -> (f64, f64) {
        let p = self.affine(camera).inverse() * Point::new(screen.0, screen.1);
        (p.x, p.y)
    }

    /// Map a world point to screen pixels.
    pub fn to_screen(&self, camera: &Camera, world: (f64, f64)) -> (f64, f64) {
        let p = self.affine(camera) * Point::new(world.0, world.1);
        (p.x, p.y)
    }
}

/// A rectangle as a closed [`BezPath`].
///
/// `kurbo` 0.13 has no `From<Rect> for BezPath`, and a straight-edged shape still
/// has to be expressed as a path before it can be filled. Keeping the conversion
/// here rather than in each caller means the corner order is decided once.
pub fn rect_path(rect: Rect) -> BezPath {
    BezPath::from_iter([
        PathEl::MoveTo(Point::new(rect.x0, rect.y0)),
        PathEl::LineTo(Point::new(rect.x1, rect.y0)),
        PathEl::LineTo(Point::new(rect.x1, rect.y1)),
        PathEl::LineTo(Point::new(rect.x0, rect.y1)),
        PathEl::ClosePath,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_maps_to_frame_centre() {
        let vp = Viewport::default();
        assert_eq!(vp.to_screen(&Camera::default(), (0.0, 0.0)), (640.0, 360.0));
    }

    /// The default camera must reproduce the projection every golden image was
    /// made with. This is the test that says so, in the terms the picture uses.
    #[test]
    fn the_default_camera_reproduces_the_legacy_projection() {
        let vp = Viewport::default();
        // 8 units tall over 720 px => 90 px per unit.
        assert_eq!(vp.pixels_per_unit(&Camera::default()), 90.0);
        // +1 world unit is *up*, which is a smaller screen y.
        let (_, y_up) = vp.to_screen(&Camera::default(), (0.0, 1.0));
        let (_, y_down) = vp.to_screen(&Camera::default(), (0.0, -1.0));
        assert_eq!(y_up, 270.0);
        assert_eq!(y_down, 450.0);
        assert!(y_up < y_down, "world +y must render above world -y");
    }

    #[test]
    fn zoom_halves_the_visible_height_and_doubles_pixels_per_unit() {
        let vp = Viewport::default();
        let camera = Camera::new((0.0, 0.0), 2.0);
        assert_eq!(camera.visible_height(), 4.0);
        assert_eq!(vp.pixels_per_unit(&camera), 180.0);
        // The frame's half-height is 2 world units at zoom 2, so 2 units above
        // the centre is the top edge of the frame.
        assert_eq!(vp.to_screen(&camera, (0.0, 2.0)), (640.0, 0.0));
    }

    #[test]
    fn pan_maps_the_camera_centre_to_the_frame_centre() {
        let vp = Viewport::default();
        let camera = Camera::new((1.5, -0.5), 1.25);
        assert_eq!(vp.to_screen(&camera, camera.center), (640.0, 360.0));
    }

    #[test]
    fn screen_and_world_are_inverses() {
        let vp = Viewport::default();
        for camera in [
            Camera::default(),
            Camera::new((1.5, -0.5), 1.0),
            Camera::new((-3.0, 2.0), 4.0),
        ] {
            for world in [(0.0, 0.0), (3.0, 4.0), (-2.25, 1.75)] {
                let back = vp.to_world(&camera, vp.to_screen(&camera, world));
                assert!((back.0 - world.0).abs() < 1e-9, "{back:?} vs {world:?}");
                assert!((back.1 - world.1).abs() < 1e-9, "{back:?} vs {world:?}");
            }
        }
    }

    #[test]
    fn translation_preserves_distances() {
        let vp = Viewport::default();
        let a = vp.to_screen(&Camera::default(), (0.0, 0.0));
        let b = vp.to_screen(&Camera::default(), (3.0, 4.0));
        let d = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        // A 3-4-5 triangle at 90 px/unit is 5 * 90 px.
        assert!((d - 450.0).abs() < 1e-9);
    }

    /// The derived `Default` would have been `zoom = 0.0`, which makes
    /// `visible_height()` infinite. The hand-written one is the point.
    #[test]
    fn the_default_camera_does_not_divide_by_zero() {
        let camera = Camera::default();
        assert_eq!(camera.zoom, 1.0);
        assert_eq!(camera.visible_height(), Camera::WORLD_HEIGHT);
        assert!(camera.visible_height().is_finite());
    }
}
