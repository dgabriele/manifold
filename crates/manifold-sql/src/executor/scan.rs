use manifold::{
    MultimapTableDefinition, ReadableMultimapTable, ReadableTable, ReadableTableMetadata,
    TableDefinition,
};

use crate::catalog::Catalog;
use crate::catalog::schema::IndexDef;
use crate::error::{Result, SqlError};
use crate::expr::eval::evaluate;
use crate::planner::plan::{PlanSchema, ScalarExpr};
use crate::storage::index::encode_index_key;
use crate::storage::row_format::decode_row;
use crate::types::Value;

/// Table scan operator: materializes all rows from a Manifold data table.
///
/// Each output row is prefixed with the rowid (as Value::Integer) so that
/// UPDATE/DELETE can identify which physical row to modify.
pub struct TableScan {
    rows: Vec<Vec<Value>>,
    position: usize,
    #[allow(dead_code)]
    schema: PlanSchema,
}

impl TableScan {
    /// Create a new TableScan by reading all rows from the given table.
    ///
    /// This works with a `ReadTransaction`.
    pub fn from_read_txn(
        txn: &manifold::ReadTransaction,
        catalog: &Catalog,
        table_name: &str,
        schema: PlanSchema,
    ) -> Result<Self> {
        let table_schema = catalog
            .get_table(table_name)
            .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?;
        let col_types = table_schema.column_types();

        let data_name: &'static str = Box::leak(format!("data_{table_name}").into_boxed_str());
        let def = TableDefinition::<u64, &[u8]>::new(data_name);

        let mut rows = Vec::new();
        match txn.open_table(def) {
            Ok(table) => {
                let iter = table.iter().map_err(SqlError::Storage)?;
                for entry in iter {
                    let (key_guard, value_guard) = entry.map_err(SqlError::Storage)?;
                    let rowid = key_guard.value();
                    let bytes = value_guard.value();
                    let mut row = vec![Value::Integer(rowid as i64)];
                    let decoded = decode_row(&col_types, bytes)?;
                    row.extend(decoded);
                    rows.push(row);
                }
            }
            Err(manifold::TableError::TableDoesNotExist(_)) => {
                // Table has no data yet (never had an insert) — return empty
            }
            Err(e) => return Err(SqlError::TableError(e)),
        }

        Ok(Self {
            rows,
            position: 0,
            schema,
        })
    }

    /// Create a new TableScan by reading all rows within a write transaction.
    pub fn from_write_txn(
        txn: &manifold::WriteTransaction,
        catalog: &Catalog,
        table_name: &str,
        schema: PlanSchema,
    ) -> Result<Self> {
        let table_schema = catalog
            .get_table(table_name)
            .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?;
        let col_types = table_schema.column_types();

        let data_name: &'static str = Box::leak(format!("data_{table_name}").into_boxed_str());
        let def = TableDefinition::<u64, &[u8]>::new(data_name);

        let mut rows = Vec::new();
        let table = txn.open_table(def)?;
        {
            let iter = table.iter().map_err(SqlError::Storage)?;
            for entry in iter {
                let (key_guard, value_guard) = entry.map_err(SqlError::Storage)?;
                let rowid = key_guard.value();
                let bytes = value_guard.value();
                let mut row = vec![Value::Integer(rowid as i64)];
                let decoded = decode_row(&col_types, bytes)?;
                row.extend(decoded);
                rows.push(row);
            }
        }

        Ok(Self {
            rows,
            position: 0,
            schema,
        })
    }

    #[allow(dead_code)]
    pub fn schema(&self) -> &PlanSchema {
        &self.schema
    }
}

impl super::Executor for TableScan {
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        if self.position < self.rows.len() {
            let row = self.rows[self.position].clone();
            self.position += 1;
            Ok(Some(row))
        } else {
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// Index point-lookup scan
// ---------------------------------------------------------------------------

/// Index point-lookup operator: uses a B-tree index to find matching rowids,
/// then fetches only those rows from the data table.
///
/// Like `TableScan`, each output row is prefixed with the rowid.
pub struct IndexPointScan {
    rows: Vec<Vec<Value>>,
    position: usize,
}

impl IndexPointScan {
    /// Resolve lookup values, encode them as an index key, look up matching
    /// rowids in the index table, and fetch the corresponding data rows.
    fn resolve_lookup_values(lookup_values: &[ScalarExpr], params: &[Value]) -> Result<Vec<Value>> {
        lookup_values
            .iter()
            .map(|expr| evaluate(expr, &[], params))
            .collect()
    }

    /// Build an IndexPointScan from a read transaction.
    pub fn from_read_txn(
        txn: &manifold::ReadTransaction,
        catalog: &Catalog,
        table_name: &str,
        index_name: &str,
        lookup_values: &[ScalarExpr],
        params: &[Value],
    ) -> Result<Self> {
        let table_schema = catalog
            .get_table(table_name)
            .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?;
        let col_types = table_schema.column_types();

        let index_def = catalog
            .get_index(index_name)
            .ok_or_else(|| SqlError::IndexNotFound(index_name.to_string()))?
            .clone();

        let values = Self::resolve_lookup_values(lookup_values, params)?;
        let key_bytes = encode_index_key(&values);

        let data_name: &'static str = Box::leak(format!("data_{table_name}").into_boxed_str());
        let data_def = TableDefinition::<u64, &[u8]>::new(data_name);

        let rowids = Self::lookup_rowids_read(txn, table_name, &index_def, &key_bytes)?;

        let mut rows = Vec::with_capacity(rowids.len());
        if !rowids.is_empty() {
            let data_table = txn.open_table(data_def).map_err(SqlError::TableError)?;
            for rowid in rowids {
                if let Some(row_guard) = data_table.get(rowid).map_err(SqlError::Storage)? {
                    let mut row = vec![Value::Integer(rowid as i64)];
                    let decoded = decode_row(&col_types, row_guard.value())?;
                    row.extend(decoded);
                    rows.push(row);
                }
            }
        }

        Ok(Self { rows, position: 0 })
    }

    /// Build an IndexPointScan from a write transaction.
    pub fn from_write_txn(
        txn: &manifold::WriteTransaction,
        catalog: &Catalog,
        table_name: &str,
        index_name: &str,
        lookup_values: &[ScalarExpr],
        params: &[Value],
    ) -> Result<Self> {
        let table_schema = catalog
            .get_table(table_name)
            .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?;
        let col_types = table_schema.column_types();

        let index_def = catalog
            .get_index(index_name)
            .ok_or_else(|| SqlError::IndexNotFound(index_name.to_string()))?
            .clone();

        let values = Self::resolve_lookup_values(lookup_values, params)?;
        let key_bytes = encode_index_key(&values);

        let data_name: &'static str = Box::leak(format!("data_{table_name}").into_boxed_str());
        let data_def = TableDefinition::<u64, &[u8]>::new(data_name);

        let rowids = Self::lookup_rowids_write(txn, table_name, &index_def, &key_bytes)?;

        let mut rows = Vec::with_capacity(rowids.len());
        if !rowids.is_empty() {
            let data_table = txn.open_table(data_def)?;
            for rowid in rowids {
                if let Some(row_guard) = data_table.get(rowid).map_err(SqlError::Storage)? {
                    let mut row = vec![Value::Integer(rowid as i64)];
                    let decoded = decode_row(&col_types, row_guard.value())?;
                    row.extend(decoded);
                    rows.push(row);
                }
            }
        }

        Ok(Self { rows, position: 0 })
    }

    /// Look up rowids from the index using a read transaction.
    fn lookup_rowids_read(
        txn: &manifold::ReadTransaction,
        table_name: &str,
        index_def: &IndexDef,
        key_bytes: &[u8],
    ) -> Result<Vec<u64>> {
        let idx_tbl_name = super::index_table_name(table_name, &index_def.name, index_def.unique);
        let mut rowids = Vec::new();

        if index_def.unique {
            let idx_def = TableDefinition::<&[u8], u64>::new(idx_tbl_name);
            match txn.open_table(idx_def) {
                Ok(idx_table) => {
                    if let Some(guard) = idx_table.get(key_bytes).map_err(SqlError::Storage)? {
                        rowids.push(guard.value());
                    }
                }
                Err(manifold::TableError::TableDoesNotExist(_)) => {}
                Err(e) => return Err(SqlError::TableError(e)),
            }
        } else {
            let idx_def = MultimapTableDefinition::<&[u8], u64>::new(idx_tbl_name);
            match txn.open_multimap_table(idx_def) {
                Ok(idx_table) => {
                    let values = idx_table.get(key_bytes).map_err(SqlError::Storage)?;
                    for item in values {
                        rowids.push(item.map_err(SqlError::Storage)?.value());
                    }
                }
                Err(manifold::TableError::TableDoesNotExist(_)) => {}
                Err(e) => return Err(SqlError::TableError(e)),
            }
        }

        Ok(rowids)
    }

    /// Look up rowids from the index using a write transaction.
    fn lookup_rowids_write(
        txn: &manifold::WriteTransaction,
        table_name: &str,
        index_def: &IndexDef,
        key_bytes: &[u8],
    ) -> Result<Vec<u64>> {
        let idx_tbl_name = super::index_table_name(table_name, &index_def.name, index_def.unique);
        let mut rowids = Vec::new();

        if index_def.unique {
            let idx_def = TableDefinition::<&[u8], u64>::new(idx_tbl_name);
            let idx_table = txn.open_table(idx_def)?;
            if let Some(guard) = idx_table.get(key_bytes).map_err(SqlError::Storage)? {
                rowids.push(guard.value());
            }
        } else {
            let idx_def = MultimapTableDefinition::<&[u8], u64>::new(idx_tbl_name);
            let idx_table = txn.open_multimap_table(idx_def)?;
            let values = idx_table.get(key_bytes).map_err(SqlError::Storage)?;
            for item in values {
                rowids.push(item.map_err(SqlError::Storage)?.value());
            }
        }

        Ok(rowids)
    }
}

impl super::Executor for IndexPointScan {
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        if self.position < self.rows.len() {
            let row = self.rows[self.position].clone();
            self.position += 1;
            Ok(Some(row))
        } else {
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// Direct rowid lookup (PK = value)
// ---------------------------------------------------------------------------

/// Direct rowid lookup: fetches a single row by its u64 rowid key.
/// Skips the index entirely — the data table IS the index for PK lookups.
pub struct RowidLookupScan {
    row: Option<Vec<Value>>,
    done: bool,
}

impl RowidLookupScan {
    fn resolve_rowid(rowid_expr: &ScalarExpr, params: &[Value]) -> Result<u64> {
        let val = evaluate(rowid_expr, &[], params)?;
        match val {
            Value::Integer(i) if i > 0 => Ok(i as u64),
            _ => Ok(0), // invalid rowid, will yield no results
        }
    }

    pub fn from_read_txn(
        txn: &manifold::ReadTransaction,
        catalog: &Catalog,
        table_name: &str,
        rowid_expr: &ScalarExpr,
        params: &[Value],
    ) -> Result<Self> {
        let table_schema = catalog
            .get_table(table_name)
            .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?;
        let col_types = table_schema.column_types();

        let rowid = Self::resolve_rowid(rowid_expr, params)?;
        if rowid == 0 {
            return Ok(Self {
                row: None,
                done: false,
            });
        }

        let data_name: &'static str = Box::leak(format!("data_{table_name}").into_boxed_str());
        let def = TableDefinition::<u64, &[u8]>::new(data_name);

        let row = match txn.open_table(def) {
            Ok(table) => {
                if let Some(guard) = table.get(rowid).map_err(SqlError::Storage)? {
                    let mut r = vec![Value::Integer(rowid as i64)];
                    r.extend(decode_row(&col_types, guard.value())?);
                    Some(r)
                } else {
                    None
                }
            }
            Err(manifold::TableError::TableDoesNotExist(_)) => None,
            Err(e) => return Err(SqlError::TableError(e)),
        };

        Ok(Self { row, done: false })
    }

    pub fn from_write_txn(
        txn: &manifold::WriteTransaction,
        catalog: &Catalog,
        table_name: &str,
        rowid_expr: &ScalarExpr,
        params: &[Value],
    ) -> Result<Self> {
        let table_schema = catalog
            .get_table(table_name)
            .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?;
        let col_types = table_schema.column_types();

        let rowid = Self::resolve_rowid(rowid_expr, params)?;
        if rowid == 0 {
            return Ok(Self {
                row: None,
                done: false,
            });
        }

        let data_name: &'static str = Box::leak(format!("data_{table_name}").into_boxed_str());
        let def = TableDefinition::<u64, &[u8]>::new(data_name);

        let table = txn.open_table(def)?;
        let row = if let Some(guard) = table.get(rowid).map_err(SqlError::Storage)? {
            let mut r = vec![Value::Integer(rowid as i64)];
            r.extend(decode_row(&col_types, guard.value())?);
            Some(r)
        } else {
            None
        };

        Ok(Self { row, done: false })
    }
}

impl super::Executor for RowidLookupScan {
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        if self.done {
            return Ok(None);
        }
        self.done = true;
        Ok(self.row.take())
    }
}

// ---------------------------------------------------------------------------
// Rowid range scan (PK BETWEEN start AND end)
// ---------------------------------------------------------------------------

/// Rowid range scan: reads a contiguous range of rows using B-tree range lookup.
pub struct RowidRangeScan {
    rows: Vec<Vec<Value>>,
    position: usize,
}

impl RowidRangeScan {
    fn resolve_u64(expr: &ScalarExpr, params: &[Value]) -> Result<u64> {
        let val = evaluate(expr, &[], params)?;
        match val {
            Value::Integer(i) => Ok(i as u64),
            _ => Ok(0),
        }
    }

    pub fn from_read_txn(
        txn: &manifold::ReadTransaction,
        catalog: &Catalog,
        table_name: &str,
        start_expr: &ScalarExpr,
        end_expr: &ScalarExpr,
        params: &[Value],
    ) -> Result<Self> {
        let table_schema = catalog
            .get_table(table_name)
            .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?;
        let col_types = table_schema.column_types();

        let start = Self::resolve_u64(start_expr, params)?;
        let end = Self::resolve_u64(end_expr, params)?;

        let data_name: &'static str = Box::leak(format!("data_{table_name}").into_boxed_str());
        let def = TableDefinition::<u64, &[u8]>::new(data_name);

        let mut rows = Vec::new();
        match txn.open_table(def) {
            Ok(table) => {
                let iter = table.range(start..=end).map_err(SqlError::Storage)?;
                for entry in iter {
                    let (key_guard, value_guard) = entry.map_err(SqlError::Storage)?;
                    let rowid = key_guard.value();
                    let mut row = vec![Value::Integer(rowid as i64)];
                    row.extend(decode_row(&col_types, value_guard.value())?);
                    rows.push(row);
                }
            }
            Err(manifold::TableError::TableDoesNotExist(_)) => {}
            Err(e) => return Err(SqlError::TableError(e)),
        }

        Ok(Self { rows, position: 0 })
    }

    pub fn from_write_txn(
        txn: &manifold::WriteTransaction,
        catalog: &Catalog,
        table_name: &str,
        start_expr: &ScalarExpr,
        end_expr: &ScalarExpr,
        params: &[Value],
    ) -> Result<Self> {
        let table_schema = catalog
            .get_table(table_name)
            .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?;
        let col_types = table_schema.column_types();

        let start = Self::resolve_u64(start_expr, params)?;
        let end = Self::resolve_u64(end_expr, params)?;

        let data_name: &'static str = Box::leak(format!("data_{table_name}").into_boxed_str());
        let def = TableDefinition::<u64, &[u8]>::new(data_name);

        let mut rows = Vec::new();
        let table = txn.open_table(def)?;
        let iter = table.range(start..=end).map_err(SqlError::Storage)?;
        for entry in iter {
            let (key_guard, value_guard) = entry.map_err(SqlError::Storage)?;
            let rowid = key_guard.value();
            let mut row = vec![Value::Integer(rowid as i64)];
            row.extend(decode_row(&col_types, value_guard.value())?);
            rows.push(row);
        }

        Ok(Self { rows, position: 0 })
    }
}

impl super::Executor for RowidRangeScan {
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        if self.position < self.rows.len() {
            let row = self.rows[self.position].clone();
            self.position += 1;
            Ok(Some(row))
        } else {
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// O(1) COUNT(*) scan
// ---------------------------------------------------------------------------

/// O(1) row count: reads table.len() from B-tree metadata.
pub struct CountScan {
    count: Option<u64>,
}

impl CountScan {
    pub fn from_read_txn(
        txn: &manifold::ReadTransaction,
        table_name: &str,
    ) -> Result<Self> {
        let data_name: &'static str = Box::leak(format!("data_{table_name}").into_boxed_str());
        let def = TableDefinition::<u64, &[u8]>::new(data_name);
        let count = match txn.open_table(def) {
            Ok(table) => table.len().map_err(SqlError::Storage)?,
            Err(manifold::TableError::TableDoesNotExist(_)) => 0,
            Err(e) => return Err(SqlError::TableError(e)),
        };
        Ok(Self { count: Some(count) })
    }

    pub fn from_write_txn(
        txn: &manifold::WriteTransaction,
        table_name: &str,
    ) -> Result<Self> {
        let data_name: &'static str = Box::leak(format!("data_{table_name}").into_boxed_str());
        let def = TableDefinition::<u64, &[u8]>::new(data_name);
        let table = txn.open_table(def)?;
        let count = table.len().map_err(SqlError::Storage)?;
        Ok(Self { count: Some(count) })
    }
}

impl super::Executor for CountScan {
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        Ok(self.count.take().map(|c| vec![Value::Integer(c as i64)]))
    }
}
