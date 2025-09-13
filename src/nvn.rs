use std::{ops::{Deref, DerefMut}, ptr::NonNull};

pub mod abstraction;

macro_rules! opaque_ptr {
    ($v:vis $name:ident) => {
        #[repr(transparent)]
        #[derive(Copy, Clone, PartialEq, Eq, Debug)]
        $v struct $name(*mut ());

        impl $name {
            $v const fn new() -> Self {
                Self(std::ptr::null_mut())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Pointer for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Pointer::fmt(&self.0, f)
            }
        }
    }
}

opaque_ptr!(pub DisplayHandle);
opaque_ptr!(pub LayerHandle);
opaque_ptr!(pub WindowHandle);

pub const PAGE_ALIGNMENT: usize = 0x1000;
pub const fn align_up(size: usize, alignment: usize) -> usize {
    debug_assert!(alignment.is_power_of_two());
    (size + (alignment - 1)) & !(alignment - 1)
}

unsafe extern "C" {
    #[link_name = "nvnBootstrapLoader"]
    unsafe fn nvn_bootstrap_loader(ptr: *const i8) -> Option<NonNull<()>>;
}

macro_rules! decl_api {
    (
        $(
            $outer_v:vis $name:ident(align=$align:expr,size=$size:expr) {
                $(
                    $v:vis $fn_name:ident($($arg_name:ident: $arg_ty:ty),*) $(-> $ret_ty:ty)? $(| $t:tt)?;
                )*
            }
        )*
    ) => {
        paste::paste! {
            $(
                struct [<$name Procs>] {
                    $(
                        [<$fn_name:snake>]: extern "C" fn(& $($t)? $name, $($arg_ty),*) $(-> $ret_ty)?,
                    )*
                }

                impl [<$name Procs>] {
                    fn load(device: *const Device, loader: extern "C" fn(*const Device, *const i8) -> Option<NonNull<()>>) -> Self {
                        Self {
                            $(
                                [<$fn_name:snake>]: unsafe {
                                    log::info!(concat!("Loading nvn", stringify!($name), stringify!($fn_name)));
                                    std::mem::transmute(loader(device, concat!("nvn", stringify!($name), stringify!($fn_name), "\0").as_ptr().cast()).unwrap())
                                },
                            )*
                        }
                    }
                }

                #[repr(align($align), C)]
                $outer_v struct $name([u8; $size]);

                impl std::fmt::Debug for &$name {
                    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str(concat!(stringify!($name), "("))?;
                        std::fmt::Pointer::fmt(&(*self as *const $name), f)?;
                        f.write_str(")")
                    }
                }

                impl std::fmt::Debug for &mut $name {
                    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str(concat!(stringify!($name), "("))?;
                        std::fmt::Pointer::fmt(&(*self as *const $name), f)?;
                        f.write_str(")")
                    }
                }

                impl $name {
                    pub const fn zeroed() -> Self {
                        Self([0u8; $size])
                    }

                    $(
                        #[allow(static_mut_refs)]
                        $v fn [<$fn_name:snake>](& $($t)? self, $($arg_name: $arg_ty),*) $(-> $ret_ty)? {
                            log::trace!(
                                concat!(
                                    "[INVOKE] nvn", stringify!($name), stringify!($fn_name), "(",
                                    "{:#x?}",
                                    $(
                                        concat!(", ", stringify!($arg_name), ": {:#x?}"),
                                    )*
                                    ")",
                                ),
                                self,
                                $($arg_name),*
                            );

                            let out = (NvnProcs::get().[<$name:snake>].[<$fn_name:snake>])(self, $($arg_name),*);
                            $(
                                let _: std::marker::PhantomData<$ret_ty> = std::marker::PhantomData;
                                log::trace!(
                                    concat!(
                                        "[RETURN] nvn", stringify!($name), stringify!($fn_name), " -> {:#x?}",
                                    ),
                                    out
                                );
                            )?
                            out
                        }
                    )*
                }
            )*

            static mut NVN_PROCS: Option<NvnProcs> = None;

            struct NvnProcs {
                $(
                    [<$name:snake>]: [<$name Procs>],
                )*
            }

            impl NvnProcs {
                fn get() -> &'static Self {
                   if cfg!(debug_assertions) {
                        unsafe { NVN_PROCS.as_ref().unwrap() }
                    } else {
                        unsafe {
                            NVN_PROCS.as_ref().unwrap_unchecked()
                        }
                    }
                }

                fn load() -> Self {
                    let get_proc_address: extern "C" fn(*const Device, *const i8) -> Option<NonNull<()>> = unsafe {
                        std::mem::transmute(nvn_bootstrap_loader(c"nvnDeviceGetProcAddress".as_ptr()).unwrap())
                    };

                    Self {
                        $(
                            [<$name:snake>]: [<$name Procs>]::load(std::ptr::null(), get_proc_address),
                        )*
                    }
                }

                fn load_with_device(device: &Device) -> Self {
                    let mut get_proc_address: extern "C" fn(*const Device, *const i8) -> Option<NonNull<()>> = unsafe {
                        std::mem::transmute(nvn_bootstrap_loader(c"nvnDeviceGetProcAddress".as_ptr()).unwrap())
                    };

                    get_proc_address = unsafe {
                        std::mem::transmute(get_proc_address(device, c"nvnDeviceGetProcAddress".as_ptr()).unwrap())
                    };

                    Self {
                        $(
                            [<$name:snake>]: [<$name Procs>]::load(device, get_proc_address),
                        )*
                    }
                }
            }
        }
    }
}

#[repr(transparent)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct TextureHandle(u64);

#[repr(transparent)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct CommandHandle(u64);

#[repr(transparent)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BufferAddress(u64);

impl BufferAddress {
    pub fn offset(&self, amount: usize) -> Self {
        Self(self.0 + amount as u64)
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ShaderData {
    pub code: BufferAddress,
    pub control: *const u8,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct CopyRegion {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub w: i32,
    pub h: i32,
    pub d: i32,
}

impl CopyRegion {
    pub fn from_size_2d(width: i32, height: i32) -> Self {
        Self {
            x: 0,
            y: 0,
            z: 0,
            w: width,
            h: height,
            d: 1,
        }
    }
}

#[repr(i32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum DeviceInfo {
    BufferAlignment = 4,
    TextureDescriptorSize = 15,
    SamplerDescriptorSize = 16,
    ReservedTextureDescriptors = 17,
    ReservedSamplerDescriptors = 18,
    CmdbufCommandAlignment = 19,
    CmdbufControlAlignment = 20,
    ShaderPadding = 78,
    MinQueueCommandMemorySize = 81,
}

#[repr(u32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Format {
    Rg32 = 0x16,
    Rgb32 = 0x22,
    Rgba8 = 0x25,
    Rgba32 = 0x2E,
    Rgba8Srgb = 0x38,
}

#[repr(u32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum TextureTarget {
    D1 = 0x0,
    D2 = 0x1,
    D3 = 0x2,
}

bitflags::bitflags! {
    #[repr(C)]
    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    pub struct MemoryPoolFlags : u32 {
        const CPU_NO_ACCESS = 1 << 0;
        const CPU_UNCACHED = 1 << 1;
        const CPU_CACHED = 1 << 2;
        const GPU_NO_ACCESS = 1 << 3;
        const GPU_UNCACHED = 1 << 4;
        const GPU_CACHED = 1 << 5;
        const SHADER_CODE = 1 << 6;
        const COMPRESSIBLE = 1 << 7;
        const PHYSICAL = 1 << 8;
        const VIRTUAL = 1 << 9;

        /// [`Self::CPU_UNCACHED`] and [`Self::GPU_CACHED`]
        const STANDARD = (1 << 1) | (1 << 5);
    }
}

decl_api! {
    pub DeviceBuilder(align=8, size=0x40) {
        pub SetDefaults() | mut;
        pub SetFlags(flags: u32) | mut;
    }

    pub Device(align=8, size=0x3000) {
        pub Initialize(builder: &DeviceBuilder) -> bool | mut;
        pub Finalize() | mut;
        GetInteger(integer: DeviceInfo, out: &mut i32);
        pub GetTextureHandle(texture_id: i32, sampler_id: i32) -> TextureHandle;
    }

    pub QueueBuilder(align=8, size=0x40) {
        pub SetDevice(device: &Device) | mut;
        pub SetDefaults() | mut;
        pub SetComputeMemorySize(size: usize) | mut;
        pub SetCommandMemorySize(size: usize) | mut;
        pub SetCommandFlushThreshold(size: usize) | mut;
        pub SetQueueMemory(memory: *mut u8, size: usize) | mut;
        pub GetQueueMemorySize() -> usize;
    }

    pub Queue(align=8, size=0x2000) {
        pub Initialize(builder: &QueueBuilder) -> bool | mut;
        pub Finish() | mut;
        pub Finalize() | mut;
        pub Flush() | mut;
        SubmitCommands(count: i32, handles: *const CommandHandle) | mut;
        pub FenceSync(sync: &mut Sync, condition: u32, flags: u32)| mut;
        pub PresentTexture(window: &mut Window, texture: i32) | mut;
    }

    pub WindowBuilder(align=8, size=0x40) {
        pub SetDefaults() | mut;
        pub SetDevice(device: &Device) | mut;
        pub SetNativeWindow(window: WindowHandle) | mut;
        SetTextures(count: i32, textures: *const *const Texture) | mut;
    }

    pub Window(align=8, size=0x180) {
        pub Initialize(builder: &WindowBuilder) -> bool | mut;
        pub Finalize() | mut;
        pub AcquireTexture(sync: &mut Sync, current_texture: &mut i32) -> u32 | mut;
    }

    pub Sync(align=8, size=0x40) {
        pub Initialize(device: &Device) -> bool | mut;
        pub Finalize() | mut;
        pub Wait(timeout_ns: u64);
    }

    pub MemoryPoolBuilder(align=8, size=0x40) {
        pub SetDefaults() | mut;
        pub SetDevice(device: &Device) | mut;
        pub SetFlags(flags: MemoryPoolFlags) | mut;
        pub SetStorage(memory: *mut u8, size: usize);
    }

    pub MemoryPool(align=8, size=0x100) {
        pub Initialize(builder: &MemoryPoolBuilder) -> bool | mut;
        pub Finalize() | mut;
    }

    pub TexturePool(align=8, size=0x20) {
        pub Initialize(mempool: &MemoryPool, offset: isize, num: usize) -> bool | mut;
        pub Finalize() | mut;
        pub RegisterTexture(id: i32, texture: &Texture, view: Option<&TextureView>) | mut;
    }

    pub SamplerPool(align=8, size=0x20) {
        pub Initialize(mempool: &MemoryPool, offset: isize, num: usize) -> bool | mut;
        pub Finalize() | mut;
        pub RegisterSampler(id: i32, sampler: &Sampler) | mut;
    }

    pub SamplerBuilder(align=8, size=0x60) {
        pub SetDefaults() | mut;
        pub SetDevice(device: &Device) | mut;
        pub SetMinMagFilter(min: u32, mag: u32) | mut;
    }

    pub Sampler(align=8, size=0x60) {
        pub Initialize(builder: &SamplerBuilder) -> bool | mut;
        pub Finalize() | mut;
    }

    pub TextureBuilder(align=8, size=0x80) {
        pub SetDefaults() | mut;
        pub SetDevice(device: &Device) | mut;
        pub SetFlags(flags: u32) | mut;
        pub SetTarget(target: TextureTarget) | mut;
        pub SetFormat(format: Format) | mut;
        pub SetSize2D(width: i32, height: i32) | mut;
        pub SetStorage(pool: &MemoryPool, offset: isize) | mut;
        pub GetStorageSize() -> usize;
        pub GetStorageAlignment() -> usize;
    }

    pub Texture(align=8, size=0xC0) {
        pub Initialize(builder: &TextureBuilder) -> bool | mut;
        pub Finalize() | mut;
        pub WriteTexels(view: Option<&TextureView>, region: &CopyRegion, data: *const u8) | mut;
        pub FlushTexels(view: Option<&TextureView>, region: &CopyRegion) | mut;
    }

    pub TextureView(align=0x8, size=0x28) {
    }

    pub CommandBuffer(align=8, size=0xA0) {
        pub Initialize(device: &Device) -> bool | mut;
        pub Finalize() | mut;
        pub AddCommandMemory(pool: &MemoryPool, offset: isize, size: usize) | mut;
        pub AddControlMemory(memory: *mut u8, size: usize) | mut;
        pub BeginRecording() | mut;
        pub SetScissor(x: i32, y: i32, width: i32, height: i32) | mut;
        pub SetViewport(x: i32, y: i32, width: i32, height: i32) | mut;
        pub ClearColor(render_target_id: i32, color: *const f32, mask: u32);
        pub SetRenderTargets(count: i32, colors: *const &Texture, color_views: *const Option<&TextureView>, depth: Option<&Texture>, depth_view: Option<&TextureView>);

        pub BindBlendState(blend_state: &BlendState) | mut;
        pub BindColorState(color_state: &ColorState) | mut;
        pub BindDepthStencilState(depth_stencil_state: &DepthStencilState) | mut;
        pub BindMultisampleState(multisample_state: &MultisampleState) | mut;
        pub BindPolygonState(polygon_state: &PolygonState) | mut;
        pub BindChannelMaskState(channel_mask_state: &ChannelMaskState) | mut;
        pub BindVertexStreamState(count: i32, states: *const VertexStreamState) | mut;
        pub BindVertexAttribState(count: i32, states: *const VertexAttribState) | mut;
        pub BindVertexBuffer(idx: i32, address: BufferAddress, size: usize) | mut;
        pub BindProgram(program: &Program, stages: i32) | mut;
        pub BindTexture(stage: u32, idx: i32, handle: TextureHandle) | mut;
        pub BindUniformBuffer(stage: u32, idx: i32, address: BufferAddress, size: usize) | mut;
        pub SetTexturePool(pool: &TexturePool) | mut;
        pub SetSamplerPool(pool: &SamplerPool) | mut;
        pub DrawArrays(prim: u32, first: i32, count: i32) | mut;
        pub DrawElements(prim: u32, index_kind: u32, count: i32, handle: BufferAddress);
        pub DrawElementsInstanced(prim: u32, index_ty: u32, count: u32, index_buffer: BufferAddress, base_vertex: i32, base_instance: i32, instance_count: i32) | mut;

        pub EndRecording() -> CommandHandle | mut;
    }

    pub Program(align=8, size=0xC0) {
        pub Initialize(device: &Device) -> bool | mut;
        pub Finalize() | mut;
        pub SetShaders(count: i32, shader_data: *const ShaderData) -> bool | mut;
    }

    pub Buffer(align=8, size=0x30) {
        pub Initialize(builder: &BufferBuilder) -> bool | mut;
        pub Finalize() | mut;
        pub GetAddress() -> BufferAddress;
        pub Map() -> *mut u8;
        pub FlushMappedRange(offset: isize, size: usize);
    }

    pub BufferBuilder(align=8, size=0x40) {
        pub SetDefaults() | mut;
        pub SetDevice(device: &Device) | mut;
        pub SetStorage(pool: &MemoryPool, offset: isize, size: usize) | mut;
    }

    pub BlendState(align=8, size=8) {
        pub SetDefaults() | mut;
        pub SetBlendTarget(target: i32) | mut;
        pub SetBlendFunc(src: u32, dst: u32, src_alpha: u32, dst_alpha: u32) | mut;
        pub SetBlendEquation(color: u32, alpha: u32) | mut;
    }

    pub ColorState(align=4, size=4) {
        pub SetDefaults() | mut;
        pub SetBlendEnable(idx: i32, enable: bool) | mut;
        pub SetLogicOp(op: i32) | mut;
    }

    pub ChannelMaskState(align=4, size=4) {
        pub SetDefaults() | mut;
    }

    pub PolygonState(align=4, size=4) {
        pub SetDefaults() | mut;
        pub SetCullFace(face: u32) | mut;
        pub SetFrontFace(face: u32) | mut;
        pub SetPolygonMode(mode: u32) | mut;
    }

    pub MultisampleState(align=8, size=0x18) {
        pub SetDefaults() | mut;
    }

    pub DepthStencilState(align=8, size=8) {
        pub SetDefaults() | mut;
    }

    pub VertexAttribState(align=4, size=4) {
        pub SetDefaults() | mut;
        pub SetFormat(format: Format, offset: isize) | mut;
        pub SetStreamIndex(index: u32) | mut;
    }

    pub VertexStreamState(align=8, size=8) {
        pub SetDefaults() | mut;
        pub SetStride(stride: isize) | mut;
        pub SetDivisor(divisor: i32) | mut;
    }
}

impl Device {
    pub fn get_int(&self, what: DeviceInfo) -> i32 {
        let mut out = 0i32;
        self.get_integer(what, &mut out);
        out
    }
}

impl Queue {
    pub fn submit(&mut self, commands: &[CommandHandle]) {
        let length = if cfg!(debug_assertions) {
            commands.len().try_into().unwrap()
        } else {
            commands.len() as i32
        };
        self.submit_commands(length, commands.as_ptr())
    }
}

pub struct WindowBuilderGuard<'a> {
    builder: &'a mut WindowBuilder,
}

impl Deref for WindowBuilderGuard<'_> {
    type Target = WindowBuilder;

    fn deref(&self) -> &Self::Target {
        self.builder
    }
}

impl DerefMut for WindowBuilderGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.builder
    }
}

impl WindowBuilder {
    /// SAFETY: The references to textures passed into this function must be alive
    ///     at least until this builder is dropped (or is at least done building windows)
    pub unsafe fn set_render_textures(&mut self, textures: &[&Texture]) {
        let length = if cfg!(debug_assertions) {
            textures.len().try_into().unwrap()
        } else {
            textures.len() as i32
        };

        self.set_textures(length, textures.as_ptr().cast());
    }
}

/// SAFETY: This function must not be called while any other threads are making NVN API calls
pub unsafe fn init_api() {
    NVN_PROCS = Some(NvnProcs::load());
}

/// SAFETY:
/// - This function must not be called while any other threads are making NVN API calls
/// - [`fini_api`] must be called immediately after `device` is finalized
/// - [`fini_api`] must not be called before `device` is finalized
pub unsafe fn init_api_with_device(device: &Device) {
    NVN_PROCS = Some(NvnProcs::load_with_device(device));
}

/// SAFETY:
/// - This function must not be called while any other threads are making NVN API calls
pub unsafe fn fini_api() {
    NVN_PROCS = None;
}
