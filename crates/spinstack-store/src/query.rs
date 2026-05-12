use std::marker::PhantomData;
use crate::descriptor::{AggregateFunc, Filter, QueryDescriptor};
use crate::field::Field;
use crate::types::Direction;

pub struct Query<T> {
    table_name: &'static str,
    filters: Vec<Filter>,
    order_by: Option<(u16, Direction)>,
    limit: Option<u32>,
    offset: Option<u32>,
    _phantom: PhantomData<T>,
}

impl<T> Query<T> {
    pub fn new(table_name: &'static str) -> Self {
        Self { table_name, filters: Vec::new(), order_by: None, limit: None, offset: None, _phantom: PhantomData }
    }
    pub fn filter(mut self, f: Filter) -> Self { self.filters.push(f); self }
    pub fn order_by<F>(mut self, field: Field<F>, dir: Direction) -> Self {
        self.order_by = Some((field.column_index, dir)); self
    }
    pub fn limit(mut self, n: u32) -> Self { self.limit = Some(n); self }
    pub fn offset(mut self, n: u32) -> Self { self.offset = Some(n); self }
    pub fn build(self) -> QueryDescriptor {
        QueryDescriptor::Scan { table_name: self.table_name, filters: self.filters, order_by: self.order_by, limit: self.limit, offset: self.offset }
    }
    pub fn build_count(self) -> QueryDescriptor {
        QueryDescriptor::Aggregate { table_name: self.table_name, filters: self.filters, func: AggregateFunc::Count, column: None }
    }
}
