//! Morphing one subtree into another — Manim's `Transform` across a family.
//!
//! [`Transform`](crate::Transform) morphs a node's *own* point array against a
//! donor `Mobject` and stops there. That is enough for every shape builder in
//! `rustim-mobject`, and it is not enough for a group: a group has no points of
//! its own, it has children, and the thing a viewer calls "the morph" is the
//! pairwise morph of those children. This module is that recursion.
//!
//! # The rules, from `Mobject.align_data`
//!
//! Manim's `align_data` (`mobject/mobject.py:3107-3142`) does four things, and
//! this port keeps three of them:
//!
//! 1. **`align_submobjects` pads child counts** (`:3177-3184`). The shorter side
//!    gains copies of its own children, each faded to transparent by
//!    `add_n_more_submobjects`'s `.copy().fade(1)` (`:3209-3232`). The copies are
//!    **interleaved proportionally**, not appended: for two children becoming
//!    five, the split factors are `[3, 2]`, so the order is
//!    `[c0, c0', c0'', c1, c1']`. That is why the order is computed here with
//!    Manim's exact `(j · n) // m` arithmetic rather than by repeating the list.
//! 2. **The pairs are then aligned recursively by index** (`:3141-3142`), each
//!    pair running its own `align_data` — including its own padding.
//! 3. **`align_points` reconciles each pair's own point arrays** (`:883-909` in
//!    `rustim-math`), which is a no-op for groups (both arrays empty) and the
//!    whole story for leaves.
//!
//! # Where the mutation happens, and why it is not in `begin`
//!
//! Manim aligns *in place* inside `begin`: the copies become real submobjects of
//! the source before the first frame. Here `Animation::begin` is handed
//! `&SceneTree`, deliberately — an animation that starts must not restructure the
//! scene. So `begin` builds an immutable **plan** and the first `interpolate`
//! materialises it: the padded copies are inserted (with fresh [`NodeId`]s), the
//! parent's child order is put back into Manim's interleaved order, and every
//! pair in the whole subtree becomes one flat list of interpolation frames.
//! Nothing observes the difference: `interpolate(0)` is the first frame either
//! way, and reduced motion drives the same path once.
//!
//! **The copies stay in the tree afterwards.** That is Manim's outcome too —
//! `align_submobject_count` mutates `self.submobjects`, so a three-child group
//! morphed into a two-child one ends with three children, the third transparent.
//! Removing them at `finish` would be tidier and would be a different animation;
//! the transparent leftover is what a later `Transform` on the same group aligns
//! against.
//!
//! # Not ported
//!
//! * **`null_point_align`** (`:3186-3207`), which wraps a mobject that has points
//!   and no children into a one-child group when the other side is a group. So a
//!   **shape ↔ group** morph is refused (an engine no-op; the command layer
//!   reports it) rather than approximated. Group ↔ group and shape ↔ shape are
//!   the two cases this implements.
//! * **Text and formulas as donors of glyph geometry.** A text mobject's picture
//!   is pushed by the scene for its id, not stored on the mobject, so a copied
//!   text node has nothing to copy. A formula's glyphs are ordinary child nodes
//!   and do morph; only the text case is out.
//!
//! # What the source keeps
//!
//! Exactly what [`Transform`](crate::Transform) keeps: its id, its kind and its
//! label. The points, transform, opacity and style of every node in the family
//! are interpolated toward the donor's, and the donor is never inserted, so the
//! target stays where it was — a plain `Transform` is neither an introducer nor a
//! remover.

use rustim_math::{Affine2, Point, bezier};
use rustim_mobject::{NodeId, SceneTree, Style, Subtree};

use crate::Animation;
use crate::ops::{lerp_affine, lerp_scalar, path_point};

/// Morphs the subtree at `source` into the subtree at `target`.
///
/// `target` is a **live scene node**, read once at `begin` into a
/// [`Subtree`] snapshot and never mutated, so the donor can remain in the scene.
/// Its visibility is the caller's business, exactly as Manim's `Transform(a, b)`
/// leaves `b` where it is.
///
/// See the module doc for the alignment rules, for what is deferred to the first
/// frame, and for the two morphs that are refused.
#[derive(Debug, Clone)]
pub struct TransformGroup {
    source: NodeId,
    target: NodeId,
    plan: Option<NodePlan>,
    running: Option<Vec<Frame>>,
    duration: f64,
    /// Manim's `path_arc` on the `Transform` that drives the family: every
    /// paired point takes the same arc, which is why it belongs to the animation
    /// rather than to each frame.
    path_arc: Option<f64>,
}

/// One alignment, computed once in `begin`, applied every frame.
///
/// The same six pairs of values [`crate::Transform`] captures for a single node;
/// a family is just many of them, flattened. The panels are stored *already
/// aligned*, so the per-frame cost is one zip per node and nothing else.
#[derive(Debug, Clone)]
struct Frame {
    id: NodeId,
    from_points: Vec<Point>,
    to_points: Vec<Point>,
    from_transform: Affine2,
    to_transform: Affine2,
    from_opacity: f64,
    to_opacity: f64,
    from_style: Style,
    to_style: Style,
}

/// Where a node in the plan comes from.
#[derive(Debug, Clone)]
enum FromNode {
    /// A node that already exists and will be written through.
    Live(NodeId),
    /// A padded copy, inserted on the first frame with fresh ids.
    Fresh(Box<Subtree>),
}

/// The immutable result of `begin`'s alignment.
#[derive(Debug, Clone)]
struct NodePlan {
    from: FromNode,
    from_points: Vec<Point>,
    to_points: Vec<Point>,
    from_transform: Affine2,
    to_transform: Affine2,
    from_opacity: f64,
    to_opacity: f64,
    from_style: Style,
    to_style: Style,
    children: Vec<NodePlan>,
}

impl TransformGroup {
    pub fn new(source: NodeId, target: NodeId) -> Self {
        Self {
            source,
            target,
            plan: None,
            running: None,
            duration: 1.0,
            path_arc: None,
        }
    }

    pub fn with_duration(mut self, seconds: f64) -> Self {
        self.duration = seconds;
        self
    }

    /// Route every paired point along an arc of `radians` — Manim's
    /// `Transform(path_arc=…)` applied to the whole family.
    ///
    /// Each child's route is measured in its **parent's** space, so a child that
    /// moves only through its deferred transform still bends: the arc is taken
    /// between `from_transform · a` and `to_transform · b`, and the child's own
    /// transform becomes identity for the morph. See
    /// [`Transform::with_path_arc`](crate::Transform::with_path_arc) for why that
    /// folding is necessary and what it costs. As there, the endpoints are
    /// written rather than computed.
    pub fn with_path_arc(mut self, radians: f64) -> Self {
        self.path_arc = Some(radians);
        self
    }

    /// Build the plan: align the source against a snapshot of the target.
    ///
    /// Every early return leaves `plan` unset, which turns the whole animation
    /// into a no-op. `begin` has no error channel — the same posture a command
    /// naming a deleted node has — and the command layer refuses what it can see
    /// in advance.
    fn plan(&mut self, tree: &SceneTree) {
        self.plan = None;
        self.running = None;
        if self.source == self.target
            || tree.is_ancestor_of(self.target, self.source)
            || tree.is_ancestor_of(self.source, self.target)
        {
            // A node cannot be morphed into itself or into its own parent: the
            // alignment would pair a node with one of its own descendants, and the
            // per-frame writes would then race each other.
            return;
        }
        let Some(donor) = tree.snapshot(self.target) else {
            return;
        };
        self.plan = plan_node(tree, FromNode::Live(self.source), donor);
    }
}

impl Animation for TransformGroup {
    fn target(&self) -> Option<NodeId> {
        Some(self.source)
    }

    fn duration(&self) -> f64 {
        self.duration
    }

    fn begin(&mut self, tree: &SceneTree) {
        self.plan(tree);
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        if self.running.is_none() {
            let Some(plan) = &self.plan else { return };
            let FromNode::Live(root) = &plan.from else {
                // The root is always the live source; a plan that says otherwise
                // could not have been built.
                return;
            };
            let root = *root;
            let mut frames = Vec::new();
            let mut order = Vec::new();
            for (i, child) in plan.children.iter().enumerate() {
                if let Some(id) = materialise(child, tree, root, i, &mut frames) {
                    order.push(id);
                }
            }
            // Manim's padded order is interleaved; insertion has to append, so
            // put the parent's children back into the plan's order. Without this
            // the copies land on top of every sibling they overlap.
            tree.reorder_children(root, &order);
            frames.push(frame_of(plan, root));
            self.running = Some(frames);
        }

        let Some(frames) = &self.running else { return };
        for frame in frames {
            let Some(m) = tree.get_mut(frame.id) else {
                continue;
            };
            m.points = match self.path_arc {
                Some(arc) => frame
                    .from_points
                    .iter()
                    .zip(frame.to_points.iter())
                    .map(|(a, b)| {
                        path_point(arc, frame.from_transform, *a, frame.to_transform, *b, alpha)
                    })
                    .collect(),
                None => frame
                    .from_points
                    .iter()
                    .zip(frame.to_points.iter())
                    .map(|(a, b)| {
                        Point::new(lerp_scalar(a.x, b.x, alpha), lerp_scalar(a.y, b.y, alpha))
                    })
                    .collect(),
            };
            m.set_transform(match self.path_arc {
                Some(_) => Affine2::IDENTITY,
                None => lerp_affine(frame.from_transform, frame.to_transform, alpha),
            });
            m.opacity = lerp_scalar(frame.from_opacity, frame.to_opacity, alpha);
            m.style = Style::lerp(frame.from_style, frame.to_style, alpha);
            m.dirty.style = true;
        }
    }
}

/// A frame for a node whose alignment is already in the plan.
fn frame_of(plan: &NodePlan, id: NodeId) -> Frame {
    Frame {
        id,
        from_points: plan.from_points.clone(),
        to_points: plan.to_points.clone(),
        from_transform: plan.from_transform,
        to_transform: plan.to_transform,
        from_opacity: plan.from_opacity,
        to_opacity: plan.to_opacity,
        from_style: plan.from_style,
        to_style: plan.to_style,
    }
}

/// Insert whatever `plan`'s node needs, recurse, and return the node's id.
///
/// `index` is the node's position in its parent's padded child order; it is only
/// consulted for a fresh node, because an existing node keeps the position it
/// already has (the caller reorders the parent afterwards).
fn materialise(
    plan: &NodePlan,
    tree: &mut SceneTree,
    parent: NodeId,
    index: usize,
    frames: &mut Vec<Frame>,
) -> Option<NodeId> {
    let (id, fresh) = match &plan.from {
        FromNode::Live(id) => (*id, false),
        FromNode::Fresh(subtree) => (tree.insert_subtree(parent, index, subtree)?, true),
    };
    if frames.iter().any(|f: &Frame| f.id == id) {
        // A plan cannot pair one source node twice, but a malformed tree could;
        // two frames writing the same node would make the result frame-order
        // dependent.
        return None;
    }
    let mut order = Vec::new();
    for (i, child) in plan.children.iter().enumerate() {
        if let Some(cid) = materialise(child, tree, id, i, frames) {
            order.push(cid);
        }
    }
    if fresh || !order.is_empty() {
        // A fresh node's children were inserted in plan order already; reordering
        // is harmless and keeps one path. An existing node's copies were appended,
        // so this is where Manim's interleaved order is restored.
        tree.reorder_children(id, &order);
    }
    frames.push(frame_of(plan, id));
    Some(id)
}

/// Align one pair and, recursively, their padded children.
fn plan_node(tree: &SceneTree, from: FromNode, to: Subtree) -> Option<NodePlan> {
    // Read everything off the borrowed side before `from` is moved into the plan
    // at the end: the borrow is either the tree's or the `Fresh` subtree's.
    let (from_points, to_points, from_transform, from_opacity, from_style, n_from) = {
        let from_m = match &from {
            FromNode::Live(id) => tree.get(*id)?,
            FromNode::Fresh(subtree) => &subtree.mobject,
        };
        let (from_points, to_points) =
            bezier::align_points(&from_m.points, &to.mobject.points).ok()?;
        let n_from = match &from {
            FromNode::Live(id) => tree.get(*id)?.children.len(),
            FromNode::Fresh(subtree) => subtree.children.len(),
        };
        (
            from_points,
            to_points,
            from_m.transform,
            from_m.opacity,
            from_m.style,
            n_from,
        )
    };
    let n_to = to.children.len();
    // `null_point_align` is not ported: a shape on one side and a group on the
    // other has no honest pairing here (see the module doc) and becomes a no-op
    // rather than a wrong picture.
    if (n_from == 0) != (n_to == 0) {
        return None;
    }
    let width = n_from.max(n_to);
    let mut children = Vec::with_capacity(width);
    for (fi, ti) in padded_indices(n_from, width)
        .into_iter()
        .zip(padded_indices(n_to, width))
    {
        let from_child = match &from {
            FromNode::Live(id) => {
                let child = *tree.get(*id)?.children.get(fi.0)?;
                if fi.1 {
                    let mut copy = tree.snapshot(child)?;
                    copy.fade();
                    FromNode::Fresh(Box::new(copy))
                } else {
                    FromNode::Live(child)
                }
            }
            FromNode::Fresh(subtree) => {
                let child = subtree.children.get(fi.0)?.clone();
                FromNode::Fresh(Box::new(if fi.1 {
                    let mut copy = child;
                    copy.fade();
                    copy
                } else {
                    child
                }))
            }
        };
        let to_child = to.children.get(ti.0)?.clone();
        // The donor side is a snapshot: padding it means keeping a faded copy of
        // the child the slot repeats, exactly as `add_n_more_submobjects` does.
        let to_child = if ti.1 {
            let mut copy = to_child;
            copy.fade();
            copy
        } else {
            to_child
        };
        children.push(plan_node(tree, from_child, to_child)?);
    }

    Some(NodePlan {
        from,
        from_points,
        to_points,
        from_transform,
        to_transform: to.mobject.transform,
        from_opacity,
        to_opacity: to.mobject.opacity,
        from_style,
        to_style: to.mobject.style,
        children,
    })
}

/// The padded slot order, as `(original index, is_copy)`.
///
/// Manim's `add_n_more_submobjects` (`mobject/mobject.py:3209-3232`) computes
/// `repeat_indices = (arange(target) * curr) // target`, counts how many of them
/// fall in each original slot, and then emits each original followed by its
/// copies. For `2 → 5` the counts are `[3, 2]` and the order is
/// `0, 0', 0'', 1, 1'`; for `1 → 2` it is `0, 0'`.
fn padded_indices(current: usize, width: usize) -> Vec<(usize, bool)> {
    let mut out = Vec::with_capacity(width);
    for original in 0..current {
        let repeats = (0..width)
            .filter(|&j| (j * current) / width == original)
            .count();
        out.push((original, false));
        for _ in 1..repeats.max(1) {
            out.push((original, true));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_math::Affine2;
    use rustim_mobject::{Mobject, shapes};

    /// A group node: no geometry of its own, children are the visible objects.
    fn insert_group(tree: &mut SceneTree) -> NodeId {
        tree.insert(Mobject::new(vec![]))
    }

    fn insert_child(tree: &mut SceneTree, parent: NodeId, m: Mobject) -> NodeId {
        let id = tree.insert(m);
        assert!(tree.reparent(id, parent), "reparent must succeed");
        id
    }

    /// A square translated so its centre sits at `at`, which makes "where did the
    /// child end up" a single comparison.
    fn square_at(tree: &mut SceneTree, parent: NodeId, at: (f64, f64)) -> NodeId {
        let id = insert_child(tree, parent, Mobject::new(shapes::square(2.0, 2.0)));
        tree.get_mut(id)
            .unwrap()
            .set_transform(Affine2::translate(at));
        id
    }

    fn centre(tree: &SceneTree, id: NodeId) -> Point {
        tree.get(id).unwrap().critical_point((0.0, 0.0)).unwrap()
    }

    /// Every child morphs, not just the group's (empty) own array: the property
    /// the whole module exists for.
    #[test]
    fn a_family_morph_moves_every_child() {
        let mut tree = SceneTree::new();
        let source = insert_group(&mut tree);
        let s0 = square_at(&mut tree, source, (-2.0, 0.0));
        let s1 = square_at(&mut tree, source, (0.0, 0.0));
        let target = insert_group(&mut tree);
        let t0 = square_at(&mut tree, target, (1.0, 1.0));
        let t1 = square_at(&mut tree, target, (3.0, -1.0));

        let mut anim = TransformGroup::new(source, target);
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        assert_eq!(centre(&tree, s0), Point::new(1.0, 1.0));
        assert_eq!(centre(&tree, s1), Point::new(3.0, -1.0));
        assert_eq!(tree.get(s0).unwrap().points, tree.get(t0).unwrap().points);
        assert_eq!(tree.get(s1).unwrap().points, tree.get(t1).unwrap().points);
    }

    /// The half-way frame is a real interpolation, not a snap: the point arrays
    /// are the average of the two ends.
    #[test]
    fn the_midpoint_is_halfway_between_the_two_arrays() {
        let mut tree = SceneTree::new();
        let source = insert_group(&mut tree);
        let s0 = square_at(&mut tree, source, (-2.0, 0.0));
        let target = insert_group(&mut tree);
        square_at(&mut tree, target, (2.0, 0.0));

        let mut anim = TransformGroup::new(source, target);
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        assert_eq!(centre(&tree, s0), Point::new(0.0, 0.0));
    }

    /// The arc applies to every child, and to a child whose *whole* motion is in
    /// its deferred transform — the case a naive local-array arc would draw
    /// straight.
    #[test]
    fn a_family_morph_bends_every_child_when_asked() {
        use std::f64::consts::PI;

        let mut tree = SceneTree::new();
        let source = insert_group(&mut tree);
        let s0 = square_at(&mut tree, source, (-2.0, 0.0));
        let target = insert_group(&mut tree);
        let t0 = square_at(&mut tree, target, (2.0, 0.0));

        let mut anim = TransformGroup::new(source, target).with_path_arc(PI);
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        // Halfway round a half-turn is one radius below the chord's midpoint.
        let mid = centre(&tree, s0);
        assert!(
            (mid.x).abs() < 1e-12 && (mid.y + 2.0).abs() < 1e-12,
            "{mid:?}"
        );

        // And the ends are exact, as for a leaf.
        anim.interpolate(1.0, &mut tree);
        assert_eq!(
            tree.get(s0).unwrap().materialize(),
            tree.get(t0).unwrap().materialize()
        );
    }

    /// Padding: the shorter side gains faded copies of its own children, and the
    /// pairings stay in padded order. A one-child group can therefore split into a
    /// two-child one.
    #[test]
    fn the_shorter_side_gains_faded_copies() {
        let mut tree = SceneTree::new();
        let source = insert_group(&mut tree);
        let s0 = square_at(&mut tree, source, (-1.0, 0.0));
        let target = insert_group(&mut tree);
        let t0 = square_at(&mut tree, target, (1.0, 0.0));
        let t1 = square_at(&mut tree, target, (3.0, 0.0));

        let mut anim = TransformGroup::new(source, target);
        anim.begin(&tree);
        anim.interpolate(0.0, &mut tree);

        assert_eq!(tree.child_ids(source).len(), 2, "one child became two");
        let copy = tree.child_ids(source)[1];
        assert_ne!(copy, s0, "the copy is a new node");
        assert_eq!(tree.get(copy).unwrap().opacity, 0.0, "and starts invisible");
        assert_eq!(tree.get(s0).unwrap().opacity, 1.0);

        anim.interpolate(1.0, &mut tree);
        assert_eq!(centre(&tree, s0), Point::new(1.0, 0.0));
        assert_eq!(centre(&tree, copy), Point::new(3.0, 0.0));
        assert_eq!(tree.get(copy).unwrap().opacity, 1.0);
        assert_eq!(tree.get(s0).unwrap().points, tree.get(t0).unwrap().points);
        assert_eq!(tree.get(copy).unwrap().points, tree.get(t1).unwrap().points);
    }

    /// The other direction: an excess source child morphs into a faded copy of a
    /// target child and ends transparent, because the source keeps every node.
    #[test]
    fn an_excess_child_fades_out() {
        let mut tree = SceneTree::new();
        let source = insert_group(&mut tree);
        square_at(&mut tree, source, (-1.0, 0.0));
        let extra = square_at(&mut tree, source, (1.0, 0.0));
        let target = insert_group(&mut tree);
        square_at(&mut tree, target, (2.0, 0.0));

        let mut anim = TransformGroup::new(source, target);
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        assert_eq!(tree.child_ids(source).len(), 2, "nothing is removed");
        assert_eq!(tree.get(extra).unwrap().opacity, 0.0);
        assert_eq!(tree.get(tree.child_ids(source)[0]).unwrap().opacity, 1.0);
    }

    /// Manim's padding is *interleaved*, not appended: two children becoming five
    /// order as `c0, c0', c0'', c1, c1'`, and the copies are adjacent to their
    /// original because that is what keeps the split proportional.
    #[test]
    fn padding_keeps_manims_interleaved_order() {
        let mut tree = SceneTree::new();
        let source = insert_group(&mut tree);
        let s0 = square_at(&mut tree, source, (-2.0, 0.0));
        let s1 = square_at(&mut tree, source, (-1.0, 0.0));
        let target = insert_group(&mut tree);
        for i in 0..5 {
            square_at(&mut tree, target, (i as f64, 0.0));
        }

        let mut anim = TransformGroup::new(source, target);
        anim.begin(&tree);
        anim.interpolate(0.0, &mut tree);

        let ids = tree.child_ids(source);
        assert_eq!(ids.len(), 5);
        assert_eq!(ids[0], s0);
        assert_eq!(ids[3], s1);
        // The three squares on the left are the same original, and the two on the
        // right are the other one.
        let origin = tree.get(s0).unwrap().points.clone();
        let other = tree.get(s1).unwrap().points.clone();
        for &id in &ids[1..3] {
            assert_eq!(tree.get(id).unwrap().points, origin);
        }
        for &id in &ids[4..] {
            assert_eq!(tree.get(id).unwrap().points, other);
        }
    }

    /// A family morph also works leaf-to-leaf: it is the degenerate case where both
    /// sides have no children, and it must not require a group wrapper.
    #[test]
    fn two_leaves_morph_without_any_children() {
        let mut tree = SceneTree::new();
        let a = tree.insert(Mobject::new(shapes::square(2.0, 2.0)));
        let b = tree.insert(Mobject::new(shapes::circle(1.0)));

        let mut anim = TransformGroup::new(a, b);
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        assert_eq!(tree.get(a).unwrap().points, tree.get(b).unwrap().points);
        assert_eq!(
            tree.get(a).unwrap().points.len(),
            16,
            "a circle, not a square"
        );
    }

    /// Recursion: a group inside a group is aligned as a pair of its own, so the
    /// grandchild is what moves. A port that only zipped the top level would leave
    /// this unchanged (both inner groups have no points of their own).
    #[test]
    fn nested_groups_are_morphed_recursively() {
        let mut tree = SceneTree::new();
        let source = insert_group(&mut tree);
        let inner = insert_group(&mut tree);
        assert!(tree.reparent(inner, source));
        let leaf = square_at(&mut tree, inner, (-3.0, 0.0));

        let target = insert_group(&mut tree);
        let inner_target = insert_group(&mut tree);
        assert!(tree.reparent(inner_target, target));
        let leaf_target = square_at(&mut tree, inner_target, (3.0, 0.0));

        let mut anim = TransformGroup::new(source, target);
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        assert_eq!(
            tree.get(leaf).unwrap().points,
            tree.get(leaf_target).unwrap().points
        );
        assert_eq!(tree.child_ids(source), &[inner]);
    }

    /// The donor is read once and never written: a target left in the scene must
    /// not be half-morphed by someone else's animation.
    #[test]
    fn the_donor_is_never_mutated_or_inserted() {
        let mut tree = SceneTree::new();
        let source = insert_group(&mut tree);
        square_at(&mut tree, source, (0.0, 0.0));
        let target = insert_group(&mut tree);
        let t0 = square_at(&mut tree, target, (4.0, 0.0));
        let before = (tree.get(t0).unwrap().points.clone(), tree.len());

        let mut anim = TransformGroup::new(source, target);
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        assert_eq!(tree.get(t0).unwrap().points, before.0);
        assert_eq!(tree.len(), before.1, "no padding needed, so nothing added");
    }

    /// A shape ↔ group morph is refused rather than approximated: `null_point_align`
    /// is not ported, and a half-aligned picture would be worse than a no-op.
    #[test]
    fn a_shape_to_group_morph_is_a_no_op() {
        let mut tree = SceneTree::new();
        let shape = tree.insert(Mobject::new(shapes::square(2.0, 2.0)));
        let group = insert_group(&mut tree);
        square_at(&mut tree, group, (3.0, 0.0));
        let before = tree.get(shape).unwrap().points.clone();

        let mut anim = TransformGroup::new(shape, group);
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        assert_eq!(tree.get(shape).unwrap().points, before);
    }

    /// A node cannot morph into its own descendant: the pair would be aligned
    /// against a copy of itself, and the per-frame writes would fight.
    #[test]
    fn morphing_into_its_own_child_is_a_no_op() {
        let mut tree = SceneTree::new();
        let parent = insert_group(&mut tree);
        let child = square_at(&mut tree, parent, (0.0, 0.0));
        let before = tree.get(child).unwrap().points.clone();

        let mut anim = TransformGroup::new(parent, child);
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        assert_eq!(tree.get(child).unwrap().points, before);
    }

    /// Reduced motion and any later frame take the same path: `finish` does nothing,
    /// so the end state is simply `interpolate(1)`, and it is exact.
    #[test]
    fn the_end_state_is_exact() {
        let mut tree = SceneTree::new();
        let source = insert_group(&mut tree);
        let s = square_at(&mut tree, source, (-1.0, 0.0));
        let target = insert_group(&mut tree);
        let t = square_at(&mut tree, target, (1.0, 0.0));
        tree.get_mut(t).unwrap().opacity = 0.4;

        let mut anim = TransformGroup::new(source, target).with_duration(0.7);
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        assert_eq!(tree.get(s).unwrap().points, tree.get(t).unwrap().points);
        assert_eq!(tree.get(s).unwrap().opacity, 0.4);
        assert_eq!(anim.duration(), 0.7);
    }
}
