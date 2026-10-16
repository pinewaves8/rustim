fn main() {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::all();
    let instance = wgpu::Instance::new(desc);
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    println!("{} adapters", adapters.len());
    for (i, a) in adapters.iter().enumerate() {
        let info = a.get_info();
        println!(
            "[{i}] {:?}  name={:?} vendor=0x{:x} device=0x{:x} type={:?} driver={:?}",
            info.backend, info.name, info.vendor, info.device, info.device_type, info.driver
        );
        let l = a.limits();
        println!("      max_bind_groups={} max_texture_2d={} max_buffer_size={}",
                 l.max_bind_groups, l.max_texture_dimension_2d, l.max_buffer_size);
    }
}
