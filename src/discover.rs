use std::{cell::UnsafeCell, io::Read};

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

struct NnsdkFile {
    handle: skyline::nn::fs::FileHandle,
    offset: usize,
    len: usize,
}

impl NnsdkFile {
    fn open(path: &Utf8Path) -> Self {
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
        Ok(out_size as usize)
    }
}

pub struct DiscoveredFile {
    root_idx: u32,
    size: u32,
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
        archive: UnsafeCell<zip::ZipArchive<std::fs::File>>,
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

                let local_path = hash.display().to_string();
                let mut file = archive.by_name(&local_path).unwrap();
                file.read_exact(buffer).unwrap();
                // if count != buffer.len() {
                //     panic!(
                //         "Failed to load zip file {}: read {:#x} bytes vs {:#x}",
                //         hash.display(),
                //         count,
                //         buffer.len()
                //     );
                // }
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

        if root.file_type().unwrap().is_dir() {
            let root_idx: u32 = file_system.roots.len().try_into().unwrap();
            file_system
                .roots
                .push(ModRoot::Folder(root.path().to_path_buf()));
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
                        },
                    );
                },
            );
        } else if root.file_type().unwrap().is_file() && root.path().extension() == Some("zip") {
            let archive = std::fs::File::open(root.path()).unwrap();
            let mut zip_archive = zip::ZipArchive::new(archive).unwrap();
            let root_idx: u32 = file_system.roots.len().try_into().unwrap();

            let mut indices = vec![];
            for path in zip_archive.file_names() {
                indices.push(zip_archive.index_for_name(path).unwrap());
            }

            for index in indices {
                let file = zip_archive.by_index(index).unwrap();

                if file.is_dir() {
                    continue;
                }

                let path = file.name();
                let path = Utf8Path::new(path);
                if let Some(file_name) = path.file_name() {
                    hash.intern_path(cache, Utf8Path::new(file_name));
                }
                if let Some(ext) = path.extension() {
                    hash.intern_path(cache, Utf8Path::new(ext));
                }

                let size = file.size();

                if hash.intern_path(cache, path).is_new {
                    file_system.new_files.push(NewFile {
                        filepath: FilePath::from_utf8_path(path),
                        size: size as u32,
                    });
                }
                file_system.files.insert(
                    path.into_hash(),
                    DiscoveredFile {
                        root_idx,
                        size: size as u32,
                    },
                );
            }

            file_system.roots.push(ModRoot::Zip {
                path: root.path().to_path_buf(),
                archive: UnsafeCell::new(zip_archive),
            });
        }
    }

    file_system
}
