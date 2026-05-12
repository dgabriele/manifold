use std::fmt;

#[derive(Debug)]
pub enum StoreError {
    NotFound,
    DuplicateKey,
    SchemaError(String),
    SerializationError(String),
    BackendError(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::NotFound => write!(f, "record not found"),
            StoreError::DuplicateKey => write!(f, "duplicate primary key"),
            StoreError::SchemaError(msg) => write!(f, "schema error: {msg}"),
            StoreError::SerializationError(msg) => write!(f, "serialization error: {msg}"),
            StoreError::BackendError(msg) => write!(f, "backend error: {msg}"),
        }
    }
}

impl std::error::Error for StoreError {}

pub type Result<T> = std::result::Result<T, StoreError>;
