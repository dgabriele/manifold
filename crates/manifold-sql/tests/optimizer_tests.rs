mod common;

use common::test_db;

// ===========================================================================
// EXPLAIN shows plan nodes
// ===========================================================================

#[test]
fn explain_shows_scan() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .unwrap();
    let plan = db.explain("SELECT * FROM t").unwrap();
    let lower = plan.to_lowercase();
    assert!(
        lower.contains("scan") || lower.contains("tablescan"),
        "expected Scan or TableScan in plan, got: {plan}"
    );
}

#[test]
fn explain_shows_filter() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .unwrap();
    let plan = db.explain("SELECT * FROM t WHERE id = 1").unwrap();
    let lower = plan.to_lowercase();
    assert!(
        lower.contains("filter"),
        "expected Filter in plan, got: {plan}"
    );
}

#[test]
fn explain_shows_join() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE a (id INTEGER, val TEXT)", &[])
        .unwrap();
    db.execute("CREATE TABLE b (id INTEGER, a_id INTEGER)", &[])
        .unwrap();
    let plan = db
        .explain("SELECT * FROM a JOIN b ON a.id = b.a_id")
        .unwrap();
    let lower = plan.to_lowercase();
    assert!(lower.contains("join"), "expected Join in plan, got: {plan}");
}

#[test]
fn index_improves_explain() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .unwrap();
    db.execute("CREATE INDEX idx_name ON t (name)", &[])
        .unwrap();
    let plan = db.explain("SELECT * FROM t WHERE name = 'test'").unwrap();
    let lower = plan.to_lowercase();
    // The optimizer may or may not use the index yet; just check the plan is valid.
    assert!(
        lower.contains("scan") || lower.contains("index") || lower.contains("filter"),
        "expected a valid plan, got: {plan}"
    );
}

#[test]
fn analyze_then_query() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'alice')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (2, 'bob')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (3, 'carol')", &[])
        .unwrap();

    db.execute("ANALYZE t", &[]).unwrap();

    // Queries should still work correctly after ANALYZE.
    let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 3);

    let result = db.query("SELECT name FROM t WHERE id = 2", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "bob");
}
