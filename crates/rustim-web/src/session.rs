//! One scene, one context, one renderer: the target-independent half of a host.
//!
//! The loop is deliberately the same one `rustim-app` runs for `--png`, step for
//! step:
//!
//! ```text
//! Scene::update(dt) -> Scene::build_draw_list -> Renderer::render
//! ```
//!
//! `step` reaches the scene rather than `SceneContext::advance` directly, because
//! a scene may override `update` (the rule the trait's own docs state), and a
//! host that bypassed it would silently freeze a scene that reacts to its
//! timeline draining.
//!
//! # Why `advance` is separate from `step`
//!
//! A browser frame is one update and one rasterisation. A still — the artifact
//! this crate is checked against — is [`STILL_SECONDS`] of *virtual* time at the
//! fixed [`STEP`] the desktop exporter uses, with a single rasterisation at the
//! end. Those are the same state, but rendering six hundred times to prove it
//! would only make the test slower, so `advance` mirrors the desktop `settle`
//! loop exactly: it updates `n` times and presents once.

use rustim_anim::Motion;
use rustim_math::Viewport;
use rustim_render::{CpuRenderer, DrawList, Renderer};
use rustim_scene::a11y::SemanticTree;
use rustim_scene::command::{parse_command, reply_line};
use rustim_scene::{Scene, SceneContext};

/// The virtual timestep the still path, the exporter and the browser's
/// deterministic `advance` all use.
pub const STEP: f64 = 1.0 / 60.0;

/// How much virtual time a still represents — the desktop `--png` rule.
pub const STILL_SECONDS: f64 = 10.0;

/// The scenes this host can build.
///
/// The list is a function rather than a `const` so the `formula` feature can add
/// its scene without the default build claiming one it cannot construct.
pub fn scene_names() -> &'static [&'static str] {
    #[cfg(feature = "formula")]
    const NAMES: &[&str] = &["pythagorean", "formula"];
    #[cfg(not(feature = "formula"))]
    const NAMES: &[&str] = &["pythagorean"];
    NAMES
}

fn make_scene(name: &str) -> Result<Box<dyn Scene>, String> {
    match name {
        "pythagorean" => Ok(Box::new(rustim_scenes::Pythagorean)),
        other => Err(format!(
            "unknown scene `{other}`; this build knows: {}",
            scene_names().join(", ")
        )),
    }
}

/// A running scene, ready to rasterise.
pub struct Session {
    scene: Box<dyn Scene>,
    ctx: SceneContext,
    list: DrawList,
    renderer: CpuRenderer,
    viewport: Viewport,
    /// Frames whose state was built, whether or not they were presented.
    frames: u64,
    /// The 1-based physical line counter `apply` answers with.
    seq: u64,
    /// The last rasterisation failure, kept rather than unwrapped: a render
    /// error is a value a host can display, and the browser has no terminal to
    /// print a panic to.
    render_error: Option<String>,
}

impl Session {
    /// Build `scene` at `width` × `height` physical pixels.
    ///
    /// The scene's own `construct` runs here, at virtual time zero, exactly as
    /// the desktop host runs it before the first frame — so a script applied
    /// afterwards sees the same ids in the same order.
    pub fn new(width: u16, height: u16, scene: &str) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err(format!("a {width}x{height} canvas has no pixels to draw"));
        }
        let mut scene = make_scene(scene)?;
        let viewport = Viewport {
            frame_width: width,
            frame_height: height,
        };
        let mut ctx = SceneContext::new(viewport, Motion::Full);
        scene.construct(&mut ctx);
        Ok(Self {
            scene,
            ctx,
            list: DrawList::new(width, height),
            renderer: CpuRenderer::new(width, height),
            viewport,
            frames: 0,
            seq: 0,
            render_error: None,
        })
    }

    /// One frame: update the state, rebuild the draw list, rasterise it.
    pub fn step(&mut self, dt: f64) {
        self.update(dt);
        self.present();
    }

    /// [`STILL_SECONDS`] (or any duration) of virtual time, presented once.
    ///
    /// Truncating division mirrors `rustim-app::settle`'s `(seconds / STEP) as
    /// u32` rather than rounding: the two loops must land on the same frame, and
    /// ten seconds at one sixtieth is 600.0000000000001 steps, not 600.0.
    pub fn advance(&mut self, seconds: f64) {
        let steps = if seconds.is_finite() && seconds > 0.0 {
            (seconds / STEP) as u32
        } else {
            0
        };
        for _ in 0..steps {
            self.update(STEP);
        }
        self.present();
    }

    fn update(&mut self, dt: f64) {
        self.scene.update(dt, &mut self.ctx);
        // Through the scene, not straight through the context: a scene may have
        // draw operations of its own to add.
        self.scene.build_draw_list(&self.ctx, &mut self.list);
        self.frames += 1;
    }

    fn present(&mut self) {
        if let Err(e) = self.renderer.render(&self.list) {
            self.render_error = Some(e.to_string());
        }
    }

    /// The frame, RGBA8, **premultiplied** — exactly what the renderer produced.
    ///
    /// The canvas does *not* want this shape: `ImageData` is straight alpha.
    /// Almost every pixel is opaque, so the two agree almost everywhere — but
    /// "almost" was measured: the settled Pythagorean frame has 216 pixels at
    /// alpha 254 where the translucent square meets the background, and handing
    /// the premultiplied bytes straight to the canvas would make them one step
    /// dark. `rustim_render::straight_rgba_in_place` is the conversion, and the
    /// browser shell applies it into a reused buffer.
    pub fn pixels(&self) -> &[u8] {
        self.renderer.pixels()
    }

    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    /// Frames whose state was built (not just presented).
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// The last rasterisation failure, if any. `None` is the healthy case.
    pub fn render_error(&self) -> Option<&str> {
        self.render_error.as_deref()
    }

    /// The same semantic tree `--inspect` prints and the desktop adapter feeds
    /// to AccessKit. Pure, so a caller may project it every frame.
    pub fn semantic(&self) -> SemanticTree {
        self.ctx.semantic_tree(None)
    }

    /// Apply one JSON command line, returning its reply (without the newline).
    ///
    /// `None` is the blank-line skip, not a refusal — the same protocol
    /// `rustim-app --script` implements, read from the same two functions in
    /// `rustim_scene::command`. `seq` counts **physical** lines, blank ones
    /// included, so a host reading a log and a human reading the same log count
    /// the same way.
    ///
    /// Nothing is rendered here: the caller decides when the next frame is
    /// presented. The desktop feed makes the same split — applying a command
    /// changes what the next frame will be, not the pixels already on screen.
    pub fn apply(&mut self, line: &str) -> Option<String> {
        self.seq += 1;
        let seq = self.seq;
        let parsed = parse_command(line)?;
        let outcome = parsed.and_then(|cmd| match self.ctx.handle(&cmd) {
            Ok(diff) => {
                // The scene-visible hook, called from every route that reached
                // the tree — the feed and the keyboard on the desktop, the
                // command box here.
                self.scene.on_command(&diff, &mut self.ctx);
                Ok(diff)
            }
            Err(e) => Err(e),
        });
        Some(reply_line(seq, outcome))
    }
}
