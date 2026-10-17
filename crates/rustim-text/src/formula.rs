//! Formulas: Typst in, per-glyph outlines out.
//!
//! `$x^2 + y^2 = c^2$` goes in; a [`Mobject`] holding one child mobject per
//! glyph comes out, each child an ordinary filled path in Manim's control-point
//! layout, so a formula participates in the scene tree exactly like a square:
//! it can be moved for free (the parent's transform is deferred), read by the
//! accessibility layer, addressed by id from the command layer, and morphed.
//!
//! # Why this route, in one paragraph
//!
//! The alternative is `typst-render`, which rasterises the page through
//! tiny-skia and throws the per-glyph vectors away. Per-symbol addressability is
//! the entire point of a formula in an engine an AI drives: "make the `x` red"
//! has to be a command naming an object. So the outlines are read out by hand
//! through `skrifa`, and `typst-render` is never linked — see 方案建议.md §8
//! item 8.
//!
//! # The crate split, which is not the one the tutorial's sketch assumes
//!
//! The four types the pipeline needs are not in one crate, and
//! `typst::PagedDocument` — the obvious spelling — does not exist.
//!
//! ```text
//! type                 owning crate     path
//! ------------------   -------------    -------------------------------------
//! World      (trait)   typst-library    src/lib.rs:60
//! Library              typst-library    src/lib.rs:189 (LibraryBuilder)
//! Frame                typst-library    src/layout/frame.rs:18
//! FrameItem            typst-library    src/layout/frame.rs:486
//! TextItem / Glyph     typst-library    src/text/item.rs:13
//! Font / FontInstance  typst-library    src/text/font/mod.rs:41, 172
//! Output     (trait)   typst-library    src/foundations/target.rs:13
//! PagedDocument        typst-layout     src/document.rs:17
//! compile / trace /    typst (facade)   src/lib.rs:79, 296
//!   LibraryExt::default()
//! ```
//!
//! Three consequences, in the order they bite:
//!
//! 1. **`PagedDocument` is in `typst-layout`, and the facade does not re-export
//!    it.** The facade's public surface is `pub use typst_library::*` plus
//!    `typst_syntax as syntax` and `typst_utils as utils`
//!    (`typst/src/lib.rs:33`–`40`); `typst-layout` is a crate it *calls*, not
//!    one it re-exports. So the compile call names the layout crate directly —
//!    `typst::compile::<typst_layout::PagedDocument>(&world)` — and
//!    `typst-layout` is a direct dependency rather than a transitive one.
//! 2. **The facade cannot be avoided even so.** `Library::default()` is not in
//!    `typst-library`: it is `impl LibraryExt for Library` in the facade
//!    (`typst/src/lib.rs:296`), and it needs the facade's *private*
//!    `static ROUTINES`, the function-pointer table that wires `typst-layout`'s
//!    rules into the library. `typst-library` offers
//!    `LibraryBuilder::from_routines` but no way to build the table.
//! 3. **`Source` and `FileId` are not re-exported by `typst-library` either**
//!    (`typst-library/src/lib.rs:32` imports them privately), so they are named
//!    through `typst::syntax`, which is the facade's re-export of
//!    `typst-syntax`.
//!
//! The three Typst crates this depends on — `typst`, `typst-layout`,
//! `typst-assets` — are pinned together in the root manifest at **0.15.1** and
//! have to move as one, because `typst-layout` is version-locked to the exact
//! `typst-library` the facade re-exports; a partial bump does not resolve. Every
//! `file:line` below is that version's.
//!
//! # `World` in 0.15.1, verbatim
//!
//! ```text
//! #[comemo::track]
//! pub trait World: Send + Sync {
//!     fn library(&self) -> &LazyHash<Library>;
//!     fn book(&self) -> &LazyHash<FontBook>;
//!     fn main(&self) -> FileId;
//!     fn source(&self, id: FileId) -> FileResult<Source>;
//!     fn file(&self, id: FileId) -> FileResult<Bytes>;
//!     fn font(&self, index: usize) -> Option<Font>;
//!     fn today(&self, offset: Option<Duration>) -> Option<Datetime>;
//! }
//! ```
//!
//! (`typst-library/src/lib.rs:60`.) Seven methods, **all required — there are no
//! defaults**, so `FormulaWorld` below supplies every one even though a formula
//! needs only three of them in anger. `today` returns a fixed date: `None` is
//! allowed by the trait but makes anything that asks for the date an error, and
//! the page's `DocumentInfo` date is `auto` — so "no formula needs a clock" is
//! not the same as "nothing will ask".
//!
//! # The pipeline
//!
//! ```text
//! "x^2 + y^2"  →  $x^2 + y^2$  →  compile::<PagedDocument>  →  pages[0].frame
//!              →  Frame::items() →  FrameItem::Text(TextItem)
//!              →  item.font.font().data()  →  skrifa::FontRef::from_index
//!              →  OutlineGlyph::draw(Size::unscaled(), pen)  →  kurbo::BezPath
//!              →  shapes::points_from_path  →  Mobject::points
//!
//!              ⋮ and the same three last steps for a FrameItem::Shape, after
//!                expanding its stroke into the quad that stroke covers
//! ```
//!
//! ## `Glyph::span` is why the route is worth its cost
//!
//! ```text
//! pub struct TextItem {
//!     pub font: FontInstance,   pub size: Abs,        pub fill: Paint,
//!     pub stroke: Option<FixedStroke>,
//!     pub lang: Lang,           pub region: Option<Region>,
//!     pub text: EcoString,      pub glyphs: Vec<Glyph>,
//! }
//! pub struct Glyph {
//!     pub id: u16,
//!     pub x_advance: Em,  pub x_offset: Em,
//!     pub y_advance: Em,  pub y_offset: Em,
//!     pub range: Range<u16>,    pub span: (Span, u16),
//! }
//! ```
//!
//! (`typst-library/src/text/item.rs:13`, `:93`.) Every glyph carries both the
//! character range it was shaped from **and** the source location it came from,
//! so "which symbol is this mobject?" is something Typst hands over rather than
//! something reconstructed by diffing the source against the output. Both end up
//! on [`FormulaGlyph`].
//!
//! Note there is a *second* `TextItem<'a>` in `typst-library/src/math/ir/item.rs`
//! belonging to the math intermediate representation. `FrameItem::Text` holds the
//! one in `text/item.rs`; the other is never seen here.
//!
//! # Coordinates, derived from typst's own rasteriser rather than guessed
//!
//! This is the part that cannot be settled by reading signatures, so it was read
//! out of `typst-render/src/text.rs:17` — not linked, only consulted — and then
//! confirmed against a render. `render_text` is twelve lines and settles all of
//! it:
//!
//! ```text
//! let mut x = Abs::zero(); let mut y = Abs::zero();
//! for glyph in &text.glyphs {
//!     let x_offset = x + glyph.x_offset.at(text.size);
//!     let y_offset = y + glyph.y_offset.at(text.size);
//!     … pre_translate(Point::new(x_offset, -y_offset)).pre_scale(scale, -scale) …
//!     x += glyph.x_advance.at(text.size);
//!     y += glyph.y_advance.at(text.size);
//! }
//! ```
//!
//! Three facts fall out, and each of them is a place to get it wrong:
//!
//! - **The item's `Point` is a plain translation in frame space.** `render_frame`
//!   does `render_text(canvas, state.pre_translate(*pos), text)`, so `pos` is the
//!   run's origin and everything in the run is relative to it.
//! - **The run's internal cursor is y-*up* while the frame is y-down**, which is
//!   why `y_offset` is negated at the point of use. This is the same trap
//!   `crates/rustim-app/src/text.rs` records for `parley`: flip the *position*
//!   and not the *outline* and every glyph is mirrored about its own baseline —
//!   the baselines land in the right place and the letters are upside down,
//!   which reads as a font bug rather than a coordinate one. Here both the
//!   offset and the outline are negated, by the same `pre_scale(scale, -scale)`,
//!   so the two flips are one operation and cannot drift apart.
//! - **The outline's y-flip and the frame's y-down cancel against the mobject's
//!   own y-up local space**, so the net scale a glyph outline sees is a positive
//!   uniform `size / upem`. Worth knowing: a net negative y scale is what drops
//!   a glyph run into `glifo`'s unhinted path for shaped text, and the same
//!   class of sign error is invisible until it is looked at.
//!
//! Glyph outlines are drawn at [`skrifa::instance::Size::unscaled`], i.e. in
//! **font units** with the scale factor 1, and the size is applied afterwards as
//! an affine. That is what makes one cache entry per glyph valid for every size
//! it is used at — the same reason `glifo` absorbs a uniform scale into the font
//! size rather than transforming coordinates twice.
//!
//! # Where the geometry ends up: measured, not derived
//!
//! The mobject's local space is centred on the origin with y up
//! ([`crate::MANIM_FONT_SIZE_UNITS`] fixes 1 world unit = 72 pt), while the frame
//! is y-down from its own top-left corner. The translation between them is
//! **measured from the glyph outlines**, as the union of every placed point,
//! rather than taken from the frame's declared size — the page is `width: auto`
//! and its size would include whatever the layout put around the formula, and
//! hand-deriving a figure's bounds is the mistake `fit_and_centre` in the
//! Pythagorean scene exists to avoid.
//!
//! # Colours are the engine's, deliberately
//!
//! Every glyph is painted in [`rustim_color::FG`], *not* in `TextItem::fill`.
//! Typst's default text colour is black, which is invisible on this engine's
//! `#121212` background: honouring the paint would make every formula that does
//! not name a colour render as nothing at all. A scene that wants a coloured
//! formula sets the style on the returned mobjects, which it can address
//! individually.
//!
//! # Rules: the other half of a formula
//!
//! A formula is not only glyphs. A fraction draws a bar and a radical draws an
//! overline, and Typst describes both as a `FrameItem::Shape` whose geometry is a
//! **stroked** horizontal line — a thing this engine cannot draw, because
//! `SceneContext::build_draw_list` emits `FillPath` only (which is why the
//! `primitives` scene pushes raw `DrawOp`s for strokes and clips). So the stroke
//! is expanded here into the quad it covers, and the result is an ordinary
//! mobject. Without that, `frac(1, 2)` renders as two numerals with a gap where
//! the bar should be — which reads as a typesetting bug, not a missing feature.
//!
//! They land in [`Formula::rules`], children of the formula like the glyphs, so
//! a move still costs one transform. What is *not* solved is what they say to a
//! screen reader: they arrive as unnamed graphics of a stated size, and leaving
//! a fraction bar out of the spoken tree wants a `decorative` flag on `Mobject`
//! that this step did not add. See the field's doc.
//!
//! # Structure: MathML, from the compiler's own IR
//!
//! A glyph tree says *where the symbols are*, and the relation between them
//! lives only in the layout: `x^2 + y^2` and `x2 + y2` draw the same five
//! symbols in different places, and no geometry heuristic recovers "the 2 is an
//! exponent" reliably. Typst knows, because its math IR is built *after*
//! scripts, fractions, fences and radicals are recognised. So the same fragment
//! is compiled a second time against Typst's **HTML** target, whose exporter
//! lowers that IR to MathML, and the `<math>` element is carried on the formula's
//! root mobject ([`Mobject::mathml`]).
//!
//! Three properties of the route are worth knowing:
//!
//! - **It is a second `Library`, not a second compiler.** `compile::<HtmlDocument>`
//!   refuses to run unless Typst's in-development `Feature::Html` is enabled, and
//!   the paged compile has no reason to know the feature exists. Both libraries
//!   come from [`LibraryExt::builder`], which is the same `ROUTINES` table the
//!   facade's `Library::default()` uses, so they cannot disagree about the
//!   language. `typst-html` is already in the graph through the facade; naming it
//!   directly buys the API without adding a compiler.
//! - **The MathML is Typst's, verbatim.** In particular `mi` holds the
//!   *typeset* character, so a math `x` arrives as U+1D465 `𝑥` — the
//!   mathematical italic — where the per-glyph label keeps the author's `x`. That
//!   asymmetry is deliberate: the element is what a MathML renderer draws and a
//!   reader recognises, the label is what the AI layer matches a command against,
//!   and rewriting the element's text would mean owning a Unicode mapping table.
//! - **A failed HTML pass is not a failed formula.** The geometry is the primary
//!   product; a missing structured reading is a gap in what a reader can *hear*,
//!   so the field is `None` rather than an error that would refuse to draw a
//!   formula which is on screen.
//!
//! # What is still not implemented, and is reported rather than dropped
//!
//! Anything else that is neither a glyph outline nor a stroke-to-fill rule is
//! counted in [`Formula::skipped_shapes`] instead of vanishing silently: a
//! `Geometry::Curve` (which needs a real stroker) and `FrameItem::Image`. Also
//! not carried over: per-run colour (above) and link annotations. Group clipping
//! (`GroupItem::clip`) is ignored — math nests groups for layout but does not
//! clip them.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};

use rustim_math::{Affine2, BezPath, Point};
use rustim_mobject::shapes::points_from_path;
use rustim_mobject::{Mobject, NodeId, NodeKind, SceneTree};
use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::layout::{Frame, FrameItem, Point as TypstPoint, Transform};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook, TextItem};
use typst::utils::LazyHash;
use typst::visualize::{Geometry, Shape};
use typst::{Library, LibraryExt, World};
use typst_layout::PagedDocument;

use crate::BUNDLED_DIR;

/// Length of one world unit, in typst points.
///
/// The relation is not a new decision, it is the one
/// [`crate::MANIM_FONT_SIZE_UNITS`] already encodes: that constant says a Manim
/// `font_size` is points with 72 to the world unit, which is why the default
/// typesetting size below is Manim's own default and not a coincidence.
pub const POINTS_PER_WORLD_UNIT: f64 = 72.0;

/// The size a formula is typeset at when nothing else is asked for.
///
/// 48 pt is Manim's `MathTex` default, so a formula written here comes out the
/// size the same formula would in the reference implementation — the test
/// `the_default_font_size_is_the_one_manim_uses` pins the relation to
/// [`crate::MANIM_FONT_SIZE_UNITS`] so the two cannot drift.
pub const DEFAULT_FONT_SIZE_PT: f64 = 48.0;

/// The virtual filename the fragment is compiled under.
///
/// One name for one in-memory file. It matters only because it is what the
/// interner keys the `FileId` on — see [`FormulaCompiler::file_id`].
const SOURCE_NAME: &str = "formula.typ";

/// Extension filter for the bundled fonts, so the directory's licence file is
/// not offered to a font parser.
const FONT_EXTENSIONS: [&str; 3] = ["ttc", "otf", "ttf"];

/// A formula that could not be compiled.
///
/// A value rather than a panic, for the same reason `CommandError` is: this is
/// reachable from model output, and "that formula is not valid Typst" has to be
/// reportable to whoever wrote it.
#[derive(Debug)]
pub enum FormulaError {
    /// Typst reported one or more errors. The messages are joined.
    Compile(String),
    /// The fragment compiled to a document with no pages, which is what an empty
    /// or whitespace-only fragment produces.
    Empty,
    /// The font set could not be assembled — almost always a missing
    /// `assets/fonts/`, reported rather than assumed away for the same reason
    /// [`crate::BUNDLED_DIR`] is.
    Fonts(io::Error),
}

impl fmt::Display for FormulaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compile(message) => write!(f, "formula did not compile: {message}"),
            Self::Empty => f.write_str("formula produced no pages"),
            Self::Fonts(err) => write!(f, "formula fonts could not be loaded: {err}"),
        }
    }
}

impl std::error::Error for FormulaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Fonts(err) => Some(err),
            _ => None,
        }
    }
}

/// One glyph of a compiled formula.
///
/// The mobject is ordinary — filled points, default style, no special case in
/// the draw walk — and the two annotations beside it are what make it
/// addressable: a reader is told which characters it draws, and the AI layer can
/// map it back to the text that produced it.
#[derive(Debug, Clone)]
pub struct FormulaGlyph {
    pub mobject: Mobject,
    /// The characters this glyph draws, as spelling rather than as codepoints.
    ///
    /// Taken from the caller's own fragment when Typst's span for the glyph maps
    /// one-to-one onto what was shaped — so a math `x` reports the `x` the author
    /// typed and not the mathematical-italic `𝑥` the face supplies. When it does
    /// not (`"abc"` shares one span across three glyphs; `pi` is two characters
    /// typeset as one) this is the typeset character instead. More than one
    /// character for a ligature, empty for a glyph Typst inserted.
    ///
    /// This is the field a screen reader reads and a host may match a command
    /// against; [`Self::source`] is the field that names exact bytes.
    pub text: String,
    /// Where those characters were in the *input* string, best effort.
    ///
    /// `None` when Typst's span for the glyph does not resolve to a node — a
    /// detached span, or a glyph the layout synthesised rather than shaped from
    /// source. It is a byte range into the caller's own fragment, with the
    /// prologue and the `$` delimiters this module adds already subtracted, so
    /// `&input[range]` is the source the glyph came from.
    pub source: Option<Range<usize>>,
}

/// A compiled formula, ready to be inserted into a scene tree.
#[derive(Debug, Clone)]
pub struct Formula {
    /// The container: `NodeKind::Formula`, and **no points of its own**.
    ///
    /// The glyphs are its children and carry the geometry, so moving the
    /// formula is one O(1) transform on the parent. `Mobject::has_picture`
    /// already answers `true` for a `Formula` with no points, so the keyboard and
    /// the accessibility tree see it while the draw walk — which can only emit a
    /// filled path — walks past it to its children, which is exactly right.
    pub root: Mobject,
    /// One mobject per *inked* glyph, in draw order.
    ///
    /// A glyph with no outline is absent: a space has no curves, and a mobject
    /// with no points would be an invisible node in the accessibility tree
    /// claiming to be a symbol.
    pub glyphs: Vec<FormulaGlyph>,
    /// Geometry that is not a glyph: a fraction's rule, a radical's overline.
    ///
    /// Ordinary mobjects with no label, because there are no characters to name
    /// them with, and children of the formula like the glyphs are — so moving a
    /// formula is still one transform, and a rule that was drawn the first time
    /// is still drawn after a `MoveTo`.
    ///
    /// They carry [`NodeKind::Unknown`], which the accessibility layer reads as
    /// an unnamed graphic of a stated size. That is truthful — a fraction bar
    /// *is* an unnamed graphic — but it is not what a reader wants from a
    /// formula, and the honest fix is a `decorative` flag on `Mobject` that keeps
    /// a piece of a picture out of the spoken tree without making it invisible.
    /// That is a kernel-model change and belongs in its own step.
    pub rules: Vec<Mobject>,
    /// Frame items that were neither a glyph outline nor a rule, and so drew
    /// nothing.
    ///
    /// Non-zero means the formula is incomplete on screen. It is rare now that
    /// rules are drawn: it takes a `Geometry::Curve`, which needs a stroke
    /// expanded into an outline, or an image. Checked rather than assumed to be
    /// zero, because a silent gap in a picture is indistinguishable from a
    /// typesetting mistake.
    pub skipped_shapes: usize,
}

/// The tree ids a formula was inserted under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormulaNodes {
    pub root: NodeId,
    pub glyphs: Vec<NodeId>,
    pub rules: Vec<NodeId>,
}

impl Formula {
    /// Insert the formula into a tree, returning the ids it was given.
    ///
    /// The glyphs are inserted first and then reparented, because
    /// `SceneTree::insert` always puts a node in the root bucket and there is no
    /// insert-under-parent. Afterwards the formula is one subtree under one id,
    /// which is what the AI layer wants to name and what keeps a move from
    /// costing O(glyphs).
    pub fn insert(self, tree: &mut SceneTree) -> FormulaNodes {
        // Computed before anything is moved out of `self`: the union is over the
        // glyphs and rules, and after the first insertion they belong to the
        // tree. The container itself has no points, so this box is the only
        // extent it will ever have — it is what gives the formula a `bounds` in
        // the accessibility tree and a centre for `camera focus` / `MoveTo`.
        let bounds = self.local_bounds();
        let mut root = self.root;
        root.local_bounds = bounds;
        let root = tree.insert(root);
        let adopt = |tree: &mut SceneTree, mobject| {
            let id = tree.insert(mobject);
            // **`reparent` must be called unconditionally, with its result
            // checked separately.** `debug_assert!` does not evaluate its
            // argument in a release profile, so `debug_assert!(tree.reparent(..))`
            // — the spelling that reads best — attaches every glyph to the root
            // bucket instead of to the formula, in release builds and *only*
            // there. That has now cost this function twice; the symptom is that
            // the whole formula ignores its own transform and renders on top of
            // every other formula, which looks like a layout bug rather than a
            // parenting one.
            let reparented = tree.reparent(id, root);
            // Can only fail for unknown ids or a cycle, and neither is possible
            // here: the node was inserted a line ago and a fresh node has no
            // ancestors.
            debug_assert!(reparented, "a freshly inserted child reparents");
            id
        };

        let mut ids = Vec::with_capacity(self.glyphs.len());
        for glyph in self.glyphs {
            ids.push(adopt(tree, glyph.mobject));
        }
        let rules = self.rules.into_iter().map(|m| adopt(tree, m)).collect();
        FormulaNodes {
            root,
            glyphs: ids,
            rules,
        }
    }

    /// The formula's structure as a `<math>` element, if it could be produced.
    ///
    /// Read through the root mobject: the structure travels with the object, so
    /// a formula that has been inserted into a tree — and therefore copied,
    /// morphed, re-parented or snapshotted — does not depend on the `Formula`
    /// value that built it still being alive. See [`Mobject::mathml`].
    pub fn mathml(&self) -> Option<&str> {
        self.root.mathml.as_deref()
    }

    /// The union of everything the formula draws, in its local space.
    pub fn local_bounds(&self) -> Option<rustim_math::Rect> {
        let mut bounds: Option<rustim_math::Rect> = None;
        let pieces = self
            .glyphs
            .iter()
            .map(|g| &g.mobject)
            .chain(self.rules.iter());
        for mobject in pieces {
            for point in &mobject.points {
                let rect = rustim_math::Rect::new(point.x, point.y, point.x, point.y);
                bounds = Some(match bounds {
                    Some(acc) => rustim_math::Rect::new(
                        acc.x0.min(rect.x0),
                        acc.y0.min(rect.y0),
                        acc.x1.max(rect.x1),
                        acc.y1.max(rect.y1),
                    ),
                    None => rect,
                });
            }
        }
        bounds
    }
}

/// A compiler for `$…$` fragments, and the caches that make it affordable.
///
/// Building one is the expensive step: it parses the whole embedded Typst font
/// set plus the bundled CJK face, and builds Typst's standard library. Both are
/// then reused for every formula, and the glyph outline cache grows as glyphs are
/// seen. One per scene, or one per host process.
pub struct FormulaCompiler {
    library: LazyHash<Library>,
    /// The same library with Typst's in-development **HTML** feature enabled,
    /// for the MathML pass.
    ///
    /// A second `Library` rather than one with the feature on, because
    /// `typst::compile::<HtmlDocument>` refuses to run unless `Feature::Html` is
    /// on and the paged compile has no reason to know the feature exists. Both
    /// libraries are built from the same private `ROUTINES` table — that is what
    /// `Library::builder()` is for — so they cannot disagree about the language.
    html_library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    /// The book's index is this vector's index — `FontBook::from_fonts` pushes
    /// `FontInfo`s in iteration order, so `World::font(index)` and
    /// `book.info(index)` agree. That is worth stating because the same
    /// assumption is *false* for `fontique`, where a `.ttc` picks a different
    /// face per process (see `crate::FontProvider::register`); Typst sidesteps it
    /// instead by making the face index explicit, and `FontBook`'s family map is
    /// a `BTreeMap` rather than a `HashMap`.
    fonts: Vec<Font>,
    /// Font-space outlines, one per glyph. See [`FormulaCompiler::outline`].
    outlines: HashMap<GlyphKey, BezPath>,
    font_size_pt: f64,
}

/// Identifies an outline: the face's bytes by address, its face index, and the
/// glyph.
///
/// The bytes' address stands in for the font's identity because `Font` is an
/// `Arc` wrapper with no `Eq`, and by-address is strictly the right question here
/// — two `Font`s that share a blob *are* the same face. The index separates the
/// faces of a `.ttc`, which do share a blob. Every `Font` the cache is keyed on
/// is kept alive by [`FormulaCompiler::fonts`] for the compiler's lifetime, so
/// the address is stable and cannot be recycled.
///
/// **The size is deliberately not part of the key.** Outlines are drawn at
/// [`skrifa::instance::Size::unscaled`], i.e. in font units, so one entry serves
/// every size the glyph is used at; the size is applied afterwards, as an affine.
type GlyphKey = (usize, u32, u16);

/// The fonts compiled into `typst-assets`, which travel with the binary.
///
/// Shared by both constructors so the embedded half cannot differ between a
/// desktop build and a browser build — a formula that compiled in one and not
/// the other would be a host-dependent language.
fn embedded_fonts() -> Vec<Font> {
    let mut fonts = Vec::new();
    for data in typst_assets::fonts() {
        fonts.extend(Font::iter(Bytes::new(data)));
    }
    fonts
}

impl FormulaCompiler {
    /// Load the fonts and build the library.
    ///
    /// Reads every `.ttc`/`.otf`/`.ttf` in [`BUNDLED_DIR`] — the same directory
    /// [`crate::FontProvider::with_bundled`] uses, and for the same reason: a
    /// bare Linux has no CJK font installed, and a formula with Chinese in it
    /// would otherwise render as a run of `MissingGlyph`s. `BUNDLED_DIR` is
    /// resolved against `CARGO_MANIFEST_DIR`, so this is right for a checkout,
    /// `cargo test` and `cargo run`, and wrong for an installed binary — hence
    /// the error rather than a silent fallback to a system path.
    pub fn new() -> Result<Self, FormulaError> {
        let mut fonts: Vec<Font> = Vec::new();

        // The embedded set, which is where New Computer Modern Math lives. Math
        // cannot be typeset at all without it, which is why `typst-assets` is
        // pinned with its `fonts` feature in the root manifest — the feature is
        // off by default and `typst_assets::fonts()` then returns nothing.
        fonts.extend(embedded_fonts());

        let dir = Path::new(BUNDLED_DIR);
        for path in bundled_font_paths(dir)? {
            let bytes = std::fs::read(&path).map_err(FormulaError::Fonts)?;
            fonts.extend(Font::iter(Bytes::new(bytes)));
        }

        if fonts.is_empty() {
            return Err(FormulaError::Fonts(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no fonts found in {BUNDLED_DIR} or in typst-assets"),
            )));
        }

        Ok(Self::with_fonts(fonts))
    }

    /// Load the embedded fonts only — no filesystem, so this is the wasm entry.
    ///
    /// [`new`](Self::new) reads [`BUNDLED_DIR`] and reports its absence on
    /// purpose: a desktop install that lost the directory would otherwise render
    /// Chinese as a run of `.notdef` with no error anywhere. A browser has no
    /// directory at all, and `typst-assets` already carries the math faces a
    /// formula needs, so this constructor takes exactly the half that travels in
    /// the binary. A **CJK formula on the web** therefore needs fonts this
    /// constructor does not have; that is the recorded font-subsetting cost, not
    /// something a different constructor would fix.
    pub fn from_embedded() -> Result<Self, FormulaError> {
        let fonts = embedded_fonts();
        if fonts.is_empty() {
            return Err(FormulaError::Fonts(io::Error::new(
                io::ErrorKind::NotFound,
                "typst-assets was built without its `fonts` feature",
            )));
        }
        Ok(Self::with_fonts(fonts))
    }

    /// The expensive half, shared by both constructors.
    fn with_fonts(fonts: Vec<Font>) -> Self {
        Self {
            library: LazyHash::new(Library::default()),
            html_library: LazyHash::new(
                Library::builder()
                    .with_features(typst::Features::from_iter([typst::Feature::Html]))
                    .build(),
            ),
            book: LazyHash::new(FontBook::from_fonts(&fonts)),
            fonts,
            outlines: HashMap::new(),
            font_size_pt: DEFAULT_FONT_SIZE_PT,
        }
    }

    /// Typeset at a different size. Manim's `MathTex(..., font_size=n)`.
    pub fn with_font_size(mut self, font_size_pt: f64) -> Self {
        self.font_size_pt = font_size_pt;
        self
    }

    /// How many glyph outlines are cached. Mostly a test hook.
    pub fn cached_outlines(&self) -> usize {
        self.outlines.len()
    }

    /// How many fonts the world can offer. A formula's CJK works only because
    /// this is larger than the embedded set.
    pub fn font_count(&self) -> usize {
        self.fonts.len()
    }

    /// The id of the one virtual file. Interned, so the same name is the same id
    /// every call — which is what lets `World::main` and `World::source` agree
    /// without either storing the other's answer.
    fn file_id(&self) -> FileId {
        RootedPath::new(
            VirtualRoot::Project,
            // A literal path: `VirtualPath::new` only rejects an absolute or
            // escaping path, and this one is checked here rather than at the
            // first compile, where a failure would look like a formula bug.
            VirtualPath::new(SOURCE_NAME).expect("a plain relative path is always valid"),
        )
        .intern()
    }

    /// The page setup, prepended to every fragment.
    ///
    /// `width: auto` is doing real work: without it the formula is laid out in a
    /// paragraph of the default page width and a long one wraps onto a second
    /// line, which would turn "one formula" into two rows of glyphs. `margin: 0pt`
    /// keeps the frame's origin near the ink; the centring measures the ink
    /// itself, so the margin is only a convenience, not a correctness
    /// requirement.
    ///
    /// The length of this string is *half* the offset between a span into the
    /// compiled source and an offset into the caller's fragment; the opening `$`
    /// written after it is the other byte. See [`FormulaCompiler::compile`],
    /// which subtracts both.
    fn prelude(&self) -> String {
        format!(
            "#set page(width: auto, height: auto, margin: 0pt)\n#set text(size: {}pt)\n",
            self.font_size_pt
        )
    }

    /// Compile a formula body to mobjects.
    ///
    /// The input is the **body**, without `$` delimiters — `x^2 + y^2`, not
    /// `$x^2 + y^2$` — because that is what a caller has when it is handed a
    /// formula by a model or a user, and the delimiters are the compiler's
    /// business. The syntax is **Typst math, not LaTeX**: `pi`, not `\pi`;
    /// `frac(a, b)`, not `\frac{a}{b}`. That is the point of the route — there is
    /// no TeX installation to shell out to and no subprocess in a browser.
    pub fn compile(&mut self, body: &str) -> Result<Formula, FormulaError> {
        let prelude = self.prelude();
        // Where the caller's own string starts inside what is handed to Typst:
        // past the prologue and past the opening `$`. One byte, and the span
        // arithmetic below is off by one without it — pointing every glyph one
        // character to the left.
        let body_offset = prelude.len() + 1;
        let source_text = format!("{prelude}${body}$");
        let id = self.file_id();
        let source = Source::new(id, source_text);

        // The world borrows the expensive parts and owns only the source, so
        // building one per call costs a clone of an `Arc`.
        let extracted = {
            let world = FormulaWorld {
                library: &self.library,
                book: &self.book,
                fonts: &self.fonts,
                source: source.clone(),
            };
            let warned = typst::compile::<PagedDocument>(&world);
            let document = warned.output.map_err(|errors| {
                FormulaError::Compile(
                    errors
                        .iter()
                        .map(|error| error.message.to_string())
                        .collect::<Vec<_>>()
                        .join("; "),
                )
            })?;
            let Some(page) = document.pages().first() else {
                return Err(FormulaError::Empty);
            };
            let mut extracted = Extracted::default();
            walk(
                &page.frame,
                Affine2::IDENTITY,
                &source,
                body_offset,
                &mut extracted,
            );
            extracted
        };

        if extracted.glyphs.is_empty() && extracted.shapes.is_empty() {
            return Err(FormulaError::Empty);
        }

        // Every glyph in font-unit space, mapped into frame points. Done before
        // the cache is consulted so that a glyph used twice is one lookup and two
        // transforms.
        let mut placed: Vec<(Placed, Vec<Point>)> = Vec::with_capacity(extracted.glyphs.len());
        let mut bounds: Option<(f64, f64, f64, f64)> = None;
        let measure = |points: &[Point], bounds: &mut Option<(f64, f64, f64, f64)>| {
            for point in points {
                *bounds = Some(match *bounds {
                    Some((x0, y0, x1, y1)) => (
                        x0.min(point.x),
                        y0.min(point.y),
                        x1.max(point.x),
                        y1.max(point.y),
                    ),
                    None => (point.x, point.y, point.x, point.y),
                });
            }
        };
        for glyph in extracted.glyphs {
            let Some(font_space) = self.outline(&glyph.font, glyph.glyph_id) else {
                // No outline: a space, or a font the parser could not open. Not
                // an error — there is simply nothing to draw.
                continue;
            };
            let mut path = font_space;
            path.apply_affine(glyph.affine);
            let points = points_from_path(&path);
            if points.is_empty() {
                continue;
            }
            measure(&points, &mut bounds);
            placed.push((glyph, points));
        }

        // The rules. Measured into the same box as the glyphs, because the box is
        // what gets centred: a fraction moves its rule off the axis if it is
        // centred on the numerals alone, and a radical's overline sits entirely
        // above them.
        let mut rules = Vec::with_capacity(extracted.shapes.len());
        for shape in extracted.shapes {
            let mut path = shape.path;
            path.apply_affine(shape.affine);
            let points = points_from_path(&path);
            if points.is_empty() {
                continue;
            }
            measure(&points, &mut bounds);
            rules.push(points);
        }

        let Some((x0, y0, x1, y1)) = bounds else {
            return Err(FormulaError::Empty);
        };

        // Centre the measured ink on the local origin and turn the frame's
        // y-down points into world units. Composed once and applied to every
        // point, rather than a flip here and a divide there.
        let place = Affine2::new([
            1.0 / POINTS_PER_WORLD_UNIT,
            0.0,
            0.0,
            -1.0 / POINTS_PER_WORLD_UNIT,
            -(x0 + x1) / 2.0 / POINTS_PER_WORLD_UNIT,
            (y0 + y1) / 2.0 / POINTS_PER_WORLD_UNIT,
        ]);

        let mut glyphs = Vec::with_capacity(placed.len());
        for (glyph, points) in placed {
            let points = points.iter().map(|p| place * *p).collect();
            let mut mobject = Mobject::new(points).with_kind(NodeKind::Text);
            if !glyph.text.is_empty() {
                mobject = mobject.with_label(glyph.text.clone());
            }
            glyphs.push(FormulaGlyph {
                mobject,
                text: glyph.text,
                source: glyph.source,
            });
        }

        let rules = rules
            .into_iter()
            .map(|points| Mobject::new(points.iter().map(|p| place * *p).collect()))
            .collect();

        // The structure, from the same body through a second compile. After the
        // paged one because a fragment that does not compile has no structure to
        // report, and the geometry is the primary product.
        let mathml = self.mathml(body);

        Ok(Formula {
            root: {
                let mut root = Mobject::new(Vec::new()).with_kind(NodeKind::Formula);
                if let Some(mathml) = mathml {
                    root = root.with_mathml(mathml);
                }
                root
            },
            glyphs,
            rules,
            skipped_shapes: extracted.skipped_shapes,
        })
    }

    /// The fragment's structure as a `<math>` element, or `None`.
    ///
    /// # Why a second compile
    ///
    /// The paged route has already gathered every glyph *outline* — but an
    /// outline is where a symbol is, not what it is doing there. "x, 2, +, y, 2"
    /// is the same five glyphs for `x^2 + y^2` and for `x2 + y2`, and an
    /// accessibility tree built from the frame alone can only ever say the
    /// former. Typst's HTML exporter lowers the resolved math IR — the tree
    /// *after* scripts, fractions, fences and radicals have been recognized —
    /// into MathML, so the structure is the compiler's answer rather than a
    /// geometry heuristic of ours.
    ///
    /// `None` rather than an error when the HTML pass fails, because the formula
    /// still draws: a missing structured reading is a gap in what a reader can
    /// **hear**, not in what the frame shows, and turning it into a
    /// [`FormulaError`] would refuse to draw a formula that is on screen. The
    /// caller can tell the two states apart — [`Formula::mathml`] is `Some` in
    /// the working case.
    ///
    /// # Why the output is sliced rather than serialised directly
    ///
    /// `html()` encodes the *document* — `<html><head>` with Typst's MathML
    /// stylesheet, then `<body><p><math>…</math></p>` — and the element is what
    /// a caller wants. Typst offers no public "encode one element" entry point
    /// that does not first require a `LateLinkResolver` handle this crate would
    /// have to track by hand. Slicing is safe because the encoder escapes `<` in
    /// text content: no string *inside* the document can spell `<math`, and
    /// MathML does not nest a `<math>` inside another, so the first `<math` and
    /// the first `</math>` after it bound exactly one element.
    fn mathml(&self, body: &str) -> Option<String> {
        // The same `$…$` body as the paged pass, and nothing else: an HTML
        // document has no page to set up, and the size does not affect
        // structure, so a second prologue would only be a second place the two
        // passes could disagree.
        let source_text = format!("${body}$");
        let id = self.file_id();
        let source = Source::new(id, source_text);
        let document = {
            let world = FormulaWorld {
                library: &self.html_library,
                book: &self.book,
                fonts: &self.fonts,
                source,
            };
            typst::compile::<typst_html::HtmlDocument>(&world)
                .output
                .ok()?
        };
        let encoded =
            typst_html::html(&document, &typst_html::HtmlOptions { pretty: false }).ok()?;
        let start = encoded.find("<math")?;
        let end = encoded[start..].find("</math>")? + start + "</math>".len();
        Some(encoded[start..end].to_owned())
    }

    /// The font-space outline of one glyph, from the cache or from the face.
    ///
    /// Returning an owned `BezPath` rather than a reference keeps the borrow
    /// checker out of the caller's way at the cost of a clone per use, which for
    /// a glyph of a few dozen curves is cheaper than the lookup that found it.
    fn outline(&mut self, font: &Font, glyph_id: u16) -> Option<BezPath> {
        let key = (
            font.data().as_slice().as_ptr() as usize,
            font.index(),
            glyph_id,
        );
        if let Some(path) = self.outlines.get(&key) {
            return Some(path.clone());
        }
        let path = extract_outline(font, glyph_id)?;
        self.outlines.insert(key, path.clone());
        Some(path)
    }
}

/// A glyph on its way from Typst's frame to a mobject.
#[derive(Debug, Clone)]
struct Placed {
    font: Font,
    glyph_id: u16,
    /// Font units → frame points, with the run's cursor and every enclosing
    /// group transform already folded in.
    affine: Affine2,
    text: String,
    source: Option<Range<usize>>,
}

/// A non-text frame item that became filled geometry.
#[derive(Debug, Clone)]
struct PlacedShape {
    /// The shape in the item's own local coordinates.
    path: BezPath,
    /// Local coordinates → frame points.
    affine: Affine2,
}

/// What a frame yielded.
#[derive(Debug, Default)]
struct Extracted {
    glyphs: Vec<Placed>,
    shapes: Vec<PlacedShape>,
    skipped_shapes: usize,
}

/// The `World` Typst compiles against.
///
/// Borrows the library, the font book and the font list from the
/// [`FormulaCompiler`] that built them — they are the expensive parts and they
/// do not change per formula — and owns only the one source file, which does.
struct FormulaWorld<'a> {
    library: &'a LazyHash<Library>,
    book: &'a LazyHash<FontBook>,
    fonts: &'a [Font],
    source: Source,
}

impl World for FormulaWorld<'_> {
    fn library(&self) -> &LazyHash<Library> {
        self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        self.book
    }

    fn main(&self) -> FileId {
        self.source.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.source.id() {
            Ok(self.source.clone())
        } else {
            Err(FileError::NotFound(PathBuf::from(SOURCE_NAME)))
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        // `from_string` rather than a byte copy: the trait's own docs name it as
        // the cheap way to view a source, which a formula has no reason to
        // depart from.
        Ok(Bytes::from_string(self.source(id)?.text().to_owned()))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }

    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        // A fixed date rather than `None`. `None` is legal and looks like the
        // honest answer for an engine with no clock, but it turns into an
        // *error* the moment anything asks for the date — `DocumentInfo`'s date
        // is `auto`, so that is layout reaching for the clock, not the formula.
        // A constant is one less way for a formula to fail and one less way for
        // a golden image to depend on when it was rendered.
        Datetime::from_ymd(1970, 1, 1)
    }
}

/// Collect every glyph a frame draws, in frame coordinates.
///
/// Recurses into groups because math nests them: a superscript is a group with a
/// scale and a translate, and ignoring them would stack every script on the
/// baseline. Group clipping is not applied — see the module docs.
fn walk(
    frame: &Frame,
    transform: Affine2,
    source: &Source,
    body_offset: usize,
    out: &mut Extracted,
) {
    for (position, item) in frame.items() {
        match item {
            FrameItem::Text(run) => {
                collect_run(run, *position, transform, source, body_offset, out);
            }
            FrameItem::Group(group) => {
                // The order is typst-render's: the group's own transform applies
                // to its frame's coordinates first, and the item's position then
                // places that frame. `Affine`'s `*` applies its right operand
                // first, so this reads the same way round as the prose.
                let inner = transform
                    * Affine2::translate((position.x.to_pt(), position.y.to_pt()))
                    * typst_transform(&group.transform);
                walk(&group.frame, inner, source, body_offset, out);
            }
            FrameItem::Shape(shape, _) => match filled_shape(shape) {
                Some(path) => out.shapes.push(PlacedShape {
                    path,
                    // No sub-transform of its own, unlike a glyph run: the item's
                    // position is the whole placement.
                    affine: transform
                        * Affine2::translate((position.x.to_pt(), position.y.to_pt())),
                }),
                None => out.skipped_shapes += 1,
            },
            // `Link` and `Tag` carry no pixels — a tag is introspection
            // bookkeeping — so they are not counted as skipped geometry.
            FrameItem::Image(..) | FrameItem::Link(..) | FrameItem::Tag(..) => {}
        }
    }
}

/// The filled outline of a non-text frame item, if this engine can draw it.
///
/// A formula emits two kinds of `FrameItem::Shape` and both of them are shapes
/// this engine can only draw *filled*: a fraction's rule and a radical's
/// overline, each of which Typst describes as a **stroked horizontal line**. A
/// stroked line is not a path a `FillPath` can express — the stroke has to be
/// expanded into the quad it covers — so the expansion happens here, and the
/// result is an ordinary filled mobject like a glyph's.
///
/// The quad is the stroke's centre line inflated by half the thickness, which is
/// exact for the butt cap these two constructs use and, for a round or square
/// cap, wrong only by a half-thickness of paint at each end — a fraction of a
/// point on a rule three dozen points long.
///
/// `None` for anything else, which the caller counts rather than dropping in
/// silence. `Geometry::Curve` is the case that needs real work: expanding an
/// arbitrary stroked curve into an outline is a stroker, and nothing in a
/// formula uses one by default.
fn filled_shape(shape: &Shape) -> Option<BezPath> {
    match &shape.geometry {
        Geometry::Line(to) => {
            let stroke = shape.stroke.as_ref()?;
            let half = stroke.thickness.to_pt() / 2.0;
            Some(rustim_math::rect_path(rustim_math::Rect::new(
                0.0,
                -half,
                to.x.to_pt(),
                half,
            )))
        }
        Geometry::Rect(size) => {
            // A rect with only a stroke is a *frame*, and drawing it as a filled
            // rectangle would be worse than not drawing it: an empty box would
            // become a solid one.
            shape.fill.as_ref()?;
            Some(rustim_math::rect_path(rustim_math::Rect::new(
                0.0,
                0.0,
                size.x.to_pt(),
                size.y.to_pt(),
            )))
        }
        Geometry::Curve(_) => None,
    }
}

/// Turn one text run's glyphs into [`Placed`] entries.
///
/// The cursor arithmetic is `typst-render/src/text.rs:17`'s, transcribed: `x` and
/// `y` accumulate the advances in points, `y` running **up** from the run's
/// origin, and both the glyph's own offset and the outline get the same sign
/// treatment so the two cannot disagree.
fn collect_run(
    run: &TextItem,
    position: TypstPoint,
    transform: Affine2,
    source: &Source,
    body_offset: usize,
    out: &mut Extracted,
) {
    let size_pt = run.size.to_pt();
    let upem = run.font.units_per_em();
    // `is_finite` as well as positive: a degenerate size would make every
    // coordinate infinite or NaN, and a NaN point array propagates silently into
    // the mobject rather than failing anywhere.
    if !size_pt.is_finite() || size_pt <= 0.0 || !upem.is_finite() || upem <= 0.0 {
        return;
    }
    // Font units → frame points. Applied to the *outline*, after the run's own
    // cursor has placed the glyph, which is how the y-up outline becomes a
    // y-up local space with no net flip.
    let scale = size_pt / upem;
    let font = run.font.font().clone();
    let run_text: &str = &run.text;
    let mut advance_x = 0.0;
    let mut advance_y = 0.0;

    for glyph in &run.glyphs {
        let origin_x = position.x.to_pt() + advance_x + glyph.x_offset.at(run.size).to_pt();
        let origin_y = position.y.to_pt() - (advance_y + glyph.y_offset.at(run.size).to_pt());

        // What the shaper put in the run, and what the author actually typed.
        // These are **not** the same string, and the difference is the sort that
        // silently costs a feature: math mode typesets a Latin letter from the
        // mathematical italic face, so `x` arrives in the run as U+1D465 `𝑥`. A
        // label taken from the run would name the symbol with a character nobody
        // wrote — the one thing a screen reader has no sensible reading for.
        //
        // The span is the *element's*, not the glyph's: every glyph of a run
        // carries the same one, so it is only usable as a label when it covers
        // exactly as many characters as the glyph was shaped from. Measured:
        // `"abc"` gives all three glyphs the span of the whole literal — quotes
        // included — and labelling each of them `"abc"` is worse than useless.
        // When the counts disagree the run's own text is the honest answer; it
        // is the typeset character (`pi` reports `π`), which is exactly what was
        // drawn. `source` below keeps the exact bytes either way, so nothing that
        // needs the author's spelling has to read `text`.
        let shaped = run_text.get(glyph.range()).unwrap_or_default();
        let node = source.find(glyph.span.0);
        let written = node
            .as_ref()
            .and_then(|node| source.text().get(node.range()));
        let text = written
            .filter(|w| !w.is_empty() && w.chars().count() == shaped.chars().count())
            .unwrap_or(shaped)
            .to_owned();
        let source_range = node.map(|node| node.range()).map(|r| {
            // A span inside the prologue would be a bug, but saturating keeps it
            // from panicking inside a compiler that a model can reach.
            r.start.saturating_sub(body_offset)..r.end.saturating_sub(body_offset)
        });

        out.glyphs.push(Placed {
            font: font.clone(),
            glyph_id: glyph.id,
            affine: transform
                * Affine2::translate((origin_x, origin_y))
                * Affine2::scale_non_uniform(scale, -scale),
            text,
            source: source_range,
        });

        advance_x += glyph.x_advance.at(run.size).to_pt();
        advance_y += glyph.y_advance.at(run.size).to_pt();
    }
}

/// Typst's transform as a `kurbo::Affine`.
///
/// The argument order is `typst-render/src/lib.rs:264`'s `to_sk_transform`,
/// which is the reference for how Typst means its matrix to be read: the fields
/// are *row-major* as written (`sx, ky, kx, sy, tx, ty`) but the matrix maps a
/// point to `(sx·x + kx·y + tx, ky·x + sy·y + ty)`, so the off-diagonal pair
/// swaps places on the way into `Affine::new`.
fn typst_transform(transform: &Transform) -> Affine2 {
    Affine2::new([
        transform.sx.get(),
        transform.ky.get(),
        transform.kx.get(),
        transform.sy.get(),
        transform.tx.to_pt(),
        transform.ty.to_pt(),
    ])
}

/// Draw one glyph's outline out of the face's own bytes, in font units.
///
/// The bytes come from `Font::data()`, **not** from Typst's own
/// `ttf_parser`/`rustybuzz` views. That is deliberate and is the whole reason
/// the frozen `rustybuzz ^0.20` / `ttf-parser ^0.25` subtree can stay inside
/// Typst: the two libraries are handed the same bytes and asked the same
/// question independently, so they never have to agree on types.
fn extract_outline(font: &Font, glyph_id: u16) -> Option<BezPath> {
    let bytes = font.data().as_slice();
    let face = skrifa::FontRef::from_index(bytes, font.index()).ok()?;
    let outlines = skrifa::outline::OutlineGlyphCollection::new(&face);
    let glyph = outlines.get(skrifa::GlyphId::new(u32::from(glyph_id)))?;
    let mut pen = OutlineCollector(BezPath::new());
    // Unscaled is the load-bearing word: the emitted coordinates are font units
    // with a scale factor of 1, so the cached path is size-independent and the
    // size enters later as an affine.
    glyph
        .draw(skrifa::instance::Size::unscaled(), &mut pen)
        .ok()?;
    Some(pen.0)
}

/// The `skrifa` pen that accumulates a glyph outline into a `BezPath`.
///
/// Written out rather than taken from `skrifa`'s `kurbo` support because that
/// support is a *dev*-dependency of `skrifa` — there is no feature that turns it
/// on — and because five methods of `f64::from` is cheaper than a version
/// coupling between two `kurbo`s that must agree.
struct OutlineCollector(BezPath);

impl skrifa::outline::OutlinePen for OutlineCollector {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to((f64::from(x), f64::from(y)));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to((f64::from(x), f64::from(y)));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0.quad_to(
            (f64::from(cx0), f64::from(cy0)),
            (f64::from(x), f64::from(y)),
        );
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.curve_to(
            (f64::from(cx0), f64::from(cy0)),
            (f64::from(cx1), f64::from(cy1)),
            (f64::from(x), f64::from(y)),
        );
    }

    fn close(&mut self) {
        self.0.close_path();
    }
}

/// Every font file in `dir`, sorted by name.
///
/// Sorted so the font set — and therefore the book indices, and therefore which
/// face a fallback picks — is the same on every run and every machine. The
/// unordered-collection trap is recorded in `crate::FontProvider::register`; a
/// `read_dir` has the same failure mode one level down.
fn bundled_font_paths(dir: &Path) -> Result<Vec<PathBuf>, FormulaError> {
    if !dir.is_dir() {
        return Err(FormulaError::Fonts(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is not a directory", dir.display()),
        )));
    }
    let entries = std::fs::read_dir(dir).map_err(FormulaError::Fonts)?;
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let path = entry.map_err(FormulaError::Fonts)?.path();
        let is_font = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                FONT_EXTENSIONS
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(ext))
            });
        if is_font {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_math::bezier;

    fn compiler() -> FormulaCompiler {
        FormulaCompiler::new().expect("the bundled fonts are in the repository")
    }

    #[test]
    fn the_default_font_size_is_the_one_manim_uses() {
        // The two constants encode the same decision from opposite ends:
        // `MANIM_FONT_SIZE_UNITS` says a Manim `font_size` is points with 72 to
        // the world unit, and this says the default is Manim's own default. If
        // they ever disagree, one of them was edited without the other.
        assert_eq!(
            DEFAULT_FONT_SIZE_PT / POINTS_PER_WORLD_UNIT,
            crate::MANIM_FONT_SIZE_UNITS,
        );
    }

    #[test]
    fn a_formula_compiles_to_a_formula_mobject() {
        let mut compiler = compiler();
        let formula = compiler
            .compile("x^2 + y^2")
            .expect("a plain formula compiles");

        assert_eq!(formula.root.kind, NodeKind::Formula);
        assert!(
            formula.root.points.is_empty(),
            "the container holds no geometry; its glyphs do",
        );
        assert!(
            formula.glyphs.len() >= 5,
            "x, 2, +, y, 2 is five glyphs, got {}",
            formula.glyphs.len(),
        );
        assert_eq!(
            formula.skipped_shapes, 0,
            "a formula with no fraction emits no shapes"
        );
        for glyph in &formula.glyphs {
            assert_eq!(glyph.mobject.kind, NodeKind::Text);
            assert!(!glyph.mobject.points.is_empty());
        }
    }

    /// The precondition every geometry reader in the engine depends on: points
    /// are read four at a time, so a glyph subpath whose length is not a multiple
    /// of four silently shifts every control point after it. This is the property
    /// that fails invisibly — the shape still draws, somewhere else.
    #[test]
    fn every_glyph_subpath_is_a_multiple_of_four_points() {
        let mut compiler = compiler();
        let formula = compiler.compile("a^2 + b^2 = c^2").expect("compiles");
        for glyph in &formula.glyphs {
            assert_eq!(
                glyph.mobject.points.len() % rustim_math::bezier::POINTS_PER_CURVE,
                0,
                "glyph {:?} has {} points",
                glyph.text,
                glyph.mobject.points.len(),
            );
            // And the reader agrees, which is the thing that actually matters.
            assert!(
                bezier::anchors(&glyph.mobject.points).is_some(),
                "glyph {:?} does not parse as curves",
                glyph.text,
            );
        }
    }

    /// Per-symbol addressability is the reason this module exists at all, so it
    /// is asserted rather than assumed: a glyph knows which characters produced
    /// it, both as text and as a range into the caller's own string.
    #[test]
    fn glyph_outlines_carry_their_source_span() {
        let mut compiler = compiler();
        let body = "x^2 + y";
        let formula = compiler.compile(body).expect("compiles");

        let labels: Vec<&str> = formula.glyphs.iter().map(|g| g.text.as_str()).collect();
        assert!(
            labels.contains(&"x"),
            "no glyph claims to be the x: {labels:?}"
        );
        assert!(
            labels.contains(&"+"),
            "no glyph claims to be the plus: {labels:?}"
        );

        // The byte range indexes the caller's string, not this module's
        // prologue-wrapped copy of it. Asserting the *slice* rather than the
        // bounds is what catches the off-by-one: an offset that forgot the `$`
        // delimiter lands one character to the left, which is still a valid
        // range and still inside the string.
        let x = formula
            .glyphs
            .iter()
            .find(|g| g.text == "x")
            .expect("the x glyph");
        let range = x
            .source
            .clone()
            .expect("a shaped glyph has a resolvable span");
        assert!(
            range.end <= body.len(),
            "span {range:?} runs past the caller's {}-byte string",
            body.len(),
        );
        let sliced = &body[range.clone()];
        assert!(
            sliced.contains('x'),
            "span {range:?} slices {sliced:?} out of {body:?}"
        );
    }

    /// The cache is keyed on the face and the glyph, *not* the size, because
    /// outlines are extracted in font units. Two identical symbols are therefore
    /// one extraction, and the assertion is on the count rather than on a timing.
    #[test]
    fn the_same_glyph_is_outlined_once_however_often_it_appears() {
        let mut many = compiler();
        many.compile("x x x x").expect("compiles");
        let four_x = many.cached_outlines();
        assert_eq!(
            four_x, 1,
            "four x's should extract one outline, got {four_x}"
        );

        let mut one = compiler();
        one.compile("x").expect("compiles");
        assert_eq!(one.cached_outlines(), 1);
    }

    /// Scale is applied after the cache, so a different size reuses the same
    /// outline — and still comes out bigger.
    #[test]
    fn a_larger_size_reuses_the_outline_and_grows_the_glyphs() {
        let mut compiler = compiler();
        let small = compiler.compile("x").expect("compiles");
        let small_width = width_of(&small);
        assert_eq!(compiler.cached_outlines(), 1);

        let mut compiler = compiler.with_font_size(DEFAULT_FONT_SIZE_PT * 2.0);
        let big = compiler.compile("x").expect("compiles");
        assert_eq!(
            compiler.cached_outlines(),
            1,
            "the size is not part of the key"
        );
        let big_width = width_of(&big);
        assert!(
            (big_width - small_width * 2.0).abs() < 1e-9,
            "doubling the size should double the width: {small_width} → {big_width}",
        );
    }

    fn width_of(formula: &Formula) -> f64 {
        let bounds = formula.local_bounds().expect("a formula has geometry");
        bounds.x1 - bounds.x0
    }

    /// A formula the model got wrong is an error value, not a panic — this whole
    /// module is reachable from model output.
    ///
    /// The bodies are ones that were *measured* to fail, not ones that look like
    /// they should: math mode is not LaTeX and it is not strict either. `{`, `(`
    /// and a trailing `+` are all valid math, so a test written from intuition
    /// asserts an error for a formula that compiles perfectly well and is wrong
    /// about the library rather than the library being wrong.
    #[test]
    fn an_unparsable_formula_is_an_error_not_a_panic() {
        let mut compiler = compiler();
        for body in ["$", "frac(", "a^", "#set", "\\", "\"unclosed"] {
            match compiler.compile(body) {
                Err(FormulaError::Compile(message)) => {
                    assert!(!message.is_empty(), "{body:?} failed with an empty message");
                }
                other => panic!("{body:?} should not compile, got {other:?}"),
            }
        }
    }

    /// Nothing in, nothing out — and `Empty`, which a caller can tell apart from
    /// a compile error, because "you sent an empty formula" and "your formula is
    /// wrong" want different words in front of a person.
    ///
    /// `&` is in here because it is not a syntax error either: an alignment
    /// point with nothing to align produces no pages.
    #[test]
    fn a_formula_with_nothing_in_it_is_empty_and_not_an_error() {
        let mut compiler = compiler();
        for body in ["", " ", "&"] {
            match compiler.compile(body) {
                Err(FormulaError::Empty) => {}
                other => panic!("{body:?} should be empty, got {other:?}"),
            }
        }
    }

    /// Math mode typesets a Latin letter from the *mathematical italic* face, so
    /// the run carries U+1D465 `𝑥` where the author typed `x`. A label taken from
    /// the run names the symbol with a character nobody wrote — which a screen
    /// reader reads as a separate code point, and which the AI layer cannot match
    /// a command against. The label has to come from the caller's own string.
    #[test]
    fn a_math_letter_reads_as_the_letter_the_author_typed() {
        let mut compiler = compiler();
        let formula = compiler.compile("x^2 + y").expect("compiles");
        let labels: Vec<&str> = formula.glyphs.iter().map(|g| g.text.as_str()).collect();
        assert!(labels.contains(&"x"), "not one glyph says `x`: {labels:?}");
        assert!(labels.contains(&"y"), "not one glyph says `y`: {labels:?}");
        assert!(
            !labels
                .iter()
                .any(|l| l.chars().any(|c| ('\u{1d400}'..='\u{1d7ff}').contains(&c))),
            "a mathematical alphanumeric symbol leaked into a label: {labels:?}",
        );
    }

    /// A span that covers more than the glyph was shaped from is not a name. A
    /// quoted literal is one text element, so all three of its glyphs carry the
    /// same span — the whole `"abc"`, quotes and all — and labelling each of them
    /// with that is worse than the typeset character.
    #[test]
    fn a_literal_inside_a_formula_names_each_glyph_and_not_the_whole_literal() {
        let mut compiler = compiler();
        let formula = compiler.compile("\"abc\"").expect("compiles");
        let labels: Vec<&str> = formula.glyphs.iter().map(|g| g.text.as_str()).collect();
        assert_eq!(labels, ["a", "b", "c"]);

        // CJK is where this is not academic: a Chinese word in a formula is
        // several characters of one literal, and a blind reader told the whole
        // word four times over has been told nothing about the symbols drawn.
        let cjk = compiler.compile("\"面积\" = pi r^2").expect("compiles");
        let labels: Vec<&str> = cjk.glyphs.iter().map(|g| g.text.as_str()).collect();
        assert_eq!(
            labels.iter().take(2).copied().collect::<Vec<_>>(),
            ["面", "积"]
        );
    }

    /// A fraction's bar and a radical's overline are `FrameItem::Shape`s, and
    /// both are *stroked horizontal lines* — geometry this engine can only draw
    /// filled. Expanding the stroke into the quad it covers is what makes a
    /// fraction look like a fraction, so the assertion is that a bar exists, is
    /// wider than it is tall, and sits between the two numerals.
    #[test]
    fn a_fraction_rule_is_drawn_as_a_filled_bar_and_not_dropped() {
        let mut compiler = compiler();
        let fraction = compiler.compile("frac(1, 2)").expect("compiles");
        assert_eq!(
            fraction.skipped_shapes, 0,
            "nothing about a fraction is un-drawable now"
        );
        assert_eq!(fraction.rules.len(), 1, "a fraction has exactly one rule");

        let bar = fraction.local_bounds().expect("a fraction has geometry");
        let rule = bounds_of(&fraction.rules[0].points);
        assert!(
            rule.1 - rule.0 > rule.3 - rule.2,
            "a rule is wider than it is thick: {rule:?}",
        );
        assert!(
            rule.2 > bar.y0 && rule.3 < bar.y1,
            "the rule sits inside the formula's box: {rule:?} in {bar:?}",
        );

        // A radical emits one too, from the same branch of Typst's layout code.
        let radical = compiler.compile("sqrt(2)").expect("compiles");
        assert_eq!(radical.rules.len(), 1, "a radical has an overline");
        assert_eq!(radical.skipped_shapes, 0);

        // ...and an accent does not, because a hat is a glyph in this font. So
        // the rule count is about geometry, not about how the source is written.
        let accented = compiler.compile("hat(x)").expect("compiles");
        assert!(accented.rules.is_empty());
    }

    /// The structure survives the trip: a script is an `msup`, a fraction is an
    /// `mfrac`, and both are *elements*, not a rendering hint.
    ///
    /// This is the assertion the whole item exists for. A glyph tree can say
    /// "x, 2, +, y, 2" for `x^2 + y^2`; only the MathML says the 2 is an
    /// exponent. Measured rather than assumed — the tags below are Typst's own
    /// output, not a shape this crate constructs.
    #[test]
    fn a_formula_carries_its_structure_as_mathml() {
        let mut compiler = compiler();
        let formula = compiler.compile("x^2 + frac(1, 2)").expect("compiles");
        let mathml = formula.mathml().expect("a compiled formula has structure");

        assert!(mathml.starts_with("<math>"), "{mathml}");
        assert!(mathml.ends_with("</math>"), "{mathml}");
        assert!(
            mathml.contains("<msup>"),
            "the exponent is not a script: {mathml}"
        );
        assert!(
            mathml.contains("<mfrac>"),
            "the fraction is not a fraction: {mathml}"
        );
        // And nothing outside the element leaked in: the document's stylesheet
        // and its `<body>` wrapper are the slicing's job to drop.
        assert!(!mathml.contains("<style"), "{mathml}");
        assert!(!mathml.contains("<body"), "{mathml}");
    }

    /// The structure travels with the object, so a formula that has been
    /// inserted — the only form the a11y tree can see — still has it.
    #[test]
    fn the_structure_travels_with_the_inserted_root() {
        let mut compiler = compiler();
        let formula = compiler.compile("sqrt(2)").expect("compiles");
        let expected = formula.mathml().expect("has structure").to_owned();
        assert!(expected.contains("<msqrt>"), "{expected}");

        let mut tree = SceneTree::new();
        let nodes = formula.insert(&mut tree);
        assert_eq!(
            tree.get(nodes.root).and_then(|m| m.mathml.as_deref()),
            Some(expected.as_str()),
            "the root lost its structure on the way into the tree",
        );
    }

    /// A glyph's own name and the formula's structure are different facts, and
    /// the engine keeps both: the label is what `--inspect` names a symbol with,
    /// the MathML is what the expression means.
    #[test]
    fn the_mathml_is_not_the_labels_and_not_a_substitute_for_them() {
        let mut compiler = compiler();
        let formula = compiler.compile("x^2 + y").expect("compiles");
        let mathml = formula.mathml().expect("has structure");
        // The typeset character is the mathematical italic, which is what Typst
        // emits into `mi`; the labels keep the author's spelling. That asymmetry
        // is deliberate and this test says so.
        assert!(mathml.contains('\u{1d465}'), "{mathml}");
        assert!(
            formula.glyphs.iter().any(|g| g.text == "x"),
            "no glyph keeps the author's spelling",
        );
    }

    fn bounds_of(points: &[Point]) -> (f64, f64, f64, f64) {
        points.iter().fold(
            (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
            |(x0, y0, x1, y1), p| (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y)),
        )
    }
}
