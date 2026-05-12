# SpinStack Store: Unified Data Store Interface

## Purpose

A high-level, typed data store SDK for SpinStack tenant APIs that abstracts over multiple storage backends (manifold KV, SQLite). Developers write against one API; the backend is chosen via manifest configuration. This unlocks manifold's raw KV speed (54K ops/sec) for developers who don't need SQL, while preserving SQLite as an option for complex query workloads.

## Architecture

Three layers:

1. **`spinstack-store` SDK crate** — Derive macro + query builder. Compiles to Wasm with the tenant API. Builds `QueryDescriptor` values.
2. **Wasm host boundary** — Single `store_execute(descriptor_bytes) -> result_bytes` host function. Descriptors serialized with a compact binary format.
3. **Host backend dispatch** — Deserializes the descriptor, executes against the configured backend (manifold KV or SQLite), returns serialized results.

## Data Model: Derive Macro

Developers define their schema as Rust structs:

```rust
#[derive(Store)]
struct Post {
    #[primary_key]
    id: u64,
    #[indexed]
    author_id: u64,
    content: String,
    created_at: i64,
}

#[derive(Store)]
struct Comment {
    #[primary_key]
    id: u64,
    #[indexed]
    post_id: u64,
    author_id: u64,
    body: String,
    created_at: i64,
}
```

The derive macro generates:

- **Typed field accessors** (`Post::author_id`) for use in query builder filters and ordering. Each accessor carries the column index and type.
- **Table name** (`Post::TABLE_NAME`) as a static string (lowercase struct name by default, overridable via `#[store(name = "posts")]`).
- **Schema descriptor** (`Post::schema()`) returning column definitions, types, and index declarations for backend table creation.
- **Row serialization** (`impl StoreSerialize/StoreDeserialize`) for compact binary encoding of struct fields. Not serde — a purpose-built format using fixed-width fields and length-prefixed strings, matching manifold's row format.

## Query API

### Point Operations

```rust
// Get by primary key
let post: Option<Post> = store.get::<Post>(42)?;

// Insert
store.insert(&post)?;

// Update (by primary key)
store.update(&post)?;

// Delete
store.delete::<Post>(42)?;
```

### Queries

```rust
// Filtered scan using an index
let comments = store.query::<Comment>()
    .filter(Comment::post_id.eq(42))
    .order_by(Comment::created_at, Desc)
    .limit(20)
    .fetch()?;

// Aggregate
let count = store.query::<Post>()
    .filter(Post::author_id.eq(user_id))
    .count()?;

// Join
let feed = store.query::<Post>()
    .join::<User>(Post::author_id, User::id)
    .order_by(Post::created_at, Desc)
    .limit(20)
    .fetch()?;
```

### Transactions

Multi-table atomic operations:

```rust
store.transaction(|txn| {
    let post = Post { id: next_id(), author_id: user_id, content: "hello".into(), created_at: now() };
    txn.insert(&post)?;

    let mut user = txn.get::<User>(user_id)?.unwrap();
    user.post_count += 1;
    txn.update(&user)?;

    Ok(())
})?;
```

The transaction closure receives a `Txn` handle. All operations through `txn` are batched and executed atomically. If the closure returns `Err`, the transaction rolls back.

## Wire Format: QueryDescriptor

The SDK builds a `QueryDescriptor` that crosses the Wasm boundary as serialized bytes via a single host function call.

```rust
enum QueryDescriptor {
    Get { table_id: u16, key_bytes: Vec<u8> },
    Insert { table_id: u16, row_bytes: Vec<u8> },
    Update { table_id: u16, key_bytes: Vec<u8>, row_bytes: Vec<u8> },
    Delete { table_id: u16, key_bytes: Vec<u8> },
    Scan {
        table_id: u16,
        filters: Vec<Filter>,
        order_by: Option<(u16, Direction)>,  // column index + direction
        limit: Option<u32>,
        offset: Option<u32>,
    },
    Aggregate {
        table_id: u16,
        filters: Vec<Filter>,
        func: AggregateFunc,  // Count, Sum, Min, Max, Avg
        column: Option<u16>,
    },
    Join {
        left_table: u16,
        right_table: u16,
        left_column: u16,
        right_column: u16,
        filters: Vec<Filter>,
        order_by: Option<(u16, Direction)>,
        limit: Option<u32>,
    },
    Transaction {
        ops: Vec<QueryDescriptor>,  // executed atomically
    },
}

struct Filter {
    column: u16,
    op: FilterOp,  // Eq, Ne, Lt, Le, Gt, Ge
    value: ValueBytes,
}
```

Column references use u16 indices, not strings. Table references use u16 IDs assigned at schema registration. This keeps the wire format compact and avoids string comparison at execution time.

## Host Boundary

Single host function:

```
store_execute(descriptor_ptr: u32, descriptor_len: u32) -> u64
```

Returns a packed pointer+length to the result buffer in Wasm linear memory. The host allocates the result via the Wasm allocator.

The SDK wraps this in a safe Rust API — developers never see raw pointers or serialization.

## Backend Dispatch

### Manifold Backend

Maps descriptors directly to manifold KV operations:

| Descriptor | Manifold Operation |
|---|---|
| `Get` | `table.get(key)` — O(1) B-tree lookup |
| `Insert` | `table.insert(key, value)` |
| `Update` | `table.get(key)` + `table.insert(key, new_value)` |
| `Delete` | `table.remove(key)` |
| `Scan` (with indexed filter) | Index lookup → rowid → `table.get(rowid)` per match |
| `Scan` (with non-indexed filter) | `table.iter()` + decode + filter |
| `Aggregate(Count)` (no filter) | `table.len()` — O(1) from B-tree metadata |
| `Join` (equi-join) | Hash join: build hash table on smaller side, probe |
| `Transaction` | `cf.begin_write()` → execute all ops → `commit()` |

Each column family database holds all tables for one tenant API. Indexes are separate manifold tables (`idx_{table}_{column}`).

Expected performance: ~54K ops/sec for point operations (matching raw KV benchmark).

### SQLite Backend

Translates descriptors into SQL strings:

| Descriptor | SQL |
|---|---|
| `Get` | `SELECT * FROM {table} WHERE id = ?` |
| `Scan` | `SELECT * FROM {table} WHERE {filters} ORDER BY {col} LIMIT {n}` |
| `Aggregate` | `SELECT COUNT(*)/SUM(col) FROM {table} WHERE {filters}` |
| `Join` | `SELECT ... FROM {left} JOIN {right} ON ... WHERE ...` |
| `Transaction` | `BEGIN; {ops}; COMMIT;` |

Uses `prepare_cached` for all generated SQL. Expected performance: ~54K ops/sec (SQLite benchmark with indexes).

### Schema Registration

On first request (or API deploy), the host reads all `#[derive(Store)]` schemas from the Wasm module's exported metadata and creates tables + indexes in the backend. Schema is embedded in the Wasm binary as a static byte array by the derive macro.

## SpinStack Manifest

```toml
[store]
engine = "manifold"  # or "sqlite"
```

The developer's code is identical regardless of engine. The host reads the manifest at deploy time and initializes the appropriate backend.

## What's NOT Included

- **Schema migrations** — Adding/removing columns is a future concern. V1 requires matching schema.
- **Raw SQL escape hatch** — The whole point is engine-agnostic queries.
- **Cross-API joins** — Each API has its own isolated store.
- **Replication** — Single-node only.
- **Full-text search** — Future extension via `#[full_text]` attribute.

## Crate Structure

```
spinstack-store/
  spinstack-store-macros/   # proc-macro crate (derive Store)
  spinstack-store/          # SDK crate (query builder, serialization, host bindings)
  spinstack-store-host/     # Host-side crate (backend dispatch, manifold/sqlite impls)
```

## Success Criteria

- Same API code works with both manifold and SQLite backends.
- Manifold backend achieves within 2x of raw KV benchmark (>25K ops/sec) for the social feed workload.
- SQLite backend matches current rusqlite performance (~54K ops/sec).
- Derive macro catches schema errors at compile time.
- Single Wasm boundary crossing per query/transaction.
