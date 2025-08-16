use std::io::{Read, Seek};

use bytemuck::{Pod, Zeroable};

fn ru32<R: Read>(reader: &mut R) -> u32 {
    let mut bytes = [0; std::mem::size_of::<u32>()];
    assert_eq!(reader.read(&mut bytes).unwrap(), std::mem::size_of::<u32>());

    u32::from_le_bytes(bytes)
}

#[track_caller]
fn read_exact_size<R: Read>(reader: &mut R, size: usize) -> Vec<u8> {
    let mut uninit_vec = Vec::with_capacity(size);
    // SAFETY: We require that the entire vec is filled by the reader
    unsafe {
        uninit_vec.set_len(size);
    }

    let count = reader
        .read(&mut uninit_vec)
        .expect("Failed to read from reader");

    // We don't need to worry about the panic unwindining dropping the uninit data since it's just u8 anyways
    if count < uninit_vec.len() {
        panic!("Failed to fill whole buffer");
    }

    uninit_vec
}

#[repr(C)]
struct ZstdBuffer {
    ptr: *mut u8,
    size: usize,
    pos: usize,
}

#[skyline::from_offset(0x39a2fc0)]
fn decompress_stream(unk: *mut u64, output: &mut ZstdBuffer, input: &mut ZstdBuffer) -> usize;

#[skyline::from_offset(0x35410b0)]
fn initialize_decompressor(ptr: *mut u64) -> u64;

#[skyline::from_offset(0x3541030)]
fn finalize_decompressor(ptr: *mut u64);

pub fn read_compressed_section<R: Read + Seek>(reader: &mut R) -> Vec<u8> {
    const REQUIRED_TABLE_SIZE: u32 = 0x10;

    let start = reader.stream_position().unwrap();
    assert_eq!(ru32(reader), REQUIRED_TABLE_SIZE);

    let decompressed_size = ru32(reader);
    let compressed_size = ru32(reader);
    let offset_to_next = ru32(reader);

    let mut compressed = read_exact_size(reader, compressed_size as usize);

    // SAFETY: Reference call at 0x3540bb4 - instructions initialize 0x10 bytes worth of space and pass it to
    // what we've labeled `initialize_decompressor`, it is the constructor for this type. It only uses
    // the decompression codepath if the return value of the constructor is 0
    let mut decompressor = [0u64; 2];
    unsafe {
        assert_eq!(initialize_decompressor(decompressor.as_mut_ptr()), 0);
    }

    let mut decompressed = Vec::with_capacity(decompressed_size as usize);
    // SAFETY: We assert that the count of bytes read by the compressor is the same as decompressed_size before
    // returning the vec.
    unsafe {
        decompressed.set_len(decompressed_size as usize);
    }

    let mut input_buffer = ZstdBuffer {
        ptr: compressed.as_mut_ptr(),
        size: compressed.len(),
        pos: 0,
    };

    let mut output_buffer = ZstdBuffer {
        ptr: decompressed.as_mut_ptr(),
        size: decompressed.len(),
        pos: 0,
    };

    // SAFETY: Reference call at 0x3540ca4 - Passes the second u64 from the initialize_decompressor call as well as two
    // pointer buffers that match the structure as they are defined in this file: output first then input
    let result = unsafe {
        decompress_stream(
            decompressor[1] as *mut u64,
            &mut output_buffer,
            &mut input_buffer,
        )
    };

    // NOTE: Call the destructor first since if we panic it will not exit with RAII (too lazy to write a struct for this)
    // SAFETY: Destructor for the decompressor. Disassembly does not help ensure that this is the deconstructor,
    // but in practice it seems to be fine and the function frees/releases data
    unsafe {
        finalize_decompressor(decompressor.as_mut_ptr());
    }

    // Negative value for result is error code
    assert_eq!(result, 0x0, "{result}");
    assert_eq!(output_buffer.pos, decompressed_size as usize);

    // This should seek past the compressed section and go to the start of the next valid data
    // This might skip past more bytes than are in the compressed section, but that's how the file format
    // was designed
    reader
        .seek(std::io::SeekFrom::Start(start + offset_to_next as u64))
        .unwrap();

    decompressed
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct ArchiveMetadata {
    pub magic: u64,
    pub stream_data_offset: u64,
    pub file_data_offset: u64,
    pub shared_file_data_offset: u64,
    pub resource_table_offset: u64,
    pub user_table_offset: u64,
    pub unknown_table_offset: u64,
}

impl ArchiveMetadata {
    const MAGIC: u64 = 0xABCDEF9876543210;
}

impl ArchiveMetadata {
    pub fn read<R: Read>(reader: &mut R) -> Self {
        let bytes = read_exact_size(reader, std::mem::size_of::<Self>());

        let this: &Self = bytemuck::from_bytes(&bytes);
        assert_eq!(this.magic, Self::MAGIC);
        *this
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
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
    locale_hash_to_region: [[u32; 3]; 14],

    stream_folder_count: u32,
    stream_path_count: u32,
    stream_desc_count: u32,
    stream_data_count: u32,
}

impl ResourceTableHeader {
    pub fn read<R: Read>(reader: &mut R) -> Self {
        let bytes = read_exact_size(reader, std::mem::size_of::<Self>());
        *bytemuck::from_bytes(&bytes)
    }
}
