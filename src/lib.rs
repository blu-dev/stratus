use std::{
    alloc::Layout,
    fs::File,
    io::Read,
    ops::Deref,
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
    time::Instant,
};

use camino::Utf8Path;
use skyline::hooks::InlineCtx;
use smash_hash::Hash40;

use crate::{
    archive::Archive,
    data::IntoHash,
    discover::FileSystem,
    hash_interner::{DisplayHash, HashMemorySlab, InternerCache},
};

mod archive;
mod containers;
mod data;
mod discover;
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

struct ReadOnlyFileSystem {
    hashes: HashMemorySlab,
    file_system: FileSystem,
}

impl ReadOnlyFileSystem {
    #[inline(always)]
    fn hashes() -> &'static HashMemorySlab {
        #[cfg(any(debug_assertions, feature = "sanity_checks"))]
        {
            &FILE_SYSTEM.get().unwrap().hashes
        }
        #[cfg(not(any(debug_assertions, feature = "sanity_checks")))]
        {
            // SAFETY: This is one of the first things we init, in release mode let's just declare the cold
            //  path impossible to reach
            unsafe { &FILE_SYSTEM.get().unwrap_unchecked().hashes }
        }
    }

    #[inline(always)]
    fn file_system() -> &'static FileSystem {
        #[cfg(any(debug_assertions, feature = "sanity_checks"))]
        {
            &FILE_SYSTEM.get().unwrap().file_system
        }
        #[cfg(not(any(debug_assertions, feature = "sanity_checks")))]
        {
            // SAFETY: This is one of the first things we init, in release mode let's just declare the cold
            //  path impossible to reach
            unsafe { &FILE_SYSTEM.get().unwrap_unchecked().file_system }
        }
    }
}

unsafe impl Send for ReadOnlyFileSystem {}

unsafe impl Sync for ReadOnlyFileSystem {}

static FILE_SYSTEM: OnceLock<ReadOnlyFileSystem> = OnceLock::new();

trait HashDisplay {
    fn display(self) -> DisplayHash<'static>;
}

impl HashDisplay for smash_hash::Hash40 {
    fn display(self) -> DisplayHash<'static> {
        DisplayHash {
            slab: ReadOnlyFileSystem::hashes(),
            hash: self,
        }
    }
}

fn init_hashes() {
    let _ = FILE_SYSTEM.get_or_init(|| {
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

        let mut slab = if blob_path.exists() && meta_path.exists() {
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

        let mut cache = InternerCache::default();
        let file_system = discover::discover_and_update_hashes(&mut slab, &mut cache);
        slab.finalize(cache);

        ReadOnlyFileSystem {
            hashes: slab,
            file_system,
        }
    });
}

struct ReadOnlyArchive(Archive);

impl ReadOnlyArchive {
    pub fn get() -> &'static Self {
        #[cfg(any(debug_assertions, feature = "sanity_checks"))]
        {
            ARCHIVE.get().unwrap()
        }
        #[cfg(not(any(debug_assertions, feature = "sanity_checks")))]
        {
            // SAFETY: This is one of the first things we init, in release mode let's just declare the cold
            //  path impossible to reach
            unsafe { ARCHIVE.get().unwrap_unchecked() }
        }
    }
}

impl Deref for ReadOnlyArchive {
    type Target = Archive;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

unsafe impl Send for ReadOnlyArchive {}
unsafe impl Sync for ReadOnlyArchive {}

static ARCHIVE: OnceLock<ReadOnlyArchive> = OnceLock::new();

static FILESYSTEM: OnceLock<FileSystem> = OnceLock::new();

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

extern "C" {
    #[link_name = "_ZN2nn2os16ReleaseSemaphoreEPNS0_13SemaphoreTypeE"]
    fn release_semaphore(ptr: u64);

    #[link_name = "_ZN2nn2os9WaitEventEPNS0_9EventTypeE"]
    fn wait_event(ptr: u64);
}

fn handle_io_swaps(ctx: &mut InlineCtx) {
    let res_service = ctx.registers[19].x() as *mut u8;
    let offset_into_read = unsafe { *res_service.add(0x220).cast::<u64>() };
    let mut x20 = ctx.registers[20].x();
    let mut x21 = ctx.registers[21].x();
    let mut x25 = ctx.registers[25].x();
    let target_offset_into_read = x20 + x21;

    if offset_into_read < target_offset_into_read {
        println!("Attempting to self-manage io swaps");

        let mut threshold = offset_into_read - x20;
        let mut x22 = ctx.registers[24].x();

        while threshold < x21 {
            unsafe {
                release_semaphore(*(**res_service.add(0x30).cast::<*const *const u64>()).add(0x1));
                release_semaphore(*(**res_service.add(0x28).cast::<*const *const u64>()).add(0x1));
            }

            if unsafe { *res_service.add(0xe6).cast::<bool>() } {
                panic!("Res service stopped what the hell");
            }

            let swap_event = unsafe { ***res_service.add(0x18).cast::<*const *const u64>() };
            if swap_event != 0 {
                unsafe {
                    wait_event(swap_event);
                }
            }

            x22 = unsafe { *res_service.add(0x218).cast::<u64>() };
            x25 = x25 - threshold;
            x20 = x20 + threshold;
            x21 = x21 - threshold;
            threshold =
                target_offset_into_read.min(unsafe { *res_service.add(0x220).cast::<u64>() }) - x21;
        }

        ctx.registers[20].set_x(x20);
        ctx.registers[21].set_x(x21);
        ctx.registers[22].set_x(x22);
        ctx.registers[25].set_x(x25);
    }
}

#[allow(static_mut_refs)]
#[skyline::hook(offset = 0x35442e8, inline)]
fn jemalloc_hook(ctx: &mut InlineCtx) {
    static mut BUFFER: String = String::new();
    let res_service = ctx.registers[19].x() as *const u8;
    let current_index = ctx.registers[27].w();
    let absolute_index = unsafe { *res_service.add(0x230).cast::<u32>() } + current_index;
    let ptr: *mut u8;
    let path = ReadOnlyArchive::get()
        .get_file_info(absolute_index)
        .map(|info| info.file_path().path_and_entity.hash40());

    if let Some(path) = path {
        // SAFETY: This path is only going to be called from the ResInflateThread, so this being a "static" variable is effectively a TLS variable
        if let Some(size) =
            ReadOnlyFileSystem::file_system().get_full_file_path(path, unsafe { &mut BUFFER })
        {
            // We need to create the same alignment on our buffer the game is expecting.
            // This alignment is going to be 0x1000 (page alignment) for graphics archives (BNTX and NUTEXB)
            let alignment = ctx.registers[0].x();

            // SAFETY: we check the null-ness of the pointer after allocation
            let buffer = unsafe {
                // SAFETY: We use unchecked here because the alignment comes from the game. It appears to either be 0x10 or 0x1000, both of which are powers
                //  of two, so I'm not concerned about the alignment being off.
                std::alloc::alloc(Layout::from_size_align_unchecked(
                    size as usize,
                    alignment as usize,
                ))
            };

            assert!(!buffer.is_null());

            // SAFETY: we assert that the slice is non-null, it's also allocated to the correct length and alignment above
            let slice = unsafe { std::slice::from_raw_parts_mut(buffer, size as usize) };

            // SAFETY: See above
            let mut file = std::fs::File::open(unsafe { &BUFFER }).unwrap();
            let amount_read = file.read(slice).unwrap();

            assert_eq!(amount_read, size as usize);

            ptr = buffer;
            // Register x28 is the flags for the FileData that we are loading. Setting these to 0x0 will activate the codepath
            // that enters our skip_load_hook(_2) hooks.
            //
            // For compressed files this will be 0x3, for uncompressed files it will be 0x0. Technically 0x2 is also supported but
            // literally zero files in the game have that flag, not that it would impact what we are doing here
            ctx.registers[28].set_x(0x0);
            DID_LOAD.store(true, Ordering::Relaxed);

            // We need to manually handle the IO swap mechanism here. The game will "correct" the IO swaps on the next file but either
            // I'm misunderstanding something (likely) or that codepath is actually bugged for what it's supposed to do. So instead
            // we will manually correct the IO swaps here.
            handle_io_swaps(ctx);
        } else {
            ptr = unsafe { jemalloc(ctx.registers[0].x(), ctx.registers[1].x()) };
        }
    } else {
        // log??
        ptr = unsafe { jemalloc(ctx.registers[0].x(), ctx.registers[1].x()) };
    }

    ctx.registers[0].set_x(ptr as u64);
}

/* This hook replaces a conditional branch that originally checks if the file is a "large load" (meaning that the data is split across
 *  an IO swap boundary) or a "small" load (data is contained completely within the buffer available to this thread).
 *
 * Originally the logic looks something like (inside of the switch-case statement on compression flags):
 * ```cpp
 *  // res_service->data_ptr is the pointer to currently unprocessed memory in the buffer provided by ResLoadingThread
 *  // res_service->offset_into_read is the offset of the data.arc up to where ResLoadingThread has given us. This is not a pointer
 *  // current_buffer_offset is a local variable that tracks the offset into the data.arc associated with the res_service->data_ptr
 *
 *  void* start_of_file_data = res_service->data_ptr;
 *  size_t file_end_offset = current_buffer_offset + file_size;
 *  if (file_end_offset < res_service->offset_into_read) {
 *       memcpy(file_buffer, start_of_file_data, file_size);
 *       break; // After breaking, current_buffer_offset and res_service->data_ptr are updated to reflect the new cursor into memory
 *  }
 *  // Proceeds to perform a "large load"...
 * ```
 *
 * We replace the offset conditional with our own check on what we set in this register. For more information, see `skip_load_hook_2`'s documentation
 */
#[skyline::hook(offset = 0x3544338, inline)]
fn skip_load_hook(ctx: &mut InlineCtx) {
    if DID_LOAD.swap(false, Ordering::SeqCst) {
        ctx.registers[3].set_x(2);
    } else if ctx.registers[23].x() <= ctx.registers[8].x() {
        ctx.registers[3].set_x(1);
    } else {
        ctx.registers[3].set_x(0);
    }
}

/* This hook is intended to skip a memcpy that we don't need to do
 * Explanation:
 *  In the vanilla implementation of ResInflateThread, when loading uncompressed files, the game
 *  checks to see if the remaining amount of memory to be copied (starting as FileData::decompressed_size)
 *  is larger than buffer ResLoadingThread has prepared for us.
 *  We are loading the data off of the SD card, which means that we don't need to memcpy from the buffer
 *  that was provided from ResLoadingThread.
 *
 *  If we do not load the data off of the SD card, then we need to fall back to the vanilla implementation. We prepare
 *  an unused register (x3) in jemalloc_hook and update it in skip_load_hook that tells us if we need to fallback
 *  to the game's "large load" memcpy loop (for files who's bytes to be read are larger than the available buffer).
 *
 *  Coming into this hook, this value can be one of 2:
 *  - 1: This indicates that the file is *not* a "large load" and that we should do the normal memcpy
 *  - 2: This indicates that the file was loaded by us, and that we should break out of the switch-case loop, which we accomplish
 *      via the cbnz instruction replacement (see patch_res_threads)
 *
 *  Notably, even though skip_load_hook can set the register value to 0, the instruction which runs after skip_load_hook
 *  will branch to other code (that manages the vanilla "large load") if the file was not loaded by us and is a large load
 */
#[skyline::hook(offset = 0x3544758, inline)]
fn skip_load_hook_p2(ctx: &mut InlineCtx) {
    let x = ctx.registers[3].x();
    ctx.registers[3].set_x(x - 1);
    if x - 1 == 0 {
        let mem_size = unsafe { *(ctx.sp.x() as *const u8).add(0x28).cast::<usize>() };
        ctx.registers[2].set_x(mem_size as u64);
    } else {
        // This assertion is a sanity check, it shouldn't trigger as long as jemalloc_hook is implemented correctly
        // The expalanation is that we set only three values from skip_load_hook. We can only either see 0x1 or 0x2 coming into this function.
        // We handle 0x1 in the above block, so this block must represent 0x2. If it is not that value then there is a bug.
        #[cfg(any(debug_assertions, feature = "sanity_checks"))]
        {
            assert_eq!(x - 1, 0x1);
        }

        // The replaced instruction simulates breaking from the switch-case block that this code is inside of. If you look at the decompilation in ghidra,
        // you will not see this action, but it is in the disassembly right before the break. I'm not sure what it's intended to track, but removing it
        // causes the group load to end very quickly and skip most of the FileInfo
        ctx.registers[21].set_w(unsafe { *(ctx.sp.x() as *const u8).add(0x1c).cast::<u32>() });
    }
}

/* This replaces the game's reading and decompression of the file tables. We already do this in order to patch the filesystem
 *  before the game gets around to it, so we can save time on boot by just providing the game with the result of the work
 *  that we have already done.
 */
#[skyline::hook(offset = 0x3750c2c, inline)]
fn skip_load_arc_table(ctx: &mut InlineCtx) {
    ctx.registers[0].set_x(ReadOnlyArchive::get().data_ptr() as u64);
}

fn patch_res_threads() {
    use skyline::patching::Patch;

    // jemalloc_hook
    Patch::in_text(0x35442e8).nop().unwrap(); // Nops jemalloc_hook

    // skip_load_hook
    Patch::in_text(0x3544338).data(0xB5002103u32).unwrap(); // cbnz x3, #0x420 (replacing b.ls #0x424)

    // skip_load_hook_2
    Patch::in_text(0x3544758).data(0xB5001583u32).unwrap(); // cbnz x3, #0x2b0 (replacing ldr x2, [sp, #0x28])

    Patch::in_text(0x3750c2c).nop().unwrap();
}

extern "C" {
    #[link_name = "_ZN2nn2oe22SetCpuOverclockEnabledEb"]
    fn set_overclock_enabled(enable: bool);

    #[link_name = "_ZN2nn2oe15SetCpuBoostModeENS0_12CpuBoostModeE"]
    fn set_cpu_boost_mode(val: i32);

    // #[link_name = "_ZN2nn2oe27SetPerformanceConfigurationENS0_15PerformanceModeEi"]
    // unsafe fn set_performance_config
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

        for (path, file) in ReadOnlyFileSystem::file_system().files() {
            if let Some(path) = archive.lookup_file_path_mut(*path) {
                let path_hash = path.path_and_entity.hash40();
                println!("Patching {}", path.path_and_entity.hash40().display());

                let mut info = path.entity_mut().info_mut();
                if info.path_ref().path_and_entity.hash40() != path_hash {
                    println!(
                        "Patching {} <-> {}",
                        path_hash.display(),
                        info.path_ref().path_and_entity.hash40().display()
                    );
                    info = info.entity_mut().info_mut();
                } else {
                    println!("Patching {}", path_hash.display());
                }

                let mut data = info.desc_mut().data_mut();

                data.patch(file.size());
            }
        }
        ReadOnlyArchive(archive)
    });

    unsafe {
        // set_overclock_enabled(false);
        set_cpu_boost_mode(0);
    }

    skyline::install_hooks!(
        skip_load_arc_table,
        jemalloc_hook,
        skip_load_hook,
        skip_load_hook_p2
    );
}
