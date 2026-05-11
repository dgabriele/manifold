//! Subquery materialization for the executor.
//!
//! Uncorrelated subqueries are resolved before the main execution by walking
//! the plan tree, executing each subquery, and replacing the `ScalarExpr`
//! variant with its materialized result (a literal set, boolean, or scalar).

use crate::error::Result;
use crate::planner::plan::{LogicalPlan, ScalarExpr};
use crate::types::Value;

use super::Executor;

/// Resolve all subquery expressions in a logical plan by materializing them.
///
/// For read transactions:
pub fn resolve_subqueries_read(
    plan: LogicalPlan,
    txn: &manifold::ReadTransaction,
    catalog: &crate::catalog::Catalog,
    params: &[Value],
) -> Result<LogicalPlan> {
    resolve_plan(plan, &|subplan| {
        let mut exec = super::build_read_query_executor(txn, catalog, subplan, params)?;
        collect_subquery_rows(&mut *exec)
    })
}

/// Resolve all subquery expressions in a logical plan by materializing them.
///
/// For write transactions:
pub fn resolve_subqueries_write(
    plan: LogicalPlan,
    txn: &manifold::WriteTransaction,
    catalog: &crate::catalog::Catalog,
    params: &[Value],
) -> Result<LogicalPlan> {
    resolve_plan(plan, &|subplan| {
        let mut exec = super::build_write_query_executor(txn, catalog, subplan, params)?;
        collect_subquery_rows(&mut *exec)
    })
}

fn collect_subquery_rows(exec: &mut dyn Executor) -> Result<Vec<Vec<Value>>> {
    let mut rows = Vec::new();
    while let Some(row) = exec.next()? {
        rows.push(row);
    }
    Ok(rows)
}

/// Walk the plan and resolve subquery expressions in all ScalarExpr positions.
fn resolve_plan<F>(plan: LogicalPlan, execute_subquery: &F) -> Result<LogicalPlan>
where
    F: Fn(&LogicalPlan) -> Result<Vec<Vec<Value>>>,
{
    match plan {
        LogicalPlan::Filter { predicate, input } => {
            let input = resolve_plan(*input, execute_subquery)?;
            let predicate = resolve_expr(predicate, execute_subquery)?;
            Ok(LogicalPlan::Filter {
                predicate,
                input: Box::new(input),
            })
        }
        LogicalPlan::Project {
            expressions,
            aliases,
            schema,
            input,
        } => {
            let input = resolve_plan(*input, execute_subquery)?;
            let expressions = expressions
                .into_iter()
                .map(|e| resolve_expr(e, execute_subquery))
                .collect::<Result<Vec<_>>>()?;
            Ok(LogicalPlan::Project {
                expressions,
                aliases,
                schema,
                input: Box::new(input),
            })
        }
        LogicalPlan::Join {
            join_type,
            left,
            right,
            condition,
            schema,
        } => {
            let left = resolve_plan(*left, execute_subquery)?;
            let right = resolve_plan(*right, execute_subquery)?;
            let condition = condition
                .map(|c| resolve_expr(c, execute_subquery))
                .transpose()?;
            Ok(LogicalPlan::Join {
                join_type,
                left: Box::new(left),
                right: Box::new(right),
                condition,
                schema,
            })
        }
        LogicalPlan::Sort { order_by, input } => {
            let input = resolve_plan(*input, execute_subquery)?;
            Ok(LogicalPlan::Sort {
                order_by,
                input: Box::new(input),
            })
        }
        LogicalPlan::Limit {
            count,
            offset,
            input,
        } => {
            let input = resolve_plan(*input, execute_subquery)?;
            Ok(LogicalPlan::Limit {
                count,
                offset,
                input: Box::new(input),
            })
        }
        LogicalPlan::Distinct { input } => {
            let input = resolve_plan(*input, execute_subquery)?;
            Ok(LogicalPlan::Distinct {
                input: Box::new(input),
            })
        }
        LogicalPlan::Aggregate {
            group_by,
            aggregates,
            schema,
            input,
        } => {
            let input = resolve_plan(*input, execute_subquery)?;
            Ok(LogicalPlan::Aggregate {
                group_by,
                aggregates,
                schema,
                input: Box::new(input),
            })
        }
        LogicalPlan::Union {
            left,
            right,
            all,
            schema,
        } => {
            let left = resolve_plan(*left, execute_subquery)?;
            let right = resolve_plan(*right, execute_subquery)?;
            Ok(LogicalPlan::Union {
                left: Box::new(left),
                right: Box::new(right),
                all,
                schema,
            })
        }
        // All other plan nodes pass through unchanged
        other => Ok(other),
    }
}

/// Resolve subquery expressions within a single ScalarExpr.
fn resolve_expr<F>(expr: ScalarExpr, execute_subquery: &F) -> Result<ScalarExpr>
where
    F: Fn(&LogicalPlan) -> Result<Vec<Vec<Value>>>,
{
    match expr {
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => {
            let expr = resolve_expr(*expr, execute_subquery)?;
            // Execute the subquery and collect the first column into an InList.
            let rows = execute_subquery(&subquery)?;
            let list: Vec<ScalarExpr> = rows
                .into_iter()
                .filter_map(|row| row.into_iter().next())
                .map(ScalarExpr::Literal)
                .collect();
            Ok(ScalarExpr::InList {
                expr: Box::new(expr),
                list,
                negated,
            })
        }
        ScalarExpr::Exists { subquery, negated } => {
            let rows = execute_subquery(&subquery)?;
            let exists = !rows.is_empty();
            let result = if negated { !exists } else { exists };
            Ok(ScalarExpr::Literal(Value::Boolean(result)))
        }
        ScalarExpr::ScalarSubquery { subquery } => {
            let rows = execute_subquery(&subquery)?;
            let value = if let Some(first_row) = rows.into_iter().next() {
                first_row.into_iter().next().unwrap_or(Value::Null)
            } else {
                Value::Null
            };
            Ok(ScalarExpr::Literal(value))
        }
        // Recurse into compound expressions
        ScalarExpr::BinaryOp { op, left, right } => Ok(ScalarExpr::BinaryOp {
            op,
            left: Box::new(resolve_expr(*left, execute_subquery)?),
            right: Box::new(resolve_expr(*right, execute_subquery)?),
        }),
        ScalarExpr::UnaryOp { op, operand } => Ok(ScalarExpr::UnaryOp {
            op,
            operand: Box::new(resolve_expr(*operand, execute_subquery)?),
        }),
        ScalarExpr::IsNull { operand, negated } => Ok(ScalarExpr::IsNull {
            operand: Box::new(resolve_expr(*operand, execute_subquery)?),
            negated,
        }),
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => {
            let expr = resolve_expr(*expr, execute_subquery)?;
            let list = list
                .into_iter()
                .map(|e| resolve_expr(e, execute_subquery))
                .collect::<Result<Vec<_>>>()?;
            Ok(ScalarExpr::InList {
                expr: Box::new(expr),
                list,
                negated,
            })
        }
        ScalarExpr::Between {
            expr,
            low,
            high,
            negated,
        } => Ok(ScalarExpr::Between {
            expr: Box::new(resolve_expr(*expr, execute_subquery)?),
            low: Box::new(resolve_expr(*low, execute_subquery)?),
            high: Box::new(resolve_expr(*high, execute_subquery)?),
            negated,
        }),
        ScalarExpr::Like {
            expr,
            pattern,
            negated,
        } => Ok(ScalarExpr::Like {
            expr: Box::new(resolve_expr(*expr, execute_subquery)?),
            pattern: Box::new(resolve_expr(*pattern, execute_subquery)?),
            negated,
        }),
        ScalarExpr::Function { name, args } => {
            let args = args
                .into_iter()
                .map(|a| resolve_expr(a, execute_subquery))
                .collect::<Result<Vec<_>>>()?;
            Ok(ScalarExpr::Function { name, args })
        }
        ScalarExpr::Cast { expr, target_type } => Ok(ScalarExpr::Cast {
            expr: Box::new(resolve_expr(*expr, execute_subquery)?),
            target_type,
        }),
        // Leaf nodes — nothing to resolve
        other => Ok(other),
    }
}
