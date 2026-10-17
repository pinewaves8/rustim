//! The Model Context Protocol adapter: rustim's command protocol, spoken to
//! tools that only know MCP.
//!
//! # Where this sits
//!
//! rustim's driver surface is a JSON command protocol (`驱动协议v1.md`). It has
//! three transports already: JSONL over stdio (`--script`/`--reply`), JSON-RPC
//! over a WebSocket (`rustim-daemon`), and the in-process bridge in the browser.
//! This crate is the fourth, and the difference is only framing: an MCP client
//! sends `initialize`, `tools/list` and `tools/call` instead of command lines,
//! and a tool call becomes the same command the other transports send.
//!
//! The dependency is deliberate. MCP is a session-shaped protocol (the client
//! initialises once, keeps a stdio pipe open, and calls tools for as long as it
//! likes), so the session machinery has to be process-lifetime rather than
//! line-shaped. `rustim-daemon`'s `Hub` is exactly that machinery with its
//! socket removed: a `SceneContext` on its own thread, a request queue, a clock
//! and the typesetter. This crate embeds it, and therefore inherits every
//! guarantee the daemon has — manual and auto clocks, text/formula typesetting,
//! the `hello` capability filter — without restating one of them.
//!
//! # The tool surface is a shape, not a second vocabulary
//!
//! Five tools, because a model picks better between a handful of verbs than
//! between nineteen, and every protocol primitive is already reachable through
//! the protocol's own `batch`:
//!
//! | Tool | Maps to |
//! |---|---|
//! | `rustim_capabilities` | `{"action":"hello","protocol":1}` |
//! | `rustim_batch` | `{"action":"batch","commands":[…]}` |
//! | `rustim_validate` | `{"action":"validate","commands":[…]}` |
//! | `rustim_query` | `{"action":"query","target"?:…}` |
//! | `rustim_advance` | the daemon's manual-clock `advance` |
//!
//! Nothing here adds a capability the stdio feed lacks, and the command
//! documents are passed through untouched: a `batch` member is a protocol v1
//! command, not an MCP-shaped rewrite of one.
//!
//! # Two failure vocabularies, and which is which
//!
//! * **A wrong request** — unknown tool, malformed arguments, a message that is
//!   not JSON — is a JSON-RPC `error`. The client sent something the adapter
//!   could not even hand to the engine.
//! * **An engine refusal** — an unknown label, a value out of range, an
//!   animation naming a deleted node — is a *successful* `tools/call` whose
//!   result has `isError: true` and carries the protocol's own refusal document.
//!   MCP requires this shape for tool-level errors precisely so the model sees
//!   the refusal and can correct itself; burying it in a transport error hides
//!   the one thing it needs to act on.
//!
//! # What it deliberately is not
//!
//! * **It does not render.** Like the daemon, it owns no rasteriser, so there is
//!   no snapshot tool. A picture is a display host's job: the desktop window,
//!   the browser page, or a client attached to a `rustim-daemon` session. The
//!   semantic tree from `rustim_query` is what an AI needs in order to *plan*.
//! * **It does not forward events.** MCP is request/response here; the
//!   `beat_started` / `animation_done` stream belongs to the daemon's
//!   subscribers. A tool call that plays a beat returns as soon as the beat is
//!   queued, and the lecture continues on the session's clock.
//! * **It keeps no model state.** No prompts, no history, no vendor SDK — the
//!   engine/assistant boundary is the same one `贾维斯集成方案.md` states.
//! * **It is stdio only.** Streamable HTTP (the modern MCP remote transport) is
//!   a deployment question for a later step; a local client launches the binary.

pub mod mcp;
pub mod tools;

pub use mcp::{
    AdvanceError, LATEST_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS, Server, Session,
    SessionError,
};
