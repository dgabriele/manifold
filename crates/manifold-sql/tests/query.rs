mod common;

use common::test_db;
use manifold_sql::Value;

// ===========================================================================
// SELECT basics
// ===========================================================================

#[test]
fn select_star() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'a')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (2, 'b')", &[])
        .unwrap();

    let result = db.query("SELECT * FROM t ORDER BY id", &[]).unwrap();
    assert_eq!(result.row_count(), 2);
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);
    assert_eq!(result.rows()[0].get::<String>(1).unwrap(), "a");
    assert_eq!(result.rows()[1].get::<i64>(0).unwrap(), 2);
    assert_eq!(result.rows()[1].get::<String>(1).unwrap(), "b");
}

#[test]
fn select_specific_columns() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT, age INTEGER)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name, age) VALUES (1, 'alice', 30)", &[])
        .unwrap();

    let result = db.query("SELECT name, age FROM t", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "alice");
    assert_eq!(result.rows()[0].get::<i64>(1).unwrap(), 30);
}

#[test]
fn select_with_column_alias() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'a')", &[])
        .unwrap();

    let result = db.query("SELECT name AS n FROM t", &[]).unwrap();
    assert_eq!(result.columns(), &["n"]);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "a");
}

#[test]
fn select_with_arithmetic_expression() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, price INTEGER, qty INTEGER)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, price, qty) VALUES (1, 10, 3)", &[])
        .unwrap();

    let result = db
        .query("SELECT price * qty AS total FROM t", &[])
        .unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 30);
}

#[test]
fn select_distinct() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, val TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, 'x')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (2, 'x')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (3, 'y')", &[])
        .unwrap();

    let result = db.query("SELECT DISTINCT val FROM t ORDER BY val", &[]).unwrap();
    assert_eq!(result.row_count(), 2);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "x");
    assert_eq!(result.rows()[1].get::<String>(0).unwrap(), "y");
}

// ===========================================================================
// WHERE clause operators
// ===========================================================================

/// Helper: set up a table with a few rows for WHERE-clause tests.
fn setup_where_table(db: &manifold_sql::Database) {
    db.execute(
        "CREATE TABLE items (id INTEGER, name TEXT, price INTEGER, category TEXT)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO items (id, name, price, category) VALUES (1, 'apple', 5, 'fruit')", &[]).unwrap();
    db.execute("INSERT INTO items (id, name, price, category) VALUES (2, 'banana', 3, 'fruit')", &[]).unwrap();
    db.execute("INSERT INTO items (id, name, price, category) VALUES (3, 'carrot', 4, 'vegetable')", &[]).unwrap();
    db.execute("INSERT INTO items (id, name, price, category) VALUES (4, 'donut', 8, 'pastry')", &[]).unwrap();
    db.execute("INSERT INTO items (id, name, price, category) VALUES (5, 'eclair', 10, 'pastry')", &[]).unwrap();
}

#[test]
fn where_comparison_eq() {
    let (db, _dir) = test_db();
    setup_where_table(&db);
    let result = db.query("SELECT name FROM items WHERE id = 1", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "apple");
}

#[test]
fn where_comparison_ne() {
    let (db, _dir) = test_db();
    setup_where_table(&db);
    let result = db.query("SELECT COUNT(*) FROM items WHERE id != 1", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 4);
}

#[test]
fn where_comparison_lt_gt_le_ge() {
    let (db, _dir) = test_db();
    setup_where_table(&db);

    let lt = db.query("SELECT COUNT(*) FROM items WHERE price < 5", &[]).unwrap();
    assert_eq!(lt.rows()[0].get::<i64>(0).unwrap(), 2); // banana(3), carrot(4)

    let gt = db.query("SELECT COUNT(*) FROM items WHERE price > 5", &[]).unwrap();
    assert_eq!(gt.rows()[0].get::<i64>(0).unwrap(), 2); // donut(8), eclair(10)

    let le = db.query("SELECT COUNT(*) FROM items WHERE price <= 5", &[]).unwrap();
    assert_eq!(le.rows()[0].get::<i64>(0).unwrap(), 3);

    let ge = db.query("SELECT COUNT(*) FROM items WHERE price >= 5", &[]).unwrap();
    assert_eq!(ge.rows()[0].get::<i64>(0).unwrap(), 3);
}

#[test]
fn where_and_or() {
    let (db, _dir) = test_db();
    setup_where_table(&db);

    let result = db
        .query(
            "SELECT COUNT(*) FROM items WHERE category = 'fruit' AND price > 3",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1); // apple

    let result = db
        .query(
            "SELECT COUNT(*) FROM items WHERE category = 'fruit' OR category = 'pastry'",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 4);
}

#[test]
fn where_not() {
    let (db, _dir) = test_db();
    setup_where_table(&db);

    let result = db
        .query(
            "SELECT COUNT(*) FROM items WHERE NOT category = 'fruit'",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 3);
}

#[test]
fn where_in_list() {
    let (db, _dir) = test_db();
    setup_where_table(&db);

    let result = db
        .query("SELECT COUNT(*) FROM items WHERE id IN (1, 3, 5)", &[])
        .unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 3);
}

#[test]
fn where_between() {
    let (db, _dir) = test_db();
    setup_where_table(&db);

    let result = db
        .query("SELECT COUNT(*) FROM items WHERE price BETWEEN 4 AND 8", &[])
        .unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 3); // apple(5), carrot(4), donut(8)
}

#[test]
fn where_like_percent() {
    let (db, _dir) = test_db();
    setup_where_table(&db);

    let result = db
        .query("SELECT COUNT(*) FROM items WHERE name LIKE '%a%'", &[])
        .unwrap();
    // apple, banana, carrot, eclair all contain 'a'
    assert!(result.rows()[0].get::<i64>(0).unwrap() >= 3);
}

#[test]
fn where_like_underscore() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (val TEXT)", &[]).unwrap();
    db.execute("INSERT INTO t (val) VALUES ('cat')", &[]).unwrap();
    db.execute("INSERT INTO t (val) VALUES ('car')", &[]).unwrap();
    db.execute("INSERT INTO t (val) VALUES ('card')", &[]).unwrap();

    let result = db
        .query("SELECT COUNT(*) FROM t WHERE val LIKE 'ca_'", &[])
        .unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 2); // cat, car (3 chars)
}

#[test]
fn where_is_null() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, val TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, 'a')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id) VALUES (2)", &[]).unwrap();

    let result = db
        .query("SELECT COUNT(*) FROM t WHERE val IS NULL", &[])
        .unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);
}

#[test]
fn where_is_not_null() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, val TEXT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, 'a')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id) VALUES (2)", &[]).unwrap();

    let result = db
        .query("SELECT COUNT(*) FROM t WHERE val IS NOT NULL", &[])
        .unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);
}

// ===========================================================================
// JOINs
// ===========================================================================

fn setup_join_tables(db: &manifold_sql::Database) {
    db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)", &[]).unwrap();
    db.execute("CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, item TEXT)", &[]).unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (1, 'alice')", &[]).unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (2, 'bob')", &[]).unwrap();
    db.execute("INSERT INTO users (id, name) VALUES (3, 'carol')", &[]).unwrap();
    db.execute("INSERT INTO orders (id, user_id, item) VALUES (1, 1, 'book')", &[]).unwrap();
    db.execute("INSERT INTO orders (id, user_id, item) VALUES (2, 1, 'pen')", &[]).unwrap();
    db.execute("INSERT INTO orders (id, user_id, item) VALUES (3, 2, 'laptop')", &[]).unwrap();
    // carol has no orders; no orphan orders
}

#[test]
fn inner_join() {
    let (db, _dir) = test_db();
    setup_join_tables(&db);

    let result = db
        .query(
            "SELECT users.name, orders.item FROM users INNER JOIN orders ON users.id = orders.user_id ORDER BY orders.item",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 3);
    assert_eq!(result.rows()[0].get::<String>(1).unwrap(), "book");
    assert_eq!(result.rows()[1].get::<String>(1).unwrap(), "laptop");
    assert_eq!(result.rows()[2].get::<String>(1).unwrap(), "pen");
}

#[test]
fn left_join_with_null() {
    let (db, _dir) = test_db();
    setup_join_tables(&db);

    let result = db
        .query(
            "SELECT users.name, orders.item FROM users LEFT JOIN orders ON users.id = orders.user_id ORDER BY users.name",
            &[],
        )
        .unwrap();
    // alice(2 orders) + bob(1 order) + carol(0 orders, NULL item) = 4 rows
    assert_eq!(result.row_count(), 4);

    // Carol's row should have NULL item.
    let carol_row = result.rows().iter().find(|r| r.get::<String>(0).unwrap() == "carol").unwrap();
    let item_val = carol_row.get::<Value>(1).unwrap();
    assert_eq!(item_val, Value::Null);
}

#[test]
fn right_join_with_null() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE a (id INTEGER PRIMARY KEY, val TEXT)", &[]).unwrap();
    db.execute("CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER, val TEXT)", &[]).unwrap();
    db.execute("INSERT INTO a (id, val) VALUES (1, 'x')", &[]).unwrap();
    db.execute("INSERT INTO b (id, a_id, val) VALUES (1, 1, 'match')", &[]).unwrap();
    db.execute("INSERT INTO b (id, a_id, val) VALUES (2, 99, 'orphan')", &[]).unwrap();

    let result = db
        .query(
            "SELECT a.val, b.val FROM a RIGHT JOIN b ON a.id = b.a_id ORDER BY b.val",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 2);
    // First row: match
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "x");
    assert_eq!(result.rows()[0].get::<String>(1).unwrap(), "match");
    // Second row: orphan with NULL from a
    let a_val = result.rows()[1].get::<Value>(0).unwrap();
    assert_eq!(a_val, Value::Null);
    assert_eq!(result.rows()[1].get::<String>(1).unwrap(), "orphan");
}

#[test]
fn cross_join() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE x (id INTEGER, v TEXT)", &[]).unwrap();
    db.execute("CREATE TABLE y (id INTEGER, w TEXT)", &[]).unwrap();
    db.execute("INSERT INTO x (id, v) VALUES (1, 'a')", &[]).unwrap();
    db.execute("INSERT INTO x (id, v) VALUES (2, 'b')", &[]).unwrap();
    db.execute("INSERT INTO y (id, w) VALUES (1, 'p')", &[]).unwrap();
    db.execute("INSERT INTO y (id, w) VALUES (2, 'q')", &[]).unwrap();

    let result = db.query("SELECT x.v, y.w FROM x CROSS JOIN y", &[]).unwrap();
    assert_eq!(result.row_count(), 4); // 2x2
}

#[test]
fn multi_table_join_three_tables() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE departments (id INTEGER PRIMARY KEY, name TEXT)", &[]).unwrap();
    db.execute("CREATE TABLE employees (id INTEGER PRIMARY KEY, dept_id INTEGER, name TEXT)", &[]).unwrap();
    db.execute("CREATE TABLE salaries (id INTEGER PRIMARY KEY, emp_id INTEGER, amount INTEGER)", &[]).unwrap();

    db.execute("INSERT INTO departments (id, name) VALUES (1, 'eng')", &[]).unwrap();
    db.execute("INSERT INTO employees (id, dept_id, name) VALUES (1, 1, 'alice')", &[]).unwrap();
    db.execute("INSERT INTO salaries (id, emp_id, amount) VALUES (1, 1, 100000)", &[]).unwrap();

    let result = db
        .query(
            "SELECT departments.name, employees.name, salaries.amount \
             FROM departments \
             INNER JOIN employees ON departments.id = employees.dept_id \
             INNER JOIN salaries ON employees.id = salaries.emp_id",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "eng");
    assert_eq!(result.rows()[0].get::<String>(1).unwrap(), "alice");
    assert_eq!(result.rows()[0].get::<i64>(2).unwrap(), 100000);
}

#[test]
fn self_join() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE employees (id INTEGER PRIMARY KEY, name TEXT, manager_id INTEGER)", &[]).unwrap();
    db.execute("INSERT INTO employees (id, name, manager_id) VALUES (1, 'boss', NULL)", &[]).unwrap();
    db.execute("INSERT INTO employees (id, name, manager_id) VALUES (2, 'worker', 1)", &[]).unwrap();

    let result = db
        .query(
            "SELECT e.name, m.name FROM employees AS e \
             INNER JOIN employees AS m ON e.manager_id = m.id",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "worker");
    assert_eq!(result.rows()[0].get::<String>(1).unwrap(), "boss");
}

// ===========================================================================
// Aggregates
// ===========================================================================

fn setup_agg_table(db: &manifold_sql::Database) {
    db.execute("CREATE TABLE sales (id INTEGER, category TEXT, amount INTEGER)", &[]).unwrap();
    db.execute("INSERT INTO sales (id, category, amount) VALUES (1, 'A', 10)", &[]).unwrap();
    db.execute("INSERT INTO sales (id, category, amount) VALUES (2, 'A', 20)", &[]).unwrap();
    db.execute("INSERT INTO sales (id, category, amount) VALUES (3, 'B', 30)", &[]).unwrap();
    db.execute("INSERT INTO sales (id, category, amount) VALUES (4, 'B', 40)", &[]).unwrap();
    db.execute("INSERT INTO sales (id, category, amount) VALUES (5, 'B', 50)", &[]).unwrap();
}

#[test]
fn count_star() {
    let (db, _dir) = test_db();
    setup_agg_table(&db);
    let result = db.query("SELECT COUNT(*) FROM sales", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 5);
}

#[test]
fn count_column() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, val TEXT)", &[]).unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, 'a')", &[]).unwrap();
    db.execute("INSERT INTO t (id) VALUES (2)", &[]).unwrap(); // val is NULL

    let result = db.query("SELECT COUNT(val) FROM t", &[]).unwrap();
    // COUNT(column) should exclude NULLs.
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);
}

#[test]
fn sum_aggregate() {
    let (db, _dir) = test_db();
    setup_agg_table(&db);
    let result = db.query("SELECT SUM(amount) FROM sales", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 150);
}

#[test]
fn avg_aggregate() {
    let (db, _dir) = test_db();
    setup_agg_table(&db);
    let result = db.query("SELECT AVG(amount) FROM sales", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<f64>(0).unwrap(), 30.0);
}

#[test]
fn min_max_aggregate() {
    let (db, _dir) = test_db();
    setup_agg_table(&db);
    let result = db
        .query("SELECT MIN(amount), MAX(amount) FROM sales", &[])
        .unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 10);
    assert_eq!(result.rows()[0].get::<i64>(1).unwrap(), 50);
}

#[test]
fn group_by_with_aggregate() {
    let (db, _dir) = test_db();
    setup_agg_table(&db);

    let result = db
        .query(
            "SELECT category, SUM(amount) FROM sales GROUP BY category ORDER BY category",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 2);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "A");
    assert_eq!(result.rows()[0].get::<i64>(1).unwrap(), 30);
    assert_eq!(result.rows()[1].get::<String>(0).unwrap(), "B");
    assert_eq!(result.rows()[1].get::<i64>(1).unwrap(), 120);
}

#[test]
fn having_clause() {
    let (db, _dir) = test_db();
    setup_agg_table(&db);

    let result = db
        .query(
            "SELECT category, SUM(amount) FROM sales GROUP BY category HAVING SUM(amount) > 50",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "B");
    assert_eq!(result.rows()[0].get::<i64>(1).unwrap(), 120);
}

// ===========================================================================
// ORDER BY
// ===========================================================================

#[test]
fn order_by_asc_default() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, val TEXT)", &[]).unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (3, 'c')", &[]).unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, 'a')", &[]).unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (2, 'b')", &[]).unwrap();

    let result = db.query("SELECT id FROM t ORDER BY id", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);
    assert_eq!(result.rows()[1].get::<i64>(0).unwrap(), 2);
    assert_eq!(result.rows()[2].get::<i64>(0).unwrap(), 3);
}

#[test]
fn order_by_desc() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();
    db.execute("INSERT INTO t (id) VALUES (1)", &[]).unwrap();
    db.execute("INSERT INTO t (id) VALUES (2)", &[]).unwrap();
    db.execute("INSERT INTO t (id) VALUES (3)", &[]).unwrap();

    let result = db.query("SELECT id FROM t ORDER BY id DESC", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 3);
    assert_eq!(result.rows()[1].get::<i64>(0).unwrap(), 2);
    assert_eq!(result.rows()[2].get::<i64>(0).unwrap(), 1);
}

#[test]
fn order_by_multiple_columns() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (a INTEGER, b INTEGER)", &[]).unwrap();
    db.execute("INSERT INTO t (a, b) VALUES (1, 2)", &[]).unwrap();
    db.execute("INSERT INTO t (a, b) VALUES (1, 1)", &[]).unwrap();
    db.execute("INSERT INTO t (a, b) VALUES (2, 1)", &[]).unwrap();

    let result = db
        .query("SELECT a, b FROM t ORDER BY a ASC, b ASC", &[])
        .unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);
    assert_eq!(result.rows()[0].get::<i64>(1).unwrap(), 1);
    assert_eq!(result.rows()[1].get::<i64>(0).unwrap(), 1);
    assert_eq!(result.rows()[1].get::<i64>(1).unwrap(), 2);
    assert_eq!(result.rows()[2].get::<i64>(0).unwrap(), 2);
    assert_eq!(result.rows()[2].get::<i64>(1).unwrap(), 1);
}

#[test]
fn order_by_nulls_first() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER, val INTEGER)", &[]).unwrap();
    db.execute("INSERT INTO t (id) VALUES (1)", &[]).unwrap(); // val is NULL
    db.execute("INSERT INTO t (id, val) VALUES (2, 10)", &[]).unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (3, 5)", &[]).unwrap();

    let result = db
        .query("SELECT id, val FROM t ORDER BY val NULLS FIRST", &[])
        .unwrap();
    let first_val = result.rows()[0].get::<Value>(1).unwrap();
    assert_eq!(first_val, Value::Null);
}

// ===========================================================================
// LIMIT / OFFSET
// ===========================================================================

#[test]
fn limit_only() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();
    for i in 1..=10 {
        db.execute(&format!("INSERT INTO t (id) VALUES ({i})"), &[]).unwrap();
    }

    let result = db.query("SELECT id FROM t ORDER BY id LIMIT 3", &[]).unwrap();
    assert_eq!(result.row_count(), 3);
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);
    assert_eq!(result.rows()[2].get::<i64>(0).unwrap(), 3);
}

#[test]
fn limit_with_offset() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();
    for i in 1..=10 {
        db.execute(&format!("INSERT INTO t (id) VALUES ({i})"), &[]).unwrap();
    }

    let result = db
        .query("SELECT id FROM t ORDER BY id LIMIT 3 OFFSET 5", &[])
        .unwrap();
    assert_eq!(result.row_count(), 3);
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 6);
    assert_eq!(result.rows()[1].get::<i64>(0).unwrap(), 7);
    assert_eq!(result.rows()[2].get::<i64>(0).unwrap(), 8);
}

#[test]
fn limit_zero_empty_result() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();
    db.execute("INSERT INTO t (id) VALUES (1)", &[]).unwrap();

    let result = db.query("SELECT id FROM t LIMIT 0", &[]).unwrap();
    assert_eq!(result.row_count(), 0);
}

#[test]
fn offset_beyond_row_count_empty_result() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER)", &[]).unwrap();
    db.execute("INSERT INTO t (id) VALUES (1)", &[]).unwrap();
    db.execute("INSERT INTO t (id) VALUES (2)", &[]).unwrap();

    let result = db
        .query("SELECT id FROM t ORDER BY id LIMIT 10 OFFSET 100", &[])
        .unwrap();
    assert_eq!(result.row_count(), 0);
}

// ===========================================================================
// UNION
// ===========================================================================

#[test]
fn union_all_preserves_duplicates() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE a (val TEXT)", &[]).unwrap();
    db.execute("CREATE TABLE b (val TEXT)", &[]).unwrap();
    db.execute("INSERT INTO a (val) VALUES ('x')", &[]).unwrap();
    db.execute("INSERT INTO a (val) VALUES ('y')", &[]).unwrap();
    db.execute("INSERT INTO b (val) VALUES ('y')", &[]).unwrap();
    db.execute("INSERT INTO b (val) VALUES ('z')", &[]).unwrap();

    let result = db
        .query("SELECT val FROM a UNION ALL SELECT val FROM b", &[])
        .unwrap();
    assert_eq!(result.row_count(), 4); // x, y, y, z
}

#[test]
fn union_removes_duplicates() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE a (val TEXT)", &[]).unwrap();
    db.execute("CREATE TABLE b (val TEXT)", &[]).unwrap();
    db.execute("INSERT INTO a (val) VALUES ('x')", &[]).unwrap();
    db.execute("INSERT INTO a (val) VALUES ('y')", &[]).unwrap();
    db.execute("INSERT INTO b (val) VALUES ('y')", &[]).unwrap();
    db.execute("INSERT INTO b (val) VALUES ('z')", &[]).unwrap();

    let result = db
        .query("SELECT val FROM a UNION SELECT val FROM b", &[])
        .unwrap();
    assert_eq!(result.row_count(), 3); // x, y, z
}

// ===========================================================================
// Index scan point lookups
// ===========================================================================

#[test]
fn index_scan_point_lookup() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    for i in 0..1000 {
        db.execute(
            "INSERT INTO t (id, name) VALUES ($1, $2)",
            &[Value::Integer(i), Value::Text(format!("name_{i}"))],
        )
        .unwrap();
    }
    // This should use the PK index, not full scan
    let result = db
        .query("SELECT name FROM t WHERE id = 500", &[])
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "name_500");
}

#[test]
fn index_scan_point_lookup_with_param() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    for i in 0..100 {
        db.execute(
            "INSERT INTO t (id, name) VALUES ($1, $2)",
            &[Value::Integer(i), Value::Text(format!("name_{i}"))],
        )
        .unwrap();
    }
    let result = db
        .query("SELECT name FROM t WHERE id = $1", &[Value::Integer(42)])
        .unwrap();
    assert_eq!(result.row_count(), 1);
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "name_42");
}

#[test]
fn index_scan_point_lookup_not_found() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    for i in 0..10 {
        db.execute(
            "INSERT INTO t (id, name) VALUES ($1, $2)",
            &[Value::Integer(i), Value::Text(format!("name_{i}"))],
        )
        .unwrap();
    }
    let result = db
        .query("SELECT name FROM t WHERE id = 999", &[])
        .unwrap();
    assert_eq!(result.row_count(), 0);
}

#[test]
fn index_scan_nonunique_index() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, category TEXT, name TEXT)",
        &[],
    )
    .unwrap();
    db.execute("CREATE INDEX idx_t_category ON t (category)", &[])
        .unwrap();
    for i in 0..50 {
        db.execute(
            "INSERT INTO t (id, category, name) VALUES ($1, $2, $3)",
            &[
                Value::Integer(i),
                Value::Text(format!("cat_{}", i % 5)),
                Value::Text(format!("name_{i}")),
            ],
        )
        .unwrap();
    }
    // Should find 10 rows for cat_0
    let result = db
        .query(
            "SELECT name FROM t WHERE category = 'cat_0' ORDER BY name",
            &[],
        )
        .unwrap();
    assert_eq!(result.row_count(), 10);
}

#[test]
fn index_scan_explain_shows_index() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    db.execute(
        "INSERT INTO t (id, name) VALUES (1, 'a')",
        &[],
    )
    .unwrap();
    // Use the explain() API which returns the formatted plan directly
    let plan_text = db.explain("SELECT name FROM t WHERE id = 1").unwrap();
    assert!(
        plan_text.contains("IndexScan"),
        "EXPLAIN should show IndexScan, got: {plan_text}"
    );
}
