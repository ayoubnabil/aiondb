//! Regression test for V2-01 — Raft TCP transport authentication.
//!
//! Pre-fix behaviour: an unauthenticated TCP client could ship a
//! forged AppendEntries to `RaftTcpServer` and the registry accepted
//! it (term bump + log append). The audit reproduction lives in
//! `audit/v2/poc/v2_01_raft_tcp_unauth.rs`.
//!
//! Post-fix expectation: every frame is wrapped with an HMAC-SHA256
//! tag computed from a 32-byte shared secret. Frames lacking a tag
//! (or carrying the wrong one) are dropped at the transport layer
//! without ever being forwarded to `MultiRaftRegistry`.

use std::sync::Arc;
use std::time::Duration;

use aiondb_ha::kv_engine::KvEngine;
use aiondb_ha::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
use aiondb_ha::protocol::NodeId;
use aiondb_ha::raft::{AppendEntriesRequest, RaftCommand, RaftEntry};
use aiondb_ha::raft_auth::{encode_authenticated, RaftSharedSecret};
use aiondb_ha::raft_tcp::{RaftTcpServer, RaftWireMessage};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

fn server_secret() -> RaftSharedSecret {
    RaftSharedSecret::new(vec![0xA5u8; 32])
}

#[tokio::test]
async fn v2_01_unauth_frame_is_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap());
    let _kv = KvEngine::new(Arc::clone(&registry));
    let g = MultiRaftGroupId::new(42);
    registry.create_group(g, 2).unwrap();
    let before = registry.group_state(g).expect("group exists").current_term;

    let server = RaftTcpServer::start(
        Arc::clone(&registry),
        "127.0.0.1:0".parse().unwrap(),
        server_secret(),
    )
    .await
    .unwrap();
    let addr = server.local_addr();

    // Attacker uses the pre-fix wire layout (no HMAC tag).
    let mut sock = TcpStream::connect(addr).await.unwrap();
    let forged = AppendEntriesRequest {
        term: 999,
        leader_id: 7777,
        prev_log_index: 0,
        prev_log_term: 0,
        entries: vec![RaftEntry {
            index: 1,
            term: 999,
            command: RaftCommand::AddNode {
                node_id: 666,
                address: "10.66.6.6:9999".to_owned(),
            },
        }],
        leader_commit: 1,
    };
    let body = serde_json::to_vec(&RaftWireMessage::AppendEntries {
        group: g.get(),
        request: forged,
    })
    .unwrap();
    sock.write_all(&(body.len() as u32).to_be_bytes())
        .await
        .unwrap();
    sock.write_all(&body).await.unwrap();
    sock.flush().await.unwrap();
    drop(sock);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let after = registry.group_state(g).expect("group exists").current_term;
    assert_eq!(after, before, "unauth frame must not bump term");

    server.shutdown().await;
}

#[tokio::test]
async fn v2_01_wrong_secret_is_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap());
    let _kv = KvEngine::new(Arc::clone(&registry));
    let g = MultiRaftGroupId::new(42);
    registry.create_group(g, 2).unwrap();
    let before = registry.group_state(g).expect("group exists").current_term;

    let server = RaftTcpServer::start(
        Arc::clone(&registry),
        "127.0.0.1:0".parse().unwrap(),
        server_secret(),
    )
    .await
    .unwrap();
    let addr = server.local_addr();

    let attacker_secret = RaftSharedSecret::new(vec![0xFFu8; 32]);
    let body = serde_json::to_vec(&RaftWireMessage::AppendEntries {
        group: g.get(),
        request: AppendEntriesRequest {
            term: 999,
            leader_id: 7777,
            prev_log_index: 0,
            prev_log_term: 0,
            entries: vec![RaftEntry {
                index: 1,
                term: 999,
                command: RaftCommand::AddNode {
                    node_id: 666,
                    address: "10.66.6.6:9999".to_owned(),
                },
            }],
            leader_commit: 1,
        },
    })
    .unwrap();
    let frame = encode_authenticated(&attacker_secret, &body);

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(&frame).await.unwrap();
    sock.flush().await.unwrap();
    drop(sock);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let after = registry.group_state(g).expect("group exists").current_term;
    assert_eq!(after, before, "frame signed by wrong key must be dropped");

    server.shutdown().await;
}
