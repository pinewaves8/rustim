//! The scene trait and the context a scene is handed.

use rustim_anim::AnimationScheduler;
use rustim_math::Viewport;
use rustim_mobject::{NodeId, SceneTree};
use rustim_render::{DrawList, DrawOp};

use crate::a11y;
use crate::command::{self, Command, CommandError, StateDiff};

/// A scene: the user-facing entry point, mirroring Manim's `construct()`.
///
/// Two methods rather than one, because rustim is not a batch pipeline. Manim's
/// `construct()` declares the whole timeline up front and returns; here
/// `construct` sets up the initial state and `update` is called once per frame
/// and may react to input. That split is the architecture change described in
/// `CLAUDE.md` — roughly 80% of the scheduling layer is new.
pub trait Scene {
    /// Declare objects and queue the opening animations.
    fn construct(&mut self, ctx: &mut SceneContext);

    /// Advance by `dt` seconds of virtual time.
    ///
    /// Default implementation runs the animation scheduler, which is what most
    /// scenes want; a scene that sequences stages by hand overrides this and
    /// calls `ctx.animations.update` itself.
    fn update(&mut self, dt: f64, ctx: &mut SceneContext) {
        ctx.animations.update(dt, &mut ctx.tree);
    }

    /// Called when a command changed the tree, so a reactive scene can respond.
    fn on_command(&mut self, _diff: &StateDiff, _ctx: &mut SceneContext) {}

    /// Produce this frame's draw list.
    ///
    /// The default walks the scene tree, which is what a scene built out of
    /// mobjects wants. A scene that needs a draw operation the tree cannot
    /// express yet overrides this and pushes it itself — the reference
    /// `primitives` scene does, so that the golden-image suite exercises stroke
    /// joins and clip rasterisation and not only flat fills. Both paths start
    /// from [`SceneContext::begin_frame`], so an override cannot accidentally
    /// drop the background or forget the viewport transform.
    fn build_draw_list(&self, ctx: &SceneContext, list: &mut DrawList) {
        ctx.build_draw_list(list);
    }
}

/// Everything a scene is allowed to touch.
///
/// The scene never sees a renderer, a window or a file. It mutates this context
/// and the app layer turns the result into pixels. That boundary is what lets
/// the same scene run in a window, in the exporter, and (later) in a browser.
pub struct SceneContext {
    pub tree: SceneTree,
    pub animations: AnimationScheduler,
    pub viewport: Viewport,
}

impl SceneContext {
    pub fn new(viewport: Viewport) -> Self {
        Self {
            tree: SceneTree::new(),
            animations: AnimationScheduler::new(),
            viewport,
        }
    }

    /// Insert a mobject at the root.
    pub fn add(&mut self, m: rustim_mobject::Mobject) -> NodeId {
        self.tree.insert(m)
    }

    /// The tree's root-level nodes, in draw order.
    ///
    /// Forwarded rather than cached: the tree already keeps this list and keeps
    /// it correct through `remove` and `reparent`, so a second copy here could
    /// only ever disagree with it.
    pub fn root_ids(&self) -> &[NodeId] {
        self.tree.root_ids()
    }

    /// Tear the scene down, keeping allocations.
    pub fn reset(&mut self) {
        self.tree.clear();
        self.animations.clear();
    }

    /// Apply a command from outside the engine.
    pub fn handle(&mut self, cmd: &Command) -> Result<StateDiff, CommandError> {
        command::apply(&mut self.tree, cmd)
    }

    /// This scene, projected into the semantic tree.
    ///
    /// Pure and cheap enough to call per frame, which is what the accessibility
    /// layer does — see `rustim-app::a11y` for the volume at which that stops
    /// being true.
    ///
    /// There is deliberately no `..._json` sibling here. `serde_json` is a test
    /// dependency everywhere in this workspace so that it cannot reach the
    /// `wasm32-unknown-unknown` kernel build, and this is a kernel crate; a host
    /// that wants JSON calls `serde_json::to_string(&ctx.semantic_tree(None))` on
    /// its own side of the boundary. That works because [`a11y::SemanticTree`] is
    /// derived data with no platform types in it.
    pub fn semantic_tree(&self, live: Option<String>) -> a11y::SemanticTree {
        a11y::build(&self.tree, &self.viewport, live)
    }

    /// Open a frame: clear the list, fill the background, install the viewport.
    ///
    /// Every draw list starts here, whether it is produced by the default tree
    /// walk or by a scene that pushes its own ops, so the background colour and
    /// the world→screen transform are decided in exactly one place.
    pub fn begin_frame(&self, list: &mut DrawList) {
        list.clear(self.viewport.frame_width, self.viewport.frame_height);
        list.background = Some(rustim_color::BG);
        list.push(DrawOp::SetTransform(self.viewport.affine()));
    }

    /// Flatten the tree into a backend-neutral draw list.
    ///
    /// This is where the deferred transform is finally paid for: `to_bez_path`
    /// folds each mobject's own transform together with its ancestors'. Nothing
    /// upstream of this call ever materialises world coordinates.
    pub fn build_draw_list(&self, list: &mut DrawList) {
        self.begin_frame(list);
        for id in self.tree.draw_order() {
            let Some(m) = self.tree.get(id) else { continue };
            // Points rather than `Mobject::has_picture`, and deliberately so: this
            // walk can emit one thing, a filled path, so an object with no points
            // has nothing here to draw *and nothing is missing* — a text mobject
            // is drawn by the scene that shaped it. The wider predicate is what
            // the keyboard and the semantic tree ask, and they must not ask this
            // one, or text would be unreadable by both.
            if m.opacity <= 0.0 || m.points.len() < 2 {
                continue;
            }
            let alpha = m.opacity * m.style.fill_opacity;
            let mut color = m.style.fill;
            color.components[3] = alpha as f32;
            list.push(DrawOp::SetPaint(color));
            // The mobject's own transform is folded in here rather than via a
            // `SetTransform`, because the draw list's transform is global to the
            // frame (it carries the viewport) and mixing the two would require
            // save/restore. The *ancestors'* transform is folded in by handing
            // `to_bez_path` the accumulated parent transform — passing
            // `Affine2::IDENTITY` instead silently drops every ancestor transform,
            // which is invisible only while the tree stays flat.
            list.push(DrawOp::FillPath(
                m.to_bez_path(self.tree.parent_transform(id)),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustim_mobject::shapes::{ShapeKind, ShapeParams};

    fn scene() -> SceneContext {
        SceneContext::new(Viewport::default())
    }

    #[test]
    fn adding_a_shape_puts_one_op_pair_on_the_list() {
        let mut ctx = scene();
        ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        let mut list = DrawList::default();
        ctx.build_draw_list(&mut list);
        assert_eq!(list.width, 1280);
        assert_eq!(list.background, Some(rustim_color::BG));
        let fills = list
            .ops
            .iter()
            .filter(|o| matches!(o, DrawOp::FillPath(_)))
            .count();
        assert_eq!(fills, 1);
    }

    #[test]
    fn a_fully_transparent_object_is_not_drawn() {
        let mut ctx = scene();
        let id = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Circle,
            &ShapeParams::default(),
        ));
        ctx.tree.get_mut(id).unwrap().opacity = 0.0;
        let mut list = DrawList::default();
        ctx.build_draw_list(&mut list);
        assert!(
            !list.ops.iter().any(|o| matches!(o, DrawOp::FillPath(_))),
            "an invisible object should cost nothing to draw"
        );
    }

    #[test]
    fn opacity_multiplies_into_the_paint() {
        let mut ctx = scene();
        let id = ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        {
            let m = ctx.tree.get_mut(id).unwrap();
            m.opacity = 0.5;
            m.style.fill_opacity = 0.5;
        }
        let mut list = DrawList::default();
        ctx.build_draw_list(&mut list);
        let alpha = list
            .ops
            .iter()
            .find_map(|o| match o {
                DrawOp::SetPaint(c) => Some(c.components[3]),
                _ => None,
            })
            .unwrap();
        // Composes multiplicatively, per CLAUDE.md's convention.
        assert!((alpha - 0.25).abs() < 1e-6, "got {alpha}");
    }

    #[test]
    fn commands_routed_through_the_context_stay_in_sync() {
        let mut ctx = scene();
        let cmd: Command =
            serde_json::from_str(r#"{"action":"add","type":"square","params":{"width":3.0}}"#)
                .unwrap();
        let diff = ctx.handle(&cmd).unwrap();
        assert_eq!(ctx.root_ids(), diff.created.as_slice());
        assert_eq!(ctx.tree.len(), 1);
    }

    #[test]
    fn reset_empties_everything() {
        let mut ctx = scene();
        ctx.add(rustim_mobject::shapes::build(
            ShapeKind::Square,
            &ShapeParams::default(),
        ));
        ctx.reset();
        assert!(ctx.tree.is_empty());
        assert!(ctx.root_ids().is_empty());
        assert!(ctx.animations.is_empty());
    }
}
