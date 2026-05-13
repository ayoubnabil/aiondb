//! Minimal HTTP server exposing distrib metrics on `/metrics` and
//! `/metrics/json`.
//!
//! No hyper dep -- we speak HTTP/1.1 directly over `tokio::net::TcpStream`
//! so the binary stays lean. Only GET is supported.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::distrib_metrics::DistribMetrics;

pub struct MetricsServer {
    handle: JoinHandle<()>,
    shutdown_tx: watch::Sender<bool>,
    local_addr: SocketAddr,
}

impl MetricsServer {
    pub async fn start(metrics: Arc<DistribMetrics>, bind: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(bind).await?;
        let local_addr = listener.local_addr()?;
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            return;
                        }
                    }
                    accept = listener.accept() => match accept {
                        Ok((mut stream, peer)) => {
                            let metrics = Arc::clone(&metrics);
                            tokio::spawn(async move {
                                if let Err(err) = serve_one(metrics, &mut stream).await {
                                    debug!(peer = %peer, error = %err, "metrics inbound error");
                                }
                            });
                        }
                        Err(err) => {
                            warn!(error = %err, "metrics accept error");
                        }
                    }
                }
            }
        });
        Ok(Self {
            handle,
            shutdown_tx,
            local_addr,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.handle.await;
    }
}

async fn serve_one(metrics: Arc<DistribMetrics>, stream: &mut TcpStream) -> io::Result<()> {
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }
    let head = std::str::from_utf8(&buf[..n]).unwrap_or("");
    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
    let (body, content_type) = match path.as_str() {
        "/metrics" => (metrics.prometheus(), "text/plain; version=0.0.4"),
        "/metrics/json" => (metrics.json().to_string(), "application/json"),
        _ => ("not found\n".to_string(), "text/plain"),
    };
    let status = if path == "/metrics" || path == "/metrics/json" {
        "200 OK"
    } else {
        "404 Not Found"
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::kv_engine::KvEngine;
    use crate::multi_raft::{MultiRaftGroupId, MultiRaftRegistry};
    use crate::protocol::NodeId;

    async fn boot() -> (tempfile::TempDir, MetricsServer) {
        let tmp = tempfile::tempdir().unwrap();
        let reg = Arc::new(MultiRaftRegistry::new(NodeId::new(1), tmp.path()).unwrap());
        let g = MultiRaftGroupId::new(1);
        reg.create_group(g, 1).unwrap();
        reg.become_leader(g, &[]).unwrap();
        let engine = KvEngine::new(Arc::clone(&reg));
        engine.put(g, b"k".to_vec(), b"v".to_vec()).unwrap();
        let metrics = Arc::new(DistribMetrics::new(Arc::clone(&reg), engine));
        let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = MetricsServer::start(metrics, bind).await.unwrap();
        (tmp, server)
    }

    async fn fetch(addr: SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();
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
    }

    #[tokio::test]
    async fn metrics_endpoint_returns_prometheus_text() {
        let (_t, server) = boot().await;
        let addr = server.local_addr();
        let body = fetch(addr, "/metrics").await;
        assert!(body.contains("HTTP/1.1 200 OK"));
        assert!(body.contains("aiondb_raft_commit_index"));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn metrics_json_endpoint_returns_valid_json() {
        let (_t, server) = boot().await;
        let addr = server.local_addr();
        let body = fetch(addr, "/metrics/json").await;
        assert!(body.contains("HTTP/1.1 200 OK"));
        let body_only = body.split("\r\n\r\n").nth(1).unwrap_or("");
        let parsed: serde_json::Value = serde_json::from_str(body_only).unwrap();
        assert!(parsed.get("groups").is_some());
        assert!(parsed.get("cluster").is_some());
        server.shutdown().await;
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        let (_t, server) = boot().await;
        let addr = server.local_addr();
        let body = fetch(addr, "/foo").await;
        assert!(body.contains("HTTP/1.1 404 Not Found"));
        server.shutdown().await;
    }
}
