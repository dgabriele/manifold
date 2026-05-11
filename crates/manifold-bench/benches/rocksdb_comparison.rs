//! Manifold vs RocksDB Comparative Benchmark
//!
//! Runs 8 workloads at escalating dataset sizes (100K → 10M keys) to identify
//! crossover points where one engine overtakes the other. Both engines use
//! recommended production defaults — no exotic tuning.
//!
//! Workloads:
//! 1. Sequential write (LSM append vs B-tree page splits)
//! 2. Random write (memtable absorption vs COW scattered writes)
//! 3. Point read — uniform (B-tree direct vs LSM multi-level + bloom)
//! 4. Point read — Zipfian (cache effectiveness under skew)
//! 5. Range scan (contiguous B-tree pages vs LSM level merging)
//! 6. Mixed 50/50 read/write (realistic OLTP)
//! 7. Durable write (WAL group commit vs RocksDB WAL)
//! 8. Concurrent CF writers (independent locks vs shared WAL)

use std::env::current_dir;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use std::{fs, process};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const KEY_SIZE: usize = 8; // u64 big-endian
const VALUE_SIZE: usize = 100;
const BATCH_SIZE: usize = 1000;
const RNG_SEED: u64 = 42;
const CACHE_SIZE: usize = 1024 * 1024 * 1024; // 1GB

const SCALES: &[u64] = &[100_000, 500_000, 1_000_000, 5_000_000, 10_000_000];

// For workloads that are time-bounded (mixed), duration in seconds
const MIXED_DURATION_SECS: u64 = 5;
const MIXED_THREADS: usize = 4;

// For range scan workload
const RANGE_SCAN_COUNT: u64 = 10_000;
const RANGE_SCAN_LENGTH: u64 = 100;

// For concurrent CF workload
const NUM_CFS: usize = 8;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_value(seed: u64) -> Vec<u8> {
    let mut v = vec![0u8; VALUE_SIZE];
    let bytes = seed.to_le_bytes();
    for (i, b) in v.iter_mut().enumerate() {
        *b = bytes[i % 8];
    }
    v
}

fn key_bytes(k: u64) -> [u8; 8] {
    k.to_be_bytes()
}

/// Simple deterministic shuffle using a linear congruential generator
fn shuffled_keys(n: u64, seed: u64) -> Vec<u64> {
    let mut keys: Vec<u64> = (0..n).collect();
    let mut rng = seed;
    for i in (1..keys.len()).rev() {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let j = (rng >> 33) as usize % (i + 1);
        keys.swap(i, j);
    }
    keys
}

fn zipfian_key(counter: u64, num_items: u64) -> u64 {
    let hot_threshold = num_items / 5; // top 20%
    let selector = counter % 100;
    if selector < 80 {
        (counter.wrapping_mul(2654435761)) % hot_threshold
    } else {
        hot_threshold + ((counter.wrapping_mul(2654435761)) % (num_items - hot_threshold))
    }
}

fn format_ops(ops: f64) -> String {
    if ops >= 1_000_000.0 {
        format!("{:.2}M", ops / 1_000_000.0)
    } else if ops >= 1_000.0 {
        format!("{:.1}K", ops / 1_000.0)
    } else {
        format!("{:.0}", ops)
    }
}

// ---------------------------------------------------------------------------
// Result collection
// ---------------------------------------------------------------------------

struct WorkloadResult {
    scale: u64,
    manifold_ops: f64,
    rocksdb_ops: f64,
}

impl WorkloadResult {
    fn ratio(&self) -> f64 {
        if self.rocksdb_ops > 0.0 {
            self.manifold_ops / self.rocksdb_ops
        } else {
            f64::INFINITY
        }
    }
    fn winner(&self) -> &str {
        if self.manifold_ops >= self.rocksdb_ops {
            "Manifold"
        } else {
            "RocksDB"
        }
    }
}

fn print_workload_table(name: &str, results: &[WorkloadResult]) {
    println!("\n=== {} ===", name);
    println!(
        "  {:>10} | {:>18} | {:>18} | {:>12} | {}",
        "Scale", "Manifold ops/s", "RocksDB ops/s", "Ratio (M/R)", "Winner"
    );
    println!("  {}", "-".repeat(82));
    for r in results {
        println!(
            "  {:>10} | {:>18} | {:>18} | {:>11.2}x | {}",
            format!("{}K", r.scale / 1000),
            format_ops(r.manifold_ops),
            format_ops(r.rocksdb_ops),
            r.ratio(),
            r.winner()
        );
    }
}

// ---------------------------------------------------------------------------
// Manifold helpers
// ---------------------------------------------------------------------------

fn open_manifold_cf(
    dir: &std::path::Path,
    with_wal: bool,
) -> (TempDir, Arc<manifold::column_family::ColumnFamilyDatabase>) {
    use manifold::column_family::ColumnFamilyDatabase;

    let tmpdir = TempDir::new_in(dir).unwrap();
    let db = if with_wal {
        ColumnFamilyDatabase::builder()
            .pool_size(64)
            .open(tmpdir.path().join("db"))
            .unwrap()
    } else {
        ColumnFamilyDatabase::builder()
            .without_wal()
            .open(tmpdir.path().join("db"))
            .unwrap()
    };
    db.create_column_family("default", None).unwrap();
    (tmpdir, Arc::new(db))
}

fn manifold_cf(
    db: &manifold::column_family::ColumnFamilyDatabase,
    name: &str,
) -> manifold::column_family::ColumnFamily {
    db.column_family(name).unwrap()
}

// ---------------------------------------------------------------------------
// RocksDB helpers
// ---------------------------------------------------------------------------

fn open_rocksdb(dir: &std::path::Path) -> (TempDir, Arc<rocksdb::OptimisticTransactionDB>) {
    let tmpdir = TempDir::new_in(dir).unwrap();

    let cache = rocksdb::Cache::new_lru_cache(CACHE_SIZE);
    let mut bb = rocksdb::BlockBasedOptions::default();
    bb.set_block_cache(&cache);
    bb.set_bloom_filter(10.0, false);
    bb.set_cache_index_and_filter_blocks(true);

    let mut opts = rocksdb::Options::default();
    opts.set_block_based_table_factory(&bb);
    opts.create_if_missing(true);
    opts.increase_parallelism(
        std::thread::available_parallelism().map_or(4, |n| n.get()) as i32,
    );

    let db = rocksdb::OptimisticTransactionDB::open(&opts, tmpdir.path().join("db")).unwrap();
    (tmpdir, Arc::new(db))
}

fn open_rocksdb_cf(
    dir: &std::path::Path,
    cf_names: &[String],
) -> (TempDir, Arc<rocksdb::DB>) {
    use rocksdb::{ColumnFamilyDescriptor, Options, DB};

    let tmpdir = TempDir::new_in(dir).unwrap();

    let cache = rocksdb::Cache::new_lru_cache(CACHE_SIZE);
    let mut bb = rocksdb::BlockBasedOptions::default();
    bb.set_block_cache(&cache);
    bb.set_bloom_filter(10.0, false);
    bb.set_cache_index_and_filter_blocks(true);

    let mut opts = Options::default();
    opts.set_block_based_table_factory(&bb);
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);
    opts.increase_parallelism(
        std::thread::available_parallelism().map_or(4, |n| n.get()) as i32,
    );

    let mut cfs = vec![ColumnFamilyDescriptor::new("default", Options::default())];
    for name in cf_names {
        cfs.push(ColumnFamilyDescriptor::new(name, Options::default()));
    }

    let db = DB::open_cf_descriptors(&opts, tmpdir.path().join("db"), cfs).unwrap();
    (tmpdir, Arc::new(db))
}

// ===========================================================================
// Workload 1: Sequential Write
// ===========================================================================

fn bench_sequential_write(dir: &std::path::Path, n: u64) -> WorkloadResult {
    use manifold::TableDefinition;
    const TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("data");

    // Manifold
    let (_tmp, db) = open_manifold_cf(dir, true);
    let cf = manifold_cf(&db, "default");
    let start = Instant::now();
    for batch_start in (0..n).step_by(BATCH_SIZE) {
        let txn = cf.begin_write().unwrap();
        let mut t = txn.open_table(TABLE).unwrap();
        for i in batch_start..std::cmp::min(batch_start + BATCH_SIZE as u64, n) {
            t.insert(&i, make_value(i).as_slice()).unwrap();
        }
        drop(t);
        txn.commit().unwrap();
    }
    let manifold_dur = start.elapsed();
    drop(cf);
    drop(db);
    drop(_tmp);

    // RocksDB
    let (_tmp, db) = open_rocksdb(dir);
    let start = Instant::now();
    for batch_start in (0..n).step_by(BATCH_SIZE) {
        let txn = db.transaction();
        for i in batch_start..std::cmp::min(batch_start + BATCH_SIZE as u64, n) {
            txn.put(&key_bytes(i), &make_value(i)).unwrap();
        }
        txn.commit().unwrap();
    }
    let rocksdb_dur = start.elapsed();
    drop(db);
    drop(_tmp);

    WorkloadResult {
        scale: n,
        manifold_ops: n as f64 / manifold_dur.as_secs_f64(),
        rocksdb_ops: n as f64 / rocksdb_dur.as_secs_f64(),
    }
}

// ===========================================================================
// Workload 2: Random Write
// ===========================================================================

fn bench_random_write(dir: &std::path::Path, n: u64) -> WorkloadResult {
    use manifold::TableDefinition;
    const TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("data");

    let keys = shuffled_keys(n, RNG_SEED);

    // Manifold
    let (_tmp, db) = open_manifold_cf(dir, true);
    let cf = manifold_cf(&db, "default");
    let start = Instant::now();
    for batch in keys.chunks(BATCH_SIZE) {
        let txn = cf.begin_write().unwrap();
        let mut t = txn.open_table(TABLE).unwrap();
        for &k in batch {
            t.insert(&k, make_value(k).as_slice()).unwrap();
        }
        drop(t);
        txn.commit().unwrap();
    }
    let manifold_dur = start.elapsed();
    drop(cf);
    drop(db);
    drop(_tmp);

    // RocksDB
    let (_tmp, db) = open_rocksdb(dir);
    let start = Instant::now();
    for batch in keys.chunks(BATCH_SIZE) {
        let txn = db.transaction();
        for &k in batch {
            txn.put(&key_bytes(k), &make_value(k)).unwrap();
        }
        txn.commit().unwrap();
    }
    let rocksdb_dur = start.elapsed();
    drop(db);
    drop(_tmp);

    WorkloadResult {
        scale: n,
        manifold_ops: n as f64 / manifold_dur.as_secs_f64(),
        rocksdb_ops: n as f64 / rocksdb_dur.as_secs_f64(),
    }
}

// ===========================================================================
// Workload 3: Point Read (Uniform)
// ===========================================================================

fn bench_point_read_uniform(dir: &std::path::Path, n: u64) -> WorkloadResult {
    use manifold::TableDefinition;
    const TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("data");

    // --- Populate Manifold ---
    let (_tmp_m, db_m) = open_manifold_cf(dir, true);
    {
        let cf = manifold_cf(&db_m, "default");
        for batch_start in (0..n).step_by(BATCH_SIZE) {
            let txn = cf.begin_write().unwrap();
            let mut t = txn.open_table(TABLE).unwrap();
            for i in batch_start..std::cmp::min(batch_start + BATCH_SIZE as u64, n) {
                t.insert(&i, make_value(i).as_slice()).unwrap();
            }
            drop(t);
            txn.commit().unwrap();
        }
    }

    // --- Populate RocksDB ---
    let (_tmp_r, db_r) = open_rocksdb(dir);
    {
        for batch_start in (0..n).step_by(BATCH_SIZE) {
            let txn = db_r.transaction();
            for i in batch_start..std::cmp::min(batch_start + BATCH_SIZE as u64, n) {
                txn.put(&key_bytes(i), &make_value(i)).unwrap();
            }
            txn.commit().unwrap();
        }
    }

    let read_keys = shuffled_keys(n, RNG_SEED + 1);
    let num_reads = n;

    // Manifold reads
    let cf = manifold_cf(&db_m, "default");
    let start = Instant::now();
    {
        let txn = cf.begin_read().unwrap();
        let t = txn.open_table(TABLE).unwrap();
        for i in 0..num_reads {
            let k = read_keys[i as usize % read_keys.len()];
            let _ = t.get(&k).unwrap();
        }
    }
    let manifold_dur = start.elapsed();

    // RocksDB reads
    let start = Instant::now();
    for i in 0..num_reads {
        let k = read_keys[i as usize % read_keys.len()];
        let _ = db_r.get(&key_bytes(k)).unwrap();
    }
    let rocksdb_dur = start.elapsed();

    WorkloadResult {
        scale: n,
        manifold_ops: num_reads as f64 / manifold_dur.as_secs_f64(),
        rocksdb_ops: num_reads as f64 / rocksdb_dur.as_secs_f64(),
    }
}

// ===========================================================================
// Workload 4: Point Read (Zipfian)
// ===========================================================================

fn bench_point_read_zipfian(dir: &std::path::Path, n: u64) -> WorkloadResult {
    use manifold::TableDefinition;
    const TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("data");

    // Populate both
    let (_tmp_m, db_m) = open_manifold_cf(dir, true);
    {
        let cf = manifold_cf(&db_m, "default");
        for batch_start in (0..n).step_by(BATCH_SIZE) {
            let txn = cf.begin_write().unwrap();
            let mut t = txn.open_table(TABLE).unwrap();
            for i in batch_start..std::cmp::min(batch_start + BATCH_SIZE as u64, n) {
                t.insert(&i, make_value(i).as_slice()).unwrap();
            }
            drop(t);
            txn.commit().unwrap();
        }
    }

    let (_tmp_r, db_r) = open_rocksdb(dir);
    {
        for batch_start in (0..n).step_by(BATCH_SIZE) {
            let txn = db_r.transaction();
            for i in batch_start..std::cmp::min(batch_start + BATCH_SIZE as u64, n) {
                txn.put(&key_bytes(i), &make_value(i)).unwrap();
            }
            txn.commit().unwrap();
        }
    }

    let num_reads = n;

    // Manifold
    let cf = manifold_cf(&db_m, "default");
    let start = Instant::now();
    {
        let txn = cf.begin_read().unwrap();
        let t = txn.open_table(TABLE).unwrap();
        for counter in 0..num_reads {
            let k = zipfian_key(counter, n);
            let _ = t.get(&k).unwrap();
        }
    }
    let manifold_dur = start.elapsed();

    // RocksDB
    let start = Instant::now();
    for counter in 0..num_reads {
        let k = zipfian_key(counter, n);
        let _ = db_r.get(&key_bytes(k)).unwrap();
    }
    let rocksdb_dur = start.elapsed();

    WorkloadResult {
        scale: n,
        manifold_ops: num_reads as f64 / manifold_dur.as_secs_f64(),
        rocksdb_ops: num_reads as f64 / rocksdb_dur.as_secs_f64(),
    }
}

// ===========================================================================
// Workload 5: Range Scan
// ===========================================================================

fn bench_range_scan(dir: &std::path::Path, n: u64) -> WorkloadResult {
    use manifold::TableDefinition;
    const TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("data");

    // Populate both
    let (_tmp_m, db_m) = open_manifold_cf(dir, true);
    {
        let cf = manifold_cf(&db_m, "default");
        for batch_start in (0..n).step_by(BATCH_SIZE) {
            let txn = cf.begin_write().unwrap();
            let mut t = txn.open_table(TABLE).unwrap();
            for i in batch_start..std::cmp::min(batch_start + BATCH_SIZE as u64, n) {
                t.insert(&i, make_value(i).as_slice()).unwrap();
            }
            drop(t);
            txn.commit().unwrap();
        }
    }

    let (_tmp_r, db_r) = open_rocksdb(dir);
    {
        for batch_start in (0..n).step_by(BATCH_SIZE) {
            let txn = db_r.transaction();
            for i in batch_start..std::cmp::min(batch_start + BATCH_SIZE as u64, n) {
                txn.put(&key_bytes(i), &make_value(i)).unwrap();
            }
            txn.commit().unwrap();
        }
    }

    let scan_starts = shuffled_keys(
        std::cmp::min(RANGE_SCAN_COUNT, n.saturating_sub(RANGE_SCAN_LENGTH)),
        RNG_SEED + 2,
    );
    let num_scans = scan_starts.len() as u64;
    let total_entries = num_scans * RANGE_SCAN_LENGTH;

    // Manifold range scans
    let cf = manifold_cf(&db_m, "default");
    let start = Instant::now();
    {
        let txn = cf.begin_read().unwrap();
        let t = txn.open_table(TABLE).unwrap();
        for &scan_start in &scan_starts {
            let end = scan_start + RANGE_SCAN_LENGTH;
            let mut iter = t.range(scan_start..end).unwrap();
            while let Some(Ok(_)) = iter.next() {}
        }
    }
    let manifold_dur = start.elapsed();

    // RocksDB range scans
    let start = Instant::now();
    {
        for &scan_start in &scan_starts {
            let end = scan_start + RANGE_SCAN_LENGTH;
            let mut iter = db_r.iterator(rocksdb::IteratorMode::From(
                &key_bytes(scan_start),
                rocksdb::Direction::Forward,
            ));
            let end_bytes = key_bytes(end);
            while let Some(Ok((k, _))) = iter.next() {
                if k.as_ref() >= end_bytes.as_slice() {
                    break;
                }
            }
        }
    }
    let rocksdb_dur = start.elapsed();

    WorkloadResult {
        scale: n,
        manifold_ops: total_entries as f64 / manifold_dur.as_secs_f64(),
        rocksdb_ops: total_entries as f64 / rocksdb_dur.as_secs_f64(),
    }
}

// ===========================================================================
// Workload 6: Mixed 50/50 Read/Write
// ===========================================================================

fn bench_mixed_50_50(dir: &std::path::Path, n: u64) -> WorkloadResult {
    use manifold::TableDefinition;
    const TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("data");

    let prepopulate = n / 2;

    // Populate Manifold
    let (_tmp_m, db_m) = open_manifold_cf(dir, true);
    {
        let cf = manifold_cf(&db_m, "default");
        for batch_start in (0..prepopulate).step_by(BATCH_SIZE) {
            let txn = cf.begin_write().unwrap();
            let mut t = txn.open_table(TABLE).unwrap();
            for i in batch_start..std::cmp::min(batch_start + BATCH_SIZE as u64, prepopulate) {
                t.insert(&i, make_value(i).as_slice()).unwrap();
            }
            drop(t);
            txn.commit().unwrap();
        }
    }

    // Populate RocksDB
    let (_tmp_r, db_r) = open_rocksdb(dir);
    {
        for batch_start in (0..prepopulate).step_by(BATCH_SIZE) {
            let txn = db_r.transaction();
            for i in batch_start..std::cmp::min(batch_start + BATCH_SIZE as u64, prepopulate) {
                txn.put(&key_bytes(i), &make_value(i)).unwrap();
            }
            txn.commit().unwrap();
        }
    }

    // Manifold mixed workload
    let manifold_ops = {
        let stop = Arc::new(AtomicBool::new(false));
        let total = Arc::new(AtomicU64::new(0));

        let handles: Vec<_> = (0..MIXED_THREADS)
            .map(|tid| {
                let db = db_m.clone();
                let stop = stop.clone();
                let total = total.clone();
                thread::spawn(move || {
                    let cf = db.column_family("default").unwrap();
                    let mut counter = tid as u64 * 1_000_000;
                    let mut ops = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        counter += 1;
                        if counter % 2 == 0 {
                            // Read
                            let k = zipfian_key(counter, prepopulate);
                            let txn = cf.begin_read().unwrap();
                            let t = txn.open_table(TABLE).unwrap();
                            let _ = t.get(&k);
                        } else {
                            // Write
                            let k = prepopulate + counter;
                            let txn = cf.begin_write().unwrap();
                            let mut t = txn.open_table(TABLE).unwrap();
                            let _ = t.insert(&k, make_value(k).as_slice());
                            drop(t);
                            let _ = txn.commit();
                        }
                        ops += 1;
                    }
                    total.fetch_add(ops, Ordering::Relaxed);
                })
            })
            .collect();

        thread::sleep(Duration::from_secs(MIXED_DURATION_SECS));
        stop.store(true, Ordering::Relaxed);
        for h in handles {
            h.join().unwrap();
        }
        total.load(Ordering::Relaxed) as f64 / MIXED_DURATION_SECS as f64
    };

    // RocksDB mixed workload
    let rocksdb_ops = {
        let stop = Arc::new(AtomicBool::new(false));
        let total = Arc::new(AtomicU64::new(0));

        let handles: Vec<_> = (0..MIXED_THREADS)
            .map(|tid| {
                let db = db_r.clone();
                let stop = stop.clone();
                let total = total.clone();
                thread::spawn(move || {
                    let mut counter = tid as u64 * 1_000_000;
                    let mut ops = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        counter += 1;
                        if counter % 2 == 0 {
                            // Read
                            let k = zipfian_key(counter, prepopulate);
                            let _ = db.get(&key_bytes(k));
                        } else {
                            // Write
                            let k = prepopulate + counter;
                            let txn = db.transaction();
                            txn.put(&key_bytes(k), &make_value(k)).unwrap();
                            let _ = txn.commit();
                        }
                        ops += 1;
                    }
                    total.fetch_add(ops, Ordering::Relaxed);
                })
            })
            .collect();

        thread::sleep(Duration::from_secs(MIXED_DURATION_SECS));
        stop.store(true, Ordering::Relaxed);
        for h in handles {
            h.join().unwrap();
        }
        total.load(Ordering::Relaxed) as f64 / MIXED_DURATION_SECS as f64
    };

    WorkloadResult {
        scale: n,
        manifold_ops,
        rocksdb_ops,
    }
}

// ===========================================================================
// Workload 7: Durable Write (fsync)
// ===========================================================================

fn bench_durable_write(dir: &std::path::Path, n: u64) -> WorkloadResult {
    use manifold::{Durability, TableDefinition};
    const TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("data");

    // Cap durable writes to avoid excessive runtime at large scales
    let n = std::cmp::min(n, 500_000);
    let durable_batch = 100usize;

    // Manifold with WAL (durable)
    let (_tmp, db) = open_manifold_cf(dir, true);
    let cf = manifold_cf(&db, "default");
    let start = Instant::now();
    for batch_start in (0..n).step_by(durable_batch) {
        let mut txn = cf.begin_write().unwrap();
        txn.set_durability(Durability::Immediate).unwrap();
        let mut t = txn.open_table(TABLE).unwrap();
        for i in batch_start..std::cmp::min(batch_start + durable_batch as u64, n) {
            t.insert(&i, make_value(i).as_slice()).unwrap();
        }
        drop(t);
        txn.commit().unwrap();
    }
    let manifold_dur = start.elapsed();
    drop(cf);
    drop(db);
    drop(_tmp);

    // RocksDB with sync_on_write
    let (_tmp, db) = open_rocksdb(dir);
    let start = Instant::now();
    {
        let mut write_opts = rocksdb::WriteOptions::default();
        write_opts.set_sync(true);
        for batch_start in (0..n).step_by(durable_batch) {
            let txn = db.transaction();
            for i in batch_start..std::cmp::min(batch_start + durable_batch as u64, n) {
                txn.put(&key_bytes(i), &make_value(i)).unwrap();
            }
            txn.commit().unwrap();
            // RocksDB: sync after commit via a separate flush
            db.flush().unwrap();
        }
    }
    let rocksdb_dur = start.elapsed();
    drop(db);
    drop(_tmp);

    WorkloadResult {
        scale: n,
        manifold_ops: n as f64 / manifold_dur.as_secs_f64(),
        rocksdb_ops: n as f64 / rocksdb_dur.as_secs_f64(),
    }
}

// ===========================================================================
// Workload 8: Concurrent CF Writers
// ===========================================================================

fn bench_concurrent_cf(dir: &std::path::Path, n: u64) -> WorkloadResult {
    use manifold::TableDefinition;
    const TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("data");

    let per_cf = n / NUM_CFS as u64;
    let total_ops = per_cf * NUM_CFS as u64;

    // Manifold with independent CFs
    let (_tmp, db) = open_manifold_cf(dir, true);
    for i in 1..NUM_CFS {
        db.create_column_family(&format!("cf_{}", i), None).unwrap();
    }
    // rename "default" usage to cf_0 pattern
    let cf_names_m: Vec<String> = (0..NUM_CFS)
        .map(|i| if i == 0 { "default".to_string() } else { format!("cf_{}", i) })
        .collect();

    let start = Instant::now();
    thread::scope(|s| {
        for cf_id in 0..NUM_CFS {
            let db = &db;
            let cf_name = &cf_names_m[cf_id];
            s.spawn(move || {
                let cf = db.column_family(cf_name).unwrap();
                for batch_start in (0..per_cf).step_by(BATCH_SIZE) {
                    let txn = cf.begin_write().unwrap();
                    let mut t = txn.open_table(TABLE).unwrap();
                    for i in batch_start..std::cmp::min(batch_start + BATCH_SIZE as u64, per_cf) {
                        let key = cf_id as u64 * per_cf + i;
                        t.insert(&key, make_value(key).as_slice()).unwrap();
                    }
                    drop(t);
                    txn.commit().unwrap();
                }
            });
        }
    });
    let manifold_dur = start.elapsed();
    drop(db);
    drop(_tmp);

    // RocksDB with native CFs
    let cf_names_r: Vec<String> = (0..NUM_CFS).map(|i| format!("cf_{}", i)).collect();
    let (_tmp, db) = open_rocksdb_cf(dir, &cf_names_r);

    let start = Instant::now();
    thread::scope(|s| {
        for cf_id in 0..NUM_CFS {
            let db = &db;
            let cf_name = &cf_names_r[cf_id];
            s.spawn(move || {
                let cf = db.cf_handle(cf_name).unwrap();
                for batch_start in (0..per_cf).step_by(BATCH_SIZE) {
                    let mut batch = rocksdb::WriteBatch::default();
                    for i in batch_start..std::cmp::min(batch_start + BATCH_SIZE as u64, per_cf) {
                        let key = cf_id as u64 * per_cf + i;
                        batch.put_cf(&cf, key_bytes(key), make_value(key));
                    }
                    db.write(batch).unwrap();
                }
            });
        }
    });
    let rocksdb_dur = start.elapsed();
    drop(db);
    drop(_tmp);

    WorkloadResult {
        scale: total_ops,
        manifold_ops: total_ops as f64 / manifold_dur.as_secs_f64(),
        rocksdb_ops: total_ops as f64 / rocksdb_dur.as_secs_f64(),
    }
}

// ===========================================================================
// Main
// ===========================================================================

fn main() {
    let _ = env_logger::try_init();
    let bench_dir = current_dir().unwrap().join(".benchmark_rocksdb_comparison");
    let _ = fs::remove_dir_all(&bench_dir);
    fs::create_dir_all(&bench_dir).unwrap();

    let bench_dir2 = bench_dir.clone();
    ctrlc::set_handler(move || {
        let _ = fs::remove_dir_all(&bench_dir2);
        process::exit(1);
    })
    .unwrap();

    println!("\n{}", "=".repeat(90));
    println!("  Manifold vs RocksDB Comparative Benchmark");
    println!("{}", "=".repeat(90));
    println!("\nConfiguration:");
    println!("  Key size:     {} bytes (u64 BE)", KEY_SIZE);
    println!("  Value size:   {} bytes", VALUE_SIZE);
    println!("  Batch size:   {}", BATCH_SIZE);
    println!("  Scales:       {:?}", SCALES);
    println!("  RocksDB cache: {}MB", CACHE_SIZE / 1024 / 1024);
    println!("  Mixed threads: {}", MIXED_THREADS);
    println!("  Mixed duration: {}s per scale", MIXED_DURATION_SECS);
    println!("  CF writers:   {}", NUM_CFS);

    // --- Workload 1: Sequential Write ---
    print!("\n[1/8] Sequential Write...");
    std::io::stdout().flush().unwrap();
    let mut results_seq = vec![];
    for &scale in SCALES {
        print!(" {}K", scale / 1000);
        std::io::stdout().flush().unwrap();
        results_seq.push(bench_sequential_write(&bench_dir, scale));
    }
    println!(" done");
    print_workload_table("Sequential Write", &results_seq);

    // --- Workload 2: Random Write ---
    print!("\n[2/8] Random Write...");
    std::io::stdout().flush().unwrap();
    let mut results_rnd = vec![];
    for &scale in SCALES {
        print!(" {}K", scale / 1000);
        std::io::stdout().flush().unwrap();
        results_rnd.push(bench_random_write(&bench_dir, scale));
    }
    println!(" done");
    print_workload_table("Random Write", &results_rnd);

    // --- Workload 3: Point Read (Uniform) ---
    print!("\n[3/8] Point Read (Uniform)...");
    std::io::stdout().flush().unwrap();
    let mut results_rd = vec![];
    for &scale in SCALES {
        print!(" {}K", scale / 1000);
        std::io::stdout().flush().unwrap();
        results_rd.push(bench_point_read_uniform(&bench_dir, scale));
    }
    println!(" done");
    print_workload_table("Point Read (Uniform)", &results_rd);

    // --- Workload 4: Point Read (Zipfian) ---
    print!("\n[4/8] Point Read (Zipfian)...");
    std::io::stdout().flush().unwrap();
    let mut results_zip = vec![];
    for &scale in SCALES {
        print!(" {}K", scale / 1000);
        std::io::stdout().flush().unwrap();
        results_zip.push(bench_point_read_zipfian(&bench_dir, scale));
    }
    println!(" done");
    print_workload_table("Point Read (Zipfian)", &results_zip);

    // --- Workload 5: Range Scan ---
    print!("\n[5/8] Range Scan...");
    std::io::stdout().flush().unwrap();
    let mut results_scan = vec![];
    for &scale in SCALES {
        print!(" {}K", scale / 1000);
        std::io::stdout().flush().unwrap();
        results_scan.push(bench_range_scan(&bench_dir, scale));
    }
    println!(" done");
    print_workload_table("Range Scan", &results_scan);

    // --- Workload 6: Mixed 50/50 ---
    print!("\n[6/8] Mixed 50/50...");
    std::io::stdout().flush().unwrap();
    let mut results_mixed = vec![];
    for &scale in SCALES {
        print!(" {}K", scale / 1000);
        std::io::stdout().flush().unwrap();
        results_mixed.push(bench_mixed_50_50(&bench_dir, scale));
    }
    println!(" done");
    print_workload_table("Mixed 50/50 Read/Write", &results_mixed);

    // --- Workload 7: Durable Write ---
    print!("\n[7/8] Durable Write...");
    std::io::stdout().flush().unwrap();
    let mut results_durable = vec![];
    for &scale in SCALES {
        print!(" {}K", scale / 1000);
        std::io::stdout().flush().unwrap();
        results_durable.push(bench_durable_write(&bench_dir, scale));
    }
    println!(" done");
    print_workload_table("Durable Write (fsync)", &results_durable);

    // --- Workload 8: Concurrent CF Writers ---
    print!("\n[8/8] Concurrent CF Writers...");
    std::io::stdout().flush().unwrap();
    let mut results_cf = vec![];
    for &scale in SCALES {
        print!(" {}K", scale / 1000);
        std::io::stdout().flush().unwrap();
        results_cf.push(bench_concurrent_cf(&bench_dir, scale));
    }
    println!(" done");
    print_workload_table("Concurrent CF Writers (8 CFs)", &results_cf);

    // --- Summary ---
    println!("\n{}", "=".repeat(90));
    println!("  Summary: Where Each Engine Wins");
    println!("{}", "=".repeat(90));
    println!(
        "  {:<30} | {:>12} | {:>12} | {:>10}",
        "Workload", "Avg Ratio", "Trend", "Winner"
    );
    println!("  {}", "-".repeat(72));

    let all_workloads: Vec<(&str, &[WorkloadResult])> = vec![
        ("Sequential Write", &results_seq),
        ("Random Write", &results_rnd),
        ("Point Read (Uniform)", &results_rd),
        ("Point Read (Zipfian)", &results_zip),
        ("Range Scan", &results_scan),
        ("Mixed 50/50", &results_mixed),
        ("Durable Write", &results_durable),
        ("Concurrent CF Writers", &results_cf),
    ];

    for (name, results) in &all_workloads {
        let avg_ratio: f64 = results.iter().map(|r| r.ratio()).sum::<f64>() / results.len() as f64;
        let first_ratio = results.first().map(|r| r.ratio()).unwrap_or(1.0);
        let last_ratio = results.last().map(|r| r.ratio()).unwrap_or(1.0);
        let trend = if (last_ratio - first_ratio).abs() < 0.05 {
            "stable"
        } else if last_ratio > first_ratio {
            "M improves"
        } else {
            "R improves"
        };
        let winner = if avg_ratio >= 1.0 {
            format!("Manifold ({:.1}x)", avg_ratio)
        } else {
            format!("RocksDB ({:.1}x)", 1.0 / avg_ratio)
        };
        println!(
            "  {:.<30} | {:>11.2}x | {:>12} | {:>10}",
            name, avg_ratio, trend, winner
        );
    }
    println!("  {}", "-".repeat(72));
    println!();

    let _ = fs::remove_dir_all(&bench_dir);
}
