use std::{alloc::Layout, rc::Rc, thread::JoinHandle};

use crate::{menu::{envy::NvnBackend, shaders::{PerDrawCBuffer, PerViewCBuffer, VertexPipeline}}, nvn::{self, abstraction::{BufferVec, ManagedCommandBuffer, SwapChain}, DisplayHandle, LayerHandle, WindowHandle, PAGE_ALIGNMENT}};

mod envy;
mod font;
mod shaders;

unsafe extern "C" {
    #[link_name = "_ZN2nv20SetGraphicsAllocatorEPFPvmmS0_EPFvS0_S0_EPFS0_S0_mS0_ES0_"]
    unsafe fn nv_set_graphics_alloc(
        alloc: unsafe extern "C" fn(usize, usize, *mut ()) -> *mut u8,
        free: unsafe extern "C" fn(*mut u8, *mut ()),
        realloc: unsafe extern "C" fn(*mut u8, usize, *mut ()) -> *mut u8,
        user_data: *mut ()
    );

    #[link_name = "_ZN2nv28SetGraphicsDevtoolsAllocatorEPFPvmmS0_EPFvS0_S0_EPFS0_S0_mS0_ES0_"]
    unsafe fn nv_set_graphics_devtools_alloc(
        alloc: unsafe extern "C" fn(usize, usize, *mut ()) -> *mut u8,
        free: unsafe extern "C" fn(*mut u8, *mut ()),
        realloc: unsafe extern "C" fn(*mut u8, usize, *mut ()) -> *mut u8,
        user_data: *mut ()
    );

    #[link_name = "_ZN2nv18InitializeGraphicsEPvm"]
    unsafe fn nv_init_graphics(memory: *mut u8, memory_size: usize);

    #[link_name = "_ZN2nn2vi10InitializeEv"]
    unsafe fn nn_vi_init();
    
    #[link_name = "_ZN2nn2vi8FinalizeEv"]
    unsafe fn nn_vi_fini();

    #[link_name = "_ZN2nn2vi18OpenDefaultDisplayEPPNS0_7DisplayE"]
    unsafe fn nn_vi_open_default_display(handle: &mut DisplayHandle) -> u32;

    #[link_name = "_ZN2nn2vi12CloseDisplayEPNS0_7DisplayE"]
    unsafe fn nn_vi_close_display(handle: DisplayHandle);

    #[link_name = "_ZN2nn2vi11CreateLayerEPPNS0_5LayerEPNS0_7DisplayE"]
    unsafe fn nn_vi_create_layer(out_handle: &mut LayerHandle, display: DisplayHandle) -> u32;

    #[link_name = "_ZN2nn2vi12DestroyLayerEPNS0_5LayerE"]
    unsafe fn nn_vi_destroy_layer(layer: LayerHandle);

    #[link_name = "_ZN2nn2vi15GetNativeWindowEPPvPNS0_5LayerE"]
    unsafe fn nn_vi_get_native_window(out_handle: &mut WindowHandle, layer: LayerHandle) -> u32;

    #[link_name = "_ZN2nn2oe17FinishStartupLogoEv"]
    unsafe fn nn_oe_finish_startup();
}

#[skyline::from_offset(0x37fceb0)]
extern "C" fn smash_gfx_alloc(size: usize, align: usize, user_data: *mut ()) -> *mut u8;

#[skyline::from_offset(0x37fcf70)]
extern "C" fn smash_gfx_free(ptr: *mut u8, user_data: *mut ());

#[skyline::from_offset(0x37fcfb0)]
extern "C" fn smash_gfx_realloc(ptr: *mut u8, new_size: usize, user_data: *mut ()) -> *mut u8;

#[skyline::hook(replace = nv_set_graphics_alloc)]
fn set_graphics_alloc_stub() {}

#[skyline::hook(replace = nv_set_graphics_devtools_alloc)]
fn set_graphics_devtools_alloc_stub() {}

#[skyline::hook(replace = nv_init_graphics)]
fn init_graphics_stub() {}

fn alloc_aligned_buffer(size: usize, align: usize) -> Box<[u8]> {
    let layout = Layout::from_size_align(size, align).unwrap();
    unsafe {
        Box::from_raw(std::slice::from_raw_parts_mut(
            std::alloc::alloc(layout),
            layout.size(),
        ))
    }
}

static mut SHOULD_SHUT_DOWN: bool = false;
static mut PROMISED_HANDLE: Option<JoinHandle<()>> = None;

#[skyline::hook(offset = 0x37fa140, inline)]
unsafe fn wait_for_graphics(ctx: &skyline::hooks::InlineCtx) {
    crate::initial_loading(ctx);
    SHOULD_SHUT_DOWN = true;
    PROMISED_HANDLE.take().unwrap().join();
}

pub fn init_menu() {
    // Initialize NV
    unsafe {
        nv_set_graphics_alloc(smash_gfx_alloc, smash_gfx_free, smash_gfx_realloc, std::ptr::null_mut());
        nv_set_graphics_devtools_alloc(smash_gfx_alloc, smash_gfx_free, smash_gfx_realloc, std::ptr::null_mut());
        let mem_base = skyline::hooks::getRegionAddress(skyline::hooks::Region::Text).cast::<u8>().add(0x5940000);
        let memory_size = 0x1400000usize;
        nv_init_graphics(mem_base, memory_size);

        // Stub the initialization functions since this can only be initialized once
        skyline::install_hooks!(set_graphics_alloc_stub, set_graphics_devtools_alloc_stub, init_graphics_stub, wait_for_graphics);
    }

    let handle = std::thread::spawn(move || {
        let mut display = DisplayHandle::new();
        let mut layer = LayerHandle::new();
        let mut window = WindowHandle::new();
        // Initialize nn::vi
        unsafe {
            nn_vi_init();
            assert_eq!(nn_vi_open_default_display(&mut display), 0x0);
            assert_eq!(nn_vi_create_layer(&mut layer, display), 0x0);
            assert_eq!(nn_vi_get_native_window(&mut window, layer), 0x0);
        }

        // Initialize nvn API
        unsafe {
            nvn::init_api();
        }

        let mut device = Rc::new(nvn::Device::zeroed());
        let mut builder = nvn::DeviceBuilder::zeroed();
        builder.set_defaults();
        assert!(Rc::get_mut(&mut device).unwrap().initialize(&builder));

        // Reinitialize the API with the nvn device
        unsafe {
            nvn::init_api_with_device(&device);
        }

        let mut queue = Box::new(nvn::Queue::zeroed());
        let mut builder = nvn::QueueBuilder::zeroed();
        builder.set_device(&device);
        builder.set_defaults();
        builder.set_compute_memory_size(0);

        let command_memory_size = device.get_int(nvn::DeviceInfo::MinQueueCommandMemorySize) as usize;
        builder.set_command_memory_size(command_memory_size);
        builder.set_command_flush_threshold(command_memory_size);

        let queue_memory_size = builder.get_queue_memory_size();

        let mut queue_command_memory = alloc_aligned_buffer(queue_memory_size, PAGE_ALIGNMENT);
        builder.set_queue_memory(queue_command_memory.as_mut_ptr(), queue_memory_size);

        assert!(queue.initialize(&builder));

        let mut cmdbuf_sync = Box::new(nvn::Sync::zeroed());
        assert!(cmdbuf_sync.initialize(&device));

        {
            let mut swapchain = SwapChain::new(&device, window);
            let mut cmdbuf = ManagedCommandBuffer::new(&device, 0x10000, 0x10000);

            unsafe { nn_oe_finish_startup() };

            let mut backend = NvnBackend::new(device.clone());

            let layout = std::fs::read("sd:/menu.envy").unwrap();
            let mut layout = ::envy::asset::deserialize(&mut backend, &layout);

            layout.setup(&mut backend);

            // let vertex_pipeline = VertexPipeline::new(&device);
            // let mut vertex_buffer = BufferVec::new(&device);
            // let mut view_uniform_buffer = BufferVec::new(&device);
            // view_uniform_buffer.push(PerViewCBuffer {
            //     view_projection_matrix: glam::Mat4::orthographic_lh(0.0, 1920.0, 1080.0, 0.0, 0.0, 1.0),
            //     view_matrix: glam::Mat4::from_translation(glam::Vec3::new(0.0, 0.0, 0.0)),
            //     padding: [0u8; 0x80]
            // });
            // let mut draw_uniform_buffer = BufferVec::new(&device);
            // draw_uniform_buffer.push(PerDrawCBuffer {
            //     world_matrix: glam::Mat4::IDENTITY,
            //     base_color: glam::Vec4::ONE,
            //     world_inverse_matrix: glam::Mat4::IDENTITY,
            //     padding: [0u8; 0x70]
            // });

            loop {
                let texture_index = swapchain.acquire();
                queue.fence_sync(&mut cmdbuf_sync, 0, 0);
                queue.flush();

                layout.update();
                layout.propagate();
                layout.prepare(&mut backend);

                let stage = backend.stage();

                cmdbuf_sync.wait(u64::MAX);

                stage.exec();

                if unsafe { SHOULD_SHUT_DOWN } {
                    swapchain.await_texture();
                    break;
                }

                let handle = cmdbuf.record(|cmdbuf| {
                    cmdbuf.set_render_targets(1, &swapchain.get_texture(texture_index).unwrap(), std::ptr::null(), None, None);
                    cmdbuf.set_viewport(0, 0, 1920, 1080);
                    cmdbuf.set_scissor(0, 0, 1920, 1080);
                    cmdbuf.clear_color(0, [1.0, 0.0, 0.5, 1.0].as_ptr(), 0xf);
                    backend.prepare_render(cmdbuf);
                    layout.render(&backend, cmdbuf);
                });

                swapchain.await_texture();
                queue.submit(&[handle]);
                swapchain.present(&mut queue);
            }
        }

        cmdbuf_sync.finalize();
        queue.finish();
        queue.finalize();
        drop(queue_command_memory);
        // Serves two purposes: panics if something is still holding the device so we don't get a
        // random crash, and also finalizes the device
        Rc::get_mut(&mut device).unwrap().finalize();

        // Finalize nn::vi, leave NV initialized (we stub where the game initializes NV)
        unsafe {
            nn_vi_destroy_layer(layer);
            nn_vi_close_display(display);
            nn_vi_fini();
        }
    });

    unsafe { PROMISED_HANDLE = Some(handle); }
}
