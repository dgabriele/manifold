mod common;

use common::test_db;
use manifold_sql::Value;

// ===========================================================================
// CREATE TABLE
// ===========================================================================

#[test]
fn create_table_all_column_types() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE all_types (
            b BOOLEAN,
            si SMALLINT,
            i INTEGER,
            bi BIGINT,
            r REAL,
            d DECIMAL(10, 2),
            t TEXT,
            v VARCHAR(255),
            bl BLOB,
            u UUID,
            dt DATE,
            ts TIMESTAMP,
            tstz TIMESTAMP WITH TIME ZONE,
            j JSON
        )",
        &[],
    )
    .unwrap();

    // Verify the table exists by inserting and querying.
    let result = db.query("SELECT COUNT(*) FROM all_types", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 0);
}

#[test]
fn create_table_if_not_exists_no_error_on_duplicate() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();
    // Should not error.
    db.execute("CREATE TABLE IF NOT EXISTS t (id INTEGER)", &[])
        .unwrap();
}

#[test]
fn create_table_with_primary_key() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'a')", &[])
        .unwrap();
    // Duplicate PK should error.
    let err = db
        .execute("INSERT INTO t (id, name) VALUES (1, 'b')", &[])
        .unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("unique")
            || msg.contains("primary")
            || msg.contains("duplicate")
            || msg.contains("constraint"),
        "expected PK violation error, got: {err}"
    );
}

#[test]
fn create_table_with_not_null_constraint() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT NOT NULL)", &[])
        .unwrap();
    let err = db
        .execute("INSERT INTO t (id) VALUES (1)", &[])
        .unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("not null") || msg.contains("null") || msg.contains("constraint"),
        "expected NOT NULL error, got: {err}"
    );
}

#[test]
fn create_table_with_unique_constraint() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, email TEXT UNIQUE)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, email) VALUES (1, 'a@b.com')", &[])
        .unwrap();
    let err = db
        .execute("INSERT INTO t (id, email) VALUES (2, 'a@b.com')", &[])
        .unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("unique") || msg.contains("duplicate") || msg.contains("constraint"),
        "expected UNIQUE error, got: {err}"
    );
}

#[test]
fn create_table_with_default_value() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER, status TEXT DEFAULT 'active')",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO t (id) VALUES (1)", &[]).unwrap();
    let result = db.query("SELECT status FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "active");
}

#[test]
fn create_table_with_composite_primary_key() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (a INTEGER, b INTEGER, c TEXT, PRIMARY KEY (a, b))",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO t (a, b, c) VALUES (1, 1, 'x')", &[])
        .unwrap();
    db.execute("INSERT INTO t (a, b, c) VALUES (1, 2, 'y')", &[])
        .unwrap();
    // Duplicate composite key should error.
    let err = db
        .execute("INSERT INTO t (a, b, c) VALUES (1, 1, 'z')", &[])
        .unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("unique")
            || msg.contains("primary")
            || msg.contains("duplicate")
            || msg.contains("constraint"),
        "expected composite PK violation, got: {err}"
    );
}

#[test]
fn create_table_already_exists_errors() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();
    let err = db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("exists") || msg.contains("already"),
        "expected 'already exists' error, got: {err}"
    );
}

// ===========================================================================
// DROP TABLE
// ===========================================================================

#[test]
fn drop_existing_table() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();
    db.execute("DROP TABLE t", &[]).unwrap();
    // Table should be gone — creating again should succeed.
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();
}

#[test]
fn drop_if_exists_nonexistent_no_error() {
    let (db, _dir) = test_db();
    db.execute("DROP TABLE IF EXISTS nonexistent", &[]).unwrap();
}

#[test]
fn drop_nonexistent_table_errors() {
    let (db, _dir) = test_db();
    let err = db.execute("DROP TABLE nonexistent", &[]).unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("not found") || msg.contains("does not exist") || msg.contains("unknown"),
        "expected table-not-found error, got: {err}"
    );
}

// ===========================================================================
// ALTER TABLE
// ===========================================================================

#[test]
fn alter_table_add_column() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'alice')", &[])
        .unwrap();

    db.execute("ALTER TABLE t ADD COLUMN age INTEGER", &[])
        .unwrap();

    // Existing data should be preserved; new column should be NULL.
    let result = db.query("SELECT id, name, age FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);
    assert_eq!(result.rows()[0].get::<String>(1).unwrap(), "alice");
    let age_val = result.rows()[0].get::<Value>(2).unwrap();
    assert_eq!(age_val, Value::Null);
}

#[test]
fn alter_table_drop_column() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT, age INTEGER)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name, age) VALUES (1, 'alice', 30)", &[])
        .unwrap();

    db.execute("ALTER TABLE t DROP COLUMN age", &[]).unwrap();

    let result = db.query("SELECT id, name FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);
    assert_eq!(result.rows()[0].get::<String>(1).unwrap(), "alice");
}

#[test]
fn alter_table_rename_column() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'alice')", &[])
        .unwrap();

    db.execute("ALTER TABLE t RENAME COLUMN name TO full_name", &[])
        .unwrap();

    let result = db
        .query("SELECT full_name FROM t WHERE id = 1", &[])
        .unwrap();
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "alice");
}

// ===========================================================================
// CREATE INDEX / DROP INDEX
// ===========================================================================

#[test]
fn create_nonunique_index() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .unwrap();
    db.execute("CREATE INDEX idx_name ON t (name)", &[])
        .unwrap();
    // Should still work for queries.
    db.execute("INSERT INTO t (id, name) VALUES (1, 'a')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (2, 'a')", &[])
        .unwrap(); // duplicates OK
    let result = db
        .query("SELECT COUNT(*) FROM t WHERE name = 'a'", &[])
        .unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 2);
}

#[test]
fn create_unique_index_enforces_uniqueness() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, code TEXT)", &[])
        .unwrap();
    db.execute("CREATE UNIQUE INDEX idx_code ON t (code)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, code) VALUES (1, 'abc')", &[])
        .unwrap();
    let err = db
        .execute("INSERT INTO t (id, code) VALUES (2, 'abc')", &[])
        .unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("unique") || msg.contains("duplicate") || msg.contains("constraint"),
        "expected unique index violation, got: {err}"
    );
}

#[test]
fn create_composite_index() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (a INTEGER, b INTEGER, c TEXT)", &[])
        .unwrap();
    db.execute("CREATE INDEX idx_ab ON t (a, b)", &[]).unwrap();
    db.execute("INSERT INTO t (a, b, c) VALUES (1, 2, 'x')", &[])
        .unwrap();
    let result = db
        .query("SELECT c FROM t WHERE a = 1 AND b = 2", &[])
        .unwrap();
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "x");
}

#[test]
fn create_index_if_not_exists() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .unwrap();
    db.execute("CREATE INDEX idx_name ON t (name)", &[])
        .unwrap();
    // Should not error.
    db.execute("CREATE INDEX IF NOT EXISTS idx_name ON t (name)", &[])
        .unwrap();
}

#[test]
fn drop_index() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .unwrap();
    db.execute("CREATE INDEX idx_name ON t (name)", &[])
        .unwrap();
    db.execute("DROP INDEX idx_name", &[]).unwrap();
}
