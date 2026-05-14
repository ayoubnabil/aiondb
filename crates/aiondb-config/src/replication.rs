//! Replication configuration.

use std::time::Duration;

/// Default status update interval from replica to primary.
pub const DEFAULT_REPLICATION_STATUS_INTERVAL: Duration = Duration::from_secs(10);

/// Default maximum number of WAL sender connections.
pub const DEFAULT_MAX_WAL_SENDERS: u32 = 10;

/// Default minimum WAL segments to retain for replica catch-up.
pub const DEFAULT_WAL_KEEP_SEGMENTS: u32 = 64;

/// Default WAL compression mode.
pub const DEFAULT_WAL_COMPRESSION: WalCompression = WalCompression::None;

/// Default WAL LSN progression mode.
pub const DEFAULT_WAL_LSN_MODE: WalLsnMode = WalLsnMode::ByteOffset;

/// Default write concern: local (no replica wait).
pub const DEFAULT_WRITE_CONCERN: WriteConcern = WriteConcern::Local;

/// Default replication factor (1 = single copy, no replicas).
pub const DEFAULT_REPLICATION_FACTOR: u32 = 1;

/// Default synchronous commit timeout.
pub const DEFAULT_SYNC_COMMIT_TIMEOUT: Duration = Duration::from_secs(30);

/// WAL compression mode used for persisted WAL entries and replication batches.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WalCompression {
    /// Disable WAL payload compression.
    #[default]
    None,
    /// Enable LZ4 compression.
    Lz4,
    /// Enable Zstandard compression.
    Zstd,
}

impl WalCompression {
    /// Parse from a string value (case-insensitive).
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "none" | "off" | "false" | "0" => Some(Self::None),
            "lz4" => Some(Self::Lz4),
            "zstd" | "zst" => Some(Self::Zstd),
            _ => None,
        }
    }

    /// Return the lowercase textual name of this mode.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Lz4 => "lz4",
            Self::Zstd => "zstd",
        }
    }
}

/// WAL LSN progression mode used by the WAL writer.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WalLsnMode {
    /// Legacy sequential counter (`+1` per record).
    Logical,
    /// Byte-offset progression (`+encoded_entry_bytes` per record).
    #[default]
    ByteOffset,
}

impl WalLsnMode {
    /// Parse from a string value (case-insensitive).
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "logical" | "counter" | "seq" | "sequential" => Some(Self::Logical),
            "byte" | "byte_offset" | "offset" | "bytes" => Some(Self::ByteOffset),
            _ => None,
        }
    }

    /// Return the lowercase textual name of this mode.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Logical => "logical",
            Self::ByteOffset => "byte_offset",
        }
    }
}

/// Replication role of this server in the cluster topology.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReplicationRole {
    /// Standalone server with no replication (default).
    #[default]
    Standalone,
    /// Primary server that accepts writes and streams WAL to replicas.
    Primary,
    /// Replica server that receives WAL from a primary and is read-only.
    Replica,
}

impl ReplicationRole {
    /// Parse from a string value (case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "standalone" | "off" | "disabled" => Some(Self::Standalone),
            "primary" | "master" => Some(Self::Primary),
            "replica" | "standby" | "secondary" => Some(Self::Replica),
            _ => None,
        }
    }

    /// Return the role as a lowercase string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::Primary => "primary",
            Self::Replica => "replica",
        }
    }

    /// Returns `true` if this server should reject write operations.
    pub fn is_read_only(self) -> bool {
        matches!(self, Self::Replica)
    }

    /// Returns `true` if this server should accept replication connections.
    pub fn accepts_replication_connections(self) -> bool {
        matches!(self, Self::Primary)
    }
}

/// Write consistency level controlling how many replicas must confirm a
/// flush before a commit is acknowledged to the client.
///
/// Inspired by Qdrant's configurable write concern model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteConcern {
    /// Acknowledge after local persistence only. No replica wait.
    Local,
    /// Wait for a strict majority of nodes (primary + replicas) to flush.
    /// Majority = `total_nodes / 2 + 1` where `total_nodes` includes the
    /// primary. With 1 primary + 2 replicas (total=3), majority = 2, so the
    /// primary counts as 1 and we need at least 1 replica ack.
    Majority,
    /// Wait for ALL connected replicas to flush before acknowledging.
    All,
    /// Wait for exactly `n` replicas to flush before acknowledging.
    Factor(u32),
}

impl WriteConcern {
    /// Parse from a string value (case-insensitive).
    ///
    /// Accepted forms: `local`, `majority`, `all`, `factor:N`.
    pub fn parse(s: &str) -> Option<Self> {
        let lower = s.to_ascii_lowercase();
        match lower.as_str() {
            "local" | "off" | "async" => Some(Self::Local),
            "majority" | "quorum" => Some(Self::Majority),
            "all" | "sync" => Some(Self::All),
            other => {
                let stripped = other.strip_prefix("factor:")?;
                let n: u32 = stripped.parse().ok()?;
                Some(Self::Factor(n))
            }
        }
    }

    /// Return the textual representation of this write concern.
    pub fn as_str(&self) -> String {
        match self {
            Self::Local => "local".to_owned(),
            Self::Majority => "majority".to_owned(),
            Self::All => "all".to_owned(),
            Self::Factor(n) => format!("factor:{n}"),
        }
    }

    /// Encode this concern as a `u32` level code for storage-engine internal use.
    /// 0 = Local, 1 = Majority, 2 = All, 3+n = Factor(n).
    pub fn encode_level(&self) -> u32 {
        match self {
            Self::Local => 0,
            Self::Majority => 1,
            Self::All => 2,
            Self::Factor(n) => 3u32.saturating_add(*n),
        }
    }

    /// Compute the number of replica acks required for this concern level
    /// given the current connected replica count.
    ///
    /// The primary always counts as 1 acked node (it wrote locally).
    /// This returns the number of *additional* replica flushes needed.
    pub fn required_replica_acks(&self, connected_replicas: usize) -> usize {
        match self {
            Self::Local => 0,
            Self::Majority => {
                // total_nodes = 1 (primary) + connected_replicas
                // strict majority = total / 2 + 1
                // primary already acked, so additional acks = majority - 1
                let total = connected_replicas.saturating_add(1);
                let majority = total / 2 + 1;
                majority.saturating_sub(1)
            }
            Self::All => connected_replicas,
            Self::Factor(n) => *n as usize,
        }
    }
}

/// Configuration for WAL-based streaming replication.
#[derive(Clone, Eq, PartialEq)]
pub struct ReplicationConfig {
    /// The role of this server in the replication topology.
    pub role: ReplicationRole,
    /// Connection string to the primary (used when role is `Replica`).
    /// Recognized keys include `host`, `port`, `user`, `password`,
    /// `dbname`, `application_name`, and `sslmode`.
    pub primary_conninfo: Option<String>,
    /// Maximum number of concurrent WAL sender connections (primary only).
    pub max_wal_senders: u32,
    /// Minimum number of WAL segments to retain for catch-up replication.
    pub wal_keep_segments: u32,
    /// Interval between standby status updates from replica to primary.
    pub status_interval: Duration,
    /// Whether the primary should wait for at least one synchronous
    /// replica to confirm flush before acknowledging commits.
    ///
    /// Legacy field - prefer [`write_concern`](Self::write_concern).
    /// When `true` and `write_concern` is `Local`, the effective concern
    /// is upgraded to `Majority` for backward compatibility.
    pub synchronous_commit: bool,
    /// Write consistency level controlling how many replicas must confirm
    /// flush before commits are acknowledged.
    pub write_concern: WriteConcern,
    /// Total number of data copies (including the primary).
    /// 1 = no replicas, 2 = one replica, etc.
    pub replication_factor: u32,
    /// Timeout for synchronous commit / write concern waits.
    pub sync_commit_timeout: Duration,
    /// WAL compression mode applied to persisted WAL entries and outgoing
    /// WAL replication batches.
    pub wal_compression: WalCompression,
    /// WAL LSN progression mode used when assigning WAL positions.
    pub wal_lsn_mode: WalLsnMode,
}

impl std::fmt::Debug for ReplicationConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplicationConfig")
            .field("role", &self.role)
            .field(
                "primary_conninfo",
                &self.primary_conninfo.as_ref().map(|_| "<redacted>"),
            )
            .field("max_wal_senders", &self.max_wal_senders)
            .field("wal_keep_segments", &self.wal_keep_segments)
            .field("status_interval", &self.status_interval)
            .field("synchronous_commit", &self.synchronous_commit)
            .field("write_concern", &self.write_concern)
            .field("replication_factor", &self.replication_factor)
            .field("sync_commit_timeout", &self.sync_commit_timeout)
            .field("wal_compression", &self.wal_compression)
            .field("wal_lsn_mode", &self.wal_lsn_mode)
            .finish()
    }
}

impl Default for ReplicationConfig {
    fn default() -> Self {
        Self {
            role: ReplicationRole::Standalone,
            primary_conninfo: None,
            max_wal_senders: DEFAULT_MAX_WAL_SENDERS,
            wal_keep_segments: DEFAULT_WAL_KEEP_SEGMENTS,
            status_interval: DEFAULT_REPLICATION_STATUS_INTERVAL,
            synchronous_commit: false,
            write_concern: DEFAULT_WRITE_CONCERN,
            replication_factor: DEFAULT_REPLICATION_FACTOR,
            sync_commit_timeout: DEFAULT_SYNC_COMMIT_TIMEOUT,
            wal_compression: DEFAULT_WAL_COMPRESSION,
            wal_lsn_mode: DEFAULT_WAL_LSN_MODE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WriteConcern;

    #[test]
    fn majority_write_concern_requires_strict_majority() {
        assert_eq!(WriteConcern::Majority.required_replica_acks(0), 0);
        assert_eq!(WriteConcern::Majority.required_replica_acks(1), 1);
        assert_eq!(WriteConcern::Majority.required_replica_acks(2), 1);
        assert_eq!(WriteConcern::Majority.required_replica_acks(3), 2);
    }

    #[test]
    fn majority_write_concern_saturates_connected_replica_count() {
        assert_eq!(
            WriteConcern::Majority.required_replica_acks(usize::MAX),
            usize::MAX / 2
        );
    }

    #[test]
    fn factor_write_concern_does_not_degrade_when_replicas_are_disconnected() {
        assert_eq!(WriteConcern::Factor(2).required_replica_acks(0), 2);
        assert_eq!(WriteConcern::Factor(2).required_replica_acks(1), 2);
        assert_eq!(WriteConcern::Factor(2).required_replica_acks(2), 2);
    }

    #[test]
    fn factor_write_concern_encode_level_saturates() {
        assert_eq!(WriteConcern::Factor(u32::MAX).encode_level(), u32::MAX);
    }
}
