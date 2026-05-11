pub mod binder;
pub mod catalog;
pub mod error;
pub mod executor;
pub mod expr;
pub mod optimizer;
pub mod parser;
pub mod planner;
pub mod storage;
pub mod types;

pub use error::{Result, SqlError};
pub use types::{SqlType, Value};

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use manifold::ReadableDatabase;

use catalog::Catalog;

// ---------------------------------------------------------------------------
// Row
// ---------------------------------------------------------------------------

/// A single row of query results.
#[derive(Debug, Clone)]
pub struct Row {
    values: Vec<Value>,
}

impl Row {
    /// Create a new row from a vector of values.
    pub fn new(values: Vec<Value>) -> Self {
        Self { values }
    }

    /// Returns all values in this row.
    pub fn values(&self) -> &[Value] {
        &self.values
    }

    /// Extracts a typed value at the given column index.
    pub fn get<T: FromValue>(&self, index: usize) -> Result<T> {
        let value = self
            .values
            .get(index)
            .ok_or_else(|| SqlError::Execute(format!("column index {index} out of bounds")))?;
        T::from_value(value)
    }
}

// ---------------------------------------------------------------------------
// FromValue
// ---------------------------------------------------------------------------

/// Trait for extracting a typed value from a `Value`.
pub trait FromValue: Sized {
    /// Convert a SQL `Value` into `Self`, returning an error on type mismatch.
    fn from_value(value: &Value) -> Result<Self>;
}

impl FromValue for i64 {
    fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Integer(v) => Ok(*v),
            Value::SmallInt(v) => Ok(*v as i64),
            other => Err(SqlError::TypeError(format!(
                "cannot convert {other} to i64"
            ))),
        }
    }
}

impl FromValue for i16 {
    fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::SmallInt(v) => Ok(*v),
            other => Err(SqlError::TypeError(format!(
                "cannot convert {other} to i16"
            ))),
        }
    }
}

impl FromValue for f64 {
    fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Real(v) => Ok(*v),
            Value::Integer(v) => Ok(*v as f64),
            Value::SmallInt(v) => Ok(*v as f64),
            other => Err(SqlError::TypeError(format!(
                "cannot convert {other} to f64"
            ))),
        }
    }
}

impl FromValue for bool {
    fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Boolean(v) => Ok(*v),
            other => Err(SqlError::TypeError(format!(
                "cannot convert {other} to bool"
            ))),
        }
    }
}

impl FromValue for String {
    fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Text(v) => Ok(v.clone()),
            other => Err(SqlError::TypeError(format!(
                "cannot convert {other} to String"
            ))),
        }
    }
}

impl FromValue for Vec<u8> {
    fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Blob(v) => Ok(v.clone()),
            other => Err(SqlError::TypeError(format!(
                "cannot convert {other} to Vec<u8>"
            ))),
        }
    }
}

impl FromValue for Value {
    fn from_value(value: &Value) -> Result<Self> {
        Ok(value.clone())
    }
}

// ---------------------------------------------------------------------------
// ResultSet
// ---------------------------------------------------------------------------

/// The result of a query: column names and rows.
#[derive(Debug, Clone)]
pub struct ResultSet {
    columns: Vec<String>,
    rows: Vec<Row>,
}

impl ResultSet {
    /// Create a new result set from column names and rows.
    pub fn new(columns: Vec<String>, rows: Vec<Row>) -> Self {
        Self { columns, rows }
    }

    /// Returns the column names.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// Returns all rows.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Returns the number of rows.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Returns an empty result set with no columns and no rows.
    pub fn empty() -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Database
// ---------------------------------------------------------------------------

/// State held during an active SQL-level explicit transaction (BEGIN...COMMIT/ROLLBACK).
struct ActiveTransaction {
    write_txn: manifold::WriteTransaction,
    savepoints: HashMap<String, manifold::Savepoint>,
}

/// The main entry point for the SQL engine.
pub struct Database {
    db: manifold::Database,
    catalog: Arc<Mutex<Catalog>>,
    /// Active SQL-level transaction started via BEGIN.
    active_txn: Mutex<Option<ActiveTransaction>>,
}

impl Database {
    /// Open or create a database at the given path.
    ///
    /// Initializes system tables if the database is new, then loads the catalog.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = if path.as_ref().exists() {
            manifold::Database::open(path)?
        } else {
            manifold::Database::create(path)?
        };

        // Check if system tables exist.
        let needs_init = {
            let read_txn = db.begin_read()?;
            read_txn
                .open_table(storage::catalog_tables::META_TABLE)
                .is_err()
        };

        if needs_init {
            let write_txn = db.begin_write()?;
            catalog::persist::init_system_tables(&write_txn)?;
            write_txn.commit()?;
        }

        let read_txn = db.begin_read()?;
        let catalog = catalog::persist::load_catalog(&read_txn)?;

        Ok(Self {
            db,
            catalog: Arc::new(Mutex::new(catalog)),
            active_txn: Mutex::new(None),
        })
    }

    /// Execute a mutating SQL statement. Returns the number of rows affected.
    pub fn execute(&self, sql: &str, params: &[Value]) -> Result<u64> {
        use planner::plan::LogicalPlan;

        let stmts = parser::parse(sql)?;
        let stmt = stmts
            .first()
            .ok_or_else(|| SqlError::Parse("empty SQL".into()))?;

        let mut catalog = self.catalog.lock().unwrap();
        let bound = binder::bind(&catalog, stmt, params)?;
        let plan = planner::plan(&catalog, &bound)?;
        let optimized = optimizer::optimize(plan, &catalog)?;

        // Handle transaction-control plans at the Database level.
        match &optimized {
            LogicalPlan::BeginTransaction => {
                let mut active = self.active_txn.lock().unwrap();
                if active.is_some() {
                    return Err(SqlError::Transaction(
                        "transaction already active".into(),
                    ));
                }
                let write_txn = self.db.begin_write()?;
                *active = Some(ActiveTransaction {
                    write_txn,
                    savepoints: HashMap::new(),
                });
                return Ok(0);
            }
            LogicalPlan::CommitTransaction => {
                let mut active = self.active_txn.lock().unwrap();
                let state = active.take().ok_or_else(|| {
                    SqlError::Transaction("no active transaction".into())
                })?;
                state.write_txn.commit()?;
                return Ok(0);
            }
            LogicalPlan::RollbackTransaction => {
                let mut active = self.active_txn.lock().unwrap();
                let state = active.take().ok_or_else(|| {
                    SqlError::Transaction("no active transaction".into())
                })?;
                let _ = state.write_txn.abort();
                return Ok(0);
            }
            LogicalPlan::Savepoint { name } => {
                let mut active = self.active_txn.lock().unwrap();
                let state = active.as_mut().ok_or_else(|| {
                    SqlError::Transaction("SAVEPOINT requires an active transaction".into())
                })?;
                let sp = state.write_txn.ephemeral_savepoint().map_err(|e| {
                    SqlError::Transaction(format!("savepoint error: {e}"))
                })?;
                state.savepoints.insert(name.clone(), sp);
                return Ok(0);
            }
            LogicalPlan::ReleaseSavepoint { name } => {
                let mut active = self.active_txn.lock().unwrap();
                let state = active.as_mut().ok_or_else(|| {
                    SqlError::Transaction("RELEASE SAVEPOINT requires an active transaction".into())
                })?;
                if state.savepoints.remove(name).is_none() {
                    return Err(SqlError::Transaction(format!(
                        "savepoint '{}' does not exist",
                        name
                    )));
                }
                return Ok(0);
            }
            LogicalPlan::RollbackToSavepoint { name } => {
                let mut active = self.active_txn.lock().unwrap();
                let state = active.as_mut().ok_or_else(|| {
                    SqlError::Transaction(
                        "ROLLBACK TO SAVEPOINT requires an active transaction".into(),
                    )
                })?;
                let sp = state.savepoints.get(name).ok_or_else(|| {
                    SqlError::Transaction(format!("savepoint '{}' does not exist", name))
                })?;
                state.write_txn.restore_savepoint(sp).map_err(|e| {
                    SqlError::Transaction(format!("restore savepoint error: {e}"))
                })?;
                return Ok(0);
            }
            _ => {}
        }

        // If there's an active explicit transaction, use it.
        let mut active = self.active_txn.lock().unwrap();
        if let Some(state) = active.as_mut() {
            executor::execute_in_txn(&state.write_txn, &mut catalog, optimized, params)
        } else {
            drop(active);
            executor::execute_mut(&self.db, &mut catalog, optimized, params)
        }
    }

    /// Execute a query and return the result set.
    pub fn query(&self, sql: &str, params: &[Value]) -> Result<ResultSet> {
        let stmts = parser::parse(sql)?;
        let stmt = stmts
            .first()
            .ok_or_else(|| SqlError::Parse("empty SQL".into()))?;

        let catalog = self.catalog.lock().unwrap();
        let bound = binder::bind(&catalog, stmt, params)?;
        let plan = planner::plan(&catalog, &bound)?;
        let optimized = optimizer::optimize(plan, &catalog)?;

        // If there's an active explicit transaction, query through it.
        let active = self.active_txn.lock().unwrap();
        if let Some(state) = active.as_ref() {
            executor::query_in_txn(&state.write_txn, &catalog, optimized, params)
        } else {
            drop(active);
            executor::execute_query(&self.db, &catalog, optimized, params)
        }
    }

    /// Begin an explicit transaction.
    pub fn begin(&self) -> Result<Transaction> {
        let txn = self.db.begin_write()?;
        Ok(Transaction {
            txn: Some(txn),
            catalog: Arc::clone(&self.catalog),
            finished: false,
        })
    }

    /// Show the optimized plan for a SQL statement as a human-readable tree.
    pub fn explain(&self, sql: &str) -> Result<String> {
        let stmts = parser::parse(sql)?;
        let stmt = stmts
            .first()
            .ok_or_else(|| SqlError::Parse("empty SQL".into()))?;

        let catalog = self.catalog.lock().unwrap();
        let bound = binder::bind(&catalog, stmt, &[])?;
        let plan = planner::plan(&catalog, &bound)?;
        let optimized = optimizer::optimize(plan, &catalog)?;
        Ok(executor::explain::format_plan(&optimized))
    }
}

// ---------------------------------------------------------------------------
// Transaction
// ---------------------------------------------------------------------------

/// An explicit transaction that groups multiple operations.
pub struct Transaction {
    txn: Option<manifold::WriteTransaction>,
    catalog: Arc<Mutex<Catalog>>,
    finished: bool,
}

impl Transaction {
    /// Execute a mutating SQL statement within this transaction.
    pub fn execute(&self, sql: &str, params: &[Value]) -> Result<u64> {
        let txn = self
            .txn
            .as_ref()
            .ok_or_else(|| SqlError::Transaction("transaction already finished".into()))?;

        let stmts = parser::parse(sql)?;
        let stmt = stmts
            .first()
            .ok_or_else(|| SqlError::Parse("empty SQL".into()))?;

        let mut catalog = self.catalog.lock().unwrap();
        let bound = binder::bind(&catalog, stmt, params)?;
        let plan = planner::plan(&catalog, &bound)?;
        let optimized = optimizer::optimize(plan, &catalog)?;
        executor::execute_in_txn(txn, &mut catalog, optimized, params)
    }

    /// Execute a query within this transaction.
    pub fn query(&self, sql: &str, params: &[Value]) -> Result<ResultSet> {
        let txn = self
            .txn
            .as_ref()
            .ok_or_else(|| SqlError::Transaction("transaction already finished".into()))?;

        let stmts = parser::parse(sql)?;
        let stmt = stmts
            .first()
            .ok_or_else(|| SqlError::Parse("empty SQL".into()))?;

        let catalog = self.catalog.lock().unwrap();
        let bound = binder::bind(&catalog, stmt, params)?;
        let plan = planner::plan(&catalog, &bound)?;
        let optimized = optimizer::optimize(plan, &catalog)?;
        executor::query_in_txn(txn, &catalog, optimized, params)
    }

    /// Commit the transaction.
    pub fn commit(mut self) -> Result<()> {
        let txn = self
            .txn
            .take()
            .ok_or_else(|| SqlError::Transaction("transaction already finished".into()))?;
        self.finished = true;
        txn.commit()?;
        Ok(())
    }

    /// Roll back the transaction.
    pub fn rollback(mut self) -> Result<()> {
        let txn = self
            .txn
            .take()
            .ok_or_else(|| SqlError::Transaction("transaction already finished".into()))?;
        self.finished = true;
        let _ = txn.abort();
        Ok(())
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if !self.finished && let Some(txn) = self.txn.take() {
            let _ = txn.abort();
        }
    }
}
