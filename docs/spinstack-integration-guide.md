# SpinStack Integration Guide: manifold-sql Backend

This document is a self-contained reference for SpinStack developers implementing manifold-sql as an optional storage backend. It assumes familiarity with the SpinStack codebase but no prior knowledge of manifold-sql or the Manifold storage engine.

---

## 1. What is manifold-sql?

`manifold-sql` is an embedded SQL database engine built on Manifold's copy-on-write (CoW) B-tree storage engine. It provides SQLite-level SQL functionality with strict typing and is designed to be embedded directly in a host process — no separate server process, no network round-trips.

- **Crate name:** `manifold-sql`
- **Location in Manifold repo:** `crates/manifold-sql` at `/home/daniel/projects/manifold`
- **Embedding model:** same as SQLite/LibSQL — open a file path, get a database handle
- **Key difference from LibSQL:** strict column typing, `$1/$2` parameter syntax (not `?`/`?1`), and a different feature surface (see section 7)

---

## 2. Public API Reference

The surface area you need for the SpinStack integration is small. Import the crate and use the following types and methods.

```rust
use manifold_sql::{Database, Value, ResultSet, Row, SqlError};
```

### Opening a database

```rust
// Opens an existing database or creates a new one at the given path.
let db = Database::open("path/to/db")?;
```

Returns `Result<Database, SqlError>`. The `Database` handle is `Send + Sync` and can be shared across threads (it manages its own internal locking).

### Executing DDL and DML

```rust
// Returns the number of rows affected.
db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)", &[])?;
db.execute("INSERT INTO users (name) VALUES ($1)", &[Value::Text("alice".into())])?;
db.execute("DELETE FROM users WHERE id = $1", &[Value::Integer(42)])?;
```

### Querying

```rust
// Returns a ResultSet containing zero or more rows.
let result = db.query("SELECT id, name FROM users WHERE id = $1", &[Value::Integer(1)])?;

for row in result.rows() {
    let id: i64 = row.get(0)?;
    let name: String = row.get(1)?;
}
```

`row.get::<T>(column_index)` deserializes the column value into the Rust type. It returns `Err(SqlError)` if the value is null or the type does not match.

### Transactions (explicit handle)

```rust
let tx = db.begin()?;
tx.execute("INSERT INTO users (name) VALUES ($1)", &[Value::Text("bob".into())])?;
tx.query("SELECT name FROM users WHERE name = $1", &[Value::Text("bob".into())])?;
tx.commit()?;  // or tx.rollback()?
```

The `Transaction` type exposes the same `execute` and `query` methods as `Database`. Dropping a transaction without committing rolls it back.

### Transactions (SQL-level)

SQL `BEGIN`/`COMMIT`/`ROLLBACK` statements work through the normal `execute` path:

```rust
db.execute("BEGIN", &[])?;
db.execute("INSERT INTO users (name) VALUES ($1)", &[Value::Text("carol".into())])?;
db.execute("COMMIT", &[])?;
```

---

## 3. Value Type Mapping

SpinStack's WIT interface uses `store::Value`. The table below shows how to map between WIT values and `manifold_sql::Value` in both directions.

| WIT `store::Value` | `manifold_sql::Value` |
|---|---|
| `null` | `Value::Null` |
| `text(String)` | `Value::Text(String)` |
| `integer(i64)` | `Value::Integer(i64)` |
| `real(f64)` | `Value::Real(f64)` |
| `blob(Vec<u8>)` | `Value::Blob(Vec<u8>)` |
| `boolean(bool)` | `Value::Boolean(bool)` |

### Conversion helpers

```rust
fn wit_to_manifold(v: store::Value) -> manifold_sql::Value {
    match v {
        store::Value::Null           => manifold_sql::Value::Null,
        store::Value::Text(s)        => manifold_sql::Value::Text(s),
        store::Value::Integer(i)     => manifold_sql::Value::Integer(i),
        store::Value::Real(f)        => manifold_sql::Value::Real(f),
        store::Value::Blob(b)        => manifold_sql::Value::Blob(b),
        store::Value::Boolean(b)     => manifold_sql::Value::Boolean(b),
    }
}

fn manifold_to_wit(v: manifold_sql::Value) -> store::Value {
    match v {
        manifold_sql::Value::Null       => store::Value::Null,
        manifold_sql::Value::Text(s)    => store::Value::Text(s),
        manifold_sql::Value::Integer(i) => store::Value::Integer(i),
        manifold_sql::Value::Real(f)    => store::Value::Real(f),
        manifold_sql::Value::Blob(b)    => store::Value::Blob(b),
        manifold_sql::Value::Boolean(b) => store::Value::Boolean(b),
        // Extended types — no WIT equivalent today; serialize as text or error
        manifold_sql::Value::SmallInt(i)    => store::Value::Integer(i as i64),
        manifold_sql::Value::Decimal(d)     => store::Value::Text(d.to_string()),
        manifold_sql::Value::Uuid(u)        => store::Value::Text(u.to_string()),
        manifold_sql::Value::Date(d)        => store::Value::Text(d.to_string()),
        manifold_sql::Value::Timestamp(ts)  => store::Value::Text(ts.to_string()),
        manifold_sql::Value::TimestampTz(t) => store::Value::Text(t.to_string()),
        manifold_sql::Value::Json(j)        => store::Value::Text(j.to_string()),
    }
}
```

Note: manifold-sql supports additional types (`SmallInt`, `Decimal`, `Uuid`, `Date`, `Timestamp`, `TimestampTz`, `Json`) that have no direct WIT equivalent today. The mapping above degrades them gracefully; if you need round-trip fidelity for these types, extending the WIT interface is the correct path.

---

## 4. Where to Make Changes in SpinStack

### Integration point

The primary integration point is:

```
spinstack-host/src/capabilities/store.rs
```

This file contains the `AnyStore` enum, which currently dispatches to LibSQL connections. Each API instance gets an isolated LibSQL database at `./data/{owner}/{api}.db`. The WIT `store` interface exposes `execute`, `query`, `begin_tx`, and CRUD helpers.

### Step-by-step changes

**Step 1 — Add the dependency.**

In `spinstack-host/Cargo.toml`:

```toml
[dependencies]
manifold-sql = { path = "../../manifold/crates/manifold-sql" }
```

Adjust the relative path to wherever the Manifold repo is checked out relative to the SpinStack repo. If manifold-sql is published to a registry:

```toml
manifold-sql = "0.1"
```

**Step 2 — Add a new `AnyStore` variant.**

```rust
enum AnyStore {
    LibSql(/* existing LibSQL connection type */),
    Manifold(manifold_sql::Database),
}
```

Or, if the architecture suits it better, introduce a separate `ManifoldStore` struct and integrate it at the same level.

**Step 3 — Per-API database file path.**

Use a distinct file extension to coexist with existing LibSQL databases:

```rust
let db_path = format!("./data/{}/{}.mdb", owner, api_id);
let db = manifold_sql::Database::open(&db_path)?;
```

The `.mdb` extension prevents filename collisions with the `.db` LibSQL files in the same data directory.

**Step 4 — Implement `execute` dispatch.**

```rust
fn store_execute(
    store: &AnyStore,
    sql: &str,
    params: Vec<store::Value>,
) -> Result<u64, store::Error> {
    match store {
        AnyStore::LibSql(conn) => { /* existing path */ }
        AnyStore::Manifold(db) => {
            let params: Vec<manifold_sql::Value> = params.into_iter()
                .map(wit_to_manifold)
                .collect();
            let rows_affected = db.execute(sql, &params)
                .map_err(|e| store::Error::from(e.to_string()))?;
            Ok(rows_affected)
        }
    }
}
```

**Step 5 — Implement `query` dispatch.**

```rust
fn store_query(
    store: &AnyStore,
    sql: &str,
    params: Vec<store::Value>,
) -> Result<store::ResultSet, store::Error> {
    match store {
        AnyStore::LibSql(conn) => { /* existing path */ }
        AnyStore::Manifold(db) => {
            let params: Vec<manifold_sql::Value> = params.into_iter()
                .map(wit_to_manifold)
                .collect();
            let result = db.query(sql, &params)
                .map_err(|e| store::Error::from(e.to_string()))?;
            Ok(convert_result_set(result))
        }
    }
}

fn convert_result_set(result: manifold_sql::ResultSet) -> store::ResultSet {
    let rows = result.rows().map(|row| {
        let values = (0..row.column_count())
            .map(|i| manifold_to_wit(row.get_value(i)))
            .collect();
        store::Row { values }
    }).collect();
    store::ResultSet {
        columns: result.column_names().to_vec(),
        rows,
    }
}
```

Adjust field names (`column_count`, `get_value`, `column_names`) to match the actual manifold-sql API — check the crate source at `crates/manifold-sql/src/` in the Manifold repo.

**Step 6 — Implement `begin_tx` dispatch.**

The WIT `begin_tx()` call should map to `db.begin()`. The resulting `manifold_sql::Transaction` needs to be wrapped in a resource that also implements `execute` and `query`, then `commit()`/`rollback()` on close.

```rust
// Pseudocode — adapt to SpinStack's WIT resource model
fn store_begin_tx(store: &AnyStore) -> Result<TxResource, store::Error> {
    match store {
        AnyStore::LibSql(conn) => { /* existing path */ }
        AnyStore::Manifold(db) => {
            let tx = db.begin()
                .map_err(|e| store::Error::from(e.to_string()))?;
            Ok(TxResource::Manifold(tx))
        }
    }
}
```

---

## 5. Backend Selection and Migration Strategy

### Config flag

Add a per-API or global config option to choose the storage backend. Example in the API manifest or host config:

```toml
[storage]
backend = "manifold"   # or "libsql" (default for existing APIs)
```

### Rollout approach

- New APIs: default to `manifold` backend
- Existing APIs: stay on `libsql` until explicitly migrated
- Both backends coexist in the host process simultaneously — no restart required when new APIs come online with a different backend

### Data migration

A separate migration utility (not part of the main host binary) should:

1. Open the source LibSQL `.db` file
2. Dump all tables, schemas, and rows
3. Re-create schemas in a new manifold-sql `.mdb` file using `CREATE TABLE` DDL
4. INSERT all rows, mapping SQLite types to manifold-sql `Value` variants
5. Verify row counts

This is a one-way, offline migration. There is no live replication between backends.

---

## 6. What Stays the Same

The following are unchanged by this integration. Guest code and the WIT interface are unaffected.

- **WIT `store` interface** — no changes to the `.wit` file
- **Guest SDK code** — Wasm components continue to use the same bindings
- **Capability gating** — the store capability is granted/revoked the same way
- **Per-API isolation model** — one database file per `(owner, api_id)` pair
- **Transaction semantics** — begin/commit/rollback behavior is identical from the guest's perspective

---

## 7. Known Limitations vs LibSQL

Before migrating or enabling the manifold backend for a given API, verify that the API's SQL usage is compatible.

| Area | LibSQL behavior | manifold-sql behavior |
|---|---|---|
| Parameter syntax | `?` or `?1`, `?2` | `$1`, `$2` (positional) |
| Column typing | Dynamic (any value in any column) | Strict (column type is enforced) |
| Subqueries in WHERE/FROM | Supported | Not yet implemented |
| 3-way joins | Supported | Not yet implemented |
| Self-joins | Supported | Not yet implemented |
| Full-text search (FTS) | Supported via extension | Not supported |
| JSON path operators (`->>`, etc.) | Supported | Not supported |

Guest SDK code that currently targets LibSQL will need parameter syntax updated from `?`/`?1` to `$1` when switching backends. This is the only breaking change visible to guest code authors — everything else is host-side.

---

## 8. Cargo Dependency

**Path dependency (local checkout):**

```toml
[dependencies]
manifold-sql = { path = "../../manifold/crates/manifold-sql" }
```

Adjust the path based on where the Manifold repo is checked out relative to `spinstack-host/Cargo.toml`. A common layout places the two repos side by side:

```
~/projects/
  manifold/
    crates/manifold-sql/
  spinstack/
    spinstack-host/
      Cargo.toml   ← path = "../../manifold/crates/manifold-sql"
```

**Registry dependency (once published):**

```toml
[dependencies]
manifold-sql = "0.1"
```

---

## 9. Quick Reference

| Task | manifold-sql call |
|---|---|
| Open/create database | `Database::open(path)?` |
| Execute DDL/DML | `db.execute(sql, &[Value::...])?` |
| Query rows | `db.query(sql, &[Value::...])?` |
| Start transaction | `db.begin()?` |
| Commit transaction | `tx.commit()?` |
| Rollback transaction | `tx.rollback()?` |
| Error type | `manifold_sql::SqlError` |
| Parameter placeholder | `$1`, `$2`, ... (one-indexed) |
| File extension (recommended) | `.mdb` |
