//! World-space placement: `next_to`, `arrange`, `fit`.
//!
//! These are the three sentences a lesson actually uses about position. They
//! are deliberately **not** Manim's `VGroup` methods ported line for line: a
//! group here is a list of ids the caller names, because the caller is a
//! protocol action naming stable nodes, not a Python object holding references.
//! The observable behaviour is Manim's where the two agree — a placed object
//! sits `buff` away from its reference, arranged objects have equal *gaps*
//! (not equal widths), and a fitted group keeps its aspect ratio — and each
//! difference is stated below.
//!
//! # Everything is measured, in world space
//!
//! A placement reads [`SceneTree::world_bounds`], which is the box of a node's
//! materialised anchors with every ancestor transform folded in. Three
//! consequences, all of them the answer to a question a caller would otherwise
//! ask a second time:
//!
//! - A **rotated** object is placed by its axis-aligned world box, so "to the
//!   right of the arrow" leaves a gap a viewer can see, not a gap along the
//!   arrow's own axis.
//! - A **nested** object is placed correctly: the world box already includes
//!   its parent's transform, and the shift is converted back into the parent's
//!   space before it is composed (see [`shift_world`]).
//! - A **text or formula** node has no point array but declares a box, so it
//!   is placed like any other node. A node with neither is refused, never
//!   treated as a point.
//!
//! # Movement is a translation on the deferred transform
//!
//! Placement composes `translate(δ) · transform`; the point array is never
//! rewritten. So a `next_to` costs O(1) per node and does not disturb a scale
//! or rotation the object already carries — the same rule `set position`
//! follows. `fit` additionally composes a uniform scale about the group's
//! centre, in each node's own parent space.
//!
//! # Refusals are values
//!
//! [`LayoutError`] names the node or the field at fault. There are no panics:
//! the input is the command layer's, which is a language model at one end.

use rustim_math::{Affine2, Point, Rect, Vec2};
use rustim_mobject::{NodeId, SceneTree};

/// Which way a node is placed relative to its reference.
///
/// World space is y-up, so [`Direction::Up`] increases y.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    /// The index of the axis this direction moves along: 0 is x, 1 is y.
    fn axis(self) -> usize {
        match self {
            Self::Left | Self::Right => 0,
            Self::Up | Self::Down => 1,
        }
    }
}

/// The line `arrange` places nodes along.
///
/// The order is reading order, not a direction vector: [`Axis::Row`] runs left
/// to right and [`Axis::Column`] runs **top to bottom** (decreasing y, because
/// the world is y-up). Manim spells these as `RIGHT`/`DOWN` and arranges a
/// column from the first submobject downwards; the names here say which of the
/// two the caller meant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Row,
    Column,
}

/// Why a placement could not be made.
#[derive(Clone, Debug, PartialEq)]
pub enum LayoutError {
    /// The id is not in the tree. The command layer resolves a `Target` before
    /// calling in, so this is the "it was deleted between the resolve and the
    /// shift" case, but it is still a value rather than an index panic.
    Unknown(NodeId),
    /// The node has no measurable extent: no point array, no declared box.
    Unmeasurable(NodeId),
    /// A target is a descendant of another target, so placing the group would
    /// move (or scale) it twice. Arranging siblings is the operation; a caller
    /// that really wants both should place the subtree and then the child.
    Nested(NodeId),
    /// A parameter is outside its domain, named by the field that carries it.
    OutOfRange { field: &'static str, detail: String },
    /// The node's ancestor chain collapses the plane, so a world point has no
    /// representation in the node's parent space. Placing it would write
    /// infinities into the transform.
    Singular(NodeId),
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(id) => write!(f, "node {} is not in the tree", id.raw()),
            Self::Unmeasurable(id) => {
                write!(f, "node {} has no measurable geometry", id.raw())
            }
            Self::Nested(id) => write!(
                f,
                "node {} is inside another target; place one subtree at a time",
                id.raw()
            ),
            Self::OutOfRange { field, detail } => write!(f, "{field}: {detail}"),
            Self::Singular(id) => write!(
                f,
                "node {} has a collapsed parent transform, so it has no placement",
                id.raw()
            ),
        }
    }
}

impl std::error::Error for LayoutError {}

/// Place `target` so its box is `buff` away from `reference`'s box in
/// `direction`, with the two centres aligned on the perpendicular axis.
///
/// This is Manim's `Mobject.next_to(reference, direction, buff)`
/// (`mobject.py:2042`) with the default `aligned_edge=ORIGIN`: the *near* edges
/// are separated by exactly `buff`, and the perpendicular coordinates are
/// centred on the reference's centre.
///
/// `buff` is a gap in world units, so it is what a viewer sees between the two
/// boxes. It may be negative — overlapping a label with its arrow is a real
/// design — and must be finite.
///
/// Returns the ids that actually moved; an already-correct placement returns an
/// empty list and writes nothing, so a diff reports no change.
pub fn next_to(
    tree: &mut SceneTree,
    target: NodeId,
    reference: NodeId,
    direction: Direction,
    buff: f64,
) -> Result<Vec<NodeId>, LayoutError> {
    finite("buff", buff)?;
    if target == reference {
        return Err(LayoutError::OutOfRange {
            field: "reference",
            detail: "a node cannot be placed next to itself".into(),
        });
    }
    let t = bounds(tree, target)?;
    let r = bounds(tree, reference)?;

    let mut delta = Vec2::ZERO;
    match direction {
        // Positive: the target's near edge sits at the reference's far edge
        // plus the gap. Negative: at the reference's near edge minus the gap.
        Direction::Right => delta.x = r.x1 + buff - t.x0,
        Direction::Left => delta.x = r.x0 - buff - t.x1,
        Direction::Up => delta.y = r.y1 + buff - t.y0,
        Direction::Down => delta.y = r.y0 - buff - t.y1,
    }
    let perp = direction.axis() ^ 1;
    let d = r.center() - t.center();
    if perp == 0 {
        delta.x = d.x;
    } else {
        delta.y = d.y;
    }
    Ok(shift_world(tree, target, delta)?.into_iter().collect())
}

/// Place `targets` in a row or column with equal gaps of `buff` world units,
/// centred where the group's box already was.
///
/// This is Manim's `VGroup.arrange(direction, buff)` (`mobject.py:1993`) with
/// `center=True`: the arrangement is centred on the original union box, so
/// rearranging a group in place does not move it across the frame.
///
/// Gaps, not widths, are equal — three objects of different sizes in a row have
/// the same space between neighbours, which is what reads as an arrangement.
/// The perpendicular coordinate of every target is the union box's centre, so a
/// row is a row even if its members started at different heights.
///
/// Equal ids are collapsed (first occurrence wins): placing one node twice in
/// the same arrangement is a caller mistake with no meaningful answer.
pub fn arrange(
    tree: &mut SceneTree,
    targets: &[NodeId],
    axis: Axis,
    buff: f64,
) -> Result<Vec<NodeId>, LayoutError> {
    finite("buff", buff)?;
    let targets = distinct(tree, targets)?;
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let boxes = targets
        .iter()
        .map(|&id| bounds(tree, id))
        .collect::<Result<Vec<_>, _>>()?;
    let union = boxes
        .iter()
        .copied()
        .reduce(|a, b| a.union(b))
        .unwrap_or(boxes[0]);
    let centre = union.center();

    // The arranged extent is the sum of the sizes plus one gap between each
    // pair; the group is kept centred on `centre` rather than anchored at the
    // old left edge, because that is the promise "rearrange in place" makes.
    let size = |r: &Rect| match axis {
        Axis::Row => r.width(),
        Axis::Column => r.height(),
    };
    let total: f64 = boxes.iter().map(size).sum::<f64>() + buff * (targets.len() as f64 - 1.0);
    let mut cursor = match axis {
        Axis::Row => centre.x - total / 2.0,
        Axis::Column => centre.y + total / 2.0,
    };

    let mut moved = Vec::new();
    for (id, b) in targets.iter().zip(&boxes) {
        let desired = match axis {
            Axis::Row => {
                let c = Point::new(cursor + b.width() / 2.0, centre.y);
                cursor += b.width() + buff;
                c
            }
            Axis::Column => {
                let c = Point::new(centre.x, cursor - b.height() / 2.0);
                cursor -= b.height() + buff;
                c
            }
        };
        if let Some(id) = place_centre(tree, *id, desired)? {
            moved.push(id);
        }
    }
    Ok(moved)
}

/// Scale `targets` uniformly about their common centre so the group fits
/// inside `width × height` world units.
///
/// This is Manim's `VGroup.scale_to_fit_width/height`
/// (`mobject.py:1613,1641`) collapsed into one operation: when both extents are
/// given the tighter axis decides the factor, so the group is *fitted*, not
/// stretched — a circle stays a circle. The group's centre does not move: a
/// uniform scale about it maps the box centre to itself.
///
/// A group with no extent along an axis is fitted by the other axis; one with
/// no extent at all is left alone. A factor of exactly `1.0` writes nothing, so
/// a fit that changes nothing reports nothing.
pub fn fit(
    tree: &mut SceneTree,
    targets: &[NodeId],
    width: f64,
    height: f64,
) -> Result<Vec<NodeId>, LayoutError> {
    positive("width", width)?;
    positive("height", height)?;
    let targets = distinct(tree, targets)?;
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let boxes = targets
        .iter()
        .map(|&id| bounds(tree, id))
        .collect::<Result<Vec<_>, _>>()?;
    let union = boxes
        .iter()
        .copied()
        .reduce(|a, b| a.union(b))
        .unwrap_or(boxes[0]);
    let centre = union.center();

    let mut factor = f64::INFINITY;
    if union.width() > 0.0 {
        factor = factor.min(width / union.width());
    }
    if union.height() > 0.0 {
        factor = factor.min(height / union.height());
    }
    if !factor.is_finite() {
        // No extent on either axis: nothing to scale, and no box to honour.
        return Ok(Vec::new());
    }
    if !factor.is_finite() || factor <= 0.0 {
        return Err(LayoutError::OutOfRange {
            field: "fit",
            detail: "the scale is not a positive, finite factor".into(),
        });
    }
    if factor == 1.0 {
        return Ok(Vec::new());
    }

    let mut moved = Vec::new();
    for id in targets {
        let pivot = tree
            .to_parent_space(id, centre)
            .ok_or(LayoutError::Singular(id))?;
        if let Some(m) = tree.get_mut(id) {
            m.scale_about(factor, pivot);
        }
        moved.push(id);
    }
    Ok(moved)
}

/// Collapse duplicates and refuse targets nested inside each other.
///
/// A duplicate would make `fit` scale one node twice; a nested pair would move
/// the child once with its parent and once on its own. Both are silent wrong
/// answers, and both are cheap to refuse.
fn distinct(tree: &SceneTree, targets: &[NodeId]) -> Result<Vec<NodeId>, LayoutError> {
    let mut out: Vec<NodeId> = Vec::with_capacity(targets.len());
    for &id in targets {
        if tree.get(id).is_none() {
            return Err(LayoutError::Unknown(id));
        }
        if out.contains(&id) {
            continue;
        }
        // Either order is a problem: the ancestor moves the descendant with it,
        // so whichever one is placed second finds a box the first already
        // changed — and `fit` would scale the pair by the factor twice.
        if out
            .iter()
            .any(|&other| tree.is_ancestor_of(other, id) || tree.is_ancestor_of(id, other))
        {
            return Err(LayoutError::Nested(id));
        }
        out.push(id);
    }
    Ok(out)
}

/// Move a node's world-space box centre to `centre`, if it is not already
/// there. Returns the id when a write happened.
fn place_centre(
    tree: &mut SceneTree,
    id: NodeId,
    centre: Point,
) -> Result<Option<NodeId>, LayoutError> {
    let delta = centre - bounds(tree, id)?.center();
    shift_world(tree, id, delta)
}

/// Compose a world-space translation onto a node's deferred transform.
///
/// The translation has to be expressed in the node's *parent* space, because
/// that is the space `Mobject::transform` maps from. For the common case — a
/// root node — the conversion is the identity; for a child of a rotated or
/// scaled group it is the inverse linear part, which is why
/// [`SceneTree::to_parent_space`] exists (and why it refuses a collapsed
/// parent instead of writing `NaN`).
///
/// Returns the id only when the delta was non-zero: writing an identical
/// transform would mark a `modified` id in a diff for a frame that did not
/// change.
fn shift_world(
    tree: &mut SceneTree,
    id: NodeId,
    delta: Vec2,
) -> Result<Option<NodeId>, LayoutError> {
    if delta == Vec2::ZERO {
        return Ok(None);
    }
    let a = tree
        .to_parent_space(id, Point::ZERO)
        .ok_or(LayoutError::Singular(id))?;
    let b = tree
        .to_parent_space(id, Point::ZERO + delta)
        .ok_or(LayoutError::Singular(id))?;
    let local = b - a;
    let Some(m) = tree.get_mut(id) else {
        return Err(LayoutError::Unknown(id));
    };
    m.set_transform(Affine2::translate(local) * m.transform);
    Ok(Some(id))
}

fn bounds(tree: &SceneTree, id: NodeId) -> Result<Rect, LayoutError> {
    if tree.get(id).is_none() {
        return Err(LayoutError::Unknown(id));
    }
    tree.world_bounds(id).ok_or(LayoutError::Unmeasurable(id))
}

fn finite(field: &'static str, value: f64) -> Result<(), LayoutError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(LayoutError::OutOfRange {
            field,
            detail: format!("{value} is not finite"),
        })
    }
}

fn positive(field: &'static str, value: f64) -> Result<(), LayoutError> {
    finite(field, value)?;
    if value > 0.0 {
        Ok(())
    } else {
        Err(LayoutError::OutOfRange {
            field,
            detail: format!("{value} is not a positive extent"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_mobject::{Mobject, ShapeKind, ShapeParams, build};

    /// A square of side `size` centred at `at`, inserted at the root.
    fn square(tree: &mut SceneTree, size: f64, at: (f64, f64)) -> NodeId {
        let params = ShapeParams {
            width: Some(size),
            height: Some(size),
            position: Some([at.0, at.1]),
            ..Default::default()
        };
        tree.insert(build(ShapeKind::Square, &params))
    }

    fn bounds_of(tree: &SceneTree, id: NodeId) -> Rect {
        tree.world_bounds(id).expect("a measurable node")
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-12, "{a} != {b}");
    }

    #[test]
    fn next_to_separates_the_near_edges_by_the_buff_on_every_side() {
        for direction in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            let mut tree = SceneTree::new();
            let reference = square(&mut tree, 2.0, (0.0, 0.0));
            let target = square(&mut tree, 1.0, (7.0, -5.0));

            let moved = next_to(&mut tree, target, reference, direction, 0.25).unwrap();
            assert_eq!(moved, [target]);

            let t = bounds_of(&tree, target);
            let r = bounds_of(&tree, reference);
            let gap = match direction {
                Direction::Right => t.x0 - r.x1,
                Direction::Left => r.x0 - t.x1,
                Direction::Up => t.y0 - r.y1,
                Direction::Down => r.y0 - t.y1,
            };
            close(gap, 0.25);

            // The perpendicular coordinate is centred on the reference, which
            // is what makes "to the right of it" read as beside it.
            match direction {
                Direction::Left | Direction::Right => close(t.center().y, r.center().y),
                Direction::Up | Direction::Down => close(t.center().x, r.center().x),
            }
        }
    }

    #[test]
    fn a_negative_buff_overlaps_and_a_correct_placement_does_not_write() {
        let mut tree = SceneTree::new();
        let reference = square(&mut tree, 2.0, (0.0, 0.0));
        let target = square(&mut tree, 1.0, (2.0, 0.0));

        // Already at exactly x0 = 1.5 with buff 0.5? No: x0 is 1.5 and r.x1 is
        // 1, so the gap is 0.5 already — a placement that changes nothing must
        // report nothing.
        let moved = next_to(&mut tree, target, reference, Direction::Right, 0.5).unwrap();
        assert!(moved.is_empty(), "{moved:?}");

        // A negative buff tucks the label into the shape's edge.
        next_to(&mut tree, target, reference, Direction::Right, -0.25).unwrap();
        close(bounds_of(&tree, target).x0, 0.75);
    }

    #[test]
    fn a_node_cannot_be_placed_next_to_itself() {
        let mut tree = SceneTree::new();
        let a = square(&mut tree, 1.0, (0.0, 0.0));
        assert!(matches!(
            next_to(&mut tree, a, a, Direction::Right, 0.2),
            Err(LayoutError::OutOfRange {
                field: "reference",
                ..
            })
        ));
    }

    #[test]
    fn arrange_keeps_equal_gaps_and_the_groups_original_centre() {
        let mut tree = SceneTree::new();
        let a = square(&mut tree, 1.0, (-4.0, 2.0));
        let b = square(&mut tree, 2.0, (0.0, -1.0));
        let c = square(&mut tree, 3.0, (4.0, 3.0));
        let before = [a, b, c]
            .iter()
            .map(|&id| bounds_of(&tree, id))
            .reduce(|a, b| a.union(b))
            .unwrap()
            .center();

        let moved = arrange(&mut tree, &[a, b, c], Axis::Row, 0.25).unwrap();
        assert_eq!(moved.len(), 3);

        let boxes: Vec<Rect> = [a, b, c].iter().map(|&id| bounds_of(&tree, id)).collect();
        close(boxes[1].x0 - boxes[0].x1, 0.25);
        close(boxes[2].x0 - boxes[1].x1, 0.25);
        for b in &boxes {
            close(b.center().y, before.y);
        }
        let after = boxes
            .iter()
            .copied()
            .reduce(|a, b| a.union(b))
            .unwrap()
            .center();
        close(after.x, before.x);
    }

    #[test]
    fn a_column_runs_from_the_first_target_at_the_top() {
        let mut tree = SceneTree::new();
        let a = square(&mut tree, 1.0, (-3.0, -2.0));
        let b = square(&mut tree, 2.0, (2.0, 0.0));
        let c = square(&mut tree, 1.0, (1.0, 2.0));

        arrange(&mut tree, &[a, b, c], Axis::Column, 0.1).unwrap();

        let (ba, bb, bc) = (
            bounds_of(&tree, a),
            bounds_of(&tree, b),
            bounds_of(&tree, c),
        );
        assert!(
            ba.center().y > bb.center().y,
            "the first target is the top one"
        );
        assert!(bb.center().y > bc.center().y);
        close(ba.y0 - bb.y1, 0.1);
        close(bb.y0 - bc.y1, 0.1);
        close(ba.center().x, bc.center().x);
    }

    #[test]
    fn arrange_collapses_a_repeated_id_instead_of_placing_it_twice() {
        let mut tree = SceneTree::new();
        let a = square(&mut tree, 1.0, (0.0, 0.0));
        let b = square(&mut tree, 1.0, (3.0, 0.0));
        let moved = arrange(&mut tree, &[a, a, b], Axis::Row, 0.5).unwrap();
        assert_eq!(moved, [a, b]);
        close(bounds_of(&tree, b).x0 - bounds_of(&tree, a).x1, 0.5);
    }

    #[test]
    fn fit_scales_by_the_tighter_axis_and_keeps_the_aspect_and_centre() {
        let mut tree = SceneTree::new();
        let a = square(&mut tree, 2.0, (-1.0, 0.0));
        let b = square(&mut tree, 2.0, (1.0, 0.0));
        let before = bounds_of(&tree, a).union(bounds_of(&tree, b));
        close(before.width(), 4.0);
        close(before.height(), 2.0);

        // A 2 × 2 box: the width decides, factor 0.5, and the result is
        // 2 × 1 — fitted inside, not stretched to fill.
        let moved = fit(&mut tree, &[a, b], 2.0, 2.0).unwrap();
        assert_eq!(moved.len(), 2);

        let after = bounds_of(&tree, a).union(bounds_of(&tree, b));
        close(after.width(), 2.0);
        close(after.height(), 1.0);
        close(after.center().x, before.center().x);
        close(after.center().y, before.center().y);
    }

    #[test]
    fn fit_of_an_already_fitting_group_writes_nothing() {
        let mut tree = SceneTree::new();
        let a = square(&mut tree, 1.0, (0.0, 0.0));
        assert!(fit(&mut tree, &[a], 1.0, 1.0).unwrap().is_empty());
        // An empty list is a no-op; a target with no box at all is refused
        // (`Unmeasurable`) rather than silently skipped — see the test below.
        assert!(fit(&mut tree, &[], 8.0, 8.0).unwrap().is_empty());
    }

    #[test]
    fn fit_refuses_a_box_with_no_extent_and_an_unmeasurable_target() {
        let mut tree = SceneTree::new();
        let a = square(&mut tree, 1.0, (0.0, 0.0));
        assert!(matches!(
            fit(&mut tree, &[a], 0.0, 2.0),
            Err(LayoutError::OutOfRange { field: "width", .. })
        ));
        assert!(matches!(
            fit(&mut tree, &[a], f64::NAN, 2.0),
            Err(LayoutError::OutOfRange { field: "width", .. })
        ));

        let pointless = tree.insert(Mobject::new(vec![]));
        assert_eq!(
            next_to(&mut tree, pointless, a, Direction::Right, 0.1),
            Err(LayoutError::Unmeasurable(pointless))
        );
    }

    #[test]
    fn a_nested_target_is_refused_because_it_would_move_twice() {
        let mut tree = SceneTree::new();
        let parent = tree.insert(Mobject::new(vec![]));
        let child = square(&mut tree, 1.0, (0.0, 0.0));
        assert!(tree.reparent(child, parent));
        let other = square(&mut tree, 1.0, (3.0, 0.0));

        assert_eq!(
            arrange(&mut tree, &[parent, child], Axis::Row, 0.2),
            Err(LayoutError::Nested(child))
        );
        assert_eq!(
            fit(&mut tree, &[child, parent], 4.0, 4.0),
            Err(LayoutError::Nested(parent))
        );
        let third = square(&mut tree, 1.0, (6.0, 0.0));
        assert!(arrange(&mut tree, &[other, third], Axis::Row, 0.2).is_ok());
    }

    #[test]
    fn a_child_of_a_moved_group_is_placed_by_its_world_box() {
        let mut tree = SceneTree::new();
        let group = tree.insert(Mobject::new(vec![]));
        let child = square(&mut tree, 1.0, (0.0, 0.0));
        assert!(tree.reparent(child, group));
        tree.get_mut(group)
            .unwrap()
            .set_transform(Affine2::translate((3.0, 4.0)));

        let reference = square(&mut tree, 2.0, (0.0, 0.0));
        // The child's world box is 2.5..3.5, so its centre is 3, not 0.
        close(bounds_of(&tree, child).center().x, 3.0);

        next_to(&mut tree, child, reference, Direction::Right, 0.25).unwrap();
        let t = bounds_of(&tree, child);
        close(t.x0, 1.25);
        close(t.center().y, 0.0);
        // The parent still owns the displacement: the child's local transform
        // carries the remainder and the group did not move.
        close(tree.get(group).unwrap().transform.as_coeffs()[4], 3.0);
    }

    #[test]
    fn a_child_of_a_scaled_parent_is_moved_by_a_world_delta() {
        let mut tree = SceneTree::new();
        let group = tree.insert(Mobject::new(vec![]));
        let child = square(&mut tree, 1.0, (0.0, 0.0));
        assert!(tree.reparent(child, group));
        tree.get_mut(group)
            .unwrap()
            .set_transform(Affine2::scale(2.0));

        // The child's world box is −1..1 wide, twice its local size.
        close(bounds_of(&tree, child).width(), 2.0);

        let reference = square(&mut tree, 1.0, (6.0, 0.0));
        next_to(&mut tree, child, reference, Direction::Right, 0.5).unwrap();

        let t = bounds_of(&tree, child);
        // The gap is a world-space gap even though the node lives in a space
        // scaled by two; the conversion is `to_parent_space`'s job.
        close(t.x0 - bounds_of(&tree, reference).x1, 0.5);
        close(t.width(), 2.0);
    }

    #[test]
    fn a_rotated_square_is_placed_by_the_box_a_viewer_sees() {
        let mut tree = SceneTree::new();
        let reference = square(&mut tree, 2.0, (0.0, 0.0));
        let target = square(&mut tree, 2.0, (5.0, 0.0));
        tree.get_mut(target)
            .unwrap()
            .rotate_about(std::f64::consts::FRAC_PI_4, Point::ZERO);

        // A square turned 45° has a box of side 2√2, and that is the extent
        // the gap is measured from.
        next_to(&mut tree, target, reference, Direction::Right, 0.0).unwrap();
        let t = bounds_of(&tree, target);
        close(t.width(), 2.0 * std::f64::consts::SQRT_2);
        close(t.x0, 1.0);
        // The rotation survived: the deferred transform was composed with a
        // translation, not replaced by one.
        close(t.height(), 2.0 * std::f64::consts::SQRT_2);
    }

    #[test]
    fn a_point_less_node_is_placed_by_its_declared_box() {
        let mut tree = SceneTree::new();
        let reference = square(&mut tree, 2.0, (0.0, 0.0));
        let text =
            tree.insert(Mobject::new(vec![]).with_local_bounds(Rect::new(-1.0, -0.5, 1.0, 0.5)));

        next_to(&mut tree, text, reference, Direction::Right, 0.5).unwrap();
        let t = bounds_of(&tree, text);
        close(t.x0, 1.5);
        close(t.center().y, 0.0);
        // The box is not a measurement of points; placement leaves its local
        // geometry alone and only composes a transform.
        assert!(tree.get(text).unwrap().points.is_empty());
    }

    #[test]
    fn an_unknown_node_is_a_value_not_a_panic() {
        let mut tree = SceneTree::new();
        let a = square(&mut tree, 1.0, (0.0, 0.0));
        let gone = square(&mut tree, 1.0, (1.0, 1.0));
        tree.remove(gone);

        assert_eq!(
            next_to(&mut tree, gone, a, Direction::Right, 0.1),
            Err(LayoutError::Unknown(gone))
        );
        assert_eq!(
            arrange(&mut tree, &[a, gone], Axis::Row, 0.1),
            Err(LayoutError::Unknown(gone))
        );
        assert_eq!(
            fit(&mut tree, &[gone, a], 4.0, 4.0),
            Err(LayoutError::Unknown(gone))
        );
    }
}
