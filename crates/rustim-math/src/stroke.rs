//! How a stroked path is drawn beyond its geometry.
//!
//! A **shared vocabulary**, and it is here for the reason `rustim_math::gesture`
//! is: two crates need to name it and neither can name the other's type. The
//! paint description lives on `rustim_mobject::Style`, the draw op lives in
//! `rustim_render::DrawOp`, and the two do not depend on each other — `rustim-math`
//! is the floor both already stand on.
//!
//! These are not re-inventions of `kurbo`'s `Join` and `Cap`: they are this
//! workspace's spelling of them, so that the mobject and renderer layers can name
//! a join without one of them depending on `kurbo`'s version of the other, and so
//! that the wire's vocabulary is defined here rather than derived from a
//! third-party enum's `Debug` output.

/// How two segments of a stroked path meet.
///
/// **The default is `Round`, and that is not the obvious guess.** `kurbo`'s
/// `Stroke::new` — which is what this engine stroked with until joins and caps
/// were expressible — sets `join: Join::Round`, `start_cap: Cap::Round`,
/// `end_cap: Cap::Round`. Those are *not* `kurbo`'s own enum defaults, which are
/// `Miter` and `Butt`. So writing the vocabulary's `Default` from the enums moved
/// two golden baselines the first time it was tried, which is how the real
/// default was found: every stroke in this engine has always had round joins and
/// round caps, and a vocabulary default has to describe the picture that already
/// exists rather than the one that seemed likely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Join {
    /// Extend the outer edges until they cross, cut off at the miter limit.
    Miter,
    /// Round the corner off with an arc.
    #[default]
    Round,
    /// Cut the corner straight across.
    Bevel,
}

/// How a stroked path ends, at each of its two ends.
///
/// `Round` for `Join`'s reason: it is what `kurbo::Stroke::new` does, so it is
/// what every stroke in this engine has always done. `Butt` stops exactly at the
/// endpoint and `Square` reaches half a width past it — both are things a caller
/// should have to ask for, because a cap changes a figure's silhouette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cap {
    /// Stop at the endpoint.
    Butt,
    /// A semicircle past the endpoint.
    #[default]
    Round,
    /// A half-square past the endpoint.
    Square,
}

/// A stroke's non-geometric description.
///
/// `Copy`, and that is a real constraint rather than an accident: `Style` is the
/// mobject's, and it is copied into every draw op the walk emits. A general dash
/// *pattern* is therefore not expressive here — `dash` is one on/off pair, which
/// is what "a dashed line" means and what a `Copy` type can carry. A caller who
/// needs three-length patterns needs a different type, not a wider field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrokeStyle {
    /// In **world units** — see `Style::stroke_width` for why not pixels.
    pub width: f64,
    pub join: Join,
    pub cap: Cap,
    /// `[on, off]` in world units, or `None` for a solid line.
    ///
    /// Both entries must be finite and positive; a pattern with a zero or negative
    /// entry is not a dashed line, and the command layer refuses one rather than
    /// letting the stroker decide what to do with it.
    pub dash: Option<[f64; 2]>,
}

/// The width a stroked thing gets when nobody named one.
///
/// A **world unit**, not a pixel: `0.045` is what an arrow's silhouette already
/// uses, and the value lives here rather than beside the shapes because it is a
/// stroke's property first and a shape's second — `rustim_mobject::shapes`
/// re-exports it so the two spellings cannot drift.
///
/// Small on purpose: the default stroke is *off*, so this is latent until a host
/// turns `stroke_opacity` up, and a caller who did that without naming a width
/// must get a hairline rather than a two-unit border.
pub const DEFAULT_THICKNESS: f64 = 0.045;

impl Default for StrokeStyle {
    fn default() -> Self {
        Self {
            width: DEFAULT_THICKNESS,
            join: Join::default(),
            cap: Cap::default(),
            dash: None,
        }
    }
}
