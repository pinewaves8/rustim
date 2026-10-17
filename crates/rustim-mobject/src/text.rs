//! The geometry of shaped text, carried as data.
//!
//! A text mobject has no point array: its picture is a glyph run, and only a
//! shaper can produce one. The shaper lives in `rustim-text` (which links
//! parley and Typst), and the semantic kernel must never depend on it — so the
//! *result* crosses the boundary as this type, and both the wire-level `add`
//! command and a compiled scene hand the kernel the same shape of data.
//!
//! Everything here is plain data with one shared vocabulary: [`peniko::FontData`]
//! is the font handle the renderer already takes in `DrawOp::GlyphRun`, and
//! [`rustim_math::TextGlyph`] is the glyph record both ends already use. That is
//! why these types can live in the wasm kernel crates without dragging a shaper
//! along: no font is opened here, and no byte is parsed.
//!
//! The coordinates are the *shaper's*: layout space, origin at the block's
//! top-left, y growing down. [`TextBlock::layout_to_local`] is the one conversion
//! into a mobject's local space (centred, y up) — see `rustim-app::text` for why
//! that conversion must live in the transform and never in the glyph positions.

// Re-exported rather than merely imported: a [`TextBlock`] cannot be built
// without the face it names, so a host that constructs one needs this type, and
// re-exporting keeps it the same `peniko` the renderer will read it back with.
pub use peniko::FontData;
use rustim_math::{Affine2, Rect, TextGlyph};

/// One run of glyphs sharing a face and a size, positioned in layout space.
///
/// The fields are deliberately the same three the renderer's `DrawOp::GlyphRun`
/// carries, so the draw walk is a field-for-field projection rather than a
/// translation with rules of its own.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    pub font: FontData,
    /// Em size in world units — the units the run is drawn in under the
    /// viewport transform. See `rustim_text::TextRun::size`.
    pub size: f32,
    pub glyphs: Vec<TextGlyph>,
}

/// A shaped block: the layout box plus the runs that fill it.
#[derive(Debug, Clone, PartialEq)]
pub struct TextBlock {
    /// The laid-out width in world units.
    pub width: f32,
    /// The laid-out height in world units.
    pub height: f32,
    pub runs: Vec<TextRun>,
}

impl TextBlock {
    pub fn new(width: f32, height: f32, runs: Vec<TextRun>) -> Self {
        Self {
            width,
            height,
            runs,
        }
    }

    /// Whether the block would paint nothing — an empty string, or a string
    /// whose every character was missing from the font.
    ///
    /// A block that draws nothing is still a legitimate mobject (a reader is
    /// told its label), but the draw walk skips it rather than telling the
    /// backend about empty runs.
    pub fn is_empty(&self) -> bool {
        self.runs.iter().all(|run| run.glyphs.is_empty())
    }

    /// Layout space → the mobject's local space. See [`layout_to_local`].
    pub fn layout_to_local(&self) -> Affine2 {
        layout_to_local(self.width, self.height)
    }

    /// The block's layout box in local coordinates, or `None` when it has no
    /// extent.
    ///
    /// This is a **line box, not an ink box** — a descender or an italic
    /// overhang can paint outside it. It is what `Mobject::local_bounds` wants:
    /// a centre to scale and rotate about, and a box for `camera focus`.
    pub fn local_bounds(&self) -> Option<Rect> {
        let (w, h) = (f64::from(self.width), f64::from(self.height));
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        Some(Rect::new(-w / 2.0, -h / 2.0, w / 2.0, h / 2.0))
    }
}

/// The conversion from the shaper's layout space to a mobject's local space.
///
/// Layout space has its origin at the block's top-left and `y` growing down;
/// local space is centred on the origin with `y` growing up, so that a block
/// scales and rotates about its middle like every other shape in the engine.
///
/// Written as a translation composed with a flip rather than as two statements
/// about each coordinate because that is what it is, and because the order
/// matters: flipping first puts the block's top-left at `(-width/2, +height/2)`,
/// which is the corner a caller would expect to find it in.
pub fn layout_to_local(width: f32, height: f32) -> Affine2 {
    Affine2::translate((-f64::from(width) / 2.0, f64::from(height) / 2.0))
        * Affine2::scale_non_uniform(1.0, -1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_math::Point;

    fn block(width: f32, height: f32) -> TextBlock {
        TextBlock::new(width, height, Vec::new())
    }

    /// The corner rule: flipping first puts the block's top-left where a caller
    /// expects it, and the block is centred.
    #[test]
    fn the_layout_box_is_centred_and_flipped() {
        let b = block(2.0, 1.0);
        let t = b.layout_to_local();
        let top_left = t * Point::new(0.0, 0.0);
        let bottom_right = t * Point::new(2.0, 1.0);
        assert!((top_left.x - -1.0).abs() < 1e-12);
        assert!((top_left.y - 0.5).abs() < 1e-12);
        assert!((bottom_right.x - 1.0).abs() < 1e-12);
        assert!((bottom_right.y - -0.5).abs() < 1e-12);
    }

    #[test]
    fn a_box_with_no_extent_has_no_bounds() {
        assert!(block(0.0, 0.0).local_bounds().is_none());
        assert!(block(2.0, 0.0).local_bounds().is_none());
        let b = block(2.0, 1.0).local_bounds().unwrap();
        assert_eq!((b.x0, b.y0, b.x1, b.y1), (-1.0, -0.5, 1.0, 0.5));
    }

    /// A block whose every run is empty draws nothing, but one non-empty glyph
    /// is enough to make it visible.
    #[test]
    fn emptiness_is_about_glyphs() {
        assert!(block(2.0, 1.0).is_empty());
        let mut b = block(2.0, 1.0);
        b.runs.push(TextRun {
            font: FontData::new(Vec::<u8>::new().into(), 0),
            size: 1.0,
            glyphs: vec![TextGlyph {
                id: 1,
                x: 0.0,
                y: 0.0,
            }],
        });
        assert!(!b.is_empty());
    }
}
