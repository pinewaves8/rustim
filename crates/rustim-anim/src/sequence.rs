//! The playback script: a queue of beats, played one after another.
//!
//! Manim's `construct()` body is a list of `self.play(...)` calls
//! (`scene/scene.py:1234-1240`), and each one renders its whole segment before it
//! returns (`manager.py:259-261`) — the blocking frame loop *is* the timeline.
//! rustim's `construct` returns and the loop keeps running, so the same script has
//! to be written down as data and executed a beat at a time. That is all this type
//! is: the script, with one method that hands the next beat to the scheduler.
//!
//! # Why this is not `Succession`
//!
//! Manim expresses sequence by *composing* animations — `Succession` is an
//! `AnimationGroup` with `lag_ratio = 1` (`animation/composition.py:198-236`), one
//! animation whose `alpha` sweeps a rescaled internal timeline
//! (`composition.py:157-168`). Two things follow, and both are why the port stops
//! here:
//!
//! * A group is a *different feature*: `lag_ratio` staggers children that all
//!   belong to one alpha sweep, which is what `LaggedStart` needs and what neither
//!   of this workspace's scenes does.
//! * A group's explicit `run_time` **rescales** its children rather than changing
//!   their durations (`composition.py:162-168`). Sequential playback here is a
//!   driver concept — beat *k+1* starts when beat *k* has drained — with no
//!   rescaling anywhere, so a beat's own duration always means what it says.
//!
//! What ships covers `play(a, b)` (one beat, in parallel) and `play(a); play(b)`
//! (two beats, in order), which is the whole authoring surface the scenes need.

use std::collections::VecDeque;

use crate::{Animation, AnimationScheduler};

/// A script of beats.
///
/// A *beat* is the set of animations that play together — Manim's one
/// `Scene.play(*args)`. Manim runs those args in parallel and does **not** wrap
/// them in an `AnimationGroup`: they go into one flat list
/// (`scene/scene.py:1017-1026`), the total is `max(run_time)` (`:1184-1186`), and
/// every frame applies `alpha = t / animation.run_time` to each with its own
/// duration (`:1714-1724`). That is already [`AnimationScheduler`]'s one shared
/// clock, so a beat needs no type of its own here.
// No `Debug`, for the same reason `AnimationScheduler` has none: `dyn Animation`
// is not `Debug`, so the only honest derive is one that hides the field anyway.
#[derive(Default)]
pub struct Sequence {
    steps: VecDeque<Vec<Box<dyn Animation>>>,
}

impl Sequence {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one beat.
    ///
    /// Generic over the animation *type* rather than taking `Box<dyn Animation>`
    /// directly, because a bound of the form `IntoIterator<Item = Box<dyn
    /// Animation>>` does **not** make `push([FadeIn::new(a), FadeIn::new(b)])`
    /// compile — the array's element type is settled from its contents before the
    /// bound is consulted, so the call site would have to spell out the box. With
    /// the type parameter inferred, `push([FadeIn::new(a), FadeIn::new(b)])` just
    /// works, and a beat of *mixed* types still does through the forwarding
    /// `impl Animation for Box<T>` (see `lib.rs`).
    ///
    /// **An empty iterator appends nothing**, which is a deliberate divergence
    /// from Manim: `Scene.play()` with no animations raises
    /// `ValueError("Called Scene.play with no animations")` (`scene.py:1335-1338`).
    /// A scene here is data an AI layer or a generator may produce, and a beat that
    /// animates nothing is harmless — whereas panicking out of `construct` would
    /// take the app down for it. Queueing an empty beat instead would be worse than
    /// either: the driver starts a beat and waits for it to drain, so a beat with
    /// nothing in it would consume a frame, and a run of them would stall the clock
    /// while looking like a frozen scene.
    pub fn push<A: Animation + 'static>(&mut self, anims: impl IntoIterator<Item = A>) {
        // The return type on the closure is the coercion site; `as` cannot do it,
        // because an unsizing cast is not a primitive cast.
        let beat: Vec<Box<dyn Animation>> = anims
            .into_iter()
            .map(|a| -> Box<dyn Animation> { Box::new(a) })
            .collect();
        if !beat.is_empty() {
            self.steps.push_back(beat);
        }
    }

    /// Move the next beat into `sched`, and report whether there was one.
    ///
    /// This is the only way a beat ever reaches the scheduler, which is what makes
    /// "one beat at a time" a property of the type rather than of every caller.
    pub fn start_next(&mut self, sched: &mut AnimationScheduler) -> bool {
        let Some(beat) = self.steps.pop_front() else {
            return false;
        };
        for anim in beat {
            sched.add_boxed(anim);
        }
        true
    }

    /// True when no beats are left to start.
    ///
    /// Says nothing about a beat already handed to the scheduler — a caller that
    /// wants "is anything left to play" wants both this and
    /// [`AnimationScheduler::is_empty`], which is exactly what
    /// `SceneContext::is_playing` is.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    pub fn len(&self) -> usize {
        self.steps.len()
    }

    pub fn clear(&mut self) {
        self.steps.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RateFunc, rate};
    use rustim_mobject::SceneTree;

    /// The frame grid every recorder in a test shares.
    ///
    /// "These two began together" and "this one's first alpha came after that one
    /// finished" are questions about *frames*, not about each animation's own log,
    /// so there is one counter per test rather than one per animation.
    #[derive(Clone, Default)]
    struct Clock(std::rc::Rc<std::cell::RefCell<u64>>);

    impl Clock {
        fn tick(&self) {
            *self.0.borrow_mut() += 1;
        }
        fn now(&self) -> u64 {
            *self.0.borrow()
        }
    }

    /// What a recorder saw: `(frame, alpha)` per driven frame.
    #[derive(Clone, Default)]
    struct Frames(std::rc::Rc<std::cell::RefCell<Vec<(u64, f64)>>>);

    impl Frames {
        fn alphas(&self) -> Vec<f64> {
            self.0.borrow().iter().map(|&(_, a)| a).collect()
        }
    }

    struct Recorder {
        seen: Frames,
        clock: Clock,
        duration: f64,
    }

    fn recorder(clock: &Clock, duration: f64) -> (Recorder, Frames) {
        let seen = Frames::default();
        (
            Recorder {
                seen: seen.clone(),
                clock: clock.clone(),
                duration,
            },
            seen,
        )
    }

    impl Animation for Recorder {
        fn duration(&self) -> f64 {
            self.duration
        }

        fn interpolate(&mut self, alpha: f64, _tree: &mut SceneTree) {
            self.seen.0.borrow_mut().push((self.clock.now(), alpha));
        }

        /// Linear, so an alpha is a frame count and the assertions below read as
        /// arithmetic rather than as an easing curve.
        fn rate_func(&self) -> RateFunc {
            rate::linear
        }
    }

    const DT: f64 = 1.0 / 60.0;

    /// Drive one sequence to exhaustion exactly the way `SceneContext::advance`
    /// does, and return how many frames it took.
    ///
    /// The *order* is the load-bearing part. The next beat is started **before**
    /// the update, not after: starting it afterwards would spend one frame with an
    /// empty scheduler (the very first frame of the sequence, which would then
    /// never advance the clock for any beat) and would make `started_at` a frame
    /// later than the batch's own zero. Starting it first gives every beat a first
    /// alpha of exactly `dt / duration`, and puts beat *k+1*'s first frame
    /// immediately after beat *k*'s last.
    fn play(seq: &mut Sequence, clock: &Clock) -> u64 {
        let mut sched = AnimationScheduler::new();
        let mut tree = SceneTree::new();
        let mut frames = 0;
        while !seq.is_empty() || !sched.is_empty() {
            if sched.is_empty() {
                seq.start_next(&mut sched);
            }
            sched.update(DT, &mut tree);
            clock.tick();
            frames += 1;
            assert!(frames < 10_000, "the sequence never drained");
        }
        frames
    }

    /// **The guard that separates a sequence from a bag of animations.**
    ///
    /// Three beats of half a second each. Beat *k+1* must see nothing until beat
    /// *k* has drained, and then it must see the same alphas beat *k* saw — its own
    /// zero, not a continuation of the batch clock. Dropping the offset in
    /// `AnimationScheduler::add` gives every beat a first alpha of
    /// `(something + dt) / duration`, which draws a plausible animation that
    /// starts slightly too far along — no still frame can see it, and neither can
    /// an assertion that only checks the beats ran in order.
    #[test]
    fn beats_play_in_order_and_not_at_once() {
        let clock = Clock::default();
        let (a, seen_a) = recorder(&clock, 0.5);
        let (b, seen_b) = recorder(&clock, 0.5);
        let (c, seen_c) = recorder(&clock, 0.5);

        let mut seq = Sequence::new();
        seq.push([a]);
        seq.push([b]);
        seq.push([c]);
        play(&mut seq, &clock);

        let a = seen_a.0.borrow();
        let b = seen_b.0.borrow();
        let c = seen_c.0.borrow();
        let (a_alphas, b_alphas, c_alphas) = (seen_a.alphas(), seen_b.alphas(), seen_c.alphas());

        // 0.5 s at 1/60 s per frame: alphas climbing 1/30 by 1/30, and *starting*
        // there — a first alpha near 1.0 would be the batch clock's tail, and one
        // near 2/30 would be the lost frame the offset removes.
        assert!(
            (a_alphas[0] - 1.0 / 30.0).abs() < 1e-12,
            "got {}",
            a_alphas[0]
        );
        for (n, alphas) in [("two", &b_alphas), ("three", &c_alphas)] {
            assert!(
                (alphas[0] - 1.0 / 30.0).abs() < 1e-12,
                "beat {n} must start at its own zero, got {}",
                alphas[0]
            );
            // The same alphas, to within the rounding of `started_at` itself — an
            // accumulated sum, so beating bitwise here would be asserting that two
            // sums happened to round alike. And within one frame of the same
            // *length*, because where the accumulated `1/60`s finally round past
            // the duration decides whether the last frame is the 30th or the 31st.
            assert!(
                alphas.len().abs_diff(a_alphas.len()) <= 1,
                "beat {n} took {} frames against beat one's {}",
                alphas.len(),
                a_alphas.len()
            );
            for (i, (x, y)) in alphas.iter().zip(a_alphas.iter()).enumerate() {
                assert!(
                    (x - y).abs() < 1e-12,
                    "beat {n} diverged from beat one at frame {i}: {x} vs {y}"
                );
            }
        }

        // The frame the beat is carried past its own duration on delivers exactly
        // 1.0 — not 0.9999.. — which is what makes a settled frame bit-exact, and
        // is the contract `Style::lerp`'s `t == 1.0` snap and `Transform`'s exact
        // endpoint both rest on.
        for (n, alphas) in [("one", &a_alphas), ("two", &b_alphas), ("three", &c_alphas)] {
            assert_eq!(*alphas.last().unwrap(), 1.0, "beat {n} must reach 1.0");
        }

        // Order, stated on the frame grid, and with no gap between beats: beat two
        // is driven on the very frame after beat one's last.
        assert_eq!(b[0].0, a.last().unwrap().0 + 1);
        assert_eq!(c[0].0, b.last().unwrap().0 + 1);
    }

    /// One beat is played in parallel, and animations of different durations end
    /// on their own schedules — Manim's `max(run_time)` for the beat, each child
    /// at its own pace (`scene.py:1714-1724`).
    #[test]
    fn one_beat_plays_its_animations_together() {
        let clock = Clock::default();
        let (short, seen_short) = recorder(&clock, 0.5);
        let (long, seen_long) = recorder(&clock, 1.0);

        let mut seq = Sequence::new();
        seq.push([short, long]);
        play(&mut seq, &clock);

        let short = seen_short.0.borrow();
        let long = seen_long.0.borrow();
        assert_eq!(short[0].0, long[0].0, "a beat is driven from one frame");
        assert_eq!(*seen_short.alphas().last().unwrap(), 1.0);
        assert_eq!(*seen_long.alphas().last().unwrap(), 1.0);

        // **One clock, two schedules**, stated exactly: both are linear and start on
        // the same frame, so at every frame the long one has covered precisely half
        // of the short one. The last index is skipped because that is where the
        // short one has already been clamped to 1.0.
        for i in 0..short.len().saturating_sub(1) {
            assert!(
                (long[i].1 - short[i].1 / 2.0).abs() < 1e-12,
                "frame {}: short {}, long {}",
                short[i].0,
                short[i].1,
                long[i].1
            );
        }

        // And the beat lasts as long as its *longest* member: the short one is
        // finished roughly half a second before the long one.
        let (short_end, long_end) = (short.last().unwrap().0, long.last().unwrap().0);
        assert!(
            long_end >= short_end + 29,
            "the short one ended at frame {short_end}, the long one at {long_end}"
        );
    }

    /// An empty beat is dropped at `push`, so the driver can never spend a frame on
    /// nothing. Manim raises here; the argument for skipping instead is on `push`.
    #[test]
    fn an_empty_beat_is_skipped_rather_than_stalling_the_loop() {
        let clock = Clock::default();
        let (a, seen) = recorder(&clock, 0.5);

        let mut seq = Sequence::new();
        seq.push(Vec::<Box<dyn Animation>>::new());
        assert!(seq.is_empty(), "an empty beat must not be queued");
        assert_eq!(seq.len(), 0);

        let mut sched = AnimationScheduler::new();
        assert!(!seq.start_next(&mut sched), "nothing to start");
        assert!(sched.is_empty(), "and the scheduler stays empty");

        // The real beat still plays, and — the point of the test — it starts on the
        // very first frame of the run rather than one frame in, which is what it
        // would cost to start a beat that has nothing in it.
        seq.push([a]);
        play(&mut seq, &clock);
        assert_eq!(
            seen.0.borrow()[0].0,
            0,
            "a frame was spent on the empty beat"
        );
        assert_eq!(*seen.alphas().last().unwrap(), 1.0);
    }

    #[test]
    fn starting_a_beat_says_whether_there_was_one() {
        let clock = Clock::default();
        let (a, _) = recorder(&clock, 0.5);
        let mut seq = Sequence::new();
        let mut sched = AnimationScheduler::new();
        seq.push([a]);

        assert!(seq.start_next(&mut sched));
        assert_eq!(sched.len(), 1);
        assert!(seq.is_empty());
        assert!(!seq.start_next(&mut sched), "the script is exhausted");
        assert_eq!(sched.len(), 1, "and nothing was added");
    }

    #[test]
    fn clear_drops_the_whole_script() {
        let clock = Clock::default();
        let (a, _) = recorder(&clock, 0.5);
        let mut seq = Sequence::new();
        seq.push([a]);
        seq.push(Vec::<Box<dyn Animation>>::new());
        assert_eq!(seq.len(), 1);
        seq.clear();
        assert!(seq.is_empty());
        assert_eq!(seq.len(), 0);
    }
}
