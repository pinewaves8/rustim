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

use std::f64::consts::FRAC_PI_2;

use rustim_math::{Affine2, Rect};
use rustim_mobject::shapes::{AxesScale, DEFAULT_THICKNESS, ShapeKind, ShapeParams};
use rustim_mobject::{Mobject, NodeId, NodeKind, SceneTree, TextBlock};

use crate::command::{Cell, CommandError, MatrixBrackets, StateDiff};
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

    /// Insert a geometry piece through the ordinary shape builder.
    ///
    /// A template validates every parameter before it opens the group, so an
    /// empty point array here would be a builder defect rather than caller
    /// input; the piece is adopted either way and the group's declared box is
    /// what would show the hole.
    fn piece(&mut self, kind: ShapeKind, params: ShapeParams) -> NodeId {
        let id = self
            .tree
            .insert(rustim_mobject::shapes::build(kind, &params));
        self.adopt(id);
        id
    }

    /// Insert a mobject the template built itself and attach it to the group.
    ///
    /// For a piece that needs a transform the shape builder cannot spell — a
    /// matrix's vertical determinant bar is a horizontal `Line` turned a quarter
    /// turn — the builder is called with the geometry and the transform is
    /// composed on the way in.
    fn node(&mut self, m: Mobject) -> NodeId {
        let id = self.tree.insert(m);
        self.adopt(id);
        id
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

// ---- explain.plot -------------------------------------------------------

/// The default data window a plot spans.
///
/// Symmetric about zero, like the compiled `axes` scene's ranges, so the
/// geometric centre of the axes is also the data origin and a curve drawn on
/// them starts life where a viewer expects. A caller that wants an asymmetric
/// window states it.
const PLOT_X_RANGE: [f64; 2] = [-4.0, 4.0];
const PLOT_Y_RANGE: [f64; 2] = [-3.0, 3.0];

/// The geometric size of the axes, in world units — the same proportions as
/// `--scene axes`, which fills a 16:9 frame without touching its corners.
const PLOT_WIDTH: f64 = 7.0;
const PLOT_HEIGHT: f64 = 4.0;

/// Samples per plot, and the ceiling on them.
///
/// A template is a *thick* tool, so it chooses a resolution rather than asking
/// for one: 160 samples across a 7-unit span is a little over 20 per unit,
/// which no curve in this vocabulary can visibly exploit with more. The cap
/// keeps one command from allocating an arbitrarily long polyline.
const PLOT_SAMPLES: u32 = 160;
const MAX_SAMPLES: u32 = 2048;

/// The highest polynomial degree a plot evaluates.
///
/// The wire has no expression language (a decision, not a gap), so a caller
/// hands over coefficients and the template evaluates them by Horner's rule.
/// Sixteen is more than any teaching example needs and small enough that a
/// command cannot turn into a long evaluation; a higher degree is refused
/// rather than silently truncated.
const MAX_DEGREE: usize = 16;

/// The most labels one axis may carry.
///
/// The axes builder caps its *ticks* at `MAX_TICKS`; a label is a typeset
/// object, so the template refuses a step that would need more than this
/// rather than allocating hundreds of text nodes.
const MAX_LABELS: f64 = 64.0;

/// How far a tick stands off its axis, and the em size of the labels — both
/// measured from the compiled `axes` scene, which tuned them against the frame.
const AXES_TICK_SIZE: f64 = 0.15;
const TICK_LABEL_SIZE: f64 = 0.22;
const X_LABEL_DY: f64 = -0.38;
const Y_LABEL_DX: f64 = -0.42;

/// The slots `explain.plot` fills.
pub(crate) struct Plot<'a> {
    /// `c0 + c1·x + c2·x² + …`, lowest degree first.
    pub coefficients: &'a [f64],
    pub x_range: Option<[f64; 2]>,
    pub y_range: Option<[f64; 2]>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub samples: Option<u32>,
    /// Data units between ticks and labels.
    pub step: Option<f64>,
    /// The curve's colour; the axes stay foreground.
    pub color: Option<[u8; 3]>,
    pub caption: Option<&'a str>,
    pub caption_size: Option<f64>,
    pub position: Option<[f64; 2]>,
    pub label: Option<&'a str>,
}

/// A coordinate system, a polynomial curve and its labelled ticks.
///
/// The template exists because a realtime model asked to "show y = x² − 2"
/// should not have to work out that axes go with a curve, where the tick labels
/// sit, or how many samples the curve needs. It fills the slots and the engine
/// makes those decisions.
///
/// The curve is never an expression: `coefficients` is evaluated by Horner's
/// rule in the template itself, bounded by [`MAX_DEGREE`]. Sampling, the
/// world→data mapping and the tick labels all go through the same
/// [`AxesScale`], so a curve and the labels underneath it cannot drift apart.
///
/// Pieces, in `created` order: the axes, one plot mobject per contiguous run of
/// samples inside `y_range`, the tick labels, then the caption if there is one.
/// A run becomes its own `Plot` rather than one mobject with a gap because a
/// flat point array cannot express a break — a single polyline would join the
/// pieces of a curve that leaves the top of the frame and comes back, drawing a
/// line the function never had. Runs are cut at the boundary: the last sample
/// inside is joined to the interpolated crossing, so the curve reaches the edge
/// of its range instead of stopping one sample short.
pub(crate) fn plot(
    tree: &mut SceneTree,
    typesetter: &mut dyn Typesetter,
    args: Plot<'_>,
) -> Result<StateDiff, CommandError> {
    if args.coefficients.is_empty() {
        return Err(CommandError::OutOfRange {
            property: "coefficients".to_owned(),
            detail: "a plot needs at least one coefficient".to_owned(),
        });
    }
    if args.coefficients.len() > MAX_DEGREE + 1 {
        return Err(CommandError::OutOfRange {
            property: "coefficients".to_owned(),
            detail: format!(
                "{} coefficients exceed the degree-{MAX_DEGREE} cap",
                args.coefficients.len()
            ),
        });
    }
    if let Some(value) = args.coefficients.iter().copied().find(|v| !v.is_finite()) {
        return Err(CommandError::OutOfRange {
            property: "coefficients".to_owned(),
            detail: format!("{value} is not a finite coefficient"),
        });
    }
    let x_range = finite_range(args.x_range, PLOT_X_RANGE, "x_range")?;
    let y_range = finite_range(args.y_range, PLOT_Y_RANGE, "y_range")?;
    let width = positive(args.width, PLOT_WIDTH, "width")?;
    let height = positive(args.height, PLOT_HEIGHT, "height")?;
    let step = positive(args.step, 1.0, "step")?;
    let samples = args.samples.unwrap_or(PLOT_SAMPLES);
    if !(2..=MAX_SAMPLES).contains(&samples) {
        return Err(CommandError::OutOfRange {
            property: "samples".to_owned(),
            detail: format!("{samples} is outside 2..={MAX_SAMPLES}"),
        });
    }
    let x_ticks = axis_labels(x_range, step).ok_or_else(|| CommandError::OutOfRange {
        property: "step".to_owned(),
        detail: format!(
            "x_range [{}, {}] needs more than {MAX_LABELS} labels at step {step}; use a larger step",
            x_range[0], x_range[1]
        ),
    })?;
    let y_ticks = axis_labels(y_range, step).ok_or_else(|| CommandError::OutOfRange {
        property: "step".to_owned(),
        detail: format!(
            "y_range [{}, {}] needs more than {MAX_LABELS} labels at step {step}; use a larger step",
            y_range[0], y_range[1]
        ),
    })?;
    if let Some(size) = args.caption_size
        && (!size.is_finite() || size <= 0.0)
    {
        return Err(CommandError::OutOfRange {
            property: "caption_size".to_owned(),
            detail: format!("{size} is not a finite, positive size"),
        });
    }
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
    // Tick labels are not optional — an unlabelled graph is a picture, not an
    // explanation — so the whole template needs a typesetter, caption or not.
    if !typesetter.supports_text() {
        return Err(CommandError::Unsupported {
            feature: "text".to_owned(),
        });
    }

    let mut build = Build::open(tree, args.label, args.position, "position")?;
    let result = (|| -> Result<(), CommandError> {
        build.piece(
            ShapeKind::Axes,
            ShapeParams {
                width: Some(width),
                height: Some(height),
                x_range: Some(x_range),
                y_range: Some(y_range),
                step: Some(step),
                tick_size: Some(AXES_TICK_SIZE),
                ..ShapeParams::default()
            },
        );

        let scale = AxesScale::new(
            (x_range[0], x_range[1]),
            width,
            (y_range[0], y_range[1]),
            height,
        );
        let runs = sample_runs(args.coefficients, x_range, y_range, samples);
        if runs.is_empty() {
            // The pieces are already in the tree, so this refusal is the
            // rollback path's job: a plot with axes and no curve is not the
            // explanation anyone asked for, and it is usually a y_range that
            // does not contain the curve.
            return Err(CommandError::OutOfRange {
                property: "y_range".to_owned(),
                detail: format!(
                    "the curve never enters [{}, {}] over x in [{}, {}]; widen y_range",
                    y_range[0], y_range[1], x_range[0], x_range[1]
                ),
            });
        }
        for run in runs {
            let points: Vec<[f64; 2]> = run
                .into_iter()
                .map(|[x, y]| {
                    let p = scale.to_world(x, y);
                    [p.x, p.y]
                })
                .collect();
            build.piece(
                ShapeKind::Plot,
                ShapeParams {
                    points: Some(points),
                    color: args.color,
                    ..ShapeParams::default()
                },
            );
        }

        // Labels are typeset under the group, in the group's frame: the axes
        // are centred on the geometric origin, so a tick's world position from
        // `AxesScale` is already the local position too.
        let label_spec = TextSpec {
            size: Some(TICK_LABEL_SIZE),
            max_width: None,
        };
        for &value in &x_ticks {
            if value == 0.0 {
                continue;
            }
            let text = tick_text(value);
            let block = typesetter
                .text(&text, &label_spec)
                .map_err(|error| typeset_error(error, "x_range"))?;
            let x = scale.to_world(value, 0.0).x;
            // Foreground, like the axes: `color` names the curve, and a label
            // that took it would read as part of the graph rather than as a
            // marking of the coordinate system.
            build.text(
                &text,
                block,
                &label_spec,
                [x, X_LABEL_DY],
                None,
                Some(text.clone()),
            );
        }
        for &value in &y_ticks {
            if value == 0.0 {
                continue;
            }
            let text = tick_text(value);
            let block = typesetter
                .text(&text, &label_spec)
                .map_err(|error| typeset_error(error, "y_range"))?;
            let y = scale.to_world(0.0, value).y;
            build.text(
                &text,
                block,
                &label_spec,
                [Y_LABEL_DX, y],
                None,
                Some(text.clone()),
            );
        }

        if let Some(caption) = caption {
            let spec = TextSpec {
                size: args.caption_size,
                max_width: None,
            };
            let block = typesetter
                .text(caption, &spec)
                .map_err(|error| typeset_error(error, "caption"))?;
            let caption_half = block.local_bounds().map_or(0.0, |rect| rect.height() / 2.0);
            // The lowest piece decides where the caption goes — usually the
            // axes' own bottom edge, or an x label when the axes are short.
            let bottom = build
                .created
                .iter()
                .filter_map(|id| build.tree.world_bounds(*id))
                .map(|rect| rect.y0 - build.origin[1])
                .fold(f64::INFINITY, f64::min);
            let y = if bottom.is_finite() {
                bottom - DEFAULT_GAP - caption_half
            } else {
                -height / 2.0 - DEFAULT_GAP - caption_half
            };
            build.text(caption, block, &spec, [0.0, y], None, None);
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

/// Validate a caller-supplied `[lo, hi]`, defaulting to the template's own.
fn finite_range(
    value: Option<[f64; 2]>,
    default: [f64; 2],
    field: &'static str,
) -> Result<[f64; 2], CommandError> {
    let [lo, hi] = value.unwrap_or(default);
    if !(lo.is_finite() && hi.is_finite()) || lo >= hi {
        return Err(CommandError::OutOfRange {
            property: field.to_owned(),
            detail: format!("[{lo}, {hi}] is not a finite, increasing range"),
        });
    }
    Ok([lo, hi])
}

/// Validate a positive, finite scalar.
fn positive(value: Option<f64>, default: f64, field: &'static str) -> Result<f64, CommandError> {
    let value = value.unwrap_or(default);
    if !value.is_finite() || value <= 0.0 {
        return Err(CommandError::OutOfRange {
            property: field.to_owned(),
            detail: format!("{value} is not a finite, positive value"),
        });
    }
    Ok(value)
}

/// The tick values a range and step produce, or `None` when there are too many
/// to label.
///
/// The same `ceil`/`floor` arithmetic the axes builder uses for its ticks, so
/// a label always sits under a tick that was actually drawn. `None` is a
/// refusal rather than an empty list: silently dropping labels would leave a
/// graph that looks complete and explains less.
fn axis_labels(range: [f64; 2], step: f64) -> Option<Vec<f64>> {
    let first = (range[0] / step).ceil();
    let last = (range[1] / step).floor();
    if !(first.is_finite() && last.is_finite()) || last < first {
        return Some(Vec::new());
    }
    let count = last - first + 1.0;
    if count > MAX_LABELS {
        return None;
    }
    Some(
        (0..count as usize)
            .map(|i| (first + i as f64) * step)
            .collect(),
    )
}

/// A label's text: integral values without a decimal point, others to at most
/// three places with trailing zeros trimmed.
///
/// `2.0` reads as `2`, `0.5` as `0.5`, and a step of `0.1` does not print
/// `0.30000000000000004` — the multiplication that produced it is exact only
/// to within rounding.
fn tick_text(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        let text = format!("{value:.3}");
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

/// Evaluate `c0 + c1·x + c2·x² + …` by Horner's rule.
///
/// The loop is bounded by [`MAX_DEGREE`] before it ever runs, so a long
/// coefficient list cannot turn one command into a long computation.
fn evaluate(coefficients: &[f64], x: f64) -> f64 {
    coefficients.iter().rev().fold(0.0, |acc, c| acc * x + c)
}

/// Sample the curve and cut it into the maximal runs that lie inside
/// `y_range`, each ending on an interpolated crossing at the boundary.
///
/// Runs rather than one array because a flat point array cannot express a
/// subpath break: a curve that leaves the top of the frame and returns would
/// otherwise be joined by a straight line across the gap.
fn sample_runs(
    coefficients: &[f64],
    x_range: [f64; 2],
    y_range: [f64; 2],
    samples: u32,
) -> Vec<Vec<[f64; 2]>> {
    let [x0, x1] = x_range;
    let [y0, y1] = y_range;
    let n = f64::from(samples);
    let mut runs: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut run: Vec<[f64; 2]> = Vec::new();
    let mut previous: Option<[f64; 2]> = None;
    for i in 0..=samples {
        let x = x0 + (x1 - x0) * f64::from(i) / n;
        let y = evaluate(coefficients, x);
        let inside = y.is_finite() && y >= y0 && y <= y1;
        if inside {
            // The first sample after a gap enters the window: the run starts at
            // the interpolated crossing so it reaches the edge.
            if run.is_empty()
                && let Some(point) = previous.and_then(|p| boundary(p, [x, y], y0, y1))
            {
                run.push(point);
            }
            run.push([x, y]);
        } else if !run.is_empty() {
            if let Some(point) = previous.and_then(|p| boundary(p, [x, y], y0, y1)) {
                run.push(point);
            }
            if run.len() >= 2 {
                runs.push(std::mem::take(&mut run));
            } else {
                run.clear();
            }
        }
        previous = Some([x, y]);
    }
    if run.len() >= 2 {
        runs.push(run);
    }
    runs
}

/// Where the segment `from`→`to` crosses the nearer edge of `[y0, y1]`.
///
/// Linear in x, which is exact for the crossing of a sampled segment and
/// within one sample of the true curve — the alternative is leaving the curve
/// short of the frame edge by up to one sample's width, which reads as a gap.
/// A non-finite endpoint (an overflowed evaluation) has no crossing to offer,
/// and the run simply starts or ends at the sample inside the range.
fn boundary(from: [f64; 2], to: [f64; 2], y0: f64, y1: f64) -> Option<[f64; 2]> {
    if !(from[1].is_finite() && to[1].is_finite()) || from[1] == to[1] {
        return None;
    }
    let crosses = |edge: f64| {
        let a = from[1] - edge;
        let b = to[1] - edge;
        a == 0.0 || b == 0.0 || (a < 0.0) != (b < 0.0)
    };
    let edge = if crosses(y0) {
        y0
    } else if crosses(y1) {
        y1
    } else {
        return None;
    };
    let t = (edge - from[1]) / (to[1] - from[1]);
    Some([from[0] + (to[0] - from[0]) * t, edge])
}

// ---- explain.geometry ---------------------------------------------------

/// The label the figure itself carries inside `explain.geometry`.
const FIGURE_LABEL: &str = "图形";

/// The label a right-angle marker carries.
///
/// The markers are scenery rather than subjects: they carry a label so
/// `--inspect` names them, and `NodeKind::Unknown` so a marker is not described
/// as a small four-sided figure with an area, which it is not.
const RIGHT_ANGLE_LABEL: &str = "直角标记";

/// The default fill of a figure.
///
/// A wash rather than a solid: the template exists to show the geometry, so
/// the stroke and the labels have to read on top of the fill. `1.0` is legal —
/// a caller asking for a silhouette gets one — but a filled figure with labels
/// over it is the caller's decision, not the default.
const GEOMETRY_FILL_OPACITY: f64 = 0.3;

/// The most vertices one figure may have.
///
/// The same posture as the axes' `MAX_TICKS`: a command from a model must not
/// be able to allocate an unbounded number of label nodes. Sixty-four is far
/// past any teaching figure and cheap to walk.
const MAX_VERTICES: usize = 64;

/// The gap between an edge or a vertex and the ink of the label naming it.
const LABEL_GAP: f64 = 0.12;

/// How close to a right angle a corner must be to earn a marker, as `|cos|`.
///
/// One dot product rather than an angle: `1e-3` is a tenth of a degree, far
/// tighter than a figure typed by hand will miss by, and far looser than the
/// rounding of a computation that meant exactly 90°.
const RIGHT_ANGLE_TOLERANCE: f64 = 1e-3;

/// A right-angle marker's leg: a fraction of the shorter adjoining edge,
/// clamped so it neither vanishes on a small figure nor swamps a large one.
const MARKER_FRACTION: f64 = 0.18;
const MARKER_MIN: f64 = 0.08;
const MARKER_MAX: f64 = 0.4;

/// The tolerance below which three vertices are taken to be collinear.
///
/// A figure whose vertices lie on one line has no area and draws nothing, so
/// it is refused rather than accepted as an invisible group. The test is the
/// cross product of every vertex against the first distinct edge — exact for
/// the teaching coordinates this template serves, and winding-agnostic, which
/// a signed-area test would not be (a bow tie has zero signed area and is a
/// perfectly good picture).
const COLLINEAR_EPSILON: f64 = 1e-9;

/// The slots `explain.geometry` fills.
pub(crate) struct Geometry<'a> {
    /// The figure's outline, in world coordinates. Closed by the template.
    pub vertices: &'a [[f64; 2]],
    /// One label per edge, in vertex order: label `i` names the edge from
    /// vertex `i` to vertex `i + 1`.
    pub side_labels: Option<&'a [String]>,
    /// One label per vertex, in the same order as `vertices`.
    pub vertex_labels: Option<&'a [String]>,
    /// Mark the corners that are right angles (default `true`).
    pub right_angle: Option<bool>,
    /// The figure's fill opacity (default [`GEOMETRY_FILL_OPACITY`]).
    pub fill_opacity: Option<f64>,
    /// The figure's colour; the labels stay foreground.
    pub color: Option<[u8; 3]>,
    /// The side and vertex labels' em size in world units.
    pub label_size: Option<f64>,
    pub caption: Option<&'a str>,
    pub caption_size: Option<f64>,
    pub position: Option<[f64; 2]>,
    pub label: Option<&'a str>,
}

/// A closed figure, its side and vertex labels, right-angle markers and a
/// caption.
///
/// The template exists because a model asked to "show a triangle with legs 3
/// and 4" should not have to work out where a side label sits outside the
/// edge, that a right angle deserves a marker, or where a caption goes. It
/// fills the slots and the engine makes those decisions.
///
/// The vertices are the figure: the caller supplies them in world coordinates
/// and the template **centres them on their bounding box**, so `position`
/// names the figure's centre rather than an arbitrary corner — the same
/// convention every sized shape follows, where the origin is the centre.
///
/// Pieces, in `created` order: the figure, one stroked square per right-angle
/// corner, the side labels, the vertex labels, then the caption if there is
/// one. The figure is a [`ShapeKind::Polygon`] with a stroked outline, so its
/// edge is visible against its own wash; the markers are the same builder's
/// polygon with the fill switched off — a filled marker would be a black
/// right-angle block rather than the conventional outline.
///
/// Only the optional labels and the caption typeset, so a build with no
/// typesetter can draw a bare figure and refuses a labelled one under `text`.
pub(crate) fn geometry(
    tree: &mut SceneTree,
    mut typesetter: Option<&mut dyn Typesetter>,
    args: Geometry<'_>,
) -> Result<StateDiff, CommandError> {
    let vertices = args.vertices;
    if vertices.len() < 3 {
        return Err(CommandError::OutOfRange {
            property: "vertices".to_owned(),
            detail: format!(
                "{} vertices cannot close a figure; give at least three",
                vertices.len()
            ),
        });
    }
    if vertices.len() > MAX_VERTICES {
        return Err(CommandError::OutOfRange {
            property: "vertices".to_owned(),
            detail: format!(
                "{} vertices exceed the {MAX_VERTICES}-vertex cap",
                vertices.len()
            ),
        });
    }
    if let Some([x, y]) = vertices
        .iter()
        .copied()
        .find(|[x, y]| !x.is_finite() || !y.is_finite())
    {
        return Err(CommandError::OutOfRange {
            property: "vertices".to_owned(),
            detail: format!("[{x}, {y}] is not a finite vertex"),
        });
    }
    if collinear(vertices) {
        return Err(CommandError::OutOfRange {
            property: "vertices".to_owned(),
            detail: "the vertices lie on one line, so the figure has no area".to_owned(),
        });
    }
    for (labels, field) in [
        (args.side_labels, "side_labels"),
        (args.vertex_labels, "vertex_labels"),
    ] {
        let Some(labels) = labels else { continue };
        if labels.len() != vertices.len() {
            return Err(CommandError::OutOfRange {
                property: field.to_owned(),
                detail: format!(
                    "{} labels for a figure with {} {}",
                    labels.len(),
                    vertices.len(),
                    match field {
                        "side_labels" => "edges",
                        _ => "vertices",
                    }
                ),
            });
        }
        if let Some(index) = labels.iter().position(|label| label.trim().is_empty()) {
            return Err(CommandError::OutOfRange {
                property: field.to_owned(),
                detail: format!("label {index} is blank"),
            });
        }
    }
    let fill = args.fill_opacity.unwrap_or(GEOMETRY_FILL_OPACITY);
    if !fill.is_finite() || !(0.0..=1.0).contains(&fill) {
        return Err(CommandError::OutOfRange {
            property: "fill_opacity".to_owned(),
            detail: format!("{fill} is not an opacity in 0.0..=1.0"),
        });
    }
    if let Some(size) = args.label_size
        && (!size.is_finite() || size <= 0.0)
    {
        return Err(CommandError::OutOfRange {
            property: "label_size".to_owned(),
            detail: format!("{size} is not a finite, positive size"),
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
    // Unlike a plot, whose tick labels are mandatory, a figure is a legitimate
    // picture on its own; text is required only by the optional labels.
    let needs_text =
        args.side_labels.is_some() || args.vertex_labels.is_some() || caption.is_some();
    if needs_text
        && !typesetter
            .as_ref()
            .is_some_and(|typesetter| typesetter.supports_text())
    {
        return Err(CommandError::Unsupported {
            feature: "text".to_owned(),
        });
    }
    let local = centred(vertices);

    let mut build = Build::open(tree, args.label, args.position, "position")?;
    let result = (|| -> Result<(), CommandError> {
        let figure = build.piece(
            ShapeKind::Polygon,
            ShapeParams {
                points: Some(local.clone()),
                color: args.color,
                label: Some(FIGURE_LABEL.to_owned()),
                ..ShapeParams::default()
            },
        );
        if let Some(m) = build.tree.get_mut(figure) {
            // The builder paints a polygon as a filled contour; a figure is a
            // filled contour **with its edge drawn**, so the same paint serves
            // both passes and only the opacities differ.
            m.style.fill_opacity = fill;
            m.style.stroke_opacity = 1.0;
            m.style.stroke = m.style.fill;
            m.style.stroke_width = DEFAULT_THICKNESS;
            m.dirty.style = true;
        }

        if args.right_angle.unwrap_or(true) {
            for index in right_angle_corners(&local) {
                let Some(quad) = marker_quad(&local, index) else {
                    continue;
                };
                let marker = build.piece(
                    ShapeKind::Polygon,
                    ShapeParams {
                        points: Some(quad),
                        color: args.color,
                        ..ShapeParams::default()
                    },
                );
                if let Some(m) = build.tree.get_mut(marker) {
                    m.style.fill_opacity = 0.0;
                    m.style.stroke_opacity = 1.0;
                    m.style.stroke = m.style.fill;
                    m.style.stroke_width = DEFAULT_THICKNESS;
                    m.dirty.style = true;
                    m.kind = NodeKind::Unknown;
                    m.dirty.semantic = true;
                    m.label = Some(RIGHT_ANGLE_LABEL.to_owned());
                }
            }
        }

        // `needs_text` already established that a typesetter is present when
        // any of this section has work to do, so reaching here without one
        // means every list is empty and there is nothing to place.
        if let Some(typesetter) = typesetter.as_deref_mut() {
            let label_spec = TextSpec {
                size: args.label_size,
                max_width: None,
            };
            // Labels are foreground, like a plot's tick labels: `color` names
            // the figure, and a label that took it would read as part of the
            // figure.
            if let Some(labels) = args.side_labels {
                for (index, text) in labels.iter().enumerate() {
                    let (mid, normal) = edge_frame(&local, index);
                    let block = typesetter
                        .text(text, &label_spec)
                        .map_err(|error| typeset_error(error, "side_labels"))?;
                    let position = pushed(mid, normal, block.local_bounds());
                    build.text(text, block, &label_spec, position, None, Some(text.clone()));
                }
            }
            if let Some(labels) = args.vertex_labels {
                for (index, text) in labels.iter().enumerate() {
                    let direction = outward(local[index]);
                    let block = typesetter
                        .text(text, &label_spec)
                        .map_err(|error| typeset_error(error, "vertex_labels"))?;
                    let position = pushed(local[index], direction, block.local_bounds());
                    build.text(text, block, &label_spec, position, None, Some(text.clone()));
                }
            }

            if let Some(caption) = caption {
                let spec = TextSpec {
                    size: args.caption_size,
                    max_width: None,
                };
                let block = typesetter
                    .text(caption, &spec)
                    .map_err(|error| typeset_error(error, "caption"))?;
                let caption_half = block.local_bounds().map_or(0.0, |rect| rect.height() / 2.0);
                // The lowest piece decides where the caption goes — usually
                // the figure's own bottom edge, or a vertex label hanging
                // below it.
                let bottom = build
                    .created
                    .iter()
                    .filter_map(|id| build.tree.world_bounds(*id))
                    .map(|rect| rect.y0 - build.origin[1])
                    .fold(f64::INFINITY, f64::min);
                let y = if bottom.is_finite() {
                    bottom - DEFAULT_GAP - caption_half
                } else {
                    -caption_half
                };
                build.text(caption, block, &spec, [0.0, y], None, None);
            }
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

/// Translate the vertices so their bounding box is centred on the origin.
///
/// Every sized shape in the engine is centred, and the template's `position`
/// names the group's origin; without this a caller's `position` would name an
/// arbitrary offset inside the figure and `camera focus` would frame it off
/// centre.
fn centred(vertices: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut x0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y0 = f64::INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    for [x, y] in vertices {
        x0 = x0.min(*x);
        x1 = x1.max(*x);
        y0 = y0.min(*y);
        y1 = y1.max(*y);
    }
    let cx = (x0 + x1) / 2.0;
    let cy = (y0 + y1) / 2.0;
    vertices.iter().map(|[x, y]| [x - cx, y - cy]).collect()
}

/// Whether every vertex lies on one line — a figure with no area.
fn collinear(vertices: &[[f64; 2]]) -> bool {
    let Some(&[ax, ay]) = vertices.first() else {
        return true;
    };
    let Some(&[bx, by]) = vertices.iter().find(|[x, y]| *x != ax || *y != ay) else {
        return true;
    };
    let (dx, dy) = (bx - ax, by - ay);
    vertices
        .iter()
        .all(|[x, y]| ((x - ax) * dy - (y - ay) * dx).abs() <= COLLINEAR_EPSILON)
}

/// The signed area of the vertex loop, by the shoelace formula.
///
/// Only its **sign** is read, to decide which side of an edge is outward: the
/// wire has no winding field, so the vertices' own order is the only statement
/// about orientation there is.
fn signed_area(vertices: &[[f64; 2]]) -> f64 {
    0.5 * vertices
        .iter()
        .zip(vertices.iter().cycle().skip(1))
        .map(|(a, b)| a[0] * b[1] - b[0] * a[1])
        .sum::<f64>()
}

/// The unit vector from `from` to `to`, or `None` when they coincide.
fn unit(from: [f64; 2], to: [f64; 2]) -> Option<[f64; 2]> {
    let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
    let len = dx.hypot(dy);
    (len > 0.0).then(|| [dx / len, dy / len])
}

/// The midpoint of edge `index` and its **outward** unit normal.
///
/// The normal is the edge direction turned by 90°, with the sign chosen by the
/// loop's signed area, so a label sits outside the figure whichever way the
/// caller wound the vertices. A zero-length edge — a repeated vertex, which the
/// polygon builder tolerates — has no normal to offer and gets `[0, 1]`, which
/// still places the label instead of dropping it.
fn edge_frame(vertices: &[[f64; 2]], index: usize) -> ([f64; 2], [f64; 2]) {
    let n = vertices.len();
    let a = vertices[index];
    let b = vertices[(index + 1) % n];
    let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
    let Some(direction) = unit(a, b) else {
        return (mid, [0.0, 1.0]);
    };
    let sign = if signed_area(vertices) < 0.0 {
        -1.0
    } else {
        1.0
    };
    (mid, [sign * direction[1], -sign * direction[0]])
}

/// The outward direction for a vertex: away from the figure's centre.
///
/// The centre is the origin because [`centred`] put the bounding box there; a
/// vertex *at* the origin (a figure whose bounding-box centre is one of its
/// own corners) has no direction to offer and is pushed up, which is as good
/// as any other direction and never overlaps the figure's interior more than
/// the alternatives.
fn outward(vertex: [f64; 2]) -> [f64; 2] {
    let len = vertex[0].hypot(vertex[1]);
    if len == 0.0 {
        [0.0, 1.0]
    } else {
        [vertex[0] / len, vertex[1] / len]
    }
}

/// Where a label's **centre** goes: the anchor, pushed along `direction` clear
/// of the anchor plus the label's own half-extent along that direction.
///
/// The extent is the support function of an axis-aligned box,
/// `|dx|·w/2 + |dy|·h/2`, so a wide label below an edge is pushed down by its
/// half-height only and does not float a text width away; a diagonal placement
/// clears the box's corner. Without measuring the block the label would land
/// with its centre on the edge and half of it inside the figure.
fn pushed(anchor: [f64; 2], direction: [f64; 2], bounds: Option<Rect>) -> [f64; 2] {
    let (width, height) = bounds.map_or((0.0, 0.0), |rect| (rect.width(), rect.height()));
    let reach = direction[0].abs() * width / 2.0 + direction[1].abs() * height / 2.0 + LABEL_GAP;
    [
        anchor[0] + direction[0] * reach,
        anchor[1] + direction[1] * reach,
    ]
}

/// The corners of `vertices` whose interior angle is a right angle.
fn right_angle_corners(vertices: &[[f64; 2]]) -> Vec<usize> {
    let n = vertices.len();
    (0..n)
        .filter(|&index| {
            let corner = vertices[index];
            let (Some(a), Some(b)) = (
                unit(corner, vertices[(index + n - 1) % n]),
                unit(corner, vertices[(index + 1) % n]),
            ) else {
                return false;
            };
            (a[0] * b[0] + a[1] * b[1]).abs() <= RIGHT_ANGLE_TOLERANCE
        })
        .collect()
}

/// The little square that marks a right angle at `vertices[index]`.
///
/// Two legs along the adjoining edges, closed back to the corner: an
/// axis-aligned square only for an axis-aligned right angle, and a rectangle
/// in the figure's own frame otherwise — which is the conventional mark.
/// Zero-length edges have no leg to draw and yield `None`.
fn marker_quad(vertices: &[[f64; 2]], index: usize) -> Option<Vec<[f64; 2]>> {
    let n = vertices.len();
    let corner = vertices[index];
    let prev = vertices[(index + n - 1) % n];
    let next = vertices[(index + 1) % n];
    let a = unit(corner, prev)?;
    let b = unit(corner, next)?;
    let shorter = (prev[0] - corner[0])
        .hypot(prev[1] - corner[1])
        .min((next[0] - corner[0]).hypot(next[1] - corner[1]));
    let lower = MARKER_MIN.min(shorter / 2.0).min(MARKER_MAX);
    let size = (MARKER_FRACTION * shorter).clamp(lower, MARKER_MAX);
    let along = |direction: [f64; 2]| {
        [
            corner[0] + direction[0] * size,
            corner[1] + direction[1] * size,
        ]
    };
    let (p, q) = (along(a), along(b));
    Some(vec![corner, p, [p[0] + b[0] * size, p[1] + b[1] * size], q])
}

// ---- explain.matrix -----------------------------------------------------

/// The most rows and columns one matrix may have.
///
/// A matrix is a *teaching* object, and the cap is the same posture as the
/// figure's: a command from a model must not be able to allocate hundreds of
/// text nodes. Sixteen square is already twelve times the largest matrix a
/// lecture shows, and it is cheap to validate.
const MAX_MATRIX_ROWS: usize = 16;
const MAX_MATRIX_COLS: usize = 16;

/// The gap between two columns and between two rows of cells.
///
/// Column gap wider than the row gap because entries are read across a row: a
/// little air between columns is what stops `1 2` from reading as `12`, while
/// rows are separated by their own line height already.
const MATRIX_COLUMN_GAP: f64 = 0.4;
const MATRIX_ROW_GAP: f64 = 0.25;

/// The gap between the entries and a bracket.
const BRACKET_GAP: f64 = 0.1;

/// A square bracket's arm length: a fraction of the bracket's height, clamped
/// so the arm neither vanishes on a one-row matrix nor becomes a picture of its
/// own on a tall one. It is also clamped above the shaft thickness, because an
/// arm shorter than the shaft is not a bracket.
const MATRIX_SERIF_FRACTION: f64 = 0.15;
const MATRIX_SERIF_MIN: f64 = 0.08;
const MATRIX_SERIF_MAX: f64 = 0.3;

/// A round bracket's half-width: a fraction of the bracket's height (an
/// ellipse, not a semicircle), clamped for the same two reasons.
const MATRIX_ROUND_FRACTION: f64 = 0.12;
const MATRIX_ROUND_MIN: f64 = 0.05;
const MATRIX_ROUND_MAX: f64 = 0.2;

/// Samples per round bracket. A quarter of the whole ellipse, which at any
/// size a 720p frame shows is finer than a pixel.
const ROUND_BRACKET_SAMPLES: usize = 24;

/// The names the two brackets carry.
///
/// Scenery rather than subjects, like the right-angle marker: `--inspect` names
/// them, and a screen reader says which side a reader is on without being told
/// a size it cannot use.
const LEFT_BRACKET_LABEL: &str = "左括号";
const RIGHT_BRACKET_LABEL: &str = "右括号";

/// The slots `explain.matrix` fills.
pub(crate) struct Matrix<'a> {
    /// The cell table, row by row. Every row has the same number of cells.
    pub rows: &'a [Vec<Cell>],
    pub column_labels: Option<&'a [String]>,
    pub row_labels: Option<&'a [String]>,
    pub brackets: MatrixBrackets,
    /// Text placed left of the whole matrix (`"A ="`).
    pub lhs: Option<&'a str>,
    /// The em size of the cells, the labels and the `lhs`.
    pub cell_size: Option<f64>,
    /// The cells' colour; the labels, the `lhs` and the caption stay foreground.
    pub color: Option<[u8; 3]>,
    pub position: Option<[f64; 2]>,
    pub caption: Option<&'a str>,
    pub caption_size: Option<f64>,
    pub label: Option<&'a str>,
}

/// A grid of cells with brackets, row and column labels and a caption.
///
/// The template exists because a model asked to "show the matrix A" should not
/// have to work out the column widths, where the brackets go, or that a label
/// belongs outside them. It fills the slots and the engine makes those
/// decisions.
///
/// The cells are typeset first, and **the grid is measured from them**: column
/// `c` is as wide as its widest cell and row `r` as tall as its tallest, so a
/// caller never pads a number to line a column up. The **bracketed matrix is
/// centred on `position`** — the row labels, the `lhs` and the column labels
/// hang around it — which keeps `position` meaning "the centre of the thing"
/// as it does in every other template. A caption goes under the lowest piece,
/// measured from the ink rather than from the grid.
///
/// Pieces, in `created` order: the left and right brackets (none for
/// [`MatrixBrackets::None`]), the cells row-major, the column labels, the row
/// labels, the `lhs`, then the caption. Every cell is labelled by the text it
/// renders, so a cell can be addressed by value — and a matrix with a repeated
/// value answers `ambiguous_label`, because a label is not a key.
///
/// Text is mandatory, so a build with no typesetter refuses the command under
/// `text` rather than drawing an empty grid.
pub(crate) fn matrix(
    tree: &mut SceneTree,
    typesetter: &mut dyn Typesetter,
    args: Matrix<'_>,
) -> Result<StateDiff, CommandError> {
    let rows = args.rows;
    if rows.is_empty() {
        return Err(CommandError::OutOfRange {
            property: "rows".to_owned(),
            detail: "a matrix needs at least one row".to_owned(),
        });
    }
    if rows.len() > MAX_MATRIX_ROWS {
        return Err(CommandError::OutOfRange {
            property: "rows".to_owned(),
            detail: format!("{} rows exceed the {MAX_MATRIX_ROWS}-row cap", rows.len()),
        });
    }
    let columns = rows[0].len();
    if columns == 0 {
        return Err(CommandError::OutOfRange {
            property: "rows".to_owned(),
            detail: "row 0 has no cells".to_owned(),
        });
    }
    if columns > MAX_MATRIX_COLS {
        return Err(CommandError::OutOfRange {
            property: "rows".to_owned(),
            detail: format!("{columns} cells in a row exceed the {MAX_MATRIX_COLS}-column cap"),
        });
    }
    for (r, row) in rows.iter().enumerate() {
        if row.len() != columns {
            return Err(CommandError::OutOfRange {
                property: "rows".to_owned(),
                detail: format!("row {r} has {} cells, row 0 has {columns}", row.len()),
            });
        }
        for (c, cell) in row.iter().enumerate() {
            match cell {
                Cell::Number(value) if !value.is_finite() => {
                    return Err(CommandError::OutOfRange {
                        property: "rows".to_owned(),
                        detail: format!("row {r}, cell {c} is {value}, not a finite number"),
                    });
                }
                Cell::Text(text) if text.trim().is_empty() => {
                    return Err(CommandError::OutOfRange {
                        property: "rows".to_owned(),
                        detail: format!("row {r}, cell {c} is blank"),
                    });
                }
                _ => {}
            }
        }
    }
    for (labels, field, want, what) in [
        (args.column_labels, "column_labels", columns, "columns"),
        (args.row_labels, "row_labels", rows.len(), "rows"),
    ] {
        let Some(labels) = labels else { continue };
        if labels.len() != want {
            return Err(CommandError::OutOfRange {
                property: field.to_owned(),
                detail: format!("{} labels for a matrix with {want} {what}", labels.len()),
            });
        }
        if let Some(index) = labels.iter().position(|label| label.trim().is_empty()) {
            return Err(CommandError::OutOfRange {
                property: field.to_owned(),
                detail: format!("label {index} is blank"),
            });
        }
    }
    if let Some(lhs) = args.lhs
        && lhs.trim().is_empty()
    {
        return Err(CommandError::OutOfRange {
            property: "lhs".to_owned(),
            detail: "a left-hand side needs something to say".to_owned(),
        });
    }
    if let Some(size) = args.cell_size
        && (!size.is_finite() || size <= 0.0)
    {
        return Err(CommandError::OutOfRange {
            property: "cell_size".to_owned(),
            detail: format!("{size} is not a finite, positive size"),
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
    // A matrix is its cells, and a cell is text, so there is no bare spelling
    // to fall back to — unlike a figure, which draws without labels.
    if !typesetter.supports_text() {
        return Err(CommandError::Unsupported {
            feature: "text".to_owned(),
        });
    }

    let cell_spec = TextSpec {
        size: args.cell_size,
        max_width: None,
    };
    let row_count = rows.len();

    let mut build = Build::open(tree, args.label, args.position, "position")?;
    let result = (|| -> Result<(), CommandError> {
        // Typeset every cell before placing anything: the column widths and row
        // heights decide where each cell goes, so the whole grid has to be
        // measured first.
        let mut blocks: Vec<Vec<TextBlock>> = Vec::with_capacity(row_count);
        for row in rows {
            let mut line = Vec::with_capacity(columns);
            for cell in row {
                let text = cell.to_string();
                let block = typesetter
                    .text(&text, &cell_spec)
                    .map_err(|error| typeset_error(error, "rows"))?;
                line.push(block);
            }
            blocks.push(line);
        }
        let extent = |block: &TextBlock| {
            block
                .local_bounds()
                .map_or((0.0, 0.0), |rect| (rect.width(), rect.height()))
        };
        let mut column_widths = vec![0.0_f64; columns];
        let mut row_heights = vec![0.0_f64; row_count];
        for (r, line) in blocks.iter().enumerate() {
            for (c, block) in line.iter().enumerate() {
                let (w, h) = extent(block);
                column_widths[c] = column_widths[c].max(w);
                row_heights[r] = row_heights[r].max(h);
            }
        }
        let content_width =
            column_widths.iter().sum::<f64>() + MATRIX_COLUMN_GAP * (columns - 1) as f64;
        let content_height =
            row_heights.iter().sum::<f64>() + MATRIX_ROW_GAP * (row_count - 1) as f64;
        let mut column_centres = Vec::with_capacity(columns);
        let mut x = -content_width / 2.0;
        for width in &column_widths {
            column_centres.push(x + width / 2.0);
            x += width + MATRIX_COLUMN_GAP;
        }
        let mut row_centres = Vec::with_capacity(row_count);
        let mut y = content_height / 2.0;
        for height in &row_heights {
            row_centres.push(y - height / 2.0);
            y -= height + MATRIX_ROW_GAP;
        }

        // The brackets are as tall as the grid. A hairline is the floor rather
        // than zero so a degenerate row cannot produce a zero-height polygon.
        let bracket_height = content_height.max(DEFAULT_THICKNESS * 2.0);
        let mut reach = 0.0;
        match args.brackets {
            MatrixBrackets::None => {}
            MatrixBrackets::Square => {
                let serif = (bracket_height * MATRIX_SERIF_FRACTION)
                    .clamp(MATRIX_SERIF_MIN, MATRIX_SERIF_MAX)
                    .max(DEFAULT_THICKNESS * 2.0);
                reach = BRACKET_GAP + serif;
                for (outer, direction, name) in [
                    (-content_width / 2.0 - reach, 1.0, LEFT_BRACKET_LABEL),
                    (content_width / 2.0 + reach, -1.0, RIGHT_BRACKET_LABEL),
                ] {
                    let id = build.piece(
                        ShapeKind::Polygon,
                        ShapeParams {
                            points: Some(square_bracket(
                                outer,
                                direction,
                                bracket_height,
                                serif,
                                DEFAULT_THICKNESS,
                            )),
                            color: args.color,
                            label: Some(name.to_owned()),
                            ..ShapeParams::default()
                        },
                    );
                    if let Some(m) = build.tree.get_mut(id) {
                        // A bracket is scenery: a `Polygon` kind would make
                        // `--inspect` call it a small four-sided figure with an
                        // area, which reads as a mistake.
                        m.kind = NodeKind::Unknown;
                        m.dirty.semantic = true;
                    }
                }
            }
            MatrixBrackets::Bars => {
                reach = BRACKET_GAP;
                for (x, name) in [
                    (-content_width / 2.0 - reach, LEFT_BRACKET_LABEL),
                    (content_width / 2.0 + reach, RIGHT_BRACKET_LABEL),
                ] {
                    // A `Line` is built along +x from its tail; the bar is the
                    // same stroke turned a quarter turn about its own tail.
                    let mut m = rustim_mobject::shapes::build(
                        ShapeKind::Line,
                        &ShapeParams {
                            width: Some(bracket_height),
                            color: args.color,
                            label: Some(name.to_owned()),
                            ..ShapeParams::default()
                        },
                    );
                    m.transform = Affine2::translate((x, -bracket_height / 2.0))
                        * Affine2::rotate(std::f64::consts::FRAC_PI_2);
                    build.node(m);
                }
            }
            MatrixBrackets::Round => {
                let semi_width = (bracket_height * MATRIX_ROUND_FRACTION)
                    .clamp(MATRIX_ROUND_MIN, MATRIX_ROUND_MAX);
                reach = BRACKET_GAP + semi_width;
                // `centre_x` is where the arc *opens* — one gap clear of the
                // entries — and the bulge reaches half a semi-width further
                // out. Putting the ellipse's centre there instead would leave
                // the bracket a whole semi-width too far from the grid.
                for (centre_x, left, name) in [
                    (-content_width / 2.0 - BRACKET_GAP, true, LEFT_BRACKET_LABEL),
                    (
                        content_width / 2.0 + BRACKET_GAP,
                        false,
                        RIGHT_BRACKET_LABEL,
                    ),
                ] {
                    build.piece(
                        ShapeKind::Plot,
                        ShapeParams {
                            points: Some(round_bracket(
                                centre_x,
                                semi_width,
                                bracket_height / 2.0,
                                left,
                            )),
                            color: args.color,
                            label: Some(name.to_owned()),
                            ..ShapeParams::default()
                        },
                    );
                }
            }
        }

        // The cells, row-major, centred in their measured column and row.
        for (r, line) in blocks.iter().enumerate() {
            for (c, block) in line.iter().enumerate() {
                let text = rows[r][c].to_string();
                build.text(
                    &text,
                    block.clone(),
                    &cell_spec,
                    [column_centres[c], row_centres[r]],
                    args.color,
                    Some(text.clone()),
                );
            }
        }
        // The labels are typeset at the cell size too: a column label that was
        // bigger than the entries would name the matrix rather than annotate
        // it. They stay foreground, because `color` names the cells.
        if let Some(labels) = args.column_labels {
            for (c, text) in labels.iter().enumerate() {
                let block = typesetter
                    .text(text, &cell_spec)
                    .map_err(|error| typeset_error(error, "column_labels"))?;
                let half = block.local_bounds().map_or(0.0, |rect| rect.height() / 2.0);
                let y = bracket_height / 2.0 + DEFAULT_GAP + half;
                build.text(
                    text,
                    block,
                    &cell_spec,
                    [column_centres[c], y],
                    None,
                    Some(text.clone()),
                );
            }
        }
        // The picture's left edge, moved out by whatever hangs off the left.
        let mut left_edge = -content_width / 2.0 - reach;
        if let Some(labels) = args.row_labels {
            let mut widest: f64 = 0.0;
            for (r, text) in labels.iter().enumerate() {
                let block = typesetter
                    .text(text, &cell_spec)
                    .map_err(|error| typeset_error(error, "row_labels"))?;
                let (width, _) = extent(&block);
                widest = widest.max(width);
                // Right-aligned against the bracket, so a column of row names
                // reads as a column and its right edge is one line.
                let x = left_edge - LABEL_GAP - width / 2.0;
                build.text(
                    text,
                    block,
                    &cell_spec,
                    [x, row_centres[r]],
                    None,
                    Some(text.clone()),
                );
            }
            left_edge -= LABEL_GAP + widest;
        }
        if let Some(lhs) = args.lhs {
            let block = typesetter
                .text(lhs, &cell_spec)
                .map_err(|error| typeset_error(error, "lhs"))?;
            let half = block.local_bounds().map_or(0.0, |rect| rect.width() / 2.0);
            let x = left_edge - LABEL_GAP - half;
            build.text(lhs, block, &cell_spec, [x, 0.0], None, Some(lhs.to_owned()));
        }

        if let Some(caption) = caption {
            let spec = TextSpec {
                size: args.caption_size,
                max_width: None,
            };
            let block = typesetter
                .text(caption, &spec)
                .map_err(|error| typeset_error(error, "caption"))?;
            let half = block.local_bounds().map_or(0.0, |rect| rect.height() / 2.0);
            // The lowest piece decides where the caption goes — usually a cell,
            // or a column label when the grid is short.
            let bottom = build
                .created
                .iter()
                .filter_map(|id| build.tree.world_bounds(*id))
                .map(|rect| rect.y0 - build.origin[1])
                .fold(f64::INFINITY, f64::min);
            let y = if bottom.is_finite() {
                bottom - DEFAULT_GAP - half
            } else {
                -content_height / 2.0 - DEFAULT_GAP - half
            };
            build.text(caption, block, &spec, [0.0, y], None, None);
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

/// The eight corners of a square bracket, as one simple contour.
///
/// The union of a shaft and two arms is an L-shaped outline — eight corners, not
/// three rectangles — and a single `Polygon` paints it under the fill rule with
/// no subpath break, which a flat point array cannot express anyway. `outer` is
/// the x of the bracket's outside edge; `direction` is `1.0` for a left bracket
/// (its arms reach right, towards the entries) and `-1.0` for a right one.
fn square_bracket(
    outer: f64,
    direction: f64,
    height: f64,
    serif: f64,
    thickness: f64,
) -> Vec<[f64; 2]> {
    let half = height / 2.0;
    let arm = outer + direction * serif;
    let shaft = outer + direction * thickness;
    vec![
        [outer, -half],
        [arm, -half],
        [arm, -half + thickness],
        [shaft, -half + thickness],
        [shaft, half - thickness],
        [arm, half - thickness],
        [arm, half],
        [outer, half],
    ]
}

/// A round bracket: the half of an ellipse that faces away from the entries.
///
/// Sampled rather than approximated by a cubic — the stroker flattens it into
/// exactly the polyline the samples describe, and a quarter-ellipse Bézier
/// would need its own control-point constant to be right. `centre_x` is where
/// the arc opens; `semi_width` is how far it bulges away from the entries, so
/// the caller passes the open edge and the extent grows outwards from there.
fn round_bracket(centre_x: f64, semi_width: f64, semi_height: f64, left: bool) -> Vec<[f64; 2]> {
    let start = if left {
        std::f64::consts::FRAC_PI_2
    } else {
        -std::f64::consts::FRAC_PI_2
    };
    (0..=ROUND_BRACKET_SAMPLES)
        .map(|i| {
            let angle = start + std::f64::consts::PI * i as f64 / ROUND_BRACKET_SAMPLES as f64;
            [
                centre_x + semi_width * angle.cos(),
                semi_height * angle.sin(),
            ]
        })
        .collect()
}

// ---- explain.chart ------------------------------------------------------

/// The most bars one chart may have.
///
/// The same posture as the other templates' caps: a command from a model must
/// not be able to allocate hundreds of text nodes. Thirty-two bars is already
/// more than a 720p frame can label legibly, and it leaves every bar a band of
/// roughly a fifth of a world unit at the default width.
const MAX_CATEGORIES: usize = 32;

/// The geometric size of the chart frame, in world units — `explain.plot`'s
/// proportions, so a lecture can swap one for the other without re-laying out
/// the slide.
const CHART_WIDTH: f64 = 7.0;
const CHART_HEIGHT: f64 = 4.0;

/// How much of its band a bar occupies. The rest is the gap that separates one
/// bar from the next; at `1.0` the bars would touch and read as one shape.
const CHART_BAR_FRACTION: f64 = 0.7;

/// The headroom added past the tallest (or lowest) bar when the value window is
/// chosen by the template: a fraction of the window's span, so a long bar does
/// not end flush against the frame and a value label has somewhere to sit.
const CHART_HEADROOM: f64 = 0.05;

/// The number of value-axis labels an automatically chosen step aims for.
const CHART_TARGET_TICKS: f64 = 5.0;

/// The stand-off between a bar and its own value label, and between the frame
/// and the category labels.
const CHART_LABEL_GAP: f64 = 0.12;

/// The two axes' names. Scenery, like a matrix's brackets: `--inspect` names
/// them so a reader knows which axis is which without hearing a size.
const VALUE_AXIS_LABEL: &str = "数值轴";
const CATEGORY_AXIS_LABEL: &str = "类别轴";

/// The slots `explain.chart` fills.
pub(crate) struct Chart<'a> {
    pub categories: &'a [String],
    pub values: &'a [f64],
    /// The value window; must contain zero.
    pub value_range: Option<[f64; 2]>,
    /// Data units between value-axis ticks; absent chooses a "nice" step.
    pub step: Option<f64>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    /// Whether each bar carries its own value as a label.
    pub show_values: Option<bool>,
    /// The bars' colour; the axes and every label stay foreground.
    pub color: Option<[u8; 3]>,
    pub position: Option<[f64; 2]>,
    pub caption: Option<&'a str>,
    pub caption_size: Option<f64>,
    pub label: Option<&'a str>,
}

/// A vertical bar chart: one bar per category, a labelled value axis and a
/// baseline at zero.
///
/// The template exists because a model asked to "show the sales by quarter"
/// should not have to work out the band widths, where the category labels go,
/// or that a bar chart's baseline must stay at zero. It fills the slots and the
/// engine makes those decisions.
///
/// The bars share one baseline at value `0`: a positive value rises from it and
/// a negative value hangs below it, so one chart can show a gain and a loss.
/// The value window always contains zero — a caller-supplied one that does not
/// is refused, because a truncated baseline makes every bar's height a lie
/// about its ratio.
///
/// Pieces, in `created` order: the value axis (`NumberLine` turned a quarter
/// turn so its ticks and labels read upward), the category axis (a stroked
/// `Line` along the baseline), one bar per category, one value label per bar
/// when `show_values` is on, the value axis's tick labels, the category labels,
/// then the caption. Each bar is labelled by its category; the category label
/// below the axis deliberately carries no label of its own, since a reader who
/// reaches the bar is already told its name.
pub(crate) fn chart(
    tree: &mut SceneTree,
    typesetter: &mut dyn Typesetter,
    args: Chart<'_>,
) -> Result<StateDiff, CommandError> {
    let categories = args.categories;
    let values = args.values;
    if categories.is_empty() {
        return Err(CommandError::OutOfRange {
            property: "categories".to_owned(),
            detail: "a chart needs at least one category".to_owned(),
        });
    }
    if categories.len() > MAX_CATEGORIES {
        return Err(CommandError::OutOfRange {
            property: "categories".to_owned(),
            detail: format!(
                "{} categories exceed the {MAX_CATEGORIES}-category cap",
                categories.len()
            ),
        });
    }
    if let Some(index) = categories.iter().position(|c| c.trim().is_empty()) {
        return Err(CommandError::OutOfRange {
            property: "categories".to_owned(),
            detail: format!("category {index} is blank"),
        });
    }
    if values.len() != categories.len() {
        return Err(CommandError::OutOfRange {
            property: "values".to_owned(),
            detail: format!(
                "{} values for {} categories",
                values.len(),
                categories.len()
            ),
        });
    }
    if let Some(value) = values.iter().copied().find(|v| !v.is_finite()) {
        return Err(CommandError::OutOfRange {
            property: "values".to_owned(),
            detail: format!("{value} is not a finite value"),
        });
    }
    if values.iter().all(|v| *v == 0.0) {
        return Err(CommandError::OutOfRange {
            property: "values".to_owned(),
            detail: "every value is zero, so no bar would be visible".to_owned(),
        });
    }
    let width = positive(args.width, CHART_WIDTH, "width")?;
    let height = positive(args.height, CHART_HEIGHT, "height")?;
    // The value window: the caller's, or the values' own extent with zero
    // always included and a little headroom past the extremes.
    let (low, high) = values
        .iter()
        .fold((0.0_f64, 0.0_f64), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let (mut default_lo, mut default_hi) = (low, high);
    let span = default_hi - default_lo;
    let pad = span * CHART_HEADROOM;
    if default_hi > 0.0 {
        default_hi += pad;
    }
    if default_lo < 0.0 {
        default_lo -= pad;
    }
    let value_range = finite_range(args.value_range, [default_lo, default_hi], "value_range")?;
    if value_range[0] > 0.0 || value_range[1] < 0.0 {
        return Err(CommandError::OutOfRange {
            property: "value_range".to_owned(),
            detail: format!(
                "[{}, {}] does not contain zero, and a bar chart's baseline is zero",
                value_range[0], value_range[1]
            ),
        });
    }
    if let Some((index, value)) = values
        .iter()
        .copied()
        .enumerate()
        .find(|(_, v)| *v < value_range[0] || *v > value_range[1])
    {
        return Err(CommandError::OutOfRange {
            property: "value_range".to_owned(),
            detail: format!(
                "value {value} at category {index} is outside [{}, {}]",
                value_range[0], value_range[1]
            ),
        });
    }
    let step = positive(
        Some(
            args.step
                .unwrap_or_else(|| nice_step(value_range[1] - value_range[0])),
        ),
        1.0,
        "step",
    )?;
    let ticks = axis_labels(value_range, step).ok_or_else(|| CommandError::OutOfRange {
        property: "step".to_owned(),
        detail: format!(
            "value_range [{}, {}] needs more than {MAX_LABELS} labels at step {step}; use a larger step",
            value_range[0], value_range[1]
        ),
    })?;
    if ticks.len() < 2 {
        return Err(CommandError::OutOfRange {
            property: "step".to_owned(),
            detail: format!(
                "step {step} leaves {} label on [{}, {}]; use a smaller step",
                ticks.len(),
                value_range[0],
                value_range[1]
            ),
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
    // The category labels and the value-axis ticks are not optional — a chart
    // with no category labels is a picture, not an explanation — so the whole
    // template needs a text typesetter, caption or not.
    if !typesetter.supports_text() {
        return Err(CommandError::Unsupported {
            feature: "text".to_owned(),
        });
    }

    let count = categories.len();
    let scale = AxesScale::new(
        (0.0, count as f64),
        width,
        (value_range[0], value_range[1]),
        height,
    );
    let left = -width / 2.0;
    let baseline = scale.to_world(0.0, 0.0).y;
    let band = width / count as f64;
    let bar_width = band * CHART_BAR_FRACTION;
    let show_values = args.show_values.unwrap_or(true);
    let label_spec = TextSpec {
        size: Some(TICK_LABEL_SIZE),
        max_width: None,
    };

    let mut build = Build::open(tree, args.label, args.position, "position")?;
    let result = (|| -> Result<(), CommandError> {
        // The value axis: a number line turned a quarter turn, so a data unit
        // runs up the frame and its ticks stand off to the left. Its data range
        // is the value window, which is what puts a tick and a label at every
        // round number a reader needs.
        let mut axis = rustim_mobject::shapes::build(
            ShapeKind::NumberLine,
            &ShapeParams {
                width: Some(height),
                range: Some(value_range),
                step: Some(step),
                tick_size: Some(AXES_TICK_SIZE),
                ..ShapeParams::default()
            },
        );
        axis.label = Some(VALUE_AXIS_LABEL.to_owned());
        // `NumberLine` is centred on its own length, so the turn about its
        // centre puts zero at the world origin and the data range across
        // `[-height/2, height/2]`; the translation then puts its shaft on the
        // frame's left edge. `+π/2` is the direction that puts the high end at
        // the top (`-π/2` would stand every value on its head).
        axis.transform = Affine2::translate((left, 0.0)) * Affine2::rotate(FRAC_PI_2);
        build.node(axis);

        let mut line = rustim_mobject::shapes::build(
            ShapeKind::Line,
            &ShapeParams {
                width: Some(width),
                position: Some([left, baseline]),
                ..ShapeParams::default()
            },
        );
        line.label = Some(CATEGORY_AXIS_LABEL.to_owned());
        build.node(line);

        for (index, (&value, category)) in values.iter().zip(categories).enumerate() {
            let x = scale.to_world(index as f64 + 0.5, 0.0).x;
            let top = scale.to_world(0.0, value).y;
            // A zero value still gets a bar — a hairline on the baseline —
            // rather than no node, so every category is addressable and a
            // lecture can animate it from there.
            let bar_height = (top - baseline).abs().max(DEFAULT_THICKNESS);
            build.piece(
                ShapeKind::Square,
                ShapeParams {
                    width: Some(bar_width),
                    height: Some(bar_height),
                    color: args.color,
                    position: Some([x, (top + baseline) / 2.0]),
                    label: Some(category.clone()),
                    ..ShapeParams::default()
                },
            );
        }

        if show_values {
            for (index, &value) in values.iter().enumerate() {
                let text = tick_text(value);
                let block = typesetter
                    .text(&text, &label_spec)
                    .map_err(|error| typeset_error(error, "values"))?;
                let half = block.local_bounds().map_or(0.0, |rect| rect.height() / 2.0);
                let x = scale.to_world(index as f64 + 0.5, 0.0).x;
                let edge = scale.to_world(0.0, value).y;
                // A negative bar carries its value underneath, so the label
                // never sits between the bar and its baseline.
                let y = if value < 0.0 {
                    edge - CHART_LABEL_GAP - half
                } else {
                    edge + CHART_LABEL_GAP + half
                };
                build.text(&text, block, &label_spec, [x, y], None, Some(text.clone()));
            }
        }

        // The value axis's own numbers, right-aligned one tick-length and one
        // gap clear of the shaft. The zero label is kept: unlike a cartesian
        // plot's origin, the baseline is the number a bar chart is read against.
        for &value in &ticks {
            let text = tick_text(value);
            let block = typesetter
                .text(&text, &label_spec)
                .map_err(|error| typeset_error(error, "value_range"))?;
            let half = block.local_bounds().map_or(0.0, |rect| rect.width() / 2.0);
            let x = left - AXES_TICK_SIZE - CHART_LABEL_GAP - half;
            let y = scale.to_world(0.0, value).y;
            build.text(&text, block, &label_spec, [x, y], None, Some(text.clone()));
        }
        // Category labels wrap at their own band, so a long word cannot run
        // into its neighbour; each label's measured height keeps a wrapped one
        // clear of the axis.
        let category_spec = TextSpec {
            size: Some(TICK_LABEL_SIZE),
            max_width: Some(band),
        };
        for (index, category) in categories.iter().enumerate() {
            let block = typesetter
                .text(category, &category_spec)
                .map_err(|error| typeset_error(error, "categories"))?;
            let half = block.local_bounds().map_or(0.0, |rect| rect.height() / 2.0);
            let x = scale.to_world(index as f64 + 0.5, 0.0).x;
            build.text(
                category,
                block,
                &category_spec,
                [x, -height / 2.0 - DEFAULT_GAP - half],
                None,
                None,
            );
        }

        if let Some(caption) = caption {
            let spec = TextSpec {
                size: args.caption_size,
                max_width: None,
            };
            let block = typesetter
                .text(caption, &spec)
                .map_err(|error| typeset_error(error, "caption"))?;
            let caption_half = block.local_bounds().map_or(0.0, |rect| rect.height() / 2.0);
            let bottom = build
                .created
                .iter()
                .filter_map(|id| build.tree.world_bounds(*id))
                .map(|rect| rect.y0 - build.origin[1])
                .fold(f64::INFINITY, f64::min);
            let y = if bottom.is_finite() {
                bottom - DEFAULT_GAP - caption_half
            } else {
                -height / 2.0 - DEFAULT_GAP - caption_half
            };
            build.text(caption, block, &spec, [0.0, y], None, None);
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

/// A "nice" tick step: `1`, `2`, `5` or `10` times a power of ten, chosen so
/// the window gets about [`CHART_TARGET_TICKS`] labels.
///
/// The thresholds are the usual ones (`1.5`, `3`, `7`): a raw step of `2.2`
/// rounds down to `2` rather than up to `5`, because six labels are a better
/// axis than three. A window with no span at all — refused before this is
/// called — answers `1.0` rather than producing an infinite step.
fn nice_step(span: f64) -> f64 {
    let raw = span / CHART_TARGET_TICKS;
    if !(raw.is_finite() && raw > 0.0) {
        return 1.0;
    }
    let magnitude = 10f64.powf(raw.log10().floor());
    let normalized = raw / magnitude;
    let nice = if normalized <= 1.5 {
        1.0
    } else if normalized <= 3.0 {
        2.0
    } else if normalized <= 7.0 {
        5.0
    } else {
        10.0
    };
    nice * magnitude
}

// ---- explain.algorithm --------------------------------------------------

/// The most cells one frame may have.
///
/// The same posture as [`MAX_CATEGORIES`]: a model-supplied array must not be
/// able to allocate hundreds of text nodes. At the default cell and gap this is
/// already 30 world units of row — four times a 720p frame — so the cap is a
/// guard against a runaway payload rather than a layout choice.
const MAX_CELLS: usize = 32;

/// The most named pointers one frame may carry.
///
/// A pointer is two pieces (a caret and a name) and they stack, so sixteen is
/// already more than a column can show without leaving the frame.
const MAX_POINTERS: usize = 16;

/// The side of one cell, in world units.
const ALGORITHM_CELL_SIZE: f64 = 0.8;

/// The gap between neighbouring cells.
const ALGORITHM_CELL_GAP: f64 = 0.15;

/// A cell's value text size as a fraction of the cell's side, and a cell's
/// index text size: the value is the content and the index is an annotation, so
/// the second is deliberately smaller than the first.
const ALGORITHM_VALUE_FRACTION: f64 = 0.5;
const ALGORITHM_INDEX_FRACTION: f64 = 0.28;

/// The stand-off between a cell's bottom edge and its index, and the same
/// between a pointer's tip and whatever it stands on.
const ALGORITHM_INDEX_GAP: f64 = 0.1;
const ALGORITHM_POINTER_GAP: f64 = 0.12;

/// A pointer caret's half-width and height, both as fractions of the cell side.
/// The caret is an isosceles triangle pointing **down** at the cell it names,
/// so it reads as an arrow even with its label cropped away.
const ALGORITHM_CARET_FRACTION: f64 = 0.22;

/// The slots `explain.algorithm` fills.
pub(crate) struct Algorithm<'a> {
    pub values: &'a [f64],
    /// `(name, cell index)` pairs drawn as carets above the cells they name.
    pub pointers: Option<&'a [(String, u32)]>,
    /// Cell indices painted with a wash; the rest are outlines only.
    pub highlight: Option<&'a [u32]>,
    pub cell_size: Option<f64>,
    /// The highlighted cells' colour; every outline and every label stays
    /// foreground.
    pub color: Option<[u8; 3]>,
    pub position: Option<[f64; 2]>,
    pub caption: Option<&'a str>,
    pub caption_size: Option<f64>,
    pub label: Option<&'a str>,
}

/// One frame of an algorithm's state: a row of numbered cells, with named
/// pointers over chosen ones.
///
/// The sixth and last template, and the one that is a *snapshot* rather than a
/// diagram: an algorithm explanation is a sequence of such frames, and each one
/// says "these are the values now, this is where `i` and `j` stand, these two
/// are being compared". The slots are the state; the sequence is the caller's,
/// built by sending the next frame with a different `pointers`/`highlight` (or
/// by animating a pointer with `move_to`, since the caret is an ordinary node).
///
/// Indices are **zero-based**, and the index row under the cells says so. That
/// is the one place a template deliberately does not read like prose: an
/// algorithm's own vocabulary is `a[0]`, `a[i]`, and a frame labelled `1..n`
/// would disagree with every line of code the lecture is explaining. A cell's
/// own label carries the same number (`格0`, `格1`, …), so `set` or `play` can
/// address one box without counting the tree.
///
/// Pieces, in `created` order: one cell and (value text, index text) per value,
/// then one caret and name per pointer, then the caption. Every value is
/// typeset — a frame with no numbers is not a frame — so the template needs a
/// text typesetter even when there is no caption.
///
/// Pointers over the same cell **stack** upward, one per line, so `i` and `j`
/// on the same index read as two rows rather than one on top of the other. A
/// pointer's name is its label; the caret under it is an unlabelled
/// decoration, so `set target "i"` resolves to exactly one node.
pub(crate) fn algorithm(
    tree: &mut SceneTree,
    typesetter: &mut dyn Typesetter,
    args: Algorithm<'_>,
) -> Result<StateDiff, CommandError> {
    let values = args.values;
    if values.is_empty() {
        return Err(CommandError::OutOfRange {
            property: "values".to_owned(),
            detail: "an algorithm frame needs at least one value".to_owned(),
        });
    }
    if values.len() > MAX_CELLS {
        return Err(CommandError::OutOfRange {
            property: "values".to_owned(),
            detail: format!("{} values exceed the {MAX_CELLS}-cell cap", values.len()),
        });
    }
    if let Some(value) = values.iter().copied().find(|v| !v.is_finite()) {
        return Err(CommandError::OutOfRange {
            property: "values".to_owned(),
            detail: format!("{value} is not a finite value"),
        });
    }
    let count = values.len();
    let pointers = args.pointers.unwrap_or(&[]);
    if pointers.len() > MAX_POINTERS {
        return Err(CommandError::OutOfRange {
            property: "pointers".to_owned(),
            detail: format!(
                "{} pointers exceed the {MAX_POINTERS}-pointer cap",
                pointers.len()
            ),
        });
    }
    for (i, (name, index)) in pointers.iter().enumerate() {
        if name.trim().is_empty() {
            return Err(CommandError::OutOfRange {
                property: "pointers".to_owned(),
                detail: format!("pointer {i} has a blank name"),
            });
        }
        // A name is how a lecture addresses the pointer, so two pointers with
        // one name would make `set target "i"` ambiguous — and unlike an
        // object's label, a model writing `[["i", 0], ["i", 1]]` has made a
        // mistake, not a deliberate pair of same-named things.
        if pointers[..i].iter().any(|(other, _)| other == name) {
            return Err(CommandError::OutOfRange {
                property: "pointers".to_owned(),
                detail: format!("two pointers are both named {name:?}"),
            });
        }
        if *index as usize >= count {
            return Err(CommandError::OutOfRange {
                property: "pointers".to_owned(),
                detail: format!("pointer {name:?} names cell {index}, but there are {count} cells"),
            });
        }
    }
    let highlight = args.highlight.unwrap_or(&[]);
    if highlight.len() > MAX_CELLS {
        return Err(CommandError::OutOfRange {
            property: "highlight".to_owned(),
            detail: format!(
                "{} highlighted cells exceed the {MAX_CELLS}-cell cap",
                highlight.len()
            ),
        });
    }
    if let Some(index) = highlight.iter().find(|index| **index as usize >= count) {
        return Err(CommandError::OutOfRange {
            property: "highlight".to_owned(),
            detail: format!("highlighted cell {index}, but there are {count} cells"),
        });
    }
    let cell = positive(args.cell_size, ALGORITHM_CELL_SIZE, "cell_size")?;
    if let Some(size) = args.caption_size
        && (!size.is_finite() || size <= 0.0)
    {
        return Err(CommandError::OutOfRange {
            property: "caption_size".to_owned(),
            detail: format!("{size} is not a finite, positive size"),
        });
    }
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
    // Every cell's value and every index is typeset, caption or not.
    if !typesetter.supports_text() {
        return Err(CommandError::Unsupported {
            feature: "text".to_owned(),
        });
    }

    let pitch = cell + ALGORITHM_CELL_GAP;
    let leftmost = -((count as f64 - 1.0) / 2.0) * pitch;
    let column = |index: usize| leftmost + index as f64 * pitch;
    let index_size = cell * ALGORITHM_INDEX_FRACTION;
    let index_spec = TextSpec {
        size: Some(index_size),
        max_width: None,
    };
    let value_spec = TextSpec {
        size: Some(cell * ALGORITHM_VALUE_FRACTION),
        // A long number wraps at the cell rather than running into its
        // neighbour; the cell's own box is what bounds it.
        max_width: Some(cell),
    };
    let highlighted = |index: usize| highlight.iter().any(|h| *h as usize == index);

    let mut build = Build::open(tree, args.label, args.position, "position")?;
    let result = (|| -> Result<(), CommandError> {
        for (index, &value) in values.iter().enumerate() {
            let x = column(index);
            let on = highlighted(index);
            let id = build.piece(
                ShapeKind::Square,
                ShapeParams {
                    width: Some(cell),
                    height: Some(cell),
                    color: if on { args.color } else { None },
                    position: Some([x, 0.0]),
                    label: Some(format!("格{index}")),
                    ..ShapeParams::default()
                },
            );
            if let Some(m) = build.tree.get_mut(id) {
                // A cell is an outline; a highlighted one is the same outline
                // with a translucent wash behind it, so the grid's edges stay
                // a single continuous rule either way.
                m.style.fill_opacity = if on { 0.35 } else { 0.0 };
                m.style.stroke_opacity = 1.0;
                m.style.stroke = m.style.fill;
                m.style.stroke_width = DEFAULT_THICKNESS;
                m.dirty.style = true;
            }

            let text = tick_text(value);
            let block = typesetter
                .text(&text, &value_spec)
                .map_err(|error| typeset_error(error, "values"))?;
            build.text(
                &text,
                block,
                &value_spec,
                [x, 0.0],
                None,
                Some(text.clone()),
            );

            let index_text = index.to_string();
            let block = typesetter
                .text(&index_text, &index_spec)
                .map_err(|error| typeset_error(error, "values"))?;
            let half = block.local_bounds().map_or(0.0, |rect| rect.height() / 2.0);
            // The index row hangs off the cell's bottom edge by its own measured
            // half-height, so a shaper with a taller line box does not push it
            // into the cell and a shorter one does not leave a gap.
            build.text(
                &index_text,
                block,
                &index_spec,
                [x, -cell / 2.0 - ALGORITHM_INDEX_GAP - half],
                None,
                None,
            );
        }

        // Each column's pen: how high above it the next pointer stands. Two
        // pointers on one index stack, one per line, rather than overlapping.
        let mut pen = vec![cell / 2.0; count];
        let caret = cell * ALGORITHM_CARET_FRACTION;
        for (name, index) in pointers {
            let at = *index as usize;
            let x = column(at);
            let tip = pen[at] + ALGORITHM_POINTER_GAP;
            let marker = build.piece(
                ShapeKind::Polygon,
                ShapeParams {
                    points: Some(vec![
                        [x, tip],
                        [x - caret, tip + caret],
                        [x + caret, tip + caret],
                    ]),
                    color: args.color,
                    ..ShapeParams::default()
                },
            );
            if let Some(m) = build.tree.get_mut(marker) {
                // The builder already leaves a polygon as a filled contour; the
                // caret only has to stop claiming to be a shape of its own, so
                // `--inspect` does not announce a decorative triangle as a
                // figure beside the values it points at.
                m.kind = NodeKind::Unknown;
                m.dirty.semantic = true;
            }

            let block = typesetter
                .text(name, &index_spec)
                .map_err(|error| typeset_error(error, "pointers"))?;
            let half = block.local_bounds().map_or(0.0, |rect| rect.height() / 2.0);
            let centre = tip + caret + ALGORITHM_POINTER_GAP / 2.0 + half;
            build.text(
                name,
                block,
                &index_spec,
                [x, centre],
                None,
                Some(name.clone()),
            );
            pen[at] = centre + half;
        }

        if let Some(caption) = caption {
            let spec = TextSpec {
                size: args.caption_size,
                max_width: None,
            };
            let block = typesetter
                .text(caption, &spec)
                .map_err(|error| typeset_error(error, "caption"))?;
            let caption_half = block.local_bounds().map_or(0.0, |rect| rect.height() / 2.0);
            let bottom = build
                .created
                .iter()
                .filter_map(|id| build.tree.world_bounds(*id))
                .map(|rect| rect.y0 - build.origin[1])
                .fold(f64::INFINITY, f64::min);
            let y = if bottom.is_finite() {
                bottom - DEFAULT_GAP - caption_half
            } else {
                -cell / 2.0 - DEFAULT_GAP - caption_half
            };
            build.text(caption, block, &spec, [0.0, y], None, None);
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

#[cfg(test)]
mod tests {
    //! The pure half of `explain.plot` and `explain.chart`: Horner, the
    //! sampling runs, the tick text and the "nice" step. The command-level
    //! tests in `context.rs` cover the group, the placement and the refusals;
    //! these cover the arithmetic those tests would otherwise exercise only
    //! through a whole scene.

    use super::*;

    #[test]
    fn a_nice_step_rounds_to_one_two_five_or_ten() {
        // A window of 4.2 wants a step of 1: five labels (`0`…`4`) is a better
        // axis than the three a step of 5 would leave.
        assert_eq!(nice_step(4.2), 1.0);
        // 11 wants 2, not 5 — the same argument one power of ten up.
        assert_eq!(nice_step(11.0), 2.0);
        assert_eq!(nice_step(1.0), 0.2);
        assert_eq!(nice_step(100.0), 20.0);
        // The template refuses a zero span before it gets here; the guard is
        // for a direct caller, and it must not divide by zero.
        assert_eq!(nice_step(0.0), 1.0);
        assert_eq!(nice_step(f64::NAN), 1.0);
    }

    #[test]
    fn horner_evaluates_a_polynomial_lowest_degree_first() {
        // 1 - 2x + 3x² at x = 2 is 1 - 4 + 12.
        assert_eq!(evaluate(&[1.0, -2.0, 3.0], 2.0), 9.0);
        // A constant, and the empty-ish guard the caller refuses before this.
        assert_eq!(evaluate(&[5.0], 123.0), 5.0);
    }

    #[test]
    fn a_run_is_cut_exactly_at_the_range_edge() {
        let runs = sample_runs(&[0.0, 0.0, 1.0], [-2.0, 2.0], [0.0, 3.0], 40);
        assert_eq!(runs.len(), 1, "x² crosses the top once on each side");
        let run = &runs[0];
        let first = run.first().copied().unwrap();
        let last = run.last().copied().unwrap();
        assert_eq!(
            first[1], 3.0,
            "the run starts on the boundary, not a sample short"
        );
        assert_eq!(last[1], 3.0, "and ends on it");
        // The crossing is interpolated, so it is within a sample of √3.
        assert!((first[0].abs() - 3.0_f64.sqrt()).abs() < 0.05, "{first:?}");
        assert!(run.iter().all(|[_, y]| (0.0..=3.0).contains(y)));
    }

    #[test]
    fn a_curve_that_leaves_and_returns_makes_two_runs() {
        let runs = sample_runs(&[0.0, 0.0, 1.0], [-2.0, 2.0], [0.5, 3.0], 40);
        assert_eq!(runs.len(), 2, "the well under 0.5 is a gap");
        assert!(runs[0].last().unwrap()[0] < 0.0);
        assert!(runs[1].first().unwrap()[0] > 0.0);
        for run in &runs {
            assert!(run.len() >= 2);
            assert!(run.iter().all(|[_, y]| (0.5..=3.0).contains(y)));
        }
    }

    #[test]
    fn a_curve_outside_the_band_has_no_runs() {
        assert!(sample_runs(&[100.0], [-4.0, 4.0], [-3.0, 3.0], 160).is_empty());
    }

    #[test]
    fn ticks_are_labelled_without_float_noise() {
        assert_eq!(tick_text(2.0), "2");
        assert_eq!(tick_text(-3.0), "-3");
        assert_eq!(tick_text(0.5), "0.5");
        // 3 * 0.1 is 0.30000000000000004; a label must not say so.
        assert_eq!(tick_text(3.0 * 0.1), "0.3");
    }

    #[test]
    fn too_many_labels_is_a_refusal_not_a_truncation() {
        assert!(axis_labels([-4.0, 4.0], 0.01).is_none());
        assert_eq!(
            axis_labels([-2.0, 2.0], 1.0),
            Some(vec![-2.0, -1.0, 0.0, 1.0, 2.0])
        );
    }

    // ---- explain.geometry -------------------------------------------------

    #[test]
    fn vertices_are_centred_on_their_bounding_box() {
        let local = centred(&[[2.0, 1.0], [5.0, 1.0], [2.0, 5.0]]);
        assert_eq!(local, vec![[-1.5, -2.0], [1.5, -2.0], [-1.5, 2.0]]);
    }

    #[test]
    fn an_edge_normal_points_out_whichever_way_the_figure_is_wound() {
        let ccw = centred(&[[0.0, 0.0], [3.0, 0.0], [0.0, 4.0]]);
        let mut cw = ccw.clone();
        cw.reverse();
        for vertices in [ccw, cw] {
            for index in 0..vertices.len() {
                let (mid, normal) = edge_frame(&vertices, index);
                // The third vertex of a triangle is on the inward side of the
                // edge, so the normal has to point the opposite way from it.
                let opposite = vertices[(index + 2) % vertices.len()];
                let (dx, dy) = (mid[0] - opposite[0], mid[1] - opposite[1]);
                assert!(
                    dx * normal[0] + dy * normal[1] > 0.0,
                    "edge {index} points inward: {mid:?} · {normal:?}"
                );
            }
        }
    }

    #[test]
    fn a_right_angle_is_found_whichever_way_the_figure_is_wound() {
        let ccw = [[0.0, 0.0], [3.0, 0.0], [0.0, 4.0]];
        assert_eq!(right_angle_corners(&ccw), vec![0]);
        let cw = [[0.0, 4.0], [3.0, 0.0], [0.0, 0.0]];
        assert_eq!(right_angle_corners(&cw), vec![2]);
        assert!(right_angle_corners(&[[0.0, 0.0], [2.0, 0.0], [1.0, 2.0]]).is_empty());
    }

    #[test]
    fn a_line_of_vertices_is_collinear_but_a_bow_tie_is_not() {
        assert!(collinear(&[[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]]));
        assert!(collinear(&[[0.0, 0.0], [0.0, 0.0], [1.0, 1.0]]));
        assert!(!collinear(&[[0.0, 0.0], [3.0, 0.0], [0.0, 4.0]]));
        // A bow tie's signed area is zero, which is why the refusal is a
        // cross-product test and not an area test.
        let bow_tie = [[0.0, 0.0], [2.0, 2.0], [2.0, 0.0], [0.0, 2.0]];
        assert!(signed_area(&bow_tie).abs() < 1e-12);
        assert!(!collinear(&bow_tie));
    }

    #[test]
    fn a_label_is_pushed_clear_of_its_anchor_and_its_own_extent() {
        let rect = Rect::new(-2.0, -0.5, 2.0, 0.5);
        let below = pushed([0.0, 1.0], [0.0, -1.0], Some(rect));
        assert_eq!(below, [0.0, 1.0 - 0.5 - LABEL_GAP]);
        let diagonal = pushed([0.0, 1.0], [std::f64::consts::FRAC_1_SQRT_2; 2], Some(rect));
        // A diagonal placement clears the box's corner, not its half-width.
        let expected = (2.0 + 0.5) * std::f64::consts::FRAC_1_SQRT_2 + LABEL_GAP;
        assert!(
            (diagonal[0] - expected * std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-12,
            "{diagonal:?}"
        );
    }

    #[test]
    fn a_marker_is_a_small_square_in_the_figures_own_frame() {
        let vertices = centred(&[[0.0, 0.0], [2.0, 0.0], [0.0, 2.0]]);
        let quad = marker_quad(&vertices, 0).unwrap();
        assert_eq!(quad.len(), 4);
        assert_eq!(quad[0], vertices[0]);
        let leg = |p: [f64; 2], q: [f64; 2]| (p[0] - q[0]).hypot(p[1] - q[1]);
        // 0.18 of the shorter adjoining edge, which is 2 here.
        assert!((leg(quad[0], quad[1]) - 0.36).abs() < 1e-12, "{quad:?}");
        assert!((leg(quad[0], quad[3]) - 0.36).abs() < 1e-12, "{quad:?}");
        // The far corner is the parallelogram's, not a fourth guess.
        assert!((quad[2][0] - (quad[1][0] + quad[3][0] - quad[0][0])).abs() < 1e-12);
        assert!((quad[2][1] - (quad[1][1] + quad[3][1] - quad[0][1])).abs() < 1e-12);
    }

    // ---- explain.matrix ---------------------------------------------------

    #[test]
    fn a_square_bracket_is_one_eight_corner_l_outline() {
        let left = square_bracket(-2.0, 1.0, 4.0, 0.3, 0.05);
        assert_eq!(left.len(), 8);
        // The contour runs the outside edge, then the arms and the shaft.
        assert_eq!(left[0], [-2.0, -2.0]);
        assert_eq!(left[7], [-2.0, 2.0]);
        assert!((left[1][0] - -1.7).abs() < 1e-12);
        assert!((left[3][0] - -1.95).abs() < 1e-12);
        // Mirrored, the same shape faces the other way.
        let right = square_bracket(2.0, -1.0, 4.0, 0.3, 0.05);
        assert_eq!(right[0], [2.0, -2.0]);
        assert!((right[1][0] - 1.7).abs() < 1e-12);
        assert!((right[3][0] - 1.95).abs() < 1e-12);
        for point in left.iter().chain(&right) {
            assert!(point[1].abs() <= 2.0, "{point:?}");
        }
    }

    #[test]
    fn a_round_bracket_bulges_away_from_its_opening() {
        let left = round_bracket(-2.0, 0.2, 1.0, true);
        assert_eq!(left.len(), ROUND_BRACKET_SAMPLES + 1);
        let min_x = left.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
        assert!((min_x - -2.2).abs() < 1e-9, "{min_x}");
        // It spans the full height and stays on the outward side of its centre.
        assert!((left[0][1] - 1.0).abs() < 1e-12, "{:?}", left[0]);
        assert!((left[ROUND_BRACKET_SAMPLES][1] + 1.0).abs() < 1e-12);
        assert!(left.iter().all(|p| p[0] <= -2.0 + 1e-12));
        let right = round_bracket(2.0, 0.2, 1.0, false);
        assert!(right.iter().all(|p| p[0] >= 2.0 - 1e-12));
        assert!((right[0][1] + 1.0).abs() < 1e-12, "{:?}", right[0]);
    }
}
