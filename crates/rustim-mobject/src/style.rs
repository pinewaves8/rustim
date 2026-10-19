//! Per-mobject appearance and dirty tracking.

use rustim_color::Srgb;
use rustim_math::stroke::{Cap, Join};

/// How a mobject is painted.
///
/// `fill_opacity` and `stroke_opacity` live here rather than being folded into
/// the colours, because Manim treats them as independent knobs and animations
/// like `FadeIn` move the mobject's own opacity while leaving these alone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub fill: Srgb,
    pub fill_opacity: f64,
    pub stroke: Srgb,
    pub stroke_opacity: f64,
    pub stroke_width: f64,
    /// How two segments of the stroke meet, and how its ends are finished.
    ///
    /// Both are `Copy` and both default to what the stroker already did, so a
    /// style that never mentions them draws exactly the picture it always did —
    /// which is what keeps every existing baseline valid.
    pub stroke_join: Join,
    pub stroke_cap: Cap,
    /// `[on, off]` in world units, or `None` for a solid line.
    ///
    /// One pair rather than a general pattern, because `Style` is `Copy` and a
    /// `Vec` is not. See `rustim_math::stroke::StrokeStyle`.
    pub stroke_dash: Option<[f64; 2]>,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            fill: rustim_color::FG,
            fill_opacity: 1.0,
            stroke: rustim_color::FG,
            stroke_opacity: 0.0,
            // A *world-unit* width, not Manim's pixels. The default stroke is
            // off, so the value is latent — but a host that turns
            // `stroke_opacity` up without setting a width must get a hairline
            // and not a two-unit border. See `shapes::DEFAULT_THICKNESS` for
            // where `0.045` comes from.
            stroke_width: crate::shapes::DEFAULT_THICKNESS,
            stroke_join: Join::default(),
            stroke_cap: Cap::default(),
            stroke_dash: None,
        }
    }
}

impl Style {
    /// Blend two styles for progress `t`, the way Manim's
    /// `VMobject.interpolate_color` blends `fill_rgbas` / `stroke_rgbas` /
    /// `stroke_width` (`types/vectorized_mobject.py:1965`).
    ///
    /// **`t == 1.0` returns `b` outright**, reproducing the loop's short-circuit
    /// to a *copy* of the target's value (`:1983-1987`). Be precise about what
    /// that buys, because it is easy to overstate: for finite components
    /// `(1 - 1) * a + 1 * b` already lands on `b` exactly, so the snap is a
    /// *guard* rather than a correction — it makes the endpoint unconditional
    /// (a non-finite component would otherwise turn the whole style into `NaN`)
    /// and keeps the structure identical to the source it came from.
    ///
    /// The colours are blended per channel with no colour-space conversion,
    /// because both ends are already sRGB and Manim blends the raw arrays too.
    pub fn lerp(a: Self, b: Self, t: f64) -> Self {
        if t == 1.0 {
            return b;
        }
        let k = t as f32;
        Self {
            fill: lerp_srgb(a.fill, b.fill, k),
            fill_opacity: lerp_scalar(a.fill_opacity, b.fill_opacity, t),
            stroke: lerp_srgb(a.stroke, b.stroke, k),
            stroke_opacity: lerp_scalar(a.stroke_opacity, b.stroke_opacity, t),
            stroke_width: lerp_scalar(a.stroke_width, b.stroke_width, t),
            // **Not interpolated, and they cannot be**: a miter does not become a
            // round half-way through, and halving a dash pattern is not a dash
            // pattern. A morph keeps the source's until it arrives, and the
            // `t == 1.0` short-circuit above hands over the target's outright —
            // which is the honest behaviour for a discrete property, and the
            // reason it costs no extra branch to get right.
            stroke_join: a.stroke_join,
            stroke_cap: a.stroke_cap,
            stroke_dash: a.stroke_dash,
        }
    }
}

/// `manim.utils.bezier.interpolate` (`utils/bezier.py:1067`) — `(1 - t) * a + t * b`.
///
/// Written in Manim's own order, not as `a + (b - a) * t`. The two differ in the
/// last bits, and this crate's rule is to reproduce Manim's *rounding path*, not
/// just its formula.
fn lerp_scalar(a: f64, b: f64, t: f64) -> f64 {
    (1.0 - t) * a + t * b
}

fn lerp_srgb(a: Srgb, b: Srgb, t: f32) -> Srgb {
    let (a, b) = (a.components, b.components);
    let mut out = [0.0f32; 4];
    for (i, o) in out.iter_mut().enumerate() {
        *o = (1.0 - t) * a[i] + t * b[i];
    }
    Srgb::new(out)
}

/// Which caches a change invalidates.
///
/// The frame loop in `方案建议.md` §4.3 rebuilds only the dirty part of the
/// scene, so every mutation has to say what it spoiled. Keeping this as flags
/// (rather than dirtying everything unconditionally) is the difference between
/// the manimgl approach we are copying and the ManimCommunity one we are not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirtyFlags {
    /// Point positions changed; geometry caches and any uploaded buffers are stale.
    pub geometry: bool,
    /// Colour, stroke width or opacity changed.
    pub style: bool,
    /// Draw order or parent/child structure changed.
    pub hierarchy: bool,
    /// What the object *means* changed: its label, or its
    /// [`crate::kind::NodeKind`].
    ///
    /// A flag of its own because none of the other three describes it — a
    /// relabelled square has the same points, the same paint and the same place
    /// in the tree, so a consumer that rebuilds only what `geometry | style |
    /// hierarchy` marks would serve a stale accessibility tree forever. The
    /// pixels are unaffected, which is exactly why this was easy to omit.
    pub semantic: bool,
}

impl DirtyFlags {
    pub const ALL: Self = Self {
        geometry: true,
        style: true,
        hierarchy: true,
        semantic: true,
    };

    #[inline]
    pub fn any(self) -> bool {
        self.geometry || self.style || self.hierarchy || self.semantic
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(fill: Srgb, fill_opacity: f64, stroke_width: f64) -> Style {
        Style {
            fill,
            fill_opacity,
            stroke: fill,
            stroke_opacity: 0.0,
            stroke_width,
            stroke_join: Join::default(),
            stroke_cap: Cap::default(),
            stroke_dash: None,
        }
    }

    /// Both colours and all three scalars move; a port that blended only the
    /// fill would leave a morphing shape's outline and stroke weight frozen.
    #[test]
    fn style_lerp_is_componentwise() {
        let a = styled(rustim_color::BLUE, 0.0, 0.0);
        let b = styled(rustim_color::YELLOW, 1.0, 8.0);
        let mid = Style::lerp(a, b, 0.5);

        for i in 0..4 {
            let want = 0.5 * a.fill.components[i] + 0.5 * b.fill.components[i];
            assert_eq!(mid.fill.components[i], want, "channel {i}");
        }
        assert_eq!(mid.stroke, mid.fill, "both colour slots move together");
        assert_eq!(mid.fill_opacity, 0.5);
        assert_eq!(mid.stroke_width, 4.0);
    }

    /// `t == 0.0` is the identity, so an animation interrupted before its first
    /// frame does not move the object it was handed.
    #[test]
    fn style_lerp_at_zero_is_the_start() {
        let a = styled(rustim_color::BLUE, 0.25, 3.0);
        let b = styled(rustim_color::YELLOW, 1.0, 8.0);
        assert_eq!(Style::lerp(a, b, 0.0), a);
    }

    /// The endpoint is the target *by construction*, not by arithmetic: with a
    /// non-finite component the lerp would produce `NaN` (`0 * inf`), and the
    /// short-circuit is what keeps the guarantee unconditional. Manim's own
    /// `interpolate_color` copies the target for the same reason.
    #[test]
    fn style_lerp_snaps_to_the_target_at_one() {
        let a = styled(rustim_color::BLUE, 0.0, 0.0);
        let b = styled(rustim_color::YELLOW, 1.0, 8.0);
        assert_eq!(Style::lerp(a, b, 1.0), b);

        let mut poisoned = a;
        poisoned.fill_opacity = f64::INFINITY;
        assert_eq!(Style::lerp(poisoned, b, 1.0), b);
    }
}
