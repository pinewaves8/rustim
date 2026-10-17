//! Pointing and spotlighting: the two attention helpers a lesson asks for.
//!
//! *Point at this* and *dim the rest* are the same kind of sentence as *put the
//! caption under the triangle* (see [`crate::layout`]): they are computed from
//! the world boxes a viewer sees, so the result stays right when the subject is
//! rotated, nested or resized, and the caller never hands over a coordinate.
//!
//! The two functions here are the geometry half only. [`pointing_arrow`] builds
//! and places a fresh [`Mobject`] the command layer inserts; [`spotlight`] names
//! the nodes a spotlight must dim. Turning that list into a dim-and-restore
//! beat is animation composition, which belongs to `rustim-scene` (this crate
//! does not depend on `rustim-anim`, and should not start).

use rustim_math::{Affine2, Point};
use rustim_mobject::{Mobject, NodeId, SceneTree, ShapeKind, ShapeParams, shapes};

use crate::layout::{Direction, LayoutError};

/// The default gap between a pointing arrow's tip and its subject's box, in
/// world units — Manim's `SMALL_BUFF` (`constants.py:169`), the same number
/// [`crate::layout::next_to`] defaults to.
pub const DEFAULT_POINT_AT_BUFF: f64 = 0.1;

/// The default length of a pointing arrow, in world units.
///
/// One unit is an eighth of the default frame's height, so the arrow is a
/// pointer rather than a bar across the picture. There is no Manim constant to
/// follow here — Manim has no `point_at` — so the number is stated and the
/// caller can override it.
pub const DEFAULT_POINT_AT_LENGTH: f64 = 1.0;

/// Build an arrow that points at `target` from `direction`.
///
/// The **tip** sits `buff` away from the target's world bounding box on the
/// given side, and the shaft runs outward from there — so `Direction::Down`
/// means "below the subject, pointing up at it", the placement a caption arrow
/// uses. The arrow is a normal filled [`ShapeKind::Arrow`] mobject: its local
/// points run along `+x`, and the whole placement is one affine on its deferred
/// transform, so the result can be faded, moved or removed like any other node.
///
/// `buff` may be negative (an arrow whose tip overlaps the subject is a real
/// design, and `next_to` allows the same); `length` must be positive and finite,
/// and `thickness` — when given — finite and non-negative. Refusals are values,
/// like every other helper here: a missing node, one with no measurable box, or
/// a degenerate arrow is never a panic.
pub fn pointing_arrow(
    tree: &SceneTree,
    target: NodeId,
    direction: Direction,
    buff: f64,
    length: f64,
    thickness: Option<f64>,
) -> Result<Mobject, LayoutError> {
    if !buff.is_finite() {
        return Err(LayoutError::OutOfRange {
            field: "buff",
            detail: format!("{buff} is not a finite gap"),
        });
    }
    if !length.is_finite() || length <= 0.0 {
        return Err(LayoutError::OutOfRange {
            field: "length",
            detail: format!("{length} is not a positive, finite arrow length"),
        });
    }
    if let Some(t) = thickness
        && (!t.is_finite() || t < 0.0)
    {
        return Err(LayoutError::OutOfRange {
            field: "thickness",
            detail: format!("{t} is not a finite, non-negative thickness"),
        });
    }
    // Existence first, so a stale id is `Unknown` rather than the weaker
    // "nothing to measure" — the two are different bugs for a caller.
    if tree.get(target).is_none() {
        return Err(LayoutError::Unknown(target));
    }
    let bounds = tree
        .world_bounds(target)
        .ok_or(LayoutError::Unmeasurable(target))?;

    // Where the tip lands, and which way the shaft points. The local arrow runs
    // from the origin to `(length, 0)` with its tip at the far end, so the
    // rotation that sends `+x` toward the subject is the angle named here.
    let centre = bounds.center();
    let (tip, angle) = match direction {
        Direction::Left => (
            Point::new(bounds.x0 - buff, centre.y),
            0.0, // to the subject's left, pointing right (along +x)
        ),
        Direction::Right => (
            Point::new(bounds.x1 + buff, centre.y),
            std::f64::consts::PI, // pointing left
        ),
        Direction::Up => (
            Point::new(centre.x, bounds.y1 + buff),
            -std::f64::consts::FRAC_PI_2, // pointing down
        ),
        Direction::Down => (
            Point::new(centre.x, bounds.y0 - buff),
            std::f64::consts::FRAC_PI_2, // pointing up
        ),
    };

    let params = ShapeParams {
        width: Some(length),
        thickness,
        ..ShapeParams::default()
    };
    let mut arrow = shapes::build(ShapeKind::Arrow, &params);
    // `*` applies its right operand first. The arrow's local tip is at
    // `(length, 0)`, so `T(-length, 0)` first brings the tip to the origin,
    // then the rotation aims `+x` at the subject, then the translation puts the
    // tip exactly on `tip` — the gap is to the tip, not to the tail.
    arrow.transform = Affine2::translate((tip.x, tip.y))
        * Affine2::rotate(angle)
        * Affine2::translate((-length, 0.0));
    Ok(arrow)
}

/// The nodes a spotlight on `target` must dim: every picture-bearing node
/// outside the target's subtree, in draw order.
///
/// "Outside the subtree" includes the target's **ancestors**, and that is
/// deliberate: a group has no picture, so excluding it would cost nothing, but
/// a formula's root is a picture-bearing container over glyph children — if a
/// caller spotlights one glyph, the rest of the formula should dim with the
/// rest of the scene. Descendants of the target stay lit because the target is
/// what the viewer is being asked to look at.
///
/// The list is computed from the tree, not remembered: the caller captures the
/// opacities it needs before animating, so a node created later is simply not
/// part of this spotlight.
pub fn spotlight(tree: &SceneTree, target: NodeId) -> Result<Vec<NodeId>, LayoutError> {
    if tree.get(target).is_none() {
        return Err(LayoutError::Unknown(target));
    }
    Ok(tree
        .draw_order()
        .into_iter()
        .filter(|&id| {
            id != target
                && !tree.is_ancestor_of(target, id)
                && tree.get(id).is_some_and(|m| m.has_picture())
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_mobject::shapes::build;

    fn square(tree: &mut SceneTree, size: f64, at: (f64, f64)) -> NodeId {
        let params = ShapeParams {
            width: Some(size),
            height: Some(size),
            position: Some([at.0, at.1]),
            ..Default::default()
        };
        tree.insert(build(ShapeKind::Square, &params))
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-12, "{a} != {b}");
    }

    #[test]
    fn a_pointing_arrow_puts_its_tip_a_buff_from_every_side() {
        // A 2x2 square at the origin, a 1-unit arrow a 0.2 gap away: the tip is
        // the square's near edge plus the gap, and the tail is one length past
        // it, outward. The arrow always points *at* the subject.
        let cases = [
            (
                Direction::Left,
                Point::new(-1.2, 0.0),
                Point::new(-2.2, 0.0),
            ),
            (Direction::Right, Point::new(1.2, 0.0), Point::new(2.2, 0.0)),
            (Direction::Up, Point::new(0.0, 1.2), Point::new(0.0, 2.2)),
            (
                Direction::Down,
                Point::new(0.0, -1.2),
                Point::new(0.0, -2.2),
            ),
        ];
        for (direction, tip, tail) in cases {
            let mut tree = SceneTree::new();
            let subject = square(&mut tree, 2.0, (0.0, 0.0));
            let arrow = pointing_arrow(&tree, subject, direction, 0.2, 1.0, None).unwrap();

            let world_tip = arrow.transform * Point::new(1.0, 0.0);
            let world_tail = arrow.transform * Point::ZERO;
            close(world_tip.x, tip.x);
            close(world_tip.y, tip.y);
            close(world_tail.x, tail.x);
            close(world_tail.y, tail.y);
        }
    }

    #[test]
    fn a_pointing_arrow_is_a_normal_arrow_and_the_gap_may_overlap() {
        let mut tree = SceneTree::new();
        let subject = square(&mut tree, 2.0, (0.0, 0.0));
        // A negative buff puts the tip inside the subject's box: a legal design,
        // the same allowance `next_to` makes.
        let arrow = pointing_arrow(&tree, subject, Direction::Down, -0.5, 1.5, Some(0.1)).unwrap();
        assert_eq!(arrow.kind, rustim_mobject::NodeKind::Arrow);
        assert!(!arrow.points.is_empty());
        let world_tip = arrow.transform * Point::new(1.5, 0.0);
        close(world_tip.y, -1.0 + 0.5);
    }

    #[test]
    fn a_pointing_arrow_refuses_a_missing_subject_and_a_degenerate_arrow() {
        let mut tree = SceneTree::new();
        let subject = square(&mut tree, 2.0, (0.0, 0.0));
        let ghost = tree.insert(Mobject::new(vec![]));
        tree.remove(ghost);
        assert_eq!(
            pointing_arrow(&tree, ghost, Direction::Down, 0.1, 1.0, None),
            Err(LayoutError::Unknown(ghost))
        );

        // No points and no declared box: nothing to place an arrow around.
        let empty = tree.insert(Mobject::new(vec![]));
        assert_eq!(
            pointing_arrow(&tree, empty, Direction::Down, 0.1, 1.0, None),
            Err(LayoutError::Unmeasurable(empty))
        );

        for (buff, length, thickness, field) in [
            (f64::NAN, 1.0, None, "buff"),
            (0.1, 0.0, None, "length"),
            (0.1, f64::INFINITY, None, "length"),
            (0.1, 1.0, Some(-0.1), "thickness"),
        ] {
            match pointing_arrow(&tree, subject, Direction::Down, buff, length, thickness) {
                Err(LayoutError::OutOfRange { field: got, .. }) => assert_eq!(got, field),
                other => panic!("expected a refusal under {field}, got {other:?}"),
            }
        }
    }

    #[test]
    fn spotlight_lists_every_picture_outside_the_subject_subtree() {
        let mut tree = SceneTree::new();
        let a = square(&mut tree, 1.0, (-2.0, 0.0));
        let b = square(&mut tree, 1.0, (2.0, 0.0));
        // A group's own picture is its children, so the group itself is not in
        // the list and its circle is only dimmed when the group is not the subject.
        let group = tree.insert(Mobject::new(vec![]).with_kind(rustim_mobject::NodeKind::Group));
        let circle = tree.insert(build(ShapeKind::Circle, &ShapeParams::default()));
        assert!(tree.reparent(circle, group));

        assert_eq!(spotlight(&tree, group).unwrap(), [a, b]);
        assert_eq!(spotlight(&tree, a).unwrap(), [b, circle]);
        assert_eq!(
            spotlight(&tree, NodeId::from_raw(999)),
            Err(LayoutError::Unknown(NodeId::from_raw(999)))
        );
    }
}
