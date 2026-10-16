// P0 Spike D: the real presentation chain, end to end.
//
//   winit window  +  softbuffer (X11 SHM)  +  vello_cpu (software raster)
//
// This is the "floor" configuration: it uses no GPU at all, which matters
// because the Vulkan driver on this machine segfaults on any render pipeline
// and the GL driver cannot compile vello_gpu's shaders. If this works at 60fps
// we have a viable engine on the target hardware.
//
// It also answers P0 item (2): vello_cpu -> Pixmap -> PNG is built in.
//
// Usage: window-probe [frames]      (default 300, then exit)

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::Instant;

use vello_common::peniko::color::{AlphaColor, Srgb};
use vello_common::peniko::kurbo::{Affine, BezPath, Point};
use vello_common::pixmap::Pixmap;
use vello_cpu::{RenderContext, Resources};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

const WIDTH: u16 = 1280;
const HEIGHT: u16 = 720;

// 3b1b palette, as consts (see CLAUDE.md conventions).
const BG: AlphaColor<Srgb> = AlphaColor::new([0.0706, 0.0706, 0.0706, 1.0]); // #121212
const BLUE: AlphaColor<Srgb> = AlphaColor::new([0.392, 0.710, 0.965, 1.0]); // #64B5F6
const YELLOW: AlphaColor<Srgb> = AlphaColor::new([1.0, 0.835, 0.310, 1.0]); // #FFD54F
const RED: AlphaColor<Srgb> = AlphaColor::new([0.937, 0.325, 0.314, 1.0]); // #EF5350

fn lcg(state: &mut u64) -> f64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    ((*state >> 11) as f64) / ((1u64 << 53) as f64)
}

fn blob(cx: f64, cy: f64, r: f64, rot: f64) -> BezPath {
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
    // rotate about the shape's own centre
    p.apply_affine(Affine::translate((cx, cy)) * Affine::rotate(rot) * Affine::translate((-cx, -cy)));
    p
}

struct Stats {
    frame_ms: Vec<f64>,
    present_ms: Vec<f64>,
}

impl Stats {
    fn new() -> Self {
        Self {
            frame_ms: Vec::new(),
            present_ms: Vec::new(),
        }
    }
    fn report(&self, label: &str) {
        let mut v: Vec<f64> = self.frame_ms.iter().copied().collect();
        if v.is_empty() {
            return;
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |p: f64| v[((v.len() as f64 - 1.0) * p) as usize];
        let mean: f64 = v.iter().sum::<f64>() / v.len() as f64;
        let mut pv = self.present_ms.clone();
        pv.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pmean: f64 = pv.iter().sum::<f64>() / pv.len() as f64;
        println!(
            "[{label}] n={} render: mean {:.2}ms p50 {:.2} p95 {:.2} max {:.2} | present: mean {:.2}ms  ->  {:.1} fps",
            v.len(),
            mean,
            pct(0.50),
            pct(0.95),
            v[v.len() - 1],
            pmean,
            1000.0 / mean.max(1e-9),
        );
    }
}

struct App {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    ctx: RenderContext,
    resources: Resources,
    pixmap: Pixmap,
    frame: u64,
    max_frames: u64,
    stats: Stats,
    shapes: Vec<(f64, f64, f64)>, // base cx, cy, r
    saved_png: bool,
    last: Instant,
}

impl App {
    fn render(&mut self) {
        let t = self.frame as f64 / 60.0;

        // --- 1. software rasterize into a Pixmap -------------------------
        let t0 = Instant::now();
        self.ctx.reset();
        self.ctx.set_paint(BG);
        self.ctx.fill_rect(&vello_common::peniko::kurbo::Rect::new(
            0.0,
            0.0,
            WIDTH as f64,
            HEIGHT as f64,
        ));

        let palette = [BLUE, YELLOW, RED];
        let n = self.shapes.len();
        for (i, &(bx, by, r)) in self.shapes.iter().enumerate() {
            let phase = (i as f64 / n as f64) * std::f64::consts::TAU;
            let cx = bx + (t * 0.7 + phase).sin() * 40.0;
            let cy = by + (t * 1.1 + phase).cos() * 40.0;
            self.ctx.set_paint(palette[i % palette.len()]);
            self.ctx.fill_path(&blob(cx, cy, r, t * 0.5 + phase));
        }
        // A Pythagorean-style right triangle, to prove real geometry works.
        let mut tri = BezPath::new();
        tri.move_to(Point::new(140.0, 620.0));
        tri.line_to(Point::new(440.0, 620.0));
        tri.line_to(Point::new(140.0, 320.0));
        tri.close_path();
        self.ctx.set_paint(RED);
        self.ctx.fill_path(&tri);

        self.ctx.flush(); // required before render() with `multithreading`
        self.ctx
            .render(self.pixmap.as_mut(), &mut self.resources);
        let render_ms = t0.elapsed().as_secs_f64() * 1000.0;

        // --- 2. Pixmap -> PNG (P0 item 2) --------------------------------
        if !self.saved_png {
            match self.pixmap.clone().into_png() {
                Ok(bytes) => {
                    let path = "/tmp/rustim-p0-frame.png";
                    if let Err(e) = std::fs::write(path, &bytes) {
                        eprintln!("png write failed: {e}");
                    } else {
                        println!("wrote {path} ({} bytes)", bytes.len());
                    }
                }
                Err(e) => eprintln!("png encode failed: {e}"),
            }
            self.saved_png = true;
        }

        // --- 3. Pixmap -> window via softbuffer --------------------------
        let t1 = Instant::now();
        let mut buf = self.surface.buffer_mut().expect("surface buffer");
        let src = self.pixmap.data(); // premultiplied RGBA8; bg is opaque so premul == straight
        for (dst, px) in buf.iter_mut().zip(src.iter()) {
            let [r, g, b, _a] = px.to_u8_array();
            *dst = u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b);
        }
        buf.present().expect("present");
        let present_ms = t1.elapsed().as_secs_f64() * 1000.0;

        self.stats.frame_ms.push(render_ms);
        self.stats.present_ms.push(present_ms);
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {
        self.window.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                self.render();
                self.frame += 1;
                if self.frame % 60 == 0 {
                    println!(
                        "  frame {:>4}  ({:.1} ms of the last 60 as render)",
                        self.frame,
                        self.stats.frame_ms[self.stats.frame_ms.len() - 1]
                    );
                }
                if self.frame >= self.max_frames {
                    self.stats.report("final");
                    event_loop.exit();
                } else {
                    self.window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let max_frames: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);

    // A Manim-ish scene: 60 orbiting blobs + a triangle = 61 objects / 244 curves.
    let mut seed = 0xC0FF_EE00_1234_5678u64;
    let shapes: Vec<(f64, f64, f64)> = (0..60)
        .map(|_| {
            (
                100.0 + lcg(&mut seed) * (WIDTH as f64 - 200.0),
                100.0 + lcg(&mut seed) * (HEIGHT as f64 - 200.0),
                8.0 + lcg(&mut seed) * 34.0,
            )
        })
        .collect();

    let event_loop = EventLoop::new().expect("event loop");
    let window = Rc::new(
        event_loop
            .create_window(
                Window::default_attributes()
                    .with_title("rustim P0 — vello_cpu + softbuffer")
                    .with_inner_size(winit::dpi::LogicalSize::new(WIDTH, HEIGHT)),
            )
            .expect("create window"),
    );

    let surface_ctx = softbuffer::Context::new(window.clone()).expect("softbuffer context");
    let mut surface =
        softbuffer::Surface::new(&surface_ctx, window.clone()).expect("softbuffer surface");
    surface
        .resize(
            NonZeroU32::new(WIDTH as u32).unwrap(),
            NonZeroU32::new(HEIGHT as u32).unwrap(),
        )
        .expect("resize");

    let mut app = App {
        window,
        surface,
        ctx: RenderContext::new(WIDTH, HEIGHT),
        resources: Resources::new(),
        pixmap: Pixmap::new(WIDTH, HEIGHT),
        frame: 0,
        max_frames,
        stats: Stats::new(),
        shapes,
        saved_png: false,
        last: Instant::now(),
    };

    println!("running {max_frames} frames at {WIDTH}x{HEIGHT}...");
    event_loop.run_app(&mut app).expect("run_app");
    app.stats.report("wall");
}
