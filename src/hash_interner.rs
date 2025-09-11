use std::{
    alloc::Layout,
    fmt::{Debug, Display, Write},
    num::NonZeroUsize,
    ops::Range,
};

use camino::Utf8Path;
use smash_hash::{Hash40, Hash40Map};

const IS_INTERNED_COMPONENT: u32 = 1u32 << 23;

#[derive(Copy, Clone)]
#[allow(non_camel_case_types)]
#[repr(transparent)]
pub struct u24([u8; 3]);

impl u24 {
    #[rustfmt::skip]
    pub const fn to_u32(self) -> u32 {
        let Self([a, b, c]) = self;
        u32::from_le_bytes([a, b, c, 0])
    }

    pub const fn from_u32(n: u32) -> Self {
        let [a, b, c, d] = n.to_le_bytes();
        assert!(d == 0);
        Self([a, b, c])
    }
}

impl Debug for u24 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        <u32 as Debug>::fmt(&self.to_u32(), f)
    }
}

// 3 MB
// This memory region is not to exceed 16MB so that we can use SmolRange for range definitions
const HASH_MEMORY_SLAB_SIZE: usize = 3 * 1024 * 1024; // 4 MB

// Roughly 0.4MB (size_of::<SmolRange>() * DEFAULT_STRING_MEMORY_SLAB_SIZE)
const STRING_MEMORY_SLAB_SIZE: usize = 100_000;

// size_of::<(ShiftedHash, SmolRange)> * HASH_BUCKET_COUNT * HASH_BUCKET_SIZE ~= 6 MB
// HASH_BUCKET_COUNT CANNOT CHANGE
const HASH_BUCKET_COUNT: usize = 0x100;
const HASH_BUCKET_SIZE: usize = 0xC00;

// 16 MB
// This memory region is not to exceed 16MB so that we can use SmolRange for range definitions
const INTERNED_PATH_SLAB_SIZE: usize = (4 * 1024 * 1024) / std::mem::size_of::<u24>();

/// # Safety
/// The returned memory from this function is **not** initialized, which means that the caller must
/// be cautious not to use it to return references to uninitialized memory
pub unsafe fn allocate_uninit(size: NonZeroUsize, align: NonZeroUsize) -> Box<[u8]> {
    // PERF: Unwrapping/asserting here is fine, this functinon is only going to get called a handful of times during init
    let layout = Layout::from_size_align(size.get(), align.get()).unwrap();
    assert!(layout.size() > 0);

    // SAFETY: Layout is both valid and has a size greater than 0
    let memory = unsafe { std::alloc::alloc(layout) };

    if memory.is_null() {
        panic!(
            "Failed to allocate memory region of size {} bytes (capacity of {})",
            layout.size(),
            size,
        );
    }

    // SAFETY:
    // - memory is a valid pointer, as checked above
    //  - as long as allocator is correct for system, this allocation should be contiguous memory
    // - layout.size() is lt or eq to isize::MAX
    //
    let bytes = std::ptr::slice_from_raw_parts_mut(memory, layout.size());

    // SAFETY: We are using the global allocator for allocating this memory, which will be the same as this box
    unsafe { Box::from_raw(bytes) }
}

#[repr(transparent)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct SmolRange(u32);

impl SmolRange {
    pub const fn new(len: u8, start: u24) -> Self {
        Self(((len as u32) << 24) | start.to_u32())
    }

    const fn len(self) -> u8 {
        ((self.0 & 0xFF000000) >> 24) as u8
    }

    const fn start(self) -> u24 {
        u24::from_u32(self.0 & 0x00FFFFFF)
    }

    #[allow(dead_code)]
    pub fn range(&self) -> Range<u32> {
        self.start().to_u32()..self.start().to_u32() + self.len() as u32
    }
}

pub struct HashLookupKey {
    pub shifted_hash: u32,
    pub range: SmolRange,
}

// impl HashLookupKey {
//     const fn from_hash_and_range(hash: Hash40, range: SmolRange) -> Self {
//         Self {
//             shifted_hash: hash
//         }
//     }
// }

pub struct InternerCache {
    component_index: Hash40Map<u24>,
    cached_paths: Hash40Map<u24>,
    previous_bucket_lengths: Vec<usize>,
}

impl InternerCache {
    fn new() -> Self {
        Self {
            component_index: Hash40Map::default(),
            cached_paths: Hash40Map::default(),
            previous_bucket_lengths: vec![],
        }
    }
}

pub struct HashMemorySlab {
    total_blob_size: usize,

    bytes: *mut [u8],
    byte_len: usize,

    strings: *mut [SmolRange],
    string_len: usize,

    components: *mut [u24],
    component_len: usize,

    hashes: *mut [HashLookupKey],
    bucket_lengths: *mut [u32],

    was_finalized: bool,
}

#[derive(Debug, Copy, Clone)]
pub struct MemoryUsageFraction {
    pub numer: usize,
    pub denom: usize,
}

impl Display for MemoryUsageFraction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} / {}", self.numer, self.denom)
    }
}

pub struct MemoryUsageReport {
    pub total_blob_size: usize,
    pub bytes: MemoryUsageFraction,
    pub strings: MemoryUsageFraction,
    pub components: MemoryUsageFraction,
    pub hashes: MemoryUsageFraction,
}

impl Display for MemoryUsageReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Memory Usage Report:")?;
        writeln!(f, "\tTOTAL SIZE:      {}b", self.total_blob_size)?;
        writeln!(f, "\tTEXT:            {}", self.bytes)?;
        writeln!(f, "\tSTRING SLICES:   {}", self.strings)?;
        writeln!(f, "\tHASH COMPONENTS: {}", self.components)?;
        writeln!(f, "\tHASH SLICES:     {}", self.hashes)?;

        let total_numer =
            self.bytes.numer + self.strings.numer + self.components.numer + self.hashes.numer;
        let total_denom =
            self.bytes.denom + self.strings.denom + self.components.denom + self.hashes.denom;

        writeln!(
            f,
            "\tTOTAL CONSUMED:  {} / {} ({:.2}%)",
            total_numer,
            total_denom,
            (total_numer as f32 / total_denom as f32) * 100.0
        )
    }
}

#[allow(dead_code)]
pub struct InternPathResult {
    pub range: SmolRange,
    pub is_new: bool,
}

impl HashMemorySlab {
    fn init(get_memory: impl FnOnce(NonZeroUsize, NonZeroUsize) -> Box<[u8]>) -> Self {
        const fn align_up(value: usize, align: usize) -> usize {
            (value + (align - 1)) & !(align - 1)
        }

        // PERF: Unwrapping is fine here since this should only get called once by user
        let align = [
            align_of::<u8>(),
            align_of::<SmolRange>(),
            align_of::<u24>(),
            align_of::<HashLookupKey>(),
            align_of::<u32>(),
        ]
        .into_iter()
        .max()
        .unwrap();
        assert!(align.is_power_of_two());

        let bytes_offset = 0;
        let string_offset = bytes_offset + align_up(HASH_MEMORY_SLAB_SIZE, align);
        let component_offset =
            string_offset + align_up(STRING_MEMORY_SLAB_SIZE * size_of::<SmolRange>(), align);
        let lookup_offset =
            component_offset + align_up(INTERNED_PATH_SLAB_SIZE * size_of::<u24>(), align);
        let bucket_length_offset = lookup_offset
            + align_up(
                HASH_BUCKET_COUNT * HASH_BUCKET_SIZE * size_of::<HashLookupKey>(),
                align,
            );
        let size = bucket_length_offset + align_up(HASH_BUCKET_COUNT * size_of::<u32>(), align);

        let mut blob = get_memory(
            NonZeroUsize::new(size).unwrap(),
            NonZeroUsize::new(align).unwrap(),
        );

        let blob_size = blob.len();
        // SAFETY: Addend is within range of valid values (0 <= value <= isize::MAX)
        let bytes = unsafe {
            std::ptr::slice_from_raw_parts_mut(
                blob.as_mut_ptr().add(bytes_offset),
                HASH_MEMORY_SLAB_SIZE,
            )
        };
        // SAFETY: See above
        let strings = unsafe {
            std::ptr::slice_from_raw_parts_mut(
                blob.as_mut_ptr().add(string_offset).cast::<SmolRange>(),
                STRING_MEMORY_SLAB_SIZE,
            )
        };
        // SAFETY: See above
        let components = unsafe {
            std::ptr::slice_from_raw_parts_mut(
                blob.as_mut_ptr().add(component_offset).cast::<u24>(),
                INTERNED_PATH_SLAB_SIZE,
            )
        };
        // SAFETY: See above
        let lookup = unsafe {
            std::ptr::slice_from_raw_parts_mut(
                blob.as_mut_ptr().add(lookup_offset).cast::<HashLookupKey>(),
                HASH_BUCKET_COUNT * HASH_BUCKET_SIZE,
            )
        };
        // SAFETY: See above
        let bucket_lengths = unsafe {
            std::ptr::slice_from_raw_parts_mut(
                blob.as_mut_ptr().add(bucket_length_offset).cast::<u32>(),
                HASH_BUCKET_COUNT,
            )
        };

        // Forget the blob here because we will drop it later
        std::mem::forget(blob);

        Self {
            total_blob_size: blob_size,
            bytes,
            byte_len: 0,
            strings,
            string_len: 0,
            components,
            component_len: 0,
            hashes: lookup,
            bucket_lengths,
            was_finalized: false,
        }
    }

    pub fn new() -> Self {
        let this = Self::init(|size, align| unsafe { allocate_uninit(size, align) });
        unsafe {
            (*this.bucket_lengths).fill(0u32);
        }
        this
    }

    pub fn create_cache(&self) -> InternerCache {
        let mut cache = InternerCache::new();
        if self.was_finalized {
            cache
                .previous_bucket_lengths
                .extend(unsafe { (&*self.bucket_lengths).iter().map(|len| *len as usize) });
        }
        cache
    }

    #[allow(dead_code)]
    pub fn report(&self) -> MemoryUsageReport {
        MemoryUsageReport {
            total_blob_size: self.total_blob_size,
            bytes: MemoryUsageFraction {
                numer: self.byte_len,
                denom: unsafe { (&(*self.bytes)).len() },
            },
            strings: MemoryUsageFraction {
                numer: self.string_len * std::mem::size_of::<SmolRange>(),
                denom: unsafe { (&(*self.strings)).len() } * std::mem::size_of::<SmolRange>(),
            },
            components: MemoryUsageFraction {
                numer: self.component_len * std::mem::size_of::<u24>(),
                denom: unsafe { (&(*self.components)).len() } * std::mem::size_of::<u24>(),
            },
            hashes: MemoryUsageFraction {
                numer: unsafe {
                    (*self.bucket_lengths)
                        .iter()
                        .map(|len| *len as usize)
                        .sum::<usize>()
                        * std::mem::size_of::<HashLookupKey>()
                },
                denom: unsafe { (&(*self.hashes)).len() } * std::mem::size_of::<HashLookupKey>(),
            },
        }
    }

    pub fn from_blob(blob: Box<[u8]>, meta: Box<[u8]>) -> Self {
        let mut this = Self::init(|size, align| {
            assert!(blob.len() == size.get());
            if blob.as_ptr() as usize % align != 0 {
                let mut region = unsafe { allocate_uninit(size, align) };

                region.copy_from_slice(&blob);
                region
            } else {
                blob
            }
        });

        assert!(meta.len() == size_of::<usize>() * 3);
        let slice = bytemuck::cast_slice::<u8, usize>(&meta);
        this.byte_len = usize::from_le(slice[0]);
        this.string_len = usize::from_le(slice[1]);
        this.component_len = usize::from_le(slice[2]);
        this.was_finalized = true;
        this
    }

    fn try_cache_or_finalized_self(
        this: &Self,
        cache: &InternerCache,
        hash: Hash40,
    ) -> Option<u24> {
        cache.cached_paths.get(&hash).copied().or_else(|| {
            if this.was_finalized {
                let bucket_idx = hash.crc32() as usize % HASH_BUCKET_COUNT;
                let len = cache.previous_bucket_lengths[bucket_idx];
                let start_idx = bucket_idx * HASH_BUCKET_SIZE;
                let bucket = unsafe { &(&*this.hashes)[start_idx..start_idx + len] };

                let shifted_hash = (hash.raw() >> 8) as u32;

                match bucket.binary_search_by(|a| a.shifted_hash.cmp(&shifted_hash)) {
                    Ok(idx) => Some(u24::from_u32(start_idx as u32 + idx as u32)),
                    Err(_) => None,
                }
            } else {
                None
            }
        })
    }

    pub fn intern_path(&mut self, cache: &mut InternerCache, path: &Utf8Path) -> InternPathResult {
        let range_start = u24::from_u32(self.component_len as u32);
        let mut len = 0u8;

        let full_hash = Hash40::const_new(path.as_str());

        if let Some(cached) = Self::try_cache_or_finalized_self(self, cache, full_hash) {
            unsafe {
                return InternPathResult {
                    range: (*self.hashes)[cached.to_u32() as usize].range,
                    is_new: false,
                };
            }
        }

        let mut current = path;
        while let Some(parent) = current.parent() {
            current = parent;

            let parent_hash = Hash40::const_new(current.as_str());
            if let Some(cached) = Self::try_cache_or_finalized_self(self, cache, parent_hash) {
                assert_eq!(cached.to_u32() & IS_INTERNED_COMPONENT, 0x0);
                unsafe {
                    (*self.components)[self.component_len] =
                        u24::from_u32(IS_INTERNED_COMPONENT | cached.to_u32());
                    self.component_len += 1;
                    len += 1;
                    break;
                }
            }
        }

        let mut parent_hash = Hash40::const_new(current.as_str());
        for component in path.strip_prefix(current).unwrap().components() {
            len += 1;
            let hash = Hash40::const_new(component.as_str());
            let index = if let Some(index) = cache.component_index.get(&hash) {
                *index
            } else {
                let bytes = component.as_str().as_bytes();
                let new_len = self.byte_len + bytes.len();
                unsafe {
                    (&mut (*self.bytes))[self.byte_len..new_len].copy_from_slice(bytes);
                    (&mut (*self.strings))[self.string_len] =
                        SmolRange::new(bytes.len() as u8, u24::from_u32(self.byte_len as u32));
                }
                let component_index = u24::from_u32(self.string_len as u32);
                cache.component_index.insert(hash, component_index);
                self.string_len += 1;
                self.byte_len = new_len;
                component_index
            };
            unsafe {
                (*self.components)[self.component_len] = index;
            }
            self.component_len += 1;

            if parent_hash != Hash40::const_new("") {
                parent_hash = parent_hash.const_with("/");
            }
            parent_hash = parent_hash.const_with(component.as_str());

            let range = SmolRange::new(len, range_start);

            let bucket_idx = parent_hash.crc32() as usize % HASH_BUCKET_COUNT;
            let bucket_len = unsafe { &mut (*self.bucket_lengths)[bucket_idx] };
            assert!((*bucket_len as usize) < HASH_BUCKET_SIZE);
            let hash_idx = (bucket_idx * HASH_BUCKET_SIZE) + *bucket_len as usize;
            unsafe {
                (*self.hashes)[hash_idx] = HashLookupKey {
                    shifted_hash: (parent_hash.raw() >> 8) as u32,
                    range,
                }
            };
            cache
                .cached_paths
                .insert(parent_hash, u24::from_u32(hash_idx as u32));
            *bucket_len += 1;
        }

        InternPathResult {
            range: SmolRange::new(len, range_start),
            is_new: true,
        }
    }

    pub fn finalize(&mut self, _cache: InternerCache) {
        let fix_indices = unsafe {
            (&*self.components)[..self.component_len]
                .iter()
                .enumerate()
                .filter_map(|(comp_idx, component)| {
                    let idx = component.to_u32();
                    if idx & IS_INTERNED_COMPONENT != 0 {
                        let idx = (idx & !IS_INTERNED_COMPONENT) as usize;
                        let bucket = idx / HASH_BUCKET_SIZE;
                        let hash = Hash40::from_raw(
                            (((*self.hashes)[idx].shifted_hash as u64) << 8) | bucket as u64,
                        );
                        Some((comp_idx as u32, hash))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };

        for bucket_idx in 0..HASH_BUCKET_COUNT {
            let len = unsafe { (*self.bucket_lengths)[bucket_idx] as usize };
            let start_idx = bucket_idx * HASH_BUCKET_SIZE;
            unsafe {
                (&mut (*self.hashes))[start_idx..start_idx + len]
                    .sort_unstable_by(|a, b| a.shifted_hash.cmp(&b.shifted_hash));
            }

            let mut prev: Option<u32> = None;
            for hash in unsafe { (&*self.hashes)[start_idx..start_idx + len].iter() } {
                if let Some(prev) = prev {
                    assert_ne!(hash.shifted_hash, prev);
                }
                prev = Some(hash.shifted_hash);
            }
        }

        let mut relookup: Hash40Map<u24> = Hash40Map::default();

        for (index, hash) in fix_indices {
            let new_index = if let Some(new_index) = relookup.get(&hash) {
                *new_index
            } else {
                let shifted_hash = (hash.raw() >> 8) as u32;
                // SAFETY: Within this function, we index into slices that we have properly set up in the constructor
                let bucket_idx = hash.crc32() as usize % HASH_BUCKET_COUNT;
                let len = unsafe { (*self.bucket_lengths)[bucket_idx] };
                let start_idx = bucket_idx * HASH_BUCKET_SIZE;
                let bucket = unsafe { &(&(*self.hashes))[start_idx..start_idx + len as usize] };
                let local_idx = bucket
                    .binary_search_by_key(&shifted_hash, |a| a.shifted_hash)
                    .unwrap();
                let index = u24::from_u32((start_idx + local_idx) as u32);
                relookup.insert(hash, index);
                index
            };

            unsafe {
                (*self.components)[index as usize] =
                    u24::from_u32(new_index.to_u32() | IS_INTERNED_COMPONENT);
            }
        }

        self.was_finalized = true;
    }

    pub fn dump_blob(&self) -> Vec<u8> {
        let full_blob =
            unsafe { std::slice::from_raw_parts((*self.bytes).as_ptr(), self.total_blob_size) };

        let mut out = Vec::with_capacity(full_blob.len());
        out.extend_from_slice(full_blob);
        out
    }

    pub fn dump_meta(&self) -> Vec<u8> {
        let mut meta = Vec::with_capacity(size_of::<usize>() * 3);
        meta.extend_from_slice(&self.byte_len.to_le_bytes());
        meta.extend_from_slice(&self.string_len.to_le_bytes());
        meta.extend_from_slice(&self.component_len.to_le_bytes());
        meta
    }

    fn buffer_components_for_recursive(&self, index: usize, components: &mut [Hash40]) -> usize {
        let range = unsafe { (*self.hashes)[index].range };
        let start = range.start().to_u32() as usize;
        let mut written = 0;
        for el in 0..range.len() {
            if written >= components.len() {
                return written;
            }
            let comp_idx = start + el as usize;
            let string_idx = unsafe { (*self.components)[comp_idx].to_u32() };
            if string_idx & IS_INTERNED_COMPONENT != 0 {
                written += self.buffer_components_for_recursive((string_idx & !IS_INTERNED_COMPONENT) as usize, &mut components[written..]);
            } else {
                let string = unsafe { (*self.strings)[string_idx as usize] };
                let byte_start = string.start().to_u32() as usize;
                let bytes =
                    unsafe { &(&(*self.bytes))[byte_start..byte_start + string.len() as usize] };
                components[written] = Hash40::const_new_bytes(bytes);
                written += 1;
            }
        }

        written
    }

    /// Writes the components for this hash into the components buffer, if known.
    ///
    /// Returns the number of components written into the buffer. If there is not enough space in
    /// the buffer for all of the components then this will write the number of components
    /// available
    pub fn buffer_components_for(&self, hash: Hash40, components: &mut [Hash40]) -> Option<usize> {
        // SAFETY: Within this function, we index into slices that we have properly set up in the constructor
        let bucket_idx = hash.crc32() as usize % HASH_BUCKET_COUNT;
        let len = unsafe { (*self.bucket_lengths)[bucket_idx] };
        let start_idx = bucket_idx * HASH_BUCKET_SIZE;
        let bucket = unsafe { &(&*self.hashes)[start_idx..start_idx + len as usize] };

        let shifted_hash = (hash.raw() >> 8) as u32;

        match bucket.binary_search_by(|a| a.shifted_hash.cmp(&shifted_hash)) {
            Ok(idx) => Some(self.buffer_components_for_recursive(start_idx + idx, components)),
            Err(_) => None
        }       
    }

    fn buffer_str_components_for_recursive<'a>(&'a self, index: usize, components: &mut [&'a str]) -> usize {
        let range = unsafe { (*self.hashes)[index].range };
        let start = range.start().to_u32() as usize;
        let mut written = 0;
        for el in 0..range.len() {
            if written >= components.len() {
                return written;
            }
            let comp_idx = start + el as usize;
            let string_idx = unsafe { (*self.components)[comp_idx].to_u32() };
            if string_idx & IS_INTERNED_COMPONENT != 0 {
                written += self.buffer_str_components_for_recursive((string_idx & !IS_INTERNED_COMPONENT) as usize, &mut components[written..]);
            } else {
                let string = unsafe { (*self.strings)[string_idx as usize] };
                let byte_start = string.start().to_u32() as usize;
                let bytes =
                    unsafe { &(&(*self.bytes))[byte_start..byte_start + string.len() as usize] };
                components[written] = unsafe { std::str::from_utf8_unchecked(bytes) };
                written += 1;
            }
        }

        written
    }

    pub fn buffer_str_components_for<'a>(&'a self, hash: Hash40, components: &mut [&'a str]) -> Option<usize> {
        // SAFETY: Within this function, we index into slices that we have properly set up in the constructor
        let bucket_idx = hash.crc32() as usize % HASH_BUCKET_COUNT;
        let len = unsafe { (*self.bucket_lengths)[bucket_idx] };
        let start_idx = bucket_idx * HASH_BUCKET_SIZE;
        let bucket = unsafe { &(&*self.hashes)[start_idx..start_idx + len as usize] };

        let shifted_hash = (hash.raw() >> 8) as u32;

        match bucket.binary_search_by(|a| a.shifted_hash.cmp(&shifted_hash)) {
            Ok(idx) => Some(self.buffer_str_components_for_recursive(start_idx + idx, components)),
            Err(_) => None
        }
    }

    pub fn components_for(&self, hash: Hash40) -> Option<ComponentIter<'_>> {
        ComponentIter::new(self, hash)
    }
}

pub struct ComponentIter<'a> {
    slab: &'a HashMemorySlab,
    range: SmolRange,
    current: usize,
    // Surely we can do this without actually making a box? Maybe a SmallVec for a stack of these?
    // I really don't want to do allocations but this is a minor perf thing we can address later
    nested: Option<Box<ComponentIter<'a>>>,
}

impl<'a> ComponentIter<'a> {
    fn new(slab: &'a HashMemorySlab, hash: Hash40) -> Option<Self> {
        // SAFETY: Within this function, we index into slices that we have properly set up in the constructor
        let bucket_idx = hash.crc32() as usize % HASH_BUCKET_COUNT;
        let len = unsafe { (*slab.bucket_lengths)[bucket_idx] };
        let start_idx = bucket_idx * HASH_BUCKET_SIZE;
        let bucket = unsafe { &(&(*slab.hashes))[start_idx..start_idx + len as usize] };

        let shifted_hash = (hash.raw() >> 8) as u32;

        match bucket.binary_search_by(|a| a.shifted_hash.cmp(&shifted_hash)) {
            Ok(idx) => Some(Self {
                slab,
                range: unsafe { (*slab.hashes)[start_idx + idx].range },
                current: 0,
                nested: None,
            }),
            Err(_) => None,
        }
    }
}

impl<'a> Iterator for ComponentIter<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(nested) = self.nested.as_mut() {
            if let Some(next) = nested.next() {
                return Some(next);
            }
            self.nested = None;
        }

        if self.current >= self.range.len() as usize {
            return None;
        }

        let comp_idx = self.range.start().to_u32() as usize + self.current;
        self.current += 1;
        let string_idx = unsafe { (*self.slab.components)[comp_idx].to_u32() };
        if string_idx & IS_INTERNED_COMPONENT != 0 {
            let mut nested = ComponentIter {
                slab: self.slab,
                range: unsafe {
                    (*self.slab.hashes)[(string_idx & !IS_INTERNED_COMPONENT) as usize].range
                },
                current: 0,
                nested: None,
            };

            if let Some(next) = nested.next() {
                self.nested = Some(Box::new(nested));
                Some(next)
            } else {
                unimplemented!("Interned component with no length?");
            }
        } else {
            let string = unsafe { (*self.slab.strings)[string_idx as usize] };
            let byte_start = string.start().to_u32() as usize;
            let bytes =
                unsafe { &(&*self.slab.bytes)[byte_start..byte_start + string.len() as usize] };
            Some(unsafe { std::str::from_utf8_unchecked(bytes) })
        }
    }
}

impl Drop for HashMemorySlab {
    fn drop(&mut self) {
        unsafe {
            drop(Box::from_raw(self.bytes));
        }
    }
}

pub struct DisplayHash<'a> {
    pub slab: &'a HashMemorySlab,
    pub hash: Hash40,
}

impl Debug for DisplayHash<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(self, f)
    }
}

impl Display for DisplayHash<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Don't use component iter here since this can be done recursively where iter requires allocing memory
        write_hash(self.hash, self.slab, f)
    }
}

fn write_hash_by_index(
    index: usize,
    slab: &HashMemorySlab,
    f: &mut std::fmt::Formatter,
) -> std::fmt::Result {
    let range = unsafe { (*slab.hashes)[index].range };
    let start = range.start().to_u32() as usize;
    for el in 0..range.len() {
        let comp_idx = start + el as usize;
        let string_idx = unsafe { (*slab.components)[comp_idx].to_u32() };
        if string_idx & IS_INTERNED_COMPONENT != 0 {
            write_hash_by_index((string_idx & !IS_INTERNED_COMPONENT) as usize, slab, f)?;
        } else {
            let string = unsafe { (*slab.strings)[string_idx as usize] };
            let byte_start = string.start().to_u32() as usize;
            let bytes =
                unsafe { &(&(*slab.bytes))[byte_start..byte_start + string.len() as usize] };
            // SAFETY: We take the bytes from a &str to write into this buffer
            f.write_str(unsafe { std::str::from_utf8_unchecked(bytes) })?;
        }
        if el + 1 != range.len() {
            f.write_char('/')?;
        }
    }

    Ok(())
}

pub fn write_hash(
    hash: Hash40,
    slab: &HashMemorySlab,
    f: &mut std::fmt::Formatter,
) -> std::fmt::Result {
    // SAFETY: Within this function, we index into slices that we have properly set up in the constructor
    let bucket_idx = hash.crc32() as usize % HASH_BUCKET_COUNT;
    let len = unsafe { (*slab.bucket_lengths)[bucket_idx] };
    let start_idx = bucket_idx * HASH_BUCKET_SIZE;
    let bucket = unsafe { &(&*slab.hashes)[start_idx..start_idx + len as usize] };

    let shifted_hash = (hash.raw() >> 8) as u32;

    match bucket.binary_search_by(|a| a.shifted_hash.cmp(&shifted_hash)) {
        Ok(idx) => write_hash_by_index(start_idx + idx, slab, f),
        Err(_) => {
            if f.alternate() {
                f.write_str("<unknown>")
            } else {
                write!(f, "{:#x}", hash.raw())
            }
        }
    }
}
