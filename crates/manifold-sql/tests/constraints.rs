mod common;

use common::test_db;
use manifold_sql::Value;

// ===========================================================================
// Primary key
// ===========================================================================

#[test]
fn primary_key_uniqueness() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, v) VALUES (1, 'first')", &[])
        .unwrap();

    let err = db
        .execute("INSERT INTO t (id, v) VALUES (1, 'duplicate')", &[])
        .unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("unique")
            || msg.contains("duplicate")
            || msg.contains("constraint")
            || msg.contains("primary"),
        "expected PK uniqueness error, got: {err}"
    );
}

#[test]
fn primary_key_auto_increment() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .unwrap();

    // Omitting the PK column should auto-assign a rowid.
    db.execute("INSERT INTO t (v) VALUES ('a')", &[]).unwrap();
    db.execute("INSERT INTO t (v) VALUES ('b')", &[]).unwrap();

    let result = db.query("SELECT id, v FROM t ORDER BY id", &[]).unwrap();
    assert_eq!(result.row_count(), 2);
    let id1 = result.rows()[0].get::<i64>(0).unwrap();
    let id2 = result.rows()[1].get::<i64>(0).unwrap();
    assert_ne!(id1, id2, "auto-generated PKs should be distinct");
}

// ===========================================================================
// NOT NULL
// ===========================================================================

#[test]
fn not_null_insert() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        &[],
    )
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
fn not_null_with_default() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, status TEXT NOT NULL DEFAULT 'active')",
        &[],
    )
    .unwrap();

    // Inserting without specifying the NOT NULL column should use the default.
    db.execute("INSERT INTO t (id) VALUES (1)", &[]).unwrap();

    let result = db.query("SELECT status FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "active");
}

// ===========================================================================
// UNIQUE
// ===========================================================================

#[test]
fn unique_single_column() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, email TEXT UNIQUE)",
        &[],
    )
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
fn unique_allows_nulls() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, code TEXT UNIQUE)",
        &[],
    )
    .unwrap();

    // Per SQL standard, multiple NULLs in a UNIQUE column should be allowed.
    db.execute("INSERT INTO t (id) VALUES (1)", &[]).unwrap();
    db.execute("INSERT INTO t (id) VALUES (2)", &[]).unwrap();

    let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 2);
}

// ===========================================================================
// Foreign keys
// ===========================================================================

#[test]
fn foreign_key_insert_orphan() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE parents (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE children (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parents(id))",
        &[],
    )
    .unwrap();

    // Inserting a child with a non-existent parent should fail.
    let err = db
        .execute("INSERT INTO children (id, parent_id) VALUES (1, 999)", &[])
        .unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("foreign") || msg.contains("constraint") || msg.contains("reference"),
        "expected FK error, got: {err}"
    );
}

#[test]
fn foreign_key_restrict() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE parents (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE children (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parents(id) ON DELETE RESTRICT)",
        &[],
    )
    .unwrap();

    db.execute("INSERT INTO parents (id, name) VALUES (1, 'mom')", &[])
        .unwrap();
    db.execute("INSERT INTO children (id, parent_id) VALUES (1, 1)", &[])
        .unwrap();

    // Deleting a parent with children should fail under RESTRICT.
    let err = db
        .execute("DELETE FROM parents WHERE id = 1", &[])
        .unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("foreign") || msg.contains("constraint") || msg.contains("restrict"),
        "expected FK RESTRICT error, got: {err}"
    );
}

#[test]
fn foreign_key_cascade() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE parents (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE children (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parents(id) ON DELETE CASCADE)",
        &[],
    )
    .unwrap();

    db.execute("INSERT INTO parents (id, name) VALUES (1, 'dad')", &[])
        .unwrap();
    db.execute("INSERT INTO children (id, parent_id) VALUES (1, 1)", &[])
        .unwrap();
    db.execute("INSERT INTO children (id, parent_id) VALUES (2, 1)", &[])
        .unwrap();

    // Deleting parent should cascade to children.
    db.execute("DELETE FROM parents WHERE id = 1", &[]).unwrap();

    let result = db.query("SELECT COUNT(*) FROM children", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 0);
}

#[test]
fn foreign_key_set_null() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE parents (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE children (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parents(id) ON DELETE SET NULL)",
        &[],
    )
    .unwrap();

    db.execute("INSERT INTO parents (id, name) VALUES (1, 'mom')", &[])
        .unwrap();
    db.execute("INSERT INTO children (id, parent_id) VALUES (1, 1)", &[])
        .unwrap();

    // Deleting parent should set FK column to NULL.
    db.execute("DELETE FROM parents WHERE id = 1", &[]).unwrap();

    let result = db
        .query("SELECT parent_id FROM children WHERE id = 1", &[])
        .unwrap();
    assert_eq!(result.row_count(), 1);
    let val = result.rows()[0].get::<Value>(0).unwrap();
    assert_eq!(val, Value::Null);
}

// ===========================================================================
// Type enforcement (strict mode)
// ===========================================================================

#[test]
fn type_enforcement_strict() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, count INTEGER)",
        &[],
    )
    .unwrap();

    // Text in an integer column should fail.
    let err = db
        .execute("INSERT INTO t (id, count) VALUES (1, 'not a number')", &[])
        .unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("type"),
        "expected type error, got: {err}"
    );
}

#[test]
fn varchar_length() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, code VARCHAR(3))",
        &[],
    )
    .unwrap();

    // Within limit.
    db.execute("INSERT INTO t (id, code) VALUES (1, 'abc')", &[])
        .unwrap();

    // Exceeds limit.
    let err = db
        .execute("INSERT INTO t (id, code) VALUES (2, 'abcdef')", &[])
        .unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("varchar") || msg.contains("exceeds") || msg.contains("constraint") || msg.contains("length"),
        "expected VARCHAR length error, got: {err}"
    );
}
