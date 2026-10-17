//! Out-of-band notifications: what the timeline did, as it did it.
//!
//! A reply answers a command. An event answers nothing — it reports something
//! the engine did on its own, which is exactly what a client driving a
//! *continuous* engine cannot learn from a request/response stream. A host that
//! sends `play` is told the beat was queued (an empty diff); it is not told,
//! unless it subscribes, that the beat then **started** and later **finished**.
//! Without that, a real-time assistant can only poll `query`, and polling is
//! both late and wasteful.
//!
//! # An event line is not a reply
//!
//! A reply carries `seq` and exactly one of `diff` / `tree` / `capabilities` /
//! `error` ([`crate::CommandReply`]). An event line carries neither:
//!
//! ```text
//! {"event":{"kind":"beat_started","beat":1,"members":2,"duration":1.5,"clock":0.25}}
//! ```
//!
//! The presence of `event` is the discriminant, and the absence of `seq` says
//! "this answers no command". A client therefore reads a line by asking one
//! question first, and a client that never sent [`crate::Command::Events`] never
//! sees one — subscription is what keeps a strictly request/response host from
//! having to skip lines it did not order.
//!
//! # What the three events mean
//!
//! * [`Event::BeatStarted`] — a beat left the queue and reached the clock. `beat`
//!   is its 1-based number in this session, `members` how many animations the
//!   script put in it, `duration` its nominal length (`max(duration)` over the
//!   members, the same total the scheduler uses), and `clock` the lecture time
//!   it began at.
//! * [`Event::AnimationDone`] — **every animation in that beat has finished**
//!   (or, under reduced motion, the hold that stood in for them has). One event
//!   per beat rather than one per member: a beat is the unit a script and a
//!   viewer both see, and a client that counted members would have to know how
//!   reduced motion substitutes a single [`rustim_anim::Wait`].
//! * [`Event::FrameReady`] — the host presented (or wrote) a frame. This is
//!   **host-produced**, not kernel state: the engine has no frames, only virtual
//!   time. The variant lives here because it is wire vocabulary, so a client
//!   parses one stream with one type.
//!
//! # What is deliberately *not* an event
//!
//! A **stop** does not produce [`Event::AnimationDone`]. An interrupt is not a
//! completion — no `finish` runs, and a `FadeOut` in flight leaves its target in
//! the tree — so reporting "done" would be a lie a client could act on. The same
//! goes for `undo`, `reset` and a dropped script. A subscriber learns that the
//! timeline ended by asking, or by the next `beat_started` never arriving; what
//! it must never be told is that an interrupted beat finished.
//!
//! Events are a **log, not state**: `checkpoint` does not capture them and
//! `undo` does not replay or erase them. They describe what the engine did, and
//! what it did cannot be unwound by restoring a tree.
//!
//! # A bounded tail
//!
//! The kernel records events whether or not anyone is listening, because
//! delivery is a transport question and the kernel has no transport. A host
//! drains them every frame ([`crate::SceneContext::take_events`]); a host that
//! forgets would otherwise grow the queue forever, so it is capped at
//! [`MAX_EVENTS`], oldest dropped first. A seek across a very long script is the
//! one case where events are genuinely numerous, and the tail is what a host
//! that has not drained yet can still act on.

use serde::{Deserialize, Serialize};

/// The most events the kernel keeps before dropping the oldest.
///
/// A safeguard, not a budget: a host drains every frame, and a frame produces at
/// most a handful. The cap exists because nothing in the engine may grow without
/// limit on input it does not control — a seek can cross thousands of beats, and
/// a host that stopped reading must not turn that into unbounded memory.
pub const MAX_EVENTS: usize = 4096;

/// One kind of event, as a value a filter can name.
///
/// Separate from [`Event`] because a subscription selects *kinds*, and a client
/// that wanted only `frame_ready` should not have to spell a whole event to say
/// so. The serde spelling is the wire's (`snake_case`), pinned by the
/// round-trip fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    BeatStarted,
    AnimationDone,
    FrameReady,
}

impl EventKind {
    /// Every kind, in the order [`Event`] declares them.
    pub const ALL: [EventKind; 3] = [
        EventKind::BeatStarted,
        EventKind::AnimationDone,
        EventKind::FrameReady,
    ];

    /// The wire's name, for a human line or a capability list.
    pub fn name(self) -> &'static str {
        match self {
            Self::BeatStarted => "beat_started",
            Self::AnimationDone => "animation_done",
            Self::FrameReady => "frame_ready",
        }
    }
}

/// Something the engine or a host did, published on the command stream.
///
/// See the module doc for the semantics of each variant and for what is
/// deliberately not an event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A beat left the queue and reached the clock.
    BeatStarted {
        /// 1-based beat number within this session; never reused after a stop.
        beat: u64,
        /// How many animations the script put in the beat.
        members: usize,
        /// The beat's nominal length in seconds.
        duration: f64,
        /// Lecture-clock time the beat began at.
        clock: f64,
    },
    /// Every animation in the named beat has finished.
    ///
    /// An interrupt is not a completion and never produces this; see the module
    /// doc.
    AnimationDone {
        /// The beat number the matching [`Event::BeatStarted`] reported.
        beat: u64,
        /// Lecture-clock time the beat ended at.
        clock: f64,
    },
    /// The host presented or wrote a frame.
    ///
    /// Host-produced: the engine has no frames. `frame` is 1-based and counts
    /// the frames this host has emitted since it started.
    FrameReady {
        frame: u64,
        /// Lecture-clock time the frame shows.
        clock: f64,
    },
}

impl Event {
    /// Which filter names this event.
    pub fn kind(&self) -> EventKind {
        match self {
            Self::BeatStarted { .. } => EventKind::BeatStarted,
            Self::AnimationDone { .. } => EventKind::AnimationDone,
            Self::FrameReady { .. } => EventKind::FrameReady,
        }
    }
}

/// One event line on the wire: `{"event":{…}}`.
///
/// A wrapper rather than a bare [`Event`] so the discriminant and the subject
/// are different keys — the same shape a reply uses for `diff` / `tree`, and
/// what lets a client distinguish an event from a reply without reading the
/// inner tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventLine {
    pub event: Event,
}

/// One event line, without its newline.
///
/// Cannot fail for a reason a caller could act on — every field is a finite JSON
/// scalar — so it returns the string rather than pushing the same `unwrap` up a
/// level, exactly as [`crate::command::reply_line`] does.
pub fn event_line(event: &Event) -> String {
    serde_json::to_string(&EventLine {
        event: event.clone(),
    })
    .expect("an event is serialisable by construction")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each event's line is one JSON object with the single key `event`, no
    /// `seq`, and the kind spelled the way a subscription filter names it — the
    /// three properties a client's line classifier reads.
    #[test]
    fn every_event_writes_one_untagged_line() {
        let cases = [
            (
                Event::BeatStarted {
                    beat: 1,
                    members: 2,
                    duration: 1.5,
                    clock: 0.25,
                },
                "beat_started",
            ),
            (
                Event::AnimationDone {
                    beat: 1,
                    clock: 1.75,
                },
                "animation_done",
            ),
            (
                Event::FrameReady {
                    frame: 108,
                    clock: 1.8,
                },
                "frame_ready",
            ),
        ];
        for (event, kind) in cases {
            let line = event_line(&event);
            assert!(
                !line.contains("\"seq\""),
                "an event answers no command: {line}"
            );
            let parsed: EventLine =
                serde_json::from_str(&line).unwrap_or_else(|e| panic!("{line}: {e}"));
            assert_eq!(parsed.event, event, "the line round-trips");
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(value.as_object().unwrap().len(), 1, "{line}");
            assert_eq!(value["event"]["kind"], kind, "{line}");
            assert_eq!(event.kind().name(), kind, "{line}");
        }
    }

    /// The filter's vocabulary is exactly the events that exist: every `ALL`
    /// entry names a variant that produces it, and no variant is unnamed.
    #[test]
    fn the_kind_list_is_the_event_vocabulary() {
        let produced = [
            Event::BeatStarted {
                beat: 1,
                members: 1,
                duration: 0.0,
                clock: 0.0,
            },
            Event::AnimationDone {
                beat: 1,
                clock: 0.0,
            },
            Event::FrameReady {
                frame: 1,
                clock: 0.0,
            },
        ]
        .map(|e| e.kind());
        assert_eq!(produced, EventKind::ALL);
        // And the serde spelling of a kind is the name a filter takes, so the
        // two cannot drift.
        for kind in EventKind::ALL {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.name()));
        }
    }
}
