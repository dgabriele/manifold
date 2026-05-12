//! Social Feed API Workload Benchmark
//!
//! Compares manifold-sql vs SQLite (WAL, synchronous=NORMAL) under concurrent
//! load simulating a social media backend: users, posts, comments, likes.
//!
//! Run: `cargo bench -p manifold-sql --bench api_workload`

use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use manifold_sql::{Database as ManifoldDb, Durability, Value as MValue};
use rand::Rng;
use rusqlite::{params, Connection};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum OpType {
    CreatePost,
    AddComment,
    LikePost,
    GetFeed,
    GetPostDetail,
    GetUserProfile,
}

impl OpType {
    const ALL: [OpType; 6] = [
        OpType::CreatePost,
        OpType::AddComment,
        OpType::LikePost,
        OpType::GetFeed,
        OpType::GetPostDetail,
        OpType::GetUserProfile,
    ];

    fn label(self) -> &'static str {
        match self {
            OpType::CreatePost => "create_post",
            OpType::AddComment => "add_comment",
            OpType::LikePost => "like_post",
            OpType::GetFeed => "get_feed",
            OpType::GetPostDetail => "get_post_detail",
            OpType::GetUserProfile => "get_user_profile",
        }
    }
}

struct WorkloadResults {
    total_ops: u64,
    elapsed: Duration,
    latencies: Vec<(OpType, Duration)>,
}

// ---------------------------------------------------------------------------
// Schema & Seed Constants
// ---------------------------------------------------------------------------

const NUM_USERS: i64 = 100;
const NUM_POSTS: i64 = 1_000;
const NUM_COMMENTS: i64 = 5_000;
const NUM_LIKES: i64 = 10_000;

const SCHEMA: &[&str] = &[
    "CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT, created_at INTEGER)",
    "CREATE TABLE posts (id INTEGER PRIMARY KEY, author_id INTEGER, content TEXT, created_at INTEGER)",
    "CREATE TABLE comments (id INTEGER PRIMARY KEY, post_id INTEGER, author_id INTEGER, body TEXT, created_at INTEGER)",
    "CREATE TABLE likes (id INTEGER PRIMARY KEY, post_id INTEGER, user_id INTEGER)",
];

const RUN_DURATION: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Manifold setup
// ---------------------------------------------------------------------------

fn setup_manifold() -> (ManifoldDb, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    let db = ManifoldDb::open(dir.path().join("bench.db")).unwrap();
    db.set_durability(Durability::None);

    for ddl in SCHEMA {
        db.execute(ddl, &[]).unwrap();
    }

    let mut rng = rand::rng();

    // Users
    db.execute("BEGIN", &[]).unwrap();
    for i in 1..=NUM_USERS {
        db.execute(
            "INSERT INTO users (id, username, created_at) VALUES ($1, $2, $3)",
            &[
                MValue::Integer(i),
                MValue::Text(format!("user_{i}")),
                MValue::Integer(1_000_000 + i),
            ],
        )
        .unwrap();
    }
    db.execute("COMMIT", &[]).unwrap();

    // Posts
    db.execute("BEGIN", &[]).unwrap();
    for i in 1..=NUM_POSTS {
        db.execute(
            "INSERT INTO posts (id, author_id, content, created_at) VALUES ($1, $2, $3, $4)",
            &[
                MValue::Integer(i),
                MValue::Integer(rng.random_range(1..=NUM_USERS)),
                MValue::Text(format!("Post content #{i}")),
                MValue::Integer(2_000_000 + i),
            ],
        )
        .unwrap();
    }
    db.execute("COMMIT", &[]).unwrap();

    // Comments
    db.execute("BEGIN", &[]).unwrap();
    for i in 1..=NUM_COMMENTS {
        db.execute(
            "INSERT INTO comments (id, post_id, author_id, body, created_at) VALUES ($1, $2, $3, $4, $5)",
            &[
                MValue::Integer(i),
                MValue::Integer(rng.random_range(1..=NUM_POSTS)),
                MValue::Integer(rng.random_range(1..=NUM_USERS)),
                MValue::Text(format!("Comment body #{i}")),
                MValue::Integer(3_000_000 + i),
            ],
        )
        .unwrap();
    }
    db.execute("COMMIT", &[]).unwrap();

    // Likes
    db.execute("BEGIN", &[]).unwrap();
    for i in 1..=NUM_LIKES {
        db.execute(
            "INSERT INTO likes (id, post_id, user_id) VALUES ($1, $2, $3)",
            &[
                MValue::Integer(i),
                MValue::Integer(rng.random_range(1..=NUM_POSTS)),
                MValue::Integer(rng.random_range(1..=NUM_USERS)),
            ],
        )
        .unwrap();
    }
    db.execute("COMMIT", &[]).unwrap();

    (db, dir)
}

// ---------------------------------------------------------------------------
// SQLite setup
// ---------------------------------------------------------------------------

fn setup_sqlite() -> (Connection, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    let conn = Connection::open(dir.path().join("bench.db")).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
        .unwrap();

    for ddl in SCHEMA {
        conn.execute(ddl, []).unwrap();
    }

    let mut rng = rand::rng();

    // Users
    conn.execute("BEGIN", []).unwrap();
    for i in 1..=NUM_USERS {
        conn.execute(
            "INSERT INTO users (id, username, created_at) VALUES (?1, ?2, ?3)",
            params![i, format!("user_{i}"), 1_000_000 + i],
        )
        .unwrap();
    }
    conn.execute("COMMIT", []).unwrap();

    // Posts
    conn.execute("BEGIN", []).unwrap();
    for i in 1..=NUM_POSTS {
        conn.execute(
            "INSERT INTO posts (id, author_id, content, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                i,
                rng.random_range(1..=NUM_USERS),
                format!("Post content #{i}"),
                2_000_000 + i,
            ],
        )
        .unwrap();
    }
    conn.execute("COMMIT", []).unwrap();

    // Comments
    conn.execute("BEGIN", []).unwrap();
    for i in 1..=NUM_COMMENTS {
        conn.execute(
            "INSERT INTO comments (id, post_id, author_id, body, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                i,
                rng.random_range(1..=NUM_POSTS),
                rng.random_range(1..=NUM_USERS),
                format!("Comment body #{i}"),
                3_000_000 + i,
            ],
        )
        .unwrap();
    }
    conn.execute("COMMIT", []).unwrap();

    // Likes
    conn.execute("BEGIN", []).unwrap();
    for i in 1..=NUM_LIKES {
        conn.execute(
            "INSERT INTO likes (id, post_id, user_id) VALUES (?1, ?2, ?3)",
            params![
                i,
                rng.random_range(1..=NUM_POSTS),
                rng.random_range(1..=NUM_USERS),
            ],
        )
        .unwrap();
    }
    conn.execute("COMMIT", []).unwrap();

    (conn, dir)
}

// ---------------------------------------------------------------------------
// Operation dispatch helpers
// ---------------------------------------------------------------------------

fn pick_op(rng: &mut impl Rng) -> OpType {
    let r: u32 = rng.random_range(0..100);
    match r {
        0..15 => OpType::CreatePost,
        15..30 => OpType::AddComment,
        30..45 => OpType::LikePost,
        45..70 => OpType::GetFeed,
        70..90 => OpType::GetPostDetail,
        90..100 => OpType::GetUserProfile,
        _ => unreachable!(),
    }
}

// ---------------------------------------------------------------------------
// Manifold workload
// ---------------------------------------------------------------------------

fn run_op_manifold(db: &ManifoldDb, op: OpType, rng: &mut impl Rng, next_id: &mut i64) {
    match op {
        OpType::CreatePost => {
            *next_id += 1;
            let id = *next_id;
            db.execute(
                "INSERT INTO posts (id, author_id, content, created_at) VALUES ($1, $2, $3, $4)",
                &[
                    MValue::Integer(id),
                    MValue::Integer(rng.random_range(1..=NUM_USERS)),
                    MValue::Text(format!("New post #{id}")),
                    MValue::Integer(9_000_000 + id),
                ],
            )
            .unwrap();
        }
        OpType::AddComment => {
            *next_id += 1;
            let id = *next_id;
            db.execute(
                "INSERT INTO comments (id, post_id, author_id, body, created_at) VALUES ($1, $2, $3, $4, $5)",
                &[
                    MValue::Integer(id),
                    MValue::Integer(rng.random_range(1..=NUM_POSTS)),
                    MValue::Integer(rng.random_range(1..=NUM_USERS)),
                    MValue::Text(format!("New comment #{id}")),
                    MValue::Integer(9_000_000 + id),
                ],
            )
            .unwrap();
        }
        OpType::LikePost => {
            *next_id += 1;
            let id = *next_id;
            db.execute(
                "INSERT INTO likes (id, post_id, user_id) VALUES ($1, $2, $3)",
                &[
                    MValue::Integer(id),
                    MValue::Integer(rng.random_range(1..=NUM_POSTS)),
                    MValue::Integer(rng.random_range(1..=NUM_USERS)),
                ],
            )
            .unwrap();
        }
        OpType::GetFeed => {
            let _ = db.query(
                "SELECT id, author_id, content, created_at FROM posts ORDER BY created_at DESC LIMIT 20",
                &[],
            )
            .unwrap();
        }
        OpType::GetPostDetail => {
            let post_id = rng.random_range(1..=NUM_POSTS);
            let _ = db
                .query(
                    "SELECT id, author_id, body FROM comments WHERE post_id = $1",
                    &[MValue::Integer(post_id)],
                )
                .unwrap();
        }
        OpType::GetUserProfile => {
            let user_id = rng.random_range(1..=NUM_USERS);
            let _ = db
                .query(
                    "SELECT COUNT(*) FROM posts WHERE author_id = $1",
                    &[MValue::Integer(user_id)],
                )
                .unwrap();
        }
    }
}

fn run_workload_manifold(db: &Arc<ManifoldDb>, num_threads: usize) -> WorkloadResults {
    let barrier = Arc::new(Barrier::new(num_threads));
    let mut handles = Vec::with_capacity(num_threads);

    for t in 0..num_threads {
        let db = Arc::clone(db);
        let barrier = Arc::clone(&barrier);
        // Give each thread a unique ID range to avoid PK collisions.
        // Start well above seeded data.
        let id_base = 100_000_000 + (t as i64) * 10_000_000;

        handles.push(thread::spawn(move || {
            let mut rng = rand::rng();
            let mut latencies: Vec<(OpType, Duration)> = Vec::with_capacity(128_000);
            let mut next_id = id_base;

            barrier.wait();
            let start = Instant::now();

            while start.elapsed() < RUN_DURATION {
                let op = pick_op(&mut rng);
                let t0 = Instant::now();
                run_op_manifold(&db, op, &mut rng, &mut next_id);
                latencies.push((op, t0.elapsed()));
            }

            latencies
        }));
    }

    let start = Instant::now();
    let mut all_latencies: Vec<(OpType, Duration)> = Vec::new();
    for h in handles {
        all_latencies.extend(h.join().unwrap());
    }
    // Use actual wall-clock time (threads ran concurrently).
    // Approximate: RUN_DURATION + small overhead.
    let elapsed = start.elapsed();

    WorkloadResults {
        total_ops: all_latencies.len() as u64,
        elapsed,
        latencies: all_latencies,
    }
}

// ---------------------------------------------------------------------------
// SQLite workload
// ---------------------------------------------------------------------------

fn run_op_sqlite(
    conn: &Mutex<Connection>,
    op: OpType,
    rng: &mut impl Rng,
    next_id: &mut i64,
) {
    let conn = conn.lock().unwrap();
    match op {
        OpType::CreatePost => {
            *next_id += 1;
            let id = *next_id;
            conn.execute(
                "INSERT INTO posts (id, author_id, content, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![
                    id,
                    rng.random_range(1..=NUM_USERS),
                    format!("New post #{id}"),
                    9_000_000 + id,
                ],
            )
            .unwrap();
        }
        OpType::AddComment => {
            *next_id += 1;
            let id = *next_id;
            conn.execute(
                "INSERT INTO comments (id, post_id, author_id, body, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    id,
                    rng.random_range(1..=NUM_POSTS),
                    rng.random_range(1..=NUM_USERS),
                    format!("New comment #{id}"),
                    9_000_000 + id,
                ],
            )
            .unwrap();
        }
        OpType::LikePost => {
            *next_id += 1;
            let id = *next_id;
            conn.execute(
                "INSERT INTO likes (id, post_id, user_id) VALUES (?1, ?2, ?3)",
                params![id, rng.random_range(1..=NUM_POSTS), rng.random_range(1..=NUM_USERS)],
            )
            .unwrap();
        }
        OpType::GetFeed => {
            let mut stmt = conn
                .prepare_cached(
                    "SELECT id, author_id, content, created_at FROM posts ORDER BY created_at DESC LIMIT 20",
                )
                .unwrap();
            let _ = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
        }
        OpType::GetPostDetail => {
            let post_id = rng.random_range(1..=NUM_POSTS);
            let mut stmt = conn
                .prepare_cached("SELECT id, author_id, body FROM comments WHERE post_id = ?1")
                .unwrap();
            let _ = stmt
                .query_map(params![post_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
        }
        OpType::GetUserProfile => {
            let user_id = rng.random_range(1..=NUM_USERS);
            let mut stmt = conn
                .prepare_cached("SELECT COUNT(*) FROM posts WHERE author_id = ?1")
                .unwrap();
            let _: i64 = stmt.query_row(params![user_id], |row| row.get(0)).unwrap();
        }
    }
}

fn run_workload_sqlite(conn: &Arc<Mutex<Connection>>, num_threads: usize) -> WorkloadResults {
    let barrier = Arc::new(Barrier::new(num_threads));
    let mut handles = Vec::with_capacity(num_threads);

    for t in 0..num_threads {
        let conn = Arc::clone(conn);
        let barrier = Arc::clone(&barrier);
        let id_base = 100_000_000 + (t as i64) * 10_000_000;

        handles.push(thread::spawn(move || {
            let mut rng = rand::rng();
            let mut latencies: Vec<(OpType, Duration)> = Vec::with_capacity(128_000);
            let mut next_id = id_base;

            barrier.wait();
            let start = Instant::now();

            while start.elapsed() < RUN_DURATION {
                let op = pick_op(&mut rng);
                let t0 = Instant::now();
                run_op_sqlite(&conn, op, &mut rng, &mut next_id);
                latencies.push((op, t0.elapsed()));
            }

            latencies
        }));
    }

    let start = Instant::now();
    let mut all_latencies: Vec<(OpType, Duration)> = Vec::new();
    for h in handles {
        all_latencies.extend(h.join().unwrap());
    }
    let elapsed = start.elapsed();

    WorkloadResults {
        total_ops: all_latencies.len() as u64,
        elapsed,
        latencies: all_latencies,
    }
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((sorted.len() as f64) * p).ceil() as usize;
    let idx = idx.min(sorted.len()).saturating_sub(1);
    sorted[idx]
}

fn format_duration(d: Duration) -> String {
    let us = d.as_micros();
    if us < 1_000 {
        format!("{} us", us)
    } else if us < 1_000_000 {
        format!("{:.1} ms", us as f64 / 1_000.0)
    } else {
        format!("{:.2} s", us as f64 / 1_000_000.0)
    }
}

fn format_rate(count: u64, elapsed: Duration) -> String {
    let rate = count as f64 / elapsed.as_secs_f64();
    if rate >= 1_000.0 {
        format!("{:.0}", rate)
    } else {
        format!("{:.1}", rate)
    }
}

fn print_results(num_threads: usize, m: &WorkloadResults, s: &WorkloadResults) {
    println!("Threads: {num_threads}");
    println!(
        "  {:20} {:>14} {:>14}",
        "", "manifold", "sqlite"
    );

    // Total ops/sec
    println!(
        "  {:20} {:>14} {:>14}",
        "ops/sec",
        format_rate(m.total_ops, m.elapsed),
        format_rate(s.total_ops, s.elapsed),
    );

    // Latency percentiles
    let mut m_all: Vec<Duration> = m.latencies.iter().map(|(_, d)| *d).collect();
    let mut s_all: Vec<Duration> = s.latencies.iter().map(|(_, d)| *d).collect();
    m_all.sort();
    s_all.sort();

    println!(
        "  {:20} {:>14} {:>14}",
        "p50 latency",
        format_duration(percentile(&m_all, 0.50)),
        format_duration(percentile(&s_all, 0.50)),
    );
    println!(
        "  {:20} {:>14} {:>14}",
        "p99 latency",
        format_duration(percentile(&m_all, 0.99)),
        format_duration(percentile(&s_all, 0.99)),
    );

    // Per-operation breakdown
    println!();
    for op in OpType::ALL {
        let m_count = m.latencies.iter().filter(|(o, _)| *o == op).count() as u64;
        let s_count = s.latencies.iter().filter(|(o, _)| *o == op).count() as u64;
        println!(
            "  {:20} {:>12}/s {:>12}/s",
            op.label(),
            format_rate(m_count, m.elapsed),
            format_rate(s_count, s.elapsed),
        );
    }
    println!();
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    println!("=== Social Feed API Benchmark ===");
    println!();

    for &num_threads in &[1, 2, 4, 8, 16] {
        // --- Manifold ---
        eprint!("  Setting up manifold ({num_threads} threads)...");
        let (m_db, _m_dir) = setup_manifold();
        let m_db = Arc::new(m_db);
        eprintln!(" running...");
        let m_results = run_workload_manifold(&m_db, num_threads);

        // --- SQLite ---
        eprint!("  Setting up sqlite ({num_threads} threads)...");
        let (s_conn, _s_dir) = setup_sqlite();
        let s_conn = Arc::new(Mutex::new(s_conn));
        eprintln!(" running...");
        let s_results = run_workload_sqlite(&s_conn, num_threads);

        // --- Report ---
        print_results(num_threads, &m_results, &s_results);
    }
}
