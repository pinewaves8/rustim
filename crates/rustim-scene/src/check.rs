//! The linter: what a host should hear about a frame *before* showing it.
//!
//! A model composing a scene from data can produce a picture that is
//! syntactically perfect and pedagogically wrong: an object half off the frame,
//! two labels stacked on top of each other, a line of text too small to read,
//! grey-on-black prose, a beat that flashes past. None of those is a refusal —
//! the commands are all legal — so the engine has no place to complain about
//! them, and the host has no way to ask. `check` is that way to ask.
//!
//! The answer is a [`CheckReport`]: a list of [`Finding`]s, each naming one
//! code, one severity, and (where there is one) the node at fault. It is
//! deliberately **not** an error: a scene with findings is still a scene, the
//! host decides what to do, and the frame is unchanged either way. That is the
//! same posture `query` takes — a read, not a mutation — and the same reason
//! both are answered by [`crate::SceneContext::handle`] rather than by the pure
//! tree-level `apply`: measuring an out-of-frame object needs the viewport, and
//! pacing needs the playback state.
//!
//! Two of the seven checks can only be answered from context state, and they
//! live in [`pacing`] rather than in [`check`] for that reason. The five that
//! are pure functions of `(tree, viewport)` are the geometry-and-contrast half,
//! and they are what this module's own unit tests drive directly.
//!
//! ## What each finding means
//!
//! | code | severity | rule |
//! |---|---|---|
//! | `out_of_frame` | error | a top-level object's world box is not inside the visible world box |
//! | `overlap` | warning | two top-level boxes overlap by more than [`OVERLAP_FRACTION`] of the smaller |
//! | `text_too_small` | warning | a text node's line box is under [`MIN_TEXT_PX`] pixels tall |
//! | `low_contrast` | warning | a visible node's paint is under [`MIN_CONTRAST`]:1 against the background |
//! | `empty_text` | info | a text node was created with no glyphs |
//! | `beat_too_fast` | info | a queued beat is shorter than [`MIN_BEAT_SECONDS`] |
//! | `narration_too_fast` | info | two lines are closer than [`READING_CHARS_PER_SECOND`] allows |
//!
//! ## What it cannot see, and does not pretend to
//!
//! The checks are **geometric and local**. The engine has no semantics — it
//! does not know that a label belongs to the square under it or that an arrow
//! points at the right place — so a report is a set of measurements, not a
//! judgement about whether the explanation is correct. Two consequences are
//! worth stating:
//!
//! * **Bounds are world boxes, not ink.** A rotated square's box is larger than
//!   the square (see `CLAUDE.md`'s a11y note), so an `overlap` finding between
//!   two rotated neighbours can fire on boxes that do not touch on screen. That
//!   is the same measurement the accessibility layer reports, and being
//!   consistent with it is worth more than a tighter approximation.
//! * **Only top-level objects are compared for overlap.** Children of a group
//!   are laid out *to* overlap — a template's label sits on its shape — so
//!   walking inside groups would report every template as broken. The group's
//!   own box is what a viewer reads as one object.
//!
//! The thresholds are constants in this module rather than wire parameters.
//! They are the values a 1280×720 3b1b-style frame needs to be legible; a host
//! that wants different ones should filter the report, not configure the
//! engine, because a finding's `detail` states which rule fired and a second
//! threshold table on the wire would be a second answer to the same question.

use rustim_color::Srgb;
use rustim_math::{Point, Rect, Viewport};
use rustim_mobject::{Mobject, NodeId, SceneTree};
use serde::{Deserialize, Serialize};

use crate::command::Target;

/// A text node's line box must be at least this tall, in screen pixels.
///
/// Approximates the smallest type a laptop screen reads comfortably. The input
/// is the declared line box, not ink, so a face with unusual vertical metrics
/// moves the number rather than the rule.
pub const MIN_TEXT_PX: f64 = 18.0;

/// Minimum WCAG 2.1 contrast ratio for large text (18 pt / 14 pt bold).
///
/// The engine's palette lives at 3:1 as a floor: `#64B5F6` on `#121212` is
/// 7.4:1, `#EF5350` is 4.7:1, and both stay above it, so the rule flags a
/// colour a caller invented to be dim rather than the palette the scenes use.
pub const MIN_CONTRAST: f64 = 3.0;

/// Two top-level objects whose boxes overlap by more than this fraction of the
/// smaller box's area are reported.
///
/// Not zero: adjacent shapes legitimately share a boundary, and a 10 px stroke
/// tolerance would flag every welded axis. A fifth of the smaller object buried
/// under another is unreadable, which is what this catches.
pub const OVERLAP_FRACTION: f64 = 0.1;

/// A queued beat shorter than this, in seconds, flashes rather than plays.
pub const MIN_BEAT_SECONDS: f64 = 0.2;

/// Narration is read at no more than this many characters per second.
///
/// Used as `chars / rate` to compare against the gap to the next line. The
/// same figure the caption band's `hold_seconds` is based on, so a line the
/// captions hold long enough to read is a line this check accepts.
pub const READING_CHARS_PER_SECOND: f64 = 8.0;

/// Slack for floating-point comparison of world bounds against the frame; not
/// a tolerance a caller should tune. An axis welded exactly onto the frame edge
/// is *inside* the frame.
const EDGE_EPSILON: f64 = 1e-9;

/// The severity a finding is reported at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The picture is wrong for a viewer, not merely off.
    Error,
    /// Legible but degraded; worth fixing before an explanation.
    Warning,
    /// Probably intentional, reported so a host can see it.
    Info,
}

impl Severity {
    /// The wire spelling, matching the serde rename.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

/// What one finding is about.
///
/// The code is the machine-readable discriminant (and the serde tag a host
/// should switch on); the `detail` sentence says what *this* finding measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckCode {
    OutOfFrame,
    Overlap,
    TextTooSmall,
    LowContrast,
    EmptyText,
    BeatTooFast,
    NarrationTooFast,
}

impl CheckCode {
    /// Every code, in the order the report groups them.
    pub const ALL: [Self; 7] = [
        Self::OutOfFrame,
        Self::Overlap,
        Self::TextTooSmall,
        Self::LowContrast,
        Self::EmptyText,
        Self::BeatTooFast,
        Self::NarrationTooFast,
    ];

    /// The wire spelling, matching the serde rename.
    pub const fn name(self) -> &'static str {
        match self {
            Self::OutOfFrame => "out_of_frame",
            Self::Overlap => "overlap",
            Self::TextTooSmall => "text_too_small",
            Self::LowContrast => "low_contrast",
            Self::EmptyText => "empty_text",
            Self::BeatTooFast => "beat_too_fast",
            Self::NarrationTooFast => "narration_too_fast",
        }
    }

    /// The severity this code is reported at.
    ///
    /// A function of the code rather than a field a caller supplies, so every
    /// `out_of_frame` in every report is the same severity. The field still
    /// travels on the wire (`Finding::severity`), because a model reading a
    /// finding should not have to hold the table.
    pub const fn severity(self) -> Severity {
        match self {
            // An object a viewer cannot see is the picture being wrong.
            Self::OutOfFrame => Severity::Error,
            // Legible but degraded.
            Self::Overlap | Self::TextTooSmall | Self::LowContrast => Severity::Warning,
            // Geometry does not measure whether an explanation is right, and a
            // sub-second beat is a legitimate flourish — report, do not judge.
            Self::EmptyText | Self::BeatTooFast | Self::NarrationTooFast => Severity::Info,
        }
    }
}

/// One thing the linter found.
///
/// Field order is the serde order on the wire: `code`, `severity`, `target`,
/// `label`, `other`, `detail`. A finding with no node (`beat_too_fast`,
/// `narration_too_fast`) omits `target` and `label` rather than writing nulls,
/// the same rule every optional command parameter follows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub code: CheckCode,
    pub severity: Severity,
    /// The node the finding is about, as an id — a finding always names ids,
    /// exactly as a `StateDiff` does, whatever spelling addressed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    /// The node's own label, copied for the reader's convenience. Absent when
    /// the node has none; never resolved from `target` by the consumer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The other node in a two-object finding (`overlap` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<Target>,
    /// What this finding measured, in one sentence.
    pub detail: String,
}

impl Finding {
    /// A node-less finding (pacing checks).
    #[must_use]
    pub fn new(code: CheckCode, detail: String) -> Self {
        Self {
            code,
            severity: code.severity(),
            target: None,
            label: None,
            other: None,
            detail,
        }
    }

    /// The same finding, attached to `id` in `tree`.
    #[must_use]
    pub fn with_target(mut self, tree: &SceneTree, id: NodeId) -> Self {
        self.target = Some(Target::Id(id));
        self.label = tree.get(id).and_then(|m| m.label.clone());
        self
    }
}

/// The answer to `{"action":"check"}`.
///
/// An empty `findings` list is the clean answer, and it is spelled by the
/// field's presence with an empty array — not by an absent field — so a host
/// can tell "checked and clean" from "did not ask".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CheckReport {
    pub findings: Vec<Finding>,
}

impl CheckReport {
    /// Whether the scene passed every check.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Check the current frame against the rules that need no playback state.
///
/// `viewport` is the host's window: it decides what "in frame" means and how
/// many pixels a text line box covers. The same tree checked through two
/// viewports can produce different findings, and that is correct — a scene that
/// fits a 1280×720 frame may not fit a 640×360 one.
///
/// Findings are ordered by check (`out_of_frame`, then `overlap`, then the
/// per-node text and contrast checks in draw order), never sorted by severity:
/// a stable order is what lets a host diff two reports.
#[must_use]
pub fn check(tree: &SceneTree, viewport: &Viewport) -> CheckReport {
    let mut findings = Vec::new();
    let frame = visible_world(tree, viewport);
    let pixels_per_unit = viewport.pixels_per_unit(&tree.camera);

    // Top-level objects: out-of-frame and mutual overlap. Children are laid
    // out *to* overlap inside a group, so only the root level is compared.
    let mut roots: Vec<(NodeId, Rect)> = Vec::new();
    for &id in tree.root_ids() {
        let Some(mobject) = tree.get(id) else {
            continue;
        };
        if mobject.opacity <= 0.0 {
            continue;
        }
        let Some(bounds) = subtree_bounds(tree, id) else {
            continue;
        };
        if let Some(frame) = frame
            && !inside(bounds, frame)
        {
            findings.push(
                Finding::new(
                    CheckCode::OutOfFrame,
                    format!(
                        "world box ({:.2}, {:.2})..({:.2}, {:.2}) is not inside the visible frame \
                         ({:.2}, {:.2})..({:.2}, {:.2})",
                        bounds.x0,
                        bounds.y0,
                        bounds.x1,
                        bounds.y1,
                        frame.x0,
                        frame.y0,
                        frame.x1,
                        frame.y1
                    ),
                )
                .with_target(tree, id),
            );
        }
        roots.push((id, bounds));
    }

    for (index, (a_id, a)) in roots.iter().enumerate() {
        for (b_id, b) in &roots[index + 1..] {
            if let Some(fraction) = overlap_fraction(*a, *b)
                && fraction > OVERLAP_FRACTION
            {
                let mut finding = Finding::new(
                    CheckCode::Overlap,
                    format!(
                        "boxes overlap by {:.0}% of the smaller object's area; the limit is {:.0}%",
                        fraction * 100.0,
                        OVERLAP_FRACTION * 100.0
                    ),
                )
                .with_target(tree, *a_id);
                finding.other = Some(Target::Id(*b_id));
                findings.push(finding);
            }
        }
    }

    // Per-node checks, in draw order so a report is stable across runs.
    for id in tree.draw_order() {
        let Some(mobject) = tree.get(id) else {
            continue;
        };
        if mobject.opacity <= 0.0 {
            continue;
        }

        if let Some(block) = &mobject.text {
            if block.is_empty() {
                findings.push(
                    Finding::new(
                        CheckCode::EmptyText,
                        "text has no glyphs; it will draw nothing".to_owned(),
                    )
                    .with_target(tree, id),
                );
            } else if pixels_per_unit.is_finite() && pixels_per_unit > 0.0 {
                let pixels = line_box_pixels(tree, mobject, pixels_per_unit);
                if pixels.is_finite() && pixels < MIN_TEXT_PX {
                    findings.push(
                        Finding::new(
                            CheckCode::TextTooSmall,
                            format!(
                                "line box is {pixels:.1} px tall at the current camera; the \
                                 minimum is {MIN_TEXT_PX:.0} px"
                            ),
                        )
                        .with_target(tree, id),
                    );
                }
            }
        }

        if let Some((paint, alpha)) = paint_of(mobject)
            && alpha >= 0.5
        {
            let ratio = contrast_ratio(paint, rustim_color::BG);
            if ratio < MIN_CONTRAST {
                findings.push(
                    Finding::new(
                        CheckCode::LowContrast,
                        format!(
                            "contrast against the background is {ratio:.2}:1; the minimum is \
                             {MIN_CONTRAST:.1}:1"
                        ),
                    )
                    .with_target(tree, id),
                );
            }
        }
    }

    CheckReport { findings }
}

/// Check the timeline: beat durations and narration pacing.
///
/// Split from [`check`] because these are not properties of the frame — they
/// are properties of the beats and narration lines still queued, which only
/// [`crate::SceneContext`] holds.
///
/// `beats` is one nominal duration per queued beat, in the order they will
/// play ([`rustim_anim::Sequence::durations`]); `narration` is every pending
/// line as `(due time, text)`, in any order. Findings are node-less: the
/// timeline has no addressable object.
#[must_use]
pub fn pacing(beats: &[f64], narration: &[(f64, String)]) -> Vec<Finding> {
    let mut findings = Vec::new();

    for (index, duration) in beats.iter().enumerate() {
        if duration.is_finite() && *duration > 0.0 && *duration < MIN_BEAT_SECONDS {
            findings.push(Finding::new(
                CheckCode::BeatTooFast,
                format!(
                    "queued beat {} lasts {duration:.2} s; under {MIN_BEAT_SECONDS:.1} s reads \
                     as a cut rather than a movement",
                    index + 1
                ),
            ));
        }
    }

    let mut lines: Vec<&(f64, String)> = narration.iter().collect();
    lines.sort_by(|a, b| a.0.total_cmp(&b.0));
    for pair in lines.windows(2) {
        let (due, text) = pair[0];
        let gap = pair[1].0 - due;
        let characters = text.chars().count() as f64;
        if !gap.is_finite() {
            continue;
        }
        let needed = characters / READING_CHARS_PER_SECOND;
        if gap < needed {
            findings.push(Finding::new(
                CheckCode::NarrationTooFast,
                format!(
                    "the line due at {due:.2} s needs {needed:.1} s to read ({characters:.0} \
                     characters at {READING_CHARS_PER_SECOND:.0}/s), but the next is due \
                     {gap:.2} s later"
                ),
            ));
        }
    }

    findings
}

/// The visible world as an axis-aligned rectangle, `None` for a degenerate
/// viewport or camera.
///
/// The world is y-up and the frame is symmetric about the camera's centre, so
/// the rect's corners are just `centre ± half extent`; the `Rect` type's field
/// order (`x0, y0, x1, y1`) says nothing about screen orientation.
fn visible_world(tree: &SceneTree, viewport: &Viewport) -> Option<Rect> {
    let height = f64::from(viewport.frame_height);
    let width = f64::from(viewport.frame_width);
    if !height.is_finite() || height <= 0.0 || !width.is_finite() || width <= 0.0 {
        return None;
    }
    let world_height = tree.camera.visible_height();
    if !world_height.is_finite() || world_height <= 0.0 {
        return None;
    }
    let world_width = world_height * width / height;
    let (cx, cy) = tree.camera.center;
    if !cx.is_finite() || !cy.is_finite() {
        return None;
    }
    let (half_w, half_h) = (world_width / 2.0, world_height / 2.0);
    Some(Rect::new(
        cx - half_w,
        cy - half_h,
        cx + half_w,
        cy + half_h,
    ))
}

/// The union of a subtree's own measured boxes, children included.
///
/// Iterative rather than recursive: the tree depth is whatever a model built,
/// and a report must not be able to overflow the stack on a hostile scene.
/// Nodes with no geometry contribute nothing; an empty subtree has no bounds.
fn subtree_bounds(tree: &SceneTree, root: NodeId) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if let Some(rect) = tree.world_bounds(id) {
            bounds = Some(match bounds {
                None => rect,
                Some(existing) => union(existing, rect),
            });
        }
        stack.extend(tree.child_ids(id).iter().copied());
    }
    bounds
}

/// Kurbo's `Rect` has no `union` (see `CLAUDE.md`'s "Hard-won facts"), so the
/// operator two rectangles need is spelled here once.
fn union(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.x0.min(b.x0),
        a.y0.min(b.y0),
        a.x1.max(b.x1),
        a.y1.max(b.y1),
    )
}

/// Whether `rect` lies inside `frame`, with [`EDGE_EPSILON`] of slack.
fn inside(rect: Rect, frame: Rect) -> bool {
    rect.x0 >= frame.x0 - EDGE_EPSILON
        && rect.y0 >= frame.y0 - EDGE_EPSILON
        && rect.x1 <= frame.x1 + EDGE_EPSILON
        && rect.y1 <= frame.y1 + EDGE_EPSILON
}

/// The intersection area as a fraction of the smaller rectangle's area, `None`
/// when the boxes miss each other or the smaller has no area.
///
/// Zero-area boxes are excluded on purpose: a horizontal line crossing a square
/// has zero intersection area under any sane rule, and calling that an overlap
/// would flag every plot drawn across an axis.
fn overlap_fraction(a: Rect, b: Rect) -> Option<f64> {
    let width = a.x1.min(b.x1) - a.x0.max(b.x0);
    let height = a.y1.min(b.y1) - a.y0.max(b.y0);
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let smaller = (a.width() * a.height()).min(b.width() * b.height());
    if !smaller.is_finite() || smaller <= 0.0 {
        return None;
    }
    Some(width * height / smaller)
}

/// The text line height in screen pixels under the current camera.
///
/// The line box is declared in the mobject's local space with y up, so the
/// height is the length of the transformed unit-y segment times
/// pixels-per-world-unit. That is correct for a scaled text node and for a
/// rotated one, where reading the box's y extent would overstate it.
fn line_box_pixels(tree: &SceneTree, mobject: &Mobject, pixels_per_unit: f64) -> f64 {
    let Some(block) = &mobject.text else {
        return f64::NAN;
    };
    let height = f64::from(block.height);
    if !height.is_finite() || height <= 0.0 {
        return f64::NAN;
    }
    let transform = tree.parent_transform(mobject.id) * mobject.transform;
    let bottom = transform * Point::new(0.0, 0.0);
    let top = transform * Point::new(0.0, height);
    (top - bottom).hypot() * pixels_per_unit
}

/// The colour a node paints and the alpha it paints it with, for the contrast
/// check.
///
/// The fill is judged when the node draws one, because it is the larger area;
/// a stroke is judged only when there is no fill to judge. `None` for a node
/// that paints nothing (a group, or an invisible style).
///
/// "Draws a fill" mirrors the draw walk's own test: some points, or wire text
/// that the context will expand into glyph runs.
fn paint_of(mobject: &Mobject) -> Option<(Srgb, f64)> {
    let alpha = mobject.opacity;
    let style = &mobject.style;
    let fill_alpha = alpha * style.fill_opacity;
    if fill_alpha > 0.0 && (mobject.points.len() >= 2 || mobject.text.is_some()) {
        return Some((style.fill, fill_alpha));
    }
    let stroke_alpha = alpha * style.stroke_opacity;
    let strokes = mobject.points.len() >= 2
        && style.stroke_width > 0.0
        && style.stroke_width.is_finite()
        && stroke_alpha > 0.0;
    strokes.then_some((style.stroke, stroke_alpha))
}

/// WCAG 2.1 contrast ratio between two opaque colours.
///
/// `L = 0.2126 R + 0.7152 G + 0.0722 B` over **linearised** components, and
/// `(L_light + 0.05) / (L_dark + 0.05)` — the standard formula, not a
/// perceptual shortcut. The workspace's colours arrive gamma-encoded, so each
/// channel is linearised first; skipping that step understates a dark
/// background's contrast badly enough to pass grey-on-black.
#[must_use]
pub fn contrast_ratio(a: Srgb, b: Srgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (light, dark) = if la >= lb { (la, lb) } else { (lb, la) };
    (light + 0.05) / (dark + 0.05)
}

/// The WCAG relative luminance of an opaque sRGB colour.
fn luminance(color: Srgb) -> f64 {
    let [r, g, b, _] = color.components;
    let channel = |c: f32| -> f64 {
        let c = f64::from(c);
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_color::{BLUE, RED};
    use rustim_math::Affine2;
    use rustim_mobject::{NodeKind, TextBlock, shapes};

    fn viewport() -> Viewport {
        Viewport {
            frame_width: 1280,
            frame_height: 720,
        }
    }

    #[test]
    fn every_code_names_itself_and_has_a_severity() {
        for code in CheckCode::ALL {
            // `name` must match serde's rename, or a host switching on one
            // will not match the other.
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{}\"", code.name()));
            let severity = serde_json::to_string(&code.severity()).unwrap();
            assert_eq!(severity, format!("\"{}\"", code.severity().name()));
        }
    }

    #[test]
    fn the_palette_passes_the_contrast_floor() {
        // The rule must flag a dim colour the caller invented, not the
        // palette the reference scenes already use.
        assert!(contrast_ratio(rustim_color::FG, rustim_color::BG) > 12.0);
        assert!(contrast_ratio(BLUE, rustim_color::BG) >= MIN_CONTRAST);
        assert!(contrast_ratio(RED, rustim_color::BG) >= MIN_CONTRAST);
        assert!(
            contrast_ratio(BLUE, rustim_color::BG)
                < contrast_ratio(rustim_color::FG, rustim_color::BG)
        );
    }

    #[test]
    fn contrast_is_symmetric_and_one_for_the_same_colour() {
        let ratio = contrast_ratio(BLUE, rustim_color::BG);
        assert!((ratio - contrast_ratio(rustim_color::BG, BLUE)).abs() < 1e-12);
        assert!((contrast_ratio(BLUE, BLUE) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_scene_inside_the_frame_is_clean() {
        let mut tree = SceneTree::new();
        let a = tree.insert(Mobject::new(shapes::square(1.0, 1.0)));
        let _ = a;
        let report = check(&tree, &viewport());
        assert!(report.is_clean(), "{:?}", report.findings);
    }

    #[test]
    fn an_object_outside_the_visible_frame_is_an_error() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(shapes::square(1.0, 1.0)));
        tree.get_mut(id).unwrap().transform = Affine2::translate((100.0, 0.0));
        let report = check(&tree, &viewport());
        let finding = report
            .findings
            .iter()
            .find(|f| f.code == CheckCode::OutOfFrame)
            .expect("the square is far off frame");
        assert_eq!(finding.severity, Severity::Error);
        assert_eq!(finding.target, Some(Target::Id(id)));
    }

    #[test]
    fn an_object_crossing_the_frame_edge_is_reported() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(shapes::square(2.0, 2.0)));
        // The default frame is 8 world units tall and 14.22 wide, so x = 6.5
        // puts half the 2-wide square past the right edge.
        tree.get_mut(id).unwrap().transform = Affine2::translate((6.5, 0.0));
        let report = check(&tree, &viewport());
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == CheckCode::OutOfFrame)
        );
    }

    #[test]
    fn a_zoom_that_brings_an_object_into_view_clears_the_finding() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(shapes::square(2.0, 2.0)));
        tree.get_mut(id).unwrap().transform = Affine2::translate((6.5, 0.0));
        assert!(!check(&tree, &viewport()).is_clean());
        // The camera moved onto the object rather than the object moving.
        tree.camera.center = (6.5, 0.0);
        assert!(check(&tree, &viewport()).is_clean());
    }

    #[test]
    fn two_overlapping_objects_are_reported_with_each_other() {
        let mut tree = SceneTree::new();
        let a = tree.insert(Mobject::new(shapes::square(2.0, 2.0)));
        let b = tree.insert(Mobject::new(shapes::square(2.0, 2.0)));
        tree.get_mut(a).unwrap().transform = Affine2::translate((-0.2, 0.0));
        tree.get_mut(b).unwrap().transform = Affine2::translate((0.2, 0.0));
        let report = check(&tree, &viewport());
        let finding = report
            .findings
            .iter()
            .find(|f| f.code == CheckCode::Overlap)
            .expect("the squares overlap by most of their area");
        assert_eq!(finding.target, Some(Target::Id(a)));
        assert_eq!(finding.other, Some(Target::Id(b)));
    }

    #[test]
    fn neighbours_sharing_an_edge_do_not_overlap() {
        let mut tree = SceneTree::new();
        let a = tree.insert(Mobject::new(shapes::square(2.0, 2.0)));
        let b = tree.insert(Mobject::new(shapes::square(2.0, 2.0)));
        tree.get_mut(a).unwrap().transform = Affine2::translate((-1.0, 0.0));
        tree.get_mut(b).unwrap().transform = Affine2::translate((1.0, 0.0));
        assert!(
            check(&tree, &viewport())
                .findings
                .iter()
                .all(|f| f.code != CheckCode::Overlap)
        );
    }

    #[test]
    fn a_child_overlapping_its_parent_is_not_reported() {
        // A template's label sits on its shape; only top-level objects are
        // compared, exactly because of that.
        let mut tree = SceneTree::new();
        let group = tree.insert(Mobject::new(Vec::new()).with_kind(NodeKind::Group));
        let child = tree.insert(Mobject::new(shapes::square(1.0, 1.0)));
        tree.reparent(child, group);
        assert!(
            check(&tree, &viewport())
                .findings
                .iter()
                .all(|f| f.code != CheckCode::Overlap)
        );
    }

    #[test]
    fn an_empty_text_block_is_reported_and_a_nonempty_one_is_not() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(Vec::new()));
        tree.get_mut(id).unwrap().text = Some(Box::new(TextBlock::new(0.0, 0.0, Vec::new())));
        let report = check(&tree, &viewport());
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == CheckCode::EmptyText && f.target == Some(Target::Id(id)))
        );
    }

    #[test]
    fn a_dim_colour_is_reported_and_the_palette_is_not() {
        let mut tree = SceneTree::new();
        let dim = tree.insert(Mobject::new(shapes::square(1.0, 1.0)));
        tree.get_mut(dim).unwrap().style.fill = Srgb::from_rgba8(0x22, 0x22, 0x22, 0xFF);
        let bright = tree.insert(Mobject::new(shapes::circle(1.0)));
        tree.get_mut(bright).unwrap().transform = Affine2::translate((3.0, 0.0));
        tree.get_mut(bright).unwrap().style.fill = BLUE;
        let report = check(&tree, &viewport());
        let flagged: Vec<_> = report
            .findings
            .iter()
            .filter(|f| f.code == CheckCode::LowContrast)
            .collect();
        assert_eq!(flagged.len(), 1, "{:?}", report.findings);
        assert_eq!(flagged[0].target, Some(Target::Id(dim)));
    }

    #[test]
    fn a_nearly_transparent_object_paints_too_little_to_be_judged() {
        // Under half opacity the composite colour is the background plus a
        // little; calling that "low contrast" would flag every fade-in.
        let mut tree = SceneTree::new();
        let faint = tree.insert(Mobject::new(shapes::square(1.0, 1.0)));
        tree.get_mut(faint).unwrap().opacity = 0.2;
        tree.get_mut(faint).unwrap().style.fill = rustim_color::FG;
        assert!(
            check(&tree, &viewport())
                .findings
                .iter()
                .all(|f| f.code != CheckCode::LowContrast)
        );
    }

    #[test]
    fn a_faded_out_object_reports_nothing_at_all() {
        let mut tree = SceneTree::new();
        let id = tree.insert(Mobject::new(shapes::square(1.0, 1.0)));
        tree.get_mut(id).unwrap().opacity = 0.0;
        tree.get_mut(id).unwrap().transform = Affine2::translate((100.0, 0.0));
        assert!(check(&tree, &viewport()).is_clean());
    }

    #[test]
    fn the_same_report_is_produced_twice() {
        // A host diffs two reports; ordering must not move between runs.
        let mut tree = SceneTree::new();
        for i in 0..6 {
            let id = tree.insert(Mobject::new(shapes::square(2.0, 2.0)));
            tree.get_mut(id).unwrap().transform = Affine2::translate((-0.2 + f64::from(i), 0.0));
        }
        let first = check(&tree, &viewport());
        let second = check(&tree, &viewport());
        assert_eq!(first, second);
        assert!(!first.is_clean());
    }

    #[test]
    fn beat_pacing_flags_a_flash_and_accepts_a_readable_beat() {
        let quick = pacing(&[0.05], &[]);
        assert_eq!(quick.len(), 1);
        assert_eq!(quick[0].code, CheckCode::BeatTooFast);
        assert!(quick[0].target.is_none());
        assert!(pacing(&[0.4], &[]).is_empty());
        // A zero-duration beat is an instant snap, which is legal, not a flash.
        assert!(pacing(&[0.0], &[]).is_empty());
    }

    #[test]
    fn narration_pacing_compares_the_gap_to_the_reading_time() {
        // Sixteen characters need two seconds at 8/s.
        let lines = vec![
            (0.0, "十六个汉字到这里刚好凑满第三句了".to_owned()),
            (1.0, "下一句".to_owned()),
            (5.0, "第三句".to_owned()),
        ];
        let findings = pacing(&[], &lines);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].code, CheckCode::NarrationTooFast);
        assert!(findings[0].detail.contains("16"));
    }

    #[test]
    fn narration_pacing_sorts_before_comparing() {
        // The same lines in a different order are the same timeline.
        let lines = vec![
            (5.0, "第三句".to_owned()),
            (0.0, "十六个汉字到这里刚好凑满第三句了".to_owned()),
            (1.0, "下一句".to_owned()),
        ];
        assert_eq!(pacing(&[], &lines).len(), 1);
    }

    #[test]
    fn a_clean_report_is_an_empty_list_not_an_absent_field() {
        let json = serde_json::to_string(&CheckReport::default()).unwrap();
        assert_eq!(json, r#"{"findings":[]}"#);
    }

    #[test]
    fn a_pacing_finding_omits_the_node_fields() {
        let json = serde_json::to_string(&pacing(&[0.05], &[])[0]).unwrap();
        assert!(!json.contains("target"));
        assert!(!json.contains("label"));
        assert!(!json.contains("other"));
        assert!(json.contains("\"code\":\"beat_too_fast\""));
        assert!(json.contains("\"severity\":\"info\""));
    }
}
