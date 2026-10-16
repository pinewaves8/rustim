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
//! rustim-app --scene text --png t.png
//! ```

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::Instant;

use rustim_app::a11y::{A11y, Request};
use rustim_app::{AppEvent, Intent, input};
use rustim_math::{Affine2, BezPath, Point, Viewport, bezier};
use rustim_mobject::{Mobject, NodeId, NodeKind, Style};
use rustim_render::{CpuRenderer, DrawList, DrawOp, RenderError, Renderer};
use rustim_scene::command::Command;
use rustim_scene::{Scene, SceneContext};
use rustim_text::{FontProvider, ShapedText, TextStyle};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
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

// ---------------------------------------------------------------------------
// The reference scene
// ---------------------------------------------------------------------------

/// Build a Manim-layout point array from a polygon's corners.
///
/// Manim expresses even straight-edged shapes as cubics, with the control points
/// lying on the segment. Keeping that uniform representation is what makes a
/// straight-edged mobject morphable into a curved one without a special case.
///
/// The corner list is *open* — callers name the distinct corners and let the
/// shape imply its own closure — so closing it is this function's job. The
/// interior control points come from `bezier::points_as_corners` because Manim's
/// `set_points_as_corners` puts them at 1/3 and 2/3 of the segment, not stacked
/// on its endpoints. Both spellings draw the same line, which is exactly why the
/// difference is easy to miss and expensive to find later.
fn poly(corners: &[(f64, f64)], color: rustim_color::Srgb) -> Mobject {
    let mut closed: Vec<Point> = corners.iter().map(|&(x, y)| Point::new(x, y)).collect();
    closed.push(closed[0]);
    let pts = bezier::points_as_corners(&closed);

    let style = Style {
        fill: color,
        fill_opacity: 1.0,
        stroke: color,
        stroke_opacity: 0.0,
        stroke_width: 0.0,
    };
    Mobject::new(pts).with_style(style)
}

/// How tall the figure is drawn, in Manim units — the same 6.0 the reference
/// scene passes to `scale_to_fit_height`.
const FIGURE_HEIGHT: f64 = 6.0;

/// Fit a set of mobjects to [`FIGURE_HEIGHT`] and centre them on the origin.
///
/// Measured from the geometry rather than derived by hand: the figure's bounds
/// are not the obvious ones (legs 3 and 4 do not make it 7 units tall — the
/// square on the hypotenuse reaches to y = 7 but the largest square reaches down
/// to y = -3, so it is 10 tall and 11 wide). Hand-deriving that is how the first
/// version of this scene ended up overflowing the frame by 25%.
fn fit_and_centre(objects: &mut [Mobject]) {
    let mut bounds: Option<rustim_math::Rect> = None;
    for obj in objects.iter() {
        for p in &obj.points {
            let r = rustim_math::Rect::new(p.x, p.y, p.x, p.y);
            bounds = Some(bounds.map_or(r, |b| b.union(r)));
        }
    }
    let Some(b) = bounds else { return };
    let centre = b.center();
    let place =
        Affine2::scale(FIGURE_HEIGHT / b.height()) * Affine2::translate((-centre.x, -centre.y));
    for obj in objects.iter_mut() {
        obj.transform = place;
    }
}

/// The Pythagorean figure: legs 3 and 4, a square on each of the three sides.
///
/// The same figure the Manim reference renders (see `p0-spikes/`), which is what
/// makes the two output images directly comparable.
struct PythagoreanScene {
    /// Which square is being revealed. The tutorial drives its stages off a
    /// `state` + `timer` pair rather than a real timeline; keeping that here is
    /// deliberate — it is the *real-time* sequencing model, and it is the part
    /// that has no Manim equivalent.
    stage: usize,
    nodes: Vec<NodeId>,
}

impl PythagoreanScene {
    fn new() -> Self {
        Self {
            stage: 0,
            nodes: Vec::new(),
        }
    }
}

impl Scene for PythagoreanScene {
    fn construct(&mut self, ctx: &mut SceneContext) {
        const A: f64 = 3.0;
        const B: f64 = 4.0;

        // Triangle, legs along +x and +y, right angle at the origin.
        //
        // The kinds and labels are invisible in the golden images — they change
        // nothing about the geometry — and are what make the accessibility tree
        // say something. Without them a reader gets four unnamed "图形"; with
        // them it gets the figure the scene is about, and the description each
        // node carries is measured from the geometry, so a reader is told the
        // square on a leg of 3 has an area of 9 without anyone declaring it.
        let tri = poly(&[(0.0, 0.0), (A, 0.0), (0.0, B)], rustim_color::RED)
            .with_kind(NodeKind::RightTriangle)
            .with_label("直角三角形");
        // Square on leg a, below it.
        let sq_a = poly(
            &[(0.0, 0.0), (A, 0.0), (A, -A), (0.0, -A)],
            rustim_color::BLUE,
        )
        .with_kind(NodeKind::Square)
        .with_label("a 的正方形");
        // Square on leg b, to the left.
        let sq_b = poly(
            &[(0.0, 0.0), (0.0, B), (-B, B), (-B, 0.0)],
            rustim_color::YELLOW,
        )
        .with_kind(NodeKind::Square)
        .with_label("b 的正方形");
        // Square on the hypotenuse, erected outwards. Manim draws this one
        // translucent (`fill_opacity=0.45`) so the triangle shows through it;
        // matching that is what makes the two images directly comparable.
        let mut sq_c = poly(
            &[(A, 0.0), (0.0, B), (B, A + B), (A + B, A)],
            rustim_color::RED,
        )
        .with_kind(NodeKind::Square)
        .with_label("c 的正方形");
        sq_c.style.fill_opacity = 0.45;

        // Manim's own `VGroup(sq_c, sq_b, sq_a, tri)` order, which is also the
        // draw order: the translucent hypotenuse square first, the triangle over
        // it, so the triangle reads as solid red through the overlay.
        let mut figure = [sq_c, sq_b, sq_a, tri];
        fit_and_centre(&mut figure);
        // Note this is all world-space arithmetic in Manim units — no pixel
        // constants anywhere in scene code, which is the fix for the tutorial's
        // unhandled coordinate system.
        for mut obj in figure {
            obj.opacity = 0.0;
            self.nodes.push(ctx.add(obj));
        }
    }

    fn update(&mut self, dt: f64, ctx: &mut SceneContext) {
        ctx.animations.update(dt, &mut ctx.tree);

        // Reveal one square per 0.6 s, but only once the previous reveal has
        // finished — so a slow frame makes the sequence *later*, never wrong.
        if ctx.animations.is_empty() && self.stage < self.nodes.len() {
            let id = self.nodes[self.stage];
            ctx.animations
                .add(rustim_anim::FadeIn::new(id).with_duration(0.6));
            self.stage += 1;
        }
    }
}

/// A style that fills and does not stroke — what every shape in these scenes
/// wants, since the tree walk only emits fills.
fn filled(color: rustim_color::Srgb, alpha: f64) -> Style {
    Style {
        fill: color,
        fill_opacity: alpha,
        stroke: color,
        stroke_opacity: 0.0,
        stroke_width: 0.0,
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
            rustim_app::text::push_text(list, shaped, world, &ctx.viewport, paint);
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

/// The `--scene` value used when the flag is absent.
const DEFAULT_SCENE: &str = "pythagorean";

/// Every scene the binary can render, by the name `--scene` takes.
///
/// This exists for the golden-image suite: a test that can only render one
/// hardcoded scene proves very little, and the baselines have to be regenerated
/// per scene anyway. `pythagorean` keeps its name so the `p0-spikes/`
/// comparisons against Manim still line up.
fn scene_by_name(name: &str) -> Option<Box<dyn Scene>> {
    match name {
        "pythagorean" => Some(Box::new(PythagoreanScene::new())),
        "primitives" => Some(Box::new(PrimitivesScene)),
        "composite" => Some(Box::new(CompositeScene)),
        "text" => Some(Box::new(TextScene::new())),
        "formula" => Some(Box::new(FormulaScene::new())),
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
    ["pythagorean", "primitives", "composite", "text", "formula"].join(", ")
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
    scene: Box<dyn Scene>,
    ctx: SceneContext,
    /// Commands produced by the keyboard or by assistive technology, applied at
    /// the top of the next frame. They take the same path an AI's JSON command
    /// takes, which is the point: one validator, so input cannot reach a state
    /// the command layer would refuse.
    queued: Vec<Command>,
    /// The window's current modifier state. winit delivers modifiers in their
    /// own event, so this is tracked across events rather than read off the
    /// keypress — a Shift+Tab that read `event` alone would look like a Tab.
    mods: ModifiersState,
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
            Intent::StepFrame(n) => {
                self.paused = true;
                self.step = (self.step + i32::from(n)).clamp(-60, 60);
            }
        }
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
        surface
            .resize(
                NonZeroU32::new(u32::from(WIDTH)).unwrap(),
                NonZeroU32::new(u32::from(HEIGHT)).unwrap(),
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
        self.request_redraw();
    }

    /// Accessibility events arrive here, not in `window_event`: the platform's
    /// backend calls in from its own thread and the winit adapter forwards
    /// through the event-loop proxy.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: AppEvent) {
        // Irrefutable today. Written as a `let` rather than a match so that
        // adding a second `AppEvent` variant is a compile error here, at the
        // one place that has to learn about it.
        let AppEvent::AccessKit(event) = event;
        match event.window_event {
            accesskit_winit::WindowEvent::InitialTreeRequested => {
                // Routed through the ordinary frame path rather than answered
                // here. The adapter was built with an event-loop proxy, so it
                // cannot take a synchronous initial tree; the answer has to be
                // the next frame's full snapshot, and `a11y.update` runs before
                // `request_redraw` in that frame, which is what guarantees the
                // snapshot describes the state that is on screen.
                self.request_redraw();
            }
            accesskit_winit::WindowEvent::ActionRequested(req) => {
                let request = self
                    .windowed
                    .as_mut()
                    .and_then(|w| w.a11y.core().on_action(&req, &self.ctx));
                match request {
                    Some(Request::Command(cmd)) => {
                        self.queued.push(cmd);
                        self.request_redraw();
                    }
                    Some(Request::TogglePlayback) => {
                        self.paused = !self.paused;
                        self.request_redraw();
                    }
                    None => {}
                }
            }
            // Nothing to do: the next `update` is a full snapshot anyway, and
            // `update_if_active` will not even build one while inactive.
            accesskit_winit::WindowEvent::AccessibilityDeactivated => {}
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
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    if let Some(w) = self.windowed.as_mut() {
                        w.surface
                            .resize(
                                NonZeroU32::new(size.width).unwrap(),
                                NonZeroU32::new(size.height).unwrap(),
                            )
                            .expect("resize surface");
                    }
                    // The renderer follows the surface, so the tutorial's
                    // hardcoded 1280x720 background stops being wrong the moment
                    // the window is resized. The viewport follows with it, and
                    // the accessibility tree's bounds are derived from the
                    // viewport, so they follow too.
                    self.renderer.resize(size.width as u16, size.height as u16);
                    self.ctx.viewport.frame_width = size.width as u16;
                    self.ctx.viewport.frame_height = size.height as u16;
                }
            }
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let wall = (now - self.last).as_secs_f64();
                self.last = now;
                let dt = self.frame_dt(wall);

                // Before the frame, not after: the frame must show the result of
                // the key that caused it, and an animation applied at the end of
                // the frame would arrive one frame late.
                for cmd in std::mem::take(&mut self.queued) {
                    if let Err(e) = self.ctx.handle(&cmd) {
                        // A refusal is a value, not a panic — the rule an AI's
                        // command follows, and this path is reachable by
                        // pressing a key or by an assistive technology naming a
                        // node that no longer exists.
                        eprintln!("command refused: {e}");
                    }
                }

                let t0 = Instant::now();
                // `&mut *` rather than `&mut self.scene`: a `Box<dyn Scene>` does
                // not coerce to `&mut dyn Scene` in argument position without the
                // reborrow.
                build_frame(&mut *self.scene, &mut self.ctx, &mut self.list, dt);
                // A frame that could not be rasterised must not be blitted — the
                // pixel buffer still holds the *previous* frame, so presenting it
                // would show a stale image as if it were current. A frame that
                // was rasterised but is missing a glyph is presented, because it
                // is a true picture of the scene minus one glyph, and the
                // complaint is reported instead.
                let rasterised = match self.renderer.render(&self.list) {
                    Ok(()) => {
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
                if rasterised
                    && let Some(w) = self.windowed.as_mut()
                    && let Ok(mut buf) = w.surface.buffer_mut()
                {
                    blit(self.renderer.pixels(), &mut buf);
                    let _ = buf.present();
                }
                self.render_ms.push(t0.elapsed().as_secs_f64() * 1000.0);

                // An animation ending is the one change a blind user cannot poll
                // for: the scene is identical on either side of it.
                let animating = !self.ctx.animations.is_empty();
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
                }

                self.frame += 1;
                if self.frame.is_multiple_of(120) {
                    let recent = &self.render_ms[self.render_ms.len() - 120..];
                    let mean: f64 = recent.iter().sum::<f64>() / recent.len() as f64;
                    println!(
                        "frame {:>5}  {mean:.2} ms/frame  ({:.0} fps)  {} ops  focus {}",
                        self.frame,
                        1000.0 / mean.max(1e-9),
                        self.list.ops.len(),
                        self.windowed
                            .as_ref()
                            .and_then(|w| w.a11y.focused())
                            .map_or_else(|| "—".to_string(), |id| id.raw().to_string()),
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
    let mut inspect = false;
    let mut scene_name = DEFAULT_SCENE.to_string();
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
            "--inspect" => {
                inspect = true;
                i += 1;
            }
            "--scene" => {
                if let Some(name) = args.get(i + 1) {
                    scene_name.clone_from(name);
                }
                i += 2;
            }
            "--help" | "-h" => {
                println!("rustim-app [--frames N] [--png OUT.png | --inspect] [--scene NAME]");
                println!("scenes: {}", scene_names());
                println!("--inspect: print the accessibility tree as JSON to stdout");
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

    let viewport = Viewport {
        frame_width: WIDTH,
        frame_height: HEIGHT,
        ..Default::default()
    };

    // A bad scene name is a refusal, not a panic — same rule the command layer
    // follows, and this one is reachable from a shell so it has to read well.
    let Some(mut scene) = scene_by_name(&scene_name) else {
        eprintln!("unknown scene: {scene_name}");
        eprintln!("available scenes: {}", scene_names());
        std::process::exit(2);
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
        let mut ctx = SceneContext::new(viewport);
        scene.construct(&mut ctx);
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
        let mut ctx = SceneContext::new(viewport);
        scene.construct(&mut ctx);
        let mut list = DrawList::default();
        settle(&mut *scene, &mut ctx, &mut list);

        let mut renderer = CpuRenderer::new(WIDTH, HEIGHT);
        renderer.render(&list).expect("render");
        rustim_export::write_png(&renderer, &path).expect("write png");
        println!("wrote {path} ({}x{})", renderer.width(), renderer.height());
        return;
    }

    // The event loop is built before the window, and the window is created
    // inside `resumed`, because the accessibility adapter needs an
    // `&ActiveEventLoop` and refuses to attach to an already-visible window.
    let mut builder = EventLoop::<AppEvent>::with_user_event();
    let event_loop = builder.build().expect("event loop");
    let proxy = event_loop.create_proxy();

    let mut ctx = SceneContext::new(viewport);
    scene.construct(&mut ctx);

    let mut app = App {
        windowed: None,
        proxy,
        title: format!("rustim — {scene_name}"),
        renderer: CpuRenderer::new(WIDTH, HEIGHT),
        list: DrawList::new(WIDTH, HEIGHT),
        scene,
        ctx,
        queued: Vec::new(),
        mods: ModifiersState::empty(),
        paused: false,
        step: 0,
        was_animating: true,
        text_error: None,
        frame: 0,
        max_frames,
        render_ms: Vec::new(),
        last: Instant::now(),
    };
    event_loop.run_app(&mut app).expect("run_app");
    if !app.render_ms.is_empty() {
        let mean: f64 = app.render_ms.iter().sum::<f64>() / app.render_ms.len() as f64;
        let mut sorted = app.render_ms.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "{} frames: mean {mean:.2} ms, p50 {:.2} ms, max {:.2} ms  ({:.0} fps)",
            app.render_ms.len(),
            sorted[sorted.len() / 2],
            sorted[sorted.len() - 1],
            1000.0 / mean.max(1e-9)
        );
    }
}
