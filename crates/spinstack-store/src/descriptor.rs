use crate::types::{Direction, FilterOp, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub column_index: u16,
    pub op: FilterOp,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateFunc { Count, Sum, Min, Max, Avg }

#[derive(Debug, Clone, PartialEq)]
pub enum QueryDescriptor {
    Get { table_name: &'static str, key_bytes: Vec<u8> },
    Insert { table_name: &'static str, key_bytes: Vec<u8>, row_bytes: Vec<u8> },
    Update { table_name: &'static str, key_bytes: Vec<u8>, row_bytes: Vec<u8> },
    Delete { table_name: &'static str, key_bytes: Vec<u8> },
    Scan { table_name: &'static str, filters: Vec<Filter>, order_by: Option<(u16, Direction)>, limit: Option<u32>, offset: Option<u32> },
    Aggregate { table_name: &'static str, filters: Vec<Filter>, func: AggregateFunc, column: Option<u16> },
    Join { left_table: &'static str, right_table: &'static str, left_column: u16, right_column: u16, filters: Vec<Filter>, order_by: Option<(u16, Direction)>, limit: Option<u32> },
    Transaction { ops: Vec<QueryDescriptor> },
}
