pub mod expr;
pub mod plan;

use plan::LogicalPlan;

use crate::binder::BoundStatement;
use crate::catalog::Catalog;
use crate::error::Result;

pub fn plan(_catalog: &Catalog, _stmt: &BoundStatement) -> Result<LogicalPlan> {
    todo!()
}
