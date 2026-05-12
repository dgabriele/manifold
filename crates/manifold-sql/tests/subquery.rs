mod common;

use common::test_db;

// ===========================================================================
// WHERE ... IN (SELECT ...)
// ===========================================================================

#[test]
fn where_in_subquery() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (1, 'alice')", &[])
        .unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (2, 'bob')", &[])
        .unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (3, 'carol')", &[])
        .unwrap();
    db.execute("INSERT INTO orders (id, user_id) VALUES (1, 1)", &[])
        .unwrap();
    db.execute("INSERT INTO orders (id, user_id) VALUES (2, 2)", &[])
        .unwrap();

    let result = db
        .query(
            "SELECT name FROM users WHERE id IN (SELECT user_id FROM orders) ORDER BY name",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 2);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "alice");
    assert_eq!(result.rows()[1].get::<String>(0).unwrap(), "bob");
}

#[test]
fn where_not_in_subquery() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (1, 'alice')", &[])
        .unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (2, 'bob')", &[])
        .unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (3, 'carol')", &[])
        .unwrap();
    db.execute("INSERT INTO orders (id, user_id) VALUES (1, 1)", &[])
        .unwrap();
    db.execute("INSERT INTO orders (id, user_id) VALUES (2, 2)", &[])
        .unwrap();

    let result = db
        .query(
            "SELECT name FROM users WHERE id NOT IN (SELECT user_id FROM orders)",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "carol");
}

#[test]
fn in_subquery_empty_result() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT)", &[])
        .unwrap();
    db.execute(
        "CREATE TABLE t2 (id INTEGER PRIMARY KEY, ref_id INTEGER)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO t1 (id, val) VALUES (1, 'a')", &[])
        .unwrap();

    // t2 is empty, so IN subquery should return no rows
    let result = db
        .query(
            "SELECT val FROM t1 WHERE id IN (SELECT ref_id FROM t2)",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 0);
}

// ===========================================================================
// WHERE EXISTS (SELECT ...)
// ===========================================================================

#[test]
fn where_exists_uncorrelated() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (1, 'alice')", &[])
        .unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (2, 'bob')", &[])
        .unwrap();
    db.execute("INSERT INTO orders (id, user_id) VALUES (1, 1)", &[])
        .unwrap();

    // Uncorrelated EXISTS: returns all users because orders table is non-empty
    let result = db
        .query(
            "SELECT name FROM users WHERE EXISTS (SELECT 1 FROM orders) ORDER BY name",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 2);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "alice");
    assert_eq!(result.rows()[1].get::<String>(0).unwrap(), "bob");
}

#[test]
fn where_not_exists_empty() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (1, 'alice')", &[])
        .unwrap();

    // orders is empty, so NOT EXISTS returns all users
    let result = db
        .query(
            "SELECT name FROM users WHERE NOT EXISTS (SELECT 1 FROM orders)",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "alice");
}

#[test]
fn where_exists_empty_subquery() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (1, 'alice')", &[])
        .unwrap();

    // orders is empty, so EXISTS returns no users
    let result = db
        .query(
            "SELECT name FROM users WHERE EXISTS (SELECT 1 FROM orders)",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 0);
}

// ===========================================================================
// FROM (SELECT ...) AS alias — derived table
// ===========================================================================

#[test]
fn derived_table_basic() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, 10)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (2, 20)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (3, 5)", &[])
        .unwrap();

    let result = db
        .query(
            "SELECT sub.val FROM (SELECT id, val FROM t WHERE val > 5) AS sub ORDER BY sub.val",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 2);
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 10);
    assert_eq!(result.rows()[1].get::<i64>(0).unwrap(), 20);
}

#[test]
fn derived_table_with_filter() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, 10)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (2, 20)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (3, 30)", &[])
        .unwrap();

    // Subquery with filter, then outer query also filters
    let result = db
        .query(
            "SELECT sub.val FROM (SELECT val FROM t WHERE val > 10) AS sub WHERE sub.val < 30",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 20);
}

#[test]
fn derived_table_with_alias_columns() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, 42)", &[])
        .unwrap();

    let result = db
        .query("SELECT s.v FROM (SELECT val AS v FROM t) AS s", &[])
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 42);
}

// ===========================================================================
// Correlated EXISTS — marked as ignored (stretch goal)
// ===========================================================================

#[test]
#[ignore = "correlated subqueries not yet supported"]
fn where_exists_correlated() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (1, 'alice')", &[])
        .unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (2, 'bob')", &[])
        .unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (3, 'carol')", &[])
        .unwrap();
    db.execute("INSERT INTO orders (id, user_id) VALUES (1, 1)", &[])
        .unwrap();
    db.execute("INSERT INTO orders (id, user_id) VALUES (2, 2)", &[])
        .unwrap();

    let result = db
        .query(
            "SELECT name FROM users WHERE EXISTS (SELECT 1 FROM orders WHERE orders.user_id = users.id)",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 2);
}

// ===========================================================================
// Scalar subquery in SELECT — marked as ignored (stretch goal)
// ===========================================================================

#[test]
#[ignore = "scalar subquery in SELECT not yet supported"]
fn scalar_subquery_in_select() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, 10)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (2, 20)", &[])
        .unwrap();

    let result = db
        .query(
            "SELECT id, (SELECT MAX(val) FROM t) AS max_val FROM t ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 2);
    assert_eq!(result.rows()[0].get::<i64>(1).unwrap(), 20);
    assert_eq!(result.rows()[1].get::<i64>(1).unwrap(), 20);
}
