//! Real-TCP heartbeat integration test.
//!
//! Spins up 2 RaftTcpServers; the leader generates pulses; the
//! follower receives heartbeats and stays caught up.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use aiondb_ha::kv_engine::KvEngine;
use aiondb_ha::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
use aiondb_ha::protocol::NodeId;
use aiondb_ha::pulse_generator::PulseGenerator;
use aiondb_ha::raft_auth::{RaftSharedSecret, MIN_RAFT_SHARED_SECRET_BYTES};
use aiondb_ha::raft_tcp::RaftTcpServer;

fn test_secret() -> RaftSharedSecret {
    RaftSharedSecret::new(vec![0x42; MIN_RAFT_SHARED_SECRET_BYTES])
}

#[tokio::test]
async fn pulse_generator_advances_log_over_tcp() {
    let tmp_leader = tempfile::tempdir().unwrap();
    let tmp_follower = tempfile::tempdir().unwrap();
    let leader_reg = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp_leader.path()).unwrap());
    let follower_reg =
        Arc::new(MultiRaftRegistry::new(NodeId::new(2), tmp_follower.path()).unwrap());
    let g = MultiRaftGroupId::new(7);
    leader_reg.create_group(g, 2).unwrap();
    follower_reg.create_group(g, 2).unwrap();
    leader_reg.become_leader(g, &[2]).unwrap();
    let leader_engine = KvEngine::new(Arc::clone(&leader_reg));
    let follower_engine = KvEngine::new(Arc::clone(&follower_reg));

    let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let leader_server = RaftTcpServer::start(Arc::clone(&leader_reg), bind, test_secret())
        .await
        .unwrap();
    let follower_server = RaftTcpServer::start(Arc::clone(&follower_reg), bind, test_secret())
        .await
        .unwrap();
    leader_server
        .register_peer(2, follower_server.local_addr())
        .await;
    follower_server
        .register_peer(1, leader_server.local_addr())
        .await;

    let pulse = PulseGenerator::spawn(Arc::clone(&leader_reg), g, Duration::from_millis(20));
    leader_engine
        .put(g, b"seed".to_vec(), b"value".to_vec())
        .unwrap();
    for _ in 0..10 {
        leader_server.flush_outbound(g).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let _ = follower_engine.apply_committed(g);
    }
    pulse.shutdown().await;

    let snap = follower_engine.snapshot(g);
    assert_eq!(snap.get(b"seed".as_slice()), Some(&b"value".to_vec()));

    leader_server.shutdown().await;
    follower_server.shutdown().await;
}
