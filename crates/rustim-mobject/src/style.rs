//! Per-mobject appearance and dirty tracking.

use rustim_color::Srgb;

/// How a mobject is painted.
///
/// `fill_opacity` and `stroke_opacity` live here rather than being folded into
/// the colours, because Manim treats them as independent knobs and animations
/// like `FadeIn` move the mobject's own opacity while leaving these alone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub fill: Srgb,
    pub fill_opacity: f64,
    pub stroke: Srgb,
    pub stroke_opacity: f64,
    pub stroke_width: f64,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            fill: rustim_color::FG,
            fill_opacity: 1.0,
            stroke: rustim_color::FG,
            stroke_opacity: 0.0,
            stroke_width: 2.0,
        }
    }
}

/// Which caches a change invalidates.
///
/// The frame loop in `方案建议.md` §4.3 rebuilds only the dirty part of the
/// scene, so every mutation has to say what it spoiled. Keeping this as flags
/// (rather than dirtying everything unconditionally) is the difference between
/// the manimgl approach we are copying and the ManimCommunity one we are not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirtyFlags {
    /// Point positions changed; geometry caches and any uploaded buffers are stale.
    pub geometry: bool,
    /// Colour, stroke width or opacity changed.
    pub style: bool,
    /// Draw order or parent/child structure changed.
    pub hierarchy: bool,
    /// What the object *means* changed: its label, or its
    /// [`crate::kind::NodeKind`].
    ///
    /// A flag of its own because none of the other three describes it — a
    /// relabelled square has the same points, the same paint and the same place
    /// in the tree, so a consumer that rebuilds only what `geometry | style |
    /// hierarchy` marks would serve a stale accessibility tree forever. The
    /// pixels are unaffected, which is exactly why this was easy to omit.
    pub semantic: bool,
}

impl DirtyFlags {
    pub const ALL: Self = Self {
        geometry: true,
        style: true,
        hierarchy: true,
        semantic: true,
    };

    #[inline]
    pub fn any(self) -> bool {
        self.geometry || self.style || self.hierarchy || self.semantic
    }
}
