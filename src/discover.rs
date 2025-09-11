use std::{alloc::Layout, io::{Read, Seek, SeekFrom}, ptr::NonNull};

use camino::{Utf8Path, Utf8PathBuf};
use smash_hash::{Hash40, Hash40Map};

use crate::{
    data::{FilePath, IntoHash, Locale, Region},
    hash_interner::{HashMemorySlab, InternPathResult, InternerCache},
    mount_save::Language,
    HashDisplay, LocalePreferences,
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

#[derive(Debug, Copy, Clone)]
pub enum Regional {
    Region(Region),
    Language(Language),
    Locale(Locale),
}

impl Regional {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Region(region) => region.as_str(),
            Self::Language(language) => language.as_str(),
            Self::Locale(locale) => locale.as_str(),
        }
    }
}

#[derive(Default)]
struct DiscoveredFiles {
    base_file: Option<DiscoveredFile>,
    regions: [Option<DiscoveredFile>; Region::COUNT],
    languages: [Option<DiscoveredFile>; Language::COUNT],
    locales: [Option<DiscoveredFile>; Locale::COUNT],
}

impl DiscoveredFiles {
    fn set_by_regional(
        &mut self,
        file: DiscoveredFile,
        regional: Option<Regional>,
    ) -> Option<DiscoveredFile> {
        match regional {
            Some(Regional::Locale(locale)) => self.locales[locale as usize].replace(file),
            Some(Regional::Language(language)) => self.languages[language as usize].replace(file),
            Some(Regional::Region(region)) => self.regions[region as usize].replace(file),
            None => self.base_file.replace(file),
        }
    }

    fn with_preference(&self, preference: LocalePreferences) -> Option<&DiscoveredFile> {
        self.locales[preference.locale as usize]
            .as_ref()
            .or_else(|| self.languages[preference.language as usize].as_ref())
            .or_else(|| self.regions[preference.region as usize].as_ref())
            .or(self.base_file.as_ref())
    }
}

#[derive(Debug)]
pub struct DiscoveredFile {
    root_idx: u32,
    size: u32,
    regional: Option<Regional>,
    file: LoadableFile,
}

impl DiscoveredFile {
    pub fn compressed_size(&self) -> Option<u32> {
        match &self.file {
            LoadableFile::ZipFile { compressed_size, .. } => {
                Some(*compressed_size)
            }
            _ => None,
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
    files: Hash40Map<DiscoveredFiles>,
    checksum: u32,
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

    pub fn checksum(&self) -> u32 {
        self.checksum
    }

    pub fn get_file(&self, hash: Hash40, preference: LocalePreferences) -> Option<&DiscoveredFile> {
        self.files
            .get(&hash)
            .and_then(|files| files.with_preference(preference))
    }

    #[must_use = "This method can fail if the hash was not found"]
    fn get_full_file_path(&self, hash: Hash40, file: &DiscoveredFile, buffer: &mut String) -> bool {
        use std::fmt::Write;
        buffer.clear();

        let _ = write!(
            buffer,
            "{}/{}",
            self.roots[file.root_idx as usize],
            hash.display()
        );

        if let Some(regional) = file.regional.as_ref() {
            let file_stem_end = buffer.rfind('.').unwrap();
            buffer.insert(file_stem_end, '+');
            buffer.insert_str(file_stem_end + 1, regional.as_str());
        }

        true
    }

    pub fn decompress_loaded_file(
        file: &DiscoveredFile,
        pointer: NonNull<u8>,
        alignment: usize,
    ) -> NonNull<u8> {
        // Check if the uppermost bit is set in the pointer, if it is then we it's compressed and
        // we need to decompress it. This should also cause a memory access violation and crash if
        // we fail to decompress it before providing it to the game.
        if let Some(real_ptr) = Self::into_real_ptr(pointer.as_ptr()) {
            // TODO: Move this out of unwrap
            let compressed_size = file.compressed_size().unwrap();

            let compressed_buffer =
                unsafe { std::slice::from_raw_parts(real_ptr, compressed_size as usize) };

            let decompressed_buffer_layout =
                std::alloc::Layout::from_size_align(file.size as usize, alignment).unwrap();

            let decompressed_ptr = unsafe { std::alloc::alloc(decompressed_buffer_layout) };

            let decompressed_buffer = unsafe {
                std::slice::from_raw_parts_mut(decompressed_ptr, decompressed_buffer_layout.size())
            };

            // TODO: figure out removing unwrap, or don't this is technically user data
            flate2::bufread::DeflateDecoder::new(std::io::Cursor::new(compressed_buffer))
                .read_exact(decompressed_buffer)
                .unwrap();

            unsafe {
                std::alloc::dealloc(
                    real_ptr,
                    std::alloc::Layout::from_size_align_unchecked(compressed_size as usize, 0x1),
                );
            }

            unsafe { NonNull::new_unchecked(decompressed_ptr) }
        } else {
            pointer
        }
    }

    fn load_zip_file(
        path: &Utf8Path,
        compressed_size: u32,
        decompressed_size: u32,
        compressed_start: u32,
        alignment: usize,
    ) -> NonNull<u8> {
        // SAFETY: Compressed size must be <= u32::MAX, and if it's not then we are going
        // to OOM anyways. Realistically I don't think a user is going to do that so I
        // won't bother doing the unwrap panic check here (and if they do the worst that
        // happens is that their game crashes)
        let buffer_layout = unsafe {
            std::alloc::Layout::from_size_align(compressed_size as usize, alignment)
                .unwrap_unchecked()
        };
        let compressed_buffer = unsafe { std::alloc::alloc(buffer_layout) };
        assert!(!compressed_buffer.is_null());

        let slice =
            unsafe { std::slice::from_raw_parts_mut(compressed_buffer, buffer_layout.size()) };

        // TODO: Check if this can be unwrap_unchecked?
        let mut file = std::fs::File::open(path).unwrap();
        file.seek(SeekFrom::Start(compressed_start as u64)).unwrap();
        // let file = zip.get_entry(wayfinder).unwrap();

        file.read_exact(slice).unwrap();

        if compressed_size == decompressed_size {
            unsafe { NonNull::new_unchecked(compressed_buffer) }
        } else {
            unsafe { NonNull::new_unchecked(Self::into_compressed_ptr(compressed_buffer).unwrap()) }
        }
    }

    /// Loads the file, optionally leaving it compressed.
    ///
    /// Leaving it compressed is only going to perform performance if the loading thread is not the
    /// one that needs to do the decompression. In practice, this means that `ResLoadingThread`
    /// will leave the file compressed while `ResInflateThread` will either do the decompression
    /// after receiving the pointer, or it will decompress it while loading
    pub fn load_file(
        &self,
        hash: Hash40,
        file: &DiscoveredFile,
        filepath_buffer: &mut String,
        alignment: usize,
        leave_compressed: bool,
    ) -> NonNull<u8> {
        match &file.file {
            LoadableFile::UncompressedOnDisk => {
                // TODO: Assert?
                assert!(self.get_full_file_path(hash, file, filepath_buffer));

                let size = file.size();
                let buffer_ptr = unsafe {
                    std::alloc::alloc(Layout::from_size_align(size as usize, alignment).unwrap())
                };
                assert!(!buffer_ptr.is_null());

                let buffer = unsafe { std::slice::from_raw_parts_mut(buffer_ptr, size as usize) };
                println!("Loading {filepath_buffer}");

                let mut file = std::fs::File::open(filepath_buffer).unwrap();
                file.read_exact(buffer).unwrap();

                unsafe { NonNull::new_unchecked(buffer_ptr) }
            }
            LoadableFile::ZipFile { compressed_size, decompressed_size, compressed_start } => {
                let ptr = Self::load_zip_file(&self.roots[file.root_idx as usize], *compressed_size, *decompressed_size, *compressed_start, alignment);
                if leave_compressed {
                    ptr
                } else {
                    Self::decompress_loaded_file(file, ptr, alignment)
                }
            }
        }
    }

    pub fn files(
        &self,
        preference: LocalePreferences,
    ) -> impl IntoIterator<Item = (Hash40, &DiscoveredFile)> {
        self.files.iter().filter_map(move |(hash, files)| {
            let file = files.with_preference(preference)?;
            Some((*hash, file))
        })
    }
}

pub enum LoadableFile {
    UncompressedOnDisk,
    ZipFile {
        // zip: &'static rawzip::ZipArchive<rawzip::FileReader>,
        compressed_size: u32,
        decompressed_size: u32,
        compressed_start: u32,
    },
}

impl std::fmt::Debug for LoadableFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LoadableFile")
    }
}

fn detect_regional_and_cache(
    path: &Utf8Path,
    hash: &mut HashMemorySlab,
    cache: &mut InternerCache,
    new_filepath_buffer: &mut String,
) -> (InternPathResult, Option<Regional>) {
    let mut regional = None;
    let mut filepath = path;
    if let Some(file_stem) = path.file_stem() {
        if let Some(pos) = file_stem.find('+') {
            // +xx_yy locale indicator
            if file_stem.len() - pos == 6 {
                let locale = Locale::from_str(&file_stem[pos + 1..]).unwrap_or_else(|| {
                    panic!(
                        "Invalid locale suffix {} in file {}",
                        &file_stem[pos + 1..],
                        path
                    )
                });
                regional = Some(Regional::Locale(locale));
            }
            // +xx region/language indicator
            else if file_stem.len() - pos == 3 {
                let substr = &file_stem[pos + 1..];
                if let Some(language) = Language::from_str(substr) {
                    regional = Some(Regional::Language(language));
                } else if let Some(region) = Region::from_str(substr) {
                    regional = Some(Regional::Region(region));
                } else {
                    panic!("Invalid region/language suffix {} in file {}", substr, path);
                }
            } else {
                panic!("Invalid region/locale indicator in file {}", path);
            }
            new_filepath_buffer.clear();
            if let Some(parent) = path.parent() {
                new_filepath_buffer.push_str(parent.as_str());
                if !new_filepath_buffer.ends_with('/') {
                    new_filepath_buffer.push('/');
                }
            }
            new_filepath_buffer.push_str(&file_stem[..pos]);
            if let Some(extension) = path.extension() {
                new_filepath_buffer.push('.');
                new_filepath_buffer.push_str(extension);
            }
            filepath = Utf8Path::new(new_filepath_buffer);
        }
    }

    if let Some(file_name) = filepath.file_name() {
        hash.intern_path(cache, Utf8Path::new(file_name));
    }
    if let Some(ext) = filepath.extension() {
        hash.intern_path(cache, Utf8Path::new(ext));
    }

    let result = hash.intern_path(cache, filepath);
    (result, regional)
}

pub fn discover_and_update_hashes(
    hash: &mut HashMemorySlab,
    cache: &mut InternerCache,
) -> FileSystem {
    let mods_root = Utf8Path::new(MODS_ROOT);
    let mut file_system = FileSystem {
        roots: vec![],
        files: Hash40Map::default(),
        checksum: 0,
        new_files: vec![],
    };
    let mut modified_filepath_buffer = String::with_capacity(0x180);

    let mut checksum = crc32fast::Hasher::new();

    for folder in mods_root.read_dir_utf8().unwrap() {
        let root = folder.unwrap();

        if root.file_name().starts_with(".") {
            continue;
        }

        let now = std::time::Instant::now();
        if root.file_type().unwrap().is_dir() {
            checksum.update(root.path().as_str().as_bytes());
            let root_idx: u32 = file_system.roots.len().try_into().unwrap();
            file_system.roots.push(root.path().to_path_buf());
            println!("Discovering in {}", root.path());
            discover_and_update_recursive(
                root.path(),
                root.path(),
                &mut |file_path: &Utf8Path, len: u32| {
                    checksum.update(file_path.as_str().as_bytes());
                    checksum.update(&len.to_le_bytes());
                    println!("\tDiscovered {file_path}");
                    let (intern_result, regional) = detect_regional_and_cache(
                        file_path,
                        hash,
                        cache,
                        &mut modified_filepath_buffer,
                    );

                    if intern_result.is_new {
                        assert!(regional.is_none(), "New files cannot be regional: {}", file_path);
                        file_system.new_files.push(NewFile {
                            filepath: FilePath::from_utf8_path(file_path),
                            size: len,
                        })
                    }

                    let path = if regional.is_some() {
                        Utf8Path::new(&modified_filepath_buffer)
                    } else {
                        file_path
                    };

                    let previous = file_system
                        .files
                        .entry(path.into_hash())
                        .or_default()
                        .set_by_regional(
                            DiscoveredFile {
                                root_idx,
                                size: len,
                                regional,
                                file: LoadableFile::UncompressedOnDisk,
                            },
                            regional,
                        );

                    if let Some(previous) = previous {
                        panic!("Duplicate file discovered {}", file_path);
                    }
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

            file_system.files.reserve(archive.entries_hint() as usize);

            let mut entries_iter = archive.entries(&mut buffer);
            while let Some(entry) = entries_iter.next_entry().unwrap() {
                if entry.is_dir() {
                    continue;
                }
                let path = Utf8Path::new(unsafe {
                    std::str::from_utf8_unchecked(entry.file_path().as_bytes())
                });

                if let Some(extension) = path.extension() {
                    match extension {
                        "prcxml" => continue,
                        "xmsbt" => continue,
                        "prcx" => continue,
                        _ => {}
                    }
                }

                let size = entry.uncompressed_size_hint() as u32;
                checksum.update(path.as_str().as_bytes());
                checksum.update(&size.to_le_bytes());
                let (intern_result, regional) =
                    detect_regional_and_cache(path, hash, cache, &mut modified_filepath_buffer);

                if intern_result.is_new {
                    assert!(regional.is_none(), "New files cannot be regional");
                    file_system.new_files.push(NewFile {
                        filepath: FilePath::from_utf8_path(path),
                        size,
                    })
                }

                let path = if regional.is_some() {
                    Utf8Path::new(&modified_filepath_buffer)
                } else {
                    path
                };

                let wayfinder = entry.wayfinder();
                let file = archive.get_entry(wayfinder).unwrap();
                let file = LoadableFile::ZipFile { compressed_size: wayfinder.compressed_size_hint() as u32, decompressed_size: wayfinder.uncompressed_size_hint() as u32, compressed_start: file.compressed_data_range().0 as u32 };

                let previous = file_system
                    .files
                    .entry(path.into_hash())
                    .or_default()
                    .set_by_regional(
                        DiscoveredFile {
                            root_idx,
                            size,
                            regional,
                            file
                        },
                        regional,
                    );

                if let Some(previous) = previous {
                    panic!("Duplicate file discovered {}", path);
                }
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

    file_system.checksum = checksum.finalize();

    file_system
}
