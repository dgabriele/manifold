use manifold_sql::Database;

fn setup() -> (Database, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    (db, dir)
}

// ---------------------------------------------------------------------------
// Constraint enforcement tests
// ---------------------------------------------------------------------------

#[test]
fn not_null_violation() {
    let (db, _dir) = setup();
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
        msg.contains("not null") || msg.contains("constraint") || msg.contains("null"),
        "expected NOT NULL error, got: {err}"
    );
}

#[test]
fn not_null_allows_value() {
    let (db, _dir) = setup();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'hello')", &[])
        .unwrap();
    let result = db.query("SELECT name FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "hello");
}

#[test]
fn unique_violation() {
    let (db, _dir) = setup();
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
        msg.contains("unique") || msg.contains("constraint") || msg.contains("duplicate"),
        "expected UNIQUE error, got: {err}"
    );
}

#[test]
fn unique_allows_different_values() {
    let (db, _dir) = setup();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, email TEXT UNIQUE)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO t (id, email) VALUES (1, 'a@b.com')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, email) VALUES (2, 'c@d.com')", &[])
        .unwrap();
    let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 2);
}

#[test]
fn type_enforcement() {
    let (db, _dir) = setup();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, count INTEGER)",
        &[],
    )
    .unwrap();
    // Integer literal should work.
    db.execute("INSERT INTO t (id, count) VALUES (1, 42)", &[])
        .unwrap();
    // Text literal in integer column should fail.
    let err = db
        .execute("INSERT INTO t (id, count) VALUES (2, 'not a number')", &[])
        .unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(msg.contains("type"), "expected type error, got: {err}");
}

#[test]
fn varchar_length_enforcement() {
    let (db, _dir) = setup();
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
        msg.contains("varchar") || msg.contains("exceeds") || msg.contains("constraint"),
        "expected VARCHAR length error, got: {err}"
    );
}

#[test]
fn update_not_null_violation() {
    let (db, _dir) = setup();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'hello')", &[])
        .unwrap();
    let err = db
        .execute("UPDATE t SET name = NULL WHERE id = 1", &[])
        .unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("not null") || msg.contains("constraint") || msg.contains("null"),
        "expected NOT NULL error on UPDATE, got: {err}"
    );
}

#[test]
fn update_unique_violation() {
    let (db, _dir) = setup();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, email TEXT UNIQUE)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO t (id, email) VALUES (1, 'a@b.com')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, email) VALUES (2, 'c@d.com')", &[])
        .unwrap();
    let err = db
        .execute("UPDATE t SET email = 'a@b.com' WHERE id = 2", &[])
        .unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("unique") || msg.contains("constraint") || msg.contains("duplicate"),
        "expected UNIQUE error on UPDATE, got: {err}"
    );
}

#[test]
fn create_table_and_insert() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'hello')", &[])
        .unwrap();
    let result = db.query("SELECT id, name FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    let row = &result.rows()[0];
    assert_eq!(row.get::<i64>(0).unwrap(), 1);
    assert_eq!(row.get::<String>(1).unwrap(), "hello");
}

#[test]
fn insert_multiple_and_filter() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute(
        "CREATE TABLE users (id INTEGER, name TEXT, age INTEGER)",
        &[],
    )
    .unwrap();
    db.execute(
        "INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30)",
        &[],
    )
    .unwrap();
    db.execute(
        "INSERT INTO users (id, name, age) VALUES (2, 'Bob', 25)",
        &[],
    )
    .unwrap();
    db.execute(
        "INSERT INTO users (id, name, age) VALUES (3, 'Carol', 35)",
        &[],
    )
    .unwrap();

    // Select all
    let result = db.query("SELECT id, name, age FROM users", &[]).unwrap();
    assert_eq!(result.row_count(), 3);

    // Filter
    let result = db
        .query("SELECT id, name FROM users WHERE age > 28", &[])
        .unwrap();
    assert_eq!(result.row_count(), 2);
}

#[test]
fn update_and_delete() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute("CREATE TABLE t (id INTEGER, val TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, 'one')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (2, 'two')", &[])
        .unwrap();

    // Update
    let affected = db
        .execute("UPDATE t SET val = 'updated' WHERE id = 1", &[])
        .unwrap();
    assert_eq!(affected, 1);

    let result = db.query("SELECT val FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "updated");

    // Delete
    let affected = db.execute("DELETE FROM t WHERE id = 2", &[]).unwrap();
    assert_eq!(affected, 1);

    let result = db.query("SELECT id FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
}

#[test]
fn drop_table() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();
    db.execute("DROP TABLE t", &[]).unwrap();

    // Creating again should work (not "already exists").
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();
}

#[test]
fn order_by_and_limit() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (3, 'c')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'a')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (2, 'b')", &[])
        .unwrap();

    let result = db.query("SELECT id, name FROM t ORDER BY id", &[]).unwrap();
    assert_eq!(result.row_count(), 3);
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);
    assert_eq!(result.rows()[1].get::<i64>(0).unwrap(), 2);
    assert_eq!(result.rows()[2].get::<i64>(0).unwrap(), 3);

    let result = db
        .query("SELECT id FROM t ORDER BY id LIMIT 2", &[])
        .unwrap();
    assert_eq!(result.row_count(), 2);
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);
    assert_eq!(result.rows()[1].get::<i64>(0).unwrap(), 2);
}

#[test]
fn if_not_exists_and_if_exists() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();
    // Should not error.
    db.execute("CREATE TABLE IF NOT EXISTS t (id INTEGER)", &[])
        .unwrap();

    db.execute("DROP TABLE IF EXISTS nonexistent", &[]).unwrap();
}

#[test]
fn inner_join() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, item TEXT)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (1, 'alice')", &[])
        .unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (2, 'bob')", &[])
        .unwrap();
    db.execute(
        "INSERT INTO orders (id, user_id, item) VALUES (1, 1, 'book')",
        &[],
    )
    .unwrap();
    db.execute(
        "INSERT INTO orders (id, user_id, item) VALUES (2, 1, 'pen')",
        &[],
    )
    .unwrap();
    db.execute(
        "INSERT INTO orders (id, user_id, item) VALUES (3, 2, 'laptop')",
        &[],
    )
    .unwrap();

    let result = db.query(
        "SELECT users.name, orders.item FROM users INNER JOIN orders ON users.id = orders.user_id ORDER BY orders.item",
        &[],
    ).unwrap();
    assert_eq!(result.row_count(), 3);
    // Ordered by item: book, laptop, pen
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "alice");
    assert_eq!(result.rows()[0].get::<String>(1).unwrap(), "book");
    assert_eq!(result.rows()[1].get::<String>(0).unwrap(), "bob");
    assert_eq!(result.rows()[1].get::<String>(1).unwrap(), "laptop");
    assert_eq!(result.rows()[2].get::<String>(0).unwrap(), "alice");
    assert_eq!(result.rows()[2].get::<String>(1).unwrap(), "pen");
}

#[test]
fn left_join() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute("CREATE TABLE a (id INTEGER PRIMARY KEY, val TEXT)", &[])
        .unwrap();
    db.execute(
        "CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER, val TEXT)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO a (id, val) VALUES (1, 'x')", &[])
        .unwrap();
    db.execute("INSERT INTO a (id, val) VALUES (2, 'y')", &[])
        .unwrap();
    db.execute("INSERT INTO b (id, a_id, val) VALUES (1, 1, 'match')", &[])
        .unwrap();

    let result = db
        .query(
            "SELECT a.val, b.val FROM a LEFT JOIN b ON a.id = b.a_id ORDER BY a.val",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 2); // x+match, y+NULL
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "x");
    assert_eq!(result.rows()[0].get::<String>(1).unwrap(), "match");
    assert_eq!(result.rows()[1].get::<String>(0).unwrap(), "y");
}

#[test]
fn cross_join() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute("CREATE TABLE x (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .unwrap();
    db.execute("CREATE TABLE y (id INTEGER PRIMARY KEY, w TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO x (id, v) VALUES (1, 'a')", &[])
        .unwrap();
    db.execute("INSERT INTO x (id, v) VALUES (2, 'b')", &[])
        .unwrap();
    db.execute("INSERT INTO y (id, w) VALUES (1, 'p')", &[])
        .unwrap();
    db.execute("INSERT INTO y (id, w) VALUES (2, 'q')", &[])
        .unwrap();

    let result = db
        .query("SELECT x.v, y.w FROM x CROSS JOIN y", &[])
        .unwrap();
    assert_eq!(result.row_count(), 4); // 2x2 cartesian product
}

#[test]
fn right_join() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute("CREATE TABLE r (id INTEGER PRIMARY KEY, val TEXT)", &[])
        .unwrap();
    db.execute(
        "CREATE TABLE s (id INTEGER PRIMARY KEY, r_id INTEGER, val TEXT)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO r (id, val) VALUES (1, 'x')", &[])
        .unwrap();
    db.execute("INSERT INTO s (id, r_id, val) VALUES (1, 1, 'match')", &[])
        .unwrap();
    db.execute(
        "INSERT INTO s (id, r_id, val) VALUES (2, 99, 'orphan')",
        &[],
    )
    .unwrap();

    let result = db
        .query(
            "SELECT r.val, s.val FROM r RIGHT JOIN s ON r.id = s.r_id ORDER BY s.val",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 2); // match + orphan(NULL+orphan)
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "x");
    assert_eq!(result.rows()[0].get::<String>(1).unwrap(), "match");
    // Second row: r.val is NULL, s.val is 'orphan'
    assert_eq!(result.rows()[1].get::<String>(1).unwrap(), "orphan");
}

// ---------------------------------------------------------------------------
// UNION / UNION ALL tests
// ---------------------------------------------------------------------------

#[test]
fn union_all() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute("CREATE TABLE a (id INTEGER PRIMARY KEY, val TEXT)", &[])
        .unwrap();
    db.execute("CREATE TABLE b (id INTEGER PRIMARY KEY, val TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO a (id, val) VALUES (1, 'x')", &[])
        .unwrap();
    db.execute("INSERT INTO a (id, val) VALUES (2, 'y')", &[])
        .unwrap();
    db.execute("INSERT INTO b (id, val) VALUES (1, 'y')", &[])
        .unwrap();
    db.execute("INSERT INTO b (id, val) VALUES (2, 'z')", &[])
        .unwrap();
    let result = db
        .query("SELECT val FROM a UNION ALL SELECT val FROM b", &[])
        .unwrap();
    assert_eq!(result.row_count(), 4); // x, y, y, z (duplicates kept)
}

#[test]
fn union_distinct() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute("CREATE TABLE a (id INTEGER PRIMARY KEY, val TEXT)", &[])
        .unwrap();
    db.execute("CREATE TABLE b (id INTEGER PRIMARY KEY, val TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO a (id, val) VALUES (1, 'x')", &[])
        .unwrap();
    db.execute("INSERT INTO a (id, val) VALUES (2, 'y')", &[])
        .unwrap();
    db.execute("INSERT INTO b (id, val) VALUES (1, 'y')", &[])
        .unwrap();
    db.execute("INSERT INTO b (id, val) VALUES (2, 'z')", &[])
        .unwrap();
    let result = db
        .query("SELECT val FROM a UNION SELECT val FROM b", &[])
        .unwrap();
    assert_eq!(result.row_count(), 3); // x, y, z (duplicate 'y' removed)
}

// ---------------------------------------------------------------------------
// Aggregate tests
// ---------------------------------------------------------------------------

#[test]
fn count_all() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'a')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (2, 'b')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (3, 'c')", &[])
        .unwrap();

    let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 3);
}

#[test]
fn group_by_with_aggregates() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute("CREATE TABLE sales (category TEXT, amount INTEGER)", &[])
        .unwrap();
    db.execute(
        "INSERT INTO sales (category, amount) VALUES ('electronics', 100)",
        &[],
    )
    .unwrap();
    db.execute(
        "INSERT INTO sales (category, amount) VALUES ('electronics', 200)",
        &[],
    )
    .unwrap();
    db.execute(
        "INSERT INTO sales (category, amount) VALUES ('books', 30)",
        &[],
    )
    .unwrap();
    db.execute(
        "INSERT INTO sales (category, amount) VALUES ('books', 20)",
        &[],
    )
    .unwrap();
    db.execute(
        "INSERT INTO sales (category, amount) VALUES ('books', 50)",
        &[],
    )
    .unwrap();

    let result = db
        .query(
            "SELECT category, SUM(amount), COUNT(*) FROM sales GROUP BY category ORDER BY category",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 2);
    // books: sum=100, count=3
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "books");
    assert_eq!(result.rows()[0].get::<i64>(1).unwrap(), 100);
    assert_eq!(result.rows()[0].get::<i64>(2).unwrap(), 3);
    // electronics: sum=300, count=2
    assert_eq!(result.rows()[1].get::<String>(0).unwrap(), "electronics");
    assert_eq!(result.rows()[1].get::<i64>(1).unwrap(), 300);
    assert_eq!(result.rows()[1].get::<i64>(2).unwrap(), 2);
}

#[test]
fn having_clause() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute("CREATE TABLE sales (category TEXT, amount INTEGER)", &[])
        .unwrap();
    db.execute(
        "INSERT INTO sales (category, amount) VALUES ('electronics', 100)",
        &[],
    )
    .unwrap();
    db.execute(
        "INSERT INTO sales (category, amount) VALUES ('electronics', 200)",
        &[],
    )
    .unwrap();
    db.execute(
        "INSERT INTO sales (category, amount) VALUES ('books', 30)",
        &[],
    )
    .unwrap();
    db.execute(
        "INSERT INTO sales (category, amount) VALUES ('books', 20)",
        &[],
    )
    .unwrap();

    let result = db
        .query(
            "SELECT category, SUM(amount) FROM sales GROUP BY category HAVING SUM(amount) > 100",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "electronics");
    assert_eq!(result.rows()[0].get::<i64>(1).unwrap(), 300);
}

#[test]
fn avg_and_min_max() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute("CREATE TABLE nums (val INTEGER)", &[]).unwrap();
    db.execute("INSERT INTO nums (val) VALUES (10)", &[])
        .unwrap();
    db.execute("INSERT INTO nums (val) VALUES (20)", &[])
        .unwrap();
    db.execute("INSERT INTO nums (val) VALUES (30)", &[])
        .unwrap();

    let result = db
        .query("SELECT AVG(val), MIN(val), MAX(val) FROM nums", &[])
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<f64>(0).unwrap(), 20.0);
    assert_eq!(result.rows()[0].get::<i64>(1).unwrap(), 10);
    assert_eq!(result.rows()[0].get::<i64>(2).unwrap(), 30);
}

#[test]
fn count_empty_table() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();

    let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 0);
}

// ---------------------------------------------------------------------------
// EXPLAIN and ANALYZE tests
// ---------------------------------------------------------------------------

#[test]
fn explain_basic() {
    let (db, _dir) = setup();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)", &[])
        .unwrap();
    let plan = db.explain("SELECT name FROM t WHERE id = 1").unwrap();
    // PK equality produces a RowidLookup (direct key access).
    assert!(
        plan.contains("RowidLookup") || plan.contains("IndexScan") || plan.contains("Scan"),
        "expected RowidLookup/IndexScan/Scan in plan, got: {plan}"
    );
}

#[test]
fn analyze_command() {
    let (db, _dir) = setup();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'a')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (2, 'b')", &[])
        .unwrap();
    db.execute("ANALYZE t", &[]).unwrap();
    // After ANALYZE, the optimizer should have stats. Just verify it doesn't error.
}

// ---------------------------------------------------------------------------
// SQL-level transaction tests
// ---------------------------------------------------------------------------

#[test]
fn sql_begin_commit() {
    let (db, _dir) = setup();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .unwrap();
    db.execute("BEGIN", &[]).unwrap();
    db.execute("INSERT INTO t (id, v) VALUES (1, 'a')", &[])
        .unwrap();
    db.execute("COMMIT", &[]).unwrap();
    let result = db.query("SELECT v FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "a");
}

#[test]
fn sql_rollback() {
    let (db, _dir) = setup();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, v) VALUES (1, 'before')", &[])
        .unwrap();
    db.execute("BEGIN", &[]).unwrap();
    db.execute("INSERT INTO t (id, v) VALUES (2, 'rollback_me')", &[])
        .unwrap();
    db.execute("ROLLBACK", &[]).unwrap();
    let result = db.query("SELECT v FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 1); // only 'before' remains
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "before");
}

#[test]
fn sql_begin_double_begin_errors() {
    let (db, _dir) = setup();
    db.execute("BEGIN", &[]).unwrap();
    let err = db.execute("BEGIN", &[]).unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("transaction") || msg.contains("active"),
        "expected transaction error, got: {err}"
    );
    // Clean up
    db.execute("ROLLBACK", &[]).unwrap();
}

#[test]
fn sql_commit_without_begin_errors() {
    let (db, _dir) = setup();
    let err = db.execute("COMMIT", &[]).unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("transaction") || msg.contains("active"),
        "expected transaction error, got: {err}"
    );
}

#[test]
fn sql_rollback_without_begin_errors() {
    let (db, _dir) = setup();
    let err = db.execute("ROLLBACK", &[]).unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("transaction") || msg.contains("active"),
        "expected transaction error, got: {err}"
    );
}

#[test]
fn sql_query_within_transaction_sees_uncommitted() {
    let (db, _dir) = setup();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .unwrap();
    db.execute("BEGIN", &[]).unwrap();
    db.execute("INSERT INTO t (id, v) VALUES (1, 'pending')", &[])
        .unwrap();
    // Query within the same transaction should see the uncommitted row.
    let result = db.query("SELECT v FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "pending");
    db.execute("ROLLBACK", &[]).unwrap();
    // After rollback, row is gone.
    let result = db.query("SELECT v FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 0);
}

#[test]
fn sql_savepoint_rollback_to() {
    let (db, _dir) = setup();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, v) VALUES (1, 'base')", &[])
        .unwrap();
    db.execute("BEGIN", &[]).unwrap();
    db.execute("SAVEPOINT sp1", &[]).unwrap();
    db.execute("INSERT INTO t (id, v) VALUES (2, 'after_sp')", &[])
        .unwrap();
    db.execute("ROLLBACK TO SAVEPOINT sp1", &[]).unwrap();
    // The insert after the savepoint is undone.
    let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);
    db.execute("COMMIT", &[]).unwrap();
    let result = db.query("SELECT v FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "base");
}

#[test]
fn sql_savepoint_release() {
    let (db, _dir) = setup();
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
}
