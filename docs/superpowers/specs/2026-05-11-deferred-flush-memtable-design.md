# Deferred Flush Memtable (Phase 2 Write Optimization) Design

## Goal

Eliminate B-tree mutation from the commit path for column family databases. Write transactions append logical operations to the WAL and merge them into a shared in-memory memtable. Read transactions merge the memtable with the B-tree. A background checkpoint periodically drains the memtable into the B-tree in sorted order. This matches RocksDB's write architecture while preserving manifold's B-tree read advantages.

## Problem

Manifold's COW B-tree mutates pages on every `insert()`, causing random page I/O at scale. Even with the Phase 1 write buffer (per-batch sorting), the B-tree is fully updated on every commit. Benchmarks show 0.09x RocksDB performance for random writes at 10M keys — the B-tree's working set exceeds cache, causing disk I/O on every insert.

RocksDB avoids this by writing to an in-memory memtable + sequential WAL append on commit. B-tree (SST) updates happen asynchronously via compaction. The commit path never touches the persistent data structure.

## Opt-in activation

Enabled via builder:
```rust
ColumnFamilyDatabase::builder()
    .deferred_flush(true)  // new option, default false
    .open(path)
```

When disabled (default), the commit path works exactly as today. All existing behavior is preserved.

## Architecture

### Memtable

A `BTreeMap<Vec<u8>, Option<Vec<u8>>>` per table, wrapped in a struct that supports atomic snapshot creation. Lives at the `ColumnFamily` level, shared across transactions via `Arc`.

```rust
/// Per-table memtable: sorted key-value buffer
struct TableMemtable {
    entries: BTreeMap<Vec<u8>, Option<Vec<u8>>>,  // None = tombstone
    size_bytes: usize,  // approximate memory usage for checkpoint triggering
}

/// All memtable state for a column family
struct Memtable {
    tables: HashMap<String, TableMemtable>,
    sequence: u64,  // monotonic sequence for snapshot ordering
}
```

The memtable is behind `Arc<RwLock<Memtable>>`:
- Write transactions take a write lock briefly to merge their buffer on commit
- Read transactions take a read lock briefly to clone a snapshot at begin_read time
- The checkpoint thread takes a write lock briefly to swap in a fresh memtable

### Memtable snapshot for readers

When a read transaction starts, it captures an immutable snapshot of the memtable:

```rust
struct MemtableSnapshot {
    tables: HashMap<String, Arc<BTreeMap<Vec<u8>, Option<Vec<u8>>>>>,
    sequence: u64,
}
```

The snapshot is cheap to create: clone the `Arc` pointers to each table's BTreeMap (not the data itself). The snapshot is immutable — concurrent writes go to the live memtable, not the snapshot.

When the memtable is checkpointed (swapped for a fresh one), existing snapshots still hold `Arc` references to the old data. The old memtable is freed when the last snapshot referencing it is dropped.

## Write Path

### Current (deferred_flush disabled)
```
insert(k, v) → BtreeMut::insert() (COW page mutation)
commit()     → flush_and_close() → WAL append → non_durable_commit()
```

### New (deferred_flush enabled)
```
insert(k, v) → accumulate in transaction-local Vec<(table, key, value)>
commit()     → WAL append (LogicalOps) → merge into shared memtable → done
```

#### Transaction-local buffer

`WriteTransaction` gains a `pending_ops: Vec<PendingOp>` field (only allocated when deferred_flush is enabled):

```rust
struct PendingOp {
    table_name: String,
    key: Vec<u8>,
    value: Option<Vec<u8>>,  // None = delete
}
```

`Table::insert()` serializes key/value and pushes to `pending_ops` instead of calling `BtreeMut::insert()`. `Table::remove()` pushes a tombstone.

#### Commit (deferred path)

```rust
fn commit_inner_deferred(&mut self) -> Result<(), CommitError> {
    // 1. Append logical ops to WAL
    let wal_entry = WALEntry::new_logical_ops(
        self.cf_name.clone(),
        self.transaction_id,
        &self.pending_ops,
    );
    self.wal_journal.append(&wal_entry)?;
    self.wal_journal.wait_for_sync(sequence)?;

    // 2. Merge into shared memtable
    let mut memtable = self.memtable.write().unwrap();
    for op in self.pending_ops.drain(..) {
        let table = memtable.tables.entry(op.table_name).or_default();
        match &op.value {
            Some(v) => table.size_bytes += op.key.len() + v.len(),
            None => {}
        }
        table.entries.insert(op.key, op.value);
    }
    memtable.sequence += 1;

    // 3. Register checkpoint if memtable exceeds threshold
    if memtable.size_bytes() > self.checkpoint_threshold {
        self.checkpoint_manager.request_checkpoint();
    }

    // 4. Update transaction tracker (no B-tree roots to commit)
    self.transaction_tracker.register_non_durable_commit(...);

    Ok(())
}
```

No `flush_and_close()`, no B-tree page allocation, no COW copies. The commit cost is one WAL append (sequential I/O) + one memtable merge (in-memory).

## Read Path

### ReadTransaction creation

`ColumnFamily::begin_read()` captures a memtable snapshot:

```rust
pub fn begin_read(&self) -> Result<ReadTransaction, TransactionError> {
    let db = self.ensure_database()?;
    let memtable_snapshot = if self.deferred_flush {
        Some(self.memtable.read().unwrap().snapshot())
    } else {
        None
    };
    let txn = db.begin_read()?;
    // Thread memtable_snapshot into the transaction
    Ok(txn.with_memtable(memtable_snapshot))
}
```

### ReadOnlyTable::get()

```rust
fn get(&self, key: K::SelfType<'_>) -> Result<Option<AccessGuard<'_, V>>> {
    // Check memtable first
    if let Some(snapshot) = &self.memtable_snapshot {
        let key_bytes = K::as_bytes(&key);
        if let Some(table_mem) = snapshot.tables.get(&self.table_name) {
            if let Some(entry) = table_mem.get(key_bytes.as_ref()) {
                return match entry {
                    Some(value_bytes) => Ok(Some(AccessGuard::with_owned_value(value_bytes.clone()))),
                    None => Ok(None),  // tombstone
                };
            }
        }
    }
    // Fall through to B-tree
    self.tree.get(&key)
}
```

### ReadOnlyTable::range() / iter() — Merge Iterator

Returns a `MergeRange` that combines two sorted iterators:

```rust
struct MergeRange<'a, K, V> {
    btree_iter: BtreeRangeIter<K, V>,
    mem_iter: std::collections::btree_map::Range<'a, Vec<u8>, Option<Vec<u8>>>,
    // Peeked values for merge logic
    btree_next: Option<Result<BtreeEntry>>,
    mem_next: Option<(&'a Vec<u8>, &'a Option<Vec<u8>>)>,
}

impl Iterator for MergeRange<'_, K, V> {
    type Item = Result<(AccessGuard<K>, AccessGuard<V>)>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match (&self.btree_next, &self.mem_next) {
                (None, None) => return None,
                (Some(_), None) => return self.take_btree(),
                (None, Some((_, None))) => { self.advance_mem(); continue; }  // skip tombstone
                (None, Some((_, Some(_)))) => return self.take_mem(),
                (Some(_), Some((mem_key, _))) => {
                    let cmp = K::compare(btree_key_bytes, mem_key);
                    match cmp {
                        Ordering::Less => return self.take_btree(),
                        Ordering::Greater => {
                            if mem_value.is_none() { self.advance_mem(); continue; }
                            return self.take_mem();
                        }
                        Ordering::Equal => {
                            self.advance_btree();  // memtable wins
                            if mem_value.is_none() { self.advance_mem(); continue; }
                            return self.take_mem();
                        }
                    }
                }
            }
        }
    }
}
```

### Table (write-side) reads within a transaction

`Table::get()` with deferred flush enabled checks:
1. Transaction-local `pending_ops` buffer (for read-your-own-writes within the current uncommitted transaction)
2. Shared memtable (for writes committed by prior transactions)
3. B-tree

The Phase 1 overlay already handles step 1. Steps 2 and 3 are the new merge.

## Checkpoint

### When it triggers

- **Size threshold**: When memtable exceeds a configurable size (default: 64MB)
- **Time threshold**: When memtable age exceeds a configurable interval (default: 60 seconds)
- **Manual**: `ColumnFamily::checkpoint()` for explicit control
- **Shutdown**: Automatic flush on `ColumnFamilyDatabase::drop()`

### What it does

```
1. Lock memtable (write lock, brief)
2. Swap current memtable for fresh empty one
3. Unlock memtable (new writes go to fresh memtable)
4. Sort all entries from old memtable by (table_name, key)
5. For each table, open a write transaction:
   a. Insert all entries in sorted key order (fast sequential B-tree insertion)
   b. Apply tombstones (remove deleted keys)
   c. Commit with WALTransactionPayload (records new B-tree root)
6. Truncate WAL up to the checkpoint sequence number
7. Drop old memtable (freed when last reader snapshot releases it)
```

The expensive B-tree insertion happens here, but:
- Keys are sorted → sequential leaf page access → minimal cache misses
- Happens in the background → doesn't block write transactions
- Batches many transactions' writes into one pass → amortizes B-tree overhead

### Existing CheckpointManager integration

The existing `CheckpointManager` already has:
- Background thread with periodic wakeup
- Size and time thresholds
- Registration of pending WAL sequences
- Integration with `ColumnFamilyDatabase` lifecycle

The deferred flush checkpoint extends this by adding memtable drain before the existing WAL-to-DB checkpoint logic. The WAL `LogicalOps` entries are consumed by building the memtable; the checkpoint flushes the memtable to B-tree and records the result as a normal `WALTransactionPayload`.

## WAL Changes

### New entry type

```rust
pub enum WALPayload {
    /// Existing: records B-tree state after a commit
    Transaction(WALTransactionPayload),
    /// New: records logical key-value operations (deferred flush mode)
    LogicalOps(WALLogicalOpsPayload),
}

pub struct WALLogicalOpsPayload {
    pub table_name: String,
    pub ops: Vec<WALOp>,
}

pub struct WALOp {
    pub key: Vec<u8>,
    pub value: Option<Vec<u8>>,  // None = delete
}
```

### Serialization

`WALOp` serialization: `[key_len: u32][key: bytes][has_value: u8][value_len: u32][value: bytes]`
`WALLogicalOpsPayload` serialization: `[table_name_len: u16][table_name: bytes][op_count: u32][ops...]`

The entry type byte in `WALEntry` distinguishes `Transaction` (0x01) from `LogicalOps` (0x02).

### Recovery

On startup with deferred flush enabled:
1. Replay all `LogicalOps` entries into the memtable (in WAL sequence order)
2. `Transaction` entries update the B-tree root as before
3. After replay, the memtable holds all un-checkpointed writes
4. Normal operation resumes — reads merge memtable + B-tree

Recovery speed: proportional to WAL size. With 64MB checkpoint threshold, worst case is replaying 64MB of logical ops into a BTreeMap — sub-second on modern hardware.

## Interaction with Existing Features

### Savepoints

When a savepoint is created or restored with deferred flush enabled, flush the memtable to B-tree first (checkpoint). This ensures savepoint semantics work correctly with the B-tree-based savepoint machinery. Savepoints are rare operations — the flush cost is acceptable.

### Concurrent column families

Each CF has its own memtable. CF isolation is maintained — writes to CF A's memtable don't affect CF B's memtable or B-tree. This preserves manifold's key advantage of independent CF write locks.

### Non-CF Database

`Database` (non-CF) is unchanged. Deferred flush only applies to `ColumnFamilyDatabase` because it requires the WAL infrastructure.

### Table operations requiring B-tree access

Operations that need the full materialized state (`get_mut`, `entry`, `pop_*`, `extract_if`, `retain`, `compact`, `check_integrity`) trigger a checkpoint first (flush memtable to B-tree). These are infrequent operations where the flush cost is acceptable.

## Files Modified

| File | Change |
|------|--------|
| `src/column_family/builder.rs` | Add `deferred_flush` option |
| `src/column_family/database.rs` | Hold `Arc<RwLock<Memtable>>`, pass to transactions |
| `src/column_family/memtable.rs` | **New file**: `Memtable`, `TableMemtable`, `MemtableSnapshot` |
| `src/column_family/wal/entry.rs` | Add `WALPayload::LogicalOps` variant and serialization |
| `src/column_family/wal/journal.rs` | Handle new entry type |
| `src/column_family/wal/checkpoint.rs` | Drain memtable → B-tree during checkpoint |
| `src/transactions.rs` | Add `pending_ops` buffer, `commit_inner_deferred()` path |
| `src/table.rs` | Deferred insert path (push to pending_ops instead of B-tree) |
| `src/table.rs` (ReadOnlyTable) | Merge memtable snapshot + B-tree for get/range/iter |
| `src/table.rs` | `MergeRange` iterator |

## Expected Performance Impact

- **Random writes**: 5-10x improvement (WAL append + memtable merge vs COW B-tree mutation)
- **Sequential writes**: 2-4x improvement (still faster to skip B-tree entirely)
- **Point reads**: Slight overhead (~5-10%) for memtable check before B-tree. Memtable check is O(log M) where M is memtable size (small between checkpoints).
- **Range scans**: Moderate overhead for merge iterator. Two sorted streams merged is O(N+M). For small memtables (between checkpoints), overhead is minimal.
- **Checkpoint**: One-time cost proportional to memtable size. Amortized across all writes since last checkpoint. Sequential B-tree insertion is fast.
- **Memory**: Memtable holds all un-checkpointed writes. With 64MB threshold, worst case is 64MB additional memory per CF.

## Testing Strategy

1. All existing tests pass unchanged (deferred flush disabled by default)
2. New tests with `.deferred_flush(true)`:
   - Read-your-own-writes across commit boundaries
   - Tombstone visibility after checkpoint
   - Crash recovery: kill after WAL write, reopen, verify memtable rebuilt
   - Checkpoint triggers correctly on size/time threshold
   - Concurrent readers see consistent snapshots during checkpoint
   - Merge iterator correctness (overlapping keys, tombstones, ranges)
3. Benchmark: RocksDB comparison with deferred flush — target 0.5-1.0x RocksDB random write throughput
