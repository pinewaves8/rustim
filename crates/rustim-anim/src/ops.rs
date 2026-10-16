//! Concrete animations.
//!
//! Three of them, chosen to exercise the three shapes the design has to support:
//! an animation that drives a scalar ([`FadeIn`]), one that interpolates a
//! transform ([`MoveTo`]) — where the deferred transform pays off directly — and
//! one that interpolates *point arrays* ([`Transform`], the morph), which is what
//! puts Manim's `align_points` machinery to work.
//!
//! [`Wait`] is the fourth and has no shape at all: it writes nothing and only
//! occupies the clock, which is exactly what makes a pause an animation like any
//! other.

use rustim_math::{Affine2, Point, bezier};
use rustim_mobject::{Mobject, NodeId, SceneTree, Style};

use crate::{Animation, RateFunc, rate};

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

/// Morphs an object into another shape.
///
/// This is Manim's `Transform` (`animation/transform.py:58`), and the armature of
/// it is one sentence: **alignment happens once, in `begin`, and every frame
/// after is a pure elementwise interpolation against a frozen snapshot.**
/// `Transform.begin` (`:200-212`) copies the target, aligns the source *against
/// that copy* (`:211`), and `Animation.begin` (`animation.py:197-213`) then
/// freezes the source — after alignment, so the frozen start already has the
/// aligned point count. Per frame only `Mobject.interpolate`
/// (`mobject/mobject.py:3303-3305`) runs.
///
/// Two consequences of Manim's own design are worth naming before the port:
///
/// * **The target is a donor, not a scene object.** `self.target` is a free value
///   and is never inserted into the tree; only the *source's* id lives through
///   the animation. Manim does exactly this — a plain `Transform` is neither an
///   introducer nor a remover (`animation.py:229-241`, `:255-261`), so it adds
///   nothing and removes nothing, and `clean_up_from_scene` (`:219-222`) swaps
///   ids only for `ReplacementTransform`. In a tree keyed by [`NodeId`] that is
///   what makes "the target is not in the scene" expressible at all. The
///   node-to-node spelling the AI layer will want is
///   `Transform::new(a, tree.get(b).expect("target").clone())`, one line at the
///   call site.
/// * **Alignment is `bezier::align_points`, which can *shrink*.** Equal point
///   counts come back untouched — including the over-eager shortcut upstream
///   marks with a `TODO` — so the lerp below is not skipped for same-length
///   inputs, it simply has nothing to pad.
///
/// # The divergence from Manim, which is deliberate
///
/// Manim's points *are* world coordinates, so its lerp is a world-space lerp with
/// no transform in sight. Here [`Mobject::points`] is *local* beside the deferred
/// [`Mobject::transform`], so the pair `(points, transform)` moves together: the
/// aligned local points are lerped **and** the affine is lerped alongside them.
/// For a translate/scale delta that reproduces Manim's world-space result
/// exactly, and it is the only choice that keeps the O(1) deferred transform the
/// rest of the architecture is built on — materialising both sides to world
/// points and writing the result back with an identity transform would make a
/// `Transform` composed with a `MoveTo` impossible. The cost is that a **rotation
/// delta does not interpolate as a rotation**, because interpolating an affine
/// componentwise is not interpolating an angle; that is the same caveat
/// [`MoveTo`]'s doc carries, and for the same reason a `Rotate` deserves its own
/// animation type.
///
/// # Not ported, on purpose
///
/// * **Family and child alignment.** `Mobject.align_data`
///   (`mobject/mobject.py:3149-3156`) also runs `null_point_align`, pads child
///   counts with fully transparent placeholders (`align_submobjects`, `:3177`),
///   and recurses per immediate child pair. None of that is here: this aligns the
///   source's own point array only. Every shape in `rustim-mobject` today
///   (`shapes::square` / `circle` / `right_triangle`) is childless with all its
///   geometry in `points`, so a shape-to-shape morph is fully covered, but a
///   morph between two *grouped* mobjects would align only the top-level array.
/// * **`path_arc` / `path_func`.** Manim's default is a straight lerp anyway —
///   `path_arc = 0` is below `STRAIGHT_PATH_THRESHOLD` (`utils/paths.py:30`,
///   `:208-209`), so the default path is `straight_path()`. The seam is a single
///   function pointer, so curvature is additive later.
/// * **Sheen.** [`Style`] has no `sheen_direction` / `sheen_factor`; there is
///   nothing to interpolate.
///
/// A source whose point array is ragged (`len % 4 ∈ {2, 3}`) is refused by
/// `align_points`; `begin` has no error channel to report through, so the state
/// stays unset and every later `interpolate` is a no-op. Manim would raise here.
/// The refusal itself is pinned by `rustim-math`'s own tests, so this type only
/// has to not panic — the same posture as a command naming a deleted node.
#[derive(Debug, Clone)]
pub struct Transform {
    source: NodeId,
    /// The donor. Held by value because Manim holds a `target_copy` by value, and
    /// never mutated: `begin` aligns against a clone of it, so the caller's
    /// geometry survives the animation untouched.
    target: Mobject,
    start: Option<Start>,
    duration: f64,
}

/// What `begin` captured, once, never to be recomputed.
///
/// Both point arrays are stored *already aligned*, because the source's own array
/// is not written back at `begin` time — Manim's `align_data` mutates it in place,
/// but leaving the tree untouched until the first frame is both simpler and
/// invisible, since `interpolate(0.0)` rewrites the array wholesale on the very
/// next call.
#[derive(Debug, Clone)]
struct Start {
    from_points: Vec<Point>,
    to_points: Vec<Point>,
    from_transform: Affine2,
    from_opacity: f64,
    from_style: Style,
}

impl Transform {
    /// Morph the mobject at `source` into `target`.
    ///
    /// `target` is geometry and paint, not a scene object: it is not added to the
    /// tree, and the source ends up wearing its points, transform, opacity and
    /// style. See the type's doc for why that is the faithful reading of Manim.
    pub fn new(source: NodeId, target: Mobject) -> Self {
        Self {
            source,
            target,
            start: None,
            duration: 1.0,
        }
    }

    pub fn with_duration(mut self, seconds: f64) -> Self {
        self.duration = seconds;
        self
    }
}

impl Animation for Transform {
    fn duration(&self) -> f64 {
        self.duration
    }

    fn begin(&mut self, tree: &SceneTree) {
        let Some(m) = tree.get(self.source) else {
            return;
        };
        // One alignment, against the donor's points — Manim's `align_data`, and
        // the only place the two arrays are ever reconciled. `Err` leaves
        // `start` unset, which turns every later frame into a no-op.
        let Ok((from_points, to_points)) = bezier::align_points(&m.points, &self.target.points)
        else {
            return;
        };
        self.start = Some(Start {
            from_points,
            to_points,
            from_transform: m.transform,
            from_opacity: m.opacity,
            from_style: m.style,
        });
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        let Some(start) = &self.start else { return };
        let Some(m) = tree.get_mut(self.source) else {
            return;
        };

        // Assigned wholesale rather than zipped into the existing array: at
        // `alpha = 0` that is what grows a 3-curve triangle into the 4-curve shape
        // the target is expressed in. Manim assigns wholesale too
        // (`mobject.py:3303`).
        m.points = start
            .from_points
            .iter()
            .zip(start.to_points.iter())
            .map(|(a, b)| Point::new(lerp_scalar(a.x, b.x, alpha), lerp_scalar(a.y, b.y, alpha)))
            .collect();

        m.set_transform(lerp_affine(
            start.from_transform,
            self.target.transform,
            alpha,
        ));
        m.opacity = lerp_scalar(start.from_opacity, self.target.opacity, alpha);
        m.style = Style::lerp(start.from_style, self.target.style, alpha);
        m.dirty.style = true;
    }
}

/// A pause. It writes nothing; it only occupies the clock.
///
/// Manim's `Wait` (`animation/animation.py:584-638`), and `Scene.wait` is nothing
/// but `play(Wait(run_time=duration))` (`scene/scene.py:1242-1276`). That is the
/// whole reason this is an `Animation` rather than a scheduling special case: it
/// keeps a beat uniformly "a set of animations", so the driver needs no separate
/// notion of a gap, and "is something playing?" stays true *across* a pause rather
/// than flickering to "finished" between the beats on either side of it.
///
/// `rate_func` is `linear`, Manim's default for this type (`:612`). Nothing
/// observable follows from that — `interpolate` is empty, so every curve gives the
/// same result — but it is the choice upstream made, and pinning it with one
/// assertion is cheaper than leaving a future reader to work out whether the
/// omission was meant.
#[derive(Debug, Clone)]
pub struct Wait {
    duration: f64,
}

impl Wait {
    /// A pause of `seconds` of virtual time. Manim's default is 1.0
    /// (`DEFAULT_WAIT_TIME`, `constants.py:178`), which is also what
    /// `Scene.wait()` uses when called with no argument.
    pub fn new(seconds: f64) -> Self {
        Self { duration: seconds }
    }
}

impl Animation for Wait {
    fn duration(&self) -> f64 {
        self.duration
    }

    fn interpolate(&mut self, _alpha: f64, _tree: &mut SceneTree) {}

    fn rate_func(&self) -> RateFunc {
        rate::linear
    }
}

/// `manim.utils.bezier.interpolate` (`utils/bezier.py:1067`) — `(1 - t) * a + t * b`.
///
/// Written in Manim's own order rather than as `a + (b - a) * t`. The two differ
/// in the last bits, and reproducing Manim's *rounding path* rather than only its
/// formula is this project's rule for anything it ports.
fn lerp_scalar(a: f64, b: f64, t: f64) -> f64 {
    (1.0 - t) * a + t * b
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
    use rustim_mobject::shapes;

    /// A 3-curve / 12-point triangle: `shapes.rs:365` pins the count,
    /// `tree.rs:797-815` the 4-points-per-curve layout.
    fn triangle() -> Mobject {
        Mobject::new(shapes::right_triangle(3.0, 3.0))
    }

    /// A 4-curve / 16-point circle — the shape that forces the triangle to grow.
    fn circle() -> Mobject {
        Mobject::new(shapes::circle(1.6))
    }

    /// A 16-point square, for the pairs that need equal lengths on both sides.
    fn square() -> Mobject {
        Mobject::new(shapes::square(2.6, 2.6))
    }

    fn scene(m: Mobject) -> (SceneTree, NodeId) {
        let mut tree = SceneTree::new();
        let id = tree.insert(m);
        (tree, id)
    }

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

    /// **The guard that a morphing scene actually morphs.**
    ///
    /// `align_points` is the only thing that reconciles 12 points against 16, and
    /// it runs once, in `begin`. A port that forgot to call it, or that aligned
    /// only when the lengths already matched, would lerp the first 12 of 16 and
    /// leave the array at its original length — which still *draws*, so the only
    /// way to see it is to count.
    #[test]
    fn transform_aligns_once_and_the_source_grows_at_the_midpoint() {
        let (mut tree, id) = scene(triangle());
        assert_eq!(tree.get(id).unwrap().points.len(), 12, "3 curves to start");

        let mut anim = Transform::new(id, circle());
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        assert_eq!(
            tree.get(id).unwrap().points.len(),
            16,
            "the source must be padded to the target's curve count"
        );
    }

    /// The endpoints are exact, both of them, so a settled scene *is* its target
    /// rather than a near-miss of it.
    ///
    /// The pair here is deliberately equal-length (16 against 16): with a source
    /// that has to be padded, `alpha = 0` is the *aligned* source and not the
    /// original array, which would make the assertion restate the alignment
    /// rather than the lerp. Growth is the previous test's business.
    #[test]
    fn transform_endpoints_are_exact() {
        let (mut tree, id) = scene(square());
        let target = circle();
        let before = tree.get(id).unwrap().points.clone();

        let mut anim = Transform::new(id, target.clone());
        anim.begin(&tree);

        anim.interpolate(0.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().points, before);

        anim.interpolate(1.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().points, target.points);
        assert_eq!(tree.get(id).unwrap().style, target.style);
    }

    /// The affine is part of what morphs. Drop it and the shape changes in place
    /// instead of travelling — a defect the settled golden cannot see, because at
    /// the endpoint the source has *reached* the target's transform either way.
    #[test]
    fn transform_lerps_the_deferred_transform() {
        let (mut tree, id) = scene(triangle());
        let mut target = circle();
        target.transform = Affine2::translate((6.0, 0.0));

        let mut anim = Transform::new(id, target);
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        assert_eq!(
            tree.get(id).unwrap().transform,
            Affine2::translate((3.0, 0.0)),
            "halfway between the source's identity and the target's offset"
        );
    }

    /// The donor is read-only.
    ///
    /// Manim keeps a `target_copy` precisely so the caller's target survives the
    /// animation (`transform.py:201-203`); here taking the target by value makes
    /// that structural, so this test is cheap insurance against a refactor that
    /// "saves a clone" by aligning *into* `self.target` — which would leave the
    /// stored donor holding a padded form that no longer describes the shape the
    /// caller handed over. It passes trivially today; it is here to keep failing
    /// to be an option.
    #[test]
    fn transform_leaves_the_target_untouched() {
        let (mut tree, id) = scene(triangle());
        let target = circle();

        let mut anim = Transform::new(id, target.clone());
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);
        anim.interpolate(1.0, &mut tree);

        assert_eq!(anim.target.points, target.points);
        assert_eq!(anim.target.style, target.style);
        assert_eq!(anim.target.transform, target.transform);
    }

    /// `align_points` returns equal-length input **unchanged** before it does
    /// anything else — upstream's own `TODO` calls that shortcut over-eager, and
    /// step 1 reproduced it rather than tidying it. So a port that read "the
    /// arrays are already the same length" as "nothing to do, return early" would
    /// drop the lerp entirely on exactly the shapes it is commonest for.
    #[test]
    fn transform_of_equal_length_paths_still_lerps() {
        let (mut tree, id) = scene(circle());
        let mut target = circle();
        // Same length, different geometry: move every point, keeping 16 of them.
        for p in &mut target.points {
            p.x += 4.0;
        }

        let mut anim = Transform::new(id, target.clone());
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        let source_x = circle().points[0].x;
        let got = &tree.get(id).unwrap().points;
        assert_eq!(got.len(), 16);
        // The midpoint of `source_x` and `target.points[0].x`, compared with a
        // tolerance rather than for bit equality: `0.5 * a + 0.5 * b` is not
        // guaranteed to land on the nearest float to the exact average, and the
        // thing under test is that the lerp *ran*, not which side of a half-ulp
        // it landed on.
        let want = Point::new(source_x + 2.0, target.points[0].y);
        assert!(
            (got[0].x - want.x).abs() < 1e-12 && (got[0].y - want.y).abs() < 1e-12,
            "expected about {want:?}, got {:?}",
            got[0]
        );
        // ...and it really did move: this is the assertion a port that honoured
        // `align_points`'s equal-length early return as "nothing to do" fails.
        assert!((got[0].x - source_x).abs() > 1.0, "the lerp was skipped");
    }

    /// Colour and opacity travel with the geometry, and the final frame is the
    /// target's style *by construction* rather than by arithmetic.
    #[test]
    fn transform_lerps_style_and_opacity() {
        let (mut tree, id) = scene(triangle());
        tree.get_mut(id).unwrap().style = Style {
            fill: rustim_color::BLUE,
            fill_opacity: 0.25,
            stroke_width: 2.0,
            ..Style::default()
        };
        tree.get_mut(id).unwrap().opacity = 0.5;

        let mut target = circle();
        target.style = Style {
            fill: rustim_color::YELLOW,
            fill_opacity: 0.75,
            stroke_width: 10.0,
            ..Style::default()
        };
        target.opacity = 1.0;

        let mut anim = Transform::new(id, target.clone());
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        let mid = tree.get(id).unwrap();
        assert_eq!(mid.style.fill_opacity, 0.5);
        assert_eq!(mid.style.stroke_width, 6.0);
        assert_eq!(mid.opacity, 0.75);
        for i in 0..4 {
            let want =
                0.5 * rustim_color::BLUE.components[i] + 0.5 * rustim_color::YELLOW.components[i];
            assert_eq!(mid.style.fill.components[i], want, "channel {i}");
        }

        anim.interpolate(1.0, &mut tree);
        let end = tree.get(id).unwrap();
        assert_eq!(end.style, target.style);
        assert_eq!(end.opacity, target.opacity);
    }

    #[test]
    fn transform_marks_geometry_and_style_dirty() {
        let (mut tree, id) = scene(triangle());
        tree.get_mut(id).unwrap().dirty = Default::default();

        let mut anim = Transform::new(id, circle());
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        let dirty = tree.get(id).unwrap().dirty;
        assert!(dirty.geometry, "the point array was rewritten");
        assert!(dirty.style, "the paint may have moved");
    }

    /// A ragged source is refused by `align_points`, and `begin` has no error
    /// channel to report through — so this has to degrade to "nothing happens",
    /// not to a panic or a half-written array.
    #[test]
    fn transform_refuses_a_ragged_source_without_panicking() {
        let ragged = Mobject::new((0..6).map(|i| Point::new(i as f64, 0.0)).collect());
        let (mut tree, id) = scene(ragged.clone());

        let mut anim = Transform::new(id, circle());
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        assert_eq!(tree.get(id).unwrap().points, ragged.points);
        assert_eq!(tree.get(id).unwrap().transform, ragged.transform);
    }

    /// The same posture as `missing_node_is_not_a_panic`: a command may name a
    /// node that has since been deleted, and the AI layer can reach this.
    #[test]
    fn transform_is_a_no_op_on_a_missing_node() {
        let (mut tree, id) = scene(triangle());
        tree.clear();
        let mut anim = Transform::new(id, circle());
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);
    }
}
