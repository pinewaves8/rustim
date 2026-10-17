//! The Pythagorean figure: legs 3 and 4, a square on each of the three sides.
//!
//! The same figure the Manim reference renders (see `p0-spikes/`), which is
//! what makes the two output images directly comparable — and, now, what makes
//! the browser build's canvas directly comparable to the desktop golden.

use rustim_math::{Affine2, Point, bezier};
use rustim_mobject::{Mobject, NodeKind, Style};
use rustim_scene::{Scene, SceneContext};

/// Build a Manim-layout point array from a polygon's corners.
///
/// Manim expresses even straight-edged shapes as cubics, with the control points
/// lying on the segment. Keeping that uniform representation is what makes a
/// straight-edged mobject morphable into a curved one without a special case.
///
/// The corner list is *open* — callers name the distinct corners and let the
/// shape imply its own closure — so closing it is this function's job. The
/// interior control points come from `bezier::points_as_corners` because Manim's
/// `set_points_as_corners` puts them at 1/3 and 2/3 of the segment, not stacked
/// on its endpoints. Both spellings draw the same line, which is exactly why the
/// difference is easy to miss and expensive to find later.
fn poly(corners: &[(f64, f64)], color: rustim_color::Srgb) -> Mobject {
    let mut closed: Vec<Point> = corners.iter().map(|&(x, y)| Point::new(x, y)).collect();
    closed.push(closed[0]);
    let pts = bezier::points_as_corners(&closed);

    let style = Style {
        fill: color,
        fill_opacity: 1.0,
        stroke: color,
        stroke_opacity: 0.0,
        stroke_width: 0.0,
    };
    Mobject::new(pts).with_style(style)
}

/// How tall the figure is drawn, in Manim units — the same 6.0 the reference
/// scene passes to `scale_to_fit_height`.
const FIGURE_HEIGHT: f64 = 6.0;

/// Fit a set of mobjects to [`FIGURE_HEIGHT`] and centre them on the origin.
///
/// Measured from the geometry rather than derived by hand: the figure's bounds
/// are not the obvious ones (legs 3 and 4 do not make it 7 units tall — the
/// square on the hypotenuse reaches to y = 7 but the largest square reaches down
/// to y = -3, so it is 10 tall and 11 wide). Hand-deriving that is how the first
/// version of this scene ended up overflowing the frame by 25%.
fn fit_and_centre(objects: &mut [Mobject]) {
    let mut bounds: Option<rustim_math::Rect> = None;
    for obj in objects.iter() {
        for p in &obj.points {
            let r = rustim_math::Rect::new(p.x, p.y, p.x, p.y);
            bounds = Some(bounds.map_or(r, |b| b.union(r)));
        }
    }
    let Some(b) = bounds else { return };
    let centre = b.center();
    let place =
        Affine2::scale(FIGURE_HEIGHT / b.height()) * Affine2::translate((-centre.x, -centre.y));
    for obj in objects.iter_mut() {
        obj.transform = place;
    }
}

/// The reference scene, usable by every host.
///
/// The reveal is a list of `ctx.play` calls in `construct` — Manim's
/// `construct()` body, written down — and no state at all: the playback machine
/// knows which beat is playing, and a scene that kept its own copy would have two
/// answers to one question. The tutorial drives this off a `state` + `timer` pair,
/// which is what that split cost.
#[derive(Default)]
pub struct Pythagorean;

impl Scene for Pythagorean {
    fn construct(&mut self, ctx: &mut SceneContext) {
        const A: f64 = 3.0;
        const B: f64 = 4.0;

        // Triangle, legs along +x and +y, right angle at the origin.
        //
        // The kinds and labels are invisible in the golden images — they change
        // nothing about the geometry — and are what make the accessibility tree
        // say something. Without them a reader gets four unnamed "图形"; with
        // them it gets the figure the scene is about, and the description each
        // node carries is measured from the geometry, so a reader is told the
        // square on a leg of 3 has an area of 9 without anyone declaring it.
        let tri = poly(&[(0.0, 0.0), (A, 0.0), (0.0, B)], rustim_color::RED)
            .with_kind(NodeKind::RightTriangle)
            .with_label("直角三角形");
        // Square on leg a, below it.
        let sq_a = poly(
            &[(0.0, 0.0), (A, 0.0), (A, -A), (0.0, -A)],
            rustim_color::BLUE,
        )
        .with_kind(NodeKind::Square)
        .with_label("a 的正方形");
        // Square on leg b, to the left.
        let sq_b = poly(
            &[(0.0, 0.0), (0.0, B), (-B, B), (-B, 0.0)],
            rustim_color::YELLOW,
        )
        .with_kind(NodeKind::Square)
        .with_label("b 的正方形");
        // Square on the hypotenuse, erected outwards. Manim draws this one
        // translucent (`fill_opacity=0.45`) so the triangle shows through it;
        // matching that is what makes the two images directly comparable.
        let mut sq_c = poly(
            &[(A, 0.0), (0.0, B), (B, A + B), (A + B, A)],
            rustim_color::RED,
        )
        .with_kind(NodeKind::Square)
        .with_label("c 的正方形");
        sq_c.style.fill_opacity = 0.45;

        // Manim's own `VGroup(sq_c, sq_b, sq_a, tri)` order, which is also the
        // draw order: the translucent hypotenuse square first, the triangle over
        // it, so the triangle reads as solid red through the overlay.
        let mut figure = [sq_c, sq_b, sq_a, tri];
        fit_and_centre(&mut figure);
        // Note this is all world-space arithmetic in Manim units — no pixel
        // constants anywhere in scene code, which is the fix for the tutorial's
        // unhandled coordinate system.
        //
        // One square per beat, in the group's own order, each starting where the
        // last finished — so a slow frame makes the sequence *later*, never wrong.
        // The reveal is `FadeIn` on an object that starts at `opacity = 0`
        // (Manim's `FadeIn` draws the object from nothing, which is what this
        // engine's opacity-driven fade reproduces).
        for mut obj in figure {
            obj.opacity = 0.0;
            let id = ctx.add(obj);
            ctx.play(rustim_anim::FadeIn::new(id).with_duration(0.6));
        }
    }
}
