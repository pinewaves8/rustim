//! Positioned glyphs: the one piece of text data two layers have to agree on.
//!
//! Shaped text is produced by `rustim-text` (parley) and drawn by
//! `rustim-render` (`vello_cpu`, whose glyph runs go through glifo). Neither may
//! depend on the other: the shaping crate would drag a rasteriser into the
//! exporter and the AI layer, and the renderer would drag parley into the
//! `wasm32-unknown-unknown` kernel build, which deliberately leaves `rustim-text`
//! out. So the vocabulary between them lives here — in the crate that has no
//! backend, no platform code and no I/O, which is this workspace's stated remedy
//! for exactly this situation (`rustim-render`'s own note: "if the renderer ever
//! grows a heavier surface, `DrawList` should move down instead").
//!
//! Only the *glyph* is shared. A run also names a font, and a font handle is
//! parley's type on one side and peniko's on the other — the same type in fact,
//! but named through two different crates — so each side carries its own
//! three-field run struct and the app layer converts. That conversion is where
//! the boundary is visible, and a change to either side of it is a compile error
//! rather than a mismatch nobody notices.
//!
//! This is deliberately not `glifo::Glyph`, though the two have the same three
//! fields. The backend's glyph type is `vello_cpu`'s business; naming it in a
//! draw operation would make the draw list — and every reader of one, including
//! the exporter — depend on whichever rasteriser happens to replay it.

/// One glyph, positioned: which glyph, and where to put it.
///
/// The fields are `f32` because both ends of the pipeline are — parley positions
/// glyphs in `f32` and the backend consumes `f32` — so `f64` here would buy
/// precision nobody can see at a font size and add a conversion on the only path
/// that has one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextGlyph {
    /// The glyph index *within the font*, not a Unicode code point. Two
    /// characters can share one glyph and one character can have several, which
    /// is why shaping has to happen before this type can be built at all.
    pub id: u32,
    /// Horizontal position, in the space the surrounding transform establishes.
    pub x: f32,
    /// Vertical position, likewise — the baseline-relative offset a shaper
    /// reports, not a bounding-box corner.
    pub y: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_glyph_is_a_glyph_id_and_an_offset() {
        // The three fields and their order are the contract with `rustim-text`
        // and the backend; a field added here has to be added at both ends.
        let g = TextGlyph {
            id: 42,
            x: 1.5,
            y: -2.0,
        };
        assert_eq!(g.id, 42);
        assert_eq!(g.x, 1.5);
        assert_eq!(g.y, -2.0);
        assert_eq!(g, g);
    }
}
