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

/// Manim's coordinate convention, made explicit.
///
/// Manim is Y-up with the origin at the centre of the frame; vello and every
/// windowing API we target are Y-down with the origin at the top-left. The
/// tutorial's sketches have no transform layer at all and position objects in
/// raw pixels, which silently flips every figure (see `CLAUDE.md`, "Known issues
/// in the tutorial sketches"). This struct is the fix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// Frame size in physical pixels.
    pub frame_width: u16,
    pub frame_height: u16,
    /// How many world units tall the frame is. Manim's default `frame_height`
    /// is 8.0, which is why scene code in this workspace can use numbers like
    /// `3.0` and `4.0` for the legs of a Pythagorean triangle.
    pub units_high: f64,
    /// The world-space point that maps to the centre of the frame.
    pub center: (f64, f64),
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            frame_width: 1280,
            frame_height: 720,
            units_high: 8.0,
            center: (0.0, 0.0),
        }
    }
}

impl Viewport {
    /// World units per pixel. Uniform on both axes, so circles stay round.
    pub fn scale(&self) -> f64 {
        f64::from(self.frame_height) / self.units_high
    }

    /// The world→screen transform: Y-up centred world space to Y-down
    /// top-left-origin screen space.
    pub fn affine(&self) -> Affine2 {
        let s = self.scale();
        Affine::translate((
            f64::from(self.frame_width) / 2.0,
            f64::from(self.frame_height) / 2.0,
        )) * Affine::scale_non_uniform(s, -s)
            * Affine::translate((-self.center.0, -self.center.1))
    }

    /// The inverse, for turning a pointer position back into world coordinates.
    pub fn to_world(&self, screen: (f64, f64)) -> (f64, f64) {
        let p = self.affine().inverse() * Point::new(screen.0, screen.1);
        (p.x, p.y)
    }

    /// Map a world point to screen pixels.
    pub fn to_screen(&self, world: (f64, f64)) -> (f64, f64) {
        let p = self.affine() * Point::new(world.0, world.1);
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
        assert_eq!(vp.to_screen((0.0, 0.0)), (640.0, 360.0));
    }

    #[test]
    fn world_y_is_flipped_relative_to_screen_y() {
        let vp = Viewport::default();
        // 8 units tall over 720 px => 90 px per unit.
        assert_eq!(vp.scale(), 90.0);
        // +1 world unit is *up*, which is a smaller screen y.
        let (_, y_up) = vp.to_screen((0.0, 1.0));
        let (_, y_down) = vp.to_screen((0.0, -1.0));
        assert_eq!(y_up, 270.0);
        assert_eq!(y_down, 450.0);
        assert!(y_up < y_down, "world +y must render above world -y");
    }

    #[test]
    fn screen_and_world_are_inverses() {
        let vp = Viewport {
            center: (1.5, -0.5),
            ..Default::default()
        };
        for world in [(0.0, 0.0), (3.0, 4.0), (-2.25, 1.75)] {
            let back = vp.to_world(vp.to_screen(world));
            assert!((back.0 - world.0).abs() < 1e-9, "{back:?} vs {world:?}");
            assert!((back.1 - world.1).abs() < 1e-9, "{back:?} vs {world:?}");
        }
    }

    #[test]
    fn translation_preserves_distances() {
        let vp = Viewport::default();
        let a = vp.to_screen((0.0, 0.0));
        let b = vp.to_screen((3.0, 4.0));
        let d = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        // A 3-4-5 triangle at 90 px/unit is 5 * 90 px.
        assert!((d - 450.0).abs() < 1e-9);
    }
}
