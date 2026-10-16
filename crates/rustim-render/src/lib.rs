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

pub use draw::{DrawList, DrawOp};
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

    /// The rendered pixels: RGBA8, **premultiplied** alpha, tightly packed.
    fn pixels(&self) -> &[u8];
}

#[cfg(test)]
mod tests {
    use super::*;

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
