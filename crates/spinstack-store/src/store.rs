use crate::descriptor::QueryDescriptor;
use crate::error::Result;
use crate::query::Query;
use crate::schema::StoreRecord;

pub trait Store {
    fn get<T: StoreRecord>(&self, key: impl Into<u64>) -> Result<Option<T>>;
    fn insert<T: StoreRecord>(&self, record: &T) -> Result<()>;
    fn update<T: StoreRecord>(&self, record: &T) -> Result<()>;
    fn delete<T: StoreRecord>(&self, key: impl Into<u64>) -> Result<()>;
    fn fetch<T: StoreRecord>(&self, query: Query<T>) -> Result<Vec<T>>;
    fn count<T: StoreRecord>(&self, query: Query<T>) -> Result<u64>;
    fn execute_descriptor(&self, desc: QueryDescriptor) -> Result<Vec<Vec<u8>>>;
    fn transaction<F, R>(&self, f: F) -> Result<R>
    where F: FnOnce(&dyn TransactionOps) -> Result<R>;
}

pub trait TransactionOps {
    fn get<T: StoreRecord>(&self, key: impl Into<u64>) -> Result<Option<T>>;
    fn insert<T: StoreRecord>(&self, record: &T) -> Result<()>;
    fn update<T: StoreRecord>(&self, record: &T) -> Result<()>;
    fn delete<T: StoreRecord>(&self, key: impl Into<u64>) -> Result<()>;
}
