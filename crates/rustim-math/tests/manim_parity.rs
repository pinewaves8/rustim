//! Replays every recorded Manim 0.22 result against this crate's ports.
//!
//! The fixtures in `tests/fixtures/` were produced by `tools/manim-fixtures/gen.py`
//! calling Manim's own functions (see that script; it runs once and the output is
//! checked in, so this suite needs no Python and no Manim). Each fixture is
//! self-describing: it records the inputs alongside the expected values, and this
//! file reads both, so a test cannot drift from the fixture it claims to check.
//!
//! # What this suite enforces beyond "the numbers match"
//!
//! **Every case is consumed.** A parity suite's characteristic failure is not a
//! wrong comparison but a *missing* one — a new group of cases lands in the fixture
//! and passes because nothing looks at it. So each test asserts exhaustively over
//! the groups it names, and [`every_fixture_group_has_a_test`] asserts that the
//! groups named across this file are exactly the groups in the files. Adding a case
//! group to the generator and regenerating therefore fails this suite until a test
//! claims it.
//!
//! **The tolerance is absolute, not relative.** `1e-9` on coordinates whose
//! magnitudes run to 10 or so, with nothing admitted on the grounds of being
//! "close". The ports reproduce Manim's rounding paths, so these hold; a port that
//! only reproduced Manim's formulas would not, which is the point of the exercise.
//!
//! The fixtures are 3D and these ports are 2D. Manim's world is 3D even for a 2D
//! scene, so every fixture point has `z = 0.0`, and every routine here is a sum of
//! products of the coordinates; the converters below therefore drop `z`.
//!
//! One consequence worth stating: a signed zero in an expected value compares equal
//! to the opposite sign under an absolute difference. The `line_intersection` and
//! `z_to_vector` cases pin signed zeros that this cannot see.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use rustim_math::Point;
use rustim_math::bezier;
use rustim_math::space_ops as so;
use serde_json::{Map, Value};

/// Absolute tolerance on every coordinate, in world units.
const TOLERANCE: f64 = 1e-9;

/// The generator's own angle for the quarter turn, not `f64::consts::FRAC_PI_2`
/// re-derived here — it is read out of the fixture's `rotation_angles`, which is
/// the list the generator actually drew the angle from. They turn out to be the
/// same `f64`, and that is asserted rather than assumed.
fn half_pi(fixture: &Fixture) -> f64 {
    let quarter = fixture.input::<Vec<f64>>("rotation_angles")[2];
    assert_eq!(
        quarter,
        std::f64::consts::FRAC_PI_2,
        "the generator's quarter turn is no longer exactly FRAC_PI_2"
    );
    quarter
}
// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

/// One fixture file: the case table plus the inputs the cases were computed from.
struct Fixture {
    /// group name -> case name -> expected value.
    cases: BTreeMap<String, BTreeMap<String, Value>>,
    /// Named inputs, read by the tests rather than restated here.
    inputs: Map<String, Value>,
}

impl Fixture {
    fn load(file: &str) -> Self {
        let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", file]
            .iter()
            .collect();
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let root: Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let cases = root["cases"]
            .as_object()
            .expect("fixture has a `cases` object")
            .iter()
            .map(|(group, entries)| {
                let entries = entries.as_object().expect("case group is an object");
                (
                    group.clone(),
                    entries
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                )
            })
            .collect();
        Self {
            cases,
            inputs: root["inputs"]
                .as_object()
                .expect("fixture has inputs")
                .clone(),
        }
    }

    /// Every case group in the file, for the "all groups have a test" guard.
    fn groups(&self) -> BTreeSet<String> {
        self.cases.keys().cloned().collect()
    }

    /// A named input, converted, or a clear failure naming what was wanted.
    fn input<T: FromValue>(&self, name: &str) -> T {
        T::from_value(
            self.inputs
                .get(name)
                .unwrap_or_else(|| panic!("fixture has no input {name:?}")),
            name,
        )
    }

    /// A named input as an object, for the tests that need to iterate one.
    fn input_map(&self, name: &str) -> &Map<String, Value> {
        self.inputs
            .get(name)
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("fixture has no object input {name:?}"))
    }
}

/// Reads the shapes the generator writes, so a test can ask for a type and get
/// either the value or a message naming the input that was the wrong shape.
///
/// Arrays and objects are covered by blanket impls, so `Vec<Vec<Vec<f64>>>` (the
/// ray table) and `BTreeMap<String, Vec<Point>>` (the named curves) both work by
/// composition. What is left is the leaf types.
trait FromValue: Sized {
    fn from_value(v: &Value, what: &str) -> Self;
}

impl FromValue for f64 {
    fn from_value(v: &Value, what: &str) -> Self {
        v.as_f64()
            .unwrap_or_else(|| panic!("{what}: expected a number, got {v}"))
    }
}

impl FromValue for bool {
    fn from_value(v: &Value, what: &str) -> Self {
        v.as_bool()
            .unwrap_or_else(|| panic!("{what}: expected a bool, got {v}"))
    }
}

impl FromValue for usize {
    fn from_value(v: &Value, what: &str) -> Self {
        v.as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or_else(|| panic!("{what}: expected a count, got {v}"))
    }
}

impl FromValue for Point {
    fn from_value(v: &Value, what: &str) -> Self {
        to_point(v, what)
    }
}

/// An `[a, b]` pair, for the `partial_bezier_points` grid.
impl FromValue for (f64, f64) {
    fn from_value(v: &Value, what: &str) -> Self {
        let n = Vec::<f64>::from_value(v, what);
        assert_eq!(n.len(), 2, "{what}: expected pairs, got {v}");
        (n[0], n[1])
    }
}

impl<T: FromValue> FromValue for Vec<T> {
    fn from_value(v: &Value, what: &str) -> Self {
        v.as_array()
            .unwrap_or_else(|| panic!("{what}: expected an array, got {v}"))
            .iter()
            .map(|x| T::from_value(x, what))
            .collect()
    }
}

impl<T: FromValue> FromValue for BTreeMap<String, T> {
    fn from_value(v: &Value, what: &str) -> Self {
        v.as_object()
            .unwrap_or_else(|| panic!("{what}: expected an object, got {v}"))
            .iter()
            .map(|(k, x)| (k.clone(), T::from_value(x, what)))
            .collect()
    }
}

/// A fixture point `[x, y, z]` as this crate's 2D [`Point`]. See the module note.
fn to_point(v: &Value, what: &str) -> Point {
    let a = v
        .as_array()
        .unwrap_or_else(|| panic!("{what}: expected a point, got {v}"));
    assert!(
        a.len() >= 2,
        "{what}: expected at least 2 components, got {v}"
    );
    Point::new(f64::from_value(&a[0], what), f64::from_value(&a[1], what))
}

/// A fixture vector `[x, y, z]` as a [`so::Vec3`].
fn to_vec3(v: &Value, what: &str) -> so::Vec3 {
    let a = v
        .as_array()
        .unwrap_or_else(|| panic!("{what}: expected a vector, got {v}"));
    assert_eq!(a.len(), 3, "{what}: expected 3 components, got {v}");
    [
        f64::from_value(&a[0], what),
        f64::from_value(&a[1], what),
        f64::from_value(&a[2], what),
    ]
}

/// A matrix as rows of `f64`.
#[track_caller]
fn rows(v: &Value, what: &str) -> Vec<Vec<f64>> {
    v.as_array()
        .unwrap_or_else(|| panic!("{what}: expected a matrix, got {v}"))
        .iter()
        .map(|row| Vec::<f64>::from_value(row, what))
        .collect()
}

/// A fixture list of 2D points as [`so::Vec2`]s.
#[track_caller]
fn as_vec2(v: &Value, what: &str) -> Vec<so::Vec2> {
    Vec::<Vec<f64>>::from_value(v, what)
        .iter()
        .map(|p| [p[0], p[1]])
        .collect()
}

// ---------------------------------------------------------------------------
// the recorder
// ---------------------------------------------------------------------------

/// Hands out expected values while remembering which cases were asked for, so a
/// test can then assert it left nothing unread.
struct Recorder<'a> {
    fixture: &'a Fixture,
    seen: BTreeSet<(String, String)>,
}

impl<'a> Recorder<'a> {
    fn new(fixture: &'a Fixture) -> Self {
        Self {
            fixture,
            seen: BTreeSet::new(),
        }
    }

    /// The expected value for a case, recording that it was exercised.
    #[track_caller]
    fn expect(&mut self, group: &str, case: &str) -> Value {
        let value = self.read(group, case);
        self.seen.insert((group.to_string(), case.to_string()));
        value
    }

    /// [`Self::expect`] without recording, for a case another test owns but this
    /// one needs as an *input* — `point_lies_on_bezier` is asserted against the
    /// midpoint Manim computed, which is the `bezier@0.5` case.
    #[track_caller]
    fn peek(&self, group: &str, case: &str) -> Value {
        self.read(group, case)
    }

    #[track_caller]
    fn read(&self, group: &str, case: &str) -> Value {
        let cases = self
            .fixture
            .cases
            .get(group)
            .unwrap_or_else(|| panic!("no case group {group:?}"));
        cases.get(case).cloned().unwrap_or_else(|| {
            let prefix = case.split('@').next().unwrap_or(case);
            let near: Vec<&str> = cases
                .keys()
                .filter(|k| k.starts_with(prefix))
                .map(String::as_str)
                .collect();
            panic!("no case {case:?} in group {group:?}; nearby: {near:?}")
        })
    }

    /// Fail unless every case in each named group was consumed. This is the guard
    /// that makes the suite's coverage a property of the code rather than of
    /// someone's diligence.
    #[track_caller]
    fn assert_exhaustive(&self, groups: &[&str]) {
        let mut unread: Vec<String> = Vec::new();
        for group in groups {
            let cases = self
                .fixture
                .cases
                .get(*group)
                .unwrap_or_else(|| panic!("no case group {group:?}"));
            for case in cases.keys() {
                if !self.seen.contains(&(group.to_string(), case.clone())) {
                    unread.push(format!("{group}/{case}"));
                }
            }
        }
        assert!(
            unread.is_empty(),
            "{} fixture case(s) never asserted: {:?}",
            unread.len(),
            unread
        );
    }
}

// ---------------------------------------------------------------------------
// comparisons
// ---------------------------------------------------------------------------

#[track_caller]
fn close(got: f64, want: f64, what: &str) {
    let diff = (got - want).abs();
    assert!(
        diff <= TOLERANCE,
        "{what}: got {got:?}, want {want:?} (|diff| = {diff:e} > {TOLERANCE:e})"
    );
}

#[track_caller]
fn close_points(got: &[Point], want: &[Point], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: point count");
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        close(g.x, w.x, &format!("{what}[{i}].x"));
        close(g.y, w.y, &format!("{what}[{i}].y"));
    }
}

#[track_caller]
fn close_floats(got: &[f64], want: &[f64], what: &str) {
    assert_eq!(
        got.len(),
        want.len(),
        "{what}: length, got {got:?} want {want:?}"
    );
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        close(*g, *w, &format!("{what}[{i}]"));
    }
}

/// Guards on the comparison itself. A `close` that accepted everything would
/// leave all 426 cases green and nothing else here would notice, so the boundary
/// is asserted from both sides.
#[test]
fn tolerance_is_inclusive_at_the_boundary() {
    close(TOLERANCE, 0.0, "exactly at the tolerance");
    close(-TOLERANCE, 0.0, "exactly at the tolerance");
}

#[test]
#[should_panic(expected = "twice the tolerance")]
fn tolerance_rejects_twice_the_boundary() {
    close(2.0 * TOLERANCE, 0.0, "twice the tolerance");
}

/// Python's `str(float)`, which is how the generator interpolated numbers into
/// case names: `0.0` is `"0.0"` and `1.0` is `"1.0"`, where Rust's `Display` for
/// `f64` gives `"0"` and `"1"`. Both languages print the shortest string that
/// round-trips, so only the integral case needs the decimal point put back.
fn py_float(x: f64) -> String {
    if x.is_finite() && x.fract() == 0.0 {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}

// ---------------------------------------------------------------------------
// manim-bezier.json
// ---------------------------------------------------------------------------

/// The curve sets the degree-generic routines run over, each with the division
/// counts the generator paired it with — four-point curves with the cubic division
/// list, the quartic and quintic with theirs. This matters because the
/// degree-generic routines have general branches that four-point curves never
/// reach; dropping the high-degree curves would leave those untested.
fn degree_generic(fixture: &Fixture) -> Vec<(String, Vec<Point>, Vec<usize>)> {
    let curves: BTreeMap<String, Vec<Point>> = fixture.input("curves");
    let high: BTreeMap<String, Vec<Point>> = fixture.input("high_degree");
    let cubic_div: Vec<usize> = fixture.input("cubic_divisions");
    let high_div: Vec<usize> = fixture.input("high_degree_divisions");
    curves
        .into_iter()
        .map(|(n, p)| (n, p, cubic_div.clone()))
        .chain(high.into_iter().map(|(n, p)| (n, p, high_div.clone())))
        .collect()
}

/// The six cubics the point queries run on. The seventh, `degenerate`, is four
/// copies of one point; those two routines have nothing to return for it (Manim
/// raises there), so the generator skips the groups for it.
const QUERYABLE_CURVES: [&str; 6] = ["arch", "collinear", "cusp", "folded", "skew", "wobble"];

#[test]
fn bezier_and_split_curves_match() {
    let fixture = Fixture::load("manim-bezier.json");
    let mut rec = Recorder::new(&fixture);
    let ts: Vec<f64> = fixture.input("ts");

    for (cname, pts, _) in degree_generic(&fixture) {
        for t in &ts {
            let case = format!("{cname}@{}", py_float(*t));

            let want = rec.expect("bezier", &case);
            let got = bezier::bezier(&pts, *t);
            close(
                got.x,
                f64::from_value(&want[0], &case),
                &format!("{case}.x"),
            );
            close(
                got.y,
                f64::from_value(&want[1], &case),
                &format!("{case}.y"),
            );

            // `split_bezier` performs the same subdivision that the first half of
            // `partial_bezier_points` does; the fixture checks it separately
            // because the two reach Manim's code by different routes.
            let want = rec.expect("split_bezier", &case);
            let got = bezier::split_bezier(&pts, *t);
            close_points(&got, &Vec::<Point>::from_value(&want, &case), &case);
        }
    }

    rec.assert_exhaustive(&["bezier", "split_bezier"]);
}

#[test]
fn partial_bezier_points_match() {
    let fixture = Fixture::load("manim-bezier.json");
    let mut rec = Recorder::new(&fixture);
    let ab: Vec<(f64, f64)> = fixture.input("ab_pairs");

    for (cname, pts, _) in degree_generic(&fixture) {
        for (a, b) in &ab {
            let case = format!("{cname}@{},{}", py_float(*a), py_float(*b));
            let want = rec.expect("partial_bezier_points", &case);
            let got = bezier::partial_bezier_points(&pts, *a, *b);
            close_points(&got, &Vec::<Point>::from_value(&want, &case), &case);
        }
    }

    rec.assert_exhaustive(&["partial_bezier_points"]);
}

#[test]
fn subdivide_bezier_matches() {
    let fixture = Fixture::load("manim-bezier.json");
    let mut rec = Recorder::new(&fixture);

    for (cname, pts, divisions) in degree_generic(&fixture) {
        for n in divisions {
            let case = format!("{cname}@{n}");
            let want = rec.expect("subdivide_bezier", &case);
            let got = bezier::subdivide_bezier(&pts, n);
            close_points(&got, &Vec::<Point>::from_value(&want, &case), &case);
        }
    }

    rec.assert_exhaustive(&["subdivide_bezier"]);
}

#[test]
fn is_closed_matches() {
    let fixture = Fixture::load("manim-bezier.json");
    let mut rec = Recorder::new(&fixture);

    let curves: BTreeMap<String, Vec<Point>> = fixture.input("curves");
    let high: BTreeMap<String, Vec<Point>> = fixture.input("high_degree");
    let paths: BTreeMap<String, Vec<Point>> = fixture.input("paths");

    for (name, pts) in curves.into_iter().chain(high).chain(paths) {
        let want = rec.expect("is_closed", &name);
        assert_eq!(
            bezier::is_closed(&pts),
            bool::from_value(&want, &name),
            "is_closed {name}"
        );
    }

    rec.assert_exhaustive(&["is_closed"]);
}

#[test]
fn point_lies_on_bezier_matches() {
    let fixture = Fixture::load("manim-bezier.json");
    let mut rec = Recorder::new(&fixture);
    let offset: Vec<f64> = fixture.input("off_curve_offset");
    let (dx, dy) = (offset[0], offset[1]);
    let curves: BTreeMap<String, Vec<Point>> = fixture.input("curves");

    // Both query points are Manim's own: the "on" point is the recorded
    // `bezier@0.5` value, and the "off" point is that value plus the recorded
    // offset. Neither side involves arithmetic this crate decided.
    for cname in QUERYABLE_CURVES {
        let v = rec.peek("bezier", &format!("{cname}@0.5"));
        let mid = Point::new(
            f64::from_value(&v[0], "midpoint"),
            f64::from_value(&v[1], "midpoint"),
        );
        let off = Point::new(mid.x + dx, mid.y + dy);

        for (which, query) in [("on", mid), ("off", off)] {
            let key = format!("{cname}@{which}");
            let want = rec.expect("point_lies_on_bezier", &key);
            let got = bezier::point_lies_on_bezier(query, &curves[cname]).expect("degree 3");
            assert_eq!(
                got,
                bool::from_value(&want, &key),
                "point_lies_on_bezier {key}"
            );
        }
    }

    rec.assert_exhaustive(&["point_lies_on_bezier"]);
}

#[test]
fn proportions_along_bezier_curve_match() {
    let fixture = Fixture::load("manim-bezier.json");
    let mut rec = Recorder::new(&fixture);
    let curves: BTreeMap<String, Vec<Point>> = fixture.input("curves");

    for cname in QUERYABLE_CURVES {
        let v = rec.peek("bezier", &format!("{cname}@0.5"));
        let mid = Point::new(
            f64::from_value(&v[0], "midpoint"),
            f64::from_value(&v[1], "midpoint"),
        );
        let want = rec.expect("proportions_along_bezier_curve_for_point", cname);
        let got = bezier::proportions_along_bezier_curve_for_point(mid, &curves[cname])
            .expect("degree 3");
        close_floats(&got, &Vec::<f64>::from_value(&want, cname), cname);
    }

    // The `folded` curve's own vertex. Its `y` is constant at 1.0, so only `x`
    // constrains the answer — the one place where a single axis has a repeated
    // root, which is what pins how many roots come back rather than what order
    // they are in.
    let want = rec.expect("proportions_along_bezier_curve_for_point", "folded@vertex");
    let got =
        bezier::proportions_along_bezier_curve_for_point(Point::new(0.0, 1.0), &curves["folded"])
            .expect("degree 3");
    close_floats(
        &got,
        &Vec::<f64>::from_value(&want, "folded@vertex"),
        "folded@vertex",
    );

    rec.assert_exhaustive(&["proportions_along_bezier_curve_for_point"]);
}

#[test]
fn quadratic_approximation_matches() {
    let fixture = Fixture::load("manim-bezier.json");
    let mut rec = Recorder::new(&fixture);
    let curves: BTreeMap<String, Vec<Point>> = fixture.input("curves");

    for (cname, pts) in &curves {
        let want = rec.expect("get_quadratic_approximation_of_cubic", cname);
        let got = bezier::get_quadratic_approximation_of_cubic(pts[0], pts[1], pts[2], pts[3]);
        close_points(&got, &Vec::<Point>::from_value(&want, cname), cname);
    }

    rec.assert_exhaustive(&["get_quadratic_approximation_of_cubic"]);
}

#[test]
fn bezier_remap_matches() {
    let fixture = Fixture::load("manim-bezier.json");
    let mut rec = Recorder::new(&fixture);
    let curves: Vec<Vec<Point>> = fixture.input("remap_input");

    for n in fixture.input::<Vec<usize>>("remap_targets") {
        let case = format!("2->{n}");
        let want = Vec::<Vec<Point>>::from_value(&rec.expect("bezier_remap", &case), &case);
        let got = bezier::bezier_remap(&curves, n).expect("equal-length curves");
        assert_eq!(got.len(), want.len(), "{case}: curve count");
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            close_points(g, w, &format!("{case}[{i}]"));
        }
    }

    rec.assert_exhaustive(&["bezier_remap"]);
}

/// Both handle-point solvers, plus the dispatcher that also covers the one- and
/// two-anchor cases neither solver reaches.
#[test]
fn smooth_handle_points_match() {
    let fixture = Fixture::load("manim-bezier.json");
    let mut rec = Recorder::new(&fixture);
    let anchors: BTreeMap<String, Vec<Point>> = fixture.input("anchors");

    /// The fixture's shape for a handle pair: two lists of points. The generator
    /// normalises Manim's bare-point return for the two-anchor case into that
    /// shape, so every case here looks the same.
    fn pair(v: &Value, what: &str) -> (Vec<Point>, Vec<Point>) {
        let lists = Vec::<Vec<Point>>::from_value(v, what);
        assert_eq!(lists.len(), 2, "{what}: expected a pair of handle lists");
        (lists[0].clone(), lists[1].clone())
    }

    for name in ["open4", "open6"] {
        let want = rec.expect("get_smooth_open_cubic_bezier_handle_points", name);
        let got = bezier::get_smooth_open_cubic_bezier_handle_points(&anchors[name])
            .expect("at least 3 anchors");
        let (w1, w2) = pair(&want, name);
        close_points(&got.0, &w1, &format!("open {name} H1"));
        close_points(&got.1, &w2, &format!("open {name} H2"));
    }

    for name in ["closed4", "closed6"] {
        let want = rec.expect("get_smooth_closed_cubic_bezier_handle_points", name);
        let got = bezier::get_smooth_closed_cubic_bezier_handle_points(&anchors[name])
            .expect("at least 3 anchors");
        let (w1, w2) = pair(&want, name);
        close_points(&got.0, &w1, &format!("closed {name} H1"));
        close_points(&got.1, &w2, &format!("closed {name} H2"));
    }

    // `one1` reads as an empty pair on both sides; `two2` is the straight line
    // whose handles sit at `1/3` and `2/3`, computed as
    // `(1 - alpha) * start + alpha * end`. That is a different floating-point
    // value from `start + alpha * (end - start)`, and nothing else in this suite
    // would notice.
    for name in ["one1", "two2", "open4", "open6", "closed4", "closed6"] {
        let want = rec.expect("get_smooth_cubic_bezier_handle_points", name);
        let got = bezier::get_smooth_cubic_bezier_handle_points(&anchors[name])
            .expect("at least 1 anchor");
        let (w1, w2) = pair(&want, name);
        close_points(&got.0, &w1, &format!("dispatch {name} H1"));
        close_points(&got.1, &w2, &format!("dispatch {name} H2"));
    }

    rec.assert_exhaustive(&[
        "get_smooth_open_cubic_bezier_handle_points",
        "get_smooth_closed_cubic_bezier_handle_points",
        "get_smooth_cubic_bezier_handle_points",
    ]);
}

// ---------------------------------------------------------------------------
// manim-geometry.json — arc length and critical_point, through a mobject
// ---------------------------------------------------------------------------

/// Every named path in the geometry fixture, in Manim's own point layout: the
/// polygons expanded through `set_points_as_corners`, and the genuinely curved
/// paths as they were stored.
fn geometry_paths(fixture: &Fixture) -> BTreeMap<String, Vec<Point>> {
    let corners: BTreeMap<String, Vec<Point>> = fixture.input("corner_paths");
    let cubics: BTreeMap<String, Vec<Point>> = fixture.input("cubic_paths");
    corners
        .into_iter()
        .map(|(n, c)| (n, bezier::points_as_corners(&c)))
        .chain(cubics)
        .collect()
}

#[test]
fn arc_length_matches() {
    let fixture = Fixture::load("manim-geometry.json");
    let mut rec = Recorder::new(&fixture);
    let samples: Vec<usize> = fixture.input("arc_samples");

    for (name, pts) in geometry_paths(&fixture) {
        // The no-argument call: the default of 10 chords has to be identified
        // rather than assumed. The curved paths are what make it visible — the
        // polygons are straight, so their length is the same at every sample
        // count and cannot distinguish an analytic arc length from a sampled one.
        let want = rec.expect("get_arc_length", &name);
        close(
            bezier::arc_length(&pts, bezier::DEFAULT_ARC_SAMPLES),
            f64::from_value(&want, &name),
            &format!("arc_length {name} (default samples)"),
        );

        for n in &samples {
            let case = format!("{name}@samples={n}");
            let want = rec.expect("get_arc_length", &case);
            close(
                bezier::arc_length(&pts, *n),
                f64::from_value(&want, &case),
                &case,
            );
        }
    }

    rec.assert_exhaustive(&["get_arc_length"]);
}

#[test]
fn critical_point_matches() {
    let fixture = Fixture::load("manim-geometry.json");
    let mut rec = Recorder::new(&fixture);
    let dirs: BTreeMap<String, Vec<f64>> = fixture.input("critical_directions");

    for (name, pts) in geometry_paths(&fixture) {
        // Manim takes the boundary of the *anchors*: interior control points are
        // excluded. The cusp path is what makes that visible — its control points
        // reach y = ±2 while its anchors sit on y = 0.
        let anchors = bezier::anchors(&pts).expect("a whole number of curves");
        for (dname, d) in &dirs {
            let case = format!("{name}@{dname}");
            let want = rec.expect("critical_point", &case);
            let got = bezier::critical_point(&anchors, (d[0], d[1]));
            close(
                got.x,
                f64::from_value(&want[0], &case),
                &format!("{case}.x"),
            );
            close(
                got.y,
                f64::from_value(&want[1], &case),
                &format!("{case}.y"),
            );
        }
    }

    rec.assert_exhaustive(&["critical_point"]);
}

/// `VMobject.pointwise_become_partial` — the partial path `Create` draws.
///
/// The interesting cases are not the interiors, which any split-and-join gets
/// roughly right, but the boundaries:
///
/// - `@0.0,0.5` on a two-curve path ends on the curve boundary, so the second
///   curve comes back **degenerate** onto the shared anchor rather than absent:
///   the array keeps eight points, which is what `nppc * (upper - lower + 1)`
///   allocates.
/// - `@0.0,1.0` is the copy shortcut, so the result is bit-identical to the
///   input rather than a re-evaluated Bézier.
/// - `@0.0,0.0` is the empty range `Uncreate` reaches on its last frame; it is
///   four copies of the first point.
#[test]
fn pointwise_partial_matches() {
    let fixture = Fixture::load("manim-geometry.json");
    let mut rec = Recorder::new(&fixture);
    let bounds: Vec<(f64, f64)> = fixture.input("partial_bounds");

    for (name, pts) in geometry_paths(&fixture) {
        for (a, b) in &bounds {
            let case = format!("{name}@{},{}", py_float(*a), py_float(*b));
            let want: Vec<Point> = Vec::from_value(&rec.expect("pointwise_partial", &case), &case);
            let got = bezier::partial_path_points(&pts, *a, *b);
            close_points(&got, &want, &case);
        }
    }

    rec.assert_exhaustive(&["pointwise_partial"]);
}

/// `VMobject.align_points` — one case per pair in the `align_pairs` input.
///
/// Four of the twelve are there because a plausible port goes wrong on them, and
/// knowing which is which is most of reading a failure here:
///
/// - `single_vs_single_shortcut` — two *one-point* paths with different points
///   come back untouched, at length one. The equal-count early return runs before
///   any normalisation, and upstream flags the shortcut as over-eager with a
///   `TODO`; the port reproduces it rather than tidying it up.
/// - `trailing_null_trim` / `trailing_null_double_trim` — the output is
///   **shorter** than the input, 8 → 4 and 12 → 4, because trailing curves that
///   fold onto the point before them are trimmed, repeatedly.
/// - `trailing_null_partial_no_trim` — and the trim needs *all four* of the four
///   points to match, not just the last one, or this pair would trim too.
/// - `rtol_boundary_no_split` — two anchors 0.5 apart on a coordinate of 1e6 stay
///   one subpath, because numpy's `allclose` scales its tolerance by the
///   coordinate. A port that kept only `atol` splits the path here and then
///   answers with a different subpath count and a different array length.
#[test]
fn align_points_matches() {
    let fixture = Fixture::load("manim-geometry.json");
    let mut rec = Recorder::new(&fixture);
    let pairs: BTreeMap<String, Vec<Vec<Point>>> = fixture.input("align_pairs");

    for (name, pair) in &pairs {
        assert_eq!(pair.len(), 2, "{name}: a pair of paths");
        let want: Vec<Vec<Point>> = Vec::from_value(&rec.expect("align_points", name), name);
        assert_eq!(want.len(), 2, "{name}: the fixture records both outputs");
        let (got_a, got_b) = bezier::align_points(&pair[0], &pair[1])
            .unwrap_or_else(|e| panic!("{name}: align_points refused a valid path: {e}"));
        // Both sides are compared, because the property the animation depends on
        // is that they agree with *each other* — and a port that padded only one
        // of them would still match on the side it got right.
        close_points(&got_a, &want[0], &format!("{name}: first"));
        close_points(&got_b, &want[1], &format!("{name}: second"));
    }

    rec.assert_exhaustive(&["align_points"]);
}

// ---------------------------------------------------------------------------
// manim-space-ops.json
// ---------------------------------------------------------------------------

/// `normalize`, `norm_squared`, `cross`, `cross2d`, `angle_between_vectors`,
/// `midpoint` and `center_of_mass`.
#[test]
fn vector_helpers_match() {
    let fixture = Fixture::load("manim-space-ops.json");
    let mut rec = Recorder::new(&fixture);
    let vecs: BTreeMap<String, Vec<f64>> = fixture.input("vectors");
    let v = |name: &str| -> Vec<f64> { vecs[name].clone() };
    let v3 = |name: &str| -> so::Vec3 {
        let a = &vecs[name];
        [a[0], a[1], a[2]]
    };

    let want = rec.expect("normalize", "plain");
    close_floats(
        &so::normalize(&v("a")),
        &Vec::<f64>::from_value(&want, "plain"),
        "normalize.plain",
    );

    let want = rec.expect("normalize", "already_unit");
    close_floats(
        &so::normalize(&v("x")),
        &Vec::<f64>::from_value(&want, "already_unit"),
        "normalize.already_unit",
    );

    // The zero vector returns the fallback *verbatim*, not normalised — which is
    // why the expected value here is `a` itself and not a unit vector.
    let want = rec.expect("normalize", "zero_with_fallback");
    close_floats(
        &so::normalize_or(&v("zero"), &v("a")),
        &Vec::<f64>::from_value(&want, "zero_with_fallback"),
        "normalize.zero_with_fallback",
    );

    let want = rec.expect("norm_squared", "plain");
    close(
        so::norm_squared(&v("a")),
        f64::from_value(&want, "norm_squared"),
        "norm_squared.plain",
    );

    let want = rec.expect("cross", "ab");
    close_floats(
        &so::cross(v3("a"), v3("b")),
        &Vec::<f64>::from_value(&want, "ab"),
        "cross.ab",
    );

    let want = rec.expect("cross", "xz");
    close_floats(
        &so::cross(v3("x"), v3("z")),
        &Vec::<f64>::from_value(&want, "xz"),
        "cross.xz",
    );

    // The generator inlines this 2D pair rather than naming it in `inputs`, so it
    // is repeated here; the expected value -2.0 is what makes the operands
    // visible.
    let want = rec.expect("cross2d", "plain");
    close(
        so::cross2d([1.0, 2.0], [3.0, 4.0]),
        f64::from_value(&want, "cross2d"),
        "cross2d.plain",
    );

    let a = v("a");
    let x = v("x");
    let opposite_x: Vec<f64> = x.iter().map(|c| -c).collect();
    for (case, first, second) in [
        ("ab", &a, v("b")),
        ("xz", &x, v("z")),
        ("opposite", &x, opposite_x),
    ] {
        let want = rec.expect("angle_between_vectors", case);
        close(
            so::angle_between_vectors(first, &second),
            f64::from_value(&want, case),
            &format!("angle_between_vectors.{case}"),
        );
    }

    let pair: Vec<Vec<f64>> = fixture.input("midpoint_pair");
    let want = rec.expect("midpoint", "3d");
    close_floats(
        &so::midpoint(
            [pair[0][0], pair[0][1], pair[0][2]],
            [pair[1][0], pair[1][1], pair[1][2]],
        ),
        &Vec::<f64>::from_value(&want, "3d"),
        "midpoint.3d",
    );

    let tri: Vec<Vec<f64>> = fixture.input("triangle_3d");
    let refs: Vec<&[f64]> = tri.iter().map(Vec::as_slice).collect();
    let want = rec.expect("center_of_mass", "triangle");
    close_floats(
        &so::center_of_mass(&refs).expect("equal-length points"),
        &Vec::<f64>::from_value(&want, "triangle"),
        "center_of_mass.triangle",
    );

    rec.assert_exhaustive(&[
        "normalize",
        "norm_squared",
        "cross",
        "cross2d",
        "angle_between_vectors",
        "midpoint",
        "center_of_mass",
    ]);
}

#[test]
fn rotation_matrices_match() {
    let fixture = Fixture::load("manim-space-ops.json");
    let mut rec = Recorder::new(&fixture);
    let axes: BTreeMap<String, Vec<f64>> = fixture.input("rotation_axes");
    let angles: Vec<f64> = fixture.input("rotation_angles");

    #[track_caller]
    fn check3(got: &so::Mat3, want: &[Vec<f64>], what: &str) {
        assert_eq!(got.len(), want.len(), "{what}: rows");
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            close_floats(g, w, &format!("{what}[row {i}]"));
        }
    }

    for (aname, axis) in &axes {
        let axis = [axis[0], axis[1], axis[2]];
        for angle in &angles {
            let case = format!("{aname}@{}", py_float(*angle));

            let want = rec.expect("rotation_matrix", &case);
            check3(
                &so::rotation_matrix(*angle, axis),
                &rows(&want, &case),
                &format!("rotation_matrix {case}"),
            );

            // `homogeneous=True`: the 4x4 form. The padding is part of what is
            // being pinned, not incidental.
            let hcase = format!("{case}@h");
            let want = rec.expect("rotation_matrix", &hcase);
            let want = rows(&want, &hcase);
            let got = so::rotation_matrix_homogeneous(*angle, axis);
            assert_eq!(got.len(), want.len(), "{hcase}: rows");
            for (i, (g, w)) in got.iter().zip(&want).enumerate() {
                close_floats(g, w, &format!("rotation_matrix {hcase}[row {i}]"));
            }

            // The transpose has its own code path for an axis along z, and that
            // is the one place the two branches are known to disagree by an ULP.
            // Covering it inside this loop is what exercises the path at all.
            let want = rec.expect("rotation_matrix_transpose", &case);
            check3(
                &so::rotation_matrix_transpose(*angle, axis),
                &rows(&want, &case),
                &format!("rotation_matrix_transpose {case}"),
            );
        }
    }

    rec.assert_exhaustive(&["rotation_matrix", "rotation_matrix_transpose"]);
}

#[test]
fn z_to_vector_matches() {
    let fixture = Fixture::load("manim-space-ops.json");
    let mut rec = Recorder::new(&fixture);
    let vecs: BTreeMap<String, Vec<f64>> = fixture.input("vectors");

    // `along_x` is the target direction being +x, where `cross(axis_z, RIGHT)`
    // vanishes and Manim rebuilds the frame from UP instead. Signed zeros appear
    // in these matrices and the absolute tolerance cannot see them — see the
    // module note.
    for (case, source) in [
        ("plain", "a"),
        ("already_z", "z"),
        ("minus_z", "minus_z"),
        ("along_x", "x"),
    ] {
        let want = rows(&rec.expect("z_to_vector", case), case);
        let a = &vecs[source];
        let got = so::z_to_vector([a[0], a[1], a[2]]);
        assert_eq!(got.len(), want.len(), "z_to_vector {case}: rows");
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            close_floats(g, w, &format!("z_to_vector {case}[row {i}]"));
        }
    }

    rec.assert_exhaustive(&["z_to_vector"]);
}

#[test]
fn line_intersection_matches() {
    let fixture = Fixture::load("manim-space-ops.json");
    let mut rec = Recorder::new(&fixture);
    // Each line is a pair of points, not a point and a direction: the fixture
    // values are only reproduced when both are treated as points on the line.
    let lines = fixture.input_map("lines").clone();

    for (case, line) in &lines {
        let want = rec.expect("line_intersection", case);
        let ends = line
            .as_array()
            .unwrap_or_else(|| panic!("{case}: expected two lines"))
            .iter()
            .map(|l| {
                let pts = l.as_array().expect("points");
                assert_eq!(pts.len(), 2, "{case}: expected two points per line");
                [to_vec3(&pts[0], case), to_vec3(&pts[1], case)]
            })
            .collect::<Vec<[so::Vec3; 2]>>();
        let got = so::line_intersection(ends[0], ends[1])
            .expect("both lines lie in the plane and are not parallel");
        close_floats(&got, &Vec::<f64>::from_value(&want, case), case);
    }

    rec.assert_exhaustive(&["line_intersection"]);
}

#[test]
fn find_intersection_matches() {
    let fixture = Fixture::load("manim-space-ops.json");
    let mut rec = Recorder::new(&fixture);
    let rays: Vec<Vec<Vec<f64>>> = fixture.input("rays");
    let ray = |i: usize| -> Vec<so::Vec3> { rays[i].iter().map(|p| [p[0], p[1], p[2]]).collect() };

    // Four rays as (start, end) pairs: the first two are the ones being crossed,
    // the last two the ones doing the crossing.
    let want = Vec::<Vec<f64>>::from_value(
        &rec.expect("find_intersection", "unit_square_edges"),
        "unit_square_edges",
    );
    let got = so::find_intersection(
        &ray(0),
        &ray(1),
        &ray(2),
        &ray(3),
        so::DEFAULT_INTERSECTION_THRESHOLD,
    )
    .expect("equal-length ray arrays");
    assert_eq!(got.len(), want.len(), "find_intersection: count");
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        close_floats(g, w, &format!("find_intersection[{i}]"));
    }

    rec.assert_exhaustive(&["find_intersection"]);
}

#[test]
fn polygon_helpers_match() {
    let fixture = Fixture::load("manim-space-ops.json");
    let mut rec = Recorder::new(&fixture);

    #[track_caller]
    fn direction_name(d: so::ShoelaceDirection) -> &'static str {
        match d {
            so::ShoelaceDirection::Cw => "CW",
            so::ShoelaceDirection::Ccw => "CCW",
        }
    }
    let input = |name: &str| -> Vec<so::Vec2> {
        as_vec2(
            fixture
                .inputs
                .get(name)
                .unwrap_or_else(|| panic!("fixture has no input {name:?}")),
            name,
        )
    };

    let square = input("square_closed");
    let square_reversed: Vec<so::Vec2> = square.iter().rev().copied().collect();

    // The sign is inverted relative to the textbook formula: `np.trapezoid(y, x)`
    // reads -1.0 for a counter-clockwise polygon, and `shoelace_direction`
    // compensates with its `area > 0` test.
    let want = rec.expect("shoelace", "ccw");
    close(
        so::shoelace(&square),
        f64::from_value(&want, "ccw"),
        "shoelace.ccw",
    );
    let want = rec.expect("shoelace", "cw");
    close(
        so::shoelace(&square_reversed),
        f64::from_value(&want, "cw"),
        "shoelace.cw",
    );

    let want = rec.expect("shoelace_direction", "ccw");
    assert_eq!(
        direction_name(so::shoelace_direction(&square)),
        want.as_str().expect("a direction string"),
        "shoelace_direction.ccw"
    );
    let want = rec.expect("shoelace_direction", "cw");
    assert_eq!(
        direction_name(so::shoelace_direction(&square_reversed)),
        want.as_str().expect("a direction string"),
        "shoelace_direction.cw"
    );

    // `get_winding_number` is not a winding number: it sums the change in each
    // *vertex's* polar angle about the origin, so a shape with a corner at the
    // origin reads 0.0 in either orientation. That case is here on purpose — a
    // port that "fixed" the oddity would still pass the other three.
    let diamond_open = input("diamond_open");
    let diamond_closed = input("diamond_closed");
    let diamond_reversed: Vec<so::Vec2> = diamond_open.iter().rev().copied().collect();

    for (case, pts) in [
        ("diamond_open_ccw", &diamond_open),
        ("diamond_open_cw", &diamond_reversed),
        ("diamond_closed_ccw", &diamond_closed),
        ("origin_corner_ccw", &square),
    ] {
        let want = rec.expect("get_winding_number", case);
        close(
            so::get_winding_number(pts),
            f64::from_value(&want, case),
            &format!("get_winding_number.{case}"),
        );
    }

    rec.assert_exhaustive(&["shoelace", "shoelace_direction", "get_winding_number"]);
}

#[test]
fn quaternions_and_spherical_match() {
    let fixture = Fixture::load("manim-space-ops.json");
    let mut rec = Recorder::new(&fixture);
    let vecs: BTreeMap<String, Vec<f64>> = fixture.input("vectors");
    let v3 = |name: &str| -> so::Vec3 {
        let a = &vecs[name];
        [a[0], a[1], a[2]]
    };

    let want = rec.expect("quaternion_from_angle_axis", "z_90");
    close_floats(
        &so::quaternion_from_angle_axis(half_pi(&fixture), v3("z"), false),
        &Vec::<f64>::from_value(&want, "z_90"),
        "quaternion_from_angle_axis.z_90",
    );

    let pair: Vec<Vec<f64>> = fixture.input("quaternion_mult_pair");
    let q = |i: usize| -> so::Quat { [pair[i][0], pair[i][1], pair[i][2], pair[i][3]] };
    let want = rec.expect("quaternion_mult", "identity");
    close_floats(
        &so::quaternion_mult(q(0), q(1)),
        &Vec::<f64>::from_value(&want, "identity"),
        "quaternion_mult.identity",
    );

    let want = rec.expect("rotate_vector", "z_90");
    close_floats(
        &so::rotate_vector(v3("x"), half_pi(&fixture), v3("z")),
        &Vec::<f64>::from_value(&want, "z_90"),
        "rotate_vector.z_90",
    );

    let want = rec.expect("cartesian_to_spherical", "plain");
    close_floats(
        &so::cartesian_to_spherical(v3("a")),
        &Vec::<f64>::from_value(&want, "plain"),
        "cartesian_to_spherical.plain",
    );

    let want = rec.expect("spherical_to_cartesian", "roundtrip");
    close_floats(
        &so::spherical_to_cartesian(so::cartesian_to_spherical(v3("a"))),
        &Vec::<f64>::from_value(&want, "roundtrip"),
        "spherical_to_cartesian.roundtrip",
    );

    rec.assert_exhaustive(&[
        "quaternion_from_angle_axis",
        "quaternion_mult",
        "rotate_vector",
        "cartesian_to_spherical",
        "spherical_to_cartesian",
    ]);
}

// ---------------------------------------------------------------------------
// the coverage guards
// ---------------------------------------------------------------------------

/// The case groups this file asserts. [`every_fixture_group_has_a_test`] compares
/// this against the fixtures' own keys, so a group added to `gen.py` and
/// regenerated cannot pass unnoticed: it fails here until it is named.
const BEZIER_GROUPS: &[&str] = &[
    "bezier",
    "split_bezier",
    "partial_bezier_points",
    "subdivide_bezier",
    "is_closed",
    "point_lies_on_bezier",
    "proportions_along_bezier_curve_for_point",
    "get_quadratic_approximation_of_cubic",
    "bezier_remap",
    "get_smooth_open_cubic_bezier_handle_points",
    "get_smooth_closed_cubic_bezier_handle_points",
    "get_smooth_cubic_bezier_handle_points",
];

const GEOMETRY_GROUPS: &[&str] = &[
    "get_arc_length",
    "critical_point",
    "align_points",
    "pointwise_partial",
];

const SPACE_OPS_GROUPS: &[&str] = &[
    "normalize",
    "norm_squared",
    "cross",
    "cross2d",
    "angle_between_vectors",
    "midpoint",
    "center_of_mass",
    "rotation_matrix",
    "rotation_matrix_transpose",
    "z_to_vector",
    "line_intersection",
    "find_intersection",
    "shoelace",
    "shoelace_direction",
    "get_winding_number",
    "quaternion_from_angle_axis",
    "quaternion_mult",
    "rotate_vector",
    "cartesian_to_spherical",
    "spherical_to_cartesian",
];

/// The case count of each file. A regeneration that silently drops cases is
/// otherwise invisible, because the suite would stay green on a smaller fixture.
#[test]
fn fixture_case_counts_are_stable() {
    for (file, expected) in [
        ("manim-bezier.json", 282usize),
        ("manim-geometry.json", 145),
        ("manim-space-ops.json", 68),
    ] {
        let fixture = Fixture::load(file);
        let total: usize = fixture.cases.values().map(BTreeMap::len).sum();
        assert_eq!(total, expected, "{file}: case count changed");
    }
}

#[test]
fn every_fixture_group_has_a_test() {
    for (file, claimed) in [
        ("manim-bezier.json", BEZIER_GROUPS),
        ("manim-geometry.json", GEOMETRY_GROUPS),
        ("manim-space-ops.json", SPACE_OPS_GROUPS),
    ] {
        let fixture = Fixture::load(file);
        let declared: BTreeSet<String> = claimed.iter().map(|g| g.to_string()).collect();
        let present = fixture.groups();
        let unclaimed: Vec<&String> = present.difference(&declared).collect();
        let missing: Vec<&String> = declared.difference(&present).collect();
        assert!(
            unclaimed.is_empty(),
            "{file}: {} case group(s) no test claims: {unclaimed:?}",
            unclaimed.len()
        );
        assert!(
            missing.is_empty(),
            "{file}: {missing:?} named here but absent from the fixture — \
             regenerate it or fix the list"
        );
    }
}
