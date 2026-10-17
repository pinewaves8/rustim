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
//! # Reduced motion, and why it is one branch
//!
//! [`Motion`] is consulted in exactly one place — [`Sequence::start_next`], where
//! a beat is handed to the clock — and **not** by each animation. A reduced beat
//! is not interpolated at all: every animation in it is driven to `alpha = 1.0`
//! once, and the beat then occupies its nominal length, `max(duration)` over its
//! members, the same total [`AnimationScheduler`] would have used
//! (`scene/scene.py:1184-1186`).
//!
//! Two consequences are worth stating, because both are load-bearing:
//!
//! * **A pause needs no special case.** [`Wait`] is a hold that writes nothing, so
//!   a script written with explicit waits degrades exactly as one written with
//!   animations does — which is why there is no `is_motion()` on [`Animation`]: at
//!   the granularity a preference can honestly act on, "motion" and "a pause" are
//!   the same shape (write your end state, then occupy time), and the beat is the
//!   unit.
//! * **The rhythm is unchanged.** Every beat boundary lands at the same virtual
//!   time in both modes, so a scene does not become a different scene — only the
//!   frames in between go away. A completed animation *is* its end state, which is
//!   Manim's own reading of it too: `Animation.finish` forces `interpolate(1)`
//!   (`animation/animation.py:216-227`).
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

pub mod composition;
pub mod ops;
pub mod sequence;

use rustim_mobject::{NodeId, SceneTree};

pub use composition::{AnimationGroup, DEFAULT_LAGGED_START_LAG_RATIO, Succession};
pub use ops::{
    CameraMove, Create, FadeIn, FadeOut, MoveTo, Rotate, Scale, Transform, Uncreate, Wait,
};
pub use rustim_math::rate::{self, RateFunc};
pub use sequence::Sequence;

/// How much of a scene's motion to actually play.
///
/// The engine's answer to the platform "reduce motion" preference, and the two
/// values are the whole vocabulary: a scene is either interpolated or it is not.
/// It lives here, next to the timeline, because the timeline is the only thing
/// that reads it — see the module doc for the rule and for why there is no
/// per-animation opt-out. A host reads the preference (the platform APIs are in
/// `rustim-app`'s `motion` module, since a preference is a platform question and
/// this crate is part of the wasm kernel) and hands the answer to
/// `SceneContext`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Motion {
    /// Every frame is drawn: a beat is interpolated across its duration.
    #[default]
    Full,
    /// A beat is not interpolated — it lands on its end state and holds for its
    /// nominal length.
    Reduced,
}

impl Motion {
    /// True for [`Motion::Reduced`].
    ///
    /// A named question rather than a `matches!` at each use, because the call
    /// site should read as the *policy* ("if motion is reduced, ...") and not as a
    /// comparison against an enum variant — this is the one branch the whole
    /// feature is.
    pub fn is_reduced(self) -> bool {
        matches!(self, Motion::Reduced)
    }
}

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

    /// Called exactly once, **after** the last `interpolate`, when the animation
    /// has reached its end.
    ///
    /// The hook Manim calls `clean_up_from_scene` (`animation/animation.py:229-241`),
    /// which is what makes `FadeOut` actually remove its object (`fading.py:184-192`)
    /// rather than leaving a transparent one behind. Most animations have nothing
    /// to do here, so the default is a no-op.
    ///
    /// Two things about the contract are load-bearing:
    ///
    /// * **It runs only for animations that *complete*.** A script dropped by
    ///   [`Sequence::clear`] or [`AnimationScheduler::clear`] leaves its objects in
    ///   the tree — "the scene was dismantled" is not "the animation finished", and
    ///   a half-removed scene is worse than an un-removed one.
    /// * **It must be forwarded by the blanket `impl Animation for Box<T>`.** Both
    ///   containers here hold `Box<dyn Animation>`, so a non-forwarding impl sends
    ///   every call to this default and the removal silently never happens through
    ///   the two routes that matter — while a direct call on a concrete animation
    ///   still works, which is what makes it expensive to find. See the impl below.
    fn finish(&mut self, _tree: &mut SceneTree) {}

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

    /// Forwarded, and this one is not a formality like the others.
    ///
    /// `duration`, `begin`, `interpolate` and `rate_func` all have to be here or
    /// the box would not work at all; `finish` is different, because it has a
    /// *default*. Omit the forward and `Box<dyn Animation>::finish` resolves to
    /// the trait's no-op, which type-checks, runs, and silently skips the one
    /// thing the hook exists for — through the scheduler and through
    /// `Sequence`, the only two places that drive a boxed animation. That is why
    /// `fade_out_removes_its_target_when_the_scheduler_finishes_it` goes through
    /// `AnimationScheduler::add` rather than calling `finish` directly.
    fn finish(&mut self, tree: &mut SceneTree) {
        (**self).finish(tree)
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
        // Completion, then the sweep. The predicate is the exact complement of
        // `retain`'s, so an animation's `finish` runs on the same tick it leaves
        // the list — once, after an `interpolate(1.0)` it can rely on, and for a
        // zero-duration animation on its very first tick (`raw` is forced to 1.0
        // above, and `0.0 < 0.0` is already false for `retain`).
        //
        // `finish` may remove nodes from `tree`; `running` is a different field, so
        // the two borrows are disjoint and the walk stays valid.
        for r in &mut self.running {
            if self.elapsed - r.started_at >= r.anim.duration() {
                r.anim.finish(tree);
            }
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
                seq.start_next(&mut sched, &mut tree, Motion::Full);
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

    // ---- the `finish` hook ------------------------------------------------

    /// Records *when* `finish` ran, as the number of alphas seen by then.
    ///
    /// The contract is about order and count rather than about the hook
    /// existing, so a boolean would not be able to fail the interesting cases —
    /// `finish` running before the last frame, or running on every frame after
    /// the first.
    /// Shared with the test, which reads what the animation recorded.
    type Alphas = std::rc::Rc<std::cell::RefCell<Vec<f64>>>;
    type Finishes = std::rc::Rc<std::cell::RefCell<Vec<usize>>>;

    struct Lifecycle {
        alphas: Alphas,
        finishes: Finishes,
        duration: f64,
    }

    impl Lifecycle {
        fn new(duration: f64) -> (Self, Alphas, Finishes) {
            let alphas: Alphas = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let finishes: Finishes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            (
                Self {
                    alphas: alphas.clone(),
                    finishes: finishes.clone(),
                    duration,
                },
                alphas,
                finishes,
            )
        }
    }

    impl Animation for Lifecycle {
        fn duration(&self) -> f64 {
            self.duration
        }
        fn interpolate(&mut self, alpha: f64, _tree: &mut SceneTree) {
            self.alphas.borrow_mut().push(alpha);
        }
        fn finish(&mut self, _tree: &mut SceneTree) {
            self.finishes.borrow_mut().push(self.alphas.borrow().len());
        }
        fn rate_func(&self) -> RateFunc {
            rate::linear
        }
    }

    /// `finish` runs **once**, on the same tick the animation leaves, and after an
    /// `interpolate(1.0)` it can rely on.
    ///
    /// All three parts matter and each has its own counterfactual: running before
    /// the last frame means "the object is removed, then drawn at full opacity";
    /// running every frame after the end would attempt the removal repeatedly;
    /// never running is the `Box<dyn Animation>` trap the blanket impl's doc names.
    #[test]
    fn finish_runs_once_and_after_the_last_interpolate() {
        let (anim, alphas, finishes) = Lifecycle::new(0.5);
        let mut sched = AnimationScheduler::new();
        // Through `add`, so the call reaches `Box<dyn Animation>::finish` — the
        // only route a scene has.
        sched.add(anim);
        let (mut tree, _) = scene_with_cube();
        for _ in 0..40 {
            sched.update(1.0 / 60.0, &mut tree);
        }

        let alphas = alphas.borrow();
        let finishes = finishes.borrow();
        assert_eq!(
            finishes.len(),
            1,
            "exactly one completion, not one per frame"
        );
        assert_eq!(finishes[0], alphas.len(), "and after the last interpolate");
        assert_eq!(*alphas.last().unwrap(), 1.0, "which is the end state");
        assert!(sched.is_empty());
    }

    #[test]
    fn finish_is_not_called_while_the_animation_runs() {
        let (anim, alphas, finishes) = Lifecycle::new(1.0);
        let mut sched = AnimationScheduler::new();
        sched.add(anim);
        let (mut tree, _) = scene_with_cube();
        for _ in 0..30 {
            sched.update(1.0 / 60.0, &mut tree);
        }
        assert!(!alphas.borrow().is_empty(), "it really is running");
        assert!(!sched.is_empty());
        assert!(
            finishes.borrow().is_empty(),
            "half a second in, nothing has completed"
        );
    }

    /// The scheduler forces `raw` to `1.0` for a zero-duration animation rather
    /// than dividing by zero, and the completion predicate is that same
    /// comparison — so this reaches its end on the very first tick and must be
    /// finished there. A `Wait::new(0.0)`-style beat is the ordinary way this
    /// arises.
    #[test]
    fn a_zero_duration_animation_is_finished_too() {
        let (anim, alphas, finishes) = Lifecycle::new(0.0);
        let mut sched = AnimationScheduler::new();
        sched.add(anim);
        let (mut tree, _) = scene_with_cube();

        sched.update(1.0 / 60.0, &mut tree);

        assert_eq!(alphas.borrow().as_slice(), &[1.0]);
        assert_eq!(
            finishes.borrow().as_slice(),
            &[1],
            "finished on that same tick"
        );
        assert!(sched.is_empty());
    }

    /// "The scene was dismantled" is not "the animation finished".
    ///
    /// A dropped script must leave its objects where they are: half-removing a
    /// scene when its timeline is torn down would make a reset depend on how far
    /// the script happened to have got.
    #[test]
    fn clearing_the_scheduler_does_not_finish() {
        let (anim, _, finishes) = Lifecycle::new(2.0);
        let mut sched = AnimationScheduler::new();
        sched.add(anim);
        let (mut tree, _) = scene_with_cube();
        for _ in 0..10 {
            sched.update(1.0 / 60.0, &mut tree);
        }

        sched.clear();

        assert!(sched.is_empty());
        assert!(
            finishes.borrow().is_empty(),
            "a teardown is not a completion"
        );
    }
}
