use std::{alloc::Layout, io::Read, ptr::NonNull};

use camino::{Utf8Path, Utf8PathBuf};
use smash_hash::{Hash40, Hash40Map};

use crate::{
    data::{FilePath, IntoHash},
    hash_interner::{HashMemorySlab, InternerCache},
    HashDisplay,
};

const MODS_ROOT: &str = "sd:/ultimate/mods/";

fn discover_and_update_recursive(
    root: &Utf8Path,
    folder: &Utf8Path,
    add_path: &mut dyn FnMut(&Utf8Path, u32),
) {
    for entry in folder.read_dir_utf8().unwrap() {
        let entry = entry.unwrap();

        if entry.file_type().unwrap().is_file() {
            let size: u32 = std::fs::metadata(entry.path())
                .unwrap()
                .len()
                .try_into()
                .unwrap();
            let smash_path = entry.path().strip_prefix(root).unwrap();
            if smash_path.as_str().len() >= 256 {
                panic!("Cannot discover path with length greater than 256: '{smash_path}'");
            }

            add_path(smash_path, size);
        } else {
            discover_and_update_recursive(root, entry.path(), add_path);
        }
    }
}

pub struct DiscoveredFile {
    root_idx: u32,
    size: u32,
    file: LoadableFile,
}

impl DiscoveredFile {
    pub fn compressed_size(&self) -> Option<u32> {
        match &self.file {
            LoadableFile::ZipFile { wayfinder, .. } => Some(wayfinder.compressed_size_hint() as u32),
            _ => None
        }
    }

    pub fn size(&self) -> u32 {
        self.size
    }
}

pub struct NewFile {
    pub filepath: FilePath,
    pub size: u32,
}

pub struct FileSystem {
    roots: Vec<Utf8PathBuf>,
    files: Hash40Map<DiscoveredFile>,
    pub new_files: Vec<NewFile>,
}

impl FileSystem {
    const COMPRESSED_PTR_BIT: u64 = 1u64 << 63;

    fn into_compressed_ptr(real_ptr: *mut u8) -> Option<*mut u8> {
        if (real_ptr as u64) & Self::COMPRESSED_PTR_BIT != 0 {
            // Maybe panic here instead
            None
        } else {
            Some(((real_ptr as u64) | Self::COMPRESSED_PTR_BIT) as *mut u8)
        }
    }

    fn into_real_ptr(compressed_ptr: *mut u8) -> Option<*mut u8> {
        if (compressed_ptr as u64) & Self::COMPRESSED_PTR_BIT != 0 {
            Some(((compressed_ptr as u64) & !Self::COMPRESSED_PTR_BIT) as *mut u8)
        } else {
            None
        }
    }

    pub fn get_file(&self, hash: Hash40) -> Option<&DiscoveredFile> {
        self.files.get(&hash)
    }

    #[must_use = "This method can fail if the hash was not found"]
    pub fn get_full_file_path(&self, hash: Hash40, buffer: &mut String) -> bool {
        use std::fmt::Write;
        buffer.clear();

        let Some(file) = self.files.get(&hash) else {
            return false;
        };
        let _ = write!(buffer, "{}/{}", self.roots[file.root_idx as usize], hash.display());

        true
    }

    pub fn decompress_loaded_file(file: &DiscoveredFile, pointer: NonNull<u8>) -> NonNull<u8> {
        // Check if the uppermost bit is set in the pointer, if it is then we it's compressed and
        // we need to decompress it. This should also cause a memory access violation and crash if
        // we fail to decompress it before providing it to the game.
        if let Some(real_ptr) = Self::into_real_ptr(pointer.as_ptr()) {
            // TODO: Move this out of unwrap
            let compressed_size = file.compressed_size().unwrap();

            let compressed_buffer = unsafe {
                std::slice::from_raw_parts(real_ptr, compressed_size as usize)
            };

            let decompressed_buffer_layout = unsafe {
                // TODO: We default to page alignment here but we could probably be more optimal
                std::alloc::Layout::from_size_align_unchecked(file.size as usize, 0x1000)
            };

            let decompressed_ptr = unsafe {
                std::alloc::alloc(decompressed_buffer_layout)
            };

            let decompressed_buffer = unsafe {
                std::slice::from_raw_parts_mut(decompressed_ptr, decompressed_buffer_layout.size())
            };

            // TODO: figure out removing unwrap, or don't this is technically user data
            flate2::bufread::DeflateDecoder::new(std::io::Cursor::new(compressed_buffer)).read_exact(decompressed_buffer).unwrap();

            unsafe {
                std::alloc::dealloc(real_ptr, std::alloc::Layout::from_size_align_unchecked(compressed_size as usize, 0x1));
            }

            unsafe {
                NonNull::new_unchecked(decompressed_ptr)
            }
        } else {
            pointer
        }
    }

    fn load_zip_file(zip: &rawzip::ZipArchive<rawzip::FileReader>, wayfinder: rawzip::ZipArchiveEntryWayfinder) -> NonNull<u8> {
        // SAFETY: Compressed size must be <= u32::MAX, and if it's not then we are going
        // to OOM anyways. Realistically I don't think a user is going to do that so I
        // won't bother doing the unwrap panic check here (and if they do the worst that
        // happens is that their game crashes)
        let buffer_layout = unsafe { std::alloc::Layout::from_size_align(wayfinder.compressed_size_hint() as usize, 0x1).unwrap_unchecked() };
        let compressed_buffer = unsafe { std::alloc::alloc(buffer_layout) };
        assert!(!compressed_buffer.is_null());

        let slice = unsafe {
            std::slice::from_raw_parts_mut(compressed_buffer, buffer_layout.size())
        };

        // TODO: Check if this can be unwrap_unchecked?
        let file = zip.get_entry(wayfinder).unwrap();

        file.reader().read_exact(slice).unwrap();

        unsafe { NonNull::new_unchecked(Self::into_compressed_ptr(compressed_buffer).unwrap()) }
    }

    /// Loads the file, optionally leaving it compressed.
    ///
    /// Leaving it compressed is only going to perform performance if the loading thread is not the
    /// one that needs to do the decompression. In practice, this means that `ResLoadingThread`
    /// will leave the file compressed while `ResInflateThread` will either do the decompression
    /// after receiving the pointer, or it will decompress it while loading
    pub fn load_file(&self, hash: Hash40, file: &DiscoveredFile, filepath_buffer: &mut String, leave_compressed: bool) -> NonNull<u8> {
        match &file.file {
            LoadableFile::UncompressedOnDisk => {
                // TODO: Assert?
                assert!(self.get_full_file_path(hash, filepath_buffer));

                let size = file.size();
                let buffer_ptr = unsafe {
                    std::alloc::alloc(Layout::from_size_align_unchecked(size as usize, 0x1000))
                };
                assert!(!buffer_ptr.is_null());

                let buffer = unsafe {
                    std::slice::from_raw_parts_mut(buffer_ptr, size as usize)
                };

                let mut file = std::fs::File::open(filepath_buffer).unwrap();
                file.read_exact(buffer).unwrap();

                unsafe { NonNull::new_unchecked(buffer_ptr) }
            },
            LoadableFile::ZipFile { zip, wayfinder } => {
                let ptr = Self::load_zip_file(zip, *wayfinder);
                if leave_compressed {
                    ptr
                } else {
                    Self::decompress_loaded_file(file, ptr)
                }
            }
        }
    }

    pub fn files(&self) -> impl IntoIterator<Item = (&Hash40, &DiscoveredFile)> {
        self.files.iter()
    }
}

pub enum LoadableFile {
    UncompressedOnDisk,
    ZipFile {
        zip: &'static rawzip::ZipArchive<rawzip::FileReader>,
        wayfinder: rawzip::ZipArchiveEntryWayfinder
    },
}

pub fn discover_and_update_hashes(
    hash: &mut HashMemorySlab,
    cache: &mut InternerCache,
) -> FileSystem {
    let mods_root = Utf8Path::new(MODS_ROOT);
    let mut file_system = FileSystem {
        roots: vec![],
        files: Hash40Map::default(),
        new_files: vec![],
    };

    for folder in mods_root.read_dir_utf8().unwrap() {
        let root = folder.unwrap();

        if root.file_name().starts_with(".") {
            continue;
        }

        let now = std::time::Instant::now();
        if root.file_type().unwrap().is_dir() {
            let root_idx: u32 = file_system.roots.len().try_into().unwrap();
            file_system
                .roots
                .push(root.path().to_path_buf());
            discover_and_update_recursive(
                root.path(),
                root.path(),
                &mut |file_path: &Utf8Path, len: u32| {
                    if let Some(file_name) = file_path.file_name() {
                        hash.intern_path(cache, Utf8Path::new(file_name));
                    }
                    if let Some(ext) = file_path.extension() {
                        hash.intern_path(cache, Utf8Path::new(ext));
                    }
                    if hash.intern_path(cache, file_path).is_new {
                        file_system.new_files.push(NewFile {
                            filepath: FilePath::from_utf8_path(file_path),
                            size: len,
                        });
                    }

                    file_system.files.insert(
                        file_path.into_hash(),
                        DiscoveredFile {
                            root_idx,
                            size: len,
                            file: LoadableFile::UncompressedOnDisk,
                        },
                    );
                },
            );
        } else if root.file_type().unwrap().is_file() && root.path().extension() == Some("zip") {
            let root_idx: u32 = file_system.roots.len().try_into().unwrap();
            let now = std::time::Instant::now();
            let mut buffer = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
            let archive = rawzip::ZipArchive::from_file(
                std::fs::File::open(root.path()).unwrap(),
                &mut buffer,
            )
            .unwrap();

            let archive: &'static rawzip::ZipArchive<rawzip::FileReader> = Box::leak(Box::new(archive));

            file_system.files.reserve(archive.entries_hint() as usize);

            let mut entries_iter = archive.entries(&mut buffer);
            while let Some(entry) = entries_iter.next_entry().unwrap() {
                if entry.is_dir() {
                    continue;
                }
                let path = Utf8Path::new(unsafe {
                    std::str::from_utf8_unchecked(entry.file_path().as_bytes())
                });
                if let Some(file_name) = path.file_name() {
                    hash.intern_path(cache, Utf8Path::new(file_name));
                }
                if let Some(ext) = path.extension() {
                    hash.intern_path(cache, Utf8Path::new(ext));
                }

                let size = entry.uncompressed_size_hint() as u32;

                if hash.intern_path(cache, path).is_new {
                    file_system.new_files.push(NewFile {
                        filepath: FilePath::from_utf8_path(path),
                        size,
                    });
                }

                file_system.files.insert(
                    path.into_hash(),
                    DiscoveredFile {
                        root_idx,
                        size,
                        file: LoadableFile::ZipFile {
                            zip: archive,
                            wayfinder: entry.wayfinder(),
                        }
                    },
                );
            }
            println!(
                "Processing rawzip took: {:.3}ms",
                now.elapsed().as_micros() as f32 / 1000.0
            );

            file_system.roots.push(root.path().to_path_buf());
        }

        println!(
            "[stratus::discovery] Discovered {} in {:.3}s",
            root.path(),
            now.elapsed().as_secs_f32()
        );
    }

    file_system
}
