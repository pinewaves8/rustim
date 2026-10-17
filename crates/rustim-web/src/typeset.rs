//! The browser's typesetter: formulas only, from Typst's embedded fonts.
//!
//! The desktop's provider ([`rustim_app::typeset::AppTypesetter`]) is not
//! reusable here for one concrete reason: it reads `assets/fonts/` through
//! `FontProvider::with_bundled`, and a browser has no filesystem. Typst's
//! embedded asset set (`typst-assets`) carries the math faces — New Computer
//! Modern Math among them — and nothing else this page needs, so the formula
//! page can typeset with no font files at all. Text, which would need a CJK face
//! the embedded set does not carry, is *not* supported: see
//! [`Typesetter::supports_text`] below and the CJK-subset item in `CLAUDE.md`'s
//! deferred list.
//!
//! This module exists only under the `formula` feature, because that is the only
//! artifact that links Typst: the plain demo's `.wasm` must not grow by fourteen
//! megabytes for a capability its page never asks for.

use rustim_scene::{SceneTree, TextBlock, TextSpec, TypesetError, TypesetFormula, Typesetter};
use rustim_text::formula::{FormulaCompiler, FormulaError};

/// Typst through `from_embedded`, built on the first formula.
///
/// The compiler holds Typst's library and font set; building it is the expensive
/// step in the whole artifact and the page usually wants it once, so it is lazy
/// for the same reason the desktop provider's fields are (see
/// `rustim_app::typeset`). The failure is cached as text too: a browser has no
/// terminal to report a panic to, and the command reply is the only channel.
pub struct WebTypesetter {
    compiler: Option<Result<FormulaCompiler, String>>,
}

impl WebTypesetter {
    pub fn new() -> Self {
        Self { compiler: None }
    }

    fn compiler(&mut self) -> Result<&mut FormulaCompiler, TypesetError> {
        let slot = self.compiler.get_or_insert_with(|| {
            FormulaCompiler::from_embedded().map_err(|err| {
                format!("the formula compiler could not load its embedded fonts: {err}")
            })
        });
        slot.as_mut()
            .map_err(|detail| TypesetError::failed(detail.clone()))
    }
}

impl Default for WebTypesetter {
    fn default() -> Self {
        Self::new()
    }
}

impl Typesetter for WebTypesetter {
    /// `false`, and it is a fact about the *artifact*: the browser build has no
    /// font provider, so `hello` must not promise text. This is the half of the
    /// seam the feature split is for.
    fn supports_text(&self) -> bool {
        false
    }

    fn supports_formula(&self) -> bool {
        true
    }

    fn text(&mut self, _text: &str, _spec: &TextSpec) -> Result<TextBlock, TypesetError> {
        // The command layer checks `supports_text` before calling, so this is
        // unreachable through the wire; it is a value rather than a panic
        // because a direct caller can reach it.
        Err(TypesetError::failed(
            "this build has no text shaper; only formulas are typeset in the browser",
        ))
    }

    fn formula(
        &mut self,
        source: &str,
        tree: &mut SceneTree,
    ) -> Result<TypesetFormula, TypesetError> {
        if source.trim().is_empty() {
            return Err(TypesetError::invalid("the formula is empty"));
        }
        // The same mapping the desktop provider uses, restated rather than
        // shared: the two hosts have no crate in common below the seam, and a
        // shared helper would have to live in `rustim-scene`, which must not
        // know that Typst exists.
        let compiled = self.compiler()?.compile(source).map_err(|err| match err {
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

    /// The page's own fragment compiles through the embedded font set, which is
    /// what the formula page depends on: no filesystem, no `assets/fonts`.
    #[test]
    fn a_formula_compiles_from_the_embedded_fonts() {
        let mut typesetter = WebTypesetter::new();
        let mut tree = SceneTree::new();
        let formula = typesetter
            .formula("x^2 + frac(1, 2)", &mut tree)
            .expect("the embedded fonts");
        assert!(!formula.nodes.is_empty());
        assert_eq!(
            tree.child_ids(formula.root).len(),
            formula.nodes.len(),
            "one subtree, one root"
        );
    }

    /// The half the browser does *not* do, refused as a value.
    #[test]
    fn text_is_not_supported_and_says_so() {
        let mut typesetter = WebTypesetter::new();
        assert!(!typesetter.supports_text());
        assert!(typesetter.supports_formula());
        assert!(typesetter.text("你好", &TextSpec::default()).is_err());
    }
}
