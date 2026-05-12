use crate::error::Result;
use crate::types::Value;

use super::Executor;

// ---------------------------------------------------------------------------
// UnionAll executor
// ---------------------------------------------------------------------------

/// Concatenates rows from two child executors (UNION ALL — duplicates kept).
pub struct UnionAll {
    left: Box<dyn Executor>,
    right: Box<dyn Executor>,
    left_done: bool,
}

impl UnionAll {
    pub fn new(left: Box<dyn Executor>, right: Box<dyn Executor>) -> Self {
        Self {
            left,
            right,
            left_done: false,
        }
    }
}

impl Executor for UnionAll {
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        if !self.left_done {
            match self.left.next()? {
                Some(row) => return Ok(Some(row)),
                None => {
                    self.left_done = true;
                }
            }
        }
        self.right.next()
    }
}

// ---------------------------------------------------------------------------
// Union (DISTINCT) executor
// ---------------------------------------------------------------------------

/// Deduplicates rows from two child executors (UNION — no duplicates).
/// Collects all rows upfront, deduplicates, then streams them out.
pub struct UnionDistinct {
    rows: Vec<Vec<Value>>,
    pos: usize,
}

impl UnionDistinct {
    pub fn new(mut left: Box<dyn Executor>, mut right: Box<dyn Executor>) -> Result<Self> {
        let mut all: Vec<Vec<Value>> = Vec::new();

        while let Some(row) = left.next()? {
            all.push(row);
        }
        while let Some(row) = right.next()? {
            all.push(row);
        }

        // Deduplicate: use a simple O(n^2) scan since rows may not be Ord.
        // For larger sets a hash-based approach would be better, but rows
        // contain floats so we can't derive Hash on Value easily.
        let mut deduped: Vec<Vec<Value>> = Vec::with_capacity(all.len());
        for row in all {
            if !deduped.iter().any(|r| r == &row) {
                deduped.push(row);
            }
        }

        Ok(Self {
            rows: deduped,
            pos: 0,
        })
    }
}

impl Executor for UnionDistinct {
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        if self.pos < self.rows.len() {
            let row = self.rows[self.pos].clone();
            self.pos += 1;
            Ok(Some(row))
        } else {
            Ok(None)
        }
    }
}
