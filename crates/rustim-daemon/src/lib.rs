//! The daemon: rustim as a long-running service a remote assistant can drive.
//!
//! # Where this sits
//!
//! `rustim-app` is a process a person runs, and its stdio feed is the protocol
//! over a pipe: one scene, one client, one lifetime. That is the wrong shape for
//! an assistant that lives behind a voice UI — it needs a service that is already
//! running, that survives its own reconnect, that lets a display and an agent
//! watch the same lecture, and that pushes events instead of being polled. This
//! crate is that host, and it is a **host**, like `rustim-app` and `rustim-web`:
//! the same kernel, the same `SceneContext::handle`, the same serde commands and
//! replies (see [`rpc`] for the envelope and [`session`] for the ownership
//! model). Nothing about the engine changes; only who is on the other end.
//!
//! # The shape of the protocol
//!
//! A client opens a WebSocket, sends
//!
//! ```json
//! {"jsonrpc":"2.0","id":1,"method":"attach","params":{"scene":"blank","clock":"manual"}}
//! ```
//!
//! and receives `{"jsonrpc":"2.0","id":1,"result":{"session":"s1","created":true,…}}`.
//! Every protocol action is then a method — `{"jsonrpc":"2.0","id":2,"method":"add","params":{"type":"circle"}}`
//! — and the result is the protocol's own reply document, `{"seq":1,"diff":{…}}`,
//! unchanged. An engine refusal is a *result*: `{"seq":2,"error":{"kind":"no_such_node",…}}`.
//! JSON-RPC `error` is reserved for transport failures: malformed JSON, an
//! unknown method, not being attached. A client that has read the stdio protocol
//! already knows every payload; only the framing and the four session methods
//! are new.
//!
//! # Sessions
//!
//! `attach` with no `session` creates one; with an id it joins an existing one.
//! That is reconnect: the socket may drop, the session's thread keeps its clock
//! (or its pause), and a client that reattaches to `s1` continues the lecture
//! rather than starting over. Several clients may hold the same session — one
//! driving, one displaying — and each gets the event stream. An id that is not
//! live is a refusal (`NO_SUCH_SESSION`), never a silent new session: a client
//! that thinks it rejoined must not be handed an empty stage.
//!
//! `detach` leaves the session running and stops this connection's events;
//! `close` stops the session for everyone. Session ids are never reused, so a
//! stale id is always a stale id.
//!
//! # What the daemon is not
//!
//! * **It does not render.** It runs no rasteriser and sets no typesetter beyond
//!   text and formulas, so it emits no `frame_ready` events. A UI attaches to
//!   the same session and draws the scene itself (the browser host does; the
//!   desktop's `--script` path is its own transport). Adding a frame event that
//!   no frame was produced for would be a heartbeat lying about an artifact.
//! * **It does not authenticate.** Binding to loopback is the security boundary
//!   a deployment gets, and the flags make that the default. Exposing it on a
//!   network without a proxy that authenticates would let anyone who can reach
//!   the port drive the engine.
//! * **It does not persist.** Close the process and the sessions are gone. A
//!   lesson is a command log a client holds; replay it into a fresh session.
//! * **It does not schedule narration as audio.** `narrate` is a protocol
//!   command and the lines are queued on the session's clock; whether anyone
//!   speaks them is the client's business (the desktop's `--speak` is the
//!   reference implementation).
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() -> std::io::Result<()> {
//!     let hub = Arc::new(rustim_daemon::Hub::new(Default::default()).expect("defaults"));
//!     let listener = tokio::net::TcpListener::bind("127.0.0.1:7300").await?;
//!     rustim_daemon::serve(listener, hub).await
//! }
//! ```

pub mod rpc;
mod server;
pub mod session;
mod typeset;

pub use server::{serve, serve_connection};
pub use session::{
    AdvanceError, AttachError, AttachSpec, ClockMode, CommandFailure, DEFAULT_HEIGHT,
    DEFAULT_WIDTH, Hub, HubOptions, MAX_ADVANCE_SECONDS, STEP, SessionHandle, scene_names,
};
