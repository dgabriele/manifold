mod common;

use common::test_db;
use manifold_sql::Value;

#[test]
fn prepared_query() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'alice')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (2, 'bob')", &[])
        .unwrap();

    let stmt = db
        .prepare("SELECT name FROM t WHERE id = $1")
        .unwrap();

    let r1 = stmt.query(&db, &[Value::Integer(1)]).unwrap();
    assert_eq!(r1.rows()[0].get::<String>(0).unwrap(), "alice");

    let r2 = stmt.query(&db, &[Value::Integer(2)]).unwrap();
    assert_eq!(r2.rows()[0].get::<String>(0).unwrap(), "bob");
}

#[test]
fn prepared_insert() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();

    let stmt = db
        .prepare("INSERT INTO t (id, name) VALUES ($1, $2)")
        .unwrap();
    stmt.execute(&db, &[Value::Integer(1), Value::Text("alice".into())])
        .unwrap();
    stmt.execute(&db, &[Value::Integer(2), Value::Text("bob".into())])
        .unwrap();

    let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 2);
}

#[test]
fn prepared_wrong_param_count() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    let stmt = db
        .prepare("SELECT * FROM t WHERE id = $1")
        .unwrap();
    assert!(stmt.query(&db, &[]).is_err()); // too few params
}

#[test]
fn prepared_no_params() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'alice')", &[])
        .unwrap();

    let stmt = db.prepare("SELECT * FROM t").unwrap();
    assert_eq!(stmt.param_count(), 0);
    assert!(stmt.is_query());

    let result = stmt.query(&db, &[]).unwrap();
    assert_eq!(result.row_count(), 1);
}

#[test]
fn prepared_multiple_executions() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, value INTEGER)",
        &[],
    )
    .unwrap();

    let insert_stmt = db
        .prepare("INSERT INTO t (id, value) VALUES ($1, $2)")
        .unwrap();

    for i in 1..=100 {
        insert_stmt
            .execute(
                &db,
                &[Value::Integer(i), Value::Integer(i * 10)],
            )
            .unwrap();
    }

    let query_stmt = db
        .prepare("SELECT value FROM t WHERE id = $1")
        .unwrap();

    for i in 1..=100 {
        let result = query_stmt.query(&db, &[Value::Integer(i)]).unwrap();
        assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), i * 10);
    }
}

#[test]
fn prepared_update() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'old')", &[])
        .unwrap();

    let stmt = db
        .prepare("UPDATE t SET name = $1 WHERE id = $2")
        .unwrap();
    assert!(!stmt.is_query());
    assert_eq!(stmt.param_count(), 2);

    stmt.execute(&db, &[Value::Text("new".into()), Value::Integer(1)])
        .unwrap();

    let result = db
        .query("SELECT name FROM t WHERE id = 1", &[])
        .unwrap();
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "new");
}

#[test]
fn prepared_delete() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (1, 'alice')", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, name) VALUES (2, 'bob')", &[])
        .unwrap();

    let stmt = db.prepare("DELETE FROM t WHERE id = $1").unwrap();
    stmt.execute(&db, &[Value::Integer(1)]).unwrap();

    let result = db.query("SELECT COUNT(*) FROM t", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);
}
