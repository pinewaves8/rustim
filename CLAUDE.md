# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

> **This file is the entry point, not the whole reference.** The detail lives in `docs/`:
> [`architecture.md`](docs/architecture.md) (every subsystem), [`pitfalls.md`](docs/pitfalls.md)
> (the traps, in full), [`history.md`](docs/history.md) (how each step landed, and why) and
> [`ai-integration.md`](docs/ai-integration.md) (the driver-facing protocol). **None of them is
> loaded automatically** — "读什么 / read what" at the bottom is the routing table for the job you
> are doing. Keep this file short (~600 lines, deliberately): new detail belongs in `docs/`.

## Repository state

A Cargo workspace at the repository root (15 crates), plus the design documents, `sdks/` (two
non-Rust conformance clients), `web/` (the browser demo), `tools/`, and `p0-spikes/` — three throwaway
Phase-0 probe crates (`gpu-probe`, `window-probe`, `cpu-bench`), excluded from the workspace. The
probes are the evidence for every number quoted in this file and in `方案建议.md`, so keep them.

**Read the design documents in this order:**

1. `方案建议.md` — **the primary document.** The commissioned research and proposal: measured Manim
   internals (with `file:line` evidence), Rust crate/version selections, the recommended architecture,
   a phased roadmap with acceptance criteria, an evaluation of ~20 existing Rust Manim-family
   projects, and the cross-platform analysis. It **supersedes** the two documents below wherever they
   disagree.
2. `从零用 Rust 重写 Manim 最小验证教程.wps` — a 22-page tutorial (OLE2/Word 97 format, extract with a
   custom reader; no `catdoc`/`libreoffice`). Defines a module layout, trait designs and code sketches
   for a deliberately minimal ~800-line version. **Its code blocks are illustrative sketches, not
   compilable code**, and the workspace does not follow its flat `src/` layout — see "Where the
   workspace diverges from the tutorial" in [`docs/architecture.md`](docs/architecture.md).
3. `需求探讨.txt` — the original design rationale: why Rust, why this is an architecture change rather
   than a translation, and three candidate technical routes with cost/benefit.

Then the **external-driver documents**, the normative pair for anything an AI assistant drives:

4. `驱动协议v1.md` — the wire specification: transports, the `hello` handshake and version rules, the
   command vocabulary, the reply shapes, the error vocabulary, conformance assertions, and what is
   explicitly unimplemented.
5. `贾维斯集成方案.md` — the architecture around that wire: why the engine and the assistant are
   separate products, the five switching mechanisms, the tool tiers, the repository layout, and the
   phased roadmap (Phase 0 and Phase 1 are what has landed; the rest is planned).

`使用说明.md` is the **user-facing manual** — for someone driving the binary rather than changing it:
the scene list, the mouse and keyboard map, the headless flags, the JSON command vocabulary with
examples, export, the browser demo and troubleshooting. It is not required reading for engine work;
this file is the development reference.

**The two must be edited together.** A change to a command, a reply field, a refusal or a version
rule is a change to **`驱动协议v1.md`, `使用说明.md` and the conformance suite together**, and a stale
manual is a user-facing wrong answer that nothing in CI reads. "Confirmed by a doc" is not a test —
`crates/rustim-app/tests/conformance.rs` is what makes the wire's promises executable.

**Verified green as of 2026-10-18 on two platforms — linux-aarch64 and macos-aarch64: 938 tests**
(unit, integration and doctest — `cargo test --workspace` reproduces the number), `clippy -D warnings`
and `cargo fmt --check` clean, the six-crate semantic kernel building for `wasm32-unknown-unknown` with
no parley or Typst in it, all eight golden scenes matching their checked-in baselines **on both**,
the conformance suite driving the real binary through `rustim-client` alone, two external runners in
`sdks/python` and `sdks/typescript` sharing no code with the engine, `rustim-daemon` verified over
real loopback WebSockets and `rustim-mcp` over a real stdio pipe.
[`docs/architecture.md`](docs/architecture.md) describes each subsystem;
[`docs/history.md`](docs/history.md) says which step last moved the count.

**Windows has no golden baseline**, so the suite **fails there by design** — a missing baseline is a
failure, never a skip. That is the expected state on that platform, not a regression: **do not invent
a baseline, and do not weaken the missing-baseline rule** to make a job green.

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

`cargo`/`rustc` 1.99 may not be on a non-interactive `PATH`. On the Linux floor machine the toolchain
lives in `~/.cargo/bin` — `export PATH="$HOME/.cargo/bin:$PATH"` — while a Homebrew macOS already
has it as `/opt/homebrew/bin/cargo`. Check with `cargo --version` before assuming either.

```bash
cargo run -p rustim-app              # launch the reference window
cargo run -p rustim-app -- --png out.png   # headless: the scene after 10 s to a PNG
cargo run -p rustim-app -- --inspect       # headless: the accessibility tree as JSON
cargo run -p rustim-app -- --a11y-probe    # windowed: did a screen reader attach? (600 frames)
#   600 frames is 10 s at 60 fps but ~2 s where idle frames cost 0.12 ms, and a reader needs
#   longer than that to ask for the tree — raise --frames before believing a "not attached"
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
echo '{"action":"check"}' | cargo run -p rustim-app -- --script - --reply --png out.png  # lint the frame
#   a script that sent {"action":"events","enabled":true} gets {"event":…} lines beside the replies
cargo run -p rustim-app -- --script crates/rustim-app/tests/fixtures/script/session.jsonl --inspect
cargo run -p rustim-daemon           # ws://127.0.0.1:7300, blank scene, auto clock
cargo run -p rustim-daemon -- --scene pythagorean --clock manual --no-typeset
#   a client attaches over WebSocket/JSON-RPC; see 驱动协议v1.md §1.2 and 使用说明.md §4.5
cargo build --release -p rustim-mcp  # the MCP server a client launches on stdio
cargo run -p rustim-mcp -- --scene blank --clock manual   # speak JSON-RPC on stdin by hand
#   initialize / tools/list / tools/call; see 驱动协议v1.md §1.3 and 使用说明.md §4.6
cargo test --workspace               # unit tests across all crates
#   Windows: the 8 golden cases FAIL by design — no windows-*.png baselines are committed
#   (a missing baseline is a failure, never a skip). Read the golden rules below before touching them.
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
python3 -I sdks/python/conformance.py           # ...and from Python, sharing no code with the engine
node sdks/typescript/conformance.mjs            # ...and from Node, a third independent implementation
python3 -I sdks/python/jarvis.py --offline --out /tmp/lesson.mp4   # the example AI director, no model
python3 -I sdks/python/jarvis.py "用三步动画解释勾股定理" --png --out out.png  # ...with a real model
cargo test -p rustim-client          # the client's framing rules, over in-memory streams
cargo test -p rustim-daemon          # sessions, RPC envelope, and real loopback WebSockets
cargo test -p rustim-daemon --test daemon   # ...only the socket-level integration suite
cargo test -p rustim-mcp            # the MCP envelope and tools, natively
cargo test -p rustim-mcp --test mcp # ...only the real-process stdio suite
cargo test -p rustim-director       # the teaching tier's layout, attention and annotation, over real scene trees
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

`p0-spikes/` holds the three Phase-0 probe crates plus the Manim comparison evidence:
`manim-pythagorean.py` / `manim-pythagorean.png` (the reference frame, rendered by `/tmp/manim-venv`),
`rustim-pythagorean.png` (our frame), `rustim-p0-frame.png` (the window probe initial frame — evidence
for the presentation path rather than the rasteriser) and `compare_manim.py`, which produces the pixel
statistics quoted in `方案建议.md` §5.2. Regenerate our side with
`cargo run --release -p rustim-app -- --png p0-spikes/rustim-pythagorean.png`, then run
`/tmp/manim-venv/bin/python -I p0-spikes/compare_manim.py <ours> <theirs>`. The probes are excluded
from the workspace and are scratch work, not the engine — but they are the evidence for every number
quoted here and in `方案建议.md`, so keep them.

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

`tools/build-web.sh` writes `web/pkg/` and `web/pkg-formula/`, both gitignored. It needs two things
that are easy to be missing and are **not** implied by a working `cargo build`: the
`wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown`) and the
**`wasm-bindgen` CLI at exactly the version the crate pins** —
`cargo install wasm-bindgen-cli --version 0.2.100`, because `crates/rustim-web/Cargo.toml` pins
`wasm-bindgen = "=0.2.100"` and the glue and the crate must be the same release or the module refuses
to load. It also has to run under a `cargo` that can see that target: on a machine with both a
Homebrew `rust` and a `rustup` toolchain, the Homebrew one is not a rustup shim, so `rustup target
add` has no effect on it and `PATH` has to put the rustup toolchain first.

`web-check.mjs` needs Chrome and no npm install, and exits 2 rather than 1 when it cannot ask the
question (no browser, no DevTools endpoint). `$CHROME` wins; otherwise it searches `PATH` for
`google-chrome`/`chromium` and then the macOS bundles, because a bare `google-chrome` is a *Linux*
name and the check could not run on macOS at all when that was the only default. It is not in CI: the runner has a Chrome,
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
  platform is "download, review, commit". **`linux-aarch64.png` and `macos-aarch64.png` are checked in
  for all eight scenes; Windows has none**, so the suite still fails there by design rather than
  skipping. That remains development-list item 1.
- **What the macOS baselines turned out to be.** They were made on 2026-10-18 on a macOS 26 arm64
  machine, and every one is **byte-for-byte identical** to its `linux-aarch64` twin — same pixels
  (`max == 0`) and the same compressed bytes. That is worth stating plainly, because the rule above
  is justified by "NEON and SSE/AVX do not round identically" and the two checked-in platforms are
  the *same* architecture, so nothing here has yet exercised an actual cross-arch divergence. The
  per-arch split is a **guard**, not a reproduction of an observed difference: what it is waiting for
  is an x86_64 baseline, or a rustc whose float codegen moves. Do not read the byte-identity as
  evidence that the split is unnecessary; read it as "the effect it guards against has not been seen
  yet".
- **The CI runners are not the machine the baselines were made on, and for Linux that has always
  meant a red leg.** `platform()` derives the name from the *runner's* arch, and GitHub's
  `ubuntu-latest` is **x64** — `Ubuntu 24.04 | x64 | ubuntu-latest`, with the arm64 image under a
  separate label, `ubuntu-24.04-arm` — while `macos-26` is arm64. So the three legs ask for
  `linux-x86_64.png`, `macos-aarch64.png` and `windows-x86_64.png`. The committed pair is
  `linux-aarch64` + `macos-aarch64`, so **CI's Linux leg has never been able to pass either**, on
  any run, and the "Verified green … on linux-aarch64" above is a **development machine**, not a
  runner: to this suite they are different platforms. That is precisely what the CI-artifact route
  is for — both failing legs upload `target/golden-artifacts/`, so one push yields the
  `linux-x86_64` and `windows-x86_64` baselines that close the matrix, with no extra hardware.
- **The macOS route is now local, and it beats the CI-artifact route.** The baseline is produced by
  `BLESS=1 cargo test -p rustim-app --test goldens` on the machine itself (see "Commands"), in the
  **debug** profile, because both CI jobs that run goldens use a plain `cargo test`. `platform()`
  computes `<os>-<arch>` from the *host*, and GitHub's `macos-latest` runner is today macOS 26 arm64,
  so `macos-aarch64` is the name both sides use. That label floats, though — pin the matrix entry
  (`macos-26`) rather than trusting `macos-latest` to stay on arm64.
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
matched pair: the `scene_by_name` match (~:1106) and the literal array in `scene_names()` (~:1127) that
`--help` prints. The baseline guarantee above reads `--help`, so a scene wired into the match but not
into that array is *not* offered by `--help`, and the suite's check cannot see it — it compiles, runs,
and silently skips its baseline. Register it in both, then bootstrap the baseline for the arch you
are on; the other two platforms would be bootstrapped from the CI artifact, which is still owed
(above).
## Architecture

This section is **orientation only**: the crate map, the frame, the two structural changes relative to
Manim, and the core traits — enough to find your way and to know what not to break. The full account of
every subsystem — damage rectangles, the camera, pointer input, axes/number lines/plots, strokes, the
linter, scene plans, the timeline's three layers, rate functions, composition, family morphs, morph routes,
reduced motion, accessibility, narration and self-voicing, video export, text, the `Typesetter` seam,
Web (WASM), naming and the teaching tier — is in [`docs/architecture.md`](docs/architecture.md).

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
| `rustim-mobject` | `Mobject` (points / declared `local_bounds` / a wire-shaped `text: Option<TextBlock>` / style / deferred transform / `label` / `NodeKind`), `SceneTree` (with `camera`, `snapshot` / `insert_subtree` / `reorder_children`), stable `NodeId`, `Subtree`, shape builders (`square` / `circle` / `right_triangle` / `line` / `arrow` / `number_line` / `axes` / `plot` / `polygon` / `group`) and `AxesScale`, `text` (`TextBlock` / `TextRun` / `FontData` — the glyph geometry a shaper hands over), `critical_point`, `world_anchors`, `ids_with_label` | Real |
| `rustim-anim` | `Animation` trait (`begin` / `interpolate` / `finish`), `AnimationScheduler`, `Sequence` (the beat queue), `AnimationGroup` / `Succession` (lagged composition), `FadeIn`, `FadeOut`, `Create`, `Uncreate`, `MoveTo`, `Rotate`, `Scale`, `Transform` (with `path_arc`), `TransformGroup` (family morph, also arced), `CameraMove`, `WithRate` (a rate override wrapper), `Wait` | Real |
| `rustim-text` | Text shaping: an injectable `FontProvider` over `parley`/`fontique`, CJK from `assets/fonts`, `MissingGlyph` / `FontSubstitution` reports; `formula`, the Typst → per-glyph outline compiler, which also carries each formula's structure as MathML | Real |
| `rustim-scene` | `Scene`/`SceneContext`, the serde command layer, `event` (the wire vocabulary for out-of-band notifications), `a11y` (the platform-neutral semantic tree), `typeset` (the `Typesetter` seam — a trait, no fonts in it), `text` (`TextBlock` → `DrawOp::GlyphRun`, shared by the draw walk and both hosts), `templates` (the `explain.*` thick tools: one group, addressable pieces, a declared box and an atomic rollback) and `check` (the linter: pure frame checks plus timeline pacing — see "The linter") | Real |
| `rustim-director` | The teaching tier above the protocol: `layout` (`next_to`, `arrange`, `fit` — world-space placement measured from `world_bounds` and written as a composed translation or scale on the deferred transform) and `attention` (`pointing_arrow` builds a filled arrow whose tip sits a `buff` outside the subject's box; `callout` places a label just past that arrow's tail from one measurement of the subject, plus the arrow itself when asked; `spotlight` lists the picture-bearing nodes outside the subject's subtree, in draw order). All pure geometry; animations are *not* composed here — the director does not depend on `rustim-anim`, and typesetting is the host's, so `annotate`'s wire arm lives in `rustim-scene`. The `explain.*` templates' wire arms live in `rustim-scene::templates` because they typeset; the linter lives in `rustim-scene::check` for the same reason `handle` owns the arm — only the context holds the viewport and the queued timeline. `ScenePlan` also lives in `rustim-scene` (`plan.rs` for the document, `context.rs` for the executor), because a plan step runs commands and queues beats and both of those are the context's. Nothing here does I/O and nothing names a model | Real (layout, attention, annotation; all six templates and the plan executor live one crate up) |
| `rustim-render` | `Renderer` trait (`render`, `render_damaged`), `DrawList` and its `Damage` diff, `RenderStats`, the `vello_cpu` backend | Real (CPU only) |
| `rustim-export` | PNG encoding, the `compare` module that scores two frames, the `VideoSink` seam (`FfmpegSink`, `PngSequenceSink`), the `srt` writer, and the `rustim-compare` binary | Real |
| `rustim-scenes` | Scenes shared by more than one host — currently `Pythagorean`, which the desktop binary's `--scene pythagorean` and the browser demo both run | Real |
| `rustim-app` | The winit event loop, softbuffer presentation, keyboard and pointer input, announcements, the AccessKit adapter, the shaper↔rasteriser bridge, `typeset` (the desktop `Typesetter`: parley text, Typst formulas, installed on every `SceneContext` the binary builds), the command feed (`feed`: JSONL in, replies out, the one apply point), the caption band (`caption`) and self-voicing (`speech`) — all as a **lib** target — the export loop (`export_timeline`), eight `--scene` entries, the golden, script, caption and export suites | Real |
| `rustim-web` | The browser host: `Session` (scene + context + renderer + draw list, target-independent and testable natively), `aria` (the semantic tree → flat ARIA projection), `pointer` (DOM wheel/pointer units and signs → `rustim-math::gesture`, pure and natively tested), `typeset` (the formula-only `Typesetter`, `from_embedded`, under the `formula` feature), `web` (the wasm shell: canvas, rAF loop, pointer/wheel listeners, DOM/ARIA overlay), the command bridge and the optional `formula` scene | Real (Chrome-verified) |
| `rustim-daemon` | The long-running WebSocket host: `rpc` (the JSON-RPC envelope, method vocabulary and error codes), `session` (`Hub`, `SessionHandle`, `AttachSpec`, `ClockMode` — one thread and one `current_thread` runtime per session), `typeset` (the `Typesetter`: parley text and Typst formulas, built lazily), `server` (accept loop, per-connection dispatch, event fan-out), and the `rustim-daemon` binary (`--listen`, `--scene`, `--clock`, `--no-typeset`) | Real (loopback-verified) |
| `rustim-mcp` | The MCP host: `mcp` (the JSON-RPC 2.0 envelope over newline-delimited stdio — `initialize`, `ping`, `tools/list`, `tools/call`, version negotiation, and the rule that a refusal is an `isError` tool result rather than a protocol error) and `tools` (the five-tool catalogue and the arguments → protocol-command translation) | Real (stdio-verified) |
| `rustim-client` | The **client half** of the command protocol: `read_reply` (one reply line, the presence rule enforced) and `StdioClient` (spawn, `send`, `send_raw`, `write_raw`, `hello`, `close`, seq checking). No engine logic, no rendering, no platform code — it spawns a process, which is a host's job, so it is **not** one of the six wasm kernel crates | Real |

Dependencies point downwards only. `rustim-scene` depends on `rustim-render` for the `DrawList` type;
if the renderer ever grows a heavier surface, `DrawList` should move down instead. `rustim-director`
sits below `rustim-scene` (which depends on it for the three layout actions) and above the kernel
crates, depending only on `rustim-math` and `rustim-mobject`; it must never pull `rustim-text` into
that position — anything that typesets belongs in a crate above `rustim-scene` (see "The teaching
tier"). `rustim-web` sits
beside `rustim-app`, not under it: both are hosts of the same kernel, and neither depends on the
other. `rustim-daemon` is a third host on the same shelf — it depends on `rustim-scene`,
`rustim-scenes` and `rustim-text`, never on `rustim-app` or `rustim-web`, and it names no
rasteriser of its own — it never renders, so it emits no `frame_ready`. `rustim-mcp` is a fourth
host and the thinnest: it depends on `rustim-scene` and on `rustim-daemon`'s *library*, because it
embeds the daemon's `Hub` rather than owning a second session mechanism. That is the design, not an
accident — a tool call over MCP and a method over a WebSocket must meet in one
`SceneContext::handle`, and re-using the hub is what makes that structural rather than a promise. `rustim-client` is not a
host and not a kernel crate: it is the workspace's own *external*
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
## Hard-won facts — the ones that fail *silently*

The complete list, each with its mechanism and the evidence that found it, is in
[`docs/pitfalls.md`](docs/pitfalls.md). What follows is the subset that produces a **wrong picture or a
wrong tree with no error at all** — which is why they are repeated here. None of them is a style
preference.

- **`debug_assert!(expr)` does not evaluate `expr` in a release build**, so a side effect inside one
  happens in debug and *only* there. `Formula::insert` was written as
  `debug_assert!(tree.reparent(id, root))` — the spelling that reads best — and release builds never
  reparented, so every formula rendered at the origin on top of every other. Call the function, keep
  the result in a `let`, assert *that*. **The golden suite cannot catch it**: `CARGO_BIN_EXE_rustim-app`
  is the *debug* binary. Run `cargo test --release` when touching anything with a `debug_assert!`.
  This has now cost that function twice.
- **A blanket `Box<T>` impl that leaves out one method silently disables that method for every caller
  that goes through the box.** `impl<T: Animation + ?Sized> Animation for Box<T>` must forward
  `finish`; without the forwarding line a `FadeOut` fades out and *stays*, and only `--inspect` shows
  it. Drive a `FadeOut` through `AnimationScheduler::add` in tests, never a hand-called `finish`.
- **`fontique` hands back registered families in `HashMap` order**, so a `.ttc` picks a different face
  per process — two runs of the *same release binary* differed over the whole content box, which is
  what made `BLESS=1` produce a baseline the next run could not match. Sort families by name and give
  each generic slot **exactly one** id.
- **`parley`'s `complex-scripts` feature is not enough on its own**, and without ICU4X's `lstm`
  segmentation models CJK prints a data error per line while still drawing correctly. `rustim-text`
  names `icu_segmenter` with `lstm` purely so feature unification turns it on.
- **`serde` writes struct fields in declaration order**, so a hand-written round-trip fixture that
  spells them differently fails on a command the engine understood.
- **`serde_json` without its `float_roundtrip` feature does not round-trip every float it writes** —
  one ULP away, which breaks the byte-for-byte replay the protocol rests on.
- **A per-point route must be taken between parent-space points, not local ones.** Points here are
  local and a displacement can live entirely in the deferred transform, so `path_arc` evaluated on the
  local arrays is **exactly a straight line for the commonest request** — and the object still arrives
  where the caller asked, which is what makes it look like a feature that works.
- **`Mobject::critical_point` answers in the *parent's* space, not the world's.** Fold the parent in
  and a nested node lands somewhere no one asked for.
- **A filled open polyline is not a curve — the fill rule closes it**, so a sampled graph handed to
  `Fill::NonZero` paints the area under the curve. And **a flat point array cannot express a subpath
  break** (`bezier_path_from_points` emits one `move_to`), which is why a glyph's counter is welded
  back in through a hub and a doubled slit.
- **Manim's point array is read four points per curve, not three**, each curve restating the previous
  curve's endpoint — a closed 4-cubic circle is 16 points. Reading three at a time silently rounds a
  corner on any shape with four or more curves, and only the *image* shows it.
- **A net negative y scale silently drops a glyph run into `glifo`'s unhinted path** — it still draws,
  which is why `the_composite_scale_is_positive_and_uniform` exists. The same trap appears in Typst's
  frame-y-down / runs-y-up composition, where getting it backwards mirrors every glyph about its own
  baseline (baselines correct, letters upside down, and it reads as a font bug).
- **`Glyph::span` is the *element's* span, not the glyph's** — three glyphs from `"abc"` all share the
  literal's range, so label from it only when the shaped character count matches.
- **vello keeps two separate stacks and mixing them aborts the process** (a non-unwinding panic, no
  error value to catch): `push_clip_path`/`pop_clip` vs `push_layer`/`pop_layer`. Any clip or layer op
  forces `Damage::Full`, because the stacks are positional. `vello_cpu` also needs `ctx.flush()` before
  `ctx.render()` when `multithreading` is on.
- **`Animation::begin` is read-only**, so an in-place alignment must be split into a *plan* built in
  `begin` and *materialised* on the first `interpolate` (`TransformGroup`). Nothing may assume the
  child count in `begin` equals the one on the next frame.
- **A group begins every member eagerly; a succession begins each when its turn arrives** — so `MoveTo`
  and `Rotate` capture different starts from the same members, and the two produce different motions.
  A composed animation's `finish` must drive each child to `ease(1.0)` and `interpolate` **before**
  calling the child's `finish`. Neither is visible in a still.
- **`Uncreate` reverses outside the rate function here** (Manim composes the reversal inside it), so
  its timing differs from Manim's for asymmetric curves — a new rate function is not automatically
  available to it.
- **A focused node can be deleted by an animation**, and a dangling focus makes the consumer panic:
  release the stale focus with one `tree.get`. And `update_if_active`'s closure **never runs while no
  assistive technology is attached**, so nothing may depend on a call having happened.
- **Narration must not go through the announcer** — it holds one slot and drops anything within 750 ms
  — because a lecture's lines are content the listener is owed, not a burst of events.
- **A speech child that reads to end of input deadlocks if the parent waits with the pipe open**: the
  write end must be dropped **before** `wait`.
- **Welding rectangles into one path requires that every rectangle share its winding and close
  explicitly**, or `Fill::NonZero` subtracts the wrong regions.
- **Do not hand-derive a figure's bounds** — measure the union of the point arrays. The Pythagorean
  figure is 10 units tall, not `a + b = 7`; the first version overflowed the frame by 25%.
- **`ShapeParams` is a quarter-kilobyte**, enough to trip clippy's `large_enum_variant`, which is why
  `rustim_app::a11y::Request::Command` carries a `Box<Command>`.
- **winit traps:** `KeyEvent` cannot be constructed outside winit, so a key classifier must take a
  `KeyPress` (that is what puts the keymap in CI); the accessibility adapter must be built **before**
  the window is first shown, and every constructor takes an `&ActiveEventLoop`; do not approach
  winit 0.31 beta (`accesskit_winit` supports `^0.30.5` only).

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

## 读什么 / read what

**None of `docs/` is loaded automatically.** This is the routing table: open the named file before
changing the named thing, rather than working from this summary alone.

| Before you touch… | Read |
|---|---|
| the timeline — beats, `Sequence`, `AnimationScheduler`, the `Animation` trait, `finish`, reduced motion | [docs/architecture.md](docs/architecture.md) → "The timeline: three layers", "Core traits", "Reduced motion" |
| an animation — a new kind, `AnimationGroup`/`Succession`, `lag_ratio`, morphs, `path_arc` | [docs/architecture.md](docs/architecture.md) → "Composition", "Family morphs", "Morph routes" |
| the renderer, `DrawList`, damage rectangles, strokes, `DrawOp` | [docs/architecture.md](docs/architecture.md) → "Damage rectangles", "Strokes" |
| the camera, projection, pointer pan/zoom | [docs/architecture.md](docs/architecture.md) → "The camera", "Pointer input" |
| mobject vocabulary — a shape, axes, number lines, plots | [docs/architecture.md](docs/architecture.md) → "Axes, number lines and plots", "Strokes" |
| **the wire** — any command, reply field, refusal or version rule | [docs/ai-integration.md](docs/ai-integration.md) **and** `驱动协议v1.md`; then update `使用说明.md` and `conformance.rs` **in the same change** |
| the reply presences, `CommandOutcome`, `batch`/`validate`, `plan.*`, events | [docs/ai-integration.md](docs/ai-integration.md) → "The feed", "Events", "Scene plans" |
| the teaching tier — layout, attention, `annotate`, the six `explain.*` templates | [docs/architecture.md](docs/architecture.md) → "The teaching tier" |
| the linter (`check`) or a lecture plan | [docs/architecture.md](docs/architecture.md) → "The linter", "Scene plans" |
| text, formulas, Typst, CJK fonts, glyph runs | [docs/architecture.md](docs/architecture.md) → "Text", "Wire-level text…", "P1 step 5" |
| accessibility, announcements, captions, self-voicing | [docs/architecture.md](docs/architecture.md) → "Accessibility", "Narration, captions and self-voicing" |
| video export, the `.srt` writer, `rustim-compare` | [docs/architecture.md](docs/architecture.md) → "Video export and subtitles" |
| a host — `rustim-app`, `rustim-web`, `rustim-daemon`, `rustim-mcp`, `rustim-client` | [docs/architecture.md](docs/architecture.md) → "The frame", "Web (WASM)"; [docs/ai-integration.md](docs/ai-integration.md) → "The daemon", "MCP", "External drivers" |
| a golden baseline, or adding a scene | this file, "Commands" → the golden rules (they are normative) |
| a dependency version, a `cargo tree` CI guard (typst / `accesskit` / `skrifa`), or a trap just bit you | [docs/pitfalls.md](docs/pitfalls.md), and the guard jobs in `.github/workflows/ci.yml` |
| why something is the way it is, or which step moved a test count | [docs/history.md](docs/history.md) |

**Keeping the split honest.** New *reference* detail goes in `docs/architecture.md`; new *history* —
evidence, counts, "the count moved N → M" — goes in `docs/history.md`; a new silent trap goes in
`docs/pitfalls.md` **and** gets one line in the list above. Only orientation, commands, conventions
and that list belong in this file. `docs/history.md` is a changelog: it is allowed to be long, and
nothing in it should be the only place a fact lives.
