//! The software backend, and the one this project's floor machine actually runs.
//!
//! # What Phase 0 measured (2026-10-16, Phytium D3000M / Kylin V10)
//!
//! The floor machine has no usable GPU rendering path:
//!
//! - **Vulkan**: the adapter initialises and `vello_gpu::Renderer::new` succeeds,
//!   but every `queue.submit` **segfaults** — including a minimal 3-vertex,
//!   zero-bind-group triangle pipeline. The driver, not vello, is at fault.
//! - **GL**: `Phytium FTG340 (Gl)` renders a plain triangle correctly
//!   (`max_bind_groups = 8`), but rejects vello_gpu's fragment shader with
//!   `internal error : failed to add the source` from the vendor's GLSL compiler.
//! - No software Vulkan fallback (lavapipe) is reachable: `enumerate_adapters`
//!   returns exactly those two adapters.
//!
//! So `vello_cpu` is not a convenience fallback here, it *is* the renderer. That
//! makes the CPU path's performance a first-class design constraint rather than a
//! degradation of the GPU one, and it is why §10.5's CPU-floor decision is
//! settled rather than open.
//!
//! Measured cost at 1280x720, single-threaded, 4-cubic blobs (see the P0 spike):
//! ~0.7 ms for 10 shapes and ~9.7 ms for 2000 at 1080p, i.e. a 60fps budget
//! accommodates roughly 3500 shapes / 14000 cubic curves. Typical Manim scenes
//! are two orders of magnitude below that.

use vello_common::pixmap::Pixmap;
use vello_cpu::{RasterizerSettings, RenderContext, Resources, TargetInit};

use crate::draw::{Damage, DrawList, DrawOp, PixelRect};
use crate::{RenderError, RenderStats, Renderer};

/// Rasterises draw lists on the CPU.
pub struct CpuRenderer {
    ctx: RenderContext,
    resources: Resources,
    pixmap: Pixmap,
    width: u16,
    height: u16,
    /// The target of an incremental render, sized to the damage rectangle and
    /// copied back into `pixmap` afterwards.
    ///
    /// A separate pixmap rather than a view into `pixmap` because a region of a
    /// row-major buffer is not contiguous and `PixmapMut` has no stride. Kept
    /// across frames for the same reason `pixmap` is: the common case is the
    /// same rectangle twice.
    scratch: Option<Pixmap>,
    /// Set while encoding a frame in which a glyph could not be drawn, cleared at
    /// the start of the next one. A frame is not the right granularity to report
    /// it from — [`Self::render`] returns it after rasterising, so the pixels
    /// exist either way and the caller can decide what to do about it.
    text_error: Option<RenderError>,
}

impl CpuRenderer {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            ctx: RenderContext::new(width, height),
            resources: Resources::new(),
            pixmap: Pixmap::new(width, height),
            width,
            height,
            scratch: None,
            text_error: None,
        }
    }

    /// Replay a draw list into the context without rasterising.
    ///
    /// Split out from [`Renderer::render`] so that the export path can build a
    /// context once and rasterise it at several resolutions.
    pub fn encode(&mut self, list: &DrawList) {
        self.encode_with_origin(list, (0, 0));
    }

    /// Replay a draw list shifted by `-origin` in device space.
    ///
    /// The shift is applied to every `SetTransform` in the list rather than
    /// through the rasteriser's `offset`, because `offset` is unsigned and
    /// describes where the scene origin lands *inside* the target: to put the
    /// damage rectangle at (x, y) at the target's origin it would have to be
    /// negative. Composing `translate(-x, -y)` with the list's own transforms
    /// moves the whole scene instead, and the target's smaller size cuts it
    /// back down to the rectangle.
    ///
    /// The background fill is issued under the same shift, so it still covers
    /// every pixel of the target. It is the full frame's rectangle, not the
    /// damage rectangle's, which is larger than needed and deliberately simple:
    /// the excess is clipped away by the target and the extra coverage would be
    /// cut off anyway.
    pub fn encode_with_origin(&mut self, list: &DrawList, origin: (u16, u16)) {
        let shift = rustim_math::Affine::translate((-f64::from(origin.0), -f64::from(origin.1)));
        self.ctx.reset();
        self.text_error = None;
        self.ctx.set_transform(shift);
        if let Some(bg) = list.background {
            self.ctx.set_paint(bg);
            self.ctx.fill_rect(&rustim_math::Rect::new(
                0.0,
                0.0,
                f64::from(list.width),
                f64::from(list.height),
            ));
        }
        for op in &list.ops {
            match op {
                DrawOp::SetPaint(color) => self.ctx.set_paint(*color),
                DrawOp::SetTransform(affine) => self.ctx.set_transform(shift * *affine),
                DrawOp::FillPath(path) => self.ctx.fill_path(path),
                DrawOp::StrokePath { path, width } => {
                    self.ctx
                        .set_stroke(vello_common::kurbo::Stroke::new(*width));
                    self.ctx.stroke_path(path);
                }
                DrawOp::PushClip(path) => self.ctx.push_clip_path(path),
                // Two separate stacks in vello: `pop_clip` closes a clip,
                // `pop_layer` closes a layer. Getting this backwards aborts.
                DrawOp::PopClip => self.ctx.pop_clip(),
                DrawOp::PushLayer => self.ctx.push_layer(None, None, None, None, None),
                DrawOp::PopLayer => self.ctx.pop_layer(),
                DrawOp::GlyphRun { font, size, glyphs } => {
                    // The two borrows are of *different* fields of `self`, which
                    // is why this compiles: `glyph_run` needs the context and the
                    // resource cache at once, and both belong to this renderer.
                    // `Resources` is kept across resizes for exactly this call —
                    // it caches the glyph atlas, so a run redrawn every frame is
                    // rasterised once.
                    let result = self
                        .ctx
                        .glyph_run(&mut self.resources, font)
                        .font_size(*size)
                        .fill_glyphs(glyphs.iter().map(|g| vello_cpu::Glyph {
                            id: g.id,
                            x: g.x,
                            y: g.y,
                        }));
                    if let Err(e) = result {
                        // Recorded, not returned: the glyphs that could be drawn
                        // were, and throwing the frame away because one glyph in
                        // it was a bitmap this build cannot decode would be a
                        // worse answer than a frame with one blank in it.
                        self.text_error = Some(RenderError::GlyphsSkipped {
                            index: e.first_skipped.index,
                            count: e.skipped_count,
                        });
                    }
                }
            }
        }
    }

    /// Validate the list's size and bring the persistent target to it.
    fn prepare(&mut self, list: &DrawList) -> Result<(), RenderError> {
        if list.width == 0 || list.height == 0 {
            return Err(RenderError::BadSize(list.width, list.height));
        }
        if (list.width, list.height) != (self.width, self.height) {
            self.resize(list.width, list.height);
        }
        Ok(())
    }

    fn frame_pixels(&self, list: &DrawList) -> u64 {
        u64::from(list.width) * u64::from(list.height)
    }

    fn take_text_error(&mut self) -> Result<(), RenderError> {
        match self.text_error.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Rasterise one rectangle of `list` into the persistent pixmap.
    fn render_region(
        &mut self,
        list: &DrawList,
        rect: PixelRect,
        frame: u64,
    ) -> Result<RenderStats, RenderError> {
        self.prepare(list)?;
        let rect = PixelRect::new(
            rect.x0.min(self.width),
            rect.y0.min(self.height),
            rect.x1.min(self.width),
            rect.y1.min(self.height),
        );
        if rect.is_empty() {
            return Ok(RenderStats { painted: 0, frame });
        }
        // A rectangle that happens to be the whole frame is the full path; it
        // avoids allocating a scratch target the size of the frame.
        if rect.width() == self.width && rect.height() == self.height {
            self.encode(list);
            self.ctx.flush();
            self.ctx.render(self.pixmap.as_mut(), &mut self.resources);
            self.take_text_error()?;
            return Ok(RenderStats {
                painted: frame,
                frame,
            });
        }
        // `damage_since` refuses to produce a region for a transparent
        // background, because there would be nothing to clear the region to.
        // A caller that built such a `Damage` by hand gets a full frame rather
        // than stale pixels.
        let Some(background) = list.background else {
            return self.render_damaged(list, Damage::Full);
        };

        let (w, h) = (rect.width(), rect.height());
        if self
            .scratch
            .as_ref()
            .is_none_or(|p| (p.width(), p.height()) != (w, h))
        {
            self.scratch = Some(Pixmap::new(w, h));
        }
        self.encode_with_origin(list, (rect.x0, rect.y0));
        self.ctx.flush();
        // `Clear`, not `SrcOver`: the scratch is reused from one damaged frame
        // to the next, so it still holds the previous one's pixels, and an
        // object that left the rectangle must not leave its ghost behind.
        // Clearing to the background reproduces exactly the pixels a full
        // render would have produced there, because the full render also
        // starts from an opaque background.
        let settings = RasterizerSettings {
            target_init: TargetInit::Clear(background),
            ..RasterizerSettings::default()
        };
        self.ctx.render_with(
            self.scratch
                .as_mut()
                .expect("the scratch target was allocated just above"),
            &mut self.resources,
            settings,
        );
        copy_region(
            &mut self.pixmap,
            self.scratch
                .as_ref()
                .expect("the scratch target was allocated just above"),
            rect,
        );
        self.take_text_error()?;
        Ok(RenderStats {
            painted: u64::from(w) * u64::from(h),
            frame,
        })
    }
}

/// Copy the scratch's whole contents into `dst` at `(rect.x0, rect.y0)`.
///
/// Row by row: the two buffers have different widths, so there is no single
/// contiguous copy to make.
fn copy_region(dst: &mut Pixmap, src: &Pixmap, rect: PixelRect) {
    let dst_stride = usize::from(dst.width()) * 4;
    let src_stride = usize::from(src.width()) * 4;
    let row_bytes = usize::from(rect.width()) * 4;
    let (dst, src) = (dst.data_as_u8_slice_mut(), src.data_as_u8_slice());
    for row in 0..usize::from(rect.height()) {
        let d = (usize::from(rect.y0) + row) * dst_stride + usize::from(rect.x0) * 4;
        let s = row * src_stride;
        dst[d..d + row_bytes].copy_from_slice(&src[s..s + row_bytes]);
    }
}

impl Renderer for CpuRenderer {
    fn resize(&mut self, width: u16, height: u16) {
        if (width, height) == (self.width, self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        self.ctx = RenderContext::new(width, height);
        self.pixmap = Pixmap::new(width, height);
        // `Resources` caches pipelines and glyph atlases keyed by size; keeping
        // it across a resize is safe and avoids re-rasterising every glyph.
    }

    fn width(&self) -> u16 {
        self.width
    }

    fn height(&self) -> u16 {
        self.height
    }

    fn render(&mut self, list: &DrawList) -> Result<(), RenderError> {
        self.prepare(list)?;
        self.encode(list);
        // Required before `render` whenever the `multithreading` feature is on:
        // the parallel dispatcher batches work and panics with "attempted to
        // rasterize before flushing" if this is missing. Single-threaded builds
        // do not need it, so it is easy to omit by accident — Phase 0 found this
        // the hard way.
        self.ctx.flush();
        self.ctx.render(self.pixmap.as_mut(), &mut self.resources);
        // After rasterising, never instead of it: the frame is usable, and the
        // error says it is missing a glyph. See `text_error`.
        self.take_text_error()
    }

    fn render_damaged(
        &mut self,
        list: &DrawList,
        damage: Damage,
    ) -> Result<RenderStats, RenderError> {
        let frame = self.frame_pixels(list);
        match damage {
            // The whole point of the type: an idle frame does not touch the
            // rasteriser or the pixel buffer at all.
            Damage::None => Ok(RenderStats { painted: 0, frame }),
            Damage::Full => {
                self.render(list)?;
                Ok(RenderStats {
                    painted: frame,
                    frame,
                })
            }
            Damage::Region(rect) => self.render_region(list, rect, frame),
        }
    }

    fn pixels(&self) -> &[u8] {
        bytemuck::cast_slice(self.pixmap.data())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rustim_math::{Affine, BezPath, Point, Rect, rect_path};

    fn square() -> BezPath {
        rect_path(Rect::new(0.0, 0.0, 10.0, 10.0))
    }

    #[test]
    fn a_transformed_square_lands_where_expected() {
        let mut r = CpuRenderer::new(32, 32);
        let mut list = DrawList::new(32, 32);
        list.background = Some(rustim_color::BG);
        list.push(DrawOp::SetPaint(rustim_color::RED));
        list.push(DrawOp::SetTransform(Affine::translate((11.0, 11.0))));
        list.push(DrawOp::FillPath(square()));
        r.render(&list).unwrap();

        let at = |x: u16, y: u16| {
            let i = (usize::from(y) * 32 + usize::from(x)) * 4;
            [r.pixels()[i], r.pixels()[i + 1], r.pixels()[i + 2]]
        };
        // Inside the square: red-ish. Outside: background.
        assert!(
            at(15, 15)[0] > 200,
            "expected red at the centre, got {:?}",
            at(15, 15)
        );
        assert!(
            at(2, 2)[0] < 40,
            "expected background at the corner, got {:?}",
            at(2, 2)
        );
    }

    #[test]
    fn without_a_background_explicit_geometry_still_draws() {
        // `RasterizerSettings::default()` clears the target to transparent, so
        // `background: None` does *not* mean "keep the previous frame". The
        // incremental-redraw path in §4.3 therefore has to skip the rasteriser
        // entirely on a clean frame, rather than rely on the target persisting.
        let mut r = CpuRenderer::new(16, 16);
        let mut list = DrawList::new(16, 16);
        list.push(DrawOp::SetPaint(rustim_color::RED));
        list.push(DrawOp::FillPath(rect_path(Rect::new(0.0, 0.0, 16.0, 16.0))));
        r.render(&list).unwrap();
        assert_eq!(&r.pixels()[0..3], &[0xEF, 0x53, 0x50]);
        assert_eq!(r.pixels()[3], 255);
    }

    #[test]
    fn a_path_with_one_point_does_not_panic() {
        let mut r = CpuRenderer::new(8, 8);
        let mut list = DrawList::new(8, 8);
        let mut p = BezPath::new();
        p.move_to(Point::new(1.0, 1.0));
        list.push(DrawOp::FillPath(p));
        r.render(&list).unwrap();
    }

    #[test]
    fn clips_and_layers_balance() {
        let mut r = CpuRenderer::new(16, 16);
        let mut list = DrawList::new(16, 16);
        list.push(DrawOp::PushLayer);
        list.push(DrawOp::PushClip(square()));
        list.push(DrawOp::FillPath(square()));
        list.push(DrawOp::PopClip);
        list.push(DrawOp::PopLayer);
        r.render(&list).unwrap();
    }

    fn pixel(r: &CpuRenderer, x: u16, y: u16) -> [u8; 4] {
        let i = (usize::from(y) * usize::from(r.width()) + usize::from(x)) * 4;
        r.pixels()[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn a_clip_actually_clips_and_popping_it_restores_drawing() {
        // `clips_and_layers_balance` only proves the two stacks can be pushed and
        // popped without aborting. This one proves the clip *does* something,
        // and — the half that matters more — that `PopClip` really ends it,
        // since a leaked clip would be invisible in every scene that happens to
        // draw nothing outside its clip region.
        let mut r = CpuRenderer::new(16, 16);
        let mut list = DrawList::new(16, 16);
        list.background = Some(rustim_color::BG);
        list.push(DrawOp::SetPaint(rustim_color::RED));
        list.push(DrawOp::PushClip(rect_path(Rect::new(0.0, 0.0, 8.0, 16.0))));
        list.push(DrawOp::FillPath(rect_path(Rect::new(0.0, 0.0, 16.0, 16.0))));
        list.push(DrawOp::PopClip);
        list.push(DrawOp::SetPaint(rustim_color::BLUE));
        list.push(DrawOp::FillPath(rect_path(Rect::new(
            12.0, 0.0, 16.0, 16.0,
        ))));
        r.render(&list).unwrap();

        assert_eq!(
            &pixel(&r, 4, 8)[..3],
            &[0xEF, 0x53, 0x50],
            "inside the clip"
        );
        assert_eq!(
            &pixel(&r, 10, 8)[..3],
            &[0x12, 0x12, 0x12],
            "past the clip edge the first fill must not reach"
        );
        assert_eq!(
            &pixel(&r, 14, 8)[..3],
            &[0x64, 0xB5, 0xF6],
            "and the fill drawn after PopClip must reach"
        );
    }

    #[test]
    fn a_transparency_group_does_not_change_source_over_composition() {
        // Two half-alpha circles, drawn once inside a `PushLayer` and once
        // outside. Over is associative, so with no alpha on the group itself the
        // two must come out identical — that is the whole content of the claim
        // that a layer bounds compositing, and it is worth pinning because a
        // layer that quietly multiplied in its own alpha, or reset the backdrop,
        // would show up here and nowhere else.
        let mut half = rustim_color::FG;
        half.components[3] = 0.5;
        let pair = |list: &mut DrawList, x: f64| {
            let mut m = BezPath::new();
            m.move_to(Point::new(x, 4.0));
            m.line_to(Point::new(x + 8.0, 4.0));
            m.line_to(Point::new(x + 8.0, 12.0));
            m.line_to(Point::new(x, 12.0));
            m.close_path();
            list.push(DrawOp::FillPath(m));
        };

        let render = |layered: bool| {
            let mut r = CpuRenderer::new(32, 16);
            let mut list = DrawList::new(32, 16);
            list.background = Some(rustim_color::BG);
            list.push(DrawOp::SetPaint(half));
            if layered {
                list.push(DrawOp::PushLayer);
            }
            pair(&mut list, 0.0);
            pair(&mut list, 5.0); // overlaps the first
            if layered {
                list.push(DrawOp::PopLayer);
            }
            r.render(&list).unwrap();
            (pixel(&r, 2, 8), pixel(&r, 6, 8))
        };

        let (plain_single, plain_overlap) = render(false);
        let (group_single, group_overlap) = render(true);
        assert_eq!(
            (group_single, group_overlap),
            (plain_single, plain_overlap),
            "a group with no opacity of its own must be invisible"
        );
        // The two probes really are a single fill and a composite of two, so the
        // equality above is not the trivial equality of a flat image.
        assert!(
            plain_overlap[0] > plain_single[0] + 20,
            "expected the overlap to be brighter than one shape: {plain_overlap:?} vs {plain_single:?}"
        );
    }

    #[test]
    fn a_half_transparent_fill_composites_and_stays_opaque() {
        // Half-alpha red over the near-black background. This is the invariant
        // rustim-app's softbuffer conversion depends on: once the scene has
        // painted an opaque background, every pixel is opaque, so premultiplied
        // and straight alpha coincide and no un-premultiplying is needed.
        let mut r = CpuRenderer::new(8, 8);
        let mut list = DrawList::new(8, 8);
        list.background = Some(rustim_color::BG);
        let mut half = rustim_color::RED;
        half.components[3] = 0.5;
        list.push(DrawOp::SetPaint(half));
        list.push(DrawOp::FillPath(rect_path(Rect::new(0.0, 0.0, 8.0, 8.0))));
        r.render(&list).unwrap();

        let [red, green, _, alpha] = &r.pixels()[0..4] else {
            unreachable!()
        };
        assert_eq!(*alpha, 255, "background must keep the frame opaque");
        assert!(*red > 100 && *red < 230, "expected a blend, got red={red}");
        assert!(*green < 60, "green should stay near the background");
    }

    // ---- incremental rendering -------------------------------------------

    /// A 32x32 frame with two filled squares; `second` is the colour of the
    /// lower-right one, so changing it damages only that corner.
    fn two_square_frame(second: rustim_color::Srgb) -> DrawList {
        let mut list = DrawList::new(32, 32);
        list.background = Some(rustim_color::BG);
        list.push(DrawOp::SetTransform(Affine::IDENTITY));
        list.push(DrawOp::SetPaint(rustim_color::BLUE));
        list.push(DrawOp::FillPath(rect_path(Rect::new(2.0, 2.0, 8.0, 8.0))));
        list.push(DrawOp::SetPaint(second));
        list.push(DrawOp::FillPath(rect_path(Rect::new(
            20.0, 20.0, 28.0, 28.0,
        ))));
        list
    }

    /// The property the whole feature rests on: a region render produces the
    /// same pixels a full render would, everywhere.
    #[test]
    fn a_region_render_matches_a_full_render_pixel_for_pixel() {
        let old = two_square_frame(rustim_color::RED);
        let new = two_square_frame(rustim_color::YELLOW);
        let damage = new.damage_since(&old);
        assert!(
            matches!(damage, Damage::Region(_)),
            "expected a region, got {damage:?}"
        );

        let mut incremental = CpuRenderer::new(32, 32);
        incremental.render(&old).unwrap();
        let stats = incremental.render_damaged(&new, damage).unwrap();
        // The acceptance criterion as an assertion: the repainted area is
        // visible, and it shrank.
        assert!(!stats.is_full(), "{stats:?}");
        assert!(stats.painted * 4 < stats.frame, "{stats:?}");

        let mut full = CpuRenderer::new(32, 32);
        full.render(&new).unwrap();
        assert_eq!(
            incremental.pixels(),
            full.pixels(),
            "an incremental frame must be indistinguishable from a full one"
        );
    }

    /// A rotated shape is the case where "translate the scene by the region
    /// origin" could plausibly move an antialiased edge by a fraction of a
    /// pixel: every coverage value is computed from fractional coordinates, and
    /// a region render sees the same geometry at a different offset. It must
    /// not — the shift is an integer, so the coverage of every pixel inside the
    /// region is bit-identical to the full render's.
    #[test]
    fn a_region_render_of_a_rotated_shape_matches_a_full_one_exactly() {
        fn frame(colour: rustim_color::Srgb) -> DrawList {
            let mut list = DrawList::new(32, 32);
            list.background = Some(rustim_color::BG);
            list.push(DrawOp::SetTransform(
                Affine::translate((16.0, 16.0)) * Affine::rotate(0.37),
            ));
            list.push(DrawOp::SetPaint(colour));
            list.push(DrawOp::FillPath(rect_path(Rect::new(-9.0, -9.0, 9.0, 9.0))));
            list
        }

        let old = frame(rustim_color::RED);
        let new = frame(rustim_color::YELLOW);
        let damage = new.damage_since(&old);
        assert!(matches!(damage, Damage::Region(_)), "{damage:?}");

        let mut incremental = CpuRenderer::new(32, 32);
        incremental.render(&old).unwrap();
        incremental.render_damaged(&new, damage).unwrap();

        let mut full = CpuRenderer::new(32, 32);
        full.render(&new).unwrap();
        assert_eq!(
            incremental.pixels(),
            full.pixels(),
            "an integer shift must not change antialiased coverage"
        );
    }

    #[test]
    fn an_unchanged_frame_does_not_touch_the_pixels() {
        let frame = two_square_frame(rustim_color::RED);
        let mut r = CpuRenderer::new(32, 32);
        r.render(&frame).unwrap();
        let before = r.pixels().to_vec();
        let stats = r
            .render_damaged(&frame, frame.damage_since(&frame))
            .unwrap();
        assert_eq!(stats.painted, 0);
        assert_eq!(r.pixels(), &before[..]);
    }

    #[test]
    fn a_region_erases_an_object_that_left_it() {
        // The scratch target is reused, so a region render must clear it: a
        // full render would not show the deleted square either.
        let old = two_square_frame(rustim_color::RED);
        let mut new = old.clone();
        new.ops.pop();
        new.ops.pop();
        let damage = new.damage_since(&old);
        assert!(matches!(damage, Damage::Region(_)), "{damage:?}");

        let mut incremental = CpuRenderer::new(32, 32);
        incremental.render(&old).unwrap();
        incremental.render_damaged(&new, damage).unwrap();

        let mut full = CpuRenderer::new(32, 32);
        full.render(&new).unwrap();
        assert_eq!(incremental.pixels(), full.pixels());
    }

    #[test]
    fn a_region_crossing_the_frame_edge_is_clipped_not_wrapped() {
        let ops = |c: rustim_color::Srgb| {
            vec![
                DrawOp::SetTransform(Affine::IDENTITY),
                DrawOp::SetPaint(c),
                DrawOp::FillPath(rect_path(Rect::new(26.0, 26.0, 36.0, 36.0))),
            ]
        };
        let mut old = DrawList::new(32, 32);
        old.background = Some(rustim_color::BG);
        old.ops = ops(rustim_color::RED);
        let mut new = old.clone();
        new.ops = ops(rustim_color::YELLOW);
        let damage = new.damage_since(&old);
        let Damage::Region(r) = damage else {
            panic!("expected a region, got {damage:?}");
        };
        assert_eq!((r.x1, r.y1), (32, 32), "the region must stop at the frame");

        let mut incremental = CpuRenderer::new(32, 32);
        incremental.render(&old).unwrap();
        incremental.render_damaged(&new, damage).unwrap();
        let mut full = CpuRenderer::new(32, 32);
        full.render(&new).unwrap();
        assert_eq!(incremental.pixels(), full.pixels());
    }

    #[test]
    fn a_region_that_covers_the_frame_takes_the_full_path() {
        let old = two_square_frame(rustim_color::RED);
        let mut new = old.clone();
        new.background = Some(rustim_color::BLUE);
        let mut r = CpuRenderer::new(32, 32);
        r.render(&old).unwrap();
        // A full-frame rectangle is legal even though `damage_since` would have
        // said `Full`; it must render, not be treated as empty.
        let stats = r
            .render_damaged(&new, Damage::Region(PixelRect::frame(32, 32)))
            .unwrap();
        assert!(stats.is_full());
        let mut full = CpuRenderer::new(32, 32);
        full.render(&new).unwrap();
        assert_eq!(r.pixels(), full.pixels());
    }
}
