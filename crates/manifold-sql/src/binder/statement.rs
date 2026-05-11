use sqlparser::ast::{self, ObjectType, SetExpr, SetOperator, TableFactor};

use crate::catalog::schema::{
    self as schema, ColumnDef, ConstraintDef, ForeignKeyAction,
};
use crate::catalog::Catalog;
use crate::error::{Result, SqlError};
use crate::types::Value;

use super::expr::{bind_expr, sql_data_type_to_sql_type, Scope};
use super::{
    AlterTableOp, BoundAssignment, BoundExpr, BoundJoin, BoundOrderBy, BoundSelect,
    BoundSelectItem, BoundStatement, BoundTableRef, InsertSource, JoinType,
};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn bind_statement(
    catalog: &Catalog,
    stmt: &ast::Statement,
    params: &[Value],
) -> Result<BoundStatement> {
    match stmt {
        ast::Statement::Query(query) => bind_query(catalog, query, params),

        ast::Statement::Insert(insert) => bind_insert(catalog, insert, params),

        ast::Statement::Update {
            table,
            assignments,
            selection,
            ..
        } => bind_update(catalog, table, assignments, selection.as_ref(), params),

        ast::Statement::Delete(delete) => bind_delete(catalog, delete, params),

        ast::Statement::CreateTable(ct) => bind_create_table(ct),

        ast::Statement::CreateIndex(ci) => bind_create_index(ci),

        ast::Statement::Drop {
            object_type,
            if_exists,
            names,
            ..
        } => bind_drop(object_type, *if_exists, names),

        ast::Statement::AlterTable {
            name, operations, ..
        } => bind_alter_table(name, operations),

        ast::Statement::Explain { statement, .. } => {
            let inner = bind_statement(catalog, statement, params)?;
            Ok(BoundStatement::Explain(Box::new(inner)))
        }

        ast::Statement::Analyze {
            table_name,
            ..
        } => {
            let name = table_name.to_string();
            let table_name = if name.is_empty() { None } else { Some(name) };
            Ok(BoundStatement::Analyze { table_name })
        }

        ast::Statement::StartTransaction { .. } => Ok(BoundStatement::BeginTransaction),

        ast::Statement::Commit { .. } => Ok(BoundStatement::CommitTransaction),

        ast::Statement::Rollback { savepoint, .. } => {
            if let Some(sp_name) = savepoint {
                Ok(BoundStatement::RollbackToSavepoint {
                    name: sp_name.value.clone(),
                })
            } else {
                Ok(BoundStatement::RollbackTransaction)
            }
        }

        ast::Statement::Savepoint { name } => Ok(BoundStatement::Savepoint {
            name: name.value.clone(),
        }),

        ast::Statement::ReleaseSavepoint { name } => Ok(BoundStatement::ReleaseSavepoint {
            name: name.value.clone(),
        }),

        other => Err(SqlError::Bind(format!(
            "unsupported statement: {}",
            other.to_string().chars().take(80).collect::<String>()
        ))),
    }
}

// ---------------------------------------------------------------------------
// SELECT / Query
// ---------------------------------------------------------------------------

fn bind_query(
    catalog: &Catalog,
    query: &ast::Query,
    params: &[Value],
) -> Result<BoundStatement> {
    // Handle UNION / INTERSECT / EXCEPT at the top level before ORDER BY / LIMIT.
    if let SetExpr::SetOperation { op, set_quantifier, left, right } = query.body.as_ref() {
        let all = matches!(set_quantifier, ast::SetQuantifier::All);
        match op {
            SetOperator::Union => {
                let left_stmt = bind_query(catalog, &ast::Query {
                    with: None,
                    body: left.clone(),
                    order_by: None,
                    limit: None,
                    limit_by: vec![],
                    offset: None,
                    fetch: None,
                    locks: vec![],
                    for_clause: None,
                    settings: None,
                    format_clause: None,
                }, params)?;
                let right_stmt = bind_query(catalog, &ast::Query {
                    with: None,
                    body: right.clone(),
                    order_by: None,
                    limit: None,
                    limit_by: vec![],
                    offset: None,
                    fetch: None,
                    locks: vec![],
                    for_clause: None,
                    settings: None,
                    format_clause: None,
                }, params)?;
                return Ok(BoundStatement::Union {
                    left: Box::new(left_stmt),
                    right: Box::new(right_stmt),
                    all,
                });
            }
            other => {
                return Err(SqlError::Bind(format!(
                    "unsupported set operation: {other}"
                )));
            }
        }
    }

    let mut select = bind_query_body(catalog, &query.body, params)?;

    // ORDER BY
    if let Some(order_by) = &query.order_by
        && let ast::OrderByKind::Expressions(exprs) = &order_by.kind
    {
        let scope = build_select_scope(catalog, &select)?;
        for ob in exprs {
            let bound_expr = bind_expr(&scope, &ob.expr, params)?;
            select.order_by.push(BoundOrderBy {
                expr: bound_expr,
                asc: ob.options.asc.unwrap_or(true),
                nulls_first: ob.options.nulls_first,
            });
        }
    }

    // LIMIT
    if let Some(limit_expr) = &query.limit {
        let scope = build_select_scope(catalog, &select)?;
        select.limit = Some(bind_expr(&scope, limit_expr, params)?);
    }

    // OFFSET
    if let Some(offset) = &query.offset {
        let scope = build_select_scope(catalog, &select)?;
        select.offset = Some(bind_expr(&scope, &offset.value, params)?);
    }

    Ok(BoundStatement::Select(Box::new(select)))
}

fn bind_query_body(
    catalog: &Catalog,
    body: &SetExpr,
    params: &[Value],
) -> Result<BoundSelect> {
    match body {
        SetExpr::Select(sel) => bind_select_body(catalog, sel, params),
        SetExpr::Query(q) => {
            // Nested query — just recurse
            let stmt = bind_query(catalog, q, params)?;
            match stmt {
                BoundStatement::Select(s) => Ok(*s),
                _ => Err(SqlError::Bind("expected SELECT in subquery".to_string())),
            }
        }
        SetExpr::Values(values) => {
            // VALUES (...), (...) as a standalone select
            let mut rows = Vec::new();
            let empty_scope = Scope::new();
            for row in &values.rows {
                let bound_row = row
                    .iter()
                    .map(|e| bind_expr(&empty_scope, e, params))
                    .collect::<Result<Vec<_>>>()?;
                rows.push(bound_row);
            }
            // Convert to a select with no from — just projection
            let projection = if let Some(first_row) = rows.first() {
                first_row
                    .iter()
                    .enumerate()
                    .map(|(i, expr)| BoundSelectItem {
                        expr: expr.clone(),
                        alias: Some(format!("column{}", i + 1)),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            Ok(BoundSelect {
                from: Vec::new(),
                joins: Vec::new(),
                filter: None,
                projection,
                group_by: Vec::new(),
                having: None,
                order_by: Vec::new(),
                limit: None,
                offset: None,
                distinct: false,
            })
        }
        other => Err(SqlError::Bind(format!(
            "unsupported set expression: {other}"
        ))),
    }
}

fn bind_select_body(
    catalog: &Catalog,
    select: &ast::Select,
    params: &[Value],
) -> Result<BoundSelect> {
    let mut scope = Scope::new();
    let mut from = Vec::new();
    let mut joins = Vec::new();

    // FROM clause
    for table_with_joins in &select.from {
        let table_ref = bind_table_factor(catalog, &mut scope, &table_with_joins.relation)?;
        from.push(table_ref);

        // JOINs
        for join in &table_with_joins.joins {
            let (join_type, constraint) = match &join.join_operator {
                ast::JoinOperator::Inner(c) => (JoinType::Inner, Some(c)),
                ast::JoinOperator::Join(c) => (JoinType::Inner, Some(c)),
                ast::JoinOperator::Left(c) | ast::JoinOperator::LeftOuter(c) => {
                    (JoinType::Left, Some(c))
                }
                ast::JoinOperator::Right(c) | ast::JoinOperator::RightOuter(c) => {
                    (JoinType::Right, Some(c))
                }
                ast::JoinOperator::CrossJoin => (JoinType::Cross, None),
                ast::JoinOperator::FullOuter(c) => (JoinType::Left, Some(c)), // approximate
                other => {
                    return Err(SqlError::Bind(format!(
                        "unsupported join type: {other:?}"
                    )));
                }
            };

            let join_table =
                bind_table_factor(catalog, &mut scope, &join.relation)?;

            let condition = match constraint {
                Some(ast::JoinConstraint::On(expr)) => {
                    Some(bind_expr(&scope, expr, params)?)
                }
                Some(ast::JoinConstraint::None) | None => None,
                Some(ast::JoinConstraint::Natural) => None,
                Some(ast::JoinConstraint::Using(_)) => None,
            };

            joins.push(BoundJoin {
                join_type,
                table: join_table,
                condition,
            });
        }
    }

    // WHERE
    let filter = match &select.selection {
        Some(expr) => Some(bind_expr(&scope, expr, params)?),
        None => None,
    };

    // SELECT list (projection)
    let mut projection = Vec::new();
    for item in &select.projection {
        match item {
            ast::SelectItem::UnnamedExpr(expr) => {
                let bound = bind_expr(&scope, expr, params)?;
                let alias = None;
                projection.push(BoundSelectItem { expr: bound, alias });
            }
            ast::SelectItem::ExprWithAlias { expr, alias } => {
                let bound = bind_expr(&scope, expr, params)?;
                projection.push(BoundSelectItem {
                    expr: bound,
                    alias: Some(alias.value.clone()),
                });
            }
            ast::SelectItem::Wildcard(_) => {
                // Expand wildcard to all columns
                for col_ref in scope.all_columns() {
                    projection.push(BoundSelectItem {
                        alias: Some(col_ref.column_name.clone()),
                        expr: BoundExpr::Column(col_ref),
                    });
                }
            }
            ast::SelectItem::QualifiedWildcard(kind, _) => {
                let qualifier = match kind {
                    ast::SelectItemQualifiedWildcardKind::ObjectName(name) => name.to_string(),
                    ast::SelectItemQualifiedWildcardKind::Expr(expr) => expr.to_string(),
                };
                for col_ref in scope.columns_for_table(&qualifier)? {
                    projection.push(BoundSelectItem {
                        alias: Some(col_ref.column_name.clone()),
                        expr: BoundExpr::Column(col_ref),
                    });
                }
            }
        }
    }

    // GROUP BY
    let mut group_by = Vec::new();
    if let ast::GroupByExpr::Expressions(exprs, _) = &select.group_by {
        for expr in exprs {
            group_by.push(bind_expr(&scope, expr, params)?);
        }
    }

    // HAVING
    let having = match &select.having {
        Some(expr) => Some(bind_expr(&scope, expr, params)?),
        None => None,
    };

    let distinct = select.distinct.is_some();

    Ok(BoundSelect {
        from,
        joins,
        filter,
        projection,
        group_by,
        having,
        order_by: Vec::new(), // filled in by bind_query
        limit: None,
        offset: None,
        distinct,
    })
}

fn bind_table_factor(
    catalog: &Catalog,
    scope: &mut Scope,
    factor: &TableFactor,
) -> Result<BoundTableRef> {
    match factor {
        TableFactor::Table { name, alias, .. } => {
            let table_name = name.to_string();
            let alias_str = alias.as_ref().map(|a| a.name.value.clone());
            let table_id =
                scope.add_table(catalog, &table_name, alias_str.as_deref())?;
            Ok(BoundTableRef {
                table_id,
                table_name,
                alias: alias_str,
            })
        }
        other => Err(SqlError::Bind(format!(
            "unsupported table factor: {other}"
        ))),
    }
}

/// Build a scope from a BoundSelect's from/joins for use in ORDER BY / LIMIT binding.
fn build_select_scope(catalog: &Catalog, select: &BoundSelect) -> Result<Scope> {
    let mut scope = Scope::new();
    for table_ref in &select.from {
        scope.add_table(catalog, &table_ref.table_name, table_ref.alias.as_deref())?;
    }
    for join in &select.joins {
        scope.add_table(
            catalog,
            &join.table.table_name,
            join.table.alias.as_deref(),
        )?;
    }
    Ok(scope)
}

// ---------------------------------------------------------------------------
// INSERT
// ---------------------------------------------------------------------------

fn bind_insert(
    catalog: &Catalog,
    insert: &ast::Insert,
    params: &[Value],
) -> Result<BoundStatement> {
    let table_name = match &insert.table {
        ast::TableObject::TableName(name) => name.to_string(),
        _ => {
            return Err(SqlError::Bind(
                "unsupported INSERT table object".to_string(),
            ))
        }
    };
    let schema = catalog
        .get_table(&table_name)
        .ok_or_else(|| SqlError::TableNotFound(table_name.clone()))?;

    // Resolve column indices
    let columns: Vec<usize> = if insert.columns.is_empty() {
        // All columns
        (0..schema.columns.len()).collect()
    } else {
        insert
            .columns
            .iter()
            .map(|ident| {
                schema.column_index(&ident.value).ok_or_else(|| {
                    SqlError::ColumnNotFound(format!(
                        "{}.{}",
                        table_name, ident.value
                    ))
                })
            })
            .collect::<Result<Vec<_>>>()?
    };

    let source = match &insert.source {
        Some(query) => {
            // Could be VALUES or a SELECT
            match query.body.as_ref() {
                SetExpr::Values(values) => {
                    let empty_scope = Scope::new();
                    let mut rows = Vec::new();
                    for row in &values.rows {
                        let bound_row = row
                            .iter()
                            .map(|e| bind_expr(&empty_scope, e, params))
                            .collect::<Result<Vec<_>>>()?;
                        if bound_row.len() != columns.len() {
                            return Err(SqlError::Bind(format!(
                                "INSERT has {} columns but {} values were supplied",
                                columns.len(),
                                bound_row.len()
                            )));
                        }
                        rows.push(bound_row);
                    }
                    InsertSource::Values(rows)
                }
                _ => {
                    // INSERT ... SELECT
                    let select = bind_query_body(catalog, &query.body, params)?;
                    InsertSource::Select(Box::new(select))
                }
            }
        }
        None => {
            return Err(SqlError::Bind(
                "INSERT without VALUES or SELECT source".to_string(),
            ))
        }
    };

    Ok(BoundStatement::Insert {
        table_id: schema.id,
        table_name,
        columns,
        source,
    })
}

// ---------------------------------------------------------------------------
// UPDATE
// ---------------------------------------------------------------------------

fn bind_update(
    catalog: &Catalog,
    table: &ast::TableWithJoins,
    assignments: &[ast::Assignment],
    selection: Option<&ast::Expr>,
    params: &[Value],
) -> Result<BoundStatement> {
    let table_name = match &table.relation {
        TableFactor::Table { name, .. } => name.to_string(),
        other => {
            return Err(SqlError::Bind(format!(
                "unsupported table in UPDATE: {other}"
            )))
        }
    };

    let schema = catalog
        .get_table(&table_name)
        .ok_or_else(|| SqlError::TableNotFound(table_name.clone()))?;
    let table_id = schema.id;

    let mut scope = Scope::new();
    scope.add_table(catalog, &table_name, None)?;

    let bound_assignments = assignments
        .iter()
        .map(|a| {
            let col_name = match &a.target {
                ast::AssignmentTarget::ColumnName(name) => name.to_string(),
                ast::AssignmentTarget::Tuple(_) => {
                    return Err(SqlError::Bind(
                        "tuple assignment not supported".to_string(),
                    ))
                }
            };
            let col_idx = schema.column_index(&col_name).ok_or_else(|| {
                SqlError::ColumnNotFound(format!("{table_name}.{col_name}"))
            })?;
            let value = bind_expr(&scope, &a.value, params)?;
            Ok(BoundAssignment {
                column_index: col_idx,
                value,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let filter = match selection {
        Some(expr) => Some(bind_expr(&scope, expr, params)?),
        None => None,
    };

    Ok(BoundStatement::Update {
        table_id,
        table_name,
        assignments: bound_assignments,
        filter,
    })
}

// ---------------------------------------------------------------------------
// DELETE
// ---------------------------------------------------------------------------

fn bind_delete(
    catalog: &Catalog,
    delete: &ast::Delete,
    params: &[Value],
) -> Result<BoundStatement> {
    let tables_with_joins = match &delete.from {
        ast::FromTable::WithFromKeyword(tables) => tables,
        ast::FromTable::WithoutKeyword(tables) => tables,
    };

    let first = tables_with_joins
        .first()
        .ok_or_else(|| SqlError::Bind("DELETE requires a FROM clause".to_string()))?;

    let table_name = match &first.relation {
        TableFactor::Table { name, .. } => name.to_string(),
        other => {
            return Err(SqlError::Bind(format!(
                "unsupported table in DELETE: {other}"
            )))
        }
    };

    let schema = catalog
        .get_table(&table_name)
        .ok_or_else(|| SqlError::TableNotFound(table_name.clone()))?;
    let table_id = schema.id;

    let mut scope = Scope::new();
    scope.add_table(catalog, &table_name, None)?;

    let filter = match &delete.selection {
        Some(expr) => Some(bind_expr(&scope, expr, params)?),
        None => None,
    };

    Ok(BoundStatement::Delete {
        table_id,
        table_name,
        filter,
    })
}

// ---------------------------------------------------------------------------
// CREATE TABLE
// ---------------------------------------------------------------------------

fn bind_create_table(ct: &ast::CreateTable) -> Result<BoundStatement> {
    let name = ct.name.to_string();

    let mut columns = Vec::new();
    let mut constraints = Vec::new();
    let mut constraint_id: u32 = 0;
    let mut next_id = || {
        constraint_id += 1;
        constraint_id
    };

    for (col_idx, col_def) in ct.columns.iter().enumerate() {
        let sql_type = sql_data_type_to_sql_type(&col_def.data_type)?;
        let mut nullable = true;
        let mut is_primary_key = false;
        let mut default = None;

        for opt in &col_def.options {
            match &opt.option {
                ast::ColumnOption::NotNull => {
                    nullable = false;
                }
                ast::ColumnOption::Null => {
                    nullable = true;
                }
                ast::ColumnOption::Unique { is_primary, .. } => {
                    if *is_primary {
                        is_primary_key = true;
                        nullable = false;
                        constraints.push(ConstraintDef::PrimaryKey {
                            id: next_id(),
                            name: format!("pk_{name}_{}", col_def.name.value),
                            columns: vec![col_idx],
                        });
                    } else {
                        constraints.push(ConstraintDef::Unique {
                            id: next_id(),
                            name: format!(
                                "uq_{name}_{}",
                                col_def.name.value
                            ),
                            columns: vec![col_idx],
                        });
                    }
                }
                ast::ColumnOption::Default(expr) => {
                    default = Some(bind_default_value(expr));
                }
                ast::ColumnOption::ForeignKey {
                    foreign_table,
                    referred_columns,
                    on_delete,
                    on_update,
                    ..
                } => {
                    constraints.push(ConstraintDef::ForeignKey {
                        id: next_id(),
                        name: format!(
                            "fk_{name}_{}_{}",
                            col_def.name.value, foreign_table
                        ),
                        columns: vec![col_idx],
                        ref_table: foreign_table.to_string(),
                        ref_columns: referred_columns
                            .iter()
                            .map(|c| c.value.clone())
                            .collect(),
                        on_delete: on_delete
                            .as_ref()
                            .map(map_referential_action)
                            .unwrap_or_default(),
                        on_update: on_update
                            .as_ref()
                            .map(map_referential_action)
                            .unwrap_or_default(),
                    });
                }
                ast::ColumnOption::Check(expr) => {
                    constraints.push(ConstraintDef::Check {
                        id: next_id(),
                        name: format!(
                            "ck_{name}_{}",
                            col_def.name.value
                        ),
                        expression: expr.to_string(),
                    });
                }
                _ => {
                    // Ignore other options (e.g., dialect-specific)
                }
            }
        }

        columns.push(ColumnDef {
            name: col_def.name.value.clone(),
            sql_type,
            nullable,
            default,
            is_primary_key,
        });
    }

    // Table-level constraints
    for tc in &ct.constraints {
        match tc {
            ast::TableConstraint::PrimaryKey {
                name: tc_name,
                columns: pk_cols,
                ..
            } => {
                let col_indices = resolve_constraint_columns(&columns, pk_cols)?;
                constraints.push(ConstraintDef::PrimaryKey {
                    id: next_id(),
                    name: tc_name
                        .as_ref()
                        .map(|n| n.value.clone())
                        .unwrap_or_else(|| format!("pk_{name}")),
                    columns: col_indices,
                });
            }
            ast::TableConstraint::Unique {
                name: tc_name,
                columns: uq_cols,
                ..
            } => {
                let col_indices = resolve_constraint_columns(&columns, uq_cols)?;
                constraints.push(ConstraintDef::Unique {
                    id: next_id(),
                    name: tc_name
                        .as_ref()
                        .map(|n| n.value.clone())
                        .unwrap_or_else(|| format!("uq_{name}")),
                    columns: col_indices,
                });
            }
            ast::TableConstraint::ForeignKey {
                name: tc_name,
                columns: fk_cols,
                foreign_table,
                referred_columns,
                on_delete,
                on_update,
                ..
            } => {
                let col_indices = resolve_constraint_columns(&columns, fk_cols)?;
                constraints.push(ConstraintDef::ForeignKey {
                    id: next_id(),
                    name: tc_name
                        .as_ref()
                        .map(|n| n.value.clone())
                        .unwrap_or_else(|| format!("fk_{name}")),
                    columns: col_indices,
                    ref_table: foreign_table.to_string(),
                    ref_columns: referred_columns.iter().map(|c| c.value.clone()).collect(),
                    on_delete: on_delete
                        .as_ref()
                        .map(map_referential_action)
                        .unwrap_or_default(),
                    on_update: on_update
                        .as_ref()
                        .map(map_referential_action)
                        .unwrap_or_default(),
                });
            }
            ast::TableConstraint::Check { name: tc_name, expr } => {
                constraints.push(ConstraintDef::Check {
                    id: next_id(),
                    name: tc_name
                        .as_ref()
                        .map(|n| n.value.clone())
                        .unwrap_or_else(|| format!("ck_{name}")),
                    expression: expr.to_string(),
                });
            }
            _ => {
                // Ignore other constraint types (Index, FulltextOrSpatial)
            }
        }
    }

    Ok(BoundStatement::CreateTable {
        name,
        columns,
        constraints,
        if_not_exists: ct.if_not_exists,
    })
}

fn resolve_constraint_columns(
    columns: &[ColumnDef],
    idents: &[ast::Ident],
) -> Result<Vec<usize>> {
    idents
        .iter()
        .map(|ident| {
            let lower = ident.value.to_lowercase();
            columns
                .iter()
                .position(|c| c.name.to_lowercase() == lower)
                .ok_or_else(|| SqlError::ColumnNotFound(ident.value.clone()))
        })
        .collect()
}

fn bind_default_value(expr: &ast::Expr) -> schema::DefaultValue {
    let s = expr.to_string().to_uppercase();
    if s.contains("CURRENT_TIMESTAMP") || s.contains("NOW()") {
        return schema::DefaultValue::CurrentTimestamp;
    }
    if s.contains("CURRENT_DATE") {
        return schema::DefaultValue::CurrentDate;
    }
    if s == "NULL" {
        return schema::DefaultValue::Null;
    }
    // Try to parse as a literal value
    match expr {
        ast::Expr::Value(val_with_span) => match &val_with_span.value {
            ast::Value::Number(n, _) => {
                let n_str = n.to_string();
                if let Ok(i) = n_str.parse::<i64>() {
                    return schema::DefaultValue::Literal(Value::Integer(i));
                }
                if let Ok(f) = n_str.parse::<f64>() {
                    return schema::DefaultValue::Literal(Value::Real(f));
                }
                schema::DefaultValue::Literal(Value::Text(n_str))
            }
            ast::Value::SingleQuotedString(s) => {
                schema::DefaultValue::Literal(Value::Text(s.clone()))
            }
            ast::Value::Boolean(b) => {
                schema::DefaultValue::Literal(Value::Boolean(*b))
            }
            ast::Value::Null => schema::DefaultValue::Null,
            _ => schema::DefaultValue::Literal(Value::Text(expr.to_string())),
        },
        _ => schema::DefaultValue::Literal(Value::Text(expr.to_string())),
    }
}

fn map_referential_action(action: &ast::ReferentialAction) -> ForeignKeyAction {
    match action {
        ast::ReferentialAction::Restrict => ForeignKeyAction::Restrict,
        ast::ReferentialAction::Cascade => ForeignKeyAction::Cascade,
        ast::ReferentialAction::SetNull => ForeignKeyAction::SetNull,
        ast::ReferentialAction::NoAction => ForeignKeyAction::NoAction,
        ast::ReferentialAction::SetDefault => ForeignKeyAction::Restrict, // approximate
    }
}

// ---------------------------------------------------------------------------
// CREATE INDEX
// ---------------------------------------------------------------------------

fn bind_create_index(ci: &ast::CreateIndex) -> Result<BoundStatement> {
    let index_name = ci
        .name
        .as_ref()
        .map(|n| n.to_string())
        .ok_or_else(|| SqlError::Bind("CREATE INDEX requires an index name".to_string()))?;

    let table_name = ci.table_name.to_string();

    let columns: Vec<String> = ci
        .columns
        .iter()
        .map(|col| col.expr.to_string())
        .collect();

    Ok(BoundStatement::CreateIndex {
        index_name,
        table_name,
        columns,
        unique: ci.unique,
        if_not_exists: ci.if_not_exists,
    })
}

// ---------------------------------------------------------------------------
// DROP (Table / Index)
// ---------------------------------------------------------------------------

fn bind_drop(
    object_type: &ObjectType,
    if_exists: bool,
    names: &[ast::ObjectName],
) -> Result<BoundStatement> {
    let name = names
        .first()
        .ok_or_else(|| SqlError::Bind("DROP requires a name".to_string()))?
        .to_string();

    match object_type {
        ObjectType::Table => Ok(BoundStatement::DropTable { name, if_exists }),
        ObjectType::Index => Ok(BoundStatement::DropIndex { name, if_exists }),
        other => Err(SqlError::Bind(format!(
            "unsupported DROP object type: {other}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// ALTER TABLE
// ---------------------------------------------------------------------------

fn bind_alter_table(
    name: &ast::ObjectName,
    operations: &[ast::AlterTableOperation],
) -> Result<BoundStatement> {
    let table_name = name.to_string();

    let op = operations
        .first()
        .ok_or_else(|| SqlError::Bind("ALTER TABLE requires an operation".to_string()))?;

    let alter_op = match op {
        ast::AlterTableOperation::AddColumn { column_def, .. } => {
            let sql_type = sql_data_type_to_sql_type(&column_def.data_type)?;
            let mut nullable = true;
            let mut is_primary_key = false;
            let mut default = None;

            for opt in &column_def.options {
                match &opt.option {
                    ast::ColumnOption::NotNull => nullable = false,
                    ast::ColumnOption::Null => nullable = true,
                    ast::ColumnOption::Unique { is_primary, .. } => {
                        if *is_primary {
                            is_primary_key = true;
                            nullable = false;
                        }
                    }
                    ast::ColumnOption::Default(expr) => {
                        default = Some(bind_default_value(expr));
                    }
                    _ => {}
                }
            }

            AlterTableOp::AddColumn(ColumnDef {
                name: column_def.name.value.clone(),
                sql_type,
                nullable,
                default,
                is_primary_key,
            })
        }
        ast::AlterTableOperation::DropColumn {
            column_name, ..
        } => AlterTableOp::DropColumn(column_name.value.clone()),
        ast::AlterTableOperation::RenameColumn {
            old_column_name,
            new_column_name,
        } => AlterTableOp::RenameColumn {
            old: old_column_name.value.clone(),
            new: new_column_name.value.clone(),
        },
        other => {
            return Err(SqlError::Bind(format!(
                "unsupported ALTER TABLE operation: {other}"
            )));
        }
    };

    Ok(BoundStatement::AlterTable {
        table_name,
        operation: alter_op,
    })
}
