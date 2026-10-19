# Hard-won facts

> Detail reference for [`CLAUDE.md`](../CLAUDE.md), which is the entry point. Loaded on demand:
> `CLAUDE.md` distils the traps that fail *silently* and points here for the full mechanism.
>
> Excerpted verbatim from `CLAUDE.md` on 2026-10-18 (the `## Hard-won facts` section).


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
it.** `AnimationSpec` declares `target, type, duration, to, angle, factor, property, about_point,
into, path_arc, animations, lag_ratio, rate`; writing the nested-succession case as `{"type":"succession","animations":[...],
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
back in.** `bezier_path_from_points` (`rustim-mobject/src/tree.rs:456`) emits exactly one `move_to`
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


**`kurbo::Stroke::new` defaults to *round* joins and *round* caps, and those are not its enums' defaults.** `Stroke::new(width)` sets `join: Join::Round`, `start_cap: Cap::Round`, `end_cap: Cap::Round`, while `Join::default()` is `Miter` and `Cap::default()` is `Butt`. So the workspace's strokes have always had round ends — invisibly, because nothing could name a join until the stroke vocabulary landed — and writing that vocabulary's `Default` from the two enums was the obvious-looking thing to do and it **moved two golden baselines** (`axes` and `primitives`, the two scenes with a real stroke): 38 pixels past the tolerance on the axes shaft alone, 27 of them by more than 64.

The lesson is the one the golden suite exists to teach, in a new place: a default is a *claim about the picture*, and the only way to check it is to render. `rustim_math::stroke`'s `Join` and `Cap` therefore default to `Round` **because that is what the engine already did**, and the doc comment says where the number came from rather than presenting it as a choice.
