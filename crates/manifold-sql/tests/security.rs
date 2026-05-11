mod common;

use common::test_db;
use manifold_sql::Value;

// ===========================================================================
// Parameterized queries prevent injection
// ===========================================================================

#[test]
fn parameterized_prevents_injection() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE users (id INTEGER, name TEXT)", &[])
        .unwrap();

    // Insert a value that looks like SQL injection.
    let evil = "'; DROP TABLE users; --";
    db.execute(
        "INSERT INTO users (id, name) VALUES ($1, $2)",
        &[Value::Integer(1), Value::Text(evil.into())],
    )
    .unwrap();

    // Table should still exist and contain the literal string.
    let result = db.query("SELECT name FROM users WHERE id = 1", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), evil);

    // Verify the table was not dropped.
    let result = db.query("SELECT COUNT(*) FROM users", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);
}

// ===========================================================================
// Malformed SQL returns errors, not panics
// ===========================================================================

#[test]
fn malformed_sql_error() {
    let (db, _dir) = test_db();

    let bad_sqls = [
        "SELEC * FROM t",
        "INSERT ANTO t VALUES (1)",
        "CREATE TABEL t (id INT)",
        "SELECT * FORM t",
        "DROP TABEL t",
        ";;;",
    ];

    for sql in &bad_sqls {
        let exec_result = db.execute(sql, &[]);
        let query_result = db.query(sql, &[]);
        assert!(
            exec_result.is_err() || query_result.is_err(),
            "expected error for malformed SQL: {sql}"
        );
    }
}

// ===========================================================================
// Empty SQL
// ===========================================================================

#[test]
fn empty_sql() {
    let (db, _dir) = test_db();

    // Empty string.
    let result = db.execute("", &[]);
    assert!(result.is_err(), "empty SQL should error");

    // Whitespace only.
    let result = db.execute("   ", &[]);
    assert!(result.is_err(), "whitespace-only SQL should error");

    // Same for query.
    let result = db.query("", &[]);
    assert!(result.is_err(), "empty SQL query should error");
}

// ===========================================================================
// Huge query (1000 column names)
// ===========================================================================

#[test]
fn huge_query() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();

    // Build a SELECT with 1000 expressions.
    let cols: Vec<String> = (0..1000).map(|i| format!("{i}")).collect();
    let sql = format!("SELECT {}", cols.join(", "));

    // Should parse without panicking. May succeed or error gracefully.
    let result = db.query(&sql, &[]);
    // We only care that it doesn't panic; either Ok or Err is acceptable.
    let _ = result;
}

// ===========================================================================
// Deeply nested expressions
// ===========================================================================

#[test]
fn deeply_nested_expressions() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();
    db.execute("INSERT INTO t (id) VALUES (1)", &[]).unwrap();

    // Build ((((1+1)+1)+1)...+1) nested 50 levels deep.
    let mut expr = String::from("1");
    for _ in 0..50 {
        expr = format!("({expr}+1)");
    }
    let sql = format!("SELECT {expr} FROM t");

    // Should not panic. May succeed or error gracefully.
    let result = db.query(&sql, &[]);
    if let Ok(rs) = result {
        // If it succeeds, the answer should be 51.
        assert_eq!(rs.rows()[0].get::<i64>(0).unwrap(), 51);
    }
}

// ===========================================================================
// Large IN list
// ===========================================================================

#[test]
fn large_in_list() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();

    // Insert a few rows.
    for i in 0..10 {
        db.execute(
            "INSERT INTO t (id) VALUES ($1)",
            &[Value::Integer(i)],
        )
        .unwrap();
    }

    // Build WHERE id IN (1, 2, 3, ..., 1000).
    let items: Vec<String> = (1..=1000).map(|i| i.to_string()).collect();
    let sql = format!("SELECT COUNT(*) FROM t WHERE id IN ({})", items.join(", "));

    // Should handle gracefully — not panic.
    let result = db.query(&sql, &[]);
    if let Ok(rs) = result {
        // ids 1..=9 are in both the table and the IN list.
        assert_eq!(rs.rows()[0].get::<i64>(0).unwrap(), 9);
    }
}
