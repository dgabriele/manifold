use crate::descriptor::Filter;
use crate::types::{FilterOp, Value};

#[derive(Debug, Clone, Copy)]
pub struct Field<T> {
    pub column_index: u16,
    pub _phantom: std::marker::PhantomData<T>,
}

impl<T> Field<T> {
    pub const fn new(index: u16) -> Self {
        Self { column_index: index, _phantom: std::marker::PhantomData }
    }
}

pub trait IntoValue {
    fn into_value(self) -> Value;
}

impl IntoValue for u64 { fn into_value(self) -> Value { Value::U64(self) } }
impl IntoValue for i64 { fn into_value(self) -> Value { Value::I64(self) } }
impl IntoValue for f64 { fn into_value(self) -> Value { Value::F64(self) } }
impl IntoValue for &str { fn into_value(self) -> Value { Value::String(self.to_string()) } }
impl IntoValue for String { fn into_value(self) -> Value { Value::String(self) } }
impl IntoValue for bool { fn into_value(self) -> Value { Value::Bool(self) } }

impl<T> Field<T> {
    pub fn eq(&self, val: impl IntoValue) -> Filter {
        Filter { column_index: self.column_index, op: FilterOp::Eq, value: val.into_value() }
    }
    pub fn ne(&self, val: impl IntoValue) -> Filter {
        Filter { column_index: self.column_index, op: FilterOp::Ne, value: val.into_value() }
    }
    pub fn lt(&self, val: impl IntoValue) -> Filter {
        Filter { column_index: self.column_index, op: FilterOp::Lt, value: val.into_value() }
    }
    pub fn le(&self, val: impl IntoValue) -> Filter {
        Filter { column_index: self.column_index, op: FilterOp::Le, value: val.into_value() }
    }
    pub fn gt(&self, val: impl IntoValue) -> Filter {
        Filter { column_index: self.column_index, op: FilterOp::Gt, value: val.into_value() }
    }
    pub fn ge(&self, val: impl IntoValue) -> Filter {
        Filter { column_index: self.column_index, op: FilterOp::Ge, value: val.into_value() }
    }
}
