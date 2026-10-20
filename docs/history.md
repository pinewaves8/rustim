# History — how each step landed

> Detail reference for [`CLAUDE.md`](../CLAUDE.md), which is the entry point. Loaded on demand:
> this is a step-by-step changelog. `CLAUDE.md` carries the roadmap *status*; come here for the
> reasoning, the evidence and the test counts behind one step.
>
> Excerpted verbatim from `CLAUDE.md` on 2026-10-18 (the `## Roadmap` section).


Phase 0 (risk elimination) is done **and the skeleton it produced is verified green** — the toolchain,
the three probe crates, the window, PNG export, the workspace/CI skeleton, and the pixel-level Manim
comparison.

**P1 is complete.** The order was not arbitrary: every step moves `Mobject`, `DrawOp`, the command
protocol or the renderer backend, and moving those before the regression nets exist produces rework
that is invisible until much later. Steps 1–4 shipped those nets first for exactly that reason, so
step 5 inherited a workspace where a break shows up as a failure rather than as a wrong picture six
weeks later.

| | P1 step | State |
|---|---|---|
| 1 | Numerical parity against Manim (771 cases at 1e-9) | done — see "Commands" |
| 2 | Golden-image suite: per-platform baselines, CI job (eight scenes now) | done — see "Golden images" |
| 3 | Accessibility: structure + full keyboard + throttled announcements | done — see "Accessibility" |
| 4 | Text: `parley` end to end (string → glyphs → screen), bundled CJK, measured font size | done — see "Text" |
| 5 | Formulas: Typst → per-glyph outlines via `skrifa::OutlinePen`, never `typst-render` | done — see "P1 step 5", `rustim-text::formula`, the `formula` scene |

P1's deferred edges were MathML flattening and self-voicing TTS for P5, and **both have landed** —
TTS as P3 step 16 (see "Narration, captions and self-voicing") and the formula structure as P3 step
20 (see "Formula structure: MathML"). Reduced motion was the third, and **P2 step 4 landed it** — one form of it, still: the platform preference is read on Linux only,
so macOS and Windows honour `--motion reduced` and not the system setting (see "Reduced motion" in
the timeline section for what is read and why the rest is not). The CPU cost is
**fill-area bound, not
shape-count bound** (4 large filled polygons at 720p already cost p50 2.25 ms of a 16.7 ms budget), so
damage-rectangle invalidation was likely worth more than any micro-optimisation — and it landed as
P3's damage rectangles (see "Damage rectangles").

The accessibility layer's own deferred edges, so they are not rediscovered as gaps: the macOS/Windows
backends are **compile-and-probe only** — `--a11y-probe` turns "does VoiceOver receive this?" into a
one-command check, but this machine has no accessible bus, so it has not been run on either and
nothing beyond Linux has been observed. Updates are now incremental (only changed nodes), but a
`CameraMove` changes every node's pixel bounds and therefore still sends the whole tree. Motion has a
runtime switch (`m` and the wire command), but the *platform preference* is still read once at
startup: a desktop setting changed while the engine runs is not observed.

> **Updated 2026-10-18.** The macOS half of the first sentence is no longer true: VoiceOver was
> started against the probe on a macOS 26 arm64 machine and **attached** (`1 full, 399919
> incremental`, 5 semantic nodes). Windows is the one still unobserved. The `CameraMove` and
> "preference read once at startup" sentences still hold — macOS now reads the preference, but still
> only at startup. See the macOS entries at the end of this file.

## P1 step 5: formulas — Typst in, per-glyph outlines out

**Landed.** `crates/rustim-text/src/formula.rs` is the *authority* for this step (read it before
touching anything here; this section is the summary). A `$…$` fragment compiles to a
`NodeKind::Formula` mobject holding **one child mobject per glyph**, each child an ordinary filled
path in Manim's control-point layout — so a symbol is a real object with a `NodeId`, not a texture.
`FormulaCompiler` is built once (it reads and parses the font set, which is the expensive part) and
`compile(&str)` returns a `Formula`; `Formula::insert(&mut SceneTree)` returns a `FormulaNodes` of
ids. A bad fragment is `FormulaError`, never a panic.

The research move that preceded it settled two things, and **both came back different from the
tutorial's sketch** (which is aimed at typst 0.11/0.2 — every call in it is stale, and
`typst::eval::World::default()` does not exist: `World` is a trait you implement, handing it a
`Font`/`FontBook`):

- **The crate split, and `typst::PagedDocument` does not exist.** `World`, `Library`, `Frame`,
  `FrameItem`, `TextItem`, `Glyph`, `Font`, `FontInstance` and `Output` are all in **`typst-library`**;
  **`PagedDocument` is in `typst-layout`**. The facade's entire export list is
  `pub use typst_library::*` plus `syntax`/`utils` — it re-exports neither `typst-layout` nor
  `typst-html`. So `typst-layout` is a *direct* dependency and the compile call names it:
  `typst::compile::<typst_layout::PagedDocument>(&world)`. The facade cannot be dropped either:
  `Library::default()` is an `impl` in the facade and it needs the facade's **private** `ROUTINES`
  table. Dependencies run one way — `typst-library` does not depend on `typst-layout`.
- **The exact `World` signature in 0.15.1** — **seven methods, all required, no defaults**:
  `library`, `book`, `main`, `source(FileId)`, `file(FileId)`, `font(usize)`, `today(Option<Duration>)`;
  `#[comemo::track]`-annotated, with blanket impls for `Box<W>`/`Arc<W>`/`&W`. `book`/`font` are
  exactly what `FontProvider` already answers — the mapping the design assumed, confirmed. The
  `World` is composed inside `formula.rs` from **typst-assets** the bundled CJK read straight from
  `BUNDLED_DIR`; `FontProvider` is deliberately not reused, because it holds a fontique collection
  rather than bytes.

Two fields are what make the route work, and neither was in the sketch:

- **`Glyph::span: (Span, u16)`** gives every glyph its source location, so per-symbol addressability
  is handed over by Typst rather than reconstructed. That is the property the whole route exists for.
- **`Font::data() -> &Bytes`**, reached from `TextItem.font` through `FontInstance::font()`, gives the
  raw bytes for `skrifa::FontRef::from_index` — so the frozen `rustybuzz`/`ttf-parser` subtree stays
  inside Typst's own code while this crate keeps its `skrifa`.

Four decisions held, all confirmed by the implementation: **never `typst-render`** (it rasterises
through tiny-skia and throws the per-glyph vectors away); **fonts from `typst-assets` plus
`BUNDLED_DIR`**, never a system path (and `typst-assets` is where the `1.92` MSRV comes from);
**`typst-layout`'s frozen `rustybuzz ^0.20` / `ttf-parser ^0.25` stays inside `formula.rs`**, so the
eventual upgrade (typst#8172, expected in 0.16) is a one-file change; and **no Typst crate reaches
the six wasm kernel crates** — only `kurbo` geometry crosses, into `rustim-mobject`. `typst-layout`
also requires `kurbo = "0.13"`, which the workspace already pins, so the `BezPath` this route
produces is the workspace's own type rather than a second copy.

**What implementing it taught** — the four things that were not derivable from the sources, all
covered by a test or an image (see "Hard-won facts" for the mechanism of each):

1. **A multi-contour glyph cannot survive `points_from_path` unwelded.** `bezier_path_from_points`
   emits exactly one `move_to` (`rustim-mobject/src/tree.rs:456`), so a flat `Vec<Point>` has no way
   to start a second contour — and a glyph's counter (`o`, `d`, `=`, `%`) is a second contour. They
   are welded through a hub with a doubled slit, so every bridge cancels to zero winding and the
   counter stays open under `Fill::NonZero`.
2. **A fraction's rule and a radical's overline are *strokes*, not fills**, and the compiler's
   output is geometry with no per-contour style. `FrameItem::Shape` with `Geometry::Line` is expanded
   into a filled quad by `filled_shape` — a formula with no fraction bar reads as a bug, not as a
   missing feature.
3. **`Glyph::span` is the *element's* span, shared by every glyph of a run.** So it names a glyph
   only when one element produced one glyph; `"abc"` gives all three the whole literal, and the
   guard is a shaped-character-count comparison.
4. **Typst's frame is y-down and its text runs are y-up.** The item's `Point` is a frame-space
   translation; the outline is composed as `state · T(x, −y) · S(s, −s)`. Getting this backwards
   mirrors every glyph about its own baseline — the same trap the text section records, with the
   same appearance.

Deferred, deliberately, and reported rather than dropped: `Formula::skipped_shapes` counts the
`FrameItem::Shape` cases that are neither a rule nor a rect (a `Geometry::Curve` needs a real
stroker) and any `FrameItem::Image`; per-run colour is not carried over (every glyph takes the
engine's colour), link annotations are ignored, and `GroupItem::clip` is not applied — math nests
groups for layout but does not clip them. The macOS and Windows `formula` baselines were owed like
every other scene's — only `linux-aarch64.png` was checked in at the time of this step; the macOS
ones landed on 2026-10-18, so Windows is the one still owed (see "Golden images").

The two commands that catch the failure modes above are
`cargo tree -p rustim-text -i skrifa@0.44.0` and `cargo tree -i ttf-parser` — **and the version is
named on purpose**: a bare `cargo tree -i skrifa` reports *two* versions, because the lock has a
second, older `skrifa` inside Typst's own image subtree (`hayro-interpret`), which is harmless but
reads like a mistake. Both, plus "no Typst crate reaches the kernel" and "`rustim-render` never
depends on `rustim-text`", run as the `typst-subtree` CI job.

## Formula structure: MathML

The geometry is only half of a formula. A glyph tree says what is on the screen and nothing about what
the symbols *are to each other*: `x² + 1` and `x + 1²` are the same marks in the same boxes to a point
array, and the relation lives only in the vertical layout. A screen reader, a Web target and a host
inspecting the scene all need the tree, so `FormulaCompiler::compile` carries Typst's own MathML beside
the outlines: `Mobject.mathml` → `SemanticNode.mathml` → `--inspect`'s JSON.

The route is a second *compile*, not a second parser. `typst-html` is the only public entry point to
the resolved math IR — its lowering function is crate-private and the IR resolver needs compiler
internals — and it is already in the dependency graph through the `typst` facade, so naming it
directly buys the API and no new crate. `FormulaCompiler` therefore holds a second `Library` built
from the same `ROUTINES` with `Feature::Html` enabled (the flag is what `compile::<HtmlDocument>`
checks), and the same `World` answers both compiles. The document is encoded with `pretty: false` and
the `<math>…</math>` element is sliced out of it: the encoder escapes `<` in text content and
`<math>` never nests, so the first `<math` and the next `</math>` are the element.

Four properties are load-bearing:

- **`None` is not an error.** The geometry is the primary product, so a failed HTML pass drops the
  structure and keeps the picture. A formula compiles or it does not; a structureless formula is still
  a formula, and the field's absence is reported rather than hidden behind an empty string.
- **The MathML is Typst's verbatim.** `mi` carries the typeset character (`x` is U+1D465 `𝑥`), while
  the per-glyph labels keep the author's spelling — a deliberate asymmetry: a label is for reading one
  symbol aloud, the MathML is for rendering the expression. Do not "fix" either into the other.
- **The field is absent from the JSON, never `null`** (`#[serde(default, skip_serializing_if)]`) —
  the same rule every absent command parameter follows, because `--inspect`'s output is a thing a
  model reads back.
- **It does not replace the children.** The glyph mobjects stay addressable nodes; the structure says
  what the formula means, it does not flatten the tree.

Two things have to be true of the MathML for it to be worth carrying, and the tests assert both: a
fraction and a radical survive as `<mfrac>`/`<msqrt>` — a flat list could not spell them — and the
string is the element alone, with no `<style>` or `<body>` from the surrounding document.
`crates/rustim-app/tests/formula.rs` runs the real binary's `--scene formula --inspect` and checks the
pairing end to end, because each unit hop can stay green while the scene stops setting the field, or
`--inspect` stops printing it.

- **P2** — more mobjects (lines, arrows, axes, function plots) and animations (`Transform`, `Rotate`,
  `Scale`, `FadeOut`); morph via `align_points`. The animations are all in (steps 2 and 5); of the
  mobjects, lines and arrows are, and **axes, number lines and function plots followed the camera**
  (see the P3 entry below).
  **Step 1, the alignment core, is done**: `rustim_math::bezier::align_points` (with `subpaths`)
  is a literal port of `VMobject.align_points`, covered by twelve cases in a new `align_points`
  group in `manim-geometry.json`. It is deliberately **port-ahead-of-consumer** — nothing calls it
  yet, which is this module's established pattern (`bezier_remap`, `arc_length`, `is_closed` and the
  `get_smooth_*` family are all in the same position), and the `Mobject`-level wrapper is step 2's
  decision because it depends on how `Transform` drives it.
  Three behaviours are worth knowing before touching it, and the module doc is the authority:
  **the result can be shorter than the input** (trailing curves that fold onto the point before them
  are trimmed, in a loop, manim#1959 — so "grow the shorter one" is wrong); **equal point counts
  return untouched before any normalisation**, even when two one-point paths have different points
  (upstream's own `TODO` calls the shortcut over-eager, and it is reproduced rather than tidied); and
  **the point-equality tolerance is relative** — `consider_points_equals` is `np.allclose(p, q, atol=1e-6)`
  with numpy's default `rtol = 1e-5`, so it is a *different constant* from the one `is_closed` uses
  (`1e-8`) and must not be inherited from it. A port that kept only `atol` splits a path into two
  subpaths where Manim keeps one, and the `rtol_boundary_no_split` fixture fails.
  **Step 2, `Transform`, is done**, and it is what makes `align_points` live: the animation is in
  `rustim-anim::ops`, `Style::lerp` (the pure value half of Manim's `VMobject.interpolate_color`) is
  in `rustim-mobject`, and the `transform` scene puts both on screen. The step-1 open question —
  where the `Mobject`-level wrapper goes — answered itself: **the animation *is* the wrapper**. There
  is no `SceneTree::align_pair`, because Manim's `Transform.begin` copies the target into a private
  `target_copy` and aligns against *that* (`animation/transform.py:200-212`), so the target is never a
  scene object at all. Here the target is a free `Mobject` value handed to `Transform::new(source: NodeId,
  target: Mobject)` — the node-target spelling is one `tree.get(b).clone()` at the call site — and the
  source keeps its `NodeId` through the whole morph.
  Four things about it are load-bearing. **Points are local here and world in Manim**, so both the
  point array and the deferred affine are lerped; for a translate or scale delta that reproduces
  Manim's world-space result exactly, and for a *rotation* delta it does not, because interpolating an
  affine componentwise is not interpolating an angle — the same caveat `MoveTo`'s doc already carries.
  Materialising both sides and writing back `transform = IDENTITY` would be "more correct" and would
  throw away the O(1) deferred transform the architecture is built on. **`alpha == 1.0` is an exact
  copy**, reproducing the short-circuit at `types/vectorized_mobject.py:1983-1987`; be precise about
  what that buys — `(1 - 1) * a + 1 * b` already lands on `b` for finite values, so it is a *guard*
  (a non-finite component would otherwise make the style `NaN`), which is why the test that observes
  it poisons a component with `INFINITY`. **A ragged source is a no-op, not a panic** — `begin` has no
  error channel, so the captured state stays `None` and every `interpolate` returns early, matching
  the posture of a command naming a deleted node. And **a plain `Transform` leaves the source wearing
  the target's data**, keeping its kind and label: the donor is never added, so the settled frame
  holds two objects and not four.
  **Not ported, on purpose, and the doc comment there says so:** `align_submobjects`,
  `null_point_align` and the transparent-placeholder padding with its per-child recursion
  (`mobject.py:3149-3227`) — every shape in `rustim-mobject` today is childless with all geometry in
  `points`, so a shape-to-shape morph is fully covered while a *group*-to-group morph would align only
  the top-level array. `path_func` as a user-supplied closure is out — the one built-in it ships is
  `path_along_arc`, which this engine does have (see "Morph routes: `path_arc`"), and an arbitrary
  callable cannot cross a serde command boundary. Sheen (`Style` has no `sheen_direction`) is out for
  the same reason.
  **What the accessibility work learned from it:** a `Transform` moves geometry and says nothing
  about what the object *is*, and it cannot — nothing in the engine knows a triangle has become a
  circle. So the caller has to say it, and the `transform` scene does, once the morph settles. That is
  not a nicety: before the scene said it, `--inspect` described the settled circle as
  "两条直角边长 3.2 和 3.2 的直角三角形，面积 5.12" and the settled triangle as a 2.6-square of area
  6.76 — sizes measured honestly from the geometry, and both false. The kind is invisible in pixels
  (the two renders were byte-identical), so a golden can never catch it; `--inspect` is the tool.
  **Step 3, `play()` — the sequence state machine — is done**, and it is the step that made the
  pipeline above *writable*: `Sequence` (`rustim-anim::sequence`) queues beats, `SceneContext::play` /
  `play_all` / `wait` append them, `SceneContext::advance` runs one at a time, and `Wait` makes a
  pause an animation. Both hand-sequenced scenes were rewritten onto it and their `stage` counters
  deleted; **the six goldens passed without reblessing**, which is the claim that sequencing moved
  and the settled picture did not. Three things are worth carrying forward. **`Command::Play` became
  real** — `SceneContext::handle` intercepts it (the tree-mutating commands stay in the pure
  `command::apply`), `command::animation_for` does the wire→animation mapping and refuses a missing
  node, a `move_to` with no `to`, and a negative or non-finite `duration` (`0.0` is a legitimate
  instant snap; Manim refuses only `run_time < 0`). **`is_playing` is not `!animations.is_empty()`**:
  the scheduler is empty for every frame a beat spends queued — including the frame right after
  `construct` — so the `ANIMATION_DONE` announcement would have fired before the scene began.
  **`AnimationScheduler` gained a per-animation `started_at`**, which fixed a real off-by-one frame
  (see "Core traits"). Deliberately not ported at this step: `AnimationGroup` / `Succession` /
  `lag_ratio`, and interrupting a running beat (a command can only append; "stop that and do this
  instead" is a different, explicit command). Composition landed later — see "Composition" above and
  the P3 bullet below — and it did not change this layer: it is an `Animation`, and the queue is
  still the driver.
  **Step 4, reduced motion, is done**, and step 3 is what made it small: `play` / `play_all` / `wait`
  are now the only route a beat takes to the scheduler, so there is exactly one place to consult the
  preference — `Sequence::start_next`, which is handed a `Motion`. The rule, the four consequences,
  and the Linux-only platform probe are in "Reduced motion" above; the two claims that used to live
  in this file ("unshipped in both forms", "`MoveTo` animates at full speed for every user") are both
  now false and the corresponding sentences have been rewritten. The step's own evidence is that the
  six goldens passed **unchanged** while every scene's still and every scene's `--inspect` tree came
  out identical under the two modes — the journey moved and the picture and the rhythm did not. Its
  deferred edges were the macOS/Windows probes, a runtime toggle and re-reading the preference while
  running. The toggle landed in P3 step 11 (`Command::Motion`, the `m` key, taking effect at the next
  beat started); `--a11y-probe` makes the two probes runnable but they still need those machines;
  and the platform preference is still read once, so a desktop setting changed mid-run is not
  observed.
  **Step 5, the remaining animations — `FadeOut`, `Rotate`, `Scale` — is done**, and the three are
  not variations on one another. **`FadeOut` is the first animation with an effect *after* its last
  frame**: it mirrors `FadeIn`'s opacity ramp and then removes its target, which is what forced
  `Animation::finish` (Manim's `clean_up_from_scene`, `fading.py:184-192`) into the trait, with three
  call sites and the `Box` forwarding trap — see "Core traits" and "Hard-won facts". **`Rotate` is
  the animation `MoveTo` cannot express**: Manim's is a `Transform` with `path_arc` (`rotation.py:155-184`)
  and interpolates the *angle* linearly, so here it freezes `(pivot, start_transform)` in `begin` and
  writes `rotate_about(angle · alpha, pivot) · start`; the counterfactual is that a componentwise
  affine lerp of the same quarter turn lands a corner at distance 1 from the pivot where the arc puts
  it at √2. **`Scale` is `ScaleInPlace`, not `Scale`** — Manim has no `Scale` class (`transform.py:562`)
  — so it is in place about the object's own centre, factor interpolated linearly, and it takes no
  pivot. `Mobject::rotate_about` / `scale_about` compose onto the deferred transform in O(1); the
  pivot lives in the **parent's** space, which is where `critical_point((0.0, 0.0))` already answers,
  so the default pivot needs no inverse — and the world-space pivot `Rotate::about_point` does take
  one, guarded, with a degenerate parent making the animation a no-op rather than writing `NaN`.
  Two shapes came with them, because nothing in `shapes` was interesting to turn: `line` and `arrow`
  were **filled outlines** at the time (a line has zero area and the draw walk emitted only fills),
  anchored at the tail rather than centred, so "rotate about the tail" means what a caller expects.
  `line` has since become a two-anchor stroked centerline; `arrow` is still the filled silhouette,
  because a flat point array cannot be a stroked shaft plus a filled head — see "Strokes". **The
  step's own behavioural change is that
  `set`'s `scale` and `rotation` now compose** onto the object's transform instead of overwriting it,
  pinned by two tests; `position` stays absolute because Manim's `move_to` is.
  **The evidence is a bless run that moved nothing.** All six older scenes rendered *pixel-identical*
  to their committed baselines — the `finish` hook and the composing properties touch running code,
  and no golden noticed — which is what the four stale-byte files in that run turned out to be
  (see "Golden images"). The new `rotate` scene carries the two things no unit test can: a still with
  **two** objects where the script named three, and an `--inspect` tree to match, which is the only
  assertion in the workspace that can see a `finish` that never ran; the scene needs **no `update`
  override at all**, which is itself the claim that removing an object is the animation's business
  and not the script's. **`--inspect` also caught a defect the a11y tests had passed over**: the
  turned arrow was announced as "长度 2.49" rather than 3, because the measurement fell back to the
  bounding box — see "Accessibility" for the diameter rule that replaced it, and note that the test
  now asserts on a *rotated* arrow, since the unrotated one gave the right answer by coincidence.
  Deferred at the time, since landed: **strokes in the mobject model** (see "Strokes") and
  `Create`/`Uncreate` (see "P3's Create/Uncreate"). Deferred on purpose: `Rotating` (an animation
  that never ends makes `is_playing` permanently true, which every "react when the timeline drains"
  scene depends on) and `FadeIn`'s `shift`/`scale`; `path_arc` landed with the P3 morph work (see
  "Morph routes"), and axes and plots landed with P3's camera ("P3's coordinate systems are done").
- **The command feed — the first slice of P3's "AI command surface at full width" — is done**, and it
  is the step that made the engine *drivable* rather than merely scriptable: `--script` applies a file
  of JSON commands or reads stdin line by line, `--reply` answers each one, and a windowed run takes
  the same commands from a reader thread through the second `AppEvent` variant that enum was declared
  to make room for. Nothing in the command layer or the renderer changed — the seven goldens passed
  without a pixel moving, which is the claim that a transport was added outside them. Three things it
  fixed while wiring it: `Scene::on_command` was declared and never called (it is called now, for both
  the feed and the keyboard path); `CommandError`'s `Display` for `NoSuchNode` printed the id's
  **internal** spelling (`NodeId(999v1)`) instead of the wire integer, which is the number a user can
  find in their own script; and `--reply` exposed that `--png` and the frame-rate line both wrote to
  stdout, so `main::note` now routes every informational line to stderr when stdout is protocol. What
  it deliberately leaves for P3+: the query command (no way to read a live tree), label addressing,
  and a merged `--reply`/`--inspect`. See "The feed" under AI integration. **The query command, label
  addressing and stop/interrupt have all since landed** (P3+ steps 22a, 22b and 22c, below), which
  leaves only the merged `--reply`/`--inspect` on that list.
- **P3's camera is done** (the first half of the `axes`/`plot` step). `Viewport` was cut down to the
  window, `Camera` moved onto `SceneTree`, `CameraMove` joined the animation set, and
  `{"action":"camera","center"?,"zoom"?,"focus"?,"duration"?}` joined the protocol — see "The
  camera". The regression evidence is the strongest kind this repository has: the seven goldens
  rendered **pixel-identical with no rebless**, because the default camera reproduces the old
  projection bit-for-bit. The `--inspect` snapshot gained `camera: CameraView` so a host can map its
  pixel bounds back to world coordinates. Pointer pan/wheel zoom was deferred here and **landed as
  P3+ step 23** (see "Pointer input" and the roadmap entry below). Still deferred, deliberately:
  a `Rotating` animation (`duration = ∞` makes `alpha = age/∞ = 0`, freezing it while `is_playing`
  stays true; this needs a scheduler concept for perpetual animations), and any announcement of a
  camera change.
- **P3's coordinate systems are done** (the second half). `ShapeKind::{NumberLine, Axes, Plot}` with
  the `range`/`x_range`/`y_range`/`step`/`tick_size`/`points` parameters, `NodeKind::NumberLine`,
  `AxesScale`, the a11y rules and the wire-round-trip cases all landed, and the `axes` scene put them
  on screen under a `CameraMove` — see "Axes, number lines and plots". **The seven older goldens
  passed untouched; `axes/linux-aarch64.png` is the only new baseline**, so the claim is that the
  three shapes and the camera *added* a picture rather than moving one. Two details in the new
  baseline are deliberate: the parabola is `0.25x² − 1.75` rather than ending at the range's floor
  (an apex at `-2` ran through the y axis's own `-2` label), and samples above `y_range` are dropped
  so the curve ends on the frame. Deferred, deliberately: an expression language (the host samples,
  per decision), axis arrows and subticks, automatic label layout and number formatting, `focus` on a
  group (its bounding box would need descendants), per-tick addressability/styling, and text bounds
  — which is why `focus` still refuses a text node. Pointer pan/zoom was on this list and landed in
  step 23.
- **P3's strokes are done.** `SceneContext::build_draw_list` emits a `DrawOp::StrokePath` beside the
  fill, each pass carrying its own multiplicative alpha, with the width in **world units** so it
  scales with the camera — see "Strokes" for the four properties and the two vocabulary consequences.
  `Plot` moved from a kurbo ribbon outline to a stroked centerline, `Line` from a filled rectangle to
  a two-anchor centerline, and `Arrow` stayed a filled silhouette on purpose. The command layer gained
  `stroke_opacity`, `stroke_color` and a validated `stroke_width`. **The evidence is a bless diff
  with exactly one file in it**: all seven older goldens were pixel-identical (their bytes were not,
  for the known deflate reason — see "Golden images"), and only `axes/linux-aarch64.png` moved,
  because the plot's rasterisation genuinely changed. Deferred: stroke joins/caps/dashes and
  per-segment widths (the style has one width and `vello`'s defaults), a per-mobject stroke style on
  formulas (see "P1 step 5"), and `Create`/`Uncreate`, which is what a stroke makes visible.
- **P3's damage rectangles are done.** `DrawList::damage_since` diffs two frames positionally and
  conservatively, `Renderer::render_damaged` renders only the resulting `PixelRect`, and the app
  blits only that rectangle — see "Damage rectangles" for the mechanism, the `Full` escapes and why
  the region is expressed by shifting the scene rather than the target. The evidence is
  `RenderStats::painted` (the acceptance criterion was stated in repaint *area*, not frame time) and
  pixel-exactness: an incremental frame equals a full render byte for byte, including antialiased
  rotated shapes. All eight goldens passed without reblessing — a still frame is one frame, so the
  feature changed what a frame *costs* and not what it *is*. Deferred, deliberately: damage across
  clip/layer ops (the stacks are positional, so those frames take the full path), a real damage
  propagation from `StateDiff` (the draw-list diff already gives the same answer without trusting
  scene bookkeeping), and multi-rectangle damage (one union rectangle is what the scratch-target
  route can express; several disjoint regions would need several renders).
- **P3's `Create`/`Uncreate` are done** — the first of the remaining-animation items. The math half is
  `bezier::partial_path_points` / `integer_interpolate`, literal ports of `pointwise_become_partial`
  covered by a `pointwise_partial` group in `manim-geometry.json` (56 new cases, 145 in that file);
  the animation half is `rustim_anim::{Create, Uncreate}` and the wire kinds `create`/`uncreate`.
  **There is no new golden, on purpose**: a settled still cannot show a half-drawn path (the fixed
  10 s of virtual time always outlives the animation), so a `create` scene would only re-assert
  "the animation finished" — the evidence is the parity group, the unit tests over the point array,
  and the command-layer tests. `Uncreate` is the second animation whose effect is after its last
  frame (after `FadeOut`), so it removes its target in `finish`; reduced motion is covered because
  `Sequence`'s reduced branch calls `finish` too. Deferred: `Create` over a *group* (each child would
  need its own prefix, which is `align_submobjects` territory), and Manim's per-submobject
  `rate_func` staggering, which is the `lag_ratio` item.
- **P3's `AnimationGroup` / `Succession` / `lag_ratio` are done** — the same `lag_ratio` item, at the
  *animation* granularity rather than Manim's submobject granularity. `rustim-anim::composition`
  holds both kinds, the wire takes `group` / `succession` with nested `animations`, and
  `AnimationSpec::target` became optional because a group is a timeline, not a node (a non-group
  with no target is still a refusal, so the change is a better error, not a looser protocol).
  **No new golden, on purpose**: composition is a scheduling feature, and every static frame it can
  produce is already reachable with separate beats — the evidence is the twelve recording-`Probe`
  tests (which assert *when* each member was begun, interpolated and finished), the command-layer
  tests, and a manual `--inspect` run of “scale, then rotate” through a succession.
  Deferred: per-submobject `rate_func` staggering (the `Create`-over-a-group note above),
  `AnimationGroup`'s `lag_ratio` for a *group of groups* beyond the nesting already allowed, and
  interrupting a running composed animation.
- **P3's absolute position, done properly** — the defect that composition surfaced. A wire
  `move_to` used to build `translate(to)` and hand it to `MoveTo::new`, i.e. it replaced the whole
  matrix: a `succession` of "scale, then move_to" ended at the right centre and the *wrong size*, and
  a `set` of `position` on a scaled object did the same. Both now mean **"put the centre here"**,
  which is Manim's `move_to` — a `shift`, composed onto whatever the object already carries.
  `MoveTo` gained `to_point` beside `new`: the destination is resolved in **`begin`**, not at
  construction, because a succession builds every member before the first runs, so an eager
  translation would capture the pre-scale centre. `new` still takes a whole affine for callers that
  really mean "interpolate to this matrix", and its doc now says so. Evidence: four unit tests
  (`a_move_to_point_keeps_the_objects_scale`, `..._resolves_against_the_state_it_finds`, the
  succession ending scaled and moved, and the `set position` twin) plus the manual `--inspect` run
  that started it — a circle scaled ×1.5 and moved to (1, 0.5) now reports radius 1.5 and the right
  bounds. **No golden moved**, because no compiled scene calls either route on a transformed object;
  the eight baselines are unchanged.
- **P3's group↔group transforms are done** — `TransformGroup`, the morph a group needs and the leaf
  `Transform` cannot give: children are paired by index after the shorter side gains faded padding
  copies, exactly as `align_data` does. `begin` plans, the first `interpolate` materialises (the
  trait's `begin` is read-only — see "Family morphs"), and the donor is a live node read into a
  `Subtree` and never written, so `{"type":"transform","target":A,"into":B}` leaves `B` in the scene.
  Evidence: eleven `family.rs` tests (whole-family motion, midpoint, the 1→2 and 2→5 padding orders,
  faded copies starting at zero, an excess child fading out, leaf↔leaf, nested grandchildren, the
  donor untouched, and the three no-op pairs), eight command tests covering the mapping and the four
  refusals, the round-trip JSON case, and two `--script` runs (`--inspect` shows the source wearing
  the donor's measured size while keeping its own name; a missing `into` is refused under
  `"into"`). **No golden moved**: no compiled scene morphs a family yet, so the eight baselines are
  unchanged. Deferred: `null_point_align` (a shape ↔ group morph, refused rather than approximated).
- **P3's morph routes — `path_arc` — are done**, on both `Transform` and `TransformGroup`. The step's
  finding is that an arc has to be measured in the **parent's space**: Manim's points are world points,
  so arcing the local arrays alone would draw a straight line for "move this there along an arc",
  where the displacement sits in the deferred transform and the local arrays are identical on both
  sides. `path_point` folds each side through its transform (`from_transform · a`,
  `to_transform · b`), arcs there, and writes back with an **identity** transform — the one place the
  deferred-transform architecture is deliberately set aside, and only while an arced morph runs. Both
  ends are written rather than rotated into place (`path_along_arc` at `alpha = 1` lands on the
  endpoint only to within rounding), so a settled morph is exactly its target and reduced motion is
  the full-motion end frame. `rustim_math::paths` is a literal port with 220 recorded cases at 1e-9.
  Evidence: three leaf tests (a *translated* donor bows; both ends are exact; a sub-threshold angle
  draws the same line as no arc), one family twin, two command tests (the angle reaches the morph; a
  non-finite one is refused under `path_arc`), the round-trip JSON case, and the `--script` morph now
  carrying an arc — **with the eight goldens unchanged**, since no compiled scene asks for one.
- **P3's remaining animations — the ~40 rate functions — are done**, and the item was mostly a
  *naming* problem rather than a maths one. `rustim_math::rate` is the literal port (see "Rate
  functions"), `Rate` is the wire vocabulary with `Rate::ALL` behind `--help`, `WithRate` applies an
  override without touching the animation trait, and the wire's `AnimationSpec` gained
  `rate: Option<Rate>` as its last field. The parity fixture is the evidence: **56 recorded Manim
  results** over a 101-point grid, including eleven non-default parameter sets, in the fourth
  fixture file — 771 cases in total. Three curves that end somewhere other than 1 and one that dips
  negative are pinned as behaviour, not treated as errors. Deferred, deliberately: parameters on the
  wire (the enum is Manim's default set; a custom inflection is a Rust-level closure), parameterised
  rates for scene code beyond the explicit functions, and the `@unit_interval` / `@zero` decorators
  (the clamp lives in `Animation::ease`).
- **P3's accessibility debts are paid — text bounds, a runtime motion switch, incremental pushes and
  a real-machine probe — and the eight goldens never moved**, because none of the four steps touches
  drawing. Step 10 gave `Mobject` a declared `local_bounds` fallback, so a text or formula node has
  `bounds`, a centre for `MoveTo`/`Rotate` and a frameable subject for `camera focus`; it is
  consulted only when `points` is empty, and `a11y::build` keeps `Shape::of(anchors)` for
  descriptions so a text node is still announced by its label alone. Step 11 made `Motion` switchable
  at runtime with `{"action":"motion"}` and the `m` key, read once per beat in `Sequence::start_next`,
  so a switch lands on the next beat and never mid-beat. Step 12 turned the per-frame full snapshot
  into a difference, with the consumer's relationship rules pinned by replaying every update through
  `accesskit_consumer`, and fixed a latent panic: a `FadeOut` could delete the focused node and leave
  the projection pushing a focus the consumer cannot resolve. Step 13 added `--a11y-probe` — one
  announcement, 600 frames, and a stderr report saying whether a backend attached; the honest answer
  on this machine is "not attached, 0 pushes", and the report says so rather than pretending.
- **P3's narration, captions and self-voicing are done**, in three steps, and **not one golden
  moved**: without `--captions` or `--speak` the frame loop emits exactly the ops it always did.
  Step 14 added `{"action":"narrate","text","at"}` and gave `SceneContext` a virtual clock, so a line
  can be queued for a beat of the timeline instead of being said the instant the command lands — the
  step's evidence is that `apply` is a no-op, `handle` returns an empty diff, and the clock a window's
  `dt` and a still's 10 s both advance is the same one. Step 15 drew the live region as a caption
  band (`--captions`), including in `--png` stills — see "Narration, captions and self-voicing" for
  the transform that keeps `glifo` hinting — and its integration test asserts the *only* pixels that
  differ from a plain still are in the bottom band. Step 16 added `--speak` on `espeak-ng` / `say` /
  PowerShell's `System.Speech`, with the sentence on stdin, a speaker thread so the frame loop never
  blocks, and the built-in voice yielding to a screen reader that has ever attached. The `.srt`
  export it deferred landed with video export (the next bullet). The user-selectable voice it
  deferred landed as **`--voice NAME`** (see "Narration, captions and self-voicing"): the name is
  the backend's own and is forwarded, never chosen. Still deferred: automatic voice detection (a
  hard-coded `-v cmn` breaks machines without that voice), `piper`, and captions that follow the
  camera or scale with DPI — 24 px is the determinism promise.
- **P3's video export and the visual-regression CLI are done**, and the split is the design:
  `rustim-export` gained the `VideoSink` seam with an `ffmpeg` pipe and a PNG-sequence fallback, the
  `srt` writer, and the `rustim-compare` binary over the existing `compare`; `rustim-app` gained
  `--video` / `--video-png` / `--srt` and one `export_timeline` loop feeding either sink. The claims
  are the loop's: a one-frame export of a static scene is **pixel-identical** to the `--png` still
  (`max == 0`, empty diff box), twelve frames of `rotate` differ from each other and repeat pixel
  for pixel across runs, the cue track is timed on the virtual clock (0.0/0.1/0.2 over half a second
  yields three exact cues), and a missing `ffmpeg` is one line and exit 2 before a frame is rendered.
  **No golden
  moved** — the export path draws through `build_frame`, so it adds a retirement of the timeline and
  not a second picture. Deliberately deferred: audio muxing (the engine has no audio), a damage-based
  export renderer (every frame is a new artifact, so a region render would save nothing), resumable
  or streaming exports, and subtitle styling/RTL.
- **P3's MathML flattening is done** — the structure half of P1 step 5. `FormulaCompiler` compiles
  each formula a second time through `typst-html` and slices the `<math>` element out of the encoded
  document; it rides on `Mobject.mathml`, reaches `SemanticNode.mathml`, and `--inspect` prints it
  beside every `role: "math"` node while omitting it everywhere else. Evidence: three `formula.rs`
  tests (`msup`/`mfrac` survive, `sqrt(2)` gives `<msqrt>` and the root keeps the field after
  `insert`, and the MathML is not the labels — `mi` holds U+1D465 where a glyph label holds `x`), two
  `a11y.rs` tests (the field reaches the node and round-trips; a scene without formulas omits it from
  the JSON), and `crates/rustim-app/tests/formula.rs` end to end over the real binary. **No golden
  moved** — the field is metadata and the draw walk never reads it. The compiler's own deferrals
  (per-run colour, link annotations, `GroupItem::clip`) are unchanged: this step carries structure,
  not style.
- **Web (WASM) is done** — item 11 of the development list, and P7's three acceptance criteria are
  measured rather than asserted. `rustim-scenes` holds `Pythagorean` so the browser runs the scene
  the golden was made from; `rustim-web` holds the target-independent `Session`, the `aria`
  projection and the wasm shell (see "Web (WASM)"); `rustim-render`'s `multithreading` became an
  opt-out feature rather than an assumption. Evidence: `tools/web-check.mjs` in Chrome 151 —
  canvas vs. `--png` identical bounding box with max channel error 1 on 16 pixels, the AX tree
  holding the four figures with measured descriptions and no positions, `role: math` named 公式 on
  the formula page with `<mfrac>`/`<msup>` in the DOM, and the command bridge answering `add` with
  an id and refusing broken JSON; two artifacts at 629 375 B and 14 353 728 B gzip -9; 589 workspace
  tests and both wasm clippy runs clean. Deferred: keyboard/pointer input, font subsetting,
  WebCodecs export (above).
- **P3+ step 22a — the query command — is done.** `{"action":"query","target"?}` answers with the
  `SemanticTree` `--inspect` prints, as a third reply presence beside `diff` and `error` (see "AI
  integration" for the rules). The step's structural change is `CommandOutcome`: `handle` used to
  return a `StateDiff`, and a read has no diff to give, so the two hosts now pass an
  `Applied`/`Queried` value to `reply_line` and cannot disagree about which field answers which
  command. `SemanticTree::subtree` is the targeted half — reachable-set walk over the document's own
  `children`, draw order preserved, `root` moved to the target. Evidence: the round-trip case (both
  spellings), a reply-shape test asserting all three presences and their absences, a context test
  that a query equals `semantic_tree(None)` under a moved camera (so there is one projection, not a
  second read format), a subtree test with a sibling outside it and a deleted id refused, a feed test
  that a query reaches no `on_command`, a browser-session test, and an end-to-end `--script --reply`
  case over the real binary. **No golden moved**: a query draws nothing.
- **P3+ step 22b — label addressing — is done.** Every field that names a node (`set`/`remove`'s
  `target`, `animation.target`, a morph's `into`, `camera.focus`, `query.target`) is now a `Target`:
  a `NodeId` **or** the label the object was created with, in the same field. The type is
  hand-written over serde (`serialize_u64` / `serialize_str`, `deserialize_any`) so the spelling
  survives the round trip and a bad value says what the field wanted instead of an untagged enum's
  "did not match any variant". Resolution is one function, `Target::resolve`, called by `apply`,
  `animation_for` and `camera_for` before they touch anything; `rustim-anim` never sees a label.
  Two new refusals, because a label is not a key: `NoSuchLabel` (distinct from a stale id's
  `NoSuchNode`) and `AmbiguousLabel`, which lists candidates **in draw order** —
  `SceneTree::ids_with_label` walks `draw_order()`, since `SlotMap` order is not a promise.
  Evidence: seven
  command-layer tests (both spellings resolve to one id, unknown/duplicate/dead-id refusals with
  their exact JSON and Display, `set`/`remove` by label, `play` naming both subject and donor by
  label, a camera focus giving the same `Camera` by either spelling), a context test that a query
  can be rooted at a label, a browser-session label query, an end-to-end `--script --reply` run that
  also proves the ambiguous refusal names both ids, and the round-trip fixture gained six label
  cases. **No golden moved**, and the 595 → 604 test count is the nine new tests. Deferred on
  purpose: renaming on ambiguity (a command that rewrites the label would be a mutation nobody
  asked for), and prefix/glob matching — exact match only, because a fuzzy address that resolves is
  harder to explain than a refusal.
- **P3+ step 22c — stop/interrupt — is done**, and it is the item that makes the command surface
  complete: `{"action":"stop"}` drops every queued beat and every running animation, and the picture
  stays exactly where the last frame put it. The mechanism is two `clear`s — the scheduler's and the
  queue's — and what is load-bearing is what they **do not** call: neither runs an animation's
  `finish`, so a `FadeOut` in flight leaves its target in the tree at the opacity it reached, a morph
  leaves its subject part-way, and the next `play` starts from there. That is the difference between
  an interrupt and a fast-forward, and a still cannot see it (a half-faded object and a removed one
  are both nearly invisible), which is why the tests assert on the tree. `SceneContext::stop` is the
  named entry point, `handle` intercepts the command and answers with a present-but-empty
  `StateDiff` for `play`'s reason, and `apply` keeps a no-op arm so a direct caller replays a log
  without a panic. The lecture clock and the narration track are untouched — neither is an animation
  — while `is_playing` goes false, so a scene listening for its timeline to drain sees an ending.
  Evidence: four context tests (freeze at half-way with the next beat starting from the frozen state,
  idempotence, an interrupted `FadeOut` keeping its target against a completed one removing it, a
  queued-but-unstarted beat never running, narration still due after a stop), a command test that the
  host-answered commands are no-ops on a bare tree, a round-trip case for the bare tag, a
  browser-session interrupt through the shared bridge, and an end-to-end script whose `play` + `stop`
  render is **pixel-identical** to the same script without the play — against a counterfactual render
  where the completed fade-out removes the square. **No golden moved**: the command changes what
  plays, not what is drawn. Tests 604 → 611. Deferred on purpose: a *target-scoped* stop ("stop that
  object's animation") — the `Animation` trait exposes no target, so expressing it would mean adding
  one to every animation for a feature the timeline-wide interrupt covers — and interrupting
  narration already queued (a line is content, not motion).
- **P3+ — the AI command surface at full width: done** — query (22a), label addressing (22b),
  stop/interrupt (22c). The only known limit left is that `--reply` and `--inspect` both want
  stdout, which is a transport question rather than a protocol one.
- **P3+ step 23 — pointer interaction is done**, and it is a host-layer step: no kernel, renderer or
  protocol change, and no scene has one more draw op. `Camera::panned` / `Camera::zoomed_about` are
  pure functions in `rustim-math`; step 24 then moved the gesture *state* to
  `rustim_math::gesture` so both hosts share it, leaving `rustim-app::input` with the winit adapter
  only; `main.rs` handles `CursorMoved`, `MouseInput` and `MouseWheel` and pushes `Command::Camera`
  through the ordinary queue. See "Pointer input" for the pan/zoom algebra, why the wheel is
  `exp(k · delta)`, and why the clamp is applied to the factor rather than the zoom. Evidence: three
  `rustim-math` tests (pointer-following, cursor-anchor invariance to `1e-12` over a 4×4×4 grid,
  degenerate inputs) and seven `input.rs` tests (drag state, first-move anchoring, unit-preserving
  scroll adapter, multiplicative zoom, malformed deltas, the clamp's exact limits, and per-frame
  gesture accumulation), plus an **external** real-machine check with `xdotool`: a
  100 px drag over a 1600×900 frame makes the `query` reply's `camera.center.x` exactly
  `-0.888888…`, and one wheel event (`xdotool mousedown 4`) makes `zoom` exactly
  `1.2840254166877414 = e^0.25`, with the cursor's world point unchanged. (`xdotool click 4`
  would zoom twice — see the measurement trap under "Pointer input".) **All eight goldens
  are unchanged**, and the count moved 611 → 621. Deferred: touch/pinch, inertia, right- and
  middle-button gestures, capture outside the window, and click-to-focus.
- **P3+ step 24 — Web pointer interaction is done**, the second consumer of step 23's arithmetic and
  no new notion of a gesture: `rustim_math::gesture` holds `ScrollDelta`, `Drag`, `Pointer`,
  `zoom_factor` and `clamped_zoom_factor` (nine tests, including per-frame accumulation and the
  cursor-anchor invariant); `rustim-web::pointer` adds only the DOM translation — `deltaY`'s sign
  flip, `deltaMode` → unit (`PAGE_LINES = 10`), and `canvas.width / rect.width` scaling — in three
  natively-tested pure functions; `Session::set_camera` routes the result through `Command::Camera`
  and `handle`, so the browser cannot write the view by a path a script lacks; `web.rs` registers six
  listeners and removes them in `Drop`. Evidence: `tools/web-check.mjs` through Chrome's real input
  pipeline — a 200 px drag gives `center.x = -2.2222222222222223` exactly, a `deltaY: -100` wheel
  gives `zoom = 1.2214027581601699 = e^0.2` with the anchor moved `6.3e-16` — plus a natively-run
  session test asserting that a gesture camera moves every projected bound by the drag and that a
  `NaN` zoom is refused without changing the view. **All eight goldens are unchanged** (the browser
  frame still matches `linux-aarch64` with max channel error 1), and the count moved 621 → 628.
  Deferred at the time: pinch gestures and inertial scrolling, and keyboard/focus commands — the one
  interaction the Web version did not promise. Both later: pointer input above, and the keyboard at
  the end of this file.
- **Phase 0 of the external-driver plan is done: protocol v1's foundation.** The session handshake
  (`{"action":"hello","protocol":1}` → a `capabilities` document, `PROTOCOL_VERSION = 1`), the
  vocabulary lists derived from the enums the parser uses (`ShapeKind::ALL`,
  `AnimationKind::ALL`, `PROPERTY_NAMES`, `Rate::ALL`, plus `limits.max_ticks` and the
  `"primitive"` tool profile), the fourth reply presence (`capabilities`, beside `diff`/`tree`/
  `error`, with `CommandOutcome::Greeted`), the new `rustim-client` crate (`read_reply` +
  `StdioClient`), and `crates/rustim-app/tests/conformance.rs` — eight tests that drive the real
  binary **only** through the client crate. Evidence: 631 → 649 tests, clippy and fmt clean, all
  fourteen golden cases pass with **no baseline changed** (a handshake draws nothing). The design
  and the remaining phases (wire text/formula, `Animate`, pause/seek/undo/checkpoint, layout,
  linter, daemon/MCP) are in `贾维斯集成方案.md`; the wire itself is `驱动协议v1.md`, and
  `使用说明.md` §4 documents the handshake for a user. Deliberately not done here: label addressing
  already existed (step 22b), and no second transport, no MCP surface, no checkpointing — those are
  later phases and the documents say so. Four of those have since landed — checkpointing/undo,
  the `hello` teaching profile, the WebSocket/JSON-RPC transport (`rustim-daemon`) and the MCP
  adapter (`rustim-mcp`) — see the Phase 1 bullets below.
- **Phase 1's first slice — wire-level `text` / `formula` / `group` — is done.** `add` now accepts
  the three kinds that made a scripted lecture impossible: a sentence, a Typst formula and a group
  that adopts existing nodes by `Target`, all through the ordinary command feed. The structural
  change is `rustim_scene::typeset::Typesetter` — a trait with no fonts in it, installed on
  `SceneContext` by the host — so the kernel still compiles to wasm with neither parley nor Typst,
  and `hello`'s shape list is filtered by what the installed typesetter can actually build (11 on
  the desktop, 9 on the plain browser page, 10 on the formula page). `handle` intercepts the two
  shaped kinds and `command::apply` refuses them with `Unsupported`, because a pure tree function
  cannot typeset; `TypesetError::Invalid` maps to `OutOfRange { property: "text" }` and `Failed` to
  the new `Typeset` refusal. A wire text node stores a `TextBlock` in `Mobject::text` and
  `build_draw_list` draws it through `rustim_scene::text::text_ops` (now in the kernel, so a
  compiled scene's text and a scripted `add` produce the same ops); a wire formula is inserted as a
  subtree and every compiler-created node takes the command's colour, with `label` / `position` /
  `scale` on the root. The desktop provider (`rustim_app::typeset`) is lazy and caches a missing
  font directory as a value; the browser's (`rustim_web::typeset`) is formula-only, under the
  `formula` feature, from `typst-assets`. Evidence: six `context.rs` tests (no typesetter → refusal;
  half a typesetter advertises and refuses exactly that half; the draw walk's glyph run and the
  reinstated frame transform; a formula's coloured children; a refusal keeps its kind; `children`
  on a shaped kind names the field), five command-layer tests (group adoption, created/modified,
  draw order, the two refusals, an empty group), three `AppTypesetter` tests, two `WebTypesetter`
  tests, two feature-gated browser-session tests, and manual end-to-end runs: `--inspect` shows a
  wire text node with measured bounds and a wire formula with its `<math>` MathML and glyph
  children, `--png` renders both, and setting a group's position moved both children's bounds by
  exactly 90 px. **All eight goldens are unchanged** — no compiled scene uses wire typesetting —
  and the count moved 649 → 677. Deferred, deliberately: text styling on the wire (size and
  `max_width` only; no font family, weight or colour per run), formula styling per element, and a
  browser text path (the embedded font set has no CJK face).
- **Phase 1's second slice — the property animation `animate` — is done.** `play` gained an
  eleventh kind whose payload is `set`'s own `Property` object: `{"type":"animate","property":
  {"name":"opacity","value":0.35}}` eases one style channel from the value the object has when the
  beat begins to the value named. The kernel half is `rustim_anim::Animate` over a five-variant
  `StyleProperty` (`opacity`, `fill_opacity`, `stroke_opacity`, `stroke_width`, `stroke_color`);
  `Animate::begin` reads the start off the node, `interpolate` lerps with Manim's
  `(1 - t)·a + t·b`, and `alpha == 1.0` assigns the target verbatim — so reduced motion and a
  completed run settle on the same bits. Colour blends per channel in the sRGB encoding `Srgb`
  holds, deliberately not through `Style::lerp` (which would touch every other style field, and
  ulp drift compounds frame over frame). `Property::animatable` is the wire-side gate: it validates
  the value through the same `unit_range` / `width_range` helpers `set` uses, and refuses the five
  non-paint properties with an actionable `OutOfRange` naming the right tool — `position` →
  `move_to`, `scale` → `scale`, `rotation` → `rotate`, `label` / `kind` → instantaneous `set`.
  **Opacity does not inherit** in the draw walk, so animating a group's opacity is invisible unless
  the group has geometry; the doc on `Animate` says so rather than changing the draw walk. Evidence:
  six `rustim-anim` tests (per-channel motion, exact endpoints, colour blend, missing target is a
  no-op, a non-finite end does not poison the style, interruption composes), four command-layer
  tests (the channel moves, missing `property` is refused, the five refusals name their field,
  values validate as `set` does), two round-trip fixtures, a conformance arm that accepts the
  advertised kind, and an end-to-end `--script` still where a faded square is pixel-identical to a
  blank scene while the counterfactual render differs. `AnimationKind::ALL` is now 11, and the
  capabilities document therefore advertises 11 animations; **all eight goldens are unchanged**
  (no compiled scene uses it), and the count moved 677 → 688. Deferred, deliberately: geometry
  properties in one beat (use the dedicated animations), non-linear channel curves per property
  beyond `rate`, and animating a group's inherited appearance (children must be addressed, or
  `fade_out` used).
- **Phase 1's third slice — the playback controls `pause` / `resume` / `seek` / `tempo` — is
  done**, and they are the first commands in the protocol that act on the **clock** rather than on
  the tree. `SceneContext` gained `paused: bool` and `tempo: f64` (private, with
  `pause()`/`resume()`/`is_paused()`/`tempo()`/`set_tempo()`/`seek()`); `advance` now returns
  early when paused and otherwise calls a private `advance_clock(dt * tempo)`, which is the old
  body verbatim. The split is what lets `seek` drive the timeline without temporarily touching the
  pause flag: it steps `advance_clock` on a 1/60 grid, so the beats it crosses are played and
  `finish`ed exactly as if the wall clock had reached them — a crossed `fade_out` really removes
  its target — and then assigns `self.clock = to` so the landing point is exact rather than
  accumulated. Four behaviours are decisions. **A pause freezes both clocks** (the scheduler's and
  the lecture/narration clock) but does **not** touch `is_playing`, so a scene watching for its
  timeline to drain sees a stall and not an ending; `stop` deliberately leaves the pause in place
  (a viewer's hold is not content's to undo), and `reset` returns to playing at 1×. **Seek is
  forward-only** — a backward jump needs the state a beat already destroyed, which is what
  `checkpoint`/`undo` is for — and is capped at `MAX_SEEK_SECONDS = 600.0` so one command cannot
  spin the scheduler for an unbounded time; `to` is lecture time, so `tempo` does not rescale it,
  and a paused timeline can seek (dragging the scrubber). Narration the jump crosses is dropped
  rather than spoken in a burst. **Tempo is clamped to `0.05..=16.0`** by `set_tempo` (a
  programmatic caller) and refused outside it on the wire (`out_of_range` under `factor`); `0` is
  `pause`, not a tempo. Evidence: 15 `playback_control_tests` in `context.rs` (freeze, a `play`
  accepted during a pause waiting for `resume`, `is_playing` during a pause, `stop` not lifting it,
  both clocks scaled by tempo, the refusal's field and range, programmatic clamping, `reset`,
  seek finishing crossed beats, exact landing, forward-only, impossible targets, narration crossed
  vs. ahead, seeking while paused, and the empty diff), one conformance arm (all four answer with
  empty diffs, and backward `seek` / zero `tempo` are refusals naming their field), and three
  `--script` end-to-end stills: a paused `fade_out` is pixel-identical to never playing it, a
  `seek` past the pause finishes the fade and removes the square, and `tempo 0.05` leaves the
  square mid-fade at 10 s where full speed has removed it. **All eight goldens are unchanged** —
  the controls change what time does, not what a settled frame is — and the count moved 688 → 707.
  Deferred, deliberately: backward seek (checkpoints), pause/position in the semantic tree (they
  are session state, not nodes), per-beat tempo ramps, and stepping frame-by-frame from the wire.
- **Phase 1's fourth slice — `checkpoint` / `undo` — is done**, and it is the command pair that makes
  `seek`'s forward-only limit acceptable: a session can mark where it was and come back. A snapshot
  is the tree *with its camera*, the clock, the pause, the tempo and the motion mode; the stack holds
  at most 16 with the oldest dropped. `undo` **interrupts** rather than rewinds — the queue and the
  scheduler are cleared without `finish`, for `stop`'s reason and because a captured `begin` refers
  to a tree the restore replaces — and answers with a **non-empty** diff, the only host-answered
  command that does: `created`/`removed`/`modified` are computed by comparing the two trees with
  `Mobject::differs_from`, which excludes the `dirty` bookkeeping so a no-op write is not a change.
  The step's structural change is the anti-ABA set: `SceneTree: Clone` plus `retired: HashSet<NodeId>`,
  because cloning slot versions would otherwise let a restored tree reissue the ids of the future it
  abandoned — `insert` retries past retired ids, `retire_ids_of` records them at undo, and `clear()`
  clears the set because a reset is not an undo. `CommandError::NoCheckpoint` is a *state* error, kept
  apart from `OutOfRange` the way `NoSuchLabel` is kept apart from `NoSuchNode`. Deferred,
  deliberately: redo (a second stack with its own invalidation rules), named checkpoints,
  snapshotting the animation timeline itself, unwinding narration already spoken, and undoing across
  a `reset` (there is nothing left to undo into). See "AI integration" for the semantics a driver
  sees.
- **Phase 1's fifth slice — the list commands `batch` / `validate` and the `teaching` profile — is
  done**, and it is the step that turns "a scene build" from a round trip per step into one plan per
  command. Both are `handle`-intercepted because a list may contain commands only the context can
  run (`play`, `camera`, `undo`), and the reply machinery is shared with everything else: `batch`
  returns the **net** diff (`diff_trees`, the function `undo` now uses too — one notion of "what
  changed", not a second copy), `validate` returns an empty one. The design's load-bearing choices:
  **not a transaction** — a refusal stops at `InBatch { index, cause }` with the prefix applied, so a
  host wanting atomicity takes a checkpoint; **shape checked first** — empty lists and
  `hello`/`query`/`batch`/`validate` members are `OutOfRange` under `commands`/`commands[i]` before
  anything runs; and **`validate` is a real run on scratch state** — same `handle`, same order, tree /
  camera / clock / pause / tempo / motion / checkpoint stack copied, the host's `Typesetter` moved in
  and back, scheduler and narration track deliberately not copied. `capabilities` grew `actions` (19
  verbs, from `Command::action_name`, asserted against the round-trip fixtures' action set) and
  `profiles: ["primitive", "teaching"]`, advisory beside the exact list. Evidence: nine
  `playback_control_tests` in `context.rs` (net diff, stop-at-index with the prefix applied, atomic
  shape refusals, a queued beat inside a batch, the dry run's identical refusal, label
  interdependence, a checked-but-unqueued `play`, the copied checkpoint stack, and the typesetter
  moved in and back), one conformance arm over `rustim-client`, two `--script` tests (a batch renders
  **pixel-identical** to its members written out, and a refused batch names its member while the
  feed continues), and the fixture-set assertion in the round-trip test. **All eight goldens are
  unchanged** (a list draws nothing on its own), and the count moved 722 → 734.
- **Phase 1's sixth slice — the event stream — is done**, and it is the slice the daemon needs: a
  request/response client cannot see a beat start or end, and polling `query` is both late and
  wasteful. `rustim-scene/src/event.rs` holds the vocabulary, `SceneContext` records into a bounded
  queue and emits from `advance_clock`, `Sequence::start_next` returns `StartedBeat` so the start
  event carries the script's own beat shape, and `{"action":"events","enabled":…,"kinds"?}` joins the
  protocol (19 actions; `capabilities.actions` grew with it). The design decisions are in "Events:
  what the timeline did": per-beat `animation_done`, stop is not a completion, events are a log
  rather than state, `frame_ready` is host-produced, and subscription defaults off so a
  strictly request/response host never reads an unexpected line. Evidence: kernel tests in
  `context.rs` (start/end pairing and timing, reduced motion reporting the script's beat, a stopped
  beat never completing, the filter refusing an empty list and suppressing kinds without stopping
  the drain, `reset` keeping the subscription, a hand-added animation not being a beat), two
  `feed.rs` tests (lines written beside replies, unsubscribed drain), and the end-to-end
  `a_subscribed_script_receives_beat_events_and_a_frame_ready` over the real binary. **All eight
  goldens are unchanged** — an event is a notification, not a draw op — and the count moved
  734 → 745.
- **Phase 1's seventh slice — `rustim-daemon` — is done**, and it is the transport the event stream
  was built for. A new crate, not a mode of `rustim-app`: `rpc` (the JSON-RPC envelope and its eight
  error codes), `session` (`Hub`, `SessionHandle`, `AttachSpec`, `ClockMode`; one OS thread and one
  `current_thread` runtime per session, because `SceneContext` is not `Send`), `typeset` (a third
  `Typesetter`, parley + Typst built lazily and failures cached), `server` (accept loop,
  per-connection dispatch and event forwarder) and a binary with `--listen` / `--scene` / `--clock` /
  `--no-typeset`. The design — why thread-per-session, why the attach handshake waits for
  `construct`, why `advance` writes the clock watch before its reply, why the subscription is session
  state, why a kernel refusal stays a `result` — is in "The daemon". Evidence: **12 socket-level
  integration tests** in `crates/rustim-daemon/tests/daemon.rs` over a real loopback WebSocket
  (create-and-drive, not attached `-32001`, reconnect seeing prior objects and a constraint mismatch
  refused, kernel refusal as `result` with the id normalised to `raw()`, four transport error codes,
  manual vs auto clocks, one event stream for two clients, `close` for everyone, notification applied
  but unanswered, independent sessions, `hello` matching `--no-typeset`, text typeset over the
  socket), 18 unit tests over the envelope/session/typesetter and 4 over the binary's flags. **All
  eight goldens are unchanged** — the daemon draws nothing — and the count moved 745 → 780.
  Deliberately deferred: authentication (loopback is the boundary), persistence and resumable
  sessions, audio scheduling for `narrate`, a target-scoped stop, and any rendering/`frame_ready`.
- **Phase 1's eighth slice — `rustim-mcp` — is done**, and it is the transport that lets a model
  drive the engine without a bespoke client. A new crate with a binary: newline-delimited JSON-RPC
  2.0 on stdio (MCP 2025-11-25, accepting 2024-11-05 / 2025-03-26 / 2025-06-18 as well), five thick
  tools — `rustim_capabilities` / `rustim_batch` / `rustim_validate` / `rustim_query` /
  `rustim_advance` — and **no session machinery of its own**: the binary embeds
  `rustim_daemon::Hub`, attaches one session and blocks on `SessionHandle` from a current-thread
  runtime, so the three transports meet in one `SceneContext::handle`. The design decisions are in
  "MCP: the same protocol behind tools" — thin framing rather than a second protocol, a tool
  catalogue that is the engine's own `hello` document, the two failure vocabularies (a kernel
  refusal is an `isError: true` result whose body is protocol v1's own refusal document; malformed
  *arguments* are JSON-RPC `-32602`), and the deliberate non-goals (no rendering/snapshot, no event
  forwarding, no model state). Evidence: 12 unit tests over the envelope and tool translation (the
  handshake and version negotiation, the catalogue, the batch/query round trip, a refusal that
  keeps the session alive, malformed arguments that touch nothing, notifications never answered,
  the clock), and **11 real-process integration tests** in `crates/rustim-mcp/tests/mcp.rs` that
  launch the binary and speak the wire over its pipes — handshake and catalogue, capabilities being
  the engine's own `hello`, a batch building a scene and a query reading it back, an engine refusal
  as a tool result with the process still answering, validate changing nothing, manual vs auto
  clocks, the protocol error codes, notifications unanswered, CJK text and a Typst formula
  typeset through the pipe, `--no-typeset` reporting the honest profile and refusing text, and
  `--scene` honoured. **All eight goldens are unchanged** — the adapter draws nothing — and the
  count moved 780 → 803.
  Deliberately deferred: MCP resources, prompts and sampling (tools are the whole surface); SSE and
  streamable-HTTP transports (stdio only, per `贾维斯集成方案.md`'s local-first rule); forwarding
  the event stream as MCP notifications; and any rendering or frame snapshot — a host that needs
  pixels uses the daemon or `--png`.
- **Phase 2's first slice — the teaching tier's layout — is done.** `rustim-director` now exists,
  with `layout`'s three operations and nothing else: `next_to`, `arrange`, `fit`, pure functions over
  a `SceneTree`. The design decisions are in "The teaching tier" — world-space boxes measured from
  `world_bounds`, placement composed onto the deferred transform, the delta converted into the
  node's parent space via the new `SceneTree::to_parent_space` (the determinant guard `ops::Rotate`
  already carried, now public), and every refusal a value. The step touched `rustim-mobject` once
  (that method) and added a crate; no wire action, no host, no scene. Evidence: fifteen layout tests
  over real trees, including a child of a **scaled** parent (the parent-space conversion) and a
  rotated square placed by its 2√2 world box with its rotation intact. **All eight goldens are
  unchanged** — nothing draws through this crate yet — and the count moved 803 → 818.
- **Phase 2's second slice — the layout actions on the wire — is done.** `next_to` / `arrange` /
  `fit` are now protocol actions (`ACTIONS` 19 → 22) implemented as pure `apply` arms in
  `rustim-scene`, which depends on `rustim-director` **directly** — the earlier `Typesetter`-style
  trait seam was dropped, because layout is pure tree geometry with one implementation and the
  director's dependency set keeps the wasm kernel clean. Two hardening rules came with the wire: a
  `next_to` whose reference is *inside* its target is refused (the gap is unreachable; the reverse
  nesting stays legal), and bulk placements check every target before the first write. The arrange
  default is `0.25` (`MED_SMALL_BUFF`), not the planned `0.5`. `compose` is not a verb — it is a
  `batch` containing these and primitive commands. Evidence: two new director tests, four `apply`
  tests, five round-trip fixtures, and an end-to-end conformance case that arranges three labelled
  squares and reads the pixel gaps back through `query` — plus the six-crate wasm build passing
  with the new edge. **All eight goldens are unchanged**, and the count moved 818 → 826.
- **Phase 2's third slice — attention (`point_at` / `highlight`) — is done.** The director gained
  `attention`: `pointing_arrow` places a filled arrow's tip a `buff` outside the subject's world box
  and runs the shaft outward, and `spotlight` names the picture-bearing nodes outside the subject's
  subtree in draw order. The split from layout is deliberate — the geometry is pure tree measurement,
  but the dim-and-restore beat needs `rustim-anim`, so `rustim-scene` composes it as a
  `Succession(AnimationGroup(dim), Wait(hold), AnimationGroup(restore))` and the director stays
  wasm-clean. `point_at` is a pure `apply` arm answering `diff.created`; `highlight` is intercepted
  by `handle` and answers an empty diff, because it queues a beat rather than mutating the tree. The
  restore target is the opacity captured when the beat begins, not `1.0`, so a partly transparent
  scene comes back unchanged. Defaults: buff `0.1`, length `1.0`, dim `0.25`, duration `0.4`, hold
  `0.5`. Evidence: four director tests, two `apply` tests, three context tests, four round-trip
  fixtures and an end-to-end conformance case; `ACTIONS` 22 → 24. **All eight goldens are
  unchanged**, and the count moved 826 → 836.
- **Phase 2's fourth slice — annotation (`annotate`) — is done.** The third attention verb, and the
  first teaching action that creates **two** nodes: a typeset label and, unless `arrow: false`, the
  arrow that points at the subject. `attention::callout(tree, subject, label_extent, style)` is pure
  geometry — the label's near edge sits a `buff` past the arrow's tail, so the typesetter's own box
  decides how far out the label goes — and `SceneContext::handle` owns the arm, because a label
  means typesetting and `apply` may not read fonts. Geometry is computed before either node is
  inserted, so every refusal is atomic; `created[0]` is always the label, and `label` names the text
  only. A build with no typesetter answers `Unsupported { feature: "text" }` and creates nothing.
  The placement is **static** on purpose: following a moving subject would need a per-frame
  constraint the tree does not have, and parenting the label would rotate it with the subject — so
  re-sending `annotate` is how a host re-anchors one. Evidence: three director tests, three context
  tests, two round-trip fixtures and the extended conformance case; `ACTIONS` 24 → 25. **All eight
  goldens are unchanged**, and the count moved 836 → 842.
  Deliberately deferred at the time: labels that follow their subject, the remaining `explain.*`
  templates, the linter and `ScenePlan`. The templates, the linter and `ScenePlan` have all since
  landed; only following labels remain deferred.
- **Phase 2's fifth slice — the first template, `explain.equation` — is done.** The `explain.*` thick
  tools now exist as a layer (`rustim-scene::templates`), and the first one compiles a formula, styles
  it, measures its world bottom and hangs an optional typeset caption one `DEFAULT_GAP` below it, all
  inside a `NodeKind::Group` whose `local_bounds` is the union of the pieces. `created = [group,
  formula root, caption]`; a formula's glyphs stay its own subtree and are not reported. The group is
  the address for the whole explanation (so `next_to` / `camera focus` / `play` work on it), while the
  pieces remain individually addressable. Refusals validate before insertion and roll the group back
  on a late typesetter error, so a failed template leaves no half an equation. The arm lives in
  `SceneContext::handle` because compiling needs the host's `Typesetter`; `apply` keeps a no-op arm
  so a replayed log passes through. `ACTIONS` 25 → 26. Evidence: two context tests, a rollback test
  over a typesetter that fails only on the caption, two round-trip fixtures and a conformance case;
  **all eight goldens unchanged**, count 842 → 846.
- **Phase 2's sixth slice — the second template, `explain.plot` — is done.** It is the template layer
  applied to a function instead of a formula, and it settled two things the first template did not
  have to: how a function is spelled without an expression language (coefficients, Horner, degree cap
  16), and how a curve that leaves the window is drawn (several `Plot` pieces, split at interpolated
  range crossings, because a flat point array cannot express a subpath break). The axes, the samples
  and the tick labels go through one `AxesScale`; the labels are host-typeset and mandatory, so the
  command needs `supports_text()` even without a caption; zero is skipped on both axes and every
  label is named by its number, which makes a shared value ambiguous to address by label. The caption
  is measured off the lowest *piece* — often a y label's line box rather than the axes' edge, a
  distinction the first test got wrong. `created = [group, axes, plot…, x labels, y labels,
  caption?]`. `ACTIONS` 26 → 27. Evidence: six unit tests over the pure half, three context tests
  (geometry and declared box, a two-run curve, atomic refusals including the late `y_range` one), two
  round-trip fixtures and a conformance case through the real binary; **all eight goldens unchanged**,
  count 846 → 856.
- **The `polygon` shape is in the kernel**, added as the prerequisite for `explain.geometry` and
  useful on its own: an arbitrary triangle, parallelogram or hexagon had no spelling, because every
  other kind derives its points from a size. `ShapeKind::Polygon` (twelfth shape) reads
  `ShapeParams::points` as world-space vertices in order, closes the loop through
  `bezier::points_as_corners` (a repeated first vertex is dropped, not drawn as a zero-length
  segment), and is a **filled contour** like the other closed shapes. Three vertices are the minimum:
  fewer, or any non-finite coordinate, answers with no points in `build`, and the `add` arm refuses
  the same cases under `points` with a sentence naming which one — an invisible object that reports
  success is the answer this layer exists to avoid. `NodeKind::Polygon` takes the `Image` role and
  the description **3 条边的三角形，面积 6** (or *n* 边形), measured from the corner set by the
  shoelace formula rather than from the box, which for a turned figure would overstate the area.
  Evidence: two kernel tests over the vertex walk and the style, one command test over the three
  refusals, the sentence pinned beside the other kinds, a round-trip fixture, the capabilities count
  (`ALL` 11 → 12) and the conformance probe; **all eight goldens unchanged**, count 856 → 859.
- **Phase 2's seventh slice — the third template, `explain.geometry` — is done**, and it is the one
  template whose subject is *not* a mathematical object the engine already knew: a triangle is a
  vertex list, and everything a lesson puts around it is the template's job. Four decisions are
  load-bearing. **The vertices are centred on their bounding box** before anything is built, so
  `position` names the figure's centre (the convention every sized shape follows) and `camera focus`
  frames the figure rather than a corner. **The right angle is measured**, by the dot product of the
  two adjoining unit edges against `1e-3`; the marker is a stroked square in the figure's own frame,
  built by the same `Polygon` builder with the fill switched off and `kind = Unknown` so a reader
  hears "an image", not "a four-sided figure with an area". **Labels are placed by measurement**:
  an edge label sits on the outward normal (whose sign comes from the shoelace area, so either
  winding works) pushed clear by the label's own support function plus `0.12`; a vertex label is
  pushed away from the centre the same way. **A bare figure needs no typesetter** — unlike `plot`,
  whose tick labels are mandatory — so the template takes `Option<&mut dyn Typesetter>` and a build
  without one draws the figure and refuses only a labelled call, under `text`. Collinearity is
  refused by cross products rather than by signed area, which would wrongly accept a bow tie.
  `created = [group, figure, marker…, side labels, vertex labels, caption?]`. `ACTIONS` 27 → 28.
  Evidence: six unit tests over the pure half (centring, the winding-independent normal, right-angle
  detection, collinearity, the push distance, the marker quad), four context tests (the group with
  its placed marker, side and vertex labels and caption, the bare figure without a typesetter, ten
  by-field refusals plus three constructed ones with the tree unchanged, and a late-typesetter
  rollback), two round-trip fixtures, and a conformance case through the real binary;
  **all eight goldens unchanged**, count 859 → 870.
- **Phase 2's eighth slice — the fourth template, `explain.matrix` — is done**, and it is the first
  template whose subject is *text all the way down*: a matrix is a measured grid of typeset cells, so
  unlike `geometry` it has no bare spelling — a build with no text typesetter refuses every call under
  `text`. Four decisions are load-bearing. **A cell is plain text, not a formula** (`Cell::Number` /
  `Cell::Text`): a matrix entry is normally a number or a word, a symbolic entry (`x^2`) renders
  literally, and a lesson that needs typeset algebra uses `add` of `formula` — there is no second
  typesetting path inside the template. **Column widths and row heights are measured** as the widest
  cell per column and the tallest per row, so the caller never pads strings to make a grid line up and
  unequal cells cannot collide. **The four bracket spellings each use a shape the kernel already
  had**: `square` is one filled 8-corner L outline (`Polygon`, `kind = Unknown` so it is announced as
  an image rather than an area), `bars` is a stroked `Line` turned a quarter turn about its own tail,
  `round` is a sampled `Plot` half-ellipse, `none` emits no node — no new kernel shape was added for
  a bracket. **Every cell is addressable and labelled by its own text**, so a table with two `1`s is
  `ambiguous_label` by label exactly as a shared tick value is in `plot`; the caption keeps no label
  and is addressed by id. The round bracket cost one real bug the test caught: its `centre_x` is
  where the arc *opens*, not the ellipse's centre, and passing the latter put the bracket a whole
  semi-width too far from the grid — the bounds assertion is what saw it.
  `created = [group, brackets…, cells row-major, column labels, row labels, lhs?, caption?]`, and
  `ACTIONS` 28 → 29. Evidence: two unit tests over the pure bracket outlines, six context tests (the
  bare square-bracket layout with its declared box, the full-slot grid with unequal columns and every
  hanging label, the four bracket spellings, eleven by-field refusals plus four constructed ones with
  the tree unchanged, no-typesetter refusal, and the late-cell rollback), two round-trip fixtures,
  and a conformance case through the real binary that reads back the bracket and cell names, asserts
  the duplicate-cell `ambiguous_label`, and proves a ragged row leaves no group behind;
  **all eight goldens unchanged**, count 870 → 879.
- **Phase 2's ninth slice — the fifth template, `explain.chart` — is done**, and it is the first
  template that draws a *measured* picture: a bar chart's bars are only meaningful against the scale
  the axes declare, so the template owns the window, the step, the baseline and every label's
  position. Four decisions are load-bearing. **The baseline is always zero, and a window without zero
  is refused**: a bar chart drawn against a truncated axis distorts every ratio at once while looking
  perfectly healthy, which is the one class of wrong picture a reader cannot catch — so
  `value_range` excluding zero is an `OutOfRange` under `value_range`, and an explicit window must
  also contain every value. **Negative values grow downward and zero still gets a node**: bars run
  from the zero baseline to their scaled value, a zero gets a `DEFAULT_THICKNESS` hairline rather
  than being skipped, so every category is always addressable and animatable. **The value axis is a
  `NumberLine` turned a quarter turn** (ticks outward, `+π/2` so values rise and ticks stand to the
  left), which means the axis is the same shape and `kind` an `add` produces, and its numbers are
  drawn by the template right-aligned outside the shaft — zero included, because a bar chart is read
  against its baseline. **A bar is labelled by its category and a label by its own text**, so
  `set target "二"` moves the second bar while a repeated tick or value is `ambiguous_label`; the
  category labels deliberately carry no label, so the bar is the unambiguous address under each
  column. The automatic step is a `nice_step` of `1/2/5 × 10ⁿ` chosen for about five labels, and the
  step is then validated the same way an explicit one is (at least two labels, no more than
  `MAX_LABELS`). `created = [group, value axis, category axis, bar…, value label…, value-axis tick
  label…, category label…, caption?]`, and `ACTIONS` 29 → 30. Evidence: one unit test over
  `nice_step`, five context tests (the bare three-category chart with its baseline, bar geometry and
  label rows, a negative bar with an explicit window and position, eleven by-field refusals plus
  three constructed ones with the tree unchanged, no-typesetter refusal, and a late-typesetter
  rollback), two round-trip fixtures, and a conformance case through the real binary that reads back
  the axis names, the category names and a negative value's label, asserts a bar is addressable by
  its category, and proves a window without zero leaves no group behind; **all eight goldens
  unchanged**, count 879 → 886.
- **Phase 2's tenth slice — the sixth and last template, `explain.algorithm` — is done**, and it is
  the first template that is a *snapshot* rather than a diagram: one frame of an algorithm's state,
  a row of numbered cells with optional highlights and named pointers, meant to be sent again for
  the next step. Four decisions are load-bearing. **Indices are zero-based and the cells carry the
  same number in their labels** (`格0`, `格1`, …): an algorithm's own vocabulary is `a[0]`, and a
  frame numbered `1..n` would disagree with every line of code the lecture is explaining — the one
  place the teaching tier deliberately does not read like prose. **A pointer is a downward filled
  triangle plus its name, and the name is the label**: the caret is a `Unknown` decoration so it
  does not masquerade as a figure in the accessibility tree, and `set target "i"` resolves to
  exactly one node; two pointers over one column stack upward one per line instead of overlapping.
  **A highlight is the same outline plus a translucent wash** (`fill_opacity 0.35` in `color`,
  defaulting to the foreground), so the grid's edges stay one continuous rule whether a cell is
  highlighted or not. **The frame needs text unconditionally** — every value and every index is
  typeset — so like `explain.matrix` it refuses `unsupported` rather than drawing empty cells.
  `created = [group, (cell, value text, index text)…, (caret, name)…, caption?]`, which makes cell
  `i` `created[1 + 3i]`, and `ACTIONS` 30 → 31. Evidence: five context tests (the bare frame's
  column pitch, cell/label geometry and declared box, the stacked-pointer frame with highlight
  opacities, caret shapes and caption placement, eight by-field refusals plus five constructed ones
  with the tree unchanged, no-typesetter refusal, and the late-typesetter rollback), two round-trip
  fixtures, and a conformance case through the real binary that reads back the cell and pointer
  names, asserts `格2` and `i` each address one node, and proves an out-of-range pointer leaves no
  group behind; **all eight goldens unchanged**, count 886 → 892.
- **Phase 2's eleventh slice — the linter, `check` — is done**, and it is the only tool in the
  director's list that reads the whole frame rather than a node: `{"action":"check"}` answers with a
  `report`, the **fifth reply presence** beside `diff`, `tree`, `capabilities` and `error`. The split
  is the design, and it is forced by what the question needs: `rustim_scene::check::check(tree,
  viewport)` is pure tree-and-frame geometry (out of frame, overlap, text size, contrast, empty
  text), `pacing(beats, narration)` is pure timeline (a beat under 0.2 s, narration read faster than
  8 characters a second), and `SceneContext::check` joins the two — the viewport and the queue live
  on the context, not the tree, which is why the command is intercepted by `handle` and why it can
  answer with `CommandOutcome::Checked` and no `StateDiff`. `Sequence::durations()` is the one queue
  accessor the linter needed: the beats are boxed and cannot be peered into one by one, so the
  nominal `max(duration)` per beat is the only honest answer to "how long will these take". Five
  properties are load-bearing. **Seven codes, and severity is derived from the code**, never a wire
  field: `out_of_frame` is the only error (an object a viewer cannot see is the picture being
  wrong), `overlap` / `text_too_small` / `low_contrast` warn, and `empty_text` / `beat_too_fast` /
  `narration_too_fast` are info — geometry does not measure whether an explanation is right, and a
  sub-second beat is a legitimate flourish. **The thresholds are constants, not parameters**
  (`MIN_TEXT_PX 18`, `MIN_CONTRAST 3.0`, `OVERLAP_FRACTION 0.1`, `MIN_BEAT_SECONDS 0.2`,
  `READING_CHARS_PER_SECOND 8.0`): a configurable table would be a second answer about the same
  picture, and the `detail` string says what was measured. **The frame test follows the camera**
  (`visible_world` from `visible_height()`/zoom and the viewport's aspect), so a moved or zoomed view
  changes which objects are out of frame — the same reason `a11y` projects through `tree.camera`.
  **Only top-level objects are compared for overlap** (a group and its own child always overlap),
  and the fraction excludes zero-area intersections; a degenerate viewport hides the frame check
  rather than reporting everything. **Pacing reads only beats not yet started** — a beat already
  playing was paced when it began — and a `duration` of `0.0` is an instant snap, not a fast beat.
  Findings are in a stable order, name ids (a label too), and a node-less finding omits
  `target`/`label` rather than writing nulls. `check` is a read: it is refused as a `batch` /
  `validate` member (`check answers with a report, not a diff`) and has no `out_of_range` of its
  own. Evidence: twenty unit tests over the pure halves (each code, severities, the constants, the
  camera dependence, contrast on alpha), five context tests (the joined report, only-unstarted
  pacing, a clean scene, a degenerate viewport, `handle`'s reply), a client test reading the fifth
  half, a conformance case through the real binary (a clean `{"findings":[]}`, then a square moved
  off-frame plus a 0.05 s beat and two narration lines, all named, with a following `query` still a
  tree), and the round-trip fixture; `ACTIONS` 31 → 32. **All eight goldens unchanged** — a linter
  reads the frame and draws nothing — count 892 → 919.
- **Phase 2's final slice — the declarative lecture plan (`plan.*`) — is done**, and it is the item
  that makes a lecture a *document* rather than a command log. `rustim_scene::plan` holds `Plan`,
  `PlanStep`, `PlanStatus` and the shape check; `SceneContext` holds the executor
  (`plan_load`/`plan_next`/`plan_goto`/`plan_status`); `handle` answers `plan.status` with the sixth
  reply presence, `CommandOutcome::Planned`, while the other three reply a diff like any mutating
  command. See "Scene plans" for the contract. Three semantics are the point. **Loading is a
  pre-flight**: every step is dry-run against a scratch session through the same `handle` a live
  command reaches, so a bad label, a malformed beat or a read verb inside `ops` is refused under
  `steps[i].ops[j]` before the first sentence — with the documented blind spot that an animation's
  *end state* is not simulated, so a removal a later op depends on surfaces at `plan.next` instead.
  **`plan.goto` replays through the reduced-motion path**, `begin`/`ease(1.0)`/`interpolate`/`finish`
  via a drained queue, so it is a function of its index, it restores the clock, and a `FadeOut`
  during a replay really removes its node rather than leaving a ghost. **A refused `goto` rolls the
  tree, clock and narration back and leaves the cursor**; `plan.next` is not a transaction and says
  so. Evidence: seven `plan.rs` shape/fixture tests, ten `context.rs` execution tests (the dry run's
  blind spot reaching `InPlanStep` at `goto`, rollback, the settled removal, a settled
  baseline taken from a stopped beat, a beat naming a label its own op created, `reset` dropping the
  plan with the tree), a client test reading the sixth half, four round-trip fixtures, and a
  conformance case through the real binary that walks a two-step plan — load, next, status, goto
  back and forward, `plan_finished`, an out-of-range step, and a refused reload that leaves the
  running plan intact. `ACTIONS` 32 → 36. **All eight goldens unchanged** — a plan drives the
  timeline and draws nothing new — count 919 → 938.
- **The two external clients and their conformance runners are done** (`sdks/python/` and
  `sdks/typescript/`). This is a deliverable of `贾维斯集成方案.md`'s repository map, not a Rust
  change: each SDK is a thin client of the stdio protocol plus a runner that drives the real binary
  through it. Both clients enforce the three framing promises a client can check locally — one reply
  per command line, a blank line is a counted skip with no reply, `seq` equals the physical line —
  and refuse a reply that is not JSON or carries zero/two presences, because a driver that treats a
  malformed stream as a reply cannot tell a broken transport from an engine bug. An engine *refusal*
  is not a framing error: it is returned to the caller as a value, exactly as `rustim-client` does.
  Both runners cover the handshake vocabulary (36 actions, 45 rates, both profiles, the teaching-tier
  verbs among them), `add` → `query`-by-label → `set` → `play`'s empty diff → `check`'s report, the
  three refusals (`no_such_label`, `unparseable`, a foreign protocol as `out_of_range` under
  `protocol`) with the feed still answering afterwards, plan `status`/`next`/`goto` against a live
  tree (including that a rewound object is `no_such_label`, not a stale id), and the switching
  acceptance itself: **two independent processes replay one label-addressed log and their final
  `SemanticTree`s are compared field for field**. Exit codes are 0 pass / 1 a check failed (naming
  the command, the expected shape and the reply that arrived) / 2 the question could not be asked.
  Python needs no venv, Node no `npm install`, and neither needs the network. Evidence: three green
  checks from the real debug binary in each language, and the stderr feed summary matching the
  intentional refusals exactly (`3 of 10` and `2 of 11`) in both. **No Rust test count changes**
  (938) and **all eight goldens are unchanged** — the slice reads a binary and renders nothing.
  Deferred: packaging either SDK for a package registry, and any additional language until a real
  driver needs one.


- **The macOS platform is verified** — the item `CLAUDE.md` called "development list item 1", which
  the user deferred on 2026-10-18. The deferral had exactly one cause: no Mac, and no way to reach
  the CI artifact store. Both are gone, and the route to a baseline is local rather than "download,
  review, commit": `BLESS=1 cargo test -p rustim-app --test goldens`, in the **debug** profile both
  CI jobs use. `platform()` builds `<os>-<arch>` from the host, and GitHub's own runner-images table
  says `macos-latest` is today macOS 26 **Arm64**, so `macos-aarch64` is the name both sides use —
  which is why the CI matrix now pins `macos-26` instead of trusting a floating label that has
  already moved across macOS 13/14/15/26. Four things are worth carrying forward.

  **The determinism the baseline rests on was measured, not assumed.** The same scene rendered twice
  on `axes`, `formula` and `pythagorean` gave `max == 0` and **zero** pixels differing by any amount
  — not "inside the §5.2 tolerance", bit-identical. That was worth checking *before* blessing,
  because the desktop default enables `vello_cpu/multithreading`. The thread-count variant of that
  check was a **false test, and is recorded here as one**: `RAYON_NUM_THREADS` does nothing, because
  `vello_cpu` builds its *own* pool from `RenderSettings::default()`
  (`vello_cpu-0.3.0/src/render.rs:178-190`). The real dispatcher-agreement evidence is `cargo test -p
  rustim-web` — `rustim-web` takes `rustim-render` with `default-features = false` (single-threaded)
  and its session test asserts `max == 0` against a baseline the multi-threaded binary produced. That
  test used to **skip** on macOS for want of a baseline; it now runs, and it passes.

  **The arch caveat, stated plainly because the docs assert the opposite.** Every `macos-aarch64.png`
  is **byte-for-byte identical** to its `linux-aarch64` twin — same pixels *and* the same compressed
  bytes. §10.2(3) and `CLAUDE.md` justify per-platform baselines with "NEON and SSE/AVX do not round
  identically", but the two platforms that now have baselines are the **same architecture**, so no
  cross-arch divergence has ever been exercised. The split is a guard whose subject has not appeared;
  an x86_64 baseline is what would test it.

  **The `f32_pipeline` citation was wrong, and the conclusion survived it.** §10.2(3) argued from
  "`vello_cpu` 的 `f32_pipeline` 保证同机确定性", but `rustim-render` runs `vello_cpu`'s default feature
  set — `u8_pipeline`, i.e. `RenderMode::OptimizeSpeed` (`vello_cpu-0.3.0/src/render.rs:113-131`) —
  with neither `f32_pipeline` nor `libm` enabled. The citation is corrected in place in `方案建议.md`.
  The u8 pipeline quantises during rasterisation, which makes cross-machine agreement *more* likely
  than the f32 path would, so the decision was right for a reason other than the one given.

  Evidence: `cargo test --workspace` **938 passed, 0 failed** on macos-aarch64 — before the baselines
  existed it was 8 failures (all golden cases) and cargo *stopped there*, so the other 41 test
  binaries had never run on this platform at all; the goldens suite 14/14 on three consecutive runs;
  `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check` clean;
  `cargo test -p rustim-web` running the single-threaded-vs-multi-threaded comparison instead of
  skipping; `say` proven end to end by putting a recording stub named `say` first on `PATH`, which
  captured argv `-v Tingting` and the sentence `场景 pythagorean，4 个对象` on stdin — text on stdin,
  never in argv, exactly as the module claims; the browser check against Chrome 155 on a Retina
  display, reproducing **every** Linux number; the windowed path opening on macOS for the first time
  (240 frames, ~54 fps); and VoiceOver, started with Cmd+F5 on that machine, **attaching** to
  `--a11y-probe` — the first observation of *any* platform's accessibility backend on a real screen
  reader, reporting `assistive technology: attached` and `1 full, 399919 incremental` tree updates.
  That last one is also where the probe's own trap was found: the default 600 frames is ten seconds
  at 60 fps but **2.4 s** where an idle frame costs 0.12 ms, which is shorter than a screen reader
  needs to notice a new application — the first attempt reported "not attached, 0 pushes" with
  VoiceOver audibly running, and `--frames 400000` was what produced the result above. The count is
  **939** after the new `from_reduce_motion_flag` test (938 before it).

- **macOS reads the system "reduce motion" preference** — the one platform-shaped gap that was code
  rather than verification. `motion.rs` had macOS "named but deliberately unread", and the reason
  was recorded in the file: `accesskit_macos` declares `objc2-app-kit` without the `NSWorkspace`
  feature, and there was no macOS machine to check a binding on. Both halves are settled now.
  `objc2-app-kit` **0.2.2** was already in the graph and
  `accessibilityDisplayShouldReduceMotion` is at `generated/NSAccessibility.rs:116-128`; **both**
  features are required, because `NSWorkspace` gates the impl block while `NSAccessibility` gates the
  whole *file* (`generated/mod.rs:28`) — asking for only the obvious one would compile purely by
  feature unification with `accesskit_macos`. The restructure had one trap: `from_platform` fed a
  `(program, args)` pair to a subprocess `probe`, so a macOS early return would leave `probe` with no
  caller, which `-D warnings` turns into a build failure — the exact thing that file's own comment
  was written to avoid. The arm is `if let Some(m) = macos::reduce_motion() { return m; }`, which
  keeps the fallthrough *reachable to the compiler*, and the `bool` → `Motion` mapping is a pure
  `from_reduce_motion_flag` with its own test, so the double negative is witnessed on every platform.
  Evidence: clippy stays green on macOS, and `objc2-app-kit 0.2.2` unifies to a single build shared
  with `accesskit_macos` and `winit`. **And the end-to-end check, which is the one that matters**:
  with the system setting off, `--frames 120` costs `mean 18.4 ms / p50 19.3 ms` (the opening beat is
  interpolated frame by frame); with it on, the same command on the same machine costs
  `mean 0.50 ms / p50 0.16 ms`, three runs within a hair of each other — a 37× collapse, which is
  the signature of every beat landing on its end state immediately. Turning the switch **back off**
  returned the same command to `mean 18.42 / 18.36 ms, p50 19.19 / 19.31 ms` — the original baseline,
  to the decimal — so the read follows the switch in **both** directions and repeats, which is a
  response rather than a coincidence. The system value read back as `1` while on and `0` while off. **The `defaults` route does not work on macOS 26**: `defaults write
  com.apple.universalaccess reduceMotion` is refused (`Could not write domain`), so the switch has to
  be thrown in the UI, and the label is **减弱动态效果** — not 减少动态效果, which is why searching for
  the latter finds nothing. It is under 辅助功能 → 显示 (`display.reduceMotion` in the settings
  bundle). Still deferred: Windows (`SPI_GETCLIENTAREAANIMATION`), and the preference remains read
  once at startup on all three.

- **Three macOS portability defects found on the way, and two gaps left open.** `tools/build-web.sh`
  used `stat -c%s` — GNU-only, under `set -euo pipefail` — so the script the manual tells a developer
  to run **aborted on macOS**; it now picks the spelling from `uname -s`, and the build then produced
  both artifacts (2 160 888 B raw / 761 044 B gzip for the plain page, 34 351 433 / 14 408 186 for
  the formula page). `tools/web-check.mjs` defaulted `$CHROME` to the bare `google-chrome`, a *Linux*
  name, so the check could never run on macOS and surfaced as a confusing Chrome failure; it now
  searches `PATH` and then the macOS bundles, and exits 2 with a sentence when there is none.
  `rustim-app` seeded the surface, renderer and viewport from the **logical** window constants while
  `WindowEvent::Resized` reports **physical** pixels — a Retina window is 2560x1440 for a requested
  1280x720, so the first frames were sized against the wrong number; both paths now go through one
  `App::apply_frame_size`, and the fix shows up in `query`, where a windowed run reports
  `frame: [2560, 1440]` with `visible` unchanged at `[14.222, 8.0]`, i.e. the same world extent at
  the right resolution. Honest note: `Resized` does arrive before the first redraw on macOS in
  practice, so that one was **not** demonstrated as a user-visible defect — it removes a transient
  wrong state rather than fixing an observed symptom. Left open at that point, and neither was a code
  gap: **Windows** had no baseline (the same procedure, `windows-x86_64`, closed by the next entry),
  and **`--video out.mp4`** needs an `ffmpeg` neither macOS nor Windows ships.

- **The platform matrix is closed — and the four baselines are one image.** The first CI run this
  repository ever had failed on both x86_64 legs for want of a baseline and uploaded
  `target/golden-artifacts/`, which is the route the docs always described and which had never been
  walked. Downloading those two artifacts, reviewing them and committing them produced
  `linux-x86_64.png` and `windows-x86_64.png` beside the existing pair, so **all three CI legs now
  have a baseline, and no extra hardware was involved.**

  The measurement that fell out of it is the one the docs had been asserting without evidence. All
  four sets — `linux-aarch64`, `linux-x86_64`, `macos-aarch64`, `windows-x86_64` — are
  **byte-for-byte identical** for every scene: two ISAs (NEON and SSE/AVX), two operating systems,
  four independently built binaries, `max == 0` everywhere. "NEON and SSE/AVX do not round
  identically" is true in general and **not observable here**, almost certainly because the build
  runs `u8_pipeline` / `RenderMode::OptimizeSpeed` rather than the `f32_pipeline` the proposal cites:
  quantisation during rasterisation absorbs exactly the sub-pixel differences an f32 path would
  surface. The per-arch split therefore stands as a **guard whose subject has not appeared**, and the
  docs now say that instead of presenting the divergence as an observed fact.

  This also closes the item `CLAUDE.md` called "development list item 1" for every platform it names
  — Linux x86_64 included, which no one had noticed was missing because "verified green on
  linux-aarch64" describes a development machine rather than a runner.

- **A still is an artifact too, so `--png` no longer asks the desktop for a preference.** The video
  export had always forced `Motion::Full` on the grounds that reduced motion is a *player's*
  preference and a film rendered on a machine with it enabled would be a different film. The same
  argument applies verbatim to a still, and the CI run above is what made it urgent: reading the
  preference on macOS turned `a_tempo_slows_the_still_into_mid_fade` into a machine-dependent test —
  green on the development machine, red on the runner. Both are now `Full` unless `--motion` is
  given explicitly, so the default is deterministic and a test has to *opt in* to inheriting a
  desktop setting. Verified by rendering the same tempo script with and without `--motion full` and
  getting `max == 0`. `使用说明.md` and `docs/architecture.md` both carry the rule, and the manual's
  cross-reference is fixed while it was being edited: the macOS switch is **减弱动态效果**, not
  减少动态效果 — the wrong word is why it could not be found, and `defaults write` cannot be used
  instead because macOS 26 refuses to write that domain.

- **The first all-green CI run, and what it cost to get there.** The repository had never run CI —
  there was no remote — so every platform claim in these documents described a development machine.
  One push produced a run with **eight jobs green on three platforms at once** (macOS 26 arm64,
  Ubuntu 24.04 x64, Windows x64), and the six runs it took were not waste: each failure was a real
  defect, and five were things no amount of local work would have shown.

  1. **`from_reduce_motion_flag` is never used** — the mapping's only caller sits behind
     `#[cfg(target_os = "macos")]`, so on Linux and Windows the function had no caller and
     `-D warnings` made that a build failure. Introduced by this session's macOS work, and invisible
     locally because clippy was only ever run on macOS.
  2. **`a_tempo_slows_the_still_into_mid_fade`** — a regression from the same work. Reading the
     system reduce-motion preference on macOS turned a test that asserts an animation's *mid-beat*
     state into a machine-dependent one, and the runner has the setting on. Reproduced exactly by
     forcing `--motion reduced` here (`max == 0` against `max == 119`), and it is what pushed `--png`
     to stop asking for a preference at all: a still is an artifact like a video.
  3. **CI's Linux leg asks for `linux-x86_64.png`** — the runner is x64 while the committed baseline
     was `linux-aarch64`, so that leg could never have passed, on any run, and no one had noticed
     because it had never run.
  4. **All four baseline sets are byte-identical.** Downloading the two failing legs' artifacts gave
     the missing pair, and comparing them against the aarch64 baselines gave `max == 0` on all eight
     scenes across two ISAs and two operating systems. "NEON and SSE/AVX do not round identically" is
     the stated justification for per-platform baselines everywhere here; it has now been looked for
     and not found — almost certainly because the build runs `u8_pipeline`, not the `f32_pipeline`
     the proposal cites. The split stands as a guard whose subject has not appeared.
  5. **Five tests spelled Unix-only paths**, all found by the Windows leg on its first execution:
     `/bin/echo` in `motion.rs` (a real failure), and `/dev/null` in five places as the idiom for
     "an empty script" — one a real failure, four passing by luck because the refusals they test fire
     before the path is opened. `a_probe_that_cannot_run_answers_nothing` is the sharper case: it used
     `/bin/false` for "a nonzero exit is not an answer", and on Windows that path is *missing*, so it
     passed for the wrong reason — the case directly above it already covered missing programs.

  Item 4 also closed the last of the golden-baseline item for every platform, Linux x86_64 included,
  with no extra hardware: the failing legs upload `target/golden-artifacts/`, which is the route the
  documents always described and which had never been walked.

- **A `query` answers with the session state beside the tree, and the four controls stop being
  write-only.** `pause`/`resume`, `tempo`, `motion` and `seek` could all be *set* and none could be
  *read*: a driver taking over a session had no way to ask what state the previous one left behind,
  which is the one question that matters when two assistants share an engine. A `query` now answers
  `{"seq":n,"tree":{…},"session":{paused,tempo,motion,playing,clock}}`.

  The design decision is what it is *not*. `session` is not a seventh reply presence — the six-field
  discriminant is untouched, and it is present exactly when `tree` is — so the three clients that
  count those six by name (`rustim-client` and both SDK runners) needed no change at all; that was
  checked before the field was added rather than after. And it is not in the tree: the semantic tree
  is what a screen reader hears, and a screen reader asking what is on screen has no use for the
  tempo, so `SessionState` rides beside it as a sibling field of the reply.

  `motion` reports the policy the **next beat started** will get, not the one a running beat has,
  because that is the honest answer and the same one the `motion` command documents. `playing` is
  true through a pause and true with a beat merely *queued* — the conformance case asserts the latter
  deliberately, having queued one earlier in the same session, because "is the timeline still moving"
  is the question and a beat on the clock means yes.

  Evidence: a `context.rs` test that sets all four and reads them back through `handle` (and asserts
  the tree is unchanged beside it), a `command.rs` reply-shape test that `session` appears with
  `tree` and in no other reply, and a conformance case driving the real binary over the wire —
  pause, tempo, motion, then one `query` reporting all three, followed by an `add` whose reply
  carries no `session` at all. `驱动协议v1.md` §3.9, `使用说明.md` and `docs/ai-integration.md` all
  carry the field and the "not a seventh presence" rule, which is the three-way sync a wire change
  owes. Count 939 → 940.

- **The keymap moves down to the kernel, so the browser can have one too.** `space` means
  play-or-pause and `m` flips the motion policy; if each host decided that for itself the engine
  would have two keyboards and a user would have to learn which one they were talking to. So
  `Intent`, `KeyPress` and `classify` moved from `rustim-app::input` into `rustim_scene::input`,
  over a **winit-free** vocabulary: `Key::{Tab, ArrowUp, …, Char(char), Other}` and
  `Code::{Comma, Period, KeyM, Other}`. What stays in the desktop host is `from_event` — the
  spelling conversion — beside `wheel_delta`, which is the same arrangement the pointer already has.

  Both halves of that were forced rather than chosen. `winit` has no wasm32 build, so a keymap naming
  its types could not be reached from the browser at all; and `Key::Char(char)` rather than a string
  is honest about what the keymap actually binds, which is why the IME cases (a full-width `，` from a
  Chinese input method) are expressible in the neutral vocabulary instead of being a desktop quirk.
  The physical-key fallback survives as `Code`, because an IME that consumes the press outright
  leaves no logical key while the physical one is still whatever was pressed.

  This is `rustim_math::gesture`'s arrangement applied to the keyboard, and for the reason that step
  recorded: a vocabulary two hosts share belongs where both can reach it. Count 940 → 941.

- **The browser gets the keyboard, and focus stays the DOM's job.** The Web host was the last place
  that was not a peer: it could pan and zoom but a user could not operate it without a pointer, and
  the checks had no keyboard to check. `keys::keydown` translates a DOM `KeyboardEvent` into the
  shared `rustim_scene::input::KeyPress` — the same adapter shape the desktop has for winit — and
  `keys::handle` acts on the intents a browser can honour: space (pause/resume), `m` (motion) and the
  arrows (the focused figure's opacity), each reading the engine for the current value because a
  toggle means "the other one". It answers `false` for `,`/`.`, Tab, Enter and Escape, which is how a
  caller knows to leave the key to the page: a browser has no frame clock for a user to step, and its
  focus ring is the DOM's own.

  **Focus is not re-implemented.** Each overlay element carries `data-rustim-node` and a `tabindex`,
  so the browser's Tab order over the figures comes for free and this host keeps no `FocusState` —
  the desktop has one only because it has no DOM. A key that arrives while one of them has focus is
  therefore a key *about that figure*, which is what makes the arrows mean something.

  Two things the work turned up, both from a check rather than from reasoning. The first tab stop was
  the semantic tree's **synthetic root** — `NodeId::RESERVED`, a container whose `hidden` is `false`
  by construction because it is not an object that is painted or not — so the first Tab landed on a
  node the engine does not hold and an arrow pressed there was about nothing. The condition is now
  the desktop's focusability rule spelled for this side: `!hidden` (which is already
  `opacity <= 0.0 || !has_picture()`, exactly what `FocusState::sync` skips) **and** `bounds.is_some()`
  for the one node `hidden` cannot cover. And the arrow assertion had to pause the timeline first —
  the Pythagorean scene fades its figures in, so "the canvas changed" would otherwise be the clock's
  doing rather than the key's, and the check renders twice with no key between them as the
  counterfactual that proves the comparison can tell.

  Evidence: 9 native tests in `rustim-web` (the DOM spellings including `" "` for space and
  `"Unidentified"` with a physical `Comma`; both toggles read back through `query`; an arrow clamped
  at both ends rather than producing a value the command layer would refuse; and the four
  deliberately-unhonoured intents), and an assertion in `tools/web-check.mjs` through Chrome's real
  input pipeline — `Input.dispatchKeyEvent`, not a synthetic event — which focuses a figure, tabs to
  the next, presses space and `m` and reads both back out of the **engine's** session block, then
  shows the arrows changing the canvas while a paused timeline cannot. Count 941 → 947.

- **An animation with no end, and the scheduler concept it needed.** `Rotating` was deferred with the
  reason written down: `duration = ∞` makes `alpha = age / ∞ = 0`, so the object freezes at its start
  while `is_playing` stays true forever — which every "react when the timeline drains" scene depends
  on. The fix is a trait method rather than a very large number: `Animation::perpetual`, defaulting to
  `false`, and the scheduler hands a perpetual animation **raw elapsed seconds** instead of progress,
  skips `ease` for it (which clamps to `0.0..=1.0`, and a second is not a progress value), and
  excludes it from both the completion pass and the sweep. It has no end, so `finish` never runs
  either — a `FadeOut` and a `Rotating` differ in exactly that.

  **`Rotating::duration()` is a period, not a length.** `turns_per_second` is the parameter and its
  sign is the direction, because the angle is exactly `age × rate × τ`; a period would be divided
  back out every frame and the division is where an infinity would come back from. It reports one
  full cycle — all an endless animation can honestly offer, and what a `beat_started` event has a
  field for — and **degrades to a nominal period for a degenerate rate** rather than reporting an
  `∞`, which would reach that event and JSON has no spelling for one.

  The `Box` forwarding is the load-bearing line, and this repository has been bitten by exactly this
  twice: the scheduler holds `Box<dyn Animation>`, `perpetual` has a default, so a missing forward
  would make a *boxed* `Rotating` answer `false` and freeze it — the bug the method exists to
  prevent, arriving through the one container every beat is built from. There is a test named for it.

  Evidence: four `ops.rs` tests, including the scheduler one rather than a hand-called `interpolate`
  (two seconds at a quarter turn a second puts each corner on the opposite one; the counterfactual is
  that `age / duration` would have crept round instead or, at an infinite period, not moved at all).
  Count 947 → 951.

- **`stop` can be aimed at one object.** The scoped half of the interrupt, deferred because "the
  `Animation` trait exposes no target, so expressing it would mean adding one to every animation".
  It did: `Animation::target` — the node an animation is *about*, `None` where that is a real answer
  rather than an omission. The camera is a field and not a node, `Wait` is about nothing at all, and
  a `group`/`succession` is a *timeline* whose members may each be about a different node, so scoping
  a stop to one of them would be arbitrary rather than precise; those are left to the timeline-wide
  `stop`, and the doc comment says so instead of leaving it to be discovered.

  `{"action":"stop","target":"方块"}` drops the running animations whose subject is that node **and**
  the queued ones, because a beat that had not started would otherwise animate the object a moment
  later — the opposite of what was asked. It keeps `stop`'s posture everywhere else: **no `finish`**,
  so an interrupted `fade_out` leaves its target in the tree at the opacity it reached; the clock and
  the narration track untouched; idempotent. A target that is not there is `NoSuchNode`/`NoSuchLabel`
  rather than a quiet everything.

  `Sequence::clear_target` documents the one subtlety: **the beats themselves stay.** A beat that
  loses all its members becomes an empty one that starts and finishes immediately — the shape of
  `Wait(0.0)` — rather than being removed, because removing it would renumber the timeline and change
  what the beats after it are. A caller watching `beat_started` sees the beat happen and do nothing,
  which is the honest report.

  Evidence: a context test with the two objects fading in **in the same beat** — otherwise "drop the
  beat holding one of them" would pass it, and the whole point is that the unit is an object rather
  than a beat — plus a later beat belonging to the same object, which is what shows the queue is
  reached; and a conformance case that plays a labelled square, stops it by label, stops it again
  (idempotent), and gets `no_such_label` for a name that is not there. Two round-trip fixtures pin
  both spellings. Count 952 → 953.

- **The clip was not a reason to repaint the whole frame, and the argument for it looked sound.** The
  damage diff returned `Damage::Full` for any `PushClip`/`PopClip`/`PushLayer`/`PopLayer`, on the
  stated grounds that vello's two stacks are *positional* and a region render starts a fresh context
  — so a clip opened before the region would be missing. The stacks are positional, and that is
  exactly why the escape was unnecessary: a region render replays the **whole** op list with an
  integer shift, so the clip is re-established inside the region through the same path moved by the
  same integer, and a layer composes the same way.

  It was settled by rendering rather than by argument, which is the only way this kind of claim can be
  settled: two backend tests whose assertion is `assert_eq!` on the pixel buffers — a clipped frame
  and a layered one, each changed inside the clip and repainted as a region — both byte-identical to
  the full render. The square in the clipped case straddles the clip deliberately, so a dropped clip
  would paint corners the full render leaves empty.

  The consequence is a capability rather than a number: **a scene that clips and animates now gets a
  region instead of a full repaint**, which is what `primitives` would have been paying for had it
  moved. **All eight goldens are unchanged** — a damage rule changes what a frame *costs*, not what it
  *is* — and the count moved 961 → 963.

- **Font subsetting was measured rather than implemented, and the measurement is the deliverable.** The
  deferred item was "font subsetting and lazy loading", and the honest place to start was what is
  actually in the number. On macos-aarch64 (2026-10-20): the formula page's wasm is 34 356 006 B raw /
  **14 408 051 B gzip**, of which **`typst-assets`' fonts are 6.1 MB gzip — 42% of the download**. And
  **two thirds of that font weight is NewCM (4.03 MB gzip), the family a math fragment is rendered in**:
  the part that cannot be dropped is most of the part there is.

  A glyph-level subset is not available either, for a reason that is the page's whole point: it compiles
  **arbitrary formulas at runtime**, so the glyph set is not knowable at build time. What subsetting
  could buy is the ~2.1 MB gzip of faces math never touches — Libertinus, DejaVu Mono, and eighteen
  Foxit `.pfb` files that exist for PDF export — at the cost of ~3 MB of font files in this repository, a
  second font-loading path, and a licence surface. `typst-assets` has exactly one feature (`fonts`,
  all-or-nothing), so a *runtime* filter would not shrink the artifact at all: the bytes are linked
  regardless. That is the trap this measurement exists to avoid walking into.

  What is already in place beats the 2.1 MB: the two pages load **different modules**, so a reader of the
  Pythagorean scene downloads ~640 KB and only a page that compiles formulas pays for a compiler. The
  numbers are now in `docs/architecture.md` beside the artifact table they belong to, so the next person
  to read "font subsetting" as an outstanding win can see what it is worth before starting.
