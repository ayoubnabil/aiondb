//! Pipelined writes.
//!
//! Cockroach calls this "transactional pipelining" : the coordinator
//! does not wait for write `N`'s quorum acknowledgement before issuing
//! write `N+1`. Each pending write carries a future its ack will
//! resolve, and the commit step joins on every pending future at once.
//!
//! Net effect : a 1-second WAN-replication round trip turns the
//! N-write critical section from `N * RTT` into one `RTT` while
//! preserving linearisability (commits still wait on every ack).

use std::collections::VecDeque;
use std::sync::Arc;

use aiondb_core::{DbError, DbResult};
use tokio::sync::oneshot;

/// Token returned by `submit`. Resolves to Ok once the write is durable.
pub type WriteAck = oneshot::Receiver<DbResult<()>>;

#[derive(Debug)]
pub struct PipelinedCoordinator {
    inner: Arc<std::sync::Mutex<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    pending: VecDeque<oneshot::Sender<DbResult<()>>>,
    sequence: u64,
}

impl PipelinedCoordinator {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(Inner::default())),
        }
    }

    /// Submit a write. Returns a sequence number + an Ack token.
    pub fn submit(&self) -> (u64, WriteAck) {
        let (tx, rx) = oneshot::channel();
        let mut guard = self.inner.lock().unwrap();
        guard.sequence = guard.sequence.saturating_add(1);
        let seq = guard.sequence;
        guard.pending.push_back(tx);
        (seq, rx)
    }

    /// Acknowledge the **oldest** pending write with success.
    pub fn ack_oldest(&self) -> bool {
        let mut guard = self.inner.lock().unwrap();
        match guard.pending.pop_front() {
            Some(tx) => {
                let _ = tx.send(Ok(()));
                true
            }
            None => false,
        }
    }

    /// Fail the **oldest** pending write with `error`.
    pub fn fail_oldest(&self, error: impl Into<String>) -> bool {
        let mut guard = self.inner.lock().unwrap();
        match guard.pending.pop_front() {
            Some(tx) => {
                let _ = tx.send(Err(DbError::internal(error.into())));
                true
            }
            None => false,
        }
    }

    /// Drain every pending write with the same outcome.
    pub fn fail_all(&self, error: impl Into<String>) {
        let mut guard = self.inner.lock().unwrap();
        let reason = error.into();
        while let Some(tx) = guard.pending.pop_front() {
            let _ = tx.send(Err(DbError::internal(reason.clone())));
        }
    }

    pub fn pending_count(&self) -> usize {
        self.inner.lock().unwrap().pending.len()
    }
}

impl Default for PipelinedCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn submit_then_ack_resolves_token() {
        let coord = PipelinedCoordinator::new();
        let (seq, ack) = coord.submit();
        assert_eq!(seq, 1);
        coord.ack_oldest();
        let result = tokio::time::timeout(Duration::from_millis(20), ack)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn pipelined_acks_arrive_in_submit_order() {
        let coord = PipelinedCoordinator::new();
        let (s1, a1) = coord.submit();
        let (s2, a2) = coord.submit();
        let (s3, a3) = coord.submit();
        assert_eq!(s1, 1);
        assert_eq!(s2, 2);
        assert_eq!(s3, 3);
        coord.ack_oldest();
        coord.ack_oldest();
        coord.ack_oldest();
        let r1 = tokio::time::timeout(Duration::from_millis(20), a1)
            .await
            .unwrap()
            .unwrap();
        let r2 = tokio::time::timeout(Duration::from_millis(20), a2)
            .await
            .unwrap()
            .unwrap();
        let r3 = tokio::time::timeout(Duration::from_millis(20), a3)
            .await
            .unwrap()
            .unwrap();
        assert!(r1.is_ok());
        assert!(r2.is_ok());
        assert!(r3.is_ok());
    }

    #[tokio::test]
    async fn fail_all_drains_pending() {
        let coord = PipelinedCoordinator::new();
        let (_s1, a1) = coord.submit();
        let (_s2, a2) = coord.submit();
        coord.fail_all("rpc timeout");
        let r1 = tokio::time::timeout(Duration::from_millis(20), a1)
            .await
            .unwrap()
            .unwrap();
        let r2 = tokio::time::timeout(Duration::from_millis(20), a2)
            .await
            .unwrap()
            .unwrap();
        assert!(r1.is_err());
        assert!(r2.is_err());
        assert_eq!(coord.pending_count(), 0);
    }

    #[test]
    fn ack_without_pending_returns_false() {
        let coord = PipelinedCoordinator::new();
        assert!(!coord.ack_oldest());
    }

    #[tokio::test]
    async fn fail_oldest_marks_only_first() {
        let coord = PipelinedCoordinator::new();
        let (_s1, a1) = coord.submit();
        let (_s2, a2) = coord.submit();
        coord.fail_oldest("conflict");
        coord.ack_oldest();
        let r1 = tokio::time::timeout(Duration::from_millis(20), a1)
            .await
            .unwrap()
            .unwrap();
        let r2 = tokio::time::timeout(Duration::from_millis(20), a2)
            .await
            .unwrap()
            .unwrap();
        assert!(r1.is_err());
        assert!(r2.is_ok());
    }
}
