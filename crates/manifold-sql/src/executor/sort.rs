use crate::error::Result;
use crate::expr::eval::{compare_values, evaluate};
use crate::planner::plan::OrderByExpr;
use crate::types::Value;

/// Sort operator: materializes all rows, sorts them, then emits in order.
pub struct Sort {
    rows: Vec<Vec<Value>>,
    position: usize,
}

impl Sort {
    pub fn new(
        mut input: Box<dyn super::Executor>,
        order_by: &[OrderByExpr],
        params: &[Value],
    ) -> Result<Self> {
        // Materialize all input rows.
        let mut rows = Vec::new();
        while let Some(row) = input.next()? {
            rows.push(row);
        }

        // Pre-evaluate sort keys for each row.
        let mut keyed_rows: Vec<(Vec<Value>, Vec<Value>)> = Vec::with_capacity(rows.len());
        for row in rows {
            let mut keys = Vec::with_capacity(order_by.len());
            for ob in order_by {
                keys.push(evaluate(&ob.expr, &row, params)?);
            }
            keyed_rows.push((keys, row));
        }

        // Sort using the pre-evaluated keys.
        keyed_rows.sort_by(|(keys_a, _), (keys_b, _)| {
            for (i, ob) in order_by.iter().enumerate() {
                let a = &keys_a[i];
                let b = &keys_b[i];

                // Handle NULLS FIRST/LAST
                let null_first = ob.nulls_first.unwrap_or(!ob.asc);
                match (a.is_null(), b.is_null()) {
                    (true, true) => continue,
                    (true, false) => {
                        return if null_first {
                            std::cmp::Ordering::Less
                        } else {
                            std::cmp::Ordering::Greater
                        };
                    }
                    (false, true) => {
                        return if null_first {
                            std::cmp::Ordering::Greater
                        } else {
                            std::cmp::Ordering::Less
                        };
                    }
                    (false, false) => {}
                }

                let cmp = compare_values(a, b);
                let cmp = if ob.asc { cmp } else { cmp.reverse() };
                if cmp != std::cmp::Ordering::Equal {
                    return cmp;
                }
            }
            std::cmp::Ordering::Equal
        });

        let sorted_rows: Vec<Vec<Value>> = keyed_rows.into_iter().map(|(_, row)| row).collect();

        Ok(Self {
            rows: sorted_rows,
            position: 0,
        })
    }
}

impl super::Executor for Sort {
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        if self.position < self.rows.len() {
            let row = self.rows[self.position].clone();
            self.position += 1;
            Ok(Some(row))
        } else {
            Ok(None)
        }
    }
}
