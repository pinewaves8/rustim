// P0 Spike B: can vello_gpu actually initialise AND render on this machine's
// Vulkan driver, which has no descriptor indexing?
//
// Known obstacle from the adapter probe: wgpu's `Limits::default()` exceeds this
// device's `max_compute_workgroup_size_y` (wants 256, device allows 128), so the
// device must be requested with `adapter.limits()`.

use std::time::Instant;

use vello_common::peniko::color::{AlphaColor, Srgb};
use vello_common::peniko::kurbo::{BezPath, Point};
use vello_gpu::{
    ClearSettings, RenderSize, RenderTargetConfig, Renderer, Scene, TargetInit, TextureBindings,
};

fn lcg(state: &mut u64) -> f64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    ((*state >> 11) as f64) / ((1u64 << 53) as f64)
}

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

fn main() {
    // Vulkan segfaults in submit on this driver; GL renders. Selectable so the
    // same probe can be run against both.
    let backends = match std::env::args().nth(1).as_deref() {
        Some("gl") => wgpu::Backends::GL,
        Some("vulkan") | None => wgpu::Backends::VULKAN,
        Some(other) => panic!("unknown backend {other}"),
    };
    println!("backend = {backends:?}");

    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    let instance = wgpu::Instance::new(desc);

    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    let adapter = adapters.into_iter().next().expect("no vulkan adapter");
    let info = adapter.get_info();
    println!("adapter: {} ({:?})", info.name, info.backend);

    // The fix for the earlier failure: take the device's own limits.
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("vello_gpu probe"),
        required_limits: adapter.limits(),
        ..Default::default()
    }))
    .expect("request_device with adapter limits");
    println!("device created OK");
    println!("  max_bind_groups = {}", device.limits().max_bind_groups);

    let (w, h) = (1280u16, 720u16);
    let format = wgpu::TextureFormat::Rgba8Unorm;

    let target_config = RenderTargetConfig {
        format,
        width: w,
        height: h,
    };

    let (mut renderer, mut resources) = Renderer::new(&device, &target_config);
    println!("vello_gpu::Renderer::new OK  <- the decisive question");

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
        size: wgpu::Extent3d {
            width: w as u32,
            height: h as u32,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let paint = AlphaColor::<Srgb>::new([0.39, 0.71, 0.96, 1.0]); // 3b1b blue
    let bindings = TextureBindings::new();

    eprintln!("[step] texture+view built");

    for n in [100usize, 1000, 5000] {
        eprintln!("[step] n={n}: building scene");
        let paths = make_shapes(w, h, n);
        let mut scene = Scene::new(w, h);
        eprintln!("[step] n={n}: Scene::new ok (w={w} h={h})");
        scene.set_paint(paint);
        for p in &paths {
            scene.fill_path(p);
        }
        eprintln!("[step] n={n}: fill_path ok");

        let frame = |renderer: &mut Renderer, resources: &mut vello_gpu::Resources| {
            eprintln!("[step] n={n}: encoder");
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: None,
            });
            renderer
                .render(
                    &scene,
                    resources,
                    &device,
                    &queue,
                    &mut encoder,
                    &RenderSize {
                        width: w,
                        height: h,
                    },
                    &view,
                    None,
                    &bindings,
                    TargetInit::Clear(ClearSettings::default()),
                )
                .expect("vello_gpu render");
            eprintln!("[step] n={n}: render() returned");
            let cmd = encoder.finish();
            eprintln!("[step] n={n}: encoder.finish ok");
            queue.submit([cmd]);
            eprintln!("[step] n={n}: submitted");
        };

        frame(&mut renderer, &mut resources); // warm up (pipeline creation)
        device.poll(wgpu::PollType::wait_indefinitely()).ok();

        let reps = if n >= 5000 { 5 } else { 20 };
        let t = Instant::now();
        for _ in 0..reps {
            frame(&mut renderer, &mut resources);
        }
        device.poll(wgpu::PollType::wait_indefinitely()).ok();
        let secs = t.elapsed().as_secs_f64() / reps as f64;

        println!(
            "  {:>5} shapes ({:>6} curves): {:>8.3} ms/frame  ->  {:>7.1} fps",
            n,
            n * 4,
            secs * 1000.0,
            1.0 / secs
        );
    }
}
