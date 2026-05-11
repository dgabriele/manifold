pub mod rules;
pub mod statistics;

use crate::catalog::Catalog;
use crate::error::Result;
use crate::planner::plan::LogicalPlan;

pub fn optimize(plan: LogicalPlan, catalog: &Catalog) -> Result<LogicalPlan> {
    let plan = rules::constant_folding::fold(plan)?;
    let plan = rules::predicate_pushdown::push_down(plan)?;
    let plan = rules::index_selection::select_indexes(plan, catalog)?;
    let plan = rules::join_reorder::reorder(plan, catalog)?;
    Ok(plan)
}
