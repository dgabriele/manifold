# Manifold

[![CI](https://github.com/cberner/redb/actions/workflows/ci.yml/badge.svg)](https://github.com/cberner/redb/actions)
[![License](https://img.shields.io/crates/l/redb)](https://crates.io/crates/redb)

**A high-performance, ACID-compliant embedded database with column families, write-ahead logging, and WASM support.**

Manifold is a fork of [redb](https://github.com/cberner/redb) by Christopher Berner, extended with:
- **Column families** for concurrent writes to independent databases in a single file
- **Write-ahead log (WAL)** for fast, durable commits
- **WASM support** via Origin Private File System (OPFS)
- **Production-ready crash recovery** with comprehensive error handling

Built on redb's solid foundation of copy-on-write B-trees, Manifold adds the architecture needed for high-concurrency workloads while maintaining full ACID guarantees.

---

## Quick Start

### Basic Usage

```rust
use manifold::{Database, ReadableTable, TableDefinition};

const TABLE: TableDefinition<&str, u64> = TableDefinition::new("my_data");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = Database::create("my_db.manifold")?;
    let write_txn = db.begin_write()?;
    {
        let mut table = write_txn.open_table(TABLE)?;
        table.insert("my_key", &123)?;
    }
    write_txn.commit()?;

    let read_txn = db.begin_read()?;
    let table = read_txn.open_table(TABLE)?;
    assert_eq!(table.get("my_key")?.unwrap().value(), 123);

    Ok(())
}
```

### Column Families Example

```rust
use manifold::column_family::ColumnFamilyDatabase;
use manifold::TableDefinition;

const USERS: TableDefinition<u64, &str> = TableDefinition::new("users");
const PRODUCTS: TableDefinition<u64, &str> = TableDefinition::new("products");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Single file, multiple independent databases
    let db = ColumnFamilyDatabase::builder().open("app.manifold")?;

    // Auto-create column families on first access
    let users_cf = db.column_family_or_create("users")?;
    let products_cf = db.column_family_or_create("products")?;

    // Concurrent writes to different column families
    std::thread::scope(|s| {
        s.spawn(|| {
            let txn = users_cf.begin_write()?;
            let mut table = txn.open_table(USERS)?;
            table.insert(&1, &"alice")?;
            txn.commit()
        });

        s.spawn(|| {
            let txn = products_cf.begin_write()?;
            let mut table = txn.open_table(PRODUCTS)?;
            table.insert(&100, &"laptop")?;
            txn.commit()
        });
    });

    Ok(())
}
```

---

## Features

### Core Features (from redb)
- ✅ **Zero-copy**, thread-safe `BTreeMap`-based API
- ✅ **Fully ACID-compliant** transactions with snapshot isolation
- ✅ **MVCC** support for concurrent readers and writers without blocking
- ✅ **Crash-safe by default** with automatic recovery
- ✅ **Savepoints and rollbacks** for transaction control
- ✅ **Stable file format** with upgrade paths

### Manifold Extensions
- 🚀 **Column Families**: Multiple independent databases in a single file with concurrent write transactions
- ⚡ **Write-Ahead Log (WAL)**: Fast durable commits (~0.5ms vs ~5ms) with group commit batching
- 🌐 **WASM Support**: Full database functionality in browsers via OPFS (Chrome 102+, Edge 102+)
- 🛡️ **Production Error Handling**: Comprehensive error messages, troubleshooting guides, and recovery procedures
- 📊 **Crash Recovery Testing**: Process-based crash injection tests validate WAL replay correctness

---

## Performance

### Manifold vs Vanilla redb 3.1.0

**Concurrent Writes** (the killer feature):

| Threads | Manifold      | redb 2.6     | Speedup |
|---------|---------------|--------------|---------|
| 2       | 189K ops/sec  | 75K ops/sec  | **2.52x** |
| 4       | 293K ops/sec  | 82K ops/sec  | **3.56x** |
| 8       | **426K ops/sec** | 88K ops/sec  | **4.80x** |

**Why?** Column families enable true parallel writes. Vanilla redb serializes all write transactions.

### WAL Performance

| Configuration | Throughput     | Speedup |
|---------------|----------------|---------|
| WITH WAL      | 273K ops/sec   | **1.64x** |
| WITHOUT WAL   | 166K ops/sec   | 1.00x   |

**Recovery:** ~326K entries/sec (20,000 entries in ~61ms)

### Read Performance

- **Read-heavy workload**: 6.74M ops/sec (8 concurrent readers)
- **Mixed workload (50/50 R/W)**: 4.56M ops/sec (16 threads)
- **Sequential reads**: 4.58M ops/sec

### Comparison vs Other Databases

|                           | manifold   | lmdb        | rocksdb        | sled       | fjall           | sqlite     |
|---------------------------|------------|-------------|----------------|------------|-----------------|------------|
| bulk load                 | 47912ms    | **10520ms** | 39093ms        | 41817ms    | 91075ms         | 55585ms    |
| individual writes         | **51ms**   | 15753ms     | 20816ms        | 11038ms    | 5269ms          | 6803ms     |
| batch writes              | 1782ms     | 6413ms      | 1355ms         | 2138ms     | **727ms**       | 90617ms    |
| nosync writes             | 2483ms     | 591141ms    | **295ms**      | 463ms      | 853ms           | 283065ms   |
| len()                     | **0ms**    | **0ms**     | 1413ms         | 3277ms     | 1525ms          | 66ms       |
| random reads              | 2653ms     | **1874ms**  | 4202ms         | 2090ms     | 4540ms          | 10131ms    |
| random reads              | 3097ms     | **1723ms**  | 4070ms         | 2185ms     | 4519ms          | 10382ms    |
| random range reads        | 2229ms     | **1056ms**  | 5000ms         | 3310ms     | 4148ms          | 19729ms    |
| random range reads        | 2051ms     | **1057ms**  | 5753ms         | 3450ms     | 4146ms          | 19330ms    |
| random reads (4 threads)  | 4319ms     | **2059ms**  | 9234ms         | 3280ms     | 5785ms          | 39302ms    |
| random reads (8 threads)  | 3432ms     | **1207ms**  | 9117ms         | 2126ms     | 3329ms          | 47882ms    |
| random reads (16 threads) | 2838ms     | **1093ms**  | 8066ms         | 2057ms     | 3321ms          | 66441ms    |
| random reads (32 threads) | 2866ms     | **1128ms**  | 8173ms         | 2077ms     | 3471ms          | 78306ms    |
| removals                  | 53881ms    | **7349ms**  | 20050ms        | 15326ms    | 8049ms          | 56281ms    |
| uncompacted size          | 4.00 GiB   | 2.59 GiB    | 1016.07 MiB    | 2.14 GiB   | **1010.61 MiB** | 1.10 GiB   |
| compacted size            | N/A        | 1.26 GiB    | **459.17 MiB** | N/A        | 1010.61 MiB     | 562.31 MiB |

Source code for benchmark: [lmdb_benchmark.rs](./crates/manifold-bench/benches/lmdb_benchmark.rs). Results collected on macOS.

**Column Family Concurrent Write Benchmark:**

For a more realistic comparison showcasing column family performance with concurrent writers, see the [cf_comparison_benchmark.rs](./crates/manifold-bench/benches/cf_comparison_benchmark.rs) which compares Manifold, RocksDB (with column families), and Fjall (with partitions) under concurrent write workloads.

> **Note on Benchmarks**: These benchmarks test single-database performance. Manifold's primary advantage is **concurrent writes via column families** (see 4.8x speedup vs vanilla redb above). RocksDB and Fjall also support column families/partitions. For column-family-optimized benchmarks, run `cargo run --release --bin cf_comparison_benchmark`.

### Deferred Flush (Write Optimization)

Manifold offers an opt-in **deferred flush** mode that eliminates B-tree mutation from the commit path. When enabled, writes go to the WAL and an in-memory memtable — the B-tree is updated later by a background checkpoint. This matches the write architecture used by RocksDB and other LSM-based engines.

```rust
let db = ColumnFamilyDatabase::builder()
    .deferred_flush(true)  // opt-in, default is false
    .open("my_db.manifold")?;
```

**How it works:**
- `insert()` / `remove()` append to the WAL and merge into a shared in-memory memtable — no B-tree pages are read, copied, or written
- `commit()` cost is one sequential WAL write + one in-memory merge
- `get()` checks the memtable first, then falls through to the B-tree (read-your-own-writes)
- A background checkpoint periodically drains the memtable into the B-tree in sorted key order

**Manifold vs RocksDB** (8-byte keys, 100-byte values, 1000-key batches):

| Workload | Scale | Manifold | RocksDB | Ratio |
|----------|-------|----------|---------|-------|
| **Sequential Write** | 100K | 214K ops/s | 687K ops/s | 0.31x |
| | 1M | 185K ops/s | 646K ops/s | 0.29x |
| | 2M | 199K ops/s | 653K ops/s | 0.30x |
| **Random Write** | 100K | 197K ops/s | 354K ops/s | 0.56x |
| | 1M | 163K ops/s | 277K ops/s | 0.59x |
| | 2M | 164K ops/s | 269K ops/s | 0.61x |
| **Point Read** | 100K | 2.91M ops/s | 1.02M ops/s | **2.86x** |
| | 1M | 1.83M ops/s | 507K ops/s | **3.62x** |
| **Range Scan** | 100K | (B-tree advantage) | | |

**Key takeaways:**
- RocksDB's LSM tree wins on raw write throughput (~3x sequential, ~1.7x random) due to its append-only architecture
- Manifold wins on reads (**2-3.6x faster** point lookups up to 5M keys) thanks to B-tree's single root-to-leaf traversal vs RocksDB's multi-level search
- With deferred flush, Manifold's random write gap is **stable across scale** (~0.6x) instead of degrading (was 0.09x at 10M without deferred flush)
- Manifold's column family isolation provides additional write scaling not shown in single-table benchmarks

Source: [rocksdb_comparison.rs](./crates/manifold-bench/benches/rocksdb_comparison.rs)

---

## Column Families Architecture

### What Are Column Families?

Column families allow multiple independent redb databases to coexist in a single physical file. Each column family:
- Has its own B-tree root and transaction isolation
- Supports independent write transactions (no cross-CF locking)
- Maintains ACID guarantees within the column family
- Shares the same file with other column families

### When to Use Column Families

✅ **Good fit:**
- Multi-tenant applications (one CF per tenant)
- Domain separation (users, products, orders as separate CFs)
- Concurrent write workloads where different threads write to different data domains
- Scenarios where you'd otherwise use multiple database files

❌ **Not needed:**
- Single-threaded write workloads
- Cross-domain transactions (use tables within a single CF instead)
- Simple key-value storage (vanilla redb is simpler)

### Architecture Details

```
┌─────────────────────────────────────────┐
│         app.manifold (single file)      │
├─────────────────────────────────────────┤
│  Master Header (CRC-protected)          │
├─────────────────────────────────────────┤
│  Column Family "users"    (1GB)         │
│  - Independent redb instance            │
│  - Own transaction isolation            │
├─────────────────────────────────────────┤
│  Column Family "products" (512MB)       │
│  - Concurrent writes with "users"       │
│  - Own transaction isolation            │
├─────────────────────────────────────────┤
│  Column Family "orders"   (2GB)         │
│  - Independent write transactions       │
└─────────────────────────────────────────┘
```

See [docs/design.md](docs/design.md) for implementation details.

---

## Write-Ahead Log (WAL)

### Why WAL?

Traditional databases fsync the entire B-tree on every commit (~5ms). WAL appends commit records to a sequential log and fsyncs only that (~0.5ms), giving **~10x faster commits**.

### Performance Characteristics

- **Commit latency**: ~0.5ms (WAL) vs ~5ms (no WAL)
- **Throughput**: 250K ops/sec @ 8 threads with WAL enabled
- **Group commit**: Batches concurrent commits into single fsync
- **Recovery**: Automatic on database open (~300K entries/sec)

### Durability Modes

```rust
use manifold::Durability;

let txn = db.begin_write()?;
// ...

// Default: fsync on commit (data survives crashes)
txn.set_durability(Durability::Immediate)?;
txn.commit()?;

// Fast path: no fsync (data may be lost on crash)
txn.set_durability(Durability::None)?;
txn.commit()?;
```

### Recovery Guarantees

- ✅ Committed transactions with `Durability::Immediate`: **Survive crashes**
- ⚠️ Committed transactions with `Durability::None`: **May be lost** (depends on checkpoint)
- ❌ Uncommitted transactions: **Always lost** (expected ACID behavior)

See [docs/recovery_guarantees.md](docs/recovery_guarantees.md) for detailed recovery semantics.

---

## WASM Support

Manifold runs in browsers using the Origin Private File System (OPFS):

```javascript
// worker.js
import init, { WasmDatabase } from './manifold.js';

await init();
const db = await WasmDatabase.new("app.db");
const cf = db.column_family_or_create("users");

// Write data
cf.write("user_1", "alice");

// Read data
const value = cf.read("user_1");
console.log(value); // "alice"
```

**Requirements:**
- Modern browser (Chrome 102+, Edge 102+)
- Web Worker context (OPFS sync access requirement)
- HTTPS or localhost (secure context)

**Performance:** Same as native (OPFS provides synchronous file access in workers)

See [examples/wasm/](examples/wasm/) for complete examples.

---

## Documentation

### Guides
- [Design Document](docs/design.md) - Architecture and implementation details
- [WAL Design](docs/wal_design.md) - Write-ahead log implementation
- [Recovery Guarantees](docs/recovery_guarantees.md) - Crash recovery and durability semantics
- [Troubleshooting Guide](TROUBLESHOOTING.md) - Common errors and solutions

### Examples
- [Column Families](examples/column_families.rs) - Multi-tenant and domain separation
- [Error Handling](examples/error_handling.rs) - Production error handling patterns
- [WAL Comparison](examples/wal_comparison.rs) - WAL vs no-WAL performance
- [WASM Usage](examples/wasm/) - Browser database with OPFS

### API Documentation
- Coming soon: docs.rs link once published

---

## Error Handling

Manifold provides comprehensive error handling with clear messages:

```rust
use manifold::column_family::ColumnFamilyDatabase;

match ColumnFamilyDatabase::open("data.manifold") {
    Ok(db) => { /* use database */ }
    Err(e) => {
        eprintln!("Database error: {}", e);
        // Error includes context: corruption details, I/O errors, etc.
        // See TROUBLESHOOTING.md for recovery procedures
    }
}
```

**Error categories:**
- Storage errors (corruption, I/O, disk full)
- Table errors (type mismatches, missing tables)
- Transaction errors (conflicts, isolation violations)
- Column family errors (not found, already exists)

See [TROUBLESHOOTING.md](TROUBLESHOOTING.md) for common issues and solutions.

---

## Testing

Manifold includes comprehensive test coverage:

- **98 unit tests**: Core functionality
- **9 crash recovery tests**: Process-based crash injection validates WAL replay
- **Multiple benchmark suites**: Performance validation

**Run tests:**
```bash
cargo test --lib                      # Unit tests
cargo test --test crash_recovery_tests # Crash injection tests (Unix/macOS)
```

**Run benchmarks:**
```bash
cargo run --release --bin lmdb_benchmark           # vs other databases
cargo run --release --bin redb_comparison_benchmark # vs vanilla redb
cargo bench                                         # criterion benchmarks
```

---

## Production Readiness

Manifold is production-ready with:

- ✅ **Stable file format** (inherited from redb)
- ✅ **Comprehensive error handling** (Phase 2 complete)
- ✅ **Crash recovery validation** (process-based crash tests)
- ✅ **Clear documentation** (guides, examples, troubleshooting)
- ✅ **Performance benchmarks** (validated against redb and competitors)
- ✅ **WASM support** (full functionality in browsers)

**Known limitations:**
- Column families are auto-created; manual sizing planned for v3.2
- WAL checkpointing is time/size-based; manual checkpoint API planned
- No replication or distributed features (single-process database)

See [FINALIZATION_PLAN.md](FINALIZATION_PLAN.md) for roadmap.

---

## Credits

Manifold is a fork of [redb](https://github.com/cberner/redb) by Christopher Berner.

**Original redb features:**
- Copy-on-write B-tree implementation
- ACID transactions with MVCC
- Zero-copy reads
- Savepoints and recovery

**Manifold additions:**
- Column family architecture by Tom (Hyperspatial)
- Write-ahead log implementation
- WASM/OPFS backend
- Production error handling and crash recovery

We're deeply grateful to Christopher Berner for creating redb and maintaining it as a high-quality, production-ready embedded database. Manifold builds on that solid foundation.

---

## License

Licensed under either of:

* [Apache License, Version 2.0](LICENSE-APACHE)
* [MIT License](LICENSE-MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

---

## Development

### Building from Source

```bash
git clone https://github.com/hyperbasedev/manifold
cd manifold
cargo build --release
```

### Running Tests

```bash
cargo test --all-targets
cargo clippy --all-targets
```

### Extra Development Dependencies

For benchmarking and fuzzing:
```bash
cargo install cargo-deny --locked
cargo install cargo-fuzz --locked
```

For WASM development:
```bash
cargo install wasm-pack
```

---

**Status:** Stable and actively maintained. Phase 2 (Production Error Handling) complete. Phase 3 (API Polish) in progress.
