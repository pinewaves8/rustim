//! Concrete animations.
//!
//! The shapes the design has to support, one animation each: a scalar
//! ([`FadeIn`], [`FadeOut`]), an interpolated transform ([`MoveTo`]) — where the
//! deferred transform pays off directly — a parameterised *rigid* transform whose
//! parameter is not a matrix entry ([`Rotate`], [`Scale`]), a single style
//! channel driven to any value ([`Animate`]), point arrays
//! ([`Transform`], the morph), which is what puts Manim's `align_points`
//! machinery to work, and a *prefix* of a point array ([`Create`],
//! [`Uncreate`]), which is `pointwise_become_partial`.
//!
//! [`CameraMove`] is the one animation that writes no object at all — it moves
//! the view — and it is here rather than beside the command layer because it is
//! an ordinary beat: the same scheduler, the same `Sequence`, the same
//! reduced-motion branch.
//!
//! [`Wait`] is the last and has no shape at all: it writes nothing and only
//! occupies the clock, which is exactly what makes a pause an animation like any
//! other.

use rustim_color::Srgb;
use rustim_math::{Affine2, Camera, Point, bezier};
use rustim_mobject::{Mobject, NodeId, SceneTree, Style};

use crate::{Animation, RateFunc, rate};

/// Fades an object in by driving its opacity.
///
/// Note this touches `opacity`, not the style's `fill_opacity`. Manim keeps the
/// two independent and multiplies them at draw time; `CLAUDE.md` records that
/// they compose multiplicatively, so an object at half fill opacity that fades
/// in to 50% must end up at 25%, not 50%.
#[derive(Debug, Clone)]
pub struct FadeIn {
    target: NodeId,
    from: f64,
    to: f64,
    duration: f64,
}

impl FadeIn {
    pub fn new(target: NodeId) -> Self {
        Self {
            target,
            from: 0.0,
            to: 1.0,
            duration: 1.0,
        }
    }

    pub fn with_duration(mut self, seconds: f64) -> Self {
        self.duration = seconds;
        self
    }
}

impl Animation for FadeIn {
    fn target(&self) -> Option<NodeId> {
        Some(self.target)
    }

    fn duration(&self) -> f64 {
        self.duration
    }

    fn begin(&mut self, tree: &SceneTree) {
        // Fade from wherever the object currently is, not from hardcoded zero,
        // so a fade interrupted by another fade does not snap.
        if let Some(m) = tree.get(self.target) {
            self.from = m.opacity;
        }
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        if let Some(m) = tree.get_mut(self.target) {
            m.opacity = self.from + (self.to - self.from) * alpha;
            m.dirty.style = true;
        }
    }
}

/// Fades an object out and **removes it from the scene**.
///
/// `FadeIn`'s mirror, with one asymmetry that is the whole reason [`Animation::finish`]
/// exists: Manim's `FadeOut` is a `remover=True` animation (`animation/fading.py:184-192`),
/// so when it ends the object is gone from the scene rather than sitting there at
/// zero opacity. `Alpha`-zeroed and gone are different states — a stale node is
/// still in the accessibility tree, still addressable by an AI command, and still
/// counted by `--inspect`.
///
/// Manim spells that half as `clean_up_from_scene`, called by the renderer once
/// the animation has reached its end; here it is [`Animation::finish`], which the
/// scheduler calls on exactly the same tick. The opacity work is `FadeIn`'s with
/// the endpoints swapped.
#[derive(Debug, Clone)]
pub struct FadeOut {
    target: NodeId,
    from: f64,
    to: f64,
    duration: f64,
}

impl FadeOut {
    pub fn new(target: NodeId) -> Self {
        Self {
            target,
            from: 1.0,
            to: 0.0,
            duration: 1.0,
        }
    }

    pub fn with_duration(mut self, seconds: f64) -> Self {
        self.duration = seconds;
        self
    }
}

impl Animation for FadeOut {
    fn target(&self) -> Option<NodeId> {
        Some(self.target)
    }

    fn duration(&self) -> f64 {
        self.duration
    }

    fn begin(&mut self, tree: &SceneTree) {
        // From wherever the object currently is, for the reason `FadeIn` gives:
        // an interrupted fade must not snap.
        if let Some(m) = tree.get(self.target) {
            self.from = m.opacity;
        }
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        if let Some(m) = tree.get_mut(self.target) {
            m.opacity = self.from + (self.to - self.from) * alpha;
            m.dirty.style = true;
        }
    }

    /// The removal. Note it is the *subtree*, matching Manim's
    /// `mobject.get_family()` (`fading.py:191`): fading out a group takes the
    /// group's contents with it rather than emptying them into the scene, which
    /// is what [`SceneTree::remove`] alone would do.
    fn finish(&mut self, tree: &mut SceneTree) {
        tree.remove_subtree(self.target);
    }
}

/// Moves an object to a target transform — or to a point, keeping its scale and
/// rotation.
///
/// This is the animation that demonstrates why transforms are deferred: the
/// whole move is an interpolation of one [`Affine2`], regardless of how many
/// points the object has. Manim rewrites every descendant's point array on each
/// frame of the equivalent animation.
///
/// Note it interpolates the *transform*, not a position, so a rotation or scale
/// carried in the same transform rides along. Interpolating a general affine
/// componentwise is not the same as interpolating a rotation's angle, so a
/// `Rotate` animation needs its own type rather than riding on this one.
///
/// # Two destinations, and why the point one waits until `begin`
///
/// [`MoveTo::new`] takes the whole target transform: the caller has already said
/// exactly what the matrix should be, and the animation interpolates toward it.
/// [`MoveTo::to_point`] is the `move_to` a scene author means — **"put the
/// object's centre here"** — and it therefore has to keep whatever scale and
/// rotation the object already carries. Manim's `move_to` is a `shift`
/// (`point - get_critical_point(..)`) composed onto the existing matrix, which is
/// the same statement.
///
/// The destination is resolved in **`begin`**, not in the constructor, for a
/// reason composition made visible: a `Succession` builds all of its members
/// before the first one runs, so a `MoveTo` constructed after a `Scale` in the
/// same succession would otherwise capture the *pre-scale* transform and then
/// throw the scale away. Resolving in `begin` means "move it from wherever it is
/// when this animation starts", which is what the author wrote.
#[derive(Debug, Clone)]
pub struct MoveTo {
    target: NodeId,
    /// `Some` for [`MoveTo::to_point`]; `None` when the caller supplied the whole
    /// transform.
    point: Option<Point>,
    target_transform: Affine2,
    start: Option<Affine2>,
    duration: f64,
}

impl MoveTo {
    /// Interpolate to `target_transform`, exactly as given.
    ///
    /// Use this when the destination *is* the matrix (a caller that has already
    /// composed a scale or rotation into it). If you mean "move the centre to a
    /// point", use [`MoveTo::to_point`] — this constructor will replace the
    /// object's scale and rotation with the target's.
    pub fn new(target: NodeId, target_transform: Affine2) -> Self {
        Self {
            target,
            point: None,
            target_transform,
            start: None,
            duration: 1.0,
        }
    }

    /// Move the object's centre to `point`, preserving the rest of its transform.
    ///
    /// The point is in the **parent's** space (world units for a top-level
    /// object), the same frame [`Mobject::critical_point`] answers in. The
    /// translation is computed when the animation begins — see the type's doc.
    pub fn to_point(target: NodeId, point: Point) -> Self {
        Self {
            target,
            point: Some(point),
            target_transform: Affine2::IDENTITY,
            start: None,
            duration: 1.0,
        }
    }

    pub fn with_duration(mut self, seconds: f64) -> Self {
        self.duration = seconds;
        self
    }
}

impl Animation for MoveTo {
    fn target(&self) -> Option<NodeId> {
        Some(self.target)
    }

    fn duration(&self) -> f64 {
        self.duration
    }

    fn begin(&mut self, tree: &SceneTree) {
        let Some(m) = tree.get(self.target) else {
            self.start = None;
            return;
        };
        self.start = Some(m.transform);
        if let Some(point) = self.point {
            // `critical_point` applies the object's own transform, so this is the
            // centre as it stands at `begin` — after an earlier succession member,
            // and in the parent's space.
            let centre = m.critical_point((0.0, 0.0)).unwrap_or(Point::ZERO);
            self.target_transform =
                Affine2::translate((point.x - centre.x, point.y - centre.y)) * m.transform;
        }
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        let Some(start) = self.start else { return };
        let Some(m) = tree.get_mut(self.target) else {
            return;
        };
        m.set_transform(lerp_affine(start, self.target_transform, alpha));
    }
}

/// Spins an object about a pivot, interpolating the **angle**.
///
/// The animation [`MoveTo`] cannot express, and the reason it exists rather than
/// reusing that one: `MoveTo` interpolates an affine componentwise, which for a
/// rotation is not the same curve. Halfway between identity and a quarter turn,
/// lerping the matrix gives the shear-ish halfway matrix whose on-curve points are
/// *inside* the arc, while the angle halfway is the 45° rotation. Manim makes the
/// same distinction by having `Rotate` be a `Transform` whose `path_arc` is the
/// angle (`animation/rotation.py:155-184`) — the parameter rides beside the
/// matrix rather than inside it.
///
/// # What it is faithful to, and where it is stricter
///
/// * **The angle is interpolated linearly**, `rotate(interpolate(0, self.angle,
///   alpha), about_point=..)` (`rotation.py:181`), not eased twice and not
///   slerped. The ease is the rate function's, applied once to `alpha` before
///   `interpolate` is reached, exactly as everywhere else in this crate.
/// * **The pivot defaults to the object's own centre**, which is Manim's
///   `mobject.get_center()` (`rotation.py:164-169`) — read here as
///   `critical_point((0.0, 0.0))`, the midpoint of the anchors' box.
/// * **The pivot is captured at `begin`, not at construction**, because the
///   object may be moved between the two, and Manim reads `get_center()` when the
///   animation starts.
/// * **A rotation composes with the existing transform** rather than replacing it
///   — `rotate_about(θ·α, pivot) * start` — so "move it, then spin it" keeps the
///   move. `Mobject.rotate` is in-place in the same sense.
///
/// Manim's `about_point` is a full-width rotation carrying the object around the
/// pivot, and so is this one; [`Mobject::rotate_about`] documents what "the pivot"
/// means when the object is nested.
///
/// The one divergence: Manim's default `angle` is `TAU / 4`, which is a defect
/// nobody means to hit — a bare `Rotate(mob)` turning a quarter turn is a
/// surprise, and the command layer refuses it for that reason.
#[derive(Debug, Clone)]
pub struct Rotate {
    target: NodeId,
    angle: f64,
    /// A *world* point, as a caller thinks of it; converted once in `begin`. The
    /// stored pivot is in the parent's space, which is what
    /// [`Mobject::rotate_about`] takes.
    about_point: Option<Point>,
    start: Option<Affine2>,
    pivot: Option<Point>,
    duration: f64,
}

impl Rotate {
    /// Rotate by `angle` radians about the object's own centre.
    pub fn new(target: NodeId, angle: f64) -> Self {
        Self {
            target,
            angle,
            about_point: None,
            start: None,
            pivot: None,
            duration: 1.0,
        }
    }

    /// Rotate about a **world** point instead of the object's centre.
    ///
    /// World rather than parent-space because that is the frame the scene author
    /// writes in — no scene in this workspace has a nesting depth to speak of, and
    /// a caller who has a world point should not have to learn the tree's
    /// coordinate conventions to use it.
    pub fn about_point(mut self, point: Point) -> Self {
        self.about_point = Some(point);
        self
    }

    pub fn with_duration(mut self, seconds: f64) -> Self {
        self.duration = seconds;
        self
    }
}

impl Animation for Rotate {
    fn target(&self) -> Option<NodeId> {
        Some(self.target)
    }

    fn duration(&self) -> f64 {
        self.duration
    }

    fn begin(&mut self, tree: &SceneTree) {
        let Some(m) = tree.get(self.target) else {
            return;
        };
        self.start = Some(m.transform);
        self.pivot = match self.about_point {
            // `critical_point` answers in the parent's space already (it
            // materialises through `self.transform` alone), so the default pivot
            // needs no inverse.
            None => m.critical_point((0.0, 0.0)),
            Some(world) => to_parent_space(tree.parent_transform(self.target), world),
        };
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        let (Some(start), Some(pivot)) = (self.start, self.pivot) else {
            return;
        };
        let Some(m) = tree.get_mut(self.target) else {
            return;
        };
        let angle = lerp_scalar(0.0, self.angle, alpha);
        m.set_transform(Affine2::rotate_about(angle, pivot) * start);
    }
}

/// Keeps turning, until something stops it.
///
/// **The one animation with no end**, and the reason [`Animation::perpetual`]
/// exists: the scheduler's ordinary rule is `alpha = elapsed / duration`, which
/// for an endless thing would be `age / ∞ = 0` — frozen at its start while
/// `is_playing` stayed true. A perpetual animation is handed its own **age**
/// instead, and this reads an angle out of it.
///
/// `turns_per_second` is the whole parameter, and its **sign is the direction**:
/// `0.25` is one turn every four seconds, `-0.25` the same the other way. A rate
/// rather than a period because the angle is exactly `age × rate × τ` — a period
/// would be divided back out on every frame, and the division is where a
/// `f64::INFINITY` would come back from.
///
/// It is a **beat that never ends**: the queue will not advance past it, and the
/// `stop` command — [`AnimationScheduler::clear`] — is how it is taken off the
/// clock. `duration()` reports the *period*, one full cycle, because that is all
/// an endless animation can honestly offer and it is what a `beat_started` event
/// has a field for.
#[derive(Debug, Clone)]
pub struct Rotating {
    target: NodeId,
    turns_per_second: f64,
    start: Option<Affine2>,
    /// A *world* point, as a caller thinks of it; converted once in `begin`.
    about_point: Option<Point>,
    /// The same point in the parent's space, which is what
    /// [`Mobject::rotate_about`] takes.
    pivot: Option<Point>,
}

impl Rotating {
    /// Turn `target` about its own centre at `turns_per_second`, forever.
    ///
    /// A zero or non-finite rate is a caller bug, and the command layer refuses
    /// one rather than this constructor: that is where a model can reach, and this
    /// is the Rust-level API a scene author uses deliberately.
    pub fn new(target: NodeId, turns_per_second: f64) -> Self {
        Self {
            target,
            turns_per_second,
            start: None,
            about_point: None,
            pivot: None,
        }
    }

    /// Turn about a **world** point instead of the object's own centre.
    pub fn about_point(mut self, point: Point) -> Self {
        self.about_point = Some(point);
        self
    }
}

impl Animation for Rotating {
    fn target(&self) -> Option<NodeId> {
        Some(self.target)
    }

    /// One full turn — a **period**, not a length: the beat does not end.
    ///
    /// A degenerate rate degrades to a nominal period rather than reporting an
    /// infinity, which is the same posture a shape builder takes to a bad
    /// parameter and for a concrete reason: an `∞` here would reach a
    /// `beat_started` event, and JSON has no spelling for one.
    fn duration(&self) -> f64 {
        let rate = self.turns_per_second.abs();
        if rate.is_finite() && rate > 0.0 {
            1.0 / rate
        } else {
            1.0
        }
    }

    fn perpetual(&self) -> bool {
        true
    }

    fn begin(&mut self, tree: &SceneTree) {
        let Some(m) = tree.get(self.target) else {
            return;
        };
        self.start = Some(m.transform);
        self.pivot = match self.about_point {
            // `critical_point` answers in the parent's space already, so the
            // default pivot needs no inverse — the same note `Rotate` carries.
            None => m.critical_point((0.0, 0.0)),
            Some(world) => to_parent_space(tree.parent_transform(self.target), world),
        };
    }

    /// `elapsed` here is **seconds**, not progress — see [`Animation::perpetual`].
    fn interpolate(&mut self, elapsed: f64, tree: &mut SceneTree) {
        let (Some(start), Some(pivot)) = (self.start, self.pivot) else {
            return;
        };
        let Some(m) = tree.get_mut(self.target) else {
            return;
        };
        let angle = elapsed * self.turns_per_second * std::f64::consts::TAU;
        m.set_transform(Affine2::rotate_about(angle, pivot) * start);
    }
}

/// Grows or shrinks an object **in place**.
///
/// Manim has no `Scale` animation: scaling is `ScaleInPlace` (`animation/transform.py:562`),
/// an `ApplyMethod` over `Mobject.scale` (`mobject/mobject.py:1407-1458`), which
/// interpolates the *factor* — `interpolate(1, factor, alpha)` (`:1454`) — about
/// the object's own centre. There is no pivot parameter, and that is not an
/// omission: `ScaleInPlace` is the in-place one and `Scale`-about-a-point is what
/// `ApplyMethod` or a `Rotate`-style wrapper would give. This is named `Scale`
/// because that is the name a Manim user reaches for, and it *is* `ScaleInPlace`.
///
/// Linear in the factor is the part worth stating, because the intuitive
/// alternative is exponential: at `alpha = 0.5`, growing by ×4 is ×2.5, not ×2.
/// Manim's `interpolate(1, factor, alpha)` is arithmetic, and the parameter is the
/// factor rather than a log of it — so a scale-up and a scale-down of the same
/// magnitude are *not* mirror images in time. This reproduces that.
#[derive(Debug, Clone)]
pub struct Scale {
    target: NodeId,
    factor: f64,
    start: Option<Affine2>,
    centre: Option<Point>,
    duration: f64,
}

impl Scale {
    /// Scale by `factor` about the object's own centre.
    pub fn new(target: NodeId, factor: f64) -> Self {
        Self {
            target,
            factor,
            start: None,
            centre: None,
            duration: 1.0,
        }
    }

    pub fn with_duration(mut self, seconds: f64) -> Self {
        self.duration = seconds;
        self
    }
}

impl Animation for Scale {
    fn target(&self) -> Option<NodeId> {
        Some(self.target)
    }

    fn duration(&self) -> f64 {
        self.duration
    }

    fn begin(&mut self, tree: &SceneTree) {
        let Some(m) = tree.get(self.target) else {
            return;
        };
        self.start = Some(m.transform);
        self.centre = m.critical_point((0.0, 0.0));
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        let (Some(start), Some(centre)) = (self.start, self.centre) else {
            return;
        };
        let Some(m) = tree.get_mut(self.target) else {
            return;
        };
        let factor = lerp_scalar(1.0, self.factor, alpha);
        m.set_transform(Affine2::scale_about(factor, centre) * start);
    }
}

/// One style channel [`Animate`] can interpolate, carrying the value it moves to.
///
/// The vocabulary is deliberately the **paint** half of what `set` accepts. The
/// geometry half already has an animation whose semantics are better than "lerp
/// the field": [`MoveTo`] resolves its destination in `begin`, [`Scale`] composes
/// about the object's centre, and [`Rotate`] interpolates an angle rather than a
/// matrix. A label or a kind is not a continuous quantity at all — it is what the
/// object *means*, and there is no frame at which it is halfway a circle. So
/// this covers exactly the channels where "in between" is both defined and
/// visible.
///
/// `Srgb` is named rather than reached through [`Style`] because the colour
/// channel *is* a colour, and a second representation for it would drift from
/// the one `Style::lerp` already blends.
#[derive(Debug, Clone, Copy)]
pub enum StyleProperty {
    /// The object's own opacity, the channel [`FadeIn`] drives.
    Opacity(f64),
    FillOpacity(f64),
    StrokeOpacity(f64),
    StrokeWidth(f64),
    StrokeColor(Srgb),
}

impl StyleProperty {
    /// The value this channel currently has on `m`.
    ///
    /// Matching on `self` is what keeps the read complete: a variant added to the
    /// enum is a compile error here rather than a silently unread channel.
    fn read(self, m: &Mobject) -> Self {
        match self {
            Self::Opacity(_) => Self::Opacity(m.opacity),
            Self::FillOpacity(_) => Self::FillOpacity(m.style.fill_opacity),
            Self::StrokeOpacity(_) => Self::StrokeOpacity(m.style.stroke_opacity),
            Self::StrokeWidth(_) => Self::StrokeWidth(m.style.stroke_width),
            Self::StrokeColor(_) => Self::StrokeColor(m.style.stroke),
        }
    }

    /// Blend from `self` towards `to`.
    ///
    /// A pair of different channels cannot be produced through [`Animate`] —
    /// `begin` reads `to`'s own channel — so a mismatch is answered with `self`
    /// rather than a panic, the same posture a command naming a deleted node has.
    fn blend(self, to: Self, t: f64) -> Self {
        if t == 1.0 {
            return to;
        }
        match (self, to) {
            (Self::Opacity(a), Self::Opacity(b)) => Self::Opacity(lerp_scalar(a, b, t)),
            (Self::FillOpacity(a), Self::FillOpacity(b)) => Self::FillOpacity(lerp_scalar(a, b, t)),
            (Self::StrokeOpacity(a), Self::StrokeOpacity(b)) => {
                Self::StrokeOpacity(lerp_scalar(a, b, t))
            }
            (Self::StrokeWidth(a), Self::StrokeWidth(b)) => Self::StrokeWidth(lerp_scalar(a, b, t)),
            (Self::StrokeColor(a), Self::StrokeColor(b)) => {
                Self::StrokeColor(blend_srgb(a, b, t as f32))
            }
            _ => self,
        }
    }

    /// Write this value into the object, marking what a repaint must rebuild.
    fn write(self, m: &mut Mobject) {
        match self {
            Self::Opacity(v) => m.opacity = v,
            Self::FillOpacity(v) => m.style.fill_opacity = v,
            Self::StrokeOpacity(v) => m.style.stroke_opacity = v,
            Self::StrokeWidth(v) => m.style.stroke_width = v,
            Self::StrokeColor(c) => m.style.stroke = c,
        }
        m.dirty.style = true;
    }
}

/// Moves one style channel from wherever it is to a new value over time.
///
/// The wire's `animate`, and the piece that was missing between `set` (change a
/// property instantly) and the endpoint-only [`FadeIn`] / [`FadeOut`] pair: "take
/// the opacity to 0.35", "thicken the outline", "recolour the stroke" were all
/// expressible only as a jump.
///
/// Three properties are the design:
///
/// * **`begin` captures the current value**, not a value fixed at construction —
///   the same reason [`FadeIn`] does it: a beat may be constructed long before
///   its turn (`Succession` builds every member up front), and an animation
///   interrupted by another must not snap back. The consequence is that two
///   `animate`s in a succession compose: the second starts from where the first
///   ended.
/// * **The end is written, not computed.** `interpolate(1.0)` writes the target
///   verbatim (`Style::lerp`'s own short-circuit), so reduced motion — which
///   drives `ease(1.0)` once — settles on a frame that is bitwise identical to
///   the one full motion reaches, and a replayed run is identical too.
/// * **Opacity does not inherit in the draw walk**, so animating a group's
///   opacity changes the group's own invisible field and not its children.
///   Target the nodes the viewer can see.
///
/// A target that is gone by the time the animation runs is a no-op, as it is for
/// every other animation here: `interpolate` has no error channel.
#[derive(Debug, Clone)]
pub struct Animate {
    target: NodeId,
    to: StyleProperty,
    from: Option<StyleProperty>,
    duration: f64,
}

impl Animate {
    /// Move `target`'s style channel to `to` over `duration` seconds.
    pub fn new(target: NodeId, to: StyleProperty) -> Self {
        Self {
            target,
            to,
            from: None,
            duration: 1.0,
        }
    }

    pub fn with_duration(mut self, seconds: f64) -> Self {
        self.duration = seconds;
        self
    }
}

impl Animation for Animate {
    fn target(&self) -> Option<NodeId> {
        Some(self.target)
    }

    fn duration(&self) -> f64 {
        self.duration
    }

    fn begin(&mut self, tree: &SceneTree) {
        if let Some(m) = tree.get(self.target) {
            self.from = Some(self.to.read(m));
        }
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        let Some(from) = self.from else {
            return;
        };
        let Some(m) = tree.get_mut(self.target) else {
            return;
        };
        from.blend(self.to, alpha).write(m);
    }
}

/// Interpolate two sRGB colours per channel, the way `Style::lerp` does.
///
/// No colour-space conversion: both ends are already sRGB, and Manim blends its
/// raw arrays too.
fn blend_srgb(a: Srgb, b: Srgb, t: f32) -> Srgb {
    let (a, b) = (a.components, b.components);
    let mut out = [0.0f32; 4];
    for (i, o) in out.iter_mut().enumerate() {
        *o = (1.0 - t) * a[i] + t * b[i];
    }
    Srgb::new(out)
}

/// Morphs an object into another shape.
///
/// This is Manim's `Transform` (`animation/transform.py:58`), and the armature of
/// it is one sentence: **alignment happens once, in `begin`, and every frame
/// after is a pure elementwise interpolation against a frozen snapshot.**
/// `Transform.begin` (`:200-212`) copies the target, aligns the source *against
/// that copy* (`:211`), and `Animation.begin` (`animation.py:197-213`) then
/// freezes the source — after alignment, so the frozen start already has the
/// aligned point count. Per frame only `Mobject.interpolate`
/// (`mobject/mobject.py:3303-3305`) runs.
///
/// Two consequences of Manim's own design are worth naming before the port:
///
/// * **The target is a donor, not a scene object.** `self.target` is a free value
///   and is never inserted into the tree; only the *source's* id lives through
///   the animation. Manim does exactly this — a plain `Transform` is neither an
///   introducer nor a remover (`animation.py:229-241`, `:255-261`), so it adds
///   nothing and removes nothing, and `clean_up_from_scene` (`:219-222`) swaps
///   ids only for `ReplacementTransform`. In a tree keyed by [`NodeId`] that is
///   what makes "the target is not in the scene" expressible at all. The
///   node-to-node spelling the AI layer will want is
///   `Transform::new(a, tree.get(b).expect("target").clone())`, one line at the
///   call site.
/// * **Alignment is `bezier::align_points`, which can *shrink*.** Equal point
///   counts come back untouched — including the over-eager shortcut upstream
///   marks with a `TODO` — so the lerp below is not skipped for same-length
///   inputs, it simply has nothing to pad.
///
/// # The divergence from Manim, which is deliberate
///
/// Manim's points *are* world coordinates, so its lerp is a world-space lerp with
/// no transform in sight. Here [`Mobject::points`] is *local* beside the deferred
/// [`Mobject::transform`], so the pair `(points, transform)` moves together: the
/// aligned local points are lerped **and** the affine is lerped alongside them.
/// For a translate/scale delta that reproduces Manim's world-space result
/// exactly, and it is the only choice that keeps the O(1) deferred transform the
/// rest of the architecture is built on — materialising both sides to world
/// points and writing the result back with an identity transform would make a
/// `Transform` composed with a `MoveTo` impossible. The cost is that a **rotation
/// delta does not interpolate as a rotation**, because interpolating an affine
/// componentwise is not interpolating an angle; that is the same caveat
/// [`MoveTo`]'s doc carries, and for the same reason a `Rotate` deserves its own
/// animation type.
///
/// # Not ported, on purpose
///
/// * **Family and child alignment.** `Mobject.align_data`
///   (`mobject/mobject.py:3149-3156`) also runs `null_point_align`, pads child
///   counts with fully transparent placeholders (`align_submobjects`, `:3177`),
///   and recurses per immediate child pair. None of that is here: this aligns the
///   source's own point array only. Every shape in `rustim-mobject` today
///   (`shapes::square` / `circle` / `right_triangle`) is childless with all its
///   geometry in `points`, so a shape-to-shape morph is fully covered, but a
///   morph between two *grouped* mobjects would align only the top-level array.
/// * **`path_func` as a user-supplied closure.** Manim takes one; the only
///   built-in it ships is `path_along_arc`, which this type offers as
///   [`Transform::with_path_arc`]. An arbitrary Python callable has no
///   counterpart that could cross a serde command boundary, and the AI layer
///   names an animation by data, so the angle is the vocabulary. (Manim's own
///   default is a straight lerp: `path_arc = 0` is below
///   `STRAIGHT_PATH_THRESHOLD`, `utils/paths.py:30`.)
/// * **Sheen.** [`Style`] has no `sheen_direction` / `sheen_factor`; there is
///   nothing to interpolate.
///
/// A source whose point array is ragged (`len % 4 ∈ {2, 3}`) is refused by
/// `align_points`; `begin` has no error channel to report through, so the state
/// stays unset and every later `interpolate` is a no-op. Manim would raise here.
/// The refusal itself is pinned by `rustim-math`'s own tests, so this type only
/// has to not panic — the same posture as a command naming a deleted node.
#[derive(Debug, Clone)]
pub struct Transform {
    source: NodeId,
    /// The donor. Held by value because Manim holds a `target_copy` by value, and
    /// never mutated: `begin` aligns against a clone of it, so the caller's
    /// geometry survives the animation untouched.
    target: Mobject,
    start: Option<Start>,
    duration: f64,
    /// Manim's `path_arc`: the angle each point's route subtends. `None` is a
    /// straight lerp, which is what Manim does when no `path_func` is given.
    path_arc: Option<f64>,
}

/// What `begin` captured, once, never to be recomputed.
///
/// Both point arrays are stored *already aligned*, because the source's own array
/// is not written back at `begin` time — Manim's `align_data` mutates it in place,
/// but leaving the tree untouched until the first frame is both simpler and
/// invisible, since `interpolate(0.0)` rewrites the array wholesale on the very
/// next call.
#[derive(Debug, Clone)]
struct Start {
    from_points: Vec<Point>,
    to_points: Vec<Point>,
    from_transform: Affine2,
    from_opacity: f64,
    from_style: Style,
}

impl Transform {
    /// Morph the mobject at `source` into `target`.
    ///
    /// `target` is geometry and paint, not a scene object: it is not added to the
    /// tree, and the source ends up wearing its points, transform, opacity and
    /// style. See the type's doc for why that is the faithful reading of Manim.
    pub fn new(source: NodeId, target: Mobject) -> Self {
        Self {
            source,
            target,
            start: None,
            duration: 1.0,
            path_arc: None,
        }
    }

    pub fn with_duration(mut self, seconds: f64) -> Self {
        self.duration = seconds;
        self
    }

    /// Route every point along an arc of `radians` instead of a straight line.
    ///
    /// Manim's `Transform(path_arc=…)`, which builds
    /// [`path_along_arc`](rustim_math::paths::path_along_arc). The sign chooses
    /// which way the path bows; `0`, and anything smaller than
    /// `STRAIGHT_PATH_THRESHOLD`, is the straight line.
    ///
    /// # The arc is measured in the parent's space, and costs the deferred form
    ///
    /// Manim's points *are* world points, so its arc bows the real route. Here a
    /// node's points are local and part of its displacement may sit in
    /// [`Mobject::transform`]. Arcing only the local arrays would make
    /// `path_arc` a no-op for the commonest request — "move this there along an
    /// arc", expressed as a translated target — so the arc is computed between
    /// the two *parent-space* points (`from_transform · a` and
    /// `to_transform · b`) and the result is written back as local points with an
    /// identity transform.
    ///
    /// That is the one cost, and it is paid only when an arc is asked for: the
    /// object stops being a deferred transform for the duration of the morph.
    /// Plain `Transform` keeps it, which is the O(1) property the architecture
    /// is built on.
    pub fn with_path_arc(mut self, radians: f64) -> Self {
        self.path_arc = Some(radians);
        self
    }
}

impl Animation for Transform {
    fn target(&self) -> Option<NodeId> {
        // `source`, not `target`: this animation's `target` field is the donor
        // — a `Mobject` value, not a node in the tree — and the object being
        // morphed is the source. A scoped stop is about the thing on screen.
        Some(self.source)
    }

    fn duration(&self) -> f64 {
        self.duration
    }

    fn begin(&mut self, tree: &SceneTree) {
        let Some(m) = tree.get(self.source) else {
            return;
        };
        // One alignment, against the donor's points — Manim's `align_data`, and
        // the only place the two arrays are ever reconciled. `Err` leaves
        // `start` unset, which turns every later frame into a no-op.
        let Ok((from_points, to_points)) = bezier::align_points(&m.points, &self.target.points)
        else {
            return;
        };
        self.start = Some(Start {
            from_points,
            to_points,
            from_transform: m.transform,
            from_opacity: m.opacity,
            from_style: m.style,
        });
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        let Some(start) = &self.start else { return };
        let Some(m) = tree.get_mut(self.source) else {
            return;
        };

        // Assigned wholesale rather than zipped into the existing array: at
        // `alpha = 0` that is what grows a 3-curve triangle into the 4-curve shape
        // the target is expressed in. Manim assigns wholesale too
        // (`mobject.py:3303`).
        m.points = match self.path_arc {
            // Manim applies the path function to the *aligned* arrays, per point,
            // and to nothing else — the affine and the style still interpolate
            // linearly. Its arrays are world points; see `path_point` for why
            // these are folded into the parent's space first.
            Some(arc) => start
                .from_points
                .iter()
                .zip(start.to_points.iter())
                .map(|(a, b)| {
                    path_point(
                        arc,
                        start.from_transform,
                        *a,
                        self.target.transform,
                        *b,
                        alpha,
                    )
                })
                .collect(),
            None => start
                .from_points
                .iter()
                .zip(start.to_points.iter())
                .map(|(a, b)| {
                    Point::new(lerp_scalar(a.x, b.x, alpha), lerp_scalar(a.y, b.y, alpha))
                })
                .collect(),
        };

        m.set_transform(match self.path_arc {
            // The arc branch wrote parent-space coordinates into `points`;
            // carrying the lerped affine as well would apply the motion twice.
            Some(_) => Affine2::IDENTITY,
            None => lerp_affine(start.from_transform, self.target.transform, alpha),
        });
        m.opacity = lerp_scalar(start.from_opacity, self.target.opacity, alpha);
        m.style = Style::lerp(start.from_style, self.target.style, alpha);
        m.dirty.style = true;
    }
}

/// Draws an object's path on, from its first point to its last.
///
/// Manim's `Create` (`animation/creation.py:140`), which is its `ShowPartial`
/// with bounds `(0, alpha)` (`:133-134`): at progress `alpha` the object wears
/// the first `alpha` fraction of its own point array, so the path appears to be
/// traced by a pen. The fraction is measured in **curves**, not points — a
/// three-curve triangle is one third drawn when its first side is — via
/// [`bezier::partial_path_points`], the port of `pointwise_become_partial`.
///
/// # What it draws, and what it does not
///
/// The animation rewrites the target's own `points`, which is exactly right for
/// every shape builder in `rustim-mobject` (`square`, `circle`, `line`, `plot`,
/// …). It does **not** recurse into children, for the same reason
/// [`Transform`]'s alignment does not: a text and a formula mobject have no
/// points of their own — their geometry is a glyph run or child glyphs — so
/// `Create` on one of them traces nothing. Drawing a *group* on child by child
/// is `lag_ratio`'s job and is deferred with it.
///
/// # The last frame is a copy
///
/// `alpha = 1.0` takes `partial_path_points`' full-range shortcut, which returns
/// the captured array unchanged rather than re-evaluating each Bézier. So the
/// object ends **bit-identical** to where it started, which is what lets
/// `Create` be composed with a later animation without drift.
///
/// The captured start is a *clone*, and the tree's array is replaced wholesale
/// each frame: an interrupted `Create` leaves the object partially drawn, the
/// same way an interrupted `FadeIn` leaves it partially transparent. Manim's
/// interruption semantics are not reproduced anywhere in this engine.
#[derive(Debug, Clone)]
pub struct Create {
    target: NodeId,
    start: Option<Vec<Point>>,
    duration: f64,
}

impl Create {
    pub fn new(target: NodeId) -> Self {
        Self {
            target,
            start: None,
            duration: 1.0,
        }
    }

    pub fn with_duration(mut self, seconds: f64) -> Self {
        self.duration = seconds;
        self
    }
}

impl Animation for Create {
    fn target(&self) -> Option<NodeId> {
        Some(self.target)
    }

    fn duration(&self) -> f64 {
        self.duration
    }

    fn begin(&mut self, tree: &SceneTree) {
        // The whole path, captured once; every frame is a prefix of this.
        self.start = tree.get(self.target).map(|m| m.points.clone());
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        let Some(start) = &self.start else { return };
        let Some(m) = tree.get_mut(self.target) else {
            return;
        };
        m.points = bezier::partial_path_points(start, 0.0, alpha);
        m.dirty.geometry = true;
    }
}

/// `Create` backwards: the path is erased from its end, then the object is gone.
///
/// Manim's `Uncreate` is `Create` with `reverse_rate_function` and `remover`
/// (`animation/creation.py:180-196`). The removal is the part that matters for
/// the picture: a partially drawn object left in the tree would be a stale node
/// in the accessibility tree and in `--inspect`, the same argument `FadeOut`'s
/// [`Animation::finish`] carries.
///
/// # How the reversal is spelled
///
/// Manim composes the reverse *inside* the rate function — `rate_func(1 - raw)`
/// (`animation.py:388-391`) — while this engine's scheduler hands an animation
/// its already-eased alpha. The port therefore reverses outside:
/// `interpolate` draws `1 - alpha`, i.e. `1 - rate(raw)`. Manim draws
/// `rate(1 - raw)`, so the two are identical exactly when the curve is
/// **point-symmetric** about `(0.5, 0.5)` — `f(1 - t) == 1 - f(t)`, which
/// `linear`, `ease_in_out` and `smooth` satisfy — and differ for `ease_in`,
/// `ease_out` and `overshoot`. Recovering `raw` is not an alternative: it needs
/// the inverse of a curve that is not injective (`overshoot`). Documented rather
/// than hidden, because "Uncreate is Create in reverse" is only true this
/// precisely for the curves the engine actually offers.
#[derive(Debug, Clone)]
pub struct Uncreate {
    target: NodeId,
    start: Option<Vec<Point>>,
    duration: f64,
}

impl Uncreate {
    pub fn new(target: NodeId) -> Self {
        Self {
            target,
            start: None,
            duration: 1.0,
        }
    }

    pub fn with_duration(mut self, seconds: f64) -> Self {
        self.duration = seconds;
        self
    }
}

impl Animation for Uncreate {
    fn target(&self) -> Option<NodeId> {
        Some(self.target)
    }

    fn duration(&self) -> f64 {
        self.duration
    }

    fn begin(&mut self, tree: &SceneTree) {
        self.start = tree.get(self.target).map(|m| m.points.clone());
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        let Some(start) = &self.start else { return };
        let Some(m) = tree.get_mut(self.target) else {
            return;
        };
        m.points = bezier::partial_path_points(start, 0.0, 1.0 - alpha);
        m.dirty.geometry = true;
    }

    /// The removal, matching `FadeOut`'s: the subtree goes with the object.
    fn finish(&mut self, tree: &mut SceneTree) {
        tree.remove_subtree(self.target);
    }
}

/// Pans and zooms the view.
///
/// The one animation that writes no mobject: it moves `SceneTree::camera`, which
/// is why the camera lives in the tree at all. It is otherwise an animation like
/// every other — a beat of its own via `SceneContext::play`, or one member of a
/// parallel beat (`play(camera_move, fade)`), and it takes the reduced-motion
/// branch through the same code path.
///
/// # Zoom is interpolated geometrically
///
/// `center` is lerped linearly; `zoom` is lerped **in log space**, so that equal
/// fractions of the duration multiply the zoom by equal factors. Zoom is a
/// multiplicative quantity: a linear 1 → 9 spends most of the animation outside
/// 5, which reads as a lurch at the end. (Manim's `CameraFrame` interpolates its
/// *width* linearly, which is the same mistake in reciprocal form; this engine
/// stores zoom, so it interpolates zoom the way it is measured.)
///
/// # `alpha == 1.0` is an exact assignment
///
/// The end state is written verbatim, not computed. `x0 + (x1 - x0) * 1.0` is
/// not always bitwise `x1`, and reduced motion lands a beat by evaluating
/// `interpolate(ease(1.0))` — so the frame a reduced-motion run settles on must be
/// *the* destination, not a value that rounds to it. `Transform` makes the same
/// distinction for its style fields (see the `INFINITY` test there), and the
/// goldens depend on both.
#[derive(Debug, Clone)]
pub struct CameraMove {
    to: Camera,
    from: Option<Camera>,
    duration: f64,
}

impl CameraMove {
    pub fn new(to: Camera) -> Self {
        Self {
            to,
            from: None,
            duration: 1.0,
        }
    }

    pub fn with_duration(mut self, seconds: f64) -> Self {
        self.duration = seconds;
        self
    }
}

impl Animation for CameraMove {
    fn duration(&self) -> f64 {
        self.duration
    }

    fn begin(&mut self, tree: &SceneTree) {
        // Wherever the view is now, not where it was when this was constructed:
        // a camera move may be queued several beats ahead of its turn.
        self.from = Some(tree.camera);
    }

    fn interpolate(&mut self, alpha: f64, tree: &mut SceneTree) {
        let Some(from) = self.from else { return };
        // See the doc: the endpoints are exact, the interior is interpolated.
        tree.camera = if alpha <= 0.0 {
            from
        } else if alpha >= 1.0 {
            self.to
        } else {
            Camera {
                center: (
                    lerp_scalar(from.center.0, self.to.center.0, alpha),
                    lerp_scalar(from.center.1, self.to.center.1, alpha),
                ),
                zoom: lerp_scalar(from.zoom.ln(), self.to.zoom.ln(), alpha).exp(),
            }
        };
    }
}

/// A pause. It writes nothing; it only occupies the clock.
///
/// Manim's `Wait` (`animation/animation.py:584-638`), and `Scene.wait` is nothing
/// but `play(Wait(run_time=duration))` (`scene/scene.py:1242-1276`). That is the
/// whole reason this is an `Animation` rather than a scheduling special case: it
/// keeps a beat uniformly "a set of animations", so the driver needs no separate
/// notion of a gap, and "is something playing?" stays true *across* a pause rather
/// than flickering to "finished" between the beats on either side of it.
///
/// `rate_func` is `linear`, Manim's default for this type (`:612`). Nothing
/// observable follows from that — `interpolate` is empty, so every curve gives the
/// same result — but it is the choice upstream made, and pinning it with one
/// assertion is cheaper than leaving a future reader to work out whether the
/// omission was meant.
#[derive(Debug, Clone)]
pub struct Wait {
    duration: f64,
}

impl Wait {
    /// A pause of `seconds` of virtual time. Manim's default is 1.0
    /// (`DEFAULT_WAIT_TIME`, `constants.py:178`), which is also what
    /// `Scene.wait()` uses when called with no argument.
    pub fn new(seconds: f64) -> Self {
        Self { duration: seconds }
    }
}

impl Animation for Wait {
    fn duration(&self) -> f64 {
        self.duration
    }

    fn interpolate(&mut self, _alpha: f64, _tree: &mut SceneTree) {}

    fn rate_func(&self) -> RateFunc {
        rate::linear
    }
}

/// `manim.utils.bezier.interpolate` (`utils/bezier.py:1067`) — `(1 - t) * a + t * b`.
///
/// Written in Manim's own order rather than as `a + (b - a) * t`. The two differ
/// in the last bits, and reproducing Manim's *rounding path* rather than only its
/// formula is this project's rule for anything it ports.
pub(crate) fn lerp_scalar(a: f64, b: f64, t: f64) -> f64 {
    (1.0 - t) * a + t * b
}

/// Carry a world point into the frame [`Mobject::rotate_about`] takes its pivot
/// in, or report that no such frame exists.
///
/// `kurbo`'s `Affine::inverse` is infallible (`affine.rs:426`): for a degenerate
/// matrix it divides by a zero determinant and hands back a transform of
/// infinities and `NaN`s, which then propagate into every point of the scene and
/// come out as a blank frame. So the determinant is checked here — zero or
/// non-finite — and the caller degrades to a no-op. "A scene scaled to nothing"
/// is a scene with no answer to "rotate about here", the same posture
/// [`Transform`] takes toward a ragged source.
fn to_parent_space(parent: Affine2, p: Point) -> Option<Point> {
    let det = parent.determinant();
    if !det.is_finite() || det == 0.0 {
        return None;
    }
    Some(parent.inverse() * p)
}

/// Componentwise interpolation of two affine transforms.
///
/// `kurbo` has no `lerp` for `Affine`, and building one out of `as_coeffs` keeps
/// the six coefficients in the same layout the renderer already expects.
pub(crate) fn lerp_affine(a: Affine2, b: Affine2, t: f64) -> Affine2 {
    let (a, b) = (a.as_coeffs(), b.as_coeffs());
    let mut out = [0.0; 6];
    for i in 0..6 {
        out[i] = a[i] + (b[i] - a[i]) * t;
    }
    Affine2::new(out)
}

/// One point's position at `alpha` when the morph follows an arc.
///
/// The two sides are folded through their own transforms first, because
/// [`path_along_arc`](rustim_math::paths::path_along_arc) describes a route in
/// the space the road is actually drawn in — Manim's points are world points, and
/// a straight evaluation of the local arrays would be a no-op for every morph
/// whose displacement lives in the deferred transform. The caller writes the
/// result back with an identity transform, which is what keeps the route honest.
///
/// Both ends are **written, not computed**. `path_along_arc(arc, a, b, 1.0)` is a
/// rotation that lands on `b` only to within rounding, so a settled animation
/// would leave the object a few ulps from its target; a scene that then
/// transforms *again* would drift. Manim computes both ends; this engine's
/// "the end state is written" rule is the older claim, and it wins.
pub(crate) fn path_point(
    arc: f64,
    from: Affine2,
    a: Point,
    to: Affine2,
    b: Point,
    alpha: f64,
) -> Point {
    if alpha <= 0.0 {
        return from * a;
    }
    if alpha >= 1.0 {
        return to * b;
    }
    rustim_math::paths::path_along_arc(arc, from * a, to * b, alpha)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_mobject::shapes;

    /// A 3-curve / 12-point triangle: `shapes.rs:365` pins the count,
    /// `tree.rs:797-815` the 4-points-per-curve layout.
    fn triangle() -> Mobject {
        Mobject::new(shapes::right_triangle(3.0, 3.0))
    }

    /// A 4-curve / 16-point circle — the shape that forces the triangle to grow.
    fn circle() -> Mobject {
        Mobject::new(shapes::circle(1.6))
    }

    /// A 16-point square, for the pairs that need equal lengths on both sides.
    fn square() -> Mobject {
        Mobject::new(shapes::square(2.6, 2.6))
    }

    fn scene(m: Mobject) -> (SceneTree, NodeId) {
        let mut tree = SceneTree::new();
        let id = tree.insert(m);
        (tree, id)
    }

    /// The `Box` forwarding trap, and it is not hypothetical here: the scheduler
    /// holds `Box<dyn Animation>`, so a missing forward makes a perpetual
    /// animation answer `false` — and then the scheduler divides its age by a
    /// period and it freezes at its start while `is_playing` stays true. That is
    /// the failure `perpetual` exists to prevent, arriving through the one
    /// container every beat is built from.
    #[test]
    fn a_boxed_perpetual_animation_is_still_perpetual() {
        let (_, id) = scene(square());
        let boxed: Box<dyn Animation> = Box::new(Rotating::new(id, 0.25));
        assert!(boxed.perpetual(), "the box must forward it");
        assert_eq!(boxed.duration(), 4.0, "a quarter turn a second");
    }

    /// The scheduler hands a perpetual animation its **age**, so `interpolate` is
    /// called with seconds. Two seconds at a quarter turn per second is half a
    /// turn, and the check is a geometric one — each corner lands on the opposite
    /// one — rather than the same expression written twice.
    #[test]
    fn a_perpetual_animation_reads_its_argument_as_seconds() {
        let (mut tree, id) = scene(square());
        let start = tree.world_anchors(id).unwrap();

        let mut anim = Rotating::new(id, 0.25);
        anim.begin(&tree);
        anim.interpolate(2.0, &mut tree);

        let turned = tree.world_anchors(id).unwrap();
        assert!(
            (turned[0].x + start[0].x).abs() < 1e-9 && (turned[0].y + start[0].y).abs() < 1e-9,
            "half a turn puts corner {:?} on the opposite one, not at {:?}",
            start[0],
            turned[0]
        );
    }

    #[test]
    fn a_perpetual_animations_duration_is_a_period_and_never_an_infinity() {
        let (_, id) = scene(square());
        assert_eq!(Rotating::new(id, 0.25).duration(), 4.0);
        assert_eq!(
            Rotating::new(id, -0.25).duration(),
            4.0,
            "the sign is the direction, not the length"
        );
        // A degenerate rate is a caller bug that the command layer refuses; here
        // it degrades to a nominal period rather than reporting an `∞`, which
        // would reach a `beat_started` event and JSON has no spelling for one.
        assert!(Rotating::new(id, 0.0).duration().is_finite());
        assert!(Rotating::new(id, f64::NAN).duration().is_finite());
    }

    /// The whole point of the concept, checked through the scheduler rather than
    /// by calling `interpolate` by hand: `age / duration` would be `age / 4` here,
    /// and the animation would creep round rather than turn — or, with a period of
    /// infinity, stand still.
    #[test]
    fn the_scheduler_never_finishes_a_perpetual_animation() {
        use crate::AnimationScheduler;

        let (mut tree, id) = scene(square());
        let start = tree.world_anchors(id).unwrap();
        let mut sched = AnimationScheduler::new();
        sched.add(Box::new(Rotating::new(id, 0.25)));

        for _ in 0..120 {
            sched.update(1.0 / 60.0, &mut tree);
        }

        assert!(
            !sched.is_empty(),
            "it has no end, so it is still on the clock"
        );
        let after = tree.world_anchors(id).unwrap();
        assert!(
            (after[0].x + start[0].x).abs() < 1e-6 && (after[0].y + start[0].y).abs() < 1e-6,
            "two seconds is half a turn: corner {:?} should be opposite {:?}",
            after[0],
            start[0]
        );
    }

    /// The end state is written, not computed: `x0 + (x1 - x0) * 1.0` is not
    /// always bitwise `x1`, and reduced motion lands a beat exactly here.
    #[test]
    fn a_camera_move_lands_exactly_on_its_end_state() {
        let mut tree = SceneTree::new();
        tree.camera = Camera::new((0.3, -0.7), 1.7);
        let target = Camera::new((-1.0 / 3.0, 2.0 / 7.0), 3.0);

        let mut anim = CameraMove::new(target);
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        assert_eq!(tree.camera, target);
    }

    /// The midpoint of 1 → 9 is 3, not 5: zoom is multiplicative.
    #[test]
    fn a_camera_move_zooms_geometrically() {
        let mut tree = SceneTree::new();
        let mut anim = CameraMove::new(Camera::new((0.0, 0.0), 9.0));
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);
        assert!(
            (tree.camera.zoom - 3.0).abs() < 1e-12,
            "midpoint zoom was {}",
            tree.camera.zoom
        );
    }

    /// `begin` captures where the view *is*, not where it was when the move was
    /// constructed: a beat may be queued long before its turn.
    #[test]
    fn a_camera_move_starts_from_the_camera_it_finds() {
        let mut tree = SceneTree::new();
        let mut anim = CameraMove::new(Camera::new((4.0, 4.0), 2.0));
        // Constructed before the camera moved, begun after.
        tree.camera = Camera::new((1.0, 0.0), 1.0);
        anim.begin(&tree);
        anim.interpolate(0.0, &mut tree);
        assert_eq!(tree.camera, Camera::new((1.0, 0.0), 1.0));
    }

    /// The boxed route is how every mixed beat plays. `finish` was the method a
    /// blanket impl silently missed once; this pins that the *new* animation
    /// survives the box too, rather than trusting the impl to have every method.
    #[test]
    fn a_boxed_camera_move_still_drives_the_tree() {
        use crate::AnimationScheduler;

        let mut tree = SceneTree::new();
        let target = Camera::new((2.0, -1.0), 4.0);
        let mut sched = AnimationScheduler::new();
        sched.add_boxed(Box::new(CameraMove::new(target).with_duration(0.5)));
        sched.update(1.0, &mut tree);
        assert_eq!(tree.camera, target);
    }

    /// Under reduced motion a beat is begun and driven to `ease(1.0)` once, not
    /// interpolated — so the camera must be *exactly* the destination there too.
    #[test]
    fn a_reduced_camera_move_lands_on_its_end_state_exactly() {
        use crate::{Motion, Sequence};

        let mut tree = SceneTree::new();
        tree.camera = Camera::new((0.0, 0.0), 1.0);
        let target = Camera::new((1.0 / 9.0, -2.0 / 11.0), 2.5);
        let mut seq = Sequence::new();
        seq.push([CameraMove::new(target).with_duration(0.8)]);
        let mut sched = crate::AnimationScheduler::new();
        assert!(
            seq.start_next(&mut sched, &mut tree, Motion::Reduced)
                .is_some()
        );
        assert_eq!(tree.camera, target);
    }

    #[test]
    fn lerp_affine_endpoints_are_exact() {
        let a = Affine2::translate((1.0, 2.0));
        let b = Affine2::scale(3.0) * Affine2::translate((5.0, 6.0));
        assert_eq!(lerp_affine(a, b, 0.0), a);
        assert_eq!(lerp_affine(a, b, 1.0), b);
    }

    #[test]
    fn lerp_affine_moves_a_point_linearly() {
        let a = Affine2::IDENTITY;
        let b = Affine2::translate((10.0, -4.0));
        let mid = lerp_affine(a, b, 0.5);
        assert_eq!(mid * Point::new(0.0, 0.0), Point::new(5.0, -2.0));
    }

    #[test]
    fn move_to_marks_geometry_dirty() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(vec![Point::new(0.0, 0.0)]));
        tree.get_mut(id).unwrap().dirty = Default::default();
        let mut anim = MoveTo::new(id, Affine2::translate((1.0, 0.0)));
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);
        assert!(tree.get(id).unwrap().dirty.geometry);
    }

    /// A point destination is absolute in *position*, not in matrix: the object
    /// keeps the scale it already had. `new` with `translate(p)` would have thrown
    /// it away, which is why both spellings exist.
    #[test]
    fn a_move_to_point_keeps_the_objects_scale() {
        let (mut tree, id) = scene(square());
        let centre = tree.get(id).unwrap().critical_point((0.0, 0.0)).unwrap();
        tree.get_mut(id)
            .unwrap()
            .set_transform(Affine2::scale_about(2.0, centre));

        let mut anim = MoveTo::to_point(id, Point::new(3.0, -1.0));
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        assert_eq!(
            tree.get(id).unwrap().critical_point((0.0, 0.0)),
            Some(Point::new(3.0, -1.0))
        );
        // The linear part is untouched: a side that was 2.6 is now 5.2.
        let width = width_of(&tree, id);
        assert!(
            (width - 5.2).abs() < 1e-12,
            "width was {width}, not the scaled 5.2"
        );
    }

    /// The span of the materialised anchors, which is what a caller sees.
    fn width_of(tree: &SceneTree, id: NodeId) -> f64 {
        let anchors = tree.world_anchors(id).expect("a square has anchors");
        let xs = anchors.iter().map(|p| p.x);
        xs.clone().fold(f64::NEG_INFINITY, f64::max) - xs.fold(f64::INFINITY, f64::min)
    }

    /// The destination is resolved in `begin`, so a transform written between
    /// construction and the first frame is the one that is preserved — which is the
    /// case a `Succession` creates.
    #[test]
    fn a_move_to_point_resolves_against_the_state_it_finds() {
        let (mut tree, id) = scene(square());
        let mut anim = MoveTo::to_point(id, Point::new(2.0, 0.0));
        // An earlier beat scaled the object after this animation was built.
        let centre = tree.get(id).unwrap().critical_point((0.0, 0.0)).unwrap();
        tree.get_mut(id)
            .unwrap()
            .set_transform(Affine2::scale_about(3.0, centre));

        anim.begin(&tree);
        anim.interpolate(0.0, &mut tree);
        let start_centre = tree.get(id).unwrap().critical_point((0.0, 0.0)).unwrap();
        assert_eq!(
            start_centre, centre,
            "frame 0 is where the object already was"
        );

        anim.interpolate(1.0, &mut tree);
        assert_eq!(
            tree.get(id).unwrap().critical_point((0.0, 0.0)),
            Some(Point::new(2.0, 0.0))
        );
        let width = width_of(&tree, id);
        assert!(
            (width - 7.8).abs() < 1e-12,
            "the ×3 scale must survive the move; width was {width}"
        );
    }

    /// **The guard that a morphing scene actually morphs.**
    ///
    /// `align_points` is the only thing that reconciles 12 points against 16, and
    /// it runs once, in `begin`. A port that forgot to call it, or that aligned
    /// only when the lengths already matched, would lerp the first 12 of 16 and
    /// leave the array at its original length — which still *draws*, so the only
    /// way to see it is to count.
    #[test]
    fn transform_aligns_once_and_the_source_grows_at_the_midpoint() {
        let (mut tree, id) = scene(triangle());
        assert_eq!(tree.get(id).unwrap().points.len(), 12, "3 curves to start");

        let mut anim = Transform::new(id, circle());
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        assert_eq!(
            tree.get(id).unwrap().points.len(),
            16,
            "the source must be padded to the target's curve count"
        );
    }

    /// The endpoints are exact, both of them, so a settled scene *is* its target
    /// rather than a near-miss of it.
    ///
    /// The pair here is deliberately equal-length (16 against 16): with a source
    /// that has to be padded, `alpha = 0` is the *aligned* source and not the
    /// original array, which would make the assertion restate the alignment
    /// rather than the lerp. Growth is the previous test's business.
    #[test]
    fn transform_endpoints_are_exact() {
        let (mut tree, id) = scene(square());
        let target = circle();
        let before = tree.get(id).unwrap().points.clone();

        let mut anim = Transform::new(id, target.clone());
        anim.begin(&tree);

        anim.interpolate(0.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().points, before);

        anim.interpolate(1.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().points, target.points);
        assert_eq!(tree.get(id).unwrap().style, target.style);
    }

    /// The affine is part of what morphs. Drop it and the shape changes in place
    /// instead of travelling — a defect the settled golden cannot see, because at
    /// the endpoint the source has *reached* the target's transform either way.
    #[test]
    fn transform_lerps_the_deferred_transform() {
        let (mut tree, id) = scene(triangle());
        let mut target = circle();
        target.transform = Affine2::translate((6.0, 0.0));

        let mut anim = Transform::new(id, target);
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        assert_eq!(
            tree.get(id).unwrap().transform,
            Affine2::translate((3.0, 0.0)),
            "halfway between the source's identity and the target's offset"
        );
    }

    /// `path_arc` has to bend a **translation**, not only a change of shape.
    ///
    /// "Move this there along an arc" is the request the parameter exists for,
    /// and a translated target keeps its motion in the deferred transform. Arcing
    /// the local arrays alone would draw a straight line with no sign that the
    /// angle was ignored, which is the failure this test exists to prevent.
    #[test]
    fn a_path_arc_bends_a_translation() {
        use std::f64::consts::PI;

        let (mut tree, id) = scene(square());
        let mut target = square();
        target.transform = Affine2::translate((4.0, 0.0));

        let mut anim = Transform::new(id, target).with_path_arc(PI);
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        let centre = tree.get(id).unwrap().critical_point((0.0, 0.0)).unwrap();
        // A half-turn over a 4-unit chord passes one radius (2) along
        // `cross(OUT, v)`; for v = (+4, 0) that is −y. A straight morph would be
        // at (2, 0).
        assert!(
            (centre.x - 2.0).abs() < 1e-12 && (centre.y + 2.0).abs() < 1e-12,
            "{centre:?}"
        );
    }

    /// Both ends are written, not computed: the start is the object's own
    /// geometry and the end is the target's, exactly. `path_along_arc` at
    /// `alpha = 1` is a rotation that lands on the endpoint only to within
    /// rounding, and a morph that settles a few ulps off becomes a drift under a
    /// second morph.
    #[test]
    fn a_path_arc_writes_both_ends_exactly() {
        use std::f64::consts::FRAC_PI_2;

        let (mut tree, id) = scene(square());
        let before = tree.get(id).unwrap().materialize();
        let mut target = square();
        target.transform = Affine2::translate((4.0, 0.0));
        let end = target.materialize();

        let mut anim = Transform::new(id, target).with_path_arc(FRAC_PI_2);
        anim.begin(&tree);
        anim.interpolate(0.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().materialize(), before);
        anim.interpolate(1.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().materialize(), end);
    }

    /// Below `STRAIGHT_PATH_THRESHOLD` an angle is Manim's straight path, and it
    /// must agree with the un-arced morph. Compared in world space because the
    /// two store it differently — the arc branch folds the transform into the
    /// points, the plain one leaves it deferred.
    #[test]
    fn a_sub_threshold_path_arc_draws_the_same_line_as_no_arc() {
        let run = |arc: Option<f64>| {
            let (mut tree, id) = scene(square());
            let mut target = square();
            target.transform = Affine2::translate((4.0, 2.0));
            let mut anim = Transform::new(id, target);
            if let Some(arc) = arc {
                anim = anim.with_path_arc(arc);
            }
            anim.begin(&tree);
            anim.interpolate(0.5, &mut tree);
            tree.get(id).unwrap().materialize()
        };

        let straight = run(None);
        let tiny = run(Some(rustim_math::paths::STRAIGHT_PATH_THRESHOLD / 2.0));
        assert_eq!(straight.len(), tiny.len());
        for (a, b) in straight.iter().zip(&tiny) {
            assert!(
                (a.x - b.x).abs() < 1e-12 && (a.y - b.y).abs() < 1e-12,
                "{a:?} vs {b:?}"
            );
        }
    }

    /// The donor is read-only.
    ///
    /// Manim keeps a `target_copy` precisely so the caller's target survives the
    /// animation (`transform.py:201-203`); here taking the target by value makes
    /// that structural, so this test is cheap insurance against a refactor that
    /// "saves a clone" by aligning *into* `self.target` — which would leave the
    /// stored donor holding a padded form that no longer describes the shape the
    /// caller handed over. It passes trivially today; it is here to keep failing
    /// to be an option.
    #[test]
    fn transform_leaves_the_target_untouched() {
        let (mut tree, id) = scene(triangle());
        let target = circle();

        let mut anim = Transform::new(id, target.clone());
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);
        anim.interpolate(1.0, &mut tree);

        assert_eq!(anim.target.points, target.points);
        assert_eq!(anim.target.style, target.style);
        assert_eq!(anim.target.transform, target.transform);
    }

    /// `align_points` returns equal-length input **unchanged** before it does
    /// anything else — upstream's own `TODO` calls that shortcut over-eager, and
    /// step 1 reproduced it rather than tidying it. So a port that read "the
    /// arrays are already the same length" as "nothing to do, return early" would
    /// drop the lerp entirely on exactly the shapes it is commonest for.
    #[test]
    fn transform_of_equal_length_paths_still_lerps() {
        let (mut tree, id) = scene(circle());
        let mut target = circle();
        // Same length, different geometry: move every point, keeping 16 of them.
        for p in &mut target.points {
            p.x += 4.0;
        }

        let mut anim = Transform::new(id, target.clone());
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        let source_x = circle().points[0].x;
        let got = &tree.get(id).unwrap().points;
        assert_eq!(got.len(), 16);
        // The midpoint of `source_x` and `target.points[0].x`, compared with a
        // tolerance rather than for bit equality: `0.5 * a + 0.5 * b` is not
        // guaranteed to land on the nearest float to the exact average, and the
        // thing under test is that the lerp *ran*, not which side of a half-ulp
        // it landed on.
        let want = Point::new(source_x + 2.0, target.points[0].y);
        assert!(
            (got[0].x - want.x).abs() < 1e-12 && (got[0].y - want.y).abs() < 1e-12,
            "expected about {want:?}, got {:?}",
            got[0]
        );
        // ...and it really did move: this is the assertion a port that honoured
        // `align_points`'s equal-length early return as "nothing to do" fails.
        assert!((got[0].x - source_x).abs() > 1.0, "the lerp was skipped");
    }

    /// Colour and opacity travel with the geometry, and the final frame is the
    /// target's style *by construction* rather than by arithmetic.
    #[test]
    fn transform_lerps_style_and_opacity() {
        let (mut tree, id) = scene(triangle());
        tree.get_mut(id).unwrap().style = Style {
            fill: rustim_color::BLUE,
            fill_opacity: 0.25,
            stroke_width: 2.0,
            ..Style::default()
        };
        tree.get_mut(id).unwrap().opacity = 0.5;

        let mut target = circle();
        target.style = Style {
            fill: rustim_color::YELLOW,
            fill_opacity: 0.75,
            stroke_width: 10.0,
            ..Style::default()
        };
        target.opacity = 1.0;

        let mut anim = Transform::new(id, target.clone());
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        let mid = tree.get(id).unwrap();
        assert_eq!(mid.style.fill_opacity, 0.5);
        assert_eq!(mid.style.stroke_width, 6.0);
        assert_eq!(mid.opacity, 0.75);
        for i in 0..4 {
            let want =
                0.5 * rustim_color::BLUE.components[i] + 0.5 * rustim_color::YELLOW.components[i];
            assert_eq!(mid.style.fill.components[i], want, "channel {i}");
        }

        anim.interpolate(1.0, &mut tree);
        let end = tree.get(id).unwrap();
        assert_eq!(end.style, target.style);
        assert_eq!(end.opacity, target.opacity);
    }

    #[test]
    fn transform_marks_geometry_and_style_dirty() {
        let (mut tree, id) = scene(triangle());
        tree.get_mut(id).unwrap().dirty = Default::default();

        let mut anim = Transform::new(id, circle());
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        let dirty = tree.get(id).unwrap().dirty;
        assert!(dirty.geometry, "the point array was rewritten");
        assert!(dirty.style, "the paint may have moved");
    }

    /// A ragged source is refused by `align_points`, and `begin` has no error
    /// channel to report through — so this has to degrade to "nothing happens",
    /// not to a panic or a half-written array.
    #[test]
    fn transform_refuses_a_ragged_source_without_panicking() {
        let ragged = Mobject::new((0..6).map(|i| Point::new(i as f64, 0.0)).collect());
        let (mut tree, id) = scene(ragged.clone());

        let mut anim = Transform::new(id, circle());
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        assert_eq!(tree.get(id).unwrap().points, ragged.points);
        assert_eq!(tree.get(id).unwrap().transform, ragged.transform);
    }

    /// The same posture as `missing_node_is_not_a_panic`: a command may name a
    /// node that has since been deleted, and the AI layer can reach this.
    #[test]
    fn transform_is_a_no_op_on_a_missing_node() {
        let (mut tree, id) = scene(triangle());
        tree.clear();
        let mut anim = Transform::new(id, circle());
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);
    }

    // ---- the three P2 step 5 animations -----------------------------------

    #[test]
    fn fade_out_drives_opacity_to_zero() {
        let (mut tree, id) = scene(square());
        tree.get_mut(id).unwrap().opacity = 0.5;
        let mut anim = FadeOut::new(id);
        // From wherever it is, not from 1.0 — an interrupted fade must not jump.
        anim.begin(&tree);
        anim.interpolate(0.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().opacity, 0.5);
        anim.interpolate(1.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().opacity, 0.0);
    }

    /// **The blanket-impl guard.**
    ///
    /// `FadeOut`'s removal happens in [`Animation::finish`], which has a default
    /// no-op — so if `impl Animation for Box<T>` failed to forward it, every call
    /// reaching a boxed animation would resolve to that default and the object
    /// would never leave the tree, while a direct `FadeOut::finish` call in a test
    /// like this one would still work. The scheduler only ever holds
    /// `Box<dyn Animation>`, so driving it through `AnimationScheduler::add` — as
    /// a scene does — is the only way this test can *be* the guard. Copied from
    /// the `debug_assert!` note in CLAUDE.md: the failure mode is a release-only
    /// or container-only defect that no unit call can see.
    #[test]
    fn fade_out_removes_its_target_when_the_scheduler_finishes_it() {
        let mut tree = SceneTree::new();
        let kept = tree.insert(Mobject::new(shapes::circle(1.0)));
        let group = tree.insert(Mobject::new(shapes::square(2.0, 2.0)));
        let child = tree.insert(Mobject::new(shapes::circle(0.5)));
        assert!(tree.reparent(child, group), "the child must be nested");

        let mut sched = crate::AnimationScheduler::new();
        sched.add(FadeOut::new(group).with_duration(0.5));
        for _ in 0..40 {
            sched.update(1.0 / 60.0, &mut tree);
        }

        assert!(sched.is_empty());
        assert!(tree.get(group).is_none(), "the faded node must be gone");
        assert!(
            tree.get(child).is_none(),
            "a fade removes the family, not just the group"
        );
        assert!(tree.get(kept).is_some(), "the sibling is untouched");
        assert_eq!(
            tree.root_ids(),
            &[kept],
            "no orphan was promoted to the root"
        );
    }

    /// A triangle is three curves; at half progress the first is whole and the
    /// second is partly traced, so the object wears `partial(first) ++
    /// partial(second)` — eight points, not six — and at `alpha = 1` it is
    /// restored exactly.
    #[test]
    fn create_draws_the_path_on_and_ends_exactly_whole() {
        let (mut tree, id) = scene(triangle());
        let original = tree.get(id).unwrap().points.clone();
        assert_eq!(original.len(), 12);

        let mut anim = Create::new(id);
        anim.begin(&tree);

        // The empty prefix is a degenerate four-point curve at the start.
        anim.interpolate(0.0, &mut tree);
        let at_zero = tree.get(id).unwrap().points.clone();
        assert_eq!(at_zero.len(), 4);
        assert!(at_zero.iter().all(|p| *p == original[0]));

        anim.interpolate(0.5, &mut tree);
        let half = tree.get(id).unwrap().points.clone();
        assert_eq!(half.len(), 8, "one whole curve plus the next's prefix");
        assert_eq!(half[0], original[0], "the drawn path starts where it did");
        assert_eq!(half[3], original[3], "the first curve is complete");

        anim.interpolate(1.0, &mut tree);
        assert_eq!(
            tree.get(id).unwrap().points,
            original,
            "the last frame must be a bit-identical restore, not a re-evaluated curve"
        );
    }

    /// `Uncreate` erases from the end and takes the object with it. Both halves
    /// are asserted: a transparently *drawn* object left in the tree would pass
    /// every pixel test, and `--inspect` is the only place that could show it.
    #[test]
    fn uncreate_erases_the_path_and_removes_the_object() {
        let (mut tree, id) = scene(triangle());
        let original = tree.get(id).unwrap().points.clone();

        let mut anim = Uncreate::new(id);
        anim.begin(&tree);

        anim.interpolate(0.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().points, original, "full at the start");

        anim.interpolate(0.5, &mut tree);
        let half = tree.get(id).unwrap().points.clone();
        assert!(half.len() < original.len(), "{half:?}");
        assert_eq!(
            half[0], original[0],
            "the remaining part is the path's head"
        );

        anim.interpolate(1.0, &mut tree);
        assert_eq!(
            tree.get(id).unwrap().points.len(),
            4,
            "collapsed at the end"
        );

        anim.finish(&mut tree);
        assert!(tree.get(id).is_none(), "the erased node must be gone");
    }

    /// Through the scheduler — the route a real beat takes, and the one the
    /// blanket `Box<T>` impl's `finish` forwarding is for. `Uncreate` is the
    /// second remover in the crate, so it guards the same trap `FadeOut` does.
    #[test]
    fn a_boxed_uncreate_removes_its_target_when_the_scheduler_finishes_it() {
        use crate::AnimationScheduler;

        let (mut tree, id) = scene(Mobject::new(shapes::circle(1.0)));
        let mut sched = AnimationScheduler::new();
        sched.add_boxed(Box::new(Uncreate::new(id).with_duration(0.5)));
        for _ in 0..40 {
            sched.update(1.0 / 60.0, &mut tree);
        }
        assert!(sched.is_empty());
        assert!(tree.get(id).is_none(), "the erased node must be gone");
    }

    /// The reduced-motion branch never reaches the scheduler, so it depends on
    /// `Sequence` driving each member once and finishing it. A `Create` must
    /// land whole and an `Uncreate` must remove.
    #[test]
    fn reduced_motion_lands_create_whole_and_removes_an_uncreate() {
        use crate::{AnimationScheduler, Motion, Sequence};

        let (mut tree, whole) = scene(triangle());
        let original = tree.get(whole).unwrap().points.clone();
        let removed = tree.insert(Mobject::new(shapes::square(2.0, 2.0)));

        let mut seq = Sequence::new();
        seq.push([Create::new(whole).with_duration(0.4)]);
        seq.push([Uncreate::new(removed).with_duration(0.4)]);
        let mut sched = AnimationScheduler::new();
        assert!(
            seq.start_next(&mut sched, &mut tree, Motion::Reduced)
                .is_some()
        );
        assert!(
            seq.start_next(&mut sched, &mut tree, Motion::Reduced)
                .is_some()
        );

        assert_eq!(
            tree.get(whole).unwrap().points,
            original,
            "a reduced Create lands on the whole path"
        );
        assert!(tree.get(removed).is_none(), "a reduced Uncreate removes");
    }

    /// A quarter turn in two halves is a 45° rotation, not the componentwise
    /// halfway matrix — the difference between a rigid motion and a shear.
    ///
    /// The assertion is stated in a way that cannot be satisfied by the matrix
    /// lerp: every anchor keeps its distance from the pivot (a rotation is an
    /// isometry), and the counterfactual is computed explicitly below.
    #[test]
    fn rotate_interpolates_the_angle_and_not_the_matrix() {
        let (mut tree, id) = scene(Mobject::new(shapes::square(2.0, 2.0)));
        let quarter = std::f64::consts::FRAC_PI_2;

        let mut anim = Rotate::new(id, quarter);
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);
        let t = tree.get(id).unwrap().transform;

        // The pivot is the square's centre, which for a centred square is the
        // origin, so the distance from the corner to the pivot is √2.
        let p = t * Point::new(1.0, 1.0);
        let r = (p.x * p.x + p.y * p.y).sqrt();
        assert!(
            (r - std::f64::consts::SQRT_2).abs() < 1e-12,
            "a rotation preserves distance from the pivot, got {r}"
        );

        // The angle at `alpha = 0.5` is exactly a 45° rotation: `0.5 * (π/2)` is
        // `π/4` to the last bit, so the two transforms are built by the same
        // constructor from equal inputs.
        assert_eq!(
            t,
            Affine2::rotate_about(std::f64::consts::FRAC_PI_4, Point::new(0.0, 0.0))
        );

        // ...and this is what the animation is *not* doing. The affine lerp of
        // the same quarter turn lands the corner at distance 1, inside the arc.
        let lerped = lerp_affine(Affine2::IDENTITY, Affine2::rotate(quarter), 0.5);
        let q = lerped * Point::new(1.0, 1.0);
        let r_lerp = (q.x * q.x + q.y * q.y).sqrt();
        assert!(
            (r_lerp - 1.0).abs() < 1e-12,
            "the counterfactual must really be the shrink; got {r_lerp}"
        );
    }

    /// A settled rotation *is* the rotation, at both ends, and composes with the
    /// transform the object already had rather than replacing it.
    #[test]
    fn rotate_endpoints_are_exact_and_compose() {
        let (mut tree, id) = scene(square());
        let start = Affine2::translate((3.0, -1.0));
        tree.get_mut(id).unwrap().set_transform(start);
        let centre = tree.get(id).unwrap().critical_point((0.0, 0.0)).unwrap();

        let angle = 0.7;
        let mut anim = Rotate::new(id, angle);
        anim.begin(&tree);

        anim.interpolate(0.0, &mut tree);
        assert_eq!(
            tree.get(id).unwrap().transform,
            start,
            "alpha 0 is the start"
        );

        anim.interpolate(1.0, &mut tree);
        assert_eq!(
            tree.get(id).unwrap().transform,
            Affine2::rotate_about(angle, centre) * start,
            "the rotation composes onto the move rather than undoing it"
        );
    }

    /// An explicit pivot is a **world** point, and the animation is an isometry
    /// about it.
    ///
    /// The pivot's preimage under the object's own transform is the local point
    /// that stays put — here the square sits at `(4, 0)` and the pivot is its own
    /// centre, whose local preimage is the origin — and every other local point
    /// keeps its world distance to it. That is what distinguishes "rotate about
    /// this point" from "rotate about the object's centre and then translate",
    /// which a single fixed-point assertion would not.
    #[test]
    fn rotate_about_a_named_world_point_keeps_that_point_fixed() {
        let (mut tree, id) = scene(Mobject::new(shapes::square(2.0, 2.0)));
        tree.get_mut(id)
            .unwrap()
            .set_transform(Affine2::translate((4.0, 0.0)));
        let pivot = Point::new(4.0, 0.0);

        let mut anim = Rotate::new(id, 1.1).about_point(pivot);
        anim.begin(&tree);
        for i in 0..=10 {
            anim.interpolate(i as f64 / 10.0, &mut tree);
            let t = tree.get(id).unwrap().transform;
            let fixed = t * Point::new(0.0, 0.0);
            assert!(
                (fixed.x - pivot.x).abs() < 1e-9 && (fixed.y - pivot.y).abs() < 1e-9,
                "the point at the pivot moved to {fixed:?} at alpha {}",
                i as f64 / 10.0
            );
            let corner = t * Point::new(1.0, 1.0);
            let r = ((corner.x - pivot.x).powi(2) + (corner.y - pivot.y).powi(2)).sqrt();
            assert!(
                (r - std::f64::consts::SQRT_2).abs() < 1e-9,
                "a rotation keeps every distance to the pivot, got {r}"
            );
        }
    }

    /// A nested object's world pivot has to be carried into the parent's space
    /// through the inverse, and the degenerate case has to degrade to a no-op
    /// instead of writing `NaN`s through the scene.
    #[test]
    fn rotate_of_a_nested_object_uses_the_parents_space() {
        // Parent at (5, 0); the child's world pivot (5, 0) is its parent-space
        // origin, so a rotation there spins it about its own centre.
        let mut tree = SceneTree::new();
        let parent = tree.insert(Mobject::new(vec![Point::new(0.0, 0.0)]));
        let child = tree.insert(Mobject::new(shapes::square(2.0, 2.0)));
        tree.get_mut(parent)
            .unwrap()
            .set_transform(Affine2::translate((5.0, 0.0)));
        assert!(tree.reparent(child, parent));

        let pivot = Point::new(5.0, 0.0);
        let mut anim = Rotate::new(child, std::f64::consts::FRAC_PI_2).about_point(pivot);
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        // The child's corner (1, 0) is at (6, 0) before; a quarter turn about the
        // world point (5, 0) lands it at (5, 1).
        let t = tree.parent_transform(child) * tree.get(child).unwrap().transform;
        let p = t * Point::new(1.0, 0.0);
        assert!(
            (p.x - 5.0).abs() < 1e-9 && (p.y - 1.0).abs() < 1e-9,
            "expected (5, 1), got {p:?}"
        );

        // A parent scaled to nothing has no inverse. `kurbo`'s `inverse` would
        // hand back infinities; the guard makes the whole animation a no-op.
        let mut tree = SceneTree::new();
        let parent = tree.insert(Mobject::new(vec![Point::new(0.0, 0.0)]));
        let child = tree.insert(Mobject::new(shapes::square(2.0, 2.0)));
        tree.get_mut(parent)
            .unwrap()
            .set_transform(Affine2::scale(0.0));
        assert!(tree.reparent(child, parent));

        let mut anim =
            Rotate::new(child, std::f64::consts::FRAC_PI_2).about_point(Point::new(0.0, 0.0));
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);
        let after = tree.get(child).unwrap().transform;
        assert_eq!(
            after,
            Affine2::IDENTITY,
            "a degenerate parent makes it a no-op"
        );
        assert!(after.as_coeffs().iter().all(|c| c.is_finite()));
    }

    /// Scaling is about the object's own centre: the centre does not move, the
    /// extent grows by exactly the factor, and no pivot argument is needed.
    #[test]
    fn scale_is_in_place() {
        let (mut tree, id) = scene(Mobject::new(shapes::square(2.0, 2.0)));
        tree.get_mut(id)
            .unwrap()
            .set_transform(Affine2::translate((3.0, 0.0)));
        let centre = tree.get(id).unwrap().critical_point((0.0, 0.0)).unwrap();
        assert_eq!(centre, Point::new(3.0, 0.0));
        let before = tree.world_bounds(id).unwrap().width();

        let mut anim = Scale::new(id, 2.0);
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        let after = tree.get(id).unwrap();
        assert_eq!(
            after.critical_point((0.0, 0.0)).unwrap(),
            centre,
            "an in-place scale must not move the centre"
        );
        let width = tree.world_bounds(id).unwrap().width();
        assert!(
            (width - 2.0 * before).abs() < 1e-9,
            "extent {before} -> {width}, expected twice"
        );
    }

    /// The factor is interpolated **linearly**, so ×4 halfway through is ×2.5 —
    /// not the ×2 a geometric reading of "halfway between ×1 and ×4" suggests.
    /// Manim's `interpolate(1, factor, alpha)` is arithmetic in the factor
    /// (`mobject.py:1454`); the exponential reading is a different animation, and
    /// this pins which one we ship.
    #[test]
    fn scale_interpolates_the_factor_linearly() {
        let (mut tree, id) = scene(Mobject::new(shapes::square(2.0, 2.0)));

        let mut anim = Scale::new(id, 4.0);
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        let p = tree.get(id).unwrap().transform * Point::new(1.0, 1.0);
        assert!(
            (p.x - 2.5).abs() < 1e-12 && (p.y - 2.5).abs() < 1e-12,
            "×4 at alpha = 0.5 is ×2.5, got {p:?}"
        );
        assert!((p.x - 2.0).abs() > 1e-12, "and not the geometric mean ×2");

        anim.interpolate(0.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().transform, Affine2::IDENTITY);
        anim.interpolate(1.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().transform, Affine2::scale(4.0));
    }

    /// The posture every animation here shares: a command may name a node that
    /// has since been deleted, so this has to be a no-op rather than a panic or a
    /// half-written tree. `FadeOut`'s `finish` is included because it is the one
    /// that touches the tree's shape rather than a field.
    #[test]
    fn the_three_new_animations_are_no_ops_on_a_missing_node() {
        let (mut tree, id) = scene(square());
        tree.clear();

        let mut fade = FadeOut::new(id);
        fade.begin(&tree);
        fade.interpolate(0.5, &mut tree);
        fade.finish(&mut tree);

        let mut rotate = Rotate::new(id, 1.0);
        rotate.begin(&tree);
        rotate.interpolate(0.5, &mut tree);

        let mut scale = Scale::new(id, 2.0);
        scale.begin(&tree);
        scale.interpolate(0.5, &mut tree);

        assert!(tree.is_empty());
    }

    /// `Animate` starts from the value it *finds*, not from the value at
    /// construction — and it lands exactly on the target, which is what reduced
    /// motion (`ease(1.0)`) and a replayed run sit on.
    #[test]
    fn an_animate_starts_from_the_value_it_finds_and_lands_exactly() {
        let (mut tree, id) = scene(square());
        tree.get_mut(id).unwrap().opacity = 0.25;

        let mut anim = Animate::new(id, StyleProperty::Opacity(0.75));
        // Constructed before the object was at 0.25; `begin` is what matters.
        anim.begin(&tree);
        anim.interpolate(0.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().opacity, 0.25);

        anim.interpolate(0.5, &mut tree);
        assert!((tree.get(id).unwrap().opacity - 0.5).abs() < 1e-12);

        anim.interpolate(1.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().opacity, 0.75);
    }

    /// Two `animate`s in one succession compose: the second's `begin` sees the
    /// first's end state, so 1 → 0.5 then 0.5 → 0.2 is a monotone fade rather
    /// than a snap back to 1.
    #[test]
    fn a_second_animate_starts_where_the_first_ended() {
        let (mut tree, id) = scene(square());

        let mut first = Animate::new(id, StyleProperty::Opacity(0.5));
        first.begin(&tree);
        first.interpolate(1.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().opacity, 0.5);

        let mut second = Animate::new(id, StyleProperty::Opacity(0.2));
        second.begin(&tree);
        second.interpolate(0.5, &mut tree);
        assert!((tree.get(id).unwrap().opacity - 0.35).abs() < 1e-12);
    }

    /// A channel animation touches its own field and nothing else. `opacity` and
    /// `fill_opacity` compose multiplicatively at draw time, so a bug that wrote
    /// both would show as a squared fade — this is the unit half of that claim.
    #[test]
    fn an_animate_moves_only_its_own_channel() {
        let (mut tree, id) = scene(square());
        {
            let m = tree.get_mut(id).unwrap();
            m.opacity = 0.5;
            m.style.fill_opacity = 0.8;
            m.style.stroke_opacity = 0.4;
            m.style.stroke_width = 0.1;
        }

        let mut anim = Animate::new(id, StyleProperty::FillOpacity(0.2));
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        let m = tree.get(id).unwrap();
        assert!((m.style.fill_opacity - 0.5).abs() < 1e-12);
        assert_eq!(m.opacity, 0.5);
        assert_eq!(m.style.stroke_opacity, 0.4);
        assert_eq!(m.style.stroke_width, 0.1);
    }

    /// The colour channel blends per sRGB component, the same arithmetic
    /// `Style::lerp` uses — not a hue rotation, and not a linear-light
    /// conversion wrapped around a lerp the source does not do.
    #[test]
    fn a_stroke_colour_animate_blends_each_channel() {
        let (mut tree, id) = scene(square());
        tree.get_mut(id).unwrap().style.stroke = Srgb::from_rgba8(255, 0, 0, 255);

        let mut anim = Animate::new(
            id,
            StyleProperty::StrokeColor(Srgb::from_rgba8(0, 0, 255, 255)),
        );
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        let [r, g, b, a] = tree.get(id).unwrap().style.stroke.components;
        // Red -> blue: the shared channel (green) stays at zero, the two that
        // move meet in the middle, and alpha (255 on both ends) is untouched.
        for (name, v, want) in [("r", r, 0.5), ("g", g, 0.0), ("b", b, 0.5), ("a", a, 1.0)] {
            assert!((v - want).abs() < 1e-6, "{name} = {v}, expected {want}");
        }
    }

    /// The boxed route is how every beat actually plays, so the new animation
    /// has to survive `Box<dyn Animation>` like the others.
    #[test]
    fn a_boxed_animate_still_drives_the_tree() {
        use crate::AnimationScheduler;

        let (mut tree, id) = scene(square());
        let mut sched = AnimationScheduler::new();
        sched.add_boxed(Box::new(
            Animate::new(id, StyleProperty::Opacity(0.2)).with_duration(0.5),
        ));
        sched.update(1.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().opacity, 0.2);
    }

    /// The shared posture: a deleted target is a no-op, not a panic.
    #[test]
    fn an_animate_is_a_no_op_on_a_missing_node() {
        let (mut tree, id) = scene(square());
        tree.clear();

        let mut anim = Animate::new(id, StyleProperty::Opacity(0.2));
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);
        assert!(tree.is_empty());
    }
}
