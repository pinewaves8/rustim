//! Scene graph, stable ids, the serde command layer, and the accessibility map.
//!
//! # Status: Phase 1
//!
//! The command layer (§4.5) and the platform-neutral half of the accessibility
//! tree (§4.6, [`a11y`]) are here. The platform layers are not: `accesskit` is
//! named only in `rustim-app`, because this crate has to keep compiling to
//! `wasm32-unknown-unknown` and the Web target goes through DOM/ARIA rather than
//! AccessKit. What crosses that boundary is [`a11y::SemanticTree`] — plain data.
//!
//! # The command layer is data, not Rust
//!
//! `需求探讨.txt` is explicit that the AI should *drive* the engine rather than
//! write it. So the boundary is a serialisable command value, not generated Rust
//! source and not an ECS query language. Every command names a node by its
//! stable [`NodeId`] and a property by name, which is why
//! [`rustim_mobject::Mobject`] carries an id, a label and named fields rather
//! than being addressed by reference.
//!
//! Commands are internally tagged so that the JSON form is the same shape the
//! tutorial sketches:
//!
//! ```
//! # use rustim_scene::Command;
//! # use rustim_mobject::NodeId;
//! // Ids are opaque 64-bit handles — slot index in the low half, generation in
//! // the high half — rather than small indices, so a stale id cannot address
//! // whatever inherited the slot.
//! let id = NodeId::from_raw(0x0000_0001_0000_0003);
//! let json = format!(r#"{{"action":"remove","target":{}}}"#, id.raw());
//! let cmd: Command = serde_json::from_str(&json).unwrap();
//! assert_eq!(serde_json::to_string(&cmd).unwrap(), json);
//! ```
//!
//! Note the round-trip property the test above pins: commands must serialise
//! back to what they parsed from, so a host can log, replay and diff them. That
//! is also why absent shape parameters are omitted rather than written as
//! `null`.
//!
//! [`CommandReply`] is the other half of the same wire — one per command, back
//! to whoever sent it. It lives here rather than in the application because it
//! is protocol, not transport: the same reply describes a command a browser
//! build delivers and one an editor's process does. What does *not* live here
//! is the reading and writing of a stream: this crate has no I/O and no JSON
//! parser, by the same rule that keeps `accesskit` out of it.

pub mod a11y;
pub mod command;
pub mod context;
pub mod event;
pub mod text;
pub mod typeset;

pub use a11y::{CameraView, SemanticNode, SemanticRole, SemanticTree, SemanticValue};
pub use command::{Command, CommandError, CommandOutcome, CommandReply, StateDiff, Target};
pub use context::{Scene, SceneContext};
pub use event::{Event, EventKind, EventLine, MAX_EVENTS, event_line};
pub use typeset::{TextSpec, TypesetError, TypesetFormula, Typesetter};

/// Re-exported so scene authors do not have to depend on the mobject crate
/// directly for the common case.
pub use rustim_mobject::{Mobject, NodeId, SceneTree, TextBlock};
