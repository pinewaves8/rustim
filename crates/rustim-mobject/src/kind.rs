//! What a mobject *is*, as data, independent of what it looks like.
//!
//! `ShapeKind` says how to build a shape's points; `NodeKind` says what the
//! thing those points describe is, and is *stored* on the mobject rather than
//! inferred at draw time. That distinction is what the accessibility layer needs:
//! a reader has to be told "this is a circle of radius 1.5" even when the object
//! was moved, morphed or built by hand out of raw points.
//!
//! Two properties are deliberate.
//!
//! **`ShapeKind`'s three literals appear here with the same spelling and the same
//! JSON strings.** `方案建议.md` §4.6's fourth conclusion is that one set of names
//! serves three consumers — the `add` command, the accessibility tree and (later)
//! the DOM/ARIA target — so `"type":"square"` on the wire and
//! `{"name":"kind","value":"square"}` in a `set` command must be the same word,
//! not two enumerations that happen to agree today.
//!
//! **It is a flat enum, not `Shape(ShapeKind)`.** Under `rename_all = "snake_case"`
//! a newtype variant serialises as `{"shape":"square"}`, which is a third
//! spelling of the same word and would break the round-trip equality that the
//! command layer tests for.

use serde::{Deserialize, Serialize};

use crate::shapes::ShapeKind;

/// The semantic category of a mobject.
///
/// `Unknown` is the default because most construction paths do not know: a
/// mobject built from a raw point array by a scene, or by an animation that
/// produced new points, is legitimately unclassified. Reporting that honestly is
/// better than guessing a shape from the geometry, which would be wrong for
/// anything that has been morphed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    #[default]
    Unknown,
    /// The three shapes [`crate::shapes::build`] can construct.
    Square,
    Circle,
    RightTriangle,
    /// A container. Reserved: nothing builds one yet, `SceneTree::reparent` is
    /// the only way to make a group today.
    Group,
    Text,
    Heading,
    Formula,
    Axes,
    Plot,
}

impl From<ShapeKind> for NodeKind {
    fn from(kind: ShapeKind) -> Self {
        match kind {
            ShapeKind::Square => Self::Square,
            ShapeKind::Circle => Self::Circle,
            ShapeKind::RightTriangle => Self::RightTriangle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shape_kinds_keep_their_wire_names() {
        // The point of the flat enum: `ShapeKind::Square` and `NodeKind::Square`
        // are the same word on the wire.
        for kind in [
            ShapeKind::Square,
            ShapeKind::Circle,
            ShapeKind::RightTriangle,
        ] {
            assert_eq!(
                serde_json::to_string(&NodeKind::from(kind)).unwrap(),
                serde_json::to_string(&kind).unwrap(),
            );
        }
    }

    #[test]
    fn unknown_is_the_default_and_round_trips() {
        assert_eq!(NodeKind::default(), NodeKind::Unknown);
        for kind in [
            NodeKind::Unknown,
            NodeKind::Square,
            NodeKind::Circle,
            NodeKind::RightTriangle,
            NodeKind::Group,
            NodeKind::Text,
            NodeKind::Heading,
            NodeKind::Formula,
            NodeKind::Axes,
            NodeKind::Plot,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(serde_json::from_str::<NodeKind>(&json).unwrap(), kind);
        }
    }

    #[test]
    fn right_triangle_stays_two_words() {
        // The one name `rename_all` alone does not decide, spelled out because a
        // protocol that says "righttriangle" in one place and "right_triangle" in
        // another is exactly what this module exists to prevent.
        assert_eq!(
            serde_json::to_string(&NodeKind::RightTriangle).unwrap(),
            "\"right_triangle\""
        );
    }
}
