pub mod expr;
pub mod statement;

use sqlparser::ast::Statement;

use crate::catalog::schema::{ColumnDef, ConstraintDef, TableId};
use crate::catalog::Catalog;
use crate::error::Result;
use crate::types::{SqlType, Value};

// ---------------------------------------------------------------------------
// Core binder types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ColumnRef {
    pub table_id: TableId,
    pub table_name: String,
    pub column_index: usize,
    pub column_name: String,
    pub sql_type: SqlType,
    pub nullable: bool,
}

#[derive(Debug, Clone)]
pub enum BoundExpr {
    Column(ColumnRef),
    Literal(Value),
    Parameter(usize), // 0-based index
    BinaryOp {
        op: BinaryOp,
        left: Box<BoundExpr>,
        right: Box<BoundExpr>,
        result_type: SqlType,
    },
    UnaryOp {
        op: UnaryOp,
        operand: Box<BoundExpr>,
        result_type: SqlType,
    },
    IsNull {
        operand: Box<BoundExpr>,
        negated: bool,
    },
    InList {
        expr: Box<BoundExpr>,
        list: Vec<BoundExpr>,
        negated: bool,
    },
    Between {
        expr: Box<BoundExpr>,
        low: Box<BoundExpr>,
        high: Box<BoundExpr>,
        negated: bool,
    },
    Like {
        expr: Box<BoundExpr>,
        pattern: Box<BoundExpr>,
        negated: bool,
    },
    Function {
        name: String,
        args: Vec<BoundExpr>,
        result_type: SqlType,
    },
    Aggregate {
        func: AggregateFunc,
        arg: Option<Box<BoundExpr>>,
        distinct: bool,
        result_type: SqlType,
    },
    Cast {
        expr: Box<BoundExpr>,
        target_type: SqlType,
    },
    Wildcard,
}

impl BoundExpr {
    /// Returns a human-readable name for this expression.
    pub fn display_name(&self) -> String {
        match self {
            BoundExpr::Column(col) => col.column_name.clone(),
            BoundExpr::Literal(val) => format!("{val}"),
            BoundExpr::Parameter(idx) => format!("${}", idx + 1),
            BoundExpr::BinaryOp { op, left, right, .. } => {
                format!("({} {} {})", left.display_name(), op.as_str(), right.display_name())
            }
            BoundExpr::UnaryOp { op, operand, .. } => {
                format!("({}{})", op.as_str(), operand.display_name())
            }
            BoundExpr::IsNull { operand, negated } => {
                if *negated {
                    format!("({} IS NOT NULL)", operand.display_name())
                } else {
                    format!("({} IS NULL)", operand.display_name())
                }
            }
            BoundExpr::InList { expr, negated, .. } => {
                let not = if *negated { "NOT " } else { "" };
                format!("({} {}IN (...))", expr.display_name(), not)
            }
            BoundExpr::Between { expr, negated, .. } => {
                let not = if *negated { "NOT " } else { "" };
                format!("({} {}BETWEEN ...)", expr.display_name(), not)
            }
            BoundExpr::Like { expr, negated, .. } => {
                let not = if *negated { "NOT " } else { "" };
                format!("({} {}LIKE ...)", expr.display_name(), not)
            }
            BoundExpr::Function { name, .. } => format!("{name}(...)"),
            BoundExpr::Aggregate { func, .. } => format!("{}(...)", func.as_str()),
            BoundExpr::Cast { expr, target_type } => {
                format!("CAST({} AS {target_type})", expr.display_name())
            }
            BoundExpr::Wildcard => "*".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Neq,
    Lt,
    Gt,
    Lte,
    Gte,
    And,
    Or,
}

impl BinaryOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Mod => "%",
            BinaryOp::Eq => "=",
            BinaryOp::Neq => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::Gt => ">",
            BinaryOp::Lte => "<=",
            BinaryOp::Gte => ">=",
            BinaryOp::And => "AND",
            BinaryOp::Or => "OR",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum UnaryOp {
    Neg,
    Not,
}

impl UnaryOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            UnaryOp::Neg => "-",
            UnaryOp::Not => "NOT ",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum AggregateFunc {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

impl AggregateFunc {
    pub fn as_str(&self) -> &'static str {
        match self {
            AggregateFunc::Count => "COUNT",
            AggregateFunc::Sum => "SUM",
            AggregateFunc::Avg => "AVG",
            AggregateFunc::Min => "MIN",
            AggregateFunc::Max => "MAX",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinType {
    Inner,
    Left,
    Right,
    Cross,
}

// ---------------------------------------------------------------------------
// Bound statement types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct BoundSelect {
    pub from: Vec<BoundTableRef>,
    pub joins: Vec<BoundJoin>,
    pub filter: Option<BoundExpr>,
    pub projection: Vec<BoundSelectItem>,
    pub group_by: Vec<BoundExpr>,
    pub having: Option<BoundExpr>,
    pub order_by: Vec<BoundOrderBy>,
    pub limit: Option<BoundExpr>,
    pub offset: Option<BoundExpr>,
    pub distinct: bool,
}

#[derive(Debug, Clone)]
pub struct BoundTableRef {
    pub table_id: TableId,
    pub table_name: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BoundJoin {
    pub join_type: JoinType,
    pub table: BoundTableRef,
    pub condition: Option<BoundExpr>,
}

#[derive(Debug, Clone)]
pub struct BoundSelectItem {
    pub expr: BoundExpr,
    pub alias: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BoundOrderBy {
    pub expr: BoundExpr,
    pub asc: bool,
    pub nulls_first: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct BoundAssignment {
    pub column_index: usize,
    pub value: BoundExpr,
}

#[derive(Debug, Clone)]
pub enum InsertSource {
    Values(Vec<Vec<BoundExpr>>),
    Select(BoundSelect),
}

#[derive(Debug, Clone)]
pub enum AlterTableOp {
    AddColumn(ColumnDef),
    DropColumn(String),
    RenameColumn { old: String, new: String },
}

#[derive(Debug, Clone)]
pub enum BoundStatement {
    Select(BoundSelect),
    Insert {
        table_id: TableId,
        table_name: String,
        columns: Vec<usize>,
        source: InsertSource,
    },
    Update {
        table_id: TableId,
        table_name: String,
        assignments: Vec<BoundAssignment>,
        filter: Option<BoundExpr>,
    },
    Delete {
        table_id: TableId,
        table_name: String,
        filter: Option<BoundExpr>,
    },
    CreateTable {
        name: String,
        columns: Vec<ColumnDef>,
        constraints: Vec<ConstraintDef>,
        if_not_exists: bool,
    },
    DropTable {
        name: String,
        if_exists: bool,
    },
    AlterTable {
        table_name: String,
        operation: AlterTableOp,
    },
    CreateIndex {
        index_name: String,
        table_name: String,
        columns: Vec<String>,
        unique: bool,
        if_not_exists: bool,
    },
    DropIndex {
        name: String,
        if_exists: bool,
    },
    Union {
        left: Box<BoundStatement>,
        right: Box<BoundStatement>,
        all: bool,
    },
    Explain(Box<BoundStatement>),
    Analyze {
        table_name: Option<String>,
    },
    BeginTransaction,
    CommitTransaction,
    RollbackTransaction,
    Savepoint { name: String },
    ReleaseSavepoint { name: String },
    RollbackToSavepoint { name: String },
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn bind(
    catalog: &Catalog,
    stmt: &Statement,
    params: &[Value],
) -> Result<BoundStatement> {
    statement::bind_statement(catalog, stmt, params)
}
