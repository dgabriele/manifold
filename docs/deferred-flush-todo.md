# Deferred Flush: Status

## Completed

Deferred flush mode (`ColumnFamilyDatabase::builder().deferred_flush(true)`) is fully
functional. All 230 manifold-sql tests pass with deferred flush enabled.

### What was done

1. **Merge iterator for `range()`/`iter()`** (`src/table.rs`): `Range` now uses an enum
   inner (`BtreeOnly` vs `Merged`) that interleaves B-tree entries with in-memory
   memtable entries in correct key order using `K::compare()`. Handles tombstones,
   key deduplication (memtable wins), and range-bound filtering.

2. **Commit path fix** (`src/transactions.rs`): `commit_inner_deferred()` was renamed to
   `commit_deferred_ops()` and is now called *before* the normal B-tree commit rather
   than replacing it. This ensures table definitions and metadata are persisted to the
   B-tree while data goes to WAL + memtable.

3. **Savepoint support** (`src/transactions.rs`): `ephemeral_savepoint()` records the
   current deferred ops count; `restore_savepoint()` truncates back to that count.

4. **WAL recovery** (`src/column_family/database.rs`): `LogicalOps` entries are replayed
   into the shared memtable (not the B-tree) during recovery, avoiding type mismatches.

5. **`deferred_flush(true)` enabled in manifold-sql** (`crates/manifold-sql/src/lib.rs`).

### Known limitations

- `DoubleEndedIterator` on merged ranges works but only uses one btree `next_back()` call
  per invocation (no persistent back-peek buffer). Fine for `last()` but interleaved
  forward/backward iteration on merged ranges may skip btree entries. Not used by
  manifold-sql.
