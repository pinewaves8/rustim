# AI integration — the driver-facing protocol

> Detail reference for [`CLAUDE.md`](../CLAUDE.md), which is the entry point. Loaded on demand:
> read this before changing anything a driver sees. The normative pair is `驱动协议v1.md` (the
> wire) and `贾维斯集成方案.md` (the architecture around it), with the conformance suite as the
> executable check.
>
> Excerpted verbatim from `CLAUDE.md` on 2026-10-18 (the `## AI integration` section).


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
the engine's version string, and the exact action / shape / animation / property / rate names this
build accepts, plus its limits and two advisory profiles. The lists are **derived from serde over the
same enums the parser uses** — `actions` from `Command::action_name` (36 verbs), `PROPERTY_NAMES`,
`AnimationKind::ALL`, `Rate::ALL` and `ShapeKind::ALL` for the nouns — so an advertised name is
accepted by construction, and a conformance test feeds every advertised word back through the binary
(see `crates/rustim-app/tests/conformance.rs`). A new variant that misses its `ALL`/name table fails a
count assertion rather than silently not being advertised, and the round-trip fixtures assert their
action set *is* `ACTIONS`. `profiles` is `["primitive", "teaching"]`: `teaching` means the build
accepts command lists (`batch`/`validate`), **not** that the director's templates exist. It is
advisory; `actions` is the exact answer a driver should test.
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

**`pause`** / **`resume`** / **`seek`** / **`tempo`** are the playback controls, and they act on
time rather than on the tree. `pause` freezes the animation scheduler and the lecture clock — the
picture holds, queued narration does not fire, a queued beat does not start — while `set`, `add`
and `camera` still apply, so "pause, edit, resume" is a usable gesture; `is_playing` stays true
through a pause, and a pause outlives `stop` (a viewer's hold is not content's to undo). `seek` is
**forward-only** and absolute in lecture time: it runs the timeline it crosses on the 1/60 grid, so
beats play and `finish` (a crossed `fade_out` removes its target), then lands exactly on `to`;
backward motion is what `checkpoint`/`undo` is for. `tempo` rescales both clocks (so narration
stays in sync) within `0.05..=16.0`; zero is `pause`, and out-of-range values are `out_of_range`
under `factor`. Both `pause` and `tempo` are session state, deliberately absent from the semantic
tree; `reset` returns to playing at 1×.

**`checkpoint`** / **`undo`** are the session snapshot — see the "Session snapshots" paragraph
below; `checkpoint` is a mark and answers with an empty diff, `undo` is the one host-answered
command whose diff names nodes.

`stop`, `pause`, `resume`, `seek`, `tempo`, `checkpoint`, `undo`, `batch`, `validate`, `camera`,
`motion` and `narrate` are the commands intercepted by `SceneContext::handle`
rather than by the pure `command::apply`; all but `undo` and `batch` return an **empty** `StateDiff`
for `play`'s reason — the timeline, the view, the motion policy and a spoken line are not nodes, and
the two non-empty diffs are the honest exceptions because restoring a tree or running a list of
mutations *is* a tree change. `camera` writes the view now or queues a
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

**A `query` also reports the session, beside the tree.** `{"seq":4,"tree":{…},"session":{…}}`, where
the session is `{paused, tempo, motion, playing, clock}` — the read half of the four controls a driver
can write (`pause`/`resume`, `tempo`, `motion`, `seek`). Until it existed the protocol was write-only
for all four, so a driver taking over a session could not ask what state the previous one left it in;
that is the one thing that matters when two assistants share an engine, and it is why this is not
merely redundant with the events a host could have been listening to all along.

It is **not a seventh presence.** The six-field discriminant is unchanged — `session` is present
exactly when `tree` is — so every client that counts the six by name, which is what `rustim-client`
and both SDK runners do, needed no change to keep counting them. And it is deliberately *not* in the
tree: a screen reader asking what is on screen has no use for the tempo, so `SemanticTree` stays what
it was.

## Events: what the timeline did

A reply answers a command. An **event** answers nothing — it reports something the engine did on its
own, which is exactly what a client driving a *continuous* engine cannot learn from a
request/response stream. `{"action":"events","enabled":true}` subscribes;
`{"action":"events","enabled":false}` unsubscribes; an optional `kinds` selects the vocabulary
(`["beat_started","animation_done"]` is the useful lecture subset, because `frame_ready` is one line
per presented frame). An empty `kinds` list is refused as `OutOfRange` under `"kinds"`. The command returns an
**empty `StateDiff`**, for `play`'s reason: a subscription is session state, not a node.

An event line carries **no `seq`** and exactly one key, `event` — that absence is the classifier:

```json
{"event":{"kind":"beat_started","beat":1,"members":2,"duration":1.5,"clock":0.25}}
{"event":{"kind":"animation_done","beat":1,"clock":1.75}}
{"event":{"kind":"frame_ready","frame":108,"clock":1.8}}
```

Three kinds, in `rustim_scene::event`:

- **`beat_started`** — a beat left the queue and reached the clock. `beat` is 1-based within the
  session, `members` how many animations the script put in it, `duration` its nominal length
  (`max(duration)` over the members, the scheduler's own total), `clock` the lecture time it began
  at. `Sequence::start_next` returns a `StartedBeat { members, duration }` for exactly this, and it
  is computed from the script's beat **even under reduced motion** — the hold that substitutes for
  it is the same length, so the reported shape does not lie about what was written.
- **`animation_done`** — every animation in that beat has finished. **Per beat, not per member**:
  a beat is the unit a script and a viewer both see, and per-member counting would force a client to
  know how reduced motion replaces N animations with one `Wait`. Emitted when the scheduler empties,
  from `advance_clock`, after the tick that finished it.
- **`frame_ready`** — the host presented or wrote a frame. **Host-produced**: the kernel has no
  frames, only virtual time. The variant lives in the kernel's vocabulary so a client parses one
  stream with one type, and so `wants_event(FrameReady)` is a filter a host can honour.

**What is deliberately not an event**: a `stop`, `undo` or `reset` does not finish anything (no
`finish` runs; a `FadeOut` in flight leaves its target, see "stop"), so none of them produces
`animation_done`. Telling a client an interrupted beat *completed* is a lie it would act on. Events
are a **log, not state**: a checkpoint does not capture them, `undo` neither replays nor erases them
(what the engine did cannot be unwound by restoring a tree), and `reset` clears the log, restarts beat
numbering at 1 and **keeps the subscription**.

**The kernel records, the host delivers.** `SceneContext` keeps a `VecDeque<Event>` capped at
`MAX_EVENTS` (4096, oldest dropped) because it has no transport — a seek can cross thousands of
beats, and a host that stopped reading must not turn that into unbounded memory.
`take_events()` is the single drain point and drains **unconditionally**, so an unsubscribed host
still empties the queue; `events_subscribed()`/`wants_event(kind)` decide what is *written*.
`feed::publish_events` writes the drained tail after each reply, and `feed::publish_frame` writes one
`frame_ready`; both flush. The host layer is what connects them:

- **Feed** (`--script … --reply`): events go to stdout only when `--reply` is on **and** the session
  subscribed — a strict request/response client that never sent `events` never sees a line it did
  not order. Events for the applied script are published right after the last reply.
- **Windowed**: `App::publish_events` drains every iteration after the frame counter advances; when
  `--reply` is off it drops them (`take_events` still runs) rather than letting the queue grow.
  `frame_ready` is written only for a frame that **changed the window** — `Damage::None` presents
  nothing and a failed rasterisation is not a picture, so an idle scene emits timeline events and no
  frame lines instead of a heartbeat sixty times a second. The event's `frame` numbers presented
  frames (`App::presented`), not loop iterations, which is why the two counters are separate.
- **Headless `--png`**: after the still is written, the settle-time events and one `frame_ready`
  (frame 1, clock 10.0) are published. Video export does not publish `frame_ready` yet — the sink
  loop is not a viewer, and that decision is written down rather than implied.
- **The browser does not publish events yet.** `Session::apply` accepts the subscription (it is
  `handle`'s own arm), but nothing drains the queue to JS, so a browser subscriber sees no event
  lines. The queue is capped, so this is an undelivered feature rather than a leak; the JS callback
  belongs with the Web interaction work, not here.

The end-to-end evidence is `crates/rustim-app/tests/script.rs`'s
`a_subscribed_script_receives_beat_events_and_a_frame_ready`: it partitions the real binary's stdout
into replies and events by the presence of `seq`, and asserts a `beat_started` (beat 1, one member,
0.5 s), an `animation_done` at ≈0.5 s and a `frame_ready` at the 10 s still. The kernel tests pin
what is not visible there: reduced motion reports the script's beat shape, a stopped beat never
reports completion, the filter suppresses kinds without stopping the drain, and a hand-added
scheduler animation is not a beat.

`add` builds more than polygons: `text`, `formula` and `group` are on the wire (see "Wire-level text,
formulas and groups: the `Typesetter` seam"), and their availability is in `hello`. A scripted
lecture is therefore `add` a sentence, `add` a formula, `play` the two beats, `narrate` the sentence
— with no compiled scene at all. `group` adopts existing nodes through `children`, which is how a
model composes a figure it can move and fade as one object without losing the parts' ids.

**Session snapshots** (`checkpoint` / `undo`) are what makes "that was the wrong thing to say" a
command instead of a re-run. A checkpoint stores the tree *with its camera*, the lecture clock, the
pause, the tempo and the motion mode — the session, not just the picture — in a stack of at most
`SceneContext::MAX_CHECKPOINTS` (16; the oldest is dropped). `undo` pops the newest, **interrupts
playback exactly as `stop` does** (no `finish`, so a half-played `FadeOut` cannot run its removal
after the restore), and returns a diff *computed* from the two trees rather than remembered:
`created` is a node the snapshot has and the present does not (undoing a delete), `removed` the
reverse (undoing an add), `modified` a survivor whose `Mobject::differs_from` its past — the
comparison deliberately ignores the `dirty` bookkeeping the draw-list diff replaced, so a `set` that
wrote the value a property already had is not reported. Three absences are decisions: **the timeline
is not captured** (a queued beat captured its start in `begin`; replaying it into a different tree
would apply a motion measured against a picture that no longer exists), **narration already spoken is
not unspoken**, and **`reset` clears the stack** (a mark from before a scene rewrite would restore a
tree the new script never built).

The restore has one structural consequence, and it is the reason `SceneTree` grew a set it would
otherwise not need. Cloning a `SlotMap` clones its slot generations, so a restored tree would hand
out again every id the abandoned future used — a host still holding one would silently address a
different object. **`SceneTree::retired` is the anti-ABA set**: `insert` skips any id in it (removing
and re-inserting until the slotmap yields a fresh generation), `retire_ids_of` adds the abandoned
tree's ids on undo, and `clear()` empties it because `clear` is a reset — a rewritten scene reuses
ids by design, while an undo must not. Evidence: ten `playback_control_tests` in `context.rs`
(what the diff names for an add, a delete and a property write; clock/pause/tempo/motion restored;
the exact snapshot after an interrupted fade, asserted on the whole `SemanticTree`; an abandoned id
never reissued; newest-first unwinding; the 16-mark cap; the `NoCheckpoint` refusal and its wording;
a no-op write reported as nothing; `reset` forgetting history), two `--script` stills (checkpoint →
add → undo is pixel-identical to never adding, with the retained circle as the counterfactual, and
a refusal that does not stop the feed), and one conformance arm over `rustim-client`. **All eight
goldens are unchanged** — a snapshot draws nothing — and the count moved 707 → 722.

**`batch` / `validate` are the list commands**, and they compress a scene build from a round trip per
step to one command per plan. `batch` runs its members in order through the same `handle` — so a
later member can address a label an earlier one created — and answers with the **net** tree change
computed from before and after (`diff_trees`, shared with `undo`): a node created and then written is
one `created` and no `modified`, and a node created and removed inside the list is in neither half.
It is **not a transaction**: the first refusal stops the list with `CommandError::InBatch { index,
cause }`, the prefix stays applied and nothing after the index ran, which is why a host wanting
all-or-nothing takes a checkpoint first. The list's *shape* is checked before anything runs — empty,
or a member that is `hello`/`query`/`batch`/`validate` — so a malformed list is atomic and refused as
`OutOfRange` under `commands`/`commands[i]`, never as `InBatch`. `validate` is the dry run: the same
members through the same `handle` on a scratch context (tree, camera, clock, pause, tempo, motion and
the checkpoint stack copied; the host's `Typesetter` moved in for the duration and back, so `text` /
`formula` validate against the provider that would really build them), answering an empty diff on
success and the identical error on failure. The scheduler and narration track are deliberately not
copied — a queued animation captured its start and is not replayable data, and a dry run must not
speak.

`CommandOutcome` is the in-memory vocabulary that keeps the five answers apart —
`Applied(StateDiff)`, `Queried(SemanticTree)`, `Checked(CheckReport)`, `Planned(PlanStatus)` and
`Greeted(Capabilities)` — and `reply_line` is the only thing that turns it into JSON, so the desktop
feed and the browser bridge cannot answer one command two different ways. The hook fires only for
`Applied`: a scene that re-fitted itself on every read would do work the question did not ask for, a
lint changes nothing, a plan status is a read of the planner rather than of the tree, and a greeting
changes nothing at all.

Two wire-format invariants are load-bearing, both pinned by tests:
- **Commands round-trip byte-for-byte.** `to_string(parse(json)) == json`. A host logs, replays and
  diffs these, so anything that serialises back differently is a bug.
- **Absent shape parameters are omitted, never written as `null`.** These payloads are model output;
  echoing `"width": null` back would teach the model that nulls are part of the protocol.

## External drivers and the AI surface

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

The transports are deliberately not part of the kernel, and **three are implemented**:
`--script - --reply` (newline-delimited JSON over stdio, one client, one process lifetime),
`rustim-daemon` (WebSocket/JSON-RPC, many sessions, reconnects and event push — see "The daemon") and
`rustim-mcp` (the Model Context Protocol over stdio — see "MCP"). All three end at the same
`SceneContext::handle`; a further transport is a wrapper around this protocol, never a second
dialect of it. `rustim-client` is the stdio transport's client half: `read_reply`
enforces the one-presence rule, `send_raw` enforces `seq`, `hello` is one call, and `close` returns
the exit status. The conformance suite in `crates/rustim-app/tests/conformance.rs` is written against
that crate only — it spawns the real binary, never reaching into `rustim-app`'s modules — so a passing
test is evidence that an outside driver can do the same. It asserts the advertised vocabulary is
accepted by the binary (every shape, every animation kind with its required fields, every rate),
that ids and labels agree across `add`/`set`/`query`, that a foreign protocol is refused readably and
the feed continues, that blank lines are skipped and counted, and that two clients replay one log to
one tree. The daemon's socket-level suite is the same claim in the other transport's terms:
`crates/rustim-daemon/tests/daemon.rs` attaches two clients to one session and asserts each sees the
same events, and a reconnect sees the objects the dropped socket created. The MCP adapter's own
suite is the same claim a third time, and the tool surface adds no verb the protocol lacks — see
"MCP". The teaching tier is not a roadmap item any more: `rustim-director`'s layout and attention,
the six `explain.*` templates, the linter and `ScenePlan` are all implemented and on the wire — see
the roadmap bullets, "The teaching tier" and "Scene plans".
(Checkpoint/undo/seek, `pause`/`resume`/`tempo`, the list commands, the event stream and the daemon
transport **are** implemented — see "AI integration".)

**`sdks/` holds two more proofs, in two languages that do not share code with each other or with the
engine.** `sdks/python/rustim_client.py` and `sdks/typescript/rustim-client.mjs` each implement the
framing rules and nothing else — one line in, one reply out, `seq` equals the physical line, exactly
one of the six presences — and each `conformance` runner uses only its standard library (Python's
`stdlib`, Node's built-ins) to drive the real binary over `--script - --reply`. They check the same
list: the handshake vocabulary (36 actions, 45 rates, both profiles), the primitive reply shapes and
their refusals, plan load/next/goto against a live tree, and **two independent processes replaying
one label-addressed log** to the same `SemanticTree`, field for field. Neither replaces
`crates/rustim-app/tests/conformance.rs` (which is more exhaustive); together they are the evidence
that a client reading only `驱动协议v1.md` can be written in another language and get the same
answers — the executable meaning of "switch clients freely". Exit codes are 0 pass / 1 a check
failed / 2 the question could not be asked, in both. The untrusted-input rule lives in the
environment, not here: these SDKs are code the user asked for, so `-I` for Python and no
`node_modules` for Node are about reproducibility rather than quarantine.

**`sdks/python/jarvis.py` is the worked example of the other half of the separation: an assistant,
not an engine.** It asks any OpenAI-compatible chat-completions endpoint (`RUSTIM_LLM_BASE` /
`RUSTIM_LLM_MODEL` / `RUSTIM_LLM_KEY`, falling back to `DEEPSEEK_API_KEY` then `OPENAI_API_KEY`) for
a `plan.load` document, flattens it into one command log, and drives the unmodified binary through
`rustim_client`. It contains no engine logic and no rendering; swapping assistants is swapping three
environment variables. Five things in it are the design rather than the implementation:

- **The plan is flattened before it is sent.** A file or stdin feed applies every command at virtual
  time zero, so a `say` left inside `plan.load` would all be due in the first frame; the driver
  rewrites each `say` as `narrate` with an explicit `at`, accumulated from the beats' own nominal
  durations using the engine's `lag_ratio` arithmetic (`group` defaults to 0.0, `succession` to 1.0).
  The plan stays the model's product; only its timing is the driver's.
- **The engine is the validator.** Each candidate runs through `validate` (scratch state),
  then `batch` + `check` on a throwaway probe session, and `error`/`warning` findings are fed back to
  the model as a repair hint (at most `MAX_REPAIRS = 2`). `info` findings are pacing observations and
  are not sent back. This is what makes the linter useful to a model rather than only to a human: two
  real runs were repaired by an `unparseable` unknown variant and by
  `overlap（三角形）：boxes overlap by 100%…`, both caught before a frame was rendered.
- **Pre-flight and export are separate processes.** The frame count has to be on the export
  command line and is not known until the plan is flattened, so probes use a throwaway `--png` path
  and the real run is one `batch` on a fresh process — which also means the pre-flight tree is never
  the rendered one.
- **One content rule the engine cannot state, checked in the driver.** `validate` and `check` can
  say a picture is well formed and legible; neither can say that an explanation of Pythagoras has to
  *show the three squares the theorem is about*. `content_problem` counts the plan's
  `add`-`polygon` ops (the wire puts `type` beside `params` on the command, not inside it — reading
  `params["type"]` silently found none) and hands the model a rewrite hint below three. It fires
  only when the request names the theorem. This is the one place the example is more than a
  transport: a plan that draws only the triangle passes every engine check and teaches nothing.
- **Sound is the host's, captions are the engine's, and both are on by default.**
  `--captions` (the engine's own band, see "Narration, captions and self-voicing") and the audio
  mux are both defaults; `--no-captions` / `--no-speak` turn each half off. An export never speaks,
  so the driver synthesises the same `narrate` lines with `espeak-ng -v cmn -w … --stdin` (text on
  stdin, for the reason `rustim-app --speak` puts it there), lays the clips on a silent bed of the
  video's length with Python's `wave`/`array` — sequential, not mixed, because two sentences due at
  once are a plan talking over itself — and muxes with `ffmpeg -c:v copy -c:a aac -shortest`. The
  cue list comes from the flattened log, so the voice and the band cannot disagree about what was
  said. Neither tool present is a warning and a complete silent video, not a failure.
- **Absent from the engine.** Nothing in `crates/` changed to make this work. That is the point of
  `贾维斯集成方案.md`: the assistant is a client, and a second one — a different model, a different
  prompt, a different language — drives the same engine unchanged.

## The daemon: many sessions over a socket

`rustim-daemon` is the transport for an assistant that lives in another process and must survive its
own reconnect. It is a **host**, not a second engine: the same `SceneContext::handle`, the same serde
commands and replies, the same `SemanticTree`. New here are the envelope, the sessions and the event
push — everything a client sends is still a protocol command.

**Thread per session, because `SceneContext` is not `Send`.** `Box<dyn Scene>`, `Box<dyn Animation>`
and `Box<dyn Typesetter>` carry no `Send` bound, and adding one would change every implementor's
signature for a property only one host needs. So each session owns an OS thread that runs a
`current_thread` tokio runtime (`Runtime::block_on` has no `Send` bound). The network reactor spawns
the thread, hands it channels, and thereafter sees only `Arc<SessionHandle>` and plain data — an
`mpsc` of requests, a `broadcast` of events, a `watch` of the clock.

**`attach` waits for `construct`.** The session thread builds the scene, its `SceneContext`, and the
`DaemonTypesetter` (when enabled), calls `construct`, and only then signals a oneshot the caller is
awaiting. So the first command a client sends meets a constructed scene, and `attach` can return the
session id and its constraints without racing the setup. A scene whose `construct` is slow delays its
own attach and nobody else's.

**The clock is either automatic or manual, never both.** `auto` sessions tick `STEP = 1/60` with
`MissedTickBehavior::Skip` — a descheduled process must not replay sixty steps and make the state
depend on machine load — and the first, immediately-ready tick is consumed so clock 0 is the state
`construct` left. `manual` sessions move only on `advance`, which steps the scene on the same
`1/60` grid (the browser acceptance uses the identical grid) and caps a request at
`MAX_ADVANCE_SECONDS = 600`. `advance` on an auto session is a refusal, not a second clock.

**The watch is updated before the reply, not after.** `advance` answers with the clock it produced,
and the caller reads the watch the instant the oneshot resolves; reporting the clock on the way out
of `service` — after the reply — returned the value from *before* the call. The integration suite
caught it (`clock: 0` for a 0.5 s advance), which is why the `now` sender is passed into `service`
and the Advance arm writes it before `reply.send(())`.

**Events are notifications, and the subscription is session state.** `publish` drains the kernel's
event log unconditionally and broadcasts only what `ctx.wants_event(kind)` admits, so a session
nobody subscribed to still lets the kernel's bounded log drop rather than letting the cap do it. Each
connection owns a forwarder task that turns events into `{"jsonrpc":"2.0","method":"event",…}` frames
with no `id`; a `Lagged` receiver skips rather than stalls, because one slow client must not slow the
timeline. Because subscription lives on the session, every attached client sees the same stream —
that is what makes a display and an agent share a lecture — and events between two attaches are not
replayed: `query` is how a newcomer resyncs. The daemon emits no `frame_ready`, because it renders
nothing.

**The envelope is thin and the protocol underneath is untouched.** `rpc::parse` enforces JSON-RPC
2.0: string `method`, an object `params` (the array form is refused), and an `id` that is a number, a
string or absent/null. An action method is recognised against `rustim_scene::command::ACTIONS`, and
`command_text` re-inserts `"action": method` and hands the text to the **protocol's own parser** — the
daemon has no second dialect and no second validator. The result of a kernel refusal is a `result`
carrying `{"seq":n,"error":{…}}`, exactly as stdio would; JSON-RPC `error` is reserved for transport
failures (`-32700` parse, `-32600` invalid request, `-32601` method not found, `-32602` invalid
params, `-32603` internal, `-32001` not attached, `-32002` no such session, `-32003` not a manual
clock). A notification (no `id`) is applied and never answered, but it still consumes a protocol
`seq`.

**Replies and events share one writer, not one order.** A WebSocket sink cannot be written from two
places, so every outbound frame goes through a per-connection `mpsc` into one writer task. Frames are
ordered within that channel; a reply and the event the same command produced are *not* ordered
against each other, because the actor hands the outcome back and broadcasts on separate channels.
Clients correlate replies by JSON-RPC `id` and events by the event's `clock` — the stdio feed remains
the transport with a line order, and this one is explicitly not.

**Session policy.** `attach` without an id creates `s1`, `s2`, … (monotonic, never reused); with an id
it joins, which is reconnect. Joining an unknown id is `NO_SUCH_SESSION`, never a silent new session:
a client that thinks it rejoined must not be handed an empty stage. A rejoin may restate `scene`,
`clock`, `width`, `height` and `motion`; a disagreement is refused with both values named.
`detach` keeps the session running and only stops that connection's attachment and events; `close`
ends it for everyone, and another attached client's next use gets `NO_SUCH_SESSION` (its `ping` says
`alive: false` first). The binary's flags (`--listen`, `--scene`, `--clock`, `--no-typeset`) are
defaults for a newly created session, not policies — a client may choose another scene or clock in
its own `attach`.

**What it deliberately is not.** It does not render (no `frame_ready`), does not authenticate
(loopback binding is the boundary; exposing the port without an authenticating proxy lets anyone who
can reach it drive the engine), does not persist (the session dies with the process; the command log
is the artifact to replay), and does not emit audio (`narrate` queues lines on the session clock;
speaking them is the client's business). The typesetter is built lazily per session and caches a
failure, so `--no-typeset` is honest in `hello` and a missing font directory costs one report rather
than one per `add`.

## MCP: the same protocol behind tools

`rustim-mcp` is the fourth transport and the thinnest: an MCP client launches the binary, says
`initialize` / `tools/list` / `tools/call`, and each tool call becomes the protocol command a script
would have sent. It is a **host**, like `rustim-daemon`, and it re-uses the daemon's session
machinery instead of owning a second one — the crate depends on `rustim-daemon`'s *library*, embeds
`Hub`, and never opens a socket. That dependency is the design: an MCP tool call and a WebSocket
method end in one `SceneContext::handle`, so the two transports cannot drift apart in what they can
express.

**The wire is newline-delimited JSON-RPC 2.0 on stdio.** One message per line (JSON escapes newlines
inside strings, so the framing is total), stdout carries protocol only, and the one informational
line goes to stderr. Four methods are served — `initialize`, `ping`, `tools/list`, `tools/call` —
and anything else is `-32601`. A JSON-RPC **batch array** is `-32600`: MCP does not use batches, and
the protocol's own `batch` *action* is where a list of commands belongs, so accepting one would be a
second spelling of the same thing. `notifications/*` is executed if it is a request-shaped method
and never answered, even if a confused client attached an `id`.

**Version negotiation echoes what it knows.** `SUPPORTED_PROTOCOL_VERSIONS` is
`2024-11-05`, `2025-03-26`, `2025-06-18`, `2025-11-25`; a requested revision in that set is echoed,
and anything else is answered with `2025-11-25` (the newest implemented), which is the protocol's
rule — the client decides whether it can proceed. The method set is identical across the four, so no
payload depends on the revision.

**Five thick tools, and the command documents pass through untouched.**

| Tool | Command |
|---|---|
| `rustim_capabilities` | `{"action":"hello","protocol":1}` |
| `rustim_batch` | `{"action":"batch","commands":[…]}` |
| `rustim_validate` | `{"action":"validate","commands":[…]}` |
| `rustim_query` | `{"action":"query","target"?:…}` |
| `rustim_advance` | the daemon's manual-clock `advance` |

Nineteen primitive tools would make a real-time model worse at choosing, and every primitive is
already reachable inside `batch`. `tools::translate` checks only *shape* (a missing `commands`, a
`target` that is neither a non-negative integer nor a string) — whether `{"type":"hexagon"}` is
legal is the engine's answer, and a second validator here would drift from the first.

**Two failure vocabularies, and the split is MCP's own requirement.** A malformed request — unknown
tool, bad argument shape, a line that is not JSON — is a JSON-RPC `error` (`-32700` parse, `-32600`
invalid request, `-32601` method, `-32602` params, `-32603` when the session ended) and executes
nothing. An **engine refusal** is a *successful* `tools/call` result with `isError: true` whose body
is the protocol's refusal document (`in_batch` and its `cause` intact), carried twice: as text
(`content[0].text`) and as `structuredContent` when the document is an object. That is not a style
choice — MCP requires tool-level errors to be results precisely so the model sees the refusal and can
correct itself; burying a `no_such_label` in a transport error hides the one thing it needed.

**One session per process, created at startup.** An MCP server lives as long as its client's pipe,
so there is no `attach` tool: the process *is* the attach. The session comes from `Hub::attach`
exactly as the daemon's does — same clock, same `DaemonTypesetter`, same `--no-typeset` honesty —
and the binary's flags (`--scene`, `--clock`, `--motion`, `--width`, `--height`, `--no-typeset`)
are that session's properties.

**The blocking bridge is the `Session` trait.** `SceneContext` is not `Send` and `SessionHandle` is
async, so the envelope knows only `command(Command)` and `advance(f64)`; the binary implements them
over a current-thread runtime plus `block_on`, and the unit tests implement them over a bare
`SceneContext` on the test thread. `advance` reads `clock_now()` *after* the oneshot resolves,
which is correct because the daemon writes the watch before it sends the reply (see "The daemon").

**What it deliberately is not.** It does not forward events (`beat_started` / `animation_done`
belong to daemon subscribers), does not render (`rustim_query`'s semantic tree is what an AI needs
to plan; a picture is a display host's job, and the daemon's own crate docs make the same
distinction), keeps no model state, and speaks stdio only — streamable HTTP is a deployment question
for a later step.

**Evidence.** Twelve unit tests in `src/mcp.rs` (handshake and catalogue, batch→query through a real
`SceneContext`, a refusal as a tool error that leaves the session usable, validate-then-apply, four
invalid-argument paths, the envelope's batch/bad-JSON/unknown-method answers, notifications silent,
advance's clock), five in `src/tools.rs` (catalogue shape, untouched commands, argument messages,
target spelling, unknown-tool candidates), and eleven in `tests/mcp.rs` over the real binary:
handshake and version negotiation, the engine's own capability profile, batch + query with the text
half equal to `structuredContent`, refusal-then-ping, validate not changing the tree, manual vs auto
clock, argument errors touching nothing, notifications and blank lines unanswered, CJK text and a
Typst formula typeset through the pipe (with MathML reaching `query`), `--no-typeset` refusing text
with `unsupported`, and `--scene pythagorean` holding the four named figures. The count moved
780 → 803 and **all eight goldens passed unchanged** — a transport draws nothing.

## The feed: a session is a file of commands

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
  `tree`, `report`, `plan`, `capabilities` and `error` is present**, and the presence is the
  discriminant. `created` is the only way to learn the id of what was just created; a `play` gets a
  present-but-**empty** diff, because queueing a beat changes nothing in the tree; a `query` gets a
  `tree` and no diff at all, because an empty diff would read as a mutation that did nothing — and a
  **`session`** beside that tree, which is *not* a seventh presence: see below; a
  `check` gets a `report` — the empty-findings array is itself the clean answer, so “checked and
  clean” is distinguishable from “did not ask”; a `plan.status` gets a `plan`, and `loaded: false`
  is a legal answer for the same reason; a `hello` gets `capabilities` and none of the other five,
  because a handshake is not a read of the tree.
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
  golden tolerance). The feed is orthogonal to the scene: `--scene rotate` with a script that
  contains nothing at all is byte-identical to `--scene rotate` — the tests spell that as a real
  empty file rather than `/dev/null`, which Unix has and Windows does not.
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
