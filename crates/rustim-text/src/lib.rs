//! Text shaping: a string in, positioned glyph runs out.
//!
//! # Where this sits
//!
//! Between the scene layer, which knows a mobject's *text* and its transform, and
//! the render layer, which knows how to put a glyph on a scanline. Neither of
//! those two may depend on the other, so the shared vocabulary — a glyph, its id
//! and its offset — lives in `rustim-math` (`rustim_math::TextGlyph`), and a run
//! carries a font handle that is parley's type on this side and peniko's on the
//! other. They are the same type; see `rustim-math/src/text.rs` for why that
//! still needs a conversion at the boundary.
//!
//! # Fonts are injected, never discovered
//!
//! `fontique`'s default is to find fonts through the operating system —
//! fontconfig on Linux, CoreText on macOS, DirectWrite on Windows. This crate
//! compiles it with `default-features = false`, which removes all three, and a
//! [`FontProvider`] therefore starts with *no* fonts and gains exactly the ones
//! it is handed. `方案建议.md` §10.6 guard 3 requires this, and the reason is not
//! purity: a bare Linux container has no fonts at all, the three CI platforms
//! have different ones, and a golden image containing text must be reproducible
//! from the repository alone. It also means the browser build (Phase 2+) can
//! fetch a subsetted face instead of embedding 19 MB of CJK in the wasm.
//!
//! # Shaping is not drawing
//!
//! Everything here produces *indices*: which glyph, from which face, at which
//! offset. No outline is loaded and no pixel is touched. That split is why the
//! glyph cache in the backend can rasterise a run once and reuse it for every
//! frame it appears in.
//!
//! ## What is here, and what is not
//!
//! Two things, and they share only the font directory.
//!
//! **Shaping**, line breaking and the missing-glyph report. Everything it
//! produces is *indices* — which glyph, from which face, at which offset — and
//! the outlines they name are loaded downstream, by the backend's glyph cache.
//!
//! **Formulas**, in [`formula`]: a `$…$` fragment goes through Typst and comes
//! back as a mobject tree with one child per glyph. That is the one place in
//! this crate that touches a rasteriser's input rather than a shaper's output,
//! and it is deliberately the opposite trade from the line above: the whole
//! reason formulas do not go through `typst-render` is that per-glyph *outlines*
//! — not a rasterised page — are what per-symbol addressability needs. LaTeX is
//! out for the reason it always was: there is no TeX installation to shell out
//! to, and a subprocess is not available in a browser.
//!
//! Note what is *not* shared between them: Typst brings its own shaping, its own
//! font book and its own frozen `ttf-parser`. That subtree lives inside
//! [`formula`], and neither `parley` nor `rustim-render` can see it.

pub mod formula;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parley::fontique::{self, Blob, CollectionOptions, FamilyId, GenericFamily};
use parley::{FontContext, FontFamily, LayoutContext, PositionedLayoutItem, StyleProperty};

/// Re-exported because a [`TextRun`] names it and a caller building a
/// `rustim_render::DrawOp::GlyphRun` has to hand it straight over. It is the same
/// type parley hands out, so the shaped run passes through unconverted — which is
/// the property that makes this pipeline cheap. (Spelled as code rather than as a
/// link because this crate does not depend on `rustim-render` and must not: the
/// dependency runs the other way, through `rustim_math::TextGlyph`.)
pub use parley::FontData;

pub use rustim_math::TextGlyph;

/// Where the bundled face lives, relative to this crate.
///
/// `assets/fonts/` is in the repository, deliberately as a *file* rather than an
/// `include_bytes!`: 19 MB in a compilation unit would be re-parsed on all three
/// CI platforms for every build. The path is resolved at compile time against the
/// crate root, which is correct for a checkout, for `cargo test`, and for
/// `cargo run` — and wrong for an installed binary, which is why
/// [`FontProvider::with_bundled`] reports a missing directory rather than
/// assuming it is there. An installed layout is a packaging problem, not a
/// shaping one, and guessing a second path to paper over it would hide the
/// failure on the platform that matters.
pub const BUNDLED_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts");

/// The face inside [`BUNDLED_DIR`] that a request for no particular family gets.
///
/// The bundled `.ttc` holds six faces — SC, TC, HK, and the three Mono variants —
/// and a request for "sans-serif" reaches whichever of them the generic family
/// list names *first*. That list is filled from
/// [`fontique::Collection::register_fonts`], which collects the families it found
/// into a `HashMap` and hands them back in that map's iteration order. So the
/// order is not a property of the file, or of this crate, or of the build: it is
/// decided per process, and the same binary renders differently on two runs.
///
/// It is not a subtle difference. Measured on this repository's own font: the
/// same string shaped through the generic family picked face 5 in one process and
/// face 1 in the next, glyph `直` came out as id 27874 (SC/TC/HK agree) or 27875
/// (HK differs), and two renders of one release binary differed over the whole
/// content bbox — 24746 pixels, maximum channel delta 237. A golden baseline
/// blessed from one of those runs cannot pass on the next.
///
/// So the generic slots are set to exactly this family, by name, rather than to
/// everything the file happened to yield. `register` still accepts any provider's
/// fonts; this is the *preference*, and a provider whose faces are all something
/// else falls back to the first name in sorted order.
pub const BUNDLED_FAMILY: &str = "Noto Sans CJK SC";

/// How to shape one piece of text.
///
/// `size` is in **world units per em**, not pixels and not Manim's `font_size`.
/// World units are what scene code is written in (see `CLAUDE.md`, "World units,
/// not pixels, in scene code"), and the conversion to the pixel space the shaper
/// works in happens once, in [`FontProvider::shape`], using the viewport — so a
/// scene does not have to know the frame size and text scales with the frame
/// instead of vanishing on a 4K display.
#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    /// A family name as registered by the provider, or `None` for the generic
    /// sans-serif family. A name that matches nothing falls back rather than
    /// failing, which is `fontique`'s behaviour and the right one for text: a
    /// scene asking for a font the deployment does not carry should still draw.
    pub family: Option<String>,
    pub size: f64,
    pub bold: bool,
    pub italic: bool,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            family: None,
            // Manim's own default is `font_size = 48` in *its* units, which is
            // about half a world unit of em box. The exact correspondence is
            // measured rather than assumed — see `MANIM_FONT_SIZE_UNITS`.
            size: MANIM_FONT_SIZE_UNITS,
            bold: false,
            italic: false,
        }
    }
}

/// The em box, in world units, that Manim's `font_size = 48` corresponds to —
/// equivalently, Manim's `font_size` is in points and one world unit is 72 of
/// them.
///
/// **Measured, not derived.** `方案建议.md` §2.4 records Manim's
/// `TEXT2SVG_ADJUSTMENT_FACTOR` (4.8) and `TEXT_MOB_SCALE_FACTOR` (0.05), and
/// both belong to Manim's *SVG* pipeline: they describe how an SVG user unit is
/// scaled after `svg2paths` has produced one. Multiplying them — `48 / 4.8 *
/// 0.05`, which is `0.5` — was this constant's first value, and it is wrong by a
/// third, because the points-to-pixels step in between is not one of them.
///
/// The measurement is `crates/rustim-text/tests/manim_text_scale.rs`, over a
/// fixture from `tools/manim-fixtures/text_scale.py`. It comes out at
/// **0.666141** world units: Manim's ink box for the capital `H` is 0.48828125
/// world units at `font_size = 48`, and the bundled face's `OS/2` cap height is
/// 733/1000 em — the one part of the comparison that is a font metric rather than
/// an ink measurement, which is what makes the two sides comparable at all.
///
/// The constant is written as `2/3` rather than as the raw quotient because the
/// 0.08% between them is Manim's own arithmetic: it snaps SVG coordinates to a
/// 1/64 grid, so its `H` is 31.25/64 rather than anything that lands on `2/3`.
/// Encoding the snapped value would bake a rendering artefact into the constant.
///
/// How Manim arrives at `1/72` — Pango points, SVG user units, the `0.05` scale,
/// and whatever resolution the SVG surface is created at — is deliberately *not*
/// traced here. The composite is what a caller cares about, and the intermediate
/// steps are not independently observable from outside; the doc records the
/// measurement and the relation it supports rather than a story about the
/// internals that the next Manim release could quietly make false.
pub const MANIM_FONT_SIZE_UNITS: f64 = 2.0 / 3.0;

/// A run of glyphs sharing one face and one size, positioned relative to the
/// top-left of the shaped block, y downwards.
#[derive(Debug, Clone)]
pub struct TextRun {
    pub font: FontData,
    /// Em size in the caller's coordinates — the same units as the glyph offsets
    /// and as whatever transform the caller draws the run under. The backend
    /// multiplies this by that transform's scale, so a run drawn under the
    /// viewport transform is sized in world units.
    pub size: f32,
    pub glyphs: Vec<TextGlyph>,
}

/// A stretch of text that the font has no glyph for.
///
/// The plan's requirement is a report rather than a tofu box: drawing `.notdef`
/// tells the reader nothing they can act on, while naming the character tells
/// them the font is the problem and not the scene. Two things make this
/// reportable at all — the cluster keeps its source text range, so the offending
/// *characters* are still known here, and the glyphs are dropped from the run
/// rather than replaced, so what is drawn is correct as far as it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingGlyph {
    /// The characters that produced no glyph.
    pub characters: String,
    /// Their code points, for a report that has to survive a terminal which
    /// cannot display them — which is the situation where this message matters.
    pub codepoints: Vec<u32>,
    /// Byte offset of `characters` within the string that was shaped.
    pub index: usize,
}

/// A named family the provider does not have.
///
/// `fontique` resolves a *named* family by lookup and does not fall back when it
/// misses: the per-character fallback that covers "this face has no Chinese"
/// only runs once a family has matched. So a scene that asks for a font the
/// deployment does not carry would, unhandled, draw nothing at all — on the
/// machine where a scene full of blanks is hardest to diagnose. The provider
/// therefore does the lookup itself, substitutes the generic family, and reports
/// the substitution here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontSubstitution {
    /// What the scene asked for.
    pub requested: String,
    /// What was used instead, or `None` if the provider has no fonts at all.
    pub used: Option<String>,
}

/// Shaped text, ready to be drawn.
#[derive(Debug, Clone)]
pub struct ShapedText {
    pub runs: Vec<TextRun>,
    /// The laid-out width in pixels: the longest line's advance.
    pub width: f32,
    /// The laid-out height in pixels, including the first line's ascent and the
    /// last line's descent.
    pub height: f32,
    /// Anything the font could not draw, in reading order. Empty is the normal
    /// case, and a caller that ignores this draws a correct picture of the
    /// supported part of its string.
    pub missing: Vec<MissingGlyph>,
    /// Set when the requested family was not found. `None` means the text is in
    /// the face that was asked for.
    pub substituted: Option<FontSubstitution>,
}

impl ShapedText {
    /// A one-line complaint, or `None` if every character had a glyph.
    pub fn missing_report(&self) -> Option<String> {
        if self.missing.is_empty() {
            return None;
        }
        let listed = self
            .missing
            .iter()
            .map(|m| {
                let points = m
                    .codepoints
                    .iter()
                    .map(|c| format!("U+{c:04X}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("{:?} ({points}) at byte {}", m.characters, m.index)
            })
            .collect::<Vec<_>>()
            .join("; ");
        Some(format!("no glyph for {listed}"))
    }

    /// Whether there is anything to draw.
    pub fn is_empty(&self) -> bool {
        self.runs.iter().all(|r| r.glyphs.is_empty())
    }
}

/// Owns the font collection and the scratch space shaping needs.
///
/// Both are expensive to build and cheap to reuse, which is why the provider is
/// created once and kept: `Collection` holds every registered face, and
/// `LayoutContext` holds the allocations a layout makes.
pub struct FontProvider {
    fonts: FontContext,
    /// Scratch space. `()` is the brush type: this crate produces positioned
    /// glyphs, and colour is applied later by whoever draws them, so a per-run
    /// brush would be a second place for the same colour to live.
    layout: LayoutContext<()>,
    /// The file each registered blob came from, for the error messages that only
    /// happen when a font is wrong. Empty for a blob registered from memory.
    sources: Vec<(u64, PathBuf)>,
    /// Family names, sorted, so a caller can ask what it got instead of guessing
    /// at what a `.ttc` contains — and so that two processes holding the same
    /// fonts list them in the same order. See [`Self::register`].
    families: Vec<String>,
    /// The family the generic families resolve to, which is the one a
    /// [`TextStyle`] naming no family gets. Kept so that
    /// [`FontSubstitution::used`] can report the family actually used: reading it
    /// back off `families` would name whatever sorts first instead.
    default_family: Option<String>,
}

impl Default for FontProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FontProvider {
    /// A provider with no fonts at all.
    ///
    /// Legal and occasionally useful — the accessibility layer describes text
    /// from its label and never needs a glyph — but shaping anything that is not
    /// whitespace will report every character as missing. That is the honest
    /// answer: there is no font, so there is no glyph.
    pub fn new() -> Self {
        Self {
            // `CollectionOptions::system_fonts` defaults to true, and with the
            // `system` feature compiled out the backend is a stub — so this is
            // belt and braces. It is also the one line that would have to change
            // if anyone ever compiled fontique's `system` feature back in, and
            // that change should be a deliberate one.
            fonts: FontContext {
                collection: fontique::Collection::new(CollectionOptions {
                    system_fonts: false,
                    shared: false,
                }),
                source_cache: fontique::SourceCache::default(),
            },
            layout: LayoutContext::new(),
            sources: Vec::new(),
            families: Vec::new(),
            default_family: None,
        }
    }

    /// Every font file in `dir`, non-recursively.
    ///
    /// Recognises what fontique recognises (`.ttf`, `.otf`, `.ttc`, `.otc`) and
    /// ignores anything else rather than failing on it, because a font directory
    /// with a `LICENSE` and a `README` in it is the normal case — this
    /// repository's own `assets/fonts/` is one.
    pub fn from_dir(dir: &Path) -> io::Result<Self> {
        let mut provider = Self::new();
        let mut entries: Vec<PathBuf> = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_file() && has_font_extension(&path) {
                entries.push(path);
            }
        }
        // `read_dir` order is the filesystem's, which differs between platforms
        // and would make which face ends up as the generic family's first choice
        // depend on the machine. Sorting makes a directory of faces reproduce.
        entries.sort();
        for path in entries {
            provider.add_font_file(&path)?;
        }
        Ok(provider)
    }

    /// The fonts that ship in this repository, and nothing else.
    ///
    /// Reports a missing directory rather than panicking or quietly returning an
    /// empty provider: silently having no CJK font is precisely the failure the
    /// bundling exists to prevent, and it would otherwise show up as a scene
    /// full of blanks on the one machine that has no system fonts.
    pub fn with_bundled() -> io::Result<Self> {
        Self::from_dir(Path::new(BUNDLED_DIR))
    }

    /// Register every face in one font file.
    ///
    /// Returns the family names it added. For a `.ttc` that is every face the
    /// file contains — Noto Sans CJK, for instance, is five (SC, TC, JP, KR, HK)
    /// — because `register_fonts` scans the whole collection and there is no way
    /// to register one face of it. That is the behaviour we want anyway: a
    /// Japanese character in a mostly-Chinese scene should reach the JP face
    /// through fontique's script-based fallback rather than through a second
    /// registration.
    pub fn add_font_file(&mut self, path: &Path) -> io::Result<Vec<String>> {
        let data = std::fs::read(path)?;
        let blob = Blob::from(data);
        // The blob's id is what `register_fonts` carries into the `FontInfo`s it
        // builds, and what a shaped run's font handle can be traced back through,
        // so recording it here is what makes `source_of` possible.
        self.sources.push((blob.id(), path.to_path_buf()));
        Ok(self.register(blob))
    }

    /// Register every face in a font already in memory.
    ///
    /// The form the browser build and any caller with a font in a buffer wants:
    /// no path, no filesystem. Returns the family names registered.
    ///
    /// There is deliberately no `index` parameter, though the plan sketched one:
    /// fontique registers a blob, not a face, and offers no way to take one face
    /// out of a collection — so an index here would be a parameter that could not
    /// be honoured.
    pub fn add_font_data(&mut self, data: Arc<dyn AsRef<[u8]> + Send + Sync>) -> Vec<String> {
        self.register(Blob::new(data))
    }

    /// Register every face in one blob, and report the families it added.
    ///
    /// # Why this sorts, and why the generic family is one name
    ///
    /// `register_fonts` builds its result in a `HashMap` and returns it in that
    /// map's iteration order, which is stable for a given key set within a
    /// process and not between processes. Everything downstream that picks "the
    /// first" therefore picks at random per run — see [`BUNDLED_FAMILY`] for what
    /// that cost. Two lines here are the whole defence: the families are sorted
    /// before anything reads them, and the generic slots are given **one** id
    /// rather than the whole list, so first-match-wins has only one candidate.
    fn register(&mut self, blob: Blob<u8>) -> Vec<String> {
        let registered = self.fonts.collection.register_fonts(blob, None);
        let mut found: Vec<(FamilyId, String)> = registered
            .iter()
            .filter_map(|(id, _fonts)| {
                self.fonts
                    .collection
                    .family_name(*id)
                    .map(|name| (*id, name.to_owned()))
            })
            .collect();
        found.sort_by(|a, b| a.1.cmp(&b.1));

        // The first registration names the generic family, so that ordinary text
        // with no family named shapes in *something* rather than falling through
        // to a fallback list that is also empty. Doing it here rather than in
        // `with_bundled` means any provider — a test's, a caller's, the
        // browser's — gets the same property. Later registrations do not get a
        // vote: which of two files a face comes from is the caller's business,
        // and a provider that says "this one is my default" by registering it
        // first should not have that silently overridden.
        if self.families.is_empty() {
            let chosen = found
                .iter()
                .find(|(_, name)| name == BUNDLED_FAMILY)
                .or_else(|| found.first());
            if let Some((id, name)) = chosen {
                for generic in [
                    GenericFamily::SansSerif,
                    GenericFamily::Serif,
                    GenericFamily::Monospace,
                ] {
                    // One id, not `found.iter()`. `GenericFamilyMap` is an ordered
                    // list searched first-match-wins, so handing over every face
                    // in the file is what made the choice depend on the map above.
                    self.fonts
                        .collection
                        .set_generic_families(generic, std::iter::once(*id));
                }
                self.default_family = Some(name.clone());
            }
        }
        let added: Vec<String> = found.into_iter().map(|(_, name)| name).collect();
        self.families.extend(added.iter().cloned());
        added
    }

    /// The family names this provider knows, sorted.
    pub fn families(&self) -> &[String] {
        &self.families
    }

    /// The family a [`TextStyle`] with no `family` shapes in, or `None` if the
    /// provider has no fonts.
    pub fn default_family(&self) -> Option<&str> {
        self.default_family.as_deref()
    }

    /// Whether any font is registered.
    pub fn is_empty(&self) -> bool {
        self.families.is_empty()
    }

    /// The file a blob came from, if it was registered from one.
    pub fn source_of(&self, blob_id: u64) -> Option<&Path> {
        self.sources
            .iter()
            .find(|(id, _)| *id == blob_id)
            .map(|(_, p)| p.as_path())
    }

    /// Shape `text` at `style.size` world units per em.
    ///
    /// `unit_scale` says how many of the caller's coordinates one world unit is,
    /// and everything in the result — the font size, every glyph offset, `width`,
    /// `height` — comes back in those coordinates. **It is 1.0 in the ordinary
    /// case**, where the caller draws the run under the viewport transform: the
    /// backend multiplies the run's transform into the glyph geometry itself
    /// (glifo absorbs the scale, and hints against the size it produces), so the
    /// run is simply in world units and the viewport does the rest. Pass
    /// [`rustim_math::Viewport::pixels_per_unit`] for the current camera instead
    /// only for a caller that draws in the draw list's own pixel space with no
    /// transform of its own.
    ///
    /// It is an argument rather than a field because it belongs to the *scene*:
    /// the provider has no business caching a frame size it could be asked about
    /// from two windows at once.
    ///
    /// `max_width` is the wrap width in the caller's units, or `None` for no
    /// wrapping — which is Manim's behaviour for `Text`, and the one a scene
    /// wants when it is about to place the result itself. Line breaking is
    /// parley's, through `icu_segmenter`, so CJK breaks between characters and
    /// Latin between words without this crate knowing either rule.
    pub fn shape(
        &mut self,
        text: &str,
        style: &TextStyle,
        unit_scale: f64,
        max_width: Option<f32>,
    ) -> ShapedText {
        let size = (style.size * unit_scale) as f32;
        // Resolved before the builder exists: `ranged_builder` holds the font
        // context mutably for the whole build, and the lookup needs it too.
        let (family, substituted): (FontFamily<'_>, Option<FontSubstitution>) =
            match style.family.as_deref() {
                Some(name) if self.fonts.collection.family_id(name).is_some() => {
                    (FontFamily::named(name), None)
                }
                // Either no family was asked for, or the one asked for is not
                // here. Both take the generic family, which `register` points at
                // the first font registered: `FontFamily::Source` parses a CSS
                // list, and "sans-serif" is the generic name CSS gives it.
                requested => (
                    FontFamily::Source(std::borrow::Cow::Borrowed("sans-serif")),
                    requested.map(|requested| FontSubstitution {
                        requested: requested.to_owned(),
                        // What the generic family actually resolves to, not what
                        // sorts first: the two are the same family only by
                        // accident, and a report that names the wrong face is
                        // worse than no report at all.
                        used: self.default_family.clone(),
                    }),
                ),
            };
        let mut builder = self
            .layout
            .ranged_builder(&mut self.fonts, text, 1.0, false);
        builder.push_default(StyleProperty::FontSize(size));
        builder.push_default(StyleProperty::FontFamily(family));
        if style.bold {
            builder.push_default(StyleProperty::FontWeight(parley::FontWeight::BOLD));
        }
        if style.italic {
            builder.push_default(StyleProperty::FontStyle(parley::FontStyle::Italic));
        }
        let mut layout = builder.build(text);
        layout.break_all_lines(max_width);
        layout.align(
            parley::Alignment::Start,
            parley::AlignmentOptions::default(),
        );

        let mut shaped = ShapedText {
            runs: Vec::new(),
            width: layout.width(),
            height: layout.height(),
            missing: Vec::new(),
            substituted,
        };
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                    // An inline box: nothing in this workspace emits one, and a
                    // box has no glyphs to position.
                    continue;
                };
                let run = glyph_run.run();
                // Two passes over the run, because the two questions need
                // different iterators. The *report* needs the source text, which
                // only the cluster carries; the *glyphs* need their positions,
                // which only the positioned iterator computes. `Cluster::glyphs`
                // returns unpositioned glyphs — every `x` is zero — so drawing
                // from it would stack a whole run at the origin.
                for cluster in run.clusters() {
                    if cluster.is_space_or_nbsp() || !cluster.glyphs().any(|g| g.id == 0) {
                        continue;
                    }
                    let range = cluster.text_range();
                    let characters = text.get(range.clone()).unwrap_or_default().to_owned();
                    shaped.missing.push(MissingGlyph {
                        codepoints: characters.chars().map(u32::from).collect(),
                        characters,
                        index: range.start,
                    });
                }
                let glyphs: Vec<TextGlyph> = glyph_run
                    .positioned_glyphs()
                    .filter(|g| {
                        // Glyph 0 is `.notdef`: the face has no glyph for what
                        // this is, so there is nothing to draw. It is dropped
                        // rather than drawn as a box, and dropped rather than
                        // skipped in place, so that everything after it keeps the
                        // position the shaper gave it — the hole left behind is
                        // exactly the missing advance.
                        g.id != 0
                    })
                    .map(|g| TextGlyph {
                        id: g.id,
                        x: g.x,
                        y: g.y,
                    })
                    .collect();
                if glyphs.is_empty() {
                    continue;
                }
                shaped.runs.push(TextRun {
                    font: run.font().clone(),
                    size: run.font_size(),
                    glyphs,
                });
            }
        }
        shaped
    }
}

fn has_font_extension(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        e.eq_ignore_ascii_case("ttf")
            || e.eq_ignore_ascii_case("otf")
            || e.eq_ignore_ascii_case("ttc")
            || e.eq_ignore_ascii_case("otc")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The common case: the run is drawn under the viewport transform, so the
    /// shaper's units are already world units.
    const UNIT_SCALE: f64 = 1.0;

    fn bundled() -> FontProvider {
        FontProvider::with_bundled().expect("assets/fonts must be in the repository")
    }

    #[test]
    fn a_provider_with_no_fonts_is_honest_about_it() {
        let mut p = FontProvider::new();
        assert!(p.is_empty());
        let shaped = p.shape("hi", &TextStyle::default(), UNIT_SCALE, None);
        assert!(shaped.is_empty(), "no font means no glyphs");
    }

    #[test]
    fn the_bundled_font_registers_its_families() {
        let p = bundled();
        assert!(
            p.families().iter().any(|f| f.contains("Noto Sans CJK")),
            "expected a Noto Sans CJK family, got {:?}",
            p.families()
        );
    }

    #[test]
    fn latin_text_shapes_and_is_positioned_left_to_right() {
        let mut p = bundled();
        let shaped = p.shape("abc", &TextStyle::default(), UNIT_SCALE, None);
        assert!(shaped.missing.is_empty());
        assert_eq!(shaped.runs.len(), 1, "one face covers all of `abc`");
        let xs: Vec<f32> = shaped.runs[0].glyphs.iter().map(|g| g.x).collect();
        assert_eq!(xs.len(), 3);
        assert!(xs[0] < xs[1] && xs[1] < xs[2], "got {xs:?}");
        assert!(shaped.width > 0.0 && shaped.height > 0.0);
    }

    #[test]
    fn cjk_text_shapes_too() {
        let mut p = bundled();
        let shaped = p.shape("勾股定理", &TextStyle::default(), UNIT_SCALE, None);
        assert!(
            shaped.missing.is_empty(),
            "the bundled face must cover CJK: {:?}",
            shaped.missing
        );
        let glyphs: usize = shaped.runs.iter().map(|r| r.glyphs.len()).sum();
        assert_eq!(glyphs, 4);
    }

    #[test]
    fn a_character_no_font_has_is_reported_and_not_drawn() {
        let mut p = bundled();
        // An Egyptian hieroglyph is not in Noto Sans CJK, and is not in the
        // Private Use Area — so this is a genuine "no font here has it" rather
        // than a codepoint a fallback face happens to cover.
        let shaped = p.shape("a\u{13000}b", &TextStyle::default(), UNIT_SCALE, None);
        assert_eq!(shaped.missing.len(), 1, "{:?}", shaped.missing);
        assert_eq!(shaped.missing[0].codepoints, vec![0x13000]);
        assert_eq!(shaped.missing[0].characters, "\u{13000}");
        assert_eq!(shaped.missing[0].index, 1);
        let glyphs: usize = shaped.runs.iter().map(|r| r.glyphs.len()).sum();
        assert_eq!(glyphs, 2, "the supported characters still draw");
        let report = shaped.missing_report().expect("a report");
        assert!(report.contains("U+13000"), "{report}");
    }

    #[test]
    fn a_missing_glyph_leaves_a_hole_where_the_shaper_put_it() {
        // The two halves of dropping a `.notdef`, and both matter. *Nothing
        // closes up*: the glyphs after the hole keep the positions the shaper
        // gave them, rather than sliding left as they would if the run had been
        // rebuilt from what survived. And the hole has the width the shaper
        // allotted, which for this face is a full em — measured, not assumed,
        // because a face that gave `.notdef` a zero advance would leave the
        // characters touching, and that is worth knowing before a golden image
        // shows it.
        let mut p = bundled();
        let intact = p.shape("ab", &TextStyle::default(), UNIT_SCALE, None);
        let holed = p.shape("a\u{13000}b", &TextStyle::default(), UNIT_SCALE, None);
        let last_x = |s: &ShapedText| {
            s.runs
                .iter()
                .flat_map(|r| &r.glyphs)
                .next_back()
                .expect("a glyph to place")
                .x
        };
        let em = (TextStyle::default().size * UNIT_SCALE) as f32;
        let shifted = last_x(&holed) - last_x(&intact);
        assert!(
            (shifted - em).abs() < 1e-2,
            "expected the `b` to sit exactly one em ({em}) further right, not {shifted}"
        );
    }

    #[test]
    fn a_substituted_family_is_reported() {
        // The failure this exists to prevent: asking for a font the deployment
        // does not carry, and getting a blank frame with no explanation.
        let mut p = bundled();
        let style = TextStyle {
            family: Some("Helvetica Neue Ultra Compressed".into()),
            ..Default::default()
        };
        let shaped = p.shape("abc", &style, UNIT_SCALE, None);
        let substituted = shaped.substituted.expect("expected a substitution");
        assert_eq!(substituted.requested, "Helvetica Neue Ultra Compressed");
        assert!(
            substituted
                .used
                .as_deref()
                .is_some_and(|f| f.contains("Noto")),
            "expected the bundled face, got {:?}",
            substituted.used
        );

        // A family that *is* registered reports nothing.
        let style = TextStyle {
            family: Some("Noto Sans CJK SC".into()),
            ..Default::default()
        };
        assert!(
            p.shape("abc", &style, UNIT_SCALE, None)
                .substituted
                .is_none(),
            "a present family is not a substitution"
        );
    }

    #[test]
    fn wrapping_breaks_lines_and_grows_the_height() {
        let mut p = bundled();
        let style = TextStyle {
            size: 0.5,
            ..Default::default()
        };
        let one = p.shape("one two three four", &style, UNIT_SCALE, None);
        let wrapped = p.shape(
            "one two three four",
            &style,
            UNIT_SCALE,
            Some(one.width / 3.0),
        );
        assert!(
            wrapped.height > one.height,
            "expected the wrapped text to be taller: {} vs {}",
            wrapped.height,
            one.height
        );
        assert!(wrapped.width < one.width);
    }

    #[test]
    fn a_whitespace_only_string_has_no_glyphs_and_no_complaint() {
        let mut p = bundled();
        let shaped = p.shape("   ", &TextStyle::default(), UNIT_SCALE, None);
        assert!(shaped.missing.is_empty(), "spaces are not a font problem");
    }

    #[test]
    fn a_name_that_matches_nothing_still_draws() {
        // `fontique` falls back rather than failing, which is the right answer
        // for text: a scene that names a font the deployment does not carry
        // should produce readable text, not nothing.
        let mut p = bundled();
        let style = TextStyle {
            family: Some("Helvetica Neue Ultra Compressed".into()),
            ..Default::default()
        };
        let shaped = p.shape("abc", &style, UNIT_SCALE, None);
        assert!(!shaped.is_empty());
        assert!(shaped.missing.is_empty());
    }

    #[test]
    fn files_that_are_not_fonts_are_ignored() {
        // `assets/fonts/` contains a LICENSE; a directory walker that treated
        // every file as a font would fail on it.
        let p = bundled();
        assert!(!p.is_empty());
        assert!(
            !p.families().iter().any(|f| f.contains("LICENSE")),
            "{:?}",
            p.families()
        );
    }

    #[test]
    fn the_families_come_back_in_a_stable_order() {
        // The order `register_fonts` returns them in is a `HashMap`'s, and is
        // different in every process — which is the whole bug. Sorting is what
        // makes anything downstream that reads "the families" reproducible, so it
        // is asserted directly rather than only through its effects.
        let p = bundled();
        let sorted: Vec<&String> = {
            let mut v: Vec<&String> = p.families().iter().collect();
            v.sort();
            v
        };
        assert!(
            p.families().iter().collect::<Vec<_>>() == sorted,
            "families must come back sorted: {:?}",
            p.families()
        );
        assert!(p.families().len() > 1, "the .ttc holds several faces");
    }

    #[test]
    fn the_generic_family_is_one_face_and_it_is_the_bundled_one() {
        // Two properties, and the bug needed both to be wrong. *One* id, because
        // the generic map is searched first-match-wins and a list of six faces is
        // a coin toss per process. And *the named one*, because "whatever the map
        // yielded first" is not a choice anyone made.
        let mut p = bundled();
        let ids: Vec<_> = p
            .fonts
            .collection
            .generic_families(GenericFamily::SansSerif)
            .collect();
        assert_eq!(
            ids.len(),
            1,
            "a generic family of six candidates is a coin toss"
        );
        assert_eq!(
            Some(ids[0]),
            p.fonts.collection.family_id(BUNDLED_FAMILY),
            "{BUNDLED_FAMILY} must be a registered family"
        );
        assert_eq!(p.default_family(), Some(BUNDLED_FAMILY));

        // And it is the same for the other two slots, so a scene that asks for a
        // serif still gets text rather than nothing.
        for generic in [GenericFamily::Serif, GenericFamily::Monospace] {
            assert_eq!(
                p.fonts.collection.generic_families(generic).count(),
                1,
                "{generic:?} must resolve to exactly one face"
            );
        }
    }

    #[test]
    fn the_default_family_is_the_one_a_generic_run_draws_in() {
        // The claim `default_family` makes has to be true of the render, and a
        // face is not named on a run — it is addressed by (blob, collection
        // index) — so the test is a comparison of two shaped runs. This is also
        // the direct statement of the bug: these two used to differ, and which
        // way they differed was decided per process.
        let mut p = bundled();
        let generic = p.shape("勾股定理", &TextStyle::default(), UNIT_SCALE, None);
        let named = p.shape(
            "勾股定理",
            &TextStyle {
                family: Some(BUNDLED_FAMILY.to_owned()),
                ..Default::default()
            },
            UNIT_SCALE,
            None,
        );
        let face = |s: &ShapedText| {
            let r = s.runs.first().expect("a glyph run");
            (r.font.data.id(), r.font.index)
        };
        assert_eq!(
            face(&generic),
            face(&named),
            "a run with no family named must draw in {BUNDLED_FAMILY}"
        );
    }

    #[test]
    fn a_named_family_still_resolves_by_name() {
        // The fix sorts what a *generic* request sees; a *named* request has to
        // keep going through the lookup, or asking for one face would start
        // answering with another.
        let mut p = bundled();
        let faces: Vec<_> = ["Noto Sans CJK TC", "Noto Sans CJK HK"]
            .into_iter()
            .map(|name| {
                let shaped = p.shape(
                    "abc",
                    &TextStyle {
                        family: Some(name.to_owned()),
                        ..Default::default()
                    },
                    UNIT_SCALE,
                    None,
                );
                assert!(shaped.substituted.is_none(), "{name} is registered");
                let r = shaped.runs.first().expect("a glyph run");
                (r.font.data.id(), r.font.index)
            })
            .collect();
        assert_ne!(
            faces[0], faces[1],
            "TC and HK are different faces of the collection"
        );
    }
}
