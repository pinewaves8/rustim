# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository state

A Cargo workspace at the repository root, plus three documents and one directory of throwaway probes.

**Read the documents in this order:**

1. `方案建议.md` — **the primary document.** The commissioned research and proposal: measured Manim
   internals (with `file:line` evidence), Rust crate/version selections, the recommended
   architecture, a phased roadmap with acceptance criteria, an evaluation of ~20 existing Rust
   Manim-family projects, and the cross-platform analysis. It **supersedes** the two documents below
   wherever they disagree.
2. `从零用 Rust 重写 Manim 最小验证教程.wps` — a 22-page tutorial (OLE2/Word 97 format, extract with
   a custom reader; no `catdoc`/`libreoffice`). Defines a module layout, trait designs and code
   sketches for a deliberately minimal ~800-line version.
3. `需求探讨.txt` — the original design rationale: why Rust, why this is an architecture change
   rather than a translation, and three candidate technical routes with cost/benefit.

The tutorial's code blocks are **illustrative sketches, not compilable code**; the workspace does not
follow its flat `src/` module layout. See "Where the workspace diverges from the tutorial" below.

`p0-spikes/` holds three Phase-0 probe crates (`gpu-probe`, `window-probe`, `cpu-bench`) plus the
comparison evidence: `manim-pythagorean.py` / `manim-pythagorean.png` (the reference frame, rendered
by the `/tmp/manim-venv` Manim), `rustim-pythagorean.png` (our frame), `rustim-p0-frame.png` (the
window probe's first frame — evidence for the presentation path rather than the rasteriser), and
`compare_manim.py`, which produces the pixel statistics quoted in `方案建议.md` §5.2. Regenerate our
side with
`cargo run --release -p rustim-app -- --png p0-spikes/rustim-pythagorean.png`, then run
`/tmp/manim-venv/bin/python -I p0-spikes/compare_manim.py <ours> <theirs>`. The probes are excluded
from the workspace and are scratch work, not the engine — but they are the evidence for every number
quoted in this file and in `方案建议.md`, so keep them.

The workspace itself is **verified green** as of 2026-10-16: **205 unit tests plus a doctest** pass,
`cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check` are clean, the
six-crate semantic kernel builds for `wasm32-unknown-unknown` (with `accesskit` reachable from
`rustim-app` and **not** from `rustim-scene`), all four golden scenes match their checked-in
baselines, and `rustim-app --png out.png` reproduces the Manim reference frame with an **identical
content bounding box** (`343,90,936,629`); only 0.012% of pixels differ by more than 2/255, and none
by more than 8. See `方案建议.md` §5.2. The count is not decoration — every one of those claims is
re-run by the commands below, and the P1 rows in the roadmap say which step last moved it.

## What rustim is

A real-time, interactive 2D mathematical visualization engine in Rust, in a 3Blue1Brown-like visual
style, intended to be the interface an AI drives when explaining something to a person.

**The central architectural difference**, and the reason this is a rewrite and not a translation:
rustim has a *persistent real-time main loop* — every frame updates state, responds to input, and
redraws. Manim is a one-shot batch pipeline (arrange the whole timeline → render frame by frame →
mux a video). Roughly 80% of the scheduling and rendering architecture is new; only the math
definitions and graphics-element logic carry over. Note that `需求探讨.txt` reaches the opposite
conclusion from the tutorial — it favours building on Bevy and calls a from-scratch engine a
person-year project. The proposal resolved that in favour of from-scratch; see below.

## Settled constraints

These are decided. Do not re-litigate them without new evidence.

- **Build the engine; do not fork an existing one, and do not use Bevy's ECS.** The scene graph needs
  *stable, serializable node ids*, because the AI command layer and the accessibility tree both
  address objects by id, and recycled ECS entity ids are the wrong primitive for that. ~20 existing
  Rust Manim-family projects were evaluated; none is adoptable wholesale.
- **Cross-platform is a hard requirement: Linux, macOS and Windows.** Hence a CPU-first renderer, a
  bundled CJK font (a bare Linux has none), video encoding as a pluggable sink rather than a call to
  a system `ffmpeg` (macOS and Windows ship none), and **per-platform** golden-image baselines —
  float results are not bit-identical across NEON and SSE. Web (WASM) is a Phase-2 target; the
  day-one guards for it are listed in `方案建议.md` §10.6.
- **Concept compatibility, not API compatibility.** Keep the names a Manim user knows (`Scene` /
  `Mobject` / `Animation`, `construct()` / `play()`), but do **not** build a layer that runs existing
  Manim Python scripts. Manim's 343 classes / 1379 methods include 3D, boolean ops, `skia-pathops`
  and a LaTeX stack — chasing parity with them *is* the person-year trap.
- **CPU-first is not a preference here, it is forced by the hardware.** See "The floor machine has no
  usable GPU path" below. `vello_cpu` is the renderer, not the fallback.

## Commands

The toolchain is installed but **not on the default non-interactive `PATH`** — `cargo`/`rustc` 1.99
live in `~/.cargo/bin`. Export it or use the absolute path:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

```bash
cargo run -p rustim-app              # launch the reference window
cargo run -p rustim-app -- --png out.png   # headless: the scene after 10 s to a PNG
cargo run -p rustim-app -- --inspect       # headless: the accessibility tree as JSON
cargo run -p rustim-app -- --frames 300    # run 300 frames then exit
cargo run -p rustim-app -- --scene primitives  # ...a named scene; `--help` lists them
cargo run -p rustim-app -- --scene text --png out.png   # CJK + Latin glyph runs
cargo test --workspace               # unit tests across all crates
cargo test -p rustim-math ease_in_out       # a single test by name
cargo test -p rustim-math --test manim_parity   # replay the checked-in Manim fixtures
cargo test -p rustim-app --test goldens         # compare renders to the checked-in baselines
BLESS=1 cargo test -p rustim-app --test goldens # ...and rewrite them (review the diff!)
cargo test -p rustim-text --test manim_text_scale  # check the font size constant (needs no Python)
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo doc --open -p rustim-math      # trait-heavy API; docs are the primary reference
```

The two Manim-derived numbers in the codebase each come from a script in `tools/manim-fixtures/`,
run once against `/tmp/manim-venv` with `-I` and the output path as an argument; the results are
checked in, so neither the tests nor CI need Python. `gen.py` produces the numeric fixtures,
`text_scale.py` produces `crates/rustim-text/tests/fixtures/manim-text-scale.json`.

The wasm guard job, runnable locally (`wasm32-unknown-unknown` std is already installed here):

```bash
cargo build --target wasm32-unknown-unknown \
  -p rustim-math -p rustim-color -p rustim-mobject \
  -p rustim-anim -p rustim-scene -p rustim-render
```

It also asserts with `cargo tree` that `accesskit` reaches `rustim-app` and **not** `rustim-scene` —
`accesskit` compiles to wasm32 happily, so a stray dependency there would not fail the build above;
it would silently put a desktop accessibility binding inside the kernel. Checked both ways round,
so it cannot pass vacuously if the dependency is ever renamed away.

Always benchmark in `--release`. The debug profile is not meaningfully representative for a
rasteriser.

**`rust-toolchain.toml` pins `stable`, deliberately not an exact version — do not "restore" a `1.92`
pin there.** Every channel named there has to be fetched whole from `static.rust-lang.org`, which on
this network runs at single-digit KB/s; pinning `1.92` once made a bare `cargo build` spend 15 minutes
downloading a toolchain and never reach the workspace. `1.92` is the *MSRV floor* imposed by
`typst 0.15.1`, and that belongs in `[workspace.package] rust-version` in the root `Cargo.toml` —
where it does still bind, since cargo resolves dependencies to versions compatible with it. A floor
and a target are different things; only the floor needs to be old.

**Dependencies.** `cargo` fetches crates from crates.io normally — `index.crates.io` and
`static.crates.io` both work, and no mirror or `.cargo/config.toml` source replacement is needed.
What is blocked is only the `crates.io` **web API** (403 to a plain `curl` without a `User-Agent`)
and `github.com` (timeouts). For GitHub, `api.github.com` and `codeload.github.com` are reachable
directly, and `https://gitclone.com/github.com/<owner>/<repo>.git` works for clones. `WebFetch` is
blocked for all domains; use `curl` from bash.

**Manim, for golden images.** Local Python is 3.7; Manim needs 3.9+. A Python 3.12 venv with Manim
0.22 is installed at `/tmp/manim-venv` (recreate with `uv venv --python 3.12 /tmp/manim-venv` then
`VIRTUAL_ENV=/tmp/manim-venv uv pip install --index-url https://pypi.tuna.tsinghua.edu.cn/simple manim`).
`uv` and a Cairo 1.16 / Pango 1.44 toolchain are present, so it builds from source, which takes a few
minutes. **Check the directory before assuming it is there:** it lives in `/tmp` and does not survive
a reboot. Nothing in the test suite needs it — both fixture files are checked in — so Python is
required only to *regenerate* one. Manim writes a `media/` directory next to whatever cwd it is run
from, which is why `/media` is in `.gitignore`. Render a reference frame with:

```bash
/tmp/manim-venv/bin/python -m manim -s -r 1280,720 scene.py SceneName
```

**Manim numerical parity (`rustim-math`).** `crates/rustim-math/src/bezier.rs` and `space_ops.rs` are
literal ports of `manim.utils.bezier` and `manim.utils.space_ops`, and
`crates/rustim-math/tests/manim_parity.rs` replays **426 recorded Manim results** against them
(`tests/fixtures/manim-{bezier,geometry,space-ops}.json`, checked in — the suite needs no Python).
The acceptance rule is `1e-9` absolute on every coordinate, with no "close enough" allowances: the
ports reproduce Manim's *rounding paths*, not just its formulas, which is the only way the numbers
land in the same place.

Three properties of that suite are load-bearing:

- **Every case must be consumed.** Each test asserts exhaustively over the groups it names, and
  `every_fixture_group_has_a_test` checks the groups named in the test file are exactly the groups in
  the fixtures. Adding a group to the generator and regenerating fails the suite until a test claims
  it — the characteristic failure of a parity suite is a case nobody compares, not a wrong comparison.
- **The fixtures carry their own inputs.** Tests read the curves, `t` grids and offsets out of the
  JSON rather than restating them, so the two sides cannot drift onto different data.
- **Regeneration is a deliberate act**, needs the venv, and is done in place:
  `/tmp/manim-venv/bin/python -I tools/manim-fixtures/gen.py crates/rustim-math/tests/fixtures/`
  (`-I` matters: the interpreter must not pick up code from the script's directory — see the
  untrusted-input rule). `fixture_case_counts_are_stable` then has to be updated by hand, which is
  the point: a fixture that shrank should be a visible edit.

`crates/rustim-mobject` builds its polygons through `bezier::points_as_corners`, which is Manim's
`set_points_as_corners` — the interior control points of each straight segment sit at `1/3` and `2/3`,
**not** stacked on the endpoints. Both draw the same line, so the difference is invisible until
something parameterises the curve (morphing, `proportions_along_bezier_curve_for_point`) or flattens
it at a coarser resolution; the degenerate spelling is not an acceptable "equivalent".

**Golden images (`rustim-app`).** `crates/rustim-app/tests/goldens.rs` runs the real binary over
every scene the `--scene` registry offers and compares the render, pixel by pixel, against
`crates/rustim-app/tests/goldens/<scene>/<os>-<arch>.png`. Three properties carry the suite:

- **Baselines are per platform *and per arch*.** `vello_cpu` rasterises in f32 and NEON and SSE/AVX
  do not round identically (§10.2(3)), so one shared image would fail everywhere it was not made.
  `every_scene_the_binary_offers_has_a_baseline` reads the binary's own `--help` and fails if the
  registry and the suite disagree in either direction, so a new scene cannot skip its baseline.
- **A missing baseline fails.** Never skip: that is how a whole platform goes unchecked. The failing
  run leaves its render in `target/golden-artifacts/`, which the CI job uploads, so bootstrapping a
  platform is "download, review, commit" — the macOS and Windows baselines were made that way, not on
  this machine.
- **The thresholds are §5.2's measured ones and are not to be relaxed**: identical content bounding
  boxes, zero pixels differing by more than 8, maximum channel error 3. The comparison lives in
  `rustim_export::compare` (not in the test) because P5's visual-regression CLI reuses it.
  `rustim-app`'s `--png` renders *the scene after a fixed 10 s of virtual time*, not "as many frames
  as the scene happens to need" — otherwise a duration change would silently rebless a golden as a
  capture of a half-faded frame.

The four scenes are there to disagree with each other: `pythagorean` is the one with a Manim
reference, `composite` covers deferred transforms, alpha and draw order through the mobject tree
walk, `primitives` pushes strokes and clips as raw `DrawOp`s because nothing in the mobject model
expresses either yet — without it the baselines would never touch stroke expansion or clip
rasterisation, the two most intricate things `vello_cpu` does — and `text` is the only one whose
pixels come from a file in this repository rather than from code, so losing `assets/fonts/` fails
here rather than silently producing a blank frame for a user.

## Architecture

### Crate map

The workspace splits along the "semantic kernel vs. the world" boundary. The point of the split is
that `rustim-math` / `rustim-color` / `rustim-mobject` / `rustim-anim` / `rustim-scene` carry **no
rendering backend, no platform code and no I/O**, so they can be unit-tested without a rasteriser,
reused by the exporter and the AI layer, and compiled to `wasm32-unknown-unknown`. CI enforces that
last property.

| Crate | Contents | Status |
|---|---|---|
| `rustim-math` | Points, `Affine2`, rate functions, the `Viewport` world→screen transform, and literal ports of Manim's `bezier` / `space_ops` | Real |
| `rustim-color` | The 3b1b palette as `const`s | Real |
| `rustim-mobject` | `Mobject` (points / style / deferred transform / `label` / `NodeKind`), `SceneTree`, stable `NodeId`, shape builders, `critical_point`, `world_anchors` | Real |
| `rustim-anim` | `Animation` trait, `AnimationScheduler`, `FadeIn`, `MoveTo` | Real |
| `rustim-text` | Text shaping: an injectable `FontProvider` over `parley`/`fontique`, CJK from `assets/fonts`, `MissingGlyph` / `FontSubstitution` reports | Shaping real; **Typst formulas not started** — see its module docs for what blocks them |
| `rustim-scene` | `Scene`/`SceneContext`, the serde command layer, `a11y` (the platform-neutral semantic tree) | Real |
| `rustim-render` | `Renderer` trait, `DrawList`, the `vello_cpu` backend | Real (CPU only) |
| `rustim-export` | PNG encoding, and the `compare` module that scores two frames | Real; ffmpeg pipe not written |
| `rustim-app` | The winit event loop, softbuffer presentation, keyboard input, announcements, the AccessKit adapter and the shaper↔rasteriser bridge (both as a **lib** target), four `--scene` entries, the golden suite | Real |

Dependencies point downwards only. `rustim-scene` depends on `rustim-render` for the `DrawList` type;
if the renderer ever grows a heavier surface, `DrawList` should move down instead.

### The frame

```text
winit event loop → Scene::update(dt) → SceneContext::build_draw_list(&mut DrawList)
                 → Renderer::render(&DrawList) → softbuffer → screen
```

`SceneContext::build_draw_list` is the only place world coordinates are materialised. Everything
upstream mutates *local* points and an `Affine2`; the flattening step folds each mobject's transform
together with its ancestors' and emits paths.

### The two structural changes relative to Manim

1. **Transforms are deferred.** Manim bakes a transform into the point array of every descendant, so
   moving a `VGroup` costs O(points). Here `Mobject::transform` is an O(1) field and
   `materialize()`/`to_bez_path()` is the explicit, cacheable step that produces world coordinates.
   Manim's *semantics* survive — `critical_point` and morph alignment still see final coordinates —
   but the cost of a move collapses.
2. **Animations address their targets by `NodeId`, not by reference.** The tutorial sketches
   `Create<'a, T>` borrowing `&'a mut T`, which cannot be boxed into a `Vec<Box<dyn Animation>>`
   because that requires `'static`. Animations carry an id and mutate through the tree. This is the
   same decision the AI layer needs: an animation and a JSON command should both be *data naming a
   node*.

### Core traits

`Scene` — `construct(&mut self, ctx: &mut SceneContext)` declares the initial state;
`update(&mut self, dt: f64, ctx: &mut SceneContext)` runs once per frame and may sequence stages by
hand (the reference scene reveals one square per stage, gated on `ctx.animations.is_empty()`). This
split is the real-time-loop architecture: Manim's `construct()` returns and the timeline is fixed;
ours is called every frame forever.

`Animation` — `duration()`, `begin(&SceneTree)` (called once at start, so an animation can capture
where its target actually is), `interpolate(alpha, &mut SceneTree)` with `alpha` in `0.0..=1.0`, and
`rate_func()`. Easing is applied **inside** the animation, never by the scheduler.
`AnimationScheduler` holds one shared clock and computes each animation's progress independently, so
durations may differ and the shortest simply finishes first.

`Renderer` — `resize`, `render(&DrawList)`, `pixels()`. The vello CPU and GPU backends expose an
identical scene API, which is what makes this trait nearly free.

### Accessibility

Three layers, and the split is the design:

| Layer | Where | What it knows |
|---|---|---|
| `NodeKind` | `rustim-mobject` | What a mobject *is* (square / circle / text / …), a serde field beside `label` |
| `SemanticTree` | `rustim-scene::a11y` | Roles, names, measured descriptions, screen-pixel bounds. **No accesskit, no platform** — it compiles to wasm |
| `accesskit` | `rustim-app::a11y` | The adapter, the announcement node, action requests. The only module in the workspace that names `accesskit`; CI asserts this with `cargo tree` |

The two projections share *semantic data*, not an AccessKit tree: there is no `accesskit_web`, so a
browser build goes through DOM/ARIA (`SemanticRole::aria_role` is the string it writes). The same
structure is also the agent-facing inspection surface — `rustim-app --inspect` prints it as JSON,
which is the only way to see this layer on a machine with no screen reader.

**Sizes are measured, never declared**, from `SceneTree::world_anchors` — the materialised anchors,
so a scaled object reports its scaled size and the shape parameters (not stored on the mobject
anyway) are never consulted. For a polyline the size comes from the **edge lengths**, not the
bounding box, and that is a correctness requirement rather than a refinement: the box of a rotated
square is `side · (cos θ + sin θ)`, so the reference scene's hypotenuse square — turned by the
angle of the 3-4-5 triangle — would be announced as a 4.2-square of area 17.64 when it is a
3-square of area 9. A blind reader told that has been told the Pythagorean theorem is false, in the
one scene that exists to demonstrate it.

A description also **never contains a position**: on macOS and Windows a changed description is an
accessibility property-change event, and `MoveTo` rewrites the transform every frame of an
animation, so a position in the text would re-announce every node sixty times a second. Size enters
the description, position lives in `bounds`, which the backends only report when it really changed.

The conversation with a platform is deliberately narrow: Tab/Shift+Tab move focus, the arrow keys
become `Property::Opacity` **commands** (the same ones an AI sends, through the same validator),
space toggles playback, `,`/`.` step a frame, Enter re-reads the focused object, Escape drops focus.
`rustim-app::announce` throttles announcements and holds rather than drops a burst, so an animation
ending says where things ended up instead of narrating the whole transition.

Verifying it by hand, per platform:

```bash
cargo run -p rustim-app -- --scene pythagorean --inspect   # the tree, no screen reader needed
cargo run -p rustim-app -- --frames 300                    # Tab, arrows, space, `,`/`.`
# macOS: VoiceOver (Cmd+F5) · Windows: Narrator (Win+Ctrl+Enter) · Linux: Orca, needs a
# session bus — with none, accesskit_unix skips silently and this is all untested here.
```

### Text

Three crates, and the boundaries are the point:

| Crate | What it owns |
|---|---|
| `rustim-text` | `FontProvider` (a `parley`/`fontique` collection fed from a directory or a buffer, **never** from system font paths), `shape()`, and the `MissingGlyph` / `FontSubstitution` reports. Knows nothing about drawing |
| `rustim-app::text` | The conversion: parley's runs → `DrawOp::GlyphRun`. The one place a shaper's output meets a rasteriser's input |
| `rustim-render` | `DrawOp::GlyphRun` → `glifo`. **`rustim-render` must never depend on `rustim-text`,** and the shared type is `rustim_math::TextGlyph` |

CJK ships in the repository at `assets/fonts/` (Noto Sans CJK, OFL) and is read at **run time**, not
with `include_bytes!` — 19 MB in a compilation unit is re-parsed on all three CI platforms every
build. That is also what makes the text golden reproducible: no system font is consulted, so the
baseline does not depend on what the machine has installed. `BUNDLED_DIR` and `BUNDLED_FAMILY` name
the directory and the face; both are resolved against `CARGO_MANIFEST_DIR`, so they are right for a
checkout, `cargo test` and `cargo run` and wrong for an installed binary — which is why a missing
directory is reported rather than assumed away.

**The glyph-run transform carries the whole conversion.** Layout space has its origin at the block's
top-left with y growing down; a mobject's local space is centred with y growing up. `text_ops` writes

```text
device = viewport · world · (centre and flip)
```

and passes the shaper's glyph coordinates through unchanged. This is not a style choice. `glifo`
composes `run_transform · glyph_position · font_size · y_flip · outline`: it flips the **outline**
from font space into layout space itself, then maps everything through the run transform. Flip the
*position* and not the outline — the obvious-looking alternative — and every glyph is mirrored about
its own baseline: the baselines land correctly and the letters are upside down, which reads as a font
bug rather than a coordinate one. Consequently nothing here multiplies the size by the viewport
scale either; `glifo` absorbs a positive uniform scale into the font size and hints against the size
it actually draws at, and the two flips in the composite cancel to keep that path selected. A change
that leaves a net negative y scale silently drops the run into the unhinted path — it still draws,
which is why `the_composite_scale_is_positive_and_uniform` exists.

**A text mobject has no points.** Its geometry is a glyph run the scene pushes itself, so
`Mobject::has_picture()` — not a point count — is the test for "the user can see or reach this", and
it is what `FocusState::sync` and the a11y `hidden` flag use. `SceneContext::build_draw_list`
legitimately still tests points, because it emits filled paths and a text mobject has none to emit.
The cost of being a different kind of object shows up as `bounds: null` in `--inspect`: a text node
has no accessibility bounds yet, because `world_anchors` has nothing to measure.

`MANIM_FONT_SIZE_UNITS` (`rustim-text`) is **2/3**, i.e. Manim's `font_size` behaves as points with
72 to the world unit. Measured, not derived — see the constant's doc and
`tests/manim_text_scale.rs`; multiplying Manim's `TEXT2SVG_ADJUSTMENT_FACTOR` by
`TEXT_MOB_SCALE_FACTOR` gives `0.5` and is wrong by a third.

### Naming

`Scene`→`Scene`, `Mobject`→`Mobject`, `Animation`→`Animation`, `construct()`/`play()`→same,
`MathTex`→`Formula`, `Create`→`Create`, `Shift`→`MoveTo`.

## Hard-won facts

Each of these cost real time to discover. They are not style preferences.

**`fontique` hands back registered families in `HashMap` order, so a `.ttc` picks a different face
per process.** `CommonData::register_fonts` (`collection/mod.rs:707`) collects into
`HashMap<FamilyId, _>` and returns that map's iteration order; `GenericFamilyMap` is an ordered list
searched first-match-wins. Passing all six faces of the bundled Noto Sans CJK to
`set_generic_families` therefore made "sans-serif" resolve to whichever face came out first that run.
Measured: `直` shaped to glyph id 27874 in some processes and 27875 in others, and two runs of the
**same release binary** differed over the whole content bbox — 24746 pixels, maximum channel delta
237 — which is what made `BLESS=1` produce a baseline the next run could not match. The fix, in
`FontProvider::register`, is to sort the families by name and give each generic slot **exactly one**
id. Anything downstream that reads "the first" needs the same treatment.

**`parley`'s `complex-scripts` feature is not enough on its own, and without it CJK prints a data
error per line.** The feature only switches parley to ICU4X's `new_dictionary` segmenters; parley
pulls `icu_segmenter` with `default-features = false`, so that crate's `auto` (= `lstm`) is off and
the models the dictionary path needs do not exist. The symptom is
`ICU4X data error: No segmentation model for complex script: Chinese/Japanese` on stderr, once per
shaped line — the scene still draws, so it is easy to ship. `crates/rustim-text/Cargo.toml` names
`icu_segmenter` with `lstm` purely so feature unification turns it on; nothing there calls into it.

**Manim's `Text.height` is an *ink* box, not a line box.** `Text(" ").height` is `0`, `Text("x")` is
an x-height and `Text("H")` a cap height. This engine's shaper reports a line box, so the two cannot
be compared directly — which is exactly how `MANIM_FONT_SIZE_UNITS` came to be
`48 / 4.8 * 0.05 = 0.5`, a third too small. The comparable quantity is a **font metric**: the cap height from the face's own
`OS/2` table, against Manim's ink for a capital. Seven capitals (`H X T I E N F`) agree to the last
digit Manim prints, which is what makes cap height usable as a ruler.

**vello_gpu's scene API is identical to vello_cpu's.** Same `new` / `reset` / `fill_path` /
`stroke_path` / `set_paint` / `set_transform` / `push_layer` / `glyph_run` method set (verified
against 0.3.0). A dual backend is therefore cheap — but see the next point.

**The floor machine has no usable GPU path, and this is a driver bug, not a vello one.** Measured on
the Phytium D3000M (2026-10-16):
- **Vulkan**: the adapter enumerates and `vello_gpu::Renderer::new` succeeds, but `queue.submit`
  **segfaults** — reproduced with a minimal 3-vertex, zero-bind-group pipeline that never touches
  vello. The driver cannot execute a graphics pipeline at all.
- **GL** (`Phytium FTG340`): a plain triangle renders correctly, but vello_gpu's fragment shader is
  rejected by the vendor's GLSL compiler with `internal error : failed to add the source`.
- No software Vulkan fallback is reachable; `enumerate_adapters` returns exactly those two.

**`device.request_device` needs `required_limits: adapter.limits()` here.** `DeviceDescriptor::default()`
asks for `max_compute_workgroup_size_y = 256` and this device allows 128, so a default descriptor
fails outright.

**`vello_cpu` requires `ctx.flush()` before `ctx.render()` when the `multithreading` feature is on.**
Otherwise it panics with `attempted to rasterize before flushing`. Single-threaded builds do not need
it, so it is easy to omit by accident.

**Multithreading only pays above roughly 300–500 shapes.** It carries ~1.8 ms of fixed per-frame
overhead and is 2–3× *slower* below that. Measured single-threaded at 1080p: ~0.7 ms for 10 shapes,
~9.7 ms for 2000, ~46 ms for 10000 — i.e. a 60 fps budget accommodates roughly 3500 shapes /
14000 cubic curves. Typical Manim scenes are two orders of magnitude below that. NEON is detected;
single-thread throughput is ~392 Mpoints/s.

**`RenderMode::{OptimizeSpeed, OptimizeQuality}` is a runtime switch on `RasterizerSettings`,** not a
compile-time feature — so one binary can offer a fast preview and a deterministic f32 export path.

**`Pixmap::into_png()` exists in `vello_common`** (behind its default `png` feature), so frame export
needs no extra encoder for the simple case.

**vello keeps two separate stacks, and mixing them aborts the process.** `push_clip_path` pairs with
`pop_clip`; `push_layer` pairs with `pop_layer`. Calling `pop_layer` to close a clip underflows
vello's layer stack and takes the process down with a non-unwinding panic — there is no error value to
catch. `DrawOp` therefore has both `PopClip` and `PopLayer`; do not merge them.

**`slotmap`'s `serde` feature would put `{idx, version}` on the wire.** `NodeId` implements `serde` by
hand instead, as the opaque 64-bit `as_ffi` packing, so JSON carries a plain integer while the
generational check survives (`NodeId::raw` / `NodeId::from_raw` are the conversion points, and the
feature is off in `Cargo.toml`). `from_ffi` accepts any `u64` safely, so a hostile command can only
name a node that is not there.

**`kurbo` 0.13 has no `From<Rect> for BezPath`.** Use `rustim_math::rect_path` (or `fill_rect`, which
both backends expose) rather than reaching for a conversion that does not exist. `Point` also has no
scalar multiplication or point addition — go through `Vec2` (`p.to_vec2()`, `Vec2::hypot`).

**Manim's point array is read four points per curve, not three.** A flat array is
`(start, c1, c2, end)` per curve, and each curve **restates the previous curve's endpoint as its own
start**, so a closed 4-cubic circle is **16** points and a square is 4 curves, not 4 points. Reading
the array three at a time from index 1 — treating it as a leading point plus `(c1, c2, end)` triples —
shifts every control point by one and rounds a corner on any shape with four or more curves. This
survived review for a while because the element counts only diverge once four curves are involved: the
old degenerate `[A, A, A, B]` spelling of a straight segment agreed with the wrong reader by luck, the
module tests passed, and the corruption was visible only in the rendered image (every square's
top-right corner was rounded) — a reminder that a Bézier test suite over *small* arrays proves
nothing, and that the fix must be checked in the image too. `bezier::anchors`, `subdivide_bezier` and
`bezier_remap` all index four at a time for the same reason.

**A cubic Bézier cannot touch a circle anywhere but its endpoints.** For the 4-cubic approximation the
control points sit at `r * sqrt(1 + k²)` ≈ 1.142 r. Any test asserting "every point is at radius r"
is wrong; assert it of the on-curve points, and bound the curve's midpoint excursion (measured: under
1/3000 of the radius).

**Do not hand-derive a figure's bounds.** The Pythagorean figure is 10 units tall and 11 wide, not
`a + b = 7` tall — the square on the hypotenuse reaches y = 7 while the largest square reaches y = −3.
The first version of the scene overflowed the frame by 25%. `fit_and_centre` measures the union of the
point arrays instead.

**wgpu 30 API traps:** `Instance::new` takes the descriptor *by value*;
`InstanceDescriptor::new_without_display_handle()`; `enumerate_adapters` and `request_device` are
async; `PollType::Wait` is a struct variant (use `PollType::wait_indefinitely()`);
`PipelineLayoutDescriptor` has `immediate_size`, not `push_constant_ranges`; `multiview_mask`
replaces `multiview`; `get_mapped_range` returns a `Result`. The tutorial's winit sketch is pre-0.30
and will not compile — 0.30 needs `ApplicationHandler` + `EventLoop::run_app`.

**Do not approach `winit` 0.31 beta.** `accesskit_winit` only supports `^0.30.5`.

**The accessibility adapter must be built before the window is first shown, and every one of its
constructors takes an `&ActiveEventLoop`.** `accesskit_winit` panics otherwise. This is what forced
the window out of the pre-`run_app` creation the app had — the comment there argued for it on
grounds of not threading an `Option<Surface>` around, which was true and beside the point. The
window is now created in `resumed`, invisible, made visible once the adapter exists.

**In `accesskit` 0.25.1, `Role::Label`'s name comes from `value`, not from `label`** — the backends'
`name()` is `if label_comes_from_value() { value } else { label }`. So a text object's *content*
goes in `value`, and an announcement node must be `Role::Status`: as a `Label` it would be announced
with its own (empty) text on macOS. Related traps, all verified against the vendored source: there
is no `NodeBuilder` (`Node::new(role)` plus setters), no `set_name` (the property is `label`), no
`Role::StaticText`, no `ROOT_NODE_ID`, and live regions are only `Live`/`LiveAtomic` — `aria-relevant`
cannot be expressed. The root id is ours to pick, and it is `NodeId::RESERVED` rather than 0.

**`update_if_active`'s closure is not called while no assistive technology is attached** — the guard
is in all three platform backends, so "inactive costs nothing" is structural rather than an
optimisation to remember. The corollary is that nothing may depend on a call *having* happened: the
adapter is built with an event-loop proxy, which cannot supply a synchronous initial tree, so
**every push is a full snapshot**. Fine for four nodes; the cost of a snapshot per frame becomes
real at roughly 500 nodes, which is where an incremental update would have to be earned.

**`KeyEvent` cannot be constructed outside winit** (private `platform_specific` field), so a key
classifier that takes one is untestable. `rustim-app::input::classify` takes a `KeyPress` — the five
fields it actually reads — with `KeyPress::from_event` as the two-line adapter, which is what puts
the keymap in CI. `ModifiersState` lives in `winit::keyboard`, not `winit::event`, and arrives in
its own event, so Shift+Tab has to be tracked across events or it looks like Tab.

## Where the workspace diverges from the tutorial

The tutorial puts everything in a flat `src/` (`main.rs`, `mobject.rs`, `animation.rs`, `scene.rs`,
`examples/pythagorean.rs`). The workspace instead splits into crates so the semantic kernel can be
tested and ported independently; `cargo run --example pythagorean` will not work, and the reference
scene lives in `rustim-app`.

Issues the tutorial's sketches leave open, and where each stands now:

- **Coordinate system** — *resolved.* `rustim_math::Viewport` maps Y-up centred world units to
  Y-down pixel space. Scene code contains no pixel constants; the viewport is applied once, at the
  head of the draw list. The tutorial's own `RightTriangle` sketch has the right angle at the
  top-left despite its comment claiming bottom-left, under Y-down.
- **Animation lifetimes** — *resolved*, by ids rather than borrows (above).
- **Hardcoded viewport** — *resolved.* The app's `Resized` handler resizes the renderer and the
  viewport together, so the background and the projection follow the window.
- **`Formula` is a stub, and `typst::eval::World::default()` does not exist** (`World` is a trait you
  implement, and you must supply `Font`/`FontBook`) — *still open*; see `rustim-text`. Plain text is
  no longer part of this: `FontProvider` is the injectable font source Typst's `World` will need, and
  it already proves the "no system font paths" contract the tutorial's sketch ignores.
- **Per-frame allocation** — *resolved in the frame loop*: the `DrawList` is reused via
  `clear(w, h)`, which keeps its capacity.
- **Pinned dependency versions predate the APIs the sketches use** — verify every `winit` / `vello` /
  `wgpu` / `typst` call against the version actually pinned. The typst crates are tightly
  version-locked to each other.

## Conventions

- **3b1b palette.** Background `#121212`; foreground white; the Pythagorean scene uses blue `#64B5F6`
  for the *a* square, yellow `#FFD54F` for *b*, red `#EF5350` for *c*. These are `const`s in
  `rustim-color`, never inline literals.
- **World units, not pixels, in scene code.** Manim's default frame is 8 units tall, so legs of
  `3.0` and `4.0` mean what they would in Manim.
- **Easing** is `ease_in_out` (quadratic in/out, split at `t = 0.5`) by default, applied inside the
  animation.
- **Opacity composes multiplicatively**: a mobject's `opacity` times its style's `fill_opacity`. At
  draw time they are folded into the paint's alpha.
- **Commands are data.** Everything an AI or a host can do to the engine should be expressible as a
  serde command naming a node by `NodeId` and a property by name, and must round-trip through JSON
  unchanged. Refusals are `CommandError` values, never panics — a language model can reach every one
  of these code paths, so "that node does not exist" and "that value is out of range" have to be
  reportable.
- **No `Box::leak`, no global mutable state, no `unwrap()` on a size that came from a window.**

## Roadmap

Phase 0 (risk elimination) is done **and the skeleton it produced is verified green** — the toolchain,
the three probe crates, the window, PNG export, the workspace/CI skeleton, and the pixel-level Manim
comparison.

**P1 is in progress, and one step of it is left.** The order was not arbitrary: every step moves
`Mobject`, `DrawOp`, the command protocol or the renderer backend, and moving those before the
regression nets exist produces rework that is invisible until much later. Steps 1–4 shipped those
nets first for exactly that reason, so step 5 inherits a workspace where a break shows up as a
failure rather than as a wrong picture six weeks later.

| | P1 step | State |
|---|---|---|
| 1 | Numerical parity against Manim (426 cases at 1e-9) | done — see "Commands" |
| 2 | Golden-image suite: four scenes, per-platform baselines, CI job | done — see "Golden images" |
| 3 | Accessibility: structure + full keyboard + throttled announcements | done — see "Accessibility" |
| 4 | Text: `parley` end to end (string → glyphs → screen), bundled CJK, measured font size | done — see "Text" |
| 5 | Formulas: Typst → per-glyph outlines via `skrifa::OutlinePen`, never `typst-render` | **next** |

P1's deferred edges, deliberately: MathML flattening waits for Typst (step 5), self-voicing TTS for
P5, and reading the system's "reduce motion" preference for a later task — only the explicit
`ReducedMotion` switch and the pipe it feeds ship now. The CPU cost is **fill-area bound, not
shape-count bound** (4 large filled polygons at 720p already cost p50 2.25 ms of a 16.7 ms budget), so
damage-rectangle invalidation is likely worth more than any micro-optimisation.

The accessibility layer's own deferred edges, so they are not rediscovered as gaps: the
macOS/Windows backends are **compile-checked only** (this machine has no accessible bus, so nothing
beyond Linux compiles-and-hopes has been observed), a full-snapshot push per frame is fine at this
scale but is not the shape a 500-node scene wants, and `--reduced-motion` is the explicit switch
above rather than the system preference.

### P1 step 5 is where the next session starts

**The first move writes no product code.** `crates/rustim-text/src/lib.rs` promises a `formula.rs`
that does not exist yet, and the tutorial's `Formula` sketch is aimed at typst 0.11/0.2 — every call
in it is stale, and `typst::eval::World::default()` does not exist (that `World` is a trait you
implement, and you must hand it a `Font`/`FontBook`). So the first task is to fetch `typst 0.15.1`
and read its manifest, to establish the two things the design rests on:

- **the real crate split** — the `typst` facade against `typst-library` / `typst-layout` / `typst-kit`
  — i.e. which crate actually owns `PagedDocument`, `Frame`, `FrameItem` and `World`;
- **the exact `World` signature in 0.15.1**, since `FontProvider` is meant to be what backs it.

Only then write the pipeline: compile a `$...$` fragment → `PagedDocument` → walk `Frame::items()` →
`FrameItem::Text(TextItem)` → each glyph's outline through `skrifa::OutlinePen` → `kurbo::BezPath` →
a `FormulaMobject` of per-glyph mobjects, cached by `(font, glyph_id, size)`.

Four things already decided, so they are not rediscovered as open questions:

- **Never `typst-render`** (`方案建议.md` §8 item 8): it rasterises through tiny-skia and throws the
  per-glyph vectors away, and per-symbol addressability is the entire point of this route.
- **Fonts come from `typst-assets = { version = "0.15.1", features = ["fonts"] }`** (New Computer
  Modern and friends) — not a hand-placed file, and not a system path, which is the same contract
  `FontProvider` already honours. It is also where the `1.92` MSRV in the root `Cargo.toml` comes
  from.
- **`typst-layout` drags in frozen `rustybuzz ^0.20` / `ttf-parser ^0.25`.** Keep that subtree inside
  `formula.rs` so the eventual upgrade (typst#8172, expected in 0.16) is a one-file change. This is
  also where `skrifa` stops being a dev-dependency of `rustim-text` and becomes a real one; it is
  already the only `0.44.0` in the tree, so naming it adds no second copy.
- **No typst crate may reach the six wasm kernel crates.** `rustim-text` is deliberately *not* one of
  them, so compilation stays there and only `kurbo` points cross into `rustim-mobject` — a formula is
  a `NodeKind::Formula` mobject whose glyphs are ordinary mobjects, which is what keeps `rustim-render`
  free of `rustim-text` and the kernel free of Typst.

When it lands, the two commands that catch the failure modes above are
`cargo tree -i ttf-parser` (`rustybuzz`'s subtree must stay inside `rustim-text`) and
`cargo tree -i skrifa` (still exactly one `0.44.0`), on top of the usual wasm kernel build.

- **P2** — more mobjects (lines, arrows, axes, function plots) and animations (`Transform`, `Rotate`,
  `Scale`, `FadeOut`); morph via `align_points`.
- **P3+** — camera (pan/zoom/focus); video export; the AI command surface at full width; Web (WASM).

## AI integration

The product is an AI explainer assistant, so the engine exposes a JSON command interface
(`需求探讨.txt` is explicit that the AI should *drive* the engine, not write it):

```json
{ "action": "add", "type": "circle", "params": { "radius": 2.0, "label": "leg a" } }
{ "action": "set", "target": 4294967299, "property": { "name": "opacity", "value": 0.5 } }
```

`target` is a `NodeId`: an **opaque 64-bit integer**, not a small index (see the `slotmap` note above
— the low half is the slot, the high half the generation). Because the key is generational, a command
naming a deleted node fails cleanly instead of hitting a recycled object. Objects also carry an
optional human-readable `label`, which is what the accessibility layer speaks and what the AI layer
may address instead of the opaque id. Every command returns a `StateDiff` (`created` / `modified` /
`removed`), so a host learns the id of what it just added and a renderer learns exactly what to
invalidate.

Two wire-format invariants are load-bearing, both pinned by tests:
- **Commands round-trip byte-for-byte.** `to_string(parse(json)) == json`. A host logs, replays and
  diffs these, so anything that serialises back differently is a bug.
- **Absent shape parameters are omitted, never written as `null`.** These payloads are model output;
  echoing `"width": null` back would teach the model that nulls are part of the protocol.
