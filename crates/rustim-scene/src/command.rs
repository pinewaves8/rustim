//! The AI/host command layer: the engine's public mutation surface as data.

use serde::{Deserialize, Serialize};

use crate::a11y::SemanticTree;
use crate::check::CheckReport;
use crate::event::EventKind;
use crate::plan::{Plan, PlanStatus};
use rustim_anim::{
    Animate, Animation, AnimationGroup, Create, FadeIn, FadeOut, MoveTo, Rotate, Rotating, Scale,
    StyleProperty, Succession, TransformGroup, Uncreate, WithRate,
};
use rustim_math::rate::Rate;
use rustim_math::{Affine2, Camera, Point, Viewport};
use rustim_mobject::shapes::{MAX_TICKS, ShapeKind, ShapeParams};
use rustim_mobject::{Mobject, NodeId, NodeKind, SceneTree};

/// The version of the wire protocol this build speaks.
///
/// Integer rather than a date or a semver string because its only job is a
/// compatibility check: a client either speaks this number or is refused at
/// `hello` with a reason it can print. Same-major additions are backward
/// compatible (new commands, new optional fields, new capability names); a
/// meaning change or a removal is a new number. See `驱动协议v1.md` §6.
pub const PROTOCOL_VERSION: u32 = 1;

/// How long an animation runs when the command did not say.
///
/// Manim's `DEFAULT_ANIMATION_RUN_TIME` (`animation/animation.py:26`), which is
/// also what `Animation.__init__`'s `run_time` defaults to (`:132`). It matches
/// the defaults on `FadeIn`/`MoveTo` themselves, so a command that omits
/// `duration` and a scene that calls the constructor directly agree by
/// construction rather than by two people picking 1.0.
const DEFAULT_ANIMATION_RUN_TIME: f64 = 1.0;

/// The gap [`Command::NextTo`] uses when the command did not name one.
///
/// Manim's `SMALL_BUFF` (`constants.py:169`), in world units — the same 8-unit
/// frame the rest of the engine measures in, so the number carries over
/// unchanged.
const DEFAULT_NEXT_TO_BUFF: f64 = 0.1;

/// The gap [`Command::Arrange`] uses when the command did not name one.
///
/// Manim's `DEFAULT_MOBJECT_TO_MOBJECT_BUFFER` (`constants.py:174`), which is
/// `MED_SMALL_BUFF = 0.25` (`:170`). Measured rather than assumed: the value
/// is a quarter of a world unit, not the half a first guess suggests.
const DEFAULT_ARRANGE_BUFF: f64 = 0.25;

/// How far [`Command::Highlight`] dims the rest of the scene when the command
/// did not say.
///
/// A **factor**, not an absolute opacity, and a quarter rather than a half: the
/// subject has to stand out, and dimming to a quarter still leaves the
/// surrounding picture readable enough to keep the subject in context. The
/// value is this protocol's own — Manim has no spotlight — so it is stated and
/// the caller can override it.
pub(crate) const DEFAULT_HIGHLIGHT_DIM: f64 = 0.25;

/// How long [`Command::Highlight`] takes to dim (and, symmetrically, to
/// restore) when the command did not say.
///
/// Short enough to read as "look here" rather than as an animation in its own
/// right; the same order as a `fade_in`, which is the move a lecturer makes
/// with a hand.
pub(crate) const DEFAULT_HIGHLIGHT_DURATION: f64 = 0.4;

/// How long [`Command::Highlight`] holds the dimmed picture when the command
/// did not say.
///
/// Long enough to notice, short enough not to stall a lecture: half a second is
/// about one spoken clause.
pub(crate) const DEFAULT_HIGHLIGHT_HOLD: f64 = 0.5;

/// A mutation requested from outside the engine.
///
/// Internally tagged (`"action": "..."`) so the wire form matches the sketch in
/// `CLAUDE.md` and `需求探讨.txt`:
///
/// ```json
/// { "action": "add", "type": "triangle", "params": { "width": 3, "height": 4 } }
/// ```
///
/// Every variant that names an existing object uses `target` — or `focus`, or
/// `into`, which are the same question asked about a different role — and every
/// one takes a [`Target`], so a model may name a node by the label it created it
/// with instead of the opaque id. All fields use the same names as the JSON, so
/// the serde attributes stay the only translation layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Command {
    /// Open a session by stating the protocol version and asking what the build
    /// can do.
    ///
    /// The first command a client should send, and the one that makes the
    /// engine's vocabulary discoverable instead of guessed: a model that reads
    /// [`Capabilities`] learns the shapes, animations, properties and easing
    /// curves this build actually accepts, rather than one it saw in a prompt.
    /// A version it does not speak is a refusal here, naming both numbers —
    /// silently drawing a v2 command as if it were v1 would teach the client
    /// that a field it needed was optional.
    ///
    /// `protocol` is optional so a minimal script can say `{"action":"hello"}`
    /// without claiming a version. Absent means "whatever you speak"; present
    /// and different is [`CommandError::OutOfRange`] under `protocol`.
    ///
    /// Answered by [`crate::SceneContext::handle`], because capabilities describe
    /// the build and not the tree. A direct [`apply`] caller gets a no-op, the
    /// same round trip every other host-answered command gives it.
    Hello {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol: Option<u32>,
    },
    /// Create a shape and return its new id.
    ///
    /// `children` is meaningful for `"type":"group"` and for nothing else: each
    /// entry is a [`Target`], resolved before anything is inserted, and every
    /// child is moved under the new group. A group is the one node whose
    /// picture *is* its children, so this is how a lecture gets a compound
    /// object it can name, move and fade as one thing while the parts stay
    /// addressable. The refusals are values: children on a non-group is
    /// [`CommandError::OutOfRange`] under `children`, and an unknown child is
    /// refused as the target it was before anything is created, so a failed
    /// `add` leaves the tree exactly as it was.
    ///
    /// `text` and `formula` are the two kinds whose geometry is produced by the
    /// host's typesetter rather than by a shape builder (see
    /// [`crate::SceneContext::handle`]); a build with no typesetter refuses them
    /// with [`CommandError::Unsupported`].
    Add {
        #[serde(rename = "type")]
        kind: ShapeKind,
        #[serde(default)]
        params: ShapeParams,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        children: Option<Vec<Target>>,
    },
    /// Set one named property of one object.
    Set { target: Target, property: Property },
    /// Delete an object and its subtree.
    Remove { target: Target },
    /// Place an object `buff` away from a reference, in a direction.
    ///
    /// The teaching tier's first sentence, and the one a model reaches for
    /// instead of computing coordinates: "put the caption under the triangle"
    /// is true whatever the triangle's size, wherever the camera is, and
    /// whoever wrote the script. Both boxes are measured in **world space**
    /// (every ancestor transform folded in), so a rotated object is placed by
    /// the box a viewer sees; the near edges are separated by exactly `buff`
    /// and the perpendicular centres are aligned — Manim's
    /// `Mobject.next_to(reference, direction, buff)` with its default
    /// `aligned_edge=ORIGIN`.
    ///
    /// `buff` defaults to [`DEFAULT_NEXT_TO_BUFF`] (0.1) and may be negative —
    /// overlapping a label with its arrow is a real design. Both node fields
    /// take a [`Target`], so a label works on either side.
    ///
    /// The move composes a translation onto the target's deferred transform
    /// (O(1), and a rotation or scale already on the object survives), which is
    /// the same contract `set position` has; `modified` lists the target only
    /// when it actually moved. Refusals are values under `OutOfRange`: a node
    /// with no measurable geometry, a collapsed parent transform, a
    /// non-finite `buff`, and a reference that is a **descendant of the
    /// target** — moving the target moves the reference with it, so the gap
    /// the caller asked for cannot be produced. (The reverse nesting is the
    /// legitimate one: a label placed beside the shape it belongs to.)
    NextTo {
        target: Target,
        reference: Target,
        direction: Direction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        buff: Option<f64>,
    },
    /// Arrange several objects in a row or column with equal gaps.
    ///
    /// Gaps are equal, not widths — three objects of different sizes in a row
    /// have the same space between neighbours, which is what reads as an
    /// arrangement — and the arrangement is centred on the group's original
    /// union box, so rearranging in place does not move the group across the
    /// frame. `axis` is `"row"` (left to right, reading order) or `"column"`
    /// (top to bottom, i.e. decreasing world y). `buff` defaults to
    /// [`DEFAULT_ARRANGE_BUFF`] (0.25).
    ///
    /// Every target is resolved and checked before the first move, so a refusal
    /// never leaves the group half-arranged. Duplicate ids are collapsed; two
    /// targets nested inside one another are refused, because placing both
    /// would move the child twice. `modified` is the ids that actually moved.
    Arrange {
        targets: Vec<Target>,
        axis: Axis,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        buff: Option<f64>,
    },
    /// Scale several objects uniformly about their common centre to fit a box.
    ///
    /// One operation for Manim's `scale_to_fit_width` / `scale_to_fit_height`
    /// (`mobject.py:1613,1641`): when both extents are given the **tighter**
    /// axis decides the factor, so the group is fitted rather than stretched —
    /// a circle stays a circle. The group's centre does not move. A group with
    /// no extent on one axis is fitted by the other (a horizontal line is a
    /// legitimate subject); one with no extent at all is left alone. A factor
    /// of exactly `1.0` writes nothing, so a fit that changes nothing reports
    /// nothing.
    ///
    /// `width` and `height` are world units and must be positive and finite.
    /// Same nesting refusal and the same "checked before the first write"
    /// promise as [`Command::Arrange`].
    Fit {
        targets: Vec<Target>,
        width: f64,
        height: f64,
    },
    /// Point an arrow at an object.
    ///
    /// The second teaching-tier sentence, and the other one a model reaches for
    /// instead of computing coordinates: "point at this" is true whatever the
    /// subject's size, wherever the camera is, however the subject is rotated —
    /// the arrow is built from the subject's **world bounding box**, the same
    /// materialised anchors the accessibility tree measures.
    ///
    /// The **tip** sits `buff` away from the box on the named side and the shaft
    /// runs outward from there, so `"down"` means "below the subject, pointing
    /// up at it" — the placement a caption arrow uses, and the default. The
    /// result is a normal [`ShapeKind::Arrow`] node: it is in the tree, it has
    /// an id, it can be faded, moved, recoloured or removed like anything else.
    ///
    /// `buff` defaults to `0.1` (the same `SMALL_BUFF` `next_to` uses) and may
    /// be negative — an arrow whose tip overlaps its subject is a real design.
    /// `length` defaults to `1.0`; `thickness`, `color` and `label` are passed
    /// through to the shape. Refusals are values under `OutOfRange`: a
    /// non-finite `buff`, a non-positive or non-finite `length`, a negative or
    /// non-finite `thickness`, and a subject with no measurable box.
    PointAt {
        target: Target,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction: Option<Direction>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        buff: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        length: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thickness: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<[u8; 3]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Put a label beside an object, with an arrow pointing at it.
    ///
    /// The teaching-tier sentence `point_at` is half of: a callout names its
    /// subject and its words, and the engine computes the rest — the arrow's
    /// tip sits `buff` from the subject's world box, the shaft runs `length`
    /// outward, and the label's near edge sits `buff` past the tail, centred on
    /// the subject's perpendicular axis. The label never covers the arrow, and
    /// every gap is the number asked for, whatever the subject's size or
    /// rotation. The label's own box comes from the host's typesetter, so a
    /// long string pushes the label further out rather than overlapping the
    /// arrow.
    ///
    /// `"down"` (the default) puts the callout below the subject with the arrow
    /// pointing up at it — the caption placement. `arrow` defaults to `true`;
    /// `false` places the label alone, one arrow-and-gap away from the subject.
    /// `buff` and `length` are [`Command::PointAt`]'s numbers with the same
    /// defaults (`0.1`, `1.0`); `thickness`, `color`, `size`, `max_width` and
    /// `label` are passed to the shapes. **`color` reaches both the text and
    /// the arrow**, so one callout reads as one object; `label` names the
    /// **text** only, because one label on two nodes would be ambiguous.
    ///
    /// Answered by [`crate::SceneContext::handle`], not `apply`: typesetting is
    /// the host's, exactly as for `add` of `text`. `created` lists the text
    /// first and the arrow second when it was asked for, so `created[0]` is
    /// always the label. A build with no typesetter refuses with
    /// [`CommandError::Unsupported`] under `text`, before anything is created.
    Annotate {
        target: Target,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction: Option<Direction>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        buff: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        length: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thickness: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arrow: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<[u8; 3]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_width: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Compose a one-line equation: a typeset formula, optionally captioned.
    ///
    /// The first of the six `explain.*` templates — the *thick tools* of the
    /// teaching tier. A primitive `add` of a `formula` asks for a symbol tree; a
    /// template asks for an explanation, and the engine decides how the parts
    /// sit relative to one another. Here the caption goes **under the formula's
    /// own ink**, measured after the formula is compiled, so a tall fraction
    /// pushes its caption down and a long one does not move it sideways.
    ///
    /// `scale` is the post-typeset uniform scale (the same slot an `add` of a
    /// formula carries, and the only way to say "make this bigger" without
    /// re-typesetting); `caption_size` is the caption's em size in world units.
    /// `position` is where the template's **group** lands; the pieces are
    /// placed in the group's own frame, so moving the group moves all of it.
    ///
    /// The reply's `created` is `[group, formula_root, caption?]`: the group is
    /// what a lecture fades, moves or highlights as one thing, and the two
    /// pieces stay addressable under it. Answered by [`crate::SceneContext::handle`]
    /// — typesetting is the host's — and refused with
    /// [`CommandError::Unsupported`] under `formula` (or `text`, for a caption)
    /// before anything is inserted when the build cannot typeset.
    #[serde(rename = "explain.equation")]
    ExplainEquation {
        formula: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<[f64; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scale: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<[u8; 3]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption_size: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Draw a function: labelled axes, a polynomial curve and a caption.
    ///
    /// The second `explain.*` template. `coefficients` is the polynomial
    /// `c0 + c1·x + c2·x² + …`, lowest degree first — the protocol has no
    /// expression language (a decision, not a gap), so a caller hands over
    /// numbers and the template evaluates them itself by Horner's rule. Degrees
    /// above 16 are refused rather than truncated.
    ///
    /// `x_range` (default `[-4, 4]`) and `y_range` (default `[-3, 3]`) are the
    /// data windows; `width`/`height` are the axes' *geometric* size in world
    /// units (defaults 7 and 4, the compiled `axes` scene's proportions) and are
    /// deliberately separate numbers, so a small range can fill a large frame.
    /// `step` (default 1) is the data distance between ticks and their labels;
    /// `samples` (default 160) is the curve's resolution. `color` paints the
    /// curve — the axes and the labels stay foreground.
    ///
    /// The reply's `created` is `[group, axes, plot…, label…, caption?]`. A
    /// curve that leaves `y_range` and comes back yields **several** plot
    /// pieces, one per contiguous run, because a flat point array cannot express
    /// a break; each run is cut at the interpolated boundary crossing so the
    /// curve reaches the edge of its range. A curve that never enters `y_range`
    /// is refused under `y_range` rather than leaving axes with no graph.
    ///
    /// Tick labels are typeset, so the whole template needs a text typesetter;
    /// without one it refuses with [`CommandError::Unsupported`] under `text`
    /// before anything is inserted. Answered by [`crate::SceneContext::handle`].
    #[serde(rename = "explain.plot")]
    ExplainPlot {
        coefficients: Vec<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x_range: Option<[f64; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y_range: Option<[f64; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        samples: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<[u8; 3]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<[f64; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption_size: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Show a figure: a closed shape, its labelled sides and vertices, right-angle
    /// markers and a caption.
    ///
    /// The third `explain.*` template. `vertices` is the figure's outline in
    /// world coordinates, in winding order and **closed by the template** (a
    /// repeated first vertex is accepted and dropped) — a triangle, a
    /// parallelogram or a hexagon are the same command with a different vertex
    /// table. At least three vertices are required; at most 64; every
    /// coordinate must be finite and the vertices must not all lie on one line,
    /// because a figure with no area draws nothing.
    ///
    /// **The vertices are centred on their bounding box**, so `position` names
    /// the figure's centre rather than an arbitrary corner, and the slot means
    /// the same thing it means for every sized shape.
    ///
    /// `side_labels` names the edges in vertex order — label `i` belongs to the
    /// edge from vertex `i` to vertex `i + 1` — and `vertex_labels` names the
    /// vertices in their own order. Each list must be as long as `vertices`, or
    /// absent. Labels are placed **outside** the figure by their own measured
    /// extent plus a gap, so a long word does not overlap the edge it names;
    /// they are typeset in the foreground colour, because `color` names the
    /// figure.
    ///
    /// `right_angle` (default `true`) marks every corner whose interior angle is
    /// 90° with a small outline square — the conventional mark, and the reason a
    /// caller explaining Pythagoras does not have to draw it. `fill_opacity`
    /// (default `0.3`) is the figure's wash; its edge is always stroked, so the
    /// outline reads against the fill. `label_size` sizes the side and vertex
    /// labels.
    ///
    /// The reply's `created` is `[group, figure, marker…, side label…, vertex
    /// label…, caption?]`, so `created[1]` is always the figure. Answered by
    /// [`crate::SceneContext::handle`], because the labels are typeset; a build
    /// with no typesetter still accepts a bare figure and refuses any command
    /// that asks for text with [`CommandError::Unsupported`] under `text`.
    #[serde(rename = "explain.geometry")]
    ExplainGeometry {
        vertices: Vec<[f64; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        side_labels: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vertex_labels: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        right_angle: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill_opacity: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<[u8; 3]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label_size: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<[f64; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption_size: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Show a matrix: a grid of cells with brackets, optional row and column
    /// labels, a left-hand side and a caption.
    ///
    /// The fourth `explain.*` template. `rows` is the cell table, row by row;
    /// every cell is a number or a string ([`Cell`]), every row must have the
    /// same number of cells, and the grid is at least 1×1 and at most 16×16.
    /// A blank cell is refused: an empty entry in a matrix is a mistake, not a
    /// blank space.
    ///
    /// Cells are **plain text**, not formulas: numbers and words are what a
    /// matrix entry normally is, and a symbolic entry (`x^2`) renders literally.
    /// A lesson that needs typeset algebra in a cell uses `add` of `formula`.
    ///
    /// `brackets` (default `"square"`) selects `"square"` `[…]`, `"round"` `(…)`,
    /// `"bars"` `|…|` for a determinant, or `"none"`. `column_labels` and
    /// `row_labels` name the columns and rows; `lhs` is text placed left of the
    /// whole matrix (`"A ="`). The matrix's own cells and brackets take `color`;
    /// the labels, the `lhs` and the caption stay foreground, because `color`
    /// names the matrix.
    ///
    /// **The bracketed matrix is centred on `position`** — the labels hang around
    /// it — so the same slot means the same thing it means in every template.
    /// Column widths and row heights are each the largest cell in that column or
    /// row, so the grid is rectangular without the caller padding strings.
    ///
    /// The reply's `created` is `[group, brackets…, cell…, column label…, row
    /// label…, lhs?, caption?]`, cells row-major. Every cell is addressable and
    /// labelled by its own text, so repeated values are `ambiguous_label` — the
    /// same rule `explain.plot`'s tick labels follow. Answered by
    /// [`crate::SceneContext::handle`] because the text is typeset; without a
    /// text typesetter the command is [`CommandError::Unsupported`] under `text`,
    /// since there is no matrix without text.
    #[serde(rename = "explain.matrix")]
    ExplainMatrix {
        rows: Vec<Vec<Cell>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        column_labels: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        row_labels: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        brackets: Option<MatrixBrackets>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lhs: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cell_size: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<[u8; 3]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<[f64; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption_size: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Show a bar chart: one bar per category, a labelled value axis, category
    /// labels and a caption.
    ///
    /// The fifth `explain.*` template. `categories` and `values` are parallel
    /// lists — at least one entry, at most 32, every category non-blank, every
    /// value finite, and at least one value non-zero (a chart of nothing has no
    /// bar to explain). The bars are **vertical** and share one baseline at
    /// value `0`; a negative value draws its bar downward, so a chart with
    /// positive and negative values needs no second command.
    ///
    /// `value_range` is the data window on the value axis and **must contain
    /// zero** — a bar chart whose baseline is outside its own axis truncates
    /// every bar and lies about the ratios, so that spelling is refused rather
    /// than drawn. Without one the window is the extent of the values plus a
    /// small headroom, zero always included. `step` is the data distance between
    /// value-axis ticks; absent, a "nice" step (`1`, `2`, `5` × a power of ten)
    /// is chosen for about five labels.
    ///
    /// `width`/`height` are the frame's geometric size in world units (defaults
    /// 7 and 4, the proportions `explain.plot` uses) and `show_values` (default
    /// `true`) places each bar's own value above it (below it, for a negative
    /// bar), formatted exactly as a tick label. `color` paints the bars; the two
    /// axes, every label and the caption stay foreground, because `color` names
    /// the data.
    ///
    /// **The frame is centred on `position`** — the category labels hang below
    /// it — so `position` means "the centre of the chart" as it does in every
    /// other template.
    ///
    /// The reply's `created` is `[group, value axis, category axis, bar…, value
    /// label…, value-axis tick label…, category label…, caption?]`: one bar per
    /// category in the order given, then one value label per bar when
    /// `show_values` is on, then the value axis's own numbers, then the category
    /// labels. Each bar is labelled by its category, so a chart can be
    /// addressed bar by category name; the value labels are labelled by their
    /// own text, the same rule `explain.plot`'s tick labels follow, and repeated
    /// categories or repeated values answer `ambiguous_label`. The category
    /// labels carry no label of their own, since the bar beside them already
    /// does.
    ///
    /// Every label is typeset, so the whole template needs a text typesetter;
    /// without one it refuses with [`CommandError::Unsupported`] under `text`
    /// before anything is inserted. Answered by [`crate::SceneContext::handle`].
    #[serde(rename = "explain.chart")]
    ExplainChart {
        categories: Vec<String>,
        values: Vec<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value_range: Option<[f64; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        show_values: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<[u8; 3]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<[f64; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption_size: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Show one frame of an algorithm's state: a row of numbered cells, with
    /// named pointers under chosen ones.
    ///
    /// The sixth and last `explain.*` template, and the picture an algorithm
    /// trace is made of: a one-dimensional array drawn as boxes with a zero-based
    /// index row beneath, optional highlighted cells, and optional **named
    /// pointers** (`i`, `j`, `low`, `high`) standing under the cells they name.
    /// A lecture is a sequence of these, one per step, each with different
    /// pointers — the template owns the layout so the step does not have to.
    ///
    /// `values` is 1..=32 finite numbers; a zero value is a legitimate cell
    /// (unlike a bar, there is no "invisible" cell here). `pointers` is a list of
    /// `[name, index]` pairs — at most 16, names non-blank and distinct, indices
    /// inside the array — drawn as a small caret under the cell with the name
    /// beneath it; two pointers at the same index stack vertically. `highlight`
    /// lists the cells painted in `color` (default: the foreground).
    ///
    /// `cell_size` is each box's side in world units (default 0.8); the value and
    /// index text sizes are derived from it, so one number resizes the whole
    /// tableau. **The row is centred on `position`**, and the index row, the
    /// pointers and the caption hang below it.
    ///
    /// The reply's `created` is `[group, cell…, value…, index…, (pointer caret,
    /// pointer name)…, caption?]`, all rows in index order. Each cell is labelled
    /// `格<i>`, so `set target "格2"` paints the third box, while the value texts
    /// are labelled by their own text (repeated values are `ambiguous_label`, the
    /// rule every template's labels follow) and the index labels carry no label.
    /// Pointer names are their own labels.
    ///
    /// The values and the indices are typeset, so the template needs a text
    /// typesetter; without one it refuses with [`CommandError::Unsupported`]
    /// under `text` before anything is inserted. Answered by
    /// [`crate::SceneContext::handle`].
    #[serde(rename = "explain.algorithm")]
    ExplainAlgorithm {
        values: Vec<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pointers: Option<Vec<(String, u32)>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        highlight: Option<Vec<u32>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cell_size: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<[u8; 3]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<[f64; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption_size: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Dim everything but an object for a moment, then bring it back.
    ///
    /// The attention helper a spoken explanation needs and `point_at` does not
    /// give: pointing says *where*, dimming says *only*. Every picture-bearing
    /// node **outside the subject's subtree** is animated to `dim` times its
    /// current opacity, held, and restored to exactly the opacity it started
    /// with — one beat, so a queued `play` still runs after it as it would after
    /// any other.
    ///
    /// `dim` is a **factor** in `0.0..=1.0` (default `0.25`): `0.0` blanks the
    /// rest, `1.0` is no dimming at all and queues nothing. It multiplies each
    /// node's own opacity rather than assigning an absolute one, so an already
    /// faded object does not brighten on its way to being dimmed.
    /// `duration` (default `0.4`, may be `0.0` for an instant cut) is how long
    /// the dim takes and, symmetrically, how long the restore takes; `hold`
    /// (default `0.5`) is the pause at the dimmed opacity.
    ///
    /// **The restore is part of the beat, not an instruction to the caller.**
    /// A lecture that had to remember to put the scene back would leak a dimmed
    /// picture every time a model forgot; the beat is self-contained, and an
    /// interrupted one leaves the picture wherever the interrupt found it —
    /// `stop` is an interrupt, not a fast-forward.
    ///
    /// The reply is a present-but-empty [`StateDiff`], for `play`'s reason: the
    /// beat changes what will happen to the tree, not the tree. Answered by
    /// [`crate::SceneContext::handle`], because it queues an animation and
    /// `apply` is pure. A scene with nothing outside the subject queues nothing
    /// and succeeds.
    Highlight {
        target: Target,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dim: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hold: Option<f64>,
    },
    /// Play an animation that names its subject by id or label.
    Play { animation: AnimationSpec },
    /// Interrupt the timeline: drop every queued beat and every running animation.
    ///
    /// The counterpart of `play`, and the only command that *preempts* rather
    /// than appends. `play` puts a beat behind whatever is happening; `stop`
    /// ends everything now and leaves the tree exactly as the last frame left
    /// it. **No animation is `finish`ed**, so nothing an animation would do on
    /// completion happens: a `FadeOut` in flight leaves its target in the tree
    /// at the opacity it had reached, and a `MoveTo` leaves its subject where
    /// it was rather than at its destination. That is the point of an
    /// interrupt — "stop that" means the picture freezes, not that the engine
    /// jumps to a state the viewer never saw. The next `play` starts from
    /// there.
    ///
    /// The reply is a present-but-empty [`StateDiff`], for `play`'s reason:
    /// stopping changes what *will* happen, not what is in the tree. It is
    /// **idempotent** — stopping a scene that is already stopped succeeds and
    /// changes nothing, because "make it stop" is satisfied by a scene that has
    /// stopped; refusing would teach a model to check first.
    ///
    /// It does not touch narration already queued, nor the camera when no
    /// `CameraMove` is running: neither is an animation, and a track that is
    /// still talking or a view that has already arrived has nothing to
    /// interrupt. Answered by [`crate::SceneContext::handle`], because only the
    /// playback machine and the scheduler together are "the timeline".
    Stop {
        /// Narrow the interrupt to one object: only animations whose subject is
        /// this node are dropped, and the rest of the timeline keeps playing.
        ///
        /// Absent means the whole timeline, which is what a bare `stop` has always
        /// meant. A node that is not there is `NoSuchNode` — a caller that named a
        /// stale handle should hear about it rather than silently stopping
        /// everything.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<Target>,
    },
    /// Freeze the timeline where it is.
    ///
    /// The lecture clock stops with it, so narration due during the pause is not
    /// spoken while the picture is held — a caption over a frozen scene would be
    /// describing something the viewer cannot see moving. Everything else still
    /// works: an instant camera command, a `set`, an `add`, or a `play` that
    /// queues a beat to start on [`Command::Resume`] are all accepted, which is
    /// what makes "pause, edit, resume" a usable lecture gesture.
    ///
    /// **A paused timeline is still playing** ([`crate::SceneContext::is_playing`]),
    /// so a scene reacting to its timeline draining does not treat a pause as an
    /// ending. Pausing is idempotent; `stop` deliberately does **not** un-pause,
    /// because a viewer's hold must not be undone by a content command — call
    /// [`Command::Resume`] for that.
    ///
    /// Answered by [`crate::SceneContext::handle`] (the flag lives there, not in
    /// the tree) with an empty [`StateDiff`], for `play`'s reason.
    Pause,
    /// Let a paused timeline run again.
    ///
    /// Idempotent, and a no-op on a timeline that never paused. Resuming does
    /// not replay anything the pause skipped — nothing was skipped: the clock
    /// stopped, it did not jump. Answered by [`crate::SceneContext::handle`] with
    /// an empty diff.
    Resume,
    /// Jump the lecture clock **forward**, finishing whatever lies in between.
    ///
    /// `to` is an absolute time in virtual seconds, the same clock
    /// [`crate::SceneContext::clock`] reports — so a script that has just
    /// applied at t = 0 can say "show me where this is at 6.5 s". The jump runs
    /// the playback machine on its own 1/60 s grid, so the tree it leaves is the
    /// tree a run would have had at that time: passed beats are played and
    /// `finish`ed (a `FadeOut` there really removes its target), a beat in
    /// flight is advanced, and the next queued beat is ready to start.
    ///
    /// Three refusals are values: a negative or non-finite `to`, a `to` behind
    /// the clock (forward-only — rewinding needs a snapshot, which is the
    /// checkpoint/undo step and not this command), and a jump longer than
    /// `SceneContext::MAX_SEEK_SECONDS`, because a fast-forward is bounded work
    /// and a model must be told so rather than made to wait.
    ///
    /// Narration the jump crosses is **dropped**, not said late in a burst; a
    /// seek is a jump, and eight sentences landing on one frame is not a
    /// lecture. It works on a paused context too (scrub, then resume) and is
    /// measured in lecture time, so `tempo` does not scale where it lands.
    /// Answered by [`crate::SceneContext::handle`] with an empty diff.
    Seek { to: f64 },
    /// Play the timeline at a different speed from now on.
    ///
    /// `factor` multiplies virtual time per frame: `2.0` plays twice as fast,
    /// `0.5` half. Both the animation clock and the lecture clock scale, so
    /// narration paces along with the picture instead of drifting ahead of it.
    /// The range is `SceneContext::MIN_TEMPO..=SceneContext::MAX_TEMPO`;
    /// anything else — including `0.0`, which is what [`Command::Pause`] is for,
    /// and a negative factor, which would run the timeline backwards — is
    /// [`CommandError::OutOfRange`] under `factor`.
    ///
    /// Takes effect on the next frame and changes nothing already played, so a
    /// tempo command is safe mid-beat. Answered by [`crate::SceneContext::handle`]
    /// with an empty diff, because a speed is not a node.
    Tempo { factor: f64 },
    /// Remember the session as it is now, so [`Command::Undo`] can come back.
    ///
    /// A checkpoint is the whole of what a handover needs: the scene tree (camera
    /// included), the lecture clock, the pause and the tempo. It is a **stack**
    /// of at most [`crate::SceneContext::MAX_CHECKPOINTS`] snapshots, so several
    /// "before this step" marks can be taken and unwound newest first.
    ///
    /// What it deliberately does not carry is the timeline: queued and running
    /// animations are values with captured state, and replaying them into a tree
    /// they were not captured against is not a rewind but a different lecture.
    /// An undo therefore interrupts, exactly as `stop` does, and the restored
    /// snapshot is the state to script from.
    ///
    /// Answered by [`crate::SceneContext::handle`] with an empty diff: a
    /// checkpoint changes nothing a host can observe.
    Checkpoint,
    /// Restore the most recent [`Command::Checkpoint`] and report what moved.
    ///
    /// The reply is the one host-answered command whose diff is **not** empty,
    /// and honestly so: an undo replaces the tree, so `created` lists the nodes
    /// the snapshot brings back, `removed` lists the ones the abandoned present
    /// had and the snapshot does not, and `modified` lists the survivors whose
    /// state differs. An id that disappears here is **retired**: it can never be
    /// issued again in this session, so a host that kept it gets `no_such_node`
    /// rather than a different object wearing the old id.
    ///
    /// Three things it does not unwind, because they are not state a snapshot
    /// can hold: narration already spoken (sound is not a tape), the animations
    /// it interrupts (see [`Command::Checkpoint`]), and the checkpoint stack
    /// itself — an undo consumes the entry it restores. With nothing to restore
    /// it refuses with [`CommandError::NoCheckpoint`], a state error rather than
    /// a malformed payload.
    Undo,
    /// Apply several commands in one round trip, in order.
    ///
    /// The reply is one `diff` — the **net** change of the whole list, computed
    /// by comparing the tree before and after rather than by concatenating the
    /// members' diffs. That distinction is the honest one for a host that
    /// redraws from the reply: a node added and then removed in the same batch
    /// appears in neither list, and one added and then moved appears in
    /// `created` and not in `modified`, because that is what the tree looks
    /// like afterwards. `created` follows the final draw order — the same order
    /// `query` reports — so a model that builds five objects in one turn learns
    /// their five ids from one reply.
    ///
    /// **It is not a transaction.** Members run in order through
    /// [`crate::SceneContext::handle`], and the first refusal stops the list:
    /// the answer is [`CommandError::InBatch`] naming the index, and every
    /// member *before* it has been applied. That half-application is a
    /// documented promise rather than a silent one, and the all-or-nothing
    /// spellings are already commands — `checkpoint` before and `undo` after,
    /// or a [`Command::Validate`] of the same list first.
    ///
    /// Only primitive commands may be members. `hello` (which answers with
    /// capabilities), `query` (a tree), `check` (a report), `plan.status` (a
    /// plan), and `batch` / `validate` themselves (a reply cannot carry a list
    /// of replies) are refused **before anything runs**, so a structurally
    /// invalid batch is atomic: nothing is applied. The table is
    /// [`member_refusal`], shared with a plan step's `ops`. An empty `commands`
    /// is refused under that field, because "do nothing" is spelled by sending
    /// nothing.
    Batch { commands: Vec<Command> },
    /// Dry-run a command list: apply it to a copy, report only what would fail.
    ///
    /// The same `commands` and the same shape rules as [`Command::Batch`], run
    /// through the same [`crate::SceneContext::handle`] — but against a scratch
    /// state that is thrown away, so the live tree, camera, clock, checkpoint
    /// stack, pause, tempo and motion mode are untouched whatever the answer.
    /// Success is the **empty diff**: a validation changes nothing, and the
    /// absence of an error is the answer. Failure is [`CommandError::InBatch`]
    /// with the same index and the same refusal the real batch would produce,
    /// which is the half a model actually needs — the exact member and the
    /// exact field.
    ///
    /// Members see each other, and that is the point: a label created by member
    /// 0 resolves for member 1, and a node member 0 removed is gone for member
    /// 2, so a whole interdependent plan can be checked before any of it is
    /// sent for real. Two pieces of live state are deliberately not copied —
    /// the scheduler and the narration track — because queued animations are
    /// values that captured their start state, not replayable data. A
    /// `validate` of `play` therefore checks the animation's spec and target,
    /// not how the beat will look; a `validate` of `seek` checks the bounds
    /// against the copied clock, and its empty scratch queue is why the dry run
    /// costs no rendering.
    Validate { commands: Vec<Command> },
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
        focus: Option<Target>,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration: Option<f64>,
    },
    /// Say a line of narration now, or after a delay.
    ///
    /// Narration is not a tree change: it reaches the accessibility live region
    /// and the caption track, and the reply is an empty [`StateDiff`], exactly
    /// like a `play`. The engine keeps it on a track of its own, ordered by
    /// **virtual** time, so a wall-clock delay in a host is not what paces a
    /// recorded session — see [`crate::SceneContext::take_narration`].
    ///
    /// `at` is measured from the moment the command arrives, not from the start
    /// of the run. That is what makes a file feed (which applies every line at
    /// t = 0, the basis of replay determinism) able to write a paced lecture:
    /// the lines are all accepted at once, and `at` is what tells them apart.
    /// Absent is the same as `0.0`.
    Narrate {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<f64>,
    },
    /// Change how much motion the run plays from now on.
    ///
    /// The switch takes effect at the **next beat started**, not on whatever is
    /// already on the clock: reduced motion is decided where a beat is handed to
    /// the scheduler ([`rustim_anim::Sequence::start_next`]), and a beat in
    /// flight has already made that decision. Turning motion back on likewise
    /// cannot replay a flourish `construct` skipped — the scene's own scripting
    /// already ran.
    ///
    /// Answered by [`crate::SceneContext::handle`], because the mode lives on the
    /// context rather than in the tree; the returned diff is empty for the same
    /// reason a camera command's is.
    Motion { mode: MotionMode },
    /// Subscribe this session to out-of-band events, or stop the stream.
    ///
    /// A reply answers a command; an event reports what the engine did on its
    /// own — the two facts a client driving a *continuous* engine cannot get
    /// from request/response. After this command, the transport may interleave
    /// event lines with replies:
    ///
    /// ```text
    /// {"event":{"kind":"beat_started","beat":1,"members":2,"duration":1.5,"clock":0.25}}
    /// {"event":{"kind":"animation_done","beat":1,"clock":1.75}}
    /// {"event":{"kind":"frame_ready","frame":108,"clock":1.8}}
    /// ```
    ///
    /// An event line carries **no `seq`** and the single key `event`, so it
    /// answers no command and a client reads a line by asking which of the two
    /// it is. A client that never sends this command never sees one, which is
    /// what keeps a strictly request/response host from skipping lines it did
    /// not order.
    ///
    /// `kinds` selects a subset; absent means all of them. The useful spelling
    /// for a script-follower is `["beat_started","animation_done"]`, because a
    /// `frame_ready` arrives at the frame rate. An empty list is refused under
    /// `kinds`: it selects nothing, and `enabled: false` is the way to say
    /// stop.
    ///
    /// The subscription is **session state**, not tree state: it survives a
    /// `reset` (the next scene still publishes to the same client) and is not
    /// captured by `checkpoint`/`undo` (an undo does not rewind a transport).
    /// The reply is an empty [`StateDiff`] for `play`'s reason: a subscription
    /// changes no node.
    ///
    /// What is emitted is described in [`crate::event`] — in particular, an
    /// interrupt (`stop`, `undo`) is **not** an `animation_done`.
    Events {
        enabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kinds: Option<Vec<EventKind>>,
    },
    /// Read the semantic tree back, changing nothing.
    ///
    /// The read half of the command surface. Until it existed a live session
    /// could only learn what *changed* (through the reply diffs) or what the
    /// whole tree was at the start (`--inspect`, a separate process); a model
    /// that added three objects and wanted to check where they ended up had to
    /// keep its own mirror, which is exactly the bookkeeping the reply protocol
    /// exists to avoid.
    ///
    /// The answer is the same document `--inspect` prints — [`SemanticTree`],
    /// measured bounds and camera included — because a second read format would
    /// be a second thing to keep right. `target` names a node and keeps only its
    /// subtree, which is what makes "what is under that group" expressible
    /// without the host filtering the whole document itself.
    ///
    /// `live` is always absent from a query reply: the announcement text belongs
    /// to the host's accessibility layer (the desktop adapter, the browser
    /// overlay), not to the kernel, so the engine has nothing to say about it
    /// here. A query is answered by [`crate::SceneContext::handle`] rather than
    /// [`apply`], because projecting bounds needs the viewport and a pure
    /// tree-level function has none.
    Query {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<Target>,
    },
    /// Lint the current frame and timeline, changing nothing.
    ///
    /// The third read on the wire, after `query`. A query answers *what is
    /// there*; `check` answers *what is wrong with it* — an object off the
    /// frame, two top-level objects buried in each other, text too small or
    /// too dim to read, an empty text block, a beat that flashes past, two
    /// narration lines closer together than they can be read.
    ///
    /// It is **not** a refusal and not an error: every command a finding is
    /// about was legal. A model that composes a scene from data can be
    /// perfectly well-formed and pedagogically wrong, and the engine has no
    /// other place to say so. The answer is a [`CheckReport`] — a list of
    /// findings, each with a code, a severity, and the node at fault — and the
    /// host decides what to do with it.
    ///
    /// Answered by [`crate::SceneContext::handle`] rather than [`apply`]:
    /// "inside the frame" needs the viewport, and the pacing checks need the
    /// queued beats and pending narration, none of which live in the tree.
    ///
    /// The thresholds (`MIN_TEXT_PX`, `MIN_CONTRAST`, …) are constants in
    /// [`crate::check`], not wire parameters. A finding's `detail` states what
    /// it measured, so a host that disagrees can filter the report; a second
    /// configurable threshold table would be a second answer to one question.
    Check,
    /// Load a declarative lecture plan, replacing any loaded one.
    ///
    /// Loading is a **pre-flight**: the document's shape is checked and every
    /// step is dry-run against a scratch copy of the session, so a bad label, a
    /// malformed animation or an op with no reply is refused before the lecture
    /// starts. See [`crate::plan`] for what is and is not simulated.
    ///
    /// The tree does not change, so the reply is a present-but-empty
    /// [`StateDiff`] — the same answer a `play` gets. Playback is **stopped**,
    /// for the same reason an `undo` interrupts: the loaded plan owns the
    /// timeline from here, and the baseline `plan.goto` returns to is taken
    /// now. The baseline is the plan's own, independent of `checkpoint`/`undo`.
    #[serde(rename = "plan.load")]
    PlanLoad { plan: Plan },
    /// Run the plan's next step: apply its ops, say its line, queue its beats.
    ///
    /// The cursor advances only if everything succeeded; a refusal leaves the
    /// cursor where it was and is located by
    /// [`CommandError::InPlanStep`]. A step's ops are not a transaction, for
    /// `batch`'s reason — ops before the refusal stay applied, and
    /// `plan.goto(cursor)` rebuilds the boundary.
    ///
    /// With no plan loaded the answer is [`CommandError::NoPlan`]; with the
    /// cursor at the end, [`CommandError::PlanFinished`].
    #[serde(rename = "plan.next")]
    PlanNext,
    /// Rebuild the picture at the start of step `step`, without saying anything.
    ///
    /// The tree is the load-time baseline plus steps `0..step` replayed with
    /// their beats settled instantly, and the clock is restored to the
    /// baseline's — so the result is a function of the argument and the plan,
    /// not of what the session was doing. `step == steps` is the lecture's end.
    /// The pause, the tempo and the motion mode are the viewer's policy and are
    /// left alone. See [`crate::plan`] for the full contract.
    #[serde(rename = "plan.goto")]
    PlanGoto { step: usize },
    /// Report where the lecture is: loaded, steps, cursor, the next `say`, and
    /// whether the timeline is busy.
    ///
    /// The sixth reply presence: answered with a [`PlanStatus`] rather than a
    /// diff or a tree, because neither describes a lecture's position. It is a
    /// report and not a refusal when no plan is loaded — "there is no plan" is
    /// the answer to the question.
    #[serde(rename = "plan.status")]
    PlanStatus,
}

/// Every action name the wire accepts, alphabetically.
///
/// This is the `actions` list [`Capabilities`] advertises, and the reason a
/// client can ask "does this build have `batch`?" instead of inferring it from
/// the protocol number or from a document. Kept beside [`Command::action_name`]
/// on purpose: that method is an exhaustive `match`, so a new variant is a
/// compile error there, and
/// `commands_round_trip_through_json` asserts the fixtures reach exactly this
/// set — an action that was added to the enum but not to this list, or the
/// reverse, fails the suite rather than quietly shipping an incomplete
/// handshake.
pub const ACTIONS: &[&str] = &[
    "add",
    "annotate",
    "arrange",
    "batch",
    "camera",
    "check",
    "checkpoint",
    "events",
    "explain.algorithm",
    "explain.chart",
    "explain.equation",
    "explain.geometry",
    "explain.matrix",
    "explain.plot",
    "fit",
    "hello",
    "highlight",
    "motion",
    "narrate",
    "next_to",
    "pause",
    "plan.goto",
    "plan.load",
    "plan.next",
    "plan.status",
    "play",
    "point_at",
    "query",
    "remove",
    "resume",
    "seek",
    "set",
    "stop",
    "tempo",
    "undo",
    "validate",
];

impl Command {
    /// The wire's name for this command's action.
    ///
    /// The tag serde writes for the variant, made available without
    /// serialising an instance — a capability list has no commands to
    /// serialise. The `match` is exhaustive by construction, which is the
    /// guard: a new variant cannot be added without naming it here, and the
    /// round-trip fixtures then check the name against what the parser accepts.
    pub fn action_name(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::Add { .. } => "add",
            Self::Set { .. } => "set",
            Self::Remove { .. } => "remove",
            Self::NextTo { .. } => "next_to",
            Self::Arrange { .. } => "arrange",
            Self::Fit { .. } => "fit",
            Self::PointAt { .. } => "point_at",
            Self::Annotate { .. } => "annotate",
            Self::ExplainAlgorithm { .. } => "explain.algorithm",
            Self::ExplainChart { .. } => "explain.chart",
            Self::ExplainEquation { .. } => "explain.equation",
            Self::ExplainGeometry { .. } => "explain.geometry",
            Self::ExplainMatrix { .. } => "explain.matrix",
            Self::ExplainPlot { .. } => "explain.plot",
            Self::Highlight { .. } => "highlight",
            Self::Play { .. } => "play",
            Self::Stop { .. } => "stop",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Seek { .. } => "seek",
            Self::Tempo { .. } => "tempo",
            Self::Checkpoint => "checkpoint",
            Self::Undo => "undo",
            Self::Batch { .. } => "batch",
            Self::Validate { .. } => "validate",
            Self::Camera { .. } => "camera",
            Self::Narrate { .. } => "narrate",
            Self::Motion { .. } => "motion",
            Self::Events { .. } => "events",
            Self::Query { .. } => "query",
            Self::Check => "check",
            Self::PlanLoad { .. } => "plan.load",
            Self::PlanNext => "plan.next",
            Self::PlanGoto { .. } => "plan.goto",
            Self::PlanStatus => "plan.status",
        }
    }
}

/// How a command names a node: by its opaque id, or by the human-readable label.
///
/// The two spellings share one field because they answer one question — *which
/// node?* — and a separate `target_label` beside `target` would make every
/// command carry two mutually exclusive halves that the protocol has to keep
/// saying are exclusive. The label spelling is the one a model reaches for: it
/// sent `"label": "圆"` when it created the object and has no reason to remember
/// `4294967297`.
///
/// An integer and a string are unambiguous on the wire, and each serialises back
/// as exactly what it was: a label-addressed command replays byte for byte and is
/// never rewritten into an id. That property is why the label stays a *name* — a
/// host that logs a session can still read it a week later.
///
/// Resolution needs the tree, so [`Target::resolve`] takes one, and every entry
/// point that reads a target ([`apply`], [`animation_for`], [`camera_for`],
/// [`crate::SceneContext::handle`]'s query arm) resolves **before** touching
/// anything. A label is not a key: see [`SceneTree::ids_with_label`] for why
/// duplicates are representable, and [`CommandError::AmbiguousLabel`] for what
/// this layer does about them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Id(NodeId),
    Label(String),
}

impl From<NodeId> for Target {
    fn from(id: NodeId) -> Self {
        Self::Id(id)
    }
}

impl From<&str> for Target {
    fn from(label: &str) -> Self {
        Self::Label(label.to_owned())
    }
}

impl From<String> for Target {
    fn from(label: String) -> Self {
        Self::Label(label)
    }
}

impl Target {
    /// The node this names, if exactly one live node has it.
    ///
    /// Three outcomes, each a value:
    ///
    /// * an id that is in the tree, or a label exactly one node carries — that
    ///   node's id;
    /// * an id the tree has never held or has deleted — [`CommandError::NoSuchNode`],
    ///   the refusal every command has always given;
    /// * a label no node carries — [`CommandError::NoSuchLabel`], kept distinct
    ///   from `NoSuchNode` because "there is no node called 圆" and "there is no
    ///   node 4294967297" are different bugs to fix;
    /// * a label several nodes carry — [`CommandError::AmbiguousLabel`], carrying
    ///   their ids in draw order. Picking one would be guessing which of two
    ///   objects the caller meant, and the guess is invisible in the reply.
    pub fn resolve(&self, tree: &SceneTree) -> Result<NodeId, CommandError> {
        match self {
            Self::Id(id) => {
                if tree.get(*id).is_none() {
                    return Err(CommandError::NoSuchNode { target: *id });
                }
                Ok(*id)
            }
            Self::Label(label) => match tree.ids_with_label(label).as_slice() {
                [] => Err(CommandError::NoSuchLabel {
                    label: label.clone(),
                }),
                [id] => Ok(*id),
                candidates => Err(CommandError::AmbiguousLabel {
                    label: label.clone(),
                    candidates: candidates.to_vec(),
                }),
            },
        }
    }
}

/// Hand-written rather than `#[serde(untagged)]`, for the error message: an
/// untagged enum reports "data did not match any variant", which tells a model
/// that sent `"target": true` nothing it can act on. This one says what the
/// field wanted.
impl Serialize for Target {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Id(id) => s.serialize_u64(id.raw()),
            Self::Label(label) => s.serialize_str(label),
        }
    }
}

impl<'de> Deserialize<'de> for Target {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::{Error, Visitor};

        struct TargetVisitor;

        impl Visitor<'_> for TargetVisitor {
            type Value = Target;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a node id (integer) or a label (string)")
            }

            fn visit_u64<E: Error>(self, v: u64) -> Result<Target, E> {
                Ok(Target::Id(NodeId::from_raw(v)))
            }

            fn visit_i64<E: Error>(self, v: i64) -> Result<Target, E> {
                u64::try_from(v)
                    .map(|v| Target::Id(NodeId::from_raw(v)))
                    .map_err(|_| {
                        E::invalid_value(
                            serde::de::Unexpected::Signed(v),
                            &"a non-negative node id",
                        )
                    })
            }

            fn visit_str<E: Error>(self, v: &str) -> Result<Target, E> {
                Ok(Target::Label(v.to_owned()))
            }
        }

        // `deserialize_any` is right here and only here: the wire is JSON, which
        // is self-describing, and the whole point of the type is to accept two
        // JSON shapes in one field.
        d.deserialize_any(TargetVisitor)
    }
}

/// A matrix cell: a number the template renders in its shortest spelling, or a
/// string it renders verbatim.
///
/// Both spellings exist because both are natural. A model writing a numeric
/// matrix sends `[[1, 2], [3, 4]]`; a matrix of symbols or of exact values
/// (`"a"`, `"1/2"`, `"−2"`) sends strings, which is also the only way to choose
/// the glyphs. A number is serialised back as a JSON number, so the protocol's
/// canonical spelling of `1` is `1.0` (serde_json writes an `f64` with its
/// decimal point) — a host that needs `1` exactly sends `"1"`.
///
/// Hand-written for [`Target`]'s reason: an untagged enum's "did not match any
/// variant" would tell a client that put an object in a cell nothing useful.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Number(f64),
    Text(String),
}

impl std::fmt::Display for Cell {
    /// The glyphs the cell is rendered from. Rust's shortest round-trip
    /// spelling of a number, so `1.0` displays as `1` and `0.5` as `0.5`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(value) => write!(f, "{value}"),
            Self::Text(text) => f.write_str(text),
        }
    }
}

impl Serialize for Cell {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Number(value) => s.serialize_f64(*value),
            Self::Text(text) => s.serialize_str(text),
        }
    }
}

impl<'de> Deserialize<'de> for Cell {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::{Error, Visitor};

        struct CellVisitor;

        impl Visitor<'_> for CellVisitor {
            type Value = Cell;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a matrix cell (a number or a string)")
            }

            fn visit_u64<E: Error>(self, v: u64) -> Result<Cell, E> {
                Ok(Cell::Number(v as f64))
            }

            fn visit_i64<E: Error>(self, v: i64) -> Result<Cell, E> {
                Ok(Cell::Number(v as f64))
            }

            fn visit_f64<E: Error>(self, v: f64) -> Result<Cell, E> {
                Ok(Cell::Number(v))
            }

            fn visit_str<E: Error>(self, v: &str) -> Result<Cell, E> {
                Ok(Cell::Text(v.to_owned()))
            }
        }

        d.deserialize_any(CellVisitor)
    }
}

/// The bracket style a matrix is drawn with.
///
/// A serde enum, so an unknown spelling is a parse error rather than an
/// `out_of_range` — the same rule [`crate::Rate`] follows: a name that is not in
/// the vocabulary never becomes a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatrixBrackets {
    /// `[…]` — the default, and the notation for a matrix.
    #[default]
    Square,
    /// `(…)` — a matrix written as a tuple of columns.
    Round,
    /// `|…|` — a determinant.
    Bars,
    /// Nothing around the entries — a table that is not yet a matrix.
    None,
}

/// The wire spelling of [`rustim_anim::Motion`].
///
/// A separate enum rather than serde on `Motion` itself, because `rustim-anim`
/// is a kernel crate with no wire vocabulary of its own — and because the wire
/// names are part of *this* protocol, where renaming a variant of an animation
/// library must not silently change the JSON a host sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionMode {
    Full,
    Reduced,
}

impl From<MotionMode> for rustim_anim::Motion {
    fn from(mode: MotionMode) -> Self {
        match mode {
            MotionMode::Full => Self::Full,
            MotionMode::Reduced => Self::Reduced,
        }
    }
}

impl From<rustim_anim::Motion> for MotionMode {
    /// The reply direction, so that a driver reads back the same name it wrote.
    fn from(motion: rustim_anim::Motion) -> Self {
        match motion {
            rustim_anim::Motion::Full => Self::Full,
            rustim_anim::Motion::Reduced => Self::Reduced,
        }
    }
}

/// The wire spelling of [`rustim_director::Direction`]: which way
/// [`Command::NextTo`] places its target.
///
/// A separate enum for [`MotionMode`]'s reason — the protocol's names are its
/// own, and `rustim-director` is a kernel crate with no wire vocabulary — and
/// because this is where the definition is documented for a client. World
/// space is y-up, so `"up"` increases y; the four names are exactly the
/// directions a viewer would say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl From<Direction> for rustim_director::Direction {
    fn from(direction: Direction) -> Self {
        match direction {
            Direction::Left => Self::Left,
            Direction::Right => Self::Right,
            Direction::Up => Self::Up,
            Direction::Down => Self::Down,
        }
    }
}

/// The wire spelling of [`rustim_director::Axis`]: the line [`Command::Arrange`]
/// places its targets along.
///
/// Reading order, not a direction vector: `"row"` runs left to right and
/// `"column"` runs **top to bottom** (decreasing y), because that is what a
/// writer means by "a column of equations". Manim's `arrange(direction)`
/// spells the same two layouts as `RIGHT` and `DOWN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Row,
    Column,
}

impl From<Axis> for rustim_director::Axis {
    fn from(axis: Axis) -> Self {
        match axis {
            Axis::Row => Self::Row,
            Axis::Column => Self::Column,
        }
    }
}

/// Every property name [`Command::Set`] accepts, in declaration order.
///
/// An array rather than a `Property::ALL` because `Property` carries a value —
/// there is no value of `Property` that means "the name alone". These strings
/// are the names `Property`'s serde tag produces, and
/// [`Capabilities::properties`] is the reader; a unit test parses one command
/// per name so the two cannot drift.
pub const PROPERTY_NAMES: [&str; 13] = [
    "opacity",
    "stroke_width",
    "stroke_opacity",
    "stroke_color",
    "fill_opacity",
    "stroke_join",
    "stroke_cap",
    "stroke_dash",
    "label",
    "kind",
    "position",
    "scale",
    "rotation",
];

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
    /// How two segments of a stroke meet, and how its ends are finished.
    ///
    /// The names are the enums' own serde spelling — `miter` / `round` / `bevel`
    /// and `butt` / `round` / `square` — so nothing downstream needs a table.
    StrokeJoin(rustim_math::stroke::Join),
    StrokeCap(rustim_math::stroke::Cap),
    /// `[on, off]` in world units, or **`null` for no dash at all**.
    ///
    /// This is the one property whose `null` is a value rather than an absent
    /// field: the question is "what dash pattern", and the answer "none" is a
    /// legitimate one. An empty list would have been the other spelling, and it
    /// is worse — it looks like a pattern that dashes nothing.
    StrokeDash(Option<[f64; 2]>),
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
    pub target: Option<Target>,
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
    /// How fast a `rotating` turns, in **turns per second**.
    ///
    /// A rate rather than a period, and its **sign is the direction**: `0.25` is
    /// one turn every four seconds, `-0.25` the same the other way. Required by
    /// `rotating` and refused when absent or degenerate — a rate of zero is an
    /// animation that does nothing, which is never what a caller meant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns_per_second: Option<f64>,
    /// What a `scale` multiplies the size by. Must be positive and finite.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub factor: Option<f64>,
    /// The style property an `animate` moves to, in `set`'s own spelling —
    /// `{"name":"opacity","value":0.35}`.
    ///
    /// Read only by `animate`, and deliberately the *same* struct `set` takes:
    /// a host learns one property vocabulary, and the animation layer refuses the
    /// geometry half (`position`, `scale`, `rotation`) with a message naming the
    /// dedicated animation instead of re-implementing it badly. `label` and
    /// `kind` are refused because they are not continuous.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub property: Option<Property>,
    /// A `rotate`'s pivot, in **world** units. Absent means the object's own
    /// centre, which is what Manim's `Rotate` defaults to as well.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub about_point: Option<[f64; 2]>,
    /// The node a `transform` morphs its target into.
    ///
    /// A [`Target`] rather than an inline `Mobject`, because the donor is already
    /// in the scene and a host should address it rather than restate it — by id
    /// or by the label it was created with. The animation reads it once in
    /// `begin` and never writes to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub into: Option<Target>,
    /// A `transform`'s arc, in radians: the route every point takes instead of a
    /// straight line. Read only by `transform`; the other kinds ignore it, the
    /// same way they ignore `to` or `angle`.
    ///
    /// The sign chooses which way the path bows — positive curves one way,
    /// negative the other — and `0`, or anything below
    /// [`rustim_math::paths::STRAIGHT_PATH_THRESHOLD`], is the straight line.
    /// Absent is omitted rather than written as `null`, and it is the spelling to
    /// prefer for a straight morph: `0` draws the same route but folds the
    /// object's deferred transform into its points, which an absent angle has no
    /// reason to do.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_arc: Option<f64>,
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
    /// The easing curve, by Manim's name for it.
    ///
    /// Named rather than expressed, because a `RateFunc` is a pointer and has
    /// no JSON spelling; the vocabulary is [`Rate`], which is exactly the set of
    /// functions in `manim.utils.rate_functions` at Manim's default parameters.
    /// Absent means the animation's own default, `ease_in_out` — which is the
    /// same curve as the wire's `ease_in_out_quad`, both being Manim's
    /// `ease_in_out_quad`.
    ///
    /// It applies to **every** kind, including `group` and `succession`: Manim's
    /// `AnimationGroup` takes a `rate_func` like any other animation, and it
    /// eases the group's whole timeline. A member inside one may carry its own
    /// `rate`, which then applies to the member — the outer curve shapes when the
    /// member starts and how far into its own motion it is, the inner one how that
    /// motion is shaped.
    ///
    /// Unknown names are a serde error rather than a refusal, and that is the
    /// honest classification: `"rate": "ease_in_wobble"` is not a well-formed
    /// command, because the vocabulary is fixed and the model can read it. There
    /// is no value of `Rate` to refuse — unlike a `factor` of `0`, every variant
    /// names a real curve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<Rate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnimationKind {
    FadeIn,
    FadeOut,
    MoveTo,
    Rotate,
    /// Keep turning until something stops it — the one kind with no end.
    ///
    /// `stop` is what ends it; the beat queue will not advance past one, because
    /// there is nothing to advance past. `turns_per_second` is the parameter and
    /// its sign is the direction.
    Rotating,
    Scale,
    /// Trace the object's path on from its first point.
    Create,
    /// Erase the path from its end and remove the object.
    Uncreate,
    /// Morph the target into the node named by `into` — the whole subtree, not
    /// just the node's own points.
    Transform,
    /// Play the `animations` list in parallel, staggered by `lag_ratio`.
    Group,
    /// Play the `animations` list one after another, as one animation.
    Succession,
    /// Drive one style property to a new value over time.
    Animate,
}

impl AnimationKind {
    /// Every animation kind the wire accepts, in declaration order.
    ///
    /// The single source of truth for [`Capabilities::animations`]. The names
    /// are derived by serialising each variant, so an advertised name is one
    /// [`AnimationSpec`] parses **by construction**; what the array adds is the
    /// list. A variant added to the enum but not to this array would be
    /// invisible to a client, and no compiler can count an array literal —
    /// which is why the conformance suite asserts every name here is accepted
    /// and the count is 12.
    pub const ALL: [AnimationKind; 12] = [
        Self::FadeIn,
        Self::FadeOut,
        Self::MoveTo,
        Self::Rotate,
        Self::Rotating,
        Self::Scale,
        Self::Create,
        Self::Uncreate,
        Self::Transform,
        Self::Group,
        Self::Succession,
        Self::Animate,
    ];
}

/// Engine-enforced upper bounds, and **only** those.
///
/// A limit here is a number the engine really refuses past: `max_ticks` is
/// [`MAX_TICKS`], the per-axis tick cap `number_line` and `axes` clamp to. It is
/// not a performance guideline or a "recommended" value — a list a client
/// cannot trust is worse than no list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub max_ticks: usize,
}

/// What this build of the engine can do, in the protocol's own vocabulary.
///
/// The answer to a [`Command::Hello`], and the reason a client never has to
/// guess what a build supports: `actions`, `shapes`, `animations`, `properties`
/// and `rates` are the sets the parser in this crate actually accepts, derived
/// from the same serde definitions, not a hand-copied excerpt of the
/// documentation. A client is expected to check this rather than assume, which
/// is what makes a build that drops or adds a vocabulary item a compatible
/// change.
///
/// `engine` is an opaque string for humans and logs — a client must not parse
/// it to decide what it can send.
///
/// `actions` is the verb list: the four noun lists say what can be *named*, and
/// this one says what can be *asked*. It is the field that makes
/// [`Command::Batch`] and [`Command::Validate`] discoverable — without it a
/// client would have to infer their presence from the protocol number, which is
/// exactly the guess the handshake exists to remove.
///
/// `profiles` names the tool tiers, and is **advisory**: a client checks
/// `actions` for a specific verb and reads a profile as a floor, never as a
/// promise about a particular name. Two exist:
///
/// * `"primitive"` — the §3 command surface: the vocabulary in this struct.
/// * `"teaching"` — the batch layer, i.e. `batch` and `validate` are accepted,
///   which is the tier a model that wants fewer round trips and cheap trial and
///   error asks for. The tools a lesson is built from are in `actions` for
///   **every** build, because they are tree work that needs no external
///   provider: the layout actions (`next_to`, `arrange`, `fit`), the attention
///   actions (`highlight`, `point_at`, `annotate`), the six `explain.*`
///   templates, the linter (`check`) and the declarative plan
///   (`plan.load`/`next`/`goto`/`status`). A template that needs a typesetter
///   refuses under `text` rather than disappearing from the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// The protocol version this reply describes — the same
    /// [`PROTOCOL_VERSION`] the handshake checks.
    pub protocol: u32,
    /// Free-form build identity, e.g. `rustim/0.0.0`. Never parsed for
    /// capability decisions.
    pub engine: String,
    /// The wire's action names, from [`ACTIONS`].
    pub actions: Vec<String>,
    pub shapes: Vec<String>,
    pub animations: Vec<String>,
    pub properties: Vec<String>,
    pub rates: Vec<String>,
    pub profiles: Vec<String>,
    pub limits: Limits,
}

impl Capabilities {
    /// The vocabulary of the protocol: every name the wire defines.
    ///
    /// This is the *vocabulary*, not a promise that a particular binary has a
    /// provider behind every name — `hello` asks the context
    /// ([`crate::SceneContext::capabilities`]) for the build's own answer.
    /// Kept as the full list because it is derived from the serde definitions,
    /// and the conformance suite reads it as the vocabulary's source of truth.
    pub fn current() -> Self {
        Self::with_typesetting(true, true)
    }

    /// The capabilities of a build that may or may not typeset.
    ///
    /// `text` and `formula` are the only names a build can lack: every other
    /// shape is geometry the kernel builds itself, while these two need a
    /// [`crate::Typesetter`] the host installs. Omitting a name this build
    /// cannot honour is what makes the handshake worth sending — a capability
    /// list that promised a shape the first `add` would refuse is worse than
    /// one that made the client read the refusal it was always going to get.
    pub fn with_typesetting(text: bool, formula: bool) -> Self {
        let shapes: Vec<ShapeKind> = ShapeKind::ALL
            .into_iter()
            .filter(|kind| match kind {
                ShapeKind::Text => text,
                ShapeKind::Formula => formula,
                _ => true,
            })
            .collect();
        Self {
            protocol: PROTOCOL_VERSION,
            engine: format!("rustim/{}", env!("CARGO_PKG_VERSION")),
            actions: ACTIONS.iter().map(|name| (*name).to_owned()).collect(),
            shapes: wire_names(&shapes),
            animations: wire_names(&AnimationKind::ALL),
            properties: PROPERTY_NAMES.iter().map(|n| (*n).to_owned()).collect(),
            rates: wire_names(&Rate::ALL),
            // `teaching` is the batch layer: this build accepts `batch` and
            // `validate`, so it can offer the round-trip-reducing tier. The
            // direct check is `actions`; the profile is the coarse answer.
            profiles: vec!["primitive".to_owned(), "teaching".to_owned()],
            limits: Limits {
                max_ticks: MAX_TICKS,
            },
        }
    }
}

/// The wire spelling of each variant, straight out of its serde definition.
///
/// Serialising rather than restating is what keeps a capability list honest: a
/// name advertised here is one the parser accepts, and a `rename_all` change
/// moves both sides at once. The `expect`s cannot fire for a unit-variant enum —
/// serialising one yields a JSON string — and a silent fallback would be a
/// capability list with a hole in it, which is exactly the failure this
/// function exists to prevent.
fn wire_names<T: Serialize + Copy>(values: &[T]) -> Vec<String> {
    values
        .iter()
        .map(|value| {
            let json = serde_json::to_value(value).expect("a unit variant serialises");
            json.as_str()
                .expect("a unit variant serialises to a string")
                .to_owned()
        })
        .collect()
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
    /// A label no node in the tree carries.
    ///
    /// Separate from [`CommandError::NoSuchNode`] on purpose: a host reading a
    /// session log has to be able to tell "that object was deleted" (the id is
    /// stale) from "no object was ever called that" (the label is wrong), and
    /// they are fixed by different edits — one re-queries, the other renames.
    NoSuchLabel {
        label: String,
    },
    /// A label several nodes carry, with every candidate in draw order.
    ///
    /// Ambiguity is refused rather than resolved by a rule, because no rule this
    /// engine could pick is the one the caller meant: draw order would silently
    /// prefer the first object drawn, creation order the oldest, and the reply
    /// would say the command succeeded. The candidates are in the error so the
    /// caller can retry with an id — which is the only spelling that can be
    /// unambiguous.
    AmbiguousLabel {
        label: String,
        candidates: Vec<NodeId>,
    },
    /// A property value the engine cannot accept, with the range that would be.
    OutOfRange {
        property: String,
        detail: String,
    },
    /// A feature this build does not have, whatever the arguments.
    ///
    /// Separate from [`CommandError::OutOfRange`] because the fix is different:
    /// an out-of-range value is a payload the caller can correct, while an
    /// unsupported feature is a property of the binary — a `text` object on a
    /// host with no typesetter will not work however it is phrased. The name is
    /// the *feature*, so a client learns what to stop asking for; `hello`'s
    /// capabilities already said as much, and this is the answer to a client
    /// that asked anyway.
    Unsupported {
        feature: String,
    },
    /// The host's typesetter failed on source it was given.
    ///
    /// The other half of [`CommandError::OutOfRange`]'s typeset arm: a source
    /// the *caller* can rewrite is refused under `text`, while a backend that
    /// cannot run — no font directory, an engine that produced nothing — is
    /// this variant. Retrying the same sentence will not help, and the detail
    /// says why.
    Typeset {
        detail: String,
    },
    /// There is no checkpoint to restore.
    ///
    /// A *state* error rather than a bad value: the payload of `undo` was never
    /// wrong, the session simply has no earlier state to come back to, and the
    /// fix is to take a `checkpoint` (before the next risky step) rather than to
    /// rephrase anything. Kept apart from [`CommandError::OutOfRange`] for the
    /// same reason [`CommandError::NoSuchLabel`] is kept apart from
    /// [`CommandError::NoSuchNode`]: a host reading a log has to tell a model's
    /// mistake from the session's history.
    NoCheckpoint,
    /// There is no scene plan to play, rewind or report on.
    ///
    /// A *state* error, like [`CommandError::NoCheckpoint`]: the sender asked a
    /// reasonable question about a lecture that was never loaded. It is kept
    /// apart from [`CommandError::OutOfRange`] because the fix is `plan.load`,
    /// not a different value — and `plan.status` is the one plan verb that
    /// answers this situation with a report (`loaded: false`) instead, because
    /// "where are we?" has an honest answer when nothing is loaded.
    NoPlan,
    /// The plan is loaded but its cursor is past the last step.
    ///
    /// The state-based sibling of [`CommandError::NoCheckpoint`]: `plan.next`
    /// at the end of a lecture is a caller that lost count, and the reply says
    /// so rather than re-running the last step or silently succeeding.
    /// `plan.goto` back to any index is the recovery.
    PlanFinished,
    /// A plan step was refused while it ran, with the step, the member and the
    /// cause.
    ///
    /// `step` is **0-based**, matching the index the client wrote in `steps`, and
    /// `member` says which part refused — `ops[1]`, `beats[0]`, or the step
    /// itself (`steps[2]`). The wrapper exists for the same reason
    /// [`CommandError::InBatch`] does: a refusal from deep inside a lecture has
    /// to be locatable in the document the caller can edit.
    InPlanStep {
        step: usize,
        member: String,
        cause: Box<CommandError>,
    },
    /// A command list was refused at one of its members, with that member's
    /// position.
    ///
    /// The wrapper exists so a refusal from inside a [`Command::Batch`] or a
    /// [`Command::Validate`] is *locatable*: the cause says what was wrong, and
    /// this says where. `index` is **0-based**, the position in the JSON array
    /// the client wrote — matching the client's own counting is the only way the
    /// number is actionable to a model that generated the list.
    ///
    /// It is the one wrapped error, and deliberately so: everything else is
    /// reported at the top level because there is only ever one command in
    /// flight. A refusal of the list's *shape* — an empty list, a nested
    /// `batch`, a `query` member — is raised before any member runs and is
    /// **not** wrapped: no member's execution failed, and nothing was applied.
    InBatch {
        index: usize,
        cause: Box<CommandError>,
    },
    /// The request was not a command at all — whoever read the wire could not
    /// turn it into one, and [`parse_command`] says why.
    ///
    /// `apply` never sees text: it receives values. The parser that produces
    /// this variant sits beside it in *this* crate rather than in each host's
    /// transport, because "a blank line is a skip and bad JSON is a refusal" is
    /// protocol, and the desktop feed and the browser build must not answer it
    /// differently. `serde_json` is a pure parser with no I/O, so it costs the
    /// wasm build nothing a `Command` was not already using.
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
            Self::NoSuchLabel { label } => write!(f, "no such label: {label}"),
            Self::AmbiguousLabel { label, candidates } => {
                let ids: Vec<String> = candidates.iter().map(|id| id.raw().to_string()).collect();
                write!(
                    f,
                    "label {label:?} names {} nodes: {}",
                    candidates.len(),
                    ids.join(", ")
                )
            }
            Self::OutOfRange { property, detail } => {
                write!(f, "{property} out of range: {detail}")
            }
            Self::Unsupported { feature } => {
                write!(f, "this build does not support {feature}")
            }
            Self::Typeset { detail } => write!(f, "typesetting failed: {detail}"),
            Self::NoCheckpoint => write!(f, "no checkpoint to restore"),
            Self::NoPlan => write!(f, "no scene plan is loaded"),
            Self::PlanFinished => write!(f, "the plan has no steps left to play"),
            Self::InPlanStep {
                step,
                member,
                cause,
            } => {
                write!(f, "step {step}, {member} was refused: {cause}")
            }
            // Position first, for the same reason a compiler prints the line
            // before the message: the number is what the caller edits.
            Self::InBatch { index, cause } => {
                write!(f, "command {index} in the list was refused: {cause}")
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

/// What handling one command produced.
///
/// The distinction the reply protocol needed once reading joined writing: a
/// command either **changed** the tree or **read** it, and a reply cannot say
/// both. The enum is the in-memory half of [`CommandReply`]'s presence rules —
/// hosts pass it straight to [`reply_line`], so the two cannot disagree about
/// which field a query answers in.
///
/// `Applied` covers everything that mutated (including the host-answered
/// commands whose diff is present and empty: `play`, `camera`, `motion`,
/// `narrate`, and a `plan.next` that queued beats), `Queried` carries the
/// document a `query` asked for, `Checked` carries the findings a `check`
/// reports, `Planned` carries the position a `plan.status` asks for, and
/// `Greeted` carries the vocabulary description a `hello` asks for. None of the
/// five is a tree change, so only `Applied` has a diff at all.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandOutcome {
    /// The command ran. The diff describes the tree change, which may be empty.
    Applied(StateDiff),
    /// The command read the tree and changed nothing, and the session state came
    /// back beside it — a `query` is the one command that answers with both.
    Queried {
        tree: SemanticTree,
        session: SessionState,
    },
    /// The command linted the frame and timeline, changing nothing.
    Checked(CheckReport),
    /// The command reported where a lecture plan is, changing nothing.
    Planned(PlanStatus),
    /// The command asked what the build can do.
    Greeted(Capabilities),
}

impl CommandOutcome {
    /// The tree change, for a command that mutated.
    pub fn diff(&self) -> Option<&StateDiff> {
        match self {
            Self::Applied(diff) => Some(diff),
            Self::Queried { .. } | Self::Checked(_) | Self::Planned(_) | Self::Greeted(_) => None,
        }
    }
}

/// The session's own state, as a `query` reports it.
///
/// **Beside the tree, not in it.** The playback machine and the motion policy are
/// properties of the *session* rather than of any node, and the semantic tree
/// deliberately has no room for them — a screen reader asking what is on screen
/// has no use for the tempo. So they travel as a sibling field of the reply,
/// which is why none of this appears in [`SemanticTree`].
///
/// Every field is one a driver can also **write** — `pause`/`resume`, `tempo`,
/// `motion` and `seek` — and until this existed the protocol was write-only for
/// all four. A driver taking over a session could not ask what state the previous
/// one left it in, which is the one thing that matters when two assistants share
/// an engine.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    /// True between a `pause` and a `resume`. A pause outlives a `stop`, because
    /// a viewer's hold is not the content's to undo.
    pub paused: bool,
    /// The tempo multiplier of both clocks, in `MIN_TEMPO..=MAX_TEMPO`.
    pub tempo: f64,
    /// The policy the **next beat started** will be given. A beat already running
    /// keeps the policy it was handed, so this is a prediction, not a report.
    pub motion: MotionMode,
    /// Whether the timeline is still moving — the beat queue or the scheduler has
    /// work. True through a pause, for the reason `stop` and `pause` document.
    pub playing: bool,
    /// Lecture time in seconds: what `seek` is expressed in, what `tempo` scales,
    /// and what an `.srt` is timed by. Not wall time, so it is the same number in
    /// a window at 60 fps and in a headless still.
    pub clock: f64,
}

/// What a host gets back for one line of a command feed.
///
/// **Exactly one of `diff`, `tree`, `report`, `plan`, `capabilities` and
/// `error` is present, and the presence is the discriminant.** The absent ones
/// are omitted rather than written as `null`, the same rule the commands
/// themselves follow — these are a model's output too.
///
/// A host that sent a `play` gets a `diff` that is present and *empty*: queueing
/// a beat changes nothing in the tree, and saying so is more useful than saying
/// nothing. A host that sent an `add` gets the new id in `created`, which is the
/// whole reason this type exists — it is the only way to learn the id of what
/// was just created, and without it nothing could be done to that object
/// afterwards. A host that sent a `query` gets a `tree`, and no `diff`, because
/// a read changes nothing and an empty diff would read as "a mutation that did
/// nothing". A host that sent a `check` gets a `report`: what the linter found,
/// which is present even when the list is empty — "checked and clean" is an
/// answer, and an absent field would read as "did not ask". A host that sent a
/// `plan.status` gets a `plan`: where the lecture is, including a report that no
/// plan is loaded. A host that sent a `hello` gets `capabilities`: the
/// vocabulary of this build, not the state of this session.
///
/// `session` is **not one of the six.** It rides with `tree` on a `query` — see
/// [`SessionState`] — so the presence discriminant is unchanged, and a client
/// that counts the six by name (all three of the ones in this repository do)
/// needs no change to keep counting them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandReply {
    /// The 1-based *physical* line the command came from, so a human reading a
    /// session log and a host reading the stream count the same way.
    pub seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<StateDiff>,
    /// The document a `query` asked for; see [`SemanticTree`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree: Option<SemanticTree>,
    /// The session state a `query` reports **beside** that document; see
    /// [`SessionState`].
    ///
    /// Present exactly when `tree` is, and absent otherwise. It is *not* a
    /// seventh presence — the six-field discriminant above is unchanged, which is
    /// what lets a client that counts them by name (`rustim-client`, and both
    /// SDK runners) keep working without knowing this field exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionState>,
    /// What a `check` found; see [`CheckReport`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<CheckReport>,
    /// Where a lecture plan is, for a `plan.status`; see [`PlanStatus`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanStatus>,
    /// What the build can do, for a `hello`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Capabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<CommandError>,
}

impl CommandReply {
    /// The reply to a command that was applied, or queued as a beat.
    pub fn applied(seq: u64, diff: StateDiff) -> Self {
        Self {
            seq,
            diff: Some(diff),
            tree: None,
            session: None,
            report: None,
            plan: None,
            capabilities: None,
            error: None,
        }
    }

    /// The reply to a command that read the tree.
    pub fn queried(seq: u64, tree: SemanticTree, session: SessionState) -> Self {
        Self {
            seq,
            diff: None,
            tree: Some(tree),
            // Always with `tree`, never without it: a `query` is the one command
            // that answers with both, and the six-presence rule counts neither.
            session: Some(session),
            report: None,
            plan: None,
            capabilities: None,
            error: None,
        }
    }

    /// The reply to a `check`: what the linter found, and nothing about the
    /// tree — a lint changes nothing.
    pub fn checked(seq: u64, report: CheckReport) -> Self {
        Self {
            seq,
            diff: None,
            tree: None,
            session: None,
            report: Some(report),
            plan: None,
            capabilities: None,
            error: None,
        }
    }

    /// The reply to a `plan.status`: where the lecture is, and nothing about
    /// the tree — a status read changes nothing. A status with no plan loaded
    /// is still this constructor: "there is no plan" is the answer, not a
    /// refusal.
    pub fn planned(seq: u64, plan: PlanStatus) -> Self {
        Self {
            seq,
            diff: None,
            tree: None,
            session: None,
            report: None,
            plan: Some(plan),
            capabilities: None,
            error: None,
        }
    }

    /// The reply to a `hello`: the build's vocabulary, and nothing about the
    /// session.
    pub fn greeted(seq: u64, capabilities: Capabilities) -> Self {
        Self {
            seq,
            diff: None,
            tree: None,
            session: None,
            report: None,
            plan: None,
            capabilities: Some(capabilities),
            error: None,
        }
    }

    /// The reply to a command that was not — refused by the engine, or not
    /// readable as a command at all.
    pub fn refused(seq: u64, error: CommandError) -> Self {
        Self {
            seq,
            diff: None,
            tree: None,
            session: None,
            report: None,
            plan: None,
            capabilities: None,
            error: Some(error),
        }
    }
}

/// One line of a command feed, as the parser sees it.
///
/// `None` means the line carried nothing — blank or whitespace — which is a
/// *skip*, not a refusal: a session log with paragraph breaks in it is still a
/// well-formed session log. Everything else is either a command or the reason
/// there is not one, and the reason is a [`CommandError`] rather than a panic,
/// because the text comes from a language model.
///
/// This lives here rather than with the feed's transport because it is protocol,
/// not I/O: the browser host applies the same lines from `rustim-web`, and a
/// second copy of the "blank is a skip, bad JSON is a refusal" rule would be a
/// second answer to what a line means.
pub fn parse_command(line: &str) -> Option<Result<Command, CommandError>> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    Some(
        serde_json::from_str(line).map_err(|e| CommandError::Unparseable {
            detail: e.to_string(),
        }),
    )
}

/// One reply line, without its newline.
pub fn reply_line(seq: u64, outcome: Result<CommandOutcome, CommandError>) -> String {
    let reply = match outcome {
        Ok(CommandOutcome::Applied(diff)) => CommandReply::applied(seq, diff),
        Ok(CommandOutcome::Queried { tree, session }) => CommandReply::queried(seq, tree, session),
        Ok(CommandOutcome::Checked(report)) => CommandReply::checked(seq, report),
        Ok(CommandOutcome::Planned(plan)) => CommandReply::planned(seq, plan),
        Ok(CommandOutcome::Greeted(capabilities)) => CommandReply::greeted(seq, capabilities),
        Err(error) => CommandReply::refused(seq, error),
    };
    // A reply is built from types that already round-trip, so this cannot fail
    // for a reason the caller could act on; a `Result` here would only push the
    // same `unwrap` up a level.
    serde_json::to_string(&reply).expect("a reply is serialisable by construction")
}

/// Resolve every target in a list, before anything is touched.
///
/// A bulk placement names a group, and the whole group has to be valid before
/// the first member moves: a refusal at the third label must not leave the
/// first two already placed. Resolution is the half that can fail on the
/// *names*; the geometry refusals happen inside `rustim-director`, which makes
/// the same promise for its own checks.
fn resolve_all(tree: &SceneTree, targets: &[Target]) -> Result<Vec<NodeId>, CommandError> {
    targets.iter().map(|target| target.resolve(tree)).collect()
}

/// Map a placement refusal onto the protocol's vocabulary.
///
/// The director's `OutOfRange.field` is carried through as the property name,
/// because it names the wire field at fault (`buff`, `width`, `reference`, …).
/// The node-level failures are reported under the field that named the nodes,
/// with the id in the detail — a caller can act on both without a second read.
/// `Unknown` cannot normally reach here (resolution checked the name first),
/// but it is mapped rather than dropped so this function is total.
pub(crate) fn layout_error(
    property: &'static str,
    error: rustim_director::LayoutError,
) -> CommandError {
    use rustim_director::LayoutError as Layout;
    match error {
        Layout::Unknown(target) => CommandError::NoSuchNode { target },
        Layout::OutOfRange { field, detail } => CommandError::OutOfRange {
            property: field.to_owned(),
            detail,
        },
        Layout::Unmeasurable(id) => CommandError::OutOfRange {
            property: property.to_owned(),
            detail: format!("node {} has no measurable geometry", id.raw()),
        },
        Layout::Nested(id) => CommandError::OutOfRange {
            property: property.to_owned(),
            detail: format!(
                "node {} is inside another target; place one subtree at a time",
                id.raw()
            ),
        },
        Layout::Singular(id) => CommandError::OutOfRange {
            property: property.to_owned(),
            detail: format!(
                "node {} has a collapsed parent transform, so it has no placement",
                id.raw()
            ),
        },
    }
}

/// Why a command cannot be a member of a list, if it cannot.
///
/// The membership table for both list-shaped commands — [`Command::Batch`] and
/// [`Command::Validate`] — and for a plan step's `ops`, so the three cannot
/// disagree about which verbs are *primitives*. A verb that answers with
/// something other than a diff has no place in a list whose reply is one diff:
/// `hello`, `query`, `check` and `plan.status`. The list-shaped commands
/// themselves cannot nest. `None` means "this one is a member".
///
/// The caller names the position (`commands[2]`, `steps[1].ops[0]`); this
/// function only says what is wrong, because the two lists count differently.
pub(crate) fn member_refusal(member: &Command) -> Option<&'static str> {
    match member {
        Command::Hello { .. } => Some("hello answers with capabilities, not a diff"),
        Command::Query { .. } => Some("query answers with a tree, not a diff"),
        Command::Check => Some("check answers with a report, not a diff"),
        Command::PlanStatus => Some("plan.status answers with a plan, not a diff"),
        // The other plan verbs *do* answer with a diff, so the reason they are
        // refused here is different: a plan drives playback and baselines the
        // session, which a member of a list cannot express — the list would run
        // on a lecture whose starting picture it just changed underneath.
        Command::PlanLoad { .. } | Command::PlanNext | Command::PlanGoto { .. } => {
            Some("a plan cannot be driven from inside a command list")
        }
        Command::Batch { .. } | Command::Validate { .. } => Some("a command list cannot be nested"),
        _ => None,
    }
}

/// Apply a command to the tree.
///
/// Pure with respect to everything except `tree`: no clock, no I/O, no renderer.
/// That is what makes the command layer testable in isolation and replayable.
pub fn apply(tree: &mut SceneTree, cmd: &Command) -> Result<StateDiff, CommandError> {
    let mut diff = StateDiff::default();
    match cmd {
        Command::Add {
            kind,
            params,
            children,
        } => {
            // The two kinds the kernel cannot build: their geometry is glyphs
            // and shaping is the host's job. `apply` is pure over the tree and
            // has no typesetter to ask, so it refuses rather than inserting the
            // empty container `shapes::build` would hand it — an invisible
            // object that reports success is the worst of the three answers.
            // A host with a provider intercepts these before reaching here; see
            // [`crate::SceneContext::handle`].
            if kind.needs_typesetter() {
                return Err(CommandError::Unsupported {
                    feature: match kind {
                        ShapeKind::Text => "text",
                        _ => "formula",
                    }
                    .to_owned(),
                });
            }
            // A polygon's geometry *is* its vertices, so there is no size to
            // fall back on: an absent, too-short or non-finite vertex list
            // would insert an invisible object that reports success. Refused
            // here under the field the caller got wrong; `shapes::build`
            // answers the same cases with no points, for a direct kernel
            // caller that never passes through this layer.
            if *kind == ShapeKind::Polygon {
                match params.points.as_deref() {
                    Some(points) if points.len() < 3 => {
                        return Err(CommandError::OutOfRange {
                            property: "points".to_owned(),
                            detail: "a polygon needs at least three vertices".to_owned(),
                        });
                    }
                    Some(points)
                        if points.iter().any(|[x, y]| !x.is_finite() || !y.is_finite()) =>
                    {
                        return Err(CommandError::OutOfRange {
                            property: "points".to_owned(),
                            detail: "every vertex must be finite".to_owned(),
                        });
                    }
                    Some(_) => {}
                    None => {
                        return Err(CommandError::OutOfRange {
                            property: "points".to_owned(),
                            detail: "a polygon needs its vertices".to_owned(),
                        });
                    }
                }
            }
            if children.is_some() && *kind != ShapeKind::Group {
                return Err(CommandError::OutOfRange {
                    property: "children".to_owned(),
                    detail: "only a group takes children".to_owned(),
                });
            }
            // Every child resolves **before** anything is inserted, so a bad
            // target leaves the tree exactly as the command found it rather
            // than leaving a half-filled group behind.
            let children: Vec<NodeId> = match children {
                Some(targets) => targets
                    .iter()
                    .map(|target| target.resolve(tree))
                    .collect::<Result<_, _>>()?,
                None => Vec::new(),
            };
            let id = tree.insert(rustim_mobject::shapes::build(*kind, params));
            diff.created.push(id);
            for child in children {
                // Unconditional, with the result checked separately: this is
                // the `reparent`-inside-`debug_assert!` trap that has cost
                // `Formula::insert` twice. The result cannot be false here —
                // the child exists and a fresh group has no ancestors — so a
                // false is left out of the diff rather than claimed as a move.
                let moved = tree.reparent(child, id);
                if moved {
                    diff.modified.push(child);
                }
            }
        }
        Command::Remove { target } => {
            // `resolve` is also the existence check, for both spellings: an id
            // the tree never held and a label no node carries are refused before
            // anything is touched, and the diff never names a node that was not
            // there.
            let target = target.resolve(tree)?;
            // The walk lives on the tree rather than here, because `FadeOut`'s
            // finish step needs exactly the same one — and two copies of "delete
            // this subtree" would eventually disagree about children.
            tree.remove_subtree(target);
            diff.removed.push(target);
        }
        Command::NextTo {
            target,
            reference,
            direction,
            buff,
        } => {
            // Both nodes resolve before either box is measured: a reference
            // that is missing or ambiguous must not leave the target moved on
            // its way to being refused.
            let target = target.resolve(tree)?;
            let reference = reference.resolve(tree)?;
            let moved = rustim_director::next_to(
                tree,
                target,
                reference,
                (*direction).into(),
                buff.unwrap_or(DEFAULT_NEXT_TO_BUFF),
            )
            .map_err(|error| layout_error("target", error))?;
            diff.modified.extend(moved);
        }
        Command::Arrange {
            targets,
            axis,
            buff,
        } => {
            let targets = resolve_all(tree, targets)?;
            let moved = rustim_director::arrange(
                tree,
                &targets,
                (*axis).into(),
                buff.unwrap_or(DEFAULT_ARRANGE_BUFF),
            )
            .map_err(|error| layout_error("targets", error))?;
            diff.modified.extend(moved);
        }
        Command::Fit {
            targets,
            width,
            height,
        } => {
            let targets = resolve_all(tree, targets)?;
            let moved = rustim_director::fit(tree, &targets, *width, *height)
                .map_err(|error| layout_error("targets", error))?;
            diff.modified.extend(moved);
        }
        Command::PointAt {
            target,
            direction,
            buff,
            length,
            thickness,
            color,
            label,
        } => {
            // Resolution doubles as the existence check, so a stale id is
            // `NoSuchNode` before anything is built.
            let target = target.resolve(tree)?;
            let mut arrow = rustim_director::pointing_arrow(
                tree,
                target,
                direction.unwrap_or(Direction::Down).into(),
                buff.unwrap_or(rustim_director::DEFAULT_POINT_AT_BUFF),
                length.unwrap_or(rustim_director::DEFAULT_POINT_AT_LENGTH),
                *thickness,
            )
            .map_err(|error| layout_error("target", error))?;
            // The shape builder paints with the engine's foreground when no
            // colour was given; a colour named here replaces both the fill and
            // the stroke, matching what `add` does with `params.color`.
            if let Some([r, g, b]) = color {
                let paint = rustim_color::Srgb::from_rgba8(*r, *g, *b, 255);
                arrow.style.fill = paint;
                arrow.style.stroke = paint;
                arrow.dirty.style = true;
            }
            if let Some(label) = label {
                arrow.label = Some(label.clone());
            }
            let id = tree.insert(arrow);
            diff.created.push(id);
        }
        Command::Set { target, property } => {
            let target = target.resolve(tree)?;
            let Some(m) = tree.get_mut(target) else {
                return Err(CommandError::NoSuchNode { target });
            };
            property.write(m)?;
            diff.modified.push(target);
        }
        Command::Hello { .. } => {
            // Answered by `SceneContext::handle`: capabilities describe the
            // build, not the tree, and `apply` has no way to return them — its
            // result is a `StateDiff`. The arm keeps `apply` total over
            // `Command`, so a direct caller replaying a session log does not
            // meet a panic on the handshake its log happens to start with.
        }
        Command::Play { .. } => {
            // `SceneContext::handle` answers this one, because playing is the one
            // command that needs the playback machine and this function is pure.
            // The arm stays so `apply` is still total over `Command` — a caller
            // driving `apply` directly gets the round-trip it always got, not a
            // panic.
        }
        Command::Stop { .. } => {
            // Answered by `SceneContext::handle`: interrupting is a property of
            // the playback machine and the scheduler, neither of which is in the
            // tree this function may touch. The arm keeps `apply` total, so a
            // caller driving it directly replays the same log without a panic.
        }
        Command::Pause
        | Command::Resume
        | Command::Seek { .. }
        | Command::Tempo { .. }
        | Command::Checkpoint
        | Command::Undo => {
            // Playback and session controls. The pause flag, the tempo, the
            // fast-forward and the checkpoint stack all live on the
            // `SceneContext` beside the clock they govern; the tree can express
            // none of them. Keeping the arms is what lets a direct `apply`
            // caller replay a whole session log — including the controls —
            // without a panic.
        }
        Command::Camera { .. } => {
            // Likewise answered by `SceneContext::handle`: resolving `focus` needs
            // the viewport's aspect ratio, and applying the result is either a
            // write to `tree.camera` or a queued beat. The arm keeps `apply` total.
        }
        Command::Motion { .. } => {
            // And likewise: the motion mode lives on the `SceneContext`, beside
            // the playback machine that consults it, not in the tree `apply` can
            // reach. Keeping the arm is what lets a direct `apply` caller replay
            // the same command log without a panic.
        }
        Command::Events { .. } => {
            // And likewise: the subscription is session state on the
            // `SceneContext`, not a node. Keeping the arm is what lets a direct
            // `apply` caller replay a whole log — including the subscription —
            // without a panic.
        }
        Command::Highlight { .. } => {
            // Answered by `SceneContext::handle`: a highlight is a queued beat,
            // and `apply` has neither the scheduler nor the queue. The arm keeps
            // `apply` total, so a caller replaying a session log through a bare
            // tree gets a no-op rather than a panic.
        }
        Command::Annotate { .. } => {
            // Answered by `SceneContext::handle` for the same reason `add` of a
            // `text` is: a callout's label is typeset, and `apply` may not read
            // fonts. The arm keeps `apply` total, so a direct caller replaying a
            // session log meets a no-op rather than a panic.
        }
        Command::ExplainEquation { .. } => {
            // Answered by `SceneContext::handle`: a template compiles a formula
            // and typesets a caption, and `apply` may not read fonts.
        }
        Command::ExplainAlgorithm { .. } => {
            // Answered by `SceneContext::handle`: every cell value, index and
            // pointer name is typeset, so there is no frame `apply` could build
            // without fonts.
        }
        Command::ExplainChart { .. } => {
            // Answered by `SceneContext::handle`: the axis ticks, the category
            // labels and the value labels are all typeset, and `apply` may not
            // read fonts.
        }
        Command::ExplainPlot { .. } => {
            // Answered by `SceneContext::handle` for the same reason: the tick
            // labels are typeset, and `apply` may not read fonts.
        }
        Command::ExplainGeometry { .. } => {
            // Answered by `SceneContext::handle` for the same reason: the side
            // and vertex labels are typeset, and `apply` may not read fonts. A
            // bare figure needs none, but `apply` cannot know which spelling
            // arrived without duplicating the template's own validation.
        }
        Command::ExplainMatrix { .. } => {
            // Answered by `SceneContext::handle`: every cell is typeset, so there
            // is no matrix `apply` could build — the template has nothing to fall
            // back to without fonts.
        }
        Command::Narrate { .. } => {
            // Answered by `SceneContext::handle`, because scheduling narration
            // needs the virtual clock and the narration track — neither of which
            // is in the tree. A direct `apply` caller gets a harmless no-op, the
            // same round-trip every other host-answered command gives it.
        }
        Command::Query { .. } => {
            // Answered by `SceneContext::handle` for a different missing
            // ingredient: projecting a `SemanticTree` needs the viewport, and
            // that lives on the context, not in the tree. Also, a query would
            // not fit this function's return type — it has no diff to give.
        }
        Command::Check => {
            // Answered by `SceneContext::handle` for both of `query`'s reasons
            // and one more: "inside the frame" needs the viewport, and the
            // pacing checks need the queued beats and pending narration, which
            // live on the context. It has no diff to give either.
        }
        Command::PlanLoad { .. }
        | Command::PlanNext
        | Command::PlanGoto { .. }
        | Command::PlanStatus => {
            // The plan lives on the `SceneContext` beside the playback machine
            // it stops and the baseline it restores; `plan.status` answers with
            // a document, which is not a diff either. Keeping the arms is what
            // lets a direct `apply` caller replay a log that navigates a plan
            // without a panic.
        }
        Command::Batch { .. } | Command::Validate { .. } => {
            // Answered by `SceneContext::handle` because a list may contain
            // commands only the context can run (`play`, `camera`, …), so
            // `apply` cannot recurse into one and stay faithful. The arm keeps
            // it total: a direct caller replays the whole log without a panic,
            // and the empty diff is the same no-op the other host-answered
            // commands give it.
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
///   refusal every other command that names a node gives — or a label that
///   resolves to none or several ([`CommandError::NoSuchLabel`],
///   [`CommandError::AmbiguousLabel`]); see [`Target::resolve`];
/// * a `focus` naming a node with neither points nor a declared
///   [`Mobject::local_bounds`] — an empty placeholder, or text whose owner never
///   told the tree how big it is — or with zero extent:
///   [`CommandError::OutOfRange`] rather than a silent no-op. A text object that
///   *does* carry its layout box is framed like any other node;
/// * a `zoom` that is not positive and finite — [`CommandError::OutOfRange`],
///   the same posture as `Scale`: a zero zoom divides by zero, a negative one
///   mirrors the world, and neither is what "zoom in" means.
///
/// `center` is not range-checked, matching `move_to`'s `to`: JSON cannot carry a
/// non-finite float and every finite point is a legitimate centre.
/// The command for "move this node's opacity by `delta`".
///
/// Clamped **here** rather than left to the command layer, which would *refuse*
/// an out-of-range value: an arrow key held at the end of the range should be a
/// no-op, not an error message.
///
/// It lives in the command layer because two hosts press that key: the desktop's
/// window and the browser page, whose keymaps share `rustim_scene::input` and
/// would otherwise share the keypress and disagree about what it does. Every
/// other intent is either a command already or a host affordance with no
/// equivalent on the other side.
pub fn opacity_step(target: NodeId, current: f64, delta: f64) -> Command {
    Command::Set {
        target: target.into(),
        property: Property::Opacity((current + delta).clamp(0.0, 1.0)),
    }
}

pub fn camera_for(
    tree: &SceneTree,
    viewport: &Viewport,
    center: Option<[f64; 2]>,
    zoom: Option<f64>,
    focus: Option<&Target>,
) -> Result<Camera, CommandError> {
    if center.is_none() && zoom.is_none() && focus.is_none() {
        return Err(CommandError::OutOfRange {
            property: "camera".into(),
            detail: "a camera command needs a centre, a zoom or a focus".into(),
        });
    }

    let mut at = tree.camera.center;
    let mut scale = tree.camera.zoom;

    if let Some(focus) = focus {
        let target = focus.resolve(tree)?;
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
/// One field is not in the refusal list because it cannot be wrong once it has
/// parsed: `rate`. An unknown curve name is a serde error — the vocabulary is
/// fixed and [`Rate`] is the whole of it — and every name it accepts is a real
/// curve. The curve is applied by wrapping the constructed animation, which is
/// why every kind can take one, `group` and `succession` included.
///
/// The refusals, each a value rather than a panic:
///
/// * a kind other than `group`/`succession` with **no** `target` —
///   [`CommandError::OutOfRange`]. Every animation but a composition acts on
///   exactly one node, and "which node?" is a question the reply can answer;
/// * a target that is not in the tree — [`CommandError::NoSuchNode`] — or a
///   label resolving to none or to several ([`CommandError::NoSuchLabel`],
///   [`CommandError::AmbiguousLabel`]); the donor `into` is resolved the same
///   way;
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
/// * `Transform` with no `into`, an `into` that is not in the tree
///   ([`CommandError::NoSuchNode`]), the target itself, an ancestor-or-descendant
///   pair, or a shape on one side and a group on the other — the last two would
///   otherwise be silent no-ops: `TransformGroup` has no error channel, and a
///   command that plays an empty beat must not answer as though it moved
///   something. Two *leaves* and two *groups* are both fine; the group case is
///   the one that pads child lists.
/// * a non-finite `path_arc` — [`CommandError::OutOfRange`]. Manim 0.22 performs
///   no range check on the angle (`utils/paths.py:96-140`) and neither does this
///   engine: every finite value is a legitimate arc, and the sign chooses the
///   side. A non-finite one is different in kind — every coordinate of the morph
///   becomes `NaN`, so the object disappears while the beat still claims to
///   play — and it is refused here for the same reason a non-finite `duration`
///   is.
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
    let anim: Box<dyn Animation> = match spec.kind {
        AnimationKind::Group | AnimationKind::Succession => composed_for(tree, spec)?,
        _ => simple_for(tree, spec)?,
    };
    // The override wraps whatever the kind produced, compositions included. It
    // has to sit *outside*: a group eases its own timeline, so a curve placed
    // inside would shape each member's motion rather than when the members play.
    Ok(match spec.rate {
        Some(rate) => Box::new(WithRate::new(anim, rate.func())),
        None => anim,
    })
}

/// The half of [`animation_for`] that builds one concrete animation.
///
/// Split out because a rate override wraps the *result*, and a group's result
/// comes from [`composed_for`] — the early return that used to open
/// [`animation_for`] left no place to say "wrap whatever this produced".
fn simple_for(tree: &SceneTree, spec: &AnimationSpec) -> Result<Box<dyn Animation>, CommandError> {
    let Some(target) = spec.target.as_ref() else {
        return Err(CommandError::OutOfRange {
            property: "target".into(),
            detail: format!(
                "a {:?} names the node it acts on; only group and succession name none",
                spec.kind
            )
            .to_lowercase(),
        });
    };
    // An id or a label, resolved once, here, before the kind reads it. The
    // animation itself only ever sees the id — the wire spelling stops at this
    // function, which is what keeps `rustim-anim` free of tree lookups.
    let target = target.resolve(tree)?;
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
            // A *point*, not a transform: `move_to` says where the object's centre
            // goes and leaves its scale and rotation alone. `MoveTo` resolves the
            // translation in `begin`, so a later member of the same succession sees
            // an earlier member's scale — see the type's doc.
            Box::new(MoveTo::to_point(target, Point::new(x, y)).with_duration(duration))
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
        AnimationKind::Rotating => {
            let Some(rate) = spec.turns_per_second else {
                return Err(CommandError::OutOfRange {
                    property: "turns_per_second".into(),
                    detail: "a rotating needs a rate in turns per second".into(),
                });
            };
            // Finite **and non-zero**: zero is an animation that does nothing,
            // and a rate that is not a number is not a rate. Both are refusals
            // rather than silent no-ops — a model that sent one has a bug worth
            // hearing about, and a beat that reports success while turning
            // nothing is the class of wrong answer this layer exists to prevent.
            if !rate.is_finite() || rate == 0.0 {
                return Err(CommandError::OutOfRange {
                    property: "turns_per_second".into(),
                    detail: format!("{rate} is not a non-zero finite rate"),
                });
            }
            let mut anim = Rotating::new(target, rate);
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
        AnimationKind::Animate => {
            let Some(property) = spec.property.as_ref() else {
                return Err(CommandError::OutOfRange {
                    property: "property".into(),
                    detail: "an animate needs the property to move, e.g. \
                             {\"name\":\"opacity\",\"value\":0.35}"
                        .into(),
                });
            };
            // `animatable` validates the value *and* refuses the channels this
            // animation does not cover, each refusal naming the animation that
            // does — see its doc.
            let to = property.animatable()?;
            Box::new(Animate::new(target, to).with_duration(duration))
        }
        AnimationKind::Create => Box::new(Create::new(target).with_duration(duration)),
        AnimationKind::Uncreate => Box::new(Uncreate::new(target).with_duration(duration)),
        AnimationKind::Transform => {
            let Some(into) = spec.into.as_ref() else {
                return Err(CommandError::OutOfRange {
                    property: "into".into(),
                    detail: "a transform morphs into another node; it needs `into`".into(),
                });
            };
            let into = into.resolve(tree)?;
            if into == target {
                return Err(CommandError::OutOfRange {
                    property: "into".into(),
                    detail: "an object cannot be morphed into itself".into(),
                });
            }
            // The engine refuses both of these by staying still (it has no error
            // channel), so the refusal has to be here — before a beat a host is
            // told played healthy turns out to have done nothing.
            if tree.is_ancestor_of(target, into) || tree.is_ancestor_of(into, target) {
                return Err(CommandError::OutOfRange {
                    property: "into".into(),
                    detail: "an object cannot be morphed into its own ancestor or descendant"
                        .into(),
                });
            }
            let target_is_group = !tree.child_ids(target).is_empty();
            let into_is_group = !tree.child_ids(into).is_empty();
            if target_is_group != into_is_group {
                return Err(CommandError::OutOfRange {
                    property: "into".into(),
                    detail: if target_is_group {
                        "a group can only be morphed into another group, not a shape".into()
                    } else {
                        "a shape can only be morphed into another shape, not a group".into()
                    },
                });
            }
            let mut anim = TransformGroup::new(target, into).with_duration(duration);
            if let Some(arc) = spec.path_arc {
                if !arc.is_finite() {
                    return Err(CommandError::OutOfRange {
                        property: "path_arc".into(),
                        detail: format!("{arc} is not a finite number of radians"),
                    });
                }
                anim = anim.with_path_arc(arc);
            }
            Box::new(anim)
        }
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

/// Reject an opacity-like value outside `0.0..=1.0`, naming the field it came
/// from.
///
/// Shared between `set` ([`Property::write`]) and `animate`
/// ([`Property::animatable`]) so the two spellings of "opacity" cannot admit
/// different ranges — the most likely way a command layer drifts.
fn unit_range(name: &str, v: f64) -> Result<f64, CommandError> {
    if (0.0..=1.0).contains(&v) {
        Ok(v)
    } else {
        Err(CommandError::OutOfRange {
            property: name.to_string(),
            detail: format!("{v} is not in 0.0..=1.0"),
        })
    }
}

/// Reject a stroke width that is negative or not finite.
fn width_range(v: f64) -> Result<f64, CommandError> {
    if v < 0.0 || !v.is_finite() {
        return Err(CommandError::OutOfRange {
            property: "stroke_width".into(),
            detail: format!("{v} is not a non-negative, finite width"),
        });
    }
    Ok(v)
}

impl Property {
    /// The style channel this property names, with its value checked — or the
    /// refusal an `animate` should answer with.
    ///
    /// The three geometry properties are refused rather than approximately
    /// animated, and each refusal names the animation that *does* mean what the
    /// caller asked for: a `position` lerp is [`MoveTo`] minus its `begin`-time
    /// resolution, a `scale` lerp is [`Scale`] without its centre, and a
    /// `rotation` lerp interpolates a matrix entry rather than an angle. `label`
    /// and `kind` are semantic, and there is no frame at which an object is half
    /// a circle.
    pub fn animatable(&self) -> Result<StyleProperty, CommandError> {
        Ok(match self {
            Self::Opacity(v) => StyleProperty::Opacity(unit_range("opacity", *v)?),
            Self::FillOpacity(v) => StyleProperty::FillOpacity(unit_range("fill_opacity", *v)?),
            Self::StrokeOpacity(v) => {
                StyleProperty::StrokeOpacity(unit_range("stroke_opacity", *v)?)
            }
            Self::StrokeWidth(v) => StyleProperty::StrokeWidth(width_range(*v)?),
            // Same construction `set` uses, so an `animate` to a colour leaves
            // the object exactly where a `set` to the same colour would.
            Self::StrokeColor([r, g, b]) => {
                StyleProperty::StrokeColor(rustim_color::Srgb::from_rgba8(*r, *g, *b, 255))
            }
            Self::Position(_)
            | Self::Scale(_)
            | Self::Rotation(_)
            | Self::Label(_)
            | Self::Kind(_)
            // The stroke's *shape* is discrete for `label`'s reason: there is no
            // frame at which a join is half round, and interpolating the pattern
            // of a dashed line would make the dashes crawl.
            | Self::StrokeJoin(_)
            | Self::StrokeCap(_)
            | Self::StrokeDash(_) => {
                let hint = match self {
                    Self::Position(_) => "a `move_to` animation moves an object",
                    Self::Scale(_) => "a `scale` animation resizes an object",
                    Self::Rotation(_) => "a `rotate` animation turns an object",
                    Self::StrokeJoin(_) | Self::StrokeCap(_) | Self::StrokeDash(_) => {
                        "a stroke's join, cap and dash are discrete; set them with `set`"
                    }
                    _ => "label and kind are set instantly with `set`",
                };
                return Err(CommandError::OutOfRange {
                    property: "property".into(),
                    detail: format!("`animate` interpolates style channels only; {hint}"),
                });
            }
        })
    }

    fn write(&self, m: &mut Mobject) -> Result<(), CommandError> {
        match self {
            Self::Opacity(v) => {
                m.opacity = unit_range("opacity", *v)?;
                m.dirty.style = true;
            }
            Self::FillOpacity(v) => {
                m.style.fill_opacity = unit_range("fill_opacity", *v)?;
                m.dirty.style = true;
            }
            Self::StrokeWidth(v) => {
                m.style.stroke_width = width_range(*v)?;
                m.dirty.style = true;
            }
            Self::StrokeJoin(join) => {
                m.style.stroke_join = *join;
                m.dirty.style = true;
            }
            Self::StrokeCap(cap) => {
                m.style.stroke_cap = *cap;
                m.dirty.style = true;
            }
            Self::StrokeDash(pattern) => {
                // Both entries finite and positive, or `None`. A pattern with a
                // zero entry draws a solid line and an infinite one draws
                // nothing, and neither is a dashed line — the refusal says so
                // rather than handing the stroker a pattern with no meaning.
                if let Some([on, off]) = pattern
                    && (!on.is_finite() || !off.is_finite() || *on <= 0.0 || *off <= 0.0)
                {
                    return Err(CommandError::OutOfRange {
                        property: "stroke_dash".into(),
                        detail: format!("[{on}, {off}] is not a pair of positive lengths"),
                    });
                }
                m.style.stroke_dash = *pattern;
                m.dirty.style = true;
            }
            Self::StrokeOpacity(v) => {
                m.style.stroke_opacity = unit_range("stroke_opacity", *v)?;
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
            // Absolute in *position*: "put it at (2, 3)" is a statement about where
            // the object is, while scale and rotation below are operations applied
            // to where it already is. Absolute in position is not the same as
            // replacing the matrix, though: the translation that sends the current
            // centre to `p` is composed onto whatever the object already carries, so
            // a rotated or scaled object keeps that. `set_transform(translate(p))`
            // silently undid an earlier scale — the same defect `Scale`/`Rotation`
            // below were fixed for. Manim's `move_to` is a `shift` for exactly this
            // reason, and a plain `set` and a `play` of `move_to` still agree: both
            // go through the object's centre.
            Self::Position(p) => {
                let here = m.critical_point((0.0, 0.0)).unwrap_or(Point::ZERO);
                m.set_transform(Affine2::translate((p[0] - here.x, p[1] - here.y)) * m.transform);
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
    use rustim_math::Rect;

    /// A session state to hand to `CommandReply::queried` in tests that are not
    /// about the session: the values a freshly built context reports.
    fn session() -> SessionState {
        SessionState {
            paused: false,
            tempo: 1.0,
            motion: MotionMode::Full,
            playing: false,
            clock: 0.0,
        }
    }

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
            // A polygon's vertices are its geometry, so the round trip has to
            // preserve the nested array exactly — and the label says the two
            // spellings of a closed loop (repeated first vertex or not) are
            // both legal input.
            r#"{"action":"add","type":"polygon","params":{"label":"三角形","points":[[0.0,0.0],[2.0,0.0],[1.0,3.0]]}}"#
                .to_owned(),
            // The two kinds only a hosted typesetter can build. The source
            // string is `params.text`, and the layout knobs are optional: a
            // read-back that wrote them as `null` would fail here.
            r#"{"action":"add","type":"text","params":{"label":"字","text":"你好","size":0.5,"max_width":3.0}}"#
                .to_owned(),
            r#"{"action":"add","type":"formula","params":{"label":"公式","text":"x^2 + 1","scale":2.0}}"#
                .to_owned(),
            // A group names *nodes* rather than geometry, and `children` is the
            // one field on `add` that does. Two spellings, because a child may
            // be an id or a label.
            format!(
                r#"{{"action":"add","type":"group","params":{{"label":"g"}},"children":[{id}]}}"#
            ),
            r#"{"action":"add","type":"group","params":{},"children":["leg a"]}"#.to_owned(),
            // Stroke knobs. The colour's `[u8; 3]` is the same spelling
            // `params.color` uses, and the alpha never appears in the colour
            // itself — one alpha per channel, in its own property.
            format!(
                r#"{{"action":"set","target":{id},"property":{{"name":"stroke_opacity","value":0.5}}}}"#
            ),
            format!(
                r#"{{"action":"set","target":{id},"property":{{"name":"stroke_color","value":[100,181,246]}}}}"#
            ),
            // The stroke's shape, including the one property whose `null` is a
            // *value* rather than an absent field: the question is "what dash
            // pattern" and "none" is a legitimate answer.
            format!(
                r#"{{"action":"set","target":{id},"property":{{"name":"stroke_join","value":"bevel"}}}}"#
            ),
            format!(
                r#"{{"action":"set","target":{id},"property":{{"name":"stroke_cap","value":"square"}}}}"#
            ),
            format!(
                r#"{{"action":"set","target":{id},"property":{{"name":"stroke_dash","value":[2.0,1.0]}}}}"#
            ),
            format!(
                r#"{{"action":"set","target":{id},"property":{{"name":"stroke_dash","value":null}}}}"#
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
            // The one kind with no end, and the only one whose canonical spelling
            // carries a *rate* rather than a length. A negative rate is spelled
            // too, because the sign is the direction and a round trip that only
            // saw positives would not pin that.
            format!(
                r#"{{"action":"play","animation":{{"target":{id},"type":"rotating","turns_per_second":0.25}}}}"#
            ),
            format!(
                r#"{{"action":"play","animation":{{"target":{id},"type":"rotating","turns_per_second":-0.25,"about_point":[1.0,-2.0]}}}}"#
            ),
            format!(
                r#"{{"action":"play","animation":{{"target":{id},"type":"scale","factor":2.5}}}}"#
            ),
            // The property animation carries `set`'s own payload inside a
            // `play` — one property vocabulary for both, and the absent-field
            // rule applies to `property` as to every other optional field.
            format!(
                r#"{{"action":"play","animation":{{"target":{id},"type":"animate","duration":0.8,"property":{{"name":"opacity","value":0.35}}}}}}"#
            ),
            format!(
                r#"{{"action":"play","animation":{{"target":{id},"type":"animate","property":{{"name":"stroke_color","value":[100,181,246]}}}}}}"#
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
            // A morph names its donor by id. `into` and `path_arc` sit between
            // `about_point` and `animations` in declaration order, which is the
            // order serde writes — a host diffing a replayed log depends on it.
            format!(
                r#"{{"action":"play","animation":{{"target":{id},"type":"transform","duration":0.8,"into":{id},"path_arc":1.5}}}}"#
            ),
            // The easing curve is last in declaration order — it wraps whatever
            // the kind built, compositions included, so a group's own curve and
            // a member's curve are two different fields in the same document.
            format!(
                r#"{{"action":"play","animation":{{"target":{id},"type":"fade_in","rate":"ease_in_out_sine"}}}}"#
            ),
            format!(
                r#"{{"action":"play","animation":{{"type":"group","animations":[{{"target":{id},"type":"fade_in","rate":"linear"}}],"rate":"there_and_back"}}}}"#
            ),
            // The camera command, in its full and partial spellings. The full
            // one exercises every optional field at once; the partial one is what
            // a host sends to "zoom in", and it has to survive without gaining
            // three `null`s.
            format!(
                r#"{{"action":"camera","center":[1.5,-2.0],"zoom":2.0,"focus":{id},"duration":0.8}}"#
            ),
            r#"{"action":"camera","zoom":2.0}"#.to_owned(),
            r#"{"action":"camera","center":[0.0,0.0]}"#.to_owned(),
            // The runtime motion switch. `mode` is the wire's own word for it,
            // not `rustim_anim::Motion`'s serde spelling — that enum has none,
            // and giving it one would tie the protocol to a library's naming.
            r#"{"action":"motion","mode":"reduced"}"#.to_owned(),
            r#"{"action":"motion","mode":"full"}"#.to_owned(),
            // The event subscription. `enabled` is the switch and `kinds` an
            // optional filter — absent means every kind, so the second spelling
            // must not gain an empty array or a `null`.
            r#"{"action":"events","enabled":true}"#.to_owned(),
            r#"{"action":"events","enabled":true,"kinds":["beat_started","frame_ready"]}"#.to_owned(),
            r#"{"action":"events","enabled":false}"#.to_owned(),
            // Narration: `text` then `at` (declaration order), and an absent
            // delay is omitted rather than written as `null`. The text is UTF-8
            // in the document — serde_json does not escape non-ASCII — so a
            // round trip that mangled it would be visible here.
            r#"{"action":"narrate","text":"先看这个三角形"}"#.to_owned(),
            r#"{"action":"narrate","text":"两条直角边是 3 和 4","at":2.0}"#.to_owned(),
            // The read half: no target is the whole tree, and a target is a
            // subtree. Neither writes a field it did not receive.
            r#"{"action":"query"}"#.to_owned(),
            format!(r#"{{"action":"query","target":{id}}}"#),
            // The linter: a unit variant like `stop`, so it is the bare tag.
            r#"{"action":"check"}"#.to_owned(),
            // The lecture plan. A step's parts are `say`, `ops`, `beats` in
            // declaration order and each is omitted when absent, so the second
            // step below — one line and nothing else — must not come back with
            // two empty arrays. The dotted tag is the wire's spelling, not
            // `snake_case`'s.
            format!(
                r#"{{"action":"plan.load","plan":{{"label":"勾股定理","steps":[{{"say":"先看这个三角形","ops":[{{"action":"set","target":{id},"property":{{"name":"opacity","value":0.5}}}}],"beats":[{{"target":{id},"type":"fade_in","duration":0.6}}]}},{{"say":"结束"}}]}}}}"#
            ),
            r#"{"action":"plan.load","plan":{"steps":[{"say":"你好"}]}}"#.to_owned(),
            // `plan.next` and `plan.status` are unit variants; `plan.goto`
            // carries the 0-based step index and nothing else.
            r#"{"action":"plan.next"}"#.to_owned(),
            r#"{"action":"plan.goto","step":2}"#.to_owned(),
            r#"{"action":"plan.status"}"#.to_owned(),
            // The interrupt: a unit variant, so it is the bare tag and nothing
            // else. An implementation that serialised an empty object or a
            // `null` payload here would change what a relayed log says.
            r#"{"action":"stop"}"#.to_owned(),
            // The scoped spelling: one object's animations rather than the whole
            // timeline. It has to round-trip too, or a host that logged a scoped
            // stop could not replay it.
            format!(r#"{{"action":"stop","target":{id}}}"#),
            r#"{"action":"stop","target":"方块"}"#.to_owned(),
            // The playback controls. `pause` and `resume` are unit variants like
            // `stop`; `seek` and `tempo` each carry one number, and an
            // implementation that wrote the defaults back would fail here.
            r#"{"action":"pause"}"#.to_owned(),
            r#"{"action":"resume"}"#.to_owned(),
            r#"{"action":"seek","to":6.5}"#.to_owned(),
            r#"{"action":"tempo","factor":2.0}"#.to_owned(),
            // The session pair: both unit variants, so both are the bare tag.
            r#"{"action":"checkpoint"}"#.to_owned(),
            r#"{"action":"undo"}"#.to_owned(),
            // A command list is a command: the members round-trip inside it by
            // the same rules, and the list itself gains no field it did not
            // receive. A label member is deliberate — resolution happens per
            // member, against the tree as the earlier members left it.
            r#"{"action":"batch","commands":[{"action":"add","type":"circle","params":{"radius":2.0,"label":"圆"}},{"action":"set","target":"圆","property":{"name":"opacity","value":0.5}}]}"#
                .to_owned(),
            format!(
                r#"{{"action":"validate","commands":[{{"action":"remove","target":{id}}},{{"action":"play","animation":{{"target":{id},"type":"fade_in"}}}}]}}"#
            ),
            // The handshake, in both spellings. An absent `protocol` is a client
            // that does not claim a version, and it must not come back as a
            // `null`; a *mismatched* version still parses — the refusal belongs
            // to `handle`, where both numbers are known, not to the parser.
            r#"{"action":"hello"}"#.to_owned(),
            r#"{"action":"hello","protocol":1}"#.to_owned(),
            r#"{"action":"hello","protocol":2}"#.to_owned(),
            // The label spelling of a target. The field is the same one; only
            // the JSON type differs, and it survives the round trip as a label —
            // a logged session stays readable, and replaying it resolves the
            // same names against the same tree.
            r#"{"action":"remove","target":"方块"}"#.to_owned(),
            r#"{"action":"set","target":"方块","property":{"name":"label","value":"圆"}}"#.to_owned(),
            r#"{"action":"play","animation":{"target":"圆","type":"fade_in"}}"#.to_owned(),
            r#"{"action":"play","animation":{"target":"圆","type":"transform","into":"方块"}}"#
                .to_owned(),
            r#"{"action":"camera","focus":"方块"}"#.to_owned(),
            r#"{"action":"query","target":"方块"}"#.to_owned(),
            // The teaching tier's three layout actions. `buff` is optional and
            // omitted when absent, so neither spelling may gain it; `direction`
            // and `axis` are the wire's own vocabulary, and each action takes
            // nodes in the same two spellings as every other target field.
            format!(
                r#"{{"action":"next_to","target":{id},"reference":"leg a","direction":"right","buff":0.2}}"#
            ),
            r#"{"action":"next_to","target":"标签","reference":"leg a","direction":"down"}"#
                .to_owned(),
            format!(r#"{{"action":"arrange","targets":[{id},"leg a"],"axis":"row"}}"#),
            r#"{"action":"arrange","targets":["a","b"],"axis":"column","buff":0.5}"#.to_owned(),
            format!(r#"{{"action":"fit","targets":[{id}],"width":4.0,"height":2.0}}"#),
            // The teaching tier's three attention actions. `point_at` creates a
            // picture and carries the same optional knobs `add` does, each
            // omitted when absent; `highlight` queues a beat and writes no
            // fields back; `annotate` creates two pictures from one measured
            // placement. All three take a label where every other target field
            // does.
            format!(
                r#"{{"action":"point_at","target":{id},"direction":"down","buff":0.2,"length":1.5,"thickness":0.05,"color":[100,181,246],"label":"看这里"}}"#
            ),
            r#"{"action":"point_at","target":"三角形"}"#.to_owned(),
            format!(
                r#"{{"action":"highlight","target":{id},"dim":0.2,"duration":0.4,"hold":0.6}}"#
            ),
            r#"{"action":"highlight","target":"三角形"}"#.to_owned(),
            // The callout: a label (required — a callout with nothing to say is
            // a typo, not a command) plus the same optional knobs `point_at`
            // carries, in declaration order. `arrow` is a boolean this action
            // has and `point_at` does not.
            format!(
                r#"{{"action":"annotate","target":{id},"text":"两条直角边是 3 和 4","direction":"up","buff":0.2,"length":1.5,"thickness":0.05,"arrow":false,"color":[100,181,246],"size":0.6,"max_width":4.0,"label":"注释"}}"#
            ),
            r#"{"action":"annotate","target":"三角形","text":"看这里"}"#.to_owned(),
            // The first template: a formula with every slot filled, then the
            // minimal spelling. The tag carries the template name, so a client
            // sees six actions in `capabilities.actions` rather than one action
            // with a sixth field to branch on.
            r#"{"action":"explain.equation","formula":"a^2 + b^2 = c^2","position":[0.0,0.5],"scale":1.2,"color":[100,181,246],"caption":"勾股定理","caption_size":0.5,"label":"定理"}"#.to_owned(),
            r#"{"action":"explain.equation","formula":"x^2"}"#.to_owned(),
            // The second template: a full plot, then the minimal spelling —
            // one coefficient per degree, lowest first. Declaration order is
            // the serialised order, so the fields here follow the enum.
            r#"{"action":"explain.plot","coefficients":[0.0,-1.75,0.25],"x_range":[-4.0,4.0],"y_range":[-3.0,3.0],"width":7.0,"height":4.0,"samples":160,"step":1.0,"color":[255,213,79],"position":[0.0,0.5],"caption":"y = 0.25x² - 1.75","caption_size":0.5,"label":"抛物线"}"#.to_owned(),
            r#"{"action":"explain.plot","coefficients":[1.0,0.0,-1.0]}"#.to_owned(),
            r#"{"action":"explain.geometry","vertices":[[0.0,0.0],[3.0,0.0],[0.0,4.0]],"side_labels":["a","b","c"],"vertex_labels":["A","B","C"],"right_angle":true,"fill_opacity":0.3,"color":[100,181,246],"label_size":0.4,"position":[0.0,0.5],"caption":"直角三角形","caption_size":0.5,"label":"图形"}"#.to_owned(),
            r#"{"action":"explain.geometry","vertices":[[0.0,0.0],[2.0,0.0],[1.0,2.0]]}"#.to_owned(),
            // The third template's cells are numbers *or* strings, and a number
            // is written back as a JSON `f64` — the canonical spelling of `1`
            // is `1.0`, which is why the fixture says so.
            r#"{"action":"explain.matrix","rows":[[1.0,2.0],[3.0,4.0]],"column_labels":["列一","列二"],"row_labels":["行一","行二"],"brackets":"bars","lhs":"A =","cell_size":0.4,"color":[100,181,246],"position":[0.0,0.5],"caption":"行列式","caption_size":0.5,"label":"矩阵"}"#.to_owned(),
            r#"{"action":"explain.matrix","rows":[["a","b"],["c","d"]]}"#.to_owned(),
            // The fourth template: a full chart, then the minimal spelling.
            // `categories` and `values` are parallel arrays, and `show_values`
            // is a plain switch — an absent one must not come back as `false`.
            r#"{"action":"explain.chart","categories":["一","二","三"],"values":[3.0,5.0,2.0],"value_range":[0.0,6.0],"step":2.0,"width":7.0,"height":4.0,"show_values":true,"color":[100,181,246],"position":[0.0,0.5],"caption":"各季度销量","caption_size":0.5,"label":"柱状图"}"#.to_owned(),
            r#"{"action":"explain.chart","categories":["甲","乙"],"values":[1.0,-2.0]}"#.to_owned(),
            // The fifth template: a full algorithm frame, then the minimal
            // spelling. `pointers` is a list of `[name, index]` pairs — a tuple
            // on the wire, an array in JSON — and the indices are `u32`, so the
            // fixture writes `1` rather than `1.0`.
            r#"{"action":"explain.algorithm","values":[5.0,3.0,8.0,1.0],"pointers":[["i",1],["j",2]],"highlight":[1,2],"cell_size":0.8,"color":[100,181,246],"position":[0.0,0.5],"caption":"冒泡排序","caption_size":0.5,"label":"数组"}"#.to_owned(),
            r#"{"action":"explain.algorithm","values":[1.0]}"#.to_owned(),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for json in cases {
            let cmd: Command =
                serde_json::from_str(&json).unwrap_or_else(|e| panic!("parsing {json}: {e}"));
            assert_eq!(serde_json::to_string(&cmd).unwrap(), json, "{json}");
            seen.insert(cmd.action_name());
        }
        // The fixtures above are the vocabulary's proof of life. Every action
        // `Capabilities` advertises must have a fixture here, and every action
        // a fixture reaches must be advertised — an action added to the enum
        // (which `action_name`'s exhaustive match forces into existence) but
        // not to `ACTIONS` would otherwise ship a handshake with a hole in it.
        assert_eq!(
            seen.into_iter().collect::<Vec<_>>(),
            ACTIONS.to_vec(),
            "the round-trip fixtures and the advertised actions have to be the same set"
        );
    }

    /// The commands `SceneContext::handle` answers are **no-ops** in `apply`, not
    /// panics — `apply` stays total over the wire so a direct caller can replay a
    /// whole session log without a host around it. Each returns the empty diff its
    /// arm promises and leaves the tree exactly as it found it.
    #[test]
    fn host_answered_commands_are_no_ops_on_the_bare_tree() {
        let mut tree = SceneTree::new();
        let id = tree.insert(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        let before = tree.len();

        for cmd in [
            Command::Hello {
                protocol: Some(PROTOCOL_VERSION),
            },
            Command::Stop { target: None },
            Command::Pause,
            Command::Resume,
            Command::Seek { to: 2.0 },
            Command::Tempo { factor: 2.0 },
            Command::Checkpoint,
            Command::Undo,
            Command::Batch {
                commands: vec![Command::Pause],
            },
            Command::Validate {
                commands: vec![Command::Pause],
            },
            Command::Motion {
                mode: MotionMode::Reduced,
            },
            Command::Events {
                enabled: true,
                kinds: None,
            },
            Command::Narrate {
                text: "先看这个三角形".to_owned(),
                at: Some(1.0),
            },
            Command::Query { target: None },
            Command::Check,
            Command::Highlight {
                target: id.into(),
                dim: None,
                duration: None,
                hold: None,
            },
            Command::Annotate {
                target: id.into(),
                text: "看这里".to_owned(),
                direction: None,
                buff: None,
                length: None,
                thickness: None,
                arrow: None,
                color: None,
                size: None,
                max_width: None,
                label: None,
            },
            Command::ExplainEquation {
                formula: "x^2".to_owned(),
                position: None,
                scale: None,
                color: None,
                caption: None,
                caption_size: None,
                label: None,
            },
            Command::ExplainAlgorithm {
                values: vec![5.0, 3.0, 8.0],
                pointers: Some(vec![("i".to_owned(), 1)]),
                highlight: Some(vec![1]),
                cell_size: None,
                color: None,
                position: None,
                caption: None,
                caption_size: None,
                label: None,
            },
            Command::ExplainChart {
                categories: vec!["一".to_owned(), "二".to_owned()],
                values: vec![1.0, -2.0],
                value_range: None,
                step: None,
                width: None,
                height: None,
                show_values: None,
                color: None,
                position: None,
                caption: None,
                caption_size: None,
                label: None,
            },
            Command::ExplainPlot {
                coefficients: vec![1.0, 0.0, -1.0],
                x_range: None,
                y_range: None,
                width: None,
                height: None,
                samples: None,
                step: None,
                color: None,
                position: None,
                caption: None,
                caption_size: None,
                label: None,
            },
            Command::ExplainGeometry {
                vertices: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                side_labels: None,
                vertex_labels: None,
                right_angle: None,
                fill_opacity: None,
                color: None,
                label_size: None,
                position: None,
                caption: None,
                caption_size: None,
                label: None,
            },
            Command::ExplainMatrix {
                rows: vec![vec![Cell::Number(1.0), Cell::Number(2.0)]],
                column_labels: None,
                row_labels: None,
                brackets: None,
                lhs: None,
                cell_size: None,
                color: None,
                position: None,
                caption: None,
                caption_size: None,
                label: None,
            },
        ] {
            let diff = apply(&mut tree, &cmd).expect("answered elsewhere, not refused");
            assert_eq!(diff, StateDiff::default(), "{cmd:?}");
            assert_eq!(tree.len(), before, "{cmd:?}");
            assert!(tree.get(id).is_some(), "{cmd:?}");
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
            // The lint's two shapes: a clean answer is an empty list in a
            // *present* field, and a node finding writes `code`, `severity`,
            // `target`, `label`, `other`, `detail` in that order. A finding
            // about the timeline has no node and omits all three node fields.
            r#"{"seq":6,"report":{"findings":[]}}"#.to_owned(),
            r#"{"seq":7,"report":{"findings":[{"code":"out_of_frame","severity":"error","target":4294967296,"label":"方块","detail":"moved off the frame"}]}}"#
                .to_owned(),
            r#"{"seq":8,"report":{"findings":[{"code":"beat_too_fast","severity":"info","detail":"beat 1 lasts 0.05 s"}]}}"#
                .to_owned(),
        ];
        for json in cases {
            let reply: CommandReply =
                serde_json::from_str(&json).unwrap_or_else(|e| panic!("parsing {json}: {e}"));
            assert_eq!(serde_json::to_string(&reply).unwrap(), json, "{json}");
        }
    }

    /// The three constructors are the only way to build one, so none can
    /// accidentally produce a reply carrying more than one half or none.
    #[test]
    fn a_reply_carries_exactly_one_half() {
        let ok = CommandReply::applied(7, StateDiff::default());
        assert!(
            ok.diff.is_some() && ok.tree.is_none() && ok.report.is_none() && ok.error.is_none()
        );
        assert!(!serde_json::to_string(&ok).unwrap().contains("error"));

        let read = CommandReply::queried(
            8,
            crate::a11y::build(&SceneTree::new(), &Viewport::default(), None),
            session(),
        );
        assert!(
            read.diff.is_none()
                && read.tree.is_some()
                && read.report.is_none()
                && read.error.is_none()
        );
        assert!(!serde_json::to_string(&read).unwrap().contains("error"));

        // `session` rides with `tree` and is not a seventh half: present exactly
        // when the tree is, so a client counting the six by name — which is what
        // `rustim-client` and both SDK runners do — keeps counting six.
        let read_json = serde_json::to_string(&read).unwrap();
        assert!(read_json.contains("\"session\""), "{read_json}");
        assert!(
            !serde_json::to_string(&ok).unwrap().contains("session"),
            "a mutation reports no session"
        );

        let lint = CommandReply::checked(11, CheckReport::default());
        assert!(
            lint.diff.is_none()
                && lint.tree.is_none()
                && lint.report.is_some()
                && lint.error.is_none()
        );
        assert!(!serde_json::to_string(&lint).unwrap().contains("null"));
        assert!(
            !serde_json::to_string(&lint).unwrap().contains("session"),
            "a lint reports no session"
        );

        let no = CommandReply::refused(
            9,
            CommandError::Unparseable {
                detail: "nope".into(),
            },
        );
        assert!(
            no.diff.is_none() && no.tree.is_none() && no.report.is_none() && no.error.is_some()
        );
        assert!(!serde_json::to_string(&no).unwrap().contains("null"));

        let hello = CommandReply::greeted(10, Capabilities::current());
        assert!(
            hello.diff.is_none()
                && hello.tree.is_none()
                && hello.report.is_none()
                && hello.error.is_none()
                && hello.capabilities.is_some()
        );
        assert!(!serde_json::to_string(&hello).unwrap().contains("null"));
    }

    /// The capability list is the vocabulary the *parser* accepts, not a copy of
    /// the documentation, and this is the test that keeps that true: every
    /// advertised name is fed back through the wire.
    ///
    /// The three shapes of drift it catches are a new enum variant missing from
    /// its `ALL` array (the count assertions), a name that no longer parses
    /// because a serde rename moved only one side (the parse loop), and a
    /// property list that was hand-written and fell behind the enum — which is
    /// why [`PROPERTY_NAMES`] has a value per name here rather than a single
    /// generic spelling.
    #[test]
    fn capabilities_name_only_what_the_wire_accepts() {
        let caps = Capabilities::current();
        assert_eq!(caps.protocol, PROTOCOL_VERSION);
        assert!(caps.engine.starts_with("rustim/"));
        assert_eq!(
            caps.profiles,
            ["primitive".to_owned(), "teaching".to_owned()],
            "the teaching profile is advertised because `batch`/`validate` are"
        );
        assert_eq!(caps.limits.max_ticks, MAX_TICKS);

        // Every verb is advertised, and every advertised verb round-trips:
        // the fixtures in the round-trip test are the parse half of this
        // claim, and that test asserts its action set *is* `ACTIONS`.
        assert_eq!(caps.actions, ACTIONS);

        assert_eq!(caps.shapes.len(), ShapeKind::ALL.len());
        for name in &caps.shapes {
            let json = format!(r#"{{"action":"add","type":"{name}"}}"#);
            serde_json::from_str::<Command>(&json).unwrap_or_else(|e| panic!("{json}: {e}"));
        }

        assert_eq!(caps.animations.len(), AnimationKind::ALL.len());
        for name in &caps.animations {
            let json = format!(r#"{{"action":"play","animation":{{"target":1,"type":"{name}"}}}}"#);
            serde_json::from_str::<Command>(&json).unwrap_or_else(|e| panic!("{json}: {e}"));
        }

        assert_eq!(caps.properties, PROPERTY_NAMES);
        for name in &caps.properties {
            // One real value per name: the tag's spelling is what is under test,
            // and a value of the wrong JSON type would fail for a reason that has
            // nothing to do with it.
            let value = match name.as_str() {
                "opacity" | "stroke_width" | "stroke_opacity" | "fill_opacity" | "scale"
                | "rotation" => "1.0",
                "stroke_color" => "[1,2,3]",
                // The stroke's shape, in the enums' own wire spelling.
                "stroke_join" => "\"round\"",
                "stroke_cap" => "\"butt\"",
                "stroke_dash" => "[2.0,1.0]",
                "label" => "\"方块\"",
                "kind" => "\"square\"",
                "position" => "[1.0,2.0]",
                other => panic!("capability names a property this test cannot spell: {other}"),
            };
            let json = format!(
                r#"{{"action":"set","target":1,"property":{{"name":"{name}","value":{value}}}}}"#
            );
            serde_json::from_str::<Command>(&json).unwrap_or_else(|e| panic!("{json}: {e}"));
        }

        assert_eq!(caps.rates.len(), Rate::ALL.len());
        for name in &caps.rates {
            let json = format!(
                r#"{{"action":"play","animation":{{"target":1,"type":"fade_in","rate":"{name}"}}}}"#
            );
            serde_json::from_str::<Command>(&json).unwrap_or_else(|e| panic!("{json}: {e}"));
        }
    }

    /// A greeting reply round-trips like every other document, and carries no
    /// `diff` — a handshake that reported "nothing changed" would be a claim
    /// about the session, not about the build.
    #[test]
    fn a_greeting_reply_round_trips_through_json() {
        let reply = CommandReply::greeted(1, Capabilities::current());
        let json = serde_json::to_string(&reply).unwrap();
        let parsed: CommandReply = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, reply);
        assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
        assert!(parsed.diff.is_none() && parsed.tree.is_none());
        assert_eq!(parsed.capabilities.unwrap().protocol, PROTOCOL_VERSION);
    }

    /// A query reply round-trips like every other document on this wire, so a
    /// host can log a session that read the tree and replay the log byte for
    /// byte. The document is built rather than pasted: a `SemanticTree` literal
    /// here would pin the projection's field order, which is [`crate::a11y`]'s
    /// business, not this test's.
    #[test]
    fn a_query_reply_round_trips_through_json() {
        let mut tree = SceneTree::new();
        tree.insert(Mobject::new(vec![]));
        let reply = CommandReply::queried(
            6,
            crate::a11y::build(&tree, &Viewport::default(), None),
            session(),
        );
        let json = serde_json::to_string(&reply).unwrap();
        let parsed: CommandReply = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, reply);
        assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
        assert!(parsed.diff.is_none());
        assert_eq!(parsed.tree.unwrap().nodes.len(), 2);
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

    /// Both spellings of a target answer with the same id, which is the whole
    /// contract: a label is an alternative *name* for a node, not a second way
    /// of identifying something else.
    #[test]
    fn a_target_resolves_by_id_or_by_label() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(vec![]).with_label("方块"));
        assert_eq!(Target::Id(id).resolve(&tree), Ok(id));
        assert_eq!(Target::from("方块").resolve(&tree), Ok(id));
        // The label of a *different* node is not this node.
        tree.insert(Mobject::new(vec![]).with_label("圆"));
        assert_eq!(Target::from("方块").resolve(&tree), Ok(id));
    }

    /// A label nothing carries is its own refusal, not `NoSuchNode`: the two
    /// are fixed by different edits, and a host reading the log has to be able
    /// to tell "deleted" from "never called that".
    #[test]
    fn an_unknown_label_is_its_own_refusal() {
        let tree = SceneTree::new();
        let e = Target::from("不存在").resolve(&tree).unwrap_err();
        assert_eq!(
            e,
            CommandError::NoSuchLabel {
                label: "不存在".into()
            }
        );
        assert_eq!(e.to_string(), "no such label: 不存在");
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"error":"no_such_label","label":"不存在"}"#,
        );
    }

    /// Duplicates are refused with every candidate, in draw order — the order
    /// the renderer and the accessibility tree already use, so a host can look
    /// at the scene and know which id is which.
    #[test]
    fn a_duplicate_label_is_refused_with_its_candidates_in_draw_order() {
        let mut tree = SceneTree::new();
        let a = tree.insert(Mobject::new(vec![]).with_label("圆"));
        let b = tree.insert(Mobject::new(vec![]).with_label("圆"));
        let e = Target::from("圆").resolve(&tree).unwrap_err();
        assert_eq!(
            e,
            CommandError::AmbiguousLabel {
                label: "圆".into(),
                candidates: vec![a, b],
            }
        );
        assert_eq!(
            e.to_string(),
            format!("label \"圆\" names 2 nodes: {}, {}", a.raw(), b.raw()),
        );
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            format!(
                r#"{{"error":"ambiguous_label","label":"圆","candidates":[{},{}]}}"#,
                a.raw(),
                b.raw()
            ),
        );
    }

    /// A dead id is not resolved, so no command can act on one — the
    /// generational key's promise, kept for the new spelling too.
    #[test]
    fn a_dead_id_is_refused_not_resolved() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(vec![]));
        tree.remove(id);
        assert_eq!(
            Target::Id(id).resolve(&tree),
            Err(CommandError::NoSuchNode { target: id })
        );
    }

    /// The mutation commands resolve labels before touching anything, so the
    /// diff names the id — the part of the address a renderer can act on.
    #[test]
    fn commands_can_address_an_object_by_its_label() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(vec![]).with_label("方块"));

        let set: Command = serde_json::from_str(
            r#"{"action":"set","target":"方块","property":{"name":"opacity","value":0.25}}"#,
        )
        .unwrap();
        assert_eq!(apply(&mut tree, &set).unwrap().modified, [id]);
        assert_eq!(tree.get(id).unwrap().opacity, 0.25);

        let remove: Command =
            serde_json::from_str(r#"{"action":"remove","target":"方块"}"#).unwrap();
        assert_eq!(apply(&mut tree, &remove).unwrap().removed, [id]);
        assert!(tree.get(id).is_none());
    }

    /// `play` resolves both node names a spec can carry: the subject and a
    /// morph's donor. The animation itself never sees a label — `rustim-anim`
    /// has no tree to look one up in, and that is by design.
    #[test]
    fn a_play_can_name_its_subject_and_its_donor_by_label() {
        let mut tree = SceneTree::new();
        let a = tree.insert(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        let b = tree.insert(rustim_mobject::shapes::build(
            ShapeKind::Circle,
            &ShapeParams::default(),
        ));
        tree.get_mut(a).unwrap().label = Some("方".into());
        tree.get_mut(b).unwrap().label = Some("圆".into());

        let cmd: Command = serde_json::from_str(
            r#"{"action":"play","animation":{"target":"方","type":"transform","into":"圆"}}"#,
        )
        .unwrap();
        let Command::Play { animation } = cmd else {
            panic!("a play command");
        };
        animation_for(&tree, &animation).expect("both labels resolve");

        let missing: Command = serde_json::from_str(
            r#"{"action":"play","animation":{"target":"无","type":"fade_in"}}"#,
        )
        .unwrap();
        let Command::Play { animation } = missing else {
            panic!("a play command");
        };
        assert_eq!(
            animation_for(&tree, &animation).err(),
            Some(CommandError::NoSuchLabel {
                label: "无".into()
            })
        );
    }

    /// A camera `focus` is a target too, and resolving it by label gives the
    /// same camera as resolving it by id — a label cannot quietly focus
    /// something else.
    #[test]
    fn a_camera_focus_can_be_a_label() {
        let mut tree = SceneTree::new();
        let id = tree.insert(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        tree.get_mut(id).unwrap().label = Some("方块".into());
        let viewport = Viewport::default();

        let by_id = camera_for(&tree, &viewport, None, None, Some(&Target::Id(id)))
            .expect("an id resolves");
        let by_label = camera_for(&tree, &viewport, None, None, Some(&Target::from("方块")))
            .expect("the label resolves");
        assert_eq!(by_id, by_label);
        assert_eq!(
            camera_for(&tree, &viewport, None, None, Some(&Target::from("无"))).unwrap_err(),
            CommandError::NoSuchLabel {
                label: "无".into()
            }
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

    /// A group is the one node built from *other nodes*: every child named is
    /// resolved, moved under the new group, and reported — the group in
    /// `created`, each child in `modified`. Both address spellings work in the
    /// same list, and the draw order puts the group before the children it
    /// adopted.
    #[test]
    fn a_group_add_adopts_the_children_it_names() {
        let mut tree = SceneTree::new();
        let a = tree.insert(rustim_mobject::Mobject::new(vec![]).with_label("a"));
        let b = tree.insert(rustim_mobject::Mobject::new(vec![]));
        let cmd: Command = serde_json::from_str(&format!(
            r#"{{"action":"add","type":"group","params":{{"label":"g"}},"children":[{}, "a"]}}"#,
            b.raw()
        ))
        .unwrap();
        let diff = apply(&mut tree, &cmd).unwrap();
        let group = diff.created[0];
        assert_eq!(diff.created.len(), 1, "the group is the object");
        assert_eq!(diff.modified, [b, a]);
        assert_eq!(tree.child_ids(group), [b, a]);
        assert!(!tree.root_ids().contains(&a) && !tree.root_ids().contains(&b));

        let order = tree.draw_order();
        let at = |id| order.iter().position(|node| *node == id).unwrap();
        assert!(at(group) < at(b) && at(b) < at(a), "draw order: {order:?}");
    }

    /// `children` is a group's grammar. On any other kind it is a malformed
    /// payload, refused under the field it named — before anything is built.
    #[test]
    fn children_on_a_non_group_are_refused() {
        let mut tree = SceneTree::new();
        let a = tree.insert(rustim_mobject::Mobject::new(vec![]));
        let cmd: Command = serde_json::from_str(&format!(
            r#"{{"action":"add","type":"square","children":[{}]}}"#,
            a.raw()
        ))
        .unwrap();
        assert_eq!(
            apply(&mut tree, &cmd),
            Err(CommandError::OutOfRange {
                property: "children".to_owned(),
                detail: "only a group takes children".to_owned()
            })
        );
        assert!(tree.root_ids().contains(&a), "the child was not moved");
    }

    /// A polygon's geometry *is* its vertex list, so the list is validated
    /// under its own field before anything is built. The three ways it can be
    /// unusable — absent, too short, non-finite — are distinct sentences, and
    /// none of them inserts an object.
    #[test]
    fn a_polygon_without_three_finite_vertices_is_refused() {
        let mut tree = SceneTree::new();
        let mut refusals = vec![
            (
                serde_json::from_str::<Command>(r#"{"action":"add","type":"polygon"}"#).unwrap(),
                "a polygon needs its vertices",
            ),
            (
                serde_json::from_str::<Command>(
                    r#"{"action":"add","type":"polygon","params":{"points":[[0.0,0.0],[1.0,0.0]]}}"#,
                )
                .unwrap(),
                "a polygon needs at least three vertices",
            ),
        ];
        // NaN has no JSON spelling, so this case is built rather than parsed —
        // and it is the one a model can send by computing a vertex.
        refusals.push((
            Command::Add {
                kind: ShapeKind::Polygon,
                params: ShapeParams {
                    points: Some(vec![[0.0, 0.0], [1.0, 0.0], [f64::NAN, 1.0]]),
                    ..Default::default()
                },
                children: None,
            },
            "every vertex must be finite",
        ));
        for (cmd, detail) in refusals {
            assert_eq!(
                apply(&mut tree, &cmd),
                Err(CommandError::OutOfRange {
                    property: "points".to_owned(),
                    detail: detail.to_owned(),
                }),
                "{cmd:?}"
            );
        }
        assert!(tree.root_ids().is_empty(), "nothing was inserted");
    }

    /// **Nothing is inserted until every child resolves.** A `children` list
    /// with one bad entry must leave the tree exactly as it found it — a group
    /// that adopted half its children would be a compound object nobody can
    /// reason about, and the caller has no way to undo it.
    #[test]
    fn an_unresolvable_child_leaves_no_half_built_group() {
        let mut tree = SceneTree::new();
        let before = tree.len();
        let cmd: Command =
            serde_json::from_str(r#"{"action":"add","type":"group","children":[4294967297]}"#)
                .unwrap();
        assert_eq!(
            apply(&mut tree, &cmd),
            Err(CommandError::NoSuchNode {
                target: NodeId::from_raw(4294967297)
            })
        );
        assert_eq!(tree.len(), before, "no group, no colourless orphan");
    }

    /// An empty group is legal: "a compound object this lecture will fill as it
    /// goes" is a thing a script builds, and `children: []` says so.
    #[test]
    fn a_group_may_start_empty() {
        let mut tree = SceneTree::new();
        let cmd: Command =
            serde_json::from_str(r#"{"action":"add","type":"group","children":[]}"#).unwrap();
        let diff = apply(&mut tree, &cmd).unwrap();
        assert_eq!(diff.created.len(), 1);
        assert!(tree.child_ids(diff.created[0]).is_empty());
    }

    /// The pure path has no typesetter, so the two shaped kinds are refused
    /// with the feature named rather than inserted as invisible containers. A
    /// host that has a provider intercepts them before reaching here — see
    /// `SceneContext::add_typeset`.
    #[test]
    fn apply_refuses_the_two_kinds_it_cannot_shape() {
        let mut tree = SceneTree::new();
        for (kind, feature) in [(ShapeKind::Text, "text"), (ShapeKind::Formula, "formula")] {
            let cmd = Command::Add {
                kind,
                params: ShapeParams {
                    text: Some("x".to_owned()),
                    ..ShapeParams::default()
                },
                children: None,
            };
            assert_eq!(
                apply(&mut tree, &cmd),
                Err(CommandError::Unsupported {
                    feature: feature.to_owned()
                })
            );
        }
        assert!(tree.is_empty());
    }

    #[test]
    fn commands_naming_a_dead_node_are_refused_not_panicking() {
        let mut tree = SceneTree::new();
        let ghost = tree.insert(Mobject::new(vec![]));
        tree.remove(ghost);
        let cmd = Command::Set {
            target: ghost.into(),
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
            target: id.into(),
            property: Property::Opacity(7.0),
        };
        assert!(matches!(
            apply(&mut tree, &cmd),
            Err(CommandError::OutOfRange { .. })
        ));
        // ...and the object was not half-modified.
        assert_eq!(tree.get(id).unwrap().opacity, 1.0);
    }

    /// A motion command is answered by the context, not by the tree — but
    /// `apply` still has to be total over `Command`, so a direct caller gets an
    /// empty diff rather than a panic.
    #[test]
    fn a_motion_command_leaves_the_pure_apply_untouched() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(vec![]));
        let diff = apply(
            &mut tree,
            &Command::Motion {
                mode: MotionMode::Reduced,
            },
        )
        .unwrap();
        assert!(diff.is_empty());
        assert!(tree.get(id).is_some(), "and the tree is exactly as it was");
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
                    target: id.into(),
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

    /// The stroke's **shape** — join, cap, dash — is settable, and it is refused
    /// from `animate` rather than half-interpolated: there is no frame at which a
    /// join is half round, and a lerped dash pattern would make the dashes crawl.
    ///
    /// A dash is the one property whose `null` is a value: the question is "what
    /// dash pattern" and "none" is a legitimate answer, so it clears rather than
    /// being refused as a missing value.
    #[test]
    fn the_stroke_shape_is_settable_and_not_animatable() {
        use rustim_math::stroke::{Cap, Join};

        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(vec![]));

        let set = |tree: &mut SceneTree, property: Property| {
            apply(
                tree,
                &Command::Set {
                    target: id.into(),
                    property,
                },
            )
        };

        set(&mut tree, Property::StrokeJoin(Join::Bevel)).unwrap();
        set(&mut tree, Property::StrokeCap(Cap::Square)).unwrap();
        set(&mut tree, Property::StrokeDash(Some([2.0, 1.0]))).unwrap();
        let style = tree.get(id).unwrap().style;
        assert_eq!(style.stroke_join, Join::Bevel);
        assert_eq!(style.stroke_cap, Cap::Square);
        assert_eq!(style.stroke_dash, Some([2.0, 1.0]));
        assert!(tree.get(id).unwrap().dirty.style);

        // A pattern with a zero or negative entry draws a solid line or nothing,
        // and neither is a dashed line — the refusal says so under the field's own
        // name rather than handing the stroker a pattern with no meaning.
        for bad in [
            [0.0, 1.0],
            [1.0, -1.0],
            [f64::NAN, 1.0],
            [1.0, f64::INFINITY],
        ] {
            assert!(
                matches!(
                    set(&mut tree, Property::StrokeDash(Some(bad))),
                    Err(CommandError::OutOfRange { ref property, .. }) if property == "stroke_dash"
                ),
                "a dash of {bad:?} must be refused"
            );
        }
        assert_eq!(
            tree.get(id).unwrap().style.stroke_dash,
            Some([2.0, 1.0]),
            "a refused write changes nothing"
        );

        // ...and `null` clears it.
        set(&mut tree, Property::StrokeDash(None)).unwrap();
        assert_eq!(tree.get(id).unwrap().style.stroke_dash, None);

        for (name, property) in [
            ("stroke_join", Property::StrokeJoin(Join::Round)),
            ("stroke_cap", Property::StrokeCap(Cap::Butt)),
            ("stroke_dash", Property::StrokeDash(Some([1.0, 1.0]))),
        ] {
            let err = property
                .animatable()
                .expect_err("discrete, so not animatable");
            assert!(
                matches!(err, CommandError::OutOfRange { ref property, .. } if property == "property"),
                "{name} must be refused under `property`"
            );
            assert!(
                err.to_string().contains("discrete"),
                "and the hint must say why: {err}"
            );
        }
    }

    #[test]
    fn a_stroke_opacity_out_of_range_is_refused() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(vec![]));
        for bad in [1.5, -0.1, f64::NAN] {
            let cmd = Command::Set {
                target: id.into(),
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
                target: id.into(),
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
                target: id.into(),
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
        apply(
            &mut tree,
            &Command::Remove {
                target: parent.into(),
            },
        )
        .unwrap();
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
                    target: Some(id.into()),
                    kind: AnimationKind::FadeIn,
                    duration: None,
                    to: None,
                    angle: None,
                    turns_per_second: None,
                    factor: None,
                    property: None,
                    about_point: None,
                    into: None,
                    path_arc: None,
                    animations: None,
                    lag_ratio: None,
                    rate: None,
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
            target: Some(id.into()),
            kind,
            duration: None,
            to: None,
            angle: None,
            turns_per_second: None,
            factor: None,
            property: None,
            about_point: None,
            into: None,
            path_arc: None,
            animations: None,
            lag_ratio: None,
            rate: None,
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

    /// The destination is a *point*, interpolated toward rather than snapped to,
    /// and on an untransformed object it is the same matrix `set position` writes.
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

    /// The span of the materialised anchors, which is what a caller sees.
    fn width_of(tree: &SceneTree, id: NodeId) -> f64 {
        let anchors = tree.world_anchors(id).expect("a square has anchors");
        let xs = anchors.iter().map(|p| p.x);
        xs.clone().fold(f64::NEG_INFINITY, f64::max) - xs.fold(f64::INFINITY, f64::min)
    }

    fn scale_object(tree: &mut SceneTree, id: NodeId, factor: f64) {
        let centre = tree.get(id).unwrap().critical_point((0.0, 0.0)).unwrap();
        tree.get_mut(id)
            .unwrap()
            .set_transform(Affine2::scale_about(factor, centre));
    }

    /// "Put the centre here" is a statement about position, not about the whole
    /// matrix: a scaled object that moves is still scaled. Replacing the matrix
    /// with a translation was a silent ×0.5 on anything scaled first.
    #[test]
    fn a_move_to_command_keeps_the_objects_scale() {
        let (mut tree, id) = scene_with_a_blank();
        scale_object(&mut tree, id, 2.0);
        let mut what = spec(id, AnimationKind::MoveTo);
        what.to = Some([1.0, 0.5]);

        let mut anim = animation_for(&tree, &what).unwrap();
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        assert_eq!(
            tree.get(id).unwrap().critical_point((0.0, 0.0)),
            Some(Point::new(1.0, 0.5))
        );
        assert!(
            (width_of(&tree, id) - 4.0).abs() < 1e-12,
            "×2 scale was lost"
        );
    }

    /// The composition that made the defect visible: both members are built before
    /// either runs, so the move must resolve its destination when it *begins*, not
    /// when the command was read.
    #[test]
    fn a_scale_then_move_succession_ends_scaled_and_moved() {
        let (mut tree, id) = scene_with_a_blank();
        let mut scale = spec(id, AnimationKind::Scale);
        scale.factor = Some(2.0);
        scale.duration = Some(0.5);
        let mut mv = spec(id, AnimationKind::MoveTo);
        mv.to = Some([5.0, 0.0]);
        mv.duration = Some(0.5);

        let mut anim =
            animation_for(&tree, &composed(AnimationKind::Succession, vec![scale, mv])).unwrap();
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        assert_eq!(
            tree.get(id).unwrap().critical_point((0.0, 0.0)),
            Some(Point::new(5.0, 0.0))
        );
        assert!(
            (width_of(&tree, id) - 4.0).abs() < 1e-12,
            "the scale member must survive the move member"
        );
    }

    /// `set`'s `position` and `play`'s `move_to` agree, and both preserve the rest
    /// of the transform — which is what makes the property's "absolute" claim
    /// honest about *position* rather than about the matrix.
    #[test]
    fn setting_position_keeps_the_rest_of_the_transform() {
        let (mut tree, id) = scene_with_a_blank();
        scale_object(&mut tree, id, 3.0);
        apply(
            &mut tree,
            &Command::Set {
                target: id.into(),
                property: Property::Position([2.0, -1.0]),
            },
        )
        .unwrap();

        assert_eq!(
            tree.get(id).unwrap().critical_point((0.0, 0.0)),
            Some(Point::new(2.0, -1.0))
        );
        assert!(
            (width_of(&tree, id) - 6.0).abs() < 1e-12,
            "×3 scale was lost"
        );
    }

    /// A duration the command did not give is Manim's `DEFAULT_ANIMATION_RUN_TIME`,
    /// which is also what the animations' own constructors pick — so a command and
    /// a scene calling the constructor agree without either restating 1.0.
    #[test]
    fn an_omitted_duration_is_manims_default() {
        let (mut tree, id) = scene_with_a_blank();
        let donor = tree.insert(Mobject::new(rustim_mobject::shapes::circle(1.0)));
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
            AnimationKind::Transform,
            AnimationKind::Animate,
        ] {
            let mut what = spec(id, kind);
            match kind {
                AnimationKind::MoveTo => what.to = Some([0.0, 0.0]),
                AnimationKind::Rotate => what.angle = Some(1.0),
                AnimationKind::Scale => what.factor = Some(1.5),
                AnimationKind::Transform => what.into = Some(donor.into()),
                AnimationKind::Animate => what.property = Some(Property::Opacity(0.5)),
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

    /// `turns_per_second` is required, and **zero is a refusal rather than a
    /// no-op**: a rotating that turns nothing answers success while doing
    /// nothing, which is the class of wrong answer this layer exists to prevent.
    /// A **negative** rate is legal — the sign is the direction.
    #[test]
    fn a_rotating_command_needs_a_non_zero_finite_rate() {
        let (tree, id) = scene_with_a_blank();
        assert!(matches!(
            animation_for(&tree, &spec(id, AnimationKind::Rotating)),
            Err(CommandError::OutOfRange { property, .. }) if property == "turns_per_second"
        ));
        for bad in [0.0, f64::INFINITY, f64::NAN] {
            let mut what = spec(id, AnimationKind::Rotating);
            what.turns_per_second = Some(bad);
            assert!(
                matches!(
                    animation_for(&tree, &what),
                    Err(CommandError::OutOfRange { ref property, .. })
                        if property == "turns_per_second"
                ),
                "a rate of {bad} must be refused"
            );
        }
        // Both directions are rates. Asking through the box is also the one place
        // in the command layer that exercises the `perpetual` forward: the
        // scheduler drives every animation through this same container.
        for rate in [0.25, -0.25] {
            let mut what = spec(id, AnimationKind::Rotating);
            what.turns_per_second = Some(rate);
            let anim = animation_for(&tree, &what).expect("a signed rate is a rate");
            assert!(anim.perpetual(), "and it is the kind with no end");
        }
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

    // ---- the `animate` kind -----------------------------------------------

    /// The command's only real job is the mapping, so this drives the built
    /// animation through its cardinal points: it must start from the opacity the
    /// object has, move linearly, and land exactly on the target.
    #[test]
    fn an_animate_command_drives_a_style_channel() {
        let (mut tree, id) = scene_with_a_blank();
        tree.get_mut(id).unwrap().opacity = 0.8;
        let mut what = spec(id, AnimationKind::Animate);
        what.property = Some(Property::Opacity(0.2));
        what.duration = Some(0.4);

        let mut anim = animation_for(&tree, &what).unwrap();
        assert_eq!(anim.duration(), 0.4);
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);
        assert!((tree.get(id).unwrap().opacity - 0.5).abs() < 1e-12);
        anim.interpolate(1.0, &mut tree);
        assert_eq!(tree.get(id).unwrap().opacity, 0.2);
    }

    #[test]
    fn an_animate_command_needs_a_property() {
        let (tree, id) = scene_with_a_blank();
        assert!(matches!(
            animation_for(&tree, &spec(id, AnimationKind::Animate)),
            Err(CommandError::OutOfRange { property, .. }) if property == "property"
        ));
    }

    /// Every property that already has a dedicated animation is refused with an
    /// actionable message rather than duplicated worse: a componentwise
    /// `rotation` lerp is not a rotation, and a `position` lerp is `MoveTo` minus
    /// its `begin`-time resolution.
    #[test]
    fn an_animate_refuses_the_properties_that_have_their_own_animation() {
        let (tree, id) = scene_with_a_blank();
        for property in [
            Property::Position([1.0, 0.0]),
            Property::Scale(2.0),
            Property::Rotation(1.0),
            Property::Label("名字".into()),
            Property::Kind(NodeKind::Circle),
        ] {
            let mut what = spec(id, AnimationKind::Animate);
            what.property = Some(property.clone());
            let Err(CommandError::OutOfRange {
                property: field,
                detail,
            }) = animation_for(&tree, &what)
            else {
                panic!("{property:?} must not be animatable");
            };
            assert_eq!(field, "property");
            assert!(detail.contains("`animate`"), "{detail}");
        }
    }

    /// The value is checked by the same range `set` uses, so an `animate` cannot
    /// reach a state a `set` would have refused.
    #[test]
    fn an_animate_validates_its_value_like_a_set() {
        let (tree, id) = scene_with_a_blank();
        for (property, expected) in [
            (Property::Opacity(7.0), "opacity"),
            (Property::FillOpacity(-0.1), "fill_opacity"),
            (Property::StrokeOpacity(f64::NAN), "stroke_opacity"),
            (Property::StrokeWidth(-1.0), "stroke_width"),
        ] {
            let mut what = spec(id, AnimationKind::Animate);
            what.property = Some(property);
            assert!(
                matches!(
                    animation_for(&tree, &what),
                    Err(CommandError::OutOfRange { ref property, .. }) if property == expected
                ),
                "expected a refusal under {expected}"
            );
        }
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
            turns_per_second: None,
            factor: None,
            property: None,
            about_point: None,
            into: None,
            path_arc: None,
            animations: Some(members),
            lag_ratio: None,
            rate: None,
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
        what.target = Some(id.into());
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

    // ---- the P3 transform kind --------------------------------------------

    /// A group node with `n` square children, which is the shape a host building
    /// a family sends: one `add`, then `add`s into it.
    fn group_of(tree: &mut SceneTree, n: usize) -> NodeId {
        let group = tree.insert(Mobject::new(vec![]));
        for i in 0..n {
            let child = tree.insert(Mobject::new(rustim_mobject::shapes::square(2.0, 2.0)));
            tree.get_mut(child)
                .unwrap()
                .set_transform(Affine2::translate((i as f64, 0.0)));
            assert!(tree.reparent(child, group));
        }
        group
    }

    /// The command morphs the children, not just the source's own (empty) array:
    /// that is the whole difference between `transform` and the engine's leaf
    /// `Transform`, and a mapping that built the wrong one would pass every
    /// duration assertion.
    #[test]
    fn a_transform_command_morphs_the_whole_family() {
        let mut tree = SceneTree::new();
        let source = group_of(&mut tree, 1);
        let donor = group_of(&mut tree, 2);
        let donor_points: Vec<_> = tree
            .child_ids(donor)
            .iter()
            .map(|&id| tree.get(id).unwrap().points.clone())
            .collect();

        let mut what = spec(source, AnimationKind::Transform);
        what.into = Some(donor.into());
        let mut anim = animation_for(&tree, &what).unwrap();
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        let ids = tree.child_ids(source).to_vec();
        assert_eq!(ids.len(), 2, "the shorter side gains a faded copy");
        assert_eq!(tree.get(ids[0]).unwrap().points, donor_points[0]);
        assert_eq!(tree.get(ids[1]).unwrap().points, donor_points[1]);
        assert_eq!(tree.get(ids[1]).unwrap().opacity, 1.0);
    }

    /// The donor is read, never claimed: a host that morphed `a` into `b` still
    /// has `b`, unchanged and reachable.
    #[test]
    fn a_transform_command_leaves_the_donor_alone() {
        let mut tree = SceneTree::new();
        let source = group_of(&mut tree, 1);
        let donor = group_of(&mut tree, 1);
        let donor_child = tree.child_ids(donor)[0];
        let before = tree.get(donor_child).unwrap().points.clone();

        let mut what = spec(source, AnimationKind::Transform);
        what.into = Some(donor.into());
        let mut anim = animation_for(&tree, &what).unwrap();
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        assert_eq!(tree.get(donor_child).unwrap().points, before);
        assert!(tree.get(donor).is_some());
    }

    #[test]
    fn a_transform_command_needs_into() {
        let (tree, id) = scene_with_a_blank();
        assert!(matches!(
            animation_for(&tree, &spec(id, AnimationKind::Transform)),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "into"
        ));
    }

    #[test]
    fn a_transform_command_refuses_an_unknown_donor() {
        let (mut tree, id) = scene_with_a_blank();
        let ghost = tree.insert(Mobject::new(vec![]));
        tree.remove(ghost);
        let mut what = spec(id, AnimationKind::Transform);
        what.into = Some(ghost.into());
        assert!(matches!(
            animation_for(&tree, &what),
            Err(CommandError::NoSuchNode { target }) if target == ghost
        ));
    }

    /// The engine stays still for these three pairs (it has no error channel), so
    /// the refusal is the command layer's job — otherwise a host is told a beat
    /// played when nothing happened.
    #[test]
    fn a_transform_command_refuses_itself_and_its_own_relatives() {
        let mut tree = SceneTree::new();
        let parent = group_of(&mut tree, 1);
        let child = tree.child_ids(parent)[0];

        let mut itself = spec(parent, AnimationKind::Transform);
        itself.into = Some(parent.into());
        assert!(matches!(
            animation_for(&tree, &itself),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "into"
        ));

        let mut downward = spec(parent, AnimationKind::Transform);
        downward.into = Some(child.into());
        assert!(matches!(
            animation_for(&tree, &downward),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "into"
        ));

        let mut upward = spec(child, AnimationKind::Transform);
        upward.into = Some(parent.into());
        assert!(matches!(
            animation_for(&tree, &upward),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "into"
        ));
    }

    /// A shape and a group have no honest pairing here (`null_point_align` is not
    /// ported), so both directions are refused by name.
    #[test]
    fn a_transform_command_refuses_a_shape_into_a_group() {
        let mut tree = SceneTree::new();
        let shape = tree.insert(Mobject::new(rustim_mobject::shapes::square(2.0, 2.0)));
        let group = group_of(&mut tree, 1);

        let mut into_group = spec(shape, AnimationKind::Transform);
        into_group.into = Some(group.into());
        let Err(CommandError::OutOfRange { detail, .. }) = animation_for(&tree, &into_group) else {
            panic!("a shape into a group must be refused");
        };
        assert!(detail.contains("shape"), "{detail}");

        let mut into_shape = spec(group, AnimationKind::Transform);
        into_shape.into = Some(shape.into());
        let Err(CommandError::OutOfRange { detail, .. }) = animation_for(&tree, &into_shape) else {
            panic!("a group into a shape must be refused");
        };
        assert!(detail.contains("group"), "{detail}");
    }

    /// Leaves morph too: `transform` is not group-only, and a shape-to-shape pair
    /// is the case the engine's leaf `Transform` already covers one level down.
    #[test]
    fn a_transform_command_morphs_two_shapes() {
        let (mut tree, id) = scene_with_a_blank();
        let circle = tree.insert(Mobject::new(rustim_mobject::shapes::circle(1.0)));
        let mut what = spec(id, AnimationKind::Transform);
        what.into = Some(circle.into());

        let mut anim = animation_for(&tree, &what).unwrap();
        anim.begin(&tree);
        anim.interpolate(1.0, &mut tree);

        assert_eq!(
            tree.get(id).unwrap().points,
            tree.get(circle).unwrap().points
        );
    }

    /// `path_arc` crosses the wire and reaches the morph. The donor is *translated*
    /// — its motion lives entirely in the deferred transform — because that is the
    /// request the parameter exists for: an arc that only bent local point arrays
    /// would draw this straight.
    #[test]
    fn a_transform_command_carries_its_path_arc() {
        use std::f64::consts::PI;

        let mut tree = SceneTree::new();
        let source = tree.insert(Mobject::new(rustim_mobject::shapes::square(2.0, 2.0)));
        let donor = tree.insert(Mobject::new(rustim_mobject::shapes::square(2.0, 2.0)));
        tree.get_mut(donor)
            .unwrap()
            .set_transform(Affine2::translate((4.0, 0.0)));

        let mut what = spec(source, AnimationKind::Transform);
        what.into = Some(donor.into());
        what.path_arc = Some(PI);
        let mut anim = animation_for(&tree, &what).unwrap();
        anim.begin(&tree);
        anim.interpolate(0.5, &mut tree);

        let centre = tree
            .get(source)
            .unwrap()
            .critical_point((0.0, 0.0))
            .unwrap();
        // Halfway round a half-turn over a 4-unit chord: (2, 0) pushed one radius
        // along `cross(OUT, v)`, which for v = (+4, 0) is −y.
        assert!(
            (centre.x - 2.0).abs() < 1e-12 && (centre.y + 2.0).abs() < 1e-12,
            "{centre:?}"
        );
    }

    /// The curve on the wire is the curve in play.
    ///
    /// `ease` is the observable rather than a frame: it is where the scheduler
    /// reads the rate, so a wrapper that changed nothing would leave the default
    /// at `ease_in_out`'s `0.125`. The counterfactual is the second build.
    #[test]
    fn a_play_command_carries_its_rate() {
        let (tree, id) = scene_with_a_blank();

        let mut eased = spec(id, AnimationKind::FadeIn);
        eased.rate = Some(Rate::Linear);
        let anim = animation_for(&tree, &eased).unwrap();
        assert_eq!(anim.ease(0.25), 0.25);

        let plain = animation_for(&tree, &spec(id, AnimationKind::FadeIn)).unwrap();
        assert_eq!(
            plain.ease(0.25),
            0.125,
            "absent rate must keep the engine's default"
        );
    }

    /// The override wraps the whole composition, not its members: a group's own
    /// rate stays `linear` here, so a curve that changed `ease` can only have
    /// been applied outside. If the rate had been handed to the members instead,
    /// `ease(0.25)` would still be 0.25.
    #[test]
    fn a_group_can_carry_its_own_rate() {
        let (tree, id) = scene_with_a_blank();
        let mut what = composed(AnimationKind::Group, vec![spec(id, AnimationKind::FadeIn)]);
        what.rate = Some(Rate::EaseInCubic);

        let anim = animation_for(&tree, &what).unwrap();
        assert_eq!(anim.ease(0.25), 0.25f64.powi(3));
        assert_ne!(anim.ease(0.25), 0.25, "the group's default is linear");
    }

    /// ...and a member may carry its own, which shapes its motion inside the
    /// group's timeline. A quarter of the way into a cubic is a sixty-fourth, so
    /// the opacity says which curve reached it.
    #[test]
    fn a_rate_inside_a_group_shapes_only_its_member() {
        let (mut tree, id) = scene_with_a_blank();
        let mut member = spec(id, AnimationKind::FadeIn);
        member.duration = Some(1.0);
        member.rate = Some(Rate::EaseInCubic);
        let what = composed(AnimationKind::Group, vec![member]);

        let mut anim = animation_for(&tree, &what).unwrap();
        anim.begin(&tree);
        anim.interpolate(0.25, &mut tree);

        let opacity = tree.get(id).unwrap().opacity;
        assert!(
            (opacity - 0.25f64.powi(3)).abs() < 1e-12,
            "member opacity {opacity}, expected the cubic's 1/64"
        );
    }

    /// Absent means straight, but a number that arrived has to be a number. The
    /// engine propagates a non-finite angle as `NaN` geometry and cannot say so;
    /// this is the layer that can.
    #[test]
    fn a_non_finite_path_arc_is_refused() {
        let (mut tree, id) = scene_with_a_blank();
        let donor = tree.insert(Mobject::new(rustim_mobject::shapes::square(1.0, 1.0)));
        let mut what = spec(id, AnimationKind::Transform);
        what.into = Some(donor.into());
        what.path_arc = Some(f64::NAN);
        assert!(matches!(
            animation_for(&tree, &what),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "path_arc"
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
                target: id.into(),
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
                target: id.into(),
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
                            target: id.into(),
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

    // ---- the teaching tier's layout actions -------------------------------

    fn square_at(tree: &mut SceneTree, size: f64, at: [f64; 2]) -> NodeId {
        tree.insert(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams {
                width: Some(size),
                height: Some(size),
                position: Some(at),
                ..Default::default()
            },
        ))
    }

    /// The three actions reach the same placement the director's own tests
    /// measure, through the wire: labels resolve on both node fields, an
    /// already-correct placement reports nothing, and the reported `modified`
    /// is exactly what moved.
    #[test]
    fn next_to_places_by_label_and_reports_only_a_real_move() {
        let mut tree = SceneTree::new();
        let reference = square_at(&mut tree, 2.0, [0.0, 0.0]);
        let target = square_at(&mut tree, 1.0, [7.0, -5.0]);
        tree.get_mut(target).unwrap().label = Some("标签".into());
        tree.get_mut(reference).unwrap().label = Some("方块".into());

        let cmd: Command = serde_json::from_str(
            r#"{"action":"next_to","target":"标签","reference":"方块","direction":"right","buff":0.25}"#,
        )
        .unwrap();
        let diff = apply(&mut tree, &cmd).unwrap();
        assert_eq!(diff.modified, [target]);

        let t = tree.world_bounds(target).unwrap();
        let r = tree.world_bounds(reference).unwrap();
        assert!((t.x0 - r.x1 - 0.25).abs() < 1e-12, "{t:?} {r:?}");
        assert!((t.center().y - r.center().y).abs() < 1e-12);

        // Reasserting a placement that already holds must not claim a change:
        // a replay that reported a modified id every frame would make a host
        // redraw a still scene.
        assert!(apply(&mut tree, &cmd).unwrap().is_empty());
    }

    /// A reference inside the target cannot be placed away from it — the move
    /// carries the reference along — and the refusal names the field that
    /// carried the nodes. The reverse nesting is what "a label beside its
    /// shape" uses and must keep working.
    #[test]
    fn next_to_refuses_a_reference_inside_its_target() {
        let mut tree = SceneTree::new();
        let parent = square_at(&mut tree, 4.0, [0.0, 0.0]);
        let child = square_at(&mut tree, 1.0, [0.0, 0.0]);
        assert!(tree.reparent(child, parent));

        let nested = Command::NextTo {
            target: parent.into(),
            reference: child.into(),
            direction: Direction::Right,
            buff: None,
        };
        assert!(
            matches!(
                apply(&mut tree, &nested),
                Err(CommandError::OutOfRange { ref property, ref detail })
                    if property == "target" && detail.contains(&child.raw().to_string())
            ),
            "{:?}",
            apply(&mut tree, &nested)
        );

        let reverse = Command::NextTo {
            target: child.into(),
            reference: parent.into(),
            direction: Direction::Right,
            buff: Some(0.5),
        };
        assert_eq!(apply(&mut tree, &reverse).unwrap().modified, [child]);
    }

    #[test]
    fn arrange_puts_equal_gaps_in_a_row_and_refuses_nested_targets() {
        let mut tree = SceneTree::new();
        let a = square_at(&mut tree, 1.0, [-4.0, 3.0]);
        let b = square_at(&mut tree, 2.0, [0.0, -2.0]);
        let c = square_at(&mut tree, 0.5, [4.0, 1.0]);

        let row = Command::Arrange {
            targets: vec![a.into(), b.into(), c.into()],
            axis: Axis::Row,
            buff: Some(0.5),
        };
        assert_eq!(apply(&mut tree, &row).unwrap().modified, [a, b, c]);

        let ba = tree.world_bounds(a).unwrap();
        let bb = tree.world_bounds(b).unwrap();
        let bc = tree.world_bounds(c).unwrap();
        // Equal gaps between neighbours, not equal widths.
        assert!((bb.x0 - ba.x1 - 0.5).abs() < 1e-12);
        assert!((bc.x0 - bb.x1 - 0.5).abs() < 1e-12);
        // A row is a row even though its members started at three heights.
        assert!((ba.center().y - bb.center().y).abs() < 1e-12);
        assert!((bc.center().y - bb.center().y).abs() < 1e-12);

        // A parent and its child would each be moved by the arrangement, and
        // the child twice; the refusal is under the list's own field.
        let parent = square_at(&mut tree, 2.0, [0.0, 0.0]);
        let child = square_at(&mut tree, 1.0, [0.0, 0.0]);
        assert!(tree.reparent(child, parent));
        let nested = Command::Arrange {
            targets: vec![parent.into(), child.into()],
            axis: Axis::Column,
            buff: None,
        };
        assert!(matches!(
            apply(&mut tree, &nested),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "targets"
        ));
    }

    #[test]
    fn fit_scales_to_the_tighter_axis_and_refuses_a_bad_box() {
        let mut tree = SceneTree::new();
        let a = square_at(&mut tree, 1.0, [-1.0, 0.0]);
        let b = square_at(&mut tree, 1.0, [1.0, 0.0]);

        // The union box is 3 × 1, so a 6 × 2 target fits it at exactly 2 on
        // both axes; the fit is uniform and the centre does not move.
        let fit = Command::Fit {
            targets: vec![a.into(), b.into()],
            width: 6.0,
            height: 2.0,
        };
        assert_eq!(apply(&mut tree, &fit).unwrap().modified, [a, b]);
        let union = tree
            .world_bounds(a)
            .unwrap()
            .union(tree.world_bounds(b).unwrap());
        assert!((union.width() - 6.0).abs() < 1e-12);
        assert!((union.height() - 2.0).abs() < 1e-12);
        assert!(union.center().x.abs() < 1e-12);

        // A degenerate box is a payload error, named by its field.
        let bad = Command::Fit {
            targets: vec![a.into()],
            width: 0.0,
            height: 2.0,
        };
        assert!(matches!(
            apply(&mut tree, &bad),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "width"
        ));

        // A non-finite gap is the same kind of payload error, in the other
        // command's vocabulary.
        let nan = Command::NextTo {
            target: a.into(),
            reference: b.into(),
            direction: Direction::Left,
            buff: Some(f64::NAN),
        };
        assert!(matches!(
            apply(&mut tree, &nan),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "buff"
        ));
    }

    /// Resolution happens for the whole list before the first move: a bad name
    /// at the end must not leave the objects before it placed.
    #[test]
    fn a_bulk_placement_that_names_a_missing_node_moves_nothing() {
        let mut tree = SceneTree::new();
        let a = square_at(&mut tree, 1.0, [-3.0, 0.0]);
        let before = tree.world_bounds(a).unwrap();

        let missing = Command::Arrange {
            targets: vec![a.into(), "无".into()],
            axis: Axis::Row,
            buff: None,
        };
        assert_eq!(
            apply(&mut tree, &missing),
            Err(CommandError::NoSuchLabel {
                label: "无".into()
            })
        );
        assert_eq!(tree.world_bounds(a).unwrap(), before);
    }

    // ---- the teaching tier's attention actions ----------------------------

    /// `point_at` creates a real node whose tip sits the asked-for gap from the
    /// subject's world box, on the named side, pointing at it. The subject may
    /// be named by label, and the knobs a bare command omits take the defaults.
    #[test]
    fn point_at_builds_an_arrow_whose_tip_faces_the_subject() {
        let mut tree = SceneTree::new();
        let subject = square_at(&mut tree, 2.0, [0.0, 0.0]);
        tree.get_mut(subject).unwrap().label = Some("三角形".into());

        let cmd: Command = serde_json::from_str(
            r#"{"action":"point_at","target":"三角形","direction":"left","buff":0.25,"length":1.5,"color":[100,181,246],"label":"看这里"}"#,
        )
        .unwrap();
        let diff = apply(&mut tree, &cmd).unwrap();
        assert_eq!(diff.created.len(), 1);
        let arrow = tree.get(diff.created[0]).unwrap();
        assert_eq!(arrow.kind, NodeKind::Arrow);
        assert_eq!(arrow.label.as_deref(), Some("看这里"));
        assert_eq!(arrow.style.fill, rustim_color::BLUE);

        // A 2×2 square's left edge is at −1, so the tip is 0.25 further out and
        // the tail one length past the tip. The tip is the local point
        // `(length, 0)`; the tail is the local origin.
        let tip = arrow.transform * Point::new(1.5, 0.0);
        let tail = arrow.transform * Point::ZERO;
        assert!(
            (tip.x + 1.25).abs() < 1e-12 && tip.y.abs() < 1e-12,
            "tip {tip:?}"
        );
        assert!(
            (tail.x + 2.75).abs() < 1e-12 && tail.y.abs() < 1e-12,
            "tail {tail:?}"
        );

        // The bare spelling: default direction is `down` — below the subject,
        // pointing up at it — with the action's own default gap and length.
        let bare: Command =
            serde_json::from_str(r#"{"action":"point_at","target":"三角形"}"#).unwrap();
        let diff = apply(&mut tree, &bare).unwrap();
        let arrow = tree.get(diff.created[0]).unwrap();
        let tip = arrow.transform * Point::new(rustim_director::DEFAULT_POINT_AT_LENGTH, 0.0);
        assert!(
            tip.x.abs() < 1e-12
                && (tip.y + 1.0 + rustim_director::DEFAULT_POINT_AT_BUFF).abs() < 1e-12,
            "tip {tip:?}"
        );
    }

    /// A refusal leaves the tree untouched: an arrow is a new node, so "refused"
    /// has to mean "nothing was inserted", not "an arrow nobody wanted".
    #[test]
    fn point_at_refuses_a_bad_subject_and_a_bad_arrow() {
        let mut tree = SceneTree::new();
        let subject = square_at(&mut tree, 2.0, [0.0, 0.0]);
        let before = tree.len();
        let bare = |target: Target, buff, length, thickness| Command::PointAt {
            target,
            direction: None,
            buff,
            length,
            thickness,
            color: None,
            label: None,
        };

        assert_eq!(
            apply(&mut tree, &bare("无".into(), None, None, None)),
            Err(CommandError::NoSuchLabel {
                label: "无".into()
            })
        );
        let ghost = tree.insert(Mobject::new(vec![]));
        tree.remove(ghost);
        assert_eq!(
            apply(&mut tree, &bare(ghost.into(), None, None, None)),
            Err(CommandError::NoSuchNode { target: ghost })
        );
        // A node with no points and no declared box has nothing to point at.
        let empty = tree.insert(Mobject::new(vec![]));
        assert!(matches!(
            apply(&mut tree, &bare(empty.into(), None, None, None)),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "target"
        ));

        for (buff, length, thickness, field) in [
            (Some(f64::NAN), None, None, "buff"),
            (None, Some(0.0), None, "length"),
            (None, Some(f64::INFINITY), None, "length"),
            (None, None, Some(-0.1), "thickness"),
        ] {
            let cmd = bare(subject.into(), buff, length, thickness);
            assert!(
                matches!(
                    apply(&mut tree, &cmd),
                    Err(CommandError::OutOfRange { ref property, .. }) if property == field
                ),
                "{field} must be refused: {cmd:?}"
            );
        }
        assert_eq!(tree.len(), before + 1, "only the empty subject was added");
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
            camera_for(&tree, &Viewport::default(), None, None, Some(&Target::Id(ghost))),
            Err(CommandError::NoSuchNode { target }) if target == ghost
        ));
    }

    /// A node with neither points nor a declared box has nothing to frame.
    /// Refusing is honest: a silent success would leave the camera where it was
    /// and the host believing it had framed something.
    #[test]
    fn a_focus_on_a_node_without_geometry_is_refused() {
        let mut tree = SceneTree::new();
        let empty = tree.insert(Mobject::new(vec![]));
        assert!(matches!(
            camera_for(&tree, &Viewport::default(), None, None, Some(&Target::Id(empty))),
            Err(CommandError::OutOfRange { ref property, .. }) if property == "focus"
        ));
    }

    /// A text node is point-less but not geometry-less: its owner declares the
    /// layout box, and `focus` frames that box like any other object.
    #[test]
    fn a_focus_can_frame_a_text_nodes_declared_box() {
        let mut tree = SceneTree::new();
        let text = tree.insert(
            Mobject::new(vec![])
                .with_kind(NodeKind::Text)
                .with_local_bounds(Rect::new(-2.0, -0.5, 2.0, 0.5)),
        );

        let camera = camera_for(
            &tree,
            &Viewport::default(),
            None,
            None,
            Some(&Target::Id(text)),
        )
        .unwrap();
        assert_eq!(camera.center, (0.0, 0.0));
        // 4 wide and 1 tall: the width is the tighter axis at 1280x720
        // (visible width is ~14.22, so a 4-unit box fills it at ~2.96x), while
        // the height alone would allow 8 / (1.2 * 1) = 6.67x.
        let aspect = 1280.0 / 720.0;
        let want = Camera::WORLD_HEIGHT * aspect / (1.2 * 4.0);
        assert!((camera.zoom - want).abs() < 1e-12, "zoom {}", camera.zoom);
    }

    /// The fit leaves a 1.2 margin and is decided by the tighter axis. A 2×2 box
    /// at 1280×720: the frame is 8 units tall and ~14.2 wide, so the height is
    /// the tighter constraint — `8 / (1.2 * 2)`.
    #[test]
    fn a_focus_frames_the_box_with_a_margin() {
        let (tree, id) = tree_with_a_square();
        let camera = camera_for(
            &tree,
            &Viewport::default(),
            None,
            None,
            Some(&Target::Id(id)),
        )
        .unwrap();
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
        let camera = camera_for(&tree, &vp, None, None, Some(&Target::Id(id))).unwrap();
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
        let camera = camera_for(
            &tree,
            &Viewport::default(),
            None,
            None,
            Some(&Target::Id(id)),
        )
        .unwrap();
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
            Some(&Target::Id(id)),
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

    /// A line that carries nothing is skipped; a line that is not a command is a
    /// refusal, not an error and definitely not a panic. Both hosts — the desktop
    /// feed and the browser — read this answer from here.
    #[test]
    fn a_blank_line_is_a_skip_and_bad_json_is_a_refusal() {
        for blank in ["", "   ", "\t", " \r\n "] {
            assert!(parse_command(blank).is_none(), "{blank:?} must be a skip");
        }

        let parsed = parse_command(r#"{"action":"add","type":"circle","params":{"radius":1}}"#)
            .expect("a non-blank line is something");
        assert!(matches!(parsed, Ok(Command::Add { .. })));

        let refused = parse_command("{").expect("a broken line is something");
        assert!(matches!(refused, Err(CommandError::Unparseable { .. })));
        // Surrounding whitespace is the transport's, not the protocol's: a line
        // pasted from a log with a trailing newline still parses.
        assert!(
            parse_command("  {\"action\":\"remove\",\"target\":1}  \n")
                .unwrap()
                .is_ok(),
            "whitespace around a command must not change what it is"
        );
    }

    /// The presence of `diff`, `tree`, `report`, `capabilities` or `error` is
    /// the discriminant; the others are omitted, never written as null.
    #[test]
    fn a_reply_carries_exactly_one_half_and_never_a_null() {
        let applied: serde_json::Value = serde_json::from_str(&reply_line(
            3,
            Ok(CommandOutcome::Applied(StateDiff::default())),
        ))
        .unwrap();
        assert_eq!(applied["seq"], 3);
        assert!(applied.get("diff").is_some());
        assert!(applied.get("tree").is_none());
        assert!(applied.get("report").is_none());
        assert!(applied.get("error").is_none());

        // A read says so by carrying a document and no diff at all. An empty
        // diff here would be read as a mutation that did nothing, which is the
        // one thing a query is not.
        let queried: serde_json::Value = serde_json::from_str(&reply_line(
            4,
            Ok(CommandOutcome::Queried {
                tree: crate::a11y::build(&SceneTree::new(), &Viewport::default(), None),
                session: session(),
            }),
        ))
        .unwrap();
        assert_eq!(queried["seq"], 4);
        assert!(queried.get("tree").is_some());
        assert!(queried.get("diff").is_none());
        assert!(queried.get("report").is_none());
        assert!(queried.get("error").is_none());
        assert_eq!(queried["tree"]["nodes"].as_array().unwrap().len(), 1);

        // A lint answers in `report`, and it is present even when the list is
        // empty — "checked and clean" is an answer, and an absent field would
        // read as "did not ask". It is not a tree document, so `tree` stays
        // absent, which is the distinction from a query.
        let checked: serde_json::Value = serde_json::from_str(&reply_line(
            7,
            Ok(CommandOutcome::Checked(CheckReport::default())),
        ))
        .unwrap();
        assert_eq!(checked["seq"], 7);
        assert!(checked.get("report").is_some());
        assert!(checked.get("diff").is_none());
        assert!(checked.get("tree").is_none());
        assert!(checked.get("error").is_none());
        assert_eq!(checked["report"]["findings"].as_array().unwrap().len(), 0);

        // A greeting is the fifth presence: the document it answers with is
        // the build's vocabulary. It is neither a mutation nor a read of the
        // tree, so both of those fields are absent.
        let greeted: serde_json::Value = serde_json::from_str(&reply_line(
            6,
            Ok(CommandOutcome::Greeted(Capabilities::current())),
        ))
        .unwrap();
        assert_eq!(greeted["seq"], 6);
        assert!(greeted.get("capabilities").is_some());
        assert!(greeted.get("diff").is_none());
        assert!(greeted.get("tree").is_none());
        assert!(greeted.get("report").is_none());
        assert!(greeted.get("error").is_none());
        assert_eq!(greeted["capabilities"]["protocol"], PROTOCOL_VERSION);

        let refused: serde_json::Value = serde_json::from_str(&reply_line(
            5,
            Err(CommandError::Unparseable {
                detail: "expected value".to_owned(),
            }),
        ))
        .unwrap();
        assert_eq!(refused["seq"], 5);
        assert!(refused.get("error").is_some());
        assert!(refused.get("diff").is_none());
        assert!(refused.get("tree").is_none());
        assert!(refused.get("report").is_none());
        assert_eq!(refused["error"]["error"], "unparseable");
    }
}
