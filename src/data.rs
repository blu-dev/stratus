use std::fmt::{Debug, Display};

use bytemuck::{Pod, Zeroable};
use camino::{Utf8Path, Utf8PathBuf};
use smash_hash::Hash40;

use crate::{
    containers::{TableMut, TableRef},
    HashDisplay,
};

pub trait IntoHash {
    fn into_hash(self) -> Hash40;
}

impl IntoHash for &str {
    fn into_hash(self) -> Hash40 {
        Hash40::const_new(self)
    }
}

impl IntoHash for &Utf8Path {
    fn into_hash(self) -> Hash40 {
        Hash40::const_new(self.as_str())
    }
}

impl IntoHash for String {
    fn into_hash(self) -> Hash40 {
        Hash40::const_new(self.as_str())
    }
}

impl IntoHash for Utf8PathBuf {
    fn into_hash(self) -> Hash40 {
        Hash40::const_new(self.as_str())
    }
}

impl IntoHash for Hash40 {
    fn into_hash(self) -> Hash40 {
        self
    }
}

impl IntoHash for Hash {
    fn into_hash(self) -> Hash40 {
        self.hash40()
    }
}

impl IntoHash for HashWithData {
    fn into_hash(self) -> Hash40 {
        self.hash40()
    }
}

impl IntoHash for u64 {
    fn into_hash(self) -> Hash40 {
        Hash40::from_raw(self)
    }
}

// Needs to be aligned on 4-bytes, smash_hash::Hash40 is aligned on 8
#[repr(C)]
#[derive(Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct Hash {
    crc: u32,
    len: u32, // We only use u32 here for Pod restrictions
}

impl Debug for Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.hash40().display(), f)
    }
}

impl Display for Hash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.hash40().display(), f)
    }
}

impl Hash {
    pub const fn hash40(self) -> Hash40 {
        Hash40::from_raw(((self.len as u64) << 32) | self.crc as u64)
    }
}

#[repr(C)]
#[derive(Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct HashWithData {
    crc: u32,
    len_and_data: u32,
}

impl Debug for HashWithData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HashWithData")
            .field("hash", &self.hash40().display())
            .field("data", &self.data())
            .finish()
    }
}

impl HashWithData {
    const DATA_READ_MASK: u32 = 0xFFFF_FF00;
    const DATA_WRITE_MASK: u32 = 0x00FF_FFFF;

    pub const fn new(hash: Hash40, data: u32) -> Self {
        Self {
            crc: hash.crc32(),
            len_and_data: hash.length() as u32 | ((data & Self::DATA_WRITE_MASK) << 8),
        }
    }

    pub const fn hash40(self) -> Hash40 {
        Hash {
            crc: self.crc,
            len: self.len_and_data & 0xFF,
        }
        .hash40()
    }

    pub const fn length(self) -> usize {
        self.hash40().length() as usize
    }

    pub const fn data(self) -> u32 {
        (self.len_and_data & Self::DATA_READ_MASK) >> 8
    }

    pub fn set_hash40(&mut self, hash: Hash40) {
        self.crc = hash.crc32();
        self.len_and_data = (self.len_and_data & Self::DATA_READ_MASK) | hash.length() as u32;
    }

    pub fn set_data(&mut self, data: u32) {
        self.len_and_data = (self.len_and_data & 0xFF) | ((data & Self::DATA_WRITE_MASK) << 8);
    }
}

bitflags::bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
    pub struct FileFlags : u32 {
        const IS_ZSTD_COMPRESSION = 1 << 0;
        const IS_COMPRESSED = 1 << 1;
        const IS_REGIONAL_VERSIONED_DATA = 1 << 2;
        const IS_LOCALIZED_VERSIONED_DATA = 1 << 3;
    }

    #[repr(transparent)]
    #[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
    pub struct FileInfoFlags : u32 {
        const IS_REGULAR_FILE = 1 << 4;
        const IS_GRAPHCIS_ARCHIVE = 1 << 12;
        const IS_LOCALIZED = 1 << 15;
        const IS_REGIONAL = 1 << 16;
        const IS_SHARED = 1 << 20;
        const IS_UNKNOWN_FLAG = 1 << 21;
    }

    #[repr(transparent)]
    #[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
    pub struct FilePackageFlags : u32 {
        const IS_LOCALIZED = 1 << 24;
        const IS_REGIONAL = 1 << 25;
        const HAS_SUB_PACKAGE = 1 << 26;
        const SYM_LINK_IS_REGIONAL = 1 << 27;
        const IS_SYM_LINK = 1 << 28;
    }

    #[repr(transparent)]
    #[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
    pub struct StreamFileFlags : u32 {
        const IS_LOCALIZED = 1 << 0;
        const IS_REGIONAL = 1 << 1;
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct FileData {
    in_group_offset: u32,
    compressed_size: u32,
    decompressed_size: u32,
    flags: FileFlags,
}

impl TableMut<'_, FileData> {
    pub fn is_compressed(&self) -> bool {
        self.flags.contains(FileFlags::IS_COMPRESSED)
    }

    pub fn compressed_size(&self) -> u32 {
        self.compressed_size
    }

    pub fn patch(&mut self, new_size: u32) {
        // if !self.flags.contains(FileFlags::IS_COMPRESSED) {
        //     self.compressed_size = new_size;
        // }
        self.decompressed_size = new_size;
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum FileLoadMethod {
    // Index is for FileEntity
    Unowned(u32),

    // Index is for versioned patch section
    Owned(u32),

    // Index is for FileInfo
    PackageSkip(u32),

    Unknown,

    // Index is for FileEntity
    SharedButOwned(u32),

    // Index is locale or region (depending on file)
    UnsupportedRegionLocale(u32),
}

impl From<u32> for FileLoadMethod {
    fn from(value: u32) -> Self {
        let kind = value >> 24;
        let value = value & 0x00FF_FFFF;
        match kind {
            0x00 => Self::Unowned(value),
            0x01 => Self::Owned(value),
            0x03 => Self::PackageSkip(value),
            0x05 => Self::Unknown,
            0x09 => Self::SharedButOwned(value),
            0x10 => Self::UnsupportedRegionLocale(value),
            _ => panic!("Unsuppored load method {:#02x}", kind),
        }
    }
}

impl From<FileLoadMethod> for u32 {
    fn from(value: FileLoadMethod) -> Self {
        let (kind, value) = match value {
            FileLoadMethod::Unowned(value) => (0x00, value),
            FileLoadMethod::Owned(value) => (0x01, value),
            FileLoadMethod::PackageSkip(value) => (0x03, value),
            FileLoadMethod::Unknown => (0x05, 0),
            FileLoadMethod::SharedButOwned(value) => (0x09, value),
            FileLoadMethod::UnsupportedRegionLocale(value) => (0x10, value),
        };

        (kind << 24) | value
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct FileDescriptor {
    group: u32,
    file_data: u32,
    load_method: u32,
}

impl<'a> TableMut<'a, FileDescriptor> {
    pub fn data_mut(self) -> TableMut<'a, FileData> {
        let index = self.file_data;
        self.into_archive_mut().get_file_data_mut(index).unwrap()
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct FileInfo {
    path: u32,
    entity: u32,
    desc: u32,
    flags: FileInfoFlags,
}

impl<'a> TableRef<'a, FileInfo> {
    pub fn file_path(&self) -> TableRef<'a, FilePath> {
        self.archive().get_file_path(self.path).unwrap()
    }

    pub fn entity(&self) -> TableRef<'a, FileEntity> {
        self.archive().get_file_entity(self.entity).unwrap()
    }
}

impl<'a> TableMut<'a, FileInfo> {
    pub fn path_ref(&self) -> TableRef<'_, FilePath> {
        self.archive().get_file_path(self.path).unwrap()
    }

    pub fn entity_ref(&self) -> TableRef<'_, FileEntity> {
        self.archive().get_file_entity(self.entity).unwrap()
    }

    pub fn entity_mut(self) -> TableMut<'a, FileEntity> {
        let index = self.entity;
        self.into_archive_mut().get_file_entity_mut(index).unwrap()
    }

    pub fn desc_mut(self) -> TableMut<'a, FileDescriptor> {
        // println!("{self:?} => {:?}", self.archive().get_file_path(self.path));
        let index = self.desc;
        self.into_archive_mut().get_file_desc_mut(index).unwrap()
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct FileEntity {
    package_or_group: u32,
    info: u32,
}

impl<'a> TableRef<'a, FileEntity> {
    pub fn info(&self) -> TableRef<'a, FileInfo> {
        self.archive().get_file_info(self.info).unwrap()
    }
}

impl<'a> TableMut<'a, FileEntity> {
    pub fn info_mut(self) -> TableMut<'a, FileInfo> {
        let index = self.info;
        self.into_archive_mut().get_file_info_mut(index).unwrap()
    }
}

#[repr(C)]
#[derive(Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct FilePath {
    pub path_and_entity: HashWithData,
    ext_and_version: HashWithData,
    parent: Hash,
    file_name: Hash,
}

impl<'a> TableMut<'a, FilePath> {
    pub fn entity_mut(self) -> TableMut<'a, FileEntity> {
        let index = self.path_and_entity.data();
        self.into_archive_mut().get_file_entity_mut(index).unwrap()
    }
}

struct FixTrailingSlashWithData<'a>(&'a HashWithData);
impl Debug for FixTrailingSlashWithData<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HashWithData")
            .field("hash", &self.0.hash40().const_trim_trailing("/").display())
            .field("data", &self.0.data())
            .finish()
    }
}
struct FixTrailingSlash<'a>(&'a Hash);
impl Debug for FixTrailingSlash<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.0.hash40().const_trim_trailing("/").display(), f)
    }
}

impl Debug for FilePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilePath")
            .field("path_and_entity", &self.path_and_entity)
            .field("ext_and_version", &self.ext_and_version)
            .field("parent", &FixTrailingSlash(&self.parent))
            .field("file_name", &self.file_name)
            .finish()
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct FilePackage {
    path_and_group: HashWithData,
    name: Hash,
    parent: Hash,
    lifetime: Hash,
    info_start: u32,
    info_count: u32,
    child_start: u32,
    child_count: u32,
    flags: FilePackageFlags,
}

#[repr(transparent)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct FilePackageChild(HashWithData);

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct FileGroup {
    archive_offset: [u32; 2],
    decompressed_size: u32,
    compressed_size: u32,
    child_start: u32,
    child_count: u32,
    redirection: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct StreamData {
    size: u64,
    offset: u64,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct StreamEntity {
    stream_data: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct StreamFolder {
    name_and_child_count: HashWithData,
    child_start_index: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct StreamPath {
    path_and_desc: HashWithData,
    flags: StreamFileFlags,
}
