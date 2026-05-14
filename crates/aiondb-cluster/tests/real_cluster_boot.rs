//! Real cluster boot integration test.
//!
//! Spins up 3 [`NodeOrchestrator`] instances bound to OS-assigned
//! localhost ports, has them register each other in gossip + raft,
//! and verifies the full distributed stack works end-to-end with
//! actual TCP sockets.

use std::time::Duration;

use aiondb_cluster::distributed::NodeId as ClusterNodeId;
use aiondb_cluster::node_orchestrator::{NodeOrchestrator, NodeOrchestratorConfig};
use aiondb_ha::multi_raft::MultiRaftGroupId;
use aiondb_ha::raft_control_plane::DEFAULT_METADATA_GROUP_ID;
use tokio::time;

async fn boot_node(
    id: u64,
    bootstrap_leader: bool,
    peer_ids: Vec<u64>,
) -> (tempfile::TempDir, NodeOrchestrator) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = NodeOrchestratorConfig::single_node_local(id);
    cfg.state_dir = tmp.path().to_path_buf();
    cfg.metadata_voters = 3;
    cfg.bootstrap_metadata_leader = bootstrap_leader;
    cfg.metadata_peer_ids = peer_ids;
    let node = NodeOrchestrator::start(cfg).await.unwrap();
    (tmp, node)
}

#[tokio::test]
async fn three_real_orchestrators_replicate_metadata() {
    let (_t1, leader) = boot_node(1, true, vec![2, 3]).await;
    let (_t2, follower2) = boot_node(2, false, vec![]).await;
    let (_t3, follower3) = boot_node(3, false, vec![]).await;

    // Cross-register peers in both gossip and raft.
    let ids_and_addrs = [
        (
            1u64,
            ClusterNodeId::new("n1"),
            leader.gossip_addr(),
            leader.raft_addr(),
        ),
        (
            2u64,
            ClusterNodeId::new("n2"),
            follower2.gossip_addr(),
            follower2.raft_addr(),
        ),
        (
            3u64,
            ClusterNodeId::new("n3"),
            follower3.gossip_addr(),
            follower3.raft_addr(),
        ),
    ];
    for (host_idx, host) in [&leader, &follower2, &follower3].iter().enumerate() {
        for (peer_idx, (raft_id, cluster_id, g_addr, r_addr)) in ids_and_addrs.iter().enumerate() {
            if peer_idx == host_idx {
                continue;
            }
            host.register_peer(*raft_id, cluster_id.clone(), *g_addr, *r_addr)
                .await;
        }
    }

    // Leader writes metadata.
    leader.control_plane.add_node(100, "host-100").unwrap();
    leader.control_plane.assign_shard(7, 0, 100).unwrap();

    // Drive raft over TCP a few times.
    for _ in 0..30 {
        let _ = leader
            .raft_server
            .flush_outbound(MultiRaftGroupId::new(DEFAULT_METADATA_GROUP_ID))
            .await;
        time::sleep(Duration::from_millis(40)).await;
        let _ = follower2.control_plane.apply_committed();
        let _ = follower3.control_plane.apply_committed();
    }
    let _ = leader.control_plane.apply_committed();

    // Every node must observe the leader's metadata.
    for node in [&leader, &follower2, &follower3] {
        let snap = node.control_plane.snapshot();
        let has_node = snap.members.iter().any(|m| m.node_id == 100);
        assert!(
            has_node,
            "node {} did not converge to leader metadata: {:?}",
            node.raft_id, snap
        );
        let has_assignment = snap
            .assignments
            .iter()
            .any(|a| a.table_id == 7 && a.shard_id == 0 && a.node_id == 100);
        assert!(
            has_assignment,
            "node {} missing shard assignment: {:?}",
            node.raft_id, snap
        );
    }

    leader.shutdown().await;
    follower2.shutdown().await;
    follower3.shutdown().await;
}

#[tokio::test]
async fn metrics_endpoint_reflects_replication_progress() {
    let (_t, node) = boot_node(1, true, vec![]).await;
    // Bootstrap leader with 1 voter -- this writes via the metadata
    // raft group's commit pipeline.
    node.control_plane.set_config("region", "eu").unwrap();
    let addr = node.metrics_addr().unwrap();
    let body = {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match tokio::time::timeout(Duration::from_millis(200), stream.read(&mut buf)).await {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(n)) => response.extend_from_slice(&buf[..n]),
                Ok(Err(_)) => break,
            }
        }
        String::from_utf8_lossy(&response).into_owned()
    };
    assert!(body.contains("aiondb_raft_commit_index"));
    assert!(body.contains("aiondb_cluster_total_groups"));
    node.shutdown().await;
}
