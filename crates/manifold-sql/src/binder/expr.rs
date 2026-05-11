use sqlparser::ast::{self, DataType, DuplicateTreatment, FunctionArg, FunctionArgExpr, FunctionArguments};

use crate::catalog::schema::TableId;
use crate::catalog::Catalog;
use crate::error::{Result, SqlError};
use crate::types::{SqlType, Value};

use super::{AggregateFunc, BinaryOp, BoundExpr, ColumnRef, UnaryOp};

// ---------------------------------------------------------------------------
// Scope — tracks tables and columns visible in the current query context
// ---------------------------------------------------------------------------

struct ScopeTable {
    table_id: TableId,
    table_name: String,
    alias: Option<String>,
    columns: Vec<ScopeColumn>,
}

struct ScopeColumn {
    name: String,
    sql_type: SqlType,
    nullable: bool,
}

pub struct Scope {
    tables: Vec<ScopeTable>,
}

impl Scope {
    pub fn new() -> Self {
        Self { tables: Vec::new() }
    }

    /// Add a table from the catalog to this scope, with an optional alias.
    pub fn add_table(
        &mut self,
        catalog: &Catalog,
        table_name: &str,
        alias: Option<&str>,
    ) -> Result<TableId> {
        let schema = catalog
            .get_table(table_name)
            .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?;

        let columns: Vec<ScopeColumn> = schema
            .columns
            .iter()
            .map(|c| ScopeColumn {
                name: c.name.clone(),
                sql_type: c.sql_type.clone(),
                nullable: c.nullable,
            })
            .collect();

        let table_id = schema.id;
        self.tables.push(ScopeTable {
            table_id,
            table_name: table_name.to_string(),
            alias: alias.map(|s| s.to_string()),
            columns,
        });

        Ok(table_id)
    }

    /// Resolve a column reference, handling qualified (table.col) and unqualified (col) forms.
    /// Column matching is case-insensitive. Detects ambiguity when multiple tables have the
    /// same column name.
    pub fn resolve_column(
        &self,
        qualifier: Option<&str>,
        col_name: &str,
    ) -> Result<ColumnRef> {
        let col_lower = col_name.to_lowercase();

        if let Some(qual) = qualifier {
            // Qualified: find the table by name or alias
            let qual_lower = qual.to_lowercase();
            let table = self
                .tables
                .iter()
                .find(|t| {
                    t.alias
                        .as_ref()
                        .map(|a| a.to_lowercase() == qual_lower)
                        .unwrap_or(false)
                        || t.table_name.to_lowercase() == qual_lower
                })
                .ok_or_else(|| {
                    SqlError::TableNotFound(qual.to_string())
                })?;

            let (col_idx, col) = table
                .columns
                .iter()
                .enumerate()
                .find(|(_, c)| c.name.to_lowercase() == col_lower)
                .ok_or_else(|| {
                    SqlError::ColumnNotFound(format!("{}.{}", qual, col_name))
                })?;

            Ok(ColumnRef {
                table_id: table.table_id,
                table_name: table.table_name.clone(),
                column_index: col_idx,
                column_name: col.name.clone(),
                sql_type: col.sql_type.clone(),
                nullable: col.nullable,
            })
        } else {
            // Unqualified: search all tables, detect ambiguity
            let mut found: Option<ColumnRef> = None;
            for table in &self.tables {
                for (col_idx, col) in table.columns.iter().enumerate() {
                    if col.name.to_lowercase() == col_lower {
                        if found.is_some() {
                            return Err(SqlError::Bind(format!(
                                "ambiguous column reference: {col_name}"
                            )));
                        }
                        found = Some(ColumnRef {
                            table_id: table.table_id,
                            table_name: table.table_name.clone(),
                            column_index: col_idx,
                            column_name: col.name.clone(),
                            sql_type: col.sql_type.clone(),
                            nullable: col.nullable,
                        });
                    }
                }
            }
            found.ok_or_else(|| SqlError::ColumnNotFound(col_name.to_string()))
        }
    }

    /// Return all columns from all tables in scope (used for wildcard expansion).
    pub fn all_columns(&self) -> Vec<ColumnRef> {
        let mut cols = Vec::new();
        for table in &self.tables {
            for (idx, col) in table.columns.iter().enumerate() {
                cols.push(ColumnRef {
                    table_id: table.table_id,
                    table_name: table.table_name.clone(),
                    column_index: idx,
                    column_name: col.name.clone(),
                    sql_type: col.sql_type.clone(),
                    nullable: col.nullable,
                });
            }
        }
        cols
    }

    /// Return all columns from a specific table (qualified wildcard, e.g. `t.*`).
    pub fn columns_for_table(&self, qualifier: &str) -> Result<Vec<ColumnRef>> {
        let qual_lower = qualifier.to_lowercase();
        let table = self
            .tables
            .iter()
            .find(|t| {
                t.alias
                    .as_ref()
                    .map(|a| a.to_lowercase() == qual_lower)
                    .unwrap_or(false)
                    || t.table_name.to_lowercase() == qual_lower
            })
            .ok_or_else(|| SqlError::TableNotFound(qualifier.to_string()))?;

        Ok(table
            .columns
            .iter()
            .enumerate()
            .map(|(idx, col)| ColumnRef {
                table_id: table.table_id,
                table_name: table.table_name.clone(),
                column_index: idx,
                column_name: col.name.clone(),
                sql_type: col.sql_type.clone(),
                nullable: col.nullable,
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// bind_expr — convert sqlparser Expr to BoundExpr
// ---------------------------------------------------------------------------

pub fn bind_expr(
    scope: &Scope,
    expr: &ast::Expr,
    params: &[Value],
) -> Result<BoundExpr> {
    match expr {
        // Simple identifier → column lookup
        ast::Expr::Identifier(ident) => {
            let col_ref = scope.resolve_column(None, &ident.value)?;
            Ok(BoundExpr::Column(col_ref))
        }

        // Qualified identifier, e.g. table.col
        ast::Expr::CompoundIdentifier(parts) => {
            match parts.len() {
                2 => {
                    let col_ref = scope.resolve_column(
                        Some(&parts[0].value),
                        &parts[1].value,
                    )?;
                    Ok(BoundExpr::Column(col_ref))
                }
                _ => Err(SqlError::Bind(format!(
                    "unsupported compound identifier with {} parts",
                    parts.len()
                ))),
            }
        }

        // Value literals
        ast::Expr::Value(val_with_span) => bind_value(&val_with_span.value, params),

        // Binary operations
        ast::Expr::BinaryOp { left, op, right } => {
            let bound_left = bind_expr(scope, left, params)?;
            let bound_right = bind_expr(scope, right, params)?;
            let bin_op = map_binary_op(op)?;
            let result_type = infer_binary_type(&bin_op, &bound_left, &bound_right);
            Ok(BoundExpr::BinaryOp {
                op: bin_op,
                left: Box::new(bound_left),
                right: Box::new(bound_right),
                result_type,
            })
        }

        // Unary operations
        ast::Expr::UnaryOp { op, expr } => {
            let bound_expr = bind_expr(scope, expr, params)?;
            let unary_op = map_unary_op(op)?;
            let result_type = infer_unary_type(&unary_op, &bound_expr);
            Ok(BoundExpr::UnaryOp {
                op: unary_op,
                operand: Box::new(bound_expr),
                result_type,
            })
        }

        // IS NULL / IS NOT NULL
        ast::Expr::IsNull(expr) => {
            let bound_expr = bind_expr(scope, expr, params)?;
            Ok(BoundExpr::IsNull {
                operand: Box::new(bound_expr),
                negated: false,
            })
        }
        ast::Expr::IsNotNull(expr) => {
            let bound_expr = bind_expr(scope, expr, params)?;
            Ok(BoundExpr::IsNull {
                operand: Box::new(bound_expr),
                negated: true,
            })
        }

        // IN list
        ast::Expr::InList {
            expr,
            list,
            negated,
        } => {
            let bound_expr = bind_expr(scope, expr, params)?;
            let bound_list = list
                .iter()
                .map(|e| bind_expr(scope, e, params))
                .collect::<Result<Vec<_>>>()?;
            Ok(BoundExpr::InList {
                expr: Box::new(bound_expr),
                list: bound_list,
                negated: *negated,
            })
        }

        // BETWEEN
        ast::Expr::Between {
            expr,
            negated,
            low,
            high,
        } => {
            let bound_expr = bind_expr(scope, expr, params)?;
            let bound_low = bind_expr(scope, low, params)?;
            let bound_high = bind_expr(scope, high, params)?;
            Ok(BoundExpr::Between {
                expr: Box::new(bound_expr),
                low: Box::new(bound_low),
                high: Box::new(bound_high),
                negated: *negated,
            })
        }

        // LIKE
        ast::Expr::Like {
            negated,
            expr,
            pattern,
            ..
        } => {
            let bound_expr = bind_expr(scope, expr, params)?;
            let bound_pattern = bind_expr(scope, pattern, params)?;
            Ok(BoundExpr::Like {
                expr: Box::new(bound_expr),
                pattern: Box::new(bound_pattern),
                negated: *negated,
            })
        }

        // ILIKE — treat as LIKE for binding purposes
        ast::Expr::ILike {
            negated,
            expr,
            pattern,
            ..
        } => {
            let bound_expr = bind_expr(scope, expr, params)?;
            let bound_pattern = bind_expr(scope, pattern, params)?;
            Ok(BoundExpr::Like {
                expr: Box::new(bound_expr),
                pattern: Box::new(bound_pattern),
                negated: *negated,
            })
        }

        // Parenthesized expression
        ast::Expr::Nested(inner) => bind_expr(scope, inner, params),

        // Function call
        ast::Expr::Function(func) => bind_function(scope, func, params),

        // CAST
        ast::Expr::Cast {
            expr, data_type, ..
        } => {
            let bound_expr = bind_expr(scope, expr, params)?;
            let target_type = sql_data_type_to_sql_type(data_type)?;
            Ok(BoundExpr::Cast {
                expr: Box::new(bound_expr),
                target_type,
            })
        }

        // IS TRUE / IS FALSE / IS NOT TRUE / IS NOT FALSE
        ast::Expr::IsTrue(expr) => {
            let bound = bind_expr(scope, expr, params)?;
            Ok(BoundExpr::BinaryOp {
                op: BinaryOp::Eq,
                left: Box::new(bound),
                right: Box::new(BoundExpr::Literal(Value::Boolean(true))),
                result_type: SqlType::Boolean,
            })
        }
        ast::Expr::IsFalse(expr) => {
            let bound = bind_expr(scope, expr, params)?;
            Ok(BoundExpr::BinaryOp {
                op: BinaryOp::Eq,
                left: Box::new(bound),
                right: Box::new(BoundExpr::Literal(Value::Boolean(false))),
                result_type: SqlType::Boolean,
            })
        }
        ast::Expr::IsNotTrue(expr) => {
            let bound = bind_expr(scope, expr, params)?;
            Ok(BoundExpr::UnaryOp {
                op: UnaryOp::Not,
                operand: Box::new(BoundExpr::BinaryOp {
                    op: BinaryOp::Eq,
                    left: Box::new(bound),
                    right: Box::new(BoundExpr::Literal(Value::Boolean(true))),
                    result_type: SqlType::Boolean,
                }),
                result_type: SqlType::Boolean,
            })
        }
        ast::Expr::IsNotFalse(expr) => {
            let bound = bind_expr(scope, expr, params)?;
            Ok(BoundExpr::UnaryOp {
                op: UnaryOp::Not,
                operand: Box::new(BoundExpr::BinaryOp {
                    op: BinaryOp::Eq,
                    left: Box::new(bound),
                    right: Box::new(BoundExpr::Literal(Value::Boolean(false))),
                    result_type: SqlType::Boolean,
                }),
                result_type: SqlType::Boolean,
            })
        }

        other => Err(SqlError::Bind(format!(
            "unsupported expression: {other}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Value binding (including parameter placeholders)
// ---------------------------------------------------------------------------

fn bind_value(
    val: &ast::Value,
    params: &[Value],
) -> Result<BoundExpr> {
    match val {
        ast::Value::Number(s, _) => {
            let s_str = s.to_string();
            if s_str.contains('.') {
                let f: f64 = s_str
                    .parse()
                    .map_err(|_| SqlError::Bind(format!("invalid number: {s_str}")))?;
                Ok(BoundExpr::Literal(Value::Real(f)))
            } else {
                let i: i64 = s_str
                    .parse()
                    .map_err(|_| SqlError::Bind(format!("invalid integer: {s_str}")))?;
                Ok(BoundExpr::Literal(Value::Integer(i)))
            }
        }
        ast::Value::SingleQuotedString(s) => {
            Ok(BoundExpr::Literal(Value::Text(s.clone())))
        }
        ast::Value::Boolean(b) => {
            Ok(BoundExpr::Literal(Value::Boolean(*b)))
        }
        ast::Value::Null => Ok(BoundExpr::Literal(Value::Null)),
        ast::Value::Placeholder(p) => {
            // $1, $2, etc. — convert to 0-based index
            let idx_str = p.trim_start_matches('$');
            let idx: usize = idx_str
                .parse::<usize>()
                .map_err(|_| SqlError::Bind(format!("invalid placeholder: {p}")))?;
            if idx == 0 {
                return Err(SqlError::Bind(
                    "placeholder index must be >= 1".to_string(),
                ));
            }
            let zero_based = idx - 1;
            if zero_based >= params.len() {
                return Err(SqlError::Bind(format!(
                    "parameter ${idx} referenced but only {} parameters provided",
                    params.len()
                )));
            }
            Ok(BoundExpr::Parameter(zero_based))
        }
        ast::Value::DoubleQuotedString(s) => {
            Ok(BoundExpr::Literal(Value::Text(s.clone())))
        }
        other => Err(SqlError::Bind(format!(
            "unsupported value type: {other}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Function binding
// ---------------------------------------------------------------------------

fn bind_function(
    scope: &Scope,
    func: &ast::Function,
    params: &[Value],
) -> Result<BoundExpr> {
    let name_upper = func.name.to_string().to_uppercase();

    // Check if it is an aggregate function
    let agg_func = match name_upper.as_str() {
        "COUNT" => Some(AggregateFunc::Count),
        "SUM" => Some(AggregateFunc::Sum),
        "AVG" => Some(AggregateFunc::Avg),
        "MIN" => Some(AggregateFunc::Min),
        "MAX" => Some(AggregateFunc::Max),
        _ => None,
    };

    // Extract arguments and distinct flag
    let (args, distinct) = match &func.args {
        FunctionArguments::List(arg_list) => {
            let distinct = matches!(
                arg_list.duplicate_treatment,
                Some(DuplicateTreatment::Distinct)
            );
            let bound_args = arg_list
                .args
                .iter()
                .map(|arg| bind_function_arg(scope, arg, params))
                .collect::<Result<Vec<_>>>()?;
            (bound_args, distinct)
        }
        FunctionArguments::None => (Vec::new(), false),
        FunctionArguments::Subquery(_) => {
            return Err(SqlError::Bind(
                "subquery function arguments not supported".to_string(),
            ));
        }
    };

    if let Some(agg) = agg_func {
        // Aggregate function
        let arg = if args.is_empty() {
            None
        } else if args.len() == 1 {
            let a = args.into_iter().next().unwrap();
            if matches!(a, BoundExpr::Wildcard) {
                None // e.g. COUNT(*)
            } else {
                Some(Box::new(a))
            }
        } else {
            return Err(SqlError::Bind(format!(
                "aggregate function {name_upper} expects 0 or 1 arguments"
            )));
        };

        let result_type = match agg {
            AggregateFunc::Count => SqlType::BigInt,
            AggregateFunc::Sum | AggregateFunc::Avg => {
                // Derive from the argument type if available
                match &arg {
                    Some(a) => expr_type(a).unwrap_or(SqlType::Real),
                    None => SqlType::BigInt,
                }
            }
            AggregateFunc::Min | AggregateFunc::Max => {
                match &arg {
                    Some(a) => expr_type(a).unwrap_or(SqlType::Integer),
                    None => SqlType::Integer,
                }
            }
        };

        Ok(BoundExpr::Aggregate {
            func: agg,
            arg,
            distinct,
            result_type,
        })
    } else {
        // Scalar function
        let result_type = infer_scalar_function_type(&name_upper, &args);
        Ok(BoundExpr::Function {
            name: name_upper,
            args,
            result_type,
        })
    }
}

fn bind_function_arg(
    scope: &Scope,
    arg: &FunctionArg,
    params: &[Value],
) -> Result<BoundExpr> {
    match arg {
        FunctionArg::Unnamed(arg_expr) => match arg_expr {
            FunctionArgExpr::Expr(expr) => bind_expr(scope, expr, params),
            FunctionArgExpr::Wildcard => Ok(BoundExpr::Wildcard),
            FunctionArgExpr::QualifiedWildcard(_) => Ok(BoundExpr::Wildcard),
        },
        FunctionArg::Named { arg, .. } => match arg {
            FunctionArgExpr::Expr(expr) => bind_expr(scope, expr, params),
            FunctionArgExpr::Wildcard => Ok(BoundExpr::Wildcard),
            FunctionArgExpr::QualifiedWildcard(_) => Ok(BoundExpr::Wildcard),
        },
        FunctionArg::ExprNamed { arg, .. } => match arg {
            FunctionArgExpr::Expr(expr) => bind_expr(scope, expr, params),
            FunctionArgExpr::Wildcard => Ok(BoundExpr::Wildcard),
            FunctionArgExpr::QualifiedWildcard(_) => Ok(BoundExpr::Wildcard),
        },
    }
}

fn infer_scalar_function_type(name: &str, _args: &[BoundExpr]) -> SqlType {
    // Return a reasonable default type for known scalar functions
    match name {
        "LENGTH" | "CHAR_LENGTH" | "CHARACTER_LENGTH" | "OCTET_LENGTH" | "BIT_LENGTH" | "ABS"
        | "POSITION" => SqlType::Integer,
        "LOWER" | "UPPER" | "TRIM" | "LTRIM" | "RTRIM" | "REPLACE" | "SUBSTR" | "SUBSTRING"
        | "CONCAT" | "COALESCE" | "TYPEOF" => SqlType::Text,
        "RANDOM" | "ROUND" | "CEIL" | "FLOOR" | "SQRT" | "LN" | "LOG" | "EXP" | "POWER" => {
            SqlType::Real
        }
        "NOW" | "CURRENT_TIMESTAMP" => SqlType::Timestamp,
        "CURRENT_DATE" => SqlType::Date,
        "NULLIF" | "IFNULL" => SqlType::Text, // approximate
        _ => SqlType::Text, // fallback
    }
}

// ---------------------------------------------------------------------------
// Operator mapping
// ---------------------------------------------------------------------------

fn map_binary_op(op: &ast::BinaryOperator) -> Result<BinaryOp> {
    match op {
        ast::BinaryOperator::Plus => Ok(BinaryOp::Add),
        ast::BinaryOperator::Minus => Ok(BinaryOp::Sub),
        ast::BinaryOperator::Multiply => Ok(BinaryOp::Mul),
        ast::BinaryOperator::Divide => Ok(BinaryOp::Div),
        ast::BinaryOperator::Modulo => Ok(BinaryOp::Mod),
        ast::BinaryOperator::Eq => Ok(BinaryOp::Eq),
        ast::BinaryOperator::NotEq => Ok(BinaryOp::Neq),
        ast::BinaryOperator::Lt => Ok(BinaryOp::Lt),
        ast::BinaryOperator::Gt => Ok(BinaryOp::Gt),
        ast::BinaryOperator::LtEq => Ok(BinaryOp::Lte),
        ast::BinaryOperator::GtEq => Ok(BinaryOp::Gte),
        ast::BinaryOperator::And => Ok(BinaryOp::And),
        ast::BinaryOperator::Or => Ok(BinaryOp::Or),
        other => Err(SqlError::Bind(format!(
            "unsupported binary operator: {other}"
        ))),
    }
}

fn map_unary_op(op: &ast::UnaryOperator) -> Result<UnaryOp> {
    match op {
        ast::UnaryOperator::Minus => Ok(UnaryOp::Neg),
        ast::UnaryOperator::Not => Ok(UnaryOp::Not),
        ast::UnaryOperator::Plus => Ok(UnaryOp::Neg), // +x is identity, but we store it
        other => Err(SqlError::Bind(format!(
            "unsupported unary operator: {other}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Type inference helpers
// ---------------------------------------------------------------------------

/// Try to determine the SqlType of a BoundExpr (best-effort).
fn expr_type(expr: &BoundExpr) -> Option<SqlType> {
    match expr {
        BoundExpr::Column(col) => Some(col.sql_type.clone()),
        BoundExpr::Literal(val) => val.sql_type(),
        BoundExpr::Parameter(_) => None,
        BoundExpr::BinaryOp { result_type, .. } => Some(result_type.clone()),
        BoundExpr::UnaryOp { result_type, .. } => Some(result_type.clone()),
        BoundExpr::IsNull { .. } => Some(SqlType::Boolean),
        BoundExpr::InList { .. } => Some(SqlType::Boolean),
        BoundExpr::Between { .. } => Some(SqlType::Boolean),
        BoundExpr::Like { .. } => Some(SqlType::Boolean),
        BoundExpr::Function { result_type, .. } => Some(result_type.clone()),
        BoundExpr::Aggregate { result_type, .. } => Some(result_type.clone()),
        BoundExpr::Cast { target_type, .. } => Some(target_type.clone()),
        BoundExpr::Wildcard => None,
    }
}

fn infer_binary_type(op: &BinaryOp, left: &BoundExpr, right: &BoundExpr) -> SqlType {
    match op {
        // Comparison / logical operators always return boolean
        BinaryOp::Eq
        | BinaryOp::Neq
        | BinaryOp::Lt
        | BinaryOp::Gt
        | BinaryOp::Lte
        | BinaryOp::Gte
        | BinaryOp::And
        | BinaryOp::Or => SqlType::Boolean,

        // Arithmetic: try to preserve the wider type
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
            let lt = expr_type(left);
            let rt = expr_type(right);
            match (lt, rt) {
                (Some(l), Some(r)) => numeric_promotion(&l, &r),
                (Some(t), None) | (None, Some(t)) => t,
                (None, None) => SqlType::Integer,
            }
        }
    }
}

fn infer_unary_type(op: &UnaryOp, operand: &BoundExpr) -> SqlType {
    match op {
        UnaryOp::Not => SqlType::Boolean,
        UnaryOp::Neg => expr_type(operand).unwrap_or(SqlType::Integer),
    }
}

/// Promote two numeric types to a common type.
fn numeric_promotion(a: &SqlType, b: &SqlType) -> SqlType {
    // If either is Real, result is Real
    if matches!(a, SqlType::Real) || matches!(b, SqlType::Real) {
        return SqlType::Real;
    }
    // If either is Decimal, result is Decimal
    if matches!(a, SqlType::Decimal { .. }) || matches!(b, SqlType::Decimal { .. }) {
        return SqlType::Decimal {
            precision: 38,
            scale: 10,
        };
    }
    // If either is BigInt, result is BigInt
    if matches!(a, SqlType::BigInt) || matches!(b, SqlType::BigInt) {
        return SqlType::BigInt;
    }
    // If either is Integer, result is Integer
    if matches!(a, SqlType::Integer) || matches!(b, SqlType::Integer) {
        return SqlType::Integer;
    }
    // Default
    a.clone()
}

// ---------------------------------------------------------------------------
// sql_data_type_to_sql_type — map sqlparser DataType to our SqlType
// ---------------------------------------------------------------------------

pub fn sql_data_type_to_sql_type(data_type: &DataType) -> Result<SqlType> {
    match data_type {
        DataType::Boolean | DataType::Bool => Ok(SqlType::Boolean),

        DataType::SmallInt(_) | DataType::Int2(_) => Ok(SqlType::SmallInt),

        DataType::Int(None) | DataType::Int(Some(_))
        | DataType::Integer(None) | DataType::Integer(Some(_))
        | DataType::Int4(_) => Ok(SqlType::Integer),

        DataType::BigInt(_) | DataType::Int8(_) => Ok(SqlType::BigInt),

        DataType::Real | DataType::Float(None) | DataType::Float(Some(_))
        | DataType::Float4 | DataType::Float8 => Ok(SqlType::Real),

        DataType::Double(_) | DataType::DoublePrecision => Ok(SqlType::Real),

        DataType::Decimal(info) | DataType::Dec(info) | DataType::Numeric(info) => {
            let (precision, scale) = match info {
                ast::ExactNumberInfo::PrecisionAndScale(p, s) => (*p, *s),
                ast::ExactNumberInfo::Precision(p) => (*p, 0),
                ast::ExactNumberInfo::None => (38, 10),
            };
            Ok(SqlType::Decimal {
                precision: precision as u32,
                scale: scale as u32,
            })
        }

        DataType::Text => Ok(SqlType::Text),

        DataType::Varchar(len) | DataType::CharVarying(len) | DataType::CharacterVarying(len) => {
            match len {
                Some(char_len) => Ok(SqlType::Varchar(char_length_to_u32(char_len))),
                None => Ok(SqlType::Text),
            }
        }

        DataType::Char(len) | DataType::Character(len) => {
            match len {
                Some(char_len) => Ok(SqlType::Varchar(char_length_to_u32(char_len))),
                None => Ok(SqlType::Text),
            }
        }

        DataType::Blob(_) | DataType::Binary(_) | DataType::Varbinary(_)
        | DataType::Bytea => Ok(SqlType::Blob),

        DataType::Uuid => Ok(SqlType::Uuid),

        DataType::Date => Ok(SqlType::Date),

        DataType::Timestamp(_, tz_info) => {
            match tz_info {
                ast::TimezoneInfo::Tz | ast::TimezoneInfo::WithTimeZone => {
                    Ok(SqlType::TimestampTz)
                }
                _ => Ok(SqlType::Timestamp),
            }
        }

        DataType::JSON | DataType::JSONB => Ok(SqlType::Json),

        DataType::String(_) => Ok(SqlType::Text),

        other => Err(SqlError::Bind(format!(
            "unsupported data type: {other}"
        ))),
    }
}

fn char_length_to_u32(cl: &ast::CharacterLength) -> u32 {
    match cl {
        ast::CharacterLength::IntegerLength { length, .. } => *length as u32,
        ast::CharacterLength::Max => u32::MAX,
    }
}
