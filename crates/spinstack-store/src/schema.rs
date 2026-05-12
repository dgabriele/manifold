use crate::types::ColumnType;

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDef {
    pub name: &'static str,
    pub column_type: ColumnType,
    pub is_primary_key: bool,
    pub is_indexed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableSchema {
    pub table_name: &'static str,
    pub columns: &'static [ColumnDef],
}

pub trait StoreRecord: Sized {
    const TABLE_NAME: &'static str;
    fn schema() -> TableSchema;
    fn to_store_bytes(&self) -> Vec<u8>;
    fn from_store_bytes(bytes: &[u8]) -> crate::Result<Self>;
    fn primary_key_bytes(&self) -> Vec<u8>;
}
