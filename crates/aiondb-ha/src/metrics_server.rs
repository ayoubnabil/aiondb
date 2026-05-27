//! Minimal HTTP server exposing distrib metrics on `/metrics` and
//! `/metrics/json`.
//!
//! No hyper dep -- we speak HTTP/1.1 directly over `tokio::net::TcpStream`
//! so the binary stays lean. Only GET is supported.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, warn};

use crate::distrib_metrics::DistribMetrics;

const HTTP_REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(2);
const HTTP_REQUEST_BUFFER_BYTES: usize = 4096;

#[derive(Debug, Eq, PartialEq)]
struct RequestLine<'a> {
    method: &'a str,
    path: &'a str,
}

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
    let mut buf = vec![0u8; HTTP_REQUEST_BUFFER_BYTES];
    let n = time::timeout(HTTP_REQUEST_READ_TIMEOUT, stream.read(&mut buf))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "metrics request read timeout"))??;
    if n == 0 {
        return Ok(());
    }
    let response = match parse_request_line(&buf[..n]) {
        Ok(request) if request.method != "GET" => http_response(
            "405 Method Not Allowed",
            "method not allowed\n",
            "text/plain",
        ),
        Ok(request) => match request.path {
            "/metrics" => {
                http_response("200 OK", &metrics.prometheus(), "text/plain; version=0.0.4")
            }
            "/metrics/json" => {
                http_response("200 OK", &metrics.json().to_string(), "application/json")
            }
            _ => http_response("404 Not Found", "not found\n", "text/plain"),
        },
        Err(()) => http_response("400 Bad Request", "bad request\n", "text/plain"),
    };
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

fn parse_request_line(buf: &[u8]) -> Result<RequestLine<'_>, ()> {
    let head = std::str::from_utf8(buf).map_err(|_| ())?;
    let first_line = head
        .split_once('\n')
        .map_or(head, |(line, _)| line)
        .trim_end_matches('\r');
    let mut parts = first_line.split_ascii_whitespace();
    let method = parts.next().ok_or(())?;
    let path = parts.next().ok_or(())?;
    let version = parts.next().ok_or(())?;
    if parts.next().is_some() || !version.starts_with("HTTP/") {
        return Err(());
    }
    Ok(RequestLine { method, path })
}

fn http_response(status: &str, body: &str, content_type: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
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
        fetch_method(addr, "GET", path).await
    }

    async fn fetch_method(addr: SocketAddr, method: &str, path: &str) -> String {
        let req = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        fetch_raw(addr, req.as_bytes()).await
    }

    async fn fetch_raw(addr: SocketAddr, req: &[u8]) -> String {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(req).await.unwrap();
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

    #[tokio::test]
    async fn non_get_method_returns_405() {
        let (_t, server) = boot().await;
        let addr = server.local_addr();
        let body = fetch_method(addr, "POST", "/metrics").await;
        assert!(body.contains("HTTP/1.1 405 Method Not Allowed"));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn malformed_request_line_returns_400() {
        let (_t, server) = boot().await;
        let addr = server.local_addr();
        let body = fetch_raw(addr, b"GET\r\nHost: x\r\n\r\n").await;
        assert!(body.contains("HTTP/1.1 400 Bad Request"));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn invalid_utf8_request_returns_400() {
        let (_t, server) = boot().await;
        let addr = server.local_addr();
        let body = fetch_raw(addr, b"GET /\xFF HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert!(body.contains("HTTP/1.1 400 Bad Request"));
        server.shutdown().await;
    }

    #[test]
    fn request_line_parser_rejects_trailing_tokens() {
        let err = parse_request_line(b"GET /metrics HTTP/1.1 extra\r\n").unwrap_err();
        assert_eq!(err, ());
    }
}
