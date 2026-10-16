//! Deterministic offscreen rendering and the ffmpeg pipe.
//!
//! Two things are written so far: PNG encoding, and the pixel comparison that
//! turns "the two frames look the same" into numbers a test can assert on.

pub mod compare;

pub use compare::{Bbox, ImageDiff, Tolerance, compare_png_files};

use std::io::BufWriter;
use std::path::Path;

use rustim_render::Renderer;

/// Write a renderer's current pixels to a PNG.
///
/// The renderer's buffer is premultiplied RGBA8; PNG stores straight alpha, so
/// this unpremultiplies on the way out. For an opaque frame the two are equal
/// and the loop is a no-op — and every frame this engine draws for a 3b1b-style
/// scene is opaque, since the background is filled first.
pub fn write_png<R: Renderer + ?Sized>(r: &R, path: impl AsRef<Path>) -> std::io::Result<()> {
    let (w, h) = (u32::from(r.width()), u32::from(r.height()));
    let src = r.pixels();
    let mut rgba = Vec::with_capacity(src.len());
    for px in src.as_chunks::<4>().0 {
        let a = px[3];
        if a == 0 || a == 255 {
            rgba.extend_from_slice(px);
        } else {
            let un =
                |c: u8| ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8;
            rgba.extend_from_slice(&[un(px[0]), un(px[1]), un(px[2]), a]);
        }
    }

    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc
        .write_header()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    writer
        .write_image_data(&rgba)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(())
}

// Deterministic video export (fixed timestep, offscreen target, ffmpeg pipe) is
// Phase 4 work. Notes that constrain it, so they are not rediscovered:
//
// - ffmpeg is absent on a stock macOS and Windows, so it must be a pluggable
//   sink rather than a hard dependency — the format writers are the fallback.
// - Determinism means the exporter must not read wall-clock time anywhere, so
//   the same scene and the same frame count must give byte-identical output on
//   every platform. That is also why the `f32_pipeline` path is the export
//   renderer: it is the reproducible one.

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_render::{DrawList, Renderer};

    #[test]
    fn writes_a_readable_png() {
        let mut r = rustim_render::CpuRenderer::new(8, 8);
        let mut list = DrawList::new(8, 8);
        list.background = Some(rustim_color::RED);
        r.render(&list).unwrap();

        let path = std::env::temp_dir().join("rustim-export-test.png");
        write_png(&r, &path).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..8], b"\x89PNG\r\n\x1a\n");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_zero_sized_renderer_is_an_error_not_a_panic() {
        let mut r = rustim_render::CpuRenderer::new(1, 1);
        r.resize(0, 0);
        let err = r.render(&DrawList::new(0, 0));
        assert!(err.is_err());
    }
}
