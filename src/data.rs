use std::fmt::{Debug, Display};

use bytemuck::{Pod, Zeroable};
use camino::{Utf8Path, Utf8PathBuf};
use smash_hash::Hash40;

use crate::{
    containers::{TableMut, TableRef, TableSliceRef},
    HashDisplay, LocalePreferences,
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

#[repr(u32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Locale {
    Japanese = 0,
    UsEnglish,
    UsFrench,
    UsSpanish,
    EuEnglish,
    EuFrench,
    EuSpanish,
    German,
    Dutch,
    Italian,
    Russian,
    Korean,
    Chinese,
    Taiwanese,
}

impl Locale {
    pub const COUNT: usize = 14;

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "ja_jp" => Some(Self::Japanese),
            "us_en" => Some(Self::UsEnglish),
            "us_fr" => Some(Self::UsFrench),
            "us_es" => Some(Self::UsSpanish),
            "eu_en" => Some(Self::EuEnglish),
            "eu_fr" => Some(Self::EuFrench),
            "eu_es" => Some(Self::EuSpanish),
            "eu_de" => Some(Self::German),
            "eu_nl" => Some(Self::Dutch),
            "eu_it" => Some(Self::Italian),
            "eu_ru" => Some(Self::Russian),
            "kr_ko" => Some(Self::Korean),
            "zh_cn" => Some(Self::Chinese),
            "zh_tw" => Some(Self::Taiwanese),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Japanese => "ja_jp",
            Self::UsEnglish => "us_en",
            Self::UsFrench => "us_fr",
            Self::UsSpanish => "us_es",
            Self::EuEnglish => "eu_en",
            Self::EuFrench => "eu_fr",
            Self::EuSpanish => "eu_es",
            Self::German => "eu_de",
            Self::Dutch => "eu_nl",
            Self::Italian => "eu_it",
            Self::Russian => "eu_ru",
            Self::Korean => "kr_ko",
            Self::Chinese => "zh_cn",
            Self::Taiwanese => "zh_tw",
        }
    }
}

#[repr(u32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Region {
    Japan = 0,
    NorthAmerica,
    Europe,
    Korea,
    China,
}

impl Region {
    pub const COUNT: usize = 5;

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "ja" => Some(Self::Japan),
            "us" => Some(Self::NorthAmerica),
            "eu" => Some(Self::Europe),
            "kr" => Some(Self::Korea),
            "zh" => Some(Self::China),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Japan => "ja",
            Self::NorthAmerica => "us",
            Self::Europe => "eu",
            Self::Korea => "kr",
            Self::China => "zh",
        }
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
    pub const fn from_hash40(hash: Hash40) -> Self {
        Self {
            crc: hash.crc32(),
            len: hash.length() as u32,
        }
    }

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
        const IS_GRAPHICS_ARCHIVE = 1 << 12;
        const IS_LOCALIZED = 1 << 15;
        const IS_REGIONAL = 1 << 16;
        const IS_SHARED = 1 << 20;
        const IS_UNKNOWN_FLAG = 1 << 21;
        const IS_GROUP_FIXED = 1 << 30;
        const IS_RESHARED = 1 << 31;
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

impl FileData {
    pub fn new_for_unsharing(size: u32, offset: u32) -> Self {
        Self {
            in_group_offset: offset,
            compressed_size: 0,
            decompressed_size: size,
            flags: FileFlags::empty(),
        }
    }

    pub fn group_offset(&self) -> u32 {
        self.in_group_offset
    }

    pub fn is_compressed(&self) -> bool {
        self.flags.contains(FileFlags::IS_COMPRESSED)
    }

    pub fn compressed_size(&self) -> u32 {
        self.compressed_size
    }

    pub fn set_compressed_size(&mut self, new_size: u32) {
        self.compressed_size = new_size;
    }

    pub fn patch(&mut self, new_size: u32) {
        self.decompressed_size = new_size;
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum FileLoadMethod {
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

impl FileLoadMethod {
    pub fn is_owned(&self) -> bool {
        matches!(self, Self::Owned(_))
    }

    pub fn is_skip(&self) -> bool {
        matches!(self, Self::PackageSkip(_))
    }
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

impl FileDescriptor {
    pub fn new(group: u32, file_data: u32, load_method: FileLoadMethod) -> Self {
        Self {
            group,
            file_data,
            load_method: load_method.into(),
        }
    }

    pub fn set_data(&mut self, data: u32) {
        self.file_data = data;
    }

    pub fn load_method(&self) -> FileLoadMethod {
        FileLoadMethod::from(self.load_method)
    }

    pub fn set_load_method(&mut self, method: FileLoadMethod) {
        self.load_method = method.into();
    }

    pub fn group_idx(&self) -> u32 {
        self.group
    }

    pub fn set_group(&mut self, group: u32) {
        self.group = group;
    }
}

impl<'a> TableRef<'a, FileDescriptor> {
    pub fn data(&self) -> TableRef<'a, FileData> {
        self.archive().get_file_data(self.file_data).unwrap()
    }
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

impl FileInfo {
    // const RESHARED_FILE_PATH_BIT: u32 = 0x80000000;
    pub fn new(path: u32, entity: u32, desc: u32, flags: FileInfoFlags) -> Self {
        Self {
            path,
            entity,
            desc,
            flags,
        }
    }

    pub fn is_regional(&self) -> bool {
        self.flags.intersects(FileInfoFlags::IS_REGIONAL)
    }

    pub fn is_localized(&self) -> bool {
        self.flags.intersects(FileInfoFlags::IS_LOCALIZED)
    }

    fn desc_index(&self) -> u32 {
        if self.is_regional() {
            self.desc + LocalePreferences::get().region as u32 + 1
        } else if self.is_localized() {
            self.desc + LocalePreferences::get().locale as u32 + 1
        } else {
            self.desc
        }
    }

    pub fn flags(&self) -> FileInfoFlags {
        self.flags
    }

    pub fn set_flags(&mut self, flags: FileInfoFlags) {
        self.flags = flags;
    }

    pub fn set_non_localized(&mut self) {
        self.flags &= !(FileInfoFlags::IS_LOCALIZED | FileInfoFlags::IS_REGIONAL)
    }

    #[track_caller]
    pub fn set_as_reshared(&mut self) {
        assert!(!self.flags.intersects(FileInfoFlags::IS_RESHARED));
        self.flags |= FileInfoFlags::IS_RESHARED;
    }

    #[track_caller]
    pub fn set_as_group_fixed(&mut self) {
        assert!(!self.flags.intersects(FileInfoFlags::IS_GROUP_FIXED));
        self.flags |= FileInfoFlags::IS_GROUP_FIXED;
    }

    pub fn set_path(&mut self, path: u32) {
        self.path = path;
    }

    pub fn set_entity(&mut self, entity: u32) {
        self.entity = entity;
    }

    #[track_caller]
    pub fn set_desc(&mut self, desc: u32) {
        // We panic here because we should be very explicit about when we are making a file
        // no-longer regional or localized. Simply doing it here swallows potential implementation
        // errors
        if self
            .flags
            .intersects(FileInfoFlags::IS_LOCALIZED | FileInfoFlags::IS_REGIONAL)
        {
            panic!("Cannot set descriptor for localized/regional file, please set the file as non-localized first");
        }
        self.desc = desc;
    }
}

pub enum TryFilePathResult<'a> {
    FilePath(TableRef<'a, FilePath>),
    Reshared(TableRef<'a, FilePath>),
    Missing,
}

impl<'a> TryFilePathResult<'a> {
    pub fn unwrap(self) -> TableRef<'a, FilePath> {
        match self {
            Self::FilePath(path) | Self::Reshared(path) => path,
            Self::Missing => panic!("FilePath is missing"),
        }
    }
}

impl<'a> TableRef<'a, FileInfo> {
    pub fn try_file_path(&self) -> TryFilePathResult<'a> {
        let Some(path) = self.archive().get_file_path(self.path) else {
            return TryFilePathResult::Missing;
        };

        if self.flags.intersects(FileInfoFlags::IS_RESHARED) {
            TryFilePathResult::Reshared(path)
        } else {
            TryFilePathResult::FilePath(path)
        }
    }

    pub fn file_path(&self) -> TableRef<'a, FilePath> {
        self.archive().get_file_path(self.path).unwrap()
    }

    pub fn entity(&self) -> TableRef<'a, FileEntity> {
        self.archive().get_file_entity(self.entity).unwrap()
    }

    pub fn desc(&self) -> TableRef<'a, FileDescriptor> {
        self.archive().get_file_desc(self.desc).unwrap()
    }
}

impl<'a> TableMut<'a, FileInfo> {
    pub fn path_ref(&self) -> TableRef<'_, FilePath> {
        self.archive().get_file_path(self.path).unwrap()
    }

    pub fn path_mut(&mut self) -> TableMut<'_, FilePath> {
        let index = self.path;
        self.archive_mut().get_file_path_mut(index).unwrap()
    }

    pub fn path(self) -> TableMut<'a, FilePath> {
        let index = self.path;
        self.into_archive_mut().get_file_path_mut(index).unwrap()
    }

    pub fn entity_ref(&self) -> TableRef<'_, FileEntity> {
        self.archive().get_file_entity(self.entity).unwrap()
    }

    pub fn entity_mut(self) -> TableMut<'a, FileEntity> {
        let index = self.entity;
        self.into_archive_mut().get_file_entity_mut(index).unwrap()
    }

    pub fn desc_ref(&self) -> TableRef<'_, FileDescriptor> {
        self.archive().get_file_desc(self.desc_index()).unwrap()
    }

    pub fn desc_mut(&mut self) -> TableMut<'_, FileDescriptor> {
        let index = self.desc_index();

        self.archive_mut().get_file_desc_mut(index).unwrap()
    }

    pub fn desc(self) -> TableMut<'a, FileDescriptor> {
        let index = self.desc_index();
        self.into_archive_mut().get_file_desc_mut(index).unwrap()
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct FileEntity {
    package_or_group: u32,
    info: u32,
}

impl FileEntity {
    pub fn new(package_or_group: u32, info: u32) -> Self {
        Self {
            package_or_group,
            info,
        }
    }

    pub fn package_or_group(&self) -> u32 {
        self.package_or_group
    }

    pub fn set_info(&mut self, index: u32) {
        self.info = index;
    }
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

impl FilePath {
    pub fn from_utf8_path(path: impl AsRef<Utf8Path>) -> Self {
        let path = path.as_ref();
        let parent_str = path.parent().unwrap().as_str();
        let mut parent = Hash40::const_new(parent_str);
        if !parent_str.ends_with('/') {
            parent = parent.const_with("/");
        }
        let file_name = path
            .file_name()
            .map(Hash40::const_new)
            .unwrap_or(Hash40::from_raw(0));
        let extension = path
            .extension()
            .map(Hash40::const_new)
            .unwrap_or(Hash40::from_raw(0));
        let path = Hash40::const_new(path.as_str());

        Self::from_parts(path, parent, file_name, extension, 0xFFFFFF)
    }

    pub fn from_parts(
        path: Hash40,
        parent: Hash40,
        file_name: Hash40,
        extension: Hash40,
        entity: u32,
    ) -> Self {
        Self {
            path_and_entity: HashWithData::new(path, entity),
            ext_and_version: HashWithData::new(extension, 0xFFFFFF),
            parent: Hash::from_hash40(parent),
            file_name: Hash::from_hash40(file_name),
        }
    }

    pub fn path(&self) -> Hash40 {
        self.path_and_entity.hash40()
    }

    pub fn parent(&self) -> Hash40 {
        self.parent.hash40()
    }

    pub fn file_name(&self) -> Hash40 {
        self.file_name.hash40()
    }

    pub fn extension(&self) -> Hash40 {
        self.ext_and_version.hash40()
    }
}

impl<'a> TableRef<'a, FilePath> {
    pub fn entity(&self) -> TableRef<'a, FileEntity> {
        self.archive()
            .get_file_entity(self.path_and_entity.data())
            .unwrap()
    }
}

impl<'a> TableMut<'a, FilePath> {
    pub fn set_entity(&mut self, entity: u32) {
        self.path_and_entity.set_data(entity);
    }

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

impl FilePackage {
    pub fn new(
        path: impl IntoHash,
        name: impl IntoHash,
        parent: impl IntoHash,
        group: u32,
    ) -> Self {
        Self {
            path_and_group: HashWithData::new(path.into_hash(), group),
            name: Hash::from_hash40(name.into_hash()),
            parent: Hash::from_hash40(parent.into_hash()),
            lifetime: Hash::from_hash40(Hash40::const_new("")),
            info_start: 0,
            info_count: 0,
            child_start: 0,
            child_count: 0,
            flags: FilePackageFlags::empty(),
        }
    }

    pub fn path(&self) -> Hash40 {
        self.path_and_group.hash40()
    }

    pub fn name(&self) -> Hash40 {
        self.name.hash40()
    }

    pub fn parent(&self) -> Hash40 {
        self.parent.hash40()
    }

    pub fn set_child_package_range(&mut self, start: u32, count: u32) {
        self.child_start = start;
        self.child_count = count;
    }

    pub fn has_file_group(&self) -> bool {
        (self.flags & (FilePackageFlags::HAS_SUB_PACKAGE | FilePackageFlags::IS_SYM_LINK))
            == FilePackageFlags::HAS_SUB_PACKAGE
    }

    pub fn has_sym_link(&self) -> bool {
        let flags = FilePackageFlags::HAS_SUB_PACKAGE | FilePackageFlags::IS_SYM_LINK;
        (self.flags & flags) == flags
    }

    pub fn set_info_range(&mut self, start: u32, count: u32) {
        self.info_start = start;
        self.info_count = count;
    }

    pub fn flags(&self) -> FilePackageFlags {
        self.flags
    }

    pub fn set_flags(&mut self, flags: FilePackageFlags) {
        self.flags = flags;
    }
}

impl<'a> TableRef<'a, FilePackage> {
    pub fn data_group(&self) -> TableRef<'a, FileGroup> {
        self.archive()
            .get_file_group(self.path_and_group.data())
            .unwrap()
    }

    pub fn sym_link(&self) -> TableRef<'a, FilePackage> {
        assert!(self.has_sym_link());

        let dg = self
            .archive()
            .get_file_group(self.path_and_group.data())
            .unwrap();

        self.archive().get_file_package(dg.redirection).unwrap()
    }

    pub fn file_group(&self) -> Option<TableRef<'a, FileGroup>> {
        assert!(self.has_file_group());

        let dg = self
            .archive()
            .get_file_group(self.path_and_group.data())
            .unwrap();

        self.archive().get_file_group(dg.redirection)
    }

    pub fn child_packages(&self) -> TableSliceRef<'a, FilePackageChild> {
        self.archive()
            .get_file_package_child_slice(self.child_start, self.child_count)
            .unwrap()
    }

    pub fn infos(&self) -> TableSliceRef<'a, FileInfo> {
        self.archive()
            .get_file_info_slice(self.info_start, self.info_count)
            .unwrap()
    }
}

#[repr(transparent)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct FilePackageChild(HashWithData);

impl FilePackageChild {
    pub fn new(path: Hash40, index: u32) -> Self {
        Self(HashWithData::new(path, index))
    }

    pub fn path(&self) -> Hash40 {
        self.0.hash40()
    }
}

impl<'a> TableRef<'a, FilePackageChild> {
    pub fn package(&self) -> TableRef<'a, FilePackage> {
        self.archive().get_file_package(self.0.data()).unwrap()
    }
}

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

impl FileGroup {
    pub fn new_for_new_package() -> Self {
        Self {
            archive_offset: [0; 2],
            decompressed_size: 0x10,
            compressed_size: 0x10,
            child_count: 0,
            child_start: 0,
            redirection: 0xffffff,
        }
    }

    pub fn redirection(&self) -> u32 {
        self.redirection
    }

    pub fn set_redirection(&mut self, redirection: u32) {
        self.redirection = redirection;
    }

    pub fn compressed_size(&self) -> u32 {
        self.compressed_size
    }

    pub fn set_compressed_size(&mut self, new_size: u32) {
        self.compressed_size = new_size;
    }
}

impl<'a> TableRef<'a, FileGroup> {
    pub fn file_info_slice(&self) -> TableSliceRef<'a, FileInfo> {
        self.archive()
            .get_file_info_slice(self.child_start, self.child_count)
            .unwrap()
    }
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

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct SearchFolder {
    path_and_folder_count: HashWithData,
    parent_and_file_count: HashWithData,
    name: Hash,
    first_child_index: u32,
    _padding: u32,
}

impl SearchFolder {
    pub fn new(path: impl IntoHash, parent: impl IntoHash, name: impl IntoHash) -> Self {
        Self {
            path_and_folder_count: HashWithData::new(path.into_hash(), 0x0),
            parent_and_file_count: HashWithData::new(parent.into_hash(), 0x0),
            name: Hash::from_hash40(name.into_hash()),
            first_child_index: u32::MAX,
            _padding: 0x0,
        }
    }

    pub fn path(&self) -> Hash40 {
        self.path_and_folder_count.hash40()
    }

    pub fn folder_count(&self) -> u32 {
        self.path_and_folder_count.data()
    }

    pub fn set_folder_count(&mut self, count: u32) {
        self.path_and_folder_count.set_data(count);
    }

    pub fn name(&self) -> Hash40 {
        self.name.hash40()
    }

    pub fn parent(&self) -> Hash40 {
        self.parent_and_file_count.hash40()
    }

    pub fn file_count(&self) -> u32 {
        self.parent_and_file_count.data()
    }

    pub fn set_file_count(&mut self, count: u32) {
        self.parent_and_file_count.set_data(count);
    }

    pub fn set_first_child_index(&mut self, index: u32) {
        self.first_child_index = index;
    }

    pub fn has_first_child(&self) -> bool {
        self.first_child_index != u32::MAX
    }
}

impl<'a> TableRef<'a, SearchFolder> {
    pub fn first_child(&self) -> TableRef<'a, SearchPath> {
        self.archive()
            .get_search_path_link(self.first_child_index)
            .unwrap()
            .path()
    }
}

impl<'a> TableMut<'a, SearchFolder> {
    pub fn first_child_ref(&self) -> TableRef<'_, SearchPath> {
        self.archive()
            .get_search_path_link(self.first_child_index)
            .unwrap()
            .path()
    }

    pub fn first_child_mut(&mut self) -> TableMut<'_, SearchPath> {
        let index = self.first_child_index;
        self.archive_mut()
            .get_search_path_link_mut(index)
            .unwrap()
            .path()
    }

    pub fn first_child(self) -> TableMut<'a, SearchPath> {
        let index = self.first_child_index;
        self.into_archive_mut()
            .get_search_path_link_mut(index)
            .unwrap()
            .path()
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct SearchPath {
    path_and_next_index: HashWithData,
    parent_and_is_folder: HashWithData,
    name: Hash,
    extension: Hash,
}

impl SearchPath {
    const IS_FOLDER_BIT: u32 = 0x0040_0000;

    pub fn new_folder(path: impl IntoHash, parent: impl IntoHash, name: impl IntoHash) -> Self {
        Self {
            path_and_next_index: HashWithData::new(path.into_hash(), 0xFFFFFF),
            parent_and_is_folder: HashWithData::new(parent.into_hash(), Self::IS_FOLDER_BIT),
            name: Hash::from_hash40(name.into_hash()),
            extension: Hash::from_hash40(Hash40::const_new("")),
        }
    }

    pub fn new(
        path: impl IntoHash,
        parent: impl IntoHash,
        name: impl IntoHash,
        extension: impl IntoHash,
    ) -> Self {
        Self {
            path_and_next_index: HashWithData::new(path.into_hash(), 0xFFFFFF),
            parent_and_is_folder: HashWithData::new(parent.into_hash(), 0x0),
            name: Hash::from_hash40(name.into_hash()),
            extension: Hash::from_hash40(extension.into_hash()),
        }
    }

    pub fn from_file_path(path: &FilePath) -> Self {
        Self::new(
            path.path(),
            path.parent().const_trim_trailing("/"),
            path.file_name(),
            path.extension(),
        )
    }

    pub fn path(&self) -> Hash40 {
        self.path_and_next_index.hash40()
    }

    pub fn parent(&self) -> Hash40 {
        self.parent_and_is_folder.hash40()
    }

    pub fn name(&self) -> Hash40 {
        self.name.hash40()
    }

    pub fn extension(&self) -> Option<Hash40> {
        if self.is_folder() {
            None
        } else {
            Some(self.extension.hash40())
        }
    }

    pub fn is_folder(&self) -> bool {
        self.parent_and_is_folder.data() & Self::IS_FOLDER_BIT != 0
    }

    pub fn is_end(&self) -> bool {
        self.path_and_next_index.data() == 0xFFFFFF
    }

    pub fn set_end(&mut self) {
        self.set_next_index(0xFFFFFF);
    }

    pub fn set_next_index(&mut self, index: u32) {
        self.path_and_next_index.set_data(index);
    }
}

impl<'a> TableRef<'a, SearchPath> {
    pub fn next(&self) -> TableRef<'a, SearchPath> {
        assert!(!self.is_end());
        self.archive()
            .get_search_path_link(self.path_and_next_index.data())
            .unwrap()
            .path()
    }

    pub fn as_folder(&self) -> TableRef<'a, SearchFolder> {
        assert!(self.is_folder());
        self.archive().lookup_search_folder(self.path()).unwrap()
    }
}

impl<'a> TableMut<'a, SearchPath> {
    pub fn next_ref(&self) -> TableRef<'_, SearchPath> {
        assert!(!self.is_end());
        self.archive()
            .get_search_path_link(self.path_and_next_index.data())
            .unwrap()
            .path()
    }

    pub fn next_mut(&mut self) -> TableMut<'_, SearchPath> {
        assert!(!self.is_end());
        let index = self.path_and_next_index.data();
        self.archive_mut()
            .get_search_path_link_mut(index)
            .unwrap()
            .path()
    }

    pub fn next(self) -> TableMut<'a, SearchPath> {
        assert!(!self.is_end());
        let index = self.path_and_next_index.data();
        self.into_archive_mut()
            .get_search_path_link_mut(index)
            .unwrap()
            .path()
    }

    pub fn as_folder_ref(&self) -> TableRef<'_, SearchFolder> {
        assert!(self.is_folder());
        self.archive().lookup_search_folder(self.path()).unwrap()
    }

    pub fn as_folder_mut(&mut self) -> TableMut<'_, SearchFolder> {
        assert!(self.is_folder());
        let path = self.path();
        self.archive_mut().lookup_search_folder_mut(path).unwrap()
    }

    pub fn into_folder(self) -> TableMut<'a, SearchFolder> {
        assert!(self.is_folder());
        let path = self.path();
        self.into_archive_mut()
            .lookup_search_folder_mut(path)
            .unwrap()
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub struct SearchPathLink(u32);

impl SearchPathLink {
    pub const fn new(path_index: u32) -> Self {
        Self(path_index)
    }

    pub const fn path_index(&self) -> u32 {
        self.0
    }

    pub const fn invalid() -> Self {
        Self(u32::MAX)
    }

    pub const fn is_invalid(&self) -> bool {
        self.0 == u32::MAX
    }
}

impl<'a> TableRef<'a, SearchPathLink> {
    pub fn path(&self) -> TableRef<'a, SearchPath> {
        assert!(!self.is_invalid());
        self.archive().get_search_path(self.0).unwrap()
    }

    pub fn try_path(&self) -> Option<TableRef<'a, SearchPath>> {
        if self.is_invalid() {
            None
        } else {
            Some(self.archive().get_search_path(self.0).unwrap())
        }
    }
}

impl<'a> TableMut<'a, SearchPathLink> {
    pub fn path_ref(&self) -> TableRef<'_, SearchPath> {
        assert!(!self.is_invalid());
        self.archive().get_search_path(self.0).unwrap()
    }

    pub fn path_mut(&mut self) -> TableMut<'_, SearchPath> {
        assert!(!self.is_invalid());
        let index = self.0;
        self.archive_mut().get_search_path_mut(index).unwrap()
    }

    pub fn path(self) -> TableMut<'a, SearchPath> {
        assert!(!self.is_invalid());
        let index = self.0;
        self.into_archive_mut().get_search_path_mut(index).unwrap()
    }
}
