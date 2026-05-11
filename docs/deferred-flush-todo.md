# Deferred Flush: Remaining Work

## Status

Deferred flush mode (`ColumnFamilyDatabase::builder().deferred_flush(true)`) is partially fixed but **not yet safe to enable**. Point lookups (`Table::get()`) work correctly, but range scans (`Table::range()`, `Table::iter()`) skip memtable data.

## What's Already Done

Three fixes landed in commit `314d1a7` on `feature/manifold-sql`:

1. **Shared memtable per CF** (`src/column_family/state.rs`, `src/column_family/database.rs`): `SharedMemtable` is stored in `ColumnFamilyState` and shared across all `ColumnFamily` handles. Previously, each `column_family()` / `column_family_or_create()` call created a fresh empty memtable, so writes committed via one handle were invisible to reads via another.

2. **Deferred op visibility in same txn** (`src/transactions.rs`, `src/table.rs`): `Table::get()` now calls `WriteTransaction::get_deferred_op()` to find writes from prior Table handles in the same transaction. Without this, the pattern `open_table → insert → drop → open_table → get` returned None because the overlay was lost with the first Table handle.

3. **Memtable visibility in write txns** (`src/transactions.rs`, `src/table.rs`): `Table::get()` now calls `WriteTransaction::get_from_memtable()` to find committed data from prior transactions that hasn't been checkpointed to the B-tree yet.

## What's Missing: Merge Iterator for `range()` / `iter()`

### The Problem

`Table::range()` and `ReadOnlyTable::range()` delegate directly to `self.tree.range()` — the B-tree. They do not consult the memtable. This means:

- `SELECT * FROM t` after an INSERT in deferred_flush mode returns 0 rows
- Any full table scan, range scan, or iteration misses unflushed data
- FK constraint checks that scan child tables fail
- `COUNT(*)` returns wrong results

### The Fix: MergeIterator

Build a merge iterator that interleaves two sorted streams:
1. The B-tree range iterator (`BtreeRangeIter`)
2. A memtable range iterator (subset of the memtable's `BTreeMap` for this table)

The merge iterator must:
- Emit entries in key order (both streams are sorted)
- When the same key appears in both streams, prefer the memtable version (it's newer)
- Handle tombstones: memtable entry with `value = None` means the key was deleted — skip it AND suppress the B-tree entry for that key
- Support `DoubleEndedIterator` (reverse iteration)
- Work for both `ReadOnlyTable` (uses memtable snapshot) and `Table` (uses overlay + deferred ops + memtable)

### Implementation Plan

**Files to modify:**
- `src/table.rs` — `ReadableTable::range()` and `ReadOnlyTable::range()` implementations
- New file: `src/tree_store/merge_iter.rs` — `MergeIterator<K, V>` that wraps a B-tree iterator + memtable iterator

**MergeIterator sketch:**

```rust
pub struct MergeIterator<K: Key, V: Value> {
    btree_iter: BtreeRangeIter<K, V>,
    mem_iter: std::collections::btree_map::Range<'_, Vec<u8>, Option<Vec<u8>>>,
    // Peek buffers for merge logic
    btree_next: Option<(Vec<u8>, Vec<u8>)>,
    mem_next: Option<(Vec<u8>, Option<Vec<u8>>)>,
}

impl<K: Key, V: Value> Iterator for MergeIterator<K, V> {
    type Item = Result<(AccessGuard<K>, AccessGuard<V>)>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match (&self.btree_next, &self.mem_next) {
                (None, None) => return None,
                (Some(_), None) => return self.emit_btree(),
                (None, Some((_, None))) => { self.advance_mem(); continue; } // tombstone, skip
                (None, Some((_, Some(_)))) => return self.emit_mem(),
                (Some((bk, _)), Some((mk, mv))) => {
                    match bk.cmp(mk) {
                        Ordering::Less => return self.emit_btree(),
                        Ordering::Greater => {
                            if mv.is_none() { self.advance_mem(); continue; } // tombstone
                            return self.emit_mem();
                        }
                        Ordering::Equal => {
                            // Same key — memtable wins (newer)
                            self.advance_btree(); // discard B-tree entry
                            if mv.is_none() { self.advance_mem(); continue; } // tombstone
                            return self.emit_mem();
                        }
                    }
                }
            }
        }
    }
}
```

**Integration in `Table::range()`:**

```rust
fn range<'a, KR>(&self, range: impl RangeBounds<KR> + 'a) -> Result<Range<'_, K, V>> {
    if self.transaction.is_deferred_flush() {
        // Get memtable entries for this table within the range
        let mem_entries = self.get_memtable_range(&range);
        let btree_iter = self.tree.range(&range)?;
        Ok(Range::new_merged(btree_iter, mem_entries))
    } else {
        self.tree.range(&range).map(|x| Range::new(x))
    }
}
```

**For `ReadOnlyTable::range()`:** Same approach but using the memtable snapshot instead of the live memtable.

### Complexity Estimate

- MergeIterator: ~150 lines
- Integration in Table/ReadOnlyTable: ~50 lines
- Tests: ~100 lines
- Total: ~300 lines, medium complexity

The tricky part is handling the overlay (current Table handle's writes) + deferred ops (prior handles' writes) + memtable (committed prior txns) + B-tree (checkpointed data) — four levels of data that all need to be merged. For correctness, the merge order is: overlay > deferred_ops > memtable > B-tree.

### Performance Impact

Once deferred_flush is enabled with correct merge iterators:
- **Writes:** Only WAL append + memtable insert per commit (no B-tree page copies)
- **Reads:** Slightly slower due to merge overhead, but memtable is in-memory so the merge is fast
- **Expected write improvement:** 2-5x faster single inserts, bringing us close to SQLite WAL performance

### How to Verify

Enable deferred_flush in `crates/manifold-sql/src/lib.rs`:
```rust
.deferred_flush(true)
```

Then run the full test suite:
```bash
cargo test -p manifold-sql
```

All 230 tests should pass. Then benchmark:
```bash
cargo bench -p manifold-sql -- "single_insert|bulk_insert" 2>&1 | grep "time:"
```
