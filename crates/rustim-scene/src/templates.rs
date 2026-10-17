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
use rustim_mobject::shapes::{AxesScale, ShapeKind, ShapeParams};
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

#[cfg(test)]
mod tests {
    //! The pure half of `explain.plot`: Horner, the sampling runs and the tick
    //! text. The command-level tests in `context.rs` cover the group, the
    //! placement and the refusals; these cover the arithmetic those tests would
    //! otherwise exercise only through a whole scene.

    use super::*;

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
}
