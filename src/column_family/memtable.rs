use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Per-table sorted key-value buffer.
/// Keys and values are stored as raw bytes. None values are tombstones (deletions).
#[derive(Debug, Default, Clone)]
pub(crate) struct TableMemtable {
    pub(crate) entries: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    pub(crate) size_bytes: usize,
}

impl TableMemtable {
    pub(crate) fn insert(&mut self, key: Vec<u8>, value: Option<Vec<u8>>) {
        let added = key.len() + value.as_ref().map_or(0, Vec::len);
        if let Some(old) = self.entries.insert(key, value) {
            let removed = old.as_ref().map_or(0, Vec::len);
            self.size_bytes = self.size_bytes.wrapping_add(added).wrapping_sub(removed);
        } else {
            self.size_bytes += added;
        }
    }
}

/// All memtable state for a column family.
#[derive(Debug, Default)]
pub(crate) struct Memtable {
    pub(crate) tables: BTreeMap<String, TableMemtable>,
    sequence: AtomicU64,
}

impl Memtable {
    pub(crate) fn new() -> Self {
        Self {
            tables: BTreeMap::new(),
            sequence: AtomicU64::new(0),
        }
    }

    pub(crate) fn next_sequence(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn size_bytes(&self) -> usize {
        self.tables.values().map(|t| t.size_bytes).sum()
    }

    /// Creates an immutable snapshot by cloning each table's BTreeMap into an Arc.
    pub(crate) fn snapshot(&self) -> MemtableSnapshot {
        let tables = self
            .tables
            .iter()
            .map(|(name, table)| (name.clone(), Arc::new(table.entries.clone())))
            .collect();
        MemtableSnapshot { tables }
    }

    /// Swaps this memtable's tables with a fresh empty set, returning the old data.
    pub(crate) fn drain(&mut self) -> BTreeMap<String, TableMemtable> {
        std::mem::take(&mut self.tables)
    }
}

/// Immutable snapshot of memtable state, held by read transactions.
#[derive(Debug, Clone, Default)]
pub(crate) struct MemtableSnapshot {
    pub(crate) tables: BTreeMap<String, Arc<BTreeMap<Vec<u8>, Option<Vec<u8>>>>>,
}

/// Shared handle to a column family's memtable.
pub(crate) type SharedMemtable = Arc<RwLock<Memtable>>;

pub(crate) fn new_shared_memtable() -> SharedMemtable {
    Arc::new(RwLock::new(Memtable::new()))
}
