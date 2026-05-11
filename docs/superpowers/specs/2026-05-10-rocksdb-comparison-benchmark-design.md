# RocksDB Comparison Benchmark Design

## Goal

A single benchmark binary that runs 8 workloads at 5 escalating dataset sizes, printing tabular results that show ops/sec for both Manifold and RocksDB at each scale. The output reveals crossover points where one engine overtakes the other, guiding optimization priorities.

## Architecture

One new file: `crates/manifold-bench/benches/rocksdb_comparison.rs` with `harness = false`. Follows the existing bench crate conventions (tempfile for storage, `Instant` timing, table-formatted output). Does not use the `common.rs` trait abstraction — instead uses direct API calls for both engines to keep the benchmark self-contained and allow engine-specific tuning visibility.

Each workload is a standalone function that takes a scale parameter and returns `(manifold_ops_per_sec, rocksdb_ops_per_sec)`. The `main()` function iterates scales and workloads, collecting results into a summary table.

## Dataset Scales

5 points: 100K, 500K, 1M, 5M, 10M keys. Key size: 8 bytes (u64 big-endian). Value size: 100 bytes (fixed random payload seeded for reproducibility).

## Engine Configuration

**Manifold (ColumnFamilyDatabase with WAL):**
- Default builder with WAL enabled (production config)
- For CF workload: 8 column families

**RocksDB (OptimisticTransactionDB):**
- LRU block cache sized to match available memory (1GB default, configurable)
- Bloom filters (10 bits/key)
- Block-based table factory with cache_index_and_filter_blocks
- Parallelism set to available CPUs
- For CF workload: 8 native column families
- Default compaction (leveled) — no exotic tuning

Both engines get the same durability guarantees per workload (sync vs no-sync matched).

## Workloads

### 1. Sequential Write
Insert keys 0..N in ascending order, one transaction per batch of 1000. Non-durable (no fsync). Measures raw ingestion throughput. RocksDB's LSM append path should excel here.

### 2. Random Write
Insert N keys in random order (seeded RNG), batches of 1000, non-durable. Tests scattered write performance. LSM memtable absorbs randomness; B-tree COW has random page splits.

### 3. Point Read (Uniform)
Pre-populate N keys, then perform N random point lookups (uniform distribution). Measures steady-state read throughput. B-tree has O(log N) with direct page access; LSM has O(L) level checks with bloom filters.

### 4. Point Read (Zipfian)
Same as #3 but with Zipfian distribution (80% of reads hit 20% of keys). Tests cache effectiveness. Both engines should benefit from caching, but the relative advantage may differ.

### 5. Range Scan
Pre-populate N keys, perform 10K range scans of 100 consecutive keys from random start points. B-tree pages are physically sorted; LSM must merge across levels.

### 6. Mixed 50/50
Pre-populate N/2 keys. Then run 4 threads for 5 seconds: each thread randomly reads or writes (50/50 split) using Zipfian key distribution. Measures total ops/sec. Most realistic OLTP proxy.

### 7. Durable Write
Sequential write of N keys with durable commits (fsync). Batches of 100 with sync after each batch. Tests Manifold's WAL group commit vs RocksDB's WAL.

### 8. Concurrent CF Writers
8 threads writing to 8 independent column families, N/8 keys per CF, batches of 1000, non-durable. Tests Manifold's independent CF write locks vs RocksDB's column family implementation.

## Output Format

Per-workload table:
```
=== Sequential Write ===
  Scale  |  Manifold ops/s  |  RocksDB ops/s  |  Ratio (M/R)  |  Winner
  100K   |        425,000   |        890,000   |        0.48x  |  RocksDB
  500K   |        380,000   |        850,000   |        0.45x  |  RocksDB
  ...
```

Final summary table:
```
=== Summary: Where Each Engine Wins ===
  Workload              |  Best Scale  |  Manifold  |  RocksDB  |  Winner
  Sequential Write      |  all         |            |           |  RocksDB (2.1x avg)
  Random Write          |  all         |            |           |  RocksDB (1.3x avg)
  Point Read (Uniform)  |  all         |            |           |  Manifold (1.8x avg)
  ...
```

## What This Is Not

- Not a tuning exercise — both engines use recommended defaults
- Not a stress test — each workload runs to completion, not for sustained duration (except mixed workload)
- Not measuring tail latency — ops/sec only (latency histograms are a separate project)
- Not testing compression — both engines store uncompressed for fair comparison

## Test Plan

- Run `cargo bench --bench rocksdb_comparison` — should complete in ~10-15 minutes
- Verify both engines produce correct data (spot-check reads after writes)
- Run twice to check for variance — results should be within 10%
