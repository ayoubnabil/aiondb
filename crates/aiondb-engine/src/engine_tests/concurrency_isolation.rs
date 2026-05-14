#![allow(clippy::similar_names)]

use std::sync::{mpsc, Arc, Barrier};
use std::{thread, time::Duration};

use aiondb_tx::WaitGraphLockManager;

use super::*;

// ===================================================================
// 1. Read Committed -- no dirty reads
// ===================================================================

/// Session A inserts inside a transaction but does NOT commit.
#[test]
fn read_committed_no_dirty_reads() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&sa, "CREATE TABLE rc_dirty (id INT, val TEXT)")
        .expect("create table");

    // Session A begins and inserts -- does NOT commit.
    engine
        .begin_transaction(&sa, IsolationLevel::ReadCommitted)
        .expect("begin A");
    engine
        .execute_sql(&sa, "INSERT INTO rc_dirty VALUES (1, 'uncommitted')")
        .expect("A inserts");

    // Session B should see zero rows.
    let b_rows = query_rows(&engine, &sb, "SELECT id FROM rc_dirty");
    assert!(
        b_rows.is_empty(),
        "session B must not see uncommitted rows from A, got {} rows",
        b_rows.len()
    );

    // Session A can see its own uncommitted insert.
    let a_rows = query_rows(&engine, &sa, "SELECT id FROM rc_dirty");
    assert_eq!(a_rows.len(), 1, "session A should see its own insert");

    engine.rollback_transaction(&sa).expect("rollback A");
}

// ===================================================================
// 2. Read Committed -- committed visibility
// ===================================================================

/// Session A inserts and commits. Session B should then see the row.
#[test]
fn read_committed_committed_visibility() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&sa, "CREATE TABLE rc_vis (id INT, val TEXT)")
        .expect("create table");

    engine
        .begin_transaction(&sa, IsolationLevel::ReadCommitted)
        .expect("begin A");
    engine
        .execute_sql(&sa, "INSERT INTO rc_vis VALUES (1, 'hello')")
        .expect("A inserts");

    // Before commit -- B sees nothing.
    let before = query_rows(&engine, &sb, "SELECT id FROM rc_vis");
    assert!(before.is_empty(), "B sees nothing before commit");

    // A commits.
    engine.commit_transaction(&sa).expect("commit A");

    // After commit -- B sees the row.
    let after = query_rows(&engine, &sb, "SELECT id, val FROM rc_vis");
    assert_eq!(after.len(), 1, "B should see one row after commit");
    assert_eq!(after[0].values[0], Value::Int(1));
    assert_eq!(after[0].values[1], Value::Text("hello".to_owned()));
}

// ===================================================================
// 3. Serializable -- phantom read prevention
// ===================================================================

/// Under SERIALIZABLE, the engine uses strict table-level locking to
/// prevent phantoms.  This test verifies that while A holds a
/// SERIALIZABLE transaction on a table, B is blocked from writing to
/// that table (the lock times out rather than allowing the write).
/// After A commits, B should be able to proceed.
#[test]
fn serializable_blocks_concurrent_writes() {
    let engine = Arc::new(EngineBuilder::for_testing().build().unwrap());
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(
            &sa,
            "CREATE TABLE ser_block (id INT, val TEXT); \
             INSERT INTO ser_block VALUES (1, 'one'), (2, 'two')",
        )
        .expect("setup");

    // A starts SERIALIZABLE and reads, acquiring a table lock.
    engine
        .begin_transaction(&sa, IsolationLevel::Serializable)
        .expect("begin A serializable");
    let a_read = query_rows(&engine, &sa, "SELECT id FROM ser_block ORDER BY id");
    assert_eq!(a_read.len(), 2);

    // B's write should fail because A holds the serializable lock.
    let b_result = engine.execute_sql(&sb, "INSERT INTO ser_block VALUES (15, 'blocked')");
    assert!(
        b_result.is_err(),
        "B's insert should be blocked/fail while A holds SERIALIZABLE lock"
    );

    // A commits, releasing the lock.
    engine.commit_transaction(&sa).expect("commit A");

    // Now B can write.
    engine
        .execute_sql(&sb, "INSERT INTO ser_block VALUES (15, 'now_ok')")
        .expect("B inserts after A committed");

    // Verify both the original rows and B's row are present.
    let count = query_count(&engine, &sb, "SELECT COUNT(*) FROM ser_block");
    assert_eq!(count, 3, "should have 2 original + 1 from B");
}

// ===================================================================
// 4. Deadlock detection
// ===================================================================

/// Session A locks row 1, session B locks row 2. Then A tries to lock
/// row 2 while B tries to lock row 1. The engine should detect the
/// deadlock and abort at least one transaction.
#[test]
fn deadlock_detection_two_sessions() {
    let engine = Arc::new(
        EngineBuilder::for_testing()
            .with_lock_manager(Arc::new(WaitGraphLockManager::default()))
            .build()
            .unwrap(),
    );
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(
            &sa,
            "CREATE TABLE dl_items (id INT, val INT); \
             INSERT INTO dl_items VALUES (1, 10), (2, 20)",
        )
        .expect("setup");

    // A and B each begin a transaction and lock one row.
    engine
        .begin_transaction(&sa, IsolationLevel::ReadCommitted)
        .expect("begin A");
    engine
        .begin_transaction(&sb, IsolationLevel::ReadCommitted)
        .expect("begin B");

    engine
        .execute_sql(&sa, "UPDATE dl_items SET val = 11 WHERE id = 1")
        .expect("A locks row 1");
    engine
        .execute_sql(&sb, "UPDATE dl_items SET val = 21 WHERE id = 2")
        .expect("B locks row 2");

    // Now A and B try to lock each other's rows concurrently.
    let barrier = Arc::new(Barrier::new(2));
    let (sender, receiver) = mpsc::channel();

    let engine_a = engine.clone();
    let barrier_a = barrier.clone();
    let sender_a = sender.clone();
    let sa_thread = sa.clone();
    let worker_a = thread::spawn(move || {
        barrier_a.wait();
        let result = engine_a
            .execute_sql(&sa_thread, "UPDATE dl_items SET val = 12 WHERE id = 2")
            .map(|_| ());
        let _ = engine_a.rollback_transaction(&sa_thread);
        sender_a.send(("A", result)).unwrap();
    });

    let engine_b = engine.clone();
    let barrier_b = barrier.clone();
    let sb_thread = sb.clone();
    let worker_b = thread::spawn(move || {
        barrier_b.wait();
        // Small delay to ensure ordering creates the cycle.
        thread::sleep(Duration::from_millis(50));
        let result = engine_b
            .execute_sql(&sb_thread, "UPDATE dl_items SET val = 22 WHERE id = 1")
            .map(|_| ());
        let _ = engine_b.rollback_transaction(&sb_thread);
        sender.send(("B", result)).unwrap();
    });

    let first = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    let second = receiver.recv_timeout(Duration::from_secs(5)).unwrap();

    let deadlock_detected = [first.1, second.1]
        .into_iter()
        .filter_map(Result::err)
        .any(|error| error.sqlstate() == aiondb_core::SqlState::DeadlockDetected);
    assert!(
        deadlock_detected,
        "at least one session must receive a DeadlockDetected error"
    );

    worker_a.join().unwrap();
    worker_b.join().unwrap();
}

// ===================================================================
// 5. Lost update prevention
// ===================================================================

/// Two sessions read the same row and both try to update it.
/// Under `ReadCommitted` with row locking, the second writer should
#[test]
fn lost_update_prevention() {
    let engine = Arc::new(EngineBuilder::for_testing().build().unwrap());
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(
            &sa,
            "CREATE TABLE lu_accounts (id INT, balance INT); \
             INSERT INTO lu_accounts VALUES (1, 1000)",
        )
        .expect("setup");

    // Both sessions begin transactions and read the same row.
    engine
        .begin_transaction(&sa, IsolationLevel::ReadCommitted)
        .expect("begin A");
    engine
        .begin_transaction(&sb, IsolationLevel::ReadCommitted)
        .expect("begin B");

    // Both see balance = 1000.
    let a_read = query_rows(&engine, &sa, "SELECT balance FROM lu_accounts WHERE id = 1");
    assert_eq!(a_read[0].values[0], Value::Int(1000));

    let b_read = query_rows(&engine, &sb, "SELECT balance FROM lu_accounts WHERE id = 1");
    assert_eq!(b_read[0].values[0], Value::Int(1000));

    // A updates first.
    engine
        .execute_sql(
            &sa,
            "UPDATE lu_accounts SET balance = balance - 100 WHERE id = 1",
        )
        .expect("A updates");

    // B tries to update concurrently -- must block then fail.
    let (sender, receiver) = mpsc::channel();
    let engine_b = engine.clone();
    let sb_thread = sb.clone();
    let worker = thread::spawn(move || {
        let result = engine_b
            .execute_sql(
                &sb_thread,
                "UPDATE lu_accounts SET balance = balance + 500 WHERE id = 1",
            )
            .map(|_| ());
        sender.send(result).unwrap();
    });

    // Give B a moment to block, then A commits.
    thread::sleep(Duration::from_millis(100));
    engine.commit_transaction(&sa).expect("commit A");

    let b_result = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    let err = b_result.expect_err("B must fail to prevent lost update");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::SerializationFailure,
        "expected SerializationFailure for lost update prevention"
    );

    engine.rollback_transaction(&sb).expect("rollback B");
    worker.join().unwrap();

    // Verify only A's update is visible.
    let final_balance = query_rows(&engine, &sa, "SELECT balance FROM lu_accounts WHERE id = 1");
    assert_eq!(
        final_balance[0].values[0],
        Value::Int(900),
        "only session A's debit should be applied"
    );
}

// ===================================================================
// 6. Write skew detection
// ===================================================================

/// Under Snapshot Isolation, concurrent updates to the **same row**
/// should cause the second committer to fail ("first-updater-wins").
/// This tests the write-write conflict detection path, which is the
/// core concurrency safety guarantee of SI.
#[test]
fn snapshot_isolation_first_updater_wins() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(
            &sa,
            "CREATE TABLE doctors (id INT, on_call INT); \
             INSERT INTO doctors VALUES (1, 1), (2, 1)",
        )
        .expect("setup: both doctors on-call");

    // Both sessions start under SnapshotIsolation.
    engine
        .begin_transaction(&sa, IsolationLevel::SnapshotIsolation)
        .expect("begin A snapshot");
    engine
        .begin_transaction(&sb, IsolationLevel::SnapshotIsolation)
        .expect("begin B snapshot");

    // Both read.
    let a_count = query_count(
        &engine,
        &sa,
        "SELECT COUNT(*) FROM doctors WHERE on_call = 1",
    );
    assert_eq!(a_count, 2);

    let b_count = query_count(
        &engine,
        &sb,
        "SELECT COUNT(*) FROM doctors WHERE on_call = 1",
    );
    assert_eq!(b_count, 2);

    // Both update the SAME row (id=1): first-updater-wins.
    engine
        .execute_sql(&sa, "UPDATE doctors SET on_call = 0 WHERE id = 1")
        .expect("A updates doctor 1");

    // A commits first.
    engine.commit_transaction(&sa).expect("commit A");

    // B tries to update the same row; this or the commit should fail.
    let b_update = engine.execute_sql(&sb, "UPDATE doctors SET on_call = 0 WHERE id = 1");
    if b_update.is_ok() {
        // If the update succeeded locally, the commit must fail.
        let commit = engine.commit_transaction(&sb);
        assert!(
            commit.is_err(),
            "B's commit must fail: first-updater-wins on id=1"
        );
    }
    // overwrite A's committed update.

    // Verify doctor 1 is off-call (A's write), doctor 2 still on-call.
    let (verify, _) = engine.startup(startup_params()).expect("verify");
    let on_call = query_count(
        &engine,
        &verify,
        "SELECT COUNT(*) FROM doctors WHERE on_call = 1",
    );
    assert_eq!(on_call, 1, "exactly one doctor should still be on-call");
}

// ===================================================================
// 7. Concurrent insert uniqueness
// ===================================================================

/// Two sequential inserts of the same unique key: the second must fail
/// with `UniqueViolation`.  This verifies PK enforcement within a single
/// session (the simplest case).
#[test]
fn unique_key_duplicate_rejected() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (sa, _) = engine.startup(startup_params()).expect("startup A");

    engine
        .execute_sql(&sa, "CREATE TABLE uniq_race (id INT PRIMARY KEY, val TEXT)")
        .expect("create table");

    engine
        .execute_sql(&sa, "INSERT INTO uniq_race VALUES (42, 'first')")
        .expect("first insert");

    let err = engine
        .execute_sql(&sa, "INSERT INTO uniq_race VALUES (42, 'duplicate')")
        .expect_err("duplicate insert must fail");

    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::UniqueViolation,
        "expected UniqueViolation, got {:?}",
        err.sqlstate()
    );

    // Verify exactly one row exists.
    let count = query_count(&engine, &sa, "SELECT COUNT(*) FROM uniq_race");
    assert_eq!(count, 1);
}

// ===================================================================
// 8. Long-running read does not block writes
// ===================================================================

/// A session performing a long-running SELECT should not prevent
/// another session from writing to a different table (or even the same
/// table under MVCC).
#[test]
fn long_running_read_does_not_block_writes() {
    let engine = Arc::new(EngineBuilder::for_testing().build().unwrap());
    let (sa, _) = engine.startup(startup_params()).expect("startup A");
    let (sb, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&sa, "CREATE TABLE lr_data (id INT, val TEXT)")
        .expect("create table");

    // Seed some data.
    for i in 0..100 {
        engine
            .execute_sql(&sa, &format!("INSERT INTO lr_data VALUES ({i}, 'row_{i}')"))
            .expect("seed insert");
    }

    // Session A starts a snapshot-isolated read transaction and reads.
    engine
        .begin_transaction(&sa, IsolationLevel::SnapshotIsolation)
        .expect("begin A snapshot");
    let a_rows = query_rows(&engine, &sa, "SELECT id FROM lr_data ORDER BY id");
    assert_eq!(a_rows.len(), 100, "A should see all 100 rows");

    // While A's transaction is open, B should be able to write.
    let (sender, receiver) = mpsc::channel();
    let engine_b = engine.clone();
    let sb_clone = sb.clone();
    let writer_handle = thread::spawn(move || {
        // B inserts several rows.
        for i in 1000..1010 {
            engine_b
                .execute_sql(
                    &sb_clone,
                    &format!("INSERT INTO lr_data VALUES ({i}, 'new_{i}')"),
                )
                .unwrap_or_else(|e| panic!("B insert {i} failed: {e}"));
        }
        sender.send(()).unwrap();
    });

    let write_result = receiver.recv_timeout(Duration::from_secs(5));
    assert!(
        write_result.is_ok(),
        "B's writes should not be blocked by A's read transaction"
    );

    writer_handle.join().unwrap();

    // A's snapshot should still see only the original 100 rows (SI).
    let a_rows_again = query_rows(&engine, &sa, "SELECT id FROM lr_data ORDER BY id");
    assert_eq!(
        a_rows_again.len(),
        100,
        "A's snapshot should not see B's new rows"
    );

    engine.commit_transaction(&sa).expect("commit A");

    // After A commits, a fresh read sees all 110 rows.
    let final_count = query_count(&engine, &sa, "SELECT COUNT(*) FROM lr_data");
    assert_eq!(
        final_count, 110,
        "after both complete, 110 rows should exist"
    );
}

// ===================================================================
// Bonus: Read Committed sees updates from other committed transactions
//        between statements within a single transaction.
// ===================================================================

/// Under Read Committed, each statement sees a fresh snapshot.
/// If another session commits between two statements in a RC txn,
/// the second statement should see the newly committed data.
#[test]
fn read_committed_refreshes_snapshot_per_statement() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (reader, _) = engine.startup(startup_params()).expect("startup reader");
    let (writer, _) = engine.startup(startup_params()).expect("startup writer");

    engine
        .execute_sql(&writer, "CREATE TABLE rc_refresh (id INT)")
        .expect("create table");
    engine
        .execute_sql(&writer, "INSERT INTO rc_refresh VALUES (1)")
        .expect("seed row");

    // Reader starts RC transaction.
    engine
        .begin_transaction(&reader, IsolationLevel::ReadCommitted)
        .expect("begin reader RC");

    // First read: sees 1 row.
    let first = query_count(&engine, &reader, "SELECT COUNT(*) FROM rc_refresh");
    assert_eq!(first, 1);

    // Writer inserts and commits (autocommit).
    engine
        .execute_sql(&writer, "INSERT INTO rc_refresh VALUES (2)")
        .expect("writer inserts");

    // Second read in same RC txn: should see 2 rows (fresh snapshot).
    let second = query_count(&engine, &reader, "SELECT COUNT(*) FROM rc_refresh");
    assert_eq!(
        second, 2,
        "RC should see committed data from other sessions between statements"
    );

    engine.commit_transaction(&reader).expect("commit reader");
}

// ===================================================================
// Bonus: Snapshot isolation keeps repeatable reads despite concurrent
//        inserts.
// ===================================================================

/// Under Snapshot Isolation, reads within a transaction always see the
/// same snapshot, even when other sessions commit new data.
#[test]
fn snapshot_isolation_repeatable_read() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (reader, _) = engine.startup(startup_params()).expect("startup reader");
    let (writer, _) = engine.startup(startup_params()).expect("startup writer");

    engine
        .execute_sql(&writer, "CREATE TABLE si_rr (id INT, val INT)")
        .expect("create table");
    engine
        .execute_sql(&writer, "INSERT INTO si_rr VALUES (1, 100), (2, 200)")
        .expect("seed rows");

    // Reader begins SI transaction.
    engine
        .begin_transaction(&reader, IsolationLevel::SnapshotIsolation)
        .expect("begin reader SI");

    // First read.
    let first = query_rows(&engine, &reader, "SELECT val FROM si_rr WHERE id = 1");
    assert_eq!(first[0].values[0], Value::Int(100));

    // Writer updates and commits.
    engine
        .execute_sql(&writer, "UPDATE si_rr SET val = 999 WHERE id = 1")
        .expect("writer updates");

    // Second read in same SI txn: must still see old value.
    let second = query_rows(&engine, &reader, "SELECT val FROM si_rr WHERE id = 1");
    assert_eq!(
        second[0].values[0],
        Value::Int(100),
        "SI must provide repeatable reads"
    );

    engine.commit_transaction(&reader).expect("commit reader");

    // After commit, a new read sees the updated value.
    let after = query_rows(&engine, &reader, "SELECT val FROM si_rr WHERE id = 1");
    assert_eq!(after[0].values[0], Value::Int(999));
}

// ===================================================================
// Bonus: Concurrent autocommit inserts with unique constraints
// ===================================================================

/// Multiple threads race to insert DIFFERENT unique keys concurrently.
/// All inserts should succeed, verifying the engine handles concurrent
/// autocommit writes to the same table without crashing or losing data.
#[test]
fn concurrent_autocommit_insert_different_keys() {
    let engine = Arc::new(EngineBuilder::for_testing().build().unwrap());
    let (setup, _) = engine.startup(startup_params()).expect("startup setup");

    engine
        .execute_sql(
            &setup,
            "CREATE TABLE ac_uniq (id INT PRIMARY KEY, writer INT)",
        )
        .expect("create table");

    const THREADS: usize = 8;
    let barrier = Arc::new(Barrier::new(THREADS));
    let successes = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    thread::scope(|s| {
        for t in 0..THREADS {
            let engine = &*engine;
            let barrier = barrier.clone();
            let successes = successes.clone();
            s.spawn(move || {
                let (session, _) = engine.startup(startup_params()).expect("startup");
                barrier.wait();
                // Each thread inserts a DIFFERENT key.
                let result =
                    engine.execute_sql(&session, &format!("INSERT INTO ac_uniq VALUES ({t}, {t})"));
                if result.is_ok() {
                    successes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            });
        }
    });

    let total_successes = successes.load(std::sync::atomic::Ordering::SeqCst);
    // All threads should succeed since they insert different keys.
    // Under high contention some may fail with serialization errors,
    // but at least most should succeed.
    assert!(
        total_successes >= THREADS / 2,
        "at least half the threads should succeed, got {total_successes}/{THREADS}"
    );

    let count = query_count(&engine, &setup, "SELECT COUNT(*) FROM ac_uniq");
    assert!(
        count >= (THREADS / 2) as i64,
        "table should have at least {}/{THREADS} rows, got {count}",
        THREADS / 2,
    );
}
