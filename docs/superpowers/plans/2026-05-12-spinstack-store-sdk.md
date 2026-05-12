# SpinStack Store SDK Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `spinstack-store` SDK crate (query builder + types) and `spinstack-store-macros` proc-macro crate (derive Store), testable without Wasm boundary. The host-side backend dispatch is a separate plan.

**Architecture:** Two crates: a proc-macro crate that generates schema/serialization code from Rust structs, and an SDK crate that provides the query builder API and descriptor types. Tests verify the derive macro generates correct code, the query builder produces correct descriptors, and serialization round-trips correctly. No actual storage backend — that comes in the host crate.

**Tech Stack:** Rust, proc-macro2/syn/quote for derive macro, workspace crates under `crates/`.

---

## File Structure

| File | Responsibility |
|------|---------------|
| `crates/spinstack-store-macros/Cargo.toml` | Proc-macro crate manifest |
| `crates/spinstack-store-macros/src/lib.rs` | `#[derive(Store)]` implementation |
| `crates/spinstack-store/Cargo.toml` | SDK crate manifest |
| `crates/spinstack-store/src/lib.rs` | Public API re-exports |
| `crates/spinstack-store/src/types.rs` | `Value`, `ColumnType`, `Direction`, `FilterOp` enums |
| `crates/spinstack-store/src/schema.rs` | `TableSchema`, `ColumnDef`, `IndexDef` — schema descriptor types |
| `crates/spinstack-store/src/descriptor.rs` | `QueryDescriptor`, `Filter` — wire format types |
| `crates/spinstack-store/src/query.rs` | `Query<T>` builder with `.filter()`, `.order_by()`, `.limit()`, `.fetch()` |
| `crates/spinstack-store/src/field.rs` | `Field<T>` typed column accessor for filter expressions |
| `crates/spinstack-store/src/serialize.rs` | `StoreSerialize`/`StoreDeserialize` traits + impls for primitives |
| `crates/spinstack-store/src/error.rs` | `StoreError` type |
| `crates/spinstack-store/src/store.rs` | `Store` trait — the main interface (`get`, `insert`, `query`, `transaction`) |
| `crates/spinstack-store/src/transaction.rs` | `Txn` handle for multi-table atomic ops |
| `crates/spinstack-store/tests/derive_test.rs` | Tests for derive macro code generation |
| `crates/spinstack-store/tests/query_test.rs` | Tests for query builder → descriptor |
| `crates/spinstack-store/tests/serialize_test.rs` | Tests for row serialization round-trips |

---

### Task 1: Workspace setup + core types

**Files:**
- Create: `crates/spinstack-store/Cargo.toml`
- Create: `crates/spinstack-store/src/lib.rs`
- Create: `crates/spinstack-store/src/types.rs`
- Create: `crates/spinstack-store/src/error.rs`
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Create SDK crate**

`crates/spinstack-store/Cargo.toml`:
```toml
[package]
name = "spinstack-store"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]

[dev-dependencies]
tempfile = "3.5"
```

- [ ] **Step 2: Create core types**

`crates/spinstack-store/src/types.rs`:
```rust
/// Column data types supported by the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    U64,
    I64,
    F64,
    String,
    Bytes,
    Bool,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Asc,
    Desc,
}

/// Filter comparison operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// A dynamically-typed value for filter comparisons and row data.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    U64(u64),
    I64(i64),
    F64(f64),
    String(String),
    Bytes(Vec<u8>),
    Bool(bool),
    Null,
}
```

- [ ] **Step 3: Create error type**

`crates/spinstack-store/src/error.rs`:
```rust
use std::fmt;

#[derive(Debug)]
pub enum StoreError {
    NotFound,
    DuplicateKey,
    SchemaError(String),
    SerializationError(String),
    BackendError(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::NotFound => write!(f, "record not found"),
            StoreError::DuplicateKey => write!(f, "duplicate primary key"),
            StoreError::SchemaError(msg) => write!(f, "schema error: {msg}"),
            StoreError::SerializationError(msg) => write!(f, "serialization error: {msg}"),
            StoreError::BackendError(msg) => write!(f, "backend error: {msg}"),
        }
    }
}

impl std::error::Error for StoreError {}

pub type Result<T> = std::result::Result<T, StoreError>;
```

- [ ] **Step 4: Create lib.rs with re-exports**

`crates/spinstack-store/src/lib.rs`:
```rust
pub mod types;
pub mod error;

pub use types::{ColumnType, Direction, FilterOp, Value};
pub use error::{StoreError, Result};
```

- [ ] **Step 5: Add to workspace**

In root `Cargo.toml`, add `"crates/spinstack-store"` to the `members` array.

- [ ] **Step 6: Verify**

Run: `cargo check -p spinstack-store`
Expected: compiles

- [ ] **Step 7: Commit**

```bash
git add crates/spinstack-store/ Cargo.toml
git commit -m "feat(spinstack-store): workspace setup + core types"
```

---

### Task 2: Schema descriptor types

**Files:**
- Create: `crates/spinstack-store/src/schema.rs`
- Modify: `crates/spinstack-store/src/lib.rs`

- [ ] **Step 1: Create schema types**

`crates/spinstack-store/src/schema.rs`:
```rust
use crate::types::ColumnType;

/// Describes a single column in a table.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDef {
    pub name: &'static str,
    pub column_type: ColumnType,
    pub is_primary_key: bool,
    pub is_indexed: bool,
}

/// Describes a table schema, generated by the derive macro.
#[derive(Debug, Clone, PartialEq)]
pub struct TableSchema {
    pub table_name: &'static str,
    pub columns: &'static [ColumnDef],
}

/// Trait implemented by `#[derive(Store)]` structs.
pub trait StoreRecord: Sized {
    /// The table name for this record type.
    const TABLE_NAME: &'static str;

    /// The schema descriptor for this record type.
    fn schema() -> TableSchema;

    /// Serialize this record to bytes (row format).
    fn to_store_bytes(&self) -> Vec<u8>;

    /// Deserialize a record from bytes.
    fn from_store_bytes(bytes: &[u8]) -> crate::Result<Self>;

    /// Extract the primary key as bytes.
    fn primary_key_bytes(&self) -> Vec<u8>;
}
```

- [ ] **Step 2: Export from lib.rs**

Add to `crates/spinstack-store/src/lib.rs`:
```rust
pub mod schema;
pub use schema::{ColumnDef, TableSchema, StoreRecord};
```

- [ ] **Step 3: Verify and commit**

Run: `cargo check -p spinstack-store`

```bash
git add crates/spinstack-store/
git commit -m "feat(spinstack-store): schema descriptor types + StoreRecord trait"
```

---

### Task 3: QueryDescriptor + Filter types

**Files:**
- Create: `crates/spinstack-store/src/descriptor.rs`
- Create: `crates/spinstack-store/src/field.rs`
- Modify: `crates/spinstack-store/src/lib.rs`

- [ ] **Step 1: Create descriptor types**

`crates/spinstack-store/src/descriptor.rs`:
```rust
use crate::types::{Direction, FilterOp, Value};

/// A filter predicate: column op value.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub column_index: u16,
    pub op: FilterOp,
    pub value: Value,
}

/// Aggregate functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateFunc {
    Count,
    Sum,
    Min,
    Max,
    Avg,
}

/// A query descriptor — the wire format between SDK and host backend.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryDescriptor {
    Get {
        table_name: &'static str,
        key_bytes: Vec<u8>,
    },
    Insert {
        table_name: &'static str,
        key_bytes: Vec<u8>,
        row_bytes: Vec<u8>,
    },
    Update {
        table_name: &'static str,
        key_bytes: Vec<u8>,
        row_bytes: Vec<u8>,
    },
    Delete {
        table_name: &'static str,
        key_bytes: Vec<u8>,
    },
    Scan {
        table_name: &'static str,
        filters: Vec<Filter>,
        order_by: Option<(u16, Direction)>,
        limit: Option<u32>,
        offset: Option<u32>,
    },
    Aggregate {
        table_name: &'static str,
        filters: Vec<Filter>,
        func: AggregateFunc,
        column: Option<u16>,
    },
    Join {
        left_table: &'static str,
        right_table: &'static str,
        left_column: u16,
        right_column: u16,
        filters: Vec<Filter>,
        order_by: Option<(u16, Direction)>,
        limit: Option<u32>,
    },
    Transaction {
        ops: Vec<QueryDescriptor>,
    },
}
```

- [ ] **Step 2: Create typed field accessor**

`crates/spinstack-store/src/field.rs`:
```rust
use crate::descriptor::Filter;
use crate::types::{FilterOp, Value};

/// A typed reference to a column, generated by the derive macro.
/// Carries the column index and provides filter builder methods.
#[derive(Debug, Clone, Copy)]
pub struct Field<T> {
    pub column_index: u16,
    pub _phantom: std::marker::PhantomData<T>,
}

impl<T> Field<T> {
    pub const fn new(index: u16) -> Self {
        Self {
            column_index: index,
            _phantom: std::marker::PhantomData,
        }
    }
}

/// Trait for converting a Rust value into a store Value for filter comparisons.
pub trait IntoValue {
    fn into_value(self) -> Value;
}

impl IntoValue for u64 {
    fn into_value(self) -> Value { Value::U64(self) }
}
impl IntoValue for i64 {
    fn into_value(self) -> Value { Value::I64(self) }
}
impl IntoValue for f64 {
    fn into_value(self) -> Value { Value::F64(self) }
}
impl IntoValue for &str {
    fn into_value(self) -> Value { Value::String(self.to_string()) }
}
impl IntoValue for String {
    fn into_value(self) -> Value { Value::String(self) }
}
impl IntoValue for bool {
    fn into_value(self) -> Value { Value::Bool(self) }
}

impl<T> Field<T> {
    pub fn eq(&self, val: impl IntoValue) -> Filter {
        Filter { column_index: self.column_index, op: FilterOp::Eq, value: val.into_value() }
    }
    pub fn ne(&self, val: impl IntoValue) -> Filter {
        Filter { column_index: self.column_index, op: FilterOp::Ne, value: val.into_value() }
    }
    pub fn lt(&self, val: impl IntoValue) -> Filter {
        Filter { column_index: self.column_index, op: FilterOp::Lt, value: val.into_value() }
    }
    pub fn le(&self, val: impl IntoValue) -> Filter {
        Filter { column_index: self.column_index, op: FilterOp::Le, value: val.into_value() }
    }
    pub fn gt(&self, val: impl IntoValue) -> Filter {
        Filter { column_index: self.column_index, op: FilterOp::Gt, value: val.into_value() }
    }
    pub fn ge(&self, val: impl IntoValue) -> Filter {
        Filter { column_index: self.column_index, op: FilterOp::Ge, value: val.into_value() }
    }
}
```

- [ ] **Step 3: Export from lib.rs**

Add to `crates/spinstack-store/src/lib.rs`:
```rust
pub mod descriptor;
pub mod field;
pub use descriptor::{QueryDescriptor, Filter, AggregateFunc};
pub use field::{Field, IntoValue};
```

- [ ] **Step 4: Verify and commit**

Run: `cargo check -p spinstack-store`

```bash
git add crates/spinstack-store/
git commit -m "feat(spinstack-store): QueryDescriptor + typed Field accessors"
```

---

### Task 4: Query builder

**Files:**
- Create: `crates/spinstack-store/src/query.rs`
- Modify: `crates/spinstack-store/src/lib.rs`
- Create: `crates/spinstack-store/tests/query_test.rs`

- [ ] **Step 1: Write test**

`crates/spinstack-store/tests/query_test.rs`:
```rust
use spinstack_store::*;
use spinstack_store::descriptor::QueryDescriptor;

// Manual StoreRecord impl (derive macro not built yet)
struct Post;
impl Post {
    const ID: Field<u64> = Field::new(0);
    const AUTHOR_ID: Field<u64> = Field::new(1);
    const CREATED_AT: Field<i64> = Field::new(3);
}

#[test]
fn test_scan_with_filter_and_limit() {
    let desc = Query::<Post>::new("posts")
        .filter(Post::AUTHOR_ID.eq(42u64))
        .order_by(Post::CREATED_AT, Direction::Desc)
        .limit(20)
        .build();

    match desc {
        QueryDescriptor::Scan {
            table_name,
            filters,
            order_by,
            limit,
            ..
        } => {
            assert_eq!(table_name, "posts");
            assert_eq!(filters.len(), 1);
            assert_eq!(filters[0].column_index, 1);
            assert_eq!(filters[0].op, FilterOp::Eq);
            assert_eq!(filters[0].value, Value::U64(42));
            assert_eq!(order_by, Some((3, Direction::Desc)));
            assert_eq!(limit, Some(20));
        }
        _ => panic!("expected Scan descriptor"),
    }
}

#[test]
fn test_aggregate_count() {
    let desc = Query::<Post>::new("posts")
        .filter(Post::AUTHOR_ID.eq(5u64))
        .build_count();

    match desc {
        QueryDescriptor::Aggregate {
            table_name,
            filters,
            func,
            column,
        } => {
            assert_eq!(table_name, "posts");
            assert_eq!(filters.len(), 1);
            assert_eq!(func, AggregateFunc::Count);
            assert!(column.is_none());
        }
        _ => panic!("expected Aggregate descriptor"),
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spinstack-store --test query_test`
Expected: FAIL (Query type not found)

- [ ] **Step 3: Implement Query builder**

`crates/spinstack-store/src/query.rs`:
```rust
use std::marker::PhantomData;

use crate::descriptor::{AggregateFunc, Filter, QueryDescriptor};
use crate::field::Field;
use crate::types::Direction;

/// A query builder that accumulates filters, ordering, and limits,
/// then produces a QueryDescriptor.
pub struct Query<T> {
    table_name: &'static str,
    filters: Vec<Filter>,
    order_by: Option<(u16, Direction)>,
    limit: Option<u32>,
    offset: Option<u32>,
    _phantom: PhantomData<T>,
}

impl<T> Query<T> {
    pub fn new(table_name: &'static str) -> Self {
        Self {
            table_name,
            filters: Vec::new(),
            order_by: None,
            limit: None,
            offset: None,
            _phantom: PhantomData,
        }
    }

    pub fn filter(mut self, f: Filter) -> Self {
        self.filters.push(f);
        self
    }

    pub fn order_by<F>(mut self, _field: Field<F>, dir: Direction) -> Self {
        self.order_by = Some((_field.column_index, dir));
        self
    }

    pub fn limit(mut self, n: u32) -> Self {
        self.limit = Some(n);
        self
    }

    pub fn offset(mut self, n: u32) -> Self {
        self.offset = Some(n);
        self
    }

    /// Build a Scan descriptor.
    pub fn build(self) -> QueryDescriptor {
        QueryDescriptor::Scan {
            table_name: self.table_name,
            filters: self.filters,
            order_by: self.order_by,
            limit: self.limit,
            offset: self.offset,
        }
    }

    /// Build a COUNT aggregate descriptor.
    pub fn build_count(self) -> QueryDescriptor {
        QueryDescriptor::Aggregate {
            table_name: self.table_name,
            filters: self.filters,
            func: AggregateFunc::Count,
            column: None,
        }
    }
}
```

- [ ] **Step 4: Export from lib.rs**

Add to `crates/spinstack-store/src/lib.rs`:
```rust
pub mod query;
pub use query::Query;
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p spinstack-store --test query_test`
Expected: all pass

- [ ] **Step 6: Commit**

```bash
git add crates/spinstack-store/
git commit -m "feat(spinstack-store): Query builder produces QueryDescriptors"
```

---

### Task 5: Row serialization

**Files:**
- Create: `crates/spinstack-store/src/serialize.rs`
- Modify: `crates/spinstack-store/src/lib.rs`
- Create: `crates/spinstack-store/tests/serialize_test.rs`

- [ ] **Step 1: Write test**

`crates/spinstack-store/tests/serialize_test.rs`:
```rust
use spinstack_store::serialize::*;

#[test]
fn test_u64_round_trip() {
    let mut buf = Vec::new();
    encode_u64(&mut buf, 42);
    let (val, consumed) = decode_u64(&buf).unwrap();
    assert_eq!(val, 42);
    assert_eq!(consumed, 8);
}

#[test]
fn test_string_round_trip() {
    let mut buf = Vec::new();
    encode_string(&mut buf, "hello world");
    let (val, consumed) = decode_string(&buf).unwrap();
    assert_eq!(val, "hello world");
    assert_eq!(consumed, 4 + 11); // length prefix + bytes
}

#[test]
fn test_mixed_row() {
    let mut buf = Vec::new();
    encode_u64(&mut buf, 1);
    encode_u64(&mut buf, 99);
    encode_string(&mut buf, "test post");
    encode_i64(&mut buf, 1234567890);

    let mut offset = 0;
    let (id, n) = decode_u64(&buf[offset..]).unwrap();
    offset += n;
    let (author, n) = decode_u64(&buf[offset..]).unwrap();
    offset += n;
    let (content, n) = decode_string(&buf[offset..]).unwrap();
    offset += n;
    let (ts, _) = decode_i64(&buf[offset..]).unwrap();

    assert_eq!(id, 1);
    assert_eq!(author, 99);
    assert_eq!(content, "test post");
    assert_eq!(ts, 1234567890);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spinstack-store --test serialize_test`
Expected: FAIL

- [ ] **Step 3: Implement serialization primitives**

`crates/spinstack-store/src/serialize.rs`:
```rust
use crate::error::{StoreError, Result};

pub fn encode_u64(buf: &mut Vec<u8>, val: u64) {
    buf.extend_from_slice(&val.to_le_bytes());
}

pub fn decode_u64(buf: &[u8]) -> Result<(u64, usize)> {
    if buf.len() < 8 {
        return Err(StoreError::SerializationError("truncated u64".into()));
    }
    Ok((u64::from_le_bytes(buf[..8].try_into().unwrap()), 8))
}

pub fn encode_i64(buf: &mut Vec<u8>, val: i64) {
    buf.extend_from_slice(&val.to_le_bytes());
}

pub fn decode_i64(buf: &[u8]) -> Result<(i64, usize)> {
    if buf.len() < 8 {
        return Err(StoreError::SerializationError("truncated i64".into()));
    }
    Ok((i64::from_le_bytes(buf[..8].try_into().unwrap()), 8))
}

pub fn encode_f64(buf: &mut Vec<u8>, val: f64) {
    buf.extend_from_slice(&val.to_le_bytes());
}

pub fn decode_f64(buf: &[u8]) -> Result<(f64, usize)> {
    if buf.len() < 8 {
        return Err(StoreError::SerializationError("truncated f64".into()));
    }
    Ok((f64::from_le_bytes(buf[..8].try_into().unwrap()), 8))
}

pub fn encode_bool(buf: &mut Vec<u8>, val: bool) {
    buf.push(if val { 1 } else { 0 });
}

pub fn decode_bool(buf: &[u8]) -> Result<(bool, usize)> {
    if buf.is_empty() {
        return Err(StoreError::SerializationError("truncated bool".into()));
    }
    Ok((buf[0] != 0, 1))
}

pub fn encode_string(buf: &mut Vec<u8>, val: &str) {
    let bytes = val.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

pub fn decode_string(buf: &[u8]) -> Result<(String, usize)> {
    if buf.len() < 4 {
        return Err(StoreError::SerializationError("truncated string length".into()));
    }
    let len = u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize;
    if buf.len() < 4 + len {
        return Err(StoreError::SerializationError("truncated string data".into()));
    }
    let s = String::from_utf8(buf[4..4 + len].to_vec())
        .map_err(|e| StoreError::SerializationError(format!("invalid utf8: {e}")))?;
    Ok((s, 4 + len))
}

pub fn encode_bytes(buf: &mut Vec<u8>, val: &[u8]) {
    buf.extend_from_slice(&(val.len() as u32).to_le_bytes());
    buf.extend_from_slice(val);
}

pub fn decode_bytes(buf: &[u8]) -> Result<(Vec<u8>, usize)> {
    if buf.len() < 4 {
        return Err(StoreError::SerializationError("truncated bytes length".into()));
    }
    let len = u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize;
    if buf.len() < 4 + len {
        return Err(StoreError::SerializationError("truncated bytes data".into()));
    }
    Ok((buf[4..4 + len].to_vec(), 4 + len))
}
```

- [ ] **Step 4: Export from lib.rs**

Add to `crates/spinstack-store/src/lib.rs`:
```rust
pub mod serialize;
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p spinstack-store --test serialize_test`
Expected: all pass

- [ ] **Step 6: Commit**

```bash
git add crates/spinstack-store/
git commit -m "feat(spinstack-store): row serialization primitives"
```

---

### Task 6: Store trait + Transaction handle

**Files:**
- Create: `crates/spinstack-store/src/store.rs`
- Create: `crates/spinstack-store/src/transaction.rs`
- Modify: `crates/spinstack-store/src/lib.rs`

- [ ] **Step 1: Create Store trait**

`crates/spinstack-store/src/store.rs`:
```rust
use crate::descriptor::QueryDescriptor;
use crate::error::Result;
use crate::query::Query;
use crate::schema::StoreRecord;

/// The main store interface. Backend implementations (manifold, sqlite)
/// implement this trait. The SDK provides the query builder; the backend
/// executes the descriptors.
pub trait Store {
    /// Get a record by primary key.
    fn get<T: StoreRecord>(&self, key: impl Into<u64>) -> Result<Option<T>>;

    /// Insert a record.
    fn insert<T: StoreRecord>(&self, record: &T) -> Result<()>;

    /// Update a record (by primary key).
    fn update<T: StoreRecord>(&self, record: &T) -> Result<()>;

    /// Delete a record by primary key.
    fn delete<T: StoreRecord>(&self, key: impl Into<u64>) -> Result<()>;

    /// Execute a scan query and return matching records.
    fn fetch<T: StoreRecord>(&self, query: Query<T>) -> Result<Vec<T>>;

    /// Execute a COUNT query.
    fn count<T: StoreRecord>(&self, query: Query<T>) -> Result<u64>;

    /// Execute a query descriptor directly (for advanced use).
    fn execute_descriptor(&self, desc: QueryDescriptor) -> Result<Vec<Vec<u8>>>;

    /// Run a multi-operation transaction atomically.
    fn transaction<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&dyn TransactionOps) -> Result<R>;
}

/// Operations available inside a transaction closure.
pub trait TransactionOps {
    fn get<T: StoreRecord>(&self, key: impl Into<u64>) -> Result<Option<T>>;
    fn insert<T: StoreRecord>(&self, record: &T) -> Result<()>;
    fn update<T: StoreRecord>(&self, record: &T) -> Result<()>;
    fn delete<T: StoreRecord>(&self, key: impl Into<u64>) -> Result<()>;
}
```

- [ ] **Step 2: Export from lib.rs**

Add to `crates/spinstack-store/src/lib.rs`:
```rust
pub mod store;
pub use store::{Store, TransactionOps};
```

- [ ] **Step 3: Verify and commit**

Run: `cargo check -p spinstack-store`

```bash
git add crates/spinstack-store/
git commit -m "feat(spinstack-store): Store trait + TransactionOps"
```

---

### Task 7: Derive macro crate

**Files:**
- Create: `crates/spinstack-store-macros/Cargo.toml`
- Create: `crates/spinstack-store-macros/src/lib.rs`
- Modify: `Cargo.toml` (workspace members)
- Modify: `crates/spinstack-store/Cargo.toml` (add dependency)
- Create: `crates/spinstack-store/tests/derive_test.rs`

- [ ] **Step 1: Create proc-macro crate**

`crates/spinstack-store-macros/Cargo.toml`:
```toml
[package]
name = "spinstack-store-macros"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[lib]
proc-macro = true

[dependencies]
syn = { version = "2", features = ["full"] }
quote = "1"
proc-macro2 = "1"
```

Add `"crates/spinstack-store-macros"` to root `Cargo.toml` workspace members.

Add to `crates/spinstack-store/Cargo.toml` dependencies:
```toml
spinstack-store-macros = { path = "../spinstack-store-macros" }
```

- [ ] **Step 2: Write derive test**

`crates/spinstack-store/tests/derive_test.rs`:
```rust
use spinstack_store::*;
use spinstack_store::schema::StoreRecord;

#[derive(Store)]
struct Post {
    #[primary_key]
    id: u64,
    #[indexed]
    author_id: u64,
    content: String,
    created_at: i64,
}

#[test]
fn test_table_name() {
    assert_eq!(Post::TABLE_NAME, "post");
}

#[test]
fn test_schema() {
    let schema = Post::schema();
    assert_eq!(schema.table_name, "post");
    assert_eq!(schema.columns.len(), 4);
    assert!(schema.columns[0].is_primary_key);
    assert_eq!(schema.columns[0].name, "id");
    assert!(schema.columns[1].is_indexed);
    assert_eq!(schema.columns[1].name, "author_id");
}

#[test]
fn test_field_accessors() {
    assert_eq!(Post::ID.column_index, 0);
    assert_eq!(Post::AUTHOR_ID.column_index, 1);
    assert_eq!(Post::CONTENT.column_index, 2);
    assert_eq!(Post::CREATED_AT.column_index, 3);
}

#[test]
fn test_serialization_round_trip() {
    let post = Post {
        id: 42,
        author_id: 7,
        content: "hello world".to_string(),
        created_at: 1234567890,
    };
    let bytes = post.to_store_bytes();
    let decoded = Post::from_store_bytes(&bytes).unwrap();
    assert_eq!(decoded.id, 42);
    assert_eq!(decoded.author_id, 7);
    assert_eq!(decoded.content, "hello world");
    assert_eq!(decoded.created_at, 1234567890);
}

#[test]
fn test_primary_key_bytes() {
    let post = Post {
        id: 42,
        author_id: 7,
        content: "test".to_string(),
        created_at: 0,
    };
    let key_bytes = post.primary_key_bytes();
    assert_eq!(key_bytes, 42u64.to_le_bytes().to_vec());
}

#[test]
fn test_filter_with_field() {
    let filter = Post::AUTHOR_ID.eq(42u64);
    assert_eq!(filter.column_index, 1);
    assert_eq!(filter.op, FilterOp::Eq);
    assert_eq!(filter.value, Value::U64(42));
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p spinstack-store --test derive_test`
Expected: FAIL (derive macro not implemented)

- [ ] **Step 4: Implement derive macro**

`crates/spinstack-store-macros/src/lib.rs`:
```rust
use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, Data, Fields, Ident};

#[proc_macro_derive(Store, attributes(primary_key, indexed, store))]
pub fn derive_store(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let table_name = name.to_string().to_lowercase();

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => panic!("Store derive only supports named fields"),
        },
        _ => panic!("Store derive only supports structs"),
    };

    let mut column_defs = Vec::new();
    let mut field_consts = Vec::new();
    let mut encode_steps = Vec::new();
    let mut decode_steps = Vec::new();
    let mut field_names = Vec::new();
    let mut pk_field: Option<Ident> = None;

    for (i, field) in fields.iter().enumerate() {
        let field_name = field.ident.as_ref().unwrap();
        let field_name_str = field_name.to_string();
        let const_name = Ident::new(&field_name_str.to_uppercase(), field_name.span());
        let idx = i as u16;

        let is_pk = field.attrs.iter().any(|a| a.path().is_ident("primary_key"));
        let is_indexed = field.attrs.iter().any(|a| a.path().is_ident("indexed"));

        if is_pk {
            pk_field = Some(field_name.clone());
        }

        let ty = &field.ty;
        let ty_str = quote!(#ty).to_string();

        let (col_type, encode, decode) = match ty_str.as_str() {
            "u64" => (
                quote!(spinstack_store::ColumnType::U64),
                quote!(spinstack_store::serialize::encode_u64(&mut buf, self.#field_name);),
                quote!({
                    let (v, n) = spinstack_store::serialize::decode_u64(&bytes[offset..])?;
                    offset += n;
                    v
                }),
            ),
            "i64" => (
                quote!(spinstack_store::ColumnType::I64),
                quote!(spinstack_store::serialize::encode_i64(&mut buf, self.#field_name);),
                quote!({
                    let (v, n) = spinstack_store::serialize::decode_i64(&bytes[offset..])?;
                    offset += n;
                    v
                }),
            ),
            "f64" => (
                quote!(spinstack_store::ColumnType::F64),
                quote!(spinstack_store::serialize::encode_f64(&mut buf, self.#field_name);),
                quote!({
                    let (v, n) = spinstack_store::serialize::decode_f64(&bytes[offset..])?;
                    offset += n;
                    v
                }),
            ),
            "String" => (
                quote!(spinstack_store::ColumnType::String),
                quote!(spinstack_store::serialize::encode_string(&mut buf, &self.#field_name);),
                quote!({
                    let (v, n) = spinstack_store::serialize::decode_string(&bytes[offset..])?;
                    offset += n;
                    v
                }),
            ),
            "bool" => (
                quote!(spinstack_store::ColumnType::Bool),
                quote!(spinstack_store::serialize::encode_bool(&mut buf, self.#field_name);),
                quote!({
                    let (v, n) = spinstack_store::serialize::decode_bool(&bytes[offset..])?;
                    offset += n;
                    v
                }),
            ),
            _ => panic!("Unsupported field type: {ty_str}. Store supports: u64, i64, f64, String, bool"),
        };

        column_defs.push(quote! {
            spinstack_store::schema::ColumnDef {
                name: #field_name_str,
                column_type: #col_type,
                is_primary_key: #is_pk,
                is_indexed: #is_indexed,
            }
        });

        field_consts.push(quote! {
            pub const #const_name: spinstack_store::Field<#ty> = spinstack_store::Field::new(#idx);
        });

        encode_steps.push(encode);
        decode_steps.push(decode);
        field_names.push(field_name.clone());
    }

    let pk_encode = pk_field.as_ref().map(|pk| {
        quote! {
            fn primary_key_bytes(&self) -> Vec<u8> {
                let mut buf = Vec::new();
                spinstack_store::serialize::encode_u64(&mut buf, self.#pk);
                buf
            }
        }
    }).unwrap_or_else(|| {
        panic!("Store derive requires exactly one #[primary_key] field");
    });

    let num_columns = fields.len();

    let expanded = quote! {
        impl #name {
            #(#field_consts)*
        }

        impl spinstack_store::schema::StoreRecord for #name {
            const TABLE_NAME: &'static str = #table_name;

            fn schema() -> spinstack_store::schema::TableSchema {
                static COLUMNS: [spinstack_store::schema::ColumnDef; #num_columns] = [
                    #(#column_defs),*
                ];
                spinstack_store::schema::TableSchema {
                    table_name: #table_name,
                    columns: &COLUMNS,
                }
            }

            fn to_store_bytes(&self) -> Vec<u8> {
                let mut buf = Vec::new();
                #(#encode_steps)*
                buf
            }

            fn from_store_bytes(bytes: &[u8]) -> spinstack_store::Result<Self> {
                let mut offset = 0;
                #(let #field_names = #decode_steps;)*
                Ok(Self { #(#field_names),* })
            }

            #pk_encode
        }
    };

    TokenStream::from(expanded)
}
```

- [ ] **Step 5: Re-export derive macro from SDK crate**

In `crates/spinstack-store/src/lib.rs`, add:
```rust
pub use spinstack_store_macros::Store;
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p spinstack-store --test derive_test`
Expected: all 6 tests pass

- [ ] **Step 7: Commit**

```bash
git add crates/spinstack-store-macros/ crates/spinstack-store/ Cargo.toml
git commit -m "feat(spinstack-store): derive Store macro with schema, serialization, field accessors"
```

---

### Task 8: Integration test — full query builder with derive

**Files:**
- Create: `crates/spinstack-store/tests/integration_test.rs`

- [ ] **Step 1: Write integration test**

`crates/spinstack-store/tests/integration_test.rs`:
```rust
use spinstack_store::*;
use spinstack_store::descriptor::QueryDescriptor;
use spinstack_store::schema::StoreRecord;

#[derive(Store, Debug, PartialEq)]
struct User {
    #[primary_key]
    id: u64,
    username: String,
    active: bool,
}

#[derive(Store, Debug, PartialEq)]
struct Comment {
    #[primary_key]
    id: u64,
    #[indexed]
    post_id: u64,
    author_id: u64,
    body: String,
}

#[test]
fn test_query_with_derived_struct() {
    let desc = Query::<Comment>::new(Comment::TABLE_NAME)
        .filter(Comment::POST_ID.eq(42u64))
        .limit(10)
        .build();

    if let QueryDescriptor::Scan { table_name, filters, limit, .. } = desc {
        assert_eq!(table_name, "comment");
        assert_eq!(filters[0].value, Value::U64(42));
        assert_eq!(limit, Some(10));
    } else {
        panic!("expected Scan");
    }
}

#[test]
fn test_serialize_and_query() {
    let user = User {
        id: 1,
        username: "alice".to_string(),
        active: true,
    };

    let bytes = user.to_store_bytes();
    let decoded = User::from_store_bytes(&bytes).unwrap();
    assert_eq!(user, decoded);

    // The derived schema should have correct column info
    let schema = User::schema();
    assert_eq!(schema.columns[0].name, "id");
    assert!(schema.columns[0].is_primary_key);
    assert_eq!(schema.columns[1].column_type, ColumnType::String);
    assert_eq!(schema.columns[2].column_type, ColumnType::Bool);
}

#[test]
fn test_transaction_descriptor() {
    let user = User { id: 1, username: "bob".to_string(), active: true };

    let desc = QueryDescriptor::Transaction {
        ops: vec![
            QueryDescriptor::Insert {
                table_name: User::TABLE_NAME,
                key_bytes: user.primary_key_bytes(),
                row_bytes: user.to_store_bytes(),
            },
            QueryDescriptor::Delete {
                table_name: "comment",
                key_bytes: 42u64.to_le_bytes().to_vec(),
            },
        ],
    };

    if let QueryDescriptor::Transaction { ops } = desc {
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], QueryDescriptor::Insert { .. }));
        assert!(matches!(ops[1], QueryDescriptor::Delete { .. }));
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spinstack-store`
Expected: all tests pass (derive_test + query_test + serialize_test + integration_test)

- [ ] **Step 3: Commit**

```bash
git add crates/spinstack-store/tests/integration_test.rs
git commit -m "test(spinstack-store): integration tests — derive + query builder + serialization"
```
