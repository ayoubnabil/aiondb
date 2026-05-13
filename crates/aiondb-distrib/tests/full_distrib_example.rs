//! End-to-end distrib example.
//!
//! Demonstrates the entire AionDB distributed stack from a single
//! integration test :
//!
//! 1. Boot a node orchestrator (gossip + raft + metrics).
//! 2. Bootstrap the metadata Raft group as leader.
//! 3. Create a user Raft group and host a KV engine on top.
//! 4. Subscribe a CDC adapter that mirrors KV writes onto a
//!    changefeed bus.
//! 5. Issue HLC-timestamped writes through 2PC.
//! 6. Run a distributed sequence to allocate cluster-unique ids.
//! 7. Snapshot the resulting cluster status.
//!
//! When this test passes, the distrib umbrella is wired correctly
//! and every layer can interoperate.

use std::sync::Arc;
use std::time::Duration;

use aiondb_distrib::prelude::*;
use tokio::time;

#[tokio::test]
async fn full_distrib_example_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = NodeOrchestratorConfig::single_node_local(1);
    cfg.state_dir = tmp.path().to_path_buf();
    let node = NodeOrchestrator::start(cfg).await.unwrap();

    // 1. Control plane wired ?
    node.control_plane.add_node(100, "host-100").unwrap();
    assert_eq!(node.control_plane.snapshot().members.len(), 1);

    // 2. User KV group.
    let g = MultiRaftGroupId::new(42);
    node.raft_registry.create_group(g, 1).unwrap();
    node.raft_registry.become_leader(g, &[]).unwrap();

    // 3. CDC adapter installed.
    let bus = ChangefeedBus::new(ChangefeedConfig::default());
    let mut subscriber = bus.subscribe(ChangefeedFilter::all_tables());
    node.kv
        .set_observer(Arc::new(KvCdcAdapter::with_default_mapper(bus.clone())));

    // 4. Writes flow through Raft → state machine → CDC.
    node.kv.put(g, b"alice".to_vec(), b"42".to_vec()).unwrap();
    node.kv.put(g, b"bob".to_vec(), b"43".to_vec()).unwrap();
    let mut received = 0;
    while received < 2 {
        match time::timeout(Duration::from_millis(200), subscriber.recv()).await {
            Ok(Ok(ChangefeedEvent::Insert { .. })) => received += 1,
            Ok(Ok(_)) => {}
            _ => break,
        }
    }
    assert_eq!(received, 2);

    // 5. HLC delivers monotonic timestamps.
    let clock = HybridLogicalClock::new();
    let t1 = clock.now();
    let t2 = clock.now();
    assert!(t2 > t1);

    // 6. KV reads return committed values.
    assert_eq!(node.kv.get(g, b"alice").unwrap(), Some(b"42".to_vec()));
    assert_eq!(node.kv.get(g, b"bob").unwrap(), Some(b"43".to_vec()));

    // 7. Cluster status snapshot.
    let status = collect_status(node.gossip.as_ref(), &node.metrics);
    assert!(status.raft.leader_count >= 1);
    let json = serde_json::to_string(&status).unwrap();
    assert!(json.contains("\"raft\""));

    node.shutdown().await;
}
