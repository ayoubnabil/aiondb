//! Parallel fanout executor.
//!
//! Sends an identical request to many shards in parallel and gathers
//! their responses. Caller supplies an async closure per shard.
//! Each individual call has an independent timeout; the gather phase
//! respects an overall deadline. Failed shards are returned as
//! `Result::Err` so the caller decides quorum / best-effort semantics.

use std::future::Future;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio::time;

use crate::range_descriptor::RangeId;

#[derive(Clone, Debug)]
pub struct FanoutOptions {
    pub per_call_timeout: Duration,
    pub overall_timeout: Duration,
    pub concurrency: usize,
}

impl Default for FanoutOptions {
    fn default() -> Self {
        Self {
            per_call_timeout: Duration::from_secs(2),
            overall_timeout: Duration::from_secs(10),
            concurrency: 64,
        }
    }
}

#[derive(Debug)]
pub struct ShardResult<T> {
    pub range: RangeId,
    pub outcome: Result<T, FanoutError>,
}

#[derive(Debug)]
pub enum FanoutError {
    Timeout(Duration),
    Shard(String),
    OverallDeadline,
}

impl std::fmt::Display for FanoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FanoutError::Timeout(d) => write!(f, "call timed out after {d:?}"),
            FanoutError::Shard(e) => write!(f, "shard returned error: {e}"),
            FanoutError::OverallDeadline => write!(f, "overall fanout deadline exceeded"),
        }
    }
}

impl std::error::Error for FanoutError {}

pub async fn fanout<T, F, Fut>(
    targets: Vec<RangeId>,
    opts: FanoutOptions,
    call: F,
) -> Vec<ShardResult<T>>
where
    T: Send + 'static,
    F: Fn(RangeId) -> Fut + Send + Sync + 'static + Clone,
    Fut: Future<Output = Result<T, String>> + Send + 'static,
{
    let per = opts.per_call_timeout;
    let overall = opts.overall_timeout;
    let mut set: JoinSet<ShardResult<T>> = JoinSet::new();
    for range in targets {
        let call = call.clone();
        set.spawn(async move {
            let fut = call(range);
            let outcome = match time::timeout(per, fut).await {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(FanoutError::Shard(e)),
                Err(_) => Err(FanoutError::Timeout(per)),
            };
            ShardResult { range, outcome }
        });
    }
    let mut out = Vec::new();
    let deadline = time::Instant::now() + overall;
    loop {
        let remaining = deadline.saturating_duration_since(time::Instant::now());
        match time::timeout(remaining, set.join_next()).await {
            Ok(Some(Ok(res))) => out.push(res),
            Ok(Some(Err(e))) => out.push(ShardResult {
                range: RangeId::new(0),
                outcome: Err(FanoutError::Shard(e.to_string())),
            }),
            Ok(None) => break,
            Err(_) => {
                set.abort_all();
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn fanout_collects_all_results() {
        let ranges: Vec<RangeId> = (1..=5).map(RangeId::new).collect();
        let opts = FanoutOptions {
            per_call_timeout: Duration::from_secs(1),
            overall_timeout: Duration::from_secs(5),
            concurrency: 8,
        };
        let results = fanout(ranges, opts, |r| async move { Ok::<u64, String>(r.get()) }).await;
        assert_eq!(results.len(), 5);
        assert!(results.iter().all(|r| r.outcome.is_ok()));
    }

    #[tokio::test]
    async fn fanout_records_per_shard_errors() {
        let ranges: Vec<RangeId> = vec![RangeId::new(1), RangeId::new(2)];
        let results = fanout(ranges, FanoutOptions::default(), |r| async move {
            if r.get() == 1 {
                Err("boom".to_string())
            } else {
                Ok(7)
            }
        })
        .await;
        let errs = results.iter().filter(|r| r.outcome.is_err()).count();
        assert_eq!(errs, 1);
    }

    #[tokio::test]
    async fn fanout_honors_per_call_timeout() {
        let ranges = vec![RangeId::new(1)];
        let opts = FanoutOptions {
            per_call_timeout: Duration::from_millis(20),
            overall_timeout: Duration::from_secs(5),
            concurrency: 4,
        };
        let results = fanout(ranges, opts, |_| async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok::<(), String>(())
        })
        .await;
        assert!(matches!(results[0].outcome, Err(FanoutError::Timeout(_))));
    }

    #[tokio::test]
    async fn fanout_runs_concurrently() {
        let counter = Arc::new(AtomicUsize::new(0));
        let ranges: Vec<RangeId> = (1..=4).map(RangeId::new).collect();
        let c2 = counter.clone();
        let start = std::time::Instant::now();
        let _ = fanout(ranges, FanoutOptions::default(), move |_| {
            let c2 = c2.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(80)).await;
                c2.fetch_add(1, Ordering::Relaxed);
                Ok::<(), String>(())
            }
        })
        .await;
        let elapsed = start.elapsed();
        assert_eq!(counter.load(Ordering::Relaxed), 4);
        assert!(elapsed < Duration::from_millis(300));
    }

    #[tokio::test]
    async fn fanout_empty_targets_returns_empty() {
        let results: Vec<ShardResult<u64>> =
            fanout(vec![], FanoutOptions::default(), |_| async move { Ok(1) }).await;
        assert!(results.is_empty());
    }
}
