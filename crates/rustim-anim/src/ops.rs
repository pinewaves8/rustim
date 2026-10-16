//! Concrete animations.
//!
//! Three of them, chosen to exercise the three shapes the design has to support:
//! an animation that drives a scalar ([`FadeIn`]), one that interpolates a
//! transform ([`MoveTo`]) — where the deferred transform pays off directly — and
//! (next) one that interpolates *point arrays* (morph), which is what forces
//! Manim's `align_points` machinery and is deliberately not written yet.

use rustim_math::Affine2;
use rustim_mobject::{NodeId, SceneTree};

use crate::Animation;

/// Fades an object in by driving its opacity.
///
/// Note this touches `opacity`, not the style's `fill_opacity`. Manim keeps the
/// two independent and multiplies them at draw time; `CLAUDE.md` records that
/// they compose multiplicatively, so an object at half fill opacity that fades
/// in to 50% must end up at 25%, not 50%.
#[derive(Debug, Clone)]
pub struct FadeIn {
    target: NodeId,
    from: f64,
    to: f64,
    duration: f64,
}

impl FadeIn {
    pub fn new(target: NodeId) -> Self {
        Self {
            target,
            from: 0.0,
            to: 1.0,
            duration: 1.0,
        }
    }

    pub fn with_duration(mut self, seconds: f64) -> Self {
        self.duration = seconds;
        self
    }
}

impl Animation for FadeIn {
    fn duration(&self) -> f64 {
        self.duration
    }

    fn begin(&mut self, tree: &SceneTree) {
        // Fade from wherever the object currently is, not from hardcoded zero,
        // so a fade interrupted by another fade does not snap.
        if let Some(m) = tree.get(self.target) {
            self.from = m.opacity;
        }
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        if let Some(m) = tree.get_mut(self.target) {
            m.opacity = self.from + (self.to - self.from) * alpha;
            m.dirty.style = true;
        }
    }
}

/// Moves an object to a target transform.
///
/// This is the animation that demonstrates why transforms are deferred: the
/// whole move is an interpolation of one [`Affine2`], regardless of how many
/// points the object has. Manim rewrites every descendant's point array on each
/// frame of the equivalent animation.
///
/// Note it interpolates the *transform*, not a position, so a rotation or scale
/// carried in the same transform rides along. Interpolating a general affine
/// componentwise is not the same as interpolating a rotation's angle, so a
/// `Rotate` animation needs its own type rather than riding on this one.
#[derive(Debug, Clone)]
pub struct MoveTo {
    target: NodeId,
    target_transform: Affine2,
    start: Option<Affine2>,
    duration: f64,
}

impl MoveTo {
    pub fn new(target: NodeId, target_transform: Affine2) -> Self {
        Self {
            target,
            target_transform,
            start: None,
            duration: 1.0,
        }
    }

    pub fn with_duration(mut self, seconds: f64) -> Self {
        self.duration = seconds;
        self
    }
}

impl Animation for MoveTo {
    fn duration(&self) -> f64 {
        self.duration
    }

    fn begin(&mut self, tree: &SceneTree) {
        self.start = tree.get(self.target).map(|m| m.transform);
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        let Some(start) = self.start else { return };
        let Some(m) = tree.get_mut(self.target) else {
            return;
        };
        m.set_transform(lerp_affine(start, self.target_transform, alpha));
    }
}

/// Componentwise interpolation of two affine transforms.
///
/// `kurbo` has no `lerp` for `Affine`, and building one out of `as_coeffs` keeps
/// the six coefficients in the same layout the renderer already expects.
fn lerp_affine(a: Affine2, b: Affine2, t: f64) -> Affine2 {
    let (a, b) = (a.as_coeffs(), b.as_coeffs());
    let mut out = [0.0; 6];
    for i in 0..6 {
        out[i] = a[i] + (b[i] - a[i]) * t;
    }
    Affine2::new(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_math::Point;
    use rustim_mobject::Mobject;

    #[test]
    fn lerp_affine_endpoints_are_exact() {
        let a = Affine2::translate((1.0, 2.0));
        let b = Affine2::scale(3.0) * Affine2::translate((5.0, 6.0));
        assert_eq!(lerp_affine(a, b, 0.0), a);
        assert_eq!(lerp_affine(a, b, 1.0), b);
    }

    #[test]
    fn lerp_affine_moves_a_point_linearly() {
        let a = Affine2::IDENTITY;
        let b = Affine2::translate((10.0, -4.0));
        let mid = lerp_affine(a, b, 0.5);
        assert_eq!(mid * Point::new(0.0, 0.0), Point::new(5.0, -2.0));
    }

    #[test]
    fn move_to_marks_geometry_dirty() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(vec![Point::new(0.0, 0.0)]));
        tree.get_mut(id).unwrap().dirty = Default::default();
        let mut anim = MoveTo::new(id, Affine2::translate((1.0, 0.0)));
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);
        assert!(tree.get(id).unwrap().dirty.geometry);
    }
}
