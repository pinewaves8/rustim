//! The DOM/ARIA projection of a [`SemanticTree`].
//!
//! There is no `accesskit_web`, so the browser cannot consume the tree the
//! desktop adapter builds. It does not need to: both platforms want the same
//! *semantic data*, and `rustim_scene::a11y` is already the platform-neutral
//! form of it. This module is the one conversion a browser host adds — role
//! names via [`SemanticRole::aria_role`], ids as plain integers, and the
//! children list preserved so the DOM layer can nest rather than flatten.
//!
//! It is a pure function on purpose. The accessibility tree a reader sees should
//! be a function of the scene, not of which host rendered it, and keeping the
//! mapping testable natively is what lets the browser's claims be checked from
//! `cargo test` as well as from Chrome.
//!
//! # MathML is the one place Web is *richer*
//!
//! The native semantic tree carries a formula's structure as a `mathml` string
//! beside the spoken name and description (P3 step 20). On the desktop that
//! string is something a host may inspect; in a browser it becomes a real
//! `<math>` element, which a MathML-aware reader walks as structure rather than
//! hearing as one sentence. The projection keeps the field untouched — the
//! decision about how to render it belongs to the DOM layer, and the check that
//! it survived belongs here.

use rustim_math::Rect;
use rustim_scene::a11y::SemanticTree;

/// One node, in the shape a DOM builder needs.
///
/// `id` is the wire integer (`NodeId::raw`) rather than the generational
/// handle, because it is used as an element id and compared against command
/// targets a host may have sent as JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct AriaNode {
    pub id: u64,
    pub role: &'static str,
    pub name: String,
    pub description: String,
    /// Screen pixels, Y down, origin at the canvas's top-left.
    pub bounds: Option<Rect>,
    pub hidden: bool,
    pub mathml: Option<String>,
    pub children: Vec<u64>,
}

/// Project the semantic tree into flat ARIA nodes, root first, in tree order.
///
/// Order is preserved rather than rebuilt from `children`: the semantic tree is
/// already in draw order with the root at index zero, and re-sorting it here
/// would give the two projections (AccessKit and DOM/ARIA) different orders for
/// the same scene.
pub fn project(tree: &SemanticTree) -> Vec<AriaNode> {
    tree.nodes
        .iter()
        .map(|n| AriaNode {
            id: n.id.raw(),
            role: n.role.aria_role(),
            name: n.name.clone(),
            description: n.description.clone(),
            bounds: n.bounds,
            hidden: n.hidden,
            mathml: n.mathml.clone(),
            children: n.children.iter().map(|c| c.raw()).collect(),
        })
        .collect()
}

/// Find one node by its wire id.
///
/// Small and linear on purpose: a scene at this scale has tens of nodes, and a
/// map would be a second structure to keep in step with the projection.
pub fn get(nodes: &[AriaNode], id: u64) -> Option<&AriaNode> {
    nodes.iter().find(|n| n.id == id)
}
