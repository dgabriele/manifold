use crate::binder::{BinaryOp, UnaryOp};
use crate::error::Result;
use crate::planner::plan::{LogicalPlan, ScalarExpr};
use crate::types::Value;

/// Walk the plan tree and fold any `ScalarExpr` that contains only literals
/// into a single `Literal` value.
pub fn fold(plan: LogicalPlan) -> Result<LogicalPlan> {
    fold_plan(plan)
}

fn fold_plan(plan: LogicalPlan) -> Result<LogicalPlan> {
    match plan {
        LogicalPlan::Filter { predicate, input } => {
            let input = fold_plan(*input)?;
            let predicate = fold_expr(predicate);
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
            let input = fold_plan(*input)?;
            let expressions = expressions.into_iter().map(fold_expr).collect();
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
            let left = fold_plan(*left)?;
            let right = fold_plan(*right)?;
            let condition = condition.map(fold_expr);
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
            let input = fold_plan(*input)?;
            let group_by = group_by.into_iter().map(fold_expr).collect();
            Ok(LogicalPlan::Aggregate {
                group_by,
                aggregates,
                schema,
                input: Box::new(input),
            })
        }
        LogicalPlan::Sort { order_by, input } => {
            let input = fold_plan(*input)?;
            let order_by = order_by
                .into_iter()
                .map(|mut o| {
                    o.expr = fold_expr(o.expr);
                    o
                })
                .collect();
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
            let input = fold_plan(*input)?;
            let count = count.map(fold_expr);
            let offset = offset.map(fold_expr);
            Ok(LogicalPlan::Limit {
                count,
                offset,
                input: Box::new(input),
            })
        }
        LogicalPlan::Distinct { input } => {
            let input = fold_plan(*input)?;
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
            let left = fold_plan(*left)?;
            let right = fold_plan(*right)?;
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
            let source = fold_plan(*source)?;
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
            let input = fold_plan(*input)?;
            let assignments = assignments
                .into_iter()
                .map(|(idx, expr)| (idx, fold_expr(expr)))
                .collect();
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
            let input = fold_plan(*input)?;
            Ok(LogicalPlan::Delete {
                table_id,
                table_name,
                input: Box::new(input),
            })
        }
        LogicalPlan::Values { rows, schema } => {
            let rows = rows
                .into_iter()
                .map(|row| row.into_iter().map(fold_expr).collect())
                .collect();
            Ok(LogicalPlan::Values { rows, schema })
        }
        LogicalPlan::Explain { input } => {
            let input = fold_plan(*input)?;
            Ok(LogicalPlan::Explain {
                input: Box::new(input),
            })
        }
        // Leaf nodes and DDL -- no expressions to fold
        other => Ok(other),
    }
}

/// Attempt to fold a scalar expression. First recurse into children, then
/// try to evaluate the result if all children are now literals.
fn fold_expr(expr: ScalarExpr) -> ScalarExpr {
    match expr {
        ScalarExpr::BinaryOp { op, left, right } => {
            let left = fold_expr(*left);
            let right = fold_expr(*right);
            if let (ScalarExpr::Literal(lv), ScalarExpr::Literal(rv)) = (&left, &right)
                && let Some(result) = eval_binary(&op, lv, rv)
            {
                return ScalarExpr::Literal(result);
            }
            ScalarExpr::BinaryOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }
        }
        ScalarExpr::UnaryOp { op, operand } => {
            let operand = fold_expr(*operand);
            if let ScalarExpr::Literal(ref v) = operand
                && let Some(result) = eval_unary(&op, v)
            {
                return ScalarExpr::Literal(result);
            }
            ScalarExpr::UnaryOp {
                op,
                operand: Box::new(operand),
            }
        }
        ScalarExpr::IsNull { operand, negated } => {
            let operand = fold_expr(*operand);
            if let ScalarExpr::Literal(ref v) = operand {
                let is_null = v.is_null();
                let result = if negated { !is_null } else { is_null };
                return ScalarExpr::Literal(Value::Boolean(result));
            }
            ScalarExpr::IsNull {
                operand: Box::new(operand),
                negated,
            }
        }
        ScalarExpr::Cast { expr, target_type } => {
            let expr = fold_expr(*expr);
            ScalarExpr::Cast {
                expr: Box::new(expr),
                target_type,
            }
        }
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => {
            let expr = fold_expr(*expr);
            let list: Vec<_> = list.into_iter().map(fold_expr).collect();
            ScalarExpr::InList {
                expr: Box::new(expr),
                list,
                negated,
            }
        }
        ScalarExpr::Between {
            expr,
            low,
            high,
            negated,
        } => {
            let expr = fold_expr(*expr);
            let low = fold_expr(*low);
            let high = fold_expr(*high);
            ScalarExpr::Between {
                expr: Box::new(expr),
                low: Box::new(low),
                high: Box::new(high),
                negated,
            }
        }
        ScalarExpr::Like {
            expr,
            pattern,
            negated,
        } => {
            let expr = fold_expr(*expr);
            let pattern = fold_expr(*pattern);
            ScalarExpr::Like {
                expr: Box::new(expr),
                pattern: Box::new(pattern),
                negated,
            }
        }
        ScalarExpr::Function { name, args } => {
            let args: Vec<_> = args.into_iter().map(fold_expr).collect();
            ScalarExpr::Function { name, args }
        }
        // Subquery expressions: fold the outer expr but leave the subquery plan as-is.
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => ScalarExpr::InSubquery {
            expr: Box::new(fold_expr(*expr)),
            subquery,
            negated,
        },
        // EXISTS and ScalarSubquery have no outer expr to fold.
        ScalarExpr::Exists { .. } | ScalarExpr::ScalarSubquery { .. } => expr,
        // Leaf nodes -- nothing to fold
        other => other,
    }
}

/// Evaluate a binary operation on two literal values.
fn eval_binary(op: &BinaryOp, left: &Value, right: &Value) -> Option<Value> {
    match (op, left, right) {
        // Integer arithmetic
        (BinaryOp::Add, Value::Integer(a), Value::Integer(b)) => Some(Value::Integer(a + b)),
        (BinaryOp::Sub, Value::Integer(a), Value::Integer(b)) => Some(Value::Integer(a - b)),
        (BinaryOp::Mul, Value::Integer(a), Value::Integer(b)) => Some(Value::Integer(a * b)),
        (BinaryOp::Div, Value::Integer(a), Value::Integer(b)) if *b != 0 => {
            Some(Value::Integer(a / b))
        }
        (BinaryOp::Mod, Value::Integer(a), Value::Integer(b)) if *b != 0 => {
            Some(Value::Integer(a % b))
        }

        // Real arithmetic
        (BinaryOp::Add, Value::Real(a), Value::Real(b)) => Some(Value::Real(a + b)),
        (BinaryOp::Sub, Value::Real(a), Value::Real(b)) => Some(Value::Real(a - b)),
        (BinaryOp::Mul, Value::Real(a), Value::Real(b)) => Some(Value::Real(a * b)),
        (BinaryOp::Div, Value::Real(a), Value::Real(b)) if *b != 0.0 => {
            Some(Value::Real(a / b))
        }

        // Integer comparisons
        (BinaryOp::Eq, Value::Integer(a), Value::Integer(b)) => Some(Value::Boolean(a == b)),
        (BinaryOp::Neq, Value::Integer(a), Value::Integer(b)) => Some(Value::Boolean(a != b)),
        (BinaryOp::Lt, Value::Integer(a), Value::Integer(b)) => Some(Value::Boolean(a < b)),
        (BinaryOp::Gt, Value::Integer(a), Value::Integer(b)) => Some(Value::Boolean(a > b)),
        (BinaryOp::Lte, Value::Integer(a), Value::Integer(b)) => Some(Value::Boolean(a <= b)),
        (BinaryOp::Gte, Value::Integer(a), Value::Integer(b)) => Some(Value::Boolean(a >= b)),

        // String comparisons
        (BinaryOp::Eq, Value::Text(a), Value::Text(b)) => Some(Value::Boolean(a == b)),
        (BinaryOp::Neq, Value::Text(a), Value::Text(b)) => Some(Value::Boolean(a != b)),

        // Boolean logic
        (BinaryOp::And, Value::Boolean(a), Value::Boolean(b)) => Some(Value::Boolean(*a && *b)),
        (BinaryOp::Or, Value::Boolean(a), Value::Boolean(b)) => Some(Value::Boolean(*a || *b)),

        // String concatenation
        (BinaryOp::Add, Value::Text(a), Value::Text(b)) => {
            Some(Value::Text(format!("{}{}", a, b)))
        }

        _ => None,
    }
}

/// Evaluate a unary operation on a literal value.
fn eval_unary(op: &UnaryOp, val: &Value) -> Option<Value> {
    match (op, val) {
        (UnaryOp::Neg, Value::Integer(v)) => Some(Value::Integer(-v)),
        (UnaryOp::Neg, Value::Real(v)) => Some(Value::Real(-v)),
        (UnaryOp::Not, Value::Boolean(v)) => Some(Value::Boolean(!v)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::plan::PlanSchema;

    #[test]
    fn fold_integer_addition() {
        let expr = ScalarExpr::BinaryOp {
            op: BinaryOp::Add,
            left: Box::new(ScalarExpr::Literal(Value::Integer(1))),
            right: Box::new(ScalarExpr::Literal(Value::Integer(2))),
        };
        let plan = LogicalPlan::Project {
            expressions: vec![expr],
            aliases: vec!["result".to_string()],
            schema: PlanSchema::empty(),
            input: Box::new(LogicalPlan::Empty),
        };
        let folded = fold(plan).unwrap();
        if let LogicalPlan::Project { expressions, .. } = folded {
            assert!(matches!(
                expressions[0],
                ScalarExpr::Literal(Value::Integer(3))
            ));
        } else {
            panic!("expected Project");
        }
    }

    #[test]
    fn fold_boolean_and() {
        let expr = ScalarExpr::BinaryOp {
            op: BinaryOp::And,
            left: Box::new(ScalarExpr::Literal(Value::Boolean(true))),
            right: Box::new(ScalarExpr::Literal(Value::Boolean(true))),
        };
        let result = fold_expr(expr);
        assert!(matches!(result, ScalarExpr::Literal(Value::Boolean(true))));
    }

    #[test]
    fn fold_nested_arithmetic() {
        // (2 * 3) + 1 => 7
        let expr = ScalarExpr::BinaryOp {
            op: BinaryOp::Add,
            left: Box::new(ScalarExpr::BinaryOp {
                op: BinaryOp::Mul,
                left: Box::new(ScalarExpr::Literal(Value::Integer(2))),
                right: Box::new(ScalarExpr::Literal(Value::Integer(3))),
            }),
            right: Box::new(ScalarExpr::Literal(Value::Integer(1))),
        };
        let result = fold_expr(expr);
        assert!(matches!(result, ScalarExpr::Literal(Value::Integer(7))));
    }

    #[test]
    fn fold_preserves_column_ref() {
        // col + 1 should not be folded
        let expr = ScalarExpr::BinaryOp {
            op: BinaryOp::Add,
            left: Box::new(ScalarExpr::ColumnRef { index: 0 }),
            right: Box::new(ScalarExpr::Literal(Value::Integer(1))),
        };
        let result = fold_expr(expr);
        assert!(matches!(result, ScalarExpr::BinaryOp { .. }));
    }

    #[test]
    fn fold_unary_neg() {
        let expr = ScalarExpr::UnaryOp {
            op: UnaryOp::Neg,
            operand: Box::new(ScalarExpr::Literal(Value::Integer(5))),
        };
        let result = fold_expr(expr);
        assert!(matches!(result, ScalarExpr::Literal(Value::Integer(-5))));
    }

    #[test]
    fn fold_is_null() {
        let expr = ScalarExpr::IsNull {
            operand: Box::new(ScalarExpr::Literal(Value::Null)),
            negated: false,
        };
        let result = fold_expr(expr);
        assert!(matches!(result, ScalarExpr::Literal(Value::Boolean(true))));
    }
}
