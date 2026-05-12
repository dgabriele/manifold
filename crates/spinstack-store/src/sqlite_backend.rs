use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use crate::descriptor::{AggregateFunc, Filter, QueryDescriptor};
use crate::error::{Result, StoreError};
use crate::query::Query;
use crate::schema::StoreRecord;
use crate::store::{Store, TransactionOps};
use crate::types::{Direction, FilterOp, Value};

pub struct SqliteStore {
    conn: Mutex<Connection>,
}

fn map_sqlite_err(e: rusqlite::Error) -> StoreError {
    StoreError::BackendError(format!("sqlite error: {e}"))
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(map_sqlite_err)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .map_err(map_sqlite_err)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Create table for a record type if it doesn't exist.
    pub fn ensure_schema<T: StoreRecord>(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let table = T::TABLE_NAME;
        conn.execute(
            &format!(
                "CREATE TABLE IF NOT EXISTS [{table}] (id INTEGER PRIMARY KEY, data BLOB NOT NULL)"
            ),
            [],
        )
        .map_err(map_sqlite_err)?;
        Ok(())
    }

    fn decode_column_value(
        row_bytes: &[u8],
        schema: &crate::schema::TableSchema,
        col_index: usize,
    ) -> Result<Value> {
        use crate::serialize::*;
        use crate::types::ColumnType;

        let mut offset = 0;
        for (i, col) in schema.columns.iter().enumerate() {
            if i == col_index {
                return match col.column_type {
                    ColumnType::U64 => {
                        let (v, _) = decode_u64(&row_bytes[offset..])?;
                        Ok(Value::U64(v))
                    }
                    ColumnType::I64 => {
                        let (v, _) = decode_i64(&row_bytes[offset..])?;
                        Ok(Value::I64(v))
                    }
                    ColumnType::F64 => {
                        let (v, _) = decode_f64(&row_bytes[offset..])?;
                        Ok(Value::F64(v))
                    }
                    ColumnType::String => {
                        let (v, _) = decode_string(&row_bytes[offset..])?;
                        Ok(Value::String(v))
                    }
                    ColumnType::Bool => {
                        let (v, _) = decode_bool(&row_bytes[offset..])?;
                        Ok(Value::Bool(v))
                    }
                    ColumnType::Bytes => {
                        let (v, _) = decode_bytes(&row_bytes[offset..])?;
                        Ok(Value::Bytes(v))
                    }
                };
            }
            match col.column_type {
                ColumnType::U64 | ColumnType::I64 | ColumnType::F64 => offset += 8,
                ColumnType::Bool => offset += 1,
                ColumnType::String | ColumnType::Bytes => {
                    if row_bytes.len() < offset + 4 {
                        return Err(StoreError::SerializationError("truncated".into()));
                    }
                    let len = u32::from_le_bytes(
                        row_bytes[offset..offset + 4].try_into().unwrap(),
                    ) as usize;
                    offset += 4 + len;
                }
            }
        }
        Err(StoreError::SchemaError(format!(
            "column index {col_index} out of range"
        )))
    }

    fn matches_filters(
        row_bytes: &[u8],
        key: u64,
        schema: &crate::schema::TableSchema,
        filters: &[Filter],
    ) -> Result<bool> {
        for filter in filters {
            let col_idx = filter.column_index as usize;
            let actual = if col_idx == 0 && schema.columns[0].is_primary_key {
                Value::U64(key)
            } else {
                Self::decode_column_value(row_bytes, schema, col_idx)?
            };

            if !compare_values(&actual, &filter.op, &filter.value) {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn compare_values(actual: &Value, op: &FilterOp, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::U64(a), Value::U64(b)) => compare_ord(a, op, b),
        (Value::I64(a), Value::I64(b)) => compare_ord(a, op, b),
        (Value::F64(a), Value::F64(b)) => compare_f64(*a, op, *b),
        (Value::String(a), Value::String(b)) => compare_ord(a, op, b),
        (Value::Bool(a), Value::Bool(b)) => compare_ord(a, op, b),
        _ => false,
    }
}

fn compare_ord<T: Ord>(a: &T, op: &FilterOp, b: &T) -> bool {
    match op {
        FilterOp::Eq => a == b,
        FilterOp::Ne => a != b,
        FilterOp::Lt => a < b,
        FilterOp::Le => a <= b,
        FilterOp::Gt => a > b,
        FilterOp::Ge => a >= b,
    }
}

fn compare_f64(a: f64, op: &FilterOp, b: f64) -> bool {
    match op {
        FilterOp::Eq => (a - b).abs() < f64::EPSILON,
        FilterOp::Ne => (a - b).abs() >= f64::EPSILON,
        FilterOp::Lt => a < b,
        FilterOp::Le => a <= b,
        FilterOp::Gt => a > b,
        FilterOp::Ge => a >= b,
    }
}

impl Store for SqliteStore {
    type Txn<'a> = SqliteTxnOps<'a>;

    fn get<T: StoreRecord>(&self, key: impl Into<u64>) -> Result<Option<T>> {
        let key = key.into();
        let table = T::TABLE_NAME;
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached(&format!("SELECT data FROM [{table}] WHERE id = ?1"))
            .map_err(map_sqlite_err)?;
        let result = stmt
            .query_row(rusqlite::params![key as i64], |row| {
                let blob: Vec<u8> = row.get(0)?;
                Ok(blob)
            });
        match result {
            Ok(blob) => Ok(Some(T::from_store_bytes(&blob)?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(map_sqlite_err(e)),
        }
    }

    fn insert<T: StoreRecord>(&self, record: &T) -> Result<()> {
        let table = T::TABLE_NAME;
        let key_bytes = record.primary_key_bytes();
        let key = u64::from_le_bytes(key_bytes[..8].try_into().unwrap());
        let row_bytes = record.to_store_bytes();
        let conn = self.conn.lock().unwrap();

        // Check for duplicate
        let exists: bool = conn
            .prepare_cached(&format!("SELECT 1 FROM [{table}] WHERE id = ?1"))
            .map_err(map_sqlite_err)?
            .exists(rusqlite::params![key as i64])
            .map_err(map_sqlite_err)?;
        if exists {
            return Err(StoreError::DuplicateKey);
        }

        conn.prepare_cached(&format!(
            "INSERT INTO [{table}] (id, data) VALUES (?1, ?2)"
        ))
        .map_err(map_sqlite_err)?
        .execute(rusqlite::params![key as i64, row_bytes])
        .map_err(map_sqlite_err)?;
        Ok(())
    }

    fn update<T: StoreRecord>(&self, record: &T) -> Result<()> {
        let table = T::TABLE_NAME;
        let key_bytes = record.primary_key_bytes();
        let key = u64::from_le_bytes(key_bytes[..8].try_into().unwrap());
        let row_bytes = record.to_store_bytes();
        let conn = self.conn.lock().unwrap();

        let changed = conn
            .prepare_cached(&format!(
                "UPDATE [{table}] SET data = ?2 WHERE id = ?1"
            ))
            .map_err(map_sqlite_err)?
            .execute(rusqlite::params![key as i64, row_bytes])
            .map_err(map_sqlite_err)?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    fn delete<T: StoreRecord>(&self, key: impl Into<u64>) -> Result<()> {
        let key = key.into();
        let table = T::TABLE_NAME;
        let conn = self.conn.lock().unwrap();
        conn.prepare_cached(&format!("DELETE FROM [{table}] WHERE id = ?1"))
            .map_err(map_sqlite_err)?
            .execute(rusqlite::params![key as i64])
            .map_err(map_sqlite_err)?;
        Ok(())
    }

    fn fetch<T: StoreRecord>(&self, query: Query<T>) -> Result<Vec<T>> {
        let desc = query.build();
        match desc {
            QueryDescriptor::Scan {
                filters,
                order_by,
                limit,
                offset,
                ..
            } => {
                let schema = T::schema();
                let table = T::TABLE_NAME;
                let conn = self.conn.lock().unwrap();

                let mut stmt = conn
                    .prepare_cached(&format!("SELECT id, data FROM [{table}] ORDER BY id ASC"))
                    .map_err(map_sqlite_err)?;
                let rows = stmt
                    .query_map([], |row| {
                        let id: i64 = row.get(0)?;
                        let data: Vec<u8> = row.get(1)?;
                        Ok((id as u64, data))
                    })
                    .map_err(map_sqlite_err)?;

                let mut matched: Vec<(u64, Vec<u8>)> = Vec::new();
                for row_result in rows {
                    let (key, bytes) = row_result.map_err(map_sqlite_err)?;
                    if Self::matches_filters(&bytes, key, &schema, &filters)? {
                        matched.push((key, bytes));
                    }
                }

                // Handle DESC ordering on PK
                if matches!(order_by, Some((0, Direction::Desc))) {
                    matched.reverse();
                }

                let skip = offset.unwrap_or(0) as usize;
                let take = limit.unwrap_or(u32::MAX) as usize;
                let mut results = Vec::new();
                for (_, bytes) in matched.into_iter().skip(skip).take(take) {
                    results.push(T::from_store_bytes(&bytes)?);
                }
                Ok(results)
            }
            _ => Err(StoreError::BackendError(
                "fetch only supports Scan descriptors".into(),
            )),
        }
    }

    fn count<T: StoreRecord>(&self, query: Query<T>) -> Result<u64> {
        let desc = query.build_count();
        match desc {
            QueryDescriptor::Aggregate {
                filters,
                func: AggregateFunc::Count,
                ..
            } => {
                let table = T::TABLE_NAME;
                let conn = self.conn.lock().unwrap();

                if filters.is_empty() {
                    let count: i64 = conn
                        .prepare_cached(&format!("SELECT COUNT(*) FROM [{table}]"))
                        .map_err(map_sqlite_err)?
                        .query_row([], |row| row.get(0))
                        .map_err(map_sqlite_err)?;
                    return Ok(count as u64);
                }

                let schema = T::schema();
                let mut stmt = conn
                    .prepare_cached(&format!("SELECT id, data FROM [{table}]"))
                    .map_err(map_sqlite_err)?;
                let rows = stmt
                    .query_map([], |row| {
                        let id: i64 = row.get(0)?;
                        let data: Vec<u8> = row.get(1)?;
                        Ok((id as u64, data))
                    })
                    .map_err(map_sqlite_err)?;

                let mut count = 0u64;
                for row_result in rows {
                    let (key, bytes) = row_result.map_err(map_sqlite_err)?;
                    if Self::matches_filters(&bytes, key, &schema, &filters)? {
                        count += 1;
                    }
                }
                Ok(count)
            }
            _ => Err(StoreError::BackendError(
                "count only supports Count aggregate".into(),
            )),
        }
    }

    fn execute_descriptor(&self, _desc: QueryDescriptor) -> Result<Vec<Vec<u8>>> {
        Err(StoreError::BackendError(
            "execute_descriptor not yet implemented".into(),
        ))
    }

    fn transaction<F, R>(&self, f: F) -> Result<R>
    where
        F: for<'a> FnOnce(&SqliteTxnOps<'a>) -> Result<R>,
    {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch("BEGIN").map_err(map_sqlite_err)?;
        let ops = SqliteTxnOps { conn: &conn };
        match f(&ops) {
            Ok(result) => {
                conn.execute_batch("COMMIT").map_err(map_sqlite_err)?;
                Ok(result)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }
}

pub struct SqliteTxnOps<'a> {
    conn: &'a Connection,
}

impl TransactionOps for SqliteTxnOps<'_> {
    fn get<T: StoreRecord>(&self, key: impl Into<u64>) -> Result<Option<T>> {
        let key = key.into();
        let table = T::TABLE_NAME;
        let mut stmt = self
            .conn
            .prepare_cached(&format!("SELECT data FROM [{table}] WHERE id = ?1"))
            .map_err(map_sqlite_err)?;
        let result = stmt.query_row(rusqlite::params![key as i64], |row| {
            let blob: Vec<u8> = row.get(0)?;
            Ok(blob)
        });
        match result {
            Ok(blob) => Ok(Some(T::from_store_bytes(&blob)?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(map_sqlite_err(e)),
        }
    }

    fn insert<T: StoreRecord>(&self, record: &T) -> Result<()> {
        let table = T::TABLE_NAME;
        let key_bytes = record.primary_key_bytes();
        let key = u64::from_le_bytes(key_bytes[..8].try_into().unwrap());
        let row_bytes = record.to_store_bytes();
        self.conn
            .prepare_cached(&format!(
                "INSERT INTO [{table}] (id, data) VALUES (?1, ?2)"
            ))
            .map_err(map_sqlite_err)?
            .execute(rusqlite::params![key as i64, row_bytes])
            .map_err(map_sqlite_err)?;
        Ok(())
    }

    fn update<T: StoreRecord>(&self, record: &T) -> Result<()> {
        let table = T::TABLE_NAME;
        let key_bytes = record.primary_key_bytes();
        let key = u64::from_le_bytes(key_bytes[..8].try_into().unwrap());
        let row_bytes = record.to_store_bytes();
        let changed = self
            .conn
            .prepare_cached(&format!(
                "UPDATE [{table}] SET data = ?2 WHERE id = ?1"
            ))
            .map_err(map_sqlite_err)?
            .execute(rusqlite::params![key as i64, row_bytes])
            .map_err(map_sqlite_err)?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    fn delete<T: StoreRecord>(&self, key: impl Into<u64>) -> Result<()> {
        let key = key.into();
        let table = T::TABLE_NAME;
        self.conn
            .prepare_cached(&format!("DELETE FROM [{table}] WHERE id = ?1"))
            .map_err(map_sqlite_err)?
            .execute(rusqlite::params![key as i64])
            .map_err(map_sqlite_err)?;
        Ok(())
    }
}
