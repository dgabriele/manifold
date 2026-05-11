use manifold::{ReadableTable, TableError};

use crate::catalog::schema::{ConstraintDef, IndexDef, IndexId, TableId, TableSchema};
use crate::catalog::Catalog;
use crate::error::{Result, SqlError};
use crate::storage::catalog_tables::*;

/// Create all system tables and write the initial schema version.
pub fn init_system_tables(txn: &manifold::WriteTransaction) -> Result<()> {
    // Opening tables on a WriteTransaction creates them if they don't exist.
    txn.open_table(META_TABLE)?;
    txn.open_table(TABLES_TABLE)?;
    txn.open_table(INDEXES_TABLE)?;
    txn.open_table(SEQUENCES_TABLE)?;
    txn.open_table(STATISTICS_TABLE)?;

    // Write schema version.
    let mut meta = txn.open_table(META_TABLE)?;
    let version_bytes = SCHEMA_VERSION.to_le_bytes();
    meta.insert("schema_version", version_bytes.as_slice())?;

    Ok(())
}

/// Load the full catalog from persisted system tables.
///
/// Returns a populated `Catalog` with all table schemas, indexes, sequences,
/// and restored ID counters. If the system tables don't exist yet (fresh
/// database), returns an empty catalog.
pub fn load_catalog(txn: &manifold::ReadTransaction) -> Result<Catalog> {
    let mut catalog = Catalog::new();

    // Load table schemas.
    let tables_table = match txn.open_table(TABLES_TABLE) {
        Ok(t) => t,
        Err(TableError::TableDoesNotExist(_)) => return Ok(catalog),
        Err(e) => return Err(SqlError::TableError(e)),
    };

    let mut max_table_id: TableId = 0;
    let mut max_constraint_id: u32 = 0;

    {
        let iter = tables_table.iter().map_err(SqlError::Storage)?;
        for item in iter {
            let (key_guard, value_guard) = item.map_err(SqlError::Storage)?;
            let table_id = key_guard.value();
            let json_bytes = value_guard.value();

            let schema: TableSchema = serde_json::from_slice(json_bytes).map_err(|e| {
                SqlError::Internal(format!("failed to deserialize table schema: {e}"))
            })?;

            if table_id > max_table_id {
                max_table_id = table_id;
            }

            // Track max constraint ID from this table's constraints.
            for constraint in &schema.constraints {
                let cid = constraint_id(constraint);
                if cid > max_constraint_id {
                    max_constraint_id = cid;
                }
            }

            catalog.add_table(schema);
        }
    }

    // Load indexes.
    let mut max_index_id: IndexId = 0;

    if let Ok(indexes_table) = txn.open_table(INDEXES_TABLE) {
        let iter = indexes_table.iter().map_err(SqlError::Storage)?;
        for item in iter {
            let (key_guard, value_guard) = item.map_err(SqlError::Storage)?;
            let index_id = key_guard.value();
            let json_bytes = value_guard.value();

            let index: IndexDef = serde_json::from_slice(json_bytes).map_err(|e| {
                SqlError::Internal(format!("failed to deserialize index def: {e}"))
            })?;

            if index_id > max_index_id {
                max_index_id = index_id;
            }

            catalog.add_index(index);
        }
    }

    // Load sequences (next_rowid for each table).
    if let Ok(seq_table) = txn.open_table(SEQUENCES_TABLE) {
        let iter = seq_table.iter().map_err(SqlError::Storage)?;
        for item in iter {
            let (key_guard, value_guard) = item.map_err(SqlError::Storage)?;
            let table_id = key_guard.value();
            let next_rowid = value_guard.value();

            // Find the matching table and update its next_rowid.
            // We need to iterate table names to find the right one.
            for name in catalog.table_names() {
                if let Some(schema) = catalog.get_table_mut(&name) {
                    if schema.id == table_id {
                        schema.next_rowid = next_rowid;
                        break;
                    }
                }
            }
        }
    }

    // Restore counters: next ID is max found + 1 (minimum 1).
    catalog.set_next_table_id(if max_table_id > 0 {
        max_table_id + 1
    } else {
        1
    });
    catalog.set_next_index_id(if max_index_id > 0 {
        max_index_id + 1
    } else {
        1
    });
    catalog.set_next_constraint_id(if max_constraint_id > 0 {
        max_constraint_id + 1
    } else {
        1
    });

    Ok(catalog)
}

/// Persist a table schema to the system tables.
pub fn save_table(txn: &manifold::WriteTransaction, schema: &TableSchema) -> Result<()> {
    let json = serde_json::to_vec(schema)
        .map_err(|e| SqlError::Internal(format!("failed to serialize table schema: {e}")))?;

    let mut tables = txn.open_table(TABLES_TABLE)?;
    tables.insert(schema.id, json.as_slice())?;

    let mut sequences = txn.open_table(SEQUENCES_TABLE)?;
    sequences.insert(schema.id, schema.next_rowid)?;

    Ok(())
}

/// Remove a table schema and its sequence from the system tables.
pub fn remove_table(txn: &manifold::WriteTransaction, table_id: TableId) -> Result<()> {
    let mut tables = txn.open_table(TABLES_TABLE)?;
    tables.remove(table_id)?;

    let mut sequences = txn.open_table(SEQUENCES_TABLE)?;
    sequences.remove(table_id)?;

    Ok(())
}

/// Persist an index definition to the system tables.
pub fn save_index(txn: &manifold::WriteTransaction, index: &IndexDef) -> Result<()> {
    let json = serde_json::to_vec(index)
        .map_err(|e| SqlError::Internal(format!("failed to serialize index def: {e}")))?;

    let mut indexes = txn.open_table(INDEXES_TABLE)?;
    indexes.insert(index.id, json.as_slice())?;

    Ok(())
}

/// Remove an index definition from the system tables.
pub fn remove_index(txn: &manifold::WriteTransaction, index_id: IndexId) -> Result<()> {
    let mut indexes = txn.open_table(INDEXES_TABLE)?;
    indexes.remove(index_id)?;

    Ok(())
}

/// Extract the constraint ID from a `ConstraintDef` variant.
fn constraint_id(constraint: &ConstraintDef) -> u32 {
    match constraint {
        ConstraintDef::PrimaryKey { id, .. }
        | ConstraintDef::Unique { id, .. }
        | ConstraintDef::NotNull { id, .. }
        | ConstraintDef::ForeignKey { id, .. }
        | ConstraintDef::Check { id, .. } => *id,
    }
}
