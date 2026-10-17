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
//! Above `rustim-mobject` and `rustim-anim`, below `rustim-scene`, and below
//! every host:
//!
//! ```text
//! hosts (rustim-app / rustim-daemon / rustim-mcp / rustim-web)
//!   → rustim-scene   (the protocol: commands, replies, semantics)
//!   → rustim-director (layout, annotation, attention, linter)
//!   → rustim-anim / rustim-mobject / rustim-math
//! ```
//!
//! `rustim-scene` depends on this crate because the layout functions are
//! **pure tree geometry** — they read `SceneTree` and write transforms, name no
//! font and open no file — so the three wire actions (`next_to`, `arrange`,
//! `fit`) call straight into them from [`apply`](rustim_scene::command::apply)
//! rather than going through a host-installed trait. A trait would have been a
//! seam with nothing behind it: there is only one implementation, and an
//! uninstalled one would make the actions refuse in a build that clearly has
//! the geometry to run them.
//!
//! The dependency is also what keeps the guard honest, not what threatens it:
//! this crate depends only on `rustim-math` and `rustim-mobject`, so it is
//! wasm-clean, and `rustim-scene` is in the six-crate wasm guard. **Anything
//! added here that needs `rustim-text` (a template that typesets) must go in a
//! crate above `rustim-scene` instead** — Typst must never reach the kernel.
//!
//! Nothing here does I/O, opens a font, or names a model. The functions take a
//! [`SceneTree`](rustim_mobject::SceneTree) and node ids and either mutate
//! transforms or read the tree; a host decides when to call them. That is what
//! lets the same layout be a protocol action, an MCP tool and a Rust API
//! without three implementations.
//!
//! # What is here now
//!
//! [`layout`]: `next_to`, `arrange` and `fit`, measured from each node's
//! **world-space bounding box** — the same materialised anchors the
//! accessibility tree measures, so a rotated square is placed by the box a
//! viewer sees rather than by its untransformed side. Placement is a
//! translation composed onto the node's deferred transform (never a rewritten
//! point array), and every refusal is a value: an unmeasurable node, a
//! collapsed parent transform, a box with no extent. The same three functions
//! are the wire's `next_to` / `arrange` / `fit` actions; the protocol's
//! spelling and refusal mapping live in `rustim-scene`.
//!
//! [`attention`]: the geometry of *point at this* and *dim the rest* —
//! [`pointing_arrow`](attention::pointing_arrow) builds an arrow whose tip sits
//! a gap from the subject's world box, and [`spotlight`](attention::spotlight)
//! names the picture-bearing nodes outside the subject's subtree. They are the
//! wire's `point_at` / `highlight` actions; the dim-and-restore beat itself is
//! composed in `rustim-scene`, because this crate does not depend on
//! `rustim-anim`.
//!
//! Deferred, deliberately, and named so they are not rediscovered as gaps:
//! annotation (labels that follow their subject), the six `explain.*`
//! templates, the linter (out of frame, overlap, size, contrast, pacing) and
//! the declarative `ScenePlan`.
//!
//! `rustim-text` is not a dependency and will not become one without a separate
//! crate: see this module's note on the wasm kernel.

pub mod attention;
pub mod layout;

pub use attention::{DEFAULT_POINT_AT_BUFF, DEFAULT_POINT_AT_LENGTH, pointing_arrow, spotlight};
pub use layout::{Axis, Direction, LayoutError, arrange, fit, next_to};
