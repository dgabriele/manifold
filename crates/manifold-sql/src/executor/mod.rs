use crate::catalog::Catalog;
use crate::error::Result;
use crate::planner::plan::LogicalPlan;
use crate::types::Value;
use crate::ResultSet;

/// Execute a mutating statement (INSERT, UPDATE, DELETE, CREATE, DROP, etc.).
/// Returns the number of rows affected.
pub fn execute_mut(
    _db: &manifold::Database,
    _catalog: &mut Catalog,
    _plan: LogicalPlan,
    _params: &[Value],
) -> Result<u64> {
    todo!()
}

/// Execute a query statement (SELECT). Returns a result set.
pub fn execute_query(
    _db: &manifold::Database,
    _catalog: &Catalog,
    _plan: LogicalPlan,
    _params: &[Value],
) -> Result<ResultSet> {
    todo!()
}

/// Execute a mutating statement within an existing write transaction.
pub fn execute_in_txn(
    _txn: &manifold::WriteTransaction,
    _catalog: &mut Catalog,
    _plan: LogicalPlan,
    _params: &[Value],
) -> Result<u64> {
    todo!()
}

/// Execute a query within an existing write transaction.
pub fn query_in_txn(
    _txn: &manifold::WriteTransaction,
    _catalog: &Catalog,
    _plan: LogicalPlan,
    _params: &[Value],
) -> Result<ResultSet> {
    todo!()
}
