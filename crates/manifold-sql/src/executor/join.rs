use crate::binder::JoinType;
use crate::error::Result;
use crate::expr::eval::evaluate;
use crate::planner::plan::ScalarExpr;
use crate::types::Value;

/// Nested-loop join operator.
///
/// For each row from the left input, scans all rows from the right input and
/// emits combined rows (left columns + right columns) where the join condition
/// evaluates to true.
pub struct NestedLoopJoin {
    left: Box<dyn super::Executor>,
    right_rows: Vec<Vec<Value>>,
    join_type: JoinType,
    condition: Option<ScalarExpr>,
    params: Vec<Value>,
    /// Number of leading columns to skip from left rows (e.g. 1 for rowid).
    left_skip: usize,
    /// Number of leading columns to skip from right rows (e.g. 1 for rowid).
    right_skip: usize,
    /// Width of right side after skipping (for NULL padding).
    right_width: usize,
    /// Width of left side after skipping (for NULL padding).
    left_width: usize,

    // --- iteration state ---
    current_left: Option<Vec<Value>>,
    right_pos: usize,
    left_matched: bool,
    /// For RIGHT join: tracks which right rows have been matched.
    right_matched: Vec<bool>,
    /// For RIGHT join: whether we are in the "emit unmatched right rows" phase.
    right_drain_pos: Option<usize>,
    done: bool,
}

impl NestedLoopJoin {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        left: Box<dyn super::Executor>,
        mut right: Box<dyn super::Executor>,
        join_type: JoinType,
        condition: Option<ScalarExpr>,
        params: &[Value],
        left_skip: usize,
        right_skip: usize,
        left_width: usize,
        right_width: usize,
    ) -> Result<Self> {
        // Materialize the right side.
        let mut right_rows = Vec::new();
        while let Some(row) = right.next()? {
            right_rows.push(row);
        }

        let right_matched = vec![false; right_rows.len()];

        Ok(Self {
            left,
            right_rows,
            join_type,
            condition,
            params: params.to_vec(),
            left_skip,
            right_skip,
            right_width,
            left_width,
            current_left: None,
            right_pos: 0,
            left_matched: false,
            right_matched,
            right_drain_pos: None,
            done: false,
        })
    }

    /// Build a combined row from left and right values (after skipping rowid prefixes).
    fn combine(&self, left: &[Value], right: &[Value]) -> Vec<Value> {
        let mut combined = Vec::with_capacity(self.left_width + self.right_width);
        combined.extend_from_slice(&left[self.left_skip..]);
        combined.extend_from_slice(&right[self.right_skip..]);
        combined
    }

    /// Build a row with left values + NULLs for right side.
    fn left_with_nulls(&self, left: &[Value]) -> Vec<Value> {
        let mut combined = Vec::with_capacity(self.left_width + self.right_width);
        combined.extend_from_slice(&left[self.left_skip..]);
        combined.resize(self.left_width + self.right_width, Value::Null);
        combined
    }

    /// Build a row with NULLs for left side + right values.
    fn nulls_with_right(&self, right: &[Value]) -> Vec<Value> {
        let mut combined = vec![Value::Null; self.left_width];
        combined.extend_from_slice(&right[self.right_skip..]);
        combined
    }

    fn check_condition(&self, combined: &[Value]) -> Result<bool> {
        match &self.condition {
            None => Ok(true),
            Some(cond) => {
                let result = evaluate(cond, combined, &self.params)?;
                Ok(result == Value::Boolean(true))
            }
        }
    }
}

impl super::Executor for NestedLoopJoin {
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        if self.done {
            return Ok(None);
        }

        // RIGHT join: drain unmatched right rows after all left rows are consumed.
        if let Some(pos) = self.right_drain_pos {
            for i in pos..self.right_rows.len() {
                if !self.right_matched[i] {
                    self.right_drain_pos = Some(i + 1);
                    return Ok(Some(self.nulls_with_right(&self.right_rows[i])));
                }
            }
            self.done = true;
            return Ok(None);
        }

        loop {
            // If we don't have a current left row, fetch one.
            if self.current_left.is_none() {
                match self.left.next()? {
                    Some(row) => {
                        self.current_left = Some(row);
                        self.right_pos = 0;
                        self.left_matched = false;
                    }
                    None => {
                        // Left side exhausted.
                        if self.join_type == JoinType::Right {
                            self.right_drain_pos = Some(0);
                            return self.next();
                        }
                        self.done = true;
                        return Ok(None);
                    }
                }
            }

            let left_row = self.current_left.as_ref().unwrap();

            // Scan right rows from current position.
            while self.right_pos < self.right_rows.len() {
                let right_row = &self.right_rows[self.right_pos];
                self.right_pos += 1;

                match self.join_type {
                    JoinType::Cross => {
                        return Ok(Some(self.combine(left_row, right_row)));
                    }
                    JoinType::Inner => {
                        let combined = self.combine(left_row, right_row);
                        if self.check_condition(&combined)? {
                            return Ok(Some(combined));
                        }
                    }
                    JoinType::Left => {
                        let combined = self.combine(left_row, right_row);
                        if self.check_condition(&combined)? {
                            self.left_matched = true;
                            return Ok(Some(combined));
                        }
                    }
                    JoinType::Right => {
                        let combined = self.combine(left_row, right_row);
                        if self.check_condition(&combined)? {
                            self.left_matched = true;
                            self.right_matched[self.right_pos - 1] = true;
                            return Ok(Some(combined));
                        }
                    }
                }
            }

            // Finished scanning all right rows for the current left row.
            let emit_unmatched = match self.join_type {
                JoinType::Left => !self.left_matched,
                _ => false,
            };

            if emit_unmatched {
                let row = self.left_with_nulls(left_row);
                self.current_left = None;
                return Ok(Some(row));
            }

            // Move to next left row.
            self.current_left = None;
        }
    }
}
