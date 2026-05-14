//! Cluster-wide write throttle.
//!
//! Hard cap on cluster total writes per second. Used by the
//! orchestrator to bound load on a degraded cluster (e.g. during
//! split / merge storms or recovery).

use std::sync::Arc;
use std::time::Duration;

use crate::TokenBucket;

#[derive(Clone, Debug)]
pub struct ClusterThrottleConfig {
    pub total_write_qps: f64,
    pub burst: u64,
}

impl Default for ClusterThrottleConfig {
    fn default() -> Self {
        Self {
            total_write_qps: 10_000.0,
            burst: 2_000,
        }
    }
}

#[derive(Clone)]
pub struct ClusterThrottle {
    bucket: Arc<TokenBucket>,
}

impl ClusterThrottle {
    pub fn new(config: ClusterThrottleConfig) -> aiondb_core::DbResult<Self> {
        Ok(Self {
            bucket: Arc::new(TokenBucket::new(config.burst, config.total_write_qps)?),
        })
    }

    pub fn try_admit(&self, n: u64) -> bool {
        self.bucket.try_acquire(n)
    }

    pub async fn admit(&self, n: u64, timeout: Duration) -> aiondb_core::DbResult<()> {
        self.bucket.acquire(n, Some(timeout)).await
    }

    pub fn available(&self) -> f64 {
        self.bucket.available()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_throttle_caps_burst() {
        let t = ClusterThrottle::new(ClusterThrottleConfig {
            total_write_qps: 0.0,
            burst: 5,
        })
        .unwrap();
        for _ in 0..5 {
            assert!(t.try_admit(1));
        }
        assert!(!t.try_admit(1));
    }

    #[test]
    fn admit_multiple_at_once() {
        let t = ClusterThrottle::new(ClusterThrottleConfig {
            total_write_qps: 0.0,
            burst: 10,
        })
        .unwrap();
        assert!(t.try_admit(7));
        assert!(t.try_admit(3));
        assert!(!t.try_admit(1));
    }

    #[tokio::test(start_paused = true)]
    async fn refills_at_configured_qps() {
        let t = ClusterThrottle::new(ClusterThrottleConfig {
            total_write_qps: 100.0,
            burst: 10,
        })
        .unwrap();
        for _ in 0..10 {
            assert!(t.try_admit(1));
        }
        assert!(!t.try_admit(1));
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..10 {
            assert!(t.try_admit(1));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn admit_async_succeeds_after_refill() {
        let t = ClusterThrottle::new(ClusterThrottleConfig {
            total_write_qps: 100.0,
            burst: 1,
        })
        .unwrap();
        assert!(t.try_admit(1));
        let t2 = t.clone();
        let h = tokio::spawn(async move {
            t2.admit(1, Duration::from_secs(5)).await.unwrap();
        });
        tokio::time::advance(Duration::from_millis(20)).await;
        h.await.unwrap();
    }

    #[test]
    fn available_reflects_consumed_tokens() {
        let t = ClusterThrottle::new(ClusterThrottleConfig {
            total_write_qps: 0.0,
            burst: 5,
        })
        .unwrap();
        let before = t.available();
        t.try_admit(2);
        let after = t.available();
        assert!(after < before);
    }
}
