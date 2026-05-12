use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::catalog::schema::TableId;

/// Statistics for a single table, used by the optimizer for cost estimation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableStatistics {
    pub row_count: u64,
    pub indexes: Vec<IndexStatistics>,
}

/// Statistics for a single index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStatistics {
    pub name: String,
    pub cardinality: u64,
}

/// Default row count estimate when no statistics have been collected.
const DEFAULT_ROW_COUNT: u64 = 1000;

/// Estimate statistics for a table. Until ANALYZE is implemented, this returns
/// sensible defaults (1000 rows, index cardinality = row count).
pub fn estimate_table_stats(table_id: TableId, catalog: &Catalog) -> TableStatistics {
    let indexes = catalog
        .indexes_for_table(table_id)
        .into_iter()
        .map(|idx| IndexStatistics {
            name: idx.name.clone(),
            cardinality: DEFAULT_ROW_COUNT,
        })
        .collect();

    TableStatistics {
        row_count: DEFAULT_ROW_COUNT,
        indexes,
    }
}

/// Estimate the row count for a table by name. Returns the default if the table
/// is not found.
pub fn estimate_row_count(table_name: &str, catalog: &Catalog) -> u64 {
    match catalog.get_table(table_name) {
        Some(table) => {
            let stats = estimate_table_stats(table.id, catalog);
            stats.row_count
        }
        None => DEFAULT_ROW_COUNT,
    }
}
