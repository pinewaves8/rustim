//! The teaching tier: what a lesson needs above the primitive command set.
//!
//! `rustim-scene` speaks in nodes, properties and animations — the vocabulary a
//! competent planner model can use, and the vocabulary every transport carries.
//! A real-time model, or a template, does not want to compute where a caption
//! goes: it wants to say *next to that square*, *in a row*, *fitted to the
//! frame*, *point at this*. That sentence is the teaching tier, and this crate
//! is where it is computed.
//!
//! # Where this sits
//!
//! Above `rustim-mobject` and `rustim-anim`, beside `rustim-scene`, and below
//! every host:
//!
//! ```text
//! hosts (rustim-app / rustim-daemon / rustim-mcp / rustim-web)
//!   → rustim-scene   (the protocol: commands, replies, semantics)
//!   → rustim-director (layout, annotation, attention, templates, linter)
//!   → rustim-anim / rustim-mobject / rustim-math
//! ```
//!
//! Nothing here does I/O, opens a font, or names a model. The functions take a
//! [`SceneTree`](rustim_mobject::SceneTree) and node ids and either mutate
//! transforms or read the tree; a host decides when to call them. That is what
//! lets the same layout be a protocol action, an MCP tool and a Rust API
//! without three implementations — the seam is deliberate, because the tier
//! will grow to compile formulas (`rustim-text`) and that must never reach the
//! six wasm kernel crates.
//!
//! # What is here now
//!
//! [`layout`]: `next_to`, `arrange` and `fit`, measured from each node's
//! **world-space bounding box** — the same materialised anchors the
//! accessibility tree measures, so a rotated square is placed by the box a
//! viewer sees rather than by its untransformed side. Placement is a
//! translation composed onto the node's deferred transform (never a rewritten
//! point array), and every refusal is a value: an unmeasurable node, a
//! collapsed parent transform, a box with no extent.
//!
//! Deferred, deliberately, and named so they are not rediscovered as gaps:
//! annotation (labels that follow their subject), attention animations
//! (`highlight`, `point_at`), the six `explain.*` templates, the linter
//! (`out of frame`, overlap, size, contrast, pacing) and the declarative
//! `ScenePlan`.
//!
//! `rustim-text` is not a dependency and will not become one without a separate
//! crate: see this module's note on the wasm kernel.

pub mod layout;

pub use layout::{Axis, Direction, LayoutError, arrange, fit, next_to};
