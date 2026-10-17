//! The AI/host command layer: the engine's public mutation surface as data.

use serde::{Deserialize, Serialize};

use rustim_anim::{
    Animation, AnimationGroup, Create, FadeIn, FadeOut, MoveTo, Rotate, Scale, Succession, Uncreate,
};
use rustim_math::{Affine2, Camera, Point, Viewport};
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
    /// Point the camera, now or over a beat.
    ///
    /// All four fields are optional and omitted when absent, like every other
    /// payload here. Without `duration` the camera is written immediately;
    /// with one (including `0.0`) a `CameraMove` beat is queued. `focus` frames
    /// an object by its world bounding box; see [`camera_for`] for how `focus`,
    /// `center` and `zoom` combine and for the refusals.
    Camera {
        #[serde(skip_serializing_if = "Option::is_none")]
        center: Option<[f64; 2]>,
        #[serde(skip_serializing_if = "Option::is_none")]
        zoom: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        focus: Option<NodeId>,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration: Option<f64>,
    },
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
    /// The stroke's own opacity, `0.0..=1.0` — independent of the fill's.
    ///
    /// Turning this up on an object that was built filled (a square, a circle)
    /// draws an outline around it; on a line or a plot — which `build` marks as
    /// stroked and unfilled — it is the only thing that makes the object
    /// visible at all, so `add` sets it to `1.0` for those two kinds.
    StrokeOpacity(f64),
    /// The stroke's colour as `[r, g, b]`; the alpha is `stroke_opacity`, and
    /// the fill keeps its own colour. `[u8; 3]` is the same spelling `add`'s
    /// `params.color` uses, so a host that can create a blue shape can recolour
    /// its outline.
    StrokeColor([u8; 3]),
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
    /// The node this animation acts on.
    ///
    /// **Optional**, and absent for `group` and `succession`: those name their
    /// members in `animations` and have no target of their own — the group is a
    /// timeline, not a node. Making it optional was the honest spelling: the
    /// alternative was making a host invent an id that the animation would ignore,
    /// which teaches a model that a meaningless field is part of the protocol. A
    /// non-group kind with no target is a refusal, not a parse error, so the model
    /// gets told what is missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<NodeId>,
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
    /// A `group`'s or `succession`'s member animations, each a spec of its own.
    ///
    /// Nested rather than flat because a group is a tree: a member may itself be a
    /// group, and a flat list would need a second structure saying which member
    /// belongs to which parent. `serde_json`'s own recursion limit bounds the
    /// depth, and `animation_for` walks each node once, so a hostile log cannot do
    /// worse than a deep tree it had to write out in full.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animations: Option<Vec<AnimationSpec>>,
    /// A `group`'s or `succession`'s stagger, as a fraction of the previous
    /// member's duration. Absent means Manim's default for the kind: `0.0` for
    /// `group`, `1.0` for `succession`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lag_ratio: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnimationKind {
    FadeIn,
    FadeOut,
    MoveTo,
    Rotate,
    Scale,
    /// Trace the object's path on from its first point.
    Create,
    /// Erase the path from its end and remove the object.
    Uncreate,
    /// Play the `animations` list in parallel, staggered by `lag_ratio`.
    Group,
    /// Play the `animations` list one after another, as one animation.
    Succession,
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
        Command::Camera { .. } => {
            // Likewise answered by `SceneContext::handle`: resolving `focus` needs
            // the viewport's aspect ratio, and applying the result is either a
            // write to `tree.camera` or a queued beat. The arm keeps `apply` total.
        }
    }
    Ok(diff)
}

/// Resolve the camera a [`Command::Camera`] describes.
///
/// Called from `SceneContext::handle` rather than `apply`, for the two reasons
/// the arm above gives: the fit needs the viewport (only its aspect ratio; the
/// absolute pixel size cancels), and "now or as a beat" is [`camera_duration`]'s
/// question, not the tree's.
///
/// **Precedence**, in this order: start from the current camera; `focus` replaces
/// both the centre and the zoom with the framed bounding box; an explicit
/// `center` then replaces only the centre; an explicit `zoom` replaces only the
/// zoom. So `{"focus": id, "zoom": 2.0}` frames the object and then doubles it,
/// and `{"zoom": 2.0}` alone zooms in on whatever the camera already centres on.
///
/// **Fit** leaves a `1.2` margin: the subject's box, enlarged by a fifth, must
/// fit in the frame on both axes, so the tighter axis decides the zoom. A box
/// with one zero dimension (a horizontal line, a vertical one) is fitted by the
/// other axis alone rather than refused; a box with no extent at all is refused,
/// because there is no scale at which it fills the frame.
///
/// Refusals, each a value rather than a panic — these are model output too:
///
/// * none of `center`, `zoom`, `focus` given — [`CommandError::OutOfRange`]: the
///   command named nothing to change, and answering it with silence would teach
///   a model that an empty camera request is fine;
/// * a `focus` naming a missing node — [`CommandError::NoSuchNode`], the same
///   refusal every other command that names a node gives;
/// * a `focus` naming a node with no geometry (a text mobject has no points; see
///   `Mobject::has_picture`) or with zero extent —
///   [`CommandError::OutOfRange`] rather than a silent no-op;
/// * a `zoom` that is not positive and finite — [`CommandError::OutOfRange`],
///   the same posture as `Scale`: a zero zoom divides by zero, a negative one
///   mirrors the world, and neither is what "zoom in" means.
///
/// `center` is not range-checked, matching `move_to`'s `to`: JSON cannot carry a
/// non-finite float and every finite point is a legitimate centre.
pub fn camera_for(
    tree: &SceneTree,
    viewport: &Viewport,
    center: Option<[f64; 2]>,
    zoom: Option<f64>,
    focus: Option<NodeId>,
) -> Result<Camera, CommandError> {
    if center.is_none() && zoom.is_none() && focus.is_none() {
        return Err(CommandError::OutOfRange {
            property: "camera".into(),
            detail: "a camera command needs a centre, a zoom or a focus".into(),
        });
    }

    let mut at = tree.camera.center;
    let mut scale = tree.camera.zoom;

    if let Some(target) = focus {
        if tree.get(target).is_none() {
            return Err(CommandError::NoSuchNode { target });
        }
        let Some(bounds) = tree.world_bounds(target) else {
            return Err(CommandError::OutOfRange {
                property: "focus".into(),
                detail: format!("node {} has no geometry to frame", target.raw()),
            });
        };
        let (w, h) = (bounds.width(), bounds.height());
        if w <= 0.0 && h <= 0.0 {
            return Err(CommandError::OutOfRange {
                property: "focus".into(),
                detail: format!("node {} has no extent to frame", target.raw()),
            });
        }
        at = (bounds.x0 + w / 2.0, bounds.y0 + h / 2.0);
        // visible height is WORLD_HEIGHT / zoom, and visible width is that times
        // the frame's aspect ratio, so each axis imposes `zoom <= ceiling` and
        // the smaller ceiling is the fit. A zero-width axis imposes no ceiling.
        const MARGIN: f64 = 1.2;
        let aspect = f64::from(viewport.frame_width) / f64::from(viewport.frame_height);
        let mut fit = f64::INFINITY;
        if w > 0.0 {
            fit = fit.min(Camera::WORLD_HEIGHT * aspect / (MARGIN * w));
        }
        if h > 0.0 {
            fit = fit.min(Camera::WORLD_HEIGHT / (MARGIN * h));
        }
        // `fit` is finite and positive here: one of the two terms was taken, and
        // both are positive for a positive dimension.
        if fit.is_finite() && fit > 0.0 {
            scale = fit;
        }
    }

    if let Some([x, y]) = center {
        at = (x, y);
    }
    if let Some(z) = zoom {
        if !z.is_finite() || z <= 0.0 {
            return Err(CommandError::OutOfRange {
                property: "zoom".into(),
                detail: format!("{z} is not a positive, finite zoom factor"),
            });
        }
        scale = z;
    }

    Ok(Camera::new(at, scale))
}

/// Validate a camera command's `duration`, and say whether there is one.
///
/// `None` means "write the camera now"; `Some(d)` means "queue a beat of `d`
/// seconds", including `0.0`, which the scheduler already defines as an instant
/// snap inside that beat. The accepted range is [`animation_for`]'s, for the same
/// reasons: negative is refused, `0.0` is legitimate, and a non-finite duration
/// is refused even though Manim accepts it, because an infinite one holds the
/// clock forever and a NaN one makes the beat vanish after a frame.
pub fn camera_duration(duration: Option<f64>) -> Result<Option<f64>, CommandError> {
    if let Some(d) = duration
        && (!d.is_finite() || d < 0.0)
    {
        return Err(CommandError::OutOfRange {
            property: "duration".into(),
            detail: format!("{d} is not a finite, non-negative number of seconds"),
        });
    }
    Ok(duration)
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
/// * a kind other than `group`/`succession` with **no** `target` —
///   [`CommandError::OutOfRange`]. Every animation but a composition acts on
///   exactly one node, and "which node?" is a question the reply can answer;
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
    match spec.kind {
        AnimationKind::Group | AnimationKind::Succession => return composed_for(tree, spec),
        _ => {}
    }
    let Some(target) = spec.target else {
        return Err(CommandError::OutOfRange {
            property: "target".into(),
            detail: format!(
                "a {:?} names the node it acts on; only group and succession name none",
                spec.kind
            )
            .to_lowercase(),
        });
    };
    if tree.get(target).is_none() {
        return Err(CommandError::NoSuchNode { target });
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
        // Answered above, before the target is required.
        AnimationKind::Group | AnimationKind::Succession => unreachable!(),
        AnimationKind::FadeIn => Box::new(FadeIn::new(target).with_duration(duration)),
        AnimationKind::FadeOut => Box::new(FadeOut::new(target).with_duration(duration)),
        AnimationKind::MoveTo => {
            let Some([x, y]) = spec.to else {
                return Err(CommandError::OutOfRange {
                    property: "to".into(),
                    detail: "a move_to needs a destination".into(),
                });
            };
            let to = Affine2::translate((x, y));
            Box::new(MoveTo::new(target, to).with_duration(duration))
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
            let mut anim = Rotate::new(target, angle).with_duration(duration);
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
            Box::new(Scale::new(target, factor).with_duration(duration))
        }
        AnimationKind::Create => Box::new(Create::new(target).with_duration(duration)),
        AnimationKind::Uncreate => Box::new(Uncreate::new(target).with_duration(duration)),
    })
}

/// Build a `group` or a `succession`: the half of [`animation_for`] that names no
/// node of its own.
///
/// The refusals are the ones a tree-shaped spec makes possible, and each is a
/// message a model can act on:
///
/// * a `target`, which a group does not have — refused rather than ignored, because
///   a silently dropped field teaches the model to keep sending it;
/// * no `animations`, or an empty list — Manim raises `ValueError` for the same
///   case (`composition.py:114-118`) and a beat that plays nothing is worse than a
///   refusal: the driver waits for it to drain;
/// * a `lag_ratio` that is negative or non-finite. Manim accepts any number, but a
///   non-finite one poisons every start time with `NaN` and a negative one starts a
///   member before the one it is meant to follow — neither is a stagger;
/// * a bad `duration`, by the same rule [`animation_for`] applies to every kind.
///
/// A member is itself validated by recursing through [`animation_for`], so a bad
/// node id inside a group is reported as `NoSuchNode` naming *that* id.
fn composed_for(
    tree: &SceneTree,
    spec: &AnimationSpec,
) -> Result<Box<dyn Animation>, CommandError> {
    if spec.target.is_some() {
        return Err(CommandError::OutOfRange {
            property: "target".into(),
            detail: "a group names its members in `animations`, not a node of its own".into(),
        });
    }
    let Some(members) = spec.animations.as_deref() else {
        return Err(CommandError::OutOfRange {
            property: "animations".into(),
            detail: "a group needs at least one member animation".into(),
        });
    };
    if members.is_empty() {
        return Err(CommandError::OutOfRange {
            property: "animations".into(),
            detail: "a group with no members plays nothing".into(),
        });
    }
    let lag = match (spec.kind, spec.lag_ratio) {
        (_, Some(r)) => r,
        // Manim's own defaults: `AnimationGroup` 0 and `Succession` 1.
        (AnimationKind::Succession, None) => 1.0,
        (_, None) => 0.0,
    };
    if !lag.is_finite() || lag < 0.0 {
        return Err(CommandError::OutOfRange {
            property: "lag_ratio".into(),
            detail: format!("{lag} is not a non-negative, finite fraction"),
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

    let mut children = Vec::with_capacity(members.len());
    for member in members {
        children.push(animation_for(tree, member)?);
    }
    Ok(match spec.kind {
        AnimationKind::Group => {
            let mut group = AnimationGroup::new(children).with_lag_ratio(lag);
            if let Some(d) = spec.duration {
                group = group.with_duration(d);
            }
            Box::new(group)
        }
        AnimationKind::Succession => {
            let mut succession = Succession::new(children).with_lag_ratio(lag);
            if let Some(d) = spec.duration {
                succession = succession.with_duration(d);
            }
            Box::new(succession)
        }
        // Only reachable through `animation_for`, which dispatches these two here.
        _ => unreachable!("composed_for is called for group and succession only"),
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
                if *v < 0.0 || !v.is_finite() {
                    return Err(CommandError::OutOfRange {
                        property: "stroke_width".into(),
                        detail: format!("{v} is not a non-negative, finite width"),
                    });
                }
                m.style.stroke_width = *v;
                m.dirty.style = true;
            }
            Self::StrokeOpacity(v) => {
                m.style.stroke_opacity = range("stroke_opacity", *v)?;
                m.dirty.style = true;
            }
            Self::StrokeColor([r, g, b]) => {
                // Same construction `shapes::build` uses for `params.color`, so
                // an `add` followed by a `set` can produce the identical style.
                m.style.stroke = rustim_color::Srgb::from_rgba8(*r, *g, *b, 255);
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
            // The three coordinate-system kinds, whose parameters add new
            // optional fields to `ShapeParams`. Each is spelled with only the
            // fields that shape reads, so a round trip that leaked the others as
            // `null` would fail here.
            r#"{"action":"add","type":"number_line","params":{"width":6.0,"range":[-3.0,3.0],"step":1.0,"tick_size":0.15}}"#
                .to_owned(),
            r#"{"action":"add","type":"axes","params":{"width":7.0,"height":4.0,"x_range":[-4.0,4.0],"y_range":[-2.0,2.0],"step":1.0}}"#
                .to_owned(),
            r#"{"action":"add","type":"plot","params":{"thickness":0.05,"points":[[0.0,0.0],[1.0,1.0],[2.0,0.0]]}}"#
                .to_owned(),
            // Stroke knobs. The colour's `[u8; 3]` is the same spelling
            // `params.color` uses, and the alpha never appears in the colour
            // itself — one alpha per channel, in its own property.
            format!(
                r#"{{"action":"set","target":{id},"property":{{"name":"stroke_opacity","value":0.5}}}}"#
            ),
            format!(
                r#"{{"action":"set","target":{id},"property":{{"name":"stroke_color","value":[100,181,246]}}}}"#
            ),
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
            // The two kinds that carry no parameter beyond the target: a `create`
            // and an `uncreate` are spelled with the target and nothing else.
            format!(r#"{{"action":"play","animation":{{"target":{id},"type":"create"}}}}"#),
            format!(
                r#"{{"action":"play","animation":{{"target":{id},"type":"uncreate","duration":0.7}}}}"#
            ),
            // A composed animation is a tree: no target of its own, members of
            // their own kind, and a lag. Both defaults have to survive too — a
            // `succession` with no `lag_ratio` must not gain one on the way out.
            format!(
                r#"{{"action":"play","animation":{{"type":"group","animations":[{{"target":{id},"type":"fade_in"}},{{"target":{id},"type":"rotate","angle":1.2}}],"lag_ratio":0.2}}}}"#
            ),
            r#"{"action":"play","animation":{"type":"succession","duration":2.0,"animations":[{"target":4294967296,"type":"create"},{"target":4294967296,"type":"fade_out","duration":0.5}]}}"#
                .to_owned(),
            // The camera command, in its full and partial spellings. The full
            // one exercises every optional field at once; the partial one is what
            // a host sends to "zoom in", and it has to survive without gaining
            // three `null`s.
            format!(
                r#"{{"action":"camera","center":[1.5,-2.0],"zoom":2.0,"focus":{id},"duration":0.8}}"#
            ),
            r#"{"action":"camera","zoom":2.0}"#.to_owned(),
            r#"{"action":"camera","center":[0.0,0.0]}"#.to_owned(),
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

    /// The three stroke properties reach `Style` and mark it dirty.
    #[test]
    fn stroke_properties_write_the_style_they_name() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(vec![]));
        for property in [
            Property::StrokeOpacity(0.25),
            Property::StrokeColor([100, 181, 246]),
            Property::StrokeWidth(0.08),
        ] {
            apply(
                &mut tree,
                &Command::Set {
                    target: id,
                    property,
                },
            )
            .unwrap();
        }

        let m = tree.get(id).unwrap();
        assert_eq!(m.style.stroke_opacity, 0.25);
        assert_eq!(m.style.stroke_width, 0.08);
        let [r, g, b, a] = m.style.stroke.components;
        assert!((r - 100.0 / 255.0).abs() < 1e-6, "{r}");
        assert!((g - 181.0 / 255.0).abs() < 1e-6, "{g}");
        assert!((b - 246.0 / 255.0).abs() < 1e-6, "{b}");
        assert_eq!(a, 1.0, "alpha lives in stroke_opacity, not in the colour");
        assert!(m.dirty.style, "the draw list has to be rebuilt");
    }

    #[test]
    fn a_stroke_opacity_out_of_range_is_refused() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(vec![]));
        for bad in [1.5, -0.1, f64::NAN] {
            let cmd = Command::Set {
                target: id,
                property: Property::StrokeOpacity(bad),
            };
            assert!(matches!(
                apply(&mut tree, &cmd),
                Err(CommandError::OutOfRange { .. })
            ));
        }
        assert_eq!(tree.get(id).unwrap().style.stroke_opacity, 0.0);
    }

    #[test]
    fn a_negative_or_non_finite_stroke_width_is_refused() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(vec![]));
        for bad in [-0.1, f64::NAN, f64::INFINITY] {
            let cmd = Command::Set {
                target: id,
                property: Property::StrokeWidth(bad),
            };
            assert!(matches!(
                apply(&mut tree, &cmd),
                Err(CommandError::OutOfRange { .. })
            ));
        }
        // A zero width is legitimate: it is how a host turns a stroke off
        // without touching its opacity or colour.
        apply(
            &mut tree,
            &Command::Set {
                target: id,
                property: Property::StrokeWidth(0.0),
            },
        )
        .unwrap();
        assert_eq!(tree.get(id).unwrap().style.stroke_width, 0.0);
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
                    target: Some(id),
                    kind: AnimationKind::FadeIn,
                    duration: None,
                    to: None,
                    angle: None,
                    factor: None,
                    about_point: None,
                    animations: None,
                    lag_ratio: None,
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
            target: Some(id),
            kind,
            duration: None,
            to: None,
            angle: None,
            factor: None,
            about_point: None,
            animations: None,
            lag_ratio: None,
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
        // Every single-node kind, so a new one cannot quietly acquire a *second*
        // default. `group` and `succession` are absent on purpose: their default
        // duration is the timeline their members lay out, not one second.
        for kind in [
            AnimationKind::FadeIn,
            AnimationKind::FadeOut,
            AnimationKind::MoveTo,
            AnimationKind::Rotate,
            AnimationKind::Scale,
            AnimationKind::Create,
            AnimationKind::Uncreate,
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

    // ---- the P3 create/uncreate kinds --------------------------------------

    /// A `create` draws the object's own path on, and at the end it is the whole
    /// object again — the command layer's only job is the mapping, so this asserts
    /// that the animation it builds is genuinely a prefix-of-the-point-array one.
    #[test]
    fn a_create_command_draws_the_path_on() {
        let (mut tree, id) = scene_with_a_blank();
        let whole = tree.get(id).unwrap().points.len();
        let mut what = spec(id, AnimationKind::Create);
        what.duration = Some(0.4);

        let mut anim = animation_for(&tree, &what).unwrap();
        assert_eq!(anim.duration(), 0.4);
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);
        let half = tree.get(id).unwrap().points.len();
        assert!(half < whole, "{half} of {whole} at alpha 0.5");
        anim.interpolate(1.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().points.len(), whole);
    }

    /// An `uncreate` erases the path and then **removes the object**, which is
    /// what `finish` is for. A command that built it without the removal hook
    /// would leave a fully transparent node in the tree and in `--inspect`.
    #[test]
    fn an_uncreate_command_erases_and_removes_the_object() {
        let (mut tree, id) = scene_with_a_blank();
        let mut what = spec(id, AnimationKind::Uncreate);
        what.duration = Some(0.4);

        let mut anim = animation_for(&tree, &what).unwrap();
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);
        assert!(tree.get(id).is_some(), "half-way is not gone yet");
        anim.interpolate(1.0, &mut tree);
        anim.finish(&mut tree);
        assert!(tree.get(id).is_none(), "the object outlived its uncreate");
    }

    // ---- the P3 composition kinds -----------------------------------------

    /// A spec that names no node and carries a member list: the shape a model
    /// produces for `{"type":"group",...}`.
    fn composed(kind: AnimationKind, members: Vec<AnimationSpec>) -> AnimationSpec {
        AnimationSpec {
            target: None,
            kind,
            duration: None,
            to: None,
            angle: None,
            factor: None,
            about_point: None,
            animations: Some(members),
            lag_ratio: None,
        }
    }

    /// The timeline, as the one number the wire can carry: `max(end)`, and the
    /// stagger really did move it. Both lag values are asserted so the difference
    /// cannot be an accident of the member durations.
    #[test]
    fn a_group_command_builds_a_composed_animation() {
        let (tree, id) = scene_with_a_blank();
        let one = |seconds: f64| {
            let mut s = spec(id, AnimationKind::FadeIn);
            s.duration = Some(seconds);
            s
        };

        let group = composed(AnimationKind::Group, vec![one(2.0), one(2.0)]);
        assert_eq!(animation_for(&tree, &group).unwrap().duration(), 2.0);

        let mut staggered = group.clone();
        staggered.lag_ratio = Some(0.5);
        assert_eq!(animation_for(&tree, &staggered).unwrap().duration(), 3.0);

        // `succession`'s default is Manim's 1.0, so two members simply add up.
        let succession = composed(AnimationKind::Succession, vec![one(1.0), one(2.0)]);
        assert_eq!(animation_for(&tree, &succession).unwrap().duration(), 3.0);
    }

    /// Not just the duration: the members really are at different points of their
    /// own curves at the same moment, which is the feature.
    #[test]
    fn a_group_command_staggers_its_members() {
        let mut tree = SceneTree::new();
        let a = tree.insert(Mobject::new(rustim_mobject::shapes::square(2.0, 2.0)));
        let b = tree.insert(Mobject::new(rustim_mobject::shapes::square(2.0, 2.0)));
        for id in [a, b] {
            tree.get_mut(id).unwrap().opacity = 0.0;
        }
        let fade = |id| {
            let mut s = spec(id, AnimationKind::FadeIn);
            s.duration = Some(1.0);
            s
        };
        let mut what = composed(AnimationKind::Group, vec![fade(a), fade(b)]);
        what.lag_ratio = Some(0.5);

        let mut anim = animation_for(&tree, &what).unwrap();
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree); // t = 0.75 of a 1.5 s group

        // The group's own rate is linear, so the members are 0.75 and 0.25 of the
        // way through their own curves — and each eases itself from there.
        assert_eq!(
            tree.get(a).unwrap().opacity,
            rustim_anim::rate::ease_in_out(0.75)
        );
        assert_eq!(
            tree.get(b).unwrap().opacity,
            rustim_anim::rate::ease_in_out(0.25)
        );
    }

    #[test]
    fn a_composition_command_names_no_target() {
        let (tree, id) = scene_with_a_blank();
        let mut what = composed(AnimationKind::Group, vec![spec(id, AnimationKind::FadeIn)]);
        what.target = Some(id);
        assert!(matches!(
            animation_for(&tree, &what),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "target"
        ));
    }

    #[test]
    fn a_composition_command_needs_members() {
        let (tree, _) = scene_with_a_blank();
        let mut missing = composed(AnimationKind::Group, vec![]);
        missing.animations = None;
        assert!(matches!(
            animation_for(&tree, &missing),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "animations"
        ));
        assert!(matches!(
            animation_for(&tree, &composed(AnimationKind::Succession, vec![])),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "animations"
        ));
    }

    #[test]
    fn a_composition_command_refuses_a_bad_lag_ratio() {
        let (tree, id) = scene_with_a_blank();
        for bad in [-0.5, f64::NAN, f64::INFINITY] {
            let mut what = composed(AnimationKind::Group, vec![spec(id, AnimationKind::FadeIn)]);
            what.lag_ratio = Some(bad);
            assert!(
                matches!(
                    animation_for(&tree, &what),
                    Err(CommandError::OutOfRange { ref property, .. }) if property == "lag_ratio"
                ),
                "lag_ratio {bad} must be refused"
            );
        }
    }

    /// A member is validated by the same function, so the refusal names the id the
    /// member gave — not the group, which has none.
    #[test]
    fn a_composition_command_validates_its_members() {
        let (mut tree, id) = scene_with_a_blank();
        let ghost = tree.insert(Mobject::new(vec![]));
        tree.remove(ghost);
        let what = composed(
            AnimationKind::Succession,
            vec![
                spec(id, AnimationKind::FadeIn),
                spec(ghost, AnimationKind::FadeIn),
            ],
        );
        assert!(matches!(
            animation_for(&tree, &what),
            Err(CommandError::NoSuchNode { target }) if target == ghost
        ));
    }

    #[test]
    fn a_single_node_kind_needs_a_target() {
        let (tree, _) = scene_with_a_blank();
        let mut what = spec(NodeId::from_raw(1), AnimationKind::FadeIn);
        what.target = None;
        assert!(matches!(
            animation_for(&tree, &what),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "target"
        ));
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

    // ---- the camera -------------------------------------------------------

    fn tree_with_a_square() -> (SceneTree, NodeId) {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(rustim_mobject::shapes::square(2.0, 2.0)));
        (tree, id)
    }

    #[test]
    fn a_camera_command_needs_a_focus_center_or_zoom() {
        let tree = SceneTree::new();
        assert!(matches!(
            camera_for(&tree, &Viewport::default(), None, None, None),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "camera"
        ));
    }

    /// A zoom of zero divides by zero and a negative one mirrors the world; both
    /// are refused the way `Scale` refuses them.
    #[test]
    fn a_non_positive_zoom_is_refused() {
        let tree = SceneTree::new();
        for bad in [0.0, -1.0, f64::INFINITY, f64::NAN] {
            assert!(
                matches!(
                    camera_for(&tree, &Viewport::default(), None, Some(bad), None),
                    Err(CommandError::OutOfRange { ref property, .. }) if property == "zoom"
                ),
                "zoom {bad} must be refused"
            );
        }
    }

    #[test]
    fn a_focus_on_an_unknown_node_is_refused() {
        let mut tree = SceneTree::new();
        let ghost = tree.insert(Mobject::new(vec![]));
        tree.remove(ghost);
        assert!(matches!(
            camera_for(&tree, &Viewport::default(), None, None, Some(ghost)),
            Err(CommandError::NoSuchNode { target }) if target == ghost
        ));
    }

    /// A text mobject has no points (`Mobject::has_picture` is the test for "can
    /// be seen", not a point count), so there is no box to frame. Refusing is
    /// honest: a silent success would leave the camera where it was and the host
    /// believing it had framed the text.
    #[test]
    fn a_focus_on_a_node_without_geometry_is_refused() {
        let mut tree = SceneTree::new();
        let text = tree.insert(Mobject::new(vec![]));
        assert!(matches!(
            camera_for(&tree, &Viewport::default(), None, None, Some(text)),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "focus"
        ));
    }

    /// The fit leaves a 1.2 margin and is decided by the tighter axis. A 2×2 box
    /// at 1280×720: the frame is 8 units tall and ~14.2 wide, so the height is
    /// the tighter constraint — `8 / (1.2 * 2)`.
    #[test]
    fn a_focus_frames_the_box_with_a_margin() {
        let (tree, id) = tree_with_a_square();
        let camera = camera_for(&tree, &Viewport::default(), None, None, Some(id)).unwrap();
        assert_eq!(camera.center, (0.0, 0.0));
        assert!(
            (camera.zoom - Camera::WORLD_HEIGHT / (1.2 * 2.0)).abs() < 1e-12,
            "zoom {}",
            camera.zoom
        );
    }

    /// The aspect ratio is really used: a 4×1 box cannot be fitted by height
    /// alone, because 8 units of height would push it off the sides.
    #[test]
    fn a_wide_focus_is_limited_by_the_frame_width() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(rustim_mobject::shapes::square(4.0, 1.0)));
        let vp = Viewport::default();
        let camera = camera_for(&tree, &vp, None, None, Some(id)).unwrap();
        let by_width = Camera::WORLD_HEIGHT
            * (f64::from(vp.frame_width) / f64::from(vp.frame_height))
            / (1.2 * 4.0);
        assert!((camera.zoom - by_width).abs() < 1e-12, "{}", camera.zoom);
        assert!(
            camera.zoom < Camera::WORLD_HEIGHT / (1.2 * 1.0),
            "the height alone would have allowed a closer view"
        );
    }

    /// One zero dimension is fitted by the other axis rather than refused — a
    /// horizontal line is a legitimate subject.
    #[test]
    fn a_flat_focus_is_fitted_by_its_long_axis() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(rustim_mobject::shapes::square(4.0, 0.0)));
        let camera = camera_for(&tree, &Viewport::default(), None, None, Some(id)).unwrap();
        let expected = Camera::WORLD_HEIGHT * (1280.0 / 720.0) / (1.2 * 4.0);
        assert!((camera.zoom - expected).abs() < 1e-12, "{}", camera.zoom);
    }

    #[test]
    fn an_explicit_center_and_zoom_override_the_focus_fit() {
        let (tree, id) = tree_with_a_square();
        let camera = camera_for(
            &tree,
            &Viewport::default(),
            Some([10.0, -4.0]),
            Some(1.5),
            Some(id),
        )
        .unwrap();
        assert_eq!(camera, Camera::new((10.0, -4.0), 1.5));
    }

    /// `center` and `zoom` are independent overrides: giving one keeps the other
    /// half of the current view, which is what makes `{"zoom": 2}` mean "zoom in
    /// on what I am already looking at".
    #[test]
    fn a_partial_camera_command_keeps_the_other_half() {
        let mut tree = SceneTree::new();
        tree.camera = Camera::new((2.0, 3.0), 4.0);

        let zoom_only = camera_for(&tree, &Viewport::default(), None, Some(0.5), None).unwrap();
        assert_eq!(zoom_only, Camera::new((2.0, 3.0), 0.5));

        let pan_only =
            camera_for(&tree, &Viewport::default(), Some([-1.0, 0.0]), None, None).unwrap();
        assert_eq!(pan_only, Camera::new((-1.0, 0.0), 4.0));
    }

    /// The same accepted range as an animation's duration, because it *is* one:
    /// `None` is instant, `0.0` is a legitimate snap inside a beat.
    #[test]
    fn a_camera_duration_is_validated_like_an_animation_duration() {
        assert_eq!(camera_duration(None).unwrap(), None);
        assert_eq!(camera_duration(Some(0.0)).unwrap(), Some(0.0));
        assert_eq!(camera_duration(Some(1.5)).unwrap(), Some(1.5));
        for bad in [-1.0, f64::INFINITY, f64::NAN] {
            assert!(
                matches!(
                    camera_duration(Some(bad)),
                    Err(CommandError::OutOfRange { ref property, .. }) if property == "duration"
                ),
                "{bad} must be refused"
            );
        }
    }
}
