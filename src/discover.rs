use camino::{Utf8Path, Utf8PathBuf};
use smash_hash::{Hash40, Hash40Map};

use crate::{
    data::IntoHash,
    hash_interner::{HashMemorySlab, InternerCache, SmolRange},
    HashDisplay, FILE_SYSTEM,
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

pub struct DiscoveredFile {
    root_idx: u32,
    size: u32,
}

impl DiscoveredFile {
    pub fn size(&self) -> u32 {
        self.size
    }
}

pub struct FileSystem {
    roots: Vec<Utf8PathBuf>,
    files: Hash40Map<DiscoveredFile>,
}

impl FileSystem {
    pub fn get_full_file_path(&self, hash: Hash40, buffer: &mut String) -> Option<u32> {
        use std::fmt::Write;
        buffer.clear();

        let Some(file) = self.files.get(&hash) else {
            return None;
        };

        let _ = write!(
            buffer,
            "{}/{}",
            self.roots[file.root_idx as usize],
            hash.display()
        );
        Some(file.size())
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
    };

    for folder in mods_root.read_dir_utf8().unwrap() {
        let folder = folder.unwrap();
        if folder.file_name().starts_with(".") {
            continue;
        }
        if folder.file_type().unwrap().is_dir() {
            let root_idx: u32 = file_system.roots.len().try_into().unwrap();
            file_system.roots.push(folder.path().to_path_buf());
            discover_and_update_recursive(
                folder.path(),
                folder.path(),
                &mut |file_path: &Utf8Path, len: u32| {
                    let range = hash.intern_path(cache, file_path);
                    file_system.files.insert(
                        file_path.into_hash(),
                        DiscoveredFile {
                            root_idx,
                            size: len,
                        },
                    );
                },
            );
        }
    }

    file_system
}
