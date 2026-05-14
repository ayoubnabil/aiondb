//! Per-session vector clock for read-your-writes consistency.
//!
//! Cockroach calls this a "causality token". Each session keeps a
//! per-shard vector of the highest commit timestamps it has observed.
//! Subsequent reads on the same shard must wait until the local
//! applied state has advanced past those timestamps; reads on other
//! shards are unaffected.
//!
//! This module provides the data structure and the merge / wait
//! semantics. Storage hooks plug in via the closed-timestamp tracker
//! (read waits for `closed_ts >= causality(shard)`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aiondb_core::DbResult;

use crate::hlc::HlcTimestamp;

/// Opaque shard / range identifier carried alongside causality.
pub type CausalityShardId = u64;

/// Per-session causality token. Cheap to clone -- the typical session
/// touches < 10 shards.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CausalityToken {
    entries: BTreeMap<CausalityShardId, HlcTimestamp>,
}

impl CausalityToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the session observed (or wrote) `ts` on `shard`.
    /// Monotonic : an older timestamp is silently ignored.
    pub fn observe(&mut self, shard: CausalityShardId, ts: HlcTimestamp) {
        let slot = self.entries.entry(shard).or_default();
        if ts > *slot {
            *slot = ts;
        }
    }

    /// Highest observed timestamp for `shard`, if any.
    pub fn observed(&self, shard: CausalityShardId) -> Option<HlcTimestamp> {
        self.entries.get(&shard).copied()
    }

    /// Merge another token's entries into `self`. After merging, both
    /// participants share at least the union of observed timestamps.
    pub fn merge(&mut self, other: &CausalityToken) {
        for (shard, ts) in &other.entries {
            self.observe(*shard, *ts);
        }
    }

    /// Iterate every entry, ordered by shard id.
    pub fn iter(&self) -> impl Iterator<Item = (CausalityShardId, HlcTimestamp)> + '_ {
        self.entries.iter().map(|(s, ts)| (*s, *ts))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Encode as JSON for client transport.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.entries
                .iter()
                .map(|(s, ts)| {
                    serde_json::json!({
                        "shard": s,
                        "wall_time_us": ts.wall_time_us,
                        "logical": ts.logical,
                    })
                })
                .collect(),
        )
    }

    /// Decode from a JSON value produced by [`Self::to_json`].
    pub fn from_json(value: &serde_json::Value) -> DbResult<Self> {
        let arr = value
            .as_array()
            .ok_or_else(|| aiondb_core::DbError::internal("causality token must be an array"))?;
        let mut out = Self::new();
        for entry in arr {
            let shard = entry["shard"]
                .as_u64()
                .ok_or_else(|| aiondb_core::DbError::internal("missing shard"))?;
            let wall = entry["wall_time_us"]
                .as_u64()
                .ok_or_else(|| aiondb_core::DbError::internal("missing wall_time_us"))?;
            let logical = entry["logical"].as_u64().unwrap_or(0) as u32;
            out.observe(shard, HlcTimestamp::new(wall, logical));
        }
        Ok(out)
    }
}

/// Read coordinator that uses the token to gate stale reads.
#[derive(Clone)]
pub struct CausalityCoordinator {
    /// Per-shard applied-progress callback. Returns the highest
    /// timestamp applied locally for that shard.
    progress_fn: Arc<dyn Fn(CausalityShardId) -> HlcTimestamp + Send + Sync>,
    /// How long to wait for the local replica to catch up before
    /// giving up.
    wait_budget: Duration,
}

impl CausalityCoordinator {
    pub fn new(
        progress_fn: Arc<dyn Fn(CausalityShardId) -> HlcTimestamp + Send + Sync>,
        wait_budget: Duration,
    ) -> Self {
        Self {
            progress_fn,
            wait_budget,
        }
    }

    /// Check whether the local replica has caught up to every entry
    /// of `token`. Returns the list of shards that are still behind.
    pub fn lagging_shards(&self, token: &CausalityToken) -> Vec<(CausalityShardId, HlcTimestamp)> {
        let mut behind = Vec::new();
        for (shard, ts) in token.iter() {
            let local = (self.progress_fn)(shard);
            if local < ts {
                behind.push((shard, ts));
            }
        }
        behind
    }

    /// Wait (up to `wait_budget`) for the local replica to catch up.
    /// Returns `Ok(())` on success, error on timeout.
    pub async fn wait_until_caught_up(&self, token: &CausalityToken) -> DbResult<()> {
        let start = tokio::time::Instant::now();
        loop {
            let lagging = self.lagging_shards(token);
            if lagging.is_empty() {
                return Ok(());
            }
            if start.elapsed() >= self.wait_budget {
                return Err(aiondb_core::DbError::internal(format!(
                    "causality wait timed out: {} shard(s) still behind",
                    lagging.len()
                )));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn ts(wall: u64) -> HlcTimestamp {
        HlcTimestamp::new(wall, 0)
    }

    #[test]
    fn observe_advances_monotonically() {
        let mut t = CausalityToken::new();
        t.observe(1, ts(100));
        t.observe(1, ts(50)); // older
        assert_eq!(t.observed(1).unwrap(), ts(100));
        t.observe(1, ts(200));
        assert_eq!(t.observed(1).unwrap(), ts(200));
    }

    #[test]
    fn merge_takes_max_per_shard() {
        let mut a = CausalityToken::new();
        a.observe(1, ts(100));
        a.observe(2, ts(150));
        let mut b = CausalityToken::new();
        b.observe(1, ts(200));
        b.observe(3, ts(300));
        a.merge(&b);
        assert_eq!(a.observed(1).unwrap(), ts(200));
        assert_eq!(a.observed(2).unwrap(), ts(150));
        assert_eq!(a.observed(3).unwrap(), ts(300));
    }

    #[test]
    fn json_round_trips() {
        let mut t = CausalityToken::new();
        t.observe(7, HlcTimestamp::new(123, 4));
        t.observe(11, HlcTimestamp::new(999, 0));
        let encoded = t.to_json();
        let decoded = CausalityToken::from_json(&encoded).unwrap();
        assert_eq!(t, decoded);
    }

    #[test]
    fn lagging_shards_lists_only_behind_entries() {
        let progress = AtomicU64::new(50);
        let progress_fn = Arc::new(move |_shard: CausalityShardId| {
            HlcTimestamp::new(progress.load(Ordering::SeqCst), 0)
        });
        let coord = CausalityCoordinator::new(progress_fn, Duration::from_millis(100));
        let mut token = CausalityToken::new();
        token.observe(1, ts(100));
        token.observe(2, ts(40)); // already caught up
        let behind = coord.lagging_shards(&token);
        assert_eq!(behind.len(), 1);
        assert_eq!(behind[0].0, 1);
    }

    #[tokio::test]
    async fn wait_until_caught_up_succeeds_when_progress_catches_up() {
        let progress = Arc::new(AtomicU64::new(0));
        let progress_clone = Arc::clone(&progress);
        let progress_fn = Arc::new(move |_shard: CausalityShardId| {
            HlcTimestamp::new(progress_clone.load(Ordering::SeqCst), 0)
        });
        let coord = CausalityCoordinator::new(progress_fn, Duration::from_millis(200));
        let mut token = CausalityToken::new();
        token.observe(1, ts(100));
        // Spawn task that advances progress.
        let p2 = Arc::clone(&progress);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            p2.store(150, Ordering::SeqCst);
        });
        coord.wait_until_caught_up(&token).await.unwrap();
    }

    #[tokio::test]
    async fn wait_until_caught_up_times_out() {
        let progress_fn = Arc::new(|_shard: CausalityShardId| HlcTimestamp::new(0, 0));
        let coord = CausalityCoordinator::new(progress_fn, Duration::from_millis(30));
        let mut token = CausalityToken::new();
        token.observe(1, ts(100));
        let err = coord.wait_until_caught_up(&token).await.unwrap_err();
        assert!(err.to_string().contains("causality wait timed out"));
    }
}
