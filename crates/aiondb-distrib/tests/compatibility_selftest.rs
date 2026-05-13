//! Compatibility self-test.
//!
//! Forces every distrib sub-crate's most-used primitives to be
//! instantiated through the umbrella crate. Catches dependency
//! breakage that surfaces only when multiple crates compile
//! together.

use std::sync::Arc;
use std::time::Duration;

use aiondb_distrib::prelude::*;

#[tokio::test]
async fn compatibility_smoke_test_runs_full_stack() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = NodeOrchestratorConfig::single_node_local(1);
    cfg.state_dir = tmp.path().to_path_buf();
    let node = NodeOrchestrator::start(cfg).await.unwrap();

    // Control plane.
    node.control_plane.add_node(42, "host-42").unwrap();

    // User group + KV.
    let g = MultiRaftGroupId::new(99);
    node.raft_registry.create_group(g, 1).unwrap();
    node.raft_registry.become_leader(g, &[]).unwrap();
    node.kv.put(g, b"k".to_vec(), b"v".to_vec()).unwrap();

    // HLC.
    let clock = HybridLogicalClock::new();
    let _ = clock.now();

    // Lease registry + closed timestamp + change feed.
    let leases = LeaseRegistry::new();
    let _ = leases.snapshot();
    let tracker = ClosedTimestampTracker::new();
    let _ = tracker.snapshot();
    let bus = ChangefeedBus::new(ChangefeedConfig::default());
    let _ = bus.subscribe(ChangefeedFilter::all_tables());

    // Distributed records + 2PC.
    let registry = DistributedTxnRegistry::new();
    assert_eq!(registry.len(), 0);

    // Admission control.
    let _ = TenantThrottle::new(
        aiondb_distrib::admission::tenant_throttle::TenantThrottleConfig::default(),
    );

    // Just make sure shutdown actually runs.
    let _ = Duration::from_millis(10);
    let _ = Arc::new(());
    node.shutdown().await;
}
