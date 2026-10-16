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
use vello_cpu::{RenderContext, Resources};

use crate::draw::{DrawList, DrawOp};
use crate::{RenderError, Renderer};

/// Rasterises draw lists on the CPU.
pub struct CpuRenderer {
    ctx: RenderContext,
    resources: Resources,
    pixmap: Pixmap,
    width: u16,
    height: u16,
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
            text_error: None,
        }
    }

    /// Replay a draw list into the context without rasterising.
    ///
    /// Split out from [`Renderer::render`] so that the export path can build a
    /// context once and rasterise it at several resolutions.
    pub fn encode(&mut self, list: &DrawList) {
        self.ctx.reset();
        self.text_error = None;
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
                DrawOp::SetTransform(affine) => self.ctx.set_transform(*affine),
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
        if list.width == 0 || list.height == 0 {
            return Err(RenderError::BadSize(list.width, list.height));
        }
        if (list.width, list.height) != (self.width, self.height) {
            self.resize(list.width, list.height);
        }
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
        match self.text_error.take() {
            Some(e) => Err(e),
            None => Ok(()),
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
}
