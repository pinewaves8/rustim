//! Moblet storage, addresses and the deferred transform.

use rustim_math::{Affine2, BezPath, Point, Rect, bezier};
use slotmap::{SlotMap, new_key_type};
use std::collections::HashMap;

use crate::kind::NodeKind;
use crate::style::{DirtyFlags, Style};

new_key_type! {
    /// A stable handle to a mobject.
    ///
    /// `slotmap`'s keys are generational: deleting a mobject and inserting a new
    /// one never reuses the old id, so a command that names node 7 either hits
    /// the node it meant or fails cleanly. That property is why we use this and
    /// not an ECS.
    pub struct NodeId;
}

impl NodeId {
    /// The opaque 64-bit form used on the wire.
    ///
    /// This is `slotmap`'s `as_ffi`: the generation in the high 32 bits and the
    /// slot index in the low 32. It is deliberately *not* the bare index — that
    /// would make a stale id silently address whichever mobject inherited the
    /// slot, which is exactly the failure the generational key prevents. Use
    /// [`NodeId::raw`] and [`NodeId::from_raw`] at the JSON boundary and treat
    /// the number as meaningless everywhere else.
    pub fn raw(self) -> u64 {
        slotmap::Key::data(&self).as_ffi()
    }

    /// Rebuild an id from [`NodeId::raw`].
    ///
    /// Any `u64` is accepted: an id that names nothing is not an error to
    /// construct, it just fails to resolve. `from_ffi` is safe for arbitrary
    /// input, so a hostile command cannot drive this into memory unsafety — the
    /// worst it can do is name a node that is not there.
    ///
    /// `const` so that [`NodeId::RESERVED`] and the accessibility layer's
    /// non-mobject nodes can be compile-time constants.
    pub const fn from_raw(raw: u64) -> Self {
        Self(slotmap::KeyData::from_ffi(raw))
    }

    /// An id [`SceneTree::insert`] can never return.
    ///
    /// This is `slotmap`'s null key — index `u32::MAX`, generation 1 — and it is
    /// also [`NodeId::default`], which makes it the natural spelling of "no
    /// node". `slotmap`'s `try_insert_with_key` panics on
    /// `slots.len() >= u32::MAX` *before* it can create a slot at index
    /// `u32::MAX`, so no insert can ever reach it, and `SceneTree::get` on it
    /// misses the generation check against slot 0 anyway.
    ///
    /// # What the obvious reasoning gets wrong
    ///
    /// The tempting argument is "reserve raw `0`, since the generation is packed
    /// into the high half and starts at 1, so a real id is never 0". Three things
    /// are wrong with it, and all three were found by running the test rather
    /// than by reading this far:
    ///
    /// - `NodeId::from_raw(0)` does **not** produce raw 0. `KeyData::from_ffi`
    ///   rewrites the generation to `value >> 32 | 1`, so it produces
    ///   `0x1_0000_0000`. Nothing in this type can hold raw 0.
    /// - The first id a fresh `SceneTree` hands out is `0x1_0000_0001`, not
    ///   `0x1_0000_0000` and not 0: `SlotMap` keeps a sentinel at index 0 that it
    ///   never hands out, so indices start at 1.
    /// - So `0x1_0000_0000` *does* happen to be unused — but by way of the
    ///   sentinel, an implementation detail whose purpose is that keys stay valid
    ///   when a slot map is converted between flavours, not a promise that index
    ///   0 is free for callers.
    ///
    /// Reserving the null key needs none of that reasoning. It also makes the
    /// reservation checkable by equality, which is what the accessibility layer
    /// needs: a reserved id cannot be told from a real one by looking at its
    /// magnitude, because it packs a real-looking generation.
    pub const RESERVED: Self = Self::from_raw(0x1_FFFF_FFFF);
}

/// Serialised as the opaque [`NodeId::raw`] integer, so a command on the wire
/// reads `{"action":"remove","target":4294967299}` rather than a
/// `{idx, version}` object.
///
/// This is hand-written rather than taken from `slotmap`'s `serde` feature on
/// purpose: that feature emits the two-field struct form, which both leaks the
/// generation split into the AI-facing protocol and makes every diff payload
/// four times larger than it needs to be.
impl serde::Serialize for NodeId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(self.raw())
    }
}

impl<'de> serde::Deserialize<'de> for NodeId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        u64::deserialize(d).map(Self::from_raw)
    }
}

/// A renderable 2D object.
///
/// The point layout is Manim's: a flat array read four points at a time — the
/// start of a curve, its two control points, its end — so a closed 4-cubic
/// circle is 16 points and a square is 4 curves, not 4 points. Keeping that
/// representation, rather than a tree of path commands, is what makes Manim-style
/// morphing possible: interpolating two mobjects is a point-wise operation once
/// the arrays have been padded to the same length.
#[derive(Debug, Clone)]
pub struct Mobject {
    pub id: NodeId,
    /// Points in *local* space, before [`Mobject::transform`] is applied.
    pub points: Vec<Point>,
    pub style: Style,
    /// The deferred transform. O(1) to set; see [`Mobject::materialize`].
    pub transform: Affine2,
    /// The mobject's own opacity, multiplied with the style's fill/stroke
    /// opacity. Animations like `FadeIn` drive this.
    pub opacity: f64,
    /// Human-readable name. The accessibility layer reads this; the AI layer
    /// may address the node by it as an alternative to the opaque id.
    pub label: Option<String>,
    /// What this object *is*. See [`crate::kind::NodeKind`].
    ///
    /// Stored rather than derived from the point array, because the geometry is
    /// exactly what morphing rewrites — a circle that has been morphed into a
    /// square is still the object the reader was told about. `Mobject` is not
    /// `Serialize`, so adding this field changes no JSON on the wire; the field
    /// becomes visible to a host only through the `add` command's `type` and the
    /// `set` command's `kind` property, which share this enum's names.
    pub kind: NodeKind,
    pub children: Vec<NodeId>,
    pub parent: Option<NodeId>,
    /// Draw order within a parent. Manim has no explicit z; order of addition
    /// decides, and an explicit field makes that serialisable.
    pub z: i32,
    pub dirty: DirtyFlags,
}

impl Mobject {
    pub fn new(points: Vec<Point>) -> Self {
        Self {
            id: NodeId::default(),
            points,
            style: Style::default(),
            transform: Affine2::IDENTITY,
            opacity: 1.0,
            label: None,
            kind: NodeKind::Unknown,
            children: Vec::new(),
            parent: None,
            z: 0,
            dirty: DirtyFlags::ALL,
        }
    }

    /// Declare what this object is.
    pub fn with_kind(mut self, kind: NodeKind) -> Self {
        self.kind = kind;
        self.dirty.semantic = true;
        self
    }

    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self.dirty.style = true;
        self
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self.dirty.semantic = true;
        self
    }

    /// Set the deferred transform. This is the O(1) move that replaces Manim's
    /// O(points) rewrite of every descendant's point array.
    pub fn set_transform(&mut self, transform: Affine2) {
        self.transform = transform;
        self.dirty.geometry = true;
    }

    /// Produce this mobject's points in world space.
    ///
    /// This is the explicit, cacheable half of the deferred-transform design:
    /// callers that need final coordinates (the renderer's upload step,
    /// `critical_point`, morph alignment) pay for it here, and nothing pays for
    /// it otherwise.
    pub fn materialize(&self) -> Vec<Point> {
        self.points.iter().map(|p| self.transform * *p).collect()
    }

    /// Build a drawable path from Manim-layout points, applying `parent` on top
    /// of this mobject's own transform.
    ///
    /// `parent` folds in the accumulated transform of every ancestor, which is
    /// how the scene tree is flattened at draw time without any mobject storing
    /// a world matrix.
    pub fn to_bez_path(&self, parent: Affine2) -> BezPath {
        let t = parent * self.transform;
        bezier_path_from_points(self.points.iter().map(|p| t * *p))
    }

    /// The extreme point of this mobject's bounding box along `direction`.
    ///
    /// `direction` follows Manim's convention per axis: a negative component takes
    /// that axis' minimum, a positive one its maximum, and exactly zero the
    /// **midpoint** of its extent. So `(-1, 0)` is not "centre vertically" but "left
    /// edge, vertically centred".
    ///
    /// This is `Mobject.get_critical_point`, which morph alignment reads. Two
    /// things about it are worth stating because both are Manim's behaviour and
    /// neither is obvious:
    ///
    /// - It bounds the *anchors* — each curve's start and end point — not the
    ///   control points and not the sampled curve. A cusped cubic whose control
    ///   points reach far outside its endpoints has its critical point between
    ///   those endpoints. See [`rustim_math::bezier::anchors`].
    /// - It sees **world** coordinates. Manim's mobjects store world points, so the
    ///   question does not arise there; here the transform is deferred, and the
    ///   point of this method existing on the mobject rather than on the point array
    ///   is that it materialises first. Callers must not pre-transform and pass the
    ///   result in as well.
    ///
    /// `None` when the point array is not a whole number of curves — a mobject
    /// mid-construction. Manim raises there; this reports it.
    pub fn critical_point(&self, direction: (f64, f64)) -> Option<Point> {
        let world = self.materialize();
        bezier::anchors(&world).map(|anchors| bezier::critical_point(&anchors, direction))
    }

    /// Whether this object has anything in the picture.
    ///
    /// The point array is a shape's picture, but it is not every object's. Text
    /// is a glyph run that a shaper positioned and that the scene owning the text
    /// pushes itself, so a text mobject has no points at all and is nevertheless
    /// on screen — which makes "count the points" the wrong test anywhere the
    /// question is *what the user can see or reach*. Asking it the other way is
    /// how text ends up invisible to the keyboard and to the accessibility layer
    /// while being perfectly visible to the eye.
    ///
    /// Opacity is deliberately a separate axis rather than folded in here. The
    /// callers want different things from it — a focus list drops a faded-out
    /// object, while the semantic tree keeps it and marks it hidden so that a
    /// reader can still be told it exists — and a single combined predicate
    /// could not express both.
    ///
    /// The plain tree walk in `SceneContext::build_draw_list` is narrower still
    /// and tests the points directly: it can only emit a filled path, and text
    /// has none for it to fill.
    pub fn has_picture(&self) -> bool {
        matches!(self.kind, NodeKind::Text | NodeKind::Formula) || self.points.len() >= 2
    }
}

/// Convert Manim's flat control-point array into a path.
///
/// The array is read **[`rustim_math::bezier::POINTS_PER_CURVE`] at a time** — `(start, c1,
/// c2, end)` per cubic — and each group after the first repeats the previous
/// group's endpoint as its own start. That repetition is Manim's format, not
/// redundancy to be optimised away: `get_anchors`, `subdivide_bezier` and
/// `bezier_remap` all index the array four at a time, so an array shortened to
/// "one start plus three per curve" is not a shorter picture of the same path,
/// it is a *different* path. Reading one of those three at a time shifts every
/// control point by one and rounds a corner (see the regression test).
///
/// A trailing group that does not divide evenly is ignored rather than
/// panicking, because an in-progress animation can legitimately observe a
/// partially-built array.
pub fn bezier_path_from_points(points: impl IntoIterator<Item = Point>) -> BezPath {
    let mut path = BezPath::new();
    let mut it = points.into_iter();
    let Some(s0) = it.next() else {
        return path;
    };
    path.move_to(s0);
    // Under four points is a subpath that has not been given a shape yet. It
    // still moves — a one-point mobject is a legitimate degenerate object, and
    // `bezier::anchors` accepts a one-element array for the same reason — it
    // just has no curve to draw.
    let (Some(c1), Some(c2), Some(e0)) = (it.next(), it.next(), it.next()) else {
        return path;
    };
    path.curve_to(c1, c2, e0);
    // The leading point of each later group restates where the previous curve
    // ended, so it is consumed but not emitted.
    while let (Some(_), Some(c1), Some(c2), Some(end)) =
        (it.next(), it.next(), it.next(), it.next())
    {
        path.curve_to(c1, c2, end);
    }
    path
}

/// The scene tree: the storage the rest of the engine addresses mobjects through.
#[derive(Debug, Default)]
pub struct SceneTree {
    nodes: SlotMap<NodeId, Mobject>,
    /// Draw order per parent bucket. Renderers walk this rather than the
    /// `SlotMap`, whose iteration order is not meaningful.
    order: HashMap<Option<NodeId>, Vec<NodeId>>,
}

impl SceneTree {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a mobject, assigning it a fresh stable id.
    pub fn insert(&mut self, mut m: Mobject) -> NodeId {
        m.dirty = DirtyFlags::ALL;
        let id = self.nodes.insert(m);
        self.nodes[id].id = id;
        self.order.entry(None).or_default().push(id);
        id
    }

    /// Attach `child` under `parent`, removing it from the root bucket.
    ///
    /// Returns `false` if either id is unknown, or if the move would create a
    /// cycle. Failing rather than panicking matters: this is reachable from the
    /// AI command layer, so a bad command must be an error value.
    pub fn reparent(&mut self, child: NodeId, parent: NodeId) -> bool {
        if child == parent || !self.nodes.contains_key(child) || !self.nodes.contains_key(parent) {
            return false;
        }
        if self.is_ancestor_of(child, parent) {
            return false;
        }
        let old = self.nodes[child].parent;
        if let Some(list) = self.order.get_mut(&old) {
            list.retain(|&id| id != child);
        }
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
        self.order.entry(Some(parent)).or_default().push(child);
        true
    }

    fn is_ancestor_of(&self, maybe_ancestor: NodeId, node: NodeId) -> bool {
        let mut cur = Some(node);
        while let Some(id) = cur {
            if id == maybe_ancestor {
                return true;
            }
            cur = self.nodes.get(id).and_then(|m| m.parent);
        }
        false
    }

    pub fn get(&self, id: NodeId) -> Option<&Mobject> {
        self.nodes.get(id)
    }

    /// The tree's root-level nodes, in draw order.
    ///
    /// `order[&None]` is the single source of truth for this — `remove` puts an
    /// orphaned child back into that bucket, so a second list kept alongside it
    /// could only ever drift.
    pub fn root_ids(&self) -> &[NodeId] {
        self.order.get(&None).map_or(&[], Vec::as_slice)
    }

    /// A node's direct children, in draw order. Empty for an unknown id.
    pub fn child_ids(&self, id: NodeId) -> &[NodeId] {
        self.nodes.get(id).map_or(&[], |m| m.children.as_slice())
    }

    /// The world-space bounding box of a node's own geometry, ancestors' and own
    /// transforms included.
    ///
    /// Bounds the **anchors**, like [`Mobject::critical_point`], and for the same
    /// reason: they are what the point array is *about*, and sampling the curves
    /// would make the answer depend on a flattening tolerance.
    ///
    /// `None` when the id is unknown or the point array is empty. This is the
    /// primitive `rustim-app`'s `fit_and_centre` used to hand-roll, and what the
    /// accessibility layer measures a node's on-screen rectangle from — sizes are
    /// read off materialised geometry, never off the shape parameters, which are
    /// not stored on the mobject in the first place.
    pub fn world_bounds(&self, id: NodeId) -> Option<Rect> {
        bezier::bounds_of(&self.world_anchors(id)?)
    }

    /// A node's anchors with its own and its ancestors' transforms folded in.
    ///
    /// The materialisation step, in the form the geometry readers want it:
    /// [`Mobject::materialize`] leaves the points local, and [`Self::world_bounds`]
    /// keeps only their box. A *size* that survives rotation needs the points
    /// themselves — a rotated square's box is its side times `cos + sin` — and the
    /// accessibility layer measures sizes, so the anchors are exposed rather than
    /// hidden inside the bounds.
    ///
    /// `None` for an unknown id, an empty point array, or a malformed one; see
    /// [`bezier::anchors`].
    pub fn world_anchors(&self, id: NodeId) -> Option<Vec<Point>> {
        let m = self.nodes.get(id)?;
        let t = self.parent_transform(id) * m.transform;
        bezier::anchors(&m.points.iter().map(|p| t * *p).collect::<Vec<_>>())
    }

    /// Remove a node. Does **not** recurse into children.
    ///
    /// Deliberately non-recursive: the caller (the command layer) walks the
    /// subtree explicitly so that it can report every removed id in its diff.
    /// Returns the removed mobject, or `None` if the id was already gone.
    pub fn remove(&mut self, id: NodeId) -> Option<Mobject> {
        let parent = self.nodes.get(id).and_then(|m| m.parent);
        if let Some(list) = self.order.get_mut(&parent) {
            list.retain(|&x| x != id);
        }
        let children = self
            .nodes
            .get(id)
            .map(|m| m.children.clone())
            .unwrap_or_default();
        for c in children {
            if let Some(m) = self.nodes.get_mut(c) {
                m.parent = None;
            }
            self.order.entry(None).or_default().push(c);
        }
        self.nodes.remove(id)
    }

    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut Mobject> {
        self.nodes.get_mut(id)
    }

    /// The accumulated transform of `id`'s ancestors. `Affine2::IDENTITY` for a
    /// root node or an unknown id.
    pub fn parent_transform(&self, id: NodeId) -> Affine2 {
        let mut t = Affine2::IDENTITY;
        let mut cur = self.nodes.get(id).and_then(|m| m.parent);
        while let Some(p) = cur {
            let Some(m) = self.nodes.get(p) else { break };
            // Innermost ancestor last, so `t = parent * child_local`.
            t = m.transform * t;
            cur = m.parent;
        }
        t
    }

    /// Ids in draw order, parents before their children.
    pub fn draw_order(&self) -> Vec<NodeId> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<NodeId> = self
            .order
            .get(&None)
            .map(|v| v.iter().rev().copied().collect())
            .unwrap_or_default();
        while let Some(id) = stack.pop() {
            out.push(id);
            if let Some(m) = self.nodes.get(id) {
                stack.extend(m.children.iter().rev().copied());
            }
        }
        out
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn clear(&mut self) {
        self.nodes.clear();
        self.order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Style;

    #[test]
    fn ids_are_not_reused_after_deletion() {
        let mut tree = SceneTree::new();
        let a = tree.insert(Mobject::new(vec![]));
        tree.nodes.remove(a);
        let b = tree.insert(Mobject::new(vec![]));
        assert_ne!(
            a, b,
            "a recycled id would let a stale command hit a live node"
        );
    }

    /// Nothing a slot map hands out is ever [`NodeId::RESERVED`].
    ///
    /// The assertion that matters is the *equality* one, and it has to be
    /// equality: `RESERVED` is the null key, which packs generation 1 over index
    /// `u32::MAX`, so its raw value looks exactly like a real id's. There is no
    /// magnitude test that separates them — anything distinguishing the reserved
    /// id has to compare against the constant.
    #[test]
    fn a_reserved_id_is_never_handed_out() {
        let mut tree = SceneTree::new();
        let mut live = Vec::new();
        for _ in 0..64 {
            live.push(tree.insert(Mobject::new(vec![])));
        }
        // Delete half and re-insert: a recycled slot bumps the generation, so
        // this is where an id could collide with something already handed out.
        for id in live.drain(..32) {
            tree.remove(id);
        }
        for _ in 0..32 {
            live.push(tree.insert(Mobject::new(vec![])));
        }
        for (i, id) in live.iter().enumerate() {
            assert_ne!(
                *id,
                NodeId::RESERVED,
                "insert {i} handed out the reserved id"
            );
            assert!(tree.get(*id).is_some(), "insert {i} did not resolve");
        }
        assert!(
            tree.get(NodeId::RESERVED).is_none(),
            "the reserved id resolved to a mobject"
        );
        assert_eq!(
            NodeId::RESERVED,
            NodeId::default(),
            "RESERVED should be the id a default-initialised NodeId already is"
        );
        assert_eq!(
            tree.insert(Mobject::new(vec![])).raw() >> 32,
            1,
            "generations start at 1, not 0"
        );
    }

    /// What a fresh tree's first id actually is, and why "reserve raw 0" was not
    /// the reason it is safe to reserve something.
    ///
    /// Two separate surprises, both measured rather than assumed. `from_ffi`
    /// rewrites a zero generation to 1, so `from_raw(0)` is `0x1_0000_0000` and
    /// raw 0 is unrepresentable. And the first id is not even that: `SlotMap`
    /// keeps a sentinel at index 0 that it never hands out, so indices start at
    /// 1 and the first id is `0x1_0000_0001`. `0x1_0000_0000` happens to be
    /// unused too, but because of that sentinel — a detail whose stated purpose
    /// is keeping keys valid across slot-map flavours, not reserving index 0 for
    /// callers — which is why the reservation rests on the null key instead.
    #[test]
    fn the_first_insert_is_index_one_not_zero() {
        assert_eq!(
            NodeId::from_raw(0).raw(),
            1 << 32,
            "generation 0 is rewritten to 1, so raw 0 cannot be spelled"
        );
        let mut tree = SceneTree::new();
        let first = tree.insert(Mobject::new(vec![]));
        assert_eq!(
            first.raw(),
            (1 << 32) | 1,
            "slot 0 is a sentinel, not a mobject"
        );
        assert_ne!(first.raw(), NodeId::from_raw(0).raw());
        assert_ne!(first, NodeId::RESERVED);
    }

    #[test]
    fn from_raw_round_trips_every_id_this_engine_hands_out() {
        // `from_ffi` forces the generation odd, so the round trip is exact
        // precisely when the generation it is given already is — and `as_ffi`
        // only ever emits odd ones. Worth pinning because the JSON protocol
        // relies on it: a `target` that did not survive the trip would address
        // the wrong node rather than failing.
        let mut tree = SceneTree::new();
        for _ in 0..8 {
            let id = tree.insert(Mobject::new(vec![]));
            assert_eq!(NodeId::from_raw(id.raw()), id, "{:#x}", id.raw());
        }
        // An even generation is the one class that does not survive, and it is
        // unreachable from `raw` — which is the whole reason reserving id 0 is a
        // trap rather than a convenience.
        assert_ne!(NodeId::from_raw(0).raw(), 0);
    }

    #[test]
    fn deferred_transform_defers() {
        let mut m = Mobject::new(vec![Point::new(1.0, 0.0), Point::new(2.0, 0.0)]);
        m.set_transform(Affine2::translate((10.0, 0.0)));
        // Local points untouched...
        assert_eq!(m.points[0], Point::new(1.0, 0.0));
        // ...world points moved.
        assert_eq!(m.materialize()[0], Point::new(11.0, 0.0));
    }

    #[test]
    fn parent_transform_composes_outwards_in() {
        let mut tree = SceneTree::new();
        let mut parent = Mobject::new(vec![]);
        parent.set_transform(Affine2::translate((100.0, 0.0)));
        let p = tree.insert(parent);
        let mut child = Mobject::new(vec![Point::new(1.0, 0.0)]);
        child.set_transform(Affine2::translate((0.0, 5.0)));
        let c = tree.insert(child);
        assert!(tree.reparent(c, p));
        let world = tree.get(c).unwrap().to_bez_path(tree.parent_transform(c));
        // 1 + parent(100,0) + child(0,5)
        assert_eq!(world.elements()[0], kurbo_move_to(101.0, 5.0));
    }

    fn kurbo_move_to(x: f64, y: f64) -> rustim_math::PathEl {
        rustim_math::PathEl::MoveTo(Point::new(x, y))
    }

    #[test]
    fn reparent_rejects_cycles() {
        let mut tree = SceneTree::new();
        let a = tree.insert(Mobject::new(vec![]));
        let b = tree.insert(Mobject::new(vec![]));
        assert!(tree.reparent(b, a));
        assert!(!tree.reparent(a, b), "a cycle must be refused");
        assert!(!tree.reparent(a, a), "self-parenting must be refused");
    }

    #[test]
    fn draw_order_puts_parents_first() {
        let mut tree = SceneTree::new();
        let a = tree.insert(Mobject::new(vec![]));
        let b = tree.insert(Mobject::new(vec![]));
        let c = tree.insert(Mobject::new(vec![]));
        assert!(tree.reparent(c, a));
        let order = tree.draw_order();
        assert_eq!(order.len(), 3);
        assert!(
            order.iter().position(|&x| x == a).unwrap()
                < order.iter().position(|&x| x == c).unwrap()
        );
        let _ = b;
    }

    #[test]
    fn bezier_path_walks_four_point_segments() {
        // Four points, one cubic.
        let pts = vec![
            Point::new(0.0, 0.0),
            Point::new(1.0, 0.0),
            Point::new(1.0, 1.0),
            Point::new(0.0, 1.0),
        ];
        let path = bezier_path_from_points(pts);
        assert_eq!(path.elements().len(), 2);

        // The count is the only thing that separates the two readings once four
        // curves are involved, which is why a square was where this showed up:
        // reading three at a time consumes the leading repeat as a control point
        // and finds five curves in a four-curve array.
        let circle = crate::shapes::circle(1.0);
        assert_eq!(circle.len(), 16);
        assert_eq!(bezier_path_from_points(circle).elements().len(), 5);
    }

    #[test]
    fn a_path_shorter_than_one_curve_still_moves() {
        // Not a valid curve, but a valid subpath — and a one-point mobject is
        // something scenes do build.
        let one = bezier_path_from_points(vec![Point::new(3.0, 4.0)]);
        assert_eq!(one.elements(), &[kurbo_move_to(3.0, 4.0)]);
        assert!(bezier_path_from_points(Vec::new()).is_empty());
    }

    #[test]
    fn style_builder_marks_style_dirty() {
        let m = Mobject::new(vec![]).with_style(Style::default());
        assert!(m.dirty.style);
    }

    /// The whole reason `critical_point` is a method on the mobject rather than a
    /// free function over points: the transform is deferred, and this is one of the
    /// callers that must see final coordinates. A version that read `self.points`
    /// would answer about the untransformed shape and still look plausible.
    #[test]
    fn critical_point_sees_through_the_deferred_transform() {
        let mut m = Mobject::new(crate::shapes::square(2.0, 2.0));
        // Scale by 2, then shift. Written as `then_`, so it reads left to right.
        m.set_transform(Affine2::scale(2.0).then_translate(rustim_math::Vec2::new(10.0, -5.0)));
        assert_eq!(m.points[0], Point::new(-1.0, -1.0), "local points stay put");
        assert_eq!(m.materialize()[0], Point::new(8.0, -7.0));
        // The square's anchors span ±1, so the far corner is (1, 1) in local
        // space and (12, -3) after the transform.
        assert_eq!(m.critical_point((1.0, 1.0)), Some(Point::new(12.0, -3.0)));
    }

    /// Each axis is decided independently, and a zero component takes the midpoint
    /// of that axis' extent rather than the minimum — so `(0, 0)` is the centre and
    /// `(0, -1)` is the bottom edge, centred horizontally.
    #[test]
    fn critical_point_takes_a_midpoint_on_a_zero_axis() {
        // Anchors of a 3-4-5 triangle: the right angle at the origin.
        let m = Mobject::new(crate::shapes::right_triangle(3.0, 4.0));
        let corners = [
            Point::new(0.0, 0.0),
            Point::new(3.0, 0.0),
            Point::new(0.0, 4.0),
        ];
        let span = |pick: fn(Point) -> f64| {
            let v: Vec<f64> = corners.iter().copied().map(pick).collect();
            (
                v.iter().copied().fold(f64::INFINITY, f64::min),
                v.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            )
        };
        let (x0, x1) = span(|p| p.x);
        let (y0, y1) = span(|p| p.y);

        assert_eq!(m.critical_point((-1.0, 1.0)), Some(Point::new(x0, y1)));
        assert_eq!(
            m.critical_point((0.0, -1.0)),
            Some(Point::new((x0 + x1) / 2.0, y0))
        );
        assert_eq!(
            m.critical_point((0.0, 0.0)),
            Some(Point::new((x0 + x1) / 2.0, (y0 + y1) / 2.0))
        );
    }

    /// The critical point bounds the anchors, not the control points. A cusp makes
    /// that visible: its control points reach y = ±2 while its anchors sit on y = 0,
    /// so a version that bounded every point in the array would answer ±2.
    #[test]
    fn critical_point_bounds_anchors_not_control_points() {
        let cusp = vec![
            Point::new(0.0, 0.0),
            Point::new(1.0, 2.0),
            Point::new(2.0, -2.0),
            Point::new(3.0, 0.0),
        ];
        let m = Mobject::new(cusp);
        assert_eq!(m.critical_point((0.0, 1.0)), Some(Point::new(1.5, 0.0)));
    }

    /// A point array that is not a whole number of curves is mid-construction, and
    /// there are no anchors to bound. Reported rather than panicked on, because the
    /// animation layer can observe a partially built array.
    #[test]
    fn critical_point_refuses_a_ragged_point_array() {
        let m = Mobject::new(vec![Point::new(0.0, 0.0), Point::new(1.0, 1.0)]);
        assert_eq!(m.critical_point((1.0, 1.0)), None);
    }

    /// A straight-edged closed path must come out with straight edges.
    ///
    /// This is the regression test for reading the control-point array three at
    /// a time instead of four. Under the wrong reading the square still looked
    /// like a square — three of its corners were sharp and the fourth was
    /// rounded, because the extra point at the head of each curve group became a
    /// control point and pulled the corner in. `bezier::anchors` cannot see the
    /// difference (the anchors are identical either way); only the interior
    /// control points move, so the assertion has to be on those.
    #[test]
    fn a_corner_array_reads_back_as_straight_edges() {
        use rustim_math::PathEl;

        let pts = crate::shapes::square(2.0, 2.0);
        let path = bezier_path_from_points(pts.iter().copied());

        let mut cur = Point::ORIGIN;
        let mut edges = Vec::new();
        for el in path.elements() {
            match el {
                PathEl::MoveTo(p) => cur = *p,
                PathEl::CurveTo(c1, c2, p) => {
                    edges.push((cur, *c1, *c2, *p));
                    cur = *p;
                }
                other => panic!("a corner array should emit only cubics, got {other:?}"),
            }
        }
        assert_eq!(edges.len(), 4, "four straight edges");

        for (i, (p0, c1, c2, p3)) in edges.into_iter().enumerate() {
            // Straight means both control points lie exactly on the chord. The
            // 1/3-2/3 parameterisation puts them *on* it, not merely near it, so
            // this is an equality in the arithmetic rather than a tolerance.
            for (t, c) in [(1.0 / 3.0, c1), (2.0 / 3.0, c2)] {
                let want = p0.to_vec2().lerp(p3.to_vec2(), t);
                let d = (c.to_vec2() - want).hypot();
                assert!(
                    d < 1e-12,
                    "edge {i} control point {c:?} is {d} off the chord {p0:?}..{p3:?}"
                );
            }
        }
    }

    /// The array is four points per curve, and adjacent curves repeat the shared
    /// endpoint. Pinned because a three-per-curve array is a plausible-looking
    /// simplification that silently reinterprets every path built that way.
    #[test]
    fn the_control_point_array_is_four_points_per_curve() {
        let pts = crate::shapes::square(2.0, 2.0);
        assert_eq!(pts.len(), 16, "four curves, four points each");
        // The shared endpoint is repeated in the array rather than implied:
        // entry 4 restates entry 3, entry 8 restates entry 7, and so on.
        for i in (4..pts.len()).step_by(4) {
            assert_eq!(
                pts[i],
                pts[i - 1],
                "curve {} restates the shared corner",
                i / 4
            );
        }
        assert_eq!(
            pts.first(),
            pts.last(),
            "a closed corner path returns to where it started"
        );
    }
}
