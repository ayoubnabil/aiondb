//! TCP transport for [`crate::gossip::GossipNode`].
//!
//! Wraps the pure state machine in a tokio-based runtime that:
//!
//! - Binds a `TcpListener` on the node's gossip address.
//! - Spawns a per-connection reader task that deserialises inbound
//!   [`GossipMessage`]s and feeds them into the node.
//! - Spawns a periodic ticker task that calls `GossipNode::tick`, then
//!   drains the outbox and delivers each envelope to its peer via a
//!   short-lived TCP connection.
//! - Caches a peer address book so messages can be routed by
//!   [`NodeId`].
//!
//! The wire format is the simplest thing that works: a 4-byte
//! big-endian length prefix followed by a JSON-encoded
//! [`GossipMessage`]. JSON is on the wire purely so the format is
//! easy to debug; if profiling shows it as a hot spot, swap in
//! `bincode` -- the framing stays identical.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, error, trace, warn};

use crate::distributed::NodeId;
use crate::gossip::{GossipMessage, GossipNode};

/// Maximum gossip message size on the wire. Protects against malformed
/// peers attempting to allocate gigabytes via the length prefix.
pub const MAX_MESSAGE_BYTES: u32 = 4 * 1024 * 1024;

/// Tokio handle running a [`GossipNode`] over TCP. Drop it to stop the
/// runtime; the listener and ticker shut down once the handle is
/// dropped.
pub struct GossipServer {
    node: Arc<GossipNode>,
    address_book: Arc<Mutex<HashMap<NodeId, SocketAddr>>>,
    listener_handle: JoinHandle<()>,
    ticker_handle: JoinHandle<()>,
    shutdown_tx: watch::Sender<bool>,
    local_addr: SocketAddr,
}

impl GossipServer {
    /// Start a TCP listener on `bind_addr` and a tick loop that drives
    /// gossip every `tick_interval`. Returns the running server.
    ///
    /// # Errors
    /// Returns `io::Error` when the listener fails to bind.
    pub async fn start(
        node: Arc<GossipNode>,
        bind_addr: SocketAddr,
        tick_interval: Duration,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(bind_addr).await?;
        let local_addr = listener.local_addr()?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let address_book = Arc::new(Mutex::new(HashMap::new()));

        let listener_handle = {
            let node = Arc::clone(&node);
            let mut shutdown_rx = shutdown_rx.clone();
            tokio::spawn(async move {
                run_listener(node, listener, &mut shutdown_rx).await;
            })
        };
        let ticker_handle = {
            let node = Arc::clone(&node);
            let address_book = Arc::clone(&address_book);
            let mut shutdown_rx = shutdown_rx.clone();
            tokio::spawn(async move {
                run_ticker(node, address_book, tick_interval, &mut shutdown_rx).await;
            })
        };

        Ok(Self {
            node,
            address_book,
            listener_handle,
            ticker_handle,
            shutdown_tx,
            local_addr,
        })
    }

    /// Address the listener bound to. Useful when `bind_addr` used
    /// port 0 to let the OS pick.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Register a peer's gossip address. The runtime uses this to
    /// resolve [`NodeId`] → `SocketAddr` when delivering outbound
    /// messages. Returns the previously-known address, if any.
    pub async fn register_peer(&self, peer: NodeId, addr: SocketAddr) -> Option<SocketAddr> {
        let mut guard = self.address_book.lock().await;
        guard.insert(peer, addr)
    }

    /// Look up a peer's address.
    pub async fn peer_addr(&self, peer: &NodeId) -> Option<SocketAddr> {
        self.address_book.lock().await.get(peer).copied()
    }

    /// Stop the listener + ticker. Idempotent.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.listener_handle.await;
        let _ = self.ticker_handle.await;
    }

    /// Access the underlying node (for inspection, e.g. tests).
    pub fn node(&self) -> &Arc<GossipNode> {
        &self.node
    }
}

async fn run_listener(
    node: Arc<GossipNode>,
    listener: TcpListener,
    shutdown_rx: &mut watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    debug!("gossip listener shutdown");
                    return;
                }
            }
            accept = listener.accept() => match accept {
                Ok((stream, peer)) => {
                    let node = Arc::clone(&node);
                    tokio::spawn(async move {
                        if let Err(err) = handle_inbound(node, stream).await {
                            warn!(peer = %peer, error = %err, "gossip inbound connection error");
                        }
                    });
                }
                Err(err) => {
                    error!(error = %err, "gossip listener accept error");
                    time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}

async fn handle_inbound(node: Arc<GossipNode>, mut stream: TcpStream) -> io::Result<()> {
    loop {
        match read_frame(&mut stream).await {
            Ok(Some(msg)) => {
                trace!(?msg, "gossip received");
                node.handle_message(msg);
            }
            Ok(None) => return Ok(()), // peer closed
            Err(err) => return Err(err),
        }
    }
}

async fn run_ticker(
    node: Arc<GossipNode>,
    address_book: Arc<Mutex<HashMap<NodeId, SocketAddr>>>,
    interval: Duration,
    shutdown_rx: &mut watch::Receiver<bool>,
) {
    let mut ticker = time::interval(interval);
    ticker.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    debug!("gossip ticker shutdown");
                    return;
                }
            }
            _ = ticker.tick() => {
                node.tick(std::time::Instant::now());
                let envelopes = node.drain_outbox();
                if envelopes.is_empty() {
                    continue;
                }
                let book = address_book.lock().await.clone();
                for envelope in envelopes {
                    let addr = match book.get(&envelope.to) {
                        Some(addr) => *addr,
                        None => {
                            warn!(peer = %envelope.to, "no address known for peer");
                            continue;
                        }
                    };
                    let msg = envelope.message.clone();
                    tokio::spawn(async move {
                        if let Err(err) = send_one(addr, &msg).await {
                            trace!(target_addr = %addr, error = %err, "gossip send failed");
                        }
                    });
                }
            }
        }
    }
}

async fn send_one(addr: SocketAddr, msg: &GossipMessage) -> io::Result<()> {
    let connect = time::timeout(Duration::from_millis(500), TcpStream::connect(addr))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "gossip connect timeout"))??;
    let mut stream = connect;
    write_frame(&mut stream, msg).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_frame(stream: &mut TcpStream) -> io::Result<Option<GossipMessage>> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("gossip frame {len} bytes exceeds MAX_MESSAGE_BYTES"),
        ));
    }
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).await?;
    let msg = serde_json::from_slice::<GossipMessage>(&payload)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("gossip decode: {e}")))?;
    Ok(Some(msg))
}

async fn write_frame(stream: &mut TcpStream, msg: &GossipMessage) -> io::Result<()> {
    let payload = serde_json::to_vec(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("gossip encode: {e}")))?;
    let len = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "gossip frame too large"))?;
    if len > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("gossip frame {len} bytes exceeds MAX_MESSAGE_BYTES"),
        ));
    }
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&payload).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use super::*;
    use crate::gossip::{GossipConfig, GossipNode, MemberState};

    fn node_id(n: u64) -> NodeId {
        NodeId::new(format!("n{n}"))
    }

    fn fast_config() -> GossipConfig {
        GossipConfig {
            protocol_period: Duration::from_millis(20),
            ack_timeout: Duration::from_millis(50),
            suspect_timeout: Duration::from_secs(1),
            indirect_probes: 1,
            piggyback_size: 8,
        }
    }

    async fn start_node(id: NodeId) -> GossipServer {
        let node = Arc::new(GossipNode::new(id, fast_config()));
        let addr = SocketAddr::from(([127, 0, 0, 1], 0));
        GossipServer::start(node, addr, Duration::from_millis(15))
            .await
            .expect("start server")
    }

    #[tokio::test]
    async fn two_nodes_exchange_pings_over_tcp() {
        let a = start_node(node_id(1)).await;
        let b = start_node(node_id(2)).await;
        // Tell each node where the other lives.
        a.register_peer(node_id(2), b.local_addr()).await;
        b.register_peer(node_id(1), a.local_addr()).await;
        // Seed memberships so they probe each other.
        a.node().join(node_id(2), BTreeMap::new());
        b.node().join(node_id(1), BTreeMap::new());
        // Wait long enough for several ticks.
        time::sleep(Duration::from_millis(150)).await;
        let a_view = a.node().members();
        let b_view = b.node().members();
        // Both should consider the other Alive after pings.
        let a_sees_b = a_view
            .iter()
            .find(|m| m.node_id == node_id(2))
            .map(|m| m.state)
            .unwrap_or(MemberState::Dead);
        let b_sees_a = b_view
            .iter()
            .find(|m| m.node_id == node_id(1))
            .map(|m| m.state)
            .unwrap_or(MemberState::Dead);
        assert_eq!(a_sees_b, MemberState::Alive, "A view: {a_view:?}");
        assert_eq!(b_sees_a, MemberState::Alive, "B view: {b_view:?}");
        a.shutdown().await;
        b.shutdown().await;
    }

    #[tokio::test]
    async fn dead_peer_is_detected_when_node_disappears() {
        let a = start_node(node_id(1)).await;
        let b = start_node(node_id(2)).await;
        a.register_peer(node_id(2), b.local_addr()).await;
        b.register_peer(node_id(1), a.local_addr()).await;
        a.node().join(node_id(2), BTreeMap::new());
        b.node().join(node_id(1), BTreeMap::new());
        // Let them learn each other first.
        time::sleep(Duration::from_millis(100)).await;
        // Now kill B.
        b.shutdown().await;
        // After enough ticks, A should mark B Suspect, then Dead.
        time::sleep(Duration::from_millis(2_500)).await;
        let a_view = a.node().members();
        let b_state = a_view
            .iter()
            .find(|m| m.node_id == node_id(2))
            .map(|m| m.state)
            .unwrap_or(MemberState::Alive);
        assert!(
            matches!(b_state, MemberState::Suspect | MemberState::Dead),
            "expected B to be Suspect or Dead, got {b_state:?}; full view: {a_view:?}"
        );
        a.shutdown().await;
    }

    #[tokio::test]
    async fn three_nodes_converge_over_tcp() {
        let a = start_node(node_id(1)).await;
        let b = start_node(node_id(2)).await;
        let c = start_node(node_id(3)).await;
        // A↔B and B↔C, A does not yet know C directly.
        a.register_peer(node_id(2), b.local_addr()).await;
        b.register_peer(node_id(1), a.local_addr()).await;
        b.register_peer(node_id(3), c.local_addr()).await;
        c.register_peer(node_id(2), b.local_addr()).await;
        // Once A learns of C via gossip, we need to know where to send.
        a.register_peer(node_id(3), c.local_addr()).await;
        c.register_peer(node_id(1), a.local_addr()).await;
        a.node().join(node_id(2), BTreeMap::new());
        b.node().join(node_id(1), BTreeMap::new());
        b.node().join(node_id(3), BTreeMap::new());
        c.node().join(node_id(2), BTreeMap::new());
        // Let gossip propagate.
        time::sleep(Duration::from_millis(300)).await;
        let a_view: Vec<String> = a
            .node()
            .members()
            .iter()
            .map(|m| m.node_id.to_string())
            .collect();
        assert!(
            a_view.contains(&"n3".to_owned()),
            "A should learn C via gossip: {a_view:?}"
        );
        a.shutdown().await;
        b.shutdown().await;
        c.shutdown().await;
    }
}
