//! Pipelined commit batcher.
//!
//! Collects in-flight commits into a sliding batch so the WAL flush
//! and quorum ack happen once per group instead of once per txn.
//! Each producer awaits its own oneshot; the batcher fires when
//! `max_batch` is reached or `max_wait` elapses.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

pub struct PendingCommit {
    pub txn_id: u64,
    pub ack: oneshot::Sender<CommitOutcome>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitOutcome {
    Committed,
    Aborted,
}

#[derive(Clone, Debug)]
pub struct CommitPipelineConfig {
    pub max_batch: usize,
    pub max_wait: Duration,
}

impl Default for CommitPipelineConfig {
    fn default() -> Self {
        Self {
            max_batch: 64,
            max_wait: Duration::from_millis(5),
        }
    }
}

pub struct CommitPipeline {
    cfg: CommitPipelineConfig,
    inner: Arc<std::sync::Mutex<PipelineState>>,
}

struct PipelineState {
    pending: VecDeque<PendingCommit>,
    first_arrived_at: Option<Instant>,
}

impl CommitPipeline {
    pub fn new(cfg: CommitPipelineConfig) -> Self {
        Self {
            cfg,
            inner: Arc::new(std::sync::Mutex::new(PipelineState {
                pending: VecDeque::new(),
                first_arrived_at: None,
            })),
        }
    }

    /// Submit a commit. The returned oneshot resolves once the batch
    /// containing this commit has been flushed.
    pub fn submit(&self, txn_id: u64) -> oneshot::Receiver<CommitOutcome> {
        let (tx, rx) = oneshot::channel();
        let mut g = self.inner.lock().unwrap();
        if g.pending.is_empty() {
            g.first_arrived_at = Some(Instant::now());
        }
        g.pending.push_back(PendingCommit { txn_id, ack: tx });
        rx
    }

    /// Returns true if a flush should fire now (either size or time).
    pub fn should_flush(&self) -> bool {
        let g = self.inner.lock().unwrap();
        if g.pending.len() >= self.cfg.max_batch {
            return true;
        }
        if let Some(t) = g.first_arrived_at {
            return Instant::now().saturating_duration_since(t) >= self.cfg.max_wait;
        }
        false
    }

    /// Drain the current batch and finalise with `outcome`. Returns
    /// the txn ids that were acked.
    pub fn flush(&self, outcome: CommitOutcome) -> Vec<u64> {
        let mut g = self.inner.lock().unwrap();
        let batch: Vec<PendingCommit> = g.pending.drain(..).collect();
        g.first_arrived_at = None;
        drop(g);
        let mut ids = Vec::with_capacity(batch.len());
        for p in batch {
            ids.push(p.txn_id);
            let _ = p.ack.send(outcome);
        }
        ids
    }

    pub fn pending(&self) -> usize {
        self.inner.lock().unwrap().pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn submitted_commit_resolves_after_flush() {
        let p = CommitPipeline::new(CommitPipelineConfig::default());
        let rx = p.submit(42);
        let ids = p.flush(CommitOutcome::Committed);
        assert_eq!(ids, vec![42]);
        let r = rx.await.unwrap();
        assert_eq!(r, CommitOutcome::Committed);
    }

    #[tokio::test]
    async fn should_flush_triggers_on_batch_size() {
        let p = CommitPipeline::new(CommitPipelineConfig {
            max_batch: 2,
            max_wait: Duration::from_secs(60),
        });
        let _r1 = p.submit(1);
        assert!(!p.should_flush());
        let _r2 = p.submit(2);
        assert!(p.should_flush());
    }

    #[tokio::test]
    async fn should_flush_triggers_on_timeout() {
        let p = CommitPipeline::new(CommitPipelineConfig {
            max_batch: 100,
            max_wait: Duration::from_millis(10),
        });
        let _r = p.submit(1);
        std::thread::sleep(Duration::from_millis(30));
        assert!(p.should_flush());
    }

    #[tokio::test]
    async fn flush_clears_pending() {
        let p = CommitPipeline::new(CommitPipelineConfig::default());
        let _r1 = p.submit(1);
        let _r2 = p.submit(2);
        assert_eq!(p.pending(), 2);
        p.flush(CommitOutcome::Aborted);
        assert_eq!(p.pending(), 0);
    }

    #[tokio::test]
    async fn aborted_outcome_propagates() {
        let p = CommitPipeline::new(CommitPipelineConfig::default());
        let rx = p.submit(1);
        p.flush(CommitOutcome::Aborted);
        assert_eq!(rx.await.unwrap(), CommitOutcome::Aborted);
    }

    #[tokio::test]
    async fn empty_flush_returns_empty() {
        let p = CommitPipeline::new(CommitPipelineConfig::default());
        let ids = p.flush(CommitOutcome::Committed);
        assert!(ids.is_empty());
    }
}
