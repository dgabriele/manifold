mod common;

use common::test_db;
use manifold_sql::Value;

// ===========================================================================
// Integer types
// ===========================================================================

#[test]
#[ignore] // SMALLINT + INTEGER + BIGINT in same row causes storage offset corruption
fn integer_types_mixed_row() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, s SMALLINT, i INTEGER, b BIGINT)",
        &[],
    )
    .unwrap();

    db.execute("INSERT INTO t (id, s, i, b) VALUES (1, 100, 200, 300)", &[])
        .unwrap();

    let result = db.query("SELECT s, i, b FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.row_count(), 1);
    let row = &result.rows()[0];
    let s_val = row.get::<i64>(0).unwrap();
    assert_eq!(s_val, 100);
    assert_eq!(row.get::<i64>(1).unwrap(), 200);
    assert_eq!(row.get::<i64>(2).unwrap(), 300);
}

#[test]
fn integer_roundtrip() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, 42)", &[]).unwrap();
    let result = db.query("SELECT val FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 42);
}

#[test]
fn bigint_roundtrip() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, val BIGINT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, 9999999999)", &[]).unwrap();
    let result = db.query("SELECT val FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 9999999999);
}

#[test]
fn smallint_roundtrip() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, val SMALLINT)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, 100)", &[]).unwrap();
    let result = db.query("SELECT val FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 100);
}

#[test]
#[ignore] // SMALLINT overflow not yet enforced; values silently truncated
fn integer_overflow_smallint() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, s SMALLINT)", &[])
        .unwrap();

    // SMALLINT max is 32767. Inserting a larger value should error.
    let result = db.execute("INSERT INTO t (id, s) VALUES (1, 99999)", &[]);
    assert!(
        result.is_err(),
        "expected error for SMALLINT overflow, but got success"
    );
}

// ===========================================================================
// Real / float
// ===========================================================================

#[test]
fn real_type_roundtrip() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, val REAL)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, 3.14)", &[])
        .unwrap();

    let result = db.query("SELECT val FROM t WHERE id = 1", &[]).unwrap();
    let val = result.rows()[0].get::<f64>(0).unwrap();
    assert!((val - 3.14).abs() < 1e-10, "expected ~3.14, got {val}");
}

#[test]
fn real_nan_inf() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, val REAL)", &[])
        .unwrap();

    // NaN and Infinity are not standard SQL values. Inserting via param should
    // either error or store them. We just verify no panic.
    let nan_result = db.execute(
        "INSERT INTO t (id, val) VALUES ($1, $2)",
        &[Value::Integer(1), Value::Real(f64::NAN)],
    );
    let inf_result = db.execute(
        "INSERT INTO t (id, val) VALUES ($1, $2)",
        &[Value::Integer(2), Value::Real(f64::INFINITY)],
    );

    // At least one of these behaviors is acceptable:
    // 1. They error (strictness)
    // 2. They store and round-trip
    if nan_result.is_ok() {
        let result = db.query("SELECT val FROM t WHERE id = 1", &[]).unwrap();
        let val = result.rows()[0].get::<f64>(0).unwrap();
        assert!(val.is_nan(), "expected NaN round-trip");
    }
    if inf_result.is_ok() {
        let result = db.query("SELECT val FROM t WHERE id = 2", &[]).unwrap();
        let val = result.rows()[0].get::<f64>(0).unwrap();
        assert!(val.is_infinite(), "expected Infinity round-trip");
    }
}

// ===========================================================================
// Decimal
// ===========================================================================

#[test]
fn decimal_type_roundtrip() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val DECIMAL(10, 2))",
        &[],
    )
    .unwrap();

    db.execute(
        "INSERT INTO t (id, val) VALUES ($1, $2)",
        &[
            Value::Integer(1),
            Value::Decimal(rust_decimal::Decimal::new(12345, 2)), // 123.45
        ],
    )
    .unwrap();

    let result = db.query("SELECT val FROM t WHERE id = 1", &[]).unwrap();
    let val = result.rows()[0].get::<Value>(0).unwrap();
    match val {
        Value::Decimal(d) => {
            assert_eq!(d.to_string(), "123.45");
        }
        other => panic!("expected Decimal, got {other:?}"),
    }
}

// ===========================================================================
// Text
// ===========================================================================

#[test]
fn text_type_roundtrip() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT)", &[])
        .unwrap();

    let unicode_str = "Hello, \u{1F600} world \u{4E16}\u{754C}!";
    db.execute(
        "INSERT INTO t (id, val) VALUES ($1, $2)",
        &[Value::Integer(1), Value::Text(unicode_str.into())],
    )
    .unwrap();

    let result = db.query("SELECT val FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), unicode_str);
}

// ===========================================================================
// VARCHAR
// ===========================================================================

#[test]
fn varchar_type_within_limit() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, code VARCHAR(10))",
        &[],
    )
    .unwrap();

    db.execute("INSERT INTO t (id, code) VALUES (1, 'short')", &[])
        .unwrap();

    let result = db.query("SELECT code FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<String>(0).unwrap(), "short");
}

// ===========================================================================
// Blob
// ===========================================================================

#[test]
fn blob_type_roundtrip() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, data BLOB)", &[])
        .unwrap();

    let bytes: Vec<u8> = vec![0x00, 0xFF, 0xAB, 0xCD, 0x42];
    db.execute(
        "INSERT INTO t (id, data) VALUES ($1, $2)",
        &[Value::Integer(1), Value::Blob(bytes.clone())],
    )
    .unwrap();

    let result = db.query("SELECT data FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(result.rows()[0].get::<Vec<u8>>(0).unwrap(), bytes);
}

// ===========================================================================
// Boolean
// ===========================================================================

#[test]
fn boolean_type_roundtrip() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, flag BOOLEAN)",
        &[],
    )
    .unwrap();

    db.execute("INSERT INTO t (id, flag) VALUES (1, TRUE)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, flag) VALUES (2, FALSE)", &[])
        .unwrap();

    // Query each row individually to avoid ordering issues.
    let result = db
        .query("SELECT flag FROM t WHERE id = 1", &[])
        .unwrap();
    assert_eq!(result.rows()[0].get::<bool>(0).unwrap(), true);

    let result = db
        .query("SELECT flag FROM t WHERE id = 2", &[])
        .unwrap();
    assert_eq!(result.rows()[0].get::<bool>(0).unwrap(), false);
}

// ===========================================================================
// UUID
// ===========================================================================

#[test]
fn uuid_type_roundtrip() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, uid UUID)", &[])
        .unwrap();

    let test_uuid = uuid::Uuid::new_v4();
    db.execute(
        "INSERT INTO t (id, uid) VALUES ($1, $2)",
        &[Value::Integer(1), Value::Uuid(test_uuid)],
    )
    .unwrap();

    let result = db.query("SELECT uid FROM t WHERE id = 1", &[]).unwrap();
    let val = result.rows()[0].get::<Value>(0).unwrap();
    match val {
        Value::Uuid(u) => assert_eq!(u, test_uuid),
        other => panic!("expected UUID, got {other:?}"),
    }
}

// ===========================================================================
// Date
// ===========================================================================

#[test]
fn date_type_roundtrip() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, d DATE)", &[])
        .unwrap();

    let date = chrono::NaiveDate::from_ymd_opt(2025, 6, 15).unwrap();
    db.execute(
        "INSERT INTO t (id, d) VALUES ($1, $2)",
        &[Value::Integer(1), Value::Date(date)],
    )
    .unwrap();

    let result = db.query("SELECT d FROM t WHERE id = 1", &[]).unwrap();
    let val = result.rows()[0].get::<Value>(0).unwrap();
    match val {
        Value::Date(d) => assert_eq!(d, date),
        other => panic!("expected Date, got {other:?}"),
    }
}

// ===========================================================================
// Timestamp
// ===========================================================================

#[test]
fn timestamp_type_roundtrip() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, ts TIMESTAMP)", &[])
        .unwrap();

    let ts = chrono::NaiveDate::from_ymd_opt(2025, 6, 15)
        .unwrap()
        .and_hms_opt(10, 30, 0)
        .unwrap();
    db.execute(
        "INSERT INTO t (id, ts) VALUES ($1, $2)",
        &[Value::Integer(1), Value::Timestamp(ts)],
    )
    .unwrap();

    let result = db.query("SELECT ts FROM t WHERE id = 1", &[]).unwrap();
    let val = result.rows()[0].get::<Value>(0).unwrap();
    match val {
        Value::Timestamp(t) => assert_eq!(t, ts),
        other => panic!("expected Timestamp, got {other:?}"),
    }
}

// ===========================================================================
// Timestamp with time zone
// ===========================================================================

#[test]
fn timestamp_tz_type_roundtrip() {
    let (db, _dir) = test_db();
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, ts TIMESTAMP WITH TIME ZONE)",
        &[],
    )
    .unwrap();

    let ts = chrono::DateTime::parse_from_rfc3339("2025-06-15T10:30:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    db.execute(
        "INSERT INTO t (id, ts) VALUES ($1, $2)",
        &[Value::Integer(1), Value::TimestampTz(ts)],
    )
    .unwrap();

    let result = db.query("SELECT ts FROM t WHERE id = 1", &[]).unwrap();
    let val = result.rows()[0].get::<Value>(0).unwrap();
    match val {
        Value::TimestampTz(t) => assert_eq!(t, ts),
        other => panic!("expected TimestampTz, got {other:?}"),
    }
}

// ===========================================================================
// JSON
// ===========================================================================

#[test]
fn json_type_roundtrip() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, data JSON)", &[])
        .unwrap();

    let json_val = serde_json::json!({"name": "test", "count": 42, "nested": {"a": true}});
    db.execute(
        "INSERT INTO t (id, data) VALUES ($1, $2)",
        &[Value::Integer(1), Value::Json(json_val.clone())],
    )
    .unwrap();

    let result = db.query("SELECT data FROM t WHERE id = 1", &[]).unwrap();
    let val = result.rows()[0].get::<Value>(0).unwrap();
    match val {
        Value::Json(j) => assert_eq!(j, json_val),
        other => panic!("expected JSON, got {other:?}"),
    }
}

// ===========================================================================
// NULL semantics
// ===========================================================================

#[test]
fn null_semantics_equality() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, NULL)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (2, 42)", &[])
        .unwrap();

    // NULL = NULL should not be true (SQL standard: it evaluates to NULL/unknown).
    let result = db
        .query("SELECT COUNT(*) FROM t WHERE val = NULL", &[])
        .unwrap();
    assert_eq!(
        result.rows()[0].get::<i64>(0).unwrap(),
        0,
        "NULL = NULL should not match any rows"
    );

    // IS NULL should work.
    let result = db
        .query("SELECT COUNT(*) FROM t WHERE val IS NULL", &[])
        .unwrap();
    assert_eq!(result.rows()[0].get::<i64>(0).unwrap(), 1);
}

#[test]
fn null_in_arithmetic() {
    let (db, _dir) = test_db();
    db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)", &[])
        .unwrap();
    db.execute("INSERT INTO t (id, val) VALUES (1, NULL)", &[])
        .unwrap();

    // NULL + 1 should be NULL.
    let result = db
        .query("SELECT val + 1 FROM t WHERE id = 1", &[])
        .unwrap();
    assert_eq!(result.row_count(), 1);
    let val = result.rows()[0].get::<Value>(0).unwrap();
    assert_eq!(val, Value::Null, "NULL + 1 should be NULL");
}
