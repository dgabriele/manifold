use serde::{Deserialize, Serialize};

use crate::types::SqlType;

pub type TableId = u32;
pub type IndexId = u32;
pub type ConstraintId = u32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSchema {
    pub id: TableId,
    pub name: String,
    pub columns: Vec<ColumnDef>,
    pub constraints: Vec<ConstraintDef>,
    pub next_rowid: u64,
}

impl TableSchema {
    /// Returns the index of the column with the given name (case-insensitive).
    pub fn column_index(&self, name: &str) -> Option<usize> {
        let lower = name.to_lowercase();
        self.columns
            .iter()
            .position(|c| c.name.to_lowercase() == lower)
    }

    /// Returns a reference to the column with the given name (case-insensitive).
    pub fn column_by_name(&self, name: &str) -> Option<&ColumnDef> {
        let lower = name.to_lowercase();
        self.columns.iter().find(|c| c.name.to_lowercase() == lower)
    }

    /// Returns the SQL types of all columns in order.
    pub fn column_types(&self) -> Vec<SqlType> {
        self.columns.iter().map(|c| c.sql_type.clone()).collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    pub sql_type: SqlType,
    pub nullable: bool,
    pub default: Option<DefaultValue>,
    pub is_primary_key: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DefaultValue {
    Literal(crate::types::Value),
    CurrentTimestamp,
    CurrentDate,
    Null,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexDef {
    pub id: IndexId,
    pub name: String,
    pub table_id: TableId,
    pub columns: Vec<usize>,
    pub unique: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConstraintDef {
    PrimaryKey {
        id: ConstraintId,
        name: String,
        columns: Vec<usize>,
    },
    Unique {
        id: ConstraintId,
        name: String,
        columns: Vec<usize>,
    },
    NotNull {
        id: ConstraintId,
        name: String,
        column: usize,
    },
    ForeignKey {
        id: ConstraintId,
        name: String,
        columns: Vec<usize>,
        ref_table: String,
        ref_columns: Vec<String>,
        on_delete: ForeignKeyAction,
        on_update: ForeignKeyAction,
    },
    Check {
        id: ConstraintId,
        name: String,
        expression: String,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum ForeignKeyAction {
    #[default]
    Restrict,
    Cascade,
    SetNull,
    NoAction,
}
