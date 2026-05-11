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
