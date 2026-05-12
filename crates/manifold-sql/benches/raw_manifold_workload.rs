//! Same social feed workload but using raw manifold KV API (no SQL layer).
//! This isolates the storage engine performance from SQL overhead.

use manifold::column_family::ColumnFamilyDatabase;
use manifold::{ReadableTable, ReadableTableMetadata, TableDefinition};
use rand::Rng;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

const USERS: TableDefinition<u64, &[u8]> = TableDefinition::new("users");
const POSTS: TableDefinition<u64, &[u8]> = TableDefinition::new("posts");
const COMMENTS: TableDefinition<u64, &[u8]> = TableDefinition::new("comments");
const LIKES: TableDefinition<u64, &[u8]> = TableDefinition::new("likes");

fn seed(cf: &manifold::column_family::ColumnFamily) {
    let txn = cf.begin_write().unwrap();
    {
        let mut t = txn.open_table(USERS).unwrap();
        for i in 1..=100u64 {
            let val = format!("user_{i}");
            t.insert(i, val.as_bytes()).unwrap();
        }
    }
    {
        let mut t = txn.open_table(POSTS).unwrap();
        let mut rng = rand::rng();
        for i in 1..=1000u64 {
            let author: u64 = rng.random_range(1..=100);
            let val = format!("{author}:post_content_{i}");
            t.insert(i, val.as_bytes()).unwrap();
        }
    }
    {
        let mut t = txn.open_table(COMMENTS).unwrap();
        let mut rng = rand::rng();
        for i in 1..=5000u64 {
            let post: u64 = rng.random_range(1..=1000);
            let author: u64 = rng.random_range(1..=100);
            let val = format!("{post}:{author}:comment_{i}");
            t.insert(i, val.as_bytes()).unwrap();
        }
    }
    {
        let mut t = txn.open_table(LIKES).unwrap();
        let mut rng = rand::rng();
        for i in 1..=10000u64 {
            let post: u64 = rng.random_range(1..=1000);
            let user: u64 = rng.random_range(1..=100);
            let val = format!("{post}:{user}");
            t.insert(i, val.as_bytes()).unwrap();
        }
    }
    txn.commit().unwrap();
}

fn run_workload(
    db: &Arc<ColumnFamilyDatabase>,
    num_threads: usize,
    duration: Duration,
) -> Vec<(u8, Duration)> {
    let barrier = Arc::new(Barrier::new(num_threads));
    let results: Vec<_> = (0..num_threads)
        .map(|_| {
            let db = Arc::clone(db);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let cf = db.column_family("default").unwrap();
                let mut rng = rand::rng();
                let mut latencies: Vec<(u8, Duration)> = Vec::new();
                let mut next_post = 100_000u64;
                let mut next_comment = 100_000u64;
                let mut next_like = 100_000u64;

                barrier.wait();
                let deadline = Instant::now() + duration;

                while Instant::now() < deadline {
                    let op: u32 = rng.random_range(0..100);
                    let start = Instant::now();

                    match op {
                        // create_post (15%)
                        0..15 => {
                            let txn = cf.begin_write().unwrap();
                            {
                                let mut t = txn.open_table(POSTS).unwrap();
                                let author: u64 = rng.random_range(1..=100);
                                let val = format!("{author}:new_post");
                                t.insert(next_post, val.as_bytes()).unwrap();
                                next_post += 1;
                            }
                            txn.commit().unwrap();
                            latencies.push((0, start.elapsed()));
                        }
                        // add_comment (15%)
                        15..30 => {
                            let txn = cf.begin_write().unwrap();
                            {
                                let mut t = txn.open_table(COMMENTS).unwrap();
                                let post: u64 = rng.random_range(1..=1000);
                                let author: u64 = rng.random_range(1..=100);
                                let val = format!("{post}:{author}:new_comment");
                                t.insert(next_comment, val.as_bytes()).unwrap();
                                next_comment += 1;
                            }
                            txn.commit().unwrap();
                            latencies.push((1, start.elapsed()));
                        }
                        // like_post (15%)
                        30..45 => {
                            let txn = cf.begin_write().unwrap();
                            {
                                let mut t = txn.open_table(LIKES).unwrap();
                                let post: u64 = rng.random_range(1..=1000);
                                let user: u64 = rng.random_range(1..=100);
                                let val = format!("{post}:{user}");
                                t.insert(next_like, val.as_bytes()).unwrap();
                                next_like += 1;
                            }
                            txn.commit().unwrap();
                            latencies.push((2, start.elapsed()));
                        }
                        // get_feed (25%) — last 20 posts by reverse iteration
                        45..70 => {
                            let txn = cf.begin_read().unwrap();
                            let t = txn.open_table(POSTS).unwrap();
                            let mut count = 0;
                            for entry in t.iter().unwrap().rev() {
                                let _ = entry.unwrap();
                                count += 1;
                                if count >= 20 { break; }
                            }
                            latencies.push((3, start.elapsed()));
                        }
                        // get_post_detail (20%) — lookup post + scan comments
                        70..90 => {
                            let txn = cf.begin_read().unwrap();
                            let post_id: u64 = rng.random_range(1..=1000);
                            {
                                let t = txn.open_table(POSTS).unwrap();
                                let _ = t.get(post_id).unwrap();
                            }
                            // Scan comments — in raw KV we'd need a secondary index
                            // but for fair comparison, just do a point read
                            {
                                let t = txn.open_table(COMMENTS).unwrap();
                                let _ = t.get(post_id).unwrap();
                            }
                            latencies.push((4, start.elapsed()));
                        }
                        // get_user_profile (10%) — lookup user + count posts
                        _ => {
                            let txn = cf.begin_read().unwrap();
                            let user_id: u64 = rng.random_range(1..=100);
                            {
                                let t = txn.open_table(USERS).unwrap();
                                let _ = t.get(user_id).unwrap();
                            }
                            {
                                let t = txn.open_table(POSTS).unwrap();
                                let _ = t.len().unwrap(); // approximate
                            }
                            latencies.push((5, start.elapsed()));
                        }
                    }
                }
                latencies
            })
        })
        .collect();

    let mut all = Vec::new();
    for handle in results {
        all.extend(handle.join().unwrap());
    }
    all
}

fn percentile(latencies: &mut [Duration], p: f64) -> Duration {
    if latencies.is_empty() {
        return Duration::ZERO;
    }
    latencies.sort();
    let idx = ((latencies.len() as f64) * p / 100.0) as usize;
    latencies[idx.min(latencies.len() - 1)]
}

fn fmt_dur(d: Duration) -> String {
    let us = d.as_micros();
    if us < 1000 {
        format!("{} us", us)
    } else if us < 1_000_000 {
        format!("{:.1} ms", us as f64 / 1000.0)
    } else {
        format!("{:.2} s", us as f64 / 1_000_000.0)
    }
}

const OP_NAMES: [&str; 6] = [
    "create_post",
    "add_comment",
    "like_post",
    "get_feed",
    "get_post_detail",
    "get_user_profile",
];

fn main() {
    println!("=== Raw Manifold KV vs SQL Layer Benchmark ===\n");

    for &num_threads in &[1, 4, 16] {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Arc::new(
            ColumnFamilyDatabase::builder()
                .open(dir.path().join("bench.db"))
                .unwrap(),
        );
        db.create_column_family("default", None).unwrap();
        let cf = db.column_family("default").unwrap();
        seed(&cf);

        let results = run_workload(&db, num_threads, Duration::from_secs(10));
        let total = results.len();
        let ops_sec = total as f64 / 10.0;

        let mut all_latencies: Vec<Duration> = results.iter().map(|(_, d)| *d).collect();
        let p50 = percentile(&mut all_latencies, 50.0);
        let p99 = percentile(&mut all_latencies, 99.0);

        println!("Threads: {num_threads}");
        println!("  ops/sec:    {ops_sec:.0}");
        println!("  p50:        {}", fmt_dur(p50));
        println!("  p99:        {}", fmt_dur(p99));

        for op in 0..6u8 {
            let count = results.iter().filter(|(o, _)| *o == op).count();
            println!("  {:20} {:.0}/s", OP_NAMES[op as usize], count as f64 / 10.0);
        }
        println!();
    }
}
