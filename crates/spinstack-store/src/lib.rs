pub mod types;
pub mod error;
pub mod schema;
pub mod descriptor;
pub mod field;
pub mod query;
pub mod serialize;
pub mod store;

pub use types::{ColumnType, Direction, FilterOp, Value};
pub use error::{StoreError, Result};
pub use schema::{ColumnDef, TableSchema, StoreRecord};
pub use descriptor::{QueryDescriptor, Filter, AggregateFunc};
pub use field::{Field, IntoValue};
pub use query::Query;
pub use store::{Store, TransactionOps};
// Re-export the derive macro. In Rust, derive macros and traits occupy
// different namespaces, so this coexists with the `Store` trait above.
pub use spinstack_store_macros::Store;
