//! `WithRate` — a rate override on top of any finished animation.

use crate::{Animation, RateFunc};
use rustim_mobject::NodeId;
use rustim_mobject::SceneTree;

/// An animation wrapped so that its progress is eased by `func` instead of the
/// rate function the animation itself declares.
///
/// # Why a wrapper and not a `rate` field on every animation
///
/// Manim gives every `Animation` a `rate_func` attribute, so
/// `FadeIn(x, rate_func=rate_functions.ease_in_sine)` is a constructor
/// argument. Here the curve is a trait method and the animation types each own
/// their own struct: a field on each would be a field to thread through every
/// constructor *and* every command arm, to add a property that all of them
/// handle identically. The wrapper states the operation once.
///
/// It works because of where the scheduler already looks: [`Animation::ease`]
/// calls [`Animation::rate_func`], the scheduler calls `ease`, and this
/// wrapper's `rate_func` is the override. Nothing is applied twice —
/// `interpolate` receives the eased alpha and forwards it unchanged, because
/// easing belongs to the caller (`Animation::interpolate`'s doc).
///
/// # The forwarding *is* the wrapper
///
/// `duration`, `begin`, `interpolate` and `finish` all forward. The fourth is
/// the one that type-checks when omitted and silently breaks: `finish`'s
/// default is a no-op, so a wrapper that forgot it would leave a wrapped
/// `FadeOut` fading out and staying — through the scheduler and through
/// `Sequence`, the only two routes an animation takes. `Box<dyn Animation>` has
/// the identical trap (see its impl), which is why the test here drives a
/// wrapped `FadeOut` through [`crate::AnimationScheduler`] and never calls
/// `finish` by hand.
pub struct WithRate<A: Animation> {
    inner: A,
    func: RateFunc,
}

impl<A: Animation> WithRate<A> {
    /// Wrap `inner` so its raw progress is eased by `func`.
    pub fn new(inner: A, func: RateFunc) -> Self {
        Self { inner, func }
    }

    /// The animation being eased.
    pub fn inner(&self) -> &A {
        &self.inner
    }
}

impl<A: Animation> Animation for WithRate<A> {
    /// Forwards to the wrapped animation, so a scoped `stop` sees through the
    /// wrapper — the same rule the other forwarded methods here follow.
    fn target(&self) -> Option<NodeId> {
        self.inner.target()
    }

    fn duration(&self) -> f64 {
        self.inner.duration()
    }

    fn begin(&mut self, tree: &SceneTree) {
        self.inner.begin(tree);
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        // `alpha` arrives already eased by `self.func`; the inner animation's
        // own curve must not see it a second time.
        self.inner.interpolate(alpha, tree);
    }

    fn finish(&mut self, tree: &mut SceneTree) {
        self.inner.finish(tree);
    }

    fn rate_func(&self) -> RateFunc {
        self.func
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AnimationScheduler, FadeOut, MoveTo};
    use rustim_math::Point;
    use rustim_mobject::{Mobject, NodeId, shapes};

    /// Records the alphas it is driven with, and declares its own (deliberately
    /// different) rate so a wrapper that did not override would be visible.
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
            crate::rate::smoothstep
        }
    }

    fn recorder(duration: f64) -> (Recorder, std::rc::Rc<std::cell::RefCell<Vec<f64>>>) {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        (
            Recorder {
                seen: seen.clone(),
                duration,
            },
            seen,
        )
    }

    fn scene_with_square() -> (SceneTree, NodeId) {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(shapes::square(1.0, 1.0)));
        (tree, id)
    }

    /// The override is the curve in use: the wrapped animation's own rate
    /// (`smoothstep`) is not what reaches it.
    #[test]
    fn the_override_eases_and_the_inner_curve_does_not() {
        let (inner, _) = recorder(1.0);
        let wrapped = WithRate::new(inner, crate::rate::linear);
        assert_eq!(wrapped.ease(0.25), 0.25);
        assert_eq!(wrapped.ease(0.75), 0.75);
        // The counterfactual: the same animation without the wrapper eases by
        // its own declaration, which is a different curve.
        let (inner, _) = recorder(1.0);
        assert_eq!(inner.ease(0.25), crate::rate::smoothstep(0.25));
        assert_ne!(crate::rate::smoothstep(0.25), 0.25);
    }

    /// Driven the way a scene drives it, the scheduler's eased alpha reaches the
    /// inner animation exactly once.
    #[test]
    fn the_scheduler_delivers_the_overridden_curve() {
        let (inner, seen) = recorder(1.0);
        let mut sched = AnimationScheduler::new();
        sched.add(WithRate::new(inner, crate::rate::linear));
        let (mut tree, _) = scene_with_square();

        sched.update(0.25, &mut tree);
        sched.update(0.25, &mut tree);

        assert_eq!(seen.borrow().as_slice(), &[0.25, 0.5]);
    }

    /// Same curve on both sides, applied once: the recorded alpha is
    /// `smoothstep(0.25)`, not `smoothstep(smoothstep(0.25))`.
    #[test]
    fn the_curve_is_never_applied_twice() {
        let (inner, seen) = recorder(1.0);
        let mut wrapped = WithRate::new(inner, crate::rate::smoothstep);
        let (mut tree, _) = scene_with_square();

        let eased = wrapped.ease(0.25);
        wrapped.interpolate(eased, &mut tree);

        assert_eq!(eased, crate::rate::smoothstep(0.25));
        assert_eq!(seen.borrow().as_slice(), &[eased]);
        assert_ne!(
            seen.borrow()[0],
            crate::rate::smoothstep(crate::rate::smoothstep(0.25))
        );
    }

    /// **The `finish` forwarding trap.** A `FadeOut` removes its target in
    /// `finish`; the default `finish` is a no-op, so a wrapper that forgot to
    /// forward would type-check and leave the object behind. No still frame can
    /// see it — a removed object and a transparent one draw the same pixels —
    /// which is why this asserts on the tree, through the scheduler.
    #[test]
    fn a_wrapped_fade_out_still_removes_its_target() {
        let (mut tree, id) = scene_with_square();
        let mut sched = AnimationScheduler::new();
        sched.add(WithRate::new(
            FadeOut::new(id).with_duration(0.2),
            crate::rate::ease_in_sine,
        ));

        for _ in 0..30 {
            sched.update(0.1 / 10.0, &mut tree);
        }

        assert!(sched.is_empty(), "the animation must complete");
        assert!(
            tree.get(id).is_none(),
            "a wrapped FadeOut must still remove its target"
        );
    }

    /// `begin` and `duration` forward too: a wrapped `MoveTo` captures its
    /// origin at playback and settles on its target, rather than losing either
    /// to the wrapper.
    #[test]
    fn a_wrapped_move_still_begins_and_lands() {
        let (mut tree, id) = scene_with_square();
        let target = Point::new(3.0, -2.0);
        let mut sched = AnimationScheduler::new();
        sched.add(WithRate::new(
            MoveTo::to_point(id, target).with_duration(0.2),
            crate::rate::ease_in_out_sine,
        ));

        for _ in 0..30 {
            sched.update(0.1 / 10.0, &mut tree);
        }

        let m = tree.get(id).unwrap();
        // `critical_point` is already in world coordinates (it materialises).
        let centre = m.critical_point((0.0, 0.0)).expect("a square has a centre");
        assert!((centre - target).hypot() < 1e-9, "settled at {centre:?}");
    }
}
