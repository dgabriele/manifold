use std::collections::HashMap;

use crate::binder::AggregateFunc;
use crate::error::{Result, SqlError};
use crate::expr::eval::{compare_values, evaluate};
use crate::planner::plan::{AggregateExpr, PlanSchema, ScalarExpr};
use crate::types::Value;

/// Hash aggregate operator: groups rows by GROUP BY expressions and computes
/// aggregate functions using a HashMap for grouping.
pub struct HashAggregate {
    group_by_exprs: Vec<ScalarExpr>,
    aggregate_exprs: Vec<AggregateExpr>,
    params: Vec<Value>,
    #[allow(dead_code)]
    schema: PlanSchema,
    results: Vec<Vec<Value>>,
    position: usize,
    materialized: bool,
    input: Option<Box<dyn super::Executor>>,
}

impl HashAggregate {
    pub fn new(
        input: Box<dyn super::Executor>,
        group_by_exprs: Vec<ScalarExpr>,
        aggregate_exprs: Vec<AggregateExpr>,
        params: &[Value],
        schema: PlanSchema,
    ) -> Self {
        Self {
            input: Some(input),
            group_by_exprs,
            aggregate_exprs,
            params: params.to_vec(),
            schema,
            results: Vec::new(),
            position: 0,
            materialized: false,
        }
    }

    fn materialize(&mut self) -> Result<()> {
        let mut input = self.input.take().unwrap();

        // Map from group-key to vector of accumulators.
        // We use Vec<(Vec<Value>, Vec<Accumulator>)> alongside a HashMap for key lookup
        // because Value does not implement Hash.
        let mut group_map: HashMap<GroupKey, usize> = HashMap::new();
        let mut groups: Vec<(Vec<Value>, Vec<Accumulator>)> = Vec::new();

        while let Some(row) = input.next()? {
            // Evaluate group-by expressions to build the key.
            let key_values: Vec<Value> = self
                .group_by_exprs
                .iter()
                .map(|expr| evaluate(expr, &row, &self.params))
                .collect::<Result<Vec<_>>>()?;

            let gk = GroupKey::from_values(&key_values);

            let group_idx = if let Some(&idx) = group_map.get(&gk) {
                idx
            } else {
                let idx = groups.len();
                let accumulators = self
                    .aggregate_exprs
                    .iter()
                    .map(|ae| Accumulator::new(&ae.func, ae.distinct))
                    .collect();
                groups.push((key_values, accumulators));
                group_map.insert(gk, idx);
                idx
            };

            // Update accumulators with this row.
            let accumulators = &mut groups[group_idx].1;
            for (i, agg_expr) in self.aggregate_exprs.iter().enumerate() {
                let arg_val = match &agg_expr.arg {
                    Some(expr) => Some(evaluate(expr, &row, &self.params)?),
                    None => None, // COUNT(*)
                };
                accumulators[i].accumulate(arg_val)?;
            }
        }

        // If there are no groups and no GROUP BY (scalar aggregate like COUNT(*)),
        // produce a single row with initial accumulator values.
        if groups.is_empty() && self.group_by_exprs.is_empty() {
            let accumulators: Vec<Accumulator> = self
                .aggregate_exprs
                .iter()
                .map(|ae| Accumulator::new(&ae.func, ae.distinct))
                .collect();
            groups.push((Vec::new(), accumulators));
        }

        // Convert groups into result rows: [group_by_values..., aggregate_results...]
        self.results = groups
            .into_iter()
            .map(|(key_vals, accumulators)| {
                let mut row = key_vals;
                for acc in accumulators {
                    row.push(acc.finalize()?);
                }
                Ok(row)
            })
            .collect::<Result<Vec<_>>>()?;

        self.materialized = true;
        Ok(())
    }
}

impl super::Executor for HashAggregate {
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        if !self.materialized {
            self.materialize()?;
        }
        if self.position < self.results.len() {
            let row = self.results[self.position].clone();
            self.position += 1;
            Ok(Some(row))
        } else {
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// GroupKey — a hashable wrapper around Vec<Value>
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct GroupKey {
    /// We store the debug/canonical representation for hashing.
    /// For correctness we use a byte-level encoding.
    bytes: Vec<u8>,
}

impl GroupKey {
    fn from_values(values: &[Value]) -> Self {
        // Build a deterministic byte representation of the values.
        let mut bytes = Vec::new();
        for v in values {
            // Tag byte + value bytes
            match v {
                Value::Null => bytes.push(0),
                Value::Boolean(b) => {
                    bytes.push(1);
                    bytes.push(if *b { 1 } else { 0 });
                }
                Value::SmallInt(i) => {
                    bytes.push(2);
                    bytes.extend_from_slice(&i.to_le_bytes());
                }
                Value::Integer(i) => {
                    bytes.push(3);
                    bytes.extend_from_slice(&i.to_le_bytes());
                }
                Value::Real(f) => {
                    bytes.push(4);
                    bytes.extend_from_slice(&f.to_bits().to_le_bytes());
                }
                Value::Decimal(d) => {
                    bytes.push(5);
                    bytes.extend_from_slice(&d.serialize());
                }
                Value::Text(s) => {
                    bytes.push(6);
                    bytes.extend_from_slice(&(s.len() as u32).to_le_bytes());
                    bytes.extend_from_slice(s.as_bytes());
                }
                Value::Blob(b) => {
                    bytes.push(7);
                    bytes.extend_from_slice(&(b.len() as u32).to_le_bytes());
                    bytes.extend_from_slice(b);
                }
                Value::Uuid(u) => {
                    bytes.push(8);
                    bytes.extend_from_slice(u.as_bytes());
                }
                Value::Date(d) => {
                    bytes.push(9);
                    let s = d.to_string();
                    bytes.extend_from_slice(&(s.len() as u32).to_le_bytes());
                    bytes.extend_from_slice(s.as_bytes());
                }
                Value::Timestamp(t) => {
                    bytes.push(10);
                    let s = t.to_string();
                    bytes.extend_from_slice(&(s.len() as u32).to_le_bytes());
                    bytes.extend_from_slice(s.as_bytes());
                }
                Value::TimestampTz(t) => {
                    bytes.push(11);
                    let s = t.to_string();
                    bytes.extend_from_slice(&(s.len() as u32).to_le_bytes());
                    bytes.extend_from_slice(s.as_bytes());
                }
                Value::Json(j) => {
                    bytes.push(12);
                    let s = j.to_string();
                    bytes.extend_from_slice(&(s.len() as u32).to_le_bytes());
                    bytes.extend_from_slice(s.as_bytes());
                }
            }
        }
        Self { bytes }
    }
}

impl PartialEq for GroupKey {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl Eq for GroupKey {}

impl std::hash::Hash for GroupKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.bytes.hash(state);
    }
}

// ---------------------------------------------------------------------------
// Accumulator
// ---------------------------------------------------------------------------

enum Accumulator {
    Count {
        count: i64,
        distinct_values: Option<Vec<Value>>,
    },
    Sum {
        sum: Value,
        distinct_values: Option<Vec<Value>>,
    },
    Avg {
        sum: Value,
        count: i64,
        distinct_values: Option<Vec<Value>>,
    },
    Min {
        min: Option<Value>,
    },
    Max {
        max: Option<Value>,
    },
}

impl Accumulator {
    fn new(func: &AggregateFunc, distinct: bool) -> Self {
        match func {
            AggregateFunc::Count => Accumulator::Count {
                count: 0,
                distinct_values: if distinct { Some(Vec::new()) } else { None },
            },
            AggregateFunc::Sum => Accumulator::Sum {
                sum: Value::Null,
                distinct_values: if distinct { Some(Vec::new()) } else { None },
            },
            AggregateFunc::Avg => Accumulator::Avg {
                sum: Value::Null,
                count: 0,
                distinct_values: if distinct { Some(Vec::new()) } else { None },
            },
            AggregateFunc::Min => Accumulator::Min { min: None },
            AggregateFunc::Max => Accumulator::Max { max: None },
        }
    }

    fn accumulate(&mut self, arg: Option<Value>) -> Result<()> {
        match self {
            Accumulator::Count {
                count,
                distinct_values,
            } => {
                match arg {
                    None => {
                        // COUNT(*) — count all rows
                        *count += 1;
                    }
                    Some(val) => {
                        if val.is_null() {
                            return Ok(());
                        }
                        if let Some(seen) = distinct_values {
                            if seen.contains(&val) {
                                return Ok(());
                            }
                            seen.push(val);
                        }
                        *count += 1;
                    }
                }
            }
            Accumulator::Sum {
                sum,
                distinct_values,
            } => {
                let val = match arg {
                    Some(v) => v,
                    None => return Ok(()),
                };
                if val.is_null() {
                    return Ok(());
                }
                if let Some(seen) = distinct_values {
                    if seen.contains(&val) {
                        return Ok(());
                    }
                    seen.push(val.clone());
                }
                *sum = add_values(sum, &val)?;
            }
            Accumulator::Avg {
                sum,
                count,
                distinct_values,
            } => {
                let val = match arg {
                    Some(v) => v,
                    None => return Ok(()),
                };
                if val.is_null() {
                    return Ok(());
                }
                if let Some(seen) = distinct_values {
                    if seen.contains(&val) {
                        return Ok(());
                    }
                    seen.push(val.clone());
                }
                *sum = add_values(sum, &val)?;
                *count += 1;
            }
            Accumulator::Min { min } => {
                let val = match arg {
                    Some(v) => v,
                    None => return Ok(()),
                };
                if val.is_null() {
                    return Ok(());
                }
                match min {
                    None => *min = Some(val),
                    Some(current) => {
                        if compare_values(&val, current) == std::cmp::Ordering::Less {
                            *min = Some(val);
                        }
                    }
                }
            }
            Accumulator::Max { max } => {
                let val = match arg {
                    Some(v) => v,
                    None => return Ok(()),
                };
                if val.is_null() {
                    return Ok(());
                }
                match max {
                    None => *max = Some(val),
                    Some(current) => {
                        if compare_values(&val, current) == std::cmp::Ordering::Greater {
                            *max = Some(val);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn finalize(self) -> Result<Value> {
        match self {
            Accumulator::Count { count, .. } => Ok(Value::Integer(count)),
            Accumulator::Sum { sum, .. } => Ok(sum),
            Accumulator::Avg { sum, count, .. } => {
                if count == 0 {
                    return Ok(Value::Null);
                }
                let sum_f64 = value_to_f64(&sum)?;
                Ok(Value::Real(sum_f64 / count as f64))
            }
            Accumulator::Min { min } => Ok(min.unwrap_or(Value::Null)),
            Accumulator::Max { max } => Ok(max.unwrap_or(Value::Null)),
        }
    }
}

// ---------------------------------------------------------------------------
// Arithmetic helpers
// ---------------------------------------------------------------------------

/// Add a value to a running sum. The sum starts as Null (identity for addition).
fn add_values(current: &Value, new: &Value) -> Result<Value> {
    if current.is_null() {
        return Ok(new.clone());
    }
    // Promote Integer + Real to Real
    match (current, new) {
        (Value::Integer(a), Value::Integer(b)) => Ok(Value::Integer(a + b)),
        (Value::SmallInt(a), Value::SmallInt(b)) => Ok(Value::Integer(*a as i64 + *b as i64)),
        (Value::Integer(a), Value::SmallInt(b)) | (Value::SmallInt(b), Value::Integer(a)) => {
            Ok(Value::Integer(*a + *b as i64))
        }
        (Value::Real(a), Value::Real(b)) => Ok(Value::Real(a + b)),
        (Value::Integer(a), Value::Real(b)) | (Value::Real(b), Value::Integer(a)) => {
            Ok(Value::Real(*a as f64 + b))
        }
        (Value::SmallInt(a), Value::Real(b)) | (Value::Real(b), Value::SmallInt(a)) => {
            Ok(Value::Real(*a as f64 + b))
        }
        (Value::Decimal(a), Value::Decimal(b)) => Ok(Value::Decimal(*a + *b)),
        (Value::Decimal(a), Value::Integer(b)) | (Value::Integer(b), Value::Decimal(a)) => {
            Ok(Value::Decimal(*a + rust_decimal::Decimal::from(*b)))
        }
        (Value::Decimal(a), Value::SmallInt(b)) | (Value::SmallInt(b), Value::Decimal(a)) => {
            Ok(Value::Decimal(*a + rust_decimal::Decimal::from(*b)))
        }
        _ => Err(SqlError::TypeError(format!(
            "cannot add {} and {}",
            current, new
        ))),
    }
}

fn value_to_f64(v: &Value) -> Result<f64> {
    match v {
        Value::Integer(i) => Ok(*i as f64),
        Value::SmallInt(i) => Ok(*i as f64),
        Value::Real(f) => Ok(*f),
        Value::Decimal(d) => {
            use rust_decimal::prelude::ToPrimitive;
            Ok(d.to_f64().unwrap_or(f64::NAN))
        }
        Value::Null => Ok(0.0),
        _ => Err(SqlError::TypeError(format!(
            "cannot convert {} to f64 for AVG",
            v
        ))),
    }
}
