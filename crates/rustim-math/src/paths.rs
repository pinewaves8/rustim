//! Transformation paths: where a point goes between alpha 0 and 1.
//!
//! A literal port of `manim.utils.paths.path_along_arc` — the function Manim's
//! `Transform(path_arc=…)` uses to bend a straight morph into an arc.
//!
//! # Why a per-point function and not a closure
//!
//! Manim's `path_along_arc(arc_angle)` returns a closure `(start_points,
//! end_points, alpha) -> points` and stores it on the animation. Two things make
//! that shape wrong here:
//!
//! - the closure would have to be `Box<dyn Fn(…) + Send>` to live in an
//!   `Animation`, which allocates once per animation for a number;
//! - the only axis this engine can express is out of the screen. There is no 3D
//!   camera, so `axis` is [`space_ops::OUT`] by construction rather than by
//!   argument, and dropping it is what lets the signature be a plain `fn`.
//!
//! The animation stores the **angle** and calls this per point, which is the same
//! arithmetic in the same order.
//!
//! # The arithmetic, and the two pieces that look optional and are not
//!
//! For a chord `v = end − start`, the centre of the arc is the chord's midpoint
//! displaced perpendicular by `|v|/2 / tan(arc/2)`; each point is then rotated
//! about that centre by `alpha · arc`. Three details are reproduced rather than
//! tidied:
//!
//! - **The straight-path threshold is `< 0.01`**, not zero. Below it Manim
//!   returns `straight_path()` — plain `(1 − alpha)·start + alpha·end` — so a
//!   tiny angle must produce *exactly* a lerp, not an arc with a centre a
//!   kilometre away.
//! - **The perpendicular is `cross(OUT, v/2)`, i.e. `(−v.y/2, v.x/2)`.** The
//!   other order of the operands is the same vector negated, which turns the
//!   bow inside out — the one mistake that changes the picture while every
//!   endpoint still matches.
//! - **`arc_angle == π` skips the perpendicular term entirely.** `tan(π/2)` is
//!   not infinite in floating point (it is ≈ 1.633e16), so the term would be
//!   ~1e-16 rather than zero; Manim special-cases exactly `+π` and lets `−π`
//!   fall through, so a port that "simplified" this produces a different last
//!   bit on one half-turn and not the other.
//!
//! Manim 0.22 performs no range check on the angle (the older
//! `-π ≤ arc ≤ π` `ValueError` is gone), so neither does this.

use std::f64::consts::PI;

use crate::Point;
use crate::space_ops::{OUT, rotation_matrix};

/// Below this magnitude an arc is drawn as a straight line.
///
/// Manim's `STRAIGHT_PATH_THRESHOLD` (`manim/utils/paths.py`).
pub const STRAIGHT_PATH_THRESHOLD: f64 = 0.01;

/// Where the point `start` sits at `alpha` on its way to `end`, on an arc of
/// `arc_angle` radians.
///
/// A positive angle bows the path to one side and a negative one to the other;
/// `alpha = 0` and `alpha = 1` land on the endpoints to within rounding. Manim's
/// `clockwise_path` / `counterclockwise_path` are the `−π` and `+π` cases.
///
/// A non-finite angle propagates: every coordinate becomes `NaN` rather than
/// panicking, because the command layer refuses such values at the boundary and
/// an animation that is already running has no error channel.
pub fn path_along_arc(arc_angle: f64, start: Point, end: Point, alpha: f64) -> Point {
    if arc_angle.abs() < STRAIGHT_PATH_THRESHOLD {
        // Manim's `straight_path()` is `bezier.interpolate`, whose expression is
        // this one — not `start + (end − start)·alpha`, which rounds differently.
        return Point::new(
            (1.0 - alpha) * start.x + alpha * end.x,
            (1.0 - alpha) * start.y + alpha * end.y,
        );
    }

    let vx = end.x - start.x;
    let vy = end.y - start.y;
    let mut cx = start.x + 0.5 * vx;
    let mut cy = start.y + 0.5 * vy;
    if arc_angle != PI {
        // `cross(OUT, v/2)`: OUT is +z, so this is `(−v.y, v.x)/2`.
        let perpendicular = 1.0 / (arc_angle / 2.0).tan();
        cx += -vy * 0.5 * perpendicular;
        cy += vx * 0.5 * perpendicular;
    }

    // `centers + np.dot(start − centers, rot.T)`: a row vector times the
    // transpose is the matrix times the column, so this is an ordinary rotation
    // of the offset about the centre.
    let m = rotation_matrix(alpha * arc_angle, OUT);
    let px = start.x - cx;
    let py = start.y - cy;
    Point::new(
        cx + m[0][0] * px + m[0][1] * py,
        cy + m[1][0] * px + m[1][1] * py,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::FRAC_PI_2;

    /// Below the threshold the answer is a plain lerp, *exactly* — the arc's
    /// centre would be 200 units away and its sagitta is not observable, but the
    /// order of operations is: this is Manim's `straight_path`, and a scene that
    /// asked for "straight" must get the same bits as a `MoveTo`.
    #[test]
    fn a_tiny_angle_is_a_straight_lerp() {
        let a = Point::new(-1.0, 2.0);
        let b = Point::new(3.0, -4.0);
        for alpha in [0.0, 0.25, 0.5, 1.0] {
            let got = path_along_arc(0.0099, a, b, alpha);
            assert_eq!(
                got,
                Point::new(
                    (1.0 - alpha) * a.x + alpha * b.x,
                    (1.0 - alpha) * a.y + alpha * b.y
                )
            );
        }
        // At the threshold itself the arc is used, which is the `<` in Manim.
        assert_ne!(
            path_along_arc(0.01, a, b, 0.5),
            path_along_arc(0.0, a, b, 0.5)
        );
    }

    /// Both ends are fixed points of the route; a morph that drifted even at
    /// alpha 1 would leave the object somewhere else than its target.
    #[test]
    fn the_endpoints_are_fixed() {
        let a = Point::new(-2.0, 1.0);
        let b = Point::new(3.0, 4.0);
        for arc in [0.3, FRAC_PI_2, PI, -PI, 2.0] {
            let start = path_along_arc(arc, a, b, 0.0);
            let end = path_along_arc(arc, a, b, 1.0);
            assert!(
                (start - a).hypot() < 1e-12 && (end - b).hypot() < 1e-12,
                "arc {arc}: {start:?} .. {end:?}"
            );
        }
    }

    /// A half-turn about a horizontal chord passes exactly one unit below the
    /// midpoint when the chord is two units long — that is the circle's radius,
    /// and it pins the **sign** of the perpendicular. The opposite operand order
    /// in the cross product lands at +1 and would still pass a symmetric test.
    #[test]
    fn a_half_turn_bows_the_way_manim_bows() {
        let got = path_along_arc(PI, Point::new(-1.0, 0.0), Point::new(1.0, 0.0), 0.5);
        assert!(
            (got.x).abs() < 1e-12 && (got.y + 1.0).abs() < 1e-12,
            "{got:?}"
        );
    }

    /// The quarter arc's midpoint is on the same circle as its ends: equidistant
    /// from both, and exactly the radius away from the arc centre — which for a
    /// 90° arc over a 2-unit chord is √2, sitting one unit above the chord's
    /// midpoint.
    #[test]
    fn a_quarter_arc_midpoint_is_on_the_circle() {
        let a = Point::new(-1.0, 0.0);
        let b = Point::new(1.0, 0.0);
        let mid = path_along_arc(FRAC_PI_2, a, b, 0.5);
        let (da, db) = ((mid - a).hypot(), (mid - b).hypot());
        assert!((da - db).abs() < 1e-12, "{da} vs {db}");
        let radius = (mid - Point::new(0.0, 1.0)).hypot();
        assert!((radius - 2.0_f64.sqrt()).abs() < 1e-12, "radius {radius}");
    }
}
