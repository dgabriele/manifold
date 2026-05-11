use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use manifold_sql::{Database, Value};
use rand::Rng;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn create_db() -> (Database, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open(dir.path().join("bench.db")).unwrap();
    (db, dir)
}

fn create_table(db: &Database) {
    db.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, value INTEGER, category TEXT)",
        &[],
    )
    .unwrap();
}

fn populate_table(db: &Database, n: usize) {
    db.execute("BEGIN", &[]).unwrap();
    for i in 0..n {
        db.execute(
            "INSERT INTO t (id, name, value, category) VALUES ($1, $2, $3, $4)",
            &[
                Value::Integer(i as i64 + 1),
                Value::Text(format!("name_{i}")),
                Value::Integer((i % 1000) as i64),
                Value::Text(format!("cat_{}", i % 10)),
            ],
        )
        .unwrap();
    }
    db.execute("COMMIT", &[]).unwrap();
}

// ---------------------------------------------------------------------------
// 1. Point Lookup (PK)
// ---------------------------------------------------------------------------

fn bench_point_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("point_lookup");
    let mut rng = rand::rng();

    for &size in &[100, 1_000, 10_000, 100_000] {
        if size >= 100_000 {
            group.sample_size(10);
        }

        let (db, _dir) = create_db();
        create_table(&db);
        populate_table(&db, size);

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let id = rng.random_range(1..=size as i64);
                db.query("SELECT * FROM t WHERE id = $1", &[Value::Integer(id)])
                    .unwrap();
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 1b. Point Lookup (PK) — Prepared Statement
// ---------------------------------------------------------------------------

fn bench_point_lookup_prepared(c: &mut Criterion) {
    let mut group = c.benchmark_group("point_lookup_prepared");
    let mut rng = rand::rng();

    for &size in &[100, 1_000, 10_000, 100_000] {
        if size >= 100_000 {
            group.sample_size(10);
        }

        let (db, _dir) = create_db();
        create_table(&db);
        populate_table(&db, size);

        let stmt = db
            .prepare("SELECT id, name, value, category FROM t WHERE id = $1")
            .unwrap();

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let id = rng.random_range(1..=size as i64);
                stmt.query(&db, &[Value::Integer(id)]).unwrap();
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 2. Range Scan
// ---------------------------------------------------------------------------

fn bench_range_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("range_scan");
    let mut rng = rand::rng();

    for &size in &[1_000, 10_000, 100_000] {
        if size >= 100_000 {
            group.sample_size(10);
        }

        let (db, _dir) = create_db();
        create_table(&db);
        populate_table(&db, size);

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let start = rng.random_range(1..=(size as i64 - 100));
                let end = start + 99;
                db.query(
                    "SELECT * FROM t WHERE id BETWEEN $1 AND $2",
                    &[Value::Integer(start), Value::Integer(end)],
                )
                .unwrap();
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 3. Full Table Scan
// ---------------------------------------------------------------------------

fn bench_full_table_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_table_scan");

    for &size in &[100, 1_000, 10_000] {
        let (db, _dir) = create_db();
        create_table(&db);
        populate_table(&db, size);

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                db.query("SELECT * FROM t", &[]).unwrap();
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 4. Single Insert
// ---------------------------------------------------------------------------

fn bench_single_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_insert");

    let (db, _dir) = create_db();
    create_table(&db);
    populate_table(&db, 1_000);

    let counter = std::cell::Cell::new(2_000_i64);

    group.bench_function("insert", |b| {
        b.iter(|| {
            let id = counter.get();
            counter.set(id + 1);
            db.execute(
                "INSERT INTO t (id, name, value, category) VALUES ($1, $2, $3, $4)",
                &[
                    Value::Integer(id),
                    Value::Text(format!("name_{id}")),
                    Value::Integer(id % 1000),
                    Value::Text(format!("cat_{}", id % 10)),
                ],
            )
            .unwrap();
        });
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// 4b. Single Insert — Prepared Statement
// ---------------------------------------------------------------------------

fn bench_single_insert_prepared(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_insert_prepared");

    let (db, _dir) = create_db();
    create_table(&db);
    populate_table(&db, 1_000);

    let stmt = db
        .prepare("INSERT INTO t (id, name, value, category) VALUES ($1, $2, $3, $4)")
        .unwrap();

    let counter = std::cell::Cell::new(2_000_i64);

    group.bench_function("insert", |b| {
        b.iter(|| {
            let id = counter.get();
            counter.set(id + 1);
            stmt.execute(
                &db,
                &[
                    Value::Integer(id),
                    Value::Text(format!("name_{id}")),
                    Value::Integer(id % 1000),
                    Value::Text(format!("cat_{}", id % 10)),
                ],
            )
            .unwrap();
        });
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// 5. Bulk Insert (1000 rows in one transaction)
// ---------------------------------------------------------------------------

fn bench_bulk_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("bulk_insert");
    group.sample_size(10);

    let (db, _dir) = create_db();
    create_table(&db);

    let counter = std::cell::Cell::new(1_i64);

    group.bench_function("1000_rows", |b| {
        b.iter(|| {
            let base = counter.get();
            counter.set(base + 1000);
            db.execute("BEGIN", &[]).unwrap();
            for i in 0..1000 {
                let id = base + i;
                db.execute(
                    "INSERT INTO t (id, name, value, category) VALUES ($1, $2, $3, $4)",
                    &[
                        Value::Integer(id),
                        Value::Text(format!("name_{id}")),
                        Value::Integer(id % 1000),
                        Value::Text(format!("cat_{}", id % 10)),
                    ],
                )
                .unwrap();
            }
            db.execute("COMMIT", &[]).unwrap();
        });
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// 5b. Bulk insert with prepared statements (isolates write perf from parse overhead)
// ---------------------------------------------------------------------------

fn bench_bulk_insert_prepared(c: &mut Criterion) {
    let mut group = c.benchmark_group("bulk_insert_prepared");
    group.sample_size(10);

    let (db, _dir) = create_db();
    create_table(&db);

    let stmt = db
        .prepare("INSERT INTO t (id, name, value, category) VALUES ($1, $2, $3, $4)")
        .unwrap();

    let counter = std::cell::Cell::new(1_000_000_i64);

    group.bench_function("1000_rows", |b| {
        b.iter(|| {
            let base = counter.get();
            counter.set(base + 1000);
            db.execute("BEGIN", &[]).unwrap();
            for i in 0..1000 {
                let id = base + i;
                stmt.execute(
                    &db,
                    &[
                        Value::Integer(id),
                        Value::Text(format!("name_{id}")),
                        Value::Integer(id % 1000),
                        Value::Text(format!("cat_{}", id % 10)),
                    ],
                )
                .unwrap();
            }
            db.execute("COMMIT", &[]).unwrap();
        });
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// 6. Update by PK
// ---------------------------------------------------------------------------

fn bench_update_by_pk(c: &mut Criterion) {
    let mut group = c.benchmark_group("update_by_pk");
    let mut rng = rand::rng();

    let (db, _dir) = create_db();
    create_table(&db);
    populate_table(&db, 10_000);

    group.bench_function("10000_rows", |b| {
        b.iter(|| {
            let id = rng.random_range(1..=10_000_i64);
            db.execute(
                "UPDATE t SET value = $1 WHERE id = $2",
                &[Value::Integer(999), Value::Integer(id)],
            )
            .unwrap();
        });
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// 7. Delete by PK
// ---------------------------------------------------------------------------

fn bench_delete_by_pk(c: &mut Criterion) {
    let mut group = c.benchmark_group("delete_by_pk");
    let mut rng = rand::rng();

    let (db, _dir) = create_db();
    create_table(&db);
    populate_table(&db, 10_000);

    group.bench_function("10000_rows", |b| {
        b.iter(|| {
            let id = rng.random_range(1..=10_000_i64);
            // Delete (may be a no-op if already deleted, but that's fine)
            db.execute(
                "DELETE FROM t WHERE id = $1",
                &[Value::Integer(id)],
            )
            .unwrap();
            // Re-insert to maintain table size
            let _ = db.execute(
                "INSERT INTO t (id, name, value, category) VALUES ($1, $2, $3, $4)",
                &[
                    Value::Integer(id),
                    Value::Text(format!("name_{id}")),
                    Value::Integer((id % 1000) as i64),
                    Value::Text(format!("cat_{}", id % 10)),
                ],
            );
        });
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// 8. Inner Join (2 tables)
// ---------------------------------------------------------------------------

fn bench_inner_join(c: &mut Criterion) {
    let mut group = c.benchmark_group("inner_join");
    let mut rng = rand::rng();

    for &size in &[100, 1_000] {
        let (db, _dir) = create_db();
        create_table(&db);
        populate_table(&db, size);

        db.execute(
            "CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER, value INTEGER)",
            &[],
        )
        .unwrap();

        // Populate table b with 1 row per row in t
        db.execute("BEGIN", &[]).unwrap();
        for i in 0..size {
            db.execute(
                "INSERT INTO b (id, a_id, value) VALUES ($1, $2, $3)",
                &[
                    Value::Integer(i as i64 + 1),
                    Value::Integer(i as i64 + 1),
                    Value::Integer((i * 10 % 1000) as i64),
                ],
            )
            .unwrap();
        }
        db.execute("COMMIT", &[]).unwrap();

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                let id = rng.random_range(1..=size as i64);
                db.query(
                    "SELECT a.name, b.value FROM t AS a INNER JOIN b ON a.id = b.a_id WHERE a.id = $1",
                    &[Value::Integer(id)],
                )
                .unwrap();
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 9. Aggregate (COUNT + SUM)
// ---------------------------------------------------------------------------

fn bench_aggregate(c: &mut Criterion) {
    let mut group = c.benchmark_group("aggregate_count_sum");

    for &size in &[1_000, 10_000] {
        let (db, _dir) = create_db();
        create_table(&db);
        populate_table(&db, size);

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                db.query("SELECT COUNT(*), SUM(value) FROM t", &[]).unwrap();
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// 10. Group By Aggregate
// ---------------------------------------------------------------------------

fn bench_group_by_aggregate(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_by_aggregate");

    for &size in &[1_000, 10_000] {
        let (db, _dir) = create_db();
        create_table(&db);
        populate_table(&db, size);

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                db.query(
                    "SELECT category, COUNT(*), SUM(value) FROM t GROUP BY category",
                    &[],
                )
                .unwrap();
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Criterion harness
// ---------------------------------------------------------------------------

criterion_group!(
    benches,
    bench_point_lookup,
    bench_point_lookup_prepared,
    bench_range_scan,
    bench_full_table_scan,
    bench_single_insert,
    bench_single_insert_prepared,
    bench_bulk_insert,
    bench_bulk_insert_prepared,
    bench_update_by_pk,
    bench_delete_by_pk,
    bench_inner_join,
    bench_aggregate,
    bench_group_by_aggregate,
);
criterion_main!(benches);
