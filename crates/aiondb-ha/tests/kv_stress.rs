//! Concurrent stress test : prove the replicated KV store survives
//! 8-way concurrent writers + 4 readers with strict consistency
//! invariants.
//!
//! Invariants verified:
//!
//! 1. **No lost updates.** Counter incremented N times by N writers
//!    ends at exactly N*M.
//! 2. **No torn reads.** Readers never observe a half-written value
//!    (e.g. partial JSON, prefix-only key).
//! 3. **Fenced exclusion.** When two tasks attempt to claim the same
//!    advisory lock concurrently, only one succeeds; the loser
//!    observes the fenced holder.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aiondb_ha::distributed_locks::{AcquireOutcome, DistributedLockService};
use aiondb_ha::kv_engine::KvEngine;
use aiondb_ha::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
use aiondb_ha::protocol::NodeId;

fn fresh_engine() -> (
    tempfile::TempDir,
    Arc<MultiRaftRegistry>,
    KvEngine,
    MultiRaftGroupId,
) {
    let tmp = tempfile::tempdir().unwrap();
    let reg = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap());
    let group = MultiRaftGroupId::new(1);
    reg.create_group(group, 1).unwrap();
    reg.become_leader(group, &[]).unwrap();
    let engine = KvEngine::new(Arc::clone(&reg));
    (tmp, reg, engine, group)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cas_loop_under_8_writers_preserves_count() {
    let (_tmp, _reg, engine, group) = fresh_engine();
    engine
        .put(group, b"counter".to_vec(), b"0".to_vec())
        .unwrap();

    let total: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let writers = 8;
    let increments_per_writer = 25u64;

    let mut handles = Vec::new();
    for _ in 0..writers {
        let engine = engine.clone();
        let total = Arc::clone(&total);
        handles.push(tokio::task::spawn_blocking(move || {
            for _ in 0..increments_per_writer {
                loop {
                    let current = engine.get(group, b"counter").unwrap();
                    let parsed: u64 = std::str::from_utf8(current.as_ref().unwrap())
                        .unwrap()
                        .parse()
                        .unwrap();
                    let next = (parsed + 1).to_string().into_bytes();
                    if engine
                        .cas(group, b"counter".to_vec(), current, Some(next))
                        .unwrap()
                    {
                        total.fetch_add(1, Ordering::SeqCst);
                        break;
                    }
                }
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let final_val = engine.get(group, b"counter").unwrap().unwrap();
    let parsed: u64 = std::str::from_utf8(&final_val).unwrap().parse().unwrap();
    assert_eq!(parsed, writers as u64 * increments_per_writer);
    assert_eq!(
        total.load(Ordering::SeqCst),
        writers as u64 * increments_per_writer
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn readers_never_see_torn_values_during_writer_storm() {
    let (_tmp, _reg, engine, group) = fresh_engine();
    // Seed with a sentinel value.
    engine
        .put(group, b"k".to_vec(), b"INITIAL".to_vec())
        .unwrap();

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // 4 writers spam the same key with different payloads.
    let mut writer_handles = Vec::new();
    for w in 0..4u64 {
        let engine = engine.clone();
        let stop = Arc::clone(&stop);
        writer_handles.push(tokio::task::spawn_blocking(move || {
            let mut i = 0u64;
            while !stop.load(Ordering::SeqCst) {
                let payload = format!("W{w}-{i}").into_bytes();
                engine.put(group, b"k".to_vec(), payload).unwrap();
                i += 1;
                if i > 200 {
                    break;
                }
            }
        }));
    }
    // 4 readers spin reading the same key.
    let mut reader_handles = Vec::new();
    for _ in 0..4 {
        let engine = engine.clone();
        let stop = Arc::clone(&stop);
        reader_handles.push(tokio::task::spawn_blocking(move || {
            let mut reads = 0u64;
            while !stop.load(Ordering::SeqCst) && reads < 5_000 {
                if let Some(v) = engine.get(group, b"k").unwrap() {
                    let text = std::str::from_utf8(&v).unwrap_or("");
                    // Every successful read must match either INITIAL
                    // or W<id>-<seq>. Anything else means we read a
                    // torn value.
                    let ok = text == "INITIAL"
                        || (text.starts_with('W')
                            && text.contains('-')
                            && text[1..].split('-').count() == 2);
                    assert!(ok, "torn read: {text:?}");
                }
                reads += 1;
            }
        }));
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    stop.store(true, Ordering::SeqCst);
    for h in writer_handles {
        h.await.unwrap();
    }
    for h in reader_handles {
        h.await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lock_contention_admits_exactly_one_winner() {
    let (_tmp, _reg, engine, group) = fresh_engine();
    let svc = Arc::new(DistributedLockService::new(engine, group));

    let winners = Arc::new(AtomicU64::new(0));
    let losers = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::new();
    for i in 0..16u64 {
        let svc = Arc::clone(&svc);
        let winners = Arc::clone(&winners);
        let losers = Arc::clone(&losers);
        handles.push(tokio::task::spawn_blocking(move || {
            // All 16 tasks try to grab the same lock with a long TTL.
            let outcome = svc
                .acquire("singleton", format!("task-{i}"), Duration::from_secs(60))
                .unwrap();
            match outcome {
                AcquireOutcome::Granted(_) => {
                    winners.fetch_add(1, Ordering::SeqCst);
                }
                AcquireOutcome::Held { .. } => {
                    losers.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(winners.load(Ordering::SeqCst), 1, "exactly one winner");
    assert_eq!(losers.load(Ordering::SeqCst), 15);
    let record = svc.inspect("singleton").unwrap().unwrap();
    assert!(record.holder.starts_with("task-"));
}
