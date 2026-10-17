//! The scene trait and the context a scene is handed.

use rustim_anim::{Animation, AnimationScheduler, Motion, Sequence, Wait};
use rustim_math::Viewport;
use rustim_mobject::{NodeId, SceneTree};
use rustim_render::{DrawList, DrawOp};

use crate::a11y;
use crate::command::{self, Command, CommandError, StateDiff};

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
    /// How much of the motion to play, for the whole run.
    ///
    /// Fixed at construction rather than set afterwards, because a scene is allowed
    /// to consult it while scripting (`construct` may skip a flourish it knows will
    /// not be animated) and a context with a mode that arrives *after* `construct`
    /// would let the two disagree. See [`Motion`] for what `Reduced` does to a beat
    /// — the rule is in `rustim_anim::Sequence::start_next`, the one place a beat
    /// reaches the clock.
    pub motion: Motion,
    /// The scripted beats not yet started — deliberately private.
    ///
    /// The fields above are public because a scene may read and write them
    /// directly; this one is not, because a queue advanced in any order other than
    /// "the scheduler drained, so start the next beat" is a *silently different
    /// timeline*, and the only way to keep that from happening is to be the only
    /// thing that can advance it ([`SceneContext::advance`]).
    playback: Sequence,
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
            playback: Sequence::new(),
        }
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
        if self.animations.is_empty() {
            self.playback
                .start_next(&mut self.animations, &mut self.tree, self.motion);
        }
        self.animations.update(dt, &mut self.tree);
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
    }

    /// Apply a command from outside the engine.
    ///
    /// [`Command::Play`] is answered **here** rather than in `command::apply`: a
    /// play needs the playback machine, and `apply`'s documented purity — "pure
    /// with respect to everything except `tree`: no clock, no I/O, no renderer" —
    /// is what makes the rest of the protocol testable in isolation and replayable.
    /// The wire→animation mapping still lives in `command::animation_for`, next to
    /// the wire types.
    ///
    /// The returned diff is **empty**, and that is the honest answer rather than a
    /// placeholder: a diff describes the tree, and queueing a beat does not change
    /// the tree — it changes what will happen to it. The animation would report its
    /// own modification when it runs; an `ANIMATION_DONE` announcement is how a host
    /// learns that it did.
    pub fn handle(&mut self, cmd: &Command) -> Result<StateDiff, CommandError> {
        if let Command::Play { animation } = cmd {
            let anim = command::animation_for(&self.tree, animation)?;
            self.play_all([anim]);
            return Ok(StateDiff::default());
        }
        command::apply(&mut self.tree, cmd)
    }

    /// This scene, projected into the semantic tree.
    ///
    /// Pure and cheap enough to call per frame, which is what the accessibility
    /// layer does — see `rustim-app::a11y` for the volume at which that stops
    /// being true.
    ///
    /// There is deliberately no `..._json` sibling here. `serde_json` is a test
    /// dependency everywhere in this workspace so that it cannot reach the
    /// `wasm32-unknown-unknown` kernel build, and this is a kernel crate; a host
    /// that wants JSON calls `serde_json::to_string(&ctx.semantic_tree(None))` on
    /// its own side of the boundary. That works because [`a11y::SemanticTree`] is
    /// derived data with no platform types in it.
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
        list.push(DrawOp::SetTransform(self.viewport.affine()));
    }

    /// Flatten the tree into a backend-neutral draw list.
    ///
    /// This is where the deferred transform is finally paid for: `to_bez_path`
    /// folds each mobject's own transform together with its ancestors'. Nothing
    /// upstream of this call ever materialises world coordinates.
    pub fn build_draw_list(&self, list: &mut DrawList) {
        self.begin_frame(list);
        for id in self.tree.draw_order() {
            let Some(m) = self.tree.get(id) else { continue };
            // Points rather than `Mobject::has_picture`, and deliberately so: this
            // walk can emit one thing, a filled path, so an object with no points
            // has nothing here to draw *and nothing is missing* — a text mobject
            // is drawn by the scene that shaped it. The wider predicate is what
            // the keyboard and the semantic tree ask, and they must not ask this
            // one, or text would be unreadable by both.
            if m.opacity <= 0.0 || m.points.len() < 2 {
                continue;
            }
            let alpha = m.opacity * m.style.fill_opacity;
            let mut color = m.style.fill;
            color.components[3] = alpha as f32;
            list.push(DrawOp::SetPaint(color));
            // The mobject's own transform is folded in here rather than via a
            // `SetTransform`, because the draw list's transform is global to the
            // frame (it carries the viewport) and mixing the two would require
            // save/restore. The *ancestors'* transform is folded in by handing
            // `to_bez_path` the accumulated parent transform — passing
            // `Affine2::IDENTITY` instead silently drops every ancestor transform,
            // which is invisible only while the tree stays flat.
            list.push(DrawOp::FillPath(
                m.to_bez_path(self.tree.parent_transform(id)),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_anim::{FadeIn, Motion, MoveTo, Wait, rate};
    use rustim_math::Affine2;
    use rustim_mobject::shapes::{ShapeKind, ShapeParams};

    /// The frame grid. Virtual time, never wall-clock — the same `1/60` the app's
    /// frame loop and the golden capture use.
    const DT: f64 = 1.0 / 60.0;

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

    #[test]
    fn commands_routed_through_the_context_stay_in_sync() {
        let mut ctx = scene();
        let cmd: Command =
            serde_json::from_str(r#"{"action":"add","type":"square","params":{"width":3.0}}"#)
                .unwrap();
        let diff = ctx.handle(&cmd).unwrap();
        assert_eq!(ctx.root_ids(), diff.created.as_slice());
        assert_eq!(ctx.tree.len(), 1);
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
                target: id,
                kind: command::AnimationKind::FadeIn,
                duration: Some(0.5),
                to: None,
                angle: None,
                factor: None,
                about_point: None,
            },
        };
        ctx.handle(&cmd).unwrap();

        tick(&mut ctx, 1);
        assert_eq!(opacity(&ctx, id), 1.0);
        assert!(ctx.is_playing(), "and still holds the beat's own length");
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
                target: id,
                kind: command::AnimationKind::FadeIn,
                duration: Some(0.5),
                to: None,
                angle: None,
                factor: None,
                about_point: None,
            },
        };

        let diff = ctx.handle(&cmd).unwrap();
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
                target: ghost,
                kind: command::AnimationKind::FadeIn,
                duration: None,
                to: None,
                angle: None,
                factor: None,
                about_point: None,
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
}
