//! The scene trait and the context a scene is handed.

use std::collections::VecDeque;

use rustim_anim::{
    Animate, Animation, AnimationGroup, AnimationScheduler, CameraMove, Motion, Sequence,
    StyleProperty, Succession, Wait,
};
use rustim_math::{Affine2, Camera, Vec2, Viewport};
use rustim_mobject::shapes::{ShapeKind, ShapeParams};
use rustim_mobject::{NodeId, SceneTree};
use rustim_render::{DrawList, DrawOp};

use crate::a11y;
use crate::command::{self, Capabilities, Command, CommandError, CommandOutcome, StateDiff};
use crate::event::{Event, EventKind, MAX_EVENTS};
use crate::typeset::{TextSpec, Typesetter, typeset_error};

/// A scene: the user-facing entry point, mirroring Manim's `construct()`.
///
/// Two methods rather than one, because rustim is not a batch pipeline. Manim's
/// `construct()` blocks — each `self.play(...)` renders its whole segment before
/// returning (`scene/scene.py:1401-1407`), so the frame loop *is* the timeline.
/// Here `construct` is called once and returns, and `update` is called every frame
/// for as long as the scene lives. That split is the architecture change described
/// in `CLAUDE.md` — roughly 80% of the scheduling layer is new.
///
/// What the two methods keep from Manim is the *authoring* experience: a scene
/// writes `ctx.play(...)` calls in `construct` exactly as Manim writes
/// `self.play(...)` in its own, and the machine plays them one beat at a time.
pub trait Scene {
    /// Declare objects and script the opening animations with
    /// [`SceneContext::play`] / [`SceneContext::wait`].
    fn construct(&mut self, ctx: &mut SceneContext);

    /// Advance by `dt` seconds of virtual time.
    ///
    /// The default runs the playback machine, which is what most scenes want. A
    /// scene that has to react to the timeline *having finished* overrides this,
    /// calls [`SceneContext::advance`] first, and then checks
    /// [`SceneContext::is_playing`] — it must call `advance`, because draining the
    /// queue is the one thing `update` is responsible for and a scene that forgets
    /// stalls the whole script.
    fn update(&mut self, dt: f64, ctx: &mut SceneContext) {
        ctx.advance(dt);
    }

    /// Called when a command changed the tree, so a reactive scene can respond.
    fn on_command(&mut self, _diff: &StateDiff, _ctx: &mut SceneContext) {}

    /// Produce this frame's draw list.
    ///
    /// The default walks the scene tree, which is what a scene built out of
    /// mobjects wants. A scene that needs a draw operation the tree cannot
    /// express yet overrides this and pushes it itself — the reference
    /// `primitives` scene does, so that the golden-image suite exercises stroke
    /// joins and clip rasterisation and not only flat fills. Both paths start
    /// from [`SceneContext::begin_frame`], so an override cannot accidentally
    /// drop the background or forget the viewport transform.
    fn build_draw_list(&self, ctx: &SceneContext, list: &mut DrawList) {
        ctx.build_draw_list(list);
    }
}

/// One [`SceneContext::checkpoint`]: everything an undo restores.
///
/// The tree is the bulk of it, and the other four fields are the run state the
/// tree cannot express — the lecture clock a restored picture is at, and the
/// playback policy it was being shown under. The timeline is deliberately
/// absent: queued and running animations are values with captured `begin`
/// states, and there is no sound way to replay them against a different tree.
#[derive(Clone)]
struct Checkpoint {
    tree: SceneTree,
    clock: f64,
    paused: bool,
    tempo: f64,
    motion: Motion,
}

/// The net difference between two trees, as a [`StateDiff`].
///
/// One function for the two places that report a **computed** change — an
/// [`SceneContext::undo`] and a [`SceneContext::batch`] — because "what a diff
/// means" must not have two spellings; `rebless`-style bugs start as a second
/// copy of a comparison. The order follows [`SceneTree::draw_order`] on the
/// side each id came from: created and modified walk the *after* tree, removed
/// walks the *before* tree, so replaying the same change produces the same
/// document.
///
/// The comparison is [`Mobject::differs_from`], not `PartialEq`: every write
/// sets `dirty`, and a `set` that changed nothing must not read as a
/// modification (see [`Mobject::differs_from`]).
fn diff_trees(before: &SceneTree, after: &SceneTree) -> StateDiff {
    let mut diff = StateDiff::default();
    for id in after.draw_order() {
        match (before.get(id), after.get(id)) {
            (None, Some(_)) => diff.created.push(id),
            (Some(past), Some(now)) if past.differs_from(now) => diff.modified.push(id),
            _ => {}
        }
    }
    for id in before.draw_order() {
        if after.get(id).is_none() {
            diff.removed.push(id);
        }
    }
    diff
}

/// Everything a scene is allowed to touch.
///
/// The scene never sees a renderer, a window or a file. It mutates this context
/// and the app layer turns the result into pixels. That boundary is what lets
/// the same scene run in a window, in the exporter, and (later) in a browser.
pub struct SceneContext {
    pub tree: SceneTree,
    /// The animations playing *now*. Public like the rest of the context, with one
    /// caveat worth naming: driving this directly bypasses [`Motion`], because the
    /// preference is consulted where a *beat* is handed over
    /// ([`rustim_anim::Sequence::start_next`]). Nothing in the workspace does that —
    /// a scene scripts beats through [`SceneContext::play`] — but an animation added
    /// here by hand plays at full motion whatever the user asked for.
    pub animations: AnimationScheduler,
    pub viewport: Viewport,
    /// How much of the motion to play.
    ///
    /// Set at construction from the host's flag or platform preference, and
    /// changeable while running through [`Command::Motion`] — which is what the
    /// `m` key sends. A change takes effect at the **next beat started**: the
    /// value is read each time [`SceneContext::advance`] hands a beat to the
    /// scheduler, and a beat already on the clock keeps the decision it was given
    /// ([`Motion`]'s rule is in `rustim_anim::Sequence::start_next`, the one place
    /// a beat reaches it).
    ///
    /// A scene may read it while scripting — `construct` may skip a flourish it
    /// knows will not be animated — and that reading has no later correction:
    /// turning motion back on cannot replay what was skipped, because the scene's
    /// scripting ran once. The field is public for that read; a host that wants
    /// the change to go through the command layer gets it announced as a
    /// [`Command::Motion`] would.
    pub motion: Motion,
    /// Whether [`SceneContext::advance`] moves time at all.
    ///
    /// Private because every route in and out of it is a command or a named
    /// method ([`SceneContext::pause`], [`SceneContext::resume`],
    /// [`SceneContext::is_paused`]); a host that wrote the flag directly would
    /// bypass nothing, but it *would* make the pause state one more thing a
    /// caller has to remember to keep consistent with the reply it sent.
    paused: bool,
    /// Virtual seconds per frame, if the run is not paused.
    ///
    /// Multiplies the `dt` `advance` is handed, so both the scheduler and the
    /// lecture clock run at the same speed. Clamped by
    /// [`SceneContext::set_tempo`]; `1.0` unless a [`Command::Tempo`] said
    /// otherwise.
    tempo: f64,
    /// The scripted beats not yet started — deliberately private.
    ///
    /// The fields above are public because a scene may read and write them
    /// directly; this one is not, because a queue advanced in any order other than
    /// "the scheduler drained, so start the next beat" is a *silently different
    /// timeline*, and the only way to keep that from happening is to be the only
    /// thing that can advance it ([`SceneContext::advance`]).
    playback: Sequence,
    /// Virtual time since the run began, in seconds.
    ///
    /// Advanced at the top of [`SceneContext::advance`], so a still (`settle`
    /// steps a fixed grid) and a window (each frame's `dt`) share one notion of
    /// time. Private for the same reason `playback` is: narration due times are
    /// measured against it, and a second copy of "when are we" is a second answer.
    ///
    /// It is allowed to run backwards through the `,` key (which steps a frame
    /// back) but never below zero; a narration already drained is gone, so
    /// rewinding cannot replay it.
    clock: f64,
    /// Narration waiting for its due time, as `(due, text)`.
    ///
    /// Private because the track is ordered by virtual time and only
    /// [`SceneContext::take_narration`] may drain it — a host that read it
    /// directly would have to reproduce the "due at or before the clock" rule.
    narration: Vec<(f64, String)>,
    /// What the timeline did, waiting to be published.
    ///
    /// Private because delivery is a transport question: the kernel records and
    /// a host drains ([`SceneContext::take_events`]). See [`crate::event`] for
    /// the vocabulary, the bounded tail and what is deliberately not an event.
    events: VecDeque<Event>,
    /// The number the next beat started will carry (1-based, never reused).
    next_beat: u64,
    /// The beat currently on the clock, so its completion can name it.
    ///
    /// `None` when nothing of the queue is playing — including after a `stop`,
    /// an `undo` or a `reset`, because an interrupt is not a completion and must
    /// not produce an `animation_done`.
    current_beat: Option<u64>,
    /// Which event kinds this session publishes, or `None` for no stream.
    ///
    /// A session setting, not tree state: it survives `reset` (a host that
    /// subscribed still wants events from the next scene), is **not** part of a
    /// checkpoint (an undo does not rewind a transport), and is changed only by
    /// [`Command::Events`].
    published: Option<Vec<EventKind>>,
    /// The session snapshots [`SceneContext::undo`] unwinds, newest last.
    ///
    /// Private because the stack is only useful through the two named methods:
    /// a host that popped it by hand would have to reproduce the retirement and
    /// diff rules itself, and a second copy of "what an undo means" is the thing
    /// this layer is meant to own.
    checkpoints: Vec<Checkpoint>,
    /// Whatever this host typesets with, or nothing at all.
    ///
    /// The shaper is not a kernel dependency (see [`crate::typeset`]), so the
    /// capability is injected. `None` is the honest default and not a
    /// placeholder: a context with no typesetter refuses `text` and `formula`
    /// with [`CommandError::Unsupported`] and advertises neither in
    /// [`SceneContext::capabilities`], which is exactly what a host that cannot
    /// shape glyphs should say. It survives [`SceneContext::reset`], because it
    /// is a property of the binary rather than of the scene.
    typesetter: Option<Box<dyn Typesetter>>,
}

impl SceneContext {
    /// A context for a run at the given motion.
    ///
    /// `motion` is a parameter rather than a default because the host always knows
    /// it — the flag or the platform preference — and a scene must never be able to
    /// observe the wrong one.
    pub fn new(viewport: Viewport, motion: Motion) -> Self {
        Self {
            tree: SceneTree::new(),
            animations: AnimationScheduler::new(),
            viewport,
            motion,
            paused: false,
            tempo: 1.0,
            playback: Sequence::new(),
            clock: 0.0,
            narration: Vec::new(),
            events: VecDeque::new(),
            next_beat: 0,
            current_beat: None,
            published: None,
            checkpoints: Vec::new(),
            typesetter: None,
        }
    }

    /// Install the build's typesetter.
    ///
    /// Called by a host once, at construction: the desktop binary installs the
    /// parley + Typst provider, the browser's default build installs none. A
    /// later call replaces the previous one whole — see [`Typesetter`]'s doc for
    /// why one provider per build is the contract — and it takes effect at the
    /// next `add` (nothing already in the tree is reshaped).
    pub fn set_typesetter(&mut self, typesetter: Box<dyn Typesetter>) {
        self.typesetter = Some(typesetter);
    }

    /// The vocabulary this build advertises in a handshake.
    ///
    /// [`Capabilities::current`] describes the protocol; this describes the
    /// *binary*. The two differ only in the typeset shapes, and only when the
    /// host has no typesetter (or one that claims only half the pair): a client
    /// that reads `hello` must not be told `"formula"` is available and then be
    /// refused, so the list is filtered by what the installed provider says it
    /// can do rather than by what the parser can spell.
    pub fn capabilities(&self) -> Capabilities {
        Capabilities::with_typesetting(self.supports_text(), self.supports_formula())
    }

    /// Queue one animation as the next beat, and play it when the current one ends.
    ///
    /// This is Manim's `self.play(anim)`, and it is the whole authoring surface:
    /// a scene writes its `construct` as a list of these calls and the playback
    /// machine runs them one beat at a time. Each call appends; nothing is
    /// preempted. See [`SceneContext::play_all`] for the parallel case.
    pub fn play(&mut self, anim: impl Animation + 'static) {
        self.playback.push([anim]);
    }

    /// Queue several animations as **one** beat, played in parallel.
    ///
    /// Manim's `self.play(a, b)`: the args go into one flat list and are *not*
    /// wrapped in an `AnimationGroup` (`scene/scene.py:1017-1026`), so the beat
    /// lasts `max(run_time)` (`:1184-1186`) and each animation is driven at its own
    /// pace off one shared clock (`:1714-1724`). That is [`AnimationScheduler`]'s
    /// existing semantics, so this method adds a queue and no scheduling logic.
    ///
    /// Generic over the beat's element type, exactly like `Sequence::push`, and for
    /// the same reason: the two spellings a caller wants have different types in
    /// the source. A beat of one kind is `play_all([FadeIn::new(a), FadeIn::new(b)])`
    /// and reads best unboxed; a beat of **mixed** kinds cannot be an array literal
    /// at all, because an array's element type is settled before any bound is
    /// consulted — `[Box::new(FadeIn::new(a)), Box::new(Transform::new(b, t))]` has
    /// two types and no coercion is attempted — so it is spelled with the element
    /// type written once:
    ///
    /// ```ignore
    /// let beat: Vec<Box<dyn Animation>> = vec![Box::new(FadeIn::new(a)), Box::new(Transform::new(b, t))];
    /// ctx.play_all(beat);
    /// ```
    ///
    /// which works because `Box<dyn Animation>` is itself an `Animation` (the
    /// forwarding `impl Animation for Box<T>` in `rustim_anim`). A signature that demanded
    /// `Box<dyn Animation>` would work for both spellings too, but it would charge
    /// every homogeneous beat the boxes that only a mixed beat needs.
    pub fn play_all<A: Animation + 'static>(&mut self, anims: impl IntoIterator<Item = A>) {
        self.playback.push(anims);
    }

    /// Queue a pause as a beat of its own.
    ///
    /// Manim's `Scene.wait(duration=DEFAULT_WAIT_TIME)` (`scene/scene.py:1242-1276`)
    /// is nothing but `play(Wait(run_time=duration))`, and it is spelled that way
    /// here too — a pause is an animation that happens to write nothing. The payoff
    /// is that "a beat is a set of animations" stays uniform: no gap type, and
    /// [`SceneContext::is_playing`] stays true *across* the pause instead of
    /// flickering to "finished" between the beats on either side of it.
    pub fn wait(&mut self, seconds: f64) {
        self.play(Wait::new(seconds));
    }

    /// Advance the playback machine by `dt` seconds.
    ///
    /// Two steps, and their **order is load-bearing**. The next beat is started
    /// *before* the scheduler runs, not after:
    ///
    /// * starting it afterwards spends one frame with an empty scheduler — the very
    ///   first frame of the sequence, which would then never advance the clock for
    ///   any beat;
    /// * and it would make the beat's first frame a frame later than its own zero,
    ///   because `AnimationScheduler::add` stamps `started_at` from the clock as it
    ///   is *now*.
    ///
    /// The observable difference is one frame's worth of progress at every beat
    /// boundary: no still frame can see it, which is why
    /// `rustim-anim/src/sequence.rs`'s `beats_play_in_order_and_not_at_once` pins
    /// the frame *grid* rather than the picture.
    ///
    /// `self.motion` rides along to [`rustim_anim::Sequence::start_next`], and that
    /// is the **whole** of reduced motion's presence in this layer: a reduced beat
    /// lands on its end state and holds its own length, so nothing downstream —
    /// not this method, not [`SceneContext::is_playing`], not a scene reacting to
    /// the timeline having drained — needs to know the mode to stay correct.
    pub fn advance(&mut self, dt: f64) {
        // Two host controls act on the `dt` and nothing else, which is the whole
        // reason they are two fields instead of two more branches deeper in: a
        // pause is zero time, and a tempo is time scaled. Both must move the
        // lecture clock and the scheduler together — narration timed against a
        // clock that kept running through a pause would speak over a frozen
        // picture, and a tempo that scaled only the animations would drift the
        // subtitles off the beats.
        if self.paused {
            return;
        }
        self.advance_clock(dt * self.tempo);
    }

    /// [`SceneContext::advance`] with the playback controls bypassed.
    ///
    /// `dt` is lecture seconds, scaled by nobody. The split exists for
    /// [`SceneContext::seek`], whose target is an absolute time on this clock:
    /// running a fast-forward through `advance` would scale it by the tempo and
    /// land somewhere the caller did not ask for. A pause does not stop this
    /// either — scrubbing a paused timeline is the point of a seek.
    fn advance_clock(&mut self, dt: f64) {
        // The clock is the one time base narration is scheduled against. It
        // never goes below zero: `,` steps a frame backwards, and a negative
        // `at` is refused at the wire, so a rewind is a debug gesture and not a
        // second start of the lecture. A narration already drained is gone, so
        // stepping back cannot replay it — the track is a script, not a tape.
        self.clock = (self.clock + dt).max(0.0);
        if self.animations.is_empty()
            && let Some(started) =
                self.playback
                    .start_next(&mut self.animations, &mut self.tree, self.motion)
        {
            self.next_beat += 1;
            self.current_beat = Some(self.next_beat);
            self.record_event(Event::BeatStarted {
                beat: self.next_beat,
                members: started.members,
                duration: started.duration,
                clock: self.clock,
            });
        }
        self.animations.update(dt, &mut self.tree);
        // The complement of the start above: when the clock drains, the beat
        // that was on it has ended. A zero-length beat both starts and ends on
        // the same tick, which is honest — the scheduler completes a
        // zero-duration animation on its first update.
        //
        // Only a beat *started through the queue* has a `current_beat`, so an
        // animation a caller pushed straight into `ctx.animations` is not
        // reported; the queue is what events describe, and that bypass is
        // already documented as bypassing the motion policy too.
        if self.animations.is_empty()
            && let Some(beat) = self.current_beat.take()
        {
            self.record_event(Event::AnimationDone {
                beat,
                clock: self.clock,
            });
        }
    }

    /// Record one event, keeping the queue bounded.
    ///
    /// Oldest first out: a host that has stopped draining is more likely to act
    /// on what just happened than on the beginning of a burst it missed, and a
    /// silent unbounded `Vec` on input the engine does not control is exactly
    /// what [`MAX_EVENTS`] exists to prevent.
    fn record_event(&mut self, event: Event) {
        if self.events.len() == MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    /// Take the events recorded since the last call, oldest first.
    ///
    /// Draining rather than reading, for [`SceneContext::take_narration`]'s
    /// reason: an event is a report of something that happened once, and a host
    /// that read without draining would publish it again on the next frame.
    /// A host must call this even when it is not subscribed — the queue is the
    /// kernel's memory, and delivery is the transport's job.
    ///
    /// Returns an empty vector without allocating when nothing happened, which
    /// is the common case for every frame without a beat boundary.
    pub fn take_events(&mut self) -> Vec<Event> {
        self.events.drain(..).collect()
    }

    /// Whether this session publishes events at all.
    ///
    /// Off until a [`Command::Events`] turns it on. Kept on the context rather
    /// than in a transport so the subscription is part of the session a
    /// handover inherits — and so the command layer has one place to write it.
    pub fn events_subscribed(&self) -> bool {
        self.published.is_some()
    }

    /// Whether `kind` passes the session's filter.
    ///
    /// False when nothing is subscribed, so a transport can ask this per event
    /// and never write a line the session did not order. A `frame_ready` a host
    /// produces itself is gated by this too, which is what makes
    /// `{"enabled":true,"kinds":["beat_started","animation_done"]}` a usable
    /// way to follow the timeline without a line per frame.
    pub fn wants_event(&self, kind: EventKind) -> bool {
        self.published
            .as_ref()
            .is_some_and(|kinds| kinds.contains(&kind))
    }

    /// Freeze the timeline where it is.
    ///
    /// Both clocks stop: [`SceneContext::advance`] returns before touching the
    /// scheduler or the lecture clock, so a beat in flight holds its current
    /// alpha and narration due during the pause waits. The queue is untouched, so
    /// [`SceneContext::resume`] continues from the same place and a `play`
    /// accepted while paused starts on the resume rather than now.
    ///
    /// [`SceneContext::is_playing`] stays true: a pause is not the end of the
    /// timeline, and a scene that relabels when its morph settles must not
    /// relabel because a viewer held the picture.
    pub fn pause(&mut self) {
        self.paused = true;
    }

    /// Let a paused timeline run again. Idempotent.
    pub fn resume(&mut self) {
        self.paused = false;
    }

    /// Is the timeline held? See [`SceneContext::pause`].
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// The current playback speed. See [`SceneContext::set_tempo`].
    pub fn tempo(&self) -> f64 {
        self.tempo
    }

    /// The slowest speed a command may select.
    ///
    /// At `0.05` a one-second beat takes twenty — still playback, if a slow one.
    pub const MIN_TEMPO: f64 = 0.05;

    /// The fastest speed a command may select.
    ///
    /// At `16` a one-second beat is four frames. Past that a frame skips beats
    /// rather than playing them, which is [`SceneContext::seek`]'s job: a speed
    /// that cannot be watched is not a speed.
    pub const MAX_TEMPO: f64 = 16.0;

    /// The grid [`SceneContext::seek`] fast-forwards on.
    ///
    /// The same `1/60` the desktop frame loop, the browser session and the
    /// golden capture use, so a seek leaves the tree a run would actually have
    /// produced rather than a sample on a second, invisible grid.
    pub const SEEK_STEP: f64 = 1.0 / 60.0;

    /// The longest span one seek may skip, in lecture seconds.
    ///
    /// A cap rather than a warning: the fast-forward is bounded work at
    /// `SEEK_STEP`, and a model that asks to skip an hour must be told no in
    /// one reply instead of holding the connection while 216 000 frames run.
    pub const MAX_SEEK_SECONDS: f64 = 600.0;

    /// How many [`SceneContext::checkpoint`] snapshots are kept.
    ///
    /// A bound rather than a warning. A snapshot is a deep copy of the tree —
    /// cheap per object, unbounded per scene — and a session that checkpointed
    /// every command would grow without limit if the stack did. Sixteen steps of
    /// history is more than a lecture needs; the oldest is dropped, which is
    /// what "undo the last thing" needs and no more.
    pub const MAX_CHECKPOINTS: usize = 16;

    /// Remember the session as it is, so [`SceneContext::undo`] can return.
    ///
    /// The snapshot is the tree with its camera, the lecture clock, the pause
    /// and the tempo. It is a stack: several marks can be taken and unwound
    /// newest first, capped at [`SceneContext::MAX_CHECKPOINTS`] with the oldest
    /// dropped.
    ///
    /// The *timeline* is not captured, deliberately. A queued beat is a boxed
    /// animation that captured its start state in `begin`; replaying it into a
    /// restored tree would apply a motion measured against a picture that no
    /// longer exists. An undo therefore interrupts, exactly as
    /// [`SceneContext::stop`] does, and the restored snapshot is where scripting
    /// resumes.
    pub fn checkpoint(&mut self) {
        if self.checkpoints.len() == Self::MAX_CHECKPOINTS {
            self.checkpoints.remove(0);
        }
        self.checkpoints.push(Checkpoint {
            tree: self.tree.clone(),
            clock: self.clock,
            paused: self.paused,
            tempo: self.tempo,
            motion: self.motion,
        });
    }

    /// Restore the newest checkpoint and report what the restore changed.
    ///
    /// The diff is computed, not remembered: `created` is every node the
    /// snapshot has and the present does not, `removed` is the reverse, and
    /// `modified` is a survivor whose `Mobject` compares unequal to its past —
    /// so an undo after a `set` names the object, and an undo that only dropped
    /// a temporary addition names that one. Order follows draw order on the side
    /// each id came from, so a replay produces the same diff.
    ///
    /// Two behaviours are load-bearing. **Playback is interrupted**, not
    /// rewound: an animation in flight is cleared without `finish`, for
    /// [`SceneContext::stop`]'s reason and because its `begin` captured a tree
    /// the restore replaces. And **the abandoned present is retired**: every id
    /// the current tree handed out is remembered by the restored tree, so a host
    /// still holding one gets `no_such_node` rather than a different object
    /// wearing the old id — the generational promise, preserved across a clone
    /// that would otherwise rewind the slot versions.
    ///
    /// Narration already spoken is not unspoken (sound is not a tape), and
    /// lines still queued keep their due times on the restored clock. The
    /// snapshot being restored is consumed; a second undo steps one checkpoint
    /// further back, and with none left the answer is
    /// [`CommandError::NoCheckpoint`].
    pub fn undo(&mut self) -> Result<StateDiff, CommandError> {
        let checkpoint = self.checkpoints.pop().ok_or(CommandError::NoCheckpoint)?;
        self.stop();

        // The diff describes what the undo does to the tree the host is
        // looking at: the checkpoint is the *after* side.
        let diff = diff_trees(&self.tree, &checkpoint.tree);

        let mut restored = checkpoint.tree;
        restored.retire_ids_of(&self.tree);
        self.tree = restored;
        self.clock = checkpoint.clock;
        self.paused = checkpoint.paused;
        self.tempo = checkpoint.tempo;
        self.motion = checkpoint.motion;
        Ok(diff)
    }

    /// Apply a list of commands in order and report the **net** change.
    ///
    /// The reply is computed from the tree before and after — [`diff_trees`],
    /// the same function `undo` uses — not by concatenating the members' own
    /// diffs. That is what makes it the honest answer for a host that redraws
    /// from it: a node created and then removed in the same list is in neither
    /// half, and one created and then moved is `created` and not `modified`.
    /// `created` follows the final draw order, so the ids match what a query
    /// would show.
    ///
    /// **Not a transaction.** The first refusal stops the list and comes back
    /// as [`CommandError::InBatch`] with the member's index; everything before
    /// that index has been applied, and nothing after it has. A host that wants
    /// all-or-nothing takes a [`SceneContext::checkpoint`] first and an
    /// [`SceneContext::undo`] on refusal, or validates the same list with
    /// [`SceneContext::validate`] before sending it for real.
    ///
    /// The list's *shape* is checked before anything runs
    /// ([`SceneContext::check_batchable`]), so a malformed list — empty, or
    /// containing a command whose reply cannot be a diff — leaves the tree
    /// untouched and is refused at the top level rather than as `InBatch`.
    pub fn batch(&mut self, commands: &[Command]) -> Result<StateDiff, CommandError> {
        Self::check_batchable(commands)?;
        let before = self.tree.clone();
        self.run_batch(commands)?;
        Ok(diff_trees(&before, &self.tree))
    }

    /// Try a command list against a copy of the session and report only what
    /// would fail.
    ///
    /// The dry run is a real run — the same [`SceneContext::handle`], in the
    /// same order — against scratch state that is thrown away, so the live
    /// tree, camera, clock, pause, tempo, checkpoint stack and motion mode are
    /// untouched whichever way it goes. Members see each other's effects, which
    /// is the property that makes it worth having: a label created by the first
    /// member resolves for the second, and a node the first removed is gone for
    /// the third, so an interdependent plan can be checked as a whole.
    ///
    /// The scratch copy carries the tree, the clock, the pause, the tempo, the
    /// motion mode and the checkpoint stack, and the host's typesetter is moved
    /// into it for the duration and moved back afterwards — so a `text` or
    /// `formula` member is checked by the provider that would really build it.
    /// The scheduler and the narration track are **not** copied: a queued
    /// animation is a value that captured its start state, not replayable data.
    /// A dry run therefore checks a `play`'s spec and target rather than how
    /// the beat would look, and the scratch queue's emptiness is why a `seek`
    /// costs no rendering.
    ///
    /// `Ok(())` says the list would be accepted; every refusal is the same
    /// [`CommandError::InBatch`] (or top-level shape refusal) the real call
    /// would produce, at the same index.
    pub fn validate(&mut self, commands: &[Command]) -> Result<(), CommandError> {
        Self::check_batchable(commands)?;
        let mut scratch = self.scratch();
        let result = scratch.run_batch(commands);
        // The typesetter is the one object the two contexts share: it went into
        // the copy and comes back whether the run succeeded or not.
        self.typesetter = scratch.typesetter;
        result
    }

    /// The rules a command list obeys, checked before anything runs.
    ///
    /// Two rules, both structural rather than semantic:
    ///
    /// * **Non-empty.** A list with nothing in it is not a batch; "do nothing"
    ///   is spelled by sending nothing, and a model that generated an empty
    ///   array has a bug worth hearing about.
    /// * **Primitive members only.** `hello` answers with capabilities, `query`
    ///   with a tree, and a batch replies with one diff — a reply has no field
    ///   for a list of replies, so nesting them would silently drop the
    ///   document the member asked for. `batch` and `validate` cannot nest for
    ///   the same reason.
    ///
    /// The position is named as `commands[i]`, the same field spelling every
    /// other refusal uses, so a model reads the index it wrote.
    fn check_batchable(commands: &[Command]) -> Result<(), CommandError> {
        if commands.is_empty() {
            return Err(CommandError::OutOfRange {
                property: "commands".to_owned(),
                detail: "a command list needs at least one command".to_owned(),
            });
        }
        for (index, member) in commands.iter().enumerate() {
            let why = match member {
                Command::Hello { .. } => "hello answers with capabilities, not a diff",
                Command::Query { .. } => "query answers with a tree, not a diff",
                Command::Batch { .. } | Command::Validate { .. } => {
                    "a command list cannot be nested"
                }
                _ => continue,
            };
            return Err(CommandError::OutOfRange {
                property: format!("commands[{index}]"),
                detail: why.to_owned(),
            });
        }
        Ok(())
    }

    /// Run a checked list through the one apply point, locating a refusal.
    ///
    /// The [`CommandError::InBatch`] wrapper is added here rather than in
    /// [`SceneContext::batch`] so the dry run reports exactly the same shape: a
    /// validation and the real call agree about *where* a list fails, which is
    /// the whole promise of validating first.
    fn run_batch(&mut self, commands: &[Command]) -> Result<(), CommandError> {
        for (index, member) in commands.iter().enumerate() {
            self.handle(member).map_err(|cause| CommandError::InBatch {
                index,
                cause: Box::new(cause),
            })?;
        }
        Ok(())
    }

    /// A throwaway copy of the session for [`SceneContext::validate`].
    ///
    /// Everything a refusal could depend on is copied: the tree (camera
    /// included), the clock a `seek` is measured against, the pause and tempo
    /// the playback checks read, the motion mode a queued beat would take, and
    /// the checkpoint stack an `undo` member pops. The typesetter is **moved**,
    /// not cloned — the trait has no clone, and the host installs one provider
    /// per build — and the caller moves it back.
    ///
    /// The scheduler and the narration track are deliberately fresh; see
    /// [`SceneContext::validate`] for why.
    fn scratch(&mut self) -> SceneContext {
        let mut scratch = SceneContext::new(self.viewport, self.motion);
        scratch.tree = self.tree.clone();
        scratch.clock = self.clock;
        scratch.paused = self.paused;
        scratch.tempo = self.tempo;
        scratch.checkpoints = self.checkpoints.clone();
        scratch.typesetter = self.typesetter.take();
        scratch
    }

    /// Play the timeline at a different speed from now on.
    ///
    /// `factor` multiplies the `dt` [`SceneContext::advance`] is handed, for the
    /// scheduler and the lecture clock alike. A non-finite or non-positive value
    /// is ignored (the current tempo stands) and a value outside
    /// [`SceneContext::MIN_TEMPO`]..=[`SceneContext::MAX_TEMPO`] is clamped: a
    /// programmatic caller has no error channel, and the command layer validates
    /// and refuses before it gets here, so the clamp is the guard rather than
    /// the contract.
    pub fn set_tempo(&mut self, factor: f64) {
        if factor.is_finite() && factor > 0.0 {
            self.tempo = factor.clamp(Self::MIN_TEMPO, Self::MAX_TEMPO);
        }
    }

    /// Jump the lecture clock forward to `to`, finishing what lies in between.
    ///
    /// The playback machine is driven on [`SceneContext::SEEK_STEP`] until the
    /// clock reaches `to`, so the result is what a run would have had at that
    /// time: passed beats are played and `finish`ed, a beat in flight advances,
    /// and the next queued beat is ready to start. The final step is shortened
    /// to land exactly on `to`, and the clock is then **assigned**, because
    /// `a + (b - a)` is not `b` for every float pair and "seek to 6.5 s" should
    /// mean 6.5 s.
    ///
    /// `tempo` does not scale the jump (the target is lecture time), and a pause
    /// does not stop it (scrubbing a paused timeline is a normal thing to do).
    /// Narration the jump crosses is **dropped** rather than said in a burst on
    /// the next frame.
    ///
    /// Refusals, all values: a negative or non-finite `to`; a `to` behind the
    /// clock (a rewind needs a snapshot — the checkpoint/undo step); and a span
    /// longer than [`SceneContext::MAX_SEEK_SECONDS`].
    pub fn seek(&mut self, to: f64) -> Result<(), CommandError> {
        if !to.is_finite() || to < 0.0 {
            return Err(CommandError::OutOfRange {
                property: "to".to_owned(),
                detail: format!("{to} is not a finite, non-negative time"),
            });
        }
        if to < self.clock {
            return Err(CommandError::OutOfRange {
                property: "to".to_owned(),
                detail: format!("the clock is at {} s; seek moves forward only", self.clock),
            });
        }
        let span = to - self.clock;
        if span > Self::MAX_SEEK_SECONDS {
            return Err(CommandError::OutOfRange {
                property: "to".to_owned(),
                detail: format!(
                    "{span} s is longer than the {} s one seek will fast-forward",
                    Self::MAX_SEEK_SECONDS
                ),
            });
        }
        if span == 0.0 {
            return Ok(());
        }
        while self.clock < to {
            self.advance_clock((to - self.clock).min(Self::SEEK_STEP));
        }
        self.clock = to;
        // Draining rather than reading: every line whose due time the jump
        // passed is gone. Lines still ahead of the clock stay queued.
        self.take_narration();
        Ok(())
    }

    /// Interrupt the timeline: drop every queued beat and every running
    /// animation, leaving the picture exactly where the last frame put it.
    ///
    /// Two `clear`s and nothing else, and **that is the whole semantics**: both
    /// drivers deliberately do not call [`Animation::finish`], so an animation
    /// stopped here has no completion step — a `FadeOut` in flight leaves its
    /// target in the tree at the opacity it reached, a morph leaves its subject
    /// part-way, and the next `play` starts from there. Jumping every animation
    /// to its end state instead would be `finish`, which is the opposite of what
    /// "stop" means to a viewer.
    ///
    /// The clock is untouched — [`SceneContext::clock`] is the lecture's time,
    /// and narration and subtitle cues are timed against it — but the scheduler's
    /// own elapsed time resets with its list. That is unobservable: progress is
    /// read as `elapsed - started_at`, and every animation queued afterwards is
    /// stamped from the fresh zero, so it starts at its own zero either way.
    ///
    /// [`SceneContext::is_playing`] becomes false, which is what makes a scene
    /// that reacts to the timeline draining (a relabel, the `ANIMATION_DONE`
    /// announcement) see an interrupt as an ending rather than as a stall.
    pub fn stop(&mut self) {
        self.animations.clear();
        self.playback.clear();
        // An interrupt is not a completion: drop the beat identity without
        // recording an `animation_done` for it. See `crate::event`.
        self.current_beat = None;
    }

    /// Is there anything left to play — now or later?
    ///
    /// Not `!animations.is_empty()`: that reads "finished" during the frame before
    /// the first beat starts, and during every frame a beat spends queued. A host
    /// reacting to the timeline ending (the `ANIMATION_DONE` announcement, a scene
    /// that relabels once the morph settles) would fire at the wrong moment. Since
    /// `advance` starts the next beat in the same call the previous drains, the
    /// scheduler is never *observed* empty mid-timeline, and this predicate is what
    /// makes that true by construction rather than by luck.
    pub fn is_playing(&self) -> bool {
        !self.animations.is_empty() || !self.playback.is_empty()
    }

    /// Virtual seconds since this context was constructed.
    ///
    /// The same clock narration is scheduled against, and the only one an
    /// exporter may time a subtitle cue by: wall-clock time in a headless run is
    /// how long the *machine* took to rasterise, which is exactly what a
    /// deterministic artifact must not record.
    pub fn clock(&self) -> f64 {
        self.clock
    }

    /// Take every narration line whose time has come, oldest first.
    ///
    /// Draining rather than reading: a line is said once, and a frame that
    /// called this twice would otherwise speak it twice. Entries are ordered by
    /// their due time (an explicit `at` makes out-of-order arrival normal), and
    /// lines that fall due at the same instant keep the order they arrived in.
    ///
    /// The caller decides what "saying" means — a live region, a caption, a
    /// speech backend, or nothing at all — which is why this returns text and
    /// does not know about any of them.
    ///
    /// Returns an empty vector **without allocating** when nothing is due, the
    /// common case for every frame without a script.
    pub fn take_narration(&mut self) -> Vec<String> {
        self.take_narration_timed()
            .into_iter()
            .map(|(_, text)| text)
            .collect()
    }

    /// [`SceneContext::take_narration`], keeping each line's due time.
    ///
    /// An exporter needs the time as well as the words: a subtitle cue is a
    /// `(start, end)` pair, and the start is exactly the instant the line became
    /// due. Draining has the same once-only semantics, so an export must choose
    /// one of the two methods per line, never both.
    pub fn take_narration_timed(&mut self) -> Vec<(f64, String)> {
        if self.narration.iter().all(|(due, _)| *due > self.clock) {
            return Vec::new();
        }
        let mut later = Vec::new();
        let mut due = Vec::new();
        for (at, text) in self.narration.drain(..) {
            if at <= self.clock {
                due.push((at, text));
            } else {
                later.push((at, text));
            }
        }
        self.narration = later;
        // Stable, so equal due times keep arrival order.
        due.sort_by(|a, b| a.0.total_cmp(&b.0));
        due
    }

    /// Insert a mobject at the root.
    pub fn add(&mut self, m: rustim_mobject::Mobject) -> NodeId {
        self.tree.insert(m)
    }

    /// The tree's root-level nodes, in draw order.
    ///
    /// Forwarded rather than cached: the tree already keeps this list and keeps
    /// it correct through `remove` and `reparent`, so a second copy here could
    /// only ever disagree with it.
    pub fn root_ids(&self) -> &[NodeId] {
        self.tree.root_ids()
    }

    /// Tear the scene down, keeping allocations.
    ///
    /// The queue goes with the scheduler: a rewritten scene that kept the previous
    /// scene's beats would play them into a tree they were never written for, and
    /// `handle` can append beats from outside, so this is reachable without
    /// `construct` running again.
    pub fn reset(&mut self) {
        self.tree.clear();
        self.animations.clear();
        self.playback.clear();
        // Narration is part of the timeline, so it goes with the queue: a
        // rewritten scene that kept the previous one's due lines would speak
        // them over a picture they were never written for. The clock starts
        // over for the same reason — the new scene's `at: 0` means *its* zero.
        self.clock = 0.0;
        self.narration.clear();
        // The old scene's events describe a picture that no longer exists, and
        // its beat numbers start over with the new script. The *subscription*
        // stays: it is a property of the transport, not of the scene.
        self.events.clear();
        self.current_beat = None;
        self.next_beat = 0;
        // A snapshot of the previous scene is not history for the new one:
        // undoing into a tree the script never built would put back objects
        // whose ids the new scene has already retired, and the host asking for
        // that intended `reset`, not time travel across a rewrite.
        self.checkpoints.clear();
        // A fresh run plays: a scene rewritten while the previous one was held
        // must not come up frozen at a speed nobody asked for. The pause and the
        // tempo belong to the *run*, like the clock they scale.
        self.paused = false;
        self.tempo = 1.0;
    }

    /// Apply a command from outside the engine.
    ///
    /// [`Command::Hello`], [`Command::Play`], [`Command::Stop`],
    /// [`Command::Pause`], [`Command::Resume`], [`Command::Tempo`],
    /// [`Command::Seek`], [`Command::Checkpoint`], [`Command::Undo`],
    /// [`Command::Batch`], [`Command::Validate`], [`Command::Camera`],
    /// [`Command::Motion`], [`Command::Events`], [`Command::Narrate`] and
    /// [`Command::Query`] are answered **here** rather than in `command::apply`:
    /// a hello answers with capabilities rather than a diff — a shape `apply`
    /// cannot return — a play needs the playback machine, a stop needs it *and*
    /// the scheduler, the playback controls name fields on this context beside
    /// the clock they scale (a seek additionally drives the machine), a command
    /// list may contain any of the above and so can only be run by recursing
    /// into this function, a camera command needs the playback machine *or* the
    /// viewport's aspect ratio (for `focus`), a motion command names a field on
    /// this context rather than in the tree, a narration needs the virtual clock
    /// and its own track, a query needs the viewport to project bounds — and
    /// `apply`'s documented purity — "pure with respect to everything except
    /// `tree`: no clock, no I/O, no renderer" — is what makes the rest of the
    /// protocol testable in isolation and replayable.
    /// The wire→animation and wire→camera mappings still live in `command`, next
    /// to the wire types.
    ///
    /// A [`Command::Stop`] calls [`SceneContext::stop`] and answers with an
    /// empty diff: the timeline ends where the last frame left it, and no
    /// `finish` runs — see that method for what an interrupt deliberately does
    /// *not* do.
    ///
    /// A [`Command::Checkpoint`] pushes a snapshot and answers with an empty
    /// diff (a mark changes nothing); a [`Command::Undo`] restores the newest
    /// one and is the **only** host-answered command whose diff names nodes.
    /// Both need this context rather than the tree — the snapshot is the tree
    /// *with* its camera, clock, pause and tempo — and an undo interrupts
    /// playback, so it needs the scheduler and the queue too. See
    /// [`SceneContext::checkpoint`] and [`SceneContext::undo`] for what is and
    /// is not captured.
    ///
    /// A [`Command::Batch`] runs its members through this same function, in
    /// order, and answers with the **net** diff of the whole list — computed by
    /// comparing the tree before and after, so a create-then-remove cancels. A
    /// member's refusal stops the list and comes back as
    /// [`CommandError::InBatch`] with its index; the members before it stay
    /// applied. A [`Command::Validate`] runs the same list against a scratch
    /// copy and answers with an empty diff when all of it would be accepted.
    /// Both are documented at [`SceneContext::batch`] and
    /// [`SceneContext::validate`].
    ///
    /// A camera command without a `duration` writes `tree.camera` now; one with a
    /// duration (including `0.0`) queues a single `CameraMove` beat, so it is
    /// subject to the same clock, reduced-motion policy and beat ordering as
    /// everything else. **An instant camera command is overwritten by a move
    /// already in flight**, the same way a `Set position` loses to a running
    /// `MoveTo` — an animation owns its subject until it ends.
    ///
    /// The returned diff is **empty**, and that is the honest answer rather than a
    /// placeholder: a diff describes the tree, and queueing a beat does not change
    /// the tree — it changes what will happen to it — while the camera is not a
    /// node at all and the motion mode is not either. The animation would report
    /// its own modification when it runs; an `ANIMATION_DONE` announcement is how
    /// a host learns that it did.
    ///
    /// A [`Command::Motion`] writes `self.motion` and answers with an empty diff
    /// for the same reason; it reads back through [`SceneContext::motion`]'s field,
    /// not through the tree.
    ///
    /// A [`Command::Events`] writes the session's event subscription and answers
    /// with an empty diff: a subscription is not a node. The kinds it may name
    /// are [`crate::event::EventKind`]'s, and an empty list is refused under
    /// `kinds` rather than read as "nothing" — `{"enabled":false}` is the
    /// spelling for stopping the stream. What the engine records is unaffected;
    /// see [`SceneContext::take_events`].
    ///
    /// A [`Command::Narrate`] is queued on the narration track at
    /// `clock + at`, and answers with an empty diff because speaking is not a
    /// tree change. It refuses two things a model can easily send: nothing to
    /// say (`text` trims to empty) and a delay that is negative or not finite
    /// (`at`). Both name the field that was wrong.
    ///
    /// A [`Command::Query`] is the one command answered with a *document* rather
    /// than a diff: [`CommandOutcome::Queried`] carries the same
    /// [`a11y::SemanticTree`] `--inspect` prints, projected against this
    /// context's viewport and the tree's current camera. A `target` narrows it
    /// to that node's subtree and is refused when the node does not exist. The
    /// `live` field is `None` — the announcement text belongs to a host's
    /// accessibility layer, not to the kernel.
    ///
    /// A [`Command::Hello`] is the handshake: it answers with
    /// [`CommandOutcome::Greeted`], i.e. [`SceneContext::capabilities`] — the
    /// vocabularies this *build* accepts, derived from the serde definitions
    /// themselves and narrowed by whether the host installed a typesetter for
    /// `text` / `formula`. A `protocol` naming a version this build does not
    /// speak is [`CommandError::OutOfRange`] under `protocol`, with both
    /// numbers in the detail — refusing is what stops a client from believing
    /// its v2 field was merely optional here, and the numbers are what it needs
    /// to fall back.
    ///
    /// An [`Command::Add`] of `text` or `formula` is the one *mutation* answered
    /// here rather than by [`command::apply`], for the same purity reason: a
    /// text mobject's geometry comes from the host's [`Typesetter`], and
    /// `apply` may not read a font. A context with no provider — or one that
    /// claims only half the pair — refuses with
    /// [`CommandError::Unsupported`], naming the missing feature; a provider
    /// that cannot compile the particular source refuses with the protocol's
    /// own vocabulary, a [`CommandError::OutOfRange`] under `text` for source
    /// the caller can fix and [`CommandError::Typeset`] for a backend failure
    /// it cannot. Every other `add` — groups included — still goes through the
    /// pure path.
    pub fn handle(&mut self, cmd: &Command) -> Result<CommandOutcome, CommandError> {
        if let Command::Hello { protocol } = cmd {
            if let Some(client) = protocol
                && *client != command::PROTOCOL_VERSION
            {
                return Err(CommandError::OutOfRange {
                    property: "protocol".to_owned(),
                    detail: format!(
                        "client speaks {client}; this engine speaks {}",
                        command::PROTOCOL_VERSION
                    ),
                });
            }
            return Ok(CommandOutcome::Greeted(self.capabilities()));
        }
        if let Command::Play { animation } = cmd {
            let anim = command::animation_for(&self.tree, animation)?;
            self.play_all([anim]);
            return Ok(CommandOutcome::Applied(StateDiff::default()));
        }
        if let Command::Stop = cmd {
            self.stop();
            return Ok(CommandOutcome::Applied(StateDiff::default()));
        }
        if let Command::Pause = cmd {
            self.pause();
            return Ok(CommandOutcome::Applied(StateDiff::default()));
        }
        if let Command::Resume = cmd {
            self.resume();
            return Ok(CommandOutcome::Applied(StateDiff::default()));
        }
        if let Command::Tempo { factor } = cmd {
            if !factor.is_finite() || *factor <= 0.0 {
                return Err(CommandError::OutOfRange {
                    property: "factor".to_owned(),
                    detail: format!(
                        "{factor} is not a finite, positive speed (pause is its own command)"
                    ),
                });
            }
            if !(Self::MIN_TEMPO..=Self::MAX_TEMPO).contains(factor) {
                return Err(CommandError::OutOfRange {
                    property: "factor".to_owned(),
                    detail: format!(
                        "{factor} is outside the playback range {}..={}",
                        Self::MIN_TEMPO,
                        Self::MAX_TEMPO
                    ),
                });
            }
            self.set_tempo(*factor);
            return Ok(CommandOutcome::Applied(StateDiff::default()));
        }
        if let Command::Seek { to } = cmd {
            self.seek(*to)?;
            return Ok(CommandOutcome::Applied(StateDiff::default()));
        }
        if let Command::Checkpoint = cmd {
            self.checkpoint();
            return Ok(CommandOutcome::Applied(StateDiff::default()));
        }
        if let Command::Undo = cmd {
            // The one host-answered command whose diff is *not* empty, and
            // honestly so: restoring a tree changes it, so the reply has to say
            // what moved or a host would have to re-query to find out.
            return self.undo().map(CommandOutcome::Applied);
        }
        if let Command::Batch { commands } = cmd {
            // The net diff comes from comparing the tree before and after the
            // list, not from the members' own replies, so a create-then-remove
            // cancels and a create-then-move is `created` alone.
            return self.batch(commands).map(CommandOutcome::Applied);
        }
        if let Command::Validate { commands } = cmd {
            // A dry run changes nothing, so the diff is present and empty:
            // silence in the error channel is the answer, and an empty diff is
            // the honest statement that the tree did not move.
            self.validate(commands)?;
            return Ok(CommandOutcome::Applied(StateDiff::default()));
        }
        if let Command::Motion { mode } = cmd {
            self.motion = (*mode).into();
            return Ok(CommandOutcome::Applied(StateDiff::default()));
        }
        if let Command::Events { enabled, kinds } = cmd {
            if let Some(kinds) = kinds
                && kinds.is_empty()
            {
                return Err(CommandError::OutOfRange {
                    property: "kinds".to_owned(),
                    detail: "an empty filter selects nothing; use {\"enabled\":false} to stop the stream"
                        .to_owned(),
                });
            }
            self.published =
                enabled.then(|| kinds.clone().unwrap_or_else(|| EventKind::ALL.to_vec()));
            return Ok(CommandOutcome::Applied(StateDiff::default()));
        }
        if let Command::Narrate { text, at } = cmd {
            let text = text.trim();
            if text.is_empty() {
                return Err(CommandError::OutOfRange {
                    property: "text".to_owned(),
                    detail: "narration needs something to say".to_owned(),
                });
            }
            let delay = at.unwrap_or(0.0);
            if !delay.is_finite() || delay < 0.0 {
                return Err(CommandError::OutOfRange {
                    property: "at".to_owned(),
                    detail: format!("{delay} is not a finite, non-negative delay"),
                });
            }
            // `at` is relative to arrival, and the clock is where the timeline
            // already is: a file feed applies every line at t = 0, so the delay
            // is the only thing that keeps the lecture in order.
            self.narration.push((self.clock + delay, text.to_owned()));
            return Ok(CommandOutcome::Applied(StateDiff::default()));
        }
        if let Command::Annotate {
            target,
            text,
            direction,
            buff,
            length,
            thickness,
            arrow,
            color,
            size,
            max_width,
            label,
        } = cmd
        {
            if !self.supports_text() {
                return Err(CommandError::Unsupported {
                    feature: "text".to_owned(),
                });
            }
            // Resolution before typesetting: a stale id is refused without
            // shaping a line nobody will see.
            let target = target.resolve(&self.tree)?;
            let spec = TextSpec {
                size: *size,
                max_width: *max_width,
            };
            let block = {
                let Some(typesetter) = self.typesetter.as_mut() else {
                    return Err(CommandError::Unsupported {
                        feature: "text".to_owned(),
                    });
                };
                typesetter
                    .text(text, &spec)
                    .map_err(|error| typeset_error(error, "text"))?
            };
            // The label's own size decides where it goes, so the caller's box
            // is the typesetter's, not a guess from the string length. A block
            // with no extent (an empty line) still places, half an extent of
            // nothing away from the tail.
            let extent = block
                .local_bounds()
                .map_or(Vec2::ZERO, |r| Vec2::new(r.width(), r.height()));
            let style = rustim_director::CalloutStyle {
                direction: direction.unwrap_or(command::Direction::Down).into(),
                buff: buff.unwrap_or(rustim_director::DEFAULT_POINT_AT_BUFF),
                length: length.unwrap_or(rustim_director::DEFAULT_POINT_AT_LENGTH),
                thickness: *thickness,
                arrow: arrow.unwrap_or(true),
            };
            // Geometry before insertion, so a refused callout leaves the tree
            // exactly as it found it — the same atomicity the batch command
            // keeps, and what lets a model retry a bad `length` without first
            // cleaning up half a callout.
            let callout = rustim_director::callout(&self.tree, target, extent, style)
                .map_err(|error| command::layout_error("target", error))?;
            let params = ShapeParams {
                text: Some(text.clone()),
                size: *size,
                max_width: *max_width,
                color: *color,
                label: label.clone(),
                position: Some([callout.label_center.x, callout.label_center.y]),
                ..ShapeParams::default()
            };
            let text_id = self
                .tree
                .insert(rustim_mobject::shapes::build(ShapeKind::Text, &params).with_text(block));
            let mut diff = StateDiff::default();
            diff.created.push(text_id);
            if let Some(mut arrow) = callout.arrow {
                // The shape builder paints with the foreground when no colour
                // was given; a colour named here replaces both halves, matching
                // `point_at`.
                if let Some([r, g, b]) = color {
                    let paint = rustim_color::Srgb::from_rgba8(*r, *g, *b, 255);
                    arrow.style.fill = paint;
                    arrow.style.stroke = paint;
                    arrow.dirty.style = true;
                }
                diff.created.push(self.tree.insert(arrow));
            }
            return Ok(CommandOutcome::Applied(diff));
        }
        if let Command::ExplainEquation {
            formula,
            position,
            scale,
            color,
            caption,
            caption_size,
            label,
        } = cmd
        {
            if !self.supports_formula() {
                return Err(CommandError::Unsupported {
                    feature: "formula".to_owned(),
                });
            }
            let Some(typesetter) = self.typesetter.as_mut() else {
                // Unreachable by the line above — `supports_formula` reads the
                // same field — and answered rather than unwrapped because this
                // path takes input a model wrote.
                return Err(CommandError::Unsupported {
                    feature: "formula".to_owned(),
                });
            };
            let args = crate::templates::Equation {
                formula,
                position: *position,
                scale: *scale,
                color: *color,
                caption: caption.as_deref(),
                caption_size: *caption_size,
                label: label.as_deref(),
            };
            return crate::templates::equation(&mut self.tree, typesetter.as_mut(), args)
                .map(CommandOutcome::Applied);
        }
        if let Command::ExplainPlot {
            coefficients,
            x_range,
            y_range,
            width,
            height,
            samples,
            step,
            color,
            position,
            caption,
            caption_size,
            label,
        } = cmd
        {
            // The tick labels are mandatory, so the whole template needs text
            // rather than only its optional caption. `templates::plot` makes
            // the same check as its first act; this one keeps the refusal
            // beside the capability query a host reads from `hello`.
            if !self.supports_text() {
                return Err(CommandError::Unsupported {
                    feature: "text".to_owned(),
                });
            }
            let Some(typesetter) = self.typesetter.as_mut() else {
                return Err(CommandError::Unsupported {
                    feature: "text".to_owned(),
                });
            };
            let args = crate::templates::Plot {
                coefficients,
                x_range: *x_range,
                y_range: *y_range,
                width: *width,
                height: *height,
                samples: *samples,
                step: *step,
                color: *color,
                position: *position,
                caption: caption.as_deref(),
                caption_size: *caption_size,
                label: label.as_deref(),
            };
            return crate::templates::plot(&mut self.tree, typesetter.as_mut(), args)
                .map(CommandOutcome::Applied);
        }
        if let Command::Highlight {
            target,
            dim,
            duration,
            hold,
        } = cmd
        {
            let target = target.resolve(&self.tree)?;
            let dim = dim.unwrap_or(command::DEFAULT_HIGHLIGHT_DIM);
            if !dim.is_finite() || !(0.0..=1.0).contains(&dim) {
                return Err(CommandError::OutOfRange {
                    property: "dim".to_owned(),
                    detail: format!("{dim} is not a dimming factor in 0.0..=1.0"),
                });
            }
            let duration = duration.unwrap_or(command::DEFAULT_HIGHLIGHT_DURATION);
            if !duration.is_finite() || duration < 0.0 {
                return Err(CommandError::OutOfRange {
                    property: "duration".to_owned(),
                    detail: format!("{duration} is not a finite, non-negative duration"),
                });
            }
            let hold = hold.unwrap_or(command::DEFAULT_HIGHLIGHT_HOLD);
            if !hold.is_finite() || hold < 0.0 {
                return Err(CommandError::OutOfRange {
                    property: "hold".to_owned(),
                    detail: format!("{hold} is not a finite, non-negative hold"),
                });
            }
            // The nodes to dim, and the opacities to put back. Both are read
            // **now**, before any beat exists: the restore has to return the
            // scene to the state the highlight found, and an `Animate` that
            // captured its destination later would return it to whatever the
            // dim left behind.
            let dimmed: Vec<(rustim_mobject::NodeId, f64)> =
                rustim_director::spotlight(&self.tree, target)
                    .map_err(|error| command::layout_error("target", error))?
                    .into_iter()
                    .filter_map(|id| self.tree.get(id).map(|m| (id, m.opacity)))
                    .collect();
            // `dim == 1.0` is a well-formed no-op ("dim nothing"), and a scene
            // with nothing outside the subject has nothing to dim. Either way
            // there is no beat to queue, and queuing an empty one would make
            // `is_playing` flicker for a command that asked for nothing.
            if !dimmed.is_empty() && dim < 1.0 {
                let dim_group = dimmed
                    .iter()
                    .map(|&(id, was)| {
                        Box::new(
                            Animate::new(id, StyleProperty::Opacity(was * dim))
                                .with_duration(duration),
                        ) as Box<dyn Animation>
                    })
                    .collect::<Vec<_>>();
                // The restore captures each node's *dimmed* value when its turn
                // arrives (`Animate::begin`) and blends to the opacity this
                // command found — two independent statements, so the pair closes
                // even if something else touched the same node between them.
                let restore_group = dimmed
                    .iter()
                    .map(|&(id, was)| {
                        Box::new(
                            Animate::new(id, StyleProperty::Opacity(was)).with_duration(duration),
                        ) as Box<dyn Animation>
                    })
                    .collect::<Vec<_>>();
                self.play(Succession::new(vec![
                    Box::new(AnimationGroup::new(dim_group)),
                    Box::new(Wait::new(hold)),
                    Box::new(AnimationGroup::new(restore_group)),
                ]));
            }
            return Ok(CommandOutcome::Applied(StateDiff::default()));
        }
        if let Command::Camera {
            center,
            zoom,
            focus,
            duration,
        } = cmd
        {
            let to =
                command::camera_for(&self.tree, &self.viewport, *center, *zoom, focus.as_ref())?;
            match command::camera_duration(*duration)? {
                None => self.tree.camera = to,
                Some(seconds) => self.play(CameraMove::new(to).with_duration(seconds)),
            }
            return Ok(CommandOutcome::Applied(StateDiff::default()));
        }
        if let Command::Query { target } = cmd {
            let tree = self.semantic_tree(None);
            let tree = match target {
                None => tree,
                // The refusal is `NoSuchNode` (or a label refusal) rather than
                // an empty subtree: a host that asked about a node it believes
                // exists has a bug or a stale id, and a document with no nodes
                // reads as "it is empty", which is a different (and false)
                // answer.
                Some(target) => {
                    let id = target.resolve(&self.tree)?;
                    tree.subtree(id)
                        .ok_or(CommandError::NoSuchNode { target: id })?
                }
            };
            return Ok(CommandOutcome::Queried(tree));
        }
        if let Command::Add {
            kind,
            params,
            children,
        } = cmd
            && kind.needs_typesetter()
        {
            // The children rule belongs to `Group`, and saying so here rather
            // than letting `apply` answer "unsupported" would report the wrong
            // field: a `text` with children is a malformed payload whatever
            // this build can typeset.
            if children.is_some() {
                return Err(CommandError::OutOfRange {
                    property: "children".to_owned(),
                    detail: "only a group takes children".to_owned(),
                });
            }
            return Ok(CommandOutcome::Applied(self.add_typeset(*kind, params)?));
        }
        Ok(CommandOutcome::Applied(command::apply(
            &mut self.tree,
            cmd,
        )?))
    }

    /// An `add` of a kind no tree walk can build: ask the host's typesetter.
    ///
    /// The reason this arm lives in the context and not in `command::apply` is
    /// the same one that puts `play` here: `apply` is pure over the tree, and
    /// shaping text reads fonts and runs a typesetting engine. Splitting it this
    /// way keeps the rest of the protocol replayable through a bare tree.
    fn add_typeset(
        &mut self,
        kind: ShapeKind,
        params: &ShapeParams,
    ) -> Result<StateDiff, CommandError> {
        let (supported, feature) = match kind {
            ShapeKind::Text => (self.supports_text(), "text"),
            // `needs_typesetter` is the only gate on this call, and it names
            // exactly `Text` and `Formula`; everything else is the formula case.
            _ => (self.supports_formula(), "formula"),
        };
        if !supported {
            return Err(CommandError::Unsupported {
                feature: feature.to_owned(),
            });
        }
        let Some(typesetter) = self.typesetter.as_mut() else {
            // Unreachable by the two lines above — `supports_*` reads the same
            // field — and answered rather than unwrapped because this path
            // takes input a model wrote.
            return Err(CommandError::Unsupported {
                feature: feature.to_owned(),
            });
        };

        let mut diff = StateDiff::default();
        if kind == ShapeKind::Text {
            let spec = TextSpec {
                size: params.size,
                max_width: params.max_width,
            };
            let block = typesetter
                .text(params.text(), &spec)
                .map_err(|error| typeset_error(error, "text"))?;
            // Built through the same `shapes::build` every other kind uses, so
            // the label, colour, `position` and `scale` arrive by the one code
            // path. An empty string is a legitimate text object — it has a
            // label and an opacity and just draws nothing — which is why the
            // block is attached even when it is empty.
            let m = rustim_mobject::shapes::build(kind, params).with_text(block);
            diff.created.push(self.tree.insert(m));
            return Ok(diff);
        }

        let formula = typesetter
            .formula(params.text(), &mut self.tree)
            .map_err(|error| typeset_error(error, "text"))?;
        // `shapes::build` would have done exactly this and cannot: the compiler
        // owns the root. The styling is shared with the `explain.*` templates,
        // so a formula's colour lands on the same nodes whichever route built
        // it.
        crate::typeset::style_formula(&mut self.tree, &formula, params);
        diff.created.push(formula.root);
        Ok(diff)
    }

    fn supports_text(&self) -> bool {
        self.typesetter
            .as_ref()
            .is_some_and(|typesetter| typesetter.supports_text())
    }

    fn supports_formula(&self) -> bool {
        self.typesetter
            .as_ref()
            .is_some_and(|typesetter| typesetter.supports_formula())
    }

    /// Where the view is looking.
    ///
    /// A shortcut for `self.tree.camera`: the camera is part of the tree because
    /// that is the only mutable world an [`rustim_anim::Animation`] receives (see
    /// `SceneTree`'s doc), and a scene should not have to know that to ask.
    pub fn camera(&self) -> &Camera {
        &self.tree.camera
    }

    /// The world→screen transform for this frame: the window's projection under
    /// the scene's current camera.
    ///
    /// The one thing a scene that draws its own operations (text, most notably)
    /// must fold into them. There is no camera-free variant, on purpose — a
    /// caller that forgot the camera would draw a correct-looking picture of the
    /// wrong view.
    pub fn affine(&self) -> Affine2 {
        self.viewport.affine(self.camera())
    }

    /// Pixels per world unit under the current camera.
    pub fn pixels_per_unit(&self) -> f64 {
        self.viewport.pixels_per_unit(self.camera())
    }

    /// This scene, projected into the semantic tree.
    ///
    /// Pure and cheap enough to call per frame, which is what the accessibility
    /// layer does — see `rustim-app::a11y` for the volume at which that stops
    /// being true.
    ///
    /// There is deliberately no `..._json` sibling here: a host calls
    /// `serde_json::to_string(&ctx.semantic_tree(None))` on its own side, which
    /// works because [`a11y::SemanticTree`] is derived data with no platform
    /// types in it. (`serde_json` is a real dependency of this crate — the
    /// protocol's line parser lives in [`crate::command`] — but the projection
    /// itself does not own a JSON spelling.)
    pub fn semantic_tree(&self, live: Option<String>) -> a11y::SemanticTree {
        a11y::build(&self.tree, &self.viewport, live)
    }

    /// Open a frame: clear the list, fill the background, install the viewport.
    ///
    /// Every draw list starts here, whether it is produced by the default tree
    /// walk or by a scene that pushes its own ops, so the background colour and
    /// the world→screen transform are decided in exactly one place.
    pub fn begin_frame(&self, list: &mut DrawList) {
        list.clear(self.viewport.frame_width, self.viewport.frame_height);
        list.background = Some(rustim_color::BG);
        list.push(DrawOp::SetTransform(self.affine()));
    }

    /// Flatten the tree into a backend-neutral draw list.
    ///
    /// This is where the deferred transform is finally paid for: `to_bez_path`
    /// folds each mobject's own transform together with its ancestors'. Nothing
    /// upstream of this call ever materialises world coordinates.
    pub fn build_draw_list(&self, list: &mut DrawList) {
        self.begin_frame(list);
        // Computed once. Every glyph run needs it twice — to place the run and
        // then to put the frame transform back — and the projection cannot
        // change part-way through one walk.
        let frame = self.affine();
        for id in self.tree.draw_order() {
            let Some(m) = self.tree.get(id) else { continue };
            if m.opacity <= 0.0 {
                continue;
            }
            // A wire-created text mobject carries its shaped block here and is
            // drawn by this walk; a text object a *scene* shaped has no block
            // and pushes its own ops, which is why the test below is for the
            // block and not for the kind. The block is skipped rather than
            // drawn at zero alpha for the reason the fill pass is: a fully
            // transparent object should cost nothing.
            if let Some(block) = &m.text
                && !block.is_empty()
            {
                let alpha = m.opacity * m.style.fill_opacity;
                if alpha > 0.0 {
                    let mut color = m.style.fill;
                    color.components[3] = alpha as f32;
                    let world = self.tree.parent_transform(id) * m.transform;
                    for op in crate::text::text_ops(block, world, frame, color) {
                        list.push(op);
                    }
                    // The builder leaves the run's placement in the list's
                    // transform — a glyph run is placed by it — so the frame
                    // goes back before the next object draws a path.
                    list.push(DrawOp::SetTransform(frame));
                }
            }
            // Points rather than `Mobject::has_picture`, and deliberately so: this
            // walk emits paths — filled and/or stroked — so an object with no
            // points has nothing further to draw *and nothing is missing*: text
            // was either drawn from its block above or is drawn by the scene that
            // shaped it. The wider predicate is what the keyboard and the
            // semantic tree ask, and they must not ask this one, or text would be
            // unreadable by both. One point is not a path either:
            // `bezier_path_from_points` needs a segment to emit.
            if m.points.len() < 2 {
                continue;
            }
            // The mobject's own transform is folded in here rather than via a
            // `SetTransform`, because the draw list's transform is global to the
            // frame (it carries the viewport) and mixing the two would require
            // save/restore. The *ancestors'* transform is folded in by handing
            // `to_bez_path` the accumulated parent transform — passing
            // `Affine2::IDENTITY` instead silently drops every ancestor transform,
            // which is invisible only while the tree stays flat.
            //
            // Built once per object and handed to whichever passes run: a shape
            // with both a fill and a stroke (an arrow with an outline, say) must
            // not pay for two flattenings or, worse, two slightly different ones.
            let path = m.to_bez_path(self.tree.parent_transform(id));
            // Fill first, stroke second. Manim's cairo backend does the same,
            // and the order is visible whenever the two overlap: a stroked edge
            // sits half outside the fill, and drawing it underneath would let
            // the fill cover half the stroke's width.
            //
            // `opacity` composes multiplicatively with each channel's own
            // opacity, which is the convention this engine states once and both
            // passes obey. A pass whose alpha or width is zero is skipped rather
            // than emitted transparent — an invisible stroke is not free, and a
            // zero-width `StrokePath` is geometry the stroker should never see.
            if m.style.fill_opacity > 0.0 {
                let mut color = m.style.fill;
                color.components[3] = (m.opacity * m.style.fill_opacity) as f32;
                list.push(DrawOp::SetPaint(color));
                list.push(DrawOp::FillPath(path.clone()));
            }
            if m.style.stroke_opacity > 0.0
                && m.style.stroke_width > 0.0
                && m.style.stroke_width.is_finite()
            {
                let mut color = m.style.stroke;
                color.components[3] = (m.opacity * m.style.stroke_opacity) as f32;
                list.push(DrawOp::SetPaint(color));
                list.push(DrawOp::StrokePath {
                    path,
                    width: m.style.stroke_width,
                });
            }
        }
    }
}

/// A typesetter refusal, expressed as the protocol's.
///
/// The split is the one a caller can act on: an `Invalid` source is the
/// command's own `text` field and can be rewritten, while a `Failed` backend is
/// the build's problem and retrying the same sentence will not help. Naming
/// `text` for both kinds is deliberate — it is the field the command carried,
/// and a refusal that named `formula` would send a model looking for a field it
/// never wrote.
#[cfg(test)]
mod tests {
    use super::*;
    use rustim_anim::{FadeIn, FadeOut, Motion, MoveTo, Wait, rate};
    use rustim_math::Affine2;
    use rustim_mobject::shapes::{ShapeKind, ShapeParams};
    use rustim_mobject::text::FontData;
    use rustim_mobject::{Mobject, NodeKind, TextBlock, TextRun};
    use rustim_render::TextGlyph;

    use crate::typeset::{TypesetError, TypesetFormula};

    /// The frame grid. Virtual time, never wall-clock — the same `1/60` the app's
    /// frame loop and the golden capture use.
    const DT: f64 = 1.0 / 60.0;

    /// `handle` for the mutating commands these tests send, with the read half
    /// spelled as the panic it would be to see here.
    fn applied(ctx: &mut SceneContext, cmd: &Command) -> StateDiff {
        match ctx.handle(cmd).expect("a well-formed command") {
            CommandOutcome::Applied(diff) => diff,
            CommandOutcome::Queried(_) => panic!("a query is not a mutation"),
            CommandOutcome::Greeted(_) => panic!("a greeting is not a mutation"),
        }
    }

    fn scene() -> SceneContext {
        scene_at(Motion::Full)
    }

    /// A context in a chosen motion mode. Nothing but the timeline reads it, so a
    /// reduced scene is otherwise the same scene — which is the point of the mode.
    fn scene_at(motion: Motion) -> SceneContext {
        SceneContext::new(Viewport::default(), motion)
    }

    /// A shape that is invisible until something fades it in.
    ///
    /// A fresh mobject is opaque, and `FadeIn::begin` captures `from = m.opacity`,
    /// so without this every fade below would be a no-op that still takes its
    /// duration — a test that measures the clock and calls it a picture.
    fn invisible_square(ctx: &mut SceneContext) -> NodeId {
        let id = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        ctx.tree.get_mut(id).unwrap().opacity = 0.0;
        id
    }

    fn tick(ctx: &mut SceneContext, frames: usize) {
        for _ in 0..frames {
            ctx.advance(DT);
        }
    }

    fn opacity(ctx: &SceneContext, id: NodeId) -> f64 {
        ctx.tree.get(id).unwrap().opacity
    }

    /// The handshake answers with the build's vocabulary, changes nothing, and
    /// refuses a version it does not speak with both numbers in the detail.
    ///
    /// The refusal is the part a test is owed: a client that sent a v2 field
    /// must learn that this build is not v2, and the only useful spelling is one
    /// that names both sides — "out of range" alone would leave it guessing
    /// whether to downgrade or upgrade.
    #[test]
    fn hello_answers_with_capabilities_and_refuses_a_foreign_version() {
        let mut ctx = scene();
        let before = ctx.tree.len();

        match ctx
            .handle(&Command::Hello {
                protocol: Some(command::PROTOCOL_VERSION),
            })
            .expect("the current version is spoken")
        {
            CommandOutcome::Greeted(caps) => {
                assert_eq!(caps.protocol, command::PROTOCOL_VERSION);
                assert_eq!(caps.animations.len(), 11);
                // Ten of the twelve kinds: this context has no typesetter, so
                // it must not advertise the two that need one. Advertising them
                // and refusing the first `add` would teach a client to distrust
                // the handshake.
                assert_eq!(caps.shapes.len(), 10, "{:?}", caps.shapes);
                for absent in ["text", "formula"] {
                    assert!(
                        !caps.shapes.iter().any(|shape| shape == absent),
                        "{absent} advertised without a typesetter"
                    );
                }
                // The full protocol vocabulary still names them, which is what
                // makes the two lists different rather than redundant.
                assert_eq!(Capabilities::current().shapes.len(), 12);
            }
            other => panic!("expected a greeting, got {other:?}"),
        }

        // An absent version is "whatever you speak", not a refusal.
        assert!(matches!(
            ctx.handle(&Command::Hello { protocol: None })
                .expect("absent protocol is spoken"),
            CommandOutcome::Greeted(_)
        ));
        // A handshake is not a mutation, and these tests are the ones that would
        // notice if a future implementation made it one.
        assert_eq!(ctx.tree.len(), before);

        let err = ctx
            .handle(&Command::Hello {
                protocol: Some(command::PROTOCOL_VERSION + 1),
            })
            .expect_err("a foreign version is refused");
        assert_eq!(
            err,
            CommandError::OutOfRange {
                property: "protocol".to_owned(),
                detail: format!(
                    "client speaks 2; this engine speaks {}",
                    command::PROTOCOL_VERSION
                ),
            }
        );
    }

    #[test]
    fn adding_a_shape_puts_one_op_pair_on_the_list() {
        let mut ctx = scene();
        ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        let mut list = DrawList::default();
        ctx.build_draw_list(&mut list);
        assert_eq!(list.width, 1280);
        assert_eq!(list.background, Some(rustim_color::BG));
        let fills = list
            .ops
            .iter()
            .filter(|o| matches!(o, DrawOp::FillPath(_)))
            .count();
        assert_eq!(fills, 1);
    }

    #[test]
    fn a_fully_transparent_object_is_not_drawn() {
        let mut ctx = scene();
        let id = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Circle,
            &ShapeParams::default(),
        ));
        ctx.tree.get_mut(id).unwrap().opacity = 0.0;
        let mut list = DrawList::default();
        ctx.build_draw_list(&mut list);
        assert!(
            !list.ops.iter().any(|o| matches!(o, DrawOp::FillPath(_))),
            "an invisible object should cost nothing to draw"
        );
    }

    #[test]
    fn opacity_multiplies_into_the_paint() {
        let mut ctx = scene();
        let id = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        {
            let m = ctx.tree.get_mut(id).unwrap();
            m.opacity = 0.5;
            m.style.fill_opacity = 0.5;
        }
        let mut list = DrawList::default();
        ctx.build_draw_list(&mut list);
        let alpha = list
            .ops
            .iter()
            .find_map(|o| match o {
                DrawOp::SetPaint(c) => Some(c.components[3]),
                _ => None,
            })
            .unwrap();
        // Composes multiplicatively, per CLAUDE.md's convention.
        assert!((alpha - 0.25).abs() < 1e-6, "got {alpha}");
    }

    /// A plot reaches the backend as a **stroke**, never as a fill.
    ///
    /// This is the op-level half of `shapes::tests::a_plot_is_the_centerline…`:
    /// the geometry test can only say the points are the samples, while the
    /// defect it guards against (a graph painted as the area under itself) is a
    /// property of which command the draw walk emits.
    #[test]
    fn a_plot_is_stroked_and_not_filled() {
        let mut ctx = scene();
        ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Plot,
            &ShapeParams {
                points: Some(vec![[0.0, 0.0], [1.0, 1.0], [2.0, 0.0]]),
                thickness: Some(0.05),
                ..Default::default()
            },
        ));
        let mut list = DrawList::default();
        ctx.build_draw_list(&mut list);

        assert_eq!(
            list.ops
                .iter()
                .filter(|o| matches!(o, DrawOp::FillPath(_)))
                .count(),
            0,
            "a filled open polyline is the area under the curve"
        );
        let widths: Vec<f64> = list
            .ops
            .iter()
            .filter_map(|o| match o {
                DrawOp::StrokePath { width, .. } => Some(*width),
                _ => None,
            })
            .collect();
        assert_eq!(widths, vec![0.05]);
    }

    /// Fill first, stroke second, each with its own opacity folded into it.
    #[test]
    fn a_fill_and_a_stroke_carry_separate_alpha_and_fill_comes_first() {
        let mut ctx = scene();
        let id = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        {
            let m = ctx.tree.get_mut(id).unwrap();
            m.opacity = 0.5;
            m.style.fill_opacity = 0.4;
            m.style.stroke_opacity = 0.8;
            m.style.stroke_width = 0.1;
        }
        let mut list = DrawList::default();
        ctx.build_draw_list(&mut list);

        let mut paint = 0.0f32;
        let mut alphas = Vec::new();
        let mut order = Vec::new();
        for op in &list.ops {
            match op {
                DrawOp::SetPaint(c) => paint = c.components[3],
                DrawOp::FillPath(_) => {
                    alphas.push(paint);
                    order.push("fill");
                }
                DrawOp::StrokePath { .. } => {
                    alphas.push(paint);
                    order.push("stroke");
                }
                _ => {}
            }
        }
        assert_eq!(order, ["fill", "stroke"]);
        assert!((alphas[0] - 0.2).abs() < 1e-6, "{alphas:?}");
        assert!((alphas[1] - 0.4).abs() < 1e-6, "{alphas:?}");
    }

    #[test]
    fn a_zero_width_stroke_is_not_drawn() {
        let mut ctx = scene();
        let id = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        {
            let m = ctx.tree.get_mut(id).unwrap();
            m.style.stroke_opacity = 1.0;
            m.style.stroke_width = 0.0;
        }
        let mut list = DrawList::default();
        ctx.build_draw_list(&mut list);
        assert!(
            !list
                .ops
                .iter()
                .any(|o| matches!(o, DrawOp::StrokePath { .. }))
        );
    }

    #[test]
    fn commands_routed_through_the_context_stay_in_sync() {
        let mut ctx = scene();
        let cmd: Command =
            serde_json::from_str(r#"{"action":"add","type":"square","params":{"width":3.0}}"#)
                .unwrap();
        let diff = applied(&mut ctx, &cmd);
        assert_eq!(ctx.root_ids(), diff.created.as_slice());
        assert_eq!(ctx.tree.len(), 1);
    }

    /// A query answers with the document `--inspect` prints — the *same*
    /// projection, not a second read format — and with no diff.
    ///
    /// The camera is moved first on purpose: the snapshot must be of the tree
    /// as it is now, and pixel bounds only mean something when the view that
    /// produced them rides along.
    #[test]
    fn a_query_answers_with_the_semantic_tree() {
        let mut ctx = scene();
        let id = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        ctx.tree.get_mut(id).unwrap().label = Some("方块".to_owned());
        applied(&mut ctx, &camera_cmd(Some([1.0, 0.0]), Some(2.0), None));

        let outcome = ctx.handle(&Command::Query { target: None }).unwrap();
        let CommandOutcome::Queried(tree) = outcome else {
            panic!("a query is a read, not a mutation");
        };
        assert_eq!(tree.nodes.len(), 2, "the synthetic root and one mobject");
        assert_eq!(tree.root, crate::a11y::ROOT);
        assert_eq!(tree.camera.center, [1.0, 0.0]);
        assert_eq!(tree.camera.zoom, 2.0);
        assert!(
            tree.live.is_none(),
            "the announcement text belongs to the host, not the kernel"
        );
        let node = tree.get(id).expect("the square is in the document");
        assert_eq!(node.name, "方块");
        assert!(node.bounds.is_some(), "bounds are measured, not declared");
        assert_eq!(
            tree,
            ctx.semantic_tree(None),
            "a query is the accessibility projection, not a third view of the scene"
        );
    }

    /// A targeted query keeps the node and everything under it, and refuses a
    /// node that is not there rather than answering with an empty document.
    #[test]
    fn a_query_with_a_target_keeps_only_that_subtree() {
        let mut ctx = scene();
        let parent = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        let child = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Circle,
            &ShapeParams::default(),
        ));
        let sibling = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Circle,
            &ShapeParams::default(),
        ));
        assert!(ctx.tree.reparent(child, parent));

        let outcome = ctx
            .handle(&Command::Query {
                target: Some(parent.into()),
            })
            .unwrap();
        let CommandOutcome::Queried(tree) = outcome else {
            panic!("a query is a read");
        };
        assert_eq!(tree.root, parent, "the subtree is the document now");
        let ids: Vec<NodeId> = tree.nodes.iter().map(|n| n.id).collect();
        assert!(ids.contains(&parent) && ids.contains(&child));
        assert!(!ids.contains(&sibling), "a sibling is not in the subtree");
        assert_eq!(tree.get(parent).unwrap().children, [child]);

        let ghost = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Circle,
            &ShapeParams::default(),
        ));
        ctx.tree.remove(ghost);
        assert!(matches!(
            ctx.handle(&Command::Query {
                target: Some(ghost.into())
            }),
            Err(CommandError::NoSuchNode { .. })
        ));
    }

    /// A query may name its subject by label, and the document is rooted at the
    /// node that label resolves to. The refusal for a label no node carries is
    /// `NoSuchLabel`, which is a different bug from a stale id.
    #[test]
    fn a_query_can_name_its_subject_by_label() {
        let mut ctx = scene();
        let id = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        ctx.tree.get_mut(id).unwrap().label = Some("方块".into());

        let outcome = ctx
            .handle(&Command::Query {
                target: Some("方块".into()),
            })
            .unwrap();
        let CommandOutcome::Queried(tree) = outcome else {
            panic!("a query is a read");
        };
        assert_eq!(tree.root, id);
        assert_eq!(tree.nodes.len(), 1);
        assert_eq!(tree.get(id).unwrap().name, "方块");

        assert!(matches!(
            ctx.handle(&Command::Query {
                target: Some("不存在".into())
            }),
            Err(CommandError::NoSuchLabel { .. })
        ));
    }

    #[test]
    fn reset_empties_everything() {
        let mut ctx = scene();
        ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        ctx.reset();
        assert!(ctx.tree.is_empty());
        assert!(ctx.root_ids().is_empty());
        assert!(ctx.animations.is_empty());
        assert!(!ctx.is_playing());
    }

    /// **The queue is drained by `advance`, not by anything else.**
    ///
    /// Three claims in one run: nothing happens before the first `advance`; the
    /// first `advance` already delivers a frame of the animation (it does not spend
    /// a frame starting it); and the frame it delivers is the animation's *own*
    /// zero plus one `dt`, not one further along — the context-level statement of
    /// the counterfactual in `rustim-anim`'s
    /// `an_animation_added_late_starts_from_its_own_zero`.
    #[test]
    fn play_queues_a_beat_and_advance_runs_it() {
        let mut ctx = scene();
        let id = invisible_square(&mut ctx);
        ctx.play(FadeIn::new(id).with_duration(0.5));

        assert_eq!(opacity(&ctx, id), 0.0, "nothing plays before an advance");
        assert!(ctx.is_playing());

        tick(&mut ctx, 1);
        let first = opacity(&ctx, id);
        let want = rate::ease_in_out(DT / 0.5);
        assert!(
            (first - want).abs() < 1e-12,
            "the first frame must be the animation's own `dt / duration`; got {first}, \
             want {want}"
        );

        // 0.5 s at 1/60 s per frame, plus the frame that carries it past the end.
        tick(&mut ctx, 40);
        assert_eq!(opacity(&ctx, id), 1.0, "a faded object is exactly opaque");
        assert!(!ctx.is_playing());
    }

    /// A beat starts when the one before it has drained — and not a frame sooner.
    ///
    /// The witness is a *second* object rather than the first one's transform: a
    /// `MoveTo` writes its start transform on its first frame, so a moved-but-not-
    /// yet-moved object is indistinguishable from one the second beat has just
    /// picked up. Opacity has no such ambiguity, because `FadeIn::begin` captures
    /// zero and the first frame writes strictly above it.
    #[test]
    fn beats_run_one_after_another() {
        let mut ctx = scene();
        let a = invisible_square(&mut ctx);
        let b = invisible_square(&mut ctx);
        ctx.play(FadeIn::new(a).with_duration(0.5));
        ctx.play(FadeIn::new(b).with_duration(0.5));

        // Half a second in: the first fade is under way and the second has not been
        // touched at all. (30 frames of `fl(1/60)` sum to just under 0.5, so the
        // first beat is still running here — which is what makes the `b == 0.0`
        // assertion below about *sequencing* and not about timing luck.)
        tick(&mut ctx, 30);
        assert!(opacity(&ctx, a) > 0.0, "the first beat must be playing");
        assert_eq!(
            opacity(&ctx, b),
            0.0,
            "the second beat started while the first was still playing"
        );

        // The frame after the first drains, the second is already delivering.
        tick(&mut ctx, 2);
        assert_eq!(opacity(&ctx, a), 1.0);
        assert!(opacity(&ctx, b) > 0.0);

        tick(&mut ctx, 40);
        assert_eq!(opacity(&ctx, b), 1.0);
        assert!(!ctx.is_playing());
    }

    /// `play_all` is Manim's `play(a, b)`: **one** beat, whose members are driven
    /// on the same frames from one shared clock — and, being boxed, they are not
    /// even required to have the same type.
    #[test]
    fn play_all_runs_a_mixed_beat_in_parallel() {
        let mut ctx = scene();
        let a = invisible_square(&mut ctx);
        let b = invisible_square(&mut ctx);
        let beat: Vec<Box<dyn Animation>> = vec![
            Box::new(FadeIn::new(a).with_duration(0.5)),
            Box::new(MoveTo::new(b, Affine2::translate((3.0, 0.0))).with_duration(0.5)),
        ];
        ctx.play_all(beat);

        tick(&mut ctx, 10);
        let eased = rate::ease_in_out(10.0 * DT / 0.5);
        assert!(
            (opacity(&ctx, a) - eased).abs() < 1e-12,
            "the fade is {} frames into a 0.5 s beat",
            10
        );
        // Same clock, same reading: a `MoveTo` interpolates `start → end`
        // componentwise, so at `eased` of the way it is at `3 · eased` on x.
        let (dx, dy) = {
            let c = ctx.tree.get(b).unwrap().transform.as_coeffs();
            (c[4], c[5])
        };
        assert!(
            (dx - 3.0 * eased).abs() < 1e-12 && dy.abs() < 1e-12,
            "the move is at ({dx}, {dy}), not 3 · {eased}"
        );

        tick(&mut ctx, 40);
        assert_eq!(opacity(&ctx, a), 1.0);
        assert_eq!(
            ctx.tree.get(b).unwrap().transform,
            Affine2::translate((3.0, 0.0))
        );
        assert!(!ctx.is_playing());
    }

    /// **The `ANIMATION_DONE` guard.**
    ///
    /// A host learns the timeline is over by watching this predicate. The frame it
    /// must not be wrong on is the one right after a `play` call: the scheduler is
    /// still empty (the beat has not started) while the timeline is plainly not
    /// over. A predicate spelled `!animations.is_empty()` reports "finished" there,
    /// and again in the gap between two beats — both invisible in a still frame and
    /// both a wrong announcement for a screen reader.
    #[test]
    fn is_playing_is_true_from_the_queue_to_the_last_drain() {
        let mut ctx = scene();
        assert!(!ctx.is_playing(), "an idle scene is not playing");

        let id = invisible_square(&mut ctx);
        ctx.play(FadeIn::new(id).with_duration(0.5));
        ctx.play(Wait::new(0.25));
        assert!(ctx.animations.is_empty(), "queued, not started");
        assert!(ctx.is_playing(), "and yet the timeline is not over");

        let mut frames = 0;
        while ctx.is_playing() {
            ctx.advance(DT);
            frames += 1;
            assert!(frames < 1000, "the timeline never drained");
        }
        // 0.5 s then 0.25 s of a 1/60 s grid is 45 frames, plus the frame each beat
        // is carried past its own end on. A `Wait` that did not hold the clock would
        // stop this well short of 45.
        assert!((45..=48).contains(&frames), "took {frames} frames");
        assert_eq!(opacity(&ctx, id), 1.0);
        assert!(!ctx.is_playing(), "and it really is over");
        assert!(ctx.animations.is_empty());
    }

    /// **An interrupt is not a fast-forward.**
    ///
    /// `stop` drops the running animation without running `finish`, so the tree
    /// keeps exactly the state the last frame wrote — half faded, half moved —
    /// and the next `play` starts from there. Driving the same fade to its end
    /// would take the object to 1.0; a settled `stop` must not, and neither may
    /// anything after it creep the value along.
    #[test]
    fn stop_freezes_the_picture_where_the_last_frame_left_it() {
        let mut ctx = scene();
        let id = invisible_square(&mut ctx);
        ctx.play(FadeIn::new(id).with_duration(1.0));
        ctx.play(Wait::new(0.5));

        tick(&mut ctx, 30); // half a second, and `ease_in_out(0.5) == 0.5`
        let half = opacity(&ctx, id);
        assert!((half - 0.5).abs() < 1e-12, "opacity {half} is not half-way");
        assert!(ctx.is_playing());

        let diff = applied(&mut ctx, &Command::Stop);
        assert_eq!(diff, StateDiff::default(), "stopping is not a tree change");
        assert!(!ctx.is_playing(), "the timeline is over, not paused");
        assert!(ctx.animations.is_empty());
        assert_eq!(ctx.playback.len(), 0, "the queued pause went too");

        // Idempotent: a second stop is a success that changes nothing. "Make it
        // stop" is satisfied by a scene that has stopped.
        let again = applied(&mut ctx, &Command::Stop);
        assert_eq!(again, StateDiff::default());
        assert!(!ctx.is_playing());

        // Nothing moves it afterwards: neither an `interpolate` nor a `finish`.
        tick(&mut ctx, 120);
        assert_eq!(
            opacity(&ctx, id),
            half,
            "a stopped animation must not creep"
        );

        // And the next beat starts from the frozen state rather than from zero.
        ctx.play(FadeIn::new(id).with_duration(0.5));
        tick(&mut ctx, 40);
        assert_eq!(opacity(&ctx, id), 1.0);
        assert!(!ctx.is_playing());
    }

    /// The difference between "stop" and "finish", stated on the one animation
    /// whose completion *deletes* something: an interrupted `FadeOut` leaves its
    /// target in the tree at the opacity it had reached.
    ///
    /// The counterfactual is in the same test, because a transparent object draws
    /// nothing either way — the picture cannot tell an interrupted fade from a
    /// completed one, exactly as it cannot see the `Box` forwarding trap. The tree
    /// is the only witness.
    #[test]
    fn stop_does_not_run_the_removal_an_animation_owes() {
        let mut ctx = scene();
        let id = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        ctx.play(FadeOut::new(id).with_duration(1.0));

        tick(&mut ctx, 30);
        let half = opacity(&ctx, id);
        assert!(half > 0.0 && half < 1.0, "opacity {half} is not mid-fade");

        applied(&mut ctx, &Command::Stop);
        tick(&mut ctx, 120);
        assert!(
            ctx.tree.get(id).is_some(),
            "an interrupted fade-out must not remove its target"
        );
        assert_eq!(opacity(&ctx, id), half);

        // The same script played out *does* remove it, so the assertion above is
        // about the interrupt and not about `FadeOut` being inert.
        let mut ctx = scene();
        let id = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        ctx.play(FadeOut::new(id).with_duration(1.0));
        while ctx.is_playing() {
            ctx.advance(DT);
        }
        assert!(ctx.tree.get(id).is_none());
    }

    /// A beat that was queued but never started is dropped without ever running:
    /// its animations do not `begin`, so the picture is untouched. Clearing only
    /// the scheduler would pass the two tests above and leave this one failing on
    /// the next frame, when the queue hands the dropped beat over anyway.
    #[test]
    fn stop_drops_beats_that_never_started() {
        let mut ctx = scene();
        let id = invisible_square(&mut ctx);
        ctx.play(Wait::new(0.5));
        ctx.play(FadeIn::new(id).with_duration(0.5));

        tick(&mut ctx, 10); // inside the pause; the fade is still queued
        assert_eq!(opacity(&ctx, id), 0.0);
        assert_eq!(ctx.playback.len(), 1, "the fade is waiting its turn");

        applied(&mut ctx, &Command::Stop);
        assert_eq!(ctx.playback.len(), 0);
        tick(&mut ctx, 120);
        assert_eq!(opacity(&ctx, id), 0.0, "a dropped beat must never run");
        assert!(!ctx.is_playing());
    }

    /// Narration is not an animation, so an interrupt does not cut the lecture
    /// track: a line already scheduled still falls due, and the command's own
    /// clock is not the scheduler's.
    #[test]
    fn stop_leaves_the_narration_track_alone() {
        let mut ctx = scene();
        ctx.handle(&Command::Narrate {
            text: "停下来".to_owned(),
            at: Some(0.25),
        })
        .unwrap();
        applied(&mut ctx, &Command::Stop);
        assert!(ctx.take_narration().is_empty(), "not due yet");

        tick(&mut ctx, 20);
        assert_eq!(ctx.take_narration(), vec!["停下来".to_owned()]);
    }

    /// A beat reports itself twice — once when it reaches the clock and once
    /// when its last animation ends — and says nothing in between.
    ///
    /// This is the pair a polling client cannot reconstruct: the tree is
    /// identical on either side of a beat boundary, so `query` alone can only
    /// tell a finished beat from a queued one by watching values converge. The
    /// clock in each event is the lecture clock at the boundary, which is what a
    /// host times its own narration against.
    #[test]
    fn a_beat_reports_its_start_and_its_end() {
        let mut ctx = scene();
        let id = invisible_square(&mut ctx);
        ctx.play(FadeIn::new(id).with_duration(0.5));

        tick(&mut ctx, 1);
        let started = ctx.take_events();
        assert_eq!(started.len(), 1, "{started:?}");
        match &started[0] {
            Event::BeatStarted {
                beat,
                members,
                duration,
                clock,
            } => {
                assert_eq!(*beat, 1);
                assert_eq!(*members, 1);
                assert_eq!(*duration, 0.5);
                assert!((*clock - DT).abs() < 1e-12, "{clock}");
            }
            other => panic!("{other:?}"),
        }

        tick(&mut ctx, 5);
        assert!(
            ctx.take_events().is_empty(),
            "a running beat reports nothing"
        );

        while ctx.is_playing() {
            ctx.advance(DT);
        }
        let done = ctx.take_events();
        assert_eq!(done.len(), 1, "{done:?}");
        match &done[0] {
            Event::AnimationDone { beat, clock } => {
                assert_eq!(*beat, 1);
                // The fade is half a second; one frame of overshoot at most.
                assert!((*clock - 0.5).abs() < 2.0 * DT, "{clock}");
            }
            other => panic!("{other:?}"),
        }
        assert!(ctx.take_events().is_empty(), "an event is said once");
    }

    /// Under reduced motion a beat still reports **the same members and the
    /// same duration**, because they describe the script's beat, not the single
    /// `Wait` that stood in for it. A client that counted scheduler entries
    /// would see one animation either way and get the beat wrong; this is why
    /// the event is per beat.
    #[test]
    fn reduced_motion_reports_the_beats_own_shape() {
        let mut ctx = scene_at(Motion::Reduced);
        let id = invisible_square(&mut ctx);
        ctx.play(FadeIn::new(id).with_duration(0.75));
        ctx.play(Wait::new(0.25));

        tick(&mut ctx, 1);
        match &ctx.take_events()[0] {
            Event::BeatStarted {
                beat,
                members,
                duration,
                ..
            } => {
                assert_eq!(*beat, 1);
                assert_eq!(*members, 1, "the script's members, not the hold");
                assert_eq!(*duration, 0.75);
            }
            other => panic!("{other:?}"),
        }
        // The picture is already the end state: reduced motion snaps, then holds.
        assert_eq!(opacity(&ctx, id), 1.0);

        while ctx.is_playing() {
            ctx.advance(DT);
        }
        let all = ctx.take_events();
        let beats: Vec<u64> = all
            .iter()
            .filter_map(|e| match e {
                Event::AnimationDone { beat, .. } => Some(*beat),
                Event::BeatStarted { .. } | Event::FrameReady { .. } => None,
            })
            .collect();
        assert_eq!(beats, [1, 2], "both beats complete, in order");
    }

    /// **An interrupt is not a completion.** A stopped beat never reports
    /// `animation_done`: no `finish` ran, the picture is frozen half-way, and a
    /// client told "done" would act on an end state that does not exist.
    #[test]
    fn a_stopped_beat_never_reports_completion() {
        let mut ctx = scene();
        let id = invisible_square(&mut ctx);
        ctx.play(FadeIn::new(id).with_duration(1.0));

        tick(&mut ctx, 10);
        assert!(matches!(
            ctx.take_events().as_slice(),
            [Event::BeatStarted { beat: 1, .. }]
        ));
        applied(&mut ctx, &Command::Stop);
        tick(&mut ctx, 30);
        assert!(
            ctx.take_events().is_empty(),
            "a stop is not a fast-forward and not an ending"
        );
        assert!(opacity(&ctx, id) > 0.0 && opacity(&ctx, id) < 0.2);
    }

    /// The subscription is read by `wants_event`, which is what a transport asks
    /// per event; an empty filter is refused because it selects nothing and
    /// `enabled: false` is the way to say stop.
    #[test]
    fn the_event_subscription_filters_and_refuses_an_empty_list() {
        let mut ctx = scene();
        assert!(!ctx.events_subscribed());
        assert!(!ctx.wants_event(EventKind::BeatStarted));

        let err = ctx
            .handle(&Command::Events {
                enabled: true,
                kinds: Some(vec![]),
            })
            .expect_err("an empty filter is refused");
        assert!(
            matches!(&err, CommandError::OutOfRange { property, .. } if property == "kinds"),
            "{err:?}"
        );

        let diff = applied(
            &mut ctx,
            &Command::Events {
                enabled: true,
                kinds: Some(vec![EventKind::BeatStarted]),
            },
        );
        assert!(diff.is_empty(), "a subscription is not a tree change");
        assert!(ctx.events_subscribed());
        assert!(ctx.wants_event(EventKind::BeatStarted));
        assert!(!ctx.wants_event(EventKind::FrameReady));

        // The kernel records every kind regardless; filtering is the
        // transport's job and `wants_event` is the one place it reads.
        ctx.play(Wait::new(0.5));
        tick(&mut ctx, 1);
        assert!(matches!(
            ctx.take_events().as_slice(),
            [Event::BeatStarted { .. }]
        ));

        applied(
            &mut ctx,
            &Command::Events {
                enabled: false,
                kinds: None,
            },
        );
        assert!(!ctx.events_subscribed());
        assert!(!ctx.wants_event(EventKind::BeatStarted));
    }

    /// A rewrite is a new session for the log — old events describe a picture
    /// that no longer exists and beat numbers start over — but the **transport
    /// stays subscribed**, because a subscription is a property of the client,
    /// not of the scene it happens to be watching.
    #[test]
    fn reset_clears_the_log_and_keeps_the_subscription() {
        let mut ctx = scene();
        let id = invisible_square(&mut ctx);
        ctx.play(FadeIn::new(id).with_duration(0.25));
        applied(
            &mut ctx,
            &Command::Events {
                enabled: true,
                kinds: None,
            },
        );
        tick(&mut ctx, 2);
        assert!(!ctx.take_events().is_empty());

        ctx.reset();
        assert!(ctx.events_subscribed(), "the client is still listening");
        assert!(ctx.take_events().is_empty(), "the old scene's log is gone");

        let id = invisible_square(&mut ctx);
        ctx.play(FadeIn::new(id).with_duration(0.25));
        tick(&mut ctx, 1);
        match &ctx.take_events()[0] {
            Event::BeatStarted { beat, .. } => assert_eq!(*beat, 1, "numbering starts over"),
            other => panic!("{other:?}"),
        }
    }

    /// An animation pushed straight into the scheduler is **not** reported.
    ///
    /// The queue is what events describe: a bypass skips the motion policy and
    /// the beat identity together, and inventing a beat number for it would
    /// make `beat` mean two things.
    #[test]
    fn a_hand_added_animation_is_not_a_beat() {
        let mut ctx = scene();
        let id = invisible_square(&mut ctx);
        ctx.animations.add(FadeIn::new(id).with_duration(0.25));
        while !ctx.animations.is_empty() {
            ctx.advance(DT);
        }
        assert!(ctx.take_events().is_empty());
    }

    /// Manim's `Scene.wait` is `play(Wait(duration))` (`scene.py:1242-1276`), and it
    /// is spelled that way here — which is what makes a pause hold the *script*
    /// rather than merely gap two frames: the beat behind it does not start early.
    #[test]
    fn wait_queues_a_pause_that_really_pauses() {
        let mut ctx = scene();
        let id = invisible_square(&mut ctx);
        ctx.wait(0.5);
        ctx.play(FadeIn::new(id).with_duration(0.5));

        tick(&mut ctx, 20); // a third of a second: inside the pause
        assert_eq!(opacity(&ctx, id), 0.0, "the pause did not hold the clock");
        assert!(ctx.is_playing(), "a pause is not the end of the timeline");

        tick(&mut ctx, 60);
        assert_eq!(opacity(&ctx, id), 1.0);
        assert!(!ctx.is_playing());
    }

    /// A reduced beat is not interpolated: it is *already* at its end state on the
    /// first frame, and stays there while its own length runs out.
    ///
    /// The interesting half of this is what it says about the frames after the
    /// first: an implementation that snapped the beat and forgot the hold would
    /// pass every assertion up to `!ctx.is_playing()` and get the beat's length
    /// wrong by the whole duration — a script that finishes early under reduced
    /// motion, which no still frame can see.
    #[test]
    fn reduced_motion_snaps_a_beat_instead_of_interpolating_it() {
        let mut ctx = scene_at(Motion::Reduced);
        let id = invisible_square(&mut ctx);
        ctx.play(FadeIn::new(id).with_duration(0.5));

        tick(&mut ctx, 1);
        assert_eq!(
            opacity(&ctx, id),
            1.0,
            "a reduced beat lands on its end state, not a slice of the way there"
        );

        // Still holding: 20 more frames is a third of a second, inside the beat.
        tick(&mut ctx, 20);
        assert_eq!(opacity(&ctx, id), 1.0);
        assert!(ctx.is_playing(), "the beat's length is unchanged");
    }

    /// The scene's *length* is a property of the script, not of the motion mode.
    ///
    /// Two beats of half a second each, run to exhaustion both ways at the same
    /// `dt`. Reduced motion removes frames; it must not remove time — otherwise
    /// every beat boundary in a reduced scene is earlier than the corresponding one
    /// in a full scene, and anything a scene does at a boundary (a relabel, an
    /// announcement, a next scene) drifts with the mode.
    #[test]
    fn reduced_motion_does_not_change_how_long_the_script_takes() {
        fn frames_to_drain(motion: Motion) -> usize {
            let mut ctx = scene_at(motion);
            let id = invisible_square(&mut ctx);
            ctx.play(FadeIn::new(id).with_duration(0.5));
            ctx.wait(0.25);
            ctx.play(FadeIn::new(id).with_duration(0.5));

            let mut frames = 0;
            while ctx.is_playing() {
                ctx.advance(DT);
                frames += 1;
                assert!(frames < 1000, "the script never drained");
            }
            frames
        }

        assert_eq!(
            frames_to_drain(Motion::Full),
            frames_to_drain(Motion::Reduced),
            "reduced motion changes the journey, not the length of the script"
        );
    }

    /// A beat an AI sends is reduced like any other, because `Command::Play` queues
    /// through the same funnel (`play_all`) rather than driving the scheduler.
    ///
    /// This is the test that would fail if the command path ever gained a private
    /// route to `ctx.animations`: a preference that a *host* can bypass is not a
    /// preference, and the model-facing surface is the one place where bypassing it
    /// would be easy to do by accident.
    #[test]
    fn a_play_command_is_reduced_too() {
        let mut ctx = scene_at(Motion::Reduced);
        let id = invisible_square(&mut ctx);
        let cmd = Command::Play {
            animation: command::AnimationSpec {
                target: Some(id.into()),
                kind: command::AnimationKind::FadeIn,
                duration: Some(0.5),
                to: None,
                angle: None,
                factor: None,
                property: None,
                about_point: None,
                into: None,
                path_arc: None,
                animations: None,
                lag_ratio: None,
                rate: None,
            },
        };
        ctx.handle(&cmd).unwrap();

        tick(&mut ctx, 1);
        assert_eq!(opacity(&ctx, id), 1.0);
        assert!(ctx.is_playing(), "and still holds the beat's own length");
    }

    /// The runtime switch takes effect at the **next beat started**.
    ///
    /// Both halves matter and neither is visible in a still: a switch that
    /// reached into the scheduler would snap a beat the viewer is already
    /// watching, and a switch that took effect one beat late would answer the
    /// user a whole animation after they asked. The running beat is allowed to
    /// finish its own journey — it made its decision when it was handed the
    /// clock — and the one queued behind it is reduced.
    #[test]
    fn a_motion_command_affects_the_next_beat_and_not_the_running_one() {
        let mut ctx = scene();
        let first = invisible_square(&mut ctx);
        let second = invisible_square(&mut ctx);
        ctx.play(FadeIn::new(first).with_duration(0.5));
        tick(&mut ctx, 1);
        let partial = opacity(&ctx, first);
        assert!(
            partial > 0.0 && partial < 1.0,
            "full motion, a slice of the way: {partial}"
        );

        let diff = applied(
            &mut ctx,
            &Command::Motion {
                mode: command::MotionMode::Reduced,
            },
        );
        assert_eq!(ctx.motion, Motion::Reduced);
        assert_eq!(
            diff,
            StateDiff::default(),
            "the mode is not a node, so the diff is empty"
        );

        tick(&mut ctx, 1);
        assert!(
            opacity(&ctx, first) < 1.0,
            "the beat on the clock was not snapped by a mode it never saw"
        );

        // Let the first beat run out. The second starts under `Reduced` and is
        // therefore at its end state on its very first frame.
        while ctx.is_playing() {
            ctx.advance(DT);
        }
        ctx.play(FadeIn::new(second).with_duration(0.5));
        tick(&mut ctx, 1);
        assert_eq!(opacity(&ctx, second), 1.0);

        // And back: the same switch in the other direction is a full-mode beat.
        ctx.handle(&Command::Motion {
            mode: command::MotionMode::Full,
        })
        .unwrap();
        assert_eq!(ctx.motion, Motion::Full);
    }

    /// A mode switch cannot replay what `construct` already decided to skip.
    ///
    /// Documenting this as a limit rather than treating it as one: a scene reads
    /// the mode while scripting and may omit a flourish it knows will not be
    /// animated, so the scripting that ran under `Reduced` cannot be re-run under
    /// `Full`. The test pins the honest consequence — the toggle is for the beats
    /// still queued, not for the scene's shape.
    #[test]
    fn turning_motion_back_on_does_not_replay_skipped_flourishes() {
        let mut ctx = scene_at(Motion::Reduced);
        let id = invisible_square(&mut ctx);
        // A scene that scripts differently under reduced motion.
        if !ctx.motion.is_reduced() {
            ctx.wait(1.0);
        }
        ctx.play(FadeIn::new(id).with_duration(0.25));

        ctx.handle(&Command::Motion {
            mode: command::MotionMode::Full,
        })
        .unwrap();
        // The beat was queued *after* the switch, so it is full motion: the
        // skipped `Wait` is not resurrected, which is exactly the limit.
        tick(&mut ctx, 1);
        let alpha = opacity(&ctx, id);
        assert!(alpha < 1.0, "a full beat is interpolated: {alpha}");

        let mut frames = 1;
        while ctx.is_playing() {
            ctx.advance(DT);
            frames += 1;
        }
        assert!(
            frames < 30,
            "only the 0.25 s fade should remain; the skipped Wait came back ({frames} frames)"
        );
    }

    /// **The two halves of `Command::Play` join here:** the wire mapping is
    /// `command::animation_for`, the queueing is this context, and the result is a
    /// beat that runs — while the command reports an empty diff, because queueing a
    /// beat changes what *will* happen to the tree and not the tree.
    #[test]
    fn a_play_command_queues_a_beat_that_then_runs() {
        let mut ctx = scene();
        let id = invisible_square(&mut ctx);
        let cmd = Command::Play {
            animation: command::AnimationSpec {
                target: Some(id.into()),
                kind: command::AnimationKind::FadeIn,
                duration: Some(0.5),
                to: None,
                angle: None,
                factor: None,
                property: None,
                about_point: None,
                into: None,
                path_arc: None,
                animations: None,
                lag_ratio: None,
                rate: None,
            },
        };

        let diff = applied(&mut ctx, &cmd);
        assert!(
            diff.is_empty(),
            "the tree is unchanged until the beat plays"
        );
        assert_eq!(opacity(&ctx, id), 0.0);
        assert!(ctx.is_playing());

        tick(&mut ctx, 40);
        assert_eq!(opacity(&ctx, id), 1.0);
        assert!(!ctx.is_playing());
    }

    /// A refused `Play` queues nothing. The alternative — validate late, or not at
    /// all — leaves an empty beat or a no-op beat on the timeline, so a host that
    /// handled the error and tried a different node would find the first attempt
    /// still occupying the clock.
    #[test]
    fn a_refused_play_command_queues_nothing() {
        let mut ctx = scene();
        let ghost = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        ctx.tree.remove(ghost);
        let cmd = Command::Play {
            animation: command::AnimationSpec {
                target: Some(ghost.into()),
                kind: command::AnimationKind::FadeIn,
                duration: None,
                to: None,
                angle: None,
                factor: None,
                property: None,
                about_point: None,
                into: None,
                path_arc: None,
                animations: None,
                lag_ratio: None,
                rate: None,
            },
        };

        assert!(matches!(
            ctx.handle(&cmd),
            Err(CommandError::NoSuchNode { .. })
        ));
        assert!(!ctx.is_playing(), "a refused command must not queue a beat");
    }

    /// A rewritten scene must not inherit the previous scene's script.
    ///
    /// Reachable without `construct` running twice, because `handle` can append
    /// beats from outside: a stale beat would play into a tree it was never written
    /// for, which is a wrong picture rather than a crash.
    #[test]
    fn reset_drops_the_queued_beats() {
        let mut ctx = scene();
        let id = invisible_square(&mut ctx);
        ctx.play(FadeIn::new(id));
        ctx.wait(1.0);
        assert!(ctx.is_playing());

        ctx.reset();
        assert!(!ctx.is_playing(), "the script goes with the tree");
        assert!(ctx.animations.is_empty());
        tick(&mut ctx, 10);
        assert!(!ctx.is_playing());
    }

    // ---- attention: dim the rest and put it back --------------------------

    fn square(ctx: &mut SceneContext, at: [f64; 2]) -> NodeId {
        ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams {
                width: Some(1.0),
                height: Some(1.0),
                position: Some(at),
                ..Default::default()
            },
        ))
    }

    /// A highlight is one beat: dim every picture outside the subject's subtree,
    /// hold, then restore **the opacity it found** — not 1.0, so a scene that
    /// was already partly faded comes back the way it was. The subject itself
    /// never moves.
    #[test]
    fn highlight_dims_the_rest_and_puts_every_opacity_back() {
        let mut ctx = scene();
        let subject = square(&mut ctx, [0.0, 0.0]);
        let other = square(&mut ctx, [3.0, 0.0]);
        ctx.tree.get_mut(other).unwrap().opacity = 0.8;

        let diff = applied(
            &mut ctx,
            &Command::Highlight {
                target: subject.into(),
                dim: Some(0.5),
                duration: Some(0.2),
                hold: Some(0.1),
            },
        );
        assert!(diff.is_empty(), "a queued beat is not a tree change");
        assert!(ctx.is_playing());

        // A tenth of a second into a two-tenths dim: strictly between the two
        // ends, so this is a ramp and not a jump.
        tick(&mut ctx, 6);
        let mid = opacity(&ctx, other);
        assert!(mid < 0.8 && mid > 0.4, "mid-dim opacity {mid}");
        assert_eq!(opacity(&ctx, subject), 1.0, "the subject stays lit");

        // Just past the dim, inside the hold: settled on 0.8 × 0.5 and staying.
        tick(&mut ctx, 11);
        assert!(
            (opacity(&ctx, other) - 0.4).abs() < 1e-12,
            "held at {}",
            opacity(&ctx, other)
        );
        // Past the hold, the restore is a ramp back up rather than a jump.
        tick(&mut ctx, 2);
        let rising = opacity(&ctx, other);
        assert!(rising > 0.4 && rising < 0.8, "restoring through {rising}");

        // Past dim + hold + restore: back to exactly what it was, and done.
        tick(&mut ctx, 30);
        assert!((opacity(&ctx, other) - 0.8).abs() < 1e-12);
        assert_eq!(opacity(&ctx, subject), 1.0);
        assert!(!ctx.is_playing(), "the beat ends where it started");
    }

    /// Nothing to dim is a success that queues nothing: a scene holding only its
    /// subject, and `dim: 1.0` — the well-formed way to ask for no dimming.
    /// Queuing an empty beat would make `is_playing` flicker for a command that
    /// asked for nothing.
    #[test]
    fn a_highlight_with_nothing_to_dim_queues_no_beat() {
        let mut ctx = scene();
        let subject = square(&mut ctx, [0.0, 0.0]);
        let highlight = |dim| Command::Highlight {
            target: subject.into(),
            dim,
            duration: None,
            hold: None,
        };

        applied(&mut ctx, &highlight(None));
        assert!(!ctx.is_playing());

        square(&mut ctx, [3.0, 0.0]);
        applied(&mut ctx, &highlight(Some(1.0)));
        assert!(!ctx.is_playing(), "dim 1.0 dims nothing");
    }

    #[test]
    fn highlight_refuses_a_bad_factor_and_a_bad_timing() {
        let mut ctx = scene();
        let id = square(&mut ctx, [0.0, 0.0]);
        square(&mut ctx, [3.0, 0.0]);

        for (dim, duration, hold, field) in [
            (Some(-0.1), None, None, "dim"),
            (Some(1.5), None, None, "dim"),
            (Some(f64::NAN), None, None, "dim"),
            (None, Some(-0.1), None, "duration"),
            (None, None, Some(f64::INFINITY), "hold"),
        ] {
            let cmd = Command::Highlight {
                target: id.into(),
                dim,
                duration,
                hold,
            };
            assert!(
                matches!(
                    ctx.handle(&cmd),
                    Err(CommandError::OutOfRange { ref property, .. }) if property == field
                ),
                "{field} must be refused: {cmd:?}"
            );
        }
        assert!(!ctx.is_playing(), "a refused highlight queues nothing");

        assert!(matches!(
            ctx.handle(&Command::Highlight {
                target: "无".into(),
                dim: None,
                duration: None,
                hold: None,
            }),
            Err(CommandError::NoSuchLabel { .. })
        ));
    }

    // ---- annotation: a label past an arrow --------------------------------

    fn annotate(
        target: NodeId,
        direction: Option<command::Direction>,
        arrow: Option<bool>,
        length: Option<f64>,
        color: Option<[u8; 3]>,
    ) -> Command {
        Command::Annotate {
            target: target.into(),
            text: "看这里".to_owned(),
            direction,
            buff: None,
            length,
            thickness: None,
            arrow,
            color,
            size: None,
            max_width: None,
            label: None,
        }
    }

    /// One callout, two nodes, one measurement. The fake typesetter reports
    /// "看这里" as 3 × 1, so with a 1×1 subject at the origin and the default
    /// gaps the label's centre lands at `-(0.5 + 0.1 + 1.0 + 0.1 + 0.5)` — the
    /// near edge a buff past the tail, not the centre: a longer label moves
    /// further out rather than covering the arrow.
    #[test]
    fn annotate_places_the_label_past_an_arrow_from_one_measurement() {
        let mut ctx = with_typesetter(FakeTypesetter::both());
        let subject = square(&mut ctx, [0.0, 0.0]);
        let diff = applied(
            &mut ctx,
            &annotate(subject, None, None, None, Some([100, 181, 246])),
        );

        assert_eq!(diff.created.len(), 2, "the label and its arrow");
        let (text, arrow) = (diff.created[0], diff.created[1]);
        assert_eq!(ctx.tree.get(text).unwrap().kind, NodeKind::Text);
        assert_eq!(ctx.tree.get(arrow).unwrap().kind, NodeKind::Arrow);
        assert!(
            ctx.tree.get(arrow).unwrap().label.is_none(),
            "one label on two nodes would be ambiguous"
        );

        let label = ctx.tree.world_bounds(text).unwrap();
        assert!(label.center().x.abs() < 1e-12, "{label:?}");
        assert!((label.center().y + 2.2).abs() < 1e-12, "{label:?}");
        // The arrow spans tip (a buff below the box) to tail (one length out).
        let shaft = ctx.tree.world_bounds(arrow).unwrap();
        assert!((shaft.y1 + 0.6).abs() < 0.05, "{shaft:?}");
        assert!((shaft.y0 + 1.6).abs() < 0.05, "{shaft:?}");

        // `color` reaches both halves, so one callout reads as one object.
        let paint = rustim_color::Srgb::from_rgba8(100, 181, 246, 255);
        assert_eq!(ctx.tree.get(text).unwrap().style.fill, paint);
        assert_eq!(ctx.tree.get(arrow).unwrap().style.fill, paint);
        assert_eq!(ctx.tree.get(arrow).unwrap().style.stroke, paint);
    }

    /// The other half of the placement: `up` mirrors the whole arrangement
    /// above the subject, and `arrow: false` keeps the placement (one arrow and
    /// gap away) while creating only the label.
    #[test]
    fn an_annotate_direction_and_arrow_flag_move_the_same_placement() {
        let mut ctx = with_typesetter(FakeTypesetter::both());
        let subject = square(&mut ctx, [0.0, 0.0]);
        let diff = applied(
            &mut ctx,
            &annotate(
                subject,
                Some(command::Direction::Up),
                Some(false),
                None,
                None,
            ),
        );
        assert_eq!(diff.created.len(), 1, "no arrow was asked for");
        let label = ctx.tree.world_bounds(diff.created[0]).unwrap();
        assert!(label.center().x.abs() < 1e-12, "{label:?}");
        assert!((label.center().y - 2.2).abs() < 1e-12, "{label:?}");
    }

    /// Every refusal is atomic: nothing is created, whatever failed. The
    /// geometry is computed before either node is inserted, so a model that
    /// sends a bad `length` retries against the scene it had, not half a
    /// callout.
    #[test]
    fn annotate_refuses_before_creating_anything() {
        let bare = scene();
        let mut bare = bare;
        let id = square(&mut bare, [0.0, 0.0]);
        let before = bare.tree.len();
        assert!(
            matches!(
                bare.handle(&annotate(id, None, None, None, None)),
                Err(CommandError::Unsupported { ref feature }) if feature == "text"
            ),
            "a build with no typesetter cannot typeset a label"
        );
        assert_eq!(bare.tree.len(), before);

        let mut ctx = with_typesetter(FakeTypesetter::both());
        let subject = square(&mut ctx, [0.0, 0.0]);
        let before = ctx.tree.len();
        assert!(matches!(
            ctx.handle(&annotate(subject, None, None, Some(0.0), None)),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "length"
        ));
        assert!(matches!(
            ctx.handle(&Command::Annotate {
                target: "无".into(),
                text: "看这里".to_owned(),
                direction: None,
                buff: None,
                length: None,
                thickness: None,
                arrow: None,
                color: None,
                size: None,
                max_width: None,
                label: None,
            }),
            Err(CommandError::NoSuchLabel { .. })
        ));
        assert_eq!(ctx.tree.len(), before, "a refused callout creates nothing");
    }

    // ---- the explain.equation template ------------------------------------

    fn explain_equation(
        formula: &str,
        position: Option<[f64; 2]>,
        scale: Option<f64>,
        caption: Option<&str>,
        label: Option<&str>,
    ) -> Command {
        Command::ExplainEquation {
            formula: formula.to_owned(),
            position,
            scale,
            color: Some([100, 181, 246]),
            caption: caption.map(str::to_owned),
            caption_size: None,
            label: label.map(str::to_owned),
        }
    }

    /// One group, two addressable pieces, and a caption measured off the
    /// formula's own ink rather than placed at a fixed offset.
    ///
    /// The fake reports a 2×2 formula box and a 4×1 caption, so the numbers are
    /// exact: with the group at `(0.5, 1)` and `scale = 1.2` the formula's world
    /// bottom is `1 - 1.2 = -0.2`, and the caption's centre sits one default gap
    /// plus half its height below that — `-2.05` in the group's frame. The group
    /// declares the union of the two boxes, which is what makes `next_to` and
    /// `camera focus` work on a template's result.
    #[test]
    fn an_equation_groups_a_formula_and_its_caption() {
        let mut ctx = with_typesetter(FakeTypesetter::both());
        let before = ctx.tree.len();
        let diff = applied(
            &mut ctx,
            &explain_equation(
                "a^2 + b^2 = c^2",
                Some([0.5, 1.0]),
                Some(1.2),
                Some("勾股定理"),
                Some("定理"),
            ),
        );

        assert_eq!(diff.created.len(), 3, "the group, the formula, the caption");
        assert_eq!(
            ctx.tree.len(),
            before + 4,
            "the formula's own glyph is in the tree but not in the diff"
        );
        let (group, formula, caption) = (diff.created[0], diff.created[1], diff.created[2]);

        let g = ctx.tree.get(group).unwrap();
        assert_eq!(g.kind, NodeKind::Group);
        assert_eq!(
            g.label.as_deref(),
            Some("定理"),
            "the caller's label names the group"
        );
        assert_eq!(ctx.tree.get(formula).unwrap().kind, NodeKind::Formula);
        assert_eq!(
            ctx.tree.get(formula).unwrap().label.as_deref(),
            Some("公式"),
            "the formula keeps its own name under the group's"
        );
        assert_eq!(ctx.tree.get(caption).unwrap().kind, NodeKind::Text);

        // The formula sits at the group's origin and carries the post-typeset
        // scale; an undeclared scale would land the box at ±1 instead of ±1.2.
        let ink = ctx.tree.world_bounds(formula).unwrap();
        assert!((ink.x0 + 0.7).abs() < 1e-12, "{ink:?}");
        assert!((ink.x1 - 1.7).abs() < 1e-12, "{ink:?}");
        assert!((ink.y0 + 0.2).abs() < 1e-12, "{ink:?}");

        // Measured from that bottom: one gap (0.35) and half the 1-tall caption.
        let text = ctx.tree.world_bounds(caption).unwrap();
        assert!((text.center().x - 0.5).abs() < 1e-12, "{text:?}");
        assert!((text.center().y + 1.05).abs() < 1e-12, "{text:?}");

        // The colour reaches every glyph of the formula, because opacity and
        // paint do not inherit in this draw walk.
        let paint = rustim_color::Srgb::from_rgba8(100, 181, 246, 255);
        let glyph = ctx
            .tree
            .draw_order()
            .into_iter()
            .find(|id| !diff.created.contains(id))
            .expect("the fake inserted one glyph under the formula");
        assert_eq!(ctx.tree.get(glyph).unwrap().style.fill, paint);
        assert_eq!(ctx.tree.get(caption).unwrap().style.fill, paint);

        // The declared box is the union in the group's frame, and it is what
        // `world_bounds` answers with — there are no points to measure.
        let declared = ctx.tree.get(group).unwrap().local_bounds.unwrap();
        assert!((declared.x0 + 2.0).abs() < 1e-12, "{declared:?}");
        assert!((declared.x1 - 2.0).abs() < 1e-12, "{declared:?}");
        assert!((declared.y0 + 2.55).abs() < 1e-12, "{declared:?}");
        assert!((declared.y1 - 1.2).abs() < 1e-12, "{declared:?}");
        let world = ctx.tree.world_bounds(group).unwrap();
        assert!((world.x0 + 1.5).abs() < 1e-12, "{world:?}");
        assert!((world.y1 - 2.2).abs() < 1e-12, "{world:?}");
    }

    /// Every refusal is atomic, and each names the field that caused it. The
    /// capability refusals come first so a client with no typesetter learns the
    /// build cannot do it before it spends a round trip on its parameters.
    #[test]
    fn an_equation_refuses_before_creating_anything() {
        let mut bare = scene();
        let before = bare.tree.len();
        assert!(
            matches!(
                bare.handle(&explain_equation("x^2", None, None, None, None)),
                Err(CommandError::Unsupported { ref feature }) if feature == "formula"
            ),
            "a build with no typesetter cannot compile a formula"
        );
        assert_eq!(bare.tree.len(), before);

        for (cmd, field) in [
            (
                explain_equation("x^2", None, Some(0.0), None, None),
                "scale",
            ),
            (
                explain_equation("x^2", None, Some(f64::NAN), None, None),
                "scale",
            ),
            (
                explain_equation("x^2", None, None, Some("   "), None),
                "caption",
            ),
            (
                explain_equation("x^2", Some([f64::INFINITY, 0.0]), None, None, None),
                "position",
            ),
        ] {
            let mut ctx = with_typesetter(FakeTypesetter::both());
            let before = ctx.tree.len();
            assert!(
                matches!(
                    ctx.handle(&cmd),
                    Err(CommandError::OutOfRange { ref property, .. }) if property == field
                ),
                "{cmd:?} refused as the wrong field"
            );
            assert_eq!(ctx.tree.len(), before, "{cmd:?} left nodes behind");
        }

        // A caption on a build that typesets formulas but not text.
        let mut ctx = with_typesetter(FakeTypesetter {
            text: false,
            ..FakeTypesetter::both()
        });
        let before = ctx.tree.len();
        assert!(matches!(
            ctx.handle(&explain_equation("x^2", None, None, Some("注解"), None)),
            Err(CommandError::Unsupported { ref feature }) if feature == "text"
        ));
        assert_eq!(ctx.tree.len(), before);
    }

    /// A typesetter that accepts the formula and then fails on the caption —
    /// the one window in which the group already holds a compiled formula. The
    /// rollback is the claim: a refused template leaves no half an equation.
    struct CaptionFails(FakeTypesetter);

    impl Typesetter for CaptionFails {
        fn supports_text(&self) -> bool {
            true
        }

        fn supports_formula(&self) -> bool {
            true
        }

        fn text(&mut self, _text: &str, _spec: &TextSpec) -> Result<TextBlock, TypesetError> {
            Err(TypesetError::invalid("no glyphs in this font"))
        }

        fn formula(
            &mut self,
            source: &str,
            tree: &mut SceneTree,
        ) -> Result<TypesetFormula, TypesetError> {
            self.0.formula(source, tree)
        }
    }

    #[test]
    fn a_late_refusal_rolls_the_group_back() {
        let mut ctx = scene();
        ctx.set_typesetter(Box::new(CaptionFails(FakeTypesetter::both())));
        let before = ctx.tree.len();
        let err = ctx
            .handle(&explain_equation("x^2", None, None, Some("注解"), None))
            .expect_err("the caption cannot be shaped");
        assert!(
            matches!(&err, CommandError::OutOfRange { property, .. } if property == "caption"),
            "{err}"
        );
        assert_eq!(
            ctx.tree.len(),
            before,
            "the group and the compiled formula went with the refusal"
        );
    }

    // ---- explain.plot -----------------------------------------------------

    /// A plot command with only the caller's own choices filled: every other
    /// slot is the template's default.
    fn explain_plot(
        coefficients: &[f64],
        position: Option<[f64; 2]>,
        caption: Option<&str>,
        label: Option<&str>,
    ) -> Command {
        Command::ExplainPlot {
            coefficients: coefficients.to_vec(),
            x_range: None,
            y_range: None,
            width: None,
            height: None,
            samples: None,
            step: None,
            color: Some([255, 213, 79]),
            position,
            caption: caption.map(str::to_owned),
            caption_size: None,
            label: label.map(str::to_owned),
        }
    }

    /// The centres of the created text pieces carrying `label`, in draw order.
    fn centres(ctx: &SceneContext, diff: &StateDiff, label: &str) -> Vec<rustim_math::Point> {
        diff.created
            .iter()
            .filter_map(|id| {
                let m = ctx.tree.get(*id)?;
                (m.kind == NodeKind::Text && m.label.as_deref() == Some(label))
                    .then(|| ctx.tree.world_bounds(*id).map(|rect| rect.center()))
                    .flatten()
            })
            .collect()
    }

    /// The whole teaching picture in one command: axes, a stroked curve cut at
    /// the frame's edge, a label under every tick and a caption below it all.
    ///
    /// Every number below is exact, because the data window and the geometric
    /// size are deliberately different numbers: `x_range [-4, 4]` across `width
    /// 7` puts the x tick labelled `4` at world `3.5`, and `y_range [-3, 3]`
    /// across `height 4` puts the y tick labelled `3` at world `2.0`. A template
    /// that confused data units with world units would land both somewhere else.
    #[test]
    fn a_plot_groups_axes_a_curve_and_labelled_ticks() {
        let mut ctx = with_typesetter(FakeTypesetter::both());
        let diff = applied(
            &mut ctx,
            &explain_plot(
                &[0.0, 0.0, 1.0],
                Some([0.5, 1.0]),
                Some("抛物线"),
                Some("图像"),
            ),
        );

        // Group, axes, one curve, eight x labels (-4..4 without 0), six y
        // labels (-3..3 without 0), and the caption.
        assert_eq!(diff.created.len(), 18, "{:?}", diff.created);
        let group = diff.created[0];
        assert_eq!(ctx.tree.get(group).unwrap().kind, NodeKind::Group);
        assert_eq!(ctx.tree.get(group).unwrap().label.as_deref(), Some("图像"));
        assert_eq!(ctx.tree.get(diff.created[1]).unwrap().kind, NodeKind::Axes);
        let curve = diff.created[2];
        assert_eq!(ctx.tree.get(curve).unwrap().kind, NodeKind::Plot);

        // x² lives inside y ∈ [-3, 3] only where |x| ≤ √3, so the one run is
        // cut on the top edge: its world y is 2 there, plus the group's 1.
        let bounds = ctx.tree.world_bounds(curve).unwrap();
        assert!((bounds.y0 - 1.0).abs() < 1e-12, "{bounds:?}");
        assert!((bounds.y1 - 3.0).abs() < 1e-12, "{bounds:?}");
        assert!((bounds.x1 - 2.0154).abs() < 1e-3, "{bounds:?}");
        assert!(
            (bounds.x0 + 1.0154).abs() < 1e-3,
            "the run starts left of the group"
        );

        // The curve is a stroke, not a fill: a filled polyline would paint the
        // area between the parabola and its chord.
        let style = &ctx.tree.get(curve).unwrap().style;
        assert_eq!(
            style.fill,
            rustim_color::Srgb::from_rgba8(255, 213, 79, 255)
        );
        assert_eq!(style.fill_opacity, 0.0);
        assert_eq!(style.stroke_opacity, 1.0);

        // A tick's label sits under the coordinate it names, not under the
        // number's own value: data 4 is 3.5 world units right of the centre.
        let four = centres(&ctx, &diff, "4");
        assert_eq!(four.len(), 1, "4 is an x tick only");
        assert!((four[0].x - 4.0).abs() < 1e-12, "{:?}", four[0]);
        assert!((four[0].y - 0.62).abs() < 1e-12, "{:?}", four[0]);
        let threes = centres(&ctx, &diff, "3");
        assert_eq!(threes.len(), 2, "3 labels both axes");
        assert!(
            threes
                .iter()
                .any(|p| (p.x - 3.125).abs() < 1e-12 && (p.y - 0.62).abs() < 1e-12),
            "{threes:?}"
        );
        assert!(
            threes
                .iter()
                .any(|p| (p.x - 0.08).abs() < 1e-12 && (p.y - 3.0).abs() < 1e-12),
            "{threes:?}"
        );

        // The caption is measured off the lowest piece — here the "-3" y label,
        // whose line box reaches local -2.5, below the axes' own bottom edge at
        // -2 — one gap and half a line further down.
        let caption = *diff.created.last().unwrap();
        assert_eq!(ctx.tree.get(caption).unwrap().kind, NodeKind::Text);
        let text = ctx.tree.world_bounds(caption).unwrap();
        assert!((text.center().x - 0.5).abs() < 1e-12, "{text:?}");
        assert!((text.center().y + 2.35).abs() < 1e-12, "{text:?}");

        // The declared box is the union of the pieces in the group's frame —
        // axes, curve, every label (the "-4" is 2 units wide, so it reaches
        // x = -4.5) and the caption's bottom.
        let declared = ctx.tree.get(group).unwrap().local_bounds.unwrap();
        assert!((declared.x0 + 4.5).abs() < 1e-12, "{declared:?}");
        assert!((declared.x1 - 4.0).abs() < 1e-12, "{declared:?}");
        assert!((declared.y0 + 3.85).abs() < 1e-12, "{declared:?}");
        assert!((declared.y1 - 2.5).abs() < 1e-12, "{declared:?}");
    }

    /// A curve that leaves `y_range` and comes back is **two** pieces: a flat
    /// point array cannot express a break, and one polyline would draw a chord
    /// the function never had.
    #[test]
    fn a_curve_that_leaves_the_band_becomes_two_plot_pieces() {
        let cmd: Command = serde_json::from_str(
            r#"{"action":"explain.plot","coefficients":[0.0,0.0,1.0],"y_range":[0.5,3.0],"label":"双段"}"#,
        )
        .expect("a wire plot");
        let mut ctx = with_typesetter(FakeTypesetter::both());
        let diff = applied(&mut ctx, &cmd);

        let plots: Vec<NodeId> = diff
            .created
            .iter()
            .copied()
            .filter(|id| ctx.tree.get(*id).is_some_and(|m| m.kind == NodeKind::Plot))
            .collect();
        assert_eq!(plots.len(), 2, "the well under 0.5 is a gap, not a chord");
        let left = ctx.tree.world_bounds(plots[0]).unwrap();
        let right = ctx.tree.world_bounds(plots[1]).unwrap();
        assert!(left.x1 < 0.0, "{left:?}");
        assert!(right.x0 > 0.0, "{right:?}");
        // Both runs reach the bottom edge of the band (data 0.5 maps to -2.0)
        // and the top edge (data 3 maps to 2.0).
        for run in [left, right] {
            assert!((run.y0 + 2.0).abs() < 1e-12, "{run:?}");
            assert!((run.y1 - 2.0).abs() < 1e-12, "{run:?}");
        }
    }

    /// Every refusal names its own field and leaves the tree exactly as it
    /// found it — including the late one, where the axes were already in the
    /// group when the (empty) set of curve runs was discovered.
    #[test]
    fn a_plot_refuses_before_creating_anything() {
        let mut bare = scene();
        let before = bare.tree.len();
        assert!(
            matches!(
                bare.handle(&explain_plot(&[1.0], None, None, None)),
                Err(CommandError::Unsupported { ref feature }) if feature == "text"
            ),
            "the tick labels are mandatory, so a build without a shaper cannot plot"
        );
        assert_eq!(bare.tree.len(), before);

        let mut ctx = with_typesetter(FakeTypesetter {
            text: false,
            ..FakeTypesetter::both()
        });
        assert!(matches!(
            ctx.handle(&explain_plot(&[1.0], None, None, None)),
            Err(CommandError::Unsupported { ref feature }) if feature == "text"
        ));

        for (json, field) in [
            (
                r#"{"action":"explain.plot","coefficients":[]}"#,
                "coefficients",
            ),
            (
                r#"{"action":"explain.plot","coefficients":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}"#,
                "coefficients",
            ),
            (
                r#"{"action":"explain.plot","coefficients":[1.0],"x_range":[4.0,-4.0]}"#,
                "x_range",
            ),
            (
                r#"{"action":"explain.plot","coefficients":[1.0],"y_range":[1.0,1.0]}"#,
                "y_range",
            ),
            (
                r#"{"action":"explain.plot","coefficients":[1.0],"width":0.0}"#,
                "width",
            ),
            (
                r#"{"action":"explain.plot","coefficients":[1.0],"height":-1.0}"#,
                "height",
            ),
            (
                r#"{"action":"explain.plot","coefficients":[1.0],"samples":1}"#,
                "samples",
            ),
            (
                r#"{"action":"explain.plot","coefficients":[1.0],"samples":4096}"#,
                "samples",
            ),
            (
                r#"{"action":"explain.plot","coefficients":[1.0],"step":0.0}"#,
                "step",
            ),
            (
                r#"{"action":"explain.plot","coefficients":[1.0],"step":0.01}"#,
                "step",
            ),
            (
                r#"{"action":"explain.plot","coefficients":[1.0],"caption":"   "}"#,
                "caption",
            ),
            (
                r#"{"action":"explain.plot","coefficients":[1.0],"caption_size":0.0}"#,
                "caption_size",
            ),
            (
                r#"{"action":"explain.plot","coefficients":[100.0]}"#,
                "y_range",
            ),
        ] {
            let cmd: Command = serde_json::from_str(json).expect("a wire plot");
            let mut ctx = with_typesetter(FakeTypesetter::both());
            let before = ctx.tree.len();
            assert!(
                matches!(
                    ctx.handle(&cmd),
                    Err(CommandError::OutOfRange { ref property, .. }) if property == field
                ),
                "{json} refused as the wrong field"
            );
            assert_eq!(ctx.tree.len(), before, "{json} left nodes behind");
        }

        // Non-finite values have no JSON spelling, so these are built directly.
        let nan = Command::ExplainPlot {
            coefficients: vec![f64::NAN],
            x_range: None,
            y_range: None,
            width: None,
            height: None,
            samples: None,
            step: None,
            color: None,
            position: None,
            caption: None,
            caption_size: None,
            label: None,
        };
        let mut ctx = with_typesetter(FakeTypesetter::both());
        assert!(matches!(
            ctx.handle(&nan),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "coefficients"
        ));
        let mut ctx = with_typesetter(FakeTypesetter::both());
        assert!(matches!(
            ctx.handle(&explain_plot(&[1.0], Some([f64::INFINITY, 0.0]), None, None)),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "position"
        ));
    }

    // ---- the camera -------------------------------------------------------

    fn camera_cmd(center: Option<[f64; 2]>, zoom: Option<f64>, duration: Option<f64>) -> Command {
        Command::Camera {
            center,
            zoom,
            focus: None,
            duration,
        }
    }

    /// Without a duration the camera is written at once: the very next frame is
    /// already the new view, and the playhead never moves.
    #[test]
    fn a_camera_command_without_a_duration_sets_the_camera_now() {
        let mut ctx = scene();
        let diff = applied(&mut ctx, &camera_cmd(Some([2.0, -1.0]), Some(2.0), None));
        assert!(
            diff.is_empty(),
            "the view is not a node, so nothing changed"
        );
        assert!(!ctx.is_playing(), "an instant set queues no beat");
        assert_eq!(ctx.camera(), &Camera::new((2.0, -1.0), 2.0));

        // ...and the frame's transform follows in the same breath. Not a
        // `pixels_per_unit()` assertion alone: the question a scene cares about is
        // where a world point lands on screen.
        let mut list = DrawList::default();
        ctx.build_draw_list(&mut list);
        let DrawOp::SetTransform(t) = list.ops[0] else {
            panic!("the first op must install the view");
        };
        let p = t * rustim_math::Point::new(2.0, -1.0);
        assert!(
            (p.x - 640.0).abs() < 1e-9 && (p.y - 360.0).abs() < 1e-9,
            "{p:?}"
        );
        assert_eq!(ctx.pixels_per_unit(), 180.0);
    }

    /// With a duration it is an ordinary beat: nothing happens on the frame it
    /// arrives, the camera interpolates, and the settled state is the destination.
    #[test]
    fn a_camera_command_with_a_duration_queues_one_beat() {
        let mut ctx = scene();
        let diff = applied(&mut ctx, &camera_cmd(Some([2.0, 0.0]), None, Some(0.5)));
        assert!(diff.is_empty());
        assert!(ctx.is_playing(), "the beat is queued");
        assert_eq!(
            ctx.camera(),
            &Camera::default(),
            "queueing must not teleport the view"
        );

        tick(&mut ctx, 3);
        let mid = ctx.camera().center.0;
        assert!(mid > 0.0 && mid < 2.0, "underway at {mid}");

        tick(&mut ctx, 40);
        assert!(!ctx.is_playing());
        assert_eq!(ctx.camera(), &Camera::new((2.0, 0.0), 1.0));
    }

    /// **An instant command does not pre-empt a running move.** That is the same
    /// contract `Set position` has with a running `MoveTo` — an animation owns its
    /// subject until it ends — and it is documented rather than discovered. The
    /// instant write is real for the moment it happens; the next frame of the move
    /// overwrites it.
    #[test]
    fn an_instant_camera_command_is_overwritten_by_a_running_move() {
        let mut ctx = scene();
        ctx.handle(&camera_cmd(Some([2.0, 0.0]), None, Some(1.0)))
            .unwrap();
        tick(&mut ctx, 10);

        ctx.handle(&camera_cmd(None, Some(4.0), None)).unwrap();
        assert_eq!(ctx.camera().zoom, 4.0, "the write itself lands");

        tick(&mut ctx, 1);
        assert_ne!(
            ctx.camera().zoom,
            4.0,
            "the running move owns the camera until it ends"
        );
        assert!(ctx.is_playing());

        tick(&mut ctx, 70);
        assert_eq!(ctx.camera(), &Camera::new((2.0, 0.0), 1.0));
    }

    /// A rewritten scene starts looking at the origin again.
    #[test]
    fn reset_returns_the_default_camera() {
        let mut ctx = scene();
        ctx.handle(&camera_cmd(Some([1.0, 1.0]), Some(3.0), None))
            .unwrap();
        assert_ne!(ctx.camera(), &Camera::default());

        ctx.reset();
        assert_eq!(ctx.camera(), &Camera::default());
        assert_eq!(ctx.pixels_per_unit(), 90.0);
    }

    fn narrate(
        ctx: &mut SceneContext,
        text: &str,
        at: Option<f64>,
    ) -> Result<StateDiff, CommandError> {
        ctx.handle(&Command::Narrate {
            text: text.to_owned(),
            at,
        })
        .map(|outcome| match outcome {
            CommandOutcome::Applied(diff) => diff,
            CommandOutcome::Queried(_) => panic!("narration is not a query"),
            CommandOutcome::Greeted(_) => panic!("narration is not a greeting"),
        })
    }

    #[test]
    fn a_narration_without_a_time_is_due_immediately() {
        let mut ctx = scene();
        let diff = narrate(&mut ctx, "先看这个三角形", None).unwrap();
        assert!(
            diff.is_empty(),
            "speaking is not a tree change, and the reply has to say so"
        );
        assert_eq!(ctx.take_narration(), ["先看这个三角形"]);
        assert!(
            ctx.take_narration().is_empty(),
            "draining means a line is said once"
        );
    }

    #[test]
    fn a_delayed_narration_waits_for_the_virtual_clock() {
        let mut ctx = scene();
        narrate(&mut ctx, "两条直角边是 3 和 4", Some(2.0)).unwrap();

        tick(&mut ctx, 114); // 1.9 s
        assert!(ctx.take_narration().is_empty(), "not due yet");

        tick(&mut ctx, 12); // 2.1 s
        assert_eq!(ctx.take_narration(), ["两条直角边是 3 和 4"]);
    }

    /// `at` is measured from **arrival**, not from the start of the run — the
    /// property a file feed (all lines applied at t = 0) depends on.
    #[test]
    fn a_delay_is_relative_to_when_the_command_arrived() {
        let mut ctx = scene();
        tick(&mut ctx, 180); // 3 s in
        narrate(&mut ctx, "现在看斜边", Some(1.0)).unwrap();

        tick(&mut ctx, 54); // 3.9 s
        assert!(ctx.take_narration().is_empty());

        tick(&mut ctx, 12); // 4.1 s
        assert_eq!(ctx.take_narration(), ["现在看斜边"]);
    }

    #[test]
    fn due_lines_come_out_oldest_first() {
        let mut ctx = scene();
        narrate(&mut ctx, "third", Some(2.0)).unwrap();
        narrate(&mut ctx, "first", None).unwrap();
        narrate(&mut ctx, "second", Some(1.0)).unwrap();
        tick(&mut ctx, 180);

        assert_eq!(ctx.take_narration(), ["first", "second", "third"]);
        assert!(ctx.take_narration().is_empty());
    }

    /// The timed drain is the same drain: the words and their order agree, and
    /// each start is the instant the line became due — what a subtitle cue needs.
    #[test]
    fn the_timed_drain_keeps_the_due_time() {
        let mut ctx = scene();
        narrate(&mut ctx, "third", Some(2.0)).unwrap();
        narrate(&mut ctx, "first", None).unwrap();
        narrate(&mut ctx, "second", Some(1.0)).unwrap();
        tick(&mut ctx, 180);

        let due = ctx.take_narration_timed();
        let words: Vec<&str> = due.iter().map(|(_, text)| text.as_str()).collect();
        assert_eq!(words, ["first", "second", "third"]);
        let times: Vec<f64> = due.iter().map(|(at, _)| *at).collect();
        assert_eq!(times, [0.0, 1.0, 2.0]);
        assert!(ctx.take_narration_timed().is_empty());
    }

    #[test]
    fn an_impossible_narration_is_refused_by_field() {
        let mut ctx = scene();
        for text in ["", "   ", "\n\t"] {
            let err = narrate(&mut ctx, text, None).unwrap_err();
            assert!(
                matches!(&err, CommandError::OutOfRange { property, .. } if property == "text"),
                "{text:?} refused as {err}"
            );
        }
        for at in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = narrate(&mut ctx, "说点什么", Some(at)).unwrap_err();
            assert!(
                matches!(&err, CommandError::OutOfRange { property, .. } if property == "at"),
                "{at} refused as {err}"
            );
        }
        assert!(ctx.take_narration().is_empty(), "a refusal says nothing");
    }

    /// The pure `apply` still answers the command, as a no-op, so a caller that
    /// replays a log through it does not panic and does not lose the rest.
    #[test]
    fn apply_treats_narration_as_a_harmless_no_op() {
        let mut ctx = scene();
        let diff = command::apply(
            &mut ctx.tree,
            &Command::Narrate {
                text: "hello".to_owned(),
                at: Some(1.0),
            },
        )
        .unwrap();
        assert!(diff.is_empty());
        assert!(ctx.take_narration().is_empty());
    }

    #[test]
    fn reset_clears_the_narration_track_and_its_clock() {
        let mut ctx = scene();
        narrate(&mut ctx, "old scene", Some(2.0)).unwrap();
        tick(&mut ctx, 30);

        ctx.reset();
        tick(&mut ctx, 180);
        assert!(
            ctx.take_narration().is_empty(),
            "a rewritten scene does not speak the old one's lines"
        );

        // And the clock started over, so `at` is measured from the reset: one
        // second from now, not from the run's beginning.
        narrate(&mut ctx, "new scene", Some(1.0)).unwrap();
        tick(&mut ctx, 120);
        assert_eq!(ctx.take_narration(), ["new scene"]);
    }

    #[test]
    fn the_clock_rewinds_but_never_below_zero() {
        let mut ctx = scene();
        tick(&mut ctx, 30);
        ctx.advance(-0.25);
        assert!((ctx.clock - 0.25).abs() < 1e-12, "{}", ctx.clock);

        ctx.advance(-10.0);
        assert_eq!(ctx.clock, 0.0);
    }

    // ---- the typesetter seam ----------------------------------------------

    /// A provider with no shaper behind it: it returns the smallest block that
    /// has a glyph, which is all the draw walk needs to see a `GlyphRun`, and a
    /// formula of one root and one square child. The point of the fake is that
    /// the kernel never learns what a font is; this test module need not either.
    #[derive(Clone, Default)]
    pub(super) struct FakeTypesetter {
        text: bool,
        formula: bool,
        refuse: Option<TypesetError>,
    }

    impl FakeTypesetter {
        pub(super) fn both() -> Self {
            Self {
                text: true,
                formula: true,
                refuse: None,
            }
        }
    }

    impl Typesetter for FakeTypesetter {
        fn supports_text(&self) -> bool {
            self.text
        }

        fn supports_formula(&self) -> bool {
            self.formula
        }

        fn text(&mut self, text: &str, spec: &TextSpec) -> Result<TextBlock, TypesetError> {
            if let Some(error) = self.refuse.clone() {
                return Err(error);
            }
            Ok(TextBlock::new(
                text.chars().count() as f32,
                1.0,
                vec![TextRun {
                    font: FontData::new(Vec::<u8>::new().into(), 0),
                    size: spec.size.map_or(1.0, |size| size as f32),
                    glyphs: vec![TextGlyph {
                        id: 3,
                        x: 0.5,
                        y: 0.5,
                    }],
                }],
            ))
        }

        fn formula(
            &mut self,
            _source: &str,
            tree: &mut SceneTree,
        ) -> Result<TypesetFormula, TypesetError> {
            if let Some(error) = self.refuse.clone() {
                return Err(error);
            }
            // The real compiler declares its ink box (a formula's root has no
            // points); the fake does the same so template placement can be
            // measured in a test. Its one glyph is a default 2x2 square, so the
            // box is 2x2 centred on the origin.
            let root = tree.insert(
                Mobject::new(Vec::new())
                    .with_kind(NodeKind::Formula)
                    .with_local_bounds(rustim_math::Rect::new(-1.0, -1.0, 1.0, 1.0)),
            );
            let glyph = tree.insert(rustim_mobject::shapes::build(
                ShapeKind::Square,
                &ShapeParams::default(),
            ));
            // Unconditional, result checked separately — see `Formula::insert`.
            let moved = tree.reparent(glyph, root);
            debug_assert!(moved);
            Ok(TypesetFormula {
                root,
                nodes: vec![glyph],
            })
        }
    }

    pub(super) fn with_typesetter(typesetter: FakeTypesetter) -> SceneContext {
        let mut ctx = scene();
        ctx.set_typesetter(Box::new(typesetter));
        ctx
    }

    pub(super) fn add_text() -> Command {
        serde_json::from_str(
            r#"{"action":"add","type":"text","params":{"text":"你好","label":"字","position":[1,2],"size":0.5}}"#,
        )
        .expect("a text add")
    }

    fn add_formula() -> Command {
        serde_json::from_str(
            r#"{"action":"add","type":"formula","params":{"text":"x^2","label":"公式","position":[1,2],"scale":2.0,"color":[100,181,246]}}"#,
        )
        .expect("a formula add")
    }

    /// **The honest default is "we cannot".** A context with no provider refuses
    /// both kinds with the feature named, and — the half a client actually
    /// reads — does not advertise them in the handshake either.
    #[test]
    fn text_and_formula_are_refused_without_a_typesetter() {
        let mut ctx = scene();
        for (cmd, feature) in [(add_text(), "text"), (add_formula(), "formula")] {
            assert_eq!(
                ctx.handle(&cmd).unwrap_err(),
                CommandError::Unsupported {
                    feature: feature.to_owned()
                }
            );
        }
        assert!(
            ctx.tree.is_empty(),
            "a refusal must not leave a node behind"
        );

        let caps = ctx.capabilities();
        for absent in ["text", "formula"] {
            assert!(!caps.shapes.iter().any(|shape| shape == absent));
        }
    }

    /// A provider that claims one half advertises one half, and the other
    /// refusal still names the feature that is missing rather than the one that
    /// is present.
    #[test]
    fn half_a_typesetter_advertises_and_refuses_exactly_that_half() {
        let mut ctx = with_typesetter(FakeTypesetter {
            text: true,
            ..FakeTypesetter::default()
        });
        let caps = ctx.capabilities();
        assert!(caps.shapes.iter().any(|shape| shape == "text"));
        assert!(!caps.shapes.iter().any(|shape| shape == "formula"));
        assert_eq!(
            ctx.handle(&add_formula()).unwrap_err(),
            CommandError::Unsupported {
                feature: "formula".to_owned()
            }
        );
        assert!(ctx.handle(&add_text()).is_ok());
    }

    /// **A wire-created text object is drawn by the kernel's own walk.** The
    /// block rides the mobject, the declared bounds give it a centre, and the
    /// frame transform is put back after the run — which is the sharp edge of a
    /// builder that places its subject through the list's global transform.
    #[test]
    fn a_typeset_text_node_is_drawn_and_the_frame_transform_comes_back() {
        let mut ctx = with_typesetter(FakeTypesetter::both());
        let diff = applied(&mut ctx, &add_text());
        let id = diff.created[0];

        let m = ctx.tree.get(id).unwrap();
        assert!(m.text.is_some(), "the shaped block is the object's picture");
        assert!(
            m.local_bounds.is_some(),
            "a text node declares its box, or it has no bounds at all"
        );
        assert_eq!(m.label.as_deref(), Some("字"));
        assert_eq!(
            m.transform,
            Affine2::translate((1.0, 2.0)),
            "`position` places the block like any other kind"
        );

        let mut list = DrawList::default();
        ctx.build_draw_list(&mut list);
        let runs = list
            .ops
            .iter()
            .filter(|op| matches!(op, DrawOp::GlyphRun { .. }))
            .count();
        assert_eq!(runs, 1, "one run of one glyph, from the fake");
        let Some(DrawOp::SetTransform(last)) = list.ops.last() else {
            panic!("the walk has to leave the frame transform installed");
        };
        assert_eq!(*last, ctx.affine());

        // And the block is placed, not merely drawn: the transform installed
        // *before* the run is the block's placement, and the block's own centre
        // (its layout centre, where `layout_to_local` puts the local origin)
        // lands at the point `position` named.
        let run = list
            .ops
            .iter()
            .position(|op| matches!(op, DrawOp::GlyphRun { .. }))
            .expect("a glyph run");
        let place = list.ops[..run]
            .iter()
            .rev()
            .find_map(|op| match op {
                DrawOp::SetTransform(t) => Some(*t),
                _ => None,
            })
            .expect("the run is placed by a transform");
        let block = ctx.tree.get(id).unwrap().text.clone().expect("the block");
        let (x, y) = ctx.viewport.to_screen(ctx.camera(), (1.0, 2.0));
        let centre = place
            * rustim_math::Point::new(f64::from(block.width / 2.0), f64::from(block.height / 2.0));
        assert!((centre.x - x).abs() < 1e-9 && (centre.y - y).abs() < 1e-9);
    }

    /// A formula comes back as **one** object: the root. Its glyphs are its
    /// picture, coloured from the command, and the root carries the label, the
    /// position and the scale — the four things a caller can name.
    #[test]
    fn a_typeset_formula_is_one_root_with_coloured_children() {
        let mut ctx = with_typesetter(FakeTypesetter::both());
        let diff = applied(&mut ctx, &add_formula());
        assert_eq!(diff.created.len(), 1, "the root is the object");
        let root = diff.created[0];

        let m = ctx.tree.get(root).unwrap();
        assert_eq!(m.kind, NodeKind::Formula);
        assert_eq!(m.label.as_deref(), Some("公式"));
        assert_eq!(
            m.transform,
            Affine2::translate((1.0, 2.0)) * Affine2::scale(2.0),
            "scale first, then place — `shapes::build`'s order"
        );

        let children = ctx.tree.child_ids(root);
        assert_eq!(children.len(), 1);
        let glyph = ctx.tree.get(children[0]).unwrap();
        let want = rustim_color::Srgb::from_rgba8(100, 181, 246, 255);
        assert_eq!(
            glyph.style.fill, want,
            "the command's colour reaches every glyph"
        );
        assert_eq!(glyph.style.fill_opacity, 1.0);
        assert_eq!(glyph.style.stroke_opacity, 0.0);
    }

    /// The two refusal kinds are the two different fixes, and the *field* named
    /// is the one the command carried — `text` — for both kinds of source.
    #[test]
    fn a_typesetter_refusal_keeps_its_kind() {
        let mut ctx = with_typesetter(FakeTypesetter {
            text: true,
            formula: true,
            refuse: Some(TypesetError::invalid("unbalanced brace")),
        });
        assert_eq!(
            ctx.handle(&add_formula()).unwrap_err(),
            CommandError::OutOfRange {
                property: "text".to_owned(),
                detail: "unbalanced brace".to_owned()
            }
        );

        let mut ctx = with_typesetter(FakeTypesetter {
            text: true,
            formula: true,
            refuse: Some(TypesetError::failed("assets/fonts is missing")),
        });
        assert_eq!(
            ctx.handle(&add_text()).unwrap_err(),
            CommandError::Typeset {
                detail: "assets/fonts is missing".to_owned()
            }
        );
    }

    /// Children are a group's grammar, so a text object with children is a
    /// malformed payload — refused for that before the provider is asked, and
    /// without needing one to be installed.
    #[test]
    fn children_on_a_typeset_kind_name_the_children_field() {
        let mut ctx = scene();
        let cmd: Command = serde_json::from_str(
            r#"{"action":"add","type":"text","params":{"text":"x"},"children":[4294967297]}"#,
        )
        .unwrap();
        assert_eq!(
            ctx.handle(&cmd).unwrap_err(),
            CommandError::OutOfRange {
                property: "children".to_owned(),
                detail: "only a group takes children".to_owned()
            }
        );
    }
}

#[cfg(test)]
mod playback_control_tests {
    //! `pause` / `resume` / `seek` / `tempo`, the four commands that act on the
    //! *clock* rather than on the tree.
    //!
    //! None of them can be seen in a still frame, which is why every claim here
    //! is about the state a run passes through: the scheduler's delivery, the
    //! lecture clock, `is_playing`, and the narration track.

    use super::tests::{FakeTypesetter, add_text, with_typesetter};
    use super::*;
    use rustim_anim::{FadeIn, FadeOut};

    const DT: f64 = 1.0 / 60.0;

    fn scene() -> SceneContext {
        SceneContext::new(Viewport::default(), Motion::Full)
    }

    fn tick(ctx: &mut SceneContext, frames: usize) {
        for _ in 0..frames {
            ctx.advance(DT);
        }
    }

    fn invisible_square(ctx: &mut SceneContext) -> NodeId {
        let id = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        ctx.tree.get_mut(id).unwrap().opacity = 0.0;
        id
    }

    fn opacity(ctx: &SceneContext, id: NodeId) -> f64 {
        ctx.tree.get(id).unwrap().opacity
    }

    fn applied(ctx: &mut SceneContext, cmd: &Command) {
        ctx.handle(cmd).expect("a well-formed command");
    }

    /// `handle` keeping the diff. The controls above answer with empty ones and
    /// discard them; an undo is the command whose reply has to be read.
    fn applied_diff(ctx: &mut SceneContext, cmd: &Command) -> StateDiff {
        match ctx.handle(cmd).expect("a well-formed command") {
            CommandOutcome::Applied(diff) => diff,
            _ => panic!("a mutation is not a read"),
        }
    }

    /// A pause stops the beat and the clock together, and a resume continues
    /// from exactly where they stopped — not from where the pause began, and not
    /// from where the timeline would have been had nobody paused it.
    #[test]
    fn a_pause_freezes_both_clocks_and_the_beat() {
        let mut ctx = scene();
        let id = invisible_square(&mut ctx);
        ctx.play(FadeIn::new(id).with_duration(1.0));

        tick(&mut ctx, 15);
        let held = opacity(&ctx, id);
        let clock = ctx.clock();
        assert!(held > 0.0 && held < 1.0, "half a second into a 1 s fade");

        ctx.pause();
        tick(&mut ctx, 600);
        assert_eq!(opacity(&ctx, id), held, "a pause advanced the beat");
        assert_eq!(ctx.clock(), clock, "a pause advanced the lecture clock");

        ctx.resume();
        tick(&mut ctx, 1);
        assert!(opacity(&ctx, id) > held, "the resume did not restart time");
        tick(&mut ctx, 60);
        assert_eq!(opacity(&ctx, id), 1.0);
        assert!(!ctx.is_playing());
    }

    /// A beat accepted while paused waits for the resume rather than consuming a
    /// frozen frame — which is what makes "pause, add, play, resume" a lecture.
    #[test]
    fn a_play_accepted_during_a_pause_waits_for_the_resume() {
        let mut ctx = scene();
        let id = invisible_square(&mut ctx);
        ctx.pause();
        ctx.play(FadeIn::new(id).with_duration(0.5));

        tick(&mut ctx, 120);
        assert_eq!(opacity(&ctx, id), 0.0, "the beat ran while paused");
        assert!(ctx.is_playing(), "it is queued, and the queue is not empty");

        ctx.resume();
        tick(&mut ctx, 40);
        assert_eq!(opacity(&ctx, id), 1.0);
    }

    /// Pausing is not the end of the timeline: a scene that relabels when its
    /// morph settles must not relabel because a viewer held the picture.
    #[test]
    fn a_pause_is_not_the_end_of_the_timeline() {
        let mut ctx = scene();
        let id = invisible_square(&mut ctx);
        ctx.play(FadeIn::new(id).with_duration(1.0));
        tick(&mut ctx, 5);
        ctx.pause();
        assert!(ctx.is_playing());
    }

    /// `stop` and `pause` are orthogonal: a content command must not undo a
    /// viewer's hold. `resume` is the only way back.
    #[test]
    fn stop_does_not_lift_a_pause() {
        let mut ctx = scene();
        ctx.pause();
        ctx.stop();
        assert!(ctx.is_paused());
        ctx.resume();
        assert!(!ctx.is_paused());
    }

    /// A tempo scales the beat and the lecture clock by the same factor, so
    /// narration written against the clock stays on the beats.
    #[test]
    fn tempo_scales_the_timeline_and_the_lecture_clock() {
        let mut ctx = scene();
        let id = invisible_square(&mut ctx);
        ctx.play(FadeIn::new(id).with_duration(1.0));

        applied(&mut ctx, &Command::Tempo { factor: 2.0 });
        assert_eq!(ctx.tempo(), 2.0);
        tick(&mut ctx, 30);
        assert!(
            (ctx.clock() - 1.0).abs() < 1e-12,
            "30 frames at 2× are one lecture second, got {}",
            ctx.clock()
        );
        assert_eq!(opacity(&ctx, id), 1.0, "a 1 s beat at 2× is over in 0.5 s");
        // `ease` can round to exactly 1.0 one frame before the raw age reaches
        // the duration, so completion is checked on the next tick rather than on
        // the same one — the beat is over either way, and the extra frame is the
        // scheduler's own completion predicate, not the tempo's.
        tick(&mut ctx, 1);
        assert!(!ctx.is_playing());
    }

    /// The command refuses what a speed cannot mean, and names the field. The
    /// four refusals are distinct fixes: `0` is the pause command, a negative
    /// factor would run time backwards, and the two bounds are the playback
    /// range.
    #[test]
    fn a_tempo_refusal_names_the_field_and_the_range() {
        let mut ctx = scene();
        for factor in [0.0, -2.0, f64::NAN, f64::INFINITY] {
            let err = ctx
                .handle(&Command::Tempo { factor })
                .expect_err("not a speed");
            assert!(
                matches!(
                    err,
                    CommandError::OutOfRange {
                        ref property,
                        ..
                    } if property == "factor"
                ),
                "{factor} was refused as {err:?}"
            );
        }
        for factor in [0.01, 100.0] {
            let err = ctx
                .handle(&Command::Tempo { factor })
                .expect_err("outside the range");
            let CommandError::OutOfRange { property, detail } = err else {
                panic!("wrong refusal");
            };
            assert_eq!(property, "factor");
            assert!(detail.contains("0.05") && detail.contains("16"), "{detail}");
        }
        assert_eq!(ctx.tempo(), 1.0, "a refused tempo changed the speed");
    }

    /// A programmatic caller has no error channel, so the setter clamps and
    /// ignores impossible values rather than writing one into the clock's
    /// multiplier.
    #[test]
    fn set_tempo_clamps_for_a_programmatic_caller() {
        let mut ctx = scene();
        ctx.set_tempo(100.0);
        assert_eq!(ctx.tempo(), SceneContext::MAX_TEMPO);
        ctx.set_tempo(f64::NAN);
        assert_eq!(ctx.tempo(), SceneContext::MAX_TEMPO);
        ctx.set_tempo(-1.0);
        assert_eq!(ctx.tempo(), SceneContext::MAX_TEMPO);
        ctx.set_tempo(1.0);
        assert_eq!(ctx.tempo(), 1.0);
    }

    /// A fresh run plays: a scene rewritten while the previous one was held must
    /// not come up frozen, nor at a speed nobody asked for.
    #[test]
    fn reset_returns_to_full_speed_and_play() {
        let mut ctx = scene();
        ctx.pause();
        ctx.set_tempo(4.0);
        ctx.reset();
        assert!(!ctx.is_paused());
        assert_eq!(ctx.tempo(), 1.0);
    }

    /// A seek plays what it passes *for real*: a `FadeOut` in the skipped span
    /// removes its target, because `finish` runs — a jump is not a fast-forward
    /// of the queue, it is the run itself, coarser in wall time only.
    #[test]
    fn a_seek_finishes_the_beats_it_passes() {
        let mut ctx = scene();
        let id = invisible_square(&mut ctx);
        ctx.play(FadeIn::new(id).with_duration(0.5));
        ctx.play(FadeOut::new(id).with_duration(0.5));

        ctx.seek(5.0).expect("a forward seek");
        assert!(
            ctx.tree.get(id).is_none(),
            "the skipped FadeOut left its target behind"
        );
        assert!(!ctx.is_playing());
        assert_eq!(ctx.clock(), 5.0, "the clock lands exactly on the target");
    }

    /// The target is a time, not a grid point: the clock is assigned, because
    /// `a + (b - a)` is not `b` for every float pair.
    #[test]
    fn a_seek_lands_exactly_on_its_target() {
        let mut ctx = scene();
        ctx.seek(6.5).expect("a forward seek");
        assert_eq!(ctx.clock(), 6.5);
    }

    /// Seeking backwards needs a snapshot of everything an animation may have
    /// deleted or overwritten; that is the checkpoint/undo step, and this
    /// command says so instead of pretending.
    #[test]
    fn a_seek_is_forward_only() {
        let mut ctx = scene();
        ctx.seek(5.0).unwrap();
        let err = ctx.seek(4.0).expect_err("a rewind");
        let CommandError::OutOfRange { property, detail } = err else {
            panic!("wrong refusal");
        };
        assert_eq!(property, "to");
        assert!(detail.contains("forward only"), "{detail}");
        assert_eq!(ctx.clock(), 5.0, "a refused seek moved the clock");
    }

    /// Three impossible targets, one field named.
    #[test]
    fn a_seek_refuses_impossible_targets() {
        let mut ctx = scene();
        for to in [-1.0, f64::NAN, f64::INFINITY] {
            let err = ctx.seek(to).expect_err("not a time");
            let CommandError::OutOfRange { property, .. } = err else {
                panic!("wrong refusal for {to}");
            };
            assert_eq!(property, "to");
        }
        let err = ctx
            .seek(SceneContext::MAX_SEEK_SECONDS + 1.0)
            .expect_err("an unbounded fast-forward");
        let CommandError::OutOfRange { property, detail } = err else {
            panic!("wrong refusal");
        };
        assert_eq!(property, "to");
        assert!(detail.contains("600"), "{detail}");
        assert_eq!(ctx.clock(), 0.0, "a refused seek moved the clock");
    }

    /// A jump is a jump: a line the seek crossed is dropped rather than said
    /// eight-at-once on the next frame, while a line still ahead stays queued.
    #[test]
    fn a_seek_drops_the_narration_it_crosses_and_keeps_what_is_ahead() {
        let mut ctx = scene();
        applied(
            &mut ctx,
            &Command::Narrate {
                text: "早".to_owned(),
                at: Some(0.5),
            },
        );
        applied(
            &mut ctx,
            &Command::Narrate {
                text: "晚".to_owned(),
                at: Some(100.0),
            },
        );

        ctx.seek(1.0).unwrap();
        assert!(
            ctx.take_narration().is_empty(),
            "the crossed line was still queued"
        );

        // A jump that stops before the line must leave it queued — the drop is
        // "what the seek crossed", not "everything on the track".
        ctx.seek(99.5).unwrap();
        assert!(
            ctx.take_narration().is_empty(),
            "the seek said a line it did not reach"
        );
        ctx.advance(0.5);
        assert_eq!(
            ctx.take_narration(),
            vec!["晚".to_owned()],
            "the line ahead was lost by the earlier seeks"
        );
    }

    /// A seek is a scrub: it works on a held timeline and leaves it held, so a
    /// caller can look ahead without letting the lecture run on.
    #[test]
    fn a_seek_works_on_a_paused_timeline() {
        let mut ctx = scene();
        let id = invisible_square(&mut ctx);
        ctx.play(FadeIn::new(id).with_duration(0.5));
        ctx.pause();

        ctx.seek(2.0).expect("scrubbing a paused timeline");
        assert_eq!(opacity(&ctx, id), 1.0);
        assert!(ctx.is_paused(), "the seek resumed the paused timeline");
        let clock = ctx.clock();
        tick(&mut ctx, 10);
        assert_eq!(ctx.clock(), clock, "the tick ran while paused");
    }

    /// The four controls are reported as empty diffs through the one apply
    /// point, and they round-trip through JSON exactly as they arrived.
    #[test]
    fn the_playback_controls_answer_with_an_empty_diff() {
        let mut ctx = scene();
        for cmd in [
            Command::Pause,
            Command::Resume,
            Command::Seek { to: 1.0 },
            Command::Tempo { factor: 2.0 },
        ] {
            let json = serde_json::to_string(&cmd).unwrap();
            assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd);
            match ctx.handle(&cmd).expect("a well-formed control") {
                CommandOutcome::Applied(diff) => assert!(diff.is_empty(), "{cmd:?}"),
                _ => panic!("a control is not a read"),
            }
        }
    }

    fn square(ctx: &mut SceneContext) -> NodeId {
        ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ))
    }

    /// A mark changes nothing and says so; the undo that follows names exactly
    /// what moved — the object added since, and the one whose opacity was
    /// written.
    ///
    /// The diff is computed rather than remembered, which is why both halves are
    /// in one test: a remembered diff could be right about the removal and wrong
    /// about the property, and the tree would still look correct.
    #[test]
    fn undo_restores_the_tree_and_names_what_moved() {
        let mut ctx = scene();
        let kept = square(&mut ctx);

        assert_eq!(
            applied_diff(&mut ctx, &Command::Checkpoint),
            StateDiff::default(),
            "a mark is not a tree change"
        );
        let added = square(&mut ctx);
        applied_diff(
            &mut ctx,
            &Command::Set {
                target: kept.into(),
                property: command::Property::Opacity(0.25),
            },
        );

        let diff = applied_diff(&mut ctx, &Command::Undo);
        assert!(diff.created.is_empty(), "{diff:?}");
        assert_eq!(diff.removed, vec![added]);
        assert_eq!(diff.modified, vec![kept]);
        assert!(ctx.tree.get(added).is_none(), "the undo kept the addition");
        assert_eq!(opacity(&ctx, kept), 1.0, "the undo kept the write");
    }

    /// An object deleted after the mark comes back **under the id it had**.
    /// The restored tree is a clone, so the slot versions were cloned too and
    /// the ids are the ones the host already holds.
    #[test]
    fn undo_puts_a_removed_object_back_under_its_old_id() {
        let mut ctx = scene();
        let id = square(&mut ctx);
        applied_diff(&mut ctx, &Command::Checkpoint);
        applied_diff(&mut ctx, &Command::Remove { target: id.into() });
        assert!(ctx.tree.get(id).is_none());

        let diff = applied_diff(&mut ctx, &Command::Undo);
        assert_eq!(diff.created, vec![id]);
        assert!(diff.modified.is_empty(), "{diff:?}");
        assert!(diff.removed.is_empty(), "{diff:?}");
        assert_eq!(
            ctx.tree.get(id).map(|m| m.id),
            Some(id),
            "the restored node answers under the old id"
        );
    }

    /// The snapshot is the *session*, not just the tree: the clock, the pause,
    /// the tempo and the motion policy all come back.
    #[test]
    fn undo_restores_the_clock_the_pause_the_tempo_and_the_motion_mode() {
        let mut ctx = scene();
        applied_diff(&mut ctx, &Command::Checkpoint);

        tick(&mut ctx, 60);
        assert!((ctx.clock() - 1.0).abs() < 1e-12);
        applied_diff(&mut ctx, &Command::Pause);
        applied_diff(&mut ctx, &Command::Tempo { factor: 2.0 });
        applied_diff(
            &mut ctx,
            &Command::Motion {
                mode: command::MotionMode::Reduced,
            },
        );

        applied_diff(&mut ctx, &Command::Undo);
        assert_eq!(ctx.clock(), 0.0);
        assert!(!ctx.is_paused());
        assert_eq!(ctx.tempo(), 1.0);
        assert_eq!(ctx.motion, Motion::Full);

        // And it plays again at the restored pace rather than staying frozen.
        tick(&mut ctx, 30);
        assert!((ctx.clock() - 0.5).abs() < 1e-12);
    }

    /// Undo interrupts rather than rewinding. The restored tree is exactly what
    /// the mark saw — an animation in flight is cleared without `finish`, so a
    /// `FadeOut` that half happened leaves its target at the checkpoint's
    /// opacity rather than removed or half-faded.
    ///
    /// The semantic tree is the assertion because it is the same document a host
    /// reads back: anything the restore got wrong about a label, a bound or the
    /// camera shows up here, not only in the node count. The diff names the node
    /// and it should: the half-played fade *did* change its opacity, so a host
    /// mirroring the tree has to hear that the restore put it back.
    #[test]
    fn undo_interrupts_playback_and_restores_the_snapshot_exactly() {
        let mut ctx = scene();
        let id = square(&mut ctx);
        let before = ctx.semantic_tree(None);
        applied_diff(&mut ctx, &Command::Checkpoint);

        ctx.play(FadeOut::new(id).with_duration(1.0));
        tick(&mut ctx, 30);
        assert!(ctx.is_playing());
        let half = opacity(&ctx, id);
        assert!(half > 0.0 && half < 1.0, "opacity {half} is not mid-fade");

        let diff = applied_diff(&mut ctx, &Command::Undo);
        assert_eq!(diff.modified, vec![id]);
        assert!(
            diff.created.is_empty() && diff.removed.is_empty(),
            "{diff:?}"
        );
        assert!(!ctx.is_playing(), "the abandoned beat was left running");
        assert_eq!(ctx.semantic_tree(None), before);
        assert_eq!(opacity(&ctx, id), 1.0);

        // The cleared animation does not creep afterwards: it was dropped, not
        // paused.
        tick(&mut ctx, 120);
        assert_eq!(opacity(&ctx, id), 1.0);
    }

    /// A node created after the mark is not handed out again. Cloning a tree
    /// clones its slot versions, so without the retirement set the next `add`
    /// would reuse the abandoned id and a host still holding it would silently
    /// address a different object.
    #[test]
    fn an_id_from_the_abandoned_future_is_never_handed_out_again() {
        let mut ctx = scene();
        applied_diff(&mut ctx, &Command::Checkpoint);
        let gone = square(&mut ctx);
        applied_diff(&mut ctx, &Command::Undo);

        let fresh = square(&mut ctx);
        assert_ne!(fresh, gone, "the undone id was reissued");
        // And naming the dead id is a clean refusal, not a hit on the new node.
        let err = ctx
            .handle(&Command::Set {
                target: gone.into(),
                property: command::Property::Opacity(0.5),
            })
            .expect_err("the id was retired");
        assert!(matches!(err, CommandError::NoSuchNode { .. }), "{err:?}");
    }

    /// Several marks unwind newest first, one snapshot per undo.
    #[test]
    fn checkpoints_unwind_newest_first() {
        let mut ctx = scene();
        let a = square(&mut ctx);
        applied_diff(&mut ctx, &Command::Checkpoint);
        let b = square(&mut ctx);
        applied_diff(&mut ctx, &Command::Checkpoint);
        let c = square(&mut ctx);

        assert_eq!(applied_diff(&mut ctx, &Command::Undo).removed, vec![c]);
        assert_eq!(applied_diff(&mut ctx, &Command::Undo).removed, vec![b]);
        assert!(ctx.tree.get(a).is_some());
        assert!(ctx.tree.get(b).is_none());
        assert_eq!(
            ctx.undo(),
            Err(CommandError::NoCheckpoint),
            "three marks, two objects after them"
        );
    }

    /// The stack is bounded, and what it forgets is the *oldest* mark.
    #[test]
    fn the_checkpoint_stack_keeps_the_newest_and_drops_the_oldest() {
        let mut ctx = scene();
        for _ in 0..=SceneContext::MAX_CHECKPOINTS {
            applied_diff(&mut ctx, &Command::Checkpoint);
            square(&mut ctx);
        }

        for _ in 0..SceneContext::MAX_CHECKPOINTS {
            applied_diff(&mut ctx, &Command::Undo);
        }
        assert_eq!(
            ctx.root_ids().len(),
            1,
            "the state before the first add is not reachable"
        );
        assert_eq!(ctx.undo(), Err(CommandError::NoCheckpoint));
    }

    /// Without a mark the answer is a refusal naming the state, not a panic —
    /// a model that sends an extra `undo` must be told what was wrong with it.
    #[test]
    fn undo_without_a_checkpoint_is_a_refusal() {
        let mut ctx = scene();
        let err = ctx.undo().expect_err("nothing was marked");
        assert_eq!(err, CommandError::NoCheckpoint);
        assert_eq!(format!("{err}"), "no checkpoint to restore");

        let err = ctx
            .handle(&Command::Undo)
            .expect_err("the handler answers the same way");
        assert_eq!(err, CommandError::NoCheckpoint);
        // The mark itself is idempotent in effect: two marks and one undo leave
        // the session at the same place, and the second undo works.
        applied_diff(&mut ctx, &Command::Checkpoint);
        applied_diff(&mut ctx, &Command::Checkpoint);
        assert!(applied_diff(&mut ctx, &Command::Undo).is_empty());
        assert!(applied_diff(&mut ctx, &Command::Undo).is_empty());
        assert_eq!(ctx.undo(), Err(CommandError::NoCheckpoint));
    }

    /// A `set` that writes the value a property already had is not a
    /// modification. The mobject's dirty flags are bookkeeping the draw-list
    /// diff replaced, so an equality that read them would report a restore of a
    /// no-op as a change.
    #[test]
    fn a_write_that_changed_nothing_is_not_a_modification() {
        let mut ctx = scene();
        let id = square(&mut ctx);
        // A fresh object carries `DirtyFlags::ALL`; clearing them makes the
        // no-op write below observable at all.
        ctx.tree.get_mut(id).unwrap().dirty = Default::default();

        applied_diff(&mut ctx, &Command::Checkpoint);
        applied_diff(
            &mut ctx,
            &Command::Set {
                target: id.into(),
                property: command::Property::Opacity(1.0),
            },
        );
        assert!(
            ctx.tree.get(id).unwrap().dirty.any(),
            "the premise: the write did dirty the object"
        );

        let diff = applied_diff(&mut ctx, &Command::Undo);
        assert!(diff.is_empty(), "{diff:?}");
    }

    /// A reset is a new scene, not a step back: the old marks would restore a
    /// tree the rewritten script never built.
    #[test]
    fn reset_forgets_the_history() {
        let mut ctx = scene();
        applied_diff(&mut ctx, &Command::Checkpoint);
        ctx.reset();
        assert_eq!(ctx.undo(), Err(CommandError::NoCheckpoint));
    }

    /// A command spelled as JSON. A batch is written as data on the wire, so
    /// writing its members as data here is the same test the protocol makes.
    fn command(json: &str) -> Command {
        serde_json::from_str(json).unwrap_or_else(|e| panic!("parsing {json}: {e}"))
    }

    /// The reply is the **net** change of the list, not the concatenation of
    /// its members' diffs: an object created inside the list is `created` even
    /// if a later member also modified it, and a pair that adds and removes
    /// the same node appears in neither half.
    #[test]
    fn a_batch_applies_in_order_and_reports_the_net_change() {
        let mut ctx = scene();
        let diff = applied_diff(
            &mut ctx,
            &Command::Batch {
                commands: vec![
                    command(
                        r#"{"action":"add","type":"circle","params":{"radius":1.0,"label":"圆"}}"#,
                    ),
                    command(
                        r#"{"action":"set","target":"圆","property":{"name":"opacity","value":0.5}}"#,
                    ),
                    command(r#"{"action":"add","type":"square","params":{"label":"临时"}}"#),
                    command(r#"{"action":"remove","target":"临时"}"#),
                ],
            },
        );

        assert_eq!(diff.created.len(), 1, "{diff:?}");
        let circle = diff.created[0];
        assert!(
            diff.modified.is_empty(),
            "a create-then-modify is one creation: {diff:?}"
        );
        assert!(
            diff.removed.is_empty(),
            "a create-then-remove cancels: {diff:?}"
        );
        assert_eq!(ctx.tree.len(), 1, "the temporary square is gone");
        let m = ctx.tree.get(circle).unwrap();
        assert_eq!(m.opacity, 0.5, "the members ran in order");
        assert_eq!(m.label.as_deref(), Some("圆"));
    }

    /// A refusal stops the list at the member that caused it and names that
    /// member's index: everything before it is applied, nothing after it runs.
    /// That is the documented half-application — a host wanting all-or-nothing
    /// takes a checkpoint first — and the index is what makes it actionable.
    #[test]
    fn a_batch_stops_at_the_first_refusal_and_names_the_member() {
        let mut ctx = scene();
        let error = ctx
            .batch(&[
                command(r#"{"action":"add","type":"circle","params":{"label":"圆"}}"#),
                command(r#"{"action":"remove","target":"没有这个"}"#),
                command(r#"{"action":"add","type":"square","params":{"label":"未执行"}}"#),
            ])
            .unwrap_err();

        assert_eq!(
            error,
            CommandError::InBatch {
                index: 1,
                cause: Box::new(CommandError::NoSuchLabel {
                    label: "没有这个".to_owned(),
                }),
            }
        );
        assert_eq!(ctx.tree.len(), 1, "the prefix stays, the suffix never ran");
        assert_eq!(ctx.tree.ids_with_label("圆").len(), 1);
        assert!(ctx.tree.ids_with_label("未执行").is_empty());
    }

    /// The list's *shape* is checked before anything runs, so a malformed list
    /// is atomic. Each member kind that cannot live in a list is refused at its
    /// own position — `commands[i]`, the field spelling every refusal uses.
    #[test]
    fn a_malformed_list_is_refused_before_anything_runs() {
        let mut ctx = scene();
        assert_eq!(
            ctx.batch(&[]).unwrap_err(),
            CommandError::OutOfRange {
                property: "commands".to_owned(),
                detail: "a command list needs at least one command".to_owned(),
            }
        );

        for member in [
            command(r#"{"action":"query"}"#),
            command(r#"{"action":"hello"}"#),
            Command::Batch {
                commands: vec![Command::Pause],
            },
            Command::Validate {
                commands: vec![Command::Pause],
            },
        ] {
            let list = [
                command(r#"{"action":"add","type":"circle"}"#),
                member.clone(),
            ];
            let error = ctx.batch(&list).unwrap_err();
            assert!(
                matches!(
                    error,
                    CommandError::OutOfRange { ref property, .. } if property == "commands[1]"
                ),
                "{member:?} was not refused at its position: {error:?}"
            );
            assert!(
                ctx.tree.is_empty(),
                "a shape refusal applied something: {error}"
            );

            // The dry run refuses the same list the same way.
            assert_eq!(ctx.validate(&list).unwrap_err(), error);
        }
    }

    /// A batch may queue beats; its diff still reports only the tree, which a
    /// queued animation does not change.
    #[test]
    fn a_batch_can_queue_beats_and_reports_no_tree_change() {
        let mut ctx = scene();
        let id = invisible_square(&mut ctx);
        let diff = applied_diff(
            &mut ctx,
            &Command::Batch {
                commands: vec![command(&format!(
                    r#"{{"action":"play","animation":{{"target":{},"type":"fade_in","duration":1.0}}}}"#,
                    id.raw()
                ))],
            },
        );
        assert!(diff.is_empty(), "{diff:?}");
        assert!(ctx.is_playing(), "the beat was queued");
        tick(&mut ctx, 30);
        assert!(opacity(&ctx, id) > 0.0, "and it runs");
    }

    /// A dry run and the real call agree about *where* a list fails, and only
    /// the real call changes anything.
    #[test]
    fn validate_reports_the_refusal_the_real_call_would_give_and_changes_nothing() {
        let mut ctx = scene();
        let id = square(&mut ctx);
        let commands = vec![
            command(r#"{"action":"add","type":"circle","params":{"label":"圆"}}"#),
            command(r#"{"action":"remove","target":"没有这个"}"#),
        ];

        let refusal = ctx.validate(&commands).unwrap_err();
        assert_eq!(
            refusal,
            CommandError::InBatch {
                index: 1,
                cause: Box::new(CommandError::NoSuchLabel {
                    label: "没有这个".to_owned(),
                }),
            }
        );
        assert_eq!(ctx.tree.len(), 1, "the dry run left a node behind");
        assert!(ctx.tree.get(id).is_some());
        assert!(ctx.tree.ids_with_label("圆").is_empty());

        assert_eq!(ctx.batch(&commands).unwrap_err(), refusal);
    }

    /// Members see each other, which is what makes validating a plan worth
    /// doing: a label created by member 0 resolves for member 1 even though the
    /// live tree has never heard of it.
    #[test]
    fn validate_resolves_a_label_created_by_an_earlier_member() {
        let mut ctx = scene();
        ctx.validate(&[
            command(r#"{"action":"add","type":"circle","params":{"label":"圆"}}"#),
            command(r#"{"action":"set","target":"圆","property":{"name":"opacity","value":0.5}}"#),
        ])
        .expect("the second member names the first member's object");
        assert!(ctx.tree.is_empty(), "and the live tree was not touched");
    }

    /// A `play` is checked by building the animation and dropping it: the spec
    /// and its target are validated, no beat reaches the live queue.
    #[test]
    fn validate_checks_a_play_spec_without_queueing_a_beat() {
        let mut ctx = scene();
        let id = square(&mut ctx);
        let error = ctx
            .validate(&[
                command(&format!(
                    r#"{{"action":"play","animation":{{"target":{},"type":"fade_in"}}}}"#,
                    id.raw()
                )),
                command(r#"{"action":"play","animation":{"target":999,"type":"fade_in"}}"#),
            ])
            .unwrap_err();
        assert!(
            matches!(
                error,
                CommandError::InBatch { index: 1, ref cause, .. }
                    if matches!(**cause, CommandError::NoSuchNode { .. })
            ),
            "{error:?}"
        );
        assert!(!ctx.is_playing(), "the dry run queued a beat");
    }

    /// The scratch copy carries the checkpoint stack, so an `undo` member is
    /// validated against the history a real one would unwind — and the live
    /// stack is left for the real undo.
    #[test]
    fn validate_copies_the_checkpoint_stack_so_an_undo_member_is_checked() {
        let mut ctx = scene();
        square(&mut ctx);
        applied_diff(&mut ctx, &Command::Checkpoint);
        let second = square(&mut ctx);

        ctx.validate(&[Command::Undo])
            .expect("the scratch context has the stack");

        assert!(
            ctx.tree.get(second).is_some(),
            "the dry run restored the live tree"
        );
        ctx.undo().expect("the live stack still holds the mark");
        assert!(ctx.tree.get(second).is_none());
    }

    /// The host's typesetter travels into the copy and back: a `text` member is
    /// checked by the provider that would really build it, and the live context
    /// still has that provider afterwards.
    #[test]
    fn validate_typesets_on_the_copy_and_puts_the_provider_back() {
        let mut bare = scene();
        let error = bare.validate(&[add_text()]).unwrap_err();
        assert!(
            matches!(
                error,
                CommandError::InBatch { index: 0, ref cause, .. }
                    if matches!(**cause, CommandError::Unsupported { .. })
            ),
            "a build with no provider must validate the same refusal: {error:?}"
        );

        let mut ctx = with_typesetter(FakeTypesetter::both());
        ctx.validate(&[add_text()])
            .expect("the provider went into the copy");
        assert!(
            ctx.tree.is_empty(),
            "nothing was typeset into the real tree"
        );

        applied_diff(&mut ctx, &add_text());
        assert_eq!(ctx.tree.len(), 1, "the provider came back");
    }
}
