use manifold::column_family::ColumnFamily;
use manifold::{ReadableTable, ReadableTableMetadata, TableDefinition};

use crate::descriptor::{AggregateFunc, Filter, QueryDescriptor};
use crate::error::{Result, StoreError};
use crate::query::Query;
use crate::schema::{StoreRecord, TableSchema};
use crate::store::{Store, TransactionOps};
use crate::types::{Direction, FilterOp, Value};

pub struct ManifoldStore {
    cf: ColumnFamily,
}

impl ManifoldStore {
    pub fn new(cf: ColumnFamily) -> Self {
        Self { cf }
    }

    /// Create tables and indexes for a record type if they don't exist.
    pub fn ensure_schema<T: StoreRecord>(&self) -> Result<()> {
        let schema = T::schema();
        let data_table_name = Self::data_table_name(schema.table_name);

        // Create data table
        let txn = self.cf.begin_write().map_err(map_txn_err)?;
        {
            let _table = txn
                .open_table(TableDefinition::<u64, &[u8]>::new(data_table_name))
                .map_err(map_table_err)?;
        }

        // Create index tables for indexed columns
        for col in schema.columns {
            if col.is_indexed && !col.is_primary_key {
                let idx_name = Self::index_table_name(schema.table_name, col.name);
                // Non-unique index: multiple rowids per key
                let _table = txn
                    .open_table(TableDefinition::<&[u8], &[u8]>::new(idx_name))
                    .map_err(map_table_err)?;
            }
        }

        txn.commit().map_err(map_commit_err)?;
        Ok(())
    }

    fn data_table_name(table_name: &str) -> &'static str {
        Box::leak(format!("data_{table_name}").into_boxed_str())
    }

    fn index_table_name(table_name: &str, col_name: &str) -> &'static str {
        Box::leak(format!("idx_{table_name}_{col_name}").into_boxed_str())
    }

    /// Decode a Value from row bytes at a given column, using the schema to find the offset.
    fn decode_column_value(
        row_bytes: &[u8],
        schema: &TableSchema,
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
            // Skip this column's bytes
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
        schema: &TableSchema,
        filters: &[Filter],
    ) -> Result<bool> {
        for filter in filters {
            let col_idx = filter.column_index as usize;
            let actual = if col_idx == 0 && schema.columns[0].is_primary_key {
                // PK column -- use the key directly
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

fn map_txn_err(e: manifold::TransactionError) -> StoreError {
    StoreError::BackendError(format!("transaction error: {e}"))
}
fn map_table_err(e: manifold::TableError) -> StoreError {
    StoreError::BackendError(format!("table error: {e}"))
}
fn map_commit_err(e: manifold::CommitError) -> StoreError {
    StoreError::BackendError(format!("commit error: {e}"))
}
fn map_storage_err(e: manifold::StorageError) -> StoreError {
    StoreError::BackendError(format!("storage error: {e}"))
}

impl Store for ManifoldStore {
    type Txn<'a> = ManifoldTxnOps<'a>;

    fn get<T: StoreRecord>(&self, key: impl Into<u64>) -> Result<Option<T>> {
        let key = key.into();
        let table_name = Self::data_table_name(T::TABLE_NAME);
        let def = TableDefinition::<u64, &[u8]>::new(table_name);

        let txn = self.cf.begin_read().map_err(map_txn_err)?;
        let table = match txn.open_table(def) {
            Ok(t) => t,
            Err(manifold::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(map_table_err(e)),
        };

        match table.get(key).map_err(map_storage_err)? {
            Some(guard) => Ok(Some(T::from_store_bytes(guard.value())?)),
            None => Ok(None),
        }
    }

    fn insert<T: StoreRecord>(&self, record: &T) -> Result<()> {
        let table_name = Self::data_table_name(T::TABLE_NAME);
        let def = TableDefinition::<u64, &[u8]>::new(table_name);
        let key_bytes = record.primary_key_bytes();
        let key = u64::from_le_bytes(key_bytes[..8].try_into().unwrap());
        let row_bytes = record.to_store_bytes();

        let txn = self.cf.begin_write().map_err(map_txn_err)?;
        {
            let mut table = txn.open_table(def).map_err(map_table_err)?;
            // Check for duplicate
            if table.get(key).map_err(map_storage_err)?.is_some() {
                return Err(StoreError::DuplicateKey);
            }
            table
                .insert(key, row_bytes.as_slice())
                .map_err(map_storage_err)?;
        }
        txn.commit().map_err(map_commit_err)?;
        Ok(())
    }

    fn update<T: StoreRecord>(&self, record: &T) -> Result<()> {
        let table_name = Self::data_table_name(T::TABLE_NAME);
        let def = TableDefinition::<u64, &[u8]>::new(table_name);
        let key_bytes = record.primary_key_bytes();
        let key = u64::from_le_bytes(key_bytes[..8].try_into().unwrap());
        let row_bytes = record.to_store_bytes();

        let txn = self.cf.begin_write().map_err(map_txn_err)?;
        {
            let mut table = txn.open_table(def).map_err(map_table_err)?;
            if table.get(key).map_err(map_storage_err)?.is_none() {
                return Err(StoreError::NotFound);
            }
            table
                .insert(key, row_bytes.as_slice())
                .map_err(map_storage_err)?;
        }
        txn.commit().map_err(map_commit_err)?;
        Ok(())
    }

    fn delete<T: StoreRecord>(&self, key: impl Into<u64>) -> Result<()> {
        let key = key.into();
        let table_name = Self::data_table_name(T::TABLE_NAME);
        let def = TableDefinition::<u64, &[u8]>::new(table_name);

        let txn = self.cf.begin_write().map_err(map_txn_err)?;
        {
            let mut table = txn.open_table(def).map_err(map_table_err)?;
            table.remove(key).map_err(map_storage_err)?;
        }
        txn.commit().map_err(map_commit_err)?;
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
                let table_name = Self::data_table_name(T::TABLE_NAME);
                let def = TableDefinition::<u64, &[u8]>::new(table_name);

                let txn = self.cf.begin_read().map_err(map_txn_err)?;
                let table = match txn.open_table(def) {
                    Ok(t) => t,
                    Err(manifold::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
                    Err(e) => return Err(map_table_err(e)),
                };

                let mut results = Vec::new();

                // If ordering DESC by PK (column 0), reverse iterate
                let reverse = matches!(order_by, Some((0, Direction::Desc)));

                if reverse {
                    let iter = table.iter().map_err(map_storage_err)?;
                    // Collect in forward order, then reverse
                    let mut all: Vec<(u64, Vec<u8>)> = Vec::new();
                    for entry in iter {
                        let (k, v) = entry.map_err(map_storage_err)?;
                        let key = k.value();
                        let bytes = v.value().to_vec();
                        if Self::matches_filters(&bytes, key, &schema, &filters)? {
                            all.push((key, bytes));
                        }
                    }
                    all.reverse();
                    let skip = offset.unwrap_or(0) as usize;
                    let take = limit.unwrap_or(u32::MAX) as usize;
                    for (_, bytes) in all.into_iter().skip(skip).take(take) {
                        results.push(T::from_store_bytes(&bytes)?);
                    }
                } else {
                    let skip = offset.unwrap_or(0) as usize;
                    let take = limit.unwrap_or(u32::MAX) as usize;
                    let mut skipped = 0;
                    let mut taken = 0;

                    let iter = table.iter().map_err(map_storage_err)?;
                    for entry in iter {
                        let (k, v) = entry.map_err(map_storage_err)?;
                        let key = k.value();
                        let bytes = v.value();
                        if Self::matches_filters(bytes, key, &schema, &filters)? {
                            if skipped < skip {
                                skipped += 1;
                                continue;
                            }
                            if taken >= take {
                                break;
                            }
                            results.push(T::from_store_bytes(bytes)?);
                            taken += 1;
                        }
                    }
                }

                Ok(results)
            }
            _ => Err(StoreError::BackendError(
                "fetch only supports Scan descriptors".into(),
            )),
        }
    }

    fn fetch_page<T: StoreRecord>(&self, query: Query<T>) -> Result<(Vec<T>, u64)> {
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
                let table_name = Self::data_table_name(T::TABLE_NAME);
                let def = TableDefinition::<u64, &[u8]>::new(table_name);

                let txn = self.cf.begin_read().map_err(map_txn_err)?;
                let table = match txn.open_table(def) {
                    Ok(t) => t,
                    Err(manifold::TableError::TableDoesNotExist(_)) => return Ok((Vec::new(), 0)),
                    Err(e) => return Err(map_table_err(e)),
                };

                // If no filters, total is O(1)
                if filters.is_empty() {
                    let total = table.len().map_err(map_storage_err)?;
                    let results = self.fetch::<T>(
                        // Rebuild query since we consumed it
                        {
                            let mut q = Query::<T>::new(T::TABLE_NAME);
                            if let Some((col, dir)) = order_by { q = q.order_by_raw(col, dir); }
                            if let Some(l) = limit { q = q.limit(l); }
                            if let Some(o) = offset { q = q.offset(o); }
                            q
                        }
                    )?;
                    return Ok((results, total));
                }

                // Filtered: scan all, count matches, collect page
                let mut matching: Vec<(u64, Vec<u8>)> = Vec::new();
                let iter = table.iter().map_err(map_storage_err)?;
                for entry in iter {
                    let (k, v) = entry.map_err(map_storage_err)?;
                    let key = k.value();
                    let bytes = v.value().to_vec();
                    if Self::matches_filters(&bytes, key, &schema, &filters)? {
                        matching.push((key, bytes));
                    }
                }

                let total = matching.len() as u64;

                let reverse = matches!(order_by, Some((0, Direction::Desc)));
                if reverse {
                    matching.reverse();
                }

                let skip = offset.unwrap_or(0) as usize;
                let take = limit.unwrap_or(u32::MAX) as usize;
                let results: Result<Vec<T>> = matching
                    .into_iter()
                    .skip(skip)
                    .take(take)
                    .map(|(_, bytes)| T::from_store_bytes(&bytes))
                    .collect();

                Ok((results?, total))
            }
            _ => Err(StoreError::BackendError(
                "fetch_page only supports Scan descriptors".into(),
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
                let schema = T::schema();
                let table_name = Self::data_table_name(T::TABLE_NAME);
                let def = TableDefinition::<u64, &[u8]>::new(table_name);

                let txn = self.cf.begin_read().map_err(map_txn_err)?;
                let table = match txn.open_table(def) {
                    Ok(t) => t,
                    Err(manifold::TableError::TableDoesNotExist(_)) => return Ok(0),
                    Err(e) => return Err(map_table_err(e)),
                };

                if filters.is_empty() {
                    // O(1) count from B-tree metadata
                    return table.len().map_err(map_storage_err);
                }

                let mut count = 0u64;
                let iter = table.iter().map_err(map_storage_err)?;
                for entry in iter {
                    let (k, v) = entry.map_err(map_storage_err)?;
                    if Self::matches_filters(v.value(), k.value(), &schema, &filters)? {
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
        F: for<'a> FnOnce(&ManifoldTxnOps<'a>) -> Result<R>,
    {
        let txn = self.cf.begin_write().map_err(map_txn_err)?;
        let ops = ManifoldTxnOps { txn: &txn };
        let result = f(&ops)?;
        txn.commit().map_err(map_commit_err)?;
        Ok(result)
    }
}

pub struct ManifoldTxnOps<'a> {
    txn: &'a manifold::WriteTransaction,
}

impl TransactionOps for ManifoldTxnOps<'_> {
    fn get<T: StoreRecord>(&self, key: impl Into<u64>) -> Result<Option<T>> {
        let key = key.into();
        let table_name = ManifoldStore::data_table_name(T::TABLE_NAME);
        let def = TableDefinition::<u64, &[u8]>::new(table_name);
        let table = self.txn.open_table(def).map_err(map_table_err)?;
        match table.get(key).map_err(map_storage_err)? {
            Some(guard) => Ok(Some(T::from_store_bytes(guard.value())?)),
            None => Ok(None),
        }
    }

    fn insert<T: StoreRecord>(&self, record: &T) -> Result<()> {
        let table_name = ManifoldStore::data_table_name(T::TABLE_NAME);
        let def = TableDefinition::<u64, &[u8]>::new(table_name);
        let key_bytes = record.primary_key_bytes();
        let key = u64::from_le_bytes(key_bytes[..8].try_into().unwrap());
        let row_bytes = record.to_store_bytes();
        let mut table = self.txn.open_table(def).map_err(map_table_err)?;
        table
            .insert(key, row_bytes.as_slice())
            .map_err(map_storage_err)?;
        Ok(())
    }

    fn update<T: StoreRecord>(&self, record: &T) -> Result<()> {
        let table_name = ManifoldStore::data_table_name(T::TABLE_NAME);
        let def = TableDefinition::<u64, &[u8]>::new(table_name);
        let key_bytes = record.primary_key_bytes();
        let key = u64::from_le_bytes(key_bytes[..8].try_into().unwrap());
        let row_bytes = record.to_store_bytes();
        let mut table = self.txn.open_table(def).map_err(map_table_err)?;
        table
            .insert(key, row_bytes.as_slice())
            .map_err(map_storage_err)?;
        Ok(())
    }

    fn delete<T: StoreRecord>(&self, key: impl Into<u64>) -> Result<()> {
        let key = key.into();
        let table_name = ManifoldStore::data_table_name(T::TABLE_NAME);
        let def = TableDefinition::<u64, &[u8]>::new(table_name);
        let mut table = self.txn.open_table(def).map_err(map_table_err)?;
        table.remove(key).map_err(map_storage_err)?;
        Ok(())
    }
}
