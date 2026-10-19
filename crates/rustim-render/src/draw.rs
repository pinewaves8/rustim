//! The backend-neutral draw list.
//!
//! A `DrawList` is what the scene layer produces and every backend consumes. It
//! is intentionally close to vello's own command set, because both backends
//! implement exactly that set — but it is *ours*, so the scene layer does not
//! depend on vello and a future third backend does not have to pretend to be
//! vello.

use rustim_color::Srgb;
use rustim_math::stroke::StrokeStyle;
use rustim_math::{Affine, BezPath, Rect, Shape, TextGlyph};

/// One drawing command.
///
/// Commands are stateful in the same way vello's are: [`DrawOp::SetPaint`] and
/// [`DrawOp::SetTransform`] apply to every subsequent path until changed. That
/// mirrors both backends and avoids threading a paint through every op.
///
/// `PartialEq` is not decoration: [`DrawList::damage_since`] compares two
/// frames op by op, and an op that compares unequal would mark its pixels dirty
/// on every frame forever. Every payload type here is comparable, and for the
/// expensive one — `FontData` — equality is a handle comparison rather than a
/// byte comparison (the blob carries a unique id).
#[derive(Debug, Clone, PartialEq)]
pub enum DrawOp {
    /// Set the colour used by subsequent fills and strokes.
    SetPaint(Srgb),
    /// Set the transform applied to subsequent geometry.
    SetTransform(Affine),
    FillPath(BezPath),
    /// Stroke `path`, with a width and the joins, caps and dashes that go with
    /// it. A stroke whose width **varies along the path** is not expressible
    /// here — the stroker takes one width — and is drawn as a filled contour
    /// instead; see `rustim_mobject::stroke_widths`.
    StrokePath {
        path: BezPath,
        stroke: StrokeStyle,
    },
    /// Clip subsequent drawing to `path`, until the matching [`DrawOp::PopClip`].
    PushClip(BezPath),
    /// End the most recent [`DrawOp::PushClip`].
    ///
    /// Distinct from [`DrawOp::PopLayer`] because vello keeps two stacks: a clip
    /// stack (`push_clip_path` / `pop_clip`) and a layer stack (`push_layer` /
    /// `pop_layer`). `pop_layer` on a clip — or vice versa — underflows and
    /// aborts the process rather than failing softly, so the two must not be
    /// conflated.
    PopClip,
    /// Begin a transparency group, so overlapping children composite as a unit.
    PushLayer,
    /// End the most recent [`DrawOp::PushLayer`].
    PopLayer,
    /// Draw a run of glyphs with one font, at one size, in the current paint.
    ///
    /// Carries glyph *indices*, not characters: shaping — which character
    /// becomes which glyph, where the line breaks, which face has the glyph at
    /// all — happens in `rustim-text`, before a draw list exists. A draw list is
    /// a replay of finished geometry, and a shaper is not something a replay can
    /// run.
    ///
    /// The font is a handle to the bytes plus the index of the face inside them
    /// (`peniko::FontData`, which is also what parley hands out, so the shaped
    /// run passes straight through). It is *not* the glyph outlines: a run stays
    /// a run because loading and rasterising glyphs is the backend's job, and
    /// doing it here would make one frame's worth of a drawing cost a font parse.
    GlyphRun {
        font: peniko::FontData,
        /// Pixels per em.
        size: f32,
        glyphs: Vec<TextGlyph>,
    },
}

/// A complete frame's worth of drawing, ready to replay.
#[derive(Debug, Clone, Default)]
pub struct DrawList {
    pub ops: Vec<DrawOp>,
    /// Cleared before the ops run. `None` leaves the target untouched, which is
    /// what the "nothing changed, reuse last frame" path in §4.3 needs.
    pub background: Option<Srgb>,
    pub width: u16,
    pub height: u16,
}

impl DrawList {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            ops: Vec::new(),
            background: None,
            width,
            height,
        }
    }

    /// Drop the ops but keep the allocated capacity.
    ///
    /// The frame loop reuses one `DrawList` for the lifetime of the process; the
    /// tutorial's chapter-2 example builds a fresh mobject every frame, and that
    /// per-frame allocation is exactly what §4.3 tells us to avoid.
    pub fn clear(&mut self, width: u16, height: u16) {
        self.ops.clear();
        self.background = None;
        self.width = width;
        self.height = height;
    }

    pub fn push(&mut self, op: DrawOp) {
        self.ops.push(op);
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty() && self.background.is_none()
    }

    /// How much of the frame differs from `old`.
    ///
    /// This is the input to incremental rendering: a backend can rasterise only
    /// the returned rectangle and keep the pixels outside it, which is what
    /// makes an idle scene and a one-property change cheap. The answer is
    /// deliberately **conservative** — a region too large repaints pixels that
    /// did not change, which is invisible; a region too small leaves stale
    /// pixels, which is a wrong picture. Every uncertainty therefore resolves to
    /// [`Damage::Full`].
    ///
    /// The comparison is positional, op for op, with the state that each op is
    /// drawn in. That is sound because the scene's draw walk emits its ops in a
    /// fixed order for a given tree: a property change rewrites the same slots,
    /// and a node added at the end appends. A change that shifts the whole tail
    /// (an object inserted first) damages the union of every shifted op's
    /// bounds — larger than necessary, still correct.
    ///
    /// Three cases return [`Damage::Full`] regardless of the ops: a size change,
    /// a change of background (or a transparent one, which cannot be repainted
    /// from nothing), and any clip or layer op — those two stacks are
    /// positional state, and a region render starts a fresh context in which a
    /// clip that opened before the region would be missing.
    pub fn damage_since(&self, old: &DrawList) -> Damage {
        if (self.width, self.height) != (old.width, old.height) {
            return Damage::Full;
        }
        if self.background == old.background && self.ops == old.ops {
            return Damage::None;
        }
        // A region is repainted by clearing it to the background and replaying
        // the scene into it, so there must be a background to clear to.
        if self.background.is_none()
            || old.background.is_none()
            || self.background != old.background
        {
            return Damage::Full;
        }
        if self.ops.iter().chain(old.ops.iter()).any(|op| {
            matches!(
                op,
                DrawOp::PushClip(_) | DrawOp::PopClip | DrawOp::PushLayer | DrawOp::PopLayer
            )
        }) {
            return Damage::Full;
        }

        let mut bounds: Option<Rect> = None;
        let mut old_state = OpState::default();
        let mut new_state = OpState::default();
        for i in 0..self.ops.len().max(old.ops.len()) {
            let before = old.ops.get(i);
            let after = self.ops.get(i);
            let unchanged = match (before, after) {
                (Some(a), Some(b)) => a == b && old_state == new_state,
                (None, None) => true,
                _ => false,
            };
            if !unchanged {
                if let Some(op) = before {
                    add_bounds(&mut bounds, op, old_state.transform);
                }
                if let Some(op) = after {
                    add_bounds(&mut bounds, op, new_state.transform);
                }
            }
            if let Some(op) = before {
                old_state.apply(op);
            }
            if let Some(op) = after {
                new_state.apply(op);
            }
        }

        match bounds {
            None => Damage::None,
            // A non-finite bound means a path or glyph position that no
            // rectangle can describe; repaint everything rather than guess.
            Some(r) if !r.is_finite() => Damage::Full,
            Some(r) => PixelRect::from_bounds(r, self.width, self.height)
                .map_or(Damage::None, Damage::Region),
        }
    }
}

/// The paint and transform in force at one point in a draw list.
///
/// Equality of two of these is what makes an *unchanged* geometry op with a
/// *changed* state dirty: the path is the same, but the pixels it covers are
/// not.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct OpState {
    transform: Affine,
    paint: Option<Srgb>,
}

impl OpState {
    fn apply(&mut self, op: &DrawOp) {
        match op {
            DrawOp::SetTransform(t) => self.transform = *t,
            DrawOp::SetPaint(p) => self.paint = Some(*p),
            _ => {}
        }
    }
}

fn add_bounds(acc: &mut Option<Rect>, op: &DrawOp, transform: Affine) {
    let Some(b) = op_bounds(op, transform) else {
        return;
    };
    *acc = Some(match *acc {
        Some(existing) => existing.union(b),
        None => b,
    });
}

/// The device-space box an op can touch, before pixel rounding.
///
/// Conservative on purpose, and by construction:
///
/// * a path is measured by its transformed control-point box, which contains
///   the curve;
/// * a stroke adds half its width on every side, scaled by the largest column
///   norm of the transform (an upper bound on the largest singular value, so it
///   holds under rotation and non-uniform scale alike);
/// * a glyph is measured by a box around its position rather than by its ink —
///   the ink belongs to the backend and reading it here would mean parsing a
///   font per frame. One em left and below, two to the right, covers every
///   advance this engine shapes.
///
/// `None` for ops that paint nothing (paint, transform, clip, layer).
fn op_bounds(op: &DrawOp, transform: Affine) -> Option<Rect> {
    match op {
        DrawOp::FillPath(path) => Some(transform.transform_rect_bbox(path.bounding_box())),
        DrawOp::StrokePath { path, stroke } => {
            let b = transform.transform_rect_bbox(path.bounding_box());
            let pad = stroke.width.abs() * 0.5 * max_scale(transform);
            Some(Rect::new(b.x0 - pad, b.y0 - pad, b.x1 + pad, b.y1 + pad))
        }
        DrawOp::GlyphRun { size, glyphs, .. } => {
            let s = f64::from(*size);
            if !s.is_finite() {
                return None;
            }
            let mut union: Option<Rect> = None;
            for g in glyphs {
                let (x, y) = (f64::from(g.x), f64::from(g.y));
                let one = Rect::new(x - s, y - s, x + 2.0 * s, y + s);
                let b = transform.transform_rect_bbox(one);
                union = Some(match union {
                    Some(existing) => existing.union(b),
                    None => b,
                });
            }
            union
        }
        DrawOp::SetPaint(_)
        | DrawOp::SetTransform(_)
        | DrawOp::PushClip(_)
        | DrawOp::PopClip
        | DrawOp::PushLayer
        | DrawOp::PopLayer => None,
    }
}

/// An upper bound on the largest singular value of `t`.
///
/// The two column norms; the largest singular value of a 2x2 matrix is bounded
/// by the Frobenius norm, and `max` is tighter than that and still an upper
/// bound on either column's image length.
fn max_scale(t: Affine) -> f64 {
    let [a, b, c, d, _, _] = t.as_coeffs();
    (a * a + b * b).sqrt().max((c * c + d * d).sqrt())
}

/// How much of the frame changed between two draw lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Damage {
    /// Nothing visible changed: the previously rendered pixels are still right.
    None,
    /// Everything may have changed.
    Full,
    /// Only this rectangle changed.
    Region(PixelRect),
}

impl Damage {
    /// How many pixels this damage covers in a `width x height` frame.
    ///
    /// The number the incremental path exists to shrink; tests assert on it
    /// because it is the quantity that actually falls, while a frame time
    /// depends on the machine.
    pub fn pixels(&self, width: u16, height: u16) -> u64 {
        match self {
            Damage::None => 0,
            Damage::Full => u64::from(width) * u64::from(height),
            Damage::Region(r) => u64::from(r.width()) * u64::from(r.height()),
        }
    }
}

/// A half-open pixel rectangle, `x0..x1` by `y0..y1`, y down.
///
/// Half-open so that width is `x1 - x0` without an off-by-one, and `u16`
/// because a frame is at most 65535 pixels on a side — the same type vello uses
/// for its targets. `x0 <= x1` and `y0 <= y1` are invariants of every
/// constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x0: u16,
    pub y0: u16,
    pub x1: u16,
    pub y1: u16,
}

impl PixelRect {
    pub fn new(x0: u16, y0: u16, x1: u16, y1: u16) -> Self {
        debug_assert!(x0 <= x1 && y0 <= y1);
        Self { x0, y0, x1, y1 }
    }

    pub fn width(&self) -> u16 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> u16 {
        self.y1 - self.y0
    }

    pub fn is_empty(&self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }

    /// The whole frame.
    pub fn frame(width: u16, height: u16) -> Self {
        Self::new(0, 0, width, height)
    }

    /// Round `bounds` outward to whole pixels, grow by one pixel on each side
    /// for antialiasing, and clip to the frame.
    ///
    /// The extra pixel is not for the geometric box — coverage is zero on its
    /// boundary — but for the pixel that a half-covered edge belongs to. It
    /// costs one row and one column and removes a class of off-by-one bugs that
    /// would show up as a one-pixel seam around a moved object.
    fn from_bounds(bounds: Rect, width: u16, height: u16) -> Option<Self> {
        if !bounds.is_finite() {
            return None;
        }
        let clamp = |v: f64, max: u16| -> u16 {
            let v = v.round().clamp(0.0, f64::from(max));
            v as u16
        };
        let x0 = clamp(bounds.x0.floor() - 1.0, width);
        let y0 = clamp(bounds.y0.floor() - 1.0, height);
        let x1 = clamp(bounds.x1.ceil() + 1.0, width);
        let y1 = clamp(bounds.y1.ceil() + 1.0, height);
        (x0 < x1 && y0 < y1).then_some(Self::new(x0, y0, x1, y1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_math::{Point, rect_path};

    /// A 64x64 frame with a background and exactly the ops given.
    fn frame(ops: Vec<DrawOp>) -> DrawList {
        DrawList {
            ops,
            background: Some(rustim_color::BG),
            width: 64,
            height: 64,
        }
    }

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> DrawOp {
        DrawOp::FillPath(rect_path(Rect::new(x0, y0, x1, y1)))
    }

    /// Two squares at opposite corners, each preceded by its own paint, so a
    /// changed paint at one position is unambiguous.
    fn two_squares(second: Srgb) -> DrawList {
        frame(vec![
            DrawOp::SetTransform(Affine::IDENTITY),
            DrawOp::SetPaint(rustim_color::BLUE),
            square(4.0, 4.0, 14.0, 14.0),
            DrawOp::SetPaint(second),
            square(40.0, 40.0, 54.0, 54.0),
        ])
    }

    #[test]
    fn an_identical_frame_damages_nothing() {
        let a = two_squares(rustim_color::RED);
        let b = a.clone();
        assert_eq!(b.damage_since(&a), Damage::None);
    }

    #[test]
    fn a_paint_change_damages_only_the_geometry_it_paints() {
        let old = two_squares(rustim_color::RED);
        let new = two_squares(rustim_color::YELLOW);
        let Damage::Region(r) = new.damage_since(&old) else {
            panic!("expected a region");
        };
        // The second square's box, with the one-pixel skirt; nothing of the
        // first square may be inside it.
        assert!(
            r.x0 >= 39 && r.y0 >= 39 && r.x1 <= 55 && r.y1 <= 55,
            "region {r:?} is not the changed square"
        );
        assert!(r.x0 > 14, "the unchanged square was included: {r:?}");
    }

    /// The acceptance property, stated as an assertion: a one-property change
    /// repaints a small fraction of the frame.
    #[test]
    fn a_one_property_change_covers_much_less_than_the_frame() {
        let old = two_squares(rustim_color::RED);
        let new = two_squares(rustim_color::YELLOW);
        let damage = new.damage_since(&old);
        assert!(
            damage.pixels(64, 64) * 10 < 64 * 64,
            "{} of {} pixels damaged",
            damage.pixels(64, 64),
            64 * 64
        );
    }

    #[test]
    fn a_moved_shape_damages_where_it_was_and_where_it_is() {
        let old = frame(vec![
            DrawOp::SetTransform(Affine::IDENTITY),
            DrawOp::SetPaint(rustim_color::RED),
            square(4.0, 4.0, 14.0, 14.0),
        ]);
        let new = frame(vec![
            DrawOp::SetTransform(Affine::IDENTITY),
            DrawOp::SetPaint(rustim_color::RED),
            square(40.0, 40.0, 54.0, 54.0),
        ]);
        let Damage::Region(r) = new.damage_since(&old) else {
            panic!("expected a region");
        };
        assert!(
            r.x0 <= 3 && r.y0 <= 3 && r.x1 >= 55 && r.y1 >= 55,
            "region {r:?} must cover both the old and the new position"
        );
    }

    #[test]
    fn a_transform_change_damages_both_placements() {
        let ops = |t: Affine| {
            vec![
                DrawOp::SetTransform(t),
                DrawOp::SetPaint(rustim_color::RED),
                square(0.0, 0.0, 10.0, 10.0),
            ]
        };
        let old = frame(ops(Affine::IDENTITY));
        let new = frame(ops(Affine::translate((40.0, 40.0))));
        let Damage::Region(r) = new.damage_since(&old) else {
            panic!("expected a region, got {:?}", new.damage_since(&old));
        };
        assert!(
            r.x0 <= 1 && r.y0 <= 1 && r.x1 >= 51 && r.y1 >= 51,
            "region {r:?} must cover both placements"
        );
    }

    #[test]
    fn a_size_change_is_a_full_repaint() {
        let old = two_squares(rustim_color::RED);
        let mut new = old.clone();
        new.width = 32;
        assert_eq!(new.damage_since(&old), Damage::Full);
    }

    #[test]
    fn a_background_change_is_a_full_repaint() {
        let old = two_squares(rustim_color::RED);
        let mut new = old.clone();
        new.background = Some(rustim_color::BLUE);
        assert_eq!(new.damage_since(&old), Damage::Full);
    }

    #[test]
    fn a_transparent_background_cannot_be_repainted_by_region() {
        // There is nothing to clear a region back to, so the pixels beneath a
        // removed object could not be reconstructed.
        let mut old = two_squares(rustim_color::RED);
        old.background = None;
        let mut new = old.clone();
        new.ops.pop();
        assert_eq!(new.damage_since(&old), Damage::Full);
    }

    #[test]
    fn a_clip_makes_the_frame_positional_and_dirty() {
        let old = frame(vec![
            DrawOp::PushClip(rect_path(Rect::new(0.0, 0.0, 32.0, 32.0))),
            DrawOp::SetPaint(rustim_color::RED),
            square(4.0, 4.0, 14.0, 14.0),
            DrawOp::PopClip,
        ]);
        let mut new = old.clone();
        new.ops[2] = square(5.0, 5.0, 15.0, 15.0);
        assert_eq!(new.damage_since(&old), Damage::Full);
    }

    #[test]
    fn a_change_entirely_outside_the_frame_damages_nothing() {
        let ops = |c: Srgb| {
            vec![
                DrawOp::SetTransform(Affine::IDENTITY),
                DrawOp::SetPaint(c),
                square(100.0, 100.0, 120.0, 120.0),
            ]
        };
        let old = frame(ops(rustim_color::RED));
        let new = frame(ops(rustim_color::BLUE));
        assert_eq!(new.damage_since(&old), Damage::None);
    }

    #[test]
    fn a_stroke_is_damaged_by_its_width_not_only_its_centreline() {
        let ops = |w: f64| {
            vec![
                DrawOp::SetTransform(Affine::IDENTITY),
                DrawOp::SetPaint(rustim_color::RED),
                DrawOp::StrokePath {
                    path: rect_path(Rect::new(10.0, 10.0, 20.0, 20.0)),
                    stroke: StrokeStyle {
                        width: w,
                        ..StrokeStyle::default()
                    },
                },
            ]
        };
        let old = frame(ops(1.0));
        let new = frame(ops(9.0));
        let Damage::Region(r) = new.damage_since(&old) else {
            panic!("expected a region");
        };
        // The widest stroke reaches 4.5 units outside the path box.
        assert!(r.x0 <= 5 && r.y0 <= 5 && r.x1 >= 25 && r.y1 >= 25, "{r:?}");
    }

    #[test]
    fn appending_an_object_damages_only_the_new_object() {
        let old = frame(vec![
            DrawOp::SetTransform(Affine::IDENTITY),
            DrawOp::SetPaint(rustim_color::RED),
            square(4.0, 4.0, 14.0, 14.0),
        ]);
        let mut new = old.clone();
        new.ops.push(DrawOp::SetPaint(rustim_color::BLUE));
        new.ops.push(square(40.0, 40.0, 54.0, 54.0));
        let Damage::Region(r) = new.damage_since(&old) else {
            panic!("expected a region");
        };
        assert!(r.x0 > 14 && r.y0 > 14, "{r:?}");
    }

    #[test]
    fn a_glyph_run_is_damaged_over_the_glyphs_it_contains() {
        // The one op whose bounds come from a shaper's output rather than a
        // path: a moved glyph has to damage its own box, and only that.
        let font =
            peniko::FontData::new(peniko::Blob::new(std::sync::Arc::new(Vec::<u8>::new())), 0);
        let ops = |x: f32| {
            vec![
                DrawOp::SetTransform(Affine::IDENTITY),
                DrawOp::SetPaint(rustim_color::RED),
                DrawOp::GlyphRun {
                    font: font.clone(),
                    size: 10.0,
                    glyphs: vec![
                        TextGlyph { id: 3, x, y: 20.0 },
                        TextGlyph {
                            id: 4,
                            x: 40.0,
                            y: 20.0,
                        },
                    ],
                },
            ]
        };
        let old = frame(ops(10.0));
        let new = frame(ops(12.0));
        let Damage::Region(r) = new.damage_since(&old) else {
            panic!("expected a region");
        };
        // Glyph boxes are (x - size, y - size) .. (x + 2 size, y + size). A run
        // is one op, so a change to one glyph damages the run's whole extent —
        // here both glyphs, x in 0..=60 and y in 10..=30, plus the skirt. That
        // is conservative across the run and still far smaller than the frame:
        // a run is a line of text, and lines are small.
        assert_eq!((r.x0, r.y0, r.x1, r.y1), (0, 9, 61, 31), "{r:?}");
        assert!(
            new.damage_since(&old).pixels(64, 64) * 2 < 64 * 64,
            "a line of text must not dirty the whole frame"
        );
    }

    #[test]
    fn damage_pixels_agree_with_the_rectangle() {
        let d = Damage::Region(PixelRect::new(2, 3, 12, 8));
        assert_eq!(d.pixels(64, 64), 50);
        assert_eq!(Damage::None.pixels(64, 64), 0);
        assert_eq!(Damage::Full.pixels(64, 64), 64 * 64);
    }

    #[test]
    fn clear_keeps_capacity() {
        let mut list = DrawList::new(10, 10);
        for _ in 0..8 {
            list.push(DrawOp::FillPath(rect_path(Rect::new(0.0, 0.0, 1.0, 1.0))));
        }
        let cap = list.ops.capacity();
        list.clear(20, 20);
        assert!(list.ops.is_empty());
        assert!(list.ops.capacity() >= cap);
        assert_eq!((list.width, list.height), (20, 20));
    }

    #[test]
    fn empty_test_accounts_for_a_lone_background() {
        let mut list = DrawList::new(1, 1);
        assert!(list.is_empty());
        list.background = Some(rustim_color::BG);
        assert!(
            !list.is_empty(),
            "a background alone still changes the frame"
        );
    }

    #[test]
    fn paths_survive_being_stored() {
        let mut p = BezPath::new();
        p.move_to(Point::new(0.0, 0.0));
        p.line_to(Point::new(1.0, 1.0));
        let list = DrawList {
            ops: vec![DrawOp::FillPath(p)],
            ..DrawList::new(1, 1)
        };
        match &list.ops[0] {
            DrawOp::FillPath(path) => assert_eq!(path.elements().len(), 2),
            other => panic!("wrong op: {other:?}"),
        }
    }
}
