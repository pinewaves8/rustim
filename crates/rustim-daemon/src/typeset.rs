//! The daemon's typesetter: parley for text, Typst for formulas.
//!
//! This is the third host of [`rustim_scene::Typesetter`], after the desktop
//! binary (`rustim_app::typeset`) and the browser (`rustim_web::typeset`), and
//! it exists for the same reason they do: the trait is the kernel's seam and the
//! implementation belongs to whoever owns the fonts. A daemon is a *remote*
//! host, but it is not a thinner one — an assistant driving it over a socket
//! sends exactly the `{"action":"add","type":"text"}` a script does, and a
//! daemon that refused every label and formula would be unusable for the thing
//! this engine exists for. `hello` therefore promises text and formula, and
//! this provider honours the promise.
//!
//! The implementation is deliberately the same as the desktop's, restated
//! rather than shared: `rustim-app` links winit, softbuffer and `accesskit`, and
//! a server binary must not drag a window system into a headless deployment for
//! one trait impl. The duplication is the established cost of the seam — the
//! browser's provider is a third copy — and what keeps it honest is that the
//! kernel's own tests drive the *trait's* contract, not any one host's.
//!
//! # Everything expensive is lazy, and failures are cached
//!
//! `FontProvider::with_bundled` parses nineteen megabytes of Noto Sans CJK;
//! `FormulaCompiler::new` additionally builds Typst's library. A session that
//! never typesets must not pay either cost, so both are built on first use and
//! kept in the provider. A *failed* build is kept too, as text: an installed
//! daemon whose `BUNDLED_DIR` points at a checkout that is not there would
//! otherwise re-report the same missing directory on every `add`.
//!
//! Each session owns one provider. That means a daemon with several text-heavy
//! sessions holds the font set once per session; sharing one across sessions
//! would need a lock around a `&mut self` trait and is deliberately deferred —
//! the common deployment is one assistant, one session.

use rustim_mobject::{SceneTree, TextBlock};
use rustim_scene::{TextSpec, TypesetError, TypesetFormula, Typesetter};
use rustim_text::formula::{FormulaCompiler, FormulaError};
use rustim_text::{FontProvider, TextStyle};

/// The daemon's shaper and formula compiler, built on first use.
pub struct DaemonTypesetter {
    fonts: Option<Result<FontProvider, String>>,
    formula: Option<Result<FormulaCompiler, String>>,
}

impl DaemonTypesetter {
    pub fn new() -> Self {
        Self {
            fonts: None,
            formula: None,
        }
    }

    fn fonts(&mut self) -> Result<&mut FontProvider, TypesetError> {
        let slot = self.fonts.get_or_insert_with(|| {
            FontProvider::with_bundled()
                .map_err(|err| format!("could not read {}: {err}", rustim_text::BUNDLED_DIR))
        });
        slot.as_mut()
            .map_err(|detail| TypesetError::failed(detail.clone()))
    }

    fn compiler(&mut self) -> Result<&mut FormulaCompiler, TypesetError> {
        let slot = self
            .formula
            .get_or_insert_with(|| FormulaCompiler::new().map_err(|err| err.to_string()));
        slot.as_mut()
            .map_err(|detail| TypesetError::failed(detail.clone()))
    }
}

impl Default for DaemonTypesetter {
    fn default() -> Self {
        Self::new()
    }
}

impl Typesetter for DaemonTypesetter {
    fn supports_text(&self) -> bool {
        true
    }

    fn supports_formula(&self) -> bool {
        true
    }

    fn text(&mut self, text: &str, spec: &TextSpec) -> Result<TextBlock, TypesetError> {
        // The two refusals a model most easily sends by accident, worded as the
        // desktop host words them: an empty string would create an object that
        // draws nothing, and a non-positive size likewise.
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
        // `unit_scale` is 1.0: the size is already in world units, which is what
        // the run is drawn in under the frame transform (see `rustim_app::text`).
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
        if source.trim().is_empty() {
            return Err(TypesetError::invalid("the formula is empty"));
        }
        let compiled = self.compiler()?.compile(source).map_err(|err| match err {
            // A source Typst could not parse is the caller's to fix; a font set
            // that could not be assembled is the deployment's. The two variants
            // carry exactly that difference.
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

    /// The daemon's promise to an AI client: a Chinese label becomes real
    /// geometry at the size asked for, from the bundled font directory.
    #[test]
    fn text_is_shaped_at_the_requested_size() {
        let mut typesetter = DaemonTypesetter::new();
        let block = typesetter
            .text(
                "两条直角边",
                &TextSpec {
                    size: Some(1.0),
                    max_width: None,
                },
            )
            .expect("the bundled fonts");
        assert!(block.width > 0.0 && block.height > 0.0, "{block:?}");
        assert!(!block.is_empty());
        assert!(block.runs.iter().all(|run| !run.glyphs.is_empty()));
        assert!((f64::from(block.runs[0].size) - 1.0).abs() < 1e-6);
    }

    /// A formula arrives as a subtree with an addressable root, which is what
    /// `add` returns to the client.
    #[test]
    fn a_formula_inserts_one_subtree() {
        let mut typesetter = DaemonTypesetter::new();
        let mut tree = SceneTree::new();
        let formula = typesetter
            .formula("x^2 + frac(1, 2)", &mut tree)
            .expect("the bundled fonts and Typst");
        assert!(!formula.nodes.is_empty());
        assert_eq!(tree.child_ids(formula.root).len(), formula.nodes.len());
        assert_eq!(
            tree.get(formula.root).unwrap().kind,
            rustim_mobject::NodeKind::Formula
        );
    }

    #[test]
    fn an_empty_source_is_the_callers_to_fix() {
        let mut typesetter = DaemonTypesetter::new();
        let mut tree = SceneTree::new();
        for error in [
            typesetter.text("", &TextSpec::default()).unwrap_err(),
            typesetter.formula("  ", &mut tree).unwrap_err(),
        ] {
            assert!(matches!(error, TypesetError::Invalid { .. }), "{error:?}");
        }
    }
}
