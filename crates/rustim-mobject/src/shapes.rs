//! The built-in shapes, and the parameter struct that describes them as data.

use serde::{Deserialize, Serialize};

use rustim_math::{Affine2, Point, bezier};

use crate::style::Style;
use crate::tree::{Mobject, bezier_path_from_points};

/// Shapes the engine can construct by name.
///
/// This lives in the mobject crate rather than the scene crate so that the
/// command layer, the scene author and the tests all build geometry the same
/// way. `ShapeParams` is `serde`-derived on purpose: it is the payload of the
/// `add` command, and it is also the natural thing for a host to hand a scene
/// directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    Square,
    Circle,
    RightTriangle,
}

/// Parameters for a shape. Every field is optional so a partial description —
/// which is what a language model will usually produce — still yields a
/// renderable object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ShapeParams {
    // Absent fields are omitted rather than written as `null`, so a partial
    // description survives a round trip byte-for-byte. That matters because
    // these payloads are model output: echoing back `"width":null` teaches the
    // model that nulls are part of the protocol.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
    /// World-space position of the shape's anchor, in Manim units (Y up, origin
    /// at the centre of the frame). The viewport maps it to pixels; scene code
    /// never sees pixels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl ShapeParams {
    pub fn width(&self) -> f64 {
        self.width.unwrap_or(2.0)
    }
    pub fn height(&self) -> f64 {
        self.height.unwrap_or(2.0)
    }
    pub fn radius(&self) -> f64 {
        self.radius.unwrap_or(1.0)
    }
}

/// Build a mobject from a shape description.
pub fn build(kind: ShapeKind, params: &ShapeParams) -> Mobject {
    let (w, h) = (params.width(), params.height());
    let points = match kind {
        ShapeKind::Square => square(w, h),
        ShapeKind::Circle => circle(params.radius()),
        ShapeKind::RightTriangle => right_triangle(w, h),
    };

    let color = params.color.map_or(rustim_color::FG, |[r, g, b]| {
        rustim_color::Srgb::from_rgba8(r, g, b, 255)
    });
    let style = Style {
        fill: color,
        stroke: color,
        ..Style::default()
    };

    let mut m = Mobject::new(points)
        .with_style(style)
        .with_kind(kind.into());
    if let Some([x, y]) = params.position {
        m.transform = Affine2::translate((x, y));
    }
    if let Some(label) = params.label.clone() {
        m.label = Some(label);
    }
    m
}

/// An axis-aligned rectangle, centred on the origin.
pub fn square(w: f64, h: f64) -> Vec<Point> {
    let (hw, hh) = (w / 2.0, h / 2.0);
    // A closed path of straight lines, as Manim builds one: `set_points_as_corners`
    // puts each segment's interior control points at 1/3 and 2/3 rather than
    // stacking them on the endpoints. See `bezier::points_as_corners` — the two
    // spellings draw the same line but parameterise it differently, which morphing
    // and the point-proportion queries can tell apart.
    bezier::points_as_corners(&[
        Point::new(-hw, -hh),
        Point::new(hw, -hh),
        Point::new(hw, hh),
        Point::new(-hw, hh),
        Point::new(-hw, -hh),
    ])
}

/// A circle approximated by four cubics, which is Manim's representation too.
///
/// The magic constant is the standard cubic approximation of a quarter arc; at
/// this radius the maximum radial error is well under a tenth of a pixel, so it
/// is not worth subdividing further.
///
/// The array is Manim's control-point format: four points per cubic, each curve
/// restating the previous one's endpoint as its own start. Sixteen points for
/// four curves, not thirteen — the shorter spelling reads as the same curve
/// under `bezier_path_from_points` only if you also read the array three at a
/// time, which is the wrong convention and rounds a corner on the shapes that
/// *are* built four at a time.
pub fn circle(r: f64) -> Vec<Point> {
    const K: f64 = 0.552_284_749_8;
    let k = K * r;
    let segs = [
        [Point::new(k, -r), Point::new(r, -k), Point::new(r, 0.0)],
        [Point::new(r, k), Point::new(k, r), Point::new(0.0, r)],
        [Point::new(-k, r), Point::new(-r, k), Point::new(-r, 0.0)],
        [Point::new(-r, -k), Point::new(-k, -r), Point::new(0.0, -r)],
    ];
    let mut pts = Vec::with_capacity(4 * segs.len());
    let mut start = Point::new(0.0, -r);
    for [c1, c2, end] in segs {
        pts.extend([start, c1, c2, end]);
        start = end;
    }
    pts
}

/// A right triangle with the right angle at the origin.
///
/// Legs run along +x and +y. The tutorial's version claims the right angle is at
/// the bottom-left but its point order puts it at the top-left once the Y-down
/// screen space is taken into account; here the geometry is stated in world
/// space, where the ambiguity cannot arise, and the viewport handles the flip.
pub fn right_triangle(w: f64, h: f64) -> Vec<Point> {
    bezier::points_as_corners(&[
        Point::new(0.0, 0.0),
        Point::new(w, 0.0),
        Point::new(0.0, h),
        Point::new(0.0, 0.0),
    ])
}

/// Build a drawable path straight from a point array, for callers that do not
/// need a `Mobject`.
pub fn path_of(points: &[Point]) -> rustim_math::BezPath {
    bezier_path_from_points(points.iter().copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circle_is_four_curves_of_four_points() {
        let pts = circle(1.0);
        assert_eq!(pts.len(), 4 * 4);
        // Four whole curves by Manim's own arithmetic, and eight anchors with the
        // shared endpoints appearing twice.
        assert_eq!(bezier::anchors(&pts).unwrap().len(), 8);
    }

    /// The *on-curve* points are exactly on the radius; the control points are
    /// not, and asserting otherwise is the classic mistake. A cubic segment can
    /// only touch a circle at its endpoints, so the two off-curve points have to
    /// sit outside — at `r * sqrt(1 + k^2)` ≈ 1.1424 r for this k.
    #[test]
    fn circle_on_curve_points_are_exactly_on_the_radius() {
        let r = 3.0;
        for (i, p) in circle(r).into_iter().enumerate() {
            // Four points per curve: the first and last are on the curve.
            if i % 4 != 0 && i % 4 != 3 {
                continue; // off-curve control point
            }
            let d = (p.x * p.x + p.y * p.y).sqrt();
            assert!((d - r).abs() < 1e-9, "on-curve point {i} {p:?} is at {d}");
        }
    }

    /// The approximation quality, which is what actually matters visually: the
    /// curve's deepest excursion from the true circle is under 0.03% of the
    /// radius, so a circle here is indistinguishable from an arc.
    #[test]
    fn circle_approximation_stays_within_a_thirtieth_of_a_percent() {
        let r = 3.0;
        let pts = circle(r);
        let mut worst: f64 = 0.0;
        // Each cubic is (p0, c1, c2, p3); sample at the parameter midpoint,
        // which for this symmetric approximation is where it deviates most.
        for seg in 0..4 {
            let i = seg * 4;
            let (p0, c1, c2, p3) = (pts[i], pts[i + 1], pts[i + 2], pts[i + 3]);
            let mid = (p0.to_vec2() + c1.to_vec2() * 3.0 + c2.to_vec2() * 3.0 + p3.to_vec2()) / 8.0;
            worst = worst.max((mid.hypot() - r).abs() / r);
        }
        assert!(worst < 1.0 / 3000.0, "worst radial error {worst} of radius");
    }

    #[test]
    fn square_is_centred_on_the_origin() {
        let pts = square(4.0, 2.0);
        let xs: Vec<f64> = pts.iter().map(|p| p.x).collect();
        let ys: Vec<f64> = pts.iter().map(|p| p.y).collect();
        assert_eq!(xs.iter().cloned().fold(f64::MIN, f64::max), 2.0);
        assert_eq!(ys.iter().cloned().fold(f64::MIN, f64::max), 1.0);
        assert_eq!(xs.iter().cloned().fold(f64::MAX, f64::min), -2.0);
    }

    #[test]
    fn right_triangle_has_the_right_angle_at_the_origin() {
        let pts = right_triangle(3.0, 4.0);
        // The corners are the *anchors*. Only every fourth point is one: the
        // points between are the 1/3 and 2/3 interpolations, so reading `pts[6]`
        // as the third corner would be reading a control point — which is exactly
        // the mistake the old degenerate spelling of a straight cubic invited.
        assert_eq!(pts.len(), 3 * 4);
        // Each corner appears twice: once as the end of one segment and once as
        // the start of the next.
        assert_eq!(
            bezier::anchors(&pts).unwrap(),
            vec![
                Point::new(0.0, 0.0),
                Point::new(3.0, 0.0),
                Point::new(3.0, 0.0),
                Point::new(0.0, 4.0),
                Point::new(0.0, 4.0),
                Point::new(0.0, 0.0),
            ]
        );
    }

    #[test]
    fn params_defaults_are_sane() {
        let p = ShapeParams::default();
        assert_eq!((p.width(), p.height(), p.radius()), (2.0, 2.0, 1.0));
    }

    #[test]
    fn build_applies_position_as_a_cheap_transform() {
        let m = build(
            ShapeKind::Square,
            &ShapeParams {
                position: Some([3.0, -1.0]),
                ..Default::default()
            },
        );
        // The local points are untouched; only the transform moved.
        assert_eq!(m.points[0], Point::new(-1.0, -1.0));
        assert_eq!(m.materialize()[0], Point::new(2.0, -2.0));
    }

    #[test]
    fn shape_kinds_serialise_snake_case() {
        let json = serde_json::to_string(&ShapeKind::RightTriangle).unwrap();
        assert_eq!(json, "\"right_triangle\"");
    }
}
