//! Choose lease type per range.
//!
//! Two strategies are available :
//!
//! - **EpochBased** : the leaseholder holds the lease until its
//!   epoch is bumped by quorum. Cheap because it doesn't need a
//!   strict clock bound; can briefly stall on partition.
//! - **Expiration**  : leases carry a hard wall-clock expiry. Faster
//!   failover but requires a bounded clock skew.
//!
//! The chooser picks based on the current cluster health (skew,
//! voter count, recent partitions).

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseType {
    EpochBased,
    Expiration,
}

#[derive(Clone, Copy, Debug)]
pub struct ClusterHealth {
    pub max_clock_skew_ms: u32,
    pub voter_count: u32,
    pub recent_partition_seconds: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct LeaseChooserConfig {
    pub max_skew_for_expiration_ms: u32,
    pub min_voters_for_expiration: u32,
    pub recent_partition_window_seconds: u32,
}

impl Default for LeaseChooserConfig {
    fn default() -> Self {
        Self {
            max_skew_for_expiration_ms: 500,
            min_voters_for_expiration: 3,
            recent_partition_window_seconds: 60,
        }
    }
}

pub fn choose_lease_type(health: &ClusterHealth, cfg: &LeaseChooserConfig) -> LeaseType {
    if health.max_clock_skew_ms > cfg.max_skew_for_expiration_ms {
        return LeaseType::EpochBased;
    }
    if health.voter_count < cfg.min_voters_for_expiration {
        return LeaseType::EpochBased;
    }
    if health.recent_partition_seconds < cfg.recent_partition_window_seconds {
        return LeaseType::EpochBased;
    }
    LeaseType::Expiration
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(skew_ms: u32, voters: u32, partition_s: u32) -> ClusterHealth {
        ClusterHealth {
            max_clock_skew_ms: skew_ms,
            voter_count: voters,
            recent_partition_seconds: partition_s,
        }
    }

    #[test]
    fn high_skew_picks_epoch() {
        let l = choose_lease_type(&h(2000, 5, 999), &LeaseChooserConfig::default());
        assert_eq!(l, LeaseType::EpochBased);
    }

    #[test]
    fn low_voter_count_picks_epoch() {
        let l = choose_lease_type(&h(50, 2, 999), &LeaseChooserConfig::default());
        assert_eq!(l, LeaseType::EpochBased);
    }

    #[test]
    fn recent_partition_picks_epoch() {
        let l = choose_lease_type(&h(50, 5, 10), &LeaseChooserConfig::default());
        assert_eq!(l, LeaseType::EpochBased);
    }

    #[test]
    fn healthy_cluster_picks_expiration() {
        let l = choose_lease_type(&h(50, 5, 999), &LeaseChooserConfig::default());
        assert_eq!(l, LeaseType::Expiration);
    }

    #[test]
    fn config_overrides_defaults() {
        let cfg = LeaseChooserConfig {
            max_skew_for_expiration_ms: 100,
            min_voters_for_expiration: 5,
            recent_partition_window_seconds: 0,
        };
        let l = choose_lease_type(&h(50, 5, 5), &cfg);
        assert_eq!(l, LeaseType::Expiration);
    }
}
