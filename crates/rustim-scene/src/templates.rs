//! The `explain.*` templates: the teaching tier's thick tools.
//!
//! A primitive command says *what to draw*; a template says *what to explain*.
//! The caller fills slots — a formula, a list of values, a polygon — and the
//! engine decides how the pieces sit relative to one another, what a label says
//! and where it goes. That division is the point of the teaching tier: a small
//! realtime model should not have to work out that a caption sits under a
//! formula, measured from the formula's own ink.
//!
//! # One group, addressable pieces
//!
//! Every template returns a [`NodeKind::Group`] as `created[0]`, with the two
//! dimensions of the picture under it:
//!
//! ```text
//! created = [group, piece, piece, …]
//! ```
//!
//! The group is what a lecture names when it wants to fade, move or highlight
//! the whole explanation as one thing; the pieces stay addressable, so a later
//! `set` or `play` can style one of them. A formula's *glyphs* are not listed —
//! they are the formula's own subtree, exactly as an `add` of a formula reports
//! its root and not its symbols.
//!
//! # The group carries a declared box
//!
//! A group has no points, so [`SceneTree::world_bounds`] cannot measure it. A
//! template therefore declares the union of its pieces' boxes as the group's
//! [`Mobject::local_bounds`] when it finishes — the same declared-box fallback a
//! text mobject uses, and consulted under the same rule (only when the node has
//! no points). That is what makes `next_to` and `camera focus` work on a
//! template's result: a lecture can say "put this equation under the triangle"
//! and the engine can answer.
//!
//! It is a **snapshot**: a `set position` on a piece moves that piece and leaves
//! the group's declared box where it was. Re-measuring on every read would need
//! a descendant walk that `world_bounds` deliberately does not do; a caller that
//! rearranges the inside of a template should treat the outside position as
//! stale and place the group again.
//!
//! # Refusals are atomic
//!
//! Every parameter is validated before anything is inserted, and the pieces are
//! inserted under the group as they are built. A refusal from the typesetter
//! half-way through rolls the subtree back, so a caller that retries a bad
//! caption does not first have to clean up half an equation.
//!
//! [`SceneTree::world_bounds`]: rustim_mobject::SceneTree::world_bounds

use rustim_math::{Affine2, Rect};
use rustim_mobject::shapes::ShapeParams;
use rustim_mobject::{Mobject, NodeId, NodeKind, SceneTree, TextBlock};

use crate::command::{CommandError, StateDiff};
use crate::typeset::{TextSpec, Typesetter, style_formula, typeset_error};

/// The gap between a template's pieces, in world units.
///
/// Manim's `MED_SMALL_BUFF` is `0.25` and `LARGE_BUFF` is `0.5`; `0.35` is a
/// deliberate middle, because the pieces of a template are closer than two
/// separate ideas and further apart than a label and the thing it names.
pub(crate) const DEFAULT_GAP: f64 = 0.35;

/// The label a compiled formula's root carries inside a template.
///
/// The group gets the caller's label; this names the formula itself, so
/// `--inspect` reads "标题" over a nested "公式" rather than a nameless
/// container. It is the same word `--scene formula` uses.
const FORMULA_LABEL: &str = "公式";

/// A template under construction.
///
/// Owns the group, the pieces created so far and the group's origin, so every
/// template body is a sequence of `text`/`adopt` calls and one `finish`. The
/// `Drop`-free design is deliberate: `rollback` is explicit, and a template
/// function that returns early on a refusal takes it by calling it.
struct Build<'a> {
    tree: &'a mut SceneTree,
    group: NodeId,
    origin: [f64; 2],
    created: Vec<NodeId>,
}

impl<'a> Build<'a> {
    /// Insert the group and return the builder.
    ///
    /// `position` is the world point the group's origin lands on; the pieces a
    /// template places are in the group's own frame, so a template body never
    /// mentions the caller's position again.
    fn open(
        tree: &'a mut SceneTree,
        label: Option<&str>,
        position: Option<[f64; 2]>,
        field: &'static str,
    ) -> Result<Self, CommandError> {
        let origin = position.unwrap_or([0.0, 0.0]);
        if !(origin[0].is_finite() && origin[1].is_finite()) {
            return Err(CommandError::OutOfRange {
                property: field.to_owned(),
                detail: format!("{origin:?} is not a finite world position"),
            });
        }
        let mut m = Mobject::new(Vec::new()).with_kind(NodeKind::Group);
        if let Some(label) = label {
            m.label = Some(label.to_owned());
        }
        m.transform = Affine2::translate((origin[0], origin[1]));
        let group = tree.insert(m);
        Ok(Self {
            tree,
            group,
            origin,
            created: Vec::new(),
        })
    }

    /// Attach a freshly inserted piece to the group and remember it.
    ///
    /// `reparent` can only fail for an unknown id or a cycle, and a piece was
    /// inserted a line ago and has no ancestors — but it is called
    /// **unconditionally**, with the result held in a `let`, because
    /// `debug_assert!(tree.reparent(..))` would skip the call in a release build
    /// (see the `Formula::insert` note in `rustim-text`).
    fn adopt(&mut self, id: NodeId) {
        let attached = self.tree.reparent(id, self.group);
        debug_assert!(attached, "a template's own piece attaches to its group");
        self.created.push(id);
    }

    /// Insert a text piece from a block the caller already typeset and measured.
    fn text(
        &mut self,
        text: &str,
        block: TextBlock,
        spec: &TextSpec,
        local: [f64; 2],
        color: Option<[u8; 3]>,
        label: Option<String>,
    ) -> NodeId {
        let params = ShapeParams {
            text: Some(text.to_owned()),
            size: spec.size,
            max_width: spec.max_width,
            color,
            label,
            // The piece is inserted and then reparented under the group, so
            // its transform is already in the group's frame: the group's own
            // translation applies on the way out and must not be added here.
            position: Some(local),
            ..ShapeParams::default()
        };
        let m = rustim_mobject::shapes::build(rustim_mobject::shapes::ShapeKind::Text, &params)
            .with_text(block);
        let id = self.tree.insert(m);
        self.adopt(id);
        id
    }

    /// Declare the group's box and turn the created list into a diff.
    fn finish(self) -> StateDiff {
        let mut union: Option<Rect> = None;
        for id in &self.created {
            let Some(world) = self.tree.world_bounds(*id) else {
                continue;
            };
            // The pieces live under a group that is a pure translation, so
            // subtracting the origin is the whole world→group conversion.
            let local = Rect::new(
                world.x0 - self.origin[0],
                world.y0 - self.origin[1],
                world.x1 - self.origin[0],
                world.y1 - self.origin[1],
            );
            union = Some(match union {
                Some(acc) => acc.union(local),
                None => local,
            });
        }
        if let Some(bounds) = union
            && let Some(group) = self.tree.get_mut(self.group)
        {
            group.local_bounds = Some(bounds);
            group.dirty.semantic = true;
        }
        let mut diff = StateDiff::default();
        diff.created.push(self.group);
        diff.created.extend(self.created);
        diff
    }

    /// Remove everything this template inserted, leaving the tree as it was.
    fn rollback(self) {
        self.tree.remove_subtree(self.group);
    }
}

/// The slots `explain.equation` fills.
pub(crate) struct Equation<'a> {
    pub formula: &'a str,
    pub position: Option<[f64; 2]>,
    /// A post-typeset uniform scale for the formula.
    pub scale: Option<f64>,
    pub color: Option<[u8; 3]>,
    /// A line of text placed under the formula, measured from its ink.
    pub caption: Option<&'a str>,
    pub caption_size: Option<f64>,
    pub label: Option<&'a str>,
}

/// A formula, centred at `position`, with an optional caption beneath it.
pub(crate) fn equation(
    tree: &mut SceneTree,
    typesetter: &mut dyn Typesetter,
    args: Equation<'_>,
) -> Result<StateDiff, CommandError> {
    let scale = args.scale.unwrap_or(1.0);
    if !scale.is_finite() || scale <= 0.0 {
        return Err(CommandError::OutOfRange {
            property: "scale".to_owned(),
            detail: format!("{scale} is not a finite, positive scale"),
        });
    }
    if let Some(size) = args.caption_size
        && (!size.is_finite() || size <= 0.0)
    {
        return Err(CommandError::OutOfRange {
            property: "caption_size".to_owned(),
            detail: format!("{size} is not a finite, positive size"),
        });
    }
    // A caption sent as blank is a mistake worth naming rather than an empty
    // text node nobody can see.
    let caption = match args.caption {
        Some(text) if text.trim().is_empty() => {
            return Err(CommandError::OutOfRange {
                property: "caption".to_owned(),
                detail: "a caption needs something to say".to_owned(),
            });
        }
        Some(text) => Some(text),
        None => None,
    };
    if caption.is_some() && !typesetter.supports_text() {
        return Err(CommandError::Unsupported {
            feature: "text".to_owned(),
        });
    }

    let mut build = Build::open(tree, args.label, args.position, "position")?;
    let result = (|| -> Result<(), CommandError> {
        let params = ShapeParams {
            text: Some(args.formula.to_owned()),
            scale: Some(scale),
            color: args.color,
            label: Some(FORMULA_LABEL.to_owned()),
            ..ShapeParams::default()
        };
        let formula = typesetter
            .formula(args.formula, build.tree)
            .map_err(|error| typeset_error(error, "formula"))?;
        style_formula(build.tree, &formula, &params);
        build.adopt(formula.root);

        if let Some(caption) = caption {
            let spec = TextSpec {
                size: args.caption_size,
                max_width: None,
            };
            let block = typesetter
                .text(caption, &spec)
                .map_err(|error| typeset_error(error, "caption"))?;
            let caption_half = block.local_bounds().map_or(0.0, |rect| rect.height() / 2.0);
            // The formula's own ink decides where the caption goes. Its world
            // box already carries the group's origin and the post-typeset
            // scale; subtracting the origin lands in the group's frame.
            let formula_bottom = build
                .tree
                .world_bounds(formula.root)
                .map_or(0.0, |rect| rect.y0 - build.origin[1]);
            let y = formula_bottom - DEFAULT_GAP - caption_half;
            build.text(caption, block, &spec, [0.0, y], args.color, None);
        }
        Ok(())
    })();

    match result {
        Ok(()) => Ok(build.finish()),
        Err(error) => {
            build.rollback();
            Err(error)
        }
    }
}
