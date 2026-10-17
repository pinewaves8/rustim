//! Turning a [`TextBlock`] into draw operations.
//!
//! This is the one place a shaper's output meets a rasteriser's input, and it
//! lives in the kernel rather than in a host because a wire-created text mobject
//! has to be drawn by `SceneContext::build_draw_list` itself — the scene that
//! pushed its own text used to be the only writer. `rustim_app::text` delegates
//! here, so a compiled scene's text and a scripted `add` produce the same ops by
//! construction rather than by two people reading the same comment.
//!
//! # One conversion, and it belongs in the transform
//!
//! A shaper reports positions the way a page does: `y` grows **downwards** from
//! the top of the block, and the origin is the block's top-left corner. World
//! space is the other way up — `Viewport` maps Y-up centred world units to Y-down
//! pixels — and a mobject's local origin is its centre, because that is what its
//! transform scales and rotates about.
//!
//! So a glyph run needs two things done to it, and the only question is *where*.
//! Both go into the transform, and the glyph coordinates are passed through
//! exactly as the shaper produced them:
//!
//! ```text
//! device = (viewport · camera) · world · (centre and flip)
//! ```
//!
//! The first pair is the caller's `to_device` — `SceneContext::affine` folds the
//! viewport and the camera into one transform, so this module never has to know
//! either exists, and a camera move scales the glyphs for the same reason a
//! resize does.
//!
//! Doing it the other way round — flipping and centring the *coordinates* — looks
//! equivalent and is not. `glifo`, the backend's text layer, composes
//! `run_transform · glyph_position · font_size · y_flip · outline`: it flips the
//! **outline** from font space (Y up) to layout space (Y down) and then maps
//! everything through the run transform. Flip the position and not the outline
//! and every glyph is mirrored about its own baseline — the baselines land where
//! they should and the letters are upside down, which reads as a font problem
//! rather than as a coordinate bug. The rule the backend imposes is that the run
//! transform maps layout space to the device, so the conversion lives there and
//! nowhere else. [`TextBlock::layout_to_local`] is all of it.
//!
//! # The size is not converted either, and that is the point
//!
//! Nothing here multiplies the font size by the viewport scale. It does not need
//! to: the run is drawn *under* a transform that already contains the viewport.
//! A uniform positive scale is absorbed into the font size by
//! `glifo::prepare_glyph_run` — which is why the glyphs are hinted for the size
//! they are actually drawn at — so a run shaped at `rustim_text::TextStyle`'s
//! world units per em ends up the right number of pixels tall, and it scales
//! with the window for the same reason. That the composite really is a positive
//! uniform scale is worth knowing when editing here: the two flips in it (the
//! viewport's and ours) cancel, and a change that leaves a *net* negative y scale
//! silently drops the run out of the hinted path and into the direct one, where
//! the outlines are transformed instead of absorbed.

use rustim_color::Srgb;
use rustim_math::Affine2;
use rustim_mobject::TextBlock;
use rustim_render::{DrawList, DrawOp};

/// The ops that draw one shaped block, centred on the origin of its own frame.
///
/// `world` is the mobject's own transform composed with its ancestors' — the
/// caller folds those, because only the scene tree knows them. `to_device` is the
/// world→screen transform for the frame: the viewport *and the camera*, already
/// composed ([`crate::SceneContext::affine`]). Taking the composed
/// transform rather than the two pieces is deliberate — this module has no
/// business knowing where the view is looking, and a caller who passed only the
/// viewport would draw text at the wrong place while every filled path moved
/// correctly. The paint is given rather than read from a mobject so that this
/// stays a pure function of its arguments, which is what makes it testable
/// without a tree.
///
/// Emits a [`DrawOp::SetTransform`] first, since the draw list's transform is
/// global to the frame and the view has to be reinstated after whatever was
/// drawn before — and **the caller has to reinstate it again after this**, which
/// `SceneContext::build_draw_list` does. That is the one sharp edge of a builder
/// that returns ops rather than appending them: the transform it leaves behind
/// belongs to the block.
///
/// Nothing is emitted for an empty block: a scene whose font was missing should
/// cost nothing to draw, and an empty glyph run is not a thing the backend needs
/// to be told about.
pub fn text_ops(block: &TextBlock, world: Affine2, to_device: Affine2, paint: Srgb) -> Vec<DrawOp> {
    if block.is_empty() {
        return Vec::new();
    }
    // The block's own placement in the world, then the view: the glyphs keep the
    // shaper's coordinates, so this is the only place the flip and the centring
    // happen.
    let place = to_device * world * block.layout_to_local();

    let mut ops = Vec::with_capacity(block.runs.len() * 2 + 1);
    ops.push(DrawOp::SetTransform(place));
    ops.push(DrawOp::SetPaint(paint));
    for run in &block.runs {
        ops.push(DrawOp::GlyphRun {
            font: run.font.clone(),
            size: run.size,
            glyphs: run.glyphs.clone(),
        });
    }
    ops
}

/// The ops for one block, appended to `list`.
///
/// A convenience for a scene that wants the block in the frame it is already
/// building. The caller still owns reinstating the frame transform afterwards,
/// for the reason [`text_ops`] gives.
pub fn push_text(
    list: &mut DrawList,
    block: &TextBlock,
    world: Affine2,
    to_device: Affine2,
    paint: Srgb,
) {
    for op in text_ops(block, world, to_device, paint) {
        list.push(op);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_math::{Camera, Point, Viewport};
    use rustim_mobject::TextRun;
    use rustim_mobject::text::FontData;
    use rustim_render::TextGlyph;

    /// The world→device transform the context passes in: the default viewport
    /// under the default camera, which is 90 px per unit.
    fn device() -> Affine2 {
        Viewport::default().affine(&Camera::default())
    }

    /// A block with a real size and one glyph, built without a shaper: these
    /// tests are about the transform, and the shaper's coordinates are exactly
    /// the numbers passed through.
    fn block(w: f32, h: f32) -> TextBlock {
        TextBlock::new(
            w,
            h,
            vec![TextRun {
                font: FontData::new(Vec::<u8>::new().into(), 0),
                size: 1.0,
                glyphs: vec![TextGlyph {
                    id: 7,
                    x: 0.5,
                    y: 0.25,
                }],
            }],
        )
    }

    /// The run transform, which is what places the glyphs.
    fn place_of(ops: &[DrawOp]) -> Affine2 {
        ops.iter()
            .find_map(|op| match op {
                DrawOp::SetTransform(a) => Some(*a),
                _ => None,
            })
            .expect("the viewport has to be reinstated before the glyphs")
    }

    /// Every glyph the ops would draw, in the shaper's own frame.
    fn glyphs(ops: &[DrawOp]) -> Vec<TextGlyph> {
        ops.iter()
            .filter_map(|op| match op {
                DrawOp::GlyphRun { glyphs, .. } => Some(glyphs.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// The regression test for the mirrored-glyph bug. `glifo` flips the
    /// *outline* into layout space (y down) and then maps it through this
    /// transform; the glyph's position goes through the same transform. So the
    /// test is stated in the shaper's own frame: take a point one unit above
    /// the pen position — smaller y, since this frame grows downwards — and it
    /// must come out higher up the screen. A transform missing the flip sends
    /// it lower, and every letter is upside down while every baseline looks
    /// right.
    #[test]
    fn a_point_above_the_baseline_lands_above_the_baseline_on_screen() {
        let b = block(4.0, 2.0);
        let ops = text_ops(&b, Affine2::IDENTITY, device(), rustim_color::FG);
        let place = place_of(&ops);
        let g = glyphs(&ops)[0];
        let on_baseline = place * Point::new(f64::from(g.x), f64::from(g.y));
        let above = place * Point::new(f64::from(g.x), f64::from(g.y) - 1.0);
        assert!(
            above.y < on_baseline.y,
            "a point above the baseline has to land above it: {above:?} vs {on_baseline:?}"
        );
        assert!(
            (above.x - on_baseline.x).abs() < 1e-9,
            "the flip is vertical only"
        );
    }

    /// No pre-flipping and no pre-centring: the conversion is in the transform,
    /// so what reaches the backend is what the shaper said.
    #[test]
    fn the_glyphs_keep_the_positions_the_shaper_gave_them() {
        let b = block(4.0, 2.0);
        let ops = text_ops(&b, Affine2::IDENTITY, device(), rustim_color::FG);
        let drawn = glyphs(&ops);
        let declared: Vec<_> = b.runs.iter().flat_map(|r| r.glyphs.iter()).collect();
        assert_eq!(drawn.len(), declared.len());
        for (a, b) in drawn.iter().zip(declared) {
            assert_eq!((a.id, a.x, a.y), (b.id, b.x, b.y));
        }
    }

    /// The two halves compose: the view maps world to screen, the world
    /// transform places the block, and the block's own centre is the point that
    /// lands at `world`'s origin.
    #[test]
    fn the_block_lands_where_the_world_transform_puts_it() {
        let b = block(4.0, 2.0);
        let world = Affine2::translate((2.0, 1.0));
        let ops = text_ops(&b, world, device(), rustim_color::FG);
        let centre =
            place_of(&ops) * Point::new(f64::from(b.width / 2.0), f64::from(b.height / 2.0));
        let (x, y) = Viewport::default().to_screen(&Camera::default(), (2.0, 1.0));
        assert!((centre.x - x).abs() < 1e-9 && (centre.y - y).abs() < 1e-9);
    }

    /// Which backend path a run takes depends on this: a positive uniform scale
    /// is absorbed into the font size and the glyphs are hinted for the size
    /// they are drawn at, anything else (a rotation, a non-uniform stretch)
    /// transforms the outlines instead. Both draw, so a mistake here would not
    /// look like a bug — it would quietly cost the hinting, and the viewport's
    /// flip and ours are what cancel to keep it.
    #[test]
    fn the_composite_scale_is_positive_and_uniform() {
        let b = block(4.0, 2.0);
        let ops = text_ops(&b, Affine2::IDENTITY, device(), rustim_color::FG);
        let [_, p, q, d, _, _] = place_of(&ops).as_coeffs();
        assert_eq!((p, q), (0.0, 0.0), "no rotation or skew");
        assert!(
            d > 0.0,
            "the flips must cancel, or the run is drawn unhinted"
        );
        assert!((d - 90.0).abs() < 1e-9, "world units to pixels");
    }

    /// A block whose every run is empty emits nothing at all — not a transform
    /// and no paint — so a scene with a missing font costs nothing.
    #[test]
    fn an_empty_block_costs_nothing() {
        // No runs at all, and a run whose every character was missing from the
        // font: neither has a glyph to draw. `is_empty` is deliberately about
        // glyphs and not about the layout box — a block with a zero `width`
        // still paints the glyphs it has, so it must not be skipped.
        let no_runs = TextBlock::new(4.0, 2.0, Vec::new());
        assert!(text_ops(&no_runs, Affine2::IDENTITY, device(), rustim_color::FG).is_empty());
        let mut no_glyphs = block(4.0, 2.0);
        no_glyphs.runs[0].glyphs.clear();
        assert!(text_ops(&no_glyphs, Affine2::IDENTITY, device(), rustim_color::FG).is_empty());
        assert!(
            !text_ops(
                &block(0.0, 0.0),
                Affine2::IDENTITY,
                device(),
                rustim_color::FG
            )
            .is_empty()
        );
    }

    /// Draw commands are stateful and ordered: the paint has to be set before
    /// the run, and the transform before both.
    #[test]
    fn the_paint_is_set_before_the_glyphs() {
        let b = block(4.0, 2.0);
        let ops = text_ops(&b, Affine2::IDENTITY, device(), rustim_color::RED);
        let paint = ops
            .iter()
            .position(|op| matches!(op, DrawOp::SetPaint(_)))
            .expect("a paint");
        let glyphs = ops
            .iter()
            .position(|op| matches!(op, DrawOp::GlyphRun { .. }))
            .expect("a glyph run");
        assert!(paint < glyphs, "draw commands are stateful and ordered");
        assert!(matches!(ops[paint], DrawOp::SetPaint(c) if c == rustim_color::RED));
        assert!(matches!(ops[0], DrawOp::SetTransform(_)));
    }
}
