//! High-availability and automatic failover configuration.

use std::time::Duration;

/// Default interval between health check heartbeats.
pub const DEFAULT_HA_HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(3);

/// Default timeout before declaring a node unreachable.
pub const DEFAULT_HA_HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// Default timeout for a leader election round.
pub const DEFAULT_HA_ELECTION_TIMEOUT: Duration = Duration::from_secs(15);

/// Default port for the HA inter-node protocol.
pub const DEFAULT_HA_PORT: u16 = 5433;

/// Default maximum WAL byte lag for a replica to be eligible for promotion.
pub const DEFAULT_HA_MAX_FAILOVER_LAG: u64 = 256 * 1024 * 1024;

/// Configuration for the high-availability subsystem.
#[derive(Clone, Eq, PartialEq)]
pub struct HaConfig {
    /// Whether automatic failover is enabled.
    pub enabled: bool,
    /// Unique numeric identifier for this node in the cluster.
    pub node_id: u64,
    /// Interval between heartbeat health checks sent to peer nodes.
    pub health_check_interval: Duration,
    /// Duration without a heartbeat before a node is declared unreachable.
    pub health_check_timeout: Duration,
    /// Maximum duration for a single leader election round.
    pub election_timeout: Duration,
    /// Maximum WAL byte lag for a replica to be eligible for promotion.
    /// Replicas lagging more than this behind the last known primary LSN
    /// will not participate in leader elections.
    pub max_failover_lag: u64,
    /// Addresses of all nodes in the HA cluster (including self).
    /// Format: `"host:port"` where port is the HA protocol port.
    pub cluster_nodes: Vec<String>,
    /// Port on which this node listens for HA protocol connections.
    pub ha_port: u16,
    /// Optional filesystem path for persisting fencing tokens.
    /// When `None`, fencing tokens are held in memory only.
    pub fencing_token_path: Option<String>,
    /// Shared secret for authenticating inter-node HA protocol messages.
    /// When set, all HA messages are HMAC-SHA256 signed and verified.
    /// MUST be configured for production deployments.
    pub inter_node_auth_token: Option<String>,
}

impl std::fmt::Debug for HaConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HaConfig")
            .field("enabled", &self.enabled)
            .field("node_id", &self.node_id)
            .field("health_check_interval", &self.health_check_interval)
            .field("health_check_timeout", &self.health_check_timeout)
            .field("election_timeout", &self.election_timeout)
            .field("max_failover_lag", &self.max_failover_lag)
            .field("cluster_nodes", &self.cluster_nodes)
            .field("ha_port", &self.ha_port)
            .field("fencing_token_path", &self.fencing_token_path)
            .field(
                "inter_node_auth_token",
                &self.inter_node_auth_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl Default for HaConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            node_id: 0,
            health_check_interval: DEFAULT_HA_HEALTH_CHECK_INTERVAL,
            health_check_timeout: DEFAULT_HA_HEALTH_CHECK_TIMEOUT,
            election_timeout: DEFAULT_HA_ELECTION_TIMEOUT,
            max_failover_lag: DEFAULT_HA_MAX_FAILOVER_LAG,
            cluster_nodes: Vec::new(),
            ha_port: DEFAULT_HA_PORT,
            fencing_token_path: None,
            inter_node_auth_token: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_expose_inter_node_auth_token() {
        let mut cfg = HaConfig::default();
        cfg.inter_node_auth_token = Some("super-secret-ha-token".to_owned());
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("super-secret-ha-token"));
        assert!(debug.contains("redacted"));
    }

    #[test]
    fn debug_shows_none_when_no_token() {
        let cfg = HaConfig::default();
        let debug = format!("{cfg:?}");
        assert!(debug.contains("inter_node_auth_token: None"));
    }
}
