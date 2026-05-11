use manifold::{ReadableTable, TableDefinition};

use crate::catalog::Catalog;
use crate::error::{Result, SqlError};
use crate::planner::plan::PlanSchema;
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

        let data_name: &'static str =
            Box::leak(format!("data_{table_name}").into_boxed_str());
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

        let data_name: &'static str =
            Box::leak(format!("data_{table_name}").into_boxed_str());
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
