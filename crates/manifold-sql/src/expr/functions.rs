use crate::error::{Result, SqlError};
use crate::types::Value;

/// Dispatch a built-in scalar function call.
pub fn call_function(name: &str, args: &[Value]) -> Result<Value> {
    match name.to_uppercase().as_str() {
        "COALESCE" => coalesce(args),
        "UPPER" => upper(args),
        "LOWER" => lower(args),
        "LENGTH" => length(args),
        "ABS" => abs(args),
        "TYPEOF" => typeof_fn(args),
        other => Err(SqlError::Execute(format!("unknown function: {other}"))),
    }
}

// ---------------------------------------------------------------------------
// Built-in implementations
// ---------------------------------------------------------------------------

/// COALESCE(v1, v2, ...) — return the first non-NULL argument.
fn coalesce(args: &[Value]) -> Result<Value> {
    if args.is_empty() {
        return Err(SqlError::Execute(
            "COALESCE requires at least one argument".to_string(),
        ));
    }
    for v in args {
        if !v.is_null() {
            return Ok(v.clone());
        }
    }
    Ok(Value::Null)
}

/// UPPER(text) — convert text to uppercase.
fn upper(args: &[Value]) -> Result<Value> {
    check_arity("UPPER", args, 1)?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => Ok(Value::Text(s.to_uppercase())),
        other => Err(SqlError::TypeError(format!(
            "UPPER requires text argument, got {}",
            other
        ))),
    }
}

/// LOWER(text) — convert text to lowercase.
fn lower(args: &[Value]) -> Result<Value> {
    check_arity("LOWER", args, 1)?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => Ok(Value::Text(s.to_lowercase())),
        other => Err(SqlError::TypeError(format!(
            "LOWER requires text argument, got {}",
            other
        ))),
    }
}

/// LENGTH(text) — return the number of characters in the string.
fn length(args: &[Value]) -> Result<Value> {
    check_arity("LENGTH", args, 1)?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Text(s) => Ok(Value::Integer(s.chars().count() as i64)),
        Value::Blob(b) => Ok(Value::Integer(b.len() as i64)),
        other => Err(SqlError::TypeError(format!(
            "LENGTH requires text or blob argument, got {}",
            other
        ))),
    }
}

/// ABS(numeric) — return the absolute value.
fn abs(args: &[Value]) -> Result<Value> {
    check_arity("ABS", args, 1)?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Integer(i) => Ok(Value::Integer(i.abs())),
        Value::SmallInt(i) => Ok(Value::SmallInt(i.abs())),
        Value::Real(f) => Ok(Value::Real(f.abs())),
        Value::Decimal(d) => Ok(Value::Decimal(d.abs())),
        other => Err(SqlError::TypeError(format!(
            "ABS requires numeric argument, got {}",
            other
        ))),
    }
}

/// TYPEOF(value) — return a text description of the value's type.
fn typeof_fn(args: &[Value]) -> Result<Value> {
    check_arity("TYPEOF", args, 1)?;
    let type_name = match &args[0] {
        Value::Null => "null",
        Value::Boolean(_) => "boolean",
        Value::SmallInt(_) => "smallint",
        Value::Integer(_) => "integer",
        Value::Real(_) => "real",
        Value::Decimal(_) => "decimal",
        Value::Text(_) => "text",
        Value::Blob(_) => "blob",
        Value::Uuid(_) => "uuid",
        Value::Date(_) => "date",
        Value::Timestamp(_) => "timestamp",
        Value::TimestampTz(_) => "timestamptz",
        Value::Json(_) => "json",
    };
    Ok(Value::Text(type_name.to_string()))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn check_arity(name: &str, args: &[Value], expected: usize) -> Result<()> {
    if args.len() != expected {
        Err(SqlError::Execute(format!(
            "{name} expects {expected} argument(s), got {}",
            args.len()
        )))
    } else {
        Ok(())
    }
}
