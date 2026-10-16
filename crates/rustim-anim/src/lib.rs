//! Rate functions, the `Animation` trait, and timeline evaluation.
//!
//! Manim's animation model, which we keep, is that an animation is a *pure
//! function of progress*: given `alpha` in `0..=1` it writes a state. It owns no
//! clock. Time lives in the scheduler, and the easing curve lives inside the
//! animation (`方案建议.md` §4.4).
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

use rustim_mobject::{NodeId, SceneTree};

pub use ops::{FadeIn, MoveTo};
pub use rustim_math::rate::{self, RateFunc};

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

/// A running animation plus the scheduler's own clock reading for it.
struct Running {
    anim: Box<dyn Animation>,
    started: bool,
}

/// Plays animations in parallel.
///
/// One shared `start_time`, each animation computing its progress independently
/// from its own duration — so animations of different lengths play together and
/// the shortest one simply finishes first. That is Manim's behaviour and it is
/// worth preserving exactly.
#[derive(Default)]
pub struct AnimationScheduler {
    running: Vec<Running>,
    /// Seconds of virtual time since this batch started.
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
        self.running.push(Running {
            anim: Box::new(anim),
            started: false,
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
        // Progress is computed against the batch clock, not accumulated, so a
        // large dt cannot drift the phase of an animation.
        for r in &mut self.running {
            let d = r.anim.duration();
            let raw = if d <= 0.0 { 1.0 } else { self.elapsed / d };
            let alpha = r.anim.ease(raw);
            r.anim.interpolate(alpha, tree);
        }
        self.running.retain(|r| self.elapsed < r.anim.duration());
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
/// Morph-style animations need to pair up two point arrays; keeping the pairing
/// explicit in data (rather than implied by traversal order) is what will let
/// the AI layer express "morph this into that".
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
