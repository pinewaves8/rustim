//! A stroke whose **width varies along the path**, as a filled contour.
//!
//! This is the one thing the backend's stroker cannot express, and the reason is
//! structural rather than an omission: `kurbo::Stroke` carries a single `width`,
//! and `vello_cpu` strokes with one of them. So a variable-width stroke is drawn
//! as the *region* it covers — a ribbon — and emitted as a `FillPath`.
//!
//! # Why this is not the ribbon that was rejected
//!
//! `Plot`'s stroke was once built here as a closed ribbon expanded from a
//! polyline, and it was moved out deliberately, with the note: *"the stroker
//! belongs to the backend that already has one, and a ribbon's interior contour
//! is a second representation of a line to keep right. Do not put it back."*
//!
//! That ruling is about **fixed-width** strokes, where the stroker is available
//! and a ribbon is a second way to draw the same thing. A width that varies along
//! the path has no stroker to reach for, so the ribbon is not a second
//! representation — it is the only one. Fixed-width strokes still go through
//! [`crate::Style::stroke_width`] and the backend; this module is reached only
//! when `Mobject::stroke_widths` says the width is not constant.
//!
//! # The geometry
//!
//! Each anchor is offset along the normal of the local direction, by half the
//! width it carries, once on each side. The local direction at an interior anchor
//! is the **average of the two adjacent segments**, which is what keeps the ribbon
//! from kinking at a corner; the outline is then the left side forward and the
//! right side back, closed through the caps.
//!
//! At a sharp corner the two offsets can cross. That is left alone rather than
//! mitered: the contour is filled with `Fill::NonZero`, which fills the *union* of
//! a self-overlapping path, so a crossing draws the corner solid — the same
//! picture a miter join would give, without a second piece of geometry to keep
//! right.

use rustim_math::stroke::Cap;
use rustim_math::{BezPath, Point, Vec2};

/// The closed contour of a polyline stroked with one width per anchor.
///
/// `widths` must be the same length as `anchors`; a caller that cannot say that
/// is a caller with a bug, and the two ways of coping silently (repeat a width,
/// or truncate) both draw a picture nobody asked for. The command layer refuses
/// the mismatch by name, so this returns `None` rather than guessing.
///
/// `None` is also the answer for fewer than two anchors — there is no ribbon
/// through a single point — and for a set of anchors that are all the same point.
pub fn contour(anchors: &[Point], widths: &[f64], cap: Cap) -> Option<BezPath> {
    if anchors.len() < 2 || anchors.len() != widths.len() || widths.iter().any(|w| !w.is_finite()) {
        return None;
    }

    // **Consecutive equal anchors have to be merged first**, and this is not a
    // tidiness step: `bezier::anchors` ports Manim's `get_anchors`, which reports
    // the *start and end of every curve* — so an N-curve path arrives as 2N points
    // with every interior vertex listed twice. A repeated vertex is a zero-length
    // step, which has no direction, so leaving it in makes the whole contour
    // `None` and draws **nothing** — for the commonest input there is.
    let (anchors, widths) = merge_repeats(anchors, widths)?;

    let mut left: Vec<Point> = Vec::with_capacity(anchors.len());
    let mut right: Vec<Point> = Vec::with_capacity(anchors.len());
    for (i, (a, w)) in anchors.iter().zip(&widths).enumerate() {
        let normal = normal_at(&anchors, i)?;
        let half = w / 2.0;
        left.push(*a + normal * half);
        right.push(*a - normal * half);
    }

    let mut path = BezPath::new();
    let start_dir = direction(&anchors, 0)?;
    path.move_to((left[0].x, left[0].y));
    // The caps are the only place the ribbon needs points that are not offsets of
    // an anchor, so they are added here rather than in the per-anchor walk.
    for p in cap_points(left[0], -start_dir, widths[0], cap) {
        path.line_to((p.x, p.y));
    }
    for p in &left[1..] {
        path.line_to((p.x, p.y));
    }
    let end_dir = direction(&anchors, anchors.len() - 1)?;
    let last = anchors.len() - 1;
    for p in cap_points(left[last], end_dir, widths[last], cap) {
        path.line_to((p.x, p.y));
    }
    for p in right.iter().rev() {
        path.line_to((p.x, p.y));
    }
    for p in cap_points(right[0], -start_dir, widths[0], cap) {
        path.line_to((p.x, p.y));
    }
    path.close_path();
    Some(path)
}

/// Collapse runs of equal points, averaging the widths that met there.
///
/// `None` when fewer than two distinct points are left: a path that never goes
/// anywhere has no ribbon, and saying so beats emitting a contour with no area.
fn merge_repeats(anchors: &[Point], widths: &[f64]) -> Option<(Vec<Point>, Vec<f64>)> {
    let mut out: Vec<Point> = Vec::with_capacity(anchors.len());
    let mut out_w: Vec<f64> = Vec::with_capacity(widths.len());
    for (a, w) in anchors.iter().zip(widths) {
        match out.last_mut() {
            Some(last) if (last.x - a.x).abs() < 1e-12 && (last.y - a.y).abs() < 1e-12 => {
                let i = out_w.len() - 1;
                out_w[i] = (out_w[i] + w) / 2.0;
            }
            _ => {
                out.push(*a);
                out_w.push(*w);
            }
        }
    }
    (out.len() >= 2).then_some((out, out_w))
}

/// The unit normal of the polyline at anchor `i`.
///
/// The average of the adjacent directions at an interior anchor, and the single
/// available direction at either end. `None` when both adjacent segments are
/// degenerate — a zero-length step has no direction, and pretending it has one
/// would put the ribbon's edge somewhere arbitrary.
fn normal_at(anchors: &[Point], i: usize) -> Option<Vec2> {
    let dir = direction(anchors, i)?;
    // Perpendicular, in the screen's sense: rotate a quarter turn. Which of the
    // two perpendiculars is "left" does not matter, because the contour uses both
    // sides and the result is symmetric about the centerline.
    Some(Vec2::new(-dir.y, dir.x))
}

/// The unit direction of the polyline at anchor `i`, averaged at an interior one.
fn direction(anchors: &[Point], i: usize) -> Option<Vec2> {
    let n = anchors.len();
    let mut sum = Vec2::ZERO;
    if i > 0 {
        sum += unit(anchors[i].to_vec2() - anchors[i - 1].to_vec2())?;
    }
    if i + 1 < n {
        sum += unit(anchors[i + 1].to_vec2() - anchors[i].to_vec2())?;
    }
    unit(sum)
}

fn unit(v: Vec2) -> Option<Vec2> {
    let len = v.hypot();
    (len.is_finite() && len > 1e-12).then(|| v / len)
}

/// Extra points past `end`, in the direction `dir` is *away* from the ribbon.
///
/// A butt cap adds nothing, which is why it is the one that cannot go wrong. A
/// square cap walks half a width outward; a round one samples a half-circle, so
/// that the end reads as round at any zoom rather than as a polygon facet at the
/// resoution this happens to sample.
fn cap_points(end: Point, dir: Vec2, width: f64, cap: Cap) -> Vec<Point> {
    match cap {
        Cap::Butt => Vec::new(),
        Cap::Square => vec![end + dir * (width / 2.0)],
        Cap::Round => {
            // Eight steps, and the **endpoints are not among them**: the contour
            // already arrives at `end + normal·w/2` and leaves at
            // `end − normal·w/2`, so this samples only the interior of the arc.
            // The sweep runs from the normal side round through `dir` to the
            // other side — getting that backwards walks the cap *behind* the
            // endpoint, which reads as a cap that never reaches the far point.
            let steps = 8;
            let normal = Vec2::new(-dir.y, dir.x);
            (1..steps)
                .map(|k| {
                    let t = std::f64::consts::PI * (k as f64) / (steps as f64);
                    let (s, c) = t.sin_cos();
                    end + dir * (s * width / 2.0) + normal * (c * width / 2.0)
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_math::Shape;

    fn line() -> Vec<Point> {
        vec![
            Point::new(0.0, 0.0),
            Point::new(1.0, 0.0),
            Point::new(2.0, 1.0),
        ]
    }

    /// A **straight** run, which is the only case where "the outline is this wide"
    /// has one answer: on a polyline that turns, each anchor's normal is tilted and
    /// the box is wider than any single width — correctly so.
    fn straight() -> Vec<Point> {
        vec![
            Point::new(0.0, 0.0),
            Point::new(1.0, 0.0),
            Point::new(2.0, 0.0),
        ]
    }

    /// The width at each anchor is what the caller asked for: the outline is that
    /// far from the centerline on both sides, so a ribbon that widens along a
    /// straight run is exactly as tall as its widest end.
    #[test]
    fn the_outline_is_the_width_the_caller_named() {
        let path = contour(&straight(), &[0.4, 0.8, 1.2], Cap::Butt).expect("a ribbon");
        let bbox = path.bounding_box();
        assert!((bbox.y1 - bbox.y0 - 1.2).abs() < 1e-9, "{bbox:?}");
        assert!((bbox.y0 + 0.6).abs() < 1e-9, "and it is centred: {bbox:?}");
        assert!(bbox.x0.abs() < 1e-9, "the butt cap stops at the endpoint");
        assert!((bbox.x1 - 2.0).abs() < 1e-9, "at both of them");
    }

    /// A **constant** width is the sanity check that the contour is a stroke of
    /// that width: its area is the centerline's length times the width, which is
    /// what the stroker would have drawn through the same anchors.
    #[test]
    fn a_constant_width_has_the_area_of_that_stroke() {
        let anchors = vec![Point::new(0.0, 0.0), Point::new(3.0, 0.0)];
        let path = contour(&anchors, &[0.5, 0.5], Cap::Butt).expect("a ribbon");
        let area = path.area().abs();
        assert!(
            (area - 3.0 * 0.5).abs() < 1e-9,
            "area {area} is not length × width"
        );
    }

    /// The shape `bezier::anchors` actually reports: **two points per curve**, so
    /// an interior vertex arrives twice. Leaving the repeat in makes the contour
    /// `None` — a zero-length step has no direction — and the ribbon then draws
    /// *nothing*, which is the commonest input there is. Nothing is worse than
    /// wrong here: a wrong ribbon would at least be visible.
    #[test]
    fn a_vertex_listed_twice_is_merged_rather_than_refused() {
        let doubled = vec![
            Point::new(0.0, 0.0),
            Point::new(1.0, 0.0),
            Point::new(1.0, 0.0),
            Point::new(2.0, 0.0),
        ];
        let merged = contour(&doubled, &[0.5, 0.5, 0.5, 0.5], Cap::Butt)
            .expect("the repeat is the anchor layout's, not a mistake");
        let plain = contour(&straight(), &[0.5, 0.5, 0.5], Cap::Butt).unwrap();
        assert_eq!(
            merged.bounding_box(),
            plain.bounding_box(),
            "merging has to leave exactly the ribbon a three-anchor call gives"
        );

        // The two widths that named the shared vertex are **averaged**, so a
        // caller who gave it the same number twice gets that number.
        let same = contour(&doubled, &[0.5, 1.0, 1.0, 0.5], Cap::Butt).unwrap();
        assert!(
            (same.bounding_box().y1 - 0.5).abs() < 1e-9,
            "{:?}",
            same.bounding_box()
        );
        let split = contour(&doubled, &[0.5, 1.0, 0.5, 0.5], Cap::Butt).unwrap();
        assert!(
            (split.bounding_box().y1 - 0.375).abs() < 1e-9,
            "0.5 and 1.0 average to 0.75, whose half is 0.375: {:?}",
            split.bounding_box()
        );
    }

    /// A mismatch is a caller bug, and guessing would draw a picture nobody asked
    /// for — so it is `None`, and the command layer is where it becomes a refusal
    /// with both numbers in it.
    #[test]
    fn a_width_list_that_does_not_match_is_none() {
        assert!(contour(&line(), &[0.4, 0.8], Cap::Butt).is_none());
        assert!(contour(&line(), &[0.4, 0.8, 1.2, 1.6], Cap::Butt).is_none());
        assert!(contour(&[Point::new(0.0, 0.0)], &[1.0], Cap::Butt).is_none());
        assert!(contour(&line(), &[0.4, f64::NAN, 1.2], Cap::Butt).is_none());
    }

    /// A cap reaches past the end, which is the whole of what it does.
    #[test]
    fn a_square_cap_reaches_half_a_width_past_the_end() {
        let anchors = vec![Point::new(0.0, 0.0), Point::new(3.0, 0.0)];
        let butt = contour(&anchors, &[1.0, 1.0], Cap::Butt).unwrap();
        let square = contour(&anchors, &[1.0, 1.0], Cap::Square).unwrap();
        assert!((square.bounding_box().x1 - butt.bounding_box().x1 - 0.5).abs() < 1e-9);
        let round = contour(&anchors, &[1.0, 1.0], Cap::Round).unwrap();
        assert!(
            (round.bounding_box().x1 - butt.bounding_box().x1 - 0.5).abs() < 1e-3,
            "a round cap reaches the same distance, to within its sampling"
        );
    }
}
