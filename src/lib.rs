use std::{
    fs::File,
    io::{Cursor, Seek, SeekFrom},
    ops::Deref,
    sync::OnceLock,
    time::Instant,
};

use camino::Utf8Path;
use smash_hash::Hash40;

use crate::hash_interner::{DisplayHash, HashMemorySlab, InternerCache};

mod archive;
mod hash_interner;

const STRATUS_FOLDER: &'static str = "sd://ultimate/stratus/";

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

        let blob_path: &'static Utf8Path = Utf8Path::new("sd://ultimate/stratus/hashes.blob");
        let meta_path: &'static Utf8Path = Utf8Path::new("sd://ultimate/stratus/hashes.meta");
        let hashes_src: &'static Utf8Path = Utf8Path::new("sd://ultimate/stratus/Hashes_FullPath");

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

#[skyline::main(name = "stratus")]
pub fn main() {
    init_folder();
    init_hashes();

    let mut file = File::open("rom:/data.arc").unwrap();
    let archive = archive::ArchiveMetadata::read(&mut file);
    file.seek(SeekFrom::Start(archive.resource_table_offset))
        .unwrap();
    let decompressed_bytes = archive::read_compressed_section(&mut file);
    let table = archive::ResourceTableHeader::read(&mut Cursor::new(decompressed_bytes));
    drop(file);

    println!("{archive:x?}");
    println!("{table:x?}");

    println!(
        "{}",
        Hash40::const_new("fighter/mario/model/body/c00/model.numdlb").display()
    );
}
