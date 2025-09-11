use std::{alloc::Layout, hint::unreachable_unchecked, io::{Read, Seek, SeekFrom}, ptr::NonNull};

use bytemuck::{Pod, Zeroable};
use camino::{Utf8Path, Utf8PathBuf};
use rawzip::RECOMMENDED_BUFFER_SIZE;
use smash_hash::{Hash40, Hash40Map};

use crate::{data::{IntoHash, Locale, Region}, hash_interner::{HashMemorySlab, InternPathResult, InternerCache}, mount_save::Language, HashDisplay, LocalePreferences};

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
struct DiscoveredFilesystemHeader {
    checksum: u32,
    root_byte_len: u32,
    roots: u32,
    paths: u32,
    uncompressed_files: u32,
    compressed_files: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
struct Root {
    byte_start: u32,
    byte_count: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
struct FileHeader {
    start: u32,
    num_files: u32,
}

#[derive(Debug, Copy, Clone)]
enum Regionalized {
    None,
    Locale(u8),
    Language(u8),
    Region(u8),
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
struct FileIndex(u32);

impl FileIndex {
    const REGION_LOCALE_LANGUAGE_TY: u32 = 0xC0000000;
    const REGION_LOCALE_LANGUAGE_IDX: u32 = 0x3F000000;
    const IS_COMPRESSED: u32 = 0x00800000;
    const INDEX: u32 = 0x007FFFFF;

    const fn get_regionalized(&self) -> Regionalized {
        let ty_bits = ((self.0 & Self::REGION_LOCALE_LANGUAGE_TY) >> 30) as u8;
        let idx_bits = ((self.0 & Self::REGION_LOCALE_LANGUAGE_IDX) >> 24) as u8;
        match ty_bits {
            0b00 => Regionalized::None,
            0b01 => Regionalized::Locale(idx_bits),
            0b10 => Regionalized::Language(idx_bits),
            0b11 => Regionalized::Region(idx_bits),
            _ => unsafe {
                unreachable_unchecked()
            }
        }
    }

    const fn is_compressed(&self) -> bool {
        (self.0 & Self::IS_COMPRESSED) != 0
    }

    const fn index(&self) -> u32 {
        self.0 & Self::INDEX
    }

    const fn from_parts(regionalized: Regionalized, compressed: bool, index: u32) -> Self {
        let (ty, idx) = match regionalized {
            Regionalized::None => (0, 0),
            Regionalized::Locale(idx) => (1, idx),
            Regionalized::Language(idx) => (2, idx),
            Regionalized::Region(idx) => (3, idx)
        };

        Self(
            (((ty as u32) & 0x3) << 30)
                | (((idx as u32) & 0x3F) << 24)
                | (((compressed as u32) & 0x1) << 23)
                | (index & Self::INDEX)
        )
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct File {
    root: u32,
    index: FileIndex,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
struct UncompressedFile {
    size: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
struct CompressedFile {
    compressed_start: u32,
    compressed_size: u32,
    decompressed_size: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
struct HashedFile([u32; 2]);

impl HashedFile {
    const HASH_BITS:      u64 = 0xFF_FFFFFFFF;
    const INDEX_BITS:     u64 = 0xFFFFFF << 40;

    const fn raw(&self) -> u64 {
        (self.0[0] as u64) | ((self.0[1] as u64) << 32)
    }

    pub const fn new(path: Hash40, index: u32) -> Self {
        let raw = path.raw() | ((index as u64) << 40);
        Self([(raw & 0xFFFFFFFF) as u32, ((raw & 0xFFFFFFFF_00000000) >> 32) as u32])
    }

    pub const fn path(&self) -> Hash40 {
        Hash40::from_raw(self.raw() & Self::HASH_BITS)
    }
    
    pub const fn index(&self) -> u32 {
        ((self.raw() & Self::INDEX_BITS) >> 40) as u32
    }
}

pub struct FileSystem {
    raw: Box<[u8]>,
    header: DiscoveredFilesystemHeader,
    roots: *const [Root],
    root_bytes: *const [u8],
    lookup: *const [HashedFile],
    file_headers: *const [FileHeader],
    files: *const [File],
    uncompressed: *const [UncompressedFile],
    compressed: *const [CompressedFile],
}

impl FileSystem {
    pub fn checksum(&self) -> u32 {
        self.header.checksum
    }

    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    #[allow(dead_code)]
    pub fn iter_file_paths(&self, preferences: LocalePreferences) -> impl Iterator<Item = Hash40> + '_ {
        unsafe {
            (*self.lookup)
                .iter()
                .filter(move |hashed_file| {
                    self.get_file_by_header(hashed_file.index(), preferences).is_some()
                })
                .map(|hashed_file| hashed_file.path())
        }
    }

    pub fn iter_file_sizes<'a>(
        &'a self,
        preferences: LocalePreferences,
    ) -> impl Iterator<Item = (Hash40, u32)> + 'a {
        unsafe {
            (*self.lookup)
                .iter()
                .filter_map(move |hashed_file| {
                    self.get_file_by_header(hashed_file.index(), preferences).map(|file| {
                        let size = if file.index.is_compressed() {
                            (*self.compressed)[file.index.index() as usize].decompressed_size
                        } else {
                            (*self.uncompressed)[file.index.index() as usize].size 
                        };

                        (hashed_file.path(), size)
                    })
                })
        }
    }

    pub fn from_bytes(bytes: Box<[u8]>) -> Self {
        let header: DiscoveredFilesystemHeader = *bytemuck::from_bytes(&bytes[..std::mem::size_of::<DiscoveredFilesystemHeader>()]);
        let mut cursor = std::mem::size_of::<DiscoveredFilesystemHeader>();
        let lookup: *const [HashedFile] = bytemuck::cast_slice(&bytes[cursor..cursor + header.paths as usize * std::mem::size_of::<HashedFile>()]);
        cursor += header.paths as usize * std::mem::size_of::<HashedFile>();

        let file_headers: *const [FileHeader] = bytemuck::cast_slice(&bytes[cursor..cursor + header.paths as usize * std::mem::size_of::<FileHeader>()]);
        cursor += header.paths as usize * std::mem::size_of::<FileHeader>();

        let files: *const [File] = bytemuck::cast_slice(&bytes[cursor..cursor + (header.uncompressed_files + header.compressed_files) as usize * std::mem::size_of::<File>()]);
        cursor += (header.uncompressed_files + header.compressed_files) as usize * std::mem::size_of::<File>();

        let uncompressed: *const [UncompressedFile] = bytemuck::cast_slice(&bytes[cursor..cursor + header.uncompressed_files as usize * std::mem::size_of::<UncompressedFile>()]);
        cursor += header.uncompressed_files as usize * std::mem::size_of::<UncompressedFile>();

        let compressed: *const [CompressedFile] = bytemuck::cast_slice(&bytes[cursor..cursor + header.compressed_files as usize * std::mem::size_of::<CompressedFile>()]);
        cursor += header.compressed_files as usize * std::mem::size_of::<CompressedFile>();

        let roots: *const [Root] = bytemuck::cast_slice(&bytes[cursor..cursor + header.roots as usize * std::mem::size_of::<Root>()]);
        cursor += header.roots as usize * std::mem::size_of::<Root>();

        let root_bytes = &raw const bytes[cursor..cursor + header.root_byte_len as usize];
        cursor += header.root_byte_len as usize;
        assert_eq!(cursor, bytes.len());

        Self {
            raw: bytes,
            header,
            roots,
            root_bytes,
            file_headers,
            files,
            lookup,
            uncompressed,
            compressed
        }
    }

    pub fn get_decompressed_size(&self, file: &File) -> u32 {
        if file.index.is_compressed() {
            unsafe {
                (*self.compressed)[file.index.index() as usize].decompressed_size
            }
        } else {
            unsafe {
                (*self.uncompressed)[file.index.index() as usize].size
            }
        }
    }

    fn get_file_by_header(&self, header_idx: u32, preferences: LocalePreferences) -> Option<&File> {
        let header = unsafe { (*self.file_headers)[header_idx as usize] };
        let files = unsafe { &(&*self.files)[header.start as usize..(header.start + header.num_files) as usize] };

        let mut locale_match = None;
        let mut language_match = None;
        let mut region_match = None;
        let mut base_match = None;

        for file in files {
            match file.index.get_regionalized() {
                Regionalized::None => base_match = Some(file),
                Regionalized::Locale(idx) if preferences.locale as u8 == idx => locale_match = Some(file),
                Regionalized::Language(idx) if preferences.language as u8 == idx => language_match = Some(file), 
                Regionalized::Region(idx) if preferences.region as u8 == idx => region_match = Some(file),
                _ => {}
            }
        }

        locale_match.or(language_match).or(region_match).or(base_match)
    }

    pub fn lookup_file(
        &self, 
        hash: Hash40,
        preferences: LocalePreferences,
    ) -> Option<&File> {
        let file_header_index = unsafe {
            let index = (&*self.lookup).binary_search_by_key(&hash, |file| file.path()).ok()?;
            (*self.lookup)[index].index()
        };

        self.get_file_by_header(file_header_index, preferences)
    }

    fn get_root(&self, root_idx: u32) -> &str {
        let root = unsafe { (&*self.roots)[root_idx as usize] };
        unsafe {
            std::str::from_utf8_unchecked(&(&*self.root_bytes)[root.byte_start as usize..(root.byte_start + root.byte_count) as usize])
        }
    }

    const COMPRESSED_PTR_BIT: u64 = 1 << 63;

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

    pub fn decompress_file(&self, file: &File, pointer: NonNull<u8>, alignment: usize) -> NonNull<u8> {
        // Check if the uppermost bit is set in the pointer, if it is then we it's compressed and
        // we need to decompress it. This should also cause a memory access violation and crash if
        // we fail to decompress it before providing it to the game.
        if let Some(real_ptr) = Self::into_real_ptr(pointer.as_ptr()) {
            // TODO: Move this out of unwrap
            let compressed_file = unsafe { (*self.compressed)[file.index.index() as usize] };

            let compressed_buffer =
                unsafe { std::slice::from_raw_parts(real_ptr, compressed_file.compressed_size as usize) };

            let decompressed_buffer_layout =
                std::alloc::Layout::from_size_align(compressed_file.decompressed_size as usize, alignment).unwrap();

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
                    std::alloc::Layout::from_size_align_unchecked(compressed_file.compressed_size as usize, alignment),
                );
            }

            unsafe { NonNull::new_unchecked(decompressed_ptr) }
        } else {
            pointer
        }
    }

    fn read_zip_file(
        &self,
        root: u32,
        file: &CompressedFile,
        alignment: usize
    ) -> NonNull<u8> {
        // SAFETY: Compressed size must be <= u32::MAX, and if it's not then we are going
        // to OOM anyways. Realistically I don't think a user is going to do that so I
        // won't bother doing the unwrap panic check here (and if they do the worst that
        // happens is that their game crashes)
        let buffer_layout = unsafe {
            std::alloc::Layout::from_size_align(file.compressed_size as usize, alignment)
                .unwrap_unchecked()
        };
        let compressed_buffer = unsafe { std::alloc::alloc(buffer_layout) };
        assert!(!compressed_buffer.is_null());

        let slice =
            unsafe { std::slice::from_raw_parts_mut(compressed_buffer, buffer_layout.size()) };

        let root = self.get_root(root);

        // TODO: Check if this can be unwrap_unchecked?
        let mut zip_file = std::fs::File::open(root).unwrap();
        zip_file.seek(SeekFrom::Start(file.compressed_start as u64)).unwrap();
        // let file = zip.get_entry(wayfinder).unwrap();

        zip_file.read_exact(slice).unwrap();

        if file.compressed_size == file.decompressed_size {
            unsafe { NonNull::new_unchecked(compressed_buffer) }
        } else {
            unsafe { NonNull::new_unchecked(Self::into_compressed_ptr(compressed_buffer).unwrap()) }
        }

    }

    pub fn read_file(&self, hash: Hash40, file: &File, filepath_buffer: &mut String, leave_compressed: bool, alignment: usize) -> NonNull<u8> {
        use std::fmt::Write;
        let root = self.get_root(file.root);

        if file.index.is_compressed() {
            let compressed_file = unsafe { &(&*self.compressed)[file.index.index() as usize] };
            let ptr = self.read_zip_file(file.root, compressed_file, alignment);
            if leave_compressed {
                ptr
            } else {
                self.decompress_file(file, ptr, alignment)
            }
        } else {
            filepath_buffer.clear();
            let _ = write!(filepath_buffer, "{root}/{}", hash.display());
            let uncompressed_file = unsafe { (*self.uncompressed)[file.index.index() as usize] };

            let buffer = unsafe {
                std::slice::from_raw_parts_mut(std::alloc::alloc(Layout::from_size_align(uncompressed_file.size as usize, alignment).unwrap()), uncompressed_file.size as usize)
            };

            let mut file = std::fs::File::open(&filepath_buffer).unwrap();
            file.read_exact(buffer).unwrap();
            unsafe { NonNull::new_unchecked(buffer.as_mut_ptr()) }
        }
    }
}

enum FileKind {
    Uncompressed {
        size: u32,
    },
    Compressed {
        start: u32,
        compressed_size: u32,
        decompressed_size: u32,
    }
}

struct DiscoveredFile {
    root_index: u32,
    regionalized: Regionalized,
    kind: FileKind,
}

#[derive(Default)]
struct DiscoveredFiles {
    base: Option<DiscoveredFile>,
    by_locale: [Option<DiscoveredFile>; 14],
    by_language: [Option<DiscoveredFile>; 10],
    by_region: [Option<DiscoveredFile>; 5],
}

impl DiscoveredFiles {
    fn iter_priority_ordered(&self) -> impl Iterator<Item = &DiscoveredFile> {
        self.by_locale
            .iter()
            .filter_map(|item| item.as_ref())
            .chain(
                self.by_language
                    .iter()
                    .filter_map(|item| item.as_ref())
            )
            .chain(
                self.by_region
                    .iter()
                    .filter_map(|item| item.as_ref())
            )
            .chain(
                self.base.iter()
            )
    }

    fn set_by_regionalized(&mut self, file: DiscoveredFile, regionalized: Regionalized) -> Option<DiscoveredFile> {
        match regionalized {
            Regionalized::None => self.base.replace(file),
            Regionalized::Locale(idx) => self.by_locale[idx as usize].replace(file),
            Regionalized::Language(idx) => self.by_language[idx as usize].replace(file),
            Regionalized::Region(idx) => self.by_region[idx as usize].replace(file)
        }
    }
}

pub struct Discovery {
    compressed_files: usize,
    uncompressed_files: usize,
    roots: Vec<Utf8PathBuf>,
    files: Hash40Map<DiscoveredFiles>,
    checksum: u32,
}

fn detect_regional_and_cache(
    path: &Utf8Path,
    hash: &mut HashMemorySlab,
    cache: &mut InternerCache,
    new_filepath_buffer: &mut String,
) -> (InternPathResult, Regionalized) {
    let mut regional = Regionalized::None;
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
                regional = Regionalized::Locale(locale as u8);
            }
            // +xx region/language indicator
            else if file_stem.len() - pos == 3 {
                let substr = &file_stem[pos + 1..];
                if let Some(language) = Language::from_str(substr) {
                    regional = Regionalized::Language(language as u8);
                } else if let Some(region) = Region::from_str(substr) {
                    regional = Regionalized::Region(region as u8);
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

impl Discovery {
    pub fn as_slab(&self) -> Box<[u8]> {
        let root_byte_len = self.roots.iter().map(|root| root.as_str().len()).sum::<usize>();
        let total_memory_size = std::mem::size_of::<DiscoveredFilesystemHeader>()
            + std::mem::size_of::<HashedFile>() * self.files.len()
            + std::mem::size_of::<FileHeader>() * self.files.len()
            + std::mem::size_of::<File>() * (self.compressed_files + self.uncompressed_files)
            + std::mem::size_of::<CompressedFile>() * self.compressed_files
            + std::mem::size_of::<UncompressedFile>() * self.uncompressed_files
            + std::mem::size_of::<Root>() * self.roots.len()
            + root_byte_len;

        let slab = unsafe {
            std::slice::from_raw_parts_mut(
                std::alloc::alloc(Layout::from_size_align(total_memory_size, 0x10).unwrap()),
                total_memory_size
            )
        };

        let header = DiscoveredFilesystemHeader {
            checksum: self.checksum,
            root_byte_len: root_byte_len as u32,
            roots: self.roots.len() as u32,
            paths: self.files.len() as u32,
            uncompressed_files: self.uncompressed_files as u32,
            compressed_files: self.compressed_files as u32
        };

        let (header_bytes, remainder) = slab.split_at_mut(std::mem::size_of::<DiscoveredFilesystemHeader>());
        let (hashes, remainder) = remainder.split_at_mut(std::mem::size_of::<HashedFile>() * header.paths as usize);
        let (file_headers, remainder) = remainder.split_at_mut(std::mem::size_of::<FileHeader>() * header.paths as usize);
        let (files, remainder) = remainder.split_at_mut(std::mem::size_of::<File>() * (header.compressed_files + header.uncompressed_files) as usize);
        let (uncompressed_files, remainder) = remainder.split_at_mut(std::mem::size_of::<UncompressedFile>() * header.uncompressed_files as usize);
        let (compressed_files, remainder) = remainder.split_at_mut(std::mem::size_of::<CompressedFile>() * header.compressed_files as usize);
        let (roots, root_bytes) = remainder.split_at_mut(std::mem::size_of::<Root>() * header.roots as usize);
        assert_eq!(root_bytes.len(), header.root_byte_len as usize);

        let hashes: &mut [HashedFile] = bytemuck::cast_slice_mut(hashes);
        let file_headers: &mut [FileHeader] = bytemuck::cast_slice_mut(file_headers);
        let files: &mut [File] = bytemuck::cast_slice_mut(files);
        let uncompressed_files: &mut [UncompressedFile] = bytemuck::cast_slice_mut(uncompressed_files);
        let compressed_files: &mut [CompressedFile] = bytemuck::cast_slice_mut(compressed_files);
        let roots: &mut [Root] = bytemuck::cast_slice_mut(roots);

        header_bytes.copy_from_slice(bytemuck::bytes_of(&header));
        let mut file_cursor = 0;
        let mut uncompressed_cursor = 0;
        let mut compressed_cursor = 0;

        for (idx, (path, file)) in self.files.iter().enumerate() {
            hashes[idx] = HashedFile::new(*path, idx as u32);

            let start_file = file_cursor;
            let mut num_files = 0;

            for file in file.iter_priority_ordered() {
                num_files += 1;
                let index = match file.kind {
                    FileKind::Compressed { start, compressed_size, decompressed_size } => {
                        compressed_files[compressed_cursor] = CompressedFile {
                            compressed_start: start,
                            compressed_size,
                            decompressed_size
                        };
                        compressed_cursor += 1;
                        FileIndex::from_parts(file.regionalized, true, compressed_cursor as u32 - 1)
                    },
                    FileKind::Uncompressed { size } => {
                        uncompressed_files[uncompressed_cursor] = UncompressedFile {
                            size
                        };
                        uncompressed_cursor += 1;
                        FileIndex::from_parts(file.regionalized, false, uncompressed_cursor as u32 - 1)
                    }
                };

                files[file_cursor] = File {
                    root: file.root_index,
                    index
                };

                file_cursor += 1;
            }

            file_headers[idx] = FileHeader {
                start: start_file as u32,
                num_files,
            };
        }

        hashes.sort_unstable_by_key(|a| a.path());

        let mut root_byte_cursor = 0;

        for (idx, root) in self.roots.iter().enumerate() {
            let bytes = root.as_str().as_bytes();
            roots[idx] = Root {
                byte_start: root_byte_cursor as u32,
                byte_count: bytes.len() as u32
            };
            root_bytes[root_byte_cursor..root_byte_cursor + bytes.len()].copy_from_slice(bytes);

            root_byte_cursor += bytes.len();
        }

        unsafe { Box::from_raw(slab) }
    }

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
                Self::discover_and_update_recursive(root, entry.path(), add_path);
            }
        }
    }

    pub fn new_in_root(root: &Utf8Path, hashes: &mut HashMemorySlab, cache: &mut InternerCache) -> Self {
        let mut zip_buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
        let mut filepath_buffer = String::with_capacity(0x180);

        let mut roots = vec![];
        let mut files: Hash40Map<DiscoveredFiles> = Hash40Map::default();
        let mut compressed_files = 0;
        let mut uncompressed_files = 0;
        let mut checksum = crc32fast::Hasher::new();
        for entry in root.read_dir_utf8().unwrap() {
            let entry = entry.unwrap();

            if entry.file_name().starts_with(".") {
                continue;
            }

            let ft = entry.file_type().unwrap();

            let path = entry.path();

            let root_idx = roots.len() as u32;
            if ft.is_dir() {
                checksum.update(path.as_str().as_bytes());
                roots.push(path.to_path_buf());
                Self::discover_and_update_recursive(
                    path,
                    path,
                    &mut |file_path: &Utf8Path, len: u32| {
                        checksum.update(file_path.as_str().as_bytes());
                        checksum.update(&len.to_le_bytes());
                        println!("\tDiscovered {file_path}");
                        let (_, regional) = detect_regional_and_cache(
                            file_path,
                            hashes,
                            cache,
                            &mut filepath_buffer,
                        );

                        let path = if matches!(&regional, Regionalized::None) {
                            file_path
                        } else {
                            Utf8Path::new(&filepath_buffer)
                        };

                        uncompressed_files += 1;
                        files
                            .entry(path.into_hash())
                            .or_default()
                            .set_by_regionalized(
                                DiscoveredFile {
                                    root_index: root_idx,
                                    regionalized: regional,
                                    kind: FileKind::Uncompressed { size: len },
                                },
                                regional,
                            );
                    },
                );
            } else if ft.is_file() && entry.file_name().ends_with(".zip") {
                checksum.update(path.as_str().as_bytes());
                roots.push(path.to_path_buf());
                let zip = rawzip::ZipArchive::from_file(std::fs::File::open(path).unwrap(), &mut zip_buffer).unwrap();

                let mut entries = zip.entries(&mut zip_buffer);
                while let Some(next) = entries.next_entry().unwrap() {
                    if next.is_dir() {
                        continue;
                    }

                    let fp = unsafe { std::str::from_utf8_unchecked(next.file_path().as_bytes()) };

                    let wayfinder = next.wayfinder();
                    checksum.update(fp.as_bytes());
                    checksum.update(&(wayfinder.uncompressed_size_hint() as u32).to_le_bytes());
                    let file = zip.get_entry(wayfinder).unwrap();

                    let regional = detect_regional_and_cache(Utf8Path::new(fp), hashes, cache, &mut filepath_buffer).1;

                    let path = if matches!(&regional, Regionalized::None) {
                        Utf8Path::new(fp)
                    } else {
                        Utf8Path::new(&filepath_buffer)
                    };
                    // let hash = Hash40::const_new(fp);

                    files.entry(path.into_hash()).or_default().set_by_regionalized(DiscoveredFile {
                        root_index: root_idx,
                        regionalized: regional,
                        kind: FileKind::Compressed {
                            start: file.compressed_data_range().0 as u32,
                            compressed_size: wayfinder.compressed_size_hint() as u32,
                            decompressed_size: wayfinder.uncompressed_size_hint() as u32,
                        }
                    }, regional);
                    compressed_files += 1;
                }
            }
        }

        Self {
            compressed_files,
            uncompressed_files,
            roots,
            files,
            checksum: checksum.finalize(),
        }
    }
}
