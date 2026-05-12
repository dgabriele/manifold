# Manifold

[![CI](https://github.com/cberner/redb/actions/workflows/ci.yml/badge.svg)](https://github.com/cberner/redb/actions)
[![License](https://img.shields.io/crates/l/redb)](https://crates.io/crates/redb)

**A high-performance, ACID-compliant embedded database with column families, write-ahead logging, SQL, and WASM support.**

Manifold is a fork of [redb](https://github.com/cberner/redb) v3.1.1 by Christopher Berner, extended with production features for high-concurrency workloads.

---

## Table of Contents

- [Quick Start](#quick-start)
- [Manifold SQL](#manifold-sql)
- [Additions Over redb](#additions-over-redb)
- [Column Families](#column-families)
- [Write-Ahead Log (WAL)](#write-ahead-log-wal)
- [Deferred Flush](#deferred-flush)
- [Performance](#performance)
- [WASM Support](#wasm-support)
- [Documentation](#documentation)
- [Development](#development)
- [Credits](#credits)
- [License](#license)

---

## Quick Start

### Key-Value API

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

### Column Families

```rust
use manifold::column_family::ColumnFamilyDatabase;
use manifold::TableDefinition;

const USERS: TableDefinition<u64, &str> = TableDefinition::new("users");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = ColumnFamilyDatabase::builder().open("app.manifold")?;
    let users_cf = db.column_family_or_create("users")?;

    let txn = users_cf.begin_write()?;
    let mut table = txn.open_table(USERS)?;
    table.insert(&1, &"alice")?;
    drop(table);
    txn.commit()?;
    Ok(())
}
```

### SQL

```rust
use manifold_sql::Database;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = Database::open("app.db")?;

    db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)", &[])?;
    db.execute("INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30)", &[])?;

    let result = db.query("SELECT name, age FROM users WHERE id = 1", &[])?;
    println!("{}: {}", result.rows()[0].get::<String>(0)?, result.rows()[0].get::<i64>(1)?);
    Ok(())
}
```

---

## Manifold SQL

`manifold-sql` is an embedded SQL engine built on Manifold's column family backend. It provides a familiar SQL interface over the high-performance B-tree storage.

**Crate:** [`crates/manifold-sql`](crates/manifold-sql)

### Supported SQL

| Category | Features |
|----------|----------|
| **DDL** | CREATE TABLE, DROP TABLE, ALTER TABLE, CREATE INDEX, DROP INDEX |
| **DML** | INSERT, UPDATE, DELETE, SELECT |
| **Queries** | WHERE, ORDER BY, GROUP BY, HAVING, LIMIT/OFFSET, DISTINCT |
| **Joins** | INNER, LEFT, RIGHT, CROSS (hash join for equi-joins) |
| **Aggregates** | COUNT, SUM, AVG, MIN, MAX |
| **Subqueries** | IN, EXISTS, derived tables, scalar subqueries |
| **Transactions** | BEGIN, COMMIT, ROLLBACK, SAVEPOINT |
| **Types** | INTEGER, BIGINT, SMALLINT, REAL, TEXT, VARCHAR(n), BOOLEAN, DATE, TIMESTAMP, UUID, JSON, BLOB, DECIMAL |
| **Other** | Prepared statements, EXPLAIN/ANALYZE, UNION/UNION ALL |

### Optimizer

The query optimizer includes:
- **Rowid direct lookup**: `WHERE pk = $1` does O(1) B-tree key access
- **Rowid range scan**: `WHERE pk BETWEEN $1 AND $2` uses B-tree range iteration
- **Index selection**: Secondary indexes for non-PK equality predicates
- **Hash join**: O(n+m) for equi-join conditions (vs O(n*m) nested loop)
- **Statement cache**: Repeated queries skip parse/bind/plan/optimize
- **Predicate pushdown**, constant folding, join reordering

### Performance vs SQLite

Point lookups and range scans are near-parity. Full scans are ~2-3x SQLite due to row materialization overhead (streaming iteration planned).

| Benchmark | Manifold | SQLite | Ratio |
|-----------|----------|--------|-------|
| point_lookup (10K rows) | 2.8 us | 1.7 us | 1.6x |
| range_scan (100 rows from 10K) | 80 us | 35 us | 2.3x |
| full_scan (1K rows) | 739 us | 316 us | 2.3x |
| group_by (10K rows) | 6.9 ms | 2.4 ms | 2.8x |
| single_insert (auto-commit) | 73 us | 8 us | 9x |
| bulk_insert (1K rows/txn) | 9.5 ms | 725 us | 13x |
| update_by_pk | 73 us | 2.4 us | 30x |
| delete_by_pk | 151 us | 15 us | 10x |

Reads are near-parity (1.5-2.8x). Writes are 9-30x due to copy-on-write B-tree page mutations + WAL per auto-commit. Bulk writes in explicit transactions amortize this significantly.

Source: [`crates/manifold-sql/benches/sqlite_comparison.rs`](crates/manifold-sql/benches/sqlite_comparison.rs)

---

## Additions Over redb

Manifold is forked from redb v3.1.1. Here is everything we've added:

### Column Family Architecture
Independent databases within a single file, each with its own B-tree root, transaction isolation, and write lock. Enables true concurrent writes across domains (users, products, orders). 4.8x write throughput at 8 threads vs vanilla redb's serial writes.

### Write-Ahead Log (WAL)
Sequential log for fast durable commits (~0.5ms vs ~5ms without). Group commit batches concurrent fsyncs. Automatic crash recovery on open (~300K entries/sec replay).

### Deferred Flush Memtable
Opt-in mode where writes skip B-tree mutation entirely, going to WAL + in-memory memtable. Background checkpoint drains to B-tree. Closes the random write gap with RocksDB from 0.09x to 0.6x.

### Merge Iterator
`Table::range()` and `ReadOnlyTable::range()` merge B-tree entries with memtable entries in correct key order using `K::compare()`. Handles tombstones, key deduplication, and range-bound filtering. Enables deferred flush mode for read-heavy workloads.

### SQL Engine (`manifold-sql`)
Full SQL engine with parser, binder, planner, optimizer, and executor. 230+ tests. Prepared statements, subqueries, transactions, constraints, indexes. See [Manifold SQL](#manifold-sql) above.

### Domain-Specific Crates
- **`manifold-graph`** — Graph database built on Manifold (nodes, edges, traversals)
- **`manifold-timeseries`** — Time-series storage with downsampling and retention policies
- **`manifold-vectors`** — Vector similarity search (cosine, euclidean, dot product)

### Upstream Sync
Merged redb v3.1.1 through `upstream/master` (89 commits) including 14 bug fixes, 13 perf improvements, `Table::entry()` API, `PageResolver` abstraction, and `retain`/`extract_if` optimization.

### Other Improvements
- WASM/OPFS backend for browser support
- Production error handling with comprehensive messages and recovery procedures
- Process-based crash recovery tests
- Savepoint bug fix (`restore_savepoint()` clearing `pending_table_updates`)
- Write buffer (`insert_buffered`/`remove_buffered`) for batch operations

---

## Column Families

Column families allow multiple independent databases to coexist in a single file. Each column family has its own B-tree root and write lock, enabling concurrent writes without cross-domain contention.

```
+----------------------------------------------+
|          app.manifold (single file)          |
+----------------------------------------------+
|  Master Header (CRC-protected)               |
+----------------------------------------------+
|  Column Family "users"    (1GB segment)      |
|  - Own B-tree, own write lock                |
+----------------------------------------------+
|  Column Family "products" (512MB segment)    |
|  - Concurrent writes with "users"            |
+----------------------------------------------+
|  Column Family "orders"   (2GB segment)      |
|  - Independent transaction isolation         |
+----------------------------------------------+
```

See [docs/design.md](docs/design.md) for implementation details.

---

## Write-Ahead Log (WAL)

WAL appends commit records sequentially and fsyncs once, instead of fsyncing the entire B-tree. Group commit batches concurrent transactions into a single fsync.

| Configuration | Commit Latency | Throughput |
|---------------|----------------|------------|
| With WAL | ~0.5ms | 273K ops/sec |
| Without WAL | ~5ms | 166K ops/sec |
| Recovery | | ~326K entries/sec |

See [docs/wal_design.md](docs/wal_design.md) and [docs/recovery_guarantees.md](docs/recovery_guarantees.md).

---

## Deferred Flush

Opt-in mode where writes skip B-tree mutation, going to WAL + in-memory memtable. A background checkpoint drains the memtable to the B-tree in sorted order.

```rust
let db = ColumnFamilyDatabase::builder()
    .deferred_flush(true)
    .open("my_db.manifold")?;
```

**Manifold vs RocksDB** (deferred flush enabled, 2M keys):

| Workload | Manifold | RocksDB | Ratio |
|----------|----------|---------|-------|
| Sequential Write | 199K ops/s | 653K ops/s | 0.30x |
| Random Write | 164K ops/s | 269K ops/s | 0.61x |
| Point Read | 1.83M ops/s | 507K ops/s | **3.62x** |

Source: [rocksdb_comparison.rs](./crates/manifold-bench/benches/rocksdb_comparison.rs)

---

## Performance

### Manifold vs Vanilla redb

**Concurrent Writes** (the killer feature):

| Threads | Manifold | redb | Speedup |
|---------|----------|------|---------|
| 2 | 189K ops/sec | 75K ops/sec | **2.52x** |
| 4 | 293K ops/sec | 82K ops/sec | **3.56x** |
| 8 | **426K ops/sec** | 88K ops/sec | **4.80x** |

### Manifold vs Other Databases

|                           | manifold   | lmdb        | rocksdb        | sled       | fjall           | sqlite     |
|---------------------------|------------|-------------|----------------|------------|-----------------|------------|
| bulk load                 | 47912ms    | **10520ms** | 39093ms        | 41817ms    | 91075ms         | 55585ms    |
| individual writes         | **51ms**   | 15753ms     | 20816ms        | 11038ms    | 5269ms          | 6803ms     |
| batch writes              | 1782ms     | 6413ms      | 1355ms         | 2138ms     | **727ms**       | 90617ms    |
| random reads              | 2653ms     | **1874ms**  | 4202ms         | 2090ms     | 4540ms          | 10131ms    |
| random range reads        | 2229ms     | **1056ms**  | 5000ms         | 3310ms     | 4148ms          | 19729ms    |

Source: [lmdb_benchmark.rs](./crates/manifold-bench/benches/lmdb_benchmark.rs)

---

## WASM Support

Manifold runs in browsers using the Origin Private File System (OPFS):

```javascript
import init, { WasmDatabase } from './manifold.js';
await init();
const db = await WasmDatabase.new("app.db");
const cf = db.column_family_or_create("users");
cf.write("user_1", "alice");
```

Requirements: Chrome 102+ / Edge 102+, Web Worker context, HTTPS.

---

## Documentation

- [Design Document](docs/design.md) — Architecture and implementation details
- [WAL Design](docs/wal_design.md) — Write-ahead log implementation
- [Recovery Guarantees](docs/recovery_guarantees.md) — Crash recovery and durability semantics
- [Troubleshooting](docs/TROUBLESHOOTING.md) — Common errors and solutions
- [SpinStack Integration](docs/spinstack-integration-guide.md) — Using Manifold in SpinStack

---

## Development

```bash
cargo build --release
just test                    # clippy + fmt + deny + full test suite
just test_all                # includes workspace crates
cargo bench -p manifold-sql  # SQL benchmarks
```

See [AGENTS.md](AGENTS.md) for detailed development setup and conventions.

---

## Credits

Manifold is a fork of [redb](https://github.com/cberner/redb) by Christopher Berner. We're deeply grateful for his work creating and maintaining redb as a high-quality, production-ready embedded database.

**Original redb foundation:** Copy-on-write B-tree, ACID transactions with MVCC, zero-copy reads, savepoints and recovery, stable file format.

---

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
