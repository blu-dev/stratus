use std::{
    fs::File,
    ops::Deref,
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
    time::Instant,
};

use camino::Utf8Path;
use skyline::hooks::InlineCtx;

use crate::{
    archive::Archive,
    data::IntoHash,
    hash_interner::{DisplayHash, HashMemorySlab, InternerCache},
};

mod archive;
mod containers;
mod data;
mod hash_interner;

const STRATUS_FOLDER: &'static str = "sd:/ultimate/stratus/";

fn init_folder() {
    let path = Utf8Path::new(STRATUS_FOLDER);
    if path.exists() {
        if path.is_file() {
            panic!("stratus folder is a file and not a folder");
        }
        return;
    }

    let _ = std::fs::create_dir_all(STRATUS_FOLDER);

    assert!(
        path.exists(),
        "stratus folder does not exist after attempting to create it"
    );
}

struct ReadOnlyHashMemorySlab(HashMemorySlab);

impl Deref for ReadOnlyHashMemorySlab {
    type Target = HashMemorySlab;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

unsafe impl Send for ReadOnlyHashMemorySlab {}
unsafe impl Sync for ReadOnlyHashMemorySlab {}

static HASHES: OnceLock<ReadOnlyHashMemorySlab> = OnceLock::new();

trait HashDisplay {
    fn display(self) -> DisplayHash<'static>;
}

impl HashDisplay for smash_hash::Hash40 {
    fn display(self) -> DisplayHash<'static> {
        DisplayHash {
            slab: HASHES.get().expect("HashMemorySlab not initialized"),
            hash: self,
        }
    }
}

fn init_hashes() {
    let _ = HASHES.get_or_init(|| {
        enum LoadMethod {
            Blob,
            HashFile,
            Missing,
        }

        let blob_path: &'static Utf8Path = Utf8Path::new("sd:/ultimate/stratus/hashes.blob");
        let meta_path: &'static Utf8Path = Utf8Path::new("sd:/ultimate/stratus/hashes.meta");
        let hashes_src: &'static Utf8Path = Utf8Path::new("sd:/ultimate/stratus/Hashes_FullPath");

        let now = Instant::now();
        let load_method: LoadMethod;

        let slab = if blob_path.exists() && meta_path.exists() {
            let blob = std::fs::read(blob_path).unwrap();
            let meta = std::fs::read(meta_path).unwrap();
            load_method = LoadMethod::Blob;

            HashMemorySlab::from_blob(blob.into_boxed_slice(), meta.into_boxed_slice())
        } else {
            let mut slab = HashMemorySlab::new();

            if let Ok(file) = std::fs::read_to_string(hashes_src) {
                let mut cache = InternerCache::default();
                for line in file.lines() {
                    let path = Utf8Path::new(line);
                    if let Some(extension) = path.extension() {
                        slab.intern_path(&mut cache, Utf8Path::new(extension));
                    }
                    if let Some(file_name) = path.file_name() {
                        slab.intern_path(&mut cache, Utf8Path::new(file_name));
                    }
                    slab.intern_path(&mut cache, Utf8Path::new(line));
                }

                slab.finalize(cache);

                let blob = slab.dump_blob();
                let meta = slab.dump_meta();
                std::fs::write(blob_path, blob).unwrap();
                std::fs::write(meta_path, meta).unwrap();
                load_method = LoadMethod::HashFile;
            } else {
                load_method = LoadMethod::Missing;
            }

            slab
        };

        let elapsed = now.elapsed().as_secs_f32();
        match load_method {
            LoadMethod::Blob => println!("[stratus::hashes] Loaded hash blob in {elapsed:.3}s"),
            LoadMethod::HashFile => {
                println!("[stratus::hashes] Generated hash blob in {elapsed:.3}s")
            }
            LoadMethod::Missing => println!("[stratus::hashes] No hash blob generated"),
        }

        ReadOnlyHashMemorySlab(slab)
    });
}

struct ReadOnlyArchive(Archive);

impl Deref for ReadOnlyArchive {
    type Target = Archive;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

unsafe impl Send for ReadOnlyArchive {}
unsafe impl Sync for ReadOnlyArchive {}

static ARCHIVE: OnceLock<ReadOnlyArchive> = OnceLock::new();

#[skyline::hook(offset = 0x3542a74, inline)]
fn print_info(ctx: &skyline::hooks::InlineCtx) {
    let index = ctx.registers[8].w();
    let info = ARCHIVE.get().unwrap().get_file_info(index).unwrap();
    let path = info.file_path();
    println!("{}", path.path_and_entity.hash40().display());
}

#[skyline::from_offset(0x392cc60)]
fn jemalloc(size: u64, align: u64) -> *mut u8;

static DID_LOAD: AtomicBool = AtomicBool::new(false);

#[skyline::hook(offset = 0x35442e8, inline)]
fn jemalloc_hook(ctx: &mut InlineCtx) {
    let res_service = ctx.registers[19].x() as *const u8;
    let current_index = ctx.registers[27].w();
    let absolute_index = unsafe { *res_service.add(0x230).cast::<u32>() } + current_index;
    let ptr: *mut u8;
    if ARCHIVE
        .get()
        .unwrap()
        .get_file_info(absolute_index)
        .unwrap()
        .file_path()
        .path_and_entity
        .hash40()
        == "ui/layout/menu/main_menu/main_menu/layout.arc".into_hash()
    {
        println!("Replacing main menu");
        ptr = std::fs::read(
            "sd:/ultimate/mods/hdr-assets/ui/layout/menu/main_menu/main_menu/layout.arc",
        )
        .unwrap()
        .leak()
        .as_mut_ptr();
        DID_LOAD.store(true, Ordering::SeqCst);
        ctx.registers[28].set_x(0x0);
    } else {
        ptr = unsafe { jemalloc(ctx.registers[0].x(), ctx.registers[1].x()) };
    }

    ctx.registers[0].set_x(ptr as u64);
}

#[skyline::hook(offset = 0x3544338, inline)]
fn skip_load_hook(ctx: &mut InlineCtx) {
    if DID_LOAD.swap(false, Ordering::SeqCst) {
        ctx.registers[3].set_x(2);
        println!("Did load");
    } else if ctx.registers[23].x() <= ctx.registers[8].x() {
        ctx.registers[3].set_x(1);
    } else {
        ctx.registers[3].set_x(0);
    }
}

#[skyline::hook(offset = 0x3544758, inline)]
fn skip_load_hook_p2(ctx: &mut InlineCtx) {
    let x = ctx.registers[3].x();
    ctx.registers[3].set_x(x - 1);
    if x - 1 == 0 {
        let mem_size = unsafe { *(ctx.sp.x() as *const u8).add(0x28).cast::<usize>() };
        ctx.registers[2].set_x(mem_size as u64);
    }
}

fn patch_res_threads() {
    use skyline::patching::Patch;

    Patch::in_text(0x35442e8).nop().unwrap(); // Nops jemalloc_hook
    Patch::in_text(0x3544338).data(0xB5002103u32).unwrap();
    Patch::in_text(0x3544758).data(0xB5001583u32).unwrap();

    Patch::in_text(0x3750c2c).nop().unwrap();
}

#[skyline::hook(offset = 0x3750c2c, inline)]
fn skip_load_arc_table(ctx: &mut InlineCtx) {
    ctx.registers[0].set_x(ARCHIVE.get().unwrap().data_ptr() as u64);
}

extern "C" {
    #[link_name = "_ZN2nn2oe22SetCpuOverclockEnabledEb"]
    fn set_overclock_enabled(enable: bool);

    #[link_name = "_ZN2nn2oe15SetCpuBoostModeENS0_12CpuBoostModeE"]
    fn set_cpu_boost_mode(val: i32);
}

#[skyline::main(name = "stratus")]
pub fn main() {
    init_folder();
    init_hashes();
    patch_res_threads();
    ARCHIVE.get_or_init(|| {
        unsafe {
            set_overclock_enabled(true);
            set_cpu_boost_mode(1);
        }
        let mut file = File::open("rom:/data.arc").unwrap();
        let mut archive = archive::Archive::read(&mut file);
        archive
            .lookup_file_path_mut("ui/layout/menu/main_menu/main_menu/layout.arc")
            .unwrap()
            .entity_mut()
            .info_mut()
            .desc_mut()
            .data_mut()
            .patch(
                std::fs::metadata(
                    "sd:/ultimate/mods/hdr-assets/ui/layout/menu/main_menu/main_menu/layout.arc",
                )
                .unwrap()
                .len() as u32,
            );
        unsafe {
            set_overclock_enabled(false);
            set_cpu_boost_mode(0);
        }
        ReadOnlyArchive(archive)
    });

    skyline::install_hooks!(
        skip_load_arc_table,
        jemalloc_hook,
        skip_load_hook,
        skip_load_hook_p2
    );
}
