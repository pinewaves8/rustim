//! The seam between the kernel and whatever typesets text and formulas.
//!
//! A text mobject's picture is a glyph run, and only a shaper can produce one.
//! The shaper (`rustim-text`: parley, fontique, Typst) is deliberately **not** a
//! kernel crate — it parses fonts, reads `assets/fonts`, and links a typesetting
//! engine — while `rustim-scene` has to compile to `wasm32-unknown-unknown` and
//! must not depend on any of that. So the *capability* crosses the boundary as a
//! trait the host installs, and what comes back is plain data
//! ([`rustim_mobject::TextBlock`]) the kernel can draw and measure.
//!
//! This is the same shape as the accessibility split one layer up: the kernel
//! declares what it needs, the host supplies the one implementation it can, and
//! a host that has none says so instead of pretending. The desktop binary
//! installs `rustim_app::typeset::AppTypesetter` at every `SceneContext::new`
//! site; the browser's default build installs none and therefore refuses
//! `"type":"text"` honestly, through [`CommandError::Unsupported`].
//!
//! Three rules make the seam safe to widen:
//!
//! - **The trait has no I/O.** It takes a string and returns geometry; a host
//!   that reads a file inside its implementation does so on its own side of the
//!   boundary, and the kernel never learns what a font directory is.
//! - **Refusals are values.** A model can write a formula Typst rejects, so both
//!   methods return a [`TypesetError`] and the command layer turns it into a
//!   [`CommandError`] — never a panic.
//! - **Supports is asked before the method.** A host that can typeset formulas
//!   but not text advertises exactly that, and the command layer checks it so a
//!   client can discover the difference through `hello` rather than by sending a
//!   command and reading the refusal.
//!
//! [`CommandError`]: crate::CommandError

use rustim_mobject::{NodeId, SceneTree, TextBlock};

/// What a caller wants shaped, beyond the string itself.
///
/// Deliberately small and free of colour: a text run's paint is the mobject's
/// `Style`, folded into its `opacity` at draw time like every other shape's, so
/// a `set` of `opacity` or `fill_color` reaches wire-created text with no new
/// command. What cannot be expressed as a style is the *layout* — the em size
/// and the wrap width — and that is all this carries.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TextSpec {
    /// The em size in world units. Absent is the shaper's default.
    pub size: Option<f64>,
    /// The width to wrap at, in world units. Absent is a single line.
    pub max_width: Option<f64>,
}

/// A formula the typesetter compiled into the tree.
///
/// The geometry of a formula is a whole subtree: one mobject per glyph plus the
/// rules (a fraction's bar, a radical's overline) the compiler turned into
/// filled quads. The *root* is what the `add` command returns as its id — it is
/// the node a label, a transform and an animation address, exactly as a compiled
/// scene's `Formula::insert` would be addressed. Everything else is listed so
/// the command layer can colour it and report it in the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypesetFormula {
    /// The container node: `NodeKind::Formula`, no points of its own.
    pub root: NodeId,
    /// Every other node the compile inserted — glyphs and rules. Excludes the
    /// root, which has its own field.
    pub nodes: Vec<NodeId>,
}

/// Why a typesetter could not produce geometry.
///
/// The two variants are the two different fixes, which is why they are not one
/// string: `Invalid` means the *caller* asked for something that is not
/// typesettable (an empty string, a formula Typst cannot parse) and the caller
/// can rewrite it; `Failed` means the host could not do it at all (no font
/// directory, an engine failure) and rewriting the request will not help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypesetError {
    /// The source is not something that can be typeset.
    Invalid { detail: String },
    /// The backend could not run, or does not provide this kind at all.
    Failed { detail: String },
}

impl TypesetError {
    pub fn invalid(detail: impl Into<String>) -> Self {
        Self::Invalid {
            detail: detail.into(),
        }
    }

    pub fn failed(detail: impl Into<String>) -> Self {
        Self::Failed {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for TypesetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid { detail } => write!(f, "cannot typeset: {detail}"),
            Self::Failed { detail } => write!(f, "typesetter failed: {detail}"),
        }
    }
}

impl std::error::Error for TypesetError {}

/// The typesetting capability a host can install on a [`SceneContext`].
///
/// [`SceneContext`]: crate::SceneContext
///
/// A host installs at most one, because the vocabulary a client is told about
/// (`hello`'s `capabilities`) has one answer per build: "this build can typeset
/// text" is a fact about the binary, not about which of two providers answered.
/// A future multi-provider setup would pick one at construction, which is where
/// the choice belongs anyway.
pub trait Typesetter {
    /// Whether [`Typesetter::text`] produces geometry in this build.
    ///
    /// `false` by default, and that direction is deliberate: a provider that
    /// forgets to override this advertises *less* than it can do, and a client
    /// that believes it loses a feature but never receives a promise the engine
    /// breaks. The opposite default would turn every forgotten override into a
    /// capability list that lies.
    fn supports_text(&self) -> bool {
        false
    }

    /// Whether [`Typesetter::formula`] inserts geometry in this build.
    fn supports_formula(&self) -> bool {
        false
    }

    /// Shape a string into a block the kernel can draw.
    ///
    /// `&mut self` because shaping is stateful in practice — a `FontProvider`
    /// caches laid-out runs, and a `FormulaCompiler` caches its parsed font set
    /// — and a `&self` signature here would force every implementation into
    /// interior mutability for no benefit.
    fn text(&mut self, _text: &str, _spec: &TextSpec) -> Result<TextBlock, TypesetError> {
        Err(TypesetError::failed("this host does not typeset text"))
    }

    /// Compile a math fragment and **insert** its nodes into `tree`.
    ///
    /// Insertion rather than a returned value is the contract because a formula
    /// is a subtree, and the tree is where nodes live. The returned
    /// [`TypesetFormula`] says which nodes are new so the caller can report them
    /// in its diff and style them; the root is left untouched otherwise, which
    /// is what lets the command layer apply a label, a colour and a transform to
    /// the whole formula in one place.
    ///
    /// The `tree` is borrowed mutably for the whole call, so an implementation
    /// must not try to read the node it is styling — it styles nothing here.
    fn formula(
        &mut self,
        _source: &str,
        _tree: &mut SceneTree,
    ) -> Result<TypesetFormula, TypesetError> {
        Err(TypesetError::failed("this host does not typeset formulas"))
    }
}
