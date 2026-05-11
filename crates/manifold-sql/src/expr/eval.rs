use std::cmp::Ordering;

use rust_decimal::Decimal;

use crate::binder::{BinaryOp, UnaryOp};
use crate::error::{Result, SqlError};
use crate::expr::functions;
use crate::planner::plan::ScalarExpr;
use crate::types::{SqlType, Value};

/// Evaluate a scalar expression against a single row, with query parameters.
pub fn evaluate(expr: &ScalarExpr, row: &[Value], params: &[Value]) -> Result<Value> {
    match expr {
        ScalarExpr::ColumnRef { index } => {
            row.get(*index)
                .cloned()
                .ok_or_else(|| SqlError::Execute(format!("column index {index} out of bounds (row has {} columns)", row.len())))
        }

        ScalarExpr::Literal(v) => Ok(v.clone()),

        ScalarExpr::Parameter(i) => {
            params.get(*i)
                .cloned()
                .ok_or_else(|| SqlError::Execute(format!("parameter index {i} out of bounds ({} parameters provided)", params.len())))
        }

        ScalarExpr::BinaryOp { op, left, right } => {
            let lv = evaluate(left, row, params)?;
            let rv = evaluate(right, row, params)?;
            apply_binary_op(*op, lv, rv)
        }

        ScalarExpr::UnaryOp { op, operand } => {
            let val = evaluate(operand, row, params)?;
            apply_unary_op(*op, val)
        }

        ScalarExpr::IsNull { operand, negated } => {
            let val = evaluate(operand, row, params)?;
            let is_null = val.is_null();
            Ok(Value::Boolean(if *negated { !is_null } else { is_null }))
        }

        ScalarExpr::InList { expr, list, negated } => {
            let val = evaluate(expr, row, params)?;
            if val.is_null() {
                return Ok(Value::Null);
            }
            let mut found_null = false;
            for item_expr in list {
                let item = evaluate(item_expr, row, params)?;
                if item.is_null() {
                    found_null = true;
                    continue;
                }
                if values_equal(&val, &item) {
                    return Ok(Value::Boolean(!negated));
                }
            }
            // Not found — if any list item was NULL, result is NULL (SQL three-valued logic)
            if found_null && !negated {
                Ok(Value::Null)
            } else {
                Ok(Value::Boolean(*negated))
            }
        }

        ScalarExpr::Between { expr, low, high, negated } => {
            let val = evaluate(expr, row, params)?;
            let lo = evaluate(low, row, params)?;
            let hi = evaluate(high, row, params)?;
            if val.is_null() || lo.is_null() || hi.is_null() {
                return Ok(Value::Null);
            }
            let above_low = compare_values(&val, &lo) != Ordering::Less;
            let below_high = compare_values(&val, &hi) != Ordering::Greater;
            let result = above_low && below_high;
            Ok(Value::Boolean(if *negated { !result } else { result }))
        }

        ScalarExpr::Like { expr, pattern, negated } => {
            let val = evaluate(expr, row, params)?;
            let pat = evaluate(pattern, row, params)?;
            if val.is_null() || pat.is_null() {
                return Ok(Value::Null);
            }
            let text = match &val {
                Value::Text(s) => s.as_str(),
                _ => return Err(SqlError::TypeError(format!("LIKE requires text operand, got {}", val))),
            };
            let pattern_str = match &pat {
                Value::Text(s) => s.as_str(),
                _ => return Err(SqlError::TypeError(format!("LIKE requires text pattern, got {}", pat))),
            };
            let matched = like_match(text, pattern_str);
            Ok(Value::Boolean(if *negated { !matched } else { matched }))
        }

        ScalarExpr::Function { name, args } => {
            let mut evaluated_args = Vec::with_capacity(args.len());
            for arg in args {
                evaluated_args.push(evaluate(arg, row, params)?);
            }
            functions::call_function(name, &evaluated_args)
        }

        ScalarExpr::Cast { expr, target_type } => {
            let val = evaluate(expr, row, params)?;
            cast_value(&val, target_type)
        }
    }
}

// ---------------------------------------------------------------------------
// Binary operator application
// ---------------------------------------------------------------------------

fn apply_binary_op(op: BinaryOp, left: Value, right: Value) -> Result<Value> {
    // Logical operators with three-valued logic (short-circuit already happened above if desired;
    // we do it here with the evaluated values).
    match op {
        BinaryOp::And => {
            return match (&left, &right) {
                (Value::Boolean(false), _) | (_, Value::Boolean(false)) => Ok(Value::Boolean(false)),
                (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
                (Value::Boolean(l), Value::Boolean(r)) => Ok(Value::Boolean(*l && *r)),
                _ => Err(SqlError::TypeError("AND requires boolean operands".to_string())),
            };
        }
        BinaryOp::Or => {
            return match (&left, &right) {
                (Value::Boolean(true), _) | (_, Value::Boolean(true)) => Ok(Value::Boolean(true)),
                (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
                (Value::Boolean(l), Value::Boolean(r)) => Ok(Value::Boolean(*l || *r)),
                _ => Err(SqlError::TypeError("OR requires boolean operands".to_string())),
            };
        }
        _ => {}
    }

    // NULL propagation for all other operators
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }

    match op {
        BinaryOp::Add => numeric_arith(left, right, "+"),
        BinaryOp::Sub => numeric_arith(left, right, "-"),
        BinaryOp::Mul => numeric_arith(left, right, "*"),
        BinaryOp::Div => numeric_arith(left, right, "/"),
        BinaryOp::Mod => numeric_arith(left, right, "%"),
        BinaryOp::Eq => Ok(Value::Boolean(values_equal(&left, &right))),
        BinaryOp::Neq => Ok(Value::Boolean(!values_equal(&left, &right))),
        BinaryOp::Lt => Ok(Value::Boolean(compare_values(&left, &right) == Ordering::Less)),
        BinaryOp::Gt => Ok(Value::Boolean(compare_values(&left, &right) == Ordering::Greater)),
        BinaryOp::Lte => Ok(Value::Boolean(compare_values(&left, &right) != Ordering::Greater)),
        BinaryOp::Gte => Ok(Value::Boolean(compare_values(&left, &right) != Ordering::Less)),
        BinaryOp::And | BinaryOp::Or => unreachable!("handled above"),
    }
}

fn coerce_numeric(left: Value, right: Value) -> Result<(NumericPair, bool)> {
    // Returns the coerced pair and whether the inputs were already integers (for Mod)
    match (&left, &right) {
        (Value::Integer(_) | Value::SmallInt(_), Value::Integer(_) | Value::SmallInt(_)) => {
            let l = to_i64(&left)?;
            let r = to_i64(&right)?;
            Ok((NumericPair::Int(l, r), true))
        }
        (Value::Decimal(_), _) | (_, Value::Decimal(_)) => {
            let l = to_decimal(&left)?;
            let r = to_decimal(&right)?;
            Ok((NumericPair::Decimal(l, r), false))
        }
        (Value::Real(_), _) | (_, Value::Real(_)) => {
            let l = to_f64(&left)?;
            let r = to_f64(&right)?;
            Ok((NumericPair::Real(l, r), false))
        }
        _ => Err(SqlError::TypeError(format!("arithmetic requires numeric operands, got {} and {}", left, right))),
    }
}

enum NumericPair {
    Int(i64, i64),
    Real(f64, f64),
    Decimal(Decimal, Decimal),
}

fn numeric_arith(left: Value, right: Value, op: &str) -> Result<Value> {
    let (pair, _) = coerce_numeric(left, right)?;
    match pair {
        NumericPair::Int(l, r) => match op {
            "+" => Ok(Value::Integer(l.wrapping_add(r))),
            "-" => Ok(Value::Integer(l.wrapping_sub(r))),
            "*" => Ok(Value::Integer(l.wrapping_mul(r))),
            "/" => {
                if r == 0 {
                    Err(SqlError::Execute("division by zero".to_string()))
                } else {
                    Ok(Value::Integer(l / r))
                }
            }
            "%" => {
                if r == 0 {
                    Err(SqlError::Execute("modulo by zero".to_string()))
                } else {
                    Ok(Value::Integer(l % r))
                }
            }
            _ => unreachable!(),
        },
        NumericPair::Real(l, r) => match op {
            "+" => Ok(Value::Real(l + r)),
            "-" => Ok(Value::Real(l - r)),
            "*" => Ok(Value::Real(l * r)),
            "/" => {
                if r == 0.0 {
                    Err(SqlError::Execute("division by zero".to_string()))
                } else {
                    Ok(Value::Real(l / r))
                }
            }
            "%" => Ok(Value::Real(l % r)),
            _ => unreachable!(),
        },
        NumericPair::Decimal(l, r) => match op {
            "+" => Ok(Value::Decimal(l + r)),
            "-" => Ok(Value::Decimal(l - r)),
            "*" => Ok(Value::Decimal(l * r)),
            "/" => {
                if r.is_zero() {
                    Err(SqlError::Execute("division by zero".to_string()))
                } else {
                    Ok(Value::Decimal(l / r))
                }
            }
            "%" => Ok(Value::Decimal(l % r)),
            _ => unreachable!(),
        },
    }
}

fn to_i64(v: &Value) -> Result<i64> {
    match v {
        Value::Integer(i) => Ok(*i),
        Value::SmallInt(i) => Ok(*i as i64),
        _ => Err(SqlError::TypeError(format!("expected integer, got {}", v))),
    }
}

fn to_f64(v: &Value) -> Result<f64> {
    match v {
        Value::Real(f) => Ok(*f),
        Value::Integer(i) => Ok(*i as f64),
        Value::SmallInt(i) => Ok(*i as f64),
        Value::Decimal(d) => Ok(rust_decimal::prelude::ToPrimitive::to_f64(d).unwrap_or(f64::NAN)),
        _ => Err(SqlError::TypeError(format!("expected numeric, got {}", v))),
    }
}

fn to_decimal(v: &Value) -> Result<Decimal> {
    match v {
        Value::Decimal(d) => Ok(*d),
        Value::Integer(i) => Ok(Decimal::from(*i)),
        Value::SmallInt(i) => Ok(Decimal::from(*i)),
        Value::Real(f) => Decimal::try_from(*f).map_err(|e| SqlError::TypeError(e.to_string())),
        _ => Err(SqlError::TypeError(format!("expected numeric, got {}", v))),
    }
}

// ---------------------------------------------------------------------------
// Unary operator application
// ---------------------------------------------------------------------------

fn apply_unary_op(op: UnaryOp, val: Value) -> Result<Value> {
    if val.is_null() {
        return Ok(Value::Null);
    }
    match op {
        UnaryOp::Neg => match val {
            Value::Integer(i) => Ok(Value::Integer(-i)),
            Value::SmallInt(i) => Ok(Value::SmallInt(-i)),
            Value::Real(f) => Ok(Value::Real(-f)),
            Value::Decimal(d) => Ok(Value::Decimal(-d)),
            _ => Err(SqlError::TypeError(format!("unary minus requires numeric operand, got {}", val))),
        },
        UnaryOp::Not => match val {
            Value::Boolean(b) => Ok(Value::Boolean(!b)),
            _ => Err(SqlError::TypeError(format!("NOT requires boolean operand, got {}", val))),
        },
    }
}

// ---------------------------------------------------------------------------
// Value comparison helpers
// ---------------------------------------------------------------------------

fn values_equal(left: &Value, right: &Value) -> bool {
    compare_values(left, right) == Ordering::Equal
}

/// Cross-type ordering for SQL values. NULLs sort before everything else.
pub fn compare_values(left: &Value, right: &Value) -> Ordering {
    match (left, right) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,

        (Value::Boolean(l), Value::Boolean(r)) => l.cmp(r),
        (Value::SmallInt(l), Value::SmallInt(r)) => l.cmp(r),
        (Value::Integer(l), Value::Integer(r)) => l.cmp(r),
        (Value::Real(l), Value::Real(r)) => l.partial_cmp(r).unwrap_or(Ordering::Equal),
        (Value::Decimal(l), Value::Decimal(r)) => l.cmp(r),
        (Value::Text(l), Value::Text(r)) => l.cmp(r),
        (Value::Blob(l), Value::Blob(r)) => l.cmp(r),
        (Value::Uuid(l), Value::Uuid(r)) => l.cmp(r),
        (Value::Date(l), Value::Date(r)) => l.cmp(r),
        (Value::Timestamp(l), Value::Timestamp(r)) => l.cmp(r),
        (Value::TimestampTz(l), Value::TimestampTz(r)) => l.cmp(r),

        // Cross-numeric comparisons: promote to f64
        (l, r) => {
            let lf = numeric_to_f64(l);
            let rf = numeric_to_f64(r);
            match (lf, rf) {
                (Some(lv), Some(rv)) => lv.partial_cmp(&rv).unwrap_or(Ordering::Equal),
                _ => {
                    // Fall back to type-name ordering for incompatible types
                    type_name(left).cmp(type_name(right))
                }
            }
        }
    }
}

fn numeric_to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::SmallInt(i) => Some(*i as f64),
        Value::Integer(i) => Some(*i as f64),
        Value::Real(f) => Some(*f),
        Value::Decimal(d) => rust_decimal::prelude::ToPrimitive::to_f64(d),
        _ => None,
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
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
    }
}

// ---------------------------------------------------------------------------
// LIKE pattern matching
// ---------------------------------------------------------------------------

/// LIKE pattern matching: `%` matches any sequence, `_` matches any single character.
fn like_match(text: &str, pattern: &str) -> bool {
    like_match_bytes(text.as_bytes(), pattern.as_bytes())
}

fn like_match_bytes(text: &[u8], pattern: &[u8]) -> bool {
    match pattern.first() {
        None => text.is_empty(),
        Some(&b'%') => {
            // % matches zero or more characters
            // Try matching the rest of the pattern at each position in text
            let rest = &pattern[1..];
            for i in 0..=text.len() {
                if like_match_bytes(&text[i..], rest) {
                    return true;
                }
            }
            false
        }
        Some(&b'_') => {
            // _ matches exactly one character
            if text.is_empty() {
                false
            } else {
                like_match_bytes(&text[1..], &pattern[1..])
            }
        }
        Some(&c) => {
            // Literal character match
            if text.is_empty() || text[0] != c {
                false
            } else {
                like_match_bytes(&text[1..], &pattern[1..])
            }
        }
    }
}

// ---------------------------------------------------------------------------
// CAST
// ---------------------------------------------------------------------------

/// Convert a value to a target SQL type.
pub fn cast_value(val: &Value, target: &SqlType) -> Result<Value> {
    if val.is_null() {
        return Ok(Value::Null);
    }

    match target {
        SqlType::Text | SqlType::Varchar(_) => {
            let s = match val {
                Value::Text(s) => s.clone(),
                Value::Integer(i) => i.to_string(),
                Value::SmallInt(i) => i.to_string(),
                Value::Real(f) => f.to_string(),
                Value::Decimal(d) => d.to_string(),
                Value::Boolean(b) => b.to_string(),
                Value::Uuid(u) => u.to_string(),
                Value::Date(d) => d.to_string(),
                Value::Timestamp(t) => t.to_string(),
                Value::TimestampTz(t) => t.to_string(),
                other => return Err(SqlError::TypeError(format!("cannot cast {} to TEXT", other))),
            };
            Ok(Value::Text(s))
        }

        SqlType::Integer => {
            match val {
                Value::Integer(i) => Ok(Value::Integer(*i)),
                Value::SmallInt(i) => Ok(Value::Integer(*i as i64)),
                Value::Real(f) => Ok(Value::Integer(*f as i64)),
                Value::Decimal(d) => {
                    use rust_decimal::prelude::ToPrimitive;
                    d.to_i64()
                        .map(Value::Integer)
                        .ok_or_else(|| SqlError::TypeError(format!("cannot cast {d} to INTEGER")))
                }
                Value::Boolean(b) => Ok(Value::Integer(if *b { 1 } else { 0 })),
                Value::Text(s) => s.trim().parse::<i64>()
                    .map(Value::Integer)
                    .map_err(|_| SqlError::TypeError(format!("cannot cast '{s}' to INTEGER"))),
                other => Err(SqlError::TypeError(format!("cannot cast {} to INTEGER", other))),
            }
        }

        SqlType::SmallInt => {
            match val {
                Value::SmallInt(i) => Ok(Value::SmallInt(*i)),
                Value::Integer(i) => Ok(Value::SmallInt(*i as i16)),
                Value::Real(f) => Ok(Value::SmallInt(*f as i16)),
                Value::Boolean(b) => Ok(Value::SmallInt(if *b { 1 } else { 0 })),
                Value::Text(s) => s.trim().parse::<i16>()
                    .map(Value::SmallInt)
                    .map_err(|_| SqlError::TypeError(format!("cannot cast '{s}' to SMALLINT"))),
                other => Err(SqlError::TypeError(format!("cannot cast {} to SMALLINT", other))),
            }
        }

        SqlType::BigInt => {
            // BigInt stored as Integer(i64) — same as INTEGER cast
            match val {
                Value::Integer(i) => Ok(Value::Integer(*i)),
                Value::SmallInt(i) => Ok(Value::Integer(*i as i64)),
                Value::Real(f) => Ok(Value::Integer(*f as i64)),
                Value::Boolean(b) => Ok(Value::Integer(if *b { 1 } else { 0 })),
                Value::Text(s) => s.trim().parse::<i64>()
                    .map(Value::Integer)
                    .map_err(|_| SqlError::TypeError(format!("cannot cast '{s}' to BIGINT"))),
                other => Err(SqlError::TypeError(format!("cannot cast {} to BIGINT", other))),
            }
        }

        SqlType::Real => {
            match val {
                Value::Real(f) => Ok(Value::Real(*f)),
                Value::Integer(i) => Ok(Value::Real(*i as f64)),
                Value::SmallInt(i) => Ok(Value::Real(*i as f64)),
                Value::Decimal(d) => {
                    use rust_decimal::prelude::ToPrimitive;
                    Ok(Value::Real(d.to_f64().unwrap_or(f64::NAN)))
                }
                Value::Text(s) => s.trim().parse::<f64>()
                    .map(Value::Real)
                    .map_err(|_| SqlError::TypeError(format!("cannot cast '{s}' to REAL"))),
                other => Err(SqlError::TypeError(format!("cannot cast {} to REAL", other))),
            }
        }

        SqlType::Decimal { .. } => {
            match val {
                Value::Decimal(d) => Ok(Value::Decimal(*d)),
                Value::Integer(i) => Ok(Value::Decimal(Decimal::from(*i))),
                Value::SmallInt(i) => Ok(Value::Decimal(Decimal::from(*i))),
                Value::Real(f) => Decimal::try_from(*f)
                    .map(Value::Decimal)
                    .map_err(|e| SqlError::TypeError(e.to_string())),
                Value::Text(s) => s.trim().parse::<Decimal>()
                    .map(Value::Decimal)
                    .map_err(|_| SqlError::TypeError(format!("cannot cast '{s}' to DECIMAL"))),
                other => Err(SqlError::TypeError(format!("cannot cast {} to DECIMAL", other))),
            }
        }

        SqlType::Boolean => {
            match val {
                Value::Boolean(b) => Ok(Value::Boolean(*b)),
                Value::Integer(i) => Ok(Value::Boolean(*i != 0)),
                Value::SmallInt(i) => Ok(Value::Boolean(*i != 0)),
                Value::Text(s) => match s.to_lowercase().as_str() {
                    "true" | "t" | "yes" | "1" => Ok(Value::Boolean(true)),
                    "false" | "f" | "no" | "0" => Ok(Value::Boolean(false)),
                    _ => Err(SqlError::TypeError(format!("cannot cast '{s}' to BOOLEAN"))),
                },
                other => Err(SqlError::TypeError(format!("cannot cast {} to BOOLEAN", other))),
            }
        }

        other => Err(SqlError::TypeError(format!("CAST to {other} not supported"))),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binder::BinaryOp;
    use crate::planner::plan::ScalarExpr;
    use crate::types::Value;

    fn eval(expr: &ScalarExpr) -> Value {
        evaluate(expr, &[], &[]).unwrap()
    }

    fn eval_row(expr: &ScalarExpr, row: &[Value]) -> Value {
        evaluate(expr, row, &[]).unwrap()
    }

    fn eval_params(expr: &ScalarExpr, params: &[Value]) -> Value {
        evaluate(expr, &[], params).unwrap()
    }

    #[test]
    fn literal() {
        let expr = ScalarExpr::Literal(Value::Integer(42));
        assert_eq!(eval(&expr), Value::Integer(42));
    }

    #[test]
    fn column_ref() {
        let row = vec![Value::Integer(1), Value::Text("hello".to_string()), Value::Boolean(true)];
        let expr = ScalarExpr::ColumnRef { index: 1 };
        assert_eq!(eval_row(&expr, &row), Value::Text("hello".to_string()));
    }

    #[test]
    fn addition() {
        let expr = ScalarExpr::BinaryOp {
            op: BinaryOp::Add,
            left: Box::new(ScalarExpr::Literal(Value::Integer(3))),
            right: Box::new(ScalarExpr::Literal(Value::Integer(4))),
        };
        assert_eq!(eval(&expr), Value::Integer(7));
    }

    #[test]
    fn comparison() {
        let expr = ScalarExpr::BinaryOp {
            op: BinaryOp::Gt,
            left: Box::new(ScalarExpr::Literal(Value::Integer(10))),
            right: Box::new(ScalarExpr::Literal(Value::Integer(5))),
        };
        assert_eq!(eval(&expr), Value::Boolean(true));

        let expr2 = ScalarExpr::BinaryOp {
            op: BinaryOp::Gt,
            left: Box::new(ScalarExpr::Literal(Value::Integer(3))),
            right: Box::new(ScalarExpr::Literal(Value::Integer(5))),
        };
        assert_eq!(eval(&expr2), Value::Boolean(false));
    }

    #[test]
    fn null_propagation() {
        let expr = ScalarExpr::BinaryOp {
            op: BinaryOp::Add,
            left: Box::new(ScalarExpr::Literal(Value::Null)),
            right: Box::new(ScalarExpr::Literal(Value::Integer(5))),
        };
        assert_eq!(eval(&expr), Value::Null);
    }

    #[test]
    fn is_null_check() {
        let expr = ScalarExpr::IsNull {
            operand: Box::new(ScalarExpr::Literal(Value::Null)),
            negated: false,
        };
        assert_eq!(eval(&expr), Value::Boolean(true));

        let expr2 = ScalarExpr::IsNull {
            operand: Box::new(ScalarExpr::Literal(Value::Integer(1))),
            negated: false,
        };
        assert_eq!(eval(&expr2), Value::Boolean(false));
    }

    #[test]
    fn like_pattern() {
        let expr = ScalarExpr::Like {
            expr: Box::new(ScalarExpr::Literal(Value::Text("hello world".to_string()))),
            pattern: Box::new(ScalarExpr::Literal(Value::Text("hello%".to_string()))),
            negated: false,
        };
        assert_eq!(eval(&expr), Value::Boolean(true));

        let expr2 = ScalarExpr::Like {
            expr: Box::new(ScalarExpr::Literal(Value::Text("hello world".to_string()))),
            pattern: Box::new(ScalarExpr::Literal(Value::Text("world%".to_string()))),
            negated: false,
        };
        assert_eq!(eval(&expr2), Value::Boolean(false));
    }

    #[test]
    fn in_list() {
        let expr = ScalarExpr::InList {
            expr: Box::new(ScalarExpr::Literal(Value::Integer(3))),
            list: vec![
                ScalarExpr::Literal(Value::Integer(1)),
                ScalarExpr::Literal(Value::Integer(2)),
                ScalarExpr::Literal(Value::Integer(3)),
            ],
            negated: false,
        };
        assert_eq!(eval(&expr), Value::Boolean(true));

        let expr2 = ScalarExpr::InList {
            expr: Box::new(ScalarExpr::Literal(Value::Integer(5))),
            list: vec![
                ScalarExpr::Literal(Value::Integer(1)),
                ScalarExpr::Literal(Value::Integer(2)),
                ScalarExpr::Literal(Value::Integer(3)),
            ],
            negated: false,
        };
        assert_eq!(eval(&expr2), Value::Boolean(false));
    }

    #[test]
    fn between() {
        let expr = ScalarExpr::Between {
            expr: Box::new(ScalarExpr::Literal(Value::Integer(5))),
            low: Box::new(ScalarExpr::Literal(Value::Integer(1))),
            high: Box::new(ScalarExpr::Literal(Value::Integer(10))),
            negated: false,
        };
        assert_eq!(eval(&expr), Value::Boolean(true));

        let expr2 = ScalarExpr::Between {
            expr: Box::new(ScalarExpr::Literal(Value::Integer(15))),
            low: Box::new(ScalarExpr::Literal(Value::Integer(1))),
            high: Box::new(ScalarExpr::Literal(Value::Integer(10))),
            negated: false,
        };
        assert_eq!(eval(&expr2), Value::Boolean(false));
    }

    #[test]
    fn parameter() {
        let expr = ScalarExpr::Parameter(0);
        let params = vec![Value::Text("test_param".to_string())];
        assert_eq!(eval_params(&expr, &params), Value::Text("test_param".to_string()));
    }
}
