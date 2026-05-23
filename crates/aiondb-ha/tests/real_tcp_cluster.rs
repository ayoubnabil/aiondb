//! Real 3-node TCP cluster integration test.
//!
//! Boots three separate `RaftTcpServer` instances on `localhost` with
//! OS-assigned ports, has them register each other, and verifies that
//! writes proposed to the leader are replicated to both followers
//! over actual TCP sockets.
//!
//! This is the "battle test" : if any of the wiring between the Raft
//! state machine, the multi-raft registry and the TCP transport is
//! broken, this test will surface it.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use aiondb_ha::kv_engine::KvEngine;
use aiondb_ha::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
use aiondb_ha::protocol::NodeId;
use aiondb_ha::raft_auth::{RaftSharedSecret, MIN_RAFT_SHARED_SECRET_BYTES};
use aiondb_ha::raft_tcp::RaftTcpServer;
use tokio::time;

struct RealNode {
    raft_id: NodeId,
    server: RaftTcpServer,
    registry: Arc<MultiRaftRegistry>,
    engine: KvEngine,
    tempdir: tempfile::TempDir,
}

fn test_secret() -> RaftSharedSecret {
    RaftSharedSecret::new(vec![0x42; MIN_RAFT_SHARED_SECRET_BYTES])
}

async fn boot_node(id: u64) -> RealNode {
    let tmp = tempfile::tempdir().unwrap();
    let raft_id = NodeId::new(id);
    let registry = Arc::new(MultiRaftRegistry::new(raft_id, tmp.path()).unwrap());
    let engine = KvEngine::new(Arc::clone(&registry));
    let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let server = RaftTcpServer::start(Arc::clone(&registry), bind, test_secret())
        .await
        .unwrap();
    RealNode {
        raft_id,
        server,
        registry,
        engine,
        tempdir: tmp,
    }
}

#[tokio::test]
async fn three_node_tcp_cluster_replicates_writes() {
    let n1 = boot_node(1).await;
    let n2 = boot_node(2).await;
    let n3 = boot_node(3).await;

    // Cross-register peer addresses.
    n1.server.register_peer(2, n2.server.local_addr()).await;
    n1.server.register_peer(3, n3.server.local_addr()).await;
    n2.server.register_peer(1, n1.server.local_addr()).await;
    n2.server.register_peer(3, n3.server.local_addr()).await;
    n3.server.register_peer(1, n1.server.local_addr()).await;
    n3.server.register_peer(2, n2.server.local_addr()).await;

    let g = MultiRaftGroupId::new(7);
    for n in [&n1, &n2, &n3] {
        n.registry.create_group(g, 3).unwrap();
    }
    // Make n1 the leader.
    n1.registry.become_leader(g, &[2, 3]).unwrap();

    // Propose 25 writes through the leader.
    for i in 0..25u8 {
        n1.engine
            .put(
                g,
                format!("key-{i}").into_bytes(),
                format!("val-{i}").into_bytes(),
            )
            .unwrap();
    }

    // Drive the TCP flush + follower apply over several rounds.
    for _ in 0..30 {
        n1.server.flush_outbound(g).await.unwrap();
        time::sleep(Duration::from_millis(30)).await;
        let _ = n2.engine.apply_committed(g);
        let _ = n3.engine.apply_committed(g);
    }

    // Both followers must hold every value.
    for n in [&n2, &n3] {
        let snap = n.engine.snapshot(g);
        assert_eq!(
            snap.len(),
            25,
            "follower {} missing values: {snap:?}",
            n.raft_id.get()
        );
        for i in 0..25u8 {
            let k = format!("key-{i}").into_bytes();
            let v = format!("val-{i}").into_bytes();
            assert_eq!(
                snap.get(&k),
                Some(&v),
                "follower {} key-{i}",
                n.raft_id.get()
            );
        }
    }

    n1.server.shutdown().await;
    n2.server.shutdown().await;
    n3.server.shutdown().await;
}

#[tokio::test]
async fn cluster_recovers_after_follower_restart() {
    let n1 = boot_node(1).await;
    let n2 = boot_node(2).await;

    n1.server.register_peer(2, n2.server.local_addr()).await;
    n2.server.register_peer(1, n1.server.local_addr()).await;

    let g = MultiRaftGroupId::new(11);
    for n in [&n1, &n2] {
        n.registry.create_group(g, 2).unwrap();
    }
    n1.registry.become_leader(g, &[2]).unwrap();

    // Phase 1 : write a few values while both nodes are up.
    for i in 0..5u8 {
        n1.engine.put(g, vec![i], vec![i, 0]).unwrap();
    }
    for _ in 0..20 {
        n1.server.flush_outbound(g).await.unwrap();
        time::sleep(Duration::from_millis(20)).await;
        let _ = n2.engine.apply_committed(g);
    }
    let pre_restart = n2.engine.snapshot(g);
    assert_eq!(pre_restart.len(), 5);

    // Phase 2 : shut down follower, write more, restart follower.
    let n2_path = n2.tempdir.path().to_path_buf();
    n2.server.shutdown().await;

    for i in 5..10u8 {
        n1.engine.put(g, vec![i], vec![i, 0]).unwrap();
    }
    // Without a follower, single-voter quorum is impossible; this is
    // by design. We just verify the leader's local log still grows.
    let leader_local = n1.engine.snapshot(g);
    assert!(leader_local.len() >= 5);

    // Phase 3 : restart node 2 from on-disk state.
    let n2_registry = Arc::new(MultiRaftRegistry::new(NodeId::new(2), &n2_path).unwrap());
    n2_registry.open_group(g, 2).unwrap();
    let n2_engine = KvEngine::new(Arc::clone(&n2_registry));
    let n2_server = RaftTcpServer::start(
        Arc::clone(&n2_registry),
        "127.0.0.1:0".parse().unwrap(),
        test_secret(),
    )
    .await
    .unwrap();
    // Re-register both directions with the new follower address.
    n1.server.unregister_peer(2).await;
    n1.server.register_peer(2, n2_server.local_addr()).await;
    n2_server.register_peer(1, n1.server.local_addr()).await;

    // Re-apply any persisted entries on node 2 before catching up.
    let _ = n2_engine.apply_committed(g);

    // Heal-and-converge.
    for _ in 0..50 {
        n1.server.flush_outbound(g).await.unwrap();
        time::sleep(Duration::from_millis(20)).await;
        let _ = n2_engine.apply_committed(g);
    }

    let post_restart = n2_engine.snapshot(g);
    assert!(
        post_restart.len() >= 5,
        "restarted follower should retain pre-restart state: {post_restart:?}"
    );

    n1.server.shutdown().await;
    n2_server.shutdown().await;
}
