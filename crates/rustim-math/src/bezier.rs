//! Bézier and curve-path maths, ported from Manim's `manim.utils.bezier`.
//!
//! Everything here is checked case by case against Manim 0.22's own output by
//! `tests/manim_parity.rs`, reading the fixtures in `tests/fixtures/`. The port
//! is deliberately literal — including where Manim's behaviour surprises —
//! because the point of the module is that one scene description produces the
//! same figure on both sides.
//!
//! Four of those surprises are load-bearing enough to name up front.
//!
//! [`proportions_along_bezier_curve_for_point`] resolves the curve into the
//! power basis, finds each axis' roots as numpy does, **rounds them to six
//! decimals**, and only then intersects the axes. The rounding is not cosmetic:
//! the intersection is an exact float comparison, so without it two axes would
//! essentially never agree. And because the rounding follows numpy's sort and
//! precedes the filtering, a root whose imaginary part is below 5e-7 counts as
//! real — which is Manim's behaviour, reproduced on purpose.
//!
//! [`critical_point`] bounds the curves' *anchors* — each curve's start and end
//! point — not the control points and not the sampled curve. A cusped cubic
//! whose control points reach far outside its endpoints therefore has its
//! critical point on the segment between its endpoints.
//!
//! [`arc_length`] sums straight chords between ten sample points per curve, so it
//! is a coarse approximation wearing a name that sounds exact. See its docs.
//!
//! Several routines disagree with their own documentation, and the code wins.
//! `partial_bezier_points` short-circuits `a == 1` and `b == 0` before it looks at
//! the degree; `get_quadratic_approximation_of_cubic` computes its handle as
//! `0.25 * (3 * h + a)`, which is not the same `f64` as `0.75 * h + 0.25 * a`;
//! the two-anchor handle points are `(1 - alpha) * start + alpha * end`, again not
//! the same as `start + alpha * (end - start)`; and the closed spline's last
//! second handle uses `2 * A[N]` where the docstring says `2 * A[0]`. Each of
//! those is pinned by a fixture case, because each is a difference that stays
//! invisible until something else is compared against it.
//!
//! Points are 2D. Manim's are 3D with `z = 0.0` throughout a 2D scene, and every
//! routine here is linear in the control points, so `z` evolves independently of
//! `x` and `y` and dropping it loses nothing. The one place Manim consults `z` is
//! [`is_closed`], where a constant zero makes its third comparison trivially true;
//! that is noted there.
//!
//! One thing is deliberately *not* bit-faithful. Manim gets polynomial roots above
//! degree 1 from the eigenvalues of numpy's companion matrix; this solves them in
//! closed form. Both are accurate to far better than the 1e-6 grid the answers are
//! compared on, and numpy's eigenvalue *ordering* is a LAPACK artifact — but the
//! sort reproduced here (`real`, then `imaginary`) is the one numpy's `polyroots`
//! asks for explicitly, and the fixtures pin the results.

use crate::{Point, Rect};

/// Points per cubic curve in a Manim path — `n_points_per_cubic_curve`. Every path
/// in this workspace uses it, so a "path" is a flat `&[Point]` whose length is a
/// multiple of this, and a "curve" is four consecutive points.
pub const POINTS_PER_CURVE: usize = 4;

/// Manim's `round_to` default for [`proportions_along_bezier_curve_for_point`].
///
/// Public because it is a documented Manim parameter rather than an internal
/// detail: a caller comparing this port against Manim needs to know which grid
/// the roots were snapped to. Its coupling to [`ROUND_DECIMALS`] is asserted by
/// `tests::round_decimals_agrees_with_round_to`.
pub const ROUND_TO: f64 = 1e-6;

/// `int(np.log10(1 / round_to))` with the default [`ROUND_TO`], i.e. the decimal
/// place count the roots are rounded to. Spelled out because `log10` is not
/// available in a const context; the two have to move together, so the derivation
/// is asserted in this module's tests rather than left to a comment.
pub const ROUND_DECIMALS: i32 = 6;

/// Manim's default `sample_points_per_curve` for [`arc_length`].
pub const DEFAULT_ARC_SAMPLES: usize = 10;

/// The refusals these routines can produce.
///
/// Manim raises a Python exception for each of these conditions — a
/// `NotImplementedError` for a degree it cannot subdivide, a `ValueError` for
/// mismatched shapes, a `TypeError` out of `reduce` for a fully degenerate curve.
/// Carrying them as values keeps the same information without a panic, which
/// matters here because a language model can reach every one of these code paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurveError {
    /// A polynomial of degree above 3 would need a general root finder.
    UnsupportedDegree { degree: usize },
    /// Every coordinate function vanished: the curve is a single point, and the
    /// query point is that point.
    DegenerateCurve,
    /// The curves handed to [`bezier_remap`] do not all have the same length.
    RaggedPath,
    /// Handle-point smoothing needs at least three anchors to have a curve.
    TooFewAnchors { got: usize },
}

impl core::fmt::Display for CurveError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnsupportedDegree { degree } => write!(f, "no root finder for degree {degree}"),
            Self::DegenerateCurve => write!(f, "the curve is a single point"),
            Self::RaggedPath => write!(f, "curves of differing lengths"),
            Self::TooFewAnchors { got } => write!(f, "need at least 3 anchors, got {got}"),
        }
    }
}

impl core::error::Error for CurveError {}

// ---------------------------------------------------------------------------
// point arithmetic
// ---------------------------------------------------------------------------
//
// `kurbo`'s `Point` implements neither `Mul<f64>` nor `Add<Point>`, and
// `Point - Point` yields a `Vec2`, so the elementwise arithmetic the numpy
// originals are written in has to be spelled out. Each of these is one line, and
// its name says which numpy expression it stands for — that is what keeps the
// ports below readable next to the source.

/// `a + b`, elementwise.
fn add(a: Point, b: Point) -> Point {
    Point::new(a.x + b.x, a.y + b.y)
}

/// `a - b`, elementwise.
fn sub(a: Point, b: Point) -> Point {
    Point::new(a.x - b.x, a.y - b.y)
}

/// `a * s`, elementwise.
fn scale(a: Point, s: f64) -> Point {
    Point::new(a.x * s, a.y * s)
}

/// `a / s`, elementwise. Distinct from `scale(a, 1.0 / s)`, and the difference is
/// a fixture case: the closed spline's first reduced term is `AUX[0] / 3`.
fn div(a: Point, s: f64) -> Point {
    Point::new(a.x / s, a.y / s)
}

/// `a - b * s`.
fn sub_scaled(a: Point, b: Point, s: f64) -> Point {
    Point::new(a.x - b.x * s, a.y - b.y * s)
}

/// `(a - b) * s`.
fn diff_scaled(a: Point, b: Point, s: f64) -> Point {
    Point::new((a.x - b.x) * s, (a.y - b.y) * s)
}

/// `(1 - alpha) * start + alpha * end` — Manim's `interpolate`.
///
/// Not written as `start + alpha * (end - start)`: the two differ in the last
/// place, and one fixture case (`get_smooth_cubic_bezier_handle_points` on two
/// anchors) is exactly that difference.
fn interpolate(start: Point, end: Point, alpha: f64) -> Point {
    weighted_sum([(1.0 - alpha, start), (alpha, end)])
}

/// `Σ weightᵢ · pointᵢ`, accumulating in the order given.
///
/// Accumulating from `0.0` reproduces numpy's left-to-right sum exactly, which is
/// what keeps a row like `[1, 0, 0, 0]` bit-identical: adding `0.0` and then
/// multiplying by zero are both exact.
fn weighted_sum(terms: impl IntoIterator<Item = (f64, Point)>) -> Point {
    let mut x = 0.0;
    let mut y = 0.0;
    for (w, p) in terms {
        x += w * p.x;
        y += w * p.y;
    }
    Point::new(x, y)
}

/// `matrix @ points`, one dot product per row. This is how Manim applies its
/// portion, split and subdivision matrices.
fn apply_matrix(matrix: &[Vec<f64>], points: &[Point]) -> Vec<Point> {
    matrix
        .iter()
        .map(|row| weighted_sum(row.iter().copied().zip(points.iter().copied())))
        .collect()
}

/// The binomial coefficient, as `scipy.special.comb` returns it: an `f64`. Exact
/// at the degrees this module handles.
fn binomial(n: usize, k: usize) -> f64 {
    if k > n {
        return 0.0;
    }
    let k = k.min(n - k);
    let mut acc = 1.0;
    for i in 0..k {
        acc = acc * (n - i) as f64 / (i + 1) as f64;
    }
    acc
}

// ---------------------------------------------------------------------------
// evaluation
// ---------------------------------------------------------------------------

/// Evaluate a Bézier curve at `t`.
///
/// Manim's `bezier(points)` returns a closure that dispatches on the degree, and
/// its `nth_grade_bezier` sweeps de Casteljau above degree 3. This is that
/// dispatch with a plain parameter. Degrees 0–3 use the explicit Bernstein forms
/// rather than a sweep, so the two sides agree to the last bit instead of merely
/// to within rounding.
pub fn bezier(points: &[Point], t: f64) -> Point {
    match points.len() {
        0 => Point::new(0.0, 0.0),
        1 => points[0],
        2 => add(points[0], scale(sub(points[1], points[0]), t)),
        3 => {
            let (t2, mt) = (t * t, 1.0 - t);
            let mt2 = mt * mt;
            weighted_sum([(mt2, points[0]), (2.0 * t * mt, points[1]), (t2, points[2])])
        }
        4 => {
            let (t2, mt) = (t * t, 1.0 - t);
            let (t3, mt2) = (t2 * t, mt * mt);
            let mt3 = mt2 * mt;
            weighted_sum([
                (mt3, points[0]),
                (3.0 * t * mt2, points[1]),
                (3.0 * t2 * mt, points[2]),
                (t3, points[3]),
            ])
        }
        n => {
            // The general sweep. numpy updates the whole array per pass, so a pass
            // must not see its own output; iterating j upwards is safe anyway
            // (index j reads only j and j+1, and j is never read again), but the
            // snapshot makes that independent of the iteration order.
            let mut buf = points.to_vec();
            for i in 1..n {
                let prev = buf.clone();
                for j in 0..n - i {
                    buf[j] = add(buf[j], scale(sub(prev[j + 1], prev[j]), t));
                }
            }
            buf[0]
        }
    }
}

// ---------------------------------------------------------------------------
// splitting and extracting a portion
// ---------------------------------------------------------------------------

/// The portion of a Bézier curve between parameters `a` and `b`, returned as the
/// control points of the equivalent curve.
///
/// Two early returns come first, ahead of the degree dispatch: `a == 1` gives the
/// last point repeated, and `b == 0` the first point repeated. Both are Manim's,
/// both are hard to guess, and a port that only implements the portion matrices
/// gets them wrong.
///
/// Note also that `a > b` is not an error and does not reverse the curve — the
/// matrices simply produce the control points of a curve that wanders back — and
/// that `a == b` collapses the whole thing onto one point. Both are pinned by the
/// fixtures.
pub fn partial_bezier_points(points: &[Point], a: f64, b: f64) -> Vec<Point> {
    if a == 1.0 {
        return points
            .last()
            .map_or_else(Vec::new, |&p| vec![p; points.len()]);
    }
    if b == 0.0 {
        return points
            .first()
            .map_or_else(Vec::new, |&p| vec![p; points.len()]);
    }
    match points.len() {
        0 => Vec::new(),
        1 => points.to_vec(),
        2 => {
            let direction = sub(points[1], points[0]);
            vec![
                add(points[0], scale(direction, a)),
                add(points[0], scale(direction, b)),
            ]
        }
        3 => {
            let (ma, mb) = (1.0 - a, 1.0 - b);
            apply_matrix(
                &[
                    vec![ma * ma, 2.0 * a * ma, a * a],
                    vec![ma * mb, a * mb + ma * b, a * b],
                    vec![mb * mb, 2.0 * b * mb, b * b],
                ],
                points,
            )
        }
        4 => {
            let (ma, mb) = (1.0 - a, 1.0 - b);
            let (a2, b2) = (a * a, b * b);
            let (ma2, mb2) = (ma * ma, mb * mb);
            let (a3, b3) = (a2 * a, b2 * b);
            let (ma3, mb3) = (ma2 * ma, mb2 * mb);
            apply_matrix(
                &[
                    vec![ma3, 3.0 * ma2 * a, 3.0 * ma * a2, a3],
                    vec![
                        ma2 * mb,
                        2.0 * ma * a * mb + ma2 * b,
                        a2 * mb + 2.0 * ma * a * b,
                        a2 * b,
                    ],
                    vec![
                        ma * mb2,
                        a * mb2 + 2.0 * ma * mb * b,
                        2.0 * a * mb * b + ma * b2,
                        a * b2,
                    ],
                    vec![mb3, 3.0 * mb2 * b, 3.0 * mb * b2, b3],
                ],
                points,
            )
        }
        n => {
            let mut arr = points.to_vec();
            if a != 0.0 {
                for i in 1..n {
                    let prev = arr.clone();
                    for j in 0..n - i {
                        arr[j] = add(arr[j], scale(sub(prev[j + 1], prev[j]), a));
                    }
                }
            }
            if b != 1.0 {
                let mu = (1.0 - b) / (1.0 - a);
                // This pass reads `arr[j-1]`, which the *same* pass wrote at the
                // previous step, so the snapshot is required — unlike the pass
                // above, where reading left to right happens to coincide with
                // numpy's simultaneous update.
                for i in 1..n {
                    let prev = arr.clone();
                    for j in i..n {
                        arr[j] = add(arr[j], scale(sub(prev[j - 1], prev[j]), mu));
                    }
                }
            }
            arr
        }
    }
}

/// Split a Bézier curve at `t`, returning the first curve's control points
/// followed by the second's.
///
/// The two halves share the split point, which appears as the last point of the
/// first half and the first point of the second — that is Manim's layout, and
/// morph alignment depends on it.
pub fn split_bezier(points: &[Point], t: f64) -> Vec<Point> {
    match points.len() {
        0 => Vec::new(),
        1 => vec![points[0], points[0]],
        2 => {
            let middle = add(points[0], scale(sub(points[1], points[0]), t));
            vec![points[0], middle, middle, points[1]]
        }
        3 => {
            let (mt, t2) = (1.0 - t, t * t);
            let (mt2, two_tmt) = (mt * mt, 2.0 * t * mt);
            apply_matrix(
                &[
                    vec![1.0, 0.0, 0.0],
                    vec![mt, t, 0.0],
                    vec![mt2, two_tmt, t2],
                    vec![mt2, two_tmt, t2],
                    vec![0.0, mt, t],
                    vec![0.0, 0.0, 1.0],
                ],
                points,
            )
        }
        4 => {
            let (mt, t2) = (1.0 - t, t * t);
            let (mt2, t3) = (mt * mt, t2 * t);
            let (mt3, two_mt_t) = (mt2 * mt, 2.0 * mt * t);
            let three_mt2_t = 3.0 * mt2 * t;
            let three_mt_t2 = 3.0 * mt * t2;
            apply_matrix(
                &[
                    vec![1.0, 0.0, 0.0, 0.0],
                    vec![mt, t, 0.0, 0.0],
                    vec![mt2, two_mt_t, t2, 0.0],
                    vec![mt3, three_mt2_t, three_mt_t2, t3],
                    vec![mt3, three_mt2_t, three_mt_t2, t3],
                    vec![0.0, mt2, two_mt_t, t2],
                    vec![0.0, 0.0, mt, t],
                    vec![0.0, 0.0, 0.0, 1.0],
                ],
                points,
            )
        }
        n => {
            // Successive splitting, the fallback for an nth degree curve. `first`
            // collects the left control points, `second` the right ones, and the
            // output is `[first…, second…]`.
            let mut first = vec![Point::new(0.0, 0.0); n];
            let mut second = points.to_vec();
            first[0] = points[0];
            for (i, slot) in first.iter_mut().enumerate().take(n).skip(1) {
                for j in 0..n - i {
                    second[j] = add(second[j], scale(sub(second[j + 1], second[j]), t));
                }
                *slot = second[0];
            }
            first.extend(second);
            first
        }
    }
}

/// Re-parameterise a curve into `n_divisions` equal sub-curves, returning
/// `n_divisions * points.len()` control points.
///
/// Manim builds this as a single matrix product for curves of up to four control
/// points and by successive splitting above that. Both paths are kept, because
/// they do not agree bit for bit and the fixtures pin each separately.
pub fn subdivide_bezier(points: &[Point], n_divisions: usize) -> Vec<Point> {
    if n_divisions == 1 {
        return points.to_vec();
    }
    let n = points.len();
    // Manim builds a zero-row matrix here and multiplies, which yields an empty
    // result; its successive-splitting path would instead index the last of zero
    // rows. An empty answer avoids both the crash and the difference.
    if n_divisions == 0 || n == 0 {
        return Vec::new();
    }
    if n <= POINTS_PER_CURVE {
        return apply_matrix(&subdivision_matrix(n, n_divisions), points);
    }

    let mut beziers = vec![vec![Point::new(0.0, 0.0); n]; n_divisions];
    beziers[n_divisions - 1] = points.to_vec();
    for curve_num in (1..n_divisions).rev() {
        let a = (n_divisions - curve_num) as f64 / (n_divisions - curve_num + 1) as f64;
        // Two neighbouring slices of one buffer: `prev` receives the left control
        // points that `curr` discards as it sweeps.
        let (lo, hi) = beziers.split_at_mut(curve_num);
        let prev = &mut lo[curve_num - 1];
        let curr = &mut hi[0];
        prev[0] = curr[0];
        for (i, slot) in prev.iter_mut().enumerate().take(n).skip(1) {
            for j in 0..n - i {
                curr[j] = add(curr[j], scale(sub(curr[j + 1], curr[j]), a));
            }
            *slot = curr[0];
        }
    }
    beziers.concat()
}

/// Manim's `_get_subdivision_matrix`, for the curve sizes it supports.
///
/// The matrix is built and *then* divided by `n_divisions` raised to the power of
/// the degree — one division of the finished matrix, not of each row — so the
/// order of operations is reproduced. The single-point case is the odd one out: it
/// is all ones and is never divided.
fn subdivision_matrix(n_points: usize, n_divisions: usize) -> Vec<Vec<f64>> {
    let nd = n_divisions as f64;
    match n_points {
        // Zero-degree "curve": a single point, repeated.
        1 => vec![vec![1.0]; n_divisions],
        2 => {
            let rows = 2 * n_divisions;
            let mut m = vec![vec![0.0; 2]; rows];
            // Column 1 first; column 0 is a reversed copy of it, taken before the
            // division so both columns are divided exactly once.
            for k in 0..n_divisions {
                m[2 * k][1] = k as f64;
                m[2 * k + 1][1] = (k + 1) as f64;
            }
            for k in 0..rows {
                m[k][0] = m[rows - 1 - k][1];
            }
            for row in &mut m {
                row.iter_mut().for_each(|c| *c /= nd);
            }
            m
        }
        3 => {
            let mut m = vec![vec![0.0; 3]; 3 * n_divisions];
            for i in 0..n_divisions {
                let i_f = i as f64;
                let ip1 = (i + 1) as f64;
                let nmi = (n_divisions - i) as f64;
                let nmim1 = nmi - 1.0;
                m[3 * i] = vec![nmi * nmi, 2.0 * i_f * nmi, i_f * i_f];
                m[3 * i + 1] = vec![nmi * nmim1, i_f * nmim1 + ip1 * nmi, i_f * ip1];
                m[3 * i + 2] = vec![nmim1 * nmim1, 2.0 * ip1 * nmim1, ip1 * ip1];
            }
            let d = nd * nd;
            for row in &mut m {
                row.iter_mut().for_each(|c| *c /= d);
            }
            m
        }
        4 => {
            let mut m = vec![vec![0.0; 4]; 4 * n_divisions];
            for i in 0..n_divisions {
                let i_f = i as f64;
                let (i2, i3) = (i_f * i_f, i_f * i_f * i_f);
                let ip1 = (i + 1) as f64;
                let (ip12, ip13) = (ip1 * ip1, ip1 * ip1 * ip1);
                let nmi = (n_divisions - i) as f64;
                let (nmi2, nmi3) = (nmi * nmi, nmi * nmi * nmi);
                let nmim1 = nmi - 1.0;
                let (nmim12, nmim13) = (nmim1 * nmim1, nmim1 * nmim1 * nmim1);
                m[4 * i] = vec![nmi3, 3.0 * nmi2 * i_f, 3.0 * nmi * i2, i3];
                m[4 * i + 1] = vec![
                    nmi2 * nmim1,
                    2.0 * nmi * nmim1 * i_f + nmi2 * ip1,
                    nmim1 * i2 + 2.0 * nmi * i_f * ip1,
                    i2 * ip1,
                ];
                m[4 * i + 2] = vec![
                    nmi * nmim12,
                    nmim12 * i_f + 2.0 * nmi * nmim1 * ip1,
                    2.0 * nmim1 * i_f * ip1 + nmi * ip12,
                    i_f * ip12,
                ];
                m[4 * i + 3] = vec![nmim13, 3.0 * nmim12 * ip1, 3.0 * nmim1 * ip12, ip13];
            }
            let d = nd * nd * nd;
            for row in &mut m {
                row.iter_mut().for_each(|c| *c /= d);
            }
            m
        }
        // Unreachable: `subdivide_bezier` only gets here with 1..=POINTS_PER_CURVE.
        _ => Vec::new(),
    }
}

/// Change how many curves a path is described by, resampling to fit.
///
/// This is the primitive Manim's `align_points` and morph machinery is built on:
/// two paths morph into each other only once they have the same number of curves.
/// Each input curve is split into as many pieces as it needs, and the pieces are
/// emitted in order.
pub fn bezier_remap(
    curves: &[Vec<Point>],
    new_number_of_curves: usize,
) -> Result<Vec<Vec<Point>>, CurveError> {
    let current = curves.len();
    if current == 0 || new_number_of_curves == 0 {
        return Ok(Vec::new());
    }
    let nppc = curves[0].len();
    if curves.iter().any(|c| c.len() != nppc) {
        return Err(CurveError::RaggedPath);
    }

    // `repeat_indices[i] = (i * current) / new` names the input curve the i-th
    // output curve comes from; counting them gives each input curve its split
    // factor. With `new < current` some curves necessarily get zero, and Manim
    // still calls `subdivide_bezier(curve, 0)` on those — which builds a zero-row
    // matrix and contributes nothing. Skipping the call is the same result and
    // steps around the crash its other path would take.
    let mut split_factors = vec![0usize; current];
    for i in 0..new_number_of_curves {
        split_factors[(i * current) / new_number_of_curves] += 1;
    }

    let mut out = Vec::with_capacity(new_number_of_curves);
    for (curve, &sf) in curves.iter().zip(split_factors.iter()) {
        if sf == 0 {
            continue;
        }
        let flat = subdivide_bezier(curve, sf);
        for k in 0..sf {
            out.push(flat[k * nppc..(k + 1) * nppc].to_vec());
        }
    }
    Ok(out)
}

/// Whether a path's last point coincides with its first, within Manim's tolerance.
///
/// Manim compares all three coordinates against `atol + rtol * |start|`. In this 2D
/// port `z` is identically zero on both sides, so that comparison is always
/// satisfied and only `x` and `y` can decide the answer.
pub fn is_closed(points: &[Point]) -> bool {
    let Some((&start, &end)) = points.first().zip(points.last()) else {
        return false;
    };
    const RTOL: f64 = 1e-5;
    const ATOL: f64 = 1e-8;
    for (s, e) in [(start.x, end.x), (start.y, end.y)] {
        if (e - s).abs() > ATOL + RTOL * s.abs() {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// queries
// ---------------------------------------------------------------------------

/// The parameters at which a Bézier curve passes through `point`.
///
/// Manim expands the curve into the power basis, one polynomial per axis, finds
/// each polynomial's roots, rounds them to six decimals, keeps the real ones, and
/// returns the values common to every axis that is not constant. The rounding
/// happens *before* the intersection because the intersection is an exact float
/// comparison; it also means a root with an imaginary part below 5e-7 is treated
/// as real, which is Manim's behaviour and is reproduced.
///
/// The result is sorted ascending, as numpy's root list is, and filtered to
/// `0.0..=1.0`. It is **not** deduplicated when a single axis decides the answer:
/// numpy only deduplicates in `intersect1d`, which never runs with one axis, so a
/// repeated root comes back repeated. `folded@vertex` in the fixtures is exactly
/// that case.
///
/// An axis is skipped when it is constant along the curve *and* the query point
/// shares that constant, in which case it says nothing about where the point is.
/// If every axis is skipped the curve is a single point, which is the `TypeError`
/// Manim raises there.
///
/// Degrees above 3 are refused: numpy reaches those roots through companion matrix
/// eigenvalues, and a closed-form solver is not the same function.
pub fn proportions_along_bezier_curve_for_point(
    point: Point,
    control_points: &[Point],
) -> Result<Vec<f64>, CurveError> {
    let Some(n) = control_points.len().checked_sub(1) else {
        return Err(CurveError::TooFewAnchors { got: 0 });
    };
    if n > 3 {
        return Err(CurveError::UnsupportedDegree { degree: n });
    }

    let mut per_axis: Vec<Vec<f64>> = Vec::new();
    for axis in 0..2 {
        let coord = if axis == 0 { point.x } else { point.y };
        let coords: Vec<f64> = control_points
            .iter()
            .map(|p| if axis == 0 { p.x } else { p.y })
            .collect();

        // The Bernstein-to-power-basis expansion, term_power counting down: the
        // alternating binomial sum in `sum` is the coefficient of t^n, then of
        // t^(n-1), and so on. Coefficients are collected highest-degree first as
        // Manim does, and only the constant term carries the query point.
        let mut terms = Vec::with_capacity(n + 1);
        for term_power in (0..=n).rev() {
            let outer = binomial(n, term_power);
            let mut sum = 0.0;
            let mut sign = 1.0;
            for sub in (0..=term_power).rev() {
                let inner = binomial(term_power, sub) * sign;
                let mut subterm = inner * coords[sub];
                if term_power == 0 {
                    subterm -= coord;
                }
                sum += subterm;
                sign = -sign;
            }
            terms.push(outer * sum);
        }

        if terms.iter().all(|&t| t == 0.0) {
            continue;
        }

        let coeffs: Vec<f64> = terms.iter().rev().copied().collect();
        per_axis.push(poly_roots_rounded(&coeffs, ROUND_DECIMALS)?);
    }

    if per_axis.is_empty() {
        return Err(CurveError::DegenerateCurve);
    }

    let mut common = per_axis.remove(0);
    for roots in per_axis {
        common = intersect(&common, &roots);
    }
    Ok(common
        .into_iter()
        .filter(|&r| (0.0..=1.0).contains(&r))
        .collect())
}

/// Whether `point` lies on the Bézier curve through `control_points`.
pub fn point_lies_on_bezier(point: Point, control_points: &[Point]) -> Result<bool, CurveError> {
    Ok(!proportions_along_bezier_curve_for_point(point, control_points)?.is_empty())
}

/// Roots of a polynomial given low-order coefficient first, sorted, rounded to
/// `decimals` places, with the non-real ones dropped.
///
/// This mirrors numpy's `polyroots` — which trims high-order zero coefficients,
/// solves degree 1 directly, and takes everything above it from the companion
/// matrix's eigenvalues, sorting them as complex numbers — followed by Manim's
/// `np.around` and its `imag == 0` filter. The sort is what makes the output
/// ascending and is why duplicates survive; the rounding is what decides which
/// roots count as real.
///
/// Degrees 2 and 3 are solved in closed form rather than by eigenvalues. Both forms
/// are accurate to far better than the 1e-6 grid the answers are compared on, which
/// is the grid the caller actually uses.
fn poly_roots_rounded(coeffs: &[f64], decimals: i32) -> Result<Vec<f64>, CurveError> {
    // numpy's `as_series`: a trailing (highest-order) zero is not a coefficient.
    let Some(deg) = coeffs.iter().rposition(|&c| c != 0.0) else {
        return Ok(Vec::new());
    };
    let c = &coeffs[..=deg];

    let mut roots: Vec<(f64, f64)> = match c.len() {
        1 => Vec::new(),
        2 => vec![(-c[0] / c[1], 0.0)],
        3 => {
            // c0 + c1 x + c2 x^2
            let (c0, c1, c2) = (c[0], c[1], c[2]);
            let disc = c1 * c1 - 4.0 * c2 * c0;
            if disc >= 0.0 {
                let s = disc.sqrt();
                vec![((-c1 + s) / (2.0 * c2), 0.0), ((-c1 - s) / (2.0 * c2), 0.0)]
            } else {
                let s = (-disc).sqrt();
                vec![
                    (-c1 / (2.0 * c2), s / (2.0 * c2)),
                    (-c1 / (2.0 * c2), -s / (2.0 * c2)),
                ]
            }
        }
        4 => {
            // c0 + c1 x + c2 x^2 + c3 x^3. Monic first, then the depressed cubic
            // y^3 + p y + q with y = x + a/3.
            let (c0, c1, c2, c3) = (c[0], c[1], c[2], c[3]);
            let a = c2 / c3;
            let b = c1 / c3;
            let cc = c0 / c3;
            let p = b - a * a / 3.0;
            let q = 2.0 * a * a * a / 27.0 - a * b / 3.0 + cc;
            let disc = (q / 2.0) * (q / 2.0) + (p / 3.0) * (p / 3.0) * (p / 3.0);
            let third = a / 3.0;
            let mut out = Vec::with_capacity(3);
            if disc > 0.0 {
                let s = disc.sqrt();
                let u = (-q / 2.0 + s).cbrt();
                let v = (-q / 2.0 - s).cbrt();
                let y = u + v;
                out.push((y - third, 0.0));
                // The other two are a conjugate pair; they are carried so the
                // caller's rounding decides whether they count as real.
                let im = (3.0_f64).sqrt() / 2.0 * (u - v);
                out.push((-y / 2.0 - third, im));
                out.push((-y / 2.0 - third, -im));
            } else {
                // Three real roots, or a repeated one where disc lands exactly on
                // zero — in which case two of the three cosines coincide, which is
                // what keeps the repeated root repeated after rounding.
                let m = 2.0 * (-p / 3.0).sqrt();
                let phi = (3.0 * q / (p * m)).clamp(-1.0, 1.0).acos() / 3.0;
                for k in 0..3 {
                    let y = m * (phi - 2.0 * core::f64::consts::PI * k as f64 / 3.0).cos();
                    out.push((y - third, 0.0));
                }
            }
            out
        }
        _ => {
            return Err(CurveError::UnsupportedDegree {
                degree: c.len() - 1,
            });
        }
    };

    roots.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));

    let grid = 10f64.powi(decimals);
    Ok(roots
        .into_iter()
        .filter_map(|(re, im)| {
            let im = (im * grid).round_ties_even() / grid;
            (im == 0.0).then(|| (re * grid).round_ties_even() / grid)
        })
        .collect())
}

/// Set intersection by exact equality, as numpy's `intersect1d` — which sorts its
/// result and deduplicates it.
fn intersect(a: &[f64], b: &[f64]) -> Vec<f64> {
    let mut out: Vec<f64> = a.iter().copied().filter(|x| b.contains(x)).collect();
    out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
    out.dedup();
    out
}

// ---------------------------------------------------------------------------
// smooth handle points
// ---------------------------------------------------------------------------

/// Handle points that make a cubic spline through `anchors` smooth, as a pair of
/// one-handle-per-curve lists.
///
/// Dispatches on the anchor count and on whether the anchors form a closed loop,
/// exactly as Manim does. One anchor has no curve and yields two empty lists; two
/// anchors are a straight line whose handles sit at `alpha = 1/3` and `2/3`,
/// computed by [`interpolate`] rather than as an offset along the segment, which is
/// a different `f64`.
pub fn get_smooth_cubic_bezier_handle_points(
    anchors: &[Point],
) -> Result<(Vec<Point>, Vec<Point>), CurveError> {
    match anchors.len() {
        0 => Err(CurveError::TooFewAnchors { got: 0 }),
        1 => Ok((Vec::new(), Vec::new())),
        2 => Ok((
            vec![interpolate(anchors[0], anchors[1], 1.0 / 3.0)],
            vec![interpolate(anchors[0], anchors[1], 2.0 / 3.0)],
        )),
        _ if is_closed(anchors) => get_smooth_closed_cubic_bezier_handle_points(anchors),
        _ => get_smooth_open_cubic_bezier_handle_points(anchors),
    }
}

/// Handle points for a closed cubic spline through `anchors`.
///
/// Solves the cyclic tridiagonal system that makes the curve smooth at every
/// anchor, by forward and backward substitution. Needs at least three anchors: with
/// two there is no interior curvature to match. `anchors[0]` and `anchors[last]`
/// are expected to coincide, which is what [`is_closed`] tests — the caller
/// reaching here through [`get_smooth_cubic_bezier_handle_points`] has already
/// established that.
///
/// Manim memoises the two recurrences in module globals (`CP_CLOSED_MEMO`,
/// `UP_CLOSED_MEMO`) and seeds them with a single element, `1/3`, which is why its
/// loops start at index 1. The recurrence is deterministic and recomputing it gives
/// the same values, so this holds no global state; that seeded element is where the
/// `1/3` below comes from.
pub fn get_smooth_closed_cubic_bezier_handle_points(
    anchors: &[Point],
) -> Result<(Vec<Point>, Vec<Point>), CurveError> {
    let n_anchors = anchors.len();
    if n_anchors < 3 {
        return Err(CurveError::TooFewAnchors { got: n_anchors });
    }
    let n = n_anchors - 1;
    let a = anchors;

    // c' and u', by forward substitution from the seeded first element.
    let mut cp = vec![0.0; n - 1];
    let mut up = vec![0.0; n - 1];
    cp[0] = 1.0 / 3.0;
    up[0] = 1.0 / 3.0;
    for i in 1..n - 1 {
        cp[i] = 1.0 / (4.0 - cp[i - 1]);
        up[i] = -cp[i] * up[i - 1];
    }

    let cp_last_division = 1.0 / (3.0 - cp[n - 2]);
    let up_last = cp_last_division * (1.0 - up[n - 2]);

    // q, by backward substitution.
    let mut q = vec![0.0; n];
    q[n - 1] = up_last;
    for i in (0..n - 1).rev() {
        q[i] = up[i] - cp[i] * q[i + 1];
    }

    // D' = 4A_i + 2A_{i+1} for every i, with only the first and last divided by a
    // different scalar.
    let aux: Vec<Point> = (0..n)
        .map(|i| weighted_sum([(4.0, a[i]), (2.0, a[i + 1])]))
        .collect();
    let mut y = vec![Point::new(0.0, 0.0); n];
    y[0] = div(aux[0], 3.0);
    for i in 1..n - 1 {
        y[i] = diff_scaled(aux[i], y[i - 1], cp[i]);
    }
    y[n - 1] = diff_scaled(aux[n - 1], y[n - 2], cp_last_division);
    // Backward substitution, in place: Manim aliases Y to Dp, so each step reads the
    // value the previous step wrote and Y[i+1] is already final.
    for i in (0..n - 1).rev() {
        y[i] = sub_scaled(y[i], y[i + 1], cp[i]);
    }

    // The Sherman-Morrison correction that folds the corner ones back in.
    let scale_q = 1.0 / (1.0 + q[0] + q[n - 1]);
    let tail = add(y[0], y[n - 1]);
    let h1: Vec<Point> = (0..n)
        .map(|i| sub(y[i], scale(tail, scale_q * q[i])))
        .collect();

    let mut h2 = vec![Point::new(0.0, 0.0); n];
    for i in 0..n - 1 {
        h2[i] = weighted_sum([(2.0, a[i + 1]), (-1.0, h1[i + 1])]);
    }
    // `2 * A[N]`, not `2 * A[0]`, even though a closed path makes them the same
    // point — they are not the same two `f64`s.
    h2[n - 1] = weighted_sum([(2.0, a[n]), (-1.0, h1[0])]);

    Ok((h1, h2))
}

/// Handle points for an open cubic spline through `anchors`.
///
/// The open variant of [`get_smooth_closed_cubic_bezier_handle_points`], with free
/// rather than matched ends. Needs at least three anchors; Manim's own first
/// coefficient here is `1/2` rather than the closed case's `1/3`.
pub fn get_smooth_open_cubic_bezier_handle_points(
    anchors: &[Point],
) -> Result<(Vec<Point>, Vec<Point>), CurveError> {
    let n_anchors = anchors.len();
    if n_anchors < 3 {
        return Err(CurveError::TooFewAnchors { got: n_anchors });
    }
    let n = n_anchors - 1;
    let a = anchors;

    let mut cp = vec![0.0; n - 1];
    cp[0] = 0.5;
    for i in 1..n - 1 {
        cp[i] = 1.0 / (4.0 - cp[i - 1]);
    }

    // D', the same recurrence with free ends.
    let mut h1 = vec![Point::new(0.0, 0.0); n];
    h1[0] = weighted_sum([(0.5, a[0]), (1.0, a[1])]);
    let aux: Vec<Point> = (0..n - 2)
        .map(|i| weighted_sum([(4.0, a[i + 1]), (2.0, a[i + 2])]))
        .collect();
    for i in 1..n - 1 {
        h1[i] = diff_scaled(aux[i - 1], h1[i - 1], cp[i]);
    }
    let last_division = 1.0 / (7.0 - 2.0 * cp[n - 2]);
    h1[n - 1] = scale(
        weighted_sum([(8.0, a[n - 1]), (1.0, a[n]), (-2.0, h1[n - 2])]),
        last_division,
    );
    // Backward substitution, in place — see the closed variant.
    for i in (0..n - 1).rev() {
        h1[i] = sub_scaled(h1[i], h1[i + 1], cp[i]);
    }

    let mut h2 = vec![Point::new(0.0, 0.0); n];
    for i in 0..n - 1 {
        h2[i] = weighted_sum([(2.0, a[i + 1]), (-1.0, h1[i + 1])]);
    }
    // The sum first, then the halving: `0.5 * (A[N] + H1[N-1])`.
    h2[n - 1] = scale(add(a[n], h1[n - 1]), 0.5);

    Ok((h1, h2))
}

/// A cubic's quadratic approximation, as the six control points of two quadratics:
/// `a0, m0, k, k, m1, a1`.
///
/// The middle control point `k` is the midpoint of the two quadratics' control
/// points, which is what makes the pair meet smoothly. Manim returns this as a flat
/// array; the split into two quadratics is the caller's business.
///
/// The handles are `0.25 * (3h + a)`, computed as that sum and then one multiply —
/// not as `0.75 * h + 0.25 * a`, which differs in the last place.
pub fn get_quadratic_approximation_of_cubic(
    a0: Point,
    h0: Point,
    h1: Point,
    a1: Point,
) -> Vec<Point> {
    let m0 = scale(weighted_sum([(3.0, h0), (1.0, a0)]), 0.25);
    let m1 = scale(weighted_sum([(3.0, h1), (1.0, a1)]), 0.25);
    let k = scale(add(m0, m1), 0.5);
    vec![a0, m0, k, k, m1, a1]
}

// ---------------------------------------------------------------------------
// paths
// ---------------------------------------------------------------------------

/// Manim's `VMobject.set_points_as_corners`: join a list of corners with straight
/// cubics, one per consecutive pair of corners.
///
/// The interior control points are the linear interpolations at `1/3` and `2/3`,
/// **not** the degenerate `[A, A, A, B]` spelling of the same straight line. Both
/// draw the same segment, which is what makes this easy to get wrong without
/// anything visible changing: they parameterise it differently. Manim's form is
/// uniform in `t` (a point at `t` is that fraction of the way along), while the
/// degenerate form is not (`A + t³·(B - A)`). That difference is observable to
/// [`proportions_along_bezier_curve_for_point`], to [`arc_length`] sampling when
/// the sample count is low, and to morph alignment in `align_points`, which
/// interpolates two point arrays elementwise and would therefore take a different
/// path between the same two shapes.
///
/// Straight-cubic segments are what every polygon in this engine is made of, so
/// this is the routine a shape builder should use rather than hand-rolling the
/// four points. It is also how the `corner_paths` half of the geometry fixture was
/// built, so the paths in `tests/fixtures/manim-geometry.json` and the paths this
/// produces are the same construction by definition.
///
/// A single corner produces an empty path, not a point: there is no pair to join.
pub fn points_as_corners(corners: &[Point]) -> Vec<Point> {
    let mut out = Vec::with_capacity(corners.len().saturating_sub(1) * POINTS_PER_CURVE);
    for w in corners.windows(2) {
        for t in CORNER_T_VALUES {
            out.push(interpolate(w[0], w[1], t));
        }
    }
    out
}

/// `np.linspace(0, 1, POINTS_PER_CURVE)` — the parameters
/// [`points_as_corners`] interpolates at.
///
/// The middle entry is written `2 * (1/3)`'s own value rather than `2.0 / 3.0`
/// because that is how `linspace` reaches it (`step * i`, with
/// `step = 1 / (n - 1)`), and the two are the same `f64` here only because
/// `1 / 3 * 2` happens to round the same way. `tests::linspace_middle_matches_two_thirds`
/// asserts that rather than leaving it to inspection.
const CORNER_T_VALUES: [f64; POINTS_PER_CURVE] = [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0];

/// A path's anchors: every curve's start and end point, interleaved.
///
/// This is `VMobject.get_anchors`. It is not the same as the control points and not
/// the same as the sampled curve — interior control points are excluded and shared
/// endpoints appear twice — which is what makes it worth its own name and its own
/// note here.
///
/// `None` when the path is malformed: Manim zips its start and end anchors strictly,
/// so a length that is neither one nor a multiple of [`POINTS_PER_CURVE`] raises
/// there rather than returning something.
pub fn anchors(points: &[Point]) -> Option<Vec<Point>> {
    if points.len() == 1 {
        return Some(points.to_vec());
    }
    if !points.len().is_multiple_of(POINTS_PER_CURVE) {
        return None;
    }
    let starts = points.iter().step_by(POINTS_PER_CURVE);
    let ends = points
        .iter()
        .skip(POINTS_PER_CURVE - 1)
        .step_by(POINTS_PER_CURVE);
    Some(starts.zip(ends).flat_map(|(&s, &e)| [s, e]).collect())
}

/// The axis-aligned box around a set of anchors, or `None` if there are none.
///
/// The whole-box counterpart of [`critical_point`], over the same input and for
/// the same reason: it is the anchors that a path's extent is *about*. Public
/// because two consumers in different crates need the identical definition —
/// `SceneTree::world_bounds` and the accessibility layer's size measurements —
/// and two hand-rolled min/max loops are two chances to disagree about the empty
/// case.
pub fn bounds_of(anchors: &[Point]) -> Option<Rect> {
    let first = anchors.first()?;
    let (mut x0, mut x1, mut y0, mut y1) = (first.x, first.x, first.y, first.y);
    for p in &anchors[1..] {
        x0 = x0.min(p.x);
        x1 = x1.max(p.x);
        y0 = y0.min(p.y);
        y1 = y1.max(p.y);
    }
    Some(Rect::new(x0, y0, x1, y1))
}

/// The distinct corners of a closed polyline, in order.
///
/// [`anchors`] reports every curve's start *and* end, so a closed polygon's
/// shared corners each appear twice; and the shape's own closing corner repeats
/// the first one. Both repeats are exact — the same stored point, transformed by
/// the same affine — so this compares for equality rather than by distance, and
/// no tolerance can make it drop a corner of a genuinely thin shape.
///
/// A shape whose path is a closed loop of straight segments comes back with one
/// point per corner, which is what makes the *edge lengths* of a rotated figure
/// available: a rotated square's box is larger than the square (its side times
/// `cos + sin`), while the distance between its corners is exactly its side.
/// Returns at most one point for a path that is not a polyline, which is the
/// signal to fall back to the box.
pub fn corners(anchors: &[Point]) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::with_capacity(anchors.len());
    for &p in anchors {
        // `Point` is kurbo's, which is not `PartialEq`-blind to NaN: a NaN
        // coordinate would make every comparison false and keep every repeat,
        // which is the safe direction — it falls back to the box rather than
        // silently merging two distinct corners.
        if out.last() != Some(&p) {
            out.push(p);
        }
    }
    if out.len() > 1 && out.first() == out.last() {
        out.pop();
    }
    out
}

/// The extreme point, in the direction asked for, of the bounding box around a set
/// of anchors.
///
/// This is `Mobject.get_critical_point`, which Manim houses on the mobject and which
/// morph alignment reads. Each axis is decided independently: a direction component
/// below zero takes that axis' minimum, above zero its maximum, and exactly zero the
/// midpoint of the axis' extent.
///
/// Pass [`anchors`] of the path, not the path: see the note there.
pub fn critical_point(anchors: &[Point], direction: (f64, f64)) -> Point {
    Point::new(
        extremum_along_dim(anchors, 0, direction.0),
        extremum_along_dim(anchors, 1, direction.1),
    )
}

fn extremum_along_dim(points: &[Point], dim: usize, key: f64) -> f64 {
    let coord = |p: &Point| if dim == 0 { p.x } else { p.y };
    let min = points.iter().map(coord).fold(f64::INFINITY, f64::min);
    let max = points.iter().map(coord).fold(f64::NEG_INFINITY, f64::max);
    if min > max {
        // No points at all. Manim would index an empty array; the origin is the
        // harmless reading of "no extent" and keeps the function total.
        return 0.0;
    }
    if key < 0.0 {
        min
    } else if key == 0.0 {
        (min + max) / 2.0
    } else {
        max
    }
}

/// The length of a path, measured as Manim measures it.
///
/// Each curve is sampled at `samples_per_curve` equally spaced parameters and the
/// lengths of the straight chords between consecutive samples are summed. Manim's
/// default is [`DEFAULT_ARC_SAMPLES`] — **10 sample points**, which is 9 chords, not
/// ten — and that coarseness is visible: on a cubic arch of true length 2.0 it reads
/// 1.9903, and at 2 samples it reads the straight-line distance between the
/// endpoints. An analytic arc length would be a different number, so this is a chord
/// sum on purpose.
///
/// A trailing partial curve is ignored, as Manim's integer-division curve count
/// ignores it.
pub fn arc_length(points: &[Point], samples_per_curve: usize) -> f64 {
    let num_curves = points.len() / POINTS_PER_CURVE;
    let ts = linspace01(samples_per_curve);
    let mut total = 0.0;
    for n in 0..num_curves {
        let curve = &points[n * POINTS_PER_CURVE..(n + 1) * POINTS_PER_CURVE];
        let sampled: Vec<Point> = ts.iter().map(|&t| bezier(curve, t)).collect();
        for w in sampled.windows(2) {
            total += ((w[1].x - w[0].x).powi(2) + (w[1].y - w[0].y).powi(2)).sqrt();
        }
    }
    total
}

/// numpy's `linspace(0, 1, n)`: the endpoint is exact rather than accumulated.
fn linspace01(n: usize) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![0.0];
    }
    let step = 1.0 / (n - 1) as f64;
    let mut v: Vec<f64> = (0..n).map(|i| step * i as f64).collect();
    v[n - 1] = 1.0;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`ROUND_DECIMALS`] is `int(np.log10(1 / round_to))` with Manim's default
    /// `round_to`, and the two have to move together. `log10` is not available in
    /// a const context, so the coupling is asserted here instead of derived — and
    /// asserted with a cast, not a `round()`, because Manim truncates. It lands on
    /// `6` only because `1 / 1e-6` is exactly `1000000.0` and its `log10` is
    /// exactly `6.0`, which is exactly the kind of fact that stops being true
    /// quietly.
    #[test]
    fn round_decimals_agrees_with_round_to() {
        assert_eq!((1.0 / ROUND_TO).log10() as i32, ROUND_DECIMALS);
    }

    /// numpy's `np.around(x, 6)` is `rint(x * 1e6) / 1e6`, and `rint` resolves an
    /// exact tie to the *even* digit; Rust's `f64::round` resolves it away from
    /// zero. `0.1234565` splits the two, so the rounding in [`poly_roots_rounded`]
    /// has to be `round_ties_even`.
    #[test]
    fn rounding_is_ties_to_even_not_ties_away() {
        let scaled = 0.1234565f64 * 1e6;
        assert_ne!(scaled.round(), scaled.round_ties_even());
        assert_eq!(scaled.round_ties_even() / 1e6, 0.123456);
    }

    /// `np.linspace(0, 1, 4)`'s third entry. Written `2.0 / 3.0` in
    /// [`CORNER_T_VALUES`], and it is the same `f64` only by a rounding
    /// coincidence worth fixing in place: if it ever stopped being true, the
    /// corner paths in the geometry fixture would stop matching this code and the
    /// failure would look like a bug in the curves rather than in the grid.
    #[test]
    fn linspace_middle_matches_two_thirds() {
        let step = 1.0 / (POINTS_PER_CURVE - 1) as f64;
        assert_eq!(step * 2.0, 2.0 / 3.0);
    }

    /// The corner expansion separates its two interior control points off the
    /// endpoints, and lands exactly on the endpoints at `t = 0` and `t = 1` — so
    /// the anchors of the result are the corners themselves. Only the interior
    /// points distinguish this from the degenerate `[A, A, A, B]` spelling, and
    /// only a test that looks at them will notice.
    #[test]
    fn points_as_corners_interpolates_the_interior_handles() {
        let corners = [
            Point::new(0.0, 0.0),
            Point::new(3.0, 0.0),
            Point::new(3.0, 4.0),
        ];
        let pts = points_as_corners(&corners);

        assert_eq!(pts.len(), 2 * POINTS_PER_CURVE);
        assert_eq!(
            anchors(&pts).unwrap(),
            vec![corners[0], corners[1], corners[1], corners[2]]
        );
        assert_eq!(pts[0], corners[0]);
        assert_eq!(pts[3], corners[1]);
        assert_eq!(pts[4], corners[1]);
        assert_eq!(pts[7], corners[2]);

        // The first segment's handles: a third and two thirds of the way along,
        // not stacked on `corners[0]`.
        assert_eq!(pts[1], interpolate(corners[0], corners[1], 1.0 / 3.0));
        assert_eq!(pts[2], interpolate(corners[0], corners[1], 2.0 / 3.0));
        assert_ne!(
            pts[1], corners[0],
            "the degenerate spelling draws the same line"
        );
    }

    /// A path of one corner has nothing to join. Returning a single point would
    /// make a zero-curve path look like a one-curve path to every routine that
    /// counts `len() / POINTS_PER_CURVE`.
    #[test]
    fn points_as_corners_of_one_corner_is_empty() {
        assert!(points_as_corners(&[Point::new(1.0, 2.0)]).is_empty());
        assert!(points_as_corners(&[]).is_empty());
    }
}
