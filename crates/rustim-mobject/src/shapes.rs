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
    Line,
    Arrow,
    /// A horizontal axis with ticks: a one-dimensional coordinate frame.
    NumberLine,
    /// Two axes crossing at the origin, with ticks on both.
    Axes,
    /// A sampled curve, drawn as a **stroked centerline** rather than as a
    /// filled polygon: the fill rule would close the open polyline with its
    /// chord and paint the region under the curve.
    Plot,
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
    /// How thick a [`ShapeKind::Line`] or [`ShapeKind::Arrow`] is drawn, in world
    /// units — for a line this becomes `Style::stroke_width`, for an arrow the
    /// shaft's width inside the filled contour. It is the stroke width of a
    /// [`ShapeKind::Plot`] too. Ignored by the closed shapes, which are defined
    /// by `width`/`height`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thickness: Option<f64>,
    /// World-space position of the shape's anchor, in Manim units (Y up, origin
    /// at the centre of the frame). The viewport maps it to pixels; scene code
    /// never sees pixels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The data range a [`ShapeKind::NumberLine`] spans, `[min, max]`.
    ///
    /// Only the *tick positions* come from this: the line's geometric length is
    /// `width` (the `line` convention, not `square`'s), and the two are
    /// deliberately separate numbers so that a number line of length 6 can show
    /// the range `[-3, 3]` or `[0, 12]` without moving.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<[f64; 2]>,
    /// The data ranges a [`ShapeKind::Axes`] spans, `[min, max]` each.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x_range: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y_range: Option<[f64; 2]>,
    /// Data units between ticks, for a number line or a set of axes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    /// How far a tick rises above its axis, in world units.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tick_size: Option<f64>,
    /// A [`ShapeKind::Plot`]'s samples, **in world coordinates**.
    ///
    /// World rather than data coordinates on purpose: the command layer has no
    /// expression language and no opinion about what a sample means, so a host
    /// that wants `y = 0.25x² - 2` samples it itself and hands over the points it
    /// wants drawn. [`AxesScale::to_world`] is the mapping a scene uses to do
    /// that against the axes it is drawing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub points: Option<Vec<[f64; 2]>>,
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
    pub fn thickness(&self) -> f64 {
        self.thickness.unwrap_or(DEFAULT_THICKNESS)
    }
    pub fn step(&self) -> f64 {
        self.step.unwrap_or(1.0)
    }
    pub fn tick_size(&self) -> f64 {
        self.tick_size.unwrap_or(DEFAULT_TICK_SIZE)
    }
}

/// How thick a line or an arrow's shaft is when the caller did not say — and,
/// as of the stroke step, the default `Style::stroke_width`.
///
/// Manim's `Line` draws at `stroke_width = 4` px; against the default camera's
/// 8 world units over the default viewport's 720 px that is `4 · 8 / 720`, or 0.0444 units, rounded here
/// to 0.045. It is a *scene-side* convention rather than a property of geometry,
/// which is why it is a constant with this comment and not a derivation: the
/// px↔unit mapping is the viewport's business and a mobject never learns it.
/// The width is in **world units**, so it scales with the camera; Manim's is in
/// pixels and does not. That is the honest consequence of this engine folding
/// the projection into the path transform: a stroked circle and a filled one
/// zoom together instead of the outline staying a constant screen width.
pub const DEFAULT_THICKNESS: f64 = 0.045;

/// How far a tick rises above its axis when the caller did not say.
///
/// Manim's `NumberLine` tick size is 0.1 `frame_height/8` units, i.e. 0.1 world
/// units here; 0.15 is a little larger so that a tick survives a rasterised
/// 1-unit step without becoming a hairline — the golden image is the check.
pub const DEFAULT_TICK_SIZE: f64 = 0.15;

/// Build a mobject from a shape description.
pub fn build(kind: ShapeKind, params: &ShapeParams) -> Mobject {
    let (w, h) = (params.width(), params.height());
    let points = match kind {
        ShapeKind::Square => square(w, h),
        ShapeKind::Circle => circle(params.radius()),
        ShapeKind::RightTriangle => right_triangle(w, h),
        ShapeKind::Line => line(w),
        ShapeKind::Arrow => arrow(w, params.thickness()),
        ShapeKind::NumberLine => {
            // The default range spans the line's own length, so with no range
            // given one data unit is one world unit — the honest default for a
            // number line, and the one that keeps `{"type":"number_line"}`
            // renderable rather than empty.
            let [lo, hi] = params.range.unwrap_or([-w / 2.0, w / 2.0]);
            number_line(
                w,
                (lo, hi),
                params.step(),
                params.tick_size(),
                params.thickness(),
            )
        }
        ShapeKind::Axes => {
            let [x0, x1] = params.x_range.unwrap_or([-4.0, 4.0]);
            let [y0, y1] = params.y_range.unwrap_or([-3.0, 3.0]);
            axes(
                w,
                h,
                (x0, x1),
                (y0, y1),
                params.step(),
                params.tick_size(),
                params.thickness(),
            )
        }
        ShapeKind::Plot => match &params.points {
            Some(points) => plot(points),
            // No samples is a legitimate description — a host may add the
            // object before it has data — and it is an empty mobject rather
            // than an error, which is what `add` can say.
            None => Vec::new(),
        },
    };

    let color = params.color.map_or(rustim_color::FG, |[r, g, b]| {
        rustim_color::Srgb::from_rgba8(r, g, b, 255)
    });
    // A **line** and a **plot** are centerlines drawn by the backend's stroker;
    // everything else is a closed contour drawn by the fill rule. Two spellings
    // of the same shape are not interchangeable here: filling an open polyline
    // paints the region between it and its closing chord (the plot-as-an-area
    // defect), and stroking a closed contour draws an outline *around* a
    // rectangle that was meant to be the shape's interior (a square's edge
    // would double in size). So the kind decides which half of `Style` is on.
    let (fill_opacity, stroke_opacity) = match kind {
        ShapeKind::Line | ShapeKind::Plot => (0.0, 1.0),
        _ => (1.0, 0.0),
    };
    let style = Style {
        fill: color,
        fill_opacity,
        stroke: color,
        stroke_opacity,
        // For a stroked kind this is the stroke's width; the `thickness`
        // parameter of `Line`/`Arrow` remains the arrow's shaft thickness,
        // because an arrow is still a filled contour (see `arrow`).
        stroke_width: match kind {
            ShapeKind::Line | ShapeKind::Plot => params.thickness(),
            _ => DEFAULT_THICKNESS,
        },
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

/// A straight segment from the origin along `+x`: a **centerline**, `length`
/// long, drawn by the backend's stroker rather than faked as a thin filled
/// rectangle.
///
/// The two points below are the whole geometry. The width is not a geometric
/// argument at all — [`build`] reads `ShapeParams::thickness` into
/// `Style::stroke_width`, which is a style knob a host can animate and which
/// the draw walk emits as `DrawOp::StrokePath`. That is the honest spelling of
/// a line: its extent *is* its length (the accessibility layer's diameter
/// measurement agrees exactly instead of rounding the diagonal of the old
/// rectangle), and a zoomed camera thickens the pen rather than moving an
/// invisible edge.
///
/// Anchored at its **tail**, pointing along `+x` — matching
/// [`right_triangle`]'s "right angle at the origin" rather than
/// [`square`]'s "centred on the origin", because the tail is the point a caller
/// pivots and rotates around.
///
/// **An arrow is not built this way**, and cannot be with a flat point array:
/// Manim's arrow is a stroked shaft plus a *filled* triangular tip, which is
/// two contours — and a Manim point array has one subpath (`points_from_path`
/// welds a glyph's counters for exactly this reason). `arrow` stays a filled
/// contour until mobjects can own children.
pub fn line(length: f64) -> Vec<Point> {
    bezier::points_as_corners(&[Point::new(0.0, 0.0), Point::new(length, 0.0)])
}

/// An arrow: a shaft along `+x` from the origin, ending in a triangular head whose
/// point is at `(length, 0)`.
///
/// Drawn as filled geometry — unlike [`line`], which is a stroke — because a
/// stroked shaft and a filled head need two paint roles in one object. It is
/// **one closed contour of seven corners**, not a shaft and a head, because the
/// head is wider than the shaft: the union of the two is a single simple
/// polygon, so no subpath break is needed — and a flat Manim point array cannot
/// express one anyway (see `shapes::points_from_path` and the welded-glyph note
/// in `CLAUDE.md`). The consequence is a shape the draw walk's fill pass renders,
/// with no stroke and no welding.
///
/// The head's proportions are Manim's: `Arrow`'s `tip_length` defaults to `0.25`
/// and is capped by `max_tip_length_to_length_ratio = 0.25` times the arrow's
/// length (`animation/mobject/geometry.py`), so the head can never eat a short
/// arrow. Its *width* is ours — Manim fills a separate `ArrowTriangleFilledTip`
/// submobject and strokes the shaft, and with no groups in this module the head
/// is sized here as wide as it is long, which is what a drawn
/// triangle tip looks like.
///
/// The shaft is clamped to the head's width so that a caller asking for an absurd
/// thickness still gets a simple polygon: past that point the outline would cross
/// itself and `Fill::NonZero` would paint the overlap as a hole.
pub fn arrow(length: f64, thickness: f64) -> Vec<Point> {
    const TIP_LENGTH: f64 = 0.25;
    const MAX_TIP_TO_LENGTH: f64 = 0.25;

    let head = TIP_LENGTH.min(MAX_TIP_TO_LENGTH * length);
    let half_head = head / 2.0;
    let t = (thickness / 2.0).min(half_head);
    let base = length - head;

    bezier::points_as_corners(&[
        Point::new(0.0, -t),
        Point::new(base, -t),
        Point::new(base, -half_head),
        Point::new(length, 0.0),
        Point::new(base, half_head),
        Point::new(base, t),
        Point::new(0.0, t),
        Point::new(0.0, -t),
    ])
}

/// A number line: a horizontal filled shaft with ticks rising from it.
///
/// Built as **one welded path** rather than a shaft with tick children — see
/// [`axes`] for why — and centred on its own origin, `x` in
/// `[-length/2, length/2]`.
///
/// `range` is the *data* range the ticks mark; the mapping from data to world
/// is [`AxesScale`]'s, restricted to `x`. A non-finite or degenerate description
/// (a range that does not grow, a step that is not positive, a tick count the
/// [`MAX_TICKS`] guard refuses) draws nothing rather than allocating or
/// panicking: every number here can come from a language model.
pub fn number_line(
    length: f64,
    range: (f64, f64),
    step: f64,
    tick_size: f64,
    thickness: f64,
) -> Vec<Point> {
    if !(length.is_finite() && thickness.is_finite() && tick_size.is_finite())
        || length <= 0.0
        || thickness <= 0.0
        || tick_size < 0.0
    {
        return Vec::new();
    }
    let half_t = thickness / 2.0;
    let mut path = rustim_math::BezPath::new();
    path.extend(rustim_math::rect_path(rustim_math::Rect::new(
        -length / 2.0,
        -half_t,
        length / 2.0,
        half_t,
    )));
    for v in tick_values(range, step) {
        let x = AxesScale::map(v, range, length);
        // Clamped to the shaft: a tick at the very end protrudes half a
        // thickness past it otherwise, and that protrusion is what a
        // diameter-based description would then report as extra length.
        let (x0, x1) = clamp_span(x - half_t, x + half_t, length / 2.0);
        path.extend(rustim_math::rect_path(rustim_math::Rect::new(
            x0,
            half_t,
            x1,
            half_t + tick_size,
        )));
    }
    points_from_path(&path)
}

/// Two axes crossing at the origin, with ticks on both.
///
/// `x_length` and `y_length` are the shafts' geometric lengths; `x_range` and
/// `y_range` are the data ranges the ticks mark, mapped by [`AxesScale`]. Ticks
/// rise above the x-axis and to the right of the y-axis, and the tick at the
/// origin is omitted on both because the other axis already crosses there.
///
/// # One path, not a group of tick mobjects
///
/// Same trade as [`number_line`]: a subtree of ticks would make each tick a
/// separately addressable object, which sounds like a feature until `add`
/// returns one id for the axes and `Tab` visits twelve ticks, the parent's
/// opacity stops at its children, and [`crate::SceneTree::world_bounds`] answers
/// `None` for a parent that has only children. So the shafts and every tick are
/// subpaths of a single `BezPath`, welded by [`points_from_path`]. The cost is
/// that a tick is not addressable; the alternative costs the object model.
pub fn axes(
    x_length: f64,
    y_length: f64,
    x_range: (f64, f64),
    y_range: (f64, f64),
    step: f64,
    tick_size: f64,
    thickness: f64,
) -> Vec<Point> {
    if !(x_length.is_finite() && y_length.is_finite())
        || !(thickness.is_finite() && tick_size.is_finite())
        || x_length <= 0.0
        || y_length <= 0.0
        || thickness <= 0.0
        || tick_size < 0.0
    {
        return Vec::new();
    }
    let half_t = thickness / 2.0;
    let mut path = rustim_math::BezPath::new();
    // The x-axis, then the y-axis. Both wind the same way (`rect_path` is
    // counter-clockwise for every rectangle), so their overlap at the origin
    // counts twice under `Fill::NonZero` and still fills.
    path.extend(rustim_math::rect_path(rustim_math::Rect::new(
        -x_length / 2.0,
        -half_t,
        x_length / 2.0,
        half_t,
    )));
    path.extend(rustim_math::rect_path(rustim_math::Rect::new(
        -half_t,
        -y_length / 2.0,
        half_t,
        y_length / 2.0,
    )));
    for v in tick_values(x_range, step) {
        if v == 0.0 {
            continue;
        }
        let x = AxesScale::map(v, x_range, x_length);
        let (x0, x1) = clamp_span(x - half_t, x + half_t, x_length / 2.0);
        path.extend(rustim_math::rect_path(rustim_math::Rect::new(
            x0,
            half_t,
            x1,
            half_t + tick_size,
        )));
    }
    for v in tick_values(y_range, step) {
        if v == 0.0 {
            continue;
        }
        let y = AxesScale::map(v, y_range, y_length);
        let (y0, y1) = clamp_span(y - half_t, y + half_t, y_length / 2.0);
        path.extend(rustim_math::rect_path(rustim_math::Rect::new(
            half_t,
            y0,
            half_t + tick_size,
            y1,
        )));
    }
    points_from_path(&path)
}

/// A sampled curve, returned as its **centerline** for the backend's stroker.
///
/// # Not a filled polyline
///
/// Handing the samples to the fill walk as an open polyline would fill the
/// region between the curve and its closing chord — a function graph would be
/// drawn as a solid area rather than as a line. `build` therefore marks a
/// `Plot` as stroked (`fill_opacity = 0`, `stroke_opacity = 1`) and the draw
/// walk emits [`rustim_render::DrawOp::StrokePath`] instead. The stroke width
/// is `Style::stroke_width`, so it is a property a host can change and an
/// animation can interpolate — which the earlier ribbon spelling could not do:
/// there the width had been baked into the point array.
///
/// Consecutive duplicate samples are dropped — a host sampling at two different
/// parameters can produce them, and the stroker needs no zero-length segments.
/// Fewer than two distinct points draws nothing: there is no direction in one
/// point, and a lone sample is not a curve.
pub fn plot(points: &[[f64; 2]]) -> Vec<Point> {
    let mut kept: Vec<Point> = Vec::with_capacity(points.len());
    for &[x, y] in points {
        // A non-finite sample refuses the whole plot rather than being skipped:
        // skipping would move the curve's start to the origin, which is a wrong
        // picture instead of an absent one.
        if !(x.is_finite() && y.is_finite()) {
            return Vec::new();
        }
        let p = Point::new(x, y);
        // Consecutive duplicates would be zero-length segments; they are
        // dropped here so the stroke has no degenerate joins at all.
        if kept.last() == Some(&p) {
            continue;
        }
        kept.push(p);
    }
    if kept.len() < 2 {
        return Vec::new();
    }
    // The samples are the **centerline**. `build` marks a `Plot` as stroked, so
    // the draw walk emits `DrawOp::StrokePath` and the backend's stroker puts
    // the width on. Expanding to an outline here — what this function used to
    // do — was the interim spelling while the draw walk could only fill; it
    // hard-coded the width into geometry, made the width un-animatable, and
    // left the a11y measurement reading a ribbon rather than the curve.
    bezier::points_as_corners(&kept)
}

/// The most ticks one axis may draw.
///
/// A guard, not a taste: `step` and the ranges arrive over the wire, and
/// `step = 1e-9` over a range of 8 would otherwise build billions of rectangles
/// inside `add`. Exceeding it draws nothing, which is a refusal the caller can
/// see in the picture and act on.
pub const MAX_TICKS: usize = 256;

/// The tick values in `range` that are multiples of `step`, inclusive.
///
/// Multiplied rather than accumulated: `x += step` drifts, and a tick at 0.3
/// that lands at 0.30000000000000004 is a tick the `!= 0.0` skip misses and a
/// label disagrees with.
fn tick_values((lo, hi): (f64, f64), step: f64) -> Vec<f64> {
    if !(lo.is_finite() && hi.is_finite()) || lo >= hi || !step.is_finite() || step <= 0.0 {
        return Vec::new();
    }
    let first = (lo / step).ceil();
    let last = (hi / step).floor();
    if !(first.is_finite() && last.is_finite()) || last < first {
        return Vec::new();
    }
    // `as usize` saturates, so an enormous span is refused by the same check as
    // a merely large one.
    let count = (last - first + 1.0) as usize;
    if count == 0 || count > MAX_TICKS {
        return Vec::new();
    }
    (0..count).map(|i| (first + i as f64) * step).collect()
}

/// Clamp a tick's extent along its axis to `[-half, half]`.
///
/// A tick centred on the axis' very end would otherwise protrude half a
/// thickness past it. That is a couple of pixels in the picture and it is also
/// the ink a diameter-based description measures: with the protrusion, a
/// number line whose shaft is 6 is announced as "长度 6.05".
fn clamp_span(lo: f64, hi: f64, half: f64) -> (f64, f64) {
    (lo.max(-half), hi.min(half))
}

/// The mapping from data coordinates to world coordinates, shared by the axis
/// builders and the scenes that plot against them.
///
/// The world is Y-up with the origin at the frame's centre; a coordinate system
/// is therefore a rectangle of data mapped onto a rectangle centred on the
/// origin. Keeping that mapping in one place is what lets a scene sample a
/// function in data coordinates and hand [`ShapeKind::Plot`] the world-space
/// points it drew the axes for — there is no second, subtly different mapping
/// inside the tick placement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AxesScale {
    /// The data interval the x-axis spans.
    pub x_range: (f64, f64),
    /// The x-axis' geometric length, in world units.
    pub x_length: f64,
    pub y_range: (f64, f64),
    pub y_length: f64,
}

impl AxesScale {
    pub const fn new(
        x_range: (f64, f64),
        x_length: f64,
        y_range: (f64, f64),
        y_length: f64,
    ) -> Self {
        Self {
            x_range,
            x_length,
            y_range,
            y_length,
        }
    }

    /// Where the data point `(x, y)` lands in world space.
    pub fn to_world(&self, x: f64, y: f64) -> Point {
        Point::new(
            Self::map(x, self.x_range, self.x_length),
            Self::map(y, self.y_range, self.y_length),
        )
    }

    /// One axis of the map: centre the data interval on zero, scale it to
    /// `length`. A degenerate interval answers `0.0` — the midpoint, which is a
    /// finite point — rather than dividing by zero.
    fn map(v: f64, (lo, hi): (f64, f64), length: f64) -> f64 {
        let span = hi - lo;
        if !(span.is_finite() && span > 0.0) || !length.is_finite() {
            return 0.0;
        }
        (v - (lo + hi) / 2.0) * length / span
    }
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
        assert_eq!(p.thickness(), DEFAULT_THICKNESS);
    }

    /// A line is a two-anchor **centerline**, anchored at the tail.
    ///
    /// Asserted on the **bounds**, not on the point count alone, because the
    /// count is the same for a line of the right length in the wrong place —
    /// and "anchored at the tail" is the half of it a caller pivots on (the
    /// `rotate` scene turns an arrow about exactly this point), so `x0 == 0`
    /// is the assertion that matters rather than `x1 == length`.
    ///
    /// The zero height is the change: this used to be a `length × thickness`
    /// rectangle whose box was the picture. Now the width lives in `Style`, and
    /// the geometry is the segment itself.
    #[test]
    fn a_line_is_a_two_anchor_centerline() {
        let pts = line(3.0);
        assert_eq!(pts.len(), 4, "one segment, four control points");

        let anchors = bezier::anchors(&pts).unwrap();
        assert_eq!(
            anchors,
            vec![Point::new(0.0, 0.0), Point::new(3.0, 0.0)],
            "the tail is the anchor"
        );
        let b = bezier::bounds_of(&anchors).unwrap();
        assert_eq!((b.width(), b.height()), (3.0, 0.0));
    }

    /// An arrow is **one** contour — seven corners — with its point at
    /// `(length, 0)` and a head wider than its shaft.
    ///
    /// The single contour is the point. A shaft and a head as two subpaths would
    /// need a subpath break, and a flat Manim point array has no way to express
    /// one; because the head is wider than the shaft, their union is a single
    /// simple polygon and the question does not arise.
    #[test]
    fn an_arrow_is_one_contour_with_its_point_at_the_tip() {
        let (length, thickness) = (4.0, 0.1);
        let pts = arrow(length, thickness);
        assert_eq!(pts.len(), 7 * 4, "seven corners, one contour");

        let anchors = bezier::anchors(&pts).unwrap();
        assert_eq!(anchors.len(), 14);
        let b = bezier::bounds_of(&anchors).unwrap();
        assert_eq!((b.x0, b.x1), (0.0, length));
        assert!(
            anchors.contains(&Point::new(length, 0.0)),
            "the tip must be a corner, not a control point"
        );

        // Manim's `tip_length` default, and the head is as wide as it is long.
        assert_eq!(b.height(), 0.25);
        assert!(b.height() > thickness, "the head must overhang the shaft");
        // ...and the head starts exactly `tip_length` back from the tip.
        let base = length - 0.25;
        assert!(anchors.contains(&Point::new(base, -0.125)));
        assert!(anchors.contains(&Point::new(base, 0.125)));
    }

    /// A shaft fatter than the head it enters is clamped to it.
    ///
    /// Not a cosmetic guard: past that point the outline's base edge would run
    /// backwards through the head's own slopes, and `Fill::NonZero` would paint
    /// the self-overlap as a hole — an arrow with a bite out of it, which reads as
    /// a rasteriser bug rather than as a shape one.
    #[test]
    fn an_arrow_shaft_is_never_fatter_than_its_head() {
        let length = 4.0;
        let fat = arrow(length, 1.0);
        let b = bezier::bounds_of(&bezier::anchors(&fat).unwrap()).unwrap();

        assert_eq!(b.height(), 0.25, "the head's width caps the shaft's");
        assert!(
            b.height() < 1.0,
            "the requested thickness is not the height"
        );

        // The clamped shaft edge sits exactly on the head's outer corner, so no
        // pair of consecutive corners crosses.
        let half = 0.125;
        assert!(
            bezier::anchors(&fat)
                .unwrap()
                .contains(&Point::new(length - 0.25, -half))
        );
    }

    /// On a short arrow the head shrinks with the shaft instead of swallowing it:
    /// `tip_length` is capped at a quarter of the length, so the head's base never
    /// runs past the tail and the contour is never reversed.
    #[test]
    fn a_tiny_arrow_keeps_its_head_inside_its_length() {
        for length in [0.4, 0.1, 0.01] {
            let pts = arrow(length, 0.02);
            let b = bezier::bounds_of(&bezier::anchors(&pts).unwrap()).unwrap();
            assert_eq!(b.x1, length, "the tip is still the end");
            assert_eq!(
                b.height(),
                length / 4.0,
                "head at {length} long, at length {length}"
            );
            assert!(b.x0 == 0.0, "and the tail is still the start");

            // The head's base is inside the arrow, strictly, for every length > 0.
            let base = length - length / 4.0;
            assert!(base > 0.0 && base < length);
        }
    }

    /// A number line is one welded path: a whole number of curves, finite
    /// throughout, and closed back onto its hub so the array's implicit closing
    /// edge draws nothing.
    #[test]
    fn a_number_line_is_one_welded_closed_path() {
        let pts = number_line(6.0, (-3.0, 3.0), 1.0, 0.15, 0.045);
        assert_eq!(pts.len() % 4, 0, "every segment is a cubic");
        assert!(pts.iter().all(|p| p.x.is_finite() && p.y.is_finite()));
        assert_eq!(pts[pts.len() - 1], pts[0], "the loop returns to the hub");
        assert!(
            bezier::anchors(&pts).is_some(),
            "the welded array is a whole number of curves"
        );

        // Seven ticks and the shaft, each four straight segments. The end ticks
        // are clamped to the shaft, so the ink is exactly as long as the shaft.
        let b = bezier::bounds_of(&bezier::anchors(&pts).unwrap()).unwrap();
        assert_eq!((b.x0, b.x1), (-3.0, 3.0), "{b:?}");
        assert!((b.y1 - 0.1725).abs() < 1e-9, "{:?}", b);
    }

    /// The shaft is solid and the ticks are solid, and the space between two
    /// ticks is not: the welded loop has to keep all three true at once.
    ///
    /// Asserted as winding numbers because that is what `Fill::NonZero` reads.
    /// A weld that dropped the return-to-hub segment would leave the array's
    /// implicit closing edge running from the last tick back along the shaft,
    /// and the number line would come out half filled with a diagonal scar.
    #[test]
    fn the_ticks_do_not_punch_holes_in_the_shaft() {
        let pts = number_line(6.0, (-3.0, 3.0), 1.0, 0.15, 0.045);
        let poly = flatten(&pts);

        assert_eq!(winding(&poly, Point::new(0.5, 0.0)), 1, "inside the shaft");
        assert_eq!(winding(&poly, Point::new(1.0, 0.1)), 1, "inside a tick");
        assert_eq!(
            winding(&poly, Point::new(0.5, 0.1)),
            0,
            "the gap between ticks is not filled"
        );
        assert_eq!(winding(&poly, Point::new(4.0, 0.0)), 0, "outside");
    }

    /// A step of zero (or a negative or non-finite one) draws the shaft and no
    /// ticks, rather than dividing by zero or looping forever.
    ///
    /// These values arrive over the wire, so a refusal has to be a picture, not
    /// a hang. The same guard covers a step so small that the tick count would
    /// exceed [`MAX_TICKS`].
    #[test]
    fn a_zero_step_does_not_run_away() {
        for step in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e-12] {
            let pts = number_line(6.0, (-3.0, 3.0), step, 0.15, 0.045);
            // The shaft alone: four curves, sixteen points.
            assert_eq!(pts.len(), 4 * 4, "step {step}");
            let b = bezier::bounds_of(&bezier::anchors(&pts).unwrap()).unwrap();
            assert!((b.height() - 0.045).abs() < 1e-9, "step {step}: no ticks");
        }
    }

    /// Both axes and every tick in one path, and the filled result is the union:
    /// each shaft is solid, and the origin — where the two overlap — is still
    /// filled under `Fill::NonZero` (winding 2 is not a hole).
    #[test]
    fn an_axes_welds_both_axes_and_their_ticks() {
        let pts = axes(7.0, 4.0, (-4.0, 4.0), (-2.0, 2.0), 1.0, 0.15, 0.045);
        assert_eq!(pts.len() % 4, 0);
        let poly = flatten(&pts);

        assert_eq!(winding(&poly, Point::new(2.0, 0.0)), 1, "inside the x-axis");
        assert_eq!(winding(&poly, Point::new(0.0, 1.5)), 1, "inside the y-axis");
        assert!(
            winding(&poly, Point::new(0.0, 0.0)) >= 1,
            "the axes' overlap must not cancel: {}",
            winding(&poly, Point::new(0.0, 0.0))
        );
        // The tick at data x = 2 sits at world x = 1.75 — the ticks are placed by
        // the data→world map, not in world units.
        assert_eq!(
            winding(&poly, Point::new(1.75, 0.12)),
            1,
            "inside an x tick"
        );
        assert_eq!(winding(&poly, Point::new(0.12, 1.0)), 1, "inside a y tick");
        assert_eq!(
            winding(&poly, Point::new(2.0, 0.5)),
            0,
            "the quadrant is empty"
        );

        let b = bezier::bounds_of(&bezier::anchors(&pts).unwrap()).unwrap();
        // The end ticks are clamped to their shafts, so the ink is exactly the
        // shafts' rectangle.
        assert_eq!((b.width(), b.height()), (7.0, 4.0), "{b:?}");
    }

    /// A plot's geometry *is* its samples, not an outline around them.
    ///
    /// The counterfactual for the obvious implementation — handing the open
    /// polyline to the fill walk, which paints the area between the curve and
    /// its closing chord — is enforced at the op level, because that is where
    /// the defect lives: see `context::tests::a_plot_is_stroked_and_not_filled`.
    /// This test pins the other half: no expansion is baked into the points, so
    /// the measured size of a graph is the graph's extent and not that extent
    /// plus a pen width.
    #[test]
    fn a_plot_is_the_centerline_and_not_the_region_under_the_curve() {
        let pts = plot(&[[0.0, 0.0], [1.0, 1.0], [2.0, 0.0]]);
        // Two segments, four control points each, each restating the previous
        // endpoint as its own start — so the four anchors contain the middle
        // sample twice, exactly as Manim spells a polyline.
        assert_eq!(pts.len(), 8);
        let anchors = bezier::anchors(&pts).unwrap();
        assert_eq!(
            anchors,
            vec![
                Point::new(0.0, 0.0),
                Point::new(1.0, 1.0),
                Point::new(1.0, 1.0),
                Point::new(2.0, 0.0)
            ],
            "the samples *are* the geometry; nothing is expanded around them"
        );

        // The counterfactual the old ribbon test enforced geometrically is now
        // enforced by the op the draw walk emits — see
        // `context::tests::a_plot_is_stroked_and_not_filled`. Here the shape's
        // own claim is enough: its bounds are the samples' bounds, so no
        // region below the curve belongs to it.
        let b = bezier::bounds_of(&anchors).unwrap();
        assert_eq!(b.height(), 1.0);
        assert_eq!((b.x0, b.x1), (0.0, 2.0));
    }

    /// Fewer than two distinct samples is not a curve, and a non-finite sample
    /// refuses the whole plot rather than moving its start.
    #[test]
    fn a_plot_needs_two_distinct_points() {
        assert!(plot(&[]).is_empty());
        assert!(plot(&[[1.0, 1.0]]).is_empty());
        assert!(plot(&[[1.0, 1.0], [1.0, 1.0]]).is_empty());
        assert!(plot(&[[0.0, 0.0], [1.0, f64::NAN]]).is_empty());
        // Duplicated interior samples are dropped, not zero-length segments.
        assert!(!plot(&[[0.0, 0.0], [0.0, 0.0], [1.0, 1.0]]).is_empty());
    }

    /// The one mapping a scene samples through: the endpoints of the data range
    /// land on the ends of the axis, and the middle on the origin.
    #[test]
    fn the_data_to_world_mapping_maps_the_range_onto_the_length() {
        let scale = AxesScale::new((-4.0, 4.0), 7.0, (-2.0, 2.0), 4.0);
        assert_eq!(scale.to_world(-4.0, -2.0), Point::new(-3.5, -2.0));
        assert_eq!(scale.to_world(4.0, 2.0), Point::new(3.5, 2.0));
        assert_eq!(scale.to_world(0.0, 0.0), Point::new(0.0, 0.0));
        assert_eq!(scale.to_world(1.0, 1.0), Point::new(0.875, 1.0));

        // A degenerate interval answers the midpoint rather than dividing by
        // zero — the same refusal the builders make, in the mapping's own terms.
        let flat = AxesScale::new((1.0, 1.0), 7.0, (-2.0, 2.0), 4.0);
        assert_eq!(flat.to_world(3.0, 2.0), Point::new(0.0, 2.0));
    }

    /// A built line is a two-anchor centerline whose paint says "stroke me".
    ///
    /// The style assertions are the half a geometry test cannot make: a
    /// centerline drawn by the fill rule is invisible, so "the points are
    /// right" and "the picture is right" would disagree silently.
    #[test]
    fn a_line_is_a_centerline_with_the_stroke_switched_on() {
        let m = build(
            ShapeKind::Line,
            &ShapeParams {
                width: Some(3.0),
                thickness: Some(0.05),
                ..Default::default()
            },
        );
        assert_eq!(
            bezier::anchors(&m.points).unwrap(),
            vec![Point::new(0.0, 0.0), Point::new(3.0, 0.0)]
        );
        assert_eq!(m.style.fill_opacity, 0.0);
        assert_eq!(m.style.stroke_opacity, 1.0);
        assert_eq!(m.style.stroke_width, 0.05);
    }

    /// The kind decides which half of `Style` is on, in both directions.
    #[test]
    fn a_plot_is_stroked_and_a_square_is_filled() {
        let plot = build(
            ShapeKind::Plot,
            &ShapeParams {
                points: Some(vec![[0.0, 0.0], [1.0, 1.0]]),
                thickness: Some(0.08),
                ..Default::default()
            },
        );
        assert_eq!(
            (plot.style.fill_opacity, plot.style.stroke_opacity),
            (0.0, 1.0)
        );
        assert_eq!(plot.style.stroke_width, 0.08);

        let square = build(ShapeKind::Square, &ShapeParams::default());
        assert_eq!(
            (square.style.fill_opacity, square.style.stroke_opacity),
            (1.0, 0.0)
        );
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
