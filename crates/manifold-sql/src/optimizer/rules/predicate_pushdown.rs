use crate::binder::BinaryOp;
use crate::error::Result;
use crate::planner::plan::{LogicalPlan, ScalarExpr};

/// Push Filter nodes down through Project and Join nodes to filter rows earlier.
pub fn push_down(plan: LogicalPlan) -> Result<LogicalPlan> {
    push_down_plan(plan)
}

fn push_down_plan(plan: LogicalPlan) -> Result<LogicalPlan> {
    match plan {
        LogicalPlan::Filter { predicate, input } => {
            // First, recursively optimize the input.
            let input = push_down_plan(*input)?;
            // Now try to push this filter down.
            push_filter(predicate, input)
        }
        LogicalPlan::Project {
            expressions,
            aliases,
            schema,
            input,
        } => {
            let input = push_down_plan(*input)?;
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
            let left = push_down_plan(*left)?;
            let right = push_down_plan(*right)?;
            Ok(LogicalPlan::Join {
                join_type,
                left: Box::new(left),
                right: Box::new(right),
                condition,
                schema,
            })
        }
        LogicalPlan::Aggregate {
            group_by,
            aggregates,
            schema,
            input,
        } => {
            let input = push_down_plan(*input)?;
            Ok(LogicalPlan::Aggregate {
                group_by,
                aggregates,
                schema,
                input: Box::new(input),
            })
        }
        LogicalPlan::Sort { order_by, input } => {
            let input = push_down_plan(*input)?;
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
            let input = push_down_plan(*input)?;
            Ok(LogicalPlan::Limit {
                count,
                offset,
                input: Box::new(input),
            })
        }
        LogicalPlan::Distinct { input } => {
            let input = push_down_plan(*input)?;
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
            let left = push_down_plan(*left)?;
            let right = push_down_plan(*right)?;
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
            let source = push_down_plan(*source)?;
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
            let input = push_down_plan(*input)?;
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
            let input = push_down_plan(*input)?;
            Ok(LogicalPlan::Delete {
                table_id,
                table_name,
                input: Box::new(input),
            })
        }
        LogicalPlan::Explain { input } => {
            let input = push_down_plan(*input)?;
            Ok(LogicalPlan::Explain {
                input: Box::new(input),
            })
        }
        // Leaf nodes and DDL
        other => Ok(other),
    }
}

/// Try to push a filter predicate down through the given plan node.
fn push_filter(predicate: ScalarExpr, input: LogicalPlan) -> Result<LogicalPlan> {
    match input {
        // Filter above Filter: merge into AND
        LogicalPlan::Filter {
            predicate: inner_pred,
            input: inner_input,
        } => {
            let merged = ScalarExpr::BinaryOp {
                op: BinaryOp::And,
                left: Box::new(predicate),
                right: Box::new(inner_pred),
            };
            // Try to push the merged filter down further.
            push_filter(merged, *inner_input)
        }

        // Filter above Project: push below if predicate only references input columns
        LogicalPlan::Project {
            expressions,
            aliases,
            schema,
            input: proj_input,
        } => {
            if predicate_uses_only_passthrough_columns(&predicate, &expressions) {
                // Remap column refs through the project: if the predicate
                // references output col#i and proj_exprs[i] is ColumnRef { index: j },
                // replace col#i with col#j so the predicate works against the
                // project's input rather than its output.
                let remapped = remap_through_project(predicate, &expressions);
                let new_input = push_filter(remapped, *proj_input)?;
                Ok(LogicalPlan::Project {
                    expressions,
                    aliases,
                    schema,
                    input: Box::new(new_input),
                })
            } else {
                // Cannot push -- keep filter above project
                Ok(LogicalPlan::Filter {
                    predicate,
                    input: Box::new(LogicalPlan::Project {
                        expressions,
                        aliases,
                        schema,
                        input: proj_input,
                    }),
                })
            }
        }

        // Filter above Join: push to one side if predicate only references that side
        LogicalPlan::Join {
            join_type,
            left,
            right,
            condition,
            schema,
        } => {
            let left_cols = count_columns(&left);

            if let Some(side) = predicate_references_side(&predicate, left_cols) {
                match side {
                    JoinSide::Left => {
                        let new_left = push_filter(predicate, *left)?;
                        Ok(LogicalPlan::Join {
                            join_type,
                            left: Box::new(new_left),
                            right,
                            condition,
                            schema,
                        })
                    }
                    JoinSide::Right => {
                        // Remap column indices to be relative to the right side
                        let remapped = remap_columns(predicate, left_cols);
                        let new_right = push_filter(remapped, *right)?;
                        Ok(LogicalPlan::Join {
                            join_type,
                            left,
                            right: Box::new(new_right),
                            condition,
                            schema,
                        })
                    }
                }
            } else {
                // Predicate references both sides -- keep above join
                Ok(LogicalPlan::Filter {
                    predicate,
                    input: Box::new(LogicalPlan::Join {
                        join_type,
                        left,
                        right,
                        condition,
                        schema,
                    }),
                })
            }
        }

        // Cannot push through other nodes
        other => Ok(LogicalPlan::Filter {
            predicate,
            input: Box::new(other),
        }),
    }
}

/// Check if all column references in the predicate are simple pass-through
/// references in the project expressions (i.e., the project expression at
/// that index is just a ColumnRef itself).
fn predicate_uses_only_passthrough_columns(
    predicate: &ScalarExpr,
    proj_exprs: &[ScalarExpr],
) -> bool {
    let mut col_indices = Vec::new();
    collect_column_refs(predicate, &mut col_indices);
    col_indices.iter().all(|&idx| {
        idx < proj_exprs.len() && matches!(&proj_exprs[idx], ScalarExpr::ColumnRef { .. })
    })
}

enum JoinSide {
    Left,
    Right,
}

/// Determine if a predicate references only one side of a join.
/// `left_cols` is the number of columns from the left input.
fn predicate_references_side(predicate: &ScalarExpr, left_cols: usize) -> Option<JoinSide> {
    let mut col_indices = Vec::new();
    collect_column_refs(predicate, &mut col_indices);
    if col_indices.is_empty() {
        // Constant predicate -- keep it where it is
        return None;
    }
    let all_left = col_indices.iter().all(|&idx| idx < left_cols);
    let all_right = col_indices.iter().all(|&idx| idx >= left_cols);
    if all_left {
        Some(JoinSide::Left)
    } else if all_right {
        Some(JoinSide::Right)
    } else {
        None
    }
}

/// Collect all column reference indices from an expression.
fn collect_column_refs(expr: &ScalarExpr, out: &mut Vec<usize>) {
    match expr {
        ScalarExpr::ColumnRef { index } => out.push(*index),
        ScalarExpr::Literal(_) | ScalarExpr::Parameter(_) => {}
        ScalarExpr::BinaryOp { left, right, .. } => {
            collect_column_refs(left, out);
            collect_column_refs(right, out);
        }
        ScalarExpr::UnaryOp { operand, .. } => collect_column_refs(operand, out),
        ScalarExpr::IsNull { operand, .. } => collect_column_refs(operand, out),
        ScalarExpr::InList { expr, list, .. } => {
            collect_column_refs(expr, out);
            for item in list {
                collect_column_refs(item, out);
            }
        }
        ScalarExpr::Between {
            expr, low, high, ..
        } => {
            collect_column_refs(expr, out);
            collect_column_refs(low, out);
            collect_column_refs(high, out);
        }
        ScalarExpr::Like { expr, pattern, .. } => {
            collect_column_refs(expr, out);
            collect_column_refs(pattern, out);
        }
        ScalarExpr::Function { args, .. } => {
            for arg in args {
                collect_column_refs(arg, out);
            }
        }
        ScalarExpr::Cast { expr, .. } => collect_column_refs(expr, out),
        // Subqueries have their own scope; treat as opaque (no outer column refs to collect).
        ScalarExpr::InSubquery { .. }
        | ScalarExpr::Exists { .. }
        | ScalarExpr::ScalarSubquery { .. } => {}
    }
}

/// Remap column references in a predicate through a Project's expressions.
///
/// When pushing `Filter(col#i < 30)` below `Project([col#j, col#k])`,
/// we need to replace `col#i` with the input column that the Project maps
/// it to. For passthrough columns where `proj_exprs[i] = ColumnRef { index: j }`,
/// we replace `col#i` with `col#j`.
fn remap_through_project(expr: ScalarExpr, proj_exprs: &[ScalarExpr]) -> ScalarExpr {
    match expr {
        ScalarExpr::ColumnRef { index } => {
            if let Some(ScalarExpr::ColumnRef { index: mapped }) = proj_exprs.get(index) {
                ScalarExpr::ColumnRef { index: *mapped }
            } else {
                ScalarExpr::ColumnRef { index }
            }
        }
        ScalarExpr::BinaryOp { op, left, right } => ScalarExpr::BinaryOp {
            op,
            left: Box::new(remap_through_project(*left, proj_exprs)),
            right: Box::new(remap_through_project(*right, proj_exprs)),
        },
        ScalarExpr::UnaryOp { op, operand } => ScalarExpr::UnaryOp {
            op,
            operand: Box::new(remap_through_project(*operand, proj_exprs)),
        },
        ScalarExpr::IsNull { operand, negated } => ScalarExpr::IsNull {
            operand: Box::new(remap_through_project(*operand, proj_exprs)),
            negated,
        },
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: Box::new(remap_through_project(*expr, proj_exprs)),
            list: list
                .into_iter()
                .map(|e| remap_through_project(e, proj_exprs))
                .collect(),
            negated,
        },
        ScalarExpr::Between {
            expr,
            low,
            high,
            negated,
        } => ScalarExpr::Between {
            expr: Box::new(remap_through_project(*expr, proj_exprs)),
            low: Box::new(remap_through_project(*low, proj_exprs)),
            high: Box::new(remap_through_project(*high, proj_exprs)),
            negated,
        },
        ScalarExpr::Like {
            expr,
            pattern,
            negated,
        } => ScalarExpr::Like {
            expr: Box::new(remap_through_project(*expr, proj_exprs)),
            pattern: Box::new(remap_through_project(*pattern, proj_exprs)),
            negated,
        },
        ScalarExpr::Function { name, args } => ScalarExpr::Function {
            name,
            args: args
                .into_iter()
                .map(|e| remap_through_project(e, proj_exprs))
                .collect(),
        },
        ScalarExpr::Cast { expr, target_type } => ScalarExpr::Cast {
            expr: Box::new(remap_through_project(*expr, proj_exprs)),
            target_type,
        },
        other => other,
    }
}

/// Count the number of output columns from a plan node.
fn count_columns(plan: &LogicalPlan) -> usize {
    plan.schema().map_or(0, |s| s.len())
}

/// Remap column references by subtracting an offset (used when pushing a filter
/// from the combined join schema to the right side).
fn remap_columns(expr: ScalarExpr, offset: usize) -> ScalarExpr {
    match expr {
        ScalarExpr::ColumnRef { index } => ScalarExpr::ColumnRef {
            index: index - offset,
        },
        ScalarExpr::BinaryOp { op, left, right } => ScalarExpr::BinaryOp {
            op,
            left: Box::new(remap_columns(*left, offset)),
            right: Box::new(remap_columns(*right, offset)),
        },
        ScalarExpr::UnaryOp { op, operand } => ScalarExpr::UnaryOp {
            op,
            operand: Box::new(remap_columns(*operand, offset)),
        },
        ScalarExpr::IsNull { operand, negated } => ScalarExpr::IsNull {
            operand: Box::new(remap_columns(*operand, offset)),
            negated,
        },
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: Box::new(remap_columns(*expr, offset)),
            list: list.into_iter().map(|e| remap_columns(e, offset)).collect(),
            negated,
        },
        ScalarExpr::Between {
            expr,
            low,
            high,
            negated,
        } => ScalarExpr::Between {
            expr: Box::new(remap_columns(*expr, offset)),
            low: Box::new(remap_columns(*low, offset)),
            high: Box::new(remap_columns(*high, offset)),
            negated,
        },
        ScalarExpr::Like {
            expr,
            pattern,
            negated,
        } => ScalarExpr::Like {
            expr: Box::new(remap_columns(*expr, offset)),
            pattern: Box::new(remap_columns(*pattern, offset)),
            negated,
        },
        ScalarExpr::Function { name, args } => ScalarExpr::Function {
            name,
            args: args.into_iter().map(|e| remap_columns(e, offset)).collect(),
        },
        ScalarExpr::Cast { expr, target_type } => ScalarExpr::Cast {
            expr: Box::new(remap_columns(*expr, offset)),
            target_type,
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binder::JoinType;
    use crate::planner::plan::{PlanColumn, PlanSchema};
    use crate::types::{SqlType, Value};

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
    fn merge_stacked_filters() {
        let scan = LogicalPlan::Scan {
            table_id: 1,
            table_name: "t".to_string(),
            schema: make_schema(2),
        };
        let inner_filter = LogicalPlan::Filter {
            predicate: ScalarExpr::Literal(Value::Boolean(true)),
            input: Box::new(scan),
        };
        let outer_filter = LogicalPlan::Filter {
            predicate: ScalarExpr::Literal(Value::Boolean(false)),
            input: Box::new(inner_filter),
        };

        let result = push_down(outer_filter).unwrap();
        if let LogicalPlan::Filter { predicate, input } = &result {
            assert!(matches!(
                predicate,
                ScalarExpr::BinaryOp {
                    op: BinaryOp::And,
                    ..
                }
            ));
            assert!(matches!(input.as_ref(), LogicalPlan::Scan { .. }));
        } else {
            panic!("expected merged Filter, got {:?}", result);
        }
    }

    #[test]
    fn push_filter_below_join_to_left() {
        let left = LogicalPlan::Scan {
            table_id: 1,
            table_name: "left".to_string(),
            schema: make_schema(2),
        };
        let right = LogicalPlan::Scan {
            table_id: 2,
            table_name: "right".to_string(),
            schema: make_schema(2),
        };
        let join = LogicalPlan::Join {
            join_type: JoinType::Inner,
            left: Box::new(left),
            right: Box::new(right),
            condition: None,
            schema: make_schema(4),
        };
        let filter = LogicalPlan::Filter {
            predicate: ScalarExpr::BinaryOp {
                op: BinaryOp::Eq,
                left: Box::new(ScalarExpr::ColumnRef { index: 0 }),
                right: Box::new(ScalarExpr::Literal(Value::Integer(1))),
            },
            input: Box::new(join),
        };

        let result = push_down(filter).unwrap();
        if let LogicalPlan::Join { left, .. } = &result {
            assert!(matches!(left.as_ref(), LogicalPlan::Filter { .. }));
        } else {
            panic!("expected Join at top, got {:?}", result);
        }
    }

    #[test]
    fn push_filter_below_join_to_right() {
        let left = LogicalPlan::Scan {
            table_id: 1,
            table_name: "left".to_string(),
            schema: make_schema(2),
        };
        let right = LogicalPlan::Scan {
            table_id: 2,
            table_name: "right".to_string(),
            schema: make_schema(2),
        };
        let join = LogicalPlan::Join {
            join_type: JoinType::Inner,
            left: Box::new(left),
            right: Box::new(right),
            condition: None,
            schema: make_schema(4),
        };
        let filter = LogicalPlan::Filter {
            predicate: ScalarExpr::BinaryOp {
                op: BinaryOp::Eq,
                left: Box::new(ScalarExpr::ColumnRef { index: 3 }),
                right: Box::new(ScalarExpr::Literal(Value::Integer(1))),
            },
            input: Box::new(join),
        };

        let result = push_down(filter).unwrap();
        if let LogicalPlan::Join { right, .. } = &result {
            if let LogicalPlan::Filter { predicate, .. } = right.as_ref() {
                if let ScalarExpr::BinaryOp { left, .. } = predicate {
                    assert!(matches!(left.as_ref(), ScalarExpr::ColumnRef { index: 1 }));
                } else {
                    panic!("expected BinaryOp predicate");
                }
            } else {
                panic!("expected Filter on right side");
            }
        } else {
            panic!("expected Join at top");
        }
    }
}
