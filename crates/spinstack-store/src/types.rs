#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType { U64, I64, F64, String, Bytes, Bool }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction { Asc, Desc }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp { Eq, Ne, Lt, Le, Gt, Ge }

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    U64(u64), I64(i64), F64(f64), String(String), Bytes(Vec<u8>), Bool(bool), Null,
}
