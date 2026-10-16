//! The backend-neutral draw list.
//!
//! A `DrawList` is what the scene layer produces and every backend consumes. It
//! is intentionally close to vello's own command set, because both backends
//! implement exactly that set — but it is *ours*, so the scene layer does not
//! depend on vello and a future third backend does not have to pretend to be
//! vello.

use rustim_color::Srgb;
use rustim_math::{Affine, BezPath, TextGlyph};

/// One drawing command.
///
/// Commands are stateful in the same way vello's are: [`DrawOp::SetPaint`] and
/// [`DrawOp::SetTransform`] apply to every subsequent path until changed. That
/// mirrors both backends and avoids threading a paint through every op.
#[derive(Debug, Clone)]
pub enum DrawOp {
    /// Set the colour used by subsequent fills and strokes.
    SetPaint(Srgb),
    /// Set the transform applied to subsequent geometry.
    SetTransform(Affine),
    FillPath(BezPath),
    StrokePath {
        path: BezPath,
        width: f64,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_math::{Point, Rect, rect_path};

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
