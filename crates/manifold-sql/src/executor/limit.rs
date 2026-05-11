use crate::error::Result;
use crate::types::Value;

/// Limit/Offset operator: skips `offset` rows then emits up to `count` rows.
pub struct Limit {
    input: Box<dyn super::Executor>,
    count: Option<usize>,
    offset: usize,
    skipped: usize,
    emitted: usize,
}

impl Limit {
    pub fn new(
        input: Box<dyn super::Executor>,
        count: Option<usize>,
        offset: usize,
    ) -> Self {
        Self {
            input,
            count,
            offset,
            skipped: 0,
            emitted: 0,
        }
    }
}

impl super::Executor for Limit {
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        // Skip offset rows first.
        while self.skipped < self.offset {
            match self.input.next()? {
                Some(_) => {
                    self.skipped += 1;
                }
                None => return Ok(None),
            }
        }

        // Check if we've emitted enough.
        if let Some(count) = self.count
            && self.emitted >= count
        {
            return Ok(None);
        }

        match self.input.next()? {
            Some(row) => {
                self.emitted += 1;
                Ok(Some(row))
            }
            None => Ok(None),
        }
    }
}
