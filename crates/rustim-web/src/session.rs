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
use rustim_math::{Camera, Viewport};
use rustim_render::{CpuRenderer, DrawList, Renderer};
use rustim_scene::a11y::SemanticTree;
use rustim_scene::command::{Command, parse_command, reply_line};
use rustim_scene::{NodeId, SessionState};
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

/// The one formula the `formula` page shows.
///
/// Typst math, not LaTeX — `frac(1, 2)`, not `\frac{1}{2}` — and chosen for
/// **structure**: a superscript and a fraction are what a MathML-aware reader
/// needs to hear as structure rather than as one long sentence. Deliberately no
/// CJK: the embedded font set a browser build ships has the math faces and not
/// the bundled CJK face (see [`rustim_text::formula::FormulaCompiler::from_embedded`]).
#[cfg(feature = "formula")]
const FORMULA_BODY: &str = "x^2 + frac(1, 2)";

/// The label the semantic tree speaks for it.
#[cfg(feature = "formula")]
const FORMULA_LABEL: &str = "公式";

#[cfg(feature = "formula")]
struct FormulaScene {
    /// Taken in `construct`; the compiler is the expensive part and lives only
    /// until the mobjects exist.
    formula: Option<rustim_text::formula::Formula>,
}

#[cfg(feature = "formula")]
impl Scene for FormulaScene {
    fn construct(&mut self, ctx: &mut SceneContext) {
        let Some(formula) = self.formula.take() else {
            return;
        };
        let ids = formula.insert(&mut ctx.tree);
        if let Some(root) = ctx.tree.get_mut(ids.root) {
            root.label = Some(FORMULA_LABEL.to_owned());
        }
    }
}

fn make_scene(name: &str) -> Result<Box<dyn Scene>, String> {
    match name {
        "pythagorean" => Ok(Box::new(rustim_scenes::Pythagorean)),
        #[cfg(feature = "formula")]
        "formula" => {
            // Compiled here rather than in `construct` so a fragment Typst
            // refuses is an error the host can display before it has a canvas,
            // instead of a scene that silently constructs nothing. This is the
            // embedded-only constructor: a wasm build has no font directory.
            let mut compiler = rustim_text::formula::FormulaCompiler::from_embedded()
                .map_err(|e| format!("the formula compiler could not load its fonts: {e}"))?;
            let formula = compiler
                .compile(FORMULA_BODY)
                .map_err(|e| format!("`{FORMULA_BODY}` does not compile: {e}"))?;
            Ok(Box::new(FormulaScene {
                formula: Some(formula),
            }))
        }
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
        // The feature *is* the capability: the formula artifact links Typst and
        // can answer a wire `add` of a formula; the plain artifact cannot, and
        // its `hello` says so rather than promising what the first command
        // would refuse. The desktop binary's answer is installed the same way
        // (`rustim_app::typeset`), which is the point of the seam.
        #[cfg(feature = "formula")]
        ctx.set_typesetter(Box::new(crate::typeset::WebTypesetter::new()));
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

    /// Rebuild the draw list from the state as it stands and rasterise it.
    ///
    /// This is virtual time zero: the scene's `construct` has run and no frame
    /// has been advanced. A host that starts paused shows this, and `advance`
    /// then begins from exactly the state a fresh `Session` would have had, so
    /// the first presented frame is not an extra clock step.
    pub fn refresh(&mut self) {
        self.scene.build_draw_list(&self.ctx, &mut self.list);
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

    /// The view as it stands — the base a pointer gesture reads.
    pub fn camera(&self) -> Camera {
        *self.ctx.camera()
    }

    /// Write a camera a pointer gesture produced, through the command layer.
    ///
    /// The gesture arithmetic already produces a camera that is finite and
    /// outlives [`rustim_math::gesture::MIN_ZOOM`]..=[`MAX_ZOOM`], so this
    /// normally cannot fail — but the *command layer* is where every value from
    /// outside the engine is validated, and a host that wrote the camera field
    /// directly would be the one caller in the workspace to skip it. The reply
    /// is a value rather than a panic for the same reason every other refusal
    /// is.
    ///
    /// Instant, not a beat: the user is dragging the view, and a queued
    /// `CameraMove` would lag the pointer. An animation already owns the camera
    /// until it ends, exactly as an instant `{"action":"camera"}` does on the
    /// desktop.
    pub fn set_camera(&mut self, camera: Camera) -> Result<(), String> {
        self.command(&Command::Camera {
            center: Some([camera.center.0, camera.center.1]),
            zoom: Some(camera.zoom),
            focus: None,
            duration: None,
        })
    }

    /// Apply a command the host built rather than parsed.
    ///
    /// The same route `set_camera` takes and the keyboard needs: a command this
    /// host constructs still goes through `SceneContext::handle`, so it meets the
    /// same validator a script line does and cannot reach a state the protocol
    /// would refuse. A refusal is a `String` rather than a panic, because a host
    /// has no terminal to print one to.
    pub fn command(&mut self, cmd: &Command) -> Result<(), String> {
        let outcome = self.ctx.handle(cmd).map_err(|e| e.to_string())?;
        if let Some(diff) = outcome.diff() {
            self.scene.on_command(diff, &mut self.ctx);
        }
        Ok(())
    }

    /// The session's own state — what a `query` reports beside the tree.
    ///
    /// The keyboard needs it to *toggle*: `space` means "the other one", and the
    /// engine is where the current one lives.
    pub fn session_state(&self) -> SessionState {
        self.ctx.session_state()
    }

    /// One node's opacity, or `None` if it is not there.
    ///
    /// An arrow key has to build an absolute value — the protocol has no
    /// "adjust by" command — so the host reads the current one first, exactly as
    /// the desktop's keymap does.
    pub fn node_opacity(&self, id: NodeId) -> Option<f64> {
        Some(self.ctx.tree.get(id)?.opacity)
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
        let outcome = parsed.and_then(|cmd| {
            let outcome = self.ctx.handle(&cmd)?;
            // The scene-visible hook, called from every route that reached the
            // tree — the feed and the keyboard on the desktop, the command box
            // here — and not for a query, which changed nothing.
            if let Some(diff) = outcome.diff() {
                self.scene.on_command(diff, &mut self.ctx);
            }
            Ok(outcome)
        });
        Some(reply_line(seq, outcome))
    }
}
