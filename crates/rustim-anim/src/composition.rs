//! Composed animations: [`AnimationGroup`] and [`Succession`].
//!
//! Manim's `animation/composition.py` is the file these two are ported from, and
//! the reason they exist at all is `lag_ratio`: neither [`SceneContext::play_all`]
//! (everything in parallel) nor a queue of beats (everything one after another)
//! can express "start the second one half-way through the first". A composed
//! animation is one [`Animation`] — one entry in a beat, one duration, one clock —
//! whose `interpolate` hands each member its own slice of a rescaled internal
//! timeline.
//!
//! # The timeline
//!
//! With members `0..n` and durations `d[i]`, Manim lays them out as
//!
//! ```text
//! start[0] = 0
//! start[i] = start[i-1] + d[i-1] * lag_ratio
//! end[i]   = start[i] + d[i]
//! ```
//!
//! so `lag_ratio = 0` starts them all together (the scheduler's parallel beat) and
//! `lag_ratio = 1` starts each one when the previous ends (a queue of beats). Any
//! value between staggers them. The group's own duration is `max(end)` — **not**
//! `end[n-1]`, because with `lag_ratio < 1` a long early member can outlast a later
//! short one (`composition.py:150-161` says the same in a comment Manim added for
//! exactly this case). An explicit [`AnimationGroup::with_duration`] overrides that
//! total but **not the internal timeline**: the group then plays its whole layout
//! within the stated time, which is Manim's `init_run_time(run_time)` behaviour
//! and is what makes "the same stagger, but over 4 seconds" a parameter.
//!
//! # Easing, and why the group's own is `linear`
//!
//! Manim's `AnimationGroup` defaults `rate_func` to `linear`
//! (`composition.py:79`), not to the `smooth` everything else inherits, because
//! the members ease themselves: the group's rate function shapes the *schedule*,
//! and easing it as well would make the whole group lurch while each member also
//! eases, a double application no other animation has. This port keeps that
//! default, and hands each member `member.ease(sub)` — the member's own curve
//! applied to its slice, which is what Manim's `get_sub_alpha` does inside
//! `interpolate_mobject` (`animation.py:385-391`).
//!
//! # What begins when
//!
//! [`AnimationGroup::begin`] begins every member at once, so a member that
//! captures its origin in `begin` captures it before any sibling has moved
//! anything — Manim's own choice, and the reason a group is not a queue. A
//! [`Succession`] begins each member when its turn arrives, so a `MoveTo` in the
//! second slot starts from wherever the first slot *left* the object.
//!
//! One deliberate difference: our [`Animation::begin`] is handed `&SceneTree`, so
//! no member can be written at exactly `alpha = 0` at group begin the way Manim's
//! `Animation.begin` does with its closing `interpolate(0)`. The first frame is
//! instead the usual one `dt` into the member's slice — exactly as an ungrouped
//! animation's first frame is in this engine. It settles to the same end state.

use rustim_mobject::SceneTree;

use crate::{Animation, RateFunc, rate};

/// Manim's `DEFAULT_LAGGED_START_LAG_RATIO`, for a group built by a scene that
/// wants a gentle stagger rather than the strict `AnimationGroup` default of 0.
///
/// Named here rather than as a magic `0.05` at each call site because it is
/// Manim's number, not ours: `LaggedStart` is `AnimationGroup` with this default
/// (`composition.py:32`), and a port that picked its own would produce pictures
/// that do not match the ones the ecosystem is used to.
pub const DEFAULT_LAGGED_START_LAG_RATIO: f64 = 0.05;

/// Member start/end times on the group's internal timeline, in seconds.
#[derive(Debug, Clone)]
struct Timings {
    starts: Vec<f64>,
    ends: Vec<f64>,
    /// `max(ends)`, and the end of the group's internal timeline.
    max_end: f64,
}

impl Timings {
    /// Manim's `build_animations_with_timings` (`composition.py:167-184`).
    ///
    /// A non-finite `lag_ratio` starts everything together rather than poisoning
    /// every start with `NaN`: `NaN` compares false against everything, so a
    /// poisoned timeline would leave every member forever un-begun, frozen behind
    /// a beat that still occupies its own duration. Refusing it is the command
    /// layer's job; the engine's is to stay well-defined for a value a scene
    /// computed.
    fn build(children: &[Box<dyn Animation>], lag_ratio: f64) -> Self {
        let ratio = if lag_ratio.is_finite() {
            lag_ratio
        } else {
            0.0
        };
        let run_times: Vec<f64> = children.iter().map(|c| c.duration()).collect();
        let mut starts = vec![0.0; run_times.len()];
        for i in 1..run_times.len() {
            starts[i] = starts[i - 1] + run_times[i - 1] * ratio;
        }
        let ends: Vec<f64> = starts.iter().zip(&run_times).map(|(s, r)| s + r).collect();
        let max_end = ends.iter().copied().fold(0.0_f64, f64::max);
        Self {
            starts,
            ends,
            max_end,
        }
    }

    /// A member's own progress at the group's internal time `t`.
    ///
    /// Clamped to `0.0..=1.0`, which is [`Animation::interpolate`]'s contract, and
    /// `1.0` for a zero-length member — Manim special-cases the same two things
    /// (`composition.py:210-223`), including the zero-length one, which would
    /// otherwise be `0/0`.
    fn slice(&self, i: usize, t: f64) -> f64 {
        let run = self.ends[i] - self.starts[i];
        if run <= 0.0 {
            1.0
        } else {
            ((t - self.starts[i]) / run).clamp(0.0, 1.0)
        }
    }
}

/// Plays several animations at once, staggered by `lag_ratio`.
///
/// This is Manim's `AnimationGroup` (`composition.py:35-230`). The distinction
/// that matters here: this is **one** animation, so it is one entry in a beat and
/// one duration — which is what lets it be restated as a whole ("the same
/// staggered fade, but over 4 seconds") and nested inside a bigger group, neither
/// of which a sequence of beats can do.
///
/// Every member is begun at the group's start and finished at its end. A member
/// that completes early is therefore still in the tree until the group does — for
/// a `FadeOut` that means invisible-but-present rather than absent, which is the
/// state Manim leaves it in too: `AnimationGroup.finish` is what calls
/// `anim.finish()` for every member (`composition.py:124-133`).
pub struct AnimationGroup {
    children: Vec<Box<dyn Animation>>,
    timings: Timings,
    /// The group's stated duration; `None` means the internal timeline's length.
    stated: Option<f64>,
    begun: Vec<bool>,
    finished: Vec<bool>,
}

impl AnimationGroup {
    /// A group of `children` at `lag_ratio = 0` — everything together, which is
    /// Manim's `AnimationGroup` default.
    pub fn new(children: Vec<Box<dyn Animation>>) -> Self {
        let timings = Timings::build(&children, 0.0);
        let n = children.len();
        Self {
            children,
            timings,
            stated: None,
            begun: vec![false; n],
            finished: vec![false; n],
        }
    }

    /// Stagger the members: `ratio` is the fraction of the previous member's
    /// duration that passes before the next one starts.
    ///
    /// Recomputed immediately, so this is usable both as the last builder call and
    /// before [`Self::with_duration`]; the order between the two does not matter
    /// because the stated duration never feeds the timeline.
    pub fn with_lag_ratio(mut self, ratio: f64) -> Self {
        self.timings = Timings::build(&self.children, ratio);
        self
    }

    /// Play the whole layout in `seconds` instead of in `max(end)`.
    ///
    /// The internal timeline is unchanged and is scaled uniformly, which is what
    /// Manim's `init_run_time` does when `run_time` is passed explicitly; the
    /// members keep their relative stagger and their individual easing.
    pub fn with_duration(mut self, seconds: f64) -> Self {
        self.stated = Some(seconds);
        self
    }

    pub fn len(&self) -> usize {
        self.children.len()
    }

    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }
}

impl Animation for AnimationGroup {
    fn duration(&self) -> f64 {
        self.stated.unwrap_or(self.timings.max_end)
    }

    fn rate_func(&self) -> RateFunc {
        rate::linear
    }

    fn begin(&mut self, tree: &SceneTree) {
        self.begun = vec![false; self.children.len()];
        self.finished = vec![false; self.children.len()];
        for child in &mut self.children {
            child.begin(tree);
        }
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        // `alpha` is already eased by this animation's own rate function (the
        // scheduler does that), and the group's default is `linear`, so this is
        // Manim's `rate_func(alpha) * max_end_time` with the identity in place.
        let t = alpha * self.timings.max_end;
        let mut begun = Vec::with_capacity(self.children.len());
        let mut finished = Vec::with_capacity(self.children.len());
        for i in 0..self.children.len() {
            let new_begun = t >= self.timings.starts[i];
            // Strict, as upstream: the frame that reaches a member's end is the
            // frame on which it is delivered its final `1.0`, and only then does
            // it stop needing updates.
            let new_finished = t > self.timings.ends[i];
            let update = (self.begun[i] || new_begun) && (!self.finished[i] || !new_finished);
            begun.push(new_begun);
            finished.push(new_finished);
            if update {
                let sub = self.timings.slice(i, t);
                let eased = self.children[i].ease(sub);
                self.children[i].interpolate(eased, tree);
            }
        }
        self.begun = begun;
        self.finished = finished;
    }

    fn finish(&mut self, tree: &mut SceneTree) {
        // Manim's `AnimationGroup.finish` calls each member's `finish`, and its
        // `Animation.finish` forces `interpolate(1)` first (`animation.py:216-227`)
        // — the exact end state, which is what a `FadeOut` or an `Uncreate` needs
        // before it removes anything.
        for child in &mut self.children {
            let one = child.ease(1.0);
            child.interpolate(one, tree);
            child.finish(tree);
        }
    }
}

/// Plays several animations one after another, as **one** animation.
///
/// Manim's `Succession` is `AnimationGroup` with `lag_ratio = 1`
/// (`composition.py:233-311`), and that is all this is — but the timing work is
/// not the same as the group's: a member is begun only when its turn arrives (so
/// it captures the state its predecessors left), and it is *finished* as it is
/// left behind, not all at the end. That last difference is visible: an
/// `Uncreate` in the middle of a succession removes its object when its slot
/// ends, where in an `AnimationGroup` it would linger invisibly until the group
/// did.
///
/// Compared with queuing beats in a [`crate::Sequence`], the difference is what a
/// beat *is*: a succession is one animation with one duration, so it can be
/// nested in a parallel beat or restated as a whole, and its members cannot be
/// addressed separately. Use the queue when the steps are the scene's structure;
/// use this when the steps are one thing that happens.
pub struct Succession {
    children: Vec<Box<dyn Animation>>,
    timings: Timings,
    stated: Option<f64>,
    /// The member being played; `None` before the first and after the last.
    active: Option<usize>,
}

impl Succession {
    /// Members played back to back.
    pub fn new(children: Vec<Box<dyn Animation>>) -> Self {
        let timings = Timings::build(&children, 1.0);
        Self {
            children,
            timings,
            stated: None,
            active: None,
        }
    }

    /// Override the stagger. Manim's `Succession` accepts `lag_ratio` and defaults
    /// it to 1; a value below 1 is a `LaggedStart` in all but name, so it is
    /// allowed here rather than refused.
    pub fn with_lag_ratio(mut self, ratio: f64) -> Self {
        self.timings = Timings::build(&self.children, ratio);
        self
    }

    /// Play the whole succession in `seconds`; the internal timeline is scaled.
    pub fn with_duration(mut self, seconds: f64) -> Self {
        self.stated = Some(seconds);
        self
    }

    pub fn len(&self) -> usize {
        self.children.len()
    }

    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    /// Finish the active member and move to the next, or stop after the last.
    ///
    /// Manim's `next_animation` (`composition.py:277-287`) in two lines, because
    /// our `begin` cannot be handed a mutable tree and so the "tell the member it
    /// ended" half has to be explicit here: `interpolate(1.0)` then `finish`.
    fn advance(&mut self, tree: &mut SceneTree) {
        if let Some(i) = self.active {
            let one = self.children[i].ease(1.0);
            self.children[i].interpolate(one, tree);
            self.children[i].finish(tree);
            let next = i + 1;
            self.active = if next < self.children.len() {
                Some(next)
            } else {
                None
            };
        }
    }
}

impl Animation for Succession {
    fn duration(&self) -> f64 {
        self.stated.unwrap_or(self.timings.max_end)
    }

    fn rate_func(&self) -> RateFunc {
        rate::linear
    }

    fn begin(&mut self, tree: &SceneTree) {
        self.active = if self.children.is_empty() {
            None
        } else {
            Some(0)
        };
        if let Some(0) = self.active {
            self.children[0].begin(tree);
        }
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        let t = alpha * self.timings.max_end;
        // `while`, not `if`: several zero-length members can be left behind on one
        // frame, and every one of them has to be begun and finished.
        while let Some(i) = self.active {
            if t < self.timings.ends[i] {
                break;
            }
            self.advance(tree);
            if let Some(j) = self.active {
                self.children[j].begin(tree);
            }
        }
        if let Some(i) = self.active {
            let sub = self.timings.slice(i, t);
            let eased = self.children[i].ease(sub);
            self.children[i].interpolate(eased, tree);
        }
    }

    fn finish(&mut self, tree: &mut SceneTree) {
        // Reached with `active == None` after a normal run, because the last frame
        // at `alpha == 1.0` advances past every member. The loop is for the other
        // route: a group nested in a `Sequence` under reduced motion is driven to
        // `ease(1.0)` and then finished, and so is any caller that finishes
        // without a complete interpolation.
        while self.active.is_some() {
            self.advance(tree);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    use rustim_mobject::{Mobject, shapes};

    use crate::{AnimationScheduler, FadeOut, Motion, Sequence, Wait};

    type Log = Rc<RefCell<Vec<Event>>>;

    /// What a member saw, in order. The order is half the point: "the second
    /// member begins after the first finished" is a claim about events, not about
    /// final values.
    #[derive(Debug, Clone, PartialEq)]
    enum Event {
        Begin(&'static str),
        At(&'static str, f64),
        Finish(&'static str),
    }

    /// An animation that only records. Its own `duration` is what the group's
    /// timeline is built from, and its `rate_func` is settable so the "members ease
    /// themselves" claim can be tested with an asymmetric curve.
    struct Probe {
        name: &'static str,
        duration: f64,
        rate: RateFunc,
        log: Log,
    }

    impl Probe {
        fn new(name: &'static str, duration: f64, log: &Log) -> Self {
            Self {
                name,
                duration,
                rate: rate::linear,
                log: log.clone(),
            }
        }

        fn eased(mut self) -> Self {
            self.rate = rate::ease_in_out;
            self
        }
    }

    impl Animation for Probe {
        fn duration(&self) -> f64 {
            self.duration
        }

        fn begin(&mut self, _tree: &SceneTree) {
            self.log.borrow_mut().push(Event::Begin(self.name));
        }

        fn interpolate(&mut self, alpha: f64, _tree: &mut SceneTree) {
            self.log.borrow_mut().push(Event::At(self.name, alpha));
        }

        fn finish(&mut self, _tree: &mut SceneTree) {
            self.log.borrow_mut().push(Event::Finish(self.name));
        }

        fn rate_func(&self) -> RateFunc {
            self.rate
        }
    }

    fn log() -> Log {
        Rc::new(RefCell::new(Vec::new()))
    }

    fn probe(name: &'static str, duration: f64, log: &Log) -> Box<dyn Animation> {
        Box::new(Probe::new(name, duration, log))
    }

    /// Every `At` recorded for `name`, in order.
    fn alphas(log: &Log, name: &'static str) -> Vec<f64> {
        log.borrow()
            .iter()
            .filter_map(|e| match e {
                Event::At(n, a) if *n == name => Some(*a),
                _ => None,
            })
            .collect()
    }

    fn event_count(log: &Log, wanted: Event) -> usize {
        log.borrow().iter().filter(|e| **e == wanted).count()
    }

    #[test]
    fn a_group_at_lag_zero_plays_everything_together() {
        let log = log();
        let mut group = AnimationGroup::new(vec![probe("a", 1.0, &log), probe("b", 2.0, &log)]);
        // `max(end)`, not the last `end`: with no lag they are the same here, but
        // the assertion is the group's duration being the *longest* member.
        assert_eq!(group.duration(), 2.0);

        let mut tree = SceneTree::new();
        group.begin(&tree);
        group.interpolate(0.25, &mut tree);

        // The internal clock is `alpha * max_end = 0.5`, so the one-second member
        // is half done and the two-second member a quarter — Manim's rescaling
        // (`composition.py:200-203`), not "everyone at 0.25".
        assert_eq!(alphas(&log, "a"), vec![0.5]);
        assert_eq!(alphas(&log, "b"), vec![0.25]);
    }

    #[test]
    fn lag_ratio_staggers_the_starts() {
        let log = log();
        let mut group = AnimationGroup::new(vec![probe("a", 1.0, &log), probe("b", 1.0, &log)])
            .with_lag_ratio(0.5);
        // starts 0 and 0.5, ends 1.0 and 1.5.
        assert_eq!(group.duration(), 1.5);

        let mut tree = SceneTree::new();
        group.begin(&tree);
        log.borrow_mut().clear();

        // Half-way through a 1.5 s group is t = 0.75: three quarters through the
        // first member and a quarter through the second.
        group.interpolate(0.5, &mut tree);
        assert_eq!(alphas(&log, "a"), vec![0.75]);
        assert_eq!(alphas(&log, "b"), vec![0.25]);
    }

    /// A member that has not started is not written at all — not written at zero.
    ///
    /// The two are different states for a `Create` (whose alpha-0 write is a
    /// collapsed path) and for a `Transform` (whose `begin` aligns the source), so
    /// "skip it" is the behaviour and this test names the counterfactual.
    #[test]
    fn a_member_that_has_not_started_is_not_written() {
        let log = log();
        let mut group = AnimationGroup::new(vec![probe("a", 1.0, &log), probe("b", 1.0, &log)])
            .with_lag_ratio(0.5);

        let mut tree = SceneTree::new();
        group.begin(&tree);
        group.interpolate(0.2, &mut tree); // t = 0.3, before b's 0.5

        let a = alphas(&log, "a");
        assert_eq!(a.len(), 1);
        // `0.2 * 1.5` is not bit-exactly 0.3, so this is the one comparison here
        // that needs a tolerance.
        assert!((a[0] - 0.3).abs() < 1e-12, "{a:?}");
        assert!(alphas(&log, "b").is_empty(), "b has not begun playing yet");
        // The group begins every member eagerly (see the module doc), so `Begin`
        // and `At` are deliberately not the same question: b was begun at group
        // begin and simply has not been written since.
        assert_eq!(event_count(&log, Event::Begin("b")), 1);
    }

    #[test]
    fn members_ease_themselves_and_the_group_does_not_ease_them_again() {
        let log = log();
        let mut group = AnimationGroup::new(vec![
            Box::new(Probe::new("a", 1.0, &log).eased()),
            Box::new(Probe::new("b", 1.0, &log).eased()),
        ]);
        let mut tree = SceneTree::new();
        group.begin(&tree);
        group.interpolate(0.25, &mut tree);

        // t = 0.25, so each slice is 0.25 raw; `ease_in_out(0.25) = 0.125`. A group
        // that passed the raw slice through would record 0.25.
        assert_eq!(alphas(&log, "a"), vec![rate::ease_in_out(0.25)]);
        assert_eq!(rate::ease_in_out(0.25), 0.125);
        assert_eq!(alphas(&log, "b"), vec![0.125]);
    }

    #[test]
    fn an_explicit_duration_rescales_the_layout_and_not_its_shape() {
        let log = log();
        let mut group = AnimationGroup::new(vec![probe("a", 1.0, &log), probe("b", 1.0, &log)])
            .with_lag_ratio(0.5)
            .with_duration(4.0);
        assert_eq!(group.duration(), 4.0);

        let mut tree = SceneTree::new();
        group.begin(&tree);
        group.interpolate(0.5, &mut tree);

        // Alpha 0.5 of the stated four seconds is still t = 0.75 of the *internal*
        // 1.5 s layout — the stagger is unchanged, only how long it takes.
        assert_eq!(alphas(&log, "a"), vec![0.75]);
        assert_eq!(alphas(&log, "b"), vec![0.25]);
    }

    #[test]
    fn a_succession_plays_members_back_to_back() {
        let log = log();
        let mut succession = Succession::new(vec![probe("a", 1.0, &log), probe("b", 1.0, &log)]);
        assert_eq!(succession.duration(), 2.0);

        let mut tree = SceneTree::new();
        succession.begin(&tree);
        log.borrow_mut().clear();

        succession.interpolate(0.25, &mut tree); // t = 0.5
        assert_eq!(alphas(&log, "a"), vec![0.5]);
        assert!(alphas(&log, "b").is_empty(), "b's turn has not come");
        assert!(
            !log.borrow().contains(&Event::Begin("b")),
            "a succession begins a member when it becomes active, unlike a group"
        );

        // t = 1.0 is exactly the first member's end: it is delivered its final
        // state, finished, and only then is the second begun.
        succession.interpolate(0.5, &mut tree);
        assert_eq!(
            log.borrow().as_slice(),
            &[
                Event::At("a", 0.5),
                Event::At("a", 1.0),
                Event::Finish("a"),
                Event::Begin("b"),
                Event::At("b", 0.0),
            ]
        );
    }

    /// The difference from a group, stated as a picture test: an `Uncreate` (or a
    /// `FadeOut`) in a succession takes its object away when its slot ends, while
    /// the same member in a group leaves it in the tree until the group is over.
    #[test]
    fn a_succession_finishes_a_member_when_its_slot_ends_and_a_group_does_not() {
        let tree_with_object = || {
            let mut tree = SceneTree::new();
            let id = tree.insert(Mobject::new(shapes::square(2.0, 2.0)));
            tree.get_mut(id).unwrap().opacity = 1.0;
            (tree, id)
        };

        // Succession: gone half-way through, while the pause still plays.
        let (mut tree, id) = tree_with_object();
        let mut succession = Succession::new(vec![
            Box::new(FadeOut::new(id).with_duration(1.0)),
            Box::new(Wait::new(1.0)),
        ]);
        succession.begin(&tree);
        succession.interpolate(0.5, &mut tree); // t = 1.0
        assert!(
            tree.get(id).is_none(),
            "the removal belongs to the member's own end"
        );

        // Group: still present (and invisible) until the group finishes.
        let (mut tree, id) = tree_with_object();
        let mut group = AnimationGroup::new(vec![
            Box::new(FadeOut::new(id).with_duration(1.0)),
            Box::new(Wait::new(2.0)),
        ]);
        group.begin(&tree);
        group.interpolate(0.75, &mut tree); // t = 1.5
        let m = tree
            .get(id)
            .expect("a group finishes everyone at its own end");
        assert_eq!(m.opacity, 0.0, "but the fade did reach zero");
        group.finish(&mut tree);
        assert!(
            tree.get(id).is_none(),
            "and the group's end is when it goes"
        );
    }

    #[test]
    fn a_nested_group_is_one_member_with_its_own_duration() {
        let log = log();
        let inner = AnimationGroup::new(vec![probe("x", 1.0, &log), probe("y", 1.0, &log)]);
        assert_eq!(inner.duration(), 1.0);
        let mut outer =
            AnimationGroup::new(vec![Box::new(inner), probe("z", 1.0, &log)]).with_lag_ratio(1.0);
        assert_eq!(outer.duration(), 2.0);

        let mut tree = SceneTree::new();
        outer.begin(&tree);
        outer.interpolate(0.5, &mut tree); // t = 1.0: the inner group ends, z begins
        assert_eq!(
            alphas(&log, "x"),
            vec![1.0],
            "the inner group's members were driven to their end"
        );
        assert_eq!(alphas(&log, "z"), vec![0.0]);
    }

    /// A composed animation is one beat, so it must go through the reduced-motion
    /// branch as a whole — and its `finish` must still remove what its members
    /// remove.
    #[test]
    fn a_sequence_reduces_a_composed_animation_to_its_end_state() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(shapes::square(2.0, 2.0)));
        tree.get_mut(id).unwrap().opacity = 1.0;

        let group: Vec<Box<dyn Animation>> = vec![Box::new(
            AnimationGroup::new(vec![
                Box::new(FadeOut::new(id).with_duration(0.4)),
                Box::new(Wait::new(0.2)),
            ])
            .with_lag_ratio(0.5)
            .with_duration(1.0),
        )];
        let mut seq = Sequence::new();
        seq.push(group);

        let mut sched = AnimationScheduler::new();
        let beat = seq.start_next(&mut sched, &mut tree, Motion::Reduced);
        assert!(beat);
        assert!(
            tree.get(id).is_none(),
            "a reduced composed beat still runs its members' removals"
        );
        // And the rhythm is the composed animation's own stated duration.
        assert_eq!(sched.len(), 1);
    }

    #[test]
    fn an_empty_composition_is_a_harmless_zero_duration_animation() {
        let mut tree = SceneTree::new();
        let mut group = AnimationGroup::new(vec![]);
        assert_eq!(group.duration(), 0.0);
        group.begin(&tree);
        group.interpolate(1.0, &mut tree);
        group.finish(&mut tree);

        let mut succession = Succession::new(vec![]);
        assert_eq!(succession.duration(), 0.0);
        succession.begin(&tree);
        succession.interpolate(1.0, &mut tree);
        succession.finish(&mut tree);
    }

    /// A non-finite `lag_ratio` is the one value that would poison every start
    /// with `NaN` — `NaN >= x` is false, so nothing would ever begin — while the
    /// beat still occupies its duration. It is treated as zero here; the command
    /// layer refuses it outright.
    #[test]
    fn a_non_finite_lag_ratio_starts_everything_together() {
        let log = log();
        let mut group = AnimationGroup::new(vec![probe("a", 1.0, &log), probe("b", 1.0, &log)])
            .with_lag_ratio(f64::NAN);
        assert_eq!(group.duration(), 1.0);

        let mut tree = SceneTree::new();
        group.begin(&tree);
        group.interpolate(0.5, &mut tree);
        assert_eq!(alphas(&log, "a"), vec![0.5]);
        assert_eq!(alphas(&log, "b"), vec![0.5]);
    }

    /// A zero-length member is `0/0` on the internal timeline; it must land on its
    /// end state instead. Manim special-cases the same thing
    /// (`composition.py:210-223`), and an instant `Wait(0.0)` or a zero-duration
    /// fade is how it arises.
    #[test]
    fn a_zero_length_member_lands_on_its_end_state() {
        let log = log();
        let mut group = AnimationGroup::new(vec![probe("a", 0.0, &log), probe("b", 1.0, &log)]);
        let mut tree = SceneTree::new();
        group.begin(&tree);
        group.interpolate(0.0, &mut tree);
        assert_eq!(alphas(&log, "a"), vec![1.0]);
        assert_eq!(alphas(&log, "b"), vec![0.0]);
    }
}
