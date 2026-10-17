//! The desktop host's typesetter: parley for text, Typst for formulas.
//!
//! [`rustim_scene::Typesetter`] is the kernel's half of this seam — a trait with
//! no fonts in it. This module is the desktop's answer: the one place in the
//! binary that links a shaper, installed on every `SceneContext` it builds. The
//! browser host answers it differently (a formula-only provider on the formula
//! page, none on the main one), which is exactly why the seam exists.
//!
//! # Everything expensive is lazy
//!
//! `FontProvider::with_bundled` parses nineteen megabytes of Noto Sans CJK, and
//! `FormulaCompiler::new` additionally builds Typst's library. A `--png` of a
//! scene that contains no text should not pay either cost, so both are built on
//! the first request and cached in the field that outlives it. The cache is a
//! plain `Option` on the provider rather than a `OnceLock`, `lazy_static` or
//! thread-local: there is one provider per `SceneContext`, it lives on the main
//! thread, and global state is forbidden here for the reasons `CLAUDE.md` gives.
//!
//! A *failed* build is cached too, as a `String`. A missing `assets/fonts/`
//! would otherwise be retried once per command — and it is the expected failure
//! for an installed binary, whose `BUNDLED_DIR` points at a checkout that is not
//! there (see `rustim_text::BUNDLED_DIR`). Caching the error turns "reported
//! rather than assumed away" into one line per run instead of one per `add`.
//!
//! # Supports is a promise about the build, not about the disk
//!
//! [`Typesetter::supports_text`] / [`Typesetter::supports_formula`] answer `true`
//! unconditionally: the binary was compiled with the shaper, and `hello`'s
//! capabilities describe the build's vocabulary. Whether the font directory is
//! reachable on *this* machine is a runtime fact, and the honest place for it is
//! [`TypesetError::Failed`] on the first `add` — "this build can typeset text,
//! and here is why it just could not". Answering `false` from a missing
//! directory would make the handshake describe the deployment's disk, and would
//! differ between a checkout and an installed copy of the same binary.

use rustim_mobject::{SceneTree, TextBlock};
use rustim_scene::{TextSpec, TypesetError, TypesetFormula, Typesetter};
use rustim_text::formula::{FormulaCompiler, FormulaError};
use rustim_text::{FontProvider, TextStyle};

/// The desktop's shaper and formula compiler, built on first use.
pub struct AppTypesetter {
    fonts: Option<Result<FontProvider, String>>,
    formula: Option<Result<FormulaCompiler, String>>,
}

impl AppTypesetter {
    pub fn new() -> Self {
        Self {
            fonts: None,
            formula: None,
        }
    }

    /// The font provider, built once. The error is cached in place and returned
    /// by value on every later call.
    fn fonts(&mut self) -> Result<&mut FontProvider, TypesetError> {
        let slot = self.fonts.get_or_insert_with(|| {
            FontProvider::with_bundled()
                .map_err(|err| format!("could not read {}: {err}", rustim_text::BUNDLED_DIR))
        });
        slot.as_mut()
            .map_err(|detail| TypesetError::failed(detail.clone()))
    }

    /// The formula compiler, built once — it is the expensive one.
    fn compiler(&mut self) -> Result<&mut FormulaCompiler, TypesetError> {
        let slot = self
            .formula
            .get_or_insert_with(|| FormulaCompiler::new().map_err(|err| err.to_string()));
        slot.as_mut()
            .map_err(|detail| TypesetError::failed(detail.clone()))
    }
}

impl Default for AppTypesetter {
    fn default() -> Self {
        Self::new()
    }
}

impl Typesetter for AppTypesetter {
    fn supports_text(&self) -> bool {
        true
    }

    fn supports_formula(&self) -> bool {
        true
    }

    fn text(&mut self, text: &str, spec: &TextSpec) -> Result<TextBlock, TypesetError> {
        // An empty string is the payload a model most easily sends by accident,
        // and the object it would create draws nothing at all — there is no
        // visible object to tell the caller it went wrong. `FormulaError::Empty`
        // is the same refusal on the other half of the seam.
        if text.is_empty() {
            return Err(TypesetError::invalid("the text is empty"));
        }
        let size = spec.size.unwrap_or(TextStyle::default().size);
        if !size.is_finite() || size <= 0.0 {
            return Err(TypesetError::invalid(
                "`size` must be a positive, finite number of world units",
            ));
        }
        let max_width = match spec.max_width {
            Some(width) if !width.is_finite() || width <= 0.0 => {
                return Err(TypesetError::invalid(
                    "`max_width` must be a positive, finite number of world units",
                ));
            }
            Some(width) => Some(width as f32),
            None => None,
        };

        // `unit_scale` is 1.0: the spec's size is already in world units, the
        // units the run is drawn in under the frame transform. See
        // `rustim_app::text` for why the viewport's scale is absorbed by the
        // backend rather than applied to the size here.
        let style = TextStyle {
            size,
            ..TextStyle::default()
        };
        let shaped = self.fonts()?.shape(text, &style, 1.0, max_width);
        Ok(TextBlock::from(&shaped))
    }

    fn formula(
        &mut self,
        source: &str,
        tree: &mut SceneTree,
    ) -> Result<TypesetFormula, TypesetError> {
        // Whitespace-only included: `FormulaError::Empty` reports it too, but
        // the detail is worth saying in the seam's own words rather than as
        // "formula produced no pages".
        if source.trim().is_empty() {
            return Err(TypesetError::invalid("the formula is empty"));
        }

        let compiled = self.compiler()?.compile(source).map_err(|err| match err {
            // A source Typst could not parse is the caller's to fix, and
            // the compile message says where. A font set that could not be
            // assembled is the host's, and rewriting the formula will not
            // help — which is exactly the distinction the two variants
            // carry.
            FormulaError::Compile(message) => TypesetError::invalid(message),
            FormulaError::Empty => TypesetError::invalid("the formula is empty"),
            FormulaError::Fonts(err) => TypesetError::failed(err.to_string()),
        })?;
        let nodes = compiled.insert(tree);
        Ok(TypesetFormula {
            root: nodes.root,
            nodes: nodes.glyphs.into_iter().chain(nodes.rules).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seam's own contract, exercised against the real shaper: a text `add`
    /// becomes geometry the kernel can set on a mobject, and the block keeps the
    /// requested size.
    #[test]
    fn text_is_shaped_at_the_requested_size() {
        let mut ts = AppTypesetter::new();
        let block = ts
            .text(
                "你好",
                &TextSpec {
                    size: Some(1.0),
                    max_width: None,
                },
            )
            .expect("the bundled fonts");
        assert!(block.width > 0.0 && block.height > 0.0, "{block:?}");
        assert!(!block.is_empty());
        assert!(block.runs.iter().all(|run| !run.glyphs.is_empty()));
        // The size is the spec's size in world units, not a multiple of the
        // default: the shape of the run's em is the caller's number.
        assert!((f64::from(block.runs[0].size) - 1.0).abs() < 1e-6);
    }

    /// A formula becomes one subtree, and its root is the id the command
    /// returns: the compiler's own root, with the glyphs underneath it.
    #[test]
    fn a_formula_inserts_one_subtree() {
        let mut ts = AppTypesetter::new();
        let mut tree = SceneTree::new();
        let formula = ts
            .formula("x^2 + 1", &mut tree)
            .expect("the bundled fonts and Typst");
        assert!(!formula.nodes.is_empty(), "the glyphs are nodes too");
        let children = tree.child_ids(formula.root);
        assert_eq!(children.len(), formula.nodes.len());
        assert_eq!(
            tree.get(formula.root).unwrap().kind,
            rustim_mobject::NodeKind::Formula
        );
    }

    /// The refusals are values of the right *kind*: a source the caller can fix
    /// is `Invalid`, and an empty one is the first of those.
    #[test]
    fn an_empty_source_is_the_callers_to_fix() {
        let mut ts = AppTypesetter::new();
        let mut tree = SceneTree::new();
        for error in [
            ts.text("", &TextSpec::default()).unwrap_err(),
            ts.formula("  ", &mut tree).unwrap_err(),
        ] {
            assert!(matches!(error, TypesetError::Invalid { .. }), "{error:?}");
        }
        assert!(matches!(
            ts.text(
                "x",
                &TextSpec {
                    size: Some(-1.0),
                    max_width: None
                }
            ),
            Err(TypesetError::Invalid { .. })
        ));
    }
}
