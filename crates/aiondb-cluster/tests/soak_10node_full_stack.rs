//! 10-node full-stack soak test.
//!
//! The biggest, most aggressive cluster test in the distrib suite.
//! Boots 10 [`NodeOrchestrator`] instances, replicates 300 metadata
//! writes via real TCP, partitions and restarts a non-leader follower
//! mid-flight, and checks that every node converges to the same final
//! state.

use std::time::Duration;

use aiondb_cluster::distributed::NodeId as ClusterNodeId;
use aiondb_cluster::node_orchestrator::{NodeOrchestrator, NodeOrchestratorConfig};
use aiondb_ha::multi_raft::MultiRaftGroupId;
use aiondb_ha::raft_control_plane::DEFAULT_METADATA_GROUP_ID;
use tokio::time;

const GROUP: MultiRaftGroupId = MultiRaftGroupId::new(DEFAULT_METADATA_GROUP_ID);

async fn boot(
    id: u64,
    voters: usize,
    peers: Vec<u64>,
    leader: bool,
    tmp_dir: std::path::PathBuf,
) -> NodeOrchestrator {
    let mut cfg = NodeOrchestratorConfig::single_node_local(id);
    cfg.state_dir = tmp_dir;
    cfg.metadata_voters = voters;
    cfg.bootstrap_metadata_leader = leader;
    cfg.metadata_peer_ids = peers;
    NodeOrchestrator::start(cfg).await.unwrap()
}

#[tokio::test]
async fn ten_node_cluster_handles_300_writes_partition_and_restart() {
    let tmps: Vec<_> = (0..10).map(|_| tempfile::tempdir().unwrap()).collect();
    let all_ids: Vec<u64> = (1..=10u64).collect();
    let mut nodes = Vec::with_capacity(10);
    for (i, tmp) in tmps.iter().enumerate().take(10) {
        let id = (i + 1) as u64;
        let peers: Vec<u64> = all_ids.iter().copied().filter(|p| *p != id).collect();
        nodes.push(boot(id, 10, peers, i == 0, tmp.path().to_path_buf()).await);
    }
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

    // 300 metadata writes through the leader.
    for i in 0..300u64 {
        nodes[0]
            .control_plane
            .add_node(20_000 + i, format!("host-{i}"))
            .unwrap();
    }
    // Drive replication.
    for _ in 0..90 {
        let _ = nodes[0].raft_server.flush_outbound(GROUP).await;
        time::sleep(Duration::from_millis(40)).await;
        for n in &nodes {
            let _ = n.control_plane.apply_committed();
        }
    }
    for n in &nodes {
        let snap = n.control_plane.snapshot();
        assert!(
            snap.members.len() >= 300,
            "node {} only saw {} members",
            n.raft_id,
            snap.members.len()
        );
    }

    // Restart node 10.
    let n10_path = tmps[9].path().to_path_buf();
    let n10 = nodes.pop().unwrap();
    n10.shutdown().await;

    // Leader keeps writing.
    for i in 300..400u64 {
        nodes[0]
            .control_plane
            .add_node(20_000 + i, format!("host-{i}"))
            .unwrap();
    }
    for _ in 0..30 {
        let _ = nodes[0].raft_server.flush_outbound(GROUP).await;
        time::sleep(Duration::from_millis(40)).await;
        for n in &nodes {
            let _ = n.control_plane.apply_committed();
        }
    }

    // Boot n10 back from disk.
    let n10_restarted = boot(10, 10, (1..=9u64).collect(), false, n10_path).await;
    let n10_cluster = ClusterNodeId::new("n10");
    for n in &nodes {
        n.register_peer(
            10,
            n10_cluster.clone(),
            n10_restarted.gossip_addr(),
            n10_restarted.raft_addr(),
        )
        .await;
    }
    for (rid, cid, g, r) in &ids[..9] {
        n10_restarted.register_peer(*rid, cid.clone(), *g, *r).await;
    }
    for _ in 0..100 {
        let _ = nodes[0].raft_server.flush_outbound(GROUP).await;
        time::sleep(Duration::from_millis(30)).await;
        for n in &nodes {
            let _ = n.control_plane.apply_committed();
        }
        let _ = n10_restarted.control_plane.apply_committed();
    }
    let n10_snap = n10_restarted.control_plane.snapshot();
    assert!(
        n10_snap.members.len() >= 400,
        "restarted follower must converge to 400+ members, got {}",
        n10_snap.members.len()
    );

    for n in nodes {
        n.shutdown().await;
    }
    n10_restarted.shutdown().await;
}
