use crate::column_family::memtable::MemtableTableMap;
use crate::db::TransactionGuard;
use crate::sealed::Sealed;
use crate::tree_store::{
    AccessGuardMutInPlace, Btree, BtreeExtractIf, BtreeHeader, BtreeMut, BtreeRangeIter,
    MAX_PAIR_LENGTH, MAX_VALUE_LENGTH, PageAllocator, PageHint, PageNumber, PageResolver,
    PageTrackerPolicy, RawBtree,
};
use crate::types::{Key, MutInPlaceValue, Value};
use crate::{AccessGuard, AccessGuardMut, StorageError, WriteTransaction};
use crate::{Result, TableHandle};
use std::borrow::Borrow;
use std::cmp::Ordering;
use std::collections::{BTreeMap, VecDeque};
use std::fmt::{Debug, Formatter};
use std::marker::PhantomData;
use std::ops::RangeBounds;
use std::sync::{Arc, Mutex};
use std::thread;

// Chunk size for bulk operations - balances memory usage and performance
const BULK_INSERT_CHUNK_SIZE: usize = 10_000;

/// Informational storage stats about a table
#[derive(Debug)]
pub struct TableStats {
    pub(crate) tree_height: u32,
    pub(crate) leaf_pages: u64,
    pub(crate) branch_pages: u64,
    pub(crate) stored_leaf_bytes: u64,
    pub(crate) metadata_bytes: u64,
    pub(crate) fragmented_bytes: u64,
}

impl TableStats {
    /// Maximum traversal distance to reach the deepest (key, value) pair in the table
    pub fn tree_height(&self) -> u32 {
        self.tree_height
    }

    /// Number of leaf pages that store user data
    pub fn leaf_pages(&self) -> u64 {
        self.leaf_pages
    }

    /// Number of branch pages in the btree that store user data
    pub fn branch_pages(&self) -> u64 {
        self.branch_pages
    }

    /// Number of bytes consumed by keys and values that have been inserted.
    /// Does not include indexing overhead
    pub fn stored_bytes(&self) -> u64 {
        self.stored_leaf_bytes
    }

    /// Number of bytes consumed by keys in internal branch pages, plus other metadata
    pub fn metadata_bytes(&self) -> u64 {
        self.metadata_bytes
    }

    /// Number of bytes consumed by fragmentation, both in data pages and internal metadata tables
    pub fn fragmented_bytes(&self) -> u64 {
        self.fragmented_bytes
    }
}

/// A table containing key-value mappings
pub struct Table<'txn, K: Key + 'static, V: Value + 'static> {
    name: String,
    transaction: &'txn WriteTransaction,
    tree: BtreeMut<K, V>,
    // In-memory write buffer: keys map to Some(value_bytes) for inserts or None for tombstones
    overlay: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    // Tracks net change in entry count from buffered operations
    len_delta: i64,
}

impl<K: Key + 'static, V: Value + 'static> TableHandle for Table<'_, K, V> {
    fn name(&self) -> &str {
        &self.name
    }
}

struct RetainPanicGuard<'txn> {
    transaction: &'txn WriteTransaction,
    disarmed: bool,
}

impl<'txn> RetainPanicGuard<'txn> {
    fn new(transaction: &'txn WriteTransaction) -> Self {
        Self {
            transaction,
            disarmed: false,
        }
    }

    fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for RetainPanicGuard<'_> {
    fn drop(&mut self) {
        if !self.disarmed && thread::panicking() {
            self.transaction.poison();
        }
    }
}

impl<'txn, K: Key + 'static, V: Value + 'static> Table<'txn, K, V> {
    pub(crate) fn new(
        name: &str,
        table_root: Option<BtreeHeader>,
        freed_pages: Arc<Mutex<Vec<PageNumber>>>,
        allocated_pages: Arc<Mutex<PageTrackerPolicy>>,
        page_allocator: PageAllocator,
        transaction: &'txn WriteTransaction,
    ) -> Table<'txn, K, V> {
        Table {
            name: name.to_string(),
            transaction,
            tree: BtreeMut::new(
                table_root,
                transaction.transaction_guard(),
                page_allocator,
                freed_pages,
                allocated_pages,
            ),
            overlay: BTreeMap::new(),
            len_delta: 0,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn print_debug(&self, include_values: bool) -> Result {
        self.tree.print_debug(include_values)
    }

    /// Flushes all buffered writes from the in-memory overlay into the B-tree.
    ///
    /// The overlay is drained in sorted key order (`BTreeMap` iteration order),
    /// applying inserts for `Some(value)` entries and removes for `None` tombstones.
    /// After flushing, the overlay is empty and `len_delta` is reset to 0.
    pub fn flush_overlay(&mut self) -> Result {
        let overlay = std::mem::take(&mut self.overlay);
        for (key_bytes, value_opt) in overlay {
            let key = K::from_bytes(&key_bytes);
            match value_opt {
                Some(value_bytes) => {
                    let value = V::from_bytes(&value_bytes);
                    self.tree.insert(&key, &value)?;
                }
                None => {
                    self.tree.remove(&key)?;
                }
            }
        }
        self.len_delta = 0;
        Ok(())
    }

    /// Buffers an insert in the in-memory overlay without mutating the B-tree.
    ///
    /// The write is deferred until `flush_overlay()` is called (or Table is dropped).
    /// Reads via `get()` will see buffered values immediately.
    /// Note: `range()`, `iter()`, `first()`, and `last()` do NOT reflect buffered
    /// writes until `flush_overlay()` is called.
    pub fn insert_buffered<'k, 'v>(
        &mut self,
        key: impl Borrow<K::SelfType<'k>>,
        value: impl Borrow<V::SelfType<'v>>,
    ) -> Result {
        if self.transaction.is_deferred_flush() {
            self.insert(key, value)?;
            return Ok(());
        }

        let key_bytes = K::as_bytes(key.borrow()).as_ref().to_vec();
        let value_bytes = V::as_bytes(value.borrow()).as_ref().to_vec();

        if value_bytes.len() > MAX_VALUE_LENGTH {
            return Err(StorageError::ValueTooLarge(value_bytes.len()));
        }
        if key_bytes.len() > MAX_VALUE_LENGTH {
            return Err(StorageError::ValueTooLarge(key_bytes.len()));
        }
        if value_bytes.len() + key_bytes.len() > MAX_PAIR_LENGTH {
            return Err(StorageError::ValueTooLarge(
                value_bytes.len() + key_bytes.len(),
            ));
        }

        match self.overlay.get(&key_bytes) {
            Some(Some(_)) => {
                // Already in overlay with a value: update, no len change
            }
            Some(None) => {
                // Tombstone in overlay: un-deleting
                self.len_delta += 1;
            }
            None => {
                // Not in overlay: check B-tree
                let btree_key = K::from_bytes(&key_bytes);
                if self.tree.get(&btree_key)?.is_none() {
                    self.len_delta += 1;
                }
            }
        }

        self.overlay.insert(key_bytes, Some(value_bytes));
        Ok(())
    }

    /// Buffers a remove (tombstone) in the in-memory overlay without mutating the B-tree.
    ///
    /// The remove is deferred until `flush_overlay()` is called (or Table is dropped).
    /// Reads via `get()` will see the key as absent immediately.
    /// Note: `range()`, `iter()`, `first()`, and `last()` do NOT reflect buffered
    /// removes until `flush_overlay()` is called.
    pub fn remove_buffered<'k>(&mut self, key: impl Borrow<K::SelfType<'k>>) -> Result {
        if self.transaction.is_deferred_flush() {
            self.remove(key)?;
            return Ok(());
        }

        let key_bytes = K::as_bytes(key.borrow()).as_ref().to_vec();

        match self.overlay.get(&key_bytes) {
            Some(None) => {
                // Already a tombstone: no change
                return Ok(());
            }
            Some(Some(_)) => {
                // Was in overlay with a value: check if key exists in B-tree
                let btree_key = K::from_bytes(&key_bytes);
                if self.tree.get(&btree_key)?.is_some() {
                    // Key existed in B-tree AND was overwritten in overlay;
                    // tombstone removes the B-tree entry, net -1
                    self.len_delta -= 1;
                } else {
                    // Key only existed in overlay (new insert), tombstone cancels it
                    self.len_delta -= 1;
                }
            }
            None => {
                // Not in overlay: check B-tree
                let btree_key = K::from_bytes(&key_bytes);
                if self.tree.get(&btree_key)?.is_some() {
                    self.len_delta -= 1;
                }
            }
        }

        self.overlay.insert(key_bytes, None);
        Ok(())
    }

    /// Returns an accessor, which allows mutation, to the value corresponding to the given key
    pub fn get_mut<'k>(
        &mut self,
        key: impl Borrow<K::SelfType<'k>>,
    ) -> Result<Option<AccessGuardMut<'_, V>>> {
        self.flush_overlay()?;
        self.tree.get_mut(key.borrow())
    }

    /// Removes and returns the first key-value pair in the table
    pub fn pop_first(&mut self) -> Result<Option<(AccessGuard<'_, K>, AccessGuard<'_, V>)>> {
        self.flush_overlay()?;
        self.tree.pop_first()
    }

    /// Removes and returns the last key-value pair in the table
    pub fn pop_last(&mut self) -> Result<Option<(AccessGuard<'_, K>, AccessGuard<'_, V>)>> {
        self.flush_overlay()?;
        self.tree.pop_last()
    }

    /// Applies `predicate` to all key-value pairs. All entries for which
    /// `predicate` evaluates to `true` are returned in an iterator, and those which are read from the iterator are removed
    ///
    /// Note: values not read from the iterator will not be removed
    ///
    /// The predicate must not panic. If it panics, the write transaction is
    /// poisoned and [`crate::WriteTransaction::commit`] will return
    /// [`crate::CommitError::TransactionPoisoned`].
    pub fn extract_if<F: for<'f> FnMut(K::SelfType<'f>, V::SelfType<'f>) -> bool>(
        &mut self,
        predicate: F,
    ) -> Result<ExtractIf<'_, K, V, F>> {
        self.flush_overlay()?;
        self.extract_from_if::<K::SelfType<'_>, F>(.., predicate)
    }

    /// Applies `predicate` to all key-value pairs in the specified range. All entries for which
    /// `predicate` evaluates to `true` are returned in an iterator, and those which are read from the iterator are removed
    ///
    /// Note: values not read from the iterator will not be removed
    ///
    /// The predicate must not panic. If it panics, the write transaction is
    /// poisoned and [`crate::WriteTransaction::commit`] will return
    /// [`crate::CommitError::TransactionPoisoned`].
    pub fn extract_from_if<'a, KR, F: for<'f> FnMut(K::SelfType<'f>, V::SelfType<'f>) -> bool>(
        &mut self,
        range: impl RangeBounds<KR> + 'a,
        predicate: F,
    ) -> Result<ExtractIf<'_, K, V, F>>
    where
        KR: Borrow<K::SelfType<'a>> + 'a,
    {
        self.flush_overlay()?;
        let inner = self.tree.extract_from_if(&range, predicate)?;
        Ok(ExtractIf::new(inner, Some(self.transaction)))
    }

    /// Applies `predicate` to all key-value pairs. All entries for which
    /// `predicate` evaluates to `false` are removed.
    ///
    /// The predicate must not panic. If it panics, the write transaction is
    /// poisoned and [`crate::WriteTransaction::commit`] will return
    /// [`crate::CommitError::TransactionPoisoned`].
    ///
    pub fn retain<F: for<'f> FnMut(K::SelfType<'f>, V::SelfType<'f>) -> bool>(
        &mut self,
        predicate: F,
    ) -> Result {
        self.flush_overlay()?;
        let mut panic_guard = RetainPanicGuard::new(self.transaction);
        let result = self.tree.retain_in::<K::SelfType<'_>, F>(predicate, ..);
        panic_guard.disarm();
        result
    }

    /// Applies `predicate` to all key-value pairs in the range `start..end`. All entries for which
    /// `predicate` evaluates to `false` are removed.
    ///
    /// The predicate must not panic. If it panics, the write transaction is
    /// poisoned and [`crate::WriteTransaction::commit`] will return
    /// [`crate::CommitError::TransactionPoisoned`].
    ///
    pub fn retain_in<'a, KR, F: for<'f> FnMut(K::SelfType<'f>, V::SelfType<'f>) -> bool>(
        &mut self,
        range: impl RangeBounds<KR> + 'a,
        predicate: F,
    ) -> Result
    where
        KR: Borrow<K::SelfType<'a>> + 'a,
    {
        self.flush_overlay()?;
        let mut panic_guard = RetainPanicGuard::new(self.transaction);
        let result = self.tree.retain_in(predicate, range);
        panic_guard.disarm();
        result
    }

    /// Insert mapping of the given key to the given value
    ///
    /// If key is already present it is replaced
    ///
    /// Returns the old value, if the key was present in the table, otherwise None is returned
    pub fn insert<'k, 'v>(
        &mut self,
        key: impl Borrow<K::SelfType<'k>>,
        value: impl Borrow<V::SelfType<'v>>,
    ) -> Result<Option<AccessGuard<'_, V>>> {
        let value_len = V::as_bytes(value.borrow()).as_ref().len();
        if value_len > MAX_VALUE_LENGTH {
            return Err(StorageError::ValueTooLarge(value_len));
        }
        let key_len = K::as_bytes(key.borrow()).as_ref().len();
        if key_len > MAX_VALUE_LENGTH {
            return Err(StorageError::ValueTooLarge(key_len));
        }
        if value_len + key_len > MAX_PAIR_LENGTH {
            return Err(StorageError::ValueTooLarge(value_len + key_len));
        }

        if self.transaction.is_deferred_flush() {
            let key_bytes = K::as_bytes(key.borrow()).as_ref().to_vec();
            let value_bytes = V::as_bytes(value.borrow()).as_ref().to_vec();
            // Push to transaction's deferred ops for WAL + memtable on commit
            self.transaction.push_deferred_op(
                &self.name,
                key_bytes.clone(),
                Some(value_bytes.clone()),
            );
            // Also insert into overlay for read-your-own-writes within this transaction
            self.overlay.insert(key_bytes, Some(value_bytes));
            return Ok(None);
        }

        self.tree.insert(key.borrow(), value.borrow())
    }

    /// Removes the given key
    ///
    /// Returns the old value, if the key was present in the table
    pub fn remove<'a>(
        &mut self,
        key: impl Borrow<K::SelfType<'a>>,
    ) -> Result<Option<AccessGuard<'_, V>>> {
        if self.transaction.is_deferred_flush() {
            let key_bytes = K::as_bytes(key.borrow()).as_ref().to_vec();
            self.transaction
                .push_deferred_op(&self.name, key_bytes.clone(), None);
            self.overlay.insert(key_bytes, None);
            return Ok(None);
        }
        self.tree.remove(key.borrow())
    }

    /// Bulk insert optimized for loading large datasets
    ///
    /// This method provides significant performance improvements over individual
    /// `insert()` calls for bulk data loading scenarios.
    ///
    /// # Arguments
    ///
    /// * `items` - Iterator of (key, value) pairs to insert
    /// * `sorted` - Hint indicating whether items are already sorted by key.
    ///   Set to `true` if you know the data is sorted for optimal performance.
    ///
    /// # Performance
    ///
    /// - **Sorted data**: 2-3x faster than individual inserts (uses optimized bulk construction)
    /// - **Unsorted data**: 1.5-2x faster than individual inserts (uses batched insertion with sorting)
    ///
    /// # Returns
    ///
    /// Returns the total number of items inserted
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Any key or value exceeds maximum size limits
    /// - Storage errors occur during insertion
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use manifold::{Database, TableDefinition, Error};
    /// # const TABLE: TableDefinition<u64, &str> = TableDefinition::new("data");
    /// # fn example() -> Result<(), Error> {
    /// # let db = Database::create("example.db")?;
    /// # let txn = db.begin_write()?;
    /// # let mut table = txn.open_table(TABLE)?;
    /// // Pre-sorted data (best performance)
    /// let sorted_data = vec![(1u64, "one"), (2u64, "two"), (3u64, "three")];
    /// let count = table.insert_bulk(sorted_data.into_iter(), true)?;
    ///
    /// // Unsorted data (still faster than individual inserts)
    /// let unsorted_data = vec![(10u64, "ten"), (5u64, "five"), (7u64, "seven")];
    /// let count = table.insert_bulk(unsorted_data.into_iter(), false)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn insert_bulk<'i, I>(&mut self, items: I, sorted: bool) -> Result<usize>
    where
        I: IntoIterator<Item = (K::SelfType<'i>, V::SelfType<'i>)>,
    {
        if sorted {
            self.insert_bulk_sorted(items)
        } else {
            self.insert_bulk_unsorted(items)
        }
    }

    /// Bulk insert for pre-sorted data (internal implementation)
    ///
    /// This uses an optimized insertion strategy that assumes data arrives
    /// in sorted order, reducing tree rebalancing overhead.
    fn insert_bulk_sorted<'i, I>(&mut self, items: I) -> Result<usize>
    where
        I: IntoIterator<Item = (K::SelfType<'i>, V::SelfType<'i>)>,
    {
        let mut count = 0usize;

        // For sorted data, we can insert directly without buffering
        // The B-tree will naturally build from left to right with minimal rebalancing
        for (key, value) in items {
            self.insert(&key, &value)?;
            count += 1;
        }

        Ok(count)
    }

    /// Bulk insert for unsorted data (internal implementation)
    ///
    /// This collects items into chunks, sorts each chunk, then inserts
    /// in order to improve cache locality and reduce random tree traversals.
    fn insert_bulk_unsorted<'i, I>(&mut self, items: I) -> Result<usize>
    where
        I: IntoIterator<Item = (K::SelfType<'i>, V::SelfType<'i>)>,
    {
        let mut total_count = 0usize;
        let mut chunk = Vec::with_capacity(BULK_INSERT_CHUNK_SIZE);

        for (key, value) in items {
            // Serialize key and value to owned bytes for sorting
            let key_bytes = K::as_bytes(&key).as_ref().to_vec();
            let value_bytes = V::as_bytes(&value).as_ref().to_vec();

            chunk.push((key_bytes, value_bytes));

            // When chunk is full, sort and insert
            if chunk.len() >= BULK_INSERT_CHUNK_SIZE {
                total_count += self.insert_sorted_chunk(&mut chunk)?;
                chunk.clear();
            }
        }

        // Insert remaining items
        if !chunk.is_empty() {
            total_count += self.insert_sorted_chunk(&mut chunk)?;
        }

        Ok(total_count)
    }

    /// Helper to sort and insert a chunk of items
    fn insert_sorted_chunk(&mut self, chunk: &mut [(Vec<u8>, Vec<u8>)]) -> Result<usize> {
        // Sort chunk by key bytes
        chunk.sort_by(|a, b| K::compare(&a.0, &b.0));

        // Insert sorted items
        let count = chunk.len();
        for (key_bytes, value_bytes) in chunk.iter() {
            let key = K::from_bytes(key_bytes);
            let value = V::from_bytes(value_bytes);
            self.insert(&key, &value)?;
        }

        Ok(count)
    }

    /// Bulk remove optimized for deleting multiple keys
    ///
    /// This method provides performance improvements over individual
    /// `remove()` calls when deleting many keys at once.
    ///
    /// # Arguments
    ///
    /// * `keys` - Iterator of keys to remove
    ///
    /// # Returns
    ///
    /// Returns the number of keys that were actually removed (keys that existed in the table)
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use manifold::{Database, TableDefinition, Error};
    /// # const TABLE: TableDefinition<u64, &str> = TableDefinition::new("data");
    /// # fn example() -> Result<(), Error> {
    /// # let db = Database::create("example.db")?;
    /// # let txn = db.begin_write()?;
    /// # let mut table = txn.open_table(TABLE)?;
    /// let keys_to_delete = vec![1u64, 2u64, 3u64, 4u64, 5u64];
    /// let removed_count = table.remove_bulk(keys_to_delete.into_iter())?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn remove_bulk<'i, I>(&mut self, keys: I) -> Result<usize>
    where
        I: IntoIterator<Item = K::SelfType<'i>>,
    {
        let mut total_removed = 0usize;
        let mut chunk = Vec::with_capacity(BULK_INSERT_CHUNK_SIZE);

        for key in keys {
            // Serialize key to owned bytes for sorting
            let key_bytes = K::as_bytes(&key).as_ref().to_vec();
            chunk.push(key_bytes);

            // When chunk is full, sort and remove
            if chunk.len() >= BULK_INSERT_CHUNK_SIZE {
                total_removed += self.remove_sorted_chunk(&mut chunk)?;
                chunk.clear();
            }
        }

        // Remove remaining items
        if !chunk.is_empty() {
            total_removed += self.remove_sorted_chunk(&mut chunk)?;
        }

        Ok(total_removed)
    }

    /// Helper to sort and remove a chunk of keys
    fn remove_sorted_chunk(&mut self, chunk: &mut [Vec<u8>]) -> Result<usize> {
        // Sort chunk by key bytes
        chunk.sort_by(|a, b| K::compare(a, b));

        // Remove sorted keys
        let mut removed = 0usize;
        for key_bytes in chunk.iter() {
            let key = K::from_bytes(key_bytes);
            if self.remove(&key)?.is_some() {
                removed += 1;
            }
        }

        Ok(removed)
    }

    /// Gets the given key's corresponding entry in the table for in-place manipulation.
    ///
    /// This is analogous to [`std::collections::BTreeMap::entry`], and avoids the double
    /// lookup that a `get` followed by `insert` would require when updating a value.
    pub fn entry<'a>(&'a mut self, key: K::SelfType<'a>) -> Result<Entry<'a, K, V>> {
        self.flush_overlay()?;
        let key_len = K::as_bytes(&key).as_ref().len();
        if key_len > MAX_VALUE_LENGTH {
            return Err(StorageError::ValueTooLarge(key_len));
        }
        if self.tree.get(&key)?.is_some() {
            Ok(Entry::Occupied(OccupiedEntry {
                tree: &mut self.tree,
                key,
            }))
        } else {
            Ok(Entry::Vacant(VacantEntry {
                tree: &mut self.tree,
                key,
            }))
        }
    }
}

impl<K: Key + 'static, V: MutInPlaceValue + 'static> Table<'_, K, V> {
    /// Reserve space to insert a key-value pair
    ///
    /// If key is already present it is replaced
    ///
    /// The returned reference will have length equal to `value_length`
    #[deprecated(
        since = "3.1.3",
        note = "The returned guard must be dropped before the transaction commits, otherwise data loss may occur. This is fixed in the 4.0 release."
    )]
    pub fn insert_reserve<'a>(
        &mut self,
        key: impl Borrow<K::SelfType<'a>>,
        value_length: usize,
    ) -> Result<AccessGuardMutInPlace<'_, V>> {
        self.flush_overlay()?;
        if value_length > MAX_VALUE_LENGTH {
            return Err(StorageError::ValueTooLarge(value_length));
        }
        let key_len = K::as_bytes(key.borrow()).as_ref().len();
        if key_len > MAX_VALUE_LENGTH {
            return Err(StorageError::ValueTooLarge(key_len));
        }
        if value_length + key_len > MAX_PAIR_LENGTH {
            return Err(StorageError::ValueTooLarge(value_length + key_len));
        }
        self.tree.insert_reserve(key.borrow(), value_length)
    }
}

impl<K: Key + 'static, V: Value + 'static> ReadableTableMetadata for Table<'_, K, V> {
    fn stats(&self) -> Result<TableStats> {
        let tree_stats = self.tree.stats()?;

        Ok(TableStats {
            tree_height: tree_stats.tree_height,
            leaf_pages: tree_stats.leaf_pages,
            branch_pages: tree_stats.branch_pages,
            stored_leaf_bytes: tree_stats.stored_leaf_bytes,
            metadata_bytes: tree_stats.metadata_bytes,
            fragmented_bytes: tree_stats.fragmented_bytes,
        })
    }

    fn len(&self) -> Result<u64> {
        let base_len = self.tree.len()?;
        #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
        let result = (base_len as i64 + self.len_delta) as u64;
        Ok(result)
    }
}

impl<K: Key + 'static, V: Value + 'static> ReadableTable<K, V> for Table<'_, K, V> {
    fn get<'a>(&self, key: impl Borrow<K::SelfType<'a>>) -> Result<Option<AccessGuard<'_, V>>> {
        let key_bytes = K::as_bytes(key.borrow()).as_ref().to_vec();
        // Check local overlay first (current Table handle's buffered writes).
        if let Some(entry) = self.overlay.get(&key_bytes) {
            return match entry {
                Some(value_bytes) => Ok(Some(AccessGuard::with_owned_value(value_bytes.clone()))),
                None => Ok(None), // tombstone
            };
        }
        // In deferred_flush mode, check pending deferred ops from prior Table handles
        // in this transaction (e.g., a previous open_table/insert/drop cycle).
        if self.transaction.is_deferred_flush() {
            if let Ok(value) = self.transaction.get_deferred_op(&self.name, &key_bytes) {
                return match value {
                    Some(value_bytes) => Ok(Some(AccessGuard::with_owned_value(value_bytes))),
                    None => Ok(None), // tombstone
                };
            }
            // Also check the shared memtable for committed data from prior transactions
            // that hasn't been checkpointed to the B-tree yet.
            if let Some(value) = self.transaction.get_from_memtable(&self.name, &key_bytes) {
                return match value {
                    Some(value_bytes) => Ok(Some(AccessGuard::with_owned_value(value_bytes))),
                    None => Ok(None), // tombstone
                };
            }
        }
        self.tree.get(key.borrow())
    }

    fn range<'a, KR>(&self, range: impl RangeBounds<KR> + 'a) -> Result<Range<'_, K, V>>
    where
        KR: Borrow<K::SelfType<'a>> + 'a,
    {
        if self.transaction.is_deferred_flush() {
            let (lower, upper) = range_to_byte_bounds::<K, KR>(&range);
            let btree_iter = self.tree.range(&range)?;
            // Merge: memtable + deferred ops + overlay (highest priority last)
            let mut merged = self.transaction.merged_memtable_for_table(&self.name);
            for (k, v) in &self.overlay {
                merged.insert(k.clone(), v.clone());
            }
            Ok(Range::new_merged(
                btree_iter,
                merged,
                lower,
                upper,
                self.transaction.transaction_guard(),
            ))
        } else {
            self.tree
                .range(&range)
                .map(|x| Range::new(x, self.transaction.transaction_guard()))
        }
    }

    fn first(&self) -> Result<Option<(AccessGuard<'_, K>, AccessGuard<'_, V>)>> {
        if self.transaction.is_deferred_flush() {
            self.range::<K::SelfType<'_>>(..)?.next().transpose()
        } else {
            self.tree.first()
        }
    }

    fn last(&self) -> Result<Option<(AccessGuard<'_, K>, AccessGuard<'_, V>)>> {
        if self.transaction.is_deferred_flush() {
            self.range::<K::SelfType<'_>>(..)?.next_back().transpose()
        } else {
            self.tree.last()
        }
    }
}

impl<K: Key, V: Value> Sealed for Table<'_, K, V> {}

impl<K: Key + 'static, V: Value + 'static> Drop for Table<'_, K, V> {
    fn drop(&mut self) {
        if !self.transaction.is_deferred_flush()
            && !self.overlay.is_empty()
            && let Err(e) = self.flush_overlay()
        {
            eprintln!("[MANIFOLD] Warning: flush_overlay failed during Table drop: {e}");
        }
        self.transaction.close_table(
            &self.name,
            &self.tree,
            self.tree.get_root().map(|x| x.length).unwrap_or_default(),
        );
    }
}

fn debug_helper<K: Key + 'static, V: Value + 'static>(
    f: &mut Formatter<'_>,
    name: &str,
    len: Result<u64>,
    first: Result<Option<(AccessGuard<K>, AccessGuard<V>)>>,
    last: Result<Option<(AccessGuard<K>, AccessGuard<V>)>>,
) -> std::fmt::Result {
    write!(f, "Table [ name: \"{name}\", ")?;
    if let Ok(len) = len {
        if len == 0 {
            write!(f, "No entries")?;
        } else if len == 1 {
            if let Ok(first) = first {
                let (key, value) = first.as_ref().unwrap();
                write!(f, "One key-value: {:?} = {:?}", key.value(), value.value())?;
            } else {
                write!(f, "I/O Error accessing table!")?;
            }
        } else {
            if let Ok(first) = first {
                let (key, value) = first.as_ref().unwrap();
                write!(f, "first: {:?} = {:?}, ", key.value(), value.value())?;
            } else {
                write!(f, "I/O Error accessing table!")?;
            }
            if len > 2 {
                write!(f, "...{} more entries..., ", len - 2)?;
            }
            if let Ok(last) = last {
                let (key, value) = last.as_ref().unwrap();
                write!(f, "last: {:?} = {:?}", key.value(), value.value())?;
            } else {
                write!(f, "I/O Error accessing table!")?;
            }
        }
    } else {
        write!(f, "I/O Error accessing table!")?;
    }
    write!(f, " ]")?;

    Ok(())
}

impl<K: Key + 'static, V: Value + 'static> Debug for Table<'_, K, V> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        debug_helper(f, &self.name, self.len(), self.first(), self.last())
    }
}

pub trait ReadableTableMetadata {
    /// Retrieves information about storage usage for the table
    fn stats(&self) -> Result<TableStats>;

    /// Returns the number of entries in the table
    fn len(&self) -> Result<u64>;

    /// Returns `true` if the table is empty
    fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}

pub trait ReadableTable<K: Key + 'static, V: Value + 'static>: ReadableTableMetadata {
    /// Returns the value corresponding to the given key
    fn get<'a>(&self, key: impl Borrow<K::SelfType<'a>>) -> Result<Option<AccessGuard<'_, V>>>;

    /// Retrieves multiple values in a single batch operation.
    ///
    /// This method provides better performance than calling `get()` multiple times by:
    /// - Optimizing B-tree traversal order (sorts keys internally)
    /// - Reducing repeated lookups for nearby keys
    /// - Better cache locality
    ///
    /// # Arguments
    ///
    /// * `keys` - Iterator of keys to retrieve
    ///
    /// # Returns
    ///
    /// A vector of Option<AccessGuard> in the same order as the input keys.
    /// Missing keys will be None.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use manifold::{Database, ReadableDatabase, ReadableTable, TableDefinition, Error};
    /// # const TABLE: TableDefinition<u64, &str> = TableDefinition::new("data");
    /// # fn example() -> Result<(), Error> {
    /// # let db = Database::create("example.db")?;
    /// # let txn = db.begin_read()?;
    /// # let table = txn.open_table(TABLE)?;
    /// let keys = vec![1u64, 2u64, 3u64];
    /// let values = table.get_bulk(keys.iter().copied())?;
    /// for (i, value) in values.iter().enumerate() {
    ///     if let Some(guard) = value {
    ///         println!("Key {}: {:?}", keys[i], guard.value());
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    fn get_bulk<'i, I>(&self, keys: I) -> Result<Vec<Option<AccessGuard<'_, V>>>>
    where
        I: IntoIterator<Item = K::SelfType<'i>>,
    {
        // Default implementation: collect keys with indices, sort, retrieve, restore order
        let mut indexed_keys: Vec<(usize, Vec<u8>)> = keys
            .into_iter()
            .enumerate()
            .map(|(idx, key)| (idx, K::as_bytes(&key).as_ref().to_vec()))
            .collect();

        // Sort by key bytes for sequential B-tree access
        indexed_keys.sort_by(|a, b| a.1.cmp(&b.1));

        // Retrieve values in sorted order
        let mut sorted_results: Vec<(usize, Option<AccessGuard<'_, V>>)> =
            Vec::with_capacity(indexed_keys.len());

        for (original_idx, key_bytes) in indexed_keys {
            let key = K::from_bytes(&key_bytes);
            let value = self.get(&key)?;
            sorted_results.push((original_idx, value));
        }

        // Restore original order
        sorted_results.sort_by_key(|(idx, _)| *idx);

        // Extract just the values
        Ok(sorted_results.into_iter().map(|(_, v)| v).collect())
    }

    /// Returns a double-ended iterator over a range of elements in the table
    ///
    /// # Examples
    ///
    /// Usage:
    /// ```rust
    /// use manifold::*;
    /// # use tempfile::NamedTempFile;
    /// const TABLE: TableDefinition<&str, u64> = TableDefinition::new("my_data");
    ///
    /// # fn main() -> Result<(), Error> {
    /// # #[cfg(not(target_os = "wasi"))]
    /// # let tmpfile = NamedTempFile::new().unwrap();
    /// # #[cfg(target_os = "wasi")]
    /// # let tmpfile = NamedTempFile::new_in("/tmp").unwrap();
    /// # let filename = tmpfile.path();
    /// let db = Database::create(filename)?;
    /// let write_txn = db.begin_write()?;
    /// {
    ///     let mut table = write_txn.open_table(TABLE)?;
    ///     table.insert("a", &0)?;
    ///     table.insert("b", &1)?;
    ///     table.insert("c", &2)?;
    /// }
    /// write_txn.commit()?;
    ///
    /// let read_txn = db.begin_read()?;
    /// let table = read_txn.open_table(TABLE)?;
    /// let mut iter = table.range("a".."c")?;
    /// let (key, value) = iter.next().unwrap()?;
    /// assert_eq!("a", key.value());
    /// assert_eq!(0, value.value());
    /// # Ok(())
    /// # }
    /// ```
    fn range<'a, KR>(&self, range: impl RangeBounds<KR> + 'a) -> Result<Range<'_, K, V>>
    where
        KR: Borrow<K::SelfType<'a>> + 'a;

    /// Returns the first key-value pair in the table, if it exists
    fn first(&self) -> Result<Option<(AccessGuard<'_, K>, AccessGuard<'_, V>)>>;

    /// Returns the last key-value pair in the table, if it exists
    fn last(&self) -> Result<Option<(AccessGuard<'_, K>, AccessGuard<'_, V>)>>;

    /// Returns a double-ended iterator over all elements in the table
    fn iter(&self) -> Result<Range<'_, K, V>> {
        self.range::<K::SelfType<'_>>(..)
    }
}

/// A read-only untyped table
pub struct ReadOnlyUntypedTable {
    tree: RawBtree,
}

impl Sealed for ReadOnlyUntypedTable {}

impl ReadableTableMetadata for ReadOnlyUntypedTable {
    /// Retrieves information about storage usage for the table
    fn stats(&self) -> Result<TableStats> {
        let tree_stats = self.tree.stats()?;

        Ok(TableStats {
            tree_height: tree_stats.tree_height,
            leaf_pages: tree_stats.leaf_pages,
            branch_pages: tree_stats.branch_pages,
            stored_leaf_bytes: tree_stats.stored_leaf_bytes,
            metadata_bytes: tree_stats.metadata_bytes,
            fragmented_bytes: tree_stats.fragmented_bytes,
        })
    }

    fn len(&self) -> Result<u64> {
        self.tree.len()
    }
}

impl ReadOnlyUntypedTable {
    pub(crate) fn new(
        root_page: Option<BtreeHeader>,
        hint: PageHint,
        fixed_key_size: Option<usize>,
        fixed_value_size: Option<usize>,
        mem: PageResolver,
    ) -> Self {
        Self {
            tree: RawBtree::new(root_page, fixed_key_size, fixed_value_size, mem, hint),
        }
    }
}

/// A read-only table
pub struct ReadOnlyTable<K: Key + 'static, V: Value + 'static> {
    name: String,
    tree: Btree<K, V>,
    transaction_guard: Arc<TransactionGuard>,
    memtable: Option<MemtableTableMap>,
}

impl<K: Key + 'static, V: Value + 'static> TableHandle for ReadOnlyTable<K, V> {
    fn name(&self) -> &str {
        &self.name
    }
}

impl<K: Key + 'static, V: Value + 'static> ReadOnlyTable<K, V> {
    pub(crate) fn new(
        name: String,
        root_page: Option<BtreeHeader>,
        hint: PageHint,
        guard: Arc<TransactionGuard>,
        mem: PageResolver,
        memtable: Option<MemtableTableMap>,
    ) -> Result<ReadOnlyTable<K, V>> {
        Ok(ReadOnlyTable {
            name,
            tree: Btree::new(root_page, hint, guard.clone(), mem)?,
            transaction_guard: guard,
            memtable,
        })
    }

    /// This method is like [`ReadableTable::get()`], but the [`AccessGuard`] is reference counted
    /// and keeps the transaction alive until it is dropped.
    pub fn get<'a>(
        &self,
        key: impl Borrow<K::SelfType<'a>>,
    ) -> Result<Option<AccessGuard<'static, V>>> {
        // Check memtable snapshot first
        if let Some(mem) = &self.memtable {
            let key_bytes = K::as_bytes(key.borrow());
            if let Some(entry) = mem.get(key_bytes.as_ref()) {
                return match entry {
                    Some(value_bytes) => {
                        Ok(Some(AccessGuard::with_owned_value(value_bytes.clone())))
                    }
                    None => Ok(None), // tombstone
                };
            }
        }
        self.tree.get(key.borrow())
    }

    /// This method is like [`ReadableTable::range()`], but the iterator is reference counted and keeps the transaction
    /// alive until it is dropped.
    pub fn range<'a, KR>(&self, range: impl RangeBounds<KR>) -> Result<Range<'static, K, V>>
    where
        KR: Borrow<K::SelfType<'a>> + 'a,
    {
        if let Some(mem) = &self.memtable {
            let (lower, upper) = range_to_byte_bounds::<K, KR>(&range);
            let btree_iter = self.tree.range(&range)?;
            let mem_entries: BTreeMap<Vec<u8>, Option<Vec<u8>>> =
                mem.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            Ok(Range::new_merged(
                btree_iter,
                mem_entries,
                lower,
                upper,
                self.transaction_guard.clone(),
            ))
        } else {
            self.tree
                .range(&range)
                .map(|x| Range::new(x, self.transaction_guard.clone()))
        }
    }
}

impl<K: Key + 'static, V: Value + 'static> ReadableTableMetadata for ReadOnlyTable<K, V> {
    fn stats(&self) -> Result<TableStats> {
        let tree_stats = self.tree.stats()?;

        Ok(TableStats {
            tree_height: tree_stats.tree_height,
            leaf_pages: tree_stats.leaf_pages,
            branch_pages: tree_stats.branch_pages,
            stored_leaf_bytes: tree_stats.stored_leaf_bytes,
            metadata_bytes: tree_stats.metadata_bytes,
            fragmented_bytes: tree_stats.fragmented_bytes,
        })
    }

    fn len(&self) -> Result<u64> {
        self.tree.len()
    }
}

impl<K: Key + 'static, V: Value + 'static> ReadableTable<K, V> for ReadOnlyTable<K, V> {
    fn get<'a>(&self, key: impl Borrow<K::SelfType<'a>>) -> Result<Option<AccessGuard<'_, V>>> {
        // Check memtable snapshot first
        if let Some(mem) = &self.memtable {
            let key_bytes = K::as_bytes(key.borrow());
            if let Some(entry) = mem.get(key_bytes.as_ref()) {
                return match entry {
                    Some(value_bytes) => {
                        Ok(Some(AccessGuard::with_owned_value(value_bytes.clone())))
                    }
                    None => Ok(None), // tombstone
                };
            }
        }
        self.tree.get(key.borrow())
    }

    fn range<'a, KR>(&self, range: impl RangeBounds<KR> + 'a) -> Result<Range<'_, K, V>>
    where
        KR: Borrow<K::SelfType<'a>> + 'a,
    {
        if let Some(mem) = &self.memtable {
            let (lower, upper) = range_to_byte_bounds::<K, KR>(&range);
            let btree_iter = self.tree.range(&range)?;
            let mem_entries: BTreeMap<Vec<u8>, Option<Vec<u8>>> =
                mem.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            Ok(Range::new_merged(
                btree_iter,
                mem_entries,
                lower,
                upper,
                self.transaction_guard.clone(),
            ))
        } else {
            self.tree
                .range(&range)
                .map(|x| Range::new(x, self.transaction_guard.clone()))
        }
    }

    fn first(&self) -> Result<Option<(AccessGuard<'_, K>, AccessGuard<'_, V>)>> {
        if self.memtable.is_some() {
            self.range::<K::SelfType<'_>>(..)?.next().transpose()
        } else {
            self.tree.first()
        }
    }

    fn last(&self) -> Result<Option<(AccessGuard<'_, K>, AccessGuard<'_, V>)>> {
        if self.memtable.is_some() {
            self.range::<K::SelfType<'_>>(..)?.next_back().transpose()
        } else {
            self.tree.last()
        }
    }
}

impl<K: Key, V: Value> Sealed for ReadOnlyTable<K, V> {}

impl<K: Key + 'static, V: Value + 'static> Debug for ReadOnlyTable<K, V> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        debug_helper(f, &self.name, self.len(), self.first(), self.last())
    }
}

pub struct ExtractIf<
    'a,
    K: Key + 'static,
    V: Value + 'static,
    F: for<'f> FnMut(K::SelfType<'f>, V::SelfType<'f>) -> bool,
> {
    inner: BtreeExtractIf<'a, K, V, F>,
    poison_target: Option<&'a WriteTransaction>,
}

impl<
    'a,
    K: Key + 'static,
    V: Value + 'static,
    F: for<'f> FnMut(K::SelfType<'f>, V::SelfType<'f>) -> bool,
> ExtractIf<'a, K, V, F>
{
    pub(crate) fn new(
        inner: BtreeExtractIf<'a, K, V, F>,
        poison_target: Option<&'a WriteTransaction>,
    ) -> Self {
        Self {
            inner,
            poison_target,
        }
    }

    /// Closes the iterator.
    ///
    /// Entries already returned by the iterator remain removed, and unread
    /// entries are not tested by the predicate or removed. Dropping the iterator
    /// also closes it, but this method returns any error encountered while
    /// finalizing the iterator.
    pub fn close(mut self) -> Result {
        self.inner.close()
    }
}

impl<
    K: Key + 'static,
    V: Value + 'static,
    F: for<'f> FnMut(K::SelfType<'f>, V::SelfType<'f>) -> bool,
> Drop for ExtractIf<'_, K, V, F>
{
    fn drop(&mut self) {
        if self.inner.predicate_panicked()
            && let Some(transaction) = self.poison_target
        {
            transaction.poison();
        }
    }
}

impl<
    'a,
    K: Key + 'static,
    V: Value + 'static,
    F: for<'f> FnMut(K::SelfType<'f>, V::SelfType<'f>) -> bool,
> Iterator for ExtractIf<'a, K, V, F>
{
    type Item = Result<(AccessGuard<'a, K>, AccessGuard<'a, V>)>;

    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.inner.next()?;
        Some(entry.map(|entry| {
            let (page, key_range, value_range) = entry.into_raw();
            let key = AccessGuard::with_page(page.clone(), key_range);
            let value = AccessGuard::with_page(page, value_range);
            (key, value)
        }))
    }
}

impl<
    K: Key + 'static,
    V: Value + 'static,
    F: for<'f> FnMut(K::SelfType<'f>, V::SelfType<'f>) -> bool,
> DoubleEndedIterator for ExtractIf<'_, K, V, F>
{
    fn next_back(&mut self) -> Option<Self::Item> {
        let entry = self.inner.next_back()?;
        Some(entry.map(|entry| {
            let (page, key_range, value_range) = entry.into_raw();
            let key = AccessGuard::with_page(page.clone(), key_range);
            let value = AccessGuard::with_page(page, value_range);
            (key, value)
        }))
    }
}

#[derive(Clone)]
pub struct Range<'a, K: Key + 'static, V: Value + 'static> {
    inner: RangeInner<K, V>,
    _transaction_guard: Arc<TransactionGuard>,
    // This lifetime is here so that `&` can be held on `Table` preventing concurrent mutation
    _lifetime: PhantomData<&'a ()>,
}

#[derive(Clone)]
enum RangeInner<K: Key + 'static, V: Value + 'static> {
    BtreeOnly(BtreeRangeIter<K, V>),
    Merged {
        btree: BtreeRangeIter<K, V>,
        /// Memtable entries in key order. `None` value = tombstone.
        mem: VecDeque<(Vec<u8>, Option<Vec<u8>>)>,
        /// Peeked btree entry converted to owned bytes: (key, value).
        btree_peek: Option<(Vec<u8>, Vec<u8>)>,
    },
}

impl<K: Key + 'static, V: Value + 'static> Range<'_, K, V> {
    pub(super) fn new(inner: BtreeRangeIter<K, V>, guard: Arc<TransactionGuard>) -> Self {
        Self {
            inner: RangeInner::BtreeOnly(inner),
            _transaction_guard: guard,
            _lifetime: PhantomData,
        }
    }

    /// Create a range that merges B-tree results with in-memory entries.
    /// Entries are re-sorted using `K::compare` to match B-tree key ordering
    /// and filtered to `[lower, upper)` bounds.
    /// Entries with `None` values are tombstones that suppress B-tree entries.
    pub(super) fn new_merged(
        btree: BtreeRangeIter<K, V>,
        mem_entries: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
        lower: std::ops::Bound<Vec<u8>>,
        upper: std::ops::Bound<Vec<u8>>,
        guard: Arc<TransactionGuard>,
    ) -> Self {
        if mem_entries.is_empty() {
            return Self::new(btree, guard);
        }
        // Re-sort by K::compare since BTreeMap's byte ordering may differ
        // from the key type's logical ordering (e.g., u64 uses little-endian bytes).
        let mut sorted: Vec<(Vec<u8>, Option<Vec<u8>>)> = mem_entries
            .into_iter()
            .filter(|(k, _)| {
                let above_lower = match &lower {
                    std::ops::Bound::Unbounded => true,
                    std::ops::Bound::Included(lo) => K::compare(k, lo) != Ordering::Less,
                    std::ops::Bound::Excluded(lo) => K::compare(k, lo) == Ordering::Greater,
                };
                let below_upper = match &upper {
                    std::ops::Bound::Unbounded => true,
                    std::ops::Bound::Included(hi) => K::compare(k, hi) != Ordering::Greater,
                    std::ops::Bound::Excluded(hi) => K::compare(k, hi) == Ordering::Less,
                };
                above_lower && below_upper
            })
            .collect();
        sorted.sort_by(|(a, _), (b, _)| K::compare(a, b));
        if sorted.is_empty() {
            return Self::new(btree, guard);
        }
        Self {
            inner: RangeInner::Merged {
                btree,
                mem: sorted.into_iter().collect(),
                btree_peek: None,
            },
            _transaction_guard: guard,
            _lifetime: PhantomData,
        }
    }
}

/// Convert typed range bounds to byte bounds for memtable filtering.
fn range_to_byte_bounds<'a, K: Key + 'static, KR: Borrow<K::SelfType<'a>> + 'a>(
    range: &impl RangeBounds<KR>,
) -> (std::ops::Bound<Vec<u8>>, std::ops::Bound<Vec<u8>>) {
    let lower = match range.start_bound() {
        std::ops::Bound::Unbounded => std::ops::Bound::Unbounded,
        std::ops::Bound::Included(k) => {
            std::ops::Bound::Included(K::as_bytes(k.borrow()).as_ref().to_vec())
        }
        std::ops::Bound::Excluded(k) => {
            std::ops::Bound::Excluded(K::as_bytes(k.borrow()).as_ref().to_vec())
        }
    };
    let upper = match range.end_bound() {
        std::ops::Bound::Unbounded => std::ops::Bound::Unbounded,
        std::ops::Bound::Included(k) => {
            std::ops::Bound::Included(K::as_bytes(k.borrow()).as_ref().to_vec())
        }
        std::ops::Bound::Excluded(k) => {
            std::ops::Bound::Excluded(K::as_bytes(k.borrow()).as_ref().to_vec())
        }
    };
    (lower, upper)
}

/// Advance the btree iterator and convert the next entry to owned bytes.
fn btree_next_owned<K: Key, V: Value>(
    btree: &mut BtreeRangeIter<K, V>,
) -> Option<Result<(Vec<u8>, Vec<u8>)>> {
    btree.next().map(|r| {
        r.map(|entry| {
            let kd = entry.key_data();
            let vd = entry.value_data();
            (kd, vd)
        })
    })
}

impl<'a, K: Key + 'static, V: Value + 'static> Iterator for Range<'a, K, V> {
    type Item = Result<(AccessGuard<'a, K>, AccessGuard<'a, V>)>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            RangeInner::BtreeOnly(btree) => btree.next().map(|x| {
                x.map(|entry| {
                    let (page, key_range, value_range) = entry.into_raw();
                    let key = AccessGuard::with_page(page.clone(), key_range);
                    let value = AccessGuard::with_page(page, value_range);
                    (key, value)
                })
            }),
            RangeInner::Merged {
                btree,
                mem,
                btree_peek,
            } => {
                loop {
                    // Fill btree peek buffer if empty
                    if btree_peek.is_none()
                        && let Some(result) = btree_next_owned(btree)
                    {
                        match result {
                            Ok(entry) => *btree_peek = Some(entry),
                            Err(e) => return Some(Err(e)),
                        }
                    }

                    match (btree_peek.as_ref(), mem.front()) {
                        (None, None) => return None,
                        (Some(_), None) => {
                            let (k, v) = btree_peek.take().unwrap();
                            return Some(Ok((
                                AccessGuard::with_owned_value(k),
                                AccessGuard::with_owned_value(v),
                            )));
                        }
                        (None, Some((_, None))) => {
                            // Tombstone with no btree entry to suppress — skip
                            mem.pop_front();
                        }
                        (None, Some(_)) => {
                            let (k, v) = mem.pop_front().unwrap();
                            return Some(Ok((
                                AccessGuard::with_owned_value(k),
                                AccessGuard::with_owned_value(v.unwrap()),
                            )));
                        }
                        (Some((bk, _)), Some((mk, _))) => {
                            match K::compare(bk.as_slice(), mk.as_slice()) {
                                Ordering::Less => {
                                    let (k, v) = btree_peek.take().unwrap();
                                    return Some(Ok((
                                        AccessGuard::with_owned_value(k),
                                        AccessGuard::with_owned_value(v),
                                    )));
                                }
                                Ordering::Greater => {
                                    let (k, v) = mem.pop_front().unwrap();
                                    match v {
                                        None => {} // tombstone, no btree match
                                        Some(vb) => {
                                            return Some(Ok((
                                                AccessGuard::with_owned_value(k),
                                                AccessGuard::with_owned_value(vb),
                                            )));
                                        }
                                    }
                                }
                                Ordering::Equal => {
                                    // Same key — memtable wins (newer data)
                                    btree_peek.take();
                                    let (k, v) = mem.pop_front().unwrap();
                                    match v {
                                        None => {} // tombstone suppresses btree entry
                                        Some(vb) => {
                                            return Some(Ok((
                                                AccessGuard::with_owned_value(k),
                                                AccessGuard::with_owned_value(vb),
                                            )));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

impl<K: Key + 'static, V: Value + 'static> DoubleEndedIterator for Range<'_, K, V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            RangeInner::BtreeOnly(btree) => btree.next_back().map(|x| {
                x.map(|entry| {
                    let (page, key_range, value_range) = entry.into_raw();
                    let key = AccessGuard::with_page(page.clone(), key_range);
                    let value = AccessGuard::with_page(page, value_range);
                    (key, value)
                })
            }),
            RangeInner::Merged {
                btree,
                mem,
                btree_peek,
            } => {
                // Get the btree's back entry (if any), converting to owned bytes.
                // Note: btree_peek is for forward iteration; back iteration fetches fresh.
                let mut btree_back: Option<(Vec<u8>, Vec<u8>)> = None;
                if let Some(result) = btree.next_back() {
                    match result {
                        Ok(entry) => btree_back = Some((entry.key_data(), entry.value_data())),
                        Err(e) => return Some(Err(e)),
                    }
                }

                // Also check if the forward-peeked entry is actually the largest remaining.
                // If btree_peek is set and larger than btree_back, it should be considered.
                // For simplicity, if btree_peek exists and is larger than the btree_back,
                // swap them (put the larger one as btree_back, push the smaller one back
                // by storing in btree_peek). If btree_back is None, take btree_peek.
                if let Some(peeked) = btree_peek.take() {
                    match &btree_back {
                        None => btree_back = Some(peeked),
                        Some((bk, _)) => {
                            if peeked.0.as_slice() > bk.as_slice() {
                                // peeked is larger — use it as the back, put btree_back into peek
                                let old_back = btree_back.replace(peeked).unwrap();
                                *btree_peek = Some(old_back);
                            } else {
                                // btree_back is larger — keep it, restore peek
                                *btree_peek = Some(peeked);
                            }
                        }
                    }
                }

                loop {
                    match (&btree_back, mem.back()) {
                        (None, None) => return None,
                        (Some(_), None) => {
                            let (k, v) = btree_back.unwrap();
                            return Some(Ok((
                                AccessGuard::with_owned_value(k),
                                AccessGuard::with_owned_value(v),
                            )));
                        }
                        (None, Some((_, None))) => {
                            mem.pop_back();
                        }
                        (None, Some(_)) => {
                            let (k, v) = mem.pop_back().unwrap();
                            return Some(Ok((
                                AccessGuard::with_owned_value(k),
                                AccessGuard::with_owned_value(v.unwrap()),
                            )));
                        }
                        (Some((bk, _)), Some((mk, _))) => {
                            match K::compare(bk.as_slice(), mk.as_slice()) {
                                Ordering::Greater => {
                                    let (k, v) = btree_back.unwrap();
                                    return Some(Ok((
                                        AccessGuard::with_owned_value(k),
                                        AccessGuard::with_owned_value(v),
                                    )));
                                }
                                Ordering::Less => {
                                    let (k, v) = mem.pop_back().unwrap();
                                    match v {
                                        None => {}
                                        Some(vb) => {
                                            return Some(Ok((
                                                AccessGuard::with_owned_value(k),
                                                AccessGuard::with_owned_value(vb),
                                            )));
                                        }
                                    }
                                }
                                Ordering::Equal => {
                                    // Same key — memtable wins
                                    // btree_back already consumed from iterator
                                    let (k, v) = mem.pop_back().unwrap();
                                    match v {
                                        None => {}
                                        Some(vb) => {
                                            return Some(Ok((
                                                AccessGuard::with_owned_value(k),
                                                AccessGuard::with_owned_value(vb),
                                            )));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A view into a single entry in a [`Table`], which may either be vacant or occupied.
///
/// This `enum` is constructed from the [`entry`] method on [`Table`], and mirrors
/// [`std::collections::btree_map::Entry`] as closely as the redb data model allows.
///
/// Unlike the in-memory `BTreeMap`, redb values are stored serialized, so methods that
/// produce a "reference to the value" return an [`AccessGuardMut`] instead of `&mut V`.
///
/// [`entry`]: Table::entry
pub enum Entry<'a, K: Key + 'static, V: Value + 'static> {
    /// An occupied entry.
    Occupied(OccupiedEntry<'a, K, V>),
    /// A vacant entry.
    Vacant(VacantEntry<'a, K, V>),
}

impl<'a, K: Key + 'static, V: Value + 'static> Entry<'a, K, V> {
    /// Returns a view of this entry's key.
    pub fn key(&self) -> &K::SelfType<'a> {
        match self {
            Entry::Occupied(entry) => entry.key(),
            Entry::Vacant(entry) => entry.key(),
        }
    }

    /// Ensures a value is in the entry by inserting the provided `default` if empty,
    /// and returns a mutable accessor to the value in the entry.
    pub fn or_insert<'v>(
        self,
        default: impl Borrow<V::SelfType<'v>>,
    ) -> Result<AccessGuardMut<'a, V>> {
        match self {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(default),
        }
    }

    /// Ensures a value is in the entry by inserting the result of `default` if empty,
    /// and returns a mutable accessor to the value in the entry.
    ///
    /// Unlike [`or_insert`](Self::or_insert), the default value is only computed if the
    /// entry is vacant.
    pub fn or_insert_with<'v, F, B>(self, default: F) -> Result<AccessGuardMut<'a, V>>
    where
        F: FnOnce() -> B,
        B: Borrow<V::SelfType<'v>>,
    {
        match self {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(default()),
        }
    }

    /// Ensures a value is in the entry by inserting, if empty, the result of the `default`
    /// function, which is given a view of the key.
    pub fn or_insert_with_key<'v, F, B>(self, default: F) -> Result<AccessGuardMut<'a, V>>
    where
        F: FnOnce(&K::SelfType<'a>) -> B,
        B: Borrow<V::SelfType<'v>>,
    {
        match self {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let value = default(&entry.key);
                entry.insert(value)
            }
        }
    }

    /// Provides in-place mutable access to an occupied entry before any potential inserts
    /// into the table.
    ///
    /// The closure receives an [`AccessGuardMut`] and may replace the stored value via
    /// [`AccessGuardMut::insert`]. Any errors returned by the closure are propagated.
    pub fn and_modify<F>(self, f: F) -> Result<Self>
    where
        F: FnOnce(&mut AccessGuardMut<'_, V>) -> Result<()>,
    {
        match self {
            Entry::Occupied(mut entry) => {
                {
                    let mut guard = entry.get_mut()?;
                    f(&mut guard)?;
                }
                Ok(Entry::Occupied(entry))
            }
            Entry::Vacant(entry) => Ok(Entry::Vacant(entry)),
        }
    }
}

/// A view into an occupied entry in a [`Table`]. It is part of the [`Entry`] enum.
pub struct OccupiedEntry<'a, K: Key + 'static, V: Value + 'static> {
    tree: &'a mut BtreeMut<K, V>,
    key: K::SelfType<'a>,
}

impl<'a, K: Key + 'static, V: Value + 'static> OccupiedEntry<'a, K, V> {
    /// Returns a view of this entry's key.
    pub fn key(&self) -> &K::SelfType<'a> {
        &self.key
    }

    /// Returns a view of this entry's value.
    pub fn get(&self) -> Result<AccessGuard<'_, V>> {
        self.tree.get(&self.key)?.ok_or_else(|| {
            StorageError::Corrupted(
                "entry for key disappeared while OccupiedEntry was live".to_string(),
            )
        })
    }

    /// Returns a mutable accessor to the value in the entry.
    pub fn get_mut(&mut self) -> Result<AccessGuardMut<'_, V>> {
        self.tree.get_mut(&self.key)?.ok_or_else(|| {
            StorageError::Corrupted(
                "entry for key disappeared while OccupiedEntry was live".to_string(),
            )
        })
    }

    /// Converts the entry into a mutable accessor to the value in the entry with a lifetime
    /// bound to the table itself.
    pub fn into_mut(self) -> Result<AccessGuardMut<'a, V>> {
        self.tree.get_mut(&self.key)?.ok_or_else(|| {
            StorageError::Corrupted(
                "entry for key disappeared while OccupiedEntry was live".to_string(),
            )
        })
    }

    /// Replaces the value of the entry with the supplied value, and returns the old value.
    pub fn insert<'v>(
        &mut self,
        value: impl Borrow<V::SelfType<'v>>,
    ) -> Result<AccessGuard<'_, V>> {
        let value_len = V::as_bytes(value.borrow()).as_ref().len();
        if value_len > MAX_VALUE_LENGTH {
            return Err(StorageError::ValueTooLarge(value_len));
        }
        let key_len = K::as_bytes(&self.key).as_ref().len();
        if value_len + key_len > MAX_PAIR_LENGTH {
            return Err(StorageError::ValueTooLarge(value_len + key_len));
        }
        self.tree.insert(&self.key, value.borrow())?.ok_or_else(|| {
            StorageError::Corrupted(
                "entry for key disappeared while OccupiedEntry was live".to_string(),
            )
        })
    }

    /// Takes the value out of the entry, and returns it.
    pub fn remove(self) -> Result<AccessGuard<'a, V>> {
        self.tree.remove(&self.key)?.ok_or_else(|| {
            StorageError::Corrupted(
                "entry for key disappeared while OccupiedEntry was live".to_string(),
            )
        })
    }

    /// Takes the entry out of the table, returning the key and the value.
    pub fn remove_entry(self) -> Result<(K::SelfType<'a>, AccessGuard<'a, V>)> {
        let OccupiedEntry { tree, key } = self;
        let value = tree.remove(&key)?.ok_or_else(|| {
            StorageError::Corrupted(
                "entry for key disappeared while OccupiedEntry was live".to_string(),
            )
        })?;
        Ok((key, value))
    }
}

/// A view into a vacant entry in a [`Table`]. It is part of the [`Entry`] enum.
pub struct VacantEntry<'a, K: Key + 'static, V: Value + 'static> {
    tree: &'a mut BtreeMut<K, V>,
    key: K::SelfType<'a>,
}

impl<'a, K: Key + 'static, V: Value + 'static> VacantEntry<'a, K, V> {
    /// Returns a view of this entry's key.
    pub fn key(&self) -> &K::SelfType<'a> {
        &self.key
    }

    /// Consumes the entry and returns the key that was used to construct it.
    pub fn into_key(self) -> K::SelfType<'a> {
        self.key
    }

    /// Inserts `value` with the entry's key and returns a mutable accessor to it.
    pub fn insert<'v>(self, value: impl Borrow<V::SelfType<'v>>) -> Result<AccessGuardMut<'a, V>> {
        let value_len = V::as_bytes(value.borrow()).as_ref().len();
        if value_len > MAX_VALUE_LENGTH {
            return Err(StorageError::ValueTooLarge(value_len));
        }
        let key_len = K::as_bytes(&self.key).as_ref().len();
        if value_len + key_len > MAX_PAIR_LENGTH {
            return Err(StorageError::ValueTooLarge(value_len + key_len));
        }
        self.tree.insert(&self.key, value.borrow())?;
        self.tree.get_mut(&self.key)?.ok_or_else(|| {
            StorageError::Corrupted(
                "inserted entry not found after VacantEntry::insert".to_string(),
            )
        })
    }
}
