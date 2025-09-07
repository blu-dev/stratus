use std::{
    cell::UnsafeCell,
    io::{BufReader, Read},
};

use camino::{Utf8Path, Utf8PathBuf};
use skyline::nn::fs::FileHandle;
use smash_hash::{Hash40, Hash40Map};

use crate::{
    data::{FilePath, IntoHash},
    hash_interner::{HashMemorySlab, InternerCache},
    HashDisplay,
};

const MODS_ROOT: &'static str = "sd:/ultimate/mods/";

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

pub struct NnsdkFile {
    handle: skyline::nn::fs::FileHandle,
    offset: usize,
    len: usize,
}

impl NnsdkFile {
    pub fn open(path: &Utf8Path) -> Self {
        let mut handle = skyline::nn::fs::FileHandle { handle: 0 };
        let mut file_size = 0i64;
        let path = format!("{path}\0");
        unsafe {
            assert!(
                skyline::nn::fs::OpenFile(
                    &mut handle,
                    path.as_ptr(),
                    skyline::nn::fs::OpenMode_OpenMode_Read as i32
                ) == 0x0
            );
            assert!(skyline::nn::fs::GetFileSize(&mut file_size, handle) == 0x0);
        }
        Self {
            handle,
            offset: 0,
            len: file_size as usize,
        }
    }
}

impl std::io::Seek for NnsdkFile {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        match pos {
            std::io::SeekFrom::Start(offset) => {
                self.offset = offset as usize;
            }
            std::io::SeekFrom::Current(offset) => {
                self.offset = self.offset.wrapping_add_signed(offset as isize);
            }
            std::io::SeekFrom::End(offset) => {
                self.offset = self.len.wrapping_add_signed(offset as isize);
            }
        }

        Ok(self.offset as u64)
    }
}

unsafe extern "C" {
    #[link_name = "_ZN2nn2fs8ReadFileEPmNS0_10FileHandleElPvmRKNS0_10ReadOptionE"]
    unsafe fn read_file(
        out: &mut u64,
        handle: FileHandle,
        offset: i64,
        buf: *mut u8,
        buf_len: u64,
        read_option: &i32,
    ) -> u32;
}

impl std::io::Read for NnsdkFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut out_size = 0u64;
        unsafe {
            assert!(
                read_file(
                    &mut out_size,
                    self.handle,
                    self.offset as _,
                    buf.as_mut_ptr(),
                    buf.len() as _,
                    &0,
                ) == 0x0
            );
        }
        self.offset += out_size as usize;
        Ok(out_size as usize)
    }
}

pub struct DiscoveredFile {
    root_idx: u32,
    size: u32,
    wayfinder: Option<rawzip::ZipArchiveEntryWayfinder>,
}

impl DiscoveredFile {
    pub fn size(&self) -> u32 {
        self.size
    }
}

pub struct NewFile {
    pub filepath: FilePath,
    pub size: u32,
}

pub enum ModRoot {
    Folder(Utf8PathBuf),
    Zip {
        path: Utf8PathBuf,
        archive: UnsafeCell<rawzip::ZipArchive<rawzip::FileReader>>,
    },
}

pub struct FileSystem {
    roots: Vec<ModRoot>,
    files: Hash40Map<DiscoveredFile>,
    pub new_files: Vec<NewFile>,
}

impl FileSystem {
    pub fn get_full_file_path(&self, hash: Hash40, buffer: &mut String) -> Option<u32> {
        use std::fmt::Write;
        buffer.clear();

        let Some(file) = self.files.get(&hash) else {
            return None;
        };

        let name = match &self.roots[file.root_idx as usize] {
            ModRoot::Folder(folder) => folder.as_str(),
            ModRoot::Zip { path, .. } => path.as_str(),
        };

        let _ = write!(buffer, "{}/{}", name, hash.display());
        Some(file.size())
    }

    pub fn load_into_buffer(&self, hash: Hash40, filepath: &String, buffer: &mut [u8]) {
        let file = self.files.get(&hash).unwrap();

        match &self.roots[file.root_idx as usize] {
            ModRoot::Folder(_) => {
                let mut file = std::fs::File::open(filepath).unwrap();
                let count = file.read(buffer).unwrap();
                assert!(count == buffer.len());
            }
            ModRoot::Zip { archive, .. } => {
                let archive = unsafe { &mut *archive.get() };

                let wayfinder = self.files.get(&hash).unwrap().wayfinder.unwrap();
                let file = archive.get_entry(wayfinder).unwrap();

                let layout = std::alloc::Layout::from_size_align(
                    wayfinder.compressed_size_hint() as usize,
                    0x1,
                )
                .unwrap();
                let compressed_buffer = unsafe { std::alloc::alloc(layout) };

                assert!(!compressed_buffer.is_null());

                let slice = unsafe {
                    std::slice::from_raw_parts_mut(
                        compressed_buffer,
                        wayfinder.compressed_size_hint() as usize,
                    )
                };

                file.reader().read_exact(slice).unwrap();

                flate2::bufread::DeflateDecoder::new(std::io::Cursor::new(slice))
                    .read_exact(buffer)
                    .unwrap();

                unsafe {
                    std::alloc::dealloc(compressed_buffer, layout);
                }
            }
        }
    }

    pub fn files(&self) -> impl IntoIterator<Item = (&Hash40, &DiscoveredFile)> {
        self.files.iter()
    }
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
                .push(ModRoot::Folder(root.path().to_path_buf()));
            discover_and_update_recursive(
                root.path(),
                root.path(),
                &mut |file_path: &Utf8Path, len: u32| {
                    if len == 0 {
                        println!("{file_path} has a len of 0");
                    }
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
                            wayfinder: None,
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
                        size: size as u32,
                    });
                }
                // intern_elapsed += now.elapsed();

                file_system.files.insert(
                    path.into_hash(),
                    DiscoveredFile {
                        root_idx,
                        size: size as u32,
                        wayfinder: Some(entry.wayfinder()),
                    },
                );
            }
            println!(
                "Processing rawzip took: {:.3}ms",
                now.elapsed().as_micros() as f32 / 1000.0
            );

            file_system.roots.push(ModRoot::Zip {
                path: root.path().to_path_buf(),
                archive: UnsafeCell::new(archive),
            });

            // let archive = rawzip::ZipArchive::from_file(
            //     std::fs::File::open(root.path()).unwrap(),
            //     &mut buffer,
            // )
            // .unwrap();

            // let now = std::time::Instant::now();
            // let mut zip_archive =
            //     zip::ZipArchive::new(BufReader::new(std::fs::File::open(root.path()).unwrap()))
            //         .unwrap();
            // println!(
            //     "[stratus::discovery] Read zip file in {:.3}s",
            //     now.elapsed().as_secs_f32()
            // );

            // let mut indices = vec![];
            // let now = std::time::Instant::now();
            // for path in zip_archive.file_names() {
            //     indices.push(zip_archive.index_for_name(path).unwrap());
            // }
            // println!(
            //     "[stratus::discovery] Iterating file names {:.3}ms",
            //     now.elapsed().as_micros() as f32 / 1000.0
            // );

            // let now = std::time::Instant::now();
            // let mut obtain_elapsed = std::time::Duration::from_millis(0);
            // let mut is_dir_elapsed = std::time::Duration::from_millis(0);
            // let mut is_dir_count = 0;
            // let mut intern_elapsed = std::time::Duration::from_millis(0);
            // let mut total = 0usize;
            // for index in indices {
            //     is_dir_count += 1;
            //     let now = std::time::Instant::now();
            //     let file = zip_archive.by_index(index).unwrap();
            //     obtain_elapsed += now.elapsed();

            //     let now = std::time::Instant::now();
            //     if file.is_dir() {
            //         is_dir_elapsed += now.elapsed();
            //         continue;
            //     }

            //     total += 1;

            //     let now = std::time::Instant::now();
            //     let path = file.name();
            //     let path = Utf8Path::new(path);
            //     if let Some(file_name) = path.file_name() {
            //         hash.intern_path(cache, Utf8Path::new(file_name));
            //     }
            //     if let Some(ext) = path.extension() {
            //         hash.intern_path(cache, Utf8Path::new(ext));
            //     }

            //     let size = file.size();

            //     if hash.intern_path(cache, path).is_new {
            //         file_system.new_files.push(NewFile {
            //             filepath: FilePath::from_utf8_path(path),
            //             size: size as u32,
            //         });
            //     }
            //     intern_elapsed += now.elapsed();

            //     file_system.files.insert(
            //         path.into_hash(),
            //         DiscoveredFile {
            //             root_idx,
            //             size: size as u32,
            //         },
            //     );
            // }

            // println!(
            //     "[stratus::discovery] Avg time per file: Obtain={:.3}ms, IsDir={:.3}ms, Intern={:.3}ms, Total={:.3}ms",
            //     obtain_elapsed.as_millis() as f32 / is_dir_count as f32,
            //     is_dir_elapsed.as_millis() as f32 / is_dir_count as f32,
            //     intern_elapsed.as_millis() as f32 / total as f32,
            //     now.elapsed().as_millis() as f32 / total as f32,
            // );
        }

        println!(
            "[stratus::discovery] Discovered {} in {:.3}s",
            root.path(),
            now.elapsed().as_secs_f32()
        );
    }

    file_system
}
