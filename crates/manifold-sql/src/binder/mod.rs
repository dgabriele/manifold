pub mod expr;
pub mod statement;

use sqlparser::ast::Statement;

use crate::catalog::Catalog;
use crate::error::Result;
use crate::types::Value;

pub enum BoundStatement {}

pub fn bind(
    _catalog: &Catalog,
    _stmt: &Statement,
    _params: &[Value],
) -> Result<BoundStatement> {
    todo!()
}
