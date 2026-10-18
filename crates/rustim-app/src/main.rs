//! The reference application: a real-time window driving a scene.
//!
//! ```text
//! winit event loop  ->  Scene::update  ->  SceneContext::build_draw_list
//!                   ->  CpuRenderer     ->  softbuffer  ->  the screen
//! ```
//!
//! This is the architecture from `方案建议.md` §4.3 with the presentation layer
//! chosen for the hardware it has to run on. There is no GPU in this path at
//! all, on purpose: Phase 0 measured the floor machine's Vulkan driver
//! segfaulting on any render pipeline and its GL driver rejecting vello's
//! shaders, so the software rasteriser is the engine here rather than the
//! fallback (`rustim_render::vello_cpu_backend` has the details).
//!
//! Usage:
//! ```text
//! rustim-app                      # open a window, run until closed
//! rustim-app --frames 300         # ...and exit after 300 frames
//! rustim-app --png out.png        # no window: render one frame to a PNG
//! rustim-app --inspect            # no window: print the accessibility tree
//! rustim-app --a11y-probe         # windowed: did a screen reader attach?
//! rustim-app --scene text --png t.png
//! rustim-app --script session.jsonl --reply   # drive the engine with commands
//! ```
//!
//! `--script` is the point of the whole crate: a session is a list of JSON
//! commands, one per line, and the same list replays to the same frames. See
//! [`rustim_app::feed`] for the transport and `rustim_scene::command` for the
//! protocol.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::Instant;

use rustim_anim::Motion;
use rustim_app::a11y::{A11y, Request};
use rustim_app::{AppEvent, Intent, feed, input};
use rustim_math::{Affine2, BezPath, Camera, Point, Viewport};
use rustim_mobject::{Mobject, NodeId, NodeKind, Style};
use rustim_render::{CpuRenderer, Damage, DrawList, DrawOp, RenderError, RenderStats, Renderer};
use rustim_scene::command::{Command, MotionMode};
use rustim_scene::{Scene, SceneContext};
use rustim_text::{FontProvider, ShapedText, TextStyle};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

const WIDTH: u16 = 1280;
const HEIGHT: u16 = 720;

/// The virtual timestep a `,` or `.` press advances the animation clock by.
///
/// The same one sixtieth the headless `--png` path uses, so the frame a user
/// steps to and the frame an export renders are the same frame.
const STEP: f64 = 1.0 / 60.0;

/// Announced when the last running animation finishes.
///
/// Worth saying only because an animation is the one thing a blind user cannot
/// poll for: the scene is unchanged on either side of it, so without this there
/// is no way to know the transition is over. The throttle collapses a burst of
/// animations into one announcement.
const ANIMATION_DONE: &str = "动画结束";

/// A style that fills and does not stroke — what every closed shape in these
/// scenes wants.
fn filled(color: rustim_color::Srgb, alpha: f64) -> Style {
    Style {
        fill: color,
        fill_opacity: alpha,
        stroke: color,
        stroke_opacity: 0.0,
        stroke_width: 0.0,
    }
}

/// A style that strokes a centerline and does not fill it.
///
/// The tree walk emits the fill pass first and the stroke pass second, so a
/// style may legitimately have both on. Lines and plots want only the stroke:
/// filling an open polyline paints the area under it, which is the defect the
/// plot's stroke exists to avoid. `width` is in world units.
fn stroked(color: rustim_color::Srgb, width: f64) -> Style {
    Style {
        fill: color,
        fill_opacity: 0.0,
        stroke: color,
        stroke_opacity: 1.0,
        stroke_width: width,
    }
}

// ---------------------------------------------------------------------------
// The transform scene: the first morph
// ---------------------------------------------------------------------------

/// Place a shape so its own centre sits at `at`.
///
/// Shapes are built in their own coordinates — [`rustim_mobject::shapes::square`]
/// and `circle` are centred, `right_triangle` has its right angle *at the origin*
/// — so a scene that wants two of them side by side has to place them by
/// measurement. `critical_point((0, 0))` is the bounding box centre of the
/// materialised anchors, which is the local centre while the transform is still
/// the identity.
fn centred(mut m: Mobject, at: (f64, f64)) -> Mobject {
    // A shape built from points always has anchors; the fallback keeps this a
    // placement helper rather than one that can abort a scene.
    let c = m.critical_point((0.0, 0.0)).unwrap_or(Point::new(0.0, 0.0));
    m.transform = Affine2::translate((at.0 - c.x, at.1 - c.y));
    m
}

/// What the two sources are once the morph has settled.
///
/// A `Transform` moves geometry, paint and opacity, and it deliberately says
/// nothing about what the object *is* — it could not, since nothing in the engine
/// knows that a triangle has become a circle. So the scene says it, once, when the
/// morph ends, which is what any caller has to do. Without it the accessibility
/// tree speaks from the stale kind: `--inspect` on this scene described the
/// settled circle as "两条直角边长 3.2 和 3.2 的直角三角形，面积 5.12" and the
/// settled triangle as a 2.6-square of area 6.76 — sizes measured honestly from
/// the geometry, and both false. No pixel changes when the kind is wrong, which is
/// exactly why `--inspect` is the tool that sees it.
const SETTLED: [(&str, NodeKind); 2] = [
    ("圆形", NodeKind::Circle),
    ("直角三角形", NodeKind::RightTriangle),
];

/// Two morphs, one growing and one shrinking, so both alignment directions run.
///
/// The targets are **scene fields and not tree nodes**, which is the shape of
/// Manim's `Transform`: the target is a donor that `begin` copies and that is
/// never added to the scene (`animation/transform.py:201-203`). A plain
/// `Transform` therefore leaves the *source* wearing the target's data — the
/// settled frame shows two objects, not four — and the source also keeps its id,
/// its `NodeKind` and its label, which is Manim's behaviour and not an oversight:
/// the animation is about geometry, and the engine has no way to know that a
/// triangle has become a circle. So the scene says what the sources have become,
/// in [`SETTLED`], once the morph ends — which is what any caller has to do, and
/// is the reason that const reads the way it does.
///
/// The pair is chosen so both directions of `align_points` run: on the left the
/// 3-curve triangle is padded *up* to the circle's 4 curves, on the right it is
/// the square's target that is ragged and gets padded. A scene with only one
/// direction would leave the other half of the alignment port unexercised.
struct TransformScene {
    sources: Vec<NodeId>,
    /// The donors, held so the scene owns them long enough to hand them over.
    /// Nothing about rendering reads them back.
    targets: Vec<Mobject>,
    /// Whether the post-timeline relabel has happened.
    ///
    /// The one piece of state this scene cannot do without: the relabel belongs to
    /// the moment the morph *settles*, not to the moment it was queued — the same
    /// distinction that made a bug in step 2, where the semantic tree was rebuilt
    /// from the pre-morph state. A `play` call cannot express it, so it stays in
    /// `update` and is guarded by [`SceneContext::is_playing`].
    settled: bool,
}

impl TransformScene {
    fn new() -> Self {
        Self {
            sources: Vec::new(),
            targets: Vec::new(),
            settled: false,
        }
    }
}

impl Scene for TransformScene {
    fn construct(&mut self, ctx: &mut SceneContext) {
        // Left: 12 points / 3 curves morphing to 16 / 4. Blue to translucent
        // yellow, so the settled still pins the *style* lerp arriving as well as
        // the geometry — a morph that moved the points and left the paint behind
        // would show up as a blue circle.
        let mut tri = centred(
            Mobject::new(rustim_mobject::shapes::right_triangle(3.0, 3.0)),
            (-3.0, 0.0),
        )
        .with_style(filled(rustim_color::BLUE, 1.0))
        .with_kind(NodeKind::RightTriangle)
        .with_label("三角形");
        tri.opacity = 0.0;

        // Right: 16 points / 4 curves, morphing to a *ragged* target. Red to
        // opaque white, so this side pins the other half of the style lerp.
        let mut sq = centred(
            Mobject::new(rustim_mobject::shapes::square(2.6, 2.6)),
            (3.0, 0.0),
        )
        .with_style(filled(rustim_color::RED, 1.0))
        .with_kind(NodeKind::Square)
        .with_label("正方形");
        sq.opacity = 0.0;

        self.targets = vec![
            centred(
                Mobject::new(rustim_mobject::shapes::circle(1.6)),
                (-3.0, 0.0),
            )
            .with_style(filled(rustim_color::YELLOW, 0.45))
            .with_kind(NodeKind::Circle)
            .with_label("圆形"),
            centred(
                Mobject::new(rustim_mobject::shapes::right_triangle(2.6, 2.6)),
                (3.0, 0.0),
            )
            .with_style(filled(rustim_color::FG, 1.0))
            .with_kind(NodeKind::RightTriangle)
            .with_label("直角三角形"),
        ];

        let (a, b) = (ctx.add(tri), ctx.add(sq));
        self.sources = vec![a, b];

        // Two beats, each parallel: both shapes fade together, then both morph
        // together. The second starts only once the first has drained, so a slow
        // frame makes the sequence later and never starts a morph from a
        // half-faded shape.
        ctx.play_all(
            self.sources
                .iter()
                .map(|id| rustim_anim::FadeIn::new(*id).with_duration(0.4)),
        );
        ctx.play_all(
            self.sources
                .iter()
                .zip(self.targets.iter())
                .map(|(id, target)| {
                    rustim_anim::Transform::new(*id, target.clone()).with_duration(1.5)
                }),
        );
    }

    fn update(&mut self, dt: f64, ctx: &mut SceneContext) {
        // `advance` first, always: draining the queue is the playback machine's one
        // job and a scene that skipped it would stall the whole script behind a
        // frozen frame. Everything after this line is this scene reacting to the
        // machine having finished.
        ctx.advance(dt);

        // Reached once the morph has *settled*, not when it was queued — the
        // difference step 2's relabel bug was made of, and one `play` call cannot
        // express. `is_playing` rather than `animations.is_empty()`: the scheduler
        // is momentarily empty between the two beats, and the fades alone would
        // otherwise trip this.
        if self.settled || ctx.is_playing() {
            return;
        }
        for (id, (label, kind)) in self.sources.iter().zip(SETTLED) {
            if let Some(m) = ctx.tree.get_mut(*id) {
                m.label = Some((*label).to_owned());
                m.kind = kind;
                m.dirty.semantic = true;
            }
        }
        self.settled = true;
    }
}

// ---------------------------------------------------------------------------
// The rotate scene: a pivot, an in-place scale, and a removal
// ---------------------------------------------------------------------------

/// The arrow's tail, in world units.
///
/// A named constant because it is used twice and the two uses have to agree: once
/// to place the arrow, and once as the `Rotate` pivot. A scene that measured the
/// tail back out of `critical_point` would be testing its own arithmetic against
/// itself, and the whole claim of the middle beat — "the tail does not move" — is a
/// claim about *this* point.
const ARROW_TAIL: (f64, f64) = (-4.5, -1.6);

/// How far the arrow turns, in radians — about 34°, deliberately not a multiple of
/// a right angle: a quarter turn about a pivot lies on the pivot's axis and hides
/// which pivot was used, since the two candidates very nearly agree.
const ARROW_TURN: f64 = 0.6;

/// Three beats, each the only place one claim is visible.
///
/// 1. A fade-in of an **arrow**, a **circle** and a **square**, together.
/// 2. A `Rotate` of the arrow about **its tail** while a `Scale` grows the circle
///    **in place**. The settled frame is the evidence for both pivots at once: the
///    arrow is at an angle with its tail where it started, and the circle is
///    bigger around an unmoved centre. Neither could be read off a scene that did
///    one of them.
/// 3. A `FadeOut` of the square. **The settled frame has two objects, not three**,
///    and `--inspect` lists two nodes — the only way to see that the removal
///    happened rather than that the square merely became transparent.
///
/// There is no `update` override at all, unlike [`TransformScene`]'s post-timeline
/// relabel. That is itself the claim: the removal is the *animation's* business
/// (`Animation::finish`), not something the script has to notice and re-do, so this
/// scene is pure `construct`.
struct RotateScene;

impl Scene for RotateScene {
    fn construct(&mut self, ctx: &mut SceneContext) {
        // The arrow is a *tail-anchored* shape — `shapes::arrow` puts its tail at
        // the local origin — so its transform is the tail's position outright and
        // `centred` (which would place its middle) is exactly wrong here.
        let mut arrow = Mobject::new(rustim_mobject::shapes::arrow(3.0, 0.06))
            .with_style(filled(rustim_color::BLUE, 1.0))
            .with_kind(NodeKind::Arrow)
            .with_label("位移");
        arrow.transform = Affine2::translate(ARROW_TAIL);
        arrow.opacity = 0.0;

        let mut circle = centred(
            Mobject::new(rustim_mobject::shapes::circle(1.0)),
            (2.5, -0.6),
        )
        .with_style(filled(rustim_color::YELLOW, 1.0))
        .with_kind(NodeKind::Circle)
        .with_label("圆的面积");
        circle.opacity = 0.0;

        let mut square = centred(
            Mobject::new(rustim_mobject::shapes::square(1.5, 1.5)),
            (-0.4, 2.4),
        )
        .with_style(filled(rustim_color::RED, 1.0))
        .with_kind(NodeKind::Square)
        .with_label("待淡出的正方形");
        square.opacity = 0.0;

        let arrow_id = ctx.add(arrow);
        let circle_id = ctx.add(circle);
        let square_id = ctx.add(square);

        ctx.play_all(
            [arrow_id, circle_id, square_id]
                .into_iter()
                .map(|id| rustim_anim::FadeIn::new(id).with_duration(0.4)),
        );

        // One beat, two animations: a rotation whose parameter is an angle and a
        // scale whose parameter is a factor — neither of which a componentwise
        // affine lerp could interpolate, which is why both have their own type.
        let turn = rustim_anim::Rotate::new(arrow_id, ARROW_TURN)
            .about_point(Point::new(ARROW_TAIL.0, ARROW_TAIL.1))
            .with_duration(1.2);
        let grow = rustim_anim::Scale::new(circle_id, 1.8).with_duration(1.2);
        ctx.play_all([
            Box::new(turn) as Box<dyn rustim_anim::Animation>,
            Box::new(grow),
        ]);

        ctx.play(rustim_anim::FadeOut::new(square_id).with_duration(0.6));
    }
}

// ---------------------------------------------------------------------------
// The primitives scene: raw draw operations
// ---------------------------------------------------------------------------

/// Three filled shapes on the top row, the same outlines stroked at increasing
/// widths below, and a clipped panel on the right.
///
/// This scene exists for the golden-image suite. The other two scenes are made
/// of mobjects, and the tree walk only ever emits a flat fill — so without a
/// scene that pushes strokes and clips itself, the baselines would never notice
/// a regression in stroke expansion or in clip rasterisation. Those are the two
/// most intricate things `vello_cpu` does and the two that would fail silently.
///
/// It builds its own draw list rather than adding mobjects because nothing in
/// the mobject model expresses a stroke or a clip yet. That is a gap to close
/// when the scene layer grows those operations; it is not a reason to leave the
/// rasteriser unexercised in the meantime.
struct PrimitivesScene;

/// One pixel, in world units — 8 units over 720 pixels.
///
/// Stroke widths are in the geometry's own space, so they scale with the
/// viewport like everything else. Naming this stops a "4" being written where a
/// quarter-pixel or a 4-pixel line was meant.
const PX: f64 = 8.0 / 720.0;

/// A point array translated into place, as a closed path.
fn placed(pts: &[Point], at: (f64, f64)) -> BezPath {
    rustim_mobject::shapes::path_of(
        &pts.iter()
            .map(|p| Affine2::translate(at) * *p)
            .collect::<Vec<_>>(),
    )
}

impl Scene for PrimitivesScene {
    fn construct(&mut self, _ctx: &mut SceneContext) {}

    fn build_draw_list(&self, ctx: &SceneContext, list: &mut DrawList) {
        use rustim_mobject::shapes::{circle, right_triangle, square};

        ctx.begin_frame(list);
        let fill = |list: &mut DrawList, c, p| {
            list.push(DrawOp::SetPaint(c));
            list.push(DrawOp::FillPath(p));
        };
        let stroke = |list: &mut DrawList, c, w, p| {
            list.push(DrawOp::SetPaint(c));
            list.push(DrawOp::StrokePath { path: p, width: w });
        };

        // Top row: fills. A curve, a straight-edged closed path, a triangle, and
        // a circle drawn as an outline so a large stroke is covered as well.
        let (circle_pts, square_pts, tri_pts) =
            (circle(1.0), square(2.0, 2.0), right_triangle(2.0, 2.0));
        fill(list, rustim_color::BLUE, placed(&circle_pts, (-5.0, 2.0)));
        fill(list, rustim_color::YELLOW, placed(&square_pts, (-2.0, 2.0)));
        fill(list, rustim_color::RED, placed(&tri_pts, (0.0, 1.0)));
        stroke(
            list,
            rustim_color::FG,
            4.0 * PX,
            placed(&circle(1.4), (5.0, 2.0)),
        );

        // Bottom row: the same three outlines, stroked. Widths 1/2/4 px so a
        // regression in the stroke-to-fill conversion shows up as a change in
        // thickness rather than only in coverage.
        stroke(
            list,
            rustim_color::BLUE,
            PX,
            placed(&circle_pts, (-5.0, -2.0)),
        );
        stroke(
            list,
            rustim_color::YELLOW,
            2.0 * PX,
            placed(&square_pts, (-2.0, -2.0)),
        );
        stroke(
            list,
            rustim_color::RED,
            4.0 * PX,
            placed(&tri_pts, (0.0, -3.0)),
        );

        // Right: a clip, then geometry that overflows it, then a bar outside the
        // clip region entirely. If `PopClip` ever failed to restore — the two
        // stacks in vello make that a real hazard — the bar would disappear and
        // the baseline would say so.
        let window = rustim_math::Rect::new(3.2, -3.6, 6.4, -0.4);
        list.push(DrawOp::PushClip(rustim_math::rect_path(window)));
        fill(
            list,
            rustim_color::BLUE,
            rustim_math::rect_path(rustim_math::Rect::new(2.5, -4.0, 8.0, 0.0)),
        );
        fill(
            list,
            rustim_color::YELLOW,
            placed(&circle(1.4), (4.8, -2.0)),
        );
        list.push(DrawOp::PopClip);
        fill(
            list,
            rustim_color::RED,
            rustim_math::rect_path(rustim_math::Rect::new(6.8, -3.6, 7.1, -0.4)),
        );
    }
}

// ---------------------------------------------------------------------------
// The composite scene: transforms, alpha, and a transparency group
// ---------------------------------------------------------------------------

/// Overlapping translucent fills, composed transforms, and a layer.
///
/// Where `primitives` covers the far end of the draw list, this covers the
/// machinery upstream of it: `Mobject::transform` folded together with the
/// viewport at draw time, alpha multiplied through `opacity * fill_opacity`, and
/// draw order deciding which translucent fill sits on which.
///
/// The last two pairs are the same two circles drawn twice — once inside a
/// `PushLayer` group and once outside one — and the expected result is that the
/// two pairs are **identical**. Group opacity only separates them when the group
/// carries an alpha of its own, and `DrawOp::PushLayer` has none; source-over is
/// associative, so a layer that merely bounds compositing changes nothing. That
/// is a claim worth an image: it holds only while the layer is genuinely
/// transparent to the composition, and a backend that reset the backdrop at
/// `PushLayer` or multiplied in an alpha would break it here as well as in
/// `vello_cpu_backend`'s unit test.
struct CompositeScene;

impl CompositeScene {
    fn add(
        &self,
        ctx: &mut SceneContext,
        points: Vec<Point>,
        color: rustim_color::Srgb,
        alpha: f64,
        transform: Affine2,
    ) {
        let mut m = Mobject::new(points).with_style(filled(color, alpha));
        m.transform = transform;
        ctx.add(m);
    }
}

impl Scene for CompositeScene {
    fn construct(&mut self, ctx: &mut SceneContext) {
        let square = rustim_mobject::shapes::square(2.0, 2.0);

        // An opaque square with a translucent one overlapping it, so the blend
        // is against a shape rather than against the background.
        self.add(
            ctx,
            square.clone(),
            rustim_color::YELLOW,
            1.0,
            Affine2::translate((-4.4, 1.6)),
        );
        self.add(
            ctx,
            square.clone(),
            rustim_color::RED,
            0.5,
            Affine2::translate((-3.6, 1.6)),
        );

        // Rotation, and rotation composed with a translation and a non-uniform
        // scale — the case where folding transforms in the wrong order is
        // visible as a shape in the wrong place rather than as a subtle shift.
        self.add(
            ctx,
            rustim_mobject::shapes::square(3.0, 3.0),
            rustim_color::BLUE,
            0.5,
            Affine2::rotate(0.35),
        );
        self.add(
            ctx,
            rustim_mobject::shapes::square(2.4, 2.4),
            rustim_color::FG,
            0.5,
            Affine2::rotate(-0.6)
                * Affine2::translate((5.0, 1.2))
                * Affine2::scale_non_uniform(1.0, 0.6),
        );
    }

    fn build_draw_list(&self, ctx: &SceneContext, list: &mut DrawList) {
        // The mobjects first, through the ordinary tree walk …
        ctx.build_draw_list(list);

        // … then the two circle pairs, which have to be pushed as raw ops
        // because "composite these as one unit" is not something a mobject can
        // say yet.
        let pair = |list: &mut DrawList, cx: f64| {
            for dx in [0.0, 0.8] {
                list.push(DrawOp::FillPath(placed(
                    &rustim_mobject::shapes::circle(0.8),
                    (cx + dx, -2.4),
                )));
            }
        };
        let mut half = rustim_color::FG;
        half.components[3] = 0.5;

        list.push(DrawOp::PushLayer);
        list.push(DrawOp::SetPaint(half));
        pair(list, 0.4);
        list.push(DrawOp::PopLayer);

        list.push(DrawOp::SetPaint(half));
        pair(list, -4.4);
    }
}

// ---------------------------------------------------------------------------
// The text scene: shaping, glyph runs, and the deferred transform
// ---------------------------------------------------------------------------

/// Text from the repository's own font, drawn as glyph runs.
///
/// The `parley` half of P1 end to end: a string goes in, `rustim-text` shapes it
/// against a font registered from `assets/fonts` rather than discovered on the
/// system, and [`rustim_app::text`] turns the result into draw operations. It has
/// to be the scene that pushes them, because the tree walk cannot: it emits
/// filled paths, and a text mobject has no points to fill — which is the whole
/// story behind `Mobject::has_picture`.
///
/// Every line is also a mobject in the tree, labelled with its own text. That is
/// not decoration: the accessibility layer reads the label, so `--inspect` on
/// this scene prints what the picture says, and a reader who cannot see it can
/// still be told. A text object whose glyphs were drawn but never registered
/// would be the one failure of this pipeline that no image can show.
///
/// The lines are chosen for what the golden image can catch and nothing else:
/// CJK coverage, a Latin line with superscripts, a wrapped sentence (the
/// multi-line path), and a rotated block — the last of which is the one line that
/// takes `glifo`'s *unabsorbed* path, where the outlines are transformed instead
/// of being built at the size they are drawn at. That path shares no code with
/// the other four, and before this scene existed nothing exercised it.
struct TextScene {
    /// Parallel to `nodes`. Shaped once, in `construct`, because shaping is the
    /// expensive half and nothing about it changes between frames.
    blocks: Vec<ShapedText>,
    /// The node each block belongs to. Read every frame for the transform and the
    /// opacity, so that a command — including the accessibility layer's opacity
    /// adjustment — changes the picture and not only the description.
    nodes: Vec<NodeId>,
}

/// One line of [`TextScene`].
struct Line {
    text: &'static str,
    /// World units per em. Manim's own default is `MANIM_FONT_SIZE_UNITS`; the
    /// title is larger and the paragraph smaller, so one image shows that the
    /// size is honoured rather than hardcoded.
    size: f64,
    color: rustim_color::Srgb,
    /// Where the block's centre goes, in world units.
    at: (f64, f64),
    /// Radians, about the block's own centre.
    rotate: f64,
    opacity: f64,
    /// A wrap width, in world units. `None` for a label, which never wraps.
    max_width: Option<f32>,
}

impl Line {
    const fn new(text: &'static str, size: f64, color: rustim_color::Srgb, at: (f64, f64)) -> Self {
        Self {
            text,
            size,
            color,
            at,
            rotate: 0.0,
            opacity: 1.0,
            max_width: None,
        }
    }
}

/// The whole scene, in one table.
///
/// World units throughout, and the frame is 8 units tall, so these read the way
/// a Manim scene does — no pixel constants, and the same text on a 4K display.
const TEXT_LINES: [Line; 5] = [
    Line::new("勾股定理", 0.8, rustim_color::FG, (0.0, 2.7)),
    Line::new("a² + b² = c²", 0.5, rustim_color::BLUE, (0.0, 1.1)),
    Line::new(
        "斜边 c = √(a² + b²)",
        0.35,
        rustim_color::YELLOW,
        (0.0, 0.0),
    ),
    Line {
        // Two lines once wrapped, which is the only way to see the line-breaking
        // path in an image. The opacity is below 1 on purpose as well: it is the
        // one block that shows the mobject's opacity reaching the paint.
        opacity: 0.75,
        max_width: Some(6.0),
        ..Line::new(
            "在直角三角形中，两条直角边的平方和等于斜边的平方。",
            0.28,
            rustim_color::FG,
            (0.0, -1.5),
        )
    },
    Line {
        rotate: -0.35,
        ..Line::new("旋转的文本", 0.4, rustim_color::RED, (4.4, 2.6))
    },
];

impl TextScene {
    fn new() -> Self {
        Self {
            blocks: Vec::new(),
            nodes: Vec::new(),
        }
    }

    /// Shape every line and register it in the tree.
    ///
    /// A missing font is reported and the scene stays empty rather than panicking:
    /// the same rule the rest of the engine follows, and this path *is* reachable
    /// — a binary installed without `assets/fonts` beside it gets here.
    fn build(&mut self, ctx: &mut SceneContext) {
        let mut provider = match FontProvider::with_bundled() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("text: no font at {}: {e}", rustim_text::BUNDLED_DIR);
                return;
            }
        };
        for line in &TEXT_LINES {
            let style = TextStyle {
                size: line.size,
                ..TextStyle::default()
            };
            // 1.0: the run is drawn *under* the viewport transform, which is
            // where its world size comes from. See `rustim_app::text`.
            let shaped = provider.shape(line.text, &style, 1.0, line.max_width);
            // A glyph the font does not have is reported by the frame loop, once
            // per distinct complaint — the run still draws, with a hole where the
            // glyph would have been (`rustim-text`'s docs).
            let mut m = Mobject::new(Vec::new())
                .with_style(filled(line.color, 1.0))
                .with_kind(NodeKind::Text)
                .with_label(line.text);
            if let Some(bounds) = rustim_app::text::local_bounds(&shaped) {
                m = m.with_local_bounds(bounds);
            }
            m.opacity = line.opacity;
            m.transform = Affine2::rotate(line.rotate) * Affine2::translate(line.at);
            self.nodes.push(ctx.add(m));
            self.blocks.push(shaped);
        }
    }
}

impl Scene for TextScene {
    fn construct(&mut self, ctx: &mut SceneContext) {
        self.build(ctx);
    }

    fn build_draw_list(&self, ctx: &SceneContext, list: &mut DrawList) {
        // Through the context first, for the background and the viewport
        // transform — and because the ordering is the point: the tree walk runs
        // here and the glyphs go over whatever it drew.
        ctx.build_draw_list(list);
        for (id, shaped) in self.nodes.iter().zip(&self.blocks) {
            let Some(m) = ctx.tree.get(*id) else { continue };
            let world = ctx.tree.parent_transform(*id) * m.transform;
            // Folded exactly as the tree walk folds it, so that what is drawn and
            // what the accessibility layer describes cannot disagree: the arrow
            // keys change a text object's opacity and the picture follows.
            let mut paint = m.style.fill;
            paint.components[3] = (m.opacity * m.style.fill_opacity) as f32;
            rustim_app::text::push_text(list, shaped, world, ctx.affine(), paint);
        }
    }
}

// ---------------------------------------------------------------------------
// The formula scene: Typst, all the way to per-glyph paths
// ---------------------------------------------------------------------------

/// Formulas, typeset by Typst and drawn as one mobject per glyph.
///
/// Nothing here overrides `build_draw_list`: a formula's glyphs are ordinary
/// filled point arrays, so the *default* tree walk draws them, and that is the
/// whole point of the representation. `TextScene` above needs an override
/// because a shaped run is not points; this scene needs none.
///
/// The five lines are chosen to disagree with each other, and each one covers a
/// path that the others do not:
///
/// - a plain Pythagorean identity, which is the scene's spine and the one a bare
///   `$x^2 + y^2 = c^2$` is asked for;
/// - a fraction and a radical, which are `FrameItem::Shape` items — *stroked*
///   lines, expanded into filled quads — so without them the golden image would
///   never touch that code and a missing fraction bar would ship;
/// - a subscripted sum, which is the only line whose glyphs sit at two different
///   baselines inside one group transform, so it is what catches a composition
///   error that a body without scripts cannot show;
/// - Chinese inside math, which selects the bundled face: Typst has no CJK of
///   its own, so this line fails visibly if `BUNDLED_DIR` is not in the world;
/// - `d` and `o` in a word, which are the letters with counters — the case that
///   proves the two-contour welding in `shapes::points_from_path` keeps the hole
///   open rather than filling it in.
struct FormulaScene {
    /// The ids of the formula roots, in draw order.
    nodes: Vec<NodeId>,
}

/// One line of [`FormulaScene`]. The body is **Typst math, not LaTeX**.
struct FormulaLine {
    body: &'static str,
    label: &'static str,
    color: rustim_color::Srgb,
    at: (f64, f64),
    /// A uniform scale on the root's deferred transform, so one image shows the
    /// size following a transform rather than a `font_size`.
    scale: f64,
}

const FORMULA_LINES: [FormulaLine; 5] = [
    FormulaLine {
        body: "a^2 + b^2 = c^2",
        label: "勾股定理",
        color: rustim_color::FG,
        at: (0.0, 2.4),
        scale: 1.0,
    },
    FormulaLine {
        body: "frac(-b + sqrt(b^2 - 4a c), 2a)",
        label: "求根公式",
        color: rustim_color::BLUE,
        at: (0.0, 0.7),
        scale: 0.85,
    },
    FormulaLine {
        body: "sum_(i=1)^n i = frac(n(n+1), 2)",
        label: "等差数列求和",
        color: rustim_color::YELLOW,
        at: (0.0, -1.0),
        scale: 0.85,
    },
    FormulaLine {
        body: "d o g = \"狗\"",
        label: "中英混排",
        color: rustim_color::RED,
        at: (0.0, -2.4),
        scale: 0.9,
    },
    FormulaLine {
        body: "e^(i pi) + 1 = 0",
        label: "欧拉恒等式",
        color: rustim_color::FG,
        at: (3.9, -3.0),
        scale: 0.5,
    },
];

impl FormulaScene {
    fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    /// Compile every line and insert it.
    ///
    /// Built once, in `construct`, because a `FormulaCompiler` parses the whole
    /// embedded Typst font set plus the bundled CJK face — the expensive half —
    /// and then reuses it. A failure is reported and the line skipped, the same
    /// rule `TextScene` follows for a missing font: this path *is* reachable, a
    /// model can write a formula Typst will not parse, and a scene that panicked
    /// on one would take the window down with it.
    fn build(&mut self, ctx: &mut SceneContext) {
        let mut compiler = match rustim_text::formula::FormulaCompiler::new() {
            Ok(compiler) => compiler,
            Err(e) => {
                eprintln!("formula: {e}");
                return;
            }
        };
        for line in &FORMULA_LINES {
            let formula = match compiler.compile(line.body) {
                Ok(formula) => formula,
                Err(e) => {
                    eprintln!("formula {:?}: {e}", line.body);
                    continue;
                }
            };
            let ids = formula.insert(&mut ctx.tree);
            // The glyphs and the rules hang off the root, so colour them where
            // they are and move the root — one transform for the whole formula,
            // which is what the deferred-transform story is for.
            for id in ids.glyphs.iter().chain(ids.rules.iter()).copied() {
                if let Some(m) = ctx.tree.get_mut(id) {
                    m.style = filled(line.color, 1.0);
                }
            }
            if let Some(root) = ctx.tree.get_mut(ids.root) {
                root.label = Some(line.label.to_owned());
                // Scale first, then place: `*` applies its right operand first, so
                // the other order would scale the position along with the glyphs.
                root.transform = Affine2::translate(line.at) * Affine2::scale(line.scale);
            }
            self.nodes.push(ids.root);
        }
    }
}

impl Scene for FormulaScene {
    fn construct(&mut self, ctx: &mut SceneContext) {
        self.build(ctx);
    }
}

// ---------------------------------------------------------------------------
// The axes scene: a coordinate system, a sampled curve, a number line
// ---------------------------------------------------------------------------

/// Where the coordinate system sits, in world units.
///
/// Slightly above the origin so the number line underneath clears it, and so the
/// camera beat below has something to pan across rather than only to zoom on.
const AXES_AT: (f64, f64) = (0.0, 0.3);

/// The data ranges the axes mark, and the geometric lengths they occupy.
///
/// Data units and world units differ — 8 data units across 7 world units — which
/// is the point of drawing this: a tick's position comes from the mapping, not
/// from the number, so a scene that confused the two would put a tick labelled 4
/// somewhere other than the axis' end.
const X_RANGE: (f64, f64) = (-4.0, 4.0);
const Y_RANGE: (f64, f64) = (-2.0, 2.0);
const X_LENGTH: f64 = 7.0;
const Y_LENGTH: f64 = 4.0;

/// A coordinate system, a function graph and a number line — plus a camera beat.
///
/// The three new object kinds disagree with each other on purpose:
///
/// - `axes` is one welded path of shafts and ticks, so the golden shows whether
///   the weld leaves the quadrants empty and the axes solid;
/// - the plot is a **stroked centerline**, not a filled polyline: the
///   counterfactual is an area under `y = 0.25x² - 1.75`, which would be the
///   single most visible wrong picture in the suite — and this is the one golden
///   whose pixels go through the backend's stroker;
/// - the number line is the same weld with a different tick direction, and it is
///   the object `focus` and the accessibility layer measure by diameter;
/// - the labels are shaped text drawn *through the camera*, so a camera that
///   moved the fills and not the glyphs would show up here and nowhere else.
///
/// The parabola is a quadratic rather than a sine on purpose: `sin` comes from a
/// platform libm and can move by an ULP between targets, which would make the
/// checked-in baseline unreproducible on the platform that made it. `0.25x² - 1.75`
/// is multiplication and subtraction, which IEEE 754 rounds identically
/// everywhere.
///
/// The apex sits at `-1.75` rather than at the range's floor because the y axis
/// labels `-2` at `x ≈ -0.42`, which is inside the parabola's well: an apex at
/// `-2` put the curve through the label. Labels are drawn over the fills, so
/// that is a visible collision and not a z-order accident.
struct AxesScene {
    /// Tick labels, parallel to `nodes`, shaped once in `construct`.
    blocks: Vec<ShapedText>,
    nodes: Vec<NodeId>,
}

impl AxesScene {
    fn new() -> Self {
        Self {
            blocks: Vec::new(),
            nodes: Vec::new(),
        }
    }

    fn build(&mut self, ctx: &mut SceneContext) {
        use rustim_mobject::shapes::{AxesScale, ShapeKind, ShapeParams, build};

        let scale = AxesScale::new(X_RANGE, X_LENGTH, Y_RANGE, Y_LENGTH);

        let axes = build(
            ShapeKind::Axes,
            &ShapeParams {
                width: Some(X_LENGTH),
                height: Some(Y_LENGTH),
                x_range: Some([X_RANGE.0, X_RANGE.1]),
                y_range: Some([Y_RANGE.0, Y_RANGE.1]),
                step: Some(1.0),
                tick_size: Some(0.15),
                position: Some([AXES_AT.0, AXES_AT.1]),
                ..Default::default()
            },
        )
        .with_style(filled(rustim_color::FG, 1.0));
        ctx.add(axes);

        // `y = 0.25x² - 1.75`, sampled in data coordinates and handed over in world
        // coordinates — the same mapping the axes' ticks used, which is why it is
        // `AxesScale` and not two copies of the formula. Samples above the top of
        // `y_range` are dropped: a graph that pokes out of its coordinate system
        // reads as a layout bug, and the endpoints then land on the frame.
        let samples: Vec<[f64; 2]> = (0..=80)
            .filter_map(|i| {
                let x = f64::from(i) * 0.1 - 4.0;
                let y = 0.25 * x * x - 1.75;
                (y <= Y_RANGE.1).then(|| {
                    let p = scale.to_world(x, y);
                    [p.x + AXES_AT.0, p.y + AXES_AT.1]
                })
            })
            .collect();
        // The width lives in the style now: the plot is a stroked centerline,
        // so the backend's stroker applies `stroke_width` to it. That is what
        // makes the curve's thickness an animatable property rather than a
        // baked-in polygon.
        let curve = build(
            ShapeKind::Plot,
            &ShapeParams {
                points: Some(samples),
                ..Default::default()
            },
        )
        .with_style(stroked(rustim_color::YELLOW, 0.05));
        ctx.add(curve);

        let number_line = build(
            ShapeKind::NumberLine,
            &ShapeParams {
                width: Some(6.0),
                range: Some([-3.0, 3.0]),
                step: Some(1.0),
                tick_size: Some(0.12),
                position: Some([0.0, -2.15]),
                ..Default::default()
            },
        )
        .with_style(filled(rustim_color::BLUE, 1.0));
        ctx.add(number_line);

        self.labels(ctx, &scale);

        // The camera beat. It is a beat like any other — queued behind nothing,
        // played by the same scheduler — and the still is ten seconds in, so the
        // screenshot *is* the destination. That is what makes one image prove
        // the camera reaches fills, glyphs and the accessibility bounds.
        ctx.play(rustim_anim::CameraMove::new(Camera::new((0.15, 0.2), 1.25)).with_duration(1.5));
    }

    /// Integer labels for both axes, excluding the origin.
    ///
    /// The origin is skipped because the two axes cross there: a "0" would sit on
    /// top of the intersection and read as a smudge in the image, and it is the
    /// one value both axes would label.
    fn labels(&mut self, ctx: &mut SceneContext, scale: &rustim_mobject::shapes::AxesScale) {
        let mut provider = match FontProvider::with_bundled() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("axes: no font at {}: {e}", rustim_text::BUNDLED_DIR);
                return;
            }
        };
        let style = TextStyle {
            size: 0.22,
            ..TextStyle::default()
        };
        let mut push = |ctx: &mut SceneContext, text: String, at: (f64, f64), this: &mut Self| {
            let shaped = provider.shape(&text, &style, 1.0, None);
            let mut m = Mobject::new(Vec::new())
                .with_style(filled(rustim_color::FG, 1.0))
                .with_kind(NodeKind::Text)
                .with_label(text);
            if let Some(bounds) = rustim_app::text::local_bounds(&shaped) {
                m = m.with_local_bounds(bounds);
            }
            m.transform = Affine2::translate(at);
            this.nodes.push(ctx.add(m));
            this.blocks.push(shaped);
        };

        for i in (X_RANGE.0 as i32)..=(X_RANGE.1 as i32) {
            if i == 0 {
                continue;
            }
            let x = scale.to_world(f64::from(i), 0.0).x + AXES_AT.0;
            push(ctx, format!("{i}"), (x, AXES_AT.1 - 0.38), self);
        }
        for i in (Y_RANGE.0 as i32)..=(Y_RANGE.1 as i32) {
            if i == 0 {
                continue;
            }
            let y = scale.to_world(0.0, f64::from(i)).y + AXES_AT.1;
            push(ctx, format!("{i}"), (AXES_AT.0 - 0.42, y), self);
        }
    }
}

impl Scene for AxesScene {
    fn construct(&mut self, ctx: &mut SceneContext) {
        self.build(ctx);
    }

    fn build_draw_list(&self, ctx: &SceneContext, list: &mut DrawList) {
        // The tree walk draws the axes, the curve and the number line; the labels
        // are glyph runs and go over them, exactly as `TextScene` does it.
        ctx.build_draw_list(list);
        for (id, shaped) in self.nodes.iter().zip(&self.blocks) {
            let Some(m) = ctx.tree.get(*id) else { continue };
            let world = ctx.tree.parent_transform(*id) * m.transform;
            let mut paint = m.style.fill;
            paint.components[3] = (m.opacity * m.style.fill_opacity) as f32;
            rustim_app::text::push_text(list, shaped, world, ctx.affine(), paint);
        }
    }
}

/// The `--scene` value used when the flag is absent.
const DEFAULT_SCENE: &str = "pythagorean";

/// What a `--script` with no `--scene` runs on top of.
///
/// **Deliberately not in the `--scene` registry**, and that is not a tidiness
/// point: `--help` is what the golden suite reads to decide which scenes it owes
/// a baseline on, so registering this one would demand a `blank` baseline on
/// three platforms — a picture of an empty frame — and it would add an entry to
/// a list a user reads to find out what the engine can draw.
///
/// What it is for: `--script` alone should start from nothing and build the
/// picture out of commands, because that is what a host driving the engine
/// actually does. Starting from `pythagorean` instead would put a scene on
/// screen that the host never asked for and cannot remove by name.
struct BlankScene;

impl Scene for BlankScene {
    fn construct(&mut self, _ctx: &mut SceneContext) {}
}

/// Every scene the binary can render, by the name `--scene` takes.
///
/// This exists for the golden-image suite: a test that can only render one
/// hardcoded scene proves very little, and the baselines have to be regenerated
/// per scene anyway. `pythagorean` keeps its name so the `p0-spikes/`
/// comparisons against Manim still line up.
fn scene_by_name(name: &str) -> Option<Box<dyn Scene>> {
    match name {
        "pythagorean" => Some(Box::new(rustim_scenes::Pythagorean)),
        "primitives" => Some(Box::new(PrimitivesScene)),
        "composite" => Some(Box::new(CompositeScene)),
        "text" => Some(Box::new(TextScene::new())),
        "formula" => Some(Box::new(FormulaScene::new())),
        "transform" => Some(Box::new(TransformScene::new())),
        "rotate" => Some(Box::new(RotateScene)),
        "axes" => Some(Box::new(AxesScene::new())),
        _ => None,
    }
}

/// The scene names `--scene` accepts, for `--help` and for the error path.
///
/// **This array and the match above are a matched pair** — see `CLAUDE.md`. The
/// golden suite reads `--help` to decide which scenes it owes a baseline, so a
/// scene in the match but not here is *not* offered by `--help`, and the suite's
/// completeness check cannot see it: it compiles, it runs, and it silently skips
/// its baseline forever.
fn scene_names() -> String {
    [
        "pythagorean",
        "primitives",
        "composite",
        "text",
        "formula",
        "transform",
        "rotate",
        "axes",
    ]
    .join(", ")
}

// ---------------------------------------------------------------------------
// Presentation
// ---------------------------------------------------------------------------

/// How long the headless `--png` path runs the scene before capturing.
///
/// Not "however many frames a scene happens to need": that would make the still
/// a function of animation durations, and a duration change would silently turn
/// a golden baseline into a capture of a half-faded frame. Running past the end
/// is free, because a settled scene produces the same frame forever, so this is
/// simply "the scene after ten seconds of virtual time" — deterministic, and
/// long enough that no scene in this binary is still moving.
const STILL_SECONDS: f64 = 10.0;

/// Present a premultiplied RGBA8 buffer as softbuffer's `0x00RRGGBB` words.
///
/// The scene fills an opaque background first, so every pixel has `a == 255` and
/// premultiplied equals straight — no un-premultiplying needed on this path. A
/// scene that ever draws onto a transparent background would need it.
fn blit(src: &[u8], dst: &mut [u32]) {
    for (d, px) in dst.iter_mut().zip(src.as_chunks::<4>().0) {
        *d = u32::from(px[0]) << 16 | u32::from(px[1]) << 8 | u32::from(px[2]);
    }
}

/// Present the part of the frame that changed.
///
/// `dst` is softbuffer's buffer and `src` the renderer's, and their widths agree
/// because `Resized` resizes both. `Damage::None` is the case that matters most:
/// an idle scene presents nothing at all, so the window keeps the last image.
fn blit_damage(src: &[u8], dst: &mut [u32], frame_width: u16, damage: Damage) {
    let Damage::Region(r) = damage else {
        if damage == Damage::Full {
            blit(src, dst);
        }
        return;
    };
    let stride = usize::from(frame_width);
    if stride == 0 {
        return;
    }
    let rows = dst.len() / stride;
    let x1 = usize::from(r.x1).min(stride);
    let x0 = usize::from(r.x0).min(x1);
    let y1 = usize::from(r.y1).min(rows);
    let y0 = usize::from(r.y0).min(y1);
    for y in y0..y1 {
        for x in x0..x1 {
            let s = (y * stride + x) * 4;
            let Some(px) = src.get(s..s + 4) else {
                continue;
            };
            dst[y * stride + x] = u32::from(px[0]) << 16 | u32::from(px[1]) << 8 | u32::from(px[2]);
        }
    }
}

/// The context every path of this binary runs on: the scene's own state plus
/// the desktop's typesetter.
///
/// One constructor rather than four `SceneContext::new` calls with a
/// `set_typesetter` after each, because the four paths (`--inspect`, `--png`,
/// the export loop and the window) must advertise the *same* capabilities — the
/// handshake is a fact about the build, and a path that forgot the provider would
/// answer `hello` with a narrower vocabulary than the next one. The provider is
/// lazy (see [`rustim_app::typeset`]), so a text-free scene pays nothing.
fn context(viewport: Viewport, motion: Motion) -> SceneContext {
    let mut ctx = SceneContext::new(viewport, motion);
    ctx.set_typesetter(Box::new(rustim_app::typeset::AppTypesetter::new()));
    ctx
}

fn build_frame(scene: &mut dyn Scene, ctx: &mut SceneContext, list: &mut DrawList, dt: f64) {
    scene.update(dt, ctx);
    // Through the scene, not straight through the context: a scene may have
    // draw operations of its own to add.
    scene.build_draw_list(ctx, list);
}

/// Run a scene for [`STILL_SECONDS`] of virtual time, at a fixed timestep.
///
/// Shared by `--png` and `--inspect` so that the frame a still shows and the
/// tree an inspector prints describe the *same* moment. Two copies of this loop
/// would be two definitions of "settled", and the first change to one of them
/// would make the two flags disagree about the thing they exist to check.
///
/// Fixed virtual time, never wall-clock: an output that depended on how fast the
/// machine is could not be compared to anything.
fn settle(scene: &mut dyn Scene, ctx: &mut SceneContext, list: &mut DrawList) {
    for _ in 0..(STILL_SECONDS / STEP) as u32 {
        build_frame(scene, ctx, list, STEP);
    }
}

/// Render a timeline into a sink and return the narration track it timed.
///
/// One loop, shared by both video modes, so an `.mp4` and a PNG sequence are
/// the *same frames* — the sink is the only difference, which is what the
/// `VideoSink` seam is for.
///
/// **Every frame is a `build_frame` of one [`STEP`], exactly what `settle`
/// does**, so a video of `STILL_SECONDS` worth of frames ends on the frame the
/// `--png` still captures: the last frame of the sequence and the still are the
/// same image, not two renders that agree. Nothing reads the wall clock — the
/// subtitle cues are timed off the context's virtual clock, and a frame's
/// caption expires by how far the timeline has run, not by how long the machine
/// took to draw it.
///
/// The caption band is drawn **after** the scene's own list, at the same place
/// the windowed path appends it, so an exported frame and a displayed one agree.
fn export_timeline(
    scene: &mut dyn Scene,
    ctx: &mut SceneContext,
    frames: u64,
    sink: &mut dyn rustim_export::VideoSink,
    mut caption: Option<&mut rustim_app::caption::Caption>,
    include_summary: bool,
    scene_name: &str,
) -> std::io::Result<Vec<rustim_export::Cue>> {
    let mut list = DrawList::default();
    let mut renderer = CpuRenderer::new(WIDTH, HEIGHT);
    let mut cues: Vec<rustim_export::Cue> = Vec::new();
    // The line currently on screen and when it became due. One slot, like the
    // live region: a caption shows one sentence.
    let mut open: Option<(f64, String)> = None;
    let mut summary_pending = include_summary;

    let total = frames as f64 * STEP;
    for frame in 0..frames {
        build_frame(scene, ctx, &mut list, STEP);
        let now = ctx.clock();

        let mut due = ctx.take_narration_timed();
        // The opening summary arrives through the same channel as a scripted
        // line, on the first frame, and only when nothing was already due —
        // the windowed pipeline's rule (`summary_pending`), kept identical so a
        // video and a window describe the same scene.
        if frame == 0 && std::mem::take(&mut summary_pending) && due.is_empty() {
            let objects = ctx.semantic_tree(None).nodes.len().saturating_sub(1);
            due.push((now, rustim_app::caption::scene_summary(scene_name, objects)));
        }
        for (at, text) in due {
            // A new line replaces whatever is on screen, so it ends that cue at
            // the same instant the band changes text.
            if let Some((start, words)) = open.take() {
                let end = (start + rustim_app::caption::hold_seconds(&words)).min(at);
                if end > start {
                    cues.push(rustim_export::Cue {
                        start,
                        end,
                        text: words,
                    });
                }
            }
            open = Some((at, text));
        }
        // Expiry, on virtual time. `now` is at most one frame past the expiry,
        // so the cue ends where the band would have vanished rather than on the
        // frame boundary the detector happened to land on.
        if let Some((start, words)) = &open {
            let expiry = start + rustim_app::caption::hold_seconds(words);
            if now >= expiry {
                let (start, words) = open.take().expect("just matched `Some`");
                if expiry > start {
                    cues.push(rustim_export::Cue {
                        start,
                        end: expiry,
                        text: words,
                    });
                }
            }
        }

        if let Some(caption) = caption.as_mut() {
            match &open {
                Some((_, text)) => caption.set_text(text.as_str()),
                None => caption.clear(),
            }
            caption.push_ops(&mut list);
        }
        renderer.render(&list).expect("render");
        sink.write_frame(renderer.pixels(), u32::from(WIDTH), u32::from(HEIGHT))?;
    }

    // The last line is still on screen when the timeline ends; its cue stops
    // with the frame rather than running past the end of the file.
    if let Some((start, words)) = open {
        let end = total.min(start + rustim_app::caption::hold_seconds(&words));
        if end > start {
            cues.push(rustim_export::Cue {
                start,
                end,
                text: words,
            });
        }
    }
    Ok(cues)
}

/// Print a human-facing line on stdout — or on stderr when stdout is a protocol
/// stream.
///
/// **`--reply` makes stdout the reply channel and nothing else.** A host parsing
/// the stream cannot skip a line it did not expect, and `wrote out.png (1280x720)`
/// in the middle of it is a parse error whose *presence* depends on which other
/// flags were passed — the worst kind of protocol bug, because it hides until
/// someone adds a flag. Nothing is lost by moving these: the note is for a human,
/// and a human reads the terminal either way.
fn note(reply: bool, message: std::fmt::Arguments<'_>) {
    if reply {
        eprintln!("{message}");
    } else {
        println!("{message}");
    }
}

/// Apply a whole script to a scene, and answer each line if `--reply` asked.
///
/// Used by both headless paths and by a windowed run fed from a *file*; a
/// windowed run fed from **stdin** cannot use it, because a file is read whole
/// here and stdin has to be read on its own thread — see `feed::spawn_stdin`.
///
/// **Every line lands at virtual time zero**, which is the property the replay
/// guarantee rests on: a `play` in a script queues a beat, the next line queues
/// the next, and the still is taken after ten seconds of virtual time whichever
/// way the script was fed. Reading a file is not a scheduling decision, so it
/// must not be one.
///
/// A script the run cannot read at all is an `exit(2)` — the one failure here
/// that is not a per-line refusal, because there is no line to refuse. Every
/// *line* failure stays inside [`feed::apply_all`] and is counted.
fn apply_script(path: &str, scene: &mut dyn Scene, ctx: &mut SceneContext, reply: bool) {
    let lines = feed::lines_from(path).unwrap_or_else(|e| {
        eprintln!("--script {path}: {e}");
        std::process::exit(2);
    });
    let mut out = reply.then(|| std::io::stdout().lock());
    let counts = feed::apply_all(
        scene,
        ctx,
        lines,
        out.as_mut().map(|w| w as &mut dyn std::io::Write),
    );
    if let Some(line) = counts.report() {
        eprintln!("{line}");
    }
}

/// Everything that exists only once there is a window.
///
/// A field rather than locals because the AccessKit adapter must be created
/// *before* the window is shown and cannot be created without an
/// `&ActiveEventLoop`, which only exists inside `resumed`. So the window is
/// created there too — invisible, then the adapter, then visible. The comment
/// this replaced argued that moving window creation into `resumed` "would mean
/// threading an `Option<Surface>` through the whole app for no behavioural
/// gain"; the gain is that the accessibility layer can exist at all.
struct Windowed {
    window: Rc<Window>,
    /// The display connection the surface was created from, held for its drop
    /// side effect. `Surface` carries no lifetime and does not borrow it — which
    /// is why the leading underscore is needed to say "kept, not read" — but it
    /// is the connection, and the previous version kept it alive as a local for
    /// the same reason.
    _context: softbuffer::Context<Rc<Window>>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    a11y: A11y,
}

struct App {
    windowed: Option<Windowed>,
    /// Held rather than fetched on demand: `EventLoop::create_proxy` is the only
    /// way to get one, and the adapter needs it at window-creation time.
    proxy: EventLoopProxy<AppEvent>,
    title: String,
    renderer: CpuRenderer,
    list: DrawList,
    /// The frame currently in the renderer's pixel buffer, for
    /// [`DrawList::damage_since`]. Swapped with `list` after a frame is
    /// presented, so the buffer being rebuilt and the frame it is compared
    /// against ping-pong rather than allocate.
    prev: DrawList,
    /// What the last frame actually rasterised, for the frame-rate line. A
    /// region render is the claim that this number falls; the log is the only
    /// place that claim is visible while the engine runs.
    stats: RenderStats,
    scene: Box<dyn Scene>,
    ctx: SceneContext,
    /// Commands produced by the keyboard or by assistive technology, applied at
    /// the top of the next frame. They take the same path an AI's JSON command
    /// takes, which is the point: one validator, so input cannot reach a state
    /// the command layer would refuse.
    queued: Vec<Command>,
    /// Lines of a stdin command feed, unread until the top of the next frame.
    ///
    /// Unparsed on purpose: the reader thread cannot reply (it is not the thread
    /// that owns the tree) and must not panic (it has no way to report anything),
    /// so a malformed line stays a line until [`feed::apply_all`] can turn it
    /// into a reply.
    pending: Vec<(u64, String)>,
    /// Whether each command's reply is printed to stdout. Off unless `--reply`.
    reply: bool,
    /// The bottom caption band, when `--captions` asked for one.
    ///
    /// `None` is the default and the normal path: with no band there is not one
    /// extra draw op, which is what keeps every golden baseline byte-identical.
    caption: Option<rustim_app::caption::Caption>,
    /// The self-voicing backend, when `--speak` found one.
    ///
    /// `None` also covers `--speak` on a machine with no synthesiser: the flag
    /// asks, the machine answers, and the run continues with captions alone.
    speaker: Option<rustim_app::speech::Speaker>,
    /// The canonical scene name, for the opening summary.
    scene_name: String,
    /// Whether the opening summary is still owed.
    ///
    /// Owed on the **first frame**, after its accessibility update, not during
    /// `resumed`: the live region has one slot and `was_animating` starts true,
    /// so a static scene offers `animation ended` on frame one and would replace
    /// a summary announced earlier. Injected on the frame's narration drain
    /// instead, which is after that update has run.
    summary_pending: bool,
    /// The live region's text as of the frame currently on screen.
    ///
    /// Compared against the text after `a11y.update` so a caption is set once per
    /// change rather than every frame — `set_text` re-shapes on a new string, and
    /// sixty identical offers a second would be sixty layout passes.
    last_live: String,
    /// Whether this run is an `--a11y-probe`: offer one announcement at startup
    /// and print what the accessibility layer did when the loop ends. Windowed
    /// only, enforced in `main`.
    probe: bool,
    /// What the feed has done so far, for the closing line.
    feed: feed::Counts,
    /// The window's current modifier state. winit delivers modifiers in their
    /// own event, so this is tracked across events rather than read off the
    /// keypress — a Shift+Tab that read `event` alone would look like a Tab.
    mods: ModifiersState,
    /// The pointer's gesture state: last cursor position, drag state, and the
    /// camera the current gesture has built up.
    ///
    /// `MouseWheel` and `MouseInput` carry no position, so the anchor for
    /// "zoom about the cursor" is the last `CursorMoved`, which this keeps. The
    /// arithmetic is shared with the browser host in `rustim_math::gesture`.
    pointer: input::Pointer,
    /// Whether the animation clock runs. Space toggles it.
    paused: bool,
    /// Frames owed to `,` and `.`, signed. Clamped so that holding the key does
    /// not bank a hundred frames of catch-up.
    step: i32,
    /// Whether an animation was running last frame, so that *ending* can be
    /// announced exactly once rather than every frame of the transition.
    was_animating: bool,
    /// The last text-rendering complaint reported to the user.
    ///
    /// A missing glyph is a property of the *scene*, not of the frame, so it
    /// repeats identically sixty times a second; printing it each time would bury
    /// the frame-rate line and every other message in the log. Remembering the
    /// text and reporting only when it changes says the same thing once.
    text_error: Option<String>,
    frame: u64,
    /// Frames that actually reached the window — the number a `frame_ready`
    /// reports. Distinct from [`Self::frame`], which counts loop iterations:
    /// an idle scene iterates without presenting, so the two diverge exactly
    /// where the event would otherwise claim a picture that does not exist.
    presented: u64,
    max_frames: Option<u64>,
    render_ms: Vec<f64>,
    last: Instant,
}

impl App {
    fn request_redraw(&self) {
        if let Some(w) = &self.windowed {
            w.window.request_redraw();
        }
    }

    /// Adopt a pointer gesture's camera.
    ///
    /// Both gestures end here, and they end at the **command layer**, not by
    /// writing `tree.camera` directly: a pointer is one more host of the same
    /// protocol an AI and a script use, so a zoom the command validator would
    /// refuse cannot arrive through the mouse instead. The command has no
    /// `duration`, which is what makes it an instant write — the picture
    /// follows the hand rather than animating behind it.
    fn queue_camera(&mut self, camera: Camera) {
        self.queued.push(Command::Camera {
            center: Some([camera.center.0, camera.center.1]),
            zoom: Some(camera.zoom),
            focus: None,
            duration: None,
        });
    }

    /// Publish this session's events, and this frame's `frame_ready` if there
    /// was one.
    ///
    /// Called once per loop iteration after the frame counter has moved, so the
    /// two describe the same frame: the events are what the update in that frame
    /// did, and `frame_ready` says the picture carrying it reached the window.
    /// `drew` is false when the frame changed nothing on screen (`Damage::None`)
    /// or could not be rasterised — those iterations still produce timeline
    /// events, but nothing a subscriber could look at, and reporting a frame
    /// every tick would make the event a heartbeat rather than a notification.
    /// The kernel records events whether or not anyone listens; the queue is
    /// drained either way (see [`feed::publish_events`]), so a session that
    /// later subscribes starts from what happens next.
    ///
    /// A write failure is ignored. The windowed loop has no one to report a
    /// vanished host to — the stream is a courtesy to an attached client, and a
    /// broken pipe must not take the picture down.
    fn publish_events(&mut self, drew: bool) {
        if !self.reply {
            // Still drain: the queue is bounded, and a `--frames` run with a
            // script must not accumulate events nobody asked for.
            let _ = self.ctx.take_events();
            return;
        }
        if drew {
            self.presented += 1;
        }
        let mut out = std::io::stdout().lock();
        let _ = feed::publish_events(&mut self.ctx, &mut out).and_then(|()| {
            if drew {
                feed::publish_frame(&mut self.ctx, self.presented, &mut out)
            } else {
                Ok(())
            }
        });
    }

    /// Log a change in what the text layer is complaining about, once.
    ///
    /// `None` means this frame had no complaint, which is worth one line only if
    /// the previous frame had one — a scene that loses and then regains a glyph
    /// should say both, and a scene that is fine should say nothing forever.
    fn report_text_error(&mut self, now: Option<String>) {
        if self.text_error == now {
            return;
        }
        match &now {
            Some(text) => eprintln!("text: {text}"),
            None => eprintln!("text: every glyph in the frame was drawn"),
        }
        self.text_error = now;
    }

    /// Virtual time for this frame.
    ///
    /// Three cases in priority order: a requested step (which implies pausing,
    /// because stepping while the clock runs would fight the animation), paused,
    /// and running on the wall clock. Wall-clock time is clamped so that a stall
    /// — a debugger, a suspended laptop — does not teleport every animation to
    /// its end state in one frame.
    fn frame_dt(&mut self, wall: f64) -> f64 {
        if self.step != 0 {
            self.paused = true;
            let direction = self.step.signum();
            self.step -= direction;
            return f64::from(direction) * STEP;
        }
        if self.paused {
            return 0.0;
        }
        wall.clamp(0.0, 0.1)
    }

    /// Act on a key press.
    ///
    /// Where the focus and the throttle live, this is the only caller of the
    /// accessibility core that is not an assistive technology — so a Tab and an
    /// AT's "move to next item" do the same thing by construction, not by
    /// coincidence.
    fn handle_intent(&mut self, intent: Intent) {
        let ctx = &self.ctx;
        match intent {
            Intent::FocusNext | Intent::FocusPrev => {
                let by = if intent == Intent::FocusNext { 1 } else { -1 };
                if let Some(w) = self.windowed.as_mut() {
                    w.a11y.core().focus_step(by, ctx);
                }
            }
            Intent::Adjust(delta) => {
                if let Some(w) = self.windowed.as_mut()
                    && let Some(cmd) = w.a11y.core().adjust(delta, ctx)
                {
                    self.queued.push(cmd);
                }
            }
            Intent::AnnounceFocus => {
                if let Some(w) = self.windowed.as_mut() {
                    w.a11y.core().announce_focus(ctx);
                }
            }
            Intent::Escape => {
                if let Some(w) = self.windowed.as_mut() {
                    w.a11y.core().focus_clear();
                }
            }
            Intent::TogglePlayback => self.paused = !self.paused,
            Intent::ToggleMotion => {
                let mode = if ctx.motion.is_reduced() {
                    MotionMode::Full
                } else {
                    MotionMode::Reduced
                };
                self.queued.push(Command::Motion { mode });
                // Only the keyboard announces it. A command from a host is
                // already known to the host, and repeating it into the live
                // region would make an AT-driven switch speak twice — the same
                // rule focus announcements follow.
                if let Some(w) = self.windowed.as_mut() {
                    w.a11y
                        .core()
                        .offer(rustim_app::a11y::motion_announcement(mode));
                }
            }
            Intent::StepFrame(n) => {
                self.paused = true;
                self.step = (self.step + i32::from(n)).clamp(-60, 60);
            }
        }
    }

    /// Make the surface, the renderer and the viewport agree on one frame size.
    ///
    /// `width`/`height` are **physical** pixels — what `Window::inner_size` and
    /// `WindowEvent::Resized` both report. The logical constants the window is
    /// *asked* for are not a frame size: on a Retina display they differ by the
    /// scale factor, so seeding from them leaves the first frame's surface at half
    /// the backing store and the world-unit scale wrong until `Resized` arrives.
    /// Both call sites therefore go through here, so there is one answer to "how
    /// big is the frame" rather than two that can drift.
    fn apply_frame_size(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if let Some(w) = self.windowed.as_mut() {
            w.surface
                .resize(
                    NonZeroU32::new(width).unwrap(),
                    NonZeroU32::new(height).unwrap(),
                )
                .expect("resize surface");
        }
        // The renderer follows the surface, so the tutorial's hardcoded 1280x720
        // background stops being wrong the moment the window is resized. The
        // viewport follows with it, and the accessibility tree's bounds are
        // derived from the viewport, so they follow too.
        self.renderer.resize(width as u16, height as u16);
        self.ctx.viewport.frame_width = width as u16;
        self.ctx.viewport.frame_height = height as u16;
    }
}

impl ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.windowed.is_some() {
            return;
        }
        let window = Rc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title(self.title.clone())
                        .with_inner_size(winit::dpi::LogicalSize::new(WIDTH, HEIGHT))
                        // Invisible for exactly as long as it takes to build the
                        // adapter: `accesskit_winit` panics if the window has
                        // already been shown.
                        .with_visible(false),
                )
                .expect("create window"),
        );
        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        let mut surface =
            softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
        // The window's real **physical** size, not the logical constants asked for
        // above: on a Retina display those differ by the scale factor. Sized before
        // `set_visible` so the window is never exposed with a surface that does not
        // match its backing store, since the OS can send `RedrawRequested` on
        // exposure without waiting for our `request_redraw`.
        let size = window.inner_size();
        surface
            .resize(
                NonZeroU32::new(size.width.max(1)).unwrap(),
                NonZeroU32::new(size.height.max(1)).unwrap(),
            )
            .expect("resize");

        let a11y = A11y::new(event_loop, &window, self.proxy.clone(), self.title.clone());
        window.set_visible(true);

        self.last = Instant::now();
        self.windowed = Some(Windowed {
            window,
            _context: context,
            surface,
            a11y,
        });
        // The surface is right; the renderer and the viewport are still on the
        // logical constants. Bring them onto the same physical size now rather than
        // waiting for the `Resized` event, which is not guaranteed to arrive before
        // the first `RedrawRequested`.
        self.apply_frame_size(size.width, size.height);
        // Offered once, before the first frame, so the frame loop's own
        // `a11y.update` flushes it into the live region. That exercises the
        // announcement path end to end; the report's push counters are what
        // say whether a backend was there to receive it.
        if self.probe
            && let Some(w) = self.windowed.as_mut()
        {
            w.a11y.core().offer(rustim_app::a11y::PROBE_ANNOUNCEMENT);
        }
        self.request_redraw();
    }

    /// Accessibility events arrive here, not in `window_event`: the platform's
    /// backend calls in from its own thread and the winit adapter forwards
    /// through the event-loop proxy. Command lines from a stdin feed arrive the
    /// same way, from the reader thread `feed::spawn_stdin` starts.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: AppEvent) {
        // A `match` rather than the irrefutable `let` this used to be, which is
        // exactly what that `let` was written to force: the second variant could
        // not be added without coming here.
        match event {
            AppEvent::AccessKit(event) => match event.window_event {
                accesskit_winit::WindowEvent::InitialTreeRequested => {
                    // Routed through the ordinary frame path rather than answered
                    // here. The adapter was built with an event-loop proxy, so it
                    // cannot take a synchronous initial tree; the answer has to be
                    // the next frame's snapshot, and `a11y.update` runs before
                    // `request_redraw` in that frame, which is what guarantees the
                    // snapshot describes the state that is on screen.
                    //
                    // It also has to be a **full** snapshot: a consumer asking for
                    // an initial tree has just attached and holds nothing, and an
                    // incremental update describes a difference from a tree it
                    // does not have. The request is drained before the redraw it
                    // triggers, so the invalidation cannot miss the frame.
                    if let Some(w) = self.windowed.as_mut() {
                        w.a11y.invalidate();
                    }
                    self.request_redraw();
                }
                accesskit_winit::WindowEvent::ActionRequested(req) => {
                    let request = self
                        .windowed
                        .as_mut()
                        .and_then(|w| w.a11y.core().on_action(&req, &self.ctx));
                    match request {
                        Some(Request::Command(cmd)) => {
                            self.queued.push(*cmd);
                            self.request_redraw();
                        }
                        Some(Request::TogglePlayback) => {
                            self.paused = !self.paused;
                            self.request_redraw();
                        }
                        None => {}
                    }
                }
                // The platform is dropping our tree, so whatever was sent is no
                // longer held by anyone. The next attachment starts from an
                // `InitialTreeRequested` (which invalidates too), but doing it
                // here as well is what makes the two events independent: neither
                // can leave an incremental update describing a tree the consumer
                // never received.
                accesskit_winit::WindowEvent::AccessibilityDeactivated => {
                    if let Some(w) = self.windowed.as_mut() {
                        w.a11y.invalidate();
                    }
                }
            },
            AppEvent::Command { seq, line } => {
                // Queued, not applied: this runs on the event loop's thread but
                // outside the frame, and applying here would mutate the tree
                // between a `build_draw_list` and the blit that consumes it. The
                // frame the line belongs to is the one it asks for.
                self.pending.push((seq, line));
                self.request_redraw();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // First, and unconditionally: the adapter's contract is that it sees
        // every window event before the application handles it, so this cannot
        // live in the match arms where a later edit could stop reaching it.
        if let Some(w) = self.windowed.as_mut() {
            w.a11y.process_event(&w.window, &event);
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(modifiers) => self.mods = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                if let Some(intent) =
                    input::classify(&input::KeyPress::from_event(&event, self.mods))
                {
                    self.handle_intent(intent);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(camera) = self.pointer.cursor_moved(
                    (position.x, position.y),
                    &self.ctx.viewport,
                    *self.ctx.camera(),
                ) {
                    self.queue_camera(camera);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    match state {
                        ElementState::Pressed => self.pointer.press(),
                        ElementState::Released => self.pointer.release(),
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(camera) = self.pointer.scroll(
                    input::wheel_delta(delta),
                    &self.ctx.viewport,
                    *self.ctx.camera(),
                ) {
                    self.queue_camera(camera);
                }
            }
            // The button-up can happen outside the window, where no
            // `MouseInput` follows. Dropping the drag here means coming back in
            // does not pan with a button nobody is holding.
            WindowEvent::CursorLeft { .. } | WindowEvent::Focused(false) => {
                self.pointer.release();
            }
            // The same three steps `resumed` seeds from, so a resize and a
            // startup cannot disagree about what the frame size is.
            WindowEvent::Resized(size) => self.apply_frame_size(size.width, size.height),
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let wall = (now - self.last).as_secs_f64();
                self.last = now;
                let dt = self.frame_dt(wall);

                // Before the frame, not after: the frame must show the result of
                // the key that caused it, and an animation applied at the end of
                // the frame would arrive one frame late.
                for cmd in std::mem::take(&mut self.queued) {
                    match self.ctx.handle(&cmd) {
                        // The hook a scene reacts to commands through. Until
                        // this line existed it was declared and never called,
                        // so a scene could not have reacted to anything even in
                        // principle. A query has nothing to react to; nothing on
                        // this path issues one today, but the match has to say
                        // so rather than treat a read as a mutation.
                        Ok(outcome) => {
                            if let Some(diff) = outcome.diff() {
                                self.scene.on_command(diff, &mut self.ctx);
                            }
                        }
                        // A refusal is a value, not a panic — the rule an AI's
                        // command follows, and this path is reachable by
                        // pressing a key or by an assistive technology naming a
                        // node that no longer exists.
                        Err(e) => eprintln!("command refused: {e}"),
                    }
                }
                // The gesture commands above have been applied (or refused);
                // either way the next gesture starts from the camera that is
                // now in the tree.
                self.pointer.end_frame();

                // The lines a feed delivered since the last frame. Through
                // `feed::apply_all` rather than `ctx.handle` in a loop, so the
                // windowed path and the headless one apply a script *identically*
                // — including calling `on_command` and answering each line.
                if !self.pending.is_empty() {
                    let mut out = self.reply.then(|| std::io::stdout().lock());
                    let counts = feed::apply_all(
                        &mut *self.scene,
                        &mut self.ctx,
                        std::mem::take(&mut self.pending),
                        out.as_mut().map(|w| w as &mut dyn std::io::Write),
                    );
                    self.feed.applied += counts.applied;
                    self.feed.refused += counts.refused;
                }

                // Narration due by the clock this frame's commands saw. Taken
                // after them, so an `at: 0` line in a stdin feed is due in the
                // same frame it arrived.
                let mut narration = self.ctx.take_narration();
                if std::mem::take(&mut self.summary_pending) && narration.is_empty() {
                    // A script that opens with narration is saying something
                    // more specific than "this is the scene"; do not talk over
                    // it. With no narration, the summary is the opening line.
                    let objects = self.ctx.semantic_tree(None).nodes.len().saturating_sub(1);
                    narration.push(rustim_app::caption::scene_summary(
                        &self.scene_name,
                        objects,
                    ));
                }
                if let Some(last) = narration.last()
                    && let Some(caption) = self.caption.as_mut()
                {
                    // Set before the list is built, so a due line is drawn in
                    // *this* frame rather than the next one. Speech happens
                    // after the update below, where the AT's presence is known
                    // for this frame rather than the previous one.
                    caption.set_text(last.clone());
                }

                let t0 = Instant::now();
                // `&mut *` rather than `&mut self.scene`: a `Box<dyn Scene>` does
                // not coerce to `&mut dyn Scene` in argument position without the
                // reborrow.
                if let Some(caption) = self.caption.as_mut() {
                    // Wall clock, not virtual: a paused scene still has to stop
                    // showing a line nobody is reading any more.
                    caption.expire(Instant::now());
                }
                build_frame(&mut *self.scene, &mut self.ctx, &mut self.list, dt);
                // The band goes after the scene's own ops, so it is a screen
                // layer over the picture rather than a world object in it. It is
                // also the tail of the list, which is what makes a caption change
                // damage only the bottom `GlyphRun`'s box.
                if let Some(caption) = self.caption.as_mut() {
                    caption.push_ops(&mut self.list);
                }
                // What changed since the frame in the pixel buffer. An idle
                // scene — paused, or a script that has finished — produces
                // `None`, and the frame loop then skips both the rasteriser and
                // the blit. A one-property change produces the union of the old
                // and new bounds, and only those pixels are recomputed.
                let damage = self.list.damage_since(&self.prev);
                // A frame that could not be rasterised must not be blitted — the
                // pixel buffer still holds the *previous* frame, so presenting it
                // would show a stale image as if it were current. A frame that
                // was rasterised but is missing a glyph is presented, because it
                // is a true picture of the scene minus one glyph, and the
                // complaint is reported instead.
                let rasterised = match self.renderer.render_damaged(&self.list, damage) {
                    Ok(stats) => {
                        self.stats = stats;
                        self.report_text_error(None);
                        true
                    }
                    Err(e @ RenderError::GlyphsSkipped { .. }) => {
                        self.report_text_error(Some(e.to_string()));
                        true
                    }
                    Err(e) => {
                        eprintln!("frame not rendered: {e}");
                        false
                    }
                };
                // What `frame_ready` reports: a frame that changed the window.
                // `Damage::None` presents nothing (see `blit_damage`), so a still
                // scene emits timeline events but no frame lines — otherwise a
                // subscriber would be told sixty times a second that the same
                // picture is ready. A failed rasterisation, or a surface that
                // could not be mapped, is not a picture either.
                let mut drew = false;
                if rasterised {
                    if let Some(w) = self.windowed.as_mut()
                        && let Ok(mut buf) = w.surface.buffer_mut()
                    {
                        blit_damage(self.renderer.pixels(), &mut buf, self.list.width, damage);
                        drew = damage != Damage::None && buf.present().is_ok();
                    }
                    // Only now is `prev` the frame that is on screen. An error
                    // leaves it alone, so the next attempt is compared against
                    // what the window still shows.
                    std::mem::swap(&mut self.list, &mut self.prev);
                }
                self.render_ms.push(t0.elapsed().as_secs_f64() * 1000.0);

                // An animation ending is the one change a blind user cannot poll
                // for: the scene is identical on either side of it.
                //
                // `is_playing`, not the scheduler: the scheduler is empty for every
                // frame a beat spends queued — including the frame right after
                // `construct`, and the frame between two beats — and each of those
                // would otherwise announce that the scene had finished before it
                // had started. See `SceneContext::is_playing`.
                let animating = self.ctx.is_playing();
                if self.was_animating
                    && !animating
                    && let Some(w) = self.windowed.as_mut()
                {
                    w.a11y.core().offer(ANIMATION_DONE);
                }
                self.was_animating = animating;

                // After the frame and before the redraw request, so that what a
                // reader is told and what is on screen describe the same state.
                if let Some(w) = self.windowed.as_mut() {
                    w.a11y.update(&self.ctx, Instant::now());
                    // Has anything ever listened? The push counters only move
                    // inside `update_if_active`, whose closure does not run while
                    // no assistive technology is attached. A screen reader that
                    // later attaches turns self-voicing off on its own, and
                    // detaching turns it back on — the counters do not reset.
                    let attached = w.a11y.full_pushes() + w.a11y.incremental_pushes() > 0;
                    // The caption shows what the live region is saying, which is
                    // the same sentence a screen reader would. Reading it *after*
                    // the update means the band is one frame behind the
                    // announcement; reading it before would show last frame's
                    // text twice.
                    let live = w.a11y.core().live_text().to_owned();
                    if live != self.last_live {
                        if let Some(caption) = self.caption.as_mut() {
                            caption.set_text(live.clone());
                        }
                        // Two voices saying the same sentence is worse than one,
                        // so the built-in synthesiser yields to a real reader.
                        if !live.is_empty()
                            && !attached
                            && let Some(speaker) = self.speaker.as_ref()
                        {
                            speaker.say(&live);
                        }
                        self.last_live = live;
                    }
                    // Narration is content, not an event: it bypasses the
                    // announcer (which drops anything less than 750 ms after the
                    // last line) and is said in full, in order. The live region
                    // can only hold one sentence, so a screen reader hears the
                    // last line of a burst; a same-frame burst is a script
                    // pacing problem, and `at` is the fix.
                    if let Some(last) = narration.last() {
                        if !attached && let Some(speaker) = self.speaker.as_ref() {
                            for line in &narration {
                                speaker.say(line);
                            }
                        }
                        // Written after the flush above, so the newest line is
                        // what an attached reader receives on the next frame.
                        w.a11y.core().narrate(last.clone());
                        // The live region now says this line. Recording it keeps
                        // the next frame from treating its own narration as a
                        // fresh change and speaking it a second time.
                        self.last_live = last.clone();
                        // The caption was set from the narration before the
                        // list was built; the live-change branch above may have
                        // put an announcement ("动画结束") in its place since.
                        // Put the narration back, so the word on screen matches
                        // the word spoken.
                        if let Some(caption) = self.caption.as_mut() {
                            caption.set_text(last.clone());
                        }
                    }
                }

                self.frame += 1;
                // After the counter and after the presentation above, so a
                // client reading `frame_ready` knows the picture exists rather
                // than that it was attempted.
                self.publish_events(drew);
                if self.frame.is_multiple_of(120) {
                    let recent = &self.render_ms[self.render_ms.len() - 120..];
                    let mean: f64 = recent.iter().sum::<f64>() / recent.len() as f64;
                    note(
                        self.reply,
                        format_args!(
                            "frame {:>5}  {mean:.2} ms/frame  ({:.0} fps)  {} ops  \
                             {} px repainted  focus {}",
                            self.frame,
                            1000.0 / mean.max(1e-9),
                            // `prev`, not `list`: the lists were swapped when
                            // the frame was presented, and the one on screen is
                            // the one this line describes.
                            self.prev.ops.len(),
                            self.stats.painted,
                            self.windowed
                                .as_ref()
                                .and_then(|w| w.a11y.focused())
                                .map_or_else(|| "—".to_string(), |id| id.raw().to_string()),
                        ),
                    );
                }
                if let Some(max) = self.max_frames
                    && self.frame >= max
                {
                    event_loop.exit();
                    return;
                }
                self.request_redraw();
            }
            _ => {}
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut max_frames: Option<u64> = None;
    let mut png: Option<String> = None;
    let mut video: Option<String> = None;
    let mut video_png: Option<String> = None;
    let mut srt: Option<String> = None;
    let mut inspect = false;
    let mut scene_name = DEFAULT_SCENE.to_string();
    let mut scene_given = false;
    let mut motion: Option<Motion> = None;
    let mut script: Option<String> = None;
    let mut reply = false;
    let mut probe = false;
    let mut captions = false;
    let mut speak = false;
    let mut voice: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--frames" => {
                max_frames = args.get(i + 1).and_then(|s| s.parse().ok());
                i += 2;
            }
            "--png" => {
                png = args.get(i + 1).cloned();
                i += 2;
            }
            // The three export flags take a value and say so when it is
            // missing: a bare `--video` that fell through to the windowed path
            // would look like a successful export that wrote nothing.
            "--video" | "--video-png" | "--srt" => {
                let Some(value) = args.get(i + 1).cloned() else {
                    eprintln!("{} takes a path", args[i]);
                    std::process::exit(2);
                };
                match args[i].as_str() {
                    "--video" => video = Some(value),
                    "--video-png" => video_png = Some(value),
                    _ => srt = Some(value),
                }
                i += 2;
            }
            "--inspect" => {
                inspect = true;
                i += 1;
            }
            "--motion" => {
                motion = match args.get(i + 1).map(String::as_str) {
                    Some("full") => Some(Motion::Full),
                    Some("reduced") => Some(Motion::Reduced),
                    // A refusal rather than a default, unlike `--frames` and `--png`
                    // above: a run that misspells the mode and silently gets the
                    // other one is a run that measured the wrong thing, and this is
                    // the one flag whose whole job is to be the right one.
                    other => {
                        eprintln!(
                            "--motion takes `full` or `reduced`, not {}",
                            other.map_or("nothing".to_string(), |v| format!("`{v}`"))
                        );
                        std::process::exit(2);
                    }
                };
                i += 2;
            }
            "--scene" => {
                if let Some(name) = args.get(i + 1) {
                    scene_name.clone_from(name);
                    scene_given = true;
                }
                i += 2;
            }
            "--script" => {
                if let Some(path) = args.get(i + 1) {
                    script = Some(path.clone());
                    i += 2;
                } else {
                    eprintln!("--script takes a path, or `-` for stdin");
                    std::process::exit(2);
                }
            }
            "--reply" => {
                reply = true;
                i += 1;
            }
            "--a11y-probe" => {
                probe = true;
                i += 1;
            }
            "--captions" => {
                captions = true;
                i += 1;
            }
            "--speak" => {
                speak = true;
                i += 1;
            }
            // A voice name is backend vocabulary, not engine vocabulary: the
            // `cmn` that fixes Mandarin under espeak-ng is not what macOS's
            // `say` calls it. So the value is forwarded verbatim and never
            // validated here. A leading `-` is refused because the next token
            // would otherwise be swallowed as a voice name.
            "--voice" => match args.get(i + 1) {
                Some(name) if !name.is_empty() && !name.starts_with('-') => {
                    voice = Some(name.clone());
                    i += 2;
                }
                _ => {
                    eprintln!("--voice takes a voice name, such as `cmn` or `Tingting`");
                    std::process::exit(2);
                }
            },
            "--help" | "-h" => {
                println!(
                    "rustim-app [--frames N] \
                     [--png OUT.png | --video OUT.mp4 | --video-png DIR | --inspect | --a11y-probe] \
                     [--srt OUT.srt] [--scene NAME] [--motion full|reduced] \
                     [--script PATH|-] [--reply] [--captions] [--speak [--voice NAME]]"
                );
                println!("scenes: {}", scene_names());
                println!("--inspect: print the accessibility tree as JSON to stdout");
                println!(
                    "--motion: play every frame, or land each beat on its end state \
                     (default: this desktop's preference)"
                );
                println!(
                    "--script: apply one JSON command per line (JSONL); `-` reads stdin, \
                     and without --scene the script gets an empty one"
                );
                println!(
                    "--video: render the timeline to an mp4 by piping raw frames to \
                     `ffmpeg` on PATH (600 frames at 60 fps if --frames is absent)"
                );
                println!(
                    "--video-png: ...or write numbered PNGs into a directory, which needs \
                     no external program and is byte-reproducible"
                );
                println!(
                    "--srt: with either video mode, also write the narration track as \
                     SubRip subtitles"
                );
                println!("--reply: answer each command with one JSON line on stdout");
                println!(
                    "--captions: draw the live region's text (narration, focus, \
                     `animation ended`) at the bottom of the frame"
                );
                println!(
                    "--speak: read the same text aloud with the platform's speech \
                     synthesiser (silent while a screen reader is attached)"
                );
                println!(
                    "--voice: with --speak, name the synthesiser's voice; the value is the \
                     backend's own (`cmn` for Mandarin in espeak-ng, `Tingting` in macOS \
                     `say`), and the default is the system's, which may read Chinese text \
                     one character at a time"
                );
                println!(
                    "--a11y-probe: open a window, offer one announcement, and report on stderr \
                     at exit whether a screen reader attached (600 frames if --frames is absent)"
                );
                println!("animations: {}", feed::animation_kinds());
                println!("rates: {}", feed::rate_names());
                println!();
                println!("example --script lines:");
                println!(
                    r#"  {{"action":"add","type":"circle","params":{{"radius":1.5,"label":"圆"}}}}"#
                );
                // `camera` is a command, not an animation kind: without
                // `duration` it writes the view now, with one it queues a beat.
                println!(r#"  {{"action":"camera","focus":4294967297,"duration":1.0}}"#);
                // So is `motion`, and it takes effect at the next beat started —
                // a running beat keeps the policy it was handed.
                println!(r#"  {{"action":"motion","mode":"reduced"}}"#);
                // `narrate` is a command too: `at` is relative to when the line
                // arrived, which is how a file feed (everything applied at t = 0)
                // still paces itself.
                println!(r#"  {{"action":"narrate","text":"先看这个三角形","at":2.0}}"#);
                // `stop` interrupts: queued beats and running animations are
                // dropped *without* their completion step, so the picture stays
                // where the last frame left it.
                println!(r#"  {{"action":"stop"}}"#);
                // `rate` names the easing curve by Manim's name; absent means the
                // animation's own default. The full vocabulary is the line above.
                println!(
                    r#"  {{"action":"play","animation":{{"target":4294967297,"type":"fade_in","rate":"ease_in_out_sine"}}}}"#
                );
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }

    // Both flags mean "no event loop"; asking for both is asking for two
    // different retirements of the same scene, and silently picking one would
    // make a script that passed both look like it had worked.
    if inspect && png.is_some() {
        eprintln!("--inspect and --png are mutually exclusive");
        std::process::exit(2);
    }

    // One headless retirement per run: a still and a timeline are different
    // answers to the same scene, and a run asked for both would have to pick.
    let exporting_video = video.is_some() || video_png.is_some();
    if video.is_some() && video_png.is_some() {
        eprintln!("--video and --video-png are mutually exclusive");
        std::process::exit(2);
    }
    if exporting_video && (inspect || png.is_some()) {
        eprintln!("--video and --png/--inspect are mutually exclusive");
        std::process::exit(2);
    }
    // A subtitle track is a pair with the frames it times. Writing one from a
    // run that produced no frames would give a host a file whose cue times
    // match nothing.
    if srt.is_some() && !exporting_video {
        eprintln!("--srt needs --video or --video-png: it times the frames they write");
        std::process::exit(2);
    }

    // The other two ways to ask for something this binary cannot do, both of
    // them reported rather than silently ignored — a host that sent `--reply`
    // and got no stream would conclude the engine was refusing its commands.
    //
    // `--reply` needs a script because a reply only exists per command, and two
    // flags cannot both own stdout: a reader of the merged stream could not tell
    // an accessibility tree from a reply.
    if reply && script.is_none() {
        eprintln!("--reply needs --script: there is nothing to answer");
        std::process::exit(2);
    }
    if reply && inspect {
        eprintln!("--reply and --inspect both write to stdout; run them separately");
        std::process::exit(2);
    }
    // Captions are pixels. `--inspect` prints a tree and never renders one, so
    // accepting the pair would silently drop half of what was asked for.
    if captions && inspect {
        eprintln!("--captions needs a frame; --inspect prints a tree");
        std::process::exit(2);
    }
    // Speech takes seconds and a headless run has no time axis: the still is
    // captured in microseconds and the process exits, so every sentence would
    // be cut off. Same discipline as `--a11y-probe`.
    if speak && (inspect || png.is_some() || exporting_video) {
        eprintln!("--speak needs the window; a headless run has no time to speak during");
        std::process::exit(2);
    }
    // A voice only exists relative to a sentence. Accepting `--voice` alone
    // would look like the run had chosen a voice and then said nothing.
    if voice.is_some() && !speak {
        eprintln!("--voice changes how --speak sounds; without it there is nothing to say");
        std::process::exit(2);
    }

    // The probe is the one accessibility mode that needs a window: it exists to
    // ask a platform backend a question, and that backend exists only in the
    // windowed path. Combining it with a headless flag would answer a different
    // question, so it is a refusal rather than a precedence rule.
    if probe && (inspect || png.is_some() || reply || exporting_video) {
        eprintln!(
            "--a11y-probe needs the window and cannot be combined with --inspect, --png, \
             --reply or a video export"
        );
        std::process::exit(2);
    }
    // 600 real frames — roughly ten seconds on a vsync-paced desktop, which is
    // long enough to start a reader after the window appears. The loop is not
    // throttled here, so the wall-clock length is whatever the compositor
    // allows; an explicit `--frames` still wins.
    if probe && max_frames.is_none() {
        max_frames = Some(600);
    }
    // An export defaults to the same ten seconds the still captures, so
    // `--video out.mp4` and `--png out.png` describe the same span of the scene
    // and the last exported frame is the still. Zero is a refusal rather than a
    // technically-empty file.
    if exporting_video {
        if max_frames == Some(0) {
            eprintln!("--frames 0 would write a video with no frames in it");
            std::process::exit(2);
        }
        if max_frames.is_none() {
            max_frames = Some((STILL_SECONDS / STEP) as u64);
        }
    }

    let viewport = Viewport {
        frame_width: WIDTH,
        frame_height: HEIGHT,
    };

    // The flag wins; without it the desktop is asked, best effort, once. Read here
    // rather than in `SceneContext` because a preference is a platform question and
    // the context lives in the wasm kernel — see `rustim_app::motion` for exactly
    // which platforms answer and which do not.
    // An export is an artifact, so it must not depend on the desktop that made
    // it: reduced motion is a *player's* preference, and a video rendered on a
    // machine with it enabled would be a different film from the same command on
    // another machine. The same reason the golden suite passes `--motion full`.
    let motion = motion.unwrap_or_else(|| {
        if exporting_video {
            Motion::Full
        } else {
            rustim_app::motion::from_platform()
        }
    });

    // A script with no `--scene` runs against an empty scene rather than the
    // default one: a host that builds the picture out of commands did not ask
    // for the Pythagorean figure, and `--scene pythagorean --script …` still
    // asks for it explicitly. `--scene blank` is not a name — see `BlankScene`.
    let blank = !scene_given && script.is_some();

    // A bad scene name is a refusal, not a panic — same rule the command layer
    // follows, and this one is reachable from a shell so it has to read well.
    let mut scene: Box<dyn Scene> = if blank {
        Box::new(BlankScene)
    } else {
        let Some(scene) = scene_by_name(&scene_name) else {
            eprintln!("unknown scene: {scene_name}");
            eprintln!("available scenes: {}", scene_names());
            std::process::exit(2);
        };
        scene
    };
    // The title names the scene a *human* would recognise, and an empty scene is
    // named by what it is rather than by the default it is not.
    let scene_name = if blank {
        "blank".to_string()
    } else {
        scene_name
    };

    // Headless paths: no event loop, no window. This is the same scene and the
    // same draw list as the windowed path, which is the point — an exported
    // frame cannot drift from a displayed one, and neither can an inspected tree.
    //
    // `--inspect` first. It is the only way to see the accessibility layer on a
    // machine that has no screen reader and no AT-SPI session bus (which is this
    // one: see `rustim_app::a11y`), and it is what a script or a future agent
    // surface reads instead of a window.
    if inspect {
        let mut ctx = context(viewport, motion);
        scene.construct(&mut ctx);
        // After `construct`, so the scene's own objects keep the ids they always
        // had and a script's ids follow them deterministically — and before
        // `settle`, so the tree this prints is the tree the script built.
        if let Some(path) = &script {
            apply_script(path, &mut *scene, &mut ctx, reply);
        }
        let mut list = DrawList::default();
        settle(&mut *scene, &mut ctx, &mut list);
        // No `live` text: the announcement node exists only where there is a
        // platform to announce to, so there is nothing pending in a process that
        // was never shown.
        let tree = ctx.semantic_tree(None);
        println!(
            "{}",
            serde_json::to_string_pretty(&tree).expect("the semantic tree is serialisable")
        );
        return;
    }

    if let Some(path) = png {
        let mut ctx = context(viewport, motion);
        scene.construct(&mut ctx);
        if let Some(script) = &script {
            apply_script(script, &mut *scene, &mut ctx, reply);
        }
        let mut list = DrawList::default();
        settle(&mut *scene, &mut ctx, &mut list);

        if captions {
            // The still is taken at t = 10, so the caption is whatever line is
            // due by then — the last one, for a script that paced itself — and
            // the scene summary when there was no narration at all. Never the
            // *text of a caption that would have expired*: expiry is a wall-clock
            // policy for a window, and a still has to keep what it says.
            let objects = ctx.semantic_tree(None).nodes.len().saturating_sub(1);
            let due = ctx.take_narration();
            let text = due
                .last()
                .cloned()
                .unwrap_or_else(|| rustim_app::caption::scene_summary(&scene_name, objects));
            match rustim_app::caption::Caption::new() {
                Ok(mut caption) => {
                    caption.set_text(text);
                    // Rebuild the list **without advancing time**: a
                    // `build_frame(.., 0.0)` would still let `advance` start the
                    // next queued beat, which can change the picture a still is
                    // supposed to freeze. `build_draw_list` clears first, so this
                    // is the same frame `settle` ended on, plus the band.
                    scene.build_draw_list(&ctx, &mut list);
                    caption.push_ops(&mut list);
                }
                // A missing font disables captions, not the render: the picture
                // is still the answer to the question that was asked.
                Err(e) => eprintln!("captions: {e} (rendering without them)"),
            }
        }

        let mut renderer = CpuRenderer::new(WIDTH, HEIGHT);
        renderer.render(&list).expect("render");
        rustim_export::write_png(&renderer, &path).expect("write png");
        note(
            reply,
            format_args!("wrote {path} ({}x{})", renderer.width(), renderer.height()),
        );
        // The still is a frame this host wrote, so a subscriber hears about the
        // events its timeline produced and then one `frame_ready` for the image
        // — the same pair the windowed loop emits. After the informational line,
        // because that line went to stderr: on stdout the reply stream stays the
        // only thing with a line order a host can rely on.
        if reply {
            let mut out = std::io::stdout().lock();
            let _ = feed::publish_events(&mut ctx, &mut out)
                .and_then(|()| feed::publish_frame(&mut ctx, 1, &mut out));
        }
        return;
    }

    if exporting_video {
        let mut ctx = context(viewport, motion);
        scene.construct(&mut ctx);
        if let Some(script) = &script {
            apply_script(script, &mut *scene, &mut ctx, reply);
        }
        let frames = max_frames.unwrap_or((STILL_SECONDS / STEP) as u64);

        // Built once, before the first frame: a provider per frame would reload
        // the bundled face 600 times. A missing font disables the band and
        // leaves the export itself intact, exactly as the `--png` path does.
        let mut caption = if captions {
            match rustim_app::caption::Caption::new() {
                Ok(caption) => Some(caption),
                Err(e) => {
                    eprintln!("captions: {e} (exporting without them)");
                    None
                }
            }
        } else {
            None
        };

        let fps = (1.0 / STEP).round() as u32;
        let mut sink: Box<dyn rustim_export::VideoSink> = if let Some(path) = &video {
            // Resolved before the render, not on the first frame: "ffmpeg is
            // not installed" is one clear line, and the run has not spent ten
            // seconds rasterising by the time it is reported.
            let program = rustim_app::speech::find("ffmpeg");
            let Some(program) = program else {
                eprintln!(
                    "--video {path}: no `ffmpeg` on PATH; `--video-png DIR` writes the \
                     same frames without one"
                );
                std::process::exit(2);
            };
            match rustim_export::FfmpegSink::spawn(
                program,
                path,
                u32::from(WIDTH),
                u32::from(HEIGHT),
                fps,
            ) {
                Ok(sink) => Box::new(sink),
                Err(e) => {
                    eprintln!("--video {path}: {e}");
                    std::process::exit(2);
                }
            }
        } else {
            let dir = video_png.as_deref().expect("one of the two video modes");
            // A shorter export into a directory a longer one already filled
            // leaves the tail behind, and the directory then reads as a longer
            // sequence than the run produced. Reported rather than deleted: the
            // path came from the caller and may hold anything.
            if let Ok(entries) = std::fs::read_dir(dir) {
                let stale = entries
                    .filter_map(Result::ok)
                    .filter(|e| {
                        let name = e.file_name();
                        let name = name.to_string_lossy();
                        name.starts_with("frame-") && name.ends_with(".png")
                    })
                    .count();
                if stale > 0 {
                    eprintln!(
                        "--video-png {dir}: it already holds {stale} frame-*.png; \
                         frames past this run's end will stay"
                    );
                }
            }
            match rustim_export::PngSequenceSink::new(dir, u32::from(WIDTH), u32::from(HEIGHT)) {
                Ok(sink) => Box::new(sink),
                Err(e) => {
                    eprintln!("--video-png {dir}: {e}");
                    std::process::exit(2);
                }
            }
        };

        let cues = export_timeline(
            &mut *scene,
            &mut ctx,
            frames,
            sink.as_mut(),
            caption.as_mut(),
            captions || srt.is_some(),
            &scene_name,
        )
        .unwrap_or_else(|e| {
            eprintln!("export: {e}");
            std::process::exit(2);
        });
        if let Err(e) = sink.finish() {
            eprintln!("export: {e}");
            std::process::exit(2);
        }

        match (&video, &video_png) {
            (Some(path), _) => note(
                reply,
                format_args!("wrote {path} ({frames} frames at {fps} fps)"),
            ),
            (_, Some(dir)) => note(
                reply,
                format_args!("wrote {frames} frames into {dir} ({fps} fps)"),
            ),
            _ => {}
        }
        if let Some(path) = &srt {
            let mut cues = cues;
            cues.sort_by(|a, b| a.start.total_cmp(&b.start));
            match rustim_export::srt::write(path, &cues) {
                Ok(()) => note(reply, format_args!("wrote {path} ({} cues)", cues.len())),
                Err(e) => {
                    eprintln!("--srt {path}: {e}");
                    std::process::exit(2);
                }
            }
        }
        return;
    }

    // The event loop is built before the window, and the window is created
    // inside `resumed`, because the accessibility adapter needs an
    // `&ActiveEventLoop` and refuses to attach to an already-visible window.
    let mut builder = EventLoop::<AppEvent>::with_user_event();
    let event_loop = builder.build().expect("event loop");
    let proxy = event_loop.create_proxy();

    let mut ctx = context(viewport, motion);
    scene.construct(&mut ctx);

    // The feed, connected *before* the loop runs. A file is applied here, at
    // virtual time zero, exactly as a headless run applies it; stdin is read on
    // its own thread, because the event loop must not block on a pipe that may
    // never send anything. Both then reach the same `feed::apply_all` — the file
    // through the call below, stdin through `AppEvent::Command`.
    if let Some(path) = &script {
        if path == feed::STDIN {
            feed::spawn_stdin(proxy.clone());
        } else {
            apply_script(path, &mut *scene, &mut ctx, reply);
        }
    }

    // Built once for the whole run: the provider parses the bundled face, and a
    // caption that reloaded it per line would make every announcement cost a
    // font load. A missing `assets/fonts/` disables the band and leaves the rest
    // of the run intact.
    let caption = if captions {
        match rustim_app::caption::Caption::new() {
            Ok(caption) => Some(caption),
            Err(e) => {
                eprintln!("captions: {e} (continuing without them)");
                None
            }
        }
    } else {
        None
    };

    // Built once, before the loop. A missing backend is a report and a `None`,
    // not a refusal: the run itself is still exactly what was asked for, and a
    // machine with no `espeak-ng` is the common case this path has to survive.
    let speaker = if speak {
        match rustim_app::speech::Platform::current() {
            None => {
                eprintln!(
                    "speech: self-voicing is not implemented on this platform; \
                     --captions still works"
                );
                None
            }
            Some(platform) => {
                let found = std::env::var_os("PATH")
                    .and_then(|path| rustim_app::speech::find_any_in(&path, platform.programs()));
                match found {
                    Some((program, _)) => Some(rustim_app::speech::Speaker::spawn(
                        rustim_app::speech::ProcessVoice::new(program, platform, voice),
                    )),
                    None => {
                        eprintln!("{}", rustim_app::speech::missing_report(platform));
                        None
                    }
                }
            }
        }
    } else {
        None
    };

    let mut app = App {
        windowed: None,
        proxy,
        title: format!("rustim — {scene_name}"),
        renderer: CpuRenderer::new(WIDTH, HEIGHT),
        list: DrawList::new(WIDTH, HEIGHT),
        // A zero-sized list of no ops: whatever the first frame is, its size
        // differs and `damage_since` says `Full`. The renderer's buffer has no
        // pixels yet, so that is exactly right.
        prev: DrawList::default(),
        stats: RenderStats::default(),
        scene,
        ctx,
        queued: Vec::new(),
        pending: Vec::new(),
        reply,
        probe,
        caption,
        speaker,
        scene_name: scene_name.clone(),
        // The opening summary is only for a run that can *show* or *say* it —
        // `--captions` or `--speak`. A plain run's live region keeps its
        // existing behaviour, and the probe's own announcement is not competing
        // with a scene description it did not ask for.
        summary_pending: (captions || speak) && !probe,
        last_live: String::new(),
        feed: feed::Counts::default(),
        mods: ModifiersState::empty(),
        pointer: input::Pointer::new(),
        paused: false,
        step: 0,
        was_animating: true,
        text_error: None,
        frame: 0,
        presented: 0,
        max_frames,
        render_ms: Vec::new(),
        last: Instant::now(),
    };
    if probe {
        // On stderr, before the window appears: the report at the end is only
        // interesting if a reader attached in between, and nothing else tells
        // the person running this what the run is waiting for.
        eprintln!(
            "a11y probe: scene {} — attach a screen reader now; the report prints after {} frames",
            scene_name,
            max_frames.unwrap_or(0)
        );
    }
    event_loop.run_app(&mut app).expect("run_app");
    // The probe's answer, first, because everything after it is incidental. On
    // stderr: stdout is the protocol's even when `--reply` is off, and this
    // report is for a person or a CI log. Node count is read at exit, so it
    // describes the scene the run actually ended with.
    if app.probe {
        let (full, incremental) = app.windowed.as_ref().map_or((0, 0), |w| {
            (w.a11y.full_pushes(), w.a11y.incremental_pushes())
        });
        let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        let report = rustim_app::a11y::ProbeReport {
            platform: &platform,
            scene: &scene_name,
            semantic_nodes: app.ctx.semantic_tree(None).nodes.len(),
            full_pushes: full,
            incremental_pushes: incremental,
        };
        eprintln!("{}", report.render());
    }
    // What a live session's feed did, on stderr so it cannot be mistaken for a
    // reply line. Silent unless something was refused.
    if let Some(line) = app.feed.report() {
        eprintln!("{line}");
    }
    if !app.render_ms.is_empty() {
        let mean: f64 = app.render_ms.iter().sum::<f64>() / app.render_ms.len() as f64;
        let mut sorted = app.render_ms.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        note(
            reply,
            format_args!(
                "{} frames: mean {mean:.2} ms, p50 {:.2} ms, max {:.2} ms  ({:.0} fps)",
                app.render_ms.len(),
                sorted[sorted.len() / 2],
                sorted[sorted.len() - 1],
                1000.0 / mean.max(1e-9)
            ),
        );
    }
}
