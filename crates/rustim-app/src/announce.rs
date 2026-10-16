//! Throttled announcements: what the engine decides is worth saying out loud.
//!
//! # Why this is not "speak whenever something changes"
//!
//! The obvious wiring — every time a property moves, push its new value at the
//! screen reader — is unusable in this engine specifically, not merely noisy in
//! general. `MoveTo` interpolates once per frame, so a one-second animation
//! produces sixty state changes a second; a reader that speaks each one is still
//! on the first word when the animation is over. `方案建议.md` §6 risk 8 names
//! this, and it is the reason announcements are *produced* at semantic
//! milestones only (the focus moved, a command was accepted, an animation
//! ended) and *released* through a rate limit here.
//!
//! # Held, not dropped
//!
//! Under the limit, an announcement is **replaced**, not discarded: only the
//! newest text is held. That distinction is the whole point. A burst of changes
//! ending in a state the user must know about — press Tab during an animation
//! and land on a new object — would otherwise be announced as the first thing
//! that happened and never as the thing that is true. Holding the latest means
//! the user hears where they *arrived*, one interval later.
//!
//! # Why `now` is a parameter
//!
//! Every method takes the current instant rather than reading `Instant::now()`.
//! A throttler whose behaviour is expressed in wall-clock time is the kind of
//! code that can only be tested by sleeping, which makes the tests slow and
//! flaky in equal measure — so the clock is an argument and the tests advance it
//! by hand. Nothing in this module other than the default interval knows what a
//! millisecond feels like.

use std::time::{Duration, Instant};

/// The floor between two announcements.
///
/// 750 ms is a starting point chosen to be longer than a reader takes to finish
/// a short phrase and shorter than the pause a user reads as "nothing
/// happened". It is a constructor parameter rather than a constant because the
/// right value depends on the reader and the text, and the accessibility layer
/// may end up deriving it from the length of what it is about to say.
pub const DEFAULT_MIN_INTERVAL: Duration = Duration::from_millis(750);

/// Somewhere announcement text can go.
///
/// The engine deliberately does not know what is on the other end. On the
/// native path this writes to an AccessKit node with a live-region role; a
/// future self-voicing mode (`方案建议.md` §4.6 requirement 3, deferred to P5)
/// would write to a speech synthesiser instead, and a test writes to a `Vec`.
/// Keeping the sink behind a trait is what lets the throttling logic — the part
/// with the state machine and the bugs — be tested with no accessibility stack
/// linked and no window open.
pub trait Narrator {
    /// Say `text`. Called only when the rate limiter has allowed it through, so
    /// an implementation must not add a delay of its own; a reader that queues
    /// internally already handles a burst.
    fn announce(&mut self, text: &str);
}

/// The throttle in front of a [`Narrator`].
///
/// See the module docs for why an announcement can be held rather than dropped.
#[derive(Debug, Clone)]
pub struct Announcer {
    /// The most recent text that has not been spoken yet, if any.
    pending: Option<String>,
    /// When the last announcement was released. `None` before the first one, so
    /// the first announcement is never held back — a user who presses Tab into
    /// an empty scene should not wait three quarters of a second to be told.
    last_at: Option<Instant>,
    /// The last text actually released, for de-duplication.
    last_text: Option<String>,
    min_interval: Duration,
}

impl Default for Announcer {
    fn default() -> Self {
        Self::new()
    }
}

impl Announcer {
    pub fn new() -> Self {
        Self::with_min_interval(DEFAULT_MIN_INTERVAL)
    }

    pub fn with_min_interval(min_interval: Duration) -> Self {
        Self {
            pending: None,
            last_at: None,
            last_text: None,
            min_interval,
        }
    }

    pub fn min_interval(&self) -> Duration {
        self.min_interval
    }

    /// Report something worth saying.
    ///
    /// Text identical to the last announcement is dropped outright rather than
    /// queued. This is not only an optimisation: the AccessKit announcement node
    /// is a live region whose text is read on change, so re-offering the same
    /// string would announce nothing anyway, and a caller that polls — "the
    /// focused object is X" — would otherwise keep the throttle permanently
    /// busy with news the user already has.
    ///
    /// The corner this leaves: focus moving A → B → A faster than one interval
    /// ends with "A" held and then dropped, so the excursion through B is never
    /// spoken. That is the intended reading — from the reader's point of view
    /// nothing changed — and the alternative is announcing the same thing twice
    /// to make the middle state audible.
    pub fn offer(&mut self, text: impl Into<String>) {
        let text = text.into();
        if Some(&text) == self.last_text.as_ref() {
            return;
        }
        self.pending = Some(text);
    }

    /// Take the announcement that may be released at `now`, if any.
    ///
    /// Call once per frame. Returns the text and clears it, so a caller cannot
    /// accidentally speak the same announcement on every subsequent frame.
    pub fn take(&mut self, now: Instant) -> Option<String> {
        self.pending.as_ref()?;
        if let Some(last) = self.last_at {
            // `checked_duration_since` rather than subtraction: a caller that
            // hands over a `now` earlier than the last release — a clock that
            // moved backwards, a test that reused an instant — must hold the
            // announcement, not panic in a debug build.
            let elapsed = now.checked_duration_since(last)?;
            if elapsed < self.min_interval {
                return None;
            }
        }
        let text = self.pending.take()?;
        self.last_at = Some(now);
        self.last_text = Some(text.clone());
        Some(text)
    }

    /// [`Self::take`], forwarded to `out`, reporting whether anything was said.
    pub fn flush(&mut self, now: Instant, out: &mut dyn Narrator) -> bool {
        match self.take(now) {
            Some(text) => {
                out.announce(&text);
                true
            }
            None => false,
        }
    }

    /// What is being held back, for a caller that wants to show it or log it.
    pub fn pending(&self) -> Option<&str> {
        self.pending.as_deref()
    }

    /// Drop anything held, without speaking it.
    ///
    /// For teardown: a pending announcement describes a scene that is about to
    /// stop existing, and speaking it would describe the wrong scene.
    pub fn clear(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        said: Vec<String>,
    }

    impl Narrator for Recorder {
        fn announce(&mut self, text: &str) {
            self.said.push(text.to_string());
        }
    }

    fn interval() -> Duration {
        Duration::from_millis(750)
    }

    #[test]
    fn nothing_offered_means_nothing_said() {
        let mut a = Announcer::new();
        let t = Instant::now();
        assert_eq!(a.take(t), None);
        assert_eq!(a.take(t + Duration::from_secs(10)), None);
    }

    #[test]
    fn the_first_announcement_is_not_delayed() {
        let mut a = Announcer::new();
        let t = Instant::now();
        a.offer("已选中 a 的正方形");
        assert_eq!(a.take(t).as_deref(), Some("已选中 a 的正方形"));
    }

    #[test]
    fn an_announcement_inside_the_window_is_held_and_then_released() {
        let mut a = Announcer::new();
        let t = Instant::now();
        a.offer("第一个");
        assert_eq!(a.take(t).as_deref(), Some("第一个"));

        a.offer("第二个");
        assert_eq!(
            a.take(t + Duration::from_millis(700)),
            None,
            "inside the window"
        );
        assert_eq!(a.pending(), Some("第二个"), "and it is held, not dropped");
        assert_eq!(
            a.take(t + interval()).as_deref(),
            Some("第二个"),
            "exactly at the interval it is released"
        );
    }

    #[test]
    fn a_burst_is_announced_as_where_it_ended_not_where_it_started() {
        // The case the whole design exists for: an animation runs while the user
        // presses Tab, so a dozen changes land inside one interval.
        let mut a = Announcer::new();
        let t = Instant::now();
        for step in 0..12 {
            a.offer(format!("第 {step} 个对象"));
        }
        assert_eq!(a.pending(), Some("第 11 个对象"));
        assert_eq!(a.take(t).as_deref(), Some("第 11 个对象"));
    }

    #[test]
    fn the_same_text_is_not_repeated() {
        let mut a = Announcer::new();
        let t = Instant::now();
        a.offer("圆");
        assert_eq!(a.take(t).as_deref(), Some("圆"));

        // Well past the window, so the throttle is not what is stopping this.
        a.offer("圆");
        assert_eq!(a.take(t + Duration::from_secs(60)), None);
        assert_eq!(a.pending(), None, "and it was dropped, not queued");
    }

    #[test]
    fn a_later_different_text_is_announced_again() {
        let mut a = Announcer::new();
        let t = Instant::now();
        a.offer("圆");
        a.take(t);
        a.offer("角");
        assert_eq!(a.take(t + interval()).as_deref(), Some("角"));
    }

    #[test]
    fn an_announcement_is_never_said_twice() {
        let mut a = Announcer::new();
        let t = Instant::now();
        a.offer("只说一次");
        assert_eq!(a.take(t).as_deref(), Some("只说一次"));
        // No further `offer`, but the frame loop keeps calling `take`.
        for frame in 1..120 {
            assert_eq!(a.take(t + Duration::from_millis(frame * 16)), None);
        }
    }

    #[test]
    fn flush_reports_whether_anything_was_said() {
        let mut a = Announcer::new();
        let mut rec = Recorder::default();
        let t = Instant::now();
        a.offer("一");
        assert!(a.flush(t, &mut rec));
        assert!(!a.flush(t + Duration::from_millis(16), &mut rec));
        a.offer("二");
        assert!(!a.flush(t + Duration::from_millis(700), &mut rec));
        assert!(a.flush(t + interval(), &mut rec));
        assert_eq!(rec.said, vec!["一".to_string(), "二".to_string()]);
    }

    #[test]
    fn a_clock_that_moves_backwards_holds_rather_than_panics() {
        let mut a = Announcer::new();
        let t = Instant::now();
        a.offer("一");
        a.take(t);
        a.offer("二");
        assert_eq!(a.take(t - Duration::from_secs(1)), None);
        assert_eq!(a.pending(), Some("二"), "held for the next frame, not lost");
    }

    #[test]
    fn clearing_drops_what_was_held() {
        let mut a = Announcer::new();
        let t = Instant::now();
        a.offer("一");
        a.take(t);
        a.offer("二");
        a.clear();
        assert_eq!(a.take(t + interval()), None);
    }

    #[test]
    fn the_interval_is_configurable() {
        let mut a = Announcer::with_min_interval(Duration::from_millis(50));
        assert_eq!(a.min_interval(), Duration::from_millis(50));
        let t = Instant::now();
        a.offer("一");
        a.take(t);
        a.offer("二");
        assert_eq!(a.take(t + Duration::from_millis(50)).as_deref(), Some("二"));
    }
}
