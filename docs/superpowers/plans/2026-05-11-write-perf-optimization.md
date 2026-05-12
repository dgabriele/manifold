# manifold-sql Write Performance Optimization Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce the write-side performance gap vs SQLite from 9-30x to 3-5x on single-row auto-commit INSERT, UPDATE, DELETE, and bulk INSERT benchmarks.

**Architecture:** The write path has four major cost centers: (1) per-query SQL overhead, (2) redundant table opens, (3) CoW B-tree page copies cascading through indexes, and (4) WAL fsync latency. We attack each with targeted optimizations that don't require rearchitecting the storage engine.

**Tech Stack:** Rust, manifold-sql executor, manifold B-tree + WAL.

**Current baseline (auto-commit, single row, PK table with PK index):**

| Operation | Manifold | SQLite | Ratio |
|-----------|----------|--------|-------|
| INSERT    | 73 us    | 8 us   | 9x    |
| UPDATE    | 73 us    | 2.4 us | 30x   |
| DELETE    | 151 us   | 15 us  | 10x   |
| BULK INSERT (1K) | 9.5 ms | 725 us | 13x |

**Cost breakdown for auto-commit INSERT (1 row, 1 PK index):**

| Phase | Estimated Cost |
|-------|---------------|
| Statement cache lookup + plan clone | ~0.3 us |
| begin_write() | ~1-2 us |
| open data table | ~2-5 us |
| open PK index table | ~2-5 us |
| PK duplicate check (B-tree GET on data table) | ~1-3 us |
| Index uniqueness check (B-tree GET on index) | ~1-3 us |
| Overlay insert (BTreeMap) | ~0.1 us |
| flush_overlay (CoW B-tree insert) | ~5-10 us |
| Index insert (CoW B-tree insert) | ~5-10 us |
| flush_and_close (table tree metadata update) | ~3-5 us |
| WAL append + wait_for_sync | ~20-40 us |
| non_durable_commit (page accounting) | ~5-10 us |
| **Total** | **~45-90 us** |

The biggest single costs are WAL fsync (~20-40us), CoW B-tree mutations (~10-20us for data + index), and table open overhead (~5-10us).

---

## File Structure

| File | Change | Responsibility |
|------|--------|---------------|
| `crates/manifold-sql/src/lib.rs` | Modify | Table handle caching per transaction |
| `crates/manifold-sql/src/executor/mod.rs` | Modify | Reuse table handles, skip redundant PK index for PK=rowid tables |
| `src/transactions.rs` | Modify | Reduce commit overhead for small transactions |

---

### Task 1: Skip PK index for INTEGER PRIMARY KEY tables

**Impact: ~15-20us savings per write (eliminate PK index open, GET, INSERT)**

When column 0 is an INTEGER PRIMARY KEY and PK=rowid, the PK index is redundant — the data table IS the index. Currently every INSERT does:
1. Open PK index table
2. Check uniqueness via `idx_table.get(key_bytes)`
3. Insert into PK index via `idx_table.insert(key_bytes, rowid)`

All redundant because `data_table.get(rowid)` already serves as the uniqueness check (done in our PK duplicate check), and the data table key IS the rowid.

**Files:**
- Modify: `crates/manifold-sql/src/executor/mod.rs`

- [ ] **Step 1: Skip PK index in update_indexes_insert**

In `update_indexes_insert()` (around line 2283), skip indexes where the index column is column 0 AND the table has PK=rowid. The simplest approach: add a `skip_pk_index: bool` parameter.

In `execute_insert_inner()`, determine whether column 0 is an INTEGER PRIMARY KEY:
```rust
let has_pk_rowid = !schema.columns.is_empty()
    && schema.columns[0].is_primary_key
    && matches!(schema.columns[0].sql_type, SqlType::Integer | SqlType::BigInt);
```

Pass this info to `update_indexes_insert` and `update_indexes_delete`. When `has_pk_rowid` is true and an index has `columns == [0]`, skip it entirely.

- [ ] **Step 2: Don't create PK index for PK=rowid tables**

In `execute_create_table()` (around line 1094), when the first column is INTEGER PRIMARY KEY, skip creating the PK index. The data table's B-tree key already serves as the index.

Check: does the index creation code look like this?
```rust
if col.is_primary_key && matches!(col.sql_type, SqlType::Integer | SqlType::BigInt) {
    // Create PK index
}
```

If so, skip the index creation for column 0 INTEGER PRIMARY KEY. Non-integer PKs still need the secondary index.

- [ ] **Step 3: Test**

Run: `cargo test -p manifold-sql`
Expected: All 230 tests pass. The PK uniqueness is enforced by the data table duplicate check, not the PK index.

- [ ] **Step 4: Commit**

```bash
git add crates/manifold-sql/src/executor/mod.rs
git commit -m "perf(manifold-sql): skip redundant PK index for INTEGER PRIMARY KEY tables"
```

---

### Task 2: Cache table handles within a transaction

**Impact: ~5-10us savings per write (avoid repeated open_table)**

Each INSERT/UPDATE/DELETE call in `execute_insert_inner` does `txn.open_table(def)` which traverses the internal table tree to find the table root. For auto-commit single-row operations, this is a significant fixed cost.

For explicit transactions (BEGIN...COMMIT) with multiple statements hitting the same table, this cost is multiplied.

**Files:**
- Modify: `crates/manifold-sql/src/executor/mod.rs`

- [ ] **Step 1: Skip table re-open in RowidUpdate and RowidDelete**

The `execute_rowid_update` and `execute_rowid_delete` functions each call `txn.open_table(def)` to get the data table. When called in a loop (UPDATE multiple rows), this re-opens on every call.

This is hard to fix generically without a table handle cache (which would require lifetime changes). But for the specific case of auto-commit single-row operations, the overhead is already minimal (one open per statement).

For bulk operations within explicit transactions, the existing `execute_in_txn_buffered` path already opens the table once and inserts multiple rows.

**Skip this task** — the table open cost (~2-5us) is not the dominant bottleneck. The real wins are in Tasks 1 and 3.

---

### Task 3: Batch WAL writes for explicit transactions

**Impact: ~30-40us savings per statement in explicit transactions**

In an explicit transaction (BEGIN...COMMIT), each DML statement currently goes through the full commit path independently. The WAL fsync happens once at COMMIT, but the WAL *append* still happens per statement via `flush_overlay()` → `commit_inner()` → WAL write.

Wait — actually, explicit transactions already batch: `execute_in_txn_buffered` defers flush to COMMIT time. The WAL write only happens once at `write_txn.commit()`. So explicit transactions are already optimized.

The real problem is **auto-commit**: each single-row INSERT gets its own `begin_write()` + `commit()` cycle with one WAL fsync.

**For auto-commit optimization:** The WAL fsync cost (~20-40us) is inherent to durable commits. SQLite batches this via its WAL's `synchronous=NORMAL` mode (fsync only on checkpoint, not every commit). Manifold already supports `Durability::None` but manifold-sql always uses `Durability::Immediate`.

**Files:**
- Modify: `crates/manifold-sql/src/lib.rs`

- [ ] **Step 1: Add a durability option to Database**

Add a builder pattern or configuration option to set auto-commit durability:

```rust
impl Database {
    /// Set the durability mode for auto-commit statements.
    /// `Durability::Immediate` (default): fsync on every commit.
    /// `Durability::None`: no fsync (faster, data may be lost on crash).
    pub fn set_durability(&self, durability: manifold::Durability) {
        *self.auto_commit_durability.lock().unwrap() = durability;
    }
}
```

Add field to Database struct:
```rust
auto_commit_durability: Mutex<manifold::Durability>,
```

In the auto-commit path, apply the durability setting:
```rust
let write_txn = self.cf.begin_write()?;
write_txn.set_durability(*self.auto_commit_durability.lock().unwrap())?;
let result = executor::execute_in_txn(&write_txn, &mut catalog, optimized, params)?;
write_txn.commit()?;
```

- [ ] **Step 2: Use Durability::None in benchmarks**

In the benchmark, after creating the manifold DB, call:
```rust
mdb.set_durability(manifold::Durability::None);
```

This matches SQLite's `PRAGMA synchronous=NORMAL` which also doesn't fsync every commit.

- [ ] **Step 3: Test**

Run: `cargo test -p manifold-sql`
Expected: All pass (default durability unchanged)

- [ ] **Step 4: Commit**

```bash
git add crates/manifold-sql/src/lib.rs crates/manifold-sql/benches/sqlite_comparison.rs
git commit -m "perf(manifold-sql): configurable auto-commit durability"
```

---

### Task 4: Reduce commit overhead for small transactions

**Impact: ~5-10us savings per commit**

`commit_inner()` does several steps that are expensive for tiny transactions:

1. `flush_and_close()` iterates ALL modified tables and updates their root headers
2. `store_data_freed_pages()` processes freed page lists
3. Various lock acquisitions and assertions

For a single-row INSERT that modified 1-2 tables (data + index), most of this is overhead. The assertions alone (`assert!(freed_pages.is_empty())`) require lock acquisitions.

This is deep in the manifold storage layer and risky to change. **Defer to a future session.**

---

### Task 5: Benchmark verification

- [ ] **Step 1: Run SQLite comparison benchmark**

```bash
cargo bench -p manifold-sql --bench sqlite_comparison -- --noplot
```

- [ ] **Step 2: Compare results**

Target improvements from Task 1 + Task 3 combined:

| Benchmark | Before | Target |
|-----------|--------|--------|
| single_insert | 73 us (9x) | ~40 us (5x) |
| update_by_pk | 73 us (30x) | ~50 us (20x) |
| delete_by_pk | 151 us (10x) | ~100 us (7x) |
| bulk_insert/1K | 9.5 ms (13x) | ~5 ms (7x) |

- [ ] **Step 3: Commit**

```bash
git commit -m "bench: write performance after optimization"
```

---

## Summary

| Task | Optimization | Expected Impact | Complexity |
|------|-------------|-----------------|------------|
| 1 | Skip PK index for PK=rowid | 15-20us/write | Low |
| 2 | ~~Cache table handles~~ | Skipped | — |
| 3 | Configurable durability | 20-40us/write | Low |
| 4 | ~~Reduce commit overhead~~ | Deferred | High risk |
| 5 | Benchmark | — | — |

**Expected total improvement:** 35-60us per auto-commit write, bringing INSERT from ~73us to ~30-40us (3-5x SQLite) and UPDATE from ~73us to ~40-50us (~17-20x SQLite, down from 30x).

The UPDATE gap will remain the largest because each update inherently requires: 1 GET + 1 INSERT + 2N index operations (N indexes). SQLite's in-place page modification avoids the CoW chain entirely.

**What WON'T be fixed (requires storage engine changes):**
- CoW B-tree page copies (fundamental to manifold's crash safety model)
- Table open overhead (requires handle caching with complex lifetimes)
- WAL format efficiency (would need a rewrite)
