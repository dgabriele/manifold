# Write Buffer (Deferred B-tree Mutation) Design

## Goal

Improve random write throughput at scale by deferring B-tree mutations. Instead of mutating COW B-tree pages on every `insert()` call (causing random page I/O), buffer writes in an in-memory `BTreeMap` overlay and flush to the B-tree in sorted key order when the table handle is dropped. Sorted insertion dramatically reduces page I/O because leaf pages are accessed sequentially.

## Status: Implemented (Phase 1)

Phase 1 (per-batch overlay with sorted flush) is implemented. Benchmark results show that batch-level sorting alone does not close the gap with RocksDB at large scale — the fundamental bottleneck is the COW B-tree's page I/O pattern, not insertion order within a 1000-key batch. Phase 2 (cross-batch memtable with WAL-only commit) would be needed to match RocksDB's write throughput.

## Architecture

The `Table` struct gains an in-memory `BTreeMap<Vec<u8>, Option<Vec<u8>>>` overlay that intercepts `insert_buffered()` / `remove_buffered()` calls. The overlay is flushed to the B-tree in sorted key order when the table handle is dropped.

### API

- `insert_buffered(key, value) -> Result` — buffers write, does not return old value
- `remove_buffered(key) -> Result` — buffers tombstone deletion
- `get(key)` — checks overlay first (read-your-own-writes), falls back to B-tree
- `len()` — exact via delta counter tracking new-vs-update on each mutation
- `flush_overlay()` — manually drain overlay into B-tree in sorted order
- Complex mutations (`get_mut`, `entry`, `pop_*`, `retain`, `extract_if`) auto-flush before executing
- `range()`/`iter()` require manual `flush_overlay()` call to see buffered writes

### What doesn't change

- `insert()` / `remove()` — unchanged, direct B-tree mutation with old-value return
- `ReadTransaction` / `ReadOnlyTable` — completely untouched
- WAL, savepoints, column families, commit path, file format — all unchanged

## Benchmark Results: Manifold vs RocksDB

8-byte keys, 100-byte values, 1000-key batches, 1GB RocksDB cache.

### Sequential Write (RocksDB wins ~4x, stable across scale)
```
  Scale  |  Manifold ops/s  |  RocksDB ops/s  |  Ratio  |  Winner
  100K   |          148.7K  |          614.1K  |  0.24x  |  RocksDB
  500K   |          147.3K  |          647.6K  |  0.23x  |  RocksDB
  1M     |          134.8K  |          636.2K  |  0.21x  |  RocksDB
  5M     |          148.3K  |          665.9K  |  0.22x  |  RocksDB
  10M    |          160.1K  |          656.7K  |  0.24x  |  RocksDB
```

**Analysis:** RocksDB's LSM tree converts all writes to sequential WAL + memtable appends. Manifold's COW B-tree must update pages in-place. The ~4x gap is structural — LSM append is fundamentally faster than B-tree page splits. The gap is stable across scale because sequential B-tree insertion is already cache-friendly.

### Random Write (RocksDB wins, gap widens with scale)
```
  Scale  |  Manifold ops/s  |  RocksDB ops/s  |  Ratio  |  Winner
  100K   |          109.6K  |          353.7K  |  0.31x  |  RocksDB
  500K   |           81.2K  |          263.8K  |  0.31x  |  RocksDB
  1M     |           70.0K  |          257.6K  |  0.27x  |  RocksDB
  5M     |           43.2K  |          262.6K  |  0.16x  |  RocksDB
  10M    |           23.9K  |          280.4K  |  0.09x  |  RocksDB
```

**Analysis:** This is the worst case for COW B-trees. Random keys cause random page reads (cache misses at scale) and random COW copies. RocksDB's memtable absorbs randomness entirely — all writes go to a sorted in-memory buffer regardless of key order. The gap widens from 3x at 100K to 11x at 10M because Manifold's working set exceeds the page cache.

**Why the write buffer didn't help:** The overlay sorts keys within each 1000-key batch, but at 10M keys the B-tree has ~5 levels. Even sorted batches of 1000 keys span many leaf pages. The fundamental issue is that the B-tree is fully updated on every commit — to match RocksDB, writes would need to be deferred across commits (Phase 2 memtable).

### Point Read — Uniform (Manifold wins up to 5M, RocksDB wins at 10M)
```
  Scale  |  Manifold ops/s  |  RocksDB ops/s  |  Ratio  |  Winner
  100K   |            2.91M |            1.02M |  2.86x  |  Manifold
  500K   |            1.99M |           952.3K |  2.09x  |  Manifold
  1M     |            1.83M |           506.5K |  3.62x  |  Manifold
  5M     |           547.2K |           256.0K |  2.14x  |  Manifold
  10M    |            21.0K |            96.4K |  0.22x  |  RocksDB
```

**Analysis:** B-tree point reads are O(log N) with a single root-to-leaf traversal. RocksDB must check multiple SST levels with bloom filter lookups. Manifold wins 2-3.6x up to 5M keys. At 10M keys, the B-tree's working set exceeds the 1GB cache, causing disk I/O on every lookup, while RocksDB's bloom filters and block cache remain effective.

**Crossover point:** ~5-10M keys with a 1GB cache. Increasing Manifold's cache size would push this higher.

## Phase 2: Future Work (Cross-Batch Memtable)

To close the write gap, Manifold would need a WAL-backed memtable that persists across commits:

1. `insert()` → append to in-memory sorted buffer + WAL (sequential, fast)
2. `commit()` → WAL fsync only (no B-tree mutation)
3. `read()` → merge memtable + B-tree
4. Background checkpoint → flush memtable to B-tree in sorted order

This is the architecture used by SQLite WAL mode, WiredTiger, and RocksDB. It would require changing the read path to merge two sources, adding a background flush thread, and ensuring crash recovery replays the WAL into the memtable.
