//! 3D vector, rotation and polygon maths, ported from Manim's `manim.utils.space_ops`.
//!
//! Like [`crate::bezier`], every routine here is checked case by case against
//! Manim 0.22's own output by `tests/manim_parity.rs`, reading
//! `tests/fixtures/manim-space-ops.json`. The port is literal, and where Manim's
//! answer depends on the *rounding path* rather than the formula, the rounding
//! path is reproduced too.
//!
//! Vectors are `[f64; 3]` and matrices `[[f64; 3]; 3]` with `M[i][j]` indexing
//! row `i`, column `j` — the same convention numpy's `matrix[i, j]` prints in, so
//! a fixture value can be compared against a Rust matrix without transposing
//! anything in your head. The one exception is [`rotation_matrix_homogeneous`],
//! which is 4×4 because that is the form Manim uses for 3D projections.
//!
//! Six of these are counterintuitive enough that a reader who knows the textbook
//! formula would port them wrong, and each is pinned by a fixture:
//!
//! **[`rotation_matrix`] renormalises its own rotation vector.** Manim delegates
//! to scipy's `Rotation.from_rotvec(angle * normalize(axis))`, and scipy takes
//! the norm of that rotvec *again* rather than trusting the angle it was handed.
//! For `angle = π, axis = [1, 1, 1]` the second norm is `0x1.921fb54442d19p+1`,
//! one ULP above `π`, and every entry of the resulting matrix differs from the
//! matrix built from the textbook quaternion `[sin(θ/2)·u, cos(θ/2)]`. Both
//! routes exist in Manim and they disagree: [`quaternion_from_angle_axis`] takes
//! the textbook one, [`rotation_matrix`] the renormalising one.
//!
//! **[`rotation_matrix`] uses scipy's squared quaternion form.** Each diagonal
//! entry is `x² - y² - z² + w²`, not the algebraically equal `1 - 2(y² + z²)`.
//! The two differ by an ULP on ordinary inputs; the fixtures record the former.
//!
//! **[`rotation_matrix_transpose`] has two branches that disagree.** Rotation
//! about the z-axis goes through [`rotation_about_z`] and its transpose; every
//! other axis goes through [`rotation_matrix`] and its transpose. Because the
//! first path never touches scipy, `z@0.5` transposes to `0.8775825618903728`
//! while `x@0.5` transposes to `0.8775825618903726`. Collapsing the two branches
//! into one would pass half the fixtures.
//!
//! **[`shoelace`] is the trapezoid rule, with its sign inverted** relative to the
//! textbook shoelace formula: it computes `np.trapezoid(y, x)`, which for a
//! counter-clockwise square is `-1.0`, not `+1.0`. [`shoelace_direction`]
//! compensates by answering `"CW"` for a counter-clockwise polygon, which is
//! Manim's behaviour and is reproduced here, confusing as it reads.
//!
//! **[`get_winding_number`] is not a winding number**, and its pairs wrap. It
//! sums the change in each *vertex's* polar angle about the origin — nothing to
//! do with the polygon's edges — so a square with a corner at the origin reads
//! `0.0` in both orientations, and it is the shape's position relative to the
//! origin, not its orientation, that the answer tracks. Manim's `adjacent_pairs`
//! also closes the cycle, so an `n`-vertex polygon contributes `n` differences
//! rather than `n - 1`; iterating the obvious `windows(2)` reads a diamond as
//! `0.75` instead of `1.0`.
//!
//! **[`find_intersection`] clamps its denominator from below** with
//! `max(dot(v0, normal), 1e-5)` rather than rejecting a degenerate pair. Near-
//! parallel rays therefore return a point on the first ray at a distance of
//! `dot / 1e-5`, which is large but finite, instead of failing.
//!
//! Two smaller traps are noted at their definitions: [`cross2d`] swaps its
//! operand order in the second term, and [`line_intersection`] refuses any line
//! that leaves the `z = 0` plane instead of projecting it.

/// A 3D vector. Manim's `Vector3DLike` minus the ragged-shape possibilities.
pub type Vec3 = [f64; 3];

/// A 2D vector, for the routines [`cross2d`], [`shoelace`] and friends.
pub type Vec2 = [f64; 2];

/// A 3×3 matrix with row-major indexing: `M[i][j]` is row `i`, column `j`.
pub type Mat3 = [[f64; 3]; 3];

/// A 4×4 matrix, row-major, for [`rotation_matrix_homogeneous`].
pub type Mat4 = [[f64; 4]; 4];

/// A quaternion in **scalar-first** order: `[w, x, y, z]`.
///
/// Manim stores quaternions in this order (`manim.utils.space_ops.quaternion_mult`
/// unpacks `w, x, y, z`), unlike the `[x, y, z, w]` order scipy and most graphics
/// code use. [`rotation_quaternion`] below converts between the two, and that
/// conversion is the one place the two conventions meet.
pub type Quat = [f64; 4];

// `manim.constants`, which are plain unit vectors on the three axes. All six are
// here because a scene written against Manim reaches for all six.
/// `RIGHT` — `[1, 0, 0]`.
pub const RIGHT: Vec3 = [1.0, 0.0, 0.0];
/// `UP` — `[0, 1, 0]`.
pub const UP: Vec3 = [0.0, 1.0, 0.0];
/// `OUT` — `[0, 0, 1]`.
pub const OUT: Vec3 = [0.0, 0.0, 1.0];
/// `LEFT` — `[-1, 0, 0]`.
pub const LEFT: Vec3 = [-1.0, 0.0, 0.0];
/// `DOWN` — `[0, -1, 0]`.
pub const DOWN: Vec3 = [0.0, -1.0, 0.0];
/// `IN` — `[0, 0, -1]`.
pub const IN: Vec3 = [0.0, 0.0, -1.0];

/// Manim's default `threshold` for [`find_intersection`].
pub const DEFAULT_INTERSECTION_THRESHOLD: f64 = 1e-5;

/// The refusals these routines can produce.
///
/// Manim raises `ValueError` for each — a non-planar input line, parallel lines,
/// `zip(..., strict=True)` on arrays of different lengths. Carrying them as
/// values keeps the same information without a panic: a language model can reach
/// every one of these code paths, so each has to be reportable rather than fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpaceOpsError {
    /// [`line_intersection`] was handed a line with a non-zero `z` coordinate.
    /// Manim raises rather than projecting onto the plane.
    NotPlanar,
    /// The two lines are parallel, so there is no unique intersection point.
    ParallelLines,
    /// Parallel arrays of differing lengths, where Manim's `zip(..., strict=True)`
    /// would raise.
    UnevenInputs,
    /// [`center_of_mass`] was handed no points, or points of differing dimension.
    RaggedPoints,
}

impl core::fmt::Display for SpaceOpsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotPlanar => write!(f, "line coordinates must lie in the xy-plane"),
            Self::ParallelLines => write!(f, "the lines are parallel, there is no unique point"),
            Self::UnevenInputs => write!(f, "inputs of differing lengths"),
            Self::RaggedPoints => write!(f, "no points, or points of differing dimension"),
        }
    }
}

impl core::error::Error for SpaceOpsError {}

// ---------------------------------------------------------------------------
// vector arithmetic
// ---------------------------------------------------------------------------

/// `np.linalg.norm(vect)`.
///
/// numpy reduces the squares in index order and takes one square root; so does
/// this, which is what makes it agree to the last bit rather than to 1e-16.
/// Verified against `np.linalg.norm` for every vector in the fixtures.
pub fn norm(vect: &[f64]) -> f64 {
    let mut sum = 0.0;
    for x in vect {
        sum += x * x;
    }
    sum.sqrt()
}

/// `np.dot(v, v)` — Manim's `norm_squared`. The square of [`norm`], computed
/// without the intermediate square root, so it is not bit-equal to `norm(v).powi(2)`.
pub fn norm_squared(vect: &[f64]) -> f64 {
    let mut sum = 0.0;
    for x in vect {
        sum += x * x;
    }
    sum
}

/// Manim's `normalize(vect)`: unit length, or a zero vector when the input is zero.
///
/// The zero-length case returns `np.zeros(len(vect))` — a zero vector with
/// *positive* zeros — which is what [`z_to_vector`] tests for.
pub fn normalize(vect: &[f64]) -> Vec<f64> {
    let n = norm(vect);
    if n > 0.0 {
        vect.iter().map(|x| x / n).collect()
    } else {
        vec![0.0; vect.len()]
    }
}

/// Manim's `normalize(vect, fall_back=...)`: the fallback is returned, not copied,
/// when the input is zero-length, so it may be of any length.
pub fn normalize_or(vect: &[f64], fall_back: &[f64]) -> Vec<f64> {
    let n = norm(vect);
    if n > 0.0 {
        vect.iter().map(|x| x / n).collect()
    } else {
        fall_back.to_vec()
    }
}

/// Manim's `normalize` on the fixed-size vectors the rest of this module uses.
pub fn normalize3(vect: Vec3) -> Vec3 {
    let n = norm(&vect);
    if n > 0.0 {
        [vect[0] / n, vect[1] / n, vect[2] / n]
    } else {
        [0.0; 3]
    }
}

/// `cross(v1, v2)`, the 3D cross product.
pub fn cross(v1: Vec3, v2: Vec3) -> Vec3 {
    [
        v1[1] * v2[2] - v1[2] * v2[1],
        v1[2] * v2[0] - v1[0] * v2[2],
        v1[0] * v2[1] - v1[1] * v2[0],
    ]
}

/// `cross2d(a, b)` — the 2D cross product, i.e. the determinant `a₀b₁ - a₁b₀`.
///
/// Manim writes the second term with its operands swapped (`b[0] * a[1]` rather
/// than `a[1] * b[0]`). The value is the same for finite inputs and the sign of a
/// zero result can differ, so the expression is kept as written.
pub fn cross2d(a: Vec2, b: Vec2) -> f64 {
    a[0] * b[1] - b[0] * a[1]
}

/// Manim's `angle_between_vectors`: always in `0..=π`, and computed from the
/// *normalised* vectors, which is what keeps it well-conditioned near `0` and `π`.
///
/// The division by a zero-length vector yields zeros rather than a NaN, so two
/// zero vectors read as an angle of `2·arctan2(0, 0) = 0`.
pub fn angle_between_vectors(v1: &[f64], v2: &[f64]) -> f64 {
    let u1 = normalize(v1);
    let u2 = normalize(v2);
    let diff: Vec<f64> = u1.iter().zip(&u2).map(|(a, b)| a - b).collect();
    let sum: Vec<f64> = u1.iter().zip(&u2).map(|(a, b)| a + b).collect();
    2.0 * norm(&diff).atan2(norm(&sum))
}

/// Manim's `midpoint`, which is `center_of_mass([p1, p2])` — a sum then a divide
/// by two, not the `p1 + (p2 - p1) / 2` that would round differently.
pub fn midpoint(point1: Vec3, point2: Vec3) -> Vec3 {
    [
        (point1[0] + point2[0]) / 2.0,
        (point1[1] + point2[1]) / 2.0,
        (point1[2] + point2[2]) / 2.0,
    ]
}

/// `np.average(points, 0, np.ones(len(points)))` — the unweighted mean of the
/// points, per coordinate.
///
/// numpy multiplies by the weight vector before reducing, so an unweighted
/// average is `sum / n` and not an incremental mean; the two round differently.
pub fn center_of_mass(points: &[&[f64]]) -> Result<Vec<f64>, SpaceOpsError> {
    let Some(first) = points.first() else {
        return Err(SpaceOpsError::RaggedPoints);
    };
    let dim = first.len();
    if points.iter().any(|p| p.len() != dim) || dim == 0 {
        return Err(SpaceOpsError::RaggedPoints);
    }
    let n = points.len() as f64;
    let mut out = vec![0.0; dim];
    for p in points {
        for (o, x) in out.iter_mut().zip(p.iter()) {
            *o += x;
        }
    }
    for o in &mut out {
        *o /= n;
    }
    Ok(out)
}

/// Manim's `angle_of_vector`: `np.angle(complex(v[0], v[1]))`, i.e. `atan2(y, x)`.
///
/// `atan2(0, 0)` is `0.0` here, matching numpy — which is what makes a vertex
/// sitting on the origin contribute nothing to [`get_winding_number`].
pub fn angle_of_vector(v: Vec2) -> f64 {
    v[1].atan2(v[0])
}

/// Python's `%` on floats.
///
/// CPython computes `fmod` first and then corrects the sign, and for an exact
/// multiple it returns a zero carrying the *divisor's* sign. Rust's `%` is the
/// truncated `fmod` and `f64::rem_euclid` keeps a negative zero, so neither is a
/// drop-in for `d_angle % TAU` in [`get_winding_number`].
fn python_float_mod(vx: f64, wx: f64) -> f64 {
    let m = vx % wx;
    if m != 0.0 {
        if (wx < 0.0) != (m < 0.0) { m + wx } else { m }
    } else {
        0.0_f64.copysign(wx)
    }
}

/// `np.sign(x)`.
///
/// Not `f64::signum`, which answers `-1.0` for `-0.0`; numpy answers `+0.0` for
/// both zeros and propagates a NaN. [`rotation_matrix_transpose`] multiplies an
/// angle by this, so the zero case is reachable.
fn numpy_sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else if x.is_nan() {
        x
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// line and polygon geometry
// ---------------------------------------------------------------------------

/// Manim's `line_intersection`: where two lines, each given by a pair of points
/// on it, cross.
///
/// Both points are 3D and **must** have `z == 0`; a non-zero `z` is a refusal
/// rather than a projection, matching Manim's explicit check. (Negative zero
/// passes, since the check is a truthiness test.)
///
/// The cross products are taken in homogeneous coordinates, so the result's third
/// component is a literal `0.0` and its sign is always positive.
pub fn line_intersection(line1: [Vec3; 2], line2: [Vec3; 2]) -> Result<Vec3, SpaceOpsError> {
    if line1.iter().chain(&line2).any(|p| p[2] != 0.0) {
        return Err(SpaceOpsError::NotPlanar);
    }
    // Take the first two components of each point and append the homogeneous 1
    // (`np.pad(..., constant_values=1)`), then the cross product of the pair is
    // the line itself.
    let plane = |line: [Vec3; 2]| [[line[0][0], line[0][1], 1.0], [line[1][0], line[1][1], 1.0]];
    let l1 = cross(plane(line1)[0], plane(line1)[1]);
    let l2 = cross(plane(line2)[0], plane(line2)[1]);
    let [x, y, z] = cross(l1, l2);
    if z == 0.0 {
        return Err(SpaceOpsError::ParallelLines);
    }
    Ok([x / z, y / z, 0.0])
}

/// Manim's `find_intersection`: for each `i`, the point on the ray `p0s[i] +
/// t·v0s[i]` nearest the ray `p1s[i] + t·v1s[i]`.
///
/// Note that `v`s are *direction vectors*, not second points — Manim's signature
/// names them `v0`/`v1` but every caller, including the fixture, passes ray
/// endpoints and reads them as directions. The four slices are zipped strictly,
/// so differing lengths are a refusal.
///
/// The denominator is clamped to at least `threshold`, never rejected. For a
/// pair of parallel rays the answer is therefore a finite point far along the
/// first ray (at `dot / threshold`), not an error.
pub fn find_intersection(
    p0s: &[Vec3],
    v0s: &[Vec3],
    p1s: &[Vec3],
    v1s: &[Vec3],
    threshold: f64,
) -> Result<Vec<Vec3>, SpaceOpsError> {
    if p0s.len() != v0s.len() || p0s.len() != p1s.len() || p0s.len() != v1s.len() {
        return Err(SpaceOpsError::UnevenInputs);
    }
    let mut result = Vec::with_capacity(p0s.len());
    for i in 0..p0s.len() {
        let (p0, v0, p1, v1) = (p0s[i], v0s[i], p1s[i], v1s[i]);
        let normal = cross(v1, cross(v0, v1));
        let d =
            (p1[0] - p0[0]) * normal[0] + (p1[1] - p0[1]) * normal[1] + (p1[2] - p0[2]) * normal[2];
        let denom = (v0[0] * normal[0] + v0[1] * normal[1] + v0[2] * normal[2]).max(threshold);
        let t = d / denom;
        result.push([p0[0] + t * v0[0], p0[1] + t * v0[1], p0[2] + t * v0[2]]);
    }
    Ok(result)
}

/// Manim's `shoelace`: `np.trapezoid(y, x)`, the signed area by the trapezoid rule.
///
/// The sign convention is the *opposite* of the textbook shoelace formula — see
/// the closing note in this module's docs, and [`shoelace_direction`].
///
/// Fewer than two points sums over no terms; numpy's `add.reduce` answers `0.0`
/// there, and so does this.
pub fn shoelace(x_y: &[Vec2]) -> f64 {
    if x_y.len() < 2 {
        return 0.0;
    }
    let mut acc = None;
    for w in x_y.windows(2) {
        let (x0, y0) = (w[0][0], w[0][1]);
        let (x1, y1) = (w[1][0], w[1][1]);
        // `d * (y[slice1] + y[slice2]) / 2.0`, in that grouping.
        let term = (x1 - x0) * (y1 + y0) / 2.0;
        acc = Some(match acc {
            None => term,
            Some(a) => a + term,
        });
    }
    acc.unwrap_or(0.0)
}

/// Which way a polygon is wound. Manim's `shoelace_direction` returns the strings
/// `"CW"` / `"CCW"`; this is the typed form, and its meaning is the one described
/// on [`shoelace`] — `Cw` is what a mathematically counter-clockwise polygon gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShoelaceDirection {
    /// Positive [`shoelace`] area.
    Cw,
    /// Zero or negative [`shoelace`] area.
    Ccw,
}

/// Manim's `shoelace_direction`: `"CW" if shoelace(...) > 0 else "CCW"`.
pub fn shoelace_direction(x_y: &[Vec2]) -> ShoelaceDirection {
    if shoelace(x_y) > 0.0 {
        ShoelaceDirection::Cw
    } else {
        ShoelaceDirection::Ccw
    }
}

/// Manim's `get_winding_number`.
///
/// Despite the name this does not traverse the polygon's edges: it converts each
/// vertex to a polar angle about the origin, sums the successive differences
/// wrapped into `(-π, π]`, and divides by a full turn. The result counts how many
/// times the vertex list *rotates around the origin*, so it is `±1` for a shape
/// enclosing the origin and `0` for one that does not — in either orientation.
///
/// The pairs **wrap around**: Manim's `adjacent_pairs` is `adjacent_n_tuples(n, 2)`
/// and closes the cycle, so an `n`-vertex polygon contributes `n` differences, not
/// `n - 1`. That is what makes an open and a closed spelling of the same shape
/// agree — the closed spelling's extra pair is the duplicated vertex against
/// itself and contributes zero. Iterating `windows(2)` instead loses the last
/// difference and reads a diamond as `0.75` rather than `1.0`, which is exactly
/// what this port did before a fixture caught it.
pub fn get_winding_number(points: &[Vec2]) -> f64 {
    let pi = core::f64::consts::PI;
    let tau = 2.0 * pi;
    let n = points.len();
    let mut total_angle = 0.0;
    for i in 0..n {
        let d_angle = angle_of_vector(points[(i + 1) % n]) - angle_of_vector(points[i]);
        let d_angle = python_float_mod(d_angle + pi, tau) - pi;
        total_angle += d_angle;
    }
    total_angle / tau
}

// ---------------------------------------------------------------------------
// rotations
// ---------------------------------------------------------------------------

/// Transpose of a 3×3 matrix. Not `Mat3::transpose` because `Mat3` is a plain
/// array alias; kept private because nothing outside this module needs it.
fn transpose3(m: Mat3) -> Mat3 {
    [
        [m[0][0], m[1][0], m[2][0]],
        [m[0][1], m[1][1], m[2][1]],
        [m[0][2], m[1][2], m[2][2]],
    ]
}

/// Manim's `rotation_about_z`: the plane rotation embedded in 3D.
///
/// Written as `np.cos`/`np.sin` of the same angle, so `angle = 0` gives an entry
/// of `-0.0` in row 0, column 1 — a sign [`rotation_matrix_transpose`] inherits
/// and a fixture records.
pub fn rotation_about_z(angle: f64) -> Mat3 {
    let c = angle.cos();
    let s = angle.sin();
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}

/// `Rotation.from_rotvec(rotvec)` from scipy 1.18.1, returning `[x, y, z, w]`.
///
/// This is the route [`rotation_matrix`] takes, and the reason it is not the
/// textbook quaternion: the norm is taken of the rotvec *it is given*, not of the
/// axis before scaling, so a rounding appears that the direct formula misses.
/// Two further details are scipy's, not the textbook's:
///
/// - the angle is handled branchlessly, with a Taylor expansion for `|rotvec|`
///   below `1e-3` and a boolean added to the divisor rather than a divide-by-zero
///   guard; and
/// - the non-small branch divides `sin(θ/2)` by the recomputed norm rather than
///   scaling the rotvec by `sin(θ/2)/θ`.
fn rotation_quaternion(angle: f64, axis: Vec3) -> [f64; 4] {
    let u = normalize3(axis);
    let rotvec = [angle * u[0], angle * u[1], angle * u[2]];
    let theta = norm(&rotvec);
    let small_angle = theta <= 1e-3;
    let theta2 = theta * theta;
    let small_scale = 0.5 - theta2 / 48.0 + theta2.powi(2) / 3840.0;
    let scale = if small_angle {
        small_scale
    } else {
        (theta / 2.0).sin() / theta
    };
    let half = theta / 2.0;
    [
        rotvec[0] * scale,
        rotvec[1] * scale,
        rotvec[2] * scale,
        half.cos(),
    ]
}

/// scipy's `as_matrix` on a `[x, y, z, w]` quaternion.
///
/// The squared form `x² - y² - z² + w²` on the diagonal is load-bearing: the
/// algebraically equal `1 - 2(y² + z²)` is a different `f64` and the fixtures
/// record this one.
fn quaternion_to_matrix(q: [f64; 4]) -> Mat3 {
    let [x, y, z, w] = q;
    let (x2, y2, z2, w2) = (x * x, y * y, z * z, w * w);
    let (xy, zw) = (x * y, z * w);
    let (xz, yw) = (x * z, y * w);
    let (yz, xw) = (y * z, x * w);
    [
        [x2 - y2 - z2 + w2, 2.0 * (xy - zw), 2.0 * (xz + yw)],
        [2.0 * (xy + zw), -x2 + y2 - z2 + w2, 2.0 * (yz - xw)],
        [2.0 * (xz - yw), 2.0 * (yz + xw), -x2 - y2 + z2 + w2],
    ]
}

/// Manim's `rotation_matrix(angle, axis)`: rotation of `angle` radians about
/// `axis`, through scipy's quaternion route.
///
/// See this module's docs for why the answer differs in the last bits from the
/// textbook construction, and from [`quaternion_from_angle_axis`] + a
/// hand-written matrix. `axis` need not be normalised — [`normalize3`] is applied
/// — but a zero-length `axis` yields an all-zero rotvec, whose quaternion is
/// `[0, 0, 0, 1]` and whose matrix is the identity.
pub fn rotation_matrix(angle: f64, axis: Vec3) -> Mat3 {
    quaternion_to_matrix(rotation_quaternion(angle, axis))
}

/// Manim's `rotation_matrix(angle, axis, homogeneous=True)`: the same rotation as
/// a 4×4 matrix, laid into the top-left corner of `np.eye(4)`.
///
/// This is the form [`crate::Viewport`]-style projections and Manim's 3D camera
/// use; the bottom-right `1.0` and the zero row and column are what make it
/// compose with translations.
pub fn rotation_matrix_homogeneous(angle: f64, axis: Vec3) -> Mat4 {
    let m = rotation_matrix(angle, axis);
    [
        [m[0][0], m[0][1], m[0][2], 0.0],
        [m[1][0], m[1][1], m[1][2], 0.0],
        [m[2][0], m[2][1], m[2][2], 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// Manim's `rotation_matrix_transpose`: the transpose of [`rotation_matrix`],
/// except about the z-axis, where it is [`rotation_about_z`] and its transpose.
///
/// The exception is not just a shortcut — the two produce different last bits, so
/// this cannot be written as `transpose3(rotation_matrix(...))`. The condition for
/// taking it is that the axis's first two components are *both* exactly zero,
/// which holds for `[0, 0, -1]` and `[-0.0, -0.0, -1.0]` alike. The angle is
/// multiplied by `np.sign(axis[2])`, so a rotation about `-z` is a rotation about
/// `+z` by the negated angle.
pub fn rotation_matrix_transpose(angle: f64, axis: Vec3) -> Mat3 {
    if axis[0] == 0.0 && axis[1] == 0.0 {
        transpose3(rotation_about_z(angle * numpy_sign(axis[2])))
    } else {
        transpose3(rotation_matrix(angle, axis))
    }
}

/// Manim's `z_to_vector`: a rotation carrying `+z` onto the given direction.
///
/// The result is `[axis_x, axis_y, axis_z]ᵀ`, i.e. `M[i][j]` is component `i` of
/// `axis_j` — so the columns are the axes, as with any rotation matrix, but it is
/// spelled as a transposed row-stack.
///
/// The two branches are not cosmetic. `cross(axis_z, RIGHT)` vanishes when the
/// target direction *is* `+x`, and Manim then rebuilds the frame from `UP`
/// instead; the `-cross(axis_x, axis_z)` in that branch produces negative zeros
/// that the fixtures record, and `cross(axis_z, axis_x)` — which is the same
/// rotation — would not.
pub fn z_to_vector(vector: Vec3) -> Mat3 {
    let axis_z = normalize3(vector);
    let mut axis_y = normalize3(cross(axis_z, RIGHT));
    let mut axis_x = cross(axis_y, axis_z);
    if norm(&axis_y) == 0.0 {
        // The vector passed just so happened to be in the x direction.
        axis_x = normalize3(cross(UP, axis_z));
        let y = cross(axis_x, axis_z);
        axis_y = [-y[0], -y[1], -y[2]];
    }
    // `np.array([axis_x, axis_y, axis_z]).T`: M[i][j] is component i of axis j.
    [
        [axis_x[0], axis_y[0], axis_z[0]],
        [axis_x[1], axis_y[1], axis_z[1]],
        [axis_x[2], axis_y[2], axis_z[2]],
    ]
}

// ---------------------------------------------------------------------------
// quaternions
// ---------------------------------------------------------------------------

/// Manim's `quaternion_from_angle_axis`, in Manim's scalar-first `[w, x, y, z]`
/// order.
///
/// This is the textbook construction and, unlike [`rotation_quaternion`], it does
/// not renormalise the angle; the two disagree in the last bits, which is why
/// both exist here.
pub fn quaternion_from_angle_axis(angle: f64, axis: Vec3, axis_normalized: bool) -> Quat {
    let axis = if axis_normalized {
        axis
    } else {
        normalize3(axis)
    };
    let half = angle / 2.0;
    let s = half.sin();
    [half.cos(), s * axis[0], s * axis[1], s * axis[2]]
}

/// Manim's `quaternion_mult`: the Hamilton product of two quaternions, both
/// scalar-first.
///
/// Manim's variadic form returns `[1, 0, 0, 0]` for no arguments; the binary form
/// here takes exactly two, which is every call site in the engine. The scalar and
/// vector parts are interleaved `[w, x, y, z]`, so the sign pattern of the vector
/// part is the one to check against a table rather than to re-derive.
pub fn quaternion_mult(q1: Quat, q2: Quat) -> Quat {
    let [w1, x1, y1, z1] = q1;
    let [w2, x2, y2, z2] = q2;
    [
        w1 * w2 - x1 * x2 - y1 * y2 - z1 * z2,
        w1 * x2 + x1 * w2 + y1 * z2 - z1 * y2,
        w1 * y2 + y1 * w2 + z1 * x2 - x1 * z2,
        w1 * z2 + z1 * w2 + x1 * y2 - y1 * x2,
    ]
}

/// Manim's `rotate_vector`: `rotation_matrix(angle, axis) @ vector`.
///
/// Each output component is the dot product of a matrix row with the vector,
/// summed in index order. Manim accepts a 2D vector and appends `z = 0`; the
/// signature here is 3D, so a 2D caller appends the zero itself.
pub fn rotate_vector(vector: Vec3, angle: f64, axis: Vec3) -> Vec3 {
    let m = rotation_matrix(angle, axis);
    [
        m[0][0] * vector[0] + m[0][1] * vector[1] + m[0][2] * vector[2],
        m[1][0] * vector[0] + m[1][1] * vector[1] + m[1][2] * vector[2],
        m[2][0] * vector[0] + m[2][1] * vector[1] + m[2][2] * vector[2],
    ]
}

// ---------------------------------------------------------------------------
// spherical coordinates
// ---------------------------------------------------------------------------

/// Manim's `cartesian_to_spherical`, returning `[r, theta, phi]`.
///
/// The component order is worth stating because Manim's own spelling of `phi` is
/// non-standard: it is the polar angle from `+z`, computed as `arccos(z / r)`,
/// while `theta` is the azimuth `atan2(y, x)`. A zero vector maps to `[0, 0, 0]`
/// rather than to NaNs.
pub fn cartesian_to_spherical(vec: Vec3) -> Vec3 {
    let r = norm(&vec);
    if r == 0.0 {
        return [0.0; 3];
    }
    let phi = (vec[2] / r).acos();
    let theta = vec[1].atan2(vec[0]);
    [r, theta, phi]
}

/// Manim's `spherical_to_cartesian`: the inverse of [`cartesian_to_spherical`].
///
/// The products are grouped left to right (`r * cos(theta) * sin(phi)`), which is
/// the order Manim's numpy expression evaluates in; regrouping them changes the
/// last bit. It is not an exact inverse of [`cartesian_to_spherical`] on the two
/// branches where that routine clamps, but it does round-trip the fixture vector
/// back to its own components.
pub fn spherical_to_cartesian(spherical: Vec3) -> Vec3 {
    let [r, theta, phi] = spherical;
    [
        r * theta.cos() * phi.sin(),
        r * theta.sin() * phi.sin(),
        r * phi.cos(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handful of the fixture's cases, kept here so `cargo test -p rustim-math`
    /// catches a regression in this module even when the fixtures are not
    /// present. `tests/manim_parity.rs` replays the whole file.
    #[test]
    fn rotation_matrix_diag_pi_renormalises() {
        // One ULP above pi, because scipy norms the scaled rotvec again.
        let m = rotation_matrix(core::f64::consts::PI, [1.0, 1.0, 1.0]);
        assert_eq!(m[0][0].to_bits(), (-0.3333333333333333_f64).to_bits());
        assert_eq!(m[0][1].to_bits(), 0.6666666666666669_f64.to_bits());
        assert_eq!(m[0][2].to_bits(), 0.6666666666666664_f64.to_bits());
    }

    #[test]
    fn transpose_branches_disagree_on_the_z_axis() {
        // The two branches put the same cosine in different slots, and one ULP
        // apart: the z branch is a direct `cos(0.5)`, the generic branch's
        // squared quaternion form lands a bit low.
        let z = rotation_matrix_transpose(0.5, [0.0, 0.0, 1.0]);
        assert_eq!(z[0][0].to_bits(), 0.8775825618903728_f64.to_bits());
        let x = rotation_matrix_transpose(0.5, [1.0, 0.0, 0.0]);
        assert_eq!(x[1][1].to_bits(), 0.8775825618903726_f64.to_bits());
    }

    #[test]
    fn shoelace_inverts_the_textbook_sign() {
        let ccw = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0], [0.0, 0.0]];
        assert_eq!(shoelace(&ccw), -1.0);
        assert_eq!(shoelace_direction(&ccw), ShoelaceDirection::Ccw);
        let mut cw = ccw;
        cw.reverse();
        assert_eq!(shoelace(&cw), 1.0);
        assert_eq!(shoelace_direction(&cw), ShoelaceDirection::Cw);
    }

    #[test]
    fn winding_number_tracks_the_origin_not_the_edges() {
        let diamond = [[1.0, 0.0], [0.0, 1.0], [-1.0, 0.0], [0.0, -1.0]];
        assert_eq!(get_winding_number(&diamond), 1.0);
        // The same diamond reversed, and a square with a corner on the origin.
        assert_eq!(
            get_winding_number(&diamond.iter().rev().copied().collect::<Vec<_>>()),
            -1.0
        );
        let corner = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0], [0.0, 0.0]];
        assert_eq!(get_winding_number(&corner), 0.0);
    }

    #[test]
    fn z_to_vector_takes_the_up_fallback_for_plus_x() {
        let m = z_to_vector([1.0, 0.0, 0.0]);
        assert_eq!(m, [[0.0, -0.0, 1.0], [0.0, 1.0, 0.0], [-1.0, -0.0, 0.0]]);
    }
}
