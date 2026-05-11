use crate::error::Result;
use crate::expr::eval::evaluate;
use crate::planner::plan::ScalarExpr;
use crate::types::Value;

/// Project operator: evaluates a list of expressions against each input row.
pub struct Project {
    input: Box<dyn super::Executor>,
    expressions: Vec<ScalarExpr>,
    params: Vec<Value>,
}

impl Project {
    pub fn new(
        input: Box<dyn super::Executor>,
        expressions: Vec<ScalarExpr>,
        params: &[Value],
    ) -> Self {
        Self {
            input,
            expressions,
            params: params.to_vec(),
        }
    }
}

impl super::Executor for Project {
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        match self.input.next()? {
            Some(row) => {
                let mut output = Vec::with_capacity(self.expressions.len());
                for expr in &self.expressions {
                    output.push(evaluate(expr, &row, &self.params)?);
                }
                Ok(Some(output))
            }
            None => Ok(None),
        }
    }
}
