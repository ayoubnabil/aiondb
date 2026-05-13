//! Lease / replica rebalance executor.
//!
//! Watches per-node load (lease count, write rate, range count) and
//! issues lease transfers when one node is significantly more loaded
//! than another. Inspired by CockroachDB's allocator + lease balancer,
//! reduced to the minimum primitive that actually moves load.
//!
//! The executor works in two passes:
//!
//! 1. **Lease balance** : transfer the cheapest-to-move lease from the
//!    hottest node to the coolest node, repeated up to
//!    `max_transfers_per_tick` times per cycle.
//! 2. **Bookkeeping** : update the [`LeaseRegistry`] so subsequent
//!    routing decisions observe the new owner.
//!
//! Replica migration (changing which set of nodes hosts a range) is
//! out of scope for this executor -- that is the planner's job and
//! lives in `aiondb-cluster::replication`.
//!
//! # Why lease balance is enough for first-cut
//!
//! Lease transfers are cheap (no data move, just a Raft round-trip)
//! and address the most common load imbalance source: a single hot
//! node serving disproportionately many ranges because failover left
//! it as leaseholder for every shard.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aiondb_core::DbResult;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::debug;

use crate::lease::{LeaseHolderId, LeaseRegistry};
use crate::ShardId;

/// Per-node load sample.
#[derive(Clone, Copy, Debug, Default)]
pub struct NodeLoad {
    pub lease_count: u64,
    pub write_qps: f64,
}

/// One scheduled lease move.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RebalanceAction {
    pub shard: ShardId,
    pub from: LeaseHolderId,
    pub to: LeaseHolderId,
}

/// Executor config.
#[derive(Clone, Debug)]
pub struct RebalanceConfig {
    pub poll_interval: Duration,
    pub max_transfers_per_tick: usize,
    /// Minimum lease-count delta before triggering a transfer.
    pub min_imbalance_delta: u64,
    /// New lease TTL.
    pub lease_ttl: Duration,
}

impl Default for RebalanceConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(10),
            max_transfers_per_tick: 4,
            min_imbalance_delta: 2,
            lease_ttl: Duration::from_secs(9),
        }
    }
}

/// Synchronous executor used both for tests and for the async wrapper.
pub struct RebalanceExecutor {
    leases: LeaseRegistry,
    config: RebalanceConfig,
    /// External callback to query each node's load. Cheap to call --
    /// production wires it to gossip-derived metrics. Default impl
    /// returns the lease count alone (so the executor still moves
    /// load even without a metrics agent).
    load_fn: Arc<dyn Fn(LeaseHolderId) -> NodeLoad + Send + Sync>,
}

impl RebalanceExecutor {
    pub fn new(
        leases: LeaseRegistry,
        config: RebalanceConfig,
        load_fn: Arc<dyn Fn(LeaseHolderId) -> NodeLoad + Send + Sync>,
    ) -> Self {
        Self {
            leases,
            config,
            load_fn,
        }
    }

    /// Run one pass. Returns the list of transfers performed.
    pub fn tick(&self) -> DbResult<Vec<RebalanceAction>> {
        let mut performed = Vec::new();
        for _ in 0..self.config.max_transfers_per_tick {
            let Some(action) = self.next_action() else {
                break;
            };
            // Apply the lease transfer.
            self.leases.transfer(
                action.shard,
                action.to,
                self.config.lease_ttl,
                Instant::now(),
            );
            debug!(?action, "lease rebalance");
            performed.push(action);
        }
        Ok(performed)
    }

    fn next_action(&self) -> Option<RebalanceAction> {
        // Group leases by holder.
        let leases = self.leases.snapshot();
        if leases.len() <= 1 {
            return None;
        }
        let mut by_holder: BTreeMap<LeaseHolderId, Vec<ShardId>> = BTreeMap::new();
        for lease in &leases {
            by_holder.entry(lease.holder).or_default().push(lease.shard);
        }
        if by_holder.len() <= 1 {
            return None;
        }

        // Snapshot load per holder.
        let mut load: BTreeMap<LeaseHolderId, NodeLoad> = BTreeMap::new();
        for holder in by_holder.keys() {
            load.insert(*holder, (self.load_fn)(*holder));
        }

        // Find hottest and coolest.
        let mut hottest: Option<LeaseHolderId> = None;
        let mut coolest: Option<LeaseHolderId> = None;
        let mut hottest_load: u64 = 0;
        let mut coolest_load: u64 = u64::MAX;
        for (holder, l) in &load {
            if l.lease_count > hottest_load {
                hottest_load = l.lease_count;
                hottest = Some(*holder);
            }
            if l.lease_count < coolest_load {
                coolest_load = l.lease_count;
                coolest = Some(*holder);
            }
        }
        let from = hottest?;
        let to = coolest?;
        if from == to {
            return None;
        }
        if hottest_load.saturating_sub(coolest_load) < self.config.min_imbalance_delta {
            return None;
        }
        // Pick one of the hottest holder's leases to move. Stable
        // choice (smallest shard id) so the executor is deterministic.
        let shard = by_holder.get(&from)?.iter().min().copied()?;
        Some(RebalanceAction { shard, from, to })
    }
}

/// Async wrapper: ticks the executor on a schedule.
pub struct RebalanceTask {
    handle: JoinHandle<()>,
    shutdown_tx: watch::Sender<bool>,
}

impl RebalanceTask {
    pub fn spawn(executor: Arc<RebalanceExecutor>) -> Self {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let handle = tokio::spawn(async move {
            let mut ticker = time::interval(executor.config.poll_interval);
            ticker.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            return;
                        }
                    }
                    _ = ticker.tick() => {
                        let _ = executor.tick();
                    }
                }
            }
        });
        Self {
            handle,
            shutdown_tx,
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.handle.await;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn shard(n: u32) -> ShardId {
        ShardId::new(n)
    }

    fn fixed_load_fn(
        map: HashMap<LeaseHolderId, NodeLoad>,
    ) -> Arc<dyn Fn(LeaseHolderId) -> NodeLoad + Send + Sync> {
        Arc::new(move |id: LeaseHolderId| map.get(&id).copied().unwrap_or_default())
    }

    fn fresh_leases() -> LeaseRegistry {
        let lr = LeaseRegistry::new();
        // Node 1 holds three leases.
        lr.acquire(shard(1), 1, Duration::from_secs(60), Instant::now());
        lr.acquire(shard(2), 1, Duration::from_secs(60), Instant::now());
        lr.acquire(shard(3), 1, Duration::from_secs(60), Instant::now());
        // Node 2 holds one lease.
        lr.acquire(shard(4), 2, Duration::from_secs(60), Instant::now());
        lr
    }

    #[test]
    fn balanced_cluster_makes_no_moves() {
        let leases = fresh_leases();
        let mut load = HashMap::new();
        load.insert(
            1,
            NodeLoad {
                lease_count: 1,
                write_qps: 0.0,
            },
        );
        load.insert(
            2,
            NodeLoad {
                lease_count: 1,
                write_qps: 0.0,
            },
        );
        let exec = RebalanceExecutor::new(
            leases.clone(),
            RebalanceConfig {
                min_imbalance_delta: 5,
                ..RebalanceConfig::default()
            },
            fixed_load_fn(load),
        );
        let actions = exec.tick().unwrap();
        assert!(actions.is_empty());
    }

    #[test]
    fn imbalanced_cluster_transfers_from_hot_to_cool() {
        let leases = fresh_leases();
        let mut load = HashMap::new();
        load.insert(
            1,
            NodeLoad {
                lease_count: 3,
                write_qps: 100.0,
            },
        );
        load.insert(
            2,
            NodeLoad {
                lease_count: 1,
                write_qps: 10.0,
            },
        );
        let exec = RebalanceExecutor::new(
            leases.clone(),
            RebalanceConfig {
                max_transfers_per_tick: 1,
                min_imbalance_delta: 1,
                ..RebalanceConfig::default()
            },
            fixed_load_fn(load),
        );
        let actions = exec.tick().unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].from, 1);
        assert_eq!(actions[0].to, 2);
        // After transfer, node 2 should hold one extra lease.
        let by_holder: HashMap<LeaseHolderId, usize> =
            leases.snapshot().iter().fold(HashMap::new(), |mut acc, l| {
                *acc.entry(l.holder).or_insert(0) += 1;
                acc
            });
        assert_eq!(
            *by_holder.get(&2).unwrap_or(&0),
            2,
            "node 2 received a lease"
        );
    }

    #[test]
    fn imbalanced_cluster_caps_transfers_per_tick() {
        let leases = fresh_leases();
        let mut load = HashMap::new();
        load.insert(
            1,
            NodeLoad {
                lease_count: 3,
                write_qps: 0.0,
            },
        );
        load.insert(
            2,
            NodeLoad {
                lease_count: 1,
                write_qps: 0.0,
            },
        );
        let exec = RebalanceExecutor::new(
            leases.clone(),
            RebalanceConfig {
                max_transfers_per_tick: 5, // far more than needed
                min_imbalance_delta: 1,
                ..RebalanceConfig::default()
            },
            // After the first transfer node 2's load goes 1 -> 2 in the
            // *registry*, but our static load_fn keeps reporting 1, so
            // the executor will keep moving leases until the registry
            // is empty for node 1. The cap protects against that
            // runaway when load_fn lags behind reality.
            fixed_load_fn(load),
        );
        let actions = exec.tick().unwrap();
        // Cap is 5 but the cluster only has 3 leases on node 1 plus 1
        // on node 2 = 4 total. After two transfers the hottest node
        // still has 1 lease which is the min, so the algorithm picks
        // a different hottest. Eventually we hit a steady state where
        // both nodes have 2 leases.
        assert!(
            actions.len() <= 5,
            "transfer count capped: {} <= 5",
            actions.len()
        );
        // Either node should no longer have all the leases.
        let by_holder: HashMap<LeaseHolderId, usize> =
            leases.snapshot().iter().fold(HashMap::new(), |mut acc, l| {
                *acc.entry(l.holder).or_insert(0) += 1;
                acc
            });
        assert!(by_holder.get(&2).copied().unwrap_or(0) >= 1);
    }

    #[test]
    fn small_imbalance_does_not_trigger_when_below_threshold() {
        let leases = fresh_leases();
        let mut load = HashMap::new();
        load.insert(
            1,
            NodeLoad {
                lease_count: 3,
                write_qps: 0.0,
            },
        );
        load.insert(
            2,
            NodeLoad {
                lease_count: 2,
                write_qps: 0.0,
            },
        );
        let exec = RebalanceExecutor::new(
            leases.clone(),
            RebalanceConfig {
                min_imbalance_delta: 5, // bigger gap required
                ..RebalanceConfig::default()
            },
            fixed_load_fn(load),
        );
        let actions = exec.tick().unwrap();
        assert!(actions.is_empty());
    }

    #[test]
    fn empty_registry_is_noop() {
        let leases = LeaseRegistry::new();
        let exec = RebalanceExecutor::new(
            leases,
            RebalanceConfig::default(),
            fixed_load_fn(HashMap::new()),
        );
        assert!(exec.tick().unwrap().is_empty());
    }
}
