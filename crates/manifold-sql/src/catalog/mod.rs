pub mod persist;
pub mod schema;

use std::collections::HashMap;

use schema::{ConstraintId, IndexDef, IndexId, TableId, TableSchema};

pub struct Catalog {
    tables: HashMap<String, TableSchema>,
    indexes: HashMap<String, IndexDef>,
    next_table_id: TableId,
    next_index_id: IndexId,
    next_constraint_id: ConstraintId,
}

impl Catalog {
    pub fn new() -> Self {
        Self {
            tables: HashMap::new(),
            indexes: HashMap::new(),
            next_table_id: 1,
            next_index_id: 1,
            next_constraint_id: 1,
        }
    }

    pub fn next_table_id(&mut self) -> TableId {
        let id = self.next_table_id;
        self.next_table_id += 1;
        id
    }

    pub fn next_index_id(&mut self) -> IndexId {
        let id = self.next_index_id;
        self.next_index_id += 1;
        id
    }

    pub fn next_constraint_id(&mut self) -> ConstraintId {
        let id = self.next_constraint_id;
        self.next_constraint_id += 1;
        id
    }

    pub fn set_next_table_id(&mut self, id: TableId) {
        self.next_table_id = id;
    }

    pub fn set_next_index_id(&mut self, id: IndexId) {
        self.next_index_id = id;
    }

    pub fn set_next_constraint_id(&mut self, id: ConstraintId) {
        self.next_constraint_id = id;
    }

    pub fn add_table(&mut self, schema: TableSchema) {
        self.tables.insert(schema.name.clone(), schema);
    }

    pub fn drop_table(&mut self, name: &str) -> Option<TableSchema> {
        self.tables.remove(name)
    }

    pub fn get_table(&self, name: &str) -> Option<&TableSchema> {
        self.tables.get(name)
    }

    pub fn get_table_mut(&mut self, name: &str) -> Option<&mut TableSchema> {
        self.tables.get_mut(name)
    }

    pub fn has_table(&self, name: &str) -> bool {
        self.tables.contains_key(name)
    }

    pub fn table_names(&self) -> Vec<String> {
        self.tables.keys().cloned().collect()
    }

    pub fn add_index(&mut self, index: IndexDef) {
        self.indexes.insert(index.name.clone(), index);
    }

    pub fn drop_index(&mut self, name: &str) -> Option<IndexDef> {
        self.indexes.remove(name)
    }

    pub fn get_index(&self, name: &str) -> Option<&IndexDef> {
        self.indexes.get(name)
    }

    pub fn indexes_for_table(&self, table_id: TableId) -> Vec<&IndexDef> {
        self.indexes
            .values()
            .filter(|idx| idx.table_id == table_id)
            .collect()
    }
}

impl Default for Catalog {
    fn default() -> Self {
        Self::new()
    }
}
