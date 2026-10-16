// Control experiment for P0 Spike B.
//
// vello_gpu::Renderer::new() and render() both succeed on the Phytium Vulkan
// driver, but `queue.submit` segfaults. Before blaming vello_gpu we must know
// whether this driver can render *anything* at all.
//
// Stage 1: encoder.clear_texture + submit + readback.    (no pipeline at all)
// Stage 2: a minimal WGSL render pipeline, 0 bind groups, 3-vertex draw.
//
// Usage: minimal [vulkan|gl] [clear|pipeline]

use std::sync::mpsc;

fn main() {
    let backend_name = std::env::args().nth(1).unwrap_or_else(|| "vulkan".into());
    let stage = std::env::args().nth(2).unwrap_or_else(|| "clear".into());

    let backends = match backend_name.as_str() {
        "vulkan" => wgpu::Backends::VULKAN,
        "gl" => wgpu::Backends::GL,
        other => panic!("unknown backend {other}"),
    };

    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    let instance = wgpu::Instance::new(desc);

    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    let adapter = adapters.into_iter().next().expect("no adapter");
    let info = adapter.get_info();
    println!("adapter: {} ({:?}) driver={}", info.name, info.backend, info.driver);

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: None,
        required_limits: adapter.limits(),
        ..Default::default()
    }))
    .expect("request_device");
    println!("device ok, max_bind_groups={}", device.limits().max_bind_groups);

    // Surface uncaptured errors instead of aborting, so we see validation issues.
    device.on_uncaptured_error(std::sync::Arc::new(|e| {
        eprintln!("!! uncaptured wgpu error: {e}");
    }));

    let (w, h) = (64u32, 64u32);
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
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

    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (w * h * 4) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

    match stage.as_str() {
        "clear" => {
            println!("stage: clear_texture only (no pipeline)");
            encoder.clear_texture(
                &texture,
                &wgpu::ImageSubresourceRange {
                    aspect: wgpu::TextureAspect::All,
                    base_mip_level: 0,
                    mip_level_count: None,
                    base_array_layer: 0,
                    array_layer_count: None,
                },
            );
        }
        "pipeline" => {
            println!("stage: minimal render pipeline, 0 bind groups");
            const WGSL: &str = r#"
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) col: vec3<f32> };
@vertex
fn vs(@builtin(vertex_index) i: u32) -> VOut {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-0.8, -0.8), vec2<f32>(0.8, -0.8), vec2<f32>(0.0, 0.8));
    var o: VOut;
    o.pos = vec4<f32>(p[i], 0.0, 1.0);
    o.col = vec3<f32>(0.2, 0.9, 0.4);
    return o;
}
@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.col, 1.0);
}
"#;
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("tri"),
                source: wgpu::ShaderSource::Wgsl(WGSL.into()),
            });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[],
                immediate_size: 0,
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("tri"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });
            println!("  pipeline created");
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rp"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.1,
                            g: 0.1,
                            b: 0.1,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rp.set_pipeline(&pipeline);
            rp.draw(0..3, 0..1);
            drop(rp);
            println!("  draw recorded");
        }
        other => panic!("unknown stage {other}"),
    }

    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );

    let cmd = encoder.finish();
    println!("encoder.finish ok, submitting...");
    queue.submit([cmd]);
    println!("submitted. polling...");
    device.poll(wgpu::PollType::wait_indefinitely()).ok();
    println!("poll ok. mapping...");

    let slice = readback.slice(..);
    let (tx, rx) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).ok();
    match rx.recv() {
        Ok(Ok(())) => {
            let data = slice.get_mapped_range().expect("get_mapped_range");
            let px = ((h / 2 * w + w / 2) * 4) as usize;
            println!("center pixel = {:?}", &data[px..px + 4]);
        }
        other => println!("map_async -> {other:?}"),
    }

    println!("DONE");
}
