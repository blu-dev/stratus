use std::{
    alloc::Layout,
    collections::HashMap,
    ops::{Index, IndexMut},
    ptr::NonNull,
    rc::Rc,
    sync::Arc,
};

use bytemuck::{Pod, Zeroable};
use glam::UVec2;

use crate::nvn::{self, align_up, ShaderData, PAGE_ALIGNMENT};

pub struct ManagedMemoryPool {
    inner: Box<nvn::MemoryPool>,
    raw_ptr: NonNull<u8>,
    memory_layout: Layout,
}

impl ManagedMemoryPool {
    #[track_caller]
    pub fn new(
        device: &nvn::Device,
        flags: nvn::MemoryPoolFlags,
        size: usize,
        alignment: impl Into<Option<usize>>,
    ) -> Self {
        Self::new_with_staging(device, flags, size, alignment, |_| {})
    }

    #[track_caller]
    pub fn new_with_staging(
        device: &nvn::Device,
        flags: nvn::MemoryPoolFlags,
        size: usize,
        alignment: impl Into<Option<usize>>,
        f: impl FnOnce(&mut [u8]),
    ) -> Self {
        let mut builder = nvn::MemoryPoolBuilder::zeroed();
        builder.set_defaults();
        builder.set_device(device);
        builder.set_flags(flags);

        let alignment: Option<usize> = alignment.into();
        let alignment = alignment.unwrap_or(PAGE_ALIGNMENT);
        assert!(alignment != 0 && alignment.is_power_of_two());

        let real_size = align_up(size, PAGE_ALIGNMENT);

        let layout = Layout::from_size_align(real_size, alignment.max(PAGE_ALIGNMENT)).unwrap();
        let ptr = unsafe { std::alloc::alloc(layout) };
        if ptr.is_null() {
            panic!("Failed to allocate memory for ManagedMemoryPool");
        }

        let slice = unsafe { std::slice::from_raw_parts_mut(ptr, size) };

        f(slice);

        builder.set_storage(ptr, layout.size());

        let mut inner = Box::new(nvn::MemoryPool::zeroed());

        assert!(
            inner.initialize(&builder),
            "{:#x} {:#x}",
            layout.size(),
            layout.align()
        );

        Self {
            inner,
            raw_ptr: unsafe { NonNull::new_unchecked(ptr) },
            memory_layout: layout,
        }
    }

    pub fn get(&self) -> &nvn::MemoryPool {
        &self.inner
    }

    pub fn total_size(&self) -> usize {
        self.memory_layout.size()
    }
}

impl Drop for ManagedMemoryPool {
    fn drop(&mut self) {
        self.inner.finalize();
        unsafe { std::alloc::dealloc(self.raw_ptr.as_ptr(), self.memory_layout) }
    }
}

const SWAPCHAIN_TEXTURE_COUNT: usize = 3;

pub struct SwapChain {
    texture_memory: ManagedMemoryPool,
    textures: [Box<nvn::Texture>; SWAPCHAIN_TEXTURE_COUNT],
    window: Box<nvn::Window>,
    sync: Box<nvn::Sync>,
    current_texture: i32,
}

impl SwapChain {
    pub fn new(device: &nvn::Device, window: nvn::WindowHandle) -> Self {
        let mut builder = nvn::TextureBuilder::zeroed();
        builder.set_defaults();
        builder.set_device(device);
        builder.set_flags(0x9); // 0x8 - compressible, 0x1 display
        builder.set_target(nvn::TextureTarget::D2);
        builder.set_format(nvn::Format::Rgba8);
        // builder.set_samples(4);
        builder.set_size2_d(1920, 1080);

        let size = builder.get_storage_size();
        let align = builder.get_storage_alignment();

        let target_stride = align_up(size, align);

        let offset_0 = 0usize;
        let offset_1 = offset_0 + target_stride;
        let offset_2 = offset_1 + target_stride;
        let total_size = offset_2 + target_stride;

        let memory = ManagedMemoryPool::new(
            device,
            nvn::MemoryPoolFlags::COMPRESSIBLE
                | nvn::MemoryPoolFlags::GPU_CACHED
                | nvn::MemoryPoolFlags::CPU_UNCACHED,
            total_size,
            align,
        );

        let mut textures = [
            Box::new(nvn::Texture::zeroed()),
            Box::new(nvn::Texture::zeroed()),
            Box::new(nvn::Texture::zeroed()),
        ];
        builder.set_storage(memory.get(), offset_0 as isize);
        assert!(textures[0].initialize(&builder));
        builder.set_storage(memory.get(), offset_1 as isize);
        assert!(textures[1].initialize(&builder));
        builder.set_storage(memory.get(), offset_2 as isize);
        assert!(textures[2].initialize(&builder));

        let mut builder = nvn::WindowBuilder::zeroed();
        builder.set_defaults();
        builder.set_device(device);
        builder.set_native_window(window);
        let texture_refs = [&*textures[0], &*textures[1], &*textures[2]];
        unsafe {
            builder.set_render_textures(&texture_refs);
        }

        let mut window = Box::new(nvn::Window::zeroed());
        assert!(window.initialize(&builder));

        let mut sync = Box::new(nvn::Sync::zeroed());
        assert!(sync.initialize(device));

        Self {
            texture_memory: memory,
            textures,
            window,
            sync,
            current_texture: -1i32,
        }
    }

    pub fn acquire(&mut self) -> usize {
        assert_eq!(
            self.window
                .acquire_texture(&mut self.sync, &mut self.current_texture),
            0,
            "nvnWindowAcquireTexture failed"
        );
        self.current_texture as usize
    }

    pub fn get_texture(&self, idx: usize) -> Option<&nvn::Texture> {
        self.textures.get(idx).map(|v| &**v)
    }

    pub fn await_texture(&mut self, queue: &mut nvn::Queue) {
        queue.wait_sync(&mut self.sync);
    }

    pub fn present(&mut self, queue: &mut nvn::Queue) {
        queue.present_texture(&mut self.window, self.current_texture)
    }
}

impl Drop for SwapChain {
    fn drop(&mut self) {
        self.sync.finalize();
        self.window.finalize();
        for texture in self.textures.iter_mut() {
            texture.finalize();
        }
    }
}

pub struct ManagedCommandBuffer {
    cmdbuf: Box<nvn::CommandBuffer>,
    memory: ManagedMemoryPool,
    command_size: usize,
    control: NonNull<u8>,
    control_layout: Layout,
}

impl ManagedCommandBuffer {
    pub fn new(device: &nvn::Device, command_size: usize, control_size: usize) -> Self {
        let command_alignment = device.get_int(nvn::DeviceInfo::CmdbufCommandAlignment) as usize;
        let control_alignment = device.get_int(nvn::DeviceInfo::CmdbufControlAlignment) as usize;

        let memory = ManagedMemoryPool::new(
            device,
            nvn::MemoryPoolFlags::CPU_UNCACHED | nvn::MemoryPoolFlags::GPU_CACHED,
            command_size,
            command_alignment,
        );

        let control_layout = Layout::from_size_align(control_size, control_alignment).unwrap();
        let control = unsafe { std::alloc::alloc(control_layout) };
        assert!(!control.is_null());

        let mut cmdbuf = Box::new(nvn::CommandBuffer::zeroed());
        assert!(cmdbuf.initialize(device));

        Self {
            cmdbuf,
            memory,
            command_size,
            control: unsafe { NonNull::new_unchecked(control) },
            control_layout,
        }
    }

    pub fn record(&mut self, f: impl FnOnce(&mut nvn::CommandBuffer)) -> nvn::CommandHandle {
        self.cmdbuf
            .add_command_memory(self.memory.get(), 0, self.command_size);
        self.cmdbuf
            .add_control_memory(self.control.as_ptr(), self.control_layout.size());
        self.cmdbuf.begin_recording();
        f(&mut self.cmdbuf);
        self.cmdbuf.end_recording()
    }
}

impl Drop for ManagedCommandBuffer {
    fn drop(&mut self) {
        self.cmdbuf.finalize();
        unsafe { std::alloc::dealloc(self.control.as_ptr(), self.control_layout) }
    }
}

pub struct ManagedProgram {
    program: Box<nvn::Program>,
    buffers: [Box<nvn::Buffer>; 2],
    shader_data: Box<[ShaderData; 2]>,
    memory: ManagedMemoryPool,
}

impl ManagedProgram {
    pub fn new(
        device: &nvn::Device,
        vctrl: *const u8,
        vcode: &'static [u8],
        fctrl: *const u8,
        fcode: &'static [u8],
    ) -> Self {
        let alignment = device.get_int(nvn::DeviceInfo::BufferAlignment) as usize;
        let padding = device.get_int(nvn::DeviceInfo::ShaderPadding) as usize;

        let v_offset = 0;
        let f_offset = align_up(vcode.len(), alignment);
        let total_size = align_up(f_offset + fcode.len(), alignment) + padding;

        let memory = ManagedMemoryPool::new_with_staging(
            device,
            nvn::MemoryPoolFlags::SHADER_CODE
                | nvn::MemoryPoolFlags::CPU_NO_ACCESS
                | nvn::MemoryPoolFlags::GPU_CACHED,
            total_size,
            alignment,
            |buffer| {
                buffer[v_offset..v_offset + vcode.len()].copy_from_slice(vcode);
                buffer[f_offset..f_offset + fcode.len()].copy_from_slice(fcode);
            },
        );

        let mut builder = nvn::BufferBuilder::zeroed();
        builder.set_defaults();
        builder.set_device(device);

        let mut buffers = [
            Box::new(nvn::Buffer::zeroed()),
            Box::new(nvn::Buffer::zeroed()),
        ];

        builder.set_storage(memory.get(), v_offset as isize, vcode.len());
        assert!(buffers[0].initialize(&builder));

        builder.set_storage(memory.get(), f_offset as isize, fcode.len());
        assert!(buffers[1].initialize(&builder));

        let mut program = Box::new(nvn::Program::zeroed());
        assert!(program.initialize(device));

        let shader_data = Box::new([
            ShaderData {
                code: buffers[0].get_address(),
                control: vctrl,
            },
            ShaderData {
                code: buffers[1].get_address(),
                control: fctrl,
            },
        ]);

        assert!(program.set_shaders(2, shader_data.as_ptr()));

        Self {
            program,
            buffers,
            shader_data,
            memory,
        }
    }

    pub fn get(&self) -> &nvn::Program {
        &self.program
    }
}

impl Drop for ManagedProgram {
    fn drop(&mut self) {
        self.program.finalize();
        self.buffers[0].finalize();
        self.buffers[1].finalize();
    }
}

enum StagedBufferInner<'a> {
    NoAction,
    Flush {
        memory: &'a [u8],
        buffer_to_write: &'a mut nvn::Buffer,
    },
}

pub struct StagedBuffer<'a>(StagedBufferInner<'a>);

impl StagedBuffer<'_> {
    pub fn execute(self) {
        match self.0 {
            StagedBufferInner::NoAction => {}
            StagedBufferInner::Flush {
                memory,
                buffer_to_write,
            } => {
                let ptr = buffer_to_write.map();
                unsafe {
                    skyline::libc::memcpy(ptr.cast(), memory.as_ptr().cast(), memory.len());
                }
                buffer_to_write.flush_mapped_range(0, memory.len());
            }
        }
    }
}

pub struct BufferVec<T: Pod> {
    memory: ManagedMemoryPool,
    buffer: Box<nvn::Buffer>,
    cpu: Vec<T>,
    prune_cache: Vec<(usize, (ManagedMemoryPool, Box<nvn::Buffer>))>,
    changed: bool,
}

impl<T: Pod> BufferVec<T> {
    fn new_buffer_with_size(
        device: &nvn::Device,
        size: usize,
    ) -> (ManagedMemoryPool, Box<nvn::Buffer>) {
        let size = size.max(1);
        let memory = ManagedMemoryPool::new(
            device,
            nvn::MemoryPoolFlags::CPU_UNCACHED | nvn::MemoryPoolFlags::GPU_CACHED,
            size,
            std::mem::align_of::<T>(),
        );

        let mut builder = nvn::BufferBuilder::zeroed();
        builder.set_defaults();
        builder.set_device(device);
        builder.set_storage(memory.get(), 0, size);
        let mut buffer = Box::new(nvn::Buffer::zeroed());
        assert!(buffer.initialize(&builder));

        (memory, buffer)
    }

    pub fn new(device: &nvn::Device) -> Self {
        Self::with_capacity(device, 0)
    }

    pub fn with_capacity(device: &nvn::Device, capacity: usize) -> Self {
        let total_required_size = std::mem::size_of::<T>() * capacity;

        let (memory, buffer) = Self::new_buffer_with_size(device, total_required_size);

        Self {
            memory,
            buffer,
            cpu: Vec::with_capacity(capacity),
            prune_cache: vec![],
            changed: false,
        }
    }

    pub fn len(&self) -> usize {
        self.cpu.len()
    }

    pub fn truncate(&mut self, new_size: usize) {
        self.cpu.truncate(new_size);
        self.changed = true;
    }

    pub fn push(&mut self, value: T) {
        self.cpu.push(value);
        self.changed = true;
    }

    pub fn extend(&mut self, values: impl IntoIterator<Item = T>) {
        self.cpu.extend(values);
        self.changed = true;
    }

    pub fn stage<'a>(&'a mut self, device: &nvn::Device) -> StagedBuffer<'a> {
        if !self.changed {
            return StagedBuffer(StagedBufferInner::NoAction);
        }

        self.changed = false;

        let mut idx = 0;
        while idx < self.prune_cache.len() {
            let (count, _) = &mut self.prune_cache[idx];
            *count = count.saturating_sub(1);
            if *count == 0 {
                let (_, (pool, mut buffer)) = self.prune_cache.remove(idx);
                buffer.finalize();
                drop(pool);
            } else {
                idx += 1;
            }
        }

        let needed_length = std::mem::size_of::<T>() * self.cpu.len();
        if needed_length > self.memory.total_size() {
            let new_capacity = needed_length.max(self.memory.total_size() * 2);

            let (new_memory, new_buffer) = Self::new_buffer_with_size(device, new_capacity);
            // MAGIC NUMBER: 3 is just the number of stage calls needed before we prune this guy
            self.prune_cache.push((
                3,
                (
                    std::mem::replace(&mut self.memory, new_memory),
                    std::mem::replace(&mut self.buffer, new_buffer),
                ),
            ));
        }

        StagedBuffer(StagedBufferInner::Flush {
            memory: bytemuck::cast_slice(&self.cpu),
            buffer_to_write: &mut self.buffer,
        })
    }

    pub fn address_for_element(&self, index: usize) -> nvn::BufferAddress {
        assert!(index < self.len());
        nvn::BufferAddress(
            self.buffer().get_address().0 + (std::mem::size_of::<T>() * index) as u64,
        )
    }

    pub fn buffer(&self) -> &nvn::Buffer {
        &self.buffer
    }
}

impl<T: Pod + Zeroable, I> Index<I> for BufferVec<T>
where
    Vec<T>: Index<I>,
{
    type Output = <Vec<T> as Index<I>>::Output;

    fn index(&self, index: I) -> &Self::Output {
        &self.cpu[index]
    }
}

impl<T: Pod + Zeroable, I> IndexMut<I> for BufferVec<T>
where
    Vec<T>: IndexMut<I>,
{
    fn index_mut(&mut self, index: I) -> &mut Self::Output {
        self.changed = true;
        &mut self.cpu[index]
    }
}

#[derive(Copy, Clone)]
pub struct TextureInfo {
    pub size: UVec2,
    pub handle: nvn::TextureHandle,
}

struct LoadedTexture {
    memory: ManagedMemoryPool,
    texture: Box<nvn::Texture>,
    size: UVec2,
    texture_id: i32,
}

impl Drop for LoadedTexture {
    fn drop(&mut self) {
        self.texture.finalize();
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct ImageSampler {
    pub wrap_mode_x: nvn::WrapMode,
    pub wrap_mode_y: nvn::WrapMode,
}

struct ManagedTexture {
    sampler: Box<nvn::Sampler>,
    info: TextureInfo,
}

pub struct ManagedImages {
    device: Rc<nvn::Device>,
    loaded_textures: HashMap<String, LoadedTexture>,
    texture_pool: Box<nvn::TexturePool>,
    sampler_pool: Box<nvn::SamplerPool>,
    descriptor_pool: ManagedMemoryPool,
    textures: HashMap<(String, ImageSampler), ManagedTexture>,

    texture_current: i32,
    sampler_current: i32,
    texture_max: i32,
    sampler_max: i32,
}

impl ManagedImages {
    pub fn new(device: Rc<nvn::Device>, max_images: usize) -> Self {
        assert!(max_images > 0);

        let tex_desc_size = device.get_int(nvn::DeviceInfo::TextureDescriptorSize) as usize;
        let sam_desc_size = device.get_int(nvn::DeviceInfo::SamplerDescriptorSize) as usize;
        let reserved_tex_desc =
            device.get_int(nvn::DeviceInfo::ReservedTextureDescriptors) as usize;
        let reserved_sam_desc =
            device.get_int(nvn::DeviceInfo::ReservedSamplerDescriptors) as usize;

        let texture_size = tex_desc_size * (reserved_tex_desc + max_images);
        let sampler_size = sam_desc_size * (reserved_sam_desc + max_images);

        let descriptor_pool = ManagedMemoryPool::new(
            &device,
            nvn::MemoryPoolFlags::CPU_UNCACHED | nvn::MemoryPoolFlags::GPU_CACHED,
            texture_size + sampler_size,
            PAGE_ALIGNMENT,
        );
        let mut texture_pool = Box::new(nvn::TexturePool::zeroed());
        assert!(texture_pool.initialize(descriptor_pool.get(), 0, reserved_tex_desc + max_images));

        let mut sampler_pool = Box::new(nvn::SamplerPool::zeroed());
        assert!(sampler_pool.initialize(
            descriptor_pool.get(),
            texture_size as isize,
            reserved_sam_desc + max_images
        ));

        Self {
            device: device.clone(),
            loaded_textures: HashMap::new(),
            texture_pool,
            sampler_pool,
            descriptor_pool,
            textures: HashMap::new(),
            texture_current: reserved_tex_desc as i32,
            sampler_current: reserved_sam_desc as i32,
            texture_max: (reserved_tex_desc + max_images) as i32,
            sampler_max: (reserved_sam_desc + max_images) as i32,
        }
    }

    pub fn request_texture(&mut self, name: &str, sampler: ImageSampler) -> Option<TextureInfo> {
        let key = (name.to_string(), sampler);
        if let Some(info) = self.textures.get(&key) {
            return Some(info.info);
        }

        let texture = self.loaded_textures.get(name)?;

        let mut builder = nvn::SamplerBuilder::zeroed();
        builder.set_defaults();
        builder.set_device(&self.device);
        builder.set_wrap_mode(
            sampler.wrap_mode_x,
            sampler.wrap_mode_y,
            nvn::WrapMode::Clamp,
        );
        let mut sampler = Box::new(nvn::Sampler::zeroed());
        assert!(sampler.initialize(&builder));

        self.sampler_pool
            .register_sampler(self.sampler_current, &sampler);
        let info = TextureInfo {
            size: texture.size,
            handle: self
                .device
                .get_texture_handle(texture.texture_id, self.sampler_current),
        };
        self.textures.insert(key, ManagedTexture { sampler, info });

        self.sampler_current += 1;

        Some(info)
    }

    pub fn get_texture(&self, name: impl AsRef<str>) -> Option<&nvn::Texture> {
        self.loaded_textures.get(name.as_ref()).map(|s| &*s.texture)
    }

    pub fn new_multisampled_render_target(&mut self, name: impl AsRef<str>, target_size: glam::UVec2) -> TextureInfo {
        let name = name.as_ref();

        let mut builder = nvn::TextureBuilder::zeroed();
        builder.set_defaults();
        builder.set_device(&self.device);
        builder.set_target(nvn::TextureTarget::D2);
        builder.set_format(nvn::Format::Rgba8Srgb);
        builder.set_size2_d(target_size.x as i32 * 2, target_size.y as i32 * 2);

        let size = builder.get_storage_size();
        let align = builder.get_storage_alignment();

        let target_stride = align_up(size, align);

        let memory = ManagedMemoryPool::new(
            &self.device,
            nvn::MemoryPoolFlags::COMPRESSIBLE
                | nvn::MemoryPoolFlags::GPU_CACHED
                | nvn::MemoryPoolFlags::CPU_UNCACHED,
            target_stride,
            align,
        );

        builder.set_storage(memory.get(), 0);
        let mut multisample_target = Box::new(nvn::Texture::zeroed());
        assert!(multisample_target.initialize(&builder));


        let mut builder = nvn::SamplerBuilder::zeroed();
        builder.set_defaults();
        builder.set_device(&self.device);
        let mut sampler = Box::new(nvn::Sampler::zeroed());
        assert!(sampler.initialize(&builder));

        self.texture_pool.register_texture(self.texture_current, &multisample_target, None);
        self.sampler_pool
            .register_sampler(self.sampler_current, &sampler);

        self.loaded_textures.insert(name.to_string(), LoadedTexture {
            memory,
            texture: multisample_target,
            size: target_size,
            texture_id: self.texture_current
        });

        let info = TextureInfo {
            size: target_size,
            handle: self
                .device
                .get_texture_handle(self.texture_current, self.sampler_current),
        };

        self.texture_current += 1;
        self.sampler_current += 1;

        info

    }

    pub fn load_texture(&mut self, device: &nvn::Device, name: impl Into<String>, bytes: &[u8]) {
        assert!(self.sampler_current < self.sampler_max && self.texture_current < self.texture_max);

        let name: String = name.into();

        assert!(!self.loaded_textures.contains_key(&name));

        let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();

        let mut builder = nvn::TextureBuilder::zeroed();
        builder.set_defaults();
        builder.set_device(device);
        builder.set_format(nvn::Format::Rgba8);
        builder.set_size2_d(image.width() as i32, image.height() as i32);
        let size = builder.get_storage_size();
        let align = builder.get_storage_alignment();

        let memory = ManagedMemoryPool::new(
            device,
            nvn::MemoryPoolFlags::GPU_CACHED | nvn::MemoryPoolFlags::CPU_UNCACHED,
            size,
            align,
        );
        builder.set_storage(memory.get(), 0);
        let mut texture = Box::new(nvn::Texture::zeroed());
        assert!(texture.initialize(&builder));

        let region = nvn::CopyRegion::from_size_2d(image.width() as i32, image.height() as i32);
        texture.write_texels(None, &region, image.as_raw().as_ptr());
        texture.flush_texels(None, &region);

        self.texture_pool
            .register_texture(self.texture_current, &texture, None);
        self.loaded_textures.insert(
            name,
            LoadedTexture {
                texture,
                memory,
                size: image.dimensions().into(),
                texture_id: self.texture_current,
            },
        );
        self.texture_current += 1;
    }

    pub fn texpool(&self) -> &nvn::TexturePool {
        &self.texture_pool
    }

    pub fn sampool(&self) -> &nvn::SamplerPool {
        &self.sampler_pool
    }
}
