//! Leaseholder coordinator loop.
//!
//! A leaseholder is the replica that:
//!
//! 1. Serves linearizable reads / writes for one shard.
//! 2. Renews its lease via the [`LeaseRegistry`] before expiry.
//! 3. Publishes closed timestamps via the [`ClosedTimestampTracker`]
//!    so followers can serve stale reads.
//!
//! This module provides the background loop that does (2) and (3) for
//! a single shard. Callers spawn one loop per shard the local node
//! holds a lease on; the loop exits when the lease is preempted
//! (either by another node taking it, or because the holder calls
//! [`LeaseholderHandle::demote`]).
//!
//! Keeping the loop in its own module makes the lifecycle easy to
//! reason about and easy to test independently of the rest of the
//! cluster runtime.

use std::sync::Arc;
use std::time::{Duration, Instant as StdInstant};

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{self, Instant};
use tracing::{debug, trace, warn};

use crate::closed_timestamp::{target_closed_timestamp_now, ClosedTimestampTracker};
use crate::lease::{Lease, LeaseEpoch, LeaseHolderId, LeaseOutcome, LeaseRegistry};
use crate::ShardId;

/// Configuration for one leaseholder loop.
#[derive(Clone, Debug)]
pub struct LeaseholderConfig {
    pub shard: ShardId,
    pub holder: LeaseHolderId,
    /// Full lease lifetime. The loop renews at `ttl / 3` intervals.
    pub ttl: Duration,
    /// Closed-timestamp lag policy.
    pub closed_ts_lag: Duration,
    /// How often the loop tries to publish a new closed timestamp.
    pub closed_ts_tick: Duration,
}

impl LeaseholderConfig {
    pub fn renewal_interval(&self) -> Duration {
        let mut interval = self.ttl / 3;
        if interval.is_zero() {
            interval = Duration::from_millis(1);
        }
        interval
    }
}

/// Reason the loop stopped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LeaseholderExit {
    /// The caller asked the loop to stop by dropping the handle or
    /// calling `demote`.
    Shutdown,
    /// Another replica preempted the lease (epoch bumped or holder
    /// changed). The loop yields gracefully.
    Preempted { current: Lease },
    /// No lease was held when the loop tried to renew. Treated like
    /// preemption.
    NoLease,
}

/// Handle returned by [`spawn_leaseholder`]. Drop or call `demote` to
/// stop the loop. Awaiting `wait` resolves once the loop has exited.
#[derive(Debug)]
pub struct LeaseholderHandle {
    join: JoinHandle<LeaseholderExit>,
    shutdown: watch::Sender<bool>,
    epoch: LeaseEpoch,
}

impl LeaseholderHandle {
    pub fn epoch(&self) -> LeaseEpoch {
        self.epoch
    }

    /// Signal the loop to stop. The loop will exit at its next
    /// iteration without revoking the lease (the lease is left to
    /// expire naturally so an in-flight transfer can complete).
    pub fn demote(&self) {
        let _ = self.shutdown.send(true);
    }

    /// Wait for the loop to exit and return its termination reason.
    pub async fn wait(self) -> LeaseholderExit {
        self.join.await.unwrap_or(LeaseholderExit::Shutdown)
    }
}

/// Acquire a lease for `config.shard`, then spawn a background loop
/// that renews it and publishes closed timestamps until preempted or
/// asked to stop.
///
/// Returns `None` when the initial acquire failed because another
/// holder already owns the lease.
pub fn spawn_leaseholder(
    registry: LeaseRegistry,
    tracker: ClosedTimestampTracker,
    config: LeaseholderConfig,
) -> Option<LeaseholderHandle> {
    let now = StdInstant::now();
    let acquired = match registry.acquire(config.shard, config.holder, config.ttl, now) {
        LeaseOutcome::Granted(lease) => lease,
        LeaseOutcome::Held(_) | LeaseOutcome::Stale { .. } => return None,
    };
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let join = tokio::spawn(run_loop(registry, tracker, config, acquired, shutdown_rx));
    Some(LeaseholderHandle {
        join,
        shutdown: shutdown_tx,
        epoch: acquired.epoch,
    })
}

async fn run_loop(
    registry: LeaseRegistry,
    tracker: ClosedTimestampTracker,
    config: LeaseholderConfig,
    initial: Lease,
    mut shutdown_rx: watch::Receiver<bool>,
) -> LeaseholderExit {
    let mut current = initial;
    let mut next_renew = Instant::now() + config.renewal_interval();
    let mut next_publish = Instant::now() + config.closed_ts_tick;

    // Publish an initial closed timestamp immediately so follower reads
    // have a horizon as soon as the leaseholder appears.
    let initial_target = target_closed_timestamp_now(config.closed_ts_lag);
    tracker.publish(config.shard, initial_target);

    loop {
        if *shutdown_rx.borrow() {
            debug!(shard = ?config.shard, "leaseholder loop shutdown via handle");
            return LeaseholderExit::Shutdown;
        }

        // Pick the next event: renewal, closed-ts publish, or shutdown.
        let wake = next_renew.min(next_publish);
        tokio::select! {
            () = time::sleep_until(wake) => {},
            res = shutdown_rx.changed() => {
                // Treat dropped sender or value=true the same way: exit.
                let _ = res;
                if *shutdown_rx.borrow() {
                    return LeaseholderExit::Shutdown;
                }
            }
        }

        let now = Instant::now();
        let std_now = StdInstant::now();
        if now >= next_renew {
            match registry.extend(
                config.shard,
                config.holder,
                current.epoch,
                config.ttl,
                std_now,
            ) {
                LeaseOutcome::Granted(renewed) => {
                    trace!(shard = ?config.shard, epoch = %renewed.epoch, "lease renewed");
                    current = renewed;
                    next_renew = now + config.renewal_interval();
                }
                LeaseOutcome::Held(other) => {
                    warn!(shard = ?config.shard, other = ?other, "lease held by another holder mid-loop");
                    return LeaseholderExit::Preempted { current: other };
                }
                LeaseOutcome::Stale { current } => {
                    warn!(shard = ?config.shard, current = ?current, "leaseholder preempted");
                    if current.epoch == LeaseEpoch::default() {
                        return LeaseholderExit::NoLease;
                    }
                    return LeaseholderExit::Preempted { current };
                }
            }
        }
        if now >= next_publish {
            let target = target_closed_timestamp_now(config.closed_ts_lag);
            tracker.publish(config.shard, target);
            next_publish = now + config.closed_ts_tick;
        }
    }
}

/// Convenience: spawn one loop per shard from a slice of configs.
/// Returns the handles of every loop that actually acquired its lease.
/// Skipped shards (already held) appear in the second element.
pub fn spawn_leaseholders(
    registry: &LeaseRegistry,
    tracker: &ClosedTimestampTracker,
    configs: &[LeaseholderConfig],
) -> (Vec<LeaseholderHandle>, Vec<ShardId>) {
    let mut handles = Vec::new();
    let mut skipped = Vec::new();
    for cfg in configs {
        match spawn_leaseholder(registry.clone(), tracker.clone(), cfg.clone()) {
            Some(h) => handles.push(h),
            None => skipped.push(cfg.shard),
        }
    }
    (handles, skipped)
}

/// Test-only helpers exposed at the crate root for clarity. Kept in
/// the public module so integration tests can re-use them.
pub mod testing {
    use super::*;

    /// Build a lightweight config tuned for fast tests.
    pub fn test_config(shard: ShardId, holder: LeaseHolderId) -> LeaseholderConfig {
        LeaseholderConfig {
            shard,
            holder,
            ttl: Duration::from_millis(120),
            closed_ts_lag: Duration::from_millis(50),
            closed_ts_tick: Duration::from_millis(20),
        }
    }
}

// Compile-time check that the loop is `Send` so it can be spawned on
// the multi-thread runtime.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<JoinHandle<LeaseholderExit>>();
};

// Force `Arc` import to remain used even after we trim it later; this
// is the canonical way to hint at a future shared-state field without
// pulling in dead-code warnings.
#[allow(dead_code)]
fn _arc_anchor<T: Send + Sync>() -> Arc<T> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::testing::test_config;
    use super::*;

    fn shard(n: u32) -> ShardId {
        ShardId::new(n)
    }

    #[tokio::test]
    async fn spawn_returns_none_when_lease_already_held() {
        let registry = LeaseRegistry::new();
        let tracker = ClosedTimestampTracker::new();
        let cfg_a = test_config(shard(1), 100);
        let cfg_b = test_config(shard(1), 200);
        let handle_a = spawn_leaseholder(registry.clone(), tracker.clone(), cfg_a)
            .expect("first holder should acquire");
        assert!(spawn_leaseholder(registry.clone(), tracker.clone(), cfg_b).is_none());
        handle_a.demote();
        let exit = handle_a.wait().await;
        assert_eq!(exit, LeaseholderExit::Shutdown);
    }

    #[tokio::test]
    async fn loop_renews_lease_before_expiry() {
        let registry = LeaseRegistry::new();
        let tracker = ClosedTimestampTracker::new();
        let cfg = test_config(shard(1), 42);
        let handle =
            spawn_leaseholder(registry.clone(), tracker.clone(), cfg.clone()).expect("acquire");
        let initial_epoch = handle.epoch();
        // Wait longer than the TTL would allow without renewal.
        tokio::time::sleep(cfg.ttl + Duration::from_millis(50)).await;
        let lease = registry.current(shard(1)).expect("still held");
        assert_eq!(
            lease.epoch, initial_epoch,
            "renewal keeps epoch stable; preemption would bump it"
        );
        assert_eq!(lease.holder, 42);
        handle.demote();
        let exit = handle.wait().await;
        assert_eq!(exit, LeaseholderExit::Shutdown);
    }

    #[tokio::test]
    async fn loop_publishes_closed_timestamps() {
        let registry = LeaseRegistry::new();
        let tracker = ClosedTimestampTracker::new();
        let cfg = test_config(shard(7), 99);
        let handle =
            spawn_leaseholder(registry.clone(), tracker.clone(), cfg.clone()).expect("acquire");
        // Allow a few ticks.
        tokio::time::sleep(cfg.closed_ts_tick * 3).await;
        let first = tracker.closed_ts(shard(7));
        assert!(first > 0, "closed timestamp should advance from zero");
        tokio::time::sleep(cfg.closed_ts_tick * 2).await;
        let second = tracker.closed_ts(shard(7));
        assert!(
            second > first,
            "second tick should produce a strictly newer closed timestamp ({first} -> {second})"
        );
        handle.demote();
        let _ = handle.wait().await;
    }

    #[tokio::test]
    async fn loop_exits_when_preempted_by_transfer() {
        let registry = LeaseRegistry::new();
        let tracker = ClosedTimestampTracker::new();
        let cfg = test_config(shard(1), 1);
        let handle =
            spawn_leaseholder(registry.clone(), tracker.clone(), cfg.clone()).expect("acquire");
        // Preempt from the side by transferring to a different holder.
        let _ = registry.transfer(shard(1), 999, Duration::from_secs(5), StdInstant::now());
        let exit = handle.wait().await;
        match exit {
            LeaseholderExit::Preempted { current } => {
                assert_eq!(current.holder, 999);
            }
            other => panic!("expected Preempted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn spawn_leaseholders_skips_already_held_shards() {
        let registry = LeaseRegistry::new();
        let tracker = ClosedTimestampTracker::new();
        let cfg_a = test_config(shard(1), 100);
        let cfg_b = test_config(shard(2), 200);
        let cfg_c = test_config(shard(1), 300); // same shard as a -> skip
        let (handles, skipped) =
            spawn_leaseholders(&registry, &tracker, &[cfg_a.clone(), cfg_b.clone(), cfg_c]);
        assert_eq!(handles.len(), 2);
        assert_eq!(skipped, vec![shard(1)]);
        for h in handles {
            h.demote();
            let _ = h.wait().await;
        }
    }
}
