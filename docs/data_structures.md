# Archive Data Structures

The `data.arc` contains a few filesystem tables that contain metadata required for locating
files at runtime, and understanding how to load them.

There are four "filesystems" supported by the `data.arc`
- `stream`: Contains uncompressed file data that contians audio
and video files
- `packaged`: Contains all metadata required by the resource service
to understand how to properly load and populate file data at runtime.
- `versioned`: Contains file data from previous revisions of the `data.arc`. These
are unused in the base game as far as we can tell.
- `search`: Contains a tree-like structure that can be used to traverse the filesystem
in all directions -- very similar to a traditional file explorer view.

*NOTE: The names for these filesystems is not official and is a byproduct of reverse
engineering the intentions of the filesystems.*

### Basic Concepts
All of the `data.arc` structures are aligned to `0x4` bytes, even if they have
a 64-bit integer in them. This can be represented by using `packed` alignment in code,
or you can just split the 64-bit integer into two 32-bit integers and combine them
at runtime (what Stratus does).

There are a couple common types that you'll see throughout this document:
- `Hash40` - The `Hash40` is the name granted by the research community to how
SSBU represents its strings. Instead of doing operations on the string
`"fighter/mario/model/body/c00/model.numdlb"`, in most areas SSBU's codebase transforms
this (likely at compile time) into a type that contains the combination of the string
length and it's CRC32. So the `Hash40` for the example file path would be `0x291190785f`.
`0x29` is the length of the string, `0x1190785f` is the CRC32 of the string, so after bit shifting
the length to the left by 32-bits and ORing the two values together you get the `Hash40`.
- `Index24` - The `Index24` is a 24-bit index that is used to index into the `data.arc` tables.
It is 24-bits because it is sometimes packed into the same 64-bit integer that includes a `Hash40`.
When not packed like that, the `Index24` has the alignment of a 32-bit integer. Sometimes the
upper 8 bits are unused, and sometimes they are used for flags.
- `Hash40WithData` - This is a structure that packs a `Hash40` and 24-bit integer (not always an `Index24`)
into the same 64-bit integer. In locations where this is used, this document will explain what they are used for.

## Packaged Filesystem
The packaged filesystem is the most complex of the three. This is where the 
"meat and potatoes" of SSBU's resource service is implemented.

To begin, let's talk about a key difference: file paths vs. file entities.

A file path means one file in the archive. There can be multiple file paths that point
to the same "file entity", which is the representation of *unique* file data.

What does that mean in practice? It means that there can be two file paths:
- `fighter/mario/model/body/c00/model.numdlb`
- `fighter/mario/model/body/c01/model.numdlb`

And both of these will have a unique `FilePath` structure, but share the same
`FileEntity` structure. This saves on runtime memory usage and loading times,
as well as disk space required to store the archive's files.

The way this is implemented in practice varies depending on the kind of path
that you are observing.

### `FilePath`
```rs
pub struct FilePath {
    path_and_entity_index: Hash40WithData, 
    ext_and_version: Hash40WithData,
    parent: Hash40,
    file_name: Hash40,
}
```

- `path_and_entity_index` - Contains a `Hash40` for the full path of the file
as well as an `Index24` into the `FileEntity` table for locating this file's
data.
- `ext_and_version` - Contains a `Hash40` for the extension of the file (i.e. `nutexb`)
as well as an `Index24` into the versioned file table for the previous version of this file.
- `parent` - `Hash40` for the parent folder of this file. The hash of the parent is always
terminated with `/`.
- `file_name` - `Hash40` for the file name of this file (i.e. `model.numdlb`)

There is exactly one of these for every file path in the archive.

### `FileEntity`
```rs
pub struct FileEntity {
    pub package_or_group_index: Index24,
    pub file_info_index: Index24
}
```
- `package_or_group_index` - The index of the `FilePackage`, or the `FileGroup`, which contains
the `FileInfo` referenced by `file_info_index`. If the `package_or_group_index >= NUM_FILE_PACKAGE`,
where `NUM_FILE_PACKAGE` is the number of packages in the package table, then this index refers to a `FileGroup`.
- `file_info_index` - The index into the `FileInfo` table

There is exactly one of these for each unique piece of file data in the archive, many paths can point to the
same `FileEntity`.

### `FileInfo`
```rs
pub struct FileInfo {
    file_path_index: Index24,
    file_desc_index: Index24,
    alignment_and_flags: u32
}
```
- `file_path_index` - The index into the `FilePath` table for this `FileInfo`.
- `file_desc_index` - The start index into the `FileDescriptor` table for this `FileInfo`. 
- `alignemnt_and_flags` - This is a bitfield
    - Bits 0-15: The required memory alignment when allocating a buffer for this file. In practice this is either
    `0x10` or `0x1000`. If it is `0x00`, then the resource service defaults to a value set in the filesystem table header (which is `0x10`).
        - `let alignment = info.alignment_and_flags & 0x7FFF;`
    - Bit 16: Flag indicating if the file is localized. Mutually exclusive with the regional flag
        - `let is_localized = info.alignment_and_flags & 0x10000 != 0;`
    - Bit 17: Flag indicating if the file is regional. Mutally exclusive with the localized flag
        - `let is_regional = info.alignment_and_flags & 0x20000 != 0;`
    - Bit 20: Flag indicating if the file is shared. Note that this file is not always set on shared files it seems, but it is not found on any files that are not shared.
        - `let is_shared = info.alignment_and_flags & 0x100000 != 0;`
    - Bit 21: Unknown flag that is only ever set when the shared flag is set. It doesn't seem to be checked in the existing resource service implementation.
        - `let is_unknown = inof.alignment_and_flags & 0x200000 != 0;`

When the regional or localized flag is set, `file_desc_index` points to the first index of an array of `NUM_REGIONS + 1` or `NUM_LOCALES + 1` descriptors, depending on which flag is set. The first descriptor in that array is invalid
and should not be used for loading data. The next 5 are valid depending on their flags. 

There can be multiple `FileInfo` for the same `FilePath`. These are used as the entries in `FilePackage`s, and since
more than one `FilePackage` can refer to the same file, there will be more than one `FileInfo` per `FilePath` for some files. In the case of some files (like model and texture files shared between fighter costumes), there can also
be `FileGroup`s that point to `FileInfo`s where there is a `FilePackage` that *also* points to a `FileInfo` for the same path. There will be a graph at the end of this section.

### `FileDescriptor`
```rs
pub struct FileDescriptor {
    file_group_index: Index24,
    file_data_index: Index24,
    load_method: u32
}
```
- `file_group_index` - Index into the `FileGroup` table that the `FileData` belongs to. Even if this index points to a `FileGroup` that references `FileInfo` instead of `FileData`, the offset information in that `FileGroup` is still correct for loading this file.
- `file_data_index` - Index into the `FileData` table.
- `load_method` - The lower 24-bits of this field are an `Index24` whose use depends on the flags set in the upper 8 bits. We don't fully understand what each bit controls, so instead we've labeled the known combinations of these bits:
    - `0x00 ("Unowned")` - This `FileDescriptor` should not be used to load the file data, since the file is shared. Instead, traversing the tables beginning at the `FileEntity` pointed to by this field's `Index24` will locate the correct data.
    - `0x01 ("Owned")` - This `FileDescriptor` points to valid file data and should be used for loading the file. The `Index24` is an index into the versioned filesystem to indicate which version this descriptor is for.
    - `0x03 ("PackageSkip")` - This `FileDescriptor` should not be used to load the file data **if** it is being loaded as part of a `FilePackage`. The `Index24` points to a `FileInfo`. If this `FileDescriptor` is being loaded as part of a `FileGroup`, then the referenced `FileInfo` is one inside of a `FilePackage`, and if it is being loaded as part of a `FilePackage`, then the referenced `FileInfo` is one inside of a `FileGroup` (that points to the correct data).
    - `0x05 ("Unknown")` - It's unclear what this combination does. It's used by only a few files in the archive.
    - `0x09 ("SharedButOwned")` - Similar to `Unknown`, this combination has unknown effects. It's required to be set to this for the few files that have it, but what it accomplishes or changes in the resource service we are unsure of. The `Index24` is for a `FileEntity`
    - `0x10 ("UnsupportedRegionLocale")` - This is set on the `FileDescriptor`s for regions/locales that don't have distinct file data. For example, in SSBU, some assist trophies have unique file data for `jp_ja` and `en_us`, but no other locales. In this case, all of the other locales will pick one of either `jp_ja` and `en_us` as it's closest locale, and then the `Index24` of this field will be set to that locale's index.

The comparison of `FileDescriptor` <-> `FilePath` begins to fall apart here, so we won't comment on that, but there are many of these.

### `FileData`
```rs
pub struct FileData {
    data_group_offset: u32,
    compressed_size: u32,
    decompressed_size: u32,
    flags: u32
}
```
- `data_group_offset` is the offset (in bytes) from the archive offset specified by the `FileGroup` which this
belongs to. It should always be aligned to `0x10` bytes, and if it's not then the resource service aligns it up manually.
- `compressed_size` is the size (in bytes) of the compressed data representing this file. If this file is not compressed, this size is the same as `decompressed_size`. It can be interpreted as the size, in bytes, to read from the archive in order to obtain all of the file data.
- `decompressed_size` is the size (in bytes) of the file when decompressed.
- `flags` - There are 4 flags for `FileData`
    - Bit 0: Indicates whether this file uses ZSTD for compression. If this flag is set, then the compressed flag **must be set**. The resource service aborts if the contrary is true.
        - `let is_zstd_compression = data.flags & 0x1;`
    - Bit 1: Indicates whether this file is compressed. This can be set without the ZSTD flag being set. We don't know which compression algorithm is used for those files, but none of the files in the archive currently utilize it.
        - `let is_compressed = data.flags & 0x2;`
    - Bit 2: Indicates whether this file is regional and versioned data. Mutually exclusive with the localized flag.
        - `let is_regional_and_versioned = data.flags & 0x4;`
    - Bit 3: Indicates whether this file is localized and versioned data. Mutually exclusive with the regional flag.
        - `let is_localized_and_versioned = data.flags & 0x8;`