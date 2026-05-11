use crate::binder::{AggregateFunc, AlterTableOp, BinaryOp, JoinType, UnaryOp};
use crate::catalog::schema::{ColumnDef, ConstraintDef, TableId};
use crate::types::{SqlType, Value};

// ---------------------------------------------------------------------------
// Schema types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PlanSchema {
    pub columns: Vec<PlanColumn>,
}

impl PlanSchema {
    pub fn empty() -> Self {
        Self {
            columns: Vec::new(),
        }
    }

    /// Number of columns in the schema.
    pub fn len(&self) -> usize {
        self.columns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// Concatenate two schemas (used for joins).
    pub fn concat(left: &PlanSchema, right: &PlanSchema) -> Self {
        let mut columns = left.columns.clone();
        columns.extend(right.columns.iter().cloned());
        Self { columns }
    }
}

#[derive(Debug, Clone)]
pub struct PlanColumn {
    pub name: String,
    pub sql_type: SqlType,
    pub nullable: bool,
}

// ---------------------------------------------------------------------------
// Scalar expressions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum ScalarExpr {
    /// Reference to a column by positional index in the input row.
    ColumnRef { index: usize },
    /// A literal value.
    Literal(Value),
    /// A query parameter (0-based).
    Parameter(usize),
    /// Binary operation.
    BinaryOp {
        op: BinaryOp,
        left: Box<ScalarExpr>,
        right: Box<ScalarExpr>,
    },
    /// Unary operation.
    UnaryOp {
        op: UnaryOp,
        operand: Box<ScalarExpr>,
    },
    /// IS NULL / IS NOT NULL.
    IsNull {
        operand: Box<ScalarExpr>,
        negated: bool,
    },
    /// IN list.
    InList {
        expr: Box<ScalarExpr>,
        list: Vec<ScalarExpr>,
        negated: bool,
    },
    /// BETWEEN.
    Between {
        expr: Box<ScalarExpr>,
        low: Box<ScalarExpr>,
        high: Box<ScalarExpr>,
        negated: bool,
    },
    /// LIKE.
    Like {
        expr: Box<ScalarExpr>,
        pattern: Box<ScalarExpr>,
        negated: bool,
    },
    /// Scalar function call.
    Function {
        name: String,
        args: Vec<ScalarExpr>,
    },
    /// CAST.
    Cast {
        expr: Box<ScalarExpr>,
        target_type: SqlType,
    },
    /// `expr IN (SELECT ...)` — the subquery plan produces a set of values.
    InSubquery {
        expr: Box<ScalarExpr>,
        subquery: Box<LogicalPlan>,
        negated: bool,
    },
    /// `EXISTS (SELECT ...)` — check if subquery produces any rows.
    Exists {
        subquery: Box<LogicalPlan>,
        negated: bool,
    },
    /// Scalar subquery: `(SELECT ...)` producing a single value.
    ScalarSubquery {
        subquery: Box<LogicalPlan>,
    },
}

// ---------------------------------------------------------------------------
// Aggregate expression
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AggregateExpr {
    pub func: AggregateFunc,
    pub arg: Option<ScalarExpr>,
    pub distinct: bool,
    pub result_type: SqlType,
}

// ---------------------------------------------------------------------------
// ORDER BY expression
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct OrderByExpr {
    pub expr: ScalarExpr,
    pub asc: bool,
    pub nulls_first: Option<bool>,
}

// ---------------------------------------------------------------------------
// Logical plan
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum LogicalPlan {
    /// Sequential scan of a table.
    Scan {
        table_id: TableId,
        table_name: String,
        schema: PlanSchema,
    },
    /// Filter rows by a predicate.
    Filter {
        predicate: ScalarExpr,
        input: Box<LogicalPlan>,
    },
    /// Project (compute) expressions over input rows.
    Project {
        expressions: Vec<ScalarExpr>,
        aliases: Vec<String>,
        schema: PlanSchema,
        input: Box<LogicalPlan>,
    },
    /// Join two inputs.
    Join {
        join_type: JoinType,
        left: Box<LogicalPlan>,
        right: Box<LogicalPlan>,
        condition: Option<ScalarExpr>,
        schema: PlanSchema,
    },
    /// Aggregate with optional GROUP BY.
    Aggregate {
        group_by: Vec<ScalarExpr>,
        aggregates: Vec<AggregateExpr>,
        schema: PlanSchema,
        input: Box<LogicalPlan>,
    },
    /// Sort by ORDER BY expressions.
    Sort {
        order_by: Vec<OrderByExpr>,
        input: Box<LogicalPlan>,
    },
    /// LIMIT / OFFSET.
    Limit {
        count: Option<ScalarExpr>,
        offset: Option<ScalarExpr>,
        input: Box<LogicalPlan>,
    },
    /// DISTINCT.
    Distinct {
        input: Box<LogicalPlan>,
    },
    /// UNION / UNION ALL.
    Union {
        left: Box<LogicalPlan>,
        right: Box<LogicalPlan>,
        all: bool,
        schema: PlanSchema,
    },
    /// INSERT.
    Insert {
        table_id: TableId,
        table_name: String,
        columns: Vec<usize>,
        source: Box<LogicalPlan>,
    },
    /// UPDATE.
    Update {
        table_id: TableId,
        table_name: String,
        assignments: Vec<(usize, ScalarExpr)>,
        input: Box<LogicalPlan>,
    },
    /// DELETE.
    Delete {
        table_id: TableId,
        table_name: String,
        input: Box<LogicalPlan>,
    },
    /// Literal VALUES rows.
    Values {
        rows: Vec<Vec<ScalarExpr>>,
        schema: PlanSchema,
    },
    /// CREATE TABLE.
    CreateTable {
        name: String,
        columns: Vec<ColumnDef>,
        constraints: Vec<ConstraintDef>,
        if_not_exists: bool,
    },
    /// DROP TABLE.
    DropTable {
        name: String,
        if_exists: bool,
    },
    /// ALTER TABLE.
    AlterTable {
        table_name: String,
        operation: AlterTableOp,
    },
    /// CREATE INDEX.
    CreateIndex {
        index_name: String,
        table_name: String,
        columns: Vec<String>,
        unique: bool,
        if_not_exists: bool,
    },
    /// DROP INDEX.
    DropIndex {
        name: String,
        if_exists: bool,
    },
    /// EXPLAIN.
    Explain {
        input: Box<LogicalPlan>,
    },
    /// ANALYZE.
    Analyze {
        table_name: Option<String>,
    },
    /// Index scan (used by optimizer, not produced by planner).
    IndexScan {
        table_id: TableId,
        table_name: String,
        index_name: String,
        schema: PlanSchema,
    },
    /// An empty plan (no rows, no schema).
    Empty,
    /// BEGIN TRANSACTION.
    BeginTransaction,
    /// COMMIT.
    CommitTransaction,
    /// ROLLBACK.
    RollbackTransaction,
    /// SAVEPOINT <name>.
    Savepoint { name: String },
    /// RELEASE SAVEPOINT <name>.
    ReleaseSavepoint { name: String },
    /// ROLLBACK TO SAVEPOINT <name>.
    RollbackToSavepoint { name: String },
}

impl LogicalPlan {
    /// Returns the output schema of this plan node, if applicable.
    pub fn schema(&self) -> Option<&PlanSchema> {
        match self {
            LogicalPlan::Scan { schema, .. } => Some(schema),
            LogicalPlan::Filter { input, .. } => input.schema(),
            LogicalPlan::Project { schema, .. } => Some(schema),
            LogicalPlan::Join { schema, .. } => Some(schema),
            LogicalPlan::Aggregate { schema, .. } => Some(schema),
            LogicalPlan::Sort { input, .. } => input.schema(),
            LogicalPlan::Limit { input, .. } => input.schema(),
            LogicalPlan::Distinct { input } => input.schema(),
            LogicalPlan::Union { schema, .. } => Some(schema),
            LogicalPlan::Values { schema, .. } => Some(schema),
            LogicalPlan::IndexScan { schema, .. } => Some(schema),
            LogicalPlan::Insert { .. }
            | LogicalPlan::Update { .. }
            | LogicalPlan::Delete { .. }
            | LogicalPlan::CreateTable { .. }
            | LogicalPlan::DropTable { .. }
            | LogicalPlan::AlterTable { .. }
            | LogicalPlan::CreateIndex { .. }
            | LogicalPlan::DropIndex { .. }
            | LogicalPlan::Analyze { .. }
            | LogicalPlan::Empty
            | LogicalPlan::BeginTransaction
            | LogicalPlan::CommitTransaction
            | LogicalPlan::RollbackTransaction
            | LogicalPlan::Savepoint { .. }
            | LogicalPlan::ReleaseSavepoint { .. }
            | LogicalPlan::RollbackToSavepoint { .. } => None,
            LogicalPlan::Explain { input } => input.schema(),
        }
    }
}
