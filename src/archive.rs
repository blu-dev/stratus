use std::{
    alloc::Layout,
    io::{Read, Seek, SeekFrom},
};

use bytemuck::{Pod, Zeroable};
use camino::Utf8Path;

use crate::{
    containers::{BucketLookup, IndexLookup, Table, TableMut, TableRef, TableSliceRef},
    data::{
        FileData, FileDescriptor, FileEntity, FileGroup, FileInfo, FilePackage, FilePackageChild,
        FilePath, IntoHash, SearchFolder, SearchPath, SearchPathLink, StreamData, StreamEntity,
        StreamFolder, StreamPath,
    },
    HashDisplay,
};

fn read_pod<R: Read, T: Pod>(reader: &mut R) -> T {
    let mut uninit = T::zeroed();
    assert_eq!(
        reader
            .read(bytemuck::cast_slice_mut(std::slice::from_mut(&mut uninit)))
            .unwrap(),
        std::mem::size_of::<T>()
    );

    uninit
}

fn ru32<R: Read>(reader: &mut R) -> u32 {
    let mut bytes = [0; std::mem::size_of::<u32>()];
    assert_eq!(reader.read(&mut bytes).unwrap(), std::mem::size_of::<u32>());

    u32::from_le_bytes(bytes)
}

unsafe fn alloc_uninit(size: usize) -> Box<[u8]> {
    Box::from_raw(std::slice::from_raw_parts_mut(
        std::alloc::alloc(Layout::from_size_align(size, 0x10).unwrap()),
        size,
    ))
}

#[track_caller]
fn read_exact_size<R: Read>(reader: &mut R, size: usize) -> Box<[u8]> {
    // SAFETY: We initialize it here
    let mut uninit = unsafe { alloc_uninit(size) };

    let count = reader
        .read(&mut uninit)
        .expect("Failed to read from reader");

    // We don't need to worry about the panic unwindining dropping the uninit data since it's just u8 anyways
    if count < uninit.len() {
        panic!("Failed to fill whole buffer");
    }

    uninit
}

#[repr(C)]
#[derive(Debug)]
pub struct ZstdBuffer {
    pub ptr: *mut u8,
    pub size: usize,
    pub pos: usize,
}

#[skyline::from_offset(0x39a2fc0)]
pub fn decompress_stream(unk: *mut u64, output: &mut ZstdBuffer, input: &mut ZstdBuffer) -> usize;

#[repr(C)]
struct ZstdDecompressor([u64; 2]);

#[skyline::from_offset(0x35410b0)]
fn initialize_decompressor(ptr: *mut ZstdDecompressor) -> u64;

#[skyline::from_offset(0x3541030)]
fn finalize_decompressor(ptr: *mut ZstdDecompressor);

#[repr(align(8), C)]
struct FileNX([u8; 0x228]);

#[skyline::from_offset(0x353a3c0)]
fn init_file(file_nx: &mut *mut FileNX);

#[skyline::from_offset(0x353a500)]
fn open_file(file_nx: &mut *mut FileNX, path: *const i8) -> bool;

#[skyline::from_offset(0x3540a90)]
fn read_compressed_at_offset(file_nx: &mut *mut FileNX, offset: usize) -> *mut u8;

#[skyline::from_offset(0x37c58c0)]
fn read_into_ptr(file_nx: *mut FileNX, buffer: *mut u8, size: usize) -> usize;

pub fn read_compressed_section<R: Read + Seek>(reader: &mut R) -> Box<[u8]> {
    const REQUIRED_TABLE_SIZE: u32 = 0x10;

    let start = reader.stream_position().unwrap();
    assert_eq!(ru32(reader), REQUIRED_TABLE_SIZE);

    let decompressed_size = ru32(reader);
    let compressed_size = ru32(reader);
    let offset_to_next = ru32(reader);

    let mut file_ptr: *mut FileNX = std::ptr::null_mut();

    // unsafe {
    //     init_file(&mut file_ptr, c"rom:/data.arc".as_ptr());
    // }

    // let mut compressed = read_exact_size(reader, compressed_size as usize);
    // println!("Read {:#x} bytes", compressed.len());

    // // SAFETY: Reference call at 0x3540bb4 - instructions initialize 0x10 bytes worth of space and pass it to
    // // what we've labeled `initialize_decompressor`, it is the constructor for this type. It only uses
    // // the decompression codepath if the return value of the constructor is 0
    // let mut decompressor = ZstdDecompressor([0u64; 2]);
    // unsafe {
    //     assert_eq!(initialize_decompressor(&mut decompressor), 0);
    //     // println!("{:#x?}", decompressor);
    // }

    // // SAFETY: We initialize below, and we also assert that the count of bytes is the same
    // //  as the decompressed size
    // let mut decompressed = unsafe { alloc_uninit(decompressed_size as usize) };

    // let mut input_buffer = ZstdBuffer {
    //     ptr: compressed.as_mut_ptr(),
    //     size: compressed.len(),
    //     pos: 0,
    // };

    // let mut output_buffer = ZstdBuffer {
    //     ptr: decompressed.as_mut_ptr(),
    //     size: decompressed.len(),
    //     pos: 0,
    // };

    // // SAFETY: Reference call at 0x3540ca4 - Passes the second u64 from the initialize_decompressor call as well as two
    // // pointer buffers that match the structure as they are defined in this file: output first then input
    // let result = unsafe {
    //     decompress_stream(
    //         decompressor.0[1] as *mut u64,
    //         &mut output_buffer,
    //         &mut input_buffer,
    //     )
    // };

    // // NOTE: Call the destructor first since if we panic it will not exit with RAII (too lazy to write a struct for this)
    // // SAFETY: Destructor for the decompressor. Disassembly does not help ensure that this is the deconstructor,
    // // but in practice it seems to be fine and the function frees/releases data
    // unsafe {
    //     // println!("{:#x?}", decompressor);
    //     finalize_decompressor(&raw mut decompressor);
    // }

    // // Negative value for result is error code
    // assert_eq!(result, 0x0, "{result}");
    // assert_eq!(output_buffer.pos, decompressed_size as usize);

    // println!("Read {} decompressed bytes", decompressed.len());

    // This should seek past the compressed section and go to the start of the next valid data
    // This might skip past more bytes than are in the compressed section, but that's how the file format
    // was designed
    reader
        .seek(std::io::SeekFrom::Start(start + offset_to_next as u64))
        .unwrap();

    todo!()
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct ArchiveMetadata {
    pub magic: u64,
    pub stream_data_offset: u64,
    pub file_data_offset: u64,
    pub shared_file_data_offset: u64,
    pub resource_table_offset: u64,
    pub search_table_offset: u64,
    pub unknown_table_offset: u64,
}

impl ArchiveMetadata {
    const MAGIC: u64 = 0xABCDEF9876543210;
}

impl ArchiveMetadata {
    pub fn read<R: Read>(reader: &mut R) -> Self {
        let bytes = read_exact_size(reader, std::mem::size_of::<Self>());

        let this: &Self = bytemuck::from_bytes(&bytes);
        assert_eq!(this.magic, Self::MAGIC, "{:#x}", this.magic);
        *this
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable, PartialEq, Eq)]
pub struct SearchTableHeader {
    search_data_size: u32,
    _padding: u32,
    folder_count: u32,
    path_link_count: u32,
    path_count: u32,
}

const REGION_COUNT: usize = 5;
const LOCALE_COUNT: usize = 14;

#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable, PartialEq, Eq)]
pub struct ResourceTableHeader {
    resource_data_size: u32,
    file_path_count: u32,
    file_entity_count: u32,

    file_package_count: u32,
    file_data_group_count: u32,
    file_package_child_count: u32,
    file_package_info_count: u32,
    file_package_desc_count: u32,
    file_package_data_count: u32,

    file_info_group_count: u32,
    file_group_info_count: u32,

    padding_1: [u8; 0xC],

    locale_count: u8,
    region_count: u8,

    padding_2: [u8; 0x2],

    version_patch: u8,
    version_minor: u8,
    version_major: u16,

    versioned_file_group_count: u32,
    versioned_file_count: u32,
    padding_3: [u8; 0x4],
    versioned_file_info_count: u32,
    versioned_file_desc_count: u32,
    versioned_file_data_count: u32,

    // NOTE: This is hardcoded to be 14, but in reality it should be locale_count long
    // This assumed value for locale_count is asserted on during initialization
    locale_hash_to_region: [[u32; 3]; LOCALE_COUNT],

    pub stream_folder_count: u32,
    pub stream_path_count: u32,
    pub stream_entity_count: u32,
    pub stream_data_count: u32,
}

pub struct SearchTables {
    raw: Box<[u8]>,
    header: SearchTableHeader,
    search_folder_lookup: IndexLookup,
    search_folder: Table<SearchFolder>,
    search_path_lookup: IndexLookup,
    search_path_link: Table<SearchPathLink>,
    search_path: Table<SearchPath>,
    path_link_real_count: u32,
}

pub struct ResourceTables {
    raw: Box<[u8]>,
    header: ResourceTableHeader,
    stream_folder: Table<StreamFolder>,
    stream_path_lookup: IndexLookup,
    stream_path: Table<StreamPath>,
    stream_entity: Table<StreamEntity>,
    stream_data: Table<StreamData>,
    file_path_lookup: BucketLookup,
    file_path: Table<FilePath>,
    file_entity: Table<FileEntity>,
    file_package_lookup: IndexLookup,
    file_package: Table<FilePackage>,
    file_group: Table<FileGroup>,
    file_package_child: Table<FilePackageChild>,
    file_info: Table<FileInfo>,
    file_desc: Table<FileDescriptor>,
    file_data: Table<FileData>,
}

impl SearchTables {
    pub fn reserialize_internal(&mut self) {
        macro_rules! reserialize_order {
            ($($id:ident,)*) => {
                let mut total = std::mem::size_of::<SearchTableHeader>();
                $(
                    let $id = {
                        let current = total;
                        total += self.$id.byte_len();
                        current
                    };
                )*

                let new_buffer = unsafe {
                    std::alloc::alloc(Layout::from_size_align(total, 0x10).unwrap())
                };

                let buffer_slice = unsafe { std::slice::from_raw_parts_mut(new_buffer, total) };

                $(
                    unsafe { self.$id.write_and_update(buffer_slice, $id); }
                )*

                self.header.search_data_size = total as u32;
                self.header.folder_count = self.search_folder.len() as u32;
                self.header.path_link_count = self.search_path_link.len() as u32;
                self.header.path_count = self.search_path.len() as u32;

                // self.header.stream_data_size = total as u32;
                // self.header.file_path_count = self.file_path.len() as u32;
                // self.header.file_entity_count = self.file_entity.len() as u32;
                // self.header.file_package_info_count = self.file_info.len() as u32 - self.header.versioned_file_info_count - self.header.file_group_info_count;
                // self.header.file_package_desc_count = self.file_desc.len() as u32 - self.header.versioned_file_desc_count - self.header.file_group_info_count;
                // self.header.file_package_data_count = self.file_data.len()  as u32 - self.header.versioned_file_data_count - self.header.file_group_info_count;
                unsafe { *new_buffer.cast::<SearchTableHeader>() = self.header; }

                self.raw = unsafe { Box::from_raw(buffer_slice) };
            }
        }

        reserialize_order! {
            search_folder_lookup, search_folder, search_path_lookup, search_path_link, search_path,
        }
    }

    #[allow(unused_assignments)]
    pub fn from_bytes(mut bytes: Box<[u8]>) -> Self {
        let mut cursor = 0usize;
        let header: SearchTableHeader = *bytemuck::from_bytes(
            &bytes[cursor..cursor + std::mem::size_of::<SearchTableHeader>()],
        );

        cursor += std::mem::size_of::<SearchTableHeader>();

        macro_rules! fetch {
            ($t:path, $count:expr) => {
                unsafe {
                    let table = <$t>::new(&mut bytes[cursor..], ($count) as usize);
                    cursor += table.fixed_byte_len();
                    table
                }
            };
        }

        let search_folder_lookup = fetch!(IndexLookup, header.folder_count);
        let search_folder = fetch!(Table<SearchFolder>, header.folder_count);
        let search_path_lookup = fetch!(IndexLookup, header.path_link_count);
        let search_path_link = fetch!(Table<SearchPathLink>, header.path_link_count);
        let search_path = fetch!(Table<SearchPath>, header.path_count);
        let path_link_real_count = search_path_link
            .iter()
            .find(|(_, link)| link.is_invalid())
            .map(|(idx, _)| idx)
            .unwrap_or(search_path_link.len() as u32);

        Self {
            raw: bytes,
            header,
            search_folder_lookup,
            search_folder,
            search_path_lookup,
            search_path_link,
            search_path,
            path_link_real_count,
        }
    }
}

impl ResourceTables {
    // reserializes the tables into a new boxed slice, releasing the old one
    // this will update all tables to point to the new memory range in the new byte slice
    pub fn reserialize_internal(&mut self) {
        macro_rules! reserialize_order {
            ($($id:ident,)*) => {
                let mut total = std::mem::size_of::<ResourceTableHeader>();
                $(
                    let $id = {
                        let current = total;
                        total += self.$id.byte_len();
                        current
                    };
                )*

                let new_buffer = unsafe {
                    std::alloc::alloc(Layout::from_size_align(total, 0x10).unwrap())
                };

                let buffer_slice = unsafe { std::slice::from_raw_parts_mut(new_buffer, total) };

                $(
                    unsafe { self.$id.write_and_update(buffer_slice, $id); }
                )*

                self.header.resource_data_size = total as u32;
                self.header.file_package_count = self.file_package.len() as u32;
                self.header.file_package_child_count = self.file_package_child.len() as u32;
                self.header.file_data_group_count = self.file_group.len() as u32 - self.header.versioned_file_group_count - self.header.file_info_group_count;
                self.header.file_path_count = self.file_path.len() as u32;
                self.header.file_entity_count = self.file_entity.len() as u32;
                self.header.file_package_info_count = self.file_info.len() as u32 - self.header.versioned_file_info_count - self.header.file_group_info_count;
                self.header.file_package_desc_count = self.file_desc.len() as u32 - self.header.versioned_file_desc_count - self.header.file_group_info_count;
                self.header.file_package_data_count = self.file_data.len()  as u32 - self.header.versioned_file_data_count - self.header.file_group_info_count;
                unsafe { *new_buffer.cast::<ResourceTableHeader>() = self.header; }

                self.raw = unsafe { Box::from_raw(buffer_slice) };
            }
        }

        reserialize_order! {
            stream_folder, stream_path_lookup, stream_path,
            stream_entity, stream_data, file_path_lookup,
            file_path, file_entity, file_package_lookup,
            file_package, file_group, file_package_child,
            file_info, file_desc, file_data,
        }
    }

    #[allow(unused_assignments)]
    pub fn from_bytes(mut bytes: Box<[u8]>) -> Self {
        use std::mem::size_of as s;
        let mut cursor = 0usize;
        let header: ResourceTableHeader =
            *bytemuck::from_bytes(&bytes[cursor..cursor + s::<ResourceTableHeader>()]);

        cursor += s::<ResourceTableHeader>();

        macro_rules! fetch {
            ($t:path, $count:expr) => {
                unsafe {
                    let table = <$t>::new(&mut bytes[cursor..], ($count) as usize);
                    cursor += table.fixed_byte_len();
                    table
                }
            };
        }

        let stream_folder = fetch!(Table<StreamFolder>, header.stream_folder_count);
        let stream_path_lookup = fetch!(IndexLookup, header.stream_path_count);
        let stream_path = fetch!(Table<StreamPath>, header.stream_path_count);
        let stream_entity = fetch!(Table<StreamEntity>, header.stream_entity_count);
        let stream_data = fetch!(Table<StreamData>, header.stream_data_count);

        let file_path_lookup_count: u32 = *bytemuck::from_bytes(&bytes[cursor..cursor + 4]);
        let file_path_bucket_count: u32 = *bytemuck::from_bytes(&bytes[cursor + 4..cursor + 8]);

        cursor += 8;

        let file_path_lookup = unsafe {
            let lookup = BucketLookup::new(
                &mut bytes[cursor..],
                file_path_lookup_count as usize,
                file_path_bucket_count as usize,
            );
            cursor += lookup.fixed_byte_len();
            lookup
        };

        let file_path = fetch!(Table<FilePath>, header.file_path_count);
        let file_entity = fetch!(Table<FileEntity>, header.file_entity_count);
        let file_package_lookup = fetch!(IndexLookup, header.file_package_count);
        let file_package = fetch!(Table<FilePackage>, header.file_package_count);
        let file_group = fetch!(
            Table<FileGroup>,
            header.file_info_group_count
                + header.file_data_group_count
                + header.versioned_file_group_count
        );
        let file_package_child = fetch!(Table<FilePackageChild>, header.file_package_child_count);
        let file_info = fetch!(
            Table<FileInfo>,
            header.file_package_info_count
                + header.file_group_info_count
                + header.versioned_file_info_count
        );
        let file_desc = fetch!(
            Table<FileDescriptor>,
            header.file_package_desc_count
                + header.file_group_info_count
                + header.versioned_file_desc_count
        );
        let file_data = fetch!(
            Table<FileData>,
            header.file_package_data_count
                + header.file_group_info_count
                + header.versioned_file_data_count
        );

        Self {
            raw: bytes,
            header,
            stream_folder,
            stream_path_lookup,
            stream_path,
            stream_entity,
            stream_data,
            file_path_lookup,
            file_path,
            file_entity,
            file_package_lookup,
            file_package,
            file_group,
            file_package_child,
            file_info,
            file_desc,
            file_data,
        }
    }
}

pub struct Archive {
    metadata: ArchiveMetadata,
    resource: ResourceTables,
    search: SearchTables,
}

macro_rules! decl_lookup {
    ($($name:ident => $t:ty),*) => {
        paste::paste! {
            $(
                pub fn [<lookup_ $name>](&self, path: impl IntoHash) -> Option<TableRef<'_, $t>> {
                    let index = self.resource.[<$name _lookup>].get(path.into_hash())?;
                    TableRef::new(self, &self.resource.$name, index)
                }

                pub fn [<lookup_ $name _mut>](&mut self, path: impl IntoHash) -> Option<TableMut<'_, $t>> {
                    let index = self.resource.[<$name _lookup>].get(path.into_hash())?;
                    TableMut::new(self, |archive| &mut archive.resource.$name, index)
                }
            )*
        }
    }
}

macro_rules! decl_search_access {
    ($($name:ident => $t:ty),*) => {
        paste::paste! {
            $(
                pub fn [<iter_ $name>](&self) -> impl Iterator<Item = TableRef<'_, $t>> {
                    self.search.$name.iter().filter_map(|(index, _)| TableRef::new(self, &self.search.$name, index))
                }

                pub fn [<num_ $name>](&self) -> usize {
                    self.search.$name.len()
                }

                pub fn [<get_ $name>](&self, index: u32) -> Option<TableRef<'_, $t>> {
                    TableRef::new(self, &self.search.$name, index)
                }

                pub fn [<get_ $name _mut>](&mut self, index: u32) -> Option<TableMut<'_, $t>> {
                    TableMut::new(self, |archive| &mut archive.search.$name, index)
                }
            )*
        }
    }
}

macro_rules! decl_access {
    ($($name:ident => $t:ty),*) => {
        paste::paste! {
            $(
                pub fn [<iter_ $name>](&self) -> impl Iterator<Item = TableRef<'_, $t>> {
                    self.resource.$name.iter().filter_map(|(index, _)| TableRef::new(self, &self.resource.$name, index))
                }

                pub fn [<num_ $name>](&self) -> usize {
                    self.resource.$name.len()
                }

                pub fn [<get_ $name>](&self, index: u32) -> Option<TableRef<'_, $t>> {
                    TableRef::new(self, &self.resource.$name, index)
                }

                pub fn [<get_ $name _mut>](&mut self, index: u32) -> Option<TableMut<'_, $t>> {
                    TableMut::new(self, |archive| &mut archive.resource.$name, index)
                }

                pub fn [<get_ $name _slice>](&self, index: u32, count: u32) -> Option<TableSliceRef<'_, $t>> {
                    TableSliceRef::new(self, &self.resource.$name, index, count)
                }

                pub fn [<push_ $name>](&mut self, element: $t) -> u32 {
                    self.resource.$name.push(element)
                }
            )*
        }
    }
}

impl Archive {
    decl_lookup! {
        file_path => FilePath,
        stream_path => StreamPath,
        file_package => FilePackage
    }

    decl_access! {
        file_path => FilePath,
        file_entity => FileEntity,
        file_info => FileInfo,
        file_desc => FileDescriptor,
        file_data => FileData,
        file_package => FilePackage,
        file_package_child => FilePackageChild,
        file_group => FileGroup,
        stream_folder => StreamFolder,
        stream_path => StreamPath,
        stream_entity => StreamEntity,
        stream_data => StreamData
    }

    decl_search_access! {
        search_folder => SearchFolder,
        search_path_link => SearchPathLink,
        search_path => SearchPath
    }

    pub fn lookup_search_folder(&self, path: impl IntoHash) -> Option<TableRef<'_, SearchFolder>> {
        let index = self.search.search_folder_lookup.get(path.into_hash())?;
        TableRef::new(self, &self.search.search_folder, index)
    }

    pub fn lookup_search_folder_mut(
        &mut self,
        path: impl IntoHash,
    ) -> Option<TableMut<'_, SearchFolder>> {
        let index = self.search.search_folder_lookup.get(path.into_hash())?;
        TableMut::new(self, |archive| &mut archive.search.search_folder, index)
    }

    pub fn lookup_search_path(&self, path: impl IntoHash) -> Option<TableRef<'_, SearchPath>> {
        let index = self.search.search_path_lookup.get(path.into_hash())?;
        let link = self.search.search_path_link.get(index)?;
        if link.is_invalid() {
            return None;
        }

        TableRef::new(self, &self.search.search_path, link.path_index())
    }

    pub fn lookup_search_path_mut(
        &mut self,
        path: impl IntoHash,
    ) -> Option<TableMut<'_, SearchPath>> {
        let index = self.search.search_path_lookup.get(path.into_hash())?;
        let link = self.search.search_path_link.get(index)?;
        if link.is_invalid() {
            return None;
        }

        let index = link.path_index();

        TableMut::new(self, |archive| &mut archive.search.search_path, index)
    }

    pub fn insert_search_path(&mut self, path: SearchPath) -> u32 {
        let link_index = if self.search.path_link_real_count as usize
            >= self.search.search_path_link.len()
            || true
        {
            let index = self.search.search_path.push(path);
            self.search
                .search_path_link
                .push(SearchPathLink::new(index))
        } else {
            let index = self.search.path_link_real_count;
            self.search.path_link_real_count += 1;
            *self.search.search_path_link.get_mut(index).unwrap() = SearchPathLink::new(index);
            *self.search.search_path.get_mut(index).unwrap() = path;
            index
        };

        self.search
            .search_path_lookup
            .insert(path.path(), link_index);
        link_index
    }

    pub fn insert_search_folder(&mut self, folder: SearchFolder) -> u32 {
        let new_index = self.search.search_folder.push(folder);
        let _ = self
            .search
            .search_folder_lookup
            .insert(folder.path(), new_index);
        new_index
    }

    pub fn insert_file_path(&mut self, path: FilePath) -> u32 {
        let path_idx = self.push_file_path(path);
        self.resource
            .file_path_lookup
            .insert(path.path_and_entity.hash40(), path_idx);
        path_idx
    }

    pub fn insert_file_package(&mut self, package: FilePackage) -> u32 {
        let package_idx = self.push_file_package(package);
        self.resource
            .file_package_lookup
            .insert(package.path(), package_idx);
        package_idx
    }

    pub fn dump(&self, path: impl AsRef<Utf8Path>) {
        std::fs::write(path.as_ref(), &self.resource.raw).unwrap();
    }

    pub fn reserialize(&mut self) {
        self.resource.reserialize_internal();
        self.search.reserialize_internal();
    }

    pub fn open() -> Self {
        let mut metadata = ArchiveMetadata::zeroed();
        let resource_slice;
        let search_slice;
        unsafe {
            let mut file: *mut FileNX = std::ptr::null_mut();
            init_file(&mut file);
            let mut buffer = [0u8; 0x108];
            buffer[0x100..0x108].copy_from_slice(bytemuck::bytes_of(&0xdu64));
            buffer[.."rom:/data.arc".len()].copy_from_slice("rom:/data.arc".as_bytes());
            open_file(&mut file, buffer.as_ptr().cast());
            let size = read_into_ptr(
                file,
                (&raw mut metadata).cast::<u8>(),
                std::mem::size_of::<ArchiveMetadata>(),
            );
            assert_eq!(size, 0x38);
            assert!(metadata.magic == ArchiveMetadata::MAGIC);
            let resource_ptr =
                read_compressed_at_offset(&mut file, metadata.resource_table_offset as usize);
            let resource_size =
                (*resource_ptr.cast::<ResourceTableHeader>()).resource_data_size as usize;
            let search_ptr =
                read_compressed_at_offset(&mut file, metadata.search_table_offset as usize);
            let search_size = (*search_ptr.cast::<SearchTableHeader>()).search_data_size as usize;
            resource_slice =
                Box::from_raw(std::slice::from_raw_parts_mut(resource_ptr, resource_size));
            search_slice = Box::from_raw(std::slice::from_raw_parts_mut(search_ptr, search_size));
        }
        let resource = ResourceTables::from_bytes(resource_slice);
        let search = SearchTables::from_bytes(search_slice);

        Self {
            metadata,
            resource,
            search,
        }
    }

    pub fn resource_data_ptr(&self) -> *const u8 {
        self.resource.raw.as_ptr()
    }

    pub fn search_data_ptr(&self) -> *const u8 {
        self.search.raw.as_ptr()
    }
}
