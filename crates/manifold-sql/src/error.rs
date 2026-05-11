use std::fmt;

/// Errors that can occur in the SQL engine.
#[derive(Debug)]
#[non_exhaustive]
pub enum SqlError {
    /// SQL parsing error.
    Parse(String),
    /// Parameter binding error.
    Bind(String),
    /// Query planning error.
    Plan(String),
    /// Query execution error.
    Execute(String),
    /// A constraint (e.g. UNIQUE, NOT NULL) was violated.
    ConstraintViolation(String),
    /// A type error (e.g. incompatible types in an expression).
    TypeError(String),
    /// The referenced table was not found.
    TableNotFound(String),
    /// The table already exists.
    TableExists(String),
    /// The referenced column was not found.
    ColumnNotFound(String),
    /// The referenced index was not found.
    IndexNotFound(String),
    /// The index already exists.
    IndexExists(String),
    /// Error from the underlying storage engine.
    Storage(manifold::StorageError),
    /// Error from a table operation.
    TableError(manifold::TableError),
    /// Error related to transactions.
    Transaction(String),
    /// An internal/unexpected error.
    Internal(String),
}

impl fmt::Display for SqlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SqlError::Parse(msg) => write!(f, "SQL parse error: {msg}"),
            SqlError::Bind(msg) => write!(f, "Bind error: {msg}"),
            SqlError::Plan(msg) => write!(f, "Plan error: {msg}"),
            SqlError::Execute(msg) => write!(f, "Execution error: {msg}"),
            SqlError::ConstraintViolation(msg) => write!(f, "Constraint violation: {msg}"),
            SqlError::TypeError(msg) => write!(f, "Type error: {msg}"),
            SqlError::TableNotFound(name) => write!(f, "Table not found: {name}"),
            SqlError::TableExists(name) => write!(f, "Table already exists: {name}"),
            SqlError::ColumnNotFound(name) => write!(f, "Column not found: {name}"),
            SqlError::IndexNotFound(name) => write!(f, "Index not found: {name}"),
            SqlError::IndexExists(name) => write!(f, "Index already exists: {name}"),
            SqlError::Storage(err) => write!(f, "Storage error: {err}"),
            SqlError::TableError(err) => write!(f, "Table error: {err}"),
            SqlError::Transaction(msg) => write!(f, "Transaction error: {msg}"),
            SqlError::Internal(msg) => write!(f, "Internal error: {msg}"),
        }
    }
}

impl std::error::Error for SqlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SqlError::Storage(err) => Some(err),
            SqlError::TableError(err) => Some(err),
            _ => None,
        }
    }
}

impl From<manifold::StorageError> for SqlError {
    fn from(err: manifold::StorageError) -> Self {
        SqlError::Storage(err)
    }
}

impl From<manifold::TableError> for SqlError {
    fn from(err: manifold::TableError) -> Self {
        SqlError::TableError(err)
    }
}

impl From<manifold::CommitError> for SqlError {
    fn from(err: manifold::CommitError) -> Self {
        SqlError::Transaction(err.to_string())
    }
}

impl From<manifold::TransactionError> for SqlError {
    fn from(err: manifold::TransactionError) -> Self {
        SqlError::Transaction(err.to_string())
    }
}

impl From<manifold::DatabaseError> for SqlError {
    fn from(err: manifold::DatabaseError) -> Self {
        SqlError::Internal(err.to_string())
    }
}

/// A specialized `Result` type for SQL operations.
pub type Result<T> = std::result::Result<T, SqlError>;
