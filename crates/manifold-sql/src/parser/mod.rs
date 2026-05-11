use sqlparser::ast::Statement;
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::error::{Result, SqlError};

pub fn parse(sql: &str) -> Result<Vec<Statement>> {
    let dialect = GenericDialect {};
    Parser::parse_sql(&dialect, sql).map_err(|e| SqlError::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::ast::Statement;

    #[test]
    fn parse_create_table() {
        let stmts = parse("CREATE TABLE users (id INTEGER, name TEXT)").unwrap();
        assert_eq!(stmts.len(), 1);
        assert!(matches!(stmts[0], Statement::CreateTable(_)));
    }

    #[test]
    fn parse_insert() {
        let stmts = parse("INSERT INTO users (id, name) VALUES (1, 'Alice')").unwrap();
        assert_eq!(stmts.len(), 1);
        assert!(matches!(stmts[0], Statement::Insert(_)));
    }

    #[test]
    fn parse_select() {
        let stmts = parse("SELECT id, name FROM users WHERE id = 1").unwrap();
        assert_eq!(stmts.len(), 1);
        assert!(matches!(stmts[0], Statement::Query(_)));
    }

    #[test]
    fn parse_multiple_statements() {
        let stmts = parse("SELECT 1; SELECT 2").unwrap();
        assert_eq!(stmts.len(), 2);
        assert!(matches!(stmts[0], Statement::Query(_)));
        assert!(matches!(stmts[1], Statement::Query(_)));
    }

    #[test]
    fn parse_error() {
        // Unclosed parenthesis / truly malformed SQL
        let result = parse("SELECT (1 + FROM users");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, crate::error::SqlError::Parse(_)));
    }

    #[test]
    fn parse_parameterized() {
        // GenericDialect supports $1 placeholder syntax
        let stmts = parse("SELECT * FROM users WHERE id = $1").unwrap();
        assert_eq!(stmts.len(), 1);
        assert!(matches!(stmts[0], Statement::Query(_)));
    }
}
