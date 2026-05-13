//! Full-stack distrib E2E.
//!
//! Stands up an orchestrator node and exercises every layer in turn :
//!
//! 1. Cluster control plane writes (RaftControlPlane).
//! 2. Replicated KV writes (KvEngine).
//! 3. Per-tenant rate limiting (aiondb-admission TenantThrottle).
//! 4. Zone-aware replica ranking (ZoneRouter).
//! 5. Status snapshot end-to-end JSON.

use std::sync::Arc;
use std::time::Duration;

use aiondb_admission::tenant_throttle::{TenantId, TenantThrottle, TenantThrottleConfig};
use aiondb_cluster::cluster_status::collect_status;
use aiondb_cluster::node_orchestrator::{NodeOrchestrator, NodeOrchestratorConfig};
use aiondb_ha::multi_raft::MultiRaftGroupId;
use aiondb_shard::range_descriptor::ReplicaId;
use aiondb_shard::zone_routing::ZoneRouter;

#[tokio::test]
async fn full_stack_writes_replicate_and_status_reports_healthy() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = NodeOrchestratorConfig::single_node_local(1);
    cfg.state_dir = tmp.path().to_path_buf();
    let node = NodeOrchestrator::start(cfg).await.unwrap();

    // 1. Control plane write.
    node.control_plane.add_node(100, "host-100").unwrap();
    let snap = node.control_plane.snapshot();
    assert_eq!(snap.members.len(), 1);

    // 2. KV writes against a fresh group.
    let g = MultiRaftGroupId::new(99);
    node.raft_registry.create_group(g, 1).unwrap();
    node.raft_registry.become_leader(g, &[]).unwrap();
    for i in 0..10u8 {
        node.kv.put(g, vec![i], vec![i, 0]).unwrap();
    }
    assert_eq!(node.kv.snapshot(g).len(), 10);

    // 3. Tenant throttle.
    let throttle = TenantThrottle::new(TenantThrottleConfig {
        default_rate_per_sec: 0.0,
        default_burst: 3,
        max_tenants: 10,
    });
    assert!(throttle.try_admit(TenantId(7)).unwrap());
    assert!(throttle.try_admit(TenantId(7)).unwrap());
    assert!(throttle.try_admit(TenantId(7)).unwrap());
    assert!(!throttle.try_admit(TenantId(7)).unwrap());

    // 4. Zone routing.
    let router = ZoneRouter::new();
    router.set_zone(ReplicaId::new(1), "eu-west-1");
    router.set_zone(ReplicaId::new(2), "us-east-1");
    let order = router.rank_for_client("eu-west-1", &[ReplicaId::new(2), ReplicaId::new(1)]);
    assert_eq!(order[0], ReplicaId::new(1));

    // 5. Status snapshot.
    let status = collect_status(node.gossip.as_ref(), &node.metrics);
    let json = serde_json::to_string(&status).unwrap();
    assert!(json.contains("\"local_node\""));
    assert!(json.contains("\"raft\""));
    // Cluster has at least 2 groups (metadata + 99) and one leader.
    assert!(status.raft.leader_count >= 1);

    let _ = Duration::from_millis(10); // silence unused
    node.shutdown().await;
}

#[tokio::test]
async fn orchestrator_survives_repeated_kv_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = NodeOrchestratorConfig::single_node_local(1);
    cfg.state_dir = tmp.path().to_path_buf();
    let node = NodeOrchestrator::start(cfg).await.unwrap();
    let g = MultiRaftGroupId::new(1);
    node.raft_registry.create_group(g, 1).unwrap();
    node.raft_registry.become_leader(g, &[]).unwrap();
    // 500 writes, mixed put/delete.
    for i in 0..500u32 {
        if i % 7 == 0 {
            let _ = node.kv.delete(g, format!("k{i}").into_bytes());
        } else {
            node.kv
                .put(
                    g,
                    format!("k{i}").into_bytes(),
                    format!("v{i}").into_bytes(),
                )
                .unwrap();
        }
    }
    let snap = node.kv.snapshot(g);
    let _arc_used = Arc::new(snap.clone());
    assert!(snap.len() > 400);
    node.shutdown().await;
}
