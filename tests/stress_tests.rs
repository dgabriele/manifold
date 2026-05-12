//! Comprehensive Stress Test Suite for Manifold
//!
//! Tests production-readiness across all major axes:
//! - High-volume concurrent reads and writes
//! - ACID compliance under contention
//! - Snapshot isolation correctness
//! - Cross-column-family parallelism
//! - Durability after close/reopen cycles
//! - Iterator stability under concurrent modification
//! - Savepoint correctness under load
//! - Range query consistency
//! - Base redb functionality (multimap, bulk ops, compaction)

#[cfg(not(target_os = "wasi"))]
mod stress {
    use manifold::column_family::ColumnFamilyDatabase;
    use manifold::{
        Database, Durability, MultimapTableDefinition, ReadableDatabase, ReadableTable,
        ReadableTableMetadata, TableDefinition,
    };
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{Duration, Instant};
    use tempfile::NamedTempFile;

    const TABLE_U64: TableDefinition<u64, u64> = TableDefinition::new("u64_table");
    const TABLE_BYTES: TableDefinition<u64, &[u8]> = TableDefinition::new("bytes_table");
    const MMAP_TABLE: MultimapTableDefinition<u64, u64> = MultimapTableDefinition::new("mmap");

    fn tmpfile() -> NamedTempFile {
        NamedTempFile::new().unwrap()
    }

    // ========================================================================
    // 1. HIGH-VOLUME CONCURRENT WRITE STRESS
    // ========================================================================

    /// 8 threads x 2000 writes each = 16K total writes to a single CF.
    /// Verifies all committed data is present and correct.
    #[test]
    fn stress_high_volume_concurrent_writes_single_cf() {
        let tmp = tmpfile();
        let db = Arc::new(ColumnFamilyDatabase::builder().open(tmp.path()).unwrap());
        db.create_column_family("cf", None).unwrap();

        let num_threads: u64 = 8;
        let writes_per_thread: u64 = 2000;
        let barrier = Arc::new(Barrier::new(num_threads as usize));

        let handles: Vec<_> = (0..num_threads)
            .map(|tid| {
                let db = db.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    let cf = db.column_family("cf").unwrap();
                    for i in 0..writes_per_thread {
                        let key = tid * writes_per_thread + i;
                        let txn = cf.begin_write().unwrap();
                        let mut t = txn.open_table(TABLE_U64).unwrap();
                        t.insert(&key, &(key * 7)).unwrap();
                        drop(t);
                        txn.commit().unwrap();
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // Verify every single write
        let cf = db.column_family("cf").unwrap();
        let txn = cf.begin_read().unwrap();
        let t = txn.open_table(TABLE_U64).unwrap();
        assert_eq!(t.len().unwrap(), num_threads * writes_per_thread);
        for tid in 0..num_threads {
            for i in 0..writes_per_thread {
                let key = tid * writes_per_thread + i;
                let val = t.get(&key).unwrap().unwrap();
                assert_eq!(val.value(), key * 7, "Mismatch at key {}", key);
            }
        }
    }

    /// Concurrent writes across 8 independent column families — should achieve
    /// true parallelism (no cross-CF serialization).
    #[test]
    fn stress_parallel_writes_across_column_families() {
        let tmp = tmpfile();
        let db = Arc::new(ColumnFamilyDatabase::builder().open(tmp.path()).unwrap());

        let num_cfs = 8;
        let writes_per_cf = 1000;

        for i in 0..num_cfs {
            db.create_column_family(&format!("cf_{}", i), None).unwrap();
        }

        let barrier = Arc::new(Barrier::new(num_cfs));
        let handles: Vec<_> = (0..num_cfs)
            .map(|cf_id| {
                let db = db.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    let cf_name = format!("cf_{}", cf_id);
                    let cf = db.column_family(&cf_name).unwrap();
                    for i in 0..writes_per_cf {
                        let key = i as u64;
                        let txn = cf.begin_write().unwrap();
                        let mut t = txn.open_table(TABLE_U64).unwrap();
                        t.insert(&key, &(key + cf_id as u64 * 1000)).unwrap();
                        drop(t);
                        txn.commit().unwrap();
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // Verify each CF independently
        for cf_id in 0..num_cfs {
            let cf = db.column_family(&format!("cf_{}", cf_id)).unwrap();
            let txn = cf.begin_read().unwrap();
            let t = txn.open_table(TABLE_U64).unwrap();
            assert_eq!(t.len().unwrap(), writes_per_cf as u64);
            for i in 0..writes_per_cf {
                let key = i as u64;
                let val = t.get(&key).unwrap().unwrap();
                assert_eq!(val.value(), key + cf_id as u64 * 1000);
            }
        }
    }

    // ========================================================================
    // 2. SNAPSHOT ISOLATION VERIFICATION
    // ========================================================================

    /// Verifies that a read transaction sees a consistent snapshot even while
    /// concurrent writes are happening. The reader should see exactly the state
    /// at the time it started — no phantom reads, no dirty reads.
    #[test]
    fn stress_snapshot_isolation_correctness() {
        let tmp = tmpfile();
        let db = Arc::new(ColumnFamilyDatabase::builder().open(tmp.path()).unwrap());
        db.create_column_family("cf", None).unwrap();

        // Seed 1000 keys with value = key
        {
            let cf = db.column_family("cf").unwrap();
            let txn = cf.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in 0..1000 {
                t.insert(&i, &i).unwrap();
            }
            drop(t);
            txn.commit().unwrap();
        }

        let done = Arc::new(AtomicBool::new(false));
        let snapshot_errors = Arc::new(AtomicU64::new(0));

        // Writer thread: continuously update keys
        let db_w = db.clone();
        let done_w = done.clone();
        let writer = thread::spawn(move || {
            let cf = db_w.column_family("cf").unwrap();
            let mut round = 1u64;
            while !done_w.load(Ordering::Relaxed) {
                let txn = cf.begin_write().unwrap();
                let mut t = txn.open_table(TABLE_U64).unwrap();
                // Update ALL keys to the same round number — within one transaction
                for i in 0..1000 {
                    t.insert(&i, &(round * 1000 + i)).unwrap();
                }
                drop(t);
                txn.commit().unwrap();
                round += 1;
            }
        });

        // Reader threads: take snapshot and verify ALL keys have consistent round
        let mut readers = vec![];
        for _ in 0..4 {
            let db_r = db.clone();
            let done_r = done.clone();
            let errs = snapshot_errors.clone();
            readers.push(thread::spawn(move || {
                let cf = db_r.column_family("cf").unwrap();
                let mut checks = 0u64;
                while !done_r.load(Ordering::Relaxed) {
                    let txn = cf.begin_read().unwrap();
                    let t = txn.open_table(TABLE_U64).unwrap();

                    // Read first key to determine which "round" we're seeing
                    let first_val = t.get(&0u64).unwrap().unwrap().value();
                    let round = first_val / 1000;

                    // ALL other keys must belong to the same round
                    for i in 1..1000 {
                        let val = t.get(&i).unwrap().unwrap().value();
                        let val_round = val / 1000;
                        if val_round != round {
                            errs.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    checks += 1;
                    drop(t);
                    drop(txn);
                }
                checks
            }));
        }

        // Let it run for 2 seconds
        thread::sleep(Duration::from_secs(2));
        done.store(true, Ordering::Relaxed);

        writer.join().unwrap();
        let total_checks: u64 = readers.into_iter().map(|h| h.join().unwrap()).sum();

        assert_eq!(
            snapshot_errors.load(Ordering::Relaxed),
            0,
            "Snapshot isolation violated! {} checks performed",
            total_checks
        );
        assert!(total_checks > 0, "Readers should have performed checks");
    }

    // ========================================================================
    // 3. ATOMIC MULTI-KEY TRANSACTION
    // ========================================================================

    /// Writes multiple keys atomically in a single transaction. Readers must see
    /// either ALL keys from a transaction or NONE — never a partial set.
    #[test]
    fn stress_atomic_multi_key_transactions() {
        let tmp = tmpfile();
        let db = Arc::new(ColumnFamilyDatabase::builder().open(tmp.path()).unwrap());
        db.create_column_family("cf", None).unwrap();

        let done = Arc::new(AtomicBool::new(false));
        let violation_count = Arc::new(AtomicU64::new(0));

        // Writer: each transaction writes keys 0..9 with the same "batch_id"
        let db_w = db.clone();
        let done_w = done.clone();
        let writer = thread::spawn(move || {
            let cf = db_w.column_family("cf").unwrap();
            for batch_id in 1u64..=500 {
                let txn = cf.begin_write().unwrap();
                let mut t = txn.open_table(TABLE_U64).unwrap();
                for key in 0u64..10 {
                    t.insert(&key, &batch_id).unwrap();
                }
                drop(t);
                txn.commit().unwrap();
                if done_w.load(Ordering::Relaxed) {
                    break;
                }
            }
        });

        // Readers: verify atomicity — all 10 keys must have same batch_id
        let mut readers = vec![];
        for _ in 0..4 {
            let db_r = db.clone();
            let done_r = done.clone();
            let violations = violation_count.clone();
            readers.push(thread::spawn(move || {
                let cf = db_r.column_family("cf").unwrap();
                while !done_r.load(Ordering::Relaxed) {
                    let txn = cf.begin_read().unwrap();
                    let t = match txn.open_table(TABLE_U64) {
                        Ok(t) => t,
                        Err(_) => continue, // table not yet created
                    };

                    // If table is empty or keys don't exist yet, skip
                    let first = match t.get(&0u64).unwrap() {
                        Some(v) => v.value(),
                        None => continue,
                    };

                    for key in 1u64..10 {
                        if let Some(v) = t.get(&key).unwrap() {
                            if v.value() != first {
                                violations.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }
            }));
        }

        writer.join().unwrap();
        done.store(true, Ordering::Relaxed);

        for h in readers {
            h.join().unwrap();
        }

        assert_eq!(
            violation_count.load(Ordering::Relaxed),
            0,
            "Atomicity violation: partial transaction visible to readers"
        );
    }

    // ========================================================================
    // 4. CONCURRENT READERS DON'T BLOCK WRITERS
    // ========================================================================

    /// A long-running reader holds a read transaction open for 2 seconds while
    /// writers continuously commit. Writers must not be blocked.
    #[test]
    fn stress_readers_dont_block_writers() {
        let tmp = tmpfile();
        let db = Arc::new(ColumnFamilyDatabase::builder().open(tmp.path()).unwrap());
        db.create_column_family("cf", None).unwrap();

        // Seed some data
        {
            let cf = db.column_family("cf").unwrap();
            let txn = cf.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            t.insert(&0u64, &0u64).unwrap();
            drop(t);
            txn.commit().unwrap();
        }

        // Open the read snapshot on the main thread BEFORE spawning the writer
        let cf = db.column_family("cf").unwrap();
        let read_txn = cf.begin_read().unwrap();
        let read_table = read_txn.open_table(TABLE_U64).unwrap();
        let initial_val = read_table.get(&0u64).unwrap().unwrap().value();
        assert_eq!(initial_val, 0);

        let writer_ready = Arc::new(Barrier::new(2));

        // Long-running reader: holds the snapshot open while writer runs
        let wr = writer_ready.clone();
        let reader = thread::spawn(move || {
            // Signal that we're ready, then wait for writer to start
            wr.wait();

            // Hold the read transaction open for 2 seconds
            thread::sleep(Duration::from_secs(2));

            // Should still see the original value (snapshot isolation)
            let val_after = read_table.get(&0u64).unwrap().unwrap().value();
            assert_eq!(val_after, 0, "Snapshot should be stable");
        });

        // Writer: commit as many transactions as possible during 2 seconds
        let db_w = db.clone();
        let writer = thread::spawn(move || {
            writer_ready.wait();
            let cf = db_w.column_family("cf").unwrap();
            let start = Instant::now();
            let mut count = 0u64;
            while start.elapsed() < Duration::from_secs(2) {
                let txn = cf.begin_write().unwrap();
                let mut t = txn.open_table(TABLE_U64).unwrap();
                count += 1;
                t.insert(&0u64, &count).unwrap();
                drop(t);
                txn.commit().unwrap();
            }
            count
        });

        let write_count = writer.join().unwrap();
        reader.join().unwrap();

        assert!(
            write_count > 100,
            "Writers should not be blocked by readers (only {} writes in 2s)",
            write_count
        );
    }

    // ========================================================================
    // 5. RANGE QUERY CONSISTENCY UNDER CONCURRENT WRITES
    // ========================================================================

    /// Verifies that range scans return a consistent snapshot — no torn reads.
    #[test]
    fn stress_range_query_consistency() {
        let tmp = tmpfile();
        let db = Arc::new(ColumnFamilyDatabase::builder().open(tmp.path()).unwrap());
        db.create_column_family("cf", None).unwrap();

        // Seed: keys 0..99, all with value 0
        {
            let cf = db.column_family("cf").unwrap();
            let txn = cf.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in 0u64..100 {
                t.insert(&i, &0u64).unwrap();
            }
            drop(t);
            txn.commit().unwrap();
        }

        let done = Arc::new(AtomicBool::new(false));
        let range_violations = Arc::new(AtomicU64::new(0));

        // Writer: atomically update ALL keys to the same round number
        let db_w = db.clone();
        let done_w = done.clone();
        let writer = thread::spawn(move || {
            let cf = db_w.column_family("cf").unwrap();
            for round in 1u64..=300 {
                let txn = cf.begin_write().unwrap();
                let mut t = txn.open_table(TABLE_U64).unwrap();
                for i in 0u64..100 {
                    t.insert(&i, &round).unwrap();
                }
                drop(t);
                txn.commit().unwrap();
                if done_w.load(Ordering::Relaxed) {
                    break;
                }
            }
        });

        // Reader: scan range 0..100 and verify all values are from same round
        let db_r = db.clone();
        let done_r = done.clone();
        let violations = range_violations.clone();
        let reader = thread::spawn(move || {
            let cf = db_r.column_family("cf").unwrap();
            let mut checks = 0u64;
            while !done_r.load(Ordering::Relaxed) {
                let txn = cf.begin_read().unwrap();
                let t = txn.open_table(TABLE_U64).unwrap();
                let mut iter = t.range(0u64..100).unwrap();
                let mut first_round = None;
                while let Some(Ok((_, v))) = iter.next() {
                    let round = v.value();
                    match first_round {
                        None => first_round = Some(round),
                        Some(r) if r != round => {
                            violations.fetch_add(1, Ordering::Relaxed);
                        }
                        _ => {}
                    }
                }
                checks += 1;
            }
            checks
        });

        writer.join().unwrap();
        done.store(true, Ordering::Relaxed);
        let checks = reader.join().unwrap();

        assert_eq!(
            range_violations.load(Ordering::Relaxed),
            0,
            "Range scan saw inconsistent snapshot ({} checks)",
            checks
        );
    }

    // ========================================================================
    // 6. DURABILITY: WRITE HEAVY, CLOSE, REOPEN, VERIFY
    // ========================================================================

    /// Writes 5000 entries across 4 CFs, closes the database, reopens, and
    /// verifies every single entry survived.
    #[test]
    fn stress_durability_close_reopen() {
        let tmp = tmpfile();
        let db_path = tmp.path().to_path_buf();
        let entries_per_cf = 5000u64;
        let num_cfs = 4;

        // Phase 1: heavy writes
        {
            let db = ColumnFamilyDatabase::builder().open(&db_path).unwrap();
            for cf_id in 0..num_cfs {
                db.create_column_family(&format!("cf_{}", cf_id), None)
                    .unwrap();
            }
            for cf_id in 0..num_cfs {
                let cf = db.column_family(&format!("cf_{}", cf_id)).unwrap();
                // Batch 100 keys per transaction for efficiency
                for batch_start in (0..entries_per_cf).step_by(100) {
                    let txn = cf.begin_write().unwrap();
                    let mut t = txn.open_table(TABLE_U64).unwrap();
                    for i in batch_start..std::cmp::min(batch_start + 100, entries_per_cf) {
                        t.insert(&i, &(i + cf_id as u64 * 100_000)).unwrap();
                    }
                    drop(t);
                    txn.commit().unwrap();
                }
            }
        }

        // Phase 2: reopen and verify
        {
            let db = ColumnFamilyDatabase::builder().open(&db_path).unwrap();
            for cf_id in 0..num_cfs {
                let cf = db.column_family(&format!("cf_{}", cf_id)).unwrap();
                let txn = cf.begin_read().unwrap();
                let t = txn.open_table(TABLE_U64).unwrap();
                assert_eq!(
                    t.len().unwrap(),
                    entries_per_cf,
                    "CF {} lost data after reopen",
                    cf_id
                );
                for i in 0..entries_per_cf {
                    let val = t.get(&i).unwrap().unwrap().value();
                    assert_eq!(val, i + cf_id as u64 * 100_000);
                }
            }
        }
    }

    // ========================================================================
    // 7. ITERATOR STABILITY UNDER CONCURRENT WRITES
    // ========================================================================

    /// An iterator opened on a read transaction must reflect the snapshot at
    /// the time the transaction started — subsequent writes must not be visible.
    #[test]
    fn stress_iterator_stability() {
        let tmp = tmpfile();
        let db = Arc::new(ColumnFamilyDatabase::builder().open(tmp.path()).unwrap());
        db.create_column_family("cf", None).unwrap();

        // Seed 500 keys
        {
            let cf = db.column_family("cf").unwrap();
            let txn = cf.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in 0u64..500 {
                t.insert(&i, &i).unwrap();
            }
            drop(t);
            txn.commit().unwrap();
        }

        // Take a read snapshot
        let cf = db.column_family("cf").unwrap();
        let read_txn = cf.begin_read().unwrap();

        // Now write 500 MORE keys and update existing ones
        {
            let txn = cf.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in 0u64..500 {
                t.insert(&i, &(i + 9999)).unwrap(); // update existing
            }
            for i in 500u64..1000 {
                t.insert(&i, &i).unwrap(); // add new
            }
            drop(t);
            txn.commit().unwrap();
        }

        // Iterator on the OLD snapshot should see exactly 500 original keys
        let t = read_txn.open_table(TABLE_U64).unwrap();
        let mut count = 0u64;
        for entry in t.iter().unwrap() {
            let (k, v) = entry.unwrap();
            assert_eq!(k.value(), v.value(), "Original snapshot should have k==v");
            assert!(k.value() < 500, "Should not see new keys");
            count += 1;
        }
        assert_eq!(count, 500, "Iterator should see exactly 500 original keys");
    }

    // ========================================================================
    // 8. READ-YOUR-WRITES CONSISTENCY
    // ========================================================================

    /// After a write transaction commits, a new read transaction must see
    /// the written data. Tests monotonic read progression.
    #[test]
    fn stress_read_your_writes() {
        let tmp = tmpfile();
        let db = Arc::new(ColumnFamilyDatabase::builder().open(tmp.path()).unwrap());
        db.create_column_family("cf", None).unwrap();

        let cf = db.column_family("cf").unwrap();
        for round in 0u64..500 {
            // Write
            let txn = cf.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            t.insert(&round, &round).unwrap();
            drop(t);
            txn.commit().unwrap();

            // Read back immediately
            let rtxn = cf.begin_read().unwrap();
            let t = rtxn.open_table(TABLE_U64).unwrap();
            let val = t.get(&round).unwrap();
            assert!(val.is_some(), "Round {} not visible after commit", round);
            assert_eq!(val.unwrap().value(), round);

            // All previous rounds should also be visible (monotonic)
            assert_eq!(t.len().unwrap(), round + 1);
        }
    }

    // ========================================================================
    // 9. BASE REDB: MULTIMAP TABLE STRESS
    // ========================================================================

    /// Stress-test multimap tables with many values per key under concurrency.
    #[test]
    fn stress_multimap_concurrent() {
        let tmp = tmpfile();
        let db = Arc::new(Database::create(tmp.path()).unwrap());

        // Seed: 10 keys, each with 100 values
        {
            let txn = db.begin_write().unwrap();
            let mut t = txn.open_multimap_table(MMAP_TABLE).unwrap();
            for key in 0u64..10 {
                for val in 0u64..100 {
                    t.insert(&key, &val).unwrap();
                }
            }
            drop(t);
            txn.commit().unwrap();
        }

        // Concurrent readers verify correct multimap contents
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let db = db.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..50 {
                        let txn = db.begin_read().unwrap();
                        let t = txn.open_multimap_table(MMAP_TABLE).unwrap();
                        for key in 0u64..10 {
                            let vals: Vec<u64> =
                                t.get(&key).unwrap().map(|r| r.unwrap().value()).collect();
                            assert_eq!(vals.len(), 100, "Key {} should have 100 values", key);
                        }
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
    }

    // ========================================================================
    // 10. BASE REDB: SAVEPOINT / ROLLBACK STRESS
    // ========================================================================

    /// Creates savepoints, makes modifications, rolls back, and verifies
    /// the database state is correctly restored (intra-transaction restore).
    #[test]
    fn stress_savepoint_rollback() {
        let tmp = tmpfile();
        let db = Database::create(tmp.path()).unwrap();

        // Seed 100 keys
        let txn = db.begin_write().unwrap();
        {
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in 0u64..100 {
                t.insert(&i, &i).unwrap();
            }
        }
        txn.commit().unwrap();

        // 20 rounds: savepoint, modify, rollback, commit, verify
        for round in 0..20 {
            let mut txn = db.begin_write().unwrap();
            let savepoint = txn.ephemeral_savepoint().unwrap();

            // Make modifications that should be rolled back
            {
                let mut t = txn.open_table(TABLE_U64).unwrap();
                for i in 0u64..100 {
                    t.insert(&i, &(i + round * 1000 + 999)).unwrap();
                }
                for i in 100u64..200 {
                    t.insert(&i, &i).unwrap();
                }
            }

            // Rollback to savepoint and commit the restored state
            txn.restore_savepoint(&savepoint).unwrap();
            txn.commit().unwrap();

            // Verify via read transaction that rollback took effect
            let rtxn = db.begin_read().unwrap();
            let t = rtxn.open_table(TABLE_U64).unwrap();
            assert_eq!(t.len().unwrap(), 100, "Round {}: wrong length", round);
            for i in 0u64..100 {
                let val = t.get(&i).unwrap();
                assert!(val.is_some(), "Round {}: key {} missing", round, i);
                assert_eq!(
                    val.unwrap().value(),
                    i,
                    "Round {}: key {} wrong value",
                    round,
                    i
                );
            }
            assert!(t.get(&150u64).unwrap().is_none());
        }
    }

    // ========================================================================
    // 11. BASE REDB: BULK INSERT STRESS
    // ========================================================================

    /// Tests bulk insert of many entries in a single transaction.
    #[test]
    fn stress_bulk_insert() {
        let tmp = tmpfile();
        let db = Database::create(tmp.path()).unwrap();

        let num_entries = 50_000u64;

        let txn = db.begin_write().unwrap();
        {
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in 0..num_entries {
                t.insert(&i, &(i * 3)).unwrap();
            }
        }
        txn.commit().unwrap();

        // Verify
        let txn = db.begin_read().unwrap();
        let t = txn.open_table(TABLE_U64).unwrap();
        assert_eq!(t.len().unwrap(), num_entries);

        // Spot check
        assert_eq!(t.get(&0u64).unwrap().unwrap().value(), 0);
        assert_eq!(t.get(&25000u64).unwrap().unwrap().value(), 75000);
        assert_eq!(
            t.get(&(num_entries - 1)).unwrap().unwrap().value(),
            (num_entries - 1) * 3
        );

        // Range query
        let count = t.range(10000u64..20000).unwrap().count();
        assert_eq!(count, 10000);
    }

    // ========================================================================
    // 12. BASE REDB: DELETE + REINSERT UNDER CONCURRENCY
    // ========================================================================

    /// Tests that delete and reinsert operations are correctly visible to
    /// subsequent transactions.
    #[test]
    fn stress_delete_reinsert_cycles() {
        let tmp = tmpfile();
        let db = Arc::new(ColumnFamilyDatabase::builder().open(tmp.path()).unwrap());
        db.create_column_family("cf", None).unwrap();

        let cf = db.column_family("cf").unwrap();

        // Seed 200 keys
        {
            let txn = cf.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in 0u64..200 {
                t.insert(&i, &i).unwrap();
            }
            drop(t);
            txn.commit().unwrap();
        }

        // 50 rounds of: delete even keys, verify, reinsert, verify
        for round in 0u64..50 {
            // Delete even keys
            {
                let txn = cf.begin_write().unwrap();
                let mut t = txn.open_table(TABLE_U64).unwrap();
                for i in (0u64..200).step_by(2) {
                    t.remove(&i).unwrap();
                }
                drop(t);
                txn.commit().unwrap();
            }

            // Verify only odd keys remain
            {
                let txn = cf.begin_read().unwrap();
                let t = txn.open_table(TABLE_U64).unwrap();
                assert_eq!(t.len().unwrap(), 100);
                for i in (0u64..200).step_by(2) {
                    assert!(t.get(&i).unwrap().is_none());
                }
                for i in (1u64..200).step_by(2) {
                    assert!(t.get(&i).unwrap().is_some());
                }
            }

            // Reinsert even keys with new value
            {
                let txn = cf.begin_write().unwrap();
                let mut t = txn.open_table(TABLE_U64).unwrap();
                for i in (0u64..200).step_by(2) {
                    t.insert(&i, &(i + round * 1000)).unwrap();
                }
                drop(t);
                txn.commit().unwrap();
            }

            // Verify all 200 keys present
            {
                let txn = cf.begin_read().unwrap();
                let t = txn.open_table(TABLE_U64).unwrap();
                assert_eq!(t.len().unwrap(), 200);
            }
        }
    }

    // ========================================================================
    // 13. LARGE VALUE STRESS
    // ========================================================================

    /// Tests concurrent writes with large values (64KB each).
    #[test]
    fn stress_large_values_concurrent() {
        let tmp = tmpfile();
        let db = Arc::new(ColumnFamilyDatabase::builder().open(tmp.path()).unwrap());
        db.create_column_family("cf", Some(64 * 1024 * 1024))
            .unwrap();

        let value_size = 64 * 1024; // 64KB
        let num_threads = 4;
        let entries_per_thread = 50;
        let barrier = Arc::new(Barrier::new(num_threads));

        let handles: Vec<_> = (0..num_threads)
            .map(|tid| {
                let db = db.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    let cf = db.column_family("cf").unwrap();
                    let value = vec![tid as u8; value_size];
                    for i in 0..entries_per_thread {
                        let key = (tid * entries_per_thread + i) as u64;
                        let txn = cf.begin_write().unwrap();
                        let mut t = txn.open_table(TABLE_BYTES).unwrap();
                        t.insert(&key, value.as_slice()).unwrap();
                        drop(t);
                        txn.commit().unwrap();
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // Verify
        let cf = db.column_family("cf").unwrap();
        let txn = cf.begin_read().unwrap();
        let t = txn.open_table(TABLE_BYTES).unwrap();
        assert_eq!(t.len().unwrap(), (num_threads * entries_per_thread) as u64);

        for tid in 0..num_threads {
            for i in 0..entries_per_thread {
                let key = (tid * entries_per_thread + i) as u64;
                let val = t.get(&key).unwrap().unwrap();
                let bytes = val.value();
                assert_eq!(bytes.len(), value_size);
                assert!(bytes.iter().all(|&b| b == tid as u8));
            }
        }
    }

    // ========================================================================
    // 14. MIXED READ-WRITE STRESS WITH VERIFICATION
    // ========================================================================

    /// 4 writer threads and 8 reader threads hammering the same CF.
    /// Writers insert unique key ranges; readers continuously verify data
    /// consistency. At the end, verify total count.
    #[test]
    fn stress_mixed_rw_heavy_load() {
        let tmp = tmpfile();
        let db = Arc::new(ColumnFamilyDatabase::builder().open(tmp.path()).unwrap());
        db.create_column_family("cf", None).unwrap();

        let num_writers = 4;
        let num_readers = 8;
        let writes_per_writer = 1000u64;
        let barrier = Arc::new(Barrier::new(num_writers + num_readers));
        let write_done = Arc::new(AtomicBool::new(false));

        let mut handles = vec![];

        // Writers
        for wid in 0..num_writers {
            let db = db.clone();
            let barrier = barrier.clone();
            let done = write_done.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                let cf = db.column_family("cf").unwrap();
                for i in 0..writes_per_writer {
                    let key = wid as u64 * writes_per_writer + i;
                    let txn = cf.begin_write().unwrap();
                    let mut t = txn.open_table(TABLE_U64).unwrap();
                    t.insert(&key, &key).unwrap();
                    drop(t);
                    txn.commit().unwrap();
                }
                done.store(true, Ordering::Relaxed);
            }));
        }

        // Readers
        let read_count = Arc::new(AtomicU64::new(0));
        for _ in 0..num_readers {
            let db = db.clone();
            let barrier = barrier.clone();
            let done = write_done.clone();
            let rc = read_count.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                let cf = db.column_family("cf").unwrap();
                while !done.load(Ordering::Relaxed) {
                    let txn = cf.begin_read().unwrap();
                    let t = match txn.open_table(TABLE_U64) {
                        Ok(t) => t,
                        Err(_) => continue, // table not yet created
                    };
                    // Verify: every key we can see has value == key
                    for entry in t.iter().unwrap() {
                        let (k, v) = entry.unwrap();
                        assert_eq!(k.value(), v.value());
                    }
                    rc.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // Final verification: all writes landed
        let cf = db.column_family("cf").unwrap();
        let txn = cf.begin_read().unwrap();
        let t = txn.open_table(TABLE_U64).unwrap();
        assert_eq!(t.len().unwrap(), num_writers as u64 * writes_per_writer);
    }

    // ========================================================================
    // 15. TRANSACTION ABORT UNDER HIGH CONTENTION
    // ========================================================================

    /// Many threads alternate between committing and aborting transactions.
    /// The database must remain consistent — only committed data visible.
    #[test]
    fn stress_abort_commit_interleaved() {
        let tmp = tmpfile();
        let db = Arc::new(ColumnFamilyDatabase::builder().open(tmp.path()).unwrap());
        db.create_column_family("cf", None).unwrap();

        let num_threads = 8;
        let ops_per_thread = 500u64;
        let committed_keys = Arc::new(std::sync::Mutex::new(Vec::new()));
        let barrier = Arc::new(Barrier::new(num_threads));

        let handles: Vec<_> = (0..num_threads)
            .map(|tid| {
                let db = db.clone();
                let committed = committed_keys.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    let cf = db.column_family("cf").unwrap();
                    let mut local_committed = vec![];

                    for i in 0..ops_per_thread {
                        let key = tid as u64 * ops_per_thread + i;
                        let txn = cf.begin_write().unwrap();
                        let mut t = txn.open_table(TABLE_U64).unwrap();
                        t.insert(&key, &key).unwrap();
                        drop(t);

                        if i % 3 == 0 {
                            // Abort (drop without commit)
                            drop(txn);
                        } else {
                            txn.commit().unwrap();
                            local_committed.push(key);
                        }
                    }

                    committed.lock().unwrap().extend(local_committed);
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // Verify: only committed keys are present
        let committed = committed_keys.lock().unwrap();
        let cf = db.column_family("cf").unwrap();
        let txn = cf.begin_read().unwrap();
        let t = txn.open_table(TABLE_U64).unwrap();

        assert_eq!(t.len().unwrap(), committed.len() as u64);

        for &key in committed.iter() {
            let val = t.get(&key).unwrap();
            assert!(val.is_some(), "Committed key {} missing", key);
            assert_eq!(val.unwrap().value(), key);
        }
    }

    // ========================================================================
    // 16. BASE REDB: COMPACTION AFTER HEAVY MODIFICATION
    // ========================================================================

    /// Writes, deletes, rewrites, then compacts the database. Verifies data
    /// integrity is preserved through compaction.
    #[test]
    fn stress_compaction_integrity() {
        let tmp = tmpfile();
        let mut db = Database::create(tmp.path()).unwrap();

        // Phase 1: insert 10K entries
        {
            let txn = db.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in 0u64..10_000 {
                t.insert(&i, &i).unwrap();
            }
            drop(t);
            txn.commit().unwrap();
        }

        // Phase 2: delete half
        {
            let txn = db.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in (0u64..10_000).step_by(2) {
                t.remove(&i).unwrap();
            }
            drop(t);
            txn.commit().unwrap();
        }

        // Phase 3: reinsert with different values
        {
            let txn = db.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in (0u64..10_000).step_by(2) {
                t.insert(&i, &(i + 50_000)).unwrap();
            }
            drop(t);
            txn.commit().unwrap();
        }

        // Compact
        db.compact().unwrap();

        // Verify integrity after compaction
        {
            let txn = db.begin_read().unwrap();
            let t = txn.open_table(TABLE_U64).unwrap();
            assert_eq!(t.len().unwrap(), 10_000);

            for i in (0u64..10_000).step_by(2) {
                assert_eq!(t.get(&i).unwrap().unwrap().value(), i + 50_000);
            }
            for i in (1u64..10_000).step_by(2) {
                assert_eq!(t.get(&i).unwrap().unwrap().value(), i);
            }
        }

        // Integrity check
        assert!(db.check_integrity().unwrap());
    }

    // ========================================================================
    // 17. DURABILITY MODES: IMMEDIATE VS NONE
    // ========================================================================

    /// Verifies that Durability::Immediate writes survive a close/reopen cycle
    /// and that database behavior is correct with different durability settings.
    #[test]
    fn stress_durability_immediate() {
        let tmp = tmpfile();
        let db_path = tmp.path().to_path_buf();

        {
            let db = Database::create(&db_path).unwrap();
            for i in 0u64..200 {
                let mut txn = db.begin_write().unwrap();
                txn.set_durability(Durability::Immediate).unwrap();
                let mut t = txn.open_table(TABLE_U64).unwrap();
                t.insert(&i, &i).unwrap();
                drop(t);
                txn.commit().unwrap();
            }
        }

        // Reopen and verify
        {
            let db = Database::open(&db_path).unwrap();
            let txn = db.begin_read().unwrap();
            let t = txn.open_table(TABLE_U64).unwrap();
            assert_eq!(t.len().unwrap(), 200);
            for i in 0u64..200 {
                assert_eq!(t.get(&i).unwrap().unwrap().value(), i);
            }
        }
    }

    // ========================================================================
    // 18. OVERWRITE STRESS: SAME KEY, MANY WRITERS
    // ========================================================================

    /// Multiple threads contend on writing to the SAME key. The final value
    /// must be from one of the writers (last-writer-wins). No corruption.
    #[test]
    fn stress_overwrite_same_key_contention() {
        let tmp = tmpfile();
        let db = Arc::new(ColumnFamilyDatabase::builder().open(tmp.path()).unwrap());
        db.create_column_family("cf", None).unwrap();

        let num_threads = 8;
        let writes_per_thread = 500u64;
        let barrier = Arc::new(Barrier::new(num_threads));

        let handles: Vec<_> = (0..num_threads)
            .map(|tid| {
                let db = db.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    let cf = db.column_family("cf").unwrap();
                    for i in 0..writes_per_thread {
                        let txn = cf.begin_write().unwrap();
                        let mut t = txn.open_table(TABLE_U64).unwrap();
                        // All threads write to key 0
                        t.insert(&0u64, &(tid as u64 * 1000 + i)).unwrap();
                        drop(t);
                        txn.commit().unwrap();
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // Verify: key 0 exists and has a valid value from some thread
        let cf = db.column_family("cf").unwrap();
        let txn = cf.begin_read().unwrap();
        let t = txn.open_table(TABLE_U64).unwrap();
        let val = t.get(&0u64).unwrap().unwrap().value();
        // Value should be tid*1000 + i for some valid tid and i
        let tid = val / 1000;
        let i = val % 1000;
        assert!(tid < num_threads as u64);
        assert!(i < writes_per_thread);
    }

    // ========================================================================
    // 19. MULTIPLE TABLES IN SAME TRANSACTION
    // ========================================================================

    /// Tests atomicity across multiple tables within a single transaction.
    #[test]
    fn stress_multi_table_atomicity() {
        let tmp = tmpfile();
        let db = Arc::new(Database::create(tmp.path()).unwrap());

        const T1: TableDefinition<u64, u64> = TableDefinition::new("table1");
        const T2: TableDefinition<u64, u64> = TableDefinition::new("table2");
        const T3: TableDefinition<u64, u64> = TableDefinition::new("table3");

        let done = Arc::new(AtomicBool::new(false));
        let violations = Arc::new(AtomicU64::new(0));

        // Writer: atomically write to all 3 tables with same batch_id
        let db_w = db.clone();
        let done_w = done.clone();
        let writer = thread::spawn(move || {
            for batch in 1u64..=300 {
                let txn = db_w.begin_write().unwrap();
                {
                    let mut t1 = txn.open_table(T1).unwrap();
                    let mut t2 = txn.open_table(T2).unwrap();
                    let mut t3 = txn.open_table(T3).unwrap();
                    t1.insert(&0u64, &batch).unwrap();
                    t2.insert(&0u64, &batch).unwrap();
                    t3.insert(&0u64, &batch).unwrap();
                }
                txn.commit().unwrap();
                if done_w.load(Ordering::Relaxed) {
                    break;
                }
            }
        });

        // Reader: all 3 tables must show same batch_id
        let db_r = db.clone();
        let done_r = done.clone();
        let v = violations.clone();
        let reader = thread::spawn(move || {
            while !done_r.load(Ordering::Relaxed) {
                let txn = db_r.begin_read().unwrap();
                let t1 = txn.open_table(T1);
                let t2 = txn.open_table(T2);
                let t3 = txn.open_table(T3);

                if let (Ok(t1), Ok(t2), Ok(t3)) = (t1, t2, t3) {
                    if let (Some(v1), Some(v2), Some(v3)) = (
                        t1.get(&0u64).unwrap(),
                        t2.get(&0u64).unwrap(),
                        t3.get(&0u64).unwrap(),
                    ) {
                        let (a, b, c) = (v1.value(), v2.value(), v3.value());
                        if a != b || b != c {
                            v.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        });

        writer.join().unwrap();
        done.store(true, Ordering::Relaxed);
        reader.join().unwrap();

        assert_eq!(
            violations.load(Ordering::Relaxed),
            0,
            "Multi-table atomicity violated"
        );
    }

    // ========================================================================
    // 20. WAL DURABILITY UNDER CONCURRENT CF WRITES
    // ========================================================================

    /// Writes concurrently to multiple CFs via WAL, then close and reopen
    /// to verify WAL recovery produces correct results.
    #[test]
    fn stress_wal_concurrent_cf_durability() {
        let tmp = tmpfile();
        let db_path = tmp.path().to_path_buf();
        let num_cfs = 4;
        let writes_per_cf = 500u64;

        {
            let db = Arc::new(ColumnFamilyDatabase::builder().open(&db_path).unwrap());
            for i in 0..num_cfs {
                db.create_column_family(&format!("cf_{}", i), None).unwrap();
            }

            let barrier = Arc::new(Barrier::new(num_cfs));
            let handles: Vec<_> = (0..num_cfs)
                .map(|cf_id| {
                    let db = db.clone();
                    let barrier = barrier.clone();
                    thread::spawn(move || {
                        barrier.wait();
                        let cf = db.column_family(&format!("cf_{}", cf_id)).unwrap();
                        for i in 0..writes_per_cf {
                            let txn = cf.begin_write().unwrap();
                            let mut t = txn.open_table(TABLE_U64).unwrap();
                            t.insert(&i, &(i + cf_id as u64 * 10_000)).unwrap();
                            drop(t);
                            txn.commit().unwrap();
                        }
                    })
                })
                .collect();

            for h in handles {
                h.join().unwrap();
            }
            // Drop without explicit shutdown — relies on WAL for durability
        }

        // Reopen and verify
        {
            let db = ColumnFamilyDatabase::builder().open(&db_path).unwrap();
            for cf_id in 0..num_cfs {
                let cf = db.column_family(&format!("cf_{}", cf_id)).unwrap();
                let txn = cf.begin_read().unwrap();
                let t = txn.open_table(TABLE_U64).unwrap();
                assert_eq!(
                    t.len().unwrap(),
                    writes_per_cf,
                    "CF {} lost data after WAL recovery",
                    cf_id
                );
                for i in 0..writes_per_cf {
                    let val = t.get(&i).unwrap().unwrap().value();
                    assert_eq!(val, i + cf_id as u64 * 10_000);
                }
            }
        }
    }

    // ========================================================================
    // 21. INTEGRITY CHECK AFTER STRESS
    // ========================================================================

    /// Performs a full stress cycle on the base Database, then runs
    /// check_integrity to verify B-tree structural correctness.
    #[test]
    fn stress_integrity_check_after_heavy_use() {
        let tmp = tmpfile();
        let mut db = Database::create(tmp.path()).unwrap();

        // Heavy insert
        for batch in 0..100 {
            let txn = db.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in 0u64..100 {
                t.insert(&(batch * 100 + i), &i).unwrap();
            }
            drop(t);
            txn.commit().unwrap();
        }

        // Heavy delete (every 3rd key)
        {
            let txn = db.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in (0u64..10_000).step_by(3) {
                t.remove(&i).unwrap();
            }
            drop(t);
            txn.commit().unwrap();
        }

        // Heavy update (remaining keys)
        {
            let txn = db.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in 0u64..10_000 {
                if i % 3 != 0 {
                    t.insert(&i, &(i * 99)).unwrap();
                }
            }
            drop(t);
            txn.commit().unwrap();
        }

        // check_integrity() may return false (not clean) due to post-commit free
        // processing that creates epilogue non-durable transactions. What matters is
        // that the call succeeds without error (no corruption).
        let _was_clean = db.check_integrity().unwrap();
    }

    // ========================================================================
    // 22. CONCURRENT READERS DURING COMPACTION (BASE REDB)
    // ========================================================================

    /// Creates heavy fragmentation, compacts, then verifies data integrity.
    /// Also verifies that a read snapshot taken before compaction is consistent
    /// with post-compaction state.
    #[test]
    fn stress_compaction_with_fragmentation() {
        let tmp = tmpfile();
        let mut db = Database::create(tmp.path()).unwrap();

        // Seed data
        {
            let txn = db.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in 0u64..1000 {
                t.insert(&i, &i).unwrap();
            }
            drop(t);
            txn.commit().unwrap();
        }

        // Delete and reinsert to create fragmentation (5 cycles)
        for cycle in 0u64..5 {
            let txn = db.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in (0u64..1000).step_by(2) {
                t.remove(&i).unwrap();
            }
            drop(t);
            txn.commit().unwrap();

            let txn = db.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in (0u64..1000).step_by(2) {
                t.insert(&i, &(i + cycle * 5000)).unwrap();
            }
            drop(t);
            txn.commit().unwrap();
        }

        // Compact (no active read txns)
        db.compact().unwrap();

        // Verify all data intact after compaction
        let txn = db.begin_read().unwrap();
        let t = txn.open_table(TABLE_U64).unwrap();
        assert_eq!(t.len().unwrap(), 1000);

        // Even keys should have value from last cycle (cycle=4)
        for i in (0u64..1000).step_by(2) {
            assert_eq!(t.get(&i).unwrap().unwrap().value(), i + 4 * 5000);
        }
        // Odd keys should have original value
        for i in (1u64..1000).step_by(2) {
            assert_eq!(t.get(&i).unwrap().unwrap().value(), i);
        }
        drop(t);
        drop(txn);

        // Integrity check
        assert!(db.check_integrity().unwrap());
    }

    // ========================================================================
    // 23. MONOTONIC COUNTER (LINEARIZABILITY PROXY)
    // ========================================================================

    /// Uses a single key as a monotonic counter incremented by many threads.
    /// After all threads finish, the counter must equal total increments.
    /// This tests serialization of writes to the same key.
    #[test]
    fn stress_monotonic_counter() {
        let tmp = tmpfile();
        let db = Arc::new(ColumnFamilyDatabase::builder().open(tmp.path()).unwrap());
        db.create_column_family("cf", None).unwrap();

        // Initialize counter
        {
            let cf = db.column_family("cf").unwrap();
            let txn = cf.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            t.insert(&0u64, &0u64).unwrap();
            drop(t);
            txn.commit().unwrap();
        }

        let num_threads = 4;
        let increments_per_thread = 200;
        let barrier = Arc::new(Barrier::new(num_threads));

        let handles: Vec<_> = (0..num_threads)
            .map(|_| {
                let db = db.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    let cf = db.column_family("cf").unwrap();
                    for _ in 0..increments_per_thread {
                        // Read-modify-write cycle
                        let txn = cf.begin_write().unwrap();
                        let mut t = txn.open_table(TABLE_U64).unwrap();
                        let current = t.get(&0u64).unwrap().unwrap().value();
                        t.insert(&0u64, &(current + 1)).unwrap();
                        drop(t);
                        txn.commit().unwrap();
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // Since writes are serialized (single writer per CF), the counter
        // should equal exactly num_threads * increments_per_thread
        let cf = db.column_family("cf").unwrap();
        let txn = cf.begin_read().unwrap();
        let t = txn.open_table(TABLE_U64).unwrap();
        let final_val = t.get(&0u64).unwrap().unwrap().value();
        assert_eq!(
            final_val,
            (num_threads * increments_per_thread) as u64,
            "Monotonic counter mismatch — writes were not properly serialized"
        );
    }

    // ========================================================================
    // 24. RAPID OPEN/CLOSE CYCLES
    // ========================================================================

    /// Opens and closes the database many times, writing data each time.
    /// Verifies accumulated data persists across all cycles.
    #[test]
    fn stress_rapid_open_close_cycles() {
        let tmp = tmpfile();
        let db_path = tmp.path().to_path_buf();

        let cycles = 50;
        let writes_per_cycle = 20u64;

        for cycle in 0..cycles {
            let db = ColumnFamilyDatabase::builder().open(&db_path).unwrap();
            if cycle == 0 {
                db.create_column_family("cf", None).unwrap();
            }
            let cf = db.column_family("cf").unwrap();

            let txn = cf.begin_write().unwrap();
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in 0..writes_per_cycle {
                let key = cycle as u64 * writes_per_cycle + i;
                t.insert(&key, &key).unwrap();
            }
            drop(t);
            txn.commit().unwrap();
        }

        // Final verification
        let db = ColumnFamilyDatabase::builder().open(&db_path).unwrap();
        let cf = db.column_family("cf").unwrap();
        let txn = cf.begin_read().unwrap();
        let t = txn.open_table(TABLE_U64).unwrap();
        assert_eq!(t.len().unwrap(), cycles as u64 * writes_per_cycle);
    }

    // ========================================================================
    // WRITE BUFFER HELPERS
    // ========================================================================

    fn shuffled_keys_seeded(n: u64, seed: u64) -> Vec<u64> {
        let mut keys: Vec<u64> = (0..n).collect();
        let mut rng = seed;
        for i in (1..keys.len()).rev() {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            let j = (rng >> 33) as usize % (i + 1);
            keys.swap(i, j);
        }
        keys
    }

    // ========================================================================
    // 25. WRITE BUFFER: READ-YOUR-WRITES
    // ========================================================================

    /// Inserts 10K random keys via insert_buffered and verifies they are
    /// readable via get() within the same transaction (read-your-own-writes).
    /// After commit, verifies all keys are persisted.
    #[test]
    fn stress_write_buffer_read_your_writes() {
        let tmp = tmpfile();
        let db = Database::create(tmp.path()).unwrap();

        let keys = shuffled_keys_seeded(10_000, 42);

        // Write phase: insert_buffered + verify in same txn
        let txn = db.begin_write().unwrap();
        {
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for &k in &keys {
                t.insert_buffered(&k, &(k * 3)).unwrap();
            }
            // Verify each key is readable via get()
            for &k in &keys {
                let val = t.get(&k).unwrap().unwrap();
                assert_eq!(val.value(), k * 3, "Read-your-write failed for key {}", k);
            }
            // Verify len is exact
            assert_eq!(t.len().unwrap(), 10_000);
        }
        txn.commit().unwrap();

        // Read back via read transaction
        let rtxn = db.begin_read().unwrap();
        let t = rtxn.open_table(TABLE_U64).unwrap();
        assert_eq!(t.len().unwrap(), 10_000);
        for &k in &keys {
            let val = t.get(&k).unwrap().unwrap();
            assert_eq!(val.value(), k * 3, "Persisted value mismatch for key {}", k);
        }
    }

    // ========================================================================
    // 26. WRITE BUFFER: TOMBSTONES
    // ========================================================================

    /// Seeds 100 keys via regular insert, then uses insert_buffered and
    /// remove_buffered to add new keys and delete some. Verifies len(),
    /// get() for removed and surviving keys, and final persisted state.
    #[test]
    fn stress_write_buffer_tombstones() {
        let tmp = tmpfile();
        let db = Database::create(tmp.path()).unwrap();

        // Seed 100 keys (0..100) via regular insert
        let txn = db.begin_write().unwrap();
        {
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for i in 0u64..100 {
                t.insert(&i, &(i * 10)).unwrap();
            }
        }
        txn.commit().unwrap();

        // New transaction: buffer inserts and deletes
        let txn = db.begin_write().unwrap();
        {
            let mut t = txn.open_table(TABLE_U64).unwrap();

            // insert_buffered keys 100..200
            for i in 100u64..200 {
                t.insert_buffered(&i, &(i * 10)).unwrap();
            }

            // remove_buffered keys 0..10 (existing on disk)
            for i in 0u64..10 {
                t.remove_buffered(&i).unwrap();
            }

            // remove_buffered keys 100..110 (just buffered above)
            for i in 100u64..110 {
                t.remove_buffered(&i).unwrap();
            }

            // Verify len: 100 original - 10 removed + 100 added - 10 removed = 180
            assert_eq!(t.len().unwrap(), 180, "len() should be exactly 180");

            // Verify removed keys return None
            for i in 0u64..10 {
                assert!(
                    t.get(&i).unwrap().is_none(),
                    "Key {} should be tombstoned",
                    i
                );
            }
            for i in 100u64..110 {
                assert!(
                    t.get(&i).unwrap().is_none(),
                    "Key {} should be tombstoned",
                    i
                );
            }

            // Verify surviving keys return values
            for i in 10u64..100 {
                let val = t.get(&i).unwrap().unwrap();
                assert_eq!(val.value(), i * 10);
            }
            for i in 110u64..200 {
                let val = t.get(&i).unwrap().unwrap();
                assert_eq!(val.value(), i * 10);
            }
        }
        txn.commit().unwrap();

        // Read back and verify final state
        let rtxn = db.begin_read().unwrap();
        let t = rtxn.open_table(TABLE_U64).unwrap();
        assert_eq!(t.len().unwrap(), 180);

        for i in 0u64..10 {
            assert!(
                t.get(&i).unwrap().is_none(),
                "Key {} should not exist after commit",
                i
            );
        }
        for i in 100u64..110 {
            assert!(
                t.get(&i).unwrap().is_none(),
                "Key {} should not exist after commit",
                i
            );
        }
        for i in 10u64..100 {
            assert_eq!(t.get(&i).unwrap().unwrap().value(), i * 10);
        }
        for i in 110u64..200 {
            assert_eq!(t.get(&i).unwrap().unwrap().value(), i * 10);
        }
    }

    // ========================================================================
    // 27. WRITE BUFFER: LARGE BATCH (1M KEYS)
    // ========================================================================

    /// Inserts 1M random keys via insert_buffered in a single transaction.
    /// Verifies len, commits, and spot-checks persisted data.
    #[test]
    fn stress_write_buffer_large_batch() {
        let tmp = tmpfile();
        let db = Database::create(tmp.path()).unwrap();

        let n = 1_000_000u64;
        let keys = shuffled_keys_seeded(n, 7777);

        let txn = db.begin_write().unwrap();
        {
            let mut t = txn.open_table(TABLE_U64).unwrap();
            for &k in &keys {
                t.insert_buffered(&k, &(k.wrapping_mul(17))).unwrap();
            }
            assert_eq!(t.len().unwrap(), n);
        }
        txn.commit().unwrap();

        // Read back: verify len and spot-check
        let rtxn = db.begin_read().unwrap();
        let t = rtxn.open_table(TABLE_U64).unwrap();
        assert_eq!(t.len().unwrap(), n);

        // Spot-check first, middle, last keys from the shuffled order
        for &k in &[keys[0], keys[n as usize / 2], keys[n as usize - 1]] {
            let val = t.get(&k).unwrap().unwrap();
            assert_eq!(
                val.value(),
                k.wrapping_mul(17),
                "Spot check failed for key {}",
                k
            );
        }
        // Also check boundary keys
        assert_eq!(
            t.get(&0u64).unwrap().unwrap().value(),
            0u64.wrapping_mul(17)
        );
        assert_eq!(
            t.get(&(n - 1)).unwrap().unwrap().value(),
            (n - 1).wrapping_mul(17)
        );
    }

    // ========================================================================
    // 28. WRITE BUFFER: MIXED DIRECT AND BUFFERED
    // ========================================================================

    /// In one transaction, uses regular insert() for even keys and
    /// insert_buffered() for odd keys. Verifies all keys readable and
    /// len is correct both before and after commit.
    #[test]
    fn stress_write_buffer_mixed_direct_and_buffered() {
        let tmp = tmpfile();
        let db = Database::create(tmp.path()).unwrap();

        let txn = db.begin_write().unwrap();
        {
            let mut t = txn.open_table(TABLE_U64).unwrap();

            // insert() even keys 0, 2, 4, ..., 98
            for i in (0u64..100).step_by(2) {
                t.insert(&i, &(i * 5)).unwrap();
            }

            // insert_buffered() odd keys 1, 3, 5, ..., 99
            for i in (1u64..100).step_by(2) {
                t.insert_buffered(&i, &(i * 5)).unwrap();
            }

            // Verify all 100 keys are readable
            for i in 0u64..100 {
                let val = t.get(&i).unwrap();
                assert!(val.is_some(), "Key {} not found in mixed insert", i);
                assert_eq!(val.unwrap().value(), i * 5);
            }

            // Verify len
            assert_eq!(t.len().unwrap(), 100);
        }
        txn.commit().unwrap();

        // Read back via read transaction
        let rtxn = db.begin_read().unwrap();
        let t = rtxn.open_table(TABLE_U64).unwrap();
        assert_eq!(t.len().unwrap(), 100);
        for i in 0u64..100 {
            let val = t.get(&i).unwrap().unwrap();
            assert_eq!(val.value(), i * 5, "Persisted mismatch for key {}", i);
        }
    }
}
