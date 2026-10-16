//! The AI/host command layer: the engine's public mutation surface as data.

use serde::{Deserialize, Serialize};

use rustim_mobject::shapes::{ShapeKind, ShapeParams};
use rustim_mobject::{Mobject, NodeId, NodeKind, SceneTree};

/// A mutation requested from outside the engine.
///
/// Internally tagged (`"action": "..."`) so the wire form matches the sketch in
/// `CLAUDE.md` and `需求探讨.txt`:
///
/// ```json
/// { "action": "add", "type": "triangle", "params": { "width": 3, "height": 4 } }
/// ```
///
/// Every variant that names an existing object uses `target`, and every one uses
/// the same field names as the JSON, so the serde attributes stay the only
/// translation layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Command {
    /// Create a shape and return its new id.
    Add {
        #[serde(rename = "type")]
        kind: ShapeKind,
        #[serde(default)]
        params: ShapeParams,
    },
    /// Set one named property of one object.
    Set { target: NodeId, property: Property },
    /// Delete an object and its subtree.
    Remove { target: NodeId },
    /// Play an animation that names its subject by id.
    Play { animation: AnimationSpec },
}

/// A named, settable property.
///
/// Named rather than a struct with every field optional, because that is what
/// makes "set the stroke width of item 7" expressible without the caller having
/// to round-trip the whole object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", content = "value", rename_all = "snake_case")]
pub enum Property {
    Opacity(f64),
    StrokeWidth(f64),
    FillOpacity(f64),
    Label(String),
    /// What the object is, for the accessibility tree and the AI layer.
    ///
    /// Serialises as `{"name":"kind","value":"heading"}` — the same vocabulary as
    /// `add`'s `"type"`, because a host that can create a square should be able to
    /// say that an existing object is one.
    Kind(NodeKind),
    Position([f64; 2]),
    Scale(f64),
    Rotation(f64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimationSpec {
    pub target: NodeId,
    #[serde(rename = "type")]
    pub kind: AnimationKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<[f64; 2]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnimationKind {
    FadeIn,
    MoveTo,
}

/// Why a command was refused.
///
/// A refusal is a value, not a panic. Every command here is reachable from a
/// language model, so "the node you named does not exist" and "that value is out
/// of range" have to be reportable and recoverable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum CommandError {
    NoSuchNode {
        target: NodeId,
    },
    /// A property value the engine cannot accept, with the range that would be.
    OutOfRange {
        property: String,
        detail: String,
    },
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchNode { target } => write!(f, "no such node: {target:?}"),
            Self::OutOfRange { property, detail } => {
                write!(f, "{property} out of range: {detail}")
            }
        }
    }
}

impl std::error::Error for CommandError {}

/// What a command changed.
///
/// Returned so a host can (a) learn the id of a newly added object and (b) tell
/// a renderer exactly what to invalidate, instead of redrawing everything. An
/// empty diff means the command was a no-op.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDiff {
    pub created: Vec<NodeId>,
    pub modified: Vec<NodeId>,
    pub removed: Vec<NodeId>,
}

impl StateDiff {
    pub fn is_empty(&self) -> bool {
        self.created.is_empty() && self.modified.is_empty() && self.removed.is_empty()
    }
}

/// Apply a command to the tree.
///
/// Pure with respect to everything except `tree`: no clock, no I/O, no renderer.
/// That is what makes the command layer testable in isolation and replayable.
pub fn apply(tree: &mut SceneTree, cmd: &Command) -> Result<StateDiff, CommandError> {
    let mut diff = StateDiff::default();
    match cmd {
        Command::Add { kind, params } => {
            let id = tree.insert(rustim_mobject::shapes::build(*kind, params));
            diff.created.push(id);
        }
        Command::Remove { target } => {
            if tree.get(*target).is_none() {
                return Err(CommandError::NoSuchNode { target: *target });
            }
            remove_subtree(tree, *target);
            diff.removed.push(*target);
        }
        Command::Set { target, property } => {
            let Some(m) = tree.get_mut(*target) else {
                return Err(CommandError::NoSuchNode { target: *target });
            };
            property.write(m)?;
            diff.modified.push(*target);
        }
        Command::Play { .. } => {
            // Animations are applied by the caller, which owns the scheduler;
            // this arm exists so the command round-trips and the error surface
            // is unchanged once it lands.
        }
    }
    Ok(diff)
}

fn remove_subtree(tree: &mut SceneTree, root: NodeId) {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if let Some(m) = tree.get(id) {
            stack.extend(m.children.iter().copied());
            if let Some(p) = m.parent
                && let Some(pm) = tree.get_mut(p)
            {
                pm.children.retain(|&c| c != id);
            }
        }
        tree.remove(id);
    }
}

impl Property {
    fn write(&self, m: &mut Mobject) -> Result<(), CommandError> {
        let range = |name: &str, v: f64| -> Result<f64, CommandError> {
            if (0.0..=1.0).contains(&v) {
                Ok(v)
            } else {
                Err(CommandError::OutOfRange {
                    property: name.to_string(),
                    detail: format!("{v} is not in 0.0..=1.0"),
                })
            }
        };
        match self {
            Self::Opacity(v) => {
                m.opacity = range("opacity", *v)?;
                m.dirty.style = true;
            }
            Self::FillOpacity(v) => {
                m.style.fill_opacity = range("fill_opacity", *v)?;
                m.dirty.style = true;
            }
            Self::StrokeWidth(v) => {
                if *v < 0.0 {
                    return Err(CommandError::OutOfRange {
                        property: "stroke_width".into(),
                        detail: format!("{v} is negative"),
                    });
                }
                m.style.stroke_width = *v;
                m.dirty.style = true;
            }
            // Both of these change what the object *means* and nothing about how
            // it looks, so they are the two writes that must set `dirty.semantic`
            // — a consumer that rebuilds only what `geometry | style | hierarchy`
            // marks would otherwise serve a stale accessibility tree forever.
            Self::Label(s) => {
                m.label = Some(s.clone());
                m.dirty.semantic = true;
            }
            Self::Kind(k) => {
                m.kind = *k;
                m.dirty.semantic = true;
            }
            Self::Position(p) => {
                m.set_transform(rustim_math::Affine::translate((p[0], p[1])));
            }
            Self::Scale(s) => {
                if *s <= 0.0 {
                    return Err(CommandError::OutOfRange {
                        property: "scale".into(),
                        detail: format!("{s} is not positive"),
                    });
                }
                m.set_transform(rustim_math::Affine::scale(*s));
            }
            Self::Rotation(r) => m.set_transform(rustim_math::Affine::rotate(*r)),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip_through_json() {
        // Ids are opaque 64-bit handles, so the literal in the JSON has to be a
        // real one to survive the round trip. Everything else in the protocol is
        // stable text, which is the property the AI layer depends on.
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(vec![])).raw();
        let cases = [
            format!(r#"{{"action":"remove","target":{id}}}"#),
            format!(
                r#"{{"action":"set","target":{id},"property":{{"name":"opacity","value":0.5}}}}"#
            ),
            r#"{"action":"add","type":"circle","params":{"radius":2.0}}"#.to_owned(),
        ];
        for json in cases {
            let cmd: Command =
                serde_json::from_str(&json).unwrap_or_else(|e| panic!("parsing {json}: {e}"));
            assert_eq!(serde_json::to_string(&cmd).unwrap(), json, "{json}");
        }
    }

    #[test]
    fn a_deleted_nodes_id_does_not_round_trip_onto_the_replacement() {
        // The generational key, seen from the protocol: reusing a slot must not
        // let an old id address the new occupant.
        let mut tree = SceneTree::new();
        let old = tree.insert(Mobject::new(vec![]));
        tree.remove(old);
        let new = tree.insert(Mobject::new(vec![]));
        assert_ne!(old.raw(), new.raw());
        assert!(tree.get(NodeId::from_raw(old.raw())).is_none());
    }

    #[test]
    fn add_returns_a_usable_id() {
        let mut tree = SceneTree::new();
        let cmd: Command = serde_json::from_str(
            r#"{"action":"add","type":"square","params":{"width":2.0,"label":"leg a"}}"#,
        )
        .unwrap();
        let diff = apply(&mut tree, &cmd).unwrap();
        assert_eq!(diff.created.len(), 1);
        let id = diff.created[0];
        assert_eq!(tree.get(id).unwrap().label.as_deref(), Some("leg a"));
    }

    #[test]
    fn commands_naming_a_dead_node_are_refused_not_panicking() {
        let mut tree = SceneTree::new();
        let ghost = tree.insert(Mobject::new(vec![]));
        tree.remove(ghost);
        let cmd = Command::Set {
            target: ghost,
            property: Property::Opacity(0.5),
        };
        assert_eq!(
            apply(&mut tree, &cmd),
            Err(CommandError::NoSuchNode { target: ghost })
        );
    }

    #[test]
    fn out_of_range_opacity_is_refused() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(vec![]));
        let cmd = Command::Set {
            target: id,
            property: Property::Opacity(7.0),
        };
        assert!(matches!(
            apply(&mut tree, &cmd),
            Err(CommandError::OutOfRange { .. })
        ));
        // ...and the object was not half-modified.
        assert_eq!(tree.get(id).unwrap().opacity, 1.0);
    }

    #[test]
    fn removing_a_parent_removes_its_children() {
        let mut tree = SceneTree::new();
        let parent = tree.insert(Mobject::new(vec![]));
        let child = tree.insert(Mobject::new(vec![]));
        assert!(tree.reparent(child, parent));
        apply(&mut tree, &Command::Remove { target: parent }).unwrap();
        assert!(tree.get(parent).is_none());
        assert!(tree.get(child).is_none(), "the subtree must go too");
    }

    #[test]
    fn a_no_op_command_reports_an_empty_diff() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(vec![]));
        let diff = apply(
            &mut tree,
            &Command::Play {
                animation: AnimationSpec {
                    target: id,
                    kind: AnimationKind::FadeIn,
                    duration: None,
                    to: None,
                },
            },
        )
        .unwrap();
        assert!(diff.is_empty());
    }
}
