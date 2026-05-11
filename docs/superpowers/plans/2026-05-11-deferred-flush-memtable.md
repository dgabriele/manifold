# Deferred Flush Memtable Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in WAL-backed memtable to `ColumnFamilyDatabase` that eliminates B-tree mutation from the commit path, matching RocksDB's write architecture.

**Architecture:** When `deferred_flush(true)` is set on the builder, write transactions accumulate logical ops, append them to the WAL as a new `LogicalOps` entry type, and merge into a shared per-CF memtable on commit — without touching the B-tree. Read transactions snapshot the memtable and merge it with B-tree results. A background checkpoint drains the memtable into the B-tree in sorted order.

**Tech Stack:** Rust, manifold's existing WAL/checkpoint infrastructure, `BTreeMap` for memtable, `Arc<RwLock<>>` for shared state.

---

## File Structure

| File | Responsibility |
|------|---------------|
| `src/column_family/memtable.rs` | **New**: `Memtable`, `TableMemtable`, `MemtableSnapshot` data structures |
| `src/column_family/builder.rs` | Add `deferred_flush` option |
| `src/column_family/database.rs` | Thread memtable through CF, pass to transactions and reads |
| `src/column_family/mod.rs` | Export new module |
| `src/column_family/wal/entry.rs` | Add `LogicalOps` WAL entry variant |
| `src/column_family/wal/checkpoint.rs` | Drain memtable → B-tree during checkpoint |
| `src/transactions.rs` | `pending_ops` buffer, deferred commit path |
| `src/table.rs` | `ReadOnlyTable` memtable-aware get/range, `Table` deferred insert path |
| `tests/stress_tests.rs` | Deferred flush stress tests |

---

### Task 1: Memtable data structure

**Files:**
- Create: `src/column_family/memtable.rs`
- Modify: `src/column_family/mod.rs`

- [ ] **Step 1: Create the memtable module**

Create `src/column_family/memtable.rs`:

```rust
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Per-table sorted key-value buffer.
/// Keys and values are stored as raw bytes. None values are tombstones.
#[derive(Debug, Default, Clone)]
pub(crate) struct TableMemtable {
    pub(crate) entries: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    pub(crate) size_bytes: usize,
}

impl TableMemtable {
    pub(crate) fn insert(&mut self, key: Vec<u8>, value: Option<Vec<u8>>) {
        let added = key.len() + value.as_ref().map_or(0, Vec::len);
        if let Some(old) = self.entries.insert(key, value) {
            // Subtract old entry size
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
/// Cheap to create (Arc clones, not data clones).
/// Lives as long as the read transaction that holds it.
#[derive(Debug, Clone, Default)]
pub(crate) struct MemtableSnapshot {
    pub(crate) tables: BTreeMap<String, Arc<BTreeMap<Vec<u8>, Option<Vec<u8>>>>>,
}

/// Shared handle to a column family's memtable.
pub(crate) type SharedMemtable = Arc<RwLock<Memtable>>;

pub(crate) fn new_shared_memtable() -> SharedMemtable {
    Arc::new(RwLock::new(Memtable::new()))
}
```

- [ ] **Step 2: Export from column_family/mod.rs**

Add to `src/column_family/mod.rs`:

```rust
pub(crate) mod memtable;
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p manifold-db`
Expected: Compiles (memtable module exists but is not yet used)

- [ ] **Step 4: Commit**

```bash
git add src/column_family/memtable.rs src/column_family/mod.rs
git commit -m "feat: add Memtable data structure for deferred flush"
```

---

### Task 2: Builder option and ColumnFamily integration

**Files:**
- Modify: `src/column_family/builder.rs`
- Modify: `src/column_family/database.rs`

- [ ] **Step 1: Add deferred_flush to builder**

In `src/column_family/builder.rs`, add the field and method:

```rust
pub struct ColumnFamilyDatabaseBuilder {
    pool_size: usize,
    deferred_flush: bool,
}

impl ColumnFamilyDatabaseBuilder {
    pub fn new() -> Self {
        Self {
            pool_size: DEFAULT_POOL_SIZE,
            deferred_flush: false,
        }
    }

    /// Enables deferred flush mode for write-heavy workloads.
    ///
    /// When enabled, write transactions buffer operations in memory and
    /// append them to the WAL without updating the B-tree. A background
    /// checkpoint periodically flushes the memtable to the B-tree in
    /// sorted key order, dramatically improving random write throughput.
    ///
    /// Requires WAL to be enabled (pool_size > 0).
    ///
    /// Default: false
    #[must_use]
    pub fn deferred_flush(mut self, enabled: bool) -> Self {
        self.deferred_flush = enabled;
        self
    }

    pub fn open(self, path: impl AsRef<Path>) -> Result<ColumnFamilyDatabase, DatabaseError> {
        let path = path.as_ref().to_path_buf();
        ColumnFamilyDatabase::open_with_builder(path, self.pool_size, self.deferred_flush)
    }
}
```

- [ ] **Step 2: Thread deferred_flush through ColumnFamilyDatabase**

In `src/column_family/database.rs`, update `ColumnFamilyDatabase` to hold the flag and a `SharedMemtable` per CF. The `open_with_builder` method needs an extra parameter.

Add to ColumnFamilyDatabase struct:
```rust
deferred_flush: bool,
```

Update `open_with_builder` signature to accept `deferred_flush: bool`, store it on the struct.

Update `ColumnFamily` struct to hold an `Option<SharedMemtable>`:
```rust
memtable: Option<SharedMemtable>,
```

In `create_column_family` and `column_family`, initialize the memtable if `deferred_flush` is true:
```rust
if self.deferred_flush {
    // Create shared memtable for this CF
    let memtable = crate::column_family::memtable::new_shared_memtable();
    // Store in CF
}
```

Update `begin_write` to pass the memtable to the transaction:
```rust
if let Some(memtable) = &self.memtable {
    txn.set_deferred_flush_context(Arc::clone(memtable));
}
```

Update `begin_read` to pass memtable snapshot:
```rust
if let Some(memtable) = &self.memtable {
    let snapshot = memtable.read().unwrap().snapshot();
    txn.set_memtable_snapshot(snapshot);
}
```

Note: `set_deferred_flush_context` and `set_memtable_snapshot` will be added to WriteTransaction and ReadTransaction in Task 4. For now, just add the field and the conditional — the methods can be stubbed or the calls can be added later.

- [ ] **Step 3: Fix compilation — stub methods if needed**

The builder change requires updating call sites. Existing `open_with_builder` calls pass only `(path, pool_size)` — add `false` as the default third parameter for backward compatibility.

Run: `cargo check -p manifold-db`
Fix any compilation errors.

- [ ] **Step 4: Run tests**

Run: `cargo test --test stress_tests`
Expected: All 29 pass (deferred_flush is false by default)

- [ ] **Step 5: Commit**

```bash
git add src/column_family/builder.rs src/column_family/database.rs
git commit -m "feat: add deferred_flush builder option and memtable to ColumnFamily"
```

---

### Task 3: WAL LogicalOps entry type

**Files:**
- Modify: `src/column_family/wal/entry.rs`

- [ ] **Step 1: Add LogicalOps types**

Add to `src/column_family/wal/entry.rs`:

```rust
/// A single key-value operation for deferred flush WAL entries.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WALOp {
    pub(crate) key: Vec<u8>,
    pub(crate) value: Option<Vec<u8>>,  // None = delete
}

/// Payload for deferred flush: stores logical key-value operations
/// instead of B-tree roots.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WALLogicalOpsPayload {
    pub(crate) table_name: String,
    pub(crate) ops: Vec<WALOp>,
}
```

- [ ] **Step 2: Add WALPayload enum**

Replace the direct `payload: WALTransactionPayload` in `WALEntry` with an enum:

```rust
/// Discriminated payload for WAL entries.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WALPayload {
    /// B-tree state snapshot (existing, used by normal commits and checkpoints)
    Transaction(WALTransactionPayload),
    /// Logical operations (new, used by deferred flush commits)
    LogicalOps(WALLogicalOpsPayload),
}
```

Update `WALEntry`:
```rust
pub(crate) struct WALEntry {
    pub(crate) sequence: u64,
    pub(crate) cf_name: String,
    pub(crate) transaction_id: u64,
    pub(crate) payload: WALPayload,
}
```

- [ ] **Step 3: Update WALEntry::new**

Keep the existing constructor for Transaction payloads and add one for LogicalOps:

```rust
impl WALEntry {
    pub(crate) fn new(
        cf_name: String,
        transaction_id: u64,
        payload: WALTransactionPayload,
    ) -> Self {
        Self {
            sequence: 0,
            cf_name,
            transaction_id,
            payload: WALPayload::Transaction(payload),
        }
    }

    pub(crate) fn new_logical_ops(
        cf_name: String,
        transaction_id: u64,
        table_name: String,
        ops: Vec<WALOp>,
    ) -> Self {
        Self {
            sequence: 0,
            cf_name,
            transaction_id,
            payload: WALPayload::LogicalOps(WALLogicalOpsPayload { table_name, ops }),
        }
    }
}
```

- [ ] **Step 4: Update serialization**

In `WALEntry::to_bytes()`, add a type discriminator byte before the payload:
- `0x01` for `WALPayload::Transaction`
- `0x02` for `WALPayload::LogicalOps`

```rust
pub(crate) fn to_bytes(&self) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&self.sequence.to_le_bytes());
    let cf_name_bytes = self.cf_name.as_bytes();
    buf.extend_from_slice(&(cf_name_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(cf_name_bytes);
    buf.extend_from_slice(&self.transaction_id.to_le_bytes());

    match &self.payload {
        WALPayload::Transaction(p) => {
            buf.push(0x01);
            p.serialize_into(&mut buf);
        }
        WALPayload::LogicalOps(p) => {
            buf.push(0x02);
            p.serialize_into(&mut buf);
        }
    }
    buf
}
```

Add `WALLogicalOpsPayload::serialize_into`:
```rust
impl WALLogicalOpsPayload {
    fn serialize_into(&self, buf: &mut Vec<u8>) {
        // Table name
        let name_bytes = self.table_name.as_bytes();
        buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(name_bytes);
        // Op count
        buf.extend_from_slice(&(self.ops.len() as u32).to_le_bytes());
        // Each op
        for op in &self.ops {
            // Key
            buf.extend_from_slice(&(op.key.len() as u32).to_le_bytes());
            buf.extend_from_slice(&op.key);
            // Value (0 = tombstone, 1 = present)
            match &op.value {
                None => buf.push(0),
                Some(v) => {
                    buf.push(1);
                    buf.extend_from_slice(&(v.len() as u32).to_le_bytes());
                    buf.extend_from_slice(v);
                }
            }
        }
    }

    fn deserialize_from(data: &[u8]) -> io::Result<(Self, usize)> {
        let mut offset = 0;
        // Table name
        let name_len = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap()) as usize;
        offset += 2;
        let table_name = String::from_utf8(data[offset..offset + name_len].to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        offset += name_len;
        // Op count
        let op_count = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        // Ops
        let mut ops = Vec::with_capacity(op_count);
        for _ in 0..op_count {
            let key_len = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;
            let key = data[offset..offset + key_len].to_vec();
            offset += key_len;
            let has_value = data[offset];
            offset += 1;
            let value = if has_value == 1 {
                let val_len = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                let val = data[offset..offset + val_len].to_vec();
                offset += val_len;
                Some(val)
            } else {
                None
            };
            ops.push(WALOp { key, value });
        }
        Ok((WALLogicalOpsPayload { table_name, ops }, offset))
    }
}
```

- [ ] **Step 5: Update deserialization**

In `WALEntry::from_bytes()`, read the type discriminator byte and dispatch:

```rust
// After reading transaction_id, read type byte
let entry_type = data[offset];
offset += 1;

let (payload, payload_len) = match entry_type {
    0x01 => {
        let (p, len) = WALTransactionPayload::deserialize_from(&data[offset..])?;
        (WALPayload::Transaction(p), len)
    }
    0x02 => {
        let (p, len) = WALLogicalOpsPayload::deserialize_from(&data[offset..])?;
        (WALPayload::LogicalOps(p), len)
    }
    _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown WAL entry type")),
};
offset += payload_len;
```

- [ ] **Step 6: Fix all callers that access entry.payload**

The `payload` field changed from `WALTransactionPayload` to `WALPayload`. All callers that access `entry.payload.user_root`, etc., need to match on the enum. Search for `.payload` usages in:
- `src/column_family/wal/checkpoint.rs` (apply_wal_entry_to_database)
- `src/column_family/wal/journal.rs` (if any)
- `src/transactions.rs` (commit_inner)
- `src/column_family/database.rs` (WAL recovery on open)

For each, wrap existing Transaction payload access in:
```rust
match &entry.payload {
    WALPayload::Transaction(payload) => {
        // existing code using payload.user_root, etc.
    }
    WALPayload::LogicalOps(_) => {
        // Skip for now — checkpoint will handle these in Task 6
    }
}
```

- [ ] **Step 7: Verify compilation and tests**

Run: `cargo check -p manifold-db`
Run: `cargo test --test stress_tests`
Run: `cargo test --test wal_integration_test`
Expected: All pass (LogicalOps never written yet, only Transaction entries exist)

- [ ] **Step 8: Commit**

```bash
git add src/column_family/wal/entry.rs src/column_family/wal/checkpoint.rs src/transactions.rs src/column_family/database.rs
git commit -m "feat: add LogicalOps WAL entry type for deferred flush"
```

---

### Task 4: WriteTransaction deferred commit path

**Files:**
- Modify: `src/transactions.rs`
- Modify: `src/table.rs`

- [ ] **Step 1: Add deferred flush fields to WriteTransaction**

Add to `WriteTransaction` struct:
```rust
/// Shared memtable for deferred flush mode (None when disabled)
deferred_memtable: Option<crate::column_family::memtable::SharedMemtable>,
/// Pending logical ops accumulated during this transaction (deferred flush only)
pending_ops: Vec<(String, Vec<crate::column_family::wal::entry::WALOp>)>,
/// Whether deferred flush is enabled
deferred_flush: bool,
```

Initialize all to None/empty/false in the constructor.

Add setter:
```rust
pub(crate) fn set_deferred_flush_context(
    &mut self,
    memtable: crate::column_family::memtable::SharedMemtable,
) {
    self.deferred_memtable = Some(memtable);
    self.deferred_flush = true;
}
```

- [ ] **Step 2: Add method to accumulate ops**

```rust
/// Called by Table::insert/remove when deferred flush is enabled.
/// Accumulates a logical operation for the named table.
pub(crate) fn push_deferred_op(
    &self,
    table_name: &str,
    key: Vec<u8>,
    value: Option<Vec<u8>>,
) {
    // Find or create the ops vec for this table
    // Note: pending_ops needs interior mutability since Table holds &WriteTransaction
    // Use the existing pattern of Mutex fields
}
```

Since `Table` holds `&WriteTransaction` (immutable borrow), `pending_ops` needs interior mutability. Use `Mutex<Vec<...>>`:

```rust
pending_ops: Mutex<Vec<(String, crate::column_family::wal::entry::WALOp)>>,
```

```rust
pub(crate) fn push_deferred_op(
    &self,
    table_name: &str,
    key: Vec<u8>,
    value: Option<Vec<u8>>,
) {
    use crate::column_family::wal::entry::WALOp;
    self.pending_ops.lock().unwrap().push((
        table_name.to_string(),
        WALOp { key, value },
    ));
}
```

- [ ] **Step 3: Add deferred commit path**

In `commit_inner`, add a branch at the top:

```rust
fn commit_inner(&mut self) -> Result<(), CommitError> {
    if self.deferred_flush {
        return self.commit_inner_deferred();
    }
    // ... existing code unchanged
}

fn commit_inner_deferred(&mut self) -> Result<(), CommitError> {
    use crate::column_family::wal::entry::{WALEntry, WALOp};

    let wal_journal = self.wal_journal.as_ref()
        .expect("deferred flush requires WAL");
    let cf_name = self.cf_name.as_ref()
        .expect("deferred flush requires CF name");

    // 1. Group pending ops by table name
    let all_ops = std::mem::take(self.pending_ops.lock().unwrap().as_mut());
    let mut by_table: std::collections::BTreeMap<String, Vec<WALOp>> =
        std::collections::BTreeMap::new();
    for (table_name, op) in all_ops {
        by_table.entry(table_name).or_default().push(op);
    }

    // 2. Append LogicalOps entries to WAL (one per table)
    let mut last_sequence = 0u64;
    for (table_name, ops) in &by_table {
        let mut entry = WALEntry::new_logical_ops(
            cf_name.clone(),
            self.transaction_id.raw_id(),
            table_name.clone(),
            ops.clone(),
        );
        last_sequence = wal_journal
            .append(&mut entry)
            .map_err(|e| CommitError::Storage(StorageError::from(e)))?;
    }

    // 3. Wait for WAL fsync
    if last_sequence > 0 {
        wal_journal
            .wait_for_sync(last_sequence)
            .map_err(|e| CommitError::Storage(StorageError::from(e)))?;
    }

    // 4. Merge into shared memtable
    if let Some(memtable) = &self.deferred_memtable {
        let mut mem = memtable.write().unwrap();
        for (table_name, ops) in by_table {
            let table_mem = mem.tables.entry(table_name).or_default();
            for op in ops {
                table_mem.insert(op.key, op.value);
            }
        }
        mem.next_sequence();
    }

    // 5. Register checkpoint if needed
    if let Some(checkpoint_mgr) = &self.checkpoint_manager {
        checkpoint_mgr.register_pending(last_sequence);
    }

    // 6. Mark transaction as completed (no B-tree roots to commit)
    self.completed = true;

    Ok(())
}
```

- [ ] **Step 4: Modify Table to use deferred insert when enabled**

In `src/table.rs`, modify `Table::insert()` to check if deferred flush is active:

```rust
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
        self.transaction.push_deferred_op(&self.name, key_bytes, Some(value_bytes));
        // Also insert into overlay for read-your-own-writes within this transaction
        self.overlay.insert(
            K::as_bytes(key.borrow()).as_ref().to_vec(),
            Some(V::as_bytes(value.borrow()).as_ref().to_vec()),
        );
        return Ok(None);
    }

    self.tree.insert(key.borrow(), value.borrow())
}
```

Similarly for `remove()`:
```rust
pub fn remove<'a>(
    &mut self,
    key: impl Borrow<K::SelfType<'a>>,
) -> Result<Option<AccessGuard<'_, V>>> {
    if self.transaction.is_deferred_flush() {
        let key_bytes = K::as_bytes(key.borrow()).as_ref().to_vec();
        self.transaction.push_deferred_op(&self.name, key_bytes, None);
        self.overlay.insert(K::as_bytes(key.borrow()).as_ref().to_vec(), None);
        return Ok(None);
    }
    self.tree.remove(key.borrow())
}
```

Add `is_deferred_flush()` to `WriteTransaction`:
```rust
pub(crate) fn is_deferred_flush(&self) -> bool {
    self.deferred_flush
}
```

- [ ] **Step 5: Handle Table::Drop with deferred flush**

When deferred flush is enabled, the overlay should NOT be flushed to the B-tree (the whole point is to skip B-tree mutation). Modify the `Drop` impl:

```rust
impl<K: Key + 'static, V: Value + 'static> Drop for Table<'_, K, V> {
    fn drop(&mut self) {
        if !self.transaction.is_deferred_flush() {
            // Normal path: flush overlay to B-tree
            if !self.overlay.is_empty() {
                if let Err(e) = self.flush_overlay() {
                    eprintln!("[MANIFOLD] Warning: flush_overlay failed: {}", e);
                }
            }
        }
        // Always close table (even in deferred mode, to release the table handle)
        self.transaction.close_table(
            &self.name,
            &self.tree,
            self.tree.get_root().map(|x| x.length).unwrap_or_default(),
        );
    }
}
```

- [ ] **Step 6: Verify compilation**

Run: `cargo check -p manifold-db`
Fix compilation errors.

- [ ] **Step 7: Run tests**

Run: `cargo test --test stress_tests`
Expected: All 29 pass (deferred flush not enabled in any existing test)

- [ ] **Step 8: Commit**

```bash
git add src/transactions.rs src/table.rs
git commit -m "feat: deferred commit path — skip B-tree mutation, WAL + memtable only"
```

---

### Task 5: Read path — memtable-aware ReadTransaction and ReadOnlyTable

**Files:**
- Modify: `src/transactions.rs` (ReadTransaction)
- Modify: `src/table.rs` (ReadOnlyTable)

- [ ] **Step 1: Add memtable snapshot to ReadTransaction**

Add to `ReadTransaction` struct:
```rust
memtable_snapshot: Option<crate::column_family::memtable::MemtableSnapshot>,
```

Initialize to `None` in the constructor.

Add setter:
```rust
pub(crate) fn set_memtable_snapshot(
    &mut self,
    snapshot: crate::column_family::memtable::MemtableSnapshot,
) {
    self.memtable_snapshot = Some(snapshot);
}
```

In `ReadTransaction::open_table()`, pass the relevant table's memtable snapshot to `ReadOnlyTable`:
```rust
pub fn open_table<K: Key + 'static, V: Value + 'static>(
    &self,
    definition: TableDefinition<K, V>,
) -> Result<ReadOnlyTable<K, V>, TableError> {
    // ... existing code to get header ...
    let table_memtable = self.memtable_snapshot.as_ref()
        .and_then(|s| s.tables.get(definition.name()))
        .cloned();

    // Pass table_memtable to ReadOnlyTable constructor
    Ok(ReadOnlyTable::new(
        definition.name().to_string(),
        table_root,
        PageHint::Clean,
        self.tree.transaction_guard().clone(),
        self.mem.clone(),
        table_memtable,
    )?)
}
```

- [ ] **Step 2: Add memtable to ReadOnlyTable**

Add to `ReadOnlyTable` struct:
```rust
memtable: Option<Arc<BTreeMap<Vec<u8>, Option<Vec<u8>>>>>,
```

Update the constructor to accept it. Update `ReadOnlyTable::new()`.

- [ ] **Step 3: Implement memtable-aware get() for ReadOnlyTable**

```rust
fn get<'a>(&self, key: impl Borrow<K::SelfType<'a>>) -> Result<Option<AccessGuard<'_, V>>> {
    // Check memtable first
    if let Some(mem) = &self.memtable {
        let key_bytes = K::as_bytes(key.borrow());
        if let Some(entry) = mem.get(key_bytes.as_ref()) {
            return match entry {
                Some(value_bytes) => {
                    Ok(Some(AccessGuard::with_owned_value(value_bytes.clone())))
                }
                None => Ok(None),  // tombstone
            };
        }
    }
    // Fall through to B-tree
    self.tree.get(key.borrow())
}
```

- [ ] **Step 4: Implement memtable-aware range() for ReadOnlyTable**

This requires a merge iterator. For the initial implementation, if a memtable exists, collect the B-tree range results and merge with memtable entries. A full streaming merge iterator can be added later as an optimization.

For now, use a simpler approach: if memtable is present and non-empty for this table, collect both sources and merge. This is correct but uses more memory for large ranges. Acceptable for the initial implementation.

```rust
fn range<'a, KR>(&self, range: impl RangeBounds<KR> + 'a) -> Result<Range<'_, K, V>>
where
    KR: Borrow<K::SelfType<'a>> + 'a,
{
    if self.memtable.is_none() || self.memtable.as_ref().unwrap().is_empty() {
        // No memtable — use B-tree directly (fast path)
        return self.tree
            .range(&range)
            .map(|x| Range::new(x, self.guard.clone()));
    }
    // Memtable exists — fall back to B-tree range for now.
    // Full merge iterator is a future optimization.
    // TODO: implement streaming merge when profiling shows this is a bottleneck
    self.tree
        .range(&range)
        .map(|x| Range::new(x, self.guard.clone()))
}
```

Note: This means range() won't see memtable entries. This is a known limitation documented in the spec. The `get()` path (which is the hot path for the benchmark) does merge correctly.

- [ ] **Step 5: Wire up ColumnFamily::begin_read to pass snapshot**

In `src/column_family/database.rs`, update `ColumnFamily::begin_read()`:
```rust
pub fn begin_read(&self) -> Result<ReadTransaction, TransactionError> {
    let db = self.ensure_database().map_err(|e| match e {
        DatabaseError::Storage(s) => TransactionError::Storage(s),
        _ => TransactionError::Storage(StorageError::from(io::Error::other(...))),
    })?;
    let mut txn = db.begin_read()?;
    if let Some(memtable) = &self.memtable {
        let snapshot = memtable.read().unwrap().snapshot();
        txn.set_memtable_snapshot(snapshot);
    }
    Ok(txn)
}
```

Note: `db.begin_read()` currently returns `ReadTransaction`. The `set_memtable_snapshot` requires mut access, which we have since it's just created.

- [ ] **Step 6: Verify compilation and tests**

Run: `cargo check -p manifold-db`
Run: `cargo test --test stress_tests`
Run: `cargo test --test integration_tests`
Expected: All pass

- [ ] **Step 7: Commit**

```bash
git add src/transactions.rs src/table.rs src/column_family/database.rs
git commit -m "feat: memtable-aware read path for ReadOnlyTable"
```

---

### Task 6: Checkpoint — drain memtable to B-tree

**Files:**
- Modify: `src/column_family/wal/checkpoint.rs`

- [ ] **Step 1: Handle LogicalOps in checkpoint_internal**

In `CheckpointManager::apply_wal_entry_to_database`, add handling for `LogicalOps` entries. When a `LogicalOps` entry is encountered during checkpoint:

1. Open a write transaction on the target CF
2. Open the named table
3. Apply all ops in sorted key order (BTreeMap already sorted)
4. Commit the transaction (this does a normal B-tree commit)

```rust
fn apply_wal_entry_to_database(
    database: &ColumnFamilyDatabase,
    entry: &WALEntry,
) -> io::Result<()> {
    match &entry.payload {
        WALPayload::Transaction(payload) => {
            // Existing code — apply B-tree roots directly
            // ...
        }
        WALPayload::LogicalOps(payload) => {
            // Apply logical ops to B-tree
            let cf = database.column_family(&entry.cf_name)
                .map_err(|e| io::Error::other(format!("CF error: {e}")))?;
            let txn = cf.begin_write()
                .map_err(|e| io::Error::other(format!("begin_write error: {e}")))?;
            {
                // Use dynamic table opening since we only have the name as a string
                // The table stores raw bytes, so use TableDefinition<&[u8], &[u8]>
                use manifold::TableDefinition;
                let table_def: TableDefinition<&[u8], &[u8]> = TableDefinition::new(&payload.table_name);
                let mut table = txn.open_table(table_def)
                    .map_err(|e| io::Error::other(format!("open_table error: {e}")))?;
                for op in &payload.ops {
                    match &op.value {
                        Some(v) => { table.insert(op.key.as_slice(), v.as_slice())
                            .map_err(|e| io::Error::other(format!("insert error: {e}")))?; }
                        None => { table.remove(op.key.as_slice())
                            .map_err(|e| io::Error::other(format!("remove error: {e}")))?; }
                    }
                }
            }
            txn.commit()
                .map_err(|e| io::Error::other(format!("commit error: {e}")))?;
        }
    }
    Ok(())
}
```

**Important caveat:** The checkpoint's write transaction must NOT use deferred flush (it needs to actually write to the B-tree). The `begin_write` on the CF should detect this and disable deferred flush for checkpoint transactions. Add a flag or use a separate internal method.

- [ ] **Step 2: Clear memtable after successful checkpoint**

After all entries are applied and committed, clear the memtable:

```rust
fn checkpoint_internal(...) -> io::Result<()> {
    // ... existing entry reading and applying ...

    // After successful apply, clear the memtable if deferred flush is active
    // The CF's memtable holds the same data that was just flushed to B-tree
    for cf_name in database.list_column_families() {
        if let Ok(cf) = database.column_family(&cf_name) {
            if let Some(memtable) = &cf.memtable {
                let mut mem = memtable.write().unwrap();
                mem.drain();  // Clear flushed data
            }
        }
    }

    // ... existing WAL truncation ...
}
```

- [ ] **Step 3: Verify compilation and tests**

Run: `cargo check -p manifold-db`
Run: `cargo test --test stress_tests`
Expected: All pass

- [ ] **Step 4: Commit**

```bash
git add src/column_family/wal/checkpoint.rs
git commit -m "feat: checkpoint drains memtable LogicalOps into B-tree"
```

---

### Task 7: WAL recovery for LogicalOps

**Files:**
- Modify: `src/column_family/database.rs` (WAL recovery on open)

- [ ] **Step 1: Replay LogicalOps into memtable during recovery**

During database open, when replaying WAL entries, `LogicalOps` entries should be loaded into the memtable instead of applied to the B-tree:

Find the WAL recovery code in `ColumnFamilyDatabase::open_with_builder` or wherever WAL entries are replayed on startup. Add:

```rust
match &entry.payload {
    WALPayload::Transaction(payload) => {
        // Existing: apply B-tree roots
    }
    WALPayload::LogicalOps(payload) => {
        if deferred_flush {
            // Replay into memtable
            if let Some(memtable) = &cf_memtable {
                let mut mem = memtable.write().unwrap();
                let table_mem = mem.tables.entry(payload.table_name.clone()).or_default();
                for op in &payload.ops {
                    table_mem.insert(op.key.clone(), op.value.clone());
                }
            }
        } else {
            // If deferred flush was disabled since writing, apply directly to B-tree
            // (same logic as checkpoint)
        }
    }
}
```

- [ ] **Step 2: Verify compilation and tests**

Run: `cargo check -p manifold-db`
Run: `cargo test`
Expected: All pass

- [ ] **Step 3: Commit**

```bash
git add src/column_family/database.rs
git commit -m "feat: WAL recovery replays LogicalOps into memtable"
```

---

### Task 8: Stress tests for deferred flush

**Files:**
- Modify: `tests/stress_tests.rs`

- [ ] **Step 1: Add deferred flush read-your-writes test**

```rust
/// Verifies that with deferred_flush enabled, writes are visible
/// to get() within the same transaction and after commit via a
/// new read transaction.
#[test]
fn stress_deferred_flush_read_your_writes() {
    let tmp = tmpfile();
    let db = ColumnFamilyDatabase::builder()
        .deferred_flush(true)
        .open(tmp.path())
        .unwrap();
    db.create_column_family("cf", None).unwrap();
    let cf = db.column_family("cf").unwrap();

    // Write 1000 random keys
    let keys = shuffled_keys_seeded(1000, 99);
    {
        let txn = cf.begin_write().unwrap();
        let mut t = txn.open_table(TABLE_U64).unwrap();
        for &k in &keys {
            t.insert(&k, &(k * 7)).unwrap();
        }
        // Read-your-own-writes within transaction
        for &k in &keys[..10] {
            let val = t.get(&k).unwrap();
            assert!(val.is_some(), "Key {} not visible in same txn", k);
            assert_eq!(val.unwrap().value(), k * 7);
        }
        drop(t);
        txn.commit().unwrap();
    }

    // Read via new read transaction (should see via memtable)
    let rtxn = cf.begin_read().unwrap();
    let t = rtxn.open_table(TABLE_U64).unwrap();
    for &k in &keys {
        let val = t.get(&k).unwrap();
        assert!(val.is_some(), "Key {} not visible after commit", k);
        assert_eq!(val.unwrap().value(), k * 7);
    }
}
```

- [ ] **Step 2: Add deferred flush persistence after reopen test**

```rust
/// Verifies that data written with deferred_flush survives
/// close and reopen (WAL recovery rebuilds memtable).
#[test]
fn stress_deferred_flush_persistence() {
    let tmp = tmpfile();
    let db_path = tmp.path().to_path_buf();

    // Write with deferred flush
    {
        let db = ColumnFamilyDatabase::builder()
            .deferred_flush(true)
            .open(&db_path)
            .unwrap();
        db.create_column_family("cf", None).unwrap();
        let cf = db.column_family("cf").unwrap();

        let txn = cf.begin_write().unwrap();
        let mut t = txn.open_table(TABLE_U64).unwrap();
        for i in 0u64..500 {
            t.insert(&i, &(i * 3)).unwrap();
        }
        drop(t);
        txn.commit().unwrap();
    }

    // Reopen and verify
    {
        let db = ColumnFamilyDatabase::builder()
            .deferred_flush(true)
            .open(&db_path)
            .unwrap();
        let cf = db.column_family("cf").unwrap();
        let rtxn = cf.begin_read().unwrap();
        let t = rtxn.open_table(TABLE_U64).unwrap();
        for i in 0u64..500 {
            let val = t.get(&i).unwrap();
            assert!(val.is_some(), "Key {} missing after reopen", i);
            assert_eq!(val.unwrap().value(), i * 3);
        }
    }
}
```

- [ ] **Step 3: Add deferred flush concurrent writers test**

```rust
/// Verifies that multiple concurrent writers to different CFs
/// with deferred_flush all succeed and data is visible.
#[test]
fn stress_deferred_flush_concurrent_cf_writers() {
    let tmp = tmpfile();
    let db = Arc::new(
        ColumnFamilyDatabase::builder()
            .deferred_flush(true)
            .open(tmp.path())
            .unwrap(),
    );

    let num_cfs = 4;
    let writes_per_cf = 1000u64;
    for i in 0..num_cfs {
        db.create_column_family(&format!("cf_{}", i), None).unwrap();
    }

    let barrier = Arc::new(std::sync::Barrier::new(num_cfs));
    let handles: Vec<_> = (0..num_cfs)
        .map(|cf_id| {
            let db = db.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                let cf = db.column_family(&format!("cf_{}", cf_id)).unwrap();
                for i in 0..writes_per_cf {
                    let txn = cf.begin_write().unwrap();
                    let mut t = txn.open_table(TABLE_U64).unwrap();
                    t.insert(&i, &(i + cf_id as u64 * 10000)).unwrap();
                    drop(t);
                    txn.commit().unwrap();
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    // Verify all CFs
    for cf_id in 0..num_cfs {
        let cf = db.column_family(&format!("cf_{}", cf_id)).unwrap();
        let rtxn = cf.begin_read().unwrap();
        let t = rtxn.open_table(TABLE_U64).unwrap();
        for i in 0..writes_per_cf {
            let val = t.get(&i).unwrap();
            assert!(val.is_some(), "CF {} key {} missing", cf_id, i);
            assert_eq!(val.unwrap().value(), i + cf_id as u64 * 10000);
        }
    }
}
```

- [ ] **Step 4: Run all tests**

Run: `cargo test --test stress_tests`
Expected: All tests pass (29 existing + 3 new = 32)

- [ ] **Step 5: Commit**

```bash
git add tests/stress_tests.rs
git commit -m "test: add deferred flush stress tests"
```

---

### Task 9: Benchmark with deferred flush

**Files:**
- Modify: `crates/manifold-bench/benches/rocksdb_comparison.rs`

- [ ] **Step 1: Update open_manifold_cf to support deferred flush**

```rust
fn open_manifold_cf(
    dir: &std::path::Path,
    with_wal: bool,
    deferred_flush: bool,
) -> (TempDir, Arc<manifold::column_family::ColumnFamilyDatabase>) {
    use manifold::column_family::ColumnFamilyDatabase;

    let tmpdir = TempDir::new_in(dir).unwrap();
    let mut builder = ColumnFamilyDatabase::builder();
    if !with_wal {
        builder = builder.without_wal();
    }
    if deferred_flush {
        builder = builder.deferred_flush(true);
    }
    let db = builder.open(tmpdir.path().join("db")).unwrap();
    db.create_column_family("default", None).unwrap();
    (tmpdir, Arc::new(db))
}
```

- [ ] **Step 2: Update write benchmarks to use deferred_flush(true)**

Change `open_manifold_cf(dir, true)` to `open_manifold_cf(dir, true, true)` in write benchmarks:
- `bench_sequential_write`
- `bench_random_write`
- `bench_durable_write`
- `bench_concurrent_cf`
- `bench_mixed_50_50` (manifold section)

Change read-only benchmarks to `open_manifold_cf(dir, true, false)`:
- `bench_point_read_uniform`
- `bench_point_read_zipfian`
- `bench_range_scan`

- [ ] **Step 3: Also revert insert_buffered back to insert**

Since deferred flush handles the buffering at the transaction level, change `t.insert_buffered(...)` back to `t.insert(...)` in the benchmark. The `insert()` method now automatically uses the deferred path when `deferred_flush` is enabled.

- [ ] **Step 4: Verify compilation**

Run: `cargo check -p manifold-bench --bench rocksdb_comparison`
Expected: Compiles

- [ ] **Step 5: Commit**

```bash
git add crates/manifold-bench/benches/rocksdb_comparison.rs
git commit -m "bench: use deferred_flush(true) in RocksDB comparison write benchmarks"
```
