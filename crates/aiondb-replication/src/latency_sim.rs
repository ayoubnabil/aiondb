//! Cross-region latency simulator.
//!
//! Provides a `LatencyInjector` adapter around any byte stream that
//! delays writes by a configured RTT before flushing. Used in tests
//! to simulate WAN replication latency without spinning up real
//! geographically-distributed nodes.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Wraps an underlying stream and delays every write by `one_way_rtt`.
pub struct LatencyStream<S> {
    inner: S,
    one_way_rtt: Duration,
    pending: Option<std::pin::Pin<Box<tokio::time::Sleep>>>,
}

impl<S> LatencyStream<S> {
    pub fn new(inner: S, one_way_rtt: Duration) -> Self {
        Self {
            inner,
            one_way_rtt,
            pending: None,
        }
    }

    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for LatencyStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for LatencyStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.pending.is_none() {
            self.pending = Some(Box::pin(tokio::time::sleep(self.one_way_rtt)));
        }
        // Drive the sleep first.
        if let Some(sleep) = self.pending.as_mut() {
            match sleep.as_mut().poll(cx) {
                Poll::Ready(()) => {
                    self.pending = None;
                }
                Poll::Pending => return Poll::Pending,
            }
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Plain helper: sleep then run a future. Useful for raft tests that
/// want a coarse-grained "delay every send" wrapper without changing
/// the underlying stream type.
pub async fn with_latency<F, T>(delay: Duration, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::sleep(delay).await;
    fut.await
}

/// Region descriptor for multi-region latency models.
#[derive(Clone, Copy, Debug)]
pub struct RegionRtt {
    pub region: &'static str,
    pub rtt: Duration,
}

/// Latency matrix : map of `(src_region, dst_region) -> RTT`.
#[derive(Clone, Debug, Default)]
pub struct LatencyMatrix {
    entries:
        Arc<std::sync::Mutex<std::collections::HashMap<(&'static str, &'static str), Duration>>>,
}

impl LatencyMatrix {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, src: &'static str, dst: &'static str, rtt: Duration) {
        self.entries.lock().unwrap().insert((src, dst), rtt);
    }

    pub fn get(&self, src: &'static str, dst: &'static str) -> Duration {
        self.entries
            .lock()
            .unwrap()
            .get(&(src, dst))
            .copied()
            .unwrap_or(Duration::ZERO)
    }

    /// Stable "world-class" latency profile: 4 regions with the round
    /// trips an EU/US/AP cluster would observe.
    pub fn world_profile() -> Self {
        let m = Self::new();
        // Intra-region cheap.
        m.set("eu", "eu", Duration::from_millis(1));
        m.set("us", "us", Duration::from_millis(1));
        m.set("ap", "ap", Duration::from_millis(1));
        m.set("sa", "sa", Duration::from_millis(1));
        // EU <-> US ~ 80ms.
        m.set("eu", "us", Duration::from_millis(80));
        m.set("us", "eu", Duration::from_millis(80));
        // EU <-> AP ~ 180ms.
        m.set("eu", "ap", Duration::from_millis(180));
        m.set("ap", "eu", Duration::from_millis(180));
        // US <-> AP ~ 150ms.
        m.set("us", "ap", Duration::from_millis(150));
        m.set("ap", "us", Duration::from_millis(150));
        // SA <-> anywhere ~ 200ms.
        m.set("sa", "eu", Duration::from_millis(200));
        m.set("eu", "sa", Duration::from_millis(200));
        m.set("sa", "us", Duration::from_millis(120));
        m.set("us", "sa", Duration::from_millis(120));
        m.set("sa", "ap", Duration::from_millis(250));
        m.set("ap", "sa", Duration::from_millis(250));
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn with_latency_delays_by_configured_rtt() {
        let start = tokio::time::Instant::now();
        let value: u32 = with_latency(Duration::from_millis(100), async { 42 }).await;
        assert_eq!(value, 42);
        assert!(start.elapsed() >= Duration::from_millis(99));
    }

    #[test]
    fn matrix_defaults_to_zero_for_unknown_pairs() {
        let m = LatencyMatrix::new();
        assert_eq!(m.get("eu", "us"), Duration::ZERO);
    }

    #[test]
    fn world_profile_models_cross_region_latency() {
        let m = LatencyMatrix::world_profile();
        assert_eq!(m.get("eu", "eu"), Duration::from_millis(1));
        assert_eq!(m.get("eu", "us"), Duration::from_millis(80));
        assert_eq!(m.get("ap", "sa"), Duration::from_millis(250));
    }
}
