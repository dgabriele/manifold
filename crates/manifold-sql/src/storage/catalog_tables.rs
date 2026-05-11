use manifold::TableDefinition;

pub const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("_meta");
pub const TABLES_TABLE: TableDefinition<u32, &[u8]> = TableDefinition::new("_tables");
pub const INDEXES_TABLE: TableDefinition<u32, &[u8]> = TableDefinition::new("_indexes");
pub const SEQUENCES_TABLE: TableDefinition<u32, u64> = TableDefinition::new("_sequences");
pub const STATISTICS_TABLE: TableDefinition<u32, &[u8]> = TableDefinition::new("_statistics");
pub const SCHEMA_VERSION: u32 = 1;
