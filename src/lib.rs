use std::{
    alloc::Layout,
    collections::{HashMap, HashSet},
    fs::File,
    ops::Deref,
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
    time::Instant,
};

use camino::Utf8Path;
use log::LevelFilter;
use skyline::hooks::InlineCtx;
use smash_hash::{Hash40, Hash40Map};

use crate::{
    archive::{decompress_stream, Archive, ZstdBuffer},
    data::{
        FileData, FileDescriptor, FileEntity, FileInfo, FileInfoFlags, FileLoadMethod, FilePath,
        SearchPath, TryFilePathResult,
    },
    discover::{FileSystem, NewFile},
    hash_interner::{DisplayHash, HashMemorySlab},
    logger::NxKernelLogger,
};

mod archive;
mod containers;
mod data;
mod discover;
mod hash_interner;
mod logger;

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
            let mut cache = slab.create_cache();

            if let Ok(file) = std::fs::read_to_string(hashes_src) {
                // let mut cache = InternerCache::default();
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

        let mut cache = slab.create_cache();
        let now = std::time::Instant::now();
        let file_system = discover::discover_and_update_hashes(&mut slab, &mut cache);
        println!(
            "[stratus::hashes] Discovered mod files in {:.3}s",
            now.elapsed().as_secs_f32()
        );
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

#[skyline::from_offset(0x392cc60)]
fn jemalloc(size: u64, align: u64) -> *mut u8;

static DID_LOAD: AtomicBool = AtomicBool::new(false);

extern "C" {
    #[link_name = "_ZN2nn2os16ReleaseSemaphoreEPNS0_13SemaphoreTypeE"]
    fn release_semaphore(ptr: u64);

    #[link_name = "_ZN2nn2os9WaitEventEPNS0_9EventTypeE"]
    fn wait_event(ptr: u64);
}

fn handle_inflate_io_swaps(ctx: &mut InlineCtx) {
    let res_service = ctx.registers[19].x() as *mut u8;
    let offset_into_read = unsafe { *res_service.add(0x220).cast::<u64>() };
    let mut x20 = ctx.registers[20].x();
    let mut x21 = ctx.registers[21].x();
    let mut x25 = ctx.registers[25].x();
    let target_offset_into_read = x20 + x21;

    if offset_into_read < target_offset_into_read {
        log::info!("Attempting to self-manage io swaps");

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

    let Some(info) = ReadOnlyArchive::get().get_file_info(absolute_index) else {
        panic!("ResInflateThread handed invalid info index");
    };

    let path = match info.try_file_path() {
        TryFilePathResult::FilePath(path) => {
            // log::info!(
            //     "[jemalloc_hook] Inflating file {} with load method {:#x}",
            //     path.path().display(),
            //     unsafe { *res_service.add(0x234).cast::<u32>() }
            // );
            path.path_and_entity.hash40()
        }
        TryFilePathResult::Reshared(path) => {
            log::info!(
                "[jemalloc_hook] Inflating reshared file {}",
                path.path().const_trim_trailing(".reshared").display()
            );
            // #[cfg(not(feature = "verbose_logging"))]
            // {
            //     let _ = path;
            // }

            // #[cfg(feature = "verbose_logging")]
            // {
            //     log::info!("Encountered reshared file pointing at '{}''s original data, skipping file replacement", path.path_and_entity.hash40().display());
            // }

            let ptr = unsafe { jemalloc(ctx.registers[0].x(), ctx.registers[1].x()) };
            ctx.registers[0].set_x(ptr as u64);
            return;
        }
        TryFilePathResult::Missing => panic!("File info is not pointing to a real file path"),
    };

    // if cfg!(feature = "verbose_logging") {
    let offset_into_read = unsafe { *res_service.add(0x220).cast::<u64>() };
    let x20 = ctx.registers[20].x();
    let x21 = ctx.registers[21].x();
    let target_offset_into_read = x20 + x21;
    log::info!(
        "Attempting to load {} with cursor {:#x} | {:#x} | {:#x} | {:#x} ({} / {})",
        path.display(),
        offset_into_read,
        target_offset_into_read,
        ctx.registers[25].x(),
        unsafe { *res_service.add(0x234).cast::<u32>() },
        current_index,
        unsafe { *res_service.add(0x22C).cast::<u32>() }
    );
    // }

    // SAFETY: This path is only going to be called from the ResInflateThread, so this being a "static" variable is effectively a TLS variable
    if let Some(size) = ReadOnlyFileSystem::file_system()
        .get_full_file_path(path, unsafe { &mut BUFFER })
        .filter(|_| {
            !info
                .flags()
                .intersects(FileInfoFlags::IS_LOCALIZED | FileInfoFlags::IS_REGIONAL)
        })
    {
        // SAFETY: See above
        log::info!("[jemalloc_hook] Replacing {}", unsafe { &BUFFER });

        // We need to create the same alignment on our buffer the game is expecting.
        // This alignment is going to be 0x1000 (page alignment) for graphics archives (BNTX and NUTEXB)
        let alignment = ctx.registers[0].x();

        // We are checking the load method here. If it is 4 (single file) then that means we loaded the pointer in ResLoadingThread
        // and don't need to repeat the file IO here. The data_ptr should get replaced the next time that the game needs to load
        // something so we don't need to worry about other file loads reading from that pointer
        if unsafe { *res_service.add(0x234).cast::<u32>() == 0x4 } {
            log::info!("[jemalloc_hook] Taking loaded file pointer from ResLoadingThread (single file replacement)");
            ptr = unsafe { *res_service.add(0x218).cast::<*mut u8>() };
        } else {
            // SAFETY: we check the null-ness of the pointer after allocation
            let buffer = unsafe {
                // SAFETY: We use unchecked here because the alignment comes from the game. It appears to either be 0x10 or 0x1000, both of which are powers
                //  of two, so I'm not concerned about the alignment being off.
                std::alloc::alloc(
                    Layout::from_size_align(size as usize, alignment as usize).unwrap(),
                )
            };

            assert!(!buffer.is_null());
            assert!(buffer as u64 % alignment == 0x00);

            // SAFETY: we assert that the slice is non-null, it's also allocated to the correct length and alignment above
            let slice = unsafe { std::slice::from_raw_parts_mut(buffer, size as usize) };

            ReadOnlyFileSystem::file_system().load_into_buffer(path, unsafe { &BUFFER }, slice);

            // // SAFETY: See above
            // let mut file = std::fs::File::open(unsafe { &BUFFER }).unwrap();
            // let amount_read = file.read(slice).unwrap();

            // assert_eq!(amount_read, size as usize);

            ptr = buffer;

            // We need to manually handle the IO swap mechanism here. The game will "correct" the IO swaps on the next file but either
            // I'm misunderstanding something (likely) or that codepath is actually bugged for what it's supposed to do. So instead
            // we will manually correct the IO swaps here.
            handle_inflate_io_swaps(ctx);
        }

        // Register x28 is the flags for the FileData that we are loading. Setting these to 0x0 will activate the codepath
        // that enters our skip_load_hook(_2) hooks.
        //
        // For compressed files this will be 0x3, for uncompressed files it will be 0x0. Technically 0x2 is also supported but
        // literally zero files in the game have that flag, not that it would impact what we are doing here
        ctx.registers[28].set_x(0x0);
        DID_LOAD.store(true, Ordering::Relaxed);
    } else {
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
    if DID_LOAD.swap(false, Ordering::Relaxed) {
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

static mut LOADING_THREAD_PATCHED_POINTER: Option<*mut u8> = None;

#[allow(static_mut_refs)]
#[skyline::hook(offset = 0x3542f64, inline)]
fn process_single_patched_file_request(ctx: &mut InlineCtx) {
    static mut BUFFER: String = String::new();

    // This address is to point right after the instruction we hook.
    // This is a little sketchy because skyline technically can replace 5 instructions for a very long
    // instead the single instruction that we are depending on. The idea here is that we replace the instruction
    // we hook with br x3, and if we need to take the vanilla codepath we are going to jump to the instruction after this one.
    // If we don't want to take the vanilla codepath, we are going to jump to two instructions after this one,
    // and fake the return value
    static mut OFFSET_ABSOLUTE_ADDRESS: u64 = 0x0;

    // SAFETY: Referencing OFFSET_ABSOLUTE_ADDRESS is effectively a function local variable, it never gets referenced outside this thread
    if unsafe { OFFSET_ABSOLUTE_ADDRESS } == 0 {
        // SAFETY: See above, and our plugin cannot exist outside of the skyline runtime, so calling that function is safe
        unsafe {
            OFFSET_ABSOLUTE_ADDRESS = skyline::hooks::getRegionAddress(skyline::hooks::Region::Text)
                .cast::<u8>()
                .add(0x3542f68) as u64;
        }
    }

    let file_info_idx = ctx.registers[20].w();
    let archive = ReadOnlyArchive::get();
    let Some(info) = archive.get_file_info(file_info_idx) else {
        // LOG??
        panic!("Invalid file info index provided to ResLoadingThread");
    };

    let path = match info.try_file_path() {
        TryFilePathResult::FilePath(path) => {
            log::info!(
                "[process_single_patched_file_request] Loading file {}",
                path.path().display()
            );
            path
        }

        // If the path is reshared then we do an early exit, because we need to let the game
        // load it properly.
        //
        // We do file resharing in this way so that we can maintain information in the logs/within stratus
        // about which files were reshared and to what base path, but those files were replaced and
        // we don't want to load their file data for a file that was unchaged.
        //
        // TODO: Explain this better it's midnight and I'm eepy
        TryFilePathResult::Reshared(path) => {
            log::info!(
                "[process_single_patched_file_request] Loading reshared file {}",
                path.path().display()
            );
            // See bottom of function for output variables/registers
            ctx.registers[2].set_x(ctx.registers[21].x());

            // SAFETY: See above on static mut variables
            ctx.registers[3].set_x(unsafe { OFFSET_ABSOLUTE_ADDRESS });
            return;
        }
        TryFilePathResult::Missing => panic!("File info is missing file path"),
    };

    let path = path.path_and_entity.hash40();

    // SAFETY: Referencing BUFFER here is safe since this is an inline hook only ever called from within
    // ResLoadingThread. It effectively becomes a function local variable
    if let Some(size) = ReadOnlyFileSystem::file_system()
        .get_full_file_path(path, unsafe { &mut BUFFER })
        .filter(|_| {
            !info
                .flags()
                .intersects(FileInfoFlags::IS_LOCALIZED | FileInfoFlags::IS_REGIONAL)
        })
    {
        log::info!(
            "[process_single_patched_file_request] Replacing file {}",
            path.display()
        );
        // We know that we are loading a file as a single file request, let's make use of our file IO thread to load
        // this instead of trying to read anything from the data.arc
        // For now we are still going to pass it over to the ResInflateThread. ResLoadingThread will configure the load method
        // as method #4 which we can check inside of our jemalloc_hook.
        //
        // Loading the file here is actually a requirement for unshared files, since we set their compressed_size to 0x0 so that
        // if they are loaded via any other load method, or as part of a package/group, they don't advance the buffer cursor
        // at all

        // TODO: Change the definition of FileInfoFlags to remove IS_REGULAR_FILE and IS_GRAPHICS_ARCHIVE,
        //      the lower 15 bits are used for buffer alignment
        let buffer_alignment = info.flags().bits() & 0x7FFF;

        // This is only a sanity check to make sure that the above knowledge is true
        #[cfg(any(debug_assertions, feature = "sanity_checks"))]
        {
            assert!(buffer_alignment.is_power_of_two());
        }

        // TODO: Remove unwrap
        let buffer = unsafe {
            std::alloc::alloc(
                Layout::from_size_align(size as usize, buffer_alignment as usize).unwrap(),
            )
        };

        assert!(!buffer.is_null());

        // Allocator should confirm this for us but I don't want any bugs cropping up because of it
        #[cfg(any(debug_assertions, feature = "sanity_checks"))]
        {
            assert_eq!(buffer as u64 % buffer_alignment as u64, 0x0);
        }

        // SAFETY: We assert on the alignment of the slice, the length we just have to trust the allocator
        let slice = unsafe { std::slice::from_raw_parts_mut(buffer, size as usize) };

        ReadOnlyFileSystem::file_system().load_into_buffer(path, unsafe { &BUFFER }, slice);

        // SAFETY: See above
        // let mut file = std::fs::File::open(unsafe { &BUFFER }).unwrap();
        // let amount_read = file.read(slice).unwrap();

        // // TODO: Should this be sanity?
        // assert_eq!(amount_read, size as usize);

        // OUT VARIABLES:
        // - x0 is supposed to be the return value of the vanilla read function.
        // - x21 is the compressed size read from the FileData. We are going to set our compressed size to the size
        //    of the pointer buffer, that way it accurately reflects the size of the pointer we are about to give the resource service
        // - x3 is the address of the instruction to jump to, we want to jump past the vanilla codepath so we add 4 (size of one instruction)
        //    to the cached address
        ctx.registers[21].set_x(size as u64);
        ctx.registers[0].set_x(size as u64);
        // SAFETY: See above on static mut variables
        ctx.registers[3].set_x(unsafe { OFFSET_ABSOLUTE_ADDRESS + 4 });

        // SAFETY: We need to track this pointer down a little bit when we hand it over to ResInflateThread. Storing it in a static mut is fine
        // as long as we only reference it from code that runs inside of this loading thread.
        unsafe { assert!(LOADING_THREAD_PATCHED_POINTER.replace(buffer).is_none()) };
    } else {
        // OUT VARIABLES: This is the vanilla codepath, so we need to set the pointer to the instruction we want to jump to
        //  as well as simulate the instruction that we replaced (mov x2, x21)

        ctx.registers[2].set_x(ctx.registers[21].x());

        // SAFETY: See above on static mut variables
        ctx.registers[3].set_x(unsafe { OFFSET_ABSOLUTE_ADDRESS });
    }
}

/* This replaces ResLoadingThread's assigment of res_service->data_ptr to the data we loaded above
 *  if we loaded it. This codepath will only fire for single file loads.
 */
#[allow(static_mut_refs)]
#[skyline::hook(offset = 0x3542fc4, inline)]
fn loading_thread_assign_patched_pointer(ctx: &mut InlineCtx) {
    // SAFETY: We are only accessing this static mut inside of the ResLoadingThread, so it's not worth the overhead
    // of a mutex lock here
    if let Some(pointer) = unsafe { LOADING_THREAD_PATCHED_POINTER.take() } {
        log::info!("[loading_thread_assigned_patch_pointer] Setting patched file pointer");
        ctx.registers[8].set_x(pointer as u64);
    }
}

/* This replaces the game's reading and decompression of the file tables. We already do this in order to patch the filesystem
 *  before the game gets around to it, so we can save time on boot by just providing the game with the result of the work
 *  that we have already done.
 */
#[skyline::hook(offset = 0x3750c2c, inline)]
fn skip_load_resource_tables(ctx: &mut InlineCtx) {
    ctx.registers[0].set_x(ReadOnlyArchive::get().resource_data_ptr() as u64);
}

#[skyline::hook(offset = 0x3750c44, inline)]
fn skip_load_search_tables(ctx: &mut InlineCtx) {
    ctx.registers[0].set_x(ReadOnlyArchive::get().search_data_ptr() as u64);
}

#[skyline::hook(offset = 0x3544804, inline)]
fn observe_decompression(ctx: &mut InlineCtx) {
    let compressor = ctx.registers[0].x();
    let buffer_out = ctx.registers[1].x() as *mut ZstdBuffer;
    let buffer_in = ctx.registers[2].x() as *mut ZstdBuffer;

    let before = format!(
        "[observe_decompression] Decompressing: IN: {:?}, OUT: {:?}",
        unsafe { &*buffer_in },
        unsafe { &*buffer_out }
    );
    let result = unsafe { decompress_stream(compressor as _, &mut *buffer_out, &mut *buffer_in) };
    ctx.registers[0].set_x(result as _);
    log::info!("{before}, RESULT: {:#x}", result);
}

fn patch_res_threads() {
    use skyline::patching::Patch;

    // jemalloc_hook
    Patch::in_text(0x35442e8).nop().unwrap(); // Nops jemalloc_hook

    // // skip_load_hook
    Patch::in_text(0x3544338).data(0xB5002103u32).unwrap(); // cbnz x3, #0x420 (replacing b.ls #0x424)

    // // skip_load_hook_2
    Patch::in_text(0x3544758).data(0xB5001583u32).unwrap(); // cbnz x3, #0x2b0 (replacing ldr x2, [sp, #0x28])

    // // process_single_patched_file_request
    Patch::in_text(0x3542f64).data(0xD61F0060u32).unwrap(); // br x3 (replacing mov x2, x21)

    // skip_load_resource_tables
    Patch::in_text(0x3750c2c).nop().unwrap();

    // skip_load_search_tables
    Patch::in_text(0x3750c44).nop().unwrap();

    // observe_decompression
    Patch::in_text(0x3544804).nop().unwrap();

    // Patch::in_text(0x3544e7c).nop().unwrap();
    // Patch::in_text(0x3544e84).nop().unwrap();
}

extern "C" {
    #[link_name = "_ZN2nn2oe22SetCpuOverclockEnabledEb"]
    fn set_overclock_enabled(enable: bool);

    #[link_name = "_ZN2nn2oe15SetCpuBoostModeENS0_12CpuBoostModeE"]
    fn set_cpu_boost_mode(val: i32);
}

#[skyline::hook(offset = 0x3544a1c, inline)]
fn panic_set_invalid_state(ctx: &InlineCtx) {
    if ctx.registers[8].w() == 0xFFFFFFFF {
        let res_service = ctx.registers[19].x() as *const u8;
        let current_index = ctx.registers[27].w();
        let absolute_index = unsafe { *res_service.add(0x230).cast::<u32>() } + current_index;
        let Some(info) = ReadOnlyArchive::get().get_file_info(absolute_index) else {
            panic!("ResInflateThread handed invalid info index");
        };
        panic!("Loading file {} failed", info.file_path().path().display());
    }
}

#[skyline::hook(offset = 0x3543c34, inline)]
fn observe_res_service_inflate(ctx: &InlineCtx) {
    let res_service = ctx.registers[19].x() as *const u8;
    let current_index = ctx.registers[27].w();
    let absolute_index = unsafe { *res_service.add(0x230).cast::<u32>() } + current_index;
    let Some(info) = ReadOnlyArchive::get().get_file_info(absolute_index) else {
        panic!("ResInflateThread handed invalid info index");
    };
    log::info!(
        "[observe_inflate] Inflating file {} (info: {:#x}) with load method {:#x} (data ptr: {:#x})",
        info.try_file_path().unwrap().path().display(),
        info.index(),
        unsafe { *res_service.add(0x234).cast::<u32>() },
        ctx.registers[24].x()
    );
}

#[no_mangle]
pub extern "C" fn arcrop_is_mod_enabled(_: u64) -> bool {
    true
}

#[repr(C)]
pub struct ApiVersion {
    major: u32,
    minor: u32,
}

#[no_mangle]
pub extern "C" fn arcrop_api_version() -> &'static ApiVersion {
    static VERSION: ApiVersion = ApiVersion { major: 1, minor: 8 };
    &VERSION
}

#[no_mangle]
pub extern "C" fn arcrop_require_api_version(_major: u32, _minor: u32) {}

#[global_allocator]
static ALLOC: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

#[skyline::hook(offset = 0x3750b8c, inline)]
fn initial_loading(_ctx: &InlineCtx) {
    ARCHIVE.get_or_init(|| {
        let now = std::time::Instant::now();
        let mut archive = archive::Archive::open();

        println!(
            "[stratus::patching] Loaded archive tables in {:.3}s",
            now.elapsed().as_secs_f32()
        );

        struct UnsharedFileInfo {
            real_infos: Vec<(u32, u32)>,
            group_offset: u32,
            package_index: u32,
        }

        #[derive(Default)]
        struct ReshareFileInfo {
            dependents: Vec<u32>,
        }

        let mut unshare_cache = Hash40Map::default();
        let mut reverse_unshare_cache: HashMap<u32, ReshareFileInfo> = HashMap::default();
        let mut unshare_secondary_cache: HashMap<u32, Vec<u32>> = HashMap::default();

        let now = std::time::Instant::now();
        for package_idx in 0..archive.num_file_package() {
            let package = archive.get_file_package(package_idx as u32).unwrap();
            let infos = package.infos();
            for idx in 0..infos.len() {
                let info = infos.get_local(idx).unwrap();
                let shared_info = info.entity().info();

                if info.index() != shared_info.index() {
                    let entry = reverse_unshare_cache
                        .entry(shared_info.index())
                        .or_default();

                    // Some files are shared to themselves! This is because of 1 of 2 situations:
                    // 1. The file is included in multiple different file packages. This is the case for some PRC
                    //      files and possibly some other files (I think Corrin has one?) (less common case). In these
                    //      cases we don't need to do anything to said file, because the original data is still going to be
                    //      expected to be loaded as a single file request (it's original data is in a file package, not a group)
                    // 2. The file is shared across file package and file groups. This is the case for lots of files like
                    //      models and textures. In these cases, we need to do some manipulation. For our use cases we call this "renaming" the file,
                    //      which will add a new FilePath entry and allow unsharing to work like normal. We have to do this because the
                    //      OG file data is a part of a file group and will be loaded regardless, we cannot replace it effectively.
                    //
                    // To figure out which one of these it is, we look at the file descriptor's load method. The correct way would be
                    // to check if the index of the file info is >= the index of the first FileGroup's FileInfo. Perhaps at a later
                    // date we will do it that way
                    if info.file_path().index() == shared_info.file_path().index() {
                        if shared_info.desc().load_method().is_skip() {
                            // rename_cache.insert(shared_info.index());
                        } else {
                            // Skip unsharing since this file should continue to be shared to itself
                            continue;
                        }
                    }
                    entry.dependents.push(info.index());

                    // group_offset = (group_offset + 0xf) & 0x10;
                    unshare_cache
                        .entry(info.file_path().path_and_entity.hash40())
                        .or_insert_with(|| UnsharedFileInfo {
                            real_infos: vec![],
                            package_index: package.index(),
                            group_offset: 0,
                        })
                        .real_infos
                        .push((package_idx as u32, info.index()));
                }
            }
        }
        println!(
            "[stratus::patching] Built unshare and rename cache in {:.3}s",
            now.elapsed().as_secs_f32()
        );

        let now = std::time::Instant::now();
        let mut renamed = HashMap::new();
        let mut managed_groups = HashSet::new();
        for package_idx in 0..archive.num_file_package() as u32 {
            let package = archive.get_file_package(package_idx).unwrap();
            if let Some(group) = package.file_group() {
                if !managed_groups.insert(group.index()) {
                    continue;
                }
                let info_range = group.file_info_slice().range();
                for info_idx in info_range {
                    let mut info = archive.get_file_info_mut(info_idx).unwrap();
                    let path_idx = info.path_ref().index();
                    let new_fp_idx = if let Some(new_idx) = renamed.get(&path_idx) {
                        *new_idx
                    } else {
                        let file_path = info.path_ref().clone();

                        let new_idx = info.archive_mut().insert_file_path(FilePath::from_parts(
                            file_path.path().const_with(".reshared"),
                            file_path.parent(),
                            file_path.file_name(),
                            file_path.extension(),
                            file_path.path_and_entity.data(),
                        ));

                        renamed.insert(path_idx, new_idx);
                        new_idx
                    };

                    info.set_path(new_fp_idx);
                    info.set_as_reshared();
                    info.desc()
                        .set_load_method(FileLoadMethod::PackageSkip(info_idx));
                }
            }
        }
        // for file in rename_cache {
        //     let mut info = archive.get_file_info_mut(file).unwrap();
        //     let file_path = info.path_ref().clone();

        //     let new_fp_idx = info.archive_mut().insert_file_path(FilePath::from_parts(
        //         file_path.path().const_with(".reshared"),
        //         file_path.parent(),
        //         file_path.file_name(),
        //         file_path.extension(),
        //         file_path.path_and_entity.data(),
        //     ));

        //     println!(
        //         "Processing {} ({:#x?})",
        //         file_path.path().display(),
        //         info.desc_ref().load_method() // original_info.display()
        //     );
        //     info.set_path(new_fp_idx);
        //     info.set_as_reshared();
        //     info.desc()
        //         .set_load_method(FileLoadMethod::PackageSkip(file));
        // }
        println!(
            "[stratus::patching] Renamed group-shared files in {:.3}s",
            now.elapsed().as_secs_f32()
        );

        let now = std::time::Instant::now();
        for (path, file) in ReadOnlyFileSystem::file_system().files() {
            if let Some(path) = archive.lookup_file_path_mut(*path) {
                let path_hash = path.path_and_entity.hash40();

                if let Some(unshare_info) = unshare_cache.get(&path_hash) {
                    let source_path = archive
                        .get_file_info(unshare_info.real_infos[0].1)
                        .unwrap()
                        .file_path()
                        .entity()
                        .info()
                        .try_file_path()
                        .unwrap()
                        .path();

                    if archive
                        .get_file_info(unshare_info.real_infos[0].1)
                        .unwrap()
                        .flags()
                        .intersects(FileInfoFlags::IS_REGIONAL | FileInfoFlags::IS_LOCALIZED)
                    {
                        log::info!(
                            "Skipping {} because it is localized/regional",
                            path_hash.display()
                        );
                        continue;
                    }

                    log::info!(
                        "Unsharing {} from {}",
                        path_hash.display(),
                        source_path.display(),
                    );
                    for (package_idx, info_idx) in unshare_info.real_infos.iter().copied() {
                        log::info!(
                            "\tInfo {:#x} in {} (Flags: {:?})",
                            info_idx,
                            archive
                                .get_file_package(package_idx)
                                .unwrap()
                                .path()
                                .display(),
                            archive.get_file_info(info_idx).unwrap().flags(),
                        );
                    }

                    let new_data_idx = archive.push_file_data(FileData::new_for_unsharing(
                        file.size(),
                        unshare_info.group_offset,
                    ));

                    let data_group_idx = archive
                        .get_file_package(unshare_info.package_index)
                        .unwrap()
                        .data_group()
                        .index();

                    let new_entity_idx = archive.push_file_entity(FileEntity::new(
                        unshare_info.package_index,
                        unshare_info.real_infos[0].1,
                    ));
                    let new_desc_idx = archive.push_file_desc(FileDescriptor::new(
                        data_group_idx,
                        new_data_idx,
                        FileLoadMethod::Owned(0),
                    ));

                    let mut group = archive.get_file_group_mut(data_group_idx).unwrap();

                    // HACK: Some files, when unshared, belong to a data group whose size is zero. ResLoadingThread
                    // will skip loading that data group if this is the case. Instead, we politely tell it that there
                    // is actually data to read. This allows the streaming decompressor to work on our files.
                    if group.compressed_size() == 0 {
                        group.set_compressed_size(0x10);
                    }

                    let mut first_info = archive
                        .get_file_info_mut(unshare_info.real_infos[0].1)
                        .unwrap();
                    let mut flags = first_info.flags();
                    flags.set(FileInfoFlags::IS_SHARED, false);
                    flags.set(FileInfoFlags::IS_UNKNOWN_FLAG, false);
                    first_info.set_flags(flags);
                    first_info.set_entity(new_entity_idx);
                    first_info.set_desc(new_desc_idx);
                    first_info.path_mut().set_entity(new_entity_idx);

                    for (_, info) in unshare_info.real_infos.iter().skip(1) {
                        let archive = first_info.archive_mut();
                        let mut info = archive.get_file_info_mut(*info).unwrap();
                        info.set_entity(new_entity_idx);
                        let mut flags = info.flags();
                        flags.set(FileInfoFlags::IS_SHARED, true);
                        flags.set(FileInfoFlags::IS_UNKNOWN_FLAG, true);
                        info.set_flags(flags);
                        info.desc_mut()
                            .set_load_method(FileLoadMethod::Unowned(new_entity_idx));
                    }

                    if let Some(secondary) =
                        unshare_secondary_cache.remove(&first_info.path_ref().index())
                    {
                        println!(
                            "Unsharing secondary {}",
                            first_info.path_ref().path().display()
                        );
                        let archive = first_info.into_archive_mut();
                        for secondary in secondary {
                            let mut secondary_info = archive.get_file_info_mut(secondary).unwrap();
                            secondary_info.set_entity(new_entity_idx);
                        }
                    }
                } else {
                    let mut info_mut = path.entity_mut().info_mut();

                    if info_mut
                        .flags()
                        .intersects(FileInfoFlags::IS_LOCALIZED | FileInfoFlags::IS_REGIONAL)
                    {
                        continue;
                    }

                    if let Some(reshare_info) = reverse_unshare_cache.remove(&info_mut.index()) {
                        assert_eq!(info_mut.index(), info_mut.entity_ref().info().index());
                        let mut info = info_mut.clone();
                        let mut desc = info_mut.desc_ref().clone();
                        let data = info_mut.desc_ref().data().clone();
                        let entity_index = info_mut.entity_ref().index();
                        let group_idx = info_mut.entity_ref().package_or_group();
                        let archive = info_mut.archive_mut();
                        let new_data_idx = archive.push_file_data(data);
                        desc.set_data(new_data_idx);
                        let new_desc_idx = archive.push_file_desc(desc);
                        info.set_desc(new_desc_idx);
                        info.set_entity(archive.num_file_entity() as u32);
                        info.set_as_reshared();
                        let new_info_idx = archive.push_file_info(info);
                        let entity = FileEntity::new(group_idx, new_info_idx);
                        let new_entity_idx = archive.push_file_entity(entity);

                        for file in reshare_info.dependents.iter().copied() {
                            let mut dependent_info = archive.get_file_info_mut(file).unwrap();
                            // The file is no longer shared, we don't have to do anything
                            if dependent_info.entity_ref().index() != entity_index {
                                continue;
                            }
                            log::info!(
                                "Reversing shared requirement for {}",
                                dependent_info.path_ref().path_and_entity.hash40().display()
                            );

                            dependent_info.path_mut().set_entity(new_entity_idx);
                            dependent_info.set_entity(new_entity_idx);
                            let mut dependent_desc = dependent_info.desc();
                            dependent_desc.set_load_method(FileLoadMethod::Unowned(new_entity_idx));
                        }
                    }

                    let mut data = info_mut.desc().data_mut();
                    data.patch(file.size());
                }
            }
        }
        println!(
            "[stratus::patching] Unshared files in {:.3}s",
            now.elapsed().as_secs_f32()
        );

        let mut new_files_by_package: Hash40Map<Vec<&NewFile>> = Hash40Map::default();

        let now = std::time::Instant::now();
        for file in ReadOnlyFileSystem::file_system().new_files.iter() {
            // Just means that the user didn't have a hashes file
            if archive.lookup_file_path(file.filepath.path()).is_some() {
                continue;
            }

            let mut package = None;

            let components = ReadOnlyFileSystem::hashes()
                .components_for(file.filepath.path())
                .unwrap()
                .collect::<Vec<_>>();
            match components[0] {
                "fighter" => {
                    if let Some(fighter_name) = components.get(1) {
                        if let Some(fighter_slot) = components.get(4) {
                            if fighter_slot.starts_with("c") && fighter_slot.len() == 3 {
                                package = Some(
                                    Hash40::const_new("fighter")
                                        .const_with("/")
                                        .const_with(fighter_name)
                                        .const_with("/")
                                        .const_with(fighter_slot),
                                );
                            }
                        }
                    }
                }
                "stage" => {
                    package = Some(file.filepath.parent().const_trim_trailing("/"));
                }
                _ => {}
            }

            if let Some(package) = package {
                let search_path = SearchPath::from_file_path(&file.filepath);

                let new_index = archive.insert_search_path(search_path);

                if let Some(parent) = archive.lookup_search_folder_mut(search_path.parent()) {
                    let mut child = parent.first_child();
                    while !child.is_end() {
                        child = child.next();
                    }

                    child.set_next_index(new_index);
                    new_files_by_package.entry(package).or_default().push(&file);
                }
            }
        }
        println!(
            "[stratus::patching] Built file-addition cache and updated search section in {:.3}s",
            now.elapsed().as_secs_f32()
        );

        let now = std::time::Instant::now();
        for (package_hash, files) in new_files_by_package {
            if files.is_empty() {
                continue;
            }

            let Some(package) = archive.lookup_file_package(package_hash) else {
                continue;
            };

            let should_log = package_hash == Hash40::const_new("fighter/samus/c00");

            let file_info_range = package.infos().range();

            if should_log {
                println!(
                    "Relocating package infos range {:#x} - {:#x}",
                    file_info_range.start, file_info_range.end
                );
            }

            let data_group = package.data_group().index();

            let new_range_start = archive.num_file_info() as u32;
            let new_range_len = (file_info_range.end - file_info_range.start) + files.len() as u32;
            for file_info_idx in file_info_range {
                let info = archive.get_file_info_mut(file_info_idx).unwrap().clone();
                let new_idx = archive.push_file_info(info);
                let mut info = archive.get_file_info_mut(new_idx).unwrap();
                if info.path_ref().entity().info().index() == file_info_idx {
                    info.path_mut().entity_mut().set_info(new_idx);
                }
            }

            for file in files {
                let new_entity_idx =
                    archive.push_file_entity(FileEntity::new(data_group, 0xFFFFFF));
                let mut filepath = file.filepath;
                filepath.path_and_entity.set_data(new_entity_idx);
                let new_file_path = archive.insert_file_path(filepath);
                let new_data = archive.push_file_data(FileData::new_for_unsharing(file.size, 0));
                let new_desc = archive.push_file_desc(FileDescriptor::new(
                    data_group,
                    new_data,
                    FileLoadMethod::Owned(0),
                ));
                let new_info = archive.push_file_info(FileInfo::new(
                    new_file_path,
                    new_entity_idx,
                    new_desc,
                    FileInfoFlags::IS_GRAPHICS_ARCHIVE,
                ));
                archive
                    .get_file_entity_mut(new_entity_idx)
                    .unwrap()
                    .set_info(new_info);
            }

            archive
                .lookup_file_package_mut(package_hash)
                .unwrap()
                .set_info_range(new_range_start, new_range_len);
        }
        println!(
            "[stratus::patching] Added files in {:.3}s",
            now.elapsed().as_secs_f32()
        );

        let now = std::time::Instant::now();
        archive.reserialize();
        println!(
            "[stratus::patching] Rebuilt archive tables in {:.3}s",
            now.elapsed().as_secs_f32()
        );

        // panic!(
        //     "{:#x?}",
        //     archive
        //         .lookup_file_package("fighter/element/c04")
        //         .unwrap()
        //         .data_group()
        // );

        ReadOnlyArchive(archive)
    });
}

#[skyline::main(name = "stratus")]
pub fn main() {
    std::panic::set_hook(Box::new(|info| {
        let location = info.location().unwrap();

        let msg = match info.payload().downcast_ref::<&'static str>() {
            Some(s) => *s,
            None => match info.payload().downcast_ref::<String>() {
                Some(s) => &s[..],
                None => "Box<Any>",
            },
        };

        let err_msg = format!("smashline has panicked: '{}', {}\0", msg, location);
        skyline::error::show_error(
            69,
            "Smashline has panicked! Please open Details and post an issue at https://github.com/HDR-Development/smashline.\0",
            err_msg.as_str(),
        );
    }));

    logger::install_hooks();

    unsafe {
        set_overclock_enabled(true);
        set_cpu_boost_mode(1);
    }

    init_folder();
    init_hashes();
    patch_res_threads();
    let _ = log::set_logger(Box::leak(Box::new(NxKernelLogger::new())));
    unsafe { log::set_max_level_racy(LevelFilter::Info) };

    unsafe {
        set_cpu_boost_mode(0);
    }

    skyline::install_hooks!(
        skip_load_resource_tables,
        skip_load_search_tables,
        initial_loading,
        jemalloc_hook,
        skip_load_hook,
        skip_load_hook_p2,
        observe_decompression,
        observe_res_service_inflate,
        process_single_patched_file_request,
        loading_thread_assign_patched_pointer,
        panic_set_invalid_state,
    );
}
