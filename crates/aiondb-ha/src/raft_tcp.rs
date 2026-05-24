//! TCP transport for [`MultiRaftRegistry`] AppendEntries RPC.
//!
//! Production deployments need real network delivery of Raft
//! messages. This module wires the existing single-process
//! `MultiRaftRegistry` to a TCP server + a per-peer client. Each
//! envelope carries the `group_id` so multiple Raft groups can
//! multiplex over the same connection.
//!
//! # Wire format
//!
//! 4-byte big-endian length prefix, then a JSON-encoded
//! [`RaftWireMessage`]. JSON is sufficient for prod throughput at the
//! scale of metadata Raft; tests verify round-trip semantics.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, error, trace, warn};

use crate::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
use crate::raft::{AppendEntriesRequest, AppendEntriesResponse};
use crate::raft_auth::{
    decode_authenticated, encode_authenticated, RaftSharedSecret, MIN_RAFT_SHARED_SECRET_BYTES,
};

/// Maximum on-the-wire frame size. Generous default for a single
/// AppendEntries batch.
pub const MAX_RAFT_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// Envelope multiplexing AppendEntries / Response over the same TCP
/// connection. Indexed by `group` so per-range raft groups share the
/// transport.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RaftWireMessage {
    AppendEntries {
        group: u64,
        request: AppendEntriesRequest,
    },
    AppendEntriesResponse {
        group: u64,
        response: AppendEntriesResponse,
    },
}

/// Per-peer address book. Maps `raft_node_id -> tcp_addr` so the
/// server can deliver outbound messages.
pub type PeerAddressBook = HashMap<u64, SocketAddr>;

/// TCP Raft server. Binds a listener, accepts AppendEntries from
/// peers, routes them into the local `MultiRaftRegistry`, and ships
/// responses back over a short-lived TCP connection.
pub struct RaftTcpServer {
    registry: Arc<MultiRaftRegistry>,
    peers: Arc<Mutex<PeerAddressBook>>,
    secret: RaftSharedSecret,
    listener_handle: JoinHandle<()>,
    shutdown_tx: watch::Sender<bool>,
    local_addr: SocketAddr,
}

impl RaftTcpServer {
    pub async fn start(
        registry: Arc<MultiRaftRegistry>,
        bind_addr: SocketAddr,
        secret: RaftSharedSecret,
    ) -> io::Result<Self> {
        if !secret.is_strong_enough() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "raft tcp shared secret must be at least {MIN_RAFT_SHARED_SECRET_BYTES} bytes"
                ),
            ));
        }
        let listener = TcpListener::bind(bind_addr).await?;
        let local_addr = listener.local_addr()?;
        let peers: Arc<Mutex<PeerAddressBook>> = Arc::new(Mutex::new(HashMap::new()));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let listener_handle = {
            let registry = Arc::clone(&registry);
            let peers = Arc::clone(&peers);
            let secret = secret.clone();
            let mut shutdown_rx = shutdown_rx.clone();
            tokio::spawn(async move {
                run_listener(registry, peers, listener, secret, &mut shutdown_rx).await;
            })
        };

        Ok(Self {
            registry,
            peers,
            secret,
            listener_handle,
            shutdown_tx,
            local_addr,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub async fn register_peer(&self, node_id: u64, addr: SocketAddr) {
        self.peers.lock().await.insert(node_id, addr);
    }

    pub async fn unregister_peer(&self, node_id: u64) {
        self.peers.lock().await.remove(&node_id);
    }

    /// Send every locally-buffered AppendEntries for `group` to its
    /// followers over TCP. Caller must call this on every tick.
    pub async fn flush_outbound(&self, group: MultiRaftGroupId) -> io::Result<()> {
        let reqs = self
            .registry
            .build_append_entries_requests(group)
            .map_err(|e| io::Error::other(e.to_string()))?;
        if reqs.is_empty() {
            return Ok(());
        }
        let peers = self.peers.lock().await.clone();
        for (target_id, req) in reqs {
            let Some(addr) = peers.get(&target_id).copied() else {
                continue;
            };
            let msg = RaftWireMessage::AppendEntries {
                group: group.get(),
                request: req,
            };
            let registry = Arc::clone(&self.registry);
            let secret = self.secret.clone();
            tokio::spawn(async move {
                if let Err(err) =
                    send_and_apply_response(addr, &msg, &registry, group, &secret).await
                {
                    trace!(target = target_id, addr = %addr, error = %err, "raft tcp send failed");
                }
            });
        }
        Ok(())
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.listener_handle.await;
    }
}

async fn run_listener(
    registry: Arc<MultiRaftRegistry>,
    _peers: Arc<Mutex<PeerAddressBook>>,
    listener: TcpListener,
    secret: RaftSharedSecret,
    shutdown_rx: &mut watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    return;
                }
            }
            accept = listener.accept() => match accept {
                Ok((mut stream, peer)) => {
                    let registry = Arc::clone(&registry);
                    let secret = secret.clone();
                    tokio::spawn(async move {
                        match read_frame(&mut stream, &secret).await {
                            Ok(Some(RaftWireMessage::AppendEntries { group, request })) => {
                                match registry.handle_append_entries(
                                    MultiRaftGroupId::new(group),
                                    &request,
                                ) {
                                    Ok(response) => {
                                        let reply = RaftWireMessage::AppendEntriesResponse {
                                            group,
                                            response,
                                        };
                                        if let Err(err) = write_frame(&mut stream, &reply, &secret).await {
                                            debug!(error = %err, "raft reply write failed");
                                        }
                                    }
                                    Err(err) => warn!(error = %err, "handle_append_entries failed"),
                                }
                            }
                            Ok(Some(RaftWireMessage::AppendEntriesResponse { group, response })) => {
                                let _ = registry
                                    .handle_append_entries_response(MultiRaftGroupId::new(group), &response);
                            }
                            Ok(None) => {} // peer closed
                            Err(err) => {
                                debug!(peer = %peer, error = %err, "raft inbound read failed");
                            }
                        }
                    });
                }
                Err(err) => {
                    error!(error = %err, "raft tcp accept error");
                    time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}

async fn send_and_apply_response(
    addr: SocketAddr,
    msg: &RaftWireMessage,
    registry: &Arc<MultiRaftRegistry>,
    group: MultiRaftGroupId,
    secret: &RaftSharedSecret,
) -> io::Result<()> {
    let mut stream = time::timeout(Duration::from_millis(500), TcpStream::connect(addr))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "raft connect timeout"))??;
    write_frame(&mut stream, msg, secret).await?;
    stream.flush().await?;
    // Read the response back on the same connection so we can apply it
    // locally without setting up a separate inbound path.
    match read_frame(&mut stream, secret).await? {
        Some(RaftWireMessage::AppendEntriesResponse { group: g, response }) => {
            if g == group.get() {
                let _ = registry.handle_append_entries_response(group, &response);
            }
        }
        _ => {} // unexpected, treat as no-op
    }
    Ok(())
}

async fn read_frame(
    stream: &mut TcpStream,
    secret: &RaftSharedSecret,
) -> io::Result<Option<RaftWireMessage>> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    let len = u32::from_be_bytes(len_buf);
    if len == 0 || len > MAX_RAFT_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("raft frame {len} bytes exceeds MAX_RAFT_FRAME_BYTES"),
        ));
    }
    let mut frame = Vec::with_capacity(4 + len as usize + 32);
    frame.extend_from_slice(&len_buf);
    let mut payload_and_tag = vec![0u8; len as usize + 32];
    stream.read_exact(&mut payload_and_tag).await?;
    frame.extend_from_slice(&payload_and_tag);
    let payload = decode_authenticated(secret, &frame).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "raft frame authentication failed",
        )
    })?;
    let msg = serde_json::from_slice::<RaftWireMessage>(&payload)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("raft decode: {e}")))?;
    Ok(Some(msg))
}

async fn write_frame(
    stream: &mut TcpStream,
    msg: &RaftWireMessage,
    secret: &RaftSharedSecret,
) -> io::Result<()> {
    let payload = serde_json::to_vec(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("raft encode: {e}")))?;
    let len = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "raft frame too large"))?;
    if len > MAX_RAFT_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("raft frame {len} bytes exceeds MAX_RAFT_FRAME_BYTES"),
        ));
    }
    let frame = encode_authenticated(secret, &payload);
    stream.write_all(&frame).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kv_engine::KvEngine;
    use crate::protocol::NodeId;

    fn test_secret() -> RaftSharedSecret {
        RaftSharedSecret::new(vec![0x42; MIN_RAFT_SHARED_SECRET_BYTES])
    }

    async fn make_server(
        id: u64,
    ) -> (
        RaftTcpServer,
        Arc<MultiRaftRegistry>,
        KvEngine,
        tempfile::TempDir,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let registry = Arc::new(MultiRaftRegistry::new(NodeId::new(id), tmp.path()).unwrap());
        let engine = KvEngine::new(Arc::clone(&registry));
        let server = RaftTcpServer::start(
            Arc::clone(&registry),
            SocketAddr::from(([127, 0, 0, 1], 0)),
            test_secret(),
        )
        .await
        .unwrap();
        (server, registry, engine, tmp)
    }

    #[tokio::test]
    async fn start_rejects_short_shared_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap());

        let result = RaftTcpServer::start(
            registry,
            SocketAddr::from(([127, 0, 0, 1], 0)),
            RaftSharedSecret::new(vec![0x42; MIN_RAFT_SHARED_SECRET_BYTES - 1]),
        )
        .await;
        let err = match result {
            Ok(_) => panic!("short secret must be rejected"),
            Err(err) => err,
        };

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn append_entries_round_trips_over_tcp() {
        let (s_leader, reg_leader, kv_leader, _g1) = make_server(1).await;
        let (s_follower, reg_follower, kv_follower, _g2) = make_server(2).await;
        let g = MultiRaftGroupId::new(7);
        reg_leader.create_group(g, 2).unwrap();
        reg_follower.create_group(g, 2).unwrap();
        reg_leader.become_leader(g, &[2]).unwrap();

        s_leader.register_peer(2, s_follower.local_addr()).await;
        s_follower.register_peer(1, s_leader.local_addr()).await;

        // Leader writes via the KV engine.
        kv_leader.put(g, b"k".to_vec(), b"v".to_vec()).unwrap();
        // Flush over TCP a few times to let replication + apply complete.
        for _ in 0..10 {
            s_leader.flush_outbound(g).await.unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
            // Drain follower-applied entries.
            let _ = kv_follower.apply_committed(g);
        }
        let got = kv_follower.get(g, b"k").unwrap();
        assert_eq!(
            got,
            Some(b"v".to_vec()),
            "follower received replicated write"
        );

        s_leader.shutdown().await;
        s_follower.shutdown().await;
    }

    #[tokio::test]
    async fn multiple_groups_multiplex_over_same_listener() {
        let (s_leader, reg_leader, kv_leader, _g1) = make_server(1).await;
        let (s_follower, reg_follower, kv_follower, _g2) = make_server(2).await;
        let g1 = MultiRaftGroupId::new(1);
        let g2 = MultiRaftGroupId::new(2);
        for g in [g1, g2] {
            reg_leader.create_group(g, 2).unwrap();
            reg_follower.create_group(g, 2).unwrap();
            reg_leader.become_leader(g, &[2]).unwrap();
        }

        s_leader.register_peer(2, s_follower.local_addr()).await;
        s_follower.register_peer(1, s_leader.local_addr()).await;

        kv_leader.put(g1, b"a".to_vec(), b"A".to_vec()).unwrap();
        kv_leader.put(g2, b"b".to_vec(), b"B".to_vec()).unwrap();
        for _ in 0..10 {
            s_leader.flush_outbound(g1).await.unwrap();
            s_leader.flush_outbound(g2).await.unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
            let _ = kv_follower.apply_committed(g1);
            let _ = kv_follower.apply_committed(g2);
        }
        assert_eq!(kv_follower.get(g1, b"a").unwrap(), Some(b"A".to_vec()));
        assert_eq!(kv_follower.get(g2, b"b").unwrap(), Some(b"B".to_vec()));

        s_leader.shutdown().await;
        s_follower.shutdown().await;
    }
}
