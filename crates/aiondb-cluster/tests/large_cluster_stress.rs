//! 5-node large cluster stress test.
//!
//! Boots a fully-wired 5-node cluster on `localhost`, drives 200
//! metadata writes through the leader, and verifies that every
//! follower converges to the same final state. Includes one
//! follower restart cycle to prove durability + recovery.

use std::time::Duration;

use aiondb_cluster::distributed::NodeId as ClusterNodeId;
use aiondb_cluster::node_orchestrator::{NodeOrchestrator, NodeOrchestratorConfig};
use aiondb_ha::multi_raft::MultiRaftGroupId;
use aiondb_ha::raft_control_plane::DEFAULT_METADATA_GROUP_ID;
use tokio::time;

const GROUP: MultiRaftGroupId = MultiRaftGroupId::new(DEFAULT_METADATA_GROUP_ID);

async fn boot_node(
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
async fn five_real_nodes_handle_200_writes_and_restart() {
    let tmps: Vec<_> = (0..5).map(|_| tempfile::tempdir().unwrap()).collect();
    let all_ids: Vec<u64> = (1..=5u64).collect();

    let mut nodes = Vec::with_capacity(5);
    for (i, tmp) in tmps.iter().enumerate().take(5) {
        let id = (i + 1) as u64;
        let peers: Vec<u64> = all_ids.iter().copied().filter(|p| *p != id).collect();
        nodes.push(boot_node(id, 5, peers, i == 0, tmp.path().to_path_buf()).await);
    }

    // Register every pair in gossip + raft.
    let ids_and_addrs: Vec<_> = nodes
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
        for (peer_idx, (raft_id, cluster_id, g_addr, r_addr)) in ids_and_addrs.iter().enumerate() {
            if peer_idx == host_idx {
                continue;
            }
            host.register_peer(*raft_id, cluster_id.clone(), *g_addr, *r_addr)
                .await;
        }
    }

    // 200 metadata writes via the leader (nodes[0]).
    for i in 0..200u64 {
        nodes[0]
            .control_plane
            .add_node(1000 + i, format!("host-{i}"))
            .unwrap();
    }
    // Pump replication.
    for _ in 0..60 {
        let _ = nodes[0].raft_server.flush_outbound(GROUP).await;
        time::sleep(Duration::from_millis(40)).await;
        for n in &nodes {
            let _ = n.control_plane.apply_committed();
        }
    }

    // All 5 should see exactly 200 added members.
    for n in &nodes {
        let snap = n.control_plane.snapshot();
        assert_eq!(
            snap.members.len(),
            200,
            "node {} expected 200 members, got {}",
            n.raft_id,
            snap.members.len()
        );
    }

    // Restart follower #5.
    let n5_path = tmps[4].path().to_path_buf();
    let n5 = nodes.pop().unwrap();
    let n5_raft_addr_old = n5.raft_addr();
    let _ = n5_raft_addr_old;
    n5.shutdown().await;

    // Boot it back as follower from the same state dir.
    let n5_restarted = boot_node(5, 5, vec![1, 2, 3, 4], false, n5_path).await;
    // Re-register peers between everyone and the restarted node.
    let n5_cluster = ClusterNodeId::new("n5");
    for n in &nodes {
        n.register_peer(
            5,
            n5_cluster.clone(),
            n5_restarted.gossip_addr(),
            n5_restarted.raft_addr(),
        )
        .await;
    }
    let other_addrs: Vec<_> = ids_and_addrs[..4].to_vec();
    for (rid, cid, g, r) in other_addrs {
        n5_restarted.register_peer(rid, cid, g, r).await;
    }

    // Drive replication so n5 catches up via on-disk + incoming AppendEntries.
    for _ in 0..60 {
        let _ = nodes[0].raft_server.flush_outbound(GROUP).await;
        time::sleep(Duration::from_millis(40)).await;
        for n in &nodes {
            let _ = n.control_plane.apply_committed();
        }
        let _ = n5_restarted.control_plane.apply_committed();
    }
    let n5_snap = n5_restarted.control_plane.snapshot();
    assert!(
        n5_snap.members.len() >= 200,
        "restarted follower must converge: got {}",
        n5_snap.members.len()
    );

    for n in nodes {
        n.shutdown().await;
    }
    n5_restarted.shutdown().await;
}
