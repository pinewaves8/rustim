// P0 Spike C: what does vello_cpu actually cost per frame here?
//
// Each "shape" is a closed path of 4 cubic segments (Manim-ish: a rounded blob).
// We build the paths once, then time {reset + fill all + render} as one frame.
// Reports ms/frame and how many shapes fit in a 16.67 ms (60fps) / 33.3 ms (30fps) budget.

use std::time::Instant;

use vello_common::peniko::kurbo::{BezPath, Point};
use vello_common::pixmap::Pixmap;
use vello_cpu::{RenderContext, RenderSettings, Resources};

fn lcg(state: &mut u64) -> f64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    ((*state >> 11) as f64) / ((1u64 << 53) as f64)
}

/// Closed 4-cubic blob (circle-ish approximation), like a Manim Circle/Square.
fn make_shape(cx: f64, cy: f64, r: f64) -> BezPath {
    const K: f64 = 0.552_284_749_8;
    let k = K * r;
    let mut p = BezPath::new();
    p.move_to(Point::new(cx, cy - r));
    p.curve_to(
        Point::new(cx + k, cy - r),
        Point::new(cx + r, cy - k),
        Point::new(cx + r, cy),
    );
    p.curve_to(
        Point::new(cx + r, cy + k),
        Point::new(cx + k, cy + r),
        Point::new(cx, cy + r),
    );
    p.curve_to(
        Point::new(cx - k, cy + r),
        Point::new(cx - r, cy + k),
        Point::new(cx - r, cy),
    );
    p.curve_to(
        Point::new(cx - r, cy - k),
        Point::new(cx - k, cy - r),
        Point::new(cx, cy - r),
    );
    p.close_path();
    p
}

fn make_shapes(w: u16, h: u16, n: usize) -> Vec<BezPath> {
    let mut seed = 0x1234_5678_9abc_def0u64;
    (0..n)
        .map(|_| {
            let cx = lcg(&mut seed) * w as f64;
            let cy = lcg(&mut seed) * h as f64;
            let r = 2.0 + lcg(&mut seed) * 30.0;
            make_shape(cx, cy, r)
        })
        .collect()
}

fn bench(w: u16, h: u16, n: usize, reps: usize) -> f64 {
    let paths = make_shapes(w, h, n);
    let mut resources = Resources::new();
    let mut ctx = RenderContext::new(w, h);
    let mut pixmap = Pixmap::new(w, h);

    // warm up: first frame pays for strip-buffer growth etc.
    for _ in 0..2 {
        ctx.reset();
        for p in &paths {
            ctx.fill_path(p);
        }
        ctx.flush(); // required before render() when `multithreading` is enabled
        ctx.render(pixmap.as_mut(), &mut resources);
    }

    let t = Instant::now();
    for _ in 0..reps {
        ctx.reset();
        for p in &paths {
            ctx.fill_path(p);
        }
        ctx.flush();
        ctx.render(pixmap.as_mut(), &mut resources);
    }
    t.elapsed().as_secs_f64() / reps as f64
}

fn main() {
    let settings = RenderSettings::default();
    println!("vello_cpu RenderSettings: {settings:?}");
    println!(
        "cores={:?}\n",
        std::thread::available_parallelism().map(|n| n.get())
    );

    for (w, h) in [(1280u16, 720u16), (1920, 1080)] {
        println!("== {w}x{h} ==");
        println!("{:>8}  {:>10}  {:>12}  {:>10}", "shapes", "curves", "ms/frame", "fps");
        for n in [10usize, 50, 100, 500, 1000, 2000, 5000, 10000] {
            let reps = if n >= 2000 { 5 } else { 20 };
            let secs = bench(w, h, n, reps);
            println!(
                "{:>8}  {:>10}  {:>12.3}  {:>10.1}",
                n,
                n * 4,
                secs * 1000.0,
                1.0 / secs
            );
        }
        println!();
    }
}
