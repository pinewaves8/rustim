//! End-to-end damage rectangles over the *real* draw walk.
//!
//! The unit tests in `rustim-render` prove that two hand-built draw lists diff
//! correctly, and that a region render reproduces a full one. This file proves
//! the two meet: a property changed on a real `SceneContext` produces a small
//! damage rectangle, the renderer repaints exactly that rectangle, and the
//! result is indistinguishable from a full render.
//!
//! The acceptance criterion for this step was stated in terms of repaint area,
//! not frame time — a frame time moves with the machine, a scheduler and the
//! clock, while the number of pixels the rasteriser had to compute is what the
//! damage rectangle actually controls. `RenderStats::painted` is that number,
//! and the assertions below are about it.

use rustim_anim::Motion;
use rustim_math::{Affine, Viewport};
use rustim_mobject::shapes::{ShapeKind, ShapeParams, build};
use rustim_render::{CpuRenderer, Damage, DrawList, Renderer};
use rustim_scene::SceneContext;

/// Two default squares, moved well apart so a change to one cannot touch the
/// other's pixels.
fn two_square_scene() -> (SceneContext, rustim_mobject::NodeId, rustim_mobject::NodeId) {
    let mut ctx = SceneContext::new(Viewport::default(), Motion::Full);
    let left = ctx.add(build(ShapeKind::Square, &ShapeParams::default()));
    let right = ctx.add(build(ShapeKind::Square, &ShapeParams::default()));
    ctx.tree.get_mut(left).unwrap().transform = Affine::translate((-3.0, 2.0));
    ctx.tree.get_mut(right).unwrap().transform = Affine::translate((3.0, -2.0));
    (ctx, left, right)
}

fn screen(ctx: &SceneContext, world: (f64, f64)) -> (u16, u16) {
    let (x, y) = ctx.viewport.to_screen(ctx.camera(), world);
    (x as u16, y as u16)
}

#[test]
fn a_one_property_change_repaints_only_its_object() {
    let (mut ctx, _left, right) = two_square_scene();

    let mut before = DrawList::default();
    ctx.build_draw_list(&mut before);
    let mut incremental = CpuRenderer::new(before.width, before.height);
    incremental.render(&before).unwrap();

    // Exactly one property, on exactly one object.
    ctx.tree.get_mut(right).unwrap().opacity = 0.5;
    let mut after = DrawList::default();
    ctx.build_draw_list(&mut after);

    let damage = after.damage_since(&before);
    let Damage::Region(region) = damage else {
        panic!("expected a region, got {damage:?}");
    };
    let frame = u64::from(after.width) * u64::from(after.height);
    assert!(
        damage.pixels(after.width, after.height) * 20 < frame,
        "a one-object change covered {} of {frame} pixels",
        damage.pixels(after.width, after.height)
    );

    // The region is the changed square's neighbourhood and not the other one's.
    let (rx, ry) = screen(&ctx, (3.0, -2.0));
    let (lx, ly) = screen(&ctx, (-3.0, 2.0));
    assert!(
        (region.x0..region.x1).contains(&rx) && (region.y0..region.y1).contains(&ry),
        "region {region:?} does not cover the changed square at ({rx}, {ry})"
    );
    assert!(
        !(region.x0..region.x1).contains(&lx) || !(region.y0..region.y1).contains(&ly),
        "region {region:?} includes the untouched square at ({lx}, {ly})"
    );

    // The repaint really was small, measured rather than asserted by geometry.
    let stats = incremental.render_damaged(&after, damage).unwrap();
    assert!(
        !stats.is_full() && stats.painted * 20 < stats.frame,
        "{stats:?}"
    );

    // And it is the same picture a full render would have produced. Exact
    // equality, not the golden suite's tolerance: this is one frame rendered
    // two ways, not two approximations of a reference.
    let mut full = CpuRenderer::new(after.width, after.height);
    full.render(&after).unwrap();
    assert_eq!(
        incremental.pixels(),
        full.pixels(),
        "an incremental frame must be indistinguishable from a full one"
    );
}

#[test]
fn a_frame_that_did_not_change_damages_nothing() {
    let (ctx, _, _) = two_square_scene();
    let mut first = DrawList::default();
    ctx.build_draw_list(&mut first);
    let mut second = DrawList::default();
    ctx.build_draw_list(&mut second);
    assert_eq!(second.damage_since(&first), Damage::None);

    let mut renderer = CpuRenderer::new(first.width, first.height);
    renderer.render(&first).unwrap();
    let pixels = renderer.pixels().to_vec();
    let stats = renderer.render_damaged(&second, Damage::None).unwrap();
    assert_eq!(stats.painted, 0);
    assert_eq!(renderer.pixels(), &pixels[..]);
}

/// A move is the case a region render is most likely to get wrong: the pixels
/// at the old position have to be restored as well as the new ones drawn.
#[test]
fn a_move_repaints_the_old_position_and_the_new_one() {
    let (mut ctx, _, right) = two_square_scene();
    let mut before = DrawList::default();
    ctx.build_draw_list(&mut before);

    ctx.tree.get_mut(right).unwrap().transform = Affine::translate((-3.0, -2.0));
    let mut after = DrawList::default();
    ctx.build_draw_list(&mut after);

    let damage = after.damage_since(&before);
    let Damage::Region(region) = damage else {
        panic!("expected a region, got {damage:?}");
    };
    let (old_x, old_y) = screen(&ctx, (3.0, -2.0));
    let (new_x, new_y) = screen(&ctx, (-3.0, -2.0));
    for (x, y) in [(old_x, old_y), (new_x, new_y)] {
        assert!(
            (region.x0..region.x1).contains(&x) && (region.y0..region.y1).contains(&y),
            "region {region:?} misses ({x}, {y})"
        );
    }

    let mut incremental = CpuRenderer::new(before.width, before.height);
    incremental.render(&before).unwrap();
    incremental.render_damaged(&after, damage).unwrap();
    let mut full = CpuRenderer::new(after.width, after.height);
    full.render(&after).unwrap();
    assert_eq!(incremental.pixels(), full.pixels());
}
