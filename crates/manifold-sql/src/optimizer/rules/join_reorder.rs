use crate::binder::JoinType;
use crate::catalog::Catalog;
use crate::error::Result;
use crate::optimizer::statistics;
use crate::planner::plan::{LogicalPlan, ScalarExpr};
use crate::types::Value;

/// Cost-based join reordering: for inner joins, put the smaller table on the
/// left (outer) side to reduce nested-loop iterations.
pub fn reorder(plan: LogicalPlan, catalog: &Catalog) -> Result<LogicalPlan> {
    reorder_plan(plan, catalog)
}

fn reorder_plan(plan: LogicalPlan, catalog: &Catalog) -> Result<LogicalPlan> {
    match plan {
        LogicalPlan::Join {
            join_type,
            left,
            right,
            condition,
            schema,
        } => {
            // Recursively reorder children first
            let left = reorder_plan(*left, catalog)?;
            let right = reorder_plan(*right, catalog)?;

            // Only reorder inner and cross joins -- outer joins are order-sensitive
            if matches!(join_type, JoinType::Inner | JoinType::Cross) {
                let left_cost = estimate_rows(&left, catalog);
                let right_cost = estimate_rows(&right, catalog);

                if right_cost < left_cost {
                    // Swap: put smaller table on left
                    return Ok(LogicalPlan::Join {
                        join_type,
                        left: Box::new(right),
                        right: Box::new(left),
                        condition,
                        schema,
                    });
                }
            }

            Ok(LogicalPlan::Join {
                join_type,
                left: Box::new(left),
                right: Box::new(right),
                condition,
                schema,
            })
        }
        LogicalPlan::Filter { predicate, input } => {
            let input = reorder_plan(*input, catalog)?;
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
            let input = reorder_plan(*input, catalog)?;
            Ok(LogicalPlan::Project {
                expressions,
                aliases,
                schema,
                input: Box::new(input),
            })
        }
        LogicalPlan::Aggregate {
            group_by,
            aggregates,
            schema,
            input,
        } => {
            let input = reorder_plan(*input, catalog)?;
            Ok(LogicalPlan::Aggregate {
                group_by,
                aggregates,
                schema,
                input: Box::new(input),
            })
        }
        LogicalPlan::Sort { order_by, input } => {
            let input = reorder_plan(*input, catalog)?;
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
            let input = reorder_plan(*input, catalog)?;
            Ok(LogicalPlan::Limit {
                count,
                offset,
                input: Box::new(input),
            })
        }
        LogicalPlan::Distinct { input } => {
            let input = reorder_plan(*input, catalog)?;
            Ok(LogicalPlan::Distinct {
                input: Box::new(input),
            })
        }
        LogicalPlan::Union {
            left,
            right,
            all,
            schema,
        } => {
            let left = reorder_plan(*left, catalog)?;
            let right = reorder_plan(*right, catalog)?;
            Ok(LogicalPlan::Union {
                left: Box::new(left),
                right: Box::new(right),
                all,
                schema,
            })
        }
        LogicalPlan::Insert {
            table_id,
            table_name,
            columns,
            source,
        } => {
            let source = reorder_plan(*source, catalog)?;
            Ok(LogicalPlan::Insert {
                table_id,
                table_name,
                columns,
                source: Box::new(source),
            })
        }
        LogicalPlan::Update {
            table_id,
            table_name,
            assignments,
            input,
        } => {
            let input = reorder_plan(*input, catalog)?;
            Ok(LogicalPlan::Update {
                table_id,
                table_name,
                assignments,
                input: Box::new(input),
            })
        }
        LogicalPlan::Delete {
            table_id,
            table_name,
            input,
        } => {
            let input = reorder_plan(*input, catalog)?;
            Ok(LogicalPlan::Delete {
                table_id,
                table_name,
                input: Box::new(input),
            })
        }
        LogicalPlan::Explain { input } => {
            let input = reorder_plan(*input, catalog)?;
            Ok(LogicalPlan::Explain {
                input: Box::new(input),
            })
        }
        other => Ok(other),
    }
}

/// Estimate the number of rows produced by a plan node.
fn estimate_rows(plan: &LogicalPlan, catalog: &Catalog) -> u64 {
    match plan {
        LogicalPlan::Scan { table_name, .. } => {
            statistics::estimate_row_count(table_name, catalog)
        }
        LogicalPlan::IndexScan { table_name, .. } => {
            // Index scan is typically more selective; estimate 10% of table
            statistics::estimate_row_count(table_name, catalog) / 10
        }
        LogicalPlan::Filter { input, .. } => {
            // Assume a filter passes ~33% of rows
            estimate_rows(input, catalog) / 3
        }
        LogicalPlan::Join { left, right, .. } => {
            let l = estimate_rows(left, catalog);
            let r = estimate_rows(right, catalog);
            l.saturating_mul(r)
        }
        LogicalPlan::Aggregate { input, .. } => {
            let input_rows = estimate_rows(input, catalog);
            (input_rows / 10).max(1)
        }
        LogicalPlan::Limit { count, input, .. } => {
            if let Some(ScalarExpr::Literal(Value::Integer(n))) = count.as_ref() {
                (*n as u64).min(estimate_rows(input, catalog))
            } else {
                estimate_rows(input, catalog)
            }
        }
        LogicalPlan::Project { input, .. }
        | LogicalPlan::Sort { input, .. }
        | LogicalPlan::Distinct { input }
        | LogicalPlan::Explain { input } => estimate_rows(input, catalog),
        LogicalPlan::Values { rows, .. } => rows.len() as u64,
        _ => 1000, // default
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::plan::{PlanColumn, PlanSchema};
    use crate::types::SqlType;

    fn make_schema(n: usize) -> PlanSchema {
        PlanSchema {
            columns: (0..n)
                .map(|i| PlanColumn {
                    name: format!("col{}", i),
                    sql_type: SqlType::Integer,
                    nullable: false,
                })
                .collect(),
        }
    }

    #[test]
    fn does_not_reorder_when_left_is_smaller() {
        let catalog = Catalog::new();
        // Both are scans with unknown stats (1000 rows each), so no swap
        let left = LogicalPlan::Scan {
            table_id: 1,
            table_name: "a".to_string(),
            schema: make_schema(2),
        };
        let right = LogicalPlan::Scan {
            table_id: 2,
            table_name: "b".to_string(),
            schema: make_schema(2),
        };
        let join = LogicalPlan::Join {
            join_type: JoinType::Inner,
            left: Box::new(left),
            right: Box::new(right),
            condition: None,
            schema: make_schema(4),
        };

        let result = reorder(join, &catalog).unwrap();
        if let LogicalPlan::Join { left, .. } = &result {
            if let LogicalPlan::Scan { table_name, .. } = left.as_ref() {
                assert_eq!(table_name, "a");
            }
        }
    }

    #[test]
    fn reorders_when_right_is_smaller() {
        let catalog = Catalog::new();
        let left = LogicalPlan::Values {
            rows: vec![vec![]; 100],
            schema: make_schema(2),
        };
        let right = LogicalPlan::Values {
            rows: vec![vec![]; 2],
            schema: make_schema(2),
        };
        let join = LogicalPlan::Join {
            join_type: JoinType::Inner,
            left: Box::new(left),
            right: Box::new(right),
            condition: None,
            schema: make_schema(4),
        };

        let result = reorder(join, &catalog).unwrap();
        if let LogicalPlan::Join { left, .. } = &result {
            if let LogicalPlan::Values { rows, .. } = left.as_ref() {
                assert_eq!(rows.len(), 2);
            } else {
                panic!("expected Values on left after reorder");
            }
        } else {
            panic!("expected Join");
        }
    }

    #[test]
    fn does_not_reorder_left_join() {
        let catalog = Catalog::new();
        let left = LogicalPlan::Values {
            rows: vec![vec![]; 100],
            schema: make_schema(2),
        };
        let right = LogicalPlan::Values {
            rows: vec![vec![]; 2],
            schema: make_schema(2),
        };
        let join = LogicalPlan::Join {
            join_type: JoinType::Left,
            left: Box::new(left),
            right: Box::new(right),
            condition: None,
            schema: make_schema(4),
        };

        let result = reorder(join, &catalog).unwrap();
        if let LogicalPlan::Join { left, .. } = &result {
            if let LogicalPlan::Values { rows, .. } = left.as_ref() {
                assert_eq!(rows.len(), 100); // unchanged
            } else {
                panic!("expected Values on left");
            }
        }
    }
}
