//! HTTP health-check server.
//!
//! Exposes `/healthz` and `/readyz` over TCP. Uses
//! [`crate::health_probe`] for the logic.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::health_probe::{
    evaluate_healthz, evaluate_readyz, HealthStatus, ProbeInputs, ReadinessStatus,
};

pub trait ProbeSource: Send + Sync {
    fn collect(&self) -> ProbeInputs;
}

pub struct HealthServer {
    handle: JoinHandle<()>,
    shutdown_tx: watch::Sender<bool>,
    local_addr: SocketAddr,
}

impl HealthServer {
    pub async fn start(source: Arc<dyn ProbeSource>, bind: SocketAddr) -> io::Result<Self> {
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
                    accept = listener.accept() => if let Ok((mut stream, _)) = accept {
                        let source = Arc::clone(&source);
                        tokio::spawn(async move {
                            let _ = serve(source, &mut stream).await;
                        });
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

async fn serve(source: Arc<dyn ProbeSource>, stream: &mut TcpStream) -> io::Result<()> {
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }
    let head = std::str::from_utf8(&buf[..n]).unwrap_or("");
    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
    let inputs = source.collect();
    let (status, body) = match path.as_str() {
        "/healthz" => match evaluate_healthz(&inputs) {
            HealthStatus::Healthy => ("200 OK", "ok\n".to_string()),
            HealthStatus::Degraded { reason_code } => (
                "503 Service Unavailable",
                format!("degraded {reason_code}\n"),
            ),
            HealthStatus::Down { reason_code } => {
                ("503 Service Unavailable", format!("down {reason_code}\n"))
            }
        },
        "/readyz" => match evaluate_readyz(&inputs) {
            ReadinessStatus::Ready => ("200 OK", "ready\n".to_string()),
            ReadinessStatus::NotReady { reason_code } => (
                "503 Service Unavailable",
                format!("not-ready {reason_code}\n"),
            ),
        },
        _ => ("404 Not Found", "not found\n".to_string()),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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

    struct FixedSource(ProbeInputs);
    impl ProbeSource for FixedSource {
        fn collect(&self) -> ProbeInputs {
            self.0
        }
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

    fn healthy() -> ProbeInputs {
        ProbeInputs {
            raft_leader_present: true,
            gossip_alive_count: 3,
            gossip_total_count: 3,
            last_apply_lag_entries: 0,
            draining: false,
        }
    }

    #[tokio::test]
    async fn healthy_returns_200() {
        let server = HealthServer::start(
            Arc::new(FixedSource(healthy())),
            "127.0.0.1:0".parse().unwrap(),
        )
        .await
        .unwrap();
        let body = fetch(server.local_addr(), "/healthz").await;
        assert!(body.contains("HTTP/1.1 200 OK"));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn degraded_returns_503() {
        let mut inputs = healthy();
        inputs.last_apply_lag_entries = 5_000_000;
        let server = HealthServer::start(
            Arc::new(FixedSource(inputs)),
            "127.0.0.1:0".parse().unwrap(),
        )
        .await
        .unwrap();
        let body = fetch(server.local_addr(), "/healthz").await;
        assert!(body.contains("HTTP/1.1 503"));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        let server = HealthServer::start(
            Arc::new(FixedSource(healthy())),
            "127.0.0.1:0".parse().unwrap(),
        )
        .await
        .unwrap();
        let body = fetch(server.local_addr(), "/foo").await;
        assert!(body.contains("HTTP/1.1 404"));
        server.shutdown().await;
    }
}
