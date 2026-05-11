# Manifold-SQL: Embedded SQL Engine Design

## Overview

`manifold-sql` is a standalone embedded SQL engine built on top of Manifold's CoW B-tree storage engine. It provides SQLite-level SQL functionality with strict typing, targeting OLTP workloads. It is designed to be used as a Rust library (like `rusqlite`) and can serve as a drop-in replacement for LibSQL in SpinStack's per-API storage layer.

The engine is intentionally designed as a SQL layer only — Manifold's existing `manifold-vectors`, `manifold-graph`, and `manifold-timeseries` crates handle other data models. Future convergence between these is possible but out of scope.

## Crate Structure

- **Crate name:** `manifold-sql`
- **Location:** `crates/manifold-sql`
- **Workspace member** alongside existing domain crates

### Dependencies

- `manifold-db` — core storage engine
- `sqlparser` — SQL parsing
- `rust_decimal` — `DECIMAL`/`NUMERIC` support
- `chrono` — `DATE`/`TIMESTAMP` types
- `uuid` — `UUID` type
- `serde_json` — `JSON`/`JSONB` support

## Public API

Embedded library API — no wire protocol, no networking.

```rust
use manifold_sql::{Database, Value};

// Open or create
let db = Database::open("path/to/db")?;

// Execute statements (DDL, DML)
db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)", &[])?;
db.execute("INSERT INTO users (name) VALUES (?1)", &[Value::Text("alice".into())])?;

// Query
let rows = db.query("SELECT id, name FROM users WHERE id = ?1", &[Value::Integer(1)])?;
for row in rows {
    let id: i64 = row.get(0)?;
    let name: &str = row.get(1)?;
}

// Transactions
let tx = db.begin()?;
tx.execute("INSERT INTO users (name) VALUES (?1)", &[Value::Text("bob".into())])?;
tx.execute("INSERT INTO users (name) VALUES (?1)", &[Value::Text("carol".into())])?;
tx.commit()?;

// EXPLAIN
let plan = db.explain("SELECT * FROM users JOIN orders ON users.id = orders.user_id")?;
println!("{}", plan);
```

### Value Enum

```rust
pub enum Value {
    Null,
    Boolean(bool),
    SmallInt(i16),
    Integer(i64),
    Real(f64),
    Decimal(rust_decimal::Decimal),
    Text(String),
    Blob(Vec<u8>),
    Uuid(uuid::Uuid),
    Date(chrono::NaiveDate),
    Timestamp(chrono::NaiveDateTime),
    TimestampTz(chrono::DateTime<chrono::Utc>),
    Json(serde_json::Value),
}
```

## Architecture: Classic Layered Pipeline

```
SQL Text → Parser → Binder → Planner → Optimizer → Executor → Manifold
```

Each phase has its own intermediate representation. The pipeline is fully inspectable via `EXPLAIN`.

## Module Layout

```
manifold-sql/src/
├── lib.rs                  # Public API (Database, Transaction, Value, Row)
├── types.rs                # Value enum, type IDs, type coercion rules
├── catalog/
│   ├── mod.rs              # Catalog trait + in-memory catalog
│   ├── schema.rs           # TableSchema, ColumnDef, IndexDef, ConstraintDef
│   └── persist.rs          # Catalog <-> Manifold tables (serialize/deserialize schemas)
├── parser/
│   └── mod.rs              # Thin wrapper around sqlparser-rs, dialect config
├── binder/
│   ├── mod.rs              # Name resolution, type checking
│   ├── expr.rs             # Expression binding (resolve columns, check types)
│   └── statement.rs        # Statement-level binding (SELECT, INSERT, etc.)
├── planner/
│   ├── mod.rs              # Logical plan builder
│   ├── plan.rs             # LogicalPlan enum
│   └── expr.rs             # Scalar expressions, aggregate expressions
├── optimizer/
│   ├── mod.rs              # Optimizer pipeline (run rules in order)
│   ├── rules/
│   │   ├── predicate_pushdown.rs
│   │   ├── join_reorder.rs
│   │   ├── index_selection.rs
│   │   └── constant_folding.rs
│   └── statistics.rs       # Table stats (row count, index cardinality)
├── executor/
│   ├── mod.rs              # Volcano iterator trait, execution context
│   ├── scan.rs             # TableScan, IndexScan, IndexOnlyScan
│   ├── filter.rs           # Filter operator
│   ├── project.rs          # Projection operator
│   ├── join.rs             # NestedLoopJoin, IndexJoin
│   ├── aggregate.rs        # HashAggregate
│   ├── sort.rs             # Sort operator (in-memory)
│   ├── limit.rs            # Limit/Offset
│   ├── modify.rs           # Insert, Update, Delete executors
│   ├── union.rs            # Union / Union All
│   └── subquery.rs         # Subquery evaluation
├── storage/
│   ├── mod.rs              # StorageEngine trait
│   ├── table.rs            # Row storage (rowid -> encoded row)
│   ├── index.rs            # Index storage (index key -> rowid)
│   ├── row_format.rs       # Binary row encoding/decoding
│   └── catalog_tables.rs   # System tables (_tables, _columns, _indexes, etc.)
├── expr/
│   ├── eval.rs             # Expression evaluator
│   └── functions.rs        # Built-in scalar functions
└── error.rs                # Error types
```

## Type System

Strict static typing — columns have enforced types, inserts with wrong types are rejected.

### Supported Types

| SQL Type | Rust Storage | Fixed Width | Size |
|---|---|---|---|
| `BOOLEAN` | `u8` | Yes | 1 byte |
| `SMALLINT` | `i16` | Yes | 2 bytes |
| `INTEGER` | `i64` | Yes | 8 bytes |
| `BIGINT` | `i64` | Yes | 8 bytes |
| `REAL` / `DOUBLE` | `f64` | Yes | 8 bytes |
| `DECIMAL(p,s)` / `NUMERIC(p,s)` | `rust_decimal::Decimal` | Yes | 16 bytes |
| `TEXT` | UTF-8 bytes | No | variable |
| `VARCHAR(n)` | UTF-8 bytes (length-checked) | No | variable |
| `BLOB` | raw bytes | No | variable |
| `UUID` | `[u8; 16]` | Yes | 16 bytes |
| `DATE` | `i32` (days from epoch) | Yes | 4 bytes |
| `TIMESTAMP` | `i64` (microseconds from epoch) | Yes | 8 bytes |
| `TIMESTAMP WITH TIME ZONE` | `i64` (microseconds from epoch, UTC) | Yes | 8 bytes |
| `JSON` / `JSONB` | UTF-8 JSON text | No | variable |

## Storage Mapping

### System Tables

| System Table | Key | Value | Purpose |
|---|---|---|---|
| `_meta` | `&str` | `&[u8]` | DB version, settings |
| `_tables` | `u32` (table_id) | `&[u8]` (serialized TableSchema) | Table definitions |
| `_columns` | `(u32, u16)` (table_id, col_idx) | `&[u8]` (serialized ColumnDef) | Column definitions |
| `_indexes` | `u32` (index_id) | `&[u8]` (serialized IndexDef) | Index definitions |
| `_constraints` | `u32` (constraint_id) | `&[u8]` (serialized ConstraintDef) | Constraint definitions |
| `_statistics` | `u32` (table_id) | `&[u8]` (serialized TableStats) | Row count, cardinality estimates |
| `_sequences` | `u32` (table_id) | `u64` | Next rowid per table |

### User Data Tables

Each SQL table `foo` maps to a Manifold table:

```
Manifold table: "data_foo"
Key:   u64 (rowid)
Value: &[u8] (encoded row bytes)
```

Primary key model: implicit rowid (SQLite-style). Every row gets an auto-incrementing `u64` rowid as the Manifold table key. `INTEGER PRIMARY KEY` aliases the rowid. All other primary keys are implemented as unique indexes pointing back to the rowid.

### Indexes

Each index maps to a Manifold table:

```
// Non-unique index:
Manifold multimap table: "idx_{table}_{name}"
Key:   &[u8] (encoded index key)
Value: u64 (rowid)

// Unique index:
Manifold table: "idx_{table}_{name}_uq"
Key:   &[u8] (encoded index key)
Value: u64 (rowid)
```

Composite index keys are encoded by concatenating column values with length prefixes for variable-width types, preserving sort order so Manifold's range scans work for prefix queries on composite indexes.

### Row Format

Binary encoding:

```
┌─────────────────────────────────────────────────┐
│ Column count (u16)                              │
│ Null bitmap (ceil(col_count / 8) bytes)         │
│ Fixed-width values (inline, in column order)    │
│ Variable-width offsets (u32 per var-width col)  │
│ Variable-width data (concatenated)              │
└─────────────────────────────────────────────────┘
```

- Fixed-width types stored inline at known offsets — O(1) access. Null fixed-width columns still occupy their full slot (zeroed) so subsequent column offsets remain stable.
- Variable-width types use an offset table — O(1) access without scanning. Null variable-width columns store a zero-length entry (offset points to same position as next column).
- The null bitmap is the authoritative source for null checks — the zeroed slot values are never read for null columns.

### Transaction Mapping

| SQL | Manifold |
|---|---|
| `BEGIN` | `db.begin_write()` (or `begin_read()` for read-only) |
| `COMMIT` | `write_txn.commit()` |
| `ROLLBACK` | `write_txn.abort()` |
| `SAVEPOINT name` | `write_txn.ephemeral_savepoint()` |
| `ROLLBACK TO name` | `write_txn.restore_savepoint()` |

## Query Pipeline

### Phase 1: Parse

Thin wrapper around `sqlparser-rs` with generic SQL dialect. Produces `sqlparser::ast::Statement`. Unsupported constructs rejected in the binder.

### Phase 2: Bind

- Column references → `(table_id, column_index)` pairs
- Table references → verified against catalog, aliases tracked
- Expressions → type-annotated (every node knows its output type)
- Type checking → mismatched types rejected (e.g., `WHERE name > 5`)
- Parameter placeholders (`?1`) → verified against provided values
- Subqueries → recursively bound with outer scope for correlated refs

Output: `BoundStatement` with resolved references and type annotations.

### Phase 3: Plan

Converts `BoundStatement` into a `LogicalPlan` tree:

```rust
enum LogicalPlan {
    Scan { table_id, projected_columns },
    Filter { predicate, input },
    Project { expressions, input },
    Join { join_type, left, right, condition },
    Aggregate { group_by, aggregates, input },
    Sort { order_by, input },
    Limit { count, offset, input },
    Distinct { input },
    Union { left, right, all },
    Insert { table_id, columns, source },
    Update { table_id, assignments, filter },
    Delete { table_id, filter },
    Empty,
}
```

Each node carries a schema (output column names + types).

### Phase 4: Optimize

Rules applied in order:

1. **Constant folding** — `1 + 2` → `3`, `true AND x` → `x`
2. **Predicate pushdown** — push `WHERE` conditions through joins and projections
3. **Index selection** — replace `Scan + Filter` with `IndexScan` when beneficial. Handles equality, range, `BETWEEN`, and composite index prefix matches.
4. **Join reordering** — cost-based using table statistics. Estimates cost as `left_rows × right_rows`, reduced by index availability. Exhaustive search for ≤5-way joins, greedy heuristic beyond that.

Statistics maintained in `_statistics` system table:
- `row_count` — updated on INSERT/DELETE
- `index_cardinality` — distinct key count per index, sampled on `ANALYZE`

`ANALYZE` command refreshes statistics by scanning tables/indexes.

### Phase 5: Execute

Volcano iterator model:

```rust
trait Executor {
    fn schema(&self) -> &Schema;
    fn open(&mut self, ctx: &ExecutionContext) -> Result<()>;
    fn next(&mut self) -> Result<Option<Row>>;
    fn close(&mut self) -> Result<()>;
}
```

| Operator | Description |
|---|---|
| `TableScan` | Full scan via `manifold_table.range(..)` |
| `IndexScan` | Range scan on index, then rowid lookup |
| `IndexOnlyScan` | Index scan when all needed columns are in the index |
| `Filter` | Evaluates predicate, passes or skips row |
| `Project` | Evaluates output expressions, produces new row |
| `NestedLoopJoin` | For small tables or when no index available |
| `IndexJoin` | Probe index on inner table for each outer row |
| `HashAggregate` | Hash-based grouping with aggregate accumulators |
| `Sort` | In-memory sort (collect all, sort, emit) |
| `Limit` | Pass through N rows, then stop |
| `Union` | Concatenate (UNION ALL) or deduplicate (UNION) |
| `Insert/Update/Delete` | Modify storage, maintain indexes, check constraints |

`EXPLAIN` walks the optimized plan tree and prints operator types, index usage, and estimated row counts.

## Constraint Enforcement

All constraints checked within the executor during write operations, inside the same Manifold write transaction. Constraint violation rolls back the entire statement.

- **NOT NULL** — checked during row encoding before writing
- **Type enforcement** — checked in binder for literals, in executor for parameter values
- **PRIMARY KEY** — implicit rowid is always unique; `INTEGER PRIMARY KEY` aliases rowid; other PK types use a unique index
- **UNIQUE** — enforced via unique index table; point lookup before insert
- **FOREIGN KEY** — on child INSERT/UPDATE: point lookup on parent's referenced index. On parent DELETE/UPDATE: `RESTRICT` (check children, fail if any), `CASCADE` (delete/update children recursively), `SET NULL` (null out referencing columns). FK checks deferred to statement end for bulk insert ordering.
- **CHECK** — expression evaluated against the row being inserted/updated, using the same `expr::eval` module as queries
- **DEFAULT** — resolved during INSERT planning; missing columns get DEFAULT expression evaluated; missing + NOT NULL + no DEFAULT = error

Constraint violation errors include constraint name, table, column(s), and offending value.

## DDL Scope

- `CREATE TABLE` / `DROP TABLE` (with `IF EXISTS` / `IF NOT EXISTS`)
- `ALTER TABLE` — add column, drop column, rename column
- `CREATE INDEX` / `DROP INDEX` (unique and non-unique, composite, with `IF EXISTS` / `IF NOT EXISTS`)

## DML Scope

- `INSERT INTO ... VALUES`, `INSERT INTO ... SELECT`
- `UPDATE ... SET ... WHERE`
- `DELETE FROM ... WHERE`

## Query Scope

- `SELECT` with column lists, `*`, expressions, aliases
- `WHERE` with comparison, `AND`/`OR`/`NOT`, `IN`, `BETWEEN`, `LIKE`, `IS NULL`
- `JOIN` — `INNER`, `LEFT`, `RIGHT`, `CROSS`
- `GROUP BY` with aggregates: `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`
- `HAVING`
- `ORDER BY` (asc/desc, nulls first/last)
- `LIMIT` / `OFFSET`
- `DISTINCT`
- Subqueries (in `WHERE`, `FROM`, `SELECT`)
- `UNION` / `UNION ALL`
- `BEGIN`, `COMMIT`, `ROLLBACK`
- `SAVEPOINT`, `RELEASE SAVEPOINT`, `ROLLBACK TO SAVEPOINT`
- `EXPLAIN`
- `ANALYZE`

## SpinStack Integration

`manifold-sql` has zero knowledge of SpinStack. Integration happens in SpinStack's `store.rs` capability:

```
Current:                          Future:
WASM guest                        WASM guest
  ↓ WIT store interface             ↓ WIT store interface (same)
store.rs → LibSQL connection      store.rs → manifold_sql::Database
  ↓                                  ↓
SQLite file per API               Manifold file per API
```

- The WIT `store` interface stays the same
- `AnyStore` gets a new variant backed by `manifold_sql::Database`
- Per-API isolation unchanged — one database per `(owner, api_id)`
- `store::Value` variants map directly to `manifold_sql::Value`
- Guest SDK code, capability gating, and transaction semantics unchanged
- Both backends can coexist behind a config flag for gradual migration
- Data migration (LibSQL → Manifold-SQL) is a SpinStack concern, not a `manifold-sql` concern

## Test Suite

~40 test files across 9 categories:

### DDL Tests (`tests/ddl/`)
- `create_table.rs` — all column types, constraints, IF NOT EXISTS
- `drop_table.rs` — drop, IF EXISTS, cascade behavior
- `alter_table.rs` — add/drop/rename columns, constraint changes
- `create_index.rs` — unique, non-unique, composite, IF NOT EXISTS

### DML Tests (`tests/dml/`)
- `insert.rs` — single, multi-row, INSERT...SELECT, defaults, type enforcement
- `update.rs` — SET, WHERE, expressions, type checking
- `delete.rs` — WHERE, cascading FK deletes

### Query Tests (`tests/query/`)
- `select_basic.rs` — column lists, *, aliases, expressions, DISTINCT
- `where_clause.rs` — comparisons, AND/OR/NOT, IN, BETWEEN, LIKE, IS NULL
- `joins.rs` — INNER, LEFT, RIGHT, CROSS, multi-table, self-joins
- `aggregates.rs` — COUNT, SUM, AVG, MIN, MAX, GROUP BY, HAVING
- `ordering.rs` — ORDER BY, NULLS FIRST/LAST, multi-column
- `limit_offset.rs` — LIMIT, OFFSET, edge cases
- `subqueries.rs` — in WHERE, FROM, SELECT, correlated
- `union.rs` — UNION, UNION ALL, type compatibility

### Transaction Tests (`tests/transactions/`)
- `basic.rs` — BEGIN, COMMIT, ROLLBACK, isolation
- `savepoints.rs` — SAVEPOINT, RELEASE, ROLLBACK TO
- `concurrent.rs` — multiple readers, writer isolation, snapshot consistency

### Constraint Tests (`tests/constraints/`)
- `primary_key.rs` — uniqueness, auto-increment rowid
- `not_null.rs` — reject nulls, defaults
- `unique.rs` — single-column, composite, null handling
- `foreign_key.rs` — references, CASCADE, SET NULL, RESTRICT
- `check.rs` — expression constraints, multi-column checks
- `type_enforcement.rs` — strict typing, reject wrong types

### Type Tests (`tests/types/`)
- `integers.rs` — SMALLINT, INTEGER, BIGINT, overflow behavior
- `floats.rs` — REAL, NaN/Inf handling
- `decimal.rs` — precision, scale, arithmetic
- `text.rs` — TEXT, VARCHAR(n), length enforcement, unicode
- `blob.rs` — binary data round-trip
- `uuid.rs` — UUID parsing, storage, comparison
- `datetime.rs` — DATE, TIMESTAMP, TIMESTAMP TZ, arithmetic
- `json.rs` — JSON/JSONB storage, round-trip
- `null.rs` — NULL semantics (three-valued logic, comparisons)

### Optimizer Tests (`tests/optimizer/`)
- `index_selection.rs` — verify indexes used for equality, range, composite
- `predicate_pushdown.rs` — verify pushdown through joins, subqueries
- `join_reorder.rs` — verify cost-based reordering with varying table sizes
- `explain.rs` — EXPLAIN output correctness

### Stress Tests (`tests/stress/`)
- `large_tables.rs` — 100K+ rows, bulk insert, full scans
- `wide_rows.rs` — many columns, large TEXT/BLOB values
- `concurrent_load.rs` — many readers + writer under load
- `transaction_heavy.rs` — rapid begin/commit/rollback cycles
- `crash_recovery.rs` — kill mid-transaction, verify consistency on reopen

### Security Tests (`tests/security/`)
- `sql_injection.rs` — parameterized queries prevent injection, malicious literals
- `malformed_input.rs` — invalid SQL, partial statements, huge inputs
- `resource_limits.rs` — deeply nested expressions, huge IN lists, runaway queries
- `type_confusion.rs` — attempts to bypass strict typing

### Robustness Tests (`tests/robustness/`)
- `empty_tables.rs` — operations on empty tables, empty results
- `edge_values.rs` — i64::MAX, empty strings, zero-length blobs, max precision decimals
- `reopen.rs` — close and reopen DB, verify all data/schema intact
- `schema_evolution.rs` — ALTER sequences, add/drop columns with existing data
- `error_messages.rs` — verify errors are clear and actionable
