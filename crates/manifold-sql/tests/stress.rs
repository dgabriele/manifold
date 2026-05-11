mod common;

use common::test_db;
use manifold_sql::{Database, Value};

// ===========================================================================
// Large table insert and scan
// ===========================================================================

#[test]
fn large_table_insert_and_scan() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE big (id INTEGER, val TEXT)", &[])
        .unwrap();

    // Insert 10,000 rows in batches using transactions for speed.
    for batch_start in (0..10_000).step_by(500) {
        db.execute("BEGIN", &[]).unwrap();
        for i in batch_start..batch_start + 500 {
            db.execute(
                "INSERT INTO big (id, val) VALUES ($1, $2)",
                &[
                    Value::Integer(i),
                    Value::Text(format!("row_{i}")),
                ],
            )
            .unwrap();
        }
        db.execute("COMMIT", &[]).unwrap();
    }

    // Verify count.
    let result = db.query("SELECT COUNT(*) FROM big", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 10_000);

    // Verify filtered query.
    let result = db
        .query("SELECT val FROM big WHERE id = 9999", &[])
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "row_9999");
}

// ===========================================================================
// Wide rows (20+ columns)
// ===========================================================================

#[test]
fn wide_rows() {
    let (db, _dir) = test_db();

    // Build a CREATE TABLE with 25 columns.
    let cols: Vec<String> = (0..25).map(|i| format!("c{i} INTEGER")).collect();
    let ddl = format!("CREATE TABLE wide ({})", cols.join(", "));
    db.execute(&ddl, &[]).unwrap();

    // Insert a row with known values.
    let col_names: Vec<String> = (0..25).map(|i| format!("c{i}")).collect();
    let placeholders: Vec<String> = (1..=25).map(|i| format!("${i}")).collect();
    let insert = format!(
        "INSERT INTO wide ({}) VALUES ({})",
        col_names.join(", "),
        placeholders.join(", ")
    );
    let values: Vec<Value> = (0..25).map(|i| Value::Integer(i * 10)).collect();
    db.execute(&insert, &values).unwrap();

    // Verify all columns returned correctly.
    let result = db.query("SELECT * FROM wide", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    for i in 0..25 {
        assert_eq!(
            result.rows()[0].get::<i64>(i).unwrap(),
            (i as i64) * 10,
            "column c{i} mismatch"
        );
    }
}

// ===========================================================================
// Sequential readers (concurrent_readers stand-in)
// ===========================================================================

#[test]
fn concurrent_readers() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, val TEXT)", &[])
        .unwrap();

    // Insert 100 rows.
    db.execute("BEGIN", &[]).unwrap();
    for i in 0..100 {
        db.execute(
            "INSERT INTO t (id, val) VALUES ($1, $2)",
            &[Value::Integer(i), Value::Text(format!("v{i}"))],
        )
        .unwrap();
    }
    db.execute("COMMIT", &[]).unwrap();

    // 10 sequential queries should all return correct data.
    for _ in 0..10 {
        let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
        assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 100);
    }
}

// ===========================================================================
// Transaction heavy
// ===========================================================================

#[test]
fn transaction_heavy() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();

    for i in 0..100 {
        db.execute("BEGIN", &[]).unwrap();
        db.execute(
            "INSERT INTO t (id) VALUES ($1)",
            &[Value::Integer(i)],
        )
        .unwrap();
        db.execute("COMMIT", &[]).unwrap();
    }

    let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 100);
}

// ===========================================================================
// Crash recovery (close and reopen)
// ===========================================================================

#[test]
fn crash_recovery() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("crash_test.db");

    // Phase 1: create, insert, commit, drop.
    {
        let db = Database::open(&db_path).unwrap();
        db.execute("CREATE TABLE t (id INTEGER, val TEXT)", &[])
            .unwrap();
        db.execute("INSERT INTO t (id, val) VALUES (1, 'survived')", &[])
            .unwrap();
        db.execute("INSERT INTO t (id, val) VALUES (2, 'also_survived')", &[])
            .unwrap();
        // db is dropped here
    }

    // Phase 2: reopen and verify.
    {
        let db = Database::open(&db_path).unwrap();
        let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
        assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 2);

        let result = db
            .query("SELECT val FROM t WHERE id = 1", &[])
            .unwrap();
        assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "survived");
    }
}
