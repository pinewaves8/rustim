//! The scene graph projected into a semantic tree — plain data, no platform.
//!
//! # Why this is not AccessKit
//!
//! `方案建议.md` §4.6 asks for "the scene graph → an Accessibility tree, with
//! `NodeId` as the stable id". It is tempting to read that as "build
//! `accesskit::Node`s", but that would put a platform binding in a crate that has
//! to compile to `wasm32-unknown-unknown`, and it would be the wrong shape for
//! the Web target anyway: AccessKit has no web backend (the official crates are
//! windows/macos/unix/ios/android), so a browser build goes through DOM/ARIA.
//!
//! Those two consumers need the same **semantic data**, not the same tree. So the
//! data lives here, with no dependency beyond `rustim-mobject` and `kurbo`, and
//! `rustim-app::a11y` — the only module in the workspace that names `accesskit` —
//! projects it. The same structure is also the agent-facing `inspect` surface
//! (`rustim-app --inspect`, which prints it as JSON) and the input for a kittest-style
//! automated test, which is what makes "accessible" and "testable" the same
//! property here rather than two projects.
//!
//! # Two rules that are not stylistic
//!
//! 1. **Sizes are measured from materialised world geometry, never from the
//!    shape parameters.** `ShapeParams` is not stored on the mobject and the
//!    transform can change at any time, so the only truthful source is
//!    [`SceneTree::world_bounds`].
//! 2. **A description never contains a position.** On macOS and Windows a change
//!    to a node's description is an accessibility property-change event, and
//!    `MoveTo` changes the transform on every frame of an animation — a position
//!    in the text would rewrite every node's description 60 times a second.
//!    Size enters the description, position does not; movement is carried by
//!    `bounds`, which the backends only report when it really changed.

use serde::{Deserialize, Serialize};

use rustim_math::{Camera, Point, Rect, Viewport, bezier};
use rustim_mobject::{NodeId, NodeKind, SceneTree};

/// The semantic tree's root.
///
/// [`NodeId::RESERVED`], which no `SceneTree::insert` can produce. Note that the
/// obvious spelling — reserve raw `0` — is a trap: see that constant.
pub const ROOT: NodeId = NodeId::RESERVED;

/// How the outside world should see a node.
///
/// Deliberately smaller than `NodeKind`: several kinds are the same thing to a
/// reader. There is no `Figure` *and* `GraphicsObject` here because ARIA's
/// `graphics-object` is only valid inside a `graphics-document`, and our root is
/// a window — an unused role that could only be used wrongly is not worth
/// carrying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRole {
    /// A drawn figure: a shape, a plot, anything whose value is its geometry.
    Image,
    /// A container.
    Group,
    /// A run of text.
    Label,
    /// A heading, which a reader's navigation keys jump between.
    Heading,
    /// A formula. The kind exists from this step so that flattening the formula
    /// into a structured math subtree (P1 step 5, once Typst lands) adds fields
    /// to [`SemanticNode`] rather than changing this mapping.
    Math,
    /// A live region: a node whose text changes are announcements, not content.
    /// Only ever used for the announcement node, which is not a mobject.
    Status,
}

impl SemanticRole {
    pub fn from_kind(kind: NodeKind) -> Self {
        match kind {
            // A line and an arrow are pictures too. What distinguishes them is
            // their *kind*, which the description reads; the role a reader's
            // navigation acts on is the same for every figure here.
            NodeKind::Square
            | NodeKind::Circle
            | NodeKind::RightTriangle
            | NodeKind::Line
            | NodeKind::Arrow => Self::Image,
            NodeKind::Axes | NodeKind::Plot | NodeKind::NumberLine => Self::Image,
            NodeKind::Group => Self::Group,
            NodeKind::Text => Self::Label,
            NodeKind::Heading => Self::Heading,
            NodeKind::Formula => Self::Math,
            // Not a fallback for convenience: an unclassified mobject is a
            // picture, and "img" is what tells a reader to announce the name and
            // description rather than to walk imaginary children.
            NodeKind::Unknown => Self::Image,
        }
    }

    /// The role name a DOM/ARIA target would write, and the word `--inspect`
    /// prints. Kept beside [`Self::from_kind`] so the two mappings cannot drift.
    pub fn aria_role(self) -> &'static str {
        match self {
            Self::Image => "img",
            Self::Group => "group",
            Self::Label => "text",
            Self::Heading => "heading",
            Self::Math => "math",
            Self::Status => "status",
        }
    }
}

/// A named, adjustable value.
///
/// The keyboard layer and a screen reader's increment/decrement actions both
/// travel this: an arrow key becomes a `Property::Opacity` command, and the
/// platform's `Increment` action becomes the same command. One validation path,
/// so neither can produce a value the engine would refuse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticValue {
    pub current: f64,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    /// What a reader voices after the name — the value as the reader would say
    /// it, not the raw number.
    pub text: String,
}

/// One node of the semantic tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticNode {
    pub id: NodeId,
    pub role: SemanticRole,
    /// The short name, read first. This is what a reader focuses.
    pub name: String,
    /// The full sentence, read on request. See the module docs for why it holds
    /// no position.
    pub description: String,
    pub children: Vec<NodeId>,
    /// On-screen rectangle in **physical pixels, Y down, origin at the window's
    /// top-left** — the coordinate system every backend's `Node::bounds`
    /// documents. `None` for a node with no geometry of its own, such as the
    /// root or a pure group.
    pub bounds: Option<Rect>,
    /// Present but not currently painted. Reported rather than dropped so that a
    /// reader can say "3 of 4 objects are hidden" instead of silently
    /// misnumbering them.
    pub hidden: bool,
    pub value: Option<SemanticValue>,
}

/// The camera the bounds in this snapshot were projected with.
///
/// [`SemanticNode::bounds`] are **screen pixels**, and that is the right thing
/// for a platform — but it is not enough for the two consumers this layer also
/// serves. An AI host deciding where to put a new object, and a reader being told
/// which part of a figure is on screen, both need the inverse mapping, and the
/// alternative to shipping it is each of them re-deriving the projection from
/// `Viewport` and the camera — a duplicate that would drift from the one that
/// produced the bounds. So the view rides along as data.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CameraView {
    /// The world point at the centre of the frame.
    pub center: [f64; 2],
    pub zoom: f64,
    /// Frame size in physical pixels.
    pub frame: [u16; 2],
    /// How much of the world the frame shows, in world units: `[width, height]`.
    pub visible: [f64; 2],
}

impl CameraView {
    /// The snapshot's camera fields for a given view.
    pub fn new(camera: &Camera, viewport: &Viewport) -> Self {
        let height = camera.visible_height();
        let aspect = f64::from(viewport.frame_width) / f64::from(viewport.frame_height);
        Self {
            center: [camera.center.0, camera.center.1],
            zoom: camera.zoom,
            frame: [viewport.frame_width, viewport.frame_height],
            visible: [height * aspect, height],
        }
    }
}

/// The default view, for reading a snapshot written before the camera was part
/// of the protocol. The `bounds` in such a snapshot were made with it.
impl Default for CameraView {
    fn default() -> Self {
        Self::new(&Camera::default(), &Viewport::default())
    }
}

/// A complete snapshot of everything outside the engine needs to know.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticTree {
    pub root: NodeId,
    /// The view this snapshot's pixel bounds belong to; see [`CameraView`].
    #[serde(default)]
    pub camera: CameraView,
    pub nodes: Vec<SemanticNode>,
    /// The announcement text, if the engine wants to say something. Not a node
    /// id: the announcement node is the app layer's, since it exists only where
    /// there is a platform to announce to.
    pub live: Option<String>,
}

impl SemanticTree {
    pub fn get(&self, id: NodeId) -> Option<&SemanticNode> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

/// Project a scene tree. Pure: no clock, no I/O, no renderer, so the same tree
/// always yields the same semantic tree — which is what makes it unit-testable
/// and reusable by the Web target.
///
/// Nodes appear in draw order, root first. Focus is *not* a parameter: the
/// platform carries it in its own update struct, and making the tree's content
/// depend on interaction state would give the two projections (AccessKit and
/// DOM/ARIA) different trees for the same scene.
pub fn build(tree: &SceneTree, viewport: &Viewport, live: Option<String>) -> SemanticTree {
    let camera = tree.camera;
    let to_screen = viewport.affine(&camera);
    let mut nodes = Vec::with_capacity(tree.len() + 1);
    nodes.push(SemanticNode {
        id: ROOT,
        role: SemanticRole::Group,
        name: String::new(),
        description: String::new(),
        children: tree.root_ids().to_vec(),
        bounds: None,
        hidden: false,
        value: None,
    });

    for id in tree.draw_order() {
        let Some(m) = tree.get(id) else { continue };
        let anchors = tree.world_anchors(id);
        let shape = anchors.as_deref().and_then(Shape::of);
        // `bounds` and `describe` read two different things on purpose. The
        // box comes from `world_bounds`, which also knows about an object's
        // declared local extent — text has no anchors but does have a layout
        // box. The *shape* stays anchor-only: its corner and edge-length rules
        // are what make a rotated square report its side rather than its
        // bounding box, and a rectangle invented for text would put it through
        // those rules as if it were a polyline.
        let bounds = tree.world_bounds(id);
        let name = m
            .label
            .clone()
            .unwrap_or_else(|| default_name(m.kind).to_string());
        nodes.push(SemanticNode {
            id,
            role: SemanticRole::from_kind(m.kind),
            name,
            description: describe(m.kind, m.label.as_deref(), shape.as_ref()),
            children: tree.child_ids(id).to_vec(),
            bounds: bounds.map(|b| to_screen.transform_rect_bbox(b)),
            // "Cannot be seen", not "was faded out": an object with no picture is
            // no more on screen than a transparent one is. Both are reported
            // rather than omitted — a reader who is told a faded-out square exists
            // can ask for it to come back, and cannot ask about one that was
            // never mentioned. See `Mobject::has_picture`.
            hidden: m.opacity <= 0.0 || !m.has_picture(),
            value: Some(SemanticValue {
                current: m.opacity,
                min: 0.0,
                max: 1.0,
                step: 0.05,
                text: format!("不透明度 {:.0}%", m.opacity * 100.0),
            }),
        });
    }

    SemanticTree {
        root: ROOT,
        camera: CameraView::new(&camera, viewport),
        nodes,
        live,
    }
}

/// The name to use when an object has no label.
///
/// A Chinese noun phrase rather than a type name: this string is read aloud, and
/// "Square" is what a Rust enum is called, not what a person is told.
pub fn default_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Square => "方形",
        NodeKind::Circle => "圆",
        NodeKind::RightTriangle => "直角三角形",
        NodeKind::Line => "线段",
        NodeKind::Arrow => "箭头",
        NodeKind::NumberLine => "数轴",
        NodeKind::Group => "组合",
        NodeKind::Text => "文本",
        NodeKind::Heading => "标题",
        NodeKind::Formula => "公式",
        NodeKind::Axes => "坐标轴",
        NodeKind::Plot => "函数图象",
        NodeKind::Unknown => "图形",
    }
}

/// A node's materialised world geometry, reduced to what a description needs.
///
/// Both fields come from the same anchors, so they cannot disagree about which
/// frame they are in.
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    /// The axis-aligned box. Exact for a circle, an overestimate for anything
    /// rotated.
    pub bounds: Rect,
    /// The distinct corners of a closed polyline, for a shape that is one.
    /// A circle's anchors are on the curve, so its "corners" are not corners and
    /// no kind that uses them reads them.
    pub corners: Vec<Point>,
}

impl Shape {
    /// `None` for a node with no geometry at all.
    pub fn of(anchors: &[Point]) -> Option<Self> {
        Some(Self {
            bounds: bezier::bounds_of(anchors)?,
            corners: bezier::corners(anchors),
        })
    }
}

/// One sentence describing a node: what it is and how big, never where.
///
/// # What the numbers are, exactly
///
/// Sizes come from the node's **world-space anchors** under its own and its
/// ancestors' transforms, so a scaled object reports its scaled size and the
/// parameters are never consulted — they are not stored on the mobject.
///
/// For a polyline the size comes from its *edges*, not its box, and that is not
/// a refinement: the box of a rotated square is `side * (cos + sin)`, so the
/// reference scene's hypotenuse square — turned by the angle of the 3-4-5
/// triangle — would be announced as a 4.2-square of area 17.64 when it is a
/// 3-square of area 9. A reader told that is told the Pythagorean theorem is
/// false. Edge lengths are invariant under the rotation, and they are what
/// [`Shape::corners`] exists to make available.
///
/// Only shapes whose kind says what they are measured this way. Everything else
/// — a circle, an unclassified mobject — falls back to the box, which for a
/// circle is exact.
pub fn describe(kind: NodeKind, label: Option<&str>, shape: Option<&Shape>) -> String {
    // A label *names* the object, so when one is present it speaks for the kind
    // and the body below stops repeating it. Without this, a labelled square is
    // read as "a 的正方形，边长 3 的正方形，面积 9" — the noun three times in one
    // breath, which is the kind of thing that only shows up when someone
    // actually listens to it.
    let named = label.is_some() && !mentions_its_own_text(kind);
    let body = match kind {
        NodeKind::Text | NodeKind::Heading | NodeKind::Formula => match label {
            Some(label) => label.to_string(),
            None => default_name(kind).to_string(),
        },
        NodeKind::Group => "包含若干对象的组合".to_string(),
        _ => match shape {
            None => default_name(kind).to_string(),
            Some(s) => {
                let (w, h) = (s.bounds.width(), s.bounds.height());
                match kind {
                    NodeKind::Circle => {
                        let radius = w.min(h) / 2.0;
                        let area = num(radius * radius * PI);
                        if named {
                            format!("半径 {}，面积 {area}", num(radius))
                        } else {
                            format!("半径 {} 的圆，面积 {area}", num(radius))
                        }
                    }
                    NodeKind::Square => rectangle_sentence(s, w, h, !named),
                    NodeKind::RightTriangle => triangle_sentence(s, w, h, !named),
                    NodeKind::Line | NodeKind::Arrow | NodeKind::NumberLine => {
                        line_sentence(kind, s, w, h, !named)
                    }
                    _ if named => format!("{} × {}", num(w), num(h)),
                    _ => format!("{} × {} 的{}", num(w), num(h), default_name(kind)),
                }
            }
        },
    };
    match label {
        // The label leads: a reader who has heard "a 的正方形" does not need the
        // geometry repeated, and the label is the part the AI layer addresses.
        Some(label) if !mentions_its_own_text(kind) => format!("{label}，{body}"),
        _ => body,
    }
}

/// Whether this kind's description *is* its text, in which case a label adds
/// nothing to it and must not be prefixed to it.
fn mentions_its_own_text(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::Text | NodeKind::Heading | NodeKind::Formula)
}

/// A four-cornered figure, from its edges when it has them.
///
/// One tolerance, in world units, shared by the two things it decides: an
/// isoceles box is a square, and which of two opposite edges is the figure's
/// width. Manim's frame is 8 units tall, so `1e-9` is a tenth of a nanometre on
/// screen — far below any difference a scene means to draw, and far above the
/// f64 error of one affine application.
const SAME: f64 = 1e-9;

/// `with_noun` is false when the caller has already named the kind.
fn rectangle_sentence(s: &Shape, w: f64, h: f64, with_noun: bool) -> String {
    let (side, other) = match quad_edges(s) {
        // The corners are in order, so the first two edges are one pair of the
        // rectangle's sides. Opposite edges agreeing is what makes the two
        // numbers a width and a height rather than two arbitrary chords — and if
        // they do not, this is not a rectangle and the box is all that can
        // honestly be said about it.
        Some(e) if same(e[0], e[2]) && same(e[1], e[3]) => (e[0], e[1]),
        _ => (w, h),
    };
    let area = num(side * other);
    match (same(side, other), with_noun) {
        (true, true) => format!("边长 {} 的正方形，面积 {area}", num(side)),
        (true, false) => format!("边长 {}，面积 {area}", num(side)),
        (false, true) => format!("{} × {} 的矩形，面积 {area}", num(side), num(other)),
        (false, false) => format!("{} × {}，面积 {area}", num(side), num(other)),
    }
}

/// A right triangle: its two shorter sides are its legs, and its area is half
/// their product.
///
/// The right angle is in the name, so the ordering is not a guess — the hypotenuse
/// is the longest side of any triangle, and taking the two shorter ones is what
/// makes the two numbers this reports the legs a reader is being told about. A
/// figure with corners that do not form a triangle falls back to the box.
fn triangle_sentence(s: &Shape, w: f64, h: f64, with_noun: bool) -> String {
    // Three edges, sorted ascending: the two shortest are the legs, whichever
    // corner the scene happened to start at and however the figure is turned.
    let mut edges = match s.corners.as_slice() {
        [a, b, c] => [distance(a, b), distance(b, c), distance(c, a)],
        // Not a triangle. Falling back to the box's own dimensions keeps the
        // sentence's *shape* — "two legs and an area" — rather than reporting a
        // hypotenuse of infinity.
        _ => [w, h, w.max(h)],
    };
    edges.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let [first, second, _hypotenuse] = edges;
    let legs = format!("两条直角边长 {} 和 {}", num(first), num(second));
    let area = num(first * second / 2.0);
    if with_noun {
        format!("{legs} 的直角三角形，面积 {area}")
    } else {
        format!("{legs}，面积 {area}")
    }
}

/// A line, an arrow or a number line: its **length**.
///
/// The one thing about any of them that a reader can act on, and the thing the
/// fallback below cannot say: a line is a **stroked centerline** (see
/// `rustim_mobject::shapes::line`), so its box has a zero height and the stroke
/// width is not part of the geometry at all — but an arrow's box still mixes the
/// head's width into the answer, and a box-based sentence would announce a
/// number line as "6 × 0.3 的图形" with the ticks read as a dimension. The
/// diameter of the corner set is the length in every one of those cases.
///
/// A number line joins them rather than taking the box sentence because its box
/// includes the ticks, whose height is a decoration and not a dimension: the
/// length is the shaft, and the corner set's diameter *is* the shaft's length
/// because the shaft's own ends are its farthest corners.
///
/// Neither is measured from the box, and for the line that is the same reason
/// [`rectangle_sentence`] reads edges: a rotated line's box is
/// `(L cos θ + t sin θ) × …`, so its "length" would shrink as the figure turned.
/// A line's corners are its rectangle, so its long edge is available and exact.
///
/// An arrow's corners are **not** its endpoints — its tail is two corners and its
/// tip is a seventh — so there is no edge that spans it and the measurement is the
/// corner set's **diameter**. That is the arrow's tail-to-tip length to within
/// `t² / 2L` (a part in ten thousand at the default thickness); the alternative,
/// measuring a rotated arrow by its box, reported a 3-unit arrow as 2.49 the first
/// time this scene ran with one turned through 34°.
///
/// The thickness is deliberately not reported: it is a rendering width rather than
/// a property of the figure, and "长 3、粗 0.05 的线段" is a sentence no listener
/// asked for.
fn line_sentence(kind: NodeKind, s: &Shape, w: f64, h: f64, with_noun: bool) -> String {
    // The corners of this outline are in order, so a rectangle's first two edges
    // are its length and its thickness, in some order.
    let length = match quad_edges(s) {
        Some(e) if same(e[0], e[2]) && same(e[1], e[3]) => e[0].max(e[1]),
        _ => diameter(s).unwrap_or(w.max(h)),
    };
    if with_noun {
        format!("长度 {} 的{}", num(length), default_name(kind))
    } else {
        format!("长度 {}", num(length))
    }
}

/// The greatest distance between any two of a shape's corners.
///
/// `None` for fewer than two corners, and for a set that is all one point — a
/// figure with no extent has no length to report, and caller falls back to its box.
fn diameter(s: &Shape) -> Option<f64> {
    let mut best = 0.0_f64;
    for (i, a) in s.corners.iter().enumerate() {
        for b in &s.corners[i + 1..] {
            best = best.max(distance(a, b));
        }
    }
    (best > 0.0).then_some(best)
}

/// The four edge lengths of a quadrilateral, in order, or `None` if the shape
/// has some other number of corners.
fn quad_edges(s: &Shape) -> Option<[f64; 4]> {
    let [a, b, c, d] = s.corners.as_slice() else {
        return None;
    };
    Some([
        distance(a, b),
        distance(b, c),
        distance(c, d),
        distance(d, a),
    ])
}

fn same(a: f64, b: f64) -> bool {
    (a - b).abs() < SAME
}

fn distance(a: &Point, b: &Point) -> f64 {
    ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt()
}

const PI: f64 = std::f64::consts::PI;

/// A number as a reader should hear it: at most two decimals, no trailing zeros.
///
/// Fixed formatting because the description is compared and tested as a string;
/// whatever the float prints by default is neither stable across platforms nor
/// meaningful to a listener.
fn num(v: f64) -> String {
    // Round *before* formatting, and treat a zero result as zero outright. The
    // tempting inverse — format, then repair the string — misses `-0.001`:
    // `"-0.00"` trims its trailing zeros to `"-0."` and then its trailing dot to
    // `"-0"`, and no amount of `trim_end_matches` catches that without also
    // eating the `0` of a legitimate `10`. `-0.0 == 0.0` in IEEE 754, so this
    // check covers negative zero for free.
    let v = (v * 100.0).round() / 100.0;
    if v == 0.0 {
        return "0".to_string();
    }
    format!("{v:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_math::Affine;
    use rustim_mobject::shapes::{ShapeKind, ShapeParams};

    fn scene() -> SceneTree {
        let mut tree = SceneTree::new();
        tree.insert(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams {
                width: Some(3.0),
                height: Some(3.0),
                label: Some("a 的正方形".into()),
                ..Default::default()
            },
        ));
        tree
    }

    #[test]
    fn the_root_is_not_a_scene_node() {
        // The whole reservation scheme in one assertion: build a tree, and the
        // root must not be one of the nodes in it.
        let tree = scene();
        let s = build(&tree, &Viewport::default(), None);
        assert_eq!(s.root, ROOT);
        assert_eq!(s.nodes[0].id, ROOT);
        assert!(
            tree.get(ROOT).is_none(),
            "the reserved root id must not resolve in a real tree"
        );
        // Compared by equality, not by magnitude: the reserved id is slotmap's
        // null key, which packs a generation just like a real one does, so
        // "is this raw big enough to be a mobject" would accept it.
        for node in &s.nodes[1..] {
            assert_ne!(node.id, ROOT);
            assert!(
                tree.get(node.id).is_some(),
                "{:#x} is not a mobject",
                node.id.raw()
            );
        }
    }

    #[test]
    fn every_node_appears_exactly_once_and_children_resolve() {
        // A flat list plus child id lists is only a tree if the two agree. A
        // missing child is the failure that makes a reader's "next sibling"
        // silently skip an object.
        let mut tree = scene();
        let parent = tree.root_ids()[0];
        let child = tree.insert(rustim_mobject::shapes::build(
            ShapeKind::Circle,
            &ShapeParams::default(),
        ));
        assert!(tree.reparent(child, parent));
        let s = build(&tree, &Viewport::default(), None);

        let ids: Vec<NodeId> = s.nodes.iter().map(|n| n.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_by_key(|id| id.raw());
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "duplicate node ids");
        assert_eq!(
            ids.len(),
            tree.len() + 1,
            "one node per mobject plus the root"
        );

        for node in &s.nodes {
            for c in &node.children {
                assert!(s.get(*c).is_some(), "child {c:?} has no node");
            }
        }
        assert_eq!(s.nodes[0].children, vec![parent]);
    }

    #[test]
    fn the_root_lists_only_roots_and_a_reparent_moves_the_child() {
        let mut tree = scene();
        let a = tree.root_ids()[0];
        let b = tree.insert(rustim_mobject::shapes::build(
            ShapeKind::Circle,
            &ShapeParams::default(),
        ));
        assert_eq!(
            build(&tree, &Viewport::default(), None).nodes[0].children,
            vec![a, b]
        );
        assert!(tree.reparent(b, a));
        assert_eq!(
            build(&tree, &Viewport::default(), None).nodes[0].children,
            vec![a]
        );
    }

    #[test]
    fn sizes_are_measured_not_declared() {
        // A 3-unit square at 90 px per unit is 270 px on a side, and the scale
        // transform must show up in the reported size or the layer would be
        // reporting the parameters rather than the geometry.
        let mut tree = scene();
        let id = tree.root_ids()[0];
        let s = build(&tree, &Viewport::default(), None);
        let b = s.get(id).unwrap().bounds.unwrap();
        assert!((b.width() - 270.0).abs() < 1e-9, "{b:?}");

        tree.get_mut(id)
            .unwrap()
            .set_transform(rustim_math::Affine::scale(2.0));
        let s = build(&tree, &Viewport::default(), None);
        let scaled = s.get(id).unwrap().bounds.unwrap();
        assert!((scaled.width() - 540.0).abs() < 1e-9, "{scaled:?}");
        assert!(
            s.get(id).unwrap().description.contains("边长 6"),
            "{}",
            s.get(id).unwrap().description
        );
    }

    #[test]
    fn bounds_are_in_screen_pixels_y_down() {
        // The coordinate flip is the one thing the two projections share by
        // construction, and getting it wrong puts a reader's focus rectangle
        // somewhere else on the screen. A square centred on the world origin is
        // centred on the frame; one above the origin must have the *smaller* y.
        let mut tree = SceneTree::new();
        let hi = tree.insert(
            rustim_mobject::shapes::build(
                ShapeKind::Square,
                &ShapeParams {
                    position: Some([0.0, 2.0]),
                    ..Default::default()
                },
            )
            .with_kind(NodeKind::Square),
        );
        let lo = tree.insert(
            rustim_mobject::shapes::build(
                ShapeKind::Square,
                &ShapeParams {
                    position: Some([0.0, -2.0]),
                    ..Default::default()
                },
            )
            .with_kind(NodeKind::Square),
        );
        let s = build(&tree, &Viewport::default(), None);
        let hi = s.get(hi).unwrap().bounds.unwrap();
        let lo = s.get(lo).unwrap().bounds.unwrap();
        assert!(
            hi.y1 < lo.y0,
            "world +y must be screen -y: {hi:?} vs {lo:?}"
        );
        assert!(((hi.x0 + hi.x1) / 2.0 - 640.0).abs() < 1e-9);
    }

    #[test]
    fn a_description_never_mentions_a_position() {
        // The load-bearing property: this is what keeps a `MoveTo` from
        // rewriting every node's description on every frame.
        let mut tree = scene();
        let id = tree.root_ids()[0];
        let before = build(&tree, &Viewport::default(), None);
        let position = before.get(id).unwrap().description.clone();
        tree.get_mut(id)
            .unwrap()
            .set_transform(rustim_math::Affine::translate((3.0, -2.0)));
        let after = build(&tree, &Viewport::default(), None);
        assert_eq!(position, after.get(id).unwrap().description);
        // ...but the bounds did move, which is where position belongs.
        assert_ne!(
            before.get(id).unwrap().bounds,
            after.get(id).unwrap().bounds
        );
    }

    #[test]
    fn a_hidden_object_is_reported_not_dropped() {
        let mut tree = scene();
        let id = tree.root_ids()[0];
        tree.get_mut(id).unwrap().opacity = 0.0;
        let s = build(&tree, &Viewport::default(), None);
        let n = s.get(id).unwrap();
        assert!(n.hidden);
        assert_eq!(s.nodes.len(), 2, "the node is still in the tree");
    }

    #[test]
    fn descriptions_read_like_sentences() {
        // Built through `shapes::build`, not from a rectangle literal, so the
        // corners the sentences measure are the corners a scene would really
        // produce — including the control points `points_as_corners` inserts.
        let shaped = |kind: ShapeKind, params: &ShapeParams, node: NodeKind| {
            let m = rustim_mobject::shapes::build(kind, params).with_kind(node);
            let t = Affine::IDENTITY;
            let anchors =
                rustim_math::bezier::anchors(&m.points.iter().map(|p| t * *p).collect::<Vec<_>>())
                    .unwrap();
            Shape::of(&anchors).unwrap()
        };

        let square = shaped(
            ShapeKind::Square,
            &ShapeParams {
                width: Some(3.0),
                height: Some(3.0),
                ..Default::default()
            },
            NodeKind::Square,
        );
        assert_eq!(
            describe(NodeKind::Square, None, Some(&square)),
            "边长 3 的正方形，面积 9"
        );

        let rect = shaped(
            ShapeKind::Square,
            &ShapeParams {
                width: Some(4.0),
                height: Some(2.0),
                ..Default::default()
            },
            NodeKind::Square,
        );
        assert_eq!(
            describe(NodeKind::Square, None, Some(&rect)),
            "4 × 2 的矩形，面积 8"
        );

        let tri = shaped(
            ShapeKind::RightTriangle,
            &ShapeParams {
                width: Some(3.0),
                height: Some(4.0),
                ..Default::default()
            },
            NodeKind::RightTriangle,
        );
        assert_eq!(
            describe(NodeKind::RightTriangle, None, Some(&tri)),
            "两条直角边长 3 和 4 的直角三角形，面积 6"
        );

        let circle = shaped(
            ShapeKind::Circle,
            &ShapeParams {
                radius: Some(1.5),
                ..Default::default()
            },
            NodeKind::Circle,
        );
        assert_eq!(
            describe(NodeKind::Circle, None, Some(&circle)),
            format!("半径 1.5 的圆，面积 {}", num(2.25 * PI))
        );

        // The label leads and the kind noun is not repeated after it — that is
        // what a reader hears, and it is checked as a string rather than by
        // inspection. Text is its own description, so its label is not
        // prefixed to itself.
        assert_eq!(
            describe(NodeKind::Square, Some("a 的正方形"), Some(&square)),
            "a 的正方形，边长 3，面积 9"
        );
        assert_eq!(describe(NodeKind::Text, Some("斜边"), None), "斜边");
        // Every kind that takes a label in the reference scene, so that adding a
        // kind whose labelled form still repeats its noun is a failing test
        // rather than something a listener has to notice.
        assert_eq!(
            describe(NodeKind::RightTriangle, Some("三角形"), Some(&tri)),
            "三角形，两条直角边长 3 和 4，面积 6"
        );
        assert_eq!(
            describe(NodeKind::Circle, Some("单位圆"), Some(&circle)),
            format!("单位圆，半径 1.5，面积 {}", num(2.25 * PI))
        );
        assert_eq!(
            describe(NodeKind::Unknown, Some("随便什么"), Some(&square)),
            "随便什么，3 × 3"
        );
    }

    #[test]
    fn a_rotated_square_is_measured_by_its_edges_not_its_box() {
        // The reference scene's hypotenuse square: side 3, turned by the angle of
        // the 3-4-5 triangle — a 53.13° rotation, `cos + sin == 1.4`. Its box is
        // therefore 4.2 on a side, and a box-measured description announces an
        // area of 17.64 for a square of 9. This scene exists to say that
        // 9 + 16 == 25, so that number is the opposite of what it should teach.
        let pts = rustim_mobject::shapes::square(3.0, 3.0);
        let t = Affine::rotate(4.0f64.atan2(3.0));
        let anchors = bezier::anchors(&pts.iter().map(|p| t * *p).collect::<Vec<_>>()).unwrap();
        let shape = Shape::of(&anchors).unwrap();
        assert!(
            (shape.bounds.width() - 4.2).abs() < 1e-9,
            "the box is the overestimate that must not be reported: {:?}",
            shape.bounds
        );
        assert_eq!(
            describe(NodeKind::Square, None, Some(&shape)),
            "边长 3 的正方形，面积 9"
        );

        // Rotating a triangle must not turn its legs into its box either.
        let pts = rustim_mobject::shapes::right_triangle(3.0, 4.0);
        let t = Affine::rotate(0.7);
        let anchors = bezier::anchors(&pts.iter().map(|p| t * *p).collect::<Vec<_>>()).unwrap();
        let shape = Shape::of(&anchors).unwrap();
        assert_eq!(
            describe(NodeKind::RightTriangle, None, Some(&shape)),
            "两条直角边长 3 和 4 的直角三角形，面积 6"
        );
    }

    #[test]
    fn a_shape_with_no_geometry_still_describes_itself() {
        assert_eq!(describe(NodeKind::Circle, None, None), "圆");
        assert_eq!(describe(NodeKind::Unknown, None, None), "图形");
    }

    #[test]
    fn numbers_are_formatted_for_listening() {
        assert_eq!(num(3.0), "3");
        assert_eq!(num(3.5), "3.5");
        assert_eq!(num(3.25), "3.25");
        assert_eq!(num(1.0 / 3.0), "0.33");
        assert_eq!(num(0.0), "0");
        assert_eq!(num(-0.001), "0");
    }

    /// The world-space shape of a built mobject, as the layer would measure it.
    fn shape_of(m: &rustim_mobject::Mobject) -> Shape {
        let anchors = rustim_math::bezier::anchors(&m.materialize()).unwrap();
        Shape::of(&anchors).unwrap()
    }

    /// A line is described by its **length**.
    ///
    /// The `Line`/`Arrow` arms are the point of this edit rather than a
    /// formality: `describe`'s match has a `_` fallback, so omitting them
    /// compiles, every other test still passes, and a 3-unit line is announced as
    /// "3 × 0.045 的图形" — the stroke width read out as if it were a dimension,
    /// which is exactly the kind of confident nonsense a fallback branch produces.
    /// The last assertion is the one a box-based sentence cannot pass.
    #[test]
    fn a_line_is_described_by_its_length() {
        let params = ShapeParams {
            width: Some(3.0),
            thickness: Some(0.045),
            ..Default::default()
        };
        let line = rustim_mobject::shapes::build(ShapeKind::Line, &params);
        let s = shape_of(&line);
        assert_eq!(describe(NodeKind::Line, None, Some(&s)), "长度 3 的线段");
        assert_eq!(
            describe(NodeKind::Line, Some("边 a"), Some(&s)),
            "边 a，长度 3"
        );

        // Turned, the length is still 3 — while the box the fallback would have
        // read is a good deal shorter, because a rotated segment's box is its
        // projection onto both axes rather than its extent along its own.
        let mut turned = line;
        turned.set_transform(Affine::rotate(0.7));
        let s = shape_of(&turned);
        assert!(
            s.bounds.width() < 2.5,
            "the box really is the overestimate here: {:?}",
            s.bounds
        );
        assert_eq!(describe(NodeKind::Line, None, Some(&s)), "长度 3 的线段");
    }

    #[test]
    fn an_arrow_is_described_by_its_length() {
        let params = ShapeParams {
            width: Some(3.0),
            thickness: Some(0.045),
            ..Default::default()
        };
        let arrow = rustim_mobject::shapes::build(ShapeKind::Arrow, &params);
        let s = shape_of(&arrow);
        // Seven corners — the tail is *two* of them and the tip is a seventh — so
        // no single edge spans the arrow and the measurement is the corner set's
        // diameter, not the box. It comes out a ten-thousandth over 3, because the
        // tail corners sit half a thickness off the axis; the sentence rounds.
        assert_eq!(s.corners.len(), 7, "an arrow is one 7-corner contour");
        assert!(diameter(&s).unwrap() > 3.0 && diameter(&s).unwrap() < 3.001);
        assert_eq!(describe(NodeKind::Arrow, None, Some(&s)), "长度 3 的箭头");
        assert_eq!(
            describe(NodeKind::Arrow, Some("位移"), Some(&s)),
            "位移，长度 3"
        );

        // The case the box cannot survive, and the reason this test has a second
        // half: `--inspect` on the `rotate` scene announced a turned arrow as
        // "长度 2.49". The diameter is invariant under the rotation; the box is its
        // projection onto both axes.
        let mut turned = arrow;
        turned.set_transform(Affine::rotate(0.6));
        let s = shape_of(&turned);
        assert!(
            s.bounds.width() < 2.6,
            "the box really is the underestimate here: {:?}",
            s.bounds
        );
        assert_eq!(
            describe(NodeKind::Arrow, Some("位移"), Some(&s)),
            "位移，长度 3"
        );
    }

    /// A number line is measured like a line, not like its box.
    ///
    /// The box includes the ticks, so its *height* is the tick height — a
    /// decoration — and its width is the shaft only until the line is turned.
    /// The diameter of the corner set is the shaft's length either way.
    #[test]
    fn a_number_line_is_described_by_its_length() {
        let params = ShapeParams {
            width: Some(6.0),
            range: Some([-3.0, 3.0]),
            ..Default::default()
        };
        let line = rustim_mobject::shapes::build(ShapeKind::NumberLine, &params);
        let s = shape_of(&line);
        assert_eq!(
            describe(NodeKind::NumberLine, None, Some(&s)),
            "长度 6 的数轴"
        );
        assert_eq!(
            describe(NodeKind::NumberLine, Some("数轴"), Some(&s)),
            "数轴，长度 6"
        );

        let mut turned = line;
        turned.set_transform(Affine::rotate(0.5));
        let s = shape_of(&turned);
        assert!(
            s.bounds.width() < 5.6,
            "the box really is the underestimate here: {:?}",
            s.bounds
        );
        assert_eq!(
            describe(NodeKind::NumberLine, None, Some(&s)),
            "长度 6 的数轴"
        );
    }

    #[test]
    fn roles_do_not_change_without_someone_noticing() {
        // The mapping is a decision, not a default, so it is pinned rather than
        // left to whichever arm a later edit adds first.
        assert_eq!(
            SemanticRole::from_kind(NodeKind::Square),
            SemanticRole::Image
        );
        assert_eq!(
            SemanticRole::from_kind(NodeKind::Unknown),
            SemanticRole::Image
        );
        assert_eq!(
            SemanticRole::from_kind(NodeKind::Group),
            SemanticRole::Group
        );
        assert_eq!(SemanticRole::from_kind(NodeKind::Text), SemanticRole::Label);
        // A line and an arrow are figures like the rest: the kind is what the
        // *description* distinguishes, not the role.
        assert_eq!(SemanticRole::from_kind(NodeKind::Line), SemanticRole::Image);
        assert_eq!(
            SemanticRole::from_kind(NodeKind::Arrow),
            SemanticRole::Image
        );
        assert_eq!(
            SemanticRole::from_kind(NodeKind::NumberLine),
            SemanticRole::Image
        );
        assert_eq!(
            SemanticRole::from_kind(NodeKind::Formula),
            SemanticRole::Math
        );
        // A live region must not be a plain label: on macOS and Windows the
        // announcement text is read from `label` only for roles whose name does
        // not come from `value`, and `Role::Label` is exactly the role where it
        // does.
        assert_ne!(SemanticRole::Status, SemanticRole::Label);
    }

    #[test]
    fn the_live_text_rides_along_with_the_snapshot() {
        let tree = scene();
        let s = build(
            &tree,
            &Viewport::default(),
            Some("a 的正方形，边长 3".into()),
        );
        assert_eq!(s.live.as_deref(), Some("a 的正方形，边长 3"));
        // It survives the JSON round trip the agent-facing `inspect` surface uses.
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<SemanticTree>(&json).unwrap(), s);
    }

    /// The pixel `bounds` are only usable by a host that also knows the view they
    /// were made with, so the view travels with them.
    #[test]
    fn the_semantic_tree_carries_the_camera() {
        let mut tree = scene();
        tree.camera = Camera::new((1.0, -2.0), 2.0);
        let s = build(&tree, &Viewport::default(), None);
        assert_eq!(s.camera.center, [1.0, -2.0]);
        assert_eq!(s.camera.zoom, 2.0);
        assert_eq!(s.camera.frame, [1280, 720]);
        // 8 / 2 units tall, and the frame is 16:9.
        assert!((s.camera.visible[1] - 4.0).abs() < 1e-12);
        assert!((s.camera.visible[0] - 4.0 * 1280.0 / 720.0).abs() < 1e-12);
    }

    /// Moving the camera moves the reported bounds; moving the *object* is the
    /// other test. Both projections read the same transform, and this is the
    /// half the camera introduced.
    #[test]
    fn a_camera_pan_moves_the_reported_screen_bounds() {
        let mut tree = scene();
        let id = tree.root_ids()[0];
        let before = build(&tree, &Viewport::default(), None)
            .get(id)
            .unwrap()
            .bounds
            .unwrap();

        tree.camera.center = (3.0, 0.0);
        let after = build(&tree, &Viewport::default(), None)
            .get(id)
            .unwrap()
            .bounds
            .unwrap();

        // Panning the view right by 3 units moves everything on screen left by
        // 3 * 90 px. The object itself did not change size.
        assert!(
            (before.x0 - after.x0 - 3.0 * 90.0).abs() < 1e-9,
            "{before:?} vs {after:?}"
        );
        assert!((before.width() - after.width()).abs() < 1e-9);
    }

    /// A snapshot written before the camera joined the protocol still reads, and
    /// reads as the view its bounds were actually made with.
    #[test]
    fn a_snapshot_from_before_the_camera_still_reads() {
        let json = format!(r#"{{"root":{},"nodes":[],"live":null}}"#, ROOT.raw());
        let s: SemanticTree = serde_json::from_str(&json).unwrap();
        assert_eq!(s.camera, CameraView::default());
        assert_eq!(s.camera.zoom, 1.0);
        assert_eq!(s.camera.visible, [8.0 * (1280.0 / 720.0), 8.0]);
    }

    /// Text and a formula container have no points but do have a declared local
    /// extent, so they get screen bounds — while keeping the description they
    /// had before the bounds existed.
    ///
    /// The description half is the load-bearing one: routing bounds through
    /// `world_bounds` is fine, but someone extending `world_anchors` instead
    /// would hand the Describe path a rectangle and change 「文本」 into a
    /// measured sentence that no listener asked for. The text should still be
    /// spoken as its own label.
    #[test]
    fn a_text_box_gets_bounds_without_changing_its_description() {
        let mut tree = SceneTree::new();
        let mut m = rustim_mobject::Mobject::new(Vec::new())
            .with_kind(rustim_mobject::kind::NodeKind::Text)
            .with_label("斜边");
        m.local_bounds = Some(rustim_math::Rect::new(-2.0, -0.5, 2.0, 0.5));
        let id = tree.insert(m);

        let s = build(&tree, &Viewport::default(), None);
        let node = s.get(id).unwrap();
        let bounds = node.bounds.expect("a declared box is a bounds");
        // The default viewport is 90 px per world unit. The box is 4 x 1 world
        // units and is centred, so it spans 360 x 90 pixels about the origin.
        assert!((bounds.width() - 4.0 * 90.0).abs() < 1e-9);
        assert!((bounds.height() - 1.0 * 90.0).abs() < 1e-9);
        assert_eq!(node.description, "斜边", "the text speaks as itself");
        assert_eq!(node.name, "斜边");
    }
}
