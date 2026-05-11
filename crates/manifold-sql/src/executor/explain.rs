//! EXPLAIN plan formatter.
//!
//! Produces a human-readable, indented tree representation of a `LogicalPlan`.

use crate::binder::JoinType;
use crate::planner::plan::LogicalPlan;

/// Format a `LogicalPlan` as a human-readable tree string.
pub fn format_plan(plan: &LogicalPlan) -> String {
    let mut out = String::new();
    format_node(plan, 0, &mut out);
    // Trim trailing newline for cleanliness.
    out.trim_end_matches('\n').to_string()
}

fn indent(depth: usize) -> String {
    "  ".repeat(depth)
}

fn format_node(plan: &LogicalPlan, depth: usize, out: &mut String) {
    let pfx = indent(depth);
    match plan {
        LogicalPlan::Scan { table_name, .. } => {
            out.push_str(&format!("{pfx}Scan {table_name}\n"));
        }
        LogicalPlan::IndexScan {
            table_name,
            index_name,
            lookup_values,
            ..
        } => {
            let vals: Vec<String> = lookup_values.iter().map(format_expr_brief).collect();
            out.push_str(&format!(
                "{pfx}IndexScan {table_name} using {index_name} [{}]\n",
                vals.join(", ")
            ));
        }
        LogicalPlan::Filter { predicate, input } => {
            out.push_str(&format!("{pfx}Filter ({})\n", format_expr_brief(predicate)));
            format_node(input, depth + 1, out);
        }
        LogicalPlan::Project {
            aliases,
            expressions,
            input,
            ..
        } => {
            let cols = if !aliases.is_empty() {
                aliases.join(", ")
            } else {
                expressions
                    .iter()
                    .map(format_expr_brief)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            out.push_str(&format!("{pfx}Project [{cols}]\n"));
            format_node(input, depth + 1, out);
        }
        LogicalPlan::Join {
            join_type,
            left,
            right,
            condition,
            ..
        } => {
            let jt = match join_type {
                JoinType::Inner => "INNER",
                JoinType::Left => "LEFT",
                JoinType::Right => "RIGHT",
                JoinType::Cross => "CROSS",
            };
            if let Some(cond) = condition {
                out.push_str(&format!(
                    "{pfx}Join {jt} on {}\n",
                    format_expr_brief(cond)
                ));
            } else {
                out.push_str(&format!("{pfx}Join {jt}\n"));
            }
            format_node(left, depth + 1, out);
            format_node(right, depth + 1, out);
        }
        LogicalPlan::Aggregate {
            group_by,
            aggregates,
            input,
            ..
        } => {
            let agg_strs: Vec<String> = aggregates
                .iter()
                .map(|a| {
                    let func = format!("{:?}", a.func);
                    if let Some(arg) = &a.arg {
                        format!("{}({})", func, format_expr_brief(arg))
                    } else {
                        format!("{func}(*)")
                    }
                })
                .collect();
            if group_by.is_empty() {
                out.push_str(&format!("{pfx}Aggregate [{}]\n", agg_strs.join(", ")));
            } else {
                let gb_strs: Vec<String> = group_by.iter().map(format_expr_brief).collect();
                out.push_str(&format!(
                    "{pfx}Aggregate [{}] group by [{}]\n",
                    agg_strs.join(", "),
                    gb_strs.join(", ")
                ));
            }
            format_node(input, depth + 1, out);
        }
        LogicalPlan::Sort { order_by, input } => {
            let obs: Vec<String> = order_by
                .iter()
                .map(|ob| {
                    let dir = if ob.asc { "ASC" } else { "DESC" };
                    format!("{} {dir}", format_expr_brief(&ob.expr))
                })
                .collect();
            out.push_str(&format!("{pfx}Sort [{}]\n", obs.join(", ")));
            format_node(input, depth + 1, out);
        }
        LogicalPlan::Limit {
            count,
            offset,
            input,
        } => {
            let cnt = count
                .as_ref()
                .map(format_expr_brief)
                .unwrap_or_else(|| "ALL".to_string());
            let off = offset
                .as_ref()
                .map(|e| format!(" offset {}", format_expr_brief(e)))
                .unwrap_or_default();
            out.push_str(&format!("{pfx}Limit {cnt}{off}\n"));
            format_node(input, depth + 1, out);
        }
        LogicalPlan::Distinct { input } => {
            out.push_str(&format!("{pfx}Distinct\n"));
            format_node(input, depth + 1, out);
        }
        LogicalPlan::Union { left, right, all, .. } => {
            let kind = if *all { "UNION ALL" } else { "UNION" };
            out.push_str(&format!("{pfx}{kind}\n"));
            format_node(left, depth + 1, out);
            format_node(right, depth + 1, out);
        }
        LogicalPlan::Values { rows, .. } => {
            out.push_str(&format!("{pfx}Values ({} rows)\n", rows.len()));
        }
        LogicalPlan::Insert { table_name, .. } => {
            out.push_str(&format!("{pfx}Insert {table_name}\n"));
        }
        LogicalPlan::Update { table_name, .. } => {
            out.push_str(&format!("{pfx}Update {table_name}\n"));
        }
        LogicalPlan::Delete { table_name, .. } => {
            out.push_str(&format!("{pfx}Delete {table_name}\n"));
        }
        LogicalPlan::CreateTable { name, .. } => {
            out.push_str(&format!("{pfx}CreateTable {name}\n"));
        }
        LogicalPlan::DropTable { name, .. } => {
            out.push_str(&format!("{pfx}DropTable {name}\n"));
        }
        LogicalPlan::AlterTable { table_name, .. } => {
            out.push_str(&format!("{pfx}AlterTable {table_name}\n"));
        }
        LogicalPlan::CreateIndex {
            index_name,
            table_name,
            ..
        } => {
            out.push_str(&format!("{pfx}CreateIndex {index_name} on {table_name}\n"));
        }
        LogicalPlan::DropIndex { name, .. } => {
            out.push_str(&format!("{pfx}DropIndex {name}\n"));
        }
        LogicalPlan::Explain { input } => {
            out.push_str(&format!("{pfx}Explain\n"));
            format_node(input, depth + 1, out);
        }
        LogicalPlan::Analyze { table_name } => {
            if let Some(name) = table_name {
                out.push_str(&format!("{pfx}Analyze {name}\n"));
            } else {
                out.push_str(&format!("{pfx}Analyze (all tables)\n"));
            }
        }
        LogicalPlan::Empty => {
            out.push_str(&format!("{pfx}Empty\n"));
        }
        LogicalPlan::BeginTransaction => {
            out.push_str(&format!("{pfx}BeginTransaction\n"));
        }
        LogicalPlan::CommitTransaction => {
            out.push_str(&format!("{pfx}CommitTransaction\n"));
        }
        LogicalPlan::RollbackTransaction => {
            out.push_str(&format!("{pfx}RollbackTransaction\n"));
        }
        LogicalPlan::Savepoint { name } => {
            out.push_str(&format!("{pfx}Savepoint {name}\n"));
        }
        LogicalPlan::ReleaseSavepoint { name } => {
            out.push_str(&format!("{pfx}ReleaseSavepoint {name}\n"));
        }
        LogicalPlan::RollbackToSavepoint { name } => {
            out.push_str(&format!("{pfx}RollbackToSavepoint {name}\n"));
        }
    }
}

/// Produce a compact single-line representation of a scalar expression for
/// display in EXPLAIN output.  We intentionally keep this brief.
fn format_expr_brief(expr: &crate::planner::plan::ScalarExpr) -> String {
    use crate::planner::plan::ScalarExpr;
    match expr {
        ScalarExpr::ColumnRef { index } => format!("col#{index}"),
        ScalarExpr::Literal(v) => format!("{v}"),
        ScalarExpr::Parameter(i) => format!("?{i}"),
        ScalarExpr::BinaryOp { op, left, right } => format!(
            "({} {op:?} {})",
            format_expr_brief(left),
            format_expr_brief(right)
        ),
        ScalarExpr::UnaryOp { op, operand } => {
            format!("({op:?} {})", format_expr_brief(operand))
        }
        ScalarExpr::IsNull { operand, negated } => {
            let suffix = if *negated { "IS NOT NULL" } else { "IS NULL" };
            format!("({} {suffix})", format_expr_brief(operand))
        }
        ScalarExpr::InList {
            expr,
            list: _,
            negated,
        } => {
            let kw = if *negated { "NOT IN" } else { "IN" };
            format!("({} {kw} [...])", format_expr_brief(expr))
        }
        ScalarExpr::Between {
            expr,
            low,
            high,
            negated,
        } => {
            let kw = if *negated { "NOT BETWEEN" } else { "BETWEEN" };
            format!(
                "({} {kw} {} AND {})",
                format_expr_brief(expr),
                format_expr_brief(low),
                format_expr_brief(high)
            )
        }
        ScalarExpr::Like {
            expr,
            pattern,
            negated,
        } => {
            let kw = if *negated { "NOT LIKE" } else { "LIKE" };
            format!("({} {kw} {})", format_expr_brief(expr), format_expr_brief(pattern))
        }
        ScalarExpr::Function { name, args } => {
            let arg_str = args
                .iter()
                .map(format_expr_brief)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{name}({arg_str})")
        }
        ScalarExpr::Cast { expr, target_type } => {
            format!("CAST({} AS {target_type})", format_expr_brief(expr))
        }
        ScalarExpr::InSubquery { expr, negated, .. } => {
            let kw = if *negated { "NOT IN" } else { "IN" };
            format!("({} {kw} (SELECT ...))", format_expr_brief(expr))
        }
        ScalarExpr::Exists { negated, .. } => {
            let kw = if *negated { "NOT EXISTS" } else { "EXISTS" };
            format!("{kw} (SELECT ...)")
        }
        ScalarExpr::ScalarSubquery { .. } => "(SELECT ...)".to_string(),
    }
}
