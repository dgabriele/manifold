use manifold_sql::Database;

#[test]
fn create_table_and_insert() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
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
    db.execute("CREATE TABLE users (id INTEGER, name TEXT, age INTEGER)", &[])
        .unwrap();
    db.execute("INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30)", &[])
        .unwrap();
    db.execute("INSERT INTO users (id, name, age) VALUES (2, 'Bob', 25)", &[])
        .unwrap();
    db.execute("INSERT INTO users (id, name, age) VALUES (3, 'Carol', 35)", &[])
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

    let result = db
        .query("SELECT val FROM t WHERE id = 1", &[])
        .unwrap();
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
    db.execute("INSERT INTO t (id, name) VALUES (3, 'c')", &[]).unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'a')", &[]).unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (2, 'b')", &[]).unwrap();

    let result = db
        .query("SELECT id, name FROM t ORDER BY id", &[])
        .unwrap();
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
    db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)", &[]).unwrap();
    db.execute("CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, item TEXT)", &[]).unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (1, 'alice')", &[]).unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (2, 'bob')", &[]).unwrap();
    db.execute("INSERT INTO orders (id, user_id, item) VALUES (1, 1, 'book')", &[]).unwrap();
    db.execute("INSERT INTO orders (id, user_id, item) VALUES (2, 1, 'pen')", &[]).unwrap();
    db.execute("INSERT INTO orders (id, user_id, item) VALUES (3, 2, 'laptop')", &[]).unwrap();

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
    db.execute("CREATE TABLE a (id INTEGER PRIMARY KEY, val TEXT)", &[]).unwrap();
    db.execute("CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER, val TEXT)", &[]).unwrap();
    db.execute("INSERT INTO a (id, val) VALUES (1, 'x')", &[]).unwrap();
    db.execute("INSERT INTO a (id, val) VALUES (2, 'y')", &[]).unwrap();
    db.execute("INSERT INTO b (id, a_id, val) VALUES (1, 1, 'match')", &[]).unwrap();

    let result = db.query(
        "SELECT a.val, b.val FROM a LEFT JOIN b ON a.id = b.a_id ORDER BY a.val",
        &[],
    ).unwrap();
    assert_eq!(result.row_count(), 2); // x+match, y+NULL
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "x");
    assert_eq!(result.rows()[0].get::<String>(1).unwrap(), "match");
    assert_eq!(result.rows()[1].get::<String>(0).unwrap(), "y");
}

#[test]
fn cross_join() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute("CREATE TABLE x (id INTEGER PRIMARY KEY, v TEXT)", &[]).unwrap();
    db.execute("CREATE TABLE y (id INTEGER PRIMARY KEY, w TEXT)", &[]).unwrap();
    db.execute("INSERT INTO x (id, v) VALUES (1, 'a')", &[]).unwrap();
    db.execute("INSERT INTO x (id, v) VALUES (2, 'b')", &[]).unwrap();
    db.execute("INSERT INTO y (id, w) VALUES (1, 'p')", &[]).unwrap();
    db.execute("INSERT INTO y (id, w) VALUES (2, 'q')", &[]).unwrap();

    let result = db.query("SELECT x.v, y.w FROM x CROSS JOIN y", &[]).unwrap();
    assert_eq!(result.row_count(), 4); // 2x2 cartesian product
}

#[test]
fn right_join() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    db.execute("CREATE TABLE r (id INTEGER PRIMARY KEY, val TEXT)", &[]).unwrap();
    db.execute("CREATE TABLE s (id INTEGER PRIMARY KEY, r_id INTEGER, val TEXT)", &[]).unwrap();
    db.execute("INSERT INTO r (id, val) VALUES (1, 'x')", &[]).unwrap();
    db.execute("INSERT INTO s (id, r_id, val) VALUES (1, 1, 'match')", &[]).unwrap();
    db.execute("INSERT INTO s (id, r_id, val) VALUES (2, 99, 'orphan')", &[]).unwrap();

    let result = db.query(
        "SELECT r.val, s.val FROM r RIGHT JOIN s ON r.id = s.r_id ORDER BY s.val",
        &[],
    ).unwrap();
    assert_eq!(result.row_count(), 2); // match + orphan(NULL+orphan)
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "x");
    assert_eq!(result.rows()[0].get::<String>(1).unwrap(), "match");
    // Second row: r.val is NULL, s.val is 'orphan'
    assert_eq!(result.rows()[1].get::<String>(1).unwrap(), "orphan");
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
    db.execute(
        "CREATE TABLE sales (category TEXT, amount INTEGER)",
        &[],
    )
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
    db.execute(
        "CREATE TABLE sales (category TEXT, amount INTEGER)",
        &[],
    )
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
