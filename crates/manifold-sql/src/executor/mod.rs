pub mod aggregate;
pub mod explain;
mod filter;
pub mod join;
mod limit;
mod project;
mod scan;
mod sort;
pub mod subquery;
pub mod union;
mod values;

use manifold::{MultimapTableDefinition, ReadableDatabase, ReadableTable, TableDefinition};

use crate::binder::AlterTableOp;
use crate::catalog::persist;
use crate::catalog::schema::{ColumnDef, ConstraintDef, IndexDef, TableSchema};
use crate::catalog::Catalog;
use crate::error::{Result, SqlError};
use crate::expr::eval::evaluate;
use crate::optimizer::statistics::{IndexStatistics, TableStatistics};
use crate::planner::plan::{LogicalPlan, ScalarExpr};
use crate::storage::catalog_tables::STATISTICS_TABLE;
use crate::storage::index::encode_index_key;
use crate::storage::row_format::{decode_row, encode_row};
use crate::types::{SqlType, Value};
use crate::ResultSet;
use crate::Row;

// ---------------------------------------------------------------------------
// Executor trait
// ---------------------------------------------------------------------------

pub trait Executor {
    fn next(&mut self) -> Result<Option<Vec<Value>>>;
}

// ---------------------------------------------------------------------------
// Table name helpers
// ---------------------------------------------------------------------------

fn data_table_name(table_name: &str) -> &'static str {
    Box::leak(format!("data_{table_name}").into_boxed_str())
}

fn index_table_name(table_name: &str, index_name: &str, unique: bool) -> &'static str {
    let suffix = if unique { "_uq" } else { "" };
    Box::leak(format!("idx_{table_name}_{index_name}{suffix}").into_boxed_str())
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Execute a mutating statement (INSERT, UPDATE, DELETE, CREATE, DROP, etc.).
/// Returns the number of rows affected.
pub fn execute_mut(
    db: &manifold::Database,
    catalog: &mut Catalog,
    plan: LogicalPlan,
    params: &[Value],
) -> Result<u64> {
    let write_txn = db.begin_write()?;
    let result = execute_plan_mut(&write_txn, catalog, &plan, params)?;
    write_txn.commit()?;
    Ok(result)
}

/// Execute a query statement (SELECT). Returns a result set.
pub fn execute_query(
    db: &manifold::Database,
    catalog: &Catalog,
    plan: LogicalPlan,
    params: &[Value],
) -> Result<ResultSet> {
    let read_txn = db.begin_read()?;
    // Resolve subquery expressions before building the executor tree.
    let plan = subquery::resolve_subqueries_read(plan, &read_txn, catalog, params)?;
    let mut executor = build_read_query_executor(&read_txn, catalog, &plan, params)?;
    collect_result_set(&mut *executor, &plan)
}

/// Execute a mutating statement within an existing write transaction.
pub fn execute_in_txn(
    txn: &manifold::WriteTransaction,
    catalog: &mut Catalog,
    plan: LogicalPlan,
    params: &[Value],
) -> Result<u64> {
    execute_plan_mut(txn, catalog, &plan, params)
}

/// Execute a query within an existing write transaction.
pub fn query_in_txn(
    txn: &manifold::WriteTransaction,
    catalog: &Catalog,
    plan: LogicalPlan,
    params: &[Value],
) -> Result<ResultSet> {
    // Resolve subquery expressions before building the executor tree.
    let plan = subquery::resolve_subqueries_write(plan, txn, catalog, params)?;
    let mut executor = build_write_query_executor(txn, catalog, &plan, params)?;
    collect_result_set(&mut *executor, &plan)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn collect_result_set(executor: &mut dyn Executor, plan: &LogicalPlan) -> Result<ResultSet> {
    let mut rows = Vec::new();
    while let Some(row) = executor.next()? {
        rows.push(Row::new(row));
    }
    let columns = plan
        .schema()
        .map(|s| s.columns.iter().map(|c| c.name.clone()).collect())
        .unwrap_or_default();
    Ok(ResultSet::new(columns, rows))
}

// ---------------------------------------------------------------------------
// Build query executor from a read transaction
// ---------------------------------------------------------------------------

fn build_read_query_executor(
    txn: &manifold::ReadTransaction,
    catalog: &Catalog,
    plan: &LogicalPlan,
    params: &[Value],
) -> Result<Box<dyn Executor>> {
    match plan {
        LogicalPlan::Scan {
            table_name, schema, ..
        } => {
            let scan = scan::TableScan::from_read_txn(txn, catalog, table_name, schema.clone())?;
            Ok(Box::new(scan))
        }
        LogicalPlan::IndexScan {
            table_name,
            index_name,
            lookup_values,
            ..
        } => {
            let scan = scan::IndexPointScan::from_read_txn(
                txn, catalog, table_name, index_name, lookup_values, params,
            )?;
            Ok(Box::new(scan))
        }
        LogicalPlan::Filter { predicate, input } => {
            let child = build_read_query_executor(txn, catalog, input, params)?;
            let pred = if plan_has_scan_leaf(input) {
                shift_column_refs(predicate, 1)
            } else {
                predicate.clone()
            };
            Ok(Box::new(filter::Filter::new(child, pred, params)))
        }
        LogicalPlan::Project {
            expressions,
            input,
            ..
        } => {
            let child = build_read_query_executor(txn, catalog, input, params)?;
            let exprs = if plan_has_scan_leaf(input) {
                expressions.iter().map(|e| shift_column_refs(e, 1)).collect()
            } else {
                expressions.clone()
            };
            Ok(Box::new(project::Project::new(child, exprs, params)))
        }
        LogicalPlan::Sort { order_by, input } => {
            let child = build_read_query_executor(txn, catalog, input, params)?;
            let obs = if plan_has_scan_leaf(input) {
                order_by.iter().map(|ob| crate::planner::plan::OrderByExpr {
                    expr: shift_column_refs(&ob.expr, 1),
                    asc: ob.asc,
                    nulls_first: ob.nulls_first,
                }).collect::<Vec<_>>()
            } else {
                order_by.clone()
            };
            Ok(Box::new(sort::Sort::new(child, &obs, params)?))
        }
        LogicalPlan::Limit {
            count,
            offset,
            input,
        } => {
            let child = build_read_query_executor(txn, catalog, input, params)?;
            let count_val = count
                .as_ref()
                .map(|e| eval_to_usize(e, params))
                .transpose()?;
            let offset_val = offset
                .as_ref()
                .map(|e| eval_to_usize(e, params))
                .transpose()?
                .unwrap_or(0);
            Ok(Box::new(limit::Limit::new(child, count_val, offset_val)))
        }
        LogicalPlan::Values { rows, .. } => {
            Ok(Box::new(values::Values::new(rows.clone(), params)))
        }
        LogicalPlan::Join {
            join_type,
            left,
            right,
            condition,
            ..
        } => build_join_executor(
            |plan| build_read_query_executor(txn, catalog, plan, params),
            *join_type,
            left,
            right,
            condition.as_ref(),
            params,
        ),
        LogicalPlan::Aggregate {
            group_by,
            aggregates,
            schema,
            input,
        } => {
            let child = build_read_query_executor(txn, catalog, input, params)?;
            let has_scan = plan_has_scan_leaf(input);
            let gb = if has_scan {
                group_by.iter().map(|e| shift_column_refs(e, 1)).collect()
            } else {
                group_by.clone()
            };
            let aggs = if has_scan {
                aggregates
                    .iter()
                    .map(|ae| crate::planner::plan::AggregateExpr {
                        func: ae.func,
                        arg: ae.arg.as_ref().map(|a| shift_column_refs(a, 1)),
                        distinct: ae.distinct,
                        result_type: ae.result_type.clone(),
                    })
                    .collect()
            } else {
                aggregates.clone()
            };
            Ok(Box::new(aggregate::HashAggregate::new(
                child,
                gb,
                aggs,
                params,
                schema.clone(),
            )))
        }
        LogicalPlan::Union { left, right, all, .. } => {
            let left_exec = build_read_query_executor(txn, catalog, left, params)?;
            let right_exec = build_read_query_executor(txn, catalog, right, params)?;
            if *all {
                Ok(Box::new(union::UnionAll::new(left_exec, right_exec)))
            } else {
                Ok(Box::new(union::UnionDistinct::new(left_exec, right_exec)?))
            }
        }
        LogicalPlan::Distinct { input } => {
            let child = build_read_query_executor(txn, catalog, input, params)?;
            Ok(Box::new(union::UnionDistinct::new(child, Box::new(EmptyExecutor))?))
        }
        LogicalPlan::Explain { input } => {
            let formatted = explain::format_plan(input);
            Ok(Box::new(values::Values::new(
                vec![vec![crate::planner::plan::ScalarExpr::Literal(
                    Value::Text(formatted),
                )]],
                params,
            )))
        }
        LogicalPlan::Empty => Ok(Box::new(EmptyExecutor)),
        _ => Err(SqlError::Execute(format!(
            "unsupported plan node in query: {plan:?}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Build query executor from a write transaction
// ---------------------------------------------------------------------------

fn build_write_query_executor(
    txn: &manifold::WriteTransaction,
    catalog: &Catalog,
    plan: &LogicalPlan,
    params: &[Value],
) -> Result<Box<dyn Executor>> {
    match plan {
        LogicalPlan::Scan {
            table_name, schema, ..
        } => {
            let scan = scan::TableScan::from_write_txn(txn, catalog, table_name, schema.clone())?;
            Ok(Box::new(scan))
        }
        LogicalPlan::IndexScan {
            table_name,
            index_name,
            lookup_values,
            ..
        } => {
            let scan = scan::IndexPointScan::from_write_txn(
                txn, catalog, table_name, index_name, lookup_values, params,
            )?;
            Ok(Box::new(scan))
        }
        LogicalPlan::Filter { predicate, input } => {
            let child = build_write_query_executor(txn, catalog, input, params)?;
            let pred = if plan_has_scan_leaf(input) {
                shift_column_refs(predicate, 1)
            } else {
                predicate.clone()
            };
            Ok(Box::new(filter::Filter::new(child, pred, params)))
        }
        LogicalPlan::Project {
            expressions,
            input,
            ..
        } => {
            let child = build_write_query_executor(txn, catalog, input, params)?;
            let exprs = if plan_has_scan_leaf(input) {
                expressions.iter().map(|e| shift_column_refs(e, 1)).collect()
            } else {
                expressions.clone()
            };
            Ok(Box::new(project::Project::new(child, exprs, params)))
        }
        LogicalPlan::Sort { order_by, input } => {
            let child = build_write_query_executor(txn, catalog, input, params)?;
            let obs = if plan_has_scan_leaf(input) {
                order_by.iter().map(|ob| crate::planner::plan::OrderByExpr {
                    expr: shift_column_refs(&ob.expr, 1),
                    asc: ob.asc,
                    nulls_first: ob.nulls_first,
                }).collect::<Vec<_>>()
            } else {
                order_by.clone()
            };
            Ok(Box::new(sort::Sort::new(child, &obs, params)?))
        }
        LogicalPlan::Limit {
            count,
            offset,
            input,
        } => {
            let child = build_write_query_executor(txn, catalog, input, params)?;
            let count_val = count
                .as_ref()
                .map(|e| eval_to_usize(e, params))
                .transpose()?;
            let offset_val = offset
                .as_ref()
                .map(|e| eval_to_usize(e, params))
                .transpose()?
                .unwrap_or(0);
            Ok(Box::new(limit::Limit::new(child, count_val, offset_val)))
        }
        LogicalPlan::Values { rows, .. } => {
            Ok(Box::new(values::Values::new(rows.clone(), params)))
        }
        LogicalPlan::Join {
            join_type,
            left,
            right,
            condition,
            ..
        } => build_join_executor(
            |plan| build_write_query_executor(txn, catalog, plan, params),
            *join_type,
            left,
            right,
            condition.as_ref(),
            params,
        ),
        LogicalPlan::Aggregate {
            group_by,
            aggregates,
            schema,
            input,
        } => {
            let child = build_write_query_executor(txn, catalog, input, params)?;
            let has_scan = plan_has_scan_leaf(input);
            let gb = if has_scan {
                group_by.iter().map(|e| shift_column_refs(e, 1)).collect()
            } else {
                group_by.clone()
            };
            let aggs = if has_scan {
                aggregates
                    .iter()
                    .map(|ae| crate::planner::plan::AggregateExpr {
                        func: ae.func,
                        arg: ae.arg.as_ref().map(|a| shift_column_refs(a, 1)),
                        distinct: ae.distinct,
                        result_type: ae.result_type.clone(),
                    })
                    .collect()
            } else {
                aggregates.clone()
            };
            Ok(Box::new(aggregate::HashAggregate::new(
                child,
                gb,
                aggs,
                params,
                schema.clone(),
            )))
        }
        LogicalPlan::Union { left, right, all, .. } => {
            let left_exec = build_write_query_executor(txn, catalog, left, params)?;
            let right_exec = build_write_query_executor(txn, catalog, right, params)?;
            if *all {
                Ok(Box::new(union::UnionAll::new(left_exec, right_exec)))
            } else {
                Ok(Box::new(union::UnionDistinct::new(left_exec, right_exec)?))
            }
        }
        LogicalPlan::Distinct { input } => {
            let child = build_write_query_executor(txn, catalog, input, params)?;
            Ok(Box::new(union::UnionDistinct::new(child, Box::new(EmptyExecutor))?))
        }
        LogicalPlan::Explain { input } => {
            let formatted = explain::format_plan(input);
            Ok(Box::new(values::Values::new(
                vec![vec![crate::planner::plan::ScalarExpr::Literal(
                    Value::Text(formatted),
                )]],
                params,
            )))
        }
        LogicalPlan::Empty => Ok(Box::new(EmptyExecutor)),
        _ => Err(SqlError::Execute(format!(
            "unsupported plan node in query: {plan:?}"
        ))),
    }
}

/// Build a NestedLoopJoin from left/right sub-plans.
///
/// The `build_child` closure constructs an executor for a child plan node
/// (abstracting over read vs write transaction).
fn build_join_executor<F>(
    build_child: F,
    join_type: crate::binder::JoinType,
    left: &LogicalPlan,
    right: &LogicalPlan,
    condition: Option<&ScalarExpr>,
    params: &[Value],
) -> Result<Box<dyn Executor>>
where
    F: Fn(&LogicalPlan) -> Result<Box<dyn Executor>>,
{
    let left_has_scan = plan_has_scan_leaf(left);
    let right_has_scan = plan_has_scan_leaf(right);
    let left_skip = if left_has_scan { 1 } else { 0 };
    let right_skip = if right_has_scan { 1 } else { 0 };

    let left_plan_width = left.schema().map_or(0, |s| s.len());
    let right_plan_width = right.schema().map_or(0, |s| s.len());

    let left_exec = build_child(left)?;
    let right_exec = build_child(right)?;

    // Shift condition column refs to account for rowid prefixes from scans.
    // Left columns in the condition are at plan indices [0..left_plan_width).
    // In the combined row we emit (after stripping rowids), they stay at [0..left_plan_width).
    // Right columns in the condition are at plan indices [left_plan_width..).
    // In the combined row they are also at [left_plan_width..) since we strip rowids.
    // So no shifting is needed for the condition - the combined row matches the plan schema.
    let cond = condition.cloned();

    Ok(Box::new(join::NestedLoopJoin::new(
        left_exec,
        right_exec,
        join_type,
        cond,
        params,
        left_skip,
        right_skip,
        left_plan_width,
        right_plan_width,
    )?))
}

/// Shift all ColumnRef indices in a ScalarExpr by a given offset.
/// This is needed because TableScan prepends the rowid as column 0,
/// shifting all user columns by 1.
fn shift_column_refs(expr: &ScalarExpr, offset: usize) -> ScalarExpr {
    match expr {
        ScalarExpr::ColumnRef { index } => ScalarExpr::ColumnRef {
            index: index + offset,
        },
        ScalarExpr::Literal(_) | ScalarExpr::Parameter(_) => expr.clone(),
        ScalarExpr::BinaryOp { op, left, right } => ScalarExpr::BinaryOp {
            op: *op,
            left: Box::new(shift_column_refs(left, offset)),
            right: Box::new(shift_column_refs(right, offset)),
        },
        ScalarExpr::UnaryOp { op, operand } => ScalarExpr::UnaryOp {
            op: *op,
            operand: Box::new(shift_column_refs(operand, offset)),
        },
        ScalarExpr::IsNull { operand, negated } => ScalarExpr::IsNull {
            operand: Box::new(shift_column_refs(operand, offset)),
            negated: *negated,
        },
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: Box::new(shift_column_refs(expr, offset)),
            list: list.iter().map(|e| shift_column_refs(e, offset)).collect(),
            negated: *negated,
        },
        ScalarExpr::Between {
            expr,
            low,
            high,
            negated,
        } => ScalarExpr::Between {
            expr: Box::new(shift_column_refs(expr, offset)),
            low: Box::new(shift_column_refs(low, offset)),
            high: Box::new(shift_column_refs(high, offset)),
            negated: *negated,
        },
        ScalarExpr::Like {
            expr,
            pattern,
            negated,
        } => ScalarExpr::Like {
            expr: Box::new(shift_column_refs(expr, offset)),
            pattern: Box::new(shift_column_refs(pattern, offset)),
            negated: *negated,
        },
        ScalarExpr::Function { name, args } => ScalarExpr::Function {
            name: name.clone(),
            args: args.iter().map(|a| shift_column_refs(a, offset)).collect(),
        },
        ScalarExpr::Cast { expr, target_type } => ScalarExpr::Cast {
            expr: Box::new(shift_column_refs(expr, offset)),
            target_type: target_type.clone(),
        },
        // Subquery expressions: shift outer expr refs but leave subquery plan as-is
        // (the subquery has its own independent column namespace).
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => ScalarExpr::InSubquery {
            expr: Box::new(shift_column_refs(expr, offset)),
            subquery: subquery.clone(),
            negated: *negated,
        },
        ScalarExpr::Exists { subquery, negated } => ScalarExpr::Exists {
            subquery: subquery.clone(),
            negated: *negated,
        },
        ScalarExpr::ScalarSubquery { subquery } => ScalarExpr::ScalarSubquery {
            subquery: subquery.clone(),
        },
    }
}

/// Check if a plan has a table scan at its leaf (meaning output rows include a rowid prefix).
fn plan_has_scan_leaf(plan: &LogicalPlan) -> bool {
    match plan {
        LogicalPlan::Scan { .. } | LogicalPlan::IndexScan { .. } => true,
        LogicalPlan::Filter { input, .. }
        | LogicalPlan::Sort { input, .. }
        | LogicalPlan::Limit { input, .. }
        | LogicalPlan::Distinct { input } => plan_has_scan_leaf(input),
        LogicalPlan::Project { .. } => false, // Project produces its own columns
        _ => false,
    }
}

fn eval_to_usize(expr: &ScalarExpr, params: &[Value]) -> Result<usize> {
    let val = evaluate(expr, &[], params)?;
    match val {
        Value::Integer(n) if n >= 0 => Ok(n as usize),
        Value::SmallInt(n) if n >= 0 => Ok(n as usize),
        _ => Err(SqlError::Execute(format!(
            "expected non-negative integer, got {val}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Empty executor
// ---------------------------------------------------------------------------

struct EmptyExecutor;

impl Executor for EmptyExecutor {
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Mutating plan execution
// ---------------------------------------------------------------------------

fn execute_plan_mut(
    txn: &manifold::WriteTransaction,
    catalog: &mut Catalog,
    plan: &LogicalPlan,
    params: &[Value],
) -> Result<u64> {
    match plan {
        LogicalPlan::CreateTable {
            name,
            columns,
            constraints,
            if_not_exists,
        } => execute_create_table(txn, catalog, name, columns, constraints, *if_not_exists),

        LogicalPlan::DropTable { name, if_exists } => {
            execute_drop_table(txn, catalog, name, *if_exists)
        }

        LogicalPlan::AlterTable {
            table_name,
            operation,
        } => execute_alter_table(txn, catalog, table_name, operation),

        LogicalPlan::CreateIndex {
            index_name,
            table_name,
            columns,
            unique,
            if_not_exists,
        } => execute_create_index(
            txn,
            catalog,
            index_name,
            table_name,
            columns,
            *unique,
            *if_not_exists,
        ),

        LogicalPlan::DropIndex { name, if_exists } => {
            execute_drop_index(txn, catalog, name, *if_exists)
        }

        LogicalPlan::Insert {
            table_name,
            columns,
            source,
            ..
        } => execute_insert(txn, catalog, table_name, columns, source, params),

        LogicalPlan::Update {
            table_name,
            assignments,
            input,
            ..
        } => execute_update(txn, catalog, table_name, assignments, input, params),

        LogicalPlan::Delete {
            table_name, input, ..
        } => execute_delete(txn, catalog, table_name, input, params),

        LogicalPlan::Analyze { table_name } => {
            execute_analyze(txn, catalog, table_name.as_deref())
        }

        // EXPLAIN can also arrive via execute_mut when issued as a bare
        // statement (e.g. EXPLAIN CREATE TABLE …).  Just no-op.
        LogicalPlan::Explain { .. } => Ok(0),

        _ => Err(SqlError::Execute(format!(
            "unsupported mutating plan: {plan:?}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// DDL: CREATE TABLE
// ---------------------------------------------------------------------------

fn execute_create_table(
    txn: &manifold::WriteTransaction,
    catalog: &mut Catalog,
    name: &str,
    columns: &[ColumnDef],
    constraints: &[ConstraintDef],
    if_not_exists: bool,
) -> Result<u64> {
    if catalog.has_table(name) {
        if if_not_exists {
            return Ok(0);
        }
        return Err(SqlError::TableExists(name.to_string()));
    }

    let table_id = catalog.next_table_id();

    // Assign IDs to constraints.
    let mut owned_constraints = Vec::new();
    for c in constraints {
        let new_c = assign_constraint_id(catalog, c);
        owned_constraints.push(new_c);
    }

    let schema = TableSchema {
        id: table_id,
        name: name.to_string(),
        columns: columns.to_vec(),
        constraints: owned_constraints.clone(),
        next_rowid: 1,
    };

    // Create the data table in Manifold.
    let data_name = data_table_name(name);
    let def = TableDefinition::<u64, &[u8]>::new(data_name);
    txn.open_table(def)?;

    // Create index tables for PK and UNIQUE constraints.
    for constraint in &owned_constraints {
        match constraint {
            ConstraintDef::PrimaryKey {
                name: cname,
                columns: cols,
                ..
            } => {
                let idx_name = index_table_name(name, &format!("pk_{cname}"), true);
                let idx_def = TableDefinition::<&[u8], u64>::new(idx_name);
                txn.open_table(idx_def)?;

                // Register as an index in the catalog.
                let index_id = catalog.next_index_id();
                let index = IndexDef {
                    id: index_id,
                    name: format!("pk_{cname}"),
                    table_id,
                    columns: cols.clone(),
                    unique: true,
                };
                catalog.add_index(index.clone());
                persist::save_index(txn, &index)?;
            }
            ConstraintDef::Unique {
                name: cname,
                columns: cols,
                ..
            } => {
                let idx_name = index_table_name(name, &format!("uq_{cname}"), true);
                let idx_def = TableDefinition::<&[u8], u64>::new(idx_name);
                txn.open_table(idx_def)?;

                let index_id = catalog.next_index_id();
                let index = IndexDef {
                    id: index_id,
                    name: format!("uq_{cname}"),
                    table_id,
                    columns: cols.clone(),
                    unique: true,
                };
                catalog.add_index(index.clone());
                persist::save_index(txn, &index)?;
            }
            _ => {}
        }
    }

    catalog.add_table(schema.clone());
    persist::save_table(txn, &schema)?;

    Ok(0)
}

fn assign_constraint_id(catalog: &mut Catalog, c: &ConstraintDef) -> ConstraintDef {
    let id = catalog.next_constraint_id();
    match c {
        ConstraintDef::PrimaryKey {
            name, columns, ..
        } => ConstraintDef::PrimaryKey {
            id,
            name: name.clone(),
            columns: columns.clone(),
        },
        ConstraintDef::Unique {
            name, columns, ..
        } => ConstraintDef::Unique {
            id,
            name: name.clone(),
            columns: columns.clone(),
        },
        ConstraintDef::NotNull {
            name, column, ..
        } => ConstraintDef::NotNull {
            id,
            name: name.clone(),
            column: *column,
        },
        ConstraintDef::ForeignKey {
            name,
            columns,
            ref_table,
            ref_columns,
            on_delete,
            on_update,
            ..
        } => ConstraintDef::ForeignKey {
            id,
            name: name.clone(),
            columns: columns.clone(),
            ref_table: ref_table.clone(),
            ref_columns: ref_columns.clone(),
            on_delete: on_delete.clone(),
            on_update: on_update.clone(),
        },
        ConstraintDef::Check {
            name, expression, ..
        } => ConstraintDef::Check {
            id,
            name: name.clone(),
            expression: expression.clone(),
        },
    }
}

// ---------------------------------------------------------------------------
// DDL: DROP TABLE
// ---------------------------------------------------------------------------

fn execute_drop_table(
    txn: &manifold::WriteTransaction,
    catalog: &mut Catalog,
    name: &str,
    if_exists: bool,
) -> Result<u64> {
    let schema = match catalog.get_table(name) {
        Some(s) => s.clone(),
        None => {
            if if_exists {
                return Ok(0);
            }
            return Err(SqlError::TableNotFound(name.to_string()));
        }
    };

    // Drop data table.
    let data_name = data_table_name(name);
    let def = TableDefinition::<u64, &[u8]>::new(data_name);
    txn.delete_table(def)?;

    // Drop all indexes for this table.
    let indexes: Vec<IndexDef> = catalog
        .indexes_for_table(schema.id)
        .into_iter()
        .cloned()
        .collect();
    for index in &indexes {
        let idx_name = index_table_name(name, &index.name, index.unique);
        if index.unique {
            let idx_def = TableDefinition::<&[u8], u64>::new(idx_name);
            let _ = txn.delete_table(idx_def);
        } else {
            let idx_def = MultimapTableDefinition::<&[u8], u64>::new(idx_name);
            let _ = txn.delete_multimap_table(idx_def);
        }
        persist::remove_index(txn, index.id)?;
        catalog.drop_index(&index.name);
    }

    persist::remove_table(txn, schema.id)?;
    catalog.drop_table(name);

    Ok(0)
}

// ---------------------------------------------------------------------------
// DDL: ALTER TABLE
// ---------------------------------------------------------------------------

fn execute_alter_table(
    txn: &manifold::WriteTransaction,
    catalog: &mut Catalog,
    table_name: &str,
    operation: &AlterTableOp,
) -> Result<u64> {
    match operation {
        AlterTableOp::AddColumn(col_def) => {
            let schema = catalog
                .get_table(table_name)
                .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?
                .clone();
            let old_types = schema.column_types();
            let mut new_columns = schema.columns.clone();
            new_columns.push(col_def.clone());
            let new_types: Vec<_> = new_columns.iter().map(|c| c.sql_type.clone()).collect();

            // Re-encode all existing rows with the new column (NULL default).
            let data_name = data_table_name(table_name);
            let def = TableDefinition::<u64, &[u8]>::new(data_name);
            let mut table = txn.open_table(def)?;

            let mut updates = Vec::new();
            {
                let iter = table.iter().map_err(SqlError::Storage)?;
                for entry in iter {
                    let (k, v) = entry.map_err(SqlError::Storage)?;
                    let rowid = k.value();
                    let old_vals = decode_row(&old_types, v.value())?;
                    let mut new_vals = old_vals;
                    // Add NULL (or default) for the new column.
                    let default_val = col_def
                        .default
                        .as_ref()
                        .map(|d| match d {
                            crate::catalog::schema::DefaultValue::Literal(v) => v.clone(),
                            crate::catalog::schema::DefaultValue::Null => Value::Null,
                            _ => Value::Null,
                        })
                        .unwrap_or(Value::Null);
                    new_vals.push(default_val);
                    let encoded = encode_row(&new_types, &new_vals)?;
                    updates.push((rowid, encoded));
                }
            }
            for (rowid, encoded) in &updates {
                table.insert(*rowid, encoded.as_slice())?;
            }
            drop(table);

            // Update catalog.
            let schema_mut = catalog
                .get_table_mut(table_name)
                .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?;
            schema_mut.columns.push(col_def.clone());
            let updated = schema_mut.clone();
            persist::save_table(txn, &updated)?;

            Ok(0)
        }

        AlterTableOp::DropColumn(col_name) => {
            let schema = catalog
                .get_table(table_name)
                .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?
                .clone();
            let col_idx = schema
                .column_index(col_name)
                .ok_or_else(|| SqlError::ColumnNotFound(col_name.clone()))?;

            let old_types = schema.column_types();
            let mut new_columns = schema.columns.clone();
            new_columns.remove(col_idx);
            let new_types: Vec<_> = new_columns.iter().map(|c| c.sql_type.clone()).collect();

            // Re-encode all existing rows without the dropped column.
            let data_name = data_table_name(table_name);
            let def = TableDefinition::<u64, &[u8]>::new(data_name);
            let mut table = txn.open_table(def)?;

            let mut updates = Vec::new();
            {
                let iter = table.iter().map_err(SqlError::Storage)?;
                for entry in iter {
                    let (k, v) = entry.map_err(SqlError::Storage)?;
                    let rowid = k.value();
                    let mut old_vals = decode_row(&old_types, v.value())?;
                    old_vals.remove(col_idx);
                    let encoded = encode_row(&new_types, &old_vals)?;
                    updates.push((rowid, encoded));
                }
            }
            for (rowid, encoded) in &updates {
                table.insert(*rowid, encoded.as_slice())?;
            }
            drop(table);

            // Update catalog.
            let schema_mut = catalog
                .get_table_mut(table_name)
                .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?;
            schema_mut.columns.remove(col_idx);
            // Update constraint column indices that reference columns after the dropped one.
            // For simplicity, remove constraints that reference the dropped column.
            schema_mut.constraints.retain(|c| match c {
                ConstraintDef::NotNull { column, .. } => *column != col_idx,
                ConstraintDef::PrimaryKey { columns, .. }
                | ConstraintDef::Unique { columns, .. } => !columns.contains(&col_idx),
                _ => true,
            });
            let updated = schema_mut.clone();
            persist::save_table(txn, &updated)?;

            Ok(0)
        }

        AlterTableOp::RenameColumn { old, new } => {
            let schema_mut = catalog
                .get_table_mut(table_name)
                .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?;
            let col = schema_mut
                .columns
                .iter_mut()
                .find(|c| c.name.to_lowercase() == old.to_lowercase())
                .ok_or_else(|| SqlError::ColumnNotFound(old.clone()))?;
            col.name = new.clone();
            let updated = schema_mut.clone();
            persist::save_table(txn, &updated)?;

            Ok(0)
        }
    }
}

// ---------------------------------------------------------------------------
// DDL: CREATE INDEX
// ---------------------------------------------------------------------------

fn execute_create_index(
    txn: &manifold::WriteTransaction,
    catalog: &mut Catalog,
    index_name: &str,
    table_name: &str,
    column_names: &[String],
    unique: bool,
    if_not_exists: bool,
) -> Result<u64> {
    if catalog.get_index(index_name).is_some() {
        if if_not_exists {
            return Ok(0);
        }
        return Err(SqlError::IndexExists(index_name.to_string()));
    }

    let schema = catalog
        .get_table(table_name)
        .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?
        .clone();

    // Resolve column names to indices.
    let col_indices: Vec<usize> = column_names
        .iter()
        .map(|name| {
            schema
                .column_index(name)
                .ok_or_else(|| SqlError::ColumnNotFound(name.clone()))
        })
        .collect::<Result<Vec<_>>>()?;

    let index_id = catalog.next_index_id();
    let index_def = IndexDef {
        id: index_id,
        name: index_name.to_string(),
        table_id: schema.id,
        columns: col_indices.clone(),
        unique,
    };

    // Create the index table and backfill.
    let idx_tbl_name = index_table_name(table_name, index_name, unique);
    let col_types = schema.column_types();

    if unique {
        let idx_def = TableDefinition::<&[u8], u64>::new(idx_tbl_name);
        let mut idx_table = txn.open_table(idx_def)?;

        // Backfill from data table.
        let data_name = data_table_name(table_name);
        let data_def = TableDefinition::<u64, &[u8]>::new(data_name);
        let data_table = txn.open_table(data_def)?;
        let iter = data_table.iter().map_err(SqlError::Storage)?;
        for entry in iter {
            let (k, v) = entry.map_err(SqlError::Storage)?;
            let rowid = k.value();
            let row = decode_row(&col_types, v.value())?;
            let key_values: Vec<Value> = col_indices.iter().map(|&i| row[i].clone()).collect();
            let key_bytes = encode_index_key(&key_values);
            // Check uniqueness.
            if idx_table
                .get(key_bytes.as_slice())
                .map_err(SqlError::Storage)?
                .is_some()
            {
                return Err(SqlError::ConstraintViolation(format!(
                    "duplicate key in unique index '{index_name}'"
                )));
            }
            idx_table.insert(key_bytes.as_slice(), rowid)?;
        }
        drop(data_table);
        drop(idx_table);
    } else {
        let idx_def = MultimapTableDefinition::<&[u8], u64>::new(idx_tbl_name);
        let mut idx_table = txn.open_multimap_table(idx_def)?;

        let data_name = data_table_name(table_name);
        let data_def = TableDefinition::<u64, &[u8]>::new(data_name);
        let data_table = txn.open_table(data_def)?;
        let iter = data_table.iter().map_err(SqlError::Storage)?;
        for entry in iter {
            let (k, v) = entry.map_err(SqlError::Storage)?;
            let rowid = k.value();
            let row = decode_row(&col_types, v.value())?;
            let key_values: Vec<Value> = col_indices.iter().map(|&i| row[i].clone()).collect();
            let key_bytes = encode_index_key(&key_values);
            idx_table.insert(key_bytes.as_slice(), rowid)?;
        }
        drop(data_table);
        drop(idx_table);
    }

    catalog.add_index(index_def.clone());
    persist::save_index(txn, &index_def)?;

    Ok(0)
}

// ---------------------------------------------------------------------------
// DDL: DROP INDEX
// ---------------------------------------------------------------------------

fn execute_drop_index(
    txn: &manifold::WriteTransaction,
    catalog: &mut Catalog,
    name: &str,
    if_exists: bool,
) -> Result<u64> {
    let index = match catalog.get_index(name) {
        Some(idx) => idx.clone(),
        None => {
            if if_exists {
                return Ok(0);
            }
            return Err(SqlError::IndexNotFound(name.to_string()));
        }
    };

    // Find the table name.
    let table_name = catalog
        .get_table_by_id(index.table_id)
        .map(|s| s.name.clone())
        .ok_or_else(|| {
            SqlError::Internal(format!("table with id {} not found for index", index.table_id))
        })?;

    let idx_tbl_name = index_table_name(&table_name, name, index.unique);
    if index.unique {
        let idx_def = TableDefinition::<&[u8], u64>::new(idx_tbl_name);
        let _ = txn.delete_table(idx_def);
    } else {
        let idx_def = MultimapTableDefinition::<&[u8], u64>::new(idx_tbl_name);
        let _ = txn.delete_multimap_table(idx_def);
    }

    persist::remove_index(txn, index.id)?;
    catalog.drop_index(name);

    Ok(0)
}

// ---------------------------------------------------------------------------
// DML: INSERT
// ---------------------------------------------------------------------------

fn execute_insert(
    txn: &manifold::WriteTransaction,
    catalog: &mut Catalog,
    table_name: &str,
    target_columns: &[usize],
    source: &LogicalPlan,
    params: &[Value],
) -> Result<u64> {
    let schema = catalog
        .get_table(table_name)
        .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?
        .clone();
    let col_types = schema.column_types();
    let num_cols = schema.columns.len();

    // Build source executor.
    let mut source_exec = build_write_query_executor(txn, catalog, source, params)?;

    // Collect all source rows first (to release borrow on txn).
    let mut source_rows = Vec::new();
    while let Some(row) = source_exec.next()? {
        source_rows.push(row);
    }
    drop(source_exec);

    let data_name = data_table_name(table_name);
    let def = TableDefinition::<u64, &[u8]>::new(data_name);
    let mut data_table = txn.open_table(def)?;

    // Get indexes for this table.
    let indexes: Vec<IndexDef> = catalog
        .indexes_for_table(schema.id)
        .into_iter()
        .cloned()
        .collect();

    let mut next_rowid = schema.next_rowid;
    let mut count = 0u64;

    for source_row in &source_rows {
        // Build full row: fill in NULL for missing columns.
        let mut full_row = vec![Value::Null; num_cols];

        // If target_columns is empty, assume all columns in order.
        if target_columns.is_empty() {
            for (i, val) in source_row.iter().enumerate() {
                if i < num_cols {
                    full_row[i] = val.clone();
                }
            }
        } else {
            for (src_idx, &tgt_col) in target_columns.iter().enumerate() {
                if src_idx < source_row.len() && tgt_col < num_cols {
                    full_row[tgt_col] = source_row[src_idx].clone();
                }
            }
        }

        // Apply defaults for NULL columns that have defaults.
        for (i, col) in schema.columns.iter().enumerate() {
            if full_row[i].is_null()
                && let Some(default) = &col.default
            {
                full_row[i] = match default {
                    crate::catalog::schema::DefaultValue::Literal(v) => v.clone(),
                    crate::catalog::schema::DefaultValue::Null => Value::Null,
                    crate::catalog::schema::DefaultValue::CurrentTimestamp => {
                        // chrono's `now` requires the "clock" feature; use Null as fallback.
                        Value::Null
                    }
                    crate::catalog::schema::DefaultValue::CurrentDate => Value::Null,
                };
            }
        }

        // Auto-fill INTEGER PRIMARY KEY columns that are NULL (auto-increment).
        for (i, col) in schema.columns.iter().enumerate() {
            if col.is_primary_key
                && full_row[i].is_null()
                && matches!(col.sql_type, SqlType::Integer | SqlType::BigInt)
            {
                full_row[i] = Value::Integer(next_rowid as i64);
            }
        }

        // Run all constraint checks (NOT NULL, type, VARCHAR length, UNIQUE, FK).
        check_insert_constraints(txn, catalog, &schema, &full_row)?;

        let rowid = next_rowid;
        next_rowid += 1;

        let encoded = encode_row(&col_types, &full_row)?;
        data_table.insert(rowid, encoded.as_slice())?;

        // Update indexes.
        update_indexes_insert(txn, table_name, &indexes, &full_row, rowid)?;

        count += 1;
    }

    drop(data_table);

    // Update next_rowid in catalog.
    let schema_mut = catalog
        .get_table_mut(table_name)
        .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?;
    schema_mut.next_rowid = next_rowid;
    let updated = schema_mut.clone();
    persist::save_table(txn, &updated)?;

    Ok(count)
}

// ---------------------------------------------------------------------------
// DML: UPDATE
// ---------------------------------------------------------------------------

fn execute_update(
    txn: &manifold::WriteTransaction,
    catalog: &mut Catalog,
    table_name: &str,
    assignments: &[(usize, ScalarExpr)],
    input: &LogicalPlan,
    params: &[Value],
) -> Result<u64> {
    let schema = catalog
        .get_table(table_name)
        .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?
        .clone();
    let col_types = schema.column_types();

    // Build input executor (scan + filter) to get matching rows.
    // Rows include rowid as first column.
    let mut input_exec = build_write_query_executor(txn, catalog, input, params)?;

    let mut rows_to_update = Vec::new();
    while let Some(row) = input_exec.next()? {
        rows_to_update.push(row);
    }
    drop(input_exec);

    let data_name = data_table_name(table_name);
    let def = TableDefinition::<u64, &[u8]>::new(data_name);
    let mut data_table = txn.open_table(def)?;

    let indexes: Vec<IndexDef> = catalog
        .indexes_for_table(schema.id)
        .into_iter()
        .cloned()
        .collect();

    let mut count = 0u64;

    for row in &rows_to_update {
        // row[0] is the rowid; row[1..] are the column values.
        let rowid = match &row[0] {
            Value::Integer(id) => *id as u64,
            _ => {
                return Err(SqlError::Internal("expected rowid as first column".into()));
            }
        };

        let old_values = &row[1..];

        // Apply assignments.
        let mut new_values: Vec<Value> = old_values.to_vec();
        for (col_idx, expr) in assignments {
            // Shift column refs by 1 because row includes rowid at position 0.
            let shifted = shift_column_refs(expr, 1);
            let val = evaluate(&shifted, row, params)?;
            new_values[*col_idx] = val;
        }

        // Run constraint checks on the new row values.
        check_insert_constraints(txn, catalog, &schema, &new_values)?;

        // Remove old index entries, add new ones.
        update_indexes_delete(txn, table_name, &indexes, old_values, rowid)?;
        update_indexes_insert(txn, table_name, &indexes, &new_values, rowid)?;

        let encoded = encode_row(&col_types, &new_values)?;
        data_table.insert(rowid, encoded.as_slice())?;

        count += 1;
    }

    drop(data_table);
    Ok(count)
}

// ---------------------------------------------------------------------------
// DML: DELETE
// ---------------------------------------------------------------------------

fn execute_delete(
    txn: &manifold::WriteTransaction,
    catalog: &mut Catalog,
    table_name: &str,
    input: &LogicalPlan,
    params: &[Value],
) -> Result<u64> {
    let schema = catalog
        .get_table(table_name)
        .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?
        .clone();

    // Build input executor to get matching rows.
    let mut input_exec = build_write_query_executor(txn, catalog, input, params)?;

    let mut rows_to_delete = Vec::new();
    while let Some(row) = input_exec.next()? {
        rows_to_delete.push(row);
    }
    drop(input_exec);

    let data_name = data_table_name(table_name);
    let def = TableDefinition::<u64, &[u8]>::new(data_name);
    let mut data_table = txn.open_table(def)?;

    let indexes: Vec<IndexDef> = catalog
        .indexes_for_table(schema.id)
        .into_iter()
        .cloned()
        .collect();

    let mut count = 0u64;

    for row in &rows_to_delete {
        let rowid = match &row[0] {
            Value::Integer(id) => *id as u64,
            _ => {
                return Err(SqlError::Internal("expected rowid as first column".into()));
            }
        };

        let values = &row[1..];

        // Check FK constraints: other tables referencing this row.
        check_delete_constraints(txn, catalog, table_name, values)?;

        // Remove index entries.
        update_indexes_delete(txn, table_name, &indexes, values, rowid)?;

        data_table.remove(rowid)?;
        count += 1;
    }

    drop(data_table);
    Ok(count)
}

// ---------------------------------------------------------------------------
// Constraint checking
// ---------------------------------------------------------------------------

/// Check constraints for INSERT and UPDATE operations.
///
/// Validates NOT NULL, type compatibility, VARCHAR length, and FK (child-side).
/// UNIQUE is checked via index insertion in `update_indexes_insert`.
fn check_insert_constraints(
    txn: &manifold::WriteTransaction,
    catalog: &Catalog,
    schema: &TableSchema,
    row: &[Value],
) -> Result<()> {
    // 1. NOT NULL constraints (explicit ConstraintDef).
    for constraint in &schema.constraints {
        if let ConstraintDef::NotNull { column, name, .. } = constraint
            && row[*column].is_null()
        {
            return Err(SqlError::ConstraintViolation(format!(
                "NOT NULL constraint '{name}' violated on column '{}'",
                schema.columns[*column].name
            )));
        }
    }

    // 2. Column-level `nullable: false` (from is_primary_key, etc.).
    for (i, col) in schema.columns.iter().enumerate() {
        if !col.nullable && row[i].is_null() {
            return Err(SqlError::ConstraintViolation(format!(
                "column '{}' cannot be null",
                col.name
            )));
        }
    }

    // 3. Type enforcement — check value is compatible with declared column type.
    for (i, col) in schema.columns.iter().enumerate() {
        if !row[i].is_compatible_with(&col.sql_type) {
            return Err(SqlError::TypeError(format!(
                "column '{}' has type {} but got value {}",
                col.name, col.sql_type, row[i]
            )));
        }

        // 3b. Range checking for narrowing integer conversions.
        if let SqlType::SmallInt = &col.sql_type {
            if let Value::Integer(v) = &row[i] {
                if *v < i16::MIN as i64 || *v > i16::MAX as i64 {
                    return Err(SqlError::TypeError(format!(
                        "value {} out of range for column '{}' of type SMALLINT (range {}..{})",
                        v, col.name, i16::MIN, i16::MAX
                    )));
                }
            }
        }
    }

    // 4. VARCHAR length check.
    for (i, col) in schema.columns.iter().enumerate() {
        if let SqlType::Varchar(max_len) = &col.sql_type
            && let Value::Text(s) = &row[i]
            && s.len() > *max_len as usize
        {
            return Err(SqlError::ConstraintViolation(format!(
                "value for column '{}' exceeds VARCHAR({}) limit (got {} chars)",
                col.name, max_len, s.len()
            )));
        }
    }

    // 5. FK constraints (child side): verify referenced rows exist in parent table.
    for constraint in &schema.constraints {
        if let ConstraintDef::ForeignKey {
            name,
            columns,
            ref_table,
            ref_columns,
            ..
        } = constraint
        {
            check_fk_parent_exists(txn, catalog, name, schema, row, columns, ref_table, ref_columns)?;
        }
    }

    Ok(())
}

/// Verify that the referenced parent row exists for a FK constraint.
#[allow(clippy::too_many_arguments)]
fn check_fk_parent_exists(
    txn: &manifold::WriteTransaction,
    catalog: &Catalog,
    fk_name: &str,
    _child_schema: &TableSchema,
    child_row: &[Value],
    child_cols: &[usize],
    ref_table_name: &str,
    ref_col_names: &[String],
) -> Result<()> {
    // If all FK columns are NULL, skip (NULLs satisfy FK constraint).
    if child_cols.iter().all(|&i| child_row[i].is_null()) {
        return Ok(());
    }

    let parent_schema = catalog
        .get_table(ref_table_name)
        .ok_or_else(|| SqlError::ConstraintViolation(format!(
            "FK '{fk_name}': referenced table '{ref_table_name}' not found"
        )))?;

    let parent_col_indices: Vec<usize> = ref_col_names
        .iter()
        .map(|name| {
            parent_schema.column_index(name).ok_or_else(|| {
                SqlError::ConstraintViolation(format!(
                    "FK '{fk_name}': referenced column '{name}' not found in '{ref_table_name}'"
                ))
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let child_values: Vec<Value> = child_cols.iter().map(|&i| child_row[i].clone()).collect();

    // Try to use a unique index on the parent table for efficient lookup.
    let parent_indexes: Vec<IndexDef> = catalog
        .indexes_for_table(parent_schema.id)
        .into_iter()
        .cloned()
        .collect();

    // Check if there's a unique index on exactly the referenced columns.
    let matching_index = parent_indexes.iter().find(|idx| {
        idx.unique && idx.columns == parent_col_indices
    });

    if let Some(idx) = matching_index {
        let key_bytes = encode_index_key(&child_values);
        let idx_name = index_table_name(ref_table_name, &idx.name, true);
        let idx_def = TableDefinition::<&[u8], u64>::new(idx_name);
        let idx_table = txn.open_table(idx_def)?;
        if idx_table
            .get(key_bytes.as_slice())
            .map_err(SqlError::Storage)?
            .is_none()
        {
            return Err(SqlError::ConstraintViolation(format!(
                "FK constraint '{fk_name}' violated: no matching row in '{ref_table_name}'"
            )));
        }
    } else {
        // Fallback: scan the parent data table.
        let data_name = data_table_name(ref_table_name);
        let def = TableDefinition::<u64, &[u8]>::new(data_name);
        let parent_table = txn.open_table(def)?;
        let parent_types = parent_schema.column_types();
        let mut found = false;
        let iter = parent_table.iter().map_err(SqlError::Storage)?;
        for entry in iter {
            let (_, v) = entry.map_err(SqlError::Storage)?;
            let parent_row = decode_row(&parent_types, v.value())?;
            let matches = parent_col_indices
                .iter()
                .zip(child_values.iter())
                .all(|(&pi, cv)| parent_row[pi] == *cv);
            if matches {
                found = true;
                break;
            }
        }
        if !found {
            return Err(SqlError::ConstraintViolation(format!(
                "FK constraint '{fk_name}' violated: no matching row in '{ref_table_name}'"
            )));
        }
    }

    Ok(())
}

/// Check FK constraints when deleting a row from a parent table.
///
/// Scans all tables in the catalog for FK constraints referencing this table,
/// and enforces the appropriate action (RESTRICT, CASCADE, SET NULL).
fn check_delete_constraints(
    txn: &manifold::WriteTransaction,
    catalog: &Catalog,
    parent_table_name: &str,
    parent_row: &[Value],
) -> Result<()> {
    let parent_schema = match catalog.get_table(parent_table_name) {
        Some(s) => s.clone(),
        None => return Ok(()),
    };

    // Find all FK constraints in other tables that reference this table.
    let all_table_names: Vec<String> = catalog.table_names();
    for child_table_name in &all_table_names {
        let child_schema = match catalog.get_table(child_table_name) {
            Some(s) => s.clone(),
            None => continue,
        };

        for constraint in &child_schema.constraints {
            if let ConstraintDef::ForeignKey {
                name,
                columns: child_cols,
                ref_table,
                ref_columns,
                on_delete,
                ..
            } = constraint
            {
                if ref_table != parent_table_name {
                    continue;
                }

                // Resolve referenced column indices in the parent.
                let parent_col_indices: Vec<usize> = match ref_columns
                    .iter()
                    .map(|n| parent_schema.column_index(n))
                    .collect::<Option<Vec<_>>>()
                {
                    Some(v) => v,
                    None => continue,
                };

                // Extract the parent values for the referenced columns.
                let parent_key: Vec<Value> = parent_col_indices
                    .iter()
                    .map(|&i| parent_row[i].clone())
                    .collect();

                // Scan the child table for matching rows.
                let child_data_name = data_table_name(child_table_name);
                let child_def = TableDefinition::<u64, &[u8]>::new(child_data_name);
                let child_types = child_schema.column_types();

                match on_delete {
                    crate::catalog::schema::ForeignKeyAction::Restrict
                    | crate::catalog::schema::ForeignKeyAction::NoAction => {
                        let child_table = txn.open_table(child_def)?;
                        let iter = child_table.iter().map_err(SqlError::Storage)?;
                        for entry in iter {
                            let (_, v) = entry.map_err(SqlError::Storage)?;
                            let child_row = decode_row(&child_types, v.value())?;
                            let matches = child_cols
                                .iter()
                                .zip(parent_key.iter())
                                .all(|(&ci, pk)| child_row[ci] == *pk);
                            if matches {
                                return Err(SqlError::ConstraintViolation(format!(
                                    "FK constraint '{name}' on '{child_table_name}' restricts delete from '{parent_table_name}'"
                                )));
                            }
                        }
                    }
                    crate::catalog::schema::ForeignKeyAction::Cascade => {
                        // Delete all child rows that reference this parent row.
                        let mut child_table = txn.open_table(child_def)?;
                        let mut to_delete = Vec::new();
                        {
                            let iter = child_table.iter().map_err(SqlError::Storage)?;
                            for entry in iter {
                                let (k, v) = entry.map_err(SqlError::Storage)?;
                                let rowid = k.value();
                                let child_row = decode_row(&child_types, v.value())?;
                                let matches = child_cols
                                    .iter()
                                    .zip(parent_key.iter())
                                    .all(|(&ci, pk)| child_row[ci] == *pk);
                                if matches {
                                    to_delete.push((rowid, child_row));
                                }
                            }
                        }

                        let child_indexes: Vec<IndexDef> = catalog
                            .indexes_for_table(child_schema.id)
                            .into_iter()
                            .cloned()
                            .collect();

                        for (rowid, child_row) in &to_delete {
                            update_indexes_delete(
                                txn,
                                child_table_name,
                                &child_indexes,
                                child_row,
                                *rowid,
                            )?;
                            child_table.remove(*rowid)?;
                        }
                    }
                    crate::catalog::schema::ForeignKeyAction::SetNull => {
                        // Set FK columns to NULL in child rows.
                        let mut child_table = txn.open_table(child_def)?;
                        let mut to_update = Vec::new();
                        {
                            let iter = child_table.iter().map_err(SqlError::Storage)?;
                            for entry in iter {
                                let (k, v) = entry.map_err(SqlError::Storage)?;
                                let rowid = k.value();
                                let child_row = decode_row(&child_types, v.value())?;
                                let matches = child_cols
                                    .iter()
                                    .zip(parent_key.iter())
                                    .all(|(&ci, pk)| child_row[ci] == *pk);
                                if matches {
                                    to_update.push((rowid, child_row));
                                }
                            }
                        }

                        let child_indexes: Vec<IndexDef> = catalog
                            .indexes_for_table(child_schema.id)
                            .into_iter()
                            .cloned()
                            .collect();

                        for (rowid, old_child_row) in &to_update {
                            update_indexes_delete(
                                txn,
                                child_table_name,
                                &child_indexes,
                                old_child_row,
                                *rowid,
                            )?;
                            let mut new_child_row = old_child_row.clone();
                            for &ci in child_cols {
                                new_child_row[ci] = Value::Null;
                            }
                            update_indexes_insert(
                                txn,
                                child_table_name,
                                &child_indexes,
                                &new_child_row,
                                *rowid,
                            )?;
                            let encoded = encode_row(&child_types, &new_child_row)?;
                            child_table.insert(*rowid, encoded.as_slice())?;
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// ANALYZE
// ---------------------------------------------------------------------------

/// Execute an ANALYZE statement.
///
/// For each table being analyzed, count rows and gather cardinality estimates
/// for indexed columns, then persist the result to `_statistics`.
fn execute_analyze(
    txn: &manifold::WriteTransaction,
    catalog: &Catalog,
    table_name: Option<&str>,
) -> Result<u64> {
    let names: Vec<String> = if let Some(name) = table_name {
        if !catalog.has_table(name) {
            return Err(SqlError::TableNotFound(name.to_string()));
        }
        vec![name.to_string()]
    } else {
        catalog.table_names()
    };

    let mut stats_table = txn.open_table(STATISTICS_TABLE)?;

    for name in &names {
        let schema = match catalog.get_table(name) {
            Some(s) => s.clone(),
            None => continue,
        };

        let col_types = schema.column_types();

        // Count rows and collect per-column distinct values.
        let data_name = data_table_name(name);
        let data_def = TableDefinition::<u64, &[u8]>::new(data_name);
        let data_table = txn.open_table(data_def)?;

        let indexes: Vec<IndexDef> = catalog
            .indexes_for_table(schema.id)
            .into_iter()
            .cloned()
            .collect();

        // Track distinct values per index column set using encoded key bytes.
        let mut distinct_sets: Vec<std::collections::HashSet<Vec<u8>>> =
            indexes.iter().map(|_| std::collections::HashSet::new()).collect();

        let mut row_count: u64 = 0;

        let iter = data_table.iter().map_err(SqlError::Storage)?;
        for entry in iter {
            let (_, v) = entry.map_err(SqlError::Storage)?;
            let row = decode_row(&col_types, v.value())?;
            row_count += 1;

            for (i, idx) in indexes.iter().enumerate() {
                let key_vals: Vec<Value> = idx.columns.iter().map(|&ci| row[ci].clone()).collect();
                let key_bytes = encode_index_key(&key_vals);
                distinct_sets[i].insert(key_bytes);
            }
        }

        let index_stats: Vec<IndexStatistics> = indexes
            .iter()
            .zip(distinct_sets.iter())
            .map(|(idx, distinct)| IndexStatistics {
                name: idx.name.clone(),
                cardinality: distinct.len() as u64,
            })
            .collect();

        let table_stats = TableStatistics {
            row_count,
            indexes: index_stats,
        };

        let json = serde_json::to_vec(&table_stats).map_err(|e| {
            SqlError::Internal(format!("failed to serialize statistics for '{name}': {e}"))
        })?;

        stats_table.insert(schema.id, json.as_slice())?;
    }

    Ok(0)
}

// ---------------------------------------------------------------------------
// Index update helpers
// ---------------------------------------------------------------------------

fn update_indexes_insert(
    txn: &manifold::WriteTransaction,
    table_name: &str,
    indexes: &[IndexDef],
    row: &[Value],
    rowid: u64,
) -> Result<()> {
    for index in indexes {
        let key_values: Vec<Value> = index.columns.iter().map(|&i| row[i].clone()).collect();

        // Per SQL standard, NULLs are not considered equal for UNIQUE constraints.
        // If any column in a unique index key is NULL, skip the uniqueness check
        // (but still store the entry so we can clean it up on delete).
        let has_null = key_values.iter().any(|v| v.is_null());

        let key_bytes = encode_index_key(&key_values);

        let idx_name = index_table_name(table_name, &index.name, index.unique);

        if index.unique {
            let idx_def = TableDefinition::<&[u8], u64>::new(idx_name);
            let mut idx_table = txn.open_table(idx_def)?;

            // Check for uniqueness violation only when no key column is NULL.
            if !has_null {
                if let Some(existing) = idx_table
                    .get(key_bytes.as_slice())
                    .map_err(SqlError::Storage)?
                    && existing.value() != rowid
                {
                    return Err(SqlError::ConstraintViolation(format!(
                        "duplicate key in unique index '{}'",
                        index.name
                    )));
                }
            }

            idx_table.insert(key_bytes.as_slice(), rowid)?;
        } else {
            let idx_def = MultimapTableDefinition::<&[u8], u64>::new(idx_name);
            let mut idx_table = txn.open_multimap_table(idx_def)?;
            idx_table.insert(key_bytes.as_slice(), rowid)?;
        }
    }
    Ok(())
}

fn update_indexes_delete(
    txn: &manifold::WriteTransaction,
    table_name: &str,
    indexes: &[IndexDef],
    row: &[Value],
    rowid: u64,
) -> Result<()> {
    for index in indexes {
        let key_values: Vec<Value> = index.columns.iter().map(|&i| row[i].clone()).collect();
        let key_bytes = encode_index_key(&key_values);

        let idx_name = index_table_name(table_name, &index.name, index.unique);

        if index.unique {
            let idx_def = TableDefinition::<&[u8], u64>::new(idx_name);
            let mut idx_table = txn.open_table(idx_def)?;
            idx_table.remove(key_bytes.as_slice())?;
        } else {
            let idx_def = MultimapTableDefinition::<&[u8], u64>::new(idx_name);
            let mut idx_table = txn.open_multimap_table(idx_def)?;
            idx_table.remove(key_bytes.as_slice(), rowid)?;
        }
    }
    Ok(())
}
