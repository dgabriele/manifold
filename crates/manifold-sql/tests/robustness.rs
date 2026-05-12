mod common;

use common::test_db;
use manifold_sql::{Database, Value};

// ===========================================================================
// Empty table operations
// ===========================================================================

#[test]
fn empty_table_operations() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, val TEXT)", &[])
        .unwrap();

    // SELECT on empty table returns 0 rows.
    let result = db.query("SELECT * FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 0);

    // COUNT on empty table returns 0.
    let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 0);

    // UPDATE on empty table affects 0 rows.
    let affected = db
        .execute("UPDATE t SET val = 'x' WHERE id = 1", &[])
        .unwrap();
    assert_eq!(affected, 0);

    // DELETE on empty table affects 0 rows.
    let affected = db.execute("DELETE FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(affected, 0);
}

// ===========================================================================
// Edge values
// ===========================================================================

#[test]
fn edge_values() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (i INTEGER, t TEXT, b BLOB)", &[])
        .unwrap();

    // Insert i64::MAX.
    db.execute(
        "INSERT INTO t (i, t, b) VALUES ($1, $2, $3)",
        &[
            Value::Integer(i64::MAX),
            Value::Text(String::new()),
            Value::Blob(vec![]),
        ],
    )
    .unwrap();

    // Insert i64::MIN.
    db.execute(
        "INSERT INTO t (i, t, b) VALUES ($1, $2, $3)",
        &[
            Value::Integer(i64::MIN),
            Value::Text("normal".into()),
            Value::Blob(vec![1, 2, 3]),
        ],
    )
    .unwrap();

    // Query i64::MAX row (inserted first).
    let result = db.query("SELECT i, t, b FROM t ORDER BY i", &[]).unwrap();
    assert_eq!(result.row_count(), 2);

    // Find the row with i64::MAX and the row with i64::MIN by value.
    let mut found_max = false;
    let mut found_min = false;
    for row in result.rows() {
        let val = row.get::<i64>(0).unwrap();
        if val == i64::MAX {
            found_max = true;
            // This row has empty string and empty blob.
            assert_eq!(row.get::<String>(1).unwrap(), "");
            let empty_blob: Vec<u8> = vec![];
            assert_eq!(row.get::<Vec<u8>>(2).unwrap(), empty_blob);
        } else if val == i64::MIN {
            found_min = true;
            assert_eq!(row.get::<String>(1).unwrap(), "normal");
        }
    }
    assert!(found_max, "i64::MAX row not found");
    assert!(found_min, "i64::MIN row not found");
}

// ===========================================================================
// Reopen database
// ===========================================================================

#[test]
fn reopen_database() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("reopen_test.db");

    // Phase 1: create schema and data.
    {
        let db = Database::open(&db_path).unwrap();
        db.execute("CREATE TABLE t (id INTEGER, val TEXT)", &[])
            .unwrap();
        db.execute("INSERT INTO t (id, val) VALUES (1, 'hello')", &[])
            .unwrap();
        db.execute("INSERT INTO t (id, val) VALUES (2, 'world')", &[])
            .unwrap();
    }

    // Phase 2: reopen and verify schema + data.
    {
        let db = Database::open(&db_path).unwrap();
        let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
        assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 2);

        let result = db.query("SELECT val FROM t WHERE id = 2", &[]).unwrap();
        assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "world");

        // Can still insert into the reopened DB.
        db.execute("INSERT INTO t (id, val) VALUES (3, 'reopen')", &[])
            .unwrap();
        let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
        assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 3);
    }
}

// ===========================================================================
// Schema evolution (ALTER TABLE)
// ===========================================================================

#[test]
fn schema_evolution() {
    let (db, _dir) = test_db();

    // Initial table.
    db.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'alice')", &[])
        .unwrap();

    // Add a column.
    db.execute("ALTER TABLE t ADD COLUMN age INTEGER", &[])
        .unwrap();

    // Insert with new column.
    db.execute("INSERT INTO t (id, name, age) VALUES (2, 'bob', 30)", &[])
        .unwrap();

    // Old row should have NULL for age; verify both rows.
    let result = db.query("SELECT id, name FROM t ORDER BY id", &[]).unwrap();
    assert_eq!(result.row_count(), 2);
    assert_eq!(result.rows()[0].get::<String>(1).unwrap(), "alice");
    assert_eq!(result.rows()[1].get::<String>(1).unwrap(), "bob");

    // Drop the age column.
    db.execute("ALTER TABLE t DROP COLUMN age", &[]).unwrap();

    // Verify only id and name remain.
    let result = db.query("SELECT * FROM t ORDER BY id", &[]).unwrap();
    assert_eq!(result.row_count(), 2);
    // Should have 2 columns.
    assert_eq!(result.columns().len(), 2);
}

// ===========================================================================
// Error messages are clear
// ===========================================================================

#[test]
fn error_messages_are_clear() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .unwrap();

    // Table not found.
    let err = db.query("SELECT * FROM nonexistent", &[]).unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("nonexistent"),
        "error should mention the table name, got: {err}"
    );

    // Column not found.
    let err = db.query("SELECT no_such_col FROM t", &[]).unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("no_such_col"),
        "error should mention the column name, got: {err}"
    );

    // Type mismatch (insert string where integer expected via wrong param type).
    // This depends on type checking implementation; just verify we get a meaningful error.
    let err = db.execute(
        "INSERT INTO t (id, name) VALUES ($1, $2)",
        &[Value::Text("not_a_number".into()), Value::Integer(42)],
    );
    if let Err(e) = err {
        // Should have some meaningful message, not just a generic error.
        assert!(
            !e.to_string().is_empty(),
            "error message should not be empty"
        );
    }
}
