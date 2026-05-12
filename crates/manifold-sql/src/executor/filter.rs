use crate::error::Result;
use crate::expr::eval::evaluate;
use crate::planner::plan::ScalarExpr;
use crate::types::Value;

/// Filter operator: passes through only rows where the predicate evaluates to true.
pub struct Filter {
    input: Box<dyn super::Executor>,
    predicate: ScalarExpr,
    params: Vec<Value>,
}

impl Filter {
    pub fn new(input: Box<dyn super::Executor>, predicate: ScalarExpr, params: &[Value]) -> Self {
        Self {
            input,
            predicate,
            params: params.to_vec(),
        }
    }
}

impl super::Executor for Filter {
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        loop {
            match self.input.next()? {
                Some(row) => {
                    let result = evaluate(&self.predicate, &row, &self.params)?;
                    if result == Value::Boolean(true) {
                        return Ok(Some(row));
                    }
                    // Otherwise skip this row and try next
                }
                None => return Ok(None),
            }
        }
    }
}
