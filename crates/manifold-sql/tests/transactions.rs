mod common;

use common::test_db;

// ===========================================================================
// basic: BEGIN + INSERT + COMMIT / ROLLBACK
// ===========================================================================

#[test]
fn begin_insert_commit_visible() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .unwrap();

    db.execute("BEGIN", &[]).unwrap();
    db.execute("INSERT INTO t (id, v) VALUES (1, 'committed')", &[])
        .unwrap();
    db.execute("COMMIT", &[]).unwrap();

    let result = db.query("SELECT v FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "committed");
}

#[test]
fn begin_insert_rollback_invisible() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .unwrap();

    db.execute("BEGIN", &[]).unwrap();
    db.execute("INSERT INTO t (id, v) VALUES (1, 'rolled_back')", &[])
        .unwrap();
    db.execute("ROLLBACK", &[]).unwrap();

    let result = db.query("SELECT v FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 0);
}

// ===========================================================================
// isolation: uncommitted data visible within txn, not outside
// ===========================================================================

#[test]
fn isolation_visible_within_txn() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .unwrap();

    db.execute("BEGIN", &[]).unwrap();
    db.execute("INSERT INTO t (id, v) VALUES (1, 'pending')", &[])
        .unwrap();

    // Query within the same active transaction should see the uncommitted row.
    let result = db.query("SELECT v FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "pending");

    db.execute("ROLLBACK", &[]).unwrap();

    // After rollback, row should be gone.
    let result = db.query("SELECT v FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 0);
}

// ===========================================================================
// savepoints
// ===========================================================================

#[test]
fn savepoint_rollback_to() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .unwrap();
    // Insert before BEGIN so it's committed.
    db.execute("INSERT INTO t (id, v) VALUES (1, 'before_txn')", &[])
        .unwrap();

    db.execute("BEGIN", &[]).unwrap();
    // Savepoint must be created before modifications within the txn
    // (ephemeral_savepoint limitation).
    db.execute("SAVEPOINT sp1", &[]).unwrap();
    db.execute("INSERT INTO t (id, v) VALUES (2, 'after_sp')", &[])
        .unwrap();

    // Verify both rows visible within txn.
    let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 2);

    // Rollback to savepoint: row 2 should disappear, row 1 preserved.
    db.execute("ROLLBACK TO SAVEPOINT sp1", &[]).unwrap();

    let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);

    db.execute("COMMIT", &[]).unwrap();

    let result = db.query("SELECT id FROM t ORDER BY id", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).ok(), None); // id column, not text
}

#[test]
fn savepoint_release() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .unwrap();

    db.execute("BEGIN", &[]).unwrap();
    db.execute("SAVEPOINT sp1", &[]).unwrap();
    db.execute("INSERT INTO t (id, v) VALUES (1, 'a')", &[])
        .unwrap();
    db.execute("RELEASE SAVEPOINT sp1", &[]).unwrap();
    db.execute("COMMIT", &[]).unwrap();

    let result = db.query("SELECT v FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "a");
}

// ===========================================================================
// auto_commit: without BEGIN each execute() auto-commits
// ===========================================================================

#[test]
fn auto_commit_each_statement() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .unwrap();

    // Each INSERT auto-commits -- no BEGIN needed.
    db.execute("INSERT INTO t (id, v) VALUES (1, 'first')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, v) VALUES (2, 'second')", &[])
        .unwrap();

    let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 2);
}

// ===========================================================================
// Transaction API: db.begin() -> tx.execute/query/commit/rollback
// ===========================================================================

#[test]
fn transaction_api_commit() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .unwrap();

    let tx = db.begin().unwrap();
    tx.execute("INSERT INTO t (id, v) VALUES (1, 'via_api')", &[])
        .unwrap();

    // Query within the transaction.
    let result = tx.query("SELECT v FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "via_api");

    tx.commit().unwrap();

    // Visible after commit.
    let result = db.query("SELECT v FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "via_api");
}

#[test]
fn transaction_api_rollback() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .unwrap();

    let tx = db.begin().unwrap();
    tx.execute("INSERT INTO t (id, v) VALUES (1, 'doomed')", &[])
        .unwrap();
    tx.rollback().unwrap();

    // Not visible after rollback.
    let result = db.query("SELECT v FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 0);
}

#[test]
fn transaction_api_drop_aborts() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .unwrap();

    {
        let tx = db.begin().unwrap();
        tx.execute("INSERT INTO t (id, v) VALUES (1, 'dropped')", &[])
            .unwrap();
        // tx is dropped without commit or rollback -- should abort.
    }

    let result = db.query("SELECT v FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 0);
}

// ===========================================================================
// concurrent_readers: multiple queries after commit
// ===========================================================================

#[test]
fn concurrent_readers_after_commit() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, v) VALUES (1, 'a')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, v) VALUES (2, 'b')", &[])
        .unwrap();

    // Multiple sequential reads should all work correctly.
    let r1 = db.query("SELECT v FROM t WHERE id = 1", &[]).unwrap();
    let r2 = db.query("SELECT v FROM t WHERE id = 2", &[]).unwrap();
    let r3 = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();

    assert_eq!(r1.rows()[0].get::<String>(0).unwrap(), "a");
    assert_eq!(r2.rows()[0].get::<String>(0).unwrap(), "b");
    assert_eq!(r3.rows()[0].get::<i64>(0).unwrap(), 2);
}
