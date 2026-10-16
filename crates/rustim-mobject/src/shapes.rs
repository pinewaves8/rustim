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

/// The inverse of [`path_of`]: a `kurbo` path back into Manim's flat
/// control-point array, four points per cubic.
///
/// Glyph outlines arrive as `BezPath`s (from `skrifa`), and a mobject stores
/// points, so something has to cross between the two. Doing it here — in the
/// mobject crate, next to `path_of` — keeps the outline *source* out of this
/// layer entirely: nothing below `rustim-text` learns what a font is, and a
/// future outline source (a different shaper, hand-authored SVG paths, a
/// boolean-op result) reuses this instead of reimplementing it.
///
/// **Every segment is emitted as a cubic, and every segment restates the
/// previous segment's endpoint as its own start.** That is the same format
/// [`bezier_path_from_points`] reads and the one [`bezier::anchors`] requires a
/// multiple of four for; a `LineTo` written as a bare pair of points would make
/// the array a different shape than every polygon in this engine and quietly
/// break the geometry readers. `LineTo` and `ClosePath` therefore go through
/// [`bezier::points_as_corners`], which puts the interior control points at
/// `1/3` and `2/3` rather than stacking them on the endpoints — the degenerate
/// spelling draws the same line and parameterises it differently, and
/// `CLAUDE.md` records why that is not an acceptable "equivalent".
///
/// A subpath that is closed with its current point already at the start emits
/// no closing segment: the points would be four copies of one coordinate, which
/// adds nothing to the drawn path and does add a curve to traverse for anything
/// that parameterises the array (morphing, `proportions_along_bezier_curve`).
///
/// # More than one subpath becomes one loop through a doubled slit
///
/// This is a real limitation of the format, not a detail. [`path_of`] reads the
/// array back with a single `move_to` and nothing but `curve_to` after it, so an
/// array cannot say "lift the pen here" — which means the second contour of an
/// `o`, a `d`, an `=`, a `%` has to be *reachable* from the first by a segment
/// that is part of the path. Concatenating naively would leave the implicit
/// closing edge of the whole array running from the last point back to the
/// first: one real edge straight across the glyph, on top of a counter that
/// fills in solid.
///
/// So each later contour is entered through a **hub** — the first point of the
/// first contour — and the loop returns to the hub at the end. Every bridge is
/// therefore traversed exactly twice, in opposite directions, and a segment
/// travelled once each way contributes nothing to the winding number. Under
/// `Fill::NonZero`, which is what this engine's rasteriser uses, the filled
/// region is then exactly the union of the contours with their own orientations:
/// a font's outer contour and its counter wind opposite ways, so the counter
/// cancels and stays empty. This is the same "keyhole" a triangulator uses to
/// put a hole in a polygon, and it is why the alternative — one mobject per
/// contour — is not needed to keep the per-symbol addressability the format is
/// otherwise costing.
///
/// The hub's own point is a genuine contour point, so [`bezier::anchors`] still
/// bounds the ink, and `critical_point` is unaffected.
///
/// An *open* single-contour path gets no trailing segment: the array is a
/// polyline, and closing it is the fill's business (a fill closes an open
/// subpath implicitly anyway).
pub fn points_from_path(path: &rustim_math::BezPath) -> Vec<Point> {
    use rustim_math::PathEl;

    let mut out = Vec::new();
    // Where the pen is now, and where the current subpath began. `ClosePath`
    // needs both, and the origin is otherwise unrecoverable — `kurbo` does not
    // restate it.
    let mut cur: Option<Point> = None;
    let mut origin: Option<Point> = None;
    // The first point of the first contour: every later contour hangs off it.
    let mut hub: Option<Point> = None;
    // Whether a second contour was ever entered, which is the only case that
    // needs the loop closed back onto the hub at the end.
    let mut bridged = false;

    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                match hub {
                    None => hub = Some(p),
                    Some(h) => {
                        // Out along the slit, then back along it to the new
                        // contour. Zero-length halves are skipped so that a
                        // contour that happens to start where the hub is does not
                        // add a curve that goes nowhere.
                        if let Some(from) = cur
                            && from != h
                        {
                            out.extend(bezier::points_as_corners(&[from, h]));
                        }
                        out.extend(bezier::points_as_corners(&[h, p]));
                        bridged = true;
                    }
                }
                cur = Some(p);
                origin = Some(p);
            }
            PathEl::LineTo(p) => {
                if let Some(from) = cur {
                    out.extend(bezier::points_as_corners(&[from, p]));
                }
                cur = Some(p);
            }
            PathEl::QuadTo(c, p) => {
                if let Some(from) = cur {
                    // Degree elevation. A quadratic is exactly representable as
                    // a cubic, so this loses nothing — and emitting a quadratic
                    // as if it were a cubic would bend the curve.
                    let c1 = from + (c - from) * (2.0 / 3.0);
                    let c2 = p + (c - p) * (2.0 / 3.0);
                    out.extend([from, c1, c2, p]);
                }
                cur = Some(p);
            }
            PathEl::CurveTo(c1, c2, p) => {
                if let Some(from) = cur {
                    out.extend([from, c1, c2, p]);
                }
                cur = Some(p);
            }
            PathEl::ClosePath => {
                if let (Some(from), Some(start)) = (cur, origin)
                    && from != start
                {
                    out.extend(bezier::points_as_corners(&[from, start]));
                }
                cur = origin;
            }
        }
    }

    // Close the loop onto the hub, so the array's implicit closing edge is a
    // point rather than a segment across the glyph.
    if bridged
        && let (Some(h), Some(from)) = (hub, cur)
        && from != h
    {
        out.extend(bezier::points_as_corners(&[from, h]));
    }
    out
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

    /// A `LineTo` has to become a *uniform* cubic, not four copies of its
    /// endpoints. Both draw the same segment, which is exactly why the
    /// difference is easy to lose: `bezier::anchors` sees a different curve,
    /// and morphing between a glyph outline and a square interpolates the two
    /// arrays elementwise.
    #[test]
    fn a_straight_segment_becomes_a_uniform_cubic_not_a_degenerate_one() {
        let mut path = rustim_math::BezPath::new();
        path.move_to(Point::new(0.0, 0.0));
        path.line_to(Point::new(2.0, 0.0));
        let pts = points_from_path(&path);

        assert_eq!(pts.len(), 4);
        assert_eq!(pts[0], Point::new(0.0, 0.0));
        assert_eq!(pts[3], Point::new(2.0, 0.0));
        // The interior control points sit a third and two thirds along — the
        // whole point of going through `points_as_corners`. Stacking them on
        // the endpoints would still draw this line.
        assert!((pts[1].x - 2.0 / 3.0).abs() < 1e-12, "{:?}", pts[1]);
        assert!((pts[2].x - 4.0 / 3.0).abs() < 1e-12, "{:?}", pts[2]);
    }

    /// The round trip the glyph pipeline depends on: a path built from a point
    /// array reads back as the same array. If this drifts, every glyph outline
    /// arrives in the engine subtly reshaped rather than obviously broken.
    #[test]
    fn a_closed_triangle_round_trips_through_points_and_back() {
        let corners = [
            Point::new(0.0, 0.0),
            Point::new(3.0, 0.0),
            Point::new(0.0, 4.0),
            Point::new(0.0, 0.0),
        ];
        let original = bezier::points_as_corners(&corners);
        assert_eq!(points_from_path(&path_of(&original)), original);
    }

    /// A quadratic lifted to a cubic is the *same curve*, which is the only
    /// reason degree elevation is allowed here. Reading a quadratic's control
    /// point as a cubic's first would move the curve anywhere off the line.
    #[test]
    fn a_quadratic_is_elevated_to_the_same_curve() {
        let mut path = rustim_math::BezPath::new();
        path.move_to(Point::new(0.0, 0.0));
        path.quad_to(Point::new(0.0, 1.0), Point::new(1.0, 1.0));
        let pts = points_from_path(&path);

        assert_eq!(pts.len(), 4);
        // A cubic's midpoint is `(start + 3·c1 + 3·c2 + end) / 8`.
        let mid =
            (pts[0].to_vec2() + pts[1].to_vec2() * 3.0 + pts[2].to_vec2() * 3.0 + pts[3].to_vec2())
                / 8.0;
        // ...and the quadratic's is `(start + 2·c + end) / 4` = (0.25, 0.75).
        assert!(
            (mid.x - 0.25).abs() < 1e-12 && (mid.y - 0.75).abs() < 1e-12,
            "{mid:?}"
        );
    }

    /// A subpath closed where it already started needs no closing segment. One
    /// would be four copies of a coordinate — invisible when drawn, but a whole
    /// extra curve for anything that walks the array.
    #[test]
    fn closing_a_path_that_is_already_at_its_origin_adds_no_segment() {
        let mut path = rustim_math::BezPath::new();
        path.move_to(Point::new(0.0, 0.0));
        path.line_to(Point::new(1.0, 0.0));
        path.line_to(Point::new(0.0, 0.0));
        path.close_path();

        let pts = points_from_path(&path);
        assert_eq!(pts.len(), 2 * 4);
        assert_eq!(pts[pts.len() - 1], Point::new(0.0, 0.0));
    }

    /// The property that makes a two-contour glyph survive the flat array: the
    /// counter stays empty. A `Vec<Point>` has no way to say "lift the pen", so
    /// the second contour is reached through a hub and a doubled slit — and if
    /// either half of that is wrong, the counter fills in solid.
    ///
    /// Asserted as a *winding number* rather than by eye, because that is the
    /// quantity `Fill::NonZero` actually reads: outside the ring 0, in the ring
    /// 1, in the counter 0.
    #[test]
    fn a_counter_stays_empty_when_the_contours_are_welded_into_one_loop() {
        let mut path = rustim_math::BezPath::new();
        // Outer, counter-clockwise.
        path.move_to(Point::new(-2.0, -2.0));
        path.line_to(Point::new(2.0, -2.0));
        path.line_to(Point::new(2.0, 2.0));
        path.line_to(Point::new(-2.0, 2.0));
        path.close_path();
        // Inner, wound the other way, as a font's counter is.
        path.move_to(Point::new(-1.0, -1.0));
        path.line_to(Point::new(-1.0, 1.0));
        path.line_to(Point::new(1.0, 1.0));
        path.line_to(Point::new(1.0, -1.0));
        path.close_path();

        let pts = points_from_path(&path);
        assert!(
            bezier::anchors(&pts).is_some(),
            "the welded array is still a whole number of curves",
        );
        assert_eq!(
            pts[pts.len() - 1],
            pts[0],
            "the loop returns to the hub, so the array's implicit closing edge is a point",
        );

        let poly = flatten(&pts);
        assert_eq!(winding(&poly, Point::new(3.0, 0.0)), 0, "outside the ring");
        assert_eq!(winding(&poly, Point::new(1.5, 0.0)), 1, "on the ring");
        assert_eq!(winding(&poly, Point::new(0.0, 0.0)), 0, "in the counter");
        // The slit runs from the hub to the second contour's start; a point on
        // either side of it must not be affected by the doubled traversal.
        assert_eq!(winding(&poly, Point::new(-1.5, -1.5)), 1, "beside the slit");
    }

    /// Sample a control-point array into a polygon, densely enough that the
    /// winding test below is about the topology and not the flattening.
    fn flatten(points: &[Point]) -> Vec<Point> {
        const STEPS: usize = 32;
        let mut poly = Vec::with_capacity(points.len() / 4 * STEPS);
        // `as_chunks` rather than `chunks_exact`: the trailing remainder is
        // empty by construction (the array is a whole number of curves) and the
        // fixed-size array is destructured directly.
        for &[p0, c1, c2, p3] in points.as_chunks::<4>().0 {
            for i in 0..STEPS {
                let t = i as f64 / STEPS as f64;
                let u = 1.0 - t;
                let b = p0.to_vec2() * (u * u * u)
                    + c1.to_vec2() * (3.0 * u * u * t)
                    + c2.to_vec2() * (3.0 * u * t * t)
                    + p3.to_vec2() * (t * t * t);
                poly.push(Point::new(b.x, b.y));
            }
        }
        poly
    }

    /// The standard crossing count, which is exactly what a nonzero-fill
    /// rasteriser computes.
    fn winding(poly: &[Point], p: Point) -> i32 {
        let mut w = 0;
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            let side = (b - a).cross(p - a);
            if a.y <= p.y {
                if b.y > p.y && side > 0.0 {
                    w += 1;
                }
            } else if b.y <= p.y && side < 0.0 {
                w -= 1;
            }
        }
        w
    }
}
