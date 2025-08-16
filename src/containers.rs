use std::{collections::BTreeMap, fmt::Debug};

use bytemuck::{Pod, Zeroable};
use smash_hash::Hash40;

use crate::{archive::Archive, data::HashWithData};

/// Table that represents a growable region of data
///
/// Tables consist of two parts: a fixed-length array and a dynamic region. The fixed-length array
/// is a reference to the decompressed data straight from the archive. This enables us to have
/// fast, zero-copy access to the data in the array. It also keeps our memory footprint low!
///
/// The dynamic region is used when new entries are added via table manipulation. This table
/// can only be indexed by [`Index`](crate::index::Index), which informs this table if it should
/// pull data from the fixed-length array or the dynamic region.
pub(crate) struct Table<T> {
    fixed: *mut [T],
    dynamic: Vec<T>,
}

impl<T: Pod> Table<T> {
    /// SAFETY:
    /// - Caller must ensure that the data contained within the first
    ///     `count * std::mem::size_of::<T>()` bytes of `slice` are valid
    ///     values for `T`
    /// - Caller must ensure that the returned table does not outlive `slice`
    /// - Caller must ensure that the range of data pointed to in the first
    ///     `count * std::mem::size_of::<T>()` has no other exclusive references
    ///     before or after creation of this table
    pub unsafe fn new(slice: &mut [u8], count: usize) -> Self {
        let slice = &mut slice[..count * std::mem::size_of::<T>()];
        let slice = bytemuck::cast_slice_mut(slice);

        Self {
            fixed: slice,
            dynamic: vec![],
        }
    }

    /// Returns the length of the fixed array, in bytes
    pub fn fixed_byte_len(&self) -> usize {
        // SAFETY: Caller guarantees in constructor that there are no other mutable references
        //      to this data
        unsafe { (&(*self.fixed)).len() * std::mem::size_of::<T>() }
    }
}

impl<T> Table<T> {
    /// Gets a value from the table
    ///
    /// This uses the fixed-size array if the index is internal, otherwise uses the dynamic array
    pub fn get(&self, index: u32) -> Option<&T> {
        // SAFETY: Caller guarantees in constructor that there are no other mutable references
        //      to this data, also they provide a reference so the pointer is non-null
        let fixed_len = self.fixed_len() as u32;
        if index < fixed_len {
            unsafe { Some((&*self.fixed).get_unchecked(index as usize)) }
        } else {
            self.dynamic.get((index - fixed_len) as usize)
        }
    }

    /// Gets a value as a mutable reference from the table
    ///
    /// This uses the fixed-size array if the index is internal, otherwise uses the dynamic array
    pub fn get_mut(&mut self, index: u32) -> Option<&mut T> {
        let fixed_len = self.fixed_len() as u32;
        if index < fixed_len {
            // SAFETY: See above
            unsafe { Some((&mut *self.fixed).get_unchecked_mut(index as usize)) }
        } else {
            self.dynamic.get_mut((index - fixed_len) as usize)
        }
    }

    pub fn fixed(&self) -> &[T] {
        // SAFETY: See above
        unsafe { &(*self.fixed) }
    }

    pub fn dynamic(&self) -> &[T] {
        &self.dynamic
    }

    /// Checks if a table contains the provided index
    pub fn contains(&self, index: u32) -> bool {
        // SAFETY: See above
        (self.dynamic.len() + unsafe { (&*self.fixed).len() }) as u32 > index
    }

    /// Gets the length of the fixed-size array
    pub fn fixed_len(&self) -> usize {
        // SAFETY: See above
        unsafe { (&*self.fixed).len() }
    }

    /// Gets the length of the dynamic array
    pub fn dynamic_len(&self) -> usize {
        self.dynamic.len()
    }

    /// Gets the total length of the table
    pub fn len(&self) -> usize {
        self.fixed_len() + self.dynamic_len()
    }

    /// Checks if the fixed-size array is empty
    pub fn is_fixed_empty(&self) -> bool {
        self.fixed_len() == 0
    }

    /// Checks if the dynamic array is empty
    pub fn is_dynamic_empty(&self) -> bool {
        self.dynamic_len() == 0
    }

    /// Checks if the entire table is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Gets a value from the table without bounds checking
    ///
    /// SAFETY: Caller guarantees that the index provided is contained within this table
    pub unsafe fn get_unchecked(&self, index: u32) -> &T {
        self.get(index).unwrap_unchecked()
    }

    /// Gets a mutable reference to a value from the table without bounds checking
    ///
    /// SAFETY: Caller guarantees that the index provided is contained within this table
    pub unsafe fn get_unchecked_mut(&mut self, index: u32) -> &mut T {
        self.get_mut(index).unwrap_unchecked()
    }

    /// Pushes a new value to the dynamic region of this table, returning an index
    /// that can be used to reference it
    pub fn push(&mut self, value: T) -> u32 {
        let length = self.dynamic.len();
        self.dynamic.push(value);
        (self.fixed_len() + length) as u32
    }

    pub fn iter(&self) -> impl Iterator<Item = (u32, &T)> {
        // SAFETY: See above
        unsafe {
            (*self.fixed)
                .iter()
                .enumerate()
                .map(|(index, data)| (index as u32, data))
                .chain(
                    self.dynamic
                        .iter()
                        .enumerate()
                        .map(|(index, data)| ((index + self.fixed_len()) as u32, data)),
                )
        }
    }
}

/// Represents an immutable reference to a piece of data in a table
///
/// This is the core of how we integrate what is otherwise an insane data structure
/// with Rust's memory safety and borrow checker, while still being ergonomic.
///
/// A table ref is checked to index safely into the table it references upon construction,
/// and then allows access to the underlying data while still being able to navigate around
/// the archive tables.
///
/// Direct access to the archive is not supported via the public API, instead each
/// table data implements their own methods on `TableRef<Self>` to make navigation
/// and mutation more intuitive/easier.
pub struct TableRef<'a, T> {
    archive: &'a Archive,
    table: &'a Table<T>,
    index: u32,
}

impl<T> std::ops::Deref for TableRef<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: We check that this table contains this index before
        //      construction
        unsafe { self.table.get_unchecked(self.index) }
    }
}

impl<'a, T> TableRef<'a, T> {
    /// Attempts to construct a new table reference from the provided table and index.
    ///
    /// Thie method takes a reference to the [`Archive`] as well, so that when all you have is a
    /// [`TableRef`] you can still perform operations and queries on the archive.
    ///
    /// If the index is out of bounds of the table, this method returns [`None`]
    pub(crate) fn new(archive: &'a Archive, table: &'a Table<T>, index: u32) -> Option<Self> {
        table.contains(index).then_some(Self {
            archive,
            table,
            index,
        })
    }

    /// Fetches the archive that created this [`TableRef`]
    pub(crate) fn archive(&self) -> &Archive {
        self.archive
    }

    /// Fetches the index for this [`TableRef`]
    pub(crate) fn index(&self) -> u32 {
        self.index
    }
}

/// Represents a mutable reference to a piece of data in a table
///
/// This is similar to [`TableRef`], however it holds on to a mutable reference to the table instead.
/// When accessing the [`Archive`] from this mutable reference, it is fully safe because the scope of
/// the lifetime shrinks down to the lifetime of this type. For exaxmple:
/// ```norun
/// let archive = open_archive();
/// let mut filepath = archive.get_file_path_mut("fighter/mario/model/body/c00/model.numdlb").unwrap();
/// let archive = filepath.archive_mut(); // This shrinks the lifetime of `archive`
/// let mut filepath2 = archive.get_file_path_mut("fighter/mario/model/body/c00/model.numdlb").unwrap();
/// // The below won't compile because we had two active references at the same time
/// // filepath.set_file_name("model2.numdlb");
/// // println!("{}", filepath2.file_name());
/// // But this will!
/// filepath2.set_file_name("model2.numdlb");
/// println!("{}", filepath.file_name()); // prints filepath
/// ```
///
/// This is completely safe because both the [`TableRef`] and [`TableMut`] reference data based on *index*
/// instead of *pointer*, which allows the contents (and even location) of the referenced data to change between
/// lifetime holders **safely**.
pub struct TableMut<'a, T> {
    archive: *mut Archive,
    table: &'a mut Table<T>,
    index: u32,
}

impl<T> std::ops::Deref for TableMut<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: We check that this table contains this index before
        //      construction
        unsafe { self.table.get_unchecked(self.index) }
    }
}

impl<T> std::ops::DerefMut for TableMut<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: See above
        unsafe { self.table.get_unchecked_mut(self.index) }
    }
}

impl<'a, T> TableMut<'a, T> {
    // We take the whole archive as an exclusive reference to guarantee that the caller
    // doesn't do any funny business with it while we are in ownership
    pub(crate) fn new(
        archive: &'a mut Archive,
        get_table: impl FnOnce(&mut Archive) -> &mut Table<T>,
        index: u32,
    ) -> Option<Self> {
        let archive: *mut Archive = archive;
        let table = get_table(unsafe { &mut *archive });
        table.contains(index).then(|| Self {
            archive,
            table,
            index,
        })
    }

    pub(crate) fn archive(&self) -> &Archive {
        // SAFETY: We took an exclusive reference when constructing this data,
        //      so this pointer is non-null and is aligned
        unsafe { &*self.archive }
    }

    pub(crate) fn archive_mut(&mut self) -> &mut Archive {
        // SAFETY:
        // There are two invariants we must hold to be true:
        //  - The invariant that we need to uphold is that we never invalid the *index*
        //      of this table reference, since when we fetch the data we do so
        //      via `get_unchecked` and `get_unchecked_mut`.
        //      All of our containers (lookups and tables) only have methods to
        //      *grow* and never to remove or to shrink -- cleanup is done at reserialization.
        //  - The invariant that we are not creating double mutable access to the same
        //      location in memory. Because this method returns a shorter
        //      lifetime than 'a, we will not be able to reference this mutable
        //      table reference and another at the same time, therefore
        //      we maintain exclusive aliasing.
        //      This means that something like:
        //      let mut fp = archive.get_file_path_mut(ArcIndex::internal(0)).unwrap();
        //      let mut archive = fp.archive_mut().get_file_path_mut(ArcIndex::internal(0)).unwrap();
        //      fp.set_entity(ArcIndex::internal(0));
        //      archive.set_entity(ArcIndex::external(2));
        //      Would not compile!
        unsafe { &mut *self.archive }
    }

    /// Fetches the index for this [`TableMut`]
    pub(crate) fn index(&self) -> u32 {
        self.index
    }
}

pub struct TableSliceRef<'a, T> {
    archive: &'a Archive,
    table: &'a Table<T>,
    start: u32,
    count: u32,
}

impl<'a, T> TableSliceRef<'a, T> {
    pub(crate) fn new(
        archive: &'a Archive,
        table: &'a Table<T>,
        start: u32,
        count: u32,
    ) -> Option<Self> {
        if !table.contains(start + count - 1) {
            return None;
        }

        Some(Self {
            archive,
            table,
            start,
            count,
        })
    }

    pub fn len(&self) -> u32 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, index: u32) -> Option<TableRef<'_, T>> {
        (index - self.start < self.count).then_some(TableRef {
            archive: self.archive,
            table: self.table,
            index,
        })
    }

    pub fn iter(&self) -> TableSliceIter<'_, T> {
        TableSliceIter {
            archive: self.archive,
            table: self.table,
            range: self.start..self.start + self.count,
        }
    }
}

pub struct TableSliceIter<'a, T> {
    archive: &'a Archive,
    table: &'a Table<T>,
    range: std::ops::Range<u32>,
}

impl<'a, T> Iterator for TableSliceIter<'a, T> {
    type Item = TableRef<'a, T>;

    fn next(&mut self) -> Option<Self::Item> {
        let next = self.range.next()?;
        TableRef::new(self.archive, self.table, next)
    }
}

impl<'a, T> IntoIterator for TableSliceRef<'a, T> {
    type IntoIter = TableSliceIter<'a, T>;
    type Item = TableRef<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        TableSliceIter {
            archive: self.archive,
            table: self.table,
            range: self.start..self.start + self.count,
        }
    }
}

impl<'a, T> IntoIterator for &TableSliceRef<'a, T> {
    type IntoIter = TableSliceIter<'a, T>;
    type Item = TableRef<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        TableSliceIter {
            archive: self.archive,
            table: self.table,
            range: self.start..self.start + self.count,
        }
    }
}

/// A container for looking up table indexes from a [`hash`](Hash40)
///
/// Like the [`Table`], the index lookup contains both a fixed-length array and a dynamic region.
/// In the case of the index lookup, the fixed-length array is still a borrowed slice, however it is
/// sorted so it is bsearchable.
///
/// The dynamic region of the lookup is a [`BTreeMap`] of [`Hash40`] -> [`Index`].
pub struct IndexLookup {
    fixed: *mut [HashWithData],
    dynamic: BTreeMap<Hash40, u32>,
}

impl IndexLookup {
    /// SAFETY:
    /// - Caller must ensure that the data contained within the first
    ///     `count * std::mem::size_of::<HashWithData>()` bytes of `slice` are valid
    ///     values for `T`
    /// - Caller must ensure that the returned table does not outlive `slice`
    /// - Caller must ensure that the range of data pointed to in the first
    ///     `count * std::mem::size_of::<HashWithData>()` has no other exclusive references
    ///     before or after creation of this table
    pub unsafe fn new(slice: &mut [u8], count: usize) -> Self {
        let slice = &mut slice[..count * std::mem::size_of::<HashWithData>()];
        let slice = bytemuck::cast_slice_mut(slice);

        Self {
            fixed: slice,
            dynamic: BTreeMap::new(),
        }
    }

    /// Returns the length of the fixed-size section, in bytes
    pub fn fixed_byte_len(&self) -> usize {
        // SAFETY: Caller guarantees in constructor that there are no other mutable references
        //      to this data. They also provide a reference so the pointer is aligned and non-null
        unsafe { (&*self.fixed).len() * std::mem::size_of::<HashWithData>() }
    }

    /// Checks if the provided hash is contained within this lookup
    pub fn contains_key(&self, hash: Hash40) -> bool {
        // SAFETY: See above
        unsafe {
            (*self.fixed)
                .binary_search_by_key(&hash, |key| key.hash40())
                .is_ok()
                || self.dynamic.contains_key(&hash)
        }
    }

    /// Gets the index that the provided hash points to
    ///
    /// If this hash is not in this lookup, this method returns [`None`]
    pub fn get(&self, hash: Hash40) -> Option<u32> {
        // SAFETY: See above
        unsafe {
            (*self.fixed)
                .binary_search_by_key(&hash, |key| key.hash40())
                .ok()
                .map(|index| (*self.fixed)[index].data())
                .or_else(|| self.dynamic.get(&hash).copied())
        }
    }

    /// Sets the index of the provided hash
    ///
    /// If this hash is not present in the lookup, this method returns `false`.
    ///
    /// The return value of this function is marked `#[must_use]` because if you are using this method,
    /// you are intending to set an index for an **existing** hash. If you want to set or insert, use [`Self::insert`]
    #[must_use = "Operation can fail if the hash is not present"]
    pub fn set(&mut self, hash: Hash40, new_index: u32) -> bool {
        // SAFETY: See above
        if let Ok(pos) = unsafe { (*self.fixed).binary_search_by_key(&hash, |key| key.hash40()) } {
            unsafe {
                (*self.fixed)[pos].set_data(new_index);
            }
            true
        } else if let Some(index) = self.dynamic.get_mut(&hash) {
            *index = new_index;
            true
        } else {
            false
        }
    }

    /// Inserts the index for the associated hash
    ///
    /// This will return whatever the previous index was
    pub fn insert(&mut self, hash: Hash40, index: u32) -> Option<u32> {
        if let Ok(pos) = unsafe { (*self.fixed).binary_search_by_key(&hash, |key| key.hash40()) } {
            unsafe {
                let prev = (*self.fixed)[pos].data();
                (*self.fixed)[pos].set_data(index);
                Some(prev)
            }
        } else {
            self.dynamic.insert(hash, index)
        }
    }

    pub(crate) fn iter(&self) -> IndexLookupIter<'_> {
        // SAFETY: See above
        let mut fixed = unsafe { (*self.fixed).iter() };
        let mut dynamic = self.dynamic.iter();
        IndexLookupIter {
            current_fixed: fixed.next().map(|h| (h.hash40(), h.data())),
            current_dynamic: dynamic.next().map(|(h, i)| (*h, *i)),
            fixed_iter: fixed,
            dynamic_iter: dynamic,
        }
    }
}

pub(crate) struct IndexLookupIter<'a> {
    current_fixed: Option<(Hash40, u32)>,
    current_dynamic: Option<(Hash40, u32)>,
    fixed_iter: std::slice::Iter<'a, HashWithData>,
    dynamic_iter: std::collections::btree_map::Iter<'a, Hash40, u32>,
}

impl<'a> Iterator for IndexLookupIter<'a> {
    type Item = (Hash40, u32);

    fn next(&mut self) -> Option<Self::Item> {
        match (self.current_fixed, self.current_dynamic) {
            (None, None) => None,
            (Some((fixed_hash, fixed_index)), Some((dyn_hash, dyn_index))) => {
                if fixed_hash < dyn_hash {
                    self.current_fixed = self.fixed_iter.next().map(|h| (h.hash40(), h.data()));

                    Some((fixed_hash, fixed_index))
                } else {
                    self.current_fixed = self.dynamic_iter.next().map(|(h, i)| (*h, *i));
                    Some((dyn_hash, dyn_index))
                }
            }
            (Some((hash, index)), None) => {
                self.current_fixed = self.fixed_iter.next().map(|h| (h.hash40(), h.data()));
                Some((hash, index))
            }
            (None, Some((hash, index))) => {
                self.current_dynamic = self.dynamic_iter.next().map(|(h, i)| (*h, *i));
                Some((hash, index))
            }
        }
    }
}

/// Represents a bucket in a [`BucketLookup`]
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Pod, Zeroable)]
pub(crate) struct Bucket {
    start: u32,
    count: u32,
}

/// Container for looking up indexes from hashes, but with manual buckets for faster search times
///
/// This is used for hash lookups that can be **very large** in size. The fixed-length section
/// consists of a list of buckets followed by an array of partially sorted hashes.
///
/// The buckets point to subslices within the hashes, where each range pointed to by a bucket
/// is sorted and can be binary searched.
///
/// The dynamic section is also a bucketed list of [`BTreeMap`] of [`Hash40`] -> [`Index`]
pub struct BucketLookup {
    fixed_hashes: *mut [HashWithData],
    fixed_buckets: *const [Bucket],
    dynamic: Box<[BTreeMap<Hash40, u32>]>,
}

impl BucketLookup {
    /// SAFETY:
    /// - Caller must ensure that the data contained within the first [`Self::fixed_byte_len`]
    ///     bytes of `slice` are valid values for `T`
    /// - Caller must ensure that the returned table does not outlive `slice`
    /// - Caller must ensure that the range of data pointed to in the first
    ///     `count * std::mem::size_of::<HashWithData>()` has no other exclusive references
    ///     before or after creation of this table
    pub unsafe fn new(slice: &mut [u8], hash_count: usize, bucket_count: usize) -> Self {
        let bucket_len = bucket_count * std::mem::size_of::<Bucket>();
        let bucket_slice = &mut slice[..bucket_len];

        let bucket_slice: *mut [Bucket] = bytemuck::cast_slice_mut::<_, Bucket>(bucket_slice);

        let hash_slice =
            &mut slice[bucket_len..bucket_len + hash_count * std::mem::size_of::<HashWithData>()];

        let hash_slice = bytemuck::cast_slice_mut(hash_slice);

        let mut buckets = Vec::with_capacity(bucket_count);
        for _ in 0..bucket_count {
            buckets.push(BTreeMap::new());
        }

        Self {
            fixed_hashes: hash_slice,
            fixed_buckets: bucket_slice,
            dynamic: buckets.into_boxed_slice(),
        }
    }

    /// Returns the size of the fixed-length section, in bytes
    pub fn fixed_byte_len(&self) -> usize {
        // SAFETY: Caller guarantees in constructor that there are no other mutable references
        //      to this data
        unsafe {
            (&*self.fixed_hashes).len() * std::mem::size_of::<HashWithData>()
                + (&*self.fixed_buckets).len() * std::mem::size_of::<Bucket>()
        }
    }

    fn borrow_bucket(&self, hash: Hash40) -> (usize, &[HashWithData]) {
        // SAFETY: See above
        unsafe {
            let length = (&*self.fixed_buckets).len();
            let bucket_index = (hash.raw() as usize) % length;
            let bucket = &(*self.fixed_buckets)[bucket_index];

            (
                bucket_index,
                &(&*self.fixed_hashes)
                    [bucket.start as usize..(bucket.start + bucket.count) as usize],
            )
        }
    }

    fn borrow_bucket_mut(&mut self, hash: Hash40) -> (usize, &mut [HashWithData]) {
        // SAFETY: See above
        unsafe {
            let length = (&*self.fixed_buckets).len();
            let bucket_index = (hash.raw() as usize) % length;
            let bucket = &(*self.fixed_buckets)[bucket_index];

            (
                bucket_index,
                &mut (&mut *self.fixed_hashes)
                    [bucket.start as usize..(bucket.start + bucket.count) as usize],
            )
        }
    }

    /// Calculates the total length of the bucket lookup
    pub fn len(&self) -> usize {
        // SAFETY: See above
        unsafe {
            (&*self.fixed_buckets).len()
                + self
                    .dynamic
                    .iter()
                    .map(|dyn_bucket| dyn_bucket.len())
                    .sum::<usize>()
        }
    }

    /// Returns the number of buckets
    pub fn bucket_count(&self) -> usize {
        self.dynamic.len()
    }

    /// Returns an iterator over the **new** buckets
    pub(crate) fn buckets(&self) -> impl Iterator<Item = Bucket> + '_ {
        // SAFETY: See above
        unsafe {
            let mut prev_end = 0;
            (*self.fixed_buckets)
                .iter()
                .zip(self.dynamic.iter())
                .map(move |(fixed, dynamic)| {
                    let start = prev_end;
                    let count = fixed.count + dynamic.len() as u32;
                    prev_end += count;
                    Bucket { start, count }
                })
        }
    }

    /// Checks if the provided hash is contained within this lookup
    pub fn contains_key(&self, hash: Hash40) -> bool {
        let (bucket_index, hashes) = self.borrow_bucket(hash);

        hashes
            .binary_search_by_key(&hash, |key| key.hash40())
            .is_ok()
            || self.dynamic[bucket_index].contains_key(&hash)
    }

    /// Gets the index that the provided hash points to
    ///
    /// If this hash is not in this lookup, this method returns [`None`]
    pub fn get(&self, hash: Hash40) -> Option<u32> {
        let (bucket_index, hashes) = self.borrow_bucket(hash);

        hashes
            .binary_search_by_key(&hash, |key| key.hash40())
            .ok()
            .map(|index| u32::from(hashes[index].data()))
            .or_else(|| self.dynamic[bucket_index].get(&hash).copied())
    }

    /// Sets the index of the provided hash
    ///
    /// If this hash is not present in the lookup, this method returns `false`.
    ///
    /// The return value of this function is marked `#[must_use]` because if you are using this method,
    /// you are intending to set an index for an **existing** hash. If you want to set or insert, use [`Self::insert`]
    #[must_use = "Operation can fail if the hash is not present"]
    pub fn set(&mut self, hash: Hash40, new_index: u32) -> bool {
        let (bucket_index, hashes) = self.borrow_bucket_mut(hash);

        if let Ok(pos) = hashes.binary_search_by_key(&hash, |key| key.hash40()) {
            hashes[pos].set_data(new_index);
            true
        } else if let Some(index) = self.dynamic[bucket_index].get_mut(&hash) {
            *index = new_index;
            true
        } else {
            false
        }
    }

    /// Inserts the index for the associated hash
    ///
    /// This will return whatever the previous index was
    pub fn insert(&mut self, hash: Hash40, index: u32) -> Option<u32> {
        let (bucket_index, hashes) = self.borrow_bucket_mut(hash);

        if let Ok(pos) = hashes.binary_search_by_key(&hash, |key| key.hash40()) {
            let prev = hashes[pos].data();
            hashes[pos].set_data(index);
            Some(prev)
        } else {
            self.dynamic[bucket_index].insert(hash, index)
        }
    }

    pub(crate) fn iter(&self) -> BucketLookupIter<'_> {
        // SAFETY: See above
        let fixed_hashes = unsafe { &(*self.fixed_hashes) };
        let fixed_buckets = unsafe { &(*self.fixed_buckets) };
        let dynamic_buckets = &self.dynamic;

        BucketLookupIter {
            bucket_count: self.dynamic.len(),
            current_bucket: 0,
            current_fixed: None,
            current_dynamic: None,
            fixed_bucket: fixed_hashes.iter(),
            dynamic_bucket: dynamic_buckets[0].iter(),
            fixed_hashes,
            fixed_buckets,
            dynamic: dynamic_buckets,
        }
    }
}

pub(crate) struct BucketLookupIter<'a> {
    bucket_count: usize,
    current_bucket: usize,

    current_fixed: Option<(Hash40, u32)>,
    current_dynamic: Option<(Hash40, u32)>,

    fixed_bucket: std::slice::Iter<'a, HashWithData>,
    dynamic_bucket: std::collections::btree_map::Iter<'a, Hash40, u32>,

    fixed_hashes: &'a [HashWithData],
    fixed_buckets: &'a [Bucket],
    dynamic: &'a [BTreeMap<Hash40, u32>],
}

impl<'a> Iterator for BucketLookupIter<'a> {
    type Item = (Hash40, u32);

    fn next(&mut self) -> Option<Self::Item> {
        while self.current_fixed.is_none()
            && self.current_dynamic.is_none()
            && self.current_bucket < self.bucket_count
        {
            let fixed_bucket = self.fixed_buckets[self.current_bucket];
            self.fixed_bucket = self.fixed_hashes
                [fixed_bucket.start as usize..(fixed_bucket.start + fixed_bucket.count) as usize]
                .iter();

            self.dynamic_bucket = self.dynamic[self.current_bucket].iter();

            self.current_fixed = self.fixed_bucket.next().map(|h| (h.hash40(), h.data()));
            self.current_dynamic = self.dynamic_bucket.next().map(|(h, i)| (*h, *i));
            self.current_bucket += 1;
        }

        match (self.current_fixed, self.current_dynamic) {
            (None, None) => return None,
            (Some((hash, index)), None) => {
                self.current_fixed = self.fixed_bucket.next().map(|h| (h.hash40(), h.data()));
                Some((hash, index))
            }
            (None, Some((hash, index))) => {
                self.current_dynamic = self.dynamic_bucket.next().map(|(h, i)| (*h, *i));
                Some((hash, index))
            }
            (Some((fixed_hash, fixed_index)), Some((dyn_hash, dyn_index))) => {
                if fixed_hash < dyn_hash {
                    self.current_fixed = self.fixed_bucket.next().map(|h| (h.hash40(), h.data()));
                    Some((fixed_hash, fixed_index))
                } else {
                    self.current_dynamic = self.dynamic_bucket.next().map(|(h, i)| (*h, *i));
                    Some((dyn_hash, dyn_index))
                }
            }
        }
    }
}

impl<T: Debug> Debug for TableRef<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        T::fmt(self, f)
    }
}

impl<T: Debug> Debug for TableMut<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        T::fmt(self, f)
    }
}

impl<T: Debug> Debug for TableSliceRef<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.table.iter().map(|(_, item)| item))
            .finish()
    }
}
