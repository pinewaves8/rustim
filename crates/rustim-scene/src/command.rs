//! The AI/host command layer: the engine's public mutation surface as data.

use serde::{Deserialize, Serialize};

use rustim_anim::{Animation, FadeIn, FadeOut, MoveTo, Rotate, Scale};
use rustim_math::{Affine2, Point};
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
    /// Where a `move_to` ends up, in world units.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<[f64; 2]>,
    /// How far a `rotate` turns, in radians.
    ///
    /// Required by `rotate` and refused when absent, unlike Manim — see
    /// [`animation_for`]'s doc for why the π/4 default is not one to reproduce.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub angle: Option<f64>,
    /// What a `scale` multiplies the size by. Must be positive and finite.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub factor: Option<f64>,
    /// A `rotate`'s pivot, in **world** units. Absent means the object's own
    /// centre, which is what Manim's `Rotate` defaults to as well.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub about_point: Option<[f64; 2]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnimationKind {
    FadeIn,
    FadeOut,
    MoveTo,
    Rotate,
    Scale,
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
    /// The request was not a command at all — whoever read the wire could not
    /// turn it into one, and the transport's parser says why.
    ///
    /// **Nothing here constructs it.** `apply` never sees text, and this crate
    /// has no JSON parser on purpose (it compiles to wasm and the browser build
    /// reaches the engine through the same commands, from a host that does own
    /// one). It is part of *this* vocabulary anyway, because a host reading a
    /// reply stream has to tell "the engine refused what I asked for" from "the
    /// engine could not read what I sent" — and one error type on the wire is
    /// simpler for that host than two.
    Unparseable {
        detail: String,
    },
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The opaque integer, not the `Debug` of the id. `NodeId`'s internal
            // spelling is `NodeId(999v1)` — slot and generation — which is
            // neither what a host sent nor anything it could act on, and this
            // string is what a refusal says to a human. The wire form is the
            // number, so the message uses the number.
            Self::NoSuchNode { target } => write!(f, "no such node: {}", target.raw()),
            Self::OutOfRange { property, detail } => {
                write!(f, "{property} out of range: {detail}")
            }
            Self::Unparseable { detail } => write!(f, "not a command: {detail}"),
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

/// What a host gets back for one line of a command feed.
///
/// **Exactly one of `diff` and `error` is present, and the presence is the
/// discriminant.** The absent one is omitted rather than written as `null`, the
/// same rule the commands themselves follow — these are a model's output too.
///
/// A host that sent a `play` gets a `diff` that is present and *empty*: queueing
/// a beat changes nothing in the tree, and saying so is more useful than saying
/// nothing. A host that sent an `add` gets the new id in `created`, which is the
/// whole reason this type exists — it is the only way to learn the id of what
/// was just created, and without it nothing could be done to that object
/// afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandReply {
    /// The 1-based *physical* line the command came from, so a human reading a
    /// session log and a host reading the stream count the same way.
    pub seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<StateDiff>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<CommandError>,
}

impl CommandReply {
    /// The reply to a command that was applied, or queued as a beat.
    pub fn applied(seq: u64, diff: StateDiff) -> Self {
        Self {
            seq,
            diff: Some(diff),
            error: None,
        }
    }

    /// The reply to a command that was not — refused by the engine, or not
    /// readable as a command at all.
    pub fn refused(seq: u64, error: CommandError) -> Self {
        Self {
            seq,
            diff: None,
            error: Some(error),
        }
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
            // The walk lives on the tree rather than here, because `FadeOut`'s
            // finish step needs exactly the same one — and two copies of "delete
            // this subtree" would eventually disagree about children.
            tree.remove_subtree(*target);
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
/// * `Rotate` with no `angle` — [`CommandError::OutOfRange`]. Manim's own default
///   is `TAU / 4` (`animation/rotation.py:161`), and reproducing it would mean a
///   bare `{"type":"rotate"}` turning the object a quarter turn. A model that left
///   the angle out did not mean π/2; it meant to fill it in, and being told so is
///   cheaper than a scene that turns things nobody asked to turn. A non-finite
///   angle is refused for the same reason a non-finite duration is.
/// * `Scale` with a `factor` that is not positive and finite —
///   [`CommandError::OutOfRange`]. `0` collapses the object to a point where
///   nothing can reach it again and a negative one mirrors it, neither of which is
///   what "scale it" means to say. The same posture as [`Property::Scale`], which
///   refuses the same values.
/// * a negative or non-finite `duration` — [`CommandError::OutOfRange`]. `0.0` is
///   accepted: Manim refuses only `run_time < 0` (`animation/animation.py:175-181`)
///   and an instant snap is legitimate — `AnimationScheduler::update` already
///   defines it (`raw = 1.0` when `duration <= 0.0`).
///   Non-finite is refused here and *not* upstream, where `run_time = nan` is
///   accepted: an infinite one holds the clock forever and a NaN one makes
///   `age < duration` false, so the animation vanishes after a single frame.
///   Neither is a thing to answer a host with silence about.
///
/// `to` and `about_point` are plain coordinate pairs and are not range-checked,
/// matching each other: they arrive from JSON, which cannot carry a non-finite
/// float, and every finite point is a legitimate one.
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
        AnimationKind::FadeOut => Box::new(FadeOut::new(spec.target).with_duration(duration)),
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
        AnimationKind::Rotate => {
            let Some(angle) = spec.angle else {
                return Err(CommandError::OutOfRange {
                    property: "angle".into(),
                    detail: "a rotate needs an angle in radians".into(),
                });
            };
            if !angle.is_finite() {
                return Err(CommandError::OutOfRange {
                    property: "angle".into(),
                    detail: format!("{angle} is not a finite number of radians"),
                });
            }
            let mut anim = Rotate::new(spec.target, angle).with_duration(duration);
            if let Some([x, y]) = spec.about_point {
                anim = anim.about_point(Point::new(x, y));
            }
            Box::new(anim)
        }
        AnimationKind::Scale => {
            let Some(factor) = spec.factor else {
                return Err(CommandError::OutOfRange {
                    property: "factor".into(),
                    detail: "a scale needs a factor to multiply the size by".into(),
                });
            };
            if !factor.is_finite() || factor <= 0.0 {
                return Err(CommandError::OutOfRange {
                    property: "factor".into(),
                    detail: format!("{factor} is not a positive, finite scale factor"),
                });
            }
            Box::new(Scale::new(spec.target, factor).with_duration(duration))
        }
    })
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
            // Absolute, and deliberately the odd one out among the three
            // transform properties: "put it at (2, 3)" is a statement about where
            // the object *is*, while scale and rotation below are operations
            // applied to where it already is. Manim's `move_to` is absolute in the
            // same way, so a plain `set` and a `play` of `move_to` agree.
            Self::Position(p) => {
                m.set_transform(rustim_math::Affine::translate((p[0], p[1])));
            }
            // Both of these **compose** onto the object's transform rather than
            // replacing it, which is the one behavioural change in P2 step 5.
            // `set_transform(scale(s))` threw the object's position away: "move it
            // to the corner, then make it twice as big" silently teleported it back
            // to the origin, and a caller who scaled after moving had no way to see
            // why. Manim's `scale`/`rotate` are in-place too, and both take a pivot
            // that defaults to the object's own centre — `critical_point((0, 0))`,
            // whose `None` (a mobject with no measurable geometry, such as text)
            // falls back to the parent-space origin, since an object with no
            // extent has no other centre to speak of.
            Self::Scale(s) => {
                if *s <= 0.0 || !s.is_finite() {
                    return Err(CommandError::OutOfRange {
                        property: "scale".into(),
                        detail: format!("{s} is not a positive, finite factor"),
                    });
                }
                let pivot = m.critical_point((0.0, 0.0)).unwrap_or(Point::new(0.0, 0.0));
                m.scale_about(*s, pivot);
            }
            Self::Rotation(r) => {
                if !r.is_finite() {
                    return Err(CommandError::OutOfRange {
                        property: "rotation".into(),
                        detail: format!("{r} is not a finite number of radians"),
                    });
                }
                let pivot = m.critical_point((0.0, 0.0)).unwrap_or(Point::new(0.0, 0.0));
                m.rotate_about(*r, pivot);
            }
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
            // The three P2 step 5 kinds. `rotate`'s optional pivot is written only
            // when the caller gave one, so both spellings have to round-trip.
            format!(
                r#"{{"action":"play","animation":{{"target":{id},"type":"fade_out","duration":0.6}}}}"#
            ),
            format!(
                r#"{{"action":"play","animation":{{"target":{id},"type":"rotate","angle":1.2}}}}"#
            ),
            format!(
                r#"{{"action":"play","animation":{{"target":{id},"type":"rotate","angle":1.2,"about_point":[1.0,-2.0]}}}}"#
            ),
            format!(
                r#"{{"action":"play","animation":{{"target":{id},"type":"scale","factor":2.5}}}}"#
            ),
        ];
        for json in cases {
            let cmd: Command =
                serde_json::from_str(&json).unwrap_or_else(|e| panic!("parsing {json}: {e}"));
            assert_eq!(serde_json::to_string(&cmd).unwrap(), json, "{json}");
        }
    }

    /// The other half of the wire: what comes *back* has to round-trip too, and
    /// has to keep the "absent, never null" rule — a host that echoed a reply
    /// into a transcript must get the same bytes out of it.
    #[test]
    fn replies_round_trip_through_json() {
        let cases = [
            r#"{"seq":1,"diff":{"created":[],"modified":[],"removed":[]}}"#.to_owned(),
            r#"{"seq":2,"diff":{"created":[4294967296],"modified":[],"removed":[]}}"#.to_owned(),
            r#"{"seq":3,"error":{"error":"no_such_node","target":4294967299}}"#.to_owned(),
            r#"{"seq":4,"error":{"error":"out_of_range","property":"opacity","detail":"1.5 is not in 0.0..=1.0"}}"#
                .to_owned(),
            r#"{"seq":5,"error":{"error":"unparseable","detail":"expected `,` at line 1 column 12"}}"#
                .to_owned(),
        ];
        for json in cases {
            let reply: CommandReply =
                serde_json::from_str(&json).unwrap_or_else(|e| panic!("parsing {json}: {e}"));
            assert_eq!(serde_json::to_string(&reply).unwrap(), json, "{json}");
        }
    }

    /// The two constructors are the only way to build one, so neither can
    /// accidentally produce a reply carrying both halves or neither.
    #[test]
    fn a_reply_carries_exactly_one_half() {
        let ok = CommandReply::applied(7, StateDiff::default());
        assert!(ok.diff.is_some() && ok.error.is_none());
        assert!(!serde_json::to_string(&ok).unwrap().contains("error"));

        let no = CommandReply::refused(
            8,
            CommandError::Unparseable {
                detail: "nope".into(),
            },
        );
        assert!(no.diff.is_none() && no.error.is_some());
        assert!(!serde_json::to_string(&no).unwrap().contains("null"));
    }

    /// An unreadable line is a *refusal*, not a panic, and it reads as one to a
    /// human in the log.
    #[test]
    fn an_unparseable_command_displays_as_such() {
        let e = CommandError::Unparseable {
            detail: "expected value".into(),
        };
        assert_eq!(e.to_string(), "not a command: expected value");
    }

    /// A refusal names the id the way the wire does.
    ///
    /// `NodeId`'s `Debug` is `NodeId(999v1)` — slot and generation, the *internal*
    /// spelling, which is neither what a host sent nor anything it could act on.
    /// This string is what a live session's `command refused:` line prints, so it
    /// has to be a number somebody can find in their own script.
    ///
    /// Note the number is not the `999` that went in: `KeyData::from_ffi`
    /// normalises the generation to `… | 1`, so `from_raw(999)` is `4294968295`
    /// on the way out. The property this test can hold is that the message and
    /// the wire agree — both are `raw()`, and a host is never shown one and
    /// answered in the other.
    #[test]
    fn a_refusal_names_the_id_the_way_the_wire_does() {
        let target = NodeId::from_raw(999);
        let e = CommandError::NoSuchNode { target };
        assert_eq!(e.to_string(), format!("no such node: {}", target.raw()));
        assert!(
            !e.to_string().contains("NodeId"),
            "the internal spelling must not leak into a message a user reads"
        );
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            format!(r#"{{"error":"no_such_node","target":{}}}"#, target.raw()),
        );
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
                    angle: None,
                    factor: None,
                    about_point: None,
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

    /// A spec with everything but its kind left absent — the shape a model
    /// produces when it names an animation and forgets its parameters, which is
    /// the case three of the refusals below are about.
    fn spec(id: NodeId, kind: AnimationKind) -> AnimationSpec {
        AnimationSpec {
            target: id,
            kind,
            duration: None,
            to: None,
            angle: None,
            factor: None,
            about_point: None,
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
    /// which is also what the animations' own constructors pick — so a command and
    /// a scene calling the constructor agree without either restating 1.0.
    #[test]
    fn an_omitted_duration_is_manims_default() {
        let (tree, id) = scene_with_a_blank();
        assert_eq!(DEFAULT_ANIMATION_RUN_TIME, 1.0);
        // Every kind, so a new one cannot quietly acquire a *second* default.
        for kind in [
            AnimationKind::FadeIn,
            AnimationKind::FadeOut,
            AnimationKind::MoveTo,
            AnimationKind::Rotate,
            AnimationKind::Scale,
        ] {
            let mut what = spec(id, kind);
            match kind {
                AnimationKind::MoveTo => what.to = Some([0.0, 0.0]),
                AnimationKind::Rotate => what.angle = Some(1.0),
                AnimationKind::Scale => what.factor = Some(1.5),
                _ => {}
            }
            assert_eq!(
                animation_for(&tree, &what).unwrap().duration(),
                1.0,
                "{kind:?}"
            );
        }
    }

    // ---- the three P2 step 5 kinds ----------------------------------------

    #[test]
    fn a_fade_out_command_builds_a_fade_out() {
        let (mut tree, id) = scene_with_a_blank();
        tree.get_mut(id).unwrap().opacity = 1.0;

        let mut what = spec(id, AnimationKind::FadeOut);
        what.duration = Some(0.25);
        let mut anim = animation_for(&tree, &what).unwrap();
        assert_eq!(anim.duration(), 0.25);
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().opacity, 0.0);
    }

    /// The angle is required, and the refusal says so. Manim's own default is
    /// `TAU / 4` (`animation/rotation.py:161`), which is not one to reproduce: a
    /// bare `{"type":"rotate"}` turning the object a quarter turn is a surprise,
    /// and a model that left the angle out was going to fill it in.
    #[test]
    fn a_rotate_command_needs_an_angle() {
        let (tree, id) = scene_with_a_blank();
        assert!(matches!(
            animation_for(&tree, &spec(id, AnimationKind::Rotate)),
            Err(CommandError::OutOfRange { property, .. }) if property == "angle"
        ));
    }

    #[test]
    fn a_scale_command_needs_a_positive_factor() {
        let (tree, id) = scene_with_a_blank();
        assert!(matches!(
            animation_for(&tree, &spec(id, AnimationKind::Scale)),
            Err(CommandError::OutOfRange { property, .. }) if property == "factor"
        ));
        for bad in [0.0, -2.0, f64::INFINITY, f64::NAN] {
            let mut what = spec(id, AnimationKind::Scale);
            what.factor = Some(bad);
            assert!(
                matches!(
                    animation_for(&tree, &what),
                    Err(CommandError::OutOfRange { ref property, .. }) if property == "factor"
                ),
                "factor {bad} must be refused"
            );
        }
    }

    #[test]
    fn a_non_finite_angle_is_refused() {
        let (tree, id) = scene_with_a_blank();
        for bad in [f64::INFINITY, f64::NAN] {
            let mut what = spec(id, AnimationKind::Rotate);
            what.angle = Some(bad);
            assert!(
                matches!(
                    animation_for(&tree, &what),
                    Err(CommandError::OutOfRange { ref property, .. }) if property == "angle"
                ),
                "angle {bad} must be refused"
            );
        }
    }

    /// The settled rotation is the angle that was asked for, about the object's
    /// own centre when no pivot was given.
    #[test]
    fn a_rotate_command_lands_on_the_named_angle() {
        let (mut tree, id) = scene_with_a_blank();
        let mut what = spec(id, AnimationKind::Rotate);
        what.angle = Some(std::f64::consts::FRAC_PI_2);

        let mut anim = animation_for(&tree, &what).unwrap();
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        // A centred square's pivot is the origin, so the stored transform is the
        // bare rotation.
        assert_eq!(
            tree.get(id).unwrap().transform,
            Affine2::rotate(std::f64::consts::FRAC_PI_2)
        );
    }

    /// An explicit pivot is a world point, and it stays where it was put.
    #[test]
    fn a_rotate_command_honours_its_about_point() {
        let (mut tree, id) = scene_with_a_blank();
        tree.get_mut(id)
            .unwrap()
            .set_transform(Affine2::translate((3.0, 0.0)));
        let mut what = spec(id, AnimationKind::Rotate);
        what.angle = Some(std::f64::consts::FRAC_PI_2);
        what.about_point = Some([3.0, 0.0]);

        let mut anim = animation_for(&tree, &what).unwrap();
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        // The local point the pivot sits on has not moved; the square has turned.
        let m = tree.get(id).unwrap();
        let fixed = m.transform * Point::new(0.0, 0.0);
        assert!(
            (fixed.x - 3.0).abs() < 1e-12 && fixed.y.abs() < 1e-12,
            "{fixed:?}"
        );
        let corner = m.transform * Point::new(1.0, 1.0);
        assert!(
            (corner.x - 2.0).abs() < 1e-12 && (corner.y - 1.0).abs() < 1e-12,
            "{corner:?}"
        );
    }

    /// `Scale` is `ScaleInPlace`: the centre does not move and the extent grows by
    /// exactly the factor.
    #[test]
    fn a_scale_command_is_in_place() {
        let (mut tree, id) = scene_with_a_blank();
        tree.get_mut(id)
            .unwrap()
            .set_transform(Affine2::translate((3.0, 0.0)));
        let before = tree.world_bounds(id).unwrap().width();

        let mut what = spec(id, AnimationKind::Scale);
        what.factor = Some(2.0);
        let mut anim = animation_for(&tree, &what).unwrap();
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        let after = tree.world_bounds(id).unwrap().width();
        assert!((after - 2.0 * before).abs() < 1e-9, "{before} -> {after}");
        // The centre is the pivot, so it is exactly where the move put it.
        let m = tree.get(id).unwrap();
        let centre = m.transform * Point::new(0.0, 0.0);
        assert!(
            (centre.x - 3.0).abs() < 1e-12 && centre.y.abs() < 1e-12,
            "{centre:?}"
        );
    }

    /// **The behavioural change of P2 step 5, pinned.**
    ///
    /// `set`'s transform properties used to *replace* the object's transform, so
    /// "move it, then rotate it" teleported the object back to the origin. They now
    /// compose. Both halves are asserted, because either alone is satisfiable by
    /// the wrong implementation: a rotation that kept the position but did not
    /// rotate, and one that rotated but threw the position away.
    #[test]
    fn setting_rotation_composes_with_the_transform() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(rustim_mobject::shapes::square(2.0, 2.0)));
        tree.get_mut(id)
            .unwrap()
            .set_transform(Affine2::translate((5.0, 0.0)));

        apply(
            &mut tree,
            &Command::Set {
                target: id,
                property: Property::Rotation(std::f64::consts::FRAC_PI_2),
            },
        )
        .unwrap();

        let m = tree.get(id).unwrap();
        let centre = m.critical_point((0.0, 0.0)).unwrap();
        assert!(
            (centre.x - 5.0).abs() < 1e-12 && centre.y.abs() < 1e-12,
            "the move was discarded: the centre is {centre:?}"
        );
        let corner = m.transform * Point::new(1.0, 1.0);
        assert!(
            (corner.x - 4.0).abs() < 1e-12 && (corner.y - 1.0).abs() < 1e-12,
            "but it did not turn: {corner:?}"
        );
    }

    #[test]
    fn setting_scale_composes_with_the_transform() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(rustim_mobject::shapes::square(2.0, 2.0)));
        tree.get_mut(id)
            .unwrap()
            .set_transform(Affine2::translate((5.0, 0.0)));

        apply(
            &mut tree,
            &Command::Set {
                target: id,
                property: Property::Scale(3.0),
            },
        )
        .unwrap();

        let m = tree.get(id).unwrap();
        let centre = m.critical_point((0.0, 0.0)).unwrap();
        assert!(
            (centre.x - 5.0).abs() < 1e-12 && centre.y.abs() < 1e-12,
            "scaling about its own centre must not move it: {centre:?}"
        );
        let corner = m.transform * Point::new(1.0, 1.0);
        assert!(
            (corner.x - 8.0).abs() < 1e-12 && (corner.y - 3.0).abs() < 1e-12,
            "a corner one unit from the centre is now three: {corner:?}"
        );
    }

    /// A non-finite factor on the `set` path is refused for the same reason it is
    /// on the animation path — and the object is not half-modified.
    #[test]
    fn a_non_finite_scale_property_is_refused() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(rustim_mobject::shapes::square(2.0, 2.0)));
        let before = tree.get(id).unwrap().transform;
        for bad in [f64::NAN, f64::INFINITY, 0.0] {
            assert!(
                matches!(
                    apply(
                        &mut tree,
                        &Command::Set {
                            target: id,
                            property: Property::Scale(bad),
                        }
                    ),
                    Err(CommandError::OutOfRange { ref property, .. }) if property == "scale"
                ),
                "scale {bad} must be refused"
            );
            assert_eq!(tree.get(id).unwrap().transform, before);
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
