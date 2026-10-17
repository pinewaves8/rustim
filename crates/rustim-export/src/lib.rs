//! Deterministic offscreen rendering, the pluggable video sink, and the pixel
//! comparison that turns "the two frames look the same" into numbers a test can
//! assert on.
//!
//! # Why encoding is a sink and not a call to `ffmpeg`
//!
//! `ffmpeg` is absent on a stock macOS and Windows, so a hard dependency on the
//! binary would make the engine's headline output work on exactly the platform
//! it was developed on. [`video::VideoSink`] is the seam: [`video::FfmpegSink`]
//! pipes raw frames to the binary when it exists, and
//! [`video::PngSequenceSink`] writes numbered PNGs with the encoder this crate
//! already ships — the fallback that needs no external program, and the
//! artifact a determinism claim can actually be made about (an mp4's bytes move
//! with the encoder's version; the frame stream does not).
//!
//! # Determinism
//!
//! The exporter must not read wall-clock time anywhere, so the same scene and
//! the same frame count give byte-identical *frames* on every run. The app's
//! export loop drives the scene with a fixed `dt` for exactly that reason; the
//! renderer's `f32_pipeline` is the reproducible one.

pub mod compare;
pub mod srt;
pub mod video;

pub use compare::{Bbox, ImageDiff, Tolerance, compare_png_files};
pub use srt::Cue;
pub use video::{FfmpegSink, PngSequenceSink, VideoSink};

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
    let straight = straight_rgba(r.pixels());
    write_rgba_png(&straight, u32::from(r.width()), u32::from(r.height()), path)
}

/// Un-premultiply an RGBA8 frame.
///
/// `a == 0` is left as it is (there is no colour to recover), and `a == 255` is
/// copied without arithmetic, which is the whole frame in practice.
pub fn straight_rgba(premultiplied: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(premultiplied.len());
    for px in premultiplied.as_chunks::<4>().0 {
        let a = px[3];
        if a == 0 || a == 255 {
            rgba.extend_from_slice(px);
        } else {
            let un =
                |c: u8| ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8;
            rgba.extend_from_slice(&[un(px[0]), un(px[1]), un(px[2]), a]);
        }
    }
    rgba
}

/// Encode one already-straight RGBA8 frame as a PNG.
///
/// Separate from [`write_png`] because the sequence sink holds raw pixels, not
/// a renderer. The length must be exactly `width * height * 4`.
pub fn write_rgba_png(
    rgba: &[u8],
    width: u32,
    height: u32,
    path: impl AsRef<Path>,
) -> std::io::Result<()> {
    let expected = width as usize * height as usize * 4;
    if rgba.len() != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "frame is {} bytes, expected {expected} for {width}x{height} RGBA",
                rgba.len()
            ),
        ));
    }
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(BufWriter::new(file), width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc
        .write_header()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    writer
        .write_image_data(rgba)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(())
}

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
