pub mod rules;
pub mod statistics;

use crate::catalog::Catalog;
use crate::error::Result;
use crate::planner::plan::LogicalPlan;

pub fn optimize(plan: LogicalPlan, _catalog: &Catalog) -> Result<LogicalPlan> {
    Ok(plan)
}
