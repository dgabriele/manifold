mod common;

use common::test_db;
use manifold_sql::Value;

// ===========================================================================
// INSERT
// ===========================================================================

#[test]
fn insert_single_row_all_values() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT, active BOOLEAN)", &[])
        .unwrap();
    db.execute(
        "INSERT INTO t (id, name, active) VALUES (1, 'alice', TRUE)",
        &[],
    )
    .unwrap();
    let result = db.query("SELECT id, name, active FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);
    assert_eq!(result.rows()[0].get::<String>(1).unwrap(), "alice");
    assert_eq!(result.rows()[0].get::<bool>(2).unwrap(), true);
}

#[test]
fn insert_multi_row() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .unwrap();
    db.execute(
        "INSERT INTO t (id, name) VALUES (1, 'a'), (2, 'b'), (3, 'c')",
        &[],
    )
    .unwrap();
    let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 3);
}

#[test]
fn insert_subset_of_columns_others_null() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT, age INTEGER)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'alice')", &[])
        .unwrap();
    let result = db.query("SELECT age FROM t WHERE id = 1", &[]).unwrap();
    let age_val = result.rows()[0].get::<Value>(0).unwrap();
    assert_eq!(age_val, Value::Null);
}

#[test]
fn insert_with_default_values() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER, status TEXT DEFAULT 'pending')",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO t (id) VALUES (1)", &[]).unwrap();
    let result = db.query("SELECT status FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "pending");
}

#[test]
fn insert_with_parameters() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .unwrap();
    db.execute(
        "INSERT INTO t (id, name) VALUES ($1, $2)",
        &[Value::Integer(42), Value::Text("param_test".into())],
    )
    .unwrap();
    let result = db.query("SELECT id, name FROM t", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 42);
    assert_eq!(result.rows()[0].get::<String>(1).unwrap(), "param_test");
}

// ===========================================================================
// UPDATE
// ===========================================================================

#[test]
fn update_with_where_clause() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, val TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, 'old')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (2, 'keep')", &[])
        .unwrap();

    let affected = db
        .execute("UPDATE t SET val = 'new' WHERE id = 1", &[])
        .unwrap();
    assert_eq!(affected, 1);

    let result = db.query("SELECT val FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "new");
    let result = db.query("SELECT val FROM t WHERE id = 2", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "keep");
}

#[test]
fn update_multiple_columns() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, a TEXT, b TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, a, b) VALUES (1, 'x', 'y')", &[])
        .unwrap();

    db.execute("UPDATE t SET a = 'A', b = 'B' WHERE id = 1", &[])
        .unwrap();

    let result = db.query("SELECT a, b FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "A");
    assert_eq!(result.rows()[0].get::<String>(1).unwrap(), "B");
}

#[test]
fn update_all_rows_no_where() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, val TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, 'a')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (2, 'b')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (3, 'c')", &[])
        .unwrap();

    let affected = db.execute("UPDATE t SET val = 'z'", &[]).unwrap();
    assert_eq!(affected, 3);

    let result = db.query("SELECT DISTINCT val FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "z");
}

#[test]
fn update_with_arithmetic_expression() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, count INTEGER)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, count) VALUES (1, 10)", &[])
        .unwrap();

    db.execute("UPDATE t SET count = count + 1 WHERE id = 1", &[])
        .unwrap();

    let result = db.query("SELECT count FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 11);
}

// ===========================================================================
// DELETE
// ===========================================================================

#[test]
fn delete_with_where_clause() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, val TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, 'a')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (2, 'b')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (3, 'c')", &[])
        .unwrap();

    let affected = db.execute("DELETE FROM t WHERE id = 2", &[]).unwrap();
    assert_eq!(affected, 1);

    let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 2);
}

#[test]
fn delete_all_rows_no_where() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();
    db.execute("INSERT INTO t (id) VALUES (1)", &[]).unwrap();
    db.execute("INSERT INTO t (id) VALUES (2)", &[]).unwrap();
    db.execute("INSERT INTO t (id) VALUES (3)", &[]).unwrap();

    let affected = db.execute("DELETE FROM t", &[]).unwrap();
    assert_eq!(affected, 3);

    let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 0);
}

#[test]
fn delete_returns_correct_affected_count() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, group_id INTEGER)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, group_id) VALUES (1, 1)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, group_id) VALUES (2, 1)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, group_id) VALUES (3, 2)", &[])
        .unwrap();

    let affected = db.execute("DELETE FROM t WHERE group_id = 1", &[]).unwrap();
    assert_eq!(affected, 2);
}
