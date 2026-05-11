use std::fmt;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// SQL data types supported by the engine.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SqlType {
    Boolean,
    SmallInt,
    Integer,
    BigInt,
    Real,
    Decimal { precision: u32, scale: u32 },
    Text,
    Varchar(u32),
    Blob,
    Uuid,
    Date,
    Timestamp,
    TimestampTz,
    Json,
}

impl SqlType {
    /// Returns the fixed width in bytes for fixed-width types, or `None` for variable-width types.
    pub fn fixed_width(&self) -> Option<usize> {
        match self {
            SqlType::Boolean => Some(1),
            SqlType::SmallInt => Some(2),
            SqlType::Integer => Some(8),
            SqlType::BigInt => Some(8),
            SqlType::Real => Some(8),
            SqlType::Decimal { .. } => Some(16),
            SqlType::Uuid => Some(16),
            SqlType::Date => Some(4),
            SqlType::Timestamp => Some(12),
            SqlType::TimestampTz => Some(12),
            SqlType::Text | SqlType::Varchar(_) | SqlType::Blob | SqlType::Json => None,
        }
    }

    /// Returns `true` if this type has variable width (i.e., not fixed-width).
    pub fn is_variable_width(&self) -> bool {
        self.fixed_width().is_none()
    }
}

impl fmt::Display for SqlType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SqlType::Boolean => write!(f, "BOOLEAN"),
            SqlType::SmallInt => write!(f, "SMALLINT"),
            SqlType::Integer => write!(f, "INTEGER"),
            SqlType::BigInt => write!(f, "BIGINT"),
            SqlType::Real => write!(f, "REAL"),
            SqlType::Decimal { precision, scale } => {
                write!(f, "DECIMAL({precision}, {scale})")
            }
            SqlType::Text => write!(f, "TEXT"),
            SqlType::Varchar(len) => write!(f, "VARCHAR({len})"),
            SqlType::Blob => write!(f, "BLOB"),
            SqlType::Uuid => write!(f, "UUID"),
            SqlType::Date => write!(f, "DATE"),
            SqlType::Timestamp => write!(f, "TIMESTAMP"),
            SqlType::TimestampTz => write!(f, "TIMESTAMPTZ"),
            SqlType::Json => write!(f, "JSON"),
        }
    }
}

/// A runtime SQL value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Boolean(bool),
    SmallInt(i16),
    Integer(i64),
    Real(f64),
    Decimal(Decimal),
    Text(String),
    Blob(Vec<u8>),
    Uuid(Uuid),
    Date(NaiveDate),
    Timestamp(NaiveDateTime),
    TimestampTz(DateTime<Utc>),
    Json(serde_json::Value),
}

impl Value {
    /// Returns the `SqlType` of this value, or `None` if it is `Null`.
    pub fn sql_type(&self) -> Option<SqlType> {
        match self {
            Value::Null => None,
            Value::Boolean(_) => Some(SqlType::Boolean),
            Value::SmallInt(_) => Some(SqlType::SmallInt),
            Value::Integer(_) => Some(SqlType::Integer),
            Value::Real(_) => Some(SqlType::Real),
            Value::Decimal(_) => Some(SqlType::Decimal {
                precision: 38,
                scale: 10,
            }),
            Value::Text(_) => Some(SqlType::Text),
            Value::Blob(_) => Some(SqlType::Blob),
            Value::Uuid(_) => Some(SqlType::Uuid),
            Value::Date(_) => Some(SqlType::Date),
            Value::Timestamp(_) => Some(SqlType::Timestamp),
            Value::TimestampTz(_) => Some(SqlType::TimestampTz),
            Value::Json(_) => Some(SqlType::Json),
        }
    }

    /// Returns `true` if this value is `Null`.
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Returns `true` if this value is compatible with the given SQL type.
    ///
    /// `Null` is compatible with any type. Numeric types have some cross-compatibility
    /// (e.g., `SmallInt` is compatible with `Integer` and `BigInt`).
    pub fn is_compatible_with(&self, ty: &SqlType) -> bool {
        match self {
            Value::Null => true,
            Value::Boolean(_) => matches!(ty, SqlType::Boolean),
            Value::SmallInt(_) => matches!(
                ty,
                SqlType::SmallInt | SqlType::Integer | SqlType::BigInt | SqlType::Real | SqlType::Decimal { .. }
            ),
            Value::Integer(_) => matches!(
                ty,
                SqlType::SmallInt | SqlType::Integer | SqlType::BigInt | SqlType::Real | SqlType::Decimal { .. }
            ),
            Value::Real(_) => matches!(ty, SqlType::Real | SqlType::Decimal { .. }),
            Value::Decimal(_) => matches!(ty, SqlType::Decimal { .. } | SqlType::Real),
            Value::Text(_) => matches!(ty, SqlType::Text | SqlType::Varchar(_)),
            Value::Blob(_) => matches!(ty, SqlType::Blob),
            Value::Uuid(_) => matches!(ty, SqlType::Uuid),
            Value::Date(_) => matches!(ty, SqlType::Date | SqlType::Timestamp | SqlType::TimestampTz),
            Value::Timestamp(_) => matches!(ty, SqlType::Timestamp | SqlType::TimestampTz),
            Value::TimestampTz(_) => matches!(ty, SqlType::TimestampTz | SqlType::Timestamp),
            Value::Json(_) => matches!(ty, SqlType::Json),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "NULL"),
            Value::Boolean(v) => write!(f, "{v}"),
            Value::SmallInt(v) => write!(f, "{v}"),
            Value::Integer(v) => write!(f, "{v}"),
            Value::Real(v) => write!(f, "{v}"),
            Value::Decimal(v) => write!(f, "{v}"),
            Value::Text(v) => write!(f, "'{v}'"),
            Value::Blob(v) => write!(f, "x'{}'", hex_encode(v)),
            Value::Uuid(v) => write!(f, "'{v}'"),
            Value::Date(v) => write!(f, "'{v}'"),
            Value::Timestamp(v) => write!(f, "'{v}'"),
            Value::TimestampTz(v) => write!(f, "'{v}'"),
            Value::Json(v) => write!(f, "'{v}'"),
        }
    }
}

/// Simple hex encoding for blob display.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
