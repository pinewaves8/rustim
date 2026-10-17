# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository state

A Cargo workspace at the repository root, plus six documents and one directory of throwaway probes.

**Read the design documents in this order:**

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

`使用说明.md` is the fourth: the **user-facing manual**, for someone driving the binary rather than
changing it — the scene list, the mouse and keyboard map, the headless flags, the JSON command
vocabulary with examples, export, the browser demo, and troubleshooting. It is not required reading
for engine work, and this file is the development reference. The two must be edited together when a
flag, a wire field or a key binding changes: a stale manual is a user-facing wrong answer, and nothing
in CI reads it.

The last two are the **external-driver documents**, and they are the normative pair for anything an
AI assistant drives:

5. `驱动协议v1.md` — the wire specification: transports, the `hello` handshake and version rules,
   the command vocabulary, the reply shapes, the error vocabulary, conformance assertions, and what
   is explicitly unimplemented.
6. `贾维斯集成方案.md` — the architecture around that wire: why the engine and the assistant are
   separate products, the five switching mechanisms, the tool tiers, the repository layout, and the
   phased roadmap (Phase 0 is what has landed; the rest is planned).

A change to a command, a reply field, a refusal or a version rule is a change to **`驱动协议v1.md`,
`使用说明.md` and the conformance suite together**. "Confirmed by a doc" is not a test — the suite in
`crates/rustim-app/tests/conformance.rs` is what makes the wire's promises executable.

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

The workspace itself is **verified green** as of 2026-10-18: **677 tests** (unit, integration and
doctest) pass, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check`
are clean, the
six-crate semantic kernel builds for `wasm32-unknown-unknown` (with `accesskit` reachable from
`rustim-app` and **not** from `rustim-scene`, and with no parley or Typst in it), all eight golden
scenes match their checked-in
baselines, a script replays to a byte-identical frame, an exported frame equals the still pixel for
pixel, a scripted `add` of `text` / `formula` / `group` builds real objects through the host's
`Typesetter` (the desktop `hello` advertises all **11** shapes; the plain browser page advertises
nine, the formula page ten), and `rustim-app --png out.png` reproduces the
Manim reference frame with an **identical content bounding box** (`343,90,936,629`); only 0.012% of
pixels differ by more than 2/255, and none by more than 8. See `方案建议.md` §5.2. The browser host
builds both ways (with and without Typst) and was **run in Chrome 151**, where its canvas matches the
desktop still's content bounding box with a maximum channel error of 1 on 16 pixels, Chrome's own
accessibility tree holds the four named figures, a formula reaches it as `<math>` structure, and a
drag and a wheel dispatched through Chrome's input pipeline move the camera exactly as the desktop's
shared gesture code does — see "Web (WASM)". The **conformance suite** at
`crates/rustim-app/tests/conformance.rs` drives the real binary only through `rustim-client`, so the
protocol's framing promises are checked by a client that has no access to engine internals — see
"External drivers and the AI surface". The count is not
decoration — every one of those claims is re-run by the commands below, and the P2 rows in the
roadmap say which step last moved it.

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
  float results are not bit-identical across NEON and SSE. Web (WASM) is a *host*, not a port of
  `rustim-app`: it is `rustim-web`, and the day-one guards from `方案建议.md` §10.6 are what made it
  possible without touching the kernel (see "Web (WASM)").
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
cargo run -p rustim-app -- --a11y-probe    # windowed: did a screen reader attach? (600 frames)
cargo run -p rustim-app -- --frames 300    # run 300 frames then exit
cargo run -p rustim-app -- --scene primitives  # ...a named scene; `--help` lists them
cargo run -p rustim-app -- --scene text --png out.png   # CJK + Latin glyph runs
cargo run -p rustim-app -- --scene formula --png out.png  # Typst → per-glyph outlines
cargo run -p rustim-app -- --scene formula --inspect  # ...and each formula's MathML, in the tree
cargo run -p rustim-app -- --scene rotate --inspect  # rotate + scale, then a FadeOut removal
cargo run -p rustim-app -- --scene axes --png out.png  # axes, ticks, a plot and a camera beat
cargo run -p rustim-app -- --motion reduced  # ignore the platform "reduce motion" preference
cargo run -p rustim-app -- --captions --png out.png  # ...and draw the live region's sentence on it
cargo run -p rustim-app -- --speak --frames 300      # self-voicing (windowed only)
cargo run -p rustim-app -- --speak --voice cmn       # ...in espeak-ng's Mandarin voice
cargo run --release -p rustim-app -- --video out.mp4 --srt out.srt   # export 10 s via ffmpeg
cargo run --release -p rustim-app -- --video-png frames/ --srt out.srt  # ...or numbered PNGs
cargo run -p rustim-export --bin rustim-compare -- a.png b.png   # exit 0 pass / 1 fail / 2 cannot
cargo run -p rustim-app -- --script sessions/intro.jsonl --reply  # drive it with JSON commands
cargo run -p rustim-app -- --script - --reply --png out.png       # ...from stdin, answering each
cargo run -p rustim-app -- --script crates/rustim-app/tests/fixtures/script/session.jsonl --inspect
cargo test --workspace               # unit tests across all crates
cargo test -p rustim-math ease_in_out       # a single test by name
cargo test -p rustim-math --test manim_parity   # replay the checked-in Manim fixtures
cargo test -p rustim-app --test goldens         # compare renders to the checked-in baselines
BLESS=1 cargo test -p rustim-app --test goldens # ...and rewrite them (review the diff!)
cargo test -p rustim-app --test script          # the command feed, end to end, through the binary
cargo test -p rustim-app --test captions        # the band is in the bottom of the still, and replays
cargo test -p rustim-app --test speech          # --speak refuses the headless flags (no backend needed)
cargo test -p rustim-app --test export          # the export loop, the sinks and the .srt timings
cargo test -p rustim-app --test formula         # formula structure reaches --inspect, via the binary
cargo test -p rustim-app --test conformance     # the protocol, driven only through rustim-client
cargo test -p rustim-client          # the client's framing rules, over in-memory streams
cargo test -p rustim-export --test compare_cli  # the visual-regression CLI's exit codes
cargo test -p rustim-text --test manim_text_scale  # check the font size constant (needs no Python)
cargo test -p rustim-web             # the browser session core, natively (no Chrome, no wasm)
cargo test -p rustim-web --features formula  # ...and the scene Typst creates
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

The browser host and its two artifacts (see "Web (WASM)"):

```bash
cargo build --target wasm32-unknown-unknown -p rustim-scenes -p rustim-web
cargo build --target wasm32-unknown-unknown -p rustim-web --features formula   # Typst, deliberately
tools/build-web.sh                     # both artifacts + their raw/gzip sizes, printed
node tools/web-check.mjs               # real Chrome: pixels, AX tree, drag/wheel, command bridge, MathML
tools/build-web.sh && (cd web && python3 -m http.server 8000)   # ...or look at it by hand
gzip -9 -c web/pkg-formula/rustim_web_bg.wasm | wc -c            # the formula page's download
```

`tools/build-web.sh` writes `web/pkg/` and `web/pkg-formula/`, both gitignored; `web-check.mjs` needs
Chrome (`$CHROME`, default `google-chrome`) and no npm install, and exits 2 rather than 1 when it
cannot ask the question (no browser, no DevTools endpoint). It is not in CI: the runner has a Chrome,
but a check never run against an image is a claim rather than a test — what CI does is compile both
artifacts.

Always benchmark in `--release`. The debug profile is not meaningfully representative for a
rasteriser — and *not at all* for an **export**: `--video-png` in a debug build spends ~0.6 s per
1280×720 frame in the unoptimised `flate2` (the whole 600-frame default is six minutes), against
~8 ms per frame under `--release` end to end. That is why `tests/export.rs` stays at a few dozen
frames, and why the commands above name `--release` for the export examples.

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

**Manim numerical parity (`rustim-math`).** `crates/rustim-math/src/bezier.rs`, `space_ops.rs`,
`paths.rs` and `rate.rs` are literal ports of `manim.utils.bezier`, `manim.utils.space_ops`,
`manim.utils.paths` and `manim.utils.rate_functions`, and
`crates/rustim-math/tests/manim_parity.rs` replays **771 recorded Manim results** against them
(`tests/fixtures/manim-{bezier,geometry,space-ops,rate}.json`, checked in — the suite needs no
Python). The acceptance rule is `1e-9` absolute on every coordinate and rate value, with no "close
enough" allowances: the ports reproduce Manim's *rounding paths*, not just its formulas, which is the
only way the numbers land in the same place.

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
`crates/rustim-app/tests/goldens/<scene>/<os>-<arch>.png`. Four properties carry the suite:

- **Every baseline render passes `--motion full` explicitly.** What the motion mode does to a still
  frame is worth asserting (there is a test for it) but the baseline itself must not depend on the
  machine's desktop settings — the same reason the `text` scene ships its font rather than asking the
  system for one.

- **Baselines are per platform *and per arch*.** `vello_cpu` rasterises in f32 and NEON and SSE/AVX
  do not round identically (§10.2(3)), so one shared image would fail everywhere it was not made.
  `every_scene_the_binary_offers_has_a_baseline` reads the binary's own `--help` and fails if the
  registry and the suite disagree in either direction, so a new scene cannot skip its baseline.
- **A missing baseline fails.** Never skip: that is how a whole platform goes unchecked. The failing
  run leaves its render in `target/golden-artifacts/`, which the CI job uploads, so bootstrapping a
  platform is "download, review, commit". **Only `linux-aarch64.png` is checked in for each scene
  today.** The macOS and Windows baselines have *not* been made: this machine cannot reach
  `github.com`, so the artifacts the CI job would have uploaded are unreachable from here, and the
  suite therefore fails on those two platforms by design rather than skipping them. That is
  development-list item 1, blocked on how the artifacts are to be obtained (see the memory note and
  the roadmap); do not invent baselines or weaken the missing-baseline rule to make the job green.
- **The thresholds are §5.2's measured ones and are not to be relaxed**: identical content bounding
  boxes, zero pixels differing by more than 8, maximum channel error 3. The comparison lives in
  `rustim_export::compare` (not in the test), and the `rustim-compare` CLI is a thin shell over the
  same function — so the suite and a host cannot drift onto two notions of "the same image".
  `rustim-app`'s `--png` renders *the scene after a fixed 10 s of virtual time*, not "as many frames
  as the scene happens to need" — otherwise a duration change would silently rebless a golden as a
  capture of a half-faded frame.

The eight scenes are there to disagree with each other: `pythagorean` is the one with a Manim
reference, `composite` covers deferred transforms, alpha and draw order through the mobject tree
walk, `primitives` pushes strokes and clips as raw `DrawOp`s because the mobject model still has no
clip — its stroke half is now redundant with `axes`, whose plot is a real `DrawOp::StrokePath`, but
it is the only scene that exercises *clip* rasterisation, and keeping both ends of the stroker
under test is cheaper than trusting one — `text` is the only one whose pixels
come from a file in this repository rather than from code, so losing `assets/fonts/` fails here
rather than silently producing a blank frame for a user, `formula` is the only one whose
geometry comes from a *third-party* typesetting engine, which is what makes the image the only place
that can see a dropped fraction rule or a filled-in counter (see "P1 step 5"), `transform` is the
only one whose still is the *end* of an animation rather than a static arrangement: it morphs a
3-curve triangle *up* into a circle and a 4-curve square *down* into a triangle, so one image shows
both directions of the alignment padding and that the style lerp arrived (see "P2 step 2"), and
`rotate` is the only one where an object **leaves the scene**: a `FadeOut` removes its square as its
third beat, so the still has two objects where the script named three (see "P2 step 5"), and `axes`
is the only one shot through a **non-default camera**: a `CameraMove` beat lands on
`center (0.15, 0.2), zoom 1.25`, and the still holds welded axes with ticks, a **stroked**
parabola, a number line and integer tick labels drawn through that same projection — so a camera
that moved the fills and not the glyphs would show up here and nowhere else, and it is the only
mobject-driven golden whose pixels go through the backend's stroker (see "Axes, number lines and
plots" and "Strokes").

**A baseline's *bytes* are not a property of the image, and `BLESS=1` rewrites all of them** —
`check()` copies the render unconditionally, not just the one under review. In the step-5 bless run
four of the six older baselines came back with different bytes and decoded to **pixel-identical**
images (`composite`: 28255 bytes against 28688, RGBA extrema all `(0, 0)`, diff bbox `None`), while
`formula` and `transform` matched byte for byte. The cause turned out to be the deflate backend:
`typst-library` enables `flate2/zlib-rs`, feature unification applies it to the whole graph, and every
`png` write in the workspace changed its compressed stream. The two that already matched were blessed
*after* that lock change; the four dated from before it. So a bless run can leave a non-empty diff in
which nothing moved, **`git checkout`ing the untouched scenes is part of reviewing one**, and **`cmp`
between a render and its baseline is a false alarm while `compare_png_files` is the honest check.**

**Two tests must not render to the same artifact path.** `render_at`'s file name contains the
caller's `tag` for exactly this reason: the golden test and the reduced-motion comparison both ask for
`(scene, "full")`, cargo runs tests in parallel threads, and the reader intermittently got a file the
other thread had just truncated — `transform-full-linux-aarch64.png: unexpected end of file`, from a
test whose own two renders were fine.

**Adding a scene means editing two places in `crates/rustim-app/src/main.rs`**, and they are a
matched pair: the `scene_by_name` match (~:997) and the literal array in `scene_names()` (~:1017) that
`--help` prints. The baseline guarantee above reads `--help`, so a scene wired into the match but not
into that array is *not* offered by `--help`, and the suite's check cannot see it — it compiles, runs,
and silently skips its baseline. Register it in both, then bootstrap the baseline for the arch you
are on; the other two platforms would be bootstrapped from the CI artifact, which is still owed
(above).

## Architecture

### Crate map

The workspace splits along the "semantic kernel vs. the world" boundary. The point of the split is
that `rustim-math` / `rustim-color` / `rustim-mobject` / `rustim-anim` / `rustim-scene` carry **no
rendering backend, no platform code and no I/O**, so they can be unit-tested without a rasteriser,
reused by the exporter and the AI layer, and compiled to `wasm32-unknown-unknown`. CI enforces that
last property.

| Crate | Contents | Status |
|---|---|---|
| `rustim-math` | Points, `Affine2`, rate functions and the `Rate` wire vocabulary, the pixel-only `Viewport` and the `Camera` view (world→screen), the pointer gesture vocabulary (`gesture`: drag, scroll, zoom factor, zoom range) shared by both hosts, and literal ports of Manim's `bezier` / `space_ops` / `paths` / `rate_functions` | Real |
| `rustim-color` | The 3b1b palette as `const`s | Real |
| `rustim-mobject` | `Mobject` (points / declared `local_bounds` / a wire-shaped `text: Option<TextBlock>` / style / deferred transform / `label` / `NodeKind`), `SceneTree` (with `camera`, `snapshot` / `insert_subtree` / `reorder_children`), stable `NodeId`, `Subtree`, shape builders (`square` / `circle` / `right_triangle` / `line` / `arrow` / `number_line` / `axes` / `plot` / `group`) and `AxesScale`, `text` (`TextBlock` / `TextRun` / `FontData` — the glyph geometry a shaper hands over), `critical_point`, `world_anchors`, `ids_with_label` | Real |
| `rustim-anim` | `Animation` trait (`begin` / `interpolate` / `finish`), `AnimationScheduler`, `Sequence` (the beat queue), `AnimationGroup` / `Succession` (lagged composition), `FadeIn`, `FadeOut`, `Create`, `Uncreate`, `MoveTo`, `Rotate`, `Scale`, `Transform` (with `path_arc`), `TransformGroup` (family morph, also arced), `CameraMove`, `WithRate` (a rate override wrapper), `Wait` | Real |
| `rustim-text` | Text shaping: an injectable `FontProvider` over `parley`/`fontique`, CJK from `assets/fonts`, `MissingGlyph` / `FontSubstitution` reports; `formula`, the Typst → per-glyph outline compiler, which also carries each formula's structure as MathML | Real |
| `rustim-scene` | `Scene`/`SceneContext`, the serde command layer, `a11y` (the platform-neutral semantic tree), `typeset` (the `Typesetter` seam — a trait, no fonts in it) and `text` (`TextBlock` → `DrawOp::GlyphRun`, shared by the draw walk and both hosts) | Real |
| `rustim-render` | `Renderer` trait (`render`, `render_damaged`), `DrawList` and its `Damage` diff, `RenderStats`, the `vello_cpu` backend | Real (CPU only) |
| `rustim-export` | PNG encoding, the `compare` module that scores two frames, the `VideoSink` seam (`FfmpegSink`, `PngSequenceSink`), the `srt` writer, and the `rustim-compare` binary | Real |
| `rustim-scenes` | Scenes shared by more than one host — currently `Pythagorean`, which the desktop binary's `--scene pythagorean` and the browser demo both run | Real |
| `rustim-app` | The winit event loop, softbuffer presentation, keyboard and pointer input, announcements, the AccessKit adapter, the shaper↔rasteriser bridge, `typeset` (the desktop `Typesetter`: parley text, Typst formulas, installed on every `SceneContext` the binary builds), the command feed (`feed`: JSONL in, replies out, the one apply point), the caption band (`caption`) and self-voicing (`speech`) — all as a **lib** target — the export loop (`export_timeline`), eight `--scene` entries, the golden, script, caption and export suites | Real |
| `rustim-web` | The browser host: `Session` (scene + context + renderer + draw list, target-independent and testable natively), `aria` (the semantic tree → flat ARIA projection), `pointer` (DOM wheel/pointer units and signs → `rustim-math::gesture`, pure and natively tested), `typeset` (the formula-only `Typesetter`, `from_embedded`, under the `formula` feature), `web` (the wasm shell: canvas, rAF loop, pointer/wheel listeners, DOM/ARIA overlay), the command bridge and the optional `formula` scene | Real (Chrome-verified) |
| `rustim-client` | The **client half** of the command protocol: `read_reply` (one reply line, the presence rule enforced) and `StdioClient` (spawn, `send`, `send_raw`, `write_raw`, `hello`, `close`, seq checking). No engine logic, no rendering, no platform code — it spawns a process, which is a host's job, so it is **not** one of the six wasm kernel crates | Real |

Dependencies point downwards only. `rustim-scene` depends on `rustim-render` for the `DrawList` type;
if the renderer ever grows a heavier surface, `DrawList` should move down instead. `rustim-web` sits
beside `rustim-app`, not under it: both are hosts of the same kernel, and neither depends on the
other. `rustim-client` is not a host and not a kernel crate: it is the workspace's own *external*
consumer of `rustim-scene`'s protocol, existing so that the conformance suite cannot lean on
anything a driver outside the workspace lacks.

### The frame

```text
winit event loop → Scene::update(dt) → SceneContext::advance(dt) → AnimationScheduler::update
                 → SceneContext::build_draw_list(&mut DrawList)
                 → DrawList::damage_since(&previous_frame) → Damage
                 → Renderer::render_damaged(&DrawList, Damage) → softbuffer (damaged region) → screen
```

`SceneContext::build_draw_list` is the only place world coordinates are materialised. Everything
upstream mutates *local* points and an `Affine2`; the flattening step folds each mobject's transform
together with its ancestors' and emits paths.

### Damage rectangles

A frame that changed one object's opacity should not cost a whole-frame rasterisation, and an idle
frame should cost nothing at all. `DrawList::damage_since(&old) -> Damage` answers this from the draw
lists alone — no scene bookkeeping, no dirty flags on mobjects — and `Renderer::render_damaged`
rasterises only what it says.

```rust
pub enum Damage { None, Full, Region(PixelRect) }
```

`PixelRect` is a half-open pixel rectangle (`x0..x1`, `y0..y1`) in the frame's own coordinates.

**The diff is positional and conservative.** Ops are paired by index, carrying the `(transform,
paint)` state each one draws under; an op that differs *or* whose governing state differs marks both
its old and its new bounds, and the result is the union. That is O(ops) with no allocation, and it is
correct across the draw walk's actual shape — one `SetTransform` at the head of the frame, then
`SetPaint` + `FillPath`/`StrokePath` per mobject — because a paint change and a transform change each
mark exactly the geometry they govern. It is deliberately not a diff of *geometry*: two lists with
their ops in a different order are treated as a full repaint rather than matched, and a `GlyphRun` is
one op, so a change to any glyph damages the union of that run's glyph boxes. Conservative is the
right side to err on.

**The escapes to `Full` are the cases a region render cannot express.** A size or background change
(the frame is new, or what is under every pixel changed); a transparent background (`None` means "do
not touch the target", which a fresh region cannot honour — it would leave the previous frame's
pixels inside the region); and any `PushClip`/`PopClip`/`PushLayer`/`PopLayer`. The last is not
caution: vello's clip and layer stacks are *positional*, and a region render starts a fresh context,
so the state at the top of the region is not the state the full frame would have had there. A change
entirely outside the frame is `None`; an identical list is `None`.

**The region render shifts the scene rather than the target.** In vello_cpu 0.3.0
`RasterizerSettings::offset` is `(u16, u16)` — an *unsigned* offset, so a region at `(x0, y0)` cannot
be expressed as a negative offset from the frame's origin. `CpuRenderer::encode_with_origin` therefore
pre-multiplies every `DrawOp::SetTransform(affine)` with `translate(-x0, -y0)`; the geometry, the
glyph-run transforms and the background rect all move by the same integer amount. The render happens
into a **reused scratch `Pixmap`** sized to the damage rectangle, initialised with
`TargetInit::Clear(background)` — `SrcOver` would preserve the scratch's previous contents, which is
the opposite of what a reprojected region needs — and is then copied row by row into the persistent
pixmap. (`PixmapMut::new` demands exactly `width * height * 4` bytes with no stride, so a region of
the main pixmap cannot be viewed in place.) The blend is exact because the shift is an integer: the
tests assert an incremental frame is **pixel-identical** to a full one — `assert_eq` on the buffers,
not the golden suite's tolerance — including antialiased rotated shapes and a region crossing the
frame edge.

`RenderStats { painted, frame }` is what makes the feature observable: `painted` is the area the
rasteriser had to compute, which is the number the damage rectangle controls. Frame time is the wrong
witness — it moves with the machine, the clock and the scheduler. The acceptance test for this step
is stated in those terms: a one-property change repaints less than 1/20th of the frame, and the
result is indistinguishable from a full render (`crates/rustim-scene/tests/damage.rs`, over the real
`SceneContext` draw walk, not hand-built op lists).

The trait's default `render_damaged` renders the whole list and reports the whole frame, so a backend
that cannot cut out a region remains correct; `Damage::None` is honoured by every backend, which is
the cheap and common case — a paused scene, or a script that has finished, presents nothing.

In the app the rebuilt list and the list on screen ping-pong through `prev`, and the swap happens
only after a frame was rasterised **and** presented: a failed frame leaves `prev` alone, so the next
attempt is diffed against what the window still shows rather than against an image that never
reached it. `blit_damage` copies the rectangle out of the renderer's buffer into softbuffer's (their
widths agree because `Resized` sizes both), and for `Damage::None` it presents nothing at all. The
periodic frame line gained `N px repainted`, and prints `prev.ops.len()` because the lists were
swapped.

### The camera

`rustim_math::Viewport` is the **window** — a frame size in physical pixels — and knows nothing
about what part of the world fills it. `Camera { center, zoom }` is the **view**, and it lives on
`SceneTree` as `tree.camera`, not in `SceneContext`. That placement is forced: `interpolate` is
handed `&mut SceneTree` and nothing else, so a camera move has to be an ordinary `Animation`, and
`rustim-anim` cannot depend on `rustim-scene` (that would be a cycle). Manim's camera frame is a
mobject for the same reason. **The camera is a field, not a node** — `len()`, `root_ids()`,
`draw_order()`, the accessibility tree and the Tab order never see it, and `SceneTree::clear()` puts
it back to the default so a rewritten scene does not stay zoomed into the previous one.

`visible_height() = Camera::WORLD_HEIGHT / zoom` with `WORLD_HEIGHT = 8.0` (Manim's default frame),
`pixels_per_unit = frame_height / visible_height`, and
`affine = T(W/2, H/2) · S(s, -s) · T(-center)`. **The default camera reproduces the pre-camera
projection bit-for-bit** (`8.0 / 1.0 == 8.0`, `720 / 8.0 == 90.0`, same operation order) — that is
the invariant the seven pre-existing goldens were re-rendered against, unchanged, when the camera
landed. `Camera::default()` is hand-written because a derived one would be `zoom = 0.0`, a camera
that divides by zero.

`CameraMove` lerps `center` linearly and `zoom` **in log space** (zoom is multiplicative; a linear
lerp gives most of the visual speed to the first half). `alpha >= 1.0` assigns the destination
verbatim rather than computing `from + (to - from) * 1.0`, so reduced motion and repeated runs land
on bitwise-identical state.

The wire command is `{"action":"camera", "center"?, "zoom"?, "focus"?, "duration"?}`, answered by
`SceneContext::handle` (it needs the playback machine, or the viewport's aspect ratio for `focus`):

- **without `duration` the camera is written now; with one — including `0.0` — a single
  `CameraMove` beat is queued.** An instant write is *overwritten by a move already in flight*, the
  same contract `Set position` has with a running `MoveTo`: an animation owns its subject until it
  ends.
- `focus` frames a node by its **world bounding box** with a `1.2` margin; the tighter axis decides
  the zoom, and one zero dimension is fitted by the other (a horizontal line is a legitimate
  subject). Then explicit `center` and `zoom` override only their own half. A text or formula node
  has no points but carries a declared box (see "Accessibility"), so it is framed like any other
  node; a point-less node with no declared box is still refused.
- It returns an **empty `StateDiff`**, for `play`'s reason: the view is not a node.
- Refusals are values: a missing `focus` node is `NoSuchNode`; no geometry/zero extent, a
  non-finite or non-positive `zoom`, a bad `duration`, or a command naming none of the three is
  `OutOfRange`.

`a11y::build` projects the bounds with `tree.camera`, and `SemanticTree.camera: CameraView` travels
with them (`center`, `zoom`, frame pixels, and the visible world width/height) so a host can map the
pixel bounds back to world coordinates without re-deriving the projection. `text_ops`/`push_text`
take the **composed world→device transform** (`SceneContext::affine()`), not a `Viewport`: there is
deliberately no camera-free variant, because a caller that forgot the camera would draw
correct-looking text in the wrong place while every filled path moved correctly.

### Pointer input

The window handles three pointer events and they are the camera's manual controls: **left-button
drag pans**, **wheel/trackpad scroll zooms about the cursor**. Before this there were no pointer
events at all — the event loop matched keys, resize and redraw and dropped everything else.

- **The arithmetic is pure, in `rustim-math::gesture`.** `Camera::panned(viewport, delta_px)` is one
  division by `pixels_per_unit`: dragging 90 px at zoom 1 moves the centre by one world unit, with
  the y sign flipped because screen y runs down. `Camera::zoomed_about(viewport, factor, cursor_px)`
  zooms first, then shifts the centre by the displacement that puts the **same world point back
  under the cursor**, so the view grows around the pointer rather than jumping. Both return `self` on
  a degenerate viewport, zoom or delta: pointer input has no error channel, and the tree's
  invariant — finite, positive zoom — has to survive whatever the platform sends.
- **The wheel's factor is multiplicative, `exp(k · delta)`.** Equal scroll steps multiply the zoom by
  equal ratios, so ten notches in and ten out return to the start and a notch feels the same at every
  scale; adding a constant would be a huge jump zoomed out and invisible zoomed in. winit reports
  notches (`LineDelta`) and trackpad pixels (`PixelDelta`), which are not convertible — the OS decides
  how many pixels a notch is — so `gesture::ScrollDelta` keeps the unit and `zoom_factor` gives each
  its own rate (`0.25`/line ⇒ ×1.284 per notch, `0.002`/px). Horizontal scroll is dropped rather than
  folded in.
- **The clamp is the host's, and it clamps the factor rather than the zoom.** `0.05..=64` is the range
  in which a gesture stays reversible; a script may still ask the command layer for any positive
  zoom. `gesture::clamped_zoom_factor(current, factor)` re-expresses the factor so the product lands
  on the limit exactly — clamping the resulting zoom instead would move the centre and break the
  cursor-anchor invariant.
- **Both gestures end at the command layer**, as `{"action":"camera","center":…,"zoom":…}` with no
  `duration` — the same command an AI or a script sends, through the same validator. A pointer is one
  more host of the protocol, not a side door into `tree.camera`. An instant write is not an
  animation, so pointer input is motion-mode independent; and an in-flight `CameraMove` still owns
  its subject, so a drag during one is overwritten until the move ends (the documented contract
  `Set position` has with `MoveTo`).
- **The state a gesture needs is `gesture::Pointer`, shared with the browser.** It owns the cursor
  (winit's `MouseWheel` and `MouseInput` carry no position; a wheel before the pointer ever entered
  has no anchor and is ignored), the drag (`Drag`), and the camera the current gesture has built. The
  desktop releases the drag on `CursorLeft` and on losing focus, because the button-up can happen
  outside the window where no `MouseInput` follows — coming back in must not pan with a button nobody
  holds. `Drag::press` deliberately keeps the last position, so the first movement after the button
  goes down produces a correct delta instead of being swallowed as the anchor.
- **A gesture continues from the previous gesture, not from the screen.** A cursor can move several
  times between two frames, and each delta has to apply to the camera the *earlier delta of that
  frame* produced — reading `tree.camera` every time keeps only the last move and makes a fast drag
  trail the pointer. `Pointer` keeps that pending camera in `record`; `App` calls `end_frame()` once
  the frame has applied its queued commands.
- **The browser host is the second consumer**, not a reimplementation: `rustim-web` feeds the same
  `Pointer` and writes the resulting camera through `Session::set_camera`, which is `Command::Camera`
  through `Session::apply`'s `handle`. See "Web (WASM)" for the DOM side of the translation.

The classifier is winit-free except for two-line adapters (`input::wheel_delta`,
`KeyPress::from_event`), so the gesture arithmetic is tested in CI where there is no window: the pan
follows the pointer at every zoom, zooming about a cursor keeps its world point fixed to `1e-12` at
four start zooms × four cursor corners × four factors, malformed factors are the identity, and the
clamp lands exactly on both limits.

The real-machine evidence is an external gesture, because no unit test can prove winit delivered
anything: `xdotool` drags the window by 100 px over 1600×900 (112.5 px/unit, so the query reply's
`camera.center.x` becomes exactly `-0.888888…`) and sends one wheel event (`xdotool mousedown 4`,
which makes `zoom` exactly `1.2840254166877414 = e^0.25`), read back through `--script - --reply`
and a `query`; the cursor's world point is the same before and after the zoom. **All eight goldens
are unchanged** — the step adds an input path, not a drawing one. Deferred, deliberately: touch and
pinch, inertia, right/middle-button gestures, pointer capture beyond the window edge, and having a
click move keyboard focus.

One measurement trap belongs here because it cost a confusing run: **`xdotool click 4` is not one
notch.** It sends a button press *and* a release, and winit 0.30's X11 backend maps buttons 4–7 to
`MouseWheel` on both (`event_processor.rs:1083-1091` ignores the event's press/release state for
those buttons), so a `click 4` zooms twice and the reply reads `e^0.5`. A real wheel is unaffected —
the same source notes evdev reports it as axis motion, which arrives as one event — but an
automated check has to use `mousedown 4`, or assert `e^0.5` and say why.

### Axes, number lines and plots

Three `ShapeKind`s — `NumberLine`, `Axes`, `Plot` — complete the mobject vocabulary the command
layer already named (`NodeKind::{NumberLine, Axes, Plot}`), and each is built by
`rustim_mobject::shapes::build` from one flat `ShapeParams`:

- **`NumberLine`** and **`Axes`** are **one welded filled path**, not a subtree. A subtree would
  make every tick an independent mobject: `Command::Add` can return only one id, opacity would not
  inherit (a transparent axis with lit ticks), the accessibility tree and Tab order would each tick
  as its own image, and `world_bounds` on a point-less parent would return `None` — which is
  `focus`'s input and the a11y `bounds`' source. So the shafts and every tick rectangle are
  `rect_path`s (same winding, explicitly closed), welded into one `BezPath` and handed to the
  existing `points_from_path` (hub + doubled zero-winding slit, `Fill::NonZero`). The consequence,
  and it is a trade rather than a gap: **a tick is not addressable and cannot be styled on its own**.
- **Every tick is clamped to the shaft.** Without the clamp the end ticks protrude half a stroke
  width past the axis and the corner set is longer than the axis is; the a11y description measures a
  number line by the **diameter** of its corners (the rotation-invariant rule arrows use), so an
  unclamped line was announced as "长度 6.05 的数轴" when it is 6 long.
- **`Plot` is a stroked centerline, not a filled polyline.** Feeding sampled points to the fill rule
  closes the polyline with its chord and paints the region under the curve — a graph would render as
  an area, the single most visible wrong picture this suite could hold. The points are instead
  exactly the samples, and the mobject's style selects `stroke_opacity = 1, fill_opacity = 0`; the
  draw walk emits a `DrawOp::StrokePath` and the backend's stroker expands it (see "Strokes"). The
  host supplies the points **in world coordinates**; there is no expression language and no parser
  in the command layer, by decision — a scene samples with Rust.
- **`AxesScale::new(x_range, x_length, y_range, y_length).to_world(x, y)`** is the shared data→world
  map for the scene's own samples *and* the ticks' positions, so a curve and its grid cannot drift
  apart. It centres on zero, scales each range onto its length, and returns `0.0` on a degenerate
  axis rather than dividing by zero.

Guards are values, not panics, because `ShapeParams` arrives from a model: non-finite, `min >= max`,
`step <= 0` and `NaN`/infinite steps degrade to no ticks or to the defaults, and **`MAX_TICKS = 256`
per axis** caps what one `add` can allocate. The `axes` scene is the one golden where all three
objects, integer tick labels drawn through the camera, and a `CameraMove` beat meet: one image
proves the camera reaches fills, glyphs and accessibility bounds at once (see "Golden images").

### Strokes

`Style` has always carried `stroke` / `stroke_opacity` / `stroke_width`; until P3 step 2 the draw walk
read only the fill half and the stroke fields were latent. `SceneContext::build_draw_list` now emits
a `DrawOp::StrokePath { path, width }` as well as, or instead of, the `DrawOp::FillPath`, and the
backend hands it to vello's stroker.

Four properties are load-bearing:

- **Each pass has its own alpha**, both folded with the mobject's opacity: `m.opacity *
  style.fill_opacity` and `m.opacity * style.stroke_opacity`. That is what lets a plot be a stroke on
  an *empty* fill without a second style type, and what makes `FadeOut` fade a stroke at the same rate
  as a fill.
- **Fill first, stroke second.** A stroke straddles its path — half inside, half outside — so
  drawing it under the fill buries half its width. This is Manim's cairo order too.
- **`stroke_width` is in *world units*, not Manim's pixels, and that is a deliberate divergence.**
  vello's `stroke_path` strokes in path space and applies the frame transform afterwards
  (`render.rs:229` passes `state.transform` alongside the `Stroke`), so the visible width is
  `width · pixels_per_unit` and it **scales with the camera** — zooming in thickens the pen, which is
  what moving closer should mean. Manim's pixel width is right for a fixed canvas and wrong for a
  viewport the user can zoom. `Style::default().stroke_width = DEFAULT_THICKNESS` (0.045, the same
  number an arrow's silhouette uses) rather than Manim's 2.0: the default stroke is off, but a host
  that turns `stroke_opacity` up without naming a width must get a hairline, not a two-unit border.
- **A zero or non-finite width is skipped**, not emitted: `vello`'s stroker should never see it, and a
  transparent stroke still costs a flattening and a strip build. Zero is *legal* in the command layer
  (`stroke_width` refuses only negative and non-finite) because "make the outline vanish" is a
  meaningful thing to ask for.

Two consequences in the mobject vocabulary:

- **`Line` is a two-anchor stroked centerline; `Arrow` is still a filled contour.** A flat point array
  cannot be a stroked shaft plus a filled head — one path has one paint role — so the arrow stays the
  filled silhouette it was and the line becomes the object the new pass exists for.
- **`Plot` changed from a ribbon outline to a stroked centerline** in this step, so the `axes` golden
  is the one image that moved. Its samples are now the geometry itself, the fill is off, and this is
  the first mobject-driven golden whose pixels go through the stroker (the `primitives` scene uses
  `DrawOp`s directly).

The command layer exposes `stroke_opacity` (0..=1), `stroke_color` (`[r, g, b]`) and `stroke_width`
(finite, `>= 0`), all writing `dirty.style` like `fill_opacity` does.

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
semantics, so a beat reimplements nothing.

### Rate functions

`rustim_math::rate` is a literal port of `manim.utils.rate_functions`: **45 exported curves plus
11 parameterised fixture cases** replayed against 56 recorded Manim results
(`manim-rate.json`, a 101-point `t` grid). It is also the engine's *wire vocabulary* — `Rate` is a
serde enum with one variant per exported curve, so a command names `"ease_in_out_sine"` and nothing
downstream needs a name table.

Three things about the port are deliberate:

- **`smooth` is Manim's sigmoid, not the cubic.** The engine's historical `smooth` (the
  `3t² − 2t³` cubic) was Manim's `smoothstep`; it was renamed, and `smooth` now means
  `smooth(t, inflection = 10)`. The historical `ease_in` / `ease_out` / `ease_in_out` survive as
  **bit-identical aliases** of the quad curves, so the engine default and the wire's
  `ease_in_out_quad` cannot diverge (a test compares them to the bit).
- **`RateFunc` is a plain `fn(f64) -> f64`, so a parameterised curve cannot be one.** Each ported
  function takes Manim's parameters explicitly (`smooth(t, inflection)`); `Rate::func()` supplies
  the defaults through private one-argument shims, and a caller wanting another value passes a
  **non-capturing closure** — `|t| smooth(t, 4.0)` coerces to `RateFunc`. The wire carries only the
  default set; a parameter is not on it.
- **Parameters are part of the port, not an extension.** The fixture's second group records eleven
  non-default calls (inflection 4, pull factor −0.3, pause ratio 0.5, a squish over `[0.2, 0.8]`, …),
  so "the parameterised spelling agrees with Manim" is measured rather than assumed.

Three curves do not do what "rate function" suggests, and the fixture pins all three:
`there_and_back` and its pausing sibling **end at 0** (a scene that settles on them settles where it
started), `wiggle` is a there-and-back too, `running_start` dips **below zero** (it backtracks before
moving), and `exponential_decay` **never arrives** (`1 − e^{−10} ≈ 0.9999546`). Manim's
`@unit_interval` / `@zero` decorators are not ported: they clamp arguments, and `Animation::ease`
already clamps once (`rate::clamp01`), so a curve need only be correct on `[0, 1]` — exactly the
domain the fixture feeds it.

On the wire, a `play` animation takes an optional `"rate"` naming one of the 45; absent means the
animation's own default (`ease_in_out`, i.e. Manim's `ease_in_out_quad`). An unknown name is a serde
error at parse time, not a `CommandError` — it never becomes a command. `rustim_anim::WithRate` is
the wrapper that applies it: it overrides `rate_func` and forwards `duration` / `begin` /
`interpolate` / `finish`, the last of which is the `Box<dyn Animation>` forwarding trap again — so
`a_wrapped_fade_out_still_removes_its_target` drives it through the scheduler rather than calling it.
For a `group` / `succession` the rate wraps the **composition** (shaping the whole timeline), while
each member may carry its own `rate` recursively; a `WithRate` around a group does not replace the
members' curves. `Rate::ALL` is the array `--help` prints, and a feed test proves the documented
names are exactly the ones the wire accepts.

### Composition: a fourth layer, inside the first

`AnimationGroup` / `Succession` (`rustim-anim/src/composition.rs`) are the port of
`animation/composition.py`, and they are **one animation built from a list** — not a fourth driver.
That placement is the whole design: a beat can hold a composed timeline without the scheduler or the
queue knowing it exists, a composed animation can be nested inside another, and `Sequence` keeps
meaning what its doc says (one beat at a time; a beat's duration is `max(duration)` over its members,
not something an inner layout can rescale).

The layout is Manim's, verbatim: `start[0] = 0`, `start[i] = start[i-1] + d[i-1] · lag_ratio`,
`end[i] = start[i] + d[i]`, duration `max(end)` unless `with_duration` rescales the internal timeline
(which changes the speed, not the shape). The group's own `rate_func` is **`linear`** — its alpha is
a position on the timeline, and every member eases itself from its own sliced alpha
(`child.ease(alpha)`), so an ease applied to the group would ease the timeline itself.

Three behaviours differ between the two kinds, all pinned in `composition.rs`:

- **A group begins every member eagerly**, at the group's `begin`, so each member's `begin` sees the
  tree as it is before the group starts. That is Manim's own choice, and it is why a `MoveTo` inside
  a group captures the *pre-group* position.
- **A succession begins a member when its turn arrives** and calls that member's `finish` as it is
  left behind, so "scale, then rotate" composes: the rotate's `begin` sees the scaled transform.
  A group instead `finish`es every member at the group's end, whether or not it had finished its own
  curve — the same reason its `finish` first forces `interpolate(1)` on each child
  (`Animation.finish`'s own first line).
- **`finish` reaches the children**, which is what makes a `FadeOut` *inside* a group or succession
  actually remove its target. This is a new route into `Animation::finish`, beside the scheduler and
  `Sequence`'s reduced branch, and it inherits the `Box` forwarding trap: `AnimationGroup::finish`
  holds `Box<dyn Animation>`, so the blanket impl has to forward.

One deliberate difference, forced by the trait: Manim's `Animation.begin` ends with
`interpolate(0)`, and our `begin` only has `&SceneTree`. A member that begins at alpha 0 therefore
first hears from the group on the next frame, at its own small alpha. The settled frame is unaffected
(both ends clamp), and the alternative — making `begin` take `&mut SceneTree` — would change every
animation's signature to reproduce one redundant write.

On the wire a group is `{"type":"group"|"succession", "animations":[...], "lag_ratio":0.2,
"duration":2.0}` with **no `target` of its own** (that is why `AnimationSpec::target` is optional;
a group names members, not a node). An explicit `duration` rescales the layout; an absent one is the
layout itself, so `succession`'s default (lag 1.0) makes two one-second members a two-second
animation. The refusals say which field is wrong: a `target` on a group, no members, an empty member
list, a non-finite or negative `lag_ratio`, and any member's own refusal (a missing target, an
unknown node, a bad duration) is reported under the *member's* id.

### Family morphs: transforming a subtree

`Transform` morphs one node's own point array and stops there. That covers every shape in
`rustim-mobject`, and it says nothing about a group: a group has no points, it has children, and the
thing a viewer calls "the morph" is the pairwise morph of those children. `rustim-anim::family`
(`TransformGroup`) is that recursion, and it is the port of `Mobject.align_data`
(`mobject/mobject.py:3107-3142`).

Three rules are kept:

- **The shorter side is padded with faded copies of its own children**
  (`add_n_more_submobjects`, `:3209-3232`). The copies are **interleaved proportionally**, not
  appended: the split is Manim's exact `(j · n) // m` arithmetic, so two children becoming five
  order as `[c0, c0', c0'', c1, c1']` with split factors `[3, 2]`. Each copy is `.fade(1)`: opacity
  zero throughout the subtree, because **opacity does not inherit in the draw walk** — a transparent
  parent with opaque children would draw the children anyway.
- **Pairs are aligned recursively by index**, each pair running its own alignment including its own
  padding.
- **`align_points` reconciles each pair's own arrays**, which is a no-op for groups (both empty) and
  the whole story for leaves.

Where the mutation happens is where this engine differs from Manim, and the trait forces it. Manim
aligns *in place* inside `begin`; here `Animation::begin` is handed `&SceneTree`, deliberately, and
an animation that starts must not restructure the scene. So `begin` builds an immutable **plan** and
the first `interpolate` materialises it: `SceneTree::snapshot` reads the donor into a detached
`Subtree`, `insert_subtree` inserts the padded copies with fresh `NodeId`s, and `reorder_children`
puts each parent back into Manim's interleaved order (insertion can only append, so without the
reorder the copies land on top of the siblings they overlap). Nothing observes the difference:
`interpolate(0)` is the first frame either way, and reduced motion drives the same path once. The
copies **stay in the tree** afterwards — that is Manim's outcome too — so a one-child group morphed
into a two-child one really does contain two nodes at the end.

The donor is read once and never written, and the source keeps its own `NodeId`, kind and label —
the same limit `Transform` has: nothing in the engine knows a square became a circle, so a caller
that cares has to relabel (see "Accessibility" for what `--inspect` showed when one did not).

On the wire it is `{"type":"transform","target":A,"into":B}` (plus the optional `path_arc`, see the
next section), with `into` a node id that must already exist. Two morphs
the engine refuses by staying still — a shape into a group (`null_point_align` is not ported) and a
node into itself or its own ancestor — are refused by `animation_for` with a `CommandError` instead,
because a host told a beat played healthy should not find it did nothing.

### Morph routes: `path_arc`

Manim's `Transform(path_arc=…)` bends every point's route into an arc
(`utils/paths.py:96-140`). `rustim_math::paths::path_along_arc` is a literal port of that function,
covered by 220 recorded cases like the rest of the parity suite. Four things about it are decisions,
not accidents:

- **It is a per-point function of an angle, not a closure.** Manim builds a `path_func` closure and
  stores it on the animation. A closure cannot cross the serde command boundary and the AI layer
  names an animation by data, so the animation stores the `f64` and calls the function per point.
  Dropping Manim's `axis` argument is what lets the signature be a plain `fn`: the only axis this
  engine can express is out of the screen, so it is `OUT` by construction rather than by argument.
- **The arc is measured in the parent's space, and costs the deferred transform.** Manim's points are
  world points, so its arc bends the real route. Here a node's points are local and part of its
  displacement may sit in `Mobject::transform`; arcing the local arrays alone would make `path_arc` a
  **no-op for the commonest request** — "move this there along an arc", written as a translated
  target. So both sides are folded through their own transforms first (`from_transform · a`,
  `to_transform · b`), the arc is taken there, and the result is written back as local points with an
  **identity** transform. A plain `Transform` keeps its O(1) deferred transform; only an arced one
  pays with it, and only while it runs.
- **Both ends are written, not computed.** `path_along_arc` at `alpha = 1` is a rotation that lands on
  the endpoint only to within rounding; a morph that settles a few ulps off its target becomes a drift
  under a second morph, and reduced motion must produce the full-motion settled frame. `path_point`
  returns the endpoints exactly. This is a deliberate divergence from Manim, which computes both.
- **`0` is the straight path, and so is anything below `STRAIGHT_PATH_THRESHOLD` (0.01).** The
  sub-threshold branch is `(1 − a)·start + a·end`, in that order, because that is Manim's
  `straight_path` — not `start + (end − start)·a`. The sign of the perpendicular is
  `cross(OUT, v/2) = (−v.y/2, v.x/2)`; the other operand order is the same vector negated, which bows
  the path inside out while every endpoint still matches.

`TransformGroup::with_path_arc` applies the same rule per child, each in its own parent's space, so a
child whose motion is entirely deferred still bends. On the wire it is one more optional field:
`{"type":"transform","target":A,"into":B,"path_arc":1.2}`. Declaration order is
`… duration, into, path_arc, animations`, so serde writes it in that order; absent is omitted, never
`null`, and a non-finite angle is refused under `path_arc` (every *finite* angle is legitimate —
Manim 0.22 performs no range check, and `±π` are its own `clockwise_path` / `counterclockwise_path`).

### Reduced motion

`rustim_anim::Motion` is `Full` or `Reduced`, and it is consulted in **exactly one place**:
`Sequence::start_next`, where a beat is handed to the clock. There is no `is_motion()` on `Animation`
and no per-animation opt-out, because at the granularity a preference can honestly act on, motion and
a pause are the same shape — write your end state, then occupy time.

> A reduced beat is not interpolated: every animation in it is begun and driven to `ease(1.0)` once,
> and a single `Wait` of the beat's nominal length — `max(duration)` over its members, the same total
> the scheduler would have used (`scene.py:1184-1186`) — takes its place on the clock.

Four things follow, and each is why this is one branch and not a feature:

- **The rhythm is untouched.** Every beat boundary lands at the same virtual time in both modes, so
  `is_playing`, the `ANIMATION_DONE` announcement, and a scene reacting to its timeline having drained
  (`TransformScene`'s relabel) are all correct with no change at all.
- **`Wait` needs no special case**, which is the same statement as "a pause is a beat with nothing to
  snap".
- **The settled picture is identical**, because a finished animation *is* its end state — Manim's own
  reading (`Animation.finish` forces `interpolate(1)`, `animation/animation.py:216-227`). Measured, not
  assumed: all eight scenes render **byte-identical** stills at `--motion full` and `--motion reduced`,
  and `--inspect` prints the identical semantic tree, labels and measured sizes included. The suite
  asserts two of them (`reduced_motion_leaves_the_settled_frame_identical` and its `rotate` twin,
  `max == 0` — not the golden tolerance), and the second is not redundant: for a *removal* the reduced
  path is a different call site (below).
- **It is cheaper**: one frame of state per beat instead of N.

`Motion` is passed into `SceneContext::new`, and it is **changeable at runtime** through
`Command::Motion` — the wire's `{"action":"motion","mode":"reduced"}` and the `m` key, which queues
the same command. Both go through `SceneContext::handle`, the one apply point, and the mode is read
once per beat in `Sequence::start_next`. So a switch takes effect at the **next beat started**: a
running beat keeps the policy it was handed, and a beat that was already reduced cannot replay its
skipped flourish when motion is turned back on. A scene that consults `ctx.motion` while scripting in
`construct` still sees the construction-time mode, because no command can arrive before `construct`
returns. The host reads the preference once, at startup: the `--motion full|reduced` flag, else
`rustim_app::motion::from_platform()` — which is **best effort and Linux only**. It runs
`gsettings get org.gnome.desktop.interface enable-animations` (the GNOME-family schemas, which UKUI
here honours; KDE is not read and the XDG portal has no such key) and returns `Full` for everything
else: a missing command, a missing schema, an unparsable answer. macOS
(`NSWorkspace.accessibilityDisplayShouldReduceMotion`) and Windows
(`SPI_GETCLIENTAREAANIMATION`) are named in that module and deliberately unread, so those two
platforms have the flag and not the preference; neither is a listener, so a desktop setting changed
mid-run is not observed — the explicit switch is the runtime route. That mapping lives in
`rustim-app` and not in the kernel for the same reason `accesskit` does: `rustim-anim` compiles to
wasm and a crate that spawns a process does not.

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
where its target actually is), `interpolate(alpha, &mut SceneTree)` with `alpha` in `0.0..=1.0`,
`finish(&mut SceneTree)` (called once, after the last `interpolate`, when the animation has reached
its end — Manim's `clean_up_from_scene`; a default no-op keeps the other animations out of it), and
`rate_func()`, plus `ease(raw)`, which clamps and applies it. The scheduler computes each
animation's **raw** progress and calls `anim.ease(raw)` — it never applies a curve of its own, so an
animation can change its curve without the scheduler knowing. `AnimationScheduler` holds one shared
clock and computes each animation's progress independently, so durations may differ and the shortest
simply finishes first.

`finish` is the hook `FadeOut` removes its target through, and it has **three call sites** — the
scheduler's completion loop, and `Sequence`'s reduced branch, where a beat's members never reach the
scheduler at all. It runs only for animations that *complete*: `AnimationScheduler::clear` and
`Sequence::clear` do not call it, so a dropped script leaves its objects in the tree rather than
half-removing them.

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
| `SemanticTree` | `rustim-scene::a11y` | Roles, names, descriptions, screen-pixel bounds (measured, or declared for a point-less glyph tree), and a formula's `mathml`. **No accesskit, no platform** — it compiles to wasm |
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

A **line**, an **arrow** and a **number line** are the same rule applied to figures whose box is not
a measurement at all — a line's is `3 × 0` now that it is a centerline (it was `3 × 0.045` while it
was a filled rectangle, which is the stroke width read out as a dimension). A line's corners *are*
its two anchors, so its length is exact; an arrow's corners are neither its tail nor
its tip (the tail is two corners, the tip a seventh), so it is measured by the **diameter** of its
corner set, which is its tail-to-tip length to within `t²/2L`; a number line is the same diameter
rule, because its box height is tick decoration rather than a dimension. All three are
rotation-invariant. That is not a refinement either: read from the box, the `rotate` scene's arrow —
turned through 34° — was announced as "长度 2.49" when it is 3 long. A **number line** also has a
name of its own — "数轴", measured "长度 6 的数轴" — while `Axes` and `Plot` keep the generic
`w × h` sentence, where the box is honest.

**One bounds exception, and it is a declaration rather than a measurement: a text or formula node
carries its own box.** A text mobject has no point array — its geometry is a glyph run the scene
pushes — so `world_anchors` has nothing to measure, and `Formula::insert`'s root is a container with
no points either. `Mobject::local_bounds: Option<Rect>` is therefore a fallback consulted **only when
`points` is empty**, never an override: a node with points answers exactly as it did before, a ragged
array still answers `None`, and every measured-bounds rule above is untouched.
`SceneTree::world_bounds` applies the fallback through `parent_transform · transform`;
`Mobject::critical_point` applies it through `self.transform` alone, because that function answers in
the **parent's** space (see "Hard-won facts"). `rustim-app::text::local_bounds` derives the box from
`ShapedText.width/height` and `Formula::insert` takes the union of its glyphs' local points. It is a
**line box, not an ink box** — a glyph may paint outside it. The same field is what lets `camera
focus` frame a text object and what gives `MoveTo`/`Rotate` a centre to work with.

`a11y::build` splits the two uses deliberately: `bounds` comes from `SceneTree::world_bounds` (so a
text node now has one), while `shape` and `describe` still come from `Shape::of(anchors)` — a text
description stays its label alone, with no size sentence, because a `Scale` animation changes the
size every frame and a changing description is an accessibility property-change event.

A description also **never contains a position**: on macOS and Windows a changed description is an
accessibility property-change event, and `MoveTo` rewrites the transform every frame of an
animation, so a position in the text would re-announce every node sixty times a second. Size enters
the description, position lives in `bounds`, which the backends only report when it really changed.

The conversation with a platform is deliberately narrow: Tab/Shift+Tab move focus, the arrow keys
become `Property::Opacity` **commands** (the same ones an AI sends, through the same validator),
space toggles playback, `,`/`.` step a frame, Enter re-reads the focused object, Escape drops focus,
and `m` flips the motion mode (`{"action":"motion"}` — see "Reduced motion"). The pointer has two
gestures of its own — drag to pan, wheel to zoom — which also become commands rather than direct
writes (see "Pointer input"); a click does not move the keyboard focus, because "focus follows
click" is a decision about the Tab order and not a side effect of panning.
`rustim-app::announce` throttles announcements and holds rather than drops a burst, so an animation
ending says where things ended up instead of narrating the whole transition.

**The adapter sends a full snapshot only when it must.** `A11yCore` keeps the last `SemanticTree`
and title it handed to a backend (`sent`); while that is present every update is a **difference**:
only nodes that were added or whose projection changed, the window node when the top-level children
or the title changed, the live-region node when its text changed, and the focus on every update. The
rules are the consumer's, not ours — a parent must arrive in the same update as an added or removed
child, a removed node must not be named, a title change needs no full snapshot (the window node alone
carries it). The tests replay every update through `accesskit_consumer`, the implementation the
platform backends themselves use, because an assertion over our own `TreeUpdate` cannot prove them.
Two reset points force `sent = None` and a complete snapshot: `InitialTreeRequested` (the consumer
attached and holds nothing) and `AccessibilityDeactivated` (the platform dropped our tree); the
invalidation is wired at both sites in `main.rs`, and neither is redundant — a consumer attached
fresh on macOS must never receive a difference from a tree it never had.

**A focused node can be deleted by an animation.** `FadeOut` removes its target in `finish`, and
`FocusState` only resyncs on a key or AT event, so the projection would otherwise push a focus
pointing at a node no longer in the tree — `accesskit_consumer` panics on that. `to_update`
therefore releases a stale focus first (an O(1) `tree.get`); syncing the whole focus walk every frame
would cost the tree walk the incremental update exists to avoid.
`removing_the_focused_node_releases_the_focus` is the panic regression.

**`--a11y-probe` is the real-machine instrument.** It opens the window, offers one announcement at
startup, runs 600 frames by default (`--frames` overrides) and prints a report on **stderr**: the
platform, the scene, the exit-time semantic node count, whether an assistive technology attached,
and the full/incremental push counts. The counters double as the attachment signal because
`accesskit_winit` has no `is_active`: with no AT the `update_if_active` closure never runs, so **zero
pushes is the honest answer, not a failure** — the report says so and tells the reader to check the
screen reader. It is windowed-only and refuses `--inspect`, `--png` and `--reply` (`exit(2)`),
because the headless paths have no backend to ask. The report text is a pure function
(`ProbeReport::render`), tested active, inactive and zero-count, so its wording is pinned without a
screen reader.

Verifying it by hand, per platform:

```bash
cargo run -p rustim-app -- --scene pythagorean --inspect   # the tree, no screen reader needed
cargo run -p rustim-app -- --frames 300                    # Tab, arrows, space, `,`/`.`, `m`
cargo run -p rustim-app -- --a11y-probe                    # did a reader attach? report on stderr
cargo run -p rustim-app -- --captions                     # the same sentences, as a caption band
cargo run -p rustim-app -- --speak                        # ...and aloud, if a synthesiser exists
# macOS: start VoiceOver (Cmd+F5), then run the probe. Windows: Narrator (Win+Ctrl+Enter),
# then run the probe. Linux: Orca needs a session bus — with none, accesskit_unix skips
# silently, so nothing here has been heard. The probe is what makes the other two machines
# a ten-minute check rather than an unfinished claim; the audible result still needs them.
```

### Narration, captions and self-voicing

The live region reaches exactly the people who have a screen reader attached. Two features close that
gap, and they share one pipeline: **whatever the live region is saying is also what the caption draws
and what the built-in voice speaks.**

- **`{"action":"narrate","text":"…","at":2.0}`** puts a line on the scene's own timeline.
  `SceneContext` gained a private `clock`, advanced in `advance(dt)` — the same clock a windowed
  `frame_dt` and the headless 10 s `settle` drive, so both kinds of run expire narration by the same
  rule. `take_narration()` drains the lines whose time has come, sorted by due time.
  **`at` is a delay from when the command arrived, not an absolute time**, because a file feed applies
  every line at t = 0 (that is what makes replay deterministic); with no `at` a line is due
  immediately. Two narrations pushed in one frame are all drained in that frame, which is a script
  pacing problem, not an engine one. The command is a harmless no-op in `apply` and is intercepted in
  `SceneContext::handle`, returning an empty `StateDiff` for `play`'s reason: the tree did not change.
  Refusals are `OutOfRange` under `text` (empty or whitespace — the payload a model most easily sends
  by accident) and `at` (negative or non-finite: "in the past" is not "now").

- **`--captions` draws the live region at the bottom of the frame.** `rustim_app::caption` appends the
  band after the scene's own ops, so it is a *screen layer*: it takes neither a `Viewport` nor a
  `Camera`, and 24 px stays 24 px at every zoom. Consequently it is also the visual twin of what a
  screen reader would say — a Tab focus announcement, `动画结束` and a scripted narration all appear
  there. The font provider is built once (19 MB parsed per caption would be absurd), the band reshapes
  on a text or frame-size change, and a line expires after `hold_seconds(text)` — about eight
  characters a second, clamped to 3–12 s — of **wall** time, so a paused scene does not keep showing a
  sentence nobody is reading. A still keeps it: `--png --captions` deliberately never expires, because
  the still's answer to "what was said by t = 10" must not depend on how long the process took.

  The transform is the whole trick. `text_ops` computes `place = to_device · world ·
  layout_to_local(w, h)` with `layout_to_local = T(-w/2, +h/2) · S(1, -1)`; the caption passes
  `to_device = S(1, -1)` and `world = T(x0 + w/2, -(y0 + h/2))`, and the two y flips cancel to the
  **identity** linear part. Anything else — composing the camera, or flipping the position instead of
  the layout — leaves a net negative y scale, which silently moves the run off `glifo`'s hinted path
  while still drawing correct-looking text (see "Text").

- **`--speak` reads the same text aloud.** `rustim_app::speech` (app layer only: the kernel may not
  spawn a process) tries `espeak-ng` then `espeak` on Linux, `say` on macOS, and Windows PowerShell's
  `System.Speech` assembly. The sentence always travels on **standard input**: `espeak-ng --stdin`,
  `say` with no file arguments, `[Console]::In.ReadToEnd()` on Windows. Args would need quoting per
  platform, would truncate on some, and would leave the prose in every process listing; `command_for`
  is pure so all three spellings are tested on any one platform. `Speaker` owns a thread and a
  channel, because speech takes seconds and a frame takes milliseconds; dropping it stops at the end
  of the current sentence rather than joining. No backend is reported in one line and the run
  continues — a bare Linux is the common case, and captions still work.

- **`--voice` is the host's half of self-voicing.** The backend's default voice is the
  distribution's, and an English default on Chinese prose is not an accent: espeak-ng reads every
  Han character as the words "Chinese letter", so a caption reading `场景 pythagorean，4 个对象` is
  heard as a burst of "Chinese letter", one per character — which sounds like the engine repeating
  itself and does not match the caption at all. `--voice NAME` forwards the name **verbatim**:
  `-v NAME` for `espeak-ng`/`espeak` and `say`, `SelectVoice` for PowerShell's `System.Speech`,
  because a voice name is backend vocabulary (`cmn` fixes Mandarin in espeak-ng; macOS's equivalent
  is `Tingting`) and this module cannot know which names a machine has. On Windows the name travels
  in the `RUSTIM_VOICE` environment variable rather than being spliced into the `-Command` string:
  it is host input, and the command string is not a place to quote it. `--voice` without `--speak`
  is a refusal (exit 2), like `--srt` without a video mode. The module still never picks a voice on
  its own — auto-detection would need each backend's listing syntax, and a hard-coded `-v cmn` breaks
  every machine that lacks it.

- **A screen reader wins.** `--speak` goes silent once `full_pushes + incremental_pushes > 0`, i.e.
  once an assistive technology has ever attached (the counters only move inside `update_if_active`),
  and comes back automatically when it detaches. Two voices reading the same sentence is worse than
  one. The caption is unaffected; it is for the people neither voice can reach.

- **Narration bypasses the announcer, on purpose.** `A11yCore::narrate` writes `live` directly.
  `offer` goes through the 750 ms throttle, which *holds one slot and drops what follows*, so a
  scripted lecture with lines closer together than that would lose them. Narration is content, not an
  event; a burst is said in full by the built-in voice, and the live region (one slot) carries the
  last line. The host sets `last_live` when it calls `narrate`, so the next frame does not mistake the
  engine's own narration for a fresh change and say it twice.

- **The opening summary is injected on the first frame's narration drain**, after that frame's
  `a11y.update`, not in `resumed`: a static scene offers `动画结束` on frame one (`was_animating`
  starts true) and the live region has one slot. With an opening narration, the narration wins —
  saying "this is the scene" over the first thing the script wants to say is not an introduction.

The dependency is one-directional and worth stating: captions and speech consume the **same**
`live_text()` the adapter pushes, so there is no second source of truth about what was said. The cost
is one frame of latency (the band is read after the update that produced the sentence) and the
announcer's merge window for announcements — which is why narration does not go through it.

### Video export and subtitles

`rustim-export` owns both halves of "get this out of the process": the frame comparison and the
**sink**. `VideoSink` is the seam (`write_frame(rgba, w, h)` and `finish`), and the two
implementations are the two honest answers:

- **`FfmpegSink`** pipes raw RGBA to an `ffmpeg` binary on `PATH` — `-f rawvideo -pixel_format rgba`
  in, `libx264`/`yuv420p` out. It is `--video out.mp4`.
- **`PngSequenceSink`** writes `frame-00000.png`, … with this crate's own encoder. It needs no
  external program, so it is the one that works on every platform the engine targets, and it is the
  artifact a determinism claim can be made about (an mp4's bytes move with the encoder's build; the
  frame stream does not). It is `--video-png DIR`.

**The frames are premultiplied and the sinks disagree about what to do with that.** `Renderer::pixels`
is premultiplied RGBA8; every scene fills the background first, so every frame is opaque and
premultiplied == straight. `PngSequenceSink` un-premultiplies anyway (`straight_rgba`, a no-op for
`a == 255`), because a PNG of a transparent frame should be right; `FfmpegSink` pipes the bytes
through as they are, because converting 600 frames of alpha the engine never produces would be a
per-frame allocation for nothing. A scene that ever drew onto a transparent background would make
that visible here and nowhere else.

**`finish` is a method, not `Drop`, and the difference is the report.** `Drop` closes the pipe so the
encoder flushes, but only `finish` can wait and return a non-zero exit as an error — a truncated file
must not look like a successful export. The app resolves `ffmpeg` before the first frame (the same
`speech::find` PATH search), so "not installed" is one line and no render was spent on it.

**One loop feeds both sinks** (`main::export_timeline`), so an mp4 and a frame sequence are the same
frames. It builds each frame with `build_frame(.., STEP)` — exactly what `settle` does — so a
600-frame export ends on the frame `--png` captures, and the test asserts `max == 0` rather than a
golden tolerance. Every frame is a **full** render: damage rectangles exist to avoid recomputing what
is already on screen, and there is no screen. Three properties are decisions:

- **An export is `Motion::Full` unless `--motion` says otherwise.** Reduced motion is a *player's*
  preference; a video rendered on a machine with it enabled would be a different film from the same
  command elsewhere — the same reason the golden suite passes `--motion full` explicitly.
- **The default is `STILL_SECONDS` worth of frames (600) at `1/STEP` = 60 fps**, so `--video` and
  `--png` describe the same span. `--frames 0` is refused rather than writing an empty file.
- **`--srt` writes the narration track timed by `SceneContext::clock()`** — virtual time, because
  wall time in a headless run is how long the machine took to draw (`take_narration_timed` keeps each
  line's due time for exactly this). A new line **ends the cue it replaces** (that is when the band
  changes text), and the last cue stops at the end of the file rather than running past it. With
  `--srt` alone the opening summary is included, so a scene with no narration still yields a track;
  with `--captions` the same lines are also drawn into the frames.

`--video-png` **reports stale frames rather than deleting them**: exporting a shorter timeline into a
directory a longer one filled would otherwise read as a longer sequence, and the path came from the
caller. `--video`, `--video-png`, `--png`, `--inspect`, `--a11y-probe` and `--speak` are mutually
exclusive pair by pair with a message naming the pair; `--srt` without a video mode is refused.

**`rustim-compare`** is the visual-regression CLI over `compare`: `rustim-compare [--bbox-any] A B`,
exit 0 pass / 1 fail / 2 cannot compare (missing file, size mismatch, wrong argument count), printing
`ImageDiff::report`. `--bbox-any` relaxes **only** the identical-content-box rule; the per-pixel
thresholds are the golden suite's and are not relaxed by any flag. The golden suite calls
`compare_png_files` directly, so the CLI cannot drift into a different notion of "the same image".

### Text

Three crates, and the boundaries are the point:

| Crate | What it owns |
|---|---|
| `rustim-text` | `FontProvider` (a `parley`/`fontique` collection fed from a directory or a buffer, **never** from system font paths), `shape()`, and the `MissingGlyph` / `FontSubstitution` reports. Knows nothing about drawing |
| `rustim-scene::text` | `TextBlock` → `DrawOp::GlyphRun` (`text_ops`, `push_text`). The one place a shaper's output meets a rasteriser's input, and it lives in the kernel because a **wire-created** text mobject is drawn by `SceneContext::build_draw_list` — the scene that pushed its own text used to be the only writer |
| `rustim-app::text` | The shaper side: `ShapedText` → `TextBlock`, then delegates to `rustim_scene::text`. `rustim-text` knows nothing about drawing |
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
The consequence used to be `bounds: null` in `--inspect` and a `camera focus` that refused a text
node; the declared box described under "Accessibility" fixed both, and the shaper's own
`ShapedText.width/height` is where it comes from.

`MANIM_FONT_SIZE_UNITS` (`rustim-text`) is **2/3**, i.e. Manim's `font_size` behaves as points with
72 to the world unit. Measured, not derived — see the constant's doc and
`tests/manim_text_scale.rs`; multiplying Manim's `TEXT2SVG_ADJUSTMENT_FACTOR` by
`TEXT_MOB_SCALE_FACTOR` gives `0.5` and is wrong by a third.

### Wire-level text, formulas and groups: the `Typesetter` seam

`add` accepts three kinds that are not built by `shapes::build`: `text`, `formula` and `group`. The
first two need a shaper, and **the kernel has none** — `rustim-scene` must keep compiling to wasm
without parley or Typst — so the host installs one through a trait:

```rust
pub trait Typesetter {
    fn supports_text(&self) -> bool { false }
    fn supports_formula(&self) -> bool { false }
    fn text(&mut self, text: &str, spec: &TextSpec) -> Result<TextBlock, TypesetError>;
    fn formula(&mut self, source: &str, tree: &mut SceneTree) -> Result<TypesetFormula, TypesetError>;
}
```

`SceneContext::set_typesetter` stores a `Box<dyn Typesetter>`; a context built without one (every
kernel test, the export path before the app installs one, the plain browser page) simply has no text
or formula vocabulary. Four properties are load-bearing:

- **`handle` intercepts the two kinds; `command::apply` refuses them.** A pure tree-level function
  cannot typeset, so `apply` answers `Unsupported { feature }` rather than inserting a node with no
  picture — a scene that was told it added text must not then draw nothing. The interception lives
  beside `play`/`query`/`camera` for the same reason: those need state (the timeline, the viewport)
  that `apply` does not have. `group` is *not* intercepted: adopting children is pure tree surgery.
- **`hello`'s capability list is filtered by the installed typesetter**
  (`SceneContext::capabilities` → `Capabilities::with_typesetting`), so a build never advertises a
  kind its first `add` would refuse. `Capabilities::current()` stays the full vocabulary — it is what
  the conformance suite reads as the protocol's word list, and it is not a claim about any binary.
- **The error mapping is the seam's, once.** `TypesetError::Invalid { detail }` is a source the caller
  can rewrite and becomes `OutOfRange { property: "text", detail }`; `TypesetError::Failed { detail }`
  is a backend that cannot run (a missing `assets/fonts/`, an engine with no output) and becomes
  `CommandError::Typeset { detail }`. Retrying a sentence does not fix the second, which is why the two
  variants exist.
- **The desktop's provider is lazy and caches its failures** (`rustim_app::typeset::AppTypesetter`):
  `FontProvider::with_bundled` parses 19 MB and `FormulaCompiler::new` builds Typst's library, so a
  `--png` of a scene with no text pays neither. `supports_text`/`supports_formula` answer `true`
  unconditionally — the build was compiled with the shaper; whether the font directory is reachable
  on this machine is a runtime fact and belongs in `Failed`, not in a handshake that would then
  describe the deployment's disk.

**A wire text node stores a `TextBlock`, and the draw walk draws it.** `Mobject::text` is a new field
(`rustim_mobject::text::TextBlock`) holding the shaper's runs; `build_draw_list` emits
`rustim_scene::text::text_ops(block, parent_transform * m.transform, ctx.affine(), paint)` and then
reinstates `SetTransform(affine)` — the sharp edge `text_ops` documents: it returns ops and leaves the
transform belonging to the block. Paint alpha is `m.opacity * style.fill_opacity`, the same composition
every filled path uses. `local_bounds` is set from the block's `width`/`height`, which is what gives
the node measured bounds, a centre for `MoveTo` and a `camera focus` subject.

**A wire formula is a subtree, styled at insertion.** `Typesetter::formula` compiles and inserts into
the live tree, returning the root and the created nodes (`glyphs` and `rules`); the context then
writes the command's `color` (`fill_opacity = 1`, `stroke_opacity = 0`) onto **every** created node,
because a formula's glyphs are ordinary mobjects and their default style is off, and applies `label` /
`position` / `scale` to the root in `build`'s order (`translate * scale`). The answer's
`created` is the root id alone — a host asked for one object and gets one handle; the glyphs stay
addressable through `query`. `spec.text` is Typst math source without `$`.

**`group` is the one container the wire can build.** `children: Option<Vec<Target>>` is meaningful
only for `"type":"group"`: every entry is resolved before anything is inserted (so a bad id leaves no
half-built group), then reparented under the new node in the given order — draw order, so a later child
paints over an earlier one. `diff.created` names the group, `diff.modified` the adopted children;
`children` on any other kind is `OutOfRange { property: "children" }`, and an empty list is legal. A
group has no geometry of its own, so `set position`, `MoveTo` and `FadeOut` act on the whole set while
the members keep their ids and labels — which is what makes "name a compound figure, then move it" a
two-command idea instead of a reparenting dance.

The browser host's answer is `rustim_web::typeset::WebTypesetter`, compiled only under the `formula`
feature: `FormulaCompiler::from_embedded` (typst-assets, no filesystem) and `supports_text() -> false`
— the embedded font set has no CJK face, and promising text would make `hello` describe a deployment
rather than the artifact. The plain page installs nothing and advertises neither kind. Both halves are
pinned by `crates/rustim-web/tests/session.rs` under `cfg(feature)`.

### Web (WASM)

`rustim-web` is a **host**, like `rustim-app` — not a port of it. It runs the same kernel, the same
`SceneContext` and, through `rustim-scenes`, literally the same `Pythagorean` scene the desktop
binary renders, which is what lets the browser's canvas be compared against the golden baseline
rather than against a screenshot. `rustim-app` is not involved: winit, softbuffer and
`accesskit_unix` have no wasm32 build, and `accesskit` has no web adapter, so the Web path is the one
§10.6 named — `vello_cpu` → canvas pixels, `SemanticTree` → DOM/ARIA, the existing serde commands.

Three pieces, with the same "pure core, thin shell" split as the desktop:

| Piece | What it is |
|---|---|
| `session.rs` | `Session`: scene + `SceneContext` + `CpuRenderer` + `DrawList`, **no wasm types**, so `cargo test -p rustim-web` runs the real frame path natively. `set_camera` writes a gesture's view through `Command::Camera` |
| `aria.rs` | `SemanticTree` → `Vec<AriaNode>`: a pure projection to roles, names, descriptions, bounds and MathML. No `web-sys`, no `accesskit` |
| `pointer.rs` | DOM `WheelEvent`/client coordinates → `rustim_math::gesture`: the sign flip, the delta unit, and the canvas-pixel conversion. Pure, natively tested |
| `web.rs` | `#[cfg(target_arch = "wasm32")]` `WebApp`: the canvas, the rAF loop, the six pointer/wheel listeners, the DOM/ARIA overlay, the command bridge |

**Presentation.** `Renderer::pixels()` is **premultiplied** RGBA; the browser wants straight. The
copy is one `straight_rgba_in_place` into a `Vec<u8>` that `Session` owns and every frame reuses, then
a `Uint8ClampedArray` **shared** with a single `ImageData` (`new_with_js_u8_clamped_array_and_sh` —
no per-frame copy), then `putImageData`. Measured in Chrome 151 on linux-aarch64 against
`rustim-app --png` at the same virtual time: **identical content bounding box**
(`343, 90`)..(`936, 629`), 16 pixels differ by one LSB (0.002%), max channel error **1**. That is not colour management —
`putImageData` is byte-exact; `toDataURL` has to un-premultiply, and the alpha-254 edge pixels where
the translucent square meets the background round one LSB differently there. The §5.2 tolerance is
met with room, and `max == 0` is not a property the canvas API offers — `tools/web-check.mjs` records
the number rather than asserting a fairytale.

**The rAF loop holds no global state.** `WebApp` owns the `Closure<dyn FnMut(f64)>`; the callback
captures `Weak<RefCell<Inner>>` plus a `Weak` of its own slot, and `Drop` cancels the pending frame
by its stored handle and takes the closure. Nothing is `forget()`ed, `Box::leak`ed or put in a
`thread_local!`. `dt` comes from `performance.now()` clamped to `0.0..=0.1`, so returning to a
backgrounded tab does not fast-forward ten seconds. `?paused=1` draws virtual time zero and waits for
`advance(seconds)` — that is how the acceptance check gets a deterministic frame.

**Pointer input is the desktop's gesture code, reached through the DOM.** The wasm shell owns no
gesture arithmetic: `rustim-web::pointer` only translates. `scroll_delta` flips `WheelEvent.deltaY`
(a DOM positive is scrolling down; the gesture's positive is zoom-in) and maps `deltaMode` onto
`ScrollDelta` — pixel and line directly, a page as `PAGE_LINES = 10` — and `canvas_point` scales a
client position by `canvas.width / rect.width`, which is the one conversion right for both a
CSS-shrunk canvas (`max-width: 100%`) and a HiDPI one. `web.rs` installs six listeners on the canvas
— `pointerdown` (left button, primary pointer), `pointermove`, `pointerup`, `pointercancel`,
`pointerleave`, `wheel` — keeps each in a `Listener` whose `Drop` removes it, and feeds the shared
`rustim_math::gesture::Pointer` exactly as `main.rs` does. The press handler records the contact
point *before* arming the drag: a mouse has already sent a hover move, a touch has not, and without
that the first finger movement would only establish the anchor. Every resulting camera goes through
`Session::set_camera`, i.e. `Command::Camera` through `handle` — the same validator a script meets.
`preventDefault` on the wheel keeps the page from scrolling under the zoom, and `touch-action: none`
does the same for a drag.

Measured by `tools/web-check.mjs` in Chrome 151 through `Input.dispatchMouseEvent` — the browser's
own input pipeline, so the listeners see real `PointerEvent`s rather than a synthetic dispatch — at a
1:1 canvas: a 200 px drag moves `center.x` to `-2.2222222222222223` (= −200/90, exact, with a
vertical component of zero), and `deltaY: -100` sets `zoom` to `1.2214027581601699 = e^0.2` while
the world point under the cursor moves `6.3e-16`. The values are asserted, not printed hopefully.

**The rasteriser is single-threaded here.** `rustim-render` gained
`default = ["multithreading"]` / `multithreading = ["vello_cpu/multithreading"]`, and `rustim-web`
takes `default-features = false`. That is §10.6's guard 2 as a cargo feature instead of a promise;
the desktop default is unchanged, and `ctx.flush()` is harmless on the single-threaded path.

**Accessibility is a projection, not an adapter.** There is no `accesskit_web`, so `aria::project`
flattens the semantic tree the same way the AccessKit adapter consumes it and the shell writes one
absolutely-positioned `<div>` per node: `role`, `aria-label`, `aria-description`, `aria-hidden` for
hidden nodes, and a `role="status" aria-live="polite"` region for `tree.live`. A formula keeps its
`<math>` — `innerHTML` of the MathML Typst emitted, which is the *engine's* output and the trust
boundary, never user input — because ARIA has no math role while the browser gives MathML elements
one; this is §10.6's "the Web does not have to flatten a formula". The overlay is `opacity: 0` rather
than `display: none`/`visibility: hidden`, both of which prune the node; Chrome 151 was measured to
keep `opacity: 0` in its AX tree (`Accessibility.getFullAXTree` finds all four figures), so the
`.sr-only` fallback was not needed. The DOM is rebuilt only when `SemanticTree` changes (`PartialEq`
on the whole tree).

**Two artifacts, because Typst costs an order of magnitude.** `tools/build-web.sh` emits `web/pkg`
(the Pythagorean page) and `web/pkg-formula` (the same shell with `--features formula`, which is the
only place a Typst crate reaches wasm32 on purpose). Measured 2026-10-17, `rustim_web_bg.wasm`:

| Artifact | raw | gzip -9 | JS glue |
|---|---|---|---|
| `web/pkg` | 1 807 846 B | 639 863 B | 24 485 B |
| `web/pkg-formula` | 33 996 198 B | **14 361 238 B** | 24 487 B |

The second number is §10.6's "Typst's WASM size needs measuring", measured. It is dominated by
Typst's compiler, ICU data and the nine megabytes of `typst-assets` fonts, and the answer to it is
packaging, not denial: the pages load **different modules**, so a reader of the Pythagorean scene
never downloads a typesetting engine. Two profiles, because the obvious choice was wrong twice:
`opt-level = "z"` + fat LTO made the plain artifact *larger* and the formula one smaller, so
`[profile.web]` keeps release's optimisations and only strips (`debug = 0`,
`strip = "symbols"`), and `[profile.web-formula]` adds the size-shaped pair. The formula page also
uses `FormulaCompiler::from_embedded` — `typst-assets` only, no filesystem — because a browser has
no `BUNDLED_DIR`; the embedded half is shared with `new()` so a fragment compiles on both hosts or
neither.

Deferred, deliberately: **keyboard input and focus commands** (the Web version promises "a screen
reader can read the structure", not interaction; pointer pan/zoom has landed — see above); pinch
gestures and inertial scrolling; font subsetting and lazy loading (the two-page split is the
packaging answer for now, and a CJK formula on the web needs fonts `from_embedded` does not carry);
and WebCodecs/`MediaRecorder` export (the `VideoSink` seam is the place it goes). The query command
is *not* deferred any more — `Session::apply` answers it through the same `reply_line` the desktop
feed uses.

### Naming

`Scene`→`Scene`, `Mobject`→`Mobject`, `Animation`→`Animation`, `construct()`/`play()`→same,
`MathTex`→`Formula`, `Create`→`Create`, `Shift`→`MoveTo`, `NumberLine`→`NumberLine`,
`Axes`→`Axes`. `Plot` is not a Manim class: it is the wire spelling of an `Axes.plot` curve whose
sampled points the host supplies (see "Axes, number lines and plots").

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
name a node that is not there. **`from_raw` is not a round trip for an arbitrary integer**: `from_ffi`
normalises the generation to `… | 1`, so a host that sends `999` is answered with `4294968295` — the
same id, spelled the way the engine holds it. That is why a refusal's `Display` prints `raw()` (the
reply's number) rather than the `Debug` of the id, which is `NodeId(999v1)` and matches nothing anyone
typed.

**`kurbo` 0.13 has no `From<Rect> for BezPath`.** Use `rustim_math::rect_path` (or `fill_rect`, which
both backends expose) rather than reaching for a conversion that does not exist. `Point` also has no
scalar multiplication or point addition — go through `Vec2` (`p.to_vec2()`, `Vec2::hypot`).

**A filled open polyline is not a curve — the fill rule closes it.** Handing a sampled function's
points to `Fill::NonZero` paints the region between the curve and its closing chord, so a graph
renders as an area. `Plot`'s points are therefore the samples themselves, with the fill switched off
and the stroke switched on, and the picture comes from `DrawOp::StrokePath`
(`a_plot_is_stroked_and_not_filled` counts zero fills and one stroke at the width the style asked
for). The first implementation expanded the polyline into a closed ribbon with `kurbo::stroke` in the
shape builder; it worked, and it was moved out deliberately — the stroker belongs to the backend that
already has one, and a ribbon's interior contour is a second representation of a line to keep right.
Do not put it back. The same trap is why a formula's fraction bar is expanded to a quad rather than
drawn as a polyline (see "P1 step 5").

**Welding rectangles into one path requires that every rectangle share its winding and close
explicitly.** `points_from_path` walks each `MoveTo` out to a hub with a doubled slit, so the
subpaths' orientations decide what `Fill::NonZero` subtracts. Every axis shaft and tick is built
with `rustim_math::rect_path` (CCW, `ClosePath`), never with hand-picked corners, and the test is a
winding count at four points (inside the shaft, inside a tick, in the gap above a tick, outside).
A tick that protrudes past the axis's end also breaks the a11y diameter measurement, which is why
the extents are clamped to the shaft — see "Axes, number lines and plots".

**`ShapeParams` is now a quarter-kilobyte, and that is enough to trip `large_enum_variant`.**
Adding `range`/`x_range`/`y_range`/`step`/`tick_size`/`points` pushed `Command` past clippy's 256-byte
threshold, so `rustim_app::a11y::Request::Command` carries `Box<Command>` — the request enum is
cloned/moved once per action, not per frame, so the box costs nothing measurable and keeps the
warning from being silenced workspace-wide.

**`serde_json` without its `float_roundtrip` feature does not round-trip every float it writes.**
Its parser can land one ULP away from the number serde_json itself serialised, so
`to_string(from_str(s)) == s` — the byte-for-byte property the command/reply protocol rests on —
fails for long mantissas. Nothing noticed until `SemanticTree.camera.visible` (an aspect-ratio
product like `14.222222222222221`, not a number anyone typed) made the whole-tree comparison in
`the_live_text_rides_along_with_the_snapshot` fail. The feature is enabled in both the workspace
manifest and `rustim-scene`'s own `serde_json` line; turning it off restores a class of one-ULP
replay differences rather than a visible bug, which is exactly why it needs to be written down.

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

**A per-point route must be taken between parent-space points, not local ones.** Manim's `points`
are world coordinates, so `path_along_arc`, `pointwise_become_partial` and every other per-point
Manim routine assumes the array it is handed is the object actually travels in. Here points are
local and a mobject's displacement can live entirely in its deferred `transform` — so a
`path_arc` evaluated on the local arrays is **exactly a straight line for the commonest request**,
"move this there along an arc", and the object still arrives where the caller asked, which is what
makes it look like a feature that works. `path_arc` folds both sides through their own transforms
before arcing and writes the result back with an identity transform (see "Morph routes"). The same
question has to be asked of any future per-point operation: `align_points` is safe because it only
pairs corresponding *local* points, but anything that measures a route is not.

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

**`winit` 0.30.13 has a pre-existing X11 race that can abort a windowed run.** Approximately two in
ten `--frames` runs ended with "panic in a destructor during cleanup"; the backtrace is
`get_frame_extents_heuristic` → `.expect("Failed to translate window coordinates")`, with
`X11Error { error_kind: Window, request_name: Some("TranslateCoordinates") }`, reached from
`configure_notify` while events were being pumped. Five sequential runs then exited 0, and no
non-windowed path (`--png`, `--inspect`, the tests) is affected. It is not caused by the damage
rectangles — it reproduces the same way with a full render — so do not chase it when a windowed run
aborts this way.

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
optimisation to remember. The corollary is that nothing may depend on a call *having* happened, and
that the push counters read zero unattached: `--a11y-probe` reports that as "not attached", which is
the only signal `accesskit_winit` gives (there is no `is_active`).

**Narration must not go through the announcer, and that is a content decision.** `Announcer` exists to
keep a burst of *events* from overwhelming a reader: it holds one slot and drops anything offered
within 750 ms of the last flush. A scripted lecture is not a burst of events — each line is content
the listener is owed — and a pacing of "one sentence every half second" would silently lose every
other line. `A11yCore::narrate` therefore writes `live` directly, and the pipeline says the whole
drained list while the live region (which can hold one string) keeps the last line. The corollary is
that narration and the announcer share one slot: an announcement that flushes afterwards replaces the
narration in the live region, which is why the host records `last_live` when it narrates — otherwise
the next frame would treat its own line as a fresh change and speak it twice.

**A speech child that reads to end of input deadlocks if the parent waits with the pipe open.**
`espeak-ng --stdin`, `say` and PowerShell's `[Console]::In.ReadToEnd()` all speak until EOF, so the
write end must be dropped **before** `wait`. In Rust that is the borrow rule doing the work: the
`ChildStdin` is a local, and the `if let Some(mut stdin) = child.stdin.take()` block's end is the
close. A broken pipe is ignored — the synthesiser exiting without reading is its answer, not a reason
to take the speaker thread down. Text is always written to stdin and never placed in argv, so a
sentence with quotes, newlines or CJK cannot be mangled by per-platform quoting and never appears in a
process listing.

**An incremental AccessKit update is a set of relationship promises, not merely a smaller
snapshot.** `tree: None` is legal only if the consumer can still reach every node it needs to
change: a parent must arrive in the same update as an added or removed child, a removed node's id
must not appear anywhere in the update, a reparent requires both parents, and `focus` must be
present every time and must name a node that exists. The whole-tree assertion the full update
satisfies ("every named node's parent is named or is the root") is *false* for the incremental case
by design, so the tests replay each update through `accesskit_consumer` — the same code the platform
backends run — rather than asserting over our own `TreeUpdate`.

**A focused node can be removed by an animation, and a dangling focus makes the consumer panic.**
`FadeOut` deletes its target in `finish`; `FocusState` resyncs only on a key or AT event, so between
the removal and the next keypress the core still holds the id. `A11yCore::to_update` releases a stale
focus with one `tree.get` before projecting, which is the O(1) fix — a per-frame `FocusState::sync`
would walk the whole tree and undo what the incremental update bought.

**`accesskit_consumer` is already in the Linux graph**, pulled in by `accesskit_unix` (the AT-SPI
backend), so `rustim-app`'s `[dev-dependencies]` line adds no crate and no wasm risk; it only puts
the same implementation in scope for tests. Check with
`cargo tree -p rustim-app -i accesskit_consumer` before repeating the "it never reaches a binary"
claim — that claim was wrong when this was written.

**`Mobject::critical_point` answers in the parent's space, not the world's.** It applies
`self.transform` and nothing else, unlike `SceneTree::world_bounds`, which multiplies in
`parent_transform`. The declared `local_bounds` fallback must therefore use `self.transform`; folding
the parent in as well applies the ancestor transform twice, and on a nested text node the object
lands somewhere no one asked for. The doc comment used to say "world coordinates", which is what made
the mistake attractive.

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

**A blanket impl that leaves out one method silently disables that method for every caller that goes
through the box.** `impl<T: Animation + ?Sized> Animation for Box<T>` has to forward `finish`; without
the forwarding line the default no-op is what `Box<dyn Animation>::finish` resolves to, and since the
scheduler and `Sequence` *both* hold `Box<dyn Animation>` — the two routes a scene actually plays
through — the removal never happens, while a direct call on a concrete `FadeOut` still works. The
symptom is a `FadeOut` that fades out and stays, which no image can see: a fully transparent mobject
draws nothing either way, so only `--inspect` shows the leftover node. The test therefore drives a
`FadeOut` through `AnimationScheduler::add`, never through a hand-called `finish`.

**A group begins every member before its first frame, so a member's `begin` sees the tree as it was
*before the group* — and a succession's does not.** This is not an optimisation detail: `MoveTo` and
`Rotate` capture their start in `begin`, so the two kinds of composition produce different motions
from the same members. In "scale, then rotate" the rotate begins after the scale and composes on top
of it; put the same two in a group and the rotate captures the *unscaled* transform, then both
members write their own capture each frame and the later writer wins — which is exactly Manim's
semantics, and why a group is for parallel effects on *different* objects rather than competing ones
on the same object. The recording-`Probe` tests assert it — `Begin("b")` is already in the log by the
time the group has delivered its first `At("a")`, and a succession at the same moment has not begun
`b` at all — because it cannot be seen in a still.

**A composed animation's `finish` must drive each child to `ease(1.0)` and `interpolate` *before*
calling the child's `finish`.** `Animation.finish` (`animation/animation.py:216-227`) opens with
`interpolate(1)` for the same reason: `finish` is a hook that acts on the object's *end* state, and a
`FadeOut`'s hook deletes the object. Call `child.finish` on a half-interpolated member and the
removal happens from an opacity it never reached — invisible in a still (the object is gone either
way), visible as a wrong end state for any member whose `finish` reads its geometry. The same applies
to `Succession`, which must finish a member at the moment it leaves it behind, not only at the very
end.

**A target affine is not a position, and computing it eagerly is wrong twice over.** `MoveTo` used
to be handed `translate(to)` by the wire layer, which is correct only while the object's transform is
the identity: on a scaled or rotated object it silently undid that, and the existing move test could
not see it because its object was untransformed. The second half is timing — the
translation that sends the *current centre* to a point has to be computed in `begin`, because a
`Succession` constructs all its members before the first runs; an eager `translate(to)` also throws
away the scale an earlier member applied. The rule that survives both: **a position is a point plus
the state the object has when the animation starts**, and only `begin` knows the second half.

**`serde` writes struct fields in declaration order, so a hand-written round-trip fixture must match
it.** `AnimationSpec` declares `target, type, duration, to, angle, factor, about_point, into,
animations, lag_ratio`; writing the nested-succession case as `{"type":"succession","animations":[...],
"duration":2.0}` parses fine and then serialises back with `duration` first, so the byte-for-byte
round-trip test fails on a command the engine understood. The fixture is not wrong about JSON — JSON
objects are unordered — it is wrong about the protocol's canonical spelling, which is the thing the
test exists to pin.

**A faded padding copy has to be a real node in the tree, because opacity does not inherit.** Manim's
`add_n_more_submobjects` pads the shorter side with `.copy().fade(1)` — copies of a group's own
children, transparent, morphing into the extra targets. Here the draw walk folds each node's own
`opacity` into its paint and does *not* multiply a parent's into its children, so "a transparent
parent" draws nothing only if it has no geometry; the padding has to fade **recursively**
(`Subtree::fade`), and the copies have to be inserted as nodes with fresh `NodeId`s. The alternative
— a temporary `Mobject` value aligned against the target and thrown away — was rejected because the
source must keep its child count and its later alignment must see those children, which is exactly
what Manim's mutation of `self.submobjects` buys.

**`begin` is read-only, so an in-place alignment has to be split into plan and materialise.** Manim's
`align_data` mutates the source inside `begin`; this trait hands `begin` a `&SceneTree` on purpose, so
`TransformGroup::begin` computes an immutable `NodePlan` and the first `interpolate` inserts the
padding copies (it is the first call with `&mut SceneTree`). Two consequences to keep in mind when
touching it: nothing may assume the child count in `begin` equals the one on the next frame, and the
plan must be built from `SceneTree::snapshot` rather than from live nodes, because the donor has to
be read without holding a borrow while the source is written.

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

**A formula's rules are strokes, and a formula's output is geometry, not style.** A fraction's bar
and a radical's overline are `FrameItem::Shape` carrying `Geometry::Line(..)`
(`typst-layout/src/math/fraction.rs:74`, `radical.rs:137`) — `Shape` stores geometry *plus* an
optional fill and an optional stroke, and these have no fill. `Formula::insert` turns them into
mobjects whose geometry lives in a point array, and a point array cannot say "draw this contour with
a pen and that one with a brush"; so a stroke-only shape is dropped and the formula reads as a bug.
`formula::filled_shape` expands one into a filled quad of the stroke's own thickness (a rect for
`Geometry::Rect`, `None` reported into `skipped_shapes` for anything needing a real stroker). The
draw walk *can* stroke now (see "Strokes"), but that does not reach this case: the distinction the
rule needs is per-contour within one compile step, and the compiler's only output channel is
geometry. `FrameItem::Shape` is the one thing this step needed that the tutorial's sketch does not
mention at all.

**`Glyph::span` is the element's span, not the glyph's.** Every glyph produced by one math element
shares that element's `Span`, so it identifies a symbol only when one element produced one glyph.
`"abc"` in math gives all three glyphs the whole literal's range, and labelling them from it makes
three children with the same name. The usable rule is a comparison: use the source slice only when
its **character count equals the shaped character count**, and fall back to the shaped text
otherwise. (Related: math typesets a Latin letter as the *mathematical italic* codepoint — `x`
becomes U+1D465 — so a label taken from the shaped glyph is not the character the author typed,
which is what makes the source slice worth preferring at all.)

**A partial range that lands exactly on a curve boundary keeps *eight* points, not four.**
`integer_interpolate(0, n, 1/n)` returns `(1, 0.0)`: the index is the curve you are *on*, and its residue
is inclusive at the start, so the boundary belongs to the upper curve. `partial_path_points` over
exactly one third of a three-curve path is therefore the whole first curve plus a second curve
collapsed onto its start — the point before and the point after the boundary are the same coordinate,
and the array is 8 long. (It only reads wrong if you expect "a third of the path" to mean "a third of
the points"; every Manim renderer draws the same 8-point spelling and cannot tell.) Two ends of the
same contract: `b <= a` is a degenerate four-point curve rather than an error — `Uncreate` reaches it
at its final frame, where the removal follows immediately — and a **ragged** array (not a multiple of
four) is returned unchanged, because there is no curve to split. The parity group is what pins all
three; a hand-written test of `(0.25, 0.75)` alone would not have told you the boundary rule.

**`Uncreate` reverses outside the rate function, and the difference is visible for asymmetric curves.**
Manim composes the reversal *inside* it — a per-submobject alpha of `rate_func(1 - raw)`
(`animation/animation.py:385-391`), with `Create._get_bounds` still returning `(0, alpha)` — because
its `Animation` owns the raw clock value; this engine's scheduler hands an animation an alpha that is
already eased, so the port can only write `1 - rate(raw)`. The two agree exactly for every curve that
is **point-symmetric** about `(0.5, 0.5)`, i.e. `f(1 - t) == 1 - f(t)` — `linear`, `ease_in_out` and
`smooth` are — and differ for `ease_in`, `ease_out` and `overshoot`, so under those the erase has
different timing from Manim's. It is documented on the type rather than hidden, and it is the
constraint the "~40 rate functions" item has to respect: a new rate function is not automatically
available to `Uncreate` with Manim's timing. Recovering `raw` is not an option — it would need the
inverse of a curve that is not injective (`overshoot`), and a numerical inverse would not be
bit-identical anyway.

## Where the workspace diverges from the tutorial

The tutorial puts everything in a flat `src/` (`main.rs`, `mobject.rs`, `animation.rs`, `scene.rs`,
`examples/pythagorean.rs`). The workspace instead splits into crates so the semantic kernel can be
tested and ported independently; `cargo run --example pythagorean` will not work, and the reference
scene lives in `rustim-app`.

Issues the tutorial's sketches leave open, and where each stands now:

- **Coordinate system** — *resolved.* `rustim_math::Viewport` (pixels) and `rustim_math::Camera`
  (where the pixels look) together map Y-up centred world units to Y-down pixel space. Scene code
  contains no pixel constants; the composed transform is applied once, at the head of the draw
  list, and `SceneContext::affine()` is the only way to get it. The tutorial's own `RightTriangle` sketch has the right angle at the
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
- **Easing** is `ease_in_out` (quadratic in/out, split at `t = 0.5`) by default, applied by
  `Animation::ease` from the animation's own `rate_func`; a beat may override it with a wire `rate`
  or a `WithRate` wrapper.
- **Opacity composes multiplicatively**: a mobject's `opacity` times its style's `fill_opacity` for
  the fill and times its `stroke_opacity` for the stroke. At draw time each is folded into that
  pass's paint alpha, so the two channels fade independently.
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
groups for layout but does not clip them. The macOS and Windows `formula` baselines are owed like
every other scene's — only `linux-aarch64.png` is checked in (see "Golden images").

The two commands that catch the failure modes above are
`cargo tree -p rustim-text -i skrifa@0.44.0` and `cargo tree -i ttf-parser` — **and the version is
named on purpose**: a bare `cargo tree -i skrifa` reports *two* versions, because the lock has a
second, older `skrifa` inside Typst's own image subtree (`hayro-interpret`), which is harmless but
reads like a mistake. Both, plus "no Typst crate reaches the kernel" and "`rustim-render` never
depends on `rustim-text`", run as the `typst-subtree` CI job.

### Formula structure: MathML

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
  Deferred: pinch gestures and inertial scrolling, and keyboard/focus commands (still the one
  interaction the Web version does not promise).
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
  later phases and the documents say so.
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

## AI integration

The product is an AI explainer assistant, so the engine exposes a JSON command interface
(`需求探讨.txt` is explicit that the AI should *drive* the engine, not write it):

```json
{ "action": "add", "type": "circle", "params": { "radius": 2.0, "label": "leg a" } }
{ "action": "set", "target": 4294967299, "property": { "name": "opacity", "value": 0.5 } }
{ "action": "play", "animation": { "target": 4294967299, "type": "fade_in", "duration": 0.6, "rate": "ease_in_out_sine" } }
{ "action": "narrate", "text": "两条直角边是 3 和 4", "at": 2.0 }
```

**The engine and the assistant are separate products, and the protocol is the seam.** The assistant
(Jarvis) is not in this repository and must not be: any model that can write JSON must be able to
drive rustim, and a driver must be able to switch between assistants without the engine changing.
The two normative documents are `驱动协议v1.md` (the wire) and `贾维斯集成方案.md` (the
architecture: transports, switching mechanisms, tool tiers, roadmap). `PROTOCOL_VERSION` (`1`) and
`Capabilities::current()` are the machine-readable half of that agreement — see "External drivers and
the AI surface" below.

**The handshake is `{"action":"hello","protocol":1}`**, and it is the recommended first line of every
session. It does not touch the tree; it answers with a `capabilities` document — the protocol version,
the engine's version string, and the exact shape / animation / property / rate names this build
accepts, plus its limits. The lists are **derived from serde over the same enums the parser uses**,
so an advertised name is accepted by construction, and a conformance test feeds every advertised word
back through the binary (see `crates/rustim-app/tests/conformance.rs`). `PROPERTY_NAMES`,
`AnimationKind::ALL`, `Rate::ALL` and `ShapeKind::ALL` are what supply the lists, and a new variant
that misses its `ALL`/name table fails a count assertion rather than silently not being advertised.
A foreign version is refused in `SceneContext::handle` (not at parse time, so both numbers can be
named) as `OutOfRange { property: "protocol", detail: "client speaks 2; this engine speaks 1" }`;
the refusal round-trips as JSON like any other command. A driver that skips the handshake still
works — the protocol is additive — but it gives up the one place a version mismatch can be reported
before a command means something else.

`target` is a **`Target`**: either a `NodeId` — an **opaque 64-bit integer**, not a small index (see
the `slotmap` note above — the low half is the slot, the high half the generation) — or the
human-readable `label` the object was given. Because the id is generational, a command naming a
deleted node fails cleanly instead of hitting a recycled object.

The label spelling is the one a model reaches for: it sent `"label": "圆"` when it created the object
and has no reason to remember `4294967297`. It is the **same field**, not a second one — an integer
and a string are unambiguous in JSON, and each serialises back exactly as it arrived, so a
label-addressed command replays byte for byte and a logged session stays readable a week later. Every
place a command names a node takes the two spellings: `set`/`remove`'s `target`, `play`'s
`animation.target`, a morph's `into`, `camera.focus` and `query.target`. Resolution happens once, in
the command layer, before anything is touched (`apply`, `animation_for`, `camera_for`, and the query
arm of `handle` all call `Target::resolve`); the animations themselves only ever see an id, because
`rustim-anim` has no tree to look a label up in. A label is **not a key** — the engine has never
refused a duplicate and a scene with two "圆"s is legitimate — so an unknown label is `NoSuchLabel`
(a different bug, and a different fix, from a stale id's `NoSuchNode`) and a duplicated one is
`AmbiguousLabel`, which **lists every candidate in draw order** rather than picking one. Guessing
which of two "圆"s was meant would be invisible in the reply; being handed the ids is not.

Every command returns a `StateDiff` (`created` / `modified` / `removed`), so a host learns the id of
what it just added and a renderer learns exactly what to invalidate. A diff always names **ids**,
even when the command addressed the object by label — the label is an input spelling, and the
unambiguous handle is what a host stores for the commands that come after.

`play` is the one command that **appends rather than mutates**: it queues an animation as the next
beat of the same timeline a scene's own `ctx.play` writes into, which is what makes the picture a pure
function of the command log — a host logs, replays and diffs a session, and gets the same frames. It
therefore returns an **empty** `StateDiff`, and honestly so: a diff describes the tree, and queueing a
beat does not change the tree, it changes what will happen to it. An interruption is a *different*
command, spelled out in the data — `stop`, below — because `play` cannot preempt what is already
playing.

**`stop`** (`{"action":"stop"}`) is the one command that **interrupts**: it drops every queued beat
and every running animation and answers with a present-but-empty `StateDiff`, for `play`'s reason.
**No animation is `finish`ed**, and that is the whole semantics — a `FadeOut` in flight leaves its
target in the tree at the opacity it reached, a morph leaves its subject part-way, and the next
`play` starts from there. Jumping each animation to its end state would be `finish`, which is the
opposite of what "stop" means to a viewer. The lecture clock and the narration track are untouched
(neither is an animation), `is_playing` becomes false so a scene reacting to its timeline draining
sees an ending rather than a stall, and the command is idempotent: stopping a stopped scene succeeds
and changes nothing.

`stop`, `camera`, `motion` and `narrate` are the commands intercepted by `SceneContext::handle`
rather than by the pure `command::apply`, and all four return an **empty** `StateDiff` for `play`'s
reason — the timeline, the view, the motion policy and a spoken line are not nodes. `camera` writes the view now or queues a
`CameraMove`; `motion` (`{"action":"motion","mode":"full"|"reduced"}`) writes the policy the **next
beat started** will be given, without touching the beat already running; `narrate` queues a line on
the narration track, due `at` seconds after it arrived (see "Narration, captions and
self-voicing").

**`query` is the read half of the same surface** — `{"action":"query"}` or
`{"action":"query","target":<id|label>}` — and it is answered with a *document*, not a diff:
`{"seq":4,"tree":{…}}`, where the tree is exactly the `SemanticTree` `--inspect` prints, projected
against the context's viewport and the scene's current camera. It lives in `handle` rather than
`apply` because a pure tree-level function has no viewport to compute pixel bounds with. A `target`
keeps that node and its descendants and sets the document's `root` to it; a missing node is
`NoSuchNode`, never an empty tree, because "there is nothing under it" and "it is not there" are
different answers and a host cannot tell them apart afterwards. The reply's `live` is always `null`:
the announcement text belongs to a host's accessibility layer, not to the kernel.

`add` builds more than polygons: `text`, `formula` and `group` are on the wire (see "Wire-level text,
formulas and groups: the `Typesetter` seam"), and their availability is in `hello`. A scripted
lecture is therefore `add` a sentence, `add` a formula, `play` the two beats, `narrate` the sentence
— with no compiled scene at all. `group` adopts existing nodes through `children`, which is how a
model composes a figure it can move and fade as one object without losing the parts' ids.

`CommandOutcome` is the in-memory vocabulary that keeps the three answers apart — `Applied(StateDiff)`,
`Queried(SemanticTree)` and `Greeted(Capabilities)` — and `reply_line` is the only thing that turns it
into JSON, so the desktop feed and the browser bridge cannot answer one command two different ways.
The hook fires only for `Applied`: a scene that re-fitted itself on every read would do work the
question did not ask for, and a greeting changes nothing at all.

Two wire-format invariants are load-bearing, both pinned by tests:
- **Commands round-trip byte-for-byte.** `to_string(parse(json)) == json`. A host logs, replays and
  diffs these, so anything that serialises back differently is a bug.
- **Absent shape parameters are omitted, never written as `null`.** These payloads are model output;
  echoing `"width": null` back would teach the model that nulls are part of the protocol.

### External drivers and the AI surface

The command layer is designed so that **the engine is one process and the assistant is another**.
That separation is what makes "switch AI assistants freely" a property rather than a promise, and it
is why the protocol is specified in `驱动协议v1.md` independently of any particular driver
(`贾维斯集成方案.md` is the architecture document; both are normative for this section).

Three properties make switching work, and each is already a property of the engine rather than a
feature to add:

- **All conversation state lives in the engine, not in the model.** The tree, the camera, the
  timeline, the motion policy and the narration track are `SceneContext`'s; a driver that is replaced
  mid-session loses nothing, because it never held anything. The obvious corollary is that a driver
  keeps no private model of the scene either — it reads one back with `query`.
- **A session is a command log, and the log is the state.** A script applies at virtual time zero,
  commands round-trip byte for byte, and the headless path is deterministic; the conformance suite
  pins this by running the same eleven-line log through **two independent processes** and comparing
  the reply vectors and the final `SemanticTree` field for field. That is the executable definition
  of "a second assistant takes over".
- **Capability negotiation is explicit.** `hello` names the protocol version and the vocabulary;
  a mismatch is refused with both numbers. An assistant adapted to one build can therefore be told
  by another build, before it sends anything, whether it will be understood.

The transports are deliberately not part of the kernel. `--script - --reply` (newline-delimited
JSON over stdio) is the one implemented today, and `rustim-client` is its client half: `read_reply`
enforces the one-presence rule, `send_raw` enforces `seq`, `hello` is one call, and `close` returns
the exit status. The conformance suite in `crates/rustim-app/tests/conformance.rs` is written against
that crate only — it spawns the real binary, never reaching into `rustim-app`'s modules — so a passing
test is evidence that an outside driver can do the same. It asserts the advertised vocabulary is
accepted by the binary (every shape, every animation kind with its required fields, every rate),
that ids and labels agree across `add`/`set`/`query`, that a foreign protocol is refused readably and
the feed continues, that blank lines are skipped and counted, and that two clients replay one log to
one tree. A planned WebSocket/JSON-RPC transport (`rustim-daemon`, for the Jarvis UI) and an MCP
tool surface (`rustim-mcp`) are **not implemented** and must map onto the same `SceneContext::handle`
— a second transport is a wrapper around this protocol, never a second dialect of it.
Checkpoint/undo/seek, the teaching-tier tools (`rustim-director`) and the two tool tiers are roadmap
items in `贾维斯集成方案.md`; nothing in this repository currently claims them.

### The feed: a session is a file of commands

`rustim-app --script <path|->` applies one JSON command per line (JSONL) and is *how a host drives the
engine at all* — without it the only scenes that exist are the eight compiled in. The transport is
`rustim_app::feed`; the protocol is `rustim_scene::command`, which owns `CommandReply` and stays free
of I/O and of a JSON parser (it compiles to wasm).

```jsonl
{"action":"add","type":"circle","params":{"radius":1.6,"color":[100,181,246],"label":"圆"}}
{"action":"play","animation":{"target":4294967297,"type":"move_to","to":[2,1],"duration":1.0}}
```

- **`--reply` answers every command with one JSON line on stdout**, and then stdout is *only* replies:
  every human-facing line the binary prints (`wrote out.png (1280x720)`, the frame-rate line) moves to
  stderr (`main::note`). A host cannot skip a line it did not expect.
  `{"seq":1,"diff":{"created":[4294967297],"modified":[],"removed":[]}}` — **exactly one of `diff`,
  `tree`, `capabilities` and `error` is present**, and the presence is the discriminant. `created` is
  the only way to learn the id of what was just created; a `play` gets a present-but-**empty** diff,
  because queueing a beat changes nothing in the tree; a `query` gets a `tree` and no diff at all,
  because an empty diff would read as a mutation that did nothing; a `hello` gets `capabilities` and
  neither of the other two, because a handshake is not a read of the tree.
- **`seq` is the 1-based *physical* line number**, blank lines included, so a human reading a log and a
  host reading the stream count the same way. A blank line is skipped silently and produces no reply.
- **Every reply is flushed before the next line is read.** A file source is read whole; `-` is stdin,
  read line by line and answered line by line, so a host can send `add`, read the id back, and animate
  it. Windowed, stdin is read on its own thread and arrives as `AppEvent::Command`.
- **`CommandError::Unparseable { detail }`** joins the vocabulary for a line the transport could not
  turn into a command. Only the application constructs it (`apply` never sees text), but a host has to
  tell "the engine declined what I asked" from "the engine could not read what I sent", and one error
  type on the wire is simpler than two.
- **A refusal does not stop the feed**, and neither does it fail the run: refusals are counted and
  summarised on stderr (`feed: 1 of 3 commands refused`); exit code stays 0. Only a script path that
  cannot be read, `--reply` with no `--script`, and `--reply` with `--inspect` are `exit(2)`.
- **A script is applied after `construct` and at virtual time zero**, so the scene's own objects keep
  the ids they always had and the same script replays to the same frame (asserted `max == 0`, not the
  golden tolerance). The feed is orthogonal to the scene: `--scene rotate --script /dev/null` is
  byte-identical to `--scene rotate`.
- **`--script` without `--scene` runs on a `BlankScene`** — an empty `construct`, deliberately **not**
  in the `--scene` registry, so `--help` does not list it and the golden suite does not owe it a
  baseline on three platforms.
- **A `narrate` line is paced by `at`, not by where it sits in the file.** Every line of a file feed
  is applied at t = 0 — that is the determinism property, not a quirk — so two `narrate` lines
  without `at` are both due in the frame the file is read. Its reply is a present-but-empty diff,
  like `play`'s.
- **`--inspect` carries a formula's structure.** A `role: "math"` node holds its MathML in a
  `mathml` field — absent, not null, on every other node — so a host reads the expression as a tree
  rather than reconstructing it from the glyph children's names. It is one more thing a host can read
  and nothing a host can write: there is no command that sets structure on a formula (see "Formula
  structure: MathML").

`Scene::on_command(&StateDiff, &mut SceneContext)` — declared since the trait was written, never called
until this step — now fires for every command that reached the tree, from the feed *and* from the
keyboard/AT path. A command that was refused does not fire it.

**Known limits, deliberately:** `--reply` and `--inspect` cannot both own stdout. Two earlier
entries have since been settled: label addressing by `Target` (step 22b), and interrupting a running
beat by `stop` (step 22c), whose `clear`s point at the beat and the queue rather than at a variant of
`play` — for the same reason an interruption is a command of its own.
