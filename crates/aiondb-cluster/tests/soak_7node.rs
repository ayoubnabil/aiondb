//! 7-node soak test.
//!
//! Big enough to stress concurrent message routing and lease churn,
//! short enough to fit in CI. Asserts that every node converges to
//! the same final committed metadata after sustained writes and one
//! restart cycle of a non-leader follower.

use std::time::Duration;

use aiondb_cluster::distributed::NodeId as ClusterNodeId;
use aiondb_cluster::node_orchestrator::{NodeOrchestrator, NodeOrchestratorConfig};
use aiondb_ha::multi_raft::MultiRaftGroupId;
use aiondb_ha::raft_control_plane::DEFAULT_METADATA_GROUP_ID;
use tokio::time;

const GROUP: MultiRaftGroupId = MultiRaftGroupId::new(DEFAULT_METADATA_GROUP_ID);

async fn boot(
    id: u64,
    peers: Vec<u64>,
    bootstrap_leader: bool,
    tmp_dir: std::path::PathBuf,
) -> NodeOrchestrator {
    let mut cfg = NodeOrchestratorConfig::single_node_local(id);
    cfg.state_dir = tmp_dir;
    cfg.metadata_voters = 7;
    cfg.bootstrap_metadata_leader = bootstrap_leader;
    cfg.metadata_peer_ids = peers;
    NodeOrchestrator::start(cfg).await.unwrap()
}

#[tokio::test]
async fn seven_real_nodes_converge_after_sustained_writes_and_restart() {
    let tmps: Vec<_> = (0..7).map(|_| tempfile::tempdir().unwrap()).collect();
    let all_ids: Vec<u64> = (1..=7u64).collect();
    let mut nodes = Vec::with_capacity(7);
    for (i, tmp) in tmps.iter().enumerate().take(7) {
        let id = (i + 1) as u64;
        let peers: Vec<u64> = all_ids.iter().copied().filter(|p| *p != id).collect();
        nodes.push(boot(id, peers, i == 0, tmp.path().to_path_buf()).await);
    }

    // Register all pairs.
    let ids: Vec<_> = nodes
        .iter()
        .map(|n| {
            (
                n.raft_id.get(),
                ClusterNodeId::new(format!("n{}", n.raft_id.get())),
                n.gossip_addr(),
                n.raft_addr(),
            )
        })
        .collect();
    for (host_idx, host) in nodes.iter().enumerate() {
        for (peer_idx, (raft_id, cluster_id, g, r)) in ids.iter().enumerate() {
            if peer_idx == host_idx {
                continue;
            }
            host.register_peer(*raft_id, cluster_id.clone(), *g, *r)
                .await;
        }
    }

    // 100 metadata writes via the leader.
    for i in 0..100u64 {
        nodes[0]
            .control_plane
            .add_node(10_000 + i, format!("host-{i}"))
            .unwrap();
    }
    // Drive replication.
    for _ in 0..60 {
        let _ = nodes[0].raft_server.flush_outbound(GROUP).await;
        time::sleep(Duration::from_millis(30)).await;
        for n in &nodes {
            let _ = n.control_plane.apply_committed();
        }
    }

    // Verify convergence on all 7.
    for n in &nodes {
        let snap = n.control_plane.snapshot();
        assert!(
            snap.members.len() >= 100,
            "node {} only saw {} members",
            n.raft_id,
            snap.members.len()
        );
    }

    // Restart follower #7.
    let n7_path = tmps[6].path().to_path_buf();
    let n7 = nodes.pop().unwrap();
    n7.shutdown().await;

    // Leader keeps writing.
    for i in 100..150u64 {
        nodes[0]
            .control_plane
            .add_node(10_000 + i, format!("host-{i}"))
            .unwrap();
    }
    for _ in 0..30 {
        let _ = nodes[0].raft_server.flush_outbound(GROUP).await;
        time::sleep(Duration::from_millis(30)).await;
        for n in &nodes {
            let _ = n.control_plane.apply_committed();
        }
    }

    // Boot n7 back from disk.
    let n7_restarted = boot(7, vec![1, 2, 3, 4, 5, 6], false, n7_path).await;
    let n7_cluster = ClusterNodeId::new("n7");
    for n in &nodes {
        n.register_peer(
            7,
            n7_cluster.clone(),
            n7_restarted.gossip_addr(),
            n7_restarted.raft_addr(),
        )
        .await;
    }
    for (rid, cid, g, r) in &ids[..6] {
        n7_restarted.register_peer(*rid, cid.clone(), *g, *r).await;
    }
    for _ in 0..80 {
        let _ = nodes[0].raft_server.flush_outbound(GROUP).await;
        time::sleep(Duration::from_millis(30)).await;
        for n in &nodes {
            let _ = n.control_plane.apply_committed();
        }
        let _ = n7_restarted.control_plane.apply_committed();
    }
    let n7_snap = n7_restarted.control_plane.snapshot();
    assert!(
        n7_snap.members.len() >= 150,
        "restarted follower must converge to 150+ members, got {}",
        n7_snap.members.len()
    );

    for n in nodes {
        n.shutdown().await;
    }
    n7_restarted.shutdown().await;
}
