use std::{alloc::Layout, cell::RefCell, collections::VecDeque, rc::Rc, thread::JoinHandle, time::Instant};

use ::envy::{LayoutTree, Node, NodeDisjointAccessor, NodeUpdateCallback, NodeVisibility, SublayoutNode, TextNode};
use hound::WavReader;
use ninput::Buttons;

use crate::{
    audio::LoopingAudio, menu::{
        envy::{NvnBackend, NvnBackendStage},
        shaders::{PerDrawCBuffer, PerViewCBuffer, VertexPipeline},
    }, nvn::{
        self, DisplayHandle, LayerHandle, PAGE_ALIGNMENT, WindowHandle, abstraction::{BufferVec, ManagedCommandBuffer, ManagedMemoryPool, SwapChain}, align_up
    }
};

mod envy;
mod font;
mod shaders;

unsafe extern "C" {
    #[link_name = "_ZN2nv20SetGraphicsAllocatorEPFPvmmS0_EPFvS0_S0_EPFS0_S0_mS0_ES0_"]
    unsafe fn nv_set_graphics_alloc(
        alloc: unsafe extern "C" fn(usize, usize, *mut ()) -> *mut u8,
        free: unsafe extern "C" fn(*mut u8, *mut ()),
        realloc: unsafe extern "C" fn(*mut u8, usize, *mut ()) -> *mut u8,
        user_data: *mut (),
    );

    #[link_name = "_ZN2nv28SetGraphicsDevtoolsAllocatorEPFPvmmS0_EPFvS0_S0_EPFS0_S0_mS0_ES0_"]
    unsafe fn nv_set_graphics_devtools_alloc(
        alloc: unsafe extern "C" fn(usize, usize, *mut ()) -> *mut u8,
        free: unsafe extern "C" fn(*mut u8, *mut ()),
        realloc: unsafe extern "C" fn(*mut u8, usize, *mut ()) -> *mut u8,
        user_data: *mut (),
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
static mut PROMISED_HANDLE: Option<Box<skyline::nn::os::ThreadType>> = None;

#[skyline::hook(offset = 0x37fa140, inline)]
unsafe fn wait_for_graphics(ctx: &skyline::hooks::InlineCtx) {
    crate::initial_loading(ctx);
    // SHOULD_SHUT_DOWN = true;
    let mut thread = PROMISED_HANDLE.take().unwrap();
    skyline::nn::os::WaitThread(&mut *thread);
    skyline::nn::os::DestroyThread(&mut *thread);
}

#[derive(Debug, Copy, Clone)]
enum RootEvent {
    ShowMainMenu,
    ShowMods,
    ShowSettings,
    ShowUpdate,
    Play,
    Quit,
}

enum MainMenuButtonEvent {
    Select,
    Unselect,
}

struct MainMenuButton {
    controller: Rc<RefCell<VirtualController>>,
    prev: LocalChannel<MainMenuButtonEvent>,
    this: LocalChannel<MainMenuButtonEvent>,
    next: LocalChannel<MainMenuButtonEvent>,
    root_channel: LocalChannel<RootEvent>,
    root_event: RootEvent,
    scene: Rc<RefCell<MenuScene>>,
    is_selected: bool,
    is_scene_default: bool,
    was_disabled: bool,
}

impl NodeUpdateCallback<NvnBackend> for MainMenuButton {
    fn update(&mut self, node: NodeDisjointAccessor<NvnBackend>) {
        if *self.scene.borrow() != MenuScene::MainMenu {
            self.was_disabled = true;
            return;
        }

        if self.was_disabled && self.is_scene_default {
            self.this.send(MainMenuButtonEvent::Select);
        }

        self.was_disabled = false;

        let controller = self.controller.borrow();
        if self.is_selected {
            if controller.down() {
                self.next.send(MainMenuButtonEvent::Select);
                self.this.send(MainMenuButtonEvent::Unselect);
            } else if controller.up() {
                self.prev.send(MainMenuButtonEvent::Select);
                self.this.send(MainMenuButtonEvent::Unselect);
            } else if controller.select() {
                self.this.send(MainMenuButtonEvent::Unselect);
                self.root_channel.send(self.root_event);
            }
        }

        let mut sublayout = node.self_mut();
        let sublayout = sublayout.downcast_mut::<SublayoutNode<NvnBackend>>().unwrap().as_layout_mut();
        match self.this.recv() {
            Some(MainMenuButtonEvent::Select) => {
                self.is_selected = true;
                sublayout.get_node_by_path_mut("select").unwrap().set_visibility(NodeVisibility::Visible);
                sublayout.get_node_by_path_mut("unselect").unwrap().set_visibility(NodeVisibility::Hidden);
            },
            Some(MainMenuButtonEvent::Unselect) => {
                self.is_selected = false;
                sublayout.get_node_by_path_mut("select").unwrap().set_visibility(NodeVisibility::Hidden);
                sublayout.get_node_by_path_mut("unselect").unwrap().set_visibility(NodeVisibility::Visible);
            },
            None => {}
        }

    }
}

struct VirtualController(Box<[ninput::Controller]>);

impl VirtualController {
    fn new() -> Self {
        let controllers = [
            ninput::Controller::new(0x20),
            ninput::Controller::new(0x10),
            ninput::Controller::new(0),
            ninput::Controller::new(1),
            ninput::Controller::new(2),
            ninput::Controller::new(3),
            ninput::Controller::new(4),
            ninput::Controller::new(5),
            ninput::Controller::new(6),
            ninput::Controller::new(7),
        ];

        Self(Box::new(controllers))
    }

    fn update(&mut self) {
        self.0.iter_mut().for_each(|controller| controller.update());
    }

    fn up(&self) -> bool {
        self.0.iter().any(|controller| controller.pressed_buttons.intersects(Buttons::up()))
    }

    fn down(&self) -> bool {
        self.0.iter().any(|controller| controller.pressed_buttons.intersects(Buttons::down()))
    }

    fn right(&self) -> bool {
        self.0.iter().any(|controller| controller.pressed_buttons.intersects(Buttons::right()))
    }

    fn left(&self) -> bool {
        self.0.iter().any(|controller| controller.pressed_buttons.intersects(Buttons::left()))
    }

    fn select(&self) -> bool {
        self.0.iter().any(|controller| controller.pressed_buttons.intersects(Buttons::A))
    }

    fn cancel(&self) -> bool {
        self.0.iter().any(|controller| controller.pressed_buttons.intersects(Buttons::B))
    }

    fn shoulder_r(&self) -> bool {
        self.0.iter().any(|controller| controller.pressed_buttons.intersects(Buttons::R))
    }

    fn shoulder_l(&self) -> bool {
        self.0.iter().any(|controller| controller.pressed_buttons.intersects(Buttons::L))
    }

    fn shoulder_r_down(&self) -> bool {
        self.0.iter().any(|controller| controller.buttons.intersects(Buttons::R))
    }

    fn shoulder_l_down(&self) -> bool {
        self.0.iter().any(|controller| controller.buttons.intersects(Buttons::L))
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum MenuScene {
    MainMenu,
    Mods,
    Settings,
    Update,
}

fn init_main_menu(
    root: &mut LayoutTree<NvnBackend>,
    root_channel: LocalChannel<RootEvent>,
    scene: Rc<RefCell<MenuScene>>,
    controller: Rc<RefCell<VirtualController>>
) {
    let mm_buttons = root
        .get_node_by_path_mut("Stratus/Main Menu/mm_btns")
        .unwrap();

    let mm_button_names = [
        "main_menu_btn_play",
        "main_menu_btn_mods",
        "main_menu_btn_settings",
        "main_menu_btn_update",
        "main_menu_btn_quit",
    ];
    let mm_button_labels = ["Play", "Mods", "Settings", "Update", "Quit"];
    let mm_marks = ["category/icon/mark_play", "category/icon/mark_mods", "category/icon/mark_settings", "category/icon/mark_update", "category/icon/mark_quit"];
    let mm_channels = [LocalChannel::new(), LocalChannel::new(), LocalChannel::new(), LocalChannel::new(), LocalChannel::new()];
    let mm_events = [RootEvent::Play, RootEvent::ShowMods, RootEvent::ShowSettings, RootEvent::ShowUpdate, RootEvent::Quit];

    for idx in 0..5 {
        let prev = mm_channels[(idx + 4) % 5].clone();
        let this = mm_channels[idx].clone();
        let next = mm_channels[(idx + 1) % 5].clone();
        let node = mm_buttons.child_mut(mm_button_names[idx]);
        node.add_on_update(MainMenuButton {
            controller: controller.clone(),
            prev,
            this,
            next,
            root_channel: root_channel.clone(),
            root_event: mm_events[idx],
            scene: scene.clone(),
            is_selected: false,
            is_scene_default: idx == 0,
            was_disabled: false,
        });
        let layout = node
            .as_sublayout_mut()
            .as_layout_mut();
        layout
            .get_node_by_path_mut("select")
            .unwrap()
            .set_visibility(NodeVisibility::Hidden);
        layout
            .get_node_by_path_mut("unselect")
            .unwrap()
            .set_visibility(NodeVisibility::Visible);
        layout
            .get_node_by_path_mut("category/txt_label")
            .unwrap()
            .as_text_mut()
            .set_text(mm_button_labels[idx]);
        layout.get_node_by_path_mut(mm_marks[idx]).unwrap().set_visibility(NodeVisibility::Inherited);
    }

    mm_channels[0].send(MainMenuButtonEvent::Select);

    let controller = controller.clone();
    root.get_node_by_path_mut("Stratus/Main Menu/stratus_icon").unwrap().add_on_update(move |node: NodeDisjointAccessor<NvnBackend>| {
        if *scene.borrow() != MenuScene::MainMenu {
            return;
        }

        let controller = controller.borrow();
        let mut node = node.self_mut();
        if controller.shoulder_r_down() {
            node.transform_mut().angle += 3.0;
            node.mark_changed();
        }

        if controller.shoulder_l_down() {
            node.transform_mut().angle -= 3.0;
            node.mark_changed();
        }
    });
}

struct ModListEntry {
    name: String,
    is_enabled: bool,
    is_zip_file: bool,
    preview: (),
    authors: Vec<String>,
    version: Option<String>,
    description: Option<String>,
}

impl ModListEntry {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            is_enabled: true,
            is_zip_file: false,
            preview: (),
            authors: vec![],
            version: None,
            description: None,
        }
    }

    fn zip(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            is_enabled: true,
            is_zip_file: true,
            preview: (),
            authors: vec![],
            version: None,
            description: None,
        }
    }
}

struct ModsList {
    controller: Rc<RefCell<VirtualController>>,
    scene: Rc<RefCell<MenuScene>>,
    root: LocalChannel<RootEvent>,
    entries: Vec<ModListEntry>,
    current_local: usize,
    current_page: usize,
    was_disabled_last: bool,
}

impl NodeUpdateCallback<NvnBackend> for ModsList {
    fn update(&mut self, node: NodeDisjointAccessor<'_, NvnBackend>) {
        if *self.scene.borrow() != MenuScene::Mods {
            self.was_disabled_last = true;
            return;
        }

        if self.controller.borrow().cancel() {
            self.root.send(RootEvent::ShowMainMenu);
            return;
        }

        let child_names = [
            "mod_btn_01",
            "mod_btn_02",
            "mod_btn_03",
            "mod_btn_04",
            "mod_btn_05",
            "mod_btn_06",
        ];

        let page_count = self.entries.len() / 6 + 1;

        if self.was_disabled_last {
            self.current_local = 0;
            self.current_page = 0;
            {
                let mut sibling = node.sibling_mut("mod_page_btn_list").unwrap();

                sibling
                        .downcast_mut::<SublayoutNode<NvnBackend>>()
                        .unwrap()
                        .as_layout_mut()
                        .get_node_by_path_mut("mod_page_bg/mod_page_txt")
                        .unwrap()
                        .as_text_mut()
                        .set_text(format!("Mods (1/{page_count})"));
                sibling.mark_changed();
            }

            for (idx, name) in child_names.into_iter().enumerate() {
                let mut child = node.child_mut(name).unwrap();
                if idx >= self.entries.len() {
                    child.set_visibility(NodeVisibility::Hidden);
                } else {
                    child.set_visibility(NodeVisibility::Inherited);
                    let layout = child.downcast_mut::<SublayoutNode<NvnBackend>>().unwrap();
                    let layout = layout.as_layout_mut();
                    layout.get_node_by_path_mut("select/btn_item_decide").unwrap().set_visibility(NodeVisibility::Hidden);
                    layout.get_node_by_path_mut("unselect").unwrap().set_visibility(NodeVisibility::Inherited);
                    layout.get_node_by_path_mut("mod_txt_name").unwrap().as_text_mut().set_text(&self.entries[idx].name);
                    layout.play_animation_looping("select");
                }

                child.mark_changed();
            }
        }

        let controller = self.controller.borrow();

        let mut new_page = self.current_page;
        let mut new_local = self.current_local;

        if controller.down() {
            if self.current_local == 5 || (self.current_page == page_count - 1 && self.current_local == (self.entries.len() - self.current_page * 6 - 1)) {
                new_local = 0;
                new_page = (self.current_page + 1) % page_count;
            } else {
                new_page = self.current_page;
                new_local = self.current_local + 1;
            }
        } else if controller.up() {
            if self.current_local == 0 {
                new_page = (self.current_page + page_count - 1) % page_count;

                if new_page == page_count - 1 {
                    new_local = (self.entries.len() - new_page * 6) - 1;
                } else {
                    new_local = 5;
                }
            } else {
                new_page = self.current_page;
                new_local = self.current_local - 1;
            }
        } else if controller.shoulder_r() {
            if self.current_page == page_count - 1 {
                new_page = 0;
            } else {
                new_page = self.current_page + 1;
            }
            new_local = self.current_local.min(self.entries.len() - new_page * 6 - 1);
        } else if controller.shoulder_l() {
            if self.current_page == 0 {
                new_page = page_count - 1;
            } else {
                new_page = self.current_page - 1;
            }
            new_local = self.current_local.min(self.entries.len() - new_page * 6 - 1);
        };

        if new_local != self.current_local {
            let mut old = node.child_mut(child_names[self.current_local]).unwrap();
            let layout = old.downcast_mut::<SublayoutNode<NvnBackend>>().unwrap();
            let layout = layout.as_layout_mut();
            layout.get_node_by_path_mut("select/btn_item_decide").unwrap().set_visibility(NodeVisibility::Hidden);
            layout.get_node_by_path_mut("unselect").unwrap().set_visibility(NodeVisibility::Inherited);
        }

        if new_page != self.current_page {
            let page_offset = new_page * 6;

            {
                let mut sibling = node.sibling_mut("mod_page_btn_list").unwrap();

                sibling
                        .downcast_mut::<SublayoutNode<NvnBackend>>()
                        .unwrap()
                        .as_layout_mut()
                        .get_node_by_path_mut("mod_page_bg/mod_page_txt")
                        .unwrap()
                        .as_text_mut()
                        .set_text(format!("Mods ({}/{page_count})", new_page + 1));
                sibling.mark_changed();
            }

            for (idx, name) in child_names.into_iter().enumerate() {
                let mut child = node.child_mut(name).unwrap();
                let idx = idx + page_offset;
                if idx >= self.entries.len() {
                    child.set_visibility(NodeVisibility::Hidden);
                } else {
                    child.set_visibility(NodeVisibility::Inherited);
                    let layout = child.downcast_mut::<SublayoutNode<NvnBackend>>().unwrap();
                    let layout = layout.as_layout_mut();
                    layout.get_node_by_path_mut("select/btn_item_decide").unwrap().set_visibility(NodeVisibility::Hidden);
                    layout.get_node_by_path_mut("unselect").unwrap().set_visibility(NodeVisibility::Inherited);
                    layout.get_node_by_path_mut("mod_txt_name").unwrap().as_text_mut().set_text(&self.entries[idx].name);
                    layout.get_node_by_path_mut("on").unwrap().set_visibility(if self.entries[idx].is_enabled {
                        NodeVisibility::Inherited
                    } else {
                        NodeVisibility::Hidden
                    });
                }

                child.mark_changed();
            }
        }

        self.current_page = new_page;
        self.current_local = new_local;

        let entry_count = self.entries.len();
        let entry = &mut self.entries[self.current_page * 6 + self.current_local];

        {
            let mut sibling = node.sibling_mut("mod_info").unwrap();

            let layout = sibling.downcast_mut::<SublayoutNode<NvnBackend>>().unwrap().as_layout_mut();
            layout
                    .get_node_by_path_mut("Preview/mod_number_bg/num_txt")
                    .unwrap()
                    .as_text_mut()
                    .set_text(format!("{}/{}", self.current_page * 6 + self.current_local + 1, entry_count));
            layout.get_node_by_path_mut("Preview/mod_category_txt").unwrap().as_text_mut().set_text(if entry.is_zip_file { "Compressed Zip File" } else { "Mod Folder" });
            layout.get_node_by_path_mut("Preview/mod_name_txt").unwrap().as_text_mut().set_text(&entry.name);
            layout.get_node_by_path_mut("txt_info/Info").unwrap().as_text_mut().set_text(
                format!("Authors: {}\nVersion: {}", if entry.authors.is_empty() { "???".to_string() } else { entry.authors.join(", ") }, entry.version.as_deref().unwrap_or("???"))
            );
            layout.get_node_by_path_mut("txt_info/Description").unwrap().as_text_mut().set_text(entry.description.as_deref().unwrap_or(""));

            sibling.mark_changed();
        }

        if controller.select() {
            entry.is_enabled = !entry.is_enabled;
        }

        let mut showing = node.child_mut(child_names[self.current_local]).unwrap();
        let layout = showing.downcast_mut::<SublayoutNode<NvnBackend>>().unwrap();
        let layout = layout.as_layout_mut();
        layout.get_node_by_path_mut("select/btn_item_decide").unwrap().set_visibility(NodeVisibility::Inherited);
        layout.get_node_by_path_mut("unselect").unwrap().set_visibility(NodeVisibility::Hidden);
        layout.get_node_by_path_mut("on").unwrap().set_visibility(if self.entries[self.current_page * 6 + self.current_local].is_enabled {
            NodeVisibility::Inherited
        } else {
            NodeVisibility::Hidden
        });

        self.was_disabled_last = false;
    }
}

fn init_mods(
    root: &mut LayoutTree<NvnBackend>,
    channel: LocalChannel<RootEvent>,
    scene: Rc<RefCell<MenuScene>>,
    controller: Rc<RefCell<VirtualController>>
) {
    root.get_node_by_path_mut("Stratus/Mods/mod_btns").unwrap().add_on_update(ModsList {
        controller: controller.clone(),
        scene,
        root: channel,
        entries: vec![
            ModListEntry::zip("HDR-Skins"),
            ModListEntry::zip("HDR-Stages"),
            ModListEntry::zip("Ponytail Peach"),
            ModListEntry::new("Thwomp Kirby (C08)"),
            ModListEntry::new("Knuckles"),
            ModListEntry::new("Colored Turnips"),
            ModListEntry::zip("P5D Joker"),
            ModListEntry::new("Octoling (C13)"),
            ModListEntry::new("MP2 Dark Samus (C09)"),
            ModListEntry::zip("Secret Sauce"),
        ],
        current_local: 0,
        current_page: 0,
        was_disabled_last: true
    });
}

fn initialize_root(
    layout: &mut LayoutTree<NvnBackend>,
    channel: LocalChannel<RootEvent>,
    scene: Rc<RefCell<MenuScene>>,
) -> Rc<RefCell<VirtualController>> {
    let controller = Rc::new(RefCell::new(VirtualController::new()));

    // Set the version in the header
    layout.get_node_by_path_mut("Stratus/Background/top_header/ver_txt")
        .unwrap()
        .as_text_mut()
        .set_text(concat!("Version ", env!("CARGO_PKG_VERSION")));

    // Update visibility of root root nodes
    layout.get_node_by_path_mut("Stratus/Main Menu").unwrap().set_visibility(NodeVisibility::Inherited);
    layout.get_node_by_path_mut("Stratus/Settings").unwrap().set_visibility(NodeVisibility::Hidden);
    layout.get_node_by_path_mut("Stratus/Mods").unwrap().set_visibility(NodeVisibility::Hidden);
    layout.get_node_by_path_mut("Stratus/Update").unwrap().set_visibility(NodeVisibility::Hidden);

    init_main_menu(layout, channel.clone(), scene.clone(), controller.clone());
    init_mods(layout, channel.clone(), scene.clone(), controller.clone());

    // Entrance Anims
    layout.get_node_by_path_mut("Stratus/Background/bg_set").unwrap().as_sublayout_mut().as_layout_mut().play_animation("entrance");
    layout.get_node_by_path_mut("Stratus/Background/top_header").unwrap().as_sublayout_mut().as_layout_mut().play_animation("entrance");


    controller
}

struct LocalChannel<T>(Rc<RefCell<VecDeque<T>>>);

impl<T> Clone for LocalChannel<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> LocalChannel<T> {
    fn new() -> Self {
        Self(Rc::new(RefCell::new(Default::default())))
    }

    fn send(&self, event: T) {
        self.0.borrow_mut().push_back(event);
    }

    fn recv(&self) -> Option<T> {
        self.0.borrow_mut().pop_front()
    }
}

extern "C" fn menu_thread(_: *mut skyline::libc::c_void) {
    // let mut wav = WavReader::new(std::io::Cursor::new(include_bytes!("bgm.wav"))).unwrap();
    // let samples: Vec<i16> = wav.samples::<i16>().map(|x| x.unwrap()).collect();
    // let audio = LoopingAudio::new(
    //     samples,
    //     151836 * 2,
    //     1776452 * 2,
    //     0.5,
    //     0.0,
    //     30,
    //     3.0
    // );

    // audio.start();

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

        let mut builder = nvn::TextureBuilder::zeroed();
        builder.set_defaults();
        builder.set_device(&device);
        builder.set_target(nvn::TextureTarget::D2Multisample);
        builder.set_format(nvn::Format::Rgba8Srgb);
        builder.set_samples(4);
        builder.set_size2_d(1920, 1080);

        let size = builder.get_storage_size();
        let align = builder.get_storage_alignment();

        let target_stride = align_up(size, align);

        let memory = ManagedMemoryPool::new(
            &device,
            nvn::MemoryPoolFlags::COMPRESSIBLE
                | nvn::MemoryPoolFlags::GPU_CACHED
                | nvn::MemoryPoolFlags::CPU_UNCACHED,
            target_stride,
            align,
        );

        builder.set_storage(memory.get(), 0);
        let mut multisample_target = nvn::Texture::zeroed();
        assert!(multisample_target.initialize(&builder));

        let mut cmdbuf = ManagedCommandBuffer::new(&device, 0x100000, 0x100000);

        unsafe { nn_oe_finish_startup() };

        let mut backend = NvnBackend::new(device.clone());

        let layout = std::fs::read("sd:/menu.envy").unwrap();
        let mut layout = ::envy::asset::deserialize(&mut backend, &layout);

        layout.setup(&mut backend);

        // layout.as_layout_mut().get_node_by_path_mut("Stratus/Background/bg_set").unwrap().as_sublayout_mut().as_layout_mut().play_animation("Idling");

        let scene = Rc::new(RefCell::new(MenuScene::MainMenu));
        let root_channel = LocalChannel::new();
        let controller = initialize_root(layout.as_layout_mut(), root_channel.clone(), scene.clone());

        loop {
            let texture_index = swapchain.acquire();
            queue.fence_sync(&mut cmdbuf_sync, 0, 0);
            queue.flush();


            controller.borrow_mut().update();
            layout.update();

            layout.as_layout_mut().update_animations();
            layout.as_layout_mut().propagate();
            layout.prepare(&mut backend);

            while let Some(event) = root_channel.recv() {
                match event {
                    RootEvent::Play => unsafe { SHOULD_SHUT_DOWN = true },
                    RootEvent::ShowMainMenu => {
                        *scene.borrow_mut() = MenuScene::MainMenu;
                        let layout = layout.as_layout_mut();
                        layout.get_node_by_path_mut("Stratus/Main Menu").unwrap().set_visibility(NodeVisibility::Inherited);
                        layout.get_node_by_path_mut("Stratus/Mods").unwrap().set_visibility(NodeVisibility::Hidden);
                        layout.get_node_by_path_mut("Stratus/Settings").unwrap().set_visibility(NodeVisibility::Hidden);
                        layout.get_node_by_path_mut("Stratus/Update").unwrap().set_visibility(NodeVisibility::Hidden);
                    },
                    RootEvent::ShowMods => {
                        *scene.borrow_mut() = MenuScene::Mods;
                        let layout = layout.as_layout_mut();
                        layout.get_node_by_path_mut("Stratus/Main Menu").unwrap().set_visibility(NodeVisibility::Hidden);
                        layout.get_node_by_path_mut("Stratus/Mods").unwrap().set_visibility(NodeVisibility::Inherited);
                        layout.get_node_by_path_mut("Stratus/Settings").unwrap().set_visibility(NodeVisibility::Hidden);
                        layout.get_node_by_path_mut("Stratus/Update").unwrap().set_visibility(NodeVisibility::Hidden);
                    },
                    RootEvent::ShowSettings => {
                        *scene.borrow_mut() = MenuScene::Settings;
                        let layout = layout.as_layout_mut();
                        layout.get_node_by_path_mut("Stratus/Main Menu").unwrap().set_visibility(NodeVisibility::Hidden);
                        layout.get_node_by_path_mut("Stratus/Mods").unwrap().set_visibility(NodeVisibility::Hidden);
                        layout.get_node_by_path_mut("Stratus/Settings").unwrap().set_visibility(NodeVisibility::Inherited);
                        layout.get_node_by_path_mut("Stratus/Update").unwrap().set_visibility(NodeVisibility::Hidden);
                    },
                    RootEvent::ShowUpdate => {
                        *scene.borrow_mut() = MenuScene::Update;
                        let layout = layout.as_layout_mut();
                        layout.get_node_by_path_mut("Stratus/Main Menu").unwrap().set_visibility(NodeVisibility::Hidden);
                        layout.get_node_by_path_mut("Stratus/Mods").unwrap().set_visibility(NodeVisibility::Hidden);
                        layout.get_node_by_path_mut("Stratus/Settings").unwrap().set_visibility(NodeVisibility::Hidden);
                        layout.get_node_by_path_mut("Stratus/Update").unwrap().set_visibility(NodeVisibility::Inherited);
                    },
                    RootEvent::Quit => {
                        unsafe { skyline::nn::oe::ExitApplication() }
                    }
                }
            }

            let stage = backend.stage();

            cmdbuf_sync.wait(u64::MAX);

            stage.exec();

            if unsafe { SHOULD_SHUT_DOWN } {
                swapchain.await_texture(&mut queue);
                break;
            }

            let handle = cmdbuf.record(|cmdbuf| {
                cmdbuf.set_render_targets(1, &&multisample_target, std::ptr::null(), None, None);
                cmdbuf.set_viewport(0, 0, 1920, 1080);
                cmdbuf.set_scissor(0, 0, 1920, 1080);
                cmdbuf.clear_color(0, [1.0, 0.0, 0.0, 1.0].as_ptr(), 0xf);
                backend.prepare_render(cmdbuf);
                layout.render(&backend, cmdbuf);
                cmdbuf.set_render_targets(1, &swapchain.get_texture(texture_index).unwrap(), std::ptr::null(), None, None);
                cmdbuf.set_viewport(0, 0, 1920, 1080);
                cmdbuf.set_scissor(0, 0, 1920, 1080);
                cmdbuf.downsample(
                    &multisample_target,
                    swapchain.get_texture(texture_index).unwrap(),
                );
            });

            swapchain.await_texture(&mut queue);
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
}

pub fn init_menu() {
    // Initialize NV
    unsafe {
        nv_set_graphics_alloc(
            smash_gfx_alloc,
            smash_gfx_free,
            smash_gfx_realloc,
            std::ptr::null_mut(),
        );
        nv_set_graphics_devtools_alloc(
            smash_gfx_alloc,
            smash_gfx_free,
            smash_gfx_realloc,
            std::ptr::null_mut(),
        );
        let mem_base = skyline::hooks::getRegionAddress(skyline::hooks::Region::Text)
            .cast::<u8>()
            .add(0x5940000);
        let memory_size = 0x1400000usize;
        nv_init_graphics(mem_base, memory_size);

        // Stub the initialization functions since this can only be initialized once
        skyline::install_hooks!(
            set_graphics_alloc_stub,
            set_graphics_devtools_alloc_stub,
            init_graphics_stub,
            wait_for_graphics
        );
    }

    // menu_thread(std::ptr::null_mut());

    unsafe {
        let mut thread = Box::new(skyline::nn::os::ThreadType::new());
        skyline::nn::os::CreateThread1(
            &mut *thread,
            menu_thread,
            std::ptr::null_mut(),
            skyline::libc::memalign(0x1000, 0x40000).cast(),
            0x40000,
            0,
            1,
        );
        skyline::nn::os::StartThread(&mut *thread);
        PROMISED_HANDLE = Some(thread);
    }
}
