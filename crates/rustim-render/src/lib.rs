//! The `Renderer` trait and its backends.
//!
//! # Why this is a trait and not "just call vello"
//!
//! The two vello backends we target expose *identical* scene APIs — the same
//! `new` / `reset` / `fill_path` / `stroke_path` / `set_paint` / `set_transform`
//! / `push_layer` / `glyph_run` method set, one on `vello_cpu::RenderContext`
//! and one on `vello_gpu::Scene`. (Verified against 0.3.0 during Phase 0.) So
//! the abstraction costs almost nothing to maintain, while letting a scene be
//! replayed into a software rasteriser on a machine with no usable GPU and into
//! a compute-driven one where there is. See `方案建议.md` §3.1.
//!
//! Backends consume an immutable [`DrawList`]. The scene layer never talks to
//! vello directly, which keeps the semantic kernel testable without a
//! rasteriser and keeps the exporter and the live window sharing one path.

pub mod draw;
pub mod vello_cpu_backend;

pub use draw::{Damage, DrawList, DrawOp, PixelRect};
/// Re-exported because it is a [`DrawOp`] payload: a caller building a glyph run
/// should not have to reach into `rustim-math` for the type the field is.
pub use rustim_math::TextGlyph;
pub use vello_cpu_backend::CpuRenderer;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("render target is {0}x{1}, which is not a valid size")]
    BadSize(u16, u16),
    #[error("no usable rendering backend: {0}")]
    NoBackend(String),
    /// Glyphs in a [`DrawOp::GlyphRun`] that the backend could not draw.
    ///
    /// A value rather than a panic, and reported rather than swallowed: the
    /// glyphs that *could* be drawn are still drawn (the backend's contract), so
    /// this describes a frame that is missing something rather than a frame that
    /// failed. `index` is the position in the run's glyph array, which is what
    /// points back at the character.
    ///
    /// Absent characters — the common case in a CJK scene where a face is
    /// missing — are caught earlier, by `rustim-text` at shaping time, where the
    /// offending *character* is still known. By here they are drawn as blank or
    /// as `.notdef`, which is why the check has to exist upstream.
    #[error("{count} glyph(s) could not be drawn; first at index {index} of its run")]
    GlyphsSkipped { index: usize, count: usize },
}

/// What a render actually did.
///
/// `painted` is the point of the type: it is the area the rasteriser had to
/// compute, not the frame's area, and a test can assert that it fell. Frame
/// time is the wrong witness — it moves with the machine, the clock and the
/// scheduler — while this number is the thing the damage rectangle controls.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RenderStats {
    /// Pixels rasterised by this call.
    pub painted: u64,
    /// Pixels in the whole frame.
    pub frame: u64,
}

impl RenderStats {
    /// Everything was repainted.
    pub fn is_full(&self) -> bool {
        self.painted == self.frame
    }
}

/// A thing that can turn a [`DrawList`] into pixels.
///
/// Deliberately synchronous and sans I/O. Presenting those pixels to a window is
/// the app layer's job, and encoding them to a file is the export layer's.
pub trait Renderer {
    fn resize(&mut self, width: u16, height: u16);
    fn width(&self) -> u16;
    fn height(&self) -> u16;

    /// Rasterise `list` into this renderer's own pixel buffer.
    fn render(&mut self, list: &DrawList) -> Result<(), RenderError>;

    /// Rasterise only what [`DrawList::damage_since`] says changed.
    ///
    /// The default is correct for any backend that cannot cut out a region: it
    /// renders the whole list and reports the whole frame. `Damage::None` is
    /// honoured everywhere, and is the case that matters even for a backend
    /// that cannot do regions — an idle frame costs nothing.
    fn render_damaged(
        &mut self,
        list: &DrawList,
        damage: Damage,
    ) -> Result<RenderStats, RenderError> {
        let frame = u64::from(list.width) * u64::from(list.height);
        if damage == Damage::None {
            return Ok(RenderStats { painted: 0, frame });
        }
        self.render(list)?;
        Ok(RenderStats {
            painted: frame,
            frame,
        })
    }

    /// The rendered pixels: RGBA8, **premultiplied** alpha, tightly packed.
    fn pixels(&self) -> &[u8];
}

/// Un-premultiply an RGBA8 frame into a new buffer.
///
/// The conversion lives beside [`Renderer::pixels`] because it is a property of
/// *that* format, not of any particular consumer — the PNG encoder and the
/// browser canvas both need straight alpha and neither should own the rule.
/// `a == 0` is left as it is (there is no colour to recover), and `a == 255` is
/// copied without arithmetic, which is the whole frame in practice.
pub fn straight_rgba(premultiplied: &[u8]) -> Vec<u8> {
    let mut rgba = premultiplied.to_vec();
    straight_rgba_in_place(&mut rgba);
    rgba
}

/// [`straight_rgba`] without the allocation, for a buffer a host reuses.
///
/// The browser needs this shape: a 1280x720 frame is 3.7 MB, and allocating one
/// per presented frame at 60 fps is pure churn when the destination buffer is
/// the same size every time.
pub fn straight_rgba_in_place(rgba: &mut [u8]) {
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = px[3];
        if a == 0 || a == 255 {
            continue;
        }
        let un = |c: u8| ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8;
        px[0] = un(px[0]);
        px[1] = un(px[1]);
        px[2] = un(px[2]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canvas and the PNG encoder both depend on this conversion, so it is
    /// pinned here rather than only where it is consumed: a half-covered pixel
    /// recovers its colour, a fully transparent one is left alone (there is
    /// nothing to recover), and an opaque one is untouched by definition.
    #[test]
    fn straight_alpha_recovers_the_colour() {
        let mut px = vec![
            64, 32, 0, 128, // premultiplied half-coverage -> straight (128, 64, 0)
            10, 20, 30, 0, // transparent: unchanged
            1, 2, 3, 255, // opaque: unchanged
        ];
        straight_rgba_in_place(&mut px);
        assert_eq!(px, vec![128, 64, 0, 128, 10, 20, 30, 0, 1, 2, 3, 255]);
        // And the allocating wrapper is the same function: applying it twice is
        // *not* idempotent (the output is straight, not premultiplied), which is
        // exactly why the two entry points are named for their input.
        assert_eq!(
            straight_rgba(&[64, 32, 0, 128, 1, 2, 3, 255]),
            vec![128, 64, 0, 128, 1, 2, 3, 255]
        );
    }

    #[test]
    fn cpu_backend_renders_a_background() {
        let mut r = CpuRenderer::new(16, 16);
        let mut list = DrawList::new(16, 16);
        list.background = Some(rustim_color::BLUE);
        r.render(&list).unwrap();
        let px = r.pixels();
        assert_eq!(px.len(), 16 * 16 * 4);
        // Opaque background means premultiplied == straight, so we can compare.
        assert_eq!(&px[0..3], &[0x64, 0xB5, 0xF6]);
        assert_eq!(px[3], 255);
    }

    #[test]
    fn resize_keeps_buffer_consistent() {
        let mut r = CpuRenderer::new(16, 16);
        r.resize(32, 8);
        assert_eq!((r.width(), r.height()), (32, 8));
        let list = DrawList::new(32, 8);
        r.render(&list).unwrap();
        assert_eq!(r.pixels().len(), 32 * 8 * 4);
    }

    #[test]
    fn empty_draw_list_is_fine() {
        let mut r = CpuRenderer::new(4, 4);
        r.render(&DrawList::new(4, 4)).unwrap();
    }
}
