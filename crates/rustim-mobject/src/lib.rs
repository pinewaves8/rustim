//! The `Mobject` data model: points, style, deferred transform, scene tree.
//!
//! # Status: Phase 1
//!
//! Only the storage layout is settled here; `rustim-mobject` is where the
//! deferred-transform design from `方案建议.md` §4.2 lands. Two structural
//! commitments are already made and are visible in the type below:
//!
//! 1. **Every mobject carries a stable [`NodeId`].** Manim identifies mobjects
//!    by object reference; we need an addressable id because the same object has
//!    to be named by the AI command layer (§4.5), by the accessibility tree
//!    (§4.6), and by the serialiser. `slotmap` is the storage primitive — Bevy's
//!    ECS is explicitly rejected (`方案建议.md` §7.5) because recycled entity ids
//!    are the wrong primitive for commands that name a node across frames.
//!
//! 2. **Transforms are deferred, not baked.** Manim writes a transform into the
//!    point array of every descendant, so moving a `VGroup` costs O(points).
//!    Here `transform` is an O(1) field and `materialize` is the explicit,
//!    cacheable step that produces final coordinates.

pub mod kind;
pub mod shapes;
pub mod style;
pub mod text;
pub mod tree;

pub use kind::NodeKind;
pub use shapes::{ShapeKind, ShapeParams, build};
pub use style::{DirtyFlags, Style};
pub use text::{TextBlock, TextRun};
pub use tree::{Mobject, NodeId, SceneTree, Subtree};
