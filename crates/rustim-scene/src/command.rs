//! The AI/host command layer: the engine's public mutation surface as data.

use serde::{Deserialize, Serialize};

use rustim_anim::{Animation, FadeIn, MoveTo};
use rustim_math::Affine2;
use rustim_mobject::shapes::{ShapeKind, ShapeParams};
use rustim_mobject::{Mobject, NodeId, NodeKind, SceneTree};

/// How long an animation runs when the command did not say.
///
/// Manim's `DEFAULT_ANIMATION_RUN_TIME` (`animation/animation.py:26`), which is
/// also what `Animation.__init__`'s `run_time` defaults to (`:132`). It matches
/// the defaults on `FadeIn`/`MoveTo` themselves, so a command that omits
/// `duration` and a scene that calls the constructor directly agree by
/// construction rather than by two people picking 1.0.
const DEFAULT_ANIMATION_RUN_TIME: f64 = 1.0;

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
            // `SceneContext::handle` answers this one, because playing is the one
            // command that needs the playback machine and this function is pure.
            // The arm stays so `apply` is still total over `Command` — a caller
            // driving `apply` directly gets the round-trip it always got, not a
            // panic.
        }
    }
    Ok(diff)
}

/// Build the animation a [`Command::Play`] names.
///
/// The wire→animation mapping lives here, beside the wire types, rather than in
/// `SceneContext`: the refusal vocabulary a language model sees (`NoSuchNode`,
/// `OutOfRange`) is this module's, and splitting the mapping across the two would
/// put half of one protocol's error surface in a scheduler's file.
///
/// Takes the tree even though it only reads it, because **a `Play` naming a node
/// that does not exist has to be refused rather than quietly playing nothing** —
/// the same reason `Remove` and `Set` look their target up before touching it.
/// `FadeIn` and `MoveTo` both degrade to a no-op on a missing target by design (a
/// deleted node may race an animation), but that posture is for an animation
/// *already running*; a command arriving now is a request that can still be
/// answered.
///
/// The refusals, each a value rather than a panic:
///
/// * a target that is not in the tree — [`CommandError::NoSuchNode`];
/// * `MoveTo` with no `to` — [`CommandError::OutOfRange`], because there is no
///   destination and silently doing nothing would teach a model that `to` is
///   optional when every `move_to` needs it;
/// * a negative or non-finite `duration` — [`CommandError::OutOfRange`]. `0.0` is
///   accepted: Manim refuses only `run_time < 0` (`animation/animation.py:175-181`)
///   and an instant snap is legitimate — `AnimationScheduler::update` already
///   defines it (`raw = 1.0` when `duration <= 0.0`).
///   Non-finite is refused here and *not* upstream, where `run_time = nan` is
///   accepted: an infinite one holds the clock forever and a NaN one makes
///   `age < duration` false, so the animation vanishes after a single frame.
///   Neither is a thing to answer a host with silence about.
pub fn animation_for(
    tree: &SceneTree,
    spec: &AnimationSpec,
) -> Result<Box<dyn Animation>, CommandError> {
    if tree.get(spec.target).is_none() {
        return Err(CommandError::NoSuchNode {
            target: spec.target,
        });
    }
    if let Some(d) = spec.duration
        && (!d.is_finite() || d < 0.0)
    {
        return Err(CommandError::OutOfRange {
            property: "duration".into(),
            detail: format!("{d} is not a finite, non-negative number of seconds"),
        });
    }
    let duration = spec.duration.unwrap_or(DEFAULT_ANIMATION_RUN_TIME);

    Ok(match spec.kind {
        AnimationKind::FadeIn => Box::new(FadeIn::new(spec.target).with_duration(duration)),
        AnimationKind::MoveTo => {
            let Some([x, y]) = spec.to else {
                return Err(CommandError::OutOfRange {
                    property: "to".into(),
                    detail: "a move_to needs a destination".into(),
                });
            };
            let to = Affine2::translate((x, y));
            Box::new(MoveTo::new(spec.target, to).with_duration(duration))
        }
    })
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
            // The animation specs round-trip too, which is what lets a host log and
            // replay a session: `range`'s absent-field rule holds for `duration`
            // and `to` as well, so neither is written back as `null`.
            format!(
                r#"{{"action":"play","animation":{{"target":{id},"type":"fade_in","duration":0.6}}}}"#
            ),
            format!(
                r#"{{"action":"play","animation":{{"target":{id},"type":"move_to","to":[1.5,-2.0]}}}}"#
            ),
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

    /// A tree with one object, whose opacity starts at zero so that fading it in is
    /// observable. `FadeIn::begin` captures `from = m.opacity`, so a fresh mobject —
    /// opaque by default — would make this a no-op with a duration.
    fn scene_with_a_blank() -> (SceneTree, NodeId) {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(rustim_mobject::shapes::square(2.0, 2.0)));
        tree.get_mut(id).unwrap().opacity = 0.0;
        (tree, id)
    }

    fn spec(id: NodeId, kind: AnimationKind) -> AnimationSpec {
        AnimationSpec {
            target: id,
            kind,
            duration: None,
            to: None,
        }
    }

    /// The mapping itself: the wire spec becomes an animation that runs for the
    /// stated time and reaches the right end state.
    #[test]
    fn a_fade_in_command_builds_a_fade_in() {
        let (mut tree, id) = scene_with_a_blank();
        let mut what = spec(id, AnimationKind::FadeIn);
        what.duration = Some(0.25);

        let mut anim = animation_for(&tree, &what).unwrap();
        assert_eq!(anim.duration(), 0.25);
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().opacity, 1.0);
    }

    /// The destination is applied as a *transform*, the same way
    /// `Property::Position` applies one — so the animation interpolates toward it
    /// rather than snapping to it.
    #[test]
    fn a_move_to_command_moves_to_the_given_point() {
        let (mut tree, id) = scene_with_a_blank();
        let mut what = spec(id, AnimationKind::MoveTo);
        what.to = Some([3.0, -4.0]);

        let mut anim = animation_for(&tree, &what).unwrap();
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);
        assert_eq!(
            tree.get(id).unwrap().transform,
            Affine2::translate((3.0, -4.0))
        );
    }

    /// A duration the command did not give is Manim's `DEFAULT_ANIMATION_RUN_TIME`,
    /// which is also what `FadeIn::new`/`MoveTo::new` pick — so a command and a
    /// scene calling the constructor agree without either restating 1.0.
    #[test]
    fn an_omitted_duration_is_manims_default() {
        let (tree, id) = scene_with_a_blank();
        assert_eq!(DEFAULT_ANIMATION_RUN_TIME, 1.0);
        for kind in [AnimationKind::FadeIn, AnimationKind::MoveTo] {
            let mut what = spec(id, kind);
            if kind == AnimationKind::MoveTo {
                what.to = Some([0.0, 0.0]);
            }
            assert_eq!(animation_for(&tree, &what).unwrap().duration(), 1.0);
        }
    }

    /// A `Play` naming a node that is not there is refused, like `Set` and
    /// `Remove`. The animations themselves degrade to no-ops on a missing target by
    /// design — that posture is for one already running — but a command arriving now
    /// is a request that can still be answered, and answering it with silence
    /// teaches a model that the id it invented was fine.
    #[test]
    fn playing_a_missing_node_is_refused() {
        let mut tree = SceneTree::new();
        let ghost = tree.insert(Mobject::new(vec![]));
        tree.remove(ghost);
        // `matches!` rather than `assert_eq!(.., Err(..))`: the `Ok` arm is a
        // `Box<dyn Animation>`, which is neither `Debug` nor `PartialEq`, so the
        // comparison has to be one-sided.
        assert!(matches!(
            animation_for(&tree, &spec(ghost, AnimationKind::FadeIn)),
            Err(CommandError::NoSuchNode { target }) if target == ghost
        ));
    }

    /// A `move_to` with no `to` has no destination, and the refusal says so rather
    /// than playing nothing.
    #[test]
    fn a_move_to_without_a_destination_is_refused() {
        let (tree, id) = scene_with_a_blank();
        assert!(matches!(
            animation_for(&tree, &spec(id, AnimationKind::MoveTo)),
            Err(CommandError::OutOfRange { property, .. }) if property == "to"
        ));
    }

    /// `duration: 0.0` is a legitimate instant snap and is accepted — Manim refuses
    /// only `run_time < 0` (`animation/animation.py:175-181`). Negative is refused,
    /// and so is non-finite, which Manim *does* accept and which has no honest
    /// answer: an infinite duration holds the clock forever, and a NaN one makes
    /// `age < duration` false, so the animation vanishes after a single frame.
    #[test]
    fn a_duration_that_is_negative_or_not_a_number_is_refused() {
        let (tree, id) = scene_with_a_blank();
        let with = |d: f64| {
            let mut what = spec(id, AnimationKind::FadeIn);
            what.duration = Some(d);
            animation_for(&tree, &what)
        };

        assert!(with(0.0).is_ok(), "an instant snap is legitimate");
        for bad in [-1.0, f64::INFINITY, f64::NAN] {
            assert!(
                matches!(
                    with(bad),
                    Err(CommandError::OutOfRange { ref property, .. }) if property == "duration"
                ),
                "{bad} must be refused"
            );
        }
    }
}
