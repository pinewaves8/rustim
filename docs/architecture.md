# rustim — architecture

> Detail reference for [`CLAUDE.md`](../CLAUDE.md), which is the entry point. Loaded on demand:
> `CLAUDE.md` carries the routing table that says when to read this.
>
> Excerpted verbatim from `CLAUDE.md` on 2026-10-18 — the `## Architecture` section, the teaching
> tier, and the notes on where the workspace diverges from the tutorial.


## Crate map

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

## The frame

```text
winit event loop → Scene::update(dt) → SceneContext::advance(dt) → AnimationScheduler::update
                 → SceneContext::build_draw_list(&mut DrawList)
                 → DrawList::damage_since(&previous_frame) → Damage
                 → Renderer::render_damaged(&DrawList, Damage) → softbuffer (damaged region) → screen
```

`SceneContext::build_draw_list` is the only place world coordinates are materialised. Everything
upstream mutates *local* points and an `Affine2`; the flattening step folds each mobject's transform
together with its ancestors' and emits paths.

## Damage rectangles

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

## The camera

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

## Pointer input

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

## Axes, number lines and plots

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

## Strokes

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

## The linter

`{"action":"check"}` is a **read**: it lints the frame *and* the timeline that has not played yet,
changes nothing, and answers a `report` — the fifth reply presence (see "The feed"). The split is the
design: `rustim_scene::check::check(tree, viewport)` is a pure function of the tree and the frame,
`check::pacing(beats, narration)` is a pure function of timelines, and `SceneContext::check` is the
only place the two meet — only the context holds the viewport, the queued beat durations
(`Sequence::durations`) and the narration lines still waiting for their due time. A pure
`command::apply` function has none of them, so the arm is in `handle`.

Seven codes, and the severity is a **function of the code**, never a field a caller supplies:
`OutOfFrame` is `Error` (an object a viewer cannot see is the picture being wrong);
`Overlap` / `TextTooSmall` / `LowContrast` are `Warning` (legible but degraded); `EmptyText` /
`BeatTooFast` / `NarrationTooFast` are `Info` (geometry cannot measure whether an explanation is
right, and a sub-second beat is a legitimate flourish — report, do not judge). The `severity` field
still travels on the wire because a model reading a finding should not have to hold the table.

The thresholds are **constants in `check`, not wire parameters**: `MIN_TEXT_PX = 18`,
`MIN_CONTRAST = 3.0` (WCAG large-text floor; the palette lives at 7.4:1 / 4.7:1, so the rule flags a
colour a caller invented to be dim), `OVERLAP_FRACTION = 0.1`, `MIN_BEAT_SECONDS = 0.2`,
`READING_CHARS_PER_SECOND = 8` — the same figure the caption band's `hold_seconds` is based on, so a
line the captions hold long enough to read is a line this check accepts. Every `detail` states the
number *it* measured, which is what lets a host that disagrees filter the report; a configurable
threshold table would be a second answer to one question.

Five properties are load-bearing:

- **Geometry follows the camera.** `out_of_frame` compares each top-level object's subtree box
  against `visible_height()/zoom` and the viewport aspect, and `text_too_small` measures the line box
  through `parent_transform · transform` times pixels-per-unit, so zooming in clears both and zooming
  out creates both. Contrast is the one camera-free rule (it is a property of the paint).
- **Only top-level objects are compared for out-of-frame and overlap.** A group's children are laid
  out *to* overlap inside it; comparing pairs would flag every template. Text and contrast are
  per-node, walked in `draw_order()` so a report is stable.
- **Overlap is an area fraction of the smaller box, and zero-area boxes are excluded.** A horizontal
  plot crossing an axis has zero intersection area; flagging that would make every graph yellow.
  Neighbours sharing an edge have zero intersection too, which is why the rule is not "any
  intersection".
- **Fully transparent nodes are skipped**, and a pass whose alpha is below `0.5` is not judged for
  contrast — an invisible colour is not a legible-colour problem. Only the channel the node actually
  paints is judged: the fill when there are points or text, otherwise a finite positive-width stroke.
- **A finding names ids.** Node findings carry `target` (always the id, whatever spelling the scene
  used) and `label` when the node has one; `overlap` also carries `other`. Pacing findings — which
  have no node at all — omit all three rather than writing nulls.

Pacing is deliberately half-blind: **only beats not yet handed to the clock are paced.** The beat on
the clock has only its remaining time known, and calling a half-played one-second beat "too fast"
would flag a beat that was fine when it was queued. A duration of exactly `0.0` is legal (an instant
snap) and is not flagged. Narration is sorted by due time and each gap is compared with
`chars / 8.0` for the line before it.

`check` cannot be a `batch`/`validate` member: a list replies with one diff and `check` answers a
report, so `check_batchable` refuses it under `commands[i]` before any member runs — the same rule
that keeps `query` and `hello` out. It has no fields, so it has no `OutOfRange` of its own; an
unknown field is `unparseable`, and otherwise the answer is always a report, empty findings
included. A degenerate viewport or non-finite camera only makes the visible window unknown, so those
two rules do not fire rather than the command failing.

Evidence: 20 unit tests in `check.rs` over hand-built trees (including the palette passing the
contrast floor, a camera zoom clearing an out-of-frame finding, a child overlapping its parent not
being reported, a fully faded object reporting nothing, and the report's field order), five context
tests, a conformance case through the real binary, and — the point of the whole feature — **all
eight goldens unchanged**: a read draws nothing.

## Scene plans

A lecture an AI composes is not a bag of commands — it is an ordered set of **steps**, each saying
something, arranging something and moving something. `rustim_scene::plan` makes that a **document**
the engine can pre-flight, return to, replay and be asked "where are we?" about. That is the four
capabilities `贾维斯集成方案.md` §5.3 named — 可预演、可回退、可重放、可被实时模型导航 — and they
are the difference between a command log and a plan.

The wire is four actions: `plan.load` (a document), `plan.next`, `plan.goto` (an index) and
`plan.status`. A step is `say` (a line, queued on the narration track due now — so it reaches the
live region, the caption band and the `.srt` for free), `ops` (primitive commands applied in order,
the same vocabulary a `batch` accepts, including layout and the six `explain.*` templates) and
`beats` (animation specs queued as successive beats; parallel motion inside one beat is one
`group`, which is how the timeline has always spelled it). Every part is optional, but a step with
none of them is refused: it does nothing, and a model that generated one has a bug worth hearing
before the lecture runs.

Five properties are load-bearing:

- **`plan.load` is a pre-flight, not a stored blob.** `Plan::check_shape` refuses a non-empty
  document with an empty step, a `say` that trims to nothing, and any read command (`query`,
  `check`, `hello`, `batch`, `validate`, `plan.*`) inside an `ops` list — reusing `member_refusal`,
  so "what can be in a batch" has one answer. Every step is then **dry-run** against a scratch
  copy of the session through the same `handle` a live command reaches, so a label no earlier step
  created, a malformed animation or a missing `into` is refused under `steps[i].ops[j]` /
  `steps[i].beats[j]` **before the first sentence**. What the dry run cannot see is an animation's
  *end state* — a `fade_out`'s removal lives in `finish`, not in its spec — and that refusal
  arrives at `plan.next` instead, at the same path. The blind spot is documented rather than
  papered over, and a test drives it.
- **Loading stops the timeline and baselines the settled picture.** The baseline is
  `{ tree, clock }` as they are after `stop()`; a beat captured a tree the restore would replace,
  so leaving one running would make `goto` meaningless. Loading **replaces** any previous plan and
  its baseline, and the undo stack is deliberately not consulted — a checkpoint taken before a load
  knows nothing of the plan, so the two histories stay independent.
- **`plan.next` is not a transaction**, for `batch`'s reason: ops run in order, beats are built
  only after them (a beat may name a label a step's own op creates), a refusal leaves the earlier
  ops applied and the cursor where it was — and `plan.goto(cursor)` is the recovery, rebuilding
  the step's boundary from the baseline. The reply is the net diff, empty for a step that only
  speaks and animates, because queueing is not a tree mutation.
- **`plan.goto(n)` is a function of `n` and the plan.** It restores the baseline tree (retiring
  the abandoned present's ids, so a stale handle is `no_such_node` rather than a different object
  wearing the id), restores the clock, and replays steps `0..n` with every beat driven through
  the **reduced-motion path** — `begin`, `ease(1.0)`, `interpolate`, `finish`. Using the
  scheduler's own settled path rather than spelling those four calls here is what makes a
  `FadeOut`/`Uncreate` actually remove its target during a replay; hand-rolled settling would
  leave ghost nodes. It is **silent** (a restore is not a re-performance, so a replay's `say`s are
  dropped and the narration track is put back), it interrupts playback, and it leaves pause,
  tempo, motion mode, event subscription and the checkpoint stack alone — those are the viewer's
  policy, not the lecture's position. `goto(0)` is the baseline; `goto(total)` is the end. A
  refusal mid-replay rolls the picture, the clock and the narration back and answers
  `InPlanStep`; the cursor does not move.
- **`plan.status` never refuses.** "There is no plan" is `loaded: false` — the answer the caller
  asked for — not an error, so a host can poll it from the first frame. `say` is the *next* step's
  line, and `playing` is `is_playing()` (the queue's or the scheduler's), which is the honest
  answer to "is the picture still moving" while a step's beats play out.

The reply shape is the sixth presence, `plan` (see "The feed"); `plan.load`, `plan.next` and
`plan.goto` answer a `diff` like every mutating command. The three refusals are `no_plan`,
`plan_finished` and `in_plan_step { step, member }`; an out-of-range `goto` is `OutOfRange` under
`step`, and a malformed document is `OutOfRange` under `plan.steps` / `steps[i].say` /
`steps[i].ops[j]` / `steps[i].beats[j]`, so the index names the mistake rather than the verb.

Deliberately not a plan: **named cues** (`at_ms` marks for a host's event loop — a step boundary is
already observable through `plan.status` and the event stream already reports `beat_started`), and
narrating a `goto` replay.

## The timeline: three layers

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

## Rate functions

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

## Composition: a fourth layer, inside the first

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

## Family morphs: transforming a subtree

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

## Morph routes: `path_arc`

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

## Reduced motion

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
`rustim_app::motion::from_platform()` — which is **best effort, and reads two platforms of three**.
On Linux it runs `gsettings get org.gnome.desktop.interface enable-animations` (the GNOME-family
schemas, which UKUI here honours; KDE is not read and the XDG portal has no such key) and returns
`Full` for everything else: a missing command, a missing schema, an unparsable answer. On macOS it
calls `NSWorkspace.accessibilityDisplayShouldReduceMotion` **in process**, through `objc2-app-kit` —
no subprocess, no parsing, and it must be called from the main thread, which it is (the one caller is
`main`, before the event loop). Windows (`SPI_GETCLIENTAREAANIMATION`) is named in that module and
deliberately unread, so that platform has the flag and not the preference. None of the three is a
listener, so a desktop setting changed mid-run is not observed — the explicit switch is the runtime
route. That mapping lives in `rustim-app` and not in the kernel for the same reason `accesskit` does:
`rustim-anim` compiles to wasm, and a crate that spawns a process or calls AppKit does not.

**The preference is asked only by the window, not by anything that writes a file.** `--png`,
`--video` and `--video-png` are all `Motion::Full` unless `--motion` says otherwise: a preference is
something a *player* has, and an artifact that changed with the machine that produced it would not be
the same artifact. The video half of that rule came first; the still half arrived when reading the
preference on macOS turned `a_tempo_slows_the_still_into_mid_fade` into a machine-dependent test — it
passed here and failed on the CI runner, which has the setting on. The rule for tests follows from
it: **a test that asserts an animation's state mid-beat passes `--motion full` explicitly** (the
golden suite always did), so that it opts in to being exposed to a desktop preference rather than
silently inheriting one.

## The two structural changes relative to Manim

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

## Core traits

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

## Accessibility

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

**The frame count is a frame count, not a duration, and that is a trap.** 600 frames is ten seconds
on a machine that renders at 60 fps, and it is **2.4 seconds** on one where an idle frame costs
0.12 ms — the settle animation ends, `Damage::None` takes over, and the loop spins. A screen reader
needs longer than that to notice a new application and ask for its tree, so a macOS run that reported
"not attached, 0 pushes" with VoiceOver audibly running was measuring the *probe's* brevity, not the
backend. `--frames 400000` on that machine (≈50 s) gave `assistive technology: attached`,
`1 full, 399919 incremental` — one full snapshot on attach, exactly the `InitialTreeRequested` reset
the adapter documents, and a difference per frame after it.

**So the probe now holds for a wall-clock minimum.** `PROBE_MIN_SECONDS` in `main` is 10 seconds, and
probe mode does not end on the frame count alone: the count is a *floor*, and the run continues until
it has been open long enough for a reader to act on. That is what makes the report's "not attached"
mean **no reader** — which is the answer the report exists to give. Before the fix it could equally
have meant *not enough time*, and on a fast machine it did.

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
#
# macOS, run 2026-10-18: VoiceOver attached, `1 full, 399919 incremental` — see above for the
# `--frames` trap. That is the first observation of any platform's backend on a real reader,
# and it is what turns "compile-and-probe only" into a measured claim. Windows is still
# unobserved: Narrator has never been started against this program.
```

## Narration, captions and self-voicing

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

## Video export and subtitles

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

## Text

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

## Wire-level text, formulas and groups: the `Typesetter` seam

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

## Web (WASM)

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
(`343, 90`)..(`936, 629`), 16 pixels differ by one LSB (0.002%), max channel error **1** — and
re-measured on 2026-10-18 in Chrome 155 on macos-aarch64, where **every one of those numbers is the
same** (same box, same 16 pixels, same max error 1, same `6.28e-16` cursor-anchor drift). That is not colour management —
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

## Naming

`Scene`→`Scene`, `Mobject`→`Mobject`, `Animation`→`Animation`, `construct()`/`play()`→same,
`MathTex`→`Formula`, `Create`→`Create`, `Shift`→`MoveTo`, `NumberLine`→`NumberLine`,
`Axes`→`Axes`. `Plot` is not a Manim class: it is the wire spelling of an `Axes.plot` curve whose
sampled points the host supplies (see "Axes, number lines and plots").

## The teaching tier (`rustim-director`)

A competent planner can drive the primitive protocol; a real-time model, or a template, should not
have to compute where a caption goes. `rustim-director` is the layer that turns the three sentences a
lesson actually uses — *next to that*, *in a row*, *fit the frame* — into tree mutations. Its first
slice is `layout`: `next_to`, `arrange`, `fit`, pure functions over a `SceneTree` and node ids.

Four decisions are the whole design:

- **Everything is measured, in world space, from `SceneTree::world_bounds`** — the box of a node's
  materialised anchors with every ancestor fold in, the same primitive the accessibility tree and
  `camera focus` use. So a rotated object is placed by the box a viewer sees, a nested node is placed
  correctly, and a text or formula node is placed by its declared box like any other. A node with no
  box is refused (`Unmeasurable`), never treated as a point.
- **Placement composes onto the deferred transform**: `translate(δ) · transform`, never a rewritten
  point array, so a placement costs O(1) and does not disturb a rotation or scale the object
  already carries — the contract `set position` established. `fit` composes a uniform scale about the
  group's centre in the same way.
- **The delta is converted into the node's parent space** before it is written, because that is the
  space `Mobject::transform` maps from. `SceneTree::to_parent_space` is the public spelling of the
  check `rustim_anim::ops` already carried privately: `kurbo`'s `Affine::inverse` is infallible and
  returns `NaN`s for a collapsed matrix, so the determinant is checked and the caller gets
  `Singular` instead of a blank frame.
- **Refusals are values**: an unknown or unmeasurable node, a target nested inside another target
  (it would move or scale twice), a non-finite buff, a box with no extent. Duplicate ids are
  collapsed, first occurrence wins. A placement that changes nothing writes nothing, so a diff does
  not report a frame that did not move.

`next_to` is Manim's `next_to` with the default aligned edge: near edges separated by exactly `buff`
(a world-space gap a viewer can see), perpendicular coordinates centred on the reference. `arrange`
uses Manim's arrange-with-`center=True` semantics — equal *gaps*, not equal widths, and the
arrangement is centred on the group's original box — with a column running top to bottom in reading
order. `fit` is `scale_to_fit_width/height` collapsed into one call: the tighter axis decides, so a
circle stays a circle.

**The three functions are the wire's `next_to` / `arrange` / `fit` actions, and `rustim-scene`
depends on `rustim-director` directly rather than installing a trait.** The earlier plan was the
`Typesetter` pattern — a seam a host installs — and it was the wrong shape here: layout is pure tree
geometry (reads `SceneTree`, writes transforms, names no font, opens no file), there is only one
implementation, and an uninstalled seam would make a build refuse actions it clearly has the geometry
to run. The guard is not weakened: the director depends only on `rustim-math` and `rustim-mobject`,
so it is wasm-clean and the six-crate wasm build passes with the edge in place. What the edge does
mean is that **anything needing `rustim-text` (a template that typesets) belongs in a crate above
`rustim-scene`**, never here — Typst must not reach the kernel. The protocol spelling is in
`rustim-scene::command`: `{"action":"next_to","target":…,"reference":…,"direction":"left|right|up|down","buff"?}`,
`{"action":"arrange","targets":[…],"axis":"row|column","buff"?}`,
`{"action":"fit","targets":[…],"width":…,"height":…}`, with defaults `0.1` (`SMALL_BUFF`) and `0.25`
(`MED_SMALL_BUFF`) — not the `0.5` the plan assumed; all three are **pure** `apply` arms, so a `batch`
or `validate` carries them with no host involvement. `compose` is therefore not a separate verb: it
is a `batch` containing these and primitive commands.

Two hardening rules were added with the wire: **`next_to` refuses a reference that is inside the
target** (moving the target carries the reference, so the requested gap is unreachable; the reverse
nesting — a label beside its parent — stays legal), and **bulk placements check every target before
the first write**, so a refusal never leaves a half-arranged tree.

**The second slice is `attention` — `point_at` and `highlight` — and its split is deliberately not
layout's.** The geometry lives in the director; the animation composition lives in `rustim-scene`,
because a dim-and-restore beat needs `rustim-anim` and the director's dependency set (only
`rustim-math` + `rustim-mobject`) is what keeps it wasm-clean. Two pure functions:

- **`attention::pointing_arrow(tree, target, direction, buff, length, thickness) -> Mobject`** builds
  an ordinary filled `ShapeKind::Arrow` whose **tip** sits `buff` outside the subject's world box on
  the named side and whose shaft runs *outward* from there — so `Direction::Down` means "below the
  subject, pointing up at it". The placement is one affine on the deferred transform
  (`translate(tip) · rotate(angle) · translate(-length, 0)`), so the arrow is a normal node: it can
  be faded, moved or removed. `buff` may be negative (overlapping tip), `length` must be positive and
  finite, `thickness` finite and non-negative; a stale id is `Unknown` and a node with no box is
  `Unmeasurable`, checked in that order so the two bugs stay distinguishable. Defaults are
  `DEFAULT_POINT_AT_BUFF = 0.1` (Manim's `SMALL_BUFF`, the same `next_to` uses) and
  `DEFAULT_POINT_AT_LENGTH = 1.0` (an eighth of the frame height — a pointer, not a bar).
- **`attention::spotlight(tree, target) -> Vec<NodeId>`** names every **picture-bearing** node
  outside the target's subtree, in draw order, for `highlight` to dim. "Outside the subtree"
  includes ancestors on purpose: a formula's root is a picture-bearing container over its glyphs, so
  spotlighting one glyph should dim the rest of the formula too. The list is computed at command
  time, not remembered — the caller captures the opacities before animating, and a node created
  later is simply not part of this spotlight.

**On the wire, the two actions reply differently, and that is the point.** `point_at` is a pure
`apply` arm: it resolves the target, builds the arrow (honouring optional `color` and `label`), and
returns `diff.created` with the new id. `highlight` is intercepted by `SceneContext::handle`, because
it queues a beat and only the playback machine can do that: it resolves the target, validates `dim`
(finite, `0.0..=1.0`), `duration` and `hold` (finite, `>= 0`), collects `spotlight`, and — unless
there is nothing to dim or `dim == 1.0` — appends a single self-contained `Succession`:

```text
AnimationGroup(dim each node) → Wait(hold) → AnimationGroup(restore each node)
```

Every member is an `Animate` on `StyleProperty::Opacity`, so the restore target is the value
**captured when that animation begins**, not a hard-coded `1.0` — a scene that was already partly
transparent comes back exactly as it was, and a `Succession` begins a member when its turn arrives,
so the restore sees the dimmed state rather than the original. The beat's reply is a
present-but-empty `StateDiff` for `play`'s reason. Defaults: `dim = 0.25`, `duration = 0.4`,
`hold = 0.5` (`DEFAULT_HIGHLIGHT_*` in `rustim-scene::command`). `point_at` names `direction`,
`buff`, `length`, `thickness`, `color`, `label`; `highlight` names `dim`, `duration`, `hold` — all
optional, all absent rather than `null` on the wire.

Evidence for the slice: four director tests (the tip and outward tail from all four sides, a
negative buff producing an overlapping arrow that is still a normal node, stale/degenerate refusals,
and a spotlight excluding the subject's subtree and any picture-less node), two `command.rs` apply
tests for `point_at`, three `context.rs` tests (dim → hold → restore puts every opacity back, an
empty spotlight or `dim == 1.0` queues no beat, and bad factors/timings are refused under their
field names), four round-trip fixtures (both spellings for both actions), and one conformance case
that points at a labelled square, reads the arrow back through `query`, highlights, and sees
`dim: 2.0` refused. `ACTIONS` 22 → 24. **All eight goldens are unchanged**, and the count moved
826 → 836.

**The third slice is `annotate`, and it is the one teaching action that cannot be a pure `apply`
arm.** A callout is a label *and* an arrow placed from one measurement of the subject: the arrow's
tip sits `buff` from the subject's box, its shaft runs `length` outward, and the label's near edge
sits `buff` past the tail, centred on the subject's perpendicular axis. Two layers:

- **`attention::callout(tree, subject, label_extent, CalloutStyle) -> Callout`** is pure geometry.
  `CalloutStyle { direction, buff, length, thickness, arrow }` is the caller's half; the validation
  is `check_arrow` — the same function `pointing_arrow` now calls — plus a non-finite or negative
  `label_extent` refused under `"text"`, and the subject measured exactly as `pointing_arrow`
  measures it. It returns the label's centre and the optional arrow **mobject**, built by the same
  `arrow_at`, so the two placement functions cannot drift apart. `arrow: false` keeps the placement
  (one arrow-and-gap away) and drops the arrow, so the label never lands on the subject. The
  `label_extent` comes from the **typesetter**: the label's own box decides how far out it goes, so
  a long sentence pushes the label out rather than overlapping the arrow.
- **The wire arm lives in `SceneContext::handle`**, because a label means typesetting and `apply`
  may not read fonts. It refuses `Unsupported { feature: "text" }` when the build has no typesetter,
  resolves the target, asks the host for the `TextBlock`, takes its `local_bounds()` as the extent,
  calls `callout`, and only then inserts: the text node first (so `created[0]` is always the label),
  the arrow second, with `color` applied to both halves. Every refusal is therefore atomic — a bad
  `length` leaves the tree exactly as it found it, so a model retries against the scene it had.

**The placement is static, and that is a decision rather than a gap.** Following a moving subject
would need a per-frame constraint the tree deliberately does not have, and parenting the label to
the subject would carry it through the subject's rotations — not what a caption does. Re-sending
`annotate` re-anchors it, and a host that wants to move it can `play`/`set` it like any other node.
Evidence for the slice: three director tests (the label centre past the tail from all four
directions, an arrow-less callout keeping the placement, and the three refusals), three context
tests (the full two-node placement with the colour reaching both halves, `up` + `arrow: false`, and
atomic refusals for a missing typesetter / a bad `length` / an unknown label), two round-trip
fixtures (the full spelling and the minimal one), and the conformance case extended to annotate a
labelled square and read the label back. `ACTIONS` 24 → 25. **All eight goldens are unchanged**,
and the count moved 836 → 842.

**The fourth slice is `explain.equation` — the first of the six templates — and it is the point at
which the teaching tier becomes a layer rather than a set of verbs.** The templates live in
`rustim-scene::templates`, **not** in the director, because they typeset: the director's dependency
set has no `rustim-text` and Typst must never reach the kernel. Every template builds its picture as
one `NodeKind::Group` with addressable pieces under it, and four rules make that shape work:

- **`created[0]` is always the group**, and the pieces follow it. A lecture names the group to fade,
  move or focus the whole explanation as one object, and names a piece to style one part of it. A
  formula's glyphs are **not** listed — they are the formula's own subtree, the same rule `add`
  follows when it reports a formula's root and not its symbols.
- **The group declares its box.** A group has no points, so `world_bounds` cannot measure it;
  `Build::finish` writes the union of the pieces' world boxes — converted into the group's frame by
  subtracting its origin — into the group's `local_bounds`, which is the declared-box fallback a text
  mobject already uses. That is what lets `next_to`, `camera focus` and `query`'s `bounds` treat a
  template's result as one object. It is a **snapshot**: a later `set position` on a piece moves that
  piece and leaves the group's box where it was; the module doc says so and treats re-placing the
  group as the caller's move.
- **Pieces are placed in the group's frame.** `Build::text` inserts a piece at a group-local position
  and *then* reparents it under the group, so the group's translation applies exactly once. The first
  version added the origin in both places and put every caption at twice the requested offset — a bug
  no golden could see, because no compiled scene uses a template yet. It is the same class as the
  `path_arc` trap: a piece's transform is relative to its parent, and reparenting is what decides
  which parent that is.
- **Refusals are atomic, and the group is the unit of rollback.** Parameters are validated before the
  group is inserted, and the typesetting half runs inside a closure whose error path calls
  `Build::rollback` → `remove_subtree(group)`. A blank caption, a non-positive `scale` or
  `caption_size`, a non-finite `position`, a missing typesetter, or a compiler failure leaves
  **nothing** behind.

`explain.equation` compiles through the same `Typesetter` seam `add` uses and colours through
`typeset::style_formula`, which was extracted from `add_typeset` for exactly this reason — the two
paths cannot drift on which nodes a formula's colour lands on. The caption is placed by measurement:
the formula root's world bottom, less `DEFAULT_GAP` (0.35), less half the caption's own height, so a
scaled-up formula pushes its caption down rather than overlapping it. The capability refusal names
the missing feature (`"formula"`, or `"text"` for a caption on a formula-only build), and the arm
lives in `handle` because `apply` may not read fonts. Evidence: two context tests (the
group/pieces/caption geometry including the declared box, and every refusal leaving the tree
unchanged), a rollback test whose fake typesetter succeeds on the formula and fails on the caption —
the only window in which the group already holds geometry — two round-trip fixtures, and a
conformance case that drives the real binary's typesetter through the template and reads the group's
declared bounds back; `ACTIONS` 25 → 26. **All eight goldens are unchanged**, and the count moved
842 → 846.

**The fifth slice is `explain.plot`, and it is where the template layer first has to say what a
function *is* without an expression language.** The wire has no evaluator by decision, so the caller
hands over `coefficients` (`c0 + c1·x + …`, lowest degree first) and the template evaluates them by
Horner's rule, bounded at `MAX_DEGREE = 16`. Everything else is the engine's: a `ShapeKind::Axes`
shaft with ticks, an `AxesScale` shared by the samples and the tick positions so a curve and its
labels cannot drift apart, host-typeset tick labels, and a caption measured off the lowest piece.
Four properties are worth carrying forward:

- **A curve that leaves the window comes back as several `Plot` pieces.** A flat point array cannot
  express a subpath break (`bezier_path_from_points` emits one `move_to`), so one polyline over
  `x² ∈ [0.5, 3]` would draw the chord between the two branches — a line the function never had.
  `sample_runs` splits the samples into contiguous in-range runs and `boundary` interpolates the
  crossing, so each piece ends *on* the range's edge rather than one sample short of it. A curve that
  never enters `y_range` is refused under `"y_range"`, through the rollback path (the axes are
  already in the tree by then).
- **Tick labels are mandatory, so the whole template needs `supports_text()`** — caption or not. An
  unlabelled graph is a picture, not an explanation, and the refusal is `Unsupported { feature:
  "text" }`. The labels' `label` is the number itself, which makes two axes' shared values
  (`"3"`, `"-2"`) `AmbiguousLabel` when addressed by label; the ids the diff reports are the
  precise spelling. Zero is skipped on both axes because the two would overlap at the origin.
- **The caption is measured off the lowest piece, not off the axes.** The test that first asserted
  `axes_bottom - gap` was wrong: with a 4-unit-tall axes the `"-3"` y label's line box reaches half a
  line below the shaft, so the lowest piece is a *label*. `Build::created`'s world boxes — not the
  requested `height` — are the honest input, which is the same rule `explain.equation` follows from
  the formula's ink.
- **The template reuses `Build` verbatim** — group first, pieces inserted then reparented into the
  group's frame, `finish` declaring the union, `rollback` removing the group. There is no second
  rollback implementation, and `handle` is the entry point for the same reason as `annotate`'s: tick
  labels need the host's typesetter.

Evidence: six `templates.rs` unit tests over the pure half (Horner order, a run cut at the edge, a
two-run curve, a curve outside the band, tick text without float noise, the label cap), three
`context.rs` tests (the full group/axes/curve/labels/caption geometry including the declared box, a
band the curve leaves twice yielding two `Plot` nodes with disjoint boxes, and every refusal leaving
the tree unchanged — including the late `y_range` one), two round-trip fixtures, and a conformance
case through the real binary that reads the group, the caption and a tick label back and asserts the
two-piece count. `ACTIONS` 26 → 27. **All eight goldens are unchanged**, and the count moved
846 → 856.

Deferred, and named so they are not rediscovered as gaps: labels that *follow* their subject. The
linter (out of frame, overlap, size, contrast, pacing) and the declarative `ScenePlan` were on this
list and have both since landed (see "The linter" and "Scene plans"); all six `explain.*`
templates landed too (`algorithm` was the last). Deliberately not ported with attention: an arrow that *tracks* a moving subject (the
arrow is a static node placed once; re-placing it is a `point_at` again, and following would need a
per-frame constraint the tree does not have), and spotlight-beyond-opacity (a true mask over the
rest of the scene). Several subjects at once is a `group` — `target` names one node, and the group's
subtree stays lit while everything outside it dims.

Evidence: fifteen layout tests over real `SceneTree`s — all four directions and the perpendicular
centring, a negative buff, a self-placement refusal, row and column arrangements with equal gaps and a
preserved group centre, duplicate collapsing, `fit` by the tighter axis with the aspect and centre
kept, an identity fit writing nothing, refusals for a degenerate box and an unmeasurable node, nested
targets refused in both orders, a child of a translated parent placed by its world box, a child of a
**scaled** parent moved by a world-space delta (the `to_parent_space` conversion), a rotated square
placed by its 2√2 box with its rotation intact, and a point-less node placed by its declared box —
plus two atomicity tests (the nested-reference refusal leaves the child where it was; an `arrange`
and a `fit` over a collapsed parent refuse with `Singular` before moving anything), four
`command.rs` apply tests and five round-trip fixtures for the wire spellings, and one end-to-end
`--script`/`--reply` conformance case that arranges by label and reads the pixel gaps back from a
`query`. **All eight goldens are unchanged** — nothing draws through this crate and no compiled scene
calls it — and the count moved 818 → 826.

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

