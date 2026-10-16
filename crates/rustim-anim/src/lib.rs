//! Rate functions, the `Animation` trait, and timeline evaluation.
//!
//! Manim's animation model, which we keep, is that an animation is a *pure
//! function of progress*: given `alpha` in `0..=1` it writes a state. It owns no
//! clock. Time lives in the scheduler, and the easing curve lives inside the
//! animation (`方案建议.md` §4.4).
//!
//! Three layers, and the split is the whole timeline design:
//!
//! * [`Animation`] — one state change, a pure function of `alpha`.
//! * [`AnimationScheduler`] — the animations playing *now*, in parallel, on one
//!   shared clock. This is Manim's `Scene.play(*args)`.
//! * [`Sequence`] — the beats still to come, one after another. This is Manim's
//!   `construct()` body, written down as data because our loop cannot block.
//!
//! # The tutorial's lifetime problem, resolved
//!
//! The tutorial sketches `Create<'a, T>` and `MoveTo<'a, T>` holding
//! `&'a mut T`, then tries to put them in a `Vec<Box<dyn Animation>>`, which
//! cannot compile: a non-`'static` borrow cannot be boxed into a trait object
//! that outlives the frame (`CLAUDE.md`, "Known issues"). Of the two fixes the
//! tutorial lists, we take the second: **animations carry a [`NodeId`] and
//! mutate through the scene tree**.
//!
//! That choice is not just about lifetimes. It is the same decision the AI
//! command layer needs (§4.5): an animation and a JSON command should both be
//! *data naming a node*, so that a command can start, inspect, or cancel an
//! animation without holding a Rust reference to it.

pub mod ops;
pub mod sequence;

use rustim_mobject::{NodeId, SceneTree};

pub use ops::{FadeIn, MoveTo, Transform, Wait};
pub use rustim_math::rate::{self, RateFunc};
pub use sequence::Sequence;

/// A state change that plays out over a duration.
pub trait Animation {
    /// Length in seconds of virtual time.
    fn duration(&self) -> f64;

    /// Capture whatever initial state this animation needs from the tree.
    ///
    /// Called exactly once, when the animation starts. Animations that are
    /// defined by a *delta* (move by 2 units) or that need to blend from
    /// wherever the object happens to be cannot know their start state at
    /// construction time, because construction may happen arbitrarily long
    /// before playback. The tutorial has no hook for this.
    fn begin(&mut self, _tree: &SceneTree) {}

    /// Write the state for progress `alpha`, which is always in `0.0..=1.0`.
    ///
    /// Easing is applied *inside* the implementation, not by the scheduler.
    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree);

    /// The easing curve applied to raw progress before `interpolate`.
    fn rate_func(&self) -> RateFunc {
        rate::ease_in_out
    }

    /// Apply a rate function, mapping raw progress to eased progress.
    fn ease(&self, raw: f64) -> f64 {
        (self.rate_func())(rate::clamp01(raw))
    }
}

/// A boxed animation is an animation.
///
/// Not decoration: it is what lets a beat of *mixed* animation types be written
/// down. An array literal's element type is settled from its contents before any
/// bound is consulted, so `[Box::new(FadeIn::new(a)), Box::new(MoveTo::new(b, t))]`
/// has two types and coerces to neither — no signature makes that line compile.
/// With this impl the mixed beat is spelled with the element type written once:
///
/// ```ignore
/// let beat: Vec<Box<dyn Animation>> = vec![Box::new(FadeIn::new(a)), Box::new(MoveTo::new(b, t))];
/// ctx.play_all(beat);
/// ```
///
/// which works precisely because `Box<dyn Animation>` now satisfies `Animation`.
/// A homogeneous beat needs none of that: `ctx.play_all([FadeIn::new(a), FadeIn::new(b)])`.
impl<T: Animation + ?Sized> Animation for Box<T> {
    fn duration(&self) -> f64 {
        (**self).duration()
    }

    fn begin(&mut self, tree: &SceneTree) {
        (**self).begin(tree)
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        (**self).interpolate(alpha, tree)
    }

    fn rate_func(&self) -> RateFunc {
        (**self).rate_func()
    }
}

/// A running animation plus the scheduler's own clock reading for it.
struct Running {
    anim: Box<dyn Animation>,
    started: bool,
    /// The scheduler's clock when this animation was queued.
    ///
    /// Progress is read as `elapsed - started_at`, so an animation added to a
    /// batch already in flight starts from its own zero rather than from wherever
    /// the batch happens to be. See [`AnimationScheduler::add`] for why that is
    /// not a refinement.
    started_at: f64,
}

/// Plays animations in parallel.
///
/// One shared clock, each animation computing its progress independently from its
/// own duration — so animations of different lengths play together and the
/// shortest one simply finishes first. That is Manim's behaviour and it is worth
/// preserving exactly: `Scene.play(*args)` is one flat list whose total is
/// `max(run_time)` (`scene/scene.py:1184-1186`), with every child advanced by
/// `alpha = t / animation.run_time` (`:1714-1724`).
#[derive(Default)]
pub struct AnimationScheduler {
    running: Vec<Running>,
    /// Seconds of virtual time since this scheduler was created or cleared.
    elapsed: f64,
}

impl AnimationScheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue an animation. It starts on the next [`Self::update`].
    ///
    /// Requires `'static` because animations are held in a `Vec` that outlives
    /// the call — which is precisely why they address nodes by id instead of
    /// borrowing them.
    pub fn add(&mut self, anim: impl Animation + 'static) {
        self.add_boxed(Box::new(anim));
    }

    /// [`Self::add`] for an animation that is already boxed.
    ///
    /// Exists for [`Sequence::start_next`], which moves a whole beat across; the
    /// alternative is an iterator of `impl Animation` with one concrete type per
    /// call, and a beat is exactly where two animations of *different* types have
    /// to sit together.
    pub fn add_boxed(&mut self, anim: Box<dyn Animation>) {
        self.running.push(Running {
            anim,
            started: false,
            started_at: self.elapsed,
        });
    }

    /// Advance virtual time by `dt` and write the resulting state into `tree`.
    ///
    /// `dt` is virtual, never wall-clock: the frame loop in §4.3 uses a fixed
    /// timestep so that a slow frame changes how long the scene takes to play,
    /// but never *what it looks like* at a given moment.
    pub fn update(&mut self, dt: f64, tree: &mut SceneTree) {
        self.elapsed += dt;
        for r in &mut self.running {
            if !r.started {
                r.anim.begin(tree);
                r.started = true;
            }
        }
        // Progress is computed against the clock, not accumulated, so a large dt
        // cannot drift the phase of an animation.
        for r in &mut self.running {
            let d = r.anim.duration();
            let age = self.elapsed - r.started_at;
            let raw = if d <= 0.0 { 1.0 } else { age / d };
            let alpha = r.anim.ease(raw);
            r.anim.interpolate(alpha, tree);
        }
        self.running
            .retain(|r| self.elapsed - r.started_at < r.anim.duration());
    }

    /// True when nothing is left to play.
    pub fn is_empty(&self) -> bool {
        self.running.is_empty()
    }

    pub fn len(&self) -> usize {
        self.running.len()
    }

    /// Drop every queued animation. Used when the scene is reset.
    pub fn clear(&mut self) {
        self.running.clear();
        self.elapsed = 0.0;
    }

    pub fn elapsed(&self) -> f64 {
        self.elapsed
    }
}

/// A scene's node ids, in the order an animation should visit them.
///
/// Kept as a hook for the animations that genuinely address *two nodes* — an
/// id-swapping `ReplacementTransform`, or a `LaggedStart` over a run of ids.
///
/// It is deliberately **not** what [`Transform`] takes, which is the one thing
/// this alias was originally written for. Reading `animation/transform.py`
/// settled it the other way: Manim's target is a donor `Mobject` that is never
/// inserted and never touched (see [`Transform`]'s doc), so the honest pair is
/// `(NodeId, Mobject)` — a node and a value, not two nodes.
pub type NodePair = (NodeId, NodeId);

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_math::{Affine2, Point};
    use rustim_mobject::Mobject;

    /// Records the alphas it was driven with, so tests can inspect the clock.
    struct Recorder {
        seen: std::rc::Rc<std::cell::RefCell<Vec<f64>>>,
        duration: f64,
    }

    impl Animation for Recorder {
        fn duration(&self) -> f64 {
            self.duration
        }
        fn interpolate(&mut self, alpha: f64, _tree: &mut SceneTree) {
            self.seen.borrow_mut().push(alpha);
        }
        fn rate_func(&self) -> RateFunc {
            rate::linear
        }
    }

    fn scene_with_cube() -> (SceneTree, NodeId) {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(vec![Point::new(0.0, 0.0)]));
        (tree, id)
    }

    #[test]
    fn scheduler_reaches_alpha_one_and_then_empties() {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut sched = AnimationScheduler::new();
        sched.add(Recorder {
            seen: seen.clone(),
            duration: 1.0,
        });
        let (mut tree, _) = scene_with_cube();

        for _ in 0..70 {
            sched.update(1.0 / 60.0, &mut tree);
        }
        let alphas = seen.borrow();
        assert!((alphas.last().copied().unwrap() - 1.0).abs() < 1e-9);
        assert!(sched.is_empty(), "finished animations must be dropped");
    }

    #[test]
    fn animations_of_different_lengths_share_one_clock() {
        let short = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let long = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut sched = AnimationScheduler::new();
        sched.add(Recorder {
            seen: short.clone(),
            duration: 0.5,
        });
        sched.add(Recorder {
            seen: long.clone(),
            duration: 2.0,
        });
        let (mut tree, _) = scene_with_cube();

        // 0.25 s in: the short one is half done, the long one an eighth.
        sched.update(0.25, &mut tree);
        assert!((short.borrow()[0] - 0.5).abs() < 1e-9);
        assert!((long.borrow()[0] - 0.125).abs() < 1e-9);
    }

    #[test]
    fn zero_duration_animation_completes_immediately() {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut sched = AnimationScheduler::new();
        sched.add(Recorder {
            seen: seen.clone(),
            duration: 0.0,
        });
        let (mut tree, _) = scene_with_cube();
        sched.update(1.0 / 60.0, &mut tree);
        assert_eq!(seen.borrow()[0], 1.0);
    }

    #[test]
    fn move_to_captures_its_origin_at_begin() {
        let (mut tree, id) = scene_with_cube();
        let target = Affine2::translate((10.0, 0.0));

        // Construct the animation *before* the object moves.
        let mut anim = MoveTo::new(id, target);
        tree.get_mut(id)
            .unwrap()
            .set_transform(Affine2::translate((2.0, 0.0)));
        // `begin` sees the moved state, so the delta starts from where it is.
        anim.begin(&tree);

        anim.interpolate(0.0, &mut tree);
        let start = tree.get(id).unwrap().transform;
        anim.interpolate(1.0, &mut tree);
        let end = tree.get(id).unwrap().transform;

        assert_eq!(start, Affine2::translate((2.0, 0.0)));
        assert_eq!(end, target);
    }

    #[test]
    fn fade_in_drives_opacity_from_zero_to_one() {
        let (mut tree, id) = scene_with_cube();
        let mut anim = FadeIn::new(id);
        anim.interpolate(0.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().opacity, 0.0);
        anim.interpolate(1.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().opacity, 1.0);
    }

    /// The whole step, driven the way a scene drives it rather than by calling
    /// `interpolate` by hand: queue it, tick past its duration, and the source is
    /// wearing the target.
    ///
    /// The hand-called unit tests in `ops.rs` pin the *shape* of each frame; this
    /// one pins that the scheduler actually reaches `alpha == 1.0` before it
    /// `retain`s the animation away (`lib.rs:116`). If it dropped a frame early,
    /// every unit test would still pass and a scene would settle one tick short of
    /// the target — a defect no still frame can see.
    #[test]
    fn a_queued_transform_reaches_its_target() {
        let mut tree = SceneTree::new();
        let source = tree.insert(Mobject::new(rustim_mobject::shapes::right_triangle(
            3.0, 3.0,
        )));
        let target = Mobject::new(rustim_mobject::shapes::circle(1.6));

        let mut sched = AnimationScheduler::new();
        sched.add(Transform::new(source, target.clone()));

        for _ in 0..70 {
            sched.update(1.0 / 60.0, &mut tree);
        }

        assert!(sched.is_empty(), "a finished transform must be dropped");
        let got = &tree.get(source).unwrap().points;
        assert_eq!(got.len(), target.points.len());
        for (a, b) in got.iter().zip(target.points.iter()) {
            assert!(
                (a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9,
                "settled at {a:?}, target is {b:?}"
            );
        }
    }

    /// **The guard for the scheduler's per-animation start offset.**
    ///
    /// Every scene in the workspace calls `animations.update(dt, ..)` and *then*
    /// `add`s, so before `started_at` existed the clock had always already been
    /// advanced by one `dt` before the batch existed: the first frame of every
    /// animation was delivered one frame too far along its own curve. No still
    /// frame can see that (the end is clamped to 1.0 either way and the goldens
    /// are captured ten seconds in), which is exactly why it is pinned here.
    ///
    /// The counterfactual is stated in the last assertion: without the offset the
    /// late animation's first `alpha` would be `31/60`, not `1/60`.
    #[test]
    fn an_animation_added_late_starts_from_its_own_zero() {
        let early = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let late = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut sched = AnimationScheduler::new();
        let (mut tree, _) = scene_with_cube();

        sched.add(Recorder {
            seen: early.clone(),
            duration: 1.0,
        });
        for _ in 0..30 {
            sched.update(1.0 / 60.0, &mut tree);
        }
        assert!((early.borrow().last().copied().unwrap() - 0.5).abs() < 1e-12);

        sched.add(Recorder {
            seen: late.clone(),
            duration: 1.0,
        });
        sched.update(1.0 / 60.0, &mut tree);

        let late = late.borrow();
        assert!(
            (late[0] - 1.0 / 60.0).abs() < 1e-12,
            "a late animation starts at its own zero, got {} (31/60 would mean the \
             offset is gone)",
            late[0]
        );
        assert!(late[0] < 31.0 / 60.0);
    }

    /// A pause is an animation, and it occupies exactly its own duration — the
    /// property that makes `SceneContext::wait` a `play` with a `Wait` in it rather
    /// than a second kind of step for the driver to know about.
    #[test]
    fn wait_holds_the_clock_without_touching_the_tree() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(rustim_mobject::shapes::square(2.0, 2.0)));
        let before = tree.get(id).unwrap().clone();

        let mut sched = AnimationScheduler::new();
        sched.add(Wait::new(0.5));

        let mut ticks = 0;
        while !sched.is_empty() {
            sched.update(1.0 / 60.0, &mut tree);
            ticks += 1;
            assert!(ticks < 100, "a pause must end on its own");
        }
        // Half a second at 1/60 s per frame, to within the frame that carries it
        // past the end: 30 ticks of `1/60` sum to just *under* 0.5 in f64, so the
        // 31st is the one that drains. The range is what makes this a real
        // assertion — a `Wait` that reported the wrong duration (or zero) would
        // drain outside it rather than inside.
        assert!(
            (0.5..0.5 + 1.0 / 60.0).contains(&sched.elapsed()),
            "a pause lasts its own duration; drained at {}",
            sched.elapsed()
        );
        assert!((30..=31).contains(&ticks), "took {ticks} ticks");

        let after = tree.get(id).unwrap();
        assert_eq!(after.points, before.points);
        assert_eq!(after.transform, before.transform);
        assert_eq!(after.opacity, before.opacity);
        assert_eq!(after.style, before.style);
    }

    /// Manim's `Wait` defaults to `linear` (`animation.py:612`) rather than to the
    /// `ease_in_out` everything else inherits. Nothing follows from it today — a
    /// `Wait` writes nothing — but a default that only *looks* arbitrary is one a
    /// later reader will "simplify" away, so it is pinned.
    #[test]
    fn wait_uses_linear_so_a_pause_is_a_pause() {
        assert_eq!(Wait::new(1.0).ease(0.25), 0.25);
        // The curve it is *not* using, so the assertion above cannot pass by luck.
        assert_eq!(rate::ease_in_out(0.25), 0.125);
    }

    /// The whole point of the three layers, driven the way a scene drives them: a
    /// beat queued behind another does not start until the first has drained, and
    /// the sequence plus the scheduler together carry the scene to its final state.
    #[test]
    fn a_sequence_driven_through_the_scheduler_settles_the_scene() {
        let mut tree = SceneTree::new();
        let source = tree.insert(Mobject::new(rustim_mobject::shapes::right_triangle(
            3.0, 3.0,
        )));
        let target = Mobject::new(rustim_mobject::shapes::circle(1.6));

        let mut seq = Sequence::new();
        // A `FadeIn` fades from wherever the object *is* (see its `begin`) and a
        // fresh mobject is opaque, so without this the fade is a no-op and the whole
        // test measures nothing.
        tree.get_mut(source).unwrap().opacity = 0.0;
        let fade: Vec<Box<dyn Animation>> = vec![Box::new(FadeIn::new(source).with_duration(0.3))];
        let morph: Vec<Box<dyn Animation>> = vec![Box::new(Transform::new(source, target.clone()))];
        seq.push(fade);
        seq.push(morph);

        let mut sched = AnimationScheduler::new();
        let mut frames = 0;
        let (mut faded_at, mut morphed_at) = (None, None);
        // The order `SceneContext::advance` uses: start the next beat, then advance.
        while !seq.is_empty() || !sched.is_empty() {
            if sched.is_empty() {
                seq.start_next(&mut sched);
            }
            sched.update(1.0 / 60.0, &mut tree);
            frames += 1;
            assert!(frames < 1000, "the sequence never drained");

            // Alignment rewrites the point array wholesale, so "16 points" is the
            // morph having begun and "12" is it not having begun. Comparing the
            // two frame numbers rather than hard-coding one is what makes this
            // immune to a float-accumulation off-by-one in the clock.
            let m = tree.get(source).unwrap();
            if faded_at.is_none() && m.opacity == 1.0 {
                faded_at = Some(frames);
            }
            if morphed_at.is_none() && m.points.len() == 16 {
                morphed_at = Some(frames);
            }
        }
        let (faded_at, morphed_at) = (faded_at.unwrap(), morphed_at.unwrap());
        assert!(faded_at > 1, "the fade took more than one frame to finish");
        assert!(
            morphed_at > faded_at,
            "the second beat began at {morphed_at}, during the first (which finished \
             at {faded_at})"
        );

        assert_eq!(tree.get(source).unwrap().opacity, 1.0);
        let got = &tree.get(source).unwrap().points;
        assert_eq!(got.len(), target.points.len());
        for (a, b) in got.iter().zip(target.points.iter()) {
            assert!((a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9);
        }
    }

    #[test]
    fn missing_node_is_not_a_panic() {
        let mut tree = SceneTree::new();
        let ghost = tree.insert(Mobject::new(vec![]));
        tree.clear();
        // A command may name a node that has since been deleted; that must be a
        // no-op, not a crash, because the AI layer can reach this.
        let mut anim = FadeIn::new(ghost);
        anim.interpolate(0.5, &mut tree);
    }
}
