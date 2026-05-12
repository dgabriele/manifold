use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use manifold_sql::{Database as ManifoldDb, Value as MValue};
use rusqlite::{Connection, params};

// ---------------------------------------------------------------------------
// Helpers — Manifold
// ---------------------------------------------------------------------------

fn manifold_db() -> (ManifoldDb, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    let db = ManifoldDb::open(dir.path().join("bench.db")).unwrap();
    db.set_durability(manifold_sql::Durability::None);
    (db, dir)
}

fn manifold_create_table(db: &ManifoldDb) {
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, value INTEGER, category TEXT)",
        &[],
    )
    .unwrap();
}

fn manifold_populate(db: &ManifoldDb, n: usize) {
    db.execute("BEGIN", &[]).unwrap();
    for i in 0..n {
        db.execute(
            "INSERT INTO t (id, name, value, category) VALUES ($1, $2, $3, $4)",
            &[
                MValue::Integer(i as i64 + 1),
                MValue::Text(format!("name_{i}")),
                MValue::Integer((i % 1000) as i64),
                MValue::Text(format!("cat_{}", i % 10)),
            ],
        )
        .unwrap();
    }
    db.execute("COMMIT", &[]).unwrap();
}

/// Populate, then close and reopen to flush WAL/memtable to B-tree via recovery.
fn manifold_populated(n: usize) -> (ManifoldDb, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("bench.db");
    {
        let db = ManifoldDb::open(&path).unwrap();
        db.set_durability(manifold_sql::Durability::None);
        manifold_create_table(&db);
        manifold_populate(&db, n);
    }
    // Reopen: WAL recovery flushes data to B-tree
    let db = ManifoldDb::open(&path).unwrap();
    db.set_durability(manifold_sql::Durability::None);
    (db, dir)
}

// ---------------------------------------------------------------------------
// Helpers — SQLite
// ---------------------------------------------------------------------------

fn sqlite_db() -> (Connection, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    let conn = Connection::open(dir.path().join("bench.db")).unwrap();
    // WAL mode for fair comparison (manifold uses WAL by default)
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
        .unwrap();
    (conn, dir)
}

fn sqlite_create_table(conn: &Connection) {
    conn.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, value INTEGER, category TEXT)",
        [],
    )
    .unwrap();
}

fn sqlite_populate(conn: &Connection, n: usize) {
    conn.execute("BEGIN", []).unwrap();
    for i in 0..n {
        conn.execute(
            "INSERT INTO t (id, name, value, category) VALUES (?1, ?2, ?3, ?4)",
            params![
                i as i64 + 1,
                format!("name_{i}"),
                (i % 1000) as i64,
                format!("cat_{}", i % 10),
            ],
        )
        .unwrap();
    }
    conn.execute("COMMIT", []).unwrap();
}

// ---------------------------------------------------------------------------
// 1. Point Lookup (PK)
// ---------------------------------------------------------------------------

fn bench_point_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("point_lookup");
    let mut rng = rand::rng();
    use rand::Rng;

    for &size in &[1_000, 10_000] {
        let (mdb, _d1) = manifold_populated(size);

        let (sdb, _d2) = sqlite_db();
        sqlite_create_table(&sdb);
        sqlite_populate(&sdb, size);

        group.bench_with_input(BenchmarkId::new("manifold", size), &size, |b, &size| {
            b.iter(|| {
                let id = rng.random_range(1..=size as i64);
                mdb.query("SELECT * FROM t WHERE id = $1", &[MValue::Integer(id)])
                    .unwrap();
            });
        });

        group.bench_with_input(BenchmarkId::new("sqlite", size), &size, |b, &size| {
            let mut stmt = sdb.prepare_cached("SELECT * FROM t WHERE id = ?1").unwrap();
            b.iter(|| {
                let id = rng.random_range(1..=size as i64);
                let _rows: Vec<(i64, String, i64, String)> = stmt
                    .query_map([id], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                    })
                    .unwrap()
                    .map(|r| r.unwrap())
                    .collect();
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 2. Full Table Scan
// ---------------------------------------------------------------------------

fn bench_full_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_scan");

    for &size in &[1_000, 10_000] {
        let (mdb, _d1) = manifold_populated(size);

        let (sdb, _d2) = sqlite_db();
        sqlite_create_table(&sdb);
        sqlite_populate(&sdb, size);

        group.bench_with_input(BenchmarkId::new("manifold", size), &size, |b, _| {
            b.iter(|| {
                mdb.query("SELECT * FROM t", &[]).unwrap();
            });
        });

        group.bench_with_input(BenchmarkId::new("sqlite", size), &size, |b, _| {
            let mut stmt = sdb.prepare_cached("SELECT * FROM t").unwrap();
            b.iter(|| {
                let _rows: Vec<(i64, String, i64, String)> = stmt
                    .query_map([], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                    })
                    .unwrap()
                    .map(|r| r.unwrap())
                    .collect();
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 3. Range Scan (100 rows)
// ---------------------------------------------------------------------------

fn bench_range_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("range_scan");
    let mut rng = rand::rng();
    use rand::Rng;

    for &size in &[1_000, 10_000] {
        let (mdb, _d1) = manifold_populated(size);

        let (sdb, _d2) = sqlite_db();
        sqlite_create_table(&sdb);
        sqlite_populate(&sdb, size);

        group.bench_with_input(BenchmarkId::new("manifold", size), &size, |b, &size| {
            b.iter(|| {
                let start = rng.random_range(1..=(size as i64 - 100));
                let end = start + 99;
                mdb.query(
                    "SELECT * FROM t WHERE id BETWEEN $1 AND $2",
                    &[MValue::Integer(start), MValue::Integer(end)],
                )
                .unwrap();
            });
        });

        group.bench_with_input(BenchmarkId::new("sqlite", size), &size, |b, &size| {
            let mut stmt = sdb
                .prepare_cached("SELECT * FROM t WHERE id BETWEEN ?1 AND ?2")
                .unwrap();
            b.iter(|| {
                let start = rng.random_range(1..=(size as i64 - 100));
                let end = start + 99;
                let _rows: Vec<(i64, String, i64, String)> = stmt
                    .query_map([start, end], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                    })
                    .unwrap()
                    .map(|r| r.unwrap())
                    .collect();
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 4. Single Insert (auto-commit)
// ---------------------------------------------------------------------------

fn bench_single_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_insert");

    let (mdb, _d1) = manifold_db();
    manifold_create_table(&mdb);
    manifold_populate(&mdb, 1_000);
    let m_ctr = std::cell::Cell::new(100_000_i64);

    let (sdb, _d2) = sqlite_db();
    sqlite_create_table(&sdb);
    sqlite_populate(&sdb, 1_000);
    let s_ctr = std::cell::Cell::new(100_000_i64);

    group.bench_function("manifold", |b| {
        b.iter(|| {
            let id = m_ctr.get();
            m_ctr.set(id + 1);
            mdb.execute(
                "INSERT INTO t (id, name, value, category) VALUES ($1, $2, $3, $4)",
                &[
                    MValue::Integer(id),
                    MValue::Text(format!("name_{id}")),
                    MValue::Integer(id % 1000),
                    MValue::Text(format!("cat_{}", id % 10)),
                ],
            )
            .unwrap();
        });
    });

    group.bench_function("sqlite", |b| {
        b.iter(|| {
            let id = s_ctr.get();
            s_ctr.set(id + 1);
            sdb.execute(
                "INSERT INTO t (id, name, value, category) VALUES (?1, ?2, ?3, ?4)",
                params![
                    id,
                    format!("name_{id}"),
                    id % 1000,
                    format!("cat_{}", id % 10),
                ],
            )
            .unwrap();
        });
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// 5. Bulk Insert (1000 rows / transaction)
// ---------------------------------------------------------------------------

fn bench_bulk_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("bulk_insert_1000");
    group.sample_size(10);

    let (mdb, _d1) = manifold_db();
    manifold_create_table(&mdb);
    let m_ctr = std::cell::Cell::new(1_i64);

    let (sdb, _d2) = sqlite_db();
    sqlite_create_table(&sdb);
    let s_ctr = std::cell::Cell::new(1_i64);

    group.bench_function("manifold", |b| {
        b.iter(|| {
            let base = m_ctr.get();
            m_ctr.set(base + 1000);
            mdb.execute("BEGIN", &[]).unwrap();
            for i in 0..1000 {
                let id = base + i;
                mdb.execute(
                    "INSERT INTO t (id, name, value, category) VALUES ($1, $2, $3, $4)",
                    &[
                        MValue::Integer(id),
                        MValue::Text(format!("name_{id}")),
                        MValue::Integer(id % 1000),
                        MValue::Text(format!("cat_{}", id % 10)),
                    ],
                )
                .unwrap();
            }
            mdb.execute("COMMIT", &[]).unwrap();
        });
    });

    group.bench_function("sqlite", |b| {
        b.iter(|| {
            let base = s_ctr.get();
            s_ctr.set(base + 1000);
            sdb.execute("BEGIN", []).unwrap();
            {
                let mut stmt = sdb
                    .prepare_cached(
                        "INSERT INTO t (id, name, value, category) VALUES (?1, ?2, ?3, ?4)",
                    )
                    .unwrap();
                for i in 0..1000 {
                    let id = base + i;
                    stmt.execute(params![
                        id,
                        format!("name_{id}"),
                        id % 1000,
                        format!("cat_{}", id % 10),
                    ])
                    .unwrap();
                }
            }
            sdb.execute("COMMIT", []).unwrap();
        });
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// 6. Update by PK
// ---------------------------------------------------------------------------

fn bench_update(c: &mut Criterion) {
    let mut group = c.benchmark_group("update_by_pk");
    let mut rng = rand::rng();
    use rand::Rng;

    let (mdb, _d1) = manifold_populated(10_000);

    let (sdb, _d2) = sqlite_db();
    sqlite_create_table(&sdb);
    sqlite_populate(&sdb, 10_000);

    group.bench_function("manifold", |b| {
        b.iter(|| {
            let id = rng.random_range(1..=10_000_i64);
            mdb.execute(
                "UPDATE t SET value = $1 WHERE id = $2",
                &[MValue::Integer(999), MValue::Integer(id)],
            )
            .unwrap();
        });
    });

    group.bench_function("sqlite", |b| {
        let mut stmt = sdb
            .prepare_cached("UPDATE t SET value = ?1 WHERE id = ?2")
            .unwrap();
        b.iter(|| {
            let id = rng.random_range(1..=10_000_i64);
            stmt.execute(params![999_i64, id]).unwrap();
        });
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// 7. Delete by PK
// ---------------------------------------------------------------------------

fn bench_delete(c: &mut Criterion) {
    let mut group = c.benchmark_group("delete_by_pk");
    let mut rng = rand::rng();
    use rand::Rng;

    let (mdb, _d1) = manifold_populated(10_000);

    let (sdb, _d2) = sqlite_db();
    sqlite_create_table(&sdb);
    sqlite_populate(&sdb, 10_000);

    group.bench_function("manifold", |b| {
        b.iter(|| {
            let id = rng.random_range(1..=10_000_i64);
            mdb.execute("DELETE FROM t WHERE id = $1", &[MValue::Integer(id)])
                .unwrap();
            let _ = mdb.execute(
                "INSERT INTO t (id, name, value, category) VALUES ($1, $2, $3, $4)",
                &[
                    MValue::Integer(id),
                    MValue::Text(format!("name_{id}")),
                    MValue::Integer(id % 1000),
                    MValue::Text(format!("cat_{}", id % 10)),
                ],
            );
        });
    });

    group.bench_function("sqlite", |b| {
        b.iter(|| {
            let id = rng.random_range(1..=10_000_i64);
            sdb.execute("DELETE FROM t WHERE id = ?1", [id]).unwrap();
            let _ = sdb.execute(
                "INSERT INTO t (id, name, value, category) VALUES (?1, ?2, ?3, ?4)",
                params![
                    id,
                    format!("name_{id}"),
                    id % 1000,
                    format!("cat_{}", id % 10),
                ],
            );
        });
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// 8. Aggregate (COUNT + SUM)
// ---------------------------------------------------------------------------

fn bench_aggregate(c: &mut Criterion) {
    let mut group = c.benchmark_group("aggregate");

    for &size in &[1_000, 10_000] {
        let (mdb, _d1) = manifold_populated(size);

        let (sdb, _d2) = sqlite_db();
        sqlite_create_table(&sdb);
        sqlite_populate(&sdb, size);

        group.bench_with_input(BenchmarkId::new("manifold", size), &size, |b, _| {
            b.iter(|| {
                mdb.query("SELECT COUNT(*), SUM(value) FROM t", &[])
                    .unwrap();
            });
        });

        group.bench_with_input(BenchmarkId::new("sqlite", size), &size, |b, _| {
            let mut stmt = sdb
                .prepare_cached("SELECT COUNT(*), SUM(value) FROM t")
                .unwrap();
            b.iter(|| {
                let _: Vec<(i64, i64)> = stmt
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                    .unwrap()
                    .map(|r| r.unwrap())
                    .collect();
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 9. GROUP BY
// ---------------------------------------------------------------------------

fn bench_group_by(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_by");

    for &size in &[1_000, 10_000] {
        let (mdb, _d1) = manifold_populated(size);

        let (sdb, _d2) = sqlite_db();
        sqlite_create_table(&sdb);
        sqlite_populate(&sdb, size);

        group.bench_with_input(BenchmarkId::new("manifold", size), &size, |b, _| {
            b.iter(|| {
                mdb.query(
                    "SELECT category, COUNT(*), SUM(value) FROM t GROUP BY category",
                    &[],
                )
                .unwrap();
            });
        });

        group.bench_with_input(BenchmarkId::new("sqlite", size), &size, |b, _| {
            let mut stmt = sdb
                .prepare_cached("SELECT category, COUNT(*), SUM(value) FROM t GROUP BY category")
                .unwrap();
            b.iter(|| {
                let _: Vec<(String, i64, i64)> = stmt
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                    .unwrap()
                    .map(|r| r.unwrap())
                    .collect();
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 10. Inner Join
// ---------------------------------------------------------------------------

fn bench_join(c: &mut Criterion) {
    let mut group = c.benchmark_group("inner_join");
    let mut rng = rand::rng();
    use rand::Rng;
    let size = 1_000;

    // Manifold — populate both tables, close/reopen to flush to B-tree
    let dir1 = tempfile::TempDir::new().unwrap();
    let mpath = dir1.path().join("bench.db");
    {
        let db = ManifoldDb::open(&mpath).unwrap();
        manifold_create_table(&db);
        manifold_populate(&db, size);
        db.execute(
            "CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER, val INTEGER)",
            &[],
        )
        .unwrap();
        db.execute("BEGIN", &[]).unwrap();
        for i in 0..size {
            db.execute(
                "INSERT INTO b (id, a_id, val) VALUES ($1, $2, $3)",
                &[
                    MValue::Integer(i as i64 + 1),
                    MValue::Integer(i as i64 + 1),
                    MValue::Integer((i * 10 % 1000) as i64),
                ],
            )
            .unwrap();
        }
        db.execute("COMMIT", &[]).unwrap();
    }
    let (mdb, _d1) = (ManifoldDb::open(&mpath).unwrap(), dir1);

    // SQLite
    let (sdb, _d2) = sqlite_db();
    sqlite_create_table(&sdb);
    sqlite_populate(&sdb, size);
    sdb.execute(
        "CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER, val INTEGER)",
        [],
    )
    .unwrap();
    sdb.execute("BEGIN", []).unwrap();
    for i in 0..size {
        sdb.execute(
            "INSERT INTO b (id, a_id, val) VALUES (?1, ?2, ?3)",
            params![i as i64 + 1, i as i64 + 1, (i * 10 % 1000) as i64],
        )
        .unwrap();
    }
    sdb.execute("COMMIT", []).unwrap();

    group.bench_function("manifold", |b| {
        b.iter(|| {
            let id = rng.random_range(1..=size as i64);
            mdb.query(
                "SELECT a.name, b.val FROM t AS a INNER JOIN b ON a.id = b.a_id WHERE a.id = $1",
                &[MValue::Integer(id)],
            )
            .unwrap();
        });
    });

    group.bench_function("sqlite", |b| {
        let mut stmt = sdb
            .prepare_cached(
                "SELECT a.name, b.val FROM t AS a INNER JOIN b ON a.id = b.a_id WHERE a.id = ?1",
            )
            .unwrap();
        b.iter(|| {
            let id = rng.random_range(1..=size as i64);
            let _: Vec<(String, i64)> = stmt
                .query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
        });
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// Criterion harness
// ---------------------------------------------------------------------------

criterion_group!(
    benches,
    bench_point_lookup,
    bench_full_scan,
    bench_range_scan,
    bench_single_insert,
    bench_bulk_insert,
    bench_update,
    bench_delete,
    bench_aggregate,
    bench_group_by,
    bench_join,
);
criterion_main!(benches);
