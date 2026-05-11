use crate::error::Result;
use crate::expr::eval::evaluate;
use crate::planner::plan::ScalarExpr;
use crate::types::Value;

/// Values operator: emits literal rows from a VALUES clause.
pub struct Values {
    rows: Vec<Vec<ScalarExpr>>,
    params: Vec<Value>,
    position: usize,
}

impl Values {
    pub fn new(rows: Vec<Vec<ScalarExpr>>, params: &[Value]) -> Self {
        Self {
            rows,
            params: params.to_vec(),
            position: 0,
        }
    }
}

impl super::Executor for Values {
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        if self.position < self.rows.len() {
            let exprs = &self.rows[self.position];
            let mut row = Vec::with_capacity(exprs.len());
            for expr in exprs {
                row.push(evaluate(expr, &[], &self.params)?);
            }
            self.position += 1;
            Ok(Some(row))
        } else {
            Ok(None)
        }
    }
}
