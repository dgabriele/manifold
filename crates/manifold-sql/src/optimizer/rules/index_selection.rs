use crate::binder::BinaryOp;
use crate::catalog::Catalog;
use crate::error::Result;
use crate::planner::plan::{LogicalPlan, ScalarExpr};


/// For `Filter(col = literal, Scan)` patterns, replace with `IndexScan` when
/// an index covers the filtered column.
pub fn select_indexes(plan: LogicalPlan, catalog: &Catalog) -> Result<LogicalPlan> {
    select_plan(plan, catalog)
}

fn select_plan(plan: LogicalPlan, catalog: &Catalog) -> Result<LogicalPlan> {
    match plan {
        LogicalPlan::Filter { predicate, input } => {
            let input = select_plan(*input, catalog)?;
            // Check for Filter(col = literal, Scan) pattern.
            // Replace the Scan with IndexScan but keep the Filter so the
            // predicate is still evaluated (the executor falls back to a
            // full scan until native index-scan support is added).
            if let LogicalPlan::Scan {
                table_id,
                ref table_name,
                ref schema,
            } = input
                && let Some(index_name) =
                    find_index_for_predicate(&predicate, table_id, table_name, catalog)
            {
                let index_scan = LogicalPlan::IndexScan {
                    table_id,
                    table_name: table_name.clone(),
                    index_name,
                    schema: schema.clone(),
                };
                return Ok(LogicalPlan::Filter {
                    predicate,
                    input: Box::new(index_scan),
                });
            }
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
            let input = select_plan(*input, catalog)?;
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
            let left = select_plan(*left, catalog)?;
            let right = select_plan(*right, catalog)?;
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
            let input = select_plan(*input, catalog)?;
            Ok(LogicalPlan::Aggregate {
                group_by,
                aggregates,
                schema,
                input: Box::new(input),
            })
        }
        LogicalPlan::Sort { order_by, input } => {
            let input = select_plan(*input, catalog)?;
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
            let input = select_plan(*input, catalog)?;
            Ok(LogicalPlan::Limit {
                count,
                offset,
                input: Box::new(input),
            })
        }
        LogicalPlan::Distinct { input } => {
            let input = select_plan(*input, catalog)?;
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
            let left = select_plan(*left, catalog)?;
            let right = select_plan(*right, catalog)?;
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
            let source = select_plan(*source, catalog)?;
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
            let input = select_plan(*input, catalog)?;
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
            let input = select_plan(*input, catalog)?;
            Ok(LogicalPlan::Delete {
                table_id,
                table_name,
                input: Box::new(input),
            })
        }
        LogicalPlan::Explain { input } => {
            let input = select_plan(*input, catalog)?;
            Ok(LogicalPlan::Explain {
                input: Box::new(input),
            })
        }
        other => Ok(other),
    }
}

/// Check if the predicate is an equality comparison `col = literal` and if there
/// is an index on that column. Returns the index name if found.
fn find_index_for_predicate(
    predicate: &ScalarExpr,
    table_id: u32,
    _table_name: &str,
    catalog: &Catalog,
) -> Option<String> {
    // Match: col = literal (either order)
    if let ScalarExpr::BinaryOp {
        op: BinaryOp::Eq,
        left,
        right,
    } = predicate
    {
        let col_index = match (left.as_ref(), right.as_ref()) {
            (ScalarExpr::ColumnRef { index }, ScalarExpr::Literal(_)) => Some(*index),
            (ScalarExpr::Literal(_), ScalarExpr::ColumnRef { index }) => Some(*index),
            _ => None,
        };

        if let Some(col_idx) = col_index {
            let indexes = catalog.indexes_for_table(table_id);
            for idx_def in indexes {
                // Match if the column is the first (or only) column of the index
                if !idx_def.columns.is_empty() && idx_def.columns[0] == col_idx {
                    return Some(idx_def.name.clone());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::schema::{ColumnDef, IndexDef, TableSchema};
    use crate::planner::plan::{PlanColumn, PlanSchema};
    use crate::types::{SqlType, Value};

    fn test_catalog() -> Catalog {
        let mut catalog = Catalog::new();
        let table = TableSchema {
            id: 1,
            name: "users".to_string(),
            columns: vec![
                ColumnDef {
                    name: "id".to_string(),
                    sql_type: SqlType::Integer,
                    nullable: false,
                    default: None,
                    is_primary_key: true,
                },
                ColumnDef {
                    name: "name".to_string(),
                    sql_type: SqlType::Text,
                    nullable: false,
                    default: None,
                    is_primary_key: false,
                },
            ],
            constraints: vec![],
            next_rowid: 1,
        };
        catalog.add_table(table);
        catalog.add_index(IndexDef {
            id: 1,
            name: "idx_users_id".to_string(),
            table_id: 1,
            columns: vec![0],
            unique: true,
        });
        catalog
    }

    #[test]
    fn replaces_filter_scan_with_index_scan() {
        let catalog = test_catalog();
        let scan = LogicalPlan::Scan {
            table_id: 1,
            table_name: "users".to_string(),
            schema: PlanSchema {
                columns: vec![
                    PlanColumn {
                        name: "id".to_string(),
                        sql_type: SqlType::Integer,
                        nullable: false,
                    },
                    PlanColumn {
                        name: "name".to_string(),
                        sql_type: SqlType::Text,
                        nullable: false,
                    },
                ],
            },
        };
        let filter = LogicalPlan::Filter {
            predicate: ScalarExpr::BinaryOp {
                op: BinaryOp::Eq,
                left: Box::new(ScalarExpr::ColumnRef { index: 0 }),
                right: Box::new(ScalarExpr::Literal(Value::Integer(42))),
            },
            input: Box::new(scan),
        };

        let result = select_indexes(filter, &catalog).unwrap();
        // Should be Filter(IndexScan) -- the filter is preserved
        if let LogicalPlan::Filter { input, .. } = &result {
            if let LogicalPlan::IndexScan {
                index_name,
                table_name,
                ..
            } = input.as_ref()
            {
                assert_eq!(index_name, "idx_users_id");
                assert_eq!(table_name, "users");
            } else {
                panic!("expected IndexScan inside Filter, got {:?}", input);
            }
        } else {
            panic!("expected Filter(IndexScan), got {:?}", result);
        }
    }

    #[test]
    fn no_index_keeps_filter() {
        let catalog = test_catalog();
        let scan = LogicalPlan::Scan {
            table_id: 1,
            table_name: "users".to_string(),
            schema: PlanSchema {
                columns: vec![PlanColumn {
                    name: "name".to_string(),
                    sql_type: SqlType::Text,
                    nullable: false,
                }],
            },
        };
        // Filter on col1 (name) -- no index on that column
        let filter = LogicalPlan::Filter {
            predicate: ScalarExpr::BinaryOp {
                op: BinaryOp::Eq,
                left: Box::new(ScalarExpr::ColumnRef { index: 1 }),
                right: Box::new(ScalarExpr::Literal(Value::Text("alice".to_string()))),
            },
            input: Box::new(scan),
        };

        let result = select_indexes(filter, &catalog).unwrap();
        assert!(matches!(result, LogicalPlan::Filter { .. }));
    }
}
