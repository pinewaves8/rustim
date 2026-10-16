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

The workspace itself is **verified green** as of 2026-10-17: **269 unit tests plus a doctest** pass,
`cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check` are clean, the
six-crate semantic kernel builds for `wasm32-unknown-unknown` (with `accesskit` reachable from
`rustim-app` and **not** from `rustim-scene`), all six golden scenes match their checked-in
baselines, and `rustim-app --png out.png` reproduces the Manim reference frame with an **identical
content bounding box** (`343,90,936,629`); only 0.012% of pixels differ by more than 2/255, and none
by more than 8. See `方案建议.md` §5.2. The count is not decoration — every one of those claims is
re-run by the commands below, and the P2 rows in the roadmap say which step last moved it.

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
cargo run -p rustim-app -- --scene formula --png out.png  # Typst → per-glyph outlines
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
`crates/rustim-math/tests/manim_parity.rs` replays **439 recorded Manim results** against them
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

The six scenes are there to disagree with each other: `pythagorean` is the one with a Manim
reference, `composite` covers deferred transforms, alpha and draw order through the mobject tree
walk, `primitives` pushes strokes and clips as raw `DrawOp`s because nothing in the mobject model
expresses either yet — without it the baselines would never touch stroke expansion or clip
rasterisation, the two most intricate things `vello_cpu` does — `text` is the only one whose pixels
come from a file in this repository rather than from code, so losing `assets/fonts/` fails here
rather than silently producing a blank frame for a user, `formula` is the only one whose
geometry comes from a *third-party* typesetting engine, which is what makes the image the only place
that can see a dropped fraction rule or a filled-in counter (see "P1 step 5"), and `transform` is the
only one whose still is the *end* of an animation rather than a static arrangement: it morphs a
3-curve triangle *up* into a circle and a 4-curve square *down* into a triangle, so one image shows
both directions of the alignment padding and that the style lerp arrived (see "P2 step 2").

Two things about the baselines themselves, both learned the hard way. **`BLESS=1` rewrites every
baseline**, not just the one under review, because `check()` copies the render unconditionally. Of
the five older baselines, four come back with different bytes and decode to pixel-identical images
(`composite`: 28255 against 28688, and the diff bbox is `None`); the fifth, `formula`, matches byte
for byte. Two renders of one scene from this binary *are* byte-identical, so this is a property of the
committed files rather than of the encoder run — the cause is not established and does not matter,
because the suite scores pixels. What does matter is that a bless run always leaves a non-empty diff,
in which `git checkout`ing the untouched scenes is part of reviewing it, and that **`cmp` between a
render and its baseline is a false alarm while `compare_png_files` is the honest check.**

**Adding a scene means editing two places in `crates/rustim-app/src/main.rs`**, and they are a
matched pair: the `scene_by_name` match (~:750) and the literal array in `scene_names()` (~:768) that
`--help` prints. The baseline guarantee above reads `--help`, so a scene wired into the match but not
into that array is *not* offered by `--help`, and the suite's check cannot see it — it compiles, runs,
and silently skips its baseline. Register it in both, then bootstrap the baseline for the arch you
are on; the other two platforms are bootstrapped from the CI artifact (above).

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
| `rustim-anim` | `Animation` trait, `AnimationScheduler`, `Sequence` (the beat queue), `FadeIn`, `MoveTo`, `Transform`, `Wait` | Real |
| `rustim-text` | Text shaping: an injectable `FontProvider` over `parley`/`fontique`, CJK from `assets/fonts`, `MissingGlyph` / `FontSubstitution` reports; `formula`, the Typst → per-glyph outline compiler | Real |
| `rustim-scene` | `Scene`/`SceneContext`, the serde command layer, `a11y` (the platform-neutral semantic tree) | Real |
| `rustim-render` | `Renderer` trait, `DrawList`, the `vello_cpu` backend | Real (CPU only) |
| `rustim-export` | PNG encoding, and the `compare` module that scores two frames | Real; ffmpeg pipe not written |
| `rustim-app` | The winit event loop, softbuffer presentation, keyboard input, announcements, the AccessKit adapter and the shaper↔rasteriser bridge (both as a **lib** target), six `--scene` entries, the golden suite | Real |

Dependencies point downwards only. `rustim-scene` depends on `rustim-render` for the `DrawList` type;
if the renderer ever grows a heavier surface, `DrawList` should move down instead.

### The frame

```text
winit event loop → Scene::update(dt) → SceneContext::advance(dt) → AnimationScheduler::update
                 → SceneContext::build_draw_list(&mut DrawList)
                 → Renderer::render(&DrawList) → softbuffer → screen
```

`SceneContext::build_draw_list` is the only place world coordinates are materialised. Everything
upstream mutates *local* points and an `Affine2`; the flattening step folds each mobject's transform
together with its ancestors' and emits paths.

### The timeline: three layers

| Layer | Type | What it is |
|---|---|---|
| One state change | `Animation` | A pure function of `alpha` in `0.0..=1.0`; owns no clock |
| What plays **now** | `AnimationScheduler` | Animations in parallel on one shared clock — Manim's `Scene.play(a, b)` |
| What plays **later** | `Sequence` | A queue of beats, one after another — Manim's `construct()` body |

The third layer exists only because rustim's loop cannot block. Manim's `construct()` *is* a list of
`self.play(...)` calls (`scene/scene.py:1234-1240`) and each one renders its whole segment
synchronously before returning (`manager.py:259-261`), so the blocking frame loop is the timeline.
Here `construct` returns, so the same script has to be written down as data: `ctx.play(anim)` /
`ctx.play_all([..])` / `ctx.wait(seconds)` append a beat, and `SceneContext::advance` plays them one
at a time. `Wait` is a no-op `Animation`, exactly as upstream
(`Scene.wait(d) == play(Wait(d))`, `scene.py:1242-1276`), which keeps "a beat is a set of
animations" uniform — no separate gap type, and `is_playing` stays true *across* a pause.

`AnimationScheduler`'s "one flat list, total `max(run_time)`, each child at `alpha = t /
its own run_time`" (`scene.py:1017-1026`, `:1184-1186`, `:1714-1724`) **is** Manim's `play(*args)`
semantics, so a beat reimplements nothing. What is not ported is `AnimationGroup` / `Succession`:
Manim expresses sequence by *composing* animations into one whose `alpha` sweeps a rescaled internal
timeline (`animation/composition.py:198-236`), which is a different feature (`lag_ratio` staggering)
and would rescale the durations of the beats this engine plays at face value.

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

`Scene` — `construct(&mut self, ctx: &mut SceneContext)` declares the objects and scripts the opening
beats with `ctx.play(..)`; `update(&mut self, dt: f64, ctx: &mut SceneContext)` runs once per frame.
This split is the real-time-loop architecture: Manim's `construct()` blocks, so its timeline is fixed
by the time it returns; ours is called once, and the loop then runs forever.

`Scene::update`'s default is `ctx.advance(dt)` and a scene that overrides it **must still call
`advance`** — draining the queue is the playback machine's one job, and a scene that called
`ctx.animations.update(..)` directly instead would leave the queue undrained: a script that stalls on
its first beat, which reads as a frozen frame. `TransformScene` is the canonical override: advance
first, then react to `!ctx.is_playing()` (it relabels once the morph has *settled*, which is not the
moment the morph was *queued* — that distinction was a bug in P2 step 2).

`Animation` — `duration()`, `begin(&SceneTree)` (called once at start, so an animation can capture
where its target actually is), `interpolate(alpha, &mut SceneTree)` with `alpha` in `0.0..=1.0`, and
`rate_func()`. Easing is applied **inside** the animation, never by the scheduler.
`AnimationScheduler` holds one shared clock and computes each animation's progress independently, so
durations may differ and the shortest simply finishes first.

**The clock is shared; the start is not.** `Running::started_at` records the clock when an animation
was queued and progress is read as `elapsed - started_at`, so an animation added to a batch already
in flight starts from its own zero. This is a bug fix, not a refinement: every scene used to call
`animations.update(dt, ..)` and *then* `add`, so the first frame of every animation was delivered one
frame too far along its own curve (`2·dt/d` instead of `dt/d`). No golden can see it — both ends clamp
to 1.0 and the still is ten seconds in — which is why `an_animation_added_late_starts_from_its_own_zero`
exists and states the counterfactual.

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

**`debug_assert!(expr)` does not evaluate `expr` in a release build, so a side effect inside one
happens in debug and *only* there.** `Formula::insert` reparents each freshly inserted glyph with
`tree.reparent(id, root)`. Written as `debug_assert!(tree.reparent(id, root))` — the spelling that
reads best — release builds never reparent, every glyph stays in the tree's root bucket,
`parent_transform` returns IDENTITY for it, and the whole formula ignores its own transform and
renders at the origin on top of every other formula. It looks like a layout bug, not a parenting
one. Call the function, keep the result in a `let`, assert *that*. This has now cost that function
twice; the comment there says so. The **golden suite cannot catch it**: `CARGO_BIN_EXE_rustim-app`
is the *debug* binary, so a release-only defect is invisible to both the unit tests and the
baselines. Run `cargo test --release` when touching anything with a `debug_assert!` in it.

**Typst's frame is y-down; its text runs are y-up, and the two are composed, not cancelled.**
`Frame::items()` gives `(Point, FrameItem)` in frame space (y down, `Abs` = points), while a text
run's internal cursor is y-up. Following `typst-render`'s own `render_text`, the outline is
`state · T(x_offset, −y_offset) · S(s, −s)`: the item's `Point` is a frame-space *translation*, and
the run's own offsets carry the flip. The consequence that matters is that the glyph outline arrives
y-up from `skrifa` (at `Size::unscaled()`, in font units) and the *same* sign flip serves both it
and the run — so a formula is centred and flipped once, by the affine `FormulaCompiler` builds, and
never per-glyph. Get the direction wrong and every glyph is mirrored about its own baseline
(baselines correct, letters upside down), which is the identical failure `glifo`'s composite has and
which reads as a font bug rather than a coordinate one.

**A flat Manim point array cannot express a subpath break, so a glyph's counter has to be welded
back in.** `bezier_path_from_points` (`rustim-mobject/src/tree.rs:269`) emits exactly one `move_to`
and only `curve_to` thereafter; a font glyph with a hole (`o`, `d`, `g`, `=`, `%`, `8`) is *two*
contours, and a counter filled solid is the visible result. `shapes::points_from_path` therefore
walks each `MoveTo` after the first out to a **hub** point on the outer contour, traverses a doubled
slit to the new subpath's start, and returns to the hub at the end (the "keyhole" retrace). Every
bridge is traversed once in each direction, so its winding contribution is zero and
`Fill::NonZero` leaves the counter empty. The test that guards it asserts a *winding number* at four
points — outside, on the ring, inside the counter, beside the slit — because winding is the quantity
the fill rule actually reads; "it looks right" cannot distinguish a welded glyph from a filled one.

**A formula's rules are strokes, and the draw walk only emits fills.** A fraction's bar and a
radical's overline are `FrameItem::Shape` carrying `Geometry::Line(..)`
(`typst-layout/src/math/fraction.rs:74`, `radical.rs:137`) — `Shape` stores geometry *plus* an
optional fill and an optional stroke, and these have no fill. `SceneContext::build_draw_list` emits
paths, so a stroke-only shape is dropped and the formula reads as a bug. `formula::filled_shape`
expands one into a filled quad of the stroke's own thickness (a rect for `Geometry::Rect`, `None`
reported into `skipped_shapes` for anything needing a real stroker). `FrameItem::Shape` is the one
thing this step needed that the tutorial's sketch does not mention at all.

**`Glyph::span` is the element's span, not the glyph's.** Every glyph produced by one math element
shares that element's `Span`, so it identifies a symbol only when one element produced one glyph.
`"abc"` in math gives all three glyphs the whole literal's range, and labelling them from it makes
three children with the same name. The usable rule is a comparison: use the source slice only when
its **character count equals the shaped character count**, and fall back to the shaped text
otherwise. (Related: math typesets a Latin letter as the *mathematical italic* codepoint — `x`
becomes U+1D465 — so a label taken from the shaped glyph is not the character the author typed,
which is what makes the source slice worth preferring at all.)

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
  implement, and you must supply `Font`/`FontBook`) — *resolved*: the sketch is aimed at typst
  0.11/0.2 and every call in it is stale, but the route is now implemented in
  `rustim-text::formula` and rendered by the `formula` scene (see "P1 step 5"). One correction to
  the sketch's premise: `FontProvider` turned out *not* to be the injectable source Typst's `World`
  needs — it holds a fontique collection, not bytes, so the `World` is fed from `typst-assets` plus
  `BUNDLED_DIR` directly. The "no system font paths" contract it proves is honoured either way.
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
- **`Scene::update` calls `ctx.advance(dt)` first**, always. See "Core traits" for what a scene that
  skips it does to the script.
- **A scene owns no timeline state.** No `stage: usize`, no copy of "which beat are we on" — the
  playback machine knows, and a second answer is a second thing to get wrong. What a scene keeps is
  state the *machine cannot express*: `TransformScene`'s `settled`, which is about the beat being
  over rather than about it being queued.
- **No `Box::leak`, no global mutable state, no `unwrap()` on a size that came from a window.**

## Roadmap

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
| 1 | Numerical parity against Manim (439 cases at 1e-9) | done — see "Commands" |
| 2 | Golden-image suite: six scenes, per-platform baselines, CI job | done — see "Golden images" |
| 3 | Accessibility: structure + full keyboard + throttled announcements | done — see "Accessibility" |
| 4 | Text: `parley` end to end (string → glyphs → screen), bundled CJK, measured font size | done — see "Text" |
| 5 | Formulas: Typst → per-glyph outlines via `skrifa::OutlinePen`, never `typst-render` | done — see "P1 step 5", `rustim-text::formula`, the `formula` scene |

P1's deferred edges, deliberately: MathML flattening (a formula is now a glyph tree with per-symbol
spans, so this is unblocked but unwritten), self-voicing TTS for
P5, and **reduced motion is unshipped in both forms** — nothing in the workspace reads a platform
"reduce motion" preference, and there is no explicit switch either (no `--reduced-motion` flag; the
binary's only flags are `--frames`/`--png`/`--inspect`/`--scene`). Do not go looking for a
`ReducedMotion` type: the sole motion-related accessibility control that exists today is
`rustim-app::announce`'s throttle. The CPU cost is **fill-area bound, not
shape-count bound** (4 large filled polygons at 720p already cost p50 2.25 ms of a 16.7 ms budget), so
damage-rectangle invalidation is likely worth more than any micro-optimisation.

The accessibility layer's own deferred edges, so they are not rediscovered as gaps: the
macOS/Windows backends are **compile-checked only** (this machine has no accessible bus, so nothing
beyond Linux compiles-and-hopes has been observed), a full-snapshot push per frame is fine at this
scale but is not the shape a 500-node scene wants, and reduced motion is unshipped entirely (see
above), so `MoveTo` animates at full speed for every user whatever their platform preference is set
to.

### P1 step 5: formulas — Typst in, per-glyph outlines out

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
   emits exactly one `move_to` (`rustim-mobject/src/tree.rs:269`), so a flat `Vec<Point>` has no way
   to start a second contour — and a glyph's counter (`o`, `d`, `=`, `%`) is a second contour. They
   are welded through a hub with a doubled slit, so every bridge cancels to zero winding and the
   counter stays open under `Fill::NonZero`.
2. **A fraction's rule and a radical's overline are *strokes*, not fills**, and the draw walk emits
   only fills. `FrameItem::Shape` with `Geometry::Line` is expanded into a filled quad by
   `filled_shape` — a formula with no fraction bar reads as a bug, not as a missing feature.
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
groups for layout but does not clip them. macOS and Windows `formula` baselines are bootstrapped
from the CI artifact like the other five scenes'.

The two commands that catch the failure modes above are
`cargo tree -p rustim-text -i skrifa@0.44.0` and `cargo tree -i ttf-parser` — **and the version is
named on purpose**: a bare `cargo tree -i skrifa` reports *two* versions, because the lock has a
second, older `skrifa` inside Typst's own image subtree (`hayro-interpret`), which is harmless but
reads like a mistake. Both, plus "no Typst crate reaches the kernel" and "`rustim-render` never
depends on `rustim-text`", run as the `typst-subtree` CI job.

- **P2** — more mobjects (lines, arrows, axes, function plots) and animations (`Transform`, `Rotate`,
  `Scale`, `FadeOut`); morph via `align_points`.
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
  the top-level array. `path_arc` / `path_func` (the default is a straight lerp, and the seam is one
  function pointer, so it is additive later) and sheen (`Style` has no `sheen_direction`) are out for
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
  (see "Core traits"). Deliberately not ported: `AnimationGroup` / `Succession` / `lag_ratio` (a
  *group* is one animation whose alpha sweeps a rescaled child timeline — sequential playback here is
  a driver concept, and a beat's duration means what it says), and interrupting a running beat (a
  command can only append; "stop that and do this instead" is a different, explicit command).
- **P3+** — camera (pan/zoom/focus); video export; the AI command surface at full width; Web (WASM).

## AI integration

The product is an AI explainer assistant, so the engine exposes a JSON command interface
(`需求探讨.txt` is explicit that the AI should *drive* the engine, not write it):

```json
{ "action": "add", "type": "circle", "params": { "radius": 2.0, "label": "leg a" } }
{ "action": "set", "target": 4294967299, "property": { "name": "opacity", "value": 0.5 } }
{ "action": "play", "animation": { "target": 4294967299, "type": "fade_in", "duration": 0.6 } }
```

`target` is a `NodeId`: an **opaque 64-bit integer**, not a small index (see the `slotmap` note above
— the low half is the slot, the high half the generation). Because the key is generational, a command
naming a deleted node fails cleanly instead of hitting a recycled object. Objects also carry an
optional human-readable `label`, which is what the accessibility layer speaks and what the AI layer
may address instead of the opaque id. Every command returns a `StateDiff` (`created` / `modified` /
`removed`), so a host learns the id of what it just added and a renderer learns exactly what to
invalidate.

`play` is the one command that **appends rather than mutates**: it queues an animation as the next
beat of the same timeline a scene's own `ctx.play` writes into, which is what makes the picture a pure
function of the command log — a host logs, replays and diffs a session, and gets the same frames. It
therefore returns an **empty** `StateDiff`, and honestly so: a diff describes the tree, and queueing a
beat does not change the tree, it changes what will happen to it. An interruption would be a *different*
command, spelled out in the data; `play` cannot preempt what is already playing.

Two wire-format invariants are load-bearing, both pinned by tests:
- **Commands round-trip byte-for-byte.** `to_string(parse(json)) == json`. A host logs, replays and
  diffs these, so anything that serialises back differently is a bug.
- **Absent shape parameters are omitted, never written as `null`.** These payloads are model output;
  echoing `"width": null` back would teach the model that nulls are part of the protocol.
