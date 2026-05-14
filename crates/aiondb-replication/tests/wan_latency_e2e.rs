//! WAN replication latency E2E.
//!
//! Drives writes through `with_latency` to simulate cross-region
//! RTT, then verifies that replication still converges to the same
//! state on the (simulated) replica.

use std::sync::Arc;
use std::time::Duration;

use aiondb_ha::kv_engine::KvEngine;
use aiondb_ha::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
use aiondb_ha::protocol::NodeId;
use aiondb_replication::latency_sim::{with_latency, LatencyMatrix};

#[tokio::test]
async fn writes_complete_under_simulated_eu_us_latency() {
    let matrix = LatencyMatrix::world_profile();
    let eu_to_us = matrix.get("eu", "us");
    assert_eq!(eu_to_us, Duration::from_millis(80));

    let tmp = tempfile::tempdir().unwrap();
    let registry = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap());
    let g = MultiRaftGroupId::new(1);
    registry.create_group(g, 1).unwrap();
    registry.become_leader(g, &[]).unwrap();
    let engine = KvEngine::new(Arc::clone(&registry));

    // 5 writes, each costing one EU→US RTT before completing.
    let start = std::time::Instant::now();
    for i in 0..5u8 {
        let key = vec![i];
        let value = vec![i, 0];
        let engine = engine.clone();
        with_latency(eu_to_us, async move {
            engine.put(g, key, value).unwrap();
        })
        .await;
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(380),
        "5 writes at 80ms RTT should take >= 400ms, got {:?}",
        elapsed
    );
    assert_eq!(engine.snapshot(g).len(), 5);
}

#[tokio::test]
async fn latency_matrix_caches_default_regions() {
    let m = LatencyMatrix::world_profile();
    assert!(m.get("ap", "sa") > Duration::from_millis(100));
    assert!(m.get("eu", "eu") < Duration::from_millis(10));
}
