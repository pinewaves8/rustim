//! A compiled scene's text: `ShapedText` → the kernel's op builder.
//!
//! The op builder itself lives in [`rustim_scene::text`], because a text object
//! created by a script's `add` is drawn by `SceneContext::build_draw_list` and
//! must go through the same transform rule as a scene that pushes its own run.
//! This module is the app-side adapter: it carries the shaper's type across the
//! boundary ([`block_of`]), re-exports the one layout conversion, and keeps the
//! `push_text` helper the scenes and the caption band call.
//!
//! The doc comment below is about that conversion, and it is worth keeping here
//! — next to the shaper's type — because the subtle mistakes it names are made
//! when someone is looking at a `ShapedText`, not at a `TextBlock`.
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
//! The parenthesised pair is the caller's [`Affine2`] argument — `SceneContext`
//! folds it into one transform ([`rustim_scene::SceneContext::affine`]), so this
//! module never has to know a `Viewport` or a `Camera` exists, and a camera move
//! scales the glyphs for the same reason a resize does.
//!
//! Doing it the other way round — flipping and centring the *coordinates* — looks
//! equivalent and is not. `glifo`, the backend's text layer, composes
//! `run_transform · glyph_position · font_size · y_flip · outline`: it flips the
//! **outline** from font space (Y up) to layout space (Y down) and then maps
//! everything through the run transform. Flip the position and not the outline
//! and every glyph is mirrored about its own baseline — the baselines land where
//! they should and the letters are upside down, which reads as a font problem
//! rather than as a coordinate bug. The rule the backend imposes is simply that
//! the run transform maps layout space to the device, so the conversion lives
//! there and nowhere else. [`layout_to_local`] is all of it.
//!
//! # The size is not converted either, and that is the point
//!
//! Nothing here multiplies the font size by the viewport scale. It does not need
//! to: the run is drawn *under* a transform that already contains the viewport.
//! A uniform positive scale is absorbed into the font size by
//! `glifo::prepare_glyph_run` — which is why the glyphs are hinted for the size
//! they are actually drawn at — so a run shaped at
//! [`rustim_text::TextStyle`]'s world units per em ends up the right number of
//! pixels tall, and it scales with the window for the same reason. That the
//! composite really is a positive uniform scale is worth knowing when editing
//! here: the two flips in it (the viewport's and ours) cancel, and a change that
//! leaves a *net* negative y scale silently drops the run out of the hinted path
//! and into the direct one, where the outlines are transformed instead of
//! absorbed.

use rustim_color::Srgb;
use rustim_math::{Affine2, Rect};
use rustim_mobject::TextBlock;
use rustim_render::{DrawList, DrawOp};
use rustim_text::ShapedText;

/// The conversion from the shaper's layout space to a mobject's local space.
///
/// Re-exported from [`rustim_mobject::text`], which is where the wire-level
/// `add` path also reads it: the flip and the centring have to happen in exactly
/// one place, and that place is the kernel — a compiled scene's text and a
/// scripted `add` must not be two conversions that agree today.
pub use rustim_mobject::text::layout_to_local;

/// A shaped block in the kernel's data form.
///
/// This is the boundary crossing: `ShapedText` is what the shaper hands out,
/// `TextBlock` is what the kernel stores and draws. Everything below is a thin
/// wrapper so a scene's own text goes through the same code as a wire-created
/// one — [`rustim_scene::text`] holds the actual op builder.
pub fn block_of(shaped: &ShapedText) -> TextBlock {
    TextBlock::from(shaped)
}

/// A shaped block's extent in its own local space.
///
/// This is the box [`layout_to_local`] centres on the origin, in world units —
/// **not** the glyphs' ink box. A descender, an italic overhang or a combining
/// mark can reach outside it, exactly as they can reach outside a font's line
/// box; that is what a layout box means, and it is the box a glyph run is placed
/// by. Measuring ink instead would need a per-run outline walk and would make
/// the accessibility bounds disagree with where the run was actually drawn.
///
/// `None` for a block with no extent: an empty string has nothing to frame, and
/// a zero-area bounds would be a focus target with no size.
pub fn local_bounds(shaped: &ShapedText) -> Option<Rect> {
    block_of(shaped).local_bounds()
}

/// The ops that draw one shaped block, centred on the origin of its own frame.
///
/// See [`rustim_scene::text::text_ops`] for what the transform is and why the
/// flip lives in it rather than in the glyph positions. This wrapper exists so a
/// scene holding a [`ShapedText`] does not have to convert by hand; the body is
/// the shared builder, not a second copy of it.
///
/// Emits a [`DrawOp::SetTransform`] first, since the draw list's transform is
/// global to the frame and the view has to be reinstated after whatever was
/// drawn before. Nothing is emitted for an empty block: a scene whose font was
/// missing should cost nothing to draw, and an empty glyph run is not a thing the
/// backend needs to be told about.
pub fn text_ops(
    shaped: &ShapedText,
    world: Affine2,
    to_device: Affine2,
    paint: Srgb,
) -> Vec<DrawOp> {
    rustim_scene::text::text_ops(&block_of(shaped), world, to_device, paint)
}

/// The ops for one shaped block, appended to `list`.
///
/// A convenience for a scene that wants the block in the frame it is already
/// building, where the world→device transform has to come from the context
/// (`ctx.affine()`) rather than from an argument the scene chose.
pub fn push_text(
    list: &mut DrawList,
    shaped: &ShapedText,
    world: Affine2,
    to_device: Affine2,
    paint: Srgb,
) {
    for op in text_ops(shaped, world, to_device, paint) {
        list.push(op);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_math::{Camera, Point, Viewport};
    use rustim_render::TextGlyph;
    use rustim_text::{FontProvider, TextStyle};

    const UNIT_SCALE: f64 = 1.0;

    /// The world→device transform the app passes in: the default viewport under
    /// the default camera, which is 90 px per unit.
    fn device() -> Affine2 {
        Viewport::default().affine(&Camera::default())
    }

    fn shaped(text: &str) -> ShapedText {
        let mut provider = FontProvider::with_bundled().expect("assets/fonts");
        provider.shape(text, &TextStyle::default(), UNIT_SCALE, None)
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

    /// Every glyph the ops would draw, as `(id, x, y)` in the shaper's own frame.
    fn glyphs(ops: &[DrawOp]) -> Vec<TextGlyph> {
        ops.iter()
            .filter_map(|op| match op {
                DrawOp::GlyphRun { glyphs, .. } => Some(glyphs.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    #[test]
    fn the_block_is_centred_on_the_origin_and_stands_up() {
        // The whole reason `layout_to_local` exists. In layout space the block
        // occupies `[0, width] x [0, height]`, y down; in local space it has to
        // straddle the origin, y up, or a text mobject would scale about its
        // top-left corner and its glyphs would be drawn upside down.
        let (w, h) = (4.0_f32, 2.0_f32);
        let l = layout_to_local(w, h);
        let (top_left, bottom_right) = (
            l * Point::new(0.0, 0.0),
            l * Point::new(f64::from(w), f64::from(h)),
        );
        assert!((top_left.x + 2.0).abs() < 1e-9 && (top_left.y - 1.0).abs() < 1e-9);
        assert!((bottom_right.x - 2.0).abs() < 1e-9 && (bottom_right.y + 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_point_above_the_baseline_lands_above_the_baseline_on_screen() {
        // The regression test for the mirrored-glyph bug. `glifo` flips the
        // *outline* into layout space (y down) and then maps it through this
        // transform; the glyph's position goes through the same transform. So the
        // test is stated in the shaper's own frame: take a point one unit above
        // the pen position — smaller y, since this frame grows downwards — and it
        // must come out higher up the screen. A transform missing the flip sends
        // it lower, and every letter is upside down while every baseline looks
        // right.
        let s = shaped("abc");
        let ops = text_ops(&s, Affine2::IDENTITY, device(), rustim_color::FG);
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

    #[test]
    fn the_glyphs_keep_the_positions_the_shaper_gave_them() {
        // No pre-flipping and no pre-centring: the conversion is in the
        // transform, so what reaches the backend is what `parley` said. A second
        // conversion here is exactly the mirrored-glyph bug, and it is invisible
        // in the positions this test reads.
        let s = shaped("abc");
        let ops = text_ops(&s, Affine2::IDENTITY, device(), rustim_color::FG);
        let drawn = glyphs(&ops);
        let shaped_glyphs: Vec<_> = s.runs.iter().flat_map(|r| r.glyphs.iter()).collect();
        assert_eq!(drawn.len(), shaped_glyphs.len());
        for (a, b) in drawn.iter().zip(shaped_glyphs) {
            assert_eq!((a.id, a.x, a.y), (b.id, b.x, b.y));
        }
    }

    #[test]
    fn the_block_lands_where_the_world_transform_puts_it() {
        // The two halves have to compose, not just each be right: the view
        // maps world to screen, the world transform places the block, and the
        // block's own centre is the point that lands at `world`'s origin.
        let s = shaped("abc");
        let world = Affine2::translate((2.0, 1.0));
        let ops = text_ops(&s, world, device(), rustim_color::FG);
        let centre =
            place_of(&ops) * Point::new(f64::from(s.width / 2.0), f64::from(s.height / 2.0));
        let (x, y) = Viewport::default().to_screen(&Camera::default(), (2.0, 1.0));
        assert!((centre.x - x).abs() < 1e-9 && (centre.y - y).abs() < 1e-9);

        // And the run is still sized in world units: the viewport's scale is
        // absorbed by the backend, not applied here. The slack is `f32::EPSILON`-
        // sized because a run's size is an `f32` and the style's is an `f64`, not
        // because anything rounds the size on the way through.
        assert!((f64::from(s.runs[0].size) - TextStyle::default().size).abs() < 1e-6);
    }

    #[test]
    fn the_composite_scale_is_positive_and_uniform() {
        // Which backend path a run takes depends on this: a positive uniform
        // scale is absorbed into the font size and the glyphs are hinted for the
        // size they are drawn at, anything else (a rotation, a non-uniform
        // stretch) transforms the outlines instead. Both draw, so a mistake here
        // would not look like a bug — it would quietly cost the hinting, and the
        // viewport's flip and ours are what cancel to keep it.
        let s = shaped("abc");
        let ops = text_ops(&s, Affine2::IDENTITY, device(), rustim_color::FG);
        let [_, b, c, d, _, _] = place_of(&ops).as_coeffs();
        assert_eq!((b, c), (0.0, 0.0), "no rotation or skew");
        assert!(
            d > 0.0,
            "the flips must cancel, or the run is drawn unhinted"
        );
        assert!((d - 90.0).abs() < 1e-9, "world units to pixels");
    }

    /// The camera is folded in by the caller, so a zoom scales the glyphs the
    /// same way a window resize does — and, crucially, it stays a *positive
    /// uniform* composite, which is the condition for the hinted path. A camera
    /// that produced a rotation or a flip would silently cost the hinting rather
    /// than look wrong.
    #[test]
    fn a_zoomed_camera_scales_the_glyph_run() {
        let s = shaped("abc");
        let camera = Camera::new((0.5, -0.25), 2.0);
        let to_device = Viewport::default().affine(&camera);
        let ops = text_ops(&s, Affine2::IDENTITY, to_device, rustim_color::FG);
        let place = place_of(&ops);
        let [_, b, c, d, _, _] = place.as_coeffs();
        assert_eq!((b, c), (0.0, 0.0), "a zoom is not a rotation");
        assert!((d - 180.0).abs() < 1e-9, "zoom 2 doubles px/unit: {d}");

        // And the pan shows: the block sits at the world origin, while the camera
        // is centred on (0.5, -0.25) — so the block lands three quarters of a
        // unit left of and a quarter unit below the frame's centre, at 180 px per
        // unit.
        let centre = place * Point::new(f64::from(s.width / 2.0), f64::from(s.height / 2.0));
        assert!(
            (centre.x - 550.0).abs() < 1e-9 && (centre.y - 315.0).abs() < 1e-9,
            "{centre:?}"
        );
    }

    #[test]
    fn a_rotated_block_is_drawn_at_the_same_size() {
        // The other path: `world` carries a rotation, so the scale cannot be
        // absorbed into the font size and the outlines are transformed instead.
        // The size has to come out the same either way — it is the same em — and
        // this is the test that says so, since the two paths share no code.
        let s = shaped("abc");
        let unit_length = |ops: &[DrawOp]| {
            let place = place_of(ops);
            let o = place * Point::new(0.0, 0.0);
            let u = place * Point::new(1.0, 0.0);
            (u.x - o.x).hypot(u.y - o.y)
        };
        let straight = unit_length(&text_ops(&s, Affine2::IDENTITY, device(), rustim_color::FG));
        let rotated = unit_length(&text_ops(
            &s,
            Affine2::rotate(0.4),
            device(),
            rustim_color::FG,
        ));
        assert!((straight - 90.0).abs() < 1e-9);
        assert!(
            (straight - rotated).abs() < 1e-9,
            "a rotation is not a resize"
        );
    }

    #[test]
    fn an_empty_block_costs_nothing() {
        let mut provider = FontProvider::new();
        let s = provider.shape("abc", &TextStyle::default(), UNIT_SCALE, None);
        assert!(
            text_ops(&s, Affine2::IDENTITY, device(), rustim_color::FG).is_empty(),
            "a scene with no font should emit no ops at all"
        );
    }

    #[test]
    fn the_paint_is_set_before_the_glyphs() {
        let s = shaped("a");
        let ops = text_ops(&s, Affine2::IDENTITY, device(), rustim_color::RED);
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
    }
}
