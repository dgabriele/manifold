pub mod expr;
pub mod plan;

use std::collections::HashMap;

use plan::{
    AggregateExpr, LogicalPlan, OrderByExpr, PlanColumn, PlanSchema, ScalarExpr,
};

use crate::binder::{BoundExpr, BoundSelect, BoundStatement, InsertSource};
use crate::catalog::schema::TableId;
use crate::catalog::Catalog;
use crate::error::{Result, SqlError};
use crate::types::SqlType;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn plan(catalog: &Catalog, stmt: &BoundStatement) -> Result<LogicalPlan> {
    match stmt {
        BoundStatement::Select(select) => plan_select(catalog, select),

        BoundStatement::Insert {
            table_id,
            table_name,
            columns,
            source,
        } => plan_insert(catalog, *table_id, table_name, columns, source),

        BoundStatement::Update {
            table_id,
            table_name,
            assignments,
            filter,
        } => plan_update(catalog, *table_id, table_name, assignments, filter.as_ref()),

        BoundStatement::Delete {
            table_id,
            table_name,
            filter,
        } => plan_delete(catalog, *table_id, table_name, filter.as_ref()),

        BoundStatement::CreateTable {
            name,
            columns,
            constraints,
            if_not_exists,
        } => Ok(LogicalPlan::CreateTable {
            name: name.clone(),
            columns: columns.clone(),
            constraints: constraints.clone(),
            if_not_exists: *if_not_exists,
        }),

        BoundStatement::DropTable { name, if_exists } => Ok(LogicalPlan::DropTable {
            name: name.clone(),
            if_exists: *if_exists,
        }),

        BoundStatement::AlterTable {
            table_name,
            operation,
        } => Ok(LogicalPlan::AlterTable {
            table_name: table_name.clone(),
            operation: operation.clone(),
        }),

        BoundStatement::CreateIndex {
            index_name,
            table_name,
            columns,
            unique,
            if_not_exists,
        } => Ok(LogicalPlan::CreateIndex {
            index_name: index_name.clone(),
            table_name: table_name.clone(),
            columns: columns.clone(),
            unique: *unique,
            if_not_exists: *if_not_exists,
        }),

        BoundStatement::DropIndex { name, if_exists } => Ok(LogicalPlan::DropIndex {
            name: name.clone(),
            if_exists: *if_exists,
        }),

        BoundStatement::Union { left, right, all } => {
            let left_plan = plan(catalog, left)?;
            let right_plan = plan(catalog, right)?;
            // Use the left side's schema for the union output.
            let schema = left_plan
                .schema()
                .cloned()
                .unwrap_or_else(PlanSchema::empty);
            Ok(LogicalPlan::Union {
                left: Box::new(left_plan),
                right: Box::new(right_plan),
                all: *all,
                schema,
            })
        }

        BoundStatement::Explain(inner) => {
            let inner_plan = plan(catalog, inner)?;
            Ok(LogicalPlan::Explain {
                input: Box::new(inner_plan),
            })
        }

        BoundStatement::Analyze { table_name } => Ok(LogicalPlan::Analyze {
            table_name: table_name.clone(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Column offset mapping
// ---------------------------------------------------------------------------

/// Maps (table_id) -> starting column offset in the combined row.
/// This is the key data structure for converting BoundExpr (which references
/// columns by table_id + column_index) to ScalarExpr (positional index).
type ColumnOffsets = HashMap<TableId, usize>;

// ---------------------------------------------------------------------------
// SELECT planning
// ---------------------------------------------------------------------------

fn plan_select(catalog: &Catalog, select: &BoundSelect) -> Result<LogicalPlan> {
    let mut offsets = ColumnOffsets::new();

    // 1. Build FROM: initial scan node(s)
    let mut current = if select.from.is_empty() {
        // No FROM clause — e.g., SELECT 1+1 or VALUES
        LogicalPlan::Empty
    } else {
        let first_table = &select.from[0];
        let schema = build_scan_schema(catalog, &first_table.table_name)?;
        offsets.insert(first_table.table_id, 0);
        LogicalPlan::Scan {
            table_id: first_table.table_id,
            table_name: first_table.table_name.clone(),
            schema,
        }
    };

    // Additional FROM tables are implicit cross joins
    for table_ref in select.from.iter().skip(1) {
        let current_width = plan_width(&current);
        offsets.insert(table_ref.table_id, current_width);
        let right_schema = build_scan_schema(catalog, &table_ref.table_name)?;
        let combined_schema = PlanSchema::concat(
            current.schema().unwrap_or(&PlanSchema::empty()),
            &right_schema,
        );
        current = LogicalPlan::Join {
            join_type: crate::binder::JoinType::Cross,
            left: Box::new(current),
            right: Box::new(LogicalPlan::Scan {
                table_id: table_ref.table_id,
                table_name: table_ref.table_name.clone(),
                schema: right_schema,
            }),
            condition: None,
            schema: combined_schema,
        };
    }

    // 2. Process explicit JOINs
    for join in &select.joins {
        let current_width = plan_width(&current);
        offsets.insert(join.table.table_id, current_width);
        let right_schema = build_scan_schema(catalog, &join.table.table_name)?;
        let combined_schema = PlanSchema::concat(
            current.schema().unwrap_or(&PlanSchema::empty()),
            &right_schema,
        );
        let condition = match &join.condition {
            Some(expr) => Some(lower_expr(expr, &offsets)?),
            None => None,
        };
        current = LogicalPlan::Join {
            join_type: join.join_type,
            left: Box::new(current),
            right: Box::new(LogicalPlan::Scan {
                table_id: join.table.table_id,
                table_name: join.table.table_name.clone(),
                schema: right_schema,
            }),
            condition,
            schema: combined_schema,
        };
    }

    // 3. WHERE filter
    if let Some(filter_expr) = &select.filter {
        let predicate = lower_expr(filter_expr, &offsets)?;
        current = LogicalPlan::Filter {
            predicate,
            input: Box::new(current),
        };
    }

    // 4. Check if we need aggregation (GROUP BY or aggregate functions in projection/having)
    let has_aggregates = select.projection.iter().any(|item| has_aggregate(&item.expr))
        || select.having.as_ref().map_or(false, |h| has_aggregate(h))
        || !select.group_by.is_empty();

    if has_aggregates {
        // Lower GROUP BY expressions
        let group_by: Vec<ScalarExpr> = select
            .group_by
            .iter()
            .map(|expr| lower_expr(expr, &offsets))
            .collect::<Result<Vec<_>>>()?;

        // Collect aggregate expressions from projection and having
        let mut aggregates = Vec::new();
        collect_aggregates_from_items(&select.projection, &mut aggregates);
        if let Some(having) = &select.having {
            collect_aggregates_from_expr(having, &mut aggregates);
        }

        // Build aggregate schema: group_by columns first, then aggregate results
        let mut agg_columns = Vec::new();
        for gb_expr in &select.group_by {
            agg_columns.push(plan_column_for_bound_expr(gb_expr));
        }
        let lowered_aggregates: Vec<AggregateExpr> = aggregates
            .iter()
            .map(|agg| lower_aggregate(agg, &offsets))
            .collect::<Result<Vec<_>>>()?;
        for agg in &lowered_aggregates {
            agg_columns.push(PlanColumn {
                name: format!("{}()", agg.func.as_str()),
                sql_type: agg.result_type.clone(),
                nullable: true,
            });
        }

        let agg_schema = PlanSchema {
            columns: agg_columns,
        };

        current = LogicalPlan::Aggregate {
            group_by,
            aggregates: lowered_aggregates,
            schema: agg_schema,
            input: Box::new(current),
        };

        // After aggregation, the offset map changes. Group-by columns are at
        // positions 0..n_group_by and aggregates follow. We build a new offset
        // context for post-aggregation expressions.

        // 5. HAVING filter (post-aggregation)
        if let Some(having_expr) = &select.having {
            let predicate = lower_post_agg_expr(
                having_expr,
                &select.group_by,
                &aggregates,
                &offsets,
            )?;
            current = LogicalPlan::Filter {
                predicate,
                input: Box::new(current),
            };
        }

        // 6. Project (post-aggregation)
        let (expressions, aliases, proj_schema) = build_post_agg_projection(
            &select.projection,
            &select.group_by,
            &aggregates,
            &offsets,
        )?;

        current = LogicalPlan::Project {
            expressions,
            aliases,
            schema: proj_schema,
            input: Box::new(current),
        };
    } else {
        // 6. Project (no aggregation)
        let mut expressions = Vec::new();
        let mut aliases = Vec::new();
        let mut proj_columns = Vec::new();

        for item in &select.projection {
            let scalar = lower_expr(&item.expr, &offsets)?;
            let alias = item
                .alias
                .clone()
                .unwrap_or_else(|| item.expr.display_name());
            let col = plan_column_for_bound_expr(&item.expr);
            expressions.push(scalar);
            aliases.push(alias);
            proj_columns.push(col);
        }

        let proj_schema = PlanSchema {
            columns: proj_columns,
        };

        current = LogicalPlan::Project {
            expressions,
            aliases,
            schema: proj_schema,
            input: Box::new(current),
        };
    }

    // 7. DISTINCT
    if select.distinct {
        current = LogicalPlan::Distinct {
            input: Box::new(current),
        };
    }

    // 8. ORDER BY
    if !select.order_by.is_empty() {
        // After projection, we need to resolve ORDER BY expressions against the
        // projection output. For simplicity, we lower them against the original
        // offsets and wrap them. However, since ORDER BY comes after projection
        // in the logical plan, we need to reference projection output columns.
        // We do this by looking up the expression in the projection list.
        let order_by = select
            .order_by
            .iter()
            .map(|ob| {
                let expr = lower_order_by_expr(
                    &ob.expr,
                    &select.projection,
                    &offsets,
                )?;
                Ok(OrderByExpr {
                    expr,
                    asc: ob.asc,
                    nulls_first: ob.nulls_first,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        current = LogicalPlan::Sort {
            order_by,
            input: Box::new(current),
        };
    }

    // 9. LIMIT / OFFSET
    if select.limit.is_some() || select.offset.is_some() {
        let count = select
            .limit
            .as_ref()
            .map(|e| lower_expr(e, &offsets))
            .transpose()?;
        let offset = select
            .offset
            .as_ref()
            .map(|e| lower_expr(e, &offsets))
            .transpose()?;
        current = LogicalPlan::Limit {
            count,
            offset,
            input: Box::new(current),
        };
    }

    Ok(current)
}

// ---------------------------------------------------------------------------
// INSERT planning
// ---------------------------------------------------------------------------

fn plan_insert(
    catalog: &Catalog,
    table_id: TableId,
    table_name: &str,
    columns: &[usize],
    source: &InsertSource,
) -> Result<LogicalPlan> {
    let source_plan = match source {
        InsertSource::Values(rows) => {
            let empty_offsets = ColumnOffsets::new();
            let mut lowered_rows = Vec::with_capacity(rows.len());

            // Infer schema from first row
            let schema_cols = if let Some(first_row) = rows.first() {
                first_row
                    .iter()
                    .enumerate()
                    .map(|(i, expr)| PlanColumn {
                        name: format!("column{}", i + 1),
                        sql_type: bound_expr_type(expr),
                        nullable: true,
                    })
                    .collect()
            } else {
                Vec::new()
            };

            for row in rows {
                let lowered_row = row
                    .iter()
                    .map(|expr| lower_expr(expr, &empty_offsets))
                    .collect::<Result<Vec<_>>>()?;
                lowered_rows.push(lowered_row);
            }

            LogicalPlan::Values {
                rows: lowered_rows,
                schema: PlanSchema {
                    columns: schema_cols,
                },
            }
        }
        InsertSource::Select(select) => plan_select(catalog, select)?,
    };

    Ok(LogicalPlan::Insert {
        table_id,
        table_name: table_name.to_string(),
        columns: columns.to_vec(),
        source: Box::new(source_plan),
    })
}

// ---------------------------------------------------------------------------
// UPDATE planning
// ---------------------------------------------------------------------------

fn plan_update(
    catalog: &Catalog,
    table_id: TableId,
    table_name: &str,
    assignments: &[crate::binder::BoundAssignment],
    filter: Option<&BoundExpr>,
) -> Result<LogicalPlan> {
    let schema = build_scan_schema(catalog, table_name)?;
    let mut offsets = ColumnOffsets::new();
    offsets.insert(table_id, 0);

    let mut current: LogicalPlan = LogicalPlan::Scan {
        table_id,
        table_name: table_name.to_string(),
        schema,
    };

    if let Some(filter_expr) = filter {
        let predicate = lower_expr(filter_expr, &offsets)?;
        current = LogicalPlan::Filter {
            predicate,
            input: Box::new(current),
        };
    }

    let lowered_assignments = assignments
        .iter()
        .map(|a| {
            let value = lower_expr(&a.value, &offsets)?;
            Ok((a.column_index, value))
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(LogicalPlan::Update {
        table_id,
        table_name: table_name.to_string(),
        assignments: lowered_assignments,
        input: Box::new(current),
    })
}

// ---------------------------------------------------------------------------
// DELETE planning
// ---------------------------------------------------------------------------

fn plan_delete(
    catalog: &Catalog,
    table_id: TableId,
    table_name: &str,
    filter: Option<&BoundExpr>,
) -> Result<LogicalPlan> {
    let schema = build_scan_schema(catalog, table_name)?;
    let mut offsets = ColumnOffsets::new();
    offsets.insert(table_id, 0);

    let mut current: LogicalPlan = LogicalPlan::Scan {
        table_id,
        table_name: table_name.to_string(),
        schema,
    };

    if let Some(filter_expr) = filter {
        let predicate = lower_expr(filter_expr, &offsets)?;
        current = LogicalPlan::Filter {
            predicate,
            input: Box::new(current),
        };
    }

    Ok(LogicalPlan::Delete {
        table_id,
        table_name: table_name.to_string(),
        input: Box::new(current),
    })
}

// ---------------------------------------------------------------------------
// Expression lowering: BoundExpr -> ScalarExpr
// ---------------------------------------------------------------------------

/// Convert a `BoundExpr` to a `ScalarExpr` using column offset mapping.
/// The key operation: a `BoundExpr::Column { table_id, column_index }` becomes
/// `ScalarExpr::ColumnRef { index: offsets[table_id] + column_index }`.
fn lower_expr(expr: &BoundExpr, offsets: &ColumnOffsets) -> Result<ScalarExpr> {
    match expr {
        BoundExpr::Column(col_ref) => {
            let base = offsets.get(&col_ref.table_id).copied().unwrap_or(0);
            Ok(ScalarExpr::ColumnRef {
                index: base + col_ref.column_index,
            })
        }
        BoundExpr::Literal(val) => Ok(ScalarExpr::Literal(val.clone())),
        BoundExpr::Parameter(idx) => Ok(ScalarExpr::Parameter(*idx)),
        BoundExpr::BinaryOp {
            op, left, right, ..
        } => Ok(ScalarExpr::BinaryOp {
            op: *op,
            left: Box::new(lower_expr(left, offsets)?),
            right: Box::new(lower_expr(right, offsets)?),
        }),
        BoundExpr::UnaryOp { op, operand, .. } => Ok(ScalarExpr::UnaryOp {
            op: *op,
            operand: Box::new(lower_expr(operand, offsets)?),
        }),
        BoundExpr::IsNull { operand, negated } => Ok(ScalarExpr::IsNull {
            operand: Box::new(lower_expr(operand, offsets)?),
            negated: *negated,
        }),
        BoundExpr::InList {
            expr,
            list,
            negated,
        } => {
            let lowered_list = list
                .iter()
                .map(|e| lower_expr(e, offsets))
                .collect::<Result<Vec<_>>>()?;
            Ok(ScalarExpr::InList {
                expr: Box::new(lower_expr(expr, offsets)?),
                list: lowered_list,
                negated: *negated,
            })
        }
        BoundExpr::Between {
            expr,
            low,
            high,
            negated,
        } => Ok(ScalarExpr::Between {
            expr: Box::new(lower_expr(expr, offsets)?),
            low: Box::new(lower_expr(low, offsets)?),
            high: Box::new(lower_expr(high, offsets)?),
            negated: *negated,
        }),
        BoundExpr::Like {
            expr,
            pattern,
            negated,
        } => Ok(ScalarExpr::Like {
            expr: Box::new(lower_expr(expr, offsets)?),
            pattern: Box::new(lower_expr(pattern, offsets)?),
            negated: *negated,
        }),
        BoundExpr::Function { name, args, .. } => {
            let lowered_args = args
                .iter()
                .map(|a| lower_expr(a, offsets))
                .collect::<Result<Vec<_>>>()?;
            Ok(ScalarExpr::Function {
                name: name.clone(),
                args: lowered_args,
            })
        }
        BoundExpr::Aggregate { .. } => {
            // Aggregates in non-aggregate context should not appear here.
            // They are handled by the aggregation planning path.
            Err(SqlError::Plan(
                "aggregate function in unexpected position".to_string(),
            ))
        }
        BoundExpr::Cast {
            expr, target_type, ..
        } => Ok(ScalarExpr::Cast {
            expr: Box::new(lower_expr(expr, offsets)?),
            target_type: target_type.clone(),
        }),
        BoundExpr::Wildcard => {
            // Wildcard should have been expanded by the binder
            Err(SqlError::Plan(
                "unexpected wildcard in expression".to_string(),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Post-aggregation expression lowering
// ---------------------------------------------------------------------------

/// After aggregation, the output schema is:
///   [group_by_0, group_by_1, ..., agg_0, agg_1, ...]
/// This function lowers a BoundExpr that may reference group-by columns
/// or contain aggregate calls into a ScalarExpr that references the
/// aggregate node's output by positional index.
fn lower_post_agg_expr(
    expr: &BoundExpr,
    group_by: &[BoundExpr],
    aggregates: &[BoundExpr],
    offsets: &ColumnOffsets,
) -> Result<ScalarExpr> {
    match expr {
        BoundExpr::Column(col_ref) => {
            // Check if this column is one of the group-by expressions
            for (i, gb) in group_by.iter().enumerate() {
                if let BoundExpr::Column(gb_col) = gb {
                    if gb_col.table_id == col_ref.table_id
                        && gb_col.column_index == col_ref.column_index
                    {
                        return Ok(ScalarExpr::ColumnRef { index: i });
                    }
                }
            }
            // Not in group by — this would be an error in strict SQL, but we
            // allow it by resolving against the original offsets
            let base = offsets.get(&col_ref.table_id).copied().unwrap_or(0);
            Ok(ScalarExpr::ColumnRef {
                index: base + col_ref.column_index,
            })
        }
        BoundExpr::Aggregate { .. } => {
            // Find this aggregate's position in the collected aggregates list
            let n_group = group_by.len();
            for (i, agg) in aggregates.iter().enumerate() {
                if std::ptr::eq(
                    expr as *const BoundExpr,
                    agg as *const BoundExpr,
                ) || aggregates_match(expr, agg)
                {
                    return Ok(ScalarExpr::ColumnRef {
                        index: n_group + i,
                    });
                }
            }
            // Not found in collected aggregates — shouldn't happen
            Err(SqlError::Plan(
                "aggregate expression not found in aggregate list".to_string(),
            ))
        }
        BoundExpr::BinaryOp {
            op, left, right, ..
        } => Ok(ScalarExpr::BinaryOp {
            op: *op,
            left: Box::new(lower_post_agg_expr(left, group_by, aggregates, offsets)?),
            right: Box::new(lower_post_agg_expr(right, group_by, aggregates, offsets)?),
        }),
        BoundExpr::UnaryOp { op, operand, .. } => Ok(ScalarExpr::UnaryOp {
            op: *op,
            operand: Box::new(lower_post_agg_expr(operand, group_by, aggregates, offsets)?),
        }),
        BoundExpr::IsNull { operand, negated } => Ok(ScalarExpr::IsNull {
            operand: Box::new(lower_post_agg_expr(operand, group_by, aggregates, offsets)?),
            negated: *negated,
        }),
        BoundExpr::Literal(val) => Ok(ScalarExpr::Literal(val.clone())),
        BoundExpr::Parameter(idx) => Ok(ScalarExpr::Parameter(*idx)),
        BoundExpr::Function { name, args, .. } => {
            let lowered = args
                .iter()
                .map(|a| lower_post_agg_expr(a, group_by, aggregates, offsets))
                .collect::<Result<Vec<_>>>()?;
            Ok(ScalarExpr::Function {
                name: name.clone(),
                args: lowered,
            })
        }
        BoundExpr::Cast {
            expr, target_type, ..
        } => Ok(ScalarExpr::Cast {
            expr: Box::new(lower_post_agg_expr(expr, group_by, aggregates, offsets)?),
            target_type: target_type.clone(),
        }),
        BoundExpr::InList {
            expr,
            list,
            negated,
        } => {
            let lowered_list = list
                .iter()
                .map(|e| lower_post_agg_expr(e, group_by, aggregates, offsets))
                .collect::<Result<Vec<_>>>()?;
            Ok(ScalarExpr::InList {
                expr: Box::new(lower_post_agg_expr(expr, group_by, aggregates, offsets)?),
                list: lowered_list,
                negated: *negated,
            })
        }
        BoundExpr::Between {
            expr,
            low,
            high,
            negated,
        } => Ok(ScalarExpr::Between {
            expr: Box::new(lower_post_agg_expr(expr, group_by, aggregates, offsets)?),
            low: Box::new(lower_post_agg_expr(low, group_by, aggregates, offsets)?),
            high: Box::new(lower_post_agg_expr(high, group_by, aggregates, offsets)?),
            negated: *negated,
        }),
        BoundExpr::Like {
            expr,
            pattern,
            negated,
        } => Ok(ScalarExpr::Like {
            expr: Box::new(lower_post_agg_expr(expr, group_by, aggregates, offsets)?),
            pattern: Box::new(lower_post_agg_expr(pattern, group_by, aggregates, offsets)?),
            negated: *negated,
        }),
        BoundExpr::Wildcard => Err(SqlError::Plan(
            "unexpected wildcard in post-aggregation expression".to_string(),
        )),
    }
}

/// Check if two BoundExpr::Aggregate values describe the same aggregate.
fn aggregates_match(a: &BoundExpr, b: &BoundExpr) -> bool {
    match (a, b) {
        (
            BoundExpr::Aggregate {
                func: f1,
                arg: a1,
                distinct: d1,
                ..
            },
            BoundExpr::Aggregate {
                func: f2,
                arg: a2,
                distinct: d2,
                ..
            },
        ) => {
            std::mem::discriminant(f1) == std::mem::discriminant(f2)
                && *d1 == *d2
                && format!("{a1:?}") == format!("{a2:?}")
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// ORDER BY lowering
// ---------------------------------------------------------------------------

/// Lower an ORDER BY expression. ORDER BY occurs after projection, so we
/// first try to match it against the projection output columns by position.
/// If the expression matches a projection item, we reference it by position
/// in the projection output. Otherwise we lower it against original offsets.
fn lower_order_by_expr(
    expr: &BoundExpr,
    projection: &[crate::binder::BoundSelectItem],
    offsets: &ColumnOffsets,
) -> Result<ScalarExpr> {
    // Check if this ORDER BY expression matches a projection item
    for (i, item) in projection.iter().enumerate() {
        if bound_exprs_match(expr, &item.expr) {
            return Ok(ScalarExpr::ColumnRef { index: i });
        }
    }
    // Fall back: lower against offsets (this handles expressions not in projection)
    // but reference projection output indices. For simple columns, map them.
    if let BoundExpr::Column(col_ref) = expr {
        // Try to find this column in the projection
        for (i, item) in projection.iter().enumerate() {
            if let BoundExpr::Column(proj_col) = &item.expr {
                if proj_col.table_id == col_ref.table_id
                    && proj_col.column_index == col_ref.column_index
                {
                    return Ok(ScalarExpr::ColumnRef { index: i });
                }
            }
        }
    }
    // Final fallback: lower against original offsets
    lower_expr(expr, offsets)
}

/// Simple structural equality check for BoundExpr.
fn bound_exprs_match(a: &BoundExpr, b: &BoundExpr) -> bool {
    format!("{a:?}") == format!("{b:?}")
}

// ---------------------------------------------------------------------------
// Aggregate helpers
// ---------------------------------------------------------------------------

/// Check whether a BoundExpr contains any aggregate function call.
fn has_aggregate(expr: &BoundExpr) -> bool {
    match expr {
        BoundExpr::Aggregate { .. } => true,
        BoundExpr::BinaryOp { left, right, .. } => has_aggregate(left) || has_aggregate(right),
        BoundExpr::UnaryOp { operand, .. } => has_aggregate(operand),
        BoundExpr::IsNull { operand, .. } => has_aggregate(operand),
        BoundExpr::InList { expr, list, .. } => {
            has_aggregate(expr) || list.iter().any(has_aggregate)
        }
        BoundExpr::Between {
            expr, low, high, ..
        } => has_aggregate(expr) || has_aggregate(low) || has_aggregate(high),
        BoundExpr::Like { expr, pattern, .. } => has_aggregate(expr) || has_aggregate(pattern),
        BoundExpr::Function { args, .. } => args.iter().any(has_aggregate),
        BoundExpr::Cast { expr, .. } => has_aggregate(expr),
        _ => false,
    }
}

/// Collect all aggregate expressions from the projection list.
fn collect_aggregates_from_items(
    items: &[crate::binder::BoundSelectItem],
    out: &mut Vec<BoundExpr>,
) {
    for item in items {
        collect_aggregates_from_expr(&item.expr, out);
    }
}

/// Recursively collect aggregate expressions from a BoundExpr.
fn collect_aggregates_from_expr(expr: &BoundExpr, out: &mut Vec<BoundExpr>) {
    match expr {
        BoundExpr::Aggregate { .. } => {
            // Check for duplicates
            if !out.iter().any(|existing| aggregates_match(existing, expr)) {
                out.push(expr.clone());
            }
        }
        BoundExpr::BinaryOp { left, right, .. } => {
            collect_aggregates_from_expr(left, out);
            collect_aggregates_from_expr(right, out);
        }
        BoundExpr::UnaryOp { operand, .. } => {
            collect_aggregates_from_expr(operand, out);
        }
        BoundExpr::IsNull { operand, .. } => {
            collect_aggregates_from_expr(operand, out);
        }
        BoundExpr::InList { expr, list, .. } => {
            collect_aggregates_from_expr(expr, out);
            for e in list {
                collect_aggregates_from_expr(e, out);
            }
        }
        BoundExpr::Between {
            expr, low, high, ..
        } => {
            collect_aggregates_from_expr(expr, out);
            collect_aggregates_from_expr(low, out);
            collect_aggregates_from_expr(high, out);
        }
        BoundExpr::Like { expr, pattern, .. } => {
            collect_aggregates_from_expr(expr, out);
            collect_aggregates_from_expr(pattern, out);
        }
        BoundExpr::Function { args, .. } => {
            for a in args {
                collect_aggregates_from_expr(a, out);
            }
        }
        BoundExpr::Cast { expr, .. } => {
            collect_aggregates_from_expr(expr, out);
        }
        _ => {}
    }
}

/// Lower a BoundExpr::Aggregate to an AggregateExpr.
fn lower_aggregate(expr: &BoundExpr, offsets: &ColumnOffsets) -> Result<AggregateExpr> {
    match expr {
        BoundExpr::Aggregate {
            func,
            arg,
            distinct,
            result_type,
        } => {
            let lowered_arg = match arg {
                Some(a) => Some(lower_expr(a, offsets)?),
                None => None,
            };
            Ok(AggregateExpr {
                func: *func,
                arg: lowered_arg,
                distinct: *distinct,
                result_type: result_type.clone(),
            })
        }
        _ => Err(SqlError::Plan(
            "expected aggregate expression".to_string(),
        )),
    }
}

/// Build projection expressions and schema for a post-aggregation projection.
fn build_post_agg_projection(
    projection: &[crate::binder::BoundSelectItem],
    group_by: &[BoundExpr],
    aggregates: &[BoundExpr],
    offsets: &ColumnOffsets,
) -> Result<(Vec<ScalarExpr>, Vec<String>, PlanSchema)> {
    let mut expressions = Vec::new();
    let mut aliases = Vec::new();
    let mut columns = Vec::new();

    for item in projection {
        let scalar = lower_post_agg_expr(&item.expr, group_by, aggregates, offsets)?;
        let alias = item
            .alias
            .clone()
            .unwrap_or_else(|| item.expr.display_name());
        let col = plan_column_for_bound_expr(&item.expr);
        expressions.push(scalar);
        aliases.push(alias);
        columns.push(col);
    }

    Ok((expressions, aliases, PlanSchema { columns }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a PlanSchema for a table scan from the catalog.
fn build_scan_schema(catalog: &Catalog, table_name: &str) -> Result<PlanSchema> {
    let table = catalog
        .get_table(table_name)
        .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?;
    let columns = table
        .columns
        .iter()
        .map(|col| PlanColumn {
            name: col.name.clone(),
            sql_type: col.sql_type.clone(),
            nullable: col.nullable,
        })
        .collect();
    Ok(PlanSchema { columns })
}

/// Compute the total width (number of columns) of a plan's output.
fn plan_width(plan: &LogicalPlan) -> usize {
    plan.schema().map_or(0, |s| s.len())
}

/// Best-effort type inference for a BoundExpr.
fn bound_expr_type(expr: &BoundExpr) -> SqlType {
    match expr {
        BoundExpr::Column(col) => col.sql_type.clone(),
        BoundExpr::Literal(val) => val.sql_type().unwrap_or(SqlType::Text),
        BoundExpr::BinaryOp { result_type, .. } => result_type.clone(),
        BoundExpr::UnaryOp { result_type, .. } => result_type.clone(),
        BoundExpr::IsNull { .. } => SqlType::Boolean,
        BoundExpr::InList { .. } => SqlType::Boolean,
        BoundExpr::Between { .. } => SqlType::Boolean,
        BoundExpr::Like { .. } => SqlType::Boolean,
        BoundExpr::Function { result_type, .. } => result_type.clone(),
        BoundExpr::Aggregate { result_type, .. } => result_type.clone(),
        BoundExpr::Cast { target_type, .. } => target_type.clone(),
        BoundExpr::Parameter(_) | BoundExpr::Wildcard => SqlType::Text,
    }
}

/// Create a PlanColumn from a BoundExpr (for projection schemas).
fn plan_column_for_bound_expr(expr: &BoundExpr) -> PlanColumn {
    let name = expr.display_name();
    let sql_type = bound_expr_type(expr);
    let nullable = match expr {
        BoundExpr::Column(col) => col.nullable,
        BoundExpr::Literal(val) => val.is_null(),
        BoundExpr::IsNull { .. } => false,
        _ => true,
    };
    PlanColumn {
        name,
        sql_type,
        nullable,
    }
}
